use crate::evolution::{
    EvolutionContext, EvolutionRecord, EvolutionStatus, FeedbackEntry,
    LLMProvider, SkillEvolution, TriggerReason,
};
use blockcell_core::{Error, Result};
use std::collections::{HashMap, HashSet};
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
    "termux_api",
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
    /// skill_name -> (错误时间戳列表, 已触发进化的时间戳)
    errors: HashMap<String, (Vec<i64>, Option<i64>)>,
    /// 触发进化所需的连续错误次数
    threshold: u32,
    /// 错误统计的时间窗口（分钟）
    window_minutes: u32,
    /// 回滚冷却期：skill_name -> 冷却结束时间戳
    /// 在冷却期内不会触发新的进化，避免“进化→回滚→再进化”死循环
    cooldowns: HashMap<String, i64>,
    /// 冷却期时长（分钟），默认 60 分钟
    cooldown_minutes: u32,
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
            cooldowns: HashMap::new(),
            cooldown_minutes: 60, // 默认 1 小时冷却期
        }
    }

    /// 记录一次错误，返回计数信息和是否触发进化
    fn record_error(&mut self, skill_name: &str) -> TrackResult {
        let now = chrono::Utc::now().timestamp();
        let cutoff = now - (self.window_minutes as i64 * 60);

        let entry = self.errors.entry(skill_name.to_string()).or_insert((Vec::new(), None));
        let (timestamps, triggered_at) = entry;
        
        let was_empty = timestamps.is_empty();
        timestamps.push(now);

        // 清理过期的错误记录
        timestamps.retain(|&t| t > cutoff);
        
        // 如果已触发的进化也过期了，清除标记
        if let Some(trigger_time) = *triggered_at {
            if trigger_time <= cutoff {
                *triggered_at = None;
            }
        }

        let count = timestamps.len() as u32;
        let is_first = was_empty || count == 1;

        // 检查冷却期：回滚后的冷却期内不触发新进化
        let in_cooldown = if let Some(&cooldown_until) = self.cooldowns.get(skill_name) {
            if now < cooldown_until {
                true
            } else {
                // 冷却期已过，清除
                self.cooldowns.remove(skill_name);
                false
            }
        } else {
            false
        };

        // 检查是否应该触发进化：达到阈值 且 未在窗口期内触发过 且 不在冷却期
        let should_trigger = count >= self.threshold && triggered_at.is_none() && !in_cooldown;
        
        if should_trigger {
            // 标记已触发，但不清空计数器（保留历史用于统计）
            *triggered_at = Some(now);
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
    
    /// 重置触发标记（允许再次触发进化）
    #[allow(dead_code)]
    fn reset_trigger(&mut self, skill_name: &str) {
        if let Some(entry) = self.errors.get_mut(skill_name) {
            entry.1 = None;
        }
    }

    /// 设置冷却期（回滚后调用，避免立即重新触发进化）
    fn set_cooldown(&mut self, skill_name: &str) {
        let cooldown_until = chrono::Utc::now().timestamp()
            + (self.cooldown_minutes as i64 * 60);
        self.cooldowns.insert(skill_name.to_string(), cooldown_until);
    }

    /// 检查某个技能是否在冷却期内
    #[allow(dead_code)]
    fn is_in_cooldown(&self, skill_name: &str) -> bool {
        if let Some(&cooldown_until) = self.cooldowns.get(skill_name) {
            chrono::Utc::now().timestamp() < cooldown_until
        } else {
            false
        }
    }
}

/// 观察期统计追踪器：记录部署后观察窗口内的执行统计
#[derive(Debug, Clone, Default)]
struct ObservationStats {
    /// evolution_id -> (total_calls, error_calls)
    active: HashMap<String, (u64, u64)>,
}

impl ObservationStats {
    /// 记录一次技能调用结果
    fn record_call(&mut self, evolution_id: &str, is_error: bool) {
        let entry = self.active.entry(evolution_id.to_string())
            .or_insert((0, 0));
        entry.0 += 1;
        if is_error {
            entry.1 += 1;
        }
    }

    /// 获取当前错误率
    fn error_rate(&self, evolution_id: &str) -> f64 {
        if let Some(&(total, errors)) = self.active.get(evolution_id) {
            if total == 0 { 0.0 } else { errors as f64 / total as f64 }
        } else {
            0.0
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
    /// LLM 调用超时时间（秒）
    pub llm_timeout_secs: u64,
}

impl Default for EvolutionServiceConfig {
    fn default() -> Self {
        Self {
            error_threshold: 1,
            error_window_minutes: 30,
            enabled: true,
            max_retries: 3,
            llm_timeout_secs: 300, // 5分钟
        }
    }
}

/// 进化服务：组合错误追踪、进化编排、灰度调度
///
/// 这是自升级系统的入口。外部通过以下方式交互：
/// - `report_error()`: 技能执行失败时调用，内部自动判断是否触发进化
/// - `run_pending_evolutions()`: 执行待处理的进化流程（生成→审计→dry run→测试→发布）
/// - `tick()`: 定期调用，驱动灰度发布的阶段推进和自动回滚
/// - `set_llm_provider()`: 设置 LLM provider，使 tick() 能自动驱动完整 pipeline
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
}

impl EvolutionService {
    pub fn new(skills_dir: PathBuf, config: EvolutionServiceConfig) -> Self {
        let error_tracker = ErrorTracker::new(
            config.error_threshold,
            config.error_window_minutes,
        );

        Self {
            evolution: SkillEvolution::new(skills_dir, config.llm_timeout_secs),
            error_tracker: Arc::new(Mutex::new(error_tracker)),
            observation_stats: Arc::new(Mutex::new(ObservationStats::default())),
            active_evolutions: Arc::new(Mutex::new(HashMap::new())),
            pipeline_locks: Arc::new(Mutex::new(HashSet::new())),
            config,
            llm_provider: None,
        }
    }

    /// 设置 LLM provider，使 tick() 能自动驱动完整进化 pipeline。
    /// 应在 agent 启动时调用，传入与主 agent 相同的 provider。
    pub fn set_llm_provider(&mut self, provider: Arc<dyn LLMProvider>) {
        self.llm_provider = Some(provider);
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
    /// 流程：生成补丁 → 审计 → 编译检查 → 部署+观察
    /// 需要 LLM provider 来驱动。
    pub async fn run_pending_evolutions(
        &self,
        llm_provider: &dyn LLMProvider,
    ) -> Result<Vec<String>> {
        let active = self.active_evolutions.lock().await;
        let pending: Vec<(String, String)> = active.iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        drop(active);

        let mut completed = Vec::new();

        for (skill_name, evolution_id) in pending {
            match self.run_single_evolution(&evolution_id, llm_provider).await {
                Ok(true) => {
                    info!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        "Evolution pipeline completed, observation started"
                    );
                    completed.push(evolution_id);
                }
                Ok(false) => {
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
    /// 流程：1. 生成补丁 → 2. 审计 → 3. 编译检查 → 4. 部署+观察
    ///
    /// 如果审计/编译失败，会将失败反馈给 LLM 重新生成，最多重试 max_retries 次。
    async fn run_single_evolution(
        &self,
        evolution_id: &str,
        llm_provider: &dyn LLMProvider,
    ) -> Result<bool> {
        // P2-6: 获取 pipeline 锁，防止同一 evolution 并发执行
        {
            let mut locks = self.pipeline_locks.lock().await;
            if locks.contains(evolution_id) {
                info!(evolution_id = %evolution_id, "🧠 [pipeline] Already running, skipping");
                return Ok(true); // 已在执行中，不重复
            }
            locks.insert(evolution_id.to_string());
        }

        let result = self.run_single_evolution_inner(evolution_id, llm_provider).await;

        // 释放 pipeline 锁
        {
            let mut locks = self.pipeline_locks.lock().await;
            locks.remove(evolution_id);
        }

        result
    }

    /// pipeline 内部实现（被 run_single_evolution 包装以管理锁）
    async fn run_single_evolution_inner(
        &self,
        evolution_id: &str,
        llm_provider: &dyn LLMProvider,
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

                    self.evolution.regenerate_with_feedback(evolution_id, llm_provider, &feedback).await?;
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
                    continue;
                }
                info!(evolution_id = %evolution_id, "🧠 [pipeline] ✅ Compile check passed (attempt {})", attempt);
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
        Ok(true)
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

        // Phase 2: Check observation windows
        let active = self.active_evolutions.lock().await;
        let observing: Vec<(String, String)> = active.iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        drop(active);

        for (skill_name, evolution_id) in observing {
            if let Err(e) = self.tick_single_observation(&skill_name, &evolution_id).await {
                error!(
                    evolution_id = %evolution_id,
                    error = %e,
                    "🧠 [自进化] 观察窗口 tick 错误"
                );
            }
        }

        Ok(())
    }

    /// Process a pending evolution.
    ///
    /// If an LLM provider is configured, runs the full pipeline (generate→audit→compile→deploy+observe).
    /// Otherwise, just marks the record as "Generating" so list_skills can show it.
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
                if error_stack.chars().count() > 200 {
                    format!("{}...", error_stack.chars().take(200).collect::<String>())
                } else {
                    error_stack.clone()
                }
            );
        }

        // If we have an LLM provider, run the full pipeline
        if let Some(ref llm_provider) = self.llm_provider {
            info!(
                skill = %skill_name,
                evolution_id = %evolution_id,
                "🧠 [自进化] LLM provider 可用，开始执行完整进化 pipeline"
            );
            match self.run_single_evolution(evolution_id, llm_provider.as_ref()).await {
                Ok(true) => {
                    info!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        "🧠 [自进化] 技能 `{}` 进化 pipeline 完成，观察窗口已启动",
                        skill_name
                    );
                    // Observation stats already initialized in run_single_evolution
                }
                Ok(false) => {
                    warn!(
                        skill = %skill_name,
                        evolution_id = %evolution_id,
                        "🧠 [自进化] 技能 `{}` 进化 pipeline 失败（所有重试已耗尽）",
                        skill_name
                    );
                    self.cleanup_evolution(skill_name, evolution_id).await;
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
        } else {
            // No LLM provider — just mark as Generating so list_skills can show it
            info!(
                skill = %skill_name,
                evolution_id = %evolution_id,
                "🧠 [自进化] 无 LLM provider，技能 `{}` 标记为学习中 (Generating)，等待手动执行",
                skill_name
            );
            let mut updated_record = record;
            updated_record.status = EvolutionStatus::Generating;
            updated_record.updated_at = chrono::Utc::now().timestamp();
            self.evolution.save_record_public(&updated_record)?;
        }

        Ok(())
    }

    /// P1: 观察窗口 tick — 检查错误率和观察时间
    async fn tick_single_observation(
        &self,
        skill_name: &str,
        evolution_id: &str,
    ) -> Result<()> {
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
        let error_rate = stats.error_rate(evolution_id);
        drop(stats);

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
                self.cleanup_evolution(skill_name, evolution_id).await;
            }
            Some(false) => {
                // 错误率超阈值，回滚
                warn!(
                    evolution_id = %evolution_id,
                    error_rate = error_rate,
                    "🧠 [观察] 错误率超阈值，回滚"
                );
                self.evolution.rollback(evolution_id, &format!(
                    "Error rate {:.2}% exceeded threshold during observation",
                    error_rate * 100.0,
                )).await?;
                self.cleanup_evolution_rollback(skill_name, evolution_id).await;
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
        let active = self.active_evolutions.lock().await;
        if let Some(evolution_id) = active.get(skill_name) {
            let evolution_id = evolution_id.clone();
            drop(active);
            let mut stats = self.observation_stats.lock().await;
            stats.record_call(&evolution_id, is_error);
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

    /// 清理已完成/失败的进化（成功时清除错误计数器）
    async fn cleanup_evolution(&self, skill_name: &str, evolution_id: &str) {
        self.cleanup_evolution_inner(skill_name, evolution_id, false).await;
    }

    /// 清理回滚的进化（设置冷却期，不清除错误计数器）
    async fn cleanup_evolution_rollback(&self, skill_name: &str, evolution_id: &str) {
        self.cleanup_evolution_inner(skill_name, evolution_id, true).await;
    }

    async fn cleanup_evolution_inner(&self, skill_name: &str, evolution_id: &str, is_rollback: bool) {
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
                    if path.extension().is_some_and(|e| e == "json")
                        && std::fs::remove_file(&path).is_ok() {
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
                                    && std::fs::remove_file(&path).is_ok() {
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
                    EvolutionStatus::AuditFailed => "审计失败".to_string(),
                    EvolutionStatus::CompilePassed => "编译检查通过".to_string(),
                    EvolutionStatus::CompileFailed => "编译检查失败".to_string(),
                    EvolutionStatus::Observing => "已部署，观察中".to_string(),
                    EvolutionStatus::Completed => "已完成".to_string(),
                    EvolutionStatus::RolledBack => "已回滚".to_string(),
                    EvolutionStatus::Failed => "失败".to_string(),
                    // Legacy statuses
                    EvolutionStatus::DryRunPassed | EvolutionStatus::TestPassed => "编译检查通过".to_string(),
                    EvolutionStatus::DryRunFailed | EvolutionStatus::TestFailed | EvolutionStatus::Testing => "编译检查失败".to_string(),
                    EvolutionStatus::RollingOut => "已部署，观察中".to_string(),
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
                    | EvolutionStatus::AuditFailed | EvolutionStatus::CompileFailed
                    | EvolutionStatus::DryRunFailed | EvolutionStatus::TestFailed => failed.push(summary),
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
        tracker.clear("test_skill");
        let r = tracker.record_error("test_skill");
        assert!(r.is_first);
        assert!(r.trigger.is_some());
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
    fn test_observation_stats() {
        let mut stats = ObservationStats::default();
        stats.active.insert("evo_1".to_string(), (0, 0));

        stats.record_call("evo_1", false);
        stats.record_call("evo_1", false);
        stats.record_call("evo_1", true);

        assert!((stats.error_rate("evo_1") - 1.0 / 3.0).abs() < 0.01);
        assert_eq!(stats.error_rate("evo_unknown"), 0.0);
    }
}
