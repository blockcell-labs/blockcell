//! Extended NapCatQQ tools for BlockCell.
//!
//! This module provides extended tools for:
//! - Forward message: get_forward_msg
//! - Emoji reaction: set_msg_emoji_like
//! - Message status: mark_msg_as_read
//! - Essence message: set_essence_msg, delete_essence_msg, get_essence_msg_list
//! - Group extended: get_group_at_all_remain
//! - Media resources: get_image, get_record, get_video, download_file
//!
//! All tools support both WebSocket and HTTP modes. WebSocket is preferred when available.

use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};

use crate::napcat::common::{
    build_description, build_napcat_permissions, check_channel,
    resolve_account_id, call_api, download_media_if_needed,
    RiskLevel, ApiRequest,
};
use crate::{Tool, ToolContext, ToolSchema};

// =============================================================================
// Forward Message Tool
// =============================================================================

/// Get forward message content tool.
pub struct NapcatGetForwardMsgTool;

#[async_trait]
impl Tool for NapcatGetForwardMsgTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_forward_msg",
            description: build_description(
                "[QQ Message] Get the content of a forwarded/merged QQ message. Use this when you receive a forwarded message (合并消息) in QQ - it expands and returns all nested message nodes.",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "message_id": {
                        "type": "string",
                        "description": "Forward message ID (the id field from a forward segment)"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["message_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("message_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: message_id".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_forward_msg")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let message_id = params.get("message_id").and_then(|v| v.as_str()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_forward_msg(message_id, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_forward_msg failed: {}",
                response.error_message()
            )));
        }

        tracing::info!(message_id = message_id, "Retrieved forward message content");

        Ok(json!({
            "success": true,
            "message_id": message_id,
            "content": response.data
        }))
    }
}

// =============================================================================
// Emoji Reaction Tool
// =============================================================================

/// Set message emoji like tool (add emoji reaction).
pub struct NapcatSetMsgEmojiLikeTool;

#[async_trait]
impl Tool for NapcatSetMsgEmojiLikeTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_set_msg_emoji_like",
            description: build_description(
                "[QQ Message] Add or remove an emoji reaction to a QQ message. Risk: Medium.",
                RiskLevel::MediumRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "message_id": {
                        "type": "integer",
                        "description": "Message ID to react to"
                    },
                    "emoji_id": {
                        "type": "string",
                        "description": "Emoji ID (e.g., '1' for like, '2' for heart)"
                    },
                    "set": {
                        "type": "boolean",
                        "description": "Whether to set (add) or unset (remove) the emoji reaction (default: true)"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["message_id", "emoji_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("message_id").and_then(|v| v.as_i64()).is_none() {
            return Err(Error::Validation("Missing required parameter: message_id".into()));
        }
        if params.get("emoji_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: emoji_id".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_set_msg_emoji_like")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let message_id = params.get("message_id").and_then(|v| v.as_i64()).unwrap();
        let emoji_id = params.get("emoji_id").and_then(|v| v.as_str()).unwrap();
        let set = params.get("set").and_then(|v| v.as_bool());
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::set_msg_emoji_like(message_id, emoji_id, set, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "set_msg_emoji_like failed: {}",
                response.error_message()
            )));
        }

        let action = if set.unwrap_or(true) { "Added" } else { "Removed" };
        tracing::info!(message_id = message_id, emoji_id = emoji_id, set = ?set, "{} emoji reaction", action);

        Ok(json!({
            "success": true,
            "message": format!("{} emoji {} {} message {}", action, emoji_id, if set.unwrap_or(true) { "to" } else { "from" }, message_id)
        }))
    }
}

// =============================================================================
// Mark as Read Tool
// =============================================================================

/// Mark message as read tool.
pub struct NapcatMarkMsgAsReadTool;

#[async_trait]
impl Tool for NapcatMarkMsgAsReadTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_mark_msg_as_read",
            description: build_description(
                "[QQ Message] Mark a QQ message as read.",
                RiskLevel::LowRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "message_id": {
                        "type": "integer",
                        "description": "Message ID to mark as read"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["message_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("message_id").and_then(|v| v.as_i64()).is_none() {
            return Err(Error::Validation("Missing required parameter: message_id".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_mark_msg_as_read")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let message_id = params.get("message_id").and_then(|v| v.as_i64()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::mark_msg_as_read(message_id, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "mark_msg_as_read failed: {}",
                response.error_message()
            )));
        }

        tracing::info!(message_id = message_id, "Message marked as read");

        Ok(json!({
            "success": true,
            "message": format!("Message {} marked as read", message_id)
        }))
    }
}

