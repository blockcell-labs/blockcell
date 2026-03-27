//! WebSocket client for NapCatQQ.
//!
//! This module implements the WebSocket client mode where BlockCell
//! connects to NapCatQQ's WebSocket server.

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message as WsMessage},
};
use tracing::{error, info, warn};

use blockcell_core::{Config, Error, InboundMessage, Result};

use super::super::media::{process_media_segments, build_enhanced_content, build_media_metadata};

use crate::account::napcat_account_id;
use super::super::event::{
    MessageEvent, GroupRecallEvent, GroupIncreaseEvent, GroupDecreaseEvent,
    GroupAdminEvent, GroupBanEvent, PokeEvent, FriendAddEvent,
    FriendRequestEvent, GroupRequestEvent,
};
use super::sender::{init_sender, init_api_caller, init_stream_caller, OutboundMessage, ApiCallRequest, StreamCallRequest};
use super::super::types::{ApiRequest, ApiResponse, StreamChunkData};

/// Message deduplication cache.
static DEDUP_CACHE: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn dedup_cache() -> &'static Mutex<std::collections::HashSet<String>> {
    DEDUP_CACHE.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Check if a message ID has been seen before (deduplication).
async fn is_duplicate(msg_id: &str) -> bool {
    let mut dedup = dedup_cache().lock().await;
    if dedup.contains(msg_id) {
        return true;
    }

    // Evict half if at capacity
    if dedup.len() >= 10_000 {
        let to_remove = dedup.len() / 2;
        for key in dedup.iter().take(to_remove).cloned().collect::<Vec<_>>() {
            dedup.remove(&key);
        }
    }

    dedup.insert(msg_id.to_string());
    false
}

/// WebSocket client for NapCatQQ.
///
/// Connects to NapCatQQ's WebSocket server and handles event messages.
pub struct NapCatWsClient {
    config: Config,
    inbound_tx: mpsc::Sender<InboundMessage>,
    request_id: AtomicU64,
    /// Pending API requests waiting for responses.
    pending_requests: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<ApiResponse>>>>,
    /// Active stream sessions (stream_id -> chunk sender).
    active_streams: Arc<Mutex<HashMap<String, mpsc::Sender<StreamChunkData>>>>,
    /// WebSocket write channel for sending API requests (set during run).
    ws_tx: Arc<Mutex<Option<mpsc::Sender<String>>>>,
}

