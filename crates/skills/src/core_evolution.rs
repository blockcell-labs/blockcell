use crate::capability_provider::{
    CapabilityExecutor, CapabilityRegistryHandle, ProcessProvider, ScriptProvider,
};
use crate::capability_versioning::{CapabilityVersionManager, CapabilityVersionSource};
use crate::evolution::LLMProvider;
use blockcell_core::evolution_boundary::ensure_single_path_component;
use blockcell_core::{
    CapabilityDescriptor, CapabilityStatus, CapabilityType, Error, PrivilegeLevel, ProviderKind,
    Result,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, info, warn};

static CORE_RECORD_TMP_COUNTER: AtomicU64 = AtomicU64::new(1);

fn default_max_attempts() -> u32 {
    3
}

/// Core-level evolution record — tracks the lifecycle of a capability evolution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreEvolutionRecord {
    pub id: String,
    pub capability_id: String,
    pub description: String,
    pub status: CoreEvolutionStatus,
    pub provider_kind: ProviderKind,
    /// Generated source code (Rust, Python, shell script, etc.)
    pub source_code: Option<String>,
    /// Path to compiled artifact (.dylib, .so, script, etc.)
    pub artifact_path: Option<String>,
    /// Compilation output / errors
    pub compile_output: Option<String>,
    /// Validation results
    pub validation: Option<ValidationResult>,
    /// Attempt count
    pub attempt: u32,
    /// Maximum retry attempts
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// Feedback history for retries
    pub feedback_history: Vec<CoreFeedbackEntry>,
    /// Input schema (JSON Schema) extracted from LLM response
    #[serde(default)]
    pub input_schema: Option<serde_json::Value>,
    /// Output schema (JSON Schema) extracted from LLM response
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CoreEvolutionStatus {
    /// 需求已识别
    Requested,
    /// 正在生成代码
    Generating,
    /// 代码已生成
    Generated,
    /// 正在编译
    Compiling,
    /// 编译成功
    Compiled,
    /// 编译失败
    CompileFailed,
    /// 正在验证
    Validating,
    /// 验证通过
    Validated,
    /// 验证失败
    ValidationFailed,
    /// 正在加载
    Loading,
    /// 已激活
    Active,
    /// 失败
    Failed,
    /// 被阻止（连续失败过多，需人工介入）
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreFeedbackEntry {
    pub attempt: u32,
    pub stage: String,
    pub feedback: String,
    pub previous_code: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub passed: bool,
    pub checks: Vec<ValidationCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationCheck {
    pub name: String,
    pub passed: bool,
    pub message: String,
}

/// Maximum consecutive failures before a capability is blocked from auto-triggering.
const MAX_AUTO_FAILURES: u32 = 3;

/// Blocked records auto-expire after this many seconds (7 days).
const BLOCK_EXPIRY_SECS: i64 = 7 * 24 * 3600;

/// Evolution step names — used by the workflow worker to advance one step at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvolutionStep {
    BuildPrompt,
    GenerateCode,
    CompileArtifact,
    ValidateArtifact,
    LoadCapability,
    Promote,
}

impl EvolutionStep {
    /// All steps in execution order.
    pub fn all_steps() -> &'static [EvolutionStep] {
        &[
            EvolutionStep::BuildPrompt,
            EvolutionStep::GenerateCode,
            EvolutionStep::CompileArtifact,
            EvolutionStep::ValidateArtifact,
            EvolutionStep::LoadCapability,
            EvolutionStep::Promote,
        ]
    }

    /// Step name as stored in the workflow steps table.
    pub fn name(&self) -> &'static str {
        match self {
            EvolutionStep::BuildPrompt => "build_prompt",
            EvolutionStep::GenerateCode => "generate_code",
            EvolutionStep::CompileArtifact => "compile_artifact",
            EvolutionStep::ValidateArtifact => "validate_artifact",
            EvolutionStep::LoadCapability => "load_capability",
            EvolutionStep::Promote => "promote",
        }
    }

    /// Find the next step after the given completed step name.
    /// Returns None if this was the last step.
    pub fn next_after(completed: &str) -> Option<EvolutionStep> {
        let steps = Self::all_steps();
        for (i, s) in steps.iter().enumerate() {
            if s.name() == completed && i + 1 < steps.len() {
                return Some(steps[i + 1]);
            }
        }
        None
    }

    /// Find the first step.
    pub fn first() -> EvolutionStep {
        EvolutionStep::BuildPrompt
    }
}

/// Core-level evolution engine
///
/// Unlike the Rhai skill evolution which generates scripts, this generates
/// actual executable capabilities:
/// - Python/Shell scripts → validate → register as ScriptProvider
/// - Process commands → validate binary exists → register as ProcessProvider
pub struct CoreEvolution {
    /// Directory for evolution artifacts
    artifacts_dir: PathBuf,
    /// Directory for evolution records
    records_dir: PathBuf,
    /// Reference to the capability registry for hot-loading
    registry: CapabilityRegistryHandle,
    /// Version manager for capability artifact snapshots
    version_manager: CapabilityVersionManager,
    /// Max retries for generation
    max_retries: u32,
    /// Optional LLM provider for autonomous evolution
    llm_provider: Option<Arc<dyn LLMProvider>>,
    /// LLM call timeout in seconds
    llm_timeout_secs: u64,
    /// Serialize `request_capability` check-then-act so concurrent requests for the
    /// same capability cannot bypass idempotency or collide on record filenames.
    request_lock: tokio::sync::Mutex<()>,
    /// Per-record active pipeline set so concurrent callers (`EvolutionWorker`,
    /// `run_one_pending_evolution`, `run_pending_evolutions`) never double-run the
    /// same `evolution_id`.
    pipeline_locks: Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
}

struct PipelineLockGuard {
    locks: Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
    evolution_id: String,
}

impl Drop for PipelineLockGuard {
    fn drop(&mut self) {
        if let Ok(mut locks) = self.locks.try_lock() {
            locks.remove(&self.evolution_id);
        } else {
            let locks = Arc::clone(&self.locks);
            let evolution_id = std::mem::take(&mut self.evolution_id);
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    locks.lock().await.remove(&evolution_id);
                });
            }
        }
    }
}

mod generation;

impl CoreEvolution {
    /// Evolved artifact names must stay inside `artifacts_dir`. Returns the artifact-safe capability id.
    fn check_evolution_boundary(capability_id: &str) -> Result<String> {
        let safe_id = capability_id.replace('.', "_");
        ensure_single_path_component(&safe_id)?;
        Ok(safe_id)
    }

