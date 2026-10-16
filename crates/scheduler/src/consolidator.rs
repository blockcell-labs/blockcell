//! 梦境机制 - Layer 6 知识整合
//!
//! 后台跨会话知识整合，使用三重门控机制。
//!
//! ## 三重门控
//! 1. 时间门控：距上次整合 > 24 小时
//! 2. 会话门控：新会话数 > 5
//! 3. 锁门控：无其他进程正在整合
//!
//! ## 四阶段执行
//! 1. Orient - 定位现有内容
//! 2. Gather - 收集新信号
//! 3. Consolidate - 整合知识（使用 Forked Agent）
//! 4. Prune - 修剪索引

use blockcell_agent::forked::{
    build_forked_tool_schemas, create_dream_can_use_tool, run_forked_agent, CacheSafeParams,
    ForkedAgentParams, ForkedAgentResult,
};
use blockcell_agent::memory_event;
use blockcell_agent::session_metrics::get_dream_circuit_breaker;
use blockcell_agent::CrossProcessLock;
use blockcell_core::file_store::ExclusiveFileLock;
use blockcell_core::types::ChatMessage;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::fs;

/// 门控配置
pub const TIME_GATE_THRESHOLD_HOURS: u64 = 24;
pub const SESSION_GATE_THRESHOLD: usize = 5;
pub const LOCK_FILE_NAME: &str = ".dream_lock";
pub const DREAM_STATE_FILE: &str = ".dream_state.json";
const DREAM_COMMIT_BACKUP_DIR: &str = ".dream_commit_backup";
const DREAM_COMMIT_MANIFEST_FILE: &str = "manifest.json";

/// Session Memory 过期阈值（天）
pub const SESSION_MEMORY_EXPIRY_DAYS: u64 = 7;
/// 每次处理的最大 session memory 文件数
pub const MAX_SESSIONS_TO_PROCESS: usize = 10;
pub const DREAM_FORKED_AGENT_MAX_TURNS: u32 = 16;
/// is_consolidating 标记的 stale 阈值（秒）
///
/// 超过此时间仍为 is_consolidating=true 时，视为上次整合异常退出留下的 stale 标记，
/// gate 自动清除并允许新的整合。默认 1 小时，远大于正常整合超时（300s）。
pub const CONSOLIDATING_STALE_THRESHOLD_SECS: u64 = 3600;

// --- submodules extracted from the original monolithic consolidator.rs ---
mod consolidate;
mod dream;
mod gather;
mod locking;
mod prune;
mod setup;
mod staging;

pub(crate) use staging::*;
/// Dream 执行统计数据
#[derive(Debug, Clone, Default)]
pub struct DreamStats {
    /// 创建的记忆数
    pub memories_created: usize,
    /// 更新的记忆数
    pub memories_updated: usize,
    /// 删除的记忆数
    pub memories_deleted: usize,
    /// 修剪的会话数
    pub sessions_pruned: usize,
    /// 处理的会话数
    pub sessions_processed: usize,
}

impl DreamStats {
    fn has_memory_changes(&self) -> bool {
        self.memories_created > 0 || self.memories_updated > 0 || self.memories_deleted > 0
    }
}

/// 梦境状态
///
/// 字段必须添加 `#[serde(default)]` 以保证向后兼容：
/// 当新增字段时，旧版 .dream_state.json 文件仍能正确反序列化。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DreamState {
    /// 上次整合时间戳
    #[serde(default)]
    pub last_consolidation_time: Option<u64>,
    /// 上次整合时的会话数
    #[serde(default)]
    pub last_session_count: usize,
    /// 当前会话数
    #[serde(default)]
    pub current_session_count: usize,
    /// 整合次数
    #[serde(default)]
    pub consolidation_count: usize,
    /// 是否正在整合
    #[serde(default)]
    pub is_consolidating: bool,
    /// 整合开始时间戳（Unix 秒），用于检测 stale 的 is_consolidating 标记
    ///
    /// 当 is_consolidating=true 但 consolidating_started_at 超过
    /// CONSOLIDATING_STALE_THRESHOLD_SECS 时，视为 stale 标记，
    /// gate 自动清除并允许新的整合。
    #[serde(default)]
    pub consolidating_started_at: Option<u64>,
}

