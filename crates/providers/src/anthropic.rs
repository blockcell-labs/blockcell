use async_trait::async_trait;
use blockcell_core::types::{ChatMessage, LLMResponse, ToolCallRequest};
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, error, info};

use crate::Provider;

const ANTHROPIC_API_BASE: &str = "https://api.anthropic.com/v1";
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    api_base: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
}

impl AnthropicProvider {
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
                .unwrap_or(ANTHROPIC_API_BASE)
                .trim_end_matches('/')
                .to_string(),
            model: model.to_string(),
            max_tokens,
            temperature,
        }
    }

    /// Convert OpenAI-style tool schemas to Anthropic tool format.
    /// OpenAI: { type: "function", function: { name, description, parameters } }
    /// Anthropic: { name, description, input_schema }
    fn convert_tools(tools: &[Value]) -> Vec<Value> {
        tools
            .iter()
            .filter_map(|tool| {
                let func = tool.get("function")?;
                let name = func.get("name")?.as_str()?;
                let description = func.get("description").and_then(|v| v.as_str()).unwrap_or("");
                let parameters = func.get("parameters").cloned().unwrap_or(serde_json::json!({
                    "type": "object",
                    "properties": {}
                }));

                Some(serde_json::json!({
                    "name": name,
                    "description": description,
                    "input_schema": parameters,
                }))
            })
            .collect()
    }

    /// Convert ChatMessage list to Anthropic format.
    /// Anthropic uses a separate `system` parameter and only `user`/`assistant` messages.
    /// Tool results use `role: "user"` with `type: "tool_result"` content blocks.
    fn convert_messages(messages: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
        let mut system_text: Option<String> = None;
        let mut anthropic_messages: Vec<Value> = Vec::new();

        for msg in messages {
            match msg.role.as_str() {
                "system" => {
                    // Anthropic takes system as a top-level parameter
                    let text = msg.content.as_str().unwrap_or("").to_string();
                    system_text = Some(match system_text {
                        Some(existing) => format!("{}\n\n{}", existing, text),
                        None => text,
                    });
                }
                "user" => {
                    // Handle multimodal content (array of content blocks)
                    if let Some(arr) = msg.content.as_array() {
                        let mut blocks: Vec<Value> = Vec::new();
                        for block in arr {
                            let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            match block_type {
                                "text" => {
                                    blocks.push(block.clone());
                                }
                                "image_url" => {
                                    // Convert OpenAI image_url format to Anthropic image format
                                    if let Some(url) = block.get("image_url")
                                        .and_then(|v| v.get("url"))
                                        .and_then(|v| v.as_str())
                                    {
                                        if let Some(rest) = url.strip_prefix("data:") {
                                            if let Some(semi) = rest.find(';') {
                                                let mime = &rest[..semi];
                                                if let Some(data) = rest[semi..].strip_prefix(";base64,") {
                                                    blocks.push(serde_json::json!({
                                                        "type": "image",
                                                        "source": {
                                                            "type": "base64",
                                                            "media_type": mime,
                                                            "data": data
                                                        }
                                                    }));
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {
                                    blocks.push(block.clone());
                                }
                            }
                        }
                        anthropic_messages.push(serde_json::json!({
                            "role": "user",
                            "content": blocks,
                        }));
                    } else {
                        let text = msg.content.as_str().unwrap_or("").to_string();
                        anthropic_messages.push(serde_json::json!({
                            "role": "user",
                            "content": text,
                        }));
                    }
                }
                "assistant" => {
                    let mut content_blocks: Vec<Value> = Vec::new();

                    // Add text content if present
                    let text = msg.content.as_str().unwrap_or("").to_string();
                    if !text.is_empty() {
                        content_blocks.push(serde_json::json!({
                            "type": "text",
                            "text": text,
                        }));
                    }

                    // Add tool_use blocks if present
                    if let Some(tool_calls) = &msg.tool_calls {
                        for tc in tool_calls {
                            content_blocks.push(serde_json::json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.name,
                                "input": tc.arguments,
                            }));
                        }
                    }

                    if content_blocks.is_empty() {
                        content_blocks.push(serde_json::json!({
                            "type": "text",
                            "text": "",
                        }));
                    }

                    anthropic_messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": content_blocks,
                    }));
                }
                "tool" => {
                    // Anthropic expects tool results as user messages with tool_result content blocks
                    let tool_call_id = msg.tool_call_id.as_deref().unwrap_or("");
                    let result_text = msg.content.as_str().unwrap_or("").to_string();

                    let tool_result_block = serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": result_text,
                    });

                    // Try to merge with the previous user message if it's also tool results
                    if let Some(last) = anthropic_messages.last_mut() {
                        if last.get("role").and_then(|v| v.as_str()) == Some("user") {
                            if let Some(content) = last.get_mut("content") {
                                if let Some(arr) = content.as_array_mut() {
                                    // Check if this is a tool_result array
                                    if arr.first()
                                        .and_then(|v| v.get("type"))
                                        .and_then(|v| v.as_str())
                                        == Some("tool_result")
                                    {
                                        arr.push(tool_result_block);
                                        continue;
                                    }
                                }
                            }
                        }
                    }

                    // Create new user message with tool_result content
                    anthropic_messages.push(serde_json::json!({
                        "role": "user",
                        "content": [tool_result_block],
                    }));
                }
                _ => {
                    // Unknown role, treat as user
                    let text = msg.content.as_str().unwrap_or("").to_string();
                    anthropic_messages.push(serde_json::json!({
                        "role": "user",
                        "content": text,
                    }));
                }
            }
        }

        // Anthropic requires alternating user/assistant messages.
        // Merge consecutive same-role messages if needed.
        let merged = Self::merge_consecutive_roles(anthropic_messages);

        (system_text, merged)
    }

    /// Merge consecutive messages with the same role (Anthropic requirement).
    fn merge_consecutive_roles(messages: Vec<Value>) -> Vec<Value> {
        let mut result: Vec<Value> = Vec::new();

        for msg in messages {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let last_role = result
                .last()
                .and_then(|v| v.get("role"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if role == last_role && !result.is_empty() {
                // Merge content into the last message
                if let Some(last) = result.last_mut() {
                    let last_content = last.get("content").cloned().unwrap_or(Value::Null);
                    let new_content = msg.get("content").cloned().unwrap_or(Value::Null);

                    let merged_content = match (last_content, new_content) {
                        (Value::Array(mut a), Value::Array(b)) => {
                            a.extend(b);
                            Value::Array(a)
                        }
                        (Value::Array(mut a), Value::String(s)) => {
                            a.push(serde_json::json!({"type": "text", "text": s}));
                            Value::Array(a)
                        }
                        (Value::String(s1), Value::String(s2)) => {
                            Value::String(format!("{}\n\n{}", s1, s2))
                        }
                        (Value::String(s), Value::Array(mut a)) => {
                            let mut new_arr = vec![serde_json::json!({"type": "text", "text": s})];
                            new_arr.append(&mut a);
                            Value::Array(new_arr)
                        }
                        (existing, _new) => existing, // Fallback: keep existing
                    };

                    last["content"] = merged_content;
                }
            } else {
                result.push(msg);
            }
        }

        result
    }

    /// Strip the "anthropic/" or "claude-" prefix from model names for the API.
    /// Config may store "anthropic/claude-sonnet-4-20250514" but the API expects "claude-sonnet-4-20250514".
    fn normalize_model(model: &str) -> &str {
        model.strip_prefix("anthropic/").unwrap_or(model)
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<LLMResponse> {
        let url = format!("{}/messages", self.api_base);
        let model = Self::normalize_model(&self.model);

        let (system, anthropic_messages) = Self::convert_messages(messages);
        let anthropic_tools = Self::convert_tools(tools);

        let mut request = serde_json::json!({
            "model": model,
            "max_tokens": self.max_tokens,
            "temperature": self.temperature,
            "messages": anthropic_messages,
        });

        if let Some(sys) = &system {
            request["system"] = Value::String(sys.clone());
        }

        if !anthropic_tools.is_empty() {
            request["tools"] = Value::Array(anthropic_tools);
        }

        info!(
            url = %url,
            model = %model,
            tools_count = tools.len(),
            messages_count = messages.len(),
            "Calling Anthropic API"
        );

        let response = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("Anthropic request failed: {}", e)))?;

        let status = response.status();
        let raw_body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            error!(status = %status, body = %raw_body, "Anthropic API error");
            return Err(Error::Provider(format!(
                "Anthropic API error {}: {}",
                status, raw_body
            )));
        }

        debug!(body_len = raw_body.len(), "Anthropic raw response");

        let resp: AnthropicResponse = serde_json::from_str(&raw_body).map_err(|e| {
            Error::Provider(format!(
                "Failed to parse Anthropic response: {}. Body: {}",
                e,
                &raw_body[..raw_body.len().min(500)]
            ))
        })?;

        // Extract text content and tool_use blocks
        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCallRequest> = Vec::new();

        for block in &resp.content {
            match block.block_type.as_str() {
                "text" => {
                    if let Some(text) = &block.text {
                        if !text.is_empty() {
                            text_parts.push(text.clone());
                        }
                    }
                }
                "tool_use" => {
                    if let (Some(id), Some(name)) = (&block.id, &block.name) {
                        let arguments = block.input.clone().unwrap_or(Value::Object(serde_json::Map::new()));
                        tool_calls.push(ToolCallRequest {
                            id: id.clone(),
                            name: name.clone(),
                            arguments,
                        });
                    }
                }
                _ => {}
            }
        }

        let content_text = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join("\n"))
        };

        let finish_reason = match resp.stop_reason.as_deref() {
            Some("end_turn") => "stop".to_string(),
            Some("tool_use") => "tool_calls".to_string(),
            Some("max_tokens") => "length".to_string(),
            Some(other) => other.to_string(),
            None => "stop".to_string(),
        };

        let usage = serde_json::json!({
            "prompt_tokens": resp.usage.as_ref().and_then(|u| u.input_tokens),
            "completion_tokens": resp.usage.as_ref().and_then(|u| u.output_tokens),
        });

        info!(
            content_len = content_text.as_ref().map(|c| c.len()).unwrap_or(0),
            tool_calls_count = tool_calls.len(),
            finish_reason = %finish_reason,
            "Anthropic response parsed"
        );

        Ok(LLMResponse {
            content: content_text,
            reasoning_content: None,
            tool_calls,
            finish_reason,
            usage,
        })
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[allow(dead_code)]
    id: String,
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_tools() {
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a file",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    },
                    "required": ["path"]
                }
            }
        })];

        let converted = AnthropicProvider::convert_tools(&tools);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0]["name"], "read_file");
        assert_eq!(converted[0]["description"], "Read a file");
        assert!(converted[0]["input_schema"].is_object());
    }

    #[test]
    fn test_convert_messages_system_extraction() {
        let messages = vec![
            ChatMessage::system("You are helpful"),
            ChatMessage::user("Hello"),
        ];

        let (system, msgs) = AnthropicProvider::convert_messages(&messages);
        assert_eq!(system, Some("You are helpful".to_string()));
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
    }

    #[test]
    fn test_convert_messages_tool_results() {
        let mut assistant = ChatMessage::assistant("");
        assistant.tool_calls = Some(vec![ToolCallRequest {
            id: "tc_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "/tmp/test"}),
        }]);

        let tool_result = ChatMessage::tool_result("tc_1", "file contents here");

        let messages = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("read /tmp/test"),
            assistant,
            tool_result,
        ];

        let (system, msgs) = AnthropicProvider::convert_messages(&messages);
        assert_eq!(system, Some("sys".to_string()));
        assert_eq!(msgs.len(), 3); // user, assistant, user(tool_result)
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[2]["role"], "user");

        // Check tool_use in assistant
        let assistant_content = msgs[1]["content"].as_array().unwrap();
        assert_eq!(assistant_content[0]["type"], "tool_use");
        assert_eq!(assistant_content[0]["name"], "read_file");

        // Check tool_result in user
        let user_content = msgs[2]["content"].as_array().unwrap();
        assert_eq!(user_content[0]["type"], "tool_result");
        assert_eq!(user_content[0]["tool_use_id"], "tc_1");
    }

    #[test]
    fn test_normalize_model() {
        assert_eq!(
            AnthropicProvider::normalize_model("anthropic/claude-sonnet-4-20250514"),
            "claude-sonnet-4-20250514"
        );
        assert_eq!(
            AnthropicProvider::normalize_model("claude-3-opus-20240229"),
            "claude-3-opus-20240229"
        );
    }

    #[test]
    fn test_parse_response() {
        let json = r#"{
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "I'll read that file for you."},
                {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "/tmp/test"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 100, "output_tokens": 50}
        }"#;

        let resp: AnthropicResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content.len(), 2);
        assert_eq!(resp.content[0].block_type, "text");
        assert_eq!(resp.content[1].block_type, "tool_use");
        assert_eq!(resp.content[1].name.as_deref(), Some("read_file"));
        assert_eq!(resp.stop_reason.as_deref(), Some("tool_use"));
    }

    #[test]
    fn test_merge_consecutive_roles() {
        let messages = vec![
            serde_json::json!({"role": "user", "content": "hello"}),
            serde_json::json!({"role": "user", "content": "world"}),
            serde_json::json!({"role": "assistant", "content": "hi"}),
        ];

        let merged = AnthropicProvider::merge_consecutive_roles(messages);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0]["role"], "user");
        assert_eq!(merged[0]["content"], "hello\n\nworld");
        assert_eq!(merged[1]["role"], "assistant");
    }
}
