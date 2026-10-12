//! `process_message_inner` 的早返回阶段处理：从主流程中抽离两个相互独立、
//! 命中即直接返回的分支——Cron 提醒快路径与手动 `/compact` 压缩请求。
//!
//! 抽离目的是缩小 `process_message_inner` 巨型方法体积、隔离这两个清晰职责，
//! 行为与原内联实现完全一致（纯搬运，不改语义）。

use super::*;

impl AgentRuntime {
    /// Cron 提醒快路径：当消息带 `reminder` 元数据时直接投递提醒，不经过 LLM。
    ///
    /// 命中并处理后返回 `Some(final_response)`，未命中返回 `None`，由主流程继续。
    pub(super) async fn try_cron_reminder_fast_path(&self, msg: &InboundMessage) -> Option<String> {
        if !msg
            .metadata
            .get("reminder")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return None;
        }
        let reminder_msg = msg
            .metadata
            .get("reminder_message")
            .and_then(|v| v.as_str())
            .unwrap_or(&msg.content);
        let job_name = msg
            .metadata
            .get("job_name")
            .and_then(|v| v.as_str())
            .unwrap_or("提醒");
        let final_response = format!("⏰ [{}] {}", job_name, reminder_msg);
        info!(job_name = %job_name, "Cron reminder delivered directly (bypassing LLM)");

        // Don't store reminder message in history to prevent LLM from learning the format
        // Users can view their scheduled tasks via `cron list` tool

        // Send to outbound (CLI printer + gateway's outbound_to_ws_bridge)
        if let Some(tx) = &self.outbound_tx {
            let mut outbound = OutboundMessage::new(&msg.channel, &msg.chat_id, &final_response);
            outbound.account_id = msg.account_id.clone();
            let _ = tx.send(outbound).await;
        }

        // Deliver to external channel if configured
        if let Some(true) = msg.metadata.get("deliver").and_then(|v| v.as_bool()) {
            if let (Some(channel), Some(to)) = (
                msg.metadata.get("deliver_channel").and_then(|v| v.as_str()),
                msg.metadata.get("deliver_to").and_then(|v| v.as_str()),
            ) {
                if channel == "ws" {
                    if let Some(ref event_tx) = self.event_tx {
                        let event = serde_json::json!({
                            "type": "message_done",
                            "channel": "ws",
                            "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                            "chat_id": to,
                            "task_id": "",
                            "content": final_response,
                            "tool_calls": 0,
                            "duration_ms": 0,
                            "media": [],
                            "background_delivery": true,
                            "delivery_kind": "cron",
                            "cron_kind": "reminder",
                        });
                        let _ = event_tx.send(event.to_string());
                    }
                }
                if let Some(tx) = &self.outbound_tx {
                    let outbound = OutboundMessage::new(channel, to, &final_response);
                    let _ = tx.send(outbound).await;
                }
            }
        }

        Some(final_response)
    }

    /// 处理来自 `/compact` 命令的手动压缩请求（`msg.content == "__COMPACT_REQUEST__"`）。
    pub(super) async fn handle_manual_compact_request(
        &mut self,
        msg: &InboundMessage,
        session_key: &str,
        persist_session_key: &str,
        metrics: &mut ProcessingMetrics,
    ) -> Result<()> {
        info!(
            session_key = %persist_session_key,
            channel = %msg.channel,
            "[compact] Manual compact request received"
        );

        // 使用 persist_session_key 加载和保存历史：cron 转发场景下
        // persist_session_key 是目标会话，session_key 是来源会话
        let history = self.session_store.load(persist_session_key)?;
        if let Err(e) = self
            .capture_pre_compress_learning_boundary(persist_session_key, &history)
            .await
        {
            warn!(error = %e, session_key = %persist_session_key, "Ghost learning pre-compress capture failed");
        }

        // Execute compact directly (is_auto=false for manual trigger)
        let result = self
            .execute_layer4_compact(&history, msg, persist_session_key, "manual", false)
            .await;

        if result.success {
            // Store compacted history
            let mut compacted_messages = vec![
                ChatMessage::system(&result.to_compact_message()),
                ChatMessage::user("请继续当前任务。"),
            ];
            compacted_messages.extend(result.recent_messages);
            self.session_store
                .save(persist_session_key, &compacted_messages)?;

            // Clear trackers
            if let Some(ms) = self.memory_system.as_mut() {
                ms.file_tracker_mut().clear();
                ms.skill_tracker_mut().clear();
                // 重置 Session/Auto 增量基线，避免压缩后永远不触发记忆提取
                ms.reset_baselines_after_compact(&compacted_messages);
            }

            // Record compression metrics
            metrics.record_compression();

            // Send WebSocket notification for ws channel
            if msg.channel == "ws" {
                if let Some(ref event_tx) = self.event_tx {
                    let notification_content = if result.pre_compact_tokens > 0 {
                        let compression_ratio =
                            (result.pre_compact_tokens - result.post_compact_tokens) as f64
                                / result.pre_compact_tokens as f64
                                * 100.0;
                        format!(
                            "✅ 已压缩对话历史，保留关键信息。\n📊 Token: {} → {} (压缩 {:.0}%)",
                            result.pre_compact_tokens,
                            result.post_compact_tokens,
                            compression_ratio
                        )
                    } else {
                        "✅ 压缩完成（无历史内容需要压缩）".to_string()
                    };
                    let event = serde_json::json!({
                        "type": "message_done",
                        "channel": msg.channel,
                        "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                        "chat_id": msg.chat_id,
                        "task_id": "",
                        "content": notification_content,
                        "tool_calls": 0,
                        "duration_ms": 0,
                        "media": [],
                        "is_markdown": true,
                    });
                    let _ = event_tx.send(event.to_string());
                }
            }
        } else {
            // Log failure for debugging
            warn!(
                session_key = %session_key,
                reason = result.error.as_deref().unwrap_or("unknown"),
                "[compact] Manual compact request failed"
            );

            // Send failure notification
            if msg.channel == "ws" {
                if let Some(ref event_tx) = self.event_tx {
                    let error_msg = result.error.as_deref().unwrap_or("压缩失败，请稍后重试。");
                    let notification_content = format!("⚠️ 压缩失败: {}", error_msg);
                    let event = serde_json::json!({
                        "type": "message_done",
                        "channel": msg.channel,
                        "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                        "chat_id": msg.chat_id,
                        "task_id": "",
                        "content": notification_content,
                        "tool_calls": 0,
                        "duration_ms": 0,
                        "media": [],
                        "is_markdown": true,
                    });
                    let _ = event_tx.send(event.to_string());
                }
            } else if let Some(ref tx) = &self.outbound_tx {
                let error_msg = result.error.as_deref().unwrap_or("压缩失败，请稍后重试。");
                let notification_content = format!("⚠️ 压缩失败: {}", error_msg);
                let mut notification =
                    OutboundMessage::new(&msg.channel, &msg.chat_id, &notification_content);
                if let Some(aid) = msg.account_id.as_deref() {
                    notification.account_id = Some(aid.to_string());
                }
                let _ = tx.send(notification).await;
            }
        }

        Ok(())
    }
}
