//! WebSocket server mode for NapCatQQ.
//!
//! In server mode, BlockCell acts as the WebSocket server,
//! and NapCatQQ connects to it. This is useful when NapCatQQ
//! is behind a NAT or firewall and cannot be directly connected to.
//!
//! # Configuration
//!
//! Add to your `~/.blockcell/config.json5`:
//!
//! ```json5
//! {
//!   "channels": {
//!     "napcat": {
//!       "enabled": true,
//!       "mode": "server",
//!       "serverHost": "0.0.0.0",
//!       "serverPort": 8080,
//!       "serverPath": "/onebot/v11/ws",
//!       "accessToken": "your-token"
//!     }
//!   }
//! }
//! ```

use futures::{SinkExt, StreamExt};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::{accept_hdr_async, tungstenite::Message as WsMessage};
use tracing::{error, info, warn};

use blockcell_core::{Config, Error, InboundMessage, Result};

use super::super::event::MessageEvent;
use super::sender::{init_sender, init_api_caller, init_stream_caller, OutboundMessage, ApiCallRequest, StreamCallRequest};
use super::super::types::{ApiRequest, ApiResponse, StreamChunkData};
use super::super::media::{process_media_segments, build_enhanced_content, build_media_metadata};

/// Message deduplication cache (shared with client mode).
static DEDUP_CACHE: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();

/// Maximum number of concurrent WebSocket connections.
const MAX_CONNECTIONS: usize = 100;

fn dedup_cache() -> &'static Mutex<HashSet<String>> {
    DEDUP_CACHE.get_or_init(|| Mutex::new(HashSet::new()))
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

/// Connection info for a NapCatQQ client.
#[derive(Debug)]
pub struct ConnectionInfo {
    /// Self QQ number (bot account).
    pub self_id: Option<String>,
    /// Remote address.
    pub remote_addr: String,
}

/// WebSocket server for NapCatQQ.
///
/// This server listens for incoming WebSocket connections from NapCatQQ
/// instances and handles event distribution and API calls.
pub struct NapCatWsServer {
    config: Config,
    inbound_tx: mpsc::Sender<InboundMessage>,
    request_id: AtomicU64,
    /// Pending API requests waiting for responses.
    pending_requests: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<ApiResponse>>>>,
    /// Active stream sessions (stream_id -> chunk sender).
    active_streams: Arc<Mutex<HashMap<String, mpsc::Sender<StreamChunkData>>>>,
    /// Active connections: self_id -> (ws_tx, connection_info).
    connections: Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>,
    /// Current active connection count.
    connection_count: AtomicUsize,
}

impl NapCatWsServer {
    /// Create a new WebSocket server.
    pub fn new(config: Config, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        Self {
            config,
            inbound_tx,
            request_id: AtomicU64::new(0),
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            active_streams: Arc::new(Mutex::new(HashMap::new())),
            connections: Arc::new(Mutex::new(HashMap::new())),
            connection_count: AtomicUsize::new(0),
        }
    }

