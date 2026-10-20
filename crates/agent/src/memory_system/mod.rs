//! 记忆系统集成模块
//!
//! 封装所有 7 层记忆系统的状态和操作，提供统一接口。

use crate::auto_memory::{ExtractionCursor, ExtractionCursorManager, MemoryType};
use crate::compact::{should_compact, CompactHookRegistry, FileTracker, SkillTracker};
use crate::response_cache::ContentReplacementState;
use crate::session_memory::{
    get_session_memory_path, should_extract_memory, SessionMemoryConfig, SessionMemoryState,
};
use blockcell_core::types::ChatMessage;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::task::JoinHandle;

/// 后台任务句柄类型
pub type BackgroundTaskHandle = JoinHandle<()>;

/// 后台记忆提取占用标记的生命周期守卫。
///
/// Tokio 任务发生 panic 时会执行栈展开，因此用 Drop 清理可以避免同一进程内
/// 遗留 marker/journal，导致后续提取被永久误判为仍在运行。
pub(crate) struct ExtractionMarkerGuard {
    marker_path: PathBuf,
    journal_path: PathBuf,
    cleanup_on_drop: bool,
}

impl ExtractionMarkerGuard {
    pub(crate) fn new(marker_path: PathBuf, journal_path: PathBuf) -> Self {
        Self {
            marker_path,
            journal_path,
            cleanup_on_drop: true,
        }
    }

    /// 保留占用标记，供需要在下一轮重试的持久化失败路径使用。
    pub(crate) fn preserve(&mut self) {
        self.cleanup_on_drop = false;
    }
}

impl Drop for ExtractionMarkerGuard {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = std::fs::remove_file(&self.marker_path);
            let _ = std::fs::remove_file(&self.journal_path);
        }
    }
}

// Re-export MemorySystemConfig from core crate
pub use blockcell_core::config::MemorySystemConfig;

/// Session Memory 提取结果（后台任务完成后传递给主线程）
#[derive(Debug, Default, Clone)]
pub struct SessionMemoryExtractionResult {
    /// 最后一条消息 ID
    pub last_message_id: Option<String>,
    /// 最后一条消息索引
    pub last_message_index: usize,
    /// 当前 Token 数
    pub token_count: usize,
    /// 提取是否成功
    pub success: bool,
}

/// Session Memory 状态的持久化版本
///
/// 用于在 runtime 重建时恢复提取进度，避免 Gateway/异步消息模式下
/// `tokens_at_last_extraction` 和 `initialized` 丢失导致重复触发提取。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct PersistedSessionMemoryState {
    /// 上次提取时的 Token 数
    tokens_at_last_extraction: usize,
    /// 是否已初始化
    initialized: bool,
    /// 上次提取时的消息 ID
    last_memory_message_id: Option<String>,
    /// 上次提取时的消息索引（向后兼容，用于消息无 ID 时的 fallback）
    last_memory_message_index: Option<usize>,
}

/// 记忆系统状态
#[derive(Debug, Default)]
pub struct MemorySystemState {
    /// Session Memory 状态
    pub session_memory: SessionMemoryState,
    /// 内容替换状态 (Layer 1)
    pub content_replacement: ContentReplacementState,
    /// 自动记忆提取游标
    pub auto_memory_cursors: Vec<ExtractionCursor>,
    /// 是否有待处理的提取任务
    pub has_pending_extraction: bool,
    /// 文件追踪器 (Layer 4 Compact 恢复)
    pub file_tracker: FileTracker,
    /// 技能追踪器 (Layer 4 Compact 恢复)
    pub skill_tracker: SkillTracker,
    /// 后台任务句柄列表 (用于追踪和取消)
    pub background_tasks: Vec<BackgroundTaskHandle>,
    /// 是否需要重新加载游标状态（后台提取完成后设置）
    pub needs_cursor_reload: bool,
}

/// 记忆系统集成器
///
/// 封装所有记忆系统操作，提供统一接口
pub struct MemorySystem {
    /// 配置
    config: MemorySystemConfig,
    /// 状态
    state: MemorySystemState,
    /// Compact Hooks 注册表
    compact_hooks: CompactHookRegistry,
    /// 工作目录
    workspace_dir: PathBuf,
    /// 配置目录
    config_dir: PathBuf,
    /// 会话 ID
    session_id: String,
    /// 自动记忆提取游标管理器（缓存已加载的状态）
    cursor_manager: ExtractionCursorManager,
    /// 游标重新加载标志（用于后台任务通知主线程）
    cursor_reload_flag: Arc<std::sync::atomic::AtomicBool>,
    /// Session Memory 提取结果通道（后台任务完成后通知主线程更新状态）
    session_memory_result_tx: tokio::sync::watch::Sender<SessionMemoryExtractionResult>,
    /// Session Memory 提取结果接收端
    session_memory_result_rx: tokio::sync::watch::Receiver<SessionMemoryExtractionResult>,
}

impl MemorySystem {
    /// 创建记忆系统
    pub fn new(
        config: MemorySystemConfig,
        workspace_dir: PathBuf,
        config_dir: PathBuf,
        session_id: String,
    ) -> Self {
        let cursor_manager = ExtractionCursorManager::new(&config_dir);

        let tracker_summary_chars = config.layer4.tracker_summary_chars;
        let session_memory_config: SessionMemoryConfig = config.layer3.clone().into();
        let max_replacement_entries = config.layer1.max_replacement_entries;
        let (session_memory_result_tx, session_memory_result_rx) =
            tokio::sync::watch::channel(SessionMemoryExtractionResult::default());

        Self {
            config,
            state: MemorySystemState {
                content_replacement: ContentReplacementState::with_max_entries(
                    max_replacement_entries,
                ),
                file_tracker: FileTracker::with_config(tracker_summary_chars),
                skill_tracker: SkillTracker::with_config(tracker_summary_chars),
                session_memory: SessionMemoryState {
                    config: session_memory_config,
                    ..Default::default()
                },
                ..Default::default()
            },
            compact_hooks: CompactHookRegistry::new(),
            workspace_dir,
            config_dir,
            session_id,
            cursor_manager,
            cursor_reload_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            session_memory_result_tx,
            session_memory_result_rx,
        }
    }

    /// 异步初始化（加载游标状态 + 标记会话活跃 + 恢复持久化的 Session Memory 状态）
    pub async fn initialize(&mut self) -> std::io::Result<()> {
        self.cursor_manager.load().await?;
        // 恢复持久化的 Session Memory 状态（避免 runtime 重建后丢失提取进度）
        self.load_session_memory_state().await?;
        // 标记会话为活跃状态
        self.mark_session_active().await
    }

