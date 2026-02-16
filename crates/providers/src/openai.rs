use async_trait::async_trait;
use blockcell_core::types::{ChatMessage, LLMResponse, ToolCallRequest};
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, error, info, warn};

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
        Self {
            client: Client::new(),
            api_key: api_key.to_string(),
            api_base: api_base
                .unwrap_or("https://api.openai.com/v1")
                .trim_end_matches('/')
                .to_string(),
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

    /// Parse `<tool_call>...</tool_call>` blocks from the response content.
    /// Returns (remaining_text, parsed_tool_calls).
    fn parse_text_tool_calls(content: &str) -> (String, Vec<ToolCallRequest>) {
        let mut tool_calls = Vec::new();
        let mut remaining = String::new();
        let mut rest = content;
        let mut call_index = 0u64;

        loop {
            if let Some(start) = rest.find("<tool_call>") {
                // Text before the tag
                remaining.push_str(&rest[..start]);
                let after_tag = &rest[start + "<tool_call>".len()..];
                if let Some(end) = after_tag.find("</tool_call>") {
                    let json_str = after_tag[..end].trim();
                    // Try to parse the JSON
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
                        });
                        call_index += 1;
                    } else {
                        warn!(json = %json_str, "Failed to parse tool_call JSON");
                        // Keep the raw text if parsing fails
                        remaining.push_str(&rest[start..start + "<tool_call>".len() + end + "</tool_call>".len()]);
                    }
                    rest = &after_tag[end + "</tool_call>".len()..];
                } else {
                    // No closing tag, keep everything
                    remaining.push_str(&rest[start..]);
                    break;
                }
            } else {
                remaining.push_str(rest);
                break;
            }
        }

        // Clean up remaining text
        let remaining = remaining.trim().to_string();
        (remaining, tool_calls)
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
            } else {
                return Ok(LLMResponse {
                    content: if content.is_empty() { None } else { Some(content) },
                    reasoning_content,
                    tool_calls: native_tool_calls,
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
