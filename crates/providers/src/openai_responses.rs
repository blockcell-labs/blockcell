use async_trait::async_trait;
use blockcell_core::config::ToolCallMode;
use blockcell_core::types::{ChatMessage, LLMResponse, ToolCallRequest};
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde::{Serialize, Serializer};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use crate::client::build_http_client;
use crate::security::{
    consume_error_body, http_status_error_with_retry_after, read_response_limited, redact_url,
    request_error, MAX_PROVIDER_RESPONSE_BYTES,
};
use crate::Provider;

pub struct OpenAIResponsesProvider {
    client: Client,
    api_key: String,
    api_base: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
    tool_call_mode: ToolCallMode,
}

impl OpenAIResponsesProvider {
    fn normalize_usage(mut usage: Value) -> Value {
        let cached_tokens = usage
            .get("input_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64);
        if let (Some(cached_tokens), Some(object)) = (cached_tokens, usage.as_object_mut()) {
            object.insert(
                "cache_read_input_tokens".to_string(),
                Value::from(cached_tokens),
            );
        }
        usage
    }

    fn request_text_part(role: &str, text: &str) -> Value {
        let part_type = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        json!({
            "type": part_type,
            "text": text,
        })
    }

    pub fn new(
        api_key: &str,
        api_base: Option<&str>,
        model: &str,
        max_tokens: u32,
        temperature: f32,
    ) -> Result<Self> {
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
    ) -> Result<Self> {
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
        )?;
        Ok(Self {
            client,
            api_key: api_key.to_string(),
            api_base: resolved_base,
            model: model.to_string(),
            max_tokens,
            temperature,
            tool_call_mode,
        })
    }

