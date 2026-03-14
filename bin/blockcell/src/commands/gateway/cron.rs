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
        return "reminder";
    };

    let user_dir = paths.skills_dir().join(skill_name);
    let builtin_dir = paths.builtin_skills_dir().join(skill_name);

    let has_rhai = user_dir.join("SKILL.rhai").exists() || builtin_dir.join("SKILL.rhai").exists();
    let has_py = user_dir.join("SKILL.py").exists() || builtin_dir.join("SKILL.py").exists();
    let has_md = user_dir.join("SKILL.md").exists() || builtin_dir.join("SKILL.md").exists();

    if has_rhai {
        "rhai"
    } else if has_py {
        "python"
    } else if has_md {
        "markdown"
    } else {
        "rhai"
    }
}

fn build_manual_cron_inbound(job: &CronJob, agent_id: &str) -> InboundMessage {
    let (content, mut metadata) = match job.payload.kind.as_str() {
        "reminder" => (
            job.payload.message.clone(),
            serde_json::json!({
                "job_id": job.id,
                "job_name": job.name,
                "manual_trigger": true,
                "reminder": true,
                "reminder_message": job.payload.message,
                "deliver": job.payload.deliver,
                "deliver_channel": job.payload.channel,
                "deliver_to": job.payload.to,
            }),
        ),
        "script" => {
            let kind = job.payload.script_kind.as_deref().unwrap_or("rhai");
            let skill_name = job.payload.skill_name.as_deref().unwrap_or("unknown");
            let mut meta = serde_json::json!({
                "job_id": job.id,
                "job_name": job.name,
                "manual_trigger": true,
                "skill_script": true,
                "skill_script_kind": kind,
                "skill_name": skill_name,
                "deliver": job.payload.deliver,
                "deliver_channel": job.payload.channel,
                "deliver_to": job.payload.to,
            });
            if kind == "python" {
                meta["skill_python"] = serde_json::json!(true);
            } else if kind == "markdown" {
                meta["skill_markdown"] = serde_json::json!(true);
            } else {
                meta["skill_rhai"] = serde_json::json!(true);
            }
            (
                format!(
                    "[系统定时任务] 执行技能脚本 {} — {}",
                    skill_name, job.payload.message
                ),
                meta,
            )
        }
        "agent" => (
            job.payload.message.clone(),
            serde_json::json!({
                "job_id": job.id,
                "job_name": job.name,
                "manual_trigger": true,
                "cron_agent": true,
                "deliver": job.payload.deliver,
                "deliver_channel": job.payload.channel,
                "deliver_to": job.payload.to,
            }),
        ),
        _ => (
            job.payload.message.clone(),
            serde_json::json!({
                "job_id": job.id,
                "job_name": job.name,
                "manual_trigger": true,
                "deliver": job.payload.deliver,
                "deliver_channel": job.payload.channel,
                "deliver_to": job.payload.to,
            }),
        ),
    };
    if let Some(obj) = metadata.as_object_mut() {
        obj.entry("route_agent_id".to_string())
            .or_insert_with(|| serde_json::json!(agent_id));
    }

    with_route_agent_id(
        InboundMessage {
            channel: "cron".to_string(),
            account_id: None,
            sender_id: "cron".to_string(),
            chat_id: job.id.clone(),
            content,
            media: vec![],
            metadata,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        },
        agent_id,
    )
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
    let payload_kind = if req.skill_name.is_some() {
        "script"
    } else {
        "reminder"
    };
    let script_kind = req.skill_name.as_deref().map(|skill_name| {
        resolve_cron_skill_payload_kind(&state.paths.for_agent(&agent_id), Some(skill_name))
    });

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
            script_kind: script_kind.map(|value| value.to_string()),
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
            let inbound = build_manual_cron_inbound(job, &agent_id);
            let _ = state.inbound_tx.send(inbound).await;
            Json(serde_json::json!({ "status": "triggered", "job_id": job.id }))
        }
        None => Json(serde_json::json!({ "status": "not_found", "job_id": job_id })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_job(kind: &str) -> CronJob {
        let now_ms = chrono::Utc::now().timestamp_millis();
        CronJob {
            id: "job-1".to_string(),
            name: "test job".to_string(),
            enabled: true,
            schedule: JobSchedule {
                kind: ScheduleKind::At,
                at_ms: Some(now_ms + 60_000),
                every_ms: None,
                expr: None,
                tz: None,
            },
            payload: JobPayload {
                kind: kind.to_string(),
                message: "payload body".to_string(),
                deliver: true,
                channel: Some("ws".to_string()),
                to: Some("manual:test".to_string()),
                script_kind: Some("markdown".to_string()),
                skill_name: Some("weather".to_string()),
            },
            state: JobState::default(),
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            delete_after_run: false,
        }
    }

    #[test]
    fn test_build_manual_cron_inbound_routes_reminder_with_delivery_metadata() {
        let inbound = build_manual_cron_inbound(&test_job("reminder"), "default");
        assert_eq!(inbound.content, "payload body");
        assert_eq!(
            inbound.metadata.get("reminder").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            inbound
                .metadata
                .get("deliver_channel")
                .and_then(|v| v.as_str()),
            Some("ws")
        );
        assert_eq!(
            inbound.metadata.get("deliver_to").and_then(|v| v.as_str()),
            Some("manual:test")
        );
        assert!(inbound.metadata.get("skill_script").is_none());
    }

    #[test]
    fn test_build_manual_cron_inbound_routes_script_with_current_kind_flags() {
        let inbound = build_manual_cron_inbound(&test_job("script"), "default");
        assert!(inbound.content.contains("执行技能脚本 weather"));
        assert_eq!(
            inbound
                .metadata
                .get("skill_script")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            inbound
                .metadata
                .get("skill_script_kind")
                .and_then(|v| v.as_str()),
            Some("markdown")
        );
        assert_eq!(
            inbound
                .metadata
                .get("deliver_channel")
                .and_then(|v| v.as_str()),
            Some("ws")
        );
        assert_eq!(
            inbound.metadata.get("deliver_to").and_then(|v| v.as_str()),
            Some("manual:test")
        );
    }
}
