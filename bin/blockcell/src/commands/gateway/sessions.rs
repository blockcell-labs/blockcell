use super::*;
use blockcell_core::{
    resolve_session_key_from_id, session_file_stem, session_id_from_file_stem,
    session_title_from_id,
};
use blockcell_storage::SessionStore;
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

fn session_file_stems(sessions_dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(sessions_dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                return None;
            }
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
        })
        .collect()
}

#[derive(Deserialize)]
pub(super) struct SessionsListQuery {
    limit: Option<usize>,
    cursor: Option<usize>,
    agent: Option<String>,
}

/// GET /v1/sessions — list sessions (supports pagination)
pub(super) async fn handle_sessions_list(
    State(state): State<GatewayState>,
    Query(params): Query<SessionsListQuery>,
) -> impl IntoResponse {
    let agent_id = match resolve_requested_agent_id(&state.config, params.agent.as_deref()) {
        Ok(agent_id) => agent_id,
        Err(err) => return Json(serde_json::json!({ "error": err })),
    };
    let agent_paths = state.paths.for_agent(&agent_id);
    let sessions_dir = agent_paths.sessions_dir();
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
                let file_stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();

                let session_id = session_id_from_file_stem(&file_stem);

                let updated_at = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .map(|t| {
                        let dt: chrono::DateTime<chrono::Utc> = t.into();
                        dt.to_rfc3339()
                    })
                    .unwrap_or_default();

                let message_count = std::fs::read_to_string(&path)
                    .map(|c| {
                        c.lines()
                            .filter(|l| !l.trim().is_empty())
                            .count()
                            .saturating_sub(1)
                    })
                    .unwrap_or(0);

                let name = meta
                    .get(&file_stem)
                    .or_else(|| meta.get(&session_id))
                    .and_then(|v| v.get("name"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| session_title_from_id(&session_id));

                sessions.push(SessionInfo {
                    id: session_id,
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
pub(super) async fn handle_session_get(
    State(state): State<GatewayState>,
    AxumPath(session_id): AxumPath<String>,
    Query(agent): Query<AgentScopedQuery>,
) -> impl IntoResponse {
    let agent_id = match resolve_requested_agent_id(&state.config, agent.agent.as_deref()) {
        Ok(agent_id) => agent_id,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": err })),
            )
                .into_response()
        }
    };
    let agent_paths = state.paths.for_agent(&agent_id);
    let session_stems = session_file_stems(&agent_paths.sessions_dir());
    let session_key =
        resolve_session_key_from_id(&session_id, session_stems.iter().map(|s| s.as_str()));
    let session_store = SessionStore::new(agent_paths);
    let loaded_messages: blockcell_core::Result<Vec<blockcell_core::types::ChatMessage>> =
        session_store.load(&session_key);
    match loaded_messages {
        Ok(messages) if !messages.is_empty() => {
            let msgs: Vec<serde_json::Value> = messages
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "role": m.role,
                        "content": m.content,
                        "tool_calls": m.tool_calls,
                        "tool_call_id": m.tool_call_id,
                        "reasoning_content": m.reasoning_content,
                    })
                })
                .collect();
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "session_id": session_id,
                    "messages": msgs,
                })),
            )
                .into_response()
        }
        Ok(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Session not found or empty"
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("Session not found: {}", e)
            })),
        )
            .into_response(),
    }
}

/// DELETE /v1/sessions/:id — delete a session
pub(super) async fn handle_session_delete(
    State(state): State<GatewayState>,
    AxumPath(session_id): AxumPath<String>,
    Query(agent): Query<AgentScopedQuery>,
) -> impl IntoResponse {
    let agent_id = match resolve_requested_agent_id(&state.config, agent.agent.as_deref()) {
        Ok(agent_id) => agent_id,
        Err(err) => return Json(serde_json::json!({ "status": "error", "message": err })),
    };
    let agent_paths = state.paths.for_agent(&agent_id);
    let session_stems = session_file_stems(&agent_paths.sessions_dir());
    let session_key =
        resolve_session_key_from_id(&session_id, session_stems.iter().map(|s| s.as_str()));
    let path = agent_paths.session_file(&session_key);
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
pub(super) struct RenameRequest {
    name: String,
}

/// PUT /v1/sessions/:id/rename — rename a session (stored as metadata)
pub(super) async fn handle_session_rename(
    State(state): State<GatewayState>,
    AxumPath(session_id): AxumPath<String>,
    Query(agent): Query<AgentScopedQuery>,
    Json(req): Json<RenameRequest>,
) -> impl IntoResponse {
    let agent_id = match resolve_requested_agent_id(&state.config, agent.agent.as_deref()) {
        Ok(agent_id) => agent_id,
        Err(err) => return Json(serde_json::json!({ "status": "error", "message": err })),
    };
    let agent_paths = state.paths.for_agent(&agent_id);
    let session_stems = session_file_stems(&agent_paths.sessions_dir());
    let session_key =
        resolve_session_key_from_id(&session_id, session_stems.iter().map(|s| s.as_str()));
    let file_stem = session_file_stem(&session_key);
    let normalized_id = session_file_stem(&session_id);
    let meta_path = agent_paths.sessions_dir().join("_meta.json");
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

        meta.insert(file_stem, serde_json::json!({ "name": name.clone() }));
        meta.insert(normalized_id, serde_json::json!({ "name": name.clone() }));

        match std::fs::write(
            &meta_path,
            serde_json::to_string_pretty(&meta).unwrap_or_default(),
        ) {
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
