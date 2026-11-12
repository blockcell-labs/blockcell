use super::*;
use crate::commands::gateway::chat::assign_session_id;
use crate::commands::slash_commands::{CommandContext, CommandResult, SLASH_COMMAND_HANDLER};
// ---------------------------------------------------------------------------
// P0: WebSocket with structured protocol
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
struct WsSessionScope {
    agent_id: String,
    chat_id: String,
}

/// Only the fields needed to route a broadcast event to a connection. Used
/// instead of `serde_json::Value` so visibility checks skip the heavy payload
/// (`content`/`token`/…) without allocating a full JSON tree — this runs once
/// per event *per connection* on the streaming hot path.
#[derive(Deserialize)]
struct WsEventRouting<'a> {
    #[serde(rename = "type", borrow, default)]
    event_type: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow, default)]
    channel: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow, default)]
    chat_id: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow, default)]
    agent_id: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow, default)]
    ws_connection_id: Option<std::borrow::Cow<'a, str>>,
}

const MAX_WS_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_WS_MESSAGES_PER_WINDOW: usize = 60;
const WS_RATE_LIMIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

fn ws_inbound_message_size(msg: &WsMessage) -> usize {
    match msg {
        WsMessage::Text(text) => text.len(),
        WsMessage::Binary(bytes) | WsMessage::Ping(bytes) | WsMessage::Pong(bytes) => bytes.len(),
        WsMessage::Close(_) => 0,
    }
}

fn ws_inbound_message_within_size_limit(msg: &WsMessage) -> bool {
    ws_inbound_message_size(msg) <= MAX_WS_MESSAGE_BYTES
}

// Combined size + rate-limit gate. The runtime loop inlines these checks (so it
// can emit distinct log messages per rejection reason); this helper exists to
// unit-test the combined policy.
#[cfg(test)]
fn ws_inbound_message_allowed(
    msg: &WsMessage,
    limiter: &mut WsRateLimiter,
    now: std::time::Instant,
) -> bool {
    if !ws_inbound_message_within_size_limit(msg) {
        return false;
    }
    if matches!(msg, WsMessage::Close(_)) {
        return true;
    }
    limiter.allow(now)
}

struct WsRateLimiter {
    capacity: usize,
    window: std::time::Duration,
    seen_at: std::collections::VecDeque<std::time::Instant>,
}

impl WsRateLimiter {
    fn new(capacity: usize, window: std::time::Duration) -> Self {
        Self {
            capacity,
            window,
            seen_at: std::collections::VecDeque::new(),
        }
    }

    fn allow(&mut self, now: std::time::Instant) -> bool {
        while self
            .seen_at
            .front()
            .is_some_and(|seen| now.duration_since(*seen) >= self.window)
        {
            self.seen_at.pop_front();
        }

        if self.seen_at.len() >= self.capacity {
            return false;
        }

        self.seen_at.push_back(now);
        true
    }
}

fn ws_event_visible_to_connection(
    subscriptions: &std::collections::HashSet<WsSessionScope>,
    connection_id: &str,
    msg: &str,
) -> bool {
    let Ok(event) = serde_json::from_str::<WsEventRouting>(msg) else {
        return false;
    };

    let event_type = event.event_type.as_deref().unwrap_or("");
    if matches!(
        event_type,
        "skills_updated"
            | "evolution_triggered"
            | "evolution_resumed"
            | "evolution_stopped"
            | "evolution_deleted"
    ) {
        return true;
    }

    if event.channel.as_deref() != Some("ws") {
        return false;
    }

    let chat_id = match event.chat_id.as_deref() {
        Some(c) if !c.is_empty() => c,
        _ => return false,
    };

    let agent_id = event.agent_id.as_deref().unwrap_or("default");

    if !subscriptions.contains(&WsSessionScope {
        agent_id: agent_id.to_string(),
        chat_id: chat_id.to_string(),
    }) {
        return false;
    }

    if let Some(expected_connection_id) = event.ws_connection_id.as_deref() {
        return expected_connection_id == connection_id;
    }

    true
}

fn ws_confirm_response_allowed(
    subscriptions: &std::collections::HashSet<WsSessionScope>,
    connection_id: &str,
    pending: &PendingWsConfirmScope,
) -> bool {
    let subscribed = subscriptions.contains(&WsSessionScope {
        agent_id: pending.agent_id.clone(),
        chat_id: pending.chat_id.clone(),
    });
    if !subscribed {
        return false;
    }

    match pending.ws_connection_id.as_deref() {
        Some(expected) => expected == connection_id,
        None => false,
    }
}

