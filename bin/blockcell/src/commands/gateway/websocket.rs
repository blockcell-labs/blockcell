use super::*;
use crate::commands::gateway::chat::assign_session_id;
use crate::commands::slash_commands::{
    CommandContext, CommandResult, SLASH_COMMAND_HANDLER,
};
// ---------------------------------------------------------------------------
// P0: WebSocket with structured protocol
// ---------------------------------------------------------------------------

pub(super) async fn handle_ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<GatewayState>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    // Validate token inside the WS handler so we can close with code 4401
    // instead of rejecting the HTTP upgrade with 401 (which gives client code 1006).
    let token_valid = match &state.api_token {
        Some(t) if !t.is_empty() => {
            let auth_header = req
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok());
            let from_header = match auth_header {
                Some(h) if h.starts_with("Bearer ") => secure_eq(&h[7..], t.as_str()),
                _ => false,
            };
            let from_query = token_from_query(&req)
                .map(|v| secure_eq(&v, t.as_str()))
                .unwrap_or(false);
            from_header || from_query
        }
        _ => true, // no token configured → open access
    };

    ws.on_upgrade(move |socket| async move {
        if !token_valid {
            let mut socket = socket;
            let _ = socket
                .send(WsMessage::Close(Some(axum::extract::ws::CloseFrame {
                    code: 4401,
                    reason: std::borrow::Cow::Borrowed("Unauthorized"),
                })))
                .await;
            return;
        }
        handle_ws_connection(socket, state).await;
    })
}

