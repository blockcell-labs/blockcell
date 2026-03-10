use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
    /// 该 provider 专用代理（可选）。优先级高于全局 network.proxy。
    /// 设置为空字符串 "" 可强制该 provider 直连（跳过全局代理）。
    /// 格式："http://host:port" 或 "socks5://host:port"
    #[serde(default)]
    pub proxy: Option<String>,
    /// API 接口类型："openai" | "openai_responses" | "anthropic" | "gemini" | "ollama"
    /// 用于前端显示和接口兼容性标识，默认 "openai"（序列化时省略默认值）
    #[serde(
        default = "default_api_type",
        skip_serializing_if = "is_default_api_type"
    )]
    pub api_type: String,
}

fn default_api_type() -> String {
    "openai".to_string()
}

fn is_default_api_type(t: &str) -> bool {
    t == "openai"
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

/// 一个可用的"模型+供应商"条目，用于 model_pool 多模型高可用配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    /// 模型名称，例如 "deepseek-chat"、"claude-3-5-sonnet"
    pub model: String,
    /// 对应 providers 表中的 key，例如 "deepseek"、"anthropic"
    pub provider: String,
    /// 负载均衡权重（正整数，越大越优先被选中），默认 1
    #[serde(default = "default_entry_weight")]
    pub weight: u32,
    /// 优先级（小数字 = 高优先级），同优先级内按 weight 加权随机，默认 1
    #[serde(default = "default_entry_priority")]
    pub priority: u32,
    /// 输入价格（USD/1M tokens），可选
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_price: Option<f64>,
    /// 输出价格（USD/1M tokens），可选
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_price: Option<f64>,
}

fn default_entry_weight() -> u32 {
    1
}
fn default_entry_priority() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    /// 多模型高可用池（可选）。
    /// 配置后，系统将从池中按优先级+权重选取 provider，失败自动降级。
    /// 若留空，则沿用旧的单 model + provider 配置（向后兼容）。
    #[serde(default)]
    pub model_pool: Vec<ModelEntry>,
    /// Allowed MCP server names visible to this agent.
    #[serde(default)]
    pub allowed_mcp_servers: Vec<String>,
    /// Allowed MCP tool names visible to this agent.
    #[serde(default)]
    pub allowed_mcp_tools: Vec<String>,
}

fn default_workspace() -> String {
    "~/.blockcell/workspace".to_string()
}

