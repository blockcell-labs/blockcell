use serde_json::json;

/// Build a structured JSON error string for tool execution failures.
/// This is the standard format returned to the LLM when a tool call is rejected.
pub(crate) fn tool_denied_json(tool_name: &str, error: &str, hint: &str) -> String {
    json!({
        "error": error,
        "tool": tool_name,
        "hint": hint,
    })
    .to_string()
}

/// Build a scoped-tool-denied result when a tool is not in the current scope.
pub(crate) fn scoped_tool_denied_result(tool_name: &str) -> String {
    let base = "Do not retry this unavailable capability or search for a bypass. Check the agent tool profile, denyTools, and active skill scope; explain the limitation. Tool discovery cannot grant permissions.";
    let hint = match replacement_tool_hint(tool_name) {
        Some(replacement) => format!("{replacement} {base}"),
        None => base.to_string(),
    };
    tool_denied_json(
        tool_name,
        &format!(
            "Tool '{}' is not available in the current built-in/skill scope.",
            tool_name
        ),
        &hint,
    )
}

/// 旧名/别名对应的真实工具名与调用方式。模型偶尔会凭记忆调用已合并的工具，
/// 直接给出正确写法比让它继续猜更省一轮。
fn replacement_tool_hint(tool_name: &str) -> Option<&'static str> {
    Some(match tool_name {
        "shell_run" => {
            "This build exposes the persistent `shell` tool instead: call `shell` with action:\"run\"."
        }
        "shell_poll" => {
            "This build exposes the persistent `shell` tool instead: call `shell` with action:\"poll\"."
        }
        "shell_input" => {
            "This build exposes the persistent `shell` tool instead: call `shell` with action:\"input\"."
        }
        "shell_close" => {
            "This build exposes the persistent `shell` tool instead: call `shell` with action:\"close\"."
        }
        "exec_local" | "exec_skill_script" => {
            "These tools only work inside an active skill scope: call `activate_skill` first."
        }
        _ => return None,
    })
}

/// Build a disabled-tool error result.
pub(crate) fn disabled_tool_result(tool_name: &str) -> String {
    tool_denied_json(
        tool_name,
        &format!(
            "Tool '{}' is currently disabled via toggles.",
            tool_name
        ),
        "This tool has been disabled by the user. Use toggle_manage to re-enable it, or use an alternative tool.",
    )
}

/// Build a disabled-skill error result.
pub(crate) fn disabled_skill_result(skill_name: &str) -> String {
    tool_denied_json(
        skill_name,
        &format!("Skill '{}' is currently disabled via toggles.", skill_name),
        "This skill has been disabled by the user. Use toggle_manage to re-enable it.",
    )
}

/// Build a permission-denied error for dangerous exec/shell commands.
pub(crate) fn dangerous_exec_denied(tool_name: &str, has_confirm_channel: bool) -> String {
    let hint = if has_confirm_channel {
        "The command looks dangerous (e.g. kill/pkill/killall/service stop). Ask the user to confirm explicitly before running it."
    } else {
        "This channel cannot show an interactive confirm prompt. Approval must name the exact tool and JSON arguments; a generic confirmation cannot authorize execution."
    };
    json!({
        "error": format!("Permission denied: dangerous {tool_name} command requires explicit user confirmation."),
        "tool": tool_name,
        "code": "NEEDS_CONFIRMATION_NONINTERACTIVE",
        "hint": hint,
        "alternative": "Only this operation requires approval; unrelated tool calls remain available. Do not retry it through equivalent commands or other tools. Leave artifacts in place and skip optional cleanup; request explicit approval only if the operation is necessary to the user's task."
    }).to_string()
}

/// Build a permission-denied error for dangerous file_ops.
pub(crate) fn dangerous_file_ops_denied() -> String {
    json!({
        "error": "Permission denied: this file operation requires explicit user confirmation.",
        "tool": "file_ops",
        "code": "NEEDS_CONFIRMATION_NONINTERACTIVE",
        "hint": "Only this operation requires approval; unrelated tool calls remain available. Do not retry it through equivalent commands or other tools. Leave artifacts in place and skip optional cleanup; request explicit approval only if the operation is necessary to the user's task.",
    }).to_string()
}

/// Build a path-access denied error.
pub(crate) fn path_access_denied(tool_name: &str, path: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(path) {
        if value.get("code").is_some() {
            return value.to_string();
        }
    }
    json!({
        "error": format!("Path access denied by security policy: {}", path),
        "tool": tool_name,
        "code": "PATH_ACCESS_DENIED",
        "hint": "Only this invocation was rejected; the tool and unrelated authorized steps remain available. Keep and use files and facts already discovered. A rejected optional probe does not invalidate earlier results or prove that project files are inaccessible. Continue the remaining authorized work. Do not retry denied access or bypass explicit user/policy denials; correct the path only when the intended access itself is allowed.",
    }).to_string()
}

