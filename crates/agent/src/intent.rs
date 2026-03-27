use blockcell_core::mcp_config::McpResolvedConfig;
use blockcell_core::{Config, Error, Result};
use blockcell_tools::ToolRegistry;
use regex::Regex;
use std::collections::HashSet;

/// Intent categories for user messages.
/// Used to determine which tools, rules, and domain knowledge to load.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IntentCategory {
    /// 日常闲聊、问候、闲谈 — 不需要任何工具
    Chat,
    /// 文件/代码操作 — read_file, write_file, edit_file, list_dir, exec, file_ops
    FileOps,
    /// 网页/搜索 — web_search, web_fetch, browse
    WebSearch,
    /// 金融/行情/告警 — alert_rule, stream_subscribe, ...
    Finance,
    /// 区块链/链上资产相关请求
    Blockchain,
    /// 数据处理/可视化 — data_process, chart_generate, office_write
    DataAnalysis,
    /// 通信/邮件/消息 — email, message
    Communication,
    /// 系统/硬件/应用控制/Android — system_info, app_control, camera_capture, termux_api
    SystemControl,
    /// 日程/任务/记忆 — cron, memory_*, knowledge_graph, list_tasks
    Organization,
    /// IoT/设备控制类请求
    IoT,
    /// 媒体处理 — audio_transcribe, tts, ocr, image_understand, video_process
    Media,
    /// 开发/运维 — network_monitor, encrypt
    DevOps,
    /// 健康/生活类请求
    Lifestyle,
    /// 无法判断 — 加载核心工具集
    Unknown,
}

impl IntentCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            IntentCategory::Chat => "Chat",
            IntentCategory::FileOps => "FileOps",
            IntentCategory::WebSearch => "WebSearch",
            IntentCategory::Finance => "Finance",
            IntentCategory::Blockchain => "Blockchain",
            IntentCategory::DataAnalysis => "DataAnalysis",
            IntentCategory::Communication => "Communication",
            IntentCategory::SystemControl => "SystemControl",
            IntentCategory::Organization => "Organization",
            IntentCategory::IoT => "IoT",
            IntentCategory::Media => "Media",
            IntentCategory::DevOps => "DevOps",
            IntentCategory::Lifestyle => "Lifestyle",
            IntentCategory::Unknown => "Unknown",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim() {
            "Chat" => Some(IntentCategory::Chat),
            "FileOps" => Some(IntentCategory::FileOps),
            "WebSearch" => Some(IntentCategory::WebSearch),
            "Finance" => Some(IntentCategory::Finance),
            "Blockchain" => Some(IntentCategory::Blockchain),
            "DataAnalysis" => Some(IntentCategory::DataAnalysis),
            "Communication" => Some(IntentCategory::Communication),
            "SystemControl" => Some(IntentCategory::SystemControl),
            "Organization" => Some(IntentCategory::Organization),
            "IoT" => Some(IntentCategory::IoT),
            "Media" => Some(IntentCategory::Media),
            "DevOps" => Some(IntentCategory::DevOps),
            "Lifestyle" => Some(IntentCategory::Lifestyle),
            "Unknown" => Some(IntentCategory::Unknown),
            _ => None,
        }
    }
}

struct IntentRule {
    category: IntentCategory,
    keywords: Vec<&'static str>,
    patterns: Vec<Regex>,
    negative: Vec<&'static str>,
    priority: u8,
}

pub struct IntentClassifier {
    rules: Vec<IntentRule>,
}

impl Default for IntentClassifier {
    fn default() -> Self {
        Self::new()
    }
}

impl IntentClassifier {
    pub fn new() -> Self {
        let rules = vec![
            // ── Chat (highest priority) ──
            IntentRule {
                category: IntentCategory::Chat,
                keywords: vec![],
                patterns: vec![
                    Regex::new(r"(?i)^(你好|hi|hello|hey|嗨|早安|晚安|早上好|下午好|晚上好|good\s*(morning|afternoon|evening))[\s!！。.？?~～]*$").unwrap(),
                    Regex::new(r"(?i)^(谢谢|感谢|辛苦了|好的|明白了|知道了|ok|okay|got\s*it|thanks|thank\s*you)[\s!！。.？?~～]*$").unwrap(),
                    Regex::new(r"(?i)^(再见|拜拜|bye|goodbye|see\s*you)[\s!！。.？?~～]*$").unwrap(),
                    Regex::new(r"(?i)^(你是谁|who\s*are\s*you|你能做什么|what\s*can\s*you\s*do|帮助|help)[\s？?]*$").unwrap(),
                    Regex::new(r"(?i)^(哈哈|嘿嘿|呵呵|lol|haha|😂|👍|🙏|❤️|😊)[\s!！。.？?~～]*$").unwrap(),
                ],
                negative: vec![],
                priority: 10,
            },
        ];

        Self { rules }
    }

