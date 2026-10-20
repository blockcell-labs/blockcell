use blockcell_core::{Error, Paths, Result};
use chrono::Utc;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::error;
use uuid::Uuid;

pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

static AUDIT_WRITE_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuditEvent {
    SessionStart {
        session_key: String,
        provider: String,
        model: String,
        channel: String,
        timestamp_ms: i64,
    },
    SessionEnd {
        session_key: String,
        turns: u32,
        total_tokens: u64,
        total_cost_cents: u64,
        timestamp_ms: i64,
    },
    ToolCall {
        tool_name: String,
        params: serde_json::Value,
        result: serde_json::Value,
        timestamp_ms: i64,
        session_key: String,
        trace_id: Option<String>,
        duration_ms: Option<u64>,
        /// 与 Session Event Log 中同一调用的 `call_id` 对齐；旧记录没有该字段。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        /// Runtime 判定的执行结论（`ToolOutcome`），被拒绝 / 未开始的调用同样记录。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<String>,
        /// 工具对本次调用声明的执行环境（与 `tool/call` 事件同名对齐）；旧记录没有该字段。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        environment: Option<String>,
    },
    PermissionDecision {
        tool_name: String,
        decision: String,
        matched_rule: Option<String>,
        description: Option<String>,
        simulated: bool,
        timestamp_ms: i64,
        session_key: String,
    },
    ProviderCall {
        provider: String,
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        cost_cents: u64,
        duration_ms: u64,
        timestamp_ms: i64,
        session_key: String,
    },
    BudgetEvent {
        event_kind: String,
        usage_ratio: f64,
        tokens_used: u64,
        cost_cents: u64,
        timestamp_ms: i64,
        session_key: String,
    },
    SkillSwitch {
        skill_name: String,
        from_version: Option<String>,
        to_version: String,
        reason: String,
        timestamp_ms: i64,
        session_key: String,
    },
    UpgradeAction {
        action: String,
        from_version: Option<String>,
        to_version: String,
        result: String,
        timestamp_ms: i64,
        error: Option<String>,
    },
}

/// 一次工具调用的审计内容。`trace_id`（回合 id）、`call_id` 与 `environment` 用于和
/// Session Event Log 对齐。
pub struct ToolCallAudit<'a> {
    pub tool_name: &'a str,
    pub params: serde_json::Value,
    pub result: serde_json::Value,
    pub session_key: &'a str,
    pub trace_id: Option<String>,
    pub duration_ms: Option<u64>,
    pub call_id: Option<String>,
    pub outcome: Option<&'a str>,
    /// 工具声明的执行环境；嵌套调用等拿不到注册表的场景为 `None`。
    pub environment: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub event_id: String,
    pub timestamp_ms: i64,
    pub session_id: String,
    pub event: AuditEvent,
    pub prev_hash: String,
    pub hash: String,
}

#[derive(Debug, Clone)]
pub struct ChainVerifyResult {
    pub valid: bool,
    pub total_records: usize,
    pub skipped_records: usize,
    pub errors: Vec<String>,
}

pub struct AuditLogger {
    paths: Paths,
    current_date: String,
    prev_hash: String,
    session_id: String,
}

impl AuditLogger {
    pub fn new(paths: Paths) -> Self {
        let mut logger = Self {
            paths,
            current_date: Utc::now().format("%Y-%m-%d").to_string(),
            prev_hash: GENESIS_HASH.to_string(),
            session_id: String::new(),
        };
        logger.recover_prev_hash();
        logger
    }

    pub fn set_session_id(&mut self, session_id: &str) {
        self.session_id = session_id.to_string();
    }

    pub fn log_session_start(
        &mut self,
        session_key: &str,
        provider: &str,
        model: &str,
        channel: &str,
    ) -> Result<()> {
        self.set_session_id(session_key);
        let event = AuditEvent::SessionStart {
            session_key: session_key.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            channel: channel.to_string(),
            timestamp_ms: Utc::now().timestamp_millis(),
        };
        self.write_event(event)
    }

