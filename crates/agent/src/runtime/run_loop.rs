use super::*;

impl AgentRuntime {
    pub async fn run_loop(
        &mut self,
        mut inbound_rx: mpsc::Receiver<InboundMessage>,
        mut shutdown_rx: Option<broadcast::Receiver<()>>,
    ) {
        info!("AgentRuntime started");

        if self.skill_evolution_worker.is_some() {
            info!("Skill evolution durable workflow scheduler enabled");
        }

        let tick_secs = self.config.tools.tick_interval_secs.clamp(10, 300) as u64;
        info!(tick_secs = tick_secs, "Tick interval configured");
        let mut tick_interval = tokio::time::interval(std::time::Duration::from_secs(tick_secs));
        tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut active_chat_tasks: HashMap<ActiveConversationKey, String> = HashMap::new();
        let mut active_steering_senders: HashMap<ActiveConversationKey, SteeringSender> =
            HashMap::new();
        let mut active_message_tasks: HashMap<String, tokio::task::AbortHandle> = HashMap::new();
        let mut active_abort_tokens: HashMap<String, AbortToken> = HashMap::new();
        let (task_done_tx, mut task_done_rx) =
            mpsc::unbounded_channel::<(String, ActiveConversationKey)>();
        let active_steering_registry = self.active_steering_registry.clone();
        let runtime_agent_id = self
            .agent_id
            .clone()
            .unwrap_or_else(|| "default".to_string());

        async fn abort_active_message_tasks(
            task_manager: &TaskManager,
            active_steering_registry: Option<&SteeringRegistry>,
            active_chat_tasks: &mut HashMap<ActiveConversationKey, String>,
            active_steering_senders: &mut HashMap<ActiveConversationKey, SteeringSender>,
            active_message_tasks: &mut HashMap<String, tokio::task::AbortHandle>,
            active_abort_tokens: &mut HashMap<String, AbortToken>,
        ) {
            let active_task_ids: Vec<String> = active_message_tasks.keys().cloned().collect();
            for task_id in active_task_ids {
                // Graceful cancellation via AbortToken
                if let Some(token) = active_abort_tokens.remove(&task_id) {
                    token.cancel();
                }
                if let Some(handle) = active_message_tasks.remove(&task_id) {
                    handle.abort();
                }
                task_manager.remove_task(&task_id).await;
            }
            if let Some(registry) = active_steering_registry {
                let mut registry = registry.lock().await;
                for conversation_key in active_chat_tasks.keys() {
                    registry.remove(conversation_key);
                }
            }
            active_chat_tasks.clear();
            active_steering_senders.clear();
        }

        loop {
            tokio::select! {
                _ = async {
                    if let Some(ref mut rx) = shutdown_rx {
                        let _ = rx.recv().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    if let Err(e) = self.capture_main_session_end_learning_boundary().await {
                        warn!(error = %e, "Ghost learning session-end capture failed during shutdown");
                    }
                    abort_active_message_tasks(
                        &self.task_manager,
                        active_steering_registry.as_ref(),
                        &mut active_chat_tasks,
                        &mut active_steering_senders,
                        &mut active_message_tasks,
                        &mut active_abort_tokens,
                    ).await;
                    self.task_manager.shutdown_shell_processes();
                    break;
                }
                done = task_done_rx.recv() => {
                    if let Some((task_id, conversation_key)) = done {
                        active_message_tasks.remove(&task_id);
                        active_abort_tokens.remove(&task_id);
                        if active_chat_tasks.get(&conversation_key).is_some_and(|id| id == &task_id) {
                            active_chat_tasks.remove(&conversation_key);
                            active_steering_senders.remove(&conversation_key);
                            if let Some(registry) = active_steering_registry.as_ref() {
                                registry.lock().await.remove(&conversation_key);
                            }
                        }
                    }
                }
                msg = inbound_rx.recv() => {
                    match msg {
                        Some(mut msg) => {
                            if msg.metadata.get("cancel").and_then(|v| v.as_bool()).unwrap_or(false) {
                                let chat_id = msg.chat_id.clone();
                                let conversation_key = ActiveConversationKey::from_message(
                                    &runtime_agent_id,
                                    &msg,
                                );
                                let mut cancelled = false;
                                if let Some(task_id) = active_chat_tasks.remove(&conversation_key) {
                                    active_steering_senders.remove(&conversation_key);
                                    if let Some(registry) = active_steering_registry.as_ref() {
                                        registry.lock().await.remove(&conversation_key);
                                    }
                                    // Graceful cancellation via AbortToken
                                    if let Some(token) = active_abort_tokens.remove(&task_id) {
                                        token.cancel();
                                    }
                                    if let Some(handle) = active_message_tasks.remove(&task_id) {
                                        handle.abort();
                                        cancelled = true;
                                        self.task_manager.remove_task(&task_id).await;
                                        info!(chat_id = %chat_id, task_id = %task_id, "Cancelled running chat task");
                                    }
                                }
                                if cancelled {
                                    if let Some(ref event_tx) = self.event_tx {
                                        let _ = event_tx.send(
                                            serde_json::json!({
                                                "type": "message_done",
                                                "channel": "ws",
                                                "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                                                "chat_id": chat_id,
                                                "task_id": "",
                                                "content": "⏹️ 当前对话已终止",
                                                "tool_calls": 0,
                                                "duration_ms": 0
                                            }).to_string()
                                        );
                                    }
                                }
                                continue;
                            }

                            // ── 处理 /cancel-task 取消指令 ──
                            // ForwardToRuntime 传递 [cancel:task_id=xxx]，runtime 触发 AbortToken + JoinHandle 取消
                            // 安全检查：仅接受来自斜杠命令系统的消息，防止用户伪造指令
                            if msg.content.starts_with("[cancel:task_id=") {
                                if msg.metadata.get("source").and_then(|v| v.as_str()) != Some("slash_command") {
                                    warn!("Ignoring cancel directive from non-slash-command source");
                                    continue;
                                }
                                let task_id = msg.content
                                    .strip_prefix("[cancel:task_id=")
                                    .and_then(|s| s.strip_suffix("]"))
                                    .unwrap_or("");
                                if !task_id.is_empty() {
                                    // 1. 触发 AbortToken 取消（链式取消子任务）
                                    if let Some(token) = active_abort_tokens.remove(task_id) {
                                        token.cancel();
                                        info!(task_id = %task_id, "Cancelled AbortToken for task");
                                    } else {
                                        warn!(task_id = %task_id, "No AbortToken found for cancel");
                                    }

                                    // 2. 终止 JoinHandle（停止 tokio task）
                                    if let Some(handle) = active_message_tasks.remove(task_id) {
                                        handle.abort();
                                        info!(task_id = %task_id, "Aborted JoinHandle for task");
                                    }

                                    // 3. 从 active_chat_tasks 中移除
                                    let conversation_key_to_remove: Option<ActiveConversationKey> = {
                                        active_chat_tasks
                                            .iter()
                                            .find(|(_, tid)| *tid == task_id)
                                            .map(|(key, _)| key.clone())
                                    };
                                    if let Some(conversation_key) = conversation_key_to_remove {
                                        active_chat_tasks.remove(&conversation_key);
                                        active_steering_senders.remove(&conversation_key);
                                        if let Some(registry) = active_steering_registry.as_ref() {
                                            registry.lock().await.remove(&conversation_key);
                                        }
                                    }

                                    info!(task_id = %task_id, "Task cancellation completed");
                                }
                                continue;
                            }

                            // ── 处理 /resume_task 恢复指令 ──
                            // ForwardToRuntime 传递 [resume_task:task_id=xxx]，runtime 从 checkpoint 加载对话历史
                            // 安全检查：仅接受来自斜杠命令系统的消息，防止用户伪造指令
                            if msg.content.starts_with("[resume_task:task_id=") {
                                if msg.metadata.get("source").and_then(|v| v.as_str()) != Some("slash_command") {
                                    warn!("Ignoring resume_task directive from non-slash-command source");
                                    continue;
                                }
                                let task_id = msg.content
                                    .strip_prefix("[resume_task:task_id=")
                                    .and_then(|s| s.strip_suffix("]"))
                                    .unwrap_or("")
                                    .to_string(); // 转为 owned String，解除对 msg.content 的借用
                                if !task_id.is_empty() {
                                    // 从 checkpoint 加载对话历史并注入当前会话
                                    let checkpoint_manager = crate::checkpoint::CheckpointManager::new(&self.paths.workspace());
                                    match checkpoint_manager.load_for_origin(
                                        &task_id,
                                        &msg.channel,
                                        &msg.chat_id,
                                    ) {
                                        Ok(Some(cp)) => {
                                            // 将 checkpoint 的对话历史注入到 session store
                                            let session_key = msg.session_key();
                                            // 使用 save 替换整个会话历史为 checkpoint 内容
                                            if let Err(e) = self.session_store.save(&session_key, &cp.messages) {
                                                warn!(error = %e, "Failed to save resumed checkpoint to session store");
                                                continue;
                                            }
                                            info!(
                                                task_id = %task_id,
                                                messages = cp.messages.len(),
                                                turn = cp.turn,
                                                "Resumed task from checkpoint"
                                            );
                                            // 注意：不立即标记 checkpoint 为已完成
                                            // 如果恢复后执行再次失败，用户可以再次 /resume
                                            // checkpoint 会在任务最终完成时由 run_message_task 标记

                                            // 发送恢复确认事件
                                            if let Some(ref event_tx) = self.event_tx {
                                                let _ = event_tx.send(
                                                    serde_json::json!({
                                                        "type": "message_done",
                                                        "channel": msg.channel,
                                                        "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                                                        "chat_id": msg.chat_id,
                                                        "task_id": task_id,
                                                        "content": format!("🔄 已从断点恢复任务，轮次: {}，消息数: {}，正在继续执行...", cp.turn, cp.messages.len()),
                                                        "tool_calls": 0,
                                                        "duration_ms": 0
                                                    }).to_string()
                                                );
                                            }

                                            // 从 checkpoint 中提取最后一条用户消息作为继续执行的输入
                                            // 这样 LLM 会基于恢复的对话历史继续生成回复
                                            let last_user_content: String = cp.messages.iter().rev()
                                                .find(|m| m.role == "user")
                                                .and_then(|m| m.content.as_str())
                                                .unwrap_or("请继续执行未完成的任务")
                                                .to_string();

                                            // 将消息内容替换为继续指令，走正常的消息处理流程
                                            // 标记 metadata 表明这是 resume 自动继续，不是用户新输入
                                            msg.content = format!("[resume_task:continue] {}", last_user_content);
                                            msg.metadata = serde_json::json!({
                                                "source": "resume_auto_continue",
                                                "resumed_task_id": task_id
                                            });
                                            // 不 continue，让消息走下面的正常 spawn 流程
                                        }
                                        Ok(None) => {
                                            warn!(task_id = %task_id, "Checkpoint not found for resume");
                                            continue;
                                        }
                                        Err(e) => {
                                            warn!(task_id = %task_id, error = %e, "Failed to load checkpoint for resume");
                                            continue;
                                        }
                                    }
                                } else {
                                    continue;
                                }
                            }

                            let source = msg.metadata.get("source").and_then(|v| v.as_str());
                            let may_route_to_steering = msg.media.is_empty()
                                && !matches!(
                                    source,
                                    Some("slash_command") | Some("resume_auto_continue")
                                );
                            if may_route_to_steering {
                                let conversation_key = ActiveConversationKey::from_message(
                                    &runtime_agent_id,
                                    &msg,
                                );
                                if let Some(sender) = active_steering_senders.get(&conversation_key).cloned() {
                                    let steering_message = SteeringMessage {
                                        content: msg.content.clone(),
                                        channel: msg.channel.clone(),
                                        chat_id: msg.chat_id.clone(),
                                    };
                                    match sender.try_route(steering_message) {
                                        SteeringRouteOutcome::Enqueued => {
                                            info!(
                                                channel = %msg.channel,
                                                chat_id = %msg.chat_id,
                                                "Routed inbound message to active steering channel"
                                            );
                                            continue;
                                        }
                                        SteeringRouteOutcome::Full => {
                                            warn!(
                                                channel = %msg.channel,
                                                chat_id = %msg.chat_id,
                                                "Active steering channel is full; rejecting newest steering message"
                                            );
                                            super::message_task::deliver_message_task_failure(
                                                &msg,
                                                "steering_rejected",
                                                self.agent_id.as_deref(),
                                                "当前任务正在处理中，输入队列已满，请稍后重发这条消息",
                                                self.event_tx.as_ref(),
                                                self.outbound_tx.as_ref(),
                                            )
                                            .await;
                                            continue;
                                        }
                                        SteeringRouteOutcome::Closed => {
                                            warn!(
                                                chat_id = %msg.chat_id,
                                                "Active steering channel closed; falling back to new message task"
                                            );
                                            active_steering_senders.remove(&conversation_key);
                                            if let Some(registry) = active_steering_registry.as_ref() {
                                                registry.lock().await.remove(&conversation_key);
                                            }
                                        }
                                    }
                                }
                            }

                            self.update_main_session_target(&msg).await;

                            // Spawn each message as a background task so the loop
                            // stays responsive for new user input.
                            let task_id = format!("msg_{}", uuid::Uuid::new_v4());
                            let label = if msg.content.chars().count() > 40 {
                                format!("{}...", truncate_str(&msg.content, 40))
                            } else {
                                msg.content.clone()
                            };

                            let task_manager = self.task_manager.clone();
                            let task_id_clone = task_id.clone();
                            let conversation_key =
                                ActiveConversationKey::from_message(&runtime_agent_id, &msg);
                            let task_done_tx = task_done_tx.clone();
                            let done_task_id = task_id.clone();
                            let done_conversation_key = conversation_key.clone();
                            let completion_receipt_id = msg
                                .metadata
                                .get(blockcell_core::message_receipt::MESSAGE_RECEIPT_ID)
                                .and_then(|value| value.as_str())
                                .map(str::to_string);

                            // 原子性地注册任务并标记为 Running（消除竞态窗口）
                            task_manager.create_and_start_task(
                                &task_id,
                                &label,
                                &msg.content,
                                &msg.channel,
                                &msg.chat_id,
                                self.agent_id.as_deref(),
                                false,
                                None,   // agent_type
                                false,  // one_shot
                            ).await;

                            if !msg.metadata.is_object() {
                                msg.metadata = serde_json::json!({});
                            }
                            if let Some(metadata) = msg.metadata.as_object_mut() {
                                metadata.insert(
                                    "runtime_task_id".to_string(),
                                    serde_json::Value::String(task_id.clone()),
                                );
                            }

                            if let Some(prev_task_id) = active_chat_tasks.remove(&conversation_key) {
                                active_steering_senders.remove(&conversation_key);
                                if let Some(registry) = active_steering_registry.as_ref() {
                                    registry.lock().await.remove(&conversation_key);
                                }
                                // 清理前一个任务的 AbortToken（防止内存泄漏）
                                if let Some(prev_token) = active_abort_tokens.remove(&prev_task_id) {
                                    prev_token.cancel();
                                }
                                if let Some(prev_handle) = active_message_tasks.remove(&prev_task_id) {
                                    prev_handle.abort();
                                    self.task_manager.remove_task(&prev_task_id).await;
                                    info!(
                                        session_key = %conversation_key.session_key,
                                        task_id = %prev_task_id,
                                        "Cancelled previous running chat task"
                                    );
                                }
                            }

                            let (steering, steering_sender) = SteeringChannel::new(16);
                            active_chat_tasks.insert(conversation_key.clone(), task_id.clone());
                            active_steering_senders
                                .insert(conversation_key.clone(), steering_sender.clone());
                            if let Some(registry) = active_steering_registry.as_ref() {
                                registry.lock().await.insert(
                                    conversation_key.clone(),
                                    steering_sender.clone(),
                                );
                            }
                            // Create AbortToken for this message task (child of runtime's token)
                            let msg_abort_token = self.abort_token.child();
                            active_abort_tokens.insert(task_id.clone(), msg_abort_token.clone());
                            let message_runtime = self.fork_for_message_task(
                                steering,
                                steering_sender,
                                msg_abort_token,
                            );
                            let handle = tokio::spawn(async move {
                                run_message_task(
                                    message_runtime,
                                    msg,
                                    task_id_clone,
                                ).await;
                            });
                            let abort_handle = handle.abort_handle();
                            active_message_tasks.insert(task_id, abort_handle);
                            tokio::spawn(supervise_message_task(
                                handle,
                                self.task_manager.clone(),
                                done_task_id,
                                completion_receipt_id,
                                task_done_tx,
                                done_conversation_key,
                            ));
                        }
                        None => {
                            if let Err(e) = self.capture_main_session_end_learning_boundary().await {
                                warn!(error = %e, "Ghost learning session-end capture failed on inbound close");
                            }
                            abort_active_message_tasks(
                                &self.task_manager,
                                active_steering_registry.as_ref(),
                                &mut active_chat_tasks,
                                &mut active_steering_senders,
                                &mut active_message_tasks,
                                &mut active_abort_tokens,
                            ).await;
                            break
                        }, // channel closed
                    }
                }
                _ = tick_interval.tick() => {
                    // Auto-cleanup completed/failed tasks older than 5 minutes
                    self.task_manager.cleanup_old_tasks(
                        std::time::Duration::from_secs(300)
                    ).await;

                    // Memory maintenance (TTL cleanup, recycle bin purge)
                    if let Some(ref store) = self.memory_store {
                        if let Err(e) = store.maintenance(30) {
                            warn!(error = %e, "Memory maintenance error");
                        }
                    }

                    // .tool_results 磁盘清理：删除过期条目并限制每会话数量
                    // 防止持久化大工具输出无限累积占用磁盘空间。
                    // 使用 Layer1 配置中的 cache_max_per_session 而非硬编码值，
                    // 保证清理策略与运行时配置一致
                    let max_per_session = self
                        .config
                        .memory
                        .memory_system
                        .layer1
                        .cache_max_per_session;
                    let (removed_entries, _removed_dirs) = cleanup_tool_results(
                        &self.paths.workspace(),
                        7, // 7 天 TTL（磁盘持久化结果的标准保留期）
                        max_per_session,
                    ).await;
                    // 同步更新 Layer1 指标，避免 /session-metrics 显示只增不减的存储数
                    if removed_entries > 0 {
                        crate::session_metrics::get_memory_metrics()
                            .layer1
                            .decrement_stored_count(removed_entries as u64);
                    }

                    // session_events 清理：会话超过保留期未产生新事件时删除其事件日志
                    let event_log = blockcell_storage::JsonlSessionEventLog::new(self.paths.clone());
                    match tokio::task::spawn_blocking(move || {
                        event_log.cleanup_older_than(std::time::Duration::from_secs(
                            blockcell_storage::SESSION_EVENT_RETENTION_DAYS * 86_400,
                        ))
                    })
                    .await
                    {
                        Ok(Ok(removed)) if removed > 0 => {
                            debug!(removed, "Cleaned up expired session event logs");
                        }
                        Ok(Err(e)) => warn!(error = %e, "Session event log cleanup failed"),
                        _ => {}
                    }

                    let _ = self
                        .process_system_event_tick(chrono::Utc::now().timestamp_millis())
                        .await;

                    // Wake skill evolution worker. The worker owns the long LLM/audit/compile
                    // pipeline, so this select branch stays responsive.
                    if let Some(ref worker) = self.skill_evolution_worker {
                        worker.notify();
                    }

                    // 唤醒核心进化 worker — 轻量操作，不执行长任务，不拿 mutex
                    if let Some(ref worker) = self.evolution_worker {
                        worker.notify();
                    }

                    // Periodic skill hot-reload (picks up skills created by chat)
                    let new_skills = self.context_builder.reload_skills();
                    if !new_skills.is_empty() {
                        info!(skills = ?new_skills, "🔄 Tick: hot-reloaded new skills");
                        if let Some(ref event_tx) = self.event_tx {
                            let event = serde_json::json!({
                                "type": "skills_updated",
                                "new_skills": new_skills,
                            });
                            let _ = event_tx.send(event.to_string());
                        }
                    }

                    // Refresh capability brief for prompt injection + sync capability IDs to SkillManager
                    if let Some(ref registry_handle) = self.capability_registry {
                        let brief = registry_handle.generate_brief().await;
                        self.context_builder.set_capability_brief(brief);
                        // Sync available capability IDs so SkillManager can validate skill dependencies
                        let cap_ids = registry_handle.list_available_ids().await;
                        self.context_builder.sync_capabilities(cap_ids);
                    }

                    // 自动触发缺失能力的进化 — 通过 workflow store 快速入队，不拿 engine mutex
                    // 24 小时冷却，防止重复请求
                    if let Some(ref workflow_store) = self.evolution_workflow_store {
                        let missing = self.context_builder.get_missing_capabilities();
                        let now = chrono::Utc::now().timestamp();
                        const COOLDOWN_SECS: i64 = 86400; // 24 小时

                        for (skill_name, cap_id) in missing {
                            // 冷却检查：24 小时内不重复请求
                            if let Some(&last_request) = self.cap_request_cooldown.get(&cap_id) {
                                if now - last_request < COOLDOWN_SECS {
                                    continue;
                                }
                            }

                            // 检查是否已有活跃或阻塞的工作流
                            match workflow_store.is_active_or_blocked(&cap_id) {
                                Ok(true) => {
                                    debug!(
                                        capability_id = %cap_id,
                                        "🧬 能力 '{}' 已有活跃/阻塞工作流，跳过",
                                        cap_id
                                    );
                                    continue;
                                }
                                Err(e) => {
                                    warn!(error = %e, "检查工作流状态失败");
                                    continue;
                                }
                                _ => {}
                            }

                            let description = format!(
                                "Auto-requested: required by skill '{}'",
                                skill_name
                            );
                            match workflow_store.enqueue(&cap_id, &description, "script") {
                                Ok(_) => {
                                    self.cap_request_cooldown.insert(cap_id.clone(), now);
                                    info!(
                                        capability_id = %cap_id,
                                        skill = %skill_name,
                                        "🧬 自动入队缺失能力 '{}' (skill '{}')",
                                        cap_id, skill_name
                                    );
                                    // 唤醒 worker 处理新入队的工作流
                                    if let Some(ref worker) = self.evolution_worker {
                                        worker.notify();
                                    }
                                }
                                Err(e) => {
                                    self.cap_request_cooldown.insert(cap_id.clone(), now);
                                    debug!(
                                        capability_id = %cap_id,
                                        error = %e,
                                        "入队能力进化失败（已设冷却）"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        abort_active_message_tasks(
            &self.task_manager,
            active_steering_registry.as_ref(),
            &mut active_chat_tasks,
            &mut active_steering_senders,
            &mut active_message_tasks,
            &mut active_abort_tokens,
        )
        .await;
        if let Some(manager) = self.ghost_memory_lifecycle.as_ref() {
            manager.shutdown_all();
        }
        info!("AgentRuntime stopped");
    }
}
