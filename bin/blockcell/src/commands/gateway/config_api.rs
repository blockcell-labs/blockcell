use super::*;

fn load_config_or_state(state: &GatewayState) -> Config {
    Config::load(&state.paths.config_file()).unwrap_or_else(|_| state.config.clone())
}

fn load_config_value_or_state(state: &GatewayState) -> serde_json::Value {
    let config_path = state.paths.config_file();
    match std::fs::read_to_string(&config_path) {
        Ok(content) => blockcell_core::config::parse_json5_value(&content)
            .unwrap_or_else(|_| serde_json::to_value(&state.config).unwrap_or_default()),
        Err(_) => serde_json::to_value(&state.config).unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// P1: Config management endpoints
// ---------------------------------------------------------------------------

/// GET /v1/config — get config with credentials masked.
/// Always reads from disk so edits via PUT are immediately reflected.
pub(super) async fn handle_config_get(State(state): State<GatewayState>) -> impl IntoResponse {
    let mut value = load_config_value_or_state(&state);
    blockcell_core::config::mask_secret_values(&mut value);
    Json(value)
}

/// GET /v1/config/raw — get raw config.json5 text
pub(super) async fn handle_config_raw_get(State(state): State<GatewayState>) -> impl IntoResponse {
    let config_path = state.paths.config_file();
    let content = match tokio::fs::read_to_string(&config_path).await {
        Ok(content) => content,
        Err(_) => blockcell_core::config::stringify_json5_pretty(&state.config)
            .unwrap_or_else(|_| "{}".to_string()),
    };

    Json(serde_json::json!({
        "status": "ok",
        "path": config_path.display().to_string(),
        "content": content,
    }))
}

#[derive(Deserialize)]
pub(super) struct ConfigUpdateRequest {
    #[serde(flatten)]
    config: serde_json::Value,
}

#[derive(Deserialize)]
pub(super) struct ConfigRawUpdateRequest {
    content: String,
}

/// PUT /v1/config — update config with structured JSON payload
pub(super) async fn handle_config_update(
    State(state): State<GatewayState>,
    Json(req): Json<ConfigUpdateRequest>,
) -> impl IntoResponse {
    let _config_guard = state.config_write_lock.lock().await;
    let config_path = state.paths.config_file();

    // The read endpoint masks credentials; a client echoing the mask back means
    // "keep the stored value", so restore them from disk before applying.
    let current = load_config_value_or_state(&state);
    let mut incoming = req.config;
    blockcell_core::config::restore_masked_secrets(&current, &mut incoming);

    match serde_json::from_value::<Config>(incoming) {
        Ok(new_config) => match new_config.save(&config_path) {
            Ok(_) => Json(
                serde_json::json!({ "status": "ok", "message": "Config updated. Restart gateway to apply changes." }),
            ),
            Err(e) => Json(serde_json::json!({ "status": "error", "message": format!("{}", e) })),
        },
        Err(e) => Json(
            serde_json::json!({ "status": "error", "message": format!("Invalid config: {}", e) }),
        ),
    }
}

/// PUT /v1/config/raw — validate and write raw config.json5 text as-is
pub(super) async fn handle_config_raw_put(
    State(state): State<GatewayState>,
    Json(req): Json<ConfigRawUpdateRequest>,
) -> impl IntoResponse {
    let _config_guard = state.config_write_lock.lock().await;
    let config_path = state.paths.config_file();
    match blockcell_core::config::write_raw_validated_config_json5(&config_path, &req.content) {
        Ok(_) => Json(serde_json::json!({
            "status": "ok",
            "message": "Config updated. Restart gateway to apply changes.",
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "message": format!("Invalid config.json5: {}", e),
        })),
    }
}

/// POST /v1/config/reload — validate config.json5 from disk
pub(super) async fn handle_config_reload(State(state): State<GatewayState>) -> impl IntoResponse {
    let config_path = state.paths.config_file();

    match tokio::fs::read_to_string(&config_path).await {
        Ok(content) => match blockcell_core::config::validate_config_json5_str(&content) {
            Ok(_) => Json(serde_json::json!({
                "status": "ok",
                "message": "Config.json5 validated successfully. Note: Full reload still requires gateway restart for some settings.",
            })),
            Err(e) => Json(serde_json::json!({
                "status": "error",
                "message": format!("Invalid config.json5: {}", e),
            })),
        },
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "message": format!("Failed to read config file: {}", e),
        })),
    }
}

