use crate::evolution::{
    EvolutionContext, EvolutionRecord, EvolutionStatus, FeedbackEntry,
    LLMProvider, ShadowTestExecutor, SkillEvolution, TriggerReason,
};
use blockcell_core::{Error, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Built-in tool names that should NOT trigger skill evolution.
/// These are internal system tools — their failures are transient errors,
/// not missing skills that can be "learned".
const BUILTIN_TOOLS: &[&str] = &[
    "__llm_provider__",
    "read_file", "write_file", "edit_file", "list_dir",
    "exec",
    "web_search", "web_fetch",
    "browse",
    "message", "spawn",
    "list_tasks",
    "cron",
    "memory_query", "memory_upsert", "memory_forget",
    "list_skills",
    "system_info", "capability_evolve",
    "camera_capture",
    "chrome_control",
    "app_control",
    "file_ops",
    "data_process",
    "http_request",
    "email",
    "audio_transcribe",
    "chart_generate",
    "office_write",
    "calendar_api",
    "iot_control",
    "tts",
    "ocr",
    "image_understand",
    "social_media",
    "notification",
    "cloud_api",
    "git_api",
    "finance_api",
    "video_process",
    "health_api",
    "map_api",
    "contacts",
    "encrypt",
    "network_monitor",
    "knowledge_graph",
    "stream_subscribe",
    "alert_rule",
    "blockchain_rpc",
    "exchange_api",
    "blockchain_tx",
    "contract_security",
    "bridge_api",
    "nft_market",
    "multisig",
    "community_hub",
    "memory_maintenance",
    "toggle_manage",
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

/// 错误追踪器：记录每个技能的错误次数和时间窗口
#[derive(Debug, Clone)]
struct ErrorTracker {
    /// skill_name -> 错误时间戳列表
    errors: HashMap<String, Vec<i64>>,
    /// 触发进化所需的连续错误次数
    threshold: u32,
    /// 错误统计的时间窗口（分钟）
    window_minutes: u32,
}

/// ErrorTracker 内部返回
struct TrackResult {
    count: u32,
    is_first: bool,
    trigger: Option<TriggerReason>,
}

impl ErrorTracker {
    fn new(threshold: u32, window_minutes: u32) -> Self {
        Self {
            errors: HashMap::new(),
            threshold,
            window_minutes,
        }
    }

    /// 记录一次错误，返回计数信息和是否触发进化
    fn record_error(&mut self, skill_name: &str) -> TrackResult {
        let now = chrono::Utc::now().timestamp();
        let cutoff = now - (self.window_minutes as i64 * 60);

        let timestamps = self.errors.entry(skill_name.to_string()).or_default();
        let was_empty = timestamps.is_empty();
        timestamps.push(now);

        // 清理过期的错误记录
        timestamps.retain(|&t| t > cutoff);

        let count = timestamps.len() as u32;
        let is_first = was_empty || count == 1;

        if count >= self.threshold {
            // 清空计数器，避免重复触发
            timestamps.clear();
            TrackResult {
                count,
                is_first,
                trigger: Some(TriggerReason::ConsecutiveFailures {
                    count,
                    window_minutes: self.window_minutes,
                }),
            }
        } else {
            TrackResult {
                count,
                is_first,
                trigger: None,
            }
        }
    }

    /// 清除某个技能的错误记录（进化成功后调用）
    fn clear(&mut self, skill_name: &str) {
        self.errors.remove(skill_name);
    }
}

/// 灰度发布追踪器：记录灰度期间的执行统计
#[derive(Debug, Clone, Default)]
struct RolloutStats {
    /// evolution_id -> (total_calls, error_calls, stage_started_at)
    active: HashMap<String, (u64, u64, i64)>,
}

impl RolloutStats {
    /// 记录一次技能调用结果
    fn record_call(&mut self, evolution_id: &str, is_error: bool) {
        let entry = self.active.entry(evolution_id.to_string())
            .or_insert((0, 0, chrono::Utc::now().timestamp()));
        entry.0 += 1;
        if is_error {
            entry.1 += 1;
        }
    }

    /// 获取当前错误率
    fn error_rate(&self, evolution_id: &str) -> f64 {
        if let Some(&(total, errors, _)) = self.active.get(evolution_id) {
            if total == 0 { 0.0 } else { errors as f64 / total as f64 }
        } else {
            0.0
        }
    }

    /// 获取当前阶段已经运行的分钟数
    fn stage_elapsed_minutes(&self, evolution_id: &str) -> u32 {
        if let Some(&(_, _, started_at)) = self.active.get(evolution_id) {
            let elapsed = chrono::Utc::now().timestamp() - started_at;
            (elapsed / 60).max(0) as u32
        } else {
            0
        }
    }

    /// 重置某个 evolution 的阶段统计（推进到下一阶段时调用）
    fn reset_stage(&mut self, evolution_id: &str) {
        if let Some(entry) = self.active.get_mut(evolution_id) {
            entry.0 = 0;
            entry.1 = 0;
            entry.2 = chrono::Utc::now().timestamp();
        }
    }

    /// 移除已完成的 evolution
    fn remove(&mut self, evolution_id: &str) {
        self.active.remove(evolution_id);
    }
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
    /// 每个阶段失败后的最大重试次数（审计/编译/测试失败都会重试）
    pub max_retries: u32,
}

impl Default for EvolutionServiceConfig {
    fn default() -> Self {
        Self {
            error_threshold: 1,
            error_window_minutes: 30,
            enabled: true,
            max_retries: 3,
        }
    }
}

/// 进化服务：组合错误追踪、进化编排、灰度调度
///
/// 这是自升级系统的入口。外部通过以下方式交互：
/// - `report_error()`: 技能执行失败时调用，内部自动判断是否触发进化
/// - `run_pending_evolutions()`: 执行待处理的进化流程（生成→审计→dry run→测试→发布）
/// - `tick()`: 定期调用，驱动灰度发布的阶段推进和自动回滚
pub struct EvolutionService {
    evolution: SkillEvolution,
    error_tracker: Arc<Mutex<ErrorTracker>>,
    rollout_stats: Arc<Mutex<RolloutStats>>,
    /// 当前正在进行中的 evolution_id 列表（skill_name -> evolution_id）
    active_evolutions: Arc<Mutex<HashMap<String, String>>>,
    config: EvolutionServiceConfig,
}

impl EvolutionService {
    pub fn new(skills_dir: PathBuf, config: EvolutionServiceConfig) -> Self {
        let error_tracker = ErrorTracker::new(
            config.error_threshold,
            config.error_window_minutes,
        );

        Self {
            evolution: SkillEvolution::new(skills_dir),
            error_tracker: Arc::new(Mutex::new(error_tracker)),
            rollout_stats: Arc::new(Mutex::new(RolloutStats::default())),
            active_evolutions: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
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

        // 如果该技能已有进行中的进化，不重复触发
        let already_evolving = {
            let active = self.active_evolutions.lock().await;
            active.contains_key(skill_name)
        };

        let track_result = {
            let mut tracker = self.error_tracker.lock().await;
            tracker.record_error(skill_name)
        };

        if already_evolving {
            info!(
                skill = %skill_name,
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

        let current_version = self.evolution.version_manager()
            .get_current_version(skill_name)
            .unwrap_or_else(|_| "unknown".to_string());

        let context = EvolutionContext {
            skill_name: skill_name.to_string(),
            current_version,
            trigger,
            error_stack: Some(error_msg.to_string()),
            source_snippet,
            tool_schemas,
            timestamp: chrono::Utc::now().timestamp(),
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
    /// 流程：生成补丁 → 审计 → Dry Run → Shadow Test → 开始灰度发布
    /// 需要 LLM provider 和 test executor 来驱动。
    pub async fn run_pending_evolutions(
        &self,
        llm_provider: &dyn LLMProvider,
        test_executor: &dyn ShadowTestExecutor,
    ) -> Result<Vec<String>> {
        let active = self.active_evolutions.lock().await;
        let pending: Vec<(String, String)> = active.iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        drop(active);

        let mut completed = Vec::new();

        for (skill_name, evolution_id) in pending {
            match self.run_single_evolution(&evolution_id, llm_provider, test_executor).await {
                Ok(true) => {
                    info!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        "Evolution pipeline completed, rollout started"
                    );
                    completed.push(evolution_id);
                }
                Ok(false) => {
                    // 某个阶段失败，清理资源（包括错误计数器，允许重新触发）
                    warn!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        "Evolution pipeline failed at some stage"
                    );
                    self.cleanup_evolution(&skill_name, &evolution_id).await;
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
    /// 新流程：
    /// 1. 生成补丁 → 2. 审计 → 3. 编译检查 → 4. Shadow Test → 5. 灰度发布
    ///
    /// 如果审计/编译/测试失败，会将失败反馈给 LLM 重新生成，最多重试 max_retries 次。
    /// 目标是尽一切努力让进化成功，而不是遇到问题就终止。
    async fn run_single_evolution(
        &self,
        evolution_id: &str,
        llm_provider: &dyn LLMProvider,
        test_executor: &dyn ShadowTestExecutor,
    ) -> Result<bool> {
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
            let patch = self.evolution.generate_patch(evolution_id, llm_provider).await?;
            info!(
                evolution_id = %evolution_id,
                patch_id = %patch.patch_id,
                diff_len = patch.diff.len(),
                "🧠 [pipeline] Step 1 DONE: initial patch generated ({})",
                patch.patch_id
            );
        }

        // ═══════════════════════════════════════════════════════════
        // Step 2+3+4: 审计 → 编译 → 测试（带重试循环）
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
                return Ok(false);
            }

            if attempt > 1 {
                info!(
                    evolution_id = %evolution_id,
                    attempt = attempt,
                    "🧠 [pipeline] ═══ Retry attempt #{}/{} ═══",
                    attempt - 1, max_retries
                );
            }

            // --- 2. 审计 ---
            let record = self.evolution.load_record(evolution_id)?;
            if record.status == EvolutionStatus::Generated {
                info!(evolution_id = %evolution_id, "🧠 [pipeline] ═══ Auditing patch (attempt {}) ═══", attempt);
                let audit = self.evolution.audit_patch(evolution_id, llm_provider).await?;

                if !audit.passed {
                    let issues_text = audit.issues.iter()
                        .map(|i| format!("[{}][{}] {}", i.severity, i.category, i.message))
                        .collect::<Vec<_>>()
                        .join("\n");

                    warn!(
                        evolution_id = %evolution_id,
                        issues = audit.issues.len(),
                        "🧠 [pipeline] Audit FAILED ({} issues), will regenerate with feedback",
                        audit.issues.len()
                    );

                    // 获取当前代码用于反馈
                    let current_code = record.patch.as_ref()
                        .map(|p| p.diff.clone())
                        .unwrap_or_default();

                    let feedback = FeedbackEntry {
                        attempt: record.attempt,
                        stage: "audit".to_string(),
                        feedback: format!("Audit found {} issues:\n{}", audit.issues.len(), issues_text),
                        previous_code: current_code,
                        timestamp: chrono::Utc::now().timestamp(),
                    };

                    // 重新生成
                    self.evolution.regenerate_with_feedback(evolution_id, llm_provider, &feedback).await?;
                    continue; // 回到循环顶部重新审计
                }
                info!(evolution_id = %evolution_id, "🧠 [pipeline] ✅ Audit passed (attempt {})", attempt);
            }

            // --- 3. Dry Run (编译检查) ---
            let record = self.evolution.load_record(evolution_id)?;
            if record.status == EvolutionStatus::AuditPassed {
                info!(evolution_id = %evolution_id, "🧠 [pipeline] ═══ Dry run / compile check (attempt {}) ═══", attempt);
                let (passed, compile_error) = self.evolution.dry_run(evolution_id).await?;

                if !passed {
                    let error_msg = compile_error.unwrap_or_else(|| "Unknown compilation error".to_string());
                    warn!(
                        evolution_id = %evolution_id,
                        "🧠 [pipeline] Compile FAILED: {}, will regenerate with feedback",
                        error_msg
                    );

                    let current_code = record.patch.as_ref()
                        .map(|p| p.diff.clone())
                        .unwrap_or_default();

                    let feedback = FeedbackEntry {
                        attempt: record.attempt,
                        stage: "compile".to_string(),
                        feedback: format!("Rhai compilation failed with error:\n{}", error_msg),
                        previous_code: current_code,
                        timestamp: chrono::Utc::now().timestamp(),
                    };

                    self.evolution.regenerate_with_feedback(evolution_id, llm_provider, &feedback).await?;
                    continue; // 回到循环顶部重新审计+编译
                }
                info!(evolution_id = %evolution_id, "🧠 [pipeline] ✅ Compilation passed (attempt {})", attempt);
            }

            // --- 4. Shadow Test ---
            let record = self.evolution.load_record(evolution_id)?;
            if record.status == EvolutionStatus::DryRunPassed {
                info!(evolution_id = %evolution_id, "🧠 [pipeline] ═══ Shadow test (attempt {}) ═══", attempt);
                let result = self.evolution.shadow_test(evolution_id, test_executor).await?;

                if !result.passed {
                    let errors_text = result.errors.join("\n");
                    warn!(
                        evolution_id = %evolution_id,
                        errors = result.errors.len(),
                        "🧠 [pipeline] Shadow test FAILED ({} errors), will regenerate with feedback",
                        result.errors.len()
                    );

                    let current_code = record.patch.as_ref()
                        .map(|p| p.diff.clone())
                        .unwrap_or_default();

                    let feedback = FeedbackEntry {
                        attempt: record.attempt,
                        stage: "test".to_string(),
                        feedback: format!("Shadow test failed with {} errors:\n{}", result.errors.len(), errors_text),
                        previous_code: current_code,
                        timestamp: chrono::Utc::now().timestamp(),
                    };

                    self.evolution.regenerate_with_feedback(evolution_id, llm_provider, &feedback).await?;
                    continue; // 回到循环顶部重新审计+编译+测试
                }
                info!(evolution_id = %evolution_id, "🧠 [pipeline] ✅ Shadow test passed (attempt {})", attempt);
            }

            // 所有检查都通过了，跳出循环
            break;
        }

        // ═══════════════════════════════════════════════════════════
        // Step 5: 灰度发布
        // ═══════════════════════════════════════════════════════════
        let record = self.evolution.load_record(evolution_id)?;
        if record.status == EvolutionStatus::TestPassed {
            info!(evolution_id = %evolution_id, "🧠 [pipeline] ═══ Step 5: Starting rollout ═══");
            self.evolution.start_rollout(evolution_id).await?;

            // 初始化灰度统计
            let mut stats = self.rollout_stats.lock().await;
            stats.active.insert(
                evolution_id.to_string(),
                (0, 0, chrono::Utc::now().timestamp()),
            );
            info!(evolution_id = %evolution_id, "🧠 [pipeline] Step 5 DONE: rollout started");
        }

        let record = self.evolution.load_record(evolution_id)?;
        info!(
            evolution_id = %evolution_id,
            final_status = ?record.status,
            total_attempts = record.attempt,
            "🧠 [pipeline] ═══ Pipeline completed successfully (after {} attempt(s)) ═══",
            record.attempt
        );
        Ok(true)
    }

    /// 定时调度器 tick
    ///
    /// 应由外部定时调用（建议每 60 秒一次）。
    /// 1. 处理待执行的进化（Triggered 状态 → 记录学习意图）
    /// 2. 检查所有正在灰度发布的进化记录：
    ///    - 如果错误率超过阈值 → 自动回滚
    ///    - 如果当前阶段持续时间已满且错误率正常 → 推进到下一阶段
    ///    - 如果已到最后阶段 → 标记完成，清理资源
    pub async fn tick(&self) -> Result<()> {
        // Phase 1: Process pending evolutions (Triggered → record as learning)
        let pending = self.list_pending_ids().await;
        if !pending.is_empty() {
            info!(
                count = pending.len(),
                "🧠 [自进化] 发现 {} 个待处理的进化任务",
                pending.len()
            );
        }
        for (skill_name, evolution_id) in &pending {
            info!(
                skill = %skill_name,
                evolution_id = %evolution_id,
                "🧠 [自进化] 开始处理技能 `{}` 的进化 ({})",
                skill_name, evolution_id
            );
            if let Err(e) = self.process_pending_evolution(skill_name, evolution_id).await {
                error!(
                    skill = %skill_name,
                    evolution_id = %evolution_id,
                    error = %e,
                    "🧠 [自进化] 处理进化失败"
                );
            }
        }

        // Phase 2: Drive rollout advancement
        let active = self.active_evolutions.lock().await;
        let rolling_out: Vec<(String, String)> = active.iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        drop(active);

        for (skill_name, evolution_id) in rolling_out {
            if let Err(e) = self.tick_single_rollout(&skill_name, &evolution_id).await {
                error!(
                    evolution_id = %evolution_id,
                    error = %e,
                    "🧠 [自进化] 灰度发布 tick 错误"
                );
            }
        }

        Ok(())
    }

    /// Process a pending evolution: record the learning intent.
    ///
    /// Since the full LLM-based pipeline (generate→audit→dry run→shadow test→rollout)
    /// requires an external LLM provider, this simplified path records the evolution
    /// as "learning in progress" so the user can query it via list_skills.
    /// When a full LLM provider is available, this can be upgraded to run the full pipeline.
    async fn process_pending_evolution(
        &self,
        skill_name: &str,
        evolution_id: &str,
    ) -> Result<()> {
        let record = self.evolution.load_record(evolution_id)?;

        if record.status != EvolutionStatus::Triggered {
            return Ok(());
        }

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
                if error_stack.len() > 200 {
                    format!("{}...", &error_stack[..error_stack.char_indices().nth(200).map(|(i,_)|i).unwrap_or(error_stack.len())])
                } else {
                    error_stack.clone()
                }
            );
        }

        // Mark as "Generating" to indicate learning is in progress
        // This record persists on disk so list_skills can find it
        let mut updated_record = record;
        updated_record.status = EvolutionStatus::Generating;
        updated_record.updated_at = chrono::Utc::now().timestamp();
        self.evolution.save_record_public(&updated_record)?;

        info!(
            skill = %skill_name,
            evolution_id = %evolution_id,
            "🧠 [自进化] 技能 `{}` 已标记为学习中 (Generating)",
            skill_name
        );

        Ok(())
    }

    async fn tick_single_rollout(
        &self,
        skill_name: &str,
        evolution_id: &str,
    ) -> Result<()> {
        let record = match self.evolution.load_record(evolution_id) {
            Ok(r) => r,
            Err(_) => return Ok(()), // 记录不存在，跳过
        };

        // 只处理 RollingOut 状态
        if record.status != EvolutionStatus::RollingOut {
            // 如果已完成或已回滚，清理
            if record.status == EvolutionStatus::Completed
                || record.status == EvolutionStatus::RolledBack
                || record.status == EvolutionStatus::Failed
            {
                self.cleanup_evolution(skill_name, evolution_id).await;
            }
            return Ok(());
        }

        let rollout = record.rollout.as_ref()
            .ok_or_else(|| Error::Evolution("No rollout config".to_string()))?;

        let current_stage = &rollout.stages[rollout.current_stage];
        let stats = self.rollout_stats.lock().await;
        let error_rate = stats.error_rate(evolution_id);
        let elapsed_minutes = stats.stage_elapsed_minutes(evolution_id);
        drop(stats);

        // 检查是否需要回滚
        if error_rate > current_stage.error_threshold {
            warn!(
                evolution_id = %evolution_id,
                error_rate = error_rate,
                threshold = current_stage.error_threshold,
                stage = rollout.current_stage,
                "Error rate exceeded threshold, rolling back"
            );
            self.evolution.rollback(evolution_id, &format!(
                "Error rate {:.2}% exceeded threshold {:.2}% at stage {}",
                error_rate * 100.0,
                current_stage.error_threshold * 100.0,
                rollout.current_stage,
            )).await?;
            self.cleanup_evolution(skill_name, evolution_id).await;
            return Ok(());
        }

        // 检查是否可以推进到下一阶段
        if elapsed_minutes >= current_stage.duration_minutes {
            info!(
                evolution_id = %evolution_id,
                stage = rollout.current_stage,
                elapsed_minutes = elapsed_minutes,
                error_rate = error_rate,
                "Stage duration met, advancing rollout"
            );

            let completed = self.evolution.advance_rollout_stage(evolution_id).await?;

            if completed {
                info!(
                    evolution_id = %evolution_id,
                    skill = %skill_name,
                    "Rollout completed successfully"
                );
                self.cleanup_evolution(skill_name, evolution_id).await;
            } else {
                // 重置阶段统计
                let mut stats = self.rollout_stats.lock().await;
                stats.reset_stage(evolution_id);
            }
        }

        Ok(())
    }

    /// 报告能力执行错误（统一错误追踪）
    ///
    /// 与 report_error() 类似，但用于 Capability 执行失败。
    /// 当错误达到阈值时，返回 should_re_evolve=true，
    /// 由调用方决定是否触发 CoreEvolution 重新进化。
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

    /// 报告灰度期间的技能调用结果（供外部在执行技能后调用）
    pub async fn report_skill_call(&self, skill_name: &str, is_error: bool) {
        let active = self.active_evolutions.lock().await;
        if let Some(evolution_id) = active.get(skill_name) {
            let evolution_id = evolution_id.clone();
            drop(active);
            let mut stats = self.rollout_stats.lock().await;
            stats.record_call(&evolution_id, is_error);
        }
    }

    /// 获取某个技能当前的灰度百分比（供路由逻辑使用）
    pub async fn get_rollout_percentage(&self, skill_name: &str) -> Option<u8> {
        let active = self.active_evolutions.lock().await;
        let evolution_id = active.get(skill_name)?.clone();
        drop(active);

        let record = self.evolution.load_record(&evolution_id).ok()?;
        let rollout = record.rollout.as_ref()?;
        if record.status == EvolutionStatus::RollingOut {
            Some(rollout.stages[rollout.current_stage].percentage)
        } else {
            None
        }
    }

    /// 获取活跃进化列表
    pub async fn active_evolutions(&self) -> HashMap<String, String> {
        self.active_evolutions.lock().await.clone()
    }

    /// 清理已完成/失败的进化
    async fn cleanup_evolution(&self, skill_name: &str, evolution_id: &str) {
        let mut active = self.active_evolutions.lock().await;
        active.remove(skill_name);
        drop(active);

        let mut stats = self.rollout_stats.lock().await;
        stats.remove(evolution_id);
        drop(stats);

        let mut tracker = self.error_tracker.lock().await;
        tracker.clear(skill_name);

        info!(
            skill = %skill_name,
            evolution_id = %evolution_id,
            "🧠 [自进化] 技能 `{}` 进化记录已清理 ({})",
            skill_name, evolution_id
        );
    }

    /// 列出所有待处理的进化 ID（状态为 Triggered 但尚未开始 pipeline 的）
    pub async fn list_pending_ids(&self) -> Vec<(String, String)> {
        let active = self.active_evolutions.lock().await;
        let mut pending = Vec::new();
        for (skill_name, evolution_id) in active.iter() {
            if let Ok(record) = self.evolution.load_record(evolution_id) {
                // 只有 Triggered 状态才需要 pipeline 驱动
                if record.status == EvolutionStatus::Triggered {
                    pending.push((skill_name.clone(), evolution_id.clone()));
                }
            }
        }
        pending
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
        // 检查是否已有进行中的进化
        {
            let active = self.active_evolutions.lock().await;
            if let Some(existing_id) = active.get(skill_name) {
                return Err(Error::Evolution(format!(
                    "技能 `{}` 已有进行中的进化: {}",
                    skill_name, existing_id
                )));
            }
        }

        let current_version = self.evolution.version_manager()
            .get_current_version(skill_name)
            .unwrap_or_else(|_| "0.0.0".to_string());

        // Try to load existing SKILL.rhai source for context
        let skill_path = self.evolution.skills_dir().join(skill_name).join("SKILL.rhai");
        let source_snippet = if skill_path.exists() {
            std::fs::read_to_string(&skill_path).ok()
        } else {
            None
        };

        let context = EvolutionContext {
            skill_name: skill_name.to_string(),
            current_version,
            trigger: TriggerReason::ManualRequest {
                description: description.to_string(),
            },
            error_stack: None,
            source_snippet,
            tool_schemas: vec![],
            timestamp: chrono::Utc::now().timestamp(),
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

    /// 获取进化记录目录路径
    fn records_dir(&self) -> PathBuf {
        self.evolution.records_dir()
    }

    /// 列出所有进化记录（返回 EvolutionRecord 列表）
    pub fn list_all_records(&self) -> Result<Vec<EvolutionRecord>> {
        let records_dir = self.records_dir();
        if !records_dir.exists() {
            return Ok(Vec::new());
        }

        let mut records = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&records_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |e| e == "json") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(record) = serde_json::from_str::<EvolutionRecord>(&content) {
                            records.push(record);
                        }
                    }
                }
            }
        }

        // Sort by created_at descending
        records.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(records)
    }

    /// 清空所有进化记录（磁盘 + 内存）
    pub async fn clear_all_records(&self) -> Result<usize> {
        let records_dir = self.records_dir();
        let mut count = 0;

        if records_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&records_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().map_or(false, |e| e == "json") {
                        if std::fs::remove_file(&path).is_ok() {
                            count += 1;
                        }
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
            let mut stats = self.rollout_stats.lock().await;
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
                    if path.extension().map_or(false, |e| e == "json") {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            if let Ok(record) = serde_json::from_str::<EvolutionRecord>(&content) {
                                if record.skill_name == skill_name {
                                    if std::fs::remove_file(&path).is_ok() {
                                        count += 1;
                                    }
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
    pub fn list_records_summary(&self) -> Result<(Vec<SkillRecordSummary>, Vec<SkillRecordSummary>, Vec<SkillRecordSummary>)> {
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
                    EvolutionStatus::DryRunPassed => "编译检查通过".to_string(),
                    EvolutionStatus::Testing => "正在测试".to_string(),
                    EvolutionStatus::TestPassed => "测试通过".to_string(),
                    EvolutionStatus::RollingOut => "灰度发布中".to_string(),
                    EvolutionStatus::Completed => "已完成".to_string(),
                    EvolutionStatus::RolledBack => "已回滚".to_string(),
                    EvolutionStatus::Failed => "失败".to_string(),
                    _ => "未知".to_string(),
                },
                created_at: r.created_at,
                error_snippet: r.context.error_stack.as_ref().map(|e| {
                    if e.chars().count() > 80 {
                        format!("{}...", &e[..e.char_indices().nth(80).map(|(i,_)|i).unwrap_or(e.len())])
                    } else {
                        e.clone()
                    }
                }),
            };

            match r.status {
                EvolutionStatus::Completed => learned.push(summary),
                EvolutionStatus::Failed | EvolutionStatus::RolledBack
                    | EvolutionStatus::AuditFailed | EvolutionStatus::DryRunFailed
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

    #[test]
    fn test_error_tracker_threshold_1_triggers_immediately() {
        let mut tracker = ErrorTracker::new(1, 30);
        let r = tracker.record_error("test_skill");
        assert!(r.is_first);
        assert!(r.trigger.is_some());
        assert_eq!(r.count, 1);
        match r.trigger.unwrap() {
            TriggerReason::ConsecutiveFailures { count, window_minutes } => {
                assert_eq!(count, 1);
                assert_eq!(window_minutes, 30);
            }
            _ => panic!("Expected ConsecutiveFailures"),
        }
    }

    #[test]
    fn test_error_tracker_threshold_3() {
        let mut tracker = ErrorTracker::new(3, 30);
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
        let mut tracker = ErrorTracker::new(1, 30);
        let r = tracker.record_error("test_skill");
        assert!(r.trigger.is_some());
        // After trigger, counter is cleared internally.
        // But clear() also resets, so next error is first again.
        tracker.clear("test_skill");
        let r = tracker.record_error("test_skill");
        assert!(r.is_first);
        assert!(r.trigger.is_some()); // triggers again at threshold=1
    }

    #[test]
    fn test_error_tracker_independent_skills() {
        let mut tracker = ErrorTracker::new(1, 30);
        let ra = tracker.record_error("skill_a");
        assert!(ra.is_first);
        assert!(ra.trigger.is_some());
        let rb = tracker.record_error("skill_b");
        assert!(rb.is_first);
        assert!(rb.trigger.is_some());
    }

    #[test]
    fn test_rollout_stats() {
        let mut stats = RolloutStats::default();
        stats.active.insert("evo_1".to_string(), (0, 0, chrono::Utc::now().timestamp()));

        stats.record_call("evo_1", false);
        stats.record_call("evo_1", false);
        stats.record_call("evo_1", true);

        assert!((stats.error_rate("evo_1") - 1.0 / 3.0).abs() < 0.01);
        assert_eq!(stats.error_rate("evo_unknown"), 0.0);
    }
}
