//! Guarded Tool Pipeline：把安全与观测逻辑从具体工具中集中出来。
//!
//! ```text
//! ToolCall
//!  → guards: Toggle → Policy → DangerousOperation → PathAccess → ShellFullAccess
//!  → tool/pre-execute（listener 可改写参数或拒绝）→ tool/call
//!  → execute（含 shell sandbox escalation 审批与重试）
//!  → tool/post-execute
//!  → classify outcome（自进化上报 / 学习提示）
//!  → tool/result + audit + 学习计数 + Layer4 追踪
//! ```
//!
//! 具体工具只需实现业务 `execute`，不再自行处理 permission / approval / audit。

use super::tool_exec::{
    full_access_retry_args, shell_action_is_run, shell_requests_full_access,
    shell_sandbox_escalation_reason,
};
use super::*;

/// 单次工具调用在管线中流动的状态。
pub(crate) struct ToolInvocation<'a> {
    pub(crate) call: &'a ToolCallRequest,
    pub(crate) msg: &'a InboundMessage,
    /// 经过 guard / listener 处理后真正交给工具的参数。
    pub(crate) args: serde_json::Value,
    /// Policy 阶段已获得用户显式确认，后续审批阶段不再重复询问。
    pub(crate) policy_confirmed: bool,
}

/// 工具执行结果的规范化表示。
pub(crate) struct ToolOutcome {
    pub(crate) result_str: String,
    pub(crate) result_json: serde_json::Value,
    pub(crate) is_error: bool,
    pub(crate) duration_ms: u64,
}

/// Guard 拒绝时直接作为工具结果返回给模型的字符串。
type GuardResult = std::result::Result<(), String>;

#[derive(Debug, Clone, Copy)]
enum ToolGuard {
    Toggle,
    Policy,
    DangerousOperation,
    PathAccess,
    ShellFullAccess,
}

const TOOL_GUARDS: [ToolGuard; 5] = [
    ToolGuard::Toggle,
    ToolGuard::Policy,
    ToolGuard::DangerousOperation,
    ToolGuard::PathAccess,
    ToolGuard::ShellFullAccess,
];

impl AgentRuntime {
    pub(crate) async fn execute_tool_call(
        &mut self,
        tool_call: &ToolCallRequest,
        msg: &InboundMessage,
        active_skill_dir: Option<PathBuf>,
    ) -> String {
        // 管线 future 较大，放到堆上避免把调用方（主循环）的 future 撑大导致栈溢出。
        Box::pin(self.run_tool_pipeline(tool_call, msg, active_skill_dir)).await
    }

    async fn run_tool_pipeline(
        &mut self,
        tool_call: &ToolCallRequest,
        msg: &InboundMessage,
        active_skill_dir: Option<PathBuf>,
    ) -> String {
        let mut inv = ToolInvocation {
            call: tool_call,
            msg,
            args: tool_call.arguments.clone(),
            policy_confirmed: false,
        };
        if let Err(denied) = self.run_tool_guards(&mut inv).await {
            return denied;
        }
        if let Err(denied) = self.dispatch_tool_pre_execute(&mut inv).await {
            return denied;
        }

        let ctx = self.build_tool_context(msg, active_skill_dir);
        let outcome = self.execute_guarded_tool(&mut inv, ctx).await;

        self.after_tool_execute(&inv, &outcome).await;
        let learning_hint = self.classify_tool_outcome(tool_call, &outcome).await;
        self.record_tool_result(&inv, &outcome).await;

        // 在工具结果中追加学习提示，让 LLM 自然地回复用户
        match learning_hint {
            Some(hint) => format!("{}\n\n{}", outcome.result_str, hint),
            None => outcome.result_str,
        }
    }

