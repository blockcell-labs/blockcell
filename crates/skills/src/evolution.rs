use crate::versioning::{VersionManager, VersionSource};
use blockcell_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, info, warn};

static RECORD_TMP_COUNTER: AtomicU64 = AtomicU64::new(1);

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

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

/// 技能类型：决定进化 pipeline 的行为
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum SkillType {
    /// Rhai 脚本技能（需要 SKILL.rhai 编译检查）
    #[default]
    Rhai,
    /// 纯 prompt 技能（meta.yaml + SKILL.md，无脚本）
    PromptOnly,
    /// Python 脚本技能（SKILL.py，需要 Python 语法检查）
    Python,
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
    /// 技能类型（Rhai 脚本 or 纯 Prompt），默认为 Rhai
    #[serde(default)]
    pub skill_type: SkillType,

    /// If true, this evolution is operating on a staged external skill install.
    /// The skill should be promoted (moved) into the main skills_dir when deployment
    /// reaches Observing.
    #[serde(default)]
    pub staged: bool,

    /// Workspace directory used for staged external skill installs (e.g. ~/.blockcell/workspace/import_staging/skills).
    /// When staged=true, the pipeline writes files into this directory first.
    #[serde(default)]
    pub staging_skills_dir: Option<String>,
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
    pub stage: String,         // "audit", "compile", "test"
    pub feedback: String,      // 具体的错误/问题描述
    pub previous_code: String, // 上一次生成的代码
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

fn default_attempt() -> u32 {
    1
}

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
            EvolutionStatus::DryRunPassed | EvolutionStatus::TestPassed => {
                &EvolutionStatus::CompilePassed
            }
            EvolutionStatus::DryRunFailed
            | EvolutionStatus::TestFailed
            | EvolutionStatus::Testing => &EvolutionStatus::CompileFailed,
            EvolutionStatus::RollingOut => &EvolutionStatus::Observing,
            other => other,
        }
    }

    /// 检查状态是否等价于 CompilePassed（包括旧状态）
    pub fn is_compile_passed(&self) -> bool {
        matches!(
            self,
            EvolutionStatus::CompilePassed
                | EvolutionStatus::DryRunPassed
                | EvolutionStatus::TestPassed
        )
    }
}

impl SkillEvolution {
    pub fn new(skills_dir: PathBuf, llm_timeout_secs: u64) -> Self {
        let evolution_db = skills_dir
            .parent()
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

    fn is_openclaw_import_description(description: &str) -> bool {
        description.contains("Convert the following OpenClaw-compatible skill into a Blockcell")
    }

    fn trigger_rules_prompt() -> &'static str {
        "## meta.yaml trigger rules\n\
- Generate `triggers` as a concise YAML string list.\n\
- Generate **4 to 8** triggers total. Do not output fewer than 4 or more than 8.\n\
- Each trigger should be **2 to 8 Chinese characters** or **1 to 3 English words**.\n\
- Prefer **Chinese natural-language triggers** when the skill is mainly for Chinese users. Add **0 to 2 English triggers** only when the capability is commonly searched in English or the skill name itself is English.\n\
- Include **1 exact skill-name trigger** or a very close stable alias.\n\
- Include **2 to 4 user-intent triggers** that reflect how users actually ask for this capability in chat.\n\
- Include **1 to 2 narrow synonyms or aliases** only when they are genuinely equivalent in meaning. Do not pad with weak variants.\n\
- Avoid overly broad or generic triggers such as `查询`, `工具`, `助手`, `分析`, `处理`, `搜索`, `生成`, `数据`, `信息`, `market`, `crypto` unless the phrase is paired with the skill's specific domain intent.\n\
- Avoid triggers that overlap too broadly with unrelated skills. Triggers must be specific enough to activate this skill but not so specific that normal user wording would never match.\n\
- Prefer wording that matches **actual user utterances**, not taxonomy labels. Good triggers sound like something a user would really type.\n\
- Do not include punctuation, full sentences, repeated variants, or explanation text in `triggers`.\n\
- Keep triggers accurate and conservative. Do **not** over-expand with speculative synonyms.\n\n"
    }

    /// Get the evolution records directory path.
    pub fn records_dir(&self) -> PathBuf {
        self.evolution_db
            .parent()
            .unwrap()
            .join("evolution_records")
    }

    fn skill_root_dir_for_record(&self, record: &EvolutionRecord) -> PathBuf {
        if record.context.staged {
            if let Some(ref dir) = record.context.staging_skills_dir {
                let p = PathBuf::from(dir);
                if p.is_absolute() {
                    return p;
                }
            }
        }
        self.skills_dir.clone()
    }

