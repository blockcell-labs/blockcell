use blockcell_core::Paths;
use blockcell_scheduler::{CronService, ScheduleKind};
use chrono::{TimeZone, Utc};
use tokio::sync::mpsc;

/// List cron jobs for a given agent (read-only, reads from disk).
/// agent_id: agent to query; empty string or "default" uses the default agent path.
pub async fn list(show_all: bool, agent_id: &str) -> anyhow::Result<()> {
    let paths = Paths::new().for_agent(agent_id);
    let (tx, _rx) = mpsc::channel(1);
    let service = CronService::new(paths, tx);
    service.load().await?;

    let jobs = service.list_jobs().await;

    if jobs.is_empty() {
        if agent_id.is_empty() || agent_id == "default" {
            println!("No cron jobs configured.");
        } else {
            println!("No cron jobs for agent '{}'.", agent_id);
        }
        return Ok(());
    }

    if agent_id != "default" && !agent_id.is_empty() {
        println!("Agent: {}", agent_id);
    }
    println!(
        "{:<8} {:<22} {:<8} {:<18} {}",
        "ID", "Name", "Enabled", "Next Run", "Schedule"
    );
    println!("{}", "-".repeat(80));

    for job in &jobs {
        if !show_all && !job.enabled {
            continue;
        }

        let next_run = job
            .state
            .next_run_at_ms
            .map(|ms| {
                Utc.timestamp_millis_opt(ms)
                    .single()
                    .map(|dt| dt.format("%m-%d %H:%M:%S").to_string())
                    .unwrap_or_else(|| "invalid".to_string())
            })
            .unwrap_or_else(|| "-".to_string());

        let schedule = match job.schedule.kind {
            ScheduleKind::At => {
                let ms = job.schedule.at_ms.unwrap_or(0);
                let dt = Utc
                    .timestamp_millis_opt(ms)
                    .single()
                    .map(|dt| dt.format("%m-%d %H:%M:%S").to_string())
                    .unwrap_or_else(|| ms.to_string());
                format!("at {}", dt)
            }
            ScheduleKind::Every => {
                let secs = job.schedule.every_ms.unwrap_or(0) / 1000;
                format!("every {}s", secs)
            }
            ScheduleKind::Cron => {
                format!("cron: {}", job.schedule.expr.as_deref().unwrap_or("-"))
            }
        };

        println!(
            "{:<8} {:<22} {:<8} {:<18} {}",
            &job.id.chars().take(8).collect::<String>(),
            truncate(&job.name, 22),
            if job.enabled { "yes" } else { "no" },
            next_run,
            schedule
        );
    }

    println!("\nTotal: {} job(s)", jobs.iter().filter(|j| show_all || j.enabled).count());
    Ok(())
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}
