use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::agent_prompts::{
    CODER_SYSTEM_PROMPT, EXPLORE_SYSTEM_PROMPT, GENERAL_SYSTEM_PROMPT, PLAN_SYSTEM_PROMPT,
    REVIEWER_SYSTEM_PROMPT, TESTER_SYSTEM_PROMPT, VERIFICATION_SYSTEM_PROMPT, VIPER_SYSTEM_PROMPT,
};

/// Agent 定义来源
///
/// 标识 Agent 定义从何处加载:
/// - `BuiltIn`: Rust 源码中硬编码
/// - `UserLevel`: 从 `~/.blockcell/workspace/agents/*.md` 加载
/// - `ProjectLevel`: 从 `<project>/.blockcell/agents/*.md` 加载
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum AgentSource {
    /// 内置 Agent (Rust 源码硬编码)
    #[default]
    BuiltIn,
    /// 用户级 Agent (~/.blockcell/workspace/agents/)
    UserLevel,
    /// 项目级 Agent (<project>/.blockcell/agents/)
    ProjectLevel,
}

/// Agent 类型的权限模式
///
/// 决定父子 Agent 之间的权限流转方式:
/// - `Inherit`: 子 Agent 继承父 Agent 的权限 (默认)
/// - `Bubble`: 权限请求向上冒泡到父 Agent
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum PermissionMode {
    /// 继承父 Agent 的权限
    #[default]
    Inherit,
    /// 权限请求向上冒泡到父 Agent
    Bubble,
}

/// Agent 执行隔离模式
///
/// 决定 Agent 是否在隔离环境中运行:
/// - `None`: 无隔离，共享工作目录 (默认)
/// - `Worktree`: Git worktree 隔离，适用于代码编写类 Agent
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub enum IsolationMode {
    /// 无隔离，共享工作目录
    #[default]
    None,
    /// Git worktree 隔离，适用于代码编写类 Agent
    Worktree,
}

/// Agent 类型定义
///
/// 定义 Agent 类型的完整配置，包括:
/// - 类型标识符和使用场景描述
/// - 禁止的工具列表
/// - 可选的最大轮次限制
/// - 可选的系统提示模板
/// - ONE_SHOT 标志 (完成后不能继续 SendMessage)
/// - 父子 Agent 之间的权限流转模式
/// - 可选的允许工具列表 (None = 所有工具)
/// - 可选的模型覆盖
/// - 预加载技能列表
/// - MCP 服务器引用
/// - 可选的首轮提示注入
/// - 后台执行标志
/// - UI 显示颜色
/// - 定义来源和文件元数据
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTypeDefinition {
    /// Agent 类型标识符
    pub agent_type: String,

    /// 使用场景描述 (注入到系统提示中)
    pub when_to_use: String,

    /// 禁止的工具列表
    pub disallowed_tools: Vec<String>,

    /// 最大轮次限制 (可选)
    pub max_turns: Option<u32>,

    /// 系统提示模板 (可选)
    pub system_prompt_template: Option<String>,

    /// ONE_SHOT 标志 — 为 true 时完成后不能继续 SendMessage
    pub one_shot: bool,

    /// 权限流转模式
    pub permission_mode: PermissionMode,

    /// 隔离模式 (可选) — 决定执行环境的隔离方式
    pub isolation: Option<IsolationMode>,

    // === 自定义 Agent 配置字段 ===
    /// 允许的工具列表 (None = 所有工具, Some(["*"]) 也表示所有工具)
    #[serde(default)]
    pub tools: Option<Vec<String>>,

    /// 模型覆盖 (None = 继承父 Agent 的模型)
    #[serde(default)]
    pub model: Option<String>,

    /// 预加载技能列表
    #[serde(default)]
    pub skills: Vec<String>,

    /// MCP 服务器引用 (必须在全局配置中存在)
    #[serde(default)]
    pub mcp_servers: Vec<String>,

    /// 首轮提示注入 (在第一条用户消息之前)
    #[serde(default)]
    pub initial_prompt: Option<String>,

    /// 是否始终后台运行
    #[serde(default)]
    pub background: bool,

    /// UI 显示颜色
    #[serde(default)]
    pub color: Option<String>,

    /// Agent 定义来源
    #[serde(skip)]
    pub source: AgentSource,

    /// 原始文件名 (不含 .md 扩展名)
    #[serde(skip)]
    pub filename: Option<String>,

    /// 定义文件所在目录
    #[serde(skip)]
    pub base_dir: Option<PathBuf>,
}

