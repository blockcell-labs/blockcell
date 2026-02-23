pub mod fs;
pub mod exec;
pub mod web;
pub mod html_to_md;
pub mod message;
pub mod spawn;
pub mod cron;
pub mod office;
pub mod tasks;
pub mod browser;
pub mod memory;
pub mod skills;
pub mod system_info;
pub mod camera;
pub mod chrome_control;
pub mod app_control;
pub mod file_ops;
pub mod data_process;
pub mod http_request;
pub mod email;
pub mod audio_transcribe;
pub mod chart_generate;
pub mod office_write;
pub mod calendar_api;
pub mod iot_control;
pub mod tts;
pub mod ocr;
pub mod image_understand;
pub mod social_media;
pub mod notification;
pub mod cloud_api;
pub mod git_api;
pub mod finance_api;
pub mod video_process;
pub mod health_api;
pub mod map_api;
pub mod contacts;
pub mod encrypt;
pub mod network_monitor;
pub mod knowledge_graph;
pub mod stream_subscribe;
pub mod alert_rule;
pub mod blockchain_rpc;
pub mod exchange_api;
pub mod blockchain_tx;
pub mod contract_security;
pub mod bridge_api;
pub mod nft_market;
pub mod multisig;
pub mod community_hub;
pub mod memory_maintenance;
pub mod toggle_manage;
pub mod termux_api;
pub mod registry;

use async_trait::async_trait;
use blockcell_core::{Config, OutboundMessage, Result};
use blockcell_core::types::PermissionSet;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

pub use registry::ToolRegistry;

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
    fn spawn(&self, task: &str, label: &str, origin_channel: &str, origin_chat_id: &str) -> Result<Value>;
}

/// Opaque handle to the task manager, passed through ToolContext.
/// This avoids a circular dependency between tools and agent crates.
pub type TaskManagerHandle = Arc<dyn TaskManagerOps + Send + Sync>;

/// Opaque handle to the memory store, passed through ToolContext.
pub type MemoryStoreHandle = Arc<dyn MemoryStoreOps + Send + Sync>;

/// Opaque handle to the capability registry, passed through ToolContext.
pub type CapabilityRegistryHandle = Arc<Mutex<dyn CapabilityRegistryOps + Send + Sync>>;

/// Opaque handle to the core evolution engine, passed through ToolContext.
pub type CoreEvolutionHandle = Arc<Mutex<dyn CoreEvolutionOps + Send + Sync>>;

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
    async fn request_capability(&self, capability_id: &str, description: &str, provider_kind_str: &str) -> Result<Value>;
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
    pub session_key: String,
    pub channel: String,
    pub chat_id: String,
    pub config: Config,
    pub permissions: PermissionSet,
    pub task_manager: Option<TaskManagerHandle>,
    pub memory_store: Option<MemoryStoreHandle>,
    pub outbound_tx: Option<OutboundSender>,
    pub spawn_handle: Option<Arc<dyn SpawnHandle>>,
    pub capability_registry: Option<CapabilityRegistryHandle>,
    pub core_evolution: Option<CoreEvolutionHandle>,
}

pub struct ToolSchema {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;
    fn validate(&self, params: &Value) -> Result<()>;
    fn required_permissions(&self, _params: &Value) -> PermissionSet {
        PermissionSet::new()
    }
    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value>;
}
