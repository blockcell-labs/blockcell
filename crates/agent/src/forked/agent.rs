//! Forked Agent 执行核心
//!
//! 提供与父进程共享 Prompt Cache 但状态隔离的子代理执行能力。
//!
//! ## 核心特性
//!
//! - **缓存共享**: 通过 CacheSafeParams 保证 Prompt Cache 命中
//! - **状态隔离**: 可变状态克隆独立副本
//! - **权限控制**: 通过 CanUseToolFn 限制工具调用
//! - **用量追踪**: 追踪所有 API 调用的 token 使用
//! - **工具执行**: 执行有限的文件操作工具（read/write/edit）

use super::{
    create_subagent_context, CacheSafeParams, CanUseToolFn, SubagentOverrides, ToolPermission,
};
use crate::memory_event;
use blockcell_core::types::ChatMessage;
use blockcell_core::UsageMetrics;
use blockcell_providers::ProviderPool;
use blockcell_tools::fuzzy_match::fuzzy_find_and_replace;
use blockcell_tools::security_scan::{scan_skill_content, scan_skill_dir_with_trust};
use blockcell_tools::skill_manage::{atomic_write_text, extract_frontmatter};
#[allow(deprecated)]
pub(crate) use blockcell_tools::SkillMutexHandle;
use blockcell_tools::{MemoryFileStoreHandle, MemoryStoreHandle, SkillFileStoreHandle};
use regex::Regex;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

// --- submodules extracted from the original monolithic forked/agent.rs ---
mod event;
mod run;
mod tool_exec;

pub use event::*;
pub use run::*;
pub(crate) use tool_exec::*;
/// Provider 获取重试配置
const PROVIDER_RETRY_MAX_ATTEMPTS: usize = 3;
const PROVIDER_RETRY_INITIAL_DELAY_MS: u64 = 100;
const PROVIDER_RETRY_MAX_DELAY_MS: u64 = 2000;

/// A file or directory that a typed agent is allowed to modify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceScopeEntry {
    pub path: PathBuf,
    pub recursive: bool,
}

impl WorkspaceScopeEntry {
    pub fn file(path: PathBuf) -> Self {
        Self {
            path,
            recursive: false,
        }
    }

    pub fn directory(path: PathBuf) -> Self {
        Self {
            path,
            recursive: true,
        }
    }

    pub fn allows(&self, path: &Path) -> bool {
        path == self.path || (self.recursive && path.starts_with(&self.path))
    }
}