impl NapCatWsClient {
    /// Create a new WebSocket client.
    pub fn new(config: Config, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        Self {
            config,
            inbound_tx,
            request_id: AtomicU64::new(0),
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            active_streams: Arc::new(Mutex::new(HashMap::new())),
            ws_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Generate a new request ID.
    fn next_request_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Check if a user is allowed.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        let napcat = &self.config.channels.napcat;

        // Check blocklist first
        if napcat.block_from.iter().any(|b| b == user_id || b == "*") {
            return false;
        }

        // Check allowlist
        if napcat.allow_from.is_empty() {
            return true;
        }

        napcat.allow_from.iter().any(|a| a == user_id || a == "*")
    }

    /// Check if a group is allowed.
    fn is_group_allowed(&self, group_id: &str) -> bool {
        let napcat = &self.config.channels.napcat;

        if napcat.allow_groups.is_empty() {
            return true;
        }

        napcat.allow_groups.iter().any(|g| g == group_id || g == "*")
    }

    /// Check if should respond to a group message based on response mode.
    fn should_respond_to_group(&self, event: &MessageEvent) -> bool {
        let mode = &self.config.channels.napcat.group_response_mode;

        match mode.as_str() {
            "none" => false,
            "at_only" => event.is_at_me() || event.is_at_all(),
            _ => true,
        }
    }

    /// Handle a message event.
    async fn handle_message_event(&self, event: &MessageEvent) -> Result<()> {
        // Deduplicate
        let msg_id = event.message_id.to_string();
        if is_duplicate(&msg_id).await {
            return Ok(());
        }

        // Check user permission
        if !self.is_user_allowed(&event.user_id) {
            return Ok(());
        }

        // Check group permission for group messages
        if event.is_group() {
            if let Some(ref group_id) = event.group_id {
                if !self.is_group_allowed(group_id) {
                    return Ok(());
                }
            }

            // Check group response mode
            if !self.should_respond_to_group(event) {
                return Ok(());
            }
        }

        // Build chat_id first (needed for media download)
        let chat_id = if event.is_group() {
            format!("group:{}", event.group_id.clone().unwrap_or_default())
        } else {
            format!("user:{}", event.user_id)
        };

        // Get original text content
        let original_text = if event.is_group() && event.is_at_me() {
            event.get_text_without_at()
        } else {
            event.get_text()
        };

        // Get message segments for media processing
        let segments = event.message.as_segments();

        // Auto-download media if configured
        let workspace = &self.config.agents.defaults.workspace;
        let napcat_config = &self.config.channels.napcat;
        let downloaded = process_media_segments(
            napcat_config,
            segments,
            &chat_id,
            workspace,
        ).await?;

        // Build enhanced content with downloaded media info
        let content = build_enhanced_content(&original_text, &downloaded, &chat_id);

        // Skip if no content and no media
        if content.is_empty() && downloaded.is_empty() {
            return Ok(());
        }

        // Build metadata
        let mut metadata = if event.is_group() {
            let group_id = event.group_id.clone().unwrap_or_default();
            serde_json::json!({
                "message_id": event.message_id,
                "group_id": group_id,
                "message_type": "group",
                "sender_nickname": event.sender.nickname,
                "sender_card": event.sender.card,
                "sender_role": event.sender.role,
            })
        } else {
            serde_json::json!({
                "message_id": event.message_id,
                "message_type": "private",
                "sender_nickname": event.sender.nickname,
            })
        };

        // Add downloaded media info to metadata
        if !downloaded.is_empty() {
            let media_metadata = build_media_metadata(&downloaded);
            if let Some(obj) = metadata.as_object_mut() {
                if let Some(media_obj) = media_metadata.as_object() {
                    for (k, v) in media_obj {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        // Extract media paths for InboundMessage.media field
        // NOTE: 不把下载的媒体放入 media 字段，避免 LLM 把它发送回群聊。
        // 这些媒体已经在聊天中存在，无需再次发送。
        // 下载的媒体路径已经在 content 中展示给 LLM（通过 build_enhanced_content）。
        // media 字段应保留给工具生成的媒体（如截图、生成图片等）。
        let media: Vec<String> = vec![];

        let inbound = InboundMessage {
            channel: "napcat".to_string(),
            account_id: napcat_account_id(&self.config),
            sender_id: event.user_id.clone(),
            chat_id: chat_id.clone(),
            content,
            media,
            metadata,
            timestamp_ms: event.time * 1000,
        };

        // Send to agent for processing
        self.inbound_tx
            .send(inbound)
            .await
            .map_err(|e| Error::Channel(e.to_string()))?;

        Ok(())
    }

    // =========================================================================
    // Notice Event Handlers
    // =========================================================================

    /// Handle group recall event.
    async fn handle_group_recall(&self, event: &GroupRecallEvent) {
        info!(
            group_id = %event.group_id,
            operator_id = %event.operator_id,
            message_id = event.message_id,
            "Group message recalled"
        );

        // Send notification to agent
        let notification = InboundMessage {
            channel: "napcat".to_string(),
            account_id: napcat_account_id(&self.config),
            sender_id: event.operator_id.clone(),
            chat_id: format!("group:{}", event.group_id),
            content: format!(
                "[系统] 消息 {} 在群 {} 被 {} 撤回",
                event.message_id, event.group_id, event.operator_id
            ),
            media: vec![],
            metadata: serde_json::json!({
                "event_type": "group_recall",
                "group_id": event.group_id,
                "user_id": event.user_id,
                "operator_id": event.operator_id,
                "message_id": event.message_id,
            }),
            timestamp_ms: event.time * 1000,
        };

        let _ = self.inbound_tx.send(notification).await;
    }

    /// Handle group member increase event.
    async fn handle_group_increase(&self, event: &GroupIncreaseEvent) {
        info!(
            group_id = %event.group_id,
            user_id = %event.user_id,
            operator_id = %event.operator_id,
            sub_type = %event.sub_type,
            "Group member increased"
        );

        let action = match event.sub_type.as_str() {
            "approve" => "加入群",
            "invite" => "被邀请加入群",
            _ => "加入群",
        };

        let notification = InboundMessage {
            channel: "napcat".to_string(),
            account_id: napcat_account_id(&self.config),
            sender_id: event.operator_id.clone(),
            chat_id: format!("group:{}", event.group_id),
            content: format!(
                "[系统] 用户 {} {} {}",
                event.user_id, action, event.group_id
            ),
            media: vec![],
            metadata: serde_json::json!({
                "event_type": "group_increase",
                "group_id": event.group_id,
                "user_id": event.user_id,
                "operator_id": event.operator_id,
                "sub_type": event.sub_type,
            }),
            timestamp_ms: event.time * 1000,
        };

        let _ = self.inbound_tx.send(notification).await;
    }

    /// Handle group member decrease event.
    async fn handle_group_decrease(&self, event: &GroupDecreaseEvent) {
        info!(
            group_id = %event.group_id,
            user_id = %event.user_id,
            operator_id = %event.operator_id,
            sub_type = %event.sub_type,
            "Group member decreased"
        );

        let action = match event.sub_type.as_str() {
            "leave" => "主动退出群",
            "kick" => "被踢出群",
            "kick_me" => "机器人被踢出群",
            _ => "离开群",
        };

        let notification = InboundMessage {
            channel: "napcat".to_string(),
            account_id: napcat_account_id(&self.config),
            sender_id: event.operator_id.clone(),
            chat_id: format!("group:{}", event.group_id),
            content: format!(
                "[系统] 用户 {} {} {}",
                event.user_id, action, event.group_id
            ),
            media: vec![],
            metadata: serde_json::json!({
                "event_type": "group_decrease",
                "group_id": event.group_id,
                "user_id": event.user_id,
                "operator_id": event.operator_id,
                "sub_type": event.sub_type,
            }),
            timestamp_ms: event.time * 1000,
        };

        let _ = self.inbound_tx.send(notification).await;
    }

    /// Handle group admin change event.
    async fn handle_group_admin(&self, event: &GroupAdminEvent) {
        info!(
            group_id = %event.group_id,
            user_id = %event.user_id,
            sub_type = %event.sub_type,
            "Group admin changed"
        );

        let action = match event.sub_type.as_str() {
            "set" => "被设置为管理员",
            "unset" => "被取消管理员",
            _ => "管理员状态变更",
        };

        let notification = InboundMessage {
            channel: "napcat".to_string(),
            account_id: napcat_account_id(&self.config),
            sender_id: event.user_id.clone(),
            chat_id: format!("group:{}", event.group_id),
            content: format!(
                "[系统] 用户 {} 在群 {} {}",
                event.user_id, event.group_id, action
            ),
            media: vec![],
            metadata: serde_json::json!({
                "event_type": "group_admin",
                "group_id": event.group_id,
                "user_id": event.user_id,
                "sub_type": event.sub_type,
            }),
            timestamp_ms: event.time * 1000,
        };

        let _ = self.inbound_tx.send(notification).await;
    }

    /// Handle group ban event.
    async fn handle_group_ban(&self, event: &GroupBanEvent) {
        info!(
            group_id = %event.group_id,
            user_id = %event.user_id,
            operator_id = %event.operator_id,
            duration = event.duration,
            "Group ban event"
        );

        let content = if event.duration == 0 {
            format!(
                "[系统] 用户 {} 在群 {} 被解除禁言",
                event.user_id, event.group_id
            )
        } else {
            format!(
                "[系统] 用户 {} 在群 {} 被禁言 {} 秒",
                event.user_id, event.group_id, event.duration
            )
        };

        let notification = InboundMessage {
            channel: "napcat".to_string(),
            account_id: napcat_account_id(&self.config),
            sender_id: event.operator_id.clone(),
            chat_id: format!("group:{}", event.group_id),
            content,
            media: vec![],
            metadata: serde_json::json!({
                "event_type": "group_ban",
                "group_id": event.group_id,
                "user_id": event.user_id,
                "operator_id": event.operator_id,
                "duration": event.duration,
            }),
            timestamp_ms: event.time * 1000,
        };

        let _ = self.inbound_tx.send(notification).await;
    }

    /// Handle friend add event.
    async fn handle_friend_add(&self, event: &FriendAddEvent) {
        info!(
            user_id = %event.user_id,
            "New friend added"
        );

        let notification = InboundMessage {
            channel: "napcat".to_string(),
            account_id: napcat_account_id(&self.config),
            sender_id: event.user_id.clone(),
            chat_id: format!("user:{}", event.user_id),
            content: format!("[系统] 新好友添加: {}", event.user_id),
            media: vec![],
            metadata: serde_json::json!({
                "event_type": "friend_add",
                "user_id": event.user_id,
            }),
            timestamp_ms: event.time * 1000,
        };

        let _ = self.inbound_tx.send(notification).await;
    }

    /// Handle poke event.
    async fn handle_poke(&self, event: &PokeEvent) {
        info!(
            user_id = %event.user_id,
            target_id = %event.target_id,
            group_id = ?event.group_id,
            "Poke event received"
        );

        let chat_id = if let Some(ref group_id) = event.group_id {
            format!("group:{}", group_id)
        } else {
            format!("user:{}", event.user_id)
        };

        let notification = InboundMessage {
            channel: "napcat".to_string(),
            account_id: napcat_account_id(&self.config),
            sender_id: event.user_id.clone(),
            chat_id,
            content: format!("[系统] 用户 {} 戳了戳你", event.user_id),
            media: vec![],
            metadata: serde_json::json!({
                "event_type": "poke",
                "user_id": event.user_id,
                "target_id": event.target_id,
                "group_id": event.group_id,
            }),
            timestamp_ms: event.time * 1000,
        };

        let _ = self.inbound_tx.send(notification).await;
    }

    // =========================================================================
    // Request Event Handlers
    // =========================================================================

    /// Handle friend request event.
    async fn handle_friend_request(&self, event: &FriendRequestEvent) {
        info!(
            user_id = %event.user_id,
            comment = %event.comment,
            "Friend request received"
        );

        let notification = InboundMessage {
            channel: "napcat".to_string(),
            account_id: napcat_account_id(&self.config),
            sender_id: event.user_id.clone(),
            chat_id: format!("user:{}", event.user_id),
            content: format!(
                "[系统] 好友请求: 用户 {} 请求添加好友. 验证信息: {}",
                event.user_id,
                if event.comment.is_empty() { "无" } else { &event.comment }
            ),
            media: vec![],
            metadata: serde_json::json!({
                "event_type": "friend_request",
                "user_id": event.user_id,
                "comment": event.comment,
                "flag": event.flag,
            }),
            timestamp_ms: event.time * 1000,
        };

        let _ = self.inbound_tx.send(notification).await;
    }

    /// Handle group request event.
    async fn handle_group_request(&self, event: &GroupRequestEvent) {
        info!(
            group_id = %event.group_id,
            user_id = %event.user_id,
            sub_type = %event.sub_type,
            "Group request received"
        );

        let action = match event.sub_type.as_str() {
            "add" => "请求加入群",
            "invite" => "邀请加入群",
            _ => "群请求",
        };

        let notification = InboundMessage {
            channel: "napcat".to_string(),
            account_id: napcat_account_id(&self.config),
            sender_id: event.user_id.clone(),
            chat_id: format!("group:{}", event.group_id),
            content: format!(
                "[系统] {}: 用户 {} {} {}. 验证信息: {}",
                action,
                event.user_id,
                action,
                event.group_id,
                if event.comment.is_empty() { "无" } else { &event.comment }
            ),
            media: vec![],
            metadata: serde_json::json!({
                "event_type": "group_request",
                "group_id": event.group_id,
                "user_id": event.user_id,
                "sub_type": event.sub_type,
                "comment": event.comment,
                "flag": event.flag,
            }),
            timestamp_ms: event.time * 1000,
        };

        let _ = self.inbound_tx.send(notification).await;
    }

    /// Handle a WebSocket event message.
    async fn handle_ws_message(self: &Arc<Self>, text: &str) {
        // Try to parse as event
        if let Ok(event) = serde_json::from_str::<Value>(text) {
            let post_type = event.get("post_type").and_then(|v| v.as_str()).unwrap_or("");

            match post_type {
                "message" => {
                    if let Ok(msg_event) = serde_json::from_value::<MessageEvent>(event.clone()) {
                        // Spawn a new task to handle the message event
                        // This prevents blocking the WebSocket loop when the handler
                        // needs to make API calls (like get_private_file_url)
                        let self_clone = Arc::clone(self);
                        tokio::spawn(async move {
                            if let Err(e) = self_clone.handle_message_event(&msg_event).await {
                                error!("Failed to handle message event: {}", e);
                            }
                        });
                    } else {
                        let parse_error = serde_json::from_value::<MessageEvent>(event.clone()).unwrap_err();
                        warn!(
                            error = %parse_error,
                            raw_json = %serde_json::to_string(&event).unwrap_or_else(|_| "serialize error".to_string()),
                            "Failed to parse message event"
                        );
                    }
                }
                "notice" => {
                    let notice_type = event.get("notice_type").and_then(|v| v.as_str()).unwrap_or("");
                    match notice_type {
                        "group_recall" => {
                            if let Ok(recall) = serde_json::from_value::<GroupRecallEvent>(event.clone()) {
                                self.handle_group_recall(&recall).await;
                            }
                        }
                        "group_increase" => {
                            if let Ok(increase) = serde_json::from_value::<GroupIncreaseEvent>(event.clone()) {
                                self.handle_group_increase(&increase).await;
                            }
                        }
                        "group_decrease" => {
                            if let Ok(decrease) = serde_json::from_value::<GroupDecreaseEvent>(event.clone()) {
                                self.handle_group_decrease(&decrease).await;
                            }
                        }
                        "group_admin" => {
                            if let Ok(admin) = serde_json::from_value::<GroupAdminEvent>(event.clone()) {
                                self.handle_group_admin(&admin).await;
                            }
                        }
                        "group_ban" => {
                            if let Ok(ban) = serde_json::from_value::<GroupBanEvent>(event.clone()) {
                                self.handle_group_ban(&ban).await;
                            }
                        }
                        "friend_add" => {
                            if let Ok(add) = serde_json::from_value::<FriendAddEvent>(event.clone()) {
                                self.handle_friend_add(&add).await;
                            }
                        }
                        "notify" => {
                            // Check sub_type for poke
                            let sub_type = event.get("sub_type").and_then(|v| v.as_str()).unwrap_or("");
                            if sub_type == "poke" {
                                if let Ok(poke) = serde_json::from_value::<PokeEvent>(event.clone()) {
                                    self.handle_poke(&poke).await;
                                }
                            } else {
                                // Unhandled notify event
                            }
                        }
                        _ => {
                            // Unhandled notice event
                        }
                    }
                }
                "request" => {
                    let request_type = event.get("request_type").and_then(|v| v.as_str()).unwrap_or("");
                    match request_type {
                        "friend" => {
                            if let Ok(friend_req) = serde_json::from_value::<FriendRequestEvent>(event.clone()) {
                                self.handle_friend_request(&friend_req).await;
                            }
                        }
                        "group" => {
                            if let Ok(group_req) = serde_json::from_value::<GroupRequestEvent>(event.clone()) {
                                self.handle_group_request(&group_req).await;
                            }
                        }
                        _ => {}
                    }
                }
                "meta_event" => {
                    let meta_type = event
                        .get("meta_event_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match meta_type {
                        "lifecycle" => {
                            let sub_type = event.get("sub_type").and_then(|v| v.as_str()).unwrap_or("");
                            info!("NapCatQQ lifecycle event: {}", sub_type);
                        }
                        "heartbeat" => {}
                        _ => {}
                    }
                }
                "" => {
                    // Could be an API response or stream chunk
                    // First, check if it's a stream chunk response
                    if let Some(data) = event.get("data") {
                        if let Ok(chunk) = serde_json::from_value::<StreamChunkData>(data.clone()) {
                            // This is a stream chunk - route to active stream
                            let stream_id = chunk.stream_id.clone();
                            let chunk_index = chunk.chunk_index;
                            let total = chunk.total_chunks;
                            let is_last = chunk_index + 1 >= total;

                            // Find and send to the stream handler
                            let mut active = self.active_streams.lock().await;

                            // Try to find by stream_id first
                            if let Some(tx) = active.get(&stream_id) {
                                if let Err(e) = tx.send(chunk.clone()).await {
                                    error!("Failed to send stream chunk: {}", e);
                                }
                                if is_last {
                                    active.remove(&stream_id);
                                }
                            } else {
                                // Try to find by echo placeholder (keys starting with "stream_")
                                let placeholder_key = active.keys()
                                    .find(|k| k.starts_with("stream_"))
                                    .cloned();

                                if let Some(key) = placeholder_key {
                                    if let Some(tx) = active.get(&key) {
                                        let tx_clone = tx.clone();
                                        // Send before remapping to avoid borrow issues
                                        let send_result = tx_clone.send(chunk.clone()).await;
                                        if send_result.is_ok() {
                                            // Remap to actual stream_id
                                            active.insert(stream_id.clone(), tx_clone);
                                            active.remove(&key);
                                            if is_last {
                                                active.remove(&stream_id);
                                            }
                                        }
                                    }
                                }
                            }

                            return; // Stream chunk handled
                        }
                    }

                    // Not a stream chunk - handle as normal API response
                    // Extract echo - it could be a string or a number
                    let echo = event.get("echo").and_then(|v| {
                        v.as_str().map(|s| s.to_string()).or_else(|| v.as_i64().map(|n| n.to_string()))
                    });

                    if let Some(echo) = echo {
                        let mut pending = self.pending_requests.lock().await;
                        if let Some(tx) = pending.remove(&echo) {
                            if let Ok(response) = serde_json::from_value::<ApiResponse>(event.clone()) {
                                let _ = tx.send(response);
                            }
                        }
                    } else {
                        // API response without echo - might be an async notification or error
                        warn!(
                            status = ?event.get("status").and_then(|v| v.as_str()),
                            retcode = ?event.get("retcode").and_then(|v| v.as_i64()),
                            message = ?event.get("message").and_then(|v| v.as_str()),
                            wording = ?event.get("wording").and_then(|v| v.as_str()),
                            "API response received without echo field"
                        );
                    }
                }
                _ => {}
            }
        }
    }

    /// Run the WebSocket client loop.
    pub async fn run(self: Arc<Self>, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        let napcat = &self.config.channels.napcat;
        let ws_url = napcat.ws_url.clone();
        let access_token = napcat.access_token.clone();
        let heartbeat_interval = napcat.heartbeat_interval_secs;

        let mut reconnect_delay = napcat.reconnect_delay_secs;

        // Initialize the global outbound sender for WebSocket mode
        // This allows outbound.rs to send messages via WebSocket
        let mut outbound_rx = init_sender();

        // Initialize the global API caller for request-response pattern
        // This allows tools to call APIs via WebSocket
        let mut api_call_rx = init_api_caller();

        // Initialize the global stream caller for streaming APIs
        // This allows tools to call streaming APIs via WebSocket
        let mut stream_call_rx = init_stream_caller();

        loop {
            // Check for shutdown
            if shutdown.try_recv().is_ok() {
                info!("NapCatQQ WebSocket client shutting down");
                // Clear ws_tx on shutdown
                let mut ws_tx = self.ws_tx.lock().await;
                *ws_tx = None;
                return;
            }

            // Build WebSocket request with optional access_token
            // NapCatQQ requires URL to end with "/" (e.g., ws://127.0.0.1:13001/)
            // Ensure the URL has a trailing slash before the path
            let ws_url_with_path = if ws_url.ends_with('/') {
                ws_url.clone()
            } else if ws_url.contains('?') {
                // URL has query params, insert "/" before "?"
                ws_url.replacen('?', "/?", 1)
            } else {
                format!("{}/", ws_url)
            };

            // Add token parameter for authentication
            let ws_url_with_token = if !access_token.is_empty() {
                // Use "access_token" as parameter name (OneBot 11 standard)
                if ws_url_with_path.contains('?') {
                    format!("{}&access_token={}", ws_url_with_path, access_token)
                } else {
                    format!("{}?access_token={}", ws_url_with_path, access_token)
                }
            } else {
                ws_url_with_path
            };

            let ws_request_result = ws_url_with_token.clone().into_client_request();

            let ws_request = match ws_request_result {
                Ok(mut req) => {
                    // Also add Authorization header for compatibility with OneBot 11 spec
                    if !access_token.is_empty() {
                        if let Ok(auth_value) = HeaderValue::from_str(&format!("Bearer {}", access_token)) {
                            req.headers_mut().insert("Authorization", auth_value);
                        }
                        info!("Connecting to NapCatQQ WebSocket server: {} (with token)", ws_url);
                    } else {
                        info!("Connecting to NapCatQQ WebSocket server: {} (no token)", ws_url);
                    }
                    req
                }
                Err(e) => {
                    error!("Invalid WebSocket URL '{}': {}", ws_url, e);
                    return;
                }
            };

            match connect_async(ws_request).await {
                Ok((ws_stream, _)) => {
                    info!("NapCatQQ WebSocket connected");
                    reconnect_delay = napcat.reconnect_delay_secs; // Reset delay on success

                    let (mut write, mut read) = ws_stream.split();

                    // Create channel for API requests and store it
                    let (api_tx, mut api_rx) = mpsc::channel::<String>(64);
                    {
                        let mut ws_tx = self.ws_tx.lock().await;
                        *ws_tx = Some(api_tx);
                    }

                    // Heartbeat timer
                    let mut heartbeat_timer = tokio::time::interval(Duration::from_secs(
                        heartbeat_interval as u64,
                    ));
                    heartbeat_timer
                        .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

                    // Main event loop
                    loop {
                        tokio::select! {
                            _ = shutdown.recv() => {
                                info!("NapCatQQ WebSocket shutting down");
                                let _ = write.send(WsMessage::Close(None)).await;
                                // Clear ws_tx
                                let mut ws_tx = self.ws_tx.lock().await;
                                *ws_tx = None;
                                return;
                            }

                            _ = heartbeat_timer.tick() => {
                                // Send heartbeat via API
                                let heartbeat = serde_json::json!({
                                    "action": "get_status",
                                    "echo": format!("heartbeat_{}", self.next_request_id())
                                });
                                if let Ok(text) = serde_json::to_string(&heartbeat) {
                                    if let Err(e) = write.send(WsMessage::Text(text)).await {
                                        error!("Failed to send heartbeat: {}", e);
                                        break;
                                    }
                                }
                            }

                            // Forward API requests from the channel
                            api_msg = api_rx.recv() => {
                                match api_msg {
                                    Some(text) => {
                                        if let Err(e) = write.send(WsMessage::Text(text)).await {
                                            error!("Failed to send API request: {}", e);
                                            break;
                                        }
                                    }
                                    None => {
                                        // Channel closed
                                        break;
                                    }
                                }
                            }

                            // Handle outbound messages from the global sender
                            // This is how outbound.rs sends messages via WebSocket
                            outbound_msg = outbound_rx.recv() => {
                                match outbound_msg {
                                    Some(OutboundMessage { request, self_id: _ }) => {
                                        // Serialize the request and send via WebSocket
                                        // Note: self_id is ignored in client mode (only one connection)
                                        match serde_json::to_string(&request) {
                                            Ok(text) => {
                                                if let Err(e) = write.send(WsMessage::Text(text)).await {
                                                    error!("Failed to send outbound message: {}", e);
                                                    break;
                                                }
                                            }
                                            Err(e) => {
                                                error!("Failed to serialize outbound request: {}", e);
                                            }
                                        }
                                    }
                                    None => {
                                        // Channel closed, should not happen
                                        warn!("Outbound channel closed");
                                    }
                                }
                            }

                            // Handle API call requests from tools (request-response pattern)
                            api_call = api_call_rx.recv() => {
                                match api_call {
                                    Some(ApiCallRequest { request, response_tx }) => {
                                        // Generate echo for response matching
                                        let echo = request.echo.clone()
                                            .unwrap_or_else(|| self.next_request_id().to_string());

                                        // Register pending request
                                        {
                                            let mut pending = self.pending_requests.lock().await;
                                            pending.insert(echo.clone(), response_tx);
                                        }

                                        // Build request with echo
                                        let request_with_echo = ApiRequest {
                                            action: request.action,
                                            params: request.params,
                                            echo: Some(echo),
                                        };

                                        // Send request via WebSocket
                                        match serde_json::to_string(&request_with_echo) {
                                            Ok(text) => {
                                                if let Err(e) = write.send(WsMessage::Text(text)).await {
                                                    error!("Failed to send API call request: {}", e);
                                                    // Remove pending request on error
                                                    let mut pending = self.pending_requests.lock().await;
                                                    pending.remove(&request.echo.clone().unwrap_or_default());
                                                    break;
                                                }
                                            }
                                            Err(e) => {
                                                error!("Failed to serialize API call request: {}", e);
                                            }
                                        }
                                    }
                                    None => {
                                        warn!("API call channel closed");
                                    }
                                }
                            }

                            // Handle stream call requests from tools (streaming API pattern)
                            stream_call = stream_call_rx.recv() => {
                                match stream_call {
                                    Some(StreamCallRequest { request, chunk_tx, done_tx }) => {
                                        // Generate echo for this stream session
                                        let echo = request.echo.clone()
                                            .unwrap_or_else(|| format!("stream_{}", self.next_request_id()));

                                        // Generate a stream_id (we'll receive the actual one from first response)
                                        let stream_id_placeholder = echo.clone();

                                        // Register the chunk sender for this stream
                                        {
                                            let mut active = self.active_streams.lock().await;
                                            active.insert(stream_id_placeholder.clone(), chunk_tx);
                                        }

                                        // Build request with echo
                                        let request_with_echo = ApiRequest {
                                            action: request.action,
                                            params: request.params,
                                            echo: Some(echo.clone()),
                                        };

                                        // Send request via WebSocket
                                        match serde_json::to_string(&request_with_echo) {
                                            Ok(text) => {
                                                if let Err(e) = write.send(WsMessage::Text(text)).await {
                                                    error!("Failed to send stream call request: {}", e);
                                                    // Clean up on error
                                                    let mut active = self.active_streams.lock().await;
                                                    active.remove(&stream_id_placeholder);
                                                    let _ = done_tx.send(Err(format!("Failed to send request: {}", e)));
                                                }
                                                // On success, the stream will be handled in handle_ws_message
                                                // We'll signal completion when we receive all chunks
                                            }
                                            Err(e) => {
                                                error!("Failed to serialize stream call request: {}", e);
                                                let mut active = self.active_streams.lock().await;
                                                active.remove(&stream_id_placeholder);
                                                let _ = done_tx.send(Err(format!("Failed to serialize: {}", e)));
                                            }
                                        }
                                    }
                                    None => {
                                        warn!("Stream call channel closed");
                                    }
                                }
                            }

                            msg = read.next() => {
                                match msg {
                                    Some(Ok(WsMessage::Text(text))) => {
                                        // Handle the message in a spawned task to avoid blocking
                                        // the main loop (media download may take time and needs to
                                        // receive API responses via the same WebSocket)
                                        let client = self.clone();
                                        tokio::spawn(async move {
                                            client.handle_ws_message(&text).await;
                                        });
                                    }
                                    Some(Ok(WsMessage::Ping(data))) => {
                                        if let Err(e) = write.send(WsMessage::Pong(data)).await {
                                            error!("Failed to send pong: {}", e);
                                            break;
                                        }
                                    }
                                    Some(Ok(WsMessage::Pong(_))) => {
                                        // Ignore pong
                                    }
                                    Some(Ok(WsMessage::Close(frame))) => {
                                        warn!("NapCatQQ WebSocket closed by server: {:?}", frame);
                                        break;
                                    }
                                    Some(Err(e)) => {
                                        error!("NapCatQQ WebSocket error: {}", e);
                                        break;
                                    }
                                    None => {
                                        warn!("NapCatQQ WebSocket stream ended");
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }

                    // Clear ws_tx on disconnect
                    {
                        let mut ws_tx = self.ws_tx.lock().await;
                        *ws_tx = None;
                    }
                }
                Err(e) => {
                    // Provide detailed error message with troubleshooting hints
                    let error_hint = match &e {
                        tokio_tungstenite::tungstenite::Error::Http(response) => {
                            let status = response.status();
                            match status.as_u16() {
                                400 => "HTTP 400 Bad Request. Possible causes:\n\
                                    1. access_token mismatch (check NapCatQQ config)\n\
                                    2. Invalid WebSocket endpoint path\n\
                                    3. NapCatQQ may require a different URL format".to_string(),
                                401 => "HTTP 401 Unauthorized. access_token is missing or incorrect".to_string(),
                                403 => "HTTP 403 Forbidden. access_token is invalid".to_string(),
                                404 => format!(
                                    "HTTP 404 Not Found. The WebSocket endpoint '{}' may be incorrect.\n\
                                    NapCatQQ default WebSocket path is '/' (e.g., ws://127.0.0.1:3001)",
                                    ws_url
                                ),
                                _ => format!("HTTP {} {}",
                                    status.as_u16(),
                                    status.canonical_reason().unwrap_or("Unknown")
                                ),
                            }
                        }
                        tokio_tungstenite::tungstenite::Error::Io(io_err) => {
                            match io_err.kind() {
                                std::io::ErrorKind::ConnectionRefused => format!(
                                    "Connection refused. Is NapCatQQ running at {}?\n\
                                    Check: 1) NapCatQQ is started 2) WebSocket is enabled 3) Port is correct",
                                    ws_url
                                ),
                                std::io::ErrorKind::TimedOut => format!(
                                    "Connection timed out. Check network connectivity to {}",
                                    ws_url
                                ),
                                _ => format!("IO error: {}", io_err),
                            }
                        }
                        _ => e.to_string(),
                    };
                    error!("Failed to connect to NapCatQQ WebSocket: {}\n{}", e, error_hint);
                }
            }

            // Wait before reconnecting with exponential backoff
            info!(
                "Reconnecting to NapCatQQ WebSocket in {} seconds...",
                reconnect_delay
            );

            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(reconnect_delay as u64)) => {}
                _ = shutdown.recv() => {
                    info!("NapCatQQ WebSocket client shutting down");
                    return;
                }
            }

            // Exponential backoff
            reconnect_delay = std::cmp::min(reconnect_delay * 2, 60);
        }
    }

    /// Call an API via WebSocket and wait for response.
    pub async fn call_api(&self, request: ApiRequest) -> Result<ApiResponse> {
        let echo = request
            .echo
            .clone()
            .unwrap_or_else(|| self.next_request_id().to_string());

        let (tx, rx) = tokio::sync::oneshot::channel();

        // Register pending request
        {
            let mut pending = self.pending_requests.lock().await;
            pending.insert(echo.clone(), tx);
        }

        // Get the WebSocket sender
        let ws_tx = {
            let ws_tx_guard = self.ws_tx.lock().await;
            ws_tx_guard.clone()
        };

        let ws_tx = ws_tx.ok_or_else(|| {
            Error::Channel("WebSocket not connected".to_string())
        })?;

        // Send request
        let request_json = serde_json::to_string(&ApiRequest {
            action: request.action,
            params: request.params,
            echo: Some(echo.clone()),
        })
        .map_err(|e| Error::Channel(format!("Failed to serialize request: {}", e)))?;

        ws_tx
            .send(request_json)
            .await
            .map_err(|e| Error::Channel(format!("Failed to send WebSocket request: {}", e)))?;

        // Wait for response with timeout
        let response = tokio::time::timeout(Duration::from_secs(30), rx)
            .await
            .map_err(|_| Error::Channel("API response timeout".to_string()))?
            .map_err(|_| Error::Channel("API response channel closed".to_string()))?;

        Ok(response)
    }

    // =========================================================================
    // Convenience API Methods (WebSocket Client)
    // =========================================================================

    /// Send a private message via WebSocket.
    pub async fn send_private_msg(
        &self,
        user_id: &str,
        message: &serde_json::Value,
    ) -> Result<i64> {
        let request = ApiRequest::send_private_msg(user_id, message, None, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "send_private_msg failed: {}",
                response.error_message()
            )));
        }
        let msg_id = response
            .data
            .get("message_id")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        Ok(msg_id)
    }

    /// Send a group message via WebSocket.
    pub async fn send_group_msg(
        &self,
        group_id: &str,
        message: &serde_json::Value,
    ) -> Result<i64> {
        let request = ApiRequest::send_group_msg(group_id, message, None, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "send_group_msg failed: {}",
                response.error_message()
            )));
        }
        let msg_id = response
            .data
            .get("message_id")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        Ok(msg_id)
    }

    /// Recall a message via WebSocket.
    pub async fn delete_msg(&self, message_id: i64) -> Result<()> {
        let request = ApiRequest::delete_msg(message_id, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "delete_msg failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Get a message via WebSocket.
    pub async fn get_msg(&self, message_id: i64) -> Result<serde_json::Value> {
        let request = ApiRequest::get_msg(message_id, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_msg failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Get login info via WebSocket.
    pub async fn get_login_info(&self) -> Result<serde_json::Value> {
        let request = ApiRequest::get_login_info(None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_login_info failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Get group list via WebSocket.
    pub async fn get_group_list(&self) -> Result<Vec<serde_json::Value>> {
        let request = ApiRequest::get_group_list(None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_group_list failed: {}",
                response.error_message()
            )));
        }
        let groups: Vec<serde_json::Value> = serde_json::from_value(response.data)
            .map_err(|e| Error::Channel(format!("Failed to parse group list: {}", e)))?;
        Ok(groups)
    }

    /// Get friend list via WebSocket.
    pub async fn get_friend_list(&self) -> Result<Vec<serde_json::Value>> {
        let request = ApiRequest::get_friend_list(None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_friend_list failed: {}",
                response.error_message()
            )));
        }
        let friends: Vec<serde_json::Value> = serde_json::from_value(response.data)
            .map_err(|e| Error::Channel(format!("Failed to parse friend list: {}", e)))?;
        Ok(friends)
    }

    // =========================================================================
    // Group Management via WebSocket
    // =========================================================================

    /// Set group admin via WebSocket.
    pub async fn set_group_admin(
        &self,
        group_id: &str,
        user_id: &str,
        enable: bool,
    ) -> Result<()> {
        let request = ApiRequest::set_group_admin(group_id, user_id, enable, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_group_admin failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Set group card via WebSocket.
    pub async fn set_group_card(
        &self,
        group_id: &str,
        user_id: &str,
        card: &str,
    ) -> Result<()> {
        let request = ApiRequest::set_group_card(group_id, user_id, card, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_group_card failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Set group name via WebSocket.
    pub async fn set_group_name(&self, group_id: &str, group_name: &str) -> Result<()> {
        let request = ApiRequest::set_group_name(group_id, group_name, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_group_name failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Get group member info via WebSocket.
    pub async fn get_group_member_info(
        &self,
        group_id: &str,
        user_id: &str,
        no_cache: bool,
    ) -> Result<serde_json::Value> {
        let request = ApiRequest::get_group_member_info(group_id, user_id, no_cache, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_group_member_info failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Get group member list via WebSocket.
    pub async fn get_group_member_list(&self, group_id: &str) -> Result<Vec<serde_json::Value>> {
        let request = ApiRequest::get_group_member_list(group_id, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_group_member_list failed: {}",
                response.error_message()
            )));
        }
        let members: Vec<serde_json::Value> = serde_json::from_value(response.data)
            .map_err(|e| Error::Channel(format!("Failed to parse member list: {}", e)))?;
        Ok(members)
    }

    /// Set group kick via WebSocket.
    pub async fn set_group_kick(&self, group_id: &str, user_id: &str) -> Result<()> {
        let request = ApiRequest::set_group_kick(group_id, user_id, None, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_group_kick failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Set group ban via WebSocket.
    pub async fn set_group_ban(
        &self,
        group_id: &str,
        user_id: &str,
        duration: u32,
    ) -> Result<()> {
        let request = ApiRequest::set_group_ban(group_id, user_id, duration, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_group_ban failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Set group whole ban via WebSocket.
    pub async fn set_group_whole_ban(&self, group_id: &str, enable: bool) -> Result<()> {
        let request = ApiRequest::set_group_whole_ban(group_id, enable, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_group_whole_ban failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Leave a group via WebSocket.
    pub async fn set_group_leave(&self, group_id: &str, is_dismiss: bool) -> Result<()> {
        let request = ApiRequest::set_group_leave(group_id, is_dismiss, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_group_leave failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Set group special title via WebSocket.
    pub async fn set_group_special_title(
        &self,
        group_id: &str,
        user_id: &str,
        special_title: &str,
    ) -> Result<()> {
        let request = ApiRequest::set_group_special_title(group_id, user_id, special_title, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_group_special_title failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    // =========================================================================
    // User Info via WebSocket
    // =========================================================================

    /// Get stranger info via WebSocket.
    pub async fn get_stranger_info(
        &self,
        user_id: &str,
        no_cache: bool,
    ) -> Result<serde_json::Value> {
        let request = ApiRequest::get_stranger_info(user_id, no_cache, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_stranger_info failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Send like via WebSocket.
    pub async fn send_like(&self, user_id: &str, times: u32) -> Result<()> {
        let request = ApiRequest::send_like(user_id, times, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "send_like failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Set friend remark via WebSocket.
    pub async fn set_friend_remark(&self, user_id: &str, remark: &str) -> Result<()> {
        let request = ApiRequest::set_friend_remark(user_id, remark, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_friend_remark failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Delete friend via WebSocket.
    pub async fn delete_friend(&self, user_id: &str) -> Result<()> {
        let request = ApiRequest::delete_friend(user_id, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "delete_friend failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    // =========================================================================
    // File Operations via WebSocket
    // =========================================================================

    /// Upload group file via WebSocket.
    pub async fn upload_group_file(
        &self,
        group_id: &str,
        file: &str,
        name: Option<&str>,
    ) -> Result<serde_json::Value> {
        let request = ApiRequest::upload_group_file(group_id, file, name, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "upload_group_file failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Get group file system info via WebSocket.
    pub async fn get_group_file_system_info(
        &self,
        group_id: &str,
    ) -> Result<serde_json::Value> {
        let request = ApiRequest::get_group_file_system_info(group_id, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_group_file_system_info failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Get group files by folder via WebSocket.
    pub async fn get_group_files_by_folder(
        &self,
        group_id: &str,
        folder_id: &str,
    ) -> Result<serde_json::Value> {
        let request = ApiRequest::get_group_files_by_folder(group_id, folder_id, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_group_files_by_folder failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Delete group file via WebSocket.
    pub async fn delete_group_file(
        &self,
        group_id: &str,
        file_id: &str,
        busid: Option<i32>,
    ) -> Result<()> {
        let request = ApiRequest::delete_file(group_id, file_id, busid, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "delete_group_file failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    // =========================================================================
    // Misc Operations via WebSocket
    // =========================================================================

    /// Get status via WebSocket.
    pub async fn get_status(&self) -> Result<serde_json::Value> {
        let request = ApiRequest::get_status(None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_status failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Get version info via WebSocket.
    pub async fn get_version_info(&self) -> Result<serde_json::Value> {
        let request = ApiRequest::get_version_info(None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_version_info failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Set QQ profile via WebSocket.
    pub async fn set_qq_profile(
        &self,
        nickname: Option<&str>,
        personal_note: Option<&str>,
        sex: Option<&str>,
    ) -> Result<()> {
        let request = ApiRequest::set_qq_profile(nickname, personal_note, sex, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_qq_profile failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Get cookies via WebSocket.
    pub async fn get_cookies(&self, domain: &str) -> Result<serde_json::Value> {
        let request = ApiRequest::get_cookies(domain, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_cookies failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Get CSRF token via WebSocket.
    pub async fn get_csrf_token(&self) -> Result<serde_json::Value> {
        let request = ApiRequest::get_csrf_token(None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_csrf_token failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    // =========================================================================
    // Extended Message APIs via WebSocket
    // =========================================================================

    /// Get forward message content via WebSocket.
    pub async fn get_forward_msg(&self, message_id: &str) -> Result<serde_json::Value> {
        let request = ApiRequest::get_forward_msg(message_id, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_forward_msg failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Set message emoji like via WebSocket.
    pub async fn set_msg_emoji_like(&self, message_id: i64, emoji_id: &str) -> Result<()> {
        let request = ApiRequest::set_msg_emoji_like(message_id, emoji_id, None, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_msg_emoji_like failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Mark message as read via WebSocket.
    pub async fn mark_msg_as_read(&self, message_id: i64) -> Result<()> {
        let request = ApiRequest::mark_msg_as_read(message_id, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "mark_msg_as_read failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    // =========================================================================
    // Essence Message APIs via WebSocket
    // =========================================================================

    /// Set essence message via WebSocket.
    pub async fn set_essence_msg(&self, message_id: i64) -> Result<()> {
        let request = ApiRequest::set_essence_msg(message_id, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_essence_msg failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Delete essence message via WebSocket.
    pub async fn delete_essence_msg(&self, message_id: i64) -> Result<()> {
        let request = ApiRequest::delete_essence_msg(message_id, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "delete_essence_msg failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Get essence message list via WebSocket.
    pub async fn get_essence_msg_list(&self, group_id: &str) -> Result<Vec<serde_json::Value>> {
        let request = ApiRequest::get_essence_msg_list(group_id, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_essence_msg_list failed: {}",
                response.error_message()
            )));
        }
        let list: Vec<serde_json::Value> = serde_json::from_value(response.data)
            .map_err(|e| Error::Channel(format!("Failed to parse essence list: {}", e)))?;
        Ok(list)
    }

    // =========================================================================
    // Group Extended APIs via WebSocket
    // =========================================================================

    /// Get group at all remain count via WebSocket.
    pub async fn get_group_at_all_remain(&self, group_id: &str) -> Result<serde_json::Value> {
        let request = ApiRequest::get_group_at_all_remain(group_id, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_group_at_all_remain failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Set group portrait via WebSocket.
    pub async fn set_group_portrait(&self, group_id: &str, file: &str) -> Result<()> {
        let request = ApiRequest::set_group_portrait(group_id, file, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_group_portrait failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    // =========================================================================
    // Media/Resource APIs via WebSocket
    // =========================================================================

    /// Get image info via WebSocket.
    pub async fn get_image(&self, file: &str) -> Result<serde_json::Value> {
        let request = ApiRequest::get_image(Some(file), None, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_image failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Get record (voice) info via WebSocket.
    pub async fn get_record(&self, file: &str, out_format: &str) -> Result<serde_json::Value> {
        let request = ApiRequest::get_record(Some(file), None, out_format, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_record failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Download file via WebSocket.
    pub async fn download_file(
        &self,
        url: &str,
        thread_count: Option<i32>,
        headers: Option<&[&str]>,
    ) -> Result<serde_json::Value> {
        let request = ApiRequest::download_file(url, thread_count, headers, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "download_file failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    // =========================================================================
    // Capability Check APIs via WebSocket
    // =========================================================================

    /// Check if can send image via WebSocket.
    pub async fn can_send_image(&self) -> Result<bool> {
        let request = ApiRequest::can_send_image(None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "can_send_image failed: {}",
                response.error_message()
            )));
        }
        let yes = response
            .data
            .get("yes")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(yes)
    }

    /// Check if can send record via WebSocket.
    pub async fn can_send_record(&self) -> Result<bool> {
        let request = ApiRequest::can_send_record(None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "can_send_record failed: {}",
                response.error_message()
            )));
        }
        let yes = response
            .data
            .get("yes")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(yes)
    }

    // =========================================================================
    // Friend/Group Request Handling via WebSocket
    // =========================================================================

    /// Handle friend request via WebSocket.
    pub async fn set_friend_add_request(&self, flag: &str, approve: bool) -> Result<()> {
        let request = ApiRequest::set_friend_add_request(flag, approve, None, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_friend_add_request failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Handle group request via WebSocket.
    pub async fn set_group_add_request(
        &self,
        flag: &str,
        sub_type: &str,
        approve: bool,
    ) -> Result<()> {
        let request = ApiRequest::set_group_add_request(flag, sub_type, approve, None, None);
        let response = self.call_api(request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_group_add_request failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_user_allowed_empty_allowlist() {
        let config = Config::default();
        let (tx, _rx) = mpsc::channel(1);
        let client = NapCatWsClient::new(config, tx);

        // Empty allowlist should allow all (but blocklist is empty too)
        assert!(client.is_user_allowed("123456"));
    }

    #[test]
    fn test_is_user_allowed_blocklist() {
        let mut config = Config::default();
        config.channels.napcat.block_from = vec!["123456".to_string()];

        let (tx, _rx) = mpsc::channel(1);
        let client = NapCatWsClient::new(config, tx);

        // Blocklist should block
        assert!(!client.is_user_allowed("123456"));
        assert!(client.is_user_allowed("789012"));
    }

    #[test]
    fn test_is_user_allowed_allowlist() {
        let mut config = Config::default();
        config.channels.napcat.allow_from = vec!["123456".to_string()];

        let (tx, _rx) = mpsc::channel(1);
        let client = NapCatWsClient::new(config, tx);

        // Allowlist should only allow specified users
        assert!(client.is_user_allowed("123456"));
        assert!(!client.is_user_allowed("789012"));
    }
}