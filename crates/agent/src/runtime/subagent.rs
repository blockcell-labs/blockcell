use super::*;

/// 把后台子代理的结束回报为发起会话上的 `subagent/completed` 事件。
#[derive(Clone)]
pub(crate) struct SubagentCompletionReporter {
    event_bus: EventBus,
    ctx: AgentEventContext,
}

impl SubagentCompletionReporter {
    pub(crate) fn new(
        event_bus: EventBus,
        agent_id: Option<String>,
        session_id: String,
        channel: &str,
        chat_id: &str,
    ) -> Self {
        Self {
            event_bus,
            ctx: AgentEventContext {
                agent_id: agent_id.unwrap_or_else(|| "default".to_string()),
                session_id,
                channel: channel.to_string(),
                chat_id: chat_id.to_string(),
                ..Default::default()
            },
        }
    }

    pub(crate) async fn report(&self, task_id: &str, success: bool, summary: &str) {
        let mut event = AgentEvent::SubagentCompleted {
            task_id: task_id.to_string(),
            success,
            summary: truncate_str(summary, 500),
        };
        self.event_bus.dispatch(&self.ctx, &mut event).await;
    }
}

/// Free async function that runs a subagent task in the background.
/// This is separate from `AgentRuntime` methods to break the recursive async type
/// chain that would otherwise prevent the future from being `Send`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_subagent_task(
    config: Config,
    paths: Paths,
    provider_pool: Arc<ProviderPool>,
    task_manager: TaskManager,
    outbound_tx: Option<mpsc::Sender<OutboundMessage>>,
    task_str: String,
    task_id: String,
    label: String,
    origin_channel: String,
    origin_chat_id: String,
    origin_account_id: Option<String>,
    agent_id: Option<String>,
    event_tx: Option<broadcast::Sender<String>>,
    origin_history_seed: Vec<ChatMessage>,
    origin_session_key: String,
    event_emitter: EventEmitterHandle,
    agent_type: Option<String>,
    abort_token: Option<AbortToken>,
    reporter: Option<SubagentCompletionReporter>,
    // 发起会话的预算 tracker；挂到子运行时后，子消耗归并到父预算。
    parent_budget: Option<BudgetTrackerHandle>,
) {
    // Create the task entry and mark it running atomically.
    // This eliminates the race condition where a concurrent cleanup could
    // remove the task between create_task and set_running.
    task_manager
        .create_and_start_task_with_route(
            &task_id,
            &label,
            &task_str,
            &origin_channel,
            &origin_chat_id,
            origin_account_id.as_deref(),
            Some(&origin_session_key),
            agent_id.as_deref(),
            true,
            agent_type.as_deref(), // agent_type
            false,                 // one_shot
        )
        .await;
    task_manager.set_progress(&task_id, "Processing...").await;

    // 发送开始进度
    task_manager
        .send_progress(crate::agent_progress::AgentProgress::Delta {
            task_id: task_id.clone(),
            tokens_added: 0,
            tools_added: 0,
            total_tokens: 0,
            total_tools: 0,
        })
        .await;

    // Create isolated runtime with restricted tools
    let tool_registry = AgentRuntime::subagent_tool_registry();
    let paths_for_persist = paths.clone();
    let learning_config = config.clone();
    let learning_paths = paths.clone();
    let mut sub_runtime = match AgentRuntime::new(config, paths, provider_pool, tool_registry) {
        Ok(r) => r,
        Err(e) => {
            task_manager.set_failed(&task_id, &format!("{}", e)).await;
            if let Some(reporter) = &reporter {
                reporter.report(&task_id, false, &e.to_string()).await;
            }
            return;
        }
    };
    sub_runtime.set_task_manager(task_manager.clone());
    sub_runtime.set_agent_id(agent_id.clone());
    // 子 Agent 的 LLM 消耗归并到发起会话的总预算，避免父预算看不到子消耗。
    sub_runtime.set_parent_budget(parent_budget);
    sub_runtime.set_event_emitter(event_emitter);
    if let Some(tx) = event_tx.clone() {
        sub_runtime.event_tx = Some(tx);
    }
    if let Some(tx) = outbound_tx.clone() {
        sub_runtime.outbound_tx = Some(tx);
    }
    let cancellation_token = abort_token.clone();
    if let Some(at) = abort_token {
        sub_runtime.abort_token = at.clone();
        // Register the AbortToken with the task manager so that /tasks cancel
        // can trigger chain cancellation. Without this registration, cancelling
        // a task via TaskManager would not propagate to the subagent runtime.
        task_manager.register_abort_token(&task_id, at);
    }
    if let Err(e) = sub_runtime.init_memory_file_store() {
        tracing::warn!(error = %e, "Failed to initialize subagent file memory store");
    }
    if let Err(e) = sub_runtime.init_skill_file_store() {
        tracing::warn!(error = %e, "Failed to initialize subagent skill file store");
    }

    // Create a unique session key for this subagent
    let session_key = format!("subagent:{}", task_id);
    if !origin_history_seed.is_empty() {
        if let Err(e) = sub_runtime
            .session_store
            .save(&session_key, &origin_history_seed)
        {
            warn!(task_id = %task_id, error = %e, "Failed to seed subagent session history");
        }
    }

    let mut subagent_metadata = build_subagent_metadata(agent_id.as_deref());
    if !subagent_metadata.is_object() {
        subagent_metadata = serde_json::json!({});
    }
    if let Some(obj) = subagent_metadata.as_object_mut() {
        obj.insert(
            "origin_channel".to_string(),
            serde_json::json!(origin_channel.clone()),
        );
        obj.insert(
            "origin_chat_id".to_string(),
            serde_json::json!(origin_chat_id.clone()),
        );
    }

    let inbound = build_subagent_inbound_message(
        &task_str,
        &origin_channel,
        &origin_chat_id,
        &subagent_metadata,
        &session_key,
    );
    let result = if let Some(token) = cancellation_token {
        tokio::select! {
            biased;
            _ = token.cancelled() => {
                info!(task_id = %task_id, "Subagent stopped after cancellation");
                // 父任务取消也必须写入终态，不能只注销 token 后留下 Running 记录。
                if let Err(e) = task_manager
                    .set_cancelled(&task_id, "父任务已取消")
                    .await
                {
                    warn!(task_id = %task_id, error = %e, "Failed to finalize cancelled subagent task");
                }
                if let Some(reporter) = &reporter {
                    reporter.report(&task_id, false, "cancelled").await;
                }
                return;
            }
            result = sub_runtime.process_message(inbound) => result,
        }
    } else {
        sub_runtime.process_message(inbound).await
    };

    match result {
        Ok(result) => {
            task_manager.set_completed(&task_id, &result).await;
            info!(task_id = %task_id, label = %label, "Subagent completed");
            if let Some(reporter) = &reporter {
                reporter.report(&task_id, true, &result).await;
            }

            if let Err(err) = capture_delegation_end_learning_boundary_with_config(
                &learning_config,
                &learning_paths,
                &origin_channel,
                &origin_chat_id,
                Some(&task_id),
                &task_str,
                &result,
                true,
            ) {
                warn!(
                    task_id = %task_id,
                    error = %err,
                    "Failed to persist delegation-end ghost learning episode"
                );
            }
            if let Some(manager) = sub_runtime.ghost_memory_lifecycle.as_ref() {
                manager.on_delegation(&task_str, &result, &session_key);
            }

            deliver_subagent_result_to_origin(
                &origin_channel,
                &origin_chat_id,
                &result,
                &task_id,
                agent_id.as_deref(),
                outbound_tx.clone(),
                event_tx.clone(),
                Some(&SessionStore::new(paths_for_persist.clone())),
                Some(&origin_session_key),
            )
            .await;
        }
        Err(e) => {
            let err_msg = format!("{}", e);
            task_manager.set_failed(&task_id, &err_msg).await;
            error!(task_id = %task_id, error = %e, "Subagent failed");
            if let Some(reporter) = &reporter {
                reporter.report(&task_id, false, &err_msg).await;
            }

            // 失败的 delegation 也是重要的学习机会
            if let Err(ghost_err) = capture_delegation_end_learning_boundary_with_config(
                &learning_config,
                &learning_paths,
                &origin_channel,
                &origin_chat_id,
                Some(&task_id),
                &task_str,
                &err_msg,
                false,
            ) {
                warn!(
                    task_id = %task_id,
                    error = %ghost_err,
                    "Failed to persist delegation-end ghost learning episode (failure case)"
                );
            }

            let short_id = truncate_str(&task_id, 8);
            let failure_message = format!(
                "\n❌ 后台任务失败: **{}** (ID: {})\n错误: {}",
                label, short_id, err_msg
            );
            deliver_subagent_result_to_origin(
                &origin_channel,
                &origin_chat_id,
                &failure_message,
                &task_id,
                agent_id.as_deref(),
                outbound_tx.clone(),
                event_tx.clone(),
                Some(&SessionStore::new(paths_for_persist.clone())),
                Some(&origin_session_key),
            )
            .await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(unused_variables)]
pub(crate) async fn deliver_subagent_result_to_origin(
    origin_channel: &str,
    origin_chat_id: &str,
    content: &str,
    task_id: &str,
    agent_id: Option<&str>,
    outbound_tx: Option<mpsc::Sender<OutboundMessage>>,
    event_tx: Option<broadcast::Sender<String>>,
    session_store: Option<&SessionStore>,
    session_key: Option<&str>,
) {
    // 将子agent结果持久化到 SessionStore，使 WebUI 恢复会话时能看到
    if let (Some(store), Some(key)) = (session_store, session_key) {
        use blockcell_core::types::ChatMessage;
        let msg = ChatMessage::assistant(content);
        if let Err(e) = store.append(key, &msg) {
            tracing::warn!(error = %e, task_id = %task_id, "Failed to persist subagent result to session store");
        }
    }

    // ws 渠道：发送 message_done 事件（带 background_delivery 标记）
    // WebUI 需要此事件来显示后台任务完成结果
    // cli/internal 渠道不需要独立事件，主agent会整合结果
    if origin_channel == "ws" {
        if let Some(tx) = event_tx {
            let event = serde_json::json!({
                "type": "message_done",
                "channel": "ws",
                "chat_id": origin_chat_id,
                "content": content,
                "is_markdown": true,
                "background_delivery": true,
                "task_id": task_id,
                "agent_id": agent_id.unwrap_or(""),
            });
            let _ = tx.send(event.to_string());
        }
        return;
    }

    if origin_channel == "cli" || origin_channel == "internal" {
        return;
    }

    if let Some(tx) = outbound_tx {
        let notification = OutboundMessage::new(origin_channel, origin_chat_id, content);
        let _ = tx.send(notification).await;
    }
}

pub(crate) fn append_ephemeral_context_to_latest_user_message<'a>(
    messages: &'a [ChatMessage],
    context_block: Option<&str>,
) -> std::borrow::Cow<'a, [ChatMessage]> {
    let Some(context_block) = context_block
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        // No ephemeral context to inject (the common case): borrow the existing
        // slice instead of deep-cloning the entire history on every LLM call.
        return std::borrow::Cow::Borrowed(messages);
    };
    let mut api_messages = messages.to_vec();
    if let Some(message) = api_messages
        .iter_mut()
        .rev()
        .find(|message| message.role == "user")
    {
        let base = chat_message_text(message);
        *message = ChatMessage::user(&format!("{}\n\n{}", base, context_block));
    }
    std::borrow::Cow::Owned(api_messages)
}