/// Forked Agent 参数
///
/// ## 必须设置 provider_pool
///
/// `provider_pool` 是必需参数。使用以下方式创建：
///
/// ```ignore
/// // 方式 1: 使用 new() 构造函数（推荐）
/// let params = ForkedAgentParams::new(provider_pool, prompt_messages, cache_safe_params);
///
/// // 方式 2: 使用 builder()
/// let params = ForkedAgentParams::builder()
///     .provider_pool(provider_pool)
///     .prompt_messages(prompt_messages)
///     .cache_safe_params(cache_safe_params)
///     .build();
///
/// // 方式 3: Default + 必须调用 set_provider_pool()
/// let params = ForkedAgentParams {
///     provider_pool: Some(provider_pool),
///     ..Default::default()
/// };
/// ```
///
/// **警告**: 如果 `provider_pool` 为 `None`，`run_forked_agent` 会返回 `NoProviderAvailable` 错误。
#[allow(deprecated)]
pub struct ForkedAgentParams {
    /// 子代理查询循环的初始消息
    pub prompt_messages: Vec<ChatMessage>,
    /// 缓存安全参数
    pub cache_safe_params: CacheSafeParams,
    /// Provider 池 (必须设置)
    pub provider_pool: Option<Arc<ProviderPool>>,
    /// Optional origin-scoped observation and shared budget for user delegation.
    pub observation: Option<ForkObservation>,
    /// 权限检查函数
    pub can_use_tool: CanUseToolFn,
    /// 来源标识符
    pub query_source: &'static str,
    /// 分析标签
    pub fork_label: &'static str,
    /// 子代理上下文覆盖选项
    pub overrides: Option<SubagentOverrides>,
    /// 输出 token 上限（注意：会改变缓存键！）
    pub max_output_tokens: Option<u32>,
    /// 最大轮次限制
    pub max_turns: Option<u32>,
    /// 跳过 transcript 记录
    pub skip_transcript: bool,
    /// 跳过最后消息的缓存写入
    pub skip_cache_write: bool,
    /// 系统提示（可选，覆盖 cache_safe_params）
    pub system_prompt: Option<String>,
    /// Agent 类型（Fork 模式为 None）
    pub agent_type: Option<String>,
    /// 禁用工具列表
    pub disallowed_tools: Vec<String>,
    /// ONE_SHOT 标记
    pub one_shot: bool,
    /// 工作目录（用于 worktree 隔离）
    pub working_dir: Option<PathBuf>,
    /// Files/directories this agent may modify.
    pub workspace_scope: Vec<WorkspaceScopeEntry>,
    /// 事件发送通道（可选，用于向父级转发进度事件如 tool_call_start、token 等）
    pub event_tx: Option<tokio::sync::broadcast::Sender<String>>,
    /// 进度通道（可选，用于通过 TaskManager 转发工具调用事件到外部渠道）
    pub progress_tx: Option<tokio::sync::mpsc::Sender<crate::agent_progress::AgentProgress>>,
    /// 工具 schema 定义（发送给 LLM，让它知道可以调用哪些工具）
    pub tool_schemas: Vec<serde_json::Value>,
    /// 任务 ID（用于在事件中区分同类型多个子agent）
    pub task_id: Option<String>,
    /// Memory store handle (shared from parent agent via Arc)
    pub memory_store: Option<MemoryStoreHandle>,
    /// File-backed memory store handle (USER.md / MEMORY.md).
    pub memory_file_store: Option<MemoryFileStoreHandle>,
    /// File-backed skill store handle.
    pub skill_file_store: Option<SkillFileStoreHandle>,
    /// Skills directory (for skill_manage/list_skills in review mode)
    pub skills_dir: Option<PathBuf>,
    /// External skills directories (builtin_skills_dir etc., for skill search)
    pub external_skills_dirs: Vec<PathBuf>,
    /// Skill mutex (shared with parent agent to prevent concurrent skill modifications)
    pub skill_mutex: Option<SkillMutexHandle>,
    /// 允许的工具列表 (None = 全部工具)
    pub tools: Option<Vec<String>>,
    /// 模型覆盖 (None = inherit from parent)
    pub model: Option<String>,
    /// 预加载的技能列表
    pub skills: Vec<String>,
    /// MCP 服务器引用列表
    pub mcp_servers: Vec<String>,
    /// 首轮提示注入
    pub initial_prompt: Option<String>,
    /// 是否后台运行
    pub background: bool,
    /// UI 显示颜色
    pub color: Option<String>,
}

impl ForkedAgentParams {
    /// 创建新的 ForkedAgentParams（推荐方式）
    ///
    /// 必须参数通过构造函数强制设置，可选参数通过方法链设置。
    ///
    /// ## 参数
    /// - `provider_pool`: LLM Provider 池（必需）
    /// - `prompt_messages`: 子代理的初始消息
    /// - `cache_safe_params`: 缓存安全参数
    pub fn new(
        provider_pool: Arc<ProviderPool>,
        prompt_messages: Vec<ChatMessage>,
        cache_safe_params: CacheSafeParams,
    ) -> Self {
        Self {
            provider_pool: Some(provider_pool),
            observation: None,
            prompt_messages,
            cache_safe_params,
            // Fail closed: a forked agent must be given an explicit policy (roster or
            // allowlist). The old allow-all default made "forgot to configure" equal to
            // "everything permitted".
            can_use_tool: crate::forked::create_allowlist_can_use_tool(Vec::new()),
            query_source: "forked",
            fork_label: "forked",
            overrides: None,
            max_output_tokens: None,
            max_turns: None,
            skip_transcript: true,
            skip_cache_write: false,
            system_prompt: None,
            agent_type: None,
            disallowed_tools: Vec::new(),
            one_shot: false,
            working_dir: None,
            workspace_scope: Vec::new(),
            event_tx: None,
            progress_tx: None,
            tool_schemas: Vec::new(),
            task_id: None,
            memory_store: None,
            memory_file_store: None,
            skill_file_store: None,
            skills_dir: None,
            external_skills_dirs: Vec::new(),
            skill_mutex: None,
            tools: None,
            model: None,
            skills: Vec::new(),
            mcp_servers: Vec::new(),
            initial_prompt: None,
            background: false,
            color: None,
        }
    }

