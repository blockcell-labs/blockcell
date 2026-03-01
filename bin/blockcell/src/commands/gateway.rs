use blockcell_agent::{
    AgentRuntime, CapabilityRegistryAdapter, ConfirmRequest, CoreEvolutionAdapter,
    MemoryStoreAdapter, MessageBus, ProviderLLMBridge, TaskManager,
};
use blockcell_skills::{EvolutionService, EvolutionServiceConfig};
use blockcell_channels::ChannelManager;
#[cfg(feature = "telegram")]
use blockcell_channels::telegram::TelegramChannel;
#[cfg(feature = "whatsapp")]
use blockcell_channels::whatsapp::WhatsAppChannel;
#[cfg(feature = "feishu")]
use blockcell_channels::feishu::FeishuChannel;
#[cfg(feature = "slack")]
use blockcell_channels::slack::SlackChannel;
#[cfg(feature = "discord")]
use blockcell_channels::discord::DiscordChannel;
#[cfg(feature = "dingtalk")]
use blockcell_channels::dingtalk::DingTalkChannel;
#[cfg(feature = "wecom")]
use blockcell_channels::wecom::WeComChannel;
use blockcell_core::{Config, InboundMessage, Paths};
use blockcell_scheduler::{CronService, CronJob, JobSchedule, JobPayload, JobState, ScheduleKind, HeartbeatService, GhostService, GhostServiceConfig};
use blockcell_skills::{new_registry_handle, CoreEvolution};
use blockcell_storage::{MemoryStore, SessionStore};
use blockcell_tools::{CapabilityRegistryHandle, CoreEvolutionHandle, MemoryStoreHandle, ToolRegistry};
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{info, warn, error, debug};

use axum::{
    extract::{State, Path as AxumPath, Query, ws::{Message as WsMessage, WebSocket, WebSocketUpgrade}},
    http::{Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put, delete},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use rust_embed::Embed;
use tower_http::cors::CorsLayer;

// ---------------------------------------------------------------------------
// WebSocket event types for structured protocol
// ---------------------------------------------------------------------------

/// Events broadcast from runtime to all connected WebSocket clients
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum WsEvent {
    #[serde(rename = "message_done")]
    MessageDone {
        chat_id: String,
        task_id: String,
        content: String,
        tool_calls: usize,
        duration_ms: u64,
    },
    #[serde(rename = "error")]
    Error {
        chat_id: String,
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Shared state passed to HTTP/WS handlers
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct GatewayState {
    inbound_tx: mpsc::Sender<InboundMessage>,
    task_manager: TaskManager,
    config: Config,
    paths: Paths,
    api_token: Option<String>,
    /// Broadcast channel for streaming events to WebSocket clients
    ws_broadcast: broadcast::Sender<String>,
    /// Pending path-confirmation requests waiting for WebUI user response
    pending_confirms: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>>,
    /// Session store for session CRUD
    session_store: Arc<SessionStore>,
    /// Cron service for cron CRUD
    cron_service: Arc<CronService>,
    /// Memory store handle
    memory_store: Option<MemoryStoreHandle>,
    /// Tool registry for listing tools
    tool_registry: Arc<ToolRegistry>,
    /// Password for WebUI login (configured or auto-generated)
    web_password: String,
    /// Channel manager for status reporting
    channel_manager: Arc<blockcell_channels::ChannelManager>,
    /// Shared EvolutionService for trigger/delete/status handlers
    evolution_service: Arc<Mutex<EvolutionService>>,
}

fn secure_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (&x, &y) in a.as_bytes().iter().zip(b.as_bytes().iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn url_decode(input: &str) -> Option<String> {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' => {
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hi = bytes[i + 1];
                let lo = bytes[i + 2];
                let hex = |c: u8| -> Option<u8> {
                    match c {
                        b'0'..=b'9' => Some(c - b'0'),
                        b'a'..=b'f' => Some(c - b'a' + 10),
                        b'A'..=b'F' => Some(c - b'A' + 10),
                        _ => None,
                    }
                };
                let h = hex(hi)?;
                let l = hex(lo)?;
                out.push((h * 16 + l) as char);
                i += 3;
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    Some(out)
}

fn token_from_query(req: &Request<axum::body::Body>) -> Option<String> {
    let q = req.uri().query()?;
    for pair in q.split('&') {
        let (k, v) = pair.split_once('=')?;
        
        
        if k == "token" {
            return url_decode(v);
        }
    }
    None
}

fn validate_workspace_relative_path(path: &str) -> Result<std::path::PathBuf, String> {
    if path.trim().is_empty() {
        return Err("path is required".to_string());
    }
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return Err("absolute paths are not allowed".to_string());
    }
    let mut normalized = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(s) => normalized.push(s),
            std::path::Component::ParentDir => {
                return Err("path traversal (..) is not allowed".to_string());
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err("invalid path".to_string());
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err("invalid path".to_string());
    }
    Ok(normalized)
}

// ---------------------------------------------------------------------------
// Bearer token authentication middleware
// ---------------------------------------------------------------------------

async fn auth_middleware(
    State(state): State<GatewayState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let token = match &state.api_token {
        Some(t) if !t.is_empty() => t,
        _ => return next.run(req).await,
    };

    if req.uri().path() == "/v1/health" || req.uri().path() == "/v1/auth/login" {
        return next.run(req).await;
    }

    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let authorized = match auth_header {
        Some(h) if h.starts_with("Bearer ") => secure_eq(&h[7..], token.as_str()),
        _ => false,
    };

    let authorized = authorized
        || token_from_query(&req)
            .map(|v| secure_eq(&v, token.as_str()))
            .unwrap_or(false);

    if authorized {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "Unauthorized: invalid or missing Bearer token").into_response()
    }
}

// ---------------------------------------------------------------------------
// HTTP request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ChatRequest {
    content: String,
    #[serde(default = "default_channel")]
    channel: String,
    #[serde(default = "default_sender")]
    sender_id: String,
    #[serde(default = "default_chat")]
    chat_id: String,
    #[serde(default)]
    media: Vec<String>,
}

fn default_channel() -> String { "ws".to_string() }
fn default_sender() -> String { "user".to_string() }
fn default_chat() -> String { "default".to_string() }

#[derive(Serialize)]
struct ChatResponse {
    status: String,
    message: String,
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    model: String,
    uptime_secs: u64,
    version: String,
}

#[derive(Serialize)]
struct TasksResponse {
    queued: usize,
    running: usize,
    completed: usize,
    failed: usize,
    tasks: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Auth handler — login with password, returns Bearer token
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct LoginRequest {
    password: String,
}

async fn handle_login(
    State(state): State<GatewayState>,
    Json(req): Json<LoginRequest>,
) -> Response {
    if !secure_eq(&req.password, &state.web_password) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "Invalid password" }))).into_response();
    }
    // Return the api_token as the Bearer token for subsequent API requests
    match &state.api_token {
        Some(token) if !token.is_empty() => {
            Json(serde_json::json!({ "token": token })).into_response()
        }
        _ => {
            // Should never happen after the defensive guarantee above
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": "Server token not configured" }))).into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// P0 HTTP handlers — Core chat + tasks
// ---------------------------------------------------------------------------

async fn handle_chat(
    State(state): State<GatewayState>,
    Json(req): Json<ChatRequest>,
) -> impl IntoResponse {
    let inbound = InboundMessage {
        channel: req.channel,
        sender_id: req.sender_id,
        chat_id: req.chat_id,
        content: req.content,
        media: req.media,
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    match state.inbound_tx.send(inbound).await {
        Ok(_) => (
            StatusCode::ACCEPTED,
            Json(ChatResponse {
                status: "accepted".to_string(),
                message: "Message queued for processing".to_string(),
            }),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ChatResponse {
                status: "error".to_string(),
                message: format!("Failed to queue message: {}", e),
            }),
        ),
    }
}

async fn handle_health(State(state): State<GatewayState>) -> impl IntoResponse {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);

    Json(HealthResponse {
        status: "ok".to_string(),
        model: state.config.agents.defaults.model.clone(),
        uptime_secs: start.elapsed().as_secs(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

async fn handle_tasks(State(state): State<GatewayState>) -> impl IntoResponse {
    let (queued, running, completed, failed) = state.task_manager.summary().await;
    let tasks = state.task_manager.list_tasks(None).await;
    let tasks_json = serde_json::to_value(&tasks).unwrap_or(serde_json::Value::Array(vec![]));

    Json(TasksResponse {
        queued,
        running,
        completed,
        failed,
        tasks: tasks_json,
    })
}

// ---------------------------------------------------------------------------
// P0: Session management endpoints
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
struct SessionInfo {
    id: String,
    name: String,
    updated_at: String,
    message_count: usize,
}

#[derive(Deserialize)]
struct SessionsListQuery {
    limit: Option<usize>,
    cursor: Option<usize>,
}

/// GET /v1/sessions — list sessions (supports pagination)
async fn handle_sessions_list(
    State(state): State<GatewayState>,
    Query(params): Query<SessionsListQuery>,
) -> impl IntoResponse {
    let sessions_dir = state.paths.sessions_dir();
    let limit = params.limit;
    let cursor = params.cursor;

    let result = tokio::task::spawn_blocking(move || {
        let mut sessions = Vec::new();
        let meta_path = sessions_dir.join("_meta.json");
        let meta: serde_json::Map<String, serde_json::Value> = if meta_path.exists() {
            std::fs::read_to_string(&meta_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            serde_json::Map::new()
        };

        if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let file_name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();

                let updated_at = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .map(|t| {
                        let dt: chrono::DateTime<chrono::Utc> = t.into();
                        dt.to_rfc3339()
                    })
                    .unwrap_or_default();

                let message_count = std::fs::read_to_string(&path)
                    .map(|c| c.lines().filter(|l| !l.trim().is_empty()).count().saturating_sub(1))
                    .unwrap_or(0);

                let name = meta
                    .get(&file_name)
                    .and_then(|v| v.get("name"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| file_name.replace('_', ":"));

                sessions.push(SessionInfo {
                    id: file_name,
                    name,
                    updated_at,
                    message_count,
                });
            }
        }

        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        let total = sessions.len();
        let limit = limit.unwrap_or(total);
        let cursor = cursor.unwrap_or(0);

        if cursor >= total {
            return serde_json::json!({
                "sessions": [],
                "next_cursor": null,
                "total": total,
            });
        }

        let end = std::cmp::min(cursor.saturating_add(limit), total);
        let page = sessions[cursor..end].to_vec();
        let next_cursor = if end < total { Some(end) } else { None };

        serde_json::json!({
            "sessions": page,
            "next_cursor": next_cursor,
            "total": total,
        })
    })
    .await;

    match result {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({ "error": format!("Failed to list sessions: {}", e) })),
    }
}

/// GET /v1/sessions/:id — get session history
async fn handle_session_get(
    State(state): State<GatewayState>,
    AxumPath(session_id): AxumPath<String>,
) -> impl IntoResponse {
    let session_key = session_id.replace('_', ":");
    match state.session_store.load(&session_key) {
        Ok(messages) if !messages.is_empty() => {
            let msgs: Vec<serde_json::Value> = messages.iter().map(|m| {
                serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                    "tool_calls": m.tool_calls,
                    "tool_call_id": m.tool_call_id,
                    "reasoning_content": m.reasoning_content,
                })
            }).collect();
            (StatusCode::OK, Json(serde_json::json!({
                "session_id": session_id,
                "messages": msgs,
            }))).into_response()
        }
        Ok(_) => {
            (StatusCode::NOT_FOUND, Json(serde_json::json!({
                "error": "Session not found or empty"
            }))).into_response()
        }
        Err(e) => {
            (StatusCode::NOT_FOUND, Json(serde_json::json!({
                "error": format!("Session not found: {}", e)
            }))).into_response()
        }
    }
}

/// DELETE /v1/sessions/:id — delete a session
async fn handle_session_delete(
    State(state): State<GatewayState>,
    AxumPath(session_id): AxumPath<String>,
) -> impl IntoResponse {
    let session_key = session_id.replace('_', ":");
    let path = state.paths.session_file(&session_key);
    let session_id_clone = session_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        if path.exists() {
            let _ = std::fs::remove_file(&path);
            serde_json::json!({ "status": "deleted", "session_id": session_id_clone })
        } else {
            serde_json::json!({ "status": "not_found", "session_id": session_id_clone })
        }
    })
    .await;

    match result {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({ "status": "error", "message": format!("{}", e) })),
    }
}

#[derive(Deserialize)]
struct RenameRequest {
    name: String,
}

/// PUT /v1/sessions/:id/rename — rename a session (stored as metadata)
async fn handle_session_rename(
    State(state): State<GatewayState>,
    AxumPath(session_id): AxumPath<String>,
    Json(req): Json<RenameRequest>,
) -> impl IntoResponse {
    let meta_path = state.paths.sessions_dir().join("_meta.json");
    let name = req.name;
    let session_id_clone = session_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut meta: serde_json::Map<String, serde_json::Value> = if meta_path.exists() {
            std::fs::read_to_string(&meta_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            serde_json::Map::new()
        };

        meta.insert(session_id_clone.clone(), serde_json::json!({ "name": name.clone() }));

        match std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap_or_default()) {
            Ok(_) => serde_json::json!({
                "status": "ok",
                "session_id": session_id_clone,
                "name": name,
            }),
            Err(e) => serde_json::json!({ "status": "error", "message": format!("{}", e) }),
        }
    })
    .await;

    match result {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({ "status": "error", "message": format!("{}", e) })),
    }
}

// ---------------------------------------------------------------------------
// P0: WebSocket with structured protocol
// ---------------------------------------------------------------------------