    /// 从会话目录加载持久化的 Session Memory 状态
    ///
    /// 在 Gateway/异步消息模式下，runtime 会被重建，此方法确保
    /// `tokens_at_last_extraction`、`initialized` 和 `last_memory_message_id`
    /// 在 runtime 重建后仍能正确恢复，避免重复触发提取。
    async fn load_session_memory_state(&mut self) -> std::io::Result<()> {
        let state_path = self.session_memory_state_path();
        if !state_path.exists() {
            return Ok(());
        }

        match std::fs::read_to_string(&state_path) {
            Ok(content) => match serde_json::from_str::<PersistedSessionMemoryState>(&content) {
                Ok(persisted) => {
                    self.state.session_memory.tokens_at_last_extraction =
                        persisted.tokens_at_last_extraction;
                    self.state.session_memory.initialized = persisted.initialized;
                    self.state.session_memory.last_memory_message_id =
                        persisted.last_memory_message_id;
                    self.state.session_memory.last_memory_message_index =
                        persisted.last_memory_message_index;
                    tracing::info!(
                        tokens_at_last_extraction = persisted.tokens_at_last_extraction,
                        initialized = persisted.initialized,
                        "[memory_system] 已恢复持久化的 Session Memory 状态"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "[memory_system] 解析 Session Memory 状态文件失败，使用默认状态"
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "[memory_system] 读取 Session Memory 状态文件失败，使用默认状态"
                );
            }
        }
        Ok(())
    }

    /// 持久化当前 Session Memory 状态到会话目录（同步，适用于同步上下文调用）
    ///
    /// 在提取完成或状态变更后调用，确保 runtime 重建后能恢复提取进度。
    pub fn save_session_memory_state_sync(&self) {
        let state_path = self.session_memory_state_path();
        if let Some(parent) = state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let persisted = PersistedSessionMemoryState {
            tokens_at_last_extraction: self.state.session_memory.tokens_at_last_extraction,
            initialized: self.state.session_memory.initialized,
            last_memory_message_id: self.state.session_memory.last_memory_message_id.clone(),
            last_memory_message_index: self.state.session_memory.last_memory_message_index,
        };

        match serde_json::to_string_pretty(&persisted) {
            Ok(content) => {
                // 使用原子写入，防止崩溃或并发读取时得到半截 JSON
                if let Err(e) = crate::fs_util::atomic_write(&state_path, content.as_bytes()) {
                    tracing::warn!(
                        error = %e,
                        "[memory_system] 保存 Session Memory 状态失败"
                    );
                } else {
                    tracing::trace!("[memory_system] Session Memory 状态已持久化");
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "[memory_system] 序列化 Session Memory 状态失败"
                );
            }
        }
    }

    /// 获取 Session Memory 状态文件路径
    fn session_memory_state_path(&self) -> PathBuf {
        let session_dir = self.session_dir();
        session_dir.join(".session_memory_state.json")
    }

    /// 创建 Session Memory 提取 pending 文件标记（同步，跨 runtime 可见）
    /// 使用原子创建（create_new），返回是否成功创建。
    /// 两个 runtime 同时调用时，只有一个能成功创建，避免并发重复提取。
    fn touch_extraction_marker_sync(&self) -> bool {
        let marker_path = self.extraction_pending_marker_path();
        if let Some(parent) = marker_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        // 使用 create_new(true) 实现原子创建：
        // 文件已存在时返回 Err，只有一个调用者能成功
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker_path)
        {
            Ok(mut file) => {
                use std::io::Write;
                let _ = file.write_all(timestamp_ms.to_string().as_bytes());
                true
            }
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "[memory_system] 创建提取标记失败（可能已有其他 runtime 在提取）"
                );
                false
            }
        }
    }

    /// 清除 Session Memory 提取 pending 文件标记
    fn clear_extraction_marker_sync(&self) {
        let marker_path = self.extraction_pending_marker_path();
        if marker_path.exists() {
            let _ = std::fs::remove_file(&marker_path);
        }
    }

    // ── 提取任务 Journal（stale marker 清理辅助） ──────────
    //
    // Journal 记录提取任务的元数据（started_at、message_count 等），用于：
    // 1. 启动时检测孤儿 journal：通过 started_at 判断任务是否已过期
    // 2. 过期 journal 清除对应的 stale pending marker，允许下次交互重新触发提取
    // 注意：Journal 不存储可重放的提取参数，不是可靠恢复队列。
    // 进程退出时正在运行的任务会丢失，只能通过 stale 机制在下次启动时清理。

    /// 获取 Session Memory 提取 journal 文件路径
    fn session_memory_journal_path(&self) -> PathBuf {
        self.session_dir().join(".extraction_journal")
    }

    /// 获取 Auto Memory 提取 journal 文件路径
    pub fn auto_memory_journal_path(&self, memory_type: &MemoryType) -> PathBuf {
        self.config_dir
            .join(format!(".extraction_journal.{}", memory_type.name()))
    }

    /// 写入 Session Memory 提取 journal
    /// 在 spawn 前调用，记录任务元数据（用于 stale marker 检测，不是可靠恢复队列）
    /// 包含 owner_pid 用于检测任务所属进程是否存活，避免误删长耗时任务
    pub fn write_session_memory_journal(&self, message_count: usize) {
        let journal_path = self.session_memory_journal_path();
        if let Some(parent) = journal_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let journal = serde_json::json!({
            "task_type": "session_memory",
            "session_id": self.session_id,
            "message_count": message_count,
            "started_at": chrono::Utc::now().to_rfc3339(),
            "owner_pid": std::process::id(),
        });
        if let Ok(content) = serde_json::to_string_pretty(&journal) {
            if let Err(e) = crate::fs_util::atomic_write(&journal_path, content.as_bytes()) {
                tracing::debug!(error = %e, "[memory_system] 写入 Session Memory journal 失败");
            }
        }
    }

    /// 清除 Session Memory 提取 journal
    pub fn clear_session_memory_journal(&self) {
        let journal_path = self.session_memory_journal_path();
        if journal_path.exists() {
            let _ = std::fs::remove_file(&journal_path);
        }
    }

    /// 写入 Auto Memory 提取 journal（用于 stale marker 检测，不是可靠恢复队列）
    /// 包含 owner_pid 用于检测任务所属进程是否存活，避免误删长耗时任务
    pub fn write_auto_memory_journal(&self, memory_type: &MemoryType, message_count: usize) {
        let journal_path = self.auto_memory_journal_path(memory_type);
        if let Some(parent) = journal_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let journal = serde_json::json!({
            "task_type": "auto_memory",
            "memory_type": memory_type.name(),
            "session_id": self.session_id,
            "message_count": message_count,
            "started_at": chrono::Utc::now().to_rfc3339(),
            "owner_pid": std::process::id(),
        });
        if let Ok(content) = serde_json::to_string_pretty(&journal) {
            if let Err(e) = crate::fs_util::atomic_write(&journal_path, content.as_bytes()) {
                tracing::debug!(
                    error = %e,
                    memory_type = memory_type.name(),
                    "[memory_system] 写入 Auto Memory journal 失败"
                );
            }
        }
    }

    /// 清除 Auto Memory 提取 journal
    pub fn clear_auto_memory_journal(&self, memory_type: &MemoryType) {
        let journal_path = self.auto_memory_journal_path(memory_type);
        if journal_path.exists() {
            let _ = std::fs::remove_file(&journal_path);
        }
    }

    /// 扫描并清理孤儿 journal（启动时调用）
    ///
    /// 检测残留的 journal 文件，仅当任务已过期（started_at 超过 stale 阈值）时
    /// 才清除 pending marker 和 journal，允许下次交互重新触发提取。
    /// 仍在运行的任务（journal 未过期）不会被干扰，避免误删活跃任务的 marker。
    pub fn cleanup_orphaned_journals(&self) {
        // Session Memory journal：使用 Layer 3 的 stale 阈值
        let sm_journal = self.session_memory_journal_path();
        if sm_journal.exists() {
            let stale_threshold_ms = self.config.layer3.extraction_stale_threshold_ms;
            if self.is_journal_stale(&sm_journal, stale_threshold_ms) {
                tracing::warn!(
                    path = %sm_journal.display(),
                    stale_threshold_ms,
                    "[memory_system] 发现过期 Session Memory journal，清除 pending marker 允许重新提取"
                );
                self.clear_extraction_marker_sync();
                let _ = std::fs::remove_file(&sm_journal);
            } else {
                tracing::debug!(
                    path = %sm_journal.display(),
                    "[memory_system] Session Memory journal 仍在有效期内，保留（任务可能仍在运行）"
                );
            }
        }

        // Auto Memory journal 扫描：使用 Layer 5 的 stale 阈值
        let auto_stale_threshold_ms = self.config.layer5.extraction_stale_threshold_ms;
        if let Ok(entries) = std::fs::read_dir(&self.config_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(".extraction_journal.") {
                    let journal_path = entry.path();
                    if self.is_journal_stale(&journal_path, auto_stale_threshold_ms) {
                        tracing::warn!(
                            path = %journal_path.display(),
                            stale_threshold_ms = auto_stale_threshold_ms,
                            "[memory_system] 发现过期 Auto Memory journal，清除对应 pending marker"
                        );
                        // 从 journal 文件名推导 pending marker 文件名
                        let pending_name =
                            name_str.replace(".extraction_journal.", ".extraction_pending.");
                        let pending_path = self.config_dir.join(&*pending_name);
                        if pending_path.exists() {
                            let _ = std::fs::remove_file(&pending_path);
                        }
                        let _ = std::fs::remove_file(&journal_path);
                    } else {
                        tracing::debug!(
                            path = %journal_path.display(),
                            "[memory_system] Auto Memory journal 仍在有效期内，保留（任务可能仍在运行）"
                        );
                    }
                }
            }
        }
    }

    /// 检查 journal 是否已过期
    ///
    /// 按优先级判断：
    /// 1. owner_pid == 当前进程：同一进程刚启动，journal 不是孤儿 → 不清理
    /// 2. owner_pid 对应进程仍存活（Unix: /proc/{pid}）：任务可能在运行 → 不清理
    /// 3. 进程已死或无法判断：使用 3x stale_threshold_ms 作为清理阈值
    ///    （3x 裕量覆盖真实 LLM/Forked 提取的耗时，避免误删长耗时任务）
    fn is_journal_stale(&self, journal_path: &Path, stale_threshold_ms: u64) -> bool {
        let content = match std::fs::read_to_string(journal_path) {
            Ok(c) => c,
            Err(_) => return true, // 无法读取视为过期
        };
        let journal_value: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => return true, // 无法解析视为过期
        };

        // 检查 owner_pid：如果进程仍存活，journal 不是孤儿
        if let Some(owner_pid) = journal_value.get("owner_pid").and_then(|v| v.as_u64()) {
            let pid = owner_pid as u32;
            if pid == std::process::id() {
                // 同一进程：刚启动的 runtime，journal 不是孤儿
                return false;
            }
            if is_pid_alive(pid) {
                // 进程仍在运行：任务可能仍在执行
                tracing::debug!(pid, "[memory_system] journal owner 进程仍存活，不清理");
                return false;
            }
            // 进程已死：使用 3x 阈值，比正常 stale 多一些裕量
        }

        // 无 owner_pid（旧格式 journal）或进程已死：基于时间判断
        let started_at_str = journal_value
            .get("started_at")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let started_at = match chrono::DateTime::parse_from_rfc3339(started_at_str) {
            Ok(dt) => dt,
            Err(_) => return true, // 时间格式无效视为过期
        };
        // 使用 3x 阈值：真实 LLM/Forked 提取可能超过单次 stale 阈值
        let effective_threshold_ms = stale_threshold_ms * 3;
        let elapsed = chrono::Utc::now()
            .signed_duration_since(started_at.with_timezone(&chrono::Utc))
            .num_milliseconds();
        elapsed >= effective_threshold_ms as i64
    }

    /// 检查是否有未完成的 Session Memory 提取标记（跨 runtime 可见）
    pub fn has_pending_extraction_marker(&self) -> bool {
        self.extraction_pending_marker_path().exists()
    }

    /// 读取提取 pending 标记中存储的开始时间戳（Unix epoch 毫秒）
    /// 返回 None 表示标记不存在或内容无法解析
    pub fn read_extraction_marker_timestamp_ms(&self) -> Option<u64> {
        let marker_path = self.extraction_pending_marker_path();
        let content = std::fs::read_to_string(&marker_path).ok()?;
        content.trim().parse().ok()
    }

    /// 获取提取 pending 标记文件路径
    fn extraction_pending_marker_path(&self) -> PathBuf {
        let session_dir = self.session_dir();
        session_dir.join(".extraction_pending")
    }

    /// 重新加载游标状态
    ///
    /// 在后台提取任务完成后调用，确保下次检查使用最新的游标状态。
    pub async fn reload_cursors(&mut self) -> std::io::Result<()> {
        self.cursor_manager.load().await?;
        tracing::trace!("[memory_system] Cursor state reloaded");
        Ok(())
    }

    /// 获取会话目录路径
    ///
    /// 注意：session_id 中的冒号、斜杠等字符会被替换为下划线，以确保跨平台兼容性。
    /// 例如：`cli:default` -> `cli_default`
    pub fn session_dir(&self) -> PathBuf {
        use blockcell_core::session_file_stem;
        let safe_session_id = session_file_stem(&self.session_id);
        self.workspace_dir.join("sessions").join(safe_session_id)
    }

    /// 标记会话为活跃状态
    ///
    /// 创建/更新 `.active` 文件，用于防止 prune 删除活跃会话。
    async fn mark_session_active(&self) -> std::io::Result<()> {
        let session_dir = self.session_dir();
        tokio::fs::create_dir_all(&session_dir).await?;

        let active_file = session_dir.join(".active");
        tokio::fs::write(&active_file, chrono::Utc::now().to_rfc3339()).await?;

        tracing::trace!(
            session_id = %self.session_id,
            active_file = %active_file.display(),
            "[memory_system] Session marked as active"
        );
        Ok(())
    }

    /// 清除会话活跃标记
    ///
    /// 在会话结束时调用，允许 prune 清理该会话。
    pub async fn clear_session_active(&self) -> std::io::Result<()> {
        let active_file = self.session_dir().join(".active");

        if tokio::fs::try_exists(&active_file).await? {
            tokio::fs::remove_file(&active_file).await?;
            tracing::trace!(
                session_id = %self.session_id,
                "[memory_system] Session active marker cleared"
            );
        }
        Ok(())
    }

    /// Clear the active marker from synchronous teardown paths (the runtime
    /// memory-system pool returns entries from `Drop`, where awaiting is not
    /// possible). Idle pooled sessions must not block pruning.
    pub(crate) fn clear_session_active_sync(&self) {
        let active_file = self.session_dir().join(".active");
        if let Err(error) = std::fs::remove_file(&active_file) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(
                    session_id = %self.session_id,
                    error = %error,
                    "Failed to clear pooled session active marker"
                );
            }
        }
    }

    /// 创建并初始化记忆系统（便捷方法）
    pub async fn new_initialized(
        config: MemorySystemConfig,
        workspace_dir: PathBuf,
        config_dir: PathBuf,
        session_id: String,
    ) -> std::io::Result<Self> {
        let mut system = Self::new(config, workspace_dir, config_dir, session_id);
        system.initialize().await?;
        Ok(system)
    }

    /// 获取 Session Memory 文件路径
    pub fn session_memory_path(&self) -> PathBuf {
        get_session_memory_path(&self.workspace_dir, &self.session_id)
    }

    /// 检查是否应该提取 Session Memory
    pub fn should_extract_session_memory(&self, messages: &[ChatMessage]) -> bool {
        // 检查跨 runtime 可见的提取 pending 文件标记
        // 在 Gateway/异步消息模式下，前一个 runtime 可能已被 drop，
        // 但其后台提取任务可能仍在运行。此检查防止在新 runtime 中重复触发提取。
        if self.has_pending_extraction_marker() {
            let marker_path = self.extraction_pending_marker_path();
            if let Ok(metadata) = std::fs::metadata(&marker_path) {
                if let Ok(modified) = metadata.modified() {
                    let stale_threshold = std::time::Duration::from_millis(
                        self.state
                            .session_memory
                            .config
                            .extraction_stale_threshold_ms,
                    );
                    if let Ok(elapsed) = modified.elapsed() {
                        if elapsed < stale_threshold {
                            // 标记未过期，前一个 runtime 的提取可能仍在进行
                            tracing::debug!(
                                elapsed_ms = elapsed.as_millis(),
                                "[memory_system] 提取 pending 标记未过期，跳过重复触发"
                            );
                            return false;
                        }
                    }
                }
            }
            // 标记已过期，但在清理前先检查 journal 的 owner_pid
            // 如果 journal 显示任务所属进程仍存活，说明长耗时任务正在运行，不应删除 marker
            let journal_path = self.session_memory_journal_path();
            let stale_threshold_ms = self
                .state
                .session_memory
                .config
                .extraction_stale_threshold_ms;
            if journal_path.exists() && !self.is_journal_stale(&journal_path, stale_threshold_ms) {
                tracing::debug!(
                    "[memory_system] Session Memory marker 虽然过期，但 journal 显示任务仍在运行，不删除"
                );
                return false;
            }
            // journal 也确认过期或不存在，安全清理 marker
            tracing::warn!(
                "[memory_system] 提取 pending 标记已过期且 journal 确认可清理，允许重新触发"
            );
            self.clear_extraction_marker_sync();
        }
        should_extract_memory(messages, &self.state.session_memory)
    }

    /// 检查是否应该执行 Compact
    pub fn should_compact(&self, current_tokens: usize) -> bool {
        if !self.config.compact_enabled {
            return false;
        }
        should_compact(
            current_tokens,
            self.config.token_budget,
            self.config.layer4.compact_threshold_ratio,
        )
    }

    /// 更新 Session Memory 状态
    pub fn update_session_memory_state(&mut self, message_index: usize, token_count: usize) {
        self.state.session_memory.last_memory_message_index = Some(message_index);
        self.state.session_memory.tokens_at_last_extraction = token_count;
        self.state.session_memory.initialized = true;
    }

    /// 更新 Session Memory 状态（包含消息 ID）
    ///
    /// 推荐使用此方法，因为消息 ID 在消息列表被修改时仍然有效
    pub fn update_session_memory_state_with_id(
        &mut self,
        message_id: Option<String>,
        message_index: usize,
        token_count: usize,
    ) {
        self.state.session_memory.last_memory_message_id = message_id;
        self.state.session_memory.last_memory_message_index = Some(message_index);
        self.state.session_memory.tokens_at_last_extraction = token_count;
        self.state.session_memory.initialized = true;
        self.state.session_memory.last_extracted_at = Some(std::time::Instant::now());
        self.state.session_memory.extraction_started_at = None;
        self.state.has_pending_extraction = false;
        // 清除跨 runtime 可见的提取 pending 文件标记
        self.clear_extraction_marker_sync();
    }

    /// Compact 后重置 Session/Auto 增量基线
    ///
    /// Compact 会把历史替换为短摘要，token 数和消息数大幅减少。
    /// 如果不重置基线：
    /// - Session Memory: `current_tokens.saturating_sub(tokens_at_last_extraction)` 会为 0
    /// - Auto Memory: `current_count.saturating_sub(last_message_count)` 会为 0
    ///
    /// 导致记忆提取长期不再触发，直到压缩后历史重新长到压缩前大小。
    pub fn reset_baselines_after_compact(&mut self, post_compact_messages: &[ChatMessage]) {
        // Session Memory: 重置 token 基线
        let post_compact_tokens = crate::token::estimate_messages_tokens(post_compact_messages);
        let old_tokens = self.state.session_memory.tokens_at_last_extraction;
        if old_tokens > post_compact_tokens {
            tracing::info!(
                old_tokens,
                new_tokens = post_compact_tokens,
                "[memory_system] Compact 后重置 Session Memory token 基线"
            );
            self.state.session_memory.tokens_at_last_extraction = post_compact_tokens;
        }

        // Auto Memory: 重置游标消息计数基线
        self.cursor_manager
            .reset_message_count_baseline(post_compact_messages.len());

        // 持久化 Session Memory 状态
        self.save_session_memory_state_sync();
    }

    /// 标记提取开始（先创建文件标记，成功后再设内存状态）
    /// 返回 true 表示标记创建成功，false 表示已有其他 runtime 在提取（应跳过 spawn）
    /// 先创建 marker 再设内存状态，失败时内存状态保持干净，避免污染
    pub fn mark_extraction_started(&mut self) -> bool {
        // 先原子创建跨 runtime 可见的文件标记
        let marker_created = self.touch_extraction_marker_sync();
        if marker_created {
            // 标记创建成功，再设置内存状态
            self.state.session_memory.extraction_started_at = Some(std::time::Instant::now());
            self.state.has_pending_extraction = true;
        }
        marker_created
    }

    /// 标记提取失败（清除 extraction_started_at + 文件标记，保留其他状态）
    pub fn mark_extraction_failed(&mut self) {
        self.state.session_memory.extraction_started_at = None;
        self.state.has_pending_extraction = false;
        // 清除跨 runtime 可见的文件标记
        self.clear_extraction_marker_sync();
    }

    /// 获取内容替换状态
    pub fn content_replacement_state(&self) -> &ContentReplacementState {
        &self.state.content_replacement
    }

    /// 获取可变内容替换状态
    pub fn content_replacement_state_mut(&mut self) -> &mut ContentReplacementState {
        &mut self.state.content_replacement
    }

    /// 获取 Session Memory 状态
    pub fn session_memory_state(&self) -> &SessionMemoryState {
        &self.state.session_memory
    }

    /// 获取可变 Session Memory 状态
    pub fn session_memory_state_mut(&mut self) -> &mut SessionMemoryState {
        &mut self.state.session_memory
    }

    /// 获取 Compact Hooks 注册表
    pub fn compact_hooks(&self) -> &CompactHookRegistry {
        &self.compact_hooks
    }

    /// 获取可变 Compact Hooks 注册表
    pub fn compact_hooks_mut(&mut self) -> &mut CompactHookRegistry {
        &mut self.compact_hooks
    }

    /// 标记有待处理的提取任务
    pub fn set_pending_extraction(&mut self, pending: bool) {
        self.state.has_pending_extraction = pending;
    }

    /// 检查是否有待处理的提取任务
    pub fn has_pending_extraction(&self) -> bool {
        self.state.has_pending_extraction
    }

    /// 标记需要重新加载游标状态
    pub fn set_needs_cursor_reload(&mut self, needs_reload: bool) {
        self.state.needs_cursor_reload = needs_reload;
    }

    /// 检查是否需要重新加载游标状态
    pub fn needs_cursor_reload(&self) -> bool {
        self.state.needs_cursor_reload
    }

    /// 获取游标重新加载标志（用于后台任务通知）
    pub fn cursor_reload_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.cursor_reload_flag)
    }

    /// 检查并清除游标重新加载标志
    ///
    /// 如果后台任务设置了标志，返回 true 并清除标志。
    fn check_and_clear_cursor_reload(&self) -> bool {
        self.cursor_reload_flag
            .swap(false, std::sync::atomic::Ordering::Acquire)
    }

    /// 获取 Session Memory 提取结果发送端（用于后台任务传递提取结果）
    pub fn session_memory_result_sender(
        &self,
    ) -> tokio::sync::watch::Sender<SessionMemoryExtractionResult> {
        self.session_memory_result_tx.clone()
    }

    /// 检查并应用后台 Session Memory 提取结果
    ///
    /// 如果后台提取任务已完成并发送了结果，更新 SessionMemoryState。
    /// 应在主循环的 evaluate_memory_hooks 中调用。
    pub fn apply_session_memory_result(&mut self) -> bool {
        // 使用 has_changed() 检查是否有新结果
        if self.session_memory_result_rx.has_changed().ok() != Some(true) {
            return false;
        }

        let result = self.session_memory_result_rx.borrow_and_update();
        let success = result.success;
        let last_message_id = result.last_message_id.clone();
        let last_message_index = result.last_message_index;
        let token_count = result.token_count;
        drop(result); // 释放 borrow 后再调用 &mut self 方法

        if success {
            tracing::info!(
                message_id = ?last_message_id,
                message_index = last_message_index,
                token_count = token_count,
                "[memory_system] 已应用后台 Session Memory 提取结果"
            );
            self.update_session_memory_state_with_id(
                last_message_id,
                last_message_index,
                token_count,
            );
            // 持久化状态，确保 runtime 重建后能恢复提取进度
            self.save_session_memory_state_sync();
        } else {
            self.mark_extraction_failed();
            // 持久化失败状态，清除提取标记
            self.save_session_memory_state_sync();
            tracing::warn!("[memory_system] 后台 Session Memory 提取失败，已清除提取标记");
        }
        true
    }

    /// 获取配置
    pub fn config(&self) -> &MemorySystemConfig {
        &self.config
    }

    /// 获取状态
    pub fn state(&self) -> &MemorySystemState {
        &self.state
    }

    /// 获取配置目录
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// 获取工作目录
    pub fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }

    /// 获取会话 ID
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 获取文件追踪器
    pub fn file_tracker(&self) -> &FileTracker {
        &self.state.file_tracker
    }

    /// 获取可变文件追踪器
    pub fn file_tracker_mut(&mut self) -> &mut FileTracker {
        &mut self.state.file_tracker
    }

    /// 获取技能追踪器
    pub fn skill_tracker(&self) -> &SkillTracker {
        &self.state.skill_tracker
    }

    /// 获取可变技能追踪器
    pub fn skill_tracker_mut(&mut self) -> &mut SkillTracker {
        &mut self.state.skill_tracker
    }

    /// 记录文件读取
    pub fn record_file_read(&mut self, path: std::path::PathBuf, content: &str) {
        self.state.file_tracker.record_read(path, content);
    }

    /// 记录技能加载
    pub fn record_skill_load(&mut self, name: &str, content: &str) {
        self.state.skill_tracker.record_load(name, content);
    }

    /// 生成 Compact 恢复消息
    pub fn generate_compact_recovery(&self, session_memory_content: Option<&str>) -> String {
        let budget = crate::compact::RecoveryBudget::from(&self.config.layer4);
        crate::compact::build_recovery_message(
            &self.state.file_tracker,
            &self.state.skill_tracker,
            session_memory_content,
            &budget,
        )
    }

    /// 获取游标管理器
    pub fn cursor_manager(&self) -> &ExtractionCursorManager {
        &self.cursor_manager
    }

    /// 获取可变游标管理器
    pub fn cursor_manager_mut(&mut self) -> &mut ExtractionCursorManager {
        &mut self.cursor_manager
    }

    /// 检查是否应该触发自动记忆提取
    ///
    /// 过滤掉已有 pending marker 的 memory type，避免 detached 提取仍在运行时重复 spawn。
    pub fn should_extract_auto_memory(&self, messages: &[ChatMessage]) -> Vec<MemoryType> {
        let config = crate::auto_memory::AutoMemoryConfig::from(self.config.layer5.clone());
        let current_content = crate::auto_memory::build_message_content_signature(messages);
        let mut types = crate::auto_memory::should_extract_auto_memory_with_config(
            &self.cursor_manager,
            messages.len(),
            &current_content,
            &config,
        );

        // 过滤掉已有 pending marker 的 memory type
        // 前一个 runtime 的 detached 提取可能尚未完成并保存 cursor
        // 使用 Layer 5 独立的 stale 阈值，避免 Layer 3 短阈值误判 Layer 5 长任务
        let stale_threshold_ms = self.config.layer5.extraction_stale_threshold_ms;
        types.retain(|mt| !self.has_active_auto_memory_pending(mt, stale_threshold_ms));

        types
    }

    /// 检查指定 memory type 是否有活跃的 pending marker
    /// 过期时先检查对应 journal 的 owner_pid，避免误删长耗时任务的 marker
    fn has_active_auto_memory_pending(
        &self,
        memory_type: &MemoryType,
        stale_threshold_ms: u64,
    ) -> bool {
        let marker_path = self.auto_memory_pending_marker_path(memory_type);
        match std::fs::metadata(&marker_path) {
            Ok(meta) => {
                // 检查标记是否过期
                let is_stale = meta
                    .modified()
                    .ok()
                    .and_then(|mtime| mtime.elapsed().ok())
                    .map(|elapsed| elapsed.as_millis() as u64 >= stale_threshold_ms)
                    .unwrap_or(true);
                if is_stale {
                    // 标记已过期，但在清理前先检查 journal 的 owner_pid
                    // 如果 journal 显示任务所属进程仍存活，说明长耗时任务正在运行，不应删除 marker
                    let journal_path = self.auto_memory_journal_path(memory_type);
                    if journal_path.exists()
                        && !self.is_journal_stale(&journal_path, stale_threshold_ms)
                    {
                        tracing::debug!(
                            memory_type = memory_type.name(),
                            "[memory_system] Auto Memory marker 虽然过期，但 journal 显示任务仍在运行，不删除"
                        );
                        return true;
                    }
                    // journal 也确认过期或不存在，安全清理 marker
                    let _ = std::fs::remove_file(&marker_path);
                    false
                } else {
                    tracing::debug!(
                        memory_type = memory_type.name(),
                        "[memory_system] 检测到活跃的 Auto Memory pending marker，跳过提取"
                    );
                    true
                }
            }
            Err(_) => false,
        }
    }

    /// 获取 Auto Memory 提取 pending 标记文件路径
    pub fn auto_memory_pending_marker_path(&self, memory_type: &MemoryType) -> PathBuf {
        self.config_dir
            .join(format!(".extraction_pending.{}", memory_type.name()))
    }

    /// 添加后台任务句柄
    pub fn add_background_task(&mut self, handle: BackgroundTaskHandle) {
        self.state.background_tasks.push(handle);
    }

    /// 获取后台任务数量
    pub fn background_task_count(&self) -> usize {
        self.state.background_tasks.len()
    }

    /// 清理已完成的后台任务
    ///
    /// 返回清理的任务数量
    pub fn cleanup_completed_tasks(&mut self) -> usize {
        let before = self.state.background_tasks.len();
        self.state
            .background_tasks
            .retain(|handle| !handle.is_finished());
        before - self.state.background_tasks.len()
    }

    /// 取消所有后台任务
    ///
    /// 在会话结束或需要紧急停止时调用
    pub fn abort_all_background_tasks(&mut self) {
        for handle in self.state.background_tasks.drain(..) {
            handle.abort();
        }
    }

    /// 检查是否有正在运行的后台任务
    pub fn has_running_background_tasks(&self) -> bool {
        self.state.background_tasks.iter().any(|h| !h.is_finished())
    }

    /// 等待所有后台任务完成（带超时）
    ///
    /// 在会话结束前调用，确保后台任务有时间完成。
    /// 如果超时，会取消剩余的任务。
    ///
    /// ## 参数
    /// - `timeout_secs`: 最大等待时间（秒）
    ///
    /// ## 返回
    /// - `Ok(())`: 所有任务成功完成
    /// - `Err(timeout_secs)`: 超时，剩余任务已取消
    pub async fn wait_for_background_tasks(&mut self, timeout_secs: u64) -> Result<(), u64> {
        if self.state.background_tasks.is_empty() {
            return Ok(());
        }

        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(timeout_secs);

        // 等待所有任务完成或超时
        while self.has_running_background_tasks() {
            if start.elapsed() >= timeout {
                // 超时，取消剩余任务
                let running_count = self
                    .state
                    .background_tasks
                    .iter()
                    .filter(|h| !h.is_finished())
                    .count();
                tracing::warn!(
                    running_count,
                    timeout_secs,
                    session_id = %self.session_id,
                    "[memory_system] Timeout waiting for background tasks, aborting remaining"
                );
                self.abort_all_background_tasks();
                // 确保句柄向量已清空（abort_all_background_tasks 使用 drain，已清空）
                // 但为了安全，再次调用清理
                self.state.background_tasks.clear();
                return Err(timeout_secs);
            }

            // 短暂休眠避免忙等待
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // 清理已完成的任务
        self.cleanup_completed_tasks();

        tracing::debug!(
            session_id = %self.session_id,
            duration_ms = start.elapsed().as_millis() as u64,
            "[memory_system] All background tasks completed"
        );

        Ok(())
    }
}

