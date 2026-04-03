use async_trait::async_trait;
use blockcell_core::config::ToolCallMode;
use blockcell_core::types::{
    ChatMessage, LLMResponse, StreamChunk, ToolCallAccumulator, ToolCallRequest,
};
use blockcell_core::{Error, Result};
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::client::build_http_client;
use crate::Provider;

/// Find the largest byte index <= `max_bytes` that is a valid char boundary.
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    api_base: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
    tool_call_mode: AtomicU8,
}

impl OpenAIProvider {
    fn sanitize_messages_for_native_tools(messages: &[ChatMessage]) -> Vec<ChatMessage> {
        let mut sanitized = Vec::with_capacity(messages.len());
        let mut i = 0;

        while i < messages.len() {
            let msg = &messages[i];

            if msg.role == "tool" {
                i += 1;
                continue;
            }

            if msg.role == "assistant" {
                if let Some(tool_calls) = &msg.tool_calls {
                    if !tool_calls.is_empty() {
                        let expected_ids: std::collections::HashSet<&str> =
                            tool_calls.iter().map(|tc| tc.id.as_str()).collect();
                        let mut found_ids = std::collections::HashSet::new();
                        let mut j = i + 1;

                        while j < messages.len() && messages[j].role == "tool" {
                            if let Some(id) = messages[j].tool_call_id.as_deref() {
                                if expected_ids.contains(id) {
                                    found_ids.insert(id);
                                }
                            }
                            j += 1;
                        }

                        if expected_ids.iter().all(|id| found_ids.contains(id)) {
                            sanitized.push(msg.clone());
                            for tool_msg in &messages[(i + 1)..j] {
                                if let Some(id) = tool_msg.tool_call_id.as_deref() {
                                    if expected_ids.contains(id) {
                                        sanitized.push(tool_msg.clone());
                                    }
                                }
                            }
                        } else {
                            debug!(
                                expected = expected_ids.len(),
                                found = found_ids.len(),
                                "Dropping incomplete assistant tool-call round before native tool request"
                            );
                        }

                        i = j;
                        continue;
                    }
                }
            }

            sanitized.push(msg.clone());
            i += 1;
        }

        sanitized
    }

