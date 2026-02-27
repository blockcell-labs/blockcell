use blockcell_core::Paths;
use blockcell_tools::ToolRegistry;
use serde_json::Value;
use std::collections::BTreeMap;

fn schema_function(schema: &Value) -> &Value {
    schema.get("function").unwrap_or(schema)
}

/// List all registered tools.
pub async fn list(category: Option<String>) -> anyhow::Result<()> {
    let registry = ToolRegistry::with_defaults();
    let schemas = registry.get_tool_schemas();

    println!();
    println!("🔧 Registered tools ({} total)", schemas.len());
    println!();

    // Group by category based on tool name patterns
    let mut categorized: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();

    for schema in &schemas {
        let func = schema_function(schema);
        let name = func["name"].as_str().unwrap_or("");
        let cat = categorize_tool(name);
        categorized.entry(cat).or_default().push(schema);
    }

    let filter = category.as_deref();

    for (cat, tools) in &categorized {
        if let Some(f) = filter {
            if !cat.to_lowercase().contains(&f.to_lowercase()) {
                continue;
            }
        }

        println!("  📂 {} ({})", cat, tools.len());
        for tool in tools {
            let func = schema_function(tool);
            let name = func["name"].as_str().unwrap_or("");
            let desc = func["description"].as_str().unwrap_or("");
            let short_desc: String = desc.chars().take(60).collect();
            let ellipsis = if desc.chars().count() > 60 { "..." } else { "" };
            println!("     {:<22} {}{}", name, short_desc, ellipsis);
        }
        println!();
    }

    Ok(())
}

