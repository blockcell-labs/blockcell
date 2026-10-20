//! Unified write guard — replaces SkillMutex + per-store Mutex
//!
//! Provides a single concurrency protection mechanism for all learning-related
//! write operations: USER.md, MEMORY.md, and skill files.
//! Uses std::sync::RwLock so Drop can release synchronously without tokio runtime.

use async_trait::async_trait;
use blockcell_tools::{SkillMutexGuard, SkillMutexOps};
use std::collections::HashSet;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Unified write target — identifies what resource is being written
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum WriteTarget {
    /// USER.md file
    UserMd,
    /// MEMORY.md file
    MemoryMd,
    /// Skill directory (SKILL.md + meta.json).
    ///
    /// `name` is the canonical lock key: the skill's leaf directory name
    /// (last `/`-segment). Both the tool layer (which only sees the raw skill
    /// name) and the learning layer (which resolves a full `category/name`
    /// path) must key on the same value, so the identity is the leaf name
    /// rather than the full path. Construct via [`WriteTarget::skill`] to
    /// guarantee both sites normalize identically.
    Skill { name: String },
}

impl WriteTarget {
    /// Build a skill write target from a raw skill name or `category/name` path.
    ///
    /// Normalizes to the leaf segment so the tool layer and the learning layer
    /// produce the same lock key for the same skill.
    pub fn skill(raw: &str) -> Self {
        let name = raw
            .trim()
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        WriteTarget::Skill { name }
    }

    /// Human-readable label for logging
    pub fn label(&self) -> String {
        match self {
            WriteTarget::UserMd => "USER.md".to_string(),
            WriteTarget::MemoryMd => "MEMORY.md".to_string(),
            WriteTarget::Skill { name } => format!("skill/{}", name),
        }
    }

    /// Lockdir filename for cross-process locking
    pub fn lock_filename(&self) -> String {
        match self {
            WriteTarget::UserMd => ".user_md.lockdir".to_string(),
            WriteTarget::MemoryMd => ".memory_md.lockdir".to_string(),
            WriteTarget::Skill { name } => {
                format!(".skill_{}.lockdir", name)
            }
        }
    }
}

impl fmt::Display for WriteTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Acquire error — target is already being written
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Concurrent write in progress for {target}")]
pub struct WriteGuardError {
    pub target: WriteTarget,
}

/// Unified write guard — single concurrency point for all learning writes
///
/// Replaces:
/// - SkillMutex (skill-level RwLock<HashSet<String>>)
/// - MemoryFileStore write_lock (Arc<Mutex<()>>)
/// - SkillFileStore write_lock (Mutex<()>)
pub struct WriteGuard {
    /// In-process lock: tracks which targets are currently being written
    active_writes: Arc<RwLock<HashSet<WriteTarget>>>,
    /// 锁文件基础目录 — 预留用于跨进程文件锁
    /// 目前仅存储，未来版本将用于跨进程 WriteGuard 协调
    #[allow(dead_code)]
    lockdir_base: PathBuf,
}

impl fmt::Debug for WriteGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriteGuard").finish()
    }
}

impl WriteGuard {
    /// 创建新的 WriteGuard
    ///
    /// `lockdir_base` 为锁文件目录，目前存储用于未来跨进程锁实现。
    pub fn new(lockdir_base: PathBuf) -> Self {
        Self {
            active_writes: Arc::new(RwLock::new(HashSet::new())),
            lockdir_base,
        }
    }

    /// Try to acquire write access to a target (RAII guard)
    ///
    /// Returns `WriteGuardRAII` on success, or `WriteGuardError` if the target
    /// is already being written by another operation in this process.
    pub fn acquire(&self, target: WriteTarget) -> Result<WriteGuardRAII, WriteGuardError> {
        // In-process lock check
        {
            let mut active = self.active_writes.write().unwrap_or_else(|e| {
                tracing::warn!("WriteGuard RwLock poisoned, recovering for: {}", target);
                e.into_inner()
            });
            if active.contains(&target) {
                return Err(WriteGuardError { target });
            }
            active.insert(target.clone());
        }

        Ok(WriteGuardRAII {
            target,
            active_writes: Arc::clone(&self.active_writes),
        })
    }

    /// Check if a target is currently being written
    pub fn is_active(&self, target: &WriteTarget) -> bool {
        let active = self.active_writes.read().unwrap_or_else(|e| {
            tracing::warn!("WriteGuard RwLock poisoned during is_active check");
            e.into_inner()
        });
        active.contains(target)
    }

    /// Check if a target can be modified (not currently being written)
    pub fn can_modify(&self, target: &WriteTarget) -> bool {
        !self.is_active(target)
    }

    /// Get all currently active write targets
    pub fn active_targets(&self) -> Vec<WriteTarget> {
        let active = self.active_writes.read().unwrap_or_else(|e| {
            tracing::warn!("WriteGuard RwLock poisoned during active_targets check");
            e.into_inner()
        });
        active.iter().cloned().collect()
    }
}

impl Default for WriteGuard {
    fn default() -> Self {
        Self::new(PathBuf::new())
    }
}

/// 帮助函数: 将技能名称映射为 WriteTarget，用于 SkillMutexOps 实现
fn skill_name_to_target(skill_name: &str) -> WriteTarget {
    WriteTarget::skill(skill_name)
}

/// 为 WriteGuard 实现 SkillMutexOps trait
///
/// 使 WriteGuard 可以作为 `Arc<dyn SkillMutexOps>` (SkillMutexHandle) 传递给工具层，
/// 替代已废弃的 SkillMutex。
#[async_trait]
impl SkillMutexOps for WriteGuard {
    async fn can_modify(&self, skill_name: &str) -> bool {
        let target = skill_name_to_target(skill_name);
        WriteGuard::can_modify(self, &target)
    }

    fn try_acquire(&self, skill_name: &str) -> Option<SkillMutexGuard> {
        let target = skill_name_to_target(skill_name);
        match self.acquire(target) {
            Ok(guard) => Some(Arc::new(guard)),
            Err(_) => None,
        }
    }
}

/// RAII guard — releases the write lock on Drop
#[derive(Debug)]
pub struct WriteGuardRAII {
    target: WriteTarget,
    active_writes: Arc<RwLock<HashSet<WriteTarget>>>,
}

impl WriteGuardRAII {
    /// Get the target this guard is protecting
    pub fn target(&self) -> &WriteTarget {
        &self.target
    }
}

impl Drop for WriteGuardRAII {
    fn drop(&mut self) {
        // std::sync::RwLock write is synchronous — safe to call in Drop
        let mut active = self.active_writes.write().unwrap_or_else(|e| {
            tracing::warn!(
                "WriteGuard RwLock poisoned during RAII drop for '{}'",
                self.target
            );
            e.into_inner()
        });
        active.remove(&self.target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categorized_skill_write_blocks_tool_leaf_alias() {
        let guard = WriteGuard::default();
        let _held = guard.acquire(WriteTarget::skill("coding/debug")).unwrap();
        assert!(SkillMutexOps::try_acquire(&guard, "debug").is_none());
    }

    #[test]
    fn test_acquire_and_release() {
        let guard = WriteGuard::new(PathBuf::from("/tmp/test"));

        {
            let _lock = guard.acquire(WriteTarget::UserMd).unwrap();
            assert!(guard.is_active(&WriteTarget::UserMd));
            assert!(!guard.can_modify(&WriteTarget::UserMd));
        }

        // RAII drop releases immediately
        assert!(!guard.is_active(&WriteTarget::UserMd));
        assert!(guard.can_modify(&WriteTarget::UserMd));
    }

    #[test]
    fn test_acquire_duplicate_error() {
        let guard = WriteGuard::new(PathBuf::from("/tmp/test"));

        let _lock = guard.acquire(WriteTarget::UserMd).unwrap();
        let err = guard.acquire(WriteTarget::UserMd).unwrap_err();
        assert_eq!(err.target, WriteTarget::UserMd);
    }

    #[test]
    fn test_different_targets_independent() {
        let guard = WriteGuard::new(PathBuf::from("/tmp/test"));

        let lock1 = guard.acquire(WriteTarget::UserMd).unwrap();
        let lock2 = guard.acquire(WriteTarget::MemoryMd).unwrap();
        let lock3 = guard.acquire(WriteTarget::skill("coding/debug")).unwrap();

        assert!(guard.is_active(&WriteTarget::UserMd));
        assert!(guard.is_active(&WriteTarget::MemoryMd));
        assert!(guard.is_active(&WriteTarget::skill("coding/debug")));

        // Different skill is not blocked
        assert!(guard.can_modify(&WriteTarget::skill("coding/other")));

        drop(lock1);
        drop(lock2);
        drop(lock3);
    }

    #[test]
    fn test_skill_target_key_equality() {
        let t1 = WriteTarget::skill("coding/debug");
        let t2 = WriteTarget::skill("coding/debug");
        let t3 = WriteTarget::skill("coding/other");

        assert_eq!(t1, t2);
        assert_ne!(t1, t3);
    }

    #[test]
    fn test_skill_key_normalizes_to_leaf_name() {
        // Tool layer (raw leaf name) and learning layer (full category/name path)
        // must produce the same lock key for the same skill.
        assert_eq!(
            WriteTarget::skill("coding/debug"),
            WriteTarget::skill("debug")
        );
        assert_eq!(
            WriteTarget::skill("a/b/c/debug"),
            WriteTarget::skill("debug")
        );
        // Trailing slash and surrounding whitespace are normalized away.
        assert_eq!(
            WriteTarget::skill("coding/debug/"),
            WriteTarget::skill(" debug ")
        );
    }

    #[test]
    fn test_label_and_lock_filename() {
        assert_eq!(WriteTarget::UserMd.label(), "USER.md");
        assert_eq!(WriteTarget::MemoryMd.label(), "MEMORY.md");
        assert_eq!(WriteTarget::skill("coding/debug").label(), "skill/debug");
        assert_eq!(WriteTarget::UserMd.lock_filename(), ".user_md.lockdir");
        assert_eq!(WriteTarget::MemoryMd.lock_filename(), ".memory_md.lockdir");
        assert_eq!(
            WriteTarget::skill("coding/debug").lock_filename(),
            ".skill_debug.lockdir"
        );
    }
}
