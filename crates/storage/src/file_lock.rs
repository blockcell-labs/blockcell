use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::io;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static ATOMIC_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) struct ExclusiveFileLock {
    file: File,
}

impl Drop for ExclusiveFileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Sidecar lock file path for `path` (`.{file_name}.lock`).
///
/// The lock file is a **permanent synchronization anchor** and must never be unlinked:
/// `lock_exclusive` guards the lock file's inode, so if the path were removed a new
/// locker would `create(true)` a fresh inode and acquire it immediately, bypassing a
/// holder that still owns the (now orphaned) previous inode. Leaving the empty file in
/// place keeps every locker on the same inode and preserves mutual exclusion.
pub(crate) fn lock_path_for(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    path.with_file_name(format!(".{file_name}.lock"))
}

pub(crate) fn lock_exclusive(path: &Path) -> io::Result<ExclusiveFileLock> {
    let lock_path = lock_path_for(path);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)?;
    file.lock_exclusive()?;
    Ok(ExclusiveFileLock { file })
}

pub(crate) fn lock_exclusive_with_timeout(
    path: &Path,
    timeout: std::time::Duration,
) -> io::Result<ExclusiveFileLock> {
    let lock_path = lock_path_for(path);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)?;
    let started = std::time::Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(ExclusiveFileLock { file }),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!("timed out waiting for file lock: {}", path.display()),
                    ));
                }
                std::thread::sleep(
                    std::time::Duration::from_millis(10).min(timeout.saturating_sub(elapsed)),
                );
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;

    #[test]
    fn exclusive_lock_timeout_returns_would_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.log");
        let _held = lock_exclusive(&path).unwrap();
        let started = std::time::Instant::now();
        let error = match lock_exclusive_with_timeout(&path, std::time::Duration::from_millis(40)) {
            Ok(_) => panic!("contended file lock should time out"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}

#[cfg(windows)]
fn replace_file(temp_path: &Path, path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let temp_wide: Vec<u16> = temp_path.as_os_str().encode_wide().chain(Some(0)).collect();
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let replaced = unsafe {
        MoveFileExW(
            temp_wide.as_ptr(),
            path_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if replaced == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(temp_path: &Path, path: &Path) -> io::Result<()> {
    std::fs::rename(temp_path, path)
}

pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let counter = ATOMIC_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_path = path.with_file_name(format!(
        ".{file_name}.tmp.{}.{}",
        std::process::id(),
        counter
    ));

    let result = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        if let Ok(metadata) = std::fs::metadata(path) {
            std::fs::set_permissions(&temp_path, metadata.permissions())?;
        }
        let mut writer = BufWriter::new(file);
        writer.write_all(bytes)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        replace_file(&temp_path, path)?;

        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}
