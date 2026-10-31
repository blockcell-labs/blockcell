use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use glob::Pattern;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;
use tracing::{info, warn};

use crate::runtime_kernel::{
    AgentEvent, AgentEventContext, AgentEventListener, EventDecision, ListenerSpec,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    SessionStart,
    SessionEnd,
    #[default]
    UserPrompt,
    AgentStop,
    Compaction,
}

impl HookEvent {
    fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "pre_tool_use",
            Self::PostToolUse => "post_tool_use",
            Self::SessionStart => "session_start",
            Self::SessionEnd => "session_end",
            Self::UserPrompt => "user_prompt",
            Self::AgentStop => "agent_stop",
            Self::Compaction => "compaction",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HookConfig {
    pub event: HookEvent,
    pub command: String,
    #[serde(default)]
    pub matcher: Option<String>,
    #[serde(default = "default_hook_timeout")]
    pub timeout: f64,
}

fn default_hook_timeout() -> f64 {
    30.0
}

#[derive(Debug, Clone, Default)]
pub struct HookContext {
    pub event: HookEvent,
    pub tool_name: Option<String>,
    pub tool_args: Value,
    pub result: Option<String>,
    pub is_error: bool,
    pub session_id: String,
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HookResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HooksFileConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub hooks: Vec<HookConfig>,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone, Default)]
pub struct HookManager {
    hooks: Vec<HookConfig>,
    audit_paths: Option<blockcell_core::Paths>,
}

impl HookManager {
    pub fn new(hooks: Vec<HookConfig>) -> Self {
        Self {
            hooks,
            audit_paths: None,
        }
    }

    pub fn load(config_file: &Path) -> Self {
        if !config_file.exists() {
            return Self::default();
        }

        match std::fs::read_to_string(config_file) {
            Ok(content) => match serde_yaml::from_str::<HooksFileConfig>(&content) {
                Ok(config) => {
                    info!(hooks = config.hooks.len(), "Loaded hook configuration");
                    Self {
                        hooks: config.hooks,
                        audit_paths: None,
                    }
                }
                Err(error) => {
                    warn!(path = %config_file.display(), error = %error, "Failed to parse hook configuration");
                    Self::default()
                }
            },
            Err(error) => {
                warn!(path = %config_file.display(), error = %error, "Failed to read hook configuration");
                Self::default()
            }
        }
    }

    pub fn with_audit_paths(mut self, paths: blockcell_core::Paths) -> Self {
        self.audit_paths = Some(paths);
        self
    }

    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    pub fn hooks(&self) -> &[HookConfig] {
        &self.hooks
    }

