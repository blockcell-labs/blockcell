pub mod capability_provider;
pub mod capability_versioning;
pub mod core_evolution;
pub mod dispatcher;
pub mod engine;
pub mod evolution;
pub mod manager;
pub mod service;
pub mod versioning;

pub use capability_provider::{
    new_registry_handle, CapabilityExecutor, CapabilityRegistry, CapabilityRegistryHandle,
    ProcessProvider, RegistryStats, ScriptProvider,
};
pub use capability_versioning::{
    CapabilityVersion, CapabilityVersionHistory, CapabilityVersionManager, CapabilityVersionSource,
};
pub use core_evolution::CoreEvolution;
pub use dispatcher::{SkillDispatchResult, SkillDispatcher, ToolCallRecord};
pub use engine::{EngineConfig, ExecutionResult, RhaiEngine, SkillExecutor};
pub use evolution::{EvolutionContext, LLMProvider, SkillEvolution, SkillType, TriggerReason};
pub use manager::{Skill, SkillManager, SkillMeta, SkillTestFixture};
pub use service::{
    is_builtin_tool, CapabilityErrorReport, ErrorReport, EvolutionService, EvolutionServiceConfig,
    SkillRecordSummary,
};
pub use versioning::{SkillVersion, VersionHistory, VersionManager, VersionSource};
