use crate::versioning::{VersionManager, VersionSource};
use blockcell_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// 技能自进化管理器
pub struct SkillEvolution {
    skills_dir: PathBuf,
    evolution_db: PathBuf,
    version_manager: VersionManager,
    llm_timeout_secs: u64,
}

/// 进化触发原因
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TriggerReason {
    /// 执行错误
    ExecutionError { error: String, count: u32 },
    /// 连续失败
    ConsecutiveFailures { count: u32, window_minutes: u32 },
    /// 性能退化
    PerformanceDegradation { metric: String, threshold: f64 },
    /// 外部 API 变化
    ApiChange { endpoint: String, status_code: u16 },
    /// 用户手动请求进化
    ManualRequest { description: String },
}

/// 进化上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionContext {
    pub skill_name: String,
    pub current_version: String,
    pub trigger: TriggerReason,
    pub error_stack: Option<String>,
    pub source_snippet: Option<String>,
    pub tool_schemas: Vec<serde_json::Value>,
    pub timestamp: i64,
}

/// 生成的补丁
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedPatch {
    pub patch_id: String,
    pub skill_name: String,
    pub diff: String,
    pub explanation: String,
    pub generated_at: i64,
}

/// 审计结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditResult {
    pub passed: bool,
    pub issues: Vec<AuditIssue>,
    pub audited_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditIssue {
    pub severity: String, // "error", "warning", "info"
    pub category: String, // "syntax", "permission", "loop", "leak"
    pub message: String,
}

/// Shadow Test 结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowTestResult {
    pub passed: bool,
    pub test_cases_run: u32,
    pub test_cases_passed: u32,
    pub errors: Vec<String>,
    pub tested_at: i64,
}

/// 观察窗口配置（简化模型：部署后进入观察期，错误率超阈值则回滚）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationWindow {
    /// 观察窗口时长（分钟）
    pub duration_minutes: u32,
    /// 错误率阈值，超过则回滚
    pub error_threshold: f64,
    /// 观察开始时间戳
    pub started_at: i64,
}

impl Default for ObservationWindow {
    fn default() -> Self {
        Self {
            duration_minutes: 60,
            error_threshold: 0.1,
            started_at: chrono::Utc::now().timestamp(),
        }
    }
}

// Legacy type aliases for backward-compatible deserialization of old records
/// Legacy rollout config (kept for serde compatibility with old records)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolloutConfig {
    #[serde(default)]
    pub stages: Vec<RolloutStage>,
    #[serde(default)]
    pub current_stage: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolloutStage {
    #[serde(default)]
    pub percentage: u8,
    #[serde(default)]
    pub duration_minutes: u32,
    #[serde(default)]
    pub error_threshold: f64,
}

/// 每次重试的反馈记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEntry {
    pub attempt: u32,
    pub stage: String,           // "audit", "compile", "test"
    pub feedback: String,        // 具体的错误/问题描述
    pub previous_code: String,   // 上一次生成的代码
    pub timestamp: i64,
}

/// 进化记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionRecord {
    pub id: String,
    pub skill_name: String,
    pub context: EvolutionContext,
    pub patch: Option<GeneratedPatch>,
    pub audit: Option<AuditResult>,
    pub shadow_test: Option<ShadowTestResult>,
    /// 观察窗口（部署后的错误率监控）
    pub observation: Option<ObservationWindow>,
    /// Legacy rollout field (for backward-compatible deserialization of old records)
    #[serde(default, skip_serializing)]
    pub rollout: Option<RolloutConfig>,
    pub status: EvolutionStatus,
    /// 当前尝试次数（从 1 开始）
    #[serde(default = "default_attempt")]
    pub attempt: u32,
    /// 历次重试的反馈记录
    #[serde(default)]
    pub feedback_history: Vec<FeedbackEntry>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn default_attempt() -> u32 { 1 }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EvolutionStatus {
    Triggered,
    Generating,
    Generated,
    Auditing,
    AuditPassed,
    AuditFailed,
    /// 编译检查通过（合并了原 DryRunPassed + TestPassed）
    CompilePassed,
    /// 编译检查失败（合并了原 DryRunFailed + TestFailed）
    CompileFailed,
    /// 已部署，观察窗口中（替代原 RollingOut）
    Observing,
    Completed,
    RolledBack,
    Failed,
    // Legacy variants kept for backward-compatible deserialization of old records
    DryRunPassed,
    DryRunFailed,
    Testing,
    TestPassed,
    TestFailed,
    RollingOut,
}

