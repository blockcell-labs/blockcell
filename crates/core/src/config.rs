use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::error::Result;
use crate::paths::Paths;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_base: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityHubConfig {
    #[serde(default)]
    pub hub_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Short random identifier for this node (e.g. "54c6be7b").
    /// Auto-generated on first gateway startup and persisted to config.
    /// Used as the node display name in the community hub.
    #[serde(default)]
    pub node_alias: Option<String>,
}

fn default_community_hub_url() -> Option<String> {
    Some("https://hub-api.blockcell.dev".to_string())
}

impl Default for CommunityHubConfig {
    fn default() -> Self {
        Self {
            hub_url: default_community_hub_url(),
            api_key: None,
            node_alias: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefaults {
    #[serde(default = "default_workspace")]
    pub workspace: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_max_tool_iterations")]
    pub max_tool_iterations: u32,
    #[serde(default = "default_llm_max_retries")]
    pub llm_max_retries: u32,
    #[serde(default = "default_llm_retry_delay_ms")]
    pub llm_retry_delay_ms: u64,
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: u32,
    /// 显式指定 LLM provider（可选）
    /// 如果不指定，将从 model 字符串前缀推断（如 "anthropic/claude-..."）
    #[serde(default)]
    pub provider: Option<String>,
    /// 自进化专用模型（如果为 None，则使用主模型）
    /// 建议使用更便宜/更快的模型，避免与对话抢占并发
    #[serde(default)]
    pub evolution_model: Option<String>,
    /// 自进化专用 provider（可选）
    /// 如果不指定，将从 evolution_model 推断，或使用主 provider
    #[serde(default)]
    pub evolution_provider: Option<String>,
}

fn default_workspace() -> String {
    "~/.blockcell/workspace".to_string()
}

fn default_model() -> String {
    "anthropic/claude-sonnet-4-20250514".to_string()
}

fn default_max_tokens() -> u32 {
    8192
}

fn default_temperature() -> f32 {
    0.7
}

fn default_max_tool_iterations() -> u32 {
    20
}

fn default_llm_max_retries() -> u32 {
    3
}

fn default_llm_retry_delay_ms() -> u64 {
    2000
}

fn default_max_context_tokens() -> u32 {
    32000
}

impl Default for AgentDefaults {
    fn default() -> Self {
        Self {
            workspace: default_workspace(),
            model: default_model(),
            max_tokens: default_max_tokens(),
            temperature: default_temperature(),
            max_tool_iterations: default_max_tool_iterations(),
            llm_max_retries: default_llm_max_retries(),
            llm_retry_delay_ms: default_llm_retry_delay_ms(),
            max_context_tokens: default_max_context_tokens(),
            provider: None,
            evolution_model: None,
            evolution_provider: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhostConfig {
    #[serde(default = "default_ghost_enabled")]
    pub enabled: bool,
    /// If None, uses the default agent model.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_ghost_schedule")]
    pub schedule: String,
    #[serde(default = "default_max_syncs")]
    pub max_syncs_per_day: u32,
    #[serde(default = "default_auto_social")]
    pub auto_social: bool,
}

fn default_ghost_enabled() -> bool {
    false
}

fn default_ghost_schedule() -> String {
    "0 */4 * * *" .to_string() // Every 4 hours
}

fn default_max_syncs() -> u32 {
    10
}

fn default_auto_social() -> bool {
    true
}

impl Default for GhostConfig {
    fn default() -> Self {
        Self {
            enabled: default_ghost_enabled(),
            model: None,
            schedule: default_ghost_schedule(),
            max_syncs_per_day: default_max_syncs(),
            auto_social: default_auto_social(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentsConfig {
    #[serde(default)]
    pub defaults: AgentDefaults,
    #[serde(default)]
    pub ghost: GhostConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WhatsAppConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_whatsapp_bridge_url")]
    pub bridge_url: String,
    #[serde(default)]
    pub allow_from: Vec<String>,
}

impl Default for WhatsAppConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bridge_url: default_whatsapp_bridge_url(),
            allow_from: Vec::new(),
        }
    }
}

fn default_whatsapp_bridge_url() -> String {
    "ws://localhost:3001".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TelegramConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub allow_from: Vec<String>,
    #[serde(default)]
    pub proxy: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FeishuConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub app_secret: String,
    #[serde(default)]
    pub encrypt_key: String,
    #[serde(default)]
    pub verification_token: String,
    #[serde(default)]
    pub allow_from: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SlackConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bot_token: String,
    #[serde(default)]
    pub app_token: String,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub allow_from: Vec<String>,
    #[serde(default = "default_slack_poll_interval")]
    pub poll_interval_secs: u32,
}

fn default_slack_poll_interval() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiscordConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bot_token: String,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub allow_from: Vec<String>,
}

/// 钉钉 (DingTalk) channel configuration.
/// Uses DingTalk Stream SDK for real-time message reception.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DingTalkConfig {
    #[serde(default)]
    pub enabled: bool,
    /// DingTalk app key (AppKey from the developer console)
    #[serde(default)]
    pub app_key: String,
    /// DingTalk app secret (AppSecret from the developer console)
    #[serde(default)]
    pub app_secret: String,
    /// Optional: robot code for sending messages to users
    #[serde(default)]
    pub robot_code: String,
    /// Allowlist of sender user IDs. Empty = allow all.
    #[serde(default)]
    pub allow_from: Vec<String>,
}

/// Lark (international Feishu) channel configuration.
/// Uses the same WebSocket long-connection protocol as Feishu,
/// but connects to open.larksuite.com instead of open.feishu.cn.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LarkConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub app_secret: String,
    #[serde(default)]
    pub encrypt_key: String,
    #[serde(default)]
    pub verification_token: String,
    #[serde(default)]
    pub allow_from: Vec<String>,
}

/// 企业微信 (WeCom / WeChat Work) channel configuration.
/// Supports both callback mode (webhook) and polling mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeComConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Enterprise corp ID (企业ID)
    #[serde(default)]
    pub corp_id: String,
    /// Application secret (应用Secret)
    #[serde(default)]
    pub corp_secret: String,
    /// Application agent ID (应用AgentId)
    #[serde(default)]
    pub agent_id: i64,
    /// Callback token for message verification (企业微信回调Token)
    #[serde(default)]
    pub callback_token: String,
    /// AES key for message decryption (EncodingAESKey)
    #[serde(default)]
    pub encoding_aes_key: String,
    /// Allowlist of sender user IDs. Empty = allow all.
    #[serde(default)]
    pub allow_from: Vec<String>,
    /// Polling interval in seconds (used when callback is not configured). Default: 10.
    #[serde(default = "default_wecom_poll_interval")]
    pub poll_interval_secs: u32,
}

fn default_wecom_poll_interval() -> u32 {
    10
}

impl Default for WeComConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            corp_id: String::new(),
            corp_secret: String::new(),
            agent_id: 0,
            callback_token: String::new(),
            encoding_aes_key: String::new(),
            allow_from: Vec::new(),
            poll_interval_secs: default_wecom_poll_interval(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChannelsConfig {
    #[serde(default)]
    pub whatsapp: WhatsAppConfig,
    #[serde(default)]
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub feishu: FeishuConfig,
    #[serde(default)]
    pub slack: SlackConfig,
    #[serde(default)]
    pub discord: DiscordConfig,
    #[serde(default)]
    pub dingtalk: DingTalkConfig,
    #[serde(default)]
    pub wecom: WeComConfig,
    #[serde(default)]
    pub lark: LarkConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayConfig {
    #[serde(default = "default_gateway_host")]
    pub host: String,
    #[serde(default = "default_gateway_port")]
    pub port: u16,
    #[serde(default = "default_webui_host")]
    pub webui_host: String,
    #[serde(default = "default_webui_port")]
    pub webui_port: u16,
    #[serde(default)]
    pub api_token: Option<String>,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// WebUI login password. If empty/None, a temporary password is printed at startup.
    #[serde(default)]
    pub webui_pass: Option<String>,
}

fn default_gateway_host() -> String {
    "0.0.0.0".to_string()
}

fn default_gateway_port() -> u16 {
    18790
}

fn default_webui_host() -> String {
    "localhost".to_string()
}

fn default_webui_port() -> u16 {
    18791
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            host: default_gateway_host(),
            port: default_gateway_port(),
            webui_host: default_webui_host(),
            webui_port: default_webui_port(),
            api_token: None,
            allowed_origins: vec![],
            webui_pass: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_max_results")]
    pub max_results: u32,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
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
    #[serde(default)]
    pub restrict_to_workspace: bool,
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self {
            timeout: default_exec_timeout(),
            restrict_to_workspace: false,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutoUpgradeConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_upgrade_channel")]
    pub channel: String,
    #[serde(default = "default_manifest_url")]
    pub manifest_url: String,
    #[serde(default = "default_require_signature")]
    pub require_signature: bool,
    #[serde(default)]
    pub maintenance_window: String,
}

fn default_upgrade_channel() -> String {
    "stable".to_string()
}

fn default_require_signature() -> bool {
    false
}

fn default_manifest_url() -> String {
    "https://github.com/blockcell-labs/blockcell/releases/latest/download/manifest.json".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub community_hub: CommunityHubConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub channels: ChannelsConfig,
    #[serde(default)]
    pub gateway: GatewayConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub auto_upgrade: AutoUpgradeConfig,
}

impl Default for Config {
    fn default() -> Self {
        let mut providers = HashMap::new();
        providers.insert("openrouter".to_string(), ProviderConfig {
            api_key: String::new(),
            api_base: Some("https://openrouter.ai/api/v1".to_string()),
        });
        providers.insert("anthropic".to_string(), ProviderConfig::default());
        providers.insert("openai".to_string(), ProviderConfig::default());
        providers.insert("deepseek".to_string(), ProviderConfig::default());
        providers.insert("groq".to_string(), ProviderConfig::default());
        providers.insert("zhipu".to_string(), ProviderConfig::default());
        providers.insert("vllm".to_string(), ProviderConfig {
            api_key: "dummy".to_string(),
            api_base: Some("http://localhost:8000/v1".to_string()),
        });
        providers.insert("gemini".to_string(), ProviderConfig::default());
        providers.insert("kimi".to_string(), ProviderConfig {
            api_key: String::new(),
            api_base: Some("https://api.moonshot.cn/v1".to_string()),
        });
        providers.insert("ollama".to_string(), ProviderConfig {
            api_key: "ollama".to_string(),
            api_base: Some("http://localhost:11434".to_string()),
        });

        Self {
            providers,
            community_hub: CommunityHubConfig::default(),
            agents: AgentsConfig::default(),
            channels: ChannelsConfig::default(),
            gateway: GatewayConfig::default(),
            tools: ToolsConfig::default(),
            auto_upgrade: AutoUpgradeConfig::default(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = serde_json::from_str(&content)?;
        Ok(config)
    }

    pub fn load_or_default(paths: &Paths) -> Result<Self> {
        let config_path = paths.config_file();
        if config_path.exists() {
            Self::load(&config_path)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn get_api_key(&self) -> Option<(&str, &ProviderConfig)> {
        let priority = [
            "openrouter", "deepseek", "anthropic", "openai", "kimi", "gemini", "zhipu", "groq", "vllm", "ollama",
        ];

        for name in priority {
            if let Some(provider) = self.providers.get(name) {
                if !provider.api_key.is_empty() {
                    return Some((name, provider));
                }
            }
        }
        None
    }

    pub fn get_provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }

    pub fn community_hub_url(&self) -> Option<String> {
        if let Some(url) = self.community_hub.hub_url.as_ref() {
            let url = url.trim();
            if !url.is_empty() {
                return Some(url.trim_end_matches('/').to_string());
            }
        }
        None
    }

    pub fn community_hub_api_key(&self) -> Option<String> {
        if let Some(key) = self.community_hub.api_key.as_ref() {
            let key = key.trim();
            if !key.is_empty() {
                return Some(key.to_string());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_community_hub_top_level() {
        let raw = r#"{
  "communityHub": { "hubUrl": "http://example.com/", "apiKey": "k" },
  "providers": {}
}"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();
        assert_eq!(cfg.community_hub_url().as_deref(), Some("http://example.com"));
        assert_eq!(cfg.community_hub_api_key().as_deref(), Some("k"));
    }
}