/// POST /v1/config/test-provider — test a provider connection
pub(super) async fn handle_config_test_provider(
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    use blockcell_providers::Provider;

    let test_messages = vec![blockcell_core::types::ChatMessage::user("Say 'ok'")];

    if let Some(content) = req.get("content").and_then(|v| v.as_str()) {
        let config = match blockcell_core::config::validate_config_json5_str(content) {
            Ok(config) => config,
            Err(e) => {
                return Json(serde_json::json!({
                    "status": "error",
                    "message": format!("Invalid config.json5: {}", e),
                }))
            }
        };

        let model = req
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(config.agents.defaults.model.as_str());
        let explicit_provider = req
            .get("provider")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .or(config.agents.defaults.provider.as_deref());

        let provider = match blockcell_providers::create_provider(&config, model, explicit_provider)
        {
            Ok(provider) => provider,
            Err(e) => {
                return Json(serde_json::json!({
                    "status": "error",
                    "message": format!("{}", e),
                }))
            }
        };

        return match provider.chat(&test_messages, &[]).await {
            Ok(_) => Json(
                serde_json::json!({ "status": "ok", "message": "Provider connection successful" }),
            ),
            Err(e) => Json(serde_json::json!({ "status": "error", "message": format!("{}", e) })),
        };
    }

    let model = req
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("gpt-3.5-turbo");
    let api_key = req.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
    let api_base = req.get("api_base").and_then(|v| v.as_str());
    let proxy = req.get("proxy").and_then(|v| v.as_str());

    if api_key.is_empty() {
        return Json(serde_json::json!({ "status": "error", "message": "api_key is required" }));
    }

    let provider = match blockcell_providers::OpenAIProvider::new_with_proxy(
        api_key,
        api_base,
        model,
        100,
        0.0,
        proxy,
        None,
        &[],
        blockcell_core::config::ToolCallMode::Native,
        "",
        None,
    ) {
        Ok(p) => p,
        Err(e) => {
            return Json(serde_json::json!({
                "status": "error",
                "message": format!("Failed to create provider: {}", e)
            }));
        }
    };

    match provider.chat(&test_messages, &[]).await {
        Ok(_) => {
            Json(serde_json::json!({ "status": "ok", "message": "Provider connection successful" }))
        }
        Err(e) => Json(serde_json::json!({ "status": "error", "message": format!("{}", e) })),
    }
}

/// GET /v1/ghost/config — get scheduled Ghost maintenance configuration.
///
/// This endpoint is kept for compatibility. Embedded Ghost learning runs inside
/// AgentRuntime during normal assistant turns and is not controlled by
/// `agents.ghost.enabled`.
pub(super) async fn handle_ghost_config_get(
    State(state): State<GatewayState>,
) -> impl IntoResponse {
    let ghost = load_config_or_state(&state).agents.ghost;
    Json(ghost)
}

/// PUT /v1/ghost/config — update scheduled Ghost maintenance configuration.
///
/// This does not enable or disable embedded Ghost learning.
pub(super) async fn handle_ghost_config_update(
    State(state): State<GatewayState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let _config_guard = state.config_write_lock.lock().await;
    let config_path = state.paths.config_file();
    let mut config = load_config_or_state(&state);

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
pub(super) async fn handle_ghost_activity(
    State(state): State<GatewayState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let sessions_dir = state.paths.sessions_dir();
    let limit: usize = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);

    // 将阻塞 I/O 操作移至 spawn_blocking，避免阻塞 Tokio 工作线程
    let activities: Vec<serde_json::Value> = tokio::task::spawn_blocking(move || {
        let mut activities = Vec::new();

        let Ok(entries) = std::fs::read_dir(&sessions_dir) else {
            return activities;
        };
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
                let raw_ts = session_id
                    .strip_prefix("ghost_")
                    .unwrap_or(&session_id)
                    .to_string();
                let timestamp = chrono::NaiveDateTime::parse_from_str(&raw_ts, "%Y%m%d_%H%M%S")
                    .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or(raw_ts);
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
                                if let Some(calls) =
                                    msg.get("tool_calls").and_then(|v| v.as_array())
                                {
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
        activities
    })
    .await
    .unwrap_or_default();

    let count = activities.len();
    Json(serde_json::json!({
        "activities": activities,
        "count": count,
    }))
}

pub(super) async fn handle_ghost_metrics(State(state): State<GatewayState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "metrics": blockcell_agent::ghost_metrics_summary(&state.paths),
    }))
}

pub(super) async fn handle_ghost_model_options_get(
    State(state): State<GatewayState>,
) -> impl IntoResponse {
    let config = load_config_or_state(&state);

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
    let (default_model, _, _) = active_model_and_provider(&config);

    Json(serde_json::json!({
        "providers": providers,
        "default_model": default_model,
    }))
}