async fn handle_ws_upgrade(
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

async fn handle_ws_connection(socket: WebSocket, state: GatewayState) {
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
                    let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("chat");

                    match msg_type {
                        "chat" => {
                            let content = parsed.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let chat_id = parsed.get("chat_id").and_then(|v| v.as_str()).unwrap_or("default").to_string();
                            let media: Vec<String> = parsed.get("media")
                                .and_then(|v| v.as_array())
                                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                                .unwrap_or_default();

                            let inbound = InboundMessage {
                                channel: "ws".to_string(),
                                sender_id: "user".to_string(),
                                chat_id,
                                content,
                                media,
                                metadata: serde_json::Value::Null,
                                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                            };

                            if let Err(e) = inbound_tx.send(inbound).await {
                                let _ = ws_broadcast.send(
                                    serde_json::to_string(&WsEvent::Error {
                                        chat_id: "default".to_string(),
                                        message: format!("{}", e),
                                    }).unwrap_or_default()
                                );
                                break;
                            }
                        }
                        "confirm_response" => {
                            let request_id = parsed.get("request_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let approved = parsed.get("approved").and_then(|v| v.as_bool()).unwrap_or(false);
                            if !request_id.is_empty() {
                                let mut map = state.pending_confirms.lock().await;
                                if let Some(tx) = map.remove(&request_id) {
                                    let _ = tx.send(approved);
                                    debug!(request_id = %request_id, approved, "Confirm response routed");
                                }
                            }
                        }
                        "cancel" => {
                            debug!("Received cancel via WS");
                        }
                        _ => {
                            // Fallback: treat as plain chat
                            let inbound = InboundMessage {
                                channel: "ws".to_string(),
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

// ---------------------------------------------------------------------------
// P1: Config management endpoints
// ---------------------------------------------------------------------------

/// GET /v1/config — get config (with API keys masked)
async fn handle_config_get(State(state): State<GatewayState>) -> impl IntoResponse {
    let mut config_val = serde_json::to_value(&state.config).unwrap_or_default();

    // Mask API keys
    if let Some(providers) = config_val.get_mut("providers").and_then(|v| v.as_object_mut()) {
        for (_name, provider) in providers.iter_mut() {
            if let Some(key) = provider.get_mut("apiKey").and_then(|v| v.as_str().map(|s| s.to_string())) {
                if key.len() > 4 {
                    *provider.get_mut("apiKey").unwrap() = serde_json::json!(format!("{}****", &key[..4]));
                }
            }
        }
    }

    Json(config_val)
}

#[derive(Deserialize)]
struct ConfigUpdateRequest {
    #[serde(flatten)]
    config: serde_json::Value,
}

/// PUT /v1/config — update config
async fn handle_config_update(
    State(state): State<GatewayState>,
    Json(req): Json<ConfigUpdateRequest>,
) -> impl IntoResponse {
    // Load the current config from disk to preserve masked API keys
    let config_path = state.paths.config_file();
    let current_val = serde_json::to_value(&state.config).unwrap_or_default();

    // Merge: restore masked apiKey fields from the current config
    let mut new_val = req.config.clone();
    if let (Some(new_providers), Some(cur_providers)) = (
        new_val.get_mut("providers").and_then(|v| v.as_object_mut()),
        current_val.get("providers").and_then(|v| v.as_object()),
    ) {
        for (name, new_provider) in new_providers.iter_mut() {
            if let Some(new_key) = new_provider.get("apiKey").and_then(|v| v.as_str()) {
                // If the key contains "****", it was masked — restore the real key
                if new_key.contains("****") {
                    if let Some(real_key) = cur_providers
                        .get(name)
                        .and_then(|p| p.get("apiKey"))
                    {
                        if let Some(obj) = new_provider.as_object_mut() {
                            obj.insert("apiKey".to_string(), real_key.clone());
                        }
                    }
                }
            }
        }
    }

    match serde_json::from_value::<Config>(new_val) {
        Ok(new_config) => {
            match new_config.save(&config_path) {
                Ok(_) => Json(serde_json::json!({ "status": "ok", "message": "Config updated. Restart gateway to apply changes." })),
                Err(e) => Json(serde_json::json!({ "status": "error", "message": format!("{}", e) })),
            }
        }
        Err(e) => Json(serde_json::json!({ "status": "error", "message": format!("Invalid config: {}", e) })),
    }
}

/// POST /v1/config/test-provider — test a provider connection
async fn handle_config_test_provider(
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let model = req.get("model").and_then(|v| v.as_str()).unwrap_or("gpt-3.5-turbo");
    let api_key = req.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
    let api_base = req.get("api_base").and_then(|v| v.as_str());

    if api_key.is_empty() {
        return Json(serde_json::json!({ "status": "error", "message": "api_key is required" }));
    }

    // Try a simple completion to test the connection
    let provider = blockcell_providers::OpenAIProvider::new(
        api_key,
        api_base,
        model,
        100,
        0.0,
    );

    use blockcell_providers::Provider;
    let test_messages = vec![blockcell_core::types::ChatMessage::user("Say 'ok'")];
    match provider.chat(&test_messages, &[]).await {
        Ok(_) => Json(serde_json::json!({ "status": "ok", "message": "Provider connection successful" })),
        Err(e) => Json(serde_json::json!({ "status": "error", "message": format!("{}", e) })),
    }
}
/// GET /v1/ghost/config — get ghost agent configuration
async fn handle_ghost_config_get(State(state): State<GatewayState>) -> impl IntoResponse {
    // Read from disk each time so updates via PUT take effect immediately
    // without requiring a gateway restart.
    let config_path = state.paths.config_file();
    let ghost = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Config>(&s).ok())
        .map(|c| c.agents.ghost)
        .unwrap_or_else(|| state.config.agents.ghost.clone());

    // GhostConfig has #[serde(rename_all = "camelCase")], so this serialization
    // automatically handles maxSyncsPerDay and autoSocial keys correctly.
    Json(ghost)
}

/// PUT /v1/ghost/config — update ghost agent configuration
async fn handle_ghost_config_update(
    State(state): State<GatewayState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let config_path = state.paths.config_file();
    let mut config: Config = match std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
    {
        Some(c) => c,
        None => state.config.clone(),
    };

    if let Some(v) = req.get("enabled").and_then(|v| v.as_bool()) {
        config.agents.ghost.enabled = v;
    }
    if let Some(v) = req.get("model") {
        if v.is_null() {
            config.agents.ghost.model = None;
        } else {
            config.agents.ghost.model = v.as_str().map(|s| s.to_string());
        }
    }
    if let Some(v) = req.get("schedule").and_then(|v| v.as_str()) {
        config.agents.ghost.schedule = v.to_string();
    }
    if let Some(v) = req.get("maxSyncsPerDay").and_then(|v| v.as_u64()) {
        config.agents.ghost.max_syncs_per_day = v as u32;
    }
    if let Some(v) = req.get("autoSocial").and_then(|v| v.as_bool()) {
        config.agents.ghost.auto_social = v;
    }

    match config.save(&config_path) {
        Ok(_) => Json(serde_json::json!({
            "status": "ok",
            "message": "Ghost config updated. Changes take effect on next cycle.",
            "config": config.agents.ghost,
        })),
        Err(e) => Json(serde_json::json!({ "status": "error", "message": format!("{}", e) })),
    }
}

/// GET /v1/ghost/activity — get ghost agent activity log from session files
async fn handle_ghost_activity(
    State(state): State<GatewayState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let sessions_dir = state.paths.sessions_dir();
    let limit: usize = params.get("limit").and_then(|v| v.parse().ok()).unwrap_or(20);

    let mut activities: Vec<serde_json::Value> = Vec::new();

    // Scan session files for ghost sessions (chat_id starts with "ghost_")
    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        let mut ghost_files: Vec<_> = entries
            .flatten()
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("ghost_") && n.ends_with(".jsonl"))
                    .unwrap_or(false)
            })
            .collect();

        // Sort by modification time, newest first
        ghost_files.sort_by(|a, b| {
            let ta = a.metadata().and_then(|m| m.modified()).ok();
            let tb = b.metadata().and_then(|m| m.modified()).ok();
            tb.cmp(&ta)
        });

        for entry in ghost_files.into_iter().take(limit) {
            let path = entry.path();
            let session_id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            if let Ok(content) = std::fs::read_to_string(&path) {
                let lines: Vec<&str> = content.lines().collect();
                let message_count = lines.len();

                // Extract timestamp from session_id (ghost_YYYYMMDD_HHMMSS)
                // and normalize to "YYYY-MM-DD HH:MM" for display.
                let raw_ts = session_id
                    .strip_prefix("ghost_")
                    .unwrap_or(&session_id)
                    .to_string();
                let timestamp = chrono::NaiveDateTime::parse_from_str(&raw_ts, "%Y%m%d_%H%M%S")
                    .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or(raw_ts);

                // Get first user message (the routine prompt) and last assistant message (summary)
                let mut routine_prompt = String::new();
                let mut summary = String::new();
                let mut tool_calls: Vec<String> = Vec::new();

                for line in &lines {
                    if let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) {
                        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
                        match role {
                            "user" if routine_prompt.is_empty() => {
                                routine_prompt = msg
                                    .get("content")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .chars()
                                    .take(200)
                                    .collect();
                            }
                            "assistant" => {
                                if let Some(content) = msg.get("content").and_then(|v| v.as_str()) {
                                    summary = content.chars().take(500).collect();
                                }
                                if let Some(calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                                    for call in calls {
                                        if let Some(name) = call
                                            .get("function")
                                            .and_then(|f| f.get("name"))
                                            .and_then(|n| n.as_str())
                                        {
                                            tool_calls.push(name.to_string());
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }

                activities.push(serde_json::json!({
                    "session_id": session_id,
                    "timestamp": timestamp,
                    "message_count": message_count,
                    "routine_prompt": routine_prompt,
                    "summary": summary,
                    "tool_calls": tool_calls,
                }));
            }
        }
    }

    let count = activities.len();
    Json(serde_json::json!({
        "activities": activities,
        "count": count,
    }))
}

async fn handle_ghost_model_options_get(State(state): State<GatewayState>) -> impl IntoResponse {
    let config_path = state.paths.config_file();
    let config: Config = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| state.config.clone());

    let mut providers: Vec<String> = config
        .providers
        .iter()
        .filter_map(|(name, p)| {
            if p.api_key.trim().is_empty() {
                None
            } else {
                Some(name.clone())
            }
        })
        .collect();
    providers.sort();

    Json(serde_json::json!({
        "providers": providers,
        "default_model": config.agents.defaults.model,
    }))
}

// ---------------------------------------------------------------------------
// P1: Memory management endpoints
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct MemoryQueryParams {
    q: Option<String>,
    scope: Option<String>,
    #[serde(rename = "type")]
    mem_type: Option<String>,
    limit: Option<usize>,
}

/// GET /v1/memory — search/list memories
async fn handle_memory_list(
    State(state): State<GatewayState>,
    Query(params): Query<MemoryQueryParams>,
) -> impl IntoResponse {
    let store = match &state.memory_store {
        Some(s) => s,
        None => return Json(serde_json::json!({ "error": "Memory store not available" })),
    };

    let query = serde_json::json!({
        "query": params.q.unwrap_or_default(),
        "scope": params.scope,
        "type": params.mem_type,
        "top_k": params.limit.unwrap_or(20),
    });

    match store.query_json(query) {
        Ok(result) => Json(result),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// POST /v1/memory — create/update a memory
async fn handle_memory_create(
    State(state): State<GatewayState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let store = match &state.memory_store {
        Some(s) => s,
        None => return Json(serde_json::json!({ "error": "Memory store not available" })),
    };

    match store.upsert_json(req) {
        Ok(result) => Json(result),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// DELETE /v1/memory/:id — delete a memory
async fn handle_memory_delete(
    State(state): State<GatewayState>,
    AxumPath(id): AxumPath<String>,
) -> impl IntoResponse {
    let store = match &state.memory_store {
        Some(s) => s,
        None => return Json(serde_json::json!({ "error": "Memory store not available" })),
    };

    match store.soft_delete(&id) {
        Ok(_) => Json(serde_json::json!({ "status": "deleted", "id": id })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// GET /v1/memory/stats — memory statistics
async fn handle_memory_stats(State(state): State<GatewayState>) -> impl IntoResponse {
    let store = match &state.memory_store {
        Some(s) => s,
        None => return Json(serde_json::json!({ "error": "Memory store not available" })),
    };

    match store.stats_json() {
        Ok(result) => Json(result),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

// ---------------------------------------------------------------------------
// P1: Tools / Skills / Evolution endpoints
// ---------------------------------------------------------------------------

/// GET /v1/tools — list all registered tools
async fn handle_tools(State(state): State<GatewayState>) -> impl IntoResponse {
    let names = state.tool_registry.tool_names();
    let tools: Vec<serde_json::Value> = names.iter().map(|name| {
        if let Some(tool) = state.tool_registry.get(name) {
            let schema = tool.schema();
            serde_json::json!({
                "name": schema.name,
                "description": schema.description,
            })
        } else {
            serde_json::json!({ "name": name })
        }
    }).collect();

    let count = tools.len();
    Json(serde_json::json!({
        "tools": tools,
        "count": count,
    }))
}

/// GET /v1/skills — list skills
async fn handle_skills(State(state): State<GatewayState>) -> impl IntoResponse {
    let mut skills = Vec::new();

    // Scan user skills directory
    let skills_dir = state.paths.skills_dir();
    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                let meta_path = entry.path().join("meta.yaml");
                let has_rhai = entry.path().join("SKILL.rhai").exists();
                let has_md = entry.path().join("SKILL.md").exists();

                let mut skill_info = serde_json::json!({
                    "name": name,
                    "source": "user",
                    "has_rhai": has_rhai,
                    "has_md": has_md,
                });

                if meta_path.exists() {
                    if let Ok(content) = std::fs::read_to_string(&meta_path) {
                        // meta.yaml is simple key-value; try JSON first, fallback to raw text
                        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) {
                            skill_info["meta"] = meta;
                        } else {
                            skill_info["meta"] = serde_json::Value::String(content);
                        }
                    }
                }

                skills.push(skill_info);
            }
        }
    }

    // Scan builtin skills directory
    let builtin_dir = state.paths.builtin_skills_dir();
    if let Ok(entries) = std::fs::read_dir(&builtin_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                // Skip if already in user skills
                if skills.iter().any(|s| s.get("name").and_then(|v| v.as_str()) == Some(&name)) {
                    continue;
                }
                let has_rhai = entry.path().join("SKILL.rhai").exists();
                let has_md = entry.path().join("SKILL.md").exists();
                skills.push(serde_json::json!({
                    "name": name,
                    "source": "builtin",
                    "has_rhai": has_rhai,
                    "has_md": has_md,
                }));
            }
        }
    }

    let count = skills.len();
    Json(serde_json::json!({
        "skills": skills,
        "count": count,
    }))
}

/// POST /v1/skills/search — search skills by keyword
#[derive(Deserialize)]
struct SkillSearchRequest {
    query: String,
}

async fn handle_skills_search(
    State(state): State<GatewayState>,
    Json(req): Json<SkillSearchRequest>,
) -> impl IntoResponse {
    let query = req.query.to_lowercase();
    let mut results = Vec::new();

    // Helper: check if a skill directory matches the query
    let check_skill = |dir: &std::path::Path, source: &str| -> Option<serde_json::Value> {
        let name = dir.file_name()?.to_string_lossy().to_string();
        let meta_path = dir.join("meta.yaml");
        let has_rhai = dir.join("SKILL.rhai").exists();
        let has_md = dir.join("SKILL.md").exists();

        let mut score = 0u32;
        let mut matched_fields = Vec::new();

        // Match against name
        if name.to_lowercase().contains(&query) {
            score += 10;
            matched_fields.push("name".to_string());
        }

        // Match against meta.yaml content (triggers, description, dependencies)
        let mut meta_val = serde_json::Value::Null;
        let mut description = String::new();
        let mut triggers_str = String::new();
        if meta_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&meta_path) {
                // Extract triggers
                for line in content.lines() {
                    let trimmed = line.trim().trim_start_matches("- ");
                    if trimmed.to_lowercase().contains(&query) {
                        score += 5;
                        if !matched_fields.contains(&"triggers".to_string()) {
                            matched_fields.push("triggers".to_string());
                        }
                    }
                }
                // Extract description line
                for line in content.lines() {
                    if line.starts_with("description:") {
                        description = line.trim_start_matches("description:").trim().to_string();
                        if description.to_lowercase().contains(&query) {
                            score += 8;
                            matched_fields.push("description".to_string());
                        }
                        break;
                    }
                }
                // Collect triggers for display
                let mut in_triggers = false;
                for line in content.lines() {
                    if line.starts_with("triggers:") {
                        in_triggers = true;
                        continue;
                    }
                    if in_triggers {
                        if line.starts_with("  - ") || line.starts_with("- ") {
                            let t = line.trim().trim_start_matches("- ").trim_matches('"').trim_matches('\'');
                            if !triggers_str.is_empty() { triggers_str.push_str(", "); }
                            triggers_str.push_str(t);
                        } else if !line.starts_with(' ') && !line.is_empty() {
                            in_triggers = false;
                        }
                    }
                }
                // Try parse as JSON for meta field
                if let Ok(m) = serde_json::from_str::<serde_json::Value>(&content) {
                    meta_val = m;
                }
            }
        }

        // Match against SKILL.md content (first 500 chars)
        if has_md {
            let md_path = dir.join("SKILL.md");
            if let Ok(md_content) = std::fs::read_to_string(&md_path) {
                let preview: String = md_content.chars().take(500).collect();
                if preview.to_lowercase().contains(&query) {
                    score += 3;
                    matched_fields.push("skill_md".to_string());
                }
            }
        }

        if score == 0 {
            return None;
        }

        Some(serde_json::json!({
            "name": name,
            "source": source,
            "has_rhai": has_rhai,
            "has_md": has_md,
            "description": description,
            "triggers": triggers_str,
            "score": score,
            "matched_fields": matched_fields,
            "meta": meta_val,
        }))
    };

    // Search user skills
    let skills_dir = state.paths.skills_dir();
    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                if let Some(result) = check_skill(&entry.path(), "user") {
                    results.push(result);
                }
            }
        }
    }

    // Search builtin skills
    let builtin_dir = state.paths.builtin_skills_dir();
    if let Ok(entries) = std::fs::read_dir(&builtin_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if results.iter().any(|r| r.get("name").and_then(|v| v.as_str()) == Some(&name)) {
                    continue;
                }
                if let Some(result) = check_skill(&entry.path(), "builtin") {
                    results.push(result);
                }
            }
        }
    }

    // Sort by score descending
    results.sort_by(|a, b| {
        let sa = a.get("score").and_then(|v| v.as_u64()).unwrap_or(0);
        let sb = b.get("score").and_then(|v| v.as_u64()).unwrap_or(0);
        sb.cmp(&sa)
    });

    let count = results.len();
    Json(serde_json::json!({
        "results": results,
        "count": count,
        "query": req.query,
    }))
}

/// GET /v1/evolution — list evolution records
async fn handle_evolution(State(state): State<GatewayState>) -> impl IntoResponse {
    let records_dir = state.paths.workspace().join("evolution_records");
    let mut records = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&records_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(record) = serde_json::from_str::<serde_json::Value>(&content) {
                        records.push(record);
                    }
                }
            }
        }
    }

    let count = records.len();
    Json(serde_json::json!({
        "records": records,
        "count": count,
    }))
}

