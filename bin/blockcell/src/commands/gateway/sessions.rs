use super::*;
use blockcell_core::{
    resolve_session_key_from_id, session_file_stem, session_id_from_file_stem,
    session_title_from_id,
};
use blockcell_storage::SessionStore;

#[derive(Serialize, Clone)]
struct SessionInfo {
    id: String,
    channel: String,
    chat_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    legacy_id: Option<String>,
    name: String,
    updated_at: String,
    message_count: usize,
}

fn count_session_messages(path: &std::path::Path) -> usize {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    let mut non_empty = 0usize;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if !line.trim().is_empty() => non_empty += 1,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    non_empty.saturating_sub(1)
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
                .map(str::to_string)
        })
        .collect()
}

// Account lengths are byte lengths, matching InboundMessage::session_key.
fn session_identity(key: &str) -> Option<(&str, Option<&str>, &str)> {
    let (channel, rest) = key.split_once(':')?;
    if let Some(account) = rest.strip_prefix("account:") {
        if let Some((length, value)) = account.split_once(':') {
            if let Ok(length) = length.parse::<usize>() {
                if let (Some(account_id), Some(chat_id)) =
                    (value.get(..length), value.get(length..))
                {
                    return Some((channel, Some(account_id), chat_id));
                }
            }
        }
    }
    Some((channel, None, rest))
}

fn compatible_legacy_id(stem: &str, key: &str, stems: &[String]) -> Option<String> {
    let previous_id = if stem.starts_with("s~") {
        key.split_once(':')?.1
    } else {
        stem.split_once('_')?.1
    };
    let resolved =
        resolve_session_key_from_id(previous_id, stems.iter().map(String::as_str)).ok()?;
    (resolved == key).then(|| previous_id.to_string())
}

fn session_resolution_error(error: blockcell_core::Error) -> axum::response::Response {
    let status = if matches!(error, blockcell_core::Error::NotFound(_)) {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::BAD_REQUEST
    };
    (status, Json(serde_json::json!({"status": "error", "error": error.to_string(), "message": error.to_string()}))).into_response()
}

#[derive(Deserialize)]
pub(super) struct SessionsListQuery {
    limit: Option<usize>,
    cursor: Option<usize>,
    agent: Option<String>,
}

/// GET /v1/sessions — full opaque handles scoped by the requested Agent.
pub(super) async fn handle_sessions_list(
    State(state): State<GatewayState>,
    Query(params): Query<SessionsListQuery>,
) -> impl IntoResponse {
    let agent_id = match resolve_requested_agent_id(&state.config, params.agent.as_deref()) {
        Ok(id) => id,
        Err(err) => return Json(serde_json::json!({"error": err})),
    };
    let sessions_dir = state.paths.for_agent(&agent_id).sessions_dir();
    let result = tokio::task::spawn_blocking(move || {
        let meta = std::fs::read_to_string(sessions_dir.join("_meta.json")).ok()
            .and_then(|text| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&text).ok()).unwrap_or_default();
        let stems = session_file_stems(&sessions_dir);
        let mut sessions = Vec::new();
        for stem in &stems {
            let id = session_id_from_file_stem(stem);
            let Ok(key) = resolve_session_key_from_id(&id, [stem.as_str()]) else { continue };
            let Some((channel, account_id, chat_id)) = session_identity(&key) else { continue };
            if channel == "ghost" { continue }
            let path = sessions_dir.join(format!("{stem}.jsonl"));
            let updated_at = std::fs::metadata(&path).and_then(|m| m.modified())
                .map(|time| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339()).unwrap_or_default();
            let legacy_id = compatible_legacy_id(stem, &key, &stems);
            let name = meta.get(stem).or_else(|| legacy_id.as_ref().and_then(|id| meta.get(id)))
                .and_then(|value| value.get("name")).and_then(|value| value.as_str())
                .map(str::to_string).unwrap_or_else(|| session_title_from_id(chat_id));
            sessions.push(SessionInfo {
                id, channel: channel.to_string(), chat_id: chat_id.to_string(),
                account_id: account_id.map(str::to_string), agent_id: agent_id.clone(), legacy_id,
                name, updated_at, message_count: count_session_messages(&path),
            });
        }
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| a.id.cmp(&b.id)));
        let total = sessions.len();
        let start = params.cursor.unwrap_or(0).min(total);
        let end = start.saturating_add(params.limit.unwrap_or(total)).min(total);
        serde_json::json!({"sessions": sessions[start..end], "next_cursor": if end < total {Some(end)} else {None}, "total": total})
    }).await;
    match result {
        Ok(value) => Json(value),
        Err(error) => {
            Json(serde_json::json!({"error": format!("Failed to list sessions: {error}")}))
        }
    }
}