    pub fn new(
        api_key: &str,
        api_base: Option<&str>,
        model: &str,
        max_tokens: u32,
        temperature: f32,
    ) -> Self {
        Self::new_with_proxy(
            api_key,
            api_base,
            model,
            max_tokens,
            temperature,
            None,
            None,
            &[],
            ToolCallMode::Native,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_proxy(
        api_key: &str,
        api_base: Option<&str>,
        model: &str,
        max_tokens: u32,
        temperature: f32,
        provider_proxy: Option<&str>,
        global_proxy: Option<&str>,
        no_proxy: &[String],
        tool_call_mode: ToolCallMode,
    ) -> Self {
        let resolved_base = api_base
            .unwrap_or("https://api.openai.com/v1")
            .trim_end_matches('/')
            .to_string();
        let client = build_http_client(
            provider_proxy,
            global_proxy,
            no_proxy,
            &resolved_base,
            Duration::from_secs(120),
        );
        Self {
            client,
            api_key: api_key.to_string(),
            api_base: resolved_base,
            model: model.to_string(),
            max_tokens,
            temperature,
            tool_call_mode: AtomicU8::new(Self::mode_to_u8(tool_call_mode)),
        }
    }

    fn mode_to_u8(mode: ToolCallMode) -> u8 {
        match mode {
            ToolCallMode::Native => 0,
            ToolCallMode::Text => 1,
            ToolCallMode::None => 2,
            ToolCallMode::Auto => 3,
        }
    }

    fn mode_from_u8(mode: u8) -> ToolCallMode {
        match mode {
            1 => ToolCallMode::Text,
            2 => ToolCallMode::None,
            3 => ToolCallMode::Auto,
            _ => ToolCallMode::Native,
        }
    }

    /// Build a text description of tools to inject into the system prompt.
    fn build_tools_prompt(tools: &[Value]) -> String {
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

    /// Parse text-based tool call blocks from the response content.
    /// Handles multiple formats:
    /// - `{"name":"...","arguments":{...}}`
    /// - `[TOOL_CALL]{tool => "...", args => {...}}[/TOOL_CALL]`
    ///
    /// Returns (remaining_text, parsed_tool_calls).
    fn parse_function_parameter_tool_block(
        block: &str,
        call_index: u64,
    ) -> Option<ToolCallRequest> {
        let trimmed = block.trim();
        let lower = trimmed.to_lowercase();

        let func_start = lower.find("<function=")?;
        let after_func = &trimmed[func_start + "<function=".len()..];
        let func_end = after_func.find('>')?;
        let tool_name = after_func[..func_end].trim().to_string();
        if tool_name.is_empty() {
            return None;
        }

        let body = &after_func[func_end + 1..];
        let body_lower = body.to_lowercase();
        let body_end = body_lower.find("</function>").unwrap_or(body.len());
        let params_str = &body[..body_end];

        let mut args = serde_json::Map::new();
        let mut scan = params_str;

        loop {
            let scan_lower = scan.to_lowercase();
            let Some(param_start) = scan_lower.find("<parameter=") else {
                break;
            };

            let after_param = &scan[param_start + "<parameter=".len()..];
            let Some(param_name_end) = after_param.find('>') else {
                break;
            };

            let param_name = after_param[..param_name_end].trim().to_string();
            if param_name.is_empty() {
                scan = &after_param[param_name_end + 1..];
                continue;
            }

            let value_str = &after_param[param_name_end + 1..];
            let value_lower = value_str.to_lowercase();
            let Some(close_idx) = value_lower.find("</parameter>") else {
                break;
            };

            let raw_value = value_str[..close_idx].trim();
            let json_val = serde_json::from_str::<Value>(raw_value)
                .unwrap_or_else(|_| Value::String(raw_value.to_string()));
            args.insert(param_name, json_val);

            scan = &value_str[close_idx + "</parameter>".len()..];
        }

        Some(ToolCallRequest {
            id: format!("text_call_{}", call_index),
            name: tool_name,
            arguments: Value::Object(args),
            thought_signature: None,
        })
    }

    fn strip_wrapping_quotes(input: &str) -> String {
        let trimmed = input.trim();
        if trimmed.len() >= 2 {
            let bytes = trimmed.as_bytes();
            let first = bytes[0];
            let last = bytes[trimmed.len() - 1];
            if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
                return trimmed[1..trimmed.len() - 1].to_string();
            }
        }
        trimmed.to_string()
    }

    fn parse_native_scalar_value(input: &str) -> Value {
        let trimmed = input.trim();
        if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
            return parsed;
        }

        let unquoted = Self::strip_wrapping_quotes(trimmed);
        if unquoted.eq_ignore_ascii_case("true") {
            return Value::Bool(true);
        }
        if unquoted.eq_ignore_ascii_case("false") {
            return Value::Bool(false);
        }
        if unquoted.eq_ignore_ascii_case("null") {
            return Value::Null;
        }
        if let Ok(parsed) = unquoted.parse::<i64>() {
            return serde_json::json!(parsed);
        }
        if let Ok(parsed) = unquoted.parse::<f64>() {
            return serde_json::json!(parsed);
        }

        Value::String(unquoted)
    }

    fn parse_parameter_block_arguments(raw: &str) -> Option<Map<String, Value>> {
        let mut args = Map::new();
        let mut scan = raw;

        loop {
            let scan_lower = scan.to_lowercase();
            let Some(param_start) = scan_lower.find("<parameter=") else {
                break;
            };

            let after_param = &scan[param_start + "<parameter=".len()..];
            let Some(param_name_end) = after_param.find('>') else {
                break;
            };

            let param_name = after_param[..param_name_end].trim().to_string();
            if param_name.is_empty() {
                scan = &after_param[param_name_end + 1..];
                continue;
            }

            let value_str = &after_param[param_name_end + 1..];
            let value_lower = value_str.to_lowercase();
            let Some(close_idx) = value_lower.find("</parameter>") else {
                break;
            };

            let raw_value = value_str[..close_idx].trim();
            args.insert(param_name, Self::parse_native_scalar_value(raw_value));
            scan = &value_str[close_idx + "</parameter>".len()..];
        }

        if args.is_empty() { None } else { Some(args) }
    }

    fn parse_loose_argument_map(raw: &str) -> Option<Map<String, Value>> {
        if raw.contains("://")
            && !raw.trim_start().starts_with('{')
            && !raw.contains("=>")
            && !raw.contains(',')
            && !raw.starts_with("--")
        {
            return None;
        }

        let trimmed = raw
            .trim()
            .trim_start_matches('{')
            .trim_end_matches('}')
            .trim();
        if trimmed.is_empty() {
            return Some(Map::new());
        }

        let mut args = Map::new();
        for pair in trimmed.split(',') {
            let entry = pair.trim();
            if entry.is_empty() {
                continue;
            }

            let parsed = entry
                .split_once("=>")
                .or_else(|| entry.split_once(':'))
                .or_else(|| entry.split_once('='));
            let (raw_key, raw_value) = parsed?;

            let key = Self::strip_wrapping_quotes(raw_key.trim());
            if key.is_empty() {
                return None;
            }

            args.insert(key, Self::parse_native_scalar_value(raw_value.trim()));
        }

        Some(args)
    }

    fn single_required_string_param_name(tool_name: &str, tools: &[Value]) -> Option<String> {
        tools.iter().find_map(|tool| {
            let function = tool.get("function")?;
            let name = function.get("name")?.as_str()?;
            if name != tool_name {
                return None;
            }

            let parameters = function.get("parameters")?;
            let required = parameters.get("required")?.as_array()?;
            if required.len() != 1 {
                return None;
            }
            let param_name = required.first()?.as_str()?.to_string();
            let param_type = parameters
                .get("properties")
                .and_then(|p| p.get(&param_name))
                .and_then(|p| p.get("type"))
                .and_then(|t| t.as_str());
            if param_type == Some("string") {
                Some(param_name)
            } else {
                None
            }
        })
    }

    fn parse_native_tool_arguments(
        tool_name: &str,
        raw_arguments: &str,
        tools: &[Value],
    ) -> std::result::Result<Value, String> {
        let trimmed = raw_arguments.trim();
        if trimmed.is_empty() {
            return Ok(Value::Object(Map::new()));
        }

        if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
            return Ok(parsed);
        }

        if let Some(args) = Self::parse_parameter_block_arguments(trimmed) {
            return Ok(Value::Object(args));
        }

        if let Some(args) = Self::parse_loose_argument_map(trimmed) {
            return Ok(Value::Object(args));
        }

        let dash_args = Self::parse_dash_args(trimmed);
        if dash_args
            .as_object()
            .is_some_and(|map| !map.is_empty())
        {
            return Ok(dash_args);
        }

        if let Some(param_name) = Self::single_required_string_param_name(tool_name, tools) {
            let value = Self::strip_wrapping_quotes(trimmed);
            if !value.is_empty() {
                let mut args = Map::new();
                args.insert(param_name, Value::String(value));
                return Ok(Value::Object(args));
            }
        }

        Err(format!(
            "unrecognized native tool-call arguments: {}",
            &trimmed[..truncate_at_char_boundary(trimmed, 200)]
        ))
    }