    /// 设置 memory_store（共享父代理的 Memory Store）
    pub fn with_memory_store(mut self, store: MemoryStoreHandle) -> Self {
        self.memory_store = Some(store);
        self
    }

    /// Set file-backed memory store.
    pub fn with_memory_file_store(mut self, store: MemoryFileStoreHandle) -> Self {
        self.memory_file_store = Some(store);
        self
    }

    /// Set file-backed skill store.
    pub fn with_skill_file_store(mut self, store: SkillFileStoreHandle) -> Self {
        self.skill_file_store = Some(store);
        self
    }

    /// 设置 skills_dir（用于 skill_manage/list_skills 工具）
    pub fn with_skills_dir(mut self, dir: PathBuf) -> Self {
        self.skills_dir = Some(dir);
        self
    }

    /// 设置 external_skills_dirs（用于跨目录搜索 Skill, 如 builtin_skills_dir）
    pub fn with_external_skills_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.external_skills_dirs = dirs;
        self
    }

    /// 设置 skill_mutex（共享父代理的 SkillMutex，防止并发修改）
    #[allow(deprecated)]
    pub fn with_skill_mutex(mut self, mutex: SkillMutexHandle) -> Self {
        self.skill_mutex = Some(mutex);
        self
    }

    /// 设置工具 schema 列表（传给 provider.chat() 让 LLM 知道可用工具）
    pub fn with_tool_schemas(mut self, schemas: Vec<serde_json::Value>) -> Self {
        self.tool_schemas = schemas;
        self
    }

    /// 创建 Builder（用于复杂配置）
    ///
    /// Builder 模式允许链式设置所有参数，`build()` 会验证必需参数。
    pub fn builder() -> ForkedAgentParamsBuilder {
        ForkedAgentParamsBuilder::default()
    }

    /// 设置 prompt_messages
    pub fn with_prompt_messages(mut self, messages: Vec<ChatMessage>) -> Self {
        self.prompt_messages = messages;
        self
    }

    /// 设置 cache_safe_params
    pub fn with_cache_safe_params(mut self, params: CacheSafeParams) -> Self {
        self.cache_safe_params = params;
        self
    }

    /// 设置权限检查函数
    pub fn with_can_use_tool(mut self, can_use_tool: CanUseToolFn) -> Self {
        self.can_use_tool = can_use_tool;
        self
    }

    /// 设置来源标识符
    pub fn with_query_source(mut self, source: &'static str) -> Self {
        self.query_source = source;
        self
    }

    /// 设置分析标签
    pub fn with_fork_label(mut self, label: &'static str) -> Self {
        self.fork_label = label;
        self
    }

    /// 设置最大轮次
    pub fn with_max_turns(mut self, max_turns: u32) -> Self {
        self.max_turns = Some(max_turns);
        self
    }

    /// 设置最大输出 tokens
    pub fn with_max_output_tokens(mut self, max_tokens: u32) -> Self {
        self.max_output_tokens = Some(max_tokens);
        self
    }

    /// 设置系统提示
    pub fn with_system_prompt(mut self, prompt: String) -> Self {
        self.system_prompt = Some(prompt);
        self
    }

    /// 验证必需参数
    ///
    /// 返回 `Ok(())` 如果必需参数都已设置，否则返回错误。
    pub fn validate(&self) -> Result<(), ForkedAgentError> {
        if self.provider_pool.is_none() {
            return Err(ForkedAgentError::NoProviderAvailable);
        }
        Ok(())
    }
}