    pub fn matching_hooks<'a>(&'a self, ctx: &HookContext) -> Vec<&'a HookConfig> {
        self.hooks
            .iter()
            .filter(|hook| self.matches(hook, ctx))
            .collect()
    }

    pub async fn fire(&self, ctx: &HookContext) -> Vec<HookResult> {
        let mut results = Vec::new();
        for hook in &self.hooks {
            if self.matches(hook, ctx) {
                let started = std::time::Instant::now();
                let result = self.execute(hook, ctx).await;
                let duration_ms = started.elapsed().as_millis() as u64;
                let timed_out = result
                    .error
                    .as_deref()
                    .is_some_and(|error| error.starts_with("Hook timed out"));
                if !result.success {
                    // Never log commands, arguments or output: all may contain secrets.
                    warn!(event = hook.event.as_str(), session_id = %ctx.session_id,
                        tool = ?ctx.tool_name, duration_ms, timed_out, "Hook execution failed");
                }
                if let Some(paths) = self.audit_paths.as_ref() {
                    let paths = paths.clone();
                    let session_key = ctx.session_id.clone();
                    let event = blockcell_storage::AuditEvent::HookExecution {
                        session_key: session_key.clone(),
                        hook_event: hook.event.as_str().to_string(),
                        tool_name: ctx.tool_name.clone(),
                        success: result.success,
                        timed_out,
                        duration_ms,
                        timestamp_ms: chrono::Utc::now().timestamp_millis(),
                    };
                    match tokio::task::spawn_blocking(move || {
                        blockcell_storage::AuditLogger::new(paths)
                            .log_observed_event(&session_key, event)
                    })
                    .await
                    {
                        Ok(Ok(())) => {}
                        outcome => warn!(?outcome, "Hook audit write failed"),
                    }
                }
                results.push(result);
            }
        }
        results
    }

    fn matches(&self, hook: &HookConfig, ctx: &HookContext) -> bool {
        if hook.event != ctx.event {
            return false;
        }

        if let Some(matcher) = hook.matcher.as_deref() {
            let Some(tool_name) = ctx.tool_name.as_deref() else {
                return false;
            };
            return matcher
                .split('|')
                .filter(|part| !part.trim().is_empty())
                .any(|part| {
                    Pattern::new(part.trim())
                        .map(|pattern| pattern.matches(tool_name))
                        .unwrap_or(false)
                });
        }

        true
    }

    fn expand_command(&self, command: &str, ctx: &HookContext) -> String {
        // 单遍替换：每个 {token} 至多展开一次，已插入的值不再被后续替换扫描——
        // 否则 file_path="x {command} y" 会在 {command} 替换时被二次展开并脱离
        // shell_quote 的引号保护（重入展开 → 间接命令注入）。
        let mut replacements: Vec<(&'static str, String)> = vec![
            ("{tool_name}", ctx.tool_name.clone().unwrap_or_default()),
            ("{session_id}", ctx.session_id.clone()),
            ("{cwd}", ctx.cwd.clone()),
            ("{event}", ctx.event.as_str().to_string()),
        ];
        let file_path = ctx.tool_args.get("file_path").and_then(Value::as_str);
        let path = ctx.tool_args.get("path").and_then(Value::as_str);
        // {file_path} 回退到 path（file_path 优先，与顺序替换语义一致）
        if let Some(fp) = file_path.or(path) {
            replacements.push(("{file_path}", fp.to_string()));
        }
        if let Some(p) = path {
            replacements.push(("{path}", p.to_string()));
        }
        if let Some(command_value) = ctx.tool_args.get("command").and_then(Value::as_str) {
            replacements.push(("{command}", command_value.to_string()));
        }
        if let Some(result) = ctx.result.as_deref() {
            replacements.push(("{result}", truncate_chars(result, 1000)));
        }

        let mut expanded = String::with_capacity(command.len());
        let mut rest = command;
        while let Some(start) = rest.find('{') {
            let Some(end_rel) = rest[start..].find('}') else {
                break;
            };
            let end = start + end_rel + 1;
            let token = &rest[start..end];
            match replacements.iter().find(|(t, _)| *t == token) {
                Some((_, value)) => {
                    expanded.push_str(&rest[..start]);
                    expanded.push_str(&shell_quote(value));
                }
                None => expanded.push_str(&rest[..end]),
            }
            rest = &rest[end..];
        }
        expanded.push_str(rest);
        expanded
    }

    #[cfg(test)]
    fn expand_command_for_test(&self, command: &str, ctx: &HookContext) -> String {
        self.expand_command(command, ctx)
    }

    async fn execute(&self, hook: &HookConfig, ctx: &HookContext) -> HookResult {
        let command = self.expand_command(&hook.command, ctx);
        let timeout = Duration::from_secs_f64(hook.timeout.max(0.001));

        let mut process = if cfg!(windows) {
            let mut cmd = Command::new("cmd");
            cmd.arg("/C").arg(&command);
            cmd
        } else {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(&command);
            cmd
        };
        // On timeout the future is dropped; without this the child (and any
        // grandchildren) would keep running in the background.
        process.kill_on_drop(true);

        if !ctx.cwd.trim().is_empty() {
            process.current_dir(&ctx.cwd);
        }

        match tokio::time::timeout(timeout, process.output()).await {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let success = output.status.success();
                HookResult {
                    success,
                    output: stdout,
                    error: (!success)
                        .then_some(stderr)
                        .filter(|value| !value.is_empty()),
                }
            }
            Ok(Err(error)) => HookResult {
                success: false,
                output: String::new(),
                error: Some(format!("Hook exec failed: {}", error)),
            },
            Err(_) => HookResult {
                success: false,
                output: String::new(),
                error: Some(format!("Hook timed out after {:.3}s", hook.timeout)),
            },
        }
    }
}

/// 将用户配置的 `hooks.yaml` 适配为 EventBus listener，现有配置无需迁移。
///
/// 映射：`turn/start` → SessionStart(新会话) + UserPrompt；`tool/pre-execute` → PreToolUse；
/// `tool/post-execute` → PostToolUse；`turn/end` → AgentStop。Hook 结果仅观察，不阻断执行。
pub struct HookAdapter {
    manager: HookManager,
    cwd: String,
}

impl HookAdapter {
    pub fn new(manager: HookManager, cwd: String) -> Self {
        Self { manager, cwd }
    }

    fn context(&self, event: HookEvent, ctx: &AgentEventContext) -> HookContext {
        HookContext {
            event,
            session_id: ctx.session_id.clone(),
            cwd: self.cwd.clone(),
            ..HookContext::default()
        }
    }
}