fn route_chat_to_active_steering(
    active_steering: &std::collections::HashMap<
        blockcell_agent::SteeringSessionKey,
        blockcell_agent::SteeringSender,
    >,
    agent_id: &str,
    chat_id: &str,
    content: String,
    channel: &str,
) -> bool {
    let key = blockcell_agent::ActiveConversationKey::from_channel_chat(agent_id, channel, chat_id);
    let Some(sender) = active_steering.get(&key) else {
        return false;
    };

    sender
        .try_send(blockcell_agent::SteeringMessage {
            content,
            channel: channel.to_string(),
            chat_id: chat_id.to_string(),
        })
        .is_ok()
}

pub(super) async fn handle_ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<GatewayState>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    let requested_protocol = req
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .split(',')
                .map(str::trim)
                .find(|protocol| protocol.starts_with("blockcell-auth."))
                .map(str::to_string)
        });
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
            let from_protocol = requested_protocol
                .as_deref()
                .and_then(token_from_websocket_protocol)
                .map(|v| secure_eq(&v, t.as_str()))
                .unwrap_or(false);
            from_header || from_protocol
        }
        _ => true, // no token configured → open access
    };

    let ws = if let Some(protocol) = requested_protocol {
        ws.protocols([protocol])
    } else {
        ws
    };
    ws.max_message_size(MAX_WS_MESSAGE_BYTES)
        .max_frame_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
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

fn token_from_websocket_protocol(header: &str) -> Option<String> {
    let encoded = header
        .split(',')
        .map(str::trim)
        .find_map(|protocol| protocol.strip_prefix("blockcell-auth."))?;
    if encoded.is_empty() || encoded.len() % 2 != 0 {
        return None;
    }
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16).ok())
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}

fn with_ws_session_handle(event: String) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&event) else {
        return event;
    };
    if value.get("channel").and_then(serde_json::Value::as_str) != Some("ws") {
        return event;
    }
    let Some(chat_id) = value.get("chat_id").and_then(serde_json::Value::as_str) else {
        return event;
    };
    let key = blockcell_core::session_key::build_session_key("ws", chat_id);
    value["session_id"] =
        blockcell_core::session_id_from_file_stem(&blockcell_core::session_file_stem(&key)).into();
    value.to_string()
}

