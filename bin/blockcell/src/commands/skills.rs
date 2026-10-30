use blockcell_agent::AgentRuntime;
use blockcell_core::types::ChatMessage;
use blockcell_core::{build_session_key, Config, InboundMessage, Paths};
use blockcell_skills::evolution::EvolutionRecord;
use blockcell_skills::is_builtin_tool;
use blockcell_skills::SkillTestFixture;
use blockcell_storage::SessionStore;
use blockcell_tools::build_tool_registry_for_agent_config;
use blockcell_tools::mcp::manager::McpManager;
use std::sync::Arc;

use super::memory_store::open_memory_store;

fn skill_test_intro_note() -> &'static str {
    "SKILL.md decides when script assets run; this command only validates top-level compatibility assets and basic asset contracts."
}

fn skill_test_primary_asset_step_label() -> &'static str {
    "primary asset check"
}

fn skill_test_rhai_compat_step_label() -> &'static str {
    "legacy Rhai compat"
}

fn prepare_skill_test_workspace(source_dir: &std::path::Path) -> anyhow::Result<Paths> {
    let overlay_base = std::env::temp_dir().join(format!(
        "blockcell-skill-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let overlay_paths = Paths::with_base(overlay_base);
    let user_paths = Paths::new();

    if let Some(parent) = overlay_paths.config_file().parent() {
        std::fs::create_dir_all(parent)?;
    }

    let user_config_path = user_paths.config_file();
    if user_config_path.exists() {
        std::fs::copy(&user_config_path, overlay_paths.config_file())?;
    }

    let user_env_path = user_paths.env_file();
    if user_env_path.exists() {
        std::fs::copy(&user_env_path, overlay_paths.env_file())?;
    }

    let overlay_skills_dir = overlay_paths.skills_dir();
    std::fs::create_dir_all(&overlay_skills_dir)?;

    for entry in std::fs::read_dir(source_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let target = overlay_skills_dir.join(entry.file_name());
        copy_dir_recursive(&path, &target)?;
    }

    Ok(overlay_paths)
}

fn copy_dir_recursive(source: &std::path::Path, target: &std::path::Path) -> anyhow::Result<()> {
    if target.exists() {
        std::fs::remove_dir_all(target)?;
    }
    std::fs::create_dir_all(target)?;

    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());

        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else if entry.file_type()?.is_file() {
            if let Some(parent) = target_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&source_path, &target_path)?;
        }
    }

    Ok(())
}

#[derive(Default)]
struct FixtureSummary {
    total: usize,
    passed: usize,
    failed: usize,
}

fn record_failed_skill(failed_names: &mut Vec<String>, skill_name: &str) {
    if !failed_names.iter().any(|name| name == skill_name) {
        failed_names.push(skill_name.to_string());
    }
}

fn load_skill_test_fixtures(skill_path: &std::path::Path) -> Vec<SkillTestFixture> {
    let tests_dir = skill_path.join("tests");
    if !tests_dir.exists() {
        return vec![];
    }

    let mut fixtures = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&tests_dir) {
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.path());

        for entry in entries {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(fixture) = serde_json::from_str::<SkillTestFixture>(&content) {
                        fixtures.push(fixture);
                    }
                }
            }
        }
    }

    fixtures
}

fn merge_object_fields(
    target: &mut serde_json::Map<String, serde_json::Value>,
    value: &serde_json::Value,
) {
    if let Some(object) = value.as_object() {
        for (key, field_value) in object {
            target.insert(key.clone(), field_value.clone());
        }
    }
}

fn build_fixture_metadata(skill_name: &str, fixture: &SkillTestFixture) -> serde_json::Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert("test_mode".to_string(), serde_json::json!("fixture"));
    metadata.insert("fixture_name".to_string(), serde_json::json!(fixture.name));
    metadata.insert(
        "fixture_input".to_string(),
        serde_json::json!(fixture.input),
    );
    metadata.insert("context".to_string(), fixture.context.clone());
    metadata.insert("params".to_string(), fixture.params.clone());
    metadata.insert("constraints".to_string(), fixture.constraints.clone());

    merge_object_fields(&mut metadata, &fixture.context);
    merge_object_fields(&mut metadata, &fixture.params);
    merge_object_fields(&mut metadata, &fixture.constraints);

    metadata.insert(
        "forced_skill_name".to_string(),
        serde_json::json!(skill_name),
    );

    serde_json::Value::Object(metadata)
}

