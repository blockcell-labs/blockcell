//! 学习与记忆协调 — Review 触发、Ghost Learning 边界捕获、Memory flush
//!
//! 包含来自 AgentRuntime 的 review 生成、学习边界捕获、
//! 记忆刷新和 ghost learning 相关方法。

use super::{
    active_skill_name_from_metadata, chat_message_text, disable_skill_toggle,
    persist_ghost_learning_boundary_with_decision, truncate_str, MainSessionTarget, ReviewMode,
    COMBINED_REVIEW_PROMPT, LEARNED_SKILL_DISABLE_THRESHOLD, MEMORY_REVIEW_PROMPT,
    SESSION_ACTIVE_SKILL_CORRECTIONS_KEY, SESSION_ACTIVE_SKILL_NAME_KEY, SKILL_REVIEW_PROMPT,
};
use crate::ghost_background_review::spawn_pending_background_reviews;
use crate::ghost_learning::{
    estimate_turn_complexity_score, GhostLearningBoundary, GhostLearningBoundaryKind,
};
use crate::learning_coordinator::{LearningBoundaryJob, LearningOutputRequest};
use blockcell_core::types::ChatMessage;
use blockcell_core::Result;
use blockcell_core::{scope_abort_token, InboundMessage, OutboundMessage};
use blockcell_storage::ghost_ledger::GhostEpisodeSource;
use blockcell_tools::{CapabilityRegistryHandle, CoreEvolutionHandle, ToolRegistry};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

impl super::AgentRuntime {
    pub(super) fn spawn_review(
        &self,
        mode: ReviewMode,
        messages: Vec<ChatMessage>,
        parent_tool_schemas: Vec<serde_json::Value>,
        parent_served_model: Option<String>,
        notify_channel: Option<(String, String)>,
        reservation: crate::learning_coordinator::LearningReviewReservationGuard,
    ) {
        if !self.background_tasks_enabled {
            drop(reservation);
            debug!(
                ?mode,
                "Skipping nudge review because background tasks are disabled"
            );
            return;
        }

        let label = match mode {
            ReviewMode::Skill => "skill_nudge_review",
            ReviewMode::Memory => "memory_nudge_review",
            ReviewMode::Combined => "combined_nudge_review",
        };
        tracing::info!("[Nudge] 阈值到达, 启动后台 {:?} Review", mode);

        // 确保该 paths 对应的 GhostMetrics 实例已在全局注册表中初始化，
        // 以便 run_forked_agent 结束时按路径前缀自动归入正确的 GhostMetrics。
        let _ = crate::ghost_metrics::get_ghost_metrics(&self.paths);
        let skills_dir = self.paths.skills_dir();
        // 克隆一份供 ForkedAgent 使用（spawn_blocking 会 move 原始值）
        let skills_dir_clone = skills_dir.clone();
        let builtin_skills_dir = self.paths.builtin_skills_dir();
        let external_skills_dirs = vec![builtin_skills_dir];
        let provider_pool = self.provider_pool.clone();
        let model = parent_served_model
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| self.config.agents.defaults.model.clone());
        let max_review_rounds = self.config.self_improve.review.max_rounds;
        let memory_store = self.memory_store.clone();
        let memory_file_store = self.memory_file_store.clone();
        let skill_file_store = self.skill_file_store.clone();
        let skill_mutex = self.skill_mutex.clone();
        let mode_clone = mode.clone();
        // 与 Hermes 一致: review_agent 直接复用父快照首条已冻结/已注入的 system message，
        // 避免 build_system_prompt() 重新渲染导致前缀分叉和双 system message。
        let system_prompt = messages
            .first()
            .filter(|m| m.role == "system")
            .and_then(|m| m.content.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| self.context_builder.build_system_prompt());
        let outbound_tx = self.outbound_tx.clone();
        // 共享 skill_index_summary Arc, 供后台 Review 完成后刷新
        let skill_index_cache = self.context_builder.skill_index_summary_arc();
        // 后台 Review 挂到取消链的根节点，而不是当前消息的 token：同一会话的
        // 下一条消息会 cancel 上一条消息 token，若 Review 是其子节点会被连带取消。
        // 根节点仍覆盖进程/运行时级取消。
        let review_abort_token = self.abort_token.root().child();

