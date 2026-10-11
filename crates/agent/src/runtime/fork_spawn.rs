use super::*;

pub(crate) fn resolve_typed_workspace_scope(
    workspace: &std::path::Path,
    requested: &[String],
) -> Vec<crate::forked::WorkspaceScopeEntry> {
    requested
        .iter()
        .map(|raw| {
            let path = workspace.join(raw.trim_end_matches(['/', '\\']));
            if raw.ends_with(['/', '\\']) || path.is_dir() {
                crate::forked::WorkspaceScopeEntry::directory(path)
            } else {
                crate::forked::WorkspaceScopeEntry::file(path)
            }
        })
        .collect()
}

pub(crate) fn normalize_typed_agent_result(agent_type: &str, content: &str) -> String {
    const MAX_SUMMARY_CHARS: usize = 2_000;
    let trimmed = content.trim();
    let json_text = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    let parsed = serde_json::from_str::<serde_json::Value>(json_text).ok();
    let files_changed = parsed
        .as_ref()
        .and_then(|value| value.get("files_changed"))
        .filter(|value| value.is_array())
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let tests_run = parsed
        .as_ref()
        .and_then(|value| value.get("tests_run"))
        .filter(|value| value.is_array())
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let mut summary = parsed
        .as_ref()
        .and_then(|value| value.get("summary"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(trimmed)
        .to_string();
    if let Some(findings) = parsed
        .as_ref()
        .and_then(|value| value.get("findings"))
        .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
    {
        summary.push_str("\nfindings: ");
        summary.push_str(&findings.to_string());
    }
    if summary.chars().count() > MAX_SUMMARY_CHARS {
        summary = format!(
            "{}... [truncated typed-agent summary]",
            summary.chars().take(MAX_SUMMARY_CHARS).collect::<String>()
        );
    }
    let status = parsed
        .as_ref()
        .and_then(|value| value.get("status"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("completed");

    serde_json::json!({
        "files_changed": files_changed,
        "summary": summary,
        "tests_run": tests_run,
        "status": status,
        "agent_type": agent_type,
    })
    .to_string()
}

// Additional AgentRuntime methods for typed agent support
impl AgentRuntime {
    /// Fork 模式执行（省略 subagent_type 触发）
    ///
    /// Sanitize prompt for fork_directive to prevent injection attacks.
    /// Truncates to max length and strips control characters that could
    /// break the directive format.
    pub(crate) fn sanitize_fork_prompt(prompt: &str) -> String {
        const MAX_FORK_PROMPT_LEN: usize = 4000;
        let sanitized: String = prompt
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .take(MAX_FORK_PROMPT_LEN)
            .collect::<String>()
            // 防止闭合标签注入：替换 </fork_directive> 避免提前终止指令
            .replace("</fork_directive>", "<\\/fork_directive>");
        if prompt.len() > MAX_FORK_PROMPT_LEN {
            format!("{}[...truncated]", sanitized)
        } else {
            sanitized
        }
    }
}
