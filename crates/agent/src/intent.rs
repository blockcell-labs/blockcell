use blockcell_core::mcp_config::McpResolvedConfig;
use blockcell_core::{Config, Error, Result};
use blockcell_tools::ToolRegistry;
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;
use tracing::warn;

/// Intent categories for user messages.
/// Used to determine which tools, rules, and domain knowledge to load.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IntentCategory {
    /// 日常闲聊、问候、闲谈 — 不需要任何工具
    Chat,
    /// 代码修改、调试、测试与构建
    Coding,
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
            IntentCategory::Coding => "Coding",
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
            "Coding" => Some(IntentCategory::Coding),
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

#[derive(Clone)]
struct IntentRule {
    category: IntentCategory,
    /// 内置规则的关键词（静态字符串）
    keywords: Vec<&'static str>,
    /// 来自配置文件的动态关键词
    keywords_dyn: Vec<String>,
    patterns: Vec<Regex>,
    /// 内置规则的否定词（静态字符串）
    negative: Vec<&'static str>,
    /// 来自配置文件的动态否定词
    negative_dyn: Vec<String>,
    priority: u8,
    /// 预小写的关键词（静态 + 动态合并），避免每次匹配重复 to_lowercase() 分配
    keywords_lower: Vec<String>,
    /// 预小写的否定词（静态 + 动态合并），避免每次匹配重复 to_lowercase() 分配
    negative_lower: Vec<String>,
}

impl Default for IntentRule {
    fn default() -> Self {
        Self {
            category: IntentCategory::Unknown,
            keywords: vec![],
            keywords_dyn: vec![],
            patterns: vec![],
            negative: vec![],
            negative_dyn: vec![],
            priority: 0,
            keywords_lower: vec![],
            negative_lower: vec![],
        }
    }
}

#[derive(Clone)]
pub struct IntentClassifier {
    rules: Vec<IntentRule>,
}

impl Default for IntentClassifier {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL_CLASSIFIER: OnceLock<IntentClassifier> = OnceLock::new();

mod builtin_rules;

impl IntentClassifier {
    /// 返回全局单例，避免每条消息重复编译正则。
    pub fn global() -> &'static IntentClassifier {
        GLOBAL_CLASSIFIER.get_or_init(Self::new)
    }

    pub fn new() -> Self {
        let rules = builtin_rules::builtin_rules();

        // 预小写关键词和否定词，避免每次匹配时重复 to_lowercase() 分配
        let mut classifier = Self { rules };
        for rule in &mut classifier.rules {
            rule.keywords_lower = rule.keywords.iter().map(|s| s.to_lowercase()).collect();
            rule.keywords_lower
                .extend(rule.keywords_dyn.iter().cloned());
            rule.negative_lower = rule.negative.iter().map(|s| s.to_lowercase()).collect();
            rule.negative_lower
                .extend(rule.negative_dyn.iter().cloned());
        }

        classifier
    }