impl Default for AgentTypeDefinition {
    fn default() -> Self {
        Self {
            agent_type: "general".to_string(),
            when_to_use: String::new(),
            disallowed_tools: vec![],
            max_turns: None,
            system_prompt_template: None,
            one_shot: false,
            permission_mode: PermissionMode::default(),
            isolation: None,
            tools: None,
            model: None,
            skills: vec![],
            mcp_servers: vec![],
            initial_prompt: None,
            background: false,
            color: None,
            source: AgentSource::default(),
            filename: None,
            base_dir: None,
        }
    }
}

impl AgentTypeDefinition {
    /// Create an explore agent type definition
    pub fn explore() -> Self {
        Self {
            agent_type: "explore".to_string(),
            when_to_use: "Fast agent specialized for exploring codebases. Use this when you need to quickly find files by patterns (eg. \"src/components/**/*.tsx\"), search code for keywords (eg. \"API endpoints\"), or answer questions about the codebase (eg. \"how do API endpoints work?\"). When calling this agent, specify the desired thoroughness level: \"quick\" for basic searches, \"medium\" for moderate exploration, or \"very thorough\" for comprehensive analysis across multiple locations and naming conventions.".to_string(),
            disallowed_tools: ["agent", "spawn", "write_file", "edit_file", "exec"].iter().map(|s| s.to_string()).collect(),
            max_turns: Some(20),
            system_prompt_template: Some(EXPLORE_SYSTEM_PROMPT.to_string()),
            one_shot: true,
            permission_mode: PermissionMode::Inherit,
            isolation: None,
            tools: None,
            model: None,
            skills: vec![],
            mcp_servers: vec![],
            initial_prompt: None,
            background: false,
            color: None,
            source: AgentSource::BuiltIn,
            filename: None,
            base_dir: None,
        }
    }

    /// Create a plan agent type definition
    pub fn plan() -> Self {
        Self {
            agent_type: "plan".to_string(),
            when_to_use: "Software architect agent for designing implementation plans. Use this when you need to plan the implementation strategy for a task. Returns step-by-step plans, identifies critical files, and considers architectural trade-offs.".to_string(),
            disallowed_tools: ["agent", "spawn", "write_file", "edit_file", "exec"].iter().map(|s| s.to_string()).collect(),
            max_turns: Some(30),
            system_prompt_template: Some(PLAN_SYSTEM_PROMPT.to_string()),
            one_shot: true,
            permission_mode: PermissionMode::Inherit,
            isolation: None,
            tools: None,
            model: None,
            skills: vec![],
            mcp_servers: vec![],
            initial_prompt: None,
            background: false,
            color: None,
            source: AgentSource::BuiltIn,
            filename: None,
            base_dir: None,
        }
    }

    /// Create a coding implementation agent type definition.
    pub fn coder() -> Self {
        Self {
            agent_type: "coder".to_string(),
            when_to_use: "Implementation specialist for a clearly bounded coding subtask and explicit file list. Writes code, runs focused verification, and returns a structured change summary.".to_string(),
            disallowed_tools: ["agent", "spawn", "exec"]
                .iter()
                .map(|tool| tool.to_string())
                .collect(),
            max_turns: Some(50),
            system_prompt_template: Some(CODER_SYSTEM_PROMPT.to_string()),
            one_shot: true,
            permission_mode: PermissionMode::Bubble,
            isolation: None,
            tools: Some(
                [
                    "list_dir",
                    "read_file",
                    "grep",
                    "glob",
                    "edit_file",
                    "write_file",
                    "shell",
                ]
                .iter()
                .map(|tool| tool.to_string())
                .collect(),
            ),
            background: true,
            color: Some("green".to_string()),
            ..Default::default()
        }
    }

