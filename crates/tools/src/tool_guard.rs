//! 工具调用准入检查的共享部分。
//!
//! - [`dangerous_operation_items`]：需要用户显式确认的危险操作判定，Runtime 的 Guarded Tool
//!   Pipeline 与技能脚本嵌套调用共用同一份规则；
//! - [`NestedToolGuard`]：技能脚本（Rhai）内部的嵌套工具调用不经过 Runtime 管线，在此复用
//!   开关禁用、Tool Policy 与危险操作检查，并写入审计日志。嵌套上下文无法向用户确认，
//!   Policy 的 `ask` 与需要确认的操作一律拒绝。

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use blockcell_core::tool_policy::{ToolCallContext, ToolPolicy, ToolPolicyDecision};
use blockcell_core::{Error, Paths, Result};
use blockcell_storage::AuditLogger;
use regex::Regex;
use serde_json::Value;

use crate::ToolContext;

static DANGEROUS_EXEC_DIRECT_RES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(^|[;&|]\s*|\b(?:sudo|env)\s+)(?:rm|trash|unlink)\b",
        r"(^|[;&|]\s*|\b(?:sudo|env)\s+)rmdir\b",
        r"\bfind\b[\s\S]*\s-delete\b",
        r"\bfind\b[\s\S]*\s-exec\s+rm\b",
        r#"\bsh\s+-c\s+['"][^'"]*\brm\b"#,
        r#"\bbash\s+-c\s+['"][^'"]*\brm\b"#,
        r#"\bzsh\s+-c\s+['"][^'"]*\brm\b"#,
        r"\bpython(?:3)?\b[\s\S]*\b(?:shutil\.rmtree|os\.remove|os\.unlink|os\.rmdir)\b",
        r"\bperl\b[\s\S]*\bunlink\b",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("dangerous exec direct regex is valid"))
    .collect()
});

static DANGEROUS_EXEC_RM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(^|[;&|]\s*|\b(?:sudo|env)\s+)rm\b([^;&|]*)")
        .expect("dangerous exec rm regex is valid")
});

pub fn is_dangerous_exec_command(command: &str) -> bool {
    let c = command.to_lowercase();
    let c = c.trim();
    if c.is_empty() {
        return false;
    }

    for re in DANGEROUS_EXEC_DIRECT_RES.iter() {
        if re.is_match(c) {
            return true;
        }
    }

    {
        let rm_re = &*DANGEROUS_EXEC_RM_RE;
        for caps in rm_re.captures_iter(c) {
            let suffix = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let has_recursive = suffix.contains(" -r")
                || suffix.contains(" -rf")
                || suffix.contains(" -fr")
                || suffix.starts_with("-r")
                || suffix.starts_with("-rf")
                || suffix.starts_with("-fr");
            let has_force = suffix.contains(" -f")
                || suffix.contains(" -rf")
                || suffix.contains(" -fr")
                || suffix.starts_with("-f")
                || suffix.starts_with("-rf")
                || suffix.starts_with("-fr");
            let has_target = suffix
                .split_whitespace()
                .any(|token| !token.starts_with('-') && !token.is_empty());
            if has_target && (has_recursive || has_force) {
                return true;
            }
            if has_target && suffix.contains("../") {
                return true;
            }
        }
    }

    let dangerous = [
        "kill ",
        "pkill",
        "killall",
        "taskkill",
        "systemctl stop",
        "service stop",
        "launchctl bootout",
        "launchctl kill",
    ];

    dangerous.iter().any(|p| c.contains(p))
}

pub fn is_sensitive_filename(path: &str) -> bool {
    let p = path.replace('\\', "/");
    let name = p.rsplit('/').next().unwrap_or("").to_lowercase();
    matches!(
        name.as_str(),
        "config.json5" | "config.json" | "config.toml" | "config.yaml" | "config.yml"
    )
}