/// ForkedAgentParams Builder
///
/// 用于链式构建 ForkedAgentParams，`build()` 会验证必需参数。
#[allow(deprecated)]
#[derive(Default)]
pub struct ForkedAgentParamsBuilder {
    prompt_messages: Vec<ChatMessage>,
    cache_safe_params: CacheSafeParams,
    provider_pool: Option<Arc<ProviderPool>>,
    observation: Option<ForkObservation>,
    can_use_tool: Option<CanUseToolFn>,
    query_source: &'static str,
    fork_label: &'static str,
    overrides: Option<SubagentOverrides>,
    max_output_tokens: Option<u32>,
    max_turns: Option<u32>,
    skip_transcript: bool,
    skip_cache_write: bool,
    system_prompt: Option<String>,
    agent_type: Option<String>,
    disallowed_tools: Option<Vec<String>>,
    one_shot: bool,
    working_dir: Option<PathBuf>,
    workspace_scope: Vec<WorkspaceScopeEntry>,
    event_tx: Option<tokio::sync::broadcast::Sender<String>>,
    progress_tx: Option<tokio::sync::mpsc::Sender<crate::agent_progress::AgentProgress>>,
    tool_schemas: Option<Vec<serde_json::Value>>,
    task_id: Option<String>,
    memory_store: Option<MemoryStoreHandle>,
    memory_file_store: Option<MemoryFileStoreHandle>,
    skill_file_store: Option<SkillFileStoreHandle>,
    skills_dir: Option<PathBuf>,
    external_skills_dirs: Vec<PathBuf>,
    skill_mutex: Option<SkillMutexHandle>,
    /// 允许的工具列表
    tools: Option<Vec<String>>,
    /// 模型覆盖
    model: Option<String>,
    /// 预加载的技能列表
    skills: Vec<String>,
    /// MCP 服务器引用列表
    mcp_servers: Vec<String>,
    /// 首轮提示注入
    initial_prompt: Option<String>,
    /// 是否后台运行
    background: bool,
    /// UI 显示颜色
    color: Option<String>,
}

#[derive(Clone)]
pub struct ForkObservation {
    pub paths: blockcell_core::Paths,
    pub session_key: String,
    pub budget: blockcell_core::BudgetTrackerHandle,
    pub model_prices: Vec<blockcell_core::config::ModelEntry>,
}

impl ForkedAgentParamsBuilder {
    pub fn observation(mut self, observation: ForkObservation) -> Self {
        self.observation = Some(observation);
        self
    }

    /// 设置 provider_pool（必需）
    pub fn provider_pool(mut self, pool: Arc<ProviderPool>) -> Self {
        self.provider_pool = Some(pool);
        self
    }

    /// 设置 prompt_messages
    pub fn prompt_messages(mut self, messages: Vec<ChatMessage>) -> Self {
        self.prompt_messages = messages;
        self
    }

    /// 设置 cache_safe_params
    pub fn cache_safe_params(mut self, params: CacheSafeParams) -> Self {
        self.cache_safe_params = params;
        self
    }

    /// 设置权限检查函数
    pub fn can_use_tool(mut self, can_use_tool: CanUseToolFn) -> Self {
        self.can_use_tool = Some(can_use_tool);
        self
    }

    /// 设置来源标识符
    pub fn query_source(mut self, source: &'static str) -> Self {
        self.query_source = source;
        self
    }

    /// 设置分析标签
    pub fn fork_label(mut self, label: &'static str) -> Self {
        self.fork_label = label;
        self
    }