// =============================================================================
// Essence Message Tools
// =============================================================================

/// Set essence message tool.
pub struct NapcatSetEssenceMsgTool;

#[async_trait]
impl Tool for NapcatSetEssenceMsgTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_set_essence_msg",
            description: build_description(
                "[QQ Group] Set a QQ message as essence (pin as important message in group). Requires admin permissions. Risk: Medium.",
                RiskLevel::MediumRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "message_id": {
                        "type": "integer",
                        "description": "Message ID to set as essence"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["message_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("message_id").and_then(|v| v.as_i64()).is_none() {
            return Err(Error::Validation("Missing required parameter: message_id".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_set_essence_msg")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let message_id = params.get("message_id").and_then(|v| v.as_i64()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::set_essence_msg(message_id, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "set_essence_msg failed: {}",
                response.error_message()
            )));
        }

        tracing::info!(message_id = message_id, "Set essence message");

        Ok(json!({
            "success": true,
            "message": format!("Message {} set as essence", message_id)
        }))
    }
}

/// Delete essence message tool.
pub struct NapcatDeleteEssenceMsgTool;

#[async_trait]
impl Tool for NapcatDeleteEssenceMsgTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_delete_essence_msg",
            description: build_description(
                "[QQ Group] Remove a QQ message from essence (unpin important message). Requires admin permissions. Risk: Medium.",
                RiskLevel::MediumRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "message_id": {
                        "type": "integer",
                        "description": "Message ID to remove from essence"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["message_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("message_id").and_then(|v| v.as_i64()).is_none() {
            return Err(Error::Validation("Missing required parameter: message_id".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_delete_essence_msg")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let message_id = params.get("message_id").and_then(|v| v.as_i64()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::delete_essence_msg(message_id, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "delete_essence_msg failed: {}",
                response.error_message()
            )));
        }

        tracing::info!(message_id = message_id, "Deleted essence message");

        Ok(json!({
            "success": true,
            "message": format!("Message {} removed from essence", message_id)
        }))
    }
}

/// Get essence message list tool.
pub struct NapcatGetEssenceMsgListTool;

#[async_trait]
impl Tool for NapcatGetEssenceMsgListTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_essence_msg_list",
            description: build_description(
                "[QQ Group] Get the list of essence messages in a QQ group.",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "group_id": {
                        "type": "string",
                        "description": "Group ID"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["group_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("group_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: group_id".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_essence_msg_list")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let group_id = params.get("group_id").and_then(|v| v.as_str()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_essence_msg_list(group_id, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_essence_msg_list failed: {}",
                response.error_message()
            )));
        }

        let list: Vec<Value> = serde_json::from_value(response.data)
            .map_err(|e| Error::Tool(format!("Failed to parse essence list: {}", e)))?;

        tracing::info!(group_id = group_id, count = list.len(), "Retrieved essence message list");

        Ok(json!({
            "success": true,
            "group_id": group_id,
            "count": list.len(),
            "essence_messages": list
        }))
    }
}

// =============================================================================
// Group Extended Tools
// =============================================================================

/// Get group @all remain count tool.
pub struct NapcatGetGroupAtAllRemainTool;

#[async_trait]
impl Tool for NapcatGetGroupAtAllRemainTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_group_at_all_remain",
            description: build_description(
                "[QQ Group] Get the remaining count of @all mentions for the bot in a QQ group.",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "group_id": {
                        "type": "string",
                        "description": "Group ID"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["group_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("group_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: group_id".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_group_at_all_remain")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let group_id = params.get("group_id").and_then(|v| v.as_str()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_group_at_all_remain(group_id, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_group_at_all_remain failed: {}",
                response.error_message()
            )));
        }

        tracing::info!(group_id = group_id, "Retrieved @all remain count");

        Ok(json!({
            "success": true,
            "group_id": group_id,
            "data": response.data
        }))
    }
}

// =============================================================================
// Media/Resource Tools
// =============================================================================

/// Get image info tool.
/// Note: Auto-download is controlled by global config `auto_download_media` in message preprocessing.
pub struct NapcatGetImageTool;