#[async_trait]
impl AgentEventListener for HookAdapter {
    fn spec(&self) -> ListenerSpec {
        // 每个 hook 已按配置的 timeout 单独限时，整体不再叠加总超时。
        ListenerSpec::new("hooks").without_timeout()
    }

    async fn handle(
        &self,
        ctx: &AgentEventContext,
        event: &mut AgentEvent,
    ) -> blockcell_core::Result<EventDecision> {
        match event {
            AgentEvent::TurnStart {
                user_input,
                is_new_session,
            } => {
                if *is_new_session {
                    self.manager
                        .fire(&self.context(HookEvent::SessionStart, ctx))
                        .await;
                }
                self.manager
                    .fire(&HookContext {
                        result: Some(user_input.clone()),
                        ..self.context(HookEvent::UserPrompt, ctx)
                    })
                    .await;
            }
            AgentEvent::ToolPreExecute {
                tool_name,
                arguments,
                ..
            } => {
                self.manager
                    .fire(&HookContext {
                        tool_name: Some(tool_name.clone()),
                        tool_args: arguments.clone(),
                        ..self.context(HookEvent::PreToolUse, ctx)
                    })
                    .await;
            }
            AgentEvent::ToolPostExecute {
                tool_name,
                arguments,
                result,
                is_error,
                ..
            } => {
                let result = match result.get("error").and_then(Value::as_str) {
                    Some(error) if *is_error => error.to_string(),
                    _ => result.to_string(),
                };
                self.manager
                    .fire(&HookContext {
                        tool_name: Some(tool_name.clone()),
                        tool_args: arguments.clone(),
                        result: Some(result),
                        is_error: *is_error,
                        ..self.context(HookEvent::PostToolUse, ctx)
                    })
                    .await;
            }
            AgentEvent::TurnEnd {
                final_response,
                error,
            } => {
                self.manager
                    .fire(&HookContext {
                        result: Some(error.clone().unwrap_or_else(|| final_response.clone())),
                        is_error: error.is_some(),
                        ..self.context(HookEvent::AgentStop, ctx)
                    })
                    .await;
            }
            _ => {}
        }
        Ok(EventDecision::Continue)
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    let safe = value.chars().all(|ch| {
        ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                '_' | '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
            )
    });
    if safe {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::time::Instant;

    #[test]
    fn loads_hooks_yaml_with_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("hooks.yaml");
        fs::write(
            &file,
            r#"
version: 1
hooks:
  - event: post_tool_use
    matcher: "write_*"
    command: "echo {tool_name}"
"#,
        )
        .expect("write hooks yaml");

        let manager = HookManager::load(&file);

        assert_eq!(manager.len(), 1);
        assert_eq!(manager.hooks()[0].event, HookEvent::PostToolUse);
        assert_eq!(manager.hooks()[0].timeout, 30.0);
    }

    #[test]
    fn matches_tool_event_with_glob_matcher() {
        let manager = HookManager::new(vec![HookConfig {
            event: HookEvent::PreToolUse,
            command: "echo ok".to_string(),
            matcher: Some("write_*".to_string()),
            timeout: 5.0,
        }]);
        let ctx = HookContext {
            event: HookEvent::PreToolUse,
            tool_name: Some("write_file".to_string()),
            ..HookContext::default()
        };

        assert_eq!(manager.matching_hooks(&ctx).len(), 1);
    }

    #[test]
    fn expands_template_values_with_shell_quoting() {
        let manager = HookManager::new(Vec::new());
        let ctx = HookContext {
            event: HookEvent::PostToolUse,
            tool_name: Some("exec".to_string()),
            tool_args: json!({
                "command": "echo 'hello world'",
                "file_path": "/tmp/a b.py"
            }),
            result: Some("ok".to_string()),
            session_id: "session 1".to_string(),
            cwd: "/tmp/work dir".to_string(),
            is_error: false,
        };

        let expanded = manager.expand_command_for_test(
            "tool={tool_name} sid={session_id} cwd={cwd} cmd={command} file={file_path} result={result}",
            &ctx,
        );

        assert!(expanded.contains("tool=exec"));
        assert!(expanded.contains("sid='session 1'"));
        assert!(expanded.contains("cwd='/tmp/work dir'"));
        assert!(expanded.contains("cmd='echo '\\''hello world'\\'''"));
        assert!(expanded.contains("file='/tmp/a b.py'"));
        assert!(expanded.contains("result=ok"));
    }

