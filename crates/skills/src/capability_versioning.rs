//! 能力版本管理 — 为 Capability artifacts 提供版本快照、回滚和清理
//!
//! 提供 per-capability 跨进程文件锁，确保 rollback/create_version/cleanup_old_versions
//! 对同一 capability 的操作互斥，避免并发导致版本 history 丢更新或 active artifact 不一致。
//! save_history 使用 temp + fsync + atomic rename，防止崩溃/半写导致 JSON 损坏。

use blockcell_core::{Error, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{debug, info, warn};

/// 能力版本信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityVersion {
    pub version: String,
    pub artifact_hash: String,
    pub created_at: i64,
    pub source: CapabilityVersionSource,
    pub changelog: Option<String>,
    pub artifact_path: String,
}

/// 版本来源
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CapabilityVersionSource {
    Evolution,
    Manual,
    HotReplace,
}

/// 能力版本历史
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityVersionHistory {
    pub capability_id: String,
    pub versions: Vec<CapabilityVersion>,
    pub current_version: String,
    /// 单调递增的下一版本号；旧历史文件缺该字段时按已有版本最大编号补算。
    #[serde(default)]
    pub next_version: u64,
}

impl CapabilityVersionHistory {
    pub fn normalized_next_version(&self) -> u64 {
        let max_existing = self
            .versions
            .iter()
            .filter_map(|v| v.version.strip_prefix('v')?.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        self.next_version.max(max_existing + 1).max(1)
    }
}

// === Per-capability 跨进程文件锁 ===

/// 获取锁的重试间隔（毫秒）
const CAP_LOCK_RETRY_INTERVAL_MS: u64 = 100;
/// 获取锁的最大重试次数
const CAP_LOCK_MAX_RETRIES: u32 = 50;

/// Per-capability 跨进程文件锁。
///
/// 使用 `tool_versions/<safe_id>.lock` 文件 + fs2/flock advisory lock 实现跨进程互斥：
/// 进程退出/崩溃时 OS 自动释放，无需 stale 检测与 PID 存活探测；
/// 锁随 fd 关闭（guard Drop）释放，不存在"读 token → 比对 → 删文件"的
/// check-then-act 释放窗口。锁文件内容仅作诊断信息（PID:timestamp），不承载锁语义。
struct CapabilityFileLock {
    lock_path: PathBuf,
}

impl CapabilityFileLock {
    fn new(versions_dir: &Path, capability_id: &str) -> Self {
        let safe_id = safe_capability_id(capability_id);
        let lock_path = versions_dir.join(format!("{}.lock", safe_id));
        Self { lock_path }
    }

    /// 带重试地获取 fs2 排他文件锁。锁文件长期保留（同 file_lock 的锚点设计），
    /// 不存在 stale 锁文件概念：无持有者的文件可立即获得锁。
    fn acquire_with_retry(&self) -> Option<CapabilityLockGuard> {
        self.acquire_with_retries(CAP_LOCK_MAX_RETRIES)
    }

    fn acquire_with_retries(&self, max_retries: u32) -> Option<CapabilityLockGuard> {
        if let Some(parent) = self.lock_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.lock_path)
            .ok()?;
        for _ in 0..max_retries {
            match file.try_lock_exclusive() {
                Ok(()) => {
                    let owner = format!(
                        "{}:{}",
                        std::process::id(),
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0)
                    );
                    let _ = file.set_len(0);
                    let _ = (&file).write_all(owner.as_bytes());
                    return Some(CapabilityLockGuard { _file: file });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(CAP_LOCK_RETRY_INTERVAL_MS));
                }
                Err(_) => return None,
            }
        }
        warn!(
            lock_path = %self.lock_path.display(),
            "[能力版本] 获取跨进程锁重试耗尽"
        );
        None
    }
}

/// 锁的 RAII guard：fd 随 Drop 关闭即释放 OS 锁。
pub struct CapabilityLockGuard {
    _file: fs::File,
}

/// 使用 per-capability 锁执行操作
///
/// 获取 `tool_versions/<safe_id>.lock` 文件锁后执行闭包，
/// 确保同一 capability 的 rollback/create_version/cleanup 互斥。
/// 获取锁后首先检查并恢复未完成的 rollback journal，保证崩溃安全。
fn with_capability_lock<F, R>(versions_dir: &Path, capability_id: &str, f: F) -> Result<R>
where
    F: FnOnce() -> Result<R>,
{
    // This module's path safety historically relied on the caller
    // (core_evolution) validating the id. Enforce it at the entry point so a
    // future caller cannot inject path separators into lock/history paths.
    validate_capability_id(capability_id)?;
    let lock = CapabilityFileLock::new(versions_dir, capability_id);
    let guard = lock.acquire_with_retry();
    if guard.is_none() {
        return Err(Error::Other(format!(
            "获取 capability '{}' 的跨进程锁失败（重试 {} 次），可能另一进程正在操作",
            capability_id, CAP_LOCK_MAX_RETRIES
        )));
    }
    // 获取锁后，先恢复可能残留的 rollback journal（崩溃恢复）
    recover_rollback_journal(versions_dir, capability_id);
    let result = f();
    drop(guard);
    result
}

/// 将 capability_id 转换为文件系统安全名称
fn safe_capability_id(capability_id: &str) -> String {
    capability_id.replace('.', "_")
}

/// Validate a capability id before it is joined into any path.
fn validate_capability_id(capability_id: &str) -> Result<()> {
    if capability_id.is_empty()
        || capability_id.len() > 128
        || !capability_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        return Err(Error::Other(format!(
            "invalid capability id: '{}'",
            capability_id
        )));
    }
    Ok(())
}

/// 恢复未完成的 rollback journal（崩溃安全机制）
///
/// rollback 操作在修改文件前会写入 journal 文件（`.rollback_journal`），
/// 记录 active artifact 备份路径和目标路径。如果进程在 rollback 过程中崩溃，
/// 下次获取锁时会调用此函数恢复 journal：
/// - 如果 active artifact 不存在或损坏，从备份恢复
/// - 如果 active artifact 存在，说明 rollback 的 artifact 替换已完成但 history 未保存，
///   此时保留 active（回滚已生效），仅清理 journal
fn recover_rollback_journal(versions_dir: &Path, capability_id: &str) {
    let safe_id = safe_capability_id(capability_id);
    let journal_path = versions_dir.join(format!("{}.rollback_journal", safe_id));

    if !journal_path.exists() {
        return;
    }

    let content = match fs::read_to_string(&journal_path) {
        Ok(c) => c,
        Err(_) => {
            // journal 文件无法读取，尝试删除
            let _ = fs::remove_file(&journal_path);
            return;
        }
    };

    // journal 格式：第一行 active_backup 路径，第二行 active_path
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() < 2 {
        warn!(
            journal_path = %journal_path.display(),
            "[能力版本] rollback journal 格式无效，删除"
        );
        let _ = fs::remove_file(&journal_path);
        return;
    }

    let active_backup = PathBuf::from(lines[0]);
    let active_path = PathBuf::from(lines[1]);

    warn!(
        capability_id = %capability_id,
        journal_path = %journal_path.display(),
        active_backup = %active_backup.display(),
        active_path = %active_path.display(),
        "[能力版本] 发现未完成的 rollback journal，开始恢复"
    );

    // 恢复策略：
    // - 如果 active artifact 不存在，说明 rollback 在删除 active 后、复制新 active 前崩溃
    //   此时从备份恢复 active（回滚未生效，恢复到回滚前状态）
    // - 如果 active artifact 存在，说明 artifact 替换已完成
    //   可能是 history 保存前崩溃，此时保留 active（回滚已生效）
    if !active_path.exists() && active_backup.exists() {
        // active 缺失，从备份恢复
        warn!(
            active_path = %active_path.display(),
            active_backup = %active_backup.display(),
            "[能力版本] active artifact 缺失，从备份恢复"
        );
        if let Err(e) = fs::copy(&active_backup, &active_path) {
            warn!(error = %e, "[能力版本] 从备份恢复 active artifact 失败");
        }
    }

    // 清理备份文件
    if active_backup.exists() {
        let _ = fs::remove_file(&active_backup);
    }

    // 清理 journal 文件
    let _ = fs::remove_file(&journal_path);

    info!(
        capability_id = %capability_id,
        "[能力版本] rollback journal 恢复完成"
    );
}

/// 能力版本管理器 — 为 Capability artifacts 提供版本快照和回滚
pub struct CapabilityVersionManager {
    /// Base directory for capability artifacts
    artifacts_dir: PathBuf,
    /// Directory for version snapshots
    versions_dir: PathBuf,
}

impl CapabilityVersionManager {
    pub fn new(workspace_dir: PathBuf) -> Self {
        let artifacts_dir = workspace_dir.join("tool_artifacts");
        let versions_dir = workspace_dir.join("tool_versions");
        Self {
            artifacts_dir,
            versions_dir,
        }
    }

    /// Create a version snapshot for a capability artifact.
    /// Copies the current artifact to the versions directory.
    /// 使用 per-capability 锁保护整个操作区间。
    pub fn create_version(
        &self,
        capability_id: &str,
        artifact_path: &str,
        source: CapabilityVersionSource,
        changelog: Option<String>,
    ) -> Result<CapabilityVersion> {
        with_capability_lock(&self.versions_dir, capability_id, || {
            self.create_version_unlocked(capability_id, artifact_path, source, changelog)
        })
    }

    /// create_version 的内部实现（无锁，由 with_capability_lock 调用）
    fn create_version_unlocked(
        &self,
        capability_id: &str,
        artifact_path: &str,
        source: CapabilityVersionSource,
        changelog: Option<String>,
    ) -> Result<CapabilityVersion> {
        let safe_id = safe_capability_id(capability_id);
        let cap_versions_dir = self.versions_dir.join(&safe_id);
        fs::create_dir_all(&cap_versions_dir)?;

        // Load or create history
        let mut history = self.get_history_unlocked(capability_id)?;

        // Calculate version number (monotonic across cleanup)
        let version_num = history.normalized_next_version();
        let version = format!("v{}", version_num);
        history.next_version = version_num + 1;

        // Calculate artifact hash
        let artifact_content = fs::read(artifact_path)
            .map_err(|e| Error::Other(format!("Failed to read artifact: {}", e)))?;
        let hash = simple_hash(&artifact_content);

        // Copy artifact to version snapshot
        let ext = Path::new(artifact_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("sh");
        let snapshot_path = cap_versions_dir.join(format!("{}_{}.{}", safe_id, version, ext));
        fs::copy(artifact_path, &snapshot_path)?;

        let cap_version = CapabilityVersion {
            version: version.clone(),
            artifact_hash: hash,
            created_at: chrono::Utc::now().timestamp(),
            source,
            changelog,
            artifact_path: snapshot_path.to_string_lossy().to_string(),
        };

        history.versions.push(cap_version.clone());
        history.current_version = version.clone();
        self.save_history(&history)?;

        info!(
            capability_id = %capability_id,
            version = %version,
            "📦 [能力版本] 创建版本快照: {} -> {}",
            capability_id, version
        );

        Ok(cap_version)
    }

    /// Create a version snapshot unless an identical artifact hash already exists.
    ///
    /// Used by durable workflows when a promotion step may be replayed after a
    /// crash between the external side effect and the step checkpoint.
    /// 使用 per-capability 锁保护整个操作区间。
    pub fn create_version_if_new_artifact(
        &self,
        capability_id: &str,
        artifact_path: &str,
        source: CapabilityVersionSource,
        changelog: Option<String>,
    ) -> Result<CapabilityVersion> {
        with_capability_lock(&self.versions_dir, capability_id, || {
            self.create_version_if_new_artifact_unlocked(
                capability_id,
                artifact_path,
                source,
                changelog,
            )
        })
    }

    /// create_version_if_new_artifact 的内部实现（无锁）
    fn create_version_if_new_artifact_unlocked(
        &self,
        capability_id: &str,
        artifact_path: &str,
        source: CapabilityVersionSource,
        changelog: Option<String>,
    ) -> Result<CapabilityVersion> {
        let artifact_content = fs::read(artifact_path)
            .map_err(|e| Error::Other(format!("Failed to read artifact: {}", e)))?;
        let hash = simple_hash(&artifact_content);

        let mut history = self.get_history_unlocked(capability_id)?;
        if let Some(existing) = history
            .versions
            .iter()
            .rev()
            .find(|version| version.artifact_hash == hash)
            .cloned()
        {
            if history.current_version != existing.version {
                history.current_version = existing.version.clone();
                self.save_history(&history)?;
            }
            info!(
                capability_id = %capability_id,
                version = %existing.version,
                "📝 [能力版本] 复用已有 artifact 版本快照: {} -> {}",
                capability_id, existing.version
            );
            return Ok(existing);
        }

        self.create_version_unlocked(capability_id, artifact_path, source, changelog)
    }

    /// 回滚到当前版本的前一个版本。返回恢复后的 artifact 路径。
    /// 非破坏性回滚：保留所有版本历史，支持 roll-forward。
    /// 基于 current_version 定位，而非总是取倒数第二个版本。
    /// 使用 per-capability 锁保护整个操作区间（get_history -> artifact replace -> save_history）。
    pub fn rollback(&self, capability_id: &str) -> Result<Option<String>> {
        with_capability_lock(&self.versions_dir, capability_id, || {
            self.rollback_unlocked(capability_id)
        })
    }

    /// rollback 的内部实现（无锁，由 with_capability_lock 调用）
    ///
    /// 崩溃安全策略：使用 rollback journal 保护整个 rollback 过程。
    /// 1. 备份 active artifact
    /// 2. 写入 rollback journal（记录备份路径和目标路径）
    /// 3. 删除 active artifact
    /// 4. 复制新 active artifact
    /// 5. 保存 history
    /// 6. 删除 journal 和备份
    ///
    /// 如果进程在步骤 2-5 之间崩溃，下次获取锁时 recover_rollback_journal 会恢复：
    /// - active 缺失 → 从备份恢复（回滚未生效，恢复到回滚前状态）
    /// - active 存在 → 保留 active（回滚已生效），仅清理 journal
    fn rollback_unlocked(&self, capability_id: &str) -> Result<Option<String>> {
        let mut history = self.get_history_unlocked(capability_id)?;

        if history.versions.is_empty() {
            warn!(
                capability_id = %capability_id,
                "📦 [能力版本] 没有可回滚的版本: {}",
                capability_id
            );
            return Ok(None);
        }

        // 基于 current_version 找到当前版本在列表中的位置，再切到前一个版本
        let current_idx = history
            .versions
            .iter()
            .position(|v| v.version == history.current_version);

        let prev_idx = match current_idx {
            Some(0) => {
                // 当前已是第一个版本，无法继续回滚
                warn!(
                    capability_id = %capability_id,
                    current_version = %history.current_version,
                    "📦 [能力版本] 当前已是第一个版本，无法回滚: {}",
                    capability_id
                );
                return Ok(None);
            }
            Some(idx) => idx - 1,
            None => {
                // current_version 在列表中未找到，回退到倒数第二个版本
                if history.versions.len() < 2 {
                    return Ok(None);
                }
                history.versions.len() - 2
            }
        };

        let prev = &history.versions[prev_idx];
        history.current_version = prev.version.clone();
        let restore_path = prev.artifact_path.clone();

        let safe_id = safe_capability_id(capability_id);
        let ext = Path::new(&restore_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("sh");
        let active_path = self.artifacts_dir.join(format!("{}.{}", safe_id, ext));

        // 验证快照文件存在并可读
        if !Path::new(&restore_path).exists() {
            warn!(
                capability_id = %capability_id,
                restore_path = %restore_path,
                "📦 [能力版本] 回滚快照文件不存在: {}",
                restore_path
            );
            return Ok(None);
        }

        // 先复制 artifact 到同目录临时文件，fsync 后再原子替换，
        // 防止磁盘满/权限/中途失败时 active artifact 被截断或部分覆盖
        // 使用 UUID 避免同进程内并发 rollback 共享路径导致互相覆盖
        let rollback_id = uuid::Uuid::new_v4();
        let tmp_path = self.artifacts_dir.join(format!(
            "{}.rollback_tmp.{}.{}.{}",
            safe_id,
            std::process::id(),
            rollback_id,
            ext
        ));
        if let Err(e) = fs::copy(&restore_path, &tmp_path) {
            return Err(Error::Other(format!(
                "rollback: 复制快照到临时文件失败: {e}"
            )));
        }

        // 确保临时文件数据落盘后再替换
        {
            if let Ok(file) = fs::File::open(&tmp_path) {
                let _ = file.sync_all();
            }
        }

        // 备份当前 active artifact（若存在）
        let active_backup = self.artifacts_dir.join(format!(
            "{}.rollback_bak.{}.{}.{}",
            safe_id,
            std::process::id(),
            rollback_id,
            ext
        ));

        let had_active = active_path.exists();
        if had_active {
            fs::copy(&active_path, &active_backup).map_err(|e| {
                let _ = fs::remove_file(&tmp_path);
                Error::Other(format!("rollback: 备份 active artifact 失败: {e}"))
            })?;
        }

        // 写入 rollback journal（崩溃恢复用）
        // journal 格式：第一行 active_backup 路径，第二行 active_path 路径
        let journal_path = self
            .versions_dir
            .join(format!("{}.rollback_journal", safe_id));
        let journal_content = format!(
            "{}\n{}",
            active_backup.to_string_lossy(),
            active_path.to_string_lossy()
        );
        {
            let mut journal_file = fs::File::create(&journal_path).map_err(|e| {
                let _ = fs::remove_file(&tmp_path);
                if had_active {
                    let _ = fs::remove_file(&active_backup);
                }
                Error::Other(format!("rollback: 创建 rollback journal 失败: {e}"))
            })?;
            journal_file
                .write_all(journal_content.as_bytes())
                .map_err(|e| {
                    let _ = fs::remove_file(&tmp_path);
                    if had_active {
                        let _ = fs::remove_file(&active_backup);
                    }
                    let _ = fs::remove_file(&journal_path);
                    Error::Other(format!("rollback: 写入 rollback journal 失败: {e}"))
                })?;
            let _ = journal_file.sync_all();
        }

        // 删除当前 active artifact
        if had_active {
            let _ = fs::remove_file(&active_path);
        }

        // 将临时文件复制为 active artifact
        fs::copy(&tmp_path, &active_path).map_err(|e| {
            // 替换失败：恢复 active 备份，清理临时文件和 journal
            if had_active && active_backup.exists() {
                let _ = fs::copy(&active_backup, &active_path);
                let _ = fs::remove_file(&active_backup);
            }
            let _ = fs::remove_file(&tmp_path);
            let _ = fs::remove_file(&journal_path);
            Error::Other(format!("rollback: 替换 active artifact 失败: {e}"))
        })?;
        let _ = fs::remove_file(&tmp_path);

        // artifact 替换成功，保存 history
        if let Err(e) = self.save_history(&history) {
            // history 保存失败：恢复 active 备份以保持一致性
            if had_active && active_backup.exists() {
                let _ = fs::remove_file(&active_path);
                let _ = fs::copy(&active_backup, &active_path);
                let _ = fs::remove_file(&active_backup);
            } else if !had_active {
                let _ = fs::remove_file(&active_path);
            }
            let _ = fs::remove_file(&journal_path);
            return Err(e);
        }

        // 一切成功，清理 journal 和备份文件
        let _ = fs::remove_file(&journal_path);
        if active_backup.exists() {
            let _ = fs::remove_file(&active_backup);
        }

        info!(
            capability_id = %capability_id,
            version = %history.current_version,
            "📦 [能力版本] 回滚到: {} -> {}",
            capability_id, history.current_version
        );

        Ok(Some(active_path.to_string_lossy().to_string()))
    }

    /// List all versions for a capability.
    /// 使用 per-capability 锁保护读取，防止 save_history 写入窗口期读到空 history。
    pub fn list_versions(&self, capability_id: &str) -> Result<Vec<CapabilityVersion>> {
        with_capability_lock(&self.versions_dir, capability_id, || {
            let history = self.get_history_unlocked(capability_id)?;
            Ok(history.versions)
        })
    }

    /// Get the current version string for a capability.
    /// 使用 per-capability 锁保护读取，防止 save_history 写入窗口期读到空 history。
    pub fn get_current_version(&self, capability_id: &str) -> Result<String> {
        with_capability_lock(&self.versions_dir, capability_id, || {
            let history = self.get_history_unlocked(capability_id)?;
            Ok(history.current_version)
        })
    }

    /// Cleanup old versions, keeping only the most recent `keep_count`.
    /// 使用 per-capability 锁保护整个操作区间。
    pub fn cleanup_old_versions(&self, capability_id: &str, keep_count: usize) -> Result<usize> {
        with_capability_lock(&self.versions_dir, capability_id, || {
            self.cleanup_old_versions_unlocked(capability_id, keep_count)
        })
    }

    /// cleanup_old_versions 的内部实现（无锁，由 with_capability_lock 调用）
    fn cleanup_old_versions_unlocked(
        &self,
        capability_id: &str,
        keep_count: usize,
    ) -> Result<usize> {
        let mut history = self.get_history_unlocked(capability_id)?;

        if history.versions.len() <= keep_count {
            return Ok(0);
        }

        let remove_count = history.versions.len() - keep_count;
        let removed: Vec<CapabilityVersion> = history.versions.drain(..remove_count).collect();

        for v in &removed {
            let _ = fs::remove_file(&v.artifact_path);
        }

        self.save_history(&history)?;

        debug!(
            capability_id = %capability_id,
            removed = remove_count,
            "📦 [能力版本] 清理旧版本: {} 个",
            remove_count
        );

        Ok(remove_count)
    }

    // === Internal helpers ===

    /// 读取版本历史（无锁版本，由 with_capability_lock 调用）
    ///
    /// 包含崩溃恢复逻辑：
    /// 1. 如果主文件不存在但备份文件存在，说明 save_history 的 rename 步骤未完成，恢复备份。
    /// 2. 如果主文件存在但 JSON 解析失败（半写损坏），且备份文件存在，恢复备份。
    fn get_history_unlocked(&self, capability_id: &str) -> Result<CapabilityVersionHistory> {
        let history_file = self.history_file_path(capability_id);
        let bak_path = history_file.with_extension("json.bak");

        if !history_file.exists() {
            // 主文件不存在，检查是否有 save_history 的备份文件可恢复
            if bak_path.exists() {
                // 备份存在但主文件不存在，说明 save_history 的复制步骤崩溃中断
                // 恢复备份到主文件
                warn!(
                    history_file = %history_file.display(),
                    bak_path = %bak_path.display(),
                    "[能力版本] 主文件不存在但发现备份，恢复备份"
                );
                if let Err(e) = fs::copy(&bak_path, &history_file) {
                    warn!(error = %e, "[能力版本] 恢复备份失败，使用空 history");
                    return Ok(CapabilityVersionHistory {
                        capability_id: capability_id.to_string(),
                        versions: vec![],
                        current_version: "v0".to_string(),
                        next_version: 1,
                    });
                }
                // 恢复成功，继续读取主文件
            } else {
                return Ok(CapabilityVersionHistory {
                    capability_id: capability_id.to_string(),
                    versions: vec![],
                    current_version: "v0".to_string(),
                    next_version: 1,
                });
            }
        }

        // 主文件存在，尝试解析
        let content = fs::read_to_string(&history_file)?;
        match serde_json::from_str(&content) {
            Ok(history) => Ok(history),
            Err(parse_err) => {
                // 主文件存在但 JSON 解析失败（可能是崩溃导致半写损坏）
                // 如果备份文件存在，恢复备份
                if bak_path.exists() {
                    warn!(
                        history_file = %history_file.display(),
                        bak_path = %bak_path.display(),
                        parse_error = %parse_err,
                        "[能力版本] 主文件 JSON 解析失败（可能半写损坏），恢复备份"
                    );
                    // 用备份覆盖损坏的主文件
                    if let Err(e) = fs::copy(&bak_path, &history_file) {
                        warn!(error = %e, "[能力版本] 恢复备份失败，使用空 history");
                        return Ok(CapabilityVersionHistory {
                            capability_id: capability_id.to_string(),
                            versions: vec![],
                            current_version: "v0".to_string(),
                            next_version: 1,
                        });
                    }
                    // 恢复成功，重新读取并解析
                    let restored_content = fs::read_to_string(&history_file)?;
                    let history: CapabilityVersionHistory =
                        serde_json::from_str(&restored_content)?;
                    Ok(history)
                } else {
                    // 没有备份可恢复，返回解析错误
                    Err(Error::Other(format!(
                        "解析版本历史 JSON 失败（文件可能半写损坏）: {}",
                        parse_err
                    )))
                }
            }
        }
    }

    /// 保存版本历史到磁盘。
    /// 使用 backup-based 策略：先写入临时文件并 fsync，再备份目标文件，
    /// 最后将临时文件内容复制到目标。不先删除目标文件，避免崩溃窗口丢 history。
    ///
    /// Windows 兼容：始终使用 copy + delete 替代 rename。
    /// Windows 上 fs::rename 在目标已存在或文件被占用时返回 PermissionDenied，
    /// 而 fs::copy 可以覆盖已存在目标（只要没有独占锁）。
    fn save_history(&self, history: &CapabilityVersionHistory) -> Result<()> {
        let history_file = self.history_file_path(&history.capability_id);
        if let Some(parent) = history_file.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(history)?;

        // 1. 写入临时文件并 fsync，确保数据落盘
        let tmp_path = history_file.with_extension("json.tmp");
        let _ = fs::remove_file(&tmp_path);
        {
            let mut file = fs::File::create(&tmp_path)?;
            file.write_all(content.as_bytes())?;
            let _ = file.sync_all();
        }

        // 2. backup-based 替换策略（始终使用 copy，避免 Windows rename PermissionDenied）
        //    策略：备份当前目标 -> 复制临时文件到目标 -> 删除备份
        //    崩溃恢复：如果目标不存在但备份存在，说明复制步骤未完成，恢复备份
        let bak_path = history_file.with_extension("json.bak");
        if history_file.exists() {
            // 清理可能残留的旧备份
            let _ = fs::remove_file(&bak_path);
            // 备份当前目标文件（用于崩溃恢复）
            fs::copy(&history_file, &bak_path).map_err(|e| {
                let _ = fs::remove_file(&tmp_path);
                Error::Other(format!("save_history: 备份目标文件失败: {e}"))
            })?;
        }

        // 将临时文件内容复制到目标（覆盖已存在目标）
        fs::copy(&tmp_path, &history_file).map_err(|e| {
            // 复制失败，尝试恢复备份
            if bak_path.exists() {
                let _ = fs::copy(&bak_path, &history_file);
                let _ = fs::remove_file(&bak_path);
            }
            let _ = fs::remove_file(&tmp_path);
            Error::Other(format!("save_history: 复制临时文件到目标失败: {e}"))
        })?;

        // 成功，清理临时文件和备份
        let _ = fs::remove_file(&tmp_path);
        let _ = fs::remove_file(&bak_path);

        Ok(())
    }

    fn history_file_path(&self, capability_id: &str) -> PathBuf {
        let safe_id = safe_capability_id(capability_id);
        self.versions_dir.join(format!("{}_history.json", safe_id))
    }
}

/// Simple hash function (FNV-1a style) for artifact content.
fn simple_hash(data: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p3_invalid_capability_id_is_rejected_at_entry() {
        let root = std::env::temp_dir().join(format!("p3-cap-invalid-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let artifact = root.join("test.sh");
        std::fs::write(&artifact, "#!/bin/sh\necho test").unwrap();
        let manager = CapabilityVersionManager::new(root.clone());
        for id in ["", "../escape", "a/b", "a\\b"] {
            assert!(
                manager
                    .list_versions(id)
                    .is_err(),
                "capability id {id:?} must be rejected"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn p3_capability_versions_remain_monotonic_after_cleanup() {
        let root = std::env::temp_dir().join(format!("p3-capability-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let artifact = root.join("test.sh");
        std::fs::write(&artifact, "#!/bin/sh\necho test").unwrap();
        let manager = CapabilityVersionManager::new(root.clone());
        for _ in 0..3 {
            manager.create_version("test.cap", artifact.to_str().unwrap(), CapabilityVersionSource::Evolution, None).unwrap();
        }
        manager.cleanup_old_versions("test.cap", 1).unwrap();
        assert_eq!(manager.create_version("test.cap", artifact.to_str().unwrap(), CapabilityVersionSource::Evolution, None).unwrap().version, "v4");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn test_capability_version_create_and_list() {
        let tmp = std::env::temp_dir().join("test_cap_ver_create");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let vm = CapabilityVersionManager::new(tmp.clone());

        // Create a fake artifact
        let artifacts_dir = tmp.join("tool_artifacts");
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        let artifact = artifacts_dir.join("test_cap.sh");
        std::fs::write(&artifact, "#!/bin/bash\necho ok").unwrap();

        let v1 = vm
            .create_version(
                "test.cap",
                artifact.to_str().unwrap(),
                CapabilityVersionSource::Evolution,
                Some("Initial version".to_string()),
            )
            .unwrap();

        assert_eq!(v1.version, "v1");

        // Create v2
        std::fs::write(&artifact, "#!/bin/bash\necho ok v2").unwrap();
        let v2 = vm
            .create_version(
                "test.cap",
                artifact.to_str().unwrap(),
                CapabilityVersionSource::HotReplace,
                None,
            )
            .unwrap();
        assert_eq!(v2.version, "v2");

        let versions = vm.list_versions("test.cap").unwrap();
        assert_eq!(versions.len(), 2);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_create_version_if_new_artifact_reuses_existing_hash() {
        let tmp = std::env::temp_dir().join("test_cap_ver_reuse_hash");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let vm = CapabilityVersionManager::new(tmp.clone());
        let artifacts_dir = tmp.join("tool_artifacts");
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        let artifact = artifacts_dir.join("reuse_cap.sh");
        std::fs::write(&artifact, "#!/bin/bash\necho same").unwrap();

        let v1 = vm
            .create_version_if_new_artifact(
                "reuse.cap",
                artifact.to_str().unwrap(),
                CapabilityVersionSource::Evolution,
                Some("first".to_string()),
            )
            .unwrap();
        let v1_again = vm
            .create_version_if_new_artifact(
                "reuse.cap",
                artifact.to_str().unwrap(),
                CapabilityVersionSource::Evolution,
                Some("replay".to_string()),
            )
            .unwrap();

        assert_eq!(v1.version, "v1");
        assert_eq!(v1_again.version, "v1");
        let versions = vm.list_versions("reuse.cap").unwrap();
        assert_eq!(versions.len(), 1);

        std::fs::write(&artifact, "#!/bin/bash\necho changed").unwrap();
        let v2 = vm
            .create_version_if_new_artifact(
                "reuse.cap",
                artifact.to_str().unwrap(),
                CapabilityVersionSource::Evolution,
                Some("changed".to_string()),
            )
            .unwrap();
        assert_eq!(v2.version, "v2");

        let versions = vm.list_versions("reuse.cap").unwrap();
        assert_eq!(versions.len(), 2);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_capability_version_rollback() {
        let tmp = std::env::temp_dir().join("test_cap_ver_rollback");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let vm = CapabilityVersionManager::new(tmp.clone());

        let artifacts_dir = tmp.join("tool_artifacts");
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        let artifact = artifacts_dir.join("rollback_cap.sh");

        // v1
        std::fs::write(&artifact, "#!/bin/bash\necho v1").unwrap();
        vm.create_version(
            "rollback.cap",
            artifact.to_str().unwrap(),
            CapabilityVersionSource::Evolution,
            None,
        )
        .unwrap();

        // v2
        std::fs::write(&artifact, "#!/bin/bash\necho v2").unwrap();
        vm.create_version(
            "rollback.cap",
            artifact.to_str().unwrap(),
            CapabilityVersionSource::Evolution,
            None,
        )
        .unwrap();

        // Rollback (non-destructive: all versions preserved)
        let restored = vm.rollback("rollback.cap").unwrap();
        assert!(restored.is_some());

        let current = vm.get_current_version("rollback.cap").unwrap();
        assert_eq!(current, "v1");

        // All versions still exist after rollback (non-destructive)
        let versions = vm.list_versions("rollback.cap").unwrap();
        assert_eq!(versions.len(), 2);

        // Can roll-forward back to v2
        let history_file = tmp.join("tool_versions").join("rollback_cap_history.json");
        let content = std::fs::read_to_string(&history_file).unwrap();
        let mut history: CapabilityVersionHistory = serde_json::from_str(&content).unwrap();
        history.current_version = "v2".to_string();
        std::fs::write(
            &history_file,
            serde_json::to_string_pretty(&history).unwrap(),
        )
        .unwrap();

        let current_after_forward = vm.get_current_version("rollback.cap").unwrap();
        assert_eq!(current_after_forward, "v2");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_capability_version_cleanup() {
        let tmp = std::env::temp_dir().join("test_cap_ver_cleanup");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let vm = CapabilityVersionManager::new(tmp.clone());

        let artifacts_dir = tmp.join("tool_artifacts");
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        let artifact = artifacts_dir.join("cleanup_cap.sh");

        for i in 1..=5 {
            std::fs::write(&artifact, format!("#!/bin/bash\necho v{}", i)).unwrap();
            vm.create_version(
                "cleanup.cap",
                artifact.to_str().unwrap(),
                CapabilityVersionSource::Evolution,
                None,
            )
            .unwrap();
        }

        let removed = vm.cleanup_old_versions("cleanup.cap", 2).unwrap();
        assert_eq!(removed, 3);

        let versions = vm.list_versions("cleanup.cap").unwrap();
        assert_eq!(versions.len(), 2);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_simple_hash() {
        let h1 = simple_hash(b"hello");
        let h2 = simple_hash(b"hello");
        let h3 = simple_hash(b"world");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }

    /// 回归测试：基于 current_version 定位的连续回滚 v3 -> v2 -> v1
    #[test]
    fn test_sequential_rollback_v3_to_v2_to_v1() {
        let tmp = std::env::temp_dir().join("test_cap_ver_seq_rollback");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let vm = CapabilityVersionManager::new(tmp.clone());
        let artifacts_dir = tmp.join("tool_artifacts");
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        let artifact = artifacts_dir.join("seq_rollback_cap.sh");

        // 创建 v1
        std::fs::write(&artifact, "#!/bin/bash\necho v1").unwrap();
        vm.create_version(
            "seq.rollback",
            artifact.to_str().unwrap(),
            CapabilityVersionSource::Evolution,
            None,
        )
        .unwrap();
        assert_eq!(vm.get_current_version("seq.rollback").unwrap(), "v1");

        // 创建 v2
        std::fs::write(&artifact, "#!/bin/bash\necho v2").unwrap();
        vm.create_version(
            "seq.rollback",
            artifact.to_str().unwrap(),
            CapabilityVersionSource::Evolution,
            None,
        )
        .unwrap();
        assert_eq!(vm.get_current_version("seq.rollback").unwrap(), "v2");

        // 创建 v3
        std::fs::write(&artifact, "#!/bin/bash\necho v3").unwrap();
        vm.create_version(
            "seq.rollback",
            artifact.to_str().unwrap(),
            CapabilityVersionSource::Evolution,
            None,
        )
        .unwrap();
        assert_eq!(vm.get_current_version("seq.rollback").unwrap(), "v3");

        // 第一次回滚：v3 -> v2
        vm.rollback("seq.rollback").unwrap();
        assert_eq!(vm.get_current_version("seq.rollback").unwrap(), "v2");

        // 第二次回滚：v2 -> v1
        vm.rollback("seq.rollback").unwrap();
        assert_eq!(vm.get_current_version("seq.rollback").unwrap(), "v1");

        // 第三次回滚：v1 已是第一个版本，无法继续
        let result = vm.rollback("seq.rollback").unwrap();
        assert!(result.is_none());
        assert_eq!(vm.get_current_version("seq.rollback").unwrap(), "v1");

        // 所有版本仍然存在（非破坏性）
        let versions = vm.list_versions("seq.rollback").unwrap();
        assert_eq!(versions.len(), 3);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn capability_lock_held_guard_blocks_second_acquire_until_drop() {
        let tmp = std::env::temp_dir().join(format!(
            "test_cap_lock_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let first = CapabilityFileLock::new(&tmp, "cap.a")
            .acquire_with_retries(1)
            .expect("first acquire succeeds");
        // 同一 capability 的第二个锁（即使由另一实例发起）在持有期间不可获取
        assert!(
            CapabilityFileLock::new(&tmp, "cap.a")
                .acquire_with_retries(2)
                .is_none(),
            "held lock must block a second acquire"
        );
        drop(first);
        assert!(
            CapabilityFileLock::new(&tmp, "cap.a")
                .acquire_with_retries(2)
                .is_some(),
            "lock must be released when the guard is dropped"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn capability_lock_different_ids_are_independent() {
        let tmp = std::env::temp_dir().join(format!(
            "test_cap_lock_indep_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let _guard = CapabilityFileLock::new(&tmp, "cap.a")
            .acquire_with_retries(1)
            .expect("first acquire succeeds");
        assert!(
            CapabilityFileLock::new(&tmp, "cap.b")
                .acquire_with_retries(1)
                .is_some(),
            "a different capability id must not be blocked"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
