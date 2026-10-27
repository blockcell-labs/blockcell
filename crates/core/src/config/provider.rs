//! Provider 与模型池相关配置类型。
//!
//! 包含 ProviderConfig、CommunityHubConfig、ToolCallMode、ModelEntry、
//! RoutingStrategy、NetworkConfig。

use serde::{Deserialize, Serialize};

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
    #[serde(default = "default_community_hub_url")]
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

#[cfg(test)]
mod p3_tests {
    use super::*;

    #[test]
    fn p3_missing_hub_url_uses_default_but_null_disables_it() {
        let config: CommunityHubConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.hub_url, CommunityHubConfig::default().hub_url);
        let disabled: CommunityHubConfig = serde_json::from_str(r#"{"hubUrl":null}"#).unwrap();
        assert_eq!(disabled.hub_url, None);
    }
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum ToolCallMode {
    #[default]
    Native,
    Text,
    None,
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    /// 模型名称，例如 "deepseek-v4-pro"、"claude-sonnet-5-5"
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
    /// 模型专用温度参数（可选）。
    /// 若未配置，则沿用全局 `agents.defaults.temperature`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// 工具调用模式：
    /// - native: 使用 API 原生 tools/tool_calls
    /// - text: 不发送 tools，改为文本协议 <tool_call> ... </tool_call>
    /// - none: 禁用工具
    /// - auto: 先尝试 native，失败或被中继剥离后自动退化为 text
    #[serde(default, skip_serializing_if = "is_default_tool_call_mode")]
    pub tool_call_mode: ToolCallMode,
}

fn default_entry_weight() -> u32 {
    1
}
fn default_entry_priority() -> u32 {
    1
}

fn is_default_tool_call_mode(mode: &ToolCallMode) -> bool {
    matches!(mode, ToolCallMode::Native)
}

/// Model routing strategy for selecting entries from agents.defaults.model_pool.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoutingStrategy {
    /// Existing behavior: priority group plus weighted random selection.
    #[default]
    Manual,
    /// Short contexts use cheaper lower-priority entries; longer contexts use normal priority.
    CostOptimized,
    /// Prefer highest-priority entries.
    QualityFirst,
    /// Prefer entries with the best observed fast-success signal, falling back to normal priority.
    LatencyFirst,
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