impl Drop for MemorySystem {
    fn drop(&mut self) {
        // 清除会话活跃标记
        //
        // ## 清理策略说明
        //
        // `.active` 文件是会话活跃标记，用于防止 Dream Service 的 prune 机制删除正在运行的会话。
        // 清理失败不会影响功能正确性，因为：
        // 1. Dream Service 会检查进程是否存活（通过 PID）
        // 2. 过期的 `.active` 文件（>24小时）会被自动清理
        //
        // 因此这里使用尽力而为（best-effort）的清理策略：
        // - 尝试在后台线程执行清理，避免阻塞当前线程
        // - 如果线程创建失败或进程快速退出，清理可能未完成，但不影响功能
        // - 依赖 Dream Service 的 prune 机制作为兜底清理
        let active_file = self.session_dir().join(".active");
        // 克隆两次：一个用于线程内，一个用于线程创建失败时的日志
        let session_id_for_thread = self.session_id.clone();
        let session_id_for_error = self.session_id.clone();

        // 使用 std::thread::Builder 以便在失败时记录警告
        let thread_result = std::thread::Builder::new()
            .name("blockcell-session-cleanup".to_string())
            .spawn(move || {
                if active_file.exists() {
                    if let Err(e) = std::fs::remove_file(&active_file) {
                        // 仅在 Debug 模式记录警告，减少生产环境日志噪音
                        tracing::debug!(
                            error = %e,
                            session_id = %session_id_for_thread,
                            "[memory_system] Best-effort cleanup failed for session active marker (will be pruned by Dream Service)"
                        );
                    } else {
                        tracing::trace!(
                            session_id = %session_id_for_thread,
                            "[memory_system] Session active marker cleared on drop"
                        );
                    }
                }
            });

        // 如果线程创建失败，记录警告但不阻塞
        if let Err(e) = thread_result {
            tracing::debug!(
                error = %e,
                session_id = %session_id_for_error,
                "[memory_system] Failed to spawn cleanup thread, relying on Dream Service prune"
            );
        }

        // 自动清理所有后台任务，防止 zombie tasks
        let running_count = self
            .state
            .background_tasks
            .iter()
            .filter(|h| !h.is_finished())
            .count();
        if running_count > 0 {
            tracing::debug!(
                running_count,
                session_id = %self.session_id,
                "[memory_system] Dropping with running background tasks, aborting them"
            );
            self.abort_all_background_tasks();
        }
    }
}

