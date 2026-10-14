use super::*;

impl AgentRuntime {
    pub(crate) async fn execute_runtime_tool_call(
        &self,
        tool_name: &str,
        ctx: blockcell_tools::ToolContext,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        if let Some(manager) = self.ghost_memory_lifecycle.as_ref() {
            if manager.has_tool(tool_name) {
                return manager.handle_tool_call(tool_name, arguments);
            }
        }
        self.tool_registry.execute(tool_name, ctx, arguments).await
    }

    /// Layer 1: 在截断前将大型工具结果持久化到磁盘。
    ///
    /// 如果持久化成功，返回 `<persisted-output>` 存根字符串；
    /// 如果失败则返回 `None`，由调用方进行内联截断。
    ///
    /// 路径格式：`.tool_results/{session_key}/{tool_id}_{call_uuid}/output.txt`
    /// 引入 `session_key` 和 `call_uuid` 避免 `text_call_0`/`ollama_call_0` 等
    /// 通用 ID 跨轮次、跨会话重复，导致旧会话 recall 拿到被覆盖的错误内容。
    ///
    /// `.tool_results/` 目录通过 maintenance tick 中的 `cleanup_tool_results` 定期清理
    ///（TTL 7 天 + 每会话上限 50 条目），长期运行不会无限累积磁盘占用。
    pub(crate) async fn try_persist_large_tool_result(
        &self,
        content: &str,
        tool_call_id: Option<&str>,
        session_key: &str,
        call_uuid: &str,
    ) -> Option<String> {
        let tool_id = sanitize_tool_use_id(tool_call_id.unwrap_or("unknown"));
        // 使用 sanitize_session_key 替代 sanitize_tool_use_id，防止不同会话映射到同一目录
        // （sanitize_tool_use_id 会删除分隔符，导致 "a.b" 和 "a-b" 冲突）
        let session_id = sanitize_session_key(session_key);
        let dir_name = format!("{}_{call_uuid}", tool_id);
        let persistence_dir = self
            .paths
            .workspace()
            .join(".tool_results")
            .join(&session_id)
            .join(&dir_name);
        let output_file = persistence_dir.join("output.txt");

        match tokio::fs::create_dir_all(&persistence_dir).await {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(
                    tool_id = %tool_id,
                    error = %e,
                    "[layer1] Failed to create tool result persistence directory"
                );
                return None;
            }
        }