fn collect_fixture_tool_calls(messages: &[ChatMessage]) -> Vec<String> {
    let mut tools = Vec::new();

    for message in messages {
        if message.role != "assistant" {
            continue;
        }

        if let Some(tool_calls) = message.tool_calls.as_ref() {
            for tool_call in tool_calls {
                if tool_call.name == "skill_enter" {
                    continue;
                }
                tools.push(tool_call.name.clone());
            }
        }
    }

    tools
}

fn response_matches_expected_output(actual: &str, expected: Option<&str>) -> bool {
    let Some(expected) = expected.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };

    actual.to_lowercase().contains(&expected.to_lowercase())
}

async fn build_skill_test_runtime(paths: &Paths) -> anyhow::Result<AgentRuntime> {
    let config = Config::load_or_default(paths)?;

    let provider_pool = blockcell_providers::ProviderPool::from_config(&config)?;

    let memory_store_handle = if let Ok(store) = open_memory_store(paths, &config) {
        use blockcell_agent::MemoryStoreAdapter;
        use std::sync::Arc;
        Some(Arc::new(MemoryStoreAdapter::new(store)) as blockcell_tools::MemoryStoreHandle)
    } else {
        None
    };

    let mcp_manager = Arc::new(McpManager::load(paths).await?);
    let tool_registry = build_tool_registry_for_agent_config(&config, Some(&mcp_manager)).await?;
    let mut runtime = AgentRuntime::new(config, paths.clone(), provider_pool, tool_registry)?;

    if let Some(handle) = memory_store_handle {
        runtime.set_memory_store(handle);
    }

    runtime.init_memory_file_store()?;

    Ok(runtime)
}

async fn run_skill_fixtures(
    skill_name: &str,
    skill_path: &std::path::Path,
    runtime_paths: &Paths,
) -> anyhow::Result<FixtureSummary> {
    let fixtures = load_skill_test_fixtures(skill_path);
    if fixtures.is_empty() {
        return Ok(FixtureSummary::default());
    }

    let mut runtime = build_skill_test_runtime(runtime_paths).await?;
    let session_store = SessionStore::new(runtime_paths.clone());
    let mut summary = FixtureSummary {
        total: fixtures.len(),
        ..Default::default()
    };

    println!("  🧪 Fixture regression ({} cases)", summary.total);

    for (index, fixture) in fixtures.iter().enumerate() {
        let chat_id = format!("fixture:{}:{}", skill_name, fixture.name);
        let session_key = build_session_key("cli", &chat_id);
        let metadata = build_fixture_metadata(skill_name, fixture);

        let inbound = InboundMessage {
            channel: "cli".to_string(),
            account_id: None,
            sender_id: "user".to_string(),
            chat_id: chat_id.clone(),
            content: fixture.input.clone(),
            media: vec![],
            metadata,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };

        print!("    [{}/{}] {:<24}", index + 1, summary.total, fixture.name);
        let run_result = runtime.process_message(inbound).await;

        let outcome = match run_result {
            Ok(response) => {
                let history = session_store.load(&session_key)?;
                let actual_tools = collect_fixture_tool_calls(&history);
                let expected_tools_match =
                    fixture.expected_tools.is_empty() || actual_tools == fixture.expected_tools;
                let expected_output_match =
                    response_matches_expected_output(&response, fixture.expected_output.as_deref());

                if expected_tools_match && expected_output_match {
                    summary.passed += 1;
                    "✅"
                } else {
                    summary.failed += 1;
                    println!();
                    if !expected_tools_match {
                        println!("      expected tools: {:?}", fixture.expected_tools);
                        println!("      actual tools:   {:?}", actual_tools);
                    }
                    if !expected_output_match {
                        let preview = response.chars().take(240).collect::<String>();
                        if let Some(expected) = fixture.expected_output.as_deref() {
                            println!("      expected output: {:?}", expected.trim());
                        }
                        println!("      actual output:   {:?}", preview);
                    }
                    "❌"
                }
            }
            Err(error) => {
                summary.failed += 1;
                println!();
                println!("      error: {}", error);
                "❌"
            }
        };

        println!(" {}", outcome);
        let _ = std::fs::remove_file(runtime_paths.session_file(&session_key));
    }

    Ok(summary)
}

