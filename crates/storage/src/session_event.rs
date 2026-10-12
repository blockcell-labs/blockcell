//! Append-only Session Event Log（Phase A：transcript 仍是主存储，runtime 额外产生 SessionEvent）。
//!
//! - 存储格式是带 `schema_version` 的 JSON envelope，事件类型使用稳定字符串名，
//!   不依赖 Rust enum 的内部表示；
//! - 每个会话一个 `session_events/<stem>.jsonl` 文件，`seq` 在文件锁内单调递增；
//! - 消费方（Ghost / Replay / Evaluation）通过 [`SessionEventReader`] 读取，
//!   未来可替换为 SQLite / 远程存储而不影响调用方。

use crate::file_lock::lock_exclusive;
use blockcell_core::{Paths, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use tracing::debug;

pub const SESSION_EVENT_SCHEMA_VERSION: u16 = 1;

/// 会话超过该天数未产生新事件时，整个事件日志会被维护任务清理。
pub const SESSION_EVENT_RETENTION_DAYS: u64 = 30;

/// 单个字符串字段写入 payload 时的最大字符数，避免大工具结果撑爆日志。
pub const MAX_PAYLOAD_STRING_CHARS: usize = 8_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionEventKind {
    #[serde(rename = "session/start")]
    SessionStart,
    #[serde(rename = "turn/start")]
    TurnStart,
    #[serde(rename = "turn/end")]
    TurnEnd,
    #[serde(rename = "user/message")]
    UserMessage,
    #[serde(rename = "assistant/message")]
    AssistantMessage,
    /// 模型产生但最终未被采用的输出（例如流式中断后重试被丢弃的部分回答）。
    #[serde(rename = "assistant/attempt")]
    AssistantAttempt,
    #[serde(rename = "step/start")]
    StepStart,
    #[serde(rename = "step/end")]
    StepEnd,
    #[serde(rename = "model/request")]
    ModelRequest,
    #[serde(rename = "model/response")]
    ModelResponse,
    /// 模型请求最终失败：失败类型与 Runtime 选择的恢复策略。
    #[serde(rename = "model/request-error")]
    ModelRequestError,
    #[serde(rename = "tool/call")]
    ToolCall,
    #[serde(rename = "tool/result")]
    ToolResult,
    #[serde(rename = "skill/load")]
    SkillLoad,
    #[serde(rename = "memory/write")]
    MemoryWrite,
    /// 旧版单条压缩事件，仅用于读取历史日志；新日志写 `compaction/start` / `compaction/end`。
    #[serde(rename = "compaction")]
    Compaction,
    #[serde(rename = "compaction/start")]
    CompactionStart,
    #[serde(rename = "compaction/end")]
    CompactionEnd,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    pub schema_version: u16,
    pub seq: u64,
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    pub kind: SessionEventKind,
    #[serde(default)]
    pub payload: Value,
}

/// 待追加的事件；`seq` / `timestamp` / `schema_version` 由 writer 分配。
#[derive(Debug, Clone)]
pub struct NewSessionEvent {
    pub turn_id: Option<String>,
    pub step: Option<u32>,
    pub kind: SessionEventKind,
    pub payload: Value,
}

pub trait SessionEventReader: Send + Sync {
    fn read(&self, session_id: &str) -> Result<Vec<SessionEvent>>;

    fn read_since(&self, session_id: &str, after_seq: u64) -> Result<Vec<SessionEvent>> {
        Ok(self
            .read(session_id)?
            .into_iter()
            .filter(|event| event.seq > after_seq)
            .collect())
    }
}

pub trait SessionEventWriter: Send + Sync {
    fn append(&self, session_id: &str, event: NewSessionEvent) -> Result<SessionEvent>;
}

/// 基于 JSONL 侧车文件的事件日志。
#[derive(Clone)]
pub struct JsonlSessionEventLog {
    paths: Paths,
}

impl JsonlSessionEventLog {
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }

    /// 删除最后写入时间早于 `max_age` 的会话事件日志，返回删除的日志数。
    pub fn cleanup_older_than(&self, max_age: std::time::Duration) -> Result<usize> {
        let dir = self.paths.session_events_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(0);
        };
        let Some(cutoff) = std::time::SystemTime::now().checked_sub(max_age) else {
            return Ok(0);
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let _lock = lock_exclusive(&path)?;
            // 持锁后重新检查，避免删除刚被追加写入的日志
            let expired = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .is_ok_and(|modified| modified < cutoff);
            if expired && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
            // 锁文件保留：unlink 会破坏互斥（详见 file_lock::lock_path_for）。
        }
        Ok(removed)
    }
}

