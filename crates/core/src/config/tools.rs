//! 工具与安全相关配置类型。
//!
//! 包含 WebSearchConfig、ExecConfig、WebToolsConfig、ToolsConfig、
//! PathAccessConfig、SecurityConfig。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchConfig {
    /// Brave Search API key (optional, for brave_search)
    #[serde(default)]
    pub api_key: String,
    /// Baidu AI Search API key (optional, for baidu_search)
    /// Get from https://qianfan.baidubce.com — set env BAIDU_API_KEY or this field
    #[serde(default)]
    pub baidu_api_key: String,
    #[serde(default = "default_max_results")]
    pub max_results: u32,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            baidu_api_key: String::new(),
            max_results: default_max_results(),
        }
    }
}

fn default_max_results() -> u32 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecConfig {
    #[serde(default = "default_exec_timeout")]
    pub timeout: u32,
    #[serde(default = "crate::default_true")]
    pub restrict_to_workspace: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<String>,
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            timeout: default_exec_timeout(),
            restrict_to_workspace: true,
            sandbox_policy: None,
        }
    }
}

fn default_exec_timeout() -> u32 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WebToolsConfig {
    #[serde(default)]
    pub search: WebSearchConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsConfig {
    #[serde(default)]
    pub web: WebToolsConfig,
    #[serde(default)]
    pub exec: ExecConfig,
    /// Tick interval in seconds for the agent runtime loop (alert checks, cron, evolution).
    /// Lower values enable faster alert response. Default: 30. Min: 10. Max: 300.
    #[serde(default = "default_tick_interval")]
    pub tick_interval_secs: u32,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            web: WebToolsConfig::default(),
            exec: ExecConfig::default(),
            tick_interval_secs: default_tick_interval(),
        }
    }
}

fn default_tick_interval() -> u32 {
    30
}

/// Configuration for the path-access policy system.
/// Points to the separate `path_access.json5` rules file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathAccessConfig {
    /// Whether the path-access policy system is active.
    #[serde(default = "super::default_true")]
    pub enabled: bool,

    /// Path to the rules file. Supports `~/` expansion.
    #[serde(default = "default_path_access_policy_file")]
    pub policy_file: String,

    /// Behavior when the policy file is missing or unparseable.
    /// One of: `"fallback_to_safe_default"` | `"fail_closed"` | `"disabled"`
    #[serde(default = "default_missing_file_policy")]
    pub missing_file_policy: String,

    /// Reserved for future hot-reload support.
    #[serde(default)]
    pub reload_on_change: bool,
}

fn default_path_access_policy_file() -> String {
    "~/.blockcell/path_access.json5".to_string()
}

fn default_missing_file_policy() -> String {
    "fallback_to_safe_default".to_string()
}

impl Default for PathAccessConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            policy_file: default_path_access_policy_file(),
            missing_file_policy: default_missing_file_policy(),
            reload_on_change: false,
        }
    }
}

/// Top-level security settings for the agent runtime.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SecurityConfig {
    /// Explicit destination for unscoped background notifications. Without
    /// this setting only a local CLI session can establish the destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_session_key: Option<String>,
    /// Path-access policy rules.
    #[serde(default)]
    pub path_access: PathAccessConfig,
}
