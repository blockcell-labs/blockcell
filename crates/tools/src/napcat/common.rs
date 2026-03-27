//! Common utilities for NapCatQQ tools.
//!
//! This module provides:
//! - Permission checking logic
//! - API adapter abstraction
//! - Tool schema builders
//! - Risk level classification

use blockcell_core::types::PermissionSet;
use blockcell_core::{Error, Result};
use serde_json::Value;

// Re-export from channels crate for adapter types
pub use blockcell_channels::napcat::{ApiRequest, ApiResponse};
// Re-export NapCatConfig from core
pub use blockcell_core::config::NapCatConfig;

/// Risk level for NapCatQQ tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    /// Read-only operations - no side effects.
    ReadOnly,
    /// Low risk - reversible modifications.
    LowRisk,
    /// Medium risk - significant but reversible changes.
    MediumRisk,
    /// High risk - irreversible or major impact.
    HighRisk,
}

/// Get the risk level for a tool name.
pub fn get_tool_risk_level(tool_name: &str) -> RiskLevel {
    match tool_name {
        // Read-only operations
        "napcat_get_login_info"
        | "napcat_get_status"
        | "napcat_get_version_info"
        | "napcat_get_group_list"
        | "napcat_get_friend_list"
        | "napcat_get_group_member_list"
        | "napcat_get_group_member_info"
        | "napcat_get_group_info"
        | "napcat_get_stranger_info"
        | "napcat_get_msg"
        | "napcat_get_group_file_system_info"
        | "napcat_get_group_files_by_folder"
        | "napcat_get_cookies"
        | "napcat_get_csrf_token"
        | "napcat_get_forward_msg"
        | "napcat_get_essence_msg_list"
        | "napcat_get_group_at_all_remain"
        | "napcat_get_image"
        | "napcat_get_record"
        | "napcat_get_video" => RiskLevel::ReadOnly,

        // Low risk operations
        "napcat_set_group_card"
        | "napcat_send_like"
        | "napcat_set_friend_remark"
        | "napcat_mark_msg_as_read" => RiskLevel::LowRisk,

        // Medium risk operations
        "napcat_set_group_admin"
        | "napcat_set_group_name"
        | "napcat_set_group_special_title"
        | "napcat_set_group_ban"
        | "napcat_set_group_whole_ban"
        | "napcat_delete_msg"
        | "napcat_set_friend_add_request"
        | "napcat_set_group_add_request"
        | "napcat_upload_group_file"
        | "napcat_delete_group_file"
        | "napcat_send_private_msg"
        | "napcat_send_group_msg"
        | "napcat_set_msg_emoji_like"
        | "napcat_set_essence_msg"
        | "napcat_delete_essence_msg"
        | "napcat_download_file" => RiskLevel::MediumRisk,

        // High risk operations
        "napcat_set_group_kick" | "napcat_set_group_leave" | "napcat_delete_friend" => {
            RiskLevel::HighRisk
        }

        // Default to medium risk for unknown tools
        _ => RiskLevel::MediumRisk,
    }
}

/// Permission check result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionResult {
    /// Permission granted.
    Allowed,
    /// Permission denied with reason.
    Denied(String),
    /// Operation requires confirmation.
    NeedsConfirmation,
}

/// Permission checker for NapCatQQ admin operations.
pub struct NapCatPermissionChecker<'a> {
    config: &'a NapCatConfig,
}

impl<'a> NapCatPermissionChecker<'a> {
    /// Create a new permission checker.
    pub fn new(config: &'a NapCatConfig) -> Self {
        Self { config }
    }

