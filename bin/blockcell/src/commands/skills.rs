use blockcell_agent::AgentRuntime;
use blockcell_core::{Config, InboundMessage, Paths};
use blockcell_skills::evolution::EvolutionRecord;
use blockcell_skills::is_builtin_tool;
use blockcell_storage::MemoryStore;
use blockcell_tools::ToolRegistry;

/// List all skill evolution records.
pub async fn list(all: bool) -> anyhow::Result<()> {
    let paths = Paths::default();
    let records_dir = paths.workspace().join("evolution_records");
    let skills_dir = paths.skills_dir();

    // Load all evolution records
    let mut records: Vec<EvolutionRecord> = Vec::new();
    if records_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&records_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |e| e == "json") {
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
        if !seen.insert(r.skill_name.clone()) {
            if !all { continue; }
        }

        let status_str = format!("{:?}", r.status);
        match status_str.as_str() {
            "Completed" => learned.push(r),
            "Failed" | "RolledBack" | "AuditFailed" | "DryRunFailed" | "TestFailed" => failed.push(r),
            _ => learning.push(r),
        }
    }

    // Count available skills
    let mut available_count = 0;
    if skills_dir.exists() && skills_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && (p.join("SKILL.rhai").exists() || p.join("SKILL.md").exists()) {
                    available_count += 1;
                }
            }
        }
    }

    println!();
    println!("🧠 Skill Status");
    println!("  📦 Loaded: {}  ✅ Learned: {}  🔄 Learning: {}  ❌ Failed: {}",
        available_count, learned.len(), learning.len(), failed.len());

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

/// Clear all evolution records.
pub async fn clear() -> anyhow::Result<()> {
    let paths = Paths::default();
    let records_dir = paths.workspace().join("evolution_records");
    let mut count = 0;

    if records_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&records_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |e| e == "json") {
                    if std::fs::remove_file(&path).is_ok() {
                        count += 1;
                    }
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
                if path.extension().map_or(false, |e| e == "json") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(record) = serde_json::from_str::<EvolutionRecord>(&content) {
                            if record.skill_name == skill_name {
                                if std::fs::remove_file(&path).is_ok() {
                                    count += 1;
                                }
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
    
    // Fallback: if no dist_url (e.g. source-only), we might need to clone or error. 
    // For now, assume dist_url is present or use source_url.
    let download_url = dist_url
        .or_else(|| info.get("source_url").and_then(|v| v.as_str()))
        .ok_or_else(|| anyhow::anyhow!("No download URL (dist_url or source_url) found for skill"))?;

    println!("📦 Downloading from {}...", download_url);

    // 2. Download artifact
    let resp = client.get(download_url).send().await?;
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

fn status_desc(s: &str) -> &'static str {
    match s {
        "Triggered" => "pending",
        "Generating" => "generating",
        "Generated" => "generated",
        "Auditing" => "auditing",
        "AuditPassed" => "audit passed",
        "DryRunPassed" => "build passed",
        "Testing" => "testing",
        "TestPassed" => "test passed",
        "RollingOut" => "rolling out",
        _ => "in progress",
    }
}

fn format_ts(ts: i64) -> String {
    use chrono::{TimeZone, Local};
    match Local.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        _ => "unknown".to_string(),
    }
}