/// List all skill evolution records.
pub async fn list(all: bool, enabled_only: bool) -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let records_dir = paths.workspace().join("evolution_records");
    let skills_dir = paths.skills_dir();

    // 将阻塞 I/O 移至 spawn_blocking，避免阻塞 Tokio 工作线程
    let mut records: Vec<EvolutionRecord> = tokio::task::spawn_blocking(move || {
        let mut records = Vec::new();
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
        records
    })
    .await
    .unwrap_or_default();
    records.sort_by_key(|b| std::cmp::Reverse(b.created_at));

    // Categorize: deduplicate by skill_name (keep latest record per skill)
    let mut seen = std::collections::HashSet::new();
    let mut learning = Vec::new();
    let mut learned = Vec::new();
    let mut failed = Vec::new();
    let mut builtin_count: usize = 0;

    for r in &records {
        if is_builtin_tool(&r.skill_name) {
            builtin_count += 1;
            if !all {
                continue;
            }
        }
        if !seen.insert(r.skill_name.clone()) && !all {
            continue;
        }

        let status_str = format!("{:?}", r.status);
        match status_str.as_str() {
            "Completed" => learned.push(r),
            "Failed" | "RolledBack" | "AuditFailed" | "DryRunFailed" | "TestFailed" => {
                failed.push(r)
            }
            _ => learning.push(r),
        }
    }

    // 将阻塞 I/O 移至 spawn_blocking，收集可用 skills
    let skill_roots = [paths.builtin_skills_dir(), skills_dir.clone()];
    let mut available_skills: Vec<(String, std::path::PathBuf)> =
        tokio::task::spawn_blocking(move || {
            let mut skills = std::collections::BTreeMap::new();
            for root in skill_roots {
                if let Ok(entries) = std::fs::read_dir(&root) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if p.is_dir()
                            && (p.join("SKILL.rhai").exists()
                                || p.join("SKILL.py").exists()
                                || p.join("SKILL.md").exists())
                        {
                            let name = p
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("")
                                .to_string();
                            skills.insert(name, p);
                        }
                    }
                }
            }
            skills.into_iter().collect()
        })
        .await
        .unwrap_or_default();
    available_skills.sort_by(|a, b| a.0.cmp(&b.0));

    // Filter by enabled if requested (skills with .disabled marker are excluded)
    if enabled_only {
        available_skills.retain(|(_, p)| !p.join(".disabled").exists());
    }

    let available_count = available_skills.len();

    println!();
    println!("🧠 Skill Status");
    println!(
        "  📦 Loaded: {}  ✅ Learned: {}  🔄 Learning: {}  ❌ Failed: {}",
        available_count,
        learned.len(),
        learning.len(),
        failed.len()
    );

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
            println!(
                "    • {} [{}] ({})",
                r.skill_name,
                desc,
                format_ts(r.created_at)
            );
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
        println!(
            "  ℹ️  {} built-in tool error records hidden (use --all to view, or clear to clean up)",
            builtin_count
        );
    }

    if learning.is_empty() && learned.is_empty() && failed.is_empty() && builtin_count == 0 {
        println!("  (No skill records)");
    }
    println!();
    Ok(())
}

/// Resolve user overrides before builtins, accepting only a single skill name.
fn resolve_skill_path(paths: &Paths, name: &str) -> anyhow::Result<std::path::PathBuf> {
    let mut components = std::path::Path::new(name).components();
    anyhow::ensure!(
        matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none(),
        "Invalid skill name: {}",
        name
    );
    let user_path = paths.skills_dir().join(name);
    if user_path.is_dir() {
        Ok(user_path)
    } else {
        Ok(paths.builtin_skills_dir().join(name))
    }
}

