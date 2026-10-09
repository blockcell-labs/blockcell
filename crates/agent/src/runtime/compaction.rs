use super::*;

fn select_recent_messages(
    messages: &[ChatMessage],
    keep_recent_messages: usize,
) -> Vec<ChatMessage> {
    if keep_recent_messages == 0 || messages.is_empty() {
        return Vec::new();
    }

    let mut start = messages.len().saturating_sub(keep_recent_messages);

    // A retained suffix must not begin in the middle of a tool-result group.
    while start > 0 && messages[start].role == "tool" {
        start -= 1;
    }

    // If the boundary lands on a tool-calling assistant, retain it only when
    // every declared result is present consecutively. Otherwise skip the
    // incomplete group so the rebuilt provider request remains valid.
    if messages[start].role == "assistant" {
        if let Some(tool_calls) = &messages[start].tool_calls {
            if !tool_calls.is_empty() {
                let expected: std::collections::HashSet<&str> =
                    tool_calls.iter().map(|call| call.id.as_str()).collect();
                let mut found = std::collections::HashSet::new();
                let mut end = start + 1;
                while end < messages.len() && messages[end].role == "tool" {
                    if let Some(id) = messages[end].tool_call_id.as_deref() {
                        found.insert(id);
                    }
                    end += 1;
                }
                if !expected.iter().all(|id| found.contains(id)) {
                    start = end;
                }
            }
        }
    }

    while start < messages.len() && messages[start].role == "tool" {
        start += 1;
    }

    messages[start..].to_vec()
}

impl AgentRuntime {
    /// Build an extractive summary from session history (no LLM call).
    /// Extracts user questions and final assistant answers, truncated to fit.
    pub(crate) fn build_extractive_summary(history: &[ChatMessage]) -> String {
        let mut summary_parts: Vec<String> = Vec::new();
        let mut i = 0;
        while i < history.len() {
            let msg = &history[i];
            if msg.role == "user" {
                let user_text = match &msg.content {
                    serde_json::Value::String(s) => {
                        let chars: String = s.chars().take(100).collect();
                        if s.chars().count() > 100 {
                            format!("{}...", chars)
                        } else {
                            chars
                        }
                    }
                    _ => "(media)".to_string(),
                };
                // Find the last assistant text reply in this round
                let mut assistant_text = String::new();
                let mut j = i + 1;
                while j < history.len() && history[j].role != "user" {
                    if history[j].role == "assistant" && history[j].tool_calls.is_none() {
                        assistant_text = match &history[j].content {
                            serde_json::Value::String(s) => {
                                let chars: String = s.chars().take(150).collect();
                                if s.chars().count() > 150 {
                                    format!("{}...", chars)
                                } else {
                                    chars
                                }
                            }
                            _ => String::new(),
                        };
                    }
                    j += 1;
                }
                if !assistant_text.is_empty() {
                    summary_parts.push(format!("Q: {} → A: {}", user_text, assistant_text));
                } else {
                    summary_parts.push(format!("Q: {} → (tool interaction)", user_text));
                }
                i = j;
            } else {
                i += 1;
            }
        }

        // Cap total summary length
        let mut summary = summary_parts.join("\n");
        if summary.chars().count() > 800 {
            // Keep only the most recent entries
            while summary.chars().count() > 800 && summary_parts.len() > 1 {
                summary_parts.remove(0);
                summary = summary_parts.join("\n");
            }
        }
        summary
    }

    /// Replace `messages` in place with the compacted form: a system summary,
    /// a continuation user turn, then the retained recent messages.
    pub(crate) fn rebuild_messages_after_compact(
        messages: &mut Vec<ChatMessage>,
        compact: &crate::compact::CompactResult,
    ) {
        messages.clear();
        messages.push(ChatMessage::system(&compact.to_compact_message()));
        messages.push(ChatMessage::user("请继续当前任务。"));
        messages.extend(compact.recent_messages.iter().cloned());
    }

    /// Clear file/skill trackers and reset incremental memory baselines after a
    /// successful compact, so the next compact does not re-restore the same
    /// context and memory extraction keeps triggering.
    pub(crate) fn finalize_trackers_after_compact(&mut self, history: &[ChatMessage]) {
        if let Some(ms) = self.memory_system.as_mut() {
            ms.file_tracker_mut().clear();
            ms.skill_tracker_mut().clear();
            ms.reset_baselines_after_compact(history);
        }
    }