/// GET /v1/evolution/:id — single evolution record detail
async fn handle_evolution_detail(
    State(state): State<GatewayState>,
    AxumPath(evolution_id): AxumPath<String>,
) -> impl IntoResponse {
    // Try skill evolution records first
    let records_dir = state.paths.workspace().join("evolution_records");
    let path = records_dir.join(format!("{}.json", evolution_id));
    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(record) = serde_json::from_str::<serde_json::Value>(&content) {
                return Json(serde_json::json!({ "record": record, "kind": "skill" }));
            }
        }
    }

    // Try tool evolution records (from CoreEvolution)
    let cap_records_dir = state.paths.workspace().join("tool_evolution_records");
    let cap_path = cap_records_dir.join(format!("{}.json", evolution_id));
    if cap_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&cap_path) {
            if let Ok(record) = serde_json::from_str::<serde_json::Value>(&content) {
                return Json(serde_json::json!({ "record": record, "kind": "tool_evolution" }));
            }
        }
    }

    Json(serde_json::json!({ "error": "not_found" }))
}

/// GET /v1/evolution/tool-evolutions — list core tool evolution records
async fn handle_evolution_tool_evolutions(State(state): State<GatewayState>) -> impl IntoResponse {
    let records_dir = state.paths.workspace().join("tool_evolution_records");
    let mut records = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&records_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(record) = serde_json::from_str::<serde_json::Value>(&content) {
                        records.push(record);
                    }
                }
            }
        }
    }

    // Sort by created_at descending
    records.sort_by(|a, b| {
        let ta = a.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0);
        let tb = b.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0);
        tb.cmp(&ta)
    });

    let count = records.len();
    Json(serde_json::json!({
        "records": records,
        "count": count,
    }))
}

#[derive(Deserialize)]
struct EvolutionTriggerRequest {
    skill_name: String,
    description: String,
}

/// POST /v1/evolution/trigger — manually trigger a skill evolution
async fn handle_evolution_trigger(
    State(state): State<GatewayState>,
    Json(req): Json<EvolutionTriggerRequest>,
) -> impl IntoResponse {
    // Use EvolutionService so active_evolutions is properly updated and tick() can drive the pipeline
    let evo = state.evolution_service.lock().await;
    match evo.trigger_manual_evolution(&req.skill_name, &req.description).await {
        Ok(evolution_id) => {
            // Broadcast WS event so WebUI refreshes immediately without waiting for 10s poll
            let event = serde_json::json!({
                "type": "evolution_triggered",
                "skill_name": req.skill_name,
                "evolution_id": evolution_id,
            });
            let _ = state.ws_broadcast.send(event.to_string());

            Json(serde_json::json!({
                "status": "triggered",
                "evolution_id": evolution_id,
            }))
        }
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "error": format!("{}", e),
        })),
    }
}

/// DELETE /v1/evolution/:id — delete a single evolution record
async fn handle_evolution_delete(
    State(state): State<GatewayState>,
    AxumPath(evolution_id): AxumPath<String>,
) -> impl IntoResponse {
    // Try skill evolution records first
    let records_dir = state.paths.workspace().join("evolution_records");
    let path = records_dir.join(format!("{}.json", evolution_id));
    if path.exists() {
        // Read skill_name before deleting so we can clean up EvolutionService state
        let skill_name = std::fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
            .and_then(|v| v.get("skill_name").and_then(|s| s.as_str()).map(|s| s.to_string()));

        return match std::fs::remove_file(&path) {
            Ok(_) => {
                // Clean up in-memory EvolutionService state so the skill can be re-triggered
                if let Some(ref sn) = skill_name {
                    let evo_guard = state.evolution_service.lock().await;
                    let _ = evo_guard.delete_records_by_skill(sn).await;
                }
                // Broadcast WS event for real-time UI refresh
                let _ = state.ws_broadcast.send(serde_json::json!({
                    "type": "evolution_deleted",
                    "id": evolution_id,
                }).to_string());
                Json(serde_json::json!({ "status": "deleted", "id": evolution_id }))
            }
            Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
        };
    }

    // Try tool evolution records
    let cap_records_dir = state.paths.workspace().join("tool_evolution_records");
    let cap_path = cap_records_dir.join(format!("{}.json", evolution_id));
    if cap_path.exists() {
        return match std::fs::remove_file(&cap_path) {
            Ok(_) => {
                let _ = state.ws_broadcast.send(serde_json::json!({
                    "type": "evolution_deleted",
                    "id": evolution_id,
                }).to_string());
                Json(serde_json::json!({ "status": "deleted", "id": evolution_id }))
            }
            Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
        };
    }

    Json(serde_json::json!({ "error": "not_found" }))
}

/// POST /v1/evolution/test — test a completed skill with input
#[derive(Deserialize)]
struct EvolutionTestRequest {
    skill_name: String,
    input: String,
}

async fn handle_evolution_test(
    State(state): State<GatewayState>,
    Json(req): Json<EvolutionTestRequest>,
) -> impl IntoResponse {
    // Check if the skill exists (has SKILL.rhai or SKILL.md)
    let skill_dir = state.paths.skills_dir().join(&req.skill_name);
    let builtin_dir = state.paths.builtin_skills_dir().join(&req.skill_name);

    let exists = skill_dir.exists() || builtin_dir.exists();
    if !exists {
        return Json(serde_json::json!({
            "error": format!("Skill '{}' not found", req.skill_name),
        }));
    }

    // Create a fresh provider for this test execution
    let provider = match AgentRuntime::create_subagent_provider(&state.config) {
        Some(p) => p,
        None => {
            return Json(serde_json::json!({
                "error": "No LLM provider configured. Check config.",
            }));
        }
    };

    // Create a temporary AgentRuntime to execute the test synchronously
    let tool_registry = ToolRegistry::with_defaults();
    let mut runtime = match AgentRuntime::new(
        state.config.clone(),
        state.paths.clone(),
        provider,
        tool_registry,
    ) {
        Ok(r) => r,
        Err(e) => {
            return Json(serde_json::json!({
                "error": format!("Failed to create test runtime: {}", e),
            }));
        }
    };

    // Wire up memory store if available
    if let Some(store) = state.memory_store.clone() {
        runtime.set_memory_store(store);
    }

    // Build the test prompt
    let test_prompt = format!(
        "[Skill Test] Please use skill `{}` to process the following input:\n{}",
        req.skill_name, req.input
    );

    let inbound = InboundMessage {
        channel: "webui_test".to_string(),
        sender_id: "webui_test".to_string(),
        chat_id: format!("test_{}", chrono::Utc::now().timestamp_millis()),
        content: test_prompt,
        media: vec![],
        metadata: serde_json::json!({
            "skill_test": true,
            "skill_name": req.skill_name,
        }),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    // Execute synchronously and return the real result
    let start = std::time::Instant::now();
    match runtime.process_message(inbound).await {
        Ok(response) => {
            let duration_ms = start.elapsed().as_millis() as u64;
            Json(serde_json::json!({
                "status": "completed",
                "skill_name": req.skill_name,
                "result": response,
                "duration_ms": duration_ms,
            }))
        }
        Err(e) => Json(serde_json::json!({
            "status": "failed",
            "skill_name": req.skill_name,
            "error": format!("{}", e),
        })),
    }
}

/// POST /v1/evolution/test-suggest — generate a test input suggestion for a skill via LLM
#[derive(Deserialize)]
struct EvolutionTestSuggestRequest {
    skill_name: String,
}

async fn handle_evolution_test_suggest(
    State(state): State<GatewayState>,
    Json(req): Json<EvolutionTestSuggestRequest>,
) -> impl IntoResponse {
    let skill_dir = state.paths.skills_dir().join(&req.skill_name);
    let builtin_dir = state.paths.builtin_skills_dir().join(&req.skill_name);

    let base_dir = if skill_dir.exists() {
        skill_dir
    } else if builtin_dir.exists() {
        builtin_dir
    } else {
        return Json(serde_json::json!({
            "error": format!("Skill '{}' not found", req.skill_name),
        }));
    };

    // Read skill context files
    let skill_md = std::fs::read_to_string(base_dir.join("SKILL.md")).unwrap_or_default();
    let meta_yaml = std::fs::read_to_string(base_dir.join("meta.yaml")).unwrap_or_default();
    let skill_rhai = std::fs::read_to_string(base_dir.join("SKILL.rhai")).ok();

    // Build a concise context for the LLM
    let mut context = format!(
        "Skill name: {}\n\n## meta.yaml\n{}\n\n## SKILL.md\n{}",
        req.skill_name, meta_yaml, skill_md
    );
    if let Some(rhai) = &skill_rhai {
        // Include first 80 lines of rhai for context (function signatures, comments)
        let rhai_preview: String = rhai.lines().take(80).collect::<Vec<_>>().join("\n");
        context.push_str(&format!("\n\n## SKILL.rhai (preview)\n{}", rhai_preview));
    }

    let system_prompt = "You are a test case generation assistant. Based on the provided skill description, generate a specific, ready-to-use test input.\n\
        Requirements:\n\
        1. Only output the test input text itself, no explanations, titles, or formatting\n\
        2. The test input should be natural language a user would actually say\n\
        3. Choose the most core functionality scenario of the skill\n\
        4. Input should be specific, including necessary parameters (e.g. city name, stock ticker)\n\
        5. Output in English";

    let user_prompt = format!(
        "Based on the following skill information, generate an appropriate test input:\n\n{}\n\nOutput the test input text directly:",
        context
    );

    // Call LLM directly for a lightweight suggestion
    use blockcell_core::types::ChatMessage;

    let provider: Box<dyn blockcell_providers::Provider> = match AgentRuntime::create_subagent_provider(&state.config) {
        Some(p) => p,
        None => {
            return Json(serde_json::json!({
                "error": "No LLM provider configured",
            }));
        }
    };

    let messages = vec![
        ChatMessage::system(system_prompt),
        ChatMessage::user(&user_prompt),
    ];

    match provider.chat(&messages, &[]).await {
        Ok(resp) => {
            let suggestion = resp.content.unwrap_or_default().trim().to_string();
            Json(serde_json::json!({
                "skill_name": req.skill_name,
                "suggestion": suggestion,
            }))
        }
        Err(e) => Json(serde_json::json!({
            "error": format!("Failed to generate suggestion: {}", e),
        })),
    }
}

/// GET /v1/evolution/versions/:skill — get version history for a skill
async fn handle_evolution_versions(
    State(state): State<GatewayState>,
    AxumPath(skill_name): AxumPath<String>,
) -> impl IntoResponse {
    let history_file = state.paths.skills_dir().join(&skill_name).join("version_history.json");
    if !history_file.exists() {
        return Json(serde_json::json!({
            "skill_name": skill_name,
            "versions": [],
            "current_version": "v1",
        }));
    }

    match std::fs::read_to_string(&history_file) {
        Ok(content) => {
            match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(history) => Json(history),
                Err(_) => Json(serde_json::json!({
                    "skill_name": skill_name,
                    "versions": [],
                    "current_version": "v1",
                })),
            }
        }
        Err(_) => Json(serde_json::json!({
            "skill_name": skill_name,
            "versions": [],
            "current_version": "v1",
        })),
    }
}

