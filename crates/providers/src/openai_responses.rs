use async_trait::async_trait;
use blockcell_core::types::{ChatMessage, LLMResponse, ToolCallRequest};
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;
use tracing::{debug, error, info, warn};

use crate::client::build_http_client;
use crate::Provider;

pub struct OpenAIResponsesProvider {
    client: Client,
    api_key: String,
    api_base: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
}

impl OpenAIResponsesProvider {
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
        )
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
        }
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

        if url.starts_with("data:") {
            Some(json!({
                "type": "input_image",
                "image_url": url,
                "detail": detail,
            }))
        } else {
            Some(json!({
                "type": "input_image",
                "image_url": url,
                "detail": detail,
            }))
        }
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
            tools: Self::build_tools(tools),
            temperature: self.temperature,
            max_output_tokens: self.max_tokens,
        };

        let request_body = serde_json::to_string(&request)
            .map_err(|e| Error::Provider(format!("Failed to serialize responses request: {}", e)))?;

        info!(
            url = %url,
            model = %self.model,
            messages_count = messages.len(),
            tools_count = tools.len(),
            "Calling OpenAI Responses API"
        );
        debug!(body_len = request_body.len(), "Responses request body prepared");

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .body(request_body)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("Responses request failed: {}", e)))?;

        let status = response.status();
        let raw_body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            error!(status = %status, body = %raw_body, "OpenAI Responses API error");
            return Err(Error::Provider(format!(
                "Responses API error {}: {}",
                status, raw_body
            )));
        }

        debug!(body_len = raw_body.len(), "Responses raw response received");
        serde_json::from_str::<Value>(&raw_body).map_err(|e| {
            Error::Provider(format!(
                "Failed to parse Responses API response: {}. Body: {}",
                e, raw_body
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
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or_default();
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
            usage: response.get("usage").cloned().unwrap_or(Value::Null),
        })
    }
}

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    temperature: f32,
    max_output_tokens: u32,
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
    fn test_build_input_for_tool_result() {
        let messages = vec![ChatMessage::tool_result("call_1", "done")];
        let built = OpenAIResponsesProvider::build_input(&messages);
        assert_eq!(built.len(), 1);
        assert_eq!(built[0]["type"], "function_call_output");
        assert_eq!(built[0]["call_id"], "call_1");
        assert_eq!(built[0]["output"], "done");
    }

    #[test]
    fn test_build_input_preserves_multimodal_parts() {
        let messages = vec![ChatMessage {
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
        let parts = OpenAIResponsesProvider::content_to_input_parts("user", &Value::String("hello".to_string()));
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
}