/// Build outbound metadata containing reply-to information from an inbound message.
/// Only applies to group chats — single/DM chats return Null so no quoting is added.
pub(crate) fn extract_reply_metadata(msg: &InboundMessage) -> serde_json::Value {
    match msg.channel.as_str() {
        "telegram" => {
            // Telegram group/supergroup chat_ids are negative integers
            let is_group = msg.chat_id.parse::<i64>().unwrap_or(0) < 0;
            if is_group {
                if let Some(mid) = msg.metadata.get("message_id") {
                    return serde_json::json!({ "reply_to_message_id": mid });
                }
            }
            serde_json::Value::Null
        }
        "feishu" | "lark" => {
            // Use chat_type from metadata: "group" = group chat, "p2p" = direct message
            let is_group = msg.metadata.get("chat_type").and_then(|v| v.as_str()) == Some("group");
            if is_group {
                if let Some(mid) = msg.metadata.get("message_id").and_then(|v| v.as_str()) {
                    return serde_json::json!({ "reply_to_message_id": mid });
                }
            }
            serde_json::Value::Null
        }
        "discord" => {
            // Discord server messages carry a non-empty guild_id; DMs do not
            let in_guild = msg
                .metadata
                .get("guild_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .is_some();
            if in_guild {
                if let Some(mid) = msg.metadata.get("message_id").and_then(|v| v.as_str()) {
                    return serde_json::json!({ "reply_to_message_id": mid });
                }
            }
            serde_json::Value::Null
        }
        "slack" => {
            // Slack DM channel IDs start with 'D'; public/private channels start with 'C'/'G'
            let is_dm = msg.chat_id.starts_with('D');
            if !is_dm {
                if let Some(ts) = msg.metadata.get("ts").and_then(|v| v.as_str()) {
                    return serde_json::json!({ "thread_ts": ts });
                }
            }
            serde_json::Value::Null
        }
        "dingtalk" => {
            // DingTalk group chats have conversation_type "2"
            let is_group = msg
                .metadata
                .get("conversation_type")
                .and_then(|v| v.as_str())
                == Some("2");
            if is_group {
                if let Some(mid) = msg.metadata.get("msg_id").and_then(|v| v.as_str()) {
                    return serde_json::json!({ "reply_to_message_id": mid });
                }
            }
            serde_json::Value::Null
        }
        _ => serde_json::Value::Null,
    }
}
