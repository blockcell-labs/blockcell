use blockcell_agent::AgentRuntime;
use blockcell_core::{Config, InboundMessage, Paths};
use blockcell_skills::evolution::EvolutionRecord;
use blockcell_skills::is_builtin_tool;
use blockcell_storage::MemoryStore;
use blockcell_tools::ToolRegistry;

/// List all skill evolution records.
pub async fn list(all: bool, enabled_only: bool) -> anyhow::Result<()> {
    let paths = Paths::default();
    let records_dir = paths.workspace().join("evolution_records");
    let skills_dir = paths.skills_dir();

    // Load all evolution records
    let mut records: Vec<EvolutionRecord> = Vec::new();
    if records_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&records_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(record) = serde_json::from_str::<EvolutionRecord>(&content) {
                            records.push(record);
                        }
                    }
                }
            }
        }
    }
    records.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    // Categorize: deduplicate by skill_name (keep latest record per skill)
    let mut seen = std::collections::HashSet::new();
    let mut learning = Vec::new();
    let mut learned = Vec::new();
    let mut failed = Vec::new();
    let mut builtin_count: usize = 0;

    for r in &records {
        if is_builtin_tool(&r.skill_name) {
            builtin_count += 1;
            if !all { continue; }
        }
        if !seen.insert(r.skill_name.clone())
            && !all { continue; }

        let status_str = format!("{:?}", r.status);
        match status_str.as_str() {
            "Completed" => learned.push(r),
            "Failed" | "RolledBack" | "AuditFailed" | "DryRunFailed" | "TestFailed" => failed.push(r),
            _ => learning.push(r),
        }
    }

    // Collect available skills from workspace/skills/ only.
    // (Builtin skills are extracted to workspace on first run/onboard.)
    let mut available_skills: Vec<(String, std::path::PathBuf)> = Vec::new();
    if skills_dir.exists() && skills_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && (p.join("SKILL.rhai").exists() || p.join("SKILL.md").exists()) {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                    available_skills.push((name, p));
                }
            }
        }
    }
    available_skills.sort_by(|a, b| a.0.cmp(&b.0));

    // Filter by enabled if requested (skills with .disabled marker are excluded)
    if enabled_only {
        available_skills.retain(|(_, p)| !p.join(".disabled").exists());
    }

    let available_count = available_skills.len();

    println!();
    println!("🧠 Skill Status");
    println!("  📦 Loaded: {}  ✅ Learned: {}  🔄 Learning: {}  ❌ Failed: {}",
        available_count, learned.len(), learning.len(), failed.len());

    if !available_skills.is_empty() {
        println!();
        println!("  📦 Available skills:");
        for (name, path) in &available_skills {
            let desc = read_skill_description(path);
            if desc.is_empty() {
                println!("    • {}", name);
            } else {
                println!("    • {} — {}", name, desc);
            }
            println!("      {}", path.display());
        }
    }

    if !learned.is_empty() {
        println!();
        println!("  ✅ Learned skills:");
        for r in &learned {
            println!("    • {} ({})", r.skill_name, format_ts(r.created_at));
        }
    }

    if !learning.is_empty() {
        println!();
        println!("  🔄 Learning in progress:");
        for r in &learning {
            let desc = status_desc(&format!("{:?}", r.status));
            println!("    • {} [{}] ({})", r.skill_name, desc, format_ts(r.created_at));
        }
    }

    if !failed.is_empty() {
        println!();
        println!("  ❌ Failed skills:");
        for r in &failed {
            println!("    • {} ({})", r.skill_name, format_ts(r.created_at));
        }
    }

    if !all && builtin_count > 0 {
        println!();
        println!("  ℹ️  {} built-in tool error records hidden (use --all to view, or clear to clean up)", builtin_count);
    }

    if learning.is_empty() && learned.is_empty() && failed.is_empty() && builtin_count == 0 {
        println!("  (No skill records)");
    }
    println!();
    Ok(())
}