/// GET /v1/sessions/:id — resolve the complete handle before opening history.
pub(super) async fn handle_session_get(
    State(state): State<GatewayState>,
    AxumPath(session_id): AxumPath<String>,
    Query(agent): Query<AgentScopedQuery>,
) -> axum::response::Response {
    let agent_id = match resolve_requested_agent_id(&state.config, agent.agent.as_deref()) {
        Ok(id) => id,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": error})),
            )
                .into_response()
        }
    };
    let paths = state.paths.for_agent(&agent_id);
    let stems = session_file_stems(&paths.sessions_dir());
    let key = match resolve_session_key_from_id(&session_id, stems.iter().map(String::as_str)) {
        Ok(key) => key,
        Err(error) => return session_resolution_error(error),
    };
    match SessionStore::new(paths).load(&key) {
        Ok(messages) if !messages.is_empty() => {
            let messages: Vec<_> = messages.iter().map(|message| serde_json::json!({
                "role": message.role, "content": message.content, "tool_calls": message.tool_calls,
                "tool_call_id": message.tool_call_id, "reasoning_content": message.reasoning_content,
            })).collect();
            let (channel, account_id, chat_id) =
                session_identity(&key).expect("resolver returns a complete session key");
            Json(serde_json::json!({
                "session_id": session_id_from_file_stem(&session_file_stem(&key)),
                "channel": channel, "account_id": account_id, "chat_id": chat_id, "agent_id": agent_id,
                "messages": messages,
            })).into_response()
        }
        Ok(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "Session not found or empty"})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("Session not found: {error}")})),
        )
            .into_response(),
    }
}

/// DELETE /v1/sessions/:id — clear only the resolved current stem.
pub(super) async fn handle_session_delete(
    State(state): State<GatewayState>,
    AxumPath(session_id): AxumPath<String>,
    Query(agent): Query<AgentScopedQuery>,
) -> axum::response::Response {
    let agent_id = match resolve_requested_agent_id(&state.config, agent.agent.as_deref()) {
        Ok(id) => id,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"status": "error", "message": error})),
            )
                .into_response()
        }
    };
    let paths = state.paths.for_agent(&agent_id);
    let stems = session_file_stems(&paths.sessions_dir());
    let key = match resolve_session_key_from_id(&session_id, stems.iter().map(String::as_str)) {
        Ok(key) => key,
        Err(error) => return session_resolution_error(error),
    };
    let session_id = session_id_from_file_stem(&session_file_stem(&key));
    let result = tokio::task::spawn_blocking(move || {
        match SessionStore::new(paths).clear(&key) {
            Ok(deleted) => serde_json::json!({"status": if deleted {"deleted"} else {"not_found"}, "session_id": session_id}),
            Err(error) => serde_json::json!({"status": "error", "message": error.to_string(), "session_id": session_id}),
        }
    }).await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => Json(serde_json::json!({"status": "error", "message": error.to_string()}))
            .into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct RenameRequest {
    name: String,
}

