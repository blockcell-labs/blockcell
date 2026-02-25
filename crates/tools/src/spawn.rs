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
            description: "Spawn a long-running background sub-agent for tasks that take minutes to complete (e.g. multi-step research, batch processing, continuous monitoring). DO NOT use spawn if you can answer the user directly in this conversation — spawn is only for truly async workloads that should not block the current reply. The sub-agent runs independently and sends a completion notification when done.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "Task description for the sub-agent"
                    },
                    "label": {
                        "type": "string",
                        "description": "Optional label for identifying this task"
                    }
                },
                "required": ["task"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("task").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: task".to_string()));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let task = params["task"].as_str().unwrap();
        let label = params
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("subagent");

        // Delegate to the SpawnHandle provided by the agent runtime
        let spawn_handle = ctx.spawn_handle.as_ref().ok_or_else(|| {
            Error::Tool("No spawn handle available. Subagent spawning is not configured.".to_string())
        })?;

        spawn_handle.spawn(task, label, &ctx.channel, &ctx.chat_id)
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
        assert!(tool.validate(&json!({"task": "do something"})).is_ok());
        assert!(tool.validate(&json!({})).is_err());
    }
}
