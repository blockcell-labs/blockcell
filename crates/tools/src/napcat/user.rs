//! User info tools for NapCatQQ.
//!
//! This module provides tools for:
//! - User info queries: get_stranger_info, get_friend_list
//! - User actions: send_like, set_friend_remark, delete_friend
//! - Profile operations: get_login_info, set_qq_profile

use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};

use crate::napcat::common::{
    build_description, build_napcat_permissions, check_channel,
    resolve_account_id, RiskLevel, call_api, ApiRequest,
};
use crate::{Tool, ToolContext, ToolSchema};

// =============================================================================
// Account Info Tools
// =============================================================================

/// Get login info tool.
pub struct NapcatGetLoginInfoTool;

#[async_trait]
impl Tool for NapcatGetLoginInfoTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_login_info",
            description: build_description(
                "[QQ Account] Get the bot's QQ login account info (QQ number, nickname). Use this to identify the current bot account.",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
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
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_login_info")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_login_info(None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_login_info failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "info": response.data
        }))
    }
}

/// Get status tool.
pub struct NapcatGetStatusTool;

#[async_trait]
impl Tool for NapcatGetStatusTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_status",
            description: build_description(
                "[QQ System] Get the NapCatQQ running status.",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
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
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_status")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_status(None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_status failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "status": response.data
        }))
    }
}

/// Get version info tool.
pub struct NapcatGetVersionInfoTool;

#[async_trait]
impl Tool for NapcatGetVersionInfoTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_version_info",
            description: build_description(
                "[QQ System] Get NapCatQQ version information.",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
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
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_version_info")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_version_info(None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_version_info failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "version_info": response.data
        }))
    }
}

// =============================================================================
// User Info Tools
// =============================================================================

/// Get stranger info tool.
pub struct NapcatGetStrangerInfoTool;

#[async_trait]
impl Tool for NapcatGetStrangerInfoTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_stranger_info",
            description: build_description(
                "[QQ User] Get a QQ user's profile information by QQ number.",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "user_id": {
                        "type": "string",
                        "description": "User ID (QQ number)"
                    },
                    "no_cache": {
                        "type": "boolean",
                        "description": "Whether to skip cache",
                        "default": false
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["user_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("user_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: user_id".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_stranger_info")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let user_id = params.get("user_id").and_then(|v| v.as_str()).unwrap();
        let no_cache = params.get("no_cache").and_then(|v| v.as_bool()).unwrap_or(false);
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_stranger_info(user_id, no_cache, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_stranger_info failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "user_id": user_id,
            "info": response.data
        }))
    }
}

/// Get friend list tool.
pub struct NapcatGetFriendListTool;

#[async_trait]
impl Tool for NapcatGetFriendListTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_friend_list",
            description: build_description(
                "[QQ Friend] Get the bot's QQ friend list. Returns all friends with their nicknames and user IDs.",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
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
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_friend_list")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_friend_list(None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_friend_list failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "data": response.data
        }))
    }
}

// =============================================================================
// User Action Tools
// =============================================================================

/// Send like tool.
pub struct NapcatSendLikeTool;

#[async_trait]
impl Tool for NapcatSendLikeTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_send_like",
            description: build_description(
                "[QQ User] Send a like (superb) to a QQ user.",
                RiskLevel::LowRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "user_id": {
                        "type": "string",
                        "description": "User ID (QQ number)"
                    },
                    "times": {
                        "type": "integer",
                        "description": "Number of likes to send (1-10)",
                        "default": 1
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["user_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("user_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: user_id".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_send_like")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let user_id = params.get("user_id").and_then(|v| v.as_str()).unwrap();
        let times = params.get("times").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::send_like(user_id, times, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "send_like failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "message": format!("Sent {} like(s) to user {}", times, user_id)
        }))
    }
}

/// Set friend remark tool.
pub struct NapcatSetFriendRemarkTool;

#[async_trait]
impl Tool for NapcatSetFriendRemarkTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_set_friend_remark",
            description: build_description(
                "[QQ Friend] Set a QQ friend's remark (alias).",
                RiskLevel::LowRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "user_id": {
                        "type": "string",
                        "description": "User ID (QQ number)"
                    },
                    "remark": {
                        "type": "string",
                        "description": "New remark/alias for the friend"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["user_id", "remark"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("user_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: user_id".into()));
        }
        if params.get("remark").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: remark".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_set_friend_remark")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let user_id = params.get("user_id").and_then(|v| v.as_str()).unwrap();
        let remark = params.get("remark").and_then(|v| v.as_str()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::set_friend_remark(user_id, remark, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "set_friend_remark failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "message": format!("Set remark '{}' for friend {}", remark, user_id)
        }))
    }
}

/// Delete friend tool.
pub struct NapcatDeleteFriendTool;

#[async_trait]
impl Tool for NapcatDeleteFriendTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_delete_friend",
            description: build_description(
                "[QQ Friend] Delete a QQ friend. This is a high-risk operation.",
                RiskLevel::HighRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "user_id": {
                        "type": "string",
                        "description": "User ID (QQ number) to delete"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["user_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("user_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: user_id".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_delete_friend")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let user_id = params.get("user_id").and_then(|v| v.as_str()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::delete_friend(user_id, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "delete_friend failed: {}",
                response.error_message()
            )));
        }

        tracing::warn!(user_id = user_id, "Deleted friend");

        Ok(json!({
            "success": true,
            "message": format!("Friend {} has been deleted", user_id)
        }))
    }
}

/// Set QQ profile tool.
pub struct NapcatSetQQProfileTool;

#[async_trait]
impl Tool for NapcatSetQQProfileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_set_qq_profile",
            description: build_description(
                "[QQ Profile] Set the bot's QQ profile information (nickname, signature, gender).",
                RiskLevel::LowRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "nickname": {
                        "type": "string",
                        "description": "New nickname"
                    },
                    "personal_note": {
                        "type": "string",
                        "description": "Personal note/signature"
                    },
                    "sex": {
                        "type": "string",
                        "description": "Gender: 'male', 'female', or 'unknown'"
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
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_set_qq_profile")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let nickname = params.get("nickname").and_then(|v| v.as_str());
        let personal_note = params.get("personal_note").and_then(|v| v.as_str());
        let sex = params.get("sex").and_then(|v| v.as_str());
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::set_qq_profile(nickname, personal_note, sex, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "set_qq_profile failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "message": "Profile updated"
        }))
    }
}

/// Register all user tools into the registry.
pub fn register_user_tools(registry: &mut crate::ToolRegistry) {
    // Account info tools
    registry.register(std::sync::Arc::new(NapcatGetLoginInfoTool));
    registry.register(std::sync::Arc::new(NapcatGetStatusTool));
    registry.register(std::sync::Arc::new(NapcatGetVersionInfoTool));

    // User info tools
    registry.register(std::sync::Arc::new(NapcatGetStrangerInfoTool));
    registry.register(std::sync::Arc::new(NapcatGetFriendListTool));

    // User action tools
    registry.register(std::sync::Arc::new(NapcatSendLikeTool));
    registry.register(std::sync::Arc::new(NapcatSetFriendRemarkTool));
    registry.register(std::sync::Arc::new(NapcatDeleteFriendTool));
    registry.register(std::sync::Arc::new(NapcatSetQQProfileTool));
}