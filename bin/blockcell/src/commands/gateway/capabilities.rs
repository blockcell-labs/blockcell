use super::*;
// ---------------------------------------------------------------------------
// P1: Tools / Skills / Evolution endpoints
// ---------------------------------------------------------------------------

/// GET /v1/tools — list all registered tools
pub(super) async fn handle_tools(State(state): State<GatewayState>) -> impl IntoResponse {
    let names = state.tool_registry.tool_names();
    let tools: Vec<serde_json::Value> = names
        .iter()
        .map(|name| {
            if let Some(tool) = state.tool_registry.get(name) {
                let schema = tool.schema();
                serde_json::json!({
                    "name": schema.name,
                    "description": schema.description,
                })
            } else {
                serde_json::json!({ "name": name })
            }
        })
        .collect();

    let count = tools.len();
    Json(serde_json::json!({
        "tools": tools,
        "count": count,
    }))
}

/// GET /v1/skills — list skills
pub(super) async fn handle_skills(State(state): State<GatewayState>) -> impl IntoResponse {
    // Load disabled toggles once for all skills
    let toggles_path = state.paths.toggles_file();
    let disabled_skills: std::collections::HashSet<String> = std::fs::read_to_string(&toggles_path)
        .ok()
        .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
        .and_then(|v| v.get("skills").and_then(|s| s.as_object()).cloned())
        .map(|obj| {
            obj.into_iter()
                .filter(|(_, v)| v == &serde_json::Value::Bool(false))
                .map(|(k, _)| k)
                .collect()
        })
        .unwrap_or_default();

    let mut skills = Vec::new();

    // Scan user skills directory
    let skills_dir = state.paths.skills_dir();
    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                let meta_path = entry.path().join("meta.yaml");
                let has_rhai = entry.path().join("SKILL.rhai").exists();
                let has_py = entry.path().join("SKILL.py").exists();
                let has_md = entry.path().join("SKILL.md").exists();
                let enabled = !disabled_skills.contains(&name);

                let mut skill_info = serde_json::json!({
                    "name": name,
                    "source": "user",
                    "has_rhai": has_rhai,
                    "has_py": has_py,
                    "has_md": has_md,
                    "enabled": enabled,
                });

                if meta_path.exists() {
                    if let Ok(content) = std::fs::read_to_string(&meta_path) {
                        // Parse meta.yaml properly via serde_yaml, then convert to JSON value
                        if let Ok(parsed) = serde_yaml::from_str::<serde_json::Value>(&content) {
                            // Expose triggers at top-level for easy frontend access
                            if let Some(triggers) = parsed.get("triggers") {
                                skill_info["triggers"] = triggers.clone();
                            }
                            if let Some(desc) = parsed.get("description") {
                                skill_info["description"] = desc.clone();
                            }
                            if let Some(always) = parsed.get("always") {
                                skill_info["always"] = always.clone();
                            }
                            skill_info["meta"] = parsed;
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
                if skills
                    .iter()
                    .any(|s| s.get("name").and_then(|v| v.as_str()) == Some(&name))
                {
                    continue;
                }
                let has_rhai = entry.path().join("SKILL.rhai").exists();
                let has_py = entry.path().join("SKILL.py").exists();
                let has_md = entry.path().join("SKILL.md").exists();
                let enabled = !disabled_skills.contains(&name);
                let mut skill_info = serde_json::json!({
                    "name": name,
                    "source": "builtin",
                    "has_rhai": has_rhai,
                    "has_py": has_py,
                    "has_md": has_md,
                    "enabled": enabled,
                });
                let meta_path = entry.path().join("meta.yaml");
                if meta_path.exists() {
                    if let Ok(content) = std::fs::read_to_string(&meta_path) {
                        if let Ok(parsed) = serde_yaml::from_str::<serde_json::Value>(&content) {
                            if let Some(triggers) = parsed.get("triggers") {
                                skill_info["triggers"] = triggers.clone();
                            }
                            if let Some(desc) = parsed.get("description") {
                                skill_info["description"] = desc.clone();
                            }
                            if let Some(always) = parsed.get("always") {
                                skill_info["always"] = always.clone();
                            }
                            skill_info["meta"] = parsed;
                        }
                    }
                }
                skills.push(skill_info);
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
pub(super) struct SkillSearchRequest {
    query: String,
}

pub(super) async fn handle_skills_search(
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
        let has_py = dir.join("SKILL.py").exists();
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
                            let t = line
                                .trim()
                                .trim_start_matches("- ")
                                .trim_matches('"')
                                .trim_matches('\'');
                            if !triggers_str.is_empty() {
                                triggers_str.push_str(", ");
                            }
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
            "has_py": has_py,
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
                if results
                    .iter()
                    .any(|r| r.get("name").and_then(|v| v.as_str()) == Some(&name))
                {
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

/// GET /v1/evolution — list evolution records (lightweight: strips heavy fields for list view)
pub(super) async fn handle_evolution(State(state): State<GatewayState>) -> impl IntoResponse {
    let records_dir = state.paths.workspace().join("evolution_records");
    let mut records = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&records_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(mut record) = serde_json::from_str::<serde_json::Value>(&content) {
                        // Strip heavy fields not needed for the list view
                        if let Some(patch) = record.get_mut("patch").and_then(|p| p.as_object_mut())
                        {
                            patch.remove("diff");
                        }
                        if let Some(ctx) = record.get_mut("context").and_then(|c| c.as_object_mut())
                        {
                            ctx.remove("source_snippet");
                            ctx.remove("tool_schemas");
                        }
                        // Strip feedback_history bodies (keep attempt/stage/timestamp only)
                        if let Some(hist) = record
                            .get_mut("feedback_history")
                            .and_then(|h| h.as_array_mut())
                        {
                            for entry in hist.iter_mut() {
                                if let Some(obj) = entry.as_object_mut() {
                                    obj.remove("previous_code");
                                    obj.remove("feedback");
                                }
                            }
                        }
                        records.push(record);
                    }
                }
            }
        }
    }

    // Sort by updated_at descending
    records.sort_by(|a, b| {
        let ta = a.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0);
        let tb = b.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0);
        tb.cmp(&ta)
    });

    let count = records.len();
    Json(serde_json::json!({
        "records": records,
        "count": count,
    }))
}