/// Post-Sampling 动作集合
///
/// 支持同一轮返回多个动作，避免 Session/Auto/Compact 同时到期时互相跳过。
/// 执行顺序：Session Memory → Auto Memory → Compact
#[derive(Debug)]
pub struct PostSamplingActions {
    /// 是否触发 Session Memory 提取
    pub session_memory: bool,
    /// 需要提取的 Auto Memory 类型（空 = 不触发）
    pub auto_memory_types: Vec<MemoryType>,
    /// 是否触发 Compact
    pub compact: bool,
}

impl PostSamplingActions {
    /// 所有动作均为空
    pub fn is_empty(&self) -> bool {
        !self.session_memory && self.auto_memory_types.is_empty() && !self.compact
    }
}

/// 检查指定 PID 的进程是否仍存活
///
/// 用于 journal cleanup 判断：owner 进程仍存活说明任务可能在运行，不应清理。
/// - 同一进程：直接返回 true
/// - Unix: 检查 /proc/{pid} 是否存在
/// - Windows: 无法简单检查（需要额外依赖），返回 false，依靠 3x 阈值裕量
fn is_pid_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(unix)]
    {
        std::path::Path::new(&format!("/proc/{}", pid)).exists()
    }
    #[cfg(not(unix))]
    {
        // Windows 下无法无依赖检查 PID 存活，依靠 is_journal_stale 的 3x 阈值裕量
        let _ = pid;
        false
    }
}