#[async_trait]
impl Tool for NapcatGetImageTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_image",
            description: build_description(
                "[QQ Image] Get image metadata from QQ image message. Returns URL, size, and other info. Note: Images are auto-downloaded during message preprocessing (controlled by auto_download_media config).",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "Image file identifier (from QQ image message segment) - file path, URL or Base64"
                    },
                    "file_id": {
                        "type": "string",
                        "description": "File ID (alternative to file parameter)"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": []
            }),
        }
    }

    fn validate(&self, _params: &Value) -> Result<()> {
        // Either file or file_id should be provided, but we'll let the API handle validation
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_image")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let file = params.get("file").and_then(|v| v.as_str());
        let file_id = params.get("file_id").and_then(|v| v.as_str());
        let account_id = resolve_account_id(&ctx, &params);

        if file.is_none() && file_id.is_none() {
            return Err(Error::Validation("Either 'file' or 'file_id' parameter is required".into()));
        }

        let request = ApiRequest::get_image(file, file_id, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_image failed: {}",
                response.error_message()
            )));
        }

        tracing::info!(file = ?file, file_id = ?file_id, "Retrieved image info");

        // Extract URL from response and check/download
        let url = response.data.get("url").and_then(|v| v.as_str());
        let local_path = if let Some(url) = url {
            match download_media_if_needed(
                &ctx.config.channels.napcat,
                account_id.as_deref(),
                url,
                None,
                &ctx.config.agents.defaults.workspace,
                None,
            ).await {
                Ok(Some((path, _already_downloaded))) => Some(path),
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to download image");
                    None
                }
            }
        } else {
            None
        };

        let mut result = json!({
            "success": true,
            "info": response.data
        });

        if let Some(f) = file {
            result["file"] = json!(f);
        }
        if let Some(fid) = file_id {
            result["file_id"] = json!(fid);
        }
        if let Some(path) = local_path {
            result["local_path"] = json!(path);
        }

        Ok(result)
    }
}

/// Get record (voice) info tool.
/// Note: Auto-download is controlled by global config `auto_download_media` in message preprocessing.
pub struct NapcatGetRecordTool;

#[async_trait]
impl Tool for NapcatGetRecordTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_record",
            description: build_description(
                "[QQ Voice] Get voice record metadata from QQ voice message. Returns URL, size, and other info. Note: Voice files are auto-downloaded during message preprocessing (controlled by auto_download_media config).",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "Voice file identifier (from QQ voice message segment) - file path, URL or Base64"
                    },
                    "file_id": {
                        "type": "string",
                        "description": "File ID (alternative to file parameter)"
                    },
                    "out_format": {
                        "type": "string",
                        "description": "Output format (required, e.g., 'mp3', 'amr', 'wav')"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["out_format"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("out_format").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: out_format".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_record")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let file = params.get("file").and_then(|v| v.as_str());
        let file_id = params.get("file_id").and_then(|v| v.as_str());
        let out_format = params.get("out_format").and_then(|v| v.as_str()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        if file.is_none() && file_id.is_none() {
            return Err(Error::Validation("Either 'file' or 'file_id' parameter is required".into()));
        }

        let request = ApiRequest::get_record(file, file_id, out_format, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_record failed: {}",
                response.error_message()
            )));
        }

        tracing::info!(file = ?file, file_id = ?file_id, out_format = out_format, "Retrieved record info");

        // Extract URL from response and check/download
        let url = response.data.get("url").and_then(|v| v.as_str());
        let local_path = if let Some(url) = url {
            match download_media_if_needed(
                &ctx.config.channels.napcat,
                account_id.as_deref(),
                url,
                None,
                &ctx.config.agents.defaults.workspace,
                None,
            ).await {
                Ok(Some((path, _already_downloaded))) => Some(path),
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to download record");
                    None
                }
            }
        } else {
            None
        };

        let mut result = json!({
            "success": true,
            "out_format": out_format,
            "info": response.data
        });

        if let Some(f) = file {
            result["file"] = json!(f);
        }
        if let Some(fid) = file_id {
            result["file_id"] = json!(fid);
        }
        if let Some(path) = local_path {
            result["local_path"] = json!(path);
        }

        Ok(result)
    }
}

/// Get video info tool.
/// Note: Auto-download is controlled by global config `auto_download_media` in message preprocessing.
pub struct NapcatGetVideoTool;