    pub fn log_session_end(
        &mut self,
        session_key: &str,
        turns: u32,
        total_tokens: u64,
        total_cost_cents: u64,
    ) -> Result<()> {
        self.set_session_id(session_key);
        let event = AuditEvent::SessionEnd {
            session_key: session_key.to_string(),
            turns,
            total_tokens,
            total_cost_cents,
            timestamp_ms: Utc::now().timestamp_millis(),
        };
        self.write_event(event)
    }

    pub fn log_tool_call(
        &mut self,
        tool_name: &str,
        params: serde_json::Value,
        result: serde_json::Value,
        session_key: &str,
        trace_id: Option<String>,
        duration_ms: Option<u64>,
    ) -> Result<()> {
        self.log_tool_call_audit(ToolCallAudit {
            tool_name,
            params,
            result,
            session_key,
            trace_id,
            duration_ms,
            call_id: None,
            outcome: None,
            environment: None,
        })
    }

    pub fn log_tool_call_audit(&mut self, mut call: ToolCallAudit<'_>) -> Result<()> {
        // Tool params/results may carry credentials (api keys, tokens,
        // passwords); mask secret-looking keys before they hit disk.
        blockcell_core::config::mask_secret_values(&mut call.params);
        blockcell_core::config::mask_secret_values(&mut call.result);
        let event = AuditEvent::ToolCall {
            tool_name: call.tool_name.to_string(),
            params: call.params,
            result: call.result,
            timestamp_ms: Utc::now().timestamp_millis(),
            session_key: call.session_key.to_string(),
            trace_id: call.trace_id,
            duration_ms: call.duration_ms,
            call_id: call.call_id,
            outcome: call.outcome.map(str::to_string),
            environment: call.environment.map(str::to_string),
        };
        self.write_event(event)
    }