    /// Classify user input into one or more intent categories.
    /// Returns up to 2 categories, sorted by priority.
    pub fn classify(&self, input: &str) -> Vec<IntentCategory> {
        let input_lower = input.to_lowercase();
        let mut matches: Vec<(IntentCategory, u8)> = Vec::new();

        for rule in &self.rules {
            if self.rule_matches(rule, input, &input_lower) {
                matches.push((rule.category.clone(), rule.priority));
            }
        }

        if matches.is_empty() {
            return vec![IntentCategory::Unknown];
        }

        // Sort by priority descending
        matches.sort_by(|a, b| b.1.cmp(&a.1));
        matches.dedup_by(|a, b| a.0 == b.0);

        // If Chat is the only match, return it alone
        if matches.len() == 1 && matches[0].0 == IntentCategory::Chat {
            return vec![IntentCategory::Chat];
        }

        // If Chat is matched alongside other intents, drop Chat
        matches.retain(|m| m.0 != IntentCategory::Chat);

        if matches.is_empty() {
            return vec![IntentCategory::Unknown];
        }

        // Take top 2
        matches.into_iter().take(2).map(|(c, _)| c).collect()
    }

    fn rule_matches(&self, rule: &IntentRule, input: &str, input_lower: &str) -> bool {
        // Check negative keywords first
        for neg in &rule.negative {
            if input_lower.contains(&neg.to_lowercase()) {
                return false;
            }
        }

        // Check regex patterns
        for pattern in &rule.patterns {
            if pattern.is_match(input) {
                return true;
            }
        }

        // Check keywords
        for keyword in &rule.keywords {
            if input_lower.contains(&keyword.to_lowercase()) {
                return true;
            }
        }

        false
    }
}

pub struct IntentToolResolver<'a> {
    config: &'a Config,
}

impl<'a> IntentToolResolver<'a> {
    pub fn new(config: &'a Config) -> Self {
        Self { config }
    }

    pub fn resolve_tool_names(
        &self,
        agent_id: Option<&str>,
        intents: &[IntentCategory],
        available_tools: Option<&HashSet<String>>,
    ) -> Option<Vec<String>> {
        let default_router;
        let router = if let Some(router) = self.config.intent_router.as_ref() {
            router
        } else {
            default_router = blockcell_core::config::IntentRouterConfig::default();
            &default_router
        };

        let profile_id = self.config.resolve_intent_profile_id(agent_id)?;
        let profile = router.profiles.get(&profile_id)?;

        let effective_intents: Vec<IntentCategory> = if router.enabled {
            intents.to_vec()
        } else {
            vec![IntentCategory::Unknown]
        };

        let mut tools = HashSet::new();
        for intent in &effective_intents {
            if let Some(entry) = profile.intent_tools.get(intent.as_str()) {
                if entry.inherit_base() {
                    for tool in &profile.core_tools {
                        tools.insert(tool.clone());
                    }
                }
                for tool in entry.tools() {
                    tools.insert(tool.clone());
                }
            } else {
                for tool in &profile.core_tools {
                    tools.insert(tool.clone());
                }
            }
        }

        for tool in &profile.deny_tools {
            tools.remove(tool);
        }

        if let Some(available_tools) = available_tools {
            tools.retain(|tool| available_tools.contains(tool));
        }

        let mut result: Vec<String> = tools.into_iter().collect();
        result.sort();
        Some(result)
    }

    pub fn validate(&self, registry: &ToolRegistry) -> Result<()> {
        self.validate_with_mcp(registry, None)
    }