impl EvolutionStatus {
    /// 将旧状态映射到新状态（用于处理旧记录）
    pub fn normalize(&self) -> &EvolutionStatus {
        match self {
            EvolutionStatus::DryRunPassed | EvolutionStatus::TestPassed => &EvolutionStatus::CompilePassed,
            EvolutionStatus::DryRunFailed | EvolutionStatus::TestFailed | EvolutionStatus::Testing => &EvolutionStatus::CompileFailed,
            EvolutionStatus::RollingOut => &EvolutionStatus::Observing,
            other => other,
        }
    }

    /// 检查状态是否等价于 CompilePassed（包括旧状态）
    pub fn is_compile_passed(&self) -> bool {
        matches!(self, EvolutionStatus::CompilePassed | EvolutionStatus::DryRunPassed | EvolutionStatus::TestPassed)
    }
}

impl SkillEvolution {
    pub fn new(skills_dir: PathBuf, llm_timeout_secs: u64) -> Self {
        let evolution_db = skills_dir.parent()
            .unwrap_or(Path::new("."))
            .join("evolution.db");
        let version_manager = VersionManager::new(skills_dir.clone());
        
        Self {
            skills_dir,
            evolution_db,
            version_manager,
            llm_timeout_secs,
        }
    }

    pub fn version_manager(&self) -> &VersionManager {
        &self.version_manager
    }