pub(super) async fn handle_ws_connection(socket: WebSocket, state: GatewayState) {
    info!("WebSocket client connected");

    let (mut ws_sender, mut ws_receiver) = socket.split();
    let mut broadcast_rx = state.ws_broadcast.subscribe();

    use futures::SinkExt;
    use futures::StreamExt;

    // Task: forward broadcast events to this WS client
    let send_task = tokio::spawn(async move {
        while let Ok(msg) = broadcast_rx.recv().await {
            if ws_sender.send(WsMessage::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    // Task: receive messages from this WS client
    let inbound_tx = state.inbound_tx.clone();
    let ws_broadcast = state.ws_broadcast.clone();

    while let Some(msg) = ws_receiver.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "WebSocket receive error");
                break;
            }
        };

        match msg {
            WsMessage::Text(text) => {
                // Parse structured message
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                    let msg_type = parsed
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("chat");

                    match msg_type {
                        "chat" => {
                            let mut content = parsed
                                .get("content")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let client_chat_id = parsed
                                .get("chat_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let media: Vec<String> = parsed
                                .get("media")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                        .collect()
                                })
                                .unwrap_or_default();

                            let requested_agent_id =
                                parsed.get("agent_id").and_then(|v| v.as_str());
                            let resolved_agent_id = match requested_agent_id {
                                Some(requested) => {
                                    match resolve_requested_agent_id(&state.config, Some(requested))
                                    {
                                        Ok(agent_id) => agent_id,
                                        Err(err) => {
                                            let _ = ws_broadcast.send(
                                                serde_json::to_string(&WsEvent::Error {
                                                    chat_id: client_chat_id.clone(),
                                                    message: err,
                                                })
                                                .unwrap_or_default(),
                                            );
                                            continue;
                                        }
                                    }
                                }
                                None => "default".to_string(),
                            };

                            let chat_id = assign_session_id(&client_chat_id, &resolved_agent_id);

                            let _ = ws_broadcast.send(
                                serde_json::to_string(&WsEvent::SessionBound {
                                    client_chat_id: client_chat_id.clone(),
                                    chat_id: chat_id.clone(),
                                    agent_id: resolved_agent_id.clone(),
                                })
                                .unwrap_or_default(),
                            );

                            // 斜杠命令拦截：在创建 InboundMessage 之前检查
                            if content.starts_with('/') {
                                let ctx = CommandContext::for_websocket(
                                    state.paths.clone(),
                                    state.task_manager.clone(),
                                    chat_id.clone(),
                                );

                                match SLASH_COMMAND_HANDLER.try_handle(&content, &ctx).await {
                                    CommandResult::Handled(response) => {
                                        // 复用 message_done 事件（前端已支持）
                                        let event = serde_json::json!({
                                            "type": "message_done",
                                            "chat_id": chat_id,
                                            "content": response.content,
                                            "task_id": "",
                                        });
                                        let _ = ws_broadcast.send(event.to_string());
                                        continue; // 不转发给 AgentRuntime
                                    }
                                    CommandResult::NotACommand => {
                                        // 非斜杠命令，继续正常消息处理流程
                                    }
                                    CommandResult::PermissionDenied(msg) => {
                                        let _ = ws_broadcast.send(serde_json::json!({
                                            "type": "error",
                                            "chat_id": chat_id,
                                            "message": format!("权限不足: {}", msg),
                                        }).to_string());
                                        continue;
                                    }
                                    CommandResult::Error(e) => {
                                        let _ = ws_broadcast.send(serde_json::json!({
                                            "type": "error",
                                            "chat_id": chat_id,
                                            "message": format!("命令执行错误: {}", e),
                                        }).to_string());
                                        continue;
                                    }
                                    CommandResult::ExitRequested => {
                                        // /quit 和 /exit 在 WebSocket 模式下不应该到达这里
                                        // 因为渠道限制会在 try_handle 中处理
                                        let _ = ws_broadcast.send(serde_json::json!({
                                            "type": "error",
                                            "chat_id": chat_id,
                                            "message": "此命令仅在 CLI 模式可用",
                                        }).to_string());
                                        continue;
                                    }
                                    CommandResult::ForwardToRuntime {
                                        transformed_content,
                                        original_command,
                                    } => {
                                        // 命令需要转发给 AgentRuntime（如 /learn）
                                        tracing::info!(
                                            command = %original_command,
                                            "Forwarding command to AgentRuntime"
                                        );
                                        // 使用转换后的内容替代原始内容
                                        content = transformed_content;
                                        // 继续正常流程，转发给 AgentRuntime
                                    }
                                }
                            }

                            let inbound = InboundMessage {
                                channel: "ws".to_string(),
                                account_id: None,
                                sender_id: "user".to_string(),
                                chat_id: chat_id.clone(),
                                content,
                                media,
                                metadata: serde_json::Value::Null,
                                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                            };

                            let inbound = with_route_agent_id(inbound, &resolved_agent_id);

                            if let Err(e) = inbound_tx.send(inbound).await {
                                let _ = ws_broadcast.send(
                                    serde_json::to_string(&WsEvent::Error {
                                        chat_id: chat_id.clone(),
                                        message: format!("{}", e),
                                    })
                                    .unwrap_or_default(),
                                );
                                break;
                            }
                        }
                        "confirm_response" => {
                            let request_id = parsed
                                .get("request_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let approved = parsed
                                .get("approved")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            if !request_id.is_empty() {
                                let mut map = state.pending_confirms.lock().await;
                                if let Some(tx) = map.remove(&request_id) {
                                    let _ = tx.send(approved);
                                    debug!(request_id = %request_id, approved, "Confirm response routed");
                                }
                            }
                        }
                        "cancel" => {
                            let chat_id = parsed
                                .get("chat_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("default")
                                .to_string();
                            debug!(chat_id = %chat_id, "Received cancel via WS");

                            let inbound = InboundMessage {
                                channel: "ws".to_string(),
                                account_id: None,
                                sender_id: "user".to_string(),
                                chat_id: chat_id.clone(),
                                content: "[cancel]".to_string(),
                                media: vec![],
                                metadata: serde_json::json!({
                                    "cancel": true,
                                }),
                                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                            };

                            let inbound = match parsed.get("agent_id").and_then(|v| v.as_str()) {
                                Some(requested) => {
                                    match resolve_requested_agent_id(&state.config, Some(requested))
                                    {
                                        Ok(agent_id) => with_route_agent_id(inbound, &agent_id),
                                        Err(err) => {
                                            let _ = ws_broadcast.send(
                                                serde_json::to_string(&WsEvent::Error {
                                                    chat_id: chat_id.clone(),
                                                    message: err,
                                                })
                                                .unwrap_or_default(),
                                            );
                                            continue;
                                        }
                                    }
                                }
                                None => inbound,
                            };

                            if let Err(e) = inbound_tx.send(inbound).await {
                                let _ = ws_broadcast.send(
                                    serde_json::to_string(&WsEvent::Error {
                                        chat_id,
                                        message: format!("{}", e),
                                    })
                                    .unwrap_or_default(),
                                );
                            }
                        }
                        _ => {
                            // Fallback: treat as plain chat
                            let inbound = InboundMessage {
                                channel: "ws".to_string(),
                                account_id: None,
                                sender_id: "user".to_string(),
                                chat_id: "default".to_string(),
                                content: text.to_string(),
                                media: vec![],
                                metadata: serde_json::Value::Null,
                                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                            };
                            let _ = inbound_tx.send(inbound).await;
                        }
                    }
                } else {
                    // Plain text fallback
                    let inbound = InboundMessage {
                        channel: "ws".to_string(),
                        account_id: None,
                        sender_id: "user".to_string(),
                        chat_id: "default".to_string(),
                        content: text.to_string(),
                        media: vec![],
                        metadata: serde_json::Value::Null,
                        timestamp_ms: chrono::Utc::now().timestamp_millis(),
                    };
                    let _ = inbound_tx.send(inbound).await;
                }
            }
            WsMessage::Close(_) => break,
            _ => {}
        }
    }

    send_task.abort();
    info!("WebSocket client disconnected");
}