    pub fn validate_with_mcp(
        &self,
        registry: &ToolRegistry,
        mcp: Option<&McpResolvedConfig>,
    ) -> Result<()> {
        let default_router;
        let router = if let Some(router) = self.config.intent_router.as_ref() {
            router
        } else {
            default_router = blockcell_core::config::IntentRouterConfig::default();
            &default_router
        };

        let default_profile = router.default_profile.trim();
        if default_profile.is_empty() {
            return Err(Error::Config(
                "intentRouter.defaultProfile must not be empty".to_string(),
            ));
        }
        if !router.profiles.contains_key(default_profile) {
            return Err(Error::Config(format!(
                "intentRouter.defaultProfile '{}' does not exist",
                default_profile
            )));
        }

        for (agent_id, profile_id) in &router.agent_profiles {
            if !router.profiles.contains_key(profile_id) {
                return Err(Error::Config(format!(
                    "intentRouter.agentProfiles.{} references missing profile '{}'",
                    agent_id, profile_id
                )));
            }
        }

        for agent in &self.config.agents.list {
            if let Some(profile_id) = agent.intent_profile.as_deref() {
                let profile_id = profile_id.trim();
                if !profile_id.is_empty() && !router.profiles.contains_key(profile_id) {
                    return Err(Error::Config(format!(
                        "agents.list[{}].intentProfile references missing profile '{}'",
                        agent.id, profile_id
                    )));
                }
            }
        }

        let registered: HashSet<String> = registry.tool_names().into_iter().collect();
        for (profile_name, profile) in &router.profiles {
            if !profile
                .intent_tools
                .contains_key(IntentCategory::Unknown.as_str())
            {
                return Err(Error::Config(format!(
                    "intentRouter.profiles.{} must configure Unknown intent",
                    profile_name
                )));
            }

            for intent_name in profile.intent_tools.keys() {
                if IntentCategory::from_name(intent_name).is_none() {
                    return Err(Error::Config(format!(
                        "intentRouter.profiles.{}.intentTools contains invalid intent '{}'",
                        profile_name, intent_name
                    )));
                }
            }

            for tool in &profile.core_tools {
                ensure_known_tool(self.config, mcp, profile_name, tool, &registered)?;
            }
            for tool in &profile.deny_tools {
                ensure_known_tool(self.config, mcp, profile_name, tool, &registered)?;
            }
            for entry in profile.intent_tools.values() {
                for tool in entry.tools() {
                    ensure_known_tool(self.config, mcp, profile_name, tool, &registered)?;
                }
            }
        }

        Ok(())
    }
}

fn ensure_known_tool(
    config: &Config,
    mcp: Option<&McpResolvedConfig>,
    profile_name: &str,
    tool_name: &str,
    registered: &HashSet<String>,
) -> Result<()> {
    if registered.contains(tool_name) || declared_mcp_tool(config, mcp, tool_name, registered) {
        Ok(())
    } else if is_optional_feature_tool(tool_name) {
        // Optional feature tools (e.g., napcat_*) may not be registered if the feature is disabled
        Ok(())
    } else {
        Err(Error::Config(format!(
            "intentRouter.profiles.{} references unknown tool '{}'",
            profile_name, tool_name
        )))
    }
}

/// Check if a tool is from an optional feature that may not be enabled.
/// These tools are allowed to be missing from the registry.
fn is_optional_feature_tool(tool_name: &str) -> bool {
    tool_name.starts_with("napcat_")
}

fn declared_mcp_tool(
    _config: &Config,
    mcp: Option<&McpResolvedConfig>,
    tool_name: &str,
    registered: &HashSet<String>,
) -> bool {
    let Some((server_name, tool_suffix)) = tool_name.split_once("__") else {
        return false;
    };

    let server_name = server_name.trim();
    let tool_suffix = tool_suffix.trim();
    if server_name.is_empty() || tool_suffix.is_empty() {
        return false;
    }

    let Some(mcp) = mcp else {
        return false;
    };
    let Some(server) = mcp.servers.get(server_name) else {
        return false;
    };
    if !server.enabled {
        return false;
    }

    let server_prefix = format!("{}__", server_name);
    let discovered_server_tools = registered
        .iter()
        .filter(|name| name.starts_with(&server_prefix))
        .count();

    if discovered_server_tools == 0 {
        true
    } else {
        registered.contains(tool_name)
    }
}

