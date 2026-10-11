use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for sending messages to running agent tasks.
pub struct SendMessageTool;

/// SendMessage parameters
#[derive(Debug, Deserialize)]
pub struct SendMessageParams {
    pub task_id: String,
    pub message: String,
}

#[async_trait]
impl Tool for SendMessageTool {
    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Required
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "send_message".to_string(),
            description: "Send a message to a running agent task. \
                Only works for non-ONE_SHOT agents (verification, viper, general). \
                ONE_SHOT agents (explore, plan) cannot receive messages after completion."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "The task_id of the target agent"
                    },
                    "message": {
                        "type": "string",
                        "description": "The message content to send"
                    }
                },
                "required": ["task_id", "message"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let has_task_id = params
            .get("task_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .is_some();

        let has_message = params
            .get("message")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .is_some();

        if !has_task_id || !has_message {
            return Err(Error::Validation(
                "Both 'task_id' and 'message' are required".to_string(),
            ));
        }

        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let parsed: SendMessageParams = serde_json::from_value(params)?;

        // Check if task_manager is available
        let task_manager = ctx
            .task_manager
            .as_ref()
            .ok_or_else(|| Error::Tool("TaskManager not available".to_string()))?;

        // Check if task is ONE_SHOT type
        let is_one_shot = task_manager.is_one_shot_task(&parsed.task_id).await;

        if is_one_shot {
            return Ok(json!({
                "error": "ONE_SHOT agents (explore, plan) cannot receive SendMessage",
                "task_id": parsed.task_id,
                "hint": "Use list_tasks to find non-ONE_SHOT agent task_ids"
            }));
        }

        // Send the message
        task_manager
            .send_message(&parsed.task_id, parsed.message.clone())
            .await?;

        Ok(json!({
            "success": true,
            "task_id": parsed.task_id,
            "message": "Message sent to agent task"
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_send_message_schema() {
        let tool = SendMessageTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "send_message");
        // Check description mentions ONE_SHOT limitation
        let desc = schema.description;
        assert!(desc.contains("ONE_SHOT"));
        assert!(desc.contains("explore"));
        assert!(desc.contains("plan"));
    }

    #[test]
    fn test_send_message_schema_parameters() {
        let tool = SendMessageTool;
        let schema = tool.schema();
        let params = schema.parameters.as_object().unwrap();
        let props = params.get("properties").unwrap().as_object().unwrap();

        // Check required fields
        let required = params.get("required").unwrap().as_array().unwrap();
        assert!(required.contains(&json!("task_id")));
        assert!(required.contains(&json!("message")));

        // Check properties exist
        assert!(props.contains_key("task_id"));
        assert!(props.contains_key("message"));
    }

    #[test]
    fn test_send_message_validate() {
        let tool = SendMessageTool;
        // Valid params
        assert!(tool
            .validate(&json!({"task_id": "abc", "message": "hello"}))
            .is_ok());
        // Missing task_id
        assert!(tool.validate(&json!({"message": "hello"})).is_err());
        // Missing message
        assert!(tool.validate(&json!({"task_id": "abc"})).is_err());
        // Empty strings
        assert!(tool
            .validate(&json!({"task_id": "", "message": "hello"}))
            .is_err());
        assert!(tool
            .validate(&json!({"task_id": "abc", "message": ""}))
            .is_err());
        // Both empty
        assert!(tool
            .validate(&json!({"task_id": "", "message": ""}))
            .is_err());
    }

    #[test]
    fn test_send_message_params_deserialization() {
        let params: SendMessageParams = serde_json::from_value(json!({
            "task_id": "task-123",
            "message": "continue with the analysis"
        }))
        .unwrap();
        assert_eq!(params.task_id, "task-123");
        assert_eq!(params.message, "continue with the analysis");
    }

    #[test]
    fn test_send_message_params_missing_field() {
        // Missing message
        let result = serde_json::from_value::<SendMessageParams>(json!({
            "task_id": "task-123"
        }));
        assert!(result.is_err());

        // Missing task_id
        let result = serde_json::from_value::<SendMessageParams>(json!({
            "message": "hello"
        }));
        assert!(result.is_err());
    }
}
