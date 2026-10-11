pub mod agent;
pub mod agent_status;
pub mod alert_rule;
pub mod app_control;
pub mod audio_transcribe;
mod bounded_io;
pub mod browser;
pub mod camera;
pub mod chart_generate;
pub mod code_search;
pub mod community_hub;
pub mod context_remaining;
pub mod cron;
pub mod data_process;
pub mod email;
pub mod encrypt;
pub mod evolution_workflow;
pub mod exec;
pub mod exec_local;
pub mod exec_skill_script;
pub mod file_ops;
pub mod fs;
pub mod fuzzy_match;
pub mod html_to_md;
pub mod http_request;
pub mod image_understand;
pub mod knowledge_forget;
pub mod knowledge_graph;
pub mod mcp;
pub mod memory;
pub mod memory_maintenance;
pub mod message;
#[cfg(feature = "napcat")]
pub mod napcat;
pub mod network_monitor;
pub mod ocr;
pub mod office;
pub mod office_write;
pub mod plan;
mod process;
pub mod registry;
pub mod registry_builder;
pub mod sandbox;
pub mod security_scan;
pub mod send_message;
pub mod session_recall;
pub mod session_search;
pub mod shell_executor;
pub mod shell_session;
pub mod skill_manage;
pub mod skills;
pub mod spawn;
pub(crate) mod ssrf;
pub mod stream_subscribe;
pub mod subagent;
pub mod system_info;
pub mod tasks;
pub mod termux_api;
pub mod toggle_manage;
pub mod tts;
pub mod video_process;
pub mod web;