/// GET /v1/evolution/:id — single evolution record detail
pub(super) async fn handle_evolution_detail(
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
pub(super) async fn handle_evolution_tool_evolutions(
    State(state): State<GatewayState>,
) -> impl IntoResponse {
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

/// GET /v1/pool/status — return model pool configuration and runtime status
pub(super) async fn handle_pool_status(State(state): State<GatewayState>) -> impl IntoResponse {
    let defaults = &state.config.agents.defaults;
    let using_pool = !defaults.model_pool.is_empty();

    // Build pool entries from config for status display
    let entries: Vec<serde_json::Value> = if using_pool {
        defaults
            .model_pool
            .iter()
            .map(|e| {
                serde_json::json!({
                    "model": e.model,
                    "provider": e.provider,
                    "weight": e.weight,
                    "priority": e.priority,
                })
            })
            .collect()
    } else {
        // Single model legacy mode
        vec![serde_json::json!({
            "model": defaults.model,
            "provider": defaults.provider,
            "weight": 1,
            "priority": 1,
        })]
    };

    Json(serde_json::json!({
        "using_pool": using_pool,
        "entries": entries,
        "evolution_model": defaults.evolution_model,
        "evolution_provider": defaults.evolution_provider,
    }))
}

/// Persona files that can be edited via the WebUI
const PERSONA_FILES: &[&str] = &["AGENTS.md", "SOUL.md", "USER.md", "CONTEXT.md", "STYLE.md"];

/// GET /v1/persona/files — list persona files with their content
pub(super) async fn handle_persona_list(State(state): State<GatewayState>) -> impl IntoResponse {
    let workspace = state.paths.workspace();
    let mut files = Vec::new();

    for name in PERSONA_FILES {
        let path = workspace.join(name);
        let content = if path.exists() {
            std::fs::read_to_string(&path).unwrap_or_default()
        } else {
            String::new()
        };
        files.push(serde_json::json!({
            "name": name,
            "exists": path.exists(),
            "content": content,
            "size": content.len(),
        }));
    }

    Json(serde_json::json!({ "files": files }))
}

#[derive(Deserialize)]
pub(super) struct PersonaFileQuery {
    name: String,
}

#[derive(Deserialize)]
pub(super) struct PersonaWriteRequest {
    name: String,
    content: String,
}

/// GET /v1/persona/file?name=AGENTS.md — read a persona file
pub(super) async fn handle_persona_read(
    State(state): State<GatewayState>,
    Query(params): Query<PersonaFileQuery>,
) -> impl IntoResponse {
    // Validate file name
    if !PERSONA_FILES.contains(&params.name.as_str()) {
        return Json(serde_json::json!({ "error": "Invalid file name" }));
    }
    let path = state.paths.workspace().join(&params.name);
    let content = if path.exists() {
        std::fs::read_to_string(&path).unwrap_or_default()
    } else {
        String::new()
    };
    Json(serde_json::json!({
        "name": params.name,
        "content": content,
        "exists": path.exists(),
    }))
}

/// PUT /v1/persona/file — write a persona file
pub(super) async fn handle_persona_write(
    State(state): State<GatewayState>,
    Json(req): Json<PersonaWriteRequest>,
) -> impl IntoResponse {
    // Validate file name
    if !PERSONA_FILES.contains(&req.name.as_str()) {
        return Json(serde_json::json!({ "status": "error", "message": "Invalid file name" }));
    }
    let path = state.paths.workspace().join(&req.name);
    match std::fs::write(&path, &req.content) {
        Ok(_) => {
            Json(serde_json::json!({ "status": "ok", "name": req.name, "size": req.content.len() }))
        }
        Err(e) => Json(serde_json::json!({ "status": "error", "message": format!("{}", e) })),
    }
}

#[derive(Deserialize)]
pub(super) struct EvolutionTriggerRequest {
    skill_name: String,
    description: String,
}

/// POST /v1/evolution/trigger — manually trigger a skill evolution
pub(super) async fn handle_evolution_trigger(
    State(state): State<GatewayState>,
    Json(req): Json<EvolutionTriggerRequest>,
) -> impl IntoResponse {
    // Use EvolutionService so active_evolutions is properly updated and tick() can drive the pipeline
    let evo = state.evolution_service.lock().await;
    match evo
        .trigger_manual_evolution(&req.skill_name, &req.description)
        .await
    {
        Ok(evolution_id) => {
            // Auto-disable the skill while it evolves so the old broken version won't run
            let toggles_path = state.paths.toggles_file();
            if let Ok(content) = std::fs::read_to_string(&toggles_path) {
                if let Ok(mut store) = serde_json::from_str::<serde_json::Value>(&content) {
                    let skill_enabled = store
                        .get("skills")
                        .and_then(|s| s.get(&req.skill_name))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true); // absent = enabled
                    if skill_enabled {
                        if store.get("skills").is_none() {
                            store["skills"] = serde_json::json!({});
                        }
                        store["skills"][&req.skill_name] = serde_json::json!(false);
                        let _ = std::fs::write(
                            &toggles_path,
                            serde_json::to_string_pretty(&store).unwrap_or_default(),
                        );
                    }
                }
            } else {
                // toggles file doesn't exist yet — create it with the skill disabled
                let store =
                    serde_json::json!({ "skills": { &req.skill_name: false }, "tools": {} });
                if let Some(parent) = toggles_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(
                    &toggles_path,
                    serde_json::to_string_pretty(&store).unwrap_or_default(),
                );
            }

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
pub(super) async fn handle_evolution_delete(
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
            .and_then(|v| {
                v.get("skill_name")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string())
            });

        return match std::fs::remove_file(&path) {
            Ok(_) => {
                // Clean up in-memory EvolutionService state so the skill can be re-triggered
                if let Some(ref sn) = skill_name {
                    let evo_guard = state.evolution_service.lock().await;
                    let _ = evo_guard.delete_records_by_skill(sn).await;
                }
                // Broadcast WS event for real-time UI refresh
                let _ = state.ws_broadcast.send(
                    serde_json::json!({
                        "type": "evolution_deleted",
                        "id": evolution_id,
                    })
                    .to_string(),
                );
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
                let _ = state.ws_broadcast.send(
                    serde_json::json!({
                        "type": "evolution_deleted",
                        "id": evolution_id,
                    })
                    .to_string(),
                );
                Json(serde_json::json!({ "status": "deleted", "id": evolution_id }))
            }
            Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
        };
    }

    Json(serde_json::json!({ "error": "not_found" }))
}

/// POST /v1/evolution/test — test a completed skill with input
#[derive(Deserialize)]
pub(super) struct EvolutionTestRequest {
    skill_name: String,
    input: String,
}

pub(super) async fn handle_evolution_test(
    State(state): State<GatewayState>,
    Json(req): Json<EvolutionTestRequest>,
) -> impl IntoResponse {
    // Locate the skill directory (user skills take precedence over builtin)
    let skill_dir = state.paths.skills_dir().join(&req.skill_name);
    let builtin_dir = state.paths.builtin_skills_dir().join(&req.skill_name);

    let base_dir = if skill_dir.exists() {
        skill_dir
    } else if builtin_dir.exists() {
        builtin_dir
    } else {
        return Json(serde_json::json!({
            "status": "failed",
            "error": format!("技能 '{}' 未找到", req.skill_name),
        }));
    };

    let has_py = base_dir.join("SKILL.py").exists();
    let has_rhai = base_dir.join("SKILL.rhai").exists();

    let test_pool = match blockcell_providers::ProviderPool::from_config(&state.config) {
        Ok(p) => p,
        Err(e) => {
            return Json(serde_json::json!({
                "status": "failed",
                "error": format!("No LLM provider configured: {}", e),
            }));
        }
    };

    let tool_registry = ToolRegistry::with_defaults();
    let mut runtime = match AgentRuntime::new(
        state.config.clone(),
        state.paths.clone(),
        test_pool,
        tool_registry,
    ) {
        Ok(r) => r,
        Err(e) => {
            return Json(serde_json::json!({
                "status": "failed",
                "error": format!("Failed to create test runtime: {}", e),
            }));
        }
    };

    if let Some(store) = state.memory_store.clone() {
        runtime.set_memory_store(store);
    }

    let start = std::time::Instant::now();

    if has_py || has_rhai {
        // Rhai / Python 技能：SKILL.py / SKILL.rhai 是独立脚本，通过 stdin 接收 JSON 参数并返回结果。
        // 步骤：
        //   1. 读取 SKILL.md + meta.yaml，了解技能的参数规格
        //   2. 用轻量 LLM 调用，把用户自然语言输入解析为符合技能参数规格的 JSON
        //   3. 把该 JSON 作为 stdin 传给脚本执行

        let skill_md = std::fs::read_to_string(base_dir.join("SKILL.md")).unwrap_or_default();
        let meta_yaml = std::fs::read_to_string(base_dir.join("meta.yaml")).unwrap_or_default();

        // Step 1: 从 meta.yaml 解析参数默认值，构建 defaults map
        // 结构: parameters: { field: { type, default, description, ... } }
        let param_defaults: serde_json::Map<String, serde_json::Value> = {
            let mut defaults = serde_json::Map::new();
            if let Ok(yaml_val) = serde_yaml::from_str::<serde_yaml::Value>(&meta_yaml) {
                if let Some(params) = yaml_val.get("parameters").and_then(|v| v.as_mapping()) {
                    for (k, v) in params {
                        if let (Some(key), Some(def)) = (k.as_str(), v.get("default")) {
                            // convert serde_yaml::Value → serde_json::Value
                            if let Ok(json_val) = serde_json::to_value(def) {
                                defaults.insert(key.to_string(), json_val);
                            }
                        }
                    }
                }
            }
            defaults
        };

        // Step 2: LLM 解析用户输入为 JSON 参数
        let parse_system = "You are a parameter extraction assistant. \
            Given a skill's SKILL.md specification and meta.yaml, extract ALL parameters \
            (including optional ones with their default values) from the user request. \
            Output ONLY a valid JSON object. No explanation, no markdown, no code fences.";

        let parse_user = format!(
            "## Skill: {}\n\n## SKILL.md\n{}\n\n## meta.yaml\n{}\n\n\
            ## User Request\n{}\n\n\
            Extract ALL parameters. For parameters not mentioned by the user, use the default \
            values from meta.yaml. Output only the complete JSON parameter object:",
            req.skill_name, skill_md, meta_yaml, req.input
        );

        use blockcell_core::types::ChatMessage;
        let parse_messages = vec![
            ChatMessage::system(parse_system),
            ChatMessage::user(&parse_user),
        ];

        // 复用已有的 provider pool 做轻量解析
        let parse_pool = match blockcell_providers::ProviderPool::from_config(&state.config) {
            Ok(p) => p,
            Err(e) => {
                return Json(serde_json::json!({
                    "status": "failed",
                    "error": format!("No LLM provider: {}", e),
                }));
            }
        };

        let parsed_json = if let Some((pidx, p)) = parse_pool.acquire() {
            match p.chat(&parse_messages, &[]).await {
                Ok(resp) => {
                    parse_pool.report(pidx, blockcell_providers::CallResult::Success);
                    let text = resp.content.unwrap_or_default();
                    // 去掉可能的 markdown code fences
                    let clean = text
                        .trim()
                        .trim_start_matches("```json")
                        .trim_start_matches("```")
                        .trim_end_matches("```")
                        .trim()
                        .to_string();
                    // 解析为 JSON object，用 meta.yaml defaults 补全缺失字段
                    if let Ok(serde_json::Value::Object(mut obj)) =
                        serde_json::from_str::<serde_json::Value>(&clean)
                    {
                        for (k, v) in &param_defaults {
                            obj.entry(k).or_insert_with(|| v.clone());
                        }
                        serde_json::Value::Object(obj).to_string()
                    } else {
                        // LLM 没返回合法 JSON object：用纯默认值 + 原始输入作为 query
                        let mut fallback = param_defaults.clone();
                        fallback.insert(
                            "query".to_string(),
                            serde_json::Value::String(req.input.clone()),
                        );
                        serde_json::Value::Object(fallback).to_string()
                    }
                }
                Err(e) => {
                    parse_pool.report(
                        pidx,
                        blockcell_providers::ProviderPool::classify_error(&format!("{}", e)),
                    );
                    // fallback: defaults + 原始输入作为 query
                    let mut fallback = param_defaults.clone();
                    fallback.insert(
                        "query".to_string(),
                        serde_json::Value::String(req.input.clone()),
                    );
                    serde_json::Value::Object(fallback).to_string()
                }
            }
        } else {
            // 无可用 provider：defaults + 原始输入
            let mut fallback = param_defaults.clone();
            fallback.insert(
                "query".to_string(),
                serde_json::Value::String(req.input.clone()),
            );
            serde_json::Value::Object(fallback).to_string()
        };

        // Step 2: 用解析后的 JSON 参数执行脚本
        let script_kind = if has_py {
            SkillScriptKind::Python
        } else {
            SkillScriptKind::Rhai
        };
        let inbound = InboundMessage {
            channel: "webui_test".to_string(),
            account_id: None,
            sender_id: "webui_test".to_string(),
            chat_id: format!("test_{}", chrono::Utc::now().timestamp_millis()),
            content: parsed_json,
            media: vec![],
            metadata: serde_json::json!({
                "skill_test": true,
                "skill_name": req.skill_name,
            }),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };
        match runtime
            .execute_skill_script(&req.skill_name, &inbound, script_kind)
            .await
        {
            Ok(output) => Json(serde_json::json!({
                "status": "completed",
                "skill_name": req.skill_name,
                "result": output,
                "duration_ms": start.elapsed().as_millis() as u64,
                "dispatch": if has_py { "python" } else { "rhai" },
            })),
            Err(e) => Json(serde_json::json!({
                "status": "failed",
                "skill_name": req.skill_name,
                "error": format!("{}", e),
            })),
        }
    } else {
        // 纯 MD 技能：没有脚本文件，完全由 LLM 根据 SKILL.md 说明书执行逻辑。
        // 强制注入 SKILL.md 内容到 prompt，不依赖 match_skill trigger 匹配。
        let skill_md = match std::fs::read_to_string(base_dir.join("SKILL.md")) {
            Ok(c) => c,
            Err(_) => {
                return Json(serde_json::json!({
                    "status": "failed",
                    "error": format!("技能 '{}' 缺少 SKILL.md 文件", req.skill_name),
                }));
            }
        };

        let prompt = format!(
            "[技能说明 - {}]\n\n{}\n\n---\n\n{}",
            req.skill_name, skill_md, req.input
        );
        let inbound = InboundMessage {
            channel: "webui_test".to_string(),
            account_id: None,
            sender_id: "webui_test".to_string(),
            chat_id: format!("test_{}", chrono::Utc::now().timestamp_millis()),
            content: prompt,
            media: vec![],
            metadata: serde_json::json!({
                "skill_test": true,
                "skill_name": req.skill_name,
            }),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };
        match runtime.process_message(inbound).await {
            Ok(response) => Json(serde_json::json!({
                "status": "completed",
                "skill_name": req.skill_name,
                "result": response,
                "duration_ms": start.elapsed().as_millis() as u64,
                "dispatch": "llm_md",
            })),
            Err(e) => Json(serde_json::json!({
                "status": "failed",
                "skill_name": req.skill_name,
                "error": format!("{}", e),
            })),
        }
    }
}

/// POST /v1/evolution/test-suggest — generate a test input suggestion for a skill via LLM
#[derive(Deserialize)]
pub(super) struct EvolutionTestSuggestRequest {
    skill_name: String,
}

pub(super) async fn handle_evolution_test_suggest(
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
    let skill_py = std::fs::read_to_string(base_dir.join("SKILL.py")).ok();

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
    if let Some(py) = &skill_py {
        let py_preview: String = py.lines().take(80).collect::<Vec<_>>().join("\n");
        context.push_str(&format!("\n\n## SKILL.py (preview)\n{}", py_preview));
    }

    // 提取 triggers 列表，注入到 system prompt 要求生成的建议必须包含 trigger 关键词
    let triggers: Vec<String> = serde_yaml::from_str::<serde_json::Value>(&meta_yaml)
        .ok()
        .and_then(|v| v.get("triggers").and_then(|t| t.as_array()).cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    let trigger_rule = if triggers.is_empty() {
        String::new()
    } else {
        format!(
            "\n6. CRITICAL: The test input MUST contain one of these trigger keywords (these are the exact phrases that activate this skill): [{}]. Without a trigger keyword, the skill will NOT be activated in real chat.",
            triggers.iter().take(5).map(|s| format!("\"{}\"", s)).collect::<Vec<_>>().join(", ")
        )
    };

    let system_prompt = format!(
        "You are a test case generation assistant. Based on the provided skill description, generate a specific, ready-to-use test input.\n\
        Requirements:\n\
        1. Only output the test input text itself, no explanations, titles, or formatting\n\
        2. The test input should be natural language a user would actually say\n\
        3. Choose the most core functionality scenario of the skill\n\
        4. Input should be specific, including necessary parameters (e.g. city name, stock ticker)\n\
        5. Output in the same language as the skill's trigger keywords (Chinese if triggers are Chinese){}",
        trigger_rule
    );

    let user_prompt = format!(
        "Based on the following skill information, generate an appropriate test input:\n\n{}\n\nOutput the test input text directly:",
        context
    );

    // Call LLM directly for a lightweight suggestion
    use blockcell_core::types::ChatMessage;

    let suggestion_pool = match blockcell_providers::ProviderPool::from_config(&state.config) {
        Ok(p) => p,
        Err(e) => {
            return Json(serde_json::json!({
                "error": format!("No LLM provider configured: {}", e),
            }));
        }
    };

    let messages = vec![
        ChatMessage::system(&system_prompt),
        ChatMessage::user(&user_prompt),
    ];

    let chat_result = if let Some((pidx, p)) = suggestion_pool.acquire() {
        let r = p.chat(&messages, &[]).await;
        match &r {
            Ok(_) => suggestion_pool.report(pidx, blockcell_providers::CallResult::Success),
            Err(e) => suggestion_pool.report(
                pidx,
                blockcell_providers::ProviderPool::classify_error(&format!("{}", e)),
            ),
        }
        r
    } else {
        return Json(serde_json::json!({ "error": "No healthy provider available" }));
    };

    match chat_result {
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
pub(super) async fn handle_evolution_versions(
    State(state): State<GatewayState>,
    AxumPath(skill_name): AxumPath<String>,
) -> impl IntoResponse {
    let history_file = state
        .paths
        .skills_dir()
        .join(&skill_name)
        .join("version_history.json");
    if !history_file.exists() {
        return Json(serde_json::json!({
            "skill_name": skill_name,
            "versions": [],
            "current_version": "v1",
        }));
    }

    match std::fs::read_to_string(&history_file) {
        Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(history) => Json(history),
            Err(_) => Json(serde_json::json!({
                "skill_name": skill_name,
                "versions": [],
                "current_version": "v1",
            })),
        },
        Err(_) => Json(serde_json::json!({
            "skill_name": skill_name,
            "versions": [],
            "current_version": "v1",
        })),
    }
}

/// GET /v1/evolution/tool-versions/:id — get version history for an evolved tool
pub(super) async fn handle_evolution_tool_versions(
    State(state): State<GatewayState>,
    AxumPath(capability_id): AxumPath<String>,
) -> impl IntoResponse {
    let safe_id = capability_id.replace('.', "_");
    let history_file = state
        .paths
        .workspace()
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
        Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(history) => Json(history),
            Err(_) => Json(serde_json::json!({
                "capability_id": capability_id,
                "versions": [],
                "current_version": "v0",
            })),
        },
        Err(_) => Json(serde_json::json!({
            "capability_id": capability_id,
            "versions": [],
            "current_version": "v0",
        })),
    }
}

/// GET /v1/evolution/summary — unified evolution summary across both systems
pub(super) async fn handle_evolution_summary(
    State(state): State<GatewayState>,
) -> impl IntoResponse {
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
                            "Completed" | "Observing" | "Deployed" => skill_completed += 1,
                            "Failed" | "RolledBack" | "AuditFailed" | "CompileFailed"
                            | "DryRunFailed" | "TestFailed" => skill_failed += 1,
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
            if entry.path().is_dir() {
                user_skills += 1;
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(state.paths.builtin_skills_dir()) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                builtin_skills += 1;
            }
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
pub(super) async fn handle_stats(State(state): State<GatewayState>) -> impl IntoResponse {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);

    let (queued, running, completed, failed) = state.task_manager.summary().await;

    // Memory items count
    let memory_items: i64 = state
        .memory_store
        .as_ref()
        .and_then(|s| s.stats_json().ok())
        .and_then(|v| v.get("total_active").and_then(|n| n.as_i64()))
        .unwrap_or(0);

    // Active tasks = queued + running
    let active_tasks = queued + running;
    let (active_model, _, _) = active_model_and_provider(&state.config);

    Json(serde_json::json!({
        "uptime_secs": start.elapsed().as_secs(),
        "model": active_model,
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
