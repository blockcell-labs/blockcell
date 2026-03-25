use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolSchema};

pub struct SpawnTool;

#[async_trait]
impl Tool for SpawnTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "spawn",
            description: "Spawn a background sub-agent to execute a skill or long-running task. \
                **Preferred usage**: set `skill_name` to run a named skill (e.g. stock_analysis, crypto_tracker) — \
                the sub-agent will route that skill through the unified skill kernel. \
                Use `task` (text description) only when no matching skill exists. \
                DO NOT use spawn if you can answer the user directly — only for async workloads that should not block the current reply.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "Name of a skill to execute (e.g. 'stock_analysis', 'crypto_tracker'). \
                            When set, the sub-agent forces that skill name and runs the normal skill flow. \
                            PREFERRED over task description when a matching skill exists."
                    },
                    "params": {
                        "type": "object",
                        "description": "Parameters to pass to the skill (when skill_name is set). \
                            E.g. {\"query\": \"云天化\", \"user_query\": \"分析云天化涨停原因\", \"symbol\": \"600096\"}"
                    },
                    "task": {
                        "type": "string",
                        "description": "Task description for the sub-agent (used when no skill_name is given)"
                    },
                    "label": {
                        "type": "string",
                        "description": "Optional label for identifying this task"
                    }
                },
                "required": []
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- **`spawn` 互斥原则**: `spawn` 只用于用户明确要求后台执行、或任务需要数分钟以上的真正异步场景。**禁止**在同一轮对话中既直接回复用户又 spawn 子任务做同样的事——二者必须二选一：能直接回答就直接回答，不能直接回答才 spawn 并告知用户「正在后台处理」。".to_string())
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let has_skill = params
            .get("skill_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .is_some();
        let has_task = params
            .get("task")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .is_some();
        if !has_skill && !has_task {
            return Err(Error::Validation(
                "Either 'skill_name' or 'task' is required".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let spawn_handle = ctx.spawn_handle.as_ref().ok_or_else(|| {
            Error::Tool(
                "No spawn handle available. Subagent spawning is not configured.".to_string(),
            )
        })?;

        let skill_name = params
            .get("skill_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        if let Some(skill) = skill_name {
            // Skill-based spawn: pass only the skill name plus user-facing query text.
            // The subagent runtime then routes through the normal unified skill kernel.
            let skill_params = params.get("params").cloned().unwrap_or(json!({}));
            let label = params
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or(skill);
            let user_query = skill_params
                .get("user_query")
                .or_else(|| skill_params.get("query"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let task = format!("__SKILL_EXEC__:{}:{}", skill, user_query);
            spawn_handle.spawn(&task, label, &ctx.channel, &ctx.chat_id)
        } else {
            let task = params["task"].as_str().unwrap_or("");
            let label = params
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("subagent");
            spawn_handle.spawn(task, label, &ctx.channel, &ctx.chat_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SpawnHandle;
    use blockcell_core::Config;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    struct CaptureSpawnHandle {
        captured_task: Arc<Mutex<Option<String>>>,
    }

    impl SpawnHandle for CaptureSpawnHandle {
        fn spawn(
            &self,
            task: &str,
            _label: &str,
            _origin_channel: &str,
            _origin_chat_id: &str,
        ) -> Result<Value> {
            *self.captured_task.lock().expect("capture lock") = Some(task.to_string());
            Ok(json!({ "ok": true }))
        }
    }

    #[test]
    fn test_spawn_schema() {
        let tool = SpawnTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "spawn");
    }

    #[test]
    fn test_spawn_validate() {
        let tool = SpawnTool;
        // task only
        assert!(tool.validate(&json!({"task": "do something"})).is_ok());
        // skill_name only
        assert!(tool
            .validate(&json!({"skill_name": "stock_analysis"}))
            .is_ok());
        // both
        assert!(tool
            .validate(&json!({"skill_name": "stock_analysis", "task": "fallback"}))
            .is_ok());
        // neither — error
        assert!(tool.validate(&json!({})).is_err());
        // empty strings — error
        assert!(tool
            .validate(&json!({"skill_name": "", "task": ""}))
            .is_err());
    }

    #[tokio::test]
    async fn test_spawn_execute_formats_skill_task_without_legacy_params_blob() {
        let captured_task = Arc::new(Mutex::new(None));
        let tool = SpawnTool;
        let ctx = ToolContext {
            workspace: PathBuf::from("/tmp/workspace"),
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "cli:test".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: None,
            chat_id: "chat-1".to_string(),
            config: Config::default(),
            permissions: blockcell_core::types::PermissionSet::new(),
            task_manager: None,
            memory_store: None,
            outbound_tx: None,
            spawn_handle: Some(Arc::new(CaptureSpawnHandle {
                captured_task: Arc::clone(&captured_task),
            })),
            capability_registry: None,
            core_evolution: None,
            event_emitter: None,
            channel_contacts_file: None,
            response_cache: None,
        };

        tool.execute(
            ctx,
            json!({
                "skill_name": "weather",
                "params": {
                    "user_query": "北京天气"
                }
            }),
        )
        .await
        .expect("spawn should succeed");

        assert_eq!(
            captured_task.lock().expect("capture lock").as_deref(),
            Some("__SKILL_EXEC__:weather:北京天气")
        );
    }
}