    fn parse_text_tool_calls(content: &str) -> (String, Vec<ToolCallRequest>) {
        let mut tool_calls = Vec::new();
        let mut remaining = String::new();
        let mut rest = content;
        let mut call_index = 0u64;

        // Pass 1: extract <tool_call>...</tool_call> blocks
        loop {
            if let Some(start) = rest.find("<tool_call>") {
                remaining.push_str(&rest[..start]);
                let after_tag = &rest[start + "<tool_call>".len()..];
                if let Some(end) = after_tag.find("</tool_call>") {
                    let block = after_tag[..end].trim();
                    if let Ok(val) = serde_json::from_str::<Value>(block) {
                        let name = val
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let arguments = val
                            .get("arguments")
                            .cloned()
                            .unwrap_or(Value::Object(serde_json::Map::new()));
                        tool_calls.push(ToolCallRequest {
                            id: format!("text_call_{}", call_index),
                            name,
                            arguments,
                            thought_signature: None,
                        });
                        call_index += 1;
                    } else if let Some(tc) =
                        Self::parse_function_parameter_tool_block(block, call_index)
                    {
                        tool_calls.push(tc);
                        call_index += 1;
                    } else {
                        warn!(json = %block, "Failed to parse tool_call JSON");
                        remaining.push_str(
                            &rest[start..start + "<tool_call>".len() + end + "</tool_call>".len()],
                        );
                    }
                    rest = &after_tag[end + "</tool_call>".len()..];
                } else {
                    remaining.push_str(&rest[start..]);
                    break;
                }
            } else {
                remaining.push_str(rest);
                break;
            }
        }

        // Pass 2: extract [TOOL_CALL]...[/TOOL_CALL] blocks from remaining
        // Some models (e.g. xminimaxm25) use this format with non-JSON arrow syntax.
        if tool_calls.is_empty() {
            let mut pass2_remaining = String::new();
            let mut rest2 = remaining.as_str();
            loop {
                let lower = rest2.to_lowercase();
                if let Some(start) = lower.find("[tool_call]") {
                    pass2_remaining.push_str(&rest2[..start]);
                    let after_tag = &rest2[start + "[tool_call]".len()..];
                    let after_lower = after_tag.to_lowercase();
                    if let Some(end) = after_lower.find("[/tool_call]") {
                        let block = after_tag[..end].trim();
                        if let Some(tc) = Self::parse_nonstandard_tool_block(block, call_index) {
                            tool_calls.push(tc);
                            call_index += 1;
                        } else {
                            warn!(block = %block, "Failed to parse [TOOL_CALL] block");
                        }
                        rest2 = &after_tag[end + "[/tool_call]".len()..];
                    } else {
                        // No closing tag — try to parse what's left
                        let block = after_tag.trim();
                        if let Some(tc) = Self::parse_nonstandard_tool_block(block, call_index) {
                            tool_calls.push(tc);
                        }
                        break;
                    }
                } else {
                    pass2_remaining.push_str(rest2);
                    break;
                }
            }
            remaining = pass2_remaining;
        }

        // Pass 3: extract <minimax:tool_call> / [Called: name] ... </minimax:tool_call> blocks.
        // Format observed from xminimaxm25:
        //   [Called: exec]\n<parameter name="command">...</parameter>\n</invoke>\n</minimax:tool_call>
        // Also handles bare [Called: name] with following <parameter> tags (no closing tag).
        if tool_calls.is_empty() {
            let mut pass3_remaining = String::new();
            let mut rest3 = remaining.as_str();
            loop {
                // Look for [Called: <name>] prefix
                let lower3 = rest3.to_lowercase();
                if let Some(called_start) = lower3.find("[called:") {
                    pass3_remaining.push_str(&rest3[..called_start]);
                    let after_called = &rest3[called_start + "[called:".len()..];
                    // Extract tool name up to ']'
                    if let Some(bracket_end) = after_called.find(']') {
                        let tool_name = after_called[..bracket_end].trim().to_string();
                        let after_bracket = &after_called[bracket_end + 1..];
                        // Find end of this block: </minimax:tool_call> or </invoke>
                        let lower_after = after_bracket.to_lowercase();
                        let block_end = lower_after
                            .find("</minimax:tool_call>")
                            .or_else(|| lower_after.find("</invoke>"));
                        let (params_str, consumed) = if let Some(end) = block_end {
                            let tag_len = if lower_after[end..].starts_with("</minimax:tool_call>")
                            {
                                "</minimax:tool_call>".len()
                            } else {
                                "</invoke>".len()
                            };
                            (&after_bracket[..end], end + tag_len)
                        } else {
                            (after_bracket, after_bracket.len())
                        };
                        // Parse <parameter name="key">value</parameter> pairs
                        let mut args = serde_json::Map::new();
                        let mut scan = params_str;
                        loop {
                            let sl = scan.to_lowercase();
                            if let Some(p_start) = sl.find("<parameter") {
                                let after_p = &scan[p_start + "<parameter".len()..];
                                // Extract name="..."
                                if let Some(name_start) = after_p.find("name=\"") {
                                    let after_name = &after_p[name_start + "name=\"".len()..];
                                    if let Some(name_end) = after_name.find('"') {
                                        let param_name = after_name[..name_end].to_string();
                                        // Find > then value then </parameter>
                                        if let Some(gt) = after_p.find('>') {
                                            let value_str = &after_p[gt + 1..];
                                            let vl = value_str.to_lowercase();
                                            if let Some(close) = vl.find("</parameter>") {
                                                let value = value_str[..close].to_string();
                                                args.insert(
                                                    param_name,
                                                    serde_json::Value::String(value),
                                                );
                                                scan = &value_str[close + "</parameter>".len()..];
                                                continue;
                                            }
                                        }
                                    }
                                }
                                // Couldn't parse this parameter, skip past it
                                scan = &scan[p_start + "<parameter".len()..];
                            } else {
                                break;
                            }
                        }
                        if !tool_name.is_empty() {
                            tool_calls.push(ToolCallRequest {
                                id: format!("text_call_{}", call_index),
                                name: tool_name,
                                arguments: serde_json::Value::Object(args),
                                thought_signature: None,
                            });
                            call_index += 1;
                        }
                        rest3 = &after_called[bracket_end + 1 + consumed..];
                    } else {
                        pass3_remaining.push_str(&rest3[called_start..]);
                        break;
                    }
                } else {
                    pass3_remaining.push_str(rest3);
                    break;
                }
            }
            remaining = pass3_remaining;
        }

        // Pass 4: extract <invoke name="tool_name">...<parameter name="key">value</parameter>...</invoke>
        // with optional </minimax:tool_call> wrapper.
        // Format observed from xminimaxm25:
        //   <invoke name="list_skills">\n</invoke>\n</minimax:tool_call>
        //   <invoke name="exec">\n<parameter name="command">ls -la</parameter>\n</invoke>\n</minimax:tool_call>
        if tool_calls.is_empty() {
            let mut pass4_remaining = String::new();
            let mut rest4 = remaining.as_str();
            loop {
                let lower4 = rest4.to_lowercase();
                if let Some(invoke_start) = lower4.find("<invoke") {
                    pass4_remaining.push_str(&rest4[..invoke_start]);
                    let after_invoke = &rest4[invoke_start + "<invoke".len()..];
                    // Extract name="..." from the <invoke> tag
                    if let Some(name_attr_start) = after_invoke.find("name=\"") {
                        let after_name = &after_invoke[name_attr_start + "name=\"".len()..];
                        if let Some(name_end) = after_name.find('"') {
                            let tool_name = after_name[..name_end].trim().to_string();
                            // Find the > that closes the <invoke ...> tag
                            let tag_content_start =
                                &after_invoke[name_attr_start + "name=\"".len() + name_end + 1..];
                            if let Some(gt_pos) = tag_content_start.find('>') {
                                let body = &tag_content_start[gt_pos + 1..];
                                // Find </invoke> end
                                let body_lower = body.to_lowercase();
                                let invoke_end = body_lower.find("</invoke>");
                                let (params_str, after_body) = if let Some(end) = invoke_end {
                                    (&body[..end], &body[end + "</invoke>".len()..])
                                } else {
                                    (body, "")
                                };
                                // Parse <parameter name="key">value</parameter> pairs
                                let mut args = serde_json::Map::new();
                                let mut scan = params_str;
                                loop {
                                    let sl = scan.to_lowercase();
                                    if let Some(p_start) = sl.find("<parameter") {
                                        let after_p = &scan[p_start + "<parameter".len()..];
                                        if let Some(ns) = after_p.find("name=\"") {
                                            let an = &after_p[ns + "name=\"".len()..];
                                            if let Some(ne) = an.find('"') {
                                                let param_name = an[..ne].to_string();
                                                if let Some(gt) = after_p.find('>') {
                                                    let value_str = &after_p[gt + 1..];
                                                    let vl = value_str.to_lowercase();
                                                    if let Some(close) = vl.find("</parameter>") {
                                                        let value = value_str[..close].to_string();
                                                        // Try to parse as JSON value (number, bool, etc.)
                                                        let json_val =
                                                            serde_json::from_str::<Value>(&value)
                                                                .unwrap_or(Value::String(value));
                                                        args.insert(param_name, json_val);
                                                        scan = &value_str
                                                            [close + "</parameter>".len()..];
                                                        continue;
                                                    }
                                                }
                                            }
                                        }
                                        scan = &scan[p_start + "<parameter".len()..];
                                    } else {
                                        break;
                                    }
                                }
                                if !tool_name.is_empty() {
                                    tool_calls.push(ToolCallRequest {
                                        id: format!("text_call_{}", call_index),
                                        name: tool_name,
                                        arguments: Value::Object(args),
                                        thought_signature: None,
                                    });
                                    call_index += 1;
                                }
                                // Skip optional </minimax:tool_call> after </invoke>
                                let trimmed_after = after_body.trim_start();
                                rest4 = if trimmed_after
                                    .to_lowercase()
                                    .starts_with("</minimax:tool_call>")
                                {
                                    &trimmed_after["</minimax:tool_call>".len()..]
                                } else {
                                    after_body
                                };
                                continue;
                            }
                        }
                    }
                    // Couldn't parse this <invoke>, skip it
                    pass4_remaining.push_str(&rest4[invoke_start..invoke_start + "<invoke".len()]);
                    rest4 = &rest4[invoke_start + "<invoke".len()..];
                } else {
                    pass4_remaining.push_str(rest4);
                    break;
                }
            }
            remaining = pass4_remaining;
        }

        let remaining = remaining.trim().to_string();
        (remaining, tool_calls)
    }

