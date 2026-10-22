//! Group management tools for NapCatQQ.
//!
//! This module provides tools for:
//! - Group info queries: get_group_list, get_group_info, get_group_member_list, get_group_member_info
//! - Group administration: set_group_kick, set_group_ban, set_group_whole_ban, set_group_admin
//! - Group settings: set_group_card, set_group_name, set_group_special_title, set_group_leave

use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};

use crate::napcat::common::{
    build_description, build_napcat_permissions, call_api, check_channel, get_sender_id,
    resolve_account_id, ApiRequest, NapCatPermissionChecker, PermissionResult, RiskLevel,
};
use crate::{Tool, ToolContext, ToolSchema};

// =============================================================================
// Group Query Tools
// =============================================================================

/// Get group list tool.
pub struct NapcatGetGroupListTool;

#[async_trait]
impl Tool for NapcatGetGroupListTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_group_list".to_string(),
            description: build_description(
                "[QQ Group] Get the list of QQ groups the bot has joined. Returns all groups with their names and group IDs.",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios, defaults to current account)"
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
        build_napcat_permissions("napcat_get_group_list")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_group_list(None);
        let response =
            call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_group_list failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "data": response.data
        }))
    }
}

/// Get group info tool.
pub struct NapcatGetGroupInfoTool;

#[async_trait]
impl Tool for NapcatGetGroupInfoTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_group_info".to_string(),
            description: build_description(
                "[QQ Group] Get detailed info about a QQ group (name, member count, admin list, etc.).",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "group_id": {
                        "type": "string",
                        "description": "Group ID (QQ group number)"
                    },
                    "no_cache": {
                        "type": "boolean",
                        "description": "Whether to skip cache and fetch fresh data",
                        "default": false
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
            return Err(Error::Validation(
                "Missing required parameter: group_id".into(),
            ));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_group_info")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let group_id = params.get("group_id").and_then(|v| v.as_str()).unwrap();
        let no_cache = params
            .get("no_cache")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_group_info(group_id, no_cache, None);
        let response =
            call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_group_info failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "group_id": group_id,
            "info": response.data
        }))
    }
}

/// Get group member list tool.
pub struct NapcatGetGroupMemberListTool;

#[async_trait]
impl Tool for NapcatGetGroupMemberListTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_group_member_list".to_string(),
            description: build_description(
                "[QQ Group] Get the list of members in a specific QQ group.",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "group_id": {
                        "type": "string",
                        "description": "Group ID (QQ group number)"
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
            return Err(Error::Validation(
                "Missing required parameter: group_id".into(),
            ));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_group_member_list")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let group_id = params.get("group_id").and_then(|v| v.as_str()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_group_member_list(group_id, None);
        let response =
            call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_group_member_list failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "group_id": group_id,
            "data": response.data
        }))
    }
}

/// Get group member info tool.
pub struct NapcatGetGroupMemberInfoTool;

#[async_trait]
impl Tool for NapcatGetGroupMemberInfoTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_group_member_info".to_string(),
            description: build_description(
                "[QQ Group] Get detailed information about a specific QQ group member.",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "group_id": {
                        "type": "string",
                        "description": "Group ID (QQ group number)"
                    },
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
                "required": ["group_id", "user_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("group_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: group_id".into(),
            ));
        }
        if params.get("user_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: user_id".into(),
            ));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_group_member_info")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let group_id = params.get("group_id").and_then(|v| v.as_str()).unwrap();
        let user_id = params.get("user_id").and_then(|v| v.as_str()).unwrap();
        let no_cache = params
            .get("no_cache")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_group_member_info(group_id, user_id, no_cache, None);
        let response =
            call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_group_member_info failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "group_id": group_id,
            "user_id": user_id,
            "info": response.data
        }))
    }
}

// =============================================================================
// Group Admin Tools
// =============================================================================

