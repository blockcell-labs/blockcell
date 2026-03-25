//! Message operation tools for NapCatQQ.
//!
//! This module provides tools for:
//! - Message sending: send_private_msg, send_group_msg
//! - Message management: delete_msg (recall), get_msg
//! - Request handling: set_friend_add_request, set_group_add_request
//!
//! All tools support both WebSocket and HTTP modes based on configuration.

use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};

use crate::napcat::common::{
    build_description, build_napcat_permissions, check_channel,
    resolve_account_id, call_api, RiskLevel, ApiRequest,
};
use crate::{Tool, ToolContext, ToolSchema};

// =============================================================================
// Message Tools
// =============================================================================

/// Delete message tool (recall).
pub struct NapcatDeleteMsgTool;

#[async_trait]
impl Tool for NapcatDeleteMsgTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_delete_msg",
            description: build_description(
                "[QQ Message] Recall (delete) a QQ message that was sent. Risk: Medium.",
                RiskLevel::MediumRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "message_id": {
                        "type": "integer",
                        "description": "Message ID to recall"
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
        build_napcat_permissions("napcat_delete_msg")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let message_id = params.get("message_id").and_then(|v| v.as_i64()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::delete_msg(message_id, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "delete_msg failed: {}",
                response.error_message()
            )));
        }

        tracing::info!(message_id = message_id, "Message recalled");

        Ok(json!({
            "success": true,
            "message": format!("Message {} has been recalled", message_id)
        }))
    }
}

/// Get message tool.
pub struct NapcatGetMsgTool;

#[async_trait]
impl Tool for NapcatGetMsgTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_msg",
            description: build_description(
                "[QQ Message] Get QQ message details by message ID.",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "message_id": {
                        "type": "integer",
                        "description": "Message ID"
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
        build_napcat_permissions("napcat_get_msg")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let message_id = params.get("message_id").and_then(|v| v.as_i64()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_msg(message_id, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_msg failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "message_id": message_id,
            "message": response.data
        }))
    }
}

/// Set friend add request tool.
pub struct NapcatSetFriendAddRequestTool;

#[async_trait]
impl Tool for NapcatSetFriendAddRequestTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_set_friend_add_request",
            description: build_description(
                "[QQ Friend] Handle a QQ friend add request (approve or reject). Risk: Medium.",
                RiskLevel::MediumRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "flag": {
                        "type": "string",
                        "description": "Request flag from the friend request event"
                    },
                    "approve": {
                        "type": "boolean",
                        "description": "true to approve, false to reject"
                    },
                    "remark": {
                        "type": "string",
                        "description": "Friend remark to set when approving (optional)"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["flag", "approve"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("flag").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: flag".into()));
        }
        if params.get("approve").and_then(|v| v.as_bool()).is_none() {
            return Err(Error::Validation("Missing required parameter: approve".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_set_friend_add_request")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let flag = params.get("flag").and_then(|v| v.as_str()).unwrap();
        let approve = params.get("approve").and_then(|v| v.as_bool()).unwrap();
        let remark = params.get("remark").and_then(|v| v.as_str());
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::set_friend_add_request(flag, approve, remark, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "set_friend_add_request failed: {}",
                response.error_message()
            )));
        }

        let action = if approve { "approved" } else { "rejected" };
        tracing::info!(flag = flag, approve = approve, remark = ?remark, "Friend request {}", action);

        Ok(json!({
            "success": true,
            "message": format!("Friend request has been {}", action)
        }))
    }
}

/// Set group add request tool.
pub struct NapcatSetGroupAddRequestTool;

#[async_trait]
impl Tool for NapcatSetGroupAddRequestTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_set_group_add_request",
            description: build_description(
                "[QQ Group] Handle a QQ group join request (approve or reject). Risk: Medium.",
                RiskLevel::MediumRisk,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "flag": {
                        "type": "string",
                        "description": "Request flag from the group request event"
                    },
                    "sub_type": {
                        "type": "string",
                        "description": "Request subtype: 'add' or 'invite'"
                    },
                    "approve": {
                        "type": "boolean",
                        "description": "true to approve, false to reject"
                    },
                    "reason": {
                        "type": "string",
                        "description": "Reason for rejection (optional, used when approve is false)"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["flag", "sub_type", "approve"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("flag").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: flag".into()));
        }
        if params.get("sub_type").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: sub_type".into()));
        }
        if params.get("approve").and_then(|v| v.as_bool()).is_none() {
            return Err(Error::Validation("Missing required parameter: approve".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_set_group_add_request")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let flag = params.get("flag").and_then(|v| v.as_str()).unwrap();
        let sub_type = params.get("sub_type").and_then(|v| v.as_str()).unwrap();
        let approve = params.get("approve").and_then(|v| v.as_bool()).unwrap();
        let reason = params.get("reason").and_then(|v| v.as_str());
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::set_group_add_request(flag, sub_type, approve, reason, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "set_group_add_request failed: {}",
                response.error_message()
            )));
        }

        let action = if approve { "approved" } else { "rejected" };
        tracing::info!(flag = flag, sub_type = sub_type, reason = ?reason, "Group request {}", action);

        Ok(json!({
            "success": true,
            "message": format!("Group {} request has been {}", sub_type, action)
        }))
    }
}

/// Get cookies tool.
pub struct NapcatGetCookiesTool;

#[async_trait]
impl Tool for NapcatGetCookiesTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_cookies",
            description: build_description(
                "[QQ System] Get QQ cookies for a specific domain.",
                RiskLevel::ReadOnly,
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "domain": {
                        "type": "string",
                        "description": "Domain to get cookies for"
                    },
                    "account_id": {
                        "type": "string",
                        "description": "Account ID (for multi-account scenarios)"
                    }
                },
                "required": ["domain"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("domain").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: domain".into()));
        }
        Ok(())
    }

    fn required_permissions(&self, _params: &Value) -> blockcell_core::types::PermissionSet {
        build_napcat_permissions("napcat_get_cookies")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let domain = params.get("domain").and_then(|v| v.as_str()).unwrap();
        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_cookies(domain, None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_cookies failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "domain": domain,
            "data": response.data
        }))
    }
}

/// Get CSRF token tool.
pub struct NapcatGetCsrfTokenTool;

#[async_trait]
impl Tool for NapcatGetCsrfTokenTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "napcat_get_csrf_token",
            description: build_description(
                "[QQ System] Get QQ CSRF token for API authentication.",
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
        build_napcat_permissions("napcat_get_csrf_token")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        check_channel(&ctx)?;

        let account_id = resolve_account_id(&ctx, &params);

        let request = ApiRequest::get_csrf_token(None);
        let response = call_api(&ctx.config.channels.napcat, account_id.as_deref(), request).await?;

        if !response.is_success() {
            return Err(Error::Tool(format!(
                "get_csrf_token failed: {}",
                response.error_message()
            )));
        }

        Ok(json!({
            "success": true,
            "data": response.data
        }))
    }
}

/// Register all message tools into the registry.
pub fn register_message_tools(registry: &mut crate::ToolRegistry) {
    registry.register(std::sync::Arc::new(NapcatDeleteMsgTool));
    registry.register(std::sync::Arc::new(NapcatGetMsgTool));
    registry.register(std::sync::Arc::new(NapcatSetFriendAddRequestTool));
    registry.register(std::sync::Arc::new(NapcatSetGroupAddRequestTool));
    registry.register(std::sync::Arc::new(NapcatGetCookiesTool));
    registry.register(std::sync::Arc::new(NapcatGetCsrfTokenTool));
}