impl DreamState {
    /// 加载状态
    pub async fn load(config_dir: &Path) -> std::io::Result<Self> {
        let path = config_dir.join(DREAM_STATE_FILE);
        match fs::read_to_string(&path).await {
            Ok(content) => {
                match serde_json::from_str(&content) {
                    Ok(state) => Ok(state),
                    Err(e) => {
                        // JSON 解析失败，可能文件损坏，记录警告并使用默认值
                        tracing::warn!(
                            error = %e,
                            path = %path.display(),
                            "[dream] Failed to parse dream state file, using defaults (file may be corrupted)"
                        );
                        Ok(Self::default())
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 主文件不存在，尝试从 atomic_write 产生的备份恢复
                // 备份文件名格式为 .dream_state.json.bak.<pid>.<counter>
                let bak_path = blockcell_agent::fs_util::find_latest_backup(&path);
                if let Some(bak) = bak_path {
                    tracing::warn!(
                        path = %path.display(),
                        bak = %bak.display(),
                        "[dream] 主文件不存在但发现备份文件，尝试恢复"
                    );
                    match fs::read_to_string(&bak).await {
                        Ok(bak_content) => {
                            match serde_json::from_str(&bak_content) {
                                Ok(state) => {
                                    // 恢复成功：将备份内容写入主文件
                                    let write_content = serde_json::to_string_pretty(&state)?;
                                    tokio::task::spawn_blocking(move || {
                                        blockcell_agent::fs_util::atomic_write(
                                            &path,
                                            write_content.as_bytes(),
                                        )
                                    })
                                    .await
                                    .map_err(|e| std::io::Error::other(e.to_string()))?
                                    .map_err(|e| std::io::Error::other(e.to_string()))?;
                                    tracing::info!("[dream] 从备份文件恢复成功");
                                    Ok(state)
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "[dream] 解析备份文件失败，使用默认值");
                                    Ok(Self::default())
                                }
                            }
                        }
                        Err(_) => {
                            tracing::warn!("[dream] 读取备份文件失败，使用默认值");
                            Ok(Self::default())
                        }
                    }
                } else {
                    Ok(Self::default())
                }
            }
            Err(e) => Err(e),
        }
    }

    /// 保存状态（原子写入 + 跨进程锁，防止并发写入和崩溃导致文件损坏）
    ///
    /// 使用与 agent 侧 `increment_dream_session_count()` 相同的锁文件
    /// `.dream_state.json.lock`，确保 scheduler 和 agent 的 read-modify-write
    /// 序列互斥，避免 TOCTOU 竞争导致计数丢失。
    ///
    /// 获取锁失败时返回错误，不再继续非原子写入，
    /// 避免在锁被其他进程持有时引入覆盖 session count 的风险。
    pub async fn save(&self, config_dir: &Path) -> std::io::Result<()> {
        let lock_path = config_dir
            .join(DREAM_STATE_FILE)
            .with_extension("json.lock");
        let _lock_guard = crate::blocking::run_blocking_io({
            let lock_path = lock_path.clone();
            move || CrossProcessLock::acquire(&lock_path)
        })
        .await
        .map_err(|e| {
            tracing::warn!(
                error = %e,
                "[dream] save: 获取跨进程锁失败，拒绝写入以防止覆盖风险"
            );
            std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!("获取跨进程锁失败，拒绝非原子写入: {}", e),
            )
        })?;

        let path = config_dir.join(DREAM_STATE_FILE);
        let content = serde_json::to_string_pretty(self)?;
        // 使用 blockcell_agent::fs_util::atomic_write 保证原子性
        // （backup-based 策略，Windows 安全）
        let write_result = tokio::task::spawn_blocking(move || {
            blockcell_agent::fs_util::atomic_write(&path, content.as_bytes())
        })
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?;
        write_result.map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(())
    }

    /// 保存状态，不获取跨进程锁。
    ///
    /// 供已持有 `.dream_state.json.lock` 的调用者使用（如 `dream()` 最终合并），
    /// 避免持锁后再调用会重新抢锁的 `save()`，导致死锁或锁间隙。
    async fn save_unlocked(&self, config_dir: &Path) -> std::io::Result<()> {
        let path = config_dir.join(DREAM_STATE_FILE);
        let content = serde_json::to_string_pretty(self)?;
        let write_result = tokio::task::spawn_blocking(move || {
            blockcell_agent::fs_util::atomic_write(&path, content.as_bytes())
        })
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?;
        write_result.map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(())
    }