    /// Parse a non-standard tool call block content.
    /// Handles formats like:
    ///   {tool => "memory_query", args => { --top_k 20 }}
    ///   {"name": "memory_query", "arguments": {"top_k": 20}}
    fn parse_nonstandard_tool_block(block: &str, index: u64) -> Option<ToolCallRequest> {
        // Try standard JSON first
        if let Ok(val) = serde_json::from_str::<Value>(block) {
            let name = val
                .get("name")
                .or_else(|| val.get("tool"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let arguments = val
                .get("arguments")
                .or_else(|| val.get("args"))
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new()));
            return Some(ToolCallRequest {
                id: format!("text_call_{}", index),
                name,
                arguments,
                thought_signature: None,
            });
        }

        // Parse arrow-style: {tool => "name", args => { --key value }}
        // Only strip the outermost brace pair (trim_end_matches is greedy and would strip all)
        let trimmed = block.trim();
        let inner = if trimmed.starts_with('{') && trimmed.ends_with('}') {
            &trimmed[1..trimmed.len() - 1]
        } else {
            trimmed
        };
        let inner = inner.trim();

        // Extract tool name: tool => "name" or tool => name
        let tool_name = Self::extract_arrow_value(inner, "tool")
            .or_else(|| Self::extract_arrow_value(inner, "name"));
        let tool_name = tool_name?;

        // Extract args block
        let args = Self::extract_arrow_args(inner);

        Some(ToolCallRequest {
            id: format!("text_call_{}", index),
            name: tool_name,
            arguments: args,
            thought_signature: None,
        })
    }

