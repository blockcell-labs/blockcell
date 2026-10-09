pub mod abort_token;
pub mod abort_token_context;
pub mod agent_context;
pub mod agent_identity;
pub mod agent_result;
pub mod budget;
pub mod capability;
pub mod config;
pub mod error;
pub mod evolution_boundary;
pub mod file_store;
pub mod logging;
pub mod mcp_config;
pub mod message;
pub mod message_receipt;
pub mod path_policy;
pub mod paths;
pub mod session_key;
pub mod system_event;
pub mod tool_policy;
pub mod types;

pub use abort_token::{AbortToken, CancelledError, CleanupHandle, CleanupRegistry};
pub use abort_token_context::{current_abort_token, scope_abort_token, spawn_with_abort_token};
pub use agent_context::{can_spawn_subagent, current_agent_context, scope_agent_context};
pub use agent_identity::{AgentIdentity, AgentRole};
pub use agent_result::{AgentResult, ContentBlock, FileAction, ResultStatus, UsageMetrics};
pub use budget::{
    BudgetConfig, BudgetExhaustedError, BudgetSnapshot, BudgetTracker, BudgetTrackerHandle,
    ContextWindowSnapshot,
};

pub use capability::{
    CapabilityCost, CapabilityDescriptor, CapabilityLifecycle, CapabilityStatus, CapabilityType,
    PrivilegeLevel, ProviderKind, SurvivalInvariants,
};
pub use config::Config;
pub use config::EvolutionConfig;
pub use error::{Error, Result};
pub use message::{InboundMessage, OutboundMessage};
pub use paths::Paths;
pub use session_key::{
    build_session_key, legacy_session_file_stem, resolve_session_key_from_id, session_file_stem,
    session_id_from_file_stem, session_title_from_id, stable_hash_session_key,
};

/// 每 token 约 4 个字符的粗略估算比例。
///
/// 用于在无法使用 tiktoken 等精确计数工具时，粗略估算文本对应的 token 数。
/// 该值来源于常见的英文 token 密度（~0.75 tokens/word，~3-4 chars/token）。
/// 注意：对于中文、代码等文本，实际密度可能有显著差异。
pub const CHARS_PER_TOKEN: usize = 4;

/// Serde `default` 辅助函数：返回 `true`。
///
/// 在 core crate 内部多个 struct 的 `#[serde(default = "crate::default_true")]` 中使用。
/// 提取为共享函数以避免重复定义（见 [`config::default_true`] 和 [`path_policy::default_true`] 的历史原因）。
#[inline]
pub(crate) fn default_true() -> bool {
    true
}
