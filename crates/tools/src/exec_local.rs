use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::process::Command;

use crate::sandbox::{prepare_sandboxed, NativeSandboxProvider, SandboxPolicy, SandboxRequest};
use crate::{Tool, ToolContext, ToolSchema};

pub struct ExecLocalTool {
    policy: SandboxPolicy,
}

impl Default for ExecLocalTool {
    fn default() -> Self {
        Self::from_config(&blockcell_core::config::ExecConfig::default())
    }
}

pub(crate) const ALLOWED_RUNNERS: &[&str] =
    &["python3", "python", "bash", "sh", "node", "php", "uv"];

/// Python runner cache states.
/// 0 = not detected yet
/// 1 = python3 worked (cached)
/// 2 = python worked (cached)
const PYTHON_STATE_UNKNOWN: u8 = 0;
const PYTHON_STATE_PYTHON3: u8 = 1;
const PYTHON_STATE_PYTHON: u8 = 2;

static PYTHON_RUNNER_STATE: AtomicU8 = AtomicU8::new(PYTHON_STATE_UNKNOWN);

/// Preferred order to try Python runners.
const PYTHON_PREFERS: [&str; 2] = ["python3", "python"];

/// Get cached Python runner if available.
fn get_cached_python_runner() -> Option<&'static str> {
    match PYTHON_RUNNER_STATE.load(Ordering::Relaxed) {
        PYTHON_STATE_PYTHON3 => Some("python3"),
        PYTHON_STATE_PYTHON => Some("python"),
        _ => None,
    }
}

/// Cache the successful Python runner.
fn cache_python_runner(runner: &str) {
    let state = if runner == "python3" {
        PYTHON_STATE_PYTHON3
    } else {
        PYTHON_STATE_PYTHON
    };
    PYTHON_RUNNER_STATE.store(state, Ordering::Relaxed);
    tracing::debug!("Cached Python runner: {}", runner);
}

/// Clear the cached Python runner (for retry after failure).
fn clear_python_runner_cache() {
    PYTHON_RUNNER_STATE.store(PYTHON_STATE_UNKNOWN, Ordering::Relaxed);
    tracing::debug!("Cleared Python runner cache");
}

pub(crate) fn validate_relative_skill_path(path: &str) -> Result<()> {
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

pub(crate) fn validate_runner(runner: &str) -> Result<()> {
    if ALLOWED_RUNNERS.contains(&runner) {
        Ok(())
    } else {
        Err(Error::PermissionDenied(format!(
            "Runner '{}' is not allowed for exec_local",
            runner
        )))
    }
}

/// Infer an appropriate runner from the script file extension when none is specified.
/// Returns `None` if the extension is unknown (caller should attempt direct execution).
/// For Python scripts, uses cached runner if available, otherwise returns "python3" as first try.
pub(crate) fn infer_runner_from_extension(path: &str) -> Option<&'static str> {
    if path.ends_with(".py") {
        // Use cached runner if available, otherwise default to python3 (first in prefer list)
        get_cached_python_runner().or(Some("python3"))
    } else if path.ends_with(".sh") {
        Some("sh")
    } else if path.ends_with(".js") || path.ends_with(".mjs") {
        Some("node")
    } else if path.ends_with(".php") {
        Some("php")
    } else {
        None
    }
}

pub fn script_runner_description(params: &Value, path: &Path) -> String {
    params
        .get("runner")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            infer_runner_from_extension(&path.to_string_lossy())
                .unwrap_or("direct executable")
                .to_string()
        })
}

pub(crate) fn truncate_output(text: String, max_chars: usize, suffix: &str) -> String {
    if text.chars().count() <= max_chars {
        return text;
    }

    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => format!("{}\n{}", &text[..idx], suffix),
        None => text,
    }
}

pub fn resolve_script_path(skill_dir: &Path, relative_path: &str) -> Result<PathBuf> {
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

    // Strip \\?\ prefix on Windows for command-line compatibility.
    // canonicalize() adds this prefix on Windows for long path support,
    // but it causes issues when passed to subprocess via command line.
    #[cfg(windows)]
    {
        let path_str = canonical_target.to_string_lossy();
        if let Some(stripped) = path_str.strip_prefix("\\\\?\\") {
            return Ok(PathBuf::from(stripped));
        }
    }

    Ok(canonical_target)
}

/// Build a Python command with proper UTF-8 handling.
fn build_python_command(runner: &str, script_path: &Path, args: &[String]) -> Command {
    let mut command = Command::new(runner);
    // On Windows, add -X utf8 flag for proper UTF-8 output handling
    #[cfg(windows)]
    {
        command.arg("-X");
        command.arg("utf8");
    }
    command.arg(script_path);
    command.args(args);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUNBUFFERED", "1");
    command
}