    /// Run a Layer-4 compact and, on success, replace both the in-flight LLM
    /// message list and the persisted history with the compacted form, saving
    /// the new history to the session store. Returns whether the compact
    /// succeeded. `phase` is used only for log context (e.g. "pre-loop").
    pub(crate) async fn apply_layer4_compact_in_loop(
        &mut self,
        current_messages: &mut Vec<ChatMessage>,
        history: &mut Vec<ChatMessage>,
        msg: &InboundMessage,
        persist_session_key: &str,
        phase: &str,
    ) -> bool {
        let compact_ctx = CompactContext {
            channel: &msg.channel,
            chat_id: &msg.chat_id,
            account_id: msg.account_id.as_deref(),
        };
        if let Err(e) = self
            .capture_pre_compress_learning_boundary(persist_session_key, current_messages)
            .await
        {
            warn!(error = %e, session_key = %persist_session_key, "Ghost learning pre-compress capture failed");
        }
        let compact_result = self
            .execute_layer4_compact(
                current_messages,
                persist_session_key,
                Some(compact_ctx),
                true,
            )
            .await;
        if !compact_result.success {
            warn!(error = ?compact_result.error, phase, "[layer4] Compact failed, continuing without compression");
            return false;
        }
        let messages_before = current_messages.len();
        Self::rebuild_messages_after_compact(current_messages, &compact_result);
        self.dispatch_event(
            msg,
            persist_session_key,
            &mut AgentEvent::Compaction {
                phase: phase.to_string(),
                messages_before,
                messages_after: current_messages.len(),
            },
        )
        .await;
        info!(
            phase,
            post_compact_tokens = estimate_messages_tokens(current_messages),
            "[layer4] Compact completed, messages replaced with summary"
        );
        Self::rebuild_messages_after_compact(history, &compact_result);
        if let Err(e) = self.session_store.save(persist_session_key, history) {
            warn!(error = %e, phase, "[layer4] 无法保存 compacted history");
        }
        self.finalize_trackers_after_compact(history);
        self.context_builder
            .clear_injected_retrieval_for_session(persist_session_key);
        true
    }

