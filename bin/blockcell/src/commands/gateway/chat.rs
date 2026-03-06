use super::*;
// ---------------------------------------------------------------------------
// HTTP request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct ChatRequest {
    content: String,
    #[serde(default = "default_channel")]
    channel: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default = "default_sender")]
    sender_id: String,
    #[serde(default = "default_chat")]
    chat_id: String,
    #[serde(default)]
    media: Vec<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

fn default_channel() -> String {
    "ws".to_string()
}
fn default_sender() -> String {
    "user".to_string()
}
fn default_chat() -> String {
    "default".to_string()
}

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
pub(super) struct LoginRequest {
    password: String,
}

pub(super) async fn handle_login(
    State(state): State<GatewayState>,
    Json(req): Json<LoginRequest>,
) -> Response {
    if !secure_eq(&req.password, &state.web_password) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "Invalid password" })),
        )
            .into_response();
    }
    // Return the api_token as the Bearer token for subsequent API requests
    match &state.api_token {
        Some(token) if !token.is_empty() => {
            Json(serde_json::json!({ "token": token })).into_response()
        }
        _ => {
            // Should never happen after the defensive guarantee above
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "Server token not configured" })),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// P0 HTTP handlers — Core chat + tasks
// ---------------------------------------------------------------------------

pub(super) async fn handle_chat(
    State(state): State<GatewayState>,
    Json(req): Json<ChatRequest>,
) -> impl IntoResponse {
    let inbound = InboundMessage {
        channel: req.channel,
        account_id: req.account_id,
        sender_id: req.sender_id,
        chat_id: req.chat_id,
        content: req.content,
        media: req.media,
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    let inbound = match req.agent_id.as_deref() {
        Some(requested) => match resolve_requested_agent_id(&state.config, Some(requested)) {
            Ok(agent_id) => with_route_agent_id(inbound, &agent_id),
            Err(err) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ChatResponse {
                        status: "error".to_string(),
                        message: err,
                    }),
                )
            }
        },
        None => inbound,
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

pub(super) async fn handle_health(State(state): State<GatewayState>) -> impl IntoResponse {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    let (active_model, _, _) = active_model_and_provider(&state.config);

    Json(HealthResponse {
        status: "ok".to_string(),
        model: active_model,
        uptime_secs: start.elapsed().as_secs(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

pub(super) async fn handle_tasks(State(state): State<GatewayState>) -> impl IntoResponse {
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