    /// Create a read-only code review agent type definition.
    pub fn reviewer() -> Self {
        Self {
            agent_type: "reviewer".to_string(),
            when_to_use: "Read-only code reviewer for checking a completed diff. Returns severity-ordered findings with file and line references.".to_string(),
            disallowed_tools: ["agent", "spawn", "write_file", "edit_file", "exec"]
                .iter()
                .map(|tool| tool.to_string())
                .collect(),
            max_turns: Some(25),
            system_prompt_template: Some(REVIEWER_SYSTEM_PROMPT.to_string()),
            one_shot: true,
            permission_mode: PermissionMode::Inherit,
            isolation: None,
            tools: Some(
                ["list_dir", "read_file", "grep", "glob"]
                    .iter()
                    .map(|tool| tool.to_string())
                    .collect(),
            ),
            background: true,
            color: Some("yellow".to_string()),
            ..Default::default()
        }
    }

    /// Create a test execution agent type definition.
    pub fn tester() -> Self {
        Self {
            agent_type: "tester".to_string(),
            when_to_use: "Test execution specialist that runs requested tests, lint, and builds without modifying code. Returns structured pass/fail diagnostics.".to_string(),
            disallowed_tools: ["agent", "spawn", "write_file", "edit_file", "exec"]
                .iter()
                .map(|tool| tool.to_string())
                .collect(),
            max_turns: Some(20),
            system_prompt_template: Some(TESTER_SYSTEM_PROMPT.to_string()),
            one_shot: true,
            permission_mode: PermissionMode::Inherit,
            isolation: None,
            tools: Some(vec!["read_file".to_string(), "shell".to_string()]),
            background: true,
            color: Some("blue".to_string()),
            ..Default::default()
        }
    }

    /// Create a verification agent type definition
    pub fn verification() -> Self {
        Self {
            agent_type: "verification".to_string(),
            when_to_use: "Verification specialist for testing and validating implementations. Tries to break the implementation.".to_string(),
            disallowed_tools: [
                "agent", "spawn", "write_file", "edit_file", "exec", "shell", "skill_manage",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            max_turns: Some(15),
            system_prompt_template: Some(VERIFICATION_SYSTEM_PROMPT.to_string()),
            one_shot: false,
            permission_mode: PermissionMode::Inherit,
            isolation: None,
            tools: Some(
                ["list_dir", "read_file", "grep", "glob"]
                    .iter()
                    .map(|tool| tool.to_string())
                    .collect(),
            ),
            model: None,
            skills: vec![],
            mcp_servers: vec![],
            initial_prompt: None,
            background: false,
            color: None,
            source: AgentSource::BuiltIn,
            filename: None,
            base_dir: None,
        }
    }

    /// Create a viper agent type definition
    pub fn viper() -> Self {
        Self {
            agent_type: "viper".to_string(),
            when_to_use: "Implementation agent for writing production code, adding features, and refactoring.".to_string(),
            disallowed_tools: ["agent", "spawn"].iter().map(|s| s.to_string()).collect(),
            max_turns: Some(50),
            system_prompt_template: Some(VIPER_SYSTEM_PROMPT.to_string()),
            one_shot: false,
            permission_mode: PermissionMode::Bubble,
            isolation: Some(IsolationMode::Worktree), // viper 写代码，需要 worktree 隔离
            tools: None,
            model: None,
            skills: vec![],
            mcp_servers: vec![],
            initial_prompt: None,
            background: false,
            color: None,
            source: AgentSource::BuiltIn,
            filename: None,
            base_dir: None,
        }
    }

    /// Create a general agent type definition
    pub fn general() -> Self {
        Self {
            agent_type: "general".to_string(),
            when_to_use: "General-purpose agent for complex multi-step tasks that don't fit specialized profiles.".to_string(),
            // 禁止 agent/spawn 防止无限递归 spawn
            disallowed_tools: ["agent", "spawn"].iter().map(|s| s.to_string()).collect(),
            max_turns: None,
            system_prompt_template: Some(GENERAL_SYSTEM_PROMPT.to_string()),
            one_shot: false,
            permission_mode: PermissionMode::Bubble,
            isolation: None,
            tools: None,
            model: None,
            skills: vec![],
            mcp_servers: vec![],
            initial_prompt: None,
            background: false,
            color: None,
            source: AgentSource::BuiltIn,
            filename: None,
            base_dir: None,
        }
    }
}

/// Agent type registry
///
/// Manages registration and lookup of agent type definitions.
/// Supports both built-in types and custom types registered at runtime.
#[derive(Clone)]
pub struct AgentTypeRegistry {
    types: HashMap<String, AgentTypeDefinition>,
}

impl AgentTypeRegistry {
    /// Create an empty registry (for testing)
    pub fn new_empty() -> Self {
        Self {
            types: HashMap::new(),
        }
    }