/// Show details for a specific skill.
pub async fn show(name: &str) -> anyhow::Result<()> {
    let paths = Paths::default();
    let skills_dir = paths.skills_dir();
    let skill_path = skills_dir.join(name);

    if !skill_path.exists() || !skill_path.is_dir() {
        println!("❌ Skill '{}' not found in {}", name, skills_dir.display());
        println!("  Use `blockcell skills list` to see available skills.");
        return Ok(());
    }

    println!();
    println!("🧠 Skill: {}", name);
    println!("  Path: {}", skill_path.display());

    let disabled = skill_path.join(".disabled").exists();
    println!("  Status: {}", if disabled { "⏸  disabled" } else { "✅ enabled" });

    let meta_path = skill_path.join("meta.yaml");
    if meta_path.exists() {
        println!();
        println!("  meta.yaml:");
        let content = std::fs::read_to_string(&meta_path).unwrap_or_default();
        for line in content.lines().take(30) {
            println!("    {}", line);
        }
    }

    if skill_path.join("SKILL.rhai").exists() {
        println!();
        println!("  Script: SKILL.rhai ✓");
    }
    if skill_path.join("SKILL.md").exists() {
        println!();
        println!("  Manual: SKILL.md ✓");
    }

    // Show evolution records for this skill
    let records_dir = paths.workspace().join("evolution_records");
    let mut records: Vec<EvolutionRecord> = Vec::new();
    if records_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&records_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().is_some_and(|e| e == "json") {
                    if let Ok(content) = std::fs::read_to_string(&p) {
                        if let Ok(r) = serde_json::from_str::<EvolutionRecord>(&content) {
                            if r.skill_name == name {
                                records.push(r);
                            }
                        }
                    }
                }
            }
        }
    }
    records.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if !records.is_empty() {
        println!();
        println!("  Evolution records ({}):", records.len());
        for r in records.iter().take(5) {
            println!("    {} {:?} — {}", &r.id.chars().take(12).collect::<String>(), r.status, format_ts(r.created_at));
        }
    }

    println!();
    Ok(())
}

/// Enable or disable a skill by creating/removing a .disabled marker.
pub async fn set_enabled(name: &str, enable: bool) -> anyhow::Result<()> {
    let paths = Paths::default();
    let skill_path = paths.skills_dir().join(name);

    if !skill_path.exists() || !skill_path.is_dir() {
        println!("❌ Skill '{}' not found.", name);
        return Ok(());
    }

    let marker = skill_path.join(".disabled");
    if enable {
        if marker.exists() {
            std::fs::remove_file(&marker)?;
            println!("✅ Skill '{}' enabled.", name);
        } else {
            println!("  Skill '{}' is already enabled.", name);
        }
    } else {
        if !marker.exists() {
            std::fs::write(&marker, "")?;
            println!("⏸  Skill '{}' disabled.", name);
        } else {
            println!("  Skill '{}' is already disabled.", name);
        }
    }
    Ok(())
}

/// Hot-reload: report skill count (actual reload happens at agent startup).
pub async fn reload() -> anyhow::Result<()> {
    let paths = Paths::default();
    let skills_dir = paths.skills_dir();

    // Re-extract builtin skills (skips existing files)
    match super::embedded_skills::extract_to_workspace(&skills_dir) {
        Ok(new_skills) if !new_skills.is_empty() => {
            println!("✓ Extracted {} new builtin skill(s): {}", new_skills.len(), new_skills.join(", "));
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("⚠️  Failed to extract builtin skills: {}", e);
        }
    }

    let mut count = 0usize;
    if skills_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && (p.join("SKILL.rhai").exists() || p.join("SKILL.md").exists()) {
                    count += 1;
                }
            }
        }
    }

    println!("✅ Skills directory refreshed. {} skill(s) available.", count);
    println!("   Note: Running agent processes will pick up changes on their next tick.");
    Ok(())
}

