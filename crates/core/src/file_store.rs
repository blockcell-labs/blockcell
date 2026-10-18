use fs2::FileExt;
use std::io::{Error, ErrorKind, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// A cross-process exclusive lock backed by an OS advisory file lock.
#[derive(Debug)]
pub struct ExclusiveFileLock {
    file: std::fs::File,
    held: bool,
}

impl ExclusiveFileLock {
    const MAX_RETRIES: usize = 30;
    const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

    pub fn try_acquire(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;
        if let Err(error) = file.try_lock_exclusive() {
            return if error.kind() == ErrorKind::WouldBlock {
                Err(Error::new(
                    ErrorKind::WouldBlock,
                    format!("file lock is already held: {}", path.display()),
                ))
            } else {
                Err(error)
            };
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        file.set_len(0)?;
        write!(
            file,
            "{}:{}:{}",
            std::process::id(),
            timestamp,
            uuid::Uuid::new_v4()
        )?;
        file.sync_all()?;

        Ok(Self { file, held: true })
    }

    pub fn acquire(path: &Path) -> std::io::Result<Self> {
        for attempt in 0..Self::MAX_RETRIES {
            match Self::try_acquire(path) {
                Ok(lock) => return Ok(lock),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if attempt + 1 < Self::MAX_RETRIES {
                        std::thread::sleep(Self::RETRY_INTERVAL);
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Err(Error::new(
            ErrorKind::WouldBlock,
            format!("timed out acquiring file lock: {}", path.display()),
        ))
    }

    pub fn release(&mut self) -> std::io::Result<()> {
        if !self.held {
            return Ok(());
        }
        self.file.unlock()?;
        self.held = false;
        Ok(())
    }
}

impl Drop for ExclusiveFileLock {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// Durably writes a complete replacement in the target directory and then
/// atomically swaps it into place.
pub fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("store");
    let temp_path = path.with_file_name(format!(
        ".{file_name}.tmp.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut open_options = std::fs::OpenOptions::new();
    open_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open_options.mode(0o600);
    }
    let mut temp = open_options.open(&temp_path)?;
    if let Err(error) = (|| {
        temp.write_all(data)?;
        temp.sync_all()?;
        drop(temp);
        replace_file(&temp_path, path)
    })() {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }

    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(temp_path: &Path, path: &Path) -> std::io::Result<()> {
    std::fs::rename(temp_path, path)
}

#[cfg(windows)]
fn replace_file(temp_path: &Path, path: &Path) -> std::io::Result<()> {
    let backup_path = path.with_file_name(format!(
        ".{}.bak.{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("store"),
        uuid::Uuid::new_v4()
    ));
    if path.exists() {
        std::fs::rename(path, &backup_path)?;
    }
    match std::fs::rename(temp_path, path) {
        Ok(()) => {
            let _ = std::fs::remove_file(backup_path);
            Ok(())
        }
        Err(error) => {
            if backup_path.exists() {
                let _ = std::fs::rename(backup_path, path);
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{atomic_write, ExclusiveFileLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "blockcell-file-store-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn exclusive_lock_rejects_second_live_holder() {
        let path = temp_path("exclusive.lock");
        let first = ExclusiveFileLock::try_acquire(&path).expect("first lock");
        let second = ExclusiveFileLock::try_acquire(&path);

        assert_eq!(
            second.expect_err("second holder must be rejected").kind(),
            std::io::ErrorKind::WouldBlock
        );

        drop(first);
        let reacquired = ExclusiveFileLock::try_acquire(&path).expect("lock after release");
        drop(reacquired);
        std::fs::remove_file(path).expect("cleanup lock file");
    }

    #[test]
    fn exclusive_lock_acquires_despite_stale_token_contents() {
        let path = temp_path("stale.lock");
        std::fs::write(&path, "4294967295:1:stale-token").expect("write stale lock");

        let lock = ExclusiveFileLock::try_acquire(&path).expect("replace stale lock");
        assert_ne!(
            std::fs::read_to_string(&path).expect("read current lock"),
            "4294967295:1:stale-token"
        );

        drop(lock);
    }

    #[test]
    fn release_does_not_remove_replacement_owned_by_another_token() {
        let path = temp_path("ownership.lock");
        let mut first = ExclusiveFileLock::try_acquire(&path).expect("first lock");
        std::fs::write(&path, "1:1:replacement-token").expect("replace lock contents");

        first.release().expect("release old token");

        assert!(path.exists(), "old owner must not delete replacement lock");
        std::fs::remove_file(path).expect("cleanup replacement");
    }

    #[test]
    fn live_file_lock_cannot_be_bypassed_by_replacing_its_contents() {
        let path = temp_path("tampered-live-lock");
        let _first = ExclusiveFileLock::try_acquire(&path).expect("first lock");
        std::fs::write(&path, "4294967295:1:fake-stale-token").expect("tamper contents");

        let second = ExclusiveFileLock::try_acquire(&path);

        assert_eq!(
            second
                .expect_err("live advisory lock must remain exclusive")
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn atomic_write_replaces_file_with_complete_contents() {
        let path = temp_path("atomic.json");
        std::fs::write(&path, br#"{"version":1,"jobs":[]}"#).expect("write original");

        atomic_write(&path, br#"{"version":1,"jobs":[{"id":"new"}]}"#).expect("atomic write");

        let content = std::fs::read_to_string(&path).expect("read replacement");
        let parsed: serde_json::Value = serde_json::from_str(&content).expect("complete json");
        assert_eq!(parsed["jobs"][0]["id"], "new");
        std::fs::remove_file(path).expect("cleanup file");
    }
}
