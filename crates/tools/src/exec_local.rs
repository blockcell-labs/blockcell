use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

use crate::{Tool, ToolContext, ToolSchema};

pub struct ExecLocalTool;

const ALLOWED_RUNNERS: &[&str] = &["python3", "bash", "sh", "node", "php"];

fn validate_relative_skill_path(path: &str) -> Result<()> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(Error::Validation(
            "Missing required parameter: path".to_string(),
        ));
    }

    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return Err(Error::Validation(
            "`path` must be relative to the active skill directory".to_string(),
        ));
    }

    if candidate
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(Error::PermissionDenied(
            "`path` cannot escape the active skill directory".to_string(),
        ));
    }

    Ok(())
}

fn validate_runner(runner: &str) -> Result<()> {
    if ALLOWED_RUNNERS.contains(&runner) {
        Ok(())
    } else {
        Err(Error::PermissionDenied(format!(
            "Runner '{}' is not allowed for exec_local",
            runner
        )))
    }
}

fn truncate_output(text: String, max_chars: usize, suffix: &str) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }

    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => format!("{}\n{}", &text[..idx], suffix),
        None => text,
    }
}

fn resolve_script_path(skill_dir: &Path, relative_path: &str) -> Result<PathBuf> {
    validate_relative_skill_path(relative_path)?;
    let joined = skill_dir.join(relative_path);
    let canonical_skill_dir = std::fs::canonicalize(skill_dir)?;
    let canonical_target = std::fs::canonicalize(&joined)
        .map_err(|_| Error::NotFound(format!("Local script '{}' not found", relative_path)))?;

    if !canonical_target.starts_with(&canonical_skill_dir) {
        return Err(Error::PermissionDenied(
            "Resolved script path is outside the active skill directory".to_string(),
        ));
    }

    Ok(canonical_target)
}

