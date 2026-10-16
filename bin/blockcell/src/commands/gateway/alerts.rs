use super::*;
// ---------------------------------------------------------------------------
// P2: Alert management endpoints
// ---------------------------------------------------------------------------

/// GET /v1/alerts — list all alert rules
pub(super) async fn handle_alerts_list(State(state): State<GatewayState>) -> impl IntoResponse {
    let path = state.paths.workspace().join("alerts").join("rules.json");
    if !path.exists() {
        return Json(serde_json::json!({ "rules": [], "count": 0 }));
    }
    match tokio::fs::read_to_string(&path).await {
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
pub(super) struct AlertCreateRequest {
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

fn default_cooldown() -> u64 {
    300
}
fn default_check_interval() -> u64 {
    60
}

/// POST /v1/alerts — create an alert rule
pub(super) async fn handle_alerts_create(
    State(state): State<GatewayState>,
    Json(req): Json<AlertCreateRequest>,
) -> impl IntoResponse {
    // Serialize the read-modify-write: without the lock two concurrent requests read the
    // same snapshot and the later write drops the other's rule.
    let _config_guard = state.config_write_lock.lock().await;

    let alerts_dir = state.paths.workspace().join("alerts");
    let _ = tokio::fs::create_dir_all(&alerts_dir).await;
    let path = alerts_dir.join("rules.json");

    let mut store: serde_json::Value = if path.exists() {
        tokio::fs::read_to_string(&path)
            .await
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

    match tokio::fs::write(
        &path,
        serde_json::to_string_pretty(&store).unwrap_or_default(),
    )
    .await
    {
        Ok(_) => Json(serde_json::json!({ "status": "created", "rule_id": rule_id })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// PUT /v1/alerts/:id — update an alert rule
pub(super) async fn handle_alerts_update(
    State(state): State<GatewayState>,
    AxumPath(rule_id): AxumPath<String>,
    Json(updates): Json<serde_json::Value>,
) -> impl IntoResponse {
    // Same read-modify-write protection as `handle_alerts_create`.
    let _config_guard = state.config_write_lock.lock().await;

    let path = state.paths.workspace().join("alerts").join("rules.json");
    if !path.exists() {
        return Json(serde_json::json!({ "error": "No alert rules found" }));
    }

    let mut store: serde_json::Value = match tokio::fs::read_to_string(&path)
        .await
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
                        rule_obj.insert(
                            "updated_at".to_string(),
                            serde_json::json!(chrono::Utc::now().timestamp_millis()),
                        );
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

    match tokio::fs::write(
        &path,
        serde_json::to_string_pretty(&store).unwrap_or_default(),
    )
    .await
    {
        Ok(_) => Json(serde_json::json!({ "status": "updated", "rule_id": rule_id })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// DELETE /v1/alerts/:id — delete an alert rule
pub(super) async fn handle_alerts_delete(
    State(state): State<GatewayState>,
    AxumPath(rule_id): AxumPath<String>,
) -> impl IntoResponse {
    // Same read-modify-write protection as `handle_alerts_create`.
    let _config_guard = state.config_write_lock.lock().await;

    let path = state.paths.workspace().join("alerts").join("rules.json");
    if !path.exists() {
        return Json(serde_json::json!({ "status": "not_found" }));
    }

    let mut store: serde_json::Value = match tokio::fs::read_to_string(&path)
        .await
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

    match tokio::fs::write(
        &path,
        serde_json::to_string_pretty(&store).unwrap_or_default(),
    )
    .await
    {
        Ok(_) => Json(serde_json::json!({ "status": "deleted", "rule_id": rule_id })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// GET /v1/alerts/history — alert trigger history
pub(super) async fn handle_alerts_history(State(state): State<GatewayState>) -> impl IntoResponse {
    let path = state.paths.workspace().join("alerts").join("rules.json");
    if !path.exists() {
        return Json(serde_json::json!({ "history": [] }));
    }

    let store: serde_json::Value = match tokio::fs::read_to_string(&path)
        .await
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
            let name = rule
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let rule_id = rule.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let state = rule.get("state").cloned().unwrap_or_default();
            let trigger_count = state
                .get("trigger_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
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
        let ta = a
            .get("last_triggered_at")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let tb = b
            .get("last_triggered_at")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        tb.cmp(&ta)
    });

    Json(serde_json::json!({ "history": history }))
}
