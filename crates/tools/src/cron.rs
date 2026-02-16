use async_trait::async_trait;
use blockcell_core::{Error, Paths, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{Tool, ToolContext, ToolSchema};

pub struct CronTool;

#[derive(Debug, Serialize, Deserialize)]
struct JobStore {
    version: u32,
    jobs: Vec<Value>,
}

impl Default for JobStore {
    fn default() -> Self {
        Self {
            version: 1,
            jobs: Vec::new(),
        }
    }
}

fn load_store(paths: &Paths) -> Result<JobStore> {
    let path = paths.cron_jobs_file();
    if !path.exists() {
        return Ok(JobStore::default());
    }
    let content = std::fs::read_to_string(&path)?;
    let store: JobStore = serde_json::from_str(&content)?;
    Ok(store)
}

fn save_store(paths: &Paths, store: &JobStore) -> Result<()> {
    let path = paths.cron_jobs_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(store)?;
    std::fs::write(&path, content)?;
    Ok(())
}

#[async_trait]
impl Tool for CronTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "cron",
            description: "Manage scheduled tasks (cron jobs). Use this to set reminders, schedule recurring tasks, or run one-time delayed tasks. The agent's CronService will pick up changes automatically.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["add", "list", "remove"],
                        "description": "Action to perform: add a new job, list existing jobs, or remove a job"
                    },
                    "name": {
                        "type": "string",
                        "description": "(add) Human-readable name for the job, e.g. '吃早饭提醒'"
                    },
                    "message": {
                        "type": "string",
                        "description": "(add) The message/prompt that will be sent to the agent when the job fires"
                    },
                    "at_ms": {
                        "type": "integer",
                        "description": "(add) Unix timestamp in milliseconds for a one-time job. Use this for reminders at a specific time."
                    },
                    "delay_seconds": {
                        "type": "integer",
                        "description": "(add) Delay in seconds from now for a one-time job. E.g. 300 for 5 minutes. Alternative to at_ms."
                    },
                    "every_seconds": {
                        "type": "integer",
                        "description": "(add) Interval in seconds for a recurring job. E.g. 3600 for every hour."
                    },
                    "cron_expr": {
                        "type": "string",
                        "description": "(add) Cron expression for complex schedules, e.g. '0 30 9 * * Mon-Fri' for weekdays at 9:30"
                    },
                    "delete_after_run": {
                        "type": "boolean",
                        "description": "(add) If true, the job will be deleted after it runs once. Default false (job is disabled instead)."
                    },
                    "job_id": {
                        "type": "string",
                        "description": "(remove) The job ID (or prefix) to remove"
                    },
                    "skill_name": {
                        "type": "string",
                        "description": "(add) If set, the cron job will directly execute the named skill's SKILL.rhai script instead of going through the LLM. E.g. 'stock_monitor', 'daily_finance_report'."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: action".to_string()))?;

        match action {
            "add" => {
                if params.get("name").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation("Missing required parameter for add: name".to_string()));
                }
                if params.get("message").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation("Missing required parameter for add: message".to_string()));
                }
                let has_schedule = params.get("at_ms").is_some()
                    || params.get("delay_seconds").is_some()
                    || params.get("every_seconds").is_some()
                    || params.get("cron_expr").is_some();
                if !has_schedule {
                    return Err(Error::Validation(
                        "Must specify one of: at_ms, delay_seconds, every_seconds, or cron_expr".to_string(),
                    ));
                }
            }
            "list" => {}
            "remove" => {
                if params.get("job_id").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation("Missing required parameter for remove: job_id".to_string()));
                }
            }
            _ => {
                return Err(Error::Validation(format!("Unknown action: {}", action)));
            }
        }

        Ok(())
    }

    async fn execute(&self, _ctx: ToolContext, params: Value) -> Result<Value> {
        let paths = Paths::new();
        let action = params["action"].as_str().unwrap();

        match action {
            "add" => {
                let mut store = load_store(&paths)?;
                let now_ms = Utc::now().timestamp_millis();
                let name = params["name"].as_str().unwrap();
                let message = params["message"].as_str().unwrap();
                let delete_after_run = params.get("delete_after_run").and_then(|v| v.as_bool()).unwrap_or(false);

                let (kind, schedule) = if let Some(delay) = params.get("delay_seconds").and_then(|v| v.as_i64()) {
                    let at_ms = now_ms + delay * 1000;
                    ("at", json!({
                        "kind": "at",
                        "atMs": at_ms
                    }))
                } else if let Some(at_ms) = params.get("at_ms").and_then(|v| v.as_i64()) {
                    ("at", json!({
                        "kind": "at",
                        "atMs": at_ms
                    }))
                } else if let Some(every) = params.get("every_seconds").and_then(|v| v.as_i64()) {
                    ("every", json!({
                        "kind": "every",
                        "everyMs": every * 1000
                    }))
                } else if let Some(expr) = params.get("cron_expr").and_then(|v| v.as_str()) {
                    ("cron", json!({
                        "kind": "cron",
                        "expr": expr
                    }))
                } else {
                    return Err(Error::Validation("No schedule specified".to_string()));
                };

                let job_id = Uuid::new_v4().to_string();

                let skill_name = params.get("skill_name").and_then(|v| v.as_str());
                let payload_kind = if skill_name.is_some() { "skill_rhai" } else { "agent_turn" };

                let mut payload = json!({
                    "kind": payload_kind,
                    "message": message,
                    "deliver": false
                });
                if let Some(sn) = skill_name {
                    payload["skillName"] = json!(sn);
                }

                let job = json!({
                    "id": job_id,
                    "name": name,
                    "enabled": true,
                    "schedule": schedule,
                    "payload": payload,
                    "state": {},
                    "createdAtMs": now_ms,
                    "updatedAtMs": now_ms,
                    "deleteAfterRun": delete_after_run
                });

                store.jobs.push(job);
                save_store(&paths, &store)?;

                let schedule_desc = match kind {
                    "at" => {
                        if let Some(delay) = params.get("delay_seconds").and_then(|v| v.as_i64()) {
                            format!("{}秒后执行", delay)
                        } else {
                            "指定时间执行".to_string()
                        }
                    }
                    "every" => {
                        let secs = params.get("every_seconds").and_then(|v| v.as_i64()).unwrap_or(0);
                        format!("每{}秒执行", secs)
                    }
                    "cron" => {
                        let expr = params.get("cron_expr").and_then(|v| v.as_str()).unwrap_or("");
                        format!("cron: {}", expr)
                    }
                    _ => "unknown".to_string(),
                };

                Ok(json!({
                    "status": "created",
                    "job_id": job_id,
                    "name": name,
                    "schedule": schedule_desc,
                    "message": message
                }))
            }
            "list" => {
                let store = load_store(&paths)?;
                let jobs: Vec<Value> = store.jobs.iter().map(|j| {
                    json!({
                        "id": j.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "name": j.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "enabled": j.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false),
                        "schedule": j.get("schedule"),
                        "state": j.get("state"),
                    })
                }).collect();

                Ok(json!({
                    "jobs": jobs,
                    "count": jobs.len()
                }))
            }
            "remove" => {
                let mut store = load_store(&paths)?;
                let job_id = params["job_id"].as_str().unwrap();

                let before = store.jobs.len();
                store.jobs.retain(|j| {
                    let id = j.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    !id.starts_with(job_id)
                });
                let removed = before - store.jobs.len();

                if removed > 0 {
                    save_store(&paths, &store)?;
                }

                Ok(json!({
                    "removed": removed,
                    "job_id_prefix": job_id
                }))
            }
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_cron_schema() {
        let tool = CronTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "cron");
    }

    #[test]
    fn test_cron_validate_add() {
        let tool = CronTool;
        assert!(tool.validate(&json!({
            "action": "add", "name": "test", "message": "hi", "delay_seconds": 60
        })).is_ok());
    }

    #[test]
    fn test_cron_validate_add_missing_schedule() {
        let tool = CronTool;
        assert!(tool.validate(&json!({
            "action": "add", "name": "test", "message": "hi"
        })).is_err());
    }

    #[test]
    fn test_cron_validate_add_missing_name() {
        let tool = CronTool;
        assert!(tool.validate(&json!({
            "action": "add", "message": "hi", "delay_seconds": 60
        })).is_err());
    }

    #[test]
    fn test_cron_validate_list() {
        let tool = CronTool;
        assert!(tool.validate(&json!({"action": "list"})).is_ok());
    }

    #[test]
    fn test_cron_validate_remove() {
        let tool = CronTool;
        assert!(tool.validate(&json!({"action": "remove", "job_id": "abc"})).is_ok());
        assert!(tool.validate(&json!({"action": "remove"})).is_err());
    }

    #[test]
    fn test_cron_validate_unknown_action() {
        let tool = CronTool;
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }
}