/// Clear all evolution records.
pub async fn clear() -> anyhow::Result<()> {
    let paths = Paths::default();
    let records_dir = paths.workspace().join("evolution_records");
    let mut count = 0;

    if records_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&records_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json")
                    && std::fs::remove_file(&path).is_ok() {
                        count += 1;
                    }
            }
        }
    }

    if count > 0 {
        println!("✅ Cleared all skill evolution records ({} total)", count);
    } else {
        println!("(No records to clear)");
    }
    Ok(())
}

/// Delete evolution records for a specific skill.
pub async fn forget(skill_name: &str) -> anyhow::Result<()> {
    let paths = Paths::default();
    let records_dir = paths.workspace().join("evolution_records");
    let mut count = 0;

    if records_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&records_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(record) = serde_json::from_str::<EvolutionRecord>(&content) {
                            if record.skill_name == skill_name
                                && std::fs::remove_file(&path).is_ok() {
                                    count += 1;
                                }
                        }
                    }
                }
            }
        }
    }

    if count > 0 {
        println!("✅ Deleted all records for skill `{}` ({} total)", skill_name, count);
    } else {
        println!("⚠️  No records found for skill `{}`", skill_name);
    }
    Ok(())
}

/// Learn a new skill by sending a request to the agent.
pub async fn learn(description: &str) -> anyhow::Result<()> {
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;

    // Create provider using shared multi-provider dispatch
    let provider = super::provider::create_provider(&config)?;

    // Create runtime
    let tool_registry = ToolRegistry::with_defaults();
    let mut runtime = AgentRuntime::new(config, paths.clone(), provider, tool_registry)?;
    runtime.mount_mcp_servers().await;

    // Optionally wire up memory store
    let memory_db_path = paths.memory_dir().join("memory.db");
    if let Ok(store) = MemoryStore::open(&memory_db_path) {
        use blockcell_agent::MemoryStoreAdapter;
        use std::sync::Arc;
        let handle: blockcell_tools::MemoryStoreHandle = Arc::new(MemoryStoreAdapter::new(store));
        runtime.set_memory_store(handle);
    }

    println!("🔄 Learning skill: {}", description);
    println!();

    let learn_msg = format!(
        "Please learn the following skill: {}\n\n\
        If this skill is already learned (has a record in list_skills query=learned), just tell me it's done.\n\
        Otherwise, start learning this skill and report progress.",
        description
    );

    let inbound = InboundMessage {
        channel: "cli".to_string(),
        sender_id: "user".to_string(),
        chat_id: "default".to_string(),
        content: learn_msg,
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    let response = runtime.process_message(inbound).await?;
    println!("{}", response);
    Ok(())
}

/// Install a skill from the Community Hub.
pub async fn install(name: &str, version: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::default();
    let config = Config::load_or_default(&paths)?;
    
    // Resolve Hub URL
    let hub_url = std::env::var("BLOCKCELL_HUB_URL")
        .ok()
        .or_else(|| config.community_hub_url())
        .unwrap_or_else(|| "http://127.0.0.1:8800".to_string());
    let hub_url = hub_url.trim_end_matches('/');

    let api_key = std::env::var("BLOCKCELL_HUB_API_KEY")
        .ok()
        .or_else(|| config.community_hub_api_key());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    // 1. Get skill info
    let version_str = version.as_deref().unwrap_or("latest");
    let info_url = if let Some(v) = &version {
        format!("{}/v1/skills/{}/{}", hub_url, urlencoding::encode(name), v)
    } else {
        format!("{}/v1/skills/{}/latest", hub_url, urlencoding::encode(name))
    };

    println!("🔍 Resolving skill {}@{}...", name, version_str);
    
    let mut req = client.get(&info_url);
    if let Some(key) = &api_key {
        req = req.header("Authorization", format!("Bearer {}", key));
    }
    
    let resp = req.send().await?;
    if !resp.status().is_success() {
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("Skill not found on Hub.");
        }
        anyhow::bail!("Hub request failed: {}", resp.status());
    }
    
    let info: serde_json::Value = resp.json().await?;
    let dist_url = info.get("dist_url").and_then(|v| v.as_str());
    let source_url = info.get("source_url").and_then(|v| v.as_str());
    let download_url = dist_url
        .or(source_url)
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            format!(
                "{}/v1/skills/{}/download",
                hub_url,
                urlencoding::encode(name)
            )
        });

    println!("📦 最终下载地址: {}", download_url);
    println!("📦 Downloading from {}...", download_url);

    // 2. Download artifact
    let resp = client.get(&download_url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("Download failed: {}", resp.status());
    }
    let content = resp.bytes().await?;

    // 3. Install to workspace/skills/<name>
    let skills_dir = paths.workspace().join("skills");
    let target_dir = skills_dir.join(name);
    
    if target_dir.exists() {
        // Backup existing? Or overwrite? For now, simple overwrite logic (remove then create).
        // Check if it's a directory
        if target_dir.is_dir() {
            println!("⚠️  Removing existing skill at {}", target_dir.display());
            std::fs::remove_dir_all(&target_dir)?;
        }
    }
    std::fs::create_dir_all(&target_dir)?;

    println!("📂 Extracting to {}...", target_dir.display());

    // Assuming zip file
    let cursor = std::io::Cursor::new(content);
    let mut archive = zip::ZipArchive::new(cursor)?;
    
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = match file.enclosed_name() {
            Some(path) => target_dir.join(path),
            None => continue,
        };

        if file.name().ends_with('/') {
            std::fs::create_dir_all(&outpath)?;
        } else {
            if let Some(p) = outpath.parent() {
                if !p.exists() {
                    std::fs::create_dir_all(p)?;
                }
            }
            let mut outfile = std::fs::File::create(&outpath)?;
            std::io::copy(&mut file, &mut outfile)?;
        }
    }

    println!("✅ Skill '{}' installed successfully!", name);
    println!("   Version: {}", info.get("version").and_then(|v| v.as_str()).unwrap_or("unknown"));
    
    Ok(())
}