    /// Extract a string value from arrow syntax: `key => "value"` or `key => value`
    fn extract_arrow_value(text: &str, key: &str) -> Option<String> {
        // Match: key => "value" or key =\u003e "value" (escaped >)
        let patterns = [
            format!("{} =>", key),
            format!("{} =\\u003e", key), // JSON-escaped >
        ];
        for pat in &patterns {
            if let Some(pos) = text.find(pat.as_str()) {
                let after = text[pos + pat.len()..].trim();
                // Quoted value
                if let Some(after) = after.strip_prefix('"') {
                    if let Some(end_quote) = after.find('"') {
                        return Some(after[..end_quote].to_string());
                    }
                }
                // Unquoted value — take until comma or whitespace
                let val: String = after
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != ',' && *c != '}')
                    .collect();
                if !val.is_empty() {
                    return Some(val.trim_matches('"').to_string());
                }
            }
        }
        None
    }

    /// Extract args from arrow syntax: `args => { --key1 val1\n --key2 val2 }`
    /// or `args => {"key": "value"}` (JSON inside)
    fn extract_arrow_args(text: &str) -> Value {
        let args_markers = ["args =>", "arguments =>"];
        for marker in &args_markers {
            if let Some(pos) = text.find(marker) {
                let after = text[pos + marker.len()..].trim();
                // Find the args block between { }
                if after.starts_with('{') {
                    // Find matching closing brace
                    let mut depth = 0;
                    let mut end_pos = 0;
                    for (i, ch) in after.char_indices() {
                        match ch {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    end_pos = i;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    if end_pos > 0 {
                        let args_block = &after[1..end_pos].trim();
                        // Try JSON first
                        let json_attempt = format!("{{{}}}", args_block);
                        if let Ok(val) = serde_json::from_str::<Value>(&json_attempt) {
                            return val;
                        }
                        // Parse --key value pairs
                        return Self::parse_dash_args(args_block);
                    }
                }
                // Bare value after =>
                let val: String = after
                    .chars()
                    .take_while(|c| *c != ',' && *c != '}')
                    .collect();
                let val = val.trim();
                if !val.is_empty() {
                    let mut map = serde_json::Map::new();
                    map.insert("value".to_string(), Value::String(val.to_string()));
                    return Value::Object(map);
                }
            }
        }
        Value::Object(serde_json::Map::new())
    }

    /// Parse `--key value` or `--key` pairs into a JSON object.
    fn parse_dash_args(text: &str) -> Value {
        let mut map = serde_json::Map::new();
        let mut current_key: Option<String> = None;
        let mut current_val_parts: Vec<String> = Vec::new();

        let flush = |key: &mut Option<String>,
                     parts: &mut Vec<String>,
                     map: &mut serde_json::Map<String, Value>| {
            if let Some(k) = key.take() {
                let val_str = parts.join(" ");
                let val_str = val_str.trim().trim_matches('"').trim();
                if val_str.is_empty() {
                    map.insert(k, Value::Bool(true));
                } else if let Ok(n) = val_str.parse::<i64>() {
                    map.insert(k, Value::Number(n.into()));
                } else if let Ok(f) = val_str.parse::<f64>() {
                    if let Some(n) = serde_json::Number::from_f64(f) {
                        map.insert(k, Value::Number(n));
                    } else {
                        map.insert(k, Value::String(val_str.to_string()));
                    }
                } else if val_str == "true" {
                    map.insert(k, Value::Bool(true));
                } else if val_str == "false" {
                    map.insert(k, Value::Bool(false));
                } else {
                    map.insert(k, Value::String(val_str.to_string()));
                }
                parts.clear();
            }
        };

        for token in text.split_whitespace() {
            if let Some(key_name) = token.strip_prefix("--") {
                flush(&mut current_key, &mut current_val_parts, &mut map);
                current_key = Some(key_name.to_string());
            } else if current_key.is_some() {
                current_val_parts.push(token.to_string());
            }
        }
        flush(&mut current_key, &mut current_val_parts, &mut map);

        Value::Object(map)
    }

    /// Inject tool descriptions into the system message of the messages list.
    fn inject_tools_into_messages(messages: &[ChatMessage], tools: &[Value]) -> Vec<ChatMessage> {
        let tools_prompt = Self::build_tools_prompt(tools);
        let mut result = messages.to_vec();

        // Find the system message and append tools to it
        if let Some(sys_msg) = result.first_mut() {
            if sys_msg.role == "system" {
                if let Some(text) = sys_msg.content.as_str() {
                    sys_msg.content = Value::String(format!("{}{}", text, tools_prompt));
                }
                return result;
            }
        }

        // No system message found, prepend one
        result.insert(0, ChatMessage::system(&tools_prompt));
        result
    }

    /// Send a chat request to the API.
    async fn send_request(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        use_native_tools: bool,
    ) -> Result<(ChatResponse, String)> {
        let url = format!("{}/chat/completions", self.api_base);

        let (api_messages, api_tools) = if use_native_tools && !tools.is_empty() {
            (
                Self::sanitize_messages_for_native_tools(messages),
                tools.to_vec(),
            )
        } else if !tools.is_empty() {
            // Text-based tool mode: inject tools into system prompt, don't send tools param
            (Self::inject_tools_into_messages(messages, tools), vec![])
        } else {
            (messages.to_vec(), vec![])
        };

        let request = ChatRequest {
            model: self.model.clone(),
            messages: api_messages,
            tools: api_tools,
            tool_choice: if use_native_tools && !tools.is_empty() {
                Some("auto".to_string())
            } else {
                None
            },
            max_tokens: self.max_tokens,
            temperature: self.temperature,
        };

        let mode = if use_native_tools && !tools.is_empty() {
            "native"
        } else if !tools.is_empty() {
            "text"
        } else {
            "no-tools"
        };
        info!(url = %url, model = %self.model, tools_count = tools.len(), messages_count = messages.len(), mode = %mode, "Calling LLM");

        let request_body = serde_json::to_string(&request)
            .map_err(|e| Error::Provider(format!("Failed to serialize request: {}", e)))?;
        debug!(body_len = request_body.len(), "Request body prepared");
        debug!(target: "chat::request", request_body, "Request detail");

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .body(request_body)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("Request failed: {}", e)))?;

        let status = response.status();
        let raw_body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            error!(status = %status, body = %raw_body, "LLM API error");
            return Err(Error::Provider(format!(
                "API error {}: {}",
                status, raw_body
            )));
        }

        {
            let trimmed = raw_body.trim_start();
            let end = truncate_at_char_boundary(trimmed, 500);
            info!(body_len = raw_body.len(), preview = %&trimmed[..end], "LLM raw response");
        }
        debug!(target: "chat::response", response = raw_body, "LLM response");

        let trimmed_body = raw_body.trim();
        if trimmed_body.is_empty() {
            return Err(Error::Provider(format!(
                "LLM returned an empty response body (status {}, len {})",
                status,
                raw_body.len()
            )));
        }

        let chat_response: ChatResponse = serde_json::from_str(trimmed_body).map_err(|e| {
            let end = truncate_at_char_boundary(trimmed_body, 500);
            Error::Provider(format!(
                "Failed to parse response: {}. Body: {}",
                e,
                &trimmed_body[..end]
            ))
        })?;

        Ok((chat_response, raw_body))
    }
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    max_tokens: u32,
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: Option<String>,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ToolCall {
    id: String,
    function: FunctionCall,
}

#[derive(Debug, Deserialize)]
struct FunctionCall {
    name: String,
    arguments: String,
}

/// SSE 流式响应结构
#[derive(Debug, Deserialize)]
struct StreamResponse {
    choices: Vec<StreamChoice>,
    usage: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    content: Option<String>,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Debug, Deserialize)]
struct StreamToolCall {
    index: usize,
    id: Option<String>,
    function: StreamFunctionCall,
}

#[derive(Debug, Deserialize)]
struct StreamFunctionCall {
    name: Option<String>,
    arguments: Option<String>,
}

/// 流式请求体
fn finalize_stream_response(
    mode: ToolCallMode,
    tools_available: bool,
    accumulated_content: String,
    accumulated_reasoning: String,
    mut tool_calls: Vec<ToolCallRequest>,
    finish_reason: String,
    usage: Value,
) -> LLMResponse {
    let mut content = accumulated_content;

    if tools_available
        && !matches!(mode, ToolCallMode::None)
        && tool_calls.is_empty()
        && !content.is_empty()
    {
        let (remaining_text, parsed_calls) = OpenAIProvider::parse_text_tool_calls(&content);
        if !parsed_calls.is_empty() {
            info!(
                count = parsed_calls.len(),
                "Parsed text-based tool calls from streaming response"
            );
            content = remaining_text;
            tool_calls = parsed_calls;
        }
    }

    let final_finish_reason = if tool_calls.is_empty() {
        finish_reason
    } else {
        "tool_calls".to_string()
    };

    LLMResponse {
        content: if content.is_empty() {
            None
        } else {
            Some(content)
        },
        reasoning_content: if accumulated_reasoning.is_empty() {
            None
        } else {
            Some(accumulated_reasoning)
        },
        tool_calls,
        finish_reason: final_finish_reason,
        usage,
    }
}

