use super::*;

impl AgentRuntime {
    /// Wire the evolution deploy callback so that successful skill deployments
    /// trigger an EvolutionSuccess Ghost learning boundary.
    /// Must be called after construction (learning_coordinator needs to exist).
    pub fn wire_evolution_deploy_callback(&mut self) {
        let learning_coordinator = self.learning_coordinator.clone();
        let config = self.config.clone();
        let paths = self.paths.clone();
        let event_bus = self.event_bus.clone();
        let event_ctx = AgentEventContext {
            agent_id: self
                .agent_id
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            ..Default::default()
        };

        let callback: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |skill_name: &str| {
            // Invalidate prompt snapshot so next skill prompt generation reads fresh content
            if let Err(e) = blockcell_skills::SkillManager::invalidate_prompt_snapshot(&paths) {
                tracing::warn!(
                    skill = %skill_name,
                    error = %e,
                    "[evolution] Failed to invalidate prompt snapshot after deploy"
                );
            }

            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let event_bus = event_bus.clone();
                let ctx = event_ctx.clone();
                let mut event = AgentEvent::EvolutionDeployed {
                    skill: skill_name.to_string(),
                };
                handle.spawn(async move {
                    event_bus.dispatch(&ctx, &mut event).await;
                });
            }

            if !config.agents.ghost.learning.capture_enabled() {
                return;
            }
            let boundary = GhostLearningBoundary {
                kind: GhostLearningBoundaryKind::EvolutionSuccess,
                session_key: None,
                subject_key: Some(format!("skill:{}", skill_name)),
                user_intent_summary: format!(
                    "Skill '{}' evolution deployed successfully",
                    skill_name
                ),
                assistant_outcome_summary: String::new(),
                tool_call_count: 0,
                memory_write_count: 0,
                correction_count: 0,
                preference_correction_count: 0,
                success: true,
                complexity_score: 0,
                reusable_lesson: None,
                turn_facts: None,
            };
            learning_coordinator.update_ghost_policy(&config.agents.ghost.learning);
            let decision = learning_coordinator.ghost_decide(&boundary);
            if let Err(e) = persist_ghost_learning_boundary_with_decision(
                &config,
                &paths,
                boundary,
                vec![],
                decision,
            ) {
                tracing::warn!(
                    skill = %skill_name,
                    error = %e,
                    "[evolution] Failed to persist EvolutionSuccess ghost boundary"
                );
            }
        });
        self.context_builder.set_evolution_deploy_callback(callback);
    }

    /// Cancel this runtime and all its sub-agents.
    pub fn cancel(&self) {
        self.abort_token.cancel();
    }

    /// Check if this runtime has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.abort_token.is_cancelled()
    }

    /// Get a reference to the AbortToken.
    pub fn abort_token(&self) -> &AbortToken {
        &self.abort_token
    }

    /// Set the AbortToken (used by run_message_task to inherit parent cancellation).
    pub fn set_abort_token(&mut self, token: AbortToken) {
        self.abort_token = token;
    }

    /// 把父会话的预算 tracker 挂到本运行时（子 Agent 归并），子消耗计入父预算。
    pub fn set_parent_budget(&mut self, parent: Option<BudgetTrackerHandle>) {
        self.parent_budget = parent;
    }

    /// Build permissions for tool execution based on channel, sender, and chat context.
    ///
    /// This method grants appropriate permissions based on:
    /// - Channel type (napcat, telegram, discord, etc.)
    /// - User whitelist membership
    /// - Admin status
    pub(crate) fn build_tool_permissions(
        &self,
        channel: &str,
        sender_id: Option<&str>,
        chat_id: &str,
    ) -> blockcell_core::types::PermissionSet {
        use blockcell_core::types::PermissionSet;

        let mut perms = PermissionSet::new();

        // Grant channel-specific permissions
        match channel {
            "napcat" => {
                // Use NapCat-specific permission builder
                #[cfg(feature = "napcat")]
                {
                    perms = blockcell_tools::napcat::build_napcat_user_permissions(
                        &self.config.channels.napcat,
                        sender_id,
                        chat_id,
                    );
                }
                #[cfg(not(feature = "napcat"))]
                {
                    _ = (sender_id, chat_id); // Suppress unused variable warning
                    perms = perms.with_permission("channel:napcat");
                }
            }
            "telegram" => {
                perms = perms.with_permission("channel:telegram");
                // Grant basic tool access for telegram users
                perms = perms.with_permission("telegram:tools");
            }
            "discord" => {
                perms = perms.with_permission("channel:discord");
                perms = perms.with_permission("discord:tools");
            }
            "slack" => {
                perms = perms.with_permission("channel:slack");
                perms = perms.with_permission("slack:tools");
            }
            "feishu" | "lark" => {
                perms = perms.with_permission(&format!("channel:{}", channel));
                perms = perms.with_permission("feishu:tools");
            }
            "wecom" => {
                perms = perms.with_permission("channel:wecom");
                perms = perms.with_permission("wecom:tools");
            }
            "dingtalk" => {
                perms = perms.with_permission("channel:dingtalk");
                perms = perms.with_permission("dingtalk:tools");
            }
            "whatsapp" => {
                perms = perms.with_permission("channel:whatsapp");
                perms = perms.with_permission("whatsapp:tools");
            }
            "cli" => {
                // CLI mode gets full permissions
                perms = perms.with_permission("channel:cli");
                perms = perms.with_permission("cli:tools");
            }
            _ => {
                // Unknown channel - grant basic access
                perms = perms.with_permission(&format!("channel:{}", channel));
            }
        }

        perms
    }

    pub fn context_builder(&self) -> &ContextBuilder {
        &self.context_builder
    }

    pub fn set_outbound(&mut self, tx: mpsc::Sender<OutboundMessage>) {
        self.outbound_tx = Some(tx);
    }

    pub fn set_inbound(&mut self, tx: mpsc::Sender<InboundMessage>) {
        self.inbound_tx = Some(tx);
    }

    pub fn set_confirm(&mut self, tx: mpsc::Sender<ConfirmRequest>) {
        self.confirm_tx = Some(tx);
    }

    /// Get a reference to the task manager.
    pub fn task_manager(&self) -> &TaskManager {
        &self.task_manager
    }

    /// Set a shared task manager (e.g. from the command layer).
    pub fn set_task_manager(&mut self, tm: TaskManager) {
        self.task_manager = tm;
        self.sync_task_manager_event_emitter();
    }

    pub fn set_agent_id(&mut self, agent_id: Option<String>) {
        self.context_builder.set_agent_id(agent_id.as_deref());
        self.agent_id = agent_id;
        self.sync_task_manager_event_emitter();
        let session_id = self.tool_registry.scope_view().session_id.clone();
        self.sync_tool_scope_view(&session_id);
    }

    /// 让工具 registry 的作用域视图与当前 agent / 会话 / 进化候选保持一致，
    /// 使 agent、session、candidate 作用域注册的工具只在对应视图中可见、可执行。
    pub(crate) fn sync_tool_scope_view(&mut self, session_id: &str) {
        let view = blockcell_tools::ToolScopeView {
            agent_id: self
                .agent_id
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            session_id: session_id.to_string(),
            candidate_id: self.candidate_scope.clone(),
        };
        self.tool_registry.set_scope_view(view);
    }

    /// 在指定作用域注册工具（例如进化候选工具、仅对某会话开放的 MCP 工具）；guard 销毁时注销。
    pub fn register_scoped_tool(
        &self,
        target: crate::runtime_kernel::ScopeTarget,
        tool: Arc<dyn blockcell_tools::Tool>,
    ) -> crate::runtime_kernel::ScopeGuard<blockcell_tools::ScopedTool> {
        self.tool_registry.register_scoped(target, tool)
    }

    /// Set the broadcast sender for streaming events to WebSocket clients.
    pub fn set_event_tx(&mut self, tx: broadcast::Sender<String>) {
        *self
            .event_tx_slot
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Some(tx.clone());
        self.event_tx = Some(tx);
    }

    /// Runtime event bus. Registering a listener here does not require touching the agent loop.
    pub fn event_bus(&self) -> &EventBus {
        &self.event_bus
    }

    /// Enter (or leave with `None`) an evolution candidate scope. Candidate-scoped listeners
    /// and prompt sections become visible only to this runtime; production runtimes never see them.
    pub fn set_candidate_scope(&mut self, candidate_id: Option<String>) {
        self.context_builder
            .set_candidate_scope(candidate_id.clone());
        self.candidate_scope = candidate_id;
        let session_id = self.tool_registry.scope_view().session_id.clone();
        self.sync_tool_scope_view(&session_id);
    }

    /// Replace the loop driver used by `process_message`.
    pub fn set_agent_loop(&mut self, agent_loop: Arc<dyn agent_loop::AgentLoop>) {
        self.agent_loop = agent_loop;
    }

    /// Replace the strategy used to summarize history during Layer4 compaction.
    pub fn set_context_compactor(&mut self, compactor: Arc<dyn crate::compact::ContextCompactor>) {
        self.context_compactor = compactor;
    }

    pub fn agent_loop_name(&self) -> &'static str {
        self.agent_loop.name()
    }

    /// 开始新的 step：在整个 turn 内单调递增（包括嵌套技能循环），并设为当前 step。
    pub(crate) fn begin_step(&mut self) -> u32 {
        self.turn_step_counter = self.turn_step_counter.saturating_add(1);
        self.current_step = Some(self.turn_step_counter);
        self.turn_step_counter
    }

    pub(crate) async fn end_step(
        &self,
        msg: &InboundMessage,
        session_id: &str,
        iteration: u32,
        tool_calls: usize,
    ) {
        self.dispatch_event(
            msg,
            session_id,
            &mut AgentEvent::StepEnd {
                iteration,
                tool_calls,
            },
        )
        .await;
    }

    /// 关闭当前回合：派发 turn/end 并清除 turn/step 上下文，避免回合外事件沿用旧的 turn_id。
    pub(crate) async fn end_turn(
        &mut self,
        msg: &InboundMessage,
        session_id: &str,
        final_response: String,
        error: Option<String>,
    ) {
        self.current_step = None;
        self.dispatch_event(
            msg,
            session_id,
            &mut AgentEvent::TurnEnd {
                final_response,
                error,
            },
        )
        .await;
        self.current_turn_id = None;
    }

    pub(crate) async fn dispatch_event(
        &self,
        msg: &InboundMessage,
        session_id: &str,
        event: &mut AgentEvent,
    ) -> EventDecision {
        let ctx = AgentEventContext {
            agent_id: self
                .agent_id
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            session_id: session_id.to_string(),
            channel: msg.channel.clone(),
            chat_id: msg.chat_id.clone(),
            turn_id: self.current_turn_id.clone(),
            step: self.current_step,
            candidate_id: self.candidate_scope.clone(),
        };
        self.event_bus.dispatch(&ctx, event).await
    }

    pub fn steering_sender(&self) -> SteeringSender {
        self.steering_sender.clone()
    }

    pub fn set_steering_channel(&mut self, steering: SteeringChannel, sender: SteeringSender) {
        self.steering = steering;
        self.steering_sender = sender;
    }

    pub fn set_active_steering_registry(&mut self, registry: SteeringRegistry) {
        self.active_steering_registry = Some(registry);
    }

    pub fn set_event_emitter(&mut self, emitter: EventEmitterHandle) {
        self.system_event_emitter = emitter;
        self.sync_task_manager_event_emitter();
    }

    pub fn event_emitter_handle(&self) -> EventEmitterHandle {
        self.system_event_emitter.clone()
    }

    /// Set a shared ResponseCache instance.
    ///
    /// This allows external code (like the CLI stdin loop) to share the same
    /// cache instance with the runtime, enabling cache clearing via `/clear` command.
    pub fn set_response_cache(&mut self, cache: crate::response_cache::ResponseCache) {
        self.response_cache = cache;
    }

    /// Get a reference to the ResponseCache.
    ///
    /// This is useful for external code to clear session caches.
    pub fn response_cache(&self) -> &crate::response_cache::ResponseCache {
        &self.response_cache
    }

    /// Initialize the 7-layer memory system for this session.
    ///
    /// This method creates the memory system and performs async initialization:
    /// - Loads cursor state from disk
    /// - Marks session as active (creates `.active` file)
    pub async fn init_memory_system(&mut self, session_id: String) -> std::io::Result<()> {
        use crate::memory_system::MemorySystem;

        let mut config = self.config.memory.memory_system.clone();
        config.layer7 = self.config.memory.effective_forked_agent();
        // Use paths.base as both workspace and config directory
        let base_dir = self.paths.base.clone();

        // 注意：Dream session count 不再在此处无条件递增。
        // 改为在 process_message 中根据会话是否为全新创建来决定是否递增，
        // 避免 Gateway/异步消息模式下每条消息都错误推进 Dream 门控。
        // 参见 process_message 中 is_new_session 的判断逻辑。

        let mut memory_system = MemorySystem::new(config, base_dir.clone(), base_dir, session_id);

        // Perform async initialization: load cursor state + mark session active
        memory_system.initialize().await?;

        // 扫描并清理过期 journal（上次进程退出时遗留的未完成任务）
        // 仅清理超过 stale 阈值的 journal 及其 pending marker，保留仍在运行的任务
        memory_system.cleanup_orphaned_journals();

        // 仅当用户显式写了 circuitBreaker 时才覆盖分层默认值。
        {
            use crate::session_metrics::CircuitBreakerConfig as AgentCBConfig;
            let cb_settings = &self.config.memory.memory_system.circuit_breaker;
            if cb_settings.is_configured() {
                let cb_config = AgentCBConfig::from_memory_config(cb_settings);
                crate::session_metrics::set_circuit_breaker_configs(&cb_config);
            }
        }

        // ========== Record config for all layers to metrics ==========

        // Layer 1: Tool Result Storage
        crate::memory_event!(
            layer1,
            config,
            memory_system.config().layer1.cache_max_per_session,
            memory_system.config().layer1.preview_size_chars
        );

        // Layer 2: Micro Compact
        let layer2_config = crate::history_projector::TimeBasedMCConfig::from(
            memory_system.config().layer2.clone(),
        );
        crate::memory_event!(
            layer2,
            config,
            layer2_config.gap_threshold_minutes,
            layer2_config.keep_recent
        );

        // Layer 3: Session Memory
        crate::memory_event!(
            layer3,
            config,
            memory_system
                .config()
                .layer3
                .max_total_session_memory_tokens,
            memory_system.config().layer3.max_section_length
        );

        // Layer 4: Full Compact
        let recovery_budget = memory_system.config().layer4.max_file_recovery_tokens
            + memory_system.config().layer4.max_skill_recovery_tokens
            + memory_system
                .config()
                .layer4
                .max_session_memory_recovery_tokens;
        crate::memory_event!(
            layer4,
            config,
            memory_system.config().token_budget,
            memory_system.config().layer4.compact_threshold_ratio,
            recovery_budget
        );

        // Layer 5: Memory Extraction
        crate::memory_event!(
            layer5,
            config,
            memory_system.config().layer5.min_messages_for_extraction,
            memory_system.config().layer5.extraction_cooldown_messages,
            memory_system.config().layer5.max_memory_file_tokens
        );

        // Layer 6: Auto Dream
        crate::memory_event!(
            layer6,
            config,
            memory_system.config().layer6.time_gate_threshold_hours
        );

        // Layer 7: Forked Agent
        crate::memory_event!(layer7, config, memory_system.config().layer7.max_turns);

        self.memory_system = Some(memory_system);

        debug!("[memory_system] initialized for session");
        Ok(())
    }

    /// Get the memory system (if initialized).
    pub fn memory_system(&self) -> Option<&crate::memory_system::MemorySystem> {
        self.memory_system.as_ref()
    }

    /// Get mutable access to the memory system.
    pub fn memory_system_mut(&mut self) -> Option<&mut crate::memory_system::MemorySystem> {
        self.memory_system.as_mut()
    }

    pub(crate) fn sync_task_manager_event_emitter(&self) {
        self.task_manager
            .register_event_emitter(self.agent_id.as_deref(), self.system_event_emitter.clone());
    }

    pub(crate) async fn update_main_session_target(&mut self, msg: &InboundMessage) {
        if !is_main_session_candidate(msg) {
            return;
        }

        let next_session_key = msg.session_key();
        if let Some(expected) = self.config.security.main_session_key.as_deref() {
            if next_session_key != expected {
                return;
            }
        } else if msg.channel != "cli" {
            return;
        }
        // Shared background delivery must not follow whichever conversation
        // happens to send the latest message. Pin it atomically across forks.
        {
            let mut shared = self
                .shared_session_target
                .write()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(target) = shared.as_ref() {
                if target.session_key != next_session_key && msg.channel != "cli" {
                    self.main_session_target = Some(target.clone());
                    return;
                }
            } else {
                *shared = Some(MainSessionTarget {
                    channel: msg.channel.clone(),
                    account_id: msg.account_id.clone(),
                    chat_id: msg.chat_id.clone(),
                    session_key: next_session_key.clone(),
                    agent_id: self.agent_id.clone(),
                });
            }
        }
        if self.ghost_learning_enabled() {
            if let Some(previous) = self.main_session_target.as_ref() {
                if previous.session_key != next_session_key {
                    if let Err(err) = self
                        .capture_session_rotate_learning_boundary(previous, msg)
                        .await
                    {
                        warn!(
                            error = %err,
                            from_session = %previous.session_key,
                            to_session = %next_session_key,
                            "Ghost learning session-rotate capture failed"
                        );
                    }
                }
            }
        }

        let target = MainSessionTarget {
            channel: msg.channel.clone(),
            account_id: msg.account_id.clone(),
            chat_id: msg.chat_id.clone(),
            session_key: next_session_key,
            agent_id: self.agent_id.clone(),
        };
        self.main_session_target = Some(target.clone());
        if let Ok(mut guard) = self.shared_session_target.write() {
            if guard
                .as_ref()
                .map(|current| current.session_key == target.session_key)
                .unwrap_or(true)
            {
                *guard = Some(target);
            }
        }
    }

    pub(crate) fn resolve_event_delivery_target(
        &self,
        scope: &EventScope,
    ) -> Option<MainSessionTarget> {
        match scope {
            EventScope::Channel { channel, chat_id } => Some(MainSessionTarget {
                channel: channel.clone(),
                account_id: None,
                chat_id: chat_id.clone(),
                session_key: format!("{}:{}", channel, chat_id),
                agent_id: self.agent_id.clone(),
            }),
            EventScope::Session {
                channel,
                account_id,
                chat_id,
                session_key,
            } => Some(MainSessionTarget {
                channel: channel.clone(),
                account_id: account_id.clone(),
                chat_id: chat_id.clone(),
                session_key: session_key.clone(),
                agent_id: self.agent_id.clone(),
            }),
            EventScope::MainSession | EventScope::Global => self.main_session_target.clone(),
        }
    }

    pub(crate) async fn dispatch_system_event_notification(
        &self,
        request: &NotificationRequest,
    ) -> bool {
        let target = self.resolve_event_delivery_target(&request.scope);
        let target_channel = target.as_ref().map(|value| value.channel.clone());
        let target_chat_id = target.as_ref().map(|value| value.chat_id.clone());

        let event_sent = if let Some(ref event_tx) = self.event_tx {
            let event = serde_json::json!({
                "type": "system_event_notification",
                "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                "event_id": request.event_id.clone(),
                "priority": request.priority,
                "title": request.title.clone(),
                "body": request.body.clone(),
                "channel": target_channel,
                "chat_id": target_chat_id,
            });
            event_tx.send(event.to_string()).is_ok()
        } else {
            false
        };

        if let Some(target) = target {
            if target.channel == "ws" {
                return event_sent;
            }
            if let Some(tx) = &self.outbound_tx {
                let mut outbound = OutboundMessage::new(
                    &target.channel,
                    &target.chat_id,
                    &render_system_notification_text(request),
                );
                outbound.account_id = target.account_id.clone();
                return tx.send(outbound).await.is_ok();
            }
        }
        false
    }

    pub(crate) async fn dispatch_system_event_summary(&self, summary: &SessionSummary) -> bool {
        let target = match summary.items.first().map(|item| &item.scope) {
            Some(blockcell_core::system_event::SummaryScope::Channel { channel, chat_id }) => {
                Some(MainSessionTarget {
                    channel: channel.clone(),
                    account_id: None,
                    chat_id: chat_id.clone(),
                    session_key: format!("{}:{}", channel, chat_id),
                    agent_id: self.agent_id.clone(),
                })
            }
            Some(blockcell_core::system_event::SummaryScope::Session {
                channel,
                account_id,
                chat_id,
                session_key,
            }) => Some(MainSessionTarget {
                channel: channel.clone(),
                account_id: account_id.clone(),
                chat_id: chat_id.clone(),
                session_key: session_key.clone(),
                agent_id: self.agent_id.clone(),
            }),
            _ => self.main_session_target.clone(),
        };
        let target_channel = target.as_ref().map(|value| value.channel.clone());
        let target_chat_id = target.as_ref().map(|value| value.chat_id.clone());

        let event_sent = if let Some(ref event_tx) = self.event_tx {
            let event = serde_json::json!({
                "type": "system_event_summary",
                "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                "channel": target_channel,
                "chat_id": target_chat_id,
                "title": summary.title.clone(),
                "compact_text": summary.compact_text.clone(),
                "items": summary.items.clone(),
            });
            event_tx.send(event.to_string()).is_ok()
        } else {
            false
        };

        if let Some(target) = target {
            if target.channel == "ws" {
                return event_sent;
            }
            if let Some(tx) = &self.outbound_tx {
                let mut outbound = OutboundMessage::new(
                    &target.channel,
                    &target.chat_id,
                    &render_session_summary_text(summary),
                );
                outbound.account_id = target.account_id.clone();
                return tx.send(outbound).await.is_ok();
            }
        }
        false
    }

    pub(crate) async fn process_system_event_tick(&mut self, now_ms: i64) -> HeartbeatDecision {
        let decision = self.system_event_orchestrator.process_tick(now_ms);
        let mut delivered_event_ids = decision.ack_event_ids.clone();

        for request in &decision.immediate_notifications {
            if self.dispatch_system_event_notification(request).await {
                delivered_event_ids.push(request.event_id.clone());
            }
        }

        for summary in &decision.flushed_summaries {
            if self.dispatch_system_event_summary(summary).await {
                delivered_event_ids.extend(
                    summary
                        .items
                        .iter()
                        .flat_map(|item| item.source_event_ids.iter().cloned()),
                );
                let item_ids: Vec<String> =
                    summary.items.iter().map(|item| item.id.clone()).collect();
                self.system_event_orchestrator
                    .queue()
                    .acknowledge_items(&item_ids);
            }
        }

        if !delivered_event_ids.is_empty() {
            delivered_event_ids.sort();
            delivered_event_ids.dedup();
            self.system_event_store.mark_delivered(&delivered_event_ids);
        }

        let _ = self.system_event_store.cleanup_expired(7 * 24 * 60 * 60);

        self.spawn_pending_ghost_background_reviews();

        decision
    }

    pub fn validate_intent_router(&self) -> Result<()> {
        let resolver = crate::intent::IntentToolResolver::new(&self.config);
        let mcp = blockcell_core::mcp_config::McpResolvedConfig::load_merged(&self.paths)?;
        resolver.validate_with_mcp(&self.tool_registry, Some(&mcp))
    }

    /// 设置独立的自进化 LLM provider（可选覆盖，不影响主 pool）
    pub fn set_evolution_provider(&mut self, provider: Box<dyn Provider>) {
        let provider_arc: Arc<dyn Provider> = Arc::from(provider);
        let llm_adapter = Arc::new(ProviderLLMAdapter {
            provider: provider_arc,
        });
        self.context_builder.set_evolution_llm_provider(llm_adapter);
    }

    /// Set the memory store handle for tools and context builder.
    pub fn set_memory_store(&mut self, store: MemoryStoreHandle) {
        self.memory_store = Some(store.clone());
        self.context_builder.set_memory_store(store);
    }

    pub fn init_memory_file_store(&mut self) -> Result<()> {
        let write_enabled = self.config.agents.ghost.learning.write_enabled();
        let mut store = if write_enabled {
            MemoryFileStore::open(&self.paths)?
        } else {
            MemoryFileStore::open_shadow(&self.paths)?
        };
        if write_enabled {
            let index = Arc::new(blockcell_storage::KnowledgeIndex::open(
                &self.paths.knowledge_index_db(),
            )?);
            index.rebuild_from_files(&self.paths)?;
            store.set_knowledge_index(index, "USER.md", "memory/MEMORY.md");
        }
        store.set_write_guard(Arc::clone(&self.write_guard));
        self.memory_file_store = Some(Arc::new(store));
        Ok(())
    }

    pub fn init_skill_file_store(&mut self) -> Result<()> {
        let mut store = SkillFileStore::open(&self.paths)?;
        store.set_write_guard(Arc::clone(&self.write_guard));
        self.skill_file_store = Some(Arc::new(store));
        Ok(())
    }

    // 学习与记忆协调方法 — 已移至 runtime/learning.rs

    /// 返回当前 provider pool（供外部检查状态）
    pub fn provider_pool(&self) -> &Arc<ProviderPool> {
        &self.provider_pool
    }
}
