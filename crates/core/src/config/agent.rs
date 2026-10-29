//! Agent 默认值与多 agent 档案配置类型。
//!
//! 包含 AgentDefaults、AgentsConfig、AgentProfileConfig、ResolvedAgentConfig。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{GhostConfig, ModelEntry, RoutingStrategy};

fn is_default_routing_strategy(strategy: &RoutingStrategy) -> bool {
    matches!(strategy, RoutingStrategy::Manual)
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
    /// Per-tool max iterations. If a tool name is not present, use max_tool_iterations as default.
    #[serde(default)]
    pub max_tool_iterations_by_tool: HashMap<String, u32>,
    #[serde(default = "default_llm_max_retries")]
    pub llm_max_retries: u32,
    #[serde(default = "default_llm_retry_delay_ms")]
    pub llm_retry_delay_ms: u64,
    /// 首个流式事件允许等待的秒数。
    #[serde(default = "default_stream_first_event_timeout_secs")]
    pub stream_first_event_timeout_secs: u64,
    /// 流式事件之间允许空闲的秒数。
    #[serde(default = "default_stream_idle_timeout_secs")]
    pub stream_idle_timeout_secs: u64,
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
    /// ProviderPool routing strategy for this agent.
    #[serde(default, skip_serializing_if = "is_default_routing_strategy")]
    pub routing_strategy: RoutingStrategy,
    /// Allowed MCP server names visible to this agent.
    #[serde(default)]
    pub allowed_mcp_servers: Vec<String>,
    /// Allowed MCP tool names visible to this agent.
    #[serde(default)]
    pub allowed_mcp_tools: Vec<String>,
    /// 推理强度控制（DeepSeek V4 thinking mode 等）：
    /// - "off": 禁用思考 (thinking.type = disabled)
    /// - "low"/"medium"/"high": reasoning_effort = high (DeepSeek 默认)
    /// - "max": reasoning_effort = max (最深推理)
    /// - None: 不发送任何参数，由 provider 自行决定
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// 按工具名覆盖"模型/界面看到什么"（描述、可见性、分组、排序），不影响能否执行。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tool_presentation: HashMap<String, ToolPresentationConfig>,
}

/// 工具对模型的可见性，与执行权限无关。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolVisibility {
    #[default]
    Visible,
    /// 初始工具列表中不展示；经渐进发现（如 MCP 搜索）或被技能显式声明后才展示。
    Discoverable,
}

/// 单个工具的展示覆盖；未设置的字段沿用工具自身声明。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolPresentationConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<ToolVisibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// 越小越靠前；相同时按名称。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,
}

impl ToolPresentationConfig {
    /// 以 `other` 中已设置的字段覆盖自身。
    pub fn overlay(&self, other: &Self) -> Self {
        Self {
            description: other
                .description
                .clone()
                .or_else(|| self.description.clone()),
            visibility: other.visibility.or(self.visibility),
            group: other.group.clone().or_else(|| self.group.clone()),
            order: other.order.or(self.order),
        }
    }
}

fn default_workspace() -> String {
    "~/.blockcell/workspace".to_string()
}

fn default_model() -> String {
    // 默认走 DeepSeek 的便宜档（V4.1-Flash-1017）；更强推理可显式改成 deepseek-v4-pro
    "deepseek-flash".to_string()
}

fn default_max_tokens() -> u32 {
    8192
}

fn default_temperature() -> f32 {
    0.7
}

fn default_max_tool_iterations() -> u32 {
    200
}

fn default_llm_max_retries() -> u32 {
    6
}

fn default_llm_retry_delay_ms() -> u64 {
    2000
}

fn default_stream_first_event_timeout_secs() -> u64 {
    60
}

fn default_stream_idle_timeout_secs() -> u64 {
    45
}

fn default_max_context_tokens() -> u32 {
    1_048_576 // 1M 上下文
}

impl Default for AgentDefaults {
    fn default() -> Self {
        Self {
            workspace: default_workspace(),
            model: default_model(),
            max_tokens: default_max_tokens(),
            temperature: default_temperature(),
            max_tool_iterations: default_max_tool_iterations(),
            max_tool_iterations_by_tool: HashMap::new(),
            llm_max_retries: default_llm_max_retries(),
            llm_retry_delay_ms: default_llm_retry_delay_ms(),
            stream_first_event_timeout_secs: default_stream_first_event_timeout_secs(),
            stream_idle_timeout_secs: default_stream_idle_timeout_secs(),
            max_context_tokens: default_max_context_tokens(),
            provider: None,
            evolution_model: None,
            evolution_provider: None,
            model_pool: Vec::new(),
            routing_strategy: RoutingStrategy::Manual,
            allowed_mcp_servers: Vec::new(),
            allowed_mcp_tools: Vec::new(),
            reasoning_effort: None,
            tool_presentation: HashMap::new(),
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
    pub routing_strategy: Option<RoutingStrategy>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tool_iterations: Option<u32>,
    #[serde(default)]
    pub max_tool_iterations_by_tool: HashMap<String, u32>,
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
    /// 本 agent 的工具展示覆盖，按字段叠加在 `agents.defaults.toolPresentation` 之上。
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tool_presentation: HashMap<String, ToolPresentationConfig>,
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
            routing_strategy: None,
            max_tokens: None,
            temperature: None,
            max_tool_iterations: None,
            max_tool_iterations_by_tool: HashMap::new(),
            llm_max_retries: None,
            llm_retry_delay_ms: None,
            max_context_tokens: None,
            evolution_model: None,
            evolution_provider: None,
            allowed_mcp_servers: None,
            allowed_mcp_tools: None,
            tool_presentation: HashMap::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tool_iteration_budget_supports_long_coding_tasks() {
        assert_eq!(AgentDefaults::default().max_tool_iterations, 200);
    }

    #[test]
    fn stream_timeout_defaults_are_bounded_and_legacy_config_deserializes() {
        let defaults = AgentDefaults::default();
        assert_eq!(defaults.stream_first_event_timeout_secs, 60);
        assert_eq!(defaults.stream_idle_timeout_secs, 45);
        let parsed: AgentDefaults = serde_json::from_str(
            r#"{"workspace":"/tmp/ws","model":"test","maxTokens":10,"temperature":0.0,"maxToolIterations":1,"llmMaxRetries":0,"llmRetryDelayMs":0,"maxContextTokens":100}"#,
        )
        .unwrap();
        assert_eq!(parsed.stream_first_event_timeout_secs, 60);
        assert_eq!(parsed.stream_idle_timeout_secs, 45);
    }
}