#[derive(Debug, Serialize)]
struct StreamRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    max_tokens: u32,
    temperature: f32,
    stream: bool,
}

#[async_trait]
impl Provider for OpenAIProvider {
    async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<LLMResponse> {
        let mode = Self::mode_from_u8(self.tool_call_mode.load(Ordering::Relaxed));

        if !tools.is_empty() && !matches!(mode, ToolCallMode::Text | ToolCallMode::None) {
            let (chat_response, _raw) = self.send_request(messages, tools, true).await?;

            let choice = chat_response
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| Error::Provider("No choices in response".to_string()))?;

            let native_tool_calls: Vec<ToolCallRequest> = choice
                .message
                .tool_calls
                .unwrap_or_default()
                .into_iter()
                .map(|tc| {
                    let arguments = Self::parse_native_tool_arguments(
                        &tc.function.name,
                        &tc.function.arguments,
                        tools,
                    )
                    .unwrap_or_else(|err| {
                        warn!(
                            tool = %tc.function.name,
                            error = %err,
                            raw_arguments = %tc.function.arguments,
                            "Failed to parse native tool-call arguments; falling back to empty object"
                        );
                        Value::Object(Map::new())
                    });
                    ToolCallRequest {
                        id: tc.id,
                        name: tc.function.name,
                        arguments,
                        thought_signature: None,
                    }
                })
                .collect();

            let content = choice.message.content.unwrap_or_default();
            let reasoning_content = choice.message.reasoning_content.clone();

            if !native_tool_calls.is_empty() || tools.is_empty() {
                return Ok(LLMResponse {
                    content: if content.is_empty() {
                        None
                    } else {
                        Some(content)
                    },
                    reasoning_content,
                    tool_calls: native_tool_calls,
                    finish_reason: choice.finish_reason.unwrap_or_else(|| "stop".to_string()),
                    usage: chat_response.usage.unwrap_or(Value::Null),
                });
            }

            let (remaining_text, parsed_calls) = Self::parse_text_tool_calls(&content);
            if !parsed_calls.is_empty() {
                info!(
                    count = parsed_calls.len(),
                    "Parsed text-based tool calls from native mode response"
                );
                if matches!(mode, ToolCallMode::Auto) {
                    self.tool_call_mode
                        .store(Self::mode_to_u8(ToolCallMode::Text), Ordering::Relaxed);
                }
                return Ok(LLMResponse {
                    content: if remaining_text.is_empty() {
                        None
                    } else {
                        Some(remaining_text)
                    },
                    reasoning_content,
                    tool_calls: parsed_calls,
                    finish_reason: "tool_calls".to_string(),
                    usage: chat_response.usage.unwrap_or(Value::Null),
                });
            }

            if !content.is_empty() || matches!(mode, ToolCallMode::Native) {
                return Ok(LLMResponse {
                    content: Some(content),
                    reasoning_content,
                    tool_calls: vec![],
                    finish_reason: choice.finish_reason.unwrap_or_else(|| "stop".to_string()),
                    usage: chat_response.usage.unwrap_or(Value::Null),
                });
            }

            warn!("Native tool call returned no tool_calls; falling back to text tool mode");
            self.tool_call_mode
                .store(Self::mode_to_u8(ToolCallMode::Text), Ordering::Relaxed);
        }

        let text_tools: &[Value] = if matches!(mode, ToolCallMode::None) {
            &[]
        } else {
            tools
        };
        let (chat_response, _raw) = self.send_request(messages, text_tools, false).await?;