    /// 增加会话计数
    pub fn increment_session_count(&mut self) {
        self.current_session_count += 1;
    }
}

fn apply_successful_dream_state(
    state: &mut DreamState,
    stats: &DreamStats,
    processed_session_count: usize,
    now_secs: u64,
) {
    state.last_consolidation_time = Some(now_secs);
    // 成功执行过的会话批次都要推进游标；即使本轮没有产出记忆变更，
    // 也不能让下一次时间门打开后反复处理同一批会话。
    state.last_session_count = processed_session_count;

    if stats.has_memory_changes() {
        state.consolidation_count += 1;
    } else {
        tracing::info!("[dream] consolidation produced no changes");
    }
}

fn validate_dream_agent_result(agent_result: &ForkedAgentResult) -> Result<(), String> {
    if agent_result.had_tool_error {
        return Err("Forked Agent 工具调用失败 (had_tool_error=true)".to_string());
    }

    if agent_result.truncated {
        return Err("Forked Agent reached max_turns before finishing (truncated=true)".to_string());
    }

    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct DreamStagingRun {
    root: PathBuf,
    memory_dir: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MemoryTreeSnapshot {
    files: HashMap<PathBuf, FileFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileFingerprint {
    len: u64,
    hash: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct StagedMemoryChange {
    rel_path: PathBuf,
    staged_path: PathBuf,
    real_path: PathBuf,
    existed_before: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DreamCommitManifest {
    changes: Vec<DreamCommitManifestChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DreamCommitManifestChange {
    rel_path: PathBuf,
    existed_before: bool,
}

#[derive(Debug)]
pub(crate) struct StagedMemoryCommit {
    stats: DreamStats,
    changes: Vec<StagedMemoryChange>,
    backup_root: Option<PathBuf>,
}

impl StagedMemoryCommit {
    async fn finalize(mut self) {
        if let Some(backup_root) = self.backup_root.take() {
            cleanup_commit_backup(&backup_root).await;
        }
    }

    async fn rollback(mut self) -> Result<(), DreamError> {
        let Some(backup_root) = self.backup_root.take() else {
            return Ok(());
        };

        let result = rollback_staged_memory_changes(&self.changes, &backup_root).await;
        cleanup_commit_backup(&backup_root).await;
        result
    }
}

/// 收集到的信号
#[derive(Debug, Clone)]
pub struct GatheredSignal {
    /// 信号标题（章节名）
    pub title: String,
    /// 信号内容
    pub content: String,
    /// 重要性分数 (0-10)
    pub importance: u8,
    /// 来源时间
    pub source_time: SystemTime,
}

/// 门控检查结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateCheckResult {
    /// 通过所有门控
    Passed,
    /// 时间门控未通过
    TimeGateFailed,
    /// 会话门控未通过
    SessionGateFailed,
    /// 锁门控未通过（有其他进程正在整合）
    LockGateFailed,
}

/// 整合器配置（从 DreamServiceConfig 传入，覆盖硬编码常量）
#[derive(Debug, Clone)]
pub struct ConsolidatorConfig {
    /// 时间门限阈值（小时）
    pub time_gate_threshold_hours: f64,
    /// 会话门限阈值（会话数）
    pub session_gate_threshold: usize,
}

impl Default for ConsolidatorConfig {
    fn default() -> Self {
        Self {
            time_gate_threshold_hours: TIME_GATE_THRESHOLD_HOURS as f64,
            session_gate_threshold: SESSION_GATE_THRESHOLD,
        }
    }
}

/// 三重门控检查（使用配置值）
///
/// 当 is_consolidating=true 但 consolidating_started_at 超过 stale 阈值时，
/// 自动清除 stale 标记并持久化，允许新的整合继续。
/// 这防止了因临时磁盘/锁异常导致 is_consolidating 永久卡住的问题。
pub async fn check_gates(
    state: &mut DreamState,
    config_dir: &Path,
    config: &ConsolidatorConfig,
) -> GateCheckResult {
    let memory_dir = config_dir.join("memory");

    // 1. 检查锁门控
    if state.is_consolidating {
        // 检查是否为 stale 标记：consolidating_started_at 超过阈值
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let is_stale = match state.consolidating_started_at {
            Some(started_at) => {
                // 有开始时间，检查是否超过 stale 阈值
                now.saturating_sub(started_at) > CONSOLIDATING_STALE_THRESHOLD_SECS
            }
            None => {
                // is_consolidating=true 但没有 consolidating_started_at，
                // 说明是旧格式数据或异常状态。
                // 复用 .dream_lock 的有效性检查逻辑：
                // 如果 lock 文件不存在或已失效（进程退出/超时），标记一定是 stale。
                let lock_path = config_dir.join(LOCK_FILE_NAME);
                if !lock_path.exists() {
                    // 锁文件不存在，没有进程正在整合，标记是 stale
                    true
                } else {
                    // 锁文件存在，但需要检查其有效性（进程是否存活、是否超时）
                    match check_lock_validity(&lock_path).await {
                        Ok(true) => {
                            // 锁仍有效（进程存活且未过期），不是 stale
                            tracing::warn!(
                                "[dream] is_consolidating=true 但无 consolidating_started_at，且 .dream_lock 仍有效，gate 阻止新整合"
                            );
                            false
                        }
                        Ok(false) => {
                            // 锁已失效（进程退出或超时），标记是 stale
                            tracing::warn!(
                                "[dream] is_consolidating=true 但无 consolidating_started_at，且 .dream_lock 已失效，自动清除 stale 标记并清理无效锁"
                            );
                            // 清理无效的锁文件
                            let _ = fs::remove_file(&lock_path).await;
                            true
                        }
                        Err(e) => {
                            // 无法读取锁文件，视为 stale 并清理
                            tracing::warn!(
                                error = %e,
                                "[dream] is_consolidating=true 但无 consolidating_started_at，无法读取 .dream_lock，视为 stale 并清理"
                            );
                            let _ = fs::remove_file(&lock_path).await;
                            true
                        }
                    }
                }
            }
        };

        if is_stale {
            tracing::warn!(
                started_at = ?state.consolidating_started_at,
                "[dream] 检测到 stale 的 is_consolidating 标记，自动清除恢复"
            );
            if let Err(e) = recover_dream_commit_backups(config_dir, &memory_dir, true).await {
                tracing::warn!(
                    error = %e,
                    "[dream] Failed to recover stale dream memory commit backups"
                );
            }
            state.is_consolidating = false;
            state.consolidating_started_at = None;
            // 持久化清除后的状态，确保后续 gate 不再被卡住
            if let Err(e) = state.save(config_dir).await {
                tracing::warn!(
                    error = %e,
                    "[dream] 清除 stale 标记后保存状态失败，下次 gate 会再次尝试清除"
                );
                // 保存失败不阻止 gate 通过：内存中已清除，下次加载会重新检测
            }
        } else {
            return GateCheckResult::LockGateFailed;
        }
    } else if let Err(e) = recover_dream_commit_backups(config_dir, &memory_dir, false).await {
        tracing::warn!(
            error = %e,
            "[dream] Failed to clean finalized dream memory commit backups"
        );
    }

    // 2. 检查时间门控
    if let Some(last_time) = state.last_consolidation_time {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let hours_since_last = (now.saturating_sub(last_time)) as f64 / 3600.0;

        if hours_since_last < config.time_gate_threshold_hours {
            return GateCheckResult::TimeGateFailed;
        }
    } else {
        // 从未整合过，时间门控通过
    }

    // 3. 检查会话门控
    let new_sessions = state
        .current_session_count
        .saturating_sub(state.last_session_count);
    if new_sessions < config.session_gate_threshold {
        return GateCheckResult::SessionGateFailed;
    }

    memory_event!(layer6, gate_passed, "all_gates");
    GateCheckResult::Passed
}

/// 梦境执行器
pub struct DreamConsolidator {
    /// 配置目录
    config_dir: PathBuf,
    /// 当前状态
    state: DreamState,
    /// 门控配置
    gate_config: ConsolidatorConfig,
    /// Provider 池（用于 Forked Agent LLM 调用）
    provider_pool: Option<Arc<blockcell_providers::ProviderPool>>,
    /// Ownership-bearing guard for `.dream_lock`.
    dream_lock: std::sync::Mutex<Option<ExclusiveFileLock>>,
}

/// 检查锁的有效性（独立函数，供 check_gates 和 acquire_lock 复用）
///
/// 返回 Ok(true) 表示锁仍有效（进程存活且未过期）
/// 返回 Ok(false) 表示锁已失效（进程已死或过期）
async fn check_lock_validity(lock_path: &Path) -> Result<bool, DreamError> {
    let content = fs::read_to_string(lock_path).await?;

    // 解析 PID:TIMESTAMP[:TOKEN]
    let parts: Vec<&str> = content.split(':').collect();
    if parts.len() < 2 {
        // 格式错误，锁无效
        return Ok(false);
    }

    // 检查时间戳是否过期
    let timestamp: u64 = parts[1].parse().unwrap_or(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let age_hours = (now - timestamp) / 3600;

    if age_hours >= TIME_GATE_THRESHOLD_HOURS {
        // 锁已过期
        tracing::debug!(age_hours, "[dream] Lock expired");
        return Ok(false);
    }

    // 检查持有锁的进程是否仍在运行
    let pid: u32 = parts[0].parse().unwrap_or(0);
    if pid == 0 {
        return Ok(false);
    }

    // 跨平台进程存活检查
    let process_alive = is_process_alive(pid);

    Ok(process_alive)
}

/// 检查进程是否存活（独立函数，供 check_lock_validity 复用）
#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    // Unix: 使用 kill(pid, 0) 检查进程是否存在
    // ESRCH 表示进程不存在
    // SAFETY: libc::kill(pid, 0) 是安全的 Unix 系统调用，仅查询而不修改任何进程状态。
    //         信号值 0 是特殊用途，不发送实际信号，仅检查进程是否存在。
    //         该调用在 pid 不存在时返回 -1 并设置 errno 为 ESRCH，不存在未定义行为风险。
    unsafe {
        let result = libc::kill(pid as i32, 0);
        result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}

/// 检查进程是否存活（独立函数，供 check_lock_validity 复用）
#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    // Windows: 尝试打开进程
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_INFORMATION,
    };

    // SAFETY: 以下 Windows API 调用的安全性说明：
    // - OpenProcess: 使用 PROCESS_QUERY_INFORMATION 权限打开已命名进程，
    //   传入来自锁文件的 pid，不修改任何进程状态。返回 null 表示进程不存在。
    // - GetExitCodeProcess: 从有效进程中读取退出码，不修改任何状态。
    // - CloseHandle: 关闭已打开的句柄，标准资源清理。
    // 所有调用均符合 Windows API 安全规范，不涉及内存安全风险。
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }

        let mut exit_code: u32 = 0;
        let result = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);

        // STILL_ACTIVE (259) 表示进程仍在运行
        //
        // 已知限制：如果进程恰好以退出码 259 结束，会被误判为仍在运行。
        // 这在现实中极其罕见，因为：
        // 1. 259 不是常见的错误码
        // 2. 大多数程序使用 0 表示成功，非零值表示错误
        // 3. 即使发生误判，锁也会在 TIME_GATE_THRESHOLD_HOURS 小时后过期
        //
        // 如果需要更精确的检测，可以使用 WaitForSingleObject 等待 0 毫秒，
        // 但那会增加代码复杂性。
        result != 0 && exit_code == 259
    }
}

/// 检查进程是否存活 (非 Unix 非 Windows 平台的保守实现)
#[cfg(not(any(unix, windows)))]
fn is_process_alive(_pid: u32) -> bool {
    // 保守策略：假设进程存活
    true
}

/// 梦境错误类型
#[derive(Debug, thiserror::Error)]
pub enum DreamError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Lock already acquired by another process")]
    LockAcquired,

    #[error("Consolidation failed: {0}")]
    ConsolidationFailed(String),

    #[error("No provider pool configured - call with_provider_pool() before dream()")]
    NoProviderPool,

    #[error("Consolidation timed out after {0}s")]
    Timeout(u64),

    #[error("Circuit breaker is open, dream consolidation blocked")]
    CircuitBreakerOpen,
}

