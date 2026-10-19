use async_trait::async_trait;
use blockcell_core::file_store::{atomic_write, ExclusiveFileLock};
use blockcell_core::{Error, Paths, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{Tool, ToolContext, ToolSchema};

pub struct CronTool;

fn resolve_skill_payload_kind(paths: &Paths, skill_name: Option<&str>) -> &'static str {
    let Some(skill_name) = skill_name else {
        return "rhai";
    };

    let user_dir = paths.skills_dir().join(skill_name);
    let builtin_dir = paths.builtin_skills_dir().join(skill_name);

    let has_rhai = user_dir.join("SKILL.rhai").exists() || builtin_dir.join("SKILL.rhai").exists();
    let has_py = user_dir.join("SKILL.py").exists() || builtin_dir.join("SKILL.py").exists();

    if has_rhai {
        "rhai"
    } else if has_py {
        "python"
    } else {
        "rhai"
    }
}

fn execute_cron_action_with_paths(
    paths: &Paths,
    action: &str,
    params: &Value,
    origin_channel: &str,
    origin_chat_id: &str,
    default_timezone: Option<&str>,
) -> Result<Value> {
    let _store_lock = if matches!(action, "add" | "remove") {
        Some(ExclusiveFileLock::acquire(&paths.cron_jobs_lock_file())?)
    } else {
        None
    };
    match action {
        "add" => {
            let mut store = load_store(paths)?;
            let now_ms = Utc::now().timestamp_millis();
            let name = params.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                Error::Validation("Missing or invalid 'name' parameter".to_string())
            })?;
            let message = params
                .get("message")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    Error::Validation("Missing or invalid 'message' parameter".to_string())
                })?;
            let delete_after_run = params
                .get("delete_after_run")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let (kind, schedule) = if let Some(delay) =
                params.get("delay_seconds").and_then(|v| v.as_i64())
            {
                if delay <= 0 {
                    return Err(Error::Validation(
                        "delay_seconds must be a positive integer".to_string(),
                    ));
                }
                let delay_ms = delay
                    .checked_mul(1000)
                    .ok_or_else(|| Error::Validation("delay_seconds is too large".to_string()))?;
                let at_ms = now_ms.checked_add(delay_ms).ok_or_else(|| {
                    Error::Validation("delay_seconds produces an invalid timestamp".to_string())
                })?;
                (
                    "at",
                    json!({
                        "kind": "at",
                        "atMs": at_ms
                    }),
                )
            } else if let Some(at_ms) = params.get("at_ms").and_then(|v| v.as_i64()) {
                // Validate at_ms: detect if it's likely a seconds timestamp (wrong unit)
                // A milliseconds timestamp for year 2020-2030 should be in range 1577836800000 - 1893456000000
                // If the value is less than 1e12 (1000000000000), it's likely in seconds, not milliseconds
                if at_ms > 0 && at_ms < 1_000_000_000_000 {
                    return Err(Error::Validation(format!(
                            "at_ms value {} appears to be in seconds, not milliseconds. Multiply by 1000 or use delay_seconds instead.",
                            at_ms
                        )));
                }
                (
                    "at",
                    json!({
                        "kind": "at",
                        "atMs": at_ms
                    }),
                )
            } else if let Some(every) = params.get("every_seconds").and_then(|v| v.as_i64()) {
                if every <= 0 {
                    return Err(Error::Validation(
                        "every_seconds must be a positive integer".to_string(),
                    ));
                }
                let every_ms = every.checked_mul(1000).ok_or_else(|| {
                    Error::Validation("every_seconds is too large".to_string())
                })?;
                let run_immediately = params
                    .get("run_immediately")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                (
                    "every",
                    json!({
                        "kind": "every",
                        "everyMs": every_ms,
                        "runImmediately": run_immediately
                    }),
                )
            } else if let Some(expr) = params.get("cron_expr").and_then(|v| v.as_str()) {
                // Validate cron expression
                if expr.parse::<cron::Schedule>().is_err() {
                    return Err(Error::Validation(format!(
                            "Invalid cron expression '{}'. Use 6-field format: 'sec min hour day month weekday'",
                            expr
                        )));
                }
                // Use user-specified tz if provided, otherwise fall back to default_timezone
                let tz = params
                    .get("tz")
                    .and_then(|v| v.as_str())
                    .or(default_timezone);
                // Validate timezone if provided
                if let Some(tz_str) = tz {
                    if tz_str.parse::<chrono_tz::Tz>().is_err() {
                        return Err(Error::Validation(format!(
                                "Invalid timezone '{}'. Use IANA timezone like 'Asia/Shanghai', 'America/New_York'",
                                tz_str
                            )));
                    }
                }
                let mut schedule_json = json!({
                    "kind": "cron",
                    "expr": expr
                });
                if let Some(tz) = tz {
                    schedule_json["tz"] = json!(tz);
                }
                ("cron", schedule_json)
            } else {
                return Err(Error::Validation("No schedule specified".to_string()));
            };

            let job_id = Uuid::new_v4().to_string();

            let mode = params.get("mode").and_then(|v| v.as_str());
            let skill_name = params.get("skill_name").and_then(|v| v.as_str());
            let payload_kind = match mode {
                Some("agent") => "agent",
                Some("script") => "script",
                Some("reminder") => "reminder",
                Some(_) | None => {
                    if skill_name.is_some() {
                        "script"
                    } else {
                        "reminder"
                    }
                }
            };
            let script_kind = if payload_kind == "script" {
                skill_name.map(|sn| resolve_skill_payload_kind(paths, Some(sn)))
            } else {
                None
            };

            let deliver = !matches!(origin_channel, "cli" | "cron" | "ghost" | "");
            let mut payload = json!({
                "kind": payload_kind,
                "message": message,
                "deliver": deliver,
                "channel": origin_channel,
                "to": origin_chat_id
            });
            if let Some(kind) = script_kind {
                payload["scriptKind"] = json!(kind);
            }
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

            // Don't return 'message' to prevent LLM from echoing/simulating the reminder content
            Ok(json!({
                "status": "created",
                "job_id": job_id,
                "name": name,
                "schedule": schedule_desc
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
            let job_id = params
                .get("job_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    Error::Validation("Missing or invalid 'job_id' parameter".to_string())
                })?;

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
    let store: JobStore = serde_json::from_str(&content)?;
    Ok(store)
}

fn save_store(paths: &Paths, store: &JobStore) -> Result<()> {
    let path = paths.cron_jobs_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(store)?;
    atomic_write(&path, content.as_bytes())?;
    Ok(())
}

#[async_trait]
impl Tool for CronTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "cron".to_string(),
            description: "Manage scheduled tasks (cron jobs). You MUST provide `action`. action='add': requires `name` + `message` and exactly one schedule field from `delay_seconds`, `at_ms`, `every_seconds`, or `cron_expr`; optional `delete_after_run`, `mode`, and `skill_name`. action='list': no extra params. action='remove': requires `job_id`.".to_string(),
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
                        "description": "(add) Unix timestamp in **milliseconds** for a one-time job. MUST be milliseconds (e.g., 1743329100000), NOT seconds. For relative times, prefer delay_seconds which is easier to use."
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
                        "description": "(add) Cron expression for complex schedules, e.g. '0 30 9 * * Mon-Fri' for weekdays at 9:30. Use 6-field format: 'sec min hour day month weekday'. Time uses the user's configured timezone by default."
                    },
                    "tz": {
                        "type": "string",
                        "description": "(add) Optional. Override timezone for this job, e.g. 'America/New_York'. If not specified, uses the user's configured default timezone. Only use when user explicitly mentions a specific timezone."
                    },
                    "run_immediately": {
                        "type": "boolean",
                        "description": "(add) For every_seconds jobs. If true, execute immediately when created instead of waiting for the first interval. Default false."
                    },
                    "delete_after_run": {
                        "type": "boolean",
                        "description": "(add) If true, the job will be deleted after it runs once. Default false (job is disabled instead)."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["reminder", "script", "agent"],
                        "description": "(add) Optional execution mode. `reminder` sends fixed text directly. `script` routes the job into the named skill via the normal skill runtime and requires `skill_name`. `agent` sends the message into the normal agent LLM/tool loop so it can call tools like web_search. If omitted, defaults to `script` when `skill_name` is provided, otherwise `reminder`."
                    },
                    "job_id": {
                        "type": "string",
                        "description": "(remove) The job ID (or prefix) to remove"
                    },
                    "skill_name": {
                        "type": "string",
                        "description": "(add) Optional. Used with `mode='script'` to route execution into the named skill. The skill then follows the current skill runtime path (for example prompt skill execution, and possibly exec_local inside the skill scope). E.g. 'stock_monitor', 'daily_finance_report'."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn prompt_rule(&self, ctx: &crate::PromptContext) -> Option<String> {
        let tz_hint = match ctx.default_timezone {
            Some(tz) => format!("默认时区: `{}`", tz),
            None => "默认时区: UTC".to_string(),
        };

        Some(format!(
            "- **定时任务 (cron)**: 用户要求定时执行某项任务时，先判断执行模式。**纯提醒**（如起床提醒、喝水提醒）用 `mode='reminder'`，`message` 直接写最终发给用户的话，不要写成待分析任务；触发时会直接发送，不经过 LLM。若任务需要定时进入某个已安装技能，先调用 `list_skills` 查找技能，再用 `mode='script'` 并设置 `skill_name='...'`；这里的 `script` 是把任务路由进该技能的当前运行时，而不是简单按文件类型直接跑 `SKILL.rhai` / `SKILL.py`。若任务本身就是一段需要模型理解、调用工具、再整理结果的指令（如定时搜索新闻、抓取网页、汇总情报），用 `mode='agent'`，这样触发时会进入正常 agent LLM/tool loop。`mode` 省略时：有 `skill_name` 默认 `script`，否则默认 `reminder`。**一次性任务优先用 `delay_seconds`**（如5分钟后用 `delay_seconds=300`），避免 `at_ms` 单位错误。{}。只有用户**明确**指定特定时区时才需设置 `tz` 参数（如'纽约时间9点'、'东京时间'），自动转换为 IANA 名称（如 `America/New_York`、`Asia/Tokyo`）。一次性任务设 `delete_after_run=true`；周期任务用 `cron_expr` 或 `every_seconds`。`every_seconds` 任务默认等待一个周期后首次执行，若需立即执行设 `run_immediately=true`。**⚠️ 设置成功后用温暖的语气简洁确认，格式示例：'好的，已为你设置「任务名」，每天 19:29（东八区）会准时提醒你。'。注意括号内的时区根据实际使用的时区填写（如用户指定了东京时间则写东九区）。禁止输出⏰符号、禁止模拟提醒消息内容、禁止写'时间到啦'之类的话。提醒消息由系统自动发送，你只需确认即可。**",
            tz_hint
        ))
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
                let mode = params.get("mode").and_then(|v| v.as_str());
                match mode {
                    Some("reminder") | Some("script") | Some("agent") | None => {}
                    Some(other) => {
                        return Err(Error::Validation(format!(
                            "Invalid mode for add: {}",
                            other
                        )));
                    }
                }
                if matches!(mode, Some("script"))
                    && params.get("skill_name").and_then(|v| v.as_str()).is_none()
                {
                    return Err(Error::Validation(
                        "mode='script' requires skill_name".to_string(),
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
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: action".to_string()))?
            .to_string();
        let origin_channel = ctx.channel.clone();
        let origin_chat_id = ctx.chat_id.clone();
        let default_timezone = ctx.config.default_timezone.clone();
        // 使用 base + workspace 重建 Paths，保留 workspace override。
        // 若只用 with_base，自定义 workspace 时 skills_dir() 会指向错误的默认路径，
        // 导致 resolve_skill_payload_kind 误判 Python 技能为 rhai。
        let paths = Paths::with_base_and_workspace(ctx.base.clone(), ctx.workspace.clone());
        tokio::task::spawn_blocking(move || {
            execute_cron_action_with_paths(
                &paths,
                &action,
                &params,
                &origin_channel,
                &origin_chat_id,
                default_timezone.as_deref(),
            )
        })
        .await
        .map_err(|e| Error::Tool(format!("Cron task failed: {}", e)))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p3_rejects_overflowing_interval() {
        let paths = temp_paths("interval_overflow");
        let result = execute_cron_action_with_paths(
            &paths, "add",
            &json!({"name":"test","message":"hi","every_seconds":i64::MAX}),
            "test", "chat", None,
        );
        assert!(result.is_err());
        assert!(!paths.cron_jobs_file().exists());
        let _ = std::fs::remove_dir_all(paths.base);
    }
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
            None,
        );
        assert!(r.is_ok(), "unexpected error: {:?}", r.err());
        let _ = std::fs::remove_dir_all(paths.base);
    }

    #[test]
    fn test_cron_validate_add_agent_mode() {
        let tool = CronTool;
        assert!(tool
            .validate(&json!({
                "action": "add", "name": "test", "message": "search latest news", "delay_seconds": 60, "mode": "agent"
            }))
            .is_ok());
    }

    #[test]
    fn test_cron_validate_add_script_mode_requires_skill_name() {
        let tool = CronTool;
        assert!(tool
            .validate(&json!({
                "action": "add", "name": "test", "message": "run job", "delay_seconds": 60, "mode": "script"
            }))
            .is_err());
    }

    #[test]
    fn test_cron_add_agent_mode_persists_agent_payload() {
        let paths = temp_paths("agent");
        let r = execute_cron_action_with_paths(
            &paths,
            "add",
            &json!({
                "name": "agent-task",
                "message": "请搜索美国伊朗最新新闻并总结",
                "delay_seconds": 60,
                "mode": "agent"
            }),
            "telegram",
            "12345",
            None,
        );
        assert!(r.is_ok(), "unexpected error: {:?}", r.err());

        let store = load_store(&paths).expect("load cron store");
        let payload_kind = store.jobs[0]
            .get("payload")
            .and_then(|v| v.get("kind"))
            .and_then(|v| v.as_str());
        assert_eq!(payload_kind, Some("agent"));

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

    #[test]
    fn test_cron_validate_invalid_timezone() {
        let paths = temp_paths("tz_invalid");
        let r = execute_cron_action_with_paths(
            &paths,
            "add",
            &json!({
                "name": "test",
                "message": "hi",
                "cron_expr": "0 0 9 * * *"
            }),
            "telegram",
            "12345",
            Some("Invalid/Timezone"),
        );
        assert!(r.is_err(), "Should reject invalid timezone");
        let err = r.unwrap_err().to_string();
        assert!(
            err.contains("Invalid timezone"),
            "Error message should mention invalid timezone"
        );
        let _ = std::fs::remove_dir_all(paths.base);
    }

    #[test]
    fn test_cron_user_specified_timezone_overrides_default() {
        let paths = temp_paths("tz_override");
        let r = execute_cron_action_with_paths(
            &paths,
            "add",
            &json!({
                "name": "test",
                "message": "hi",
                "cron_expr": "0 0 9 * * *",
                "tz": "America/New_York"
            }),
            "telegram",
            "12345",
            Some("Asia/Shanghai"), // default_timezone
        );
        assert!(
            r.is_ok(),
            "Should accept user-specified timezone: {:?}",
            r.err()
        );

        // Verify the job was saved with user-specified timezone, not default
        let store = load_store(&paths).expect("load cron store");
        let saved_tz = store.jobs[0]
            .get("schedule")
            .and_then(|s| s.get("tz"))
            .and_then(|v| v.as_str());
        assert_eq!(
            saved_tz,
            Some("America/New_York"),
            "Should use user-specified timezone"
        );
        let _ = std::fs::remove_dir_all(paths.base);
    }

    #[test]
    fn test_cron_validate_invalid_cron_expr() {
        let paths = temp_paths("cron_invalid");
        let r = execute_cron_action_with_paths(
            &paths,
            "add",
            &json!({
                "name": "test",
                "message": "hi",
                "cron_expr": "not a valid cron"
            }),
            "telegram",
            "12345",
            None,
        );
        assert!(r.is_err(), "Should reject invalid cron expression");
        let err = r.unwrap_err().to_string();
        assert!(
            err.contains("Invalid cron expression"),
            "Error message should mention invalid cron expression"
        );
        let _ = std::fs::remove_dir_all(paths.base);
    }

    #[test]
    fn test_cron_validate_negative_every_seconds() {
        let paths = temp_paths("every_negative");
        let r = execute_cron_action_with_paths(
            &paths,
            "add",
            &json!({
                "name": "test",
                "message": "hi",
                "every_seconds": -10
            }),
            "telegram",
            "12345",
            None,
        );
        assert!(r.is_err(), "Should reject negative every_seconds");
        let err = r.unwrap_err().to_string();
        assert!(
            err.contains("every_seconds must be a positive integer"),
            "Error message should mention positive integer requirement"
        );
        let _ = std::fs::remove_dir_all(paths.base);
    }

    #[test]
    fn test_cron_validate_zero_every_seconds() {
        let paths = temp_paths("every_zero");
        let r = execute_cron_action_with_paths(
            &paths,
            "add",
            &json!({
                "name": "test",
                "message": "hi",
                "every_seconds": 0
            }),
            "telegram",
            "12345",
            None,
        );
        assert!(r.is_err(), "Should reject zero every_seconds");
        let _ = std::fs::remove_dir_all(paths.base);
    }

    #[test]
    fn test_cron_add_with_default_timezone() {
        let paths = temp_paths("tz_default");
        let r = execute_cron_action_with_paths(
            &paths,
            "add",
            &json!({
                "name": "morning reminder",
                "message": "Good morning!",
                "cron_expr": "0 0 9 * * *"
            }),
            "telegram",
            "12345",
            Some("Asia/Shanghai"),
        );
        assert!(r.is_ok(), "Should accept default_timezone: {:?}", r.err());
        let result = r.unwrap();
        assert_eq!(
            result.get("status").and_then(|v| v.as_str()),
            Some("created")
        );

        // Verify the job was saved with correct timezone from default_timezone
        let store = load_store(&paths).expect("load cron store");
        let saved_tz = store.jobs[0]
            .get("schedule")
            .and_then(|s| s.get("tz"))
            .and_then(|v| v.as_str());
        assert_eq!(saved_tz, Some("Asia/Shanghai"));
        let _ = std::fs::remove_dir_all(paths.base);
    }

    #[test]
    fn test_cron_add_with_run_immediately() {
        let paths = temp_paths("run_immediately");
        let r = execute_cron_action_with_paths(
            &paths,
            "add",
            &json!({
                "name": "test",
                "message": "hi",
                "every_seconds": 3600,
                "run_immediately": true
            }),
            "telegram",
            "12345",
            None,
        );
        assert!(r.is_ok(), "Should accept run_immediately: {:?}", r.err());

        // Verify the job was saved with run_immediately
        let store = load_store(&paths).expect("load cron store");
        let run_immediately = store.jobs[0]
            .get("schedule")
            .and_then(|s| s.get("runImmediately"))
            .and_then(|v| v.as_bool());
        assert_eq!(run_immediately, Some(true));
        let _ = std::fs::remove_dir_all(paths.base);
    }

    #[test]
    fn test_invalid_cron_store_is_reported_without_moving_source() {
        let paths = temp_paths("invalid_store");
        std::fs::create_dir_all(paths.cron_dir()).expect("create cron dir");
        let path = paths.cron_jobs_file();
        std::fs::write(&path, "{partial json").expect("write invalid store");

        let result = load_store(&paths);

        assert!(result.is_err(), "invalid JSON must be reported");
        assert_eq!(
            std::fs::read_to_string(&path).expect("source must remain"),
            "{partial json"
        );
        assert!(!path.with_extension("json.corrupted").exists());
        let _ = std::fs::remove_dir_all(paths.base);
    }

    #[test]
    fn test_cron_rejects_non_positive_delay_seconds() {
        let paths = temp_paths("delay_non_positive");
        for delay in [0, -1] {
            let result = execute_cron_action_with_paths(
                &paths,
                "add",
                &json!({"name":"test","message":"hi","delay_seconds":delay}),
                "test",
                "chat",
                None,
            );
            assert!(result.is_err(), "delay {delay} must be rejected");
        }
        let _ = std::fs::remove_dir_all(paths.base);
    }

    #[test]
    fn test_cron_rejects_overflowing_delay_seconds() {
        let paths = temp_paths("delay_overflow");
        let result = execute_cron_action_with_paths(
            &paths,
            "add",
            &json!({"name":"test","message":"hi","delay_seconds":i64::MAX}),
            "test",
            "chat",
            None,
        );
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(paths.base);
    }
}
