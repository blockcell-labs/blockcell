use super::*;
use blockcell_skills::read_skill_description;
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
                let presentation = state.tool_registry.presentation(name).unwrap_or_default();
                serde_json::json!({
                    "name": schema.name,
                    "description": presentation.description.as_ref().unwrap_or(&schema.description),
                    "visibility": presentation.visibility,
                    "group": presentation.group,
                    "order": presentation.order,
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

/// 判断目录是否包含 skill 标识文件
fn is_skill_dir(path: &std::path::Path) -> bool {
    path.join("SKILL.md").exists()
        || path.join("meta.yaml").exists()
        || path.join("meta.json").exists()
        || path.join("SKILL.rhai").exists()
        || path.join("SKILL.py").exists()
}

/// 递归扫描技能目录，收集 skill 信息（支持 skill 包目录和 category 目录）
fn collect_skill_entries(
    dir: &std::path::Path,
    source: &str,
    disabled_skills: &std::collections::HashSet<String>,
    out: &mut Vec<serde_json::Value>,
) {
    collect_skill_entries_recursive(dir, source, "", disabled_skills, out);
}

fn collect_skill_entries_recursive(
    dir: &std::path::Path,
    source: &str,
    category: &str,
    disabled_skills: &std::collections::HashSet<String>,
    out: &mut Vec<serde_json::Value>,
) {
    if !dir.exists() {
        return;
    }

    const EXCLUDED_DIRS: &[&str] = &[
        ".git",
        ".github",
        ".hub",
        ".snapshots",
        ".skill_file_store.lockdir",
        "__pycache__",
        "node_modules",
    ];

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let dir_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            if EXCLUDED_DIRS.contains(&dir_name.as_str()) {
                continue;
            }

            // 统一构造 sub_category，确保深层 category 路径累积完整
            let sub_category = if category.is_empty() {
                dir_name.clone()
            } else {
                format!("{}/{}", category, dir_name)
            };

            // 目录本身是 skill：优先加载自身
            if is_skill_dir(&path) {
                let composite_name = sub_category.clone();
                let meta_path = path.join("meta.yaml");
                let has_rhai = path.join("SKILL.rhai").exists();
                let has_py = path.join("SKILL.py").exists();
                let has_md = path.join("SKILL.md").exists();
                let enabled = !disabled_skills.contains(&composite_name);

                let mut skill_info = serde_json::json!({
                    "name": composite_name,
                    "category": category,
                    "source": source,
                    "has_rhai": has_rhai,
                    "has_py": has_py,
                    "has_md": has_md,
                    "enabled": enabled,
                });

                // 统一描述读取：支持 SKILL.md frontmatter / meta.yaml / meta.json
                let desc = read_skill_description(&path);
                if !desc.is_empty() {
                    skill_info["description"] = serde_json::Value::String(desc);
                }

                // meta.yaml 额外提供完整 meta 字段
                if meta_path.exists() {
                    if let Ok(content) = std::fs::read_to_string(&meta_path) {
                        if let Ok(parsed) = serde_yaml::from_str::<serde_json::Value>(&content) {
                            skill_info["meta"] = parsed;
                        }
                    }
                }

                out.push(skill_info);

                // 若同时是 skill 包（含 manifest.json），也递归扫描子目录
                // 使用 sub_category 保持路径完整，确保子 skill 可被 SkillFileStore 解析
                if path.join("manifest.json").exists() {
                    collect_skill_entries_recursive(
                        &path,
                        source,
                        &sub_category,
                        disabled_skills,
                        out,
                    );
                }
                continue;
            }

            // 非 skill 目录但含 manifest.json：作为 skill 包递归扫描子目录
            // pack 目录本身是路径的一部分，需要计入 category
            if path.join("manifest.json").exists() {
                collect_skill_entries_recursive(&path, source, &sub_category, disabled_skills, out);
                continue;
            }

            // 否则：视为 category 目录，递归扫描子目录
            // 使用 sub_category 累积路径，避免深层 category 截断
            collect_skill_entries_recursive(&path, source, &sub_category, disabled_skills, out);
        }
    }
}