    /// Get the skills directory path.
    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }

    /// Get the evolution records directory path.
    pub fn records_dir(&self) -> PathBuf {
        self.evolution_db.parent().unwrap().join("evolution_records")
    }

    /// Load the current SKILL.rhai source for a skill (returns None if not found).
    pub fn load_skill_source(&self, skill_name: &str) -> Result<Option<String>> {
        let rhai_path = self.skills_dir.join(skill_name).join("SKILL.rhai");
        if rhai_path.exists() {
            Ok(std::fs::read_to_string(&rhai_path).ok())
        } else {
            Ok(None)
        }
    }

    /// 触发技能进化
    pub async fn trigger_evolution(&self, context: EvolutionContext) -> Result<String> {
        // Use milliseconds + random suffix to guarantee uniqueness even within the same second
        let evolution_id = format!(
            "evo_{}_{:x}",
            context.skill_name,
            chrono::Utc::now().timestamp_millis()
        );

        info!(
            skill = %context.skill_name,
            evolution_id = %evolution_id,
            "Triggering skill evolution"
        );

        let record = EvolutionRecord {
            id: evolution_id.clone(),
            skill_name: context.skill_name.clone(),
            context,
            patch: None,
            audit: None,
            shadow_test: None,
            observation: None,
            rollout: None,
            status: EvolutionStatus::Triggered,
            attempt: 1,
            feedback_history: Vec::new(),
            created_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
        };

        self.save_record(&record)?;
        Ok(evolution_id)
    }

    /// 生成补丁（调用 LLM）
    pub async fn generate_patch(
        &self,
        evolution_id: &str,
        llm_provider: &dyn LLMProvider,
    ) -> Result<GeneratedPatch> {
        let mut record = self.load_record(evolution_id)?;
        record.status = EvolutionStatus::Generating;
        self.save_record(&record)?;

        info!(evolution_id = %evolution_id, "Generating patch");

        // 构建 prompt
        let prompt = self.build_generation_prompt(&record.context)?;

        info!(
            evolution_id = %evolution_id,
            prompt_len = prompt.len(),
            "📝 [generate] Prompt built"
        );
        debug!(
            evolution_id = %evolution_id,
            "📝 [generate] Full prompt:\n{}",
            prompt
        );

        // 调用 LLM（带超时保护）
        info!(evolution_id = %evolution_id, "📝 [generate] Calling LLM...");
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(self.llm_timeout_secs),
            llm_provider.generate(&prompt)
        )
        .await
        .map_err(|_| Error::Evolution(format!("LLM call timed out after {} seconds", self.llm_timeout_secs)))?
        .map_err(|e| Error::Evolution(format!("LLM generation failed: {}", e)))?;

        info!(
            evolution_id = %evolution_id,
            response_len = response.len(),
            "📝 [generate] LLM response received"
        );
        debug!(
            evolution_id = %evolution_id,
            "📝 [generate] Full LLM response:\n{}",
            response
        );

        // 解析 diff
        let diff = self.extract_diff_from_response(&response)?;

        info!(
            evolution_id = %evolution_id,
            diff_len = diff.len(),
            diff_lines = diff.lines().count(),
            "📝 [generate] Extracted diff/script ({} chars, {} lines)",
            diff.len(), diff.lines().count()
        );
        debug!(
            evolution_id = %evolution_id,
            "📝 [generate] Extracted content:\n{}",
            diff
        );

        let patch = GeneratedPatch {
            patch_id: format!("patch_{}", chrono::Utc::now().timestamp()),
            skill_name: record.skill_name.clone(),
            diff,
            explanation: response.clone(),
            generated_at: chrono::Utc::now().timestamp(),
        };

        record.patch = Some(patch.clone());
        record.status = EvolutionStatus::Generated;
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)?;

        info!(
            evolution_id = %evolution_id,
            patch_id = %patch.patch_id,
            "📝 [generate] Patch saved, status -> Generated"
        );

        Ok(patch)
    }

    /// 根据反馈重新生成补丁（用于审计/编译/测试失败后的重试）
    pub async fn regenerate_with_feedback(
        &self,
        evolution_id: &str,
        llm_provider: &dyn LLMProvider,
        feedback: &FeedbackEntry,
    ) -> Result<GeneratedPatch> {
        let mut record = self.load_record(evolution_id)?;
        record.attempt += 1;
        record.feedback_history.push(feedback.clone());
        record.status = EvolutionStatus::Generating;
        self.save_record(&record)?;

        info!(
            evolution_id = %evolution_id,
            attempt = record.attempt,
            feedback_stage = %feedback.stage,
            "🔄 [regenerate] Attempt #{}: regenerating after {} failure",
            record.attempt, feedback.stage
        );

        // 构建修复 prompt
        let prompt = self.build_fix_prompt(&record.context, feedback, &record.feedback_history)?;

        info!(
            evolution_id = %evolution_id,
            prompt_len = prompt.len(),
            "🔄 [regenerate] Fix prompt built"
        );
        debug!(
            evolution_id = %evolution_id,
            "🔄 [regenerate] Full fix prompt:\n{}",
            prompt
        );

        // 调用 LLM（带超时保护）
        info!(evolution_id = %evolution_id, "🔄 [regenerate] Calling LLM...");
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(self.llm_timeout_secs),
            llm_provider.generate(&prompt)
        )
        .await
        .map_err(|_| Error::Evolution(format!("LLM call timed out after {} seconds", self.llm_timeout_secs)))?
        .map_err(|e| Error::Evolution(format!("LLM generation failed: {}", e)))?;

        info!(
            evolution_id = %evolution_id,
            response_len = response.len(),
            "🔄 [regenerate] LLM response received"
        );
        debug!(
            evolution_id = %evolution_id,
            "🔄 [regenerate] Full LLM response:\n{}",
            response
        );

        // 解析 diff
        let diff = self.extract_diff_from_response(&response)?;

        info!(
            evolution_id = %evolution_id,
            diff_len = diff.len(),
            diff_lines = diff.lines().count(),
            "🔄 [regenerate] Extracted fixed script ({} chars, {} lines)",
            diff.len(), diff.lines().count()
        );
        debug!(
            evolution_id = %evolution_id,
            "🔄 [regenerate] Extracted content:\n{}",
            diff
        );

        let patch = GeneratedPatch {
            patch_id: format!("patch_{}_{}", chrono::Utc::now().timestamp(), record.attempt),
            skill_name: record.skill_name.clone(),
            diff,
            explanation: response.clone(),
            generated_at: chrono::Utc::now().timestamp(),
        };

        record.patch = Some(patch.clone());
        record.audit = None;       // 清除旧审计结果
        record.shadow_test = None;  // 清除旧测试结果
        record.observation = None;  // 清除观察窗口配置，确保状态一致性
        record.status = EvolutionStatus::Generated;
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)?;

        info!(
            evolution_id = %evolution_id,
            patch_id = %patch.patch_id,
            attempt = record.attempt,
            "🔄 [regenerate] New patch saved, status -> Generated"
        );

        Ok(patch)
    }

    /// 审计补丁（独立 LLM 会话）
    ///
    /// P0-1 fix: 审计基于应用后的完整脚本，而非原始 patch.diff
    pub async fn audit_patch(
        &self,
        evolution_id: &str,
        llm_provider: &dyn LLMProvider,
    ) -> Result<AuditResult> {
        let mut record = self.load_record(evolution_id)?;
        record.status = EvolutionStatus::Auditing;
        self.save_record(&record)?;

        let patch = record.patch.as_ref()
            .ok_or_else(|| Error::Evolution("No patch to audit".to_string()))?;

        info!(evolution_id = %evolution_id, "Auditing patch");

        // P0-1: 解析最终脚本内容用于审计（而非 diff 文本）
        let final_script = self.resolve_final_script(&record.skill_name, &patch.diff)?;

        let prompt = self.build_audit_prompt(&record.context, &final_script)?;

        info!(
            evolution_id = %evolution_id,
            prompt_len = prompt.len(),
            "🔍 [audit] Audit prompt built"
        );
        debug!(
            evolution_id = %evolution_id,
            "🔍 [audit] Full audit prompt:\n{}",
            prompt
        );

        info!(evolution_id = %evolution_id, "🔍 [audit] Calling LLM...");
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(self.llm_timeout_secs),
            llm_provider.generate(&prompt)
        )
        .await
        .map_err(|_| Error::Evolution(format!("LLM call timed out after {} seconds", self.llm_timeout_secs)))?
        .map_err(|e| Error::Evolution(format!("LLM generation failed: {}", e)))?;

        info!(
            evolution_id = %evolution_id,
            response_len = response.len(),
            "🔍 [audit] LLM response received"
        );
        debug!(
            evolution_id = %evolution_id,
            "🔍 [audit] Full LLM response:\n{}",
            response
        );

        let audit_result = self.parse_audit_response(&response)?;

        info!(
            evolution_id = %evolution_id,
            passed = audit_result.passed,
            issues_count = audit_result.issues.len(),
            "🔍 [audit] Audit result: passed={}, issues={}",
            audit_result.passed, audit_result.issues.len()
        );
        for (i, issue) in audit_result.issues.iter().enumerate() {
            info!(
                evolution_id = %evolution_id,
                "🔍 [audit]   Issue #{}: [{}][{}] {}",
                i + 1, issue.severity, issue.category, issue.message
            );
        }

        record.audit = Some(audit_result.clone());
        let new_status = if audit_result.passed {
            EvolutionStatus::AuditPassed
        } else {
            EvolutionStatus::AuditFailed
        };
        info!(
            evolution_id = %evolution_id,
            "🔍 [audit] Status -> {:?}",
            new_status
        );
        record.status = new_status;
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)?;

        Ok(audit_result)
    }

    /// 编译检查（合并了原 dry_run + shadow_test）
    ///
    /// P0-3: 单一编译步骤，返回 (是否通过, 编译错误信息)
    pub async fn compile_check(&self, evolution_id: &str) -> Result<(bool, Option<String>)> {
        let mut record = self.load_record(evolution_id)?;
        let patch = record.patch.as_ref()
            .ok_or_else(|| Error::Evolution("No patch for compile check".to_string()))?;

        info!(evolution_id = %evolution_id, "Running compile check");

        // 解析最终脚本内容
        let final_script = self.resolve_final_script(&record.skill_name, &patch.diff)?;

        // 写入临时文件
        let temp_path = std::env::temp_dir().join(format!("{}_compile.rhai", record.skill_name));
        std::fs::write(&temp_path, &final_script)?;

        info!(
            evolution_id = %evolution_id,
            content_len = final_script.len(),
            content_lines = final_script.lines().count(),
            "🔨 [compile] Script: {} chars, {} lines",
            final_script.len(), final_script.lines().count()
        );
        debug!(
            evolution_id = %evolution_id,
            "🔨 [compile] Script content:\n{}",
            final_script
        );

        // 编译检查
        info!(evolution_id = %evolution_id, "🔨 [compile] Compiling with Rhai engine...");
        let (passed, compile_error) = self.compile_skill(&temp_path).await?;

        // 清理临时文件
        let _ = std::fs::remove_file(&temp_path);

        info!(
            evolution_id = %evolution_id,
            passed = passed,
            "🔨 [compile] Result: {}",
            if passed { "PASSED" } else { "FAILED" }
        );
        if let Some(ref err) = compile_error {
            info!(
                evolution_id = %evolution_id,
                "🔨 [compile] Error: {}",
                err
            );
        }

        // 如果编译通过，还检查测试 fixtures
        if passed {
            let tests_dir = self.skills_dir.join(&record.skill_name).join("tests");
            if tests_dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&tests_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().is_some_and(|e| e == "json") {
                            if let Ok(fixture_content) = std::fs::read_to_string(&path) {
                                if serde_json::from_str::<serde_json::Value>(&fixture_content).is_err() {
                                    let err_msg = format!(
                                        "Invalid test fixture JSON: {}",
                                        path.file_name().unwrap_or_default().to_string_lossy()
                                    );
                                    warn!(evolution_id = %evolution_id, "🔨 [compile] {}", err_msg);
                                    record.status = EvolutionStatus::CompileFailed;
                                    record.updated_at = chrono::Utc::now().timestamp();
                                    self.save_record(&record)?;
                                    return Ok((false, Some(err_msg)));
                                }
                            }
                        }
                    }
                }
            }
        }

        let new_status = if passed {
            EvolutionStatus::CompilePassed
        } else {
            EvolutionStatus::CompileFailed
        };
        info!(
            evolution_id = %evolution_id,
            "🔨 [compile] Status -> {:?}",
            new_status
        );
        record.status = new_status;
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)?;

        Ok((passed, compile_error))
    }

    /// 部署新版本并进入观察窗口
    ///
    /// P1: 简化模型 — 直接部署，进入观察期（无灰度百分比分流）
    pub async fn deploy_and_observe(&self, evolution_id: &str) -> Result<()> {
        let mut record = self.load_record(evolution_id)?;
        
        // 检查前置条件（兼容旧状态 DryRunPassed/TestPassed）
        if !record.status.is_compile_passed() {
            return Err(Error::Evolution(format!(
                "Cannot deploy: expected status CompilePassed, got {:?}",
                record.status
            )));
        }
        if record.audit.as_ref().map(|a| !a.passed).unwrap_or(true) {
            return Err(Error::Evolution("Audit not passed".to_string()));
        }

        info!(evolution_id = %evolution_id, "Deploying and starting observation");
        info!(
            evolution_id = %evolution_id,
            skill = %record.skill_name,
            "🚀 [deploy] Pre-conditions met, deploying new version"
        );

        // 创建新版本（直接写入）
        self.create_new_version(&record)?;

        // 设置观察窗口
        record.observation = Some(ObservationWindow::default());
        record.status = EvolutionStatus::Observing;
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)?;

        info!(
            evolution_id = %evolution_id,
            skill = %record.skill_name,
            "🚀 [deploy] Version deployed, observation window started (60 min)"
        );

        Ok(())
    }

    /// 检查观察窗口状态
    ///
    /// 返回: Ok(Some(true)) = 观察完成可标记成功, Ok(Some(false)) = 需要回滚, Ok(None) = 仍在观察中
    pub fn check_observation(&self, evolution_id: &str, error_rate: f64) -> Result<Option<bool>> {
        let record = self.load_record(evolution_id)?;
        
        let obs = record.observation.as_ref()
            .ok_or_else(|| Error::Evolution("No observation window".to_string()))?;

        // 错误率超阈值 → 回滚
        if error_rate > obs.error_threshold {
            return Ok(Some(false));
        }

        // 观察时间到且错误率正常 → 完成
        let elapsed_minutes = (chrono::Utc::now().timestamp() - obs.started_at) / 60;
        if elapsed_minutes >= obs.duration_minutes as i64 {
            return Ok(Some(true));
        }

        // 仍在观察中
        Ok(None)
    }

    /// 标记进化完成
    pub fn mark_completed(&self, evolution_id: &str) -> Result<()> {
        let mut record = self.load_record(evolution_id)?;
        record.status = EvolutionStatus::Completed;
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)?;
        Ok(())
    }

    /// 回滚
    pub async fn rollback(&self, evolution_id: &str, reason: &str) -> Result<()> {
        let mut record = self.load_record(evolution_id)?;
        
        warn!(
            evolution_id = %evolution_id,
            reason = %reason,
            "Rolling back evolution"
        );

        // 恢复到上一版本
        self.restore_previous_version(&record.skill_name)?;

        record.status = EvolutionStatus::RolledBack;
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)?;

        Ok(())
    }

    // === 辅助方法 ===

    fn build_generation_prompt(&self, context: &EvolutionContext) -> Result<String> {
        let has_existing_source = context.source_snippet.is_some();
        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });

        let mut prompt = String::new();

        // System context: Rhai language
        prompt.push_str("You are a Rhai skill evolution assistant for the blockcell agent framework.\n");
        prompt.push_str("All skills MUST be written in the Rhai scripting language (.rhai files).\n");
        prompt.push_str("Do NOT generate JavaScript, Python, TypeScript, or any other language.\n\n");

        prompt.push_str("## Rhai Language Quick Reference\n");
        prompt.push_str("- Variables: `let x = 42;` (immutable by default), `let x = 42; x = 100;` (reassign ok)\n");
        prompt.push_str("- Strings: `let s = \"hello\";` with interpolation `\"value: ${x}\"`\n");
        prompt.push_str("- Arrays: `let a = [1, 2, 3];` Maps: `let m = #{x: 1, y: 2};`\n");
        prompt.push_str("- Functions: `fn add(a, b) { a + b }`\n");
        prompt.push_str("- Control: `if x > 0 { } else { }`, `for i in 0..10 { }`, `while x > 0 { }`\n");
        prompt.push_str("- String methods: `.len()`, `.contains()`, `.split()`, `.trim()`, `.to_upper()`, `.to_lower()`\n");
        prompt.push_str("- Array methods: `.push()`, `.pop()`, `.len()`, `.filter()`, `.map()`\n");
        prompt.push_str("- No classes/structs — use maps (object maps) `#{}` instead\n");
        prompt.push_str("- No `import`/`require` — all capabilities come from the host engine\n");
        prompt.push_str("- Print: `print(\"msg\");`\n\n");

        // Task description
        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                prompt.push_str(&format!("## Task\nCreate or improve a Rhai skill for: {}\n\n", description));
            }
        } else {
            prompt.push_str(&format!("## Task\nFix the following issue in Rhai skill '{}':\n\n", context.skill_name));
            prompt.push_str(&format!("Trigger: {:?}\n\n", context.trigger));
        }

        if let Some(error) = &context.error_stack {
            prompt.push_str(&format!("## Error\n```\n{}\n```\n\n", error));
        }

        // Existing source code
        if let Some(snippet) = &context.source_snippet {
            prompt.push_str(&format!("## Current SKILL.rhai Source\n```rhai\n{}\n```\n\n", snippet));
        }

        if !context.tool_schemas.is_empty() {
            prompt.push_str("## Available Host Tools\n");
            for tool in &context.tool_schemas {
                prompt.push_str(&format!("- {}\n", tool));
            }
            prompt.push('\n');
        }

        // Output format — P0-2: always request complete script (never diff)
        prompt.push_str("## Output Format\n");
        prompt.push_str("Generate the COMPLETE SKILL.rhai file content.\n");
        prompt.push_str("Output ONLY the Rhai code in a ```rhai code block.\n");
        prompt.push_str("The script must be a valid, self-contained Rhai script with no syntax errors.\n");
        let _ = has_existing_source; // suppress unused warning

        Ok(prompt)
    }

    fn build_fix_prompt(
        &self,
        context: &EvolutionContext,
        current_feedback: &FeedbackEntry,
        history: &[FeedbackEntry],
    ) -> Result<String> {
        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });

        let mut prompt = String::new();

        // System context
        prompt.push_str("You are a Rhai skill evolution assistant for the blockcell agent framework.\n");
        prompt.push_str("All skills MUST be written in the Rhai scripting language (.rhai files).\n");
        prompt.push_str("Do NOT generate JavaScript, Python, TypeScript, or any other language.\n\n");

        prompt.push_str("## Rhai Language Quick Reference\n");
        prompt.push_str("- Variables: `let x = 42;` (immutable by default), `let x = 42; x = 100;` (reassign ok)\n");
        prompt.push_str("- Strings: `let s = \"hello\";` with interpolation `\"value: ${x}\"`\n");
        prompt.push_str("- Arrays: `let a = [1, 2, 3];` Maps: `let m = #{x: 1, y: 2};`\n");
        prompt.push_str("- Functions: `fn add(a, b) { a + b }`\n");
        prompt.push_str("- Control: `if x > 0 { } else { }`, `for i in 0..10 { }`, `while x > 0 { }`\n");
        prompt.push_str("- String methods: `.len()`, `.contains()`, `.split()`, `.trim()`, `.to_upper()`, `.to_lower()`\n");
        prompt.push_str("- Array methods: `.push()`, `.pop()`, `.len()`, `.filter()`, `.map()`\n");
        prompt.push_str("- Map access: `m.key` or `m[\"key\"]`, check existence with `\"key\" in m`\n");
        prompt.push_str("- Null coalescing: `value ?? default` (use instead of .get with default)\n");
        prompt.push_str("- Type conversion: `.to_string()`, `.to_int()`, `.to_float()`\n");
        prompt.push_str("- String concat: use `+` only between strings, convert numbers with `.to_string()` first\n");
        prompt.push_str("- No classes/structs — use maps (object maps) `#{}` instead\n");
        prompt.push_str("- No `import`/`require` — all capabilities come from the host engine\n");
        prompt.push_str("- Print: `print(\"msg\");`\n\n");

        // Task description
        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                prompt.push_str(&format!("## Original Task\nCreate or improve a Rhai skill for: {}\n\n", description));
            }
        } else {
            prompt.push_str(&format!("## Original Task\nFix the following issue in Rhai skill '{}':\n\n", context.skill_name));
        }

        // Previous code that had issues
        prompt.push_str("## Previous Code (has issues)\n");
        prompt.push_str(&format!("```rhai\n{}\n```\n\n", current_feedback.previous_code));

        // Current feedback
        prompt.push_str(&format!("## Issues Found ({})\n", current_feedback.stage));
        prompt.push_str(&format!("{}\n\n", current_feedback.feedback));

        // Show history of previous attempts if any (excluding current)
        let prev_attempts: Vec<&FeedbackEntry> = history.iter()
            .filter(|h| h.attempt < current_feedback.attempt)
            .collect();
        if !prev_attempts.is_empty() {
            prompt.push_str("## Previous Attempt History\n");
            prompt.push_str("The following issues were found in earlier attempts. Make sure NOT to repeat them:\n\n");
            for entry in prev_attempts {
                prompt.push_str(&format!("### Attempt #{} ({} failure)\n", entry.attempt, entry.stage));
                prompt.push_str(&format!("{}\n\n", entry.feedback));
            }
        }

        // Output format
        prompt.push_str("## Instructions\n");
        prompt.push_str("Fix ALL the issues listed above and generate the COMPLETE corrected Rhai script.\n");
        prompt.push_str("Do NOT leave any of the reported issues unfixed.\n");
        prompt.push_str("Output ONLY the corrected Rhai code in a ```rhai code block.\n");
        prompt.push_str("The script must be a valid, self-contained Rhai script with no syntax errors.\n");

        Ok(prompt)
    }

    fn build_audit_prompt(&self, context: &EvolutionContext, script_content: &str) -> Result<String> {
        let mut prompt = String::new();

        prompt.push_str(&format!(
            "You are a security auditor for Rhai scripts in the blockcell agent framework.\n\
            Review the following complete script for skill '{}'.\n\n",
            context.skill_name
        ));

        prompt.push_str(&format!("Code:\n```rhai\n{}\n```\n\n", script_content));

        prompt.push_str("\
Check for the following Rhai-specific issues:\n\
1. **Syntax errors**: Is this valid Rhai syntax? (No JS/Python/TS syntax like `class`, `import`, `require`, `const`, `=>`, `async`)\n\
2. **Language correctness**: Uses Rhai idioms (object maps `#{}`, `fn` for functions, `let` for variables)\n\
3. **Infinite loops**: Unbounded `loop {}` or `while true {}` without break conditions\n\
4. **Resource abuse**: Operations that could consume excessive memory or CPU\n\
5. **Data leakage**: Logging sensitive information via `print()`\n\n\
Respond with ONLY a JSON object (no markdown code blocks, no extra text):\n\
{\"passed\": true, \"issues\": []}\n\
or\n\
{\"passed\": false, \"issues\": [{\"severity\": \"error\", \"category\": \"syntax\", \"message\": \"description\"}]}\n");

        Ok(prompt)
    }

    fn extract_diff_from_response(&self, response: &str) -> Result<String> {
        // Try ```diff block first (for patching existing skills)
        if let Some(start) = response.find("```diff") {
            let after_marker = start + 7;
            if let Some(end) = response[after_marker..].find("```") {
                let diff = &response[after_marker..after_marker + end];
                return Ok(diff.trim().to_string());
            }
        }

        // Try ```rhai block (for new skill creation — full script output)
        if let Some(start) = response.find("```rhai") {
            let after_marker = start + 7;
            if let Some(end) = response[after_marker..].find("```") {
                let script = &response[after_marker..after_marker + end];
                return Ok(script.trim().to_string());
            }
        }

        // Try generic ``` block
        if let Some(start) = response.find("```") {
            let after_marker = start + 3;
            let content_start = response[after_marker..].find('\n')
                .map(|i| after_marker + i + 1)
                .unwrap_or(after_marker);
            if let Some(end) = response[content_start..].find("```") {
                let content = &response[content_start..content_start + end];
                return Ok(content.trim().to_string());
            }
        }

        // Fallback: entire response
        Ok(response.trim().to_string())
    }

    fn parse_audit_response(&self, response: &str) -> Result<AuditResult> {
        // Extract JSON from ```json code blocks if present
        let json_str = if let Some(start) = response.find("```json") {
            let after_marker = start + 7;
            if let Some(end) = response[after_marker..].find("```") {
                response[after_marker..after_marker + end].trim()
            } else {
                response.trim()
            }
        } else if let Some(start) = response.find("```") {
            let after_marker = start + 3;
            // Skip optional language tag on same line
            let content_start = response[after_marker..].find('\n')
                .map(|i| after_marker + i + 1)
                .unwrap_or(after_marker);
            if let Some(end) = response[content_start..].find("```") {
                response[content_start..content_start + end].trim()
            } else {
                response.trim()
            }
        } else {
            response.trim()
        };

        let parsed: serde_json::Value = serde_json::from_str(json_str)
            .map_err(|e| Error::Evolution(format!("Failed to parse audit response: {}", e)))?;

        let passed = parsed["passed"].as_bool().unwrap_or(false);
        let empty_vec = vec![];
        let issues_json = parsed["issues"].as_array().unwrap_or(&empty_vec);

        let issues = issues_json
            .iter()
            .filter_map(|i| {
                Some(AuditIssue {
                    severity: i["severity"].as_str()?.to_string(),
                    category: i["category"].as_str()?.to_string(),
                    message: i["message"].as_str()?.to_string(),
                })
            })
            .collect();

        Ok(AuditResult {
            passed,
            issues,
            audited_at: chrono::Utc::now().timestamp(),
        })
    }

    /// 解析最终脚本内容
    ///
    /// P0-2: 由于所有生成都输出完整脚本，这里直接返回 patch.diff 内容。
    /// 保留此方法作为统一入口，便于未来扩展。
    fn resolve_final_script(&self, _skill_name: &str, script_content: &str) -> Result<String> {
        Ok(script_content.to_string())
    }

    /// 编译 Rhai 脚本，返回 (是否成功, 错误信息)
    async fn compile_skill(&self, skill_path: &Path) -> Result<(bool, Option<String>)> {
        let engine = rhai::Engine::new();
        let content = std::fs::read_to_string(skill_path)?;
        
        match engine.compile(&content) {
            Ok(_ast) => {
                info!("🔨 [compile] Rhai compilation succeeded");
                Ok((true, None))
            }
            Err(e) => {
                let error_msg = format!("{}", e);
                warn!(
                    error = %e,
                    "🔨 [compile] Rhai compilation FAILED: {}",
                    e
                );
                Ok((false, Some(error_msg)))
            }
        }
    }

    /// P0-2: create_new_version 直接写入完整脚本（不再 apply diff）
    fn create_new_version(&self, record: &EvolutionRecord) -> Result<()> {
        let patch = record.patch.as_ref()
            .ok_or_else(|| Error::Evolution("No patch to deploy".to_string()))?;

        let skill_dir = self.skills_dir.join(&record.skill_name);
        let skill_path = skill_dir.join("SKILL.rhai");

        // Ensure skill directory exists (for new skills)
        std::fs::create_dir_all(&skill_dir)?;

        // 直接写入完整脚本（所有生成都是完整脚本）
        std::fs::write(&skill_path, &patch.diff)?;

        // 通过 VersionManager 创建版本快照
        let changelog = Some(format!(
            "Evolution {}: {}",
            record.id, patch.explanation.chars().take(200).collect::<String>()
        ));
        let version = self.version_manager.create_version(
            &record.skill_name,
            VersionSource::Evolution,
            changelog,
        )?;

        info!(
            skill = %record.skill_name,
            version = %version.version,
            "New skill version deployed via evolution"
        );

        Ok(())
    }

    fn restore_previous_version(&self, skill_name: &str) -> Result<()> {
        self.version_manager.rollback(skill_name)
            .map_err(|e| Error::Evolution(format!("Rollback failed: {}", e)))
    }

    pub fn save_record_public(&self, record: &EvolutionRecord) -> Result<()> {
        self.save_record(record)
    }

    /// P2-7: 原子写入 — write-tmp-then-rename，避免崩溃时文件损坏
    fn save_record(&self, record: &EvolutionRecord) -> Result<()> {
        let records_dir = self.evolution_db.parent().unwrap().join("evolution_records");
        std::fs::create_dir_all(&records_dir)?;
        
        let record_file = records_dir.join(format!("{}.json", record.id));
        let temp_file = records_dir.join(format!("{}.json.tmp", record.id));
        let json = serde_json::to_string_pretty(record)?;
        
        // 先写入临时文件
        std::fs::write(&temp_file, &json)?;
        // 原子重命名（同一文件系统上是原子操作）
        std::fs::rename(&temp_file, &record_file)?;
        
        Ok(())
    }

    pub fn load_record(&self, evolution_id: &str) -> Result<EvolutionRecord> {
        let records_dir = self.evolution_db.parent().unwrap().join("evolution_records");
        let record_file = records_dir.join(format!("{}.json", evolution_id));
        
        let json = std::fs::read_to_string(record_file)?;
        let record = serde_json::from_str(&json)?;
        
        Ok(record)
    }
}

// === Trait 定义 ===

#[async_trait::async_trait]
pub trait LLMProvider: Send + Sync {
    async fn generate(&self, prompt: &str) -> Result<String>;
}

