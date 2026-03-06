use blockcell_core::types::ChatMessage;
use blockcell_core::{Paths, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use tracing::debug;

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
        let path = self.paths.session_file(session_key);

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

    pub fn save(&self, session_key: &str, messages: &[ChatMessage]) -> Result<()> {
        let path = self.paths.session_file(session_key);

        // Ensure directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let now = chrono::Utc::now().to_rfc3339();

        // 保留原始 created_at：若文件已存在则从第一行读取，否则使用当前时间
        let created_at = if path.exists() {
            self.read_created_at(&path).unwrap_or_else(|| now.clone())
        } else {
            now.clone()
        };

        let mut file = File::create(&path)?;

        // Write metadata
        let metadata = SessionLine::Metadata {
            created_at,
            updated_at: now,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        };
        writeln!(file, "{}", serde_json::to_string(&metadata)?)?;

        // Write messages
        for msg in messages {
            writeln!(file, "{}", serde_json::to_string(msg)?)?;
        }

        Ok(())
    }

    /// 从 session 文件第一行读取 created_at 字段。
    fn read_created_at(&self, path: &std::path::Path) -> Option<String> {
        let file = File::open(path).ok()?;
        let mut reader = BufReader::new(file);
        let mut first_line = String::new();
        reader.read_line(&mut first_line).ok()?;
        let line: SessionLine = serde_json::from_str(first_line.trim()).ok()?;
        match line {
            SessionLine::Metadata { created_at, .. } => Some(created_at),
            _ => None,
        }
    }

    pub fn append(&self, session_key: &str, message: &ChatMessage) -> Result<()> {
        let path = self.paths.session_file(session_key);

        // Ensure directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

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

    /// Set session display name in _meta.json, only if not already set.
    /// `content` is the user's first message; we take the first ~30 chars as the name.
    pub fn set_session_name_if_new(&self, session_key: &str, content: &str) -> Option<String> {
        let meta_path = self.paths.sessions_dir().join("_meta.json");
        let file_key = session_key.replace(':', "_");

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
