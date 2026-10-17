use async_trait::async_trait;
use blockcell_core::{Error, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

use crate::sandbox::SandboxPolicy;
use crate::shell_executor::{LocalShellExecutor, ShellExecutor, ShellRequest};
use crate::{Tool, ToolContext, ToolSchema};

/// 一次性命令执行。执行位置由注入的 [`ShellExecutor`] 决定，工具本身只描述命令。
///
/// 沙箱策略在构造时固定（`with_policy` / `from_config`），运行期不可变：
/// `execution_environment` 声明与实际执行永远一致，审计记录不会漂移。
pub struct ExecTool {
    executor: Arc<dyn ShellExecutor>,
    policy: SandboxPolicy,
}

impl ExecTool {
    pub fn new(executor: Arc<dyn ShellExecutor>) -> Self {
        Self::with_policy(executor, SandboxPolicy::WorkspaceWrite)
    }

    pub fn with_policy(executor: Arc<dyn ShellExecutor>, policy: SandboxPolicy) -> Self {
        Self { executor, policy }
    }

    pub fn from_config(config: &blockcell_core::config::ExecConfig) -> Self {
        let policy = Self::resolve_config_policy(config).unwrap_or(SandboxPolicy::WorkspaceWrite);
        Self::with_policy(Arc::new(LocalShellExecutor::default()), policy)
    }

    pub fn resolve_config_policy(
        config: &blockcell_core::config::ExecConfig,
    ) -> Result<SandboxPolicy> {
        if let Some(raw_policy) = config.sandbox_policy.as_deref() {
            SandboxPolicy::parse(Some(raw_policy))
        } else if config.restrict_to_workspace {
            Ok(SandboxPolicy::WorkspaceWrite)
        } else {
            Ok(SandboxPolicy::FullAccess)
        }
    }

    fn current_policy(&self) -> SandboxPolicy {
        self.policy
    }
}

impl Default for ExecTool {
    fn default() -> Self {
        Self::new(Arc::new(LocalShellExecutor::default()))
    }
}

/// Static destructive-command patterns, compiled once. Matched against a
/// whitespace-normalized, case-insensitive form of the command.
static DENY_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    [
        r"(?i)\bdd\b.*\bof=/dev/",                  // dd writing to a block device
        r"(?i)\bdd\b.*\bif=/dev/(sd|hd|nvme|disk)", // dd reading a raw disk
        r"(?i)\bmkfs(\.|\b)",                       // make filesystem
        r"(?i)\bshutdown\b",
        r"(?i)\breboot\b",
        r"(?i)\bhalt\b",
        r"(?i)\bformat\s+([a-z]:|/dev/)", // format a drive/partition
        r":\(\)\s*\{\s*:\|:\s*&\s*\}\s*;", // fork bomb
        r"(?i)>\s*/dev/(sd|hd|nvme|disk|mapper)", // overwrite a block device
        r"(?i)\bwipefs\b",
    ]
    .iter()
    .filter_map(|p| Regex::new(p).ok())
    .collect()
});