    /// Check permission for a tool operation.
    pub fn check_permission(
        &self,
        tool_name: &str,
        user_id: &str,
        group_id: Option<&str>,
        sender_role: Option<&str>,
    ) -> Result<PermissionResult> {
        // 1. Check if user is blocked
        if self.is_blocked(user_id) {
            return Ok(PermissionResult::Denied(format!(
                "User {} is in blocklist",
                user_id
            )));
        }

        // 2. Resolve allowed admins
        let allowed_admins = self.resolve_allowed_admins(tool_name);
        let allowed_groups = self.resolve_allowed_groups(tool_name);

        // 3. Check user whitelist
        if !Self::is_in_list(user_id, &allowed_admins) {
            // Check default policy
            let policy = self.resolve_default_policy(tool_name);
            if policy == "deny" {
                return Ok(PermissionResult::Denied(format!(
                    "User {} is not in admin whitelist",
                    user_id
                )));
            }
        }

        // 4. Check group permission if in a group context
        if let Some(gid) = group_id {
            if !allowed_groups.is_empty() && !Self::is_in_list(gid, &allowed_groups) {
                return Ok(PermissionResult::Denied(format!(
                    "Group {} is not in authorized list",
                    gid
                )));
            }

            // 5. Check role requirement
            if let Some(required_role) = self.get_required_role(tool_name) {
                if !Self::check_role(sender_role, &required_role) {
                    return Ok(PermissionResult::Denied(format!(
                        "Requires {} role, but user has {:?}",
                        required_role, sender_role
                    )));
                }
            }
        }

        // 6. Check if confirmation is needed
        if self.needs_confirmation(tool_name) {
            return Ok(PermissionResult::NeedsConfirmation);
        }

        Ok(PermissionResult::Allowed)
    }

    /// Check if user is in blocklist.
    fn is_blocked(&self, user_id: &str) -> bool {
        self.config
            .block_from
            .iter()
            .any(|b| b == user_id || b == "*")
    }

    /// Check if an ID is in a list (supports "*" wildcard).
    fn is_in_list(id: &str, list: &[String]) -> bool {
        list.iter().any(|item| item == id || item == "*")
    }

    /// Resolve allowed admins for a tool (with inheritance).
    fn resolve_allowed_admins(&self, tool_name: &str) -> Vec<String> {
        // Check tool-specific override
        if let Some(override_config) = self.config.admin_permissions.tool_overrides.get(tool_name)
        {
            if let Some(ref admins) = override_config.allowed_admins {
                return admins.clone();
            }
        }

        // Use admin_permissions.allowed_admins
        if !self.config.admin_permissions.allowed_admins.is_empty() {
            return self.config.admin_permissions.allowed_admins.clone();
        }

        // Inherit from allow_from
        self.config.allow_from.clone()
    }

    /// Resolve allowed groups for a tool (with inheritance).
    fn resolve_allowed_groups(&self, tool_name: &str) -> Vec<String> {
        // Check tool-specific override
        if let Some(override_config) = self.config.admin_permissions.tool_overrides.get(tool_name)
        {
            if let Some(ref groups) = override_config.allowed_groups {
                return groups.clone();
            }
        }

        // Use admin_permissions.allowed_groups
        if !self.config.admin_permissions.allowed_groups.is_empty() {
            return self.config.admin_permissions.allowed_groups.clone();
        }

        // Inherit from allow_groups
        self.config.allow_groups.clone()
    }

    /// Resolve default policy for a tool.
    fn resolve_default_policy(&self, tool_name: &str) -> String {
        if let Some(override_config) = self.config.admin_permissions.tool_overrides.get(tool_name)
        {
            if let Some(ref policy) = override_config.default_policy {
                return policy.clone();
            }
        }
        self.config.admin_permissions.default_policy.clone()
    }

    /// Get required role for a tool.
    fn get_required_role(&self, tool_name: &str) -> Option<String> {
        self.config
            .admin_permissions
            .tool_overrides
            .get(tool_name)
            .and_then(|o| o.require_role.clone())
    }

    /// Check if a tool needs confirmation.
    fn needs_confirmation(&self, tool_name: &str) -> bool {
        // Check tool-specific override
        if let Some(override_config) = self.config.admin_permissions.tool_overrides.get(tool_name)
        {
            if override_config.require_confirmation {
                return true;
            }
        }
        // Check global confirmation list
        self.config
            .admin_permissions
            .require_confirmation
            .contains(&tool_name.to_string())
    }

