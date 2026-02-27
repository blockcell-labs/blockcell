use blockcell_core::Paths;

/// List all background tasks from the task persistence file.
pub async fn list() -> anyhow::Result<()> {
    let paths = Paths::new();
    let tasks_file = paths.workspace().join("tasks.json");

    if !tasks_file.exists() {
        println!();
        println!("📋 No background tasks found.");
        println!("   Tasks are created when the agent spawns background work.");
        println!();
        return Ok(());
    }

    let content = std::fs::read_to_string(&tasks_file)?;
    let tasks: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap_or_default();

    if tasks.is_empty() {
        println!();
        println!("📋 No background tasks.");
        println!();
        return Ok(());
    }

    println!();
    println!("📋 Background Tasks ({} total)", tasks.len());
    println!();

    for task in &tasks {
        let id = task["id"].as_str().unwrap_or("?");
        let label = task["label"].as_str().unwrap_or("(no label)");
        let status = task["status"].as_str().unwrap_or("unknown");
        let created = task["created_at"].as_str().unwrap_or("");

        let icon = match status {
            "queued" => "⏳",
            "running" => "🔄",
            "completed" => "✅",
            "failed" => "❌",
            _ => "•",
        };

        let short_id: String = id.chars().take(12).collect();
        println!("  {} [{}] {} — {}", icon, short_id, status, label);

        if let Some(progress) = task["progress"].as_str() {
            if !progress.is_empty() {
                println!("     Progress: {}", progress);
            }
        }
        if let Some(result) = task["result"].as_str() {
            if !result.is_empty() {
                let preview: String = result.chars().take(120).collect();
                if result.chars().count() > 120 {
                    println!("     Result: {}...", preview);
                } else {
                    println!("     Result: {}", preview);
                }
            }
        }
        if let Some(err) = task["error"].as_str() {
            if !err.is_empty() {
                println!("     Error: {}", err);
            }
        }
        if !created.is_empty() {
            println!("     Created: {}", created);
        }
        println!();
    }

    Ok(())
}

/// Show details for a specific task by ID prefix.
pub async fn show(task_id: &str) -> anyhow::Result<()> {
    let paths = Paths::new();
    let tasks_file = paths.workspace().join("tasks.json");

    if !tasks_file.exists() {
        println!("No tasks found.");
        return Ok(());
    }

    let content = std::fs::read_to_string(&tasks_file)?;
    let tasks: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap_or_default();

    let matched: Vec<&serde_json::Value> = tasks
        .iter()
        .filter(|t| {
            t["id"]
                .as_str()
                .map(|id| id.starts_with(task_id))
                .unwrap_or(false)
        })
        .collect();

    if matched.is_empty() {
        println!("No task found with ID prefix: {}", task_id);
        return Ok(());
    }

    if matched.len() > 1 {
        println!("Ambiguous ID prefix '{}' matches {} tasks. Please be more specific:", task_id, matched.len());
        for t in matched {
            println!("  {}", t["id"].as_str().unwrap_or("?"));
        }
        return Ok(());
    }

    let task = matched[0];
    println!();
    println!("📋 Task Details");
    println!();
    println!("  ID:       {}", task["id"].as_str().unwrap_or("?"));
    println!("  Label:    {}", task["label"].as_str().unwrap_or("(no label)"));
    println!("  Status:   {}", task["status"].as_str().unwrap_or("unknown"));
    if let Some(v) = task["created_at"].as_str() {
        println!("  Created:  {}", v);
    }
    if let Some(v) = task["updated_at"].as_str() {
        println!("  Updated:  {}", v);
    }
    if let Some(v) = task["progress"].as_str() {
        if !v.is_empty() {
            println!("  Progress: {}", v);
        }
    }
    if let Some(v) = task["result"].as_str() {
        if !v.is_empty() {
            println!();
            println!("  Result:");
            println!("    {}", v.replace('\n', "\n    "));
        }
    }
    if let Some(v) = task["error"].as_str() {
        if !v.is_empty() {
            println!();
            println!("  Error: {}", v);
        }
    }
    println!();

    Ok(())
}

/// Cancel a task by ID prefix (marks it as cancelled in the file).
pub async fn cancel(task_id: &str) -> anyhow::Result<()> {
    let paths = Paths::new();
    let tasks_file = paths.workspace().join("tasks.json");

    if !tasks_file.exists() {
        println!("No tasks found.");
        return Ok(());
    }

    let content = std::fs::read_to_string(&tasks_file)?;
    let mut tasks: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap_or_default();

    let mut found = false;
    for task in tasks.iter_mut() {
        if task["id"]
            .as_str()
            .map(|id| id.starts_with(task_id))
            .unwrap_or(false)
        {
            let status = task["status"].as_str().unwrap_or("");
            if status == "completed" || status == "failed" || status == "cancelled" {
                println!(
                    "Task {} is already in terminal state: {}",
                    task["id"].as_str().unwrap_or("?"),
                    status
                );
                return Ok(());
            }
            task["status"] = serde_json::json!("cancelled");
            println!(
                "✅ Task {} marked as cancelled.",
                task["id"].as_str().unwrap_or("?")
            );
            found = true;
            break;
        }
    }

    if !found {
        println!("No task found with ID prefix: {}", task_id);
        return Ok(());
    }

    let updated = serde_json::to_string_pretty(&tasks)?;
    std::fs::write(&tasks_file, updated)?;
    println!("   Note: If the task is actively running in a gateway process, it may not stop immediately.");

    Ok(())
}
