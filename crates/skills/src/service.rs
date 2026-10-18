use crate::evolution::{
    EvolutionContext, EvolutionRecord, EvolutionStatus, FeedbackEntry, LLMProvider, SkillEvolution,
    SkillLayout, SkillType, TriggerReason,
};
use blockcell_core::{Error, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

mod error_tracker;
mod layout;
mod observation;
use error_tracker::ErrorTracker;
use observation::ObservationStats;

/// Result of a single evolution pipeline run.
///
/// Replaces the previous `bool` return from `run_single_evolution` to
/// eliminate the TOCTOU race between `is_evolution_pipeline_active()` and
/// `process_pending_evolution()`.  The lock decision is now atomic — callers
/// `process_pending_evolution()` 的原子化返回结果。
///
/// 区分 Completed / Failed / AlreadyRunning / WaitingForProvider / NotRunnable，
/// 消除调用方需要先检查 `is_evolution_pipeline_active()` 的 TOCTOU 竞态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SingleEvolutionResult {
    /// Pipeline 完成成功；技能已进入观察窗口或已完成。
    Completed,
    /// Pipeline 失败（所有重试耗尽）。
    Failed,
    /// 另一个调用方已持有该 evolution_id 的 pipeline 锁。
    AlreadyRunning,
    /// 无 LLM provider，record 仅标记为 Generating，等待 provider 配置后再推进。
    WaitingForProvider,
    /// 当前 record 状态不适合 pipeline worker 推进（非 runnable 状态）。
    NotRunnable(EvolutionStatus),
}

/// Built-in tool names that should NOT trigger skill evolution.
/// These are internal system tools — their failures are transient errors,
/// not missing skills that can be "learned".
const BUILTIN_TOOLS: &[&str] = &[
    "__llm_provider__",
    "read_file",
    "write_file",
    "edit_file",
    "list_dir",
    "exec",
    "web_search",
    "web_fetch",
    "browse",
    "message",
    "spawn",
    "list_tasks",
    "cron",
    "memory_query",
    "memory_upsert",
    "memory_forget",
    "list_skills",
    "system_info",
    "agent_status",
    "capability_evolve",
    "camera_capture",
    "app_control",
    "file_ops",
    "data_process",
    "http_request",
    "email",
    "audio_transcribe",
    "chart_generate",
    "office_write",
    "tts",
    "ocr",
    "image_understand",
    "video_process",
    "encrypt",
    "network_monitor",
    "knowledge_graph",
    "stream_subscribe",
    "alert_rule",
    "community_hub",
    "memory_maintenance",
    "toggle_manage",
    "termux_api",
    "session_recall",
    "exec_local",
    "exec_skill_script",
];

/// Check if a skill name is a built-in tool (should not trigger evolution).
pub fn is_builtin_tool(name: &str) -> bool {
    BUILTIN_TOOLS.contains(&name)
}

/// 技能记录摘要（用于 CLI 展示）
#[derive(Debug, Clone)]
pub struct SkillRecordSummary {
    pub skill_name: String,
    pub evolution_id: String,
    pub status: String,
    pub status_desc: String,
    pub created_at: i64,
    pub error_snippet: Option<String>,
}

/// 错误上报结果
#[derive(Debug, Clone)]
pub struct ErrorReport {
    /// 这是该技能在当前窗口内的第几次错误
    pub error_count: u32,
    /// 是否是第一次出错（用于通知用户）
    pub is_first_error: bool,
    /// 是否已有进化在进行中
    pub evolution_in_progress: bool,
    /// 如果达到阈值，触发的进化 ID
    pub evolution_triggered: Option<String>,
}

/// 能力执行错误上报结果
#[derive(Debug, Clone)]
pub struct CapabilityErrorReport {
    /// 当前窗口内的累计错误次数
    pub error_count: u32,
    /// 是否建议重新进化（错误达到阈值）
    pub should_re_evolve: bool,
}

/// 进化服务配置
#[derive(Debug, Clone)]
pub struct EvolutionServiceConfig {
    /// 触发进化所需的连续错误次数
    pub error_threshold: u32,
    /// 错误统计的时间窗口（分钟）
    pub error_window_minutes: u32,
    /// 是否启用自动进化
    pub enabled: bool,
    /// 每个阶段失败后的最大重试次数（静态审计/LLM 审计/编译/契约检查失败都会重试）
    pub max_retries: u32,
    /// LLM 调用超时时间（秒）
    pub llm_timeout_secs: u64,
    /// 回滚冷却期时长（分钟），默认 60 分钟
    pub cooldown_minutes: u32,
}

impl Default for EvolutionServiceConfig {
    fn default() -> Self {
        Self {
            error_threshold: 1,
            error_window_minutes: 30,
            enabled: true,
            max_retries: 3,
            llm_timeout_secs: 300, // 5分钟
            cooldown_minutes: 60,  // 1小时冷却期
        }
    }
}

/// 从 blockcell_core::EvolutionConfig 转换，接入真实配置链路
impl From<blockcell_core::EvolutionConfig> for EvolutionServiceConfig {
    fn from(cfg: blockcell_core::EvolutionConfig) -> Self {
        Self {
            error_threshold: cfg.error_threshold,
            error_window_minutes: cfg.error_window_minutes,
            enabled: cfg.enabled,
            max_retries: cfg.max_retries,
            llm_timeout_secs: cfg.llm_timeout_secs,
            cooldown_minutes: cfg.cooldown_minutes,
        }
    }
}

/// 进化服务：组合错误追踪、进化编排、灰度调度
///
/// 这是自升级系统的入口。外部通过以下方式交互：
/// - `report_error()`: 技能执行失败时调用，内部自动判断是否触发进化
type DeployCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// - `run_pending_evolutions()`: 执行待处理的进化流程（生成→审计→dry run→测试→发布）
/// - `tick()`: 定期调用，驱动灰度发布的阶段推进和自动回滚
/// - `set_llm_provider()`: 设置 LLM provider，使 tick() 能自动驱动完整 pipeline
#[derive(Clone)]
pub struct EvolutionService {
    evolution: SkillEvolution,
    error_tracker: Arc<Mutex<ErrorTracker>>,
    observation_stats: Arc<Mutex<ObservationStats>>,
    /// 当前正在进行中的 evolution_id 列表（skill_name -> evolution_id）
    active_evolutions: Arc<Mutex<HashMap<String, String>>>,
    /// P2-6: pipeline 并发互斥锁（正在执行 pipeline 的 evolution_id 集合）
    pipeline_locks: Arc<Mutex<HashSet<String>>>,
    config: EvolutionServiceConfig,
    /// 可选的 LLM provider，设置后 tick() 会自动驱动完整进化 pipeline
    llm_provider: Option<Arc<dyn LLMProvider>>,
    /// Callback invoked after a successful skill deployment.
    /// Used to invalidate caches (e.g. SkillDocCache) in the runtime.
    deploy_callback: Option<DeployCallback>,
}

impl EvolutionService {
    fn is_in_progress_status(status: &EvolutionStatus) -> bool {
        Self::is_pipeline_runnable_status(status)
            || matches!(*status.normalize(), EvolutionStatus::Observing)
    }

    fn is_pipeline_runnable_status(status: &EvolutionStatus) -> bool {
        matches!(
            *status.normalize(),
            EvolutionStatus::Triggered
                | EvolutionStatus::Generating
                | EvolutionStatus::Generated
                | EvolutionStatus::Auditing
                | EvolutionStatus::AuditFailed
                | EvolutionStatus::AuditPassed
                | EvolutionStatus::CompileFailed
                | EvolutionStatus::CompilePassed
        )
    }

    fn normalize_resumable_pipeline_record(&self, record: &mut EvolutionRecord) -> Result<()> {
        let next_status = match record.status.clone() {
            EvolutionStatus::Generating => {
                if record.patch.is_some() {
                    Some(EvolutionStatus::Generated)
                } else {
                    Some(EvolutionStatus::Triggered)
                }
            }
            EvolutionStatus::Auditing | EvolutionStatus::AuditFailed => {
                if record.patch.is_some() {
                    Some(EvolutionStatus::Generated)
                } else {
                    Some(EvolutionStatus::Triggered)
                }
            }
            EvolutionStatus::CompileFailed => {
                if record.patch.is_some() {
                    record.audit = None;
                    Some(EvolutionStatus::Generated)
                } else {
                    Some(EvolutionStatus::Triggered)
                }
            }
            _ => None,
        };

        if let Some(status) = next_status {
            record.status = status;
            record.updated_at = chrono::Utc::now().timestamp();
            self.evolution.save_record_public(record)?;
        }

        Ok(())
    }

    /// 扫描 records 目录找出该技能处于进行中状态的 evolution_id。
    /// 纯同步磁盘 IO；async 路径调用方应包 `spawn_blocking`。
    fn find_in_progress_record_in_dir(records_dir: &std::path::Path, skill_name: &str) -> Option<String> {
        let records = Self::load_records_from_dir(records_dir);
        records
            .into_iter()
            .find(|r| r.skill_name == skill_name && Self::is_in_progress_status(&r.status))
            .map(|r| r.id)
    }

    /// Reconcile in-memory `active_evolutions` with disk records and return the
    /// canonical in-progress evolution_id (if any) for this skill.
    async fn resolve_in_progress_evolution_id(&self, skill_name: &str) -> Option<String> {
        let disk_id = {
            let records_dir = self.records_dir();
            let skill_name_owned = skill_name.to_string();
            tokio::task::spawn_blocking(move || {
                Self::find_in_progress_record_in_dir(&records_dir, &skill_name_owned)
            })
            .await
            .ok()
            .flatten()
        };
        let mut active = self.active_evolutions.lock().await;
        match disk_id {
            Some(id) => {
                active.insert(skill_name.to_string(), id.clone());
                Some(id)
            }
            None => {
                active.remove(skill_name);
                None
            }
        }
    }