/// Base function to check admin permission for group operations.
fn check_admin_permission(ctx: &ToolContext, tool_name: &str, params: &Value) -> Result<()> {
    let napcat_config = &ctx.config.channels.napcat;
    let checker = NapCatPermissionChecker::new(napcat_config);

    // Get sender_id from context (set by channel handler when processing inbound messages)
    let sender_id = get_sender_id(ctx);

    let group_id = params.get("group_id").and_then(|v| v.as_str());

    // sender_role is forwarded through ToolContext from the inbound message
    // metadata ("sender_role" set by the napcat channel for group events).
    // When absent, check_role defaults to "member" (fail-closed).
    let sender_role: Option<&str> = ctx.sender_role.as_deref();

    tracing::debug!(
        tool = tool_name,
        sender_id = sender_id,
        group_id = group_id.unwrap_or("none"),
        sender_role = ?sender_role,
        "Checking admin permission"
    );

    match checker.check_permission(tool_name, &sender_id, group_id, sender_role)? {
        PermissionResult::Allowed => Ok(()),
        PermissionResult::Denied(reason) => {
            tracing::warn!(tool = tool_name, reason = reason, "Permission denied");
            Err(Error::Tool(format!("Permission denied: {}", reason)))
        }
        PermissionResult::NeedsConfirmation => {
            // Fail closed: a "needs confirmation" verdict means the caller is not
            // cleared to run this group-management action. It used to be logged and
            // then allowed, which silently voided `require_confirmation`.
            tracing::warn!(tool = tool_name, "Operation requires confirmation; denying");
            Err(Error::Tool(format!(
                "Permission denied: {} requires explicit confirmation",
                tool_name
            )))
        }
    }
}

/// Set group kick tool (remove member).
pub struct NapcatSetGroupKickTool;

#[async_trait]
impl Tool for NapcatSetGroupKickTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_set_group_kick".to_string(),
            description: build_description(
                "[QQ Group] Kick (remove) a member from a QQ group. This is a high-risk operation. Requires admin permissions.",
                RiskLevel::HighRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "group_id": {
                        "type": "string",
                        "description": "Group ID (QQ group number)"
                    },
                    "user_id": {
                        "type": "string",
                        "description": "User ID to kick (QQ number)"
                    },
                    "reject_add_request": {
                        "type": "boolean",
                        "description": "Whether to reject future group join requests from this user (default: false)"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["group_id", "user_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("group_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: group_id".into(),
            ));
        }
        if params.get("user_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: user_id".into(),
            ));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_set_group_kick")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;
        check_admin_permission(&ctx, "napcat_set_group_kick", &params)?;

        let group_id = params.get("group_id").and_then(|v| v.as_str()).unwrap();
        let user_id = params.get("user_id").and_then(|v| v.as_str()).unwrap();
        let reject_add_request = params.get("reject_add_request").and_then(|v| v.as_bool());
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::set_group_kick(group_id, user_id, reject_add_request, None);
        let response =
            call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "set_group_kick failed: {}",
                response.error_message()
            )));
        }

        tracing::info!(
            group_id = group_id,
            user_id = user_id,
            reject_add_request = ?reject_add_request,
            "Kicked user from group"
        );

        Ok(json!({
            "success": true,
            "message": format!("User {} has been kicked from group {}", user_id, group_id)
        }))
    }
}

/// Set group ban tool (mute member).
pub struct NapcatSetGroupBanTool;

#[async_trait]
impl Tool for NapcatSetGroupBanTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_set_group_ban".to_string(),
            description: build_description(
                "[QQ Group] Ban (mute) a member in a QQ group. Set duration to 0 to unban. Requires admin permissions.",
                RiskLevel::MediumRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "group_id": {
                        "type": "string",
                        "description": "Group ID (QQ group number)"
                    },
                    "user_id": {
                        "type": "string",
                        "description": "User ID to ban (QQ number)"
                    },
                    "duration": {
                        "type": "integer",
                        "description": "Ban duration in seconds (0 to unban)",
                        "default": 1800
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["group_id", "user_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("group_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: group_id".into(),
            ));
        }
        if params.get("user_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: user_id".into(),
            ));
        }
        if let Some(duration) = params.get("duration").and_then(|v| v.as_u64()) {
            if u32::try_from(duration).is_err() {
                return Err(Error::Validation(
                    "duration must fit in an unsigned 32-bit integer".into(),
                ));
            }
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_set_group_ban")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;
        check_admin_permission(&ctx, "napcat_set_group_ban", &params)?;

        let group_id = params.get("group_id").and_then(|v| v.as_str()).unwrap();
        let user_id = params.get("user_id").and_then(|v| v.as_str()).unwrap();
        let duration = params
            .get("duration")
            .and_then(|v| v.as_u64())
            .unwrap_or(1800);
        let duration = u32::try_from(duration)
            .map_err(|_| Error::Validation("duration is too large".into()))?;
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::set_group_ban(group_id, user_id, duration, None);
        let response =
            call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "set_group_ban failed: {}",
                response.error_message()
            )));
        }

        let action = if duration == 0 { "unbanned" } else { "banned" };
        tracing::info!(
            group_id = group_id,
            user_id = user_id,
            duration = duration,
            "User {} in group",
            action
        );

        Ok(json!({
            "success": true,
            "message": format!("User {} has been {} in group {} for {} seconds",
                user_id, action, group_id, duration)
        }))
    }
}