impl SessionEventWriter for JsonlSessionEventLog {
    fn append(&self, session_id: &str, event: NewSessionEvent) -> Result<SessionEvent> {
        let path = self.paths.session_events_file(session_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let _lock = lock_exclusive(&path)?;

        let record = SessionEvent {
            schema_version: SESSION_EVENT_SCHEMA_VERSION,
            seq: last_seq(&path)? + 1,
            session_id: session_id.to_string(),
            timestamp: Utc::now(),
            turn_id: event.turn_id,
            step: event.step,
            kind: event.kind,
            payload: truncate_payload(event.payload),
        };
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(file, "{}", serde_json::to_string(&record)?)?;
        Ok(record)
    }
}

impl SessionEventReader for JsonlSessionEventLog {
    fn read(&self, session_id: &str) -> Result<Vec<SessionEvent>> {
        let path = self.paths.session_events_file(session_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut events = Vec::new();
        for line in BufReader::new(File::open(&path)?).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<SessionEvent>(&line) {
                Ok(event) => events.push(event),
                Err(error) => debug!(error = %error, "Skipping unreadable session event line"),
            }
        }
        Ok(events)
    }
}

/// 读取文件最后一条完整事件的 seq；只读取文件尾部，避免每次追加都扫描全量日志。
fn last_seq(path: &Path) -> Result<u64> {
    const TAIL_BYTES: u64 = 256 * 1024;
    let Ok(mut file) = File::open(path) else {
        return Ok(0);
    };
    let len = file.metadata()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    let text = String::from_utf8_lossy(&tail);
    let lines: Vec<&str> = text.lines().collect();
    // 从尾部截断读取时第一行可能不完整，解析失败即跳过
    for line in lines.iter().rev() {
        if let Some(seq) = serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|value| value.get("seq").and_then(Value::as_u64))
        {
            return Ok(seq);
        }
    }
    if start == 0 {
        return Ok(0);
    }
    // 极端情况下尾部窗口内没有完整行：回退到全量扫描
    let mut last = 0;
    for line in BufReader::new(File::open(path)?).lines() {
        if let Some(seq) = serde_json::from_str::<Value>(&line?)
            .ok()
            .and_then(|value| value.get("seq").and_then(Value::as_u64))
        {
            last = seq;
        }
    }
    Ok(last)
}

fn truncate_payload(value: Value) -> Value {
    match value {
        Value::String(text) if text.chars().count() > MAX_PAYLOAD_STRING_CHARS => {
            let total = text.chars().count();
            let kept: String = text.chars().take(MAX_PAYLOAD_STRING_CHARS).collect();
            Value::String(format!(
                "{kept}...<truncated {} chars>",
                total - MAX_PAYLOAD_STRING_CHARS
            ))
        }
        Value::Array(items) => Value::Array(items.into_iter().map(truncate_payload).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, truncate_payload(value)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_lock::lock_path_for;
    use serde_json::json;

    fn log() -> (tempfile::TempDir, JsonlSessionEventLog) {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::with_base(dir.path().to_path_buf());
        (dir, JsonlSessionEventLog::new(paths))
    }

    fn event(kind: SessionEventKind, payload: Value) -> NewSessionEvent {
        NewSessionEvent {
            turn_id: Some("t1".into()),
            step: None,
            kind,
            payload,
        }
    }

    #[test]
    fn appends_with_monotonic_seq_and_reads_back() {
        let (_dir, log) = log();
        let first = log
            .append("cli:a", event(SessionEventKind::TurnStart, json!({})))
            .unwrap();
        let second = log
            .append(
                "cli:a",
                event(SessionEventKind::UserMessage, json!({"content": "hi"})),
            )
            .unwrap();
        log.append("cli:b", event(SessionEventKind::TurnStart, json!({})))
            .unwrap();

        assert_eq!((first.seq, second.seq), (1, 2));
        let events = log.read("cli:a").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, SessionEventKind::UserMessage);
        assert_eq!(events[1].schema_version, SESSION_EVENT_SCHEMA_VERSION);
        assert_eq!(log.read_since("cli:a", 1).unwrap(), vec![events[1].clone()]);
        assert_eq!(log.read("cli:b").unwrap()[0].seq, 1);
    }

    #[test]
    fn stores_stable_kind_names_and_truncates_large_strings() {
        let (dir, log) = log();
        let big = "x".repeat(MAX_PAYLOAD_STRING_CHARS + 10);
        log.append(
            "cli:a",
            event(SessionEventKind::ToolResult, json!({"result": big})),
        )
        .unwrap();

        let raw = std::fs::read_to_string(
            Paths::with_base(dir.path().to_path_buf()).session_events_file("cli:a"),
        )
        .unwrap();
        assert!(raw.contains("\"kind\":\"tool/result\""));
        assert!(raw.contains("<truncated 10 chars>"));
    }

    #[test]
    fn cleanup_removes_only_expired_logs() {
        let (_dir, log) = log();
        log.append("cli:old", event(SessionEventKind::TurnStart, json!({})))
            .unwrap();
        log.append("cli:new", event(SessionEventKind::TurnStart, json!({})))
            .unwrap();
        let old_path = log.paths.session_events_file("cli:old");
        let stale = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 86_400);
        File::options()
            .write(true)
            .open(&old_path)
            .unwrap()
            .set_modified(stale)
            .unwrap();

        let removed = log
            .cleanup_older_than(std::time::Duration::from_secs(
                SESSION_EVENT_RETENTION_DAYS * 86_400,
            ))
            .unwrap();

        assert_eq!(removed, 1);
        assert!(!old_path.exists());
        // 锁文件是长期存在的同步锚点，清理时保留（见 file_lock::lock_path_for）
        assert!(lock_path_for(&old_path).exists());
        assert_eq!(log.read("cli:new").unwrap().len(), 1);
    }

    #[test]
    fn seq_continues_after_large_tail() {
        let (_dir, log) = log();
        for _ in 0..40 {
            log.append(
                "cli:a",
                event(
                    SessionEventKind::ToolResult,
                    json!({"result": "y".repeat(MAX_PAYLOAD_STRING_CHARS)}),
                ),
            )
            .unwrap();
        }
        let next = log
            .append("cli:a", event(SessionEventKind::TurnEnd, json!({})))
            .unwrap();
        assert_eq!(next.seq, 41);
    }
}