    /// Artifact and version paths use `.` → `_`, so `a.b` and `a_b` would share files.
    /// Refuse a new capability whose artifact name is already owned by a different capability.
    async fn ensure_artifact_name_available(&self, capability_id: &str) -> Result<()> {
        let safe_id = Self::check_evolution_boundary(capability_id)?;
        let collides = |other: &str| other != capability_id && other.replace('.', "_") == safe_id;
        let registered = self
            .registry
            .lock()
            .await
            .list_all()
            .iter()
            .any(|descriptor| collides(&descriptor.id));
        if registered
            || self
                .list_records()?
                .iter()
                .any(|record| collides(&record.capability_id))
        {
            return Err(Error::Validation(format!(
                "capability '{capability_id}' would share artifact name '{safe_id}' with an existing capability"
            )));
        }
        Ok(())
    }

    fn audit_generated_code(provider_kind: &ProviderKind, code: &str) -> Result<()> {
        let skill_type = match provider_kind {
            ProviderKind::ExternalApi => crate::evolution::SkillType::Python,
            ProviderKind::RhaiScript => crate::evolution::SkillType::Rhai,
            _ => crate::evolution::SkillType::LocalScript,
        };
        let result = crate::audit::static_audit(&skill_type, code);
        if result.passed {
            return Ok(());
        }
        Err(Error::Evolution(
            crate::audit::format_static_audit_feedback(&result),
        ))
    }

    pub fn new(
        base_dir: PathBuf,
        registry: CapabilityRegistryHandle,
        llm_timeout_secs: u64,
    ) -> Self {
        let artifacts_dir = base_dir.join("tool_artifacts");
        let records_dir = base_dir.join("tool_evolution_records");
        let version_manager = CapabilityVersionManager::new(base_dir);
        Self {
            artifacts_dir,
            records_dir,
            registry,
            version_manager,
            max_retries: 3,
            llm_provider: None,
            llm_timeout_secs,
            request_lock: tokio::sync::Mutex::new(()),
            pipeline_locks: Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new())),
        }
    }

    async fn try_acquire_pipeline_lock(&self, evolution_id: &str) -> Option<PipelineLockGuard> {
        let mut locks = self.pipeline_locks.lock().await;
        if locks.contains(evolution_id) {
            return None;
        }
        locks.insert(evolution_id.to_string());
        Some(PipelineLockGuard {
            locks: Arc::clone(&self.pipeline_locks),
            evolution_id: evolution_id.to_string(),
        })
    }

    /// Get a reference to the capability version manager.
    pub fn version_manager(&self) -> &CapabilityVersionManager {
        &self.version_manager
    }

    /// Set the LLM provider for autonomous evolution.
    pub fn set_llm_provider(&mut self, provider: Arc<dyn LLMProvider>) {
        self.llm_provider = Some(provider);
    }

    /// 处理单个待处理（Requested）进化请求，返回处理数量（0 或 1）。
    ///
    /// 与 `run_pending_evolutions`（处理所有待处理请求，可能阻塞事件循环数分钟）不同，
    /// 此方法每次调用只处理 1 个请求，保持每个 tick 轻量且响应迅速。
    pub async fn run_one_pending_evolution(&self) -> Result<usize> {
        let provider = match &self.llm_provider {
            Some(p) => p.clone(),
            None => {
                debug!("🧬 [核心进化] 无 LLM provider，跳过待处理进化");
                return Ok(0);
            }
        };

        let records = self.list_records()?;
        let pending: Vec<_> = records
            .iter()
            .filter(|r| r.status == CoreEvolutionStatus::Requested)
            .collect();

        if pending.is_empty() {
            return Ok(0);
        }

        // 只处理第一个待处理进化
        let record = &pending[0];
        info!(
            remaining = pending.len(),
            "🧬 [核心进化] 发现 {} 个待处理请求，本次处理 1 个",
            pending.len()
        );

        match self.run_evolution(&record.id, provider.as_ref()).await {
            Ok(success) => {
                if success {
                    info!(id = %record.id, "🧬 [核心进化] ✅ 进化成功: {}", record.capability_id);
                } else {
                    warn!(id = %record.id, "🧬 [核心进化] ❌ 进化失败: {}", record.capability_id);
                }
                Ok(1)
            }
            Err(e) => {
                warn!(id = %record.id, error = %e, "🧬 [核心进化] 进化出错: {}", record.capability_id);
                Ok(0)
            }
        }
    }

    /// Process all pending (Requested) evolutions using the configured LLM provider.
    /// Returns the number of evolutions processed.
    pub async fn run_pending_evolutions(&self) -> Result<usize> {
        let provider = match &self.llm_provider {
            Some(p) => p.clone(),
            None => {
                debug!("🧬 [核心进化] 无 LLM provider，跳过待处理进化");
                return Ok(0);
            }
        };

        let records = self.list_records()?;
        let pending: Vec<_> = records
            .iter()
            .filter(|r| r.status == CoreEvolutionStatus::Requested)
            .collect();

        if pending.is_empty() {
            return Ok(0);
        }

        info!(
            count = pending.len(),
            "🧬 [核心进化] 发现 {} 个待处理的能力进化请求",
            pending.len()
        );

        let mut processed = 0;
        for record in pending {
            match self.run_evolution(&record.id, provider.as_ref()).await {
                Ok(success) => {
                    if success {
                        info!(id = %record.id, "🧬 [核心进化] ✅ 进化成功: {}", record.capability_id);
                    } else {
                        warn!(id = %record.id, "🧬 [核心进化] ❌ 进化失败: {}", record.capability_id);
                    }
                    processed += 1;
                }
                Err(e) => {
                    warn!(id = %record.id, error = %e, "🧬 [核心进化] 进化出错: {}", record.capability_id);
                }
            }
        }

        Ok(processed)
    }

    /// Run a single evolution step for a workflow.
    ///
    /// This is the Phase 2 step-by-step interface used by the EvolutionWorker.
    /// Each call advances the workflow by one step, writing progress to the
    /// CoreEvolutionRecord (JSON file). The workflow store step table is
    /// updated by the caller (EvolutionWorker).
    ///
    /// The `evolution_id` is the workflow store's UUID, which is also used
    /// as the CoreEvolutionRecord ID for traceability.
    ///
    /// Returns `Ok(step_output_json)` on success, `Err` on failure.
    pub async fn run_step(&self, evolution_id: &str, step: EvolutionStep) -> Result<String> {
        let Some(_pipeline_guard) = self.try_acquire_pipeline_lock(evolution_id).await else {
            return Err(Error::Evolution(format!(
                "Core evolution '{}' is already running in another task",
                evolution_id
            )));
        };

        // For BuildPrompt, ensure the CoreEvolutionRecord exists.
        // If the record doesn't exist yet, create it.
        if step == EvolutionStep::BuildPrompt && self.load_record(evolution_id).is_err() {
            // Record doesn't exist — we need to create it.
            // The workflow store's enqueue() already has capability_id and description,
            // but CoreEvolutionRecord needs them. We'll create a minimal record.
            // The worker will pass these via a separate method.
            warn!(
                evolution_id = %evolution_id,
                "CoreEvolutionRecord does not exist for BuildPrompt step. \
                 Use run_step_with_context() instead."
            );
            return Err(Error::Evolution(
                "CoreEvolutionRecord not found. Use run_step_with_context().".to_string(),
            ));
        }

        let mut record = self.load_record(evolution_id)?;

        match step {
            EvolutionStep::BuildPrompt => {
                // Build the generation prompt and store it as step output.
                // This step is pure computation — no LLM call.
                info!(
                    evolution_id = %evolution_id,
                    "🧬 [核心进化] Step: build_prompt"
                );
                record.status = CoreEvolutionStatus::Generating;
                record.updated_at = chrono::Utc::now().timestamp();
                self.save_record(&record)?;

                let prompt = self.build_generation_prompt(&record)?;
                Ok(serde_json::json!({ "prompt_len": prompt.len() }).to_string())
            }

            EvolutionStep::GenerateCode => {
                // Call LLM to generate code.
                let provider = self
                    .llm_provider
                    .as_ref()
                    .ok_or_else(|| Error::Evolution("No LLM provider configured".to_string()))?;

                info!(
                    evolution_id = %evolution_id,
                    "🧬 [核心进化] Step: generate_code"
                );
                record.status = CoreEvolutionStatus::Generating;
                record.updated_at = chrono::Utc::now().timestamp();
                self.save_record(&record)?;

                let (code, raw_response) = self.generate_code(&record, provider.as_ref()).await?;
                record.source_code = Some(code.clone());

                // Extract input/output schema from the raw LLM response
                if let Some((input_schema, output_schema)) =
                    self.extract_schema_from_response(&raw_response)
                {
                    record.input_schema = Some(input_schema);
                    record.output_schema = Some(output_schema);
                }

                record.status = CoreEvolutionStatus::Generated;
                record.updated_at = chrono::Utc::now().timestamp();
                self.save_record(&record)?;

                Ok(serde_json::json!({ "code_len": code.len() }).to_string())
            }

            EvolutionStep::CompileArtifact => {
                info!(
                    evolution_id = %evolution_id,
                    "🧬 [核心进化] Step: compile_artifact"
                );
                record.status = CoreEvolutionStatus::Compiling;
                record.updated_at = chrono::Utc::now().timestamp();
                self.save_record(&record)?;

                let artifact_path = self.compile_artifact(&record).await?;
                record.artifact_path = Some(artifact_path);
                record.compile_output = Some("Success".to_string());
                record.status = CoreEvolutionStatus::Compiled;
                record.updated_at = chrono::Utc::now().timestamp();
                self.save_record(&record)?;

                Ok(serde_json::json!({ "artifact_path": record.artifact_path }).to_string())
            }

            EvolutionStep::ValidateArtifact => {
                info!(
                    evolution_id = %evolution_id,
                    "🧬 [核心进化] Step: validate_artifact"
                );
                record.status = CoreEvolutionStatus::Validating;
                record.updated_at = chrono::Utc::now().timestamp();
                self.save_record(&record)?;

                let validation = self.validate_artifact(&record).await?;
                let passed = validation.passed;
                record.validation = Some(validation.clone());

                if !passed {
                    let issues: Vec<String> = validation
                        .checks
                        .iter()
                        .filter(|c| !c.passed)
                        .map(|c| format!("[{}] {}", c.name, c.message))
                        .collect();
                    let feedback_msg = format!("Validation failed:\n{}", issues.join("\n"));

                    record.status = CoreEvolutionStatus::ValidationFailed;
                    record.feedback_history.push(CoreFeedbackEntry {
                        attempt: record.attempt.max(1),
                        stage: "validation".to_string(),
                        feedback: feedback_msg,
                        previous_code: record.source_code.clone().unwrap_or_default(),
                        timestamp: chrono::Utc::now().timestamp(),
                    });
                    record.updated_at = chrono::Utc::now().timestamp();
                    self.save_record(&record)?;

                    return Err(Error::Evolution("Validation failed".to_string()));
                }

                record.status = CoreEvolutionStatus::Validated;
                record.updated_at = chrono::Utc::now().timestamp();
                self.save_record(&record)?;

                Ok(serde_json::json!({ "passed": true }).to_string())
            }

            EvolutionStep::LoadCapability => {
                info!(
                    evolution_id = %evolution_id,
                    "🧬 [核心进化] Step: load_capability"
                );
                record.status = CoreEvolutionStatus::Loading;
                record.updated_at = chrono::Utc::now().timestamp();
                self.save_record(&record)?;

                self.load_capability(&record).await?;

                record.status = CoreEvolutionStatus::Active;
                record.updated_at = chrono::Utc::now().timestamp();
                self.save_record(&record)?;

                Ok(serde_json::json!({ "capability_id": record.capability_id }).to_string())
            }

            EvolutionStep::Promote => {
                // Promote is already done in load_capability (sets Active status).
                // This step exists for audit/logging and future canary logic.
                info!(
                    evolution_id = %evolution_id,
                    capability_id = %record.capability_id,
                    "🧬 [核心进化] Step: promote — capability activated"
                );
                Ok(serde_json::json!({ "status": "Active" }).to_string())
            }
        }
    }

    /// Run a single evolution step with workflow context.
    ///
    /// This variant is used by the EvolutionWorker for the BuildPrompt step,
    /// where the CoreEvolutionRecord doesn't exist yet and needs to be created
    /// from the workflow store's metadata.
    pub async fn run_step_with_context(
        &self,
        evolution_id: &str,
        step: EvolutionStep,
        capability_id: &str,
        description: &str,
        provider_kind: ProviderKind,
    ) -> Result<String> {
        self.ensure_artifact_name_available(capability_id).await?;
        // For BuildPrompt, create the CoreEvolutionRecord if it doesn't exist
        if step == EvolutionStep::BuildPrompt && self.load_record(evolution_id).is_err() {
            let now = chrono::Utc::now().timestamp();
            let record = CoreEvolutionRecord {
                id: evolution_id.to_string(),
                capability_id: capability_id.to_string(),
                description: description.to_string(),
                provider_kind,
                status: CoreEvolutionStatus::Requested,
                attempt: 0,
                max_attempts: 3,
                source_code: None,
                artifact_path: None,
                compile_output: None,
                validation: None,
                feedback_history: Vec::new(),
                input_schema: None,
                output_schema: None,
                created_at: now,
                updated_at: now,
            };
            self.save_record(&record)?;
        }

        // Delegate to run_step
        self.run_step(evolution_id, step).await
    }

    /// Check if there is already an active (non-terminal) evolution record for a capability.
    /// Returns the existing evolution_id if found.
    pub fn find_active_record(&self, capability_id: &str) -> Result<Option<String>> {
        let records = self.list_records()?;
        for r in &records {
            if r.capability_id == capability_id {
                match r.status {
                    CoreEvolutionStatus::Requested
                    | CoreEvolutionStatus::Generating
                    | CoreEvolutionStatus::Generated
                    | CoreEvolutionStatus::Compiling
                    | CoreEvolutionStatus::Compiled
                    | CoreEvolutionStatus::Validating
                    | CoreEvolutionStatus::Validated
                    | CoreEvolutionStatus::Loading => {
                        return Ok(Some(r.id.clone()));
                    }
                    _ => {}
                }
            }
        }
        Ok(None)
    }

    /// Check if a capability has been blocked (too many consecutive failures).
    /// Blocked records auto-expire after 7 days (time decay).
    pub fn is_blocked(&self, capability_id: &str) -> Result<bool> {
        let now = chrono::Utc::now().timestamp();
        let records = self.list_records()?;
        for r in &records {
            if r.capability_id == capability_id && r.status == CoreEvolutionStatus::Blocked {
                // 时间衰减：超过 BLOCK_EXPIRY_SECS 后自动解除阻塞
                if now - r.updated_at > BLOCK_EXPIRY_SECS {
                    info!(
                        capability_id = %capability_id,
                        blocked_days = (now - r.updated_at) / 86400,
                        "🧬 [核心进化] 能力 '{}' 阻塞已过期（超过7天），自动解除",
                        capability_id
                    );
                    // 将过期的 Blocked 记录标记为 Failed（不再阻塞）
                    let mut expired = r.clone();
                    expired.status = CoreEvolutionStatus::Failed;
                    expired.updated_at = now;
                    if let Err(e) = self.save_record(&expired) {
                        tracing::warn!(
                            capability_id = %capability_id,
                            error = %e,
                            "[核心进化] 保存过期 Blocked 记录失败，该记录可能仍阻塞进化"
                        );
                    }
                    continue;
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 手动解除能力阻塞（人工干预接口）
    ///
    /// 将所有 Blocked 状态的记录标记为 Failed，允许重新触发进化。
    /// 返回解除阻塞的记录数量。
    pub fn unblock_capability(&self, capability_id: &str) -> Result<u32> {
        let now = chrono::Utc::now().timestamp();
        let records = self.list_records()?;
        let mut unblocked = 0u32;
        for r in &records {
            if r.capability_id == capability_id && r.status == CoreEvolutionStatus::Blocked {
                let mut updated = r.clone();
                updated.status = CoreEvolutionStatus::Failed;
                updated.updated_at = now;
                self.save_record(&updated)?;
                unblocked += 1;
                info!(
                    capability_id = %capability_id,
                    record_id = %r.id,
                    "🧬 [核心进化] 手动解除能力 '{}' 的阻塞",
                    capability_id
                );
            }
        }
        Ok(unblocked)
    }

    /// Count consecutive failures for a capability (most recent first).
    fn count_consecutive_failures(&self, capability_id: &str) -> Result<u32> {
        let records = self.list_records()?; // sorted by created_at desc
        let mut count = 0u32;
        for r in &records {
            if r.capability_id == capability_id {
                match r.status {
                    CoreEvolutionStatus::Failed => count += 1,
                    CoreEvolutionStatus::Active => break, // last success stops counting
                    CoreEvolutionStatus::Blocked => return Ok(MAX_AUTO_FAILURES + 1),
                    _ => {} // in-progress records — skip
                }
            }
        }
        Ok(count)
    }

    /// Request evolution of a new capability.
    ///
    /// **Idempotent**: if an active (non-terminal) record already exists for the same
    /// `capability_id`, returns the existing `evolution_id` without creating a duplicate.
    /// **Blocked**: if the capability has failed >= MAX_AUTO_FAILURES times consecutively,
    /// the request is blocked and returns an error.
    pub async fn request_capability(
        &self,
        capability_id: &str,
        description: &str,
        provider_kind: ProviderKind,
    ) -> Result<String> {
        let _request_guard = self.request_lock.lock().await;
        self.ensure_artifact_name_available(capability_id).await?;
        // Idempotency: return existing active record if present
        if let Some(existing_id) = self.find_active_record(capability_id)? {
            debug!(
                capability_id = %capability_id,
                evolution_id = %existing_id,
                "🧬 [核心进化] 幂等: 已有活跃记录，跳过重复请求"
            );
            return Ok(existing_id);
        }

        // Blocked check: too many consecutive failures
        if self.is_blocked(capability_id)? {
            return Err(Error::Evolution(format!(
                "Capability '{}' is blocked due to repeated failures. Use capability_evolve to unblock.",
                capability_id
            )));
        }

        let consecutive_failures = self.count_consecutive_failures(capability_id)?;
        if consecutive_failures >= MAX_AUTO_FAILURES {
            // Auto-block: create a Blocked record
            let seq = CORE_RECORD_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let block_id = format!(
                "core_evo_{}_{}_{}",
                capability_id.replace('.', "_"),
                chrono::Utc::now().timestamp(),
                seq
            );
            let block_record = CoreEvolutionRecord {
                id: block_id.clone(),
                capability_id: capability_id.to_string(),
                description: format!("BLOCKED: {} consecutive failures", consecutive_failures),
                status: CoreEvolutionStatus::Blocked,
                provider_kind: provider_kind.clone(),
                source_code: None,
                artifact_path: None,
                compile_output: None,
                validation: None,
                attempt: 0,
                max_attempts: 3,
                feedback_history: Vec::new(),
                input_schema: None,
                output_schema: None,
                created_at: chrono::Utc::now().timestamp(),
                updated_at: chrono::Utc::now().timestamp(),
            };
            self.save_record(&block_record)?;
            warn!(
                capability_id = %capability_id,
                failures = consecutive_failures,
                "🧬 [核心进化] ⛔ 能力 '{}' 已被阻止: 连续失败 {} 次，需人工介入",
                capability_id, consecutive_failures
            );
            return Err(Error::Evolution(format!(
                "Capability '{}' blocked after {} consecutive failures. Manual intervention required.",
                capability_id, consecutive_failures
            )));
        }

        let seq = CORE_RECORD_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let evolution_id = format!(
            "core_evo_{}_{}_{}",
            capability_id.replace('.', "_"),
            chrono::Utc::now().timestamp(),
            seq
        );

        let record = CoreEvolutionRecord {
            id: evolution_id.clone(),
            capability_id: capability_id.to_string(),
            description: description.to_string(),
            status: CoreEvolutionStatus::Requested,
            provider_kind,
            source_code: None,
            artifact_path: None,
            compile_output: None,
            validation: None,
            attempt: 0,
            max_attempts: 3,
            feedback_history: Vec::new(),
            input_schema: None,
            output_schema: None,
            created_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
        };

        self.save_record(&record)?;

        info!(
            evolution_id = %evolution_id,
            capability_id = %capability_id,
            "🧬 [核心进化] 请求新能力: {} — {}",
            capability_id, description
        );

        Ok(evolution_id)
    }

    // unblock_capability is defined above with time-decay support

    /// Run the full evolution pipeline for a capability
    pub async fn run_evolution(
        &self,
        evolution_id: &str,
        llm_provider: &dyn LLMProvider,
    ) -> Result<bool> {
        let Some(_pipeline_guard) = self.try_acquire_pipeline_lock(evolution_id).await else {
            info!(
                evolution_id = %evolution_id,
                "🧬 [核心进化] 进化任务已在其他协程运行中，跳过重复执行"
            );
            return Ok(false);
        };

        let mut record = self.load_record(evolution_id)?;

        info!(
            evolution_id = %evolution_id,
            capability_id = %record.capability_id,
            "🧬 [核心进化] 开始进化流程: {}",
            record.capability_id
        );

        for attempt in 1..=self.max_retries {
            record.attempt = attempt;
            record.updated_at = chrono::Utc::now().timestamp();

            // Step 1: Generate code
            info!(
                evolution_id = %evolution_id,
                attempt = attempt,
                "🧬 [核心进化] Step 1: 生成代码 (尝试 {}/{})",
                attempt, self.max_retries
            );

            record.status = CoreEvolutionStatus::Generating;
            self.save_record(&record)?;

            let (code, raw_response) = self.generate_code(&record, llm_provider).await?;
            record.source_code = Some(code.clone());
            // Extract input/output schema from the raw LLM response
            if let Some((input_schema, output_schema)) =
                self.extract_schema_from_response(&raw_response)
            {
                record.input_schema = Some(input_schema);
                record.output_schema = Some(output_schema);
            }
            record.status = CoreEvolutionStatus::Generated;
            self.save_record(&record)?;

            // Step 2: Compile / prepare artifact
            info!(
                evolution_id = %evolution_id,
                "🧬 [核心进化] Step 2: 编译/准备"
            );

            record.status = CoreEvolutionStatus::Compiling;
            self.save_record(&record)?;

            match self.compile_artifact(&record).await {
                Ok(artifact_path) => {
                    record.artifact_path = Some(artifact_path);
                    record.status = CoreEvolutionStatus::Compiled;
                    record.compile_output = Some("Success".to_string());
                    self.save_record(&record)?;
                    info!(evolution_id = %evolution_id, "🧬 [核心进化] ✅ 编译成功");
                }
                Err(e) => {
                    let error_msg = format!("{}", e);
                    warn!(
                        evolution_id = %evolution_id,
                        error = %error_msg,
                        "🧬 [核心进化] ❌ 编译失败"
                    );
                    record.status = CoreEvolutionStatus::CompileFailed;
                    record.compile_output = Some(error_msg.clone());
                    record.feedback_history.push(CoreFeedbackEntry {
                        attempt,
                        stage: "compile".to_string(),
                        feedback: error_msg,
                        previous_code: code,
                        timestamp: chrono::Utc::now().timestamp(),
                    });
                    self.save_record(&record)?;
                    continue;
                }
            }

            // Step 3: Validate
            info!(
                evolution_id = %evolution_id,
                "🧬 [核心进化] Step 3: 验证"
            );

            record.status = CoreEvolutionStatus::Validating;
            self.save_record(&record)?;

            let validation = self.validate_artifact(&record).await?;
            record.validation = Some(validation.clone());

            if !validation.passed {
                let issues: Vec<String> = validation
                    .checks
                    .iter()
                    .filter(|c| !c.passed)
                    .map(|c| format!("[{}] {}", c.name, c.message))
                    .collect();
                let feedback_msg = format!("Validation failed:\n{}", issues.join("\n"));
                warn!(
                    evolution_id = %evolution_id,
                    "🧬 [核心进化] ❌ 验证失败: {}",
                    feedback_msg
                );
                record.status = CoreEvolutionStatus::ValidationFailed;
                record.feedback_history.push(CoreFeedbackEntry {
                    attempt,
                    stage: "validation".to_string(),
                    feedback: feedback_msg,
                    previous_code: record.source_code.clone().unwrap_or_default(),
                    timestamp: chrono::Utc::now().timestamp(),
                });
                self.save_record(&record)?;
                continue;
            }

            record.status = CoreEvolutionStatus::Validated;
            self.save_record(&record)?;
            info!(evolution_id = %evolution_id, "🧬 [核心进化] ✅ 验证通过");

            // Step 4: Load into registry
            info!(
                evolution_id = %evolution_id,
                "🧬 [核心进化] Step 4: 加载到能力注册表"
            );

            record.status = CoreEvolutionStatus::Loading;
            self.save_record(&record)?;

            self.load_capability(&record).await?;

            record.status = CoreEvolutionStatus::Active;
            record.updated_at = chrono::Utc::now().timestamp();
            self.save_record(&record)?;

            info!(
                evolution_id = %evolution_id,
                capability_id = %record.capability_id,
                attempts = attempt,
                "🧬 [核心进化] ✅ 能力已激活: {} (经过 {} 次尝试)",
                record.capability_id, attempt
            );

            return Ok(true);
        }

        // All retries exhausted
        record.status = CoreEvolutionStatus::Failed;
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)?;

        warn!(
            evolution_id = %evolution_id,
            "🧬 [核心进化] ❌ 所有重试已用尽，进化失败"
        );

        Ok(false)
    }

    /// Compile / prepare the artifact
    async fn compile_artifact(&self, record: &CoreEvolutionRecord) -> Result<String> {
        let code = record
            .source_code
            .as_ref()
            .ok_or_else(|| Error::Evolution("No source code to compile".to_string()))?;

        Self::audit_generated_code(&record.provider_kind, code)?;
        let safe_id = Self::check_evolution_boundary(&record.capability_id)?;

        std::fs::create_dir_all(&self.artifacts_dir)?;

        match record.provider_kind {
            ProviderKind::Process | ProviderKind::BuiltIn => {
                // Shell script — 写入文件并设置可执行权限
                let script_path = self.artifacts_dir.join(format!("{}.sh", safe_id));
                std::fs::write(&script_path, code)?;

                // 设置可执行权限（仅 Unix）
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = std::fs::metadata(&script_path)?.permissions();
                    perms.set_mode(0o755);
                    std::fs::set_permissions(&script_path, perms)?;
                }

                // 语法验证：Unix 用 bash -n，Windows 跳过（bash 不可用）
                #[cfg(unix)]
                {
                    let output = tokio::process::Command::new("bash")
                        .arg("-n")
                        .arg(&script_path)
                        .output()
                        .await
                        .map_err(|e| {
                            Error::Evolution(format!("Failed to check bash syntax: {}", e))
                        })?;

                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        return Err(Error::Evolution(format!("Bash syntax error: {}", stderr)));
                    }
                }
                #[cfg(windows)]
                {
                    // Windows 上 bash 不可用，跳过语法检查
                    // 脚本将在实际执行时由 Git Bash 或 WSL 解释
                    debug!(
                        path = %script_path.display(),
                        "🧬 [核心进化] Windows 环境，跳过 bash 语法检查"
                    );
                }

                // 返回路径时统一使用正斜杠，避免 Windows 反斜杠在其他上下文中被吞掉
                let path_str = script_path.to_string_lossy().replace('\\', "/");
                Ok(path_str)
            }
            ProviderKind::ExternalApi => {
                // Python 脚本
                let script_path = self.artifacts_dir.join(format!("{}.py", safe_id));
                std::fs::write(&script_path, code)?;

                // 使用 python3 -m py_compile 验证语法
                let output = tokio::process::Command::new("python3")
                    .arg("-m")
                    .arg("py_compile")
                    .arg(&script_path)
                    .output()
                    .await;

                match output {
                    Ok(o) if !o.status.success() => {
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        return Err(Error::Evolution(format!("Python syntax error: {}", stderr)));
                    }
                    Err(e) => {
                        // python3 不可用，拒绝通过语法检查
                        warn!("python3 不可用，拒绝通过语法检查: {}", e);
                        return Err(Error::Evolution(
                            "python3 不可用: 无法验证 Python 语法，拒绝部署".to_string(),
                        ));
                    }
                    _ => {}
                }

                Ok(script_path.to_string_lossy().to_string())
            }
            ProviderKind::RhaiScript => {
                // Rhai script — compile check with Rhai engine
                let script_path = self.artifacts_dir.join(format!("{}.rhai", safe_id));
                std::fs::write(&script_path, code)?;

                let engine = rhai::Engine::new();
                if let Err(e) = engine.compile(code) {
                    return Err(Error::Evolution(format!("Rhai compilation error: {}", e)));
                }

                Ok(script_path.to_string_lossy().to_string())
            }
        }
    }

    /// Validate the compiled artifact
    async fn validate_artifact(&self, record: &CoreEvolutionRecord) -> Result<ValidationResult> {
        let artifact_path = record
            .artifact_path
            .as_ref()
            .ok_or_else(|| Error::Evolution("No artifact to validate".to_string()))?;

        let mut checks = Vec::new();

        // Check 1: File exists
        let exists = Path::new(artifact_path).exists();
        checks.push(ValidationCheck {
            name: "file_exists".to_string(),
            passed: exists,
            message: if exists {
                "Artifact file exists".to_string()
            } else {
                format!("Artifact file not found: {}", artifact_path)
            },
        });

        if !exists {
            return Ok(ValidationResult {
                passed: false,
                checks,
            });
        }

        // Check 2: File is not empty
        let metadata = std::fs::metadata(artifact_path)?;
        let not_empty = metadata.len() > 0;
        checks.push(ValidationCheck {
            name: "not_empty".to_string(),
            passed: not_empty,
            message: if not_empty {
                format!("Artifact size: {} bytes", metadata.len())
            } else {
                "Artifact file is empty".to_string()
            },
        });

        // Check 3: Generated artifacts are not executed during validation. Syntax checks
        // happen in compile_artifact; executing untrusted generated code on the host would
        // turn validation into an arbitrary-command boundary.
        match record.provider_kind {
            ProviderKind::Process | ProviderKind::BuiltIn => {
                checks.push(ValidationCheck {
                    name: "non_executing_validation".to_string(),
                    passed: true,
                    message: "Shell syntax validated without executing generated code".to_string(),
                });
            }
            _ => {
                // For other types, basic file check is sufficient
                checks.push(ValidationCheck {
                    name: "type_check".to_string(),
                    passed: true,
                    message: format!(
                        "Provider kind {:?} — basic validation passed",
                        record.provider_kind
                    ),
                });
            }
        }

        let all_passed = checks.iter().all(|c| c.passed);
        Ok(ValidationResult {
            passed: all_passed,
            checks,
        })
    }

    /// Load the capability into the registry
    async fn load_capability(&self, record: &CoreEvolutionRecord) -> Result<()> {
        let artifact_path = record
            .artifact_path
            .as_ref()
            .ok_or_else(|| Error::Evolution("No artifact to load".to_string()))?;

        let capability_type = Self::infer_capability_type(&record.capability_id);

        let mut descriptor = CapabilityDescriptor::new(
            &record.capability_id,
            &record.capability_id,
            &record.description,
            capability_type,
            record.provider_kind.clone(),
        )
        .with_privilege(PrivilegeLevel::Limited)
        .with_status(CapabilityStatus::Available) // Starts as Available; promoted to Active after canary passes
        .with_provider_path(artifact_path);

        // Apply input/output schema if extracted from LLM response
        if let Some(ref schema) = record.input_schema {
            descriptor.input_schema = Some(schema.clone());
        }
        if let Some(ref schema) = record.output_schema {
            descriptor.output_schema = Some(schema.clone());
        }

        let executor: Arc<dyn CapabilityExecutor> = match record.provider_kind {
            ProviderKind::Process | ProviderKind::BuiltIn => Arc::new(
                ProcessProvider::new(&record.capability_id, "bash")
                    .with_args(vec![artifact_path.to_string()]),
            ),
            ProviderKind::ExternalApi => Arc::new(ScriptProvider::new(
                &record.capability_id,
                PathBuf::from(artifact_path),
            )),
            ProviderKind::RhaiScript => Arc::new(ScriptProvider::new(
                &record.capability_id,
                PathBuf::from(artifact_path),
            )),
        };

        // A rollback snapshot is a required activation precondition. Do this before
        // mutating the in-memory registry so a failure cannot leave a live-only tool.
        self.version_manager.create_version_if_new_artifact(
            &record.capability_id,
            artifact_path,
            CapabilityVersionSource::Evolution,
            Some(format!("Evolution {}", record.id)),
        )?;

        let mut registry = self.registry.lock().await;
        registry.register_with_executor_and_save(descriptor, executor)?;

        Ok(())
    }

    /// Infer capability type from the ID prefix
    fn infer_capability_type(capability_id: &str) -> CapabilityType {
        let prefix = capability_id.split('.').next().unwrap_or("");
        match prefix {
            "hardware" | "camera" | "mic" | "gpu" | "usb" | "bluetooth" | "sensor" => {
                CapabilityType::Hardware
            }
            "system" | "fs" | "process" | "network" | "clipboard" | "notify" => {
                CapabilityType::System
            }
            "api" | "llm" | "search" | "external" | "web" => CapabilityType::External,
            _ => CapabilityType::Internal,
        }
    }

    // === Record persistence ===

    fn save_record(&self, record: &CoreEvolutionRecord) -> Result<()> {
        std::fs::create_dir_all(&self.records_dir)?;
        let record_file = self.records_dir.join(format!("{}.json", record.id));
        // Write-tmp-then-rename: avoids file corruption if process crashes mid-write
        let counter = CORE_RECORD_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let temp_file = self.records_dir.join(format!(
            "{}.json.tmp_{}_{}_{}",
            record.id,
            chrono::Utc::now().timestamp_millis(),
            pid,
            counter
        ));
        let json = serde_json::to_string_pretty(record)?;
        std::fs::write(&temp_file, &json)?;
        // Atomically replace the record file.
        // On Windows, rename over existing file fails, so we use a backup-based approach:
        // 1. Rename existing file to .bak (preserves data if next step fails)
        // 2. Rename temp file to target
        // 3. Remove .bak backup
        // If step 2 fails, the .bak file can be restored; no data loss.
        if record_file.exists() {
            let backup_path = record_file.with_extension("json.bak");
            let _ = std::fs::rename(&record_file, &backup_path);
            std::fs::rename(&temp_file, &record_file)?;
            let _ = std::fs::remove_file(&backup_path);
        } else {
            std::fs::rename(&temp_file, &record_file)?;
        }
        Ok(())
    }

    pub fn load_record(&self, evolution_id: &str) -> Result<CoreEvolutionRecord> {
        let file = self.records_dir.join(format!("{}.json", evolution_id));
        let json = std::fs::read_to_string(&file)?;
        let record = serde_json::from_str(&json)?;
        Ok(record)
    }

    pub fn list_records(&self) -> Result<Vec<CoreEvolutionRecord>> {
        if !self.records_dir.exists() {
            return Ok(Vec::new());
        }

        let mut records = Vec::new();
        for entry in std::fs::read_dir(&self.records_dir)?.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(record) = serde_json::from_str::<CoreEvolutionRecord>(&content) {
                        records.push(record);
                    }
                }
            }
        }
        records.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        Ok(records)
    }

    /// Extract input/output schema from LLM response (after the code block)
    fn extract_schema_from_response(
        &self,
        response: &str,
    ) -> Option<(serde_json::Value, serde_json::Value)> {
        // Look for ```json block after the code block
        let json_marker = "```json";
        if let Some(start) = response.rfind(json_marker) {
            let after = start + json_marker.len();
            if let Some(end) = response[after..].find("```") {
                let json_str = response[after..after + end].trim();
                if let Ok(schema) = serde_json::from_str::<serde_json::Value>(json_str) {
                    let input = schema.get("input_schema").cloned();
                    let output = schema.get("output_schema").cloned();
                    if input.is_some() || output.is_some() {
                        return Some((
                            input.unwrap_or(serde_json::json!({})),
                            output.unwrap_or(serde_json::json!({})),
                        ));
                    }
                }
            }
        }
        None
    }

    /// Rollback a capability to its previous version AND rebuild the executor in the registry.
    /// This ensures the rollback is effective at runtime, not just on disk.
    pub async fn rollback_capability(&self, capability_id: &str) -> Result<bool> {
        // Step 1: File-level rollback
        let restored_path = match self.version_manager.rollback(capability_id)? {
            Some(path) => path,
            None => return Ok(false),
        };

        // Step 2: Rebuild executor from the restored artifact and rebind in registry
        let ext = std::path::Path::new(&restored_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("sh");

        let executor: Arc<dyn CapabilityExecutor> = match ext {
            "py" => Arc::new(ScriptProvider::new(
                capability_id,
                std::path::PathBuf::from(&restored_path),
            )),
            "rhai" => Arc::new(ScriptProvider::new(
                capability_id,
                std::path::PathBuf::from(&restored_path),
            )),
            _ => Arc::new(
                ProcessProvider::new(capability_id, "bash").with_args(vec![restored_path.clone()]),
            ),
        };

        let new_version = self.version_manager.get_current_version(capability_id)?;

        let mut registry = self.registry.lock().await;
        registry.replace_executor(capability_id, executor, &new_version)?;

        info!(
            capability_id = %capability_id,
            version = %new_version,
            "🧬 [核心进化] 回滚完成: {} -> {} (executor 已重建)",
            capability_id, new_version
        );

        Ok(true)
    }

    /// Get the capability registry handle
    pub fn registry(&self) -> &CapabilityRegistryHandle {
        &self.registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_capability_type() {
        assert_eq!(
            CoreEvolution::infer_capability_type("hardware.camera"),
            CapabilityType::Hardware
        );
        assert_eq!(
            CoreEvolution::infer_capability_type("system.clipboard"),
            CapabilityType::System
        );
        assert_eq!(
            CoreEvolution::infer_capability_type("api.weather"),
            CapabilityType::External
        );
        assert_eq!(
            CoreEvolution::infer_capability_type("custom.something"),
            CapabilityType::Internal
        );
    }

    #[test]
    fn test_extract_code_bash() {
        let dir = std::env::temp_dir().join("test_core_evo");
        let registry = crate::capability_provider::new_registry_handle(dir.clone());
        let evo = CoreEvolution::new(dir, registry, 300);

        let response =
            "Here's the script:\n```bash\n#!/bin/bash\necho '{\"ok\": true}'\n```\nDone.";
        let code = evo
            .extract_code_from_response(response, &ProviderKind::Process)
            .unwrap();
        assert!(code.contains("#!/bin/bash"));
        assert!(code.contains("echo"));
    }

    #[test]
    fn test_extract_code_python() {
        let dir = std::env::temp_dir().join("test_core_evo_py");
        let registry = crate::capability_provider::new_registry_handle(dir.clone());
        let evo = CoreEvolution::new(dir, registry, 300);

        let response = "```python\nimport json\nprint(json.dumps({\"ok\": True}))\n```";
        let code = evo
            .extract_code_from_response(response, &ProviderKind::ExternalApi)
            .unwrap();
        assert!(code.contains("import json"));
    }

    #[test]
    fn evolution_rejects_dangerous_generated_process_code() {
        let result = CoreEvolution::audit_generated_code(
            &ProviderKind::Process,
            "#!/bin/bash\nrm -rf /tmp/blockcell-generated-test\necho '{}'",
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn evolution_boundary_rejects_escaping_capability_ids() {
        let dir = std::env::temp_dir().join("test_core_evo_boundary");
        let _ = std::fs::remove_dir_all(&dir);
        let registry = crate::capability_provider::new_registry_handle(dir.clone());
        let evo = CoreEvolution::new(dir, registry, 300);

        let escaping = evo
            .request_capability("../../escape", "escape", ProviderKind::Process)
            .await
            .unwrap_err();
        assert!(
            escaping.to_string().contains("evolution boundary"),
            "{escaping}"
        );
        assert!(evo.list_records().unwrap_or_default().is_empty());
    }

    #[tokio::test]
    async fn colliding_artifact_names_are_rejected() {
        let dir = std::env::temp_dir().join("test_core_evo_artifact_collision");
        let _ = std::fs::remove_dir_all(&dir);
        let registry = crate::capability_provider::new_registry_handle(dir.clone());
        let evo = CoreEvolution::new(dir, registry, 300);

        let first = evo
            .request_capability("api.weather", "weather", ProviderKind::Process)
            .await
            .unwrap();
        let err = evo
            .request_capability("api_weather", "weather again", ProviderKind::Process)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("artifact name"), "{err}");
        assert_eq!(
            evo.request_capability("api.weather", "same", ProviderKind::Process)
                .await
                .unwrap(),
            first
        );
    }

    #[tokio::test]
    async fn test_request_idempotent() {
        let dir = std::env::temp_dir().join("test_core_evo_idempotent");
        let _ = std::fs::remove_dir_all(&dir);
        let registry = crate::capability_provider::new_registry_handle(dir.clone());
        let evo = CoreEvolution::new(dir.clone(), registry, 300);

        // First request creates a new record
        let id1 = evo
            .request_capability("test.cap", "test", ProviderKind::Process)
            .await
            .unwrap();
        // Second request for same capability should return the same id (idempotent)
        let id2 = evo
            .request_capability("test.cap", "test again", ProviderKind::Process)
            .await
            .unwrap();
        assert_eq!(id1, id2);

        // Different capability should create a new record
        let id3 = evo
            .request_capability("test.other", "other", ProviderKind::Process)
            .await
            .unwrap();
        assert_ne!(id1, id3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_request_blocked_after_failures() {
        let dir = std::env::temp_dir().join("test_core_evo_blocked");
        let _ = std::fs::remove_dir_all(&dir);
        let registry = crate::capability_provider::new_registry_handle(dir.clone());
        let evo = CoreEvolution::new(dir.clone(), registry, 300);

        // Simulate MAX_AUTO_FAILURES consecutive failures
        for i in 0..MAX_AUTO_FAILURES {
            let record = CoreEvolutionRecord {
                id: format!("fail_{}", i),
                capability_id: "test.fail".to_string(),
                description: "test".to_string(),
                status: CoreEvolutionStatus::Failed,
                provider_kind: ProviderKind::Process,
                source_code: None,
                artifact_path: None,
                compile_output: None,
                validation: None,
                attempt: 1,
                max_attempts: 3,
                feedback_history: Vec::new(),
                input_schema: None,
                output_schema: None,
                created_at: chrono::Utc::now().timestamp() - (MAX_AUTO_FAILURES - i) as i64,
                updated_at: chrono::Utc::now().timestamp(),
            };
            evo.save_record(&record).unwrap();
        }

        // Next request should be blocked
        let result = evo
            .request_capability("test.fail", "retry", ProviderKind::Process)
            .await;
        assert!(result.is_err());
        assert!(evo.is_blocked("test.fail").unwrap());

        // Unblock should work
        assert_eq!(evo.unblock_capability("test.fail").unwrap(), 1);
        assert!(!evo.is_blocked("test.fail").unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn capability_activation_failure_does_not_leave_registered_executor() {
        let root = std::env::temp_dir().join(format!(
            "test_core_evo_activation_failure_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let artifact_dir = root.join("tool_artifacts");
        std::fs::create_dir_all(&artifact_dir).expect("create artifact dir");
        let artifact_path = artifact_dir.join("test_activation.sh");
        std::fs::write(&artifact_path, "#!/bin/sh\necho '{}'\n").expect("write artifact");

        let registry_path = root.join("registry-as-file");
        std::fs::write(&registry_path, "not a directory").expect("create registry blocker");
        let registry = crate::capability_provider::new_registry_handle(registry_path);
        let evo = CoreEvolution::new(root.clone(), registry.clone(), 300);
        let record = CoreEvolutionRecord {
            id: "activation_failure".to_string(),
            capability_id: "test.activation_failure".to_string(),
            description: "test activation transaction".to_string(),
            status: CoreEvolutionStatus::Validated,
            provider_kind: ProviderKind::Process,
            source_code: None,
            artifact_path: Some(artifact_path.to_string_lossy().to_string()),
            compile_output: None,
            validation: None,
            attempt: 1,
            max_attempts: 3,
            feedback_history: Vec::new(),
            input_schema: None,
            output_schema: None,
            created_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
        };

        assert!(evo.load_capability(&record).await.is_err());
        let registry = registry.lock().await;
        assert!(registry.get_descriptor(&record.capability_id).is_none());
        assert!(registry.get_executor(&record.capability_id).is_none());

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn per_record_pipeline_lock_prevents_concurrent_double_execution() {
        let dir = std::env::temp_dir().join(format!(
            "test_core_evo_pipeline_lock_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let registry = crate::capability_provider::new_registry_handle(dir.clone());
        let evo = CoreEvolution::new(dir.clone(), registry, 300);

        let guard = evo
            .try_acquire_pipeline_lock("evo-1")
            .await
            .expect("first lock acquisition must succeed");
        assert!(
            evo.try_acquire_pipeline_lock("evo-1").await.is_none(),
            "concurrent acquisition for the same evolution_id must be rejected"
        );
        assert!(
            evo.run_step("evo-1", EvolutionStep::BuildPrompt)
                .await
                .is_err(),
            "run_step must refuse when evolution_id is already locked"
        );
        drop(guard);
        assert!(
            evo.try_acquire_pipeline_lock("evo-1").await.is_some(),
            "lock must be released when guard is dropped"
        );

        let _ = std::fs::remove_dir_all(dir);
    }
}