/// Check if the intents should show skills list.
pub fn needs_skills_list(intents: &[IntentCategory]) -> bool {
    !intents.iter().any(|i| matches!(i, IntentCategory::Chat))
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::Config;
    use blockcell_tools::ToolRegistry;

    #[test]
    fn test_chat_classification() {
        let classifier = IntentClassifier::new();
        assert_eq!(classifier.classify("你好"), vec![IntentCategory::Chat]);
        assert_eq!(classifier.classify("hello"), vec![IntentCategory::Chat]);
        assert_eq!(classifier.classify("Hi!"), vec![IntentCategory::Chat]);
        assert_eq!(classifier.classify("谢谢"), vec![IntentCategory::Chat]);
        assert_eq!(classifier.classify("再见"), vec![IntentCategory::Chat]);
        assert_eq!(classifier.classify("你是谁?"), vec![IntentCategory::Chat]);
    }

    #[test]
    fn test_non_chat_classification_falls_back_to_unknown() {
        let classifier = IntentClassifier::new();
        assert_eq!(
            classifier.classify("查一下茅台股价"),
            vec![IntentCategory::Unknown]
        );
        assert_eq!(
            classifier.classify("0x1234567890abcdef1234567890abcdef12345678 这个地址安全吗"),
            vec![IntentCategory::Unknown]
        );
        assert_eq!(
            classifier.classify("帮我读一下 config.json5"),
            vec![IntentCategory::Unknown]
        );
        assert_eq!(
            classifier.classify("帮我做一件复杂的事情"),
            vec![IntentCategory::Unknown]
        );
    }

    #[test]
    fn test_intent_router_resolves_chat_without_base_tools() {
        let config = Config::default();
        let resolver = IntentToolResolver::new(&config);

        let tools = resolver
            .resolve_tool_names(None, &[IntentCategory::Chat], None)
            .expect("config router tools");

        assert!(tools.is_empty());
    }

    #[test]
    fn test_intent_router_resolves_agent_profile_and_applies_deny_list() {
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
        "coreTools": ["read_file", "exec", "email"],
        "intentTools": {
          "DevOps": ["network_monitor", "http_request"],
          "Unknown": ["http_request"]
        },
        "denyTools": ["email"]
      }
    }
  }
}"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let resolver = IntentToolResolver::new(&config);

        let tools = resolver
            .resolve_tool_names(Some("ops"), &[IntentCategory::DevOps], None)
            .expect("config router tools");

        assert!(tools.contains(&"read_file".to_string()));
        assert!(tools.contains(&"exec".to_string()));
        assert!(tools.contains(&"network_monitor".to_string()));
        assert!(!tools.contains(&"email".to_string()));
    }

    #[test]
    fn test_intent_router_uses_default_router_for_missing_config() {
        let config: Config = serde_json::from_str("{}").unwrap();
        let resolver = IntentToolResolver::new(&config);
        let tools = resolver
            .resolve_tool_names(None, &[IntentCategory::Unknown], None)
            .expect("default intent router tools");

        assert!(tools.contains(&"read_file".to_string()));
        assert!(tools.contains(&"browse".to_string()));
    }

    #[test]
    fn test_intent_router_validation_rejects_invalid_tools() {
        let raw = r#"{
  "intentRouter": {
    "enabled": true,
    "defaultProfile": "default",
    "profiles": {
      "default": {
        "coreTools": ["read_file"],
        "intentTools": {
          "Chat": { "inheritBase": false, "tools": [] },
          "Unknown": ["definitely_missing_tool"]
        }
      }
    }
  }
}"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let resolver = IntentToolResolver::new(&config);
        let registry = ToolRegistry::with_defaults();

        assert!(resolver.validate(&registry).is_err());
    }

    #[test]
    fn test_intent_router_validation_accepts_declared_mcp_tool_prefix() {
        let raw = r#"{
  "intentRouter": {
    "enabled": true,
    "defaultProfile": "default",
    "profiles": {
      "default": {
        "coreTools": ["read_file"],
        "intentTools": {
          "Chat": { "inheritBase": false, "tools": [] },
          "Unknown": ["github__search_repositories"]
        }
      }
    }
  }
}"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let resolver = IntentToolResolver::new(&config);
        let registry = ToolRegistry::with_defaults();
        let mut servers = std::collections::HashMap::new();
        servers.insert(
            "github".to_string(),
            blockcell_core::mcp_config::McpServerConfig {
                command: "npx".to_string(),
                args: vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-github".to_string(),
                ],
                env: std::collections::HashMap::new(),
                cwd: None,
                enabled: true,
                auto_start: true,
                startup_timeout_secs: 20,
                call_timeout_secs: 60,
            },
        );
        let mcp = blockcell_core::mcp_config::McpResolvedConfig {
            defaults: blockcell_core::mcp_config::McpDefaultsConfig::default(),
            servers,
        };

        assert!(resolver.validate_with_mcp(&registry, Some(&mcp)).is_ok());
    }

    #[test]
    fn test_disabled_intent_router_falls_back_to_unknown_profile_tools() {
        let raw = r#"{
  "intentRouter": {
    "enabled": false,
    "defaultProfile": "default",
    "profiles": {
      "default": {
        "coreTools": ["read_file"],
        "intentTools": {
          "Chat": { "inheritBase": false, "tools": [] },
          "Unknown": ["browse"]
        }
      }
    }
  }
}"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let resolver = IntentToolResolver::new(&config);

        let tools = resolver
            .resolve_tool_names(None, &[IntentCategory::Chat], None)
            .expect("disabled router still resolves config tools");

        assert!(tools.contains(&"read_file".to_string()));
        assert!(tools.contains(&"browse".to_string()));
    }
}