/// 按 markdown 标题分割内容（支持 `# ` 和 `## `）
///
/// 返回 Vec，每个元素是一个 section（包含标题行和内容）。
/// 第一个标题之前的内容会被忽略，因为 session memory 模板以标题开头。
/// 维护 fenced code block 状态，避免误切 ``` 围栏内的 `#` 标题。
fn split_by_markdown_headings(content: &str) -> Vec<&str> {
    let mut sections = Vec::new();
    let mut section_start: Option<usize> = None;
    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut pos = 0;
    // 追踪 fenced code block 状态，避免误切围栏内的 `#` 标题
    let mut in_fenced_code = false;
    // 记录当前 fence 的长度（3 或更多反引号），只有匹配长度的 fence 才能关闭
    let mut fence_len: usize = 0;

    while pos < len {
        // pos 始终在行开头（或 pos=0）
        // 查找当前行的结束位置
        let line_end = bytes[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| pos + p)
            .unwrap_or(len);

        // 检查当前行是否是 fence 开关（``` 或 ~~~）
        let line_bytes = &bytes[pos..line_end];
        let line = std::str::from_utf8(line_bytes).unwrap_or("");
        let trimmed = line.trim();

        // 检测 fence 行：行首（允许最多 3 个空格缩进）由 >=3 个 ` 或 ~ 组成
        if !in_fenced_code {
            // 尝试匹配 fence 开始
            let fence_char = if trimmed.starts_with("```") {
                b'`'
            } else if trimmed.starts_with("~~~") {
                b'~'
            } else {
                0
            };
            if fence_char != 0 {
                let fl = trimmed.bytes().take_while(|&c| c == fence_char).count();
                // fence 后面必须跟空白或行尾才算有效 fence
                if fl >= 3
                    && trimmed
                        .as_bytes()
                        .get(fl)
                        .copied()
                        .is_none_or(|c| c.is_ascii_whitespace())
                {
                    in_fenced_code = true;
                    fence_len = fl;
                }
            }
        } else {
            // 尝试匹配 fence 结束：只有相同字符、相同或更长长度才能关闭
            let fence_char = if trimmed.starts_with(&"`".repeat(fence_len)) {
                b'`'
            } else if trimmed.starts_with(&"~".repeat(fence_len)) {
                b'~'
            } else {
                0
            };
            if fence_char != 0 {
                let fl = trimmed.bytes().take_while(|&c| c == fence_char).count();
                if fl >= fence_len
                    && trimmed
                        .as_bytes()
                        .get(fl)
                        .copied()
                        .is_none_or(|c| c.is_ascii_whitespace())
                {
                    in_fenced_code = false;
                    fence_len = 0;
                }
            }
        }

        // 检查当前行是否是标题（仅在非 fenced code block 内判断）
        if !in_fenced_code {
            let is_h1 = trimmed.starts_with("# ") && !trimmed.starts_with("## ");
            let is_h2 = trimmed.starts_with("## ") && !trimmed.starts_with("### ");

            if is_h1 || is_h2 {
                // 结束前一个 section
                if let Some(start) = section_start {
                    let end = if pos > 0 && bytes[pos - 1] == b'\n' {
                        pos - 1 // 去掉前导换行符
                    } else {
                        pos
                    };
                    if end > start {
                        sections.push(&content[start..end]);
                    }
                }
                section_start = Some(pos);
            }
        }

        // 移动到下一行
        pos = if line_end < len { line_end + 1 } else { len };
    }

    // 处理最后一个 section
    if let Some(start) = section_start {
        if start < len {
            let remaining = content[start..].trim_end();
            if !remaining.is_empty() {
                sections.push(&content[start..]);
            }
        }
    } else if !content.trim().is_empty() {
        // 没有找到任何标题，整个内容作为一个 section（向后兼容旧格式）
        sections.push(content);
    }

    sections
}

/// 检查 journal 的 started_at 字段是否已超过指定阈值
///
/// 当 owner 进程已死或无法判断时，使用 started_at 时间做最终过期判断。
/// 阈值建议使用 3x 正常 stale 阈值（与 agent 侧 is_journal_stale 一致）。
fn is_journal_started_at_expired(journal: &serde_json::Value, threshold_secs: u64) -> bool {
    let started_at_str = journal
        .get("started_at")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let started_at = match chrono::DateTime::parse_from_rfc3339(started_at_str) {
        Ok(dt) => dt,
        Err(_) => return true, // 无法解析视为过期
    };
    let elapsed = chrono::Utc::now()
        .signed_duration_since(started_at.with_timezone(&chrono::Utc))
        .num_seconds();
    match elapsed {
        e if e >= 0 => (e as u64) >= threshold_secs,
        _ => false, // 未来时间戳，不视为过期
    }
}

#[cfg(test)]
mod tests;