    /// 设置子代理上下文覆盖
    pub fn overrides(mut self, overrides: SubagentOverrides) -> Self {
        self.overrides = Some(overrides);
        self
    }

    /// 设置最大轮次
    pub fn max_turns(mut self, max_turns: u32) -> Self {
        self.max_turns = Some(max_turns);
        self
    }

    /// 设置最大输出 tokens
    pub fn max_output_tokens(mut self, max_tokens: u32) -> Self {
        self.max_output_tokens = Some(max_tokens);
        self
    }

    /// 设置跳过 transcript
    pub fn skip_transcript(mut self, skip: bool) -> Self {
        self.skip_transcript = skip;
        self
    }

    /// 设置跳过缓存写入
    pub fn skip_cache_write(mut self, skip: bool) -> Self {
        self.skip_cache_write = skip;
        self
    }

    /// 设置系统提示
    pub fn system_prompt(mut self, prompt: String) -> Self {
        self.system_prompt = Some(prompt);
        self
    }

    /// 设置 Agent 类型
    pub fn agent_type(mut self, agent_type: Option<String>) -> Self {
        self.agent_type = agent_type;
        self
    }

    /// 设置禁用工具列表
    pub fn disallowed_tools(mut self, tools: Vec<String>) -> Self {
        self.disallowed_tools = Some(tools);
        self
    }

    /// 设置 ONE_SHOT 标记
    pub fn one_shot(mut self, one_shot: bool) -> Self {
        self.one_shot = one_shot;
        self
    }

