//! OneBot 11 protocol type definitions.
//!
//! This module defines the core types used in the OneBot 11 protocol,
//! including API requests, responses, and return codes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// API request structure for OneBot 11.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiRequest {
    /// Action name (e.g., "send_private_msg", "send_group_msg").
    pub action: String,
    /// Request parameters.
    #[serde(default)]
    pub params: Value,
    /// Request identifier for matching responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub echo: Option<String>,
}

impl ApiRequest {
    /// Create a send private message request.
    ///
    /// # Arguments
    /// * `user_id` - 用户QQ
    /// * `message` - 消息内容
    /// * `auto_escape` - 是否作为纯文本发送 (optional)
    pub fn send_private_msg(user_id: &str, message: &Value, auto_escape: Option<bool>, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({
            "user_id": user_id,
            "message": message,
        });
        if let Some(escape) = auto_escape {
            params["auto_escape"] = serde_json::json!(escape);
        }
        Self {
            action: "send_private_msg".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a send group message request.
    ///
    /// # Arguments
    /// * `group_id` - 群号
    /// * `message` - 消息内容
    /// * `auto_escape` - 是否作为纯文本发送 (optional)
    pub fn send_group_msg(group_id: &str, message: &Value, auto_escape: Option<bool>, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({
            "group_id": group_id,
            "message": message,
        });
        if let Some(escape) = auto_escape {
            params["auto_escape"] = serde_json::json!(escape);
        }
        Self {
            action: "send_group_msg".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get login info request.
    pub fn get_login_info(echo: Option<&str>) -> Self {
        Self {
            action: "get_login_info".to_string(),
            params: serde_json::json!({}),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get group list request.
    pub fn get_group_list(echo: Option<&str>) -> Self {
        Self {
            action: "get_group_list".to_string(),
            params: serde_json::json!({}),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get friend list request.
    pub fn get_friend_list(echo: Option<&str>) -> Self {
        Self {
            action: "get_friend_list".to_string(),
            params: serde_json::json!({}),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a delete (recall) message request.
    pub fn delete_msg(message_id: i64, echo: Option<&str>) -> Self {
        Self {
            action: "delete_msg".to_string(),
            params: serde_json::json!({
                "message_id": message_id,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get message request.
    pub fn get_msg(message_id: i64, echo: Option<&str>) -> Self {
        Self {
            action: "get_msg".to_string(),
            params: serde_json::json!({
                "message_id": message_id,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a set group kick request.
    ///
    /// # Arguments
    /// * `group_id` - 群号
    /// * `user_id` - 用户QQ
    /// * `reject_add_request` - 是否拒绝加群请求 (optional, default: false)
    pub fn set_group_kick(group_id: &str, user_id: &str, reject_add_request: Option<bool>, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({
            "group_id": group_id,
            "user_id": user_id,
        });
        if let Some(reject) = reject_add_request {
            params["reject_add_request"] = serde_json::json!(reject);
        }
        Self {
            action: "set_group_kick".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a set group ban request.
    pub fn set_group_ban(
        group_id: &str,
        user_id: &str,
        duration: u32,
        echo: Option<&str>,
    ) -> Self {
        Self {
            action: "set_group_ban".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
                "user_id": user_id,
                "duration": duration,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    // =========================================================================
    // Group Management API Requests
    // =========================================================================

    /// Create a set group admin request.
    pub fn set_group_admin(group_id: &str, user_id: &str, enable: bool, echo: Option<&str>) -> Self {
        Self {
            action: "set_group_admin".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
                "user_id": user_id,
                "enable": enable,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a set group card request.
    pub fn set_group_card(group_id: &str, user_id: &str, card: &str, echo: Option<&str>) -> Self {
        Self {
            action: "set_group_card".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
                "user_id": user_id,
                "card": card,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a set group name request.
    pub fn set_group_name(group_id: &str, group_name: &str, echo: Option<&str>) -> Self {
        Self {
            action: "set_group_name".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
                "group_name": group_name,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get group member info request.
    pub fn get_group_member_info(group_id: &str, user_id: &str, no_cache: bool, echo: Option<&str>) -> Self {
        Self {
            action: "get_group_member_info".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
                "user_id": user_id,
                "no_cache": no_cache,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get group member list request.
    pub fn get_group_member_list(group_id: &str, echo: Option<&str>) -> Self {
        Self {
            action: "get_group_member_list".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get group info request.
    pub fn get_group_info(group_id: &str, no_cache: bool, echo: Option<&str>) -> Self {
        Self {
            action: "get_group_info".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
                "no_cache": no_cache,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a set group whole ban request.
    pub fn set_group_whole_ban(group_id: &str, enable: bool, echo: Option<&str>) -> Self {
        Self {
            action: "set_group_whole_ban".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
                "enable": enable,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a set group leave request.
    pub fn set_group_leave(group_id: &str, is_dismiss: bool, echo: Option<&str>) -> Self {
        Self {
            action: "set_group_leave".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
                "is_dismiss": is_dismiss,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a set group special title request.
    pub fn set_group_special_title(group_id: &str, user_id: &str, special_title: &str, echo: Option<&str>) -> Self {
        Self {
            action: "set_group_special_title".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
                "user_id": user_id,
                "special_title": special_title,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    // =========================================================================
    // User Info API Requests
    // =========================================================================

    /// Create a get stranger info request.
    pub fn get_stranger_info(user_id: &str, no_cache: bool, echo: Option<&str>) -> Self {
        Self {
            action: "get_stranger_info".to_string(),
            params: serde_json::json!({
                "user_id": user_id,
                "no_cache": no_cache,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a send like request.
    pub fn send_like(user_id: &str, times: u32, echo: Option<&str>) -> Self {
        Self {
            action: "send_like".to_string(),
            params: serde_json::json!({
                "user_id": user_id,
                "times": times,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a set friend remark request.
    pub fn set_friend_remark(user_id: &str, remark: &str, echo: Option<&str>) -> Self {
        Self {
            action: "set_friend_remark".to_string(),
            params: serde_json::json!({
                "user_id": user_id,
                "remark": remark,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a delete friend request.
    pub fn delete_friend(user_id: &str, echo: Option<&str>) -> Self {
        Self {
            action: "delete_friend".to_string(),
            params: serde_json::json!({
                "user_id": user_id,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    // =========================================================================
    // File API Requests
    // =========================================================================

    /// Create an upload file request (group).
    pub fn upload_group_file(group_id: &str, file: &str, name: Option<&str>, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({
            "group_id": group_id,
            "file": file,
        });
        if let Some(n) = name {
            params["name"] = serde_json::json!(n);
        }
        Self {
            action: "upload_file".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create an upload file request (private).
    pub fn upload_private_file(user_id: &str, file: &str, name: Option<&str>, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({
            "user_id": user_id,
            "file": file,
        });
        if let Some(n) = name {
            params["name"] = serde_json::json!(n);
        }
        Self {
            action: "upload_file".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get file request.
    /// file parameter: file path, URL, or file_id from message event
    pub fn get_file(file: &str, echo: Option<&str>) -> Self {
        Self {
            action: "get_file".to_string(),
            params: serde_json::json!({
                "file": file,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a delete file request.
    pub fn delete_file(group_id: &str, file_id: &str, busid: Option<i32>, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({
            "group_id": group_id,
            "file_id": file_id,
        });
        if let Some(b) = busid {
            params["busid"] = serde_json::json!(b);
        }
        Self {
            action: "delete_file".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get group file system info request.
    pub fn get_group_file_system_info(group_id: &str, echo: Option<&str>) -> Self {
        Self {
            action: "get_group_file_system_info".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get group files by folder request.
    pub fn get_group_files_by_folder(group_id: &str, folder_id: &str, echo: Option<&str>) -> Self {
        Self {
            action: "get_group_files_by_folder".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
                "folder_id": folder_id,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    // =========================================================================
    // Misc API Requests
    // =========================================================================

    /// Create a get status request.
    pub fn get_status(echo: Option<&str>) -> Self {
        Self {
            action: "get_status".to_string(),
            params: serde_json::json!({}),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get version info request.
    pub fn get_version_info(echo: Option<&str>) -> Self {
        Self {
            action: "get_version_info".to_string(),
            params: serde_json::json!({}),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a set QQ profile request.
    pub fn set_qq_profile(nickname: Option<&str>, personal_note: Option<&str>, sex: Option<&str>, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({});
        if let Some(n) = nickname {
            params["nickname"] = serde_json::json!(n);
        }
        if let Some(p) = personal_note {
            params["personal_note"] = serde_json::json!(p);
        }
        if let Some(s) = sex {
            params["sex"] = serde_json::json!(s);
        }
        Self {
            action: "set_qq_profile".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get cookies request.
    pub fn get_cookies(domain: &str, echo: Option<&str>) -> Self {
        Self {
            action: "get_cookies".to_string(),
            params: serde_json::json!({
                "domain": domain,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get CSRF token request.
    pub fn get_csrf_token(echo: Option<&str>) -> Self {
        Self {
            action: "get_csrf_token".to_string(),
            params: serde_json::json!({}),
            echo: echo.map(|s| s.to_string()),
        }
    }

    // =========================================================================
    // Friend/Group Request Handling
    // =========================================================================

    /// Create a set friend add request.
    ///
    /// # Arguments
    /// * `flag` - 加好友请求的 flag (需从上报中获取)
    /// * `approve` - 是否同意请求
    /// * `remark` - 添加后的好友备注 (optional)
    pub fn set_friend_add_request(flag: &str, approve: bool, remark: Option<&str>, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({
            "flag": flag,
            "approve": approve,
        });
        if let Some(r) = remark {
            params["remark"] = serde_json::json!(r);
        }
        Self {
            action: "set_friend_add_request".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a set group add request.
    ///
    /// # Arguments
    /// * `flag` - 请求flag
    /// * `sub_type` - 请求子类型 (add/invite)
    /// * `approve` - 是否同意
    /// * `reason` - 拒绝理由 (optional)
    pub fn set_group_add_request(flag: &str, sub_type: &str, approve: bool, reason: Option<&str>, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({
            "flag": flag,
            "sub_type": sub_type,
            "approve": approve,
        });
        if let Some(r) = reason {
            params["reason"] = serde_json::json!(r);
        }
        Self {
            action: "set_group_add_request".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    // =========================================================================
    // Extended Message API Requests
    // =========================================================================

    /// Create a get forward message request.
    pub fn get_forward_msg(message_id: &str, echo: Option<&str>) -> Self {
        Self {
            action: "get_forward_msg".to_string(),
            params: serde_json::json!({
                "message_id": message_id,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a set message emoji like request (emoji reaction).
    ///
    /// # Arguments
    /// * `message_id` - 消息ID
    /// * `emoji_id` - 表情ID
    /// * `set` - 是否设置 (optional, default: true)
    pub fn set_msg_emoji_like(message_id: i64, emoji_id: &str, set: Option<bool>, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({
            "message_id": message_id,
            "emoji_id": emoji_id,
        });
        if let Some(s) = set {
            params["set"] = serde_json::json!(s);
        }
        Self {
            action: "set_msg_emoji_like".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a mark message as read request.
    pub fn mark_msg_as_read(message_id: i64, echo: Option<&str>) -> Self {
        Self {
            action: "mark_msg_as_read".to_string(),
            params: serde_json::json!({
                "message_id": message_id,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    // =========================================================================
    // Essence Message API Requests
    // =========================================================================

    /// Create a set essence message request.
    pub fn set_essence_msg(message_id: i64, echo: Option<&str>) -> Self {
        Self {
            action: "set_essence_msg".to_string(),
            params: serde_json::json!({
                "message_id": message_id,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a delete essence message request.
    pub fn delete_essence_msg(message_id: i64, echo: Option<&str>) -> Self {
        Self {
            action: "delete_essence_msg".to_string(),
            params: serde_json::json!({
                "message_id": message_id,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get essence message list request.
    pub fn get_essence_msg_list(group_id: &str, echo: Option<&str>) -> Self {
        Self {
            action: "get_essence_msg_list".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    // =========================================================================
    // Group Extended API Requests
    // =========================================================================

    /// Create a get group at all remain request.
    pub fn get_group_at_all_remain(group_id: &str, echo: Option<&str>) -> Self {
        Self {
            action: "get_group_at_all_remain".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a set group portrait (avatar) request.
    pub fn set_group_portrait(group_id: &str, file: &str, echo: Option<&str>) -> Self {
        Self {
            action: "set_group_portrait".to_string(),
            params: serde_json::json!({
                "group_id": group_id,
                "file": file,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    // =========================================================================
    // Media/Resource API Requests
    // =========================================================================

    /// Create a get image request.
    ///
    /// # Arguments
    /// * `file` - 文件路径、URL或Base64 (optional)
    /// * `file_id` - 文件ID (optional)
    pub fn get_image(file: Option<&str>, file_id: Option<&str>, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({});
        if let Some(f) = file {
            params["file"] = serde_json::json!(f);
        }
        if let Some(fid) = file_id {
            params["file_id"] = serde_json::json!(fid);
        }
        Self {
            action: "get_image".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get record (voice) request.
    ///
    /// # Arguments
    /// * `file` - 文件路径、URL或Base64 (optional)
    /// * `file_id` - 文件ID (optional)
    /// * `out_format` - 输出格式 (required, e.g., "mp3", "amr")
    pub fn get_record(file: Option<&str>, file_id: Option<&str>, out_format: &str, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({
            "out_format": out_format,
        });
        if let Some(f) = file {
            params["file"] = serde_json::json!(f);
        }
        if let Some(fid) = file_id {
            params["file_id"] = serde_json::json!(fid);
        }
        Self {
            action: "get_record".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get video request.
    pub fn get_video(file: &str, echo: Option<&str>) -> Self {
        Self {
            action: "get_video".to_string(),
            params: serde_json::json!({
                "file": file,
            }),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a get private file URL request.
    /// 获取私聊文件下载链接
    ///
    /// 私聊文件需要先调用此 API 获取实际下载 URL，然后才能下载。
    pub fn get_private_file_url(file_id: &str, user_id: Option<&str>, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({
            "file_id": file_id,
        });
        if let Some(uid) = user_id {
            params["user_id"] = serde_json::json!(uid);
        }
        Self {
            action: "get_private_file_url".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a download file request.
    pub fn download_file(url: &str, thread_count: Option<i32>, headers: Option<&[&str]>, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({
            "url": url,
        });
        if let Some(count) = thread_count {
            params["thread_count"] = serde_json::json!(count);
        }
        if let Some(h) = headers {
            params["headers"] = serde_json::json!(h);
        }
        Self {
            action: "download_file".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a download file stream request.
    /// This API returns file content in chunks via WebSocket or HTTP streaming.
    pub fn download_file_stream(url: &str, thread_count: Option<i32>, headers: Option<&[&str]>, echo: Option<&str>) -> Self {
        let mut params = serde_json::json!({
            "url": url,
        });
        if let Some(count) = thread_count {
            params["thread_count"] = serde_json::json!(count);
        }
        if let Some(h) = headers {
            params["headers"] = serde_json::json!(h);
        }
        Self {
            action: "download_file_stream".to_string(),
            params,
            echo: echo.map(|s| s.to_string()),
        }
    }

    // =========================================================================
    // Capability Check API Requests
    // =========================================================================

    /// Create a can send image check request.
    pub fn can_send_image(echo: Option<&str>) -> Self {
        Self {
            action: "can_send_image".to_string(),
            params: serde_json::json!({}),
            echo: echo.map(|s| s.to_string()),
        }
    }

    /// Create a can send record check request.
    pub fn can_send_record(echo: Option<&str>) -> Self {
        Self {
            action: "can_send_record".to_string(),
            params: serde_json::json!({}),
            echo: echo.map(|s| s.to_string()),
        }
    }
}

/// API response structure for OneBot 11.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiResponse {
    /// Status: "ok" or "failed".
    pub status: String,
    /// Return code.
    pub retcode: i32,
    /// Response data.
    #[serde(default)]
    pub data: Value,
    /// Error message.
    #[serde(default)]
    pub message: String,
    /// Error description (human-readable).
    #[serde(default)]
    pub wording: String,
    /// Request identifier (echo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub echo: Option<String>,
}

impl ApiResponse {
    /// Check if the response indicates success.
    pub fn is_success(&self) -> bool {
        self.status == "ok" && self.retcode == 0
    }

    /// Get the error message.
    pub fn error_message(&self) -> &str {
        if !self.wording.is_empty() {
            &self.wording
        } else if !self.message.is_empty() {
            &self.message
        } else {
            "unknown error"
        }
    }
}

/// Return code constants for OneBot 11.
pub mod retcode {
    /// Success.
    pub const OK: i32 = 0;
    /// Operation is disabled by the implementation.
    pub const DISABLED: i32 = 1;

    // =========================================================================
    // API Error Codes (100-199)
    // =========================================================================
    /// Invalid access token.
    pub const INVALID_TOKEN: i32 = 100;
    /// API not found.
    pub const API_NOT_FOUND: i32 = 102;
    /// API disabled.
    pub const API_DISABLED: i32 = 103;
    /// API not supported.
    pub const API_NOT_SUPPORTED: i32 = 104;

    // =========================================================================
    // Message Error Codes (1200-1299)
    // =========================================================================
    /// Message was blocked by risk control.
    pub const MESSAGE_RISK_CONTROL: i32 = 1200;
    /// User is muted in group.
    pub const USER_MUTED: i32 = 1201;
    /// Message too long.
    pub const MESSAGE_TOO_LONG: i32 = 1202;
    /// Failed to send message.
    pub const SEND_FAILED: i32 = 1203;

    // =========================================================================
    // HTTP Status-like Error Codes (1400-1499)
    // =========================================================================
    /// Client side error (bad request).
    pub const BAD_REQUEST: i32 = 1400;
    /// Unauthorized (access token missing or invalid).
    pub const UNAUTHORIZED: i32 = 1401;
    /// Resource not found.
    pub const NOT_FOUND: i32 = 1404;
    /// Internal server error.
    pub const INTERNAL_ERROR: i32 = 1500;
}

/// Send message response data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageData {
    /// Message ID.
    pub message_id: i64,
    /// Resource ID (for media messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub res_id: Option<String>,
    /// Forward ID (for merged messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_id: Option<String>,
}

/// Login info response data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginInfoData {
    /// Bot QQ number.
    pub user_id: String,
    /// Bot nickname.
    pub nickname: String,
}

/// Group info response data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupInfoData {
    /// Group ID.
    pub group_id: String,
    /// Group name.
    pub group_name: String,
    /// Current member count.
    pub member_count: i32,
    /// Maximum member count.
    pub max_member_count: i32,
}

/// Friend info response data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendInfoData {
    /// Friend QQ number.
    pub user_id: String,
    /// Friend nickname.
    pub nickname: String,
    /// Friend remark (alias).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remark: Option<String>,
}

/// Stream chunk data for download_file_stream API.
/// The response returns file content in Base64-encoded chunks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunkData {
    /// Unique identifier for this stream session.
    pub stream_id: String,
    /// Base64-encoded chunk data.
    pub chunk_data: String,
    /// Current chunk index (0-based).
    pub chunk_index: i32,
    /// Total number of chunks.
    pub total_chunks: i32,
    /// Total file size in bytes.
    #[serde(default)]
    pub file_size: Option<i64>,
}

impl StreamChunkData {
    /// Decode the chunk data from Base64.
    pub fn decode_data(&self) -> Result<Vec<u8>, base64::DecodeError> {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        STANDARD.decode(&self.chunk_data)
    }
}

/// Event post type enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostType {
    /// Message event.
    Message,
    /// Notice event.
    Notice,
    /// Request event.
    Request,
    /// Meta event.
    MetaEvent,
}

impl PostType {
    /// Parse from string.
    pub fn parse(s: &str) -> Self {
        match s {
            "message" => PostType::Message,
            "notice" => PostType::Notice,
            "request" => PostType::Request,
            "meta_event" => PostType::MetaEvent,
            _ => PostType::Message,
        }
    }
}

/// Message type enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    /// Private message.
    Private,
    /// Group message.
    Group,
}

impl MessageType {
    /// Parse from string.
    pub fn parse(s: &str) -> Self {
        match s {
            "private" => MessageType::Private,
            "group" => MessageType::Group,
            _ => MessageType::Private,
        }
    }
}

/// Notice type enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeType {
    /// Group file upload.
    GroupUpload,
    /// Group admin change.
    GroupAdmin,
    /// Group member decrease.
    GroupDecrease,
    /// Group member increase.
    GroupIncrease,
    /// Group ban.
    GroupBan,
    /// Friend add.
    FriendAdd,
    /// Group message recall.
    GroupRecall,
    /// Friend message recall.
    FriendRecall,
    /// Poke/notify.
    Notify,
    /// Other notice.
    Other,
}

impl NoticeType {
    /// Parse from string.
    pub fn parse(s: &str) -> Self {
        match s {
            "group_upload" => NoticeType::GroupUpload,
            "group_admin" => NoticeType::GroupAdmin,
            "group_decrease" => NoticeType::GroupDecrease,
            "group_increase" => NoticeType::GroupIncrease,
            "group_ban" => NoticeType::GroupBan,
            "friend_add" => NoticeType::FriendAdd,
            "group_recall" => NoticeType::GroupRecall,
            "friend_recall" => NoticeType::FriendRecall,
            "notify" => NoticeType::Notify,
            _ => NoticeType::Other,
        }
    }
}

/// Meta event type enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaEventType {
    /// Lifecycle event.
    Lifecycle,
    /// Heartbeat event.
    Heartbeat,
}

impl MetaEventType {
    /// Parse from string.
    pub fn parse(s: &str) -> Self {
        match s {
            "lifecycle" => MetaEventType::Lifecycle,
            "heartbeat" => MetaEventType::Heartbeat,
            _ => MetaEventType::Lifecycle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_request_send_private_msg() {
        let req = ApiRequest::send_private_msg("123456", &serde_json::json!("hello"), None, Some("req1"));
        assert_eq!(req.action, "send_private_msg");
        assert_eq!(req.echo, Some("req1".to_string()));
    }

    #[test]
    fn test_api_request_send_group_msg() {
        let req = ApiRequest::send_group_msg("654321", &serde_json::json!("world"), None, None);
        assert_eq!(req.action, "send_group_msg");
        assert!(req.echo.is_none());
    }

    #[test]
    fn test_api_response_is_success() {
        let resp = ApiResponse {
            status: "ok".to_string(),
            retcode: 0,
            data: serde_json::json!({"message_id": 123}),
            message: String::new(),
            wording: String::new(),
            echo: None,
        };
        assert!(resp.is_success());

        let fail_resp = ApiResponse {
            status: "failed".to_string(),
            retcode: 1400,
            data: serde_json::json!(null),
            message: "bad request".to_string(),
            wording: String::new(),
            echo: None,
        };
        assert!(!fail_resp.is_success());
    }

    #[test]
    fn test_api_response_error_message() {
        let resp = ApiResponse {
            status: "failed".to_string(),
            retcode: 1400,
            data: serde_json::json!(null),
            message: "bad request".to_string(),
            wording: "Invalid parameter".to_string(),
            echo: None,
        };
        assert_eq!(resp.error_message(), "Invalid parameter");

        let resp_no_wording = ApiResponse {
            status: "failed".to_string(),
            retcode: 1400,
            data: serde_json::json!(null),
            message: "bad request".to_string(),
            wording: String::new(),
            echo: None,
        };
        assert_eq!(resp_no_wording.error_message(), "bad request");
    }

    #[test]
    fn test_post_type_parse() {
        assert_eq!(PostType::parse("message"), PostType::Message);
        assert_eq!(PostType::parse("notice"), PostType::Notice);
        assert_eq!(PostType::parse("request"), PostType::Request);
        assert_eq!(PostType::parse("meta_event"), PostType::MetaEvent);
        assert_eq!(PostType::parse("unknown"), PostType::Message);
    }

    #[test]
    fn test_message_type_parse() {
        assert_eq!(MessageType::parse("private"), MessageType::Private);
        assert_eq!(MessageType::parse("group"), MessageType::Group);
        assert_eq!(MessageType::parse("unknown"), MessageType::Private);
    }
}