#[async_trait]
impl Tool for ExecLocalTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "exec_local",
            description:
                "Execute a local script or executable inside the active skill directory only.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the script or executable inside the active skill directory."
                    },
                    "runner": {
                        "type": "string",
                        "description": "Optional interpreter or runner. Allowed: python3, bash, sh, node, php."
                    },
                    "args": {
                        "type": "array",
                        "description": "Arguments passed to the script or executable.",
                        "items": {
                            "type": "string"
                        }
                    },
                    "cwd_mode": {
                        "type": "string",
                        "description": "Working directory mode. Only `skill` is supported.",
                        "enum": ["skill"]
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let path = params
            .get("path")
            .and_then(|value| value.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: path".to_string()))?;
        validate_relative_skill_path(path)?;

        if let Some(runner) = params.get("runner").and_then(|value| value.as_str()) {
            validate_runner(runner)?;
        }

        if let Some(args) = params.get("args").and_then(|value| value.as_array()) {
            if args.iter().any(|value| value.as_str().is_none()) {
                return Err(Error::Validation(
                    "`args` must be an array of strings".to_string(),
                ));
            }
        }

        if let Some(cwd_mode) = params.get("cwd_mode").and_then(|value| value.as_str()) {
            if cwd_mode != "skill" {
                return Err(Error::Validation(
                    "`cwd_mode` only supports `skill`".to_string(),
                ));
            }
        }

        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let skill_dir = ctx.active_skill_dir.ok_or_else(|| {
            Error::PermissionDenied(
                "`exec_local` is only available inside an active skill execution scope".to_string(),
            )
        })?;
        let relative_path = params["path"].as_str().ok_or_else(|| {
            Error::Validation("Missing required parameter: path".to_string())
        })?;
        let resolved_path = resolve_script_path(&skill_dir, relative_path)?;
        let runner = params.get("runner").and_then(|value| value.as_str());
        if let Some(runner) = runner {
            validate_runner(runner)?;
        }
        let args = params
            .get("args")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|value| {
                value.as_str().map(str::to_string).ok_or_else(|| {
                    Error::Validation("`args` must be an array of strings".to_string())
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let cwd_mode = params
            .get("cwd_mode")
            .and_then(|value| value.as_str())
            .unwrap_or("skill");
        if cwd_mode != "skill" {
            return Err(Error::Validation(
                "`cwd_mode` only supports `skill`".to_string(),
            ));
        }

        let timeout_secs = ctx.config.tools.exec.timeout as u64;
        let max_output_chars = 10_000usize;

        let mut command = if let Some(runner) = runner {
            let mut command = Command::new(runner);
            command.arg(&resolved_path);
            command
        } else {
            Command::new(&resolved_path)
        };
        command
            .args(&args)
            .current_dir(&skill_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = timeout(Duration::from_secs(timeout_secs), command.output())
            .await
            .map_err(|_| {
                Error::Timeout(format!(
                    "Local script timed out after {} seconds",
                    timeout_secs
                ))
            })?
            .map_err(|error| Error::Tool(format!("Failed to execute local script: {}", error)))?;

        let stdout = truncate_output(
            String::from_utf8_lossy(&output.stdout).to_string(),
            max_output_chars,
            "... (stdout truncated)",
        );
        let stderr = truncate_output(
            String::from_utf8_lossy(&output.stderr).to_string(),
            max_output_chars,
            "... (stderr truncated)",
        );

        let command_parts = std::iter::once(
            runner
                .map(str::to_string)
                .unwrap_or_else(|| resolved_path.display().to_string()),
        )
        .chain(if runner.is_some() {
            Some(resolved_path.display().to_string())
        } else {
            None
        })
        .chain(args.iter().cloned())
        .collect::<Vec<_>>();

        Ok(json!({
            "exit_code": output.status.code(),
            "stdout": stdout,
            "stderr": stderr,
            "command": command_parts.join(" "),
            "resolved_path": resolved_path.display().to_string(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::Config;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn temp_skill_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{}-{}", prefix, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp skill dir");
        dir
    }

    fn tool_context(skill_dir: PathBuf) -> ToolContext {
        ToolContext {
            workspace: std::env::temp_dir(),
            builtin_skills_dir: None,
            active_skill_dir: Some(skill_dir),
            session_key: "cli:test".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: None,
            chat_id: "chat-1".to_string(),
            config: Config::default(),
            permissions: blockcell_core::types::PermissionSet::new(),
            task_manager: None,
            memory_store: None,
            outbound_tx: None,
            spawn_handle: None,
            capability_registry: None,
            core_evolution: None,
            event_emitter: None,
            channel_contacts_file: None,
            response_cache: None,
        }
    }

    #[tokio::test]
    async fn test_exec_local_runs_skill_relative_script() {
        let skill_dir = temp_skill_dir("blockcell-exec-local");
        let scripts_dir = skill_dir.join("scripts");
        fs::create_dir_all(&scripts_dir).expect("create scripts dir");
        let script_path = scripts_dir.join("hello.sh");
        fs::write(&script_path, "#!/bin/sh\necho \"hello $1\"\n").expect("write script");
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&script_path)
                .expect("script metadata")
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script_path, perms).expect("set script perms");
        }

        let tool = ExecLocalTool;
        let result = tool
            .execute(
                tool_context(skill_dir.clone()),
                json!({
                    "path": "scripts/hello.sh",
                    "runner": "sh",
                    "args": ["world"],
                    "cwd_mode": "skill"
                }),
            )
            .await
            .expect("exec_local should succeed");
        let expected_path = script_path.canonicalize().expect("canonical path");

        assert_eq!(result["exit_code"].as_i64(), Some(0));
        assert!(result["stdout"].as_str().unwrap_or_default().contains("hello world"));
        assert_eq!(
            result["resolved_path"].as_str(),
            Some(expected_path.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn test_exec_local_blocks_parent_path_escape() {
        let skill_dir = temp_skill_dir("blockcell-exec-local-escape");
        let outside_path = skill_dir
            .parent()
            .expect("skill dir parent")
            .join("outside.sh");
        fs::write(&outside_path, "#!/bin/sh\necho escape\n").expect("write outside script");

        let tool = ExecLocalTool;
        let result = tool
            .execute(
                tool_context(skill_dir),
                json!({
                    "path": "../outside.sh",
                    "runner": "sh"
                }),
            )
            .await;

        assert!(result.is_err());
        assert!(format!("{}", result.expect_err("should fail")).contains("skill directory"));
    }

    #[test]
    fn test_exec_local_allows_whitelisted_runners_only() {
        let tool = ExecLocalTool;

        assert!(tool
            .validate(&json!({
                "path": "scripts/run.py",
                "runner": "python3"
            }))
            .is_ok());
        assert!(tool
            .validate(&json!({
                "path": "scripts/run.py",
                "runner": "perl"
            }))
            .is_err());
    }
}