    pub fn log_permission_decision(
        &mut self,
        tool_name: &str,
        decision: String,
        matched_rule: Option<String>,
        description: Option<String>,
        simulated: bool,
        session_key: &str,
    ) -> Result<()> {
        let event = AuditEvent::PermissionDecision {
            tool_name: tool_name.to_string(),
            decision,
            matched_rule,
            description,
            simulated,
            timestamp_ms: Utc::now().timestamp_millis(),
            session_key: session_key.to_string(),
        };
        self.write_event(event)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn log_provider_call(
        &mut self,
        session_key: &str,
        provider: &str,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        cost_cents: u64,
        duration_ms: u64,
    ) -> Result<()> {
        self.set_session_id(session_key);
        let event = AuditEvent::ProviderCall {
            provider: provider.to_string(),
            model: model.to_string(),
            input_tokens,
            output_tokens,
            cost_cents,
            duration_ms,
            timestamp_ms: Utc::now().timestamp_millis(),
            session_key: session_key.to_string(),
        };
        self.write_event(event)
    }

    pub fn log_budget_event(
        &mut self,
        session_key: &str,
        event_kind: &str,
        usage_ratio: f64,
        tokens_used: u64,
        cost_cents: u64,
    ) -> Result<()> {
        self.set_session_id(session_key);
        let event = AuditEvent::BudgetEvent {
            event_kind: event_kind.to_string(),
            usage_ratio,
            tokens_used,
            cost_cents,
            timestamp_ms: Utc::now().timestamp_millis(),
            session_key: session_key.to_string(),
        };
        self.write_event(event)
    }

    pub fn log_skill_switch(
        &mut self,
        skill_name: &str,
        from_version: Option<String>,
        to_version: &str,
        reason: &str,
        session_key: &str,
    ) -> Result<()> {
        let event = AuditEvent::SkillSwitch {
            skill_name: skill_name.to_string(),
            from_version,
            to_version: to_version.to_string(),
            reason: reason.to_string(),
            timestamp_ms: Utc::now().timestamp_millis(),
            session_key: session_key.to_string(),
        };
        self.write_event(event)
    }

    pub fn log_upgrade_action(
        &mut self,
        action: &str,
        from_version: Option<String>,
        to_version: &str,
        result: &str,
        error: Option<String>,
    ) -> Result<()> {
        let event = AuditEvent::UpgradeAction {
            action: action.to_string(),
            from_version,
            to_version: to_version.to_string(),
            result: result.to_string(),
            timestamp_ms: Utc::now().timestamp_millis(),
            error,
        };
        self.write_event(event)
    }

    fn write_event(&mut self, event: AuditEvent) -> Result<()> {
        let _guard = AUDIT_WRITE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let log_file = self.current_log_file_path();

        // Ensure audit directory exists
        if let Some(parent) = log_file.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Cross-process exclusion: the process-local mutex above cannot stop another
        // blockcell process (gateway + CLI) from appending to the same log with a stale
        // `prev_hash`, which silently breaks the hash chain.
        let _cross_process_guard = crate::file_lock::lock_exclusive_with_timeout(
            &log_file,
            std::time::Duration::from_secs(10),
        )
        .map_err(|error| {
            Error::Storage(format!(
                "failed to lock audit log {}: {error}",
                log_file.display()
            ))
        })?;

        self.recover_prev_hash();

        let event_id_full = Uuid::new_v4().simple().to_string();
        let event_id = event_id_full[..16].to_string();
        let timestamp_ms = Utc::now().timestamp_millis();
        let hash = compute_record_hash(
            &event_id,
            timestamp_ms,
            &self.session_id,
            &event,
            &self.prev_hash,
        );
        let record = AuditRecord {
            event_id,
            timestamp_ms,
            session_id: self.session_id.clone(),
            event,
            prev_hash: std::mem::replace(&mut self.prev_hash, hash.clone()),
            hash,
        };

        // Open file in append mode
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)?;

        // Serialize event to JSON and write
        let json = serde_json::to_string(&record)?;
        writeln!(file, "{}", json)?;
        file.flush()?;
        // `flush` on a File is a no-op; without sync_data a crash can drop the
        // tail of the hash chain (integrity of the surviving prefix is intact,
        // but the last event is lost).
        file.sync_data()?;

        Ok(())
    }

    /// 获取当前日期对应的日志文件路径，并同步更新缓存的 current_date。
    fn current_log_file_path(&mut self) -> PathBuf {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        if today != self.current_date {
            self.current_date = today;
            self.prev_hash = GENESIS_HASH.to_string();
            self.recover_prev_hash();
        }
        self.paths
            .audit_dir()
            .join(format!("{}.jsonl", self.current_date))
    }

    fn peek_log_file_path(&self) -> PathBuf {
        self.paths
            .audit_dir()
            .join(format!("{}.jsonl", self.current_date))
    }

    fn recover_prev_hash(&mut self) {
        self.prev_hash = GENESIS_HASH.to_string();
        let log_file = self.peek_log_file_path();
        let Some(last_line) = read_last_nonempty_line(&log_file) else {
            return;
        };

        if let Ok(record) = serde_json::from_str::<AuditRecord>(&last_line) {
            self.prev_hash = record.hash;
        }
    }

    /// Read audit events from a specific date
    pub fn read_events(&self, date: &str) -> Result<Vec<AuditEvent>> {
        let log_file = self.paths.audit_dir().join(format!("{}.jsonl", date));

        if !log_file.exists() {
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(&log_file)?;
        let mut events = Vec::new();

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<AuditRecord>(line) {
                Ok(record) => events.push(record.event),
                Err(record_error) => match serde_json::from_str::<AuditEvent>(line) {
                    Ok(event) => events.push(event),
                    Err(event_error) => {
                        error!(
                            record_error = %record_error,
                            event_error = %event_error,
                            line = %line,
                            "Failed to parse audit event"
                        );
                    }
                },
            }
        }

        Ok(events)
    }