/// 递归扫描技能目录，收集 skill 信息（跳过已存在的名称，支持 category 目录）
fn collect_skill_entries_filtered(
    dir: &std::path::Path,
    source: &str,
    disabled_skills: &std::collections::HashSet<String>,
    existing_names: &std::collections::HashSet<&str>,
    out: &mut Vec<serde_json::Value>,
) {
    collect_skill_entries_filtered_recursive(dir, source, "", disabled_skills, existing_names, out);
}

fn collect_skill_entries_filtered_recursive(
    dir: &std::path::Path,
    source: &str,
    category: &str,
    disabled_skills: &std::collections::HashSet<String>,
    existing_names: &std::collections::HashSet<&str>,
    out: &mut Vec<serde_json::Value>,
) {
    if !dir.exists() {
        return;
    }

    const EXCLUDED_DIRS: &[&str] = &[
        ".git",
        ".github",
        ".hub",
        ".snapshots",
        ".skill_file_store.lockdir",
        "__pycache__",
        "node_modules",
    ];

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let dir_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            if EXCLUDED_DIRS.contains(&dir_name.as_str()) {
                continue;
            }

            // 统一构造 sub_category，确保深层 category 路径累积完整
            let sub_category = if category.is_empty() {
                dir_name.clone()
            } else {
                format!("{}/{}", category, dir_name)
            };

            // 目录本身是 skill：优先加载自身
            if is_skill_dir(&path) {
                let composite_name = sub_category.clone();

                if !existing_names.contains(composite_name.as_str()) {
                    let meta_path = path.join("meta.yaml");
                    let has_rhai = path.join("SKILL.rhai").exists();
                    let has_py = path.join("SKILL.py").exists();
                    let has_md = path.join("SKILL.md").exists();
                    let enabled = !disabled_skills.contains(&composite_name);

                    let mut skill_info = serde_json::json!({
                        "name": composite_name,
                        "category": category,
                        "source": source,
                        "has_rhai": has_rhai,
                        "has_py": has_py,
                        "has_md": has_md,
                        "enabled": enabled,
                    });

                    // 统一描述读取：支持 SKILL.md frontmatter / meta.yaml / meta.json
                    let desc = read_skill_description(&path);
                    if !desc.is_empty() {
                        skill_info["description"] = serde_json::Value::String(desc);
                    }

                    // meta.yaml 额外提供完整 meta 字段
                    if meta_path.exists() {
                        if let Ok(content) = std::fs::read_to_string(&meta_path) {
                            if let Ok(parsed) = serde_yaml::from_str::<serde_json::Value>(&content)
                            {
                                skill_info["meta"] = parsed;
                            }
                        }
                    }

                    out.push(skill_info);
                }

                // 若同时是 skill 包（含 manifest.json），也递归扫描子目录
                if path.join("manifest.json").exists() {
                    collect_skill_entries_filtered_recursive(
                        &path,
                        source,
                        &sub_category,
                        disabled_skills,
                        existing_names,
                        out,
                    );
                }
                continue;
            }

            // 非 skill 目录但含 manifest.json：作为 skill 包递归扫描子目录
            // pack 目录本身是路径的一部分，需要计入 category
            if path.join("manifest.json").exists() {
                collect_skill_entries_filtered_recursive(
                    &path,
                    source,
                    &sub_category,
                    disabled_skills,
                    existing_names,
                    out,
                );
                continue;
            }

            // 否则：视为 category 目录，递归扫描子目录
            collect_skill_entries_filtered_recursive(
                &path,
                source,
                &sub_category,
                disabled_skills,
                existing_names,
                out,
            );
        }
    }
}

/// 递归扫描技能目录，收集搜索结果（支持 skill 包目录和 category 目录）
fn collect_skill_search_entries(
    dir: &std::path::Path,
    source: &str,
    query: &str,
    check_skill: &dyn Fn(&std::path::Path, &str, &str) -> Option<serde_json::Value>,
    out: &mut Vec<serde_json::Value>,
) {
    collect_skill_search_entries_recursive(dir, source, "", query, check_skill, out);
}