    /// 设置工作目录（用于 worktree 隔离）
    pub fn working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = Some(dir);
        self
    }

    /// Restrict file mutations to the supplied paths.
    pub fn workspace_scope(mut self, scope: Vec<WorkspaceScopeEntry>) -> Self {
        self.workspace_scope = scope;
        self
    }

    /// 设置事件发送通道（用于向父级转发进度事件）
    pub fn event_tx(mut self, tx: tokio::sync::broadcast::Sender<String>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// 设置进度通道（用于通过 TaskManager 转发工具调用事件到外部渠道）
    pub fn progress_tx(
        mut self,
        tx: tokio::sync::mpsc::Sender<crate::agent_progress::AgentProgress>,
    ) -> Self {
        self.progress_tx = Some(tx);
        self
    }

    /// 设置工具 schema 定义（发送给 LLM，让它知道可以调用哪些工具）
    pub fn tool_schemas(mut self, schemas: Vec<serde_json::Value>) -> Self {
        self.tool_schemas = Some(schemas);
        self
    }

    /// 设置任务 ID（用于在事件中区分同类型多个子agent）
    pub fn task_id(mut self, task_id: Option<String>) -> Self {
        self.task_id = task_id;
        self
    }

    /// 设置 memory_store（共享父代理的 Memory Store）
    pub fn memory_store(mut self, store: MemoryStoreHandle) -> Self {
        self.memory_store = Some(store);
        self
    }

    /// Set file-backed memory store.
    pub fn memory_file_store(mut self, store: MemoryFileStoreHandle) -> Self {
        self.memory_file_store = Some(store);
        self
    }

    /// Set file-backed skill store.
    pub fn skill_file_store(mut self, store: SkillFileStoreHandle) -> Self {
        self.skill_file_store = Some(store);
        self
    }

    /// 设置 skills_dir（用于 skill_manage/list_skills 工具）
    pub fn skills_dir(mut self, dir: PathBuf) -> Self {
        self.skills_dir = Some(dir);
        self
    }

    /// 设置 external_skills_dirs（用于跨目录搜索 Skill）
    pub fn external_skills_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.external_skills_dirs = dirs;
        self
    }

    /// 设置 skill_mutex（共享父代理的 SkillMutex，防止并发修改）
    #[allow(deprecated)]
    pub fn skill_mutex(mut self, mutex: SkillMutexHandle) -> Self {
        self.skill_mutex = Some(mutex);
        self
    }

    /// 设置允许的工具列表 (None = 全部工具)
    pub fn tools(mut self, tools: Option<Vec<String>>) -> Self {
        self.tools = tools;
        self
    }

    /// 设置模型覆盖 (None = 继承父级)
    pub fn model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    /// 设置预加载的技能列表
    pub fn skills(mut self, skills: Vec<String>) -> Self {
        self.skills = skills;
        self
    }

    /// 设置 MCP 服务器引用列表
    pub fn mcp_servers(mut self, servers: Vec<String>) -> Self {
        self.mcp_servers = servers;
        self
    }

    /// 设置首轮提示注入
    pub fn initial_prompt(mut self, prompt: Option<String>) -> Self {
        self.initial_prompt = prompt;
        self
    }

    /// 设置是否后台运行
    pub fn background(mut self, background: bool) -> Self {
        self.background = background;
        self
    }

    /// 设置 UI 显示颜色
    pub fn color(mut self, color: Option<String>) -> Self {
        self.color = color;
        self
    }

    /// 构建 ForkedAgentParams
    ///
    /// 如果 `provider_pool` 未设置，返回 `ForkedAgentError::NoProviderAvailable`。
    pub fn build(self) -> Result<ForkedAgentParams, ForkedAgentError> {
        if self.provider_pool.is_none() {
            return Err(ForkedAgentError::NoProviderAvailable);
        }

        let tool_schemas = self.tool_schemas.unwrap_or_default();
        // Fail closed: without an explicit policy, allow exactly the tools this fork
        // declares to the model and deny everything else. The previous allow-all
        // default let an unconfigured fork execute any tool the executor supported —
        // including ones never advertised in its schema list.
        let can_use_tool = self.can_use_tool.unwrap_or_else(|| {
            crate::forked::create_allowlist_can_use_tool(crate::forked::tool_names_from_schemas(
                &tool_schemas,
            ))
        });

        Ok(ForkedAgentParams {
            prompt_messages: self.prompt_messages,
            cache_safe_params: self.cache_safe_params,
            provider_pool: self.provider_pool,
            observation: self.observation,
            can_use_tool,
            query_source: self.query_source,
            fork_label: self.fork_label,
            overrides: self.overrides,
            max_output_tokens: self.max_output_tokens,
            max_turns: self.max_turns,
            skip_transcript: self.skip_transcript,
            skip_cache_write: self.skip_cache_write,
            system_prompt: self.system_prompt,
            agent_type: self.agent_type,
            disallowed_tools: self.disallowed_tools.unwrap_or_default(),
            one_shot: self.one_shot,
            working_dir: self.working_dir,
            workspace_scope: self.workspace_scope,
            event_tx: self.event_tx,
            progress_tx: self.progress_tx,
            tool_schemas,
            task_id: self.task_id,
            memory_store: self.memory_store,
            memory_file_store: self.memory_file_store,
            skill_file_store: self.skill_file_store,
            skills_dir: self.skills_dir,
            external_skills_dirs: self.external_skills_dirs,
            skill_mutex: self.skill_mutex,
            tools: self.tools,
            model: self.model,
            skills: self.skills,
            mcp_servers: self.mcp_servers,
            initial_prompt: self.initial_prompt,
            background: self.background,
            color: self.color,
        })
    }
}

// 注意：故意不实现 Default trait
// ForkedAgentParams 必须通过 new() 或 builder() 创建
// 这确保 provider_pool 在编译时被强制设置

/// Forked Agent 结果
#[derive(Debug)]
pub struct ForkedAgentResult {
    /// 查询循环产生的所有消息
    pub messages: Vec<ChatMessage>,
    /// 所有 API 调用的累积用量
    pub total_usage: UsageMetrics,
    /// 修改的文件列表
    pub files_modified: Vec<String>,
    /// 最终响应内容
    pub final_content: Option<String>,
    /// 是否因达到 max_turns 而截断（未完成所有工具调用）
    pub truncated: bool,
    /// 是否有工具调用执行失败（权限拒绝、old_string not found 等）
    /// memory extraction 应据此判断是否跳过游标推进和 record_success
    pub had_tool_error: bool,
}

