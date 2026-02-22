use blockcell_core::Paths;
use serde_json::Value;

/// List all alert rules.
pub async fn list() -> anyhow::Result<()> {
    let paths = Paths::default();
    let rules_file = paths.workspace().join("alerts").join("rules.json");

    if !rules_file.exists() {
        println!("(No alert rules. Use agent chat or `blockcell alerts add` to create one.)");
        return Ok(());
    }

    let content = std::fs::read_to_string(&rules_file)?;
    let rules: Vec<Value> = serde_json::from_str(&content).unwrap_or_default();

    if rules.is_empty() {
        println!("(No alert rules)");
        return Ok(());
    }

    println!();
    println!("🔔 Alert rules ({} total)", rules.len());
    println!();
    println!("  {:<10} {:<20} {:<10} {:<12} Condition", "ID", "Name", "Enabled", "Operator");
    println!("  {}", "-".repeat(70));

    for rule in &rules {
        let id = rule["id"].as_str().unwrap_or("?");
        let name = rule["name"].as_str().unwrap_or("?");
        let enabled = rule["enabled"].as_bool().unwrap_or(true);
        let operator = rule["operator"].as_str().unwrap_or("?");
        let threshold = &rule["threshold"];

        let short_id = if id.len() > 8 { &id[..8] } else { id };
        let short_name: String = name.chars().take(18).collect();

        println!(
            "  {:<10} {:<20} {:<10} {:<12} {}",
            short_id,
            short_name,
            if enabled { "✓" } else { "✗" },
            operator,
            threshold
        );
    }
    println!();

    Ok(())
}

/// Show alert trigger history.
pub async fn history(limit: usize) -> anyhow::Result<()> {
    let paths = Paths::default();
    let history_file = paths.workspace().join("alerts").join("history.json");

    if !history_file.exists() {
        println!("(No alert trigger history)");
        return Ok(());
    }

    let content = std::fs::read_to_string(&history_file)?;
    let entries: Vec<Value> = serde_json::from_str(&content).unwrap_or_default();

    if entries.is_empty() {
        println!("(No alert trigger history)");
        return Ok(());
    }

    let show_count = entries.len().min(limit);
    let recent = &entries[entries.len().saturating_sub(limit)..];

    println!();
    println!("📜 Alert trigger history (showing {}, {} total)", show_count, entries.len());
    println!();

    for entry in recent.iter().rev() {
        let rule_name = entry["rule_name"].as_str().unwrap_or("?");
        let triggered_at: String = if let Some(s) = entry["triggered_at"].as_str() {
            s.to_string()
        } else if let Some(ms) = entry["triggered_at_ms"].as_i64() {
            use chrono::{TimeZone, Utc};
            Utc.timestamp_millis_opt(ms)
                .single()
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| "?".to_string())
        } else {
            "?".to_string()
        };
        let value = &entry["value"];

        println!("  🔔 {} — value: {} — {}", rule_name, value, triggered_at);
    }
    println!();

    Ok(())
}

/// Manually evaluate all alert rules once.
pub async fn evaluate() -> anyhow::Result<()> {
    let paths = Paths::default();
    let rules_file = paths.workspace().join("alerts").join("rules.json");

    if !rules_file.exists() {
        println!("(No alert rules)");
        return Ok(());
    }

    let content = std::fs::read_to_string(&rules_file)?;
    let rules: Vec<Value> = serde_json::from_str(&content).unwrap_or_default();

    let enabled_count = rules.iter().filter(|r| r["enabled"].as_bool().unwrap_or(true)).count();
    println!("⏳ Evaluating {} enabled alert rules...", enabled_count);
    println!();
    println!("Note: Real-time data sources are not available in CLI mode.");
    println!("Full alert evaluation requires a running agent (via cron or agent chat).");
    println!();
    println!("Current rules overview:");
    for rule in &rules {
        let name = rule["name"].as_str().unwrap_or("?");
        let enabled = rule["enabled"].as_bool().unwrap_or(true);
        let source = rule["source"].as_str().unwrap_or("?");
        if enabled {
            println!("  📊 {} — source: {}", name, source);
        }
    }
    println!();

    Ok(())
}

/// Add a new alert rule.
pub async fn add(
    name: &str,
    source: &str,
    field: &str,
    operator: &str,
    threshold: &str,
) -> anyhow::Result<()> {
    let paths = Paths::default();
    let alerts_dir = paths.workspace().join("alerts");
    std::fs::create_dir_all(&alerts_dir)?;
    let rules_file = alerts_dir.join("rules.json");

    let mut rules: Vec<Value> = if rules_file.exists() {
        let content = std::fs::read_to_string(&rules_file)?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Vec::new()
    };

    // Parse threshold as number or string
    let threshold_val: Value = serde_json::from_str(threshold)
        .unwrap_or_else(|_| Value::String(threshold.to_string()));

    let id = uuid::Uuid::new_v4().to_string();
    let rule = serde_json::json!({
        "id": id,
        "name": name,
        "enabled": true,
        "source": source,
        "field": field,
        "operator": operator,
        "threshold": threshold_val,
        "created_at": chrono::Utc::now().to_rfc3339(),
        "on_trigger": [],
    });

    rules.push(rule);
    let content = serde_json::to_string_pretty(&rules)?;
    std::fs::write(&rules_file, content)?;

    println!("✓ Alert rule created: {} ({})", name, &id.chars().take(8).collect::<String>());
    Ok(())
}

/// Remove an alert rule by ID prefix.
pub async fn remove(rule_id: &str) -> anyhow::Result<()> {
    let paths = Paths::default();
    let rules_file = paths.workspace().join("alerts").join("rules.json");

    if !rules_file.exists() {
        println!("(No alert rules)");
        return Ok(());
    }

    let content = std::fs::read_to_string(&rules_file)?;
    let mut rules: Vec<Value> = serde_json::from_str(&content).unwrap_or_default();

    let before = rules.len();
    rules.retain(|r| {
        let id = r["id"].as_str().unwrap_or("");
        !id.starts_with(rule_id)
    });

    if rules.len() == before {
        println!("No matching rule found: {}", rule_id);
        return Ok(());
    }

    let removed = before - rules.len();
    let content = serde_json::to_string_pretty(&rules)?;
    std::fs::write(&rules_file, content)?;

    println!("✓ Removed {} alert rule(s)", removed);
    Ok(())
}