/// Set group whole ban tool (mute all).
pub struct NapcatSetGroupWholeBanTool;

#[async_trait]
impl Tool for NapcatSetGroupWholeBanTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_set_group_whole_ban".to_string(),
            description: build_description(
                "[QQ Group] Enable or disable whole-group mute (mute all members). Requires admin permissions.",
                RiskLevel::MediumRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "group_id": {
                        "type": "string",
                        "description": "Group ID (QQ group number)"
                    },
                    "enable": {
                        "type": "boolean",
                        "description": "true to enable whole-group mute, false to disable"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["group_id", "enable"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("group_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: group_id".into(),
            ));
        }
        if params.get("enable").and_then(|v| v.as_bool()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: enable".into(),
            ));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_set_group_whole_ban")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;
        check_admin_permission(&ctx, "napcat_set_group_whole_ban", &params)?;

        let group_id = params.get("group_id").and_then(|v| v.as_str()).unwrap();
        let enable = params.get("enable").and_then(|v| v.as_bool()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::set_group_whole_ban(group_id, enable, None);
        let response =
            call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "set_group_whole_ban failed: {}",
                response.error_message()
            )));
        }

        let action = if enable { "enabled" } else { "disabled" };
        tracing::info!(group_id = group_id, enable = enable, "Whole ban {}", action);

        Ok(json!({
            "success": true,
            "message": format!("Whole-group mute {} for group {}", action, group_id)
        }))
    }
}

/// Set group admin tool.
pub struct NapcatSetGroupAdminTool;

#[async_trait]
impl Tool for NapcatSetGroupAdminTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_set_group_admin".to_string(),
            description: build_description(
                "[QQ Group] Set or remove a member as QQ group admin. Requires owner permissions.",
                RiskLevel::MediumRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "group_id": {
                        "type": "string",
                        "description": "Group ID (QQ group number)"
                    },
                    "user_id": {
                        "type": "string",
                        "description": "User ID (QQ number)"
                    },
                    "enable": {
                        "type": "boolean",
                        "description": "true to set as admin, false to remove admin"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["group_id", "user_id", "enable"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("group_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: group_id".into(),
            ));
        }
        if params.get("user_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: user_id".into(),
            ));
        }
        if params.get("enable").and_then(|v| v.as_bool()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: enable".into(),
            ));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_set_group_admin")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;
        check_admin_permission(&ctx, "napcat_set_group_admin", &params)?;

        let group_id = params.get("group_id").and_then(|v| v.as_str()).unwrap();
        let user_id = params.get("user_id").and_then(|v| v.as_str()).unwrap();
        let enable = params.get("enable").and_then(|v| v.as_bool()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::set_group_admin(group_id, user_id, enable, None);
        let response =
            call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "set_group_admin failed: {}",
                response.error_message()
            )));
        }

        let action = if enable {
            "set as admin"
        } else {
            "removed as admin"
        };
        tracing::info!(
            group_id = group_id,
            user_id = user_id,
            enable = enable,
            "User {}",
            action
        );

        Ok(json!({
            "success": true,
            "message": format!("User {} has been {} in group {}", user_id, action, group_id)
        }))
    }
}

/// Set group card tool.
pub struct NapcatSetGroupCardTool;

