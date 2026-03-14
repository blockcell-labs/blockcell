use super::*;
// ---------------------------------------------------------------------------
// HTTP request / response types
// ---------------------------------------------------------------------------

pub(super) fn assign_session_id(chat_id: &str, agent_id: &str) -> String {
    let trimmed = chat_id.trim();
    if trimmed.is_empty() || trimmed == "default" {
        return format!("{}:{}", agent_id, chrono::Utc::now().timestamp_millis());
    }

    trimmed.to_string()
}

#[derive(Deserialize)]
pub(super) struct ChatRequest {
    content: String,
    #[serde(default = "default_channel")]
    channel: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default = "default_sender")]
    sender_id: String,
    #[serde(default)]
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
#[derive(Serialize)]
struct ChatResponse {
    status: String,
    message: String,
    session_id: String,
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
    let resolved_agent_id = match req.agent_id.as_deref() {
        Some(requested) => match resolve_requested_agent_id(&state.config, Some(requested)) {
            Ok(agent_id) => agent_id,
            Err(err) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ChatResponse {
                        status: "error".to_string(),
                        message: err,
                        session_id: String::new(),
                    }),
                )
            }
        },
        None => "default".to_string(),
    };

    let session_id = assign_session_id(&req.chat_id, &resolved_agent_id);

    let inbound = InboundMessage {
        channel: req.channel,
        account_id: req.account_id,
        sender_id: req.sender_id,
        chat_id: session_id.clone(),
        content: req.content,
        media: req.media,
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    let inbound = with_route_agent_id(inbound, &resolved_agent_id);

    match state.inbound_tx.send(inbound).await {
        Ok(_) => (
            StatusCode::ACCEPTED,
            Json(ChatResponse {
                status: "accepted".to_string(),
                message: "Message queued for processing".to_string(),
                session_id: session_id.clone(),
            }),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ChatResponse {
                status: "error".to_string(),
                message: format!("Failed to queue message: {}", e),
                session_id,
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

pub(super) async fn handle_tasks(
    State(state): State<GatewayState>,
    Query(agent): Query<AgentScopedQuery>,
) -> impl IntoResponse {
    let agent_id = match resolve_requested_agent_id(&state.config, agent.agent.as_deref()) {
        Ok(agent_id) => agent_id,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": err })),
            )
                .into_response();
        }
    };
    let tasks = state.task_manager.list_tasks(None).await;
    let filtered_tasks: Vec<_> = tasks
        .into_iter()
        .filter(|task| task.agent_id.as_deref().unwrap_or("default") == agent_id)
        .collect();
    let (queued, running, completed, failed) = filtered_tasks.iter().fold(
        (0usize, 0usize, 0usize, 0usize),
        |(queued, running, completed, failed), task| match task.status {
            blockcell_agent::task_manager::TaskStatus::Queued => {
                (queued + 1, running, completed, failed)
            }
            blockcell_agent::task_manager::TaskStatus::Running => {
                (queued, running + 1, completed, failed)
            }
            blockcell_agent::task_manager::TaskStatus::Completed => {
                (queued, running, completed + 1, failed)
            }
            blockcell_agent::task_manager::TaskStatus::Failed
            | blockcell_agent::task_manager::TaskStatus::Cancelled => {
                (queued, running, completed, failed + 1)
            }
        },
    );
    let tasks_json =
        serde_json::to_value(&filtered_tasks).unwrap_or(serde_json::Value::Array(vec![]));

    Json(TasksResponse {
        queued,
        running,
        completed,
        failed,
        tasks: tasks_json,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::assign_session_id;

    #[test]
    fn assign_session_id_generates_new_id_when_missing() {
        let session_id = assign_session_id("", "default");

        assert!(session_id.starts_with("default:"));
        let suffix = session_id.trim_start_matches("default:");
        assert!(suffix.chars().all(|c| c.is_ascii_digit()));
        assert!(suffix.len() >= 10);
    }

    #[test]
    fn assign_session_id_preserves_existing_ws_session_id() {
        let session_id = assign_session_id("default:1773470425266", "default");

        assert_eq!(session_id, "default:1773470425266");
    }
}