/// PUT /v1/sessions/:id/rename — metadata uses the full canonical stem only.
pub(super) async fn handle_session_rename(
    State(state): State<GatewayState>,
    AxumPath(session_id): AxumPath<String>,
    Query(agent): Query<AgentScopedQuery>,
    Json(req): Json<RenameRequest>,
) -> axum::response::Response {
    let agent_id = match resolve_requested_agent_id(&state.config, agent.agent.as_deref()) {
        Ok(id) => id,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"status": "error", "message": error})),
            )
                .into_response()
        }
    };
    let paths = state.paths.for_agent(&agent_id);
    let stems = session_file_stems(&paths.sessions_dir());
    let key = match resolve_session_key_from_id(&session_id, stems.iter().map(String::as_str)) {
        Ok(key) => key,
        Err(error) => return session_resolution_error(error),
    };
    let stem = session_file_stem(&key);
    let session_id = session_id_from_file_stem(&stem);
    let meta_path = paths.sessions_dir().join("_meta.json");
    let result = tokio::task::spawn_blocking(move || {
        let mut meta = std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|text| {
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&text).ok()
            })
            .unwrap_or_default();
        meta.insert(stem, serde_json::json!({"name": req.name}));
        match std::fs::write(
            meta_path,
            serde_json::to_vec_pretty(&meta).unwrap_or_default(),
        ) {
            Ok(()) => {
                serde_json::json!({"status": "ok", "session_id": session_id, "name": req.name})
            }
            Err(error) => serde_json::json!({"status": "error", "message": error.to_string()}),
        }
    })
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => Json(serde_json::json!({"status": "error", "message": error.to_string()}))
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use blockcell_core::types::ChatMessage;

    struct SessionFixture {
        state: GatewayState,
        directory: std::path::PathBuf,
    }

    impl SessionFixture {
        fn new() -> Self {
            let directory = std::env::temp_dir()
                .join(format!("blockcell-session-api-{}", uuid::Uuid::new_v4()));
            let paths = Paths::with_base(directory.clone());
            let config = Config::default();
            let (inbound_tx, _) = mpsc::channel(4);
            let (ws_broadcast, _) = broadcast::channel(4);
            let channel_manager = Arc::new(blockcell_channels::ChannelManager::new(
                config.clone(),
                paths.clone(),
                inbound_tx.clone(),
            ));
            let evolution_service = Arc::new(Mutex::new(EvolutionService::new(
                paths.skills_dir(),
                EvolutionServiceConfig::default(),
            )));
            let state = GatewayState {
                inbound_tx,
                task_manager: TaskManager::new(),
                config,
                config_write_lock: Arc::new(Mutex::new(())),
                paths,
                api_token: None,
                ws_broadcast,
                pending_confirms: Arc::new(Mutex::new(HashMap::new())),
                pending_channel_confirms: Arc::new(Mutex::new(HashMap::new())),
                memory_store: None,
                memory_stores: Arc::new(HashMap::new()),
                cron_services: Arc::new(HashMap::new()),
                tool_registry: Arc::new(ToolRegistry::with_defaults()),
                web_password: "test".into(),
                login_rate_limiter: Arc::new(Mutex::new(LoginRateLimiter::default())),
                channel_manager,
                evolution_service,
                response_caches: Arc::new(RwLock::new(HashMap::new())),
                active_steering: Arc::new(Mutex::new(HashMap::new())),
            };
            Self { state, directory }
        }
    }

    impl Drop for SessionFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    async fn json(response: impl IntoResponse) -> (StatusCode, serde_json::Value) {
        let response = response.into_response();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    async fn list(state: &GatewayState) -> serde_json::Value {
        json(
            handle_sessions_list(
                State(state.clone()),
                Query(SessionsListQuery {
                    limit: None,
                    cursor: None,
                    agent: None,
                }),
            )
            .await,
        )
        .await
        .1
    }

    #[tokio::test]
    async fn session_list_handles_retain_channel_and_account_identity() {
        let fixture = SessionFixture::new();
        let state = &fixture.state;
        let store = SessionStore::new(state.paths.clone());
        let mut origins = Vec::new();
        for (channel, account) in [
            ("ws", None),
            ("telegram", None),
            ("telegram", Some("work")),
            ("telegram", Some("企业")),
        ] {
            let mut message = InboundMessage::cli("seed");
            message.channel = channel.to_string();
            message.account_id = account.map(str::to_string);
            message.chat_id = "a_b".to_string();
            let key = message.session_key();
            store.save(&key, &[ChatMessage::user(&key)]).unwrap();
            origins.push((channel, account, key));
        }
        let listed = list(state).await;
        let sessions = listed["sessions"].as_array().unwrap();
        let ids: std::collections::HashSet<_> =
            sessions.iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), origins.len());
        for (channel, account, key) in origins {
            let session = sessions
                .iter()
                .find(|s| s["channel"] == channel && s["account_id"].as_str() == account)
                .unwrap();
            assert_eq!(session["chat_id"], "a_b");
            assert_eq!(session["agent_id"], "default");
            let (status, history) = json(
                handle_session_get(
                    State(state.clone()),
                    AxumPath(session["id"].as_str().unwrap().to_string()),
                    Query(AgentScopedQuery { agent: None }),
                )
                .await,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(history["messages"][0]["content"], key);
        }
        assert!(handle_session_get(
            State(state.clone()),
            AxumPath("a_b".to_string()),
            Query(AgentScopedQuery { agent: None })
        )
        .await
        .status()
        .is_client_error());
    }

    #[tokio::test]
    async fn session_handles_isolate_history_rename_and_delete_in_both_creation_orders() {
        for reverse in [false, true] {
            let fixture = SessionFixture::new();
            let state = &fixture.state;
            let store = SessionStore::new(state.paths.clone());
            let mut keys = ["ws:a:b", "ws:a_b"];
            if reverse {
                keys.reverse();
            }
            for key in keys {
                store.save(key, &[ChatMessage::user(key)]).unwrap();
            }
            let a_path = state.paths.session_file("ws:a:b");
            let a_history = std::fs::read(&a_path).unwrap();
            let a_events = state.paths.session_events_file("ws:a:b");
            std::fs::create_dir_all(a_events.parent().unwrap()).unwrap();
            std::fs::write(&a_events, "events sentinel").unwrap();
            let a_marker = a_path.with_extension("dream_counted");
            std::fs::write(&a_marker, "marker sentinel").unwrap();

            let sessions = list(state).await["sessions"].as_array().unwrap().clone();
            assert_eq!(sessions.len(), 2);
            assert_ne!(
                sessions[0]["id"], sessions[1]["id"],
                "list IDs must preserve literal separators"
            );
            let a_id = sessions.iter().find(|s| s["chat_id"] == "a:b").unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string();
            let b_id = sessions.iter().find(|s| s["chat_id"] == "a_b").unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string();
            for (id, key) in [(&a_id, "ws:a:b"), (&b_id, "ws:a_b")] {
                let (status, body) = json(
                    handle_session_get(
                        State(state.clone()),
                        AxumPath(id.clone()),
                        Query(AgentScopedQuery { agent: None }),
                    )
                    .await,
                )
                .await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(body["messages"][0]["content"], key);
            }

            for operation in ["get", "rename", "delete"] {
                let response = match operation {
                    "get" => handle_session_get(
                        State(state.clone()),
                        AxumPath("a_b".into()),
                        Query(AgentScopedQuery { agent: None }),
                    )
                    .await
                    .into_response(),
                    "rename" => handle_session_rename(
                        State(state.clone()),
                        AxumPath("a_b".into()),
                        Query(AgentScopedQuery { agent: None }),
                        Json(RenameRequest {
                            name: "wrong".into(),
                        }),
                    )
                    .await
                    .into_response(),
                    _ => handle_session_delete(
                        State(state.clone()),
                        AxumPath("a_b".into()),
                        Query(AgentScopedQuery { agent: None }),
                    )
                    .await
                    .into_response(),
                };
                assert!(
                    response.status().is_client_error(),
                    "ambiguous legacy {operation} must fail"
                );
            }

            let (_, rename) = json(
                handle_session_rename(
                    State(state.clone()),
                    AxumPath(b_id.clone()),
                    Query(AgentScopedQuery { agent: None }),
                    Json(RenameRequest {
                        name: "B renamed".into(),
                    }),
                )
                .await,
            )
            .await;
            assert_eq!(rename["status"], "ok");
            let renamed = list(state).await;
            let renamed = renamed["sessions"].as_array().unwrap();
            assert_eq!(
                renamed.iter().find(|s| s["id"] == b_id).unwrap()["name"],
                "B renamed"
            );
            assert_ne!(
                renamed.iter().find(|s| s["id"] == a_id).unwrap()["name"],
                "B renamed"
            );

            let (_, deleted) = json(
                handle_session_delete(
                    State(state.clone()),
                    AxumPath(b_id),
                    Query(AgentScopedQuery { agent: None }),
                )
                .await,
            )
            .await;
            assert_eq!(deleted["status"], "deleted");
            assert_eq!(std::fs::read(&a_path).unwrap(), a_history);
            assert_eq!(
                std::fs::read_to_string(&a_events).unwrap(),
                "events sentinel"
            );
            assert_eq!(
                std::fs::read_to_string(&a_marker).unwrap(),
                "marker sentinel"
            );
            assert!(store.load("ws:a_b").unwrap().is_empty());
            assert_eq!(store.load("ws:a:b").unwrap()[0].content, "ws:a:b");
        }
    }
}