/// GET /v1/evolution/tool-versions/:id — get version history for an evolved tool
async fn handle_evolution_tool_versions(
    State(state): State<GatewayState>,
    AxumPath(capability_id): AxumPath<String>,
) -> impl IntoResponse {
    let safe_id = capability_id.replace('.', "_");
    let history_file = state.paths.workspace()
        .join("tool_versions")
        .join(format!("{}_history.json", safe_id));

    if !history_file.exists() {
        return Json(serde_json::json!({
            "capability_id": capability_id,
            "versions": [],
            "current_version": "v0",
        }));
    }

    match std::fs::read_to_string(&history_file) {
        Ok(content) => {
            match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(history) => Json(history),
                Err(_) => Json(serde_json::json!({
                    "capability_id": capability_id,
                    "versions": [],
                    "current_version": "v0",
                })),
            }
        }
        Err(_) => Json(serde_json::json!({
            "capability_id": capability_id,
            "versions": [],
            "current_version": "v0",
        })),
    }
}

/// GET /v1/evolution/summary — unified evolution summary across both systems
async fn handle_evolution_summary(State(state): State<GatewayState>) -> impl IntoResponse {
    // Skill evolution records
    let skill_records_dir = state.paths.workspace().join("evolution_records");
    let mut skill_total = 0usize;
    let mut skill_active = 0usize;
    let mut skill_completed = 0usize;
    let mut skill_failed = 0usize;

    if let Ok(entries) = std::fs::read_dir(&skill_records_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                skill_total += 1;
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(record) = serde_json::from_str::<serde_json::Value>(&content) {
                        let status = record.get("status").and_then(|s| s.as_str()).unwrap_or("");
                        match status {
                            "Completed" => skill_completed += 1,
                            "Failed" | "RolledBack" | "AuditFailed" | "CompileFailed" | "DryRunFailed" | "TestFailed" => skill_failed += 1,
                            _ => skill_active += 1,
                        }
                    }
                }
            }
        }
    }

    // Tool evolution records
    let cap_records_dir = state.paths.workspace().join("tool_evolution_records");
    let mut cap_total = 0usize;
    let mut cap_active = 0usize;
    let mut cap_completed = 0usize;
    let mut cap_failed = 0usize;

    if let Ok(entries) = std::fs::read_dir(&cap_records_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                cap_total += 1;
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(record) = serde_json::from_str::<serde_json::Value>(&content) {
                        let status = record.get("status").and_then(|s| s.as_str()).unwrap_or("");
                        match status {
                            "Active" => cap_completed += 1,
                            "Failed" | "Blocked" => cap_failed += 1,
                            _ => cap_active += 1,
                        }
                    }
                }
            }
        }
    }

    // Count registered tools from registry
    let registered_tools = state.tool_registry.tool_names().len();

    // Count user skills
    let mut user_skills = 0usize;
    let mut builtin_skills = 0usize;
    if let Ok(entries) = std::fs::read_dir(state.paths.skills_dir()) {
        for entry in entries.flatten() {
            if entry.path().is_dir() { user_skills += 1; }
        }
    }
    if let Ok(entries) = std::fs::read_dir(state.paths.builtin_skills_dir()) {
        for entry in entries.flatten() {
            if entry.path().is_dir() { builtin_skills += 1; }
        }
    }

    Json(serde_json::json!({
        "skill_evolution": {
            "total": skill_total,
            "active": skill_active,
            "completed": skill_completed,
            "failed": skill_failed,
        },
        "tool_evolution": {
            "total": cap_total,
            "active": cap_active,
            "completed": cap_completed,
            "failed": cap_failed,
        },
        "inventory": {
            "user_skills": user_skills,
            "builtin_skills": builtin_skills,
            "registered_tools": registered_tools,
        },
    }))
}

/// GET /v1/stats — runtime statistics
async fn handle_stats(State(state): State<GatewayState>) -> impl IntoResponse {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);

    let (queued, running, completed, failed) = state.task_manager.summary().await;

    // Memory items count
    let memory_items: i64 = state.memory_store.as_ref()
        .and_then(|s| s.stats_json().ok())
        .and_then(|v| v.get("total_active").and_then(|n| n.as_i64()))
        .unwrap_or(0);

    // Active tasks = queued + running
    let active_tasks = queued + running;

    Json(serde_json::json!({
        "uptime_secs": start.elapsed().as_secs(),
        "model": state.config.agents.defaults.model,
        "memory_items": memory_items,
        "active_tasks": active_tasks,
        "tasks": {
            "queued": queued,
            "running": running,
            "completed": completed,
            "failed": failed,
        },
        "tools_count": state.tool_registry.tool_names().len(),
    }))
}

// ---------------------------------------------------------------------------
// Channels status endpoint
// ---------------------------------------------------------------------------

/// GET /v1/channels/status — connection status for all configured channels
async fn handle_channels_status(State(state): State<GatewayState>) -> impl IntoResponse {
    let statuses = state.channel_manager.get_status();
    let channels: Vec<serde_json::Value> = statuses
        .into_iter()
        .map(|(name, active, detail)| {
            serde_json::json!({
                "name": name,
                "active": active,
                "detail": detail,
            })
        })
        .collect();
    Json(serde_json::json!({ "channels": channels }))
}

// ---------------------------------------------------------------------------
// Lark webhook handler (public, no auth)
// ---------------------------------------------------------------------------