/// Show details for a specific skill.
pub async fn show(name: &str) -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let skills_dir = paths.skills_dir();
    let skill_path = resolve_skill_path(&paths, name)?;

    if !skill_path.exists() || !skill_path.is_dir() {
        println!("❌ Skill '{}' not found in {}", name, skills_dir.display());
        println!("  Use `blockcell skills list` to see available skills.");
        return Ok(());
    }

    println!();
    println!("🧠 Skill: {}", name);
    println!("  Path: {}", skill_path.display());

    let disabled = skill_path.join(".disabled").exists();
    println!(
        "  Status: {}",
        if disabled {
            "⏸  disabled"
        } else {
            "✅ enabled"
        }
    );

    let meta_path = skill_path.join("meta.yaml");
    if meta_path.exists() {
        println!();
        println!("  meta.yaml:");
        let content = tokio::fs::read_to_string(&meta_path)
            .await
            .unwrap_or_default();
        for line in content.lines().take(30) {
            println!("    {}", line);
        }
    }

    if skill_path.join("SKILL.rhai").exists() {
        println!();
        println!("  Script Asset: SKILL.rhai ✓ (top-level compatibility)");
    }
    if skill_path.join("SKILL.py").exists() {
        println!();
        println!("  Script Asset: SKILL.py ✓ (top-level compatibility)");
    }
    if skill_path.join("SKILL.md").exists() {
        println!();
        println!("  Control Plane: SKILL.md ✓");
    }

    // 将阻塞 I/O 移至 spawn_blocking，加载进化记录
    let records_dir = paths.workspace().join("evolution_records");
    let name_clone = name.to_string();
    let mut records: Vec<EvolutionRecord> = tokio::task::spawn_blocking(move || {
        let mut records = Vec::new();
        if records_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&records_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().is_some_and(|e| e == "json") {
                        if let Ok(content) = std::fs::read_to_string(&p) {
                            if let Ok(r) = serde_json::from_str::<EvolutionRecord>(&content) {
                                if r.skill_name == name_clone {
                                    records.push(r);
                                }
                            }
                        }
                    }
                }
            }
        }
        records
    })
    .await
    .unwrap_or_default();
    records.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    if !records.is_empty() {
        println!();
        println!("  Evolution records ({}):", records.len());
        for r in records.iter().take(5) {
            println!(
                "    {} {:?} — {}",
                &r.id.chars().take(12).collect::<String>(),
                r.status,
                format_ts(r.created_at)
            );
        }
    }

    println!();
    Ok(())
}

/// Enable or disable a skill by creating/removing a .disabled marker.
pub async fn set_enabled(name: &str, enable: bool) -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let skill_path = resolve_skill_path(&paths, name)?;

    if !skill_path.exists() || !skill_path.is_dir() {
        println!("❌ Skill '{}' not found.", name);
        return Ok(());
    }

    let marker = skill_path.join(".disabled");
    if enable {
        if marker.exists() {
            tokio::fs::remove_file(&marker).await?;
            println!("✅ Skill '{}' enabled.", name);
        } else {
            println!("  Skill '{}' is already enabled.", name);
        }
    } else if !marker.exists() {
        tokio::fs::write(&marker, "").await?;
        println!("⏸  Skill '{}' disabled.", name);
    } else {
        println!("  Skill '{}' is already disabled.", name);
    }
    Ok(())
}

/// Hot-reload: report skill count (actual reload happens at agent startup).
pub async fn reload() -> anyhow::Result<()> {
    let paths = Paths::new_configured();

    // Re-extract builtin skills (skips existing files)
    match super::embedded_skills::extract_to_directory(&paths.managed_builtin_skills_dir()) {
        Ok(new_skills) if !new_skills.is_empty() => {
            println!(
                "✓ Extracted {} new builtin skill(s): {}",
                new_skills.len(),
                new_skills.join(", ")
            );
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("⚠️  Failed to extract builtin skills: {}", e);
        }
    }

    let index = blockcell_agent::skill_index::SkillIndex::build_from_dirs([
        paths.builtin_skills_dir(),
        paths.skills_dir(),
    ]);
    let count = index.entries().len();

    println!(
        "✅ Skills directory refreshed. {} skill(s) available.",
        count
    );
    println!("   Note: Running agent processes will pick up changes on their next tick.");
    Ok(())
}