    /// Read today's audit events
    pub fn read_today(&self) -> Result<Vec<AuditEvent>> {
        // 实时获取当前日期，避免跨日期后读到旧日期的文件
        let today = Utc::now().format("%Y-%m-%d").to_string();
        self.read_events(&today)
    }

    pub fn verify_chain(log_file: &Path) -> ChainVerifyResult {
        let content = match std::fs::read_to_string(log_file) {
            Ok(content) => content,
            Err(e) => {
                return ChainVerifyResult {
                    valid: false,
                    total_records: 0,
                    skipped_records: 0,
                    errors: vec![format!("Cannot read file: {e}")],
                };
            }
        };

        let mut errors = Vec::new();
        let mut expected_prev = GENESIS_HASH.to_string();
        let mut total_records = 0;
        let mut skipped_records = 0;

        for (idx, line) in content.lines().enumerate() {
            let line_no = idx + 1;
            if line.trim().is_empty() {
                continue;
            }

            total_records += 1;
            let record = match serde_json::from_str::<AuditRecord>(line) {
                Ok(record) => record,
                Err(e) => {
                    skipped_records += 1;
                    errors.push(format!("Line {line_no}: invalid audit record: {e}"));
                    error!(
                        error = %e,
                        line = %line,
                        "Skipping non-hash-chain audit record"
                    );
                    continue;
                }
            };

            if record.prev_hash != expected_prev {
                errors.push(format!(
                    "Line {line_no}: prev_hash mismatch (expected {}..., got {}...)",
                    hash_prefix(&expected_prev),
                    hash_prefix(&record.prev_hash)
                ));
            }

            let recomputed = compute_record_hash(
                &record.event_id,
                record.timestamp_ms,
                &record.session_id,
                &record.event,
                &record.prev_hash,
            );
            if recomputed != record.hash {
                errors.push(format!(
                    "Line {line_no}: hash mismatch (stored {}..., recomputed {}...)",
                    hash_prefix(&record.hash),
                    hash_prefix(&recomputed)
                ));
            }

            expected_prev = record.hash;
        }

        ChainVerifyResult {
            valid: errors.is_empty(),
            total_records,
            skipped_records,
            errors,
        }
    }

    pub fn verify_today(&self) -> ChainVerifyResult {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let log_file = self.paths.audit_dir().join(format!("{today}.jsonl"));
        Self::verify_chain(&log_file)
    }
}

fn compute_record_hash(
    event_id: &str,
    timestamp_ms: i64,
    session_id: &str,
    event: &AuditEvent,
    prev_hash: &str,
) -> String {
    let event_value = canonicalize(&serde_json::to_value(event).unwrap_or(serde_json::Value::Null));

    let mut payload: BTreeMap<&str, serde_json::Value> = BTreeMap::new();
    payload.insert("event", event_value);
    payload.insert("event_id", serde_json::Value::from(event_id));
    payload.insert("prev_hash", serde_json::Value::from(prev_hash));
    payload.insert("session_id", serde_json::Value::from(session_id));
    payload.insert("timestamp_ms", serde_json::Value::from(timestamp_ms));

    let json = serde_json::to_string(&payload).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn canonicalize(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: BTreeMap<String, serde_json::Value> = map
                .iter()
                .map(|(key, value)| (key.clone(), canonicalize(value)))
                .collect();
            serde_json::to_value(sorted).unwrap_or(serde_json::Value::Null)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonicalize).collect())
        }
        other => other.clone(),
    }
}

fn hash_prefix(hash: &str) -> &str {
    &hash[..hash.len().min(12)]
}

