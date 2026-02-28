use async_trait::async_trait;
use blockcell_core::types::{ChatMessage, LLMResponse, ToolCallRequest};
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
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
    /// When true, tool schemas are injected into the system prompt as text
    /// instead of using the API `tools` parameter. This works around relays
    /// that strip `tool_calls` from the response.
    text_tool_mode: AtomicBool,
}

impl OpenAIProvider {
    pub fn new(
        api_key: &str,
        api_base: Option<&str>,
        model: &str,
        max_tokens: u32,
        temperature: f32,
    ) -> Self {
        Self::new_with_proxy(api_key, api_base, model, max_tokens, temperature, None, None, &[])
    }

    pub fn new_with_proxy(
        api_key: &str,
        api_base: Option<&str>,
        model: &str,
        max_tokens: u32,
        temperature: f32,
        provider_proxy: Option<&str>,
        global_proxy: Option<&str>,
        no_proxy: &[String],
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
            text_tool_mode: AtomicBool::new(false),
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
                let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                let desc = func.get("description").and_then(|v| v.as_str()).unwrap_or("");
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
    /// - `<tool_call>{"name":"...","arguments":{...}}</tool_call>`
    /// - `[TOOL_CALL]{tool => "...", args => {...}}[/TOOL_CALL]`
    /// Returns (remaining_text, parsed_tool_calls).
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
                    let json_str = after_tag[..end].trim();
                    if let Ok(val) = serde_json::from_str::<Value>(json_str) {
                        let name = val.get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let arguments = val.get("arguments")
                            .cloned()
                            .unwrap_or(Value::Object(serde_json::Map::new()));
                        tool_calls.push(ToolCallRequest {
                            id: format!("text_call_{}", call_index),
                            name,
                            arguments,
                            thought_signature: None,
                        });
                        call_index += 1;
                    } else {
                        warn!(json = %json_str, "Failed to parse tool_call JSON");
                        remaining.push_str(&rest[start..start + "<tool_call>".len() + end + "</tool_call>".len()]);
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
                        let block_end = lower_after.find("</minimax:tool_call>")
                            .or_else(|| lower_after.find("</invoke>"));
                        let (params_str, consumed) = if let Some(end) = block_end {
                            let tag_len = if lower_after[end..].starts_with("</minimax:tool_call>") {
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
                                                args.insert(param_name, serde_json::Value::String(value));
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
                            let tag_content_start = &after_invoke[name_attr_start + "name=\"".len() + name_end + 1..];
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
                                                        let json_val = serde_json::from_str::<Value>(&value)
                                                            .unwrap_or(Value::String(value));
                                                        args.insert(param_name, json_val);
                                                        scan = &value_str[close + "</parameter>".len()..];
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
                                rest4 = if trimmed_after.to_lowercase().starts_with("</minimax:tool_call>") {
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
            let name = val.get("name")
                .or_else(|| val.get("tool"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let arguments = val.get("arguments")
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
            format!("{} =\\u003e", key),  // JSON-escaped >
        ];
        for pat in &patterns {
            if let Some(pos) = text.find(pat.as_str()) {
                let after = text[pos + pat.len()..].trim();
                // Quoted value
                if after.starts_with('"') {
                    if let Some(end_quote) = after[1..].find('"') {
                        return Some(after[1..1 + end_quote].to_string());
                    }
                }
                // Unquoted value — take until comma or whitespace
                let val: String = after.chars()
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
                let val: String = after.chars()
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

        let flush = |key: &mut Option<String>, parts: &mut Vec<String>, map: &mut serde_json::Map<String, Value>| {
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
    async fn send_request(&self, messages: &[ChatMessage], tools: &[Value], use_native_tools: bool) -> Result<(ChatResponse, String)> {
        let url = format!("{}/chat/completions", self.api_base);

        let (api_messages, api_tools) = if use_native_tools && !tools.is_empty() {
            (messages.to_vec(), tools.to_vec())
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

        let mode = if use_native_tools && !tools.is_empty() { "native" } else if !tools.is_empty() { "text" } else { "no-tools" };
        info!(url = %url, model = %self.model, tools_count = tools.len(), messages_count = messages.len(), mode = %mode, "Calling LLM");

        let request_body = serde_json::to_string(&request)
            .map_err(|e| Error::Provider(format!("Failed to serialize request: {}", e)))?;
        debug!(body_len = request_body.len(), "Request body prepared");

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
            return Err(Error::Provider(format!("API error {}: {}", status, raw_body)));
        }

        {
            let end = truncate_at_char_boundary(&raw_body, 500);
            info!(body_len = raw_body.len(), preview = %&raw_body[..end], "LLM raw response");
        }

        let chat_response: ChatResponse = serde_json::from_str(&raw_body)
            .map_err(|e| {
                let end = truncate_at_char_boundary(&raw_body, 500);
                Error::Provider(format!("Failed to parse response: {}. Body: {}", e, &raw_body[..end]))
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

#[async_trait]
impl Provider for OpenAIProvider {
    async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<LLMResponse> {
        let use_text_mode = self.text_tool_mode.load(Ordering::Relaxed);

        if !use_text_mode && !tools.is_empty() {
            // Try native tool calling first
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
                    let arguments: Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(Value::Object(serde_json::Map::new()));
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

            // Detect if the relay stripped tool_calls:
            // - content is empty
            // - no tool_calls returned
            // - usage shows completion_tokens > 0 (model did generate something)
            if content.is_empty() && native_tool_calls.is_empty() {
                warn!("Native tool call returned empty content and no tool_calls. Switching to text-based tool mode.");
                self.text_tool_mode.store(true, Ordering::Relaxed);
                // Fall through to text mode below
            } else if !native_tool_calls.is_empty() || tools.is_empty() {
                // Native tool calls present, or no tools were requested — return as-is
                return Ok(LLMResponse {
                    content: if content.is_empty() { None } else { Some(content) },
                    reasoning_content,
                    tool_calls: native_tool_calls,
                    finish_reason: choice.finish_reason.unwrap_or_else(|| "stop".to_string()),
                    usage: chat_response.usage.unwrap_or(Value::Null),
                });
            } else {
                // No native tool_calls but content is non-empty and tools were requested.
                // Some models (e.g. xminimaxm25) return tool calls as text in content
                // instead of using the native function calling mechanism.
                // Try to parse text-based tool calls from the content.
                let (remaining_text, parsed_calls) = Self::parse_text_tool_calls(&content);
                if !parsed_calls.is_empty() {
                    info!(count = parsed_calls.len(), "Parsed text-based tool calls from native mode response");
                    return Ok(LLMResponse {
                        content: if remaining_text.is_empty() { None } else { Some(remaining_text) },
                        reasoning_content,
                        tool_calls: parsed_calls,
                        finish_reason: "tool_calls".to_string(),
                        usage: chat_response.usage.unwrap_or(Value::Null),
                    });
                }
                // No text-based tool calls found either — return as normal text
                return Ok(LLMResponse {
                    content: Some(content),
                    reasoning_content,
                    tool_calls: vec![],
                    finish_reason: choice.finish_reason.unwrap_or_else(|| "stop".to_string()),
                    usage: chat_response.usage.unwrap_or(Value::Null),
                });
            }
        }

        // Text-based tool mode (or no tools)
        let (chat_response, _raw) = self.send_request(messages, tools, false).await?;

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
            content: if remaining_text.is_empty() { None } else { Some(remaining_text) },
            reasoning_content: choice.message.reasoning_content,
            tool_calls,
            finish_reason: choice.finish_reason.unwrap_or_else(|| "stop".to_string()),
            usage: chat_response.usage.unwrap_or(Value::Null),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_parse_dash_args() {
        let args = OpenAIProvider::parse_dash_args("--top_k 20 --query hello --verbose");
        assert_eq!(args["top_k"], 20);
        assert_eq!(args["query"], "hello");
        assert_eq!(args["verbose"], true);
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
        assert!(remaining.is_empty(), "remaining should be empty, got: {:?}", remaining);
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
        let content = "<invoke name=\"finance_api\">\n<parameter name=\"action\">stock_quote</parameter>\n<parameter name=\"symbol\">601318</parameter>\n</invoke>\n</minimax:tool_call>";
        let (remaining, calls) = OpenAIProvider::parse_text_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "finance_api");
        assert_eq!(calls[0].arguments["action"], "stock_quote");
        assert_eq!(calls[0].arguments["symbol"], 601318);
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
}