#[async_trait]
impl Tool for ExecLocalTool {
    fn command_operation(&self, _params: &Value) -> crate::evidence::CommandOperation {
        crate::evidence::CommandOperation::Opaque
    }
    fn execution_environment(&self, _params: &Value) -> crate::ExecutionEnvironment {
        match self.policy {
            SandboxPolicy::FullAccess => crate::ExecutionEnvironment::FullAccess,
            _ => crate::ExecutionEnvironment::Shell,
        }
    }

    fn requires_confirmation(&self, _params: &Value) -> bool {
        self.policy == SandboxPolicy::FullAccess
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "exec_local".to_string(),
            description: "Execute a local script or executable inside the active skill directory only. The `path` must be RELATIVE (e.g. `scripts/run.py`), never absolute. If the skill manual shows `{baseDir}/scripts/...`, strip the `{baseDir}` prefix and pass only the relative portion.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "RELATIVE path to the script inside the active skill directory (e.g. `scripts/run.py`). Must NOT be absolute. The tool auto-infers the interpreter from the file extension (.py→python3/python with fallback, .sh→sh, .js→node)."
                    },
                    "runner": {
                        "type": "string",
                        "description": "Optional interpreter override. Allowed: python3, python, bash, sh, node, php, uv. Auto-inferred from extension when omitted."
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
        let skill_dir = ctx.active_skill_dir.clone().ok_or_else(|| {
            // Use Tool error (not PermissionDenied) to avoid Permanent classification.
            // Hint tells the LLM to use activate_skill first.
            Error::Tool(
                "exec_local requires an active skill context. \
                Use the `activate_skill` tool first, e.g. \
                activate_skill({skill_name: \"<skill-name>\", goal: \"<goal>\"})"
                    .to_string(),
            )
        })?;
        let relative_path = params["path"]
            .as_str()
            .ok_or_else(|| Error::Validation("Missing required parameter: path".to_string()))?;
        let resolved_path = resolve_script_path(&skill_dir, relative_path)?;
        if let Some(approved) = params
            .get("_approved_script_sha256")
            .and_then(Value::as_str)
        {
            let digest = format!("{:x}", Sha256::digest(std::fs::read(&resolved_path)?));
            if digest != approved {
                return Err(Error::PermissionDenied(
                    "Skill script content changed after approval; request confirmation again"
                        .into(),
                ));
            }
        }
        let explicit_runner = params.get("runner").and_then(|value| value.as_str());
        if let Some(runner) = explicit_runner {
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

        // Handle Python scripts with fallback mechanism
        if relative_path.ends_with(".py") && explicit_runner.is_none() {
            return self
                .execute_python_with_fallback(
                    &ctx,
                    &skill_dir,
                    &resolved_path,
                    &args,
                    timeout_secs,
                    max_output_chars,
                )
                .await;
        }

        // Non-Python scripts or explicit runner: execute directly
        let effective_runner =
            explicit_runner.or_else(|| infer_runner_from_extension(relative_path));
        self.execute_single_runner(
            &ctx,
            effective_runner,
            &skill_dir,
            &resolved_path,
            &args,
            timeout_secs,
            max_output_chars,
        )
        .await
    }
}

impl ExecLocalTool {
    pub fn from_config(config: &blockcell_core::config::ExecConfig) -> Self {
        Self {
            policy: crate::exec::ExecTool::resolve_config_policy(config)
                .unwrap_or(SandboxPolicy::WorkspaceWrite),
        }
    }

    /// Execute Python script with automatic fallback between python3 and python.
    #[allow(clippy::too_many_arguments)]
    async fn execute_python_with_fallback(
        &self,
        ctx: &ToolContext,
        skill_dir: &Path,
        resolved_path: &Path,
        args: &[String],
        timeout_secs: u64,
        max_output_chars: usize,
    ) -> Result<Value> {
        let mut runners = Vec::new();
        if let Some(runner) = get_cached_python_runner() {
            runners.push(runner);
        }
        for runner in PYTHON_PREFERS {
            if !runners.contains(&runner) {
                runners.push(runner);
            }
        }
        for runner in runners {
            // Resolve before execution. A nonzero exit or timeout can follow
            // side effects, so it must never trigger a second interpreter.
            let executable = match which::which(runner) {
                Ok(path) => path,
                Err(_) => {
                    clear_python_runner_cache();
                    continue;
                }
            };
            let executable = executable.to_string_lossy();
            let mut command = build_python_command(&executable, resolved_path, args);
            command.current_dir(skill_dir);
            let result = self
                .run_command(
                    ctx,
                    command,
                    resolved_path,
                    Some(runner),
                    args,
                    timeout_secs,
                    max_output_chars,
                )
                .await;
            if let Ok(value) = &result {
                if value["exit_code"].as_i64() == Some(0) {
                    cache_python_runner(runner);
                }
            }
            return result;
        }

        Err(Error::Tool(
            "No working Python interpreter found. Tried: python3, python".to_string(),
        ))
    }

