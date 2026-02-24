pub mod capability_provider;
pub mod capability_versioning;
pub mod core_evolution;
pub mod dispatcher;
pub mod engine;
pub mod manager;
pub mod evolution;
pub mod versioning;
pub mod service;

pub use capability_provider::{
    CapabilityExecutor, CapabilityRegistry, CapabilityRegistryHandle,
    ProcessProvider, ScriptProvider, RegistryStats,
    new_registry_handle,
};
pub use core_evolution::CoreEvolution;
pub use dispatcher::{SkillDispatcher, SkillDispatchResult, ToolCallRecord};
pub use engine::{EngineConfig, RhaiEngine, SkillExecutor, ExecutionResult};
pub use manager::{SkillManager, Skill, SkillMeta, SkillTestFixture};
pub use evolution::{SkillEvolution, EvolutionContext, TriggerReason, LLMProvider};
pub use versioning::{VersionManager, SkillVersion, VersionSource, VersionHistory};
pub use service::{EvolutionService, EvolutionServiceConfig, ErrorReport, CapabilityErrorReport, SkillRecordSummary, is_builtin_tool};
pub use capability_versioning::{CapabilityVersionManager, CapabilityVersion, CapabilityVersionSource, CapabilityVersionHistory};
