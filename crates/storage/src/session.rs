use crate::file_lock::lock_exclusive;
use blockcell_core::types::ChatMessage;
use blockcell_core::{session_file_stem, session_id_from_file_stem, Paths, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::debug;

static SESSION_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn session_temp_path(path: &std::path::Path) -> std::path::PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session.jsonl");
    let counter = SESSION_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(
        ".{file_name}.tmp.{}.{}",
        std::process::id(),
        counter
    ))
}

#[cfg(windows)]
fn replace_session_file(
    temp_path: &std::path::Path,
    path: &std::path::Path,
) -> std::io::Result<()> {
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
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_session_file(
    temp_path: &std::path::Path,
    path: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::rename(temp_path, path)
}

fn write_session_bytes_atomically(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let temp_path = session_temp_path(path);
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let file = options.open(&temp_path)?;
        if let Ok(metadata) = std::fs::metadata(path) {
            std::fs::set_permissions(&temp_path, metadata.permissions())?;
        }

        let mut writer = BufWriter::new(file);
        writer.write_all(bytes)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);

        replace_session_file(&temp_path, path)?;

        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }

        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "_type")]
enum SessionLine {
    #[serde(rename = "metadata")]
    Metadata {
        created_at: String,
        updated_at: String,
        #[serde(default)]
        metadata: serde_json::Value,
    },
    #[serde(untagged)]
    Message(ChatMessage),
}

pub struct SessionStore {
    paths: Paths,
}

