use super::*;
use futures::FutureExt;

impl AgentRuntime {
    pub(crate) async fn execute_runtime_tool_call(
        &self,
        tool_name: &str,
        ctx: blockcell_tools::ToolContext,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        if !self.tool_within_capability(tool_name, &arguments) {
            return Err(blockcell_core::Error::Tool(scoped_tool_denied_result(
                tool_name,
            )));
        }
        let fut = async {
            if let Some(manager) = self.ghost_memory_lifecycle.as_ref() {
                if manager.has_tool(tool_name) {
                    return manager.handle_tool_call(tool_name, arguments);
                }
            }
            self.tool_registry.execute(tool_name, ctx, arguments).await
        };
        // A panicking tool must not unwind the whole turn; convert it into a
        // tool error so the model can see and recover from it.
        std::panic::AssertUnwindSafe(fut)
            .catch_unwind()
            .await
            .unwrap_or_else(|payload| {
                let detail = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_string());
                tracing::error!(tool = tool_name, panic = %detail, "Tool execution panicked");
                Err(blockcell_core::Error::Tool(format!(
                    "tool {tool_name} panicked: {detail}"
                )))
            })
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
        let mut eval = self.tool_policy.evaluate(&ToolCallContext {
            tool_name,
            tool_args,
            channel: &msg.channel,
        });
        // Shell wrappers execute as `shell`; evaluate both the caller's alias
        // and the canonical/effective action after hooks. Keep the strictest
        // result and ask at most once, so neither naming surface bypasses policy.
        let is_shell = matches!(
            tool_name,
            "shell" | "shell_run" | "shell_poll" | "shell_input" | "shell_close"
        );
        if is_shell {
            let action_name = match tool_args
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("run")
            {
                "poll" => "shell_poll",
                "write_stdin" => "shell_input",
                "close" => "shell_close",
                _ => "shell_run",
            };
            let severity = |decision| match decision {
                ToolPolicyDecision::Allow => 0,
                ToolPolicyDecision::Ask => 1,
                ToolPolicyDecision::Deny => 2,
            };
            for name in ["shell", action_name] {
                let other = self.tool_policy.evaluate(&ToolCallContext {
                    tool_name: name,
                    tool_args,
                    channel: &msg.channel,
                });
                if severity(other.decision) > severity(eval.decision) {
                    eval = other;
                }
            }
        }
        self.audit_policy_decision(tool_name, &eval, msg).await;

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
                // Policy-level confirmation follows the same interactive and
                // explicit-text paths as the dangerous-operation gate. This
                // keeps non-interactive channels fail-closed while allowing a
                // caller to approve the exact tool arguments deliberately.
                if self
                    .approve_dangerous_operation(tool_name, tool_args, items, msg)
                    .await
                {
                    PolicyOutcome::ProceedConfirmed
                } else {
                    PolicyOutcome::Denied("用户拒绝了该操作".to_string())
                }
            }
        }
    }

    pub(crate) async fn audit_policy_decision(
        &self,
        tool_name: &str,
        eval: &PolicyEvalResult,
        msg: &InboundMessage,
    ) {
        let logger = std::sync::Arc::clone(&self.audit_logger);
        let session_key = msg.session_key();
        let tool_name_owned = tool_name.to_string();
        let decision = format!("{:?}", eval.decision);
        let matched_rule = eval.matched_rule.clone();
        let description = eval.description.clone();
        let simulated = eval.simulated;
        match tokio::task::spawn_blocking(move || {
            let mut logger = logger.lock().unwrap_or_else(|e| e.into_inner());
            logger.set_session_id(&session_key);
            logger.log_permission_decision(
                &tool_name_owned,
                decision,
                matched_rule,
                description,
                simulated,
                &session_key,
            )
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                warn!(tool = tool_name, error = %e, "Permission-decision audit write failed")
            }
            Err(e) => {
                warn!(tool = tool_name, error = %e, "Permission-decision audit task failed")
            }
        }
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