    /// Generate a new request ID.
    fn next_request_id(&self) -> u64 {
        self.request_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Run the WebSocket server.
    pub async fn run(self: Arc<Self>, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        let napcat = &self.config.channels.napcat;
        let host = &napcat.server_host;
        let port = napcat.server_port;
        let server_path = &napcat.server_path;
        let access_token = &napcat.access_token;

        let bind_addr = format!("{}:{}", host, port);

        info!(
            "NapCatQQ WebSocket server starting on {} (path: {}, token: {})",
            bind_addr,
            server_path,
            if access_token.is_empty() { "none" } else { "configured" }
        );

        let listener = match TcpListener::bind(&bind_addr).await {
            Ok(l) => l,
            Err(e) => {
                error!("Failed to bind to {}: {}", bind_addr, e);
                return;
            }
        };

        info!(
            "NapCatQQ WebSocket server listening on ws://{}{}{}",
            bind_addr,
            server_path,
            if access_token.is_empty() { "" } else { "?access_token=***" }
        );

        // Initialize the global outbound sender for WebSocket mode
        // This allows outbound.rs to send messages via WebSocket
        let mut outbound_rx = init_sender();

        // Initialize the global API caller for request-response pattern
        // This allows tools to call APIs via WebSocket
        let mut api_call_rx = init_api_caller();

        // Initialize the global stream caller for streaming APIs
        let mut stream_call_rx = init_stream_caller();

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, addr)) => {
                            // Check connection limit
                            let current_count = self.connection_count.load(Ordering::SeqCst);
                            if current_count >= MAX_CONNECTIONS {
                                warn!(
                                    "Rejecting connection from {}: max connections ({}) reached",
                                    addr, MAX_CONNECTIONS
                                );
                                continue;
                            }

                            // Increment connection count
                            self.connection_count.fetch_add(1, Ordering::SeqCst);

                            let server = self.clone();
                            let server_for_cleanup = self.clone();
                            tokio::spawn(async move {
                                server.handle_connection(stream, addr.to_string()).await;
                                // Decrement connection count when done
                                server_for_cleanup.connection_count.fetch_sub(1, Ordering::SeqCst);
                            });
                        }
                        Err(e) => {
                            error!("Failed to accept connection: {}", e);
                        }
                    }
                }

                // Handle outbound messages from the global sender
                outbound_msg = outbound_rx.recv() => {
                    match outbound_msg {
                        Some(OutboundMessage { request, self_id }) => {
                            // Find the connection to send to
                            let ws_tx = if let Some(ref target_self_id) = self_id {
                                // Specific connection requested
                                let conns = self.connections.lock().await;
                                conns.get(target_self_id).cloned()
                            } else {
                                // No specific connection, use the first available
                                let conns = self.connections.lock().await;
                                conns.values().next().cloned()
                            };

                            if let Some(tx) = ws_tx {
                                // Serialize the request and send
                                match serde_json::to_string(&request) {
                                    Ok(text) => {
                                        if let Err(e) = tx.send(text).await {
                                            error!("Failed to send outbound message to connection: {}", e);
                                        }
                                    }
                                    Err(e) => {
                                        error!("Failed to serialize outbound request: {}", e);
                                    }
                                }
                            } else {
                                warn!("No WebSocket connection available for outbound message");
                            }
                        }
                        None => {
                            // Channel closed, should not happen
                            warn!("Outbound channel closed");
                        }
                    }
                }

                // Handle stream call requests from tools
                stream_call = stream_call_rx.recv() => {
                    match stream_call {
                        Some(StreamCallRequest { request, chunk_tx, done_tx }) => {
                            // Generate echo for this stream session
                            let echo = request.echo.clone()
                                .unwrap_or_else(|| format!("stream_{}", self.next_request_id()));

                            // Register the chunk sender
                            {
                                let mut active = self.active_streams.lock().await;
                                active.insert(echo.clone(), chunk_tx);
                            }

                            // Find a connection to send to
                            let ws_tx = {
                                let conns = self.connections.lock().await;
                                conns.values().next().cloned()
                            };

                            if let Some(tx) = ws_tx {
                                // Build request with echo
                                let request_with_echo = super::super::types::ApiRequest {
                                    action: request.action,
                                    params: request.params,
                                    echo: Some(echo),
                                };

                                match serde_json::to_string(&request_with_echo) {
                                    Ok(text) => {
                                        if let Err(e) = tx.send(text).await {
                                            error!("Failed to send stream call request: {}", e);
                                            let mut active = self.active_streams.lock().await;
                                            active.remove(&request.echo.unwrap_or_default());
                                            let _ = done_tx.send(Err(format!("Failed to send request: {}", e)));
                                        }
                                    }
                                    Err(e) => {
                                        error!("Failed to serialize stream call request: {}", e);
                                        let mut active = self.active_streams.lock().await;
                                        active.remove(&request.echo.unwrap_or_default());
                                        let _ = done_tx.send(Err(format!("Failed to serialize: {}", e)));
                                    }
                                }
                            } else {
                                warn!("No WebSocket connection available for stream call");
                                let mut active = self.active_streams.lock().await;
                                active.remove(&request.echo.unwrap_or_default());
                                let _ = done_tx.send(Err("No WebSocket connection available".to_string()));
                            }
                        }
                        None => {
                            warn!("Stream call channel closed");
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
                                echo: Some(echo.clone()),
                            };

                            // Find a connection to send to
                            let ws_tx = {
                                let conns = self.connections.lock().await;
                                conns.values().next().cloned()
                            };

                            if let Some(tx) = ws_tx {
                                match serde_json::to_string(&request_with_echo) {
                                    Ok(text) => {
                                        if let Err(e) = tx.send(text).await {
                                            error!("Failed to send API call request: {}", e);
                                            // Remove pending request on error
                                            let mut pending = self.pending_requests.lock().await;
                                            pending.remove(&echo);
                                        }
                                    }
                                    Err(e) => {
                                        error!("Failed to serialize API call request: {}", e);
                                        let mut pending = self.pending_requests.lock().await;
                                        pending.remove(&echo);
                                    }
                                }
                            } else {
                                warn!("No WebSocket connection available for API call");
                                let mut pending = self.pending_requests.lock().await;
                                pending.remove(&echo);
                            }
                        }
                        None => {
                            warn!("API call channel closed");
                        }
                    }
                }

                _ = shutdown.recv() => {
                    info!("NapCatQQ WebSocket server shutting down");
                    break;
                }
            }
        }
    }

    /// Handle a single WebSocket connection.
    #[allow(clippy::result_large_err)]
    async fn handle_connection(
        self: Arc<Self>,
        stream: tokio::net::TcpStream,
        remote_addr: String,
    ) {
        let mut connection_info = ConnectionInfo {
            self_id: None,
            remote_addr: remote_addr.clone(),
        };

        // Get config values for validation
        let napcat = &self.config.channels.napcat;
        let expected_token = napcat.access_token.clone();
        let expected_path = napcat.server_path.clone();

        // Callback for validating the handshake request
        // The callback must return Ok(response) to accept the connection
        // or Err(response) to reject with an error response
        let expected_path_clone = expected_path.clone();
        let expected_token_clone = expected_token.clone();
        let remote_addr_for_callback = remote_addr.clone();
        let callback = move |request: &Request, response: Response| {
            // Validate path
            let request_path = request.uri().path();
            if request_path != expected_path_clone {
                warn!(
                    "Rejecting connection: invalid path '{}' (expected '{}')",
                    request_path, expected_path_clone
                );
                // Return Err to reject the connection
                return Err(Response::builder()
                    .status(404)
                    .body(Some(format!("Not Found: expected path '{}'", expected_path_clone)))
                    .unwrap());
            }

            // Validate access token
            if !expected_token_clone.is_empty() {
                let mut token_valid = false;

                // Check Authorization header
                if let Some(auth_header) = request.headers().get("Authorization") {
                    if let Ok(auth_str) = auth_header.to_str() {
                        let token = auth_str.strip_prefix("Bearer ").unwrap_or(auth_str);
                        if token == expected_token_clone {
                            token_valid = true;
                        } else {
                            warn!("Invalid Authorization header token");
                        }
                    }
                }

                // Check access_token query parameter
                if !token_valid {
                    if let Some(query) = request.uri().query() {
                        for pair in query.split('&') {
                            if let Some((key, value)) = pair.split_once('=') {
                                if key == "access_token" && value == expected_token_clone {
                                    token_valid = true;
                                    break;
                                }
                            }
                        }
                    }
                }

                if !token_valid {
                    warn!("Rejecting connection: invalid or missing access token");
                    return Err(Response::builder()
                        .status(401)
                        .body(Some("Unauthorized".to_string()))
                        .unwrap());
                }
            }

            info!(
                "Accepting WebSocket connection from {} on path '{}'",
                remote_addr_for_callback, request_path
            );

            // Accept the connection
            Ok(response)
        };

        // Accept the WebSocket connection with header callback
        let ws_stream = match accept_hdr_async(stream, callback).await {
            Ok(ws) => ws,
            Err(e) => {
                warn!(
                    "Failed to accept WebSocket connection from {}: {}",
                    remote_addr, e
                );
                return;
            }
        };

        info!("NapCatQQ WebSocket connection established from {}", remote_addr);

        // We need to validate the access token manually since accept_hdr_async
        // doesn't provide easy access to the request. For now, we'll validate
        // by checking the first message or using HTTP header inspection.
        // A simpler approach is to check the WebSocket upgrade request path.

        let (mut write, mut read) = ws_stream.split();

        // Channel for sending messages to this connection
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<String>(100);

        // Main connection loop
        loop {
            tokio::select! {
                // Handle incoming WebSocket messages
                msg = read.next() => {
                    match msg {
                        Some(Ok(WsMessage::Text(text))) => {
                            // Parse the message to extract self_id if not yet known
                            if connection_info.self_id.is_none() {
                                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                    if let Some(self_id) = json.get("self_id").and_then(|v| v.as_i64()) {
                                        connection_info.self_id = Some(self_id.to_string());
                                        info!(
                                            "NapCatQQ connection identified: self_id={}, remote={}",
                                            self_id, remote_addr
                                        );

                                        // Register this connection
                                        let mut conns = self.connections.lock().await;
                                        conns.insert(self_id.to_string(), outbound_tx.clone());
                                    }
                                }
                            }

                            // Handle the message in a spawned task to avoid blocking
                            // the main loop (media download may take time and needs to
                            // receive API responses via the same WebSocket)
                            let server = self.clone();
                            tokio::spawn(async move {
                                server.handle_ws_message(&text).await;
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
                            info!("NapCatQQ WebSocket closed by client: {:?}", frame);
                            break;
                        }
                        Some(Err(e)) => {
                            error!("NapCatQQ WebSocket error: {}", e);
                            break;
                        }
                        None => {
                            info!("NapCatQQ WebSocket stream ended");
                            break;
                        }
                        _ => {}
                    }
                }

                // Handle outgoing messages
                msg = outbound_rx.recv() => {
                    match msg {
                        Some(text) => {
                            if let Err(e) = write.send(WsMessage::Text(text)).await {
                                error!("Failed to send WebSocket message: {}", e);
                                break;
                            }
                        }
                        None => {
                            // Channel closed
                            break;
                        }
                    }
                }
            }
        }

        // Cleanup: remove connection from registry
        if let Some(ref self_id) = connection_info.self_id {
            let mut conns = self.connections.lock().await;
            conns.remove(self_id);
            info!("NapCatQQ connection removed: self_id={}", self_id);
        }

        // Send close frame
        let _ = write.send(WsMessage::Close(None)).await;
    }

    /// Handle a WebSocket message (events from NapCatQQ).
    async fn handle_ws_message(&self, text: &str) {
        // Try to parse as event
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(text) {
            let post_type = event.get("post_type").and_then(|v| v.as_str()).unwrap_or("");

            match post_type {
                "message" => {
                    // Handle message event
                    if let Err(e) = self.handle_message_event(&event).await {
                        error!("Failed to handle message event: {}", e);
                    }
                }
                "notice" => {
                    // Notice event - not handled
                }
                "request" => {
                    // Request event - not handled
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
                    let echo_value = event.get("echo");
                    let echo = echo_value.and_then(|v| {
                        v.as_str().map(|s| s.to_string()).or_else(|| v.as_i64().map(|n| n.to_string()))
                    });

                    if let Some(echo) = echo {
                        let mut pending = self.pending_requests.lock().await;
                        if let Some(tx) = pending.remove(&echo) {
                            if let Ok(response) = serde_json::from_value::<ApiResponse>(event.clone()) {
                                let _ = tx.send(response);
                            } else {
                                warn!(echo = %echo, "Failed to parse API response");
                            }
                        } else {
                            warn!(
                                echo = %echo,
                                "API response received but no matching pending request"
                            );
                        }
                    } else {
                        warn!(echo_value = ?echo_value, "API response received but no valid echo field");
                    }
                }
                _ => {}
            }
        }
    }

    /// Handle a message event from NapCatQQ.
    async fn handle_message_event(&self, event: &serde_json::Value) -> Result<()> {
        use crate::account::napcat_account_id;
        use super::super::event::MessageEvent;

        // Parse message event
        let msg_event: MessageEvent =
            serde_json::from_value(event.clone()).map_err(|e| {
                Error::Channel(format!("Failed to parse message event: {}", e))
            })?;

        // Check for duplicate message
        let msg_id = msg_event.message_id.to_string();
        if is_duplicate(&msg_id).await {
            return Ok(());
        }

        // Check user permission
        if !self.is_user_allowed(&msg_event.user_id) {
            return Ok(());
        }

        // Check group permission for group messages
        if msg_event.is_group() {
            if let Some(ref group_id) = msg_event.group_id {
                if !self.is_group_allowed(group_id) {
                    return Ok(());
                }
            }

            // Check group response mode
            if !self.should_respond_to_group(&msg_event) {
                return Ok(());
            }
        }

        // Build chat_id first (needed for media download)
        let chat_id = if msg_event.is_group() {
            format!("group:{}", msg_event.group_id.clone().unwrap_or_default())
        } else {
            format!("user:{}", msg_event.user_id)
        };

        // Get original text content
        let original_text = if msg_event.is_group() && msg_event.is_at_me() {
            msg_event.get_text_without_at()
        } else {
            msg_event.get_text()
        };

        // Get message segments for media processing
        let segments = msg_event.message.as_segments();

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
        let mut metadata = if msg_event.is_group() {
            let group_id = msg_event.group_id.clone().unwrap_or_default();
            serde_json::json!({
                "message_id": msg_event.message_id,
                "group_id": group_id,
                "message_type": "group",
                "sender_nickname": msg_event.sender.nickname,
                "sender_card": msg_event.sender.card,
                "sender_role": msg_event.sender.role,
            })
        } else {
            serde_json::json!({
                "message_id": msg_event.message_id,
                "message_type": "private",
                "sender_nickname": msg_event.sender.nickname,
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
            sender_id: msg_event.user_id.clone(),
            chat_id,
            content,
            media,
            metadata,
            timestamp_ms: msg_event.time * 1000,
        };

        // Send to agent for processing
        self.inbound_tx
            .send(inbound)
            .await
            .map_err(|e| Error::Channel(e.to_string()))?;

        Ok(())
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

    /// Send an API request via WebSocket and wait for response.
    #[allow(dead_code)]
    pub async fn call_api(
        &self,
        self_id: &str,
        request: super::super::types::ApiRequest,
    ) -> Result<ApiResponse> {
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

        // Get connection for this self_id
        let ws_tx = {
            let conns = self.connections.lock().await;
            conns.get(self_id).cloned()
        };

        let ws_tx = ws_tx.ok_or_else(|| {
            Error::Channel(format!("No connection found for self_id: {}", self_id))
        })?;

        // Send request
        let request_json = serde_json::to_string(&super::super::types::ApiRequest {
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
    // Convenience API Methods (WebSocket)
    // =========================================================================

    /// Send a private message via WebSocket.
    pub async fn send_private_msg(
        &self,
        self_id: &str,
        user_id: &str,
        message: &serde_json::Value,
    ) -> Result<i64> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::send_private_msg(user_id, message, None, None);
        let response = self.call_api(self_id, request).await?;
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
        self_id: &str,
        group_id: &str,
        message: &serde_json::Value,
    ) -> Result<i64> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::send_group_msg(group_id, message, None, None);
        let response = self.call_api(self_id, request).await?;
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
    pub async fn delete_msg(&self, self_id: &str, message_id: i64) -> Result<()> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::delete_msg(message_id, None);
        let response = self.call_api(self_id, request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "delete_msg failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Get a message via WebSocket.
    pub async fn get_msg(&self, self_id: &str, message_id: i64) -> Result<serde_json::Value> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::get_msg(message_id, None);
        let response = self.call_api(self_id, request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_msg failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Get login info via WebSocket.
    pub async fn get_login_info(&self, self_id: &str) -> Result<serde_json::Value> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::get_login_info(None);
        let response = self.call_api(self_id, request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_login_info failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Get group list via WebSocket.
    pub async fn get_group_list(&self, self_id: &str) -> Result<Vec<serde_json::Value>> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::get_group_list(None);
        let response = self.call_api(self_id, request).await?;
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
    pub async fn get_friend_list(&self, self_id: &str) -> Result<Vec<serde_json::Value>> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::get_friend_list(None);
        let response = self.call_api(self_id, request).await?;
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
        self_id: &str,
        group_id: &str,
        user_id: &str,
        enable: bool,
    ) -> Result<()> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::set_group_admin(group_id, user_id, enable, None);
        let response = self.call_api(self_id, request).await?;
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
        self_id: &str,
        group_id: &str,
        user_id: &str,
        card: &str,
    ) -> Result<()> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::set_group_card(group_id, user_id, card, None);
        let response = self.call_api(self_id, request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_group_card failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Set group name via WebSocket.
    pub async fn set_group_name(
        &self,
        self_id: &str,
        group_id: &str,
        group_name: &str,
    ) -> Result<()> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::set_group_name(group_id, group_name, None);
        let response = self.call_api(self_id, request).await?;
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
        self_id: &str,
        group_id: &str,
        user_id: &str,
        no_cache: bool,
    ) -> Result<serde_json::Value> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::get_group_member_info(group_id, user_id, no_cache, None);
        let response = self.call_api(self_id, request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_group_member_info failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Get group member list via WebSocket.
    pub async fn get_group_member_list(
        &self,
        self_id: &str,
        group_id: &str,
    ) -> Result<Vec<serde_json::Value>> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::get_group_member_list(group_id, None);
        let response = self.call_api(self_id, request).await?;
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
    pub async fn set_group_kick(
        &self,
        self_id: &str,
        group_id: &str,
        user_id: &str,
    ) -> Result<()> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::set_group_kick(group_id, user_id, None, None);
        let response = self.call_api(self_id, request).await?;
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
        self_id: &str,
        group_id: &str,
        user_id: &str,
        duration: u32,
    ) -> Result<()> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::set_group_ban(group_id, user_id, duration, None);
        let response = self.call_api(self_id, request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_group_ban failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Set group whole ban via WebSocket.
    pub async fn set_group_whole_ban(
        &self,
        self_id: &str,
        group_id: &str,
        enable: bool,
    ) -> Result<()> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::set_group_whole_ban(group_id, enable, None);
        let response = self.call_api(self_id, request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_group_whole_ban failed: {}",
                response.error_message()
            )));
        }
        Ok(())
    }

    /// Leave a group via WebSocket.
    pub async fn set_group_leave(
        &self,
        self_id: &str,
        group_id: &str,
        is_dismiss: bool,
    ) -> Result<()> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::set_group_leave(group_id, is_dismiss, None);
        let response = self.call_api(self_id, request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "set_group_leave failed: {}",
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
        self_id: &str,
        user_id: &str,
        no_cache: bool,
    ) -> Result<serde_json::Value> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::get_stranger_info(user_id, no_cache, None);
        let response = self.call_api(self_id, request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_stranger_info failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Send like via WebSocket.
    pub async fn send_like(&self, self_id: &str, user_id: &str, times: u32) -> Result<()> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::send_like(user_id, times, None);
        let response = self.call_api(self_id, request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "send_like failed: {}",
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
        self_id: &str,
        group_id: &str,
        file: &str,
        name: Option<&str>,
    ) -> Result<serde_json::Value> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::upload_group_file(group_id, file, name, None);
        let response = self.call_api(self_id, request).await?;
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
        self_id: &str,
        group_id: &str,
    ) -> Result<serde_json::Value> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::get_group_file_system_info(group_id, None);
        let response = self.call_api(self_id, request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_group_file_system_info failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    // =========================================================================
    // Misc via WebSocket
    // =========================================================================

    /// Get status via WebSocket.
    pub async fn get_status(&self, self_id: &str) -> Result<serde_json::Value> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::get_status(None);
        let response = self.call_api(self_id, request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_status failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }

    /// Get version info via WebSocket.
    pub async fn get_version_info(&self, self_id: &str) -> Result<serde_json::Value> {
        use super::super::types::ApiRequest;
        let request = ApiRequest::get_version_info(None);
        let response = self.call_api(self_id, request).await?;
        if !response.is_success() {
            return Err(Error::Channel(format!(
                "get_version_info failed: {}",
                response.error_message()
            )));
        }
        Ok(response.data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_user_allowed_empty_allowlist() {
        let config = Config::default();
        let (tx, _rx) = mpsc::channel(1);
        let server = NapCatWsServer::new(config, tx);

        // Empty allowlist should allow all (but blocklist is empty too)
        assert!(server.is_user_allowed("123456"));
    }

    #[test]
    fn test_is_user_allowed_blocklist() {
        let mut config = Config::default();
        config.channels.napcat.block_from = vec!["123456".to_string()];

        let (tx, _rx) = mpsc::channel(1);
        let server = NapCatWsServer::new(config, tx);

        // Blocklist should block
        assert!(!server.is_user_allowed("123456"));
        assert!(server.is_user_allowed("789012"));
    }

    #[test]
    fn test_is_user_allowed_allowlist() {
        let mut config = Config::default();
        config.channels.napcat.allow_from = vec!["123456".to_string()];

        let (tx, _rx) = mpsc::channel(1);
        let server = NapCatWsServer::new(config, tx);

        // Allowlist should only allow specified users
        assert!(server.is_user_allowed("123456"));
        assert!(!server.is_user_allowed("789012"));
    }
}