/// Forked Agent 错误
#[derive(Debug, thiserror::Error)]
pub enum ForkedAgentError {
    #[error("LLM provider error: {0}")]
    ProviderError(String),

    #[error("Tool execution error: {0}")]
    ToolError(String),

    #[error("Max turns exceeded")]
    MaxTurnsExceeded,

    #[error("No provider available")]
    NoProviderAvailable,

    #[error("Aborted: {0}")]
    Aborted(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Tool not supported in forked mode: {0}")]
    ToolNotSupported(String),
}

/// 执行 Forked Agent 工具
///
/// 内容大小限制常量
const MAX_FILE_SIZE: usize = 10 * 1024 * 1024; // 10MB
const MAX_EDIT_SIZE: usize = 100 * 1024; // 100KB for new_string
const MAX_SKILL_CONTENT_CHARS: usize = 100_000; // 100K chars for skill content (与主工具一致)
const MAX_OUTPUT_CHARS: usize = 50000;

/// Skill 名称正则 (与主 skill_manage 工具一致): 小写字母、数字、点、下划线、连字符
static VALID_SKILL_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("^[a-z0-9][a-z0-9._-]*$").expect("VALID_SKILL_NAME_RE"));
static DANGEROUS_COMMAND_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"rm\s+-rf\s+/",
        r"rm\s+-rf\s+~",
        r"rm\s+-rf\s+\*",
        r"\bdd\b.*\bif=",
        r"\bformat\b",
        r"\bshutdown\b",
        r"\breboot\b",
        r":\(\)\s*\{\s*:\|:\s*&\s*\}\s*;",
        r">\s*/dev/sd",
        r"mkfs\.",
    ]
    .iter()
    .map(|pattern| Regex::new(pattern).expect("dangerous command regex"))
    .collect()
});

/// 验证路径安全性（防御性检查）
///
/// 即使 can_use_tool 已经验证过，这里再次检查作为 fail-safe。
///
/// ## 检查项
///
/// 1. 路径不能包含 `..`（路径遍历）
/// 2. 路径不能包含空字节
///
/// ## 不检查绝对路径
///
/// 此函数**不检查绝对路径**，原因：
/// - `can_use_tool` 回调（如 `create_auto_mem_can_use_tool`）已限制路径范围
/// - `is_path_within_directory` 和 `is_auto_mem_path` 会解析符号链接并验证目录边界
/// - 此层仅作为 fail-safe，不应过度限制合法用例
///
/// ## 安全模型
///
/// ```text
/// 用户输入 -> can_use_tool 回调（主要防护） -> validate_path_safety（fail-safe）
/// ```
/// Forked Agent 事件
///
/// 用于遥测和日志记录
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ForkAgentEvent {
    /// Fork 标签
    pub fork_label: &'static str,
    /// 查询来源
    pub query_source: &'static str,
    /// 执行时长 (ms)
    pub duration_ms: u64,
    /// 消息数量
    pub message_count: usize,
    /// 用量指标
    pub total_usage: UsageMetrics,
}

impl ForkAgentEvent {
    /// 记录事件到日志
    #[allow(dead_code)]
    pub fn log(&self) {
        tracing::info!(
            fork_label = self.fork_label,
            query_source = self.query_source,
            duration_ms = self.duration_ms,
            message_count = self.message_count,
            input_tokens = self.total_usage.input_tokens,
            output_tokens = self.total_usage.output_tokens,
            cache_read = self.total_usage.cache_read_input_tokens,
            cache_creation = self.total_usage.cache_creation_input_tokens,
            cache_hit_rate = self.total_usage.cache_hit_rate(),
            "[fork_agent_event]"
        );
    }
}

#[cfg(test)]
mod tests;
