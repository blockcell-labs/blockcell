use blockcell_core::types::ChatMessage;
use serde_json::Value;
use std::hash::{Hash, Hasher};

pub fn prompt_cache_key(messages: &[ChatMessage]) -> Option<String> {
    prompt_cache_key_with_tools(messages, &[], "")
}

pub fn prompt_cache_key_with_tools(
    messages: &[ChatMessage],
    tools: &[Value],
    model: &str,
) -> Option<String> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut found_system = false;
    for message in messages.iter().filter(|message| message.role == "system") {
        message.content.to_string().hash(&mut hasher);
        found_system = true;
    }
    if !found_system {
        return None;
    }
    if !model.is_empty() {
        model.hash(&mut hasher);
    }
    for tool in tools {
        if let Ok(canonical) = serde_json::to_string(tool) {
            canonical.hash(&mut hasher);
        }
    }
    Some(format!("blockcell-{:016x}", hasher.finish()))
}

/// 构建工具描述文本，注入到 system prompt 中。
/// 用于不支持原生 tool calling 的模型（回退到文本格式）。
pub fn build_tools_prompt(tools: &[Value]) -> String {
    let mut s = String::new();
    s.push_str("\n\n## Available Tools\n");
    s.push_str("You MUST use tools to accomplish tasks. To call a tool, output a `<tool_call>` block with JSON inside.\n");
    s.push_str("You may call multiple tools in one response. Each call must be a separate `<tool_call>` block.\n\n");
    s.push_str("Format (you MUST follow this exact format):\n```\n<tool_call>\n{\"name\": \"tool_name\", \"arguments\": {\"param1\": \"value1\"}}\n</tool_call>\n```\n\n");
    s.push_str("IMPORTANT RULES:\n");
    s.push_str("- When the user asks you to do something that requires a tool, you MUST output <tool_call> blocks. Do NOT just describe what you would do.\n");
    s.push_str("- After outputting tool calls, STOP and wait for the results. Do NOT guess or fabricate results.\n");
    s.push_str("- If you don't need any tool, just respond normally with text.\n");
    s.push_str("- For web content, use web_fetch. For search, use web_search.\n\n");
    s.push_str("Tools:\n");

    for tool in tools {
        if let Some(func) = tool.get("function") {
            let name = func
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let desc = func
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let params = func.get("parameters").cloned().unwrap_or(Value::Null);
            s.push_str(&format!("### {}\n", name));
            s.push_str(&format!("{}\n", desc));
            if !params.is_null() {
                if let Ok(params_str) = serde_json::to_string_pretty(&params) {
                    s.push_str(&format!("Parameters: {}\n", params_str));
                }
            }
            s.push('\n');
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_cache_key_tracks_stable_system_prefix() {
        let first = vec![
            ChatMessage::system("stable rules"),
            ChatMessage::user("first dynamic query"),
        ];
        let second = vec![
            ChatMessage::system("stable rules"),
            ChatMessage::user("second dynamic query"),
        ];
        let changed = vec![
            ChatMessage::system("changed rules"),
            ChatMessage::user("second dynamic query"),
        ];

        assert_eq!(prompt_cache_key(&first), prompt_cache_key(&second));
        assert_ne!(prompt_cache_key(&first), prompt_cache_key(&changed));
        assert!(prompt_cache_key(&first).unwrap().starts_with("blockcell-"));
    }

    #[test]
    fn prompt_cache_key_with_tools_changes_when_tools_or_model_differ() {
        let messages = vec![
            ChatMessage::system("stable rules"),
            ChatMessage::user("query"),
        ];
        let tools_a = vec![serde_json::json!({
            "type": "function",
            "function": {"name": "read_file", "description": "Read file", "parameters": {}}
        })];
        let tools_b = vec![serde_json::json!({
            "type": "function",
            "function": {"name": "read_file", "description": "Read file v2", "parameters": {}}
        })];

        let key_base = prompt_cache_key_with_tools(&messages, &tools_a, "claude-sonnet-4-5");
        let key_same = prompt_cache_key_with_tools(&messages, &tools_a, "claude-sonnet-4-5");
        let key_diff_tools = prompt_cache_key_with_tools(&messages, &tools_b, "claude-sonnet-4-5");
        let key_diff_model = prompt_cache_key_with_tools(&messages, &tools_a, "gpt-4o");

        assert_eq!(key_base, key_same);
        assert_ne!(key_base, key_diff_tools);
        assert_ne!(key_base, key_diff_model);
    }
}