        match tokio::fs::write(&output_file, content).await {
            Ok(()) => {
                let byte_size = content.len();
                let char_count = content.chars().count();
                // 使用 Layer1 配置的预览大小（以字符为单位，默认 2000），
                // 按 2/3 头部 + 1/3 尾部分配，与 fallback 截断路径保持一致
                let preview_size = self.response_cache.preview_size_chars();
                let head_size = preview_size * 2 / 3;
                let tail_size = preview_size - head_size;
                let head: String = content.chars().take(head_size).collect();
                let tail: String = content
                    .chars()
                    .rev()
                    .take(tail_size)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                let trimmed_chars = char_count.saturating_sub(preview_size);
                // 包含完整 dir_name（含 UUID）作为精确引用 ID，
                // session_recall 通过 tool: 前缀识别工具结果 ID，
                // 格式 tool:{tool_id}:{call_uuid} 支持 UUID 精确匹配
                let recall_id = format!(
                    "tool:{tool_id}:{call_uuid}",
                    tool_id = tool_id,
                    call_uuid = call_uuid
                );
                let stub = format!(
                    "<persisted-output>\n\
                     tool_use_id: {tool_id}\n\
                     recall_id: {recall_id}\n\
                     file: {output_path}\n\
                     size: {byte_size} bytes ({char_count} chars)\n\
                     \n\
                     {head}\n\
                     ...<trimmed {trimmed_chars} chars>...\n\
                     {tail}\n\
                     \n\
                     </persisted-output>",
                    tool_id = tool_id,
                    recall_id = recall_id,
                    output_path = output_file.display(),
                    byte_size = byte_size,
                    char_count = char_count,
                    head = head,
                    trimmed_chars = trimmed_chars,
                    tail = tail,
                );
                // Record metrics event — 使用真实的 session_key、preview 大小和 truncated 标志
                let filepath_display = output_file.display().to_string();
                let preview_size = head.len() + tail.len();
                memory_event!(
                    layer1,
                    persisted,
                    tool_id,
                    byte_size as u64,
                    preview_size as u64,
                    filepath_display.as_str(),
                    session_key,
                    true // 工具输出已被截断替换为 preview
                );
                memory_event!(layer1, preview_generated, tool_id, stub.len() as u64);
                tracing::info!(
                    tool_id = %tool_id,
                    path = %output_file.display(),
                    byte_size = byte_size,
                    "[layer1] Persisted large tool result to disk"
                );
                Some(stub)
            }
            Err(e) => {
                tracing::warn!(
                    tool_id = %tool_id,
                    error = %e,
                    "[layer1] Failed to persist large tool result"
                );
                None
            }
        }
    }

    pub(crate) async fn check_tool_policy(
        &mut self,
        tool_name: &str,
        tool_args: &serde_json::Value,
        msg: &InboundMessage,
    ) -> PolicyOutcome {
        let eval = self.tool_policy.evaluate(&ToolCallContext {
            tool_name,
            tool_args,
            channel: &msg.channel,
        });
        self.audit_policy_decision(tool_name, &eval, msg);

        match eval.decision {
            ToolPolicyDecision::Allow => PolicyOutcome::Proceed,
            ToolPolicyDecision::Deny => {
                let reason = eval.description.unwrap_or_else(|| {
                    format!(
                        "Policy denied tool call by rule: {}",
                        eval.matched_rule.as_deref().unwrap_or("default")
                    )
                });
                PolicyOutcome::Denied(reason)
            }
            ToolPolicyDecision::Ask => {
                let items = vec![format!("{}: {}", tool_name, summarize_tool_args(tool_args))];
                if self
                    .confirm_dangerous_operation(tool_name, items, msg)
                    .await
                {
                    PolicyOutcome::ProceedConfirmed
                } else {
                    PolicyOutcome::Denied("用户拒绝了该操作".to_string())
                }
            }
        }
    }

    pub(crate) fn audit_policy_decision(
        &mut self,
        tool_name: &str,
        eval: &PolicyEvalResult,
        msg: &InboundMessage,
    ) {
        self.audit_logger.set_session_id(&msg.session_key());
        let _ = self.audit_logger.log_permission_decision(
            tool_name,
            format!("{:?}", eval.decision),
            eval.matched_rule.clone(),
            eval.description.clone(),
            eval.simulated,
            &msg.session_key(),
        );
    }
}

pub(super) fn shell_action_is_run(arguments: &serde_json::Value) -> bool {
    arguments
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or("run")
        == "run"
}

pub(super) fn shell_sandbox_escalation_reason(
    result: &blockcell_core::Result<serde_json::Value>,
) -> Option<String> {
    match result {
        Err(blockcell_core::Error::PermissionDenied(message))
            if message.contains("sandbox") && message.contains("approval required") =>
        {
            Some(message.clone())
        }
        Ok(value)
            if value.get("sandbox_policy").and_then(|item| item.as_str())
                != Some("full-access")
                && value
                    .get("exit_code")
                    .and_then(|item| item.as_i64())
                    .is_some_and(|code| code != 0) =>
        {
            let output = value
                .get("output")
                .and_then(|item| item.as_str())
                .unwrap_or_default();
            let lower = output.to_ascii_lowercase();
            [
                "operation not permitted",
                "permission denied",
                "network is unreachable",
                "sandbox violation",
            ]
            .iter()
            .find(|marker| lower.contains(**marker))
            .map(|marker| (*marker).to_string())
        }
        _ => None,
    }
}

pub(super) fn full_access_retry_args(arguments: &serde_json::Value) -> serde_json::Value {
    let mut retry = arguments.clone();
    if let Some(object) = retry.as_object_mut() {
        object.remove("session_id");
        object.insert(
            "sandbox_policy".to_string(),
            serde_json::Value::String("full-access".to_string()),
        );
    }
    retry
}
