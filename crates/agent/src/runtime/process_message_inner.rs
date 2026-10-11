use super::*;

impl AgentRuntime {
    pub(crate) async fn process_message_inner(&mut self, msg: InboundMessage) -> Result<String> {
        let mut metrics = ProcessingMetrics::new();
        let session_key = msg.session_key();
        let cron_deliver_target = resolve_cron_deliver_target(&msg);
        let persist_session_key = persist_session_key_for(&msg);
        info!(session_key = %session_key, channel = %msg.channel, "Processing message");
        info!(target: "chat::user", content = %msg.content, "User input");
        self.update_main_session_target(&msg).await;
        if let Some(manager) = self.ghost_memory_lifecycle.as_ref() {
            // cron 投递场景下使用 persist_session_key（目标会话），
            // 确保学习/召回归属到目标会话而非源会话
            let turn_number = self
                .session_store
                .load(&persist_session_key)
                .map(|history| {
                    history
                        .iter()
                        .filter(|message| message.role == "user")
                        .count() as u32
                        + 1
                })
                .unwrap_or(1);
            manager.on_turn_start(turn_number, &msg.content, &persist_session_key);
        }

        // Learning Coordinator: record user turn (replaces skill_nudge_engine.record_user_turn)
        // Only real user messages increment counters (not cron/system/heartbeat)
        if msg.channel != "system" && msg.channel != "cron" {
            self.learning_coordinator.on_turn_start(true);
        }

        // ── Refresh memory injector cache if Layer 5 extraction completed ──
        if let Err(e) = self.reload_memory_injector_if_needed().await {
            warn!(error = %e, "[Layer 5] Failed to reload memory injector cache");
        }

        // ── Record sender as a known channel contact (for cross-channel lookup) ──
        if msg.channel != "ws" && msg.channel != "cli" && msg.channel != "system" {
            let sender_name = msg
                .metadata
                .get("sender_nick")
                .and_then(|v| v.as_str())
                .or_else(|| msg.metadata.get("username").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            let chat_type = match msg
                .metadata
                .get("conversation_type")
                .and_then(|v| v.as_str())
            {
                Some("1") => "private",
                Some("2") => "group",
                _ => {
                    if msg
                        .metadata
                        .get("is_group")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        "group"
                    } else if msg.sender_id == msg.chat_id {
                        "private"
                    } else {
                        "group"
                    }
                }
            };
            if let Err(error) = self
                .channel_contacts
                .upsert(blockcell_storage::ChannelContact {
                    channel: msg.channel.clone(),
                    chat_id: msg.chat_id.clone(),
                    sender_id: msg.sender_id.clone(),
                    name: sender_name,
                    chat_type: chat_type.to_string(),
                    last_active: chrono::Utc::now().to_rfc3339(),
                })
            {
                tracing::warn!(error = %error, "Failed to persist channel contact");
            }
        }

        // ── Cron reminder fast path: deliver directly without LLM ──
        if let Some(final_response) = self.try_cron_reminder_fast_path(&msg).await {
            return Ok(final_response);
        }

        // ── Handle manual compact request from /compact command ──
        if msg.content == "__COMPACT_REQUEST__" {
            self.handle_manual_compact_request(
                &msg,
                &session_key,
                &persist_session_key,
                &mut metrics,
            )
            .await?;
            return Ok(String::new());
        }

        // 使用 persist_session_key 加载历史：cron 转发场景下 persist_session_key 是目标会话，
        // session_key 是来源会话。历史应从目标会话加载，避免源会话历史覆盖目标会话。
        let mut history = self.session_store.load(&persist_session_key)?;
        let mut session_metadata = self.session_store.load_metadata(&persist_session_key)?;
        let is_new_session = history.is_empty();

        self.current_turn_id = Some(uuid::Uuid::new_v4().to_string());
        self.current_step = None;
        self.turn_step_counter = 0;
        self.dispatch_event(
            &msg,
            &persist_session_key,
            &mut AgentEvent::TurnStart {
                user_input: msg.content.clone(),
                is_new_session,
            },
        )
        .await;

        // Dream session count：使用独立 marker 文件 + create_new 实现原子计数
        // 解决 metadata save 的 TOCTOU 竞态：两个 runtime 可能同时读到 dream_counted=false，
        // 都执行 save_with_metadata（File::create 覆盖写），导致重复递增。
        // mark_dream_counted 使用 create_new(true) 创建独立 marker 文件，只有一个调用者能成功。
        if self.session_store.mark_dream_counted(&persist_session_key) {
            if let Err(e) =
                crate::dream_state::increment_dream_session_count(&self.paths.base).await
            {
                warn!(
                    error = %e,
                    session_key = %persist_session_key,
                    "[dream] 会话计数递增失败，清除 marker 以便重试"
                );
                self.session_store
                    .clear_dream_counted_marker(&persist_session_key);
            }
        }
        if let Err(err) = self.apply_learned_skill_negative_feedback(&mut session_metadata, &msg) {
            warn!(
                error = %err,
                session_key = %persist_session_key,
                "Learned skill negative feedback handling failed"
            );
        }

        // Layer 2: 时间触发的轻量压缩
        // 检查会话最后更新时间，如果超过阈值则清理旧工具结果
        // 注意：updated_at 存储在 session 文件外层 metadata 行中，不在 metadata 子对象内，
        // 必须使用 load_timestamps() 而非从 session_metadata 读取
        let time_config = TimeBasedMCConfig::from(self.config.memory.memory_system.layer2.clone());
        if let Ok((_, Some(updated_at_str))) =
            self.session_store.load_timestamps(&persist_session_key)
        {
            if let Ok(updated_at) = chrono::DateTime::parse_from_rfc3339(&updated_at_str) {
                let last_assistant_timestamp = Some(updated_at.with_timezone(&chrono::Utc));
                let projector = HistoryProjector::new(&history);

                // 应用时间触发的轻量压缩
                if let Some(compacted) = projector.time_based_microcompact(
                    last_assistant_timestamp,
                    None, // 主线程来源
                    &time_config,
                ) {
                    tracing::info!(
                        original_count = history.len(),
                        compacted_count = compacted.len(),
                        gap_threshold_minutes = time_config.gap_threshold_minutes,
                        "[layer2] time-based microcompact applied"
                    );
                    history = compacted;
                }
            }
        }

        // Auto-set session display name from first user message
        if history.is_empty() {
            if let Some(new_name) = self
                .session_store
                .set_session_name_if_new(&persist_session_key, &msg.content)
            {
                if msg.channel == "ws" {
                    if let Some(ref event_tx) = self.event_tx {
                        let event = serde_json::json!({
                            "type": "session_renamed",
                            "channel": msg.channel,
                            "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                            "chat_id": msg.chat_id,
                            "name": new_name,
                        });
                        let _ = event_tx.send(event.to_string());
                    }
                }
            }
        }

        // 配置文件中有自定义意图规则时，叠加到内置规则上；否则使用全局单例（避免重复编译正则）
        let config_intent_rules = self
            .config
            .intent_router
            .as_ref()
            .map(|r| r.intent_rules.as_slice())
            .unwrap_or(&[]);
        let _classifier_owned;
        let classifier: &crate::intent::IntentClassifier = if config_intent_rules.is_empty() {
            crate::intent::IntentClassifier::global()
        } else {
            _classifier_owned =
                crate::intent::IntentClassifier::with_extra_rules(config_intent_rules);
            &_classifier_owned
        };

        // Load disabled toggles for filtering
        let (disabled_tools, disabled_skills) =
            load_disabled_toggles_pair(&self.paths, "tools", "skills");
        let recent_skill_name = continued_skill_name(&session_metadata, &history);
        let _ = self.context_builder.reload_skills();
        let skill_cards = self
            .context_builder
            .skill_manager()
            .map(|manager| manager.list_enabled_skill_cards(&disabled_skills))
            .unwrap_or_default();

        let decision_timer = ScopedTimer::new();
        let decision = self
            .decide_interaction(
                &msg,
                &disabled_skills,
                classifier,
                &history,
                &session_metadata,
            )
            .await?;
        metrics.record_decision(decision_timer.elapsed_ms());
        if let Some(result) = self
            .execute_decided_skill_route(&decision, &msg, &persist_session_key)
            .await
        {
            return result;
        }

        let available_tools: HashSet<String> = self
            .tool_registry
            .model_visible_tool_names()
            .into_iter()
            .collect();

        let routed_agent_id = self.agent_id.as_deref();
        let mut tool_names = resolve_effective_tool_names(
            &self.config,
            decision.mode,
            routed_agent_id,
            decision.active_skill.as_ref(),
            &decision.chat_intents,
            &available_tools,
        );

        if tool_names.is_empty() && !matches!(decision.mode, InteractionMode::Chat) {
            tool_names = global_core_tool_names();
            tool_names.retain(|name| available_tools.contains(name));
        }

        // Ghost routine: ensure required tools are always available.
        // Rationale: intent classification may treat the routine prompt as Chat, producing zero tools,
        // which would cause the LLM to think tools are unavailable.
        if msg.metadata.get("ghost").and_then(|v| v.as_bool()) == Some(true) {
            let required = [
                "community_hub",
                "memory_maintenance",
                "memory_query",
                "memory_upsert",
                "list_dir",
                "read_file",
                "file_ops",
                "notification",
            ];
            for name in required {
                if !tool_names.iter().any(|tool_name| tool_name == name) {
                    tool_names.push(name.to_string());
                }
            }
        }

        if !skill_cards.is_empty()
            && !tool_names
                .iter()
                .any(|name| name == ACTIVATE_SKILL_TOOL_NAME)
        {
            tool_names.push(ACTIVATE_SKILL_TOOL_NAME.to_string());
        }

        let provider_tool_schemas = ghost_memory_provider_tool_schemas(
            self.ghost_memory_lifecycle.as_deref(),
            &disabled_tools,
        );
        let provider_tool_names = provider_tool_schemas
            .iter()
            .filter_map(|schema| {
                schema
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect::<Vec<_>>();
        tool_names.extend(provider_tool_names);

        tool_names.sort();
        tool_names.dedup();

        // Collect tool-specific prompt rules from the registry for actually loaded tools.
        let mode_names: Vec<String> = match decision.mode {
            InteractionMode::Skill => decision
                .active_skill
                .as_ref()
                .map(|skill| vec![format!("Skill:{}", skill.name)])
                .unwrap_or_else(|| vec!["Skill".to_string()]),
            InteractionMode::Chat => vec!["Chat".to_string()],
            InteractionMode::Coding => vec!["Coding".to_string()],
            InteractionMode::General => vec!["General".to_string()],
        };
        let prompt_ctx = blockcell_tools::PromptContext {
            channel: &msg.channel,
            intents: &mode_names,
            default_timezone: self.config.default_timezone.as_deref(),
        };
        let tool_name_refs: Vec<&str> = tool_names.iter().map(|s| s.as_str()).collect();
        let mut tool_prompt_rules = self
            .tool_registry
            .get_prompt_rules(&tool_name_refs, &prompt_ctx);
        // MCP meta-rule: inject if any loaded tool is an MCP tool (name contains "__")
        if tool_names
            .iter()
            .any(|t| t.contains("__") || t == blockcell_tools::mcp::search::MCP_SEARCH_TOOL_NAME)
        {
            tool_prompt_rules.push("- **MCP (Model Context Protocol)**: blockcell **已内置 MCP 客户端支持**，可连接任意 MCP 服务器（SQLite、GitHub、文件系统、数据库等）。MCP 工具会以 `<serverName>__<toolName>` 格式出现在工具列表中。若用户询问 MCP 功能或当前工具列表中无 MCP 工具，说明尚未配置 MCP 服务器，请引导用户使用 `blockcell mcp add <template>` 快捷添加，或直接编辑 `~/.blockcell/mcp.json` / `~/.blockcell/mcp.d/*.json`。例如：`blockcell mcp add sqlite --db-path /tmp/test.db`，重启后即可使用。".to_string());
        }

        // Build messages for LLM with skill-first mode prompt.
        // Note: build_messages_for_mode_with_channel appends the current user message from user_content,
        // so we pass history WITHOUT the current user message to avoid duplication.
        let pending_intent = msg
            .metadata
            .get("media_pending_intent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // 使用 persist_session_key 构建上下文：cron delivery 下 file memory snapshot
        // 应基于目标会话而非来源会话
        let mut messages = self
            .context_builder
            .build_messages_for_session_mode_with_channel(
                &persist_session_key,
                &history,
                &msg.content,
                &msg.media,
                decision.mode,
                decision.active_skill.as_ref(),
                &disabled_skills,
                &disabled_tools,
                &msg.channel,
                pending_intent,
                &tool_names,
                &tool_prompt_rules,
            );
        if decision.active_skill.is_none() {
            inject_skill_cards_into_system_prompt(
                &mut messages,
                &skill_cards,
                recent_skill_name.as_deref(),
            );
        }

        // 注入当前后台任务状态到 system prompt
        // 让 LLM 知道哪些 typed agent 任务正在运行，避免基于过时对话历史误判
        let prompt_injected_completed_task_ids =
            inject_running_tasks_into_system_prompt(&mut messages, &self.task_manager).await;

        // Now add user message to history for session persistence
        history.push(ChatMessage::user(&msg.content));

        let runtime_task_id = msg
            .metadata
            .get("runtime_task_id")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let mut checkpoint_turn = 0_u32;
        if let Some(task_id) = runtime_task_id.as_deref() {
            let checkpoint = crate::checkpoint::TaskCheckpoint::new_for_origin(
                task_id,
                history.clone(),
                checkpoint_turn,
                &msg.channel,
                &msg.chat_id,
                &persist_session_key,
            );
            if let Err(e) =
                crate::checkpoint::CheckpointManager::new(&self.paths.workspace()).save(&checkpoint)
            {
                warn!(task_id = %task_id, error = %e, "Failed to save initial task checkpoint");
            }
        }

        // Layer 4: Initialize memory system if needed
        // 使用 persist_session_key（非 session_key）：cron delivery/转发场景下
        // Session Memory 文件、pending marker、.session_memory_state.json
        // 应写入目标会话目录，而非来源会话
        let needs_memory_system_init = self
            .memory_system
            .as_ref()
            .map(|memory_system| memory_system.session_id() != persist_session_key)
            .unwrap_or(true);
        if needs_memory_system_init {
            if let Err(e) = self.init_memory_system(persist_session_key.clone()).await {
                warn!(error = %e, "[layer4] Failed to initialize memory system");
            }
        }

        // Layer 5: Initialize memory injector if needed (load persistent memory files)
        if self.context_builder.memory_injector().is_none() {
            if let Err(e) = self.init_memory_injector().await {
                warn!(error = %e, "[layer5] Failed to initialize memory injector");
            }
        }

        // Get tool schemas from resolved tool names
        let mut tools = if tool_names.is_empty() {
            // Chat mode: no tools
            vec![]
        } else {
            let tool_name_refs: Vec<&str> = tool_names.iter().map(String::as_str).collect();
            let mut schemas = self.tool_registry.get_tiered_schemas(
                &tool_name_refs,
                blockcell_tools::registry::GLOBAL_CORE_TOOL_NAMES,
            );

            if !disabled_tools.is_empty() {
                schemas.retain(|schema| {
                    let name = schema
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    !disabled_tools.contains(name)
                });
            }
            schemas
        };

        // 动态注入 agent_type_registry 中所有 agent 类型到 agent 工具的 description
        // 让 LLM 看到自定义 agent（来自 workspace/agents/）的名称和用途
        if let Some(agent_schema) = tools.iter_mut().find(|s| {
            s.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                == Some("agent")
        }) {
            let mut type_list = String::from(
                "Launch a new agent to handle complex, multi-step tasks autonomously.\n\n\
                 Available agent types:\n",
            );
            for (name, def) in self.agent_type_registry.iter() {
                type_list.push_str(&format!("- {}: {}\n", name, def.when_to_use));
            }
            type_list.push_str(
                "\nOmit subagent_type for fork mode (inherits parent context, shares prompt cache, synchronous). \
                 Specify subagent_type for typed agents (background execution, returns task_id).",
            );
            if let Some(func) = agent_schema.get_mut("function") {
                if let Some(desc) = func.get_mut("description") {
                    *desc = serde_json::Value::String(type_list);
                }
            }
        }

        if let Some(schema) = build_activate_skill_tool_schema(&skill_cards) {
            tools.push(schema);
        }
        tools.extend(provider_tool_schemas);
        info!(
            mode = ?decision.mode,
            active_skill = decision.active_skill.as_ref().map(|s| s.name.as_str()),
            tool_count = tools.len(),
            disabled_tools = disabled_tools.len(),
            disabled_skills = disabled_skills.len(),
            "Tools loaded for interaction mode"
        );

        // Main loop with max iterations
        let max_iterations = self.config.agents.defaults.max_tool_iterations;
        let tools_max_iterations = self
            .config
            .agents
            .defaults
            .max_tool_iterations_by_tool
            .clone();
        let mut tool_call_counts: HashMap<String, u32> = HashMap::new();
        let mut over_iteration: bool = false;
        let mut current_messages = messages;

        // Layer 1: 消息级别预算检查
        // 如果工具结果总和超过预算，持久化最大的结果
        if let Some(memory_system) = self.memory_system.as_ref() {
            let candidates =
                crate::response_cache::collect_tool_result_candidates(&current_messages);
            if !candidates.is_empty() {
                let total_size: usize = candidates.iter().map(|c| c.size).sum();
                let budget = self
                    .memory_system
                    .as_ref()
                    .map(|ms| ms.config().layer1.max_tool_results_per_message_chars)
                    .unwrap_or(crate::response_cache::MAX_TOOL_RESULTS_PER_MESSAGE_CHARS);
                let preview_size_chars = self
                    .memory_system
                    .as_ref()
                    .map(|ms| ms.config().layer1.preview_size_chars)
                    .unwrap_or(crate::response_cache::PREVIEW_SIZE_CHARS);

                if total_size > budget {
                    debug!(
                        total_size = total_size,
                        budget = budget,
                        candidates_count = candidates.len(),
                        "[layer1] Message budget exceeded, applying budget"
                    );

                    let state = memory_system.content_replacement_state().clone();
                    let mut state_mut = state.clone();

                    // 使用 self.paths.workspace() 而非 self.paths.base，
                    // 保证写入的 .tool_results 目录与 session_recall 读取、
                    // cleanup_tool_results 清理共用同一个根目录（base/workspace/.tool_results）
                    // 使用 persist_session_key：cron delivery 下 history 来自目标会话，
                    // 大工具结果也应写入目标会话目录，否则后续 session_recall 找不到
                    current_messages = crate::response_cache::apply_budget_async(
                        &current_messages,
                        &candidates,
                        &mut state_mut,
                        budget,
                        &self.paths.workspace(),
                        &persist_session_key,
                        preview_size_chars,
                    )
                    .await;

                    // 更新状态
                    if let Some(ms) = self.memory_system.as_mut() {
                        *ms.content_replacement_state_mut() = state_mut;
                    }
                }
            }
        }

        // Layer 4: 第一次 LLM 调用前的 Compact 检查
        // 如果从磁盘恢复的历史已经超过阈值，先压缩再进入主循环
        {
            let estimated_tokens = estimate_messages_tokens(&current_messages);
            // Update Layer 4 token usage metrics
            let trigger_compact = if let Some(memory_system) = self.memory_system.as_ref() {
                self.record_context_window_estimate(
                    &persist_session_key,
                    estimated_tokens,
                    memory_system.config().token_budget,
                    memory_system.config().layer4.compact_threshold_ratio,
                );
                crate::memory_event!(
                    layer4,
                    token_usage,
                    estimated_tokens,
                    memory_system.config().token_budget,
                    memory_system.config().layer4.compact_threshold_ratio
                );
                if memory_system.should_compact(estimated_tokens) {
                    info!(
                        estimated_tokens,
                        token_budget = memory_system.config().token_budget,
                        threshold = memory_system.config().layer4.compact_threshold_ratio,
                        "[layer4] Pre-loop compact check triggered"
                    );
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if trigger_compact
                && self
                    .apply_layer4_compact_in_loop(
                        &mut current_messages,
                        &mut history,
                        &msg,
                        &persist_session_key,
                        "pre-loop",
                    )
                    .await
            {
                metrics.record_compression();
            }
        }

        let recall_mode = match decision.mode {
            InteractionMode::Chat => blockcell_core::config::MemoryRecallMode::Chat,
            InteractionMode::General | InteractionMode::Coding => {
                blockcell_core::config::MemoryRecallMode::General
            }
            InteractionMode::Skill => blockcell_core::config::MemoryRecallMode::Skill,
        };
        let ghost_recall_context_block =
            if should_inject_ghost_recall(&self.config, &msg, recall_mode) {
                if let Some(manager) = self.ghost_memory_lifecycle.as_ref() {
                    let learning = &self.config.agents.ghost.learning;
                    manager.prefetch_all_as_context_block(
                        &msg.content,
                        &session_key,
                        learning.recall_max_items as usize,
                        learning.recall_token_budget as usize,
                    )
                } else {
                    None
                }
            } else {
                None
            };

        let mut final_response = String::new();
        let mut llm_failed_after_retries = false;
        let mut message_tool_sent_media = false;
        let mut tool_fail_counts: HashMap<String, u32> = HashMap::new();
        let mut consecutive_failure_tracker = ConsecutiveToolFailureTracker::default();
        let mut resource_missing_hints_sent: HashSet<String> = HashSet::new();
        let mut should_throttle_next_tool_round = false;
        let mut saw_rate_limit_this_turn = false;
        // Collect media paths produced by tools (screenshots, generated images, etc.)
        let mut collected_media: Vec<String> = Vec::new();

        // Schema cache flag: tools are loaded once before the loop.
        // Only dynamic supplement (below) mutates the `tools` vec — no redundant reload.
        let mut _schema_cache_dirty = false;

        // 延迟 Review 状态 (与 Hermes 一致: 在响应发送后触发后台 Review)
        let mut deferred_review_mode: Option<ReviewMode> = None;
        let mut deferred_review_snapshot: Vec<ChatMessage> = Vec::new();
        let mut deferred_review_reservation: Option<LearningReviewReservationGuard> = None;

        // Memory Nudge: check before LLM loop (replaces skill_nudge_engine.check_memory_nudge)
        // Memory nudge is based on user turns, not tool iterations
        {
            let has_memory_store = self.memory_file_store.is_some();
            if let Some(_memory_trigger) = self
                .learning_coordinator
                .check_memory_nudge(has_memory_store)
            {
                deferred_review_mode = Some(ReviewMode::Memory);
                deferred_review_snapshot = current_messages.clone();
                deferred_review_reservation = Some(LearningReviewReservationGuard::new(
                    Arc::clone(&self.learning_coordinator),
                ));
            }
        }

        loop {
            let step = self.begin_step();
            debug!(iteration = ?tool_call_counts, "LLM call iteration");
            // Learning Coordinator: record iteration (replaces skill_nudge_engine.record_iteration)
            self.learning_coordinator.record_iteration();
            self.dispatch_event(
                &msg,
                &persist_session_key,
                &mut AgentEvent::AgentPreStep { iteration: step },
            )
            .await;
            self.dispatch_event(
                &msg,
                &persist_session_key,
                &mut AgentEvent::StepStart { iteration: step },
            )
            .await;

            if let Some(plan) =
                blockcell_tools::plan::take_changed_plan_context(&persist_session_key)
            {
                current_messages.push(ChatMessage::user(&format!(
                    "<runtime-context>\n{}\n</runtime-context>",
                    plan
                )));
            }
            if let Some(environment) = self
                .context_builder
                .incremental_project_environment(&persist_session_key)
            {
                current_messages.push(ChatMessage::user(&format!(
                    "<runtime-context>\n{}\n</runtime-context>",
                    environment
                )));
            }

            debug!(
                iteration = ?tool_call_counts,
                current_messages_len = current_messages.len(),
                tool_schema_count = tools.len(),
                "LLM loop state"
            );

            if should_throttle_next_tool_round {
                let delay = tool_round_throttle_delay(saw_rate_limit_this_turn);
                info!(
                    iteration = ?tool_call_counts,
                    delay_ms = delay.as_millis() as u64,
                    saw_rate_limit_this_turn,
                    "Throttling next LLM call after tool round"
                );
                tokio::time::sleep(delay).await;
                should_throttle_next_tool_round = false;
            }

            let injected_steering =
                self.drain_steering_messages(&mut current_messages, &mut history, &msg);
            if injected_steering > 0 {
                debug!(
                    injected_steering,
                    current_messages_len = current_messages.len(),
                    "Steering messages injected before LLM call"
                );
            }

            self.dispatch_event(
                &msg,
                &persist_session_key,
                &mut AgentEvent::AgentRequest {
                    message_count: current_messages.len(),
                    tool_count: tools.len(),
                },
            )
            .await;

            // Call LLM with extracted sub-function (#15)
            let llm_timer = ScopedTimer::new();
            let llm_result = self
                .call_llm_with_retry(
                    &current_messages,
                    &tools,
                    &msg,
                    ghost_recall_context_block.as_deref(),
                    &tool_call_counts,
                    &mut saw_rate_limit_this_turn,
                )
                .await;
            metrics.record_llm_call(llm_timer.elapsed_ms());

            let response = match llm_result {
                Ok(r) => r,
                Err(e) => {
                    self.dispatch_event(
                        &msg,
                        &persist_session_key,
                        &mut AgentEvent::AgentRequestError {
                            error: e.to_string(),
                        },
                    )
                    .await;
                    self.end_step(&msg, &persist_session_key, step, 0).await;
                    llm_failed_after_retries = true;
                    let max_retries = self.config.agents.defaults.llm_max_retries;
                    warn!(error = %e, iteration = ?tool_call_counts, retries = max_retries, "LLM call failed after all retries");
                    final_response = llm_exhausted_error(max_retries, &e);
                    if let Some(evo_service) = self.context_builder.evolution_service() {
                        if let Ok(report) = evo_service
                            .report_error("__llm_provider__", &format!("{}", e), None, vec![])
                            .await
                        {
                            if report.evolution_triggered.is_some() {
                                if let Some(ref worker) = self.skill_evolution_worker {
                                    worker.notify();
                                }
                            }
                        }
                    }
                    // Preserve reasoning_content: None here since this is a synthetic error
                    // message, not an LLM response. DeepSeek requires consistent reasoning_content
                    // across assistant messages, but this error fallback has no reasoning to preserve.
                    history.push(ChatMessage::assistant_with_reasoning(&final_response, None));
                    break;
                }
            };

            info!(
                content_len = response.content.as_ref().map(|c| c.len()).unwrap_or(0),
                tool_calls_count = response.tool_calls.len(),
                finish_reason = %response.finish_reason,
                "LLM response received"
            );
            debug!(target: "chat::response", response = serde_json::to_string(&response).unwrap_or_default(), "Response detail");
            self.dispatch_event(
                &msg,
                &persist_session_key,
                &mut AgentEvent::ModelResponse {
                    content: response.content.clone().unwrap_or_default(),
                    tool_calls: serde_json::to_value(&response.tool_calls).unwrap_or_default(),
                    finish_reason: response.finish_reason.clone(),
                },
            )
            .await;

            // Budget usage is reported by providers after an LLM call completes, so this
            // is post-hoc enforcement: the current call may exceed the configured limit,
            // and the runtime stops before the next loop iteration.
            if let Err(e) = self.record_llm_budget(&persist_session_key, &response) {
                error!(error = %e, "Budget exhausted, stopping agent loop");
                self.end_step(&msg, &persist_session_key, step, 0).await;
                final_response = format!("⚠️ {}", e.message);
                history.push(ChatMessage::assistant_with_reasoning(&final_response, None));
                break;
            }

            // Handle tool calls
            if !response.tool_calls.is_empty() {
                let short_circuit_after_tools = is_im_channel(&msg.channel)
                    && response.tool_calls.iter().all(|c| c.name == "message")
                    && response.tool_calls.iter().all(|c| {
                        let ch = c.arguments.get("channel").and_then(|v| v.as_str());
                        let to = c.arguments.get("chat_id").and_then(|v| v.as_str());
                        ch.map(|s| s == msg.channel).unwrap_or(true)
                            && to.map(|s| s == msg.chat_id).unwrap_or(true)
                    });
                let activate_skill_call = response
                    .tool_calls
                    .iter()
                    .find(|call| call.name == ACTIVATE_SKILL_TOOL_NAME)
                    .cloned();

                // Add assistant message with tool calls — use direct struct literal
                // to atomically preserve reasoning_content and tool_calls, avoiding
                // the fragile create-then-mutate pattern that silently loses data
                // if any field assignment is accidentally removed.
                let assistant_content = response.content.as_deref().unwrap_or("");
                let assistant_content = if is_tool_trace_content(assistant_content) {
                    ""
                } else {
                    assistant_content
                };
                let assistant_msg = ChatMessage {
                    id: Some(uuid::Uuid::new_v4().to_string()),
                    role: "assistant".to_string(),
                    content: serde_json::Value::String(assistant_content.to_string()),
                    reasoning_content: response.reasoning_content.clone(),
                    tool_calls: Some(response.tool_calls.clone()),
                    tool_call_id: None,
                    name: None,
                };
                current_messages.push(assistant_msg.clone());
                history.push(assistant_msg);

                if let Some(skill_call) = activate_skill_call {
                    if response.tool_calls.len() > 1 {
                        warn!(
                            tool_calls = response.tool_calls.len(),
                            "activate_skill was returned with additional tool calls; only the skill activation will be executed"
                        );
                    }

                    let raw_skill_name = skill_call
                        .arguments
                        .get("skill_name")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    let goal = skill_call
                        .arguments
                        .get("goal")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or(msg.content.as_str())
                        .to_string();
                    // 技能解析失败同样要关闭当前 step，避免留下只有 step/start 的悬空 step。
                    let resolved_skill: Result<ActiveSkillContext> = (|| {
                        let skill_name =
                            normalize_selected_skill_name(raw_skill_name, &skill_cards)
                                .ok_or_else(|| {
                                    blockcell_core::Error::Skill(format!(
                                        "Model selected unavailable skill '{}'",
                                        raw_skill_name
                                    ))
                                })?;
                        let skill_ctx = self
                            .context_builder
                            .resolve_active_skill_by_name(&skill_name, &disabled_skills)
                            .map(|skill| {
                                suppress_prompt_reinjection_for_continued_skill(
                                    skill,
                                    recent_skill_name.as_deref(),
                                )
                            })
                            .ok_or_else(|| {
                                blockcell_core::Error::Skill(format!(
                                    "Skill '{}' is not available",
                                    skill_name
                                ))
                            })?;
                        Ok(skill_ctx)
                    })();
                    let skill_ctx = match resolved_skill {
                        Ok(resolved) => resolved,
                        Err(error) => {
                            self.end_step(
                                &msg,
                                &persist_session_key,
                                step,
                                response.tool_calls.len(),
                            )
                            .await;
                            return Err(error);
                        }
                    };

                    // Layer 4: Track skill activation for Post-Compact recovery
                    if let Some(memory_system) = self.memory_system.as_mut() {
                        memory_system.record_skill_load(&skill_ctx.name, &skill_ctx.prompt_md);
                        debug!(skill_name = %skill_ctx.name, "[layer4] Tracked skill activation for recovery");
                    }
                    self.dispatch_event(
                        &msg,
                        &persist_session_key,
                        &mut AgentEvent::SkillActivated {
                            name: skill_ctx.name.clone(),
                        },
                    )
                    .await;

                    let skill_history_seed = history[..history.len().saturating_sub(1)].to_vec();
                    let skill_outcome = self
                        .run_skill_for_turn(
                            &skill_ctx,
                            &msg,
                            &skill_history_seed,
                            &persist_session_key,
                        )
                        .await;
                    let (skill_result, updated_metadata, allowed_tools) = match skill_outcome {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            self.end_step(
                                &msg,
                                &persist_session_key,
                                step,
                                response.tool_calls.len(),
                            )
                            .await;
                            return Err(error);
                        }
                    };
                    session_metadata = updated_metadata;
                    record_active_skill_name(&mut session_metadata, &skill_ctx.name);
                    append_activated_skill_history(
                        &mut history,
                        &skill_call.id,
                        &skill_ctx.name,
                        &goal,
                        &allowed_tools,
                        &skill_result.trace_messages,
                        &skill_result.final_response,
                    );
                    final_response = skill_result.final_response;
                    self.end_step(&msg, &persist_session_key, step, response.tool_calls.len())
                        .await;
                    break;
                }

                // Execute each tool call, with dynamic tool supplement for intent misclassification
                let mut supplemented_tools = false;
                let mut tool_results: Vec<ChatMessage> = Vec::new();
                let mut wants_forced_answer = false;
                let mut web_search_thin_results: Vec<String> = Vec::new(); // URLs from thin search results
                for tool_call in &response.tool_calls {
                    if tool_call.name == "web_search" || tool_call.name == "web_fetch" {
                        wants_forced_answer = true;
                    }
                    // Check message tool has media BEFORE execution (for message_tool_sent_media flag only)
                    if tool_call.name == "message" {
                        let has_media = tool_call
                            .arguments
                            .get("media")
                            .and_then(|v| v.as_array())
                            .map(|a| !a.is_empty())
                            .unwrap_or(false);
                        if has_media {
                            message_tool_sent_media = true;
                        }
                    }
                    let tool_timer = ScopedTimer::new();
                    let result = if tool_names.iter().any(|allowed| allowed == &tool_call.name) {
                        let max_iterations = tools_max_iterations
                            .get(&tool_call.name)
                            .copied()
                            .unwrap_or(max_iterations);
                        let count = tool_call_counts.entry(tool_call.name.clone()).or_insert(0);

                        *count += 1;
                        if *count > max_iterations {
                            over_iteration = true;
                            serde_json::json!({
                                "error": format!(
                                    "Tool '{}' execeeded max call limit ({}).",
                                    tool_call.name, max_iterations
                                ),
                                "tool": tool_call.name,
                                "hint": "Reduce repeated tool calls or adjust maxToolIterationsByTool."
                            })
                            .to_string()
                        } else {
                            self.execute_tool_call(tool_call, &msg, None).await
                        }
                    } else {
                        scoped_tool_denied_result(&tool_call.name)
                    };

                    metrics.record_tool_execution(&tool_call.name, tool_timer.elapsed_ms());

                    if tool_call.name == blockcell_tools::mcp::search::MCP_SEARCH_TOOL_NAME {
                        let revealed_tools = extract_mcp_search_revealed_tools(&result);
                        for revealed_tool in revealed_tools {
                            if disabled_tools.contains(&revealed_tool)
                                || !self.tool_registry.is_model_hidden(&revealed_tool)
                            {
                                continue;
                            }
                            let Some(tool) = self.tool_registry.get(&revealed_tool) else {
                                continue;
                            };
                            if !tool_names.iter().any(|name| name == &revealed_tool) {
                                tool_names.push(revealed_tool.clone());
                                tool_names.sort();
                                tool_names.dedup();
                            }
                            if !tools.iter().any(|schema| {
                                schema
                                    .get("function")
                                    .and_then(|function| function.get("name"))
                                    .and_then(|name| name.as_str())
                                    == Some(revealed_tool.as_str())
                            }) {
                                let schema = tool.schema();
                                tools.push(serde_json::json!({
                                    "type": "function",
                                    "function": {
                                        "name": schema.name,
                                        "description": schema.description,
                                        "parameters": schema.parameters
                                    }
                                }));
                                _schema_cache_dirty = true;
                                info!(tool = %revealed_tool, "Revealed MCP tool from progressive discovery search");
                            }
                        }
                    }

                    // Collect media paths from tool results for WebUI display.
                    // Skip the "message" tool — it already dispatches its own OutboundMessage
                    // with media; collecting here would cause a duplicate send.
                    if tool_call.name != "message" {
                        if let Ok(ref rv) = serde_json::from_str::<serde_json::Value>(&result) {
                            let media_exts = [
                                "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "mp3", "wav",
                                "m4a", "mp4", "webm", "mov",
                            ];
                            // Scalar fields: output_path, path, file_path, etc.
                            for key in &[
                                "output_path",
                                "path",
                                "file_path",
                                "screenshot_path",
                                "image_path",
                            ] {
                                if let Some(p) = rv.get(key).and_then(|v| v.as_str()) {
                                    let ext = p.rsplit('.').next().unwrap_or("").to_lowercase();
                                    if media_exts.contains(&ext.as_str()) {
                                        collected_media.push(p.to_string());
                                    }
                                }
                            }
                            // Array field: "media"
                            if let Some(arr) = rv.get("media").and_then(|v| v.as_array()) {
                                for mv in arr {
                                    if let Some(p) = mv.as_str() {
                                        let ext = p.rsplit('.').next().unwrap_or("").to_lowercase();
                                        if media_exts.contains(&ext.as_str()) {
                                            collected_media.push(p.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Detect thin web_search results (only titles/URLs, no actual content).
                    // When this happens, extract the top URLs so the next hint can suggest web_fetch.
                    if tool_call.name == "web_search"
                        && !result.starts_with("Tool error:")
                        && is_thin_search_result(&result)
                    {
                        let urls = extract_urls_from_search_result(&result);
                        if !urls.is_empty() {
                            web_search_thin_results.extend(urls);
                        }
                    }

                    // Dynamic tool supplement: if tool was not found or validation failed
                    // (e.g. lightweight schema had no params), inject full schema and retry.
                    let needs_supplement = should_supplement_tool_schema(&result);
                    if needs_supplement {
                        if let Some(schema) = self.tool_registry.get(&tool_call.name) {
                            // Check if we need to upgrade from lightweight to full schema
                            let already_full = tools.iter().any(|t| {
                                t.get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|n| n.as_str())
                                    == Some(&tool_call.name)
                                    && t.get("function")
                                        .and_then(|f| f.get("parameters"))
                                        .and_then(|p| p.get("properties"))
                                        .map(|props| {
                                            props.as_object().is_some_and(|o| !o.is_empty())
                                        })
                                        .unwrap_or(false)
                            });
                            if !already_full {
                                let schema_val = serde_json::json!({
                                    "type": "function",
                                    "function": {
                                        "name": schema.schema().name,
                                        "description": schema.schema().description,
                                        "parameters": schema.schema().parameters
                                    }
                                });
                                // Replace lightweight schema with full schema
                                tools.retain(|t| {
                                    t.get("function")
                                        .and_then(|f| f.get("name"))
                                        .and_then(|n| n.as_str())
                                        != Some(&tool_call.name)
                                });
                                tools.push(schema_val);
                                supplemented_tools = true;
                                _schema_cache_dirty = true;
                                info!(tool = %tool_call.name, "Dynamically supplemented tool with full schema");
                                break;
                            }
                        }
                    }

                    // Track tool failures with transient/permanent classification (#6)
                    let is_error = tool_result_indicates_error(&result);
                    if consecutive_failure_tracker.record(tool_call, is_error) {
                        over_iteration = true;
                        warn!(
                            tool = %tool_call.name,
                            arguments = %tool_call.arguments,
                            "Stopping agent loop after three identical tool-call failures"
                        );
                    }
                    if is_error {
                        let failure_kind = classify_tool_failure(&result);
                        match failure_kind {
                            ToolFailureKind::SkillContextMissing => {
                                // Skill context missing — give friendly hint to activate skill first
                                let hint = format!(
                                    "💡 工具 `{}` 需要先激活技能才能使用。\n\
                                     请先调用 `activate_skill` 工具激活技能，例如：\n\
                                     ```\n\
                                     activate_skill({{skill_name: \"<技能名>\", goal: \"<目标>\"}})\n\
                                     ```\n\
                                     激活后再调用 `{}` 执行技能脚本。",
                                    tool_call.name, tool_call.name
                                );
                                info!(tool = %tool_call.name, "Skill context missing — suggesting activate_skill");
                                current_messages.push(ChatMessage::user(&hint));
                            }
                            ToolFailureKind::Permanent | ToolFailureKind::Transient => {
                                let count =
                                    tool_fail_counts.entry(tool_call.name.clone()).or_insert(0);
                                *count += 1;

                                if failure_kind == ToolFailureKind::Permanent && *count == 1 {
                                    let hint = format!(
                                        "⚠️ 工具 `{}` 遇到永久性错误（如 API key 缺失、权限不足），请不要重试，改用其他可用工具或告知用户配置问题。",
                                        tool_call.name
                                    );
                                    warn!(tool = %tool_call.name, kind = ?failure_kind, "Permanent tool failure — injecting immediate hint");
                                    current_messages.push(ChatMessage::user(&hint));
                                }
                            }
                            ToolFailureKind::ResourceMissing => {
                                tool_fail_counts.remove(&tool_call.name);
                                if resource_missing_hints_sent.insert(tool_call.name.clone()) {
                                    let hint = format!(
                                        "⚠️ 工具 `{}` 报告目标资源不存在。不要重复调用同一工具重试同一个标识；直接向用户说明未找到，或请用户提供新的标识/范围。",
                                        tool_call.name
                                    );
                                    current_messages.push(ChatMessage::user(&hint));
                                }
                            }
                        }
                    } else {
                        // Reset on success
                        tool_fail_counts.remove(&tool_call.name);
                        resource_missing_hints_sent.remove(&tool_call.name);
                    }

                    let mut tool_msg = ChatMessage::tool_result(&tool_call.id, &result);
                    tool_msg.name = Some(tool_call.name.clone());
                    tool_results.push(tool_msg);
                }

                // If we supplemented tools, roll back the assistant message and tool results
                // so the LLM retries with the full tool schema available.
                if supplemented_tools {
                    // Remove the assistant message we just pushed (last element)
                    current_messages.pop();
                    history.pop();
                    // Do NOT push tool results — the LLM will retry from scratch
                    self.end_step(&msg, &persist_session_key, step, response.tool_calls.len())
                        .await;
                    continue;
                }

                // Normal path: commit tool results to messages and history,
                // trimming each tool result to prevent unbounded growth.
                for mut tool_msg in tool_results {
                    // Trim tool result content (tool results can be very large,
                    // e.g. web_fetch markdown, finance_api JSON arrays)
                    if let serde_json::Value::String(ref s) = tool_msg.content {
                        let char_count = s.chars().count();
                        // 使用 Layer1 配置的 max_result_size_chars 而非硬编码值，
                        // 确保用户配置的阈值（默认 50k）实际生效
                        let max_size = self.response_cache.max_result_size_chars();
                        if char_count > max_size {
                            // Layer 1: Attempt to persist large tool output to disk before truncation.
                            // This preserves the full output for later recovery by the memory system.
                            // 每次调用生成唯一 UUID，配合 session_key 避免 text_call_0/ollama_call_0
                            // 等通用 ID 跨轮次重复导致覆盖。
                            let call_uuid = uuid::Uuid::new_v4().simple().to_string();
                            // 使用 persist_session_key 而非原始 session_key，
                            // 确保 cron 投递场景下工具结果持久化目录与最终历史目录一致，
                            // 否则 session_recall 按目标 session 查找时找不到文件
                            let persisted_stub = self
                                .try_persist_large_tool_result(
                                    s,
                                    tool_msg.tool_call_id.as_deref(),
                                    &persist_session_key,
                                    &call_uuid,
                                )
                                .await;

                            if let Some(stub) = persisted_stub {
                                tool_msg.content = serde_json::Value::String(stub);
                            } else {
                                // Fallback: inline truncation when persistence is unavailable
                                // 使用配置的预览大小（以字符为单位），按 2/3 头部 + 1/3 尾部分配
                                // 注意：preview_size 配置名为 bytes 但实际按字符数使用，
                                // 使用 saturating_sub 防止 char_count < preview_size 时下溢
                                let preview_size = self.response_cache.preview_size_chars();
                                let head_size = preview_size * 2 / 3;
                                let tail_size = preview_size - head_size;
                                let head: String = s.chars().take(head_size).collect();
                                let tail: String = s
                                    .chars()
                                    .rev()
                                    .take(tail_size)
                                    .collect::<String>()
                                    .chars()
                                    .rev()
                                    .collect();
                                let trimmed_chars = char_count.saturating_sub(preview_size);
                                tool_msg.content = serde_json::Value::String(format!(
                                    "{}\n...<trimmed {} chars>...\n{}",
                                    head, trimmed_chars, tail
                                ));
                            }
                        }
                    }
                    current_messages.push(tool_msg.clone());
                    history.push(tool_msg);
                }

                checkpoint_turn = checkpoint_turn.saturating_add(1);
                if let Some(task_id) = runtime_task_id.as_deref() {
                    let checkpoint = crate::checkpoint::TaskCheckpoint::new_for_origin(
                        task_id,
                        history.clone(),
                        checkpoint_turn,
                        &msg.channel,
                        &msg.chat_id,
                        &persist_session_key,
                    );
                    if let Err(e) =
                        crate::checkpoint::CheckpointManager::new(&self.paths.workspace())
                            .save(&checkpoint)
                    {
                        warn!(task_id = %task_id, error = %e, "Failed to save task checkpoint after tool round");
                    }
                }
                self.dispatch_event(
                    &msg,
                    &persist_session_key,
                    &mut AgentEvent::StepEnd {
                        iteration: step,
                        tool_calls: response.tool_calls.len(),
                    },
                )
                .await;

                if wants_forced_answer && !over_iteration {
                    if !web_search_thin_results.is_empty() {
                        // Thin results: guide LLM to fetch actual page content instead of giving up
                        let urls_hint = web_search_thin_results
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n- ");
                        let hint = format!(
                            "搜索结果只包含链接标题，没有具体内容。**不要直接返回\"未找到\"，请立即改用 `web_fetch` 直接抓取以下页面获取真实数据**：\n- {}\n\n抓取后给出最终答案。",
                            urls_hint
                        );
                        current_messages.push(ChatMessage::user(&hint));
                    } else {
                        current_messages.push(ChatMessage::user(
                            "请基于刚才工具返回的结果直接给出最终答案（例如：整理成要点/列表/摘要）。除非结果明显不足，否则不要继续调用 web_search/web_fetch。",
                        ));
                    }
                }

                // Fallback hint: when a tool has failed 2+ times, tell the LLM to switch
                // to alternative tools. This prevents infinite retry loops (e.g. qveris without API key).
                let repeated_failures: Vec<String> = tool_fail_counts
                    .iter()
                    .filter(|(_, count)| **count >= 2)
                    .map(|(name, count)| format!("{} ({}x)", name, count))
                    .collect();
                if !repeated_failures.is_empty() {
                    let hint = format!(
                        "⚠️ 以下工具连续失败: {}。请不要继续重试，改用其他可用工具完成任务。对于金融数据查询失败，可降级使用 `web_search` 搜索相关新闻。",
                        repeated_failures.join(", ")
                    );
                    warn!(failures = ?repeated_failures, "Injecting fallback hint due to repeated tool failures");
                    current_messages.push(ChatMessage::user(&hint));
                }

                // Layer 4: Full Compact - 当 token 超过预算阈值时触发 LLM 语义压缩
                // 预算阈值: token_budget * compact_threshold (默认 100_000 * 0.8 = 80_000)
                let estimated_tokens = estimate_messages_tokens(&current_messages);
                // Update Layer 4 token usage metrics
                let trigger_compact = if let Some(memory_system) = self.memory_system.as_ref() {
                    self.record_context_window_estimate(
                        &persist_session_key,
                        estimated_tokens,
                        memory_system.config().token_budget,
                        memory_system.config().layer4.compact_threshold_ratio,
                    );
                    crate::memory_event!(
                        layer4,
                        token_usage,
                        estimated_tokens,
                        memory_system.config().token_budget,
                        memory_system.config().layer4.compact_threshold_ratio
                    );
                    if memory_system.should_compact(estimated_tokens) {
                        info!(
                            estimated_tokens,
                            token_budget = memory_system.config().token_budget,
                            threshold = memory_system.config().layer4.compact_threshold_ratio,
                            "[layer4] Full compact threshold reached"
                        );
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if trigger_compact
                    && self
                        .apply_layer4_compact_in_loop(
                            &mut current_messages,
                            &mut history,
                            &msg,
                            &persist_session_key,
                            "mid-loop",
                        )
                        .await
                {
                    metrics.record_compression();
                    // 跳过后续处理
                    continue;
                }

                // Skill Nudge: check after each iteration (replaces skill_nudge_engine.check_skill_nudge)
                // If memory nudge already triggered, upgrade to Combined
                let has_skill_tool = self.tool_registry.get("skill_manage").is_some();
                let existing_memory = matches!(deferred_review_mode, Some(ReviewMode::Memory));
                if let Some(_skill_trigger) = self
                    .learning_coordinator
                    .check_skill_nudge(has_skill_tool, existing_memory)
                {
                    if matches!(deferred_review_mode, Some(ReviewMode::Memory)) {
                        deferred_review_mode = Some(ReviewMode::Combined);
                        // Use latest messages snapshot (updated during iteration)
                        deferred_review_snapshot = current_messages.clone();
                    } else if deferred_review_mode.is_none() {
                        deferred_review_mode = Some(ReviewMode::Skill);
                        deferred_review_snapshot = current_messages.clone();
                        deferred_review_reservation = Some(LearningReviewReservationGuard::new(
                            Arc::clone(&self.learning_coordinator),
                        ));
                    }
                }

                if !over_iteration && !short_circuit_after_tools {
                    should_throttle_next_tool_round = true;
                }

                if short_circuit_after_tools {
                    final_response.clear();
                    break;
                }

                if over_iteration {
                    warn!(
                        iteration = ?tool_call_counts,
                        max_iterations,
                        ?tools_max_iterations,
                        "Reached max iterations; forcing a final no-tools answer"
                    );
                    let mut final_messages = current_messages.clone();
                    final_messages.push(ChatMessage::user(
                        "请基于以上工具调用的结果，直接给出最终答案。不要再调用任何工具，也不要输出类似[Called: ...]的过程信息。",
                    ));
                    let final_messages = append_ephemeral_context_to_latest_user_message(
                        &final_messages,
                        ghost_recall_context_block.as_deref(),
                    );

                    let chat_result = self
                        .run_final_model_step(&msg, &persist_session_key, &final_messages)
                        .await;
                    match chat_result {
                        Ok(r) => {
                            final_response = r.content.unwrap_or_default();
                            // 保留 reasoning_content，避免 DeepSeek thinking mode 400 错误
                            history.push(ChatMessage::assistant_with_reasoning(
                                &final_response,
                                r.reasoning_content.clone(),
                            ));
                        }
                        Err(e) => {
                            warn!(error = %e, "Final no-tools LLM call failed");
                            final_response =
                                "I've reached the maximum number of tool iterations.".to_string();
                            // Synthetic error message, no reasoning_content to preserve
                            history
                                .push(ChatMessage::assistant_with_reasoning(&final_response, None));
                        }
                    }
                    break;
                }
            } else {
                self.dispatch_event(
                    &msg,
                    &persist_session_key,
                    &mut AgentEvent::StepEnd {
                        iteration: step,
                        tool_calls: 0,
                    },
                )
                .await;
                // No tool calls, we have the final response
                final_response = response.content.unwrap_or_default();

                // 保留 reasoning_content，避免 DeepSeek thinking mode 400 错误
                history.push(ChatMessage::assistant_with_reasoning(
                    &final_response,
                    response.reasoning_content.clone(),
                ));
                break;
            }
        }

        self.current_step = None;

        // ── 等待运行中的子agent任务并汇总结果 ──
        // 主LLM循环结束后，检查是否还有类型化子agent任务在运行。
        // 如果有，等待其完成，然后做一次额外的LLM调用来生成汇总。
        {
            let running_tasks = self
                .task_manager
                .list_tasks_for_origin(&msg.channel, &msg.chat_id, Some(&TaskStatus::Running))
                .await;
            let typed_running: Vec<_> = running_tasks
                .iter()
                .filter(|t| t.agent_type.is_some())
                .collect();

            if !typed_running.is_empty() {
                info!(
                    running_count = typed_running.len(),
                    "Waiting for sub-agent tasks to complete before summarizing"
                );

                // 等待所有运行中的类型化任务完成（带超时）
                let max_wait_secs = 300; // 最大等待5分钟
                let deadline =
                    tokio::time::Instant::now() + tokio::time::Duration::from_secs(max_wait_secs);

                for task in &typed_running {
                    let task_id = &task.id;
                    let agent_type = task.agent_type.as_deref().unwrap_or("unknown");
                    info!(task_id = %task_id, agent_type = agent_type, "Waiting for sub-agent task");

                    // 轮询直到任务完成或超时
                    loop {
                        if tokio::time::Instant::now() >= deadline {
                            warn!(task_id = %task_id, "Timeout waiting for sub-agent task");
                            break;
                        }
                        if let Some(task) = self.task_manager.get_task(task_id).await {
                            match task.status {
                                TaskStatus::Completed
                                | TaskStatus::Failed
                                | TaskStatus::Cancelled => {
                                    info!(task_id = %task_id, status = ?task.status, "Sub-agent task finished");
                                    break;
                                }
                                TaskStatus::Running | TaskStatus::Queued => {
                                    tokio::time::sleep(tokio::time::Duration::from_millis(500))
                                        .await;
                                }
                            }
                        } else {
                            warn!(task_id = %task_id, "Task disappeared from TaskManager");
                            break;
                        }
                    }
                }

                // 收集已完成的结果，做一次汇总LLM调用
                let completed_tasks = self
                    .task_manager
                    .list_tasks_for_origin(&msg.channel, &msg.chat_id, Some(&TaskStatus::Completed))
                    .await;
                let uninject_completed: Vec<_> = completed_tasks
                    .iter()
                    .filter(|t| t.agent_type.is_some() && !t.result_injected && t.result.is_some())
                    .collect();

                if !uninject_completed.is_empty() {
                    info!(
                        completed_count = uninject_completed.len(),
                        "Making summary LLM call with sub-agent results"
                    );

                    // 将已完成的结果注入到 current_messages 中
                    let mut summary_section = String::from("\n\n## Completed Agent Results\nThe following background agent tasks have completed. Use their results to answer the user's question:\n\n");
                    for t in &uninject_completed {
                        let short_id = {
                            let meaningful = if let Some(rest) = t.id.strip_prefix("task-") {
                                rest
                            } else {
                                &t.id
                            };
                            meaningful.chars().take(8).collect::<String>()
                        };
                        let agent_type = t.agent_type.as_deref().unwrap_or("unknown");
                        let label = if t.label.is_empty() {
                            agent_type
                        } else {
                            &t.label
                        };
                        summary_section.push_str(&format!(
                            "### `[{}]` **{}** agent: {}\n\n",
                            short_id, agent_type, label
                        ));
                        if let Some(ref result) = t.result {
                            let display = if result.chars().count() > 3000 {
                                let truncated: String = result.chars().take(3000).collect();
                                format!(
                                    "{}...\n\n(Result truncated. Use `/tasks {}` to see full result)",
                                    truncated, short_id
                                )
                            } else {
                                result.clone()
                            };
                            summary_section.push_str(&display);
                            summary_section.push('\n');
                        }
                        summary_section.push('\n');
                    }
                    summary_section.push_str("- You should integrate and summarize these results for the user.\n- If the user asks for details, reference the specific task_id.\n");

                    // 将汇总作为合成用户消息追加（不追加到tool消息，避免LLM混淆）
                    let mut summary_injected = false;
                    if let Some(last_msg) = current_messages.last_mut() {
                        if last_msg.role == "user" {
                            if let Some(text) = last_msg.content.as_str() {
                                last_msg.content = serde_json::Value::String(format!(
                                    "{}{}",
                                    text, summary_section
                                ));
                                summary_injected = true;
                            }
                        }
                    }
                    if !summary_injected {
                        // 最后一条消息不是user或content非字符串，添加合成用户消息
                        current_messages.push(ChatMessage::user(&format!(
                            "All sub-agent tasks have completed. Please summarize and integrate their results for the user.{}",
                            summary_section
                        )));
                    }

                    // 做一次额外的LLM调用来生成汇总
                    let summary_result = self
                        .run_final_model_step(&msg, &persist_session_key, &current_messages)
                        .await;
                    self.current_step = None;

                    match summary_result {
                        Ok(r) => {
                            let summary_content = r.content.unwrap_or_default();
                            info!(
                                summary_len = summary_content.len(),
                                "Summary LLM call completed"
                            );
                            final_response = summary_content;
                            // Preserve reasoning_content to avoid DeepSeek 400 errors
                            history.push(ChatMessage::assistant_with_reasoning(
                                &final_response,
                                r.reasoning_content.clone(),
                            ));

                            // 汇总成功，标记结果已注入
                            for t in &uninject_completed {
                                self.task_manager.mark_result_injected(&t.id).await;
                            }

                            // 通过 event_tx 发送汇总结果，确保CLI/ws渠道能看到
                            // （persist_and_deliver_final_response 的 outbound 设置了 skip_ws_echo=true，
                            //  流式token已打印的原始响应会被跳过，但汇总内容是新的需要单独发送）
                            if let Some(ref event_tx) = self.event_tx {
                                let event = serde_json::json!({
                                    "type": "message_done",
                                    "channel": msg.channel,
                                    "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                                    "chat_id": msg.chat_id,
                                    "task_id": "",
                                    "content": final_response,
                                    "tool_calls": 0,
                                    "duration_ms": 0,
                                    "media": [],
                                    "is_markdown": true,
                                    "summary_for_subagents": true,
                                });
                                let _ = event_tx.send(event.to_string());
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "Summary LLM call failed, results not marked as injected");
                            // 不标记 result_injected，下轮 inject_running_tasks_into_system_prompt 会重新注入
                        }
                    }
                }
            }
        }

        // ── 延迟后台 Review (与 Hermes 一致: 在响应发送后触发) ──
        // 与 Hermes 一致: 只在有完整响应时才触发后台审查
        // Hermes: `if final_response and not interrupted`
        if !final_response.is_empty() {
            if let Some(mode) = deferred_review_mode.take() {
                if self.learning_coordinator.is_self_improve_enabled() {
                    // Counter already incremented by try_start_review() in
                    // check_memory_nudge()/check_skill_nudge() — do NOT call
                    // review_started() here to avoid double increment.
                    let notify = Some((msg.channel.clone(), msg.chat_id.clone()));
                    if let Some(reservation) = deferred_review_reservation.take() {
                        self.spawn_review(mode, deferred_review_snapshot, notify, reservation);
                    }
                }
            }
        }

        if is_im_channel(&msg.channel)
            && user_wants_send_image(&msg.content)
            && !message_tool_sent_media
        {
            if let Some(image_path) = pick_image_path(&self.paths, &history).await {
                info!(
                    image_path = %image_path,
                    channel = %msg.channel,
                    "Auto-sending image fallback (LLM did not call message tool)"
                );
                if let Some(tx) = &self.outbound_tx {
                    let mut outbound = OutboundMessage::new(&msg.channel, &msg.chat_id, "");
                    outbound.account_id = msg.account_id.clone();
                    outbound.media = vec![image_path.clone()];
                    let _ = tx.send(outbound).await;
                }

                final_response.clear();
                overwrite_last_assistant_message(&mut history, "");
            }
        }

        let _ghost_learning_episode_id = match self.capture_turn_end_learning_boundary(
            &msg,
            &history,
            &final_response,
            &tool_call_counts,
            !llm_failed_after_retries,
        ) {
            Ok(episode_id) => episode_id,
            Err(e) => {
                warn!(error = %e, session_key = %session_key, "Ghost learning turn-end capture failed");
                None
            }
        };

        // Post-Sampling Hooks: Layer 3 & Layer 5
        // 在主循环结束后执行 Session Memory 和 Auto Memory 提取
        // 使用 tokio::spawn 非阻塞执行，不延迟用户响应
        // 预先获取共享引用（避免借用冲突）
        let reload_flag = self.memory_injector_reload_flag();
        let cursor_reload_flag = self
            .memory_system
            .as_ref()
            .map(|ms| ms.cursor_reload_flag());

        if let Some(memory_system) = self.memory_system.as_mut() {
            let current_tokens = estimate_messages_tokens(&history);
            let mut actions = crate::memory_system::evaluate_memory_hooks(
                memory_system,
                &history,
                current_tokens,
            )
            .await;

            // Post-Sampling 动作处理：Session Memory → Auto Memory → Compact
            // 使用 PostSamplingActions struct 支持同一轮返回多个动作，
            // 避免 Session/Auto/Compact 同时到期时互相跳过。
            // 执行顺序：先 spawn 提取任务（clone 历史），再同步执行 Compact。

            // 1. Session Memory 提取
            if actions.session_memory {
                info!("[post-sampling] Spawning Session Memory extraction task");

                // 先确保 session memory 文件存在于磁盘上，
                // 必须在 mark_extraction_started() 之前完成，
                // 否则模板写入的 mtime 会晚于 extraction_start_system，
                // 导致 compact 等待时误判提取已完成
                let memory_path = crate::session_memory::get_session_memory_path(
                    memory_system.workspace_dir(),
                    memory_system.session_id(),
                );
                let template = crate::session_memory::DEFAULT_SESSION_MEMORY_TEMPLATE;
                match crate::session_memory::setup_session_memory_file(&memory_path, template).await
                {
                    Ok(_) => {
                        info!(
                            path = %memory_path.display(),
                            "[layer3] Session memory file ensured on disk"
                        );
                    }
                    Err(e) => {
                        warn!(
                            path = %memory_path.display(),
                            error = %e,
                            "[layer3] Failed to initialize session memory file"
                        );
                    }
                }

                // 标记提取开始（设置 extraction_started_at + has_pending_extraction）
                // 在模板写入之后，这样 mtime 基准不会被模板写入干扰
                // 原子创建 pending marker：如果另一个 runtime 已在提取，跳过 spawn
                if !memory_system.mark_extraction_started() {
                    info!("[layer3] 另一个 runtime 已在提取 Session Memory，跳过");
                } else {
                    // 落盘 journal：记录任务元数据，用于 stale marker 检测
                    // 后台任务完成时清除 journal；不是可靠恢复队列
                    memory_system.write_session_memory_journal(history.len());

                    // 克隆必要的数据用于异步任务
                    let provider_pool = Arc::clone(&self.provider_pool);
                    let history_clone = history.clone();
                    let model = self.config.agents.defaults.model.clone();
                    let max_section_length = memory_system.config().layer3.max_section_length;
                    // 获取提取结果发送端（用于后台任务完成后通知主线程更新状态）
                    let result_sender = memory_system.session_memory_result_sender();

                    // 获取已提取历史的最后一条消息的 ID 和索引（用于更新状态游标）
                    // 使用最后一条消息（而非最后一条 user），避免 count_tool_calls_since
                    // 重复统计已被本次提取覆盖的 assistant tool calls
                    let last_msg_id = history_clone.last().and_then(|m| m.id.clone());
                    let last_msg_index = history_clone.len().saturating_sub(1);
                    let current_token_count =
                        crate::token::estimate_messages_tokens(&history_clone);

                    // 获取状态文件和标记文件路径（用于后台任务直接落盘）
                    // 在短生命周期 runtime 下（如 gateway 模式），runtime 可能
                    // 在后台提取完成前被 drop，导致 watch channel 结果无法应用。
                    // 后台任务直接写状态文件可确保下次 runtime 能恢复提取进度。
                    let session_state_path = memory_path
                        .parent()
                        .map(|p| p.join(".session_memory_state.json"))
                        .unwrap_or_else(|| memory_path.with_extension("session_memory_state.json"));
                    let session_marker_path = memory_path
                        .parent()
                        .map(|p| p.join(".extraction_pending"))
                        .unwrap_or_else(|| memory_path.with_extension("extraction_pending"));
                    // journal 路径：后台任务完成时清除
                    let session_journal_path = memory_path
                        .parent()
                        .map(|p| p.join(".extraction_journal"))
                        .unwrap_or_else(|| memory_path.with_extension("extraction_journal"));

                    // 非阻塞执行（不追踪 handle，避免 runtime drop 时被 abort）
                    // 可靠性说明：journal 仅用于 stale marker 检测，不是可靠恢复队列。
                    // 进程退出时正在运行的任务会丢失，只能通过 stale 机制清理。
                    // 原子 pending marker 防止并发重复提取。
                    let _handle = tokio::spawn(async move {
                        let _marker_guard = crate::memory_system::ExtractionMarkerGuard::new(
                            session_marker_path,
                            session_journal_path,
                        );
                        let system_prompt = Arc::new(
                            "你是一个会话记忆提取助手。请从对话中提取关键信息并更新 Session Memory 文件。"
                                .to_string(),
                        );

                        let current_memory = tokio::fs::read_to_string(&memory_path)
                            .await
                            .unwrap_or_else(|_| template.to_string());

                        let result = crate::session_memory::extract_session_memory(
                            provider_pool,
                            &system_prompt,
                            &model,
                            history_clone,
                            &memory_path,
                            &current_memory,
                            template,
                            max_section_length,
                        )
                        .await;

                        // 通过 watch channel 发送提取结果给主线程
                        let extraction_result = match result {
                            Ok(_) => {
                                info!("[layer3] Session Memory extraction completed");
                                crate::memory_system::SessionMemoryExtractionResult {
                                    last_message_id: last_msg_id.clone(),
                                    last_message_index: last_msg_index,
                                    token_count: current_token_count,
                                    success: true,
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "[layer3] Session Memory extraction failed");
                                crate::memory_system::SessionMemoryExtractionResult {
                                    success: false,
                                    ..Default::default()
                                }
                            }
                        };
                        let _ = result_sender.send(extraction_result.clone());

                        // 直接持久化状态到文件，确保短生命周期 runtime 下状态不丢失
                        // 即使当前 runtime 已被 drop，下次 runtime 也能从文件恢复提取进度
                        if extraction_result.success {
                            let json = serde_json::json!({
                                "tokens_at_last_extraction": current_token_count,
                                "initialized": true,
                                "last_memory_message_id": last_msg_id,
                                "last_memory_message_index": last_msg_index,
                            });
                            if let Ok(content) = serde_json::to_string_pretty(&json) {
                                if let Some(parent) = session_state_path.parent() {
                                    let _ = std::fs::create_dir_all(parent);
                                }
                                // 使用原子写入，防止崩溃或并发读取时得到半截 JSON
                                if let Err(e) = crate::fs_util::atomic_write(
                                    &session_state_path,
                                    content.as_bytes(),
                                ) {
                                    tracing::warn!(
                                        error = %e,
                                        path = %session_state_path.display(),
                                        "[layer3] 直接持久化状态文件失败"
                                    );
                                }
                            }
                        }
                    });

                    // 注意：不将提取句柄加入 background_tasks，避免 runtime drop 时 abort
                    // 导致状态永远无法落盘。后台任务自行写状态文件 + 清标记，对 runtime
                    // 生命周期无依赖。tokio::spawn 的任务会持续运行直到完成。
                } // else: mark_extraction_started 成功
            }

            // 2. Auto Memory 提取
            if !actions.auto_memory_types.is_empty() {
                info!(
                    memory_types = ?actions.auto_memory_types,
                    "[post-sampling] Spawning Auto Memory extraction tasks"
                );

                // 克隆必要的数据
                let provider_pool = Arc::clone(&self.provider_pool);
                let config_dir = memory_system.config_dir().to_path_buf();
                let model = self.config.agents.defaults.model.clone();
                let layer5_config = memory_system.config().layer5.clone();
                // 使用预先获取的 cursor_reload_flag
                let cursor_reload_flag = cursor_reload_flag
                    .clone()
                    .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicBool::new(false)));

                // 为每种记忆类型创建独立的异步任务
                for memory_type in actions.auto_memory_types.drain(..) {
                    let provider_pool_for_type = Arc::clone(&provider_pool);
                    let history_for_type = history.clone();
                    let config_dir_for_type = config_dir.clone();
                    let model_for_type = model.clone();
                    let layer5_config_for_type = layer5_config.clone();
                    let reload_flag_for_type = Arc::clone(&reload_flag);
                    let cursor_reload_flag_for_type = Arc::clone(&cursor_reload_flag);

                    // 获取最后一条用户消息的 UUID（用于游标更新）
                    let last_user_uuid = history_for_type
                        .iter()
                        .rev()
                        .find(|m| m.role == "user")
                        .and_then(|m| m.id.clone())
                        .and_then(|s| uuid::Uuid::parse_str(&s).ok())
                        .unwrap_or_else(uuid::Uuid::new_v4);

                    let message_count = history_for_type.len();

                    // per-memory-type pending marker 路径
                    let auto_marker_path = config_dir_for_type
                        .join(format!(".extraction_pending.{}", memory_type.name()));
                    // per-memory-type journal 路径
                    let auto_journal_path = config_dir_for_type
                        .join(format!(".extraction_journal.{}", memory_type.name()));

                    // 在 spawn 前同步、原子创建 pending marker，防止快连续消息重复 spawn
                    // create_new(true) 保证只有一个调用者能成功创建文件
                    // 已存在的 marker 说明该 memory type 的提取正在运行，应跳过
                    if let Some(parent) = auto_marker_path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let marker_created = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&auto_marker_path)
                        .is_ok();
                    if !marker_created {
                        tracing::debug!(
                            memory_type = memory_type.name(),
                            "[layer5] 跳过已标记 pending 的 memory type 提取"
                        );
                        continue;
                    }

                    // 落盘 journal：记录任务元数据，用于 stale marker 检测
                    // 包含 owner_pid 用于检测任务所属进程是否存活，避免误删长耗时任务
                    {
                        let journal = serde_json::json!({
                            "task_type": "auto_memory",
                            "memory_type": memory_type.name(),
                            "message_count": message_count,
                            "started_at": chrono::Utc::now().to_rfc3339(),
                            "owner_pid": std::process::id(),
                        });
                        if let Ok(content) = serde_json::to_string_pretty(&journal) {
                            if let Err(e) =
                                crate::fs_util::atomic_write(&auto_journal_path, content.as_bytes())
                            {
                                tracing::debug!(
                                    error = %e,
                                    memory_type = memory_type.name(),
                                    "[layer5] 写入 Auto Memory journal 失败"
                                );
                            }
                        }
                    }

                    let _handle = tokio::spawn(async move {
                        // marker 已在 spawn 前原子创建；守卫确保错误和 panic 路径也会清理。
                        let mut marker_guard = crate::memory_system::ExtractionMarkerGuard::new(
                            auto_marker_path,
                            auto_journal_path,
                        );

                        // 创建提取器（会加载持久化的游标状态）
                        let extractor_config =
                            crate::auto_memory::AutoMemoryConfig::from(layer5_config_for_type);
                        let mut extractor =
                            match crate::auto_memory::AutoMemoryExtractor::with_config(
                                &config_dir_for_type,
                                extractor_config,
                            )
                            .await
                            {
                                Ok(e) => e,
                                Err(e) => {
                                    warn!(error = %e, "[layer5] Failed to create AutoMemoryExtractor");
                                    return;
                                }
                            };

                        let system_prompt = Arc::new(
                                "你是一个记忆提取助手。请从对话中提取用户偏好、项目信息、反馈和外部资源引用。"
                                    .to_string(),
                            );

                        // 使用 ExtractionParams 和 extract() 方法
                        // 这样游标状态会被正确更新和保存
                        let params = crate::auto_memory::ExtractionParams {
                            provider_pool: provider_pool_for_type,
                            memory_type,
                            system_prompt,
                            model: model_for_type,
                            messages: history_for_type,
                            last_message_uuid: last_user_uuid,
                            message_count,
                        };

                        let result = extractor.extract(params).await;

                        // 游标保存失败时保留 marker/journal，避免游标未推进导致重复写入；
                        // 其余路径由守卫统一清理，包括 panic unwind。
                        if result.success && result.cursor_save_failed {
                            marker_guard.preserve();
                            warn!(
                                memory_type = result.memory_type.name(),
                                "[layer5] 游标保存失败，保留 pending marker 以便重试"
                            );
                        }

                        if result.success {
                            if result.cursor_save_failed {
                                // 游标保存失败：记忆文件已更新但游标未推进
                                // 不设置 reload flag，避免刷新不完整的游标状态
                                // pending marker 已保留，下次交互会重试提取
                                warn!(
                                        memory_type = memory_type.name(),
                                        "[layer5] Auto Memory 提取完成但游标保存失败，不刷新缓存，等待重试"
                                    );
                            } else {
                                info!(
                                    memory_type = memory_type.name(),
                                    input_tokens = result.input_tokens,
                                    output_tokens = result.output_tokens,
                                    "[layer5] Auto Memory extraction completed"
                                );
                                // 标记需要刷新缓存
                                reload_flag_for_type
                                    .store(true, std::sync::atomic::Ordering::Release);
                                // 标记需要重新加载游标状态（通知主线程）
                                cursor_reload_flag_for_type
                                    .store(true, std::sync::atomic::Ordering::Release);
                            }
                        } else {
                            warn!(
                                memory_type = memory_type.name(),
                                error = ?result.error,
                                "[layer5] Auto Memory extraction failed"
                            );
                        }
                    });

                    // 注意：不将 Auto Memory 提取句柄加入 background_tasks，
                    // 避免短生命周期 runtime drop 时 abort 导致游标状态无法保存。
                    // AutoMemoryExtractor::extract() 内部已自行持久化游标状态。
                }
            }

            // 3. Compact（在 spawn 之后执行，确保提取任务已拿到完整历史快照）
            if actions.compact {
                // Post-Sampling 中的 Compact - 同步执行压缩
                // Compact 应在当前交互结束前同步执行
                // 这样下次交互时历史已经是压缩后的状态，用户无感知
                info!(
                    current_tokens,
                    token_budget = memory_system.config().token_budget,
                    "[post-sampling] Executing synchronous compact before response delivery"
                );

                let compact_ctx = CompactContext {
                    channel: &msg.channel,
                    chat_id: &msg.chat_id,
                    account_id: msg.account_id.as_deref(),
                };
                if let Err(e) = self
                    .capture_pre_compress_learning_boundary(&persist_session_key, &history)
                    .await
                {
                    warn!(error = %e, session_key = %persist_session_key, "Ghost learning pre-compress capture failed");
                }
                let compact_result = self
                    .execute_layer4_compact(
                        &history,
                        &persist_session_key,
                        Some(compact_ctx),
                        true, // is_auto for automatic compact
                    )
                    .await;
                if compact_result.success {
                    // 压缩成功，替换历史
                    Self::rebuild_messages_after_compact(&mut history, &compact_result);

                    info!(
                        post_compact_tokens = estimate_messages_tokens(&history),
                        "[post-sampling] Compact completed, history replaced"
                    );
                    metrics.record_compression();

                    self.finalize_trackers_after_compact(&history);
                } else {
                    warn!(
                        error = ?compact_result.error,
                        "[post-sampling] Compact failed, continuing without compression"
                    );
                }
            }

            // 清理已完成的后台任务
            if let Some(ms) = self.memory_system.as_mut() {
                let cleaned = ms.cleanup_completed_tasks();
                if cleaned > 0 {
                    debug!(
                        cleaned_count = cleaned,
                        "Cleaned up completed background tasks"
                    );
                }
            }
        }

        let delivered_response = self
            .persist_and_deliver_final_response(FinalResponseContext {
                msg: &msg,
                persist_session_key: &persist_session_key,
                history: &mut history,
                session_metadata: &session_metadata,
                final_response: &final_response,
                collected_media,
                cron_deliver_target,
            })
            .await?;

        if !llm_failed_after_retries {
            for task_id in &prompt_injected_completed_task_ids {
                self.task_manager.mark_result_injected(task_id).await;
            }
        }

        // cron 投递场景下 Ghost sync/prefetch 使用 persist_session_key（目标会话），
        // 确保学习/召回归属到目标会话而非源会话
        if let Some(manager) = self.ghost_memory_lifecycle.as_ref() {
            manager.sync_all(&msg.content, &delivered_response, &persist_session_key);
            manager.queue_prefetch_all(&msg.content, &persist_session_key);
        }

        self.spawn_pending_ghost_background_reviews();

        Ok(delivered_response)
    }
}
