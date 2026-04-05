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
    /// 本地脚本 / CLI 技能（scripts/、bin/ 等，走 exec_local）
    LocalScript,
}

/// 技能布局：决定技能目录的组织方式和进化分支
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum SkillLayout {
    /// 纯 Prompt 技能：以 SKILL.md 为主
    #[default]
    PromptTool,
    /// 本地脚本技能：以可执行脚本资产为主
    LocalScript,
    /// 混合技能：SKILL.md + 本地脚本资产
    Hybrid,
    /// Rhai 编排技能：以 SKILL.rhai 为主
    RhaiOrchestration,
}

impl SkillLayout {
    pub fn as_str(&self) -> &'static str {
        match self {
            SkillLayout::PromptTool => "PromptTool",
            SkillLayout::LocalScript => "LocalScript",
            SkillLayout::Hybrid => "Hybrid",
            SkillLayout::RhaiOrchestration => "RhaiOrchestration",
        }
    }
}

/// 进化上下文
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionContext {
    pub skill_name: String,
    pub current_version: String,
    pub trigger: TriggerReason,
    pub error_stack: Option<String>,
    pub source_snippet: Option<String>,
    /// Source artifact path relative to the skill directory (e.g. `SKILL.py`, `scripts/cli.sh`).
    #[serde(default)]
    pub source_path: Option<String>,
    /// 技能布局（PromptTool / LocalScript / Hybrid / RhaiOrchestration）
    #[serde(default)]
    pub layout: SkillLayout,
    pub tool_schemas: Vec<serde_json::Value>,
    pub timestamp: i64,
    /// 内部脚本类型（Rhai / PromptOnly / Python / LocalScript），用于兼容旧的编译和审计逻辑
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

/// Enriched context gathered before evolution prompt generation.
/// Contains project rules, skill docs, historical experience, and adjacent skill references.
#[derive(Debug, Clone, Default)]
pub struct EnrichedEvolutionContext {
    /// BLOCKCELL.md or CLAUDE.md content (project-level rules)
    pub blockcell_md: Option<String>,
    /// Current SKILL.md content (runtime contract)
    pub skill_md: Option<String>,
    /// manual/evolution.md content (historical fix experience)
    pub evolution_history_md: Option<String>,
    /// Adjacent skills of the same type (for style consistency)
    pub adjacent_skills: Vec<AdjacentSkillRef>,
    /// Recent evolution summaries for this skill (avoid repeating failures)
    pub recent_evolutions: Vec<String>,
}

/// Reference to an adjacent skill (name + SKILL.md snippet)
#[derive(Debug, Clone)]
pub struct AdjacentSkillRef {
    pub name: String,
    pub snippet: String,
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
        "## meta.yaml rules\n\
- Keep `meta.yaml` minimal.\n\
- Required fields: `name`, `description`.\n\
- Optional fields: `tools`, `requires`, `permissions`, `fallback`.\n\
- `tools` must be a short YAML string list of ordinary host tools actually used by the skill.\n\
- Do NOT include `exec_local` in `tools`; local execution belongs in `SKILL.md` instructions.\n\
- `requires` may contain `bins` and `env` only when there is a real local dependency.\n\
- `permissions` should be an empty list unless the skill truly needs explicit permission declarations.\n\
- `fallback` is optional; when present, keep it simple with a `strategy` and user-facing `message`.\n\
- Do NOT generate any legacy routing or formatting fields.\n\n"
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

        let prompt = match record.context.layout {
            SkillLayout::PromptTool => {
                self.build_prompt_only_audit_prompt(&record.context, &final_script)?
            }
            SkillLayout::LocalScript => {
                self.build_local_script_audit_prompt(&record.context, &final_script)?
            }
            SkillLayout::Hybrid => self.build_hybrid_audit_prompt(&record.context, &final_script)?,
            SkillLayout::RhaiOrchestration => {
                self.build_audit_prompt(&record.context, &final_script)?
            }
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

        let compile_result = match record.context.layout {
            SkillLayout::PromptTool => {
                info!(evolution_id = %evolution_id, "🔨 [compile] PromptTool skill — checking SKILL.md content");
                let content = patch.diff.trim();
                if content.is_empty() {
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
                }
            }
            SkillLayout::LocalScript => {
                info!(evolution_id = %evolution_id, "🔨 [compile] LocalScript skill — running local script syntax/entry validation");
                let source_path = record
                    .context
                    .source_path
                    .as_deref()
                    .ok_or_else(|| Error::Evolution("Missing source_path for LocalScript skill".to_string()))?;
                let script_path = self
                    .skill_root_dir_for_record(&record)
                    .join(&record.skill_name)
                    .join(source_path);
                self.compile_local_script(&script_path).await?
            }
            SkillLayout::Hybrid => {
                match record.context.skill_type {
                    SkillType::Python => {
                        info!(evolution_id = %evolution_id, "🔨 [compile] Hybrid skill — running Python syntax check for local script asset");
                        let final_script = self.resolve_final_script(&record.skill_name, &patch.diff)?;
                        let temp_path = std::env::temp_dir().join(format!("{}_compile.py", record.skill_name));
                        std::fs::write(&temp_path, &final_script)?;

                        let output = std::process::Command::new("python3")
                            .args(["-m", "py_compile", temp_path.to_str().unwrap_or("")])
                            .output();

                        let _ = std::fs::remove_file(&temp_path);

                        match output {
                            Ok(out) if out.status.success() => (true, None),
                            Ok(out) => {
                                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                                (false, Some(format!("Python syntax error:\n{}", stderr)))
                            }
                            Err(e) => {
                                warn!(evolution_id = %evolution_id, "🔨 [compile] python3 not found, skipping syntax check: {}", e);
                                (true, None)
                            }
                        }
                    }
                    SkillType::LocalScript => {
                        info!(evolution_id = %evolution_id, "🔨 [compile] Hybrid skill — validating local script asset");
                        let source_path = record
                            .context
                            .source_path
                            .as_deref()
                            .ok_or_else(|| Error::Evolution("Missing source_path for LocalScript skill".to_string()))?;
                        let script_path = self
                            .skill_root_dir_for_record(&record)
                            .join(&record.skill_name)
                            .join(source_path);
                        self.compile_local_script(&script_path).await?
                    }
                    SkillType::Rhai => {
                        info!(evolution_id = %evolution_id, "🔨 [compile] Hybrid skill — falling back to Rhai compilation");
                        let final_script = self.resolve_final_script(&record.skill_name, &patch.diff)?;
                        self.compile_rhai_check(evolution_id, &record.skill_name, &final_script)
                            .await?
                    }
                    SkillType::PromptOnly => {
                        info!(evolution_id = %evolution_id, "🔨 [compile] Hybrid skill — checking prompt content length");
                        let content = patch.diff.trim();
                        if content.is_empty() {
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
                        }
                    }
                }
            }
            SkillLayout::RhaiOrchestration => {
                let final_script = self.resolve_final_script(&record.skill_name, &patch.diff)?;
                self.compile_rhai_check(evolution_id, &record.skill_name, &final_script)
                    .await?
            }
        };

        let (passed, compile_error) = compile_result;

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

    /// Contract check: validate SKILL.md structure and meta.yaml required fields.
    ///
    /// Runs after compile check passes. This is a deterministic validation that ensures
    /// the generated code doesn't break the skill's contract (required sections, fields).
    /// Returns (passed, Option<error_description>).
    pub fn contract_check(&self, evolution_id: &str) -> Result<(bool, Option<String>)> {
        let record = self.load_record(evolution_id)?;
        let skill_root = self.skill_root_dir_for_record(&record);
        let skill_dir = skill_root.join(&record.skill_name);

        let mut issues: Vec<String> = Vec::new();

        let meta_path = skill_dir.join("meta.yaml");
        if meta_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&meta_path) {
                if !content.contains("name:") && !content.contains("name :") {
                    issues.push("meta.yaml: missing required 'name' field".to_string());
                }
                if !content.contains("description:") && !content.contains("description :") {
                    issues.push("meta.yaml: missing required 'description' field".to_string());
                }
                if content.trim().is_empty() {
                    issues.push("meta.yaml: file is empty".to_string());
                }
            }
        }

        let skill_md_path = skill_dir.join("SKILL.md");
        let skill_md_content = std::fs::read_to_string(&skill_md_path).ok();

        match record.context.layout {
            SkillLayout::PromptTool => {
                let Some(content) = skill_md_content.as_ref() else {
                    issues.push("SKILL.md: file is missing (required for PromptTool skills)".to_string());
                    let passed = issues.is_empty();
                    let error = if passed { None } else { Some(issues.join("\n")) };
                    return Ok((passed, error));
                };

                if content.trim().len() < 100 {
                    issues.push(format!(
                        "SKILL.md: content too short ({} chars, minimum 100)",
                        content.trim().len()
                    ));
                }
                if !content.contains('#') {
                    issues.push("SKILL.md: no markdown headings found".to_string());
                }
                if !content.contains("## Shared") && !content.contains("## Shared {#shared}") {
                    issues.push("SKILL.md: missing '## Shared' section".to_string());
                }
                if !content.contains("## Prompt") && !content.contains("## Prompt {#prompt}") {
                    issues.push("SKILL.md: missing '## Prompt' section".to_string());
                }
            }
            SkillLayout::LocalScript => {
                if let Some(content) = skill_md_content.as_ref() {
                    if !content.contains("## Shared") && !content.contains("## Shared {#shared}") {
                        issues.push("SKILL.md: missing '## Shared' section".to_string());
                    }
                    if !content.contains("## Prompt") && !content.contains("## Prompt {#prompt}") {
                        issues.push("SKILL.md: missing '## Prompt' section".to_string());
                    }
                }
            }
            SkillLayout::Hybrid => {
                let Some(content) = skill_md_content.as_ref() else {
                    issues.push("SKILL.md: file is missing (required for Hybrid skills)".to_string());
                    let passed = issues.is_empty();
                    let error = if passed { None } else { Some(issues.join("\n")) };
                    return Ok((passed, error));
                };

                if !content.contains("## Shared") && !content.contains("## Shared {#shared}") {
                    issues.push("SKILL.md: missing '## Shared' section".to_string());
                }
                if !content.contains("## Prompt") && !content.contains("## Prompt {#prompt}") {
                    issues.push("SKILL.md: missing '## Prompt' section".to_string());
                }
            }
            SkillLayout::RhaiOrchestration => {
                if let Some(content) = skill_md_content.as_ref() {
                    if !content.contains("## Shared") && !content.contains("## Shared {#shared}") {
                        issues.push("SKILL.md: missing '## Shared' section".to_string());
                    }
                    if !content.contains("## Prompt") && !content.contains("## Prompt {#prompt}") {
                        issues.push("SKILL.md: missing '## Prompt' section".to_string());
                    }
                }
            }
        }

        let primary_file = if let Some(source_path) = record.context.source_path.as_ref() {
            skill_dir.join(source_path)
        } else {
            match record.context.layout {
                SkillLayout::RhaiOrchestration => skill_dir.join("SKILL.rhai"),
                SkillLayout::PromptTool => skill_dir.join("SKILL.md"),
                SkillLayout::LocalScript => skill_dir.join("scripts/skill.sh"),
                SkillLayout::Hybrid => match record.context.skill_type {
                    SkillType::Python => skill_dir.join("SKILL.py"),
                    SkillType::LocalScript => skill_dir.join("scripts/skill.sh"),
                    _ => skill_dir.join("SKILL.md"),
                },
            }
        };
        if !primary_file.exists() {
            issues.push(format!(
                "Primary skill file missing: {}",
                primary_file.file_name().unwrap_or_default().to_string_lossy()
            ));
        }

        let passed = issues.is_empty();
        let error = if passed {
            None
        } else {
            Some(issues.join("\n"))
        };

        if passed {
            info!(evolution_id = %evolution_id, "📋 [contract] Contract check passed");
        } else {
            warn!(
                evolution_id = %evolution_id,
                issues = issues.len(),
                "📋 [contract] Contract check found {} issue(s)",
                issues.len()
            );
        }

        Ok((passed, error))
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

    /// Gather enriched context for evolution prompts.
    /// Reads BLOCKCELL.md (project-level rules), SKILL.md, manual/evolution.md,
    /// and adjacent skills of the same type.
    fn gather_evolution_context(&self, context: &EvolutionContext) -> EnrichedEvolutionContext {
        let skills_dir = self.skill_root_dir_by_name(&context.skill_name, context.staged, context.staging_skills_dir.as_deref());
        let skill_dir = skills_dir.join(&context.skill_name);

        // 1. Read BLOCKCELL.md — walk up from skills_dir to find it
        let blockcell_md = self.find_and_read_blockcell_md(&skills_dir);

        // 2. Read SKILL.md (the runtime contract)
        let skill_md = std::fs::read_to_string(skill_dir.join("SKILL.md")).ok();

        // 3. Read manual/evolution.md (historical fix experience)
        let evolution_history_md = std::fs::read_to_string(
            skill_dir.join("manual").join("evolution.md")
        ).ok();

        // 4. Find adjacent skills of the same type (max 3, max 500 chars each)
        let adjacent_skills = self.find_adjacent_skills(&context.skill_name, &context.layout);

        // 5. Collect recent completed evolution records for this skill
        let recent_evolutions = self.load_recent_evolution_summaries(&context.skill_name, 3);

        EnrichedEvolutionContext {
            blockcell_md,
            skill_md,
            evolution_history_md,
            adjacent_skills,
            recent_evolutions,
        }
    }

    /// Walk up from skills_dir to find BLOCKCELL.md (or CLAUDE.md as fallback)
    fn find_and_read_blockcell_md(&self, skills_dir: &Path) -> Option<String> {
        let mut dir = skills_dir.to_path_buf();
        // Walk up at most 4 levels (skills -> workspace -> .blockcell -> home)
        for _ in 0..4 {
            let candidate = dir.join("BLOCKCELL.md");
            if candidate.exists() {
                if let Ok(content) = std::fs::read_to_string(&candidate) {
                    let truncated: String = content.chars().take(2000).collect();
                    return Some(truncated);
                }
            }
            // Also check CLAUDE.md as fallback
            let claude_candidate = dir.join("CLAUDE.md");
            if claude_candidate.exists() {
                if let Ok(content) = std::fs::read_to_string(&claude_candidate) {
                    let truncated: String = content.chars().take(2000).collect();
                    return Some(truncated);
                }
            }
            if !dir.pop() {
                break;
            }
        }
        None
    }

    /// Find adjacent skills of the same SkillLayout, return up to `max` snippet references.
    fn find_adjacent_skills(&self, skill_name: &str, layout: &SkillLayout) -> Vec<AdjacentSkillRef> {
        let mut refs = Vec::new();
        let skills_dir = &self.skills_dir;

        let entries = match std::fs::read_dir(skills_dir) {
            Ok(e) => e,
            Err(_) => return refs,
        };

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == skill_name { continue; }
            if !entry.path().is_dir() { continue; }

            // Detect layout of this adjacent skill
            let adj_layout = self.detect_adjacent_skill_layout(&name);
            if &adj_layout != layout { continue; }

            // Read SKILL.md snippet
            let skill_md_path = entry.path().join("SKILL.md");
            if let Ok(content) = std::fs::read_to_string(&skill_md_path) {
                if !content.trim().is_empty() {
                    refs.push(AdjacentSkillRef {
                        name,
                        snippet: content.chars().take(500).collect(),
                    });
                }
            }

            if refs.len() >= 3 { break; }
        }
        refs
    }

    /// Simple skill layout detection for adjacent skills (no truncation needed)
    fn detect_adjacent_skill_layout(&self, skill_name: &str) -> SkillLayout {
        let skill_dir = self.skills_dir.join(skill_name);
        let has_md = skill_dir.join("SKILL.md").exists();
        if skill_dir.join("SKILL.rhai").exists() {
            SkillLayout::RhaiOrchestration
        } else if skill_dir.join("SKILL.py").exists() || Self::contains_local_script_asset(&skill_dir) {
            if has_md {
                SkillLayout::Hybrid
            } else {
                SkillLayout::LocalScript
            }
        } else {
            SkillLayout::PromptTool
        }
    }

    fn contains_local_script_asset(skill_dir: &Path) -> bool {
        let script_dir = skill_dir.join("scripts");
        let bin_dir = skill_dir.join("bin");

        if Self::dir_contains_local_script(&script_dir) || Self::dir_contains_local_script(&bin_dir) {
            return true;
        }

        let Ok(entries) = std::fs::read_dir(skill_dir) else {
            return false;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let file_name = path.file_name().and_then(|value| value.to_str()).unwrap_or("");
            if matches!(file_name, "SKILL.md" | "SKILL.rhai" | "meta.yaml" | "meta.json") {
                continue;
            }

            let ext_ok = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches!(ext, "py" | "sh" | "php" | "js" | "ts" | "rb"));
            let no_ext_exec = path.extension().is_none() && Self::looks_executable(&path);
            if ext_ok || no_ext_exec {
                return true;
            }
        }

        false
    }

    fn dir_contains_local_script(dir: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if Self::dir_contains_local_script(&path) {
                    return true;
                }
                continue;
            }

            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches!(ext, "py" | "sh" | "php" | "js" | "ts" | "rb"))
            {
                return true;
            }
        }

        false
    }

    #[cfg(unix)]
    fn looks_executable(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;

        std::fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    #[cfg(not(unix))]
    fn looks_executable(_path: &Path) -> bool {
        false
    }

    /// Load recent completed/failed evolution summaries for a skill (for prompt injection)
    fn load_recent_evolution_summaries(&self, skill_name: &str, max: usize) -> Vec<String> {
        let records_dir = self.records_dir();
        if !records_dir.exists() {
            return Vec::new();
        }

        let mut summaries: Vec<(i64, String)> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&records_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "json") { continue; }
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(record) = serde_json::from_str::<EvolutionRecord>(&content) {
                        if record.skill_name != skill_name { continue; }
                        if !matches!(record.status, EvolutionStatus::Completed | EvolutionStatus::RolledBack | EvolutionStatus::Failed) {
                            continue;
                        }
                        let summary = format!(
                            "[{:?}] attempt={}, trigger={:?}{}",
                            record.status,
                            record.attempt,
                            record.context.trigger,
                            record.patch.as_ref().map(|p| {
                                let expl: String = p.explanation.chars().take(150).collect();
                                format!(", explanation={}", expl)
                            }).unwrap_or_default()
                        );
                        summaries.push((record.created_at, summary));
                    }
                }
            }
        }

        summaries.sort_by(|a, b| b.0.cmp(&a.0));
        summaries.into_iter().take(max).map(|(_, s)| s).collect()
    }

    /// Helper: resolve skill root dir by name (handles staged vs normal)
    fn skill_root_dir_by_name(&self, _skill_name: &str, staged: bool, staging_dir: Option<&str>) -> PathBuf {
        if staged {
            if let Some(dir) = staging_dir {
                let p = PathBuf::from(dir);
                if p.is_absolute() {
                    return p;
                }
            }
        }
        self.skills_dir.clone()
    }

    /// Format enriched context as prompt sections
    fn format_enriched_context(&self, enriched: &EnrichedEvolutionContext) -> String {
        let mut sections = String::new();

        if let Some(ref md) = enriched.blockcell_md {
            sections.push_str("## Project Rules (BLOCKCELL.md)\n");
            sections.push_str(md);
            sections.push_str("\n\n");
        }

        if let Some(ref md) = enriched.skill_md {
            let truncated: String = md.chars().take(1500).collect();
            sections.push_str("## Current SKILL.md (Runtime Contract)\n");
            sections.push_str(&truncated);
            sections.push_str("\n\n");
        }

        if let Some(ref md) = enriched.evolution_history_md {
            let truncated: String = md.chars().take(1000).collect();
            sections.push_str("## Historical Fix Experience (manual/evolution.md)\n");
            sections.push_str(&truncated);
            sections.push_str("\n\n");
        }

        if !enriched.adjacent_skills.is_empty() {
            sections.push_str("## Adjacent Skills Reference (same layout, for style consistency)\n");
            for adj in &enriched.adjacent_skills {
                sections.push_str(&format!("### {}\n{}\n\n", adj.name, adj.snippet));
            }
        }

        if !enriched.recent_evolutions.is_empty() {
            sections.push_str("## Recent Evolution History (avoid repeating past failures)\n");
            for summary in &enriched.recent_evolutions {
                sections.push_str(&format!("- {}\n", summary));
            }
            sections.push('\n');
        }

        sections
    }

    fn build_generation_prompt(&self, context: &EvolutionContext) -> Result<String> {
        match context.layout {
            SkillLayout::PromptTool => return self.build_prompt_only_generation_prompt(context),
            SkillLayout::LocalScript => return self.build_local_script_generation_prompt(context),
            SkillLayout::Hybrid => return self.build_hybrid_generation_prompt(context),
            SkillLayout::RhaiOrchestration => {}
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

        // Enriched context: project rules, SKILL.md, evolution history, adjacent skills
        let enriched = self.gather_evolution_context(context);
        prompt.push_str(&self.format_enriched_context(&enriched));

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
        prompt.push_str("If you output `meta.yaml`, it must follow the minimal meta rules below.\n\n");
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

        // Enriched context: project rules, evolution history, adjacent skills
        let enriched = self.gather_evolution_context(context);
        prompt.push_str(&self.format_enriched_context(&enriched));

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

    fn build_hybrid_generation_prompt(&self, context: &EvolutionContext) -> Result<String> {
        let mut prompt = String::new();
        prompt.push_str("You are a hybrid skill evolution assistant for the blockcell agent framework.\n");
        prompt.push_str("This skill combines SKILL.md instructions with local script assets. Keep the manual, entrypoint, and fallback behavior aligned.\n\n");
        self.append_hybrid_contract_notes(&mut prompt, context);

        let body = match context.skill_type {
            SkillType::Python => self.build_python_generation_prompt(context)?,
            SkillType::LocalScript => self.build_local_script_generation_prompt(context)?,
            _ => self.build_prompt_only_generation_prompt(context)?,
        };

        prompt.push_str(&body);
        Ok(prompt)
    }

    fn build_fix_prompt(
        &self,
        context: &EvolutionContext,
        current_feedback: &FeedbackEntry,
        history: &[FeedbackEntry],
    ) -> Result<String> {
        match context.layout {
            SkillLayout::PromptTool => {
                return self.build_prompt_only_fix_prompt(context, current_feedback, history)
            }
            SkillLayout::LocalScript => {
                return self.build_local_script_fix_prompt(context, current_feedback, history)
            }
            SkillLayout::Hybrid => return self.build_hybrid_fix_prompt(context, current_feedback, history),
            SkillLayout::RhaiOrchestration => {}
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

        // Enriched context: project rules, SKILL.md, evolution history, adjacent skills
        let enriched = self.gather_evolution_context(context);
        prompt.push_str(&self.format_enriched_context(&enriched));

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
        prompt.push_str("If you output `meta.yaml`, it must follow the minimal meta rules below.\n\n");
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

        // Enriched context: project rules, evolution history, adjacent skills
        let enriched = self.gather_evolution_context(context);
        prompt.push_str(&self.format_enriched_context(&enriched));

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

    fn build_hybrid_fix_prompt(
        &self,
        context: &EvolutionContext,
        current_feedback: &FeedbackEntry,
        history: &[FeedbackEntry],
    ) -> Result<String> {
        let mut prompt = String::new();
        prompt.push_str("You are a hybrid skill evolution assistant for the blockcell agent framework.\n");
        prompt.push_str("This skill combines SKILL.md instructions with local script assets. Keep the prompt contract and the executable entrypoint consistent.\n\n");
        self.append_hybrid_contract_notes(&mut prompt, context);

        let body = match context.skill_type {
            SkillType::Python => self.build_python_fix_prompt(context, current_feedback, history)?,
            SkillType::LocalScript => self.build_local_script_fix_prompt(context, current_feedback, history)?,
            _ => self.build_prompt_only_fix_prompt(context, current_feedback, history)?,
        };

        prompt.push_str(&body);
        Ok(prompt)
    }

    fn build_local_script_generation_prompt(&self, context: &EvolutionContext) -> Result<String> {
        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });
        let mut prompt = String::new();

        prompt.push_str("You are a local script and CLI skill developer for the blockcell agent framework.\n");
        prompt.push_str("Your task is to write or improve a local script asset that will be executed through exec_local inside the active skill directory.\n\n");

        prompt.push_str("## Requirements\n");
        prompt.push_str("- Keep the script runnable from inside the skill directory\n");
        prompt.push_str("- Read input from stdin, args, or environment variables when appropriate\n");
        prompt.push_str("- Write user-facing results to stdout\n");
        prompt.push_str("- Handle errors gracefully and exit non-zero on failure\n");
        prompt.push_str("- Avoid unsafe shell expansion and command injection\n");
        prompt.push_str("- Prefer small, deterministic entrypoints\n\n");

        if let Some(source_path) = &context.source_path {
            prompt.push_str(&format!("## Target File\n{}\n\n", source_path));
        }

        let enriched = self.gather_evolution_context(context);
        prompt.push_str(&self.format_enriched_context(&enriched));

        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                prompt.push_str(&format!("## Task\nCreate or improve a Blockcell local script for: {}\n\n", description));
            }
        } else {
            prompt.push_str(&format!(
                "## Task\nFix the existing Blockcell local script for skill '{}' to address the following issue. Preserve the skill's purpose and change only what is needed.\n\n",
                context.skill_name
            ));
            if let Some(error) = &context.error_stack {
                prompt.push_str(&format!("## Issue\n```\n{}\n```\n\n", error));
            }
        }

        if let Some(snippet) = &context.source_snippet {
            let fence = context
                .source_path
                .as_deref()
                .and_then(|path| std::path::Path::new(path).extension().and_then(|ext| ext.to_str()))
                .map(|ext| match ext {
                    "sh" | "bash" | "zsh" => "bash",
                    "js" => "javascript",
                    "php" => "php",
                    "rb" => "ruby",
                    _ => "text",
                })
                .unwrap_or("text");
            prompt.push_str(&format!("## Current Script Content\n```{}\n{}\n```\n\n", fence, snippet));
        }

        prompt.push_str("## Output Format\n");
        prompt.push_str("Generate the COMPLETE local script content.\n");
        prompt.push_str("If the skill can directly produce final user-facing text, return `display_text`. Otherwise return `summary_data` as a lightweight structured summary for runtime/LLM polishing.\n");
        prompt.push_str("If you output `meta.yaml`, it must follow the minimal meta rules below.\n\n");
        prompt.push_str(Self::trigger_rules_prompt());
        prompt.push_str("The script must be runnable by exec_local and should not rely on unsafe external assumptions.\n");

        Ok(prompt)
    }

    fn build_local_script_fix_prompt(
        &self,
        context: &EvolutionContext,
        current_feedback: &FeedbackEntry,
        history: &[FeedbackEntry],
    ) -> Result<String> {
        let is_manual = matches!(context.trigger, TriggerReason::ManualRequest { .. });
        let mut prompt = String::new();

        prompt.push_str("You are a local script and CLI skill developer for the blockcell agent framework.\n");
        prompt.push_str("Your task is to fix issues in a local script asset that will be executed through exec_local.\n\n");

        let enriched = self.gather_evolution_context(context);
        prompt.push_str(&self.format_enriched_context(&enriched));

        if is_manual {
            if let TriggerReason::ManualRequest { ref description } = context.trigger {
                prompt.push_str(&format!("## Original Task\nCreate or improve a Blockcell local script for: {}\n\n", description));
            }
        } else {
            prompt.push_str(&format!(
                "## Original Task\nFix the existing Blockcell local script for skill '{}'. Keep the same scope and repair only the broken parts.\n\n",
                context.skill_name
            ));
        }

        prompt.push_str("## Previous Content (has issues)\n");
        prompt.push_str(&format!("```\n{}\n```\n\n", current_feedback.previous_code));
        prompt.push_str(&format!("## Issues Found ({})\n{}\n\n", current_feedback.stage, current_feedback.feedback));

        let prev_attempts: Vec<&FeedbackEntry> = history
            .iter()
            .filter(|h| h.attempt < current_feedback.attempt)
            .collect();
        if !prev_attempts.is_empty() {
            prompt.push_str("## Previous Attempt History\n");
            for entry in prev_attempts {
                prompt.push_str(&format!("### Attempt #{} ({} failure)\n{}\n\n", entry.attempt, entry.stage, entry.feedback));
            }
        }

        prompt.push_str("## Instructions\n");
        prompt.push_str("Fix ALL the issues listed above and generate the COMPLETE corrected local script content.\n");
        prompt.push_str("If the skill can directly produce final user-facing text, return `display_text`. Otherwise return `summary_data` as a lightweight structured summary for runtime/LLM polishing.\n");
        prompt.push_str("If you output `meta.yaml`, it must follow the minimal meta rules below.\n\n");
        prompt.push_str(Self::trigger_rules_prompt());
        prompt.push_str("Do NOT leave any of the reported issues unfixed.\n");

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

        // Enriched context: project rules, evolution history, adjacent skills
        let enriched = self.gather_evolution_context(context);
        prompt.push_str(&self.format_enriched_context(&enriched));

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
        prompt.push_str("If you output `meta.yaml`, it must follow the minimal meta rules below.\n\n");
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

    fn build_local_script_audit_prompt(
        &self,
        context: &EvolutionContext,
        script_content: &str,
    ) -> Result<String> {
        let mut prompt = String::new();

        prompt.push_str(&format!(
            "You are a security auditor for local script and CLI assets in the blockcell agent framework.\n\
            Review the following complete script for skill '{}'.\n\n",
            context.skill_name
        ));

        prompt.push_str(&format!("Code:\n```\n{}\n```\n\n", script_content));

        prompt.push_str(
            "Check for the following issues:\n\
1. **Shell injection / command injection**: unsafe string concatenation into commands\n\
2. **Unsafe file access**: path traversal or writing outside the skill directory\n\
3. **Infinite loops**: unbounded loops without break conditions\n\
4. **Resource abuse**: operations that could consume excessive memory or CPU\n\
5. **Data leakage**: logging sensitive information unintentionally\n\n\
Respond with ONLY a JSON object (no markdown code blocks, no extra text):\n\
{\"passed\": true, \"issues\": []}\n\
or\n\
{\"passed\": false, \"issues\": [{\"severity\": \"error\", \"category\": \"security\", \"message\": \"description\"}]}\n",
        );

        Ok(prompt)
    }

    fn append_hybrid_contract_notes(&self, prompt: &mut String, context: &EvolutionContext) {
        prompt.push_str("## Hybrid Contract\n");
        prompt.push_str("- `SKILL.md` defines the user-facing behavior, the tool flow, and when local execution is appropriate.\n");
        prompt.push_str("- The file at `source_path` is the executable entrypoint for local behavior.\n");
        prompt.push_str("- Keep the manual and the entrypoint aligned; if you move behavior, update both sides together.\n");
        if let Some(source_path) = context.source_path.as_ref() {
            prompt.push_str(&format!("- Current entrypoint: `{}`\n", source_path));
        }
        prompt.push_str("- Use `exec_local` only for relative paths inside the active skill directory.\n\n");
    }

    fn build_hybrid_audit_prompt(
        &self,
        context: &EvolutionContext,
        script_content: &str,
    ) -> Result<String> {
        let mut prompt = String::new();
        prompt.push_str("You are a security auditor for hybrid skills in the blockcell agent framework.\n");
        prompt.push_str("This skill combines SKILL.md with a local script asset, so audit both the contract and the executable entrypoint.\n\n");

        let body = match context.skill_type {
            SkillType::Python => self.build_python_audit_prompt(context, script_content)?,
            SkillType::LocalScript => self.build_local_script_audit_prompt(context, script_content)?,
            SkillType::Rhai => self.build_audit_prompt(context, script_content)?,
            SkillType::PromptOnly => self.build_prompt_only_audit_prompt(context, script_content)?,
        };

        prompt.push_str(&body);
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

        // Enriched context: project rules, evolution history, adjacent skills
        let enriched = self.gather_evolution_context(context);
        prompt.push_str(&self.format_enriched_context(&enriched));

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
        prompt.push_str("If you output `meta.yaml`, it must follow the minimal meta rules below.\n\n");
        prompt.push_str(Self::trigger_rules_prompt());

        Ok(prompt)
    }

    async fn compile_local_script(&self, skill_path: &Path) -> Result<(bool, Option<String>)> {
        let ext = skill_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");

        let output = match ext {
            "sh" | "bash" | "zsh" => std::process::Command::new("sh")
                .args(["-n", skill_path.to_str().unwrap_or("")])
                .output(),
            "js" => std::process::Command::new("node")
                .args(["--check", skill_path.to_str().unwrap_or("")])
                .output(),
            "php" => std::process::Command::new("php")
                .args(["-l", skill_path.to_str().unwrap_or("")])
                .output(),
            "rb" => std::process::Command::new("ruby")
                .args(["-c", skill_path.to_str().unwrap_or("")])
                .output(),
            "py" => std::process::Command::new("python3")
                .args(["-m", "py_compile", skill_path.to_str().unwrap_or("")])
                .output(),
            _ => {
                let content = std::fs::read_to_string(skill_path)
                    .map_err(|e| Error::Skill(format!("Failed to read local script: {}", e)))?;
                if content.trim().is_empty() {
                    return Ok((false, Some("Local script content is empty".to_string())));
                }
                return Ok((true, None));
            }
        };

        match output {
            Ok(out) if out.status.success() => Ok((true, None)),
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let message = if !stderr.trim().is_empty() {
                    stderr
                } else if !stdout.trim().is_empty() {
                    stdout
                } else {
                    format!("Local script syntax check failed for {:?}", skill_path.file_name())
                };
                Ok((false, Some(message)))
            }
            Err(e) => Ok((true, Some(format!("Syntax checker unavailable or failed to run: {}", e)))),
        }
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

    async fn compile_rhai_check(
        &self,
        evolution_id: &str,
        skill_name: &str,
        script_content: &str,
    ) -> Result<(bool, Option<String>)> {
        let temp_path = std::env::temp_dir().join(format!("{}_compile.rhai", skill_name));
        std::fs::write(&temp_path, script_content)?;

        info!(
            evolution_id = %evolution_id,
            content_len = script_content.len(),
            content_lines = script_content.lines().count(),
            "🔨 [compile] Script: {} chars, {} lines",
            script_content.len(),
            script_content.lines().count()
        );
        debug!(
            evolution_id = %evolution_id,
            "🔨 [compile] Script content:\n{}",
            script_content
        );

        info!(evolution_id = %evolution_id, "🔨 [compile] Compiling with Rhai engine...");
        let result = self.compile_skill(&temp_path).await;

        let _ = std::fs::remove_file(&temp_path);
        result
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

        // PromptOnly 写入 SKILL.md，Python 写入 SKILL.py，LocalScript 写入原始脚本，Rhai 技能写入 SKILL.rhai
        let skill_path = if let Some(source_path) = record.context.source_path.as_ref() {
            staged_skill_dir.join(source_path)
        } else {
            match record.context.skill_type {
                SkillType::PromptOnly => staged_skill_dir.join("SKILL.md"),
                SkillType::Python => staged_skill_dir.join("SKILL.py"),
                SkillType::LocalScript => staged_skill_dir.join("scripts/skill.sh"),
                SkillType::Rhai => staged_skill_dir.join("SKILL.rhai"),
            }
        };

        if let Some(parent) = skill_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

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
        let keep_dirs: &[&str] = &["tests", "manual"];

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_skills_dir(tag: &str) -> PathBuf {
        let mut root = std::env::temp_dir();
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        root.push(format!(
            "blockcell_hybrid_prompt_{}_{}_{}",
            tag,
            std::process::id(),
            now_ns
        ));
        std::fs::create_dir_all(&root).expect("create temp skills dir");
        root
    }

    fn sample_hybrid_context() -> EvolutionContext {
        EvolutionContext {
            skill_name: "hybrid_demo".to_string(),
            current_version: "v1".to_string(),
            trigger: TriggerReason::ManualRequest {
                description: "build a hybrid skill".to_string(),
            },
            error_stack: None,
            source_snippet: Some("print('hello')\n".to_string()),
            source_path: Some("SKILL.py".to_string()),
            layout: SkillLayout::Hybrid,
            tool_schemas: vec![],
            timestamp: chrono::Utc::now().timestamp(),
            skill_type: SkillType::Python,
            staged: false,
            staging_skills_dir: None,
        }
    }

    #[test]
    fn test_hybrid_generation_prompt_mentions_manual_and_entrypoint_boundary() {
        let skills_dir = temp_skills_dir("gen");
        let engine = SkillEvolution::new(skills_dir, 5);
        let prompt = engine
            .build_hybrid_generation_prompt(&sample_hybrid_context())
            .expect("build hybrid generation prompt");

        assert!(prompt.contains("## Hybrid Contract"));
        assert!(prompt.contains("SKILL.md` defines the user-facing behavior"));
        assert!(prompt.contains("Current entrypoint: `SKILL.py`"));
        assert!(prompt.contains("exec_local"));
        assert!(prompt.contains("local execution is appropriate"));
    }

    #[test]
    fn test_hybrid_fix_prompt_mentions_manual_and_entrypoint_boundary() {
        let skills_dir = temp_skills_dir("fix");
        let engine = SkillEvolution::new(skills_dir, 5);
        let feedback = FeedbackEntry {
            attempt: 1,
            stage: "compile".to_string(),
            feedback: "entrypoint mismatch".to_string(),
            previous_code: "print('bad')\n".to_string(),
            timestamp: chrono::Utc::now().timestamp(),
        };

        let prompt = engine
            .build_hybrid_fix_prompt(&sample_hybrid_context(), &feedback, &[])
            .expect("build hybrid fix prompt");

        assert!(prompt.contains("## Hybrid Contract"));
        assert!(prompt.contains("Keep the manual and the entrypoint aligned"));
        assert!(prompt.contains("Current entrypoint: `SKILL.py`"));
        assert!(prompt.contains("exec_local"));
        assert!(prompt.contains("entrypoint mismatch"));
    }
}