    fn truncate_chars(s: &str, max_chars: usize) -> String {
        if s.chars().count() <= max_chars {
            return s.to_string();
        }
        s.chars().take(max_chars).collect::<String>()
    }

    fn detect_skill_layout(
        &self,
        skill_name: &str,
    ) -> (SkillLayout, SkillType, Option<String>, Option<String>) {
        let skill_dir = self.evolution.skills_dir().join(skill_name);
        let has_skill_md = skill_dir.join("SKILL.md").exists();

        let rhai_path = skill_dir.join("SKILL.rhai");
        if rhai_path.exists() {
            return (
                SkillLayout::RhaiOrchestration,
                SkillType::Rhai,
                std::fs::read_to_string(&rhai_path).ok(),
                Some("SKILL.rhai".to_string()),
            );
        }

        let py_path = skill_dir.join("SKILL.py");
        if py_path.exists() {
            let layout = if has_skill_md {
                SkillLayout::Hybrid
            } else {
                SkillLayout::LocalScript
            };
            return (
                layout,
                SkillType::Python,
                std::fs::read_to_string(&py_path).ok(),
                Some("SKILL.py".to_string()),
            );
        }

        if let Some(legacy_py_path) = Self::first_legacy_python_script(&skill_dir) {
            let rel = legacy_py_path
                .strip_prefix(&skill_dir)
                .ok()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| legacy_py_path.display().to_string());

            let legacy_code = std::fs::read_to_string(&legacy_py_path)
                .ok()
                .map(|s| Self::truncate_chars(&s, 8_000))
                .unwrap_or_default();

            let skill_md = std::fs::read_to_string(skill_dir.join("SKILL.md"))
                .ok()
                .map(|s| Self::truncate_chars(&s, 3_000));

            let mut snippet = format!("# Legacy OpenClaw script: {}\n{}", rel, legacy_code);
            if let Some(md) = skill_md {
                snippet.push_str("\n\n# Current SKILL.md\n");
                snippet.push_str(&md);
            }

            let layout = if has_skill_md {
                SkillLayout::Hybrid
            } else {
                SkillLayout::LocalScript
            };

            let source_path = legacy_py_path
                .strip_prefix(&skill_dir)
                .ok()
                .map(|p| p.display().to_string());

            return (layout, SkillType::Python, Some(snippet), source_path);
        }

        if let Some(local_script_path) = Self::first_local_script_asset(&skill_dir) {
            let rel = local_script_path
                .strip_prefix(&skill_dir)
                .ok()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| local_script_path.display().to_string());

            let script_code = std::fs::read_to_string(&local_script_path)
                .ok()
                .map(|s| Self::truncate_chars(&s, 8_000))
                .unwrap_or_default();

            let skill_md = std::fs::read_to_string(skill_dir.join("SKILL.md"))
                .ok()
                .map(|s| Self::truncate_chars(&s, 3_000));

            let mut snippet = format!("# Local script asset: {}\n{}", rel, script_code);
            if let Some(md) = skill_md {
                snippet.push_str("\n\n# Current SKILL.md\n");
                snippet.push_str(&md);
            }

            let layout = if has_skill_md {
                SkillLayout::Hybrid
            } else {
                SkillLayout::LocalScript
            };