fn read_last_nonempty_line(path: &Path) -> Option<String> {
    const CHUNK_SIZE: u64 = 8192;

    let mut file = File::open(path).ok()?;
    let mut position = file.seek(SeekFrom::End(0)).ok()?;
    let mut buffer = Vec::new();

    while position > 0 {
        let read_size = position.min(CHUNK_SIZE);
        position -= read_size;
        file.seek(SeekFrom::Start(position)).ok()?;

        let mut chunk = vec![0; read_size as usize];
        file.read_exact(&mut chunk).ok()?;
        chunk.extend(buffer);
        buffer = chunk;

        let end = buffer
            .iter()
            .rposition(|byte| !byte.is_ascii_whitespace())
            .map(|idx| idx + 1)?;

        if let Some(start) = buffer[..end].iter().rposition(|byte| *byte == b'\n') {
            let candidate = String::from_utf8_lossy(&buffer[start + 1..end])
                .trim()
                .to_string();
            if !candidate.is_empty() {
                return Some(candidate);
            }
        } else if position == 0 {
            let candidate = String::from_utf8_lossy(&buffer[..end]).trim().to_string();
            if !candidate.is_empty() {
                return Some(candidate);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_audit_logger() {
        let temp_dir = TempDir::new().unwrap();
        let paths = Paths::with_base(temp_dir.path().to_path_buf());
        let mut logger = AuditLogger::new(paths.clone());

        // Log a tool call
        logger
            .log_tool_call(
                "read_file",
                serde_json::json!({"path": "/tmp/test.txt"}),
                serde_json::json!({"content": "test"}),
                "cli:default",
                Some("trace-123".to_string()),
                Some(100),
            )
            .unwrap();

        // Read back
        let events = logger.read_today().unwrap();
        assert_eq!(events.len(), 1);

        match &events[0] {
            AuditEvent::ToolCall { tool_name, .. } => {
                assert_eq!(tool_name, "read_file");
            }
            _ => panic!("Expected ToolCall event"),
        }
    }

    #[test]
    fn test_tool_call_audit_masks_secret_values() {
        let temp_dir = TempDir::new().unwrap();
        let paths = Paths::with_base(temp_dir.path().to_path_buf());
        let mut logger = AuditLogger::new(paths.clone());

        logger
            .log_tool_call(
                "encrypt",
                serde_json::json!({"password": "hunter2", "path": "/tmp/a"}),
                serde_json::json!({"api_token": "tok-123", "status": "ok"}),
                "cli:default",
                None,
                None,
            )
            .unwrap();

        let events = logger.read_today().unwrap();
        match &events[0] {
            AuditEvent::ToolCall { params, result, .. } => {
                assert_eq!(params["password"], "********");
                assert_eq!(params["path"], "/tmp/a");
                assert_eq!(result["api_token"], "********");
                assert_eq!(result["status"], "ok");
            }
            _ => panic!("Expected ToolCall event"),
        }
    }

    #[test]
    fn test_permission_decision_audit_event() {
        let temp_dir = TempDir::new().unwrap();
        let paths = Paths::with_base(temp_dir.path().to_path_buf());
        let mut logger = AuditLogger::new(paths.clone());

        logger
            .log_permission_decision(
                "exec",
                "Deny".to_string(),
                Some("deny-exec".to_string()),
                Some("exec disabled".to_string()),
                false,
                "cli:policy",
            )
            .unwrap();

        let events = logger.read_today().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            AuditEvent::PermissionDecision {
                tool_name,
                decision,
                matched_rule,
                session_key,
                ..
            } => {
                assert_eq!(tool_name, "exec");
                assert_eq!(decision, "Deny");
                assert_eq!(matched_rule.as_deref(), Some("deny-exec"));
                assert_eq!(session_key, "cli:policy");
            }
            _ => panic!("Expected PermissionDecision event"),
        }
    }

    #[test]
    fn logs_session_provider_and_budget_audit_events() {
        let temp_dir = TempDir::new().unwrap();
        let paths = Paths::with_base(temp_dir.path().to_path_buf());
        let mut logger = AuditLogger::new(paths.clone());

        logger
            .log_session_start("cli:session", "openai", "gpt-5.5", "cli")
            .unwrap();
        logger
            .log_provider_call("cli:session", "openai", "gpt-5.5", 100, 25, 3, 250)
            .unwrap();
        logger
            .log_budget_event("cli:session", "warning", 0.8, 1000, 12)
            .unwrap();
        logger.log_session_end("cli:session", 2, 125, 3).unwrap();

        let events = logger.read_today().unwrap();
        assert_eq!(events.len(), 4);
        assert!(matches!(
            &events[0],
            AuditEvent::SessionStart {
                session_key,
                provider,
                model,
                channel,
                ..
            } if session_key == "cli:session"
                && provider == "openai"
                && model == "gpt-5.5"
                && channel == "cli"
        ));
        assert!(matches!(
            &events[1],
            AuditEvent::ProviderCall {
                session_key,
                provider,
                model,
                input_tokens: 100,
                output_tokens: 25,
                cost_cents: 3,
                duration_ms: 250,
                ..
            } if session_key == "cli:session"
                && provider == "openai"
                && model == "gpt-5.5"
        ));
        assert!(matches!(
            &events[2],
            AuditEvent::BudgetEvent {
                session_key,
                event_kind,
                usage_ratio,
                tokens_used: 1000,
                cost_cents: 12,
                ..
            } if session_key == "cli:session"
                && event_kind == "warning"
                && (*usage_ratio - 0.8).abs() < f64::EPSILON
        ));
        assert!(matches!(
            &events[3],
            AuditEvent::SessionEnd {
                session_key,
                turns: 2,
                total_tokens: 125,
                total_cost_cents: 3,
                ..
            } if session_key == "cli:session"
        ));

        let log_file = paths
            .audit_dir()
            .join(format!("{}.jsonl", Utc::now().format("%Y-%m-%d")));
        let result = AuditLogger::verify_chain(&log_file);
        assert!(result.valid, "{:?}", result.errors);
        assert_eq!(result.total_records, 4);
    }

    #[test]
    fn writes_hash_chained_records_and_verifies_them() {
        let temp_dir = TempDir::new().unwrap();
        let paths = Paths::with_base(temp_dir.path().to_path_buf());
        let mut logger = AuditLogger::new(paths.clone());
        logger.set_session_id("session-1");

        logger
            .log_tool_call(
                "read_file",
                serde_json::json!({"path": "/tmp/a.txt"}),
                serde_json::json!({"ok": true}),
                "cli:default",
                None,
                Some(10),
            )
            .unwrap();
        logger
            .log_permission_decision(
                "exec",
                "Allow".to_string(),
                Some("allow-safe".to_string()),
                None,
                false,
                "cli:default",
            )
            .unwrap();

        let today = Utc::now().format("%Y-%m-%d").to_string();
        let log_file = paths.audit_dir().join(format!("{today}.jsonl"));
        let lines: Vec<_> = std::fs::read_to_string(&log_file)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(lines.len(), 2);

        let first: AuditRecord = serde_json::from_str(&lines[0]).unwrap();
        let second: AuditRecord = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(first.prev_hash, GENESIS_HASH);
        assert_eq!(first.hash.len(), 64);
        assert_eq!(second.prev_hash, first.hash);
        assert_eq!(first.session_id, "session-1");
        assert_eq!(second.session_id, "session-1");

        let result = AuditLogger::verify_chain(&log_file);
        assert!(result.valid, "{:?}", result.errors);
        assert_eq!(result.total_records, 2);
    }

    #[test]
    fn hash_is_stable_for_different_json_object_insertion_order() {
        let event_a = AuditEvent::ToolCall {
            tool_name: "echo".to_string(),
            params: serde_json::json!({"b": 2, "a": {"z": 1, "y": 2}}),
            result: serde_json::json!({"ok": true}),
            timestamp_ms: 1000,
            session_key: "cli:test".to_string(),
            trace_id: None,
            duration_ms: Some(5),
            call_id: None,
            outcome: None,
            environment: None,
        };
        let event_b = AuditEvent::ToolCall {
            tool_name: "echo".to_string(),
            params: serde_json::json!({"a": {"y": 2, "z": 1}, "b": 2}),
            result: serde_json::json!({"ok": true}),
            timestamp_ms: 1000,
            session_key: "cli:test".to_string(),
            trace_id: None,
            duration_ms: Some(5),
            call_id: None,
            outcome: None,
            environment: None,
        };

        let hash_a = compute_record_hash("event-id", 2000, "session", &event_a, GENESIS_HASH);
        let hash_b = compute_record_hash("event-id", 2000, "session", &event_b, GENESIS_HASH);

        assert_eq!(hash_a, hash_b);
    }

    #[test]
    fn tool_call_audit_links_call_id_and_outcome_and_keeps_chain_valid() {
        let temp_dir = TempDir::new().unwrap();
        let paths = Paths::with_base(temp_dir.path().to_path_buf());
        let mut logger = AuditLogger::new(paths.clone());

        // 旧格式记录（没有 call_id / outcome）与新格式记录混合时链仍然有效
        logger
            .log_tool_call(
                "read_file",
                serde_json::json!({}),
                serde_json::json!({}),
                "cli:a",
                None,
                None,
            )
            .unwrap();
        logger
            .log_tool_call_audit(ToolCallAudit {
                tool_name: "exec",
                params: serde_json::json!({"command": "ls"}),
                result: serde_json::json!({"error": "denied"}),
                session_key: "cli:a",
                trace_id: Some("turn-1".to_string()),
                duration_ms: Some(0),
                call_id: Some("call-1".to_string()),
                outcome: Some("rejected"),
                environment: Some("full-access"),
            })
            .unwrap();

        let events = logger.read_today().unwrap();
        match &events[1] {
            AuditEvent::ToolCall {
                call_id,
                outcome,
                trace_id,
                environment,
                ..
            } => {
                assert_eq!(call_id.as_deref(), Some("call-1"));
                assert_eq!(outcome.as_deref(), Some("rejected"));
                assert_eq!(trace_id.as_deref(), Some("turn-1"));
                assert_eq!(environment.as_deref(), Some("full-access"));
            }
            other => panic!("Expected ToolCall event, got {other:?}"),
        }
        let raw = std::fs::read_to_string(logger.current_log_file_path()).unwrap();
        assert!(!raw.lines().next().unwrap().contains("call_id"));
        // 旧格式记录（无 call_id / environment）保持原样，序列化时字段被跳过
        assert!(!raw.lines().next().unwrap().contains("environment"));

        let result = AuditLogger::verify_chain(&logger.current_log_file_path());
        assert!(result.valid, "{:?}", result.errors);
    }

    #[test]
    fn verify_chain_detects_tampered_record() {
        let temp_dir = TempDir::new().unwrap();
        let paths = Paths::with_base(temp_dir.path().to_path_buf());
        let mut logger = AuditLogger::new(paths.clone());

        logger
            .log_tool_call(
                "read_file",
                serde_json::json!({"path": "/tmp/a.txt"}),
                serde_json::json!({"ok": true}),
                "cli:default",
                None,
                Some(10),
            )
            .unwrap();

        let today = Utc::now().format("%Y-%m-%d").to_string();
        let log_file = paths.audit_dir().join(format!("{today}.jsonl"));
        let line = std::fs::read_to_string(&log_file).unwrap();
        let mut record: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        record["event"]["result"] = serde_json::json!({"ok": false});
        std::fs::write(
            &log_file,
            format!("{}\n", serde_json::to_string(&record).unwrap()),
        )
        .unwrap();

        let result = AuditLogger::verify_chain(&log_file);
        assert!(!result.valid);
        assert!(result
            .errors
            .iter()
            .any(|err| err.contains("hash mismatch")));
    }

    #[test]
    fn verify_chain_detects_deleted_middle_record() {
        let temp_dir = TempDir::new().unwrap();
        let paths = Paths::with_base(temp_dir.path().to_path_buf());
        let mut logger = AuditLogger::new(paths.clone());

        for idx in 0..3 {
            logger
                .log_tool_call(
                    "read_file",
                    serde_json::json!({"path": format!("/tmp/{idx}.txt")}),
                    serde_json::json!({"ok": true}),
                    "cli:default",
                    None,
                    Some(10),
                )
                .unwrap();
        }

        let today = Utc::now().format("%Y-%m-%d").to_string();
        let log_file = paths.audit_dir().join(format!("{today}.jsonl"));
        let lines: Vec<_> = std::fs::read_to_string(&log_file)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        std::fs::write(&log_file, format!("{}\n{}\n", lines[0], lines[2])).unwrap();

        let result = AuditLogger::verify_chain(&log_file);
        assert!(!result.valid);
        assert!(result
            .errors
            .iter()
            .any(|err| err.contains("prev_hash mismatch")));
    }

    #[test]
    fn verify_chain_rejects_malformed_nonempty_records() {
        let temp_dir = TempDir::new().unwrap();
        let paths = Paths::with_base(temp_dir.path().to_path_buf());
        let mut logger = AuditLogger::new(paths.clone());
        logger
            .log_permission_decision("exec", "Allow".to_string(), None, None, false, "cli:test")
            .unwrap();

        let today = Utc::now().format("%Y-%m-%d").to_string();
        let log_file = paths.audit_dir().join(format!("{today}.jsonl"));
        let mut file = OpenOptions::new().append(true).open(&log_file).unwrap();
        writeln!(file, "not-json").unwrap();

        let result = AuditLogger::verify_chain(&log_file);
        assert!(!result.valid);
        assert_eq!(result.skipped_records, 1);
    }

    #[test]
    fn read_events_accepts_legacy_and_hash_wrapped_records() {
        let temp_dir = TempDir::new().unwrap();
        let paths = Paths::with_base(temp_dir.path().to_path_buf());
        let mut logger = AuditLogger::new(paths.clone());
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let log_file = paths.audit_dir().join(format!("{today}.jsonl"));
        std::fs::create_dir_all(paths.audit_dir()).unwrap();

        let legacy = AuditEvent::SkillSwitch {
            skill_name: "legacy".to_string(),
            from_version: None,
            to_version: "1.0.0".to_string(),
            reason: "test".to_string(),
            timestamp_ms: 1000,
            session_key: "cli:legacy".to_string(),
        };
        std::fs::write(
            &log_file,
            format!("{}\n", serde_json::to_string(&legacy).unwrap()),
        )
        .unwrap();

        logger
            .log_permission_decision("exec", "Deny".to_string(), None, None, false, "cli:new")
            .unwrap();

        let events = logger.read_events(&today).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], AuditEvent::SkillSwitch { .. }));
        assert!(matches!(events[1], AuditEvent::PermissionDecision { .. }));
    }

    #[test]
    fn concurrent_logger_instances_preserve_chain() {
        let temp_dir = TempDir::new().unwrap();
        let paths = Paths::with_base(temp_dir.path().to_path_buf());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut handles = Vec::new();

        for idx in 0..8 {
            let paths = paths.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let mut logger = AuditLogger::new(paths);
                logger.set_session_id(&format!("session-{idx}"));
                barrier.wait();
                logger
                    .log_permission_decision(
                        "exec",
                        "Allow".to_string(),
                        Some(format!("rule-{idx}")),
                        None,
                        false,
                        &format!("cli:{idx}"),
                    )
                    .unwrap();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let today = Utc::now().format("%Y-%m-%d").to_string();
        let log_file = paths.audit_dir().join(format!("{today}.jsonl"));
        let result = AuditLogger::verify_chain(&log_file);
        assert!(result.valid, "{:?}", result.errors);
        assert_eq!(result.total_records, 8);
    }
}