fn default_model() -> String {
    "".to_string()
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

fn default_true() -> bool {
    true
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
            model_pool: Vec::new(),
            allowed_mcp_servers: Vec::new(),
            allowed_mcp_tools: Vec::new(),
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
    "0 */4 * * *".to_string() // Every 4 hours
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

/// 全局网络代理配置。
/// 所有 LLM provider HTTP 请求默认走此代理，可被 providers.<name>.proxy 覆盖。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NetworkConfig {
    /// 全局代理地址，例如 "http://127.0.0.1:7890"
    /// 留空或不配置则直连。
    #[serde(default)]
    pub proxy: Option<String>,
    /// 不走代理的域名/IP 列表，支持前缀通配符 "*.example.com"。
    /// 常见示例：["localhost", "127.0.0.1", "::1", "*.local"]
    #[serde(default)]
    pub no_proxy: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentsConfig {
    #[serde(default)]
    pub defaults: AgentDefaults,
    #[serde(default)]
    pub ghost: GhostConfig,
    /// Optional multi-agent definitions.
    /// If empty, runtime falls back to a single implicit "default" agent.
    #[serde(default)]
    pub list: Vec<AgentProfileConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfileConfig {
    pub id: String,
    #[serde(default = "default_agent_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub intent_profile: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model_pool: Vec<ModelEntry>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tool_iterations: Option<u32>,
    #[serde(default)]
    pub llm_max_retries: Option<u32>,
    #[serde(default)]
    pub llm_retry_delay_ms: Option<u64>,
    #[serde(default)]
    pub max_context_tokens: Option<u32>,
    #[serde(default)]
    pub evolution_model: Option<String>,
    #[serde(default)]
    pub evolution_provider: Option<String>,
    #[serde(default)]
    pub allowed_mcp_servers: Option<Vec<String>>,
    #[serde(default)]
    pub allowed_mcp_tools: Option<Vec<String>>,
}

fn default_agent_enabled() -> bool {
    true
}

impl Default for AgentProfileConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            enabled: true,
            name: None,
            intent_profile: None,
            model: None,
            provider: None,
            model_pool: Vec::new(),
            max_tokens: None,
            temperature: None,
            max_tool_iterations: None,
            llm_max_retries: None,
            llm_retry_delay_ms: None,
            max_context_tokens: None,
            evolution_model: None,
            evolution_provider: None,
            allowed_mcp_servers: None,
            allowed_mcp_tools: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAgentConfig {
    pub id: String,
    pub name: Option<String>,
    pub defaults: AgentDefaults,
    pub intent_profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentToolRuleConfig {
    #[serde(default = "default_true")]
    pub inherit_base: bool,
    #[serde(default)]
    pub tools: Vec<String>,
}

impl Default for IntentToolRuleConfig {
    fn default() -> Self {
        Self {
            inherit_base: true,
            tools: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IntentToolEntryConfig {
    Tools(Vec<String>),
    Rule(IntentToolRuleConfig),
}

impl IntentToolEntryConfig {
    pub fn inherit_base(&self) -> bool {
        match self {
            Self::Tools(_) => true,
            Self::Rule(rule) => rule.inherit_base,
        }
    }

    pub fn tools(&self) -> &[String] {
        match self {
            Self::Tools(tools) => tools,
            Self::Rule(rule) => &rule.tools,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct IntentToolProfileConfig {
    #[serde(default)]
    pub core_tools: Vec<String>,
    #[serde(default)]
    pub intent_tools: HashMap<String, IntentToolEntryConfig>,
    #[serde(default)]
    pub deny_tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentRouterConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_intent_router_profile")]
    pub default_profile: String,
    #[serde(default)]
    pub agent_profiles: HashMap<String, String>,
    #[serde(default = "default_intent_router_profiles")]
    pub profiles: HashMap<String, IntentToolProfileConfig>,
}

impl Default for IntentRouterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_profile: default_intent_router_profile(),
            agent_profiles: HashMap::new(),
            profiles: default_intent_router_profiles(),
        }
    }
}

fn default_intent_router_profile() -> String {
    "default".to_string()
}

fn default_intent_router_profiles() -> HashMap<String, IntentToolProfileConfig> {
    let mut profiles = HashMap::new();
    profiles.insert(
        "default".to_string(),
        IntentToolProfileConfig {
            core_tools: vec![
                "read_file".to_string(),
                "write_file".to_string(),
                "list_dir".to_string(),
                "exec".to_string(),
                "web_search".to_string(),
                "web_fetch".to_string(),
                "memory_query".to_string(),
                "memory_upsert".to_string(),
                "toggle_manage".to_string(),
                "message".to_string(),
                "agent_status".to_string(),
            ],
            intent_tools: HashMap::from([
                (
                    "Chat".to_string(),
                    IntentToolEntryConfig::Rule(IntentToolRuleConfig {
                        inherit_base: false,
                        tools: vec![],
                    }),
                ),
                (
                    "FileOps".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "edit_file".to_string(),
                        "file_ops".to_string(),
                        "data_process".to_string(),
                        "office_write".to_string(),
                    ]),
                ),
                (
                    "WebSearch".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "browse".to_string(),
                        "http_request".to_string(),
                    ]),
                ),
                (
                    "Finance".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "http_request".to_string(),
                        "data_process".to_string(),
                        "chart_generate".to_string(),
                        "alert_rule".to_string(),
                        "stream_subscribe".to_string(),
                        "knowledge_graph".to_string(),
                        "cron".to_string(),
                        "office_write".to_string(),
                        "browse".to_string(),
                    ]),
                ),
                (
                    "Blockchain".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "stream_subscribe".to_string(),
                        "http_request".to_string(),
                        "knowledge_graph".to_string(),
                    ]),
                ),
                (
                    "DataAnalysis".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "edit_file".to_string(),
                        "file_ops".to_string(),
                        "data_process".to_string(),
                        "chart_generate".to_string(),
                        "office_write".to_string(),
                        "http_request".to_string(),
                    ]),
                ),
                (
                    "Communication".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "email".to_string(),
                        "message".to_string(),
                        "http_request".to_string(),
                        "community_hub".to_string(),
                    ]),
                ),
                (
                    "SystemControl".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "system_info".to_string(),
                        "capability_evolve".to_string(),
                        "app_control".to_string(),
                        "camera_capture".to_string(),
                        "browse".to_string(),
                        "image_understand".to_string(),
                        "termux_api".to_string(),
                    ]),
                ),
                (
                    "Organization".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "cron".to_string(),
                        "memory_forget".to_string(),
                        "knowledge_graph".to_string(),
                        "list_tasks".to_string(),
                        "spawn".to_string(),
                        "list_skills".to_string(),
                        "memory_maintenance".to_string(),
                        "community_hub".to_string(),
                    ]),
                ),
                (
                    "IoT".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "http_request".to_string(),
                        "cron".to_string(),
                    ]),
                ),
                (
                    "Media".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "audio_transcribe".to_string(),
                        "tts".to_string(),
                        "ocr".to_string(),
                        "image_understand".to_string(),
                        "video_process".to_string(),
                        "file_ops".to_string(),
                    ]),
                ),
                (
                    "DevOps".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "network_monitor".to_string(),
                        "encrypt".to_string(),
                        "http_request".to_string(),
                        "edit_file".to_string(),
                        "file_ops".to_string(),
                    ]),
                ),
                (
                    "Lifestyle".to_string(),
                    IntentToolEntryConfig::Tools(vec!["http_request".to_string()]),
                ),
                (
                    "Unknown".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "edit_file".to_string(),
                        "file_ops".to_string(),
                        "office_write".to_string(),
                        "http_request".to_string(),
                        "browse".to_string(),
                        "spawn".to_string(),
                        "list_tasks".to_string(),
                        "cron".to_string(),
                        "memory_forget".to_string(),
                        "list_skills".to_string(),
                        "community_hub".to_string(),
                        "memory_maintenance".to_string(),
                    ]),
                ),
            ]),
            deny_tools: Vec::new(),
        },
    );
    profiles
}

