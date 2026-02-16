use async_trait::async_trait;
use blockcell_core::{Error, OutboundMessage, Result};
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

pub struct MessageTool;

#[async_trait]
impl Tool for MessageTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "message",
            description: "Send a message to a specific channel and chat. Use this only when you need to send to a different channel/chat than the current conversation.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "Message content to send"
                    },
                    "channel": {
                        "type": "string",
                        "description": "Target channel (telegram, whatsapp, feishu). Optional, defaults to current channel."
                    },
                    "chat_id": {
                        "type": "string",
                        "description": "Target chat ID. Optional, defaults to current chat."
                    }
                },
                "required": ["content"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("content").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: content".to_string()));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let content = params["content"].as_str().unwrap();
        let channel = params
            .get("channel")
            .and_then(|v| v.as_str())
            .unwrap_or(&ctx.channel);
        let chat_id = params
            .get("chat_id")
            .and_then(|v| v.as_str())
            .unwrap_or(&ctx.chat_id);

        // Send through the outbound message bus
        let outbound_tx = ctx.outbound_tx.as_ref().ok_or_else(|| {
            Error::Tool("No outbound message channel available. Message delivery is not configured.".to_string())
        })?;

        let outbound = OutboundMessage::new(channel, chat_id, content);
        outbound_tx.send(outbound).await.map_err(|e| {
            Error::Tool(format!("Failed to send message: {}", e))
        })?;

        debug!(channel = channel, chat_id = chat_id, content_len = content.len(), "Message sent via outbound_tx");

        Ok(json!({
            "status": "sent",
            "channel": channel,
            "chat_id": chat_id,
            "content_length": content.len()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_message_schema() {
        let tool = MessageTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "message");
    }

    #[test]
    fn test_message_validate() {
        let tool = MessageTool;
        assert!(tool.validate(&json!({"content": "hello"})).is_ok());
        assert!(tool.validate(&json!({})).is_err());
    }
}