#[async_trait]
impl Tool for NapcatSetGroupCardTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_set_group_card".to_string(),
            description: build_description(
                "[QQ Group] Set a member's group card (nickname in QQ group).",
                RiskLevel::LowRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "group_id": {
                        "type": "string",
                        "description": "Group ID (QQ group number)"
                    },
                    "user_id": {
                        "type": "string",
                        "description": "User ID (QQ number)"
                    },
                    "card": {
                        "type": "string",
                        "description": "New group card (nickname)"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["group_id", "user_id", "card"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("group_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: group_id".into(),
            ));
        }
        if params.get("user_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: user_id".into(),
            ));
        }
        if params.get("card").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: card".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_set_group_card")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;
        check_admin_permission(&ctx, "napcat_set_group_card", &params)?;

        let group_id = params.get("group_id").and_then(|v| v.as_str()).unwrap();
        let user_id = params.get("user_id").and_then(|v| v.as_str()).unwrap();
        let card = params.get("card").and_then(|v| v.as_str()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::set_group_card(group_id, user_id, card, None);
        let response =
            call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "set_group_card failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "message": format!("Group card for user {} set to '{}' in group {}", user_id, card, group_id)
        }))
    }
}

/// Set group name tool.
pub struct NapcatSetGroupNameTool;

#[async_trait]
impl Tool for NapcatSetGroupNameTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_set_group_name".to_string(),
            description: build_description(
                "[QQ Group] Set the QQ group name. Requires admin permissions.",
                RiskLevel::MediumRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "group_id": {
                        "type": "string",
                        "description": "Group ID (QQ group number)"
                    },
                    "group_name": {
                        "type": "string",
                        "description": "New group name"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["group_id", "group_name"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("group_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: group_id".into(),
            ));
        }
        if params.get("group_name").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: group_name".into(),
            ));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_set_group_name")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;
        check_admin_permission(&ctx, "napcat_set_group_name", &params)?;

        let group_id = params.get("group_id").and_then(|v| v.as_str()).unwrap();
        let group_name = params.get("group_name").and_then(|v| v.as_str()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::set_group_name(group_id, group_name, None);
        let response =
            call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "set_group_name failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "message": format!("Group {} name set to '{}'", group_id, group_name)
        }))
    }
}

/// Set group special title tool.
pub struct NapcatSetGroupSpecialTitleTool;

#[async_trait]
impl Tool for NapcatSetGroupSpecialTitleTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_set_group_special_title".to_string(),
            description: build_description(
                "[QQ Group] Set a member's special title in QQ group. Requires admin permissions.",
                RiskLevel::MediumRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "group_id": {
                        "type": "string",
                        "description": "Group ID (QQ group number)"
                    },
                    "user_id": {
                        "type": "string",
                        "description": "User ID (QQ number)"
                    },
                    "special_title": {
                        "type": "string",
                        "description": "Special title to set"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["group_id", "user_id", "special_title"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("group_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: group_id".into(),
            ));
        }
        if params.get("user_id").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: user_id".into(),
            ));
        }
        if params
            .get("special_title")
            .and_then(|v| v.as_str())
            .is_none()
        {
            return Err(Error::Validation(
                "Missing required parameter: special_title".into(),
            ));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_set_group_special_title")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;
        check_admin_permission(&ctx, "napcat_set_group_special_title", &params)?;

        let group_id = params.get("group_id").and_then(|v| v.as_str()).unwrap();
        let user_id = params.get("user_id").and_then(|v| v.as_str()).unwrap();
        let special_title = params
            .get("special_title")
            .and_then(|v| v.as_str())
            .unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::set_group_special_title(group_id, user_id, special_title, None);
        let response =
            call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "set_group_special_title failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "message": format!("Special title '{}' set for user {} in group {}", special_title, user_id, group_id)
        }))
    }
}

/// Set group leave tool.
pub struct NapcatSetGroupLeaveTool;

#[async_trait]
impl Tool for NapcatSetGroupLeaveTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_set_group_leave".to_string(),
            description: build_description(
                "[QQ Group] Make the bot leave a QQ group. High risk operation.",
                RiskLevel::HighRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "group_id": {
                        "type": "string",
                        "description": "Group ID (QQ group number)"
                    },
                    "is_dismiss": {
                        "type": "boolean",
                        "description": "Whether to dismiss the group (owner only)",
                        "default": false
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
            return Err(Error::Validation(
                "Missing required parameter: group_id".into(),
            ));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_set_group_leave")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;
        check_admin_permission(&ctx, "napcat_set_group_leave", &params)?;

        let group_id = params.get("group_id").and_then(|v| v.as_str()).unwrap();
        let is_dismiss = params
            .get("is_dismiss")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::set_group_leave(group_id, is_dismiss, None);
        let response =
            call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "set_group_leave failed: {}",
                response.error_message()
            )));
        }

        tracing::warn!(
            group_id = group_id,
            is_dismiss = is_dismiss,
            "Bot left group"
        );

        Ok(json!({
            "success": true,
            "message": format!("Bot has left group {}", group_id)
        }))
    }
}