fn default_intent_router_option() -> Option<IntentRouterConfig> {
    Some(IntentRouterConfig::default())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WhatsAppAccountConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_whatsapp_bridge_url")]
    pub bridge_url: String,
    #[serde(default)]
    pub allow_from: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TelegramAccountConfig {
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
pub struct FeishuAccountConfig {
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
pub struct SlackAccountConfig {
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiscordAccountConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bot_token: String,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default)]
    pub allow_from: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DingTalkAccountConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub app_key: String,
    #[serde(default)]
    pub app_secret: String,
    #[serde(default)]
    pub robot_code: String,
    #[serde(default)]
    pub allow_from: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LarkAccountConfig {
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
pub struct WeComAccountConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub corp_id: String,
    #[serde(default)]
    pub corp_secret: String,
    #[serde(default)]
    pub agent_id: i64,
    #[serde(default)]
    pub callback_token: String,
    #[serde(default)]
    pub encoding_aes_key: String,
    #[serde(default)]
    pub allow_from: Vec<String>,
    #[serde(default = "default_wecom_poll_interval")]
    pub poll_interval_secs: u32,
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
    /// Multi-account config map. Key is account_id.
    #[serde(default)]
    pub accounts: HashMap<String, WhatsAppAccountConfig>,
    #[serde(default)]
    pub default_account_id: Option<String>,
}

impl Default for WhatsAppConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bridge_url: default_whatsapp_bridge_url(),
            allow_from: Vec::new(),
            accounts: HashMap::new(),
            default_account_id: None,
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
    #[serde(default)]
    pub accounts: HashMap<String, TelegramAccountConfig>,
    #[serde(default)]
    pub default_account_id: Option<String>,
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
    #[serde(default)]
    pub accounts: HashMap<String, FeishuAccountConfig>,
    #[serde(default)]
    pub default_account_id: Option<String>,
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
    #[serde(default)]
    pub accounts: HashMap<String, SlackAccountConfig>,
    #[serde(default)]
    pub default_account_id: Option<String>,
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
    #[serde(default)]
    pub accounts: HashMap<String, DiscordAccountConfig>,
    #[serde(default)]
    pub default_account_id: Option<String>,
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
    #[serde(default)]
    pub accounts: HashMap<String, DingTalkAccountConfig>,
    #[serde(default)]
    pub default_account_id: Option<String>,
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
    #[serde(default)]
    pub accounts: HashMap<String, LarkAccountConfig>,
    #[serde(default)]
    pub default_account_id: Option<String>,
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
    #[serde(default)]
    pub accounts: HashMap<String, WeComAccountConfig>,
    #[serde(default)]
    pub default_account_id: Option<String>,
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
            accounts: HashMap::new(),
            default_account_id: None,
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
    /// Optional public API base URL injected into WebUI at runtime.
    /// Example: "https://your-domain.example.com" or "https://your-domain.example.com/api".
    /// If not set, WebUI will default to current hostname + gateway.port.
    #[serde(default)]
    pub public_api_base: Option<String>,
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
            public_api_base: None,
            api_token: None,
            allowed_origins: vec![],
            webui_pass: None,
        }
    }
}

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
    pub network: NetworkConfig,
    #[serde(default)]
    pub community_hub: CommunityHubConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub channels: ChannelsConfig,
    /// Simplified multi-agent routing table: channel -> owner agent id.
    #[serde(default)]
    pub channel_owners: HashMap<String, String>,
    /// Account-level routing overrides: channel -> account_id -> owner agent id.
    #[serde(default)]
    pub channel_account_owners: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub gateway: GatewayConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(
        default = "default_intent_router_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub intent_router: Option<IntentRouterConfig>,
    #[serde(default)]
    pub auto_upgrade: AutoUpgradeConfig,
}

