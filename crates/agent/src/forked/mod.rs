//! Forked Agent 模块 - 后台任务基础设施
//!
//! 提供与父进程共享 Prompt Cache 但状态隔离的子代理执行能力。
//! 用于 Session Memory 提取、自动记忆提取、梦境机制等后台任务。
//!
//! ## 核心概念
//!
//! - **CacheSafeParams**: 保证 Prompt Cache 命中的参数结构
//! - **SubagentOverrides**: 子代理状态隔离配置
//! - **CanUseToolFn**: 工具权限检查函数
//!
//! ## 使用示例
//!
//! ```ignore
//! use blockcell_agent::forked::{run_forked_agent, ForkedAgentParams, create_auto_mem_can_use_tool};
//!
//! let result = run_forked_agent(ForkedAgentParams {
//!     prompt_messages: vec![ChatMessage::user("任务提示")],
//!     cache_safe_params,
//!     can_use_tool: create_auto_mem_can_use_tool(&memory_dir),
//!     query_source: "auto_memory",
//!     fork_label: "auto_memory",
//!     max_turns: Some(5),
//!     skip_transcript: true,
//!     ..Default::default()
//! }).await?;
//! ```

mod agent;
mod cache_safe;
mod can_use_tool;
mod context;

// Only the cache-alignment tests build the initial fork message list directly;
// production goes through `run_forked_agent`. Gating keeps `-D warnings` clean.
#[cfg(test)]
pub(crate) use agent::build_initial_forked_messages;
pub use agent::{
    build_forked_tool_schemas, default_read_only_fork_disallowed_tools, run_forked_agent,
    ForkObservation, ForkedAgentParams, ForkedAgentParamsBuilder, ForkedAgentResult,
    WorkspaceScopeEntry,
};
// UsageMetrics 现在从 blockcell_core 导入，不再本地定义
pub use cache_safe::{
    create_cache_safe_params, create_cache_safe_params_with_tools, get_last_cache_safe_params,
    save_cache_safe_params, CacheSafeParams, ToolDefinition,
};
pub use can_use_tool::{
    build_combined_review_tool_schemas, build_flush_tool_schemas, build_memory_review_tool_schemas,
    build_skill_review_tool_schemas, create_allowlist_can_use_tool, create_auto_mem_can_use_tool,
    create_combined_review_can_use_tool, create_compact_can_use_tool, create_dream_can_use_tool,
    create_flush_can_use_tool, create_memory_file_can_use_tool, create_memory_review_can_use_tool,
    create_skill_review_can_use_tool, roster_allows, roster_deny_message, roster_names,
    tool_names_from_schemas, CanUseToolFn, ReviewToolMode, ToolPermission,
};
pub use context::{
    create_subagent_context, AbortController, ContentReplacementState, FileStateCache,
    QueryTracking, SubagentContext, SubagentOverrides,
};
// AbortToken 从 blockcell_core 导出
pub use blockcell_core::AbortToken;