/// Clear all evolution records.
pub async fn clear() -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let records_dir = paths.workspace().join("evolution_records");
    let mut count = 0;

    if records_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&records_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json")
                    && std::fs::remove_file(&path).is_ok()
                {
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
    let paths = Paths::new_configured();
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
                                && std::fs::remove_file(&path).is_ok()
                            {
                                count += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    if count > 0 {
        println!(
            "✅ Deleted all records for skill `{}` ({} total)",
            skill_name, count
        );
    } else {
        println!("⚠️  No records found for skill `{}`", skill_name);
    }
    Ok(())
}

/// Learn a new skill by sending a request to the agent.
pub async fn learn(description: &str) -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let config = Config::load_or_default(&paths)?;

    // Create provider pool using shared multi-provider dispatch
    let provider_pool = blockcell_providers::ProviderPool::from_config(&config)?;

    // Optionally wire up memory store
    let memory_store_handle = if let Ok(store) = open_memory_store(&paths, &config) {
        use blockcell_agent::MemoryStoreAdapter;
        use std::sync::Arc;
        Some(Arc::new(MemoryStoreAdapter::new(store)) as blockcell_tools::MemoryStoreHandle)
    } else {
        None
    };

    // Create runtime
    let mcp_manager = Arc::new(McpManager::load(&paths).await?);
    let tool_registry = build_tool_registry_for_agent_config(&config, Some(&mcp_manager)).await?;
    let mut runtime = AgentRuntime::new(config, paths.clone(), provider_pool, tool_registry)?;

    if let Some(handle) = memory_store_handle {
        runtime.set_memory_store(handle);
    }

    runtime.init_memory_file_store()?;

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
        account_id: None,
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
    let paths = Paths::new_configured();
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
    println!(
        "   Version: {}",
        info.get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
    );

    Ok(())
}

