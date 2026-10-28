pub mod agent_loader;
pub mod agent_progress;
pub mod agent_prompts;
pub mod agent_types;
pub mod auto_memory;
pub mod bus;
pub mod capability_adapter;
pub mod checkpoint;
pub mod compact;
pub mod context;
pub mod dream_state;
pub(crate) mod error;
pub mod forked;
pub mod fs_util;
pub mod ghost_background_review;
pub mod ghost_learning;
pub mod ghost_memory_provider;
pub mod ghost_metrics;
pub mod ghost_recall;
pub mod health;
pub mod history_projector;
pub mod hooks;
pub mod intent;
pub mod learning_coordinator;
pub mod learning_dedup;
pub mod learning_file_lock;
pub mod learning_throttle;
pub mod memory_adapter;
pub mod memory_file_store;
pub mod memory_system;
pub mod progress;
pub mod prompt_registry;
pub mod prompt_skill_executor;
pub mod response_cache;
pub mod retrieval;
pub mod runtime;
pub mod runtime_kernel;
pub mod session_memory;
pub mod session_metrics;
pub mod skill_decision;
pub mod skill_executor;
pub mod skill_file_store;
pub mod skill_index;
pub mod skill_kernel;
// skill_mutex 模块已移除 — 所有功能由 write_guard::WriteGuard 替代
// WriteGuard 实现了 SkillMutexOps trait，可作为 SkillMutexHandle 传给工具层
pub mod skill_nudge;
pub mod skill_summary;
pub mod steering;
pub mod summary_queue;
pub mod system_event_orchestrator;
pub mod system_event_store;
pub mod task_manager;
pub mod task_notification;
pub(crate) mod token;
pub mod unified_security_scanner;
pub mod write_guard;

pub use agent_loader::{AgentDefinitionLoader, AgentLoadError};
pub use agent_progress::AgentProgress;
pub use agent_prompts::{
    EXPLORE_SYSTEM_PROMPT, GENERAL_SYSTEM_PROMPT, PLAN_SYSTEM_PROMPT, VERIFICATION_SYSTEM_PROMPT,
    VIPER_SYSTEM_PROMPT,
};
pub use agent_types::{
    built_in_agent_types, AgentSource, AgentTypeDefinition, AgentTypeRegistry, PermissionMode,
};
pub use auto_memory::{
    get_memory_dir, get_memory_file_path, should_extract_auto_memory,
    should_extract_auto_memory_with_config, AutoMemoryConfig, AutoMemoryExtractor,
    CrossProcessLock, ExtractionCursor, ExtractionCursorManager, ExtractionParams,
    ExtractionResult, MemoryType,
};
pub use bus::MessageBus;
pub use capability_adapter::{
    CapabilityRegistryAdapter, CoreEvolutionAdapter, EvolutionNotifier, ProviderLLMBridge,
    SkillEvolutionLLMBridge,
};
pub use compact::{
    generate_compact_summary, CompactError, CompactHookRegistry, CompactSummary,
    CompactSummarySection, CompactionRequest, CompactionSummary, ContextCompactor,
    ExtractiveCompactor, ModelSummaryCompactor, PostCompactHook, PreCompactHook, RecoveryBudget,
};
pub use context::{ContextBuilder, InteractionMode};
pub use forked::{
    create_auto_mem_can_use_tool, create_cache_safe_params, create_cache_safe_params_with_tools,
    create_compact_can_use_tool, create_dream_can_use_tool, create_memory_file_can_use_tool,
    create_skill_review_can_use_tool, run_forked_agent, CacheSafeParams, CanUseToolFn,
    ForkedAgentParams, ForkedAgentResult, ToolDefinition, ToolPermission,
};
// UsageMetrics 从 blockcell_core 导出
pub use blockcell_core::UsageMetrics;
pub use ghost_learning::{
    GhostEpisodeSnapshot, GhostLearningBoundary, GhostLearningBoundaryKind, GhostLearningPolicy,
    LearningDecision,
};
pub use ghost_memory_provider::{
    GhostMemoryProvider, GhostMemoryProviderManager, LocalFileGhostMemoryProvider,
};
pub use ghost_metrics::{
    get_ghost_metrics, ghost_metrics_summary, reset_ghost_metrics_for_paths, GhostMetricsSnapshot,
};
pub use health::HealthChecker;
pub use intent::{IntentCategory, IntentClassifier};
pub use learning_coordinator::{
    LearningAction, LearningBoundaryJob, LearningCoordinator, LearningOutputRequest, MemoryTrigger,
    ReviewMode, SkillCandidate, SkillTrigger, UnifiedLearningOutput,
};
pub use learning_dedup::LearningDedup;
pub use learning_throttle::LearningThrottle;
pub use memory_adapter::MemoryStoreAdapter;
pub use memory_file_store::{
    MemoryFileMutation, MemoryFileSnapshot, MemoryFileStore, MemoryFileStoreRouter,
    MemoryFileTarget,
};
pub use memory_system::{
    evaluate_memory_hooks, BackgroundTaskHandle, MemorySystem, MemorySystemConfig,
    MemorySystemState, PostSamplingActions,
};
pub use response_cache::{ResponseCache, ResponseCacheConfig};
pub use runtime::agent_loop::{AgentLoop, DefaultLoop};
pub use runtime::pre_step::{PreStepDecision, PreStepState};
pub use runtime::{create_evolution_deploy_callback, AgentRuntime, ConfirmRequest};
pub use steering::{
    ActiveConversationKey, SteeringChannel, SteeringMessage, SteeringRegistry, SteeringSender,
    SteeringSessionKey,
};
// Re-export RuntimeHandle trait from tools crate for convenience
pub use blockcell_tools::RuntimeHandle;
pub use checkpoint::{CheckpointManager, TaskCheckpoint};
pub use session_memory::{
    get_session_memory_content_for_compact, get_session_memory_dir, get_session_memory_path,
    should_extract_memory, wait_for_session_memory_extraction,
    wait_for_session_memory_extraction_with_timeout, Section, SectionPriority, SessionMemoryConfig,
    SessionMemoryState, DEFAULT_SESSION_MEMORY_TEMPLATE,
};
pub use skill_file_store::{SkillFileMutation, SkillFileStore};
pub use task_manager::{TaskManager, TaskStatus};
pub use unified_security_scanner::{
    scan_learned_memory_content, scan_learned_skill_content, scan_learned_skill_dir,
    UnifiedSecurityScanner,
};
pub use write_guard::{WriteGuard, WriteGuardError, WriteGuardRAII, WriteTarget};

#[cfg(test)]
mod steering_tests {
    #[test]
    fn steering_channel_drains_pending_messages_in_order() {
        let (mut channel, sender) = crate::steering::SteeringChannel::new(4);

        sender
            .try_send(crate::steering::SteeringMessage {
                content: "first".to_string(),
                channel: "ws".to_string(),
                chat_id: "chat-a".to_string(),
            })
            .expect("first steering message should fit");
        sender
            .try_send(crate::steering::SteeringMessage {
                content: "second".to_string(),
                channel: "ws".to_string(),
                chat_id: "chat-a".to_string(),
            })
            .expect("second steering message should fit");

        assert!(channel.has_pending());
        let drained = channel.drain();
        assert_eq!(
            drained
                .iter()
                .map(|msg| msg.content.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert!(!channel.has_pending());
    }
}

mod task_context;