        let choice = chat_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| Error::Provider("No choices in response".to_string()))?;

        let raw_content = choice.message.content.unwrap_or_default();

        // Parse tool calls from text content
        let (remaining_text, tool_calls) = if !tools.is_empty() {
            Self::parse_text_tool_calls(&raw_content)
        } else {
            (raw_content.clone(), vec![])
        };

        if !tool_calls.is_empty() {
            info!(count = tool_calls.len(), "Parsed text-based tool calls");
        }

        Ok(LLMResponse {
            content: if remaining_text.is_empty() {
                None
            } else {
                Some(remaining_text)
            },
            reasoning_content: choice.message.reasoning_content,
            tool_calls,
            finish_reason: choice.finish_reason.unwrap_or_else(|| "stop".to_string()),
            usage: chat_response.usage.unwrap_or(Value::Null),
        })
    }

    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
    ) -> Result<mpsc::Receiver<StreamChunk>> {
        let url = format!("{}/chat/completions", self.api_base);
        let mode = Self::mode_from_u8(self.tool_call_mode.load(Ordering::Relaxed));
        let tools_available = !tools.is_empty();

        let (api_messages, api_tools) =
            if tools_available && !matches!(mode, ToolCallMode::Text | ToolCallMode::None) {
                (Self::sanitize_messages_for_native_tools(messages), tools.to_vec())
            } else if tools_available {
                (Self::inject_tools_into_messages(messages, tools), vec![])
            } else {
                (messages.to_vec(), vec![])
            };

        let request = StreamRequest {
            model: self.model.clone(),
            messages: api_messages,
            tools: api_tools,
            tool_choice: if tools_available
                && !matches!(mode, ToolCallMode::Text | ToolCallMode::None)
            {
                Some("auto".to_string())
            } else {
                None
            },
            max_tokens: self.max_tokens,
            temperature: self.temperature,
            stream: true,
        };

        info!(url = %url, model = %self.model, "Starting streaming LLM call");
        debug!(target: "chat::request", request = serde_json::to_string(&request).unwrap_or_default(), "Request detail");

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("Stream request failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Provider(format!(
                "Stream API error {}: {}",
                status, body
            )));
        }

        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut tool_calls: HashMap<usize, ToolCallAccumulator> = HashMap::new();
            let mut accumulated_content = String::new();
            let mut accumulated_reasoning = String::new();
            let mut finish_reason = "stop".to_string();
            let mut usage = Value::Null;

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));

                        // 处理 SSE 行
                        while let Some(pos) = buffer.find('\n') {
                            let line = buffer[..pos].trim().to_string();
                            buffer = buffer[pos + 1..].to_string();

                            if let Some(data) = line.strip_prefix("data: ") {
                                if data == "[DONE]" {
                                    // 构建最终响应
                                    let final_tool_calls: Vec<ToolCallRequest> = tool_calls
                                        .into_values()
                                        .map(|acc| acc.to_tool_call_request())
                                        .collect();

                                    let response = finalize_stream_response(
                                        mode,
                                        tools_available,
                                        accumulated_content,
                                        accumulated_reasoning,
                                        final_tool_calls,
                                        finish_reason,
                                        usage,
                                    );
                                    let _ = tx.send(StreamChunk::Done { response }).await;
                                    return;
                                }

                                // 解析 JSON
                                if let Ok(chunk) = serde_json::from_str::<StreamResponse>(data) {
                                    if let Some(choice) = chunk.choices.first() {
                                        // 处理文本增量
                                        if let Some(content) = &choice.delta.content {
                                            accumulated_content.push_str(content);
                                            let _ = tx
                                                .send(StreamChunk::TextDelta {
                                                    delta: content.clone(),
                                                })
                                                .await;
                                        }

                                        // 处理推理内容
                                        if let Some(reasoning) = &choice.delta.reasoning_content {
                                            accumulated_reasoning.push_str(reasoning);
                                            let _ = tx
                                                .send(StreamChunk::ReasoningDelta {
                                                    delta: reasoning.clone(),
                                                })
                                                .await;
                                        }

                                        // 处理工具调用
                                        if let Some(tool_call_deltas) = &choice.delta.tool_calls {
                                            for tc in tool_call_deltas {
                                                let idx = tc.index;

                                                let acc = tool_calls.entry(idx).or_default();
                                                if let Some(id) = &tc.id {
                                                    acc.id = id.clone();
                                                    let _ = tx
                                                        .send(StreamChunk::ToolCallStart {
                                                            index: idx,
                                                            id: id.clone(),
                                                            name: tc
                                                                .function
                                                                .name
                                                                .clone()
                                                                .unwrap_or_default(),
                                                        })
                                                        .await;
                                                }
                                                if let Some(name) = &tc.function.name {
                                                    acc.name = name.clone();
                                                }
                                                if let Some(args_delta) = &tc.function.arguments {
                                                    acc.arguments.push_str(args_delta);
                                                    let _ = tx
                                                        .send(StreamChunk::ToolCallDelta {
                                                            index: idx,
                                                            id: acc.id.clone(),
                                                            delta: args_delta.clone(),
                                                        })
                                                        .await;
                                                }
                                            }
                                        }

                                        // 更新 finish_reason
                                        if let Some(fr) = &choice.finish_reason {
                                            finish_reason = fr.clone();
                                        }
                                    }

                                    if let Some(u) = &chunk.usage {
                                        usage = u.clone();
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(StreamChunk::Error {
                                message: e.to_string(),
                            })
                            .await;
                        return;
                    }
                }
            }

            // 如果流结束但没有收到 [DONE]，也发送完成事件
            let final_tool_calls: Vec<ToolCallRequest> = tool_calls
                .into_values()
                .map(|acc| acc.to_tool_call_request())
                .collect();

            let response = finalize_stream_response(
                mode,
                tools_available,
                accumulated_content,
                accumulated_reasoning,
                final_tool_calls,
                finish_reason,
                usage,
            );
            let _ = tx.send(StreamChunk::Done { response }).await;
        });

        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::types::ChatMessage;

    #[test]
    fn test_parse_xml_tool_call() {
        let content = r#"I'll search for that.
<tool_call>
{"name": "web_search", "arguments": {"query": "rust async"}}
</tool_call>
Done."#;
        let (remaining, calls) = OpenAIProvider::parse_text_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "web_search");
        assert_eq!(calls[0].arguments["query"], "rust async");
        assert!(remaining.contains("I'll search"));
        assert!(remaining.contains("Done."));
    }

    #[test]
    fn test_parse_bracket_tool_call_json() {
        let content = r#"
[TOOL_CALL]
{"name": "memory_query", "arguments": {"top_k": 20}}
[/TOOL_CALL]
"#;
        let (remaining, calls) = OpenAIProvider::parse_text_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "memory_query");
        assert_eq!(calls[0].arguments["top_k"], 20);
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_parse_bracket_tool_call_arrow_syntax() {
        // This is the exact format xminimaxm25 produces
        let content = "\n\n\n[TOOL_CALL]\n{tool => \"memory_query\", args => {\n  --top_k 20\n}}\n[/TOOL_CALL]";
        let (remaining, calls) = OpenAIProvider::parse_text_tool_calls(content);
        assert_eq!(calls.len(), 1, "Should parse 1 tool call, got: {:?}", calls);
        assert_eq!(calls[0].name, "memory_query");
        assert_eq!(calls[0].arguments["top_k"], 20);
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_parse_arrow_syntax_string_args() {
        let content = "[TOOL_CALL]\n{tool => \"web_search\", args => {\n  --query \"rust programming\"\n}}\n[/TOOL_CALL]";
        let (remaining, calls) = OpenAIProvider::parse_text_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "web_search");
        assert_eq!(calls[0].arguments["query"], "rust programming");
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_parse_function_parameter_tool_call() {
        let content = r#"我来帮你查看当前目录下的文件：
<tool_call>
<function=exec>
<parameter=command>
ls -la
</parameter>
</function>
</tool_call>"#;
        let (remaining, calls) = OpenAIProvider::parse_text_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "exec");
        assert_eq!(calls[0].arguments["command"], "ls -la");
        assert!(remaining.contains("我来帮你查看当前目录下的文件"));
    }

    #[test]
    fn test_parse_dash_args() {
        let args = OpenAIProvider::parse_dash_args("--top_k 20 --query hello --verbose");
        assert_eq!(args["top_k"], 20);
        assert_eq!(args["query"], "hello");
        assert_eq!(args["verbose"], true);
    }

    #[test]
    fn test_parse_native_tool_arguments_recovers_single_required_string_parameter() {
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "web_fetch",
                "description": "Fetch a URL",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" }
                    },
                    "required": ["url"]
                }
            }
        })];

        let parsed = OpenAIProvider::parse_native_tool_arguments(
            "web_fetch",
            "https://wttr.in/Shenzhen?format=j1",
            &tools,
        )
        .expect("parse bare url");

        assert_eq!(parsed["url"], "https://wttr.in/Shenzhen?format=j1");
    }

    #[test]
    fn test_parse_native_tool_arguments_recovers_loose_map_syntax() {
        let parsed = OpenAIProvider::parse_native_tool_arguments(
            "web_fetch",
            r#"url => "https://example.com", maxChars => 5000"#,
            &[],
        )
        .expect("parse loose map");

        assert_eq!(parsed["url"], "https://example.com");
        assert_eq!(parsed["maxChars"], 5000);
    }

    #[test]
    fn test_parse_native_tool_arguments_reports_unrecognized_payload() {
        let err = OpenAIProvider::parse_native_tool_arguments(
            "web_fetch",
            "just some words without structure",
            &[],
        )
        .expect_err("should reject unrecognized payload");

        assert!(err.contains("unrecognized native tool-call arguments"));
    }

    #[test]
    fn test_no_tool_calls_returns_empty() {
        let content = "This is just a normal response with no tool calls.";
        let (remaining, calls) = OpenAIProvider::parse_text_tool_calls(content);
        assert!(calls.is_empty());
        assert_eq!(remaining, content);
    }

    #[test]
    fn test_parse_nonstandard_block_with_tool_key() {
        let block = r#"{tool => "read_file", args => {"path": "/tmp/test.txt"}}"#;
        let tc = OpenAIProvider::parse_nonstandard_tool_block(block, 0).unwrap();
        assert_eq!(tc.name, "read_file");
        assert_eq!(tc.arguments["path"], "/tmp/test.txt");
    }

    #[test]
    fn test_parse_minimax_invoke_no_params() {
        // Exact format from xminimaxm25 logs: <invoke name="list_skills">\n</invoke>\n</minimax:tool_call>
        let content = "\n\n\n\n<invoke name=\"list_skills\">\n</invoke>\n</minimax:tool_call>";
        let (remaining, calls) = OpenAIProvider::parse_text_tool_calls(content);
        assert_eq!(calls.len(), 1, "Should parse 1 tool call, got: {:?}", calls);
        assert_eq!(calls[0].name, "list_skills");
        assert!(calls[0].arguments.as_object().unwrap().is_empty());
        assert!(
            remaining.is_empty(),
            "remaining should be empty, got: {:?}",
            remaining
        );
    }

    #[test]
    fn test_parse_minimax_invoke_with_params() {
        let content = "<invoke name=\"exec\">\n<parameter name=\"command\">ls -la</parameter>\n</invoke>\n</minimax:tool_call>";
        let (remaining, calls) = OpenAIProvider::parse_text_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "exec");
        assert_eq!(calls[0].arguments["command"], "ls -la");
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_parse_minimax_invoke_multiple_params() {
        let content = "<invoke name=\"http_request\">\n<parameter name=\"action\">get</parameter>\n<parameter name=\"url\">https://example.com</parameter>\n</invoke>\n</minimax:tool_call>";
        let (remaining, calls) = OpenAIProvider::parse_text_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "http_request");
        assert_eq!(calls[0].arguments["action"], "get");
        assert_eq!(calls[0].arguments["url"], "https://example.com");
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_parse_minimax_invoke_without_minimax_wrapper() {
        // Sometimes the model omits the </minimax:tool_call> wrapper
        let content = "Let me check.\n<invoke name=\"list_skills\">\n</invoke>\nDone.";
        let (remaining, calls) = OpenAIProvider::parse_text_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "list_skills");
        assert!(remaining.contains("Let me check."));
    }

    #[test]
    fn test_finalize_stream_response_parses_text_mode_tool_call_blocks() {
        let response = finalize_stream_response(
            ToolCallMode::Text,
            true,
            "我来帮您查看上一级目录。\n<tool_call>\n<function=list_dir>\n<parameter=path>/Users/apple/.blockcell</parameter>\n</function>\n</tool_call>".to_string(),
            String::new(),
            vec![],
            "stop".to_string(),
            Value::Null,
        );

        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "list_dir");
        assert_eq!(
            response.tool_calls[0].arguments["path"],
            "/Users/apple/.blockcell"
        );
        assert_eq!(
            response.content.as_deref(),
            Some("我来帮您查看上一级目录。")
        );
        assert_eq!(response.finish_reason, "tool_calls");
    }

    #[test]
    fn test_sanitize_messages_for_native_tools_drops_incomplete_tool_round() {
        let messages = vec![
            ChatMessage::user("先写一个故事"),
            ChatMessage {
                role: "assistant".to_string(),
                content: Value::String(String::new()),
                reasoning_content: None,
                tool_calls: Some(vec![ToolCallRequest {
                    id: "call-1".to_string(),
                    name: "write_file".to_string(),
                    arguments: serde_json::json!({"path":"story.md"}),
                    thought_signature: None,
                }]),
                tool_call_id: None,
                name: None,
            },
            ChatMessage::assistant("已经写好了"),
        ];

        let sanitized = OpenAIProvider::sanitize_messages_for_native_tools(&messages);

        assert_eq!(sanitized.len(), 2);
        assert_eq!(sanitized[0].role, "user");
        assert_eq!(sanitized[1].role, "assistant");
        assert!(sanitized.iter().all(|msg| msg.tool_calls.is_none()));
        assert!(sanitized.iter().all(|msg| msg.tool_call_id.is_none()));
    }

    #[test]
    fn test_sanitize_messages_for_native_tools_keeps_complete_tool_round() {
        let messages = vec![
            ChatMessage::user("列出当前目录"),
            ChatMessage {
                role: "assistant".to_string(),
                content: Value::String(String::new()),
                reasoning_content: None,
                tool_calls: Some(vec![ToolCallRequest {
                    id: "call-1".to_string(),
                    name: "list_dir".to_string(),
                    arguments: serde_json::json!({"path":"."}),
                    thought_signature: None,
                }]),
                tool_call_id: None,
                name: None,
            },
            ChatMessage::tool_result("call-1", r#"{"entries":[]}"#),
            ChatMessage::assistant("目录为空"),
        ];

        let sanitized = OpenAIProvider::sanitize_messages_for_native_tools(&messages);

        assert_eq!(sanitized.len(), 4);
        assert_eq!(sanitized[1].role, "assistant");
        assert!(sanitized[1].tool_calls.is_some());
        assert_eq!(sanitized[2].role, "tool");
        assert_eq!(sanitized[2].tool_call_id.as_deref(), Some("call-1"));
    }
}