    /// Load the current skill source for a skill (returns None if not found).
    /// Checks SKILL.rhai, SKILL.py, and SKILL.md in that order.
    pub fn load_skill_source(&self, skill_name: &str) -> Result<Option<String>> {
        let skill_dir = self.skills_dir.join(skill_name);
        for filename in &["SKILL.rhai", "SKILL.py", "SKILL.md"] {
            let path = skill_dir.join(filename);
            if path.exists() {
                return Ok(std::fs::read_to_string(&path).ok());
            }
        }
        Ok(None)
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
            llm_provider.generate(&prompt),
        )
        .await
        .map_err(|_| {
            Error::Evolution(format!(
                "LLM call timed out after {} seconds",
                self.llm_timeout_secs
            ))
        })?
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
            llm_provider.generate(&prompt),
        )
        .await
        .map_err(|_| {
            Error::Evolution(format!(
                "LLM call timed out after {} seconds",
                self.llm_timeout_secs
            ))
        })?
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
            patch_id: format!(
                "patch_{}_{}",
                chrono::Utc::now().timestamp(),
                record.attempt
            ),
            skill_name: record.skill_name.clone(),
            diff,
            explanation: response.clone(),
            generated_at: chrono::Utc::now().timestamp(),
        };

        record.patch = Some(patch.clone());
        record.audit = None; // 清除旧审计结果
        record.shadow_test = None; // 清除旧测试结果
        record.observation = None; // 清除观察窗口配置，确保状态一致性
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

        let patch = record
            .patch
            .as_ref()
            .ok_or_else(|| Error::Evolution("No patch to audit".to_string()))?;

        info!(evolution_id = %evolution_id, "Auditing patch");

        // P0-1: 解析最终脚本内容用于审计（而非 diff 文本）
        let final_script = self.resolve_final_script(&record.skill_name, &patch.diff)?;

        let prompt = if record.context.skill_type == SkillType::PromptOnly {
            self.build_prompt_only_audit_prompt(&record.context, &final_script)?
        } else if record.context.skill_type == SkillType::Python {
            self.build_python_audit_prompt(&record.context, &final_script)?
        } else {
            self.build_audit_prompt(&record.context, &final_script)?
        };

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
            llm_provider.generate(&prompt),
        )
        .await
        .map_err(|_| {
            Error::Evolution(format!(
                "LLM call timed out after {} seconds",
                self.llm_timeout_secs
            ))
        })?
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
        let patch = record
            .patch
            .as_ref()
            .ok_or_else(|| Error::Evolution("No patch for compile check".to_string()))?;

        info!(evolution_id = %evolution_id, "Running compile check");

        // PromptOnly 技能跳过 Rhai 编译，只检查内容非空
        if record.context.skill_type == SkillType::PromptOnly {
            info!(evolution_id = %evolution_id, "🔨 [compile] PromptOnly skill — skipping Rhai compile, checking content length");
            let content = patch.diff.trim();
            let (passed, error) = if content.is_empty() {
                (false, Some("SKILL.md content is empty".to_string()))
            } else if content.len() < 50 {
                (
                    false,
                    Some(format!(
                        "SKILL.md content too short ({} chars, need >= 50)",
                        content.len()
                    )),
                )
            } else {
                (true, None)
            };
            let new_status = if passed {
                EvolutionStatus::CompilePassed
            } else {
                EvolutionStatus::CompileFailed
            };
            info!(evolution_id = %evolution_id, passed = passed, "🔨 [compile] PromptOnly content check: {}", if passed { "PASSED" } else { "FAILED" });
            record.status = new_status;
            record.updated_at = chrono::Utc::now().timestamp();
            self.save_record(&record)?;
            return Ok((passed, error));
        }

        // Python 技能使用 python3 -m py_compile 进行语法检查
        if record.context.skill_type == SkillType::Python {
            info!(evolution_id = %evolution_id, "🔨 [compile] Python skill — running py_compile syntax check");
            let final_script = self.resolve_final_script(&record.skill_name, &patch.diff)?;
            let temp_path = std::env::temp_dir().join(format!("{}_compile.py", record.skill_name));
            std::fs::write(&temp_path, &final_script)?;

            let output = std::process::Command::new("python3")
                .args(["-m", "py_compile", temp_path.to_str().unwrap_or("")])
                .output();

            let _ = std::fs::remove_file(&temp_path);

            let (passed, error) = match output {
                Ok(out) if out.status.success() => (true, None),
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                    (false, Some(format!("Python syntax error:\n{}", stderr)))
                }
                Err(e) => {
                    warn!(evolution_id = %evolution_id, "🔨 [compile] python3 not found, skipping syntax check: {}", e);
                    (true, None) // python3 not available — skip check
                }
            };

            let new_status = if passed {
                EvolutionStatus::CompilePassed
            } else {
                EvolutionStatus::CompileFailed
            };
            info!(evolution_id = %evolution_id, passed = passed, "🔨 [compile] Python syntax check: {}", if passed { "PASSED" } else { "FAILED" });
            record.status = new_status;
            record.updated_at = chrono::Utc::now().timestamp();
            self.save_record(&record)?;
            return Ok((passed, error));
        }

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
            let tests_dir = self
                .skill_root_dir_for_record(&record)
                .join(&record.skill_name)
                .join("tests");
            if tests_dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&tests_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().is_some_and(|e| e == "json") {
                            if let Ok(fixture_content) = std::fs::read_to_string(&path) {
                                if serde_json::from_str::<serde_json::Value>(&fixture_content)
                                    .is_err()
                                {
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

        let obs = record
            .observation
            .as_ref()
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
        if context.skill_type == SkillType::PromptOnly {
            return self.build_prompt_only_generation_prompt(context);
        }
        if context.skill_type == SkillType::Python {
            return self.build_python_generation_prompt(context);
        }

        let has_existing_source = context.source_snippet.is_some();
        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });

        let mut prompt = String::new();

        // System context: Rhai language
        prompt.push_str(
            "You are a Rhai skill evolution assistant for the blockcell agent framework.\n",
        );
        prompt
            .push_str("All skills MUST be written in the Rhai scripting language (.rhai files).\n");
        prompt
            .push_str("Do NOT generate JavaScript, Python, TypeScript, or any other language.\n\n");

        prompt.push_str("## Rhai Language Quick Reference\n");
        prompt.push_str("- Variables: `let x = 42;` (immutable by default), `let x = 42; x = 100;` (reassign ok)\n");
        prompt.push_str("- Strings: `let s = \"hello\";` with interpolation `\"value: ${x}\"`\n");
        prompt.push_str("- Arrays: `let a = [1, 2, 3];` Maps: `let m = #{x: 1, y: 2};`\n");
        prompt.push_str("- Functions: `fn add(a, b) { a + b }`\n");
        prompt.push_str(
            "- Control: `if x > 0 { } else { }`, `for i in 0..10 { }`, `while x > 0 { }`\n",
        );
        prompt.push_str("- String methods: `.len()`, `.contains()`, `.split()`, `.trim()`, `.to_upper()`, `.to_lower()`\n");
        prompt.push_str("- Array methods: `.push()`, `.pop()`, `.len()`, `.filter()`, `.map()`\n");
        prompt.push_str("- Built-in helpers: `len(value)`, `str_sub(text, start, len)`, `str_truncate(text, max_chars)`, `str_lines(text, max_lines)`, `arr_join(items, sep)`\n");
        prompt.push_str("- No classes/structs — use maps (object maps) `#{}` instead\n");
        prompt.push_str("- No `import`/`require` — all capabilities come from the host engine\n");
        prompt.push_str("- Print: `print(\"msg\");`\n\n");

        prompt.push_str("## Stable Built-in Helper Functions\n");
        prompt.push_str(
            "- `len(value)` -> length of string / array / map, returns 0 for null-like values\n",
        );
        prompt.push_str("- `str_sub(text, start, len)` -> safe substring by character index\n");
        prompt.push_str(
            "- `str_truncate(text, max_chars)` -> truncate text safely at character boundary\n",
        );
        prompt.push_str("- `str_lines(text, max_lines)` -> return the first N lines as an array\n");
        prompt.push_str("- `arr_join(items, sep)` -> join array items into a string\n\n");

        // Task description
        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                if Self::is_openclaw_import_description(description) {
                    prompt.push_str(&format!("## Task\n{}\n\n", description));
                } else {
                    prompt.push_str(&format!(
                        "## Task\nCreate or improve a Blockcell Rhai skill for: {}\n\n",
                        description
                    ));
                }
            }
        } else {
            prompt.push_str(&format!(
                "## Task\nFix the following issue in the existing Blockcell Rhai skill '{}'. Preserve the skill's purpose and only change what is necessary to correct the problem.\n\n",
                context.skill_name
            ));
            prompt.push_str(&format!("Trigger: {:?}\n\n", context.trigger));
        }

        if let Some(error) = &context.error_stack {
            prompt.push_str(&format!("## Error\n```\n{}\n```\n\n", error));
        }

        // Existing source code
        if let Some(snippet) = &context.source_snippet {
            prompt.push_str(&format!(
                "## Current SKILL.rhai Source\n```rhai\n{}\n```\n\n",
                snippet
            ));
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
        prompt.push_str("When the skill returns structured results, prefer returning `display_text` for final user-facing text. If the result still needs runtime/LLM polishing, return `summary_data` as a lightweight structured summary and keep large raw content out of `summary_data`.\n");
        prompt.push_str("If you output `meta.yaml`, it must follow the trigger rules below.\n\n");
        prompt.push_str(Self::trigger_rules_prompt());
        prompt.push_str("Output ONLY the Rhai code in a ```rhai code block.\n");
        prompt.push_str(
            "The script must be a valid, self-contained Rhai script with no syntax errors.\n",
        );
        let _ = has_existing_source; // suppress unused warning

        Ok(prompt)
    }

    fn build_prompt_only_generation_prompt(&self, context: &EvolutionContext) -> Result<String> {
        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });
        let mut prompt = String::new();

        prompt.push_str("You are a skill document writer for the blockcell agent framework.\n");
        prompt.push_str(
            "Your task is to write or improve a SKILL.md file — a prompt instruction document\n",
        );
        prompt.push_str(
            "that tells the AI agent how to handle specific user requests for this skill.\n\n",
        );

        prompt.push_str("## What is SKILL.md?\n");
        prompt.push_str("SKILL.md is an operation manual injected into the agent's system prompt when this skill is triggered.\n");
        prompt.push_str("It should contain:\n");
        prompt.push_str("- **Goal**: What the skill does and when it applies\n");
        prompt.push_str("- **Tools to use**: Which built-in tools to call and in what order\n");
        prompt.push_str("- **Output format**: What the final response should look like\n");
        prompt
            .push_str("- **Scenarios**: 2-4 concrete usage scenarios with step-by-step guidance\n");
        prompt.push_str("- **Fallback strategy**: What to do when tools fail\n\n");

        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                if Self::is_openclaw_import_description(description) {
                    prompt.push_str(&format!("## Task\n{}\n\n", description));
                } else {
                    prompt.push_str(&format!(
                        "## Task\nCreate or improve a Blockcell SKILL.md for: {}\n\n",
                        description
                    ));
                }
            }
        } else {
            prompt.push_str(&format!(
                "## Task\nFix the existing Blockcell SKILL.md for skill '{}' to address the following issue. Preserve the skill's original scope and intent; only tighten or correct the instructions as needed.\n\n",
                context.skill_name
            ));
            if let Some(error) = &context.error_stack {
                prompt.push_str(&format!("## Issue\n```\n{}\n```\n\n", error));
            }
        }

        if let Some(snippet) = &context.source_snippet {
            prompt.push_str(&format!(
                "## Current SKILL.md Content\n```markdown\n{}\n```\n\n",
                snippet
            ));
        }

        if context.staged {
            if let Some(ref staging_dir) = context.staging_skills_dir {
                let staged_root = std::path::PathBuf::from(staging_dir);
                let staged_skill_dir = staged_root.join(&context.skill_name);
                let staged_md = staged_skill_dir.join("SKILL.md");
                if let Ok(md) = std::fs::read_to_string(&staged_md) {
                    if !md.trim().is_empty() {
                        prompt.push_str("## Current Staged SKILL.md (reference)\n");
                        prompt.push_str(&format!("```markdown\n{}\n```\n\n", md));
                    }
                }
                let staged_meta = staged_skill_dir.join("meta.yaml");
                if let Ok(meta) = std::fs::read_to_string(&staged_meta) {
                    if !meta.trim().is_empty() {
                        prompt.push_str("## Current Staged meta.yaml (reference)\n");
                        prompt.push_str(&format!("```yaml\n{}\n```\n\n", meta));
                    }
                }
            }
        }

        prompt.push_str("## Result Contract\n");
        prompt.push_str("If the skill can directly produce final user-facing text, return `display_text`. Otherwise return `summary_data` as a lightweight structured summary for runtime/LLM polishing. Do NOT place complete webpages, long markdown, or large raw logs into `summary_data`.\n\n");
        prompt.push_str("## Output Format\n");
        prompt.push_str("Generate the COMPLETE SKILL.md content.\n");
        prompt.push_str("Output the markdown content in a ```markdown code block.\n");
        prompt.push_str("Also output an updated meta.yaml in a ```yaml code block.\n");
        prompt.push_str(Self::trigger_rules_prompt());
        prompt.push_str(
            "The document must be at least 200 characters, practical, and clearly structured.\n",
        );

        Ok(prompt)
    }

    fn build_fix_prompt(
        &self,
        context: &EvolutionContext,
        current_feedback: &FeedbackEntry,
        history: &[FeedbackEntry],
    ) -> Result<String> {
        if context.skill_type == SkillType::PromptOnly {
            return self.build_prompt_only_fix_prompt(context, current_feedback, history);
        }
        if context.skill_type == SkillType::Python {
            return self.build_python_fix_prompt(context, current_feedback, history);
        }

        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });

        let mut prompt = String::new();

        // System context
        prompt.push_str(
            "You are a Rhai skill evolution assistant for the blockcell agent framework.\n",
        );
        prompt
            .push_str("All skills MUST be written in the Rhai scripting language (.rhai files).\n");
        prompt
            .push_str("Do NOT generate JavaScript, Python, TypeScript, or any other language.\n\n");

        prompt.push_str("## Rhai Language Quick Reference\n");
        prompt.push_str("- Variables: `let x = 42;` (immutable by default), `let x = 42; x = 100;` (reassign ok)\n");
        prompt.push_str("- Strings: `let s = \"hello\";` with interpolation `\"value: ${x}\"`\n");
        prompt.push_str("- Arrays: `let a = [1, 2, 3];` Maps: `let m = #{x: 1, y: 2};`\n");
        prompt.push_str("- Functions: `fn add(a, b) { a + b }`\n");
        prompt.push_str(
            "- Control: `if x > 0 { } else { }`, `for i in 0..10 { }`, `while x > 0 { }`\n",
        );
        prompt.push_str("- String methods: `.len()`, `.contains()`, `.split()`, `.trim()`, `.to_upper()`, `.to_lower()`\n");
        prompt.push_str("- Array methods: `.push()`, `.pop()`, `.len()`, `.filter()`, `.map()`\n");
        prompt.push_str("- Built-in helpers: `len(value)`, `str_sub(text, start, len)`, `str_truncate(text, max_chars)`, `str_lines(text, max_lines)`, `arr_join(items, sep)`\n");
        prompt.push_str(
            "- Map access: `m.key` or `m[\"key\"]`, check existence with `\"key\" in m`\n",
        );
        prompt
            .push_str("- Null coalescing: `value ?? default` (use instead of .get with default)\n");
        prompt.push_str("- Type conversion: `.to_string()`, `.to_int()`, `.to_float()`\n");
        prompt.push_str("- String concat: use `+` only between strings, convert numbers with `.to_string()` first\n");
        prompt.push_str("- No classes/structs — use maps (object maps) `#{}` instead\n");
        prompt.push_str("- No `import`/`require` — all capabilities come from the host engine\n");
        prompt.push_str("- Print: `print(\"msg\");`\n\n");

        prompt.push_str("## Stable Built-in Helper Functions\n");
        prompt.push_str(
            "- `len(value)` -> length of string / array / map, returns 0 for null-like values\n",
        );
        prompt.push_str("- `str_sub(text, start, len)` -> safe substring by character index\n");
        prompt.push_str(
            "- `str_truncate(text, max_chars)` -> truncate text safely at character boundary\n",
        );
        prompt.push_str("- `str_lines(text, max_lines)` -> return the first N lines as an array\n");
        prompt.push_str("- `arr_join(items, sep)` -> join array items into a string\n\n");

        // Task description
        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                if Self::is_openclaw_import_description(description) {
                    prompt.push_str(&format!("## Original Task\n{}\n\n", description));
                } else {
                    prompt.push_str(&format!(
                        "## Original Task\nCreate or improve a Blockcell Rhai skill for: {}\n\n",
                        description
                    ));
                }
            }
        } else {
            prompt.push_str(&format!(
                "## Original Task\nFix the following issue in the existing Blockcell Rhai skill '{}'. Keep behavior changes minimal and targeted.\n\n",
                context.skill_name
            ));
        }

        // Previous code that had issues
        prompt.push_str("## Previous Code (has issues)\n");
        prompt.push_str(&format!(
            "```rhai\n{}\n```\n\n",
            current_feedback.previous_code
        ));

        // Current feedback
        prompt.push_str(&format!("## Issues Found ({})\n", current_feedback.stage));
        prompt.push_str(&format!("{}\n\n", current_feedback.feedback));

        // Show history of previous attempts if any (excluding current)
        let prev_attempts: Vec<&FeedbackEntry> = history
            .iter()
            .filter(|h| h.attempt < current_feedback.attempt)
            .collect();
        if !prev_attempts.is_empty() {
            prompt.push_str("## Previous Attempt History\n");
            prompt.push_str("The following issues were found in earlier attempts. Make sure NOT to repeat them:\n\n");
            for entry in prev_attempts {
                prompt.push_str(&format!(
                    "### Attempt #{} ({} failure)\n",
                    entry.attempt, entry.stage
                ));
                prompt.push_str(&format!("{}\n\n", entry.feedback));
            }
        }

        // Output format
        prompt.push_str("## Instructions\n");
        prompt.push_str(
            "Fix ALL the issues listed above and generate the COMPLETE corrected Rhai script.\n",
        );
        prompt.push_str("If the skill can directly produce final user-facing text, return `display_text`. Otherwise return `summary_data` as a lightweight structured summary for runtime/LLM polishing. Do NOT place complete webpages, long markdown, or large raw logs into `summary_data`.\n");
        prompt.push_str("If you output `meta.yaml`, it must follow the trigger rules below.\n\n");
        prompt.push_str(Self::trigger_rules_prompt());
        prompt.push_str("Do NOT leave any of the reported issues unfixed.\n");
        prompt.push_str("Output ONLY the corrected Rhai code in a ```rhai code block.\n");
        prompt.push_str(
            "The script must be a valid, self-contained Rhai script with no syntax errors.\n",
        );

        Ok(prompt)
    }

    fn build_prompt_only_fix_prompt(
        &self,
        context: &EvolutionContext,
        current_feedback: &FeedbackEntry,
        history: &[FeedbackEntry],
    ) -> Result<String> {
        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });
        let mut prompt = String::new();

        prompt.push_str("You are a skill document writer for the blockcell agent framework.\n");
        prompt
            .push_str("Your task is to fix issues in a SKILL.md prompt instruction document.\n\n");

        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                if Self::is_openclaw_import_description(description) {
                    prompt.push_str(&format!("## Original Task\n{}\n\n", description));
                } else {
                    prompt.push_str(&format!(
                        "## Original Task\nCreate or improve a Blockcell SKILL.md for: {}\n\n",
                        description
                    ));
                }
            }
        } else {
            prompt.push_str(&format!(
                "## Original Task\nFix the existing Blockcell SKILL.md for skill '{}'. Keep the same skill scope and repair only the broken or unclear parts.\n\n",
                context.skill_name
            ));
        }

        prompt.push_str("## Previous Content (has issues)\n");
        prompt.push_str(&format!(
            "```markdown\n{}\n```\n\n",
            current_feedback.previous_code
        ));

        prompt.push_str(&format!("## Issues Found ({})\n", current_feedback.stage));
        prompt.push_str(&format!("{}\n\n", current_feedback.feedback));

        let prev_attempts: Vec<&FeedbackEntry> = history
            .iter()
            .filter(|h| h.attempt < current_feedback.attempt)
            .collect();
        if !prev_attempts.is_empty() {
            prompt.push_str("## Previous Attempt History\n");
            for entry in prev_attempts {
                prompt.push_str(&format!(
                    "### Attempt #{} ({} failure)\n{}\n\n",
                    entry.attempt, entry.stage, entry.feedback
                ));
            }
        }

        prompt.push_str("## Instructions\n");
        prompt.push_str("Fix ALL the issues listed above and generate the COMPLETE corrected SKILL.md content.\n");
        prompt.push_str("Output the markdown content in a ```markdown code block.\n");
        prompt.push_str("Also output an updated meta.yaml in a ```yaml code block.\n");
        prompt.push_str(Self::trigger_rules_prompt());

        Ok(prompt)
    }

    fn extract_yaml_from_response(&self, response: &str) -> Option<String> {
        fn extract_with_marker(response: &str, marker: &str) -> Option<String> {
            let start = response.find(marker)?;
            let mut i = start + marker.len();

            if i < response.len() {
                let rest = &response[i..];
                let line_end = rest.find('\n').unwrap_or(rest.len());
                if line_end > 0 {
                    i += line_end;
                }
            }

            while i < response.len()
                && (response.as_bytes()[i] == b'\n' || response.as_bytes()[i] == b'\r')
            {
                i += 1;
            }

            let end_rel = response[i..].find("```")?;
            let yaml = &response[i..i + end_rel];
            let trimmed = yaml.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }

        extract_with_marker(response, "```yaml").or_else(|| extract_with_marker(response, "```yml"))
    }

    fn build_audit_prompt(
        &self,
        context: &EvolutionContext,
        script_content: &str,
    ) -> Result<String> {
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

    fn build_prompt_only_audit_prompt(
        &self,
        context: &EvolutionContext,
        md_content: &str,
    ) -> Result<String> {
        let mut prompt = String::new();

        prompt.push_str(&format!(
            "You are a quality reviewer for SKILL.md documents in the blockcell agent framework.\n\
            Review the following SKILL.md content for skill '{}'.\n\n",
            context.skill_name
        ));

        prompt.push_str(&format!("Content:\n```markdown\n{}\n```\n\n", md_content));

        prompt.push_str("\
Check for the following issues:\n\
1. **Completeness**: Does it describe what the skill does and how to use it?\n\
2. **Clarity**: Are the instructions clear and actionable for an AI agent?\n\
3. **Length**: Is the content at least 100 characters and substantive?\n\
4. **Structure**: Does it have clear sections/headings?\n\n\
Respond with ONLY a JSON object (no markdown code blocks, no extra text):\n\
{\"passed\": true, \"issues\": []}\n\
or\n\
{\"passed\": false, \"issues\": [{\"severity\": \"error\", \"category\": \"completeness\", \"message\": \"description\"}]}\n");

        Ok(prompt)
    }

    fn build_python_generation_prompt(&self, context: &EvolutionContext) -> Result<String> {
        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });
        let mut prompt = String::new();

        prompt.push_str("You are a Python skill developer for the blockcell agent framework.\n");
        prompt.push_str("Your task is to write or improve a SKILL.py file — a Python script\n");
        prompt.push_str("that implements the skill's logic. The script will be executed by the agent via `python3 SKILL.py`.\n\n");

        prompt.push_str("## Requirements\n");
        prompt.push_str("- Use Python 3.8+ compatible syntax\n");
        prompt.push_str("- Read input from stdin (JSON) or command-line arguments\n");
        prompt.push_str("- Output results to stdout (preferably JSON)\n");
        prompt.push_str("- Handle errors gracefully with try/except\n");
        prompt.push_str("- Only use standard library modules or widely available packages\n");
        prompt.push_str("- Include a `if __name__ == '__main__':` block\n\n");

        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                if Self::is_openclaw_import_description(description) {
                    prompt.push_str(&format!("## Task\n{}\n\n", description));
                } else {
                    prompt.push_str(&format!(
                        "## Task\nCreate or improve a Blockcell SKILL.py for: {}\n\n",
                        description
                    ));
                }
            }
        } else {
            prompt.push_str(&format!(
                "## Task\nFix the existing Blockcell SKILL.py for skill '{}' to address the following issue. Preserve the skill's purpose and change only what is needed to fix it.\n\n",
                context.skill_name
            ));
            if let Some(error) = &context.error_stack {
                prompt.push_str(&format!("## Issue\n```\n{}\n```\n\n", error));
            }
        }

        if let Some(snippet) = &context.source_snippet {
            prompt.push_str(&format!(
                "## Current SKILL.py Content\n```python\n{}\n```\n\n",
                snippet
            ));
        }

        prompt.push_str("## Output Format\n");
        prompt.push_str("Generate the COMPLETE SKILL.py content.\n");
        prompt.push_str("If the skill can directly produce final user-facing text, return `display_text`. Otherwise return `summary_data` as a lightweight structured summary for runtime/LLM polishing. Do NOT place complete webpages, long markdown, or large raw logs into `summary_data`.\n");
        prompt.push_str("Output the Python code in a ```python code block.\n");
        prompt.push_str("If you output `meta.yaml`, it must follow the trigger rules below.\n\n");
        prompt.push_str(Self::trigger_rules_prompt());
        prompt.push_str("The script must be syntactically valid Python.\n");

        Ok(prompt)
    }

    fn build_python_audit_prompt(
        &self,
        context: &EvolutionContext,
        script_content: &str,
    ) -> Result<String> {
        let mut prompt = String::new();

        prompt.push_str(&format!(
            "You are a security auditor for Python scripts in the blockcell agent framework.\n\
            Review the following complete Python script for skill '{}'.\n\n",
            context.skill_name
        ));

        prompt.push_str(&format!("Code:\n```python\n{}\n```\n\n", script_content));

        prompt.push_str("\
Check for the following issues:\n\
1. **Syntax errors**: Is this valid Python 3.8+ syntax?\n\
2. **Security**: No shell injection (unsafe os.system/subprocess with user input), no eval/exec of untrusted data\n\
3. **Infinite loops**: Unbounded loops without break conditions\n\
4. **Resource abuse**: Operations that could consume excessive memory or CPU\n\
5. **Data leakage**: Logging/printing sensitive information unintentionally\n\n\
Respond with ONLY a JSON object (no markdown code blocks, no extra text):\n\
{\"passed\": true, \"issues\": []}\n\
or\n\
{\"passed\": false, \"issues\": [{\"severity\": \"error\", \"category\": \"security\", \"message\": \"description\"}]}\n");

        Ok(prompt)
    }

    fn build_python_fix_prompt(
        &self,
        context: &EvolutionContext,
        current_feedback: &FeedbackEntry,
        history: &[FeedbackEntry],
    ) -> Result<String> {
        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });
        let mut prompt = String::new();

        prompt.push_str("You are a Python skill developer for the blockcell agent framework.\n");
        prompt.push_str("Your task is to fix issues in a SKILL.py Python script.\n\n");

        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                if Self::is_openclaw_import_description(description) {
                    prompt.push_str(&format!("## Original Task\n{}\n\n", description));
                } else {
                    prompt.push_str(&format!(
                        "## Original Task\nCreate or improve a Blockcell SKILL.py for: {}\n\n",
                        description
                    ));
                }
            }
        } else {
            prompt.push_str(&format!(
                "## Original Task\nFix the existing Blockcell SKILL.py for skill '{}'. Keep the same skill scope and repair only the broken parts.\n\n",
                context.skill_name
            ));
        }

        prompt.push_str("## Previous Content (has issues)\n");
        prompt.push_str(&format!(
            "```python\n{}\n```\n\n",
            current_feedback.previous_code
        ));

        prompt.push_str(&format!("## Issues Found ({})\n", current_feedback.stage));
        prompt.push_str(&format!("{}\n\n", current_feedback.feedback));

        let prev_attempts: Vec<&FeedbackEntry> = history
            .iter()
            .filter(|h| h.attempt < current_feedback.attempt)
            .collect();
        if !prev_attempts.is_empty() {
            prompt.push_str("## Previous Attempt History\n");
            for entry in prev_attempts {
                prompt.push_str(&format!(
                    "### Attempt #{} ({} failure)\n{}\n\n",
                    entry.attempt, entry.stage, entry.feedback
                ));
            }
        }

        prompt.push_str("## Instructions\n");
        prompt.push_str("Fix ALL the issues listed above and generate the COMPLETE corrected SKILL.py content.\n");
        prompt.push_str("If the skill can directly produce final user-facing text, return `display_text`. Otherwise return `summary_data` as a lightweight structured summary for runtime/LLM polishing. Do NOT place complete webpages, long markdown, or large raw logs into `summary_data`.\n");
        prompt.push_str("Output the Python code in a ```python code block.\n");
        prompt.push_str("If you output `meta.yaml`, it must follow the trigger rules below.\n\n");
        prompt.push_str(Self::trigger_rules_prompt());

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

        // Try ```python block (for Python skill creation)
        if let Some(start) = response.find("```python") {
            let after_marker = start + 9;
            if let Some(end) = response[after_marker..].find("```") {
                let script = &response[after_marker..after_marker + end];
                return Ok(script.trim().to_string());
            }
        }

        // Try ```markdown block (for prompt-only skills)
        if let Some(start) = response.find("```markdown") {
            let after_marker = start + 11;
            if let Some(end) = response[after_marker..].find("```") {
                let md = &response[after_marker..after_marker + end];
                return Ok(md.trim().to_string());
            }
        }

        // Try generic ``` block
        if let Some(start) = response.find("```") {
            let after_marker = start + 3;
            let content_start = response[after_marker..]
                .find('\n')
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
            let content_start = response[after_marker..]
                .find('\n')
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
        let patch = record
            .patch
            .as_ref()
            .ok_or_else(|| Error::Evolution("No patch to deploy".to_string()))?;

        let skill_root = self.skill_root_dir_for_record(record);
        let staged_skill_dir = skill_root.join(&record.skill_name);

        // Ensure skill directory exists (for new skills)
        std::fs::create_dir_all(&staged_skill_dir)?;

        // PromptOnly 写入 SKILL.md，Python 写入 SKILL.py，Rhai 技能写入 SKILL.rhai
        let skill_path = match record.context.skill_type {
            SkillType::PromptOnly => staged_skill_dir.join("SKILL.md"),
            SkillType::Python => staged_skill_dir.join("SKILL.py"),
            SkillType::Rhai => staged_skill_dir.join("SKILL.rhai"),
        };

        // 直接写入完整内容（所有生成都是完整文件）
        std::fs::write(&skill_path, &patch.diff)?;

        if let Some(meta) = self.extract_yaml_from_response(&patch.explanation) {
            let meta_path = staged_skill_dir.join("meta.yaml");
            let _ = std::fs::write(meta_path, meta);
        }

        // If this is a staged external skill, promote it into the main skills dir now.
        if record.context.staged {
            let dest_skill_dir = self.skills_dir.join(&record.skill_name);
            if dest_skill_dir.exists() {
                std::fs::remove_dir_all(&dest_skill_dir)?;
            }
            std::fs::create_dir_all(&self.skills_dir)?;

            // Prefer atomic rename if possible; fallback to copy+remove.
            if let Err(e) = std::fs::rename(&staged_skill_dir, &dest_skill_dir) {
                warn!(
                    skill = %record.skill_name,
                    error = %e,
                    "Staged skill promote via rename failed, falling back to copy"
                );
                copy_dir_all(&staged_skill_dir, &dest_skill_dir)?;
                std::fs::remove_dir_all(&staged_skill_dir).ok();
            }

            if let Some(meta) = self.extract_yaml_from_response(&patch.explanation) {
                let meta_path = dest_skill_dir.join("meta.yaml");
                let _ = std::fs::write(meta_path, meta);
            }

            info!(
                skill = %record.skill_name,
                from = %skill_root.display(),
                to = %self.skills_dir.display(),
                "🚚 [promote] External skill promoted into main skills directory"
            );
        }

        // 通过 VersionManager 创建版本快照
        let changelog = Some(format!(
            "Evolution {}: {}",
            record.id,
            patch.explanation.chars().take(200).collect::<String>()
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

        // Clean up skill directory — remove temp/cache/backup files
        let final_skill_dir = self.skills_dir.join(&record.skill_name);
        self.cleanup_skill_dir(&final_skill_dir, &record.skill_name);

        Ok(())
    }

    /// 清理技能目录：删除非必要文件，只保留 SKILL.rhai/SKILL.py/SKILL.md, meta.yaml, tests/, CHANGELOG.md
    fn cleanup_skill_dir(&self, skill_dir: &Path, skill_name: &str) {
        if !skill_dir.exists() {
            return;
        }

        // Files/dirs we always keep
        let keep_files: &[&str] = &[
            "SKILL.rhai",
            "SKILL.py",
            "SKILL.md",
            "meta.yaml",
            "CHANGELOG.md",
        ];
        let keep_dirs: &[&str] = &["tests"];

        let entries = match std::fs::read_dir(skill_dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        let mut removed = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if path.is_dir() {
                if keep_dirs.contains(&name_str.as_ref()) {
                    continue;
                }
                // Remove __pycache__ and other cache dirs
                if (name_str == "__pycache__" || name_str.starts_with('.'))
                    && std::fs::remove_dir_all(&path).is_ok()
                {
                    removed += 1;
                }
            } else {
                if keep_files.contains(&name_str.as_ref()) {
                    continue;
                }
                // Remove temp files, .pyc, .bak, .tmp, .orig, swap files
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                let should_remove =
                    matches!(ext, "pyc" | "pyo" | "bak" | "tmp" | "orig" | "swp" | "swo")
                        || name_str.ends_with(".bak")
                        || name_str.ends_with(".orig")
                        || name_str.starts_with('.')
                        || name_str.ends_with('~');
                if should_remove && std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }

        if removed > 0 {
            info!(
                skill = %skill_name,
                removed = removed,
                "🧹 [cleanup] Removed {} non-essential files from skill directory",
                removed
            );
        }
    }

    fn restore_previous_version(&self, skill_name: &str) -> Result<()> {
        self.version_manager
            .rollback(skill_name)
            .map_err(|e| Error::Evolution(format!("Rollback failed: {}", e)))
    }

    pub fn save_record_public(&self, record: &EvolutionRecord) -> Result<()> {
        self.save_record(record)
    }

    /// P2-7: 原子写入 — write-tmp-then-rename，避免崩溃时文件损坏
    fn save_record(&self, record: &EvolutionRecord) -> Result<()> {
        let records_dir = self
            .evolution_db
            .parent()
            .unwrap()
            .join("evolution_records");
        std::fs::create_dir_all(&records_dir)?;

        let record_file = records_dir.join(format!("{}.json", record.id));
        // Use a unique temp file name to avoid races when multiple tick loops/processes
        // attempt to write the same record concurrently.
        let counter = RECORD_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let temp_file = records_dir.join(format!(
            "{}.json.tmp_{}_{}_{}",
            record.id,
            chrono::Utc::now().timestamp_millis(),
            pid,
            counter
        ));
        let json = serde_json::to_string_pretty(record)?;

        // 先写入临时文件
        std::fs::write(&temp_file, &json)?;
        // 原子重命名（同一文件系统上是原子操作）
        std::fs::rename(&temp_file, &record_file)?;

        Ok(())
    }

    pub fn load_record(&self, evolution_id: &str) -> Result<EvolutionRecord> {
        let records_dir = self
            .evolution_db
            .parent()
            .unwrap()
            .join("evolution_records");
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