    /// Check if sender role meets requirement.
    fn check_role(sender_role: Option<&str>, required: &str) -> bool {
        let role = sender_role.unwrap_or("member");
        match required {
            "owner" => role == "owner",
            "admin" => role == "owner" || role == "admin",
            "member" => true,
            _ => false,
        }
    }
}

/// Build required permissions for a NapCatQQ tool.
///
/// Permission model:
/// - `channel:napcat`: Basic channel access (required for all napcat tools)
/// - `napcat:read_only`: Read-only operations (no side effects)
/// - `napcat:low_risk`: Low risk operations (reversible modifications)
/// - `napcat:medium_risk`: Medium risk operations (significant but reversible)
/// - `napcat:high_risk`: High risk operations (irreversible or major impact)
///
/// Users are granted these permissions based on their whitelist status and admin role.
pub fn build_napcat_permissions(tool_name: &str) -> PermissionSet {
    let mut perms = PermissionSet::new();

    // Channel restriction - all napcat tools require this
    perms = perms.with_permission("channel:napcat");

    // Risk level permission - determines who can use this tool
    match get_tool_risk_level(tool_name) {
        RiskLevel::HighRisk => {
            perms = perms.with_permission("napcat:high_risk");
        }
        RiskLevel::MediumRisk => {
            perms = perms.with_permission("napcat:medium_risk");
        }
        RiskLevel::LowRisk => {
            perms = perms.with_permission("napcat:low_risk");
        }
        RiskLevel::ReadOnly => {
            perms = perms.with_permission("napcat:read_only");
        }
    }

    perms
}

/// Check if current channel is napcat.
pub fn check_channel(ctx: &crate::ToolContext) -> Result<()> {
    if ctx.channel != "napcat" {
        return Err(Error::Tool(format!(
            "This tool only works on NapCatQQ channel, current channel: {}",
            ctx.channel
        )));
    }
    Ok(())
}