/// Register all group tools into the registry.
pub fn register_group_tools(registry: &mut crate::ToolRegistry) {
    // Query tools
    registry.register(std::sync::Arc::new(NapcatGetGroupListTool));
    registry.register(std::sync::Arc::new(NapcatGetGroupInfoTool));
    registry.register(std::sync::Arc::new(NapcatGetGroupMemberListTool));
    registry.register(std::sync::Arc::new(NapcatGetGroupMemberInfoTool));

    // Admin tools
    registry.register(std::sync::Arc::new(NapcatSetGroupKickTool));
    registry.register(std::sync::Arc::new(NapcatSetGroupBanTool));
    registry.register(std::sync::Arc::new(NapcatSetGroupWholeBanTool));
    registry.register(std::sync::Arc::new(NapcatSetGroupAdminTool));

    // Settings tools
    registry.register(std::sync::Arc::new(NapcatSetGroupCardTool));
    registry.register(std::sync::Arc::new(NapcatSetGroupNameTool));
    registry.register(std::sync::Arc::new(NapcatSetGroupSpecialTitleTool));
    registry.register(std::sync::Arc::new(NapcatSetGroupLeaveTool));
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::config::channels::ToolPermissionOverride;
    use std::path::PathBuf;

    fn ctx_with_role(sender_role: Option<&str>) -> ToolContext {
        let mut config = blockcell_core::Config::default();
        config.channels.napcat.admin_permissions.default_policy = "allow".to_string();
        config
            .channels
            .napcat
            .admin_permissions
            .tool_overrides
            .insert(
                "napcat_set_group_kick".to_string(),
                ToolPermissionOverride {
                    allowed_admins: None,
                    allowed_groups: None,
                    default_policy: None,
                    require_confirmation: false,
                    require_role: Some("admin".to_string()),
                },
            );
        ToolContext {
            workspace: PathBuf::from("/tmp/workspace"),
            base: PathBuf::from("/tmp/blockcell"),
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "napcat:g1".to_string(),
            channel: "napcat".to_string(),
            account_id: None,
            sender_id: Some("u1".to_string()),
            sender_role: sender_role.map(str::to_string),
            chat_id: "g1".to_string(),
            config,
            permissions: blockcell_core::types::PermissionSet::new(),
            task_manager: None,
            memory_store: None,
            memory_file_store: None,
            ghost_memory_lifecycle: None,
            skill_file_store: None,
            session_search: None,
            outbound_tx: None,
            spawn_handle: None,
            capability_registry: None,
            core_evolution: None,
            event_emitter: None,
            channel_contacts_file: None,
            response_cache: None,
            runtime_handle: None,
            agent_identity: None,
            skill_mutex: None,
            agent_type_registry: None,
            evolution_workflow_store: None,
        }
    }

    /// sender_role 经 ToolContext 透传：admin/owner 通过，member/缺省被拒。
    #[test]
    fn check_admin_permission_uses_context_sender_role() {
        let params = json!({"group_id": "g1"});

        let admin = ctx_with_role(Some("admin"));
        assert!(
            check_admin_permission(&admin, "napcat_set_group_kick", &params).is_ok(),
            "admin role must pass an admin-gated tool"
        );
        let owner = ctx_with_role(Some("owner"));
        assert!(
            check_admin_permission(&owner, "napcat_set_group_kick", &params).is_ok(),
            "owner role must pass an admin-gated tool"
        );

        let member = ctx_with_role(Some("member"));
        let err = check_admin_permission(&member, "napcat_set_group_kick", &params)
            .expect_err("member must be denied");
        assert!(err.to_string().contains("admin"), "{err}");

        // 渠道未提供角色（如私聊/旧事件）时 fail-closed 按 member 处理
        let unknown = ctx_with_role(None);
        assert!(
            check_admin_permission(&unknown, "napcat_set_group_kick", &params).is_err(),
            "missing role must fail closed"
        );
    }
}