    #[test]
    fn expansion_does_not_reexpand_tokens_inside_inserted_values() {
        // 回归：file_path 值内含字面 {command} 时，后续替换不得把它二次展开——
        // 否则插入值会脱离 shell_quote 引号保护造成间接命令注入。
        let manager = HookManager::new(Vec::new());
        let ctx = HookContext {
            event: HookEvent::PostToolUse,
            tool_name: Some("write_file".to_string()),
            tool_args: json!({
                "file_path": "x {command} y",
                "command": "rm -rf /"
            }),
            ..HookContext::default()
        };

        let expanded = manager.expand_command_for_test("f={file_path}", &ctx);
        assert_eq!(expanded, "f='x {command} y'");
        assert!(!expanded.contains("rm -rf"));
    }

    #[tokio::test]
    async fn executes_matching_hook_and_captures_stdout() {
        let manager = HookManager::new(vec![HookConfig {
            event: HookEvent::SessionStart,
            command: "printf hook-ok".to_string(),
            matcher: None,
            timeout: 5.0,
        }]);

        let results = manager
            .fire(&HookContext {
                event: HookEvent::SessionStart,
                ..HookContext::default()
            })
            .await;

        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert_eq!(results[0].output, "hook-ok");
    }

    #[tokio::test]
    async fn hook_adapter_maps_runtime_events_to_hook_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("events.log");
        let command = format!("echo {{event}}:{{tool_name}} >> '{}'", log.display());
        let manager = HookManager::new(
            [
                HookEvent::SessionStart,
                HookEvent::UserPrompt,
                HookEvent::PreToolUse,
                HookEvent::PostToolUse,
                HookEvent::AgentStop,
            ]
            .into_iter()
            .map(|event| HookConfig {
                event,
                command: command.clone(),
                matcher: None,
                timeout: 5.0,
            })
            .collect(),
        );
        let adapter = HookAdapter::new(manager, dir.path().display().to_string());
        let ctx = AgentEventContext::default();
        let mut events = vec![
            AgentEvent::TurnStart {
                user_input: "hi".into(),
                is_new_session: true,
            },
            AgentEvent::ToolPreExecute {
                tool_name: "exec".into(),
                call_id: "c1".into(),
                arguments: json!({}),
            },
            AgentEvent::ToolPostExecute {
                tool_name: "exec".into(),
                call_id: "c1".into(),
                arguments: json!({}),
                result: json!({"ok": true}),
                is_error: false,
                duration_ms: 1,
            },
            AgentEvent::StepStart { iteration: 1 },
            AgentEvent::TurnEnd {
                final_response: "done".into(),
                error: None,
            },
        ];
        for event in &mut events {
            assert_eq!(
                adapter.handle(&ctx, event).await.unwrap(),
                EventDecision::Continue
            );
        }

        let logged = fs::read_to_string(&log).expect("read log");
        let lines: Vec<&str> = logged.lines().collect();
        assert_eq!(
            lines,
            vec![
                "session_start:",
                "user_prompt:",
                "pre_tool_use:exec",
                "post_tool_use:exec",
                "agent_stop:"
            ]
        );
    }

    #[tokio::test]
    async fn failed_hook_is_audited_without_logging_its_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = blockcell_core::Paths::with_base(dir.path().join("base"));
        std::fs::create_dir_all(paths.audit_dir()).expect("audit dir");
        let manager = HookManager::new(vec![HookConfig {
            event: HookEvent::PreToolUse,
            command: "printf 'secret-token'; exit 3".to_string(),
            matcher: None,
            timeout: 5.0,
        }])
        .with_audit_paths(paths.clone());

        let results = manager
            .fire(&HookContext {
                event: HookEvent::PreToolUse,
                tool_name: Some("exec".to_string()),
                session_id: "ws:hook-audit".to_string(),
                ..HookContext::default()
            })
            .await;

        assert_eq!(results.len(), 1);
        assert!(!results[0].success);

        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let log = paths.audit_dir().join(format!("{date}.jsonl"));
        let content = std::fs::read_to_string(&log).expect("hook audit file");
        assert!(content.contains("\"type\":\"hook_execution\""), "{content}");
        assert!(content.contains("\"success\":false"), "{content}");
        assert!(content.contains("ws:hook-audit"), "{content}");
        assert!(
            !content.contains("secret-token"),
            "hook stdout/stderr must not reach the audit log: {content}"
        );
    }

    #[tokio::test]
    async fn hook_timeout_returns_failure() {
        let manager = HookManager::new(vec![HookConfig {
            event: HookEvent::SessionStart,
            command: "sleep 2".to_string(),
            matcher: None,
            timeout: 0.1,
        }]);
        let started = Instant::now();

        let results = manager
            .fire(&HookContext {
                event: HookEvent::SessionStart,
                ..HookContext::default()
            })
            .await;

        assert!(started.elapsed().as_secs_f64() < 1.0);
        assert_eq!(results.len(), 1);
        assert!(!results[0].success);
        assert!(results[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("timed out"));
    }
}
