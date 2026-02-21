use async_trait::async_trait;
use blockcell_core::{Error, OutboundMessage, Result};
use serde_json::{json, Value};
use tracing::debug;
use std::path::{Path, PathBuf};

use crate::{Tool, ToolContext, ToolSchema};

pub struct MessageTool;

#[async_trait]
impl Tool for MessageTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "message",
            description: "Send a message (text and/or media files) to a channel. Use this to send images, files, or text to the current or a different channel/chat. For sending images/files, provide their local file paths in the 'media' array.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "Text message content to send. Can be empty if only sending media."
                    },
                    "media": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Array of local file paths to send as media (images, documents, etc). Example: [\"/root/.blockcell/workspace/media/photo.jpg\"]"
                    },
                    "channel": {
                        "type": "string",
                        "description": "Target channel (wecom, telegram, feishu, slack, discord, dingtalk, whatsapp). Optional, defaults to current channel."
                    },
                    "chat_id": {
                        "type": "string",
                        "description": "Target chat ID. Optional, defaults to current chat."
                    }
                },
                "required": []
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let has_content = params.get("content").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
        let has_media = params.get("media").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
        if !has_content && !has_media {
            return Err(Error::Validation("At least one of 'content' or 'media' must be provided".to_string()));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let channel = params
            .get("channel")
            .and_then(|v| v.as_str())
            .unwrap_or(&ctx.channel);
        let chat_id = params
            .get("chat_id")
            .and_then(|v| v.as_str())
            .unwrap_or(&ctx.chat_id);

        let media_paths_raw: Vec<String> = params
            .get("media")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let resolved_media_paths: Vec<String> = media_paths_raw
            .iter()
            .map(|p| resolve_media_path(&ctx.workspace, p))
            .collect::<Result<Vec<String>>>()?;

        for path in &resolved_media_paths {
            if !Path::new(path).exists() {
                return Err(Error::Tool(format!("Media file not found: {}", path)));
            }
        }

        // Send through the outbound message bus
        let outbound_tx = ctx.outbound_tx.as_ref().ok_or_else(|| {
            Error::Tool("No outbound message channel available. Message delivery is not configured.".to_string())
        })?;

        let mut outbound = OutboundMessage::new(channel, chat_id, content);
        outbound.media = resolved_media_paths.clone();
        outbound_tx.send(outbound).await.map_err(|e| {
            Error::Tool(format!("Failed to send message: {}", e))
        })?;

        debug!(
            channel = channel,
            chat_id = chat_id,
            content_len = content.len(),
            media_count = resolved_media_paths.len(),
            "Message sent via outbound_tx"
        );

        Ok(json!({
            "status": "sent",
            "channel": channel,
            "chat_id": chat_id,
            "content_length": content.len(),
            "media_count": resolved_media_paths.len(),
            "media": resolved_media_paths
        }))
    }
}

fn resolve_media_path(workspace: &PathBuf, input: &str) -> Result<String> {
    let p = Path::new(input);
    if p.exists() {
        return Ok(input.to_string());
    }

    if !p.is_absolute() {
        let candidate = workspace.join(input);
        if candidate.exists() {
            return Ok(candidate.display().to_string());
        }

        let candidate = workspace.join("media").join(input);
        if candidate.exists() {
            return Ok(candidate.display().to_string());
        }
    }

    Err(Error::Tool(format!("Media file not found: {}", input)))
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
        assert!(tool.validate(&json!({"media": ["/tmp/test.jpg"]})).is_ok());
        assert!(tool.validate(&json!({"content": "hello", "media": ["/tmp/test.jpg"]})).is_ok());
        assert!(tool.validate(&json!({})).is_err());
        assert!(tool.validate(&json!({"content": ""})).is_err());
        assert!(tool.validate(&json!({"media": []})).is_err());
    }
}