/// Get account_id from params or context.
pub fn resolve_account_id(ctx: &crate::ToolContext, params: &Value) -> Option<String> {
    params
        .get("account_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| ctx.account_id.clone())
}

/// Get sender_id from context.
///
/// Returns the user ID who triggered the tool call.
/// This is set by the channel handler when processing inbound messages.
pub fn get_sender_id(ctx: &crate::ToolContext) -> String {
    ctx.sender_id.clone().unwrap_or_default()
}

/// Build tool description with NapCatQQ channel restriction note.
/// Returns a static string to satisfy ToolSchema requirements.
/// Note: Risk level info should be embedded in base_description for static lifetime.
pub fn build_description(base_description: &'static str, _risk_level: RiskLevel) -> &'static str {
    // Risk level is already included in the base_description passed by callers
    // This function exists for API consistency and future extensibility
    base_description
}

/// Check if WebSocket API is available for making calls.
pub fn is_ws_api_available() -> bool {
    blockcell_channels::napcat::is_ws_api_available()
}

/// Call an API via WebSocket.
///
/// This function uses WebSocket API for all calls.
///
/// # Arguments
///
/// * `config` - NapCat configuration
/// * `_account_id` - Optional account ID for multi-account scenarios (unused in WS mode)
/// * `request` - The API request to make
///
/// # Returns
///
/// The API response on success, or an error.
pub async fn call_api(
    _config: &NapCatConfig,
    _account_id: Option<&str>,
    request: ApiRequest,
) -> Result<ApiResponse> {
    if !is_ws_api_available() {
        return Err(Error::Channel(
            "WebSocket is not connected".to_string()
        ));
    }
    blockcell_channels::napcat::call_api_via_ws(request)
        .await
        .map_err(|e| Error::Channel(format!("WebSocket API call failed: {}", e)))
}

/// Call a streaming API and collect all chunks into file data.
///
/// This function uses `download_file_stream` API to download files
/// and returns the complete file data.
///
/// # Arguments
///
/// * `config` - NapCat configuration
/// * `_account_id` - Optional account ID for multi-account scenarios (unused in WS mode)
/// * `url` - The URL to download
/// * `thread_count` - Optional number of threads for parallel download
/// * `headers` - Optional custom headers
///
/// # Returns
///
/// The complete file data on success, or an error.
pub async fn call_stream_api(
    _config: &NapCatConfig,
    _account_id: Option<&str>,
    url: &str,
    thread_count: Option<i32>,
    headers: Option<&[&str]>,
) -> Result<Vec<u8>> {
    // Build the streaming request
    let request = ApiRequest::download_file_stream(url, thread_count, headers, None);

    if !blockcell_channels::napcat::is_ws_stream_available() {
        return Err(Error::Channel(
            "WebSocket stream caller is not available".to_string()
        ));
    }
    blockcell_channels::napcat::call_stream_api_via_ws(request)
        .await
        .map_err(|e| Error::Channel(format!("WebSocket stream API call failed: {}", e)))
}

/// Download media and save to local workspace.
///
/// This function downloads media using the streaming API and saves it to the
/// configured download directory. Files are organized by date and chat_id:
/// `downloads/YYYY-MM-DD_chat_id/filename`
///
/// # Arguments
///
/// * `config` - NapCat configuration
/// * `account_id` - Optional account ID for multi-account scenarios
/// * `url` - The URL to download
/// * `filename` - Optional filename to save as (if None, extracts from URL)
/// * `workspace` - The workspace directory path
/// * `chat_id` - Optional chat ID (format: "user:xxx" or "group:xxx") for directory organization
///
/// # Returns
///
/// The local file path on success, or an error.
pub async fn download_media_to_workspace(
    config: &NapCatConfig,
    account_id: Option<&str>,
    url: &str,
    filename: Option<&str>,
    workspace: &str,
    chat_id: Option<&str>,
) -> Result<String> {
    // Download the file using streaming API
    let file_data = call_stream_api(
        config,
        account_id,
        url,
        Some(3), // Use 3 threads for download
        None,
    ).await?;

    // Determine filename
    let filename = filename.map(|s| s.to_string()).unwrap_or_else(|| {
        // Try to extract filename from URL
        if let Some(name) = url.split('/').next_back() {
            // Remove query parameters
            name.split('?').next().unwrap_or("downloaded_file").to_string()
        } else {
            format!("media_{}", chrono::Utc::now().format("%Y%m%d_%H%M%S"))
        }
    });

    // Expand workspace path if it starts with ~
    let workspace_dir = if workspace.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            workspace.replacen('~', home.to_str().unwrap_or(""), 1)
        } else {
            workspace.to_string()
        }
    } else {
        workspace.to_string()
    };

    // Build download directory path: downloads/YYYY-MM-DD_chat_id/
    let date_str = chrono::Local::now().format("%Y-%m-%d").to_string();
    let subdir_name = if let Some(cid) = chat_id {
        // Extract the ID part (remove "user:" or "group:" prefix)
        let id_part = cid.strip_prefix("user:")
            .or_else(|| cid.strip_prefix("group:"))
            .unwrap_or(cid);
        format!("{}_{}", date_str, id_part)
    } else {
        date_str.clone()
    };

    let downloads_dir = std::path::Path::new(&workspace_dir)
        .join(&config.media_download_dir)
        .join(&subdir_name);

    if !downloads_dir.exists() {
        std::fs::create_dir_all(&downloads_dir)
            .map_err(|e| Error::Tool(format!("Failed to create downloads directory: {}", e)))?;
    }

    // Save file
    let file_path = downloads_dir.join(&filename);
    let mut file = tokio::fs::File::create(&file_path).await
        .map_err(|e| Error::Tool(format!("Failed to create file: {}", e)))?;

    use tokio::io::AsyncWriteExt;
    file.write_all(&file_data).await
        .map_err(|e| Error::Tool(format!("Failed to write file: {}", e)))?;

    tracing::info!(
        url = url,
        file_path = %file_path.display(),
        size = file_data.len(),
        chat_id = ?chat_id,
        "Media downloaded successfully"
    );

    Ok(file_path.to_string_lossy().to_string())
}