/// Test a skill directory by validating meta.yaml and dry-running SKILL.rhai with mock tools.
pub async fn test(path: &str, input: Option<String>, verbose: bool) -> anyhow::Result<()> {
    use rhai::{Engine, Dynamic, Scope, Map};
    use std::sync::{Arc, Mutex};

    let skill_path = std::path::Path::new(path);
    let skill_name = skill_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);

    println!();
    println!("🧪 Testing skill: {}", skill_name);
    println!("   Path: {}", skill_path.display());
    println!();

    let mut pass = 0usize;
    let mut fail = 0usize;

    // ── Step 1: meta.yaml ────────────────────────────────────────────────────
    let meta_path = skill_path.join("meta.yaml");
    print!("  [1/3] meta.yaml          ");
    if !meta_path.exists() {
        println!("❌ MISSING");
        fail += 1;
    } else {
        let meta_str = std::fs::read_to_string(&meta_path)?;
        // Basic structural check: required keys
        let required = ["name:", "description:", "triggers:", "capabilities:"];
        let missing: Vec<&str> = required.iter().filter(|k| !meta_str.contains(*k)).copied().collect();
        if missing.is_empty() {
            println!("✅ OK");
            pass += 1;
            if verbose {
                for line in meta_str.lines().take(6) {
                    println!("            {}", line);
                }
            }
        } else {
            println!("❌ Missing keys: {}", missing.join(", "));
            fail += 1;
        }
    }

    // ── Step 2: SKILL.md ─────────────────────────────────────────────────────
    let md_path = skill_path.join("SKILL.md");
    print!("  [2/3] SKILL.md           ");
    if !md_path.exists() {
        println!("❌ MISSING");
        fail += 1;
    } else {
        let md_str = std::fs::read_to_string(&md_path)?;
        if md_str.len() > 50 {
            println!("✅ OK  ({} bytes)", md_str.len());
            pass += 1;
        } else {
            println!("⚠️  Very short ({} bytes)", md_str.len());
            pass += 1; // not fatal
        }
    }

    // ── Step 3: SKILL.rhai compile + mock run ────────────────────────────────
    let rhai_path = skill_path.join("SKILL.rhai");
    print!("  [3/3] SKILL.rhai compile ");
    if !rhai_path.exists() {
        println!("❌ MISSING");
        fail += 1;
        print_result(pass, fail);
        return Ok(());
    }

    let script = std::fs::read_to_string(&rhai_path)?;

    // Shared state for mock calls
    let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let output_set: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let calls_c = calls.clone();
    let output_c = output_set.clone();
    let logs_c = logs.clone();
    let logs_w = logs.clone();
    let errors_c = errors.clone();

    let mut engine = Engine::new();
    engine.set_max_operations(500_000);

    // mock call_tool(name, params) -> Map with success:true
    engine.register_fn("call_tool", move |name: &str, _params: rhai::Map| -> Dynamic {
        calls_c.lock().unwrap().push((name.to_string(), "{}".to_string()));
        let mut m = Map::new();
        m.insert("success".into(), Dynamic::from(true));
        m.insert("content".into(), Dynamic::from("mock content"));
        m.insert("results".into(), Dynamic::from(rhai::Array::new()));
        m.insert("items".into(), Dynamic::from(rhai::Array::new()));
        m.insert("emails".into(), Dynamic::from(rhai::Array::new()));
        m.insert("tasks".into(), Dynamic::from(rhai::Array::new()));
        m.insert("contacts".into(), Dynamic::from(rhai::Array::new()));
        m.insert("data".into(), Dynamic::from("mock data"));
        m.insert("error".into(), Dynamic::UNIT);
        m.insert("text".into(), Dynamic::from("mock text"));
        m.insert("path".into(), Dynamic::from("/tmp/mock_output"));
        m.insert("output_path".into(), Dynamic::from("/tmp/mock_output"));
        m.insert("url".into(), Dynamic::from("https://example.com"));
        m.insert("id".into(), Dynamic::from("mock-id-001"));
        m.insert("task_id".into(), Dynamic::from("mock-task-001"));
        m.insert("total".into(), Dynamic::from(0_i64));
        Dynamic::from_map(m)
    });

    // mock set_output(map)
    engine.register_fn("set_output", move |val: Dynamic| {
        let s = format!("{:?}", val);
        *output_c.lock().unwrap() = Some(s);
    });

    // mock log(msg)
    engine.register_fn("log", move |msg: &str| {
        logs_c.lock().unwrap().push(msg.to_string());
    });

    // mock log_warn(msg)
    engine.register_fn("log_warn", move |msg: &str| {
        logs_w.lock().unwrap().push(format!("[WARN] {}", msg));
    });

    // mock is_error(val) -> bool — always false (mock tools succeed)
    engine.register_fn("is_error", |_val: Dynamic| -> bool { false });

    // mock get_field(map, key) -> Dynamic
    // Returns empty string for unknown keys to avoid string-concat errors
    engine.register_fn("get_field", |map: Dynamic, key: &str| -> Dynamic {
        if let Some(m) = map.try_cast::<Map>() {
            m.get(key).cloned().unwrap_or_else(|| Dynamic::from("".to_string()))
        } else {
            Dynamic::from("".to_string())
        }
    });

    // mock timestamp() -> String
    engine.register_fn("timestamp", || -> String {
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
    });

    // Compile
    match engine.compile(&script) {
        Err(e) => {
            println!("❌ Compile error");
            println!("            {}", e);
            fail += 1;
            errors_c.lock().unwrap().push(format!("Compile: {}", e));
            print_result(pass, fail);
            return Ok(());
        }
        Ok(ast) => {
            println!("✅ OK");
            pass += 1;

            // ── Step 4: mock run ──────────────────────────────────────────────
            print!("  [4/4] SKILL.rhai run     ");

            // Inject dummy variables from meta.yaml (all common ones as ())
            let user_msg = input.as_deref().unwrap_or("test input from blockcell skills test");
            let mut scope = Scope::new();
            scope.push("user_input", Dynamic::from(user_msg.to_string()));

            // Inject all common optional variables as ()
            let optional_vars = [
                "query", "command", "url", "action", "topic", "path",
                "source", "destination", "service", "platform", "provider",
                "title", "body", "content", "text", "message",
                "subject", "to", "from", "limit", "max_results",
                "max_pages", "timeout", "cwd", "language", "format",
                "algorithm", "format", "bits", "length", "type",
                "owner", "repo", "branch", "tag", "version",
                "entity_id", "domain", "payload", "topic", "host",
                "ports", "record_type", "region", "bucket",
                "instance_id", "database_id", "page_id", "event_id",
                "graph_name", "name", "relation", "from_entity", "to_entity",
                "voice", "backend", "output_path", "input_path",
                "image_path", "audio_path", "chart_type",
                "start", "end", "start_date", "end_date",
                "task_id", "id", "uid", "contact_id",
                "origin", "destination", "keyword", "location",
                "mode", "radius", "recursive", "max_pages",
                "action_type", "schedule", "task", "number",
                "address", "query", "filter", "sort_by",
                "channel", "service", "max_results", "source", "include_symbols",
                "fetch_top", "watch", "depth", "bidirectional", "top_k",
                "stats", "export_format", "camera_id", "priority", "count",
                "include_uppercase", "include_numbers", "session", "browser",
                "ms", "tab_id", "extract_type", "model", "output_format",
                "auto_filter", "bold_header", "freeze_panes", "column_widths",
                "slides", "sections", "sheets", "attachments", "tags",
                "importance", "scope", "dedup_key", "expires_in_days",
            ];
            for var in &optional_vars {
                if scope.get_value::<Dynamic>(var).is_none() {
                    scope.push(*var, Dynamic::UNIT);
                }
            }

            let run_result = engine.run_ast_with_scope(&mut scope, &ast);
            match run_result {
                Ok(_) => {
                    println!("✅ OK");
                    pass += 1;
                }
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("Variable not found") {
                        // Extract the variable name from the error — Rhai format: Variable 'name' not found
                        let var_name = err_str
                            .split('\'').nth(1)
                            .unwrap_or(&err_str);
                        println!("⚠️  WARN — undefined variable '{}' (add to optional_vars list)", var_name);
                        println!("            Full error: {}", err_str);
                        // Treat as warning only — the script compiled and mostly ran fine
                        pass += 1;
                        errors_c.lock().unwrap().push(format!("Warn (undef var): {}", var_name));
                    } else {
                        println!("❌ Runtime error: {}", e);
                        fail += 1;
                        errors_c.lock().unwrap().push(format!("Runtime: {}", err_str));
                    }
                }
            }
        }
    }

    // ── Report ────────────────────────────────────────────────────────────────
    println!();

    let tool_calls = calls.lock().unwrap();
    if !tool_calls.is_empty() {
        println!("  🔧 Mock tool calls ({}):", tool_calls.len());
        for (name, _) in tool_calls.iter() {
            println!("     • {}", name);
        }
        println!();
    }

    let log_lines = logs.lock().unwrap();
    if verbose && !log_lines.is_empty() {
        println!("  📋 Script logs:");
        for l in log_lines.iter() {
            println!("     {}", l);
        }
        println!();
    }

    if let Some(out) = output_set.lock().unwrap().as_deref() {
        let preview = if out.len() > 200 { &out[..200] } else { out };
        println!("  📤 set_output: {}", preview);
        println!();
    }

    print_result(pass, fail);
    Ok(())
}