fn collect_skill_search_entries_recursive(
    dir: &std::path::Path,
    source: &str,
    category: &str,
    query: &str,
    check_skill: &dyn Fn(&std::path::Path, &str, &str) -> Option<serde_json::Value>,
    out: &mut Vec<serde_json::Value>,
) {
    let _ = query; // only used in recursion
    if !dir.exists() {
        return;
    }

    const EXCLUDED_DIRS: &[&str] = &[
        ".git",
        ".github",
        ".hub",
        ".snapshots",
        ".skill_file_store.lockdir",
        "__pycache__",
        "node_modules",
    ];

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let dir_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            if EXCLUDED_DIRS.contains(&dir_name.as_str()) {
                continue;
            }

            // 统一构造 sub_category，确保深层 category 路径累积完整
            let sub_category = if category.is_empty() {
                dir_name.clone()
            } else {
                format!("{}/{}", category, dir_name)
            };

            // 目录本身是 skill：优先检查自身
            if is_skill_dir(&path) {
                let composite_name = sub_category.clone();

                if let Some(result) = check_skill(&path, source, &composite_name) {
                    out.push(result);
                }
                // 若同时是 skill 包（含 manifest.json），也递归扫描子目录
                if path.join("manifest.json").exists() {
                    collect_skill_search_entries_recursive(
                        &path,
                        source,
                        &sub_category,
                        query,
                        check_skill,
                        out,
                    );
                }
                continue;
            }

            // 非 skill 目录但含 manifest.json：作为 skill 包递归扫描子目录
            // pack 目录本身是路径的一部分，需要计入 category
            if path.join("manifest.json").exists() {
                collect_skill_search_entries_recursive(
                    &path,
                    source,
                    &sub_category,
                    query,
                    check_skill,
                    out,
                );
                continue;
            }

            // 否则：视为 category 目录，递归扫描子目录
            collect_skill_search_entries_recursive(
                &path,
                source,
                &sub_category,
                query,
                check_skill,
                out,
            );
        }
    }
}

/// 递归扫描技能目录，收集搜索结果（跳过已存在的名称，支持 category 目录）
fn collect_skill_search_entries_filtered(
    dir: &std::path::Path,
    source: &str,
    query: &str,
    existing_names: &std::collections::HashSet<&str>,
    check_skill: &dyn Fn(&std::path::Path, &str, &str) -> Option<serde_json::Value>,
    out: &mut Vec<serde_json::Value>,
) {
    collect_skill_search_entries_filtered_recursive(
        dir,
        source,
        "",
        query,
        existing_names,
        check_skill,
        out,
    );
}

fn collect_skill_search_entries_filtered_recursive(
    dir: &std::path::Path,
    source: &str,
    category: &str,
    query: &str,
    existing_names: &std::collections::HashSet<&str>,
    check_skill: &dyn Fn(&std::path::Path, &str, &str) -> Option<serde_json::Value>,
    out: &mut Vec<serde_json::Value>,
) {
    let _ = query; // only used in recursion
    if !dir.exists() {
        return;
    }

    const EXCLUDED_DIRS: &[&str] = &[
        ".git",
        ".github",
        ".hub",
        ".snapshots",
        ".skill_file_store.lockdir",
        "__pycache__",
        "node_modules",
    ];

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let dir_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            if EXCLUDED_DIRS.contains(&dir_name.as_str()) {
                continue;
            }

            // 统一构造 sub_category，确保深层 category 路径累积完整
            let sub_category = if category.is_empty() {
                dir_name.clone()
            } else {
                format!("{}/{}", category, dir_name)
            };

            // 目录本身是 skill：优先检查自身
            if is_skill_dir(&path) {
                let composite_name = sub_category.clone();

                if !existing_names.contains(composite_name.as_str()) {
                    if let Some(result) = check_skill(&path, source, &composite_name) {
                        out.push(result);
                    }
                }
                // 若同时是 skill 包（含 manifest.json），也递归扫描子目录
                if path.join("manifest.json").exists() {
                    collect_skill_search_entries_filtered_recursive(
                        &path,
                        source,
                        &sub_category,
                        query,
                        existing_names,
                        check_skill,
                        out,
                    );
                }
                continue;
            }

            // 非 skill 目录但含 manifest.json：作为 skill 包递归扫描子目录
            // pack 目录本身是路径的一部分，需要计入 category
            if path.join("manifest.json").exists() {
                collect_skill_search_entries_filtered_recursive(
                    &path,
                    source,
                    &sub_category,
                    query,
                    existing_names,
                    check_skill,
                    out,
                );
                continue;
            }

            // 否则：视为 category 目录，递归扫描子目录
            collect_skill_search_entries_filtered_recursive(
                &path,
                source,
                &sub_category,
                query,
                existing_names,
                check_skill,
                out,
            );
        }
    }
}