#[async_trait]
impl Tool for NapcatGetVideoTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_video",
            description: build_description(
                "[QQ Video] Get video metadata from QQ video message. Returns URL, size, and other info. Note: Videos are auto-downloaded during message preprocessing (controlled by auto_download_media config).",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "Video file identifier (from QQ video message segment)"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["file"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("file").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: file".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_video")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let file = params.get("file").and_then(|v| v.as_str()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_video(file, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_video failed: {}",
                response.error_message()
            )));
        }

        tracing::info!(file = file, "Retrieved video info");

        // Extract URL from response and check/download
        let url = response.data.get("url").and_then(|v| v.as_str());
        let local_path = if let Some(url) = url {
            match download_media_if_needed(
                &ctx.config.channels.napcat,
                account_id.as_deref(),
                url,
                None,
                &ctx.config.agents.defaults.workspace,
                None,
            ).await {
                Ok(Some((path, _already_downloaded))) => Some(path),
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to download video");
                    None
                }
            }
        } else {
            None
        };

        let mut result = json!({
            "success": true,
            "file": file,
            "info": response.data
        });

        if let Some(path) = local_path {
            result["local_path"] = json!(path);
        }

        Ok(result)
    }
}

/// Download file tool.
pub struct NapcatDownloadFileTool;

#[async_trait]
impl Tool for NapcatDownloadFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_download_file",
            description: build_description(
                "[QQ Download] Download files from QQ chat to local workspace. Use this when user sends a file in QQ. The file will be saved to downloads/YYYY-MM-DD_chat_id/ directory. Supports multi-threaded download.",
                RiskLevel::MediumRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "File URL to download (the url field from QQ file message)"
                    },
                    "filename": {
                        "type": "string",
                        "description": "Optional filename to save as. If not provided, will try to extract from URL or generate one."
                    },
                    "chat_id": {
                        "type": "string",
                        "description": "Chat ID for organizing downloads (format: 'user:xxx' or 'group:xxx'). Used to create subdirectory."
                    },
                    "thread_count": {
                        "type": "integer",
                        "description": "Number of threads for parallel download (optional, default 3)"
                    },
                    "headers": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Custom headers for the request (optional)"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["url"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("url").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: url".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_download_file")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let url = params.get("url").and_then(|v| v.as_str()).unwrap();
        let filename = params.get("filename").and_then(|v| v.as_str());
        let chat_id = params.get("chat_id").and_then(|v| v.as_str());
        let account_id = resolve_account_id(&ctx, &params);

        // Use the unified download function that checks for existing file first
        tracing::info!(url = url, chat_id = ?chat_id, "Starting streaming download via NapCat");

        let local_path = download_media_if_needed(
            &ctx.config.channels.napcat,
            account_id.as_deref(),
            url,
            filename,
            &ctx.config.agents.defaults.workspace,
            chat_id,
        ).await?
            .map(|(path, _)| path)
            .ok_or_else(|| Error::Tool("File not downloaded and auto_download_media is disabled".to_string()))?;

        // Extract filename from path for response
        let filename = std::path::Path::new(&local_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("downloaded_file");

        Ok(json!({
            "success": true,
            "url": url,
            "local_path": local_path,
            "filename": filename,
            "message": format!("File downloaded to: {}", local_path)
        }))
    }
}

// =============================================================================
// Tool Registration
// =============================================================================

/// Register all extended tools into the registry.
pub fn register_extend_tools(registry: &mut crate::ToolRegistry) {
    // Forward message
    registry.register(std::sync::Arc::new(NapcatGetForwardMsgTool));

    // Emoji reaction
    registry.register(std::sync::Arc::new(NapcatSetMsgEmojiLikeTool));

    // Mark as read
    registry.register(std::sync::Arc::new(NapcatMarkMsgAsReadTool));

    // Essence message
    registry.register(std::sync::Arc::new(NapcatSetEssenceMsgTool));
    registry.register(std::sync::Arc::new(NapcatDeleteEssenceMsgTool));
    registry.register(std::sync::Arc::new(NapcatGetEssenceMsgListTool));

    // Group extended
    registry.register(std::sync::Arc::new(NapcatGetGroupAtAllRemainTool));

    // Media resources
    registry.register(std::sync::Arc::new(NapcatGetImageTool));
    registry.register(std::sync::Arc::new(NapcatGetRecordTool));
    registry.register(std::sync::Arc::new(NapcatGetVideoTool));
    registry.register(std::sync::Arc::new(NapcatDownloadFileTool));
}