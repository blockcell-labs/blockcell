use crate::capability_provider::{CapabilityExecutor, CapabilityRegistryHandle, ProcessProvider, ScriptProvider};
use crate::capability_versioning::{CapabilityVersionManager, CapabilityVersionSource};
use crate::evolution::LLMProvider;
use blockcell_core::{
    CapabilityDescriptor, CapabilityStatus, CapabilityType,
    Error, ProviderKind, PrivilegeLevel, Result,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, warn};

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

/// Core-level evolution engine
///
/// Unlike the Rhai skill evolution which generates scripts, this generates
/// actual executable capabilities:
/// - Rust source → compile to .dylib → hot-load
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
}

impl CoreEvolution {
    pub fn new(base_dir: PathBuf, registry: CapabilityRegistryHandle) -> Self {
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
        }
    }

    /// Get a reference to the capability version manager.
    pub fn version_manager(&self) -> &CapabilityVersionManager {
        &self.version_manager
    }

    /// Set the LLM provider for autonomous evolution.
    pub fn set_llm_provider(&mut self, provider: Arc<dyn LLMProvider>) {
        self.llm_provider = Some(provider);
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
        let pending: Vec<_> = records.iter()
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
    pub fn is_blocked(&self, capability_id: &str) -> Result<bool> {
        let records = self.list_records()?;
        for r in &records {
            if r.capability_id == capability_id && r.status == CoreEvolutionStatus::Blocked {
                return Ok(true);
            }
        }
        Ok(false)
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
            let block_id = format!(
                "core_evo_{}_{}",
                capability_id.replace('.', "_"),
                chrono::Utc::now().timestamp()
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

        let evolution_id = format!(
            "core_evo_{}_{}", 
            capability_id.replace('.', "_"),
            chrono::Utc::now().timestamp()
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

    /// Unblock a previously blocked capability, allowing auto-triggering again.
    pub fn unblock_capability(&self, capability_id: &str) -> Result<bool> {
        let records = self.list_records()?;
        for r in &records {
            if r.capability_id == capability_id && r.status == CoreEvolutionStatus::Blocked {
                let mut updated = r.clone();
                updated.status = CoreEvolutionStatus::Failed;
                updated.description = format!("UNBLOCKED (was: {})", r.description);
                updated.updated_at = chrono::Utc::now().timestamp();
                self.save_record(&updated)?;
                info!(
                    capability_id = %capability_id,
                    "🧬 [核心进化] ✅ 能力 '{}' 已解除阻止",
                    capability_id
                );
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Run the full evolution pipeline for a capability
    pub async fn run_evolution(
        &self,
        evolution_id: &str,
        llm_provider: &dyn LLMProvider,
    ) -> Result<bool> {
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
            if let Some((input_schema, output_schema)) = self.extract_schema_from_response(&raw_response) {
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
                let issues: Vec<String> = validation.checks.iter()
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

    /// Generate code for the capability using LLM.
    /// Returns (extracted_code, raw_llm_response) so caller can also extract schemas.
    async fn generate_code(
        &self,
        record: &CoreEvolutionRecord,
        llm_provider: &dyn LLMProvider,
    ) -> Result<(String, String)> {
        let prompt = self.build_generation_prompt(record)?;

        debug!(
            evolution_id = %record.id,
            prompt_len = prompt.len(),
            "🧬 [核心进化] 生成 prompt ({} chars)",
            prompt.len()
        );

        let response = llm_provider.generate(&prompt).await?;
        let code = self.extract_code_from_response(&response, &record.provider_kind)?;

        info!(
            evolution_id = %record.id,
            code_len = code.len(),
            "🧬 [核心进化] 代码已生成 ({} chars)",
            code.len()
        );

        Ok((code, response))
    }

    fn build_generation_prompt(&self, record: &CoreEvolutionRecord) -> Result<String> {
        let mut prompt = String::new();

        prompt.push_str("You are a capability evolution engine for the blockcell self-augmenting agent.\n");
        prompt.push_str("Your task is to generate executable code that implements a new capability.\n\n");

        prompt.push_str(&format!("## Capability Request\n"));
        prompt.push_str(&format!("- **ID**: {}\n", record.capability_id));
        prompt.push_str(&format!("- **Description**: {}\n", record.description));
        prompt.push_str(&format!("- **Provider Type**: {:?}\n\n", record.provider_kind));

        match record.provider_kind {
            ProviderKind::Process => {
                prompt.push_str("## Requirements\n");
                prompt.push_str("Generate a shell script that:\n");
                prompt.push_str("1. Reads JSON input from stdin\n");
                prompt.push_str("2. Performs the requested operation\n");
                prompt.push_str("3. Outputs JSON result to stdout\n");
                prompt.push_str("4. Returns exit code 0 on success, non-zero on failure\n\n");
                prompt.push_str("The script should be self-contained and use only standard system tools.\n");
                prompt.push_str("Use `#!/bin/bash` as the shebang.\n\n");
                prompt.push_str("## Output Format\n");
                prompt.push_str("Output ONLY the shell script in a ```bash code block.\n");
            }
            ProviderKind::ExternalApi => {
                prompt.push_str("## Requirements\n");
                prompt.push_str("Generate a Python script that:\n");
                prompt.push_str("1. Reads JSON input from the CAPABILITY_INPUT environment variable\n");
                prompt.push_str("2. Performs the requested API call\n");
                prompt.push_str("3. Prints JSON result to stdout\n");
                prompt.push_str("4. Uses only standard library modules (json, urllib, os, sys)\n\n");
                prompt.push_str("## Output Format\n");
                prompt.push_str("Output ONLY the Python script in a ```python code block.\n");
            }
            _ => {
                prompt.push_str("## Requirements\n");
                prompt.push_str("Generate a shell script (bash) that:\n");
                prompt.push_str("1. Reads JSON input from stdin\n");
                prompt.push_str("2. Implements the capability using available system tools\n");
                prompt.push_str("3. Outputs JSON result to stdout\n\n");
                prompt.push_str("## Output Format\n");
                prompt.push_str("Output ONLY the script in a ```bash code block.\n");
            }
        }

        // Request input/output schema alongside the code
        prompt.push_str("\n## Schema Requirement\n");
        prompt.push_str("After the code block, output a JSON block describing the input and output schema.\n");
        prompt.push_str("Format:\n```json\n{\n  \"input_schema\": {\n    \"type\": \"object\",\n    \"properties\": { ... },\n    \"required\": [ ... ]\n  },\n  \"output_schema\": {\n    \"type\": \"object\",\n    \"properties\": { ... }\n  }\n}\n```\n");
        prompt.push_str("This schema helps the agent understand how to call this capability.\n\n");

        // Add feedback history if retrying
        if !record.feedback_history.is_empty() {
            prompt.push_str("\n## Previous Attempts (FAILED — fix these issues)\n");
            for entry in &record.feedback_history {
                prompt.push_str(&format!("### Attempt #{} ({} failure)\n", entry.attempt, entry.stage));
                prompt.push_str(&format!("**Issue**: {}\n", entry.feedback));
                prompt.push_str(&format!("**Previous code**:\n```\n{}\n```\n\n", entry.previous_code));
            }
            prompt.push_str("Fix ALL the issues above. Do NOT repeat the same mistakes.\n");
        }

        Ok(prompt)
    }

    fn extract_code_from_response(&self, response: &str, provider_kind: &ProviderKind) -> Result<String> {
        // Try language-specific code blocks first
        let markers = match provider_kind {
            ProviderKind::Process | ProviderKind::BuiltIn => vec!["```bash", "```sh", "```shell"],
            ProviderKind::ExternalApi => vec!["```python", "```py"],
            _ => vec!["```bash", "```sh", "```python"],
        };

        for marker in &markers {
            if let Some(start) = response.find(marker) {
                let after = start + marker.len();
                if let Some(end) = response[after..].find("```") {
                    return Ok(response[after..after + end].trim().to_string());
                }
            }
        }

        // Fallback: generic code block
        if let Some(start) = response.find("```") {
            let after = start + 3;
            let content_start = response[after..].find('\n')
                .map(|i| after + i + 1)
                .unwrap_or(after);
            if let Some(end) = response[content_start..].find("```") {
                return Ok(response[content_start..content_start + end].trim().to_string());
            }
        }

        // Last resort: entire response
        Ok(response.trim().to_string())
    }

    /// Compile / prepare the artifact
    async fn compile_artifact(&self, record: &CoreEvolutionRecord) -> Result<String> {
        let code = record.source_code.as_ref()
            .ok_or_else(|| Error::Evolution("No source code to compile".to_string()))?;

        std::fs::create_dir_all(&self.artifacts_dir)?;

        let safe_id = record.capability_id.replace('.', "_");

        match record.provider_kind {
            ProviderKind::Process | ProviderKind::BuiltIn => {
                // Shell script — just write to file and make executable
                let script_path = self.artifacts_dir.join(format!("{}.sh", safe_id));
                std::fs::write(&script_path, code)?;

                // Make executable
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = std::fs::metadata(&script_path)?.permissions();
                    perms.set_mode(0o755);
                    std::fs::set_permissions(&script_path, perms)?;
                }

                // Validate syntax with bash -n
                let output = tokio::process::Command::new("bash")
                    .arg("-n")
                    .arg(&script_path)
                    .output()
                    .await
                    .map_err(|e| Error::Evolution(format!("Failed to check bash syntax: {}", e)))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(Error::Evolution(format!("Bash syntax error: {}", stderr)));
                }

                Ok(script_path.to_string_lossy().to_string())
            }
            ProviderKind::ExternalApi => {
                // Python script
                let script_path = self.artifacts_dir.join(format!("{}.py", safe_id));
                std::fs::write(&script_path, code)?;

                // Validate syntax with python3 -m py_compile
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
                        // python3 not available, skip syntax check
                        warn!("python3 not available for syntax check: {}", e);
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
            ProviderKind::DynamicLibrary => {
                // For now, dynamic library compilation requires a Rust toolchain.
                // We generate a standalone Rust file and compile it.
                let src_dir = self.artifacts_dir.join(format!("{}_src", safe_id));
                std::fs::create_dir_all(&src_dir)?;

                let src_path = src_dir.join("lib.rs");
                std::fs::write(&src_path, code)?;

                // Try to compile with rustc
                let lib_name = format!("lib{}", safe_id);
                let output_path = self.artifacts_dir.join(format!("{}.dylib", lib_name));

                let output = tokio::process::Command::new("rustc")
                    .arg("--crate-type=cdylib")
                    .arg("--edition=2021")
                    .arg("-o")
                    .arg(&output_path)
                    .arg(&src_path)
                    .output()
                    .await
                    .map_err(|e| Error::Evolution(format!("Failed to invoke rustc: {}", e)))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(Error::Evolution(format!("Rust compilation error:\n{}", stderr)));
                }

                Ok(output_path.to_string_lossy().to_string())
            }
        }
    }

    /// Validate the compiled artifact
    async fn validate_artifact(&self, record: &CoreEvolutionRecord) -> Result<ValidationResult> {
        let artifact_path = record.artifact_path.as_ref()
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
            return Ok(ValidationResult { passed: false, checks });
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

        // Check 3: For scripts, try a dry-run with empty input
        match record.provider_kind {
            ProviderKind::Process | ProviderKind::BuiltIn => {
                let output = tokio::process::Command::new("bash")
                    .arg(artifact_path)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn();

                match output {
                    Ok(mut child) => {
                        // Send empty JSON input
                        if let Some(mut stdin) = child.stdin.take() {
                            use tokio::io::AsyncWriteExt;
                            let _ = stdin.write_all(b"{}").await;
                            drop(stdin);
                        }

                        let result = tokio::time::timeout(
                            std::time::Duration::from_secs(10),
                            child.wait_with_output(),
                        ).await;

                        match result {
                            Ok(Ok(output)) => {
                                let stdout = String::from_utf8_lossy(&output.stdout);
                                let exit_ok = output.status.success();
                                let is_json = serde_json::from_str::<serde_json::Value>(stdout.trim()).is_ok();
                                // Require both: successful exit AND valid JSON output
                                let passed = exit_ok && is_json;
                                checks.push(ValidationCheck {
                                    name: "dry_run".to_string(),
                                    passed,
                                    message: if passed {
                                        format!("Script executed successfully with valid JSON output ({} bytes)", stdout.len())
                                    } else if !exit_ok {
                                        let stderr = String::from_utf8_lossy(&output.stderr);
                                        format!(
                                            "Script exited with code {}: {}",
                                            output.status.code().unwrap_or(-1),
                                            stderr.chars().take(200).collect::<String>()
                                        )
                                    } else {
                                        format!(
                                            "Script ran but output is not valid JSON: {}",
                                            stdout.chars().take(100).collect::<String>()
                                        )
                                    },
                                });
                            }
                            Ok(Err(e)) => {
                                checks.push(ValidationCheck {
                                    name: "dry_run".to_string(),
                                    passed: false,
                                    message: format!("Script execution error: {}", e),
                                });
                            }
                            Err(_) => {
                                checks.push(ValidationCheck {
                                    name: "dry_run".to_string(),
                                    passed: false,
                                    message: "Script timed out (10s)".to_string(),
                                });
                            }
                        }
                    }
                    Err(e) => {
                        checks.push(ValidationCheck {
                            name: "dry_run".to_string(),
                            passed: false,
                            message: format!("Failed to spawn script: {}", e),
                        });
                    }
                }
            }
            _ => {
                // For other types, basic file check is sufficient
                checks.push(ValidationCheck {
                    name: "type_check".to_string(),
                    passed: true,
                    message: format!("Provider kind {:?} — basic validation passed", record.provider_kind),
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
        let artifact_path = record.artifact_path.as_ref()
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
            ProviderKind::Process | ProviderKind::BuiltIn => {
                Arc::new(ProcessProvider::new(&record.capability_id, "bash")
                    .with_args(vec![artifact_path.to_string()]))
            }
            ProviderKind::ExternalApi => {
                Arc::new(ScriptProvider::new(
                    &record.capability_id,
                    PathBuf::from(artifact_path),
                ))
            }
            ProviderKind::RhaiScript => {
                Arc::new(ScriptProvider::new(
                    &record.capability_id,
                    PathBuf::from(artifact_path),
                ))
            }
            ProviderKind::DynamicLibrary => {
                // Dynamic library loading would use libloading
                // For now, wrap as a process that runs the .dylib via a helper
                warn!(
                    "🧬 [核心进化] 动态库加载暂未完全实现，使用进程模式作为后备"
                );
                Arc::new(ProcessProvider::new(&record.capability_id, artifact_path))
            }
        };

        let mut registry = self.registry.lock().await;
        registry.register_with_executor(descriptor, executor);

        // Persist registry
        if let Err(e) = registry.save() {
            warn!(error = %e, "Failed to persist capability registry");
        }

        // Create version snapshot for rollback support
        if let Err(e) = self.version_manager.create_version(
            &record.capability_id,
            artifact_path,
            CapabilityVersionSource::Evolution,
            Some(format!("Evolution {}", record.id)),
        ) {
            warn!(error = %e, "Failed to create capability version snapshot");
        }

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
            "api" | "llm" | "search" | "external" | "web" => {
                CapabilityType::External
            }
            _ => CapabilityType::Internal,
        }
    }

    // === Record persistence ===

    fn save_record(&self, record: &CoreEvolutionRecord) -> Result<()> {
        std::fs::create_dir_all(&self.records_dir)?;
        let file = self.records_dir.join(format!("{}.json", record.id));
        let json = serde_json::to_string_pretty(record)?;
        std::fs::write(file, json)?;
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
            if path.extension().map_or(false, |e| e == "json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(record) = serde_json::from_str::<CoreEvolutionRecord>(&content) {
                        records.push(record);
                    }
                }
            }
        }
        records.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(records)
    }

    /// Extract input/output schema from LLM response (after the code block)
    fn extract_schema_from_response(&self, response: &str) -> Option<(serde_json::Value, serde_json::Value)> {
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
            "py" => Arc::new(ScriptProvider::new(capability_id, std::path::PathBuf::from(&restored_path))),
            "rhai" => Arc::new(ScriptProvider::new(capability_id, std::path::PathBuf::from(&restored_path))),
            _ => Arc::new(ProcessProvider::new(capability_id, "bash").with_args(vec![restored_path.clone()])),
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
        let evo = CoreEvolution::new(dir, registry);

        let response = "Here's the script:\n```bash\n#!/bin/bash\necho '{\"ok\": true}'\n```\nDone.";
        let code = evo.extract_code_from_response(response, &ProviderKind::Process).unwrap();
        assert!(code.contains("#!/bin/bash"));
        assert!(code.contains("echo"));
    }

    #[test]
    fn test_extract_code_python() {
        let dir = std::env::temp_dir().join("test_core_evo_py");
        let registry = crate::capability_provider::new_registry_handle(dir.clone());
        let evo = CoreEvolution::new(dir, registry);

        let response = "```python\nimport json\nprint(json.dumps({\"ok\": True}))\n```";
        let code = evo.extract_code_from_response(response, &ProviderKind::ExternalApi).unwrap();
        assert!(code.contains("import json"));
    }

    #[tokio::test]
    async fn test_request_idempotent() {
        let dir = std::env::temp_dir().join("test_core_evo_idempotent");
        let _ = std::fs::remove_dir_all(&dir);
        let registry = crate::capability_provider::new_registry_handle(dir.clone());
        let evo = CoreEvolution::new(dir.clone(), registry);

        // First request creates a new record
        let id1 = evo.request_capability("test.cap", "test", ProviderKind::Process).await.unwrap();
        // Second request for same capability should return the same id (idempotent)
        let id2 = evo.request_capability("test.cap", "test again", ProviderKind::Process).await.unwrap();
        assert_eq!(id1, id2);

        // Different capability should create a new record
        let id3 = evo.request_capability("test.other", "other", ProviderKind::Process).await.unwrap();
        assert_ne!(id1, id3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_request_blocked_after_failures() {
        let dir = std::env::temp_dir().join("test_core_evo_blocked");
        let _ = std::fs::remove_dir_all(&dir);
        let registry = crate::capability_provider::new_registry_handle(dir.clone());
        let evo = CoreEvolution::new(dir.clone(), registry);

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
                feedback_history: Vec::new(),
                input_schema: None,
                output_schema: None,
                created_at: chrono::Utc::now().timestamp() - (MAX_AUTO_FAILURES - i) as i64,
                updated_at: chrono::Utc::now().timestamp(),
            };
            evo.save_record(&record).unwrap();
        }

        // Next request should be blocked
        let result = evo.request_capability("test.fail", "retry", ProviderKind::Process).await;
        assert!(result.is_err());
        assert!(evo.is_blocked("test.fail").unwrap());

        // Unblock should work
        assert!(evo.unblock_capability("test.fail").unwrap());
        assert!(!evo.is_blocked("test.fail").unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
