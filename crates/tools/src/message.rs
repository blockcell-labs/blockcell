use async_trait::async_trait;
use blockcell_core::{Error, OutboundMessage, Paths, Result};
use serde_json::{json, Value};
use std::path::Path;
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

pub struct MessageTool;

#[async_trait]
impl Tool for MessageTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "message",
            description: "Send a message (text and/or media files) to a channel. Use this to send images, files, or text to the current or a different channel/chat. For cross-channel sending, you can provide either 'chat_id' directly OR 'target_name' to look up a known contact by name. The system automatically remembers users who have sent messages on each channel.",
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
                        "description": "Target chat ID. If sending to a different channel and you don't know the chat_id, use 'target_name' instead to look up by name."
                    },
                    "target_name": {
                        "type": "string",
                        "description": "Name of the target user or group to send to. The system will look up the chat_id from known contacts who have previously messaged the bot on the target channel. Use this when you don't have the exact chat_id."
                    }
                },
                "required": []
            }),
        }
    }

    fn prompt_rule(&self, ctx: &crate::PromptContext) -> Option<String> {
        let mut rules = String::new();
        if ctx.is_im_channel() {
            rules.push_str("- **当前渠道为 IM 聊天（不渲染 Markdown）**: 不要在回复文字中使用 markdown 图片语法（如 `![](path)`），IM 端不会渲染。若需展示图片内容，用文字描述即可。\n");
            rules.push_str("- **发送图片/文件给用户（⚠️ 必须调用 message 工具，否则文件不会发出）**: 当用户要求发回图片/文件时，**必须**调用 `message` 工具，参数示例：`{\"media\": [\"/root/.blockcell/workspace/media/xxx.jpg\"], \"content\": \"这是你要的图片\"}`。**绝对禁止**在不调用工具的情况下直接回复\"发送成功\"——那是幻觉，图片根本没有发出去。\n");
        } else {
            rules.push_str("- **Media display**: The WebUI can render images and play audio inline. To show an image or audio file, include the full file path in your response text (e.g. `/root/.blockcell/workspace/photo.jpg`). The frontend will auto-detect media paths and render them. You can also use markdown image syntax: `![description](file_path)`. NEVER say you cannot display images — you CAN.\n");
            rules.push_str("- **发送图片/文件给用户（通过聊天渠道）**: 调用 `message` 工具，参数 `media=[\"<本地文件路径>\"]`。仅在回复文字中写 markdown 图片语法无法真正发送文件，必须用工具调用。\n");
        }
        rules.push_str("- **发送语音给用户**: 需要先将文字合成为语音文件（TTS），再用 `message` 工具 `media=[\"<语音文件路径>\"]` 发送。TTS 能力由技能提供——如果用户要求发语音但没有 TTS 技能，请提示用户安装相应技能（如 tts 技能）。");
        Some(rules)
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let has_content = params
            .get("content")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let has_media = params
            .get("media")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        if !has_content && !has_media {
            return Err(Error::Validation(
                "At least one of 'content' or 'media' must be provided".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let channel_param = params.get("channel").and_then(|v| v.as_str());
        let chat_id_param = params.get("chat_id").and_then(|v| v.as_str());
        let target_name_param = params.get("target_name").and_then(|v| v.as_str());

        let channel = channel_param.unwrap_or(&ctx.channel);

        // Resolve chat_id: direct param > target_name lookup > same-channel default
        let resolved_chat_id: String;
        if let Some(cid) = chat_id_param {
            resolved_chat_id = cid.to_string();
        } else if channel != ctx.channel {
            // Cross-channel: try target_name lookup
            if let Some(name) = target_name_param {
                resolved_chat_id = self.lookup_contact_by_name(&ctx, channel, name)?;
            } else {
                // No chat_id and no target_name — list known contacts as hint
                let hint = self.list_known_contacts_hint(&ctx, channel);
                return Err(Error::Tool(format!(
                    "When sending to a different channel ('{}'), you must provide 'chat_id' or 'target_name'. {}",
                    channel, hint
                )));
            }
        } else {
            resolved_chat_id = ctx.chat_id.clone();
        }
        let chat_id = &resolved_chat_id;

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

        // Auto-copy files that exist but are outside the workspace into workspace/media/.
        // This allows sending desktop/Downloads files without requiring the LLM to manually copy them.
        let media_dir = ctx.workspace.join("media");
        let mut final_media_paths: Vec<String> = Vec::with_capacity(resolved_media_paths.len());
        for path in &resolved_media_paths {
            let p = Path::new(path);
            if !p.exists() {
                return Err(Error::Tool(format!("Media file not found: {}", path)));
            }
            // Check if already inside workspace
            let in_workspace = p
                .canonicalize()
                .ok()
                .and_then(|abs| {
                    ctx.workspace
                        .canonicalize()
                        .ok()
                        .map(|ws| abs.starts_with(ws))
                })
                .unwrap_or(false);
            if in_workspace {
                final_media_paths.push(path.clone());
            } else {
                // Copy to workspace/media/<filename>
                if let Err(e) = std::fs::create_dir_all(&media_dir) {
                    return Err(Error::Tool(format!("Failed to create media dir: {}", e)));
                }
                let filename = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "file".to_string());
                let dest = media_dir.join(&filename);
                if let Err(e) = std::fs::copy(p, &dest) {
                    return Err(Error::Tool(format!(
                        "Failed to copy media file {}: {}",
                        path, e
                    )));
                }
                final_media_paths.push(dest.to_string_lossy().into_owned());
            }
        }

        // Rewrite original paths in content to their copied workspace paths.
        // This fixes markdown image refs like ![alt](/Users/apple/Desktop/x.png) → new path.
        let mut rewritten_content = content.to_string();
        for (original, final_path) in resolved_media_paths.iter().zip(final_media_paths.iter()) {
            if original != final_path {
                rewritten_content =
                    rewritten_content.replace(original.as_str(), final_path.as_str());
            }
        }

        // Send through the outbound message bus
        let outbound_tx = ctx.outbound_tx.as_ref().ok_or_else(|| {
            Error::Tool(
                "No outbound message channel available. Message delivery is not configured."
                    .to_string(),
            )
        })?;

        let mut outbound = OutboundMessage::new(channel, chat_id, &rewritten_content);
        if channel == ctx.channel {
            outbound.account_id = ctx.account_id.clone();
        }
        outbound.media = final_media_paths.clone();
        outbound_tx
            .send(outbound)
            .await
            .map_err(|e| Error::Tool(format!("Failed to send message: {}", e)))?;

        debug!(
            channel = channel,
            chat_id = chat_id,
            content_len = content.len(),
            media_count = final_media_paths.len(),
            "Message sent via outbound_tx"
        );

        Ok(json!({
            "status": "sent",
            "channel": channel,
            "chat_id": chat_id,
            "content_length": content.len(),
            "media_count": final_media_paths.len(),
            "media": final_media_paths
        }))
    }
}

impl MessageTool {
    /// Look up a contact by name in the channel contacts registry.
    /// Returns the chat_id if exactly one match is found, or an error with hints.
    fn lookup_contact_by_name(
        &self,
        ctx: &ToolContext,
        channel: &str,
        name: &str,
    ) -> Result<String> {
        let contacts_file = ctx.channel_contacts_file.as_ref().ok_or_else(|| {
            Error::Tool("Channel contacts registry is not configured.".to_string())
        })?;
        // Derive the base dir from the contacts file path (it lives at ~/.blockcell/channel_contacts.json)
        let base = contacts_file.parent().unwrap_or(Path::new("."));
        let paths = Paths::with_base(base.to_path_buf());
        let store = blockcell_storage::ChannelContacts::new(paths);
        let matches = store.lookup(channel, name);

        match matches.len() {
            0 => {
                let all = store.list_by_channel(channel);
                if all.is_empty() {
                    Err(Error::Tool(format!(
                        "No known contacts for channel '{}'. A user must first send a message to the bot on '{}' before you can send messages to them.",
                        channel, channel
                    )))
                } else {
                    let names: Vec<String> = all
                        .iter()
                        .map(|c| {
                            if c.name.is_empty() {
                                c.chat_id.clone()
                            } else {
                                format!("{} ({})", c.name, c.chat_type)
                            }
                        })
                        .collect();
                    Err(Error::Tool(format!(
                        "No contact matching '{}' found on '{}'. Known contacts: {}",
                        name,
                        channel,
                        names.join(", ")
                    )))
                }
            }
            1 => {
                debug!(
                    channel = channel,
                    name = name,
                    chat_id = %matches[0].chat_id,
                    "Resolved target_name to chat_id via contacts registry"
                );
                Ok(matches[0].chat_id.clone())
            }
            _ => {
                let options: Vec<String> = matches
                    .iter()
                    .map(|c| format!("{} → chat_id: {} ({})", c.name, c.chat_id, c.chat_type))
                    .collect();
                Err(Error::Tool(format!(
                    "Multiple contacts matching '{}' on '{}'. Please be more specific or use chat_id directly: {}",
                    name, channel, options.join("; ")
                )))
            }
        }
    }

    /// Build a hint string listing known contacts for a channel.
    fn list_known_contacts_hint(&self, ctx: &ToolContext, channel: &str) -> String {
        let contacts_file = match ctx.channel_contacts_file.as_ref() {
            Some(f) => f,
            None => return "Channel contacts registry is not configured.".to_string(),
        };
        let base = contacts_file.parent().unwrap_or(Path::new("."));
        let paths = Paths::with_base(base.to_path_buf());
        let store = blockcell_storage::ChannelContacts::new(paths);
        let all = store.list_by_channel(channel);
        if all.is_empty() {
            format!(
                "No known contacts for '{}'. A user must first send a message to the bot on '{}' so the system can remember their ID.",
                channel, channel
            )
        } else {
            let names: Vec<String> = all
                .iter()
                .map(|c| {
                    if c.name.is_empty() {
                        format!("chat_id={} ({})", c.chat_id, c.chat_type)
                    } else {
                        format!("\"{}\" → chat_id={} ({})", c.name, c.chat_id, c.chat_type)
                    }
                })
                .collect();
            format!(
                "Known contacts on '{}': {}. You can use 'target_name' to send by name.",
                channel,
                names.join(", ")
            )
        }
    }
}

fn resolve_media_path(workspace: &std::path::Path, input: &str) -> Result<String> {
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
        assert!(tool
            .validate(&json!({"content": "hello", "media": ["/tmp/test.jpg"]}))
            .is_ok());
        assert!(tool.validate(&json!({})).is_err());
        assert!(tool.validate(&json!({"content": ""})).is_err());
        assert!(tool.validate(&json!({"media": []})).is_err());
    }
}