    async fn run_tool_guards(&mut self, inv: &mut ToolInvocation<'_>) -> GuardResult {
        for guard in TOOL_GUARDS {
            match guard {
                ToolGuard::Toggle => self.guard_disabled_toggles(inv),
                ToolGuard::Policy => self.guard_tool_policy(inv).await,
                ToolGuard::DangerousOperation => self.guard_dangerous_operation(inv).await,
                ToolGuard::PathAccess => self.guard_path_access(inv).await,
                ToolGuard::ShellFullAccess => self.guard_shell_full_access(inv).await,
            }?;
        }
        Ok(())
    }

    /// 非交互通道依据用户原文中的显式确认，交互通道走确认请求。
    async fn approve_dangerous_operation(
        &mut self,
        tool_name: &str,
        items: Vec<String>,
        msg: &InboundMessage,
    ) -> bool {
        if self.confirm_tx.is_none() {
            user_explicitly_confirms_dangerous_op(&msg.content)
        } else {
            self.confirm_dangerous_operation(tool_name, items, msg)
                .await
        }
    }

    /// Hard block: reject disabled tools at execution level (not just prompt filtering)
    fn guard_disabled_toggles(&self, inv: &ToolInvocation<'_>) -> GuardResult {
        let name = &inv.call.name;
        if load_disabled_toggles(&self.paths, "tools").contains(name) {
            return Err(disabled_tool_result(name));
        }
        // Also block disabled skills invoked as tools (skill scripts registered as tools)
        if load_disabled_toggles(&self.paths, "skills").contains(name) {
            return Err(disabled_skill_result(name));
        }
        Ok(())
    }

    async fn guard_tool_policy(&mut self, inv: &mut ToolInvocation<'_>) -> GuardResult {
        match self
            .check_tool_policy(&inv.call.name, &inv.call.arguments, inv.msg)
            .await
        {
            PolicyOutcome::Proceed => Ok(()),
            PolicyOutcome::ProceedConfirmed => {
                inv.policy_confirmed = true;
                Ok(())
            }
            PolicyOutcome::Denied(reason) => Err(serde_json::json!({
                "error": reason,
                "tool": inv.call.name,
                "policy": "tool_policy"
            })
            .to_string()),
        }
    }

