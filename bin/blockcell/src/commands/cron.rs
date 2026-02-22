use chrono::{TimeZone, Utc};
use blockcell_core::Paths;
use blockcell_scheduler::{CronJob, CronService, JobPayload, JobSchedule, JobState, ScheduleKind};
use tokio::sync::mpsc;
use uuid::Uuid;

pub async fn list(show_all: bool) -> anyhow::Result<()> {
    let paths = Paths::new();
    let (tx, _rx) = mpsc::channel(1);
    let service = CronService::new(paths, tx);
    service.load().await?;

    let jobs = service.list_jobs().await;

    if jobs.is_empty() {
        println!("No cron jobs configured.");
        return Ok(());
    }

    println!("{:<8} {:<20} {:<10} {:<20} Schedule", "ID", "Name", "Enabled", "Next Run");
    println!("{}", "-".repeat(80));

    for job in jobs {
        if !show_all && !job.enabled {
            continue;
        }

        let next_run = job
            .state
            .next_run_at_ms
            .map(|ms| {
                Utc.timestamp_millis_opt(ms)
                    .single()
                    .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "invalid".to_string())
            })
            .unwrap_or_else(|| "-".to_string());

        let schedule = match job.schedule.kind {
            ScheduleKind::At => format!("at: {}", job.schedule.at_ms.unwrap_or(0)),
            ScheduleKind::Every => {
                let secs = job.schedule.every_ms.unwrap_or(0) / 1000;
                format!("every: {}s", secs)
            }
            ScheduleKind::Cron => format!("cron: {}", job.schedule.expr.as_deref().unwrap_or("-")),
        };

        println!(
            "{:<8} {:<20} {:<10} {:<20} {}",
            &job.id.chars().take(8).collect::<String>(),
            truncate(&job.name, 20),
            if job.enabled { "yes" } else { "no" },
            next_run,
            schedule
        );
    }

    Ok(())
}

pub async fn add(
    name: String,
    message: String,
    every: Option<u64>,
    cron_expr: Option<String>,
    at: Option<String>,
    deliver: bool,
    to: Option<String>,
    channel: Option<String>,
) -> anyhow::Result<()> {
    let paths = Paths::new();
    let (tx, _rx) = mpsc::channel(1);
    let service = CronService::new(paths, tx);
    service.load().await?;

    let now_ms = Utc::now().timestamp_millis();

    let schedule = if let Some(secs) = every {
        JobSchedule {
            kind: ScheduleKind::Every,
            at_ms: None,
            every_ms: Some((secs * 1000) as i64),
            expr: None,
            tz: None,
        }
    } else if let Some(expr) = cron_expr {
        JobSchedule {
            kind: ScheduleKind::Cron,
            at_ms: None,
            every_ms: None,
            expr: Some(expr),
            tz: None,
        }
    } else if let Some(at_str) = at {
        let at_time = chrono::DateTime::parse_from_rfc3339(&at_str)
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(now_ms);
        JobSchedule {
            kind: ScheduleKind::At,
            at_ms: Some(at_time),
            every_ms: None,
            expr: None,
            tz: None,
        }
    } else {
        anyhow::bail!("Must specify --every, --cron, or --at");
    };

    let job = CronJob {
        id: Uuid::new_v4().to_string(),
        name: name.clone(),
        enabled: true,
        schedule,
        payload: JobPayload {
            kind: "agent_turn".to_string(),
            message,
            deliver,
            channel,
            to,
            skill_name: None,
        },
        state: JobState::default(),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
        delete_after_run: false,
    };

    let job_id = job.id.clone();
    service.add_job(job).await?;

    println!("Created job: {} ({})", name, &job_id[..8]);
    Ok(())
}

pub async fn remove(job_id: &str) -> anyhow::Result<()> {
    let paths = Paths::new();
    let (tx, _rx) = mpsc::channel(1);
    let service = CronService::new(paths, tx);
    service.load().await?;

    // Find job by prefix
    let jobs = service.list_jobs().await;
    let matching: Vec<_> = jobs.iter().filter(|j| j.id.starts_with(job_id)).collect();

    match matching.len() {
        0 => {
            println!("No job found with ID starting with: {}", job_id);
        }
        1 => {
            let job = matching[0];
            service.remove_job(&job.id).await?;
            println!("Removed job: {} ({})", job.name, &job.id.chars().take(8).collect::<String>());
        }
        _ => {
            println!("Multiple jobs match '{}'. Be more specific:", job_id);
            for job in matching {
                println!("  {} - {}", &job.id.chars().take(8).collect::<String>(), job.name);
            }
        }
    }

    Ok(())
}

pub async fn enable(job_id: &str, enabled: bool) -> anyhow::Result<()> {
    let paths = Paths::new();
    let (tx, _rx) = mpsc::channel(1);
    let service = CronService::new(paths, tx);
    service.load().await?;

    // This is a simplified implementation
    // In a real implementation, we'd update the job in place
    println!(
        "Job {} would be {}",
        job_id,
        if enabled { "enabled" } else { "disabled" }
    );

    Ok(())
}

pub async fn run_job(job_id: &str, _force: bool) -> anyhow::Result<()> {
    let paths = Paths::new();
    let (tx, _rx) = mpsc::channel(1);
    let service = CronService::new(paths, tx);
    service.load().await?;

    let jobs = service.list_jobs().await;
    let matching: Vec<_> = jobs.iter().filter(|j| j.id.starts_with(job_id)).collect();

    match matching.len() {
        0 => {
            println!("No job found with ID starting with: {}", job_id);
        }
        1 => {
            let job = matching[0];
            println!("Running job: {} ({})", job.name, &job.id.chars().take(8).collect::<String>());
            println!("Message: {}", job.payload.message);
            // In a real implementation, this would trigger the job through the agent
        }
        _ => {
            println!("Multiple jobs match '{}'. Be more specific:", job_id);
            for job in matching {
                println!("  {} - {}", &job.id.chars().take(8).collect::<String>(), job.name);
            }
        }
    }

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