    /// Create a registry with built-in types
    pub fn new() -> Self {
        let mut registry = Self::new_empty();
        for def in built_in_agent_types() {
            registry.register(def);
        }
        registry
    }

    /// Register an agent type. If a type with the same name already exists,
    /// it is overwritten with a warning log.
    ///
    /// Safety: Automatically adds "agent" and "spawn" to disallowed_tools
    /// to prevent recursive spawning (infinite recursion).
    pub fn register(&mut self, mut def: AgentTypeDefinition) {
        // 安全防护：确保所有 agent 类型都禁止递归 spawn
        // 子 agent 不应创建更深层的子 agent，防止无限递归
        let forbidden = ["agent", "spawn"];
        for tool in forbidden {
            if !def.disallowed_tools.iter().any(|t| t == tool) {
                tracing::warn!(
                    agent_type = %def.agent_type,
                    tool = tool,
                    "Auto-adding '{}' to disallowed_tools to prevent recursive agent spawning",
                    tool
                );
                def.disallowed_tools.push(tool.to_string());
            }
        }

        // 如果 tools 白名单中包含被禁止的工具，从白名单中移除
        // 避免白名单和黑名单的矛盾
        if let Some(ref mut tools) = def.tools {
            let before_len = tools.len();
            let disallowed_tools: HashSet<&str> =
                def.disallowed_tools.iter().map(String::as_str).collect();
            tools.retain(|t| t == "*" || !disallowed_tools.contains(t.as_str()));
            if tools.len() != before_len {
                tracing::warn!(
                    agent_type = %def.agent_type,
                    "Removed disallowed tools from tools whitelist"
                );
            }
        }

        if self.types.contains_key(&def.agent_type) {
            tracing::warn!(
                agent_type = %def.agent_type,
                "Overwriting existing agent type definition"
            );
        }
        self.types.insert(def.agent_type.clone(), def);
    }

    /// Get an agent type definition
    pub fn get(&self, agent_type: &str) -> Option<&AgentTypeDefinition> {
        self.types.get(agent_type)
    }

    /// Iterate over all types
    pub fn iter(&self) -> impl Iterator<Item = (&String, &AgentTypeDefinition)> {
        let mut types: Vec<_> = self.types.iter().collect();
        types.sort_by(|a, b| a.0.cmp(b.0));
        types.into_iter()
    }

    /// Get type names (for schema enum)
    pub fn type_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.types.keys().map(|s| s.as_str()).collect();
        names.sort();
        names
    }

    /// 验证 disallowed_tools 中的工具名是否存在于工具注册表
    ///
    /// 返回 (agent_type, 无效工具列表) 对，用于检测配置错误。
    pub fn validate_disallowed_tools(&self, known_tools: &[&str]) -> Vec<(&str, Vec<String>)> {
        let known_set: std::collections::HashSet<&str> = known_tools.iter().copied().collect();
        let mut invalid = Vec::new();

        for (type_name, def) in &self.types {
            let bad: Vec<String> = def
                .disallowed_tools
                .iter()
                .filter(|t| !known_set.contains(t.as_str()))
                .cloned()
                .collect();
            if !bad.is_empty() {
                invalid.push((type_name.as_str(), bad));
            }
        }
        invalid
    }
}

impl Default for AgentTypeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 为 AgentTypeRegistry 实现 blockcell_tools 的 AgentTypeRegistryOps trait
/// 避免循环依赖：tools crate 定义 trait，agent crate 实现 trait
impl blockcell_tools::AgentTypeRegistryOps for AgentTypeRegistry {
    fn type_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.types.keys().cloned().collect();
        names.sort();
        names
    }

    fn has_type(&self, agent_type: &str) -> bool {
        self.types.contains_key(agent_type)
    }

    fn get_description(&self, agent_type: &str) -> Option<String> {
        self.types.get(agent_type).map(|d| d.when_to_use.clone())
    }

    fn is_one_shot(&self, agent_type: &str) -> Option<bool> {
        self.types.get(agent_type).map(|d| d.one_shot)
    }

    fn get_permission_mode(&self, agent_type: &str) -> Option<String> {
        self.types.get(agent_type).map(|d| match d.permission_mode {
            PermissionMode::Inherit => "Inherit".to_string(),
            PermissionMode::Bubble => "Bubble".to_string(),
        })
    }
}

