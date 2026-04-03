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
    tool_denied_json(
        tool_name,
        &format!(
            "Tool '{}' is not available in the current built-in/skill scope.",
            tool_name
        ),
        "Check the allowed tools for the current interaction mode.",
    )
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
        &format!(
            "Skill '{}' is currently disabled via toggles.",
            skill_name
        ),
        "This skill has been disabled by the user. Use toggle_manage to re-enable it.",
    )
}

/// Build a permission-denied error for dangerous exec commands.
pub(crate) fn dangerous_exec_denied(has_confirm_channel: bool) -> String {
    let hint = if has_confirm_channel {
        "The command looks dangerous (e.g. kill/pkill/killall/service stop). Ask the user to confirm explicitly before running it."
    } else {
        "This channel cannot show an interactive confirm prompt. Reply with '确认执行' (or '确认重启') to proceed, otherwise I will not run kill/pkill/killall/service-stop commands."
    };
    tool_denied_json(
        "exec",
        "Permission denied: dangerous exec command requires explicit user confirmation.",
        hint,
    )
}

/// Build a permission-denied error for dangerous file_ops.
pub(crate) fn dangerous_file_ops_denied() -> String {
    tool_denied_json(
        "file_ops",
        "Permission denied: this file operation requires explicit user confirmation.",
        "The operation involves recursive deletion or sensitive config files. Ask the user to confirm.",
    )
}

/// Build a path-access denied error.
pub(crate) fn path_access_denied(tool_name: &str, path: &str) -> String {
    tool_denied_json(
        tool_name,
        &format!("Path access denied by security policy: {}", path),
        "The path is outside the allowed workspace. Check path_access.json5 configuration.",
    )
}

/// Format the user-friendly LLM error message after all retries are exhausted.
pub(crate) fn llm_exhausted_error(max_retries: u32, error: &blockcell_core::Error) -> String {
    format!(
        "抱歉，我在处理你的请求时遇到了问题（已重试 {} 次）。\n\n\
         错误信息：{}\n\n\
         这可能是临时的网络或服务问题，请稍后再试。如果问题持续，我会自动学习并改进。",
        max_retries, error
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
    /// Domain/resource-level miss — the requested object/path/page does not exist.
    ResourceMissing,
}

/// Classify a tool error result string into transient or permanent.
pub(crate) fn classify_tool_failure(result: &str) -> ToolFailureKind {
    let lower = result.to_ascii_lowercase();

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
    fn test_tool_denied_json_structure() {
        let result = tool_denied_json("test_tool", "some error", "some hint");
        let val: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(val["tool"], "test_tool");
        assert_eq!(val["error"], "some error");
        assert_eq!(val["hint"], "some hint");
    }
}