/// Collapse runs of whitespace into a single space so that flag spacing /
/// indentation cannot be used to dodge the patterns.
fn normalize_whitespace(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a `rm` target argument refers to the filesystem root, the home
/// directory, or a top-level glob — i.e. a catastrophic recursive delete.
fn is_root_like_target(target: &str) -> bool {
    // Strip quoting plus command-substitution/continuation closers so targets
    // smuggled through `$(rm -rf /)` or backticks still resolve to the root.
    let t = target.trim_matches(['"', '\'', '`', ')', ';', '|', '&']);
    matches!(
        t,
        "/" | "//" | "/./" | "/*" | "*" | "~" | "~/" | "~/*" | "$HOME" | "$HOME/" | "$HOME/*"
            | "." | "./" | "./*" | ".."
    ) || t.ends_with("/..")
}

/// Detect `rm` invocations that combine a recursive flag with a root-like
/// target, regardless of flag ordering or short/long form
/// (`rm -rf /`, `rm -fr /`, `rm -r -f /`, `rm --recursive --force /`, `rm -rf ~`, ...).
fn is_dangerous_rm(normalized: &str) -> bool {
    let tokens: Vec<&str> = normalized.split(' ').collect();
    let mut i = 0;
    while i < tokens.len() {
        // Strip shell quoting/escaping/substitution prefixes: `\rm`, `$(rm`,
        // `` `rm `` and `$(\rm` must all resolve to `rm`.
        let base = tokens[i]
            .rsplit('/')
            .next()
            .unwrap_or(tokens[i])
            .trim_start_matches(['\\', '$', '(', '`', '"', '\'']);
        if base != "rm" {
            i += 1;
            continue;
        }
        let mut recursive = false;
        let mut targets: Vec<&str> = Vec::new();
        let mut j = i + 1;
        while j < tokens.len() {
            let tok = tokens[j];
            if matches!(tok, "&&" | "||" | ";" | "|") {
                break;
            }
            if tok == "--recursive" {
                recursive = true;
            } else if tok.starts_with("--") {
                // other long option, ignore
            } else if let Some(flags) = tok.strip_prefix('-') {
                if flags.contains('r') || flags.contains('R') {
                    recursive = true;
                }
            } else {
                targets.push(tok);
            }
            j += 1;
        }
        if recursive && targets.iter().any(|t| is_root_like_target(t)) {
            return true;
        }
        i = j;
    }
    false
}

pub(crate) fn is_dangerous_command(command: &str) -> bool {
    let normalized = normalize_whitespace(command);
    if is_dangerous_rm(&normalized) {
        return true;
    }
    DENY_PATTERNS.iter().any(|re| re.is_match(&normalized))
}

#[async_trait]
impl Tool for ExecTool {
    /// 根据构造时固定的沙箱策略如实声明执行环境：`WorkspaceWrite` / `ReadOnly` 声明为
    /// `Shell`，`FullAccess` 声明为 `FullAccess`。`exec` 不接受 per-call 的
    /// `sandbox_policy` 参数——声明、审计与实际执行同源，不会被参数伪造。
    fn execution_environment(&self, _params: &Value) -> crate::ExecutionEnvironment {
        match self.current_policy() {
            SandboxPolicy::FullAccess => crate::ExecutionEnvironment::FullAccess,
            SandboxPolicy::ReadOnly | SandboxPolicy::WorkspaceWrite => {
                crate::ExecutionEnvironment::Shell
            }
        }
    }

    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Required
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "exec".to_string(),
            description: "Execute a shell command".to_string(),
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

        // `exec` 的沙箱策略只能由配置决定，不接受 per-call 覆盖——fail-closed 拒绝，
        // 避免参数伪造让声明/审计显示与实际策略不一致的 execution environment。
        if params.get("sandbox_policy").is_some() {
            return Err(Error::Validation(
                "exec does not accept a per-call sandbox_policy; it is fixed by tools.exec config"
                    .to_string(),
            ));
        }

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

        // 默认按 WorkspaceWrite 沙箱运行，防止通过 `exec` 绕过 `shell` 的 OS 沙箱限制；
        // 策略在构造时（from_config / with_policy）固定，声明与执行使用同一份值。
        let policy = self.current_policy();
        let writable_roots = if policy == SandboxPolicy::WorkspaceWrite {
            vec![ctx.workspace.clone()]
        } else {
            Vec::new()
        };

        let result = self
            .executor
            .execute(ShellRequest {
                agent_id: ctx
                    .agent_identity
                    .as_ref()
                    .map(|identity| identity.agent_id.clone())
                    .unwrap_or_default(),
                session_id: ctx.session_key.clone(),
                command: command.to_string(),
                working_dir,
                policy,
                writable_roots,
                timeout: Duration::from_secs(timeout_secs),
                max_output_bytes: max_output_chars * 4,
            })
            .await;

        match result {
            Ok(output) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();

                // Truncate if too long (use safe_truncate to avoid panic on multi-byte chars)
                let mut truncated = output.truncated;
                if stdout.len() > max_output_chars {
                    stdout = format!(
                        "{}\n... (output truncated)",
                        crate::safe_truncate(&stdout, max_output_chars)
                    );
                    truncated = true;
                }
                if stderr.len() > max_output_chars {
                    stderr = format!(
                        "{}\n... (output truncated)",
                        crate::safe_truncate(&stderr, max_output_chars)
                    );
                    truncated = true;
                }

                Ok(json!({
                    "exit_code": output.exit_code,
                    "stdout": stdout,
                    "stderr": stderr,
                    "truncated": truncated
                }))
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_exec_schema() {
        let tool = ExecTool::default();
        let schema = tool.schema();
        assert_eq!(schema.name, "exec");
    }

    #[test]
    fn test_exec_validate_ok() {
        let tool = ExecTool::default();
        assert!(tool.validate(&json!({"command": "ls -la"})).is_ok());
    }

    #[test]
    fn test_exec_validate_missing_command() {
        let tool = ExecTool::default();
        assert!(tool.validate(&json!({})).is_err());
    }

    #[test]
    fn test_exec_validate_deny_rm_rf() {
        let tool = ExecTool::default();
        assert!(tool.validate(&json!({"command": "rm -rf /"})).is_err());
    }

    #[test]
    fn test_exec_validate_deny_mkfs() {
        let tool = ExecTool::default();
        assert!(tool
            .validate(&json!({"command": "mkfs.ext4 /dev/sda"}))
            .is_err());
    }

    #[test]
    fn test_exec_validate_deny_dd() {
        let tool = ExecTool::default();
        assert!(tool
            .validate(&json!({"command": "dd if=/dev/zero of=/dev/sda"}))
            .is_err());
    }

    #[test]
    fn test_exec_validate_deny_rm_variants() {
        let tool = ExecTool::default();
        // Flag reordering / separate flags / long form / extra whitespace
        for cmd in [
            "rm -fr /",
            "rm -r -f /",
            "rm -f -r /",
            "rm --recursive --force /",
            "rm   -rf    /",
            "rm -rf ~",
            "rm -rf $HOME",
            "rm -rf /*",
            "rm -rf *",
            "/bin/rm -rf /",
            "echo hi && rm -rf /",
            // root-equivalent spellings: `//`, `/./` resolve to `/`;
            // a `..` tail climbs to a parent (e.g. `/tmp/..` → `/`)
            "rm -rf //",
            "rm -rf /./",
            "rm -rf /tmp/..",
            "rm -rf ..",
            "rm -rf ~/..",
            "rm -rf $HOME/..",
            "rm -rf ../..",
        ] {
            assert!(
                tool.validate(&json!({ "command": cmd })).is_err(),
                "expected `{cmd}` to be blocked"
            );
        }
    }

    #[test]
    fn test_exec_validate_deny_rm_obfuscated() {
        let tool = ExecTool::default();
        // Command-name confusion: escaping, command substitution, backticks —
        // all must still resolve to `rm` and match root-like targets.
        for cmd in [
            "\\rm -rf //",
            "$(rm -rf /)",
            "`rm -rf /`",
            "sudo \\rm -rf /",
            "$(\\rm -rf ~)",
            "echo hi && $(rm -rf /)",
        ] {
            assert!(
                tool.validate(&json!({ "command": cmd })).is_err(),
                "expected `{cmd}` to be blocked"
            );
        }
    }

    #[test]
    fn test_exec_validate_allows_safe_rm() {
        let tool = ExecTool::default();
        // Recursive delete of a specific project path is allowed by the denylist
        // (path policy still governs where it may run).
        for cmd in [
            "rm -rf ./build",
            "rm -rf target/debug",
            "rm file.txt",
            "rm -r src/old",
        ] {
            assert!(
                tool.validate(&json!({ "command": cmd })).is_ok(),
                "expected `{cmd}` to be allowed"
            );
        }
    }

    #[test]
    fn test_exec_validate_deny_destructive_misc() {
        let tool = ExecTool::default();
        for cmd in [
            "shutdown -h now",
            "REBOOT",
            "mkfs.ext4 /dev/sdb1",
            "wipefs -a /dev/sda",
            "dd if=/dev/zero of=/dev/sda bs=1M",
            "cat x > /dev/sda",
        ] {
            assert!(
                tool.validate(&json!({ "command": cmd })).is_err(),
                "expected `{cmd}` to be blocked"
            );
        }
    }

    #[test]
    fn test_exec_default_policy_is_workspace_write_and_matches_environment() {
        let default_cfg = blockcell_core::config::ExecConfig::default();
        assert!(default_cfg.restrict_to_workspace);
        assert_eq!(
            ExecTool::resolve_config_policy(&default_cfg).unwrap(),
            SandboxPolicy::WorkspaceWrite
        );

        let tool = ExecTool::default();
        assert_eq!(
            tool.execution_environment(&json!({"command": "ls"})),
            crate::ExecutionEnvironment::Shell
        );

        let unrestricted_cfg = blockcell_core::config::ExecConfig {
            timeout: 60,
            restrict_to_workspace: false,
            sandbox_policy: None,
        };
        let unrestricted_tool = ExecTool::from_config(&unrestricted_cfg);
        assert_eq!(
            unrestricted_tool.execution_environment(&json!({"command": "ls"})),
            crate::ExecutionEnvironment::FullAccess
        );

        let readonly_cfg = blockcell_core::config::ExecConfig {
            timeout: 60,
            restrict_to_workspace: false,
            sandbox_policy: Some("read-only".to_string()),
        };
        let readonly_tool = ExecTool::from_config(&readonly_cfg);
        assert_eq!(
            readonly_tool.execution_environment(&json!({"command": "ls"})),
            crate::ExecutionEnvironment::Shell
        );
    }

    #[test]
    fn test_exec_rejects_per_call_sandbox_policy_and_declares_fixed_policy() {
        let tool = ExecTool::default();
        // per-call sandbox_policy 参数被拒（fail-closed），不能伪造声明
        assert!(tool
            .validate(&json!({"command": "ls", "sandbox_policy": "full-access"}))
            .is_err());
        // 即便参数被 listener 注入，声明也只看构造时策略，与实际执行同源
        assert_eq!(
            tool.execution_environment(&json!({"command": "ls", "sandbox_policy": "full-access"})),
            crate::ExecutionEnvironment::Shell
        );

        let full = ExecTool::with_policy(
            std::sync::Arc::new(LocalShellExecutor::default()),
            SandboxPolicy::FullAccess,
        );
        assert_eq!(
            full.execution_environment(&json!({"command": "ls"})),
            crate::ExecutionEnvironment::FullAccess
        );
    }
}
