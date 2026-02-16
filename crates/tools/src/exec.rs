use async_trait::async_trait;
use blockcell_core::{Error, Result};
use regex::Regex;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

use crate::{Tool, ToolContext, ToolSchema};

pub struct ExecTool;

const DENY_PATTERNS: &[&str] = &[
    r"rm\s+-rf\s+/",
    r"rm\s+-rf\s+~",
    r"rm\s+-rf\s+\*",
    r"\bdd\b.*\bif=",
    r"\bformat\b",
    r"\bshutdown\b",
    r"\breboot\b",
    r":\(\)\s*\{\s*:\|:\s*&\s*\}\s*;",  // fork bomb
    r">\s*/dev/sd",
    r"mkfs\.",
];

fn is_dangerous_command(command: &str) -> bool {
    for pattern in DENY_PATTERNS {
        if let Ok(re) = Regex::new(pattern) {
            if re.is_match(command) {
                return true;
            }
        }
    }
    false
}

#[async_trait]
impl Tool for ExecTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "exec",
            description: "Execute a shell command",
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command to execute"
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Working directory for the command (optional)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: command".to_string()))?;

        if is_dangerous_command(command) {
            return Err(Error::PermissionDenied(
                "Command matches dangerous pattern and is blocked".to_string(),
            ));
        }

        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let command = params["command"].as_str().unwrap();
        let working_dir = params
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(|s| {
                if s.starts_with("~/") {
                    dirs::home_dir()
                        .map(|h| h.join(&s[2..]))
                        .unwrap_or_else(|| std::path::PathBuf::from(s))
                } else if s.starts_with('/') {
                    std::path::PathBuf::from(s)
                } else {
                    ctx.workspace.join(s)
                }
            })
            .unwrap_or_else(|| ctx.workspace.clone());

        let timeout_secs = ctx.config.tools.exec.timeout as u64;
        let max_output_chars = 10000;

        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let result = timeout(Duration::from_secs(timeout_secs), cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();

                // Truncate if too long (use safe_truncate to avoid panic on multi-byte chars)
                let mut truncated = false;
                if stdout.len() > max_output_chars {
                    stdout = format!("{}\n... (output truncated)", crate::safe_truncate(&stdout, max_output_chars));
                    truncated = true;
                }
                if stderr.len() > max_output_chars {
                    stderr = format!("{}\n... (output truncated)", crate::safe_truncate(&stderr, max_output_chars));
                    truncated = true;
                }

                Ok(json!({
                    "exit_code": output.status.code(),
                    "stdout": stdout,
                    "stderr": stderr,
                    "truncated": truncated
                }))
            }
            Ok(Err(e)) => Err(Error::Tool(format!("Failed to execute command: {}", e))),
            Err(_) => Err(Error::Timeout(format!(
                "Command timed out after {} seconds",
                timeout_secs
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_exec_schema() {
        let tool = ExecTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "exec");
    }

    #[test]
    fn test_exec_validate_ok() {
        let tool = ExecTool;
        assert!(tool.validate(&json!({"command": "ls -la"})).is_ok());
    }

    #[test]
    fn test_exec_validate_missing_command() {
        let tool = ExecTool;
        assert!(tool.validate(&json!({})).is_err());
    }

    #[test]
    fn test_exec_validate_deny_rm_rf() {
        let tool = ExecTool;
        assert!(tool.validate(&json!({"command": "rm -rf /"})).is_err());
    }

    #[test]
    fn test_exec_validate_deny_mkfs() {
        let tool = ExecTool;
        assert!(tool.validate(&json!({"command": "mkfs.ext4 /dev/sda"})).is_err());
    }

    #[test]
    fn test_exec_validate_deny_dd() {
        let tool = ExecTool;
        assert!(tool.validate(&json!({"command": "dd if=/dev/zero of=/dev/sda"})).is_err());
    }
}