/// Extract filename from URL.
///
/// Tries to extract a reasonable filename from a URL, removing query parameters
/// and handling common patterns.
pub fn extract_filename_from_url(url: &str) -> Option<String> {
    // Remove query parameters
    let url_without_query = url.split('?').next().unwrap_or(url);

    // Get the last path segment
    if let Some(segment) = url_without_query.split('/').next_back() {
        if !segment.is_empty() {
            return Some(segment.to_string());
        }
    }

    None
}

/// Generate a filename based on media type and timestamp.
///
/// Used as fallback when no filename can be extracted from URL.
pub fn generate_media_filename(media_type: &str, extension: Option<&str>) -> String {
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let ext = extension.unwrap_or(match media_type {
        "image" => "jpg",
        "voice" => "amr",
        "video" => "mp4",
        "file" => "bin",
        _ => "bin",
    });
    format!("{}_{}.{}", media_type, timestamp, ext)
}

/// Find a downloaded media file by URL.
///
/// Searches through the download directories to find a file that was downloaded
/// from the given URL. The search is based on:
/// 1. Exact URL match (if tracked in metadata)
/// 2. Filename extracted from URL
///
/// # Arguments
///
/// * `config` - NapCat configuration
/// * `url` - The original download URL
/// * `workspace` - The workspace directory path
///
/// # Returns
///
/// The local file path if found, or None.
pub fn find_downloaded_media(
    config: &NapCatConfig,
    url: &str,
    workspace: &str,
) -> Option<String> {
    // Expand workspace path if it starts with ~
    let workspace_dir = if workspace.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            workspace.replacen('~', home.to_str().unwrap_or(""), 1)
        } else {
            workspace.to_string()
        }
    } else {
        workspace.to_string()
    };

    // Get the base download directory
    let downloads_base = std::path::Path::new(&workspace_dir).join(&config.media_download_dir);

    if !downloads_base.exists() {
        return None;
    }

    // Extract expected filename from URL
    let expected_filename = extract_filename_from_url(url)?;

    // Search through all subdirectories (date_chat_id format)
    if let Ok(entries) = std::fs::read_dir(&downloads_base) {
        for entry in entries.flatten() {
            let subdir = entry.path();
            if subdir.is_dir() {
                let file_path = subdir.join(&expected_filename);
                if file_path.exists() && file_path.is_file() {
                    // Check file size > 0
                    if let Ok(metadata) = std::fs::metadata(&file_path) {
                        if metadata.len() > 0 {
                            return Some(file_path.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
    }

    None
}

/// Download media if not already downloaded.
///
/// This function first checks if the media has already been downloaded.
/// If not, it downloads based on the `auto_download_media` config.
///
/// # Arguments
///
/// * `config` - NapCat configuration
/// * `account_id` - Optional account ID for multi-account scenarios
/// * `url` - The URL to download
/// * `filename` - Optional filename to save as
/// * `workspace` - The workspace directory path
/// * `chat_id` - Optional chat ID for directory organization
///
/// # Returns
///
/// A tuple of (local_path, was_already_downloaded).
/// Returns None if not downloaded and auto_download_media is false.
pub async fn download_media_if_needed(
    config: &NapCatConfig,
    account_id: Option<&str>,
    url: &str,
    filename: Option<&str>,
    workspace: &str,
    chat_id: Option<&str>,
) -> Result<Option<(String, bool)>> {
    // First, check if already downloaded
    if let Some(local_path) = find_downloaded_media(config, url, workspace) {
        tracing::info!(url = url, local_path = %local_path, "Media already downloaded");
        return Ok(Some((local_path, true)));
    }

    // Not downloaded yet, check if auto_download is enabled
    if !config.auto_download_media {
        tracing::debug!(url = url, "Media not downloaded and auto_download_media is disabled");
        return Ok(None);
    }

    // Download the media
    let local_path = download_media_to_workspace(
        config,
        account_id,
        url,
        filename,
        workspace,
        chat_id,
    ).await?;

    Ok(Some((local_path, false)))
}

/// Build permissions for a NapCat channel user.
///
/// This function creates a PermissionSet based on:
/// 1. Channel access (channel:napcat)
/// 2. User whitelist membership (allow_from)
/// 3. Group whitelist membership (allow_groups)
/// 4. Admin status for high-risk operations
///
/// # Arguments
///
/// * `config` - NapCat configuration
/// * `sender_id` - The user ID (QQ number) who triggered the message
/// * `chat_id` - The chat ID (format: "group:xxx" or "user:xxx")
///
/// # Returns
///
/// A PermissionSet with appropriate permissions for the user.
pub fn build_napcat_user_permissions(
    config: &NapCatConfig,
    sender_id: Option<&str>,
    chat_id: &str,
) -> PermissionSet {
    let mut perms = PermissionSet::new();

    // Always grant channel access for NapCat messages
    perms = perms.with_permission("channel:napcat");

    // Check if user is in allow_from or allow_from is empty (allow all)
    let user_allowed = sender_id.map(|uid| {
        config.allow_from.is_empty()
            || config.allow_from.iter().any(|a| a == uid || a == "*")
    }).unwrap_or(true);

    // Check if user is in block_from
    let user_blocked = sender_id.map(|uid| {
        config.block_from.iter().any(|b| b == uid || b == "*")
    }).unwrap_or(false);

    // Parse group_id from chat_id if it's a group chat
    let group_id = chat_id.strip_prefix("group:");
    let is_group = group_id.is_some();

    // Check group allowlist
    let group_allowed = if is_group {
        let gid = group_id.unwrap();
        config.allow_groups.is_empty()
            || config.allow_groups.iter().any(|g| g == gid || g == "*")
    } else {
        true // Private chats don't need group check
    };

    // Grant basic permissions if user is allowed and not blocked
    if user_allowed && !user_blocked && group_allowed {
        // Grant read-only permissions
        perms = perms.with_permission("napcat:read_only");

        // Grant low-risk permissions for all allowed users
        perms = perms.with_permission("napcat:low_risk");

        // Grant medium-risk permissions
        perms = perms.with_permission("napcat:medium_risk");

        // Check if user is an admin for high-risk operations
        let is_admin = sender_id.map(|uid| {
            // Check if user is in allowed_admins or allow_from
            config.admin_permissions.allowed_admins.iter().any(|a| a == uid || a == "*")
                || (config.admin_permissions.allowed_admins.is_empty()
                    && config.allow_from.iter().any(|a| a == uid || a == "*"))
        }).unwrap_or(false);

        if is_admin {
            perms = perms.with_permission("napcat:high_risk");
        }

        // Grant all napcat tool permissions for allowed users
        // This allows the user to call any napcat tool they have risk-level access for
        perms = perms.with_permission("napcat:tools");
    }

    perms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_risk_level_classification() {
        assert_eq!(
            get_tool_risk_level("napcat_get_group_list"),
            RiskLevel::ReadOnly
        );
        assert_eq!(
            get_tool_risk_level("napcat_set_group_kick"),
            RiskLevel::HighRisk
        );
        assert_eq!(
            get_tool_risk_level("napcat_set_group_ban"),
            RiskLevel::MediumRisk
        );
        assert_eq!(
            get_tool_risk_level("napcat_send_like"),
            RiskLevel::LowRisk
        );
    }

    #[test]
    fn test_permission_result() {
        let mut config = NapCatConfig::default();
        config.admin_permissions.allowed_admins = vec!["123456".to_string()];
        config.admin_permissions.default_policy = "deny".to_string();

        let checker = NapCatPermissionChecker::new(&config);

        // Whitelisted user should be allowed
        let result = checker.check_permission("napcat_get_group_list", "123456", None, None);
        assert!(matches!(result, Ok(PermissionResult::Allowed)));

        // Non-whitelisted user should be denied
        let result = checker.check_permission("napcat_set_group_kick", "999999", None, None);
        assert!(matches!(result, Ok(PermissionResult::Denied(_))));
    }

    #[test]
    fn test_build_napcat_permissions() {
        let perms = build_napcat_permissions("napcat_set_group_kick");
        assert!(perms.has("channel:napcat"));
        assert!(perms.has("napcat:napcat_set_group_kick"));
        assert!(perms.has("napcat:high_risk"));
    }
}