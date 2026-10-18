use blockcell_core::{Error, Result};
use fs2::FileExt;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

/// 跨进程互斥锁，基于 fs2/flock advisory lock。
///
/// 相比此前的"创建锁文件 + token 校验 + mtime stale 判定"方案：
/// - 进程退出/崩溃时 OS 自动释放锁，无 stale 锁文件与心跳续期问题；
/// - 锁随 fd 关闭（guard Drop）释放，不存在"读 token → 比对 → 删文件"的
///   check-then-act 窗口，不会误删其他进程刚接管的锁。
///
/// 锁文件内容仅作诊断信息（持锁 PID:UUID），不承载锁语义。
pub(crate) struct FileOwnerLock {
    _file: fs::File,
}

impl FileOwnerLock {
    pub(crate) fn acquire(root: &Path, namespace: &str, key: &str) -> Result<Self> {
        Self::acquire_with_timeout(root, namespace, key, Duration::from_secs(10))
    }

    pub(crate) fn acquire_with_timeout(
        root: &Path,
        namespace: &str,
        key: &str,
        timeout: Duration,
    ) -> Result<Self> {
        let lock_dir = root.join(".locks");
        fs::create_dir_all(&lock_dir)?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        let path = lock_dir.join(format!("{}-{:016x}.lock", namespace, hasher.finish()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        let deadline = Instant::now() + timeout;

        loop {
            match file.try_lock_exclusive() {
                Ok(()) => {
                    let _ = file.set_len(0);
                    let owner =
                        format!("{}:{}", std::process::id(), uuid::Uuid::new_v4());
                    let _ = (&file).write_all(owner.as_bytes());
                    let _ = file.sync_all();
                    return Ok(Self { _file: file });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(Error::Other(format!(
                            "Timed out acquiring file lock: {}",
                            path.display()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn test_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "blockcell-fol-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn held_lock_blocks_second_acquire_until_drop() {
        let root = test_root("mutual-exclusion");
        let first = FileOwnerLock::acquire_with_timeout(
            &root,
            "ns",
            "key",
            Duration::from_millis(200),
        )
        .expect("first acquire");

        // 同一 key 第二次获取必须在超时后失败（锁是互斥的）
        let second = FileOwnerLock::acquire_with_timeout(
            &root,
            "ns",
            "key",
            Duration::from_millis(150),
        );
        assert!(second.is_err(), "held lock must reject second acquire");

        // Drop 释放 fd → OS 释放 flock → 可再获取（不存在 stale 锁文件概念）
        drop(first);
        FileOwnerLock::acquire_with_timeout(
            &root,
            "ns",
            "key",
            Duration::from_millis(500),
        )
        .expect("acquire after release");
    }

    #[test]
    fn different_keys_are_independent() {
        let root = test_root("independent");
        let _a = FileOwnerLock::acquire_with_timeout(
            &root,
            "ns",
            "key-a",
            Duration::from_millis(100),
        )
        .unwrap();
        FileOwnerLock::acquire_with_timeout(
            &root,
            "ns",
            "key-b",
            Duration::from_millis(100),
        )
        .expect("different key must not block");
    }
}