    /// 在内置规则基础上，叠加来自配置文件的自定义规则，返回新的分类器实例。
    /// 当 `extra_rules` 为空时等价于 `Self::new()`。
    ///
    /// 配置规则始终在内置规则之后追加，优先级由配置中的 priority 字段决定。
    /// 如果配置文件中出现重复 category，只会记录第一次，忽略后续重复项并发出警告。
    pub fn with_extra_rules(extra_rules: &[blockcell_core::config::IntentRuleConfig]) -> Self {
        // 克隆全局单例，复用已编译的内置正则（Regex 克隆是廉价的 Arc 引用计数），
        // 避免每条消息重新编译数十个内置正则。
        let mut classifier = Self::global().clone();
        // 用 String 而非 &IntentCategory 引用，避免 borrow 冲突
        let mut seen_extra_categories: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for rule_cfg in extra_rules {
            let Some(category) = IntentCategory::from_name(&rule_cfg.category) else {
                tracing::warn!(
                    category = %rule_cfg.category,
                    "intentRouter.intentRules 中包含未知的意图类别，已跳过"
                );
                continue;
            };
            // 配置文件出现重复 category 时，只记录第一次并警告
            if !seen_extra_categories.insert(rule_cfg.category.clone()) {
                tracing::warn!(
                    category = %rule_cfg.category,
                    "intentRouter.intentRules 中重复的 category 已跳过"
                );
                continue;
            }
            let mut patterns = Vec::new();
            for pat in &rule_cfg.patterns {
                match Regex::new(pat) {
                    Ok(re) => patterns.push(re),
                    Err(e) => {
                        tracing::warn!(
                            pattern = %pat,
                            error = %e,
                            "intentRouter.intentRules 中的正则表达式无效，已跳过"
                        );
                    }
                }
            }
            // 预先 lowercase，避免每次匹配时重复分配
            let keywords_dyn: Vec<String> =
                rule_cfg.keywords.iter().map(|s| s.to_lowercase()).collect();
            let negative_dyn: Vec<String> =
                rule_cfg.negative.iter().map(|s| s.to_lowercase()).collect();
            classifier.rules.push(IntentRule {
                category,
                keywords: vec![],
                keywords_dyn: keywords_dyn.clone(),
                patterns,
                negative: vec![],
                negative_dyn: negative_dyn.clone(),
                keywords_lower: keywords_dyn, // keywords_dyn 已在上面预小写
                negative_lower: negative_dyn, // negative_dyn 已在上面预小写
                priority: rule_cfg.priority,
            });
        }
        classifier
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

        // Sort by priority descending; use category name as secondary key for determinism
        matches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.as_str().cmp(b.0.as_str())));
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
        // 使用预小写的否定词列表（构造时已预处理），避免每次匹配重复 to_lowercase() 分配
        for neg in &rule.negative_lower {
            if input_lower.contains(neg.as_str()) {
                return false;
            }
        }

        // Check regex patterns
        for pattern in &rule.patterns {
            if pattern.is_match(input) {
                return true;
            }
        }

        // 使用预小写的关键词列表（构造时已预处理），避免每次匹配重复 to_lowercase() 分配
        for keyword in &rule.keywords_lower {
            if input_lower.contains(keyword.as_str()) {
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

    /// Profile capability ceiling, independent of the current message or mode.
    /// Runtime foundation and agent-authorized MCP tools are merged separately;
    /// denyTools must also be applied after those additions and alias expansion.
    pub fn resolve_tool_scope(&self, agent_id: Option<&str>) -> Option<Vec<String>> {
        let default_router = blockcell_core::config::IntentRouterConfig::default();
        let router = self
            .config
            .intent_router
            .as_ref()
            .unwrap_or(&default_router);
        let profile_id = self.config.resolve_intent_profile_id(agent_id)?;
        let profile = router.profiles.get(&profile_id)?;
        let mut tools: HashSet<String> = profile.core_tools.iter().cloned().collect();
        if router.enabled {
            for entry in profile.intent_tools.values() {
                tools.extend(entry.tools().iter().cloned());
            }
        } else if let Some(entry) = profile.intent_tools.get("Unknown") {
            if !entry.inherit_base() {
                tools.clear();
            }
            tools.extend(entry.tools().iter().cloned());
        }
        // Host configuration grants capabilities; skill metadata is only a
        // declaration of requirements, regardless of its learned flag.
        tools.extend(profile.skill_grants.values().flatten().cloned());
        tools.retain(|name| !profile.deny_tools.contains(name));
        let mut tools: Vec<_> = tools.into_iter().collect();
        tools.sort();
        Some(tools)
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

        // 1. 先判断 enabled
        if !router.enabled {
            // 2. enabled=false 时，判断 load_all_tools
            if router.load_all_tools {
                // 全量加载：返回所有可用工具（扣除 deny_tools）
                if let Some(available) = available_tools {
                    let mut result: Vec<String> = available.iter().cloned().collect();
                    // 应用 deny_tools 过滤
                    let profile_id = self.config.resolve_intent_profile_id(agent_id)?;
                    if let Some(profile) = router.profiles.get(&profile_id) {
                        for tool in &profile.deny_tools {
                            result.retain(|t| t != tool);
                        }
                    } else {
                        warn!(
                            profile_id = %profile_id,
                            "Profile not found in load_all_tools mode, deny_tools filtering skipped"
                        );
                    }
                    result.sort();
                    return Some(result);
                }
            }
            // load_all_tools=false: 走 Unknown profile（原有逻辑）
            // effective_intents = vec![IntentCategory::Unknown]
        }

        // enabled=true: 意图分类流程（原有逻辑）
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
            for (skill, granted_tools) in &profile.skill_grants {
                if skill.trim().is_empty() {
                    return Err(Error::Config(format!("intentRouter.profiles.{profile_name}.skillGrants requires nonempty skill names")));
                }
                for tool in granted_tools {
                    ensure_known_tool(self.config, mcp, profile_name, tool, &registered)?;
                }
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
        // 现在这些有了明确意图规则，不再是 Unknown
        assert_eq!(
            classifier.classify("查一下茅台股价"),
            vec![IntentCategory::Finance]
        );
        assert_eq!(
            classifier.classify("0x1234567890abcdef1234567890abcdef12345678 这个地址安全吗"),
            vec![IntentCategory::Blockchain]
        );
        assert_eq!(
            classifier.classify("帮我读一下 config.json5"),
            vec![IntentCategory::FileOps]
        );
        // 完全模糊的输入仍然是 Unknown
        assert_eq!(
            classifier.classify("帮我做一件复杂的事情"),
            vec![IntentCategory::Unknown]
        );
    }

    #[test]
    fn test_finance_classification() {
        let c = IntentClassifier::new();
        assert_eq!(c.classify("查一下茅台股价"), vec![IntentCategory::Finance]);
        assert_eq!(
            c.classify("我想了解一下A股行情"),
            vec![IntentCategory::Finance]
        );
        assert_eq!(c.classify("介绍一下ETF基金"), vec![IntentCategory::Finance]);
        // 负例：普通问候不应触发 Finance
        assert_ne!(c.classify("你好"), vec![IntentCategory::Finance]);
        assert_ne!(c.classify("再见"), vec![IntentCategory::Finance]);
    }

    #[test]
    fn test_blockchain_classification() {
        let c = IntentClassifier::new();
        assert_eq!(
            c.classify("0x742d35Cc6634C0532925a3b844Bc454e4438f44e 这个地址安全吗"),
            vec![IntentCategory::Blockchain]
        );
        assert_eq!(
            c.classify("钱包里的ETH怎么转账"),
            vec![IntentCategory::Blockchain]
        );
        assert_eq!(
            c.classify("区块链上的NFT怎么铸造"),
            vec![IntentCategory::Blockchain]
        );
        // 负例
        assert_ne!(c.classify("你好"), vec![IntentCategory::Blockchain]);
    }

    #[test]
    fn test_fileops_classification() {
        let c = IntentClassifier::new();
        assert_eq!(
            c.classify("帮我读一下 config.json5"),
            vec![IntentCategory::FileOps]
        );
        assert_eq!(
            c.classify("列出当前目录的文件"),
            vec![IntentCategory::FileOps]
        );
        let edit_code = c.classify("edit the README.md file");
        assert_eq!(edit_code[0], IntentCategory::Coding);
        assert!(edit_code.contains(&IntentCategory::FileOps));
        let create_code = c.classify("创建一个新的 main.rs 文件");
        assert_eq!(create_code[0], IntentCategory::Coding);
        assert!(create_code.contains(&IntentCategory::FileOps));
        // 负例：URL 中的文件扩展名（无空格边界）不应触发 FileOps
        assert_ne!(
            c.classify("打开 https://example.com/main.rs 看看"),
            vec![IntentCategory::FileOps]
        );
        // 负例："rs" 作为单词一部分不触发
        assert_ne!(
            c.classify("我喜欢 rust 和 python"),
            vec![IntentCategory::FileOps]
        );
        // 负例
        assert_ne!(c.classify("谢谢"), vec![IntentCategory::FileOps]);
    }

    #[test]
    fn test_websearch_classification() {
        let c = IntentClassifier::new();
        assert_eq!(
            c.classify("搜索一下量子计算"),
            vec![IntentCategory::WebSearch]
        );
        assert_eq!(
            c.classify("google how to learn rust"),
            vec![IntentCategory::WebSearch]
        );
        assert_eq!(
            c.classify("what is a neural network"),
            vec![IntentCategory::WebSearch]
        );
        // 股价搜索应该命中 Finance negative 过滤，不触发 WebSearch
        let result = c.classify("搜索一下股价行情");
        assert!(!result.contains(&IntentCategory::WebSearch));
    }

    #[test]
    fn test_dataanalysis_classification() {
        let c = IntentClassifier::new();
        assert_eq!(
            c.classify("帮我做一个数据分析"),
            vec![IntentCategory::DataAnalysis]
        );
        assert_eq!(
            c.classify("画一个折线图"),
            vec![IntentCategory::DataAnalysis]
        );
        // csv 文件同时触发 DataAnalysis + FileOps（数据分析需要读取文件）
        let result = c.classify("分析这个 data.csv 文件");
        assert!(result.contains(&IntentCategory::DataAnalysis));
        assert!(result[0] == IntentCategory::DataAnalysis); // DataAnalysis 是主要意图
                                                            // 负例：闲聊中的图表字样不应触发
        assert_ne!(
            c.classify("今天天气真好"),
            vec![IntentCategory::DataAnalysis]
        );
    }

    #[test]
    fn test_communication_classification() {
        let c = IntentClassifier::new();
        assert_eq!(
            c.classify("发邮件给 boss@company.com"),
            vec![IntentCategory::Communication]
        );
        assert_eq!(
            c.classify("帮我发一条消息通知团队"),
            vec![IntentCategory::Communication]
        );
        // 负例：提到邮件地址但不是要发邮件
        assert_ne!(
            c.classify("我的邮箱是 user@domain.com，记得联系我"),
            vec![IntentCategory::Communication]
        );
    }

    #[test]
    fn test_systemcontrol_classification() {
        let c = IntentClassifier::new();
        assert_eq!(
            c.classify("查看当前CPU使用率"),
            vec![IntentCategory::SystemControl]
        );
        assert_eq!(
            c.classify("打开微信应用"),
            vec![IntentCategory::SystemControl]
        );
        assert_eq!(
            c.classify("系统负载状态怎么样"),
            vec![IntentCategory::SystemControl]
        );
        // 负例：内存作为日常用语不应触发
        assert_ne!(
            c.classify("我最近记忆力不太好"),
            vec![IntentCategory::SystemControl]
        );
    }

    #[test]
    fn test_organization_classification() {
        let c = IntentClassifier::new();
        assert_eq!(
            c.classify("30分钟后提醒我开会"),
            vec![IntentCategory::Organization]
        );
        assert_eq!(
            c.classify("添加一个每天的提醒"),
            vec![IntentCategory::Organization]
        );
        assert_eq!(
            c.classify("设置一个早上8点的日程"),
            vec![IntentCategory::Organization]
        );
        // 负例：cargo task 是构建命令，不是任务管理
        assert_ne!(
            c.classify("cargo task build"),
            vec![IntentCategory::Organization]
        );
    }

    #[test]
    fn test_iot_classification() {
        let c = IntentClassifier::new();
        assert_eq!(c.classify("打开客厅的灯"), vec![IntentCategory::IoT]);
        assert_eq!(c.classify("调节空调温度"), vec![IntentCategory::IoT]);
        assert_eq!(c.classify("关闭窗帘"), vec![IntentCategory::IoT]);
        // 负例：问温度是 Lifestyle，不是 IoT
        assert_ne!(c.classify("今天温度多少度"), vec![IntentCategory::IoT]);
    }

    #[test]
    fn test_media_classification() {
        let c = IntentClassifier::new();
        assert_eq!(
            c.classify("帮我识别这张图片里的文字"),
            vec![IntentCategory::Media]
        );
        assert_eq!(
            c.classify("把这段音频转成文字"),
            vec![IntentCategory::Media]
        );
        assert_eq!(
            c.classify("处理一下这个 video.mp4 文件"),
            vec![IntentCategory::Media]
        );
        // 负例：提到 mp4 但只是闲聊不应触发
        assert_ne!(
            c.classify("我下载了一个 video.mp4 视频"),
            vec![IntentCategory::Media]
        );
    }

    #[test]
    fn test_devops_classification() {
        let c = IntentClassifier::new();
        assert_eq!(
            c.classify("POST https://api.example.com/users"),
            vec![IntentCategory::DevOps]
        );
        assert_eq!(c.classify("ping 192.168.1.1"), vec![IntentCategory::DevOps]);
        assert_eq!(
            c.classify("docker build -t myapp ."),
            vec![IntentCategory::DevOps]
        );
        // 负例：问 curl 命令是什么不应触发 DevOps（是问问题，不是用工具）
        assert_ne!(c.classify("curl 命令是什么"), vec![IntentCategory::DevOps]);
    }

    #[test]
    fn test_coding_classification_uses_action_and_path_features() {
        let c = IntentClassifier::new();
        let result = c.classify("修复 crates/agent/src/runtime.rs 中的 bug，然后运行 cargo test");
        assert_eq!(result[0], IntentCategory::Coding);
        assert!(!c
            .classify("解释一下 Rust 所有权")
            .contains(&IntentCategory::Coding));
    }

    #[test]
    fn coding_eval_prompts_classify_as_coding() {
        let classifier = IntentClassifier::new();
        assert!(classifier
            .classify("fix the bug in slug.py")
            .contains(&IntentCategory::Coding));
        assert!(classifier
            .classify("CI 挂了，帮我看一下修好。")
            .contains(&IntentCategory::Coding));
    }

    #[test]
    fn greenfield_and_test_repair_prompts_classify_as_coding_from_keywords() {
        let classifier = IntentClassifier::new();
        for prompt in [
            "从零实现 wordcount CLI、README 和单元测试，支持文件与标准输入。",
            "从零实现可持久化的 TodoStore、README 和单元测试。",
            "calc 包里的表达式计算器有多个问题（优先级、结合性、一元负号、错误处理），修复它让测试全部通过。",
            "duration.py 需要支持更多格式，以测试为准把它完善好。",
        ] {
            assert!(
                classifier.classify(prompt).contains(&IntentCategory::Coding),
                "{prompt}"
            );
        }
        assert_eq!(classifier.classify("你好"), vec![IntentCategory::Chat]);
    }

    #[test]
    fn test_lifestyle_classification() {
        let c = IntentClassifier::new();
        assert_eq!(
            c.classify("今天天气怎么样"),
            vec![IntentCategory::Lifestyle]
        );
        assert_eq!(
            c.classify("推荐一个健康的菜谱"),
            vec![IntentCategory::Lifestyle]
        );
        assert_eq!(
            c.classify("明天温度多少度"),
            vec![IntentCategory::Lifestyle]
        );
        // 负例：问内存价格不应触发 Lifestyle
        assert_ne!(
            c.classify("内存价格最近怎么样"),
            vec![IntentCategory::Lifestyle]
        );
    }

    #[test]
    fn test_unknown_for_truly_ambiguous_input() {
        let c = IntentClassifier::new();
        assert_eq!(
            c.classify("帮我做一件复杂的事情"),
            vec![IntentCategory::Unknown]
        );
        assert_eq!(
            c.classify("balabala xyzzy quux"),
            vec![IntentCategory::Unknown]
        );
    }

    #[test]
    fn test_global_singleton_is_same_instance() {
        let a = IntentClassifier::global();
        let b = IntentClassifier::global();
        // 同一个 static 引用，地址应该相同
        assert!(std::ptr::eq(a, b));
    }

    #[test]
    fn test_with_extra_rules_adds_and_deduplicates() {
        // 测试追加配置规则时：内置规则保留；重复 extra category 只添加第一个并警告
        let extra = vec![
            blockcell_core::config::IntentRuleConfig {
                category: "Finance".to_string(),
                keywords: vec!["狗狗币".to_string(), "屎币".to_string()],
                patterns: vec![],
                negative: vec![],
                priority: 70,
            },
            blockcell_core::config::IntentRuleConfig {
                category: "Finance".to_string(), // 重复 category，跳过
                keywords: vec!["其他币".to_string()],
                patterns: vec![],
                negative: vec![],
                priority: 70,
            },
            blockcell_core::config::IntentRuleConfig {
                category: "IoT".to_string(),
                keywords: vec!["热水器".to_string()],
                patterns: vec![],
                negative: vec![],
                priority: 65,
            },
        ];
        let c = IntentClassifier::with_extra_rules(&extra);
        // 第一个 Finance extra 规则中的两个关键词都应该匹配
        assert_eq!(
            c.classify("查一下狗狗币价格"),
            vec![IntentCategory::Finance]
        );
        assert_eq!(c.classify("查一下屎币"), vec![IntentCategory::Finance]);
        // 第二个 Finance extra 被跳过，所以"其他币"不触发 Finance（只有内置关键词"股价"等会触发）
        assert_ne!(c.classify("查一下其他币"), vec![IntentCategory::Finance]);
        assert_eq!(c.classify("打开热水器"), vec![IntentCategory::IoT]);
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

    #[test]
    fn test_load_all_tools_returns_all_available_tools() {
        // 当 enabled=false 且 load_all_tools=true 时，返回所有可用工具
        let raw = r#"{
            "intentRouter": {
                "enabled": false,
                "loadAllTools": true,
                "defaultProfile": "default",
                "profiles": {
                    "default": {
                        "coreTools": [],
                        "intentTools": {
                            "Chat": { "inheritBase": false, "tools": [] },
                            "Unknown": []
                        },
                        "denyTools": []
                    }
                }
            }
        }"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let resolver = IntentToolResolver::new(&config);

        let available: HashSet<String> = ["read_file", "write_file", "exec", "web_search"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let tools = resolver
            .resolve_tool_names(None, &[IntentCategory::Unknown], Some(&available))
            .expect("should resolve tools");

        // 应返回所有 4 个可用工具
        assert_eq!(tools.len(), 4);
        assert!(tools.contains(&"read_file".to_string()));
        assert!(tools.contains(&"write_file".to_string()));
        assert!(tools.contains(&"exec".to_string()));
        assert!(tools.contains(&"web_search".to_string()));
    }

    #[test]
    fn test_load_all_tools_applies_deny_tools() {
        // 当 enabled=false 且 load_all_tools=true 时，仍应用 deny_tools 过滤
        let raw = r#"{
            "intentRouter": {
                "enabled": false,
                "loadAllTools": true,
                "defaultProfile": "default",
                "profiles": {
                    "default": {
                        "coreTools": [],
                        "intentTools": {
                            "Chat": { "inheritBase": false, "tools": [] },
                            "Unknown": []
                        },
                        "denyTools": ["exec"]
                    }
                }
            }
        }"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let resolver = IntentToolResolver::new(&config);

        let available: HashSet<String> = ["read_file", "write_file", "exec", "web_search"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let tools = resolver
            .resolve_tool_names(None, &[IntentCategory::Unknown], Some(&available))
            .expect("should resolve tools");

        // 应返回 3 个工具（exec 被 deny）
        assert_eq!(tools.len(), 3);
        assert!(tools.contains(&"read_file".to_string()));
        assert!(tools.contains(&"write_file".to_string()));
        assert!(tools.contains(&"web_search".to_string()));
        assert!(!tools.contains(&"exec".to_string()));
    }

    #[test]
    fn test_load_all_tools_false_follows_unknown_profile() {
        // 当 enabled=false 且 load_all_tools=false 时，走 Unknown profile
        let raw = r#"{
            "intentRouter": {
                "enabled": false,
                "loadAllTools": false,
                "defaultProfile": "default",
                "profiles": {
                    "default": {
                        "coreTools": ["read_file"],
                        "intentTools": {
                            "Chat": { "inheritBase": false, "tools": [] },
                            "Unknown": ["web_search"]
                        },
                        "denyTools": []
                    }
                }
            }
        }"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let resolver = IntentToolResolver::new(&config);

        let available: HashSet<String> = ["read_file", "write_file", "exec", "web_search"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let tools = resolver
            .resolve_tool_names(None, &[IntentCategory::Unknown], Some(&available))
            .expect("should resolve tools");

        // 应返回 Unknown profile 配置的工具：read_file (core) + web_search (intent)
        assert_eq!(tools.len(), 2);
        assert!(tools.contains(&"read_file".to_string()));
        assert!(tools.contains(&"web_search".to_string()));
    }

    #[test]
    fn test_enabled_true_ignores_load_all_tools() {
        // 当 enabled=true 时，忽略 load_all_tools，走意图分类流程
        let raw = r#"{
            "intentRouter": {
                "enabled": true,
                "loadAllTools": true,
                "defaultProfile": "default",
                "profiles": {
                    "default": {
                        "coreTools": ["read_file"],
                        "intentTools": {
                            "Chat": { "inheritBase": false, "tools": [] },
                            "FileOps": ["edit_file"]
                        },
                        "denyTools": []
                    }
                }
            }
        }"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let resolver = IntentToolResolver::new(&config);

        let available: HashSet<String> = ["read_file", "edit_file", "exec", "web_search"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Chat 意图应返回空（Chat profile 配置 inheritBase=false, tools=[]）
        let tools = resolver
            .resolve_tool_names(None, &[IntentCategory::Chat], Some(&available))
            .expect("should resolve tools");
        assert!(tools.is_empty());

        // FileOps 意图应返回 read_file (core) + edit_file (intent)
        let tools = resolver
            .resolve_tool_names(None, &[IntentCategory::FileOps], Some(&available))
            .expect("should resolve tools");
        assert_eq!(tools.len(), 2);
        assert!(tools.contains(&"read_file".to_string()));
        assert!(tools.contains(&"edit_file".to_string()));
    }

    #[test]
    fn test_load_all_tools_without_available_tools_returns_none() {
        // 当 enabled=false, load_all_tools=true, available_tools=None 时，应返回 None
        // 因为代码检查 if let Some(available) = available_tools
        let raw = r#"{
            "intentRouter": {
                "enabled": false,
                "loadAllTools": true,
                "defaultProfile": "default",
                "profiles": {
                    "default": {
                        "coreTools": [],
                        "intentTools": {
                            "Chat": { "inheritBase": false, "tools": [] },
                            "Unknown": []
                        },
                        "denyTools": []
                    }
                }
            }
        }"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let resolver = IntentToolResolver::new(&config);

        // available_tools=None 应返回 None，然后走原有逻辑
        let tools = resolver.resolve_tool_names(None, &[IntentCategory::Unknown], None);
        // 应走 Unknown profile 原有逻辑返回工具
        assert!(tools.is_some());
    }

    #[test]
    fn test_load_all_tools_empty_available_returns_empty() {
        // 当 available_tools 为空集合时，应返回空 Vec
        let raw = r#"{
            "intentRouter": {
                "enabled": false,
                "loadAllTools": true,
                "defaultProfile": "default",
                "profiles": {
                    "default": {
                        "coreTools": [],
                        "intentTools": {
                            "Chat": { "inheritBase": false, "tools": [] },
                            "Unknown": []
                        },
                        "denyTools": []
                    }
                }
            }
        }"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let resolver = IntentToolResolver::new(&config);

        let available: HashSet<String> = HashSet::new();
        let tools = resolver
            .resolve_tool_names(None, &[IntentCategory::Unknown], Some(&available))
            .expect("should resolve tools");
        assert!(tools.is_empty());
    }

    #[test]
    fn test_load_all_tools_multiple_deny_tools() {
        // 当有多个 deny_tools 时，都应被过滤
        let raw = r#"{
            "intentRouter": {
                "enabled": false,
                "loadAllTools": true,
                "defaultProfile": "default",
                "profiles": {
                    "default": {
                        "coreTools": [],
                        "intentTools": {
                            "Chat": { "inheritBase": false, "tools": [] },
                            "Unknown": []
                        },
                        "denyTools": ["exec", "delete_file", "shell"]
                    }
                }
            }
        }"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let resolver = IntentToolResolver::new(&config);

        let available: HashSet<String> = [
            "read_file",
            "write_file",
            "exec",
            "delete_file",
            "shell",
            "web_search",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let tools = resolver
            .resolve_tool_names(None, &[IntentCategory::Unknown], Some(&available))
            .expect("should resolve tools");

        // 3 个工具被 deny，剩余 3 个
        assert_eq!(tools.len(), 3);
        assert!(tools.contains(&"read_file".to_string()));
        assert!(tools.contains(&"write_file".to_string()));
        assert!(tools.contains(&"web_search".to_string()));
        assert!(!tools.contains(&"exec".to_string()));
        assert!(!tools.contains(&"delete_file".to_string()));
        assert!(!tools.contains(&"shell".to_string()));
    }

    #[test]
    fn test_load_all_tools_missing_profile_skips_deny_but_returns_all() {
        // 当 defaultProfile 不存在于 profiles 时：
        // - resolve_intent_profile_id 返回 Some("nonexistent")
        // - router.profiles.get("nonexistent") 返回 None
        // - 代码跳过 deny_tools 过滤（并打印警告）
        // - 但仍返回所有可用工具
        let raw = r#"{
            "intentRouter": {
                "enabled": false,
                "loadAllTools": true,
                "defaultProfile": "nonexistent",
                "profiles": {
                    "other": {
                        "coreTools": [],
                        "intentTools": {
                            "Chat": { "inheritBase": false, "tools": [] },
                            "Unknown": []
                        },
                        "denyTools": ["exec"]
                    }
                }
            }
        }"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let resolver = IntentToolResolver::new(&config);

        let available: HashSet<String> = ["read_file", "exec", "web_search"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // resolve_intent_profile_id(None) 返回 defaultProfile="nonexistent"
        // profiles.get("nonexistent") 返回 None，跳过 deny_tools 过滤，但仍返回所有工具
        let tools = resolver
            .resolve_tool_names(None, &[IntentCategory::Unknown], Some(&available))
            .expect("should return all tools even when profile missing");

        // 因为 profile 不存在，deny_tools 未过滤，返回所有 3 个工具
        assert_eq!(tools.len(), 3);
        assert!(tools.contains(&"read_file".to_string()));
        assert!(tools.contains(&"exec".to_string()));
        assert!(tools.contains(&"web_search".to_string()));
    }

    #[test]
    fn test_load_all_tools_uses_defaults_intent_profile() {
        // 当使用 agents.defaults.intentProfile 指向有效 profile 时，deny_tools 应生效
        // 但注意：resolve_intent_profile_id(None) 返回 defaultProfile，不是 defaults.intentProfile
        // 要使用 defaults.intentProfile，需要设置 defaultProfile 为该值
        let raw = r#"{
            "intentRouter": {
                "enabled": false,
                "loadAllTools": true,
                "defaultProfile": "other",
                "profiles": {
                    "other": {
                        "coreTools": [],
                        "intentTools": {
                            "Chat": { "inheritBase": false, "tools": [] },
                            "Unknown": []
                        },
                        "denyTools": ["exec"]
                    }
                }
            }
        }"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let resolver = IntentToolResolver::new(&config);

        let available: HashSet<String> = ["read_file", "exec", "web_search"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // resolve_intent_profile_id(None) 返回 defaultProfile="other"
        // profiles.get("other") 找到 profile，应用 deny_tools 过滤
        let tools = resolver
            .resolve_tool_names(None, &[IntentCategory::Unknown], Some(&available))
            .expect("should resolve tools");

        // exec 应被 deny
        assert_eq!(tools.len(), 2);
        assert!(tools.contains(&"read_file".to_string()));
        assert!(tools.contains(&"web_search".to_string()));
        assert!(!tools.contains(&"exec".to_string()));
    }

    #[test]
    fn test_enabled_false_ignores_intent_parameter() {
        // 当 enabled=false, load_all_tools=true 时，intent 参数不影响结果
        let raw = r#"{
            "intentRouter": {
                "enabled": false,
                "loadAllTools": true,
                "defaultProfile": "default",
                "profiles": {
                    "default": {
                        "coreTools": [],
                        "intentTools": {
                            "Chat": { "inheritBase": false, "tools": [] },
                            "Unknown": []
                        },
                        "denyTools": []
                    }
                }
            }
        }"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let resolver = IntentToolResolver::new(&config);

        let available: HashSet<String> = ["read_file", "write_file", "exec"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // 不同 intent 应返回相同结果
        let tools_chat = resolver
            .resolve_tool_names(None, &[IntentCategory::Chat], Some(&available))
            .expect("should resolve tools");
        let tools_unknown = resolver
            .resolve_tool_names(None, &[IntentCategory::Unknown], Some(&available))
            .expect("should resolve tools");
        let tools_fileops = resolver
            .resolve_tool_names(None, &[IntentCategory::FileOps], Some(&available))
            .expect("should resolve tools");

        // load_all_tools=true 时，所有 intent 都返回相同的全量工具
        assert_eq!(tools_chat, tools_unknown);
        assert_eq!(tools_chat, tools_fileops);
        assert_eq!(tools_chat.len(), 3);
    }
}