impl Default for Config {
    fn default() -> Self {
        let mut providers = HashMap::new();
        providers.insert(
            "openrouter".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://openrouter.ai/api/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert("anthropic".to_string(), ProviderConfig::default());
        providers.insert("openai".to_string(), ProviderConfig::default());
        providers.insert("deepseek".to_string(), ProviderConfig::default());
        providers.insert(
            "groq".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.groq.com/openai/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert("zhipu".to_string(), ProviderConfig::default());
        providers.insert(
            "vllm".to_string(),
            ProviderConfig {
                api_key: "dummy".to_string(),
                api_base: Some("http://localhost:8000/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "gemini".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some(
                    "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
                ),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "kimi".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.moonshot.cn/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "xai".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.x.ai/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "mistral".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.mistral.ai/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "minimax".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.minimax.chat/v1".to_string()),
                proxy: None,
                api_type: "anthropic".to_string(),
            },
        );
        providers.insert(
            "qwen".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.qwen.ai/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "glm".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.z.ai/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "siliconflow".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.siliconflow.cn/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "ollama".to_string(),
            ProviderConfig {
                api_key: "ollama".to_string(),
                api_base: Some("http://localhost:11434".to_string()),
                proxy: None,
                api_type: "ollama".to_string(),
            },
        );

        Self {
            providers,
            network: NetworkConfig::default(),
            community_hub: CommunityHubConfig::default(),
            agents: AgentsConfig::default(),
            channels: ChannelsConfig::default(),
            channel_owners: HashMap::new(),
            channel_account_owners: HashMap::new(),
            gateway: GatewayConfig::default(),
            tools: ToolsConfig::default(),
            intent_router: Some(IntentRouterConfig::default()),
            auto_upgrade: AutoUpgradeConfig::default(),
        }
    }
}

pub fn parse_json5_str<T>(content: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    json5::from_str(content)
        .map_err(|e| crate::error::Error::Config(format!("Invalid JSON5 format: {}", e)))
}

pub fn parse_json5_value(content: &str) -> Result<Value> {
    parse_json5_str(content)
}

pub fn stringify_json5_pretty<T>(value: &T) -> Result<String>
where
    T: Serialize,
{
    Ok(serde_json::to_string_pretty(value)?)
}

pub fn write_json5_pretty<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    let content = stringify_json5_pretty(value)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

pub fn validate_config_json5_str(content: &str) -> Result<Config> {
    parse_json5_str(content)
}

pub fn write_raw_validated_config_json5(path: &Path, content: &str) -> Result<Config> {
    let config = validate_config_json5_str(content)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(config)
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = parse_json5_str(&content)?;
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
        write_json5_pretty(path, self)
    }

    pub fn get_api_key(&self) -> Option<(&str, &ProviderConfig)> {
        let priority = [
            "openrouter",
            "deepseek",
            "anthropic",
            "openai",
            "kimi",
            "gemini",
            "zhipu",
            "groq",
            "vllm",
            "ollama",
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

    pub fn resolve_channel_owner(&self, channel: &str) -> Option<&str> {
        self.channel_owners
            .get(channel)
            .map(|owner| owner.as_str())
            .filter(|owner| !owner.trim().is_empty())
    }

    pub fn resolve_channel_account_owner(&self, channel: &str, account_id: &str) -> Option<&str> {
        let account_id = account_id.trim();
        if account_id.is_empty() {
            return None;
        }

        self.channel_account_owners
            .get(channel)
            .and_then(|owners| owners.get(account_id))
            .map(|owner| owner.as_str())
            .filter(|owner| !owner.trim().is_empty())
    }

    pub fn resolve_effective_channel_owner(
        &self,
        channel: &str,
        account_id: Option<&str>,
    ) -> Option<&str> {
        account_id
            .and_then(|account_id| self.resolve_channel_account_owner(channel, account_id))
            .or_else(|| self.resolve_channel_owner(channel))
    }

    pub fn is_external_channel_enabled(&self, channel: &str) -> bool {
        match channel {
            "telegram" => self.channels.telegram.enabled,
            "whatsapp" => self.channels.whatsapp.enabled,
            "feishu" => self.channels.feishu.enabled,
            "slack" => self.channels.slack.enabled,
            "discord" => self.channels.discord.enabled,
            "dingtalk" => self.channels.dingtalk.enabled,
            "wecom" => self.channels.wecom.enabled,
            "lark" => self.channels.lark.enabled,
            _ => false,
        }
    }

    pub fn known_agent_ids(&self) -> Vec<String> {
        let mut ids = vec!["default".to_string()];
        for agent in self.agents.list.iter().filter(|agent| agent.enabled) {
            let agent_id = agent.id.trim();
            if agent_id.is_empty() || agent_id == "default" {
                continue;
            }
            if !ids.iter().any(|id| id == agent_id) {
                ids.push(agent_id.to_string());
            }
        }
        ids
    }

    pub fn agent_exists(&self, agent_id: &str) -> bool {
        let agent_id = agent_id.trim();
        !agent_id.is_empty() && self.known_agent_ids().iter().any(|id| id == agent_id)
    }

    pub fn resolve_agent_spec(&self, agent_id: &str) -> Option<ResolvedAgentConfig> {
        let agent_id = agent_id.trim();
        if agent_id.is_empty() {
            return None;
        }

        let agent = self
            .agents
            .list
            .iter()
            .find(|agent| agent.enabled && agent.id.trim() == agent_id);

        if agent_id != "default" && agent.is_none() {
            return None;
        }

        let mut defaults = self.agents.defaults.clone();
        if let Some(agent) = agent {
            let explicit_model = agent
                .model
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let explicit_provider = agent
                .provider
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let has_single_model_override = explicit_model.is_some() || explicit_provider.is_some();

            if let Some(model) = explicit_model {
                defaults.model = model;
            }
            if let Some(provider) = explicit_provider {
                defaults.provider = Some(provider);
            }
            if !agent.model_pool.is_empty() {
                defaults.model_pool = agent.model_pool.clone();
            } else if has_single_model_override {
                defaults.model_pool.clear();
            }
            if let Some(max_tokens) = agent.max_tokens {
                defaults.max_tokens = max_tokens;
            }
            if let Some(temperature) = agent.temperature {
                defaults.temperature = temperature;
            }
            if let Some(max_tool_iterations) = agent.max_tool_iterations {
                defaults.max_tool_iterations = max_tool_iterations;
            }
            if let Some(llm_max_retries) = agent.llm_max_retries {
                defaults.llm_max_retries = llm_max_retries;
            }
            if let Some(llm_retry_delay_ms) = agent.llm_retry_delay_ms {
                defaults.llm_retry_delay_ms = llm_retry_delay_ms;
            }
            if let Some(max_context_tokens) = agent.max_context_tokens {
                defaults.max_context_tokens = max_context_tokens;
            }
            if let Some(evolution_model) = agent
                .evolution_model
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
            {
                defaults.evolution_model = Some(evolution_model);
            }
            if let Some(evolution_provider) = agent
                .evolution_provider
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
            {
                defaults.evolution_provider = Some(evolution_provider);
            }
            if let Some(allowed_mcp_servers) = &agent.allowed_mcp_servers {
                defaults.allowed_mcp_servers = allowed_mcp_servers.clone();
            }
            if let Some(allowed_mcp_tools) = &agent.allowed_mcp_tools {
                defaults.allowed_mcp_tools = allowed_mcp_tools.clone();
            }
        }

        Some(ResolvedAgentConfig {
            id: agent_id.to_string(),
            name: agent.and_then(|entry| entry.name.clone()),
            defaults,
            intent_profile: self.resolve_intent_profile_id(Some(agent_id)),
        })
    }

    pub fn resolved_agents(&self) -> Vec<ResolvedAgentConfig> {
        self.known_agent_ids()
            .into_iter()
            .filter_map(|agent_id| self.resolve_agent_spec(&agent_id))
            .collect()
    }

    pub fn config_for_agent(&self, agent_id: &str) -> Option<Config> {
        let resolved = self.resolve_agent_spec(agent_id)?;
        let mut config = self.clone();
        config.agents.defaults = resolved.defaults;
        Some(config)
    }

    pub fn resolve_intent_profile_id(&self, agent_id: Option<&str>) -> Option<String> {
        let router = self.intent_router.clone().unwrap_or_default();

        let requested_agent_id = agent_id.map(str::trim).filter(|id| !id.is_empty());

        if let Some(agent_id) = requested_agent_id {
            if let Some(profile) = self
                .agents
                .list
                .iter()
                .find(|agent| agent.enabled && agent.id.trim() == agent_id)
                .and_then(|agent| agent.intent_profile.as_deref())
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
            {
                return Some(profile.to_string());
            }

            if let Some(profile) = router
                .agent_profiles
                .get(agent_id)
                .map(String::as_str)
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
            {
                return Some(profile.to_string());
            }
        }

        let default_profile = router.default_profile.trim();
        if default_profile.is_empty() {
            None
        } else {
            Some(default_profile.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blockcell-config-tests-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir.join(name)
    }

    #[test]
    fn test_config_load_accepts_json5_comments_and_trailing_commas() {
        let path = temp_config_path("config.json5");
        fs::write(
            &path,
            r#"{
  // provider config in JSON5
  providers: {
    openai: {
      apiKey: 'sk-test',
    },
  },
  agents: {
    defaults: {
      model: 'gpt-4.1',
    },
  },
}"#,
        )
        .expect("write config.json5");

        let cfg = Config::load(&path).expect("load json5 config");
        assert_eq!(cfg.agents.defaults.model, "gpt-4.1");
        assert_eq!(
            cfg.providers.get("openai").map(|p| p.api_key.as_str()),
            Some("sk-test")
        );
    }

    #[test]
    fn test_config_save_round_trips_via_json5_loader() {
        let path = temp_config_path("config.json5");
        let mut cfg = Config::default();
        cfg.agents.defaults.model = "deepseek-chat".to_string();

        cfg.save(&path).expect("save config json5");
        let content = fs::read_to_string(&path).expect("read saved config");
        assert!(content.contains("deepseek-chat"));

        let loaded = Config::load(&path).expect("reload saved config");
        assert_eq!(loaded.agents.defaults.model, "deepseek-chat");
    }

    #[test]
    fn test_community_hub_top_level() {
        let raw = r#"{
  "communityHub": { "hubUrl": "http://example.com/", "apiKey": "k" },
  "providers": {}
}"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();
        assert_eq!(
            cfg.community_hub_url().as_deref(),
            Some("http://example.com")
        );
        assert_eq!(cfg.community_hub_api_key().as_deref(), Some("k"));
    }

    #[test]
    fn test_channel_owners_and_accounts_deserialize() {
        let raw = r#"{
  "agents": {
    "list": [
      { "id": "chat", "enabled": true }
    ]
  },
  "channelOwners": {
    "telegram": "chat"
  },
  "channels": {
    "telegram": {
      "enabled": true,
      "defaultAccountId": "default",
      "accounts": {
        "default": {
          "enabled": true,
          "token": "tg-token"
        }
      }
    }
  }
}"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();
        assert_eq!(cfg.resolve_channel_owner("telegram"), Some("chat"));
        assert!(cfg.is_external_channel_enabled("telegram"));
        assert_eq!(
            cfg.channels.telegram.default_account_id.as_deref(),
            Some("default")
        );
        let acc = cfg.channels.telegram.accounts.get("default").unwrap();
        assert_eq!(acc.token, "tg-token");
        assert!(cfg.agent_exists("chat"));
    }

    #[test]
    fn test_channel_account_owner_override_deserializes_and_resolves() {
        let raw = r#"{
  "agents": {
    "list": [
      { "id": "ops", "enabled": true }
    ]
  },
  "channelOwners": {
    "telegram": "default"
  },
  "channelAccountOwners": {
    "telegram": {
      "bot2": "ops"
    }
  },
  "channels": {
    "telegram": {
      "enabled": true,
      "accounts": {
        "bot1": { "enabled": true, "token": "tg-bot1" },
        "bot2": { "enabled": true, "token": "tg-bot2" }
      }
    }
  }
}"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();

        assert_eq!(
            cfg.resolve_channel_account_owner("telegram", "bot2"),
            Some("ops")
        );
        assert_eq!(
            cfg.resolve_effective_channel_owner("telegram", Some("bot2")),
            Some("ops")
        );
        assert_eq!(
            cfg.resolve_effective_channel_owner("telegram", Some("bot1")),
            Some("default")
        );
        assert_eq!(
            cfg.resolve_effective_channel_owner("telegram", None),
            Some("default")
        );
    }

    #[test]
    fn test_channel_account_owner_resolution_ignores_blank_values() {
        let raw = r#"{
  "channelOwners": {
    "telegram": "default"
  },
  "channelAccountOwners": {
    "telegram": {
      "bot1": "   "
    }
  }
}"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();

        assert_eq!(cfg.resolve_channel_account_owner("telegram", "bot1"), None);
        assert_eq!(
            cfg.resolve_effective_channel_owner("telegram", Some("bot1")),
            Some("default")
        );
    }

