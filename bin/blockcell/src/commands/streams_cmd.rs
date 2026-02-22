use blockcell_core::Paths;
use serde_json::Value;

/// List all stream subscriptions (from persisted rules).
pub async fn list() -> anyhow::Result<()> {
    let paths = Paths::default();
    let subs_file = paths.workspace().join("streams").join("subscriptions.json");

    if !subs_file.exists() {
        println!("(No active stream subscriptions)");
        return Ok(());
    }

    let content = std::fs::read_to_string(&subs_file)?;
    let rules: Vec<Value> = serde_json::from_str(&content).unwrap_or_default();

    if rules.is_empty() {
        println!("(No active stream subscriptions)");
        return Ok(());
    }

    println!();
    println!("📡 Stream subscriptions ({} total)", rules.len());
    println!();
    println!("  {:<10} {:<12} {:<40} Auto-restore", "ID", "Protocol", "URL");
    println!("  {}", "-".repeat(80));

    for rule in &rules {
        let id = rule["id"].as_str().unwrap_or("?");
        let protocol = rule["protocol"].as_str().unwrap_or("?");
        let url = rule["url"].as_str().unwrap_or("?");
        let auto_restore = rule["auto_restore"].as_bool().unwrap_or(false);

        let short_id_owned: String = id.chars().take(8).collect();
        let short_id = short_id_owned.as_str();
        let short_url: String = url.chars().take(38).collect();
        let url_ellipsis = if url.chars().count() > 38 { ".." } else { "" };

        println!(
            "  {:<10} {:<12} {:<40} {}",
            short_id,
            protocol,
            format!("{}{}", short_url, url_ellipsis),
            if auto_restore { "✓" } else { "✗" }
        );
    }
    println!();

    Ok(())
}

/// Show details for a specific subscription.
pub async fn status(sub_id: &str) -> anyhow::Result<()> {
    let paths = Paths::default();
    let subs_file = paths.workspace().join("streams").join("subscriptions.json");

    if !subs_file.exists() {
        println!("(No stream subscriptions)");
        return Ok(());
    }

    let content = std::fs::read_to_string(&subs_file)?;
    let rules: Vec<Value> = serde_json::from_str(&content).unwrap_or_default();

    let found = rules.iter().find(|r| {
        r["id"].as_str().is_some_and(|id| id.starts_with(sub_id))
    });

    match found {
        Some(rule) => {
            println!();
            println!("📡 Subscription details");
            println!("{}", serde_json::to_string_pretty(rule)?);
            println!();
        }
        None => {
            println!("No matching subscription found: {}", sub_id);
        }
    }

    Ok(())
}

/// Remove a subscription from the persisted rules.
pub async fn stop(sub_id: &str) -> anyhow::Result<()> {
    let paths = Paths::default();
    let subs_file = paths.workspace().join("streams").join("subscriptions.json");

    if !subs_file.exists() {
        println!("(No stream subscriptions)");
        return Ok(());
    }

    let content = std::fs::read_to_string(&subs_file)?;
    let mut rules: Vec<Value> = serde_json::from_str(&content).unwrap_or_default();

    let before = rules.len();
    rules.retain(|r| {
        let id = r["id"].as_str().unwrap_or("");
        !id.starts_with(sub_id)
    });

    if rules.len() == before {
        println!("No matching subscription found: {}", sub_id);
        return Ok(());
    }

    let removed = before - rules.len();
    let content = serde_json::to_string_pretty(&rules)?;
    std::fs::write(&subs_file, content)?;

    println!("✓ Removed {} subscription(s) (note: active connections require agent restart to disconnect)", removed);
    Ok(())
}

/// Restore all persisted subscriptions.
pub async fn restore() -> anyhow::Result<()> {
    let paths = Paths::default();
    let subs_file = paths.workspace().join("streams").join("subscriptions.json");

    if !subs_file.exists() {
        println!("(No persisted subscription rules)");
        return Ok(());
    }

    let content = std::fs::read_to_string(&subs_file)?;
    let rules: Vec<Value> = serde_json::from_str(&content).unwrap_or_default();

    let restorable: Vec<&Value> = rules.iter()
        .filter(|r| r["auto_restore"].as_bool().unwrap_or(false))
        .collect();

    if restorable.is_empty() {
        println!("No subscriptions marked for auto-restore.");
        return Ok(());
    }

    println!("📡 Restorable subscriptions ({}):", restorable.len());
    for rule in &restorable {
        let id = rule["id"].as_str().unwrap_or("?");
        let url = rule["url"].as_str().unwrap_or("?");
        let short_id_owned: String = id.chars().take(8).collect();
        println!("  {} — {}", short_id_owned, url);
    }
    println!();
    println!("Note: Subscriptions are auto-restored when the agent starts.");
    println!("Run `blockcell agent` to restore them.");

    Ok(())
}