/// Format the user-friendly LLM error message after all retries are exhausted.
pub(crate) fn llm_exhausted_error(retry_count: u32, error: &blockcell_core::Error) -> String {
    format!(
        "抱歉，我在处理你的请求时遇到了问题（已重试 {} 次）。\n\n\
         错误信息：{}\n\n\
         这可能是临时的网络或服务问题，请稍后再试。如果问题持续，我会自动学习并改进。",
        retry_count, error
    )
}

// ── Tool failure classification ──

/// Classify whether a tool error is transient (retryable) or permanent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolFailureKind {
    /// Temporary error — network timeout, rate limit, etc.
    Transient,
    /// Permanent error — missing API key, validation failure, etc.
    Permanent,
    /// This operation needs approval; the tool itself remains available.
    ApprovalRequired,
    /// Domain/resource-level miss — the requested object/path/page does not exist.
    ResourceMissing,
    /// Skill context missing — need to call activate_skill first.
    SkillContextMissing,
}

/// 同一 turn 内出现这么多次"被拒绝"后，提示模型收尾而不是继续换工具。
pub(crate) const POLICY_REJECTION_WRAP_UP_THRESHOLD: u32 = 2;

/// 该结果是"本次调用被拒绝"，而不是"工具坏了"。三者都来自策略、路径规则或
/// 技能作用域，换等价工具不会解除，因此需要单独计数以便触发收尾提示。
pub(crate) fn is_policy_rejection(result: &str) -> bool {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(result) {
        let code = value["code"].as_str().unwrap_or_default();
        if code == "PATH_ACCESS_DENIED" || code == "NEEDS_CONFIRMATION_NONINTERACTIVE" {
            return true;
        }
    }
    let lower = result.to_ascii_lowercase();
    lower.contains("requires an active skill context") || lower.contains("skill context")
}