    /// Dangerous-operation gate: require explicit user confirmation before executing
    /// self-destructive commands or destructive file operations.
    async fn guard_dangerous_operation(&mut self, inv: &ToolInvocation<'_>) -> GuardResult {
        if inv.policy_confirmed {
            return Ok(());
        }
        let args = &inv.call.arguments;
        match inv.call.name.as_str() {
            "exec" => {
                let Some(cmd) = args.get("command").and_then(|v| v.as_str()) else {
                    return Ok(());
                };
                if is_dangerous_exec_command(cmd) {
                    let interactive = self.confirm_tx.is_some();
                    let items = vec![format!("command: {}", cmd)];
                    if !self
                        .approve_dangerous_operation("exec", items, inv.msg)
                        .await
                    {
                        return Err(dangerous_exec_denied(interactive));
                    }
                }
                Ok(())
            }
            "file_ops" => {
                let field = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("");
                let action = field("action");
                let path = field("path");
                let destination = field("destination");
                let recursive = args
                    .get("recursive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let mut items = Vec::new();
                if action == "delete" && recursive {
                    items.push(format!("file_ops delete recursive=true path={}", path));
                }
                if (action == "delete" || action == "rename" || action == "move")
                    && (is_sensitive_filename(path) || is_sensitive_filename(destination))
                {
                    items.push(format!(
                        "file_ops {} sensitive file (config*) path={} destination={}",
                        action, path, destination
                    ));
                }

                if !items.is_empty()
                    && !self
                        .approve_dangerous_operation("file_ops", items, inv.msg)
                        .await
                {
                    return Err(dangerous_file_ops_denied());
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Check path safety before executing filesystem/exec tools
    async fn guard_path_access(&mut self, inv: &ToolInvocation<'_>) -> GuardResult {
        if self
            .check_path_permission_with_confirmation(
                &inv.call.name,
                &inv.call.arguments,
                inv.msg,
                inv.policy_confirmed,
            )
            .await
        {
            Ok(())
        } else {
            Err(crate::error::path_access_denied(
                &inv.call.name,
                "outside workspace",
            ))
        }
    }

    async fn guard_shell_full_access(&mut self, inv: &ToolInvocation<'_>) -> GuardResult {
        if inv.call.name != "shell"
            || !shell_action_is_run(&inv.args)
            || !shell_requests_full_access(&inv.args)
            || inv.policy_confirmed
        {
            return Ok(());
        }
        let items = vec![format!(
            "full-access command: {}",
            inv.args
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
        )];
        if self
            .approve_dangerous_operation("shell", items, inv.msg)
            .await
        {
            Ok(())
        } else {
            Err(serde_json::json!({
                "error": "full-access shell execution was not approved",
                "tool": "shell",
                "sandbox_policy": "full-access"
            })
            .to_string())
        }
    }

    async fn dispatch_tool_pre_execute(&mut self, inv: &mut ToolInvocation<'_>) -> GuardResult {
        let mut event = AgentEvent::ToolPreExecute {
            tool_name: inv.call.name.clone(),
            call_id: inv.call.id.clone(),
            arguments: std::mem::take(&mut inv.args),
        };
        let decision = self
            .dispatch_event(inv.msg, &inv.msg.session_key(), &mut event)
            .await;
        let AgentEvent::ToolPreExecute { arguments, .. } = event else {
            unreachable!("tool/pre-execute listeners cannot replace the event kind");
        };
        inv.args = arguments;
        match decision {
            EventDecision::Continue => {
                self.dispatch_event(
                    inv.msg,
                    &inv.msg.session_key(),
                    &mut AgentEvent::ToolCall {
                        tool_name: inv.call.name.clone(),
                        call_id: inv.call.id.clone(),
                        arguments: inv.args.clone(),
                    },
                )
                .await;
                Ok(())
            }
            EventDecision::Reject { reason } => Err(serde_json::json!({
                "error": reason,
                "tool": inv.call.name,
                "policy": "event_listener"
            })
            .to_string()),
        }
    }

    fn build_tool_context(
        &self,
        msg: &InboundMessage,
        active_skill_dir: Option<PathBuf>,
    ) -> blockcell_tools::ToolContext {
        // Build TaskManager handle for tools
        let tm_handle: TaskManagerHandle = Arc::new(OriginScopedTaskManager::new(
            self.task_manager.clone(),
            &msg.channel,
            &msg.chat_id,
        ));

        // Build spawn handle for tools
        let spawn_handle = Arc::new(RuntimeSpawnHandle {
            config: self.config.clone(),
            paths: self.paths.clone(),
            task_manager: self.task_manager.clone(),
            outbound_tx: self.outbound_tx.clone(),
            provider_pool: Arc::clone(&self.provider_pool),
            agent_id: resolve_routed_agent_id(&msg.metadata).or_else(|| self.agent_id.clone()),
            event_tx: self.event_tx.clone(),
            origin_account_id: msg.account_id.clone(),
            origin_session_key: msg.session_key(),
            response_cache: self.response_cache.clone(),
            event_emitter: self.system_event_emitter.clone(),
            abort_token: Some(self.abort_token.clone()),
        });

        blockcell_tools::ToolContext {
            workspace: self.paths.workspace(),
            base: self.paths.base.clone(),
            builtin_skills_dir: Some(self.paths.builtin_skills_dir()),
            active_skill_dir,
            session_key: msg.session_key(),
            channel: msg.channel.clone(),
            account_id: msg.account_id.clone(),
            sender_id: Some(msg.sender_id.clone()),
            chat_id: msg.chat_id.clone(),
            config: self.config.clone(),
            permissions: self.build_tool_permissions(
                &msg.channel,
                Some(&msg.sender_id),
                &msg.chat_id,
            ),
            task_manager: Some(tm_handle),
            memory_store: self.memory_store.clone(),
            memory_file_store: self.memory_file_store.clone(),
            ghost_memory_lifecycle: self.ghost_memory_lifecycle.clone().map(|manager| {
                manager as Arc<dyn blockcell_tools::GhostMemoryLifecycleOps + Send + Sync>
            }),
            skill_file_store: self.skill_file_store.clone(),
            session_search: Some(Arc::new(RuntimeSessionSearch::new(
                self.paths.clone(),
                Some(msg.session_key()),
            ))),
            outbound_tx: self.outbound_tx.clone(),
            spawn_handle: Some(spawn_handle),
            capability_registry: self.capability_registry.clone(),
            core_evolution: self.core_evolution.clone(),
            event_emitter: Some(self.system_event_emitter.clone()),
            channel_contacts_file: Some(self.paths.channel_contacts_file()),
            response_cache: Some(
                Arc::new(self.response_cache.clone()) as blockcell_tools::ResponseCacheHandle
            ),
            runtime_handle: self.runtime_handle.clone(),
            agent_identity: blockcell_core::current_agent_context(),
            skill_mutex: Some(self.skill_mutex.clone()),
            agent_type_registry: Some(Arc::new(self.agent_type_registry.clone())
                as blockcell_tools::AgentTypeRegistryHandle),
            evolution_workflow_store: self.evolution_workflow_store.clone().map(|store| {
                Arc::new(EvolutionWorkflowStoreAdapter::new((*store).clone()))
                    as blockcell_tools::EvolutionWorkflowStoreHandle
            }),
        }
    }

    /// 执行工具；shell 在沙箱内失败且需要提权时，审批通过后以 full-access 重试。
    async fn execute_guarded_tool(
        &mut self,
        inv: &mut ToolInvocation<'_>,
        ctx: blockcell_tools::ToolContext,
    ) -> ToolOutcome {
        let tool_name = &inv.call.name;
        let start = std::time::Instant::now();
        let mut result = self
            .execute_runtime_tool_call(tool_name, ctx.clone(), inv.args.clone())
            .await;
        if tool_name == "shell" && shell_action_is_run(&inv.args) {
            if let Some(reason) = shell_sandbox_escalation_reason(&result) {
                let approved = inv.policy_confirmed
                    || self
                        .approve_dangerous_operation(
                            "shell",
                            vec![format!(
                                "sandbox escalation ({reason}): {}",
                                inv.args
                                    .get("command")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or_default()
                            )],
                            inv.msg,
                        )
                        .await;
                if approved {
                    inv.args = full_access_retry_args(&inv.args);
                    result = self
                        .execute_runtime_tool_call(tool_name, ctx, inv.args.clone())
                        .await;
                } else {
                    result = Err(blockcell_core::Error::PermissionDenied(format!(
                        "sandbox escalation was not approved: {reason}"
                    )));
                }
            }
        }
        let duration_ms = start.elapsed().as_millis() as u64;

        let is_error = result.is_err();
        let (result_str, result_json) = match result {
            Ok(val) => (val.to_string(), val),
            Err(e) => {
                let err_str = format!("Error: {}", e);
                (err_str.clone(), serde_json::json!({"error": err_str}))
            }
        };
        ToolOutcome {
            result_str,
            result_json,
            is_error,
            duration_ms,
        }
    }

    /// tool/post-execute 事件以及依赖执行结果的运行时副作用（技能热加载、索引刷新）。
    async fn after_tool_execute(&mut self, inv: &ToolInvocation<'_>, outcome: &ToolOutcome) {
        let tool_call = inv.call;
        self.dispatch_event(
            inv.msg,
            &inv.msg.session_key(),
            &mut AgentEvent::ToolPostExecute {
                tool_name: tool_call.name.clone(),
                call_id: tool_call.id.clone(),
                arguments: inv.args.clone(),
                result: outcome.result_json.clone(),
                is_error: outcome.is_error,
                duration_ms: outcome.duration_ms,
            },
        )
        .await;

        if outcome.is_error {
            return;
        }

        // Detect writes to the skills directory and trigger hot-reload + Dashboard refresh
        if matches!(
            tool_call.name.as_str(),
            "write_file" | "edit_file" | "skill_manage"
        ) {
            if let Some(path_str) = tool_call.arguments.get("path").and_then(|v| v.as_str()) {
                let resolved = self.resolve_path(path_str);
                let skills_dir = self.paths.skills_dir();
                let in_skills = resolved.starts_with(&skills_dir)
                    || resolved.canonicalize().ok().is_some_and(|c| {
                        skills_dir
                            .canonicalize()
                            .ok()
                            .is_some_and(|sd| c.starts_with(&sd))
                    });
                if in_skills {
                    info!(path = %path_str, "🔄 Detected write to skills directory, reloading...");
                    let new_skills = self.context_builder.reload_skills();
                    if !new_skills.is_empty() {
                        info!(skills = ?new_skills, "🔄 Hot-reloaded new skills");
                    }
                    // 刷新 Skill 索引摘要 (使下次 LLM 调用获取最新 Skill 列表)
                    self.context_builder.refresh_skill_index_summary();
                    // Always broadcast so Dashboard refreshes (even for updates to existing skills)
                    if let Some(ref event_tx) = self.event_tx {
                        let event = serde_json::json!({
                            "type": "skills_updated",
                            "new_skills": new_skills,
                        });
                        let _ = event_tx.send(event.to_string());
                    }
                }
            }
        }

        // Detect skill_manage changes and refresh Skill index summary
        if tool_call.name == "skill_manage" {
            let action = tool_call
                .arguments
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if matches!(
                action,
                "create" | "patch" | "delete" | "edit" | "write_file" | "remove_file"
            ) {
                debug!(
                    action = action,
                    "🔄 skill_manage modified skills, refreshing index summary"
                );
                self.context_builder.refresh_skill_index_summary();
            }
        }
    }

    /// 结果归类：失败时上报自进化并生成学习提示，同时上报灰度统计。
    async fn classify_tool_outcome(
        &mut self,
        tool_call: &ToolCallRequest,
        outcome: &ToolOutcome,
    ) -> Option<String> {
        let mut learning_hint: Option<String> = None;
        if outcome.is_error {
            let is_unknown_tool = outcome.result_str.contains("Unknown tool:");

            if is_unknown_tool {
                learning_hint = Some(format!(
                    "[系统] 工具 `{}` 未注册/不可用（Unknown tool）。这不是可通过技能自进化修复的问题。\
                    请改用已存在的工具完成任务，或提示用户安装/启用对应工具。",
                    tool_call.name
                ));
            } else if let Some(evo_service) = self.context_builder.evolution_service() {
                // OpenClaw skill 不触发自进化
                let is_openclaw = self
                    .context_builder
                    .skill_manager()
                    .is_some_and(|sm| sm.is_tool_from_openclaw(&tool_call.name));
                if is_openclaw {
                    debug!(
                        tool = %tool_call.name,
                        "Skipping evolution for OpenClaw skill"
                    );
                } else {
                    // Preserve any legacy top-level Rhai asset as supplemental evolution context.
                    let source_snippet = self
                        .context_builder
                        .skill_manager()
                        .and_then(|sm| sm.get(&tool_call.name))
                        .and_then(|skill| skill.load_rhai());
                    match evo_service
                        .report_error(&tool_call.name, &outcome.result_str, source_snippet, vec![])
                        .await
                    {
                        Ok(report) => {
                            if report.evolution_triggered.is_some() {
                                if let Some(ref worker) = self.skill_evolution_worker {
                                    worker.notify();
                                }
                                learning_hint = Some(format!(
                                    "[系统] 技能 `{}` 执行失败，已自动触发进化学习。\
                                请向用户坦诚说明：你暂时还不具备这个技能，但已经开始学习，\
                                学会后会自动生效。同时尝试用其他方式帮助用户解决当前问题。",
                                    tool_call.name
                                ));
                            } else if report.evolution_in_progress {
                                learning_hint = Some(format!(
                                    "[系统] 技能 `{}` 执行失败，该技能正在学习改进中。\
                                请告诉用户：这个技能正在学习中，请稍后再试。",
                                    tool_call.name
                                ));
                            }
                        }
                        Err(e) => {
                            debug!(error = %e, "Evolution report_error failed");
                        }
                    }
                }
            }
        }
        // 报告调用结果给灰度统计（OpenClaw skill 跳过）
        if let Some(evo_service) = self.context_builder.evolution_service() {
            let is_openclaw = self
                .context_builder
                .skill_manager()
                .is_some_and(|sm| sm.is_tool_from_openclaw(&tool_call.name));
            if !is_openclaw {
                let reported_name = tool_call.name.clone();
                evo_service
                    .report_skill_call(&reported_name, outcome.is_error)
                    .await;
            }
        }
        learning_hint
    }

    /// tool/result 事件、审计日志、学习计数与 Layer4 文件追踪。
    async fn record_tool_result(&mut self, inv: &ToolInvocation<'_>, outcome: &ToolOutcome) {
        let tool_call = inv.call;
        let msg = inv.msg;
        self.dispatch_event(
            msg,
            &msg.session_key(),
            &mut AgentEvent::ToolResult {
                tool_name: tool_call.name.clone(),
                call_id: tool_call.id.clone(),
                result: outcome.result_json.clone(),
                is_error: outcome.is_error,
                duration_ms: outcome.duration_ms,
            },
        )
        .await;

        // Log to audit
        self.audit_logger.set_session_id(&msg.session_key());
        let _ = self.audit_logger.log_tool_call(
            &tool_call.name,
            inv.args.clone(),
            outcome.result_json.clone(),
            &msg.session_key(),
            None, // trace_id can be added later
            Some(outcome.duration_ms),
        );

        // Skill Nudge: 两个独立计数器 (Skill + Memory)
        // 与 Hermes 一致: 只有 skill_manage 写操作重置 Skill 计数器 (view/list_skills 等只读操作不重置)
        // 与 Hermes 一致: 只有 memory 写操作重置 Memory 计数器 (memory_query 等只读操作不重置)
        let tool_name_str = tool_call.name.as_str();
        let is_skill_write_tool = tool_name_str == "skill_manage"
            && matches!(
                tool_call
                    .arguments
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                "create" | "patch" | "edit" | "delete" | "write_file" | "remove_file"
            );
        let is_memory_write_tool = matches!(
            tool_name_str,
            "memory_manage" | "memory_upsert" | "memory_forget" | "auto_memory"
        );

        // Skill/Memory write tools reset corresponding counters via learning coordinator
        if is_skill_write_tool {
            self.learning_coordinator.reset_skill();
        }
        if is_memory_write_tool {
            self.learning_coordinator.reset_memory();
        }

        // Layer 4: Track file reads for Post-Compact recovery
        // 追踪多种文件访问工具的结果，用于 Compact 后恢复
        if !outcome.is_error {
            let result_str = &outcome.result_str;
            let file_content_to_track: Option<(std::path::PathBuf, &str)> =
                match tool_call.name.as_str() {
                    "read_file" => {
                        // read_file: 直接追踪文件内容
                        if let Some(path_str) =
                            tool_call.arguments.get("path").and_then(|v| v.as_str())
                        {
                            Some((self.resolve_path(path_str), result_str))
                        } else {
                            None
                        }
                    }
                    "grep" | "rg" => {
                        // grep/rg: 追踪搜索路径和匹配结果
                        let path = tool_call
                            .arguments
                            .get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or(".");
                        Some((self.resolve_path(path), result_str))
                    }
                    "glob" => {
                        // glob: 追踪匹配的文件列表
                        let path = tool_call
                            .arguments
                            .get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or(".");
                        Some((self.resolve_path(path), result_str))
                    }
                    _ => None,
                };

            if let Some((path, content)) = file_content_to_track {
                if let Some(memory_system) = self.memory_system.as_mut() {
                    memory_system.record_file_read(path.clone(), content);
                    debug!(path = %path.display(), tool = %tool_call.name, "[layer4] Tracked file access for recovery");
                }
            }
        }
    }
}
