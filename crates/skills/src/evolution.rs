use crate::versioning::{VersionManager, VersionSource};
use blockcell_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, info, warn};

pub(crate) fn validate_skill_name(skill_name: &str) -> Result<()> {
    let path = Path::new(skill_name);
    let mut components = path.components();
    let valid = matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none();
    if !valid || skill_name.trim().is_empty() {
        return Err(Error::Skill(format!("Invalid skill name: {}", skill_name)));
    }
    Ok(())
}

// --- submodules extracted from the original monolithic evolution.rs ---
mod audit;
mod compile;
mod compile_deploy;
mod context;
mod generate;
mod lifecycle;
mod prompts_audit;
mod prompts_gen;
mod versioning;

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

/// 根据 source_path 构建临时编译文件路径。
/// 无扩展名时不加后缀，保留原始文件名语义让 shebang fallback 生效；
/// 有扩展名时保留原扩展名。
fn make_compile_temp_path(skill_name: &str, source_path: &str) -> PathBuf {
    let source_ext = Path::new(source_path).extension().and_then(|e| e.to_str());
    match source_ext {
        Some(ext) => std::env::temp_dir().join(format!(
            "{}_compile_{}.{}",
            skill_name,
            RECORD_TMP_COUNTER.fetch_add(1, Ordering::Relaxed),
            ext
        )),
        None => std::env::temp_dir().join(format!(
            "{}_compile_{}",
            skill_name,
            RECORD_TMP_COUNTER.fetch_add(1, Ordering::Relaxed),
        )),
    }
}

/// RAII guard that removes a temporary file on drop.
/// Prevents file leaks if the enclosing function panics or is cancelled.
struct TempFileGuard {
    path: Option<PathBuf>,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn path(&self) -> &Path {
        self.path.as_ref().expect("TempFileGuard path is None")
    }

    /// Consume the guard without deleting the file (transfer ownership).
    #[allow(dead_code)]
    fn into_path(mut self) -> PathBuf {
        self.path.take().unwrap()
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// 技能自进化管理器
#[derive(Debug, Clone)]
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

pub(crate) enum LocalScriptSyntaxCheck {
    Shell(&'static str),
    Node,
    Php,
    Ruby,
    Python,
}

impl LocalScriptSyntaxCheck {
    fn run(self, skill_path: &Path) -> std::io::Result<std::process::Output> {
        let path = skill_path.to_str().unwrap_or("");
        match self {
            LocalScriptSyntaxCheck::Shell(shell) => std::process::Command::new(shell)
                .args(["-n", path])
                .output(),
            LocalScriptSyntaxCheck::Node => std::process::Command::new("node")
                .args(["--check", path])
                .output(),
            LocalScriptSyntaxCheck::Php => std::process::Command::new("php")
                .args(["-l", path])
                .output(),
            LocalScriptSyntaxCheck::Ruby => std::process::Command::new("ruby")
                .args(["-c", path])
                .output(),
            LocalScriptSyntaxCheck::Python => std::process::Command::new("python3")
                .args(["-m", "py_compile", path])
                .output(),
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

/// 契约检查结果（contract_check 持久化到 record）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractResult {
    pub passed: bool,
    #[serde(default)]
    pub issues: Vec<String>,
    pub checked_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditIssue {
    pub severity: String, // "error", "warning", "info"
    pub category: String, // "syntax", "permission", "loop", "leak"
    pub message: String,
}

/// Minimum length for a PromptTool skill body. Previously compile_check (50),
/// static audit (100), contract check (100) and the generation prompt (200)
/// each used a different number, so the same content could pass one gate and
/// fail another. Keep the single source of truth here.
pub(crate) const MIN_PROMPT_TOOL_CHARS: usize = 100;

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
    /// 契约检查（contract_check）结果，与 audit 同口径持久化。
    /// deploy_and_observe 要求该字段存在且 passed=true——防止不经 service
    /// 编排次序的调用方/resume 路径跳过契约检查直接部署。
    #[serde(default)]
    pub contract: Option<ContractResult>,
    /// 观察窗口（部署后的错误率监控）
    pub observation: Option<ObservationWindow>,
    #[serde(default)]
    pub observation_total_calls: u64,
    #[serde(default)]
    pub observation_error_calls: u64,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

// === Trait 定义 ===

#[async_trait::async_trait]
pub trait LLMProvider: Send + Sync {
    async fn generate(&self, prompt: &str) -> Result<String>;
}

#[cfg(test)]
mod tests;
/// meta.yaml 严格校验：必须是合法 YAML mapping，顶层字段限于 SkillMeta
/// 白名单（含 OpenClaw 兼容字段），且 name/description 非空。
/// 对应 trigger_rules_prompt 宣称的"禁止 legacy routing/formatting 字段"——
/// 此处从"prompt 约束"变为"代码强制"。
const META_ALLOWED_KEYS: &[&str] = &[
    "name",
    "description",
    "requires",
    "permissions",
    "always",
    "tools",
    "capabilities",
    "output_format",
    "fallback",
    "learned",
    "emoji",
    "os",
    "user_invocable",
    "disable_model_invocation",
    "commands",
    "install",
];

pub(crate) fn validate_meta_yaml(meta: &str) -> Vec<String> {
    let mut issues = Vec::new();
    match serde_yaml::from_str::<serde_yaml::Value>(meta) {
        Ok(serde_yaml::Value::Mapping(map)) => {
            for key in map.keys() {
                match key.as_str() {
                    Some(k) if META_ALLOWED_KEYS.contains(&k) => {}
                    Some(k) => issues.push(format!(
                        "meta.yaml: unknown field '{k}' (legacy routing/formatting fields are not allowed)"
                    )),
                    None => issues.push("meta.yaml: non-string top-level key".to_string()),
                }
            }
            if map
                .get(serde_yaml::Value::from("name"))
                .and_then(|v| v.as_str())
                .is_none_or(|s| s.trim().is_empty())
            {
                issues.push("meta.yaml: missing required 'name' field".to_string());
            }
            if map
                .get(serde_yaml::Value::from("description"))
                .and_then(|v| v.as_str())
                .is_none_or(|s| s.trim().is_empty())
            {
                issues.push("meta.yaml: missing required 'description' field".to_string());
            }
        }
        Ok(_) => issues.push("meta.yaml: top level must be a mapping".to_string()),
        Err(e) => issues.push(format!("meta.yaml: invalid YAML: {e}")),
    }
    issues
}
