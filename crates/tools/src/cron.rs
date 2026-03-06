use async_trait::async_trait;
use blockcell_core::{Error, Paths, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{Tool, ToolContext, ToolSchema};

pub struct CronTool;

fn resolve_skill_payload_kind(paths: &Paths, skill_name: Option<&str>) -> &'static str {
    let Some(skill_name) = skill_name else {
        return "agent_turn";
    };

    let user_dir = paths.skills_dir().join(skill_name);
    let builtin_dir = paths.builtin_skills_dir().join(skill_name);

    let has_rhai = user_dir.join("SKILL.rhai").exists() || builtin_dir.join("SKILL.rhai").exists();
    let has_py = user_dir.join("SKILL.py").exists() || builtin_dir.join("SKILL.py").exists();

    if has_rhai {
        "skill_rhai"
    } else if has_py {
        "skill_python"
    } else {
        // Backward compatibility: keep legacy behavior when skill script type is unknown.
        "skill_rhai"
    }
}

fn execute_cron_action(
    action: &str,
    params: &Value,
    origin_channel: &str,
    origin_chat_id: &str,
) -> Result<Value> {
    let paths = Paths::new();
    execute_cron_action_with_paths(&paths, action, params, origin_channel, origin_chat_id)
}

fn execute_cron_action_with_paths(
    paths: &Paths,
    action: &str,
    params: &Value,
    origin_channel: &str,
    origin_chat_id: &str,
) -> Result<Value> {
    match action {
        "add" => {
            let mut store = load_store(paths)?;
            let now_ms = Utc::now().timestamp_millis();
            let name = params["name"].as_str().unwrap();
            let message = params["message"].as_str().unwrap();
            let delete_after_run = params
                .get("delete_after_run")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let (kind, schedule) =
                if let Some(delay) = params.get("delay_seconds").and_then(|v| v.as_i64()) {
                    let at_ms = now_ms + delay * 1000;
                    (
                        "at",
                        json!({
                            "kind": "at",
                            "atMs": at_ms
                        }),
                    )
                } else if let Some(at_ms) = params.get("at_ms").and_then(|v| v.as_i64()) {
                    (
                        "at",
                        json!({
                            "kind": "at",
                            "atMs": at_ms
                        }),
                    )
                } else if let Some(every) = params.get("every_seconds").and_then(|v| v.as_i64()) {
                    (
                        "every",
                        json!({
                            "kind": "every",
                            "everyMs": every * 1000
                        }),
                    )
                } else if let Some(expr) = params.get("cron_expr").and_then(|v| v.as_str()) {
                    (
                        "cron",
                        json!({
                            "kind": "cron",
                            "expr": expr
                        }),
                    )
                } else {
                    return Err(Error::Validation("No schedule specified".to_string()));
                };

            let job_id = Uuid::new_v4().to_string();

            let skill_name = params.get("skill_name").and_then(|v| v.as_str());
            let payload_kind = resolve_skill_payload_kind(paths, skill_name);

            let deliver = !matches!(origin_channel, "cli" | "cron" | "ghost" | "");
            let mut payload = json!({
                "kind": payload_kind,
                "message": message,
                "deliver": deliver,
                "channel": origin_channel,
                "to": origin_chat_id
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
            save_store(paths, &store)?;

            let schedule_desc = match kind {
                "at" => {
                    if let Some(delay) = params.get("delay_seconds").and_then(|v| v.as_i64()) {
                        format!("{}秒后执行", delay)
                    } else {
                        "指定时间执行".to_string()
                    }
                }
                "every" => {
                    let secs = params
                        .get("every_seconds")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    format!("每{}秒执行", secs)
                }
                "cron" => {
                    let expr = params
                        .get("cron_expr")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
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
            let store = load_store(paths)?;
            let jobs: Vec<Value> = store
                .jobs
                .iter()
                .map(|j| {
                    json!({
                        "id": j.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "name": j.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "enabled": j.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false),
                        "schedule": j.get("schedule"),
                        "state": j.get("state"),
                    })
                })
                .collect();

            Ok(json!({
                "jobs": jobs,
                "count": jobs.len()
            }))
        }
        "remove" => {
            let mut store = load_store(paths)?;
            let job_id = params["job_id"].as_str().unwrap();

            let before = store.jobs.len();
            store.jobs.retain(|j| {
                let id = j.get("id").and_then(|v| v.as_str()).unwrap_or("");
                !id.starts_with(job_id)
            });
            let removed = before - store.jobs.len();

            if removed > 0 {
                save_store(paths, &store)?;
            }

            Ok(json!({
                "removed": removed,
                "job_id_prefix": job_id
            }))
        }
        _ => Err(Error::Tool(format!("Unknown action: {}", action))),
    }
}

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
    let store: JobStore = match serde_json::from_str(&content) {
        Ok(store) => store,
        Err(_) => JobStore::default(),
    };
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
                        "description": "(add) If set, the cron job will directly execute the named skill script (SKILL.rhai or SKILL.py) instead of going through the LLM. E.g. 'stock_monitor', 'daily_finance_report'."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- **定时任务 (cron)**: 用户要求定时执行某项任务时，**优先**检查是否有对应技能：先调用 `list_skills` 查看可用技能列表，若有名称匹配的技能（如用户说 AI新闻 -> 技能名 `ai_news`），则在 `cron` 工具中设置 `skill_name='ai_news'`，触发时直接执行技能脚本，无需 LLM 介入，最可靠。若无匹配技能，则用 `message` 参数描述任务指令。 [TIMEZONE] `cron_expr` 使用 UTC 时间，中国用户（UTC+8）说每天 9 点应填 `cron_expr='0 0 1 * * *'`（UTC 1:00 = 北京时间 9:00）。一次性任务设 `delete_after_run=true`；周期任务用 `cron_expr` 或 `every_seconds`。".to_string())
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: action".to_string()))?;

        match action {
            "add" => {
                if params.get("name").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation(
                        "Missing required parameter for add: name".to_string(),
                    ));
                }
                if params.get("message").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation(
                        "Missing required parameter for add: message".to_string(),
                    ));
                }
                let has_schedule = params.get("at_ms").is_some()
                    || params.get("delay_seconds").is_some()
                    || params.get("every_seconds").is_some()
                    || params.get("cron_expr").is_some();
                if !has_schedule {
                    return Err(Error::Validation(
                        "Must specify one of: at_ms, delay_seconds, every_seconds, or cron_expr"
                            .to_string(),
                    ));
                }
            }
            "list" => {}
            "remove" => {
                if params.get("job_id").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation(
                        "Missing required parameter for remove: job_id".to_string(),
                    ));
                }
            }
            _ => {
                return Err(Error::Validation(format!("Unknown action: {}", action)));
            }
        }

        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap().to_string();
        let origin_channel = ctx.channel.clone();
        let origin_chat_id = ctx.chat_id.clone();
        tokio::task::spawn_blocking(move || {
            execute_cron_action(&action, &params, &origin_channel, &origin_chat_id)
        })
        .await
        .map_err(|e| Error::Tool(format!("Cron task failed: {}", e)))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_paths(tag: &str) -> Paths {
        let mut root = std::env::temp_dir();
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        root.push(format!(
            "blockcell_cron_tool_{}_{}_{}",
            tag,
            std::process::id(),
            now_ns
        ));
        std::fs::create_dir_all(&root).expect("create temp cron dir");
        Paths::with_base(root)
    }

    #[test]
    fn test_cron_schema() {
        let tool = CronTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "cron");
    }

    #[test]
    fn test_cron_validate_add() {
        let tool = CronTool;
        assert!(tool
            .validate(&json!({
                "action": "add", "name": "test", "message": "hi", "delay_seconds": 60
            }))
            .is_ok());
        // Verify execute_cron_action accepts origin params
        let paths = temp_paths("add");
        let r = execute_cron_action_with_paths(
            &paths,
            "add",
            &json!({
                "name": "t", "message": "m", "delay_seconds": 60
            }),
            "telegram",
            "12345",
        );
        assert!(r.is_ok(), "unexpected error: {:?}", r.err());
        let _ = std::fs::remove_dir_all(paths.base);
    }

    #[test]
    fn test_cron_validate_add_missing_schedule() {
        let tool = CronTool;
        assert!(tool
            .validate(&json!({
                "action": "add", "name": "test", "message": "hi"
            }))
            .is_err());
    }

    #[test]
    fn test_cron_validate_add_missing_name() {
        let tool = CronTool;
        assert!(tool
            .validate(&json!({
                "action": "add", "message": "hi", "delay_seconds": 60
            }))
            .is_err());
    }

    #[test]
    fn test_cron_validate_list() {
        let tool = CronTool;
        assert!(tool.validate(&json!({"action": "list"})).is_ok());
    }

    #[test]
    fn test_cron_validate_remove() {
        let tool = CronTool;
        assert!(tool
            .validate(&json!({"action": "remove", "job_id": "abc"}))
            .is_ok());
        assert!(tool.validate(&json!({"action": "remove"})).is_err());
    }

    #[test]
    fn test_cron_validate_unknown_action() {
        let tool = CronTool;
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }
}