/// GET /v1/skills — list skills
pub(super) async fn handle_skills(State(state): State<GatewayState>) -> impl IntoResponse {
    // Load disabled toggles once for all skills
    let toggles_path = state.paths.toggles_file();
    let disabled_skills: std::collections::HashSet<String> =
        tokio::fs::read_to_string(&toggles_path)
            .await
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

    // Scan user skills directory (支持 skill 包递归)
    collect_skill_entries(
        &state.paths.skills_dir(),
        "user",
        &disabled_skills,
        &mut skills,
    );

    // Scan builtin skills directory (支持 skill 包递归)
    let builtin_dir = state.paths.builtin_skills_dir();
    let user_names: std::collections::HashSet<String> = skills
        .iter()
        .filter_map(|s| {
            s.get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    let user_name_refs: std::collections::HashSet<&str> =
        user_names.iter().map(|s| s.as_str()).collect();
    collect_skill_entries_filtered(
        &builtin_dir,
        "builtin",
        &disabled_skills,
        &user_name_refs,
        &mut skills,
    );

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
    let check_skill =
        |dir: &std::path::Path, source: &str, composite_name: &str| -> Option<serde_json::Value> {
            let meta_path = dir.join("meta.yaml");
            let has_rhai = dir.join("SKILL.rhai").exists();
            let has_py = dir.join("SKILL.py").exists();
            let has_md = dir.join("SKILL.md").exists();

            let mut score = 0u32;
            let mut matched_fields = Vec::new();

            // Match against name (composite: category/name)
            if composite_name.to_lowercase().contains(&query) {
                score += 10;
                matched_fields.push("name".to_string());
            }

            // 统一描述读取：SKILL.md frontmatter / meta.yaml / meta.json
            let description = read_skill_description(dir);
            if description.to_lowercase().contains(&query) {
                score += 8;
                matched_fields.push("description".to_string());
            }

            // Match against meta.yaml text
            let mut meta_val = serde_json::Value::Null;
            if meta_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&meta_path) {
                    if content.to_lowercase().contains(&query) {
                        score += 2;
                        matched_fields.push("meta".to_string());
                    }
                    // Try parse as JSON for meta field
                    if let Ok(m) = serde_yaml::from_str::<serde_json::Value>(&content) {
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
                "name": composite_name,
                "source": source,
                "has_rhai": has_rhai,
                "has_py": has_py,
                "has_md": has_md,
                "description": description,
                "score": score,
                "matched_fields": matched_fields,
                "meta": meta_val,
            }))
        };

    // Search user skills (支持 skill 包递归)
    let skills_dir = state.paths.skills_dir();
    collect_skill_search_entries(&skills_dir, "user", &query, &check_skill, &mut results);

    // Search builtin skills (支持 skill 包递归)
    let builtin_dir = state.paths.builtin_skills_dir();
    let user_names: std::collections::HashSet<String> = results
        .iter()
        .filter_map(|r| {
            r.get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    let user_name_refs: std::collections::HashSet<&str> =
        user_names.iter().map(|s| s.as_str()).collect();
    collect_skill_search_entries_filtered(
        &builtin_dir,
        "builtin",
        &query,
        &user_name_refs,
        &check_skill,
        &mut results,
    );

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
                if let Ok(content) = tokio::fs::read_to_string(&path).await {
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
        if let Ok(content) = tokio::fs::read_to_string(&path).await {
            if let Ok(record) = serde_json::from_str::<serde_json::Value>(&content) {
                return Json(serde_json::json!({ "record": record, "kind": "skill" }));
            }
        }
    }

    // Try tool evolution records (from CoreEvolution)
    let cap_records_dir = state.paths.workspace().join("tool_evolution_records");
    let cap_path = cap_records_dir.join(format!("{}.json", evolution_id));
    if cap_path.exists() {
        if let Ok(content) = tokio::fs::read_to_string(&cap_path).await {
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
                if let Ok(content) = tokio::fs::read_to_string(&path).await {
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
                    "toolCallMode": e.tool_call_mode,
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
            "toolCallMode": "native",
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
const PERSONA_FILES: &[&str] = &["AGENTS.md", "SOUL.md", "USER.md"];

/// GET /v1/persona/files — list persona files with their content
pub(super) async fn handle_persona_list(State(state): State<GatewayState>) -> impl IntoResponse {
    let workspace = state.paths.workspace();
    let mut files = Vec::new();

    for name in PERSONA_FILES {
        let path = workspace.join(name);
        let exists = tokio::fs::try_exists(&path).await.unwrap_or(false);
        let content = if exists {
            tokio::fs::read_to_string(&path).await.unwrap_or_default()
        } else {
            String::new()
        };
        files.push(serde_json::json!({
            "name": name,
            "exists": exists,
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
    let exists = tokio::fs::try_exists(&path).await.unwrap_or(false);
    let content = if exists {
        tokio::fs::read_to_string(&path).await.unwrap_or_default()
    } else {
        String::new()
    };
    Json(serde_json::json!({
        "name": params.name,
        "content": content,
        "exists": exists,
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
    match tokio::fs::write(&path, &req.content).await {
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
            // 使用 tokio::fs 异步 I/O 避免阻塞工作线程
            let toggles_path = state.paths.toggles_file();
            if let Ok(content) = tokio::fs::read_to_string(&toggles_path).await {
                if let Ok(mut store) = serde_json::from_str::<serde_json::Value>(&content) {
                    let skill_enabled = store
                        .get("skills")
                        .and_then(|s| s.get(&req.skill_name))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    if skill_enabled {
                        if store.get("skills").is_none() {
                            store["skills"] = serde_json::json!({});
                        }
                        store["skills"][&req.skill_name] = serde_json::json!(false);
                        let _ = tokio::fs::write(
                            &toggles_path,
                            serde_json::to_string_pretty(&store).unwrap_or_default(),
                        )
                        .await;
                    }
                }
            } else {
                let store =
                    serde_json::json!({ "skills": { &req.skill_name: false }, "tools": {} });
                if let Some(parent) = toggles_path.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                let _ = tokio::fs::write(
                    &toggles_path,
                    serde_json::to_string_pretty(&store).unwrap_or_default(),
                )
                .await;
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

/// POST /v1/evolution/:id/resume — resume a stopped evolution
pub(super) async fn handle_evolution_resume(
    State(state): State<GatewayState>,
    AxumPath(evolution_id): AxumPath<String>,
) -> impl IntoResponse {
    let records_dir = state.paths.workspace().join("evolution_records");
    let path = records_dir.join(format!("{}.json", evolution_id));

    if path.exists() {
        let record_data = match tokio::fs::read_to_string(&path).await {
            Ok(data) => data,
            Err(e) => {
                return Json(
                    serde_json::json!({ "error": format!("Failed to read record: {}", e) }),
                )
            }
        };

        let mut record: serde_json::Value = match serde_json::from_str(&record_data) {
            Ok(r) => r,
            Err(e) => {
                return Json(
                    serde_json::json!({ "error": format!("Failed to parse record: {}", e) }),
                )
            }
        };

        // Check if evolution is stopped
        let status = record.get("status").and_then(|s| s.as_str()).unwrap_or("");
        if status != "Stopped" {
            return Json(serde_json::json!({
                "error": format!("Evolution is not stopped (status: {})", status)
            }));
        }

        // Get the previous status from context if available, otherwise default to Triggered
        let previous_status = record
            .get("context")
            .and_then(|c| c.get("stopped_from_status"))
            .and_then(|s| s.as_str())
            .unwrap_or("Triggered");

        // Restore to previous status
        record["status"] = serde_json::Value::String(previous_status.to_string());
        record["updated_at"] = serde_json::Value::Number(serde_json::Number::from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        ));

        // Remove stop reason from context
        if let Some(context) = record.get_mut("context").and_then(|c| c.as_object_mut()) {
            context.remove("stop_reason");
            context.remove("stopped_from_status");
        }

        // Save updated record
        if let Err(e) =
            tokio::fs::write(&path, serde_json::to_string_pretty(&record).unwrap()).await
        {
            return Json(serde_json::json!({ "error": format!("Failed to save record: {}", e) }));
        }

        // Broadcast WS event
        let _ = state.ws_broadcast.send(
            serde_json::json!({
                "type": "evolution_resumed",
                "id": evolution_id,
            })
            .to_string(),
        );

        return Json(serde_json::json!({
            "status": "resumed",
            "id": evolution_id,
            "message": "Evolution resumed successfully"
        }));
    }

    Json(serde_json::json!({ "error": "Evolution record not found" }))
}

/// POST /v1/evolution/:id/stop — stop an in-progress evolution
pub(super) async fn handle_evolution_stop(
    State(state): State<GatewayState>,
    AxumPath(evolution_id): AxumPath<String>,
) -> impl IntoResponse {
    // Try skill evolution records
    let records_dir = state.paths.workspace().join("evolution_records");
    let path = records_dir.join(format!("{}.json", evolution_id));

    if path.exists() {
        // Read the record to check status and get skill_name
        let record_data = match tokio::fs::read_to_string(&path).await {
            Ok(data) => data,
            Err(e) => {
                return Json(
                    serde_json::json!({ "error": format!("Failed to read record: {}", e) }),
                )
            }
        };

        let mut record: serde_json::Value = match serde_json::from_str(&record_data) {
            Ok(r) => r,
            Err(e) => {
                return Json(
                    serde_json::json!({ "error": format!("Failed to parse record: {}", e) }),
                )
            }
        };

        // Check if evolution is in progress
        let status = record.get("status").and_then(|s| s.as_str()).unwrap_or("");
        let in_progress_states = [
            "Triggered",
            "Generating",
            "Generated",
            "Auditing",
            "AuditPassed",
            "CompilePassed",
            "RollingOut",
        ];

        if !in_progress_states.contains(&status) {
            return Json(serde_json::json!({
                "error": format!("Evolution is not in progress (status: {})", status)
            }));
        }

        // Save current status before stopping so we can resume from it
        let current_status = status.to_string();

        // Update status to Stopped (not Failed, so it can be resumed)
        record["status"] = serde_json::Value::String("Stopped".to_string());
        record["updated_at"] = serde_json::Value::Number(serde_json::Number::from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        ));

        // Add stop reason and save previous status to context
        if let Some(context) = record.get_mut("context").and_then(|c| c.as_object_mut()) {
            context.insert(
                "stop_reason".to_string(),
                serde_json::Value::String("Manually stopped by user".to_string()),
            );
            context.insert(
                "stopped_from_status".to_string(),
                serde_json::Value::String(current_status),
            );
        }

        // Save updated record
        if let Err(e) =
            tokio::fs::write(&path, serde_json::to_string_pretty(&record).unwrap()).await
        {
            return Json(serde_json::json!({ "error": format!("Failed to save record: {}", e) }));
        }

        // Clean up in-memory EvolutionService state
        if let Some(skill_name) = record.get("skill_name").and_then(|s| s.as_str()) {
            let evo_guard = state.evolution_service.lock().await;
            let _ = evo_guard.delete_records_by_skill(skill_name).await;
        }

        // Broadcast WS event
        let _ = state.ws_broadcast.send(
            serde_json::json!({
                "type": "evolution_stopped",
                "id": evolution_id,
            })
            .to_string(),
        );

        return Json(serde_json::json!({
            "status": "stopped",
            "id": evolution_id,
            "message": "Evolution stopped successfully"
        }));
    }

    Json(serde_json::json!({ "error": "Evolution record not found" }))
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
        let skill_name = tokio::fs::read_to_string(&path)
            .await
            .ok()
            .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
            .and_then(|v| {
                v.get("skill_name")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string())
            });

        return match tokio::fs::remove_file(&path).await {
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
        return match tokio::fs::remove_file(&cap_path).await {
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

    if !skill_dir.exists() && !builtin_dir.exists() {
        return Json(serde_json::json!({
            "status": "failed",
            "error": format!("技能 '{}' 未找到", req.skill_name),
        }));
    }

    let test_pool = match blockcell_providers::ProviderPool::from_config(&state.config) {
        Ok(p) => p,
        Err(e) => {
            return Json(serde_json::json!({
                "status": "failed",
                "error": format!("No LLM provider configured: {}", e),
            }));
        }
    };

    let tool_registry = state.tool_registry.as_ref().clone();
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

    if let Err(e) = runtime.init_memory_file_store() {
        warn!(error = %e, "Failed to initialize file memory store");
    }

    let start = std::time::Instant::now();

    let inbound = InboundMessage {
        channel: "webui_test".to_string(),
        account_id: None,
        sender_id: "webui_test".to_string(),
        chat_id: format!("test_{}", chrono::Utc::now().timestamp_millis()),
        content: req.input.clone(),
        media: vec![],
        metadata: serde_json::json!({
            "skill_test": true,
            "skill_name": req.skill_name,
            "forced_skill_name": req.skill_name,
            "skill_run_mode": "test",
        }),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    match runtime.process_message(inbound).await {
        Ok(response) => Json(serde_json::json!({
            "status": "completed",
            "skill_name": req.skill_name,
            "result": response,
            "duration_ms": start.elapsed().as_millis() as u64,
            "dispatch": "skill_kernel",
        })),
        Err(e) => Json(serde_json::json!({
            "status": "failed",
            "skill_name": req.skill_name,
            "error": format!("{}", e),
        })),
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
    let mut skill_manager = blockcell_skills::SkillManager::new();
    if let Err(err) = skill_manager.load_from_paths(&state.paths) {
        return Json(serde_json::json!({
            "error": format!("Failed to load skills: {}", err),
        }));
    }

    let Some(skill) = skill_manager.get(&req.skill_name) else {
        return Json(serde_json::json!({
            "error": format!("Skill '{}' not found", req.skill_name),
        }));
    };

    let meta_yaml = serde_yaml::to_string(&skill.meta).unwrap_or_default();
    let skill_card = blockcell_skills::SkillManager::build_skill_card(skill);
    let prompt_bundle = skill
        .load_prompt_bundle()
        .or_else(|| skill.load_md())
        .unwrap_or_default();

    let mut context = format!(
        "Skill name: {}\nDescription: {}\nLayout: {}\nWhen to use: {}\nOutputs: {}\nAllowed tools: {}\nSupports local execution: {}\nLocal entrypoints: {}\n\n## meta.yaml\n{}\n\n## Prompt bundle\n{}",
        skill_card.name,
        skill_card.description,
        skill_card.execution_layout,
        skill_card.when_to_use,
        skill_card.outputs,
        if skill_card.allowed_tools.is_empty() {
            "(none)".to_string()
        } else {
            skill_card.allowed_tools.join(", ")
        },
        skill_card.supports_local_exec,
        if skill_card.local_exec_entrypoints.is_empty() {
            "(none)".to_string()
        } else {
            skill_card.local_exec_entrypoints.join(", ")
        },
        meta_yaml,
        prompt_bundle
    );
    if skill_card.supports_local_exec {
        context.push_str(
            "\n\n## Notes\nThis skill may execute local scripts through `exec_local`, but test input generation should still be based on the manual and the user-visible skill contract. The listed local entrypoints are the only relative paths that should be considered for local execution.",
        );
    }

    let system_prompt =
        "You are a test case generation assistant. Based on the provided skill description, generate a specific, ready-to-use test input.\n\
        Requirements:\n\
        1. Only output the test input text itself, no explanations, titles, or formatting\n\
        2. The test input should be natural language a user would actually say\n\
        3. Choose the most core functionality scenario of the skill\n\
        4. Input should be specific, including necessary parameters (e.g. city name, stock ticker)\n\
        5. Output in the same language as the skill description and SKILL.md manual"
            .to_string();

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

    match tokio::fs::read_to_string(&history_file).await {
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

    match tokio::fs::read_to_string(&history_file).await {
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
                if let Ok(content) = tokio::fs::read_to_string(&path).await {
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
                if let Ok(content) = tokio::fs::read_to_string(&path).await {
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
    let ghost_metrics = blockcell_agent::ghost_metrics_summary(&state.paths);

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
        "ghost": ghost_metrics,
    }))
}
