use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for listing and querying background tasks.
pub struct ListTasksTool;

#[async_trait]
impl Tool for ListTasksTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_tasks".to_string(),
            description: "List background tasks (subagent jobs). Shows task status, progress, and results. Use this when the user asks about ongoing work or task progress.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Optional: get details for a specific task by ID"
                    },
                    "status": {
                        "type": "string",
                        "enum": ["queued", "running", "completed", "failed", "cancelled", "all"],
                        "description": "Filter by status (default: all)"
                    }
                },
                "required": []
            }),
        }
    }

    fn validate(&self, _params: &Value) -> Result<()> {
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let tm = ctx
            .task_manager
            .as_ref()
            .ok_or_else(|| Error::Tool("Task manager not available".to_string()))?;

        // If a specific task_id is requested
        if let Some(task_id) = params.get("task_id").and_then(|v| v.as_str()) {
            return match tm.get_task_json(task_id).await {
                Some(task) => Ok(task),
                None => Err(Error::NotFound(format!("Task not found: {}", task_id))),
            };
        }

        // List tasks with optional status filter
        let status_filter = params.get("status").and_then(|v| v.as_str()).and_then(|s| {
            if s == "all" {
                None
            } else {
                Some(s.to_string())
            }
        });

        let tasks = tm.list_tasks_json(status_filter).await;
        let summary = tm.summary_json().await;

        Ok(json!({
            "summary": summary,
            "tasks": tasks
        }))
    }
}

/// Cancels tasks through the standard policy, admission and audit pipeline.
pub struct CancelTaskTool;

#[async_trait]
impl Tool for CancelTaskTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "cancel_task".to_string(),
            description: "Cancel a queued or running background task owned by this conversation. Stops execution; inspect its cancelled status with list_tasks.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {"task_id": {"type": "string", "minLength": 1}},
                "required": ["task_id"],
                "additionalProperties": false
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params
            .get("task_id")
            .and_then(Value::as_str)
            .is_none_or(|id| id.trim().is_empty())
        {
            return Err(Error::Validation(
                "A non-empty task_id is required".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        self.validate(&params)?;
        let manager = ctx
            .task_manager
            .as_ref()
            .ok_or_else(|| Error::Tool("Task manager not available".to_string()))?;
        let task_id = params["task_id"].as_str().unwrap();
        manager.cancel_task(task_id).await?;
        manager
            .get_task_json(task_id)
            .await
            .ok_or_else(|| Error::NotFound(format!("Task not found: {}", task_id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_list_tasks_schema() {
        let tool = ListTasksTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "list_tasks");
    }

    #[test]
    fn test_list_tasks_validate() {
        let tool = ListTasksTool;
        assert!(tool.validate(&json!({})).is_ok());
        assert!(tool.validate(&json!({"task_id": "abc"})).is_ok());
        assert!(tool.validate(&json!({"status": "running"})).is_ok());
    }
}