pub(super) async fn handle_ws_connection(socket: WebSocket, state: GatewayState) {
    info!("WebSocket client connected");

    let (mut ws_sender, mut ws_receiver) = socket.split();
    let mut broadcast_rx = state.ws_broadcast.subscribe();
    let subscriptions = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let (direct_tx, mut direct_rx) = mpsc::channel::<String>(16);
    let connection_id = format!("ws_{}", uuid::Uuid::new_v4().simple());

    use futures::SinkExt;
    use futures::StreamExt;

    // Task: forward broadcast events to this WS client
    let send_subscriptions = Arc::clone(&subscriptions);
    let send_connection_id = connection_id.clone();
    let send_task = tokio::spawn(async move {
        let mut direct_open = true;
        loop {
            tokio::select! {
                direct = direct_rx.recv(), if direct_open => {
                    let Some(msg) = direct else {
                        direct_open = false;
                        continue;
                    };
                    if ws_sender.send(WsMessage::Text(with_ws_session_handle(msg))).await.is_err() {
                        break;
                    }
                }
                received = broadcast_rx.recv() => {
                    let Ok(msg) = received else {
                        break;
                    };
                    let visible = {
                        let subscriptions = send_subscriptions.lock().await;
                        ws_event_visible_to_connection(&subscriptions, &send_connection_id, &msg)
                    };
                    if !visible {
                        continue;
                    }
                    if ws_sender.send(WsMessage::Text(with_ws_session_handle(msg))).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Task: receive messages from this WS client
    let inbound_tx = state.inbound_tx.clone();
    let ws_broadcast = state.ws_broadcast.clone();
    let mut rate_limiter = WsRateLimiter::new(MAX_WS_MESSAGES_PER_WINDOW, WS_RATE_LIMIT_WINDOW);

    while let Some(msg) = ws_receiver.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "WebSocket receive error");
                break;
            }
        };

        if !ws_inbound_message_within_size_limit(&msg) {
            warn!(
                bytes = ws_inbound_message_size(&msg),
                limit = MAX_WS_MESSAGE_BYTES,
                "Closing WebSocket connection after oversized inbound message"
            );
            break;
        }
        if !matches!(msg, WsMessage::Close(_)) && !rate_limiter.allow(std::time::Instant::now()) {
            warn!("Closing WebSocket connection after rate limit exceeded");
            break;
        }

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
                                            let _ = direct_tx
                                                .send(
                                                    serde_json::json!({
                                                        "type": "error",
                                                        "channel": "ws",
                                                        "client_chat_id": client_chat_id,
                                                        "chat_id": client_chat_id,
                                                        "message": err,
                                                    })
                                                    .to_string(),
                                                )
                                                .await;
                                            continue;
                                        }
                                    }
                                }
                                None => "default".to_string(),
                            };

                            let chat_id = assign_session_id(&client_chat_id, &resolved_agent_id);
                            {
                                let mut subscriptions = subscriptions.lock().await;
                                subscriptions.insert(WsSessionScope {
                                    agent_id: resolved_agent_id.clone(),
                                    chat_id: chat_id.clone(),
                                });
                            }

                            let _ = ws_broadcast.send(
                                WsEvent::SessionBound {
                                    channel: "ws".to_string(),
                                    client_chat_id: client_chat_id.clone(),
                                    chat_id: chat_id.clone(),
                                    agent_id: resolved_agent_id.clone(),
                                }
                                .to_json(),
                            );

                            // 斜杠命令拦截：在创建 InboundMessage 之前检查
                            let mut ws_metadata = serde_json::json!({
                                "ws_connection_id": connection_id.clone(),
                            });
                            if content.starts_with('/') {
                                let session_key = format!("ws:{}", chat_id);
                                let ctx = CommandContext::for_websocket(
                                    state.paths.clone(),
                                    state.task_manager.clone(),
                                    chat_id.clone(),
                                    &resolved_agent_id,
                                )
                                .with_clear_callback(
                                    super::create_session_clear_callback(
                                        state.response_caches.clone(),
                                        resolved_agent_id.clone(),
                                        session_key,
                                    ),
                                );

                                match SLASH_COMMAND_HANDLER.try_handle(&content, &ctx).await {
                                    CommandResult::Handled(response) => {
                                        // 复用 message_done 事件（前端已支持）
                                        let event = serde_json::json!({
                                            "type": "message_done",
                                            "channel": "ws",
                                            "agent_id": resolved_agent_id,
                                            "chat_id": chat_id,
                                            "content": response.content,
                                            "is_markdown": response.is_markdown,
                                            "task_id": "",
                                        });
                                        let _ = ws_broadcast.send(event.to_string());
                                        continue; // 不转发给 AgentRuntime
                                    }
                                    CommandResult::NotACommand => {
                                        // 非斜杠命令，继续正常消息处理流程
                                    }
                                    CommandResult::PermissionDenied(msg) => {
                                        let _ = ws_broadcast.send(
                                            serde_json::json!({
                                                "type": "error",
                                                "channel": "ws",
                                                "agent_id": resolved_agent_id,
                                                "chat_id": chat_id,
                                                "message": format!("权限不足: {}", msg),
                                            })
                                            .to_string(),
                                        );
                                        continue;
                                    }
                                    CommandResult::Error(e) => {
                                        let _ = ws_broadcast.send(
                                            serde_json::json!({
                                                "type": "error",
                                                "channel": "ws",
                                                "agent_id": resolved_agent_id,
                                                "chat_id": chat_id,
                                                "message": format!("命令执行错误: {}", e),
                                            })
                                            .to_string(),
                                        );
                                        continue;
                                    }
                                    CommandResult::ExitRequested => {
                                        // /quit 和 /exit 在 WebSocket 模式下不应该到达这里
                                        // 因为渠道限制会在 try_handle 中处理
                                        let _ = ws_broadcast.send(
                                            serde_json::json!({
                                                "type": "error",
                                                "channel": "ws",
                                                "agent_id": resolved_agent_id,
                                                "chat_id": chat_id,
                                                "message": "此命令仅在 CLI 模式可用",
                                            })
                                            .to_string(),
                                        );
                                        continue;
                                    }
                                    CommandResult::ForwardToRuntime {
                                        transformed_content,
                                        original_command,
                                    } => {
                                        // 命令需要转发给 AgentRuntime（如 /learn, /cancel-task, /resume）
                                        tracing::info!(
                                            command = %original_command,
                                            "Forwarding command to AgentRuntime"
                                        );
                                        // 使用转换后的内容替代原始内容
                                        content = transformed_content;
                                        // 标记来源为斜杠命令，runtime 据此验证授权
                                        ws_metadata = serde_json::json!({
                                            "ws_connection_id": connection_id.clone(),
                                            "source": "slash_command",
                                            "original_command": original_command
                                        });
                                        // 继续正常流程，转发给 AgentRuntime
                                    }
                                }
                            }

                            let is_runtime_command =
                                ws_metadata.get("source").and_then(|v| v.as_str())
                                    == Some("slash_command");
                            if !is_runtime_command && media.is_empty() {
                                let active_steering = state.active_steering.lock().await;
                                if route_chat_to_active_steering(
                                    &active_steering,
                                    &resolved_agent_id,
                                    &chat_id,
                                    content.clone(),
                                    "ws",
                                ) {
                                    tracing::info!(
                                        agent_id = %resolved_agent_id,
                                        chat_id = %chat_id,
                                        "Routed WebSocket chat message to active steering channel"
                                    );
                                    continue;
                                }
                            }

                            let inbound = InboundMessage {
                                channel: "ws".to_string(),
                                account_id: None,
                                sender_id: "user".to_string(),
                                chat_id: chat_id.clone(),
                                content,
                                media,
                                metadata: ws_metadata,
                                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                            };

                            let inbound = with_route_agent_id(inbound, &resolved_agent_id);

                            if let Err(e) = inbound_tx.send(inbound).await {
                                let _ = ws_broadcast.send(
                                    WsEvent::error(
                                        resolved_agent_id.clone(),
                                        chat_id.clone(),
                                        format!("{}", e),
                                    )
                                    .to_json(),
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
                                map.retain(|_, p| !p.response_tx.is_closed());
                                let allowed = if let Some(pending) = map.get(&request_id) {
                                    let subscriptions = subscriptions.lock().await;
                                    ws_confirm_response_allowed(
                                        &subscriptions,
                                        &connection_id,
                                        &pending.scope,
                                    )
                                } else {
                                    false
                                };
                                if allowed {
                                    if let Some(pending) = map.remove(&request_id) {
                                        let _ = pending.response_tx.send(approved);
                                        debug!(request_id = %request_id, approved, "Confirm response routed");
                                    }
                                } else {
                                    warn!(request_id = %request_id, "Rejected unauthorized confirm response");
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
                                            let _ = direct_tx
                                                .send(
                                                    serde_json::json!({
                                                        "type": "error",
                                                        "channel": "ws",
                                                        "chat_id": chat_id,
                                                        "message": err,
                                                    })
                                                    .to_string(),
                                                )
                                                .await;
                                            continue;
                                        }
                                    }
                                }
                                None => inbound,
                            };

                            let event_agent_id = resolve_runtime_agent_id(&state.config, &inbound)
                                .unwrap_or_else(|| "default".to_string());
                            if let Err(e) = inbound_tx.send(inbound).await {
                                let _ = ws_broadcast.send(
                                    WsEvent::error(event_agent_id, chat_id, format!("{}", e))
                                        .to_json(),
                                );
                            }
                        }
                        _ => {
                            // Fallback: treat as plain chat
                            {
                                let mut subscriptions = subscriptions.lock().await;
                                subscriptions.insert(WsSessionScope {
                                    agent_id: "default".to_string(),
                                    chat_id: "default".to_string(),
                                });
                            }
                            let inbound = InboundMessage {
                                channel: "ws".to_string(),
                                account_id: None,
                                sender_id: "user".to_string(),
                                chat_id: "default".to_string(),
                                content: text.to_string(),
                                media: vec![],
                                metadata: serde_json::json!({
                                    "ws_connection_id": connection_id.clone(),
                                }),
                                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                            };
                            let _ = inbound_tx.send(inbound).await;
                        }
                    }
                } else {
                    // Plain text fallback
                    {
                        let mut subscriptions = subscriptions.lock().await;
                        subscriptions.insert(WsSessionScope {
                            agent_id: "default".to_string(),
                            chat_id: "default".to_string(),
                        });
                    }
                    let inbound = InboundMessage {
                        channel: "ws".to_string(),
                        account_id: None,
                        sender_id: "user".to_string(),
                        chat_id: "default".to_string(),
                        content: text.to_string(),
                        media: vec![],
                        metadata: serde_json::json!({
                            "ws_connection_id": connection_id.clone(),
                        }),
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

#[cfg(test)]
mod tests {
    #[test]
    fn websocket_errors_preserve_the_requested_agent_scope() {
        let event = super::WsEvent::error("ops", "chat-1", "runtime unavailable").to_json();
        let subscriptions = std::collections::HashSet::from([super::WsSessionScope {
            agent_id: "ops".into(),
            chat_id: "chat-1".into(),
        }]);
        assert!(super::ws_event_visible_to_connection(
            &subscriptions,
            "connection",
            &event
        ));
        let event: serde_json::Value = serde_json::from_str(&event).unwrap();
        assert_eq!(event["agent_id"], "ops");
    }

    #[test]
    fn ws_session_events_include_a_lossless_api_handle() {
        let event = super::with_ws_session_handle(
            serde_json::json!({
                "type": "message_done", "channel": "ws", "chat_id": "a_b", "agent_id": "ops"
            })
            .to_string(),
        );
        let event: serde_json::Value = serde_json::from_str(&event).unwrap();
        assert_eq!(event["chat_id"], "a_b");
        let expected =
            blockcell_core::session_id_from_file_stem(&blockcell_core::session_file_stem("ws:a_b"));
        assert_eq!(event["session_id"], expected);
    }

    use super::*;

    #[test]
    fn websocket_auth_protocol_decodes_token_without_query_parameters() {
        let header = "chat, blockcell-auth.62635f74657374";
        assert_eq!(
            token_from_websocket_protocol(header).as_deref(),
            Some("bc_test")
        );
    }
    use std::collections::HashSet;

    fn scope(agent_id: &str, chat_id: &str) -> WsSessionScope {
        WsSessionScope {
            agent_id: agent_id.to_string(),
            chat_id: chat_id.to_string(),
        }
    }

    #[test]
    fn ws_event_filter_allows_only_subscribed_session_events() {
        let subscriptions = HashSet::from([scope("default", "chat-a")]);

        let own = serde_json::json!({
            "type": "message_done",
            "channel": "ws",
            "agent_id": "default",
            "chat_id": "chat-a",
            "content": "visible",
        })
        .to_string();
        assert!(ws_event_visible_to_connection(
            &subscriptions,
            "connection-a",
            &own
        ));

        let other_chat = serde_json::json!({
            "type": "message_done",
            "channel": "ws",
            "agent_id": "default",
            "chat_id": "chat-b",
            "content": "hidden",
        })
        .to_string();
        assert!(!ws_event_visible_to_connection(
            &subscriptions,
            "connection-a",
            &other_chat
        ));

        let other_agent = serde_json::json!({
            "type": "message_done",
            "channel": "ws",
            "agent_id": "ops",
            "chat_id": "chat-a",
            "content": "hidden",
        })
        .to_string();
        assert!(!ws_event_visible_to_connection(
            &subscriptions,
            "connection-a",
            &other_agent
        ));
    }

    #[test]
    fn ws_event_filter_rejects_non_ws_or_unscoped_session_events_even_when_chat_id_matches() {
        let subscriptions = HashSet::from([scope("default", "chat-a")]);

        let external = serde_json::json!({
            "type": "token",
            "channel": "telegram",
            "agent_id": "default",
            "chat_id": "chat-a",
            "delta": "hidden",
        })
        .to_string();
        assert!(!ws_event_visible_to_connection(
            &subscriptions,
            "connection-a",
            &external
        ));

        let missing_channel = serde_json::json!({
            "type": "token",
            "agent_id": "default",
            "chat_id": "chat-a",
            "delta": "hidden",
        })
        .to_string();
        assert!(!ws_event_visible_to_connection(
            &subscriptions,
            "connection-a",
            &missing_channel
        ));
    }

    #[test]
    fn ws_event_filter_rejects_prebind_error_without_session_subscription() {
        let subscriptions = HashSet::new();
        let event = serde_json::json!({
            "type": "error",
            "channel": "ws",
            "client_chat_id": "draft-session",
            "chat_id": "draft-session",
            "message": "Unknown agent 'missing'",
        })
        .to_string();

        assert!(!ws_event_visible_to_connection(
            &subscriptions,
            "connection-a",
            &event
        ));
    }

    #[test]
    fn ws_event_filter_keeps_global_dashboard_refresh_events() {
        let subscriptions = HashSet::new();
        let event = serde_json::json!({
            "type": "skills_updated",
            "new_skills": ["demo"],
        })
        .to_string();

        assert!(ws_event_visible_to_connection(
            &subscriptions,
            "connection-a",
            &event
        ));
    }

    #[test]
    fn ws_event_filter_restricts_connection_scoped_confirm_requests() {
        let subscriptions = HashSet::from([scope("default", "chat-a")]);
        let event = serde_json::json!({
            "type": "confirm_request",
            "channel": "ws",
            "agent_id": "default",
            "chat_id": "chat-a",
            "ws_connection_id": "origin-connection",
            "request_id": "confirm_123",
        })
        .to_string();

        assert!(!ws_event_visible_to_connection(
            &subscriptions,
            "other-connection",
            &event
        ));
        assert!(ws_event_visible_to_connection(
            &subscriptions,
            "origin-connection",
            &event
        ));
    }

    #[test]
    fn ws_confirm_response_requires_matching_connection_id() {
        let subscriptions = HashSet::from([scope("default", "chat-a")]);
        let pending = PendingWsConfirmScope {
            agent_id: "default".to_string(),
            chat_id: "chat-a".to_string(),
            ws_connection_id: Some("origin-connection".to_string()),
        };

        assert!(!ws_confirm_response_allowed(
            &subscriptions,
            "other-connection",
            &pending
        ));
        assert!(ws_confirm_response_allowed(
            &subscriptions,
            "origin-connection",
            &pending
        ));
    }

    #[test]
    fn ws_confirm_response_rejects_ws_pending_without_connection_id() {
        let pending = PendingWsConfirmScope {
            agent_id: "default".to_string(),
            chat_id: "chat-a".to_string(),
            ws_connection_id: None,
        };

        assert!(!ws_confirm_response_allowed(
            &HashSet::new(),
            "any-connection",
            &pending
        ));
        assert!(!ws_confirm_response_allowed(
            &HashSet::from([scope("default", "chat-a")]),
            "any-connection",
            &pending
        ));
    }

    #[test]
    fn ws_message_size_limit_rejects_oversized_text() {
        assert!(ws_inbound_message_within_size_limit(&WsMessage::Text(
            "a".repeat(MAX_WS_MESSAGE_BYTES)
        )));
        assert!(!ws_inbound_message_within_size_limit(&WsMessage::Text(
            "a".repeat(MAX_WS_MESSAGE_BYTES + 1)
        )));
    }

    #[test]
    fn ws_message_size_limit_rejects_oversized_binary() {
        assert!(ws_inbound_message_within_size_limit(&WsMessage::Binary(
            vec![0; MAX_WS_MESSAGE_BYTES]
        )));
        assert!(!ws_inbound_message_within_size_limit(&WsMessage::Binary(
            vec![0; MAX_WS_MESSAGE_BYTES + 1]
        )));
    }

    #[test]
    fn ws_rate_limiter_rejects_burst_above_capacity() {
        let mut limiter = WsRateLimiter::new(2, std::time::Duration::from_secs(60));

        assert!(limiter.allow(std::time::Instant::now()));
        assert!(limiter.allow(std::time::Instant::now()));
        assert!(!limiter.allow(std::time::Instant::now()));
    }

    #[test]
    fn ws_rate_limiter_applies_to_non_text_messages() {
        let mut limiter = WsRateLimiter::new(1, std::time::Duration::from_secs(60));

        assert!(ws_inbound_message_allowed(
            &WsMessage::Ping(vec![]),
            &mut limiter,
            std::time::Instant::now()
        ));
        assert!(!ws_inbound_message_allowed(
            &WsMessage::Ping(vec![]),
            &mut limiter,
            std::time::Instant::now()
        ));
    }

    #[test]
    fn active_ws_chat_routes_to_steering_channel() {
        let (mut channel, sender) = blockcell_agent::SteeringChannel::new(4);
        let active_steering = std::collections::HashMap::from([(
            blockcell_agent::ActiveConversationKey::from_channel_chat("ops", "ws", "chat-a"),
            sender,
        )]);

        let routed = route_chat_to_active_steering(
            &active_steering,
            "ops",
            "chat-a",
            "adjust course".to_string(),
            "ws",
        );

        assert!(routed);
        let drained = channel.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].content, "adjust course");
        assert_eq!(drained[0].channel, "ws");
        assert_eq!(drained[0].chat_id, "chat-a");
    }
}
