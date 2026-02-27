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
                the skill's SKILL.rhai will execute with the given params. \
                Use `task` (text description) only when no matching skill exists. \
                DO NOT use spawn if you can answer the user directly — only for async workloads that should not block the current reply.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "Name of a skill to execute (e.g. 'stock_analysis', 'crypto_tracker'). \
                            When set, the skill's SKILL.rhai runs directly with the given params. \
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

    fn validate(&self, params: &Value) -> Result<()> {
        let has_skill = params.get("skill_name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).is_some();
        let has_task = params.get("task").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).is_some();
        if !has_skill && !has_task {
            return Err(Error::Validation("Either 'skill_name' or 'task' is required".to_string()));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let spawn_handle = ctx.spawn_handle.as_ref().ok_or_else(|| {
            Error::Tool("No spawn handle available. Subagent spawning is not configured.".to_string())
        })?;

        let skill_name = params.get("skill_name").and_then(|v| v.as_str()).filter(|s| !s.is_empty());

        if let Some(skill) = skill_name {
            // Skill-based spawn: build a task string that directs the subagent to run the skill
            let skill_params = params.get("params").cloned().unwrap_or(json!({}));
            let label = params.get("label").and_then(|v| v.as_str()).unwrap_or(skill);
            // Build a structured task instruction for the subagent
            let task = format!(
                "Execute skill '{}' with params: {}. \
                Call the skill's run_skill tool or execute the skill directly using the skill engine. \
                Return the skill's output as the final result.",
                skill,
                skill_params
            );
            spawn_handle.spawn(&task, label, &ctx.channel, &ctx.chat_id)
        } else {
            let task = params["task"].as_str().unwrap_or("");
            let label = params.get("label").and_then(|v| v.as_str()).unwrap_or("subagent");
            spawn_handle.spawn(task, label, &ctx.channel, &ctx.chat_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        assert!(tool.validate(&json!({"skill_name": "stock_analysis"})).is_ok());
        // both
        assert!(tool.validate(&json!({"skill_name": "stock_analysis", "task": "fallback"})).is_ok());
        // neither — error
        assert!(tool.validate(&json!({})).is_err());
        // empty strings — error
        assert!(tool.validate(&json!({"skill_name": "", "task": ""})).is_err());
    }
}