/// Test a skill directory by validating metadata and checking skill script syntax.
/// Rhai skills run with mock tools; Python skills run syntax check only.
pub async fn test(path: &str, input: Option<String>, verbose: bool) -> anyhow::Result<()> {
    use rhai::{Dynamic, Engine, Map, Scope};
    use std::sync::{Arc, Mutex};

    let skill_path = std::path::Path::new(path);
    let skill_name = skill_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);

    println!();
    println!("🧪 Testing skill: {}", skill_name);
    println!("   Path: {}", skill_path.display());
    println!("   Note: {}", skill_test_intro_note());
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
        match serde_yaml::from_str::<serde_json::Value>(&meta_str) {
            Ok(meta) => {
                let required = ["name", "description"];
                let missing: Vec<&str> = required
                    .iter()
                    .filter(|key| {
                        meta.get(**key)
                            .and_then(|value| value.as_str())
                            .map(|value| value.trim().is_empty())
                            .unwrap_or(true)
                    })
                    .copied()
                    .collect();
                if missing.is_empty() {
                    println!("✅ OK");
                    pass += 1;
                    if verbose {
                        for line in meta_str.lines().take(8) {
                            println!("            {}", line);
                        }
                    }
                } else {
                    println!("❌ Missing required fields: {}", missing.join(", "));
                    fail += 1;
                }
            }
            Err(err) => {
                println!("❌ Invalid YAML: {}", err);
                fail += 1;
            }
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

    // ── Step 3: primary asset compatibility ─────────────────────────────────
    let rhai_path = skill_path.join("SKILL.rhai");
    let py_path = skill_path.join("SKILL.py");
    print!("  [3/3] {:<18}", skill_test_primary_asset_step_label());
    if !rhai_path.exists() {
        if py_path.exists() {
            print!("\r  [3/3] {:<18}", skill_test_primary_asset_step_label());
            match python_syntax_check(&py_path) {
                Ok(_) => {
                    println!("✅ OK (SKILL.py)");
                    pass += 1;
                }
                Err(e) => {
                    println!("❌ {}", e);
                    fail += 1;
                }
            }
        } else {
            println!("✅ OK (Prompt-only)");
            pass += 1;
        }
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
    engine.register_fn(
        "call_tool",
        move |name: &str, _params: rhai::Map| -> Dynamic {
            calls_c
                .lock()
                .unwrap()
                .push((name.to_string(), "{}".to_string()));
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
        },
    );

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
            m.get(key)
                .cloned()
                .unwrap_or_else(|| Dynamic::from("".to_string()))
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
            println!("✅ OK (SKILL.rhai)");
            pass += 1;

            // ── Step 4: top-level Rhai compatibility run ─────────────────────
            print!("  [4/4] {:<18}", skill_test_rhai_compat_step_label());

            // Inject dummy variables from meta.yaml (all common ones as ())
            let user_msg = input
                .as_deref()
                .unwrap_or("test input for skill.md-driven asset validation");
            let mut scope = Scope::new();
            scope.push("user_input", Dynamic::from(user_msg.to_string()));

            // Inject all common optional variables as ()
            let optional_vars = [
                "query",
                "command",
                "url",
                "action",
                "topic",
                "path",
                "source",
                "destination",
                "service",
                "platform",
                "provider",
                "title",
                "body",
                "content",
                "text",
                "message",
                "subject",
                "to",
                "from",
                "limit",
                "max_results",
                "max_pages",
                "timeout",
                "cwd",
                "language",
                "format",
                "algorithm",
                "format",
                "bits",
                "length",
                "type",
                "owner",
                "repo",
                "branch",
                "tag",
                "version",
                "entity_id",
                "domain",
                "payload",
                "topic",
                "host",
                "ports",
                "record_type",
                "region",
                "bucket",
                "instance_id",
                "database_id",
                "page_id",
                "event_id",
                "graph_name",
                "name",
                "relation",
                "from_entity",
                "to_entity",
                "voice",
                "backend",
                "output_path",
                "input_path",
                "image_path",
                "audio_path",
                "chart_type",
                "start",
                "end",
                "start_date",
                "end_date",
                "task_id",
                "id",
                "uid",
                "contact_id",
                "origin",
                "destination",
                "keyword",
                "location",
                "mode",
                "radius",
                "recursive",
                "max_pages",
                "action_type",
                "schedule",
                "task",
                "number",
                "address",
                "query",
                "filter",
                "sort_by",
                "channel",
                "service",
                "max_results",
                "source",
                "include_symbols",
                "fetch_top",
                "watch",
                "depth",
                "bidirectional",
                "top_k",
                "stats",
                "export_format",
                "camera_id",
                "priority",
                "count",
                "include_uppercase",
                "include_numbers",
                "session",
                "browser",
                "ms",
                "tab_id",
                "extract_type",
                "model",
                "output_format",
                "auto_filter",
                "bold_header",
                "freeze_panes",
                "column_widths",
                "slides",
                "sections",
                "sheets",
                "attachments",
                "tags",
                "importance",
                "scope",
                "dedup_key",
                "expires_in_days",
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
                        let var_name = err_str.split('\'').nth(1).unwrap_or(&err_str);
                        println!(
                            "⚠️  WARN — undefined variable '{}' (add to optional_vars list)",
                            var_name
                        );
                        println!("            Full error: {}", err_str);
                        // Treat as warning only — the script compiled and mostly ran fine
                        pass += 1;
                        errors_c
                            .lock()
                            .unwrap()
                            .push(format!("Warn (undef var): {}", var_name));
                    } else {
                        println!("❌ Runtime error: {}", e);
                        fail += 1;
                        errors_c
                            .lock()
                            .unwrap()
                            .push(format!("Runtime: {}", err_str));
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
        println!(
            "  ❌ FAIL  ({}/{} checks passed, {} failed)",
            pass, total, fail
        );
    }
    println!();
}

fn python_syntax_check(py_path: &std::path::Path) -> std::result::Result<(), String> {
    let candidates = ["python3", "python"];
    let mut last_output: Option<String> = None;
    let mut has_runtime = false;

    for bin in candidates {
        match std::process::Command::new(bin)
            .arg("-m")
            .arg("py_compile")
            .arg(py_path)
            .output()
        {
            Ok(output) => {
                has_runtime = true;
                if output.status.success() {
                    return Ok(());
                }
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let detail = if !stderr.is_empty() {
                    stderr
                } else if !stdout.is_empty() {
                    stdout
                } else {
                    format!("{} returned non-zero status", bin)
                };
                last_output = Some(detail);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(e) => {
                return Err(format!("failed to run python compiler: {}", e));
            }
        }
    }

    if !has_runtime {
        return Err("python runtime not found (python3/python)".to_string());
    }
    Err(last_output.unwrap_or_else(|| "python syntax check failed".to_string()))
}

/// Batch-test all skills under a directory.
pub async fn test_all(dir: &str, input: Option<String>, verbose: bool) -> anyhow::Result<()> {
    let base = std::path::Path::new(dir);
    if !base.exists() || !base.is_dir() {
        anyhow::bail!("Directory not found: {}", dir);
    }

    let runtime_paths = prepare_skill_test_workspace(base)?;

    let entries: Vec<_> = std::fs::read_dir(base)?
        .flatten()
        .filter(|e| {
            let p = e.path();
            p.is_dir()
                && (p.join("SKILL.rhai").exists()
                    || p.join("SKILL.py").exists()
                    || p.join("meta.yaml").exists())
        })
        .collect();

    if entries.is_empty() {
        println!("No skill directories found in: {}", dir);
        return Ok(());
    }

    let total = entries.len();
    let mut passed = 0usize;
    let mut failed_names: Vec<String> = Vec::new();
    let mut fixture_total = 0usize;
    let mut fixture_passed = 0usize;
    let mut fixture_failed = 0usize;

    println!();
    println!("🧪 Batch testing {} skills in: {}", total, dir);
    println!("{}", "─".repeat(60));

    for entry in &entries {
        let skill_path = entry.path();
        let name = skill_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?");

        // Run test and capture whether the top-level compatibility assets still validate
        let result = test(skill_path.to_str().unwrap_or(""), input.clone(), verbose).await;
        match result {
            Ok(_) => {
                let mut skill_passed = true;
                // Re-check by primary compatibility asset (top-level Rhai compile or Python syntax).
                let script_ok = {
                    let rhai_path = skill_path.join("SKILL.rhai");
                    if rhai_path.exists() {
                        let script = std::fs::read_to_string(&rhai_path).unwrap_or_default();
                        let engine = rhai::Engine::new();
                        engine.compile(&script).is_ok()
                    } else {
                        let py_path = skill_path.join("SKILL.py");
                        if py_path.exists() {
                            python_syntax_check(&py_path).is_ok()
                        } else {
                            // Prompt-only skills have no top-level script asset to compile.
                            true
                        }
                    }
                };
                if !script_ok {
                    skill_passed = false;
                    record_failed_skill(&mut failed_names, name);
                }

                match run_skill_fixtures(name, &skill_path, &runtime_paths).await {
                    Ok(summary) => {
                        fixture_total += summary.total;
                        fixture_passed += summary.passed;
                        fixture_failed += summary.failed;
                        if summary.failed > 0 {
                            skill_passed = false;
                        }
                        if summary.failed > 0 {
                            record_failed_skill(&mut failed_names, name);
                        }
                    }
                    Err(e) => {
                        println!("  ⚠️  Error running fixtures for {}: {}", name, e);
                        skill_passed = false;
                        record_failed_skill(&mut failed_names, name);
                    }
                }

                if skill_passed {
                    passed += 1;
                }
            }
            Err(e) => {
                println!("  ⚠️  Error running test for {}: {}", name, e);
                record_failed_skill(&mut failed_names, name);
            }
        }
    }

    println!("{}", "═".repeat(60));
    println!("📊 Batch Test Summary");
    println!("   Total:  {}", total);
    println!("   Passed: {}", passed);
    println!("   Failed: {}", failed_names.len());
    if fixture_total > 0 {
        println!("   Fixture cases: {}", fixture_total);
        println!("   Fixture passed: {}", fixture_passed);
        println!("   Fixture failed: {}", fixture_failed);
    }
    if !failed_names.is_empty() {
        println!();
        println!("   ❌ Failed skills:");
        for n in &failed_names {
            println!("      • {}", n);
        }
    }
    println!();

    let _ = std::fs::remove_dir_all(runtime_paths.base);

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
                return v
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
            }
        }
    }
    String::new()
}

fn format_ts(ts: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        _ => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_test_describes_rhai_as_skill_md_driven_asset() {
        let note = skill_test_intro_note();
        assert!(note.contains("SKILL.md decides when script assets run"));
        assert!(note.contains("top-level compatibility assets"));
        assert!(skill_test_primary_asset_step_label().contains("primary asset"));
        assert!(skill_test_rhai_compat_step_label().contains("compat"));
    }
}
