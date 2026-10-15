use super::*;

pub(crate) async fn supervise_message_task(
    handle: tokio::task::JoinHandle<()>,
    task_manager: TaskManager,
    task_id: String,
    completion_receipt_id: Option<String>,
    task_done_tx: mpsc::UnboundedSender<(String, ActiveConversationKey)>,
    conversation_key: ActiveConversationKey,
) {
    match handle.await {
        Ok(()) => {}
        Err(error) if error.is_panic() => {
            let message = format!("message task panicked: {error}");
            error!(task_id = %task_id, error = %error, "Message task panicked");
            task_manager.set_failed(&task_id, &message).await;
            if let Some(receipt_id) = completion_receipt_id.as_deref() {
                blockcell_core::message_receipt::complete_message_receipt(receipt_id, Err(message));
            }
        }
        Err(error) => {
            debug!(task_id = %task_id, error = %error, "Message task was cancelled");
            if let Some(receipt_id) = completion_receipt_id.as_deref() {
                blockcell_core::message_receipt::cancel_message_receipt(receipt_id);
            }
        }
    }

    let _ = task_done_tx.send((task_id, conversation_key));
}

/// Extract the first JSON object from potentially markdown-wrapped LLM output.
/// Handles ```json...```, ```...```, `<tool_call>` XML with `<parameter=argv>`,
/// bare `{...}` objects, and bare `[...]` arrays (wrapped as `{"argv":[...]}`).
#[allow(dead_code)]
pub(crate) fn extract_json_from_text(text: &str) -> String {
    // Try ```json ... ``` blocks first
    if let Some(start) = text.find("```json") {
        let after = &text[start + 7..];
        if let Some(end) = after.find("```") {
            return after[..end].trim().to_string();
        }
    }
    // Try ``` ... ``` blocks containing an object or array
    if let Some(start) = text.find("```") {
        let after = &text[start + 3..];
        if let Some(end) = after.find("```") {
            let candidate = after[..end].trim();
            if candidate.starts_with('{') || candidate.starts_with('[') {
                if candidate.starts_with('[') {
                    return format!("{{\"argv\": {}}}", candidate);
                }
                return candidate.to_string();
            }
        }
    }
    // Handle <tool_call> XML: extract argv from <parameter=argv>...</parameter>
    if text.contains("<parameter=argv>") {
        if let Some(start) = text.find("<parameter=argv>") {
            let after = &text[start + 16..];
            let end_tag = after.find("</parameter>").unwrap_or(after.len());
            let content = after[..end_tag].trim();
            if content.starts_with('[') {
                return format!("{{\"argv\": {}}}", content);
            }
            if content.starts_with('{') {
                return content.to_string();
            }
        }
    }
    // Fall back to first { ... } span
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end >= start {
                return text[start..=end].to_string();
            }
        }
    }
    // Handle bare JSON arrays (wrap as {"argv": [...]})
    if let Some(start) = text.find('[') {
        if let Some(end) = text.rfind(']') {
            if end >= start {
                return format!("{{\"argv\": {}}}", &text[start..=end]);
            }
        }
    }
    text.trim().to_string()
}

#[allow(dead_code)]
pub(crate) fn build_script_skill_summary_prompt(
    user_question: &str,
    skill_name: &str,
    method_name: &str,
    skill_md: &str,
    script_output: &str,
) -> String {
    crate::skill_summary::SkillSummaryFormatter::build_prompt(
        user_question,
        skill_name,
        Some(method_name),
        skill_md,
        script_output,
    )
}

/// Free async function that runs a user message in the background.
/// Uses a lightweight message-scoped runtime forked from the parent AgentRuntime
/// so shared caches and wired capabilities are preserved without per-message disk rebuilds.
pub(crate) async fn run_message_task(
    mut runtime: AgentRuntime,
    msg: InboundMessage,
    task_id: String,
) {
    // 注意：任务已通过 create_and_start_task 标记为 Running，无需再调用 set_running
    let checkpoint_manager = crate::checkpoint::CheckpointManager::new(&runtime.paths.workspace());
    let task_manager = runtime.task_manager.clone();
    let agent_id = runtime.agent_id.clone();
    let event_tx = runtime.event_tx.clone();
    let outbound_tx = runtime.outbound_tx.clone();
    let resumed_task_id = msg
        .metadata
        .get("resumed_task_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let completion_receipt_id = msg
        .metadata
        .get(blockcell_core::message_receipt::MESSAGE_RECEIPT_ID)
        .and_then(|value| value.as_str())
        .map(str::to_string);

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

    let error_msg = msg.clone();

    match runtime.process_message(msg).await {
        Ok(response) => {
            debug!(task_id = %task_id, response_len = response.len(), "Message task completed");
            // Mark message tasks as completed so they appear in /tasks.
            // The periodic cleanup loop will evict them after the grace period.
            // This way users can see recently completed tasks via /tasks.
            task_manager.set_completed(&task_id, &response).await;
            if let Err(e) = checkpoint_manager.mark_completed(&task_id) {
                warn!(task_id = %task_id, error = %e, "Failed to mark message checkpoint completed");
            }
            if let Some(resumed_task_id) = resumed_task_id.as_deref() {
                if let Err(e) = checkpoint_manager.mark_completed(resumed_task_id) {
                    warn!(task_id = %resumed_task_id, error = %e, "Failed to mark resumed checkpoint completed");
                }
            }
            if let Some(receipt_id) = completion_receipt_id.as_deref() {
                blockcell_core::message_receipt::complete_message_receipt(receipt_id, Ok(()));
            }
        }
        Err(e) => {
            let err_msg = format!("{}", e);
            error!(task_id = %task_id, error = %e, "Message task failed");
            deliver_message_task_failure(
                &error_msg,
                &task_id,
                agent_id.as_deref(),
                &err_msg,
                event_tx.as_ref(),
                outbound_tx.as_ref(),
            )
            .await;
            // Keep failed tasks briefly for visibility, then let tick cleanup handle them
            task_manager.set_failed(&task_id, &err_msg).await;
            if let Some(receipt_id) = completion_receipt_id.as_deref() {
                blockcell_core::message_receipt::complete_message_receipt(receipt_id, Err(err_msg));
            }
        }
    }
}

pub(super) async fn deliver_message_task_failure(
    msg: &InboundMessage,
    task_id: &str,
    agent_id: Option<&str>,
    err_msg: &str,
    event_tx: Option<&broadcast::Sender<String>>,
    outbound_tx: Option<&mpsc::Sender<OutboundMessage>>,
) {
    if let Some(event_tx) = event_tx {
        let _ = event_tx.send(
            serde_json::json!({
                "type": "error",
                "channel": msg.channel,
                "agent_id": agent_id.unwrap_or("default"),
                "chat_id": msg.chat_id,
                "task_id": task_id,
                "message": err_msg,
            })
            .to_string(),
        );
    }

    if !matches!(msg.channel.as_str(), "ws" | "cli" | "http" | "ghost") {
        if let Some(outbound_tx) = outbound_tx {
            let mut outbound =
                OutboundMessage::new(&msg.channel, &msg.chat_id, &format!("❌ {}", err_msg));
            outbound.account_id = msg.account_id.clone();
            let _ = outbound_tx.send(outbound).await;
        }
    }
}