    fn content_to_text(content: &Value) -> String {
        match content {
            Value::String(s) => s.clone(),
            Value::Array(parts) => {
                let mut out = Vec::new();
                for part in parts {
                    if let Some(part_type) = part.get("type").and_then(|v| v.as_str()) {
                        match part_type {
                            "input_text" | "output_text" | "text" => {
                                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                    out.push(text.to_string());
                                }
                            }
                            "image_url" => {
                                if let Some(url) = part
                                    .get("image_url")
                                    .and_then(|v| v.get("url"))
                                    .and_then(|v| v.as_str())
                                {
                                    out.push(format!("[image:{}]", url));
                                }
                            }
                            _ => {
                                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                    out.push(text.to_string());
                                }
                            }
                        }
                    } else if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        out.push(text.to_string());
                    }
                }
                out.join("\n")
            }
            _ => content.to_string(),
        }
    }

    fn image_part_to_input(part: &Value) -> Option<Value> {
        let image_url = part.get("image_url")?;
        let url = image_url.get("url").and_then(|v| v.as_str())?;
        let detail = image_url
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("auto");

        Some(json!({
            "type": "input_image",
            "image_url": url,
            "detail": detail,
        }))
    }

    fn content_to_input_parts(role: &str, content: &Value) -> Vec<Value> {
        match content {
            Value::String(s) => {
                if s.is_empty() {
                    Vec::new()
                } else {
                    vec![Self::request_text_part(role, s)]
                }
            }
            Value::Array(parts) => {
                let mut out = Vec::new();
                for part in parts {
                    let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("text");
                    match part_type {
                        "text" | "input_text" | "output_text" => {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    out.push(Self::request_text_part(role, text));
                                }
                            }
                        }
                        "image_url" => {
                            if let Some(image_part) = Self::image_part_to_input(part) {
                                out.push(image_part);
                            }
                        }
                        _ => {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    out.push(Self::request_text_part(role, text));
                                }
                            }
                        }
                    }
                }
                out
            }
            _ => {
                let text = content.to_string();
                if text.is_empty() || text == "null" {
                    Vec::new()
                } else {
                    vec![Self::request_text_part(role, &text)]
                }
            }
        }
    }

    fn build_input(messages: &[ChatMessage]) -> Vec<Value> {
        let mut input = Vec::new();
        let mut known_call_ids = HashSet::new();

        for message in messages {
            match message.role.as_str() {
                "system" | "user" | "assistant" => {
                    let content = Self::content_to_input_parts(&message.role, &message.content);
                    let tool_calls = message.tool_calls.as_ref().cloned().unwrap_or_default();
                    if content.is_empty() && tool_calls.is_empty() {
                        continue;
                    }

                    if !content.is_empty() {
                        input.push(json!({
                            "role": message.role,
                            "content": content,
                        }));
                    }

                    if message.role == "assistant" {
                        for tool_call in tool_calls {
                            known_call_ids.insert(tool_call.id.clone());
                            input.push(json!({
                                "type": "function_call",
                                "call_id": tool_call.id,
                                "name": tool_call.name,
                                "arguments": tool_call.arguments.to_string(),
                            }));
                        }
                    }
                }
                "tool" => {
                    let call_id = message.tool_call_id.clone().unwrap_or_default();
                    if call_id.is_empty() || !known_call_ids.contains(&call_id) {
                        debug!(
                            call_id = %call_id,
                            "Dropping orphaned function_call_output without matching function_call"
                        );
                        continue;
                    }
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": Self::content_to_text(&message.content),
                    }));
                }
                _ => {
                    let text = Self::content_to_text(&message.content);
                    if !text.is_empty() {
                        input.push(json!({
                            "role": "user",
                            "content": [Self::request_text_part("user", &text)],
                        }));
                    }
                }
            }
        }

        input
    }

    fn build_tools(tools: &[Value]) -> Vec<Value> {
        tools
            .iter()
            .filter_map(|tool| {
                let func = tool.get("function")?;
                let name = func.get("name")?.as_str()?;
                let description = func
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let parameters = func.get("parameters").cloned().unwrap_or(Value::Null);
                Some(json!({
                    "type": "function",
                    "name": name,
                    "description": description,
                    "parameters": parameters,
                }))
            })
            .collect()
    }

    async fn send_request(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<Value> {
        let url = format!("{}/responses", self.api_base);
        let request = ResponsesRequest {
            model: self.model.clone(),
            input: Self::build_input(messages),
            tools: if matches!(self.tool_call_mode, ToolCallMode::Text | ToolCallMode::None) {
                vec![]
            } else {
                Self::build_tools(tools)
            },
            max_output_tokens: self.max_tokens,
            temperature: self.temperature,
            prompt_cache_key: crate::prompt_utils::prompt_cache_key_with_tools(
                messages,
                tools,
                &self.model,
            ),
        };

        let request_body = serde_json::to_string(&request).map_err(|e| {
            Error::Provider(format!("Failed to serialize responses request: {}", e))
        })?;

        info!(
            url = %redact_url(&url),
            model = %self.model,
            messages_count = messages.len(),
            tools_count = tools.len(),
            "Calling OpenAI Responses API"
        );
        debug!(
            body_len = request_body.len(),
            "Responses request body prepared"
        );

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .body(request_body)
            .send()
            .await
            .map_err(|e| request_error("Responses request failed", e))?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body_len = consume_error_body(response).await;
            error!(status = %status, body_len, "OpenAI Responses API error");
            return Err(http_status_error_with_retry_after(
                "OpenAI Responses",
                status,
                body_len,
                retry_after.as_deref(),
            ));
        }
        let raw_bytes = read_response_limited(response, MAX_PROVIDER_RESPONSE_BYTES).await?;
        let raw_body = String::from_utf8_lossy(&raw_bytes).into_owned();

        debug!(body_len = raw_body.len(), "Responses raw response received");
        serde_json::from_str::<Value>(&raw_body).map_err(|e| {
            Error::Provider(format!(
                "Failed to parse Responses API response: {} (body_len={})",
                e,
                raw_body.len()
            ))
        })
    }

    fn parse_response(response: Value) -> Result<LLMResponse> {
        let output = response
            .get("output")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut content_parts = Vec::new();
        let mut reasoning_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for item in output {
            let item_type = item
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            match item_type {
                "message" => {
                    if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                        for part in content {
                            let part_type = part
                                .get("type")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default();
                            match part_type {
                                "output_text" | "text" => {
                                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                        content_parts.push(text.to_string());
                                    }
                                }
                                "reasoning" => {
                                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                        reasoning_parts.push(text.to_string());
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "function_call" => {
                    let call_id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("call_unknown")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let arguments = item
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
                    let thought_signature = item
                        .get("thought_signature")
                        .or_else(|| item.get("thoughtSignature"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    tool_calls.push(ToolCallRequest {
                        id: call_id,
                        name,
                        arguments,
                        thought_signature,
                    });
                }
                "reasoning" => {
                    if let Some(summary) = item.get("summary").and_then(|v| v.as_array()) {
                        for part in summary {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                reasoning_parts.push(text.to_string());
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        let finish_reason = response
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| match s {
                "completed" if !tool_calls.is_empty() => "tool_calls".to_string(),
                "completed" => "stop".to_string(),
                other => other.to_string(),
            })
            .unwrap_or_else(|| {
                if tool_calls.is_empty() {
                    "stop".to_string()
                } else {
                    "tool_calls".to_string()
                }
            });

        if content_parts.is_empty() && tool_calls.is_empty() {
            warn!("Responses API returned neither text nor tool calls");
        }

        Ok(LLMResponse {
            content: if content_parts.is_empty() {
                None
            } else {
                Some(content_parts.join("\n"))
            },
            reasoning_content: if reasoning_parts.is_empty() {
                None
            } else {
                Some(reasoning_parts.join("\n"))
            },
            tool_calls,
            finish_reason,
            usage: Self::normalize_usage(response.get("usage").cloned().unwrap_or(Value::Null)),
        })
    }
}

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    max_output_tokens: u32,
    #[serde(serialize_with = "serialize_temperature")]
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
}

fn serialize_temperature<S>(
    temperature: &f32,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let rounded = (*temperature as f64 * 1000.0).round() / 1000.0;
    serializer.serialize_f64(rounded)
}

#[async_trait]
impl Provider for OpenAIResponsesProvider {
    async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<LLMResponse> {
        let response = self.send_request(messages, tools).await?;
        Self::parse_response(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_tools() {
        let tools = vec![json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a file",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    }
                }
            }
        })];
        let built = OpenAIResponsesProvider::build_tools(&tools);
        assert_eq!(built.len(), 1);
        assert_eq!(built[0]["type"], "function");
        assert_eq!(built[0]["name"], "read_file");
    }

    #[test]
    fn test_parse_response_message_and_tool_call() {
        let response = json!({
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "content": [
                        { "type": "output_text", "text": "hello" }
                    ]
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"/tmp/a.txt\"}"
                }
            ],
            "usage": {"input_tokens": 1, "output_tokens": 2}
        });
        let parsed = OpenAIResponsesProvider::parse_response(response).unwrap();
        assert_eq!(parsed.content.as_deref(), Some("hello"));
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "read_file");
        assert_eq!(parsed.tool_calls[0].arguments["path"], "/tmp/a.txt");
        assert_eq!(parsed.finish_reason, "tool_calls");
    }

    #[test]
    fn test_build_input_for_tool_result_requires_matching_function_call() {
        let messages = vec![ChatMessage::tool_result("call_1", "done")];
        let built = OpenAIResponsesProvider::build_input(&messages);
        assert!(built.is_empty());
    }

    #[test]
    fn test_build_input_for_tool_result_after_assistant_function_call() {
        let mut assistant = ChatMessage::assistant("");
        assistant.tool_calls = Some(vec![ToolCallRequest {
            id: "call_1".to_string(),
            name: "write_file".to_string(),
            arguments: json!({"path": "./story.txt", "content": "hello"}),
            thought_signature: None,
        }]);

        let messages = vec![assistant, ChatMessage::tool_result("call_1", "done")];
        let built = OpenAIResponsesProvider::build_input(&messages);

        assert_eq!(built.len(), 2);
        assert_eq!(built[0]["type"], "function_call");
        assert_eq!(built[0]["call_id"], "call_1");
        assert_eq!(built[1]["type"], "function_call_output");
        assert_eq!(built[1]["call_id"], "call_1");
        assert_eq!(built[1]["output"], "done");
    }

    #[test]
    fn test_build_input_preserves_multimodal_parts() {
        let messages = vec![ChatMessage {
            id: None,
            role: "user".to_string(),
            content: json!([
                { "type": "text", "text": "看看这张图" },
                {
                    "type": "image_url",
                    "image_url": {
                        "url": "data:image/png;base64,abcd",
                        "detail": "high"
                    }
                }
            ]),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];
        let built = OpenAIResponsesProvider::build_input(&messages);
        assert_eq!(built.len(), 1);
        let content = built[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "看看这张图");
        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(content[1]["image_url"], "data:image/png;base64,abcd");
        assert_eq!(content[1]["detail"], "high");
    }

    #[test]
    fn test_content_to_input_parts_plain_text_fallback() {
        let parts = OpenAIResponsesProvider::content_to_input_parts(
            "user",
            &Value::String("hello".to_string()),
        );
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "input_text");
        assert_eq!(parts[0]["text"], "hello");
    }

    #[test]
    fn test_assistant_text_uses_output_text() {
        let parts = OpenAIResponsesProvider::content_to_input_parts(
            "assistant",
            &Value::String("hello back".to_string()),
        );
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "output_text");
        assert_eq!(parts[0]["text"], "hello back");
    }

    #[test]
    fn test_assistant_tool_calls_are_top_level_items() {
        let messages = vec![ChatMessage {
            id: None,
            role: "assistant".to_string(),
            content: Value::String("".to_string()),
            reasoning_content: None,
            tool_calls: Some(vec![ToolCallRequest {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: json!({"path": "/tmp/a.txt"}),
                thought_signature: Some("sig1".to_string()),
            }]),
            tool_call_id: None,
            name: None,
        }];

        let built = OpenAIResponsesProvider::build_input(&messages);
        assert_eq!(built.len(), 1);
        assert_eq!(built[0]["type"], "function_call");
        assert_eq!(built[0]["call_id"], "call_1");
        assert_eq!(built[0]["name"], "read_file");
        assert_eq!(built[0]["arguments"], "{\"path\":\"/tmp/a.txt\"}");
        assert!(built[0].get("thought_signature").is_none());
    }

    #[test]
    fn test_responses_request_includes_temperature() {
        let request = ResponsesRequest {
            model: "gpt-5.5".to_string(),
            input: Vec::new(),
            tools: Vec::new(),
            max_output_tokens: 128,
            temperature: 0.7,
            prompt_cache_key: None,
        };

        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["temperature"], 0.7);
        assert_eq!(value["max_output_tokens"], 128);
    }
}