impl SessionStore {
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }

    pub fn load(&self, session_key: &str) -> Result<Vec<ChatMessage>> {
        let path = self.read_path(session_key);

        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&path)?;
        let reader = BufReader::new(file);
        let mut messages = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<SessionLine>(&line) {
                Ok(SessionLine::Message(msg)) => {
                    messages.push(msg);
                }
                Ok(SessionLine::Metadata { .. }) => {
                    // Skip metadata line
                }
                Err(e) => {
                    debug!(error = %e, "Failed to parse session line, skipping");
                }
            }
        }

        Ok(messages)
    }

    /// 读取 session 文件外层的时间戳（created_at, updated_at）
    ///
    /// 与 load_metadata() 不同，此方法返回的是 metadata 行外层的 created_at 和 updated_at，
    /// 而非 metadata 子对象内部的内容。Layer 2 时间触发轻量压缩需要 updated_at 来判断
    /// 会话是否长时间未更新。
    pub fn load_timestamps(&self, session_key: &str) -> Result<(Option<String>, Option<String>)> {
        let path = self.read_path(session_key);

        if !path.exists() {
            return Ok((None, None));
        }

        Ok(self
            .read_metadata_line(&path)
            .map(|(created_at, _)| (Some(created_at), self.read_updated_at(&path)))
            .unwrap_or((None, None)))
    }

    /// 从 session 文件第一行读取 updated_at 字段
    fn read_updated_at(&self, path: &std::path::Path) -> Option<String> {
        let file = File::open(path).ok()?;
        let mut reader = BufReader::new(file);
        let mut first_line = String::new();
        reader.read_line(&mut first_line).ok()?;
        let line: serde_json::Value = serde_json::from_str(first_line.trim()).ok()?;
        line.get("updated_at")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    pub fn load_metadata(&self, session_key: &str) -> Result<Value> {
        let path = self.read_path(session_key);

        if !path.exists() {
            return Ok(Value::Object(serde_json::Map::new()));
        }

        Ok(self
            .read_metadata_line(&path)
            .map(|(_, metadata)| metadata)
            .unwrap_or_else(|| Value::Object(serde_json::Map::new())))
    }

    pub fn save(&self, session_key: &str, messages: &[ChatMessage]) -> Result<()> {
        let path = self.paths.session_file(session_key);

        // Ensure directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _lock = lock_exclusive(&path)?;

        let now = chrono::Utc::now().to_rfc3339();

        // 保留原始 created_at：若文件已存在则从第一行读取，否则使用当前时间
        let (created_at, metadata) = if path.exists() {
            self.read_metadata_line(&path)
                .unwrap_or_else(|| (now.clone(), Value::Object(serde_json::Map::new())))
        } else {
            (now.clone(), Value::Object(serde_json::Map::new()))
        };

        self.write_session_file(&path, &created_at, &now, messages, &metadata)
    }

    pub fn save_with_metadata(
        &self,
        session_key: &str,
        messages: &[ChatMessage],
        metadata: &Value,
    ) -> Result<()> {
        let path = self.paths.session_file(session_key);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _lock = lock_exclusive(&path)?;

        let now = chrono::Utc::now().to_rfc3339();
        let created_at = if path.exists() {
            self.read_created_at(&path).unwrap_or_else(|| now.clone())
        } else {
            now.clone()
        };

        self.write_session_file(&path, &created_at, &now, messages, metadata)
    }

    /// 从 session 文件第一行读取 created_at 字段。
    fn read_created_at(&self, path: &std::path::Path) -> Option<String> {
        self.read_metadata_line(path)
            .map(|(created_at, _)| created_at)
    }

    fn read_metadata_line(&self, path: &std::path::Path) -> Option<(String, Value)> {
        let file = File::open(path).ok()?;
        let mut reader = BufReader::new(file);
        let mut first_line = String::new();
        reader.read_line(&mut first_line).ok()?;
        let line: SessionLine = serde_json::from_str(first_line.trim()).ok()?;
        match line {
            SessionLine::Metadata {
                created_at,
                metadata,
                ..
            } => Some((created_at, metadata)),
            _ => None,
        }
    }

    fn read_path(&self, session_key: &str) -> std::path::PathBuf {
        let current = self.paths.session_file(session_key);
        if current.exists() {
            current
        } else {
            let legacy = self.paths.legacy_session_file(session_key);
            if legacy.exists() {
                legacy
            } else {
                current
            }
        }
    }

    pub fn append(&self, session_key: &str, message: &ChatMessage) -> Result<()> {
        let path = self.paths.session_file(session_key);

        // Ensure directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _lock = lock_exclusive(&path)?;

        // 使用 create_new 原子性地判断文件是否为首次创建，消除 TOCTOU 竞态
        let is_new = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map(|mut f| {
                let now = chrono::Utc::now().to_rfc3339();
                let metadata = SessionLine::Metadata {
                    created_at: now.clone(),
                    updated_at: now,
                    metadata: serde_json::Value::Object(serde_json::Map::new()),
                };
                // 写入 metadata 行；若失败忽略（后续 append 仍可工作）
                let _ = writeln!(
                    f,
                    "{}",
                    serde_json::to_string(&metadata).unwrap_or_default()
                );
                true
            })
            .unwrap_or(false);
        let _ = is_new; // 仅用于首次写入 metadata，无需后续使用

        // Append message
        let mut file = OpenOptions::new().append(true).open(&path)?;
        writeln!(file, "{}", serde_json::to_string(message)?)?;

        Ok(())
    }

    fn write_session_file(
        &self,
        path: &std::path::Path,
        created_at: &str,
        updated_at: &str,
        messages: &[ChatMessage],
        metadata: &Value,
    ) -> Result<()> {
        let metadata_line = SessionLine::Metadata {
            created_at: created_at.to_string(),
            updated_at: updated_at.to_string(),
            metadata: metadata.clone(),
        };
        let mut content = Vec::new();
        writeln!(content, "{}", serde_json::to_string(&metadata_line)?)?;

        for msg in messages {
            writeln!(content, "{}", serde_json::to_string(msg)?)?;
        }

        write_session_bytes_atomically(path, &content)?;
        Ok(())
    }

    /// Clear session history by deleting the session file.
    /// Returns true if the file existed and was deleted, false if it didn't exist.
    /// 同时删除关联的 sidecar marker 文件（如 .dream_counted），保持生命周期一致。
    pub fn clear(&self, session_key: &str) -> Result<bool> {
        let path = self.paths.session_file(session_key);
        let _lock = lock_exclusive(&path)?;
        let legacy_path = self.paths.legacy_session_file(session_key);

        // 同步删除 sidecar marker 文件，避免会话清理后同 key 复用时 marker 残留
        for marker_path in [
            path.with_extension("dream_counted"),
            legacy_path.with_extension("dream_counted"),
        ] {
            if marker_path.exists() {
                let _ = std::fs::remove_file(&marker_path);
            }
        }

        let events_path = self.paths.session_events_file(session_key);
        if events_path.exists() {
            let _ = std::fs::remove_file(&events_path);
        }

        let mut deleted = false;
        if path.exists() {
            std::fs::remove_file(&path)?;
            debug!(session_key = %session_key, path = %path.display(), "Session file deleted");
            deleted = true;
        }
        if legacy_path != path && legacy_path.exists() {
            std::fs::remove_file(&legacy_path)?;
            deleted = true;
        }
        Ok(deleted)
    }

    /// 原子标记会话已被 Dream 计数，使用独立 marker 文件 + create_new 实现并发安全。
    ///
    /// 返回 true 表示本次成功标记（应递增计数），false 表示已被其他 runtime 标记过。
    /// 解决 metadata save 的 TOCTOU 竞态：两个 runtime 可能同时读到 dream_counted=false，
    /// 都执行 save_with_metadata（File::create 覆盖写），导致重复递增。
    /// create_new(true) 保证只有一个调用者能成功创建文件。
    ///
    /// 迁移兼容：旧版本在 metadata 中存储 dream_counted=true（无 sidecar marker）。
    /// 首次遇到旧会话时，检测 metadata 中的标记，自动创建 sidecar 并返回 false（不重复计数）。
    pub fn mark_dream_counted(&self, session_key: &str) -> bool {
        let path = self.paths.session_file(session_key);
        let marker_path = path.with_extension("dream_counted");

        if let Some(parent) = marker_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // create_new(true): 文件已存在返回 Err，只有一个调用者能成功
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker_path)
            .is_ok();

        if !created {
            // sidecar marker 已存在，说明已被计数过
            return false;
        }

        // sidecar 创建成功，但需要检查旧 metadata 是否已有 dream_counted=true
        // （从旧版本迁移的会话：metadata 有标记但无 sidecar）
        if let Ok(metadata) = self.load_metadata(session_key) {
            if metadata
                .get("dream_counted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                // 旧版本已计过数，不重复计数
                debug!(
                    session_key = %session_key,
                    "[dream] 检测到旧版本 dream_counted metadata，sidecar 已创建但不重复计数"
                );
                return false;
            }
        }

        // 全新标记，应递增计数
        true
    }

    /// 清除 dream 计数 sidecar marker
    ///
    /// 当 `increment_dream_session_count` 失败时调用，
    /// 删除 marker 以便下次交互时重试递增，避免永久漏计。
    pub fn clear_dream_counted_marker(&self, session_key: &str) {
        let path = self.paths.session_file(session_key);
        let marker_path = path.with_extension("dream_counted");
        if marker_path.exists() {
            let _ = std::fs::remove_file(&marker_path);
        }
    }

    /// Set session display name in _meta.json, only if not already set.
    /// `content` is the user's first message; we take the first ~30 chars as the name.
    pub fn set_session_name_if_new(&self, session_key: &str, content: &str) -> Option<String> {
        let meta_path = self.paths.sessions_dir().join("_meta.json");
        let _lock = lock_exclusive(&meta_path).ok()?;
        let full = session_file_stem(session_key);
        let file_key = session_id_from_file_stem(&full);

        let mut meta: serde_json::Map<String, serde_json::Value> = if meta_path.exists() {
            std::fs::read_to_string(&meta_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            serde_json::Map::new()
        };

        // Skip if already has a name
        if meta
            .get(&file_key)
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .is_some()
        {
            return None;
        }

        // Take first ~30 chars (by char boundary), strip whitespace
        let trimmed = content.trim();
        let name: String = trimmed.chars().take(30).collect();
        let name = name.trim_end().to_string();
        let name = if trimmed.chars().count() > 30 {
            format!("{}…", name)
        } else {
            name
        };

        if name.is_empty() {
            return None;
        }

        meta.insert(file_key, serde_json::json!({ "name": name.clone() }));

        let _ = std::fs::write(
            &meta_path,
            serde_json::to_string_pretty(&meta).unwrap_or_default(),
        );

        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs2::FileExt;
    use serde_json::json;
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::TempDir;

    fn test_store() -> (SessionStore, TempDir) {
        let dir = TempDir::new().expect("temp dir");
        let paths = Paths::with_base(dir.path().to_path_buf());
        (SessionStore::new(paths), dir)
    }

    #[test]
    fn load_reads_legacy_sanitized_session_file() {
        let (store, dir) = test_store();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
        let legacy_path = sessions_dir.join("ws_default_123.jsonl");
        let message = ChatMessage::user("legacy message");
        std::fs::write(
            legacy_path,
            format!("{}\n", serde_json::to_string(&message).expect("serialize")),
        )
        .expect("write legacy session");

        let loaded = store.load("ws:default_123").expect("load legacy session");

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, serde_json::json!("legacy message"));
    }

    #[test]
    fn clear_deletes_legacy_sanitized_session_file() {
        let (store, dir) = test_store();
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
        let legacy_path = sessions_dir.join("ws_default_123.jsonl");
        std::fs::write(&legacy_path, "legacy").expect("write legacy session");

        assert!(store.clear("ws:default_123").expect("clear legacy"));
        assert!(!legacy_path.exists());
    }

    #[test]
    fn append_waits_for_the_session_file_lock() {
        let (store, _dir) = test_store();
        let session_key = "ws:locked-chat";
        let path = store.paths.session_file(session_key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let lock_path = path.with_file_name(format!(
            ".{}.lock",
            path.file_name().unwrap().to_string_lossy()
        ));
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        lock_file.lock_exclusive().unwrap();

        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = store.append(session_key, &ChatMessage::user("serialized"));
            done_tx.send(result).unwrap();
        });

        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        lock_file.unlock().unwrap();
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn test_save_preserves_existing_session_metadata() {
        let (store, _dir) = test_store();
        let session_key = "ws:chat-1";
        let metadata = json!({
            "skill_state": {
                "last_skill": "deep_analysis",
                "mode": "chat"
            }
        });

        store
            .save_with_metadata(
                session_key,
                &[ChatMessage::user("查看上一级目录有哪些文件")],
                &metadata,
            )
            .expect("save with metadata");

        store
            .save(session_key, &[ChatMessage::user("查看 .env 的内容")])
            .expect("save messages while preserving metadata");

        let loaded = store
            .load_metadata(session_key)
            .expect("load metadata after save");
        assert_eq!(loaded["skill_state"]["last_skill"], "deep_analysis");
    }

    #[test]
    fn session_atomic_replacement_writes_complete_file_without_temp_residue() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "old").expect("write old session");

        write_session_bytes_atomically(&path, b"metadata\nmessage\n")
            .expect("atomic replacement should succeed");

        assert_eq!(std::fs::read(&path).unwrap(), b"metadata\nmessage\n");
        let entries: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["session.jsonl"]);
    }

    #[cfg(unix)]
    #[test]
    fn session_atomic_replacement_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, "old").expect("write old session");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("set restrictive permissions");

        write_session_bytes_atomically(&path, b"new").expect("atomic replacement should succeed");

        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