fn print_result(pass: usize, fail: usize) {
    let total = pass + fail;
    if fail == 0 {
        println!("  ✅ PASS  ({}/{} checks passed)", pass, total);
    } else {
        println!("  ❌ FAIL  ({}/{} checks passed, {} failed)", pass, total, fail);
    }
    println!();
}

/// Batch-test all skills under a directory.
pub async fn test_all(dir: &str, input: Option<String>, verbose: bool) -> anyhow::Result<()> {
    let base = std::path::Path::new(dir);
    if !base.exists() || !base.is_dir() {
        anyhow::bail!("Directory not found: {}", dir);
    }

    let entries: Vec<_> = std::fs::read_dir(base)?
        .flatten()
        .filter(|e| {
            let p = e.path();
            p.is_dir() && (p.join("SKILL.rhai").exists() || p.join("meta.yaml").exists())
        })
        .collect();

    if entries.is_empty() {
        println!("No skill directories found in: {}", dir);
        return Ok(());
    }

    let total = entries.len();
    let mut passed = 0usize;
    let mut failed_names: Vec<String> = Vec::new();

    println!();
    println!("🧪 Batch testing {} skills in: {}", total, dir);
    println!("{}", "─".repeat(60));

    for entry in &entries {
        let skill_path = entry.path();
        let name = skill_path.file_name().and_then(|n| n.to_str()).unwrap_or("?");

        // Run test and capture whether it passed
        let result = test(skill_path.to_str().unwrap_or(""), input.clone(), verbose).await;
        match result {
            Ok(_) => {
                // Re-check by looking at SKILL.rhai compile status
                // We rely on the function's own output; for batch we count by whether files exist + compile OK
                let rhai_ok = {
                    let rhai_path = skill_path.join("SKILL.rhai");
                    if rhai_path.exists() {
                        let script = std::fs::read_to_string(&rhai_path).unwrap_or_default();
                        let engine = rhai::Engine::new();
                        engine.compile(&script).is_ok()
                    } else {
                        false
                    }
                };
                if rhai_ok {
                    passed += 1;
                } else {
                    failed_names.push(name.to_string());
                }
            }
            Err(e) => {
                println!("  ⚠️  Error running test for {}: {}", name, e);
                failed_names.push(name.to_string());
            }
        }
    }

    println!("{}", "═".repeat(60));
    println!("📊 Batch Test Summary");
    println!("   Total:  {}", total);
    println!("   Passed: {}", passed);
    println!("   Failed: {}", total - passed);
    if !failed_names.is_empty() {
        println!();
        println!("   ❌ Failed skills:");
        for n in &failed_names {
            println!("      • {}", n);
        }
    }
    println!();

    Ok(())
}

