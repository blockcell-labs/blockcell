use anyhow::Context;
use blockcell_core::{Config, Paths};
use serde_json::Value;

fn tasks_file(paths: &Paths) -> std::path::PathBuf {
    paths.workspace().join("tasks.json")
}

fn task_agent_id(task: &Value) -> &str {
    task.get("agent_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("default")
}

fn resolve_scope(paths: &Paths, requested_agent: Option<&str>, all: bool) -> anyhow::Result<Option<String>> {
    let requested_agent = requested_agent.map(str::trim).filter(|value| !value.is_empty());
    if all {
        if requested_agent.is_some() {
            anyhow::bail!("--all cannot be combined with --agent");
        }
        return Ok(None);
    }

    let agent_id = requested_agent.unwrap_or("default");
    let config = Config::load_or_default(paths)?;
    if !config.agent_exists(agent_id) {
        anyhow::bail!("Unknown agent '{}'", agent_id);
    }
    Ok(Some(agent_id.to_string()))
}

fn load_tasks(paths: &Paths) -> anyhow::Result<Vec<Value>> {
    let file = tasks_file(paths);
    if !file.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(&file)
        .with_context(|| format!("Failed to read {}", file.display()))?;
    Ok(serde_json::from_str(&content).unwrap_or_default())
}

fn filter_tasks(mut tasks: Vec<Value>, agent: Option<&str>, all: bool) -> anyhow::Result<Vec<Value>> {
    if all && agent.map(str::trim).filter(|value| !value.is_empty()).is_some() {
        anyhow::bail!("--all cannot be combined with --agent");
    }

    let agent = if all {
        None
    } else {
        Some(agent.map(str::trim).filter(|value| !value.is_empty()).unwrap_or("default"))
    };

    tasks.retain(|task| agent.map(|agent_id| task_agent_id(task) == agent_id).unwrap_or(true));
    tasks.sort_by(|left, right| {
        right
            .get("created_at")
            .and_then(|value| value.as_str())
            .cmp(&left.get("created_at").and_then(|value| value.as_str()))
    });
    Ok(tasks)
}

fn print_no_tasks(scope: Option<&str>) {
    println!();
    if let Some(agent_id) = scope {
        println!("📋 No background tasks for agent '{}'.", agent_id);
    } else {
        println!("📋 No background tasks found.");
        println!("   Tasks are created when the agent spawns background work.");
    }
    println!();
}

pub async fn list(agent: Option<&str>, all: bool) -> anyhow::Result<()> {
    let paths = Paths::new();
    let scope = resolve_scope(&paths, agent, all)?;
    let tasks = filter_tasks(load_tasks(&paths)?, scope.as_deref(), all)?;

    if tasks.is_empty() {
        print_no_tasks(scope.as_deref());
        return Ok(());
    }

    println!();
    match scope.as_deref() {
        None => println!("📋 Background Tasks ({} total, all agents)", tasks.len()),
        Some(agent_id) => println!("📋 Background Tasks for agent '{}' ({} total)", agent_id, tasks.len()),
    }
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
            "cancelled" => "⛔",
            _ => "•",
        };
        let short_id: String = id.chars().take(12).collect();
        println!("  {} [{}] {} — {}", icon, short_id, status, label);
        if scope.is_none() {
            println!("     Agent: {}", task_agent_id(task));
        }
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

pub async fn show(task_id: &str, agent: Option<&str>, all: bool) -> anyhow::Result<()> {
    let paths = Paths::new();
    let scope = resolve_scope(&paths, agent, all)?;
    let tasks = filter_tasks(load_tasks(&paths)?, scope.as_deref(), all)?;

    let matched: Vec<&Value> = tasks
        .iter()
        .filter(|task| task["id"].as_str().map(|id| id.starts_with(task_id)).unwrap_or(false))
        .collect();

    if matched.is_empty() {
        println!("No task found with ID prefix: {}", task_id);
        return Ok(());
    }

    if matched.len() > 1 {
        println!(
            "Ambiguous ID prefix '{}' matches {} tasks. Please be more specific:",
            task_id,
            matched.len()
        );
        for task in matched {
            println!("  {} ({})", task["id"].as_str().unwrap_or("?"), task_agent_id(task));
        }
        return Ok(());
    }

    let task = matched[0];
    println!();
    println!("📋 Task Details");
    println!();
    println!("  ID:       {}", task["id"].as_str().unwrap_or("?"));
    println!("  Agent:    {}", task_agent_id(task));
    println!("  Label:    {}", task["label"].as_str().unwrap_or("(no label)"));
    println!("  Status:   {}", task["status"].as_str().unwrap_or("unknown"));
    if let Some(value) = task["created_at"].as_str() {
        println!("  Created:  {}", value);
    }
    if let Some(value) = task["started_at"].as_str() {
        println!("  Started:  {}", value);
    }
    if let Some(value) = task["completed_at"].as_str() {
        println!("  Updated:  {}", value);
    }
    if let Some(value) = task["progress"].as_str() {
        if !value.is_empty() {
            println!("  Progress: {}", value);
        }
    }
    if let Some(value) = task["result"].as_str() {
        if !value.is_empty() {
            println!();
            println!("  Result:");
            println!("    {}", value.replace('\n', "\n    "));
        }
    }
    if let Some(value) = task["error"].as_str() {
        if !value.is_empty() {
            println!();
            println!("  Error: {}", value);
        }
    }
    println!();
    Ok(())
}

pub async fn cancel(task_id: &str, agent: Option<&str>, all: bool) -> anyhow::Result<()> {
    let paths = Paths::new();
    let scope = resolve_scope(&paths, agent, all)?;
    let mut tasks = load_tasks(&paths)?;

    let matching_indexes: Vec<usize> = tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| {
            task["id"].as_str().map(|id| id.starts_with(task_id)).unwrap_or(false)
                && scope
                    .as_deref()
                    .map(|agent_id| task_agent_id(task) == agent_id)
                    .unwrap_or(true)
        })
        .map(|(index, _)| index)
        .collect();

    if matching_indexes.is_empty() {
        println!("No task found with ID prefix: {}", task_id);
        return Ok(());
    }

    if matching_indexes.len() > 1 {
        println!(
            "Ambiguous ID prefix '{}' matches {} tasks. Please be more specific:",
            task_id,
            matching_indexes.len()
        );
        for index in matching_indexes {
            let task = &tasks[index];
            println!("  {} ({})", task["id"].as_str().unwrap_or("?"), task_agent_id(task));
        }
        return Ok(());
    }

    let task = &mut tasks[matching_indexes[0]];
    let status = task["status"].as_str().unwrap_or("");
    if matches!(status, "completed" | "failed" | "cancelled") {
        println!(
            "Task {} is already in terminal state: {}",
            task["id"].as_str().unwrap_or("?"),
            status
        );
        return Ok(());
    }

    task["status"] = serde_json::json!("cancelled");
    task["completed_at"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    println!(
        "✅ Task {} ({}) marked as cancelled.",
        task["id"].as_str().unwrap_or("?"),
        task_agent_id(task)
    );

    let updated = serde_json::to_string_pretty(&tasks)?;
    std::fs::write(tasks_file(&paths), updated)?;
    println!("   Note: If the task is actively running in a gateway process, it may not stop immediately.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, agent_id: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "label": format!("task-{id}"),
            "status": "queued",
            "created_at": "2026-03-06T00:00:00Z",
            "agent_id": agent_id,
        })
    }

    #[test]
    fn test_filter_tasks_for_default_agent_without_all() {
        let tasks = vec![task("t-default", Some("default")), task("t-ops", Some("ops"))];
        let filtered = filter_tasks(tasks, None, false).expect("filter should succeed");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0]["id"].as_str(), Some("t-default"));
    }

    #[test]
    fn test_filter_tasks_for_all_agents() {
        let tasks = vec![task("t-default", Some("default")), task("t-ops", Some("ops"))];
        let filtered = filter_tasks(tasks, None, true).expect("filter should succeed");
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_tasks_for_specific_agent() {
        let tasks = vec![task("t-default", Some("default")), task("t-ops", Some("ops"))];
        let filtered = filter_tasks(tasks, Some("ops"), false).expect("filter should succeed");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0]["id"].as_str(), Some("t-ops"));
    }
}