/// 需要用户显式确认的危险操作描述；为空表示无需确认。
pub fn dangerous_operation_items(tool_name: &str, args: &Value) -> Vec<String> {
    let field = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("");
    match tool_name {
        "exec" => {
            let cmd = field("command");
            if is_dangerous_exec_command(cmd) {
                vec![format!("command: {}", cmd)]
            } else {
                Vec::new()
            }
        }
        "file_ops" => {
            let action = field("action");
            let path = field("path");
            let destination = field("destination");
            let recursive = args
                .get("recursive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let mut items = Vec::new();
            if action == "delete" && recursive {
                items.push(format!("file_ops delete recursive=true path={}", path));
            }
            if (action == "delete" || action == "rename" || action == "move")
                && (is_sensitive_filename(path) || is_sensitive_filename(destination))
            {
                items.push(format!(
                    "file_ops {} sensitive file (config*) path={} destination={}",
                    action, path, destination
                ));
            }
            items
        }
        _ => Vec::new(),
    }
}

/// shell 以 full-access 沙箱策略运行命令，需要用户显式确认。
fn shell_requests_full_access(tool_name: &str, args: &Value) -> bool {
    tool_name == "shell"
        && args.get("action").and_then(Value::as_str).unwrap_or("run") == "run"
        && args.get("sandbox_policy").and_then(Value::as_str) == Some("full-access")
}

fn disabled_toggles(paths: &Paths) -> (HashSet<String>, HashSet<String>) {
    let mut tools = HashSet::new();
    let mut skills = HashSet::new();
    let toggles = std::fs::read_to_string(paths.toggles_file())
        .ok()
        .and_then(|content| serde_json::from_str::<Value>(&content).ok());
    if let Some(toggles) = toggles {
        for (category, out) in [("tools", &mut tools), ("skills", &mut skills)] {
            if let Some(items) = toggles.get(category).and_then(Value::as_object) {
                out.extend(
                    items
                        .iter()
                        .filter(|(_, enabled)| **enabled == false)
                        .map(|(name, _)| name.clone()),
                );
            }
        }
    }
    (tools, skills)
}

/// 技能脚本嵌套工具调用的准入守卫与审计。
pub struct NestedToolGuard {
    policy: ToolPolicy,
    channel: String,
    session_key: String,
    disabled_tools: HashSet<String>,
    disabled_skills: HashSet<String>,
    audit: Mutex<AuditLogger>,
}

impl NestedToolGuard {
    pub fn new(ctx: &ToolContext) -> Self {
        let paths = Paths::with_base_and_workspace(ctx.base.clone(), ctx.workspace.clone());
        Self::with_policy(
            ctx,
            ToolPolicy::load(&paths.base.join("tool_policy.yaml")),
            paths,
        )
    }

    fn with_policy(ctx: &ToolContext, policy: ToolPolicy, paths: Paths) -> Self {
        let (disabled_tools, disabled_skills) = disabled_toggles(&paths);
        let mut audit = AuditLogger::new(paths);
        audit.set_session_id(&ctx.session_key);
        Self {
            policy,
            channel: ctx.channel.clone(),
            session_key: ctx.session_key.clone(),
            disabled_tools,
            disabled_skills,
            audit: Mutex::new(audit),
        }
    }

    /// 准入检查；拒绝时返回 [`Error::PermissionDenied`]，被拒绝的调用同样写入审计。
    pub fn admit(&self, tool_name: &str, args: &Value) -> Result<()> {
        let denied = self.denial_reason(tool_name, args);
        if let Some(reason) = &denied {
            self.audit(
                tool_name,
                args,
                serde_json::json!({ "error": reason, "outcome": "rejected" }),
                0,
            );
        }
        denied.map_or(Ok(()), |reason| Err(Error::PermissionDenied(reason)))
    }

    fn denial_reason(&self, tool_name: &str, args: &Value) -> Option<String> {
        if self.disabled_tools.contains(tool_name) || self.disabled_skills.contains(tool_name) {
            return Some(format!("'{tool_name}' is disabled via toggles"));
        }
        let eval = self.policy.evaluate(&ToolCallContext {
            tool_name,
            tool_args: args,
            channel: &self.channel,
        });
        let _ = self
            .audit
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .log_permission_decision(
                tool_name,
                format!("{:?}", eval.decision),
                eval.matched_rule.clone(),
                eval.description.clone(),
                eval.simulated,
                &self.session_key,
            );
        match eval.decision {
            ToolPolicyDecision::Allow => {}
            ToolPolicyDecision::Deny => {
                return Some(eval.description.unwrap_or_else(|| {
                    format!(
                        "Policy denied tool call by rule: {}",
                        eval.matched_rule.as_deref().unwrap_or("default")
                    )
                }))
            }
            ToolPolicyDecision::Ask => {
                return Some(format!(
                    "'{tool_name}' requires user confirmation by tool policy, which is not available inside skill scripts"
                ))
            }
        }
        let items = dangerous_operation_items(tool_name, args);
        if !items.is_empty() || shell_requests_full_access(tool_name, args) {
            return Some(format!(
                "'{tool_name}' requires explicit user confirmation, which is not available inside skill scripts"
            ));
        }
        None
    }

    /// 记录一次嵌套调用的审计日志；审计失败不影响调用本身。
    pub fn audit(&self, tool_name: &str, args: &Value, result: Value, duration_ms: u64) {
        let _ = self
            .audit
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .log_tool_call(
                tool_name,
                args.clone(),
                result,
                &self.session_key,
                None,
                Some(duration_ms),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::tool_policy::{ToolPolicyConfig, ToolPolicyRule};
    use serde_json::json;

    fn tool_context(base: &std::path::Path) -> ToolContext {
        ToolContext {
            workspace: base.to_path_buf(),
            base: base.to_path_buf(),
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "cli:nested".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: None,
            chat_id: "nested".to_string(),
            config: blockcell_core::Config::default(),
            permissions: crate::PermissionSet::new(),
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

    fn guard_with(policy: ToolPolicy) -> (tempfile::TempDir, NestedToolGuard) {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = tool_context(dir.path());
        let paths = Paths::with_base(dir.path().to_path_buf());
        (dir, NestedToolGuard::with_policy(&ctx, policy, paths))
    }

    fn rule(tool: &str, decision: ToolPolicyDecision) -> ToolPolicyRule {
        serde_json::from_value(json!({
            "name": format!("{tool}-rule"),
            "tool": tool,
            "decision": decision,
        }))
        .expect("policy rule")
    }

    #[test]
    fn sensitive_filename_matches_json5_config() {
        assert!(is_sensitive_filename("config.json5"));
        assert!(is_sensitive_filename("/tmp/.blockcell/config.json5"));
    }

    #[test]
    fn dangerous_items_cover_exec_and_destructive_file_ops() {
        assert_eq!(
            dangerous_operation_items("exec", &json!({"command": "rm -rf build"})),
            vec!["command: rm -rf build"]
        );
        assert!(dangerous_operation_items("exec", &json!({"command": "ls"})).is_empty());
        assert_eq!(
            dangerous_operation_items(
                "file_ops",
                &json!({"action": "delete", "path": "out", "recursive": true})
            )
            .len(),
            1
        );
        assert!(dangerous_operation_items("read_file", &json!({"path": "a"})).is_empty());
    }

    #[test]
    fn nested_guard_enforces_policy_and_dangerous_operations() {
        let (dir, guard) = guard_with(ToolPolicy::from_config(ToolPolicyConfig {
            rules: vec![
                rule("web_fetch", ToolPolicyDecision::Deny),
                rule("write_file", ToolPolicyDecision::Ask),
            ],
            ..Default::default()
        }));

        assert!(guard.admit("read_file", &json!({"path": "a"})).is_ok());
        for (tool, args) in [
            ("web_fetch", json!({"url": "https://example.com"})),
            ("write_file", json!({"path": "a", "content": "x"})),
            ("exec", json!({"command": "rm -rf /tmp/x"})),
            (
                "shell",
                json!({"command": "ls", "sandbox_policy": "full-access"}),
            ),
        ] {
            let error = guard.admit(tool, &args).unwrap_err();
            assert!(
                matches!(error, Error::PermissionDenied(_)),
                "{tool}: {error}"
            );
        }

        // 被拒绝的调用写入审计
        let audit = std::fs::read_dir(Paths::with_base(dir.path().to_path_buf()).audit_dir())
            .expect("audit dir")
            .flatten()
            .map(|entry| std::fs::read_to_string(entry.path()).unwrap_or_default())
            .collect::<String>();
        assert!(audit.contains("\"rejected\""), "{audit}");
        assert!(audit.contains("web_fetch"), "{audit}");
    }

    #[test]
    fn nested_guard_blocks_disabled_tools() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            Paths::with_base_and_workspace(dir.path().to_path_buf(), dir.path().to_path_buf());
        std::fs::write(paths.toggles_file(), r#"{"tools": {"exec": false}}"#).unwrap();
        let ctx = tool_context(dir.path());
        let guard = NestedToolGuard::with_policy(&ctx, ToolPolicy::permissive(), paths);

        assert!(guard.admit("exec", &json!({"command": "ls"})).is_err());
        assert!(guard.admit("list_dir", &json!({"path": "."})).is_ok());
    }
}