use async_trait::async_trait;
use blockcell_core::system_event::{EventPriority, SystemEvent};
use blockcell_core::types::PermissionSet;
use blockcell_core::{AgentIdentity, Config, OutboundMessage, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

pub use registry::ToolRegistry;
pub use registry_builder::{
    build_tool_registry_for_agent_config, build_tool_registry_with_all_mcp,
};

/// Truncate a string to at most `max_chars` characters, respecting UTF-8 char boundaries.
/// Returns a borrowed slice if no truncation needed, or an owned String if truncated.
pub fn safe_truncate(s: &str, max_chars: usize) -> &str {
    if s.chars().count() <= max_chars {
        return s;
    }
    // 找到第 max_chars 个字符的字节边界
    let end = s
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[..end]
}

/// Sender handle for outbound messages (used by message tool).
pub type OutboundSender = mpsc::Sender<OutboundMessage>;

/// Trait for spawning subagents from tools, breaking the circular dependency
/// between the tools crate and the agent crate.
#[async_trait]
pub trait SpawnHandle: Send + Sync {
    /// Spawn a subagent task. Returns a JSON string with task_id and status.
    ///
    /// # Arguments
    /// - `task`: The task description for the subagent
    /// - `label`: A label for the task (used in task list)
    /// - `origin_channel`: The channel the request came from
    /// - `origin_chat_id`: The chat ID of the original conversation
    /// - `agent_type`: Optional agent type identifier (e.g., "explore", "plan")
    fn spawn(
        &self,
        task: &str,
        label: &str,
        origin_channel: &str,
        origin_chat_id: &str,
        agent_type: Option<&str>,
    ) -> Result<Value>;
}

/// Runtime capabilities exposed to tools (subagents, context-window estimates).
pub trait RuntimeHandle: Send + Sync {
    /// Provider used by the agent tool to spawn fork / typed subagents.
    fn subagents(&self) -> Arc<dyn subagent::SubagentProvider>;

    /// Return the latest assembled context-window estimate for a persisted session.
    fn context_window_snapshot(&self, session_key: &str) -> blockcell_core::ContextWindowSnapshot;
}

/// Opaque handle to the runtime for typed agent execution.
pub type AgentRuntimeHandle = Arc<dyn RuntimeHandle>;

/// Trait for accessing agent type registry from the agent tool.
/// Supports dynamic schema generation and type validation.
/// This avoids a circular dependency between tools and agent crates.
pub trait AgentTypeRegistryOps: Send + Sync {
    /// Get all available agent type names
    fn type_names(&self) -> Vec<String>;

    /// Check if an agent type exists
    fn has_type(&self, agent_type: &str) -> bool;

    /// Get the description (when_to_use) for an agent type
    fn get_description(&self, agent_type: &str) -> Option<String>;

    /// Check if an agent type is one-shot
    fn is_one_shot(&self, agent_type: &str) -> Option<bool>;

    /// Get the permission mode name for an agent type
    fn get_permission_mode(&self, agent_type: &str) -> Option<String>;
}

/// Opaque handle to agent type registry, passed through ToolContext.
pub type AgentTypeRegistryHandle = Arc<dyn AgentTypeRegistryOps + Send + Sync>;

/// Opaque handle to the task manager, passed through ToolContext.
/// This avoids a circular dependency between tools and agent crates.
pub type TaskManagerHandle = Arc<dyn TaskManagerOps + Send + Sync>;

/// Opaque handle to the memory store, passed through ToolContext.
pub type MemoryStoreHandle = Arc<dyn MemoryStoreOps + Send + Sync>;

/// Opaque handle to file memory, passed through ToolContext.
pub type MemoryFileStoreHandle = Arc<dyn MemoryFileStoreOps + Send + Sync>;

/// Opaque handle to workspace file skills, passed through ToolContext.
pub type SkillFileStoreHandle = Arc<dyn SkillFileStoreOps + Send + Sync>;

/// Opaque handle to read-only session search for background learning review.
pub type SessionSearchHandle = Arc<dyn SessionSearchOps + Send + Sync>;

/// Opaque handle to the response cache, passed through ToolContext.
pub type ResponseCacheHandle = Arc<dyn ResponseCacheOps + Send + Sync>;

/// Opaque handle to the capability registry, passed through ToolContext.
pub type CapabilityRegistryHandle = Arc<dyn CapabilityRegistryOps + Send + Sync>;

/// Opaque handle to the core evolution engine, passed through ToolContext.
pub type CoreEvolutionHandle = Arc<Mutex<dyn CoreEvolutionOps + Send + Sync>>;

/// Opaque handle to the system event emitter, passed through ToolContext.
pub type EventEmitterHandle = Arc<dyn SystemEventEmitter + Send + Sync>;

/// Opaque guard returned by `try_acquire`. Releases the lock on drop.
pub type SkillMutexGuard = Arc<dyn Send + Sync>;

/// Opaque handle to the skill mutex, passed through ToolContext.
/// This avoids a circular dependency between tools and agent crates.
pub type SkillMutexHandle = Arc<dyn SkillMutexOps + Send + Sync>;

/// Opaque handle to the evolution workflow store, passed through ToolContext.
/// This avoids a circular dependency between tools and storage crates.
pub type EvolutionWorkflowStoreHandle = Arc<dyn EvolutionWorkflowStoreOps + Send + Sync>;

/// Trait abstracting skill mutex operations needed by tools.
#[async_trait]
pub trait SkillMutexOps: Send + Sync {
    /// Check if a skill can be modified (not currently active).
    async fn can_modify(&self, skill_name: &str) -> bool;

    /// Try to acquire exclusive access to a skill for modification.
    /// Returns a guard that releases the lock on drop, or None if the skill is active.
    /// This is preferred over `can_modify` for write operations to prevent TOCTOU races.
    fn try_acquire(&self, skill_name: &str) -> Option<SkillMutexGuard>;
}

/// Trait abstracting system event emission needed by tools and runtime services.
pub trait SystemEventEmitter: Send + Sync {
    fn emit(&self, event: SystemEvent);

    fn emit_simple(
        &self,
        kind: &str,
        source: &str,
        priority: EventPriority,
        title: &str,
        summary: &str,
    ) {
        self.emit(SystemEvent::new_main_session(
            kind, source, priority, title, summary,
        ));
    }
}

/// Trait abstracting capability registry operations needed by tools.
#[async_trait]
pub trait CapabilityRegistryOps: Send + Sync {
    /// List all capabilities as JSON.
    async fn list_all_json(&self) -> Value;
    /// Get a capability descriptor by ID as JSON.
    async fn get_descriptor_json(&self, id: &str) -> Option<Value>;
    /// Get registry stats as JSON.
    async fn stats_json(&self) -> Value;
    /// Execute a capability by ID.
    async fn execute_capability(&self, id: &str, input: Value) -> Result<Value>;
    /// Generate brief for prompt injection.
    async fn generate_brief(&self) -> String;
    /// List IDs of all available (active) capabilities.
    async fn list_available_ids(&self) -> Vec<String>;
}

/// Trait abstracting core evolution operations needed by tools.
#[async_trait]
pub trait CoreEvolutionOps: Send + Sync {
    /// Request a new capability evolution.
    async fn request_capability(
        &self,
        capability_id: &str,
        description: &str,
        provider_kind_str: &str,
    ) -> Result<Value>;
    /// List evolution records as JSON.
    async fn list_records_json(&self) -> Result<Value>;
    /// Get a specific evolution record.
    async fn get_record_json(&self, evolution_id: &str) -> Result<Value>;
    /// Unblock a previously blocked capability.
    async fn unblock_capability(&self, capability_id: &str) -> Result<Value>;
}

/// Trait abstracting evolution workflow store operations needed by tools.
/// This avoids a circular dependency between tools and storage crates.
pub trait EvolutionWorkflowStoreOps: Send + Sync {
    /// List workflows, optionally filtered by status.
    fn list_workflows_json(&self, status_filter: Option<&str>) -> Result<Value>;
    /// Get a specific workflow by ID.
    fn get_workflow_json(&self, workflow_id: &str) -> Result<Value>;
    /// Get steps for a workflow.
    fn get_workflow_steps_json(&self, workflow_id: &str) -> Result<Value>;
    /// Cancel a workflow.
    fn cancel_workflow(&self, workflow_id: &str) -> Result<Value>;
    /// Retry a failed or blocked workflow.
    fn retry_workflow(&self, workflow_id: &str) -> Result<Value>;
    /// Unblock a capability.
    fn unblock_capability(&self, capability_id: &str) -> Result<Value>;
}

/// Trait abstracting memory store operations needed by tools.
/// This avoids a circular dependency between tools and storage crates.
pub trait MemoryStoreOps: Send + Sync {
    /// Upsert a memory item. Returns the item as JSON.
    fn upsert_json(&self, params_json: Value) -> Result<Value>;
    /// Query memory items. Returns results as JSON array.
    fn query_json(&self, params_json: Value) -> Result<Value>;
    /// Soft-delete a memory item by ID. Returns success boolean.
    fn soft_delete(&self, id: &str) -> Result<bool>;
    /// Soft-delete a memory item owned by the caller session.
    fn soft_delete_in_session(&self, _id: &str, _session_key: &str) -> Result<bool> {
        Err(blockcell_core::Error::Validation(
            "Session-scoped memory deletion is not supported by this store".to_string(),
        ))
    }
    /// Batch soft-delete by filter. Returns count of deleted items.
    fn batch_soft_delete_json(&self, params_json: Value) -> Result<usize>;
    /// Batch soft-delete caller-owned memory items only.
    fn batch_soft_delete_in_session_json(&self, _params_json: Value) -> Result<usize> {
        Err(blockcell_core::Error::Validation(
            "Session-scoped batch memory deletion is not supported by this store".to_string(),
        ))
    }
    /// Restore a soft-deleted item. Returns success boolean.
    fn restore(&self, id: &str) -> Result<bool>;
    /// Restore a memory item owned by the caller session.
    fn restore_in_session(&self, _id: &str, _session_key: &str) -> Result<bool> {
        Err(blockcell_core::Error::Validation(
            "Session-scoped memory restore is not supported by this store".to_string(),
        ))
    }
    /// Get memory stats as JSON.
    fn stats_json(&self) -> Result<Value>;
    /// Get statistics for caller-owned memory items only.
    fn stats_in_session_json(&self, _session_key: &str) -> Result<Value> {
        Err(blockcell_core::Error::Validation(
            "Session-scoped memory statistics are not supported by this store".to_string(),
        ))
    }
    /// Generate brief for prompt injection.
    fn generate_brief(&self, long_term_max: usize, short_term_max: usize) -> Result<String>;
    fn generate_brief_in_session(
        &self,
        _session_key: &str,
        long_term_max: usize,
        short_term_max: usize,
    ) -> Result<String> {
        self.generate_brief(long_term_max, short_term_max)
    }
    /// Generate brief filtered by relevance to a query (FTS5 search).
    fn generate_brief_for_query(&self, query: &str, max_items: usize) -> Result<String>;
    fn generate_brief_for_query_in_session(
        &self,
        _session_key: &str,
        query: &str,
        max_items: usize,
    ) -> Result<String> {
        self.generate_brief_for_query(query, max_items)
    }
    /// Upsert a session summary (L2 incremental summary).
    fn upsert_session_summary(&self, session_key: &str, summary: &str) -> Result<()>;
    /// Get session summary for a given session key.
    fn get_session_summary(&self, session_key: &str) -> Result<Option<String>>;
    /// Run maintenance (TTL cleanup, recycle bin purge).
    fn maintenance(&self, recycle_days: i64) -> Result<(usize, usize)>;
}

/// Trait abstracting file-backed USER.md / MEMORY.md operations.
pub trait MemoryFileStoreOps: Send + Sync {
    fn add_file_memory_json(&self, target: &str, content: &str) -> Result<Value>;
    fn replace_file_memory_json(
        &self,
        target: &str,
        old_text: &str,
        content: &str,
    ) -> Result<Value>;
    fn remove_file_memory_json(&self, target: &str, old_text: &str) -> Result<Value>;
    fn restore_latest_file_memory_json(&self, target: &str) -> Result<Value>;

    fn add_scoped_file_memory_json(
        &self,
        _scope: &str,
        target: &str,
        content: &str,
    ) -> Result<Value> {
        self.add_file_memory_json(target, content)
    }

    fn replace_scoped_file_memory_json(
        &self,
        _scope: &str,
        target: &str,
        old_text: &str,
        content: &str,
    ) -> Result<Value> {
        self.replace_file_memory_json(target, old_text, content)
    }

    fn remove_scoped_file_memory_json(
        &self,
        _scope: &str,
        target: &str,
        old_text: &str,
    ) -> Result<Value> {
        self.remove_file_memory_json(target, old_text)
    }

    fn restore_latest_scoped_file_memory_json(&self, _scope: &str, target: &str) -> Result<Value> {
        self.restore_latest_file_memory_json(target)
    }
}

pub trait GhostMemoryLifecycleOps: Send + Sync {
    fn on_memory_write_json(&self, target: &str, action: &str, content: &str) -> Result<Value>;
}

/// Trait abstracting file-backed workspace skill operations.
pub trait SkillFileStoreOps: Send + Sync {
    fn view_skill_json(&self, name: &str) -> Result<Value>;
    fn create_skill_json(&self, name: &str, description: &str, content: &str) -> Result<Value>;
    fn edit_skill_json(&self, name: &str, content: &str) -> Result<Value>;
    fn patch_skill_json(&self, name: &str, old_text: &str, content: &str) -> Result<Value>;
    fn delete_skill_json(&self, name: &str) -> Result<Value>;
    fn write_skill_file_json(&self, name: &str, path: &str, content: &str) -> Result<Value>;
    fn remove_skill_file_json(&self, name: &str, path: &str) -> Result<Value>;
    fn restore_latest_skill_json(&self, name: &str) -> Result<Value>;
}

/// Trait abstracting read-only session search operations.
pub trait SessionSearchOps: Send + Sync {
    fn search_session_json(&self, query: &str, limit: usize) -> Result<Value>;
}

/// Trait abstracting session response cache operations needed by tools.
/// The cache stores large list/table responses and allows retrieval by ref_id.
pub trait ResponseCacheOps: Send + Sync {
    /// Recall a cached response by ref_id. Returns JSON string.
    fn recall_json(&self, session_key: &str, ref_id: &str) -> String;
}

/// Trait abstracting task manager operations needed by tools.
#[async_trait]
pub trait TaskManagerOps: Send + Sync {
    async fn list_tasks_json(&self, status_filter: Option<String>) -> Value;
    async fn get_task_json(&self, task_id: &str) -> Option<Value>;
    async fn summary_json(&self) -> Value;
    /// Send a message to a running agent task's message queue.
    async fn send_message(&self, task_id: &str, message: String) -> Result<()>;
    /// Check if a task is a ONE_SHOT type (cannot receive SendMessage).
    async fn is_one_shot_task(&self, task_id: &str) -> bool;
    /// Register a persistent shell process so runtime shutdown can clean it up.
    fn register_shell_process(&self, session_id: &str, pid: u32);
    /// Remove a shell process registration after the session closes.
    fn unregister_shell_process(&self, session_id: &str);
}

#[derive(Clone)]
pub struct ToolContext {
    pub workspace: PathBuf,
    /// 根 base 目录（如 ~/.blockcell），独立于 workspace_override。
    /// 工具需要基于 base 重建 Paths 时使用此字段，而非从 workspace 反推。
    pub base: PathBuf,
    pub builtin_skills_dir: Option<PathBuf>,
    pub active_skill_dir: Option<PathBuf>,
    pub session_key: String,
    pub channel: String,
    pub account_id: Option<String>,
    /// The user ID who triggered this tool call (sender of the message).
    /// Used for permission checks in admin tools.
    pub sender_id: Option<String>,
    pub chat_id: String,
    pub config: Config,
    pub permissions: PermissionSet,
    pub task_manager: Option<TaskManagerHandle>,
    pub memory_store: Option<MemoryStoreHandle>,
    pub memory_file_store: Option<MemoryFileStoreHandle>,
    pub ghost_memory_lifecycle: Option<Arc<dyn GhostMemoryLifecycleOps + Send + Sync>>,
    pub skill_file_store: Option<SkillFileStoreHandle>,
    pub session_search: Option<SessionSearchHandle>,
    pub outbound_tx: Option<OutboundSender>,
    pub spawn_handle: Option<Arc<dyn SpawnHandle>>,
    pub capability_registry: Option<CapabilityRegistryHandle>,
    pub core_evolution: Option<CoreEvolutionHandle>,
    pub event_emitter: Option<EventEmitterHandle>,
    /// Path to channel_contacts.json for cross-channel contact lookup.
    pub channel_contacts_file: Option<PathBuf>,
    /// Session response cache handle for session_recall tool.
    pub response_cache: Option<ResponseCacheHandle>,
    /// Runtime handle for typed agent execution (agent tool).
    pub runtime_handle: Option<AgentRuntimeHandle>,
    // ===== 新增字段 =====
    /// 当前Agent身份
    pub agent_identity: Option<AgentIdentity>,
    /// Skill mutex handle for checking if a skill is currently active.
    pub skill_mutex: Option<SkillMutexHandle>,
    /// Agent 类型注册表 (用于 AgentTool 动态 schema 和验证)
    pub agent_type_registry: Option<AgentTypeRegistryHandle>,
    /// Evolution workflow store handle for workflow management tools.
    pub evolution_workflow_store: Option<EvolutionWorkflowStoreHandle>,
}

impl ToolContext {
    /// 获取当前Agent身份
    pub fn current_identity(&self) -> Option<&AgentIdentity> {
        self.agent_identity.as_ref()
    }

    /// 检查是否可以spawn子Agent（简化版）
    /// 只检查ForkChild，不查询registry
    /// 无身份信息时默认拒绝（保守策略，防止 ForkChild 逃逸）
    pub fn can_spawn_subagent(&self) -> bool {
        self.agent_identity
            .as_ref()
            .map(|id| id.can_spawn_subagent_basic())
            .unwrap_or(false)
    }
}

pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Context passed to `Tool::prompt_rule()` so each tool can emit channel-aware / intent-aware rules.
pub struct PromptContext<'a> {
    pub channel: &'a str,
    /// Intent category names (e.g. "Finance", "Blockchain", "Chat") resolved by the caller.
    /// Tools can use this to conditionally emit detailed domain-specific guidelines.
    pub intents: &'a [String],
    /// Default timezone from config, if set.
    pub default_timezone: Option<&'a str>,
}

impl<'a> PromptContext<'a> {
    pub fn is_im_channel(&self) -> bool {
        matches!(
            self.channel,
            "wecom"
                | "feishu"
                | "lark"
                | "telegram"
                | "slack"
                | "discord"
                | "dingtalk"
                | "whatsapp"
                | "napcat"
                | "qq"
        )
    }

    pub fn has_intent(&self, name: &str) -> bool {
        self.intents.iter().any(|i| i == name)
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;
    fn validate(&self, params: &Value) -> Result<()>;
    fn required_permissions(&self, _params: &Value) -> PermissionSet {
        PermissionSet::new()
    }
    /// 重复执行同一调用是否安全。Runtime 据此决定失败或结果未知时能否自动重试；
    /// 未声明的工具按有副作用对待。
    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Unknown
    }
    /// Return an optional system-prompt rule describing how the LLM should use this tool.
    /// Each line should be a markdown list item starting with `- `.
    /// Return `None` (default) if the tool needs no special instructions.
    fn prompt_rule(&self, _ctx: &PromptContext) -> Option<String> {
        None
    }
    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value>;
}