    /// Execute script with a single runner (no fallback).
    #[allow(clippy::too_many_arguments)]
    async fn execute_single_runner(
        &self,
        ctx: &ToolContext,
        effective_runner: Option<&str>,
        skill_dir: &Path,
        resolved_path: &Path,
        args: &[String],
        timeout_secs: u64,
        max_output_chars: usize,
    ) -> Result<Value> {
        let mut command = if let Some(runner) = effective_runner {
            if runner == "uv" {
                // For `uv`, use `uv run -- python3 -X utf8 <script>`
                let mut command = Command::new("uv");
                command.arg("run");
                command.arg("--");
                command.arg("python3");
                command.arg("-X");
                command.arg("utf8");
                command.arg(resolved_path);
                command.args(args);
                command
            } else if runner == "python3" || runner == "python" {
                // Use specialized Python command builder
                let mut command = build_python_command(runner, resolved_path, args);
                command.current_dir(skill_dir);
                return self
                    .run_command(
                        ctx,
                        command,
                        resolved_path,
                        effective_runner,
                        args,
                        timeout_secs,
                        max_output_chars,
                    )
                    .await;
            } else {
                // Other runners (sh, bash, node, php)
                let mut command = Command::new(runner);
                command.arg(resolved_path);
                command.args(args);
                command
            }
        } else {
            // Direct execution (no runner)
            let mut command = Command::new(resolved_path);
            command.args(args);
            command
        };

        // Set common options for non-Python runners
        command
            .current_dir(skill_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUNBUFFERED", "1");

        self.run_command(
            ctx,
            command,
            resolved_path,
            effective_runner,
            args,
            timeout_secs,
            max_output_chars,
        )
        .await
    }

    fn sandbox_request(&self, ctx: &ToolContext, command: &Command) -> SandboxRequest {
        let original = command.as_std();
        let mut writable_roots = if self.policy == SandboxPolicy::WorkspaceWrite {
            ctx.sandbox_writable_roots
                .clone()
                .unwrap_or_else(|| vec![ctx.workspace.clone()])
        } else {
            Vec::new()
        };
        if self.policy == SandboxPolicy::WorkspaceWrite {
            if let Some(skill_dir) = &ctx.active_skill_dir {
                if !writable_roots.contains(skill_dir) {
                    writable_roots.push(skill_dir.clone());
                }
            }
        }
        SandboxRequest {
            agent_id: ctx
                .agent_identity
                .as_ref()
                .map(|identity| identity.agent_id.clone())
                .unwrap_or_default(),
            session_id: ctx.session_key.clone(),
            policy: self.policy,
            writable_roots,
            readonly_roots: Vec::new(),
            program: original.get_program().to_string_lossy().into_owned(),
            args: original
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
        }
    }

    /// Run the command and process output.
    #[allow(clippy::too_many_arguments)]
    async fn run_command(
        &self,
        ctx: &ToolContext,
        command: Command,
        resolved_path: &Path,
        effective_runner: Option<&str>,
        args: &[String],
        timeout_secs: u64,
        max_output_chars: usize,
    ) -> Result<Value> {
        let original = command.as_std();
        let mut handle =
            prepare_sandboxed(&NativeSandboxProvider, self.sandbox_request(ctx, &command)).await?;
        if let Some(cwd) = original.get_current_dir() {
            handle.command.current_dir(cwd);
        }
        for (key, value) in original.get_envs() {
            if let Some(value) = value {
                handle.command.env(key, value);
            } else {
                handle.command.env_remove(key);
            }
        }
        handle
            .command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = crate::process::run_command_bounded(
            handle.command,
            Duration::from_secs(timeout_secs),
            max_output_chars.saturating_mul(4),
        )
        .await?;

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

        // Build command_parts for logging/output
        let command_parts: Vec<String> = if effective_runner == Some("uv") {
            vec![
                "uv".to_string(),
                "run".to_string(),
                "--".to_string(),
                "python3".to_string(),
                "-X".to_string(),
                "utf8".to_string(),
                resolved_path.display().to_string(),
            ]
            .into_iter()
            .chain(args.iter().cloned())
            .collect()
        } else if effective_runner == Some("python3") || effective_runner == Some("python") {
            std::iter::once(effective_runner.map(str::to_string))
                .flatten()
                .chain(if cfg!(windows) {
                    vec!["-X".to_string(), "utf8".to_string()]
                } else {
                    vec![]
                })
                .chain(Some(resolved_path.display().to_string()))
                .chain(args.iter().cloned())
                .collect()
        } else {
            std::iter::once(effective_runner.map(str::to_string))
                .flatten()
                .chain(if effective_runner.is_some() {
                    Some(resolved_path.display().to_string())
                } else {
                    None
                })
                .chain(args.iter().cloned())
                .collect()
        };

        Ok(json!({
            "exit_code": output.status.code(),
            "stdout": stdout,
            "stderr": stderr,
            "command": command_parts.join(" "),
            "resolved_path": resolved_path.display().to_string(),
            "truncated": output.truncated,
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

    fn full_access_tool() -> ExecLocalTool {
        ExecLocalTool::from_config(&blockcell_core::config::ExecConfig {
            sandbox_policy: Some("full-access".into()),
            ..Default::default()
        })
    }

    #[test]
    fn script_sandbox_request_preserves_argv_policy_and_roots() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = tool_context(dir.path().to_path_buf());
        ctx.workspace = dir.path().join("workspace");
        let mut command = build_python_command(
            "python3",
            &dir.path().join("a script.py"),
            &["a; echo unsafe".into(), "two words".into()],
        );
        command.current_dir(dir.path());
        let tool = ExecLocalTool::default();
        let request = tool.sandbox_request(&ctx, &command);
        assert_eq!(request.policy, SandboxPolicy::WorkspaceWrite);
        assert_eq!(request.program, "python3");
        assert!(request.args.contains(&"a; echo unsafe".to_string()));
        assert!(request.args.contains(&"two words".to_string()));
        assert_eq!(
            request.writable_roots,
            vec![ctx.workspace.clone(), dir.path().to_path_buf()]
        );
        ctx.sandbox_writable_roots = Some(vec![dir.path().join("authorized")]);
        assert_eq!(
            tool.sandbox_request(&ctx, &command).writable_roots,
            vec![dir.path().join("authorized"), dir.path().to_path_buf()]
        );
        assert!(!tool.requires_confirmation(&json!({})));
        assert_eq!(
            tool.execution_environment(&json!({})),
            crate::ExecutionEnvironment::Shell
        );
        assert!(full_access_tool().requires_confirmation(&json!({})));
        assert!(full_access_tool()
            .sandbox_request(&ctx, &command)
            .writable_roots
            .is_empty());
        let readonly = ExecLocalTool::from_config(&blockcell_core::config::ExecConfig {
            sandbox_policy: Some("read-only".into()),
            ..Default::default()
        });
        assert!(readonly
            .sandbox_request(&ctx, &command)
            .writable_roots
            .is_empty());
    }

    #[tokio::test]
    async fn script_sandbox_unavailable_never_falls_back() {
        if crate::sandbox::native_sandbox_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.sh"), "echo ran > marker").unwrap();
        let result = ExecLocalTool::default()
            .execute(
                tool_context(dir.path().to_path_buf()),
                json!({"path":"test.sh"}),
            )
            .await;
        assert!(result.is_err());
        assert!(!dir.path().join("marker").exists());
        assert_eq!(
            crate::sandbox::native_backend(SandboxPolicy::WorkspaceWrite),
            crate::sandbox::SandboxBackend::ApprovalRequired
        );
    }

    #[tokio::test]
    async fn python_special_environment_and_changed_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.py");
        std::fs::write(&path, "import os,sys\nprint(os.environ['PYTHONIOENCODING'], os.environ['PYTHONUNBUFFERED'], sys.argv[1])\n").unwrap();
        let params = json!({"path":"test.py", "runner":"python3", "args":["a; echo unsafe"]});
        let result = full_access_tool()
            .execute(tool_context(dir.path().to_path_buf()), params.clone())
            .await
            .unwrap();
        assert_eq!(result["stdout"], "utf-8 1 a; echo unsafe\n");
        let mut params = params;
        params["_approved_script_sha256"] = json!("not-the-script-digest");
        let err = full_access_tool()
            .execute(tool_context(dir.path().to_path_buf()), params)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("changed after approval"));
    }

    fn tool_context(skill_dir: PathBuf) -> ToolContext {
        ToolContext {
            workspace: std::env::temp_dir(),
            sandbox_writable_roots: None,
            base: std::env::temp_dir().join("blockcell"),
            builtin_skills_dir: None,
            active_skill_dir: Some(skill_dir),
            session_key: "cli:test".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: None,
            sender_role: None,
            chat_id: "chat-1".to_string(),
            config: Config::default(),
            permissions: blockcell_core::types::PermissionSet::new(),
            task_manager: None,
            memory_store: None,
            memory_file_store: None,
            ghost_memory_lifecycle: None,
            skill_file_store: None,
            session_search: None,
            outbound_tx: None,
            spawn_handle: None,
            capability_registry: None,
            core_evolution: None,
            event_emitter: None,
            channel_contacts_file: None,
            response_cache: None,
            runtime_handle: None,
            agent_identity: None,
            skill_mutex: None,
            agent_type_registry: None,
            evolution_workflow_store: None,
        }
    }

    #[cfg(not(target_os = "windows"))]
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

        let tool = full_access_tool();
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
        assert!(result["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("hello world"));
        assert_eq!(
            result["resolved_path"].as_str(),
            Some(expected_path.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn test_exec_local_runs_top_level_skill_py() {
        // Clear cache for test isolation
        clear_python_runner_cache();

        let skill_dir = temp_skill_dir("blockcell-exec-local-skill-py");
        let script_path = skill_dir.join("SKILL.py");
        fs::write(
            &script_path,
            "import sys\nprint('py:' + '-'.join(sys.argv[1:]))\n",
        )
        .expect("write skill py");

        let tool = full_access_tool();
        let result = tool
            .execute(
                tool_context(skill_dir.clone()),
                json!({
                    "path": "SKILL.py",
                    "args": ["demo", "local"],
                    "cwd_mode": "skill"
                }),
            )
            .await;

        // 当前环境没有 Python 时跳过（不视为测试失败）
        let value = match result {
            Ok(v) => v,
            Err(e) => {
                let msg = format!("{}", e);
                if msg.contains("No working Python interpreter found") {
                    eprintln!("跳过: 当前环境无 Python 解释器");
                    return;
                }
                panic!("exec_local unexpected error: {}", e);
            }
        };

        let expected_path = script_path.canonicalize().expect("canonical path");

        assert_eq!(value["exit_code"].as_i64(), Some(0));
        assert!(value["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("py:demo-local"));
        assert_eq!(
            value["resolved_path"].as_str(),
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

        let tool = full_access_tool();
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
        let tool = full_access_tool();

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

    #[tokio::test]
    async fn python_timeout_does_not_repeat_script_side_effects() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("timeout.py");
        std::fs::write(
            &script,
            "with open('calls', 'a') as f: f.write('called\\n')\nimport time\ntime.sleep(10)\n",
        )
        .unwrap();
        let result = full_access_tool()
            // 1s was too tight: interpreter startup alone costs ~0.8s on a loaded
            // host, so the child could be killed before its first statement ran and
            // the read below failed with ENOENT. The script sleeps 10s, so 5s still
            // exercises the timeout path (same fix as the cron test timeout).
            .execute_python_with_fallback(
                &tool_context(dir.path().to_path_buf()),
                dir.path(),
                &script,
                &[],
                5,
                4096,
            )
            .await;
        assert!(matches!(result, Err(Error::Timeout(_))), "{result:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls")).unwrap(),
            "called\n"
        );
    }

    #[tokio::test]
    async fn python_import_error_keeps_original_result_without_reexecution() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("failure.py");
        std::fs::write(&script, "with open('calls', 'a') as f: f.write('called\\n')\nimport blockcell_nonexistent_p2_dependency\n").unwrap();
        let value = full_access_tool()
            .execute_python_with_fallback(
                &tool_context(dir.path().to_path_buf()),
                dir.path(),
                &script,
                &[],
                5,
                4096,
            )
            .await
            .unwrap();
        assert_ne!(value["exit_code"], 0);
        assert!(value["stderr"]
            .as_str()
            .unwrap()
            .contains("ModuleNotFoundError"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("calls")).unwrap(),
            "called\n"
        );
    }

    #[test]
    fn test_python_runner_cache_operations() {
        // Clear cache
        clear_python_runner_cache();
        assert_eq!(get_cached_python_runner(), None);

        // Cache python3
        cache_python_runner("python3");
        assert_eq!(get_cached_python_runner(), Some("python3"));

        // Cache python (overwrites)
        cache_python_runner("python");
        assert_eq!(get_cached_python_runner(), Some("python"));

        // Clear again
        clear_python_runner_cache();
        assert_eq!(get_cached_python_runner(), None);
    }
}