/// 检查是否应该触发记忆操作
///
/// ## 游标状态同步
/// 如果后台任务设置了 `cursor_reload_flag`，会先重新加载游标状态。
/// 这确保了后台提取任务完成后，主线程使用最新的游标状态。
///
/// ## 为什么是 async
/// 此函数从 async 运行时循环中调用。之前使用 `block_in_place + block_on`
/// 在单线程 runtime 或 `multi_thread` 且只有 1 个 worker 时会死锁，
/// 因为 `block_on` 需要另一个 worker 来驱动被阻塞的 future。
/// 改为直接 await 可安全适用于所有 runtime 配置。
pub async fn evaluate_memory_hooks(
    memory_system: &mut MemorySystem,
    messages: &[ChatMessage],
    current_tokens: usize,
) -> PostSamplingActions {
    // 检查是否需要重新加载游标状态（后台提取完成后）
    if memory_system.check_and_clear_cursor_reload() {
        if let Err(e) = memory_system.reload_cursors().await {
            tracing::warn!(error = %e, "[evaluate_memory_hooks] Failed to reload cursor state");
        }
    }

    // 检查并应用后台 Session Memory 提取结果
    memory_system.apply_session_memory_result();

    // 收集所有需要触发的动作（不提前 return，确保 Session/Auto/Compact 同时到期时都不被跳过）
    let mut actions = PostSamplingActions {
        session_memory: false,
        auto_memory_types: Vec::new(),
        compact: false,
    };

    // 1. 检查 Session Memory 提取（在 Compact 之前，确保压缩前完成会话记忆快照）
    // 设计文档 Post-Sampling 顺序：Layer 3/5 先于 Layer 4
    // 如果长会话首次达到 compact 阈值但还没提取 Session Memory，
    // 必须先提取再压缩，否则压缩后原始长历史丢失，Layer 3 再也无法提取
    if memory_system.should_extract_session_memory(messages) {
        actions.session_memory = true;
    }

    // 2. 检查自动记忆提取（在 Compact 之前，确保压缩前用完整历史提取）
    // 设计文档 Post-Sampling 顺序：Layer 3/5 先于 Layer 4
    // Auto Memory 需要完整历史作为提取材料，必须在压缩前执行，
    // 否则压缩后 history 被替换为摘要，提取质量下降或完全丢失
    if memory_system.config().auto_memory_enabled {
        // 使用已加载的 cursor_manager，确保冷却机制正确工作
        let types_to_extract = memory_system.should_extract_auto_memory(messages);

        if !types_to_extract.is_empty() {
            actions.auto_memory_types = types_to_extract;
        }
    }

    // 3. 检查 Compact（Layer 3/5 都不需要时也检查，支持同轮组合触发）
    if memory_system.should_compact(current_tokens) {
        actions.compact = true;
    }

    actions
}