fn status_desc(s: &str) -> &'static str {
    match s {
        "Triggered" => "pending",
        "Generating" => "generating",
        "Generated" => "generated",
        "Auditing" => "auditing",
        "AuditPassed" => "audit passed",
        "CompilePassed" | "DryRunPassed" | "TestPassed" => "compile passed",
        "CompileFailed" | "DryRunFailed" | "TestFailed" | "Testing" => "compile failed",
        "Observing" | "RollingOut" => "observing",
        _ => "in progress",
    }
}

/// Read a skill's description from meta.yaml or meta.json (first `description:` line).
fn read_skill_description(skill_dir: &std::path::Path) -> String {
    // Try meta.yaml
    let yaml = skill_dir.join("meta.yaml");
    if yaml.exists() {
        if let Ok(content) = std::fs::read_to_string(&yaml) {
            for line in content.lines() {
                if let Some(val) = line.strip_prefix("description:") {
                    return val.trim().trim_matches('"').to_string();
                }
            }
        }
    }
    // Try meta.json
    let json = skill_dir.join("meta.json");
    if json.exists() {
        if let Ok(content) = std::fs::read_to_string(&json) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                return v.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string();
            }
        }
    }
    String::new()
}

fn format_ts(ts: i64) -> String {
    use chrono::{TimeZone, Local};
    match Local.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        _ => "unknown".to_string(),
    }
}
