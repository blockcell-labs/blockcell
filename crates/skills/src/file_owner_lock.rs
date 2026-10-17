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
        let deadline = Instant::now() + Duration::from_secs(10);

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