/// Show detailed info for a specific tool.
pub async fn info(tool_name: &str) -> anyhow::Result<()> {
    let registry = ToolRegistry::with_defaults();
    let schemas = registry.get_tool_schemas();

    let schema = schemas.iter().find(|s: &&Value| {
        schema_function(s)["name"].as_str() == Some(tool_name)
    });

    match schema {
        Some(s) => {
            let func = schema_function(s);
            println!();
            println!("🔧 {}", func["name"].as_str().unwrap_or(""));
            println!();
            println!("  Description: {}", func["description"].as_str().unwrap_or(""));
            println!();

            if let Some(params) = func.get("parameters") {
                println!("  Parameters:");
                let params_obj: &Value = params;
                if let Some(props) = params_obj.get("properties") {
                    if let Some(obj) = props.as_object() {
                        let required: Vec<&str> = params_obj.get("required")
                            .and_then(|r: &Value| r.as_array())
                            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<&str>>())
                            .unwrap_or_default();

                        for (key, val) in obj {
                            let typ = val.get("type").and_then(|t| t.as_str()).unwrap_or("any");
                            let desc = val.get("description").and_then(|d| d.as_str()).unwrap_or("");
                            let req = if required.contains(&key.as_str()) { " (required)" } else { "" };

                            // Show enum values if present
                            let enum_str = if let Some(enums) = val.get("enum") {
                                if let Some(arr) = enums.as_array() {
                                    let vals: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                                    format!(" [{}]", vals.join("|"))
                                } else {
                                    String::new()
                                }
                            } else {
                                String::new()
                            };

                            println!("    {:<20} {:<8}{}{}", key, typ, req, enum_str);
                            if !desc.is_empty() {
                                // Wrap long descriptions
                                let short: String = desc.chars().take(80).collect();
                                println!("      {}", short);
                                if desc.chars().count() > 80 {
                                    let rest: String = desc.chars().skip(80).take(80).collect();
                                    println!("      {}", rest);
                                }
                            }
                        }
                    }
                }
            }
            println!();
        }
        None => {
            eprintln!("Tool '{}' not found.", tool_name);
            eprintln!();
            eprintln!("Use `blockcell tools list` to see all available tools.");
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Test a tool by calling it directly with JSON params.
pub async fn test(tool_name: &str, params_json: &str) -> anyhow::Result<()> {
    let registry = ToolRegistry::with_defaults();
    let paths = Paths::new();

    let tool = registry.get(tool_name)
        .ok_or_else(|| anyhow::anyhow!("Tool '{}' not found", tool_name))?;

    let params: Value = serde_json::from_str(params_json)
        .map_err(|e| anyhow::anyhow!("Failed to parse JSON params: {}", e))?;

    // Validate
    if let Err(e) = tool.validate(&params) {
        eprintln!("❌ Parameter validation failed: {}", e);
        std::process::exit(1);
    }

    let ctx = blockcell_tools::ToolContext {
        workspace: paths.workspace(),
        builtin_skills_dir: Some(paths.builtin_skills_dir()),
        config: blockcell_core::Config::load_or_default(&paths)?,
        session_key: "cli:test".to_string(),
        channel: String::new(),
        chat_id: String::new(),
        permissions: blockcell_core::types::PermissionSet::new(),
        outbound_tx: None,
        spawn_handle: None,
        task_manager: None,
        memory_store: None,
        capability_registry: None,
        core_evolution: None,
    };

    println!("⏳ Executing {} ...", tool_name);
    let result = tool.execute(ctx, params).await;

    match result {
        Ok(value) => {
            println!("✅ Result:");
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        Err(e) => {
            eprintln!("❌ Execution failed: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Toggle a tool on/off.
pub async fn toggle(tool_name: &str, enable: bool) -> anyhow::Result<()> {
    let paths = Paths::new();
    let toggles_path = paths.workspace().join("toggles.json");

    let mut store: Value = if toggles_path.exists() {
        let content = std::fs::read_to_string(&toggles_path)?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({"skills": {}, "tools": {}}))
    } else {
        serde_json::json!({"skills": {}, "tools": {}})
    };

    // Verify tool exists
    let registry = ToolRegistry::with_defaults();
    if registry.get(tool_name).is_none() {
        eprintln!("⚠ Tool '{}' not found in registry, but toggle state will be recorded.", tool_name);
    }

    if store.get("tools").is_none() {
        store["tools"] = serde_json::json!({});
    }

    if enable {
        if let Some(obj) = store["tools"].as_object_mut() {
            obj.remove(tool_name);
        }
        println!("✓ Tool '{}' enabled", tool_name);
    } else {
        store["tools"][tool_name] = serde_json::json!(false);
        println!("✓ Tool '{}' disabled", tool_name);
    }

    let content = serde_json::to_string_pretty(&store)?;
    std::fs::create_dir_all(toggles_path.parent().unwrap())?;
    std::fs::write(&toggles_path, content)?;

    Ok(())
}

fn categorize_tool(name: &str) -> &'static str {
    match name {
        "read_file" | "write_file" | "edit_file" | "list_dir" | "file_ops" => "Filesystem",
        "exec" => "Execution",
        "web_search" | "web_fetch" | "browse" | "http_request" => "Web/Browser",
        "chrome_control" | "app_control" => "GUI Automation",
        "message" | "spawn" | "list_tasks" | "notification" | "email" => "Communication",
        "cron" => "Scheduling",
        "memory_query" | "memory_upsert" | "memory_forget" => "Memory",
        "list_skills" | "toggle_manage" => "Skill Management",
        "system_info" | "capability_evolve" => "System/Evolution",
        "camera_capture" | "ocr" | "image_understand" | "tts" | "audio_transcribe" => "Media",
        "chart_generate" | "office_write" | "data_process" => "Data/Documents",
        "video_process" => "Video",
        "finance_api" | "exchange_api" | "alert_rule" | "stream_subscribe" => "Finance/Trading",
        "blockchain_rpc" | "blockchain_tx" | "contract_security" | "bridge_api" | "nft_market" | "multisig" => "Blockchain",
        "calendar_api" | "contacts" | "social_media" => "Business API",
        "cloud_api" | "git_api" => "DevOps",
        "map_api" | "health_api" | "iot_control" => "Lifestyle/IoT",
        "encrypt" | "network_monitor" => "Security/Network",
        "knowledge_graph" => "Knowledge Graph",
        _ => "Other",
    }
}