    /// Execute Layer 4 Full Compact - LLM 语义压缩
    ///
    /// 当 token 超过预算阈值时，使用 LLM 生成 9-part structured summary，
    /// 并收集恢复信息（文件、技能、Session Memory）。
    ///
    /// ## 参数
    /// - `messages` - 要压缩的消息列表
    /// - `_session_key` - 会话标识符
    /// - `compact_ctx` - 可选的通知上下文，用于发送用户通知
    ///
    /// ## 返回
    /// - `CompactResult` - 压缩结果（通过 `success` 字段判断是否成功）
    ///   - 成功：`success: true`，包含摘要和恢复消息
    ///   - 失败：`success: false`，`error` 字段包含错误信息
    pub(crate) async fn execute_layer4_compact(
        &self,
        messages: &[ChatMessage],
        _session_key: &str,
        compact_ctx: Option<CompactContext<'_>>,
        is_auto: bool,
    ) -> crate::compact::CompactResult {
        use crate::compact::{generate_compact_summary, CompactResult};
        use crate::session_memory::get_session_memory_path;
        use crate::session_metrics::get_compact_circuit_breaker;

        let pre_compact_tokens = estimate_messages_tokens(messages);
        let keep_recent_messages = self
            .memory_system
            .as_ref()
            .map(|m| m.config().layer4.keep_recent_messages)
            .unwrap_or(2);
        let recent_messages = select_recent_messages(messages, keep_recent_messages);

        // ========== 0. Pre-Compact Hooks ==========
        // 允许注册的 hooks 在压缩前执行自定义逻辑（取消、延迟等）
        if let Some(ms) = self.memory_system.as_ref() {
            if ms.compact_hooks().has_pre_hooks() {
                let token_budget = ms.config().token_budget;
                let pre_ctx = crate::compact::PreCompactContext {
                    session_id: _session_key.to_string(),
                    current_tokens: pre_compact_tokens,
                    budget_tokens: token_budget,
                    has_pending_background_tasks: ms.has_pending_extraction(),
                };
                match ms.compact_hooks().execute_pre_hooks(pre_ctx).await {
                    crate::compact::PreCompactResult::Cancel => {
                        info!("[layer4] Compact cancelled by pre-hook");
                        return CompactResult::failed("Cancelled by pre-hook");
                    }
                    crate::compact::PreCompactResult::Delay(duration) => {
                        info!(
                            delay_ms = duration.as_millis() as u64,
                            "[layer4] Compact delayed by pre-hook"
                        );
                        tokio::time::sleep(duration).await;
                    }
                    crate::compact::PreCompactResult::Continue => {}
                }
            }
        }

        // ========== 1. 熔断器检查 ==========
        let circuit_breaker = get_compact_circuit_breaker();
        if !circuit_breaker.allow() {
            warn!(
                target: "blockcell.session_metrics.layer4",
                "[layer4] Compact skipped - circuit breaker OPEN"
            );
            return CompactResult::failed("Circuit breaker open - too many recent failures");
        }

        // ========== 2. 发送压缩开始通知 ==========
        if let (Some(ref tx), Some(ref ctx)) = (&self.outbound_tx, &compact_ctx) {
            let mut notification = OutboundMessage::new(
                ctx.channel,
                ctx.chat_id,
                "🔄 对话历史较长，正在压缩以保持性能...",
            );
            if let Some(aid) = ctx.account_id {
                notification.account_id = Some(aid.to_string());
            }
            let _ = tx.send(notification).await;
        }

        // ========== 3. 记录压缩开始事件 ==========
        let threshold = self
            .memory_system
            .as_ref()
            .map(|m| m.config().layer4.compact_threshold_ratio)
            .unwrap_or(0.8);
        crate::memory_event!(
            layer4,
            compact_started,
            pre_compact_tokens,
            threshold,
            is_auto
        );

        info!(pre_compact_tokens, "[layer4] Starting full compact");

        // ========== 4. 生成系统提示 ==========
        let system_prompt = Arc::new(
            "你是一个对话摘要助手。请根据对话历史生成结构化摘要，保留关键信息用于后续继续工作。"
                .to_string(),
        );

        // ========== 5. 获取模型配置 ==========
        let model = self.config.agents.defaults.model.clone();

        // ========== 6. 执行 LLM 语义压缩 ==========
        let max_output_tokens = self
            .memory_system
            .as_ref()
            .map(|m| m.config().layer4.max_output_tokens as u32)
            .unwrap_or(12_000);
        let summary_result = generate_compact_summary(
            Arc::clone(&self.provider_pool),
            system_prompt,
            &model,
            messages.to_vec(),
            max_output_tokens,
        )
        .await;

        let (summary_message, cache_read_tokens, cache_creation_tokens) = match summary_result {
            Ok(result) => (
                result.summary.to_markdown(),
                result.cache_read_tokens,
                result.cache_creation_tokens,
            ),
            Err(e) => {
                let error_msg = format!("LLM compact summary generation failed: {}", e);
                warn!(error = %e, "[layer4] Failed to generate compact summary");

                // 记录失败事件和熔断器状态
                crate::memory_event!(layer4, compact_failed, &error_msg, pre_compact_tokens, 1);
                circuit_breaker.record_failure();

                // 发送失败通知
                if let (Some(ref tx), Some(ref ctx)) = (&self.outbound_tx, &compact_ctx) {
                    let mut notification = OutboundMessage::new(
                        ctx.channel,
                        ctx.chat_id,
                        "⚠️ 压缩失败，继续使用当前历史。",
                    );
                    if let Some(aid) = ctx.account_id {
                        notification.account_id = Some(aid.to_string());
                    }
                    let _ = tx.send(notification).await;
                }

                return CompactResult::failed(&error_msg);
            }
        };

        // ========== 7. 收集恢复信息 ==========
        // 先等待后台 Session Memory 提取完成，避免读取过时内容
        if let Some(memory_system) = self.memory_system.as_ref() {
            // 检查跨 runtime 可见的提取 pending 标记
            // 在 gateway/异步消息模式下，前一个 runtime 的提取可能仍在运行
            // 但其 extraction_started_at (Instant) 在当前 runtime 中为 None
            let has_cross_runtime_pending = memory_system.has_pending_extraction_marker();
            let extraction_started_at = memory_system.session_memory_state().extraction_started_at;

            // 如果有跨 runtime pending 标记且不是过期的，使用 marker 中存储的时间戳
            // 构造准确的 started_at，避免使用 Instant::now() 导致无意义超时
            let effective_started_at = if extraction_started_at.is_some() {
                extraction_started_at
            } else if has_cross_runtime_pending {
                let stale_threshold_ms =
                    memory_system.config().layer3.extraction_stale_threshold_ms;
                let marker_path = memory_system.session_dir().join(".extraction_pending");
                // 读取 marker 中存储的 Unix epoch 毫秒时间戳
                let marker_ts_ms = memory_system.read_extraction_marker_timestamp_ms();
                let is_stale = marker_ts_ms
                    .map(|ts_ms| {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        now_ms.saturating_sub(ts_ms) >= stale_threshold_ms
                    })
                    .unwrap_or(true);
                if is_stale {
                    // 标记已过期，清理并跳过等待
                    let _ = std::fs::remove_file(&marker_path);
                    None
                } else {
                    info!("[layer4] 检测到跨 runtime 提取 pending 标记，等待提取完成");
                    // 从 marker 时间戳恢复准确的 Instant，避免从 Instant::now() 开始导致的无意义超时
                    marker_ts_ms.and_then(|ts_ms| {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .ok()?
                            .as_millis() as u64;
                        let elapsed_ms = now_ms.saturating_sub(ts_ms);
                        Some(
                            std::time::Instant::now()
                                - std::time::Duration::from_millis(elapsed_ms),
                        )
                    })
                }
            } else {
                None
            };

            if effective_started_at.is_some() {
                let wait_timeout_ms = memory_system.config().layer3.extraction_wait_timeout_ms;
                let stale_threshold_ms =
                    memory_system.config().layer3.extraction_stale_threshold_ms;
                let session_memory_path = get_session_memory_path(
                    memory_system.workspace_dir(),
                    memory_system.session_id(),
                );
                match crate::session_memory::recovery::wait_for_session_memory_extraction_with_timeout(
                    &session_memory_path,
                    effective_started_at,
                    wait_timeout_ms,
                    stale_threshold_ms,
                )
                .await
                {
                    Ok(_) => info!("[layer4] Session Memory 提取已完成，继续压缩"),
                    Err(e) => warn!(
                        error = %e,
                        "[layer4] 等待 Session Memory 提取超时，使用当前内容继续压缩"
                    ),
                }
            }
        }

        let mut recovery_message = if let Some(memory_system) = self.memory_system.as_ref() {
            let session_memory_path =
                get_session_memory_path(memory_system.workspace_dir(), memory_system.session_id());
            let session_memory_content =
                if tokio::fs::try_exists(&session_memory_path).await.ok() == Some(true) {
                    tokio::fs::read_to_string(&session_memory_path).await.ok()
                } else {
                    None
                };

            memory_system.generate_compact_recovery(session_memory_content.as_deref())
        } else {
            String::new()
        };
        append_plan_recovery(&mut recovery_message, _session_key);

        // ========== 8. 构建 CompactResult（初始 token 估算） ==========
        let mut post_compact_tokens = estimate_messages_tokens(&[
            ChatMessage::system(&summary_message),
            ChatMessage::user(&recovery_message),
        ]);

        // ========== 9. Post-Compact Hooks ==========
        // 允许注册的 hooks 在压缩完成后执行恢复/清理逻辑
        // 注意：post-hooks 可能追加 recovery 内容并更新 post_compact_tokens，
        // 因此指标记录必须在 post-hooks 之后，确保 token 指标准确
        if let Some(ms) = self.memory_system.as_ref() {
            if ms.compact_hooks().has_post_hooks() {
                let session_memory_path = crate::session_memory::get_session_memory_path(
                    ms.workspace_dir(),
                    ms.session_id(),
                );
                let post_ctx = crate::compact::PostCompactContext {
                    session_id: _session_key.to_string(),
                    recovery_message: recovery_message.clone(),
                    session_memory_path: if session_memory_path.exists() {
                        Some(session_memory_path)
                    } else {
                        None
                    },
                };
                match ms.compact_hooks().execute_post_hooks(post_ctx).await {
                    crate::compact::PostCompactResult::NeedRecovery(hook_recovery) => {
                        warn!(
                            recovery_msg = %hook_recovery,
                            "[layer4] Post-compact hook requested recovery, 追加恢复消息"
                        );
                        // NeedRecovery 语义为"额外恢复"，应追加而非替换
                        // 保留 generate_compact_recovery() 收集的文件/技能/session recovery
                        if !hook_recovery.is_empty() {
                            if !recovery_message.is_empty() {
                                recovery_message.push_str("\n\n");
                            }
                            recovery_message.push_str(&hook_recovery);
                            // hook 注入后重新计算 token 数
                            post_compact_tokens = estimate_messages_tokens(&[
                                ChatMessage::system(&summary_message),
                                ChatMessage::user(&recovery_message),
                            ]);
                        }
                    }
                    crate::compact::PostCompactResult::Success => {}
                }
            }
        }

        // ========== 10. 记录成功事件（post-hooks 之后，token 指标准确） ==========
        // 使用来自 LLM API 响应的真实 cache usage 数据
        crate::memory_event!(
            layer4,
            compact_completed,
            pre_compact_tokens,
            post_compact_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            is_auto
        );
        circuit_breaker.record_success();

        // 注意：如果 compact 路径中包含重试逻辑，应在重试时记录：
        //   crate::memory_event!(layer4, ptl_retry, retry_count);
        // 如果 token 预算从缓存中失效（需要重建缓存），应记录：
        //   crate::memory_event!(layer4, cache_break);

        info!(
            pre_compact_tokens,
            post_compact_tokens,
            compression_ratio = if pre_compact_tokens > 0 {
                (pre_compact_tokens - post_compact_tokens) as f64 / pre_compact_tokens as f64
            } else {
                0.0
            },
            "[layer4] Compact completed successfully"
        );

        // ========== 10. 发送压缩成功通知 ==========
        if let (Some(ref tx), Some(ref ctx)) = (&self.outbound_tx, &compact_ctx) {
            let notification_content = if pre_compact_tokens > 0 {
                let compression_ratio = (pre_compact_tokens - post_compact_tokens) as f64
                    / pre_compact_tokens as f64
                    * 100.0;
                format!(
                    "✅ 已压缩对话历史，保留关键信息。\n📊 Token: {} → {} (压缩 {:.0}%)",
                    pre_compact_tokens, post_compact_tokens, compression_ratio
                )
            } else {
                "✅ 压缩完成（无历史内容需要压缩）".to_string()
            };
            let mut notification =
                OutboundMessage::new(ctx.channel, ctx.chat_id, &notification_content);
            if let Some(aid) = ctx.account_id {
                notification.account_id = Some(aid.to_string());
            }
            let _ = tx.send(notification).await;
        }

        CompactResult {
            summary_message,
            recovery_message,
            pre_compact_tokens,
            post_compact_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            success: true,
            error: None,
            recent_messages,
        }
    }
}