        tokio::spawn(async move {
            let _review_completion_guard = reservation.into_completion_guard();
            // 用 scope_abort_token 包裹整个 review 逻辑，取消时立即退出
            scope_abort_token(review_abort_token, async {
            // 构建 Skill 索引（仅在 Skill/Combined 模式下需要）
            let skill_summary = match mode_clone {
                ReviewMode::Memory => String::new(),
                ReviewMode::Skill | ReviewMode::Combined => {
                    let index = match tokio::task::spawn_blocking(move || {
                        if skills_dir.exists() {
                            crate::skill_index::SkillIndex::build_from_dir(&skills_dir)
                        } else {
                            crate::skill_index::SkillIndex::new()
                        }
                    })
                    .await
                    {
                        Ok(idx) => idx,
                        Err(e) => {
                            tracing::warn!(error = %e, "[Nudge] 构建索引任务失败");
                            return;
                        }
                    };

                    if index.entries().is_empty() {
                        tracing::info!("[Nudge] 无可用 Skill, 跳过 Skill 部分");
                        String::new()
                    } else {
                        index.to_prompt_summary()
                    }
                }
            };

            // 构建 Review 提示词 (与 Hermes 一致: 选择对应模式的 prompt)
            let review_prompt = match mode_clone {
                ReviewMode::Skill => SKILL_REVIEW_PROMPT.to_string(),
                ReviewMode::Memory => MEMORY_REVIEW_PROMPT.to_string(),
                ReviewMode::Combined => COMBINED_REVIEW_PROMPT.to_string(),
            };

            // 构建工具权限
            // 与 Hermes 一致: review_agent 继承主 agent 的 system prompt，不设自定义系统提示词
            // Hermes: review_agent = AIAgent(model=self.model, ...) → 使用默认 system prompt
            let can_use_tool = match mode_clone {
                ReviewMode::Skill => crate::forked::create_skill_review_can_use_tool(),
                ReviewMode::Memory => crate::forked::create_memory_review_can_use_tool(),
                ReviewMode::Combined => crate::forked::create_combined_review_can_use_tool(),
            };

            // 缓存对齐: 优先原样复用父轮完整的 tool_schemas（保持 tools[] 字节一致以命中 Prompt Cache），
            // 仅当父轮无工具（如纯 Chat 模式）或缺失核心 review 工具时才回退到窄 schema。
            // 安全边界由上方的 can_use_tool 在执行期拦截保障。
            let parent_cache_safe_tools = Self::tool_definitions_from_schemas(&parent_tool_schemas);
            let parent_cache_safe = crate::forked::CacheSafeParams::new(
                system_prompt.clone(),
                model.clone(),
            )
            .with_fork_messages(messages.clone())
            .with_tools(parent_cache_safe_tools);

            let tool_schemas = Self::resolve_review_tool_schemas(&mode_clone, &parent_tool_schemas);
            let cache_safe_tools = Self::tool_definitions_from_schemas(&tool_schemas);

            // 构建 ForkedAgent 参数 (与 Hermes 一致: fork_context_messages 承载父对话前缀，
            // prompt_messages 仅承载增量 review 指令)
            let full_prompt =
                Self::format_review_user_prompt(&mode_clone, &review_prompt, &skill_summary);
            let prompt_messages = vec![ChatMessage::user(&full_prompt)];

            let cache_safe = crate::forked::CacheSafeParams::new(system_prompt, &model)
                .with_fork_messages(messages)
                .with_tools(cache_safe_tools);

            // C10: 生产期校验 CacheSafeParams 与父轮是否兼容；若不兼容（例如父轮为无工具 Chat 模式
            // 导致回退窄 schema），记录告警与指标并平滑降级为冷请求继续执行。
            if !cache_safe.is_compatible_with(&parent_cache_safe) {
                crate::ghost_metrics::record_fork_alignment_miss_from_fork(
                    Some(&skills_dir_clone),
                    label,
                    "cache_safe_params_incompatible_with_parent",
                );
            }

            let mut params =
                crate::forked::ForkedAgentParams::new(provider_pool, prompt_messages, cache_safe)
                    .with_can_use_tool(can_use_tool)
                    .with_tool_schemas(tool_schemas)
                    .with_query_source("review")
                    .with_fork_label(label)
                    .with_max_turns(max_review_rounds);

            // 传入 memory_store（Memory/Combined 模式需要）
            if let Some(store) = memory_store {
                params = params.with_memory_store(store);
            }
            if let Some(store) = memory_file_store {
                params = params.with_memory_file_store(store);
            }
            if let Some(store) = skill_file_store {
                params = params.with_skill_file_store(store);
            }

            // 传入 skill_mutex（防止 review agent 与主 agent 并发修改同一 Skill）
            params = params.with_skill_mutex(skill_mutex);

            // 传入 skills_dir（Skill/Combined 用于执行技能工具，同时为所有模式提供工作区路径提示以关联 GhostMetrics）
            params = params.with_skills_dir(skills_dir_clone.clone());
            if matches!(mode_clone, ReviewMode::Skill | ReviewMode::Combined) {
                params = params.with_external_skills_dirs(external_skills_dirs.clone());
            }

            match crate::forked::run_forked_agent(params).await {
                Ok(result) => {
                    if result.truncated {
                        tracing::warn!(mode = ?mode_clone, "[Nudge] Review 结果被截断，可能丢失部分信息");
                    }
                    tracing::info!(
                        mode = ?mode_clone,
                        tokens_out = result.total_usage.output_tokens,
                        "[Nudge] Review 完成"
                    );
                    if let Some(content) = &result.final_content {
                        let preview: String = content.chars().take(200).collect();
                        tracing::info!("[Nudge] Review 结果: {}", preview);
                    }
                    // 提取 Review 摘要并通知用户 (与 Hermes 一致)
                    if let Some((channel, chat_id)) = &notify_channel {
                        if let Some(tx) = &outbound_tx {
                            if let Some(summary) = Self::extract_review_summary(&result.messages) {
                                let outbound = OutboundMessage::new(channel, chat_id, &summary);
                                let _ = tx.send(outbound).await;
                                tracing::info!("[Nudge] Review 通知已发送: {}", summary);
                            }
                        }
                    }

                    // 刷新父 Agent 的 Skill 索引缓存 (后台 Review 可能创建/修改了 Skill)
                    // 与 Hermes 一致: 系统提示词在下次 LLM 调用时反映最新的 Skill 列表
                    if matches!(mode_clone, ReviewMode::Skill | ReviewMode::Combined) {
                        if let Ok(index) = tokio::task::spawn_blocking(move || {
                            if skills_dir_clone.exists() {
                                crate::skill_index::SkillIndex::build_from_dir(&skills_dir_clone)
                            } else {
                                crate::skill_index::SkillIndex::new()
                            }
                        })
                        .await
                        {
                            let mut cache =
                                skill_index_cache.write().unwrap_or_else(|e| e.into_inner());
                            *cache = if index.entries().is_empty() {
                                None
                            } else {
                                Some(index.to_prompt_summary())
                            };
                            tracing::info!("[Nudge] Skill 索引缓存已刷新");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(mode = ?mode_clone, error = %e, "[Nudge] Review 失败");
                }
            }
        }).await;
        });
    }

    /// 缓存对齐工具解析：当父轮 `parent_tool_schemas` 已包含当前 Review 模式所需的核心工具时，
    /// 原样返回父轮完整 `tools[]` 以保持字节级缓存前缀一致；否则回退到该模式的窄工具集。
    pub(super) fn resolve_review_tool_schemas(
        mode: &ReviewMode,
        parent_tool_schemas: &[serde_json::Value],
    ) -> Vec<serde_json::Value> {
        let has_tool = |target: &str| {
            parent_tool_schemas.iter().any(|schema| {
                schema
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    == Some(target)
            })
        };

        let parent_has_required = match mode {
            ReviewMode::Skill => has_tool("skill_manage"),
            ReviewMode::Memory => has_tool("memory_manage"),
            ReviewMode::Combined => has_tool("skill_manage") && has_tool("memory_manage"),
        };

        if parent_has_required {
            parent_tool_schemas.to_vec()
        } else {
            match mode {
                ReviewMode::Skill => crate::forked::build_skill_review_tool_schemas(),
                ReviewMode::Memory => crate::forked::build_memory_review_tool_schemas(),
                ReviewMode::Combined => crate::forked::build_combined_review_tool_schemas(),
            }
        }
    }

    pub(super) fn tool_definitions_from_schemas(
        schemas: &[serde_json::Value],
    ) -> Vec<crate::forked::ToolDefinition> {
        schemas
            .iter()
            .filter_map(|schema| {
                let func = schema.get("function")?;
                let name = func.get("name")?.as_str()?;
                let description = func
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or_default();
                let parameters = func
                    .get("parameters")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                Some(crate::forked::ToolDefinition::new(
                    name,
                    description,
                    parameters,
                ))
            })
            .collect()
    }

    pub(super) fn format_review_user_prompt(
        mode: &ReviewMode,
        review_prompt: &str,
        skill_summary: &str,
    ) -> String {
        let tool_mode = match mode {
            ReviewMode::Skill => crate::forked::ReviewToolMode::Skill,
            ReviewMode::Memory => crate::forked::ReviewToolMode::Memory,
            ReviewMode::Combined => crate::forked::ReviewToolMode::Combined,
        };
        let allowed_tools = crate::forked::roster_names(tool_mode).join(", ");
        let mut prompt = format!(
            "{}\n\nIMPORTANT: In this review turn, you may ONLY use the following tools: {}. Do not call any other tools.",
            review_prompt, allowed_tools
        );
        if !skill_summary.is_empty() {
            prompt.push_str(&format!("\n\n## Existing Skills\n{}", skill_summary));
        }
        prompt
    }

    /// 从 Review Agent 的 tool 结果中提取操作摘要 (参考 Hermes 行为)
    ///
    /// Hermes 扫描 review_agent._session_messages 中的 tool 结果,
    /// 查找 created/updated/deleted 等操作，汇总为用户可见的摘要。
    fn extract_review_summary(messages: &[ChatMessage]) -> Option<String> {
        let mut actions: Vec<String> = Vec::new();

        for msg in messages {
            if msg.role != "tool" {
                continue;
            }
            let content = match msg.content.as_str() {
                Some(c) => c,
                None => continue,
            };
            // 解析 JSON (skill_manage 和 memory 工具返回 JSON，但格式不同)
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(content) {
                // ── skill_manage 结果: {"success": true, "message": "Skill 'xxx' created", ...} ──
                let is_skill_success = data
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if is_skill_success {
                    if let Some(msg_text) = data.get("message").and_then(|v| v.as_str()) {
                        let lower = msg_text.to_lowercase();
                        if lower.contains("created")
                            || lower.contains("deleted")
                            || lower.contains("updated")
                            || lower.contains("patched")
                            || lower.contains("edited")
                            || lower.contains("added")
                            || lower.contains("removed")
                            || lower.contains("replaced")
                        {
                            actions.push(msg_text.to_string());
                        }
                    }

                    // memory 工具 (Hermes 格式): {"target": "memory", "success": true, ...}
                    if let Some(target) = data.get("target").and_then(|v| v.as_str()) {
                        if !target.is_empty() && data.get("message").is_none() {
                            let label = match target {
                                "memory" => "Memory updated",
                                "user" => "User profile updated",
                                other => other,
                            };
                            actions.push(label.to_string());
                        }
                    }
                }

                // ── memory_upsert 结果: {"status": "saved", "item": {...}} ──
                if data.get("status").and_then(|v| v.as_str()) == Some("saved") {
                    actions.push("Memory updated".to_string());
                }

                // ── memory_forget 结果: {"action": "delete", "deleted": true, ...} ──
                match data.get("action").and_then(|v| v.as_str()) {
                    Some("delete")
                        if data
                            .get("deleted")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false) =>
                    {
                        actions.push("Memory updated".to_string());
                    }
                    Some("batch_delete") => {
                        let count = data
                            .get("deleted_count")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        if count > 0 {
                            actions.push(format!("Memory updated ({} items forgotten)", count));
                        }
                    }
                    Some("restore")
                        if data
                            .get("restored")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false) =>
                    {
                        actions.push("Memory item restored".to_string());
                    }
                    _ => {}
                }
            }
        }

