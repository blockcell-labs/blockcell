use chrono::Utc;
use blockcell_core::{Paths, Result};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use tracing::error;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuditEvent {
    ToolCall {
        tool_name: String,
        params: serde_json::Value,
        result: serde_json::Value,
        timestamp_ms: i64,
        session_key: String,
        trace_id: Option<String>,
        duration_ms: Option<u64>,
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

pub struct AuditLogger {
    paths: Paths,
    current_date: String,
}

impl AuditLogger {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            current_date: Utc::now().format("%Y-%m-%d").to_string(),
        }
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
        let event = AuditEvent::ToolCall {
            tool_name: tool_name.to_string(),
            params,
            result,
            timestamp_ms: Utc::now().timestamp_millis(),
            session_key: session_key.to_string(),
            trace_id,
            duration_ms,
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
        let log_file = self.current_log_file_path();
        
        // Ensure audit directory exists
        if let Some(parent) = log_file.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Open file in append mode
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)?;

        // Serialize event to JSON and write
        let json = serde_json::to_string(&event)?;
        writeln!(file, "{}", json)?;

        Ok(())
    }

    /// 获取当前日期对应的日志文件路径，并同步更新缓存的 current_date。
    fn current_log_file_path(&mut self) -> PathBuf {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        if today != self.current_date {
            self.current_date = today;
        }
        self.paths.audit_dir().join(format!("{}.jsonl", self.current_date))
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
            match serde_json::from_str::<AuditEvent>(line) {
                Ok(event) => events.push(event),
                Err(e) => {
                    error!(error = %e, line = %line, "Failed to parse audit event");
                }
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
}