fn append_plan_recovery(recovery_message: &mut String, session_key: &str) {
    let Some(plan) = blockcell_tools::plan::render_plan_for_recovery(session_key) else {
        return;
    };
    if !recovery_message.is_empty() {
        recovery_message.push_str("\n\n");
    }
    recovery_message.push_str(&plan);
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::types::ToolCallRequest;

    #[test]
    fn compact_recent_messages_preserve_complete_tool_group() {
        let tool_calls: Vec<ToolCallRequest> = (1..=3)
            .map(|index| ToolCallRequest {
                id: format!("call-{index}"),
                name: "read_file".to_string(),
                arguments: serde_json::json!({"path": format!("file-{index}")}),
                thought_signature: None,
            })
            .collect();
        let assistant = ChatMessage {
            id: None,
            role: "assistant".to_string(),
            content: serde_json::Value::String(String::new()),
            reasoning_content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            name: None,
        };
        let messages = vec![
            ChatMessage::user("inspect files"),
            assistant,
            ChatMessage::tool_result("call-1", "one"),
            ChatMessage::tool_result("call-2", "two"),
            ChatMessage::tool_result("call-3", "three"),
        ];

        let recent = select_recent_messages(&messages, 2);

        assert_eq!(recent.len(), 4);
        assert_eq!(recent[0].role, "assistant");
        assert_eq!(recent[1].tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(recent[3].tool_call_id.as_deref(), Some("call-3"));
    }

    #[test]
    fn compact_recovery_appends_complete_current_plan() {
        let session_key = format!("compact-plan-{}", uuid::Uuid::new_v4());
        blockcell_tools::plan::replace_plan_for_session(
            &session_key,
            &serde_json::json!({"plan": [
                {"step": "implement", "status": "in_progress"},
                {"step": "verify", "status": "pending"}
            ]}),
        )
        .expect("store plan");
        let mut recovery = "## Files Previously Read\n- src/lib.rs".to_string();

        append_plan_recovery(&mut recovery, &session_key);

        assert!(recovery.contains("Files Previously Read"));
        assert!(recovery.contains("Current Plan (Recovery)"));
        assert!(recovery.contains("[pending] verify"));
    }
}