            return (layout, SkillType::LocalScript, Some(snippet), Some(rel));
        }

        let md_path = skill_dir.join("SKILL.md");
        if md_path.exists() {
            return (
                SkillLayout::PromptTool,
                SkillType::PromptOnly,
                std::fs::read_to_string(&md_path).ok(),
                Some("SKILL.md".to_string()),
            );
        }

        (SkillLayout::PromptTool, SkillType::PromptOnly, None, None)
    }

    pub fn new(skills_dir: PathBuf, config: EvolutionServiceConfig) -> Self {
        let error_tracker = ErrorTracker::new(
            config.error_threshold,
            config.error_window_minutes,
            config.cooldown_minutes,
        );

        Self {
            evolution: SkillEvolution::new(skills_dir, config.llm_timeout_secs),
            error_tracker: Arc::new(Mutex::new(error_tracker)),
            observation_stats: Arc::new(Mutex::new(ObservationStats::default())),
            active_evolutions: Arc::new(Mutex::new(HashMap::new())),
            pipeline_locks: Arc::new(Mutex::new(HashSet::new())),
            config,
            llm_provider: None,
            deploy_callback: None,
        }
    }

    /// 设置 LLM provider，使 tick() 能自动驱动完整进化 pipeline。
    /// 应在 agent 启动时调用，传入与主 agent 相同的 provider。
    pub fn set_llm_provider(&mut self, provider: Arc<dyn LLMProvider>) {
        self.llm_provider = Some(provider);
    }

    /// Set a callback to be invoked after a successful skill deployment.
    /// The callback receives the skill name as its argument.
    /// Used to invalidate caches (e.g. SkillDocCache) after evolution deploy.
    pub fn set_deploy_callback(&mut self, callback: Arc<dyn Fn(&str) + Send + Sync>) {
        self.deploy_callback = Some(callback);
    }

    /// 报告技能执行错误
    ///
    /// 每次调用都会返回 ErrorReport，包含：
    /// - `is_first_error`: 是否是该技能第一次出错（用于立即通知用户）
    /// - `error_count`: 当前窗口内的累计错误次数
    /// - `evolution_in_progress`: 是否已有进化在进行中
    /// - `evolution_triggered`: 如果达到阈值，返回触发的 evolution_id
    pub async fn report_error(
        &self,
        skill_name: &str,
        error_msg: &str,
        source_snippet: Option<String>,
        tool_schemas: Vec<serde_json::Value>,
    ) -> Result<ErrorReport> {
        if !self.config.enabled {
            return Ok(ErrorReport {
                error_count: 0,
                is_first_error: false,
                evolution_in_progress: false,
                evolution_triggered: None,
            });
        }

        // Skip built-in tools — their failures are transient, not learnable skills
        if is_builtin_tool(skill_name) {
            debug!(
                skill = %skill_name,
                "Skipping evolution for built-in tool `{}`",
                skill_name
            );
            return Ok(ErrorReport {
                error_count: 0,
                is_first_error: false,
                evolution_in_progress: false,
                evolution_triggered: None,
            });
        }

        // 以磁盘记录作为主事实来源，避免多实例内存态漂移导致误判。
        let existing_evolution_id = self.resolve_in_progress_evolution_id(skill_name).await;
        let already_evolving = existing_evolution_id.is_some();

        let track_result = {
            let mut tracker = self.error_tracker.lock().await;
            tracker.record_error(skill_name)
        };

        if already_evolving {
            info!(
                skill = %skill_name,
                evolution_id = ?existing_evolution_id,
                error_count = track_result.count,
                "🧠 [自进化] 技能 `{}` 执行出错 (第{}次)，该技能已在学习改进中",
                skill_name, track_result.count
            );
            return Ok(ErrorReport {
                error_count: track_result.count,
                is_first_error: track_result.is_first,
                evolution_in_progress: true,
                evolution_triggered: None,
            });
        }

        // 未达到阈值，只返回计数信息
        let trigger = match track_result.trigger {
            Some(t) => t,
            None => {
                info!(
                    skill = %skill_name,
                    error_count = track_result.count,
                    threshold = self.config.error_threshold,
                    "🧠 [自进化] 技能 `{}` 执行出错 (第{}/{}次)，尚未达到进化阈值",
                    skill_name, track_result.count, self.config.error_threshold
                );
                return Ok(ErrorReport {
                    error_count: track_result.count,
                    is_first_error: track_result.is_first,
                    evolution_in_progress: false,
                    evolution_triggered: None,
                });
            }
        };

        // 达到阈值，触发进化
        info!(
            skill = %skill_name,
            "🧠 [自进化] 技能 `{}` 错误达到阈值，触发自动进化学习！",
            skill_name
        );

        let current_version = self
            .evolution
            .version_manager()
            .get_current_version(skill_name)
            .unwrap_or_else(|_| "unknown".to_string());

        // 检测技能布局（支持 PromptTool / LocalScript / Hybrid / RhaiOrchestration）
        let (layout, skill_type, inferred_source_snippet, source_path) =
            self.detect_skill_layout(skill_name);
        let source_snippet = source_snippet.or(inferred_source_snippet);

        let context = EvolutionContext {
            skill_name: skill_name.to_string(),
            current_version,
            trigger,
            error_stack: Some(error_msg.to_string()),
            source_snippet,
            source_path,
            layout,
            tool_schemas,
            timestamp: chrono::Utc::now().timestamp(),
            skill_type,
            staged: false,
            staging_skills_dir: None,
        };

        let evolution_id = self.evolution.trigger_evolution(context).await?;

        {
            let mut active = self.active_evolutions.lock().await;
            active.insert(skill_name.to_string(), evolution_id.clone());
        }

        Ok(ErrorReport {
            error_count: track_result.count,
            is_first_error: track_result.is_first,
            evolution_in_progress: false,
            evolution_triggered: Some(evolution_id),
        })
    }

    /// 执行待处理的进化流程（完整 pipeline）
    ///
    /// 流程：生成补丁 → 审计 → 编译检查 → 部署+观察
    /// 需要 LLM provider 来驱动。
    pub async fn run_pending_evolutions(
        &self,
        llm_provider: &dyn LLMProvider,
    ) -> Result<Vec<String>> {
        let active = self.active_evolutions.lock().await;
        let pending: Vec<(String, String)> =
            active.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        drop(active);

        let mut completed = Vec::new();

        for (skill_name, evolution_id) in pending {
            match self.run_single_evolution(&evolution_id, llm_provider).await {
                Ok(SingleEvolutionResult::Completed) => {
                    info!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        "Evolution pipeline completed, observation started"
                    );
                    completed.push(evolution_id);
                }
                Ok(SingleEvolutionResult::Failed) => {
                    warn!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        "Evolution pipeline failed at some stage"
                    );
                    self.cleanup_evolution(&skill_name, &evolution_id).await;
                }
                Ok(SingleEvolutionResult::AlreadyRunning) => {
                    info!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        "Evolution pipeline already running, skipping"
                    );
                }
                Ok(SingleEvolutionResult::WaitingForProvider) => {
                    info!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        "Evolution waiting for LLM provider"
                    );
                }
                Ok(SingleEvolutionResult::NotRunnable(status)) => {
                    if matches!(
                        *status.normalize(),
                        EvolutionStatus::Failed | EvolutionStatus::RolledBack
                    ) {
                        warn!(
                            skill = %skill_name,
                            evolution_id = %evolution_id,
                            status = ?status,
                            "Evolution reached terminal non-runnable status, cleaning up"
                        );
                        self.cleanup_evolution(&skill_name, &evolution_id).await;
                    } else {
                        info!(
                            skill = %skill_name,
                            evolution_id = %evolution_id,
                            status = ?status,
                            "Evolution not runnable, skipping"
                        );
                    }
                }
                Err(e) => {
                    error!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        error = %e,
                        "Evolution pipeline error"
                    );
                    self.cleanup_evolution(&skill_name, &evolution_id).await;
                }
            }
        }

        Ok(completed)
    }

    /// 执行单个进化的完整 pipeline（带重试机制）
    ///
    /// 流程：1. 生成补丁 → 2. 审计 → 3. 编译检查 → 4. 部署+观察
    ///
    /// 如果审计/编译失败，会将失败反馈给 LLM 重新生成，最多重试 max_retries 次。
    ///
    /// 返回 `SingleEvolutionResult` 而非 `bool`，消除 `is_evolution_pipeline_active()`
    /// 与 `process_pending_evolution()` 之间的 TOCTOU 竞态。
    async fn run_single_evolution(
        &self,
        evolution_id: &str,
        llm_provider: &dyn LLMProvider,
    ) -> Result<SingleEvolutionResult> {
        // P2-6: 获取 pipeline 锁，防止同一 evolution 并发执行。
        // RAII guard：future 被取消（select/timeout/abort）或 inner panic 时
        // Drop 兜底释放，不会留下永久 AlreadyRunning 的泄漏锁。
        let _guard = {
            let mut locks = self.pipeline_locks.lock().await;
            if locks.contains(evolution_id) {
                info!(evolution_id = %evolution_id, "🧠 [pipeline] Already running, skipping");
                return Ok(SingleEvolutionResult::AlreadyRunning);
            }
            locks.insert(evolution_id.to_string());
            crate::core_evolution::PipelineLockGuard::new(
                Arc::clone(&self.pipeline_locks),
                evolution_id.to_string(),
            )
        };

        // 跨进程互斥：pipeline_locks 仅进程内。gateway/CLI 双实例会各自
        // adopt_orphaned_records 采纳同一磁盘记录并驱动同一 evolution
        // （双份 LLM 成本、重复建版本、观察窗口双开）。fs2 advisory lock
        // 覆盖同记录目录；对进程死亡自动释放。获取失败视为已有实例在跑。
        let records_root = self.evolution.records_dir();
        let _file_guard = {
            let evolution_id_owned = evolution_id.to_string();
            let root = records_root.clone();
            match tokio::task::spawn_blocking(move || {
                crate::file_owner_lock::FileOwnerLock::acquire(
                    &root,
                    "pipeline",
                    &evolution_id_owned,
                )
            })
            .await
            {
                Ok(Ok(guard)) => Some(guard),
                Ok(Err(e)) => {
                    warn!(
                        evolution_id = %evolution_id,
                        error = %e,
                        "🧠 [pipeline] Cross-process lock busy or unavailable, skipping"
                    );
                    return Ok(SingleEvolutionResult::AlreadyRunning);
                }
                Err(e) => {
                    warn!(
                        evolution_id = %evolution_id,
                        error = %e,
                        "🧠 [pipeline] Lock task join failed, skipping"
                    );
                    return Ok(SingleEvolutionResult::AlreadyRunning);
                }
            }
        };

        self.run_single_evolution_inner(evolution_id, llm_provider)
            .await
    }

    /// pipeline 内部实现（被 run_single_evolution 包装以管理锁）
    async fn run_single_evolution_inner(
        &self,
        evolution_id: &str,
        llm_provider: &dyn LLMProvider,
    ) -> Result<SingleEvolutionResult> {
        let max_retries = self.config.max_retries;
        let record = self.evolution.load_record(evolution_id)?;
        info!(
            evolution_id = %evolution_id,
            skill = %record.skill_name,
            current_status = ?record.status,
            max_retries = max_retries,
            "🧠 [pipeline] Starting pipeline (max {} retries), current status: {:?}",
            max_retries, record.status
        );

        // ═══════════════════════════════════════════════════════════
        // Step 1: 初次生成补丁
        // ═══════════════════════════════════════════════════════════
        if record.status == EvolutionStatus::Triggered {
            info!(evolution_id = %evolution_id, "🧠 [pipeline] ═══ Step 1: Generating initial patch ═══");
            let patch = self
                .evolution
                .generate_patch(evolution_id, llm_provider)
                .await?;
            info!(
                evolution_id = %evolution_id,
                patch_id = %patch.patch_id,
                diff_len = patch.diff.len(),
                "🧠 [pipeline] Step 1 DONE: initial patch generated ({})",
                patch.patch_id
            );
        }

        // ═══════════════════════════════════════════════════════════
        // Step 2+3: 审计 → 编译检查（带重试循环）
        // ═══════════════════════════════════════════════════════════
        let mut attempt = 0u32;
        loop {
            attempt += 1;

            if attempt > max_retries + 1 {
                warn!(
                    evolution_id = %evolution_id,
                    attempts = attempt - 1,
                    "🧠 [pipeline] ❌ Exhausted all {} retries, giving up",
                    max_retries
                );
                return Ok(SingleEvolutionResult::Failed);
            }

            if attempt > 1 {
                info!(
                    evolution_id = %evolution_id,
                    attempt = attempt,
                    "🧠 [pipeline] ═══ Retry attempt #{}/{} ═══",
                    attempt - 1, max_retries
                );
            }

            // --- 2a. 静态审计（确定性规则检查，零 LLM 成本）---
            let record = self.evolution.load_record(evolution_id)?;
            if record.status == EvolutionStatus::Generated {
                if let Some(ref patch) = record.patch {
                    let static_result =
                        crate::audit::static_audit(&record.context.skill_type, &patch.diff);
                    if !static_result.passed {
                        let feedback_text =
                            crate::audit::format_static_audit_feedback(&static_result);
                        warn!(
                            evolution_id = %evolution_id,
                            violations = static_result.violations.len(),
                            "🧠 [pipeline] Static audit FAILED ({} violations), regenerating",
                            static_result.violations.len()
                        );

                        let feedback = FeedbackEntry {
                            attempt: record.attempt,
                            stage: "static_audit".to_string(),
                            feedback: feedback_text,
                            previous_code: patch.diff.clone(),
                            timestamp: chrono::Utc::now().timestamp(),
                        };

                        self.evolution
                            .regenerate_with_feedback(evolution_id, llm_provider, &feedback)
                            .await?;
                        continue;
                    }
                    // Log warnings even if passed
                    let warnings: Vec<_> = static_result
                        .violations
                        .iter()
                        .filter(|v| v.severity == "warning")
                        .collect();
                    if !warnings.is_empty() {
                        info!(
                            evolution_id = %evolution_id,
                            "🧠 [pipeline] Static audit passed with {} warning(s)",
                            warnings.len()
                        );
                    }
                }
            }

            // --- 2b. LLM 审计 ---
            let record = self.evolution.load_record(evolution_id)?;
            if record.status == EvolutionStatus::Generated {
                info!(evolution_id = %evolution_id, "🧠 [pipeline] ═══ Auditing patch (attempt {}) ═══", attempt);
                let audit = self
                    .evolution
                    .audit_patch(evolution_id, llm_provider)
                    .await?;

                if !audit.passed {
                    let issues_text = audit
                        .issues
                        .iter()
                        .map(|i| format!("[{}][{}] {}", i.severity, i.category, i.message))
                        .collect::<Vec<_>>()
                        .join("\n");

                    warn!(
                        evolution_id = %evolution_id,
                        issues = audit.issues.len(),
                        "🧠 [pipeline] Audit FAILED ({} issues), will regenerate with feedback",
                        audit.issues.len()
                    );

                    let current_code = record
                        .patch
                        .as_ref()
                        .map(|p| p.diff.clone())
                        .unwrap_or_default();

                    let feedback = FeedbackEntry {
                        attempt: record.attempt,
                        stage: "audit".to_string(),
                        feedback: format!(
                            "Audit found {} issues:\n{}",
                            audit.issues.len(),
                            issues_text
                        ),
                        previous_code: current_code,
                        timestamp: chrono::Utc::now().timestamp(),
                    };

                    self.evolution
                        .regenerate_with_feedback(evolution_id, llm_provider, &feedback)
                        .await?;
                    continue;
                }
                info!(evolution_id = %evolution_id, "🧠 [pipeline] ✅ Audit passed (attempt {})", attempt);
            }

            // --- 3. 编译检查（合并了原 dry_run + shadow_test）---
            let record = self.evolution.load_record(evolution_id)?;
            if record.status == EvolutionStatus::AuditPassed {
                info!(evolution_id = %evolution_id, "🧠 [pipeline] ═══ Compile check (attempt {}) ═══", attempt);
                let (passed, compile_error) = self.evolution.compile_check(evolution_id).await?;

                if !passed {
                    let error_msg =
                        compile_error.unwrap_or_else(|| "Unknown compilation error".to_string());
                    warn!(
                        evolution_id = %evolution_id,
                        "🧠 [pipeline] Compile FAILED: {}, will regenerate with feedback",
                        error_msg
                    );

                    let current_code = record
                        .patch
                        .as_ref()
                        .map(|p| p.diff.clone())
                        .unwrap_or_default();

                    let feedback = FeedbackEntry {
                        attempt: record.attempt,
                        stage: "compile".to_string(),
                        feedback: format!("Rhai compilation failed with error:\n{}", error_msg),
                        previous_code: current_code,
                        timestamp: chrono::Utc::now().timestamp(),
                    };

                    self.evolution
                        .regenerate_with_feedback(evolution_id, llm_provider, &feedback)
                        .await?;
                    continue;
                }
                info!(evolution_id = %evolution_id, "🧠 [pipeline] ✅ Compile check passed (attempt {})", attempt);
            }

            // --- 3b. 契约一致性检查（SKILL.md/meta.yaml 结构验证）---
            let record = self.evolution.load_record(evolution_id)?;
            if record.status.is_compile_passed() {
                info!(evolution_id = %evolution_id, "🧠 [pipeline] ═══ Contract check (attempt {}) ═══", attempt);
                let (contract_passed, contract_error) =
                    self.evolution.contract_check(evolution_id)?;

                if !contract_passed {
                    let error_msg =
                        contract_error.unwrap_or_else(|| "Unknown contract violation".to_string());
                    warn!(
                        evolution_id = %evolution_id,
                        "🧠 [pipeline] Contract check FAILED: {}, will regenerate with feedback",
                        error_msg
                    );

                    let current_code = record
                        .patch
                        .as_ref()
                        .map(|p| p.diff.clone())
                        .unwrap_or_default();

                    let feedback = FeedbackEntry {
                        attempt: record.attempt,
                        stage: "contract".to_string(),
                        feedback: format!("Contract validation failed:\n{}", error_msg),
                        previous_code: current_code,
                        timestamp: chrono::Utc::now().timestamp(),
                    };

                    self.evolution
                        .regenerate_with_feedback(evolution_id, llm_provider, &feedback)
                        .await?;
                    continue;
                }
                info!(evolution_id = %evolution_id, "🧠 [pipeline] ✅ Contract check passed (attempt {})", attempt);
            }

            // 所有检查都通过了，跳出循环
            break;
        }

        // ═══════════════════════════════════════════════════════════
        // Step 4: 部署 + 进入观察窗口
        // ═══════════════════════════════════════════════════════════
        let record = self.evolution.load_record(evolution_id)?;
        if record.status.is_compile_passed() {
            info!(evolution_id = %evolution_id, "🧠 [pipeline] ═══ Step 4: Deploy and observe ═══");
            self.evolution.deploy_and_observe(evolution_id).await?;

            // 初始化观察期统计
            let mut stats = self.observation_stats.lock().await;
            stats.active.insert(evolution_id.to_string(), (0, 0));
            info!(evolution_id = %evolution_id, "🧠 [pipeline] Step 4 DONE: deployed, observation started");
        }

        let record = self.evolution.load_record(evolution_id)?;
        info!(
            evolution_id = %evolution_id,
            final_status = ?record.status,
            total_attempts = record.attempt,
            "🧠 [pipeline] ═══ Pipeline completed successfully (after {} attempt(s)) ═══",
            record.attempt
        );
        Ok(SingleEvolutionResult::Completed)
    }

    /// 定时调度器 tick
    ///
    /// 应由外部定时调用（建议每 60 秒一次）。
    /// 1. 处理待执行的进化（Triggered 状态 → 驱动完整 pipeline）
    /// 2. 检查所有正在观察中的进化记录：
    ///    - 如果错误率超过阈值 → 自动回滚
    ///    - 如果观察窗口到期且错误率正常 → 标记完成，清理资源
    pub async fn tick(&self) -> Result<()> {
        // Phase 1: Process pending evolutions (Triggered → run pipeline)
        let has_llm = self.llm_provider.is_some();
        debug!(
            has_llm = has_llm,
            records_dir = %self.records_dir().display(),
            "🧠 [自进化] tick() 开始 (LLM provider: {})",
            if has_llm { "已配置" } else { "未配置" }
        );
        let pending = self.list_pending_ids().await;
        if !pending.is_empty() {
            info!(
                count = pending.len(),
                has_llm = has_llm,
                "🧠 [自进化] 发现 {} 个待处理的进化任务 (LLM: {})",
                pending.len(),
                if has_llm { "ready" } else { "none" }
            );
        }
        for (skill_name, evolution_id) in &pending {
            info!(
                skill = %skill_name,
                evolution_id = %evolution_id,
                "🧠 [自进化] 开始处理技能 `{}` 的进化 ({})",
                skill_name, evolution_id
            );
            if let Err(e) = self
                .process_pending_evolution(skill_name, evolution_id)
                .await
            {
                error!(
                    skill = %skill_name,
                    evolution_id = %evolution_id,
                    error = %e,
                    "🧠 [自进化] 处理进化失败"
                );
            }
        }

        self.tick_observations().await?;

        Ok(())
    }

    /// Check observation windows without running the generation pipeline.
    pub async fn tick_observations(&self) -> Result<()> {
        self.adopt_orphaned_records().await;

        let active = self.active_evolutions.lock().await;
        let observing: Vec<(String, String)> =
            active.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        drop(active);

        for (skill_name, evolution_id) in observing {
            if let Err(e) = self
                .tick_single_observation(&skill_name, &evolution_id)
                .await
            {
                error!(
                    evolution_id = %evolution_id,
                    error = %e,
                    "🧠 [自进化] 观察窗口 tick 错误"
                );
            }
        }

        Ok(())
    }

    /// 处理一个待进化的技能。
    ///
    /// 如果 LLM provider 已配置，运行完整 pipeline（generate→audit→compile→deploy+observe）。
    /// 否则仅标记为 Generating，等待 provider 配置后再推进。
    ///
    /// 返回 `SingleEvolutionResult` 以原子化区分各种结果状态，
    /// 消除调用方需要先检查 `is_evolution_pipeline_active()` 的 TOCTOU 竞态。
    pub async fn process_pending_evolution(
        &self,
        skill_name: &str,
        evolution_id: &str,
    ) -> Result<SingleEvolutionResult> {
        let record = self.evolution.load_record(evolution_id)?;

        // 非 runnable 状态：区分真正完成 vs 未完成但暂不可推进
        if !Self::is_pipeline_runnable_status(&record.status) {
            let normalized = record.status.normalize();
            if matches!(
                *normalized,
                EvolutionStatus::Observing | EvolutionStatus::Completed
            ) {
                // 真正完成 — 只有 Observing/Completed 才返回 Completed
                return Ok(SingleEvolutionResult::Completed);
            }
            // 其他非 runnable 状态（如 Failed/RolledBack/Generating 等），
            // 不应被当作 Completed，返回 NotRunnable 让 worker 走 defer 路径
            return Ok(SingleEvolutionResult::NotRunnable(record.status.clone()));
        }

        let mut record = record;
        self.normalize_resumable_pipeline_record(&mut record)?;

        info!(
            skill = %skill_name,
            evolution_id = %evolution_id,
            trigger = ?record.context.trigger,
            "🧠 [自进化] 技能 `{}` 触发原因: {:?}",
            skill_name, record.context.trigger
        );

        if let Some(error_stack) = &record.context.error_stack {
            info!(
                skill = %skill_name,
                "🧠 [自进化] 错误信息: {}",
                if error_stack.chars().count() > 200 {
                    format!("{}...", error_stack.chars().take(200).collect::<String>())
                } else {
                    error_stack.clone()
                }
            );
        }

        // 如果 LLM provider 已配置，运行完整 pipeline
        if let Some(ref llm_provider) = self.llm_provider {
            info!(
                skill = %skill_name,
                evolution_id = %evolution_id,
                "🧠 [自进化] LLM provider 可用，开始执行完整进化 pipeline"
            );
            let result = self
                .run_single_evolution(evolution_id, llm_provider.as_ref())
                .await;

            match &result {
                Ok(SingleEvolutionResult::Completed) => {
                    info!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        "🧠 [自进化] 技能 `{}` 进化 pipeline 完成，观察窗口已启动",
                        skill_name
                    );
                    // 注意：此处不触发 deploy_callback，因为只是进入 Observing 状态，
                    // 观察期仍可能回滚。callback 应在观察期最终 Completed 后触发。
                }
                Ok(SingleEvolutionResult::Failed) => {
                    warn!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        "🧠 [自进化] 技能 `{}` 进化 pipeline 失败（所有重试已耗尽）",
                        skill_name
                    );
                    self.cleanup_evolution(skill_name, evolution_id).await;
                }
                Ok(SingleEvolutionResult::AlreadyRunning) => {
                    info!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        "🧠 [自进化] 技能 `{}` 进化 pipeline 已在运行中（由其他 worker/tick 驱动），跳过",
                        skill_name
                    );
                }
                Ok(SingleEvolutionResult::WaitingForProvider) => {
                    // 不会到达此分支 — 有 provider 时不会返回 WaitingForProvider
                }
                Ok(SingleEvolutionResult::NotRunnable(status)) => {
                    info!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        status = ?status,
                        "🧠 [自进化] 技能 `{}` 当前状态 {:?} 不适合 pipeline 推进，跳过",
                        skill_name, status
                    );
                }
                Err(e) => {
                    error!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        error = %e,
                        "🧠 [自进化] 技能 `{}` 进化 pipeline 出错: {}",
                        skill_name, e
                    );
                    self.cleanup_evolution(skill_name, evolution_id).await;
                }
            }

            result
        } else {
            // 无 LLM provider — 仅标记为 Generating，返回 WaitingForProvider
            info!(
                skill = %skill_name,
                evolution_id = %evolution_id,
                "🧠 [自进化] 无 LLM provider，技能 `{}` 标记为学习中 (Generating)，等待 provider 配置",
                skill_name
            );
            let mut updated_record = record;
            updated_record.status = EvolutionStatus::Generating;
            updated_record.updated_at = chrono::Utc::now().timestamp();
            self.evolution.save_record_public(&updated_record)?;
            Ok(SingleEvolutionResult::WaitingForProvider)
        }
    }

    /// P1: 观察窗口 tick — 检查错误率和观察时间
    async fn tick_single_observation(&self, skill_name: &str, evolution_id: &str) -> Result<()> {
        let record = match self.evolution.load_record(evolution_id) {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };

        // 只处理 Observing 状态（兼容旧 RollingOut）
        let status = record.status.normalize();
        if *status != EvolutionStatus::Observing {
            // 如果已完成或已回滚，清理
            if *status == EvolutionStatus::Completed
                || *status == EvolutionStatus::RolledBack
                || *status == EvolutionStatus::Failed
            {
                self.cleanup_evolution(skill_name, evolution_id).await;
            }
            return Ok(());
        }

        let stats = self.observation_stats.lock().await;
        let memory_error_rate = stats.error_rate(evolution_id);
        drop(stats);
        let error_rate = if record.observation_total_calls > 0 {
            record.observation_error_calls as f64 / record.observation_total_calls as f64
        } else {
            memory_error_rate
        };

        // 使用 check_observation 检查观察窗口状态
        match self.evolution.check_observation(evolution_id, error_rate)? {
            Some(true) => {
                // 观察完成，标记成功
                info!(
                    evolution_id = %evolution_id,
                    skill = %skill_name,
                    error_rate = error_rate,
                    "🧠 [观察] 观察窗口到期，错误率正常，标记完成"
                );
                self.evolution.mark_completed(evolution_id)?;
                // 观察期最终成功后才触发 deploy_callback，避免记录可能被回滚的"成功"
                if let Some(ref cb) = self.deploy_callback {
                    cb(skill_name);
                }
                self.persist_evolution_experience(evolution_id);
                self.cleanup_evolution(skill_name, evolution_id).await;
            }
            Some(false) => {
                // 错误率超阈值，回滚
                warn!(
                    evolution_id = %evolution_id,
                    error_rate = error_rate,
                    "🧠 [观察] 错误率超阈值，回滚"
                );
                self.evolution
                    .rollback(
                        evolution_id,
                        &format!(
                            "Error rate {:.2}% exceeded threshold during observation",
                            error_rate * 100.0,
                        ),
                    )
                    .await?;
                self.cleanup_evolution_rollback(skill_name, evolution_id)
                    .await;
            }
            None => {
                // 仍在观察中，不做操作
            }
        }

        Ok(())
    }

    /// 报告能力执行错误（统一错误追踪）
    pub async fn report_capability_error(
        &self,
        capability_id: &str,
        _error_msg: &str,
    ) -> CapabilityErrorReport {
        if !self.config.enabled {
            return CapabilityErrorReport {
                error_count: 0,
                should_re_evolve: false,
            };
        }

        let track_result = {
            let mut tracker = self.error_tracker.lock().await;
            tracker.record_error(capability_id)
        };

        if track_result.trigger.is_some() {
            info!(
                capability_id = %capability_id,
                error_count = track_result.count,
                "🧬 [能力错误] 能力 `{}` 错误达到阈值，建议重新进化",
                capability_id
            );
            CapabilityErrorReport {
                error_count: track_result.count,
                should_re_evolve: true,
            }
        } else {
            debug!(
                capability_id = %capability_id,
                error_count = track_result.count,
                threshold = self.config.error_threshold,
                "🧬 [能力错误] 能力 `{}` 执行出错 ({}/{})",
                capability_id, track_result.count, self.config.error_threshold
            );
            CapabilityErrorReport {
                error_count: track_result.count,
                should_re_evolve: false,
            }
        }
    }

    /// 报告观察期间的技能调用结果（供外部在执行技能后调用）
    pub async fn report_skill_call(&self, skill_name: &str, is_error: bool) {
        let evolution_id = {
            let active = self.active_evolutions.lock().await;
            active.get(skill_name).cloned()
        };
        let evolution_id = match evolution_id {
            Some(id) => Some(id),
            None => {
                // 磁盘全量扫描属同步 IO，移出 async executor
                let records_dir = self.records_dir();
                let skill_name_owned = skill_name.to_string();
                tokio::task::spawn_blocking(move || {
                    Self::find_in_progress_record_in_dir(&records_dir, &skill_name_owned)
                })
                .await
                .ok()
                .flatten()
            }
        };

        if let Some(evolution_id) = evolution_id {
            {
                let mut active = self.active_evolutions.lock().await;
                active.insert(skill_name.to_string(), evolution_id.clone());
            }

            {
                let mut stats = self.observation_stats.lock().await;
                stats.record_call(&evolution_id, is_error);
            }

            // record_observation_call 是同步 fs 读-改-写（含文件锁），不能阻塞 worker
            let evolution = self.evolution.clone();
            let evolution_id_owned = evolution_id.clone();
            let persist_result = tokio::task::spawn_blocking(move || {
                evolution.record_observation_call(&evolution_id_owned, is_error)
            })
            .await;
            let persist_error = match persist_result {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e.to_string()),
                Err(e) => Some(format!("spawn_blocking join error: {e}")),
            };
            if let Some(e) = persist_error {
                warn!(
                    skill = %skill_name,
                    evolution_id = %evolution_id,
                    error = %e,
                    "🧠 [观察] 持久化观察期调用统计失败"
                );
            }
        }
    }

    /// 检查某个技能是否在观察期中
    pub async fn is_observing(&self, skill_name: &str) -> bool {
        let active = self.active_evolutions.lock().await;
        if let Some(evolution_id) = active.get(skill_name) {
            if let Ok(record) = self.evolution.load_record(evolution_id) {
                return *record.status.normalize() == EvolutionStatus::Observing;
            }
        }
        false
    }

    /// 获取活跃进化列表
    pub async fn active_evolutions(&self) -> HashMap<String, String> {
        self.active_evolutions.lock().await.clone()
    }

    /// 触发外部技能（如 OpenClaw 兼容格式）的自进化任务。
    ///
    /// 与 report_error 不同，本方法直接注入一个 ManualRequest 触发器，
    /// 绕过错误计数阈值，立即将技能入队进化。
    /// 返回 evolution_id，可用于日志追踪。
    pub async fn trigger_external_evolution(
        &self,
        context: EvolutionContext,
    ) -> blockcell_core::Result<String> {
        let skill_name = context.skill_name.clone();
        if let Some(existing_id) = self.resolve_in_progress_evolution_id(&skill_name).await {
            return Err(Error::Evolution(format!(
                "技能 `{}` 已有进行中的进化: {}",
                skill_name, existing_id
            )));
        }
        let evolution_id = self.evolution.trigger_evolution(context).await?;

        {
            let mut active = self.active_evolutions.lock().await;
            active.insert(skill_name.clone(), evolution_id.clone());
        }

        info!(
            skill = %skill_name,
            evolution_id = %evolution_id,
            "🧠 [外部技能] 已触发自进化任务"
        );

        Ok(evolution_id)
    }

    /// Persist evolution experience to manual/evolution.md after successful completion.
    /// This is the "experience auto-sedimentation" mechanism — inspired by claude-code's MemoryWriteTool.
    /// Successful fixes are written back to the skill's manual/ directory so future evolutions
    /// can learn from past repairs.
    fn persist_evolution_experience(&self, evolution_id: &str) {
        let record = match self.evolution.load_record(evolution_id) {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    evolution_id = %evolution_id,
                    error = %e,
                    "🧠 [experience] Failed to load record for experience persistence"
                );
                return;
            }
        };

        let skill_dir = self.evolution.skills_dir().join(&record.skill_name);
        let manual_dir = skill_dir.join("manual");
        if let Err(e) = std::fs::create_dir_all(&manual_dir) {
            warn!(
                skill = %record.skill_name,
                error = %e,
                "🧠 [experience] Failed to create manual/ directory"
            );
            return;
        }

        let evolution_md_path = manual_dir.join("evolution.md");

        // Build the experience entry
        let trigger_desc = format!("{:?}", record.context.trigger);
        let explanation = record
            .patch
            .as_ref()
            .map(|p| p.explanation.chars().take(300).collect::<String>())
            .unwrap_or_else(|| "N/A".to_string());

        let feedback_summary = if record.feedback_history.is_empty() {
            "None (first attempt succeeded)".to_string()
        } else {
            record
                .feedback_history
                .iter()
                .map(|f| {
                    format!(
                        "  - Attempt #{} ({}): {}",
                        f.attempt,
                        f.stage,
                        f.feedback.chars().take(100).collect::<String>()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let timestamp = chrono::DateTime::from_timestamp(record.created_at, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_else(|| format!("timestamp={}", record.created_at));

        let entry = format!(
            "\n## {} — Evolution {}\n\n\
             - **Trigger**: {}\n\
             - **Attempts**: {}\n\
             - **Fix summary**: {}\n\
             - **Feedback history**:\n{}\n\
             - **Status**: Completed\n\n\
             ---\n",
            timestamp,
            &record.id[..8.min(record.id.len())],
            trigger_desc,
            record.attempt,
            explanation,
            feedback_summary,
        );

        // Append to evolution.md
        let write_result = if evolution_md_path.exists() {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&evolution_md_path)
                .and_then(|mut f| {
                    use std::io::Write;
                    f.write_all(entry.as_bytes())
                })
        } else {
            let header =
                "# Evolution History\n\nAutomatically recorded successful evolution fixes.\n";
            let content = format!("{}{}", header, entry);
            std::fs::write(&evolution_md_path, content)
        };

        match write_result {
            Ok(()) => {
                info!(
                    skill = %record.skill_name,
                    evolution_id = %evolution_id,
                    path = %evolution_md_path.display(),
                    "🧠 [experience] ✅ Evolution experience persisted to manual/evolution.md"
                );
            }
            Err(e) => {
                warn!(
                    skill = %record.skill_name,
                    error = %e,
                    "🧠 [experience] Failed to write evolution experience"
                );
            }
        }
    }

    /// 清理已完成/失败的进化（成功时清除错误计数器）
    async fn cleanup_evolution(&self, skill_name: &str, evolution_id: &str) {
        self.cleanup_evolution_inner(skill_name, evolution_id, false)
            .await;
    }

    /// 清理回滚的进化（设置冷却期，不清除错误计数器）
    async fn cleanup_evolution_rollback(&self, skill_name: &str, evolution_id: &str) {
        self.cleanup_evolution_inner(skill_name, evolution_id, true)
            .await;
    }

    async fn cleanup_evolution_inner(
        &self,
        skill_name: &str,
        evolution_id: &str,
        is_rollback: bool,
    ) {
        // 将磁盘上处于中间状态的记录标记为 Failed，防止孤尻记录被无限重新接管
        if let Ok(mut record) = self.evolution.load_record(evolution_id) {
            let is_terminal = matches!(
                record.status,
                EvolutionStatus::Completed
                    | EvolutionStatus::RolledBack
                    | EvolutionStatus::Failed
                    | EvolutionStatus::Observing
            );
            if !is_terminal {
                record.status = EvolutionStatus::Failed;
                record.updated_at = chrono::Utc::now().timestamp();
                if let Err(e) = self.evolution.save_record_public(&record) {
                    warn!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        error = %e,
                        "[自进化] 清理时保存 Failed 状态失败，记录可能残留中间状态"
                    );
                }
                info!(
                    skill = %skill_name,
                    evolution_id = %evolution_id,
                    "🧠 [自进化] 清理时将进化记录标记为 Failed，防止孤尻重来 ({})",
                    evolution_id
                );
            }

            if record.context.staged {
                if let Some(staging_dir) = record.context.staging_skills_dir.as_ref() {
                    let root = std::path::PathBuf::from(staging_dir);
                    let staged_skill_dir = root.join(skill_name);
                    if let (Ok(r), Ok(p)) = (root.canonicalize(), staged_skill_dir.canonicalize()) {
                        if p.starts_with(&r) {
                            std::fs::remove_dir_all(p).ok();
                        }
                    } else if staged_skill_dir.starts_with(&root) {
                        std::fs::remove_dir_all(staged_skill_dir).ok();
                    }
                }
            }
        }

        let mut active = self.active_evolutions.lock().await;
        active.remove(skill_name);
        drop(active);

        let mut stats = self.observation_stats.lock().await;
        stats.remove(evolution_id);
        drop(stats);

        let mut tracker = self.error_tracker.lock().await;
        if is_rollback {
            // 回滚时：设置冷却期，避免立即重新触发进化
            tracker.set_cooldown(skill_name);
            info!(
                skill = %skill_name,
                evolution_id = %evolution_id,
                cooldown_minutes = tracker.cooldown_minutes,
                "🧠 [自进化] 技能 `{}` 已回滚，进入 {} 分钟冷却期 ({})",
                skill_name, tracker.cooldown_minutes, evolution_id
            );
        } else {
            // 成功时：清除错误计数器
            tracker.clear(skill_name);
            info!(
                skill = %skill_name,
                evolution_id = %evolution_id,
                "🧠 [自进化] 技能 `{}` 进化记录已清理 ({})",
                skill_name, evolution_id
            );
        }
    }

    /// 列出所有待处理的进化 ID（状态为 Triggered 但尚未开始 pipeline 的）
    ///
    /// Also scans disk records for orphaned Triggered records that aren't in
    /// active_evolutions (e.g. created by the gateway's separate EvolutionService
    /// instance or surviving a restart) and adopts them.
    pub async fn list_pending_ids(&self) -> Vec<(String, String)> {
        // First, adopt orphaned disk records into active_evolutions
        self.adopt_orphaned_records().await;

        let active = self.active_evolutions.lock().await;
        let mut pending = Vec::new();
        for (skill_name, evolution_id) in active.iter() {
            if let Ok(record) = self.evolution.load_record(evolution_id) {
                // Durable worker can resume any pipeline-runnable checkpoint.
                if Self::is_pipeline_runnable_status(&record.status) {
                    pending.push((skill_name.clone(), evolution_id.clone()));
                }
            }
        }
        pending
    }

    /// Scan disk evolution_records for Triggered/Generating records that are NOT
    /// already tracked in active_evolutions. This handles the case where another
    /// EvolutionService instance (e.g. the gateway HTTP handler) created the record
    /// on disk but this instance's in-memory state doesn't know about it.
    async fn adopt_orphaned_records(&self) {
        let records_dir = self.records_dir();
        if !records_dir.exists() {
            debug!(
                "🧠 [adopt] records_dir does not exist: {}",
                records_dir.display()
            );
            return;
        }

        let entries = match std::fs::read_dir(&records_dir) {
            Ok(e) => e,
            Err(e) => {
                warn!("🧠 [adopt] Failed to read records_dir: {}", e);
                return;
            }
        };

        // Collect candidate records first (before acquiring any locks)
        let mut candidates: Vec<(String, String)> = Vec::new(); // (skill_name, evolution_id)
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let record: EvolutionRecord = match serde_json::from_str(&content) {
                Ok(r) => r,
                Err(_) => continue,
            };

            if !Self::is_in_progress_status(&record.status) {
                continue;
            }

            candidates.push((record.skill_name.clone(), record.id.clone()));
        }

        if candidates.is_empty() {
            return;
        }

        // Acquire pipeline locks to avoid conflicts with concurrent pipeline processing.
        // Check each candidate against both active_evolutions and pipeline_locks.
        let pipeline_guard = self.pipeline_locks.lock().await;
        let mut active = self.active_evolutions.lock().await;
        let active_count_before = active.len();

        for (skill_name, evolution_id) in candidates {
            // Skip if already tracked in active_evolutions
            if active.contains_key(&skill_name) {
                continue;
            }

            // Skip if already being processed by the pipeline
            if pipeline_guard.contains(&evolution_id) {
                continue;
            }

            info!(
                skill = %skill_name,
                evolution_id = %evolution_id,
                "🧠 [自进化] 从磁盘发现孤立的进化记录，已接管: {}",
                evolution_id
            );
            active.insert(skill_name, evolution_id);
        }

        drop(pipeline_guard);

        let adopted = active.len() - active_count_before;
        if adopted > 0 {
            info!(
                "🧠 [adopt] Adopted {} orphaned record(s), total active: {}",
                adopted,
                active.len()
            );
        }
    }

    /// 手动触发进化（用户通过 CLI 输入描述）
    ///
    /// 与 report_error 不同，这里不经过 ErrorTracker，直接创建进化记录。
    /// 返回 evolution_id。
    pub async fn trigger_manual_evolution(
        &self,
        skill_name: &str,
        description: &str,
    ) -> Result<String> {
        // 以磁盘记录为准检查进行中的进化，避免多实例内存状态不一致。
        if let Some(existing_id) = self.resolve_in_progress_evolution_id(skill_name).await {
            return Err(Error::Evolution(format!(
                "技能 `{}` 已有进行中的进化: {}",
                skill_name, existing_id
            )));
        }

        let current_version = self
            .evolution
            .version_manager()
            .get_current_version(skill_name)
            .unwrap_or_else(|_| "0.0.0".to_string());

        // 检测技能布局（支持 PromptTool / LocalScript / Hybrid / RhaiOrchestration）
        let (layout, skill_type, source_snippet, source_path) =
            self.detect_skill_layout(skill_name);

        let context = EvolutionContext {
            skill_name: skill_name.to_string(),
            current_version,
            trigger: TriggerReason::ManualRequest {
                description: description.to_string(),
            },
            error_stack: None,
            source_snippet,
            source_path,
            layout,
            tool_schemas: vec![],
            timestamp: chrono::Utc::now().timestamp(),
            skill_type,
            staged: false,
            staging_skills_dir: None,
        };

        let evolution_id = self.evolution.trigger_evolution(context).await?;

        {
            let mut active = self.active_evolutions.lock().await;
            active.insert(skill_name.to_string(), evolution_id.clone());
        }

        info!(
            skill = %skill_name,
            evolution_id = %evolution_id,
            "🧠 [自进化] 用户手动触发技能 `{}` 的进化: {}",
            skill_name, description
        );

        Ok(evolution_id)
    }

    /// 获取内部 SkillEvolution 引用（用于高级操作）
    pub fn evolution(&self) -> &SkillEvolution {
        &self.evolution
    }

    /// Check if the evolution pipeline is actively running for a specific evolution_id.
    /// Used to coordinate between EvolutionService.tick() and SkillEvolutionWorker
    /// to prevent duplicate processing of the same evolution.
    pub async fn is_evolution_pipeline_active(&self, evolution_id: &str) -> bool {
        self.pipeline_locks.lock().await.contains(evolution_id)
    }

    /// 获取进化记录目录路径
    fn records_dir(&self) -> PathBuf {
        self.evolution.records_dir()
    }

    /// 列出所有进化记录（返回 EvolutionRecord 列表）
    pub fn list_all_records(&self) -> Result<Vec<EvolutionRecord>> {
        Ok(Self::load_records_from_dir(&self.records_dir()))
    }

    fn load_records_from_dir(records_dir: &std::path::Path) -> Vec<EvolutionRecord> {
        if !records_dir.exists() {
            return Vec::new();
        }

        let mut records = Vec::new();
        if let Ok(entries) = std::fs::read_dir(records_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(record) = serde_json::from_str::<EvolutionRecord>(&content) {
                            records.push(record);
                        }
                    }
                }
            }
        }

        // Sort by created_at descending
        records.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        records
    }

    /// 清空所有进化记录（磁盘 + 内存）
    pub async fn clear_all_records(&self) -> Result<usize> {
        let records_dir = self.records_dir();
        let mut count = 0;

        if records_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&records_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "json")
                        && std::fs::remove_file(&path).is_ok()
                    {
                        count += 1;
                    }
                }
            }
        }

        // Clear in-memory state
        {
            let mut active = self.active_evolutions.lock().await;
            active.clear();
        }
        {
            let mut tracker = self.error_tracker.lock().await;
            tracker.errors.clear();
        }
        {
            let mut stats = self.observation_stats.lock().await;
            stats.active.clear();
        }

        info!("🧠 [自进化] 已清空所有进化记录 (共 {} 条)", count);
        Ok(count)
    }

    /// 删除指定技能名的所有进化记录
    pub async fn delete_records_by_skill(&self, skill_name: &str) -> Result<usize> {
        let records_dir = self.records_dir();
        let mut count = 0;

        if records_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&records_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "json") {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            if let Ok(record) = serde_json::from_str::<EvolutionRecord>(&content) {
                                if record.skill_name == skill_name
                                    && std::fs::remove_file(&path).is_ok()
                                {
                                    count += 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Clean in-memory state for this skill
        {
            let mut active = self.active_evolutions.lock().await;
            active.remove(skill_name);
        }
        {
            let mut tracker = self.error_tracker.lock().await;
            tracker.clear(skill_name);
        }

        info!(
            skill = %skill_name,
            "🧠 [自进化] 已删除技能 `{}` 的所有进化记录 (共 {} 条)",
            skill_name, count
        );
        Ok(count)
    }

    /// 列出进化记录的简要信息（用于 CLI 展示）
    pub fn list_records_summary(
        &self,
    ) -> Result<(
        Vec<SkillRecordSummary>,
        Vec<SkillRecordSummary>,
        Vec<SkillRecordSummary>,
    )> {
        let records = self.list_all_records()?;

        let mut learning = Vec::new();
        let mut learned = Vec::new();
        let mut failed = Vec::new();

        for r in records {
            let summary = SkillRecordSummary {
                skill_name: r.skill_name.clone(),
                evolution_id: r.id.clone(),
                status: format!("{:?}", r.status),
                status_desc: match r.status {
                    EvolutionStatus::Triggered => "已触发，等待开始学习".to_string(),
                    EvolutionStatus::Generating => "正在生成改进方案".to_string(),
                    EvolutionStatus::Generated => "改进方案已生成".to_string(),
                    EvolutionStatus::Auditing => "正在审计".to_string(),
                    EvolutionStatus::AuditPassed => "审计通过".to_string(),
                    EvolutionStatus::AuditFailed => "审计失败".to_string(),
                    EvolutionStatus::CompilePassed => "编译检查通过".to_string(),
                    EvolutionStatus::CompileFailed => "编译检查失败".to_string(),
                    EvolutionStatus::Observing => "已部署，观察中".to_string(),
                    EvolutionStatus::Completed => "已完成".to_string(),
                    EvolutionStatus::RolledBack => "已回滚".to_string(),
                    EvolutionStatus::Failed => "失败".to_string(),
                    // Legacy statuses
                    EvolutionStatus::DryRunPassed | EvolutionStatus::TestPassed => {
                        "编译检查通过".to_string()
                    }
                    EvolutionStatus::DryRunFailed
                    | EvolutionStatus::TestFailed
                    | EvolutionStatus::Testing => "编译检查失败".to_string(),
                    EvolutionStatus::RollingOut => "已部署，观察中".to_string(),
                },
                created_at: r.created_at,
                error_snippet: r.context.error_stack.as_ref().map(|e| {
                    if e.chars().count() > 80 {
                        format!(
                            "{}...",
                            &e[..e.char_indices().nth(80).map(|(i, _)| i).unwrap_or(e.len())]
                        )
                    } else {
                        e.clone()
                    }
                }),
            };

            match r.status {
                EvolutionStatus::Completed => learned.push(summary),
                EvolutionStatus::Failed
                | EvolutionStatus::RolledBack
                | EvolutionStatus::AuditFailed
                | EvolutionStatus::CompileFailed
                | EvolutionStatus::DryRunFailed
                | EvolutionStatus::TestFailed => failed.push(summary),
                _ => learning.push(summary),
            }
        }

        Ok((learning, learned, failed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evolution::ObservationWindow;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn setup_test_dirs(tag: &str) -> (PathBuf, PathBuf) {
        let mut root = std::env::temp_dir();
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        root.push(format!(
            "blockcell_evo_service_{}_{}_{}",
            tag,
            std::process::id(),
            now_ns
        ));
        let skills_dir = root.join("skills");
        std::fs::create_dir_all(&skills_dir).expect("create test skills dir");
        (root, skills_dir)
    }

    fn write_skill_dir(skill_dir: &Path, files: &[(&str, &str)]) {
        std::fs::create_dir_all(skill_dir).expect("create skill dir");
        for (relative_path, content) in files {
            let full_path = skill_dir.join(relative_path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent).expect("create parent dir");
            }
            std::fs::write(full_path, content).expect("write skill file");
        }
    }

    fn make_service(skills_dir: PathBuf) -> EvolutionService {
        EvolutionService::new(skills_dir, EvolutionServiceConfig::default())
    }

    fn test_context(skill_name: &str) -> EvolutionContext {
        EvolutionContext {
            skill_name: skill_name.to_string(),
            current_version: "0.0.0".to_string(),
            trigger: TriggerReason::ManualRequest {
                description: "test evolution".to_string(),
            },
            error_stack: None,
            source_snippet: None,
            source_path: None,
            layout: SkillLayout::PromptTool,
            tool_schemas: vec![],
            timestamp: chrono::Utc::now().timestamp(),
            skill_type: SkillType::PromptOnly,
            staged: false,
            staging_skills_dir: None,
        }
    }

    #[tokio::test]
    async fn evolution_rejects_parent_directory_skill_name() {
        let (root, skills_dir) = setup_test_dirs("invalid_skill_name");
        let service = make_service(skills_dir);

        let error = service
            .trigger_manual_evolution("..", "rewrite parent")
            .await
            .expect_err("parent directory skill name must be rejected");

        assert!(error.to_string().contains("Invalid skill name"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn test_detect_skill_layout_prompt_only() {
        let (root, skills_dir) = setup_test_dirs("layout_prompt_only");
        let skill_dir = skills_dir.join("weather");
        write_skill_dir(
            &skill_dir,
            &[
                (
                    "SKILL.md",
                    "# Weather\n\n## Shared {#shared}\nQuery weather.\n\n## Prompt {#prompt}\nAsk for a city.",
                ),
                ("meta.yaml", "name: weather\ndescription: weather skill\n"),
            ],
        );

        let service = make_service(skills_dir);
        let (layout, skill_type, snippet, source_path) = service.detect_skill_layout("weather");

        assert_eq!(layout, SkillLayout::PromptTool);
        assert_eq!(skill_type, SkillType::PromptOnly);
        assert!(snippet.is_some());
        assert_eq!(source_path.as_deref(), Some("SKILL.md"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn test_detect_skill_layout_hybrid_with_python() {
        let (root, skills_dir) = setup_test_dirs("layout_hybrid_python");
        let skill_dir = skills_dir.join("analyzer");
        write_skill_dir(
            &skill_dir,
            &[
                (
                    "SKILL.md",
                    "# Analyzer\n\n## Shared {#shared}\nUse a script and prompt together.\n\n## Prompt {#prompt}\nCall the script when needed.",
                ),
                ("SKILL.py", "print('ok')\n"),
                ("meta.yaml", "name: analyzer\ndescription: analyzer skill\n"),
            ],
        );

        let service = make_service(skills_dir);
        let (layout, skill_type, snippet, source_path) = service.detect_skill_layout("analyzer");

        assert_eq!(layout, SkillLayout::Hybrid);
        assert_eq!(skill_type, SkillType::Python);
        assert!(snippet.is_some());
        assert_eq!(source_path.as_deref(), Some("SKILL.py"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn test_detect_skill_layout_hybrid_with_python_and_scripts() {
        let (root, skills_dir) = setup_test_dirs("layout_hybrid_python_scripts");
        let skill_dir = skills_dir.join("analyzer_plus");
        write_skill_dir(
            &skill_dir,
            &[
                (
                    "SKILL.md",
                    "# Analyzer Plus\n\n## Shared {#shared}\nUse script assets together.\n\n## Prompt {#prompt}\nCall the Python entrypoint when needed.",
                ),
                ("SKILL.py", "print('ok plus')\n"),
                ("scripts/helper.sh", "#!/bin/sh\necho helper\n"),
                ("meta.yaml", "name: analyzer_plus\ndescription: analyzer plus skill\n"),
            ],
        );

        let service = make_service(skills_dir);
        let (layout, skill_type, snippet, source_path) =
            service.detect_skill_layout("analyzer_plus");

        assert_eq!(layout, SkillLayout::Hybrid);
        assert_eq!(skill_type, SkillType::Python);
        assert!(snippet.is_some());
        assert_eq!(source_path.as_deref(), Some("SKILL.py"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn test_detect_skill_layout_local_script_directory() {
        let (root, skills_dir) = setup_test_dirs("layout_local_script");
        let skill_dir = skills_dir.join("runner");
        write_skill_dir(
            &skill_dir,
            &[
                ("scripts/run.sh", "#!/bin/sh\necho ok\n"),
                ("meta.yaml", "name: runner\ndescription: runner skill\n"),
            ],
        );

        let service = make_service(skills_dir);
        let (layout, skill_type, snippet, source_path) = service.detect_skill_layout("runner");

        assert_eq!(layout, SkillLayout::LocalScript);
        assert_eq!(skill_type, SkillType::LocalScript);
        assert!(snippet.is_some());
        assert!(source_path.is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn test_detect_skill_layout_rhai_orchestration() {
        let (root, skills_dir) = setup_test_dirs("layout_rhai");
        let skill_dir = skills_dir.join("orchestrator");
        write_skill_dir(
            &skill_dir,
            &[
                ("SKILL.rhai", "let x = 1;\n"),
                ("meta.yaml", "name: orchestrator\ndescription: rhai skill\n"),
            ],
        );

        let service = make_service(skills_dir);
        let (layout, skill_type, snippet, source_path) =
            service.detect_skill_layout("orchestrator");

        assert_eq!(layout, SkillLayout::RhaiOrchestration);
        assert_eq!(skill_type, SkillType::Rhai);
        assert!(snippet.is_some());
        assert_eq!(source_path.as_deref(), Some("SKILL.rhai"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn test_error_tracker_threshold_1_triggers_immediately() {
        let mut tracker = ErrorTracker::new(1, 30, 60);
        let r = tracker.record_error("test_skill");
        assert!(r.is_first);
        assert!(r.trigger.is_some());
        assert_eq!(r.count, 1);
        match r.trigger.unwrap() {
            TriggerReason::ConsecutiveFailures {
                count,
                window_minutes,
            } => {
                assert_eq!(count, 1);
                assert_eq!(window_minutes, 30);
            }
            _ => panic!("Expected ConsecutiveFailures"),
        }
    }

    #[test]
    fn test_error_tracker_threshold_3() {
        let mut tracker = ErrorTracker::new(3, 30, 60);
        let r = tracker.record_error("test_skill");
        assert!(r.is_first);
        assert!(r.trigger.is_none());

        let r = tracker.record_error("test_skill");
        assert!(!r.is_first);
        assert!(r.trigger.is_none());

        let r = tracker.record_error("test_skill");
        assert!(r.trigger.is_some());
        assert_eq!(r.count, 3);
    }

    #[test]
    fn test_error_tracker_clear_allows_retrigger() {
        let mut tracker = ErrorTracker::new(1, 30, 60);
        let r = tracker.record_error("test_skill");
        assert!(r.trigger.is_some());
        tracker.clear("test_skill");
        let r = tracker.record_error("test_skill");
        assert!(r.is_first);
        assert!(r.trigger.is_some());
    }

    #[test]
    fn test_error_tracker_independent_skills() {
        let mut tracker = ErrorTracker::new(1, 30, 60);
        let ra = tracker.record_error("skill_a");
        assert!(ra.is_first);
        assert!(ra.trigger.is_some());
        let rb = tracker.record_error("skill_b");
        assert!(rb.is_first);
        assert!(rb.trigger.is_some());
    }

    #[test]
    fn test_observation_stats() {
        let mut stats = ObservationStats::default();
        stats.active.insert("evo_1".to_string(), (0, 0));

        stats.record_call("evo_1", false);
        stats.record_call("evo_1", false);
        stats.record_call("evo_1", true);

        assert!((stats.error_rate("evo_1") - 1.0 / 3.0).abs() < 0.01);
        assert_eq!(stats.error_rate("evo_unknown"), 0.0);
    }

    #[tokio::test]
    async fn observation_concurrent_reports_preserve_all_counts() {
        let (root, skills_dir) = setup_test_dirs("observation_concurrent");
        let service = Arc::new(make_service(skills_dir));
        let context = test_context("observed_skill");
        let evolution_id = service.evolution.trigger_evolution(context).await.unwrap();
        let mut record = service.evolution.load_record(&evolution_id).unwrap();
        record.status = EvolutionStatus::Observing;
        record.observation = Some(ObservationWindow::default());
        service.evolution.save_record_public(&record).unwrap();

        let mut tasks = Vec::new();
        for index in 0..20 {
            let service = Arc::clone(&service);
            let evolution_id = evolution_id.clone();
            tasks.push(tokio::task::spawn_blocking(move || {
                service
                    .evolution
                    .record_observation_call(&evolution_id, index % 4 == 0)
            }));
        }
        for task in tasks {
            task.await.unwrap().unwrap();
        }

        let record = service.evolution.load_record(&evolution_id).unwrap();
        assert_eq!(record.observation_total_calls, 20);
        assert_eq!(record.observation_error_calls, 5);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn test_trigger_manual_evolution_uses_disk_record_to_dedupe() {
        let (root, skills_dir) = setup_test_dirs("manual_dedupe");
        let service = EvolutionService::new(skills_dir, EvolutionServiceConfig::default());
        let existing_id = service
            .evolution
            .trigger_evolution(test_context("skill_a"))
            .await
            .expect("seed evolution record");

        let err = service
            .trigger_manual_evolution("skill_a", "retry manual trigger")
            .await
            .expect_err("should reject duplicate manual evolution");
        let msg = format!("{}", err);
        assert!(msg.contains("已有进行中的进化"));
        assert!(msg.contains(&existing_id));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn test_trigger_external_evolution_uses_disk_record_to_dedupe() {
        let (root, skills_dir) = setup_test_dirs("external_dedupe");
        let service = EvolutionService::new(skills_dir, EvolutionServiceConfig::default());
        let existing_id = service
            .evolution
            .trigger_evolution(test_context("skill_b"))
            .await
            .expect("seed evolution record");

        let err = service
            .trigger_external_evolution(test_context("skill_b"))
            .await
            .expect_err("should reject duplicate external evolution");
        let msg = format!("{}", err);
        assert!(msg.contains("已有进行中的进化"));
        assert!(msg.contains(&existing_id));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn test_trigger_evolution_ids_unique_within_same_millisecond() {
        // 同一毫秒内连续触发同一技能，序号后缀保证 id 与磁盘记录不互相覆盖。
        let (root, skills_dir) = setup_test_dirs("evolution_id_uniq");
        let service = EvolutionService::new(skills_dir, EvolutionServiceConfig::default());
        let id_a = service
            .evolution
            .trigger_evolution(test_context("skill_dup"))
            .await
            .expect("first trigger");
        let id_b = service
            .evolution
            .trigger_evolution(test_context("skill_dup"))
            .await
            .expect("second trigger");
        assert_ne!(id_a, id_b);
        assert_eq!(service.evolution.load_record(&id_a).unwrap().id, id_a);
        assert_eq!(service.evolution.load_record(&id_b).unwrap().id, id_b);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn test_report_error_reads_disk_state_when_active_memory_is_empty() {
        let (root, skills_dir) = setup_test_dirs("report_error_reconcile");
        let service = EvolutionService::new(skills_dir, EvolutionServiceConfig::default());
        let existing_id = service
            .evolution
            .trigger_evolution(test_context("skill_c"))
            .await
            .expect("seed evolution record");

        let report = service
            .report_error("skill_c", "boom", None, vec![])
            .await
            .expect("report error");
        assert!(report.evolution_in_progress);
        assert!(report.evolution_triggered.is_none());

        let active = service.active_evolutions().await;
        assert_eq!(active.get("skill_c"), Some(&existing_id));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn test_trigger_manual_evolution_infers_python_from_legacy_scripts() {
        let (root, skills_dir) = setup_test_dirs("manual_infer_py");
        let skill_name = "legacy_py_skill";
        let skill_dir = skills_dir.join(skill_name);
        std::fs::create_dir_all(skill_dir.join("scripts")).expect("create scripts dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# Legacy skill\nUses scripts/search.py\n",
        )
        .expect("write SKILL.md");
        std::fs::write(
            skill_dir.join("scripts").join("search.py"),
            "print('legacy openclaw python')\n",
        )
        .expect("write legacy search.py");

        let service = EvolutionService::new(skills_dir, EvolutionServiceConfig::default());
        let evolution_id = service
            .trigger_manual_evolution(skill_name, "convert to blockcell python style")
            .await
            .expect("trigger manual evolution");

        let record = service
            .evolution
            .load_record(&evolution_id)
            .expect("load evolution record");
        assert_eq!(record.context.skill_type, SkillType::Python);
        let snippet = record.context.source_snippet.unwrap_or_default();
        assert!(snippet.contains("scripts/search.py"));
        assert!(snippet.contains("legacy openclaw python"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn test_report_error_infers_python_from_legacy_scripts() {
        let (root, skills_dir) = setup_test_dirs("report_error_infer_py");
        let skill_name = "legacy_py_error";
        let skill_dir = skills_dir.join(skill_name);
        std::fs::create_dir_all(skill_dir.join("scripts")).expect("create scripts dir");
        std::fs::write(skill_dir.join("SKILL.md"), "# Legacy skill\n").expect("write SKILL.md");
        std::fs::write(
            skill_dir.join("scripts").join("search.py"),
            "print('legacy python from error path')\n",
        )
        .expect("write search.py");

        let service = EvolutionService::new(skills_dir, EvolutionServiceConfig::default());
        let report = service
            .report_error(skill_name, "boom", None, vec![])
            .await
            .expect("report error");
        let evo_id = report
            .evolution_triggered
            .expect("should trigger evolution at default threshold=1");

        let record = service
            .evolution
            .load_record(&evo_id)
            .expect("load evolution record");
        assert_eq!(record.context.skill_type, SkillType::Python);

        let _ = std::fs::remove_dir_all(root);
    }
}
