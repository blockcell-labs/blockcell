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
    blockcell_core::config::mask_config_secret_values(&mut value);
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
    if let Err(e) = blockcell_core::config::restore_masked_secrets(&current, &mut incoming) {
        return Json(serde_json::json!({"status":"error", "message": e.to_string()}));
    }

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
        .unwrap_or("gpt-6-luna");
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

#[cfg(test)]
mod r4_config_tests {
    use super::*;
    use axum::body::to_bytes;
    struct ConfigFixture {
        state: GatewayState,
        directory: std::path::PathBuf,
    }

    impl ConfigFixture {
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

    impl Drop for ConfigFixture {
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

    fn write_config(fixture: &ConfigFixture, value: &serde_json::Value) -> Vec<u8> {
        std::fs::create_dir_all(&fixture.directory).unwrap();
        let bytes = serde_json::to_vec_pretty(value).unwrap();
        std::fs::write(fixture.state.paths.config_file(), &bytes).unwrap();
        bytes
    }

    #[tokio::test]
    async fn r4_config_get_preserves_numeric_limits_and_masks_strings() {
        let fixture = ConfigFixture::new();
        write_config(
            &fixture,
            &serde_json::json!({
                "providers": {"p": {"apiKey": "provider-secret"}},
                "agents": {"list": [{"id":"ops", "maxTokens":1024, "maxContextTokens":4096}, {"id":"dev", "maxTokens":8192, "maxContextTokens":32768}]}
            }),
        );
        let (_, body) = json(handle_config_get(State(fixture.state.clone())).await).await;
        assert_eq!(body["agents"]["list"][0]["maxTokens"], 1024);
        assert_eq!(body["agents"]["list"][1]["maxContextTokens"], 32768);
        assert_eq!(
            body["providers"]["p"]["apiKey"],
            blockcell_core::config::SECRET_MASK
        );
    }

    #[tokio::test]
    async fn r4_structured_edit_preserves_remaining_agent_limits_and_extension_secret() {
        let fixture = ConfigFixture::new();
        write_config(
            &fixture,
            &serde_json::json!({"agents":{"list":[
                {"id":"ops", "maxTokens":1024, "maxContextTokens":4096, "apiKey":"ops-secret"},
                {"id":"dev", "maxTokens":8192, "maxContextTokens":32768, "apiKey":"dev-secret"}
            ]}}),
        );
        let (_, mut incoming) = json(handle_config_get(State(fixture.state.clone())).await).await;
        incoming["agents"]["list"].as_array_mut().unwrap().remove(0);
        let (_, response) = json(
            handle_config_update(
                State(fixture.state.clone()),
                Json(ConfigUpdateRequest { config: incoming }),
            )
            .await,
        )
        .await;
        assert_eq!(response["status"], "ok");
        let saved = blockcell_core::config::parse_json5_value(
            &std::fs::read_to_string(fixture.state.paths.config_file()).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["agents"]["list"][0]["id"], "dev");
        assert_eq!(saved["agents"]["list"][0]["maxTokens"], 8192);
        assert_eq!(saved["agents"]["list"][0]["maxContextTokens"], 32768);
        assert_eq!(saved["agents"]["list"][0]["apiKey"], "dev-secret");
    }

    #[tokio::test]
    async fn r4_unresolvable_or_ambiguous_secret_masks_are_rejected_before_writing() {
        let fixture = ConfigFixture::new();
        for incoming in [
            serde_json::json!({"providers":{"new":{"apiKey": blockcell_core::config::SECRET_MASK}}}),
            serde_json::json!({"agents":{"list":[{"id":"new", "apiKey":blockcell_core::config::SECRET_MASK}]}}),
            serde_json::json!({"agents":{"list":[{"id":"ops", "apiKey":blockcell_core::config::SECRET_MASK}, {"id":"ops", "apiKey":blockcell_core::config::SECRET_MASK}]}}),
        ] {
            let sentinel = write_config(
                &fixture,
                &serde_json::json!({"providers":{"p":{"apiKey":"provider-secret"}}, "agents":{"list":[{"id":"ops", "apiKey":"ops-secret"}]}}),
            );
            let (_, response) = json(
                handle_config_update(
                    State(fixture.state.clone()),
                    Json(ConfigUpdateRequest { config: incoming }),
                )
                .await,
            )
            .await;
            assert_eq!(response["status"], "error");
            assert_eq!(
                std::fs::read(fixture.state.paths.config_file()).unwrap(),
                sentinel
            );
        }
    }

    #[tokio::test]
    async fn r4_structured_raw_and_reload_share_startup_semantic_validation() {
        let fixture = ConfigFixture::new();
        for invalid in [
            serde_json::json!({"cronTickIntervalSecs":0}),
            serde_json::json!({"cronTickIntervalSecs":3601}),
            serde_json::json!({"defaultTimezone":"Mars/Olympus"}),
            serde_json::json!({"tools":{"exec":{"approvalPolicy":"invalid"}}}),
        ] {
            let sentinel = write_config(
                &fixture,
                &serde_json::json!({"cronTickIntervalSecs":1, "defaultTimezone":"UTC"}),
            );
            let (_, response) = json(
                handle_config_update(
                    State(fixture.state.clone()),
                    Json(ConfigUpdateRequest {
                        config: invalid.clone(),
                    }),
                )
                .await,
            )
            .await;
            assert_eq!(response["status"], "error", "structured accepted {invalid}");
            assert_eq!(
                std::fs::read(fixture.state.paths.config_file()).unwrap(),
                sentinel
            );
            let (_, response) = json(
                handle_config_raw_put(
                    State(fixture.state.clone()),
                    Json(ConfigRawUpdateRequest {
                        content: invalid.to_string(),
                    }),
                )
                .await,
            )
            .await;
            assert_eq!(response["status"], "error", "raw accepted {invalid}");
            assert_eq!(
                std::fs::read(fixture.state.paths.config_file()).unwrap(),
                sentinel
            );
            let invalid_bytes = write_config(&fixture, &invalid);
            let (_, response) =
                json(handle_config_reload(State(fixture.state.clone())).await).await;
            assert_eq!(response["status"], "error", "reload accepted {invalid}");
            assert_eq!(
                std::fs::read(fixture.state.paths.config_file()).unwrap(),
                invalid_bytes
            );
            assert!(Config::load(&fixture.state.paths.config_file()).is_err());
        }
    }

    #[tokio::test]
    async fn r4_channel_partial_update_validates_the_whole_config() {
        let fixture = ConfigFixture::new();
        let sentinel = write_config(
            &fixture,
            &serde_json::json!({"cronTickIntervalSecs":0, "channels":{"telegram":{"token":"old"}}}),
        );
        let request: super::super::channels::ChannelUpdateRequest =
            serde_json::from_value(serde_json::json!({"fields":{"token":"new"}})).unwrap();
        let (_, response) = json(
            super::super::channels::handle_channel_update(
                State(fixture.state.clone()),
                AxumPath("telegram".into()),
                Json(request),
            )
            .await,
        )
        .await;
        assert_eq!(response["status"], "error");
        assert_eq!(
            std::fs::read(fixture.state.paths.config_file()).unwrap(),
            sentinel
        );
    }
}
