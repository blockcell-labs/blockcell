use super::*;
// ---------------------------------------------------------------------------
// P1: Cron management endpoints
// ---------------------------------------------------------------------------

/// GET /v1/cron — list all cron jobs
pub(super) async fn handle_cron_list(
    State(state): State<GatewayState>,
    Query(agent): Query<AgentScopedQuery>,
) -> impl IntoResponse {
    let (_, cron_service) = match cron_service_for_agent(&state, agent.agent.as_deref()) {
        Ok(value) => value,
        Err(err) => return Json(serde_json::json!({ "error": err })),
    };
    let _ = cron_service.load().await;
    let jobs = cron_service.list_jobs().await;
    let jobs_json: Vec<serde_json::Value> = jobs
        .iter()
        .map(|j| serde_json::to_value(j).unwrap_or_default())
        .collect();

    let count = jobs_json.len();
    Json(serde_json::json!({
        "jobs": jobs_json,
        "count": count,
    }))
}

#[derive(Deserialize)]
pub(super) struct CronCreateRequest {
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

fn resolve_cron_skill_payload_kind(paths: &Paths, skill_name: Option<&str>) -> &'static str {
    let Some(skill_name) = skill_name else {
        return "agent_turn";
    };

    let user_dir = paths.skills_dir().join(skill_name);
    let builtin_dir = paths.builtin_skills_dir().join(skill_name);

    let has_rhai = user_dir.join("SKILL.rhai").exists() || builtin_dir.join("SKILL.rhai").exists();
    let has_py = user_dir.join("SKILL.py").exists() || builtin_dir.join("SKILL.py").exists();

    if has_rhai {
        "skill_rhai"
    } else if has_py {
        "skill_python"
    } else {
        // Keep backward-compatible behavior when script type is unknown.
        "skill_rhai"
    }
}

/// POST /v1/cron — create a cron job
pub(super) async fn handle_cron_create(
    State(state): State<GatewayState>,
    Query(agent): Query<AgentScopedQuery>,
    Json(req): Json<CronCreateRequest>,
) -> impl IntoResponse {
    let now_ms = chrono::Utc::now().timestamp_millis();

    let schedule = if let Some(at_ms) = req.at_ms {
        JobSchedule {
            kind: ScheduleKind::At,
            at_ms: Some(at_ms),
            every_ms: None,
            expr: None,
            tz: None,
        }
    } else if let Some(every) = req.every_seconds {
        JobSchedule {
            kind: ScheduleKind::Every,
            at_ms: None,
            every_ms: Some(every * 1000),
            expr: None,
            tz: None,
        }
    } else if let Some(expr) = req.cron_expr {
        JobSchedule {
            kind: ScheduleKind::Cron,
            at_ms: None,
            every_ms: None,
            expr: Some(expr),
            tz: None,
        }
    } else {
        return Json(
            serde_json::json!({ "error": "Must specify at_ms, every_seconds, or cron_expr" }),
        );
    };

    let agent_id = match resolve_requested_agent_id(&state.config, agent.agent.as_deref()) {
        Ok(agent_id) => agent_id,
        Err(err) => return Json(serde_json::json!({ "error": err })),
    };
    let (_, cron_service) = match cron_service_for_agent(&state, Some(&agent_id)) {
        Ok(value) => value,
        Err(err) => return Json(serde_json::json!({ "error": err })),
    };
    let payload_kind = resolve_cron_skill_payload_kind(
        &state.paths.for_agent(&agent_id),
        req.skill_name.as_deref(),
    );

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
    match cron_service.add_job(job).await {
        Ok(_) => Json(serde_json::json!({ "status": "created", "job_id": job_id })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// DELETE /v1/cron/:id — delete a cron job
pub(super) async fn handle_cron_delete(
    State(state): State<GatewayState>,
    AxumPath(job_id): AxumPath<String>,
    Query(agent): Query<AgentScopedQuery>,
) -> impl IntoResponse {
    let (_, cron_service) = match cron_service_for_agent(&state, agent.agent.as_deref()) {
        Ok(value) => value,
        Err(err) => return Json(serde_json::json!({ "error": err })),
    };
    match cron_service.remove_job(&job_id).await {
        Ok(true) => Json(serde_json::json!({ "status": "deleted", "job_id": job_id })),
        Ok(false) => Json(serde_json::json!({ "status": "not_found", "job_id": job_id })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// POST /v1/cron/:id/run — manually trigger a cron job
pub(super) async fn handle_cron_run(
    State(state): State<GatewayState>,
    AxumPath(job_id): AxumPath<String>,
    Query(agent): Query<AgentScopedQuery>,
) -> impl IntoResponse {
    let agent_id = match resolve_requested_agent_id(&state.config, agent.agent.as_deref()) {
        Ok(agent_id) => agent_id,
        Err(err) => return Json(serde_json::json!({ "error": err })),
    };
    let (_, cron_service) = match cron_service_for_agent(&state, Some(&agent_id)) {
        Ok(value) => value,
        Err(err) => return Json(serde_json::json!({ "error": err })),
    };
    let jobs = cron_service.list_jobs().await;
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
                let kind = if job.payload.kind == "skill_python" {
                    "python"
                } else {
                    "rhai"
                };
                let mut meta = serde_json::json!({
                    "job_id": job.id,
                    "job_name": job.name,
                    "manual_trigger": true,
                    "skill_script": true,
                    "skill_script_kind": kind,
                    "skill_name": job.payload.skill_name,
                });
                if kind == "python" {
                    meta["skill_python"] = serde_json::json!(true);
                } else {
                    meta["skill_rhai"] = serde_json::json!(true);
                }
                meta
            };
            let inbound = with_route_agent_id(
                InboundMessage {
                    channel: "cron".to_string(),
                    account_id: None,
                    sender_id: "cron".to_string(),
                    chat_id: job.id.clone(),
                    content: format!("[Manual trigger] {}", job.payload.message),
                    media: vec![],
                    metadata,
                    timestamp_ms: chrono::Utc::now().timestamp_millis(),
                },
                &agent_id,
            );
            let _ = state.inbound_tx.send(inbound).await;
            Json(serde_json::json!({ "status": "triggered", "job_id": job.id }))
        }
        None => Json(serde_json::json!({ "status": "not_found", "job_id": job_id })),
    }
}
