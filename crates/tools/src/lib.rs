pub mod agent_status;
pub mod session_recall;
pub mod alert_rule;
pub mod app_control;
pub mod audio_transcribe;
pub mod browser;
pub mod camera;
pub mod chart_generate;
pub mod community_hub;
pub mod cron;
pub mod data_process;
pub mod email;
pub mod encrypt;
pub mod exec;
pub mod exec_local;
pub mod file_ops;
pub mod fs;
pub mod html_to_md;
pub mod http_request;
pub mod image_understand;
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
pub mod registry;
pub mod registry_builder;
pub mod skills;
pub mod spawn;
pub mod stream_subscribe;
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
use blockcell_core::{Config, OutboundMessage, Result};
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
    if s.len() <= max_chars {
        return s;
    }
    // Find the last valid char boundary at or before max_chars bytes
    let mut end = max_chars;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Sender handle for outbound messages (used by message tool).
pub type OutboundSender = mpsc::Sender<OutboundMessage>;

/// Trait for spawning subagents from tools, breaking the circular dependency
/// between the tools crate and the agent crate.
#[async_trait]
pub trait SpawnHandle: Send + Sync {
    /// Spawn a subagent task. Returns a JSON string with task_id and status.
    fn spawn(
        &self,
        task: &str,
        label: &str,
        origin_channel: &str,
        origin_chat_id: &str,
    ) -> Result<Value>;
}

/// Opaque handle to the task manager, passed through ToolContext.
/// This avoids a circular dependency between tools and agent crates.
pub type TaskManagerHandle = Arc<dyn TaskManagerOps + Send + Sync>;

/// Opaque handle to the memory store, passed through ToolContext.
pub type MemoryStoreHandle = Arc<dyn MemoryStoreOps + Send + Sync>;

/// Opaque handle to the response cache, passed through ToolContext.
pub type ResponseCacheHandle = Arc<dyn ResponseCacheOps + Send + Sync>;

/// Opaque handle to the capability registry, passed through ToolContext.
pub type CapabilityRegistryHandle = Arc<Mutex<dyn CapabilityRegistryOps + Send + Sync>>;

/// Opaque handle to the core evolution engine, passed through ToolContext.
pub type CoreEvolutionHandle = Arc<Mutex<dyn CoreEvolutionOps + Send + Sync>>;

/// Opaque handle to the system event emitter, passed through ToolContext.
pub type EventEmitterHandle = Arc<dyn SystemEventEmitter + Send + Sync>;

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
    /// Process all pending evolutions. Returns number processed.
    async fn run_pending_evolutions(&self) -> Result<usize>;
    /// Unblock a previously blocked capability.
    async fn unblock_capability(&self, capability_id: &str) -> Result<Value>;
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
    /// Batch soft-delete by filter. Returns count of deleted items.
    fn batch_soft_delete_json(&self, params_json: Value) -> Result<usize>;
    /// Restore a soft-deleted item. Returns success boolean.
    fn restore(&self, id: &str) -> Result<bool>;
    /// Get memory stats as JSON.
    fn stats_json(&self) -> Result<Value>;
    /// Generate brief for prompt injection.
    fn generate_brief(&self, long_term_max: usize, short_term_max: usize) -> Result<String>;
    /// Generate brief filtered by relevance to a query (FTS5 search).
    fn generate_brief_for_query(&self, query: &str, max_items: usize) -> Result<String>;
    /// Upsert a session summary (L2 incremental summary).
    fn upsert_session_summary(&self, session_key: &str, summary: &str) -> Result<()>;
    /// Get session summary for a given session key.
    fn get_session_summary(&self, session_key: &str) -> Result<Option<String>>;
    /// Run maintenance (TTL cleanup, recycle bin purge).
    fn maintenance(&self, recycle_days: i64) -> Result<(usize, usize)>;
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
}

#[derive(Clone)]
pub struct ToolContext {
    pub workspace: PathBuf,
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
    pub outbound_tx: Option<OutboundSender>,
    pub spawn_handle: Option<Arc<dyn SpawnHandle>>,
    pub capability_registry: Option<CapabilityRegistryHandle>,
    pub core_evolution: Option<CoreEvolutionHandle>,
    pub event_emitter: Option<EventEmitterHandle>,
    /// Path to channel_contacts.json for cross-channel contact lookup.
    pub channel_contacts_file: Option<PathBuf>,
    /// Session response cache handle for session_recall tool.
    pub response_cache: Option<ResponseCacheHandle>,
}

pub struct ToolSchema {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

/// Context passed to `Tool::prompt_rule()` so each tool can emit channel-aware / intent-aware rules.
pub struct PromptContext<'a> {
    pub channel: &'a str,
    /// Intent category names (e.g. "Finance", "Blockchain", "Chat") resolved by the caller.
    /// Tools can use this to conditionally emit detailed domain-specific guidelines.
    pub intents: &'a [String],
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
    /// Return an optional system-prompt rule describing how the LLM should use this tool.
    /// Each line should be a markdown list item starting with `- `.
    /// Return `None` (default) if the tool needs no special instructions.
    fn prompt_rule(&self, _ctx: &PromptContext) -> Option<String> {
        None
    }
    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value>;
}