    #[test]
    fn test_legacy_single_channel_fields_still_work() {
        let raw = r#"{
  "channels": {
    "telegram": {
      "enabled": true,
      "token": "legacy-token"
    }
  }
}"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();
        assert_eq!(cfg.channels.telegram.token, "legacy-token");
        assert!(cfg.channels.telegram.accounts.is_empty());
        assert_eq!(cfg.channels.telegram.default_account_id, None);
        assert!(cfg.agent_exists("default"));
    }

    #[test]
    fn test_known_agent_ids_fallback_to_default() {
        let cfg = Config::default();
        let ids = cfg.known_agent_ids();
        assert_eq!(ids, vec!["default".to_string()]);
    }

    #[test]
    fn test_intent_router_deserializes_and_resolves_agent_profile() {
        let raw = r#"{
  "agents": {
    "list": [
      { "id": "ops", "enabled": true, "intentProfile": "ops" }
    ]
  },
  "intentRouter": {
    "enabled": true,
    "defaultProfile": "default",
    "profiles": {
      "default": {
        "coreTools": ["read_file", "message"],
        "intentTools": {
          "Chat": { "inheritBase": false, "tools": [] },
          "Unknown": ["browse"]
        }
      },
      "ops": {
        "coreTools": ["read_file", "exec"],
        "intentTools": {
          "DevOps": ["git_api"],
          "Unknown": ["http_request"]
        },
        "denyTools": ["email"]
      }
    }
  }
}"#;

        let cfg: Config = serde_json::from_str(raw).unwrap();
        let router = cfg.intent_router.as_ref().expect("intent router");
        assert!(router.enabled);
        assert_eq!(
            cfg.resolve_intent_profile_id(Some("ops")),
            Some("ops".to_string())
        );
        assert_eq!(
            cfg.resolve_intent_profile_id(Some("missing")),
            Some("default".to_string())
        );
        assert_eq!(
            cfg.resolve_intent_profile_id(None),
            Some("default".to_string())
        );
    }

    #[test]
    fn test_default_config_includes_intent_router_defaults() {
        let cfg = Config::default();
        let router = cfg.intent_router.as_ref().expect("default intent router");

        assert!(router.profiles.contains_key("default"));
        assert_eq!(
            cfg.resolve_intent_profile_id(Some("default")),
            Some("default".to_string())
        );
    }

    #[test]
    fn test_missing_intent_router_uses_default_router() {
        let cfg: Config = serde_json::from_str("{}").unwrap();
        let router = cfg.intent_router.as_ref().expect("defaulted intent router");

        assert!(router.enabled);
        assert!(router.profiles.contains_key("default"));
        assert_eq!(
            cfg.resolve_intent_profile_id(None),
            Some("default".to_string())
        );
    }

    #[test]
    fn test_resolved_agent_falls_back_to_implicit_default() {
        let cfg = Config::default();
        let resolved = cfg
            .resolve_agent_spec("default")
            .expect("implicit default agent");

        assert_eq!(resolved.id, "default");
        assert_eq!(resolved.defaults.model, cfg.agents.defaults.model);
        assert_eq!(resolved.defaults.provider, cfg.agents.defaults.provider);
        assert_eq!(resolved.intent_profile.as_deref(), Some("default"));
    }

    #[test]
    fn test_resolved_agent_inherits_and_overrides_defaults() {
        let raw = r#"{
  "agents": {
    "defaults": {
      "model": "deepseek-chat",
      "provider": "deepseek",
      "modelPool": [
        { "model": "deepseek-chat", "provider": "deepseek", "weight": 1, "priority": 1 }
      ]
    },
    "list": [
      {
        "id": "ops",
        "enabled": true,
        "intentProfile": "ops",
        "model": "gpt-4.1",
        "provider": "openai"
      }
    ]
  },
  "intentRouter": {
    "enabled": true,
    "defaultProfile": "default",
    "profiles": {
      "default": {
        "coreTools": ["read_file"],
        "intentTools": { "Unknown": [] }
      },
      "ops": {
        "coreTools": ["exec"],
        "intentTools": { "Unknown": ["http_request"] }
      }
    }
  }
}"#;

        let cfg: Config = serde_json::from_str(raw).unwrap();
        let resolved = cfg.resolve_agent_spec("ops").expect("resolved ops agent");

        assert_eq!(resolved.id, "ops");
        assert_eq!(resolved.defaults.model, "gpt-4.1");
        assert_eq!(resolved.defaults.provider.as_deref(), Some("openai"));
        assert!(
            resolved.defaults.model_pool.is_empty(),
            "explicit model/provider override should disable inherited model_pool"
        );
        assert_eq!(resolved.intent_profile.as_deref(), Some("ops"));
    }

    #[test]
    fn test_resolved_agents_always_include_default() {
        let raw = r#"{
  "agents": {
    "list": [
      { "id": "ops", "enabled": true, "intentProfile": "ops" }
    ]
  },
  "intentRouter": {
    "enabled": true,
    "defaultProfile": "default",
    "profiles": {
      "default": {
        "coreTools": ["read_file"],
        "intentTools": { "Unknown": [] }
      },
      "ops": {
        "coreTools": ["exec"],
        "intentTools": { "Unknown": ["http_request"] }
      }
    }
  }
}"#;

        let cfg: Config = serde_json::from_str(raw).unwrap();
        let ids: Vec<String> = cfg
            .resolved_agents()
            .into_iter()
            .map(|agent| agent.id)
            .collect();
        assert_eq!(ids, vec!["default".to_string(), "ops".to_string()]);
    }
}