/// Classify a tool error result string into transient or permanent.
pub(crate) fn classify_tool_failure(result: &str) -> ToolFailureKind {
    // A policy rejection describes these arguments, not tool availability.
    // Only recognize structured runtime errors, not arbitrary matching prose.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(result) {
        if value["code"] == "PATH_ACCESS_DENIED"
            || (value["code"] == "NEEDS_CONFIRMATION_NONINTERACTIVE" && value["paths"].is_array())
        {
            return ToolFailureKind::Transient;
        }
        if value["code"] == "NEEDS_CONFIRMATION_NONINTERACTIVE" {
            return ToolFailureKind::ApprovalRequired;
        }
    }
    let lower = result.to_ascii_lowercase();

    // Skill context missing — needs activate_skill first
    // This is a recoverable error: LLM just needs to activate the skill before retrying
    // Covers: exec_local, exec_skill_script and any future skill-dependent tools
    if lower.contains("requires an active skill context")
        || lower.contains("skill context")
        || (lower.contains("activate_skill") && lower.contains("first"))
        || lower.contains("active_skill_dir")
    {
        return ToolFailureKind::SkillContextMissing;
    }

    // Permanent errors — no point retrying
    if lower.contains("api key")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("authentication")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("validation error")
        || lower.contains("config error")
        || lower.contains("missing required")
        || lower.contains("permission denied")
        || lower.contains("not installed")
        || lower.contains("not available")
        || lower.contains("disabled")
    {
        return ToolFailureKind::Permanent;
    }

    if lower.contains("not found")
        || lower.contains("404")
        || lower.contains("no such file or directory")
    {
        return ToolFailureKind::ResourceMissing;
    }

    // Transient errors — may succeed on retry
    if lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("429")
        || lower.contains("503")
        || lower.contains("502")
        || lower.contains("504")
        || lower.contains("temporary")
        || lower.contains("network")
        || lower.contains("dns")
        || lower.contains("bad parameter")
    {
        return ToolFailureKind::Transient;
    }

    // Default: treat unknown errors as permanent to avoid wasting iterations
    ToolFailureKind::Permanent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_rejections_are_counted_separately_from_tool_outages() {
        for denial in [
            path_access_denied("shell", "/outside/x"),
            dangerous_exec_denied("exec", false),
            dangerous_file_ops_denied(),
        ] {
            assert!(is_policy_rejection(&denial), "{denial}");
        }
        assert!(is_policy_rejection(
            "exec_local requires an active skill context. Use the `activate_skill` tool first"
        ));
        // 真正的工具故障不是"拒绝"，不应计入收尾阈值。
        for outage in [
            r#"{"error":"connection reset","code":"TRANSIENT"}"#,
            r#"{"error":"missing API key"}"#,
            "Tool 'web_search' is not available in the current built-in/skill scope.",
        ] {
            assert!(!is_policy_rejection(outage), "{outage}");
        }
    }

    #[test]
    fn scoped_denial_names_the_replacement_tool() {
        let shell = scoped_tool_denied_result("shell_run");
        assert!(shell.contains("persistent `shell` tool"), "{shell}");
        assert!(shell.contains("run"), "{shell}");
        let skill = scoped_tool_denied_result("exec_local");
        assert!(skill.contains("activate_skill"), "{skill}");
        // 没有已知替代名时保持原提示，不编造工具。
        let other = scoped_tool_denied_result("quantum_oracle");
        assert!(!other.contains("action:"), "{other}");
    }

    #[test]
    fn efficiency_regression_approval_refusal_is_not_tool_unavailability() {
        for denial in [
            dangerous_exec_denied("shell_run", false),
            dangerous_file_ops_denied(),
        ] {
            assert_ne!(classify_tool_failure(&denial), ToolFailureKind::Permanent);
            assert_ne!(
                classify_tool_failure(&denial),
                ToolFailureKind::Transient,
                "approval refusals must not invite retry"
            );
            let value: serde_json::Value = serde_json::from_str(&denial).unwrap();
            assert_eq!(value["code"], "NEEDS_CONFIRMATION_NONINTERACTIVE");
            assert!(value.to_string().contains("skip optional cleanup"));
            assert!(!value.to_string().contains("system temp dir"));
        }
        assert_eq!(
            classify_tool_failure("API key not configured"),
            ToolFailureKind::Permanent
        );
    }

    #[test]
    fn bench_regression_path_rejection_does_not_disable_tool() {
        let denied = path_access_denied("shell_run", "path access denied by policy: /outside/out");
        let value: serde_json::Value = serde_json::from_str(&denied).unwrap();
        assert_eq!(value["code"], "PATH_ACCESS_DENIED");
        let hint = value["hint"].as_str().unwrap();
        assert!(hint.contains("Only this invocation was rejected"));
        assert!(hint.contains("Keep and use files and facts already discovered"));
        assert!(hint.contains("Continue the remaining authorized work"));
        assert_eq!(classify_tool_failure(&denied), ToolFailureKind::Transient);
        assert_eq!(
            classify_tool_failure(
                r#"{"code":"NEEDS_CONFIRMATION_NONINTERACTIVE","paths":["/outside/out"],"error":"no confirmation channel available"}"#
            ),
            ToolFailureKind::Transient
        );
        // Credential failures remain permanent; approval is invocation-scoped.
        assert_eq!(
            classify_tool_failure("API key not configured"),
            ToolFailureKind::Permanent
        );
        assert_eq!(
            classify_tool_failure(&dangerous_exec_denied("shell_run", false)),
            ToolFailureKind::ApprovalRequired
        );
    }

    #[test]
    fn test_classify_tool_failure_permanent() {
        assert_eq!(
            classify_tool_failure("Config error: API key not configured"),
            ToolFailureKind::Permanent
        );
        assert_eq!(
            classify_tool_failure("Error: Unauthorized"),
            ToolFailureKind::Permanent
        );
        assert_eq!(
            classify_tool_failure("Validation error: missing required parameter 'query'"),
            ToolFailureKind::Permanent
        );
    }

    #[test]
    fn test_classify_tool_failure_transient() {
        assert_eq!(
            classify_tool_failure("Tool error: connection timed out"),
            ToolFailureKind::Transient
        );
        assert_eq!(
            classify_tool_failure("Error: rate limit exceeded (429)"),
            ToolFailureKind::Transient
        );
        assert_eq!(
            classify_tool_failure("Network error: DNS resolution failed"),
            ToolFailureKind::Transient
        );
    }

    #[test]
    fn test_classify_tool_failure_resource_missing() {
        assert_eq!(
            classify_tool_failure("Error: feed not found"),
            ToolFailureKind::ResourceMissing
        );
        assert_eq!(
            classify_tool_failure("HTTP 404 Not Found"),
            ToolFailureKind::ResourceMissing
        );
    }

    #[test]
    fn test_classify_tool_failure_skill_context_missing() {
        // Main error message from exec_local/exec_skill_script
        assert_eq!(
            classify_tool_failure("exec_skill_script requires an active skill context. Use the `activate_skill` tool first"),
            ToolFailureKind::SkillContextMissing
        );
        assert_eq!(
            classify_tool_failure(
                "exec_local requires an active skill context. Use the `activate_skill` tool first"
            ),
            ToolFailureKind::SkillContextMissing
        );
        // Generic "skill context" detection
        assert_eq!(
            classify_tool_failure("Tool error: skill context is not set"),
            ToolFailureKind::SkillContextMissing
        );
        // "activate_skill" + "first" combination
        assert_eq!(
            classify_tool_failure("Error: please call activate_skill first"),
            ToolFailureKind::SkillContextMissing
        );
        // active_skill_dir reference
        assert_eq!(
            classify_tool_failure("Tool error: active_skill_dir is None"),
            ToolFailureKind::SkillContextMissing
        );
    }

    #[test]
    fn test_tool_denied_json_structure() {
        let result = tool_denied_json("test_tool", "some error", "some hint");
        let val: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(val["tool"], "test_tool");
        assert_eq!(val["error"], "some error");
        assert_eq!(val["hint"], "some hint");
    }
}