/// POST /webhook/lark — receives events from Lark (international) via HTTP callback.
/// This endpoint must be publicly accessible. Configure the URL in the Lark Developer Console
/// under "Event Subscriptions" → "Request URL": https://your-domain/webhook/lark
#[cfg(feature = "lark")]
async fn handle_lark_webhook(
    State(state): State<GatewayState>,
    body: String,
) -> impl IntoResponse {
    use axum::http::StatusCode;

    if !state.config.channels.lark.enabled {
        return (StatusCode::OK, axum::Json(serde_json::json!({"code": 0}))).into_response();
    }

    match blockcell_channels::lark::process_webhook(
        &state.config,
        &body,
        Some(&state.inbound_tx),
    )
    .await
    {
        Ok(resp_json) => {
            let val: serde_json::Value = serde_json::from_str(&resp_json)
                .unwrap_or(serde_json::json!({"code": 0}));
            (StatusCode::OK, axum::Json(val)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Lark webhook processing error");
            (StatusCode::OK, axum::Json(serde_json::json!({"code": 0}))).into_response()
        }
    }
}

#[cfg(not(feature = "lark"))]
async fn handle_lark_webhook(
    State(_state): State<GatewayState>,
    _body: String,
) -> impl IntoResponse {
    axum::Json(serde_json::json!({"code": 0}))
}

// ---------------------------------------------------------------------------
// WeCom webhook handler (public, no auth)
// ---------------------------------------------------------------------------

/// GET/POST /webhook/wecom — receives events from WeCom (企业微信) via HTTP callback.
/// This endpoint must be publicly accessible. Configure the URL in the WeCom admin console
/// under "企业应用" → "接收消息" → "URL": https://your-domain/webhook/wecom
///
/// GET: URL verification (returns echostr if signature valid)
/// POST: Message/event callback
#[cfg(feature = "wecom")]
async fn handle_wecom_webhook(
    State(state): State<GatewayState>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    use axum::http::StatusCode;

    if !state.config.channels.wecom.enabled {
        return (StatusCode::OK, "success".to_string()).into_response();
    }

    let http_method = req.method().as_str().to_uppercase();
    let body = if http_method == "POST" {
        match axum::body::to_bytes(req.into_body(), 1024 * 1024).await {
            Ok(b) => String::from_utf8_lossy(&b).to_string(),
            Err(_) => String::new(),
        }
    } else {
        String::new()
    };

    let (status, body_str) = blockcell_channels::wecom::process_webhook(
        &state.config,
        &http_method,
        &query,
        &body,
        Some(&state.inbound_tx),
    )
    .await;

    (StatusCode::from_u16(status).unwrap_or(StatusCode::OK), body_str).into_response()
}

#[cfg(not(feature = "wecom"))]
async fn handle_wecom_webhook(
    State(_state): State<GatewayState>,
    axum::extract::Query(_query): axum::extract::Query<std::collections::HashMap<String, String>>,
    _req: axum::extract::Request,
) -> impl IntoResponse {
    (axum::http::StatusCode::OK, "success")
}

// ---------------------------------------------------------------------------
// P1: Cron management endpoints
// ---------------------------------------------------------------------------

/// GET /v1/cron — list all cron jobs
async fn handle_cron_list(State(state): State<GatewayState>) -> impl IntoResponse {
    // Reload from disk to get latest
    let _ = state.cron_service.load().await;
    let jobs = state.cron_service.list_jobs().await;
    let jobs_json: Vec<serde_json::Value> = jobs.iter().map(|j| {
        serde_json::to_value(j).unwrap_or_default()
    }).collect();

    let count = jobs_json.len();
    Json(serde_json::json!({
        "jobs": jobs_json,
        "count": count,
    }))
}

#[derive(Deserialize)]
struct CronCreateRequest {
    name: String,
    message: String,
    #[serde(default)]
    at_ms: Option<i64>,
    #[serde(default)]
    every_seconds: Option<i64>,
    #[serde(default)]
    cron_expr: Option<String>,
    #[serde(default)]
    skill_name: Option<String>,
    #[serde(default)]
    delete_after_run: bool,
    #[serde(default)]
    deliver: bool,
    #[serde(default)]
    deliver_channel: Option<String>,
    #[serde(default)]
    deliver_to: Option<String>,
}

/// POST /v1/cron — create a cron job
async fn handle_cron_create(
    State(state): State<GatewayState>,
    Json(req): Json<CronCreateRequest>,
) -> impl IntoResponse {
    let now_ms = chrono::Utc::now().timestamp_millis();

    let schedule = if let Some(at_ms) = req.at_ms {
        JobSchedule { kind: ScheduleKind::At, at_ms: Some(at_ms), every_ms: None, expr: None, tz: None }
    } else if let Some(every) = req.every_seconds {
        JobSchedule { kind: ScheduleKind::Every, at_ms: None, every_ms: Some(every * 1000), expr: None, tz: None }
    } else if let Some(expr) = req.cron_expr {
        JobSchedule { kind: ScheduleKind::Cron, at_ms: None, every_ms: None, expr: Some(expr), tz: None }
    } else {
        return Json(serde_json::json!({ "error": "Must specify at_ms, every_seconds, or cron_expr" }));
    };

    let payload_kind = if req.skill_name.is_some() { "skill_rhai" } else { "agent_turn" };

    let job = CronJob {
        id: uuid::Uuid::new_v4().to_string(),
        name: req.name.clone(),
        enabled: true,
        schedule,
        payload: JobPayload {
            kind: payload_kind.to_string(),
            message: req.message,
            deliver: req.deliver,
            channel: req.deliver_channel,
            to: req.deliver_to,
            skill_name: req.skill_name,
        },
        state: JobState::default(),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
        delete_after_run: req.delete_after_run,
    };

    let job_id = job.id.clone();
    match state.cron_service.add_job(job).await {
        Ok(_) => Json(serde_json::json!({ "status": "created", "job_id": job_id })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// DELETE /v1/cron/:id — delete a cron job
async fn handle_cron_delete(
    State(state): State<GatewayState>,
    AxumPath(job_id): AxumPath<String>,
) -> impl IntoResponse {
    match state.cron_service.remove_job(&job_id).await {
        Ok(true) => Json(serde_json::json!({ "status": "deleted", "job_id": job_id })),
        Ok(false) => Json(serde_json::json!({ "status": "not_found", "job_id": job_id })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// POST /v1/cron/:id/run — manually trigger a cron job
async fn handle_cron_run(
    State(state): State<GatewayState>,
    AxumPath(job_id): AxumPath<String>,
) -> impl IntoResponse {
    let jobs = state.cron_service.list_jobs().await;
    let job = jobs.iter().find(|j| j.id == job_id);

    match job {
        Some(job) => {
            let is_reminder = job.payload.kind == "agent_turn";
            let metadata = if is_reminder {
                serde_json::json!({
                    "job_id": job.id,
                    "job_name": job.name,
                    "manual_trigger": true,
                    "reminder": true,
                    "reminder_message": job.payload.message,
                })
            } else {
                serde_json::json!({
                    "job_id": job.id,
                    "job_name": job.name,
                    "manual_trigger": true,
                    "skill_rhai": true,
                    "skill_name": job.payload.skill_name,
                })
            };
            let inbound = InboundMessage {
                channel: "cron".to_string(),
                sender_id: "cron".to_string(),
                chat_id: job.id.clone(),
                content: format!("[Manual trigger] {}", job.payload.message),
                media: vec![],
                metadata,
                timestamp_ms: chrono::Utc::now().timestamp_millis(),
            };
            let _ = state.inbound_tx.send(inbound).await;
            Json(serde_json::json!({ "status": "triggered", "job_id": job.id }))
        }
        None => Json(serde_json::json!({ "status": "not_found", "job_id": job_id })),
    }
}

// ---------------------------------------------------------------------------
// Toggles: enable/disable skills and tools
// ---------------------------------------------------------------------------

/// GET /v1/toggles — get all toggle states
async fn handle_toggles_get(State(state): State<GatewayState>) -> impl IntoResponse {
    let path = state.paths.toggles_file();
    if !path.exists() {
        return Json(serde_json::json!({ "skills": {}, "tools": {} }));
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(val) => Json(val),
                Err(_) => Json(serde_json::json!({ "skills": {}, "tools": {} })),
            }
        }
        Err(_) => Json(serde_json::json!({ "skills": {}, "tools": {} })),
    }
}

#[derive(Deserialize)]
struct ToggleUpdateRequest {
    category: String,  // "skills" or "tools"
    name: String,
    enabled: bool,
}

/// PUT /v1/toggles — update a single toggle
async fn handle_toggles_update(
    State(state): State<GatewayState>,
    Json(req): Json<ToggleUpdateRequest>,
) -> impl IntoResponse {
    if req.category != "skills" && req.category != "tools" {
        return Json(serde_json::json!({ "error": "category must be 'skills' or 'tools'" }));
    }

    let path = state.paths.toggles_file();
    let mut store: serde_json::Value = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or(serde_json::json!({ "skills": {}, "tools": {} }))
    } else {
        serde_json::json!({ "skills": {}, "tools": {} })
    };

    // Ensure category object exists
    if store.get(&req.category).is_none() {
        store[&req.category] = serde_json::json!({});
    }

    // Set the toggle value. If enabled=true, remove the entry (default is enabled).
    // If enabled=false, store false explicitly.
    if req.enabled {
        if let Some(obj) = store[&req.category].as_object_mut() {
            obj.remove(&req.name);
        }
    } else {
        store[&req.category][&req.name] = serde_json::json!(false);
    }

    match std::fs::write(&path, serde_json::to_string_pretty(&store).unwrap_or_default()) {
        Ok(_) => Json(serde_json::json!({
            "status": "ok",
            "category": req.category,
            "name": req.name,
            "enabled": req.enabled,
        })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

// ---------------------------------------------------------------------------
// P2: Alert management endpoints
// ---------------------------------------------------------------------------

/// GET /v1/alerts — list all alert rules
async fn handle_alerts_list(State(state): State<GatewayState>) -> impl IntoResponse {
    let path = state.paths.workspace().join("alerts").join("rules.json");
    if !path.exists() {
        return Json(serde_json::json!({ "rules": [], "count": 0 }));
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            if let Ok(store) = serde_json::from_str::<serde_json::Value>(&content) {
                let rules = store.get("rules").cloned().unwrap_or(serde_json::json!([]));
                let count = rules.as_array().map(|a| a.len()).unwrap_or(0);
                Json(serde_json::json!({ "rules": rules, "count": count }))
            } else {
                Json(serde_json::json!({ "rules": [], "count": 0 }))
            }
        }
        Err(_) => Json(serde_json::json!({ "rules": [], "count": 0 })),
    }
}

#[derive(Deserialize)]
struct AlertCreateRequest {
    name: String,
    source: serde_json::Value,
    metric_path: String,
    operator: String,
    threshold: f64,
    #[serde(default)]
    threshold2: Option<f64>,
    #[serde(default = "default_cooldown")]
    cooldown_secs: u64,
    #[serde(default = "default_check_interval")]
    check_interval_secs: u64,
    #[serde(default)]
    notify: Option<serde_json::Value>,
    #[serde(default)]
    on_trigger: Vec<serde_json::Value>,
}

fn default_cooldown() -> u64 { 300 }
fn default_check_interval() -> u64 { 60 }

/// POST /v1/alerts — create an alert rule
async fn handle_alerts_create(
    State(state): State<GatewayState>,
    Json(req): Json<AlertCreateRequest>,
) -> impl IntoResponse {
    let alerts_dir = state.paths.workspace().join("alerts");
    let _ = std::fs::create_dir_all(&alerts_dir);
    let path = alerts_dir.join("rules.json");

    let mut store: serde_json::Value = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or(serde_json::json!({"version": 1, "rules": []}))
    } else {
        serde_json::json!({"version": 1, "rules": []})
    };

    let now = chrono::Utc::now().timestamp_millis();
    let rule_id = uuid::Uuid::new_v4().to_string();

    let new_rule = serde_json::json!({
        "id": rule_id,
        "name": req.name,
        "enabled": true,
        "source": req.source,
        "metric_path": req.metric_path,
        "operator": req.operator,
        "threshold": req.threshold,
        "threshold2": req.threshold2,
        "cooldown_secs": req.cooldown_secs,
        "check_interval_secs": req.check_interval_secs,
        "notify": req.notify.unwrap_or(serde_json::json!({"channel": "desktop"})),
        "on_trigger": req.on_trigger,
        "state": {"trigger_count": 0},
        "created_at": now,
        "updated_at": now,
    });

    if let Some(rules) = store.get_mut("rules").and_then(|v| v.as_array_mut()) {
        rules.push(new_rule);
    }

    match std::fs::write(&path, serde_json::to_string_pretty(&store).unwrap_or_default()) {
        Ok(_) => Json(serde_json::json!({ "status": "created", "rule_id": rule_id })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// PUT /v1/alerts/:id — update an alert rule
async fn handle_alerts_update(
    State(state): State<GatewayState>,
    AxumPath(rule_id): AxumPath<String>,
    Json(updates): Json<serde_json::Value>,
) -> impl IntoResponse {
    let path = state.paths.workspace().join("alerts").join("rules.json");
    if !path.exists() {
        return Json(serde_json::json!({ "error": "No alert rules found" }));
    }

    let mut store: serde_json::Value = match std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
    {
        Some(s) => s,
        None => return Json(serde_json::json!({ "error": "Failed to read alert store" })),
    };

    let mut found = false;
    if let Some(rules) = store.get_mut("rules").and_then(|v| v.as_array_mut()) {
        for rule in rules.iter_mut() {
            if rule.get("id").and_then(|v| v.as_str()) == Some(&rule_id) {
                // Merge updates into rule
                if let Some(obj) = updates.as_object() {
                    if let Some(rule_obj) = rule.as_object_mut() {
                        for (k, v) in obj {
                            if k != "id" && k != "created_at" {
                                rule_obj.insert(k.clone(), v.clone());
                            }
                        }
                        rule_obj.insert("updated_at".to_string(), serde_json::json!(chrono::Utc::now().timestamp_millis()));
                    }
                }
                found = true;
                break;
            }
        }
    }

    if !found {
        return Json(serde_json::json!({ "error": "Rule not found" }));
    }

    match std::fs::write(&path, serde_json::to_string_pretty(&store).unwrap_or_default()) {
        Ok(_) => Json(serde_json::json!({ "status": "updated", "rule_id": rule_id })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// DELETE /v1/alerts/:id — delete an alert rule
async fn handle_alerts_delete(
    State(state): State<GatewayState>,
    AxumPath(rule_id): AxumPath<String>,
) -> impl IntoResponse {
    let path = state.paths.workspace().join("alerts").join("rules.json");
    if !path.exists() {
        return Json(serde_json::json!({ "status": "not_found" }));
    }

    let mut store: serde_json::Value = match std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
    {
        Some(s) => s,
        None => return Json(serde_json::json!({ "error": "Failed to read alert store" })),
    };

    let mut found = false;
    if let Some(rules) = store.get_mut("rules").and_then(|v| v.as_array_mut()) {
        let before = rules.len();
        rules.retain(|r| r.get("id").and_then(|v| v.as_str()) != Some(&rule_id));
        found = rules.len() < before;
    }

    if !found {
        return Json(serde_json::json!({ "status": "not_found" }));
    }

    match std::fs::write(&path, serde_json::to_string_pretty(&store).unwrap_or_default()) {
        Ok(_) => Json(serde_json::json!({ "status": "deleted", "rule_id": rule_id })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// GET /v1/alerts/history — alert trigger history
async fn handle_alerts_history(State(state): State<GatewayState>) -> impl IntoResponse {
    let path = state.paths.workspace().join("alerts").join("rules.json");
    if !path.exists() {
        return Json(serde_json::json!({ "history": [] }));
    }

    let store: serde_json::Value = match std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
    {
        Some(s) => s,
        None => return Json(serde_json::json!({ "history": [] })),
    };

    // Extract trigger history from rule states
    let mut history = Vec::new();
    if let Some(rules) = store.get("rules").and_then(|v| v.as_array()) {
        for rule in rules {
            let name = rule.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
            let rule_id = rule.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let state = rule.get("state").cloned().unwrap_or_default();
            let trigger_count = state.get("trigger_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let last_triggered = state.get("last_triggered_at").and_then(|v| v.as_i64());
            let last_value = state.get("last_value").and_then(|v| v.as_f64());

            if trigger_count > 0 {
                history.push(serde_json::json!({
                    "rule_id": rule_id,
                    "name": name,
                    "trigger_count": trigger_count,
                    "last_triggered_at": last_triggered,
                    "last_value": last_value,
                    "threshold": rule.get("threshold"),
                    "operator": rule.get("operator"),
                }));
            }
        }
    }

    // Sort by last_triggered_at descending
    history.sort_by(|a, b| {
        let ta = a.get("last_triggered_at").and_then(|v| v.as_i64()).unwrap_or(0);
        let tb = b.get("last_triggered_at").and_then(|v| v.as_i64()).unwrap_or(0);
        tb.cmp(&ta)
    });

    Json(serde_json::json!({ "history": history }))
}

// ---------------------------------------------------------------------------
// P2: Stream management endpoints
// ---------------------------------------------------------------------------

/// GET /v1/streams — list active stream subscriptions
async fn handle_streams_list() -> impl IntoResponse {
    let data = blockcell_tools::stream_subscribe::list_streams().await;
    Json(data)
}

#[derive(Deserialize)]
struct StreamDataQuery {
    #[serde(default = "default_stream_limit")]
    limit: usize,
}

fn default_stream_limit() -> usize { 50 }

/// GET /v1/streams/:id/data — get buffered data for a stream
async fn handle_stream_data(
    AxumPath(stream_id): AxumPath<String>,
    Query(params): Query<StreamDataQuery>,
) -> impl IntoResponse {
    match blockcell_tools::stream_subscribe::get_stream_data(&stream_id, params.limit).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

// ---------------------------------------------------------------------------
// P2: File management endpoints
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct FileListQuery {
    #[serde(default = "default_file_path")]
    path: String,
}

fn default_file_path() -> String { ".".to_string() }

/// GET /v1/files — list directory contents
async fn handle_files_list(
    State(state): State<GatewayState>,
    Query(params): Query<FileListQuery>,
) -> impl IntoResponse {
    let workspace = state.paths.workspace();
    let target = if params.path == "." || params.path.is_empty() {
        workspace.to_path_buf()
    } else {
        workspace.join(&params.path)
    };

    // Security: ensure path is within workspace
    let canonical = match target.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            if !target.exists() {
                return Json(serde_json::json!({ "error": "Path not found" }));
            }
            target.clone()
        }
    };
    let ws_canonical = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    if !canonical.starts_with(&ws_canonical) {
        return Json(serde_json::json!({ "error": "Access denied: path outside workspace" }));
    }

    if !target.is_dir() {
        return Json(serde_json::json!({ "error": "Not a directory" }));
    }

    let mut entries = Vec::new();
    if let Ok(dir) = std::fs::read_dir(&target) {
        for entry in dir.flatten() {
            let meta = entry.metadata().ok();
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let modified = meta.as_ref()
                .and_then(|m| m.modified().ok())
                .map(|t| {
                    let dt: chrono::DateTime<chrono::Utc> = t.into();
                    dt.to_rfc3339()
                });

            // Relative path from workspace
            let rel_path = entry.path()
                .strip_prefix(&workspace)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| name.clone());

            let ext = entry.path().extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_lowercase())
                .unwrap_or_default();

            let file_type = if is_dir {
                "directory".to_string()
            } else {
                match ext.as_str() {
                    "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" => "image",
                    "mp3" | "wav" | "m4a" | "flac" | "ogg" => "audio",
                    "mp4" | "mkv" | "webm" | "avi" => "video",
                    "pdf" => "pdf",
                    "json" | "jsonl" => "json",
                    "md" | "txt" | "log" | "csv" | "yaml" | "yml" | "toml" | "xml" | "html" | "css" | "js" | "ts" | "py" | "rs" | "sh" | "rhai" => "text",
                    "xlsx" | "xls" | "docx" | "pptx" => "office",
                    "zip" | "tar" | "gz" | "tgz" => "archive",
                    "db" | "sqlite" => "database",
                    _ => "file",
                }.to_string()
            };

            entries.push(serde_json::json!({
                "name": name,
                "path": rel_path,
                "is_dir": is_dir,
                "size": size,
                "type": file_type,
                "modified": modified,
            }));
        }
    }

    // Sort: directories first, then by name
    entries.sort_by(|a, b| {
        let a_dir = a.get("is_dir").and_then(|v| v.as_bool()).unwrap_or(false);
        let b_dir = b.get("is_dir").and_then(|v| v.as_bool()).unwrap_or(false);
        match (b_dir, a_dir) {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            _ => {
                let a_name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let b_name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
                a_name.cmp(b_name)
            }
        }
    });

    let count = entries.len();
    Json(serde_json::json!({
        "path": params.path,
        "entries": entries,
        "count": count,
    }))
}

#[derive(Deserialize)]
struct FileContentQuery {
    path: String,
}

/// GET /v1/files/content — read file content
async fn handle_files_content(
    State(state): State<GatewayState>,
    Query(params): Query<FileContentQuery>,
) -> Response {
    let workspace = state.paths.workspace();
    let target = workspace.join(&params.path);

    // Security check
    let canonical = match target.canonicalize() {
        Ok(p) => p,
        Err(_) => return (StatusCode::NOT_FOUND, "File not found").into_response(),
    };
    let ws_canonical = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    if !canonical.starts_with(&ws_canonical) {
        return (StatusCode::FORBIDDEN, "Access denied").into_response();
    }

    if !target.is_file() {
        return (StatusCode::NOT_FOUND, "Not a file").into_response();
    }

    let ext = target.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    // For binary files (images, etc.), return base64 encoded
    let is_binary = matches!(ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" |
        "mp3" | "wav" | "m4a" | "mp4" | "mkv" | "webm" |
        "pdf" | "xlsx" | "xls" | "docx" | "pptx" |
        "zip" | "tar" | "gz" | "db" | "sqlite"
    );

    let mime_type = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "json" | "jsonl" => "application/json",
        "html" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        _ => if is_binary { "application/octet-stream" } else { "text/plain" },
    };

    if is_binary {
        match std::fs::read(&target) {
            Ok(bytes) => {
                use base64::Engine;
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                Json(serde_json::json!({
                    "path": params.path,
                    "encoding": "base64",
                    "mime_type": mime_type,
                    "size": bytes.len(),
                    "content": b64,
                })).into_response()
            }
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Read error: {}", e)).into_response(),
        }
    } else {
        match std::fs::read_to_string(&target) {
            Ok(content) => {
                Json(serde_json::json!({
                    "path": params.path,
                    "encoding": "utf-8",
                    "mime_type": mime_type,
                    "size": content.len(),
                    "content": content,
                })).into_response()
            }
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Read error: {}", e)).into_response(),
        }
    }
}

/// GET /v1/files/download — download a file
async fn handle_files_download(
    State(state): State<GatewayState>,
    Query(params): Query<FileContentQuery>,
) -> Response {
    let workspace = state.paths.workspace();
    let target = workspace.join(&params.path);

    let canonical = match target.canonicalize() {
        Ok(p) => p,
        Err(_) => return (StatusCode::NOT_FOUND, "File not found").into_response(),
    };
    let ws_canonical = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    if !canonical.starts_with(&ws_canonical) {
        return (StatusCode::FORBIDDEN, "Access denied").into_response();
    }

    match std::fs::read(&target) {
        Ok(bytes) => {
            let filename = target.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("download");
            let headers = [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", filename)),
            ];
            (headers, bytes).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Read error: {}", e)).into_response(),
    }
}

/// GET /v1/files/serve — serve a file inline with proper Content-Type (for <img>/<audio> tags)
/// Supports both workspace-relative paths and absolute paths within ~/.blockcell/
async fn handle_files_serve(
    State(state): State<GatewayState>,
    Query(params): Query<FileContentQuery>,
) -> Response {
    let base_dir = state.paths.base.clone();
    let workspace = state.paths.workspace();

    // Determine target: absolute path or workspace-relative
    let target = if params.path.starts_with('/') {
        std::path::PathBuf::from(&params.path)
    } else {
        workspace.join(&params.path)
    };

    // Canonicalize for security check
    let canonical = match target.canonicalize() {
        Ok(p) => p,
        Err(_) => return (StatusCode::NOT_FOUND, "File not found").into_response(),
    };

    // Security: file must be within ~/.blockcell/ base directory
    let base_canonical = base_dir.canonicalize().unwrap_or_else(|_| base_dir.to_path_buf());
    if !canonical.starts_with(&base_canonical) {
        return (StatusCode::FORBIDDEN, "Access denied: file outside allowed directory").into_response();
    }

    if !target.is_file() {
        return (StatusCode::NOT_FOUND, "Not a file").into_response();
    }

    let ext = target.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    let content_type = match ext.as_str() {
        // Images
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "heic" | "heif" => "image/heic",
        "tiff" | "tif" => "image/tiff",
        // Audio
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" | "aac" => "audio/aac",
        "ogg" | "oga" => "audio/ogg",
        "flac" => "audio/flac",
        "opus" => "audio/opus",
        "weba" => "audio/webm",
        // Video
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        // Other
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    };

    match std::fs::read(&target) {
        Ok(bytes) => {
            let headers = [
                (header::CONTENT_TYPE, content_type.to_string()),
                (header::CACHE_CONTROL, "public, max-age=3600".to_string()),
            ];
            (headers, bytes).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Read error: {}", e)).into_response(),
    }
}

/// POST /v1/files/upload — upload a file to workspace
async fn handle_files_upload(
    State(state): State<GatewayState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let path = req.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let content = req.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let encoding = req.get("encoding").and_then(|v| v.as_str()).unwrap_or("utf-8");

    let rel = match validate_workspace_relative_path(path) {
        Ok(p) => p,
        Err(e) => return Json(serde_json::json!({ "error": e })),
    };

    let workspace = state.paths.workspace();
    let target = workspace.join(&rel);
    let path_echo = rel.to_string_lossy().to_string();
    let content = content.to_string();
    let encoding = encoding.to_string();

    let result = tokio::task::spawn_blocking(move || {
        if let Some(parent) = target.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Err(format!("{}", e));
            }
        }

        if encoding == "base64" {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(content.as_bytes())
                .map_err(|e| format!("Base64 decode error: {}", e))?;
            std::fs::write(&target, bytes).map_err(|e| format!("{}", e))?;
        } else {
            std::fs::write(&target, content).map_err(|e| format!("{}", e))?;
        }
        Ok(())
    })
    .await;

    match result {
        Ok(Ok(_)) => Json(serde_json::json!({ "status": "uploaded", "path": path_echo })),
        Ok(Err(e)) => Json(serde_json::json!({ "error": e })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

// ---------------------------------------------------------------------------
// Outbound → WebSocket broadcast bridge
// ---------------------------------------------------------------------------

/// Forwards outbound messages from the runtime to all connected WebSocket clients
async fn outbound_to_ws_bridge(
    mut outbound_rx: mpsc::Receiver<blockcell_core::OutboundMessage>,
    ws_broadcast: broadcast::Sender<String>,
    channel_manager: Arc<ChannelManager>,
    mut shutdown_rx: broadcast::Receiver<()>,
) {
    loop {
        tokio::select! {
            msg = outbound_rx.recv() => {
                let Some(msg) = msg else { break };
                // Forward to WebSocket clients as a message_done event.
                // Skip "ws" channel — the runtime already emits events directly via event_tx.
                // Still forward cron, subagent, and other internal channel results to WS clients.
                if msg.channel != "ws" {
                    let event = WsEvent::MessageDone {
                        chat_id: msg.chat_id.clone(),
                        task_id: String::new(),
                        content: msg.content.clone(),
                        tool_calls: 0,
                        duration_ms: 0,
                    };
                    if let Ok(json) = serde_json::to_string(&event) {
                        let _ = ws_broadcast.send(json);
                    }
                }

                // Also dispatch to external channels (telegram, slack, etc.)
                if msg.channel != "ws" && msg.channel != "cli" && msg.channel != "http" {
                    if let Err(e) = channel_manager.dispatch_outbound_msg(&msg).await {
                        error!(error = %e, channel = %msg.channel, "Failed to dispatch outbound message");
                    }
                }
            }
            _ = shutdown_rx.recv() => {
                debug!("outbound_to_ws_bridge received shutdown signal");
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Embedded WebUI static files
// ---------------------------------------------------------------------------

#[derive(Embed)]
#[folder = "../../webui/dist"]
struct WebUiAssets;

async fn handle_webui_static(uri: axum::http::Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    // Try the exact path first, then fall back to index.html for SPA routing
    let file_path = if path.is_empty() { "index.html" } else { path };

    match WebUiAssets::get(file_path) {
        Some(content) => {
            let mime = mime_guess::from_path(file_path)
                .first_or_octet_stream()
                .to_string();
            let body: Vec<u8> = content.data.into();
            // index.html must never be cached: a stale index.html that references
            // old hashed JS/CSS bundle filenames causes a blank page after rebuild.
            // Hashed assets (/assets/*.js, /assets/*.css) are safe to cache forever.
            let cache_control = if file_path == "index.html" {
                "no-store, no-cache, must-revalidate"
            } else if file_path.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "public, max-age=3600"
            };
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime),
                    (header::CACHE_CONTROL, cache_control.to_string()),
                ],
                body,
            )
                .into_response()
        }
        None => {
            // SPA fallback: serve index.html for any unknown route
            match WebUiAssets::get("index.html") {
                Some(content) => {
                    let body: Vec<u8> = content.data.into();
                    (
                        StatusCode::OK,
                        [
                            (header::CONTENT_TYPE, "text/html".to_string()),
                            (header::CACHE_CONTROL, "no-store, no-cache, must-revalidate".to_string()),
                        ],
                        body,
                    )
                        .into_response()
                }
                None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Startup banner — colored, boxed output for key information
// ---------------------------------------------------------------------------

/// ANSI color helpers
mod ansi {
    pub const RESET: &str = "\x1b[0m";
    pub const BOLD: &str = "\x1b[1m";
    pub const DIM: &str = "\x1b[2m";
    pub const GREEN: &str = "\x1b[32m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const CYAN: &str = "\x1b[36m";
    pub const WHITE: &str = "\x1b[97m";
    pub const BG_YELLOW: &str = "\x1b[43m";
    // 24-bit true-color matching the Logo.tsx palette
    pub const ORANGE: &str = "\x1b[38;2;234;88;12m";   // #ea580c
    pub const NEON_GREEN: &str = "\x1b[38;2;0;255;157m"; // #00ff9d
}

fn rand_u32() -> u32 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut h);
    std::process::id().hash(&mut h);
    h.finish() as u32
}

fn print_startup_banner(
    config: &Config,
    host: &str,
    webui_host: &str,
    webui_port: u16,
    web_password: &str,
    webui_pass_is_temp: bool,
    is_exposed: bool,
    bind_addr: &str,
) {
    let ver = env!("CARGO_PKG_VERSION");
    let model = &config.agents.defaults.model;

    // ── Logo + Header ──
    eprintln!();
    //  Layered hexagon logo — bold & colorful (matches Logo.tsx)
    let o = ansi::ORANGE;
    let g = ansi::NEON_GREEN;
    let r = ansi::RESET;

    eprintln!("           {o}▄▄▄▄▄▄▄{r}");
    eprintln!("       {o}▄█████████████▄{r}");
    eprintln!("     {o}▄████▀▀     ▀▀████▄{r}      {o}▄▄{r}");
    eprintln!("    {o}▐███▀{r}   {g}█████{r}   {o}▀███▌{r}    {o}████{r}");
    eprintln!("    {o}▐███{r}    {g}█████{r}    {o}███▌{r}     {o}▀▀{r}");
    eprintln!("    {o}▐███{r}    {g}█████{r}    {o}███▌{r}");
    eprintln!("    {o}▐███{r}    {g}█████{r}    {o}███▌{r}");
    eprintln!("    {o}▐███▄{r}   {g}▀▀▀▀▀{r}   {o}▄███▌{r}");
    eprintln!("     {o}▀████▄▄     ▄▄████▀{r}");
    eprintln!("   {o}▄▄{r}  {o}▀█████████████▀{r}");
    eprintln!("  {o}████{r}     {o}▀▀▀▀▀▀▀{r}");
    eprintln!("   {o}▀▀{r}");
    eprintln!();
    eprintln!(
        "  {}{}  BLOCKCELL GATEWAY v{}  {}",
        ansi::BOLD, ansi::CYAN, ver, ansi::RESET
    );
    eprintln!(
        "  {}Model: {}{}",
        ansi::DIM, model, ansi::RESET
    );
    eprintln!();

    // ── WebUI Password box ──
    let box_w = 62;
    if webui_pass_is_temp {
        // Temp password — show prominently, warn it changes each restart
        eprintln!("  {}┌{}┐{}", ansi::YELLOW, "─".repeat(box_w), ansi::RESET);
        let pw_label = "🔑 WebUI Password: ";
        let pw_visible = 2 + display_width(pw_label) + web_password.len();
        let pw_pad = box_w.saturating_sub(pw_visible);
        eprintln!(
            "  {}│{}  {}{}{}{}{}{}│",
            ansi::YELLOW, ansi::RESET,
            ansi::BOLD, ansi::YELLOW,
            pw_label, web_password,
            ansi::RESET,
            " ".repeat(pw_pad),
        );
        let hint1 = "  Temporary — changes every restart. Set gateway.webuiPass";
        let hint1_pad = box_w.saturating_sub(hint1.len());
        eprintln!(
            "  {}│{}  {}Temporary — changes every restart. Set gateway.webuiPass{}{}{}│{}",
            ansi::YELLOW, ansi::RESET,
            ansi::DIM, ansi::RESET,
            " ".repeat(hint1_pad),
            ansi::YELLOW, ansi::RESET,
        );
        let hint2 = "  in config.json for a stable password.";
        let hint2_pad = box_w.saturating_sub(hint2.len());
        eprintln!(
            "  {}│{}  {}in config.json for a stable password.{}{}{}│{}",
            ansi::YELLOW, ansi::RESET,
            ansi::DIM, ansi::RESET,
            " ".repeat(hint2_pad),
            ansi::YELLOW, ansi::RESET,
        );
        eprintln!("  {}└{}┘{}", ansi::YELLOW, "─".repeat(box_w), ansi::RESET);
    } else {
        // Configured stable password
        eprintln!("  {}┌{}┐{}", ansi::GREEN, "─".repeat(box_w), ansi::RESET);
        let pw_label = "🔑 WebUI Password: ";
        let pw_visible = 2 + display_width(pw_label) + web_password.len();
        let pw_pad = box_w.saturating_sub(pw_visible);
        eprintln!(
            "  {}│{}  {}{}{}{}{}{}│",
            ansi::GREEN, ansi::RESET,
            ansi::BOLD, ansi::GREEN,
            pw_label, web_password,
            ansi::RESET,
            " ".repeat(pw_pad),
        );
        let hint = "  Configured via gateway.webuiPass in config.json";
        let hint_pad = box_w.saturating_sub(hint.len());
        eprintln!(
            "  {}│{}  {}Configured via gateway.webuiPass in config.json{}{}{}│{}",
            ansi::GREEN, ansi::RESET,
            ansi::DIM, ansi::RESET,
            " ".repeat(hint_pad),
            ansi::GREEN, ansi::RESET,
        );
        eprintln!("  {}└{}┘{}", ansi::GREEN, "─".repeat(box_w), ansi::RESET);
    }
    eprintln!();

    // ── Security warning ──
    if is_exposed && webui_pass_is_temp {
        eprintln!(
            "  {}{}⚠  SECURITY: Binding to {} with an auto-generated token.{}",
            ansi::BG_YELLOW, ansi::BOLD, host, ansi::RESET
        );
        eprintln!(
            "  {}   Review gateway.apiToken in config.json before exposing to the network.{}",
            ansi::YELLOW, ansi::RESET
        );
        eprintln!();
    }

    // ── Channels status ──
    eprintln!(
        "  {}{}Channels{}",
        ansi::BOLD, ansi::WHITE, ansi::RESET
    );

    let ch = &config.channels;

    struct ChannelInfo {
        name: &'static str,
        enabled: bool,
        configured: bool,
        detail: String,
    }

    let channels = vec![
        ChannelInfo {
            name: "Telegram",
            enabled: ch.telegram.enabled,
            configured: !ch.telegram.token.is_empty(),
            detail: if ch.telegram.enabled && !ch.telegram.token.is_empty() {
                format!("allow_from: {:?}", ch.telegram.allow_from)
            } else if !ch.telegram.token.is_empty() {
                "token set but not enabled".into()
            } else {
                "no token configured".into()
            },
        },
        ChannelInfo {
            name: "Slack",
            enabled: ch.slack.enabled,
            configured: !ch.slack.bot_token.is_empty(),
            detail: if ch.slack.enabled && !ch.slack.bot_token.is_empty() {
                format!("channels: {:?}", ch.slack.channels)
            } else if !ch.slack.bot_token.is_empty() {
                "bot_token set but not enabled".into()
            } else {
                "no bot_token configured".into()
            },
        },
        ChannelInfo {
            name: "Discord",
            enabled: ch.discord.enabled,
            configured: !ch.discord.bot_token.is_empty(),
            detail: if ch.discord.enabled && !ch.discord.bot_token.is_empty() {
                format!("channels: {:?}", ch.discord.channels)
            } else if !ch.discord.bot_token.is_empty() {
                "bot_token set but not enabled".into()
            } else {
                "no bot_token configured".into()
            },
        },
        ChannelInfo {
            name: "Feishu",
            enabled: ch.feishu.enabled,
            configured: !ch.feishu.app_id.is_empty(),
            detail: if ch.feishu.enabled && !ch.feishu.app_id.is_empty() {
                "connected".into()
            } else if !ch.feishu.app_id.is_empty() {
                "app_id set but not enabled".into()
            } else {
                "no app_id configured".into()
            },
        },
        ChannelInfo {
            name: "Lark",
            enabled: ch.lark.enabled,
            configured: !ch.lark.app_id.is_empty(),
            detail: if ch.lark.enabled && !ch.lark.app_id.is_empty() {
                "webhook: POST /webhook/lark".into()
            } else if !ch.lark.app_id.is_empty() {
                "app_id set but not enabled".into()
            } else {
                "no app_id configured".into()
            },
        },
        ChannelInfo {
            name: "DingTalk",
            enabled: ch.dingtalk.enabled,
            configured: !ch.dingtalk.app_key.is_empty(),
            detail: if ch.dingtalk.enabled && !ch.dingtalk.app_key.is_empty() {
                format!("robot_code: {}", ch.dingtalk.robot_code)
            } else if !ch.dingtalk.app_key.is_empty() {
                "app_key set but not enabled".into()
            } else {
                "no app_key configured".into()
            },
        },
        ChannelInfo {
            name: "WeCom",
            enabled: ch.wecom.enabled,
            configured: !ch.wecom.corp_id.is_empty(),
            detail: if ch.wecom.enabled && !ch.wecom.corp_id.is_empty() {
                format!("agent_id: {}", ch.wecom.agent_id)
            } else if !ch.wecom.corp_id.is_empty() {
                "corp_id set but not enabled".into()
            } else {
                "no corp_id configured".into()
            },
        },
        ChannelInfo {
            name: "WhatsApp",
            enabled: ch.whatsapp.enabled,
            configured: true, // always has default bridge_url
            detail: if ch.whatsapp.enabled {
                format!("bridge: {}", ch.whatsapp.bridge_url)
            } else {
                "not enabled".into()
            },
        },
    ];

    // Enabled channels box (green)
    let enabled: Vec<&ChannelInfo> = channels.iter().filter(|c| c.enabled && c.configured).collect();
    if !enabled.is_empty() {
        let box_w = 62;
        eprintln!("  {}┌{}┐{}", ansi::GREEN, "─".repeat(box_w), ansi::RESET);
        for ch_info in &enabled {
            let line = format!("  ● {}  {}", ch_info.name, ch_info.detail);
            let pad = box_w.saturating_sub(display_width(&line));
            eprintln!(
                "  {}│{} {}{}● {}{} {}{}{}│{}",
                ansi::GREEN, ansi::RESET,
                ansi::BOLD, ansi::GREEN,
                ch_info.name,
                ansi::RESET,
                ch_info.detail,
                " ".repeat(pad),
                ansi::GREEN, ansi::RESET,
            );
        }
        eprintln!("  {}└{}┘{}", ansi::GREEN, "─".repeat(box_w), ansi::RESET);
    }

    // Disabled/unconfigured channels (dim, no box)
    let disabled: Vec<&ChannelInfo> = channels.iter().filter(|c| !c.enabled || !c.configured).collect();
    if !disabled.is_empty() {
        for ch_info in &disabled {
            eprintln!(
                "  {}  ○ {}  — {}{}",
                ansi::DIM,
                ch_info.name,
                ch_info.detail,
                ansi::RESET,
            );
        }
    }

    if channels.iter().all(|c| !c.enabled) {
        eprintln!(
            "  {}  No channels enabled. WebSocket is the only input.{}",
            ansi::DIM, ansi::RESET,
        );
    }
    eprintln!();

    // ── Server info ──
    eprintln!(
        "  {}{}Server{}",
        ansi::BOLD, ansi::WHITE, ansi::RESET
    );
    eprintln!(
        "  {}HTTP/WS:{}  http://{}",
        ansi::CYAN, ansi::RESET, bind_addr,
    );
    eprintln!(
        "  {}WebUI:{}   http://{}:{}/",
        ansi::CYAN, ansi::RESET,
        webui_host,
        webui_port,
    );
    eprintln!(
        "  {}API:{}     POST http://{}/v1/chat  |  GET /v1/health  |  GET /v1/ws",
        ansi::CYAN, ansi::RESET, bind_addr,
    );
    eprintln!();

    // ── Ready ──
    eprintln!(
        "  {}{}✓ Gateway ready.{} Press {}Ctrl+C{} to stop.",
        ansi::BOLD, ansi::GREEN, ansi::RESET,
        ansi::BOLD, ansi::RESET,
    );
    eprintln!();
}

/// Calculate the visible display width of a string (ignoring ANSI escape codes).
/// This is a simplified version — counts ASCII printable chars.
fn display_width(s: &str) -> usize {
    let mut w = 0;
    let mut in_escape = false;
    for ch in s.chars() {
        if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
            continue;
        }
        if ch == '\x1b' {
            in_escape = true;
            continue;
        }
        // CJK characters are typically 2 columns wide
        if ch as u32 >= 0x4E00 && ch as u32 <= 0x9FFF {
            w += 2;
        } else {
            w += 1;
        }
    }
    w
}

// ---------------------------------------------------------------------------
// Main gateway entry point
// ---------------------------------------------------------------------------

pub async fn run(cli_host: Option<String>, cli_port: Option<u16>) -> anyhow::Result<()> {
    let paths = Paths::new();
    let mut config = Config::load_or_default(&paths)?;

    // Auto-generate and persist node_alias if not set (short 8-char hex, e.g. "54c6be7b").
    // This becomes the stable display name for this node in the community hub.
    if config.community_hub.node_alias.is_none() {
        let alias = uuid::Uuid::new_v4().to_string().replace('-', "")[..8].to_string();
        config.community_hub.node_alias = Some(alias.clone());
        if let Err(e) = config.save(&paths.config_file()) {
            warn!("Failed to persist node_alias to config.json: {}", e);
        } else {
            info!(node_alias = %alias, "Generated and persisted node_alias to config.json");
        }
    }

    // If Community Hub is configured but apiKey is missing/empty, auto-register and persist.
    if let Some(hub_url) = config.community_hub_url() {
        if config.community_hub_api_key().is_none() {
            let register_url = format!("{}/v1/auth/register", hub_url.trim_end_matches('/'));
            let name = config.community_hub.node_alias.clone()
                .unwrap_or_else(|| "blockcell-gateway".to_string());

            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default();

            let body = serde_json::json!({
                "name": name,
                "email": null,
                "github_id": null,
            });

            match client.post(&register_url).json(&body).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    if status.is_success() {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let Some(api_key) = v.get("api_key").and_then(|x| x.as_str()) {
                                if !api_key.trim().is_empty() {
                                    config.community_hub.api_key = Some(api_key.trim().to_string());
                                    if let Err(e) = config.save(&paths.config_file()) {
                                        warn!(error = %e, "Failed to persist community hub apiKey to config file");
                                    } else {
                                        info!("Registered with Community Hub and persisted apiKey to config");
                                    }
                                }
                            }
                        }
                    } else {
                        warn!(status = %status, body = %text, "Community Hub register failed");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to register with Community Hub");
                }
            }
        }
    }

    // Resolve host/port: CLI args override config values
    let host = cli_host.unwrap_or_else(|| config.gateway.host.clone());
    let port = cli_port.unwrap_or(config.gateway.port);

    // Auto-generate and persist api_token if not configured or empty.
    // This ensures a stable token across restarts without manual setup.
    let needs_token = config.gateway.api_token.as_deref().map(|t| t.trim().is_empty()).unwrap_or(true);
    if needs_token {
        let env_token = std::env::var("BLOCKCELL_API_TOKEN").ok().filter(|t| !t.trim().is_empty());
        if let Some(token) = env_token {
            // Use env var but don't persist — user manages it externally
            config.gateway.api_token = Some(token);
        } else {
            // Generate a 64-char token (bc_ + 4×UUID hex = 3+32*4=131 chars, take first 61 for bc_+61=64)
            let raw = format!(
                "{}{}{}{}",
                uuid::Uuid::new_v4().to_string().replace('-', ""),
                uuid::Uuid::new_v4().to_string().replace('-', ""),
                uuid::Uuid::new_v4().to_string().replace('-', ""),
                uuid::Uuid::new_v4().to_string().replace('-', ""),
            );
            let generated = format!("bc_{}", &raw[..61]);
            config.gateway.api_token = Some(generated);
            if let Err(e) = config.save(&paths.config_file()) {
                warn!("Failed to persist auto-generated apiToken to config.json: {}", e);
            } else {
                info!("Auto-generated apiToken persisted to config.json");
            }
        }
    }

    info!(host = %host, port = port, "Starting blockcell gateway");

    // ── Multi-provider dispatch (same logic as agent CLI) ──
    let provider = super::provider::create_provider(&config)?;

    // ── Initialize memory store (SQLite + FTS5) ──
    let memory_db_path = paths.memory_dir().join("memory.db");
    let memory_store_handle: Option<MemoryStoreHandle> = match MemoryStore::open(&memory_db_path) {
        Ok(store) => {
            if let Err(e) = store.migrate_from_files(&paths.memory_dir()) {
                warn!("Memory migration failed: {}", e);
            }
            let adapter = MemoryStoreAdapter::new(store);
            Some(Arc::new(adapter))
        }
        Err(e) => {
            warn!("Failed to open memory store: {}. Memory tools will be unavailable.", e);
            None
        }
    };

    // ── Initialize tool evolution registry and core evolution engine ──
    let cap_registry_dir = paths.evolved_tools_dir();
    let cap_registry_raw = new_registry_handle(cap_registry_dir);
    {
        let mut reg = cap_registry_raw.lock().await;
        let _ = reg.load();
        let rehydrated = reg.rehydrate_executors();
        if rehydrated > 0 {
            info!("Rehydrated {} evolved tool executors from disk", rehydrated);
        }
    }

    let llm_timeout_secs = 300u64;
    let mut core_evo = CoreEvolution::new(
        paths.workspace().to_path_buf(),
        cap_registry_raw.clone(),
        llm_timeout_secs,
    );
    if let Ok(evo_provider) = super::provider::create_provider(&config) {
        let llm_bridge = Arc::new(ProviderLLMBridge::new(evo_provider));
        core_evo.set_llm_provider(llm_bridge);
        info!("Core evolution LLM provider configured");
    }
    let core_evo_raw = Arc::new(Mutex::new(core_evo));

    let cap_registry_adapter = CapabilityRegistryAdapter::new(cap_registry_raw.clone());
    let cap_registry_handle: CapabilityRegistryHandle = Arc::new(Mutex::new(cap_registry_adapter));

    let core_evo_adapter = CoreEvolutionAdapter::new(core_evo_raw.clone());
    let core_evo_handle: CoreEvolutionHandle = Arc::new(Mutex::new(core_evo_adapter));

    // ── Create message bus ──
    let bus = MessageBus::new(100);
    let ((inbound_tx, inbound_rx), (outbound_tx, outbound_rx)) = bus.split();

    // ── Create WebSocket broadcast channel ──
    let (ws_broadcast_tx, _) = broadcast::channel::<String>(1000);

    // ── Create shutdown channel ──
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // ── Create shared task manager ──
    let task_manager = TaskManager::new();

    // ── Create tool registry (shared for listing tools) ──
    let tool_registry = ToolRegistry::with_defaults();
    let tool_registry_shared = Arc::new(tool_registry.clone());

    // ── Create agent runtime with full component wiring ──
    let mut runtime = AgentRuntime::new(config.clone(), paths.clone(), provider, tool_registry)?;
    runtime.mount_mcp_servers().await;
    
    // 如果配置了独立的 evolution_model 或 evolution_provider，创建独立的 evolution provider
    if config.agents.defaults.evolution_model.is_some()
        || config.agents.defaults.evolution_provider.is_some()
    {
        match super::provider::create_evolution_provider(&config) {
            Ok(evo_provider) => {
                runtime.set_evolution_provider(evo_provider);
                info!("Evolution provider configured with independent model");
            }
            Err(e) => {
                warn!("Failed to create evolution provider: {}, using main provider", e);
            }
        }
    }
    
    // ── Set up WebSocket-based path confirmation channel ──
    let pending_confirms: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let (confirm_tx, mut confirm_rx) = mpsc::channel::<ConfirmRequest>(16);
    runtime.set_confirm(confirm_tx);

    // Spawn confirm handler: broadcasts confirm_request events to WS clients
    // and stores the oneshot sender keyed by request_id for later routing.
    let pending_confirms_for_handler = Arc::clone(&pending_confirms);
    let ws_broadcast_for_confirm = ws_broadcast_tx.clone();
    tokio::spawn(async move {
        while let Some(req) = confirm_rx.recv().await {
            let request_id = format!("confirm_{}", chrono::Utc::now().timestamp_millis());
            {
                let mut map = pending_confirms_for_handler.lock().await;
                map.insert(request_id.clone(), req.response_tx);
            }
            let event = serde_json::json!({
                "type": "confirm_request",
                "request_id": request_id,
                "tool": req.tool_name,
                "paths": req.paths,
            });
            let _ = ws_broadcast_for_confirm.send(event.to_string());
            info!(request_id = %request_id, tool = %req.tool_name, "Sent confirm_request to WebUI");
        }
    });

    runtime.set_outbound(outbound_tx);
    runtime.set_task_manager(task_manager.clone());
    if let Some(ref store) = memory_store_handle {
        runtime.set_memory_store(store.clone());
    }
    runtime.set_capability_registry(cap_registry_handle.clone());
    runtime.set_core_evolution(core_evo_handle.clone());
    runtime.set_event_tx(ws_broadcast_tx.clone());

    // ── Create channel manager for outbound dispatch ──
    let channel_manager = ChannelManager::new(config.clone(), paths.clone(), inbound_tx.clone());

    // ── Create session store ──
    let session_store = Arc::new(SessionStore::new(paths.clone()));

    // ── Create scheduler services ──
    let cron_service = Arc::new(CronService::new(paths.clone(), inbound_tx.clone()));
    cron_service.load().await?;

    let heartbeat_service = Arc::new(HeartbeatService::new(paths.clone(), inbound_tx.clone()));

    // Optional: register this gateway with the configured community hub.
    // This runs in the background and does not block gateway startup.
    if let Some(hub_url) = config.community_hub_url() {
        let client = reqwest::Client::new();
        let register_url = format!("{}/v1/nodes/heartbeat", hub_url.trim_end_matches('/'));
        let api_key = config.community_hub_api_key();
        let version = env!("CARGO_PKG_VERSION").to_string();
        let public_url = if host != "0.0.0.0" {
            Some(format!("http://{}:{}", host, port))
        } else {
            None
        };
        let node_alias = config.community_hub.node_alias.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(240));
            loop {
                interval.tick().await;

                let body = serde_json::json!({
                    "name": node_alias,
                    "version": version,
                    "public_url": public_url,
                    "tags": ["gateway", "cli"],
                    "skills": [],
                });

                let mut req = client.post(&register_url).json(&body);
                if let Some(key) = &api_key {
                    req = req.header("Authorization", format!("Bearer {}", key));
                }

                if let Err(e) = req.send().await {
                    warn!("Failed to send heartbeat to hub: {}", e);
                } else {
                    debug!("Sent heartbeat to hub");
                }
            }
        });
    }

    // ── Create Ghost Agent service ──
    let ghost_config = GhostServiceConfig::from_config(&config);
    let ghost_service = GhostService::new(ghost_config, paths.clone(), inbound_tx.clone());

    // ── Spawn core tasks ──
    let runtime_shutdown_rx = shutdown_tx.subscribe();
    let runtime_handle = tokio::spawn(async move {
        runtime.run_loop(inbound_rx, Some(runtime_shutdown_rx)).await;
    });

    // Wrap channel_manager in Arc so it can be shared between the outbound bridge and gateway state
    let channel_manager = Arc::new(channel_manager);

    // Outbound → WS broadcast bridge + external channel dispatch
    let ws_broadcast_for_bridge = ws_broadcast_tx.clone();
    let outbound_shutdown_rx = shutdown_tx.subscribe();
    let channel_manager_for_bridge = Arc::clone(&channel_manager);
    let outbound_handle = tokio::spawn(async move {
        outbound_to_ws_bridge(outbound_rx, ws_broadcast_for_bridge, channel_manager_for_bridge, outbound_shutdown_rx).await;
    });

    let cron_handle = {
        let cron = cron_service.clone();
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            cron.run_loop(shutdown_rx).await;
        })
    };

    let heartbeat_handle = {
        let heartbeat = heartbeat_service.clone();
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            heartbeat.run_loop(shutdown_rx).await;
        })
    };

    let ghost_handle = {
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            ghost_service.run_loop(shutdown_rx).await;
        })
    };

    // ── Start messaging channels ──
    #[cfg(feature = "telegram")]
    let telegram_handle = {
        let telegram = Arc::new(TelegramChannel::new(config.clone(), inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            telegram.run_loop(shutdown_rx).await;
        })
    };

    #[cfg(feature = "whatsapp")]
    let whatsapp_handle = {
        let whatsapp = Arc::new(WhatsAppChannel::new(config.clone(), inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            whatsapp.run_loop(shutdown_rx).await;
        })
    };

    #[cfg(feature = "feishu")]
    let feishu_handle = {
        let feishu = Arc::new(FeishuChannel::new(config.clone(), inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            feishu.run_loop(shutdown_rx).await;
        })
    };

    #[cfg(feature = "slack")]
    let slack_handle = {
        let slack = Arc::new(SlackChannel::new(config.clone(), inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            slack.run_loop(shutdown_rx).await;
        })
    };

    #[cfg(feature = "discord")]
    let discord_handle = {
        let discord = Arc::new(DiscordChannel::new(config.clone(), inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            discord.run_loop(shutdown_rx).await;
        })
    };

    #[cfg(feature = "dingtalk")]
    let dingtalk_handle = {
        let dingtalk = Arc::new(DingTalkChannel::new(config.clone(), inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            dingtalk.run_loop(shutdown_rx).await;
        })
    };

    #[cfg(feature = "wecom")]
    let wecom_handle = {
        let wecom = Arc::new(WeComChannel::new(config.clone(), inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            wecom.run_loop(shutdown_rx).await;
        })
    };

    // ── Build HTTP/WebSocket server ──
    // Guarantee api_token is Some and non-empty — defensive fallback in case auto-gen above
    // somehow produced None or empty (e.g. env var was whitespace-only).
    if config.gateway.api_token.as_deref().map(|t| t.trim().is_empty()).unwrap_or(true) {
        let raw = format!(
            "{}{}{}{}",
            uuid::Uuid::new_v4().to_string().replace('-', ""),
            uuid::Uuid::new_v4().to_string().replace('-', ""),
            uuid::Uuid::new_v4().to_string().replace('-', ""),
            uuid::Uuid::new_v4().to_string().replace('-', ""),
        );
        let fallback = format!("bc_{}", &raw[..61]);
        warn!("api_token was missing/empty before building GatewayState; using in-memory fallback");
        config.gateway.api_token = Some(fallback);
    }
    let api_token = config.gateway.api_token.clone();

    // Determine WebUI login password:
    // - If gateway.webuiPass is set in config → use it (stable across restarts)
    // - Otherwise → generate a random temp password printed at startup (NOT saved)
    let (web_password, webui_pass_is_temp) = match &config.gateway.webui_pass {
        Some(p) if !p.is_empty() => (p.clone(), false),
        _ => {
            let tmp = format!("{:08x}", rand_u32());
            (tmp, true)
        }
    };

    let is_exposed = host == "0.0.0.0" || host == "::";

    // Create a shared EvolutionService for the HTTP handlers (trigger, delete, status).
    // This is separate from the one inside AgentRuntime but shares the same disk records.
    let shared_evo_service = Arc::new(Mutex::new(
        EvolutionService::new(paths.skills_dir(), EvolutionServiceConfig::default())
    ));

    let gateway_state = GatewayState {
        inbound_tx: inbound_tx.clone(),
        task_manager,
        config: config.clone(),
        paths: paths.clone(),
        api_token: api_token.clone(),
        ws_broadcast: ws_broadcast_tx.clone(),
        pending_confirms: Arc::clone(&pending_confirms),
        session_store,
        cron_service: cron_service.clone(),
        memory_store: memory_store_handle.clone(),
        tool_registry: tool_registry_shared,
        web_password: web_password.clone(),
        channel_manager: Arc::clone(&channel_manager),
        evolution_service: shared_evo_service,
    };

    let app = Router::new()
        // Auth
        .route("/v1/auth/login", post(handle_login))
        // P0: Core
        .route("/v1/chat", post(handle_chat))
        .route("/v1/health", get(handle_health))
        .route("/v1/tasks", get(handle_tasks))
        .route("/v1/ws", get(handle_ws_upgrade))
        // P0: Sessions
        .route("/v1/sessions", get(handle_sessions_list))
        .route("/v1/sessions/:id", get(handle_session_get).delete(handle_session_delete))
        .route("/v1/sessions/:id/rename", put(handle_session_rename))
        // P1: Config
        .route("/v1/config", get(handle_config_get).put(handle_config_update))
        .route("/v1/config/test-provider", post(handle_config_test_provider))
        // Ghost Agent
        .route("/v1/ghost/config", get(handle_ghost_config_get).put(handle_ghost_config_update))
        .route("/v1/ghost/activity", get(handle_ghost_activity))
        .route("/v1/ghost/model-options", get(handle_ghost_model_options_get))
        // P1: Memory
        .route("/v1/memory", get(handle_memory_list).post(handle_memory_create))
        .route("/v1/memory/stats", get(handle_memory_stats))
        .route("/v1/memory/:id", delete(handle_memory_delete))
        // P1: Tools / Skills / Evolution / Stats
        .route("/v1/tools", get(handle_tools))
        .route("/v1/skills", get(handle_skills))
        .route("/v1/skills/search", post(handle_skills_search))
        .route("/v1/evolution", get(handle_evolution))
        .route("/v1/evolution/tool-evolutions", get(handle_evolution_tool_evolutions))
        .route("/v1/evolution/summary", get(handle_evolution_summary))
        .route("/v1/evolution/trigger", post(handle_evolution_trigger))
        .route("/v1/evolution/test", post(handle_evolution_test))
        .route("/v1/evolution/test-suggest", post(handle_evolution_test_suggest))
        .route("/v1/evolution/versions/:skill", get(handle_evolution_versions))
        .route("/v1/evolution/tool-versions/:id", get(handle_evolution_tool_versions))
        .route("/v1/evolution/:id", get(handle_evolution_detail).delete(handle_evolution_delete))
        .route("/v1/channels/status", get(handle_channels_status))
        .route("/v1/stats", get(handle_stats))
        // P1: Cron
        .route("/v1/cron", get(handle_cron_list).post(handle_cron_create))
        .route("/v1/cron/:id", delete(handle_cron_delete))
        .route("/v1/cron/:id/run", post(handle_cron_run))
        // Toggles
        .route("/v1/toggles", get(handle_toggles_get).put(handle_toggles_update))
        // P2: Alerts
        .route("/v1/alerts", get(handle_alerts_list).post(handle_alerts_create))
        .route("/v1/alerts/history", get(handle_alerts_history))
        .route("/v1/alerts/:id", put(handle_alerts_update).delete(handle_alerts_delete))
        // P2: Streams
        .route("/v1/streams", get(handle_streams_list))
        .route("/v1/streams/:id/data", get(handle_stream_data))
        // P2: Files
        .route("/v1/files", get(handle_files_list))
        .route("/v1/files/content", get(handle_files_content))
        .route("/v1/files/download", get(handle_files_download))
        .route("/v1/files/serve", get(handle_files_serve))
        .route("/v1/files/upload", post(handle_files_upload))
        .layer(middleware::from_fn_with_state(gateway_state.clone(), auth_middleware))
        .layer(build_api_cors_layer(&config))
        // Webhook endpoints — public (no auth), must be outside auth middleware
        .route("/webhook/lark", post(handle_lark_webhook))
        .route("/webhook/wecom", get(handle_wecom_webhook).post(handle_wecom_webhook))
        .with_state(gateway_state);

    let bind_addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;

    let http_shutdown_rx = shutdown_tx.subscribe();
    let http_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let mut rx = http_shutdown_rx;
                let _ = rx.recv().await;
            })
            .await
            .ok();
    });

    // ── WebUI static file server (embedded via rust-embed) ──
    let webui_host = config.gateway.webui_host.clone();
    let webui_port = config.gateway.webui_port;
    let webui_bind = format!("{}:{}", webui_host, webui_port);
    let webui_app = Router::new()
        .fallback(handle_webui_static)
        .layer(build_webui_cors_layer(&config));
    let webui_listener = tokio::net::TcpListener::bind(&webui_bind).await?;
    let webui_shutdown_rx = shutdown_tx.subscribe();
    let webui_handle = tokio::spawn(async move {
        axum::serve(webui_listener, webui_app)
            .with_graceful_shutdown(async move {
                let mut rx = webui_shutdown_rx;
                let _ = rx.recv().await;
            })
            .await
            .ok();
    });

    // ── Print beautiful startup banner ──
    print_startup_banner(&config, &host, &webui_host, webui_port, &web_password, webui_pass_is_temp, is_exposed, &bind_addr);

    // ── Wait for shutdown signal ──
    tokio::signal::ctrl_c().await?;
    info!("Shutdown signal received, draining tasks...");

    let _ = shutdown_tx.send(());
    drop(inbound_tx);
    // Drop local services that still hold inbound_tx clones so runtime can observe
    // channel closure and exit promptly.
    drop(cron_service);
    drop(heartbeat_service);

    let mut handles: Vec<(&str, tokio::task::JoinHandle<()>)> = vec![
        ("http_server", http_handle),
        ("webui_server", webui_handle),
        ("runtime", runtime_handle),
        ("outbound", outbound_handle),
        ("cron", cron_handle),
        ("heartbeat", heartbeat_handle),
        ("ghost", ghost_handle),
    ];

    #[cfg(feature = "telegram")]
    handles.push(("telegram", telegram_handle));

    #[cfg(feature = "whatsapp")]
    handles.push(("whatsapp", whatsapp_handle));

    #[cfg(feature = "feishu")]
    handles.push(("feishu", feishu_handle));

    #[cfg(feature = "slack")]
    handles.push(("slack", slack_handle));

    #[cfg(feature = "discord")]
    handles.push(("discord", discord_handle));

    #[cfg(feature = "dingtalk")]
    handles.push(("dingtalk", dingtalk_handle));

    #[cfg(feature = "wecom")]
    handles.push(("wecom", wecom_handle));


    let total = handles.len();
    let graceful_timeout = std::time::Duration::from_secs(30);
    let deadline = tokio::time::Instant::now() + graceful_timeout;

    // Wait briefly for graceful shutdown.
    loop {
        if handles.iter().all(|(_, h)| h.is_finished()) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Force-stop any stragglers so Ctrl+C returns quickly.
    let mut aborted = 0;
    for (name, handle) in &handles {
        if !handle.is_finished() {
            warn!(task = *name, "Task did not exit in graceful window, aborting");
            handle.abort();
            aborted += 1;
        }
    }

    let mut failed = 0;
    for (name, handle) in handles {
        match handle.await {
            Ok(()) => {}
            Err(e) if e.is_cancelled() => {
                debug!(task = name, "Task cancelled during shutdown");
            }
            Err(e) => {
                error!(task = name, error = %e, "Task panicked during shutdown");
                failed += 1;
            }
        }
    }

    if failed == 0 {
        info!(total, aborted, "Gateway shutdown complete");
    } else {
        warn!(failed, total, aborted, "Gateway shutdown completed with task failures");
    }

    info!("Gateway stopped");
    Ok(())
}

fn build_api_cors_layer(config: &Config) -> CorsLayer {
    let _ = config;
    CorsLayer::permissive().allow_credentials(false)
}

fn build_webui_cors_layer(config: &Config) -> CorsLayer {
    let _ = config;
    CorsLayer::permissive().allow_credentials(false)
}