/// Predefined agent types
///
/// Built-in agent types provide specialized behaviors for common tasks:
/// - `explore`: Fast read-only codebase exploration
/// - `plan`: Architecture and implementation planning
/// - `coder`: Bounded code implementation and focused verification
/// - `reviewer`: Read-only review with severity-ordered findings
/// - `tester`: Test, lint, and build execution
/// - `verification`: Testing and validation specialist
/// - `viper`: Implementation agent for writing production code
/// - `general`: General-purpose agent for complex multi-step tasks
pub fn built_in_agent_types() -> Vec<AgentTypeDefinition> {
    vec![
        AgentTypeDefinition::explore(),
        AgentTypeDefinition::plan(),
        AgentTypeDefinition::coder(),
        AgentTypeDefinition::reviewer(),
        AgentTypeDefinition::tester(),
        AgentTypeDefinition::verification(),
        AgentTypeDefinition::viper(),
        AgentTypeDefinition::general(),
    ]
}

/// Lazy static reference to built-in agent types
///
/// This provides a convenient way to access built-in types without
/// calling the function each time. Uses std::sync::OnceLock for safe one-time initialization.
#[macro_export]
macro_rules! built_in_agent_types_lazy {
    () => {{
        static TYPES: std::sync::OnceLock<Vec<AgentTypeDefinition>> = std::sync::OnceLock::new();
        TYPES.get_or_init(built_in_agent_types)
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_mode_default_is_inherit() {
        let mode = PermissionMode::default();
        assert!(matches!(mode, PermissionMode::Inherit));
    }

    #[test]
    fn test_permission_mode_serde_serialize() {
        let mode = PermissionMode::Bubble;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"Bubble\"");
    }

    #[test]
    fn test_permission_mode_serde_deserialize() {
        let json = "\"Inherit\"";
        let mode: PermissionMode = serde_json::from_str(json).unwrap();
        assert_eq!(mode, PermissionMode::Inherit);
    }

    #[test]
    fn test_permission_mode_serde_deserialize_bubble() {
        let json = "\"Bubble\"";
        let mode: PermissionMode = serde_json::from_str(json).unwrap();
        assert_eq!(mode, PermissionMode::Bubble);
    }

    #[test]
    fn test_permission_mode_debug_output() {
        let mode = PermissionMode::Inherit;
        let debug_str = format!("{:?}", mode);
        assert_eq!(debug_str, "Inherit");
    }

    #[test]
    fn test_permission_mode_clone() {
        let mode = PermissionMode::Bubble;
        let cloned = mode.clone();
        assert_eq!(mode, cloned);
    }

    #[test]
    fn test_built_in_agent_types_count() {
        let types = built_in_agent_types();
        assert_eq!(types.len(), 8);
    }

    #[test]
    fn coding_agent_types_have_role_specific_tool_whitelists() {
        let coder = AgentTypeDefinition::coder();
        let reviewer = AgentTypeDefinition::reviewer();
        let tester = AgentTypeDefinition::tester();

        assert_eq!(coder.agent_type, "coder");
        assert!(coder
            .tools
            .as_ref()
            .unwrap()
            .contains(&"edit_file".to_string()));
        assert!(coder.tools.as_ref().unwrap().contains(&"shell".to_string()));

        assert_eq!(reviewer.agent_type, "reviewer");
        assert!(!reviewer
            .tools
            .as_ref()
            .unwrap()
            .contains(&"edit_file".to_string()));
        assert!(!reviewer
            .tools
            .as_ref()
            .unwrap()
            .contains(&"shell".to_string()));

        assert_eq!(tester.agent_type, "tester");
        assert_eq!(tester.tools.as_ref().unwrap(), &vec!["read_file", "shell"]);
    }

    #[test]
    fn test_agent_type_definition_explore() {
        let def = AgentTypeDefinition::explore();
        assert_eq!(def.agent_type, "explore");
        assert!(def.one_shot);
        assert_eq!(def.permission_mode, PermissionMode::Inherit);
    }

    #[test]
    fn test_agent_type_registry_new() {
        let registry = AgentTypeRegistry::new();
        assert_eq!(registry.type_names().len(), 8);
        assert!(registry.get("coder").is_some());
        assert!(registry.get("reviewer").is_some());
        assert!(registry.get("tester").is_some());
        assert!(registry.get("explore").is_some());
        assert!(registry.get("plan").is_some());
        assert!(registry.get("verification").is_some());
        assert!(registry.get("viper").is_some());
        assert!(registry.get("general").is_some());
    }

    #[test]
    fn test_agent_type_registry_outputs_names_in_stable_order() {
        let registry = AgentTypeRegistry::new();

        let names = registry.type_names();
        let iter_names = registry
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        let trait_names = blockcell_tools::AgentTypeRegistryOps::type_names(&registry);

        assert_eq!(
            names,
            vec![
                "coder",
                "explore",
                "general",
                "plan",
                "reviewer",
                "tester",
                "verification",
                "viper"
            ]
        );
        assert_eq!(iter_names, names);
        assert_eq!(trait_names, names);
    }

    #[test]
    fn test_agent_type_registry_register() {
        let mut registry = AgentTypeRegistry::new_empty();
        let custom_def = AgentTypeDefinition {
            agent_type: "custom".to_string(),
            when_to_use: "自定义 Agent 类型".to_string(),
            disallowed_tools: vec!["exec".to_string()],
            max_turns: Some(10),
            system_prompt_template: None,
            one_shot: false,
            permission_mode: PermissionMode::Inherit,
            isolation: None,
            tools: None,
            model: None,
            skills: vec![],
            mcp_servers: vec![],
            initial_prompt: None,
            background: false,
            color: None,
            source: AgentSource::BuiltIn,
            filename: None,
            base_dir: None,
        };
        registry.register(custom_def);
        assert!(registry.get("custom").is_some());
    }

    #[test]
    fn test_agent_type_registry_removes_all_disallowed_tools_from_whitelist() {
        let mut registry = AgentTypeRegistry::new_empty();
        let custom_def = AgentTypeDefinition {
            agent_type: "custom".to_string(),
            when_to_use: "自定义 Agent 类型".to_string(),
            tools: Some(vec![
                "read_file".to_string(),
                "exec".to_string(),
                "agent".to_string(),
            ]),
            disallowed_tools: vec!["exec".to_string()],
            ..Default::default()
        };

        registry.register(custom_def);
        let tools = registry.get("custom").unwrap().tools.as_ref().unwrap();

        assert_eq!(tools, &vec!["read_file".to_string()]);
    }

    #[test]
    fn test_validate_disallowed_tools_all_valid() {
        let registry = AgentTypeRegistry::new();
        let known_tools = vec![
            "agent",
            "spawn",
            "write_file",
            "edit_file",
            "exec",
            "read_file",
            "grep",
            "web_search",
            "shell",
            "skill_manage",
        ];
        let invalid = registry.validate_disallowed_tools(&known_tools);
        assert!(
            invalid.is_empty(),
            "All built-in disallowed_tools should be valid"
        );
    }

    #[test]
    fn test_validate_disallowed_tools_catches_invalid() {
        let mut registry = AgentTypeRegistry::new_empty();
        let def = AgentTypeDefinition {
            agent_type: "test".to_string(),
            when_to_use: "测试".to_string(),
            disallowed_tools: vec!["real_tool".to_string(), "phantom_tool".to_string()],
            max_turns: None,
            system_prompt_template: None,
            one_shot: false,
            permission_mode: PermissionMode::Inherit,
            isolation: None,
            tools: None,
            model: None,
            skills: vec![],
            mcp_servers: vec![],
            initial_prompt: None,
            background: false,
            color: None,
            source: AgentSource::BuiltIn,
            filename: None,
            base_dir: None,
        };
        registry.register(def);
        let known_tools = vec!["real_tool"];
        let invalid = registry.validate_disallowed_tools(&known_tools);
        assert_eq!(invalid.len(), 1);
        assert_eq!(invalid[0].0, "test");
        assert!(invalid[0].1.contains(&"phantom_tool".to_string()));
    }
}