/// 默认记忆目录路径
pub fn default_memory_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".blockcell")
        .join("memory")
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::config::Layer4Config;

    #[test]
    fn extraction_marker_guard_cleans_files_during_panic_unwind() {
        let temp = tempfile::tempdir().expect("创建临时目录");
        let marker = temp.path().join(".extraction_pending");
        let journal = temp.path().join(".extraction_journal");
        std::fs::write(&marker, b"").expect("创建 marker");
        std::fs::write(&journal, b"{}").expect("创建 journal");

        let result = std::panic::catch_unwind(|| {
            let _guard = ExtractionMarkerGuard::new(marker.clone(), journal.clone());
            panic!("模拟后台提取 panic");
        });

        assert!(result.is_err());
        assert!(!marker.exists());
        assert!(!journal.exists());
    }

    #[test]
    fn extraction_marker_guard_preserve_keeps_files() {
        let temp = tempfile::tempdir().expect("创建临时目录");
        let marker = temp.path().join(".extraction_pending.preference");
        let journal = temp.path().join(".extraction_journal.preference");
        std::fs::write(&marker, b"").expect("创建 marker");
        std::fs::write(&journal, b"{}").expect("创建 journal");

        let mut guard = ExtractionMarkerGuard::new(marker.clone(), journal.clone());
        guard.preserve();
        drop(guard);

        assert!(marker.exists());
        assert!(journal.exists());
    }

    #[test]
    fn test_memory_system_config_default() {
        let config = MemorySystemConfig::default();
        assert!(config.auto_memory_enabled);
        assert!(config.compact_enabled);
        assert_eq!(config.layer4.compact_threshold_ratio, 0.8);
    }

    #[test]
    fn test_memory_system_new() {
        let config = MemorySystemConfig::default();
        let memory_system = MemorySystem::new(
            config,
            PathBuf::from("/workspace"),
            PathBuf::from("/config"),
            "test-session".to_string(),
        );

        assert_eq!(memory_system.session_id(), "test-session");
        assert!(!memory_system.has_pending_extraction());
    }

    #[test]
    fn test_should_compact() {
        let config = MemorySystemConfig {
            token_budget: 100_000,
            layer4: Layer4Config {
                compact_threshold_ratio: 0.8,
                ..Default::default()
            },
            ..Default::default()
        };
        let memory_system = MemorySystem::new(
            config,
            PathBuf::from("/workspace"),
            PathBuf::from("/config"),
            "test".to_string(),
        );

        // 低于阈值
        assert!(!memory_system.should_compact(70_000));

        // 达到阈值
        assert!(memory_system.should_compact(80_000));

        // 超过阈值
        assert!(memory_system.should_compact(100_000));
    }

    #[test]
    fn test_should_compact_disabled() {
        let config = MemorySystemConfig {
            compact_enabled: false,
            ..Default::default()
        };
        let memory_system = MemorySystem::new(
            config,
            PathBuf::from("/workspace"),
            PathBuf::from("/config"),
            "test".to_string(),
        );

        // 即使超过阈值也不触发
        assert!(!memory_system.should_compact(1_000_000));
    }

    #[tokio::test]
    async fn test_evaluate_memory_hooks_none() {
        let config = MemorySystemConfig::default();
        let mut memory_system = MemorySystem::new(
            config,
            PathBuf::from("/workspace"),
            PathBuf::from("/config"),
            "test".to_string(),
        );

        let messages = vec![ChatMessage::user("Hello"), ChatMessage::assistant("Hi!")];

        let actions = evaluate_memory_hooks(&mut memory_system, &messages, 100).await;
        assert!(actions.is_empty());
    }

    #[tokio::test]
    async fn test_evaluate_memory_hooks_compact() {
        let config = MemorySystemConfig {
            token_budget: 100,
            layer4: Layer4Config {
                compact_threshold_ratio: 0.8,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut memory_system = MemorySystem::new(
            config,
            PathBuf::from("/workspace"),
            PathBuf::from("/config"),
            "test".to_string(),
        );

        let messages = vec![ChatMessage::user("Test")];
        let actions = evaluate_memory_hooks(&mut memory_system, &messages, 100).await;

        assert!(actions.compact);
    }

    #[test]
    fn test_memory_system_file_tracker() {
        let config = MemorySystemConfig::default();
        let mut memory_system = MemorySystem::new(
            config,
            PathBuf::from("/workspace"),
            PathBuf::from("/config"),
            "test".to_string(),
        );

        // 记录文件读取
        memory_system.record_file_read(PathBuf::from("/test.rs"), "test content");

        let tracker = memory_system.file_tracker();
        assert_eq!(tracker.len(), 1);
    }

    #[test]
    fn test_memory_system_skill_tracker() {
        let config = MemorySystemConfig::default();
        let mut memory_system = MemorySystem::new(
            config,
            PathBuf::from("/workspace"),
            PathBuf::from("/config"),
            "test".to_string(),
        );

        // 记录技能加载
        memory_system.record_skill_load("test_skill", "skill content");

        let tracker = memory_system.skill_tracker();
        assert_eq!(tracker.len(), 1);
    }

    #[test]
    fn test_memory_system_update_session_memory_state() {
        let config = MemorySystemConfig::default();
        let mut memory_system = MemorySystem::new(
            config,
            PathBuf::from("/workspace"),
            PathBuf::from("/config"),
            "test".to_string(),
        );

        memory_system.update_session_memory_state(42, 5000);

        let state = memory_system.session_memory_state();
        assert_eq!(state.last_memory_message_index, Some(42));
        assert_eq!(state.tokens_at_last_extraction, 5000);
        assert!(state.initialized);
    }

    #[test]
    fn test_memory_system_pending_extraction() {
        let config = MemorySystemConfig::default();
        let mut memory_system = MemorySystem::new(
            config,
            PathBuf::from("/workspace"),
            PathBuf::from("/config"),
            "test".to_string(),
        );

        assert!(!memory_system.has_pending_extraction());

        memory_system.set_pending_extraction(true);
        assert!(memory_system.has_pending_extraction());

        memory_system.set_pending_extraction(false);
        assert!(!memory_system.has_pending_extraction());
    }

    #[test]
    fn test_memory_system_generate_compact_recovery() {
        let config = MemorySystemConfig::default();
        let mut memory_system = MemorySystem::new(
            config,
            PathBuf::from("/workspace"),
            PathBuf::from("/config"),
            "test".to_string(),
        );

        // 记录文件和技能
        memory_system.record_file_read(PathBuf::from("/test.rs"), "file content");
        memory_system.record_skill_load("test_skill", "skill content");

        // 生成恢复消息
        let recovery = memory_system.generate_compact_recovery(Some("session memory content"));

        assert!(recovery.contains("Files Previously Read"));
        assert!(recovery.contains("Skills Previously Loaded"));
        assert!(recovery.contains("Session Memory"));
    }

    #[test]
    fn test_memory_system_generate_compact_recovery_empty() {
        let config = MemorySystemConfig::default();
        let memory_system = MemorySystem::new(
            config,
            PathBuf::from("/workspace"),
            PathBuf::from("/config"),
            "test".to_string(),
        );

        // 不记录任何内容，生成空恢复消息
        let recovery = memory_system.generate_compact_recovery(None);

        // 应该是空字符串
        assert!(recovery.is_empty());
    }

    #[tokio::test]
    async fn test_post_sampling_action_order() {
        // 测试 Post-Sampling 优先级：Session Memory > Auto Memory > Compact
        // 设计文档要求 Layer 3/5 先于 Layer 4，确保压缩前完成提取
        let config = MemorySystemConfig {
            token_budget: 100,
            layer4: Layer4Config {
                compact_threshold_ratio: 0.8,
                ..Default::default()
            },
            auto_memory_enabled: true,
            ..Default::default()
        };
        let mut memory_system = MemorySystem::new(
            config,
            PathBuf::from("/workspace"),
            PathBuf::from("/config"),
            "test".to_string(),
        );

        // 有足够消息触发 auto memory，且 auto memory 优先级高于 Compact
        let messages: Vec<ChatMessage> = (0..20)
            .flat_map(|i| {
                vec![
                    ChatMessage::user(&format!("msg {}", i)),
                    ChatMessage::assistant("resp"),
                ]
            })
            .collect();

        let actions = evaluate_memory_hooks(&mut memory_system, &messages, 100).await;

        // Auto Memory 和 Compact 可以同时触发（设计文档：Layer 3/5 先于 Layer 4）
        assert!(
            !actions.auto_memory_types.is_empty(),
            "Auto Memory 应被触发，实际: {:?}",
            actions
        );
    }
}