        if actions.is_empty() {
            None
        } else {
            Some(format!("\u{1F4BE} {}", actions.join(" \u{00B7} ")))
        }
    }

    /// Initialize and load Layer 5 memory injector (7-layer memory system).
    /// This consolidates legacy Layer 5 files and loads only USER.md and MEMORY.md.
    pub async fn init_memory_injector(&mut self) -> std::io::Result<()> {
        use crate::auto_memory::{consolidate_legacy_layer5, InjectionConfig, MemoryInjector};

        // Forked message runtimes may initialize concurrently. Reserve the
        // one-shot attempt before doing disk I/O so a persistent failure does
        // not fan out into one retry per message.
        if self
            .memory_injector_init_failed
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return Ok(());
        }

        if let Err(error) = consolidate_legacy_layer5(&self.paths) {
            warn!(
                error = %error,
                "[Layer 5] Failed to consolidate legacy memory files"
            );
        }

        let mut injector = MemoryInjector::new(InjectionConfig::from(
            self.config.memory.memory_system.layer5.clone(),
        ));

        match injector.load_canonical(&self.paths).await {
            Ok(()) => {
                let count = injector.cache_size();
                if count > 0 {
                    info!(
                        workspace = %self.paths.workspace().display(),
                        files_loaded = count,
                        "[Layer 5] Canonical memory injector initialized with {} sections",
                        count
                    );
                } else {
                    debug!(
                        workspace = %self.paths.workspace().display(),
                        "[Layer 5] Canonical memory injector initialized (no memory found)"
                    );
                }
                self.context_builder.set_memory_injector(injector);
            }
            Err(e) => {
                // Non-fatal: memory injection is optional enhancement
                warn!(
                    workspace = %self.paths.workspace().display(),
                    error = %e,
                    "[Layer 5] Failed to load canonical memory, continuing without persistent memory injection"
                );
            }
        }

        Ok(())
    }

    /// Check if memory injector cache needs refresh.
    pub fn memory_injector_needs_reload(&self) -> bool {
        self.memory_injector_needs_reload
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Signal that memory injector cache needs refresh (called by background tasks).
    pub fn signal_memory_injector_reload(&self) {
        self.memory_injector_needs_reload
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Reload memory injector cache if needed.
    /// This should be called at the start of each conversation turn.
    pub async fn reload_memory_injector_if_needed(&mut self) -> std::io::Result<()> {
        if !self.memory_injector_needs_reload() {
            return Ok(());
        }
        self.memory_injector_init_failed
            .store(false, std::sync::atomic::Ordering::Release);

        use crate::auto_memory::{consolidate_legacy_layer5, InjectionConfig, MemoryInjector};

        if let Err(error) = consolidate_legacy_layer5(&self.paths) {
            warn!(error = %error, "[Layer 5] Failed to consolidate legacy memory during reload");
        }
        let mut injector = MemoryInjector::new(InjectionConfig::from(
            self.config.memory.memory_system.layer5.clone(),
        ));
        injector.load_canonical(&self.paths).await?;

        let count = injector.cache_size();
        info!(
            workspace = %self.paths.workspace().display(),
            files_loaded = count,
            "[Layer 5] Canonical memory injector cache reloaded after extraction"
        );

        self.context_builder.set_memory_injector(injector);
        self.memory_injector_needs_reload
            .store(false, std::sync::atomic::Ordering::Relaxed);

        Ok(())
    }

    /// Get a clone of the reload flag for use in background tasks.
    pub fn memory_injector_reload_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.memory_injector_needs_reload)
    }

    /// Set the capability registry handle for tools.
    pub fn set_capability_registry(&mut self, registry: CapabilityRegistryHandle) {
        self.capability_registry = Some(registry);
    }

    /// Set the core evolution engine handle for tools.
    pub fn set_core_evolution(&mut self, core_evo: CoreEvolutionHandle) {
        self.core_evolution = Some(core_evo);
    }

    pub fn set_evolution_worker(
        &mut self,
        worker: Arc<dyn crate::capability_adapter::EvolutionNotifier>,
    ) {
        self.evolution_worker = Some(worker);
    }

    pub fn set_skill_evolution_worker(
        &mut self,
        worker: Arc<dyn crate::capability_adapter::EvolutionNotifier>,
    ) {
        self.skill_evolution_worker = Some(worker);
    }

    pub fn set_evolution_workflow_store(
        &mut self,
        store: Arc<blockcell_storage::EvolutionWorkflowStore>,
    ) {
        self.evolution_workflow_store = Some(store);
    }

    /// Deprecated: MCP tools are now injected before runtime construction via the shared MCP manager.
    pub async fn mount_mcp_servers(&mut self) {}

    pub(super) fn ghost_learning_enabled(&self) -> bool {
        self.background_tasks_enabled
            && self.config.memory.learning_system.enabled()
            && self.config.agents.ghost.learning.capture_enabled()
    }

    pub(super) fn spawn_pending_ghost_background_reviews(&mut self) {
        if self.ghost_learning_enabled() {
            self.background_task_handles
                .push(spawn_pending_background_reviews(
                    self.paths.clone(),
                    Arc::clone(&self.provider_pool),
                    8,
                    self.config.clone(),
                ));
        }
    }

    fn persist_ghost_learning_boundary(
        &self,
        boundary: GhostLearningBoundary,
        sources: Vec<GhostEpisodeSource>,
    ) -> Result<Option<String>> {
        if !self.ghost_learning_enabled() {
            return Ok(None);
        }
        // 从当前配置刷新 ghost 策略（支持热重载）
        self.learning_coordinator
            .update_ghost_policy(&self.config.agents.ghost.learning);
        let decision = self.learning_coordinator.ghost_decide(&boundary);
        persist_ghost_learning_boundary_with_decision(
            &self.config,
            &self.paths,
            boundary,
            sources,
            decision,
        )
    }

    fn coordinate_boundary_job(
        &self,
        session_key: &str,
        boundary: &str,
        messages: &[ChatMessage],
    ) -> LearningBoundaryJob {
        let history_snapshot = messages.iter().map(chat_message_text).collect::<Vec<_>>();
        for output in [
            LearningOutputRequest::SessionSummary,
            LearningOutputRequest::DurableMemory,
            LearningOutputRequest::UserPreferences,
            LearningOutputRequest::SkillCandidate,
        ] {
            self.learning_coordinator
                .enqueue_boundary_job(LearningBoundaryJob::new(
                    session_key,
                    boundary,
                    history_snapshot.clone(),
                    output,
                ));
        }
        self.learning_coordinator
            .take_boundary_job(session_key, boundary)
            .unwrap_or_else(|| {
                let mut fallback = LearningBoundaryJob::new(
                    session_key,
                    boundary,
                    history_snapshot,
                    LearningOutputRequest::SessionSummary,
                );
                fallback.requested_outputs = vec![
                    LearningOutputRequest::SessionSummary,
                    LearningOutputRequest::DurableMemory,
                    LearningOutputRequest::UserPreferences,
                    LearningOutputRequest::SkillCandidate,
                ];
                fallback
            })
    }

    fn attach_boundary_job(
        &self,
        episode_id: Option<&str>,
        job: &LearningBoundaryJob,
    ) -> Result<()> {
        let Some(episode_id) = episode_id else {
            return Ok(());
        };
        let ledger = blockcell_storage::GhostLedger::open(&self.paths.ghost_ledger_db())?;
        ledger.merge_episode_metadata(
            episode_id,
            &serde_json::json!({
                "learningJob": job,
                "requestedOutputs": job.requested_outputs,
                "classifierCallCount": 0,
                "skillDeepeningCallCount": 0,
            }),
        )?;
        Ok(())
    }

    fn detect_correction_signal_count(user_text: &str) -> u32 {
        let lower = user_text.to_lowercase();
        let cues = [
            "correct", "fix", "instead", "prefer", "wrong", "更正", "改成", "修正", "不要", "优先",
            "正确",
        ];
        if cues.iter().any(|cue| lower.contains(cue)) {
            1
        } else {
            0
        }
    }

    fn detect_preference_correction_count(user_text: &str) -> u32 {
        let lower = user_text.to_lowercase();
        let cues = ["prefer", "use ", "instead", "优先", "改成", "不要", "以后"];
        if cues.iter().any(|cue| lower.contains(cue)) {
            1
        } else {
            0
        }
    }

    pub(super) fn apply_learned_skill_negative_feedback(
        &self,
        session_metadata: &mut serde_json::Value,
        msg: &InboundMessage,
    ) -> Result<()> {
        let correction_count = u32::from(
            Self::detect_correction_signal_count(&msg.content)
                + Self::detect_preference_correction_count(&msg.content)
                > 0,
        );
        if correction_count == 0 {
            return Ok(());
        }
        let Some(skill_name) = active_skill_name_from_metadata(session_metadata) else {
            return Ok(());
        };
        let current = session_metadata
            .get(SESSION_ACTIVE_SKILL_CORRECTIONS_KEY)
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as u32;
        let next = current.saturating_add(correction_count);
        if !session_metadata.is_object() {
            *session_metadata = serde_json::json!({});
        }
        if let Some(map) = session_metadata.as_object_mut() {
            map.insert(
                SESSION_ACTIVE_SKILL_CORRECTIONS_KEY.to_string(),
                serde_json::Value::Number(next.into()),
            );
        }
        if next >= LEARNED_SKILL_DISABLE_THRESHOLD {
            disable_skill_toggle(&self.paths, &skill_name)?;
            if let Some(map) = session_metadata.as_object_mut() {
                map.remove(SESSION_ACTIVE_SKILL_NAME_KEY);
                map.insert(
                    "auto_disabled_skill".to_string(),
                    serde_json::Value::String(skill_name.clone()),
                );
            }
            warn!(
                skill = %skill_name,
                corrections = next,
                "Auto-disabled learned skill after repeated correction"
            );
        }
        Ok(())
    }

    fn latest_role_text(messages: &[ChatMessage], role: &str) -> Option<String> {
        messages
            .iter()
            .rev()
            .find(|msg| msg.role == role)
            .map(chat_message_text)
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
    }

    pub(super) fn capture_turn_end_learning_boundary(
        &self,
        msg: &InboundMessage,
        history: &[ChatMessage],
        final_response: &str,
        tool_call_counts: &HashMap<String, u32>,
        success: bool,
    ) -> Result<Option<String>> {
        if !self.ghost_learning_enabled()
            || matches!(
                msg.channel.as_str(),
                "ghost" | "cron" | "system" | "subagent"
            )
        {
            return Ok(None);
        }

        let final_text = final_response.trim();
        if final_text.is_empty() {
            return Ok(None);
        }

        // 优先使用事件日志中本回合的事实；日志不可用时退回运行时计数。
        let turn_facts = self.current_turn_facts(msg);
        let (tool_call_count, memory_write_count) = match &turn_facts {
            Some(facts) => (facts.tool_calls.len() as u32, facts.memory_writes as u32),
            None => (tool_call_counts.values().copied().sum(), 0),
        };
        let mut sources = vec![
            GhostEpisodeSource {
                source_type: "session".to_string(),
                source_key: msg.session_key(),
                role: "primary".to_string(),
            },
            GhostEpisodeSource {
                source_type: "chat".to_string(),
                source_key: msg.chat_id.clone(),
                role: "context".to_string(),
            },
            GhostEpisodeSource {
                source_type: "history".to_string(),
                source_key: history.len().to_string(),
                role: "summary".to_string(),
            },
        ];
        if let Some(facts) = &turn_facts {
            sources.push(GhostEpisodeSource {
                source_type: "session_events".to_string(),
                source_key: facts.turn_id.clone(),
                role: "facts".to_string(),
            });
        }

        let boundary = GhostLearningBoundary {
            kind: GhostLearningBoundaryKind::TurnEnd,
            session_key: Some(msg.session_key()),
            subject_key: Some(format!("chat:{}:sender:{}", msg.chat_id, msg.sender_id)),
            user_intent_summary: msg.content.clone(),
            assistant_outcome_summary: final_text.to_string(),
            tool_call_count,
            memory_write_count,
            correction_count: Self::detect_correction_signal_count(&msg.content),
            preference_correction_count: Self::detect_preference_correction_count(&msg.content),
            success,
            complexity_score: estimate_turn_complexity_score(&msg.content),
            reusable_lesson: None,
            turn_facts,
        };

        let turn_count = history
            .iter()
            .filter(|message| message.role == "user")
            .count() as u32;
        // 从当前配置刷新 ghost 策略（支持热重载）
        self.learning_coordinator
            .update_ghost_policy(&self.config.agents.ghost.learning);
        let decision = self
            .learning_coordinator
            .ghost_decide_with_turn_count(&boundary, Some(turn_count));

        persist_ghost_learning_boundary_with_decision(
            &self.config,
            &self.paths,
            boundary,
            sources,
            decision,
        )
    }

    /// 读取当前回合在 Session Event Log 中已记录的事实（turn/end 尚未写入）。
    fn current_turn_facts(&self, msg: &InboundMessage) -> Option<blockcell_storage::TurnFacts> {
        use blockcell_storage::SessionEventReader;
        let turn_id = self.current_turn_id.as_deref()?;
        let events = blockcell_storage::JsonlSessionEventLog::new(self.paths.clone())
            .read(&super::persist_session_key_for(msg))
            .map_err(|error| debug!(error = %error, "Failed to read session events for Ghost"))
            .ok()?;
        blockcell_storage::TurnFacts::from_events(&events, turn_id)
    }

    pub(super) async fn capture_pre_compress_learning_boundary(
        &self,
        session_key: &str,
        messages: &[ChatMessage],
    ) -> Result<Option<String>> {
        if !self.ghost_learning_enabled() {
            return Ok(None);
        }
        let job = self.coordinate_boundary_job(session_key, "pre_compress", messages);
        let provider_pre_compress_context = if let Some(manager) =
            self.ghost_memory_lifecycle.as_ref()
        {
            let message_texts = messages.iter().map(chat_message_text).collect::<Vec<_>>();
            let provider_block = manager.on_pre_compress(&message_texts, session_key);
            if !provider_block.trim().is_empty() {
                debug!(session_key = %session_key, "Ghost memory provider contributed pre-compress context");
                Some(truncate_str(&provider_block, 1200))
            } else {
                None
            }
        } else {
            None
        };

        let boundary = GhostLearningBoundary {
            kind: GhostLearningBoundaryKind::PreCompress,
            session_key: Some(session_key.to_string()),
            subject_key: Some(format!("session:{}", session_key)),
            user_intent_summary: Self::latest_role_text(messages, "user")
                .unwrap_or_else(|| "pre-compress boundary".to_string()),
            assistant_outcome_summary: Self::latest_role_text(messages, "assistant")
                .unwrap_or_else(|| "conversation is about to compact".to_string()),
            tool_call_count: messages
                .iter()
                .filter_map(|msg| msg.tool_calls.as_ref().map(|calls| calls.len() as u32))
                .sum(),
            memory_write_count: 0,
            correction_count: 0,
            preference_correction_count: 0,
            success: true,
            complexity_score: 0,
            reusable_lesson: provider_pre_compress_context,
            turn_facts: None,
        };

        let episode_id = self.persist_ghost_learning_boundary(
            boundary,
            vec![GhostEpisodeSource {
                source_type: "session".to_string(),
                source_key: session_key.to_string(),
                role: "primary".to_string(),
            }],
        )?;
        self.attach_boundary_job(episode_id.as_deref(), &job)?;
        Ok(episode_id)
    }

    pub(super) async fn capture_main_session_end_learning_boundary(
        &self,
    ) -> Result<Option<String>> {
        if !self.ghost_learning_enabled() {
            return Ok(None);
        }

        let Some(target) = self.main_session_target.as_ref() else {
            return Ok(None);
        };
        let history = self.session_store.load(&target.session_key)?;
        if history.is_empty() {
            return Ok(None);
        }
        let job = self.coordinate_boundary_job(&target.session_key, "session_end", &history);
        let provider_session_end_context =
            if let Some(manager) = self.ghost_memory_lifecycle.as_ref() {
                let message_texts = history.iter().map(chat_message_text).collect::<Vec<_>>();
                manager.on_session_end(&message_texts, &target.session_key);
                let provider_block =
                    manager.on_session_boundary_context(&message_texts, &target.session_key);
                if !provider_block.trim().is_empty() {
                    Some(truncate_str(&provider_block, 1200))
                } else {
                    None
                }
            } else {
                None
            };

        let boundary = GhostLearningBoundary {
            kind: GhostLearningBoundaryKind::SessionEnd,
            session_key: Some(target.session_key.clone()),
            subject_key: Some(format!("chat:{}", target.chat_id)),
            user_intent_summary: Self::latest_role_text(&history, "user")
                .unwrap_or_else(|| "session end".to_string()),
            assistant_outcome_summary: Self::latest_role_text(&history, "assistant")
                .unwrap_or_else(|| "session end boundary".to_string()),
            tool_call_count: history
                .iter()
                .filter_map(|msg| msg.tool_calls.as_ref().map(|calls| calls.len() as u32))
                .sum(),
            memory_write_count: 0,
            correction_count: 0,
            preference_correction_count: 0,
            success: true,
            complexity_score: 0,
            reusable_lesson: provider_session_end_context,
            turn_facts: None,
        };

        let episode_id = self.persist_ghost_learning_boundary(
            boundary,
            vec![GhostEpisodeSource {
                source_type: "session".to_string(),
                source_key: target.session_key.clone(),
                role: "primary".to_string(),
            }],
        )?;
        self.attach_boundary_job(episode_id.as_deref(), &job)?;
        Ok(episode_id)
    }

    pub(super) async fn capture_session_rotate_learning_boundary(
        &self,
        previous: &MainSessionTarget,
        next_msg: &InboundMessage,
    ) -> Result<Option<String>> {
        if !self.ghost_learning_enabled() {
            return Ok(None);
        }

        let history = self.session_store.load(&previous.session_key)?;
        if history.is_empty() {
            return Ok(None);
        }
        let job = self.coordinate_boundary_job(&previous.session_key, "session_rotate", &history);
        let provider_session_end_context =
            if let Some(manager) = self.ghost_memory_lifecycle.as_ref() {
                let message_texts = history.iter().map(chat_message_text).collect::<Vec<_>>();
                manager.on_session_end(&message_texts, &previous.session_key);
                let provider_block =
                    manager.on_session_boundary_context(&message_texts, &previous.session_key);
                if !provider_block.trim().is_empty() {
                    Some(truncate_str(&provider_block, 1200))
                } else {
                    None
                }
            } else {
                None
            };

        let boundary = GhostLearningBoundary {
            kind: GhostLearningBoundaryKind::SessionRotate,
            session_key: Some(previous.session_key.clone()),
            subject_key: Some(format!("chat:{}", previous.chat_id)),
            user_intent_summary: Self::latest_role_text(&history, "user")
                .unwrap_or_else(|| "session rotate".to_string()),
            assistant_outcome_summary: Self::latest_role_text(&history, "assistant")
                .unwrap_or_else(|| "session rotated to a new active chat".to_string()),
            tool_call_count: history
                .iter()
                .filter_map(|msg| msg.tool_calls.as_ref().map(|calls| calls.len() as u32))
                .sum(),
            memory_write_count: 0,
            correction_count: 0,
            preference_correction_count: 0,
            success: true,
            complexity_score: 0,
            reusable_lesson: Some(match provider_session_end_context {
                Some(context) => format!(
                    "Switched active session from {} to {}\n\n{}",
                    previous.chat_id, next_msg.chat_id, context
                ),
                None => format!(
                    "Switched active session from {} to {}",
                    previous.chat_id, next_msg.chat_id
                ),
            }),
            turn_facts: None,
        };

        let episode_id = self.persist_ghost_learning_boundary(
            boundary,
            vec![
                GhostEpisodeSource {
                    source_type: "session".to_string(),
                    source_key: previous.session_key.clone(),
                    role: "primary".to_string(),
                },
                GhostEpisodeSource {
                    source_type: "chat".to_string(),
                    source_key: previous.chat_id.clone(),
                    role: "context".to_string(),
                },
                GhostEpisodeSource {
                    source_type: "session".to_string(),
                    source_key: next_msg.session_key(),
                    role: "next".to_string(),
                },
            ],
        )?;
        self.attach_boundary_job(episode_id.as_deref(), &job)?;
        Ok(episode_id)
    }

    /// Create a restricted tool registry for subagents (no spawn, no message, no cron).
    pub(crate) fn subagent_tool_registry() -> ToolRegistry {
        use blockcell_tools::alert_rule::AlertRuleTool;
        use blockcell_tools::app_control::AppControlTool;
        use blockcell_tools::audio_transcribe::AudioTranscribeTool;
        use blockcell_tools::browser::BrowseTool;
        use blockcell_tools::camera::CameraCaptureTool;
        use blockcell_tools::chart_generate::ChartGenerateTool;
        use blockcell_tools::community_hub::CommunityHubTool;
        use blockcell_tools::data_process::DataProcessTool;
        use blockcell_tools::email::EmailTool;
        use blockcell_tools::encrypt::EncryptTool;
        use blockcell_tools::exec::ExecTool;
        use blockcell_tools::file_ops::FileOpsTool;
        use blockcell_tools::fs::*;
        use blockcell_tools::http_request::HttpRequestTool;
        use blockcell_tools::image_understand::ImageUnderstandTool;
        use blockcell_tools::knowledge_graph::KnowledgeGraphTool;
        use blockcell_tools::memory::{
            MemoryForgetTool, MemoryManageTool, MemoryQueryTool, MemoryUpsertTool,
        };
        use blockcell_tools::memory_maintenance::MemoryMaintenanceTool;
        use blockcell_tools::network_monitor::NetworkMonitorTool;
        use blockcell_tools::ocr::OcrTool;
        use blockcell_tools::office_write::OfficeWriteTool;
        use blockcell_tools::skills::{ListSkillsTool, SkillManageTool, SkillViewTool};
        use blockcell_tools::stream_subscribe::StreamSubscribeTool;
        use blockcell_tools::system_info::{CapabilityEvolveTool, SystemInfoTool};
        use blockcell_tools::tasks::ListTasksTool;
        use blockcell_tools::termux_api::TermuxApiTool;
        use blockcell_tools::toggle_manage::ToggleManageTool;
        use blockcell_tools::tts::TtsTool;
        use blockcell_tools::video_process::VideoProcessTool;
        use blockcell_tools::web::*;

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool::default()));
        registry.register(Arc::new(WriteFileTool::default()));
        registry.register(Arc::new(EditFileTool::default()));
        registry.register(Arc::new(ListDirTool::default()));
        registry.register(Arc::new(ExecTool::default()));
        registry.register(Arc::new(WebSearchTool));
        registry.register(Arc::new(WebFetchTool));
        registry.register(Arc::new(ListTasksTool));
        registry.register(Arc::new(BrowseTool::default()));
        registry.register(Arc::new(MemoryManageTool));
        registry.register(Arc::new(MemoryQueryTool));
        registry.register(Arc::new(MemoryUpsertTool));
        registry.register(Arc::new(MemoryForgetTool));
        registry.register(Arc::new(ListSkillsTool));
        registry.register(Arc::new(SkillViewTool));
        registry.register(Arc::new(SkillManageTool));
        registry.register(Arc::new(SystemInfoTool));
        registry.register(Arc::new(CapabilityEvolveTool));
        registry.register(Arc::new(CameraCaptureTool));
        registry.register(Arc::new(AppControlTool));
        registry.register(Arc::new(FileOpsTool));
        registry.register(Arc::new(DataProcessTool));
        registry.register(Arc::new(HttpRequestTool));
        registry.register(Arc::new(EmailTool));
        registry.register(Arc::new(AudioTranscribeTool));
        registry.register(Arc::new(ChartGenerateTool));
        registry.register(Arc::new(OfficeWriteTool));
        registry.register(Arc::new(TtsTool));
        registry.register(Arc::new(OcrTool));
        registry.register(Arc::new(ImageUnderstandTool));
        registry.register(Arc::new(VideoProcessTool));
        registry.register(Arc::new(EncryptTool));
        registry.register(Arc::new(NetworkMonitorTool));
        registry.register(Arc::new(KnowledgeGraphTool));
        registry.register(Arc::new(StreamSubscribeTool));
        registry.register(Arc::new(AlertRuleTool));
        registry.register(Arc::new(CommunityHubTool));
        registry.register(Arc::new(MemoryMaintenanceTool));
        registry.register(Arc::new(ToggleManageTool));
        registry.register(Arc::new(TermuxApiTool));
        // No SpawnTool, MessageTool, CronTool — subagents can't spawn or send messages
        registry
    }
} // impl AgentRuntime

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_review_tool_schemas_preserves_parent_schemas_when_compatible() {
        let parent_schemas = vec![
            json!({"type": "function", "function": {"name": "exec", "description": "run cmd", "parameters": {}}}),
            json!({"type": "function", "function": {"name": "skill_manage", "description": "manage skills", "parameters": {}}}),
            json!({"type": "function", "function": {"name": "memory_manage", "description": "manage memory", "parameters": {}}}),
        ];

        assert_eq!(
            super::super::AgentRuntime::resolve_review_tool_schemas(
                &ReviewMode::Skill,
                &parent_schemas
            ),
            parent_schemas
        );
        assert_eq!(
            super::super::AgentRuntime::resolve_review_tool_schemas(
                &ReviewMode::Memory,
                &parent_schemas
            ),
            parent_schemas
        );
        assert_eq!(
            super::super::AgentRuntime::resolve_review_tool_schemas(
                &ReviewMode::Combined,
                &parent_schemas
            ),
            parent_schemas
        );
    }

    #[test]
    fn resolve_review_tool_schemas_falls_back_when_parent_has_no_tools() {
        let resolved =
            super::super::AgentRuntime::resolve_review_tool_schemas(&ReviewMode::Skill, &[]);
        assert_eq!(resolved, crate::forked::build_skill_review_tool_schemas());
    }

    #[test]
    fn format_review_user_prompt_includes_roster_restriction() {
        let prompt = super::super::AgentRuntime::format_review_user_prompt(
            &ReviewMode::Memory,
            MEMORY_REVIEW_PROMPT,
            "",
        );
        assert!(prompt.contains(MEMORY_REVIEW_PROMPT));
        assert!(prompt.contains("memory_manage"));
        assert!(prompt.contains("list_dir"));
    }

    #[test]
    fn nudge_review_request_aligns_system_tools_and_prefix_with_parent_turn() {
        let parent_system = "You are BlockCell.\n\n## Active Skill Cards\n- deploy-helper";
        let parent_snapshot = vec![
            ChatMessage::system(parent_system),
            ChatMessage::user("help me deploy"),
            ChatMessage::assistant("deployed successfully"),
        ];
        let parent_tools = vec![
            json!({"type": "function", "function": {"name": "exec", "description": "Execute command", "parameters": {"type": "object"}}}),
            json!({"type": "function", "function": {"name": "skill_manage", "description": "Manage skills", "parameters": {"type": "object"}}}),
            json!({"type": "function", "function": {"name": "memory_manage", "description": "Manage memory", "parameters": {"type": "object"}}}),
        ];
        let parent_model = "claude-sonnet-4-5";

        let parent_cache_safe = crate::forked::CacheSafeParams::new(parent_system, parent_model)
            .with_fork_messages(parent_snapshot.clone())
            .with_tools(super::super::AgentRuntime::tool_definitions_from_schemas(
                &parent_tools,
            ));

        // 模拟 spawn_review 构造 review 请求参数
        let review_system = parent_snapshot
            .first()
            .filter(|m| m.role == "system")
            .and_then(|m| m.content.as_str())
            .unwrap()
            .to_string();
        let review_tool_schemas = super::super::AgentRuntime::resolve_review_tool_schemas(
            &ReviewMode::Combined,
            &parent_tools,
        );
        let review_cache_safe = crate::forked::CacheSafeParams::new(review_system, parent_model)
            .with_fork_messages(parent_snapshot.clone())
            .with_tools(super::super::AgentRuntime::tool_definitions_from_schemas(
                &review_tool_schemas,
            ));

        // C6: CacheSafeParams 与父轮完全兼容
        assert!(review_cache_safe.is_compatible_with(&parent_cache_safe));
        // C3: 工具列表逐字节一致
        assert_eq!(review_tool_schemas, parent_tools);

        // C1 + C2: 构造出的消息列表只有 1 条 system message，且 review prompt 仅作为末尾 user 消息出现
        let review_user_prompt = super::super::AgentRuntime::format_review_user_prompt(
            &ReviewMode::Combined,
            COMBINED_REVIEW_PROMPT,
            "",
        );
        let built_messages = crate::forked::build_initial_forked_messages(
            &review_cache_safe,
            &[ChatMessage::user(&review_user_prompt)],
            None,
        );
        assert_eq!(
            built_messages.iter().filter(|m| m.role == "system").count(),
            1
        );
        assert_eq!(built_messages[0].content.as_str(), Some(parent_system));
        assert!(!built_messages[0]
            .content
            .as_str()
            .unwrap()
            .contains("Review the conversation above"));
        assert_eq!(built_messages.last().unwrap().role, "user");

        // 防回归断言：若 review 侧重渲染了不同的 system prompt，则兼容性检查立刻失败
        let drifted_cache_safe =
            crate::forked::CacheSafeParams::new("rebuilt general system prompt", parent_model)
                .with_fork_messages(parent_snapshot)
                .with_tools(super::super::AgentRuntime::tool_definitions_from_schemas(
                    &review_tool_schemas,
                ));
        assert!(!drifted_cache_safe.is_compatible_with(&parent_cache_safe));
    }
}
