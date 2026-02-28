use async_trait::async_trait;
use blockcell_core::types::{ChatMessage, LLMResponse, ToolCallRequest};
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use crate::client::build_http_client;
use crate::Provider;

const DEFAULT_OLLAMA_BASE: &str = "http://localhost:11434";

pub struct OllamaProvider {
    client: Client,
    api_base: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
}

impl OllamaProvider {
    pub fn new(
        api_base: Option<&str>,
        model: &str,
        max_tokens: u32,
        temperature: f32,
    ) -> Self {
        Self::new_with_proxy(api_base, model, max_tokens, temperature, None, None, &[])
    }

    pub fn new_with_proxy(
        api_base: Option<&str>,
        model: &str,
        max_tokens: u32,
        temperature: f32,
        provider_proxy: Option<&str>,
        global_proxy: Option<&str>,
        no_proxy: &[String],
    ) -> Self {
        let resolved_base = api_base
            .unwrap_or(DEFAULT_OLLAMA_BASE)
            .trim_end_matches('/')
            .to_string();
        // Ollama 本地推理使用更长的超时时间
        let client = build_http_client(
            provider_proxy,
            global_proxy,
            no_proxy,
            &resolved_base,
            Duration::from_secs(300),
        );
        Self {
            client,
            api_base: resolved_base,
            model: model.to_string(),
            max_tokens,
            temperature,
        }
    }

    /// Strip "ollama/" prefix from model names.
    /// Config may store "ollama/llama3" but the API expects "llama3".
    fn normalize_model(model: &str) -> &str {
        model.strip_prefix("ollama/").unwrap_or(model)
    }

    /// Convert ChatMessage list to Ollama chat format.
    /// Handles multimodal content arrays by extracting base64 images into the `images` field.
    fn convert_messages(messages: &[ChatMessage]) -> Vec<OllamaMessage> {
        messages
            .iter()
            .map(|msg| {
                // Handle multimodal content (array of content blocks)
                if let Some(arr) = msg.content.as_array() {
                    let mut text_parts = Vec::new();
                    let mut images = Vec::new();
                    for block in arr {
                        let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        match block_type {
                            "text" => {
                                if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                                    text_parts.push(t.to_string());
                                }
                            }
                            "image_url" => {
                                // Extract base64 data from data:mime;base64,xxx
                                if let Some(url) = block.get("image_url")
                                    .and_then(|v| v.get("url"))
                                    .and_then(|v| v.as_str())
                                {
                                    if let Some(rest) = url.strip_prefix("data:") {
                                        if let Some(semi) = rest.find(';') {
                                            if let Some(data) = rest[semi..].strip_prefix(";base64,") {
                                                images.push(data.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    OllamaMessage {
                        role: msg.role.clone(),
                        content: text_parts.join("\n"),
                        tool_calls: None,
                        images: if images.is_empty() { None } else { Some(images) },
                    }
                } else {
                    let content = msg.content.as_str().unwrap_or("").to_string();
                    OllamaMessage {
                        role: msg.role.clone(),
                        content,
                        tool_calls: None,
                        images: None,
                    }
                }
            })
            .collect()
    }

    /// Convert OpenAI-style tool schemas to Ollama tool format.
    /// Ollama uses the same format as OpenAI for tools.
    fn convert_tools(tools: &[Value]) -> Vec<Value> {
        tools.to_vec()
    }

    /// Build a text description of tools to inject into the system prompt
    /// for models that don't support native tool calling.
    fn build_tools_prompt(tools: &[Value]) -> String {
        let mut s = String::new();
        s.push_str("\n\n## Available Tools\n");
        s.push_str("You MUST use tools to accomplish tasks. To call a tool, output a `<tool_call>` block with JSON inside.\n");
        s.push_str("You may call multiple tools in one response. Each call must be a separate `<tool_call>` block.\n\n");
        s.push_str("Format:\n```\n<tool_call>\n{\"name\": \"tool_name\", \"arguments\": {\"param1\": \"value1\"}}\n</tool_call>\n```\n\n");
        s.push_str("Tools:\n");

        for tool in tools {
            if let Some(func) = tool.get("function") {
                let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                let desc = func.get("description").and_then(|v| v.as_str()).unwrap_or("");
                let params = func.get("parameters").cloned().unwrap_or(Value::Null);
                s.push_str(&format!("### {}\n{}\n", name, desc));
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

    /// Parse `<tool_call>...</tool_call>` blocks from text content.
    fn parse_text_tool_calls(content: &str) -> (String, Vec<ToolCallRequest>) {
        let mut tool_calls = Vec::new();
        let mut remaining = String::new();
        let mut rest = content;
        let mut call_index = 0u64;

        loop {
            if let Some(start) = rest.find("<tool_call>") {
                remaining.push_str(&rest[..start]);
                let after_tag = &rest[start + "<tool_call>".len()..];
                if let Some(end) = after_tag.find("</tool_call>") {
                    let json_str = after_tag[..end].trim();
                    if let Ok(val) = serde_json::from_str::<Value>(json_str) {
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
                            id: format!("ollama_call_{}", call_index),
                            name,
                            arguments,
                            thought_signature: None,
                        });
                        call_index += 1;
                    } else {
                        warn!(json = %json_str, "Failed to parse tool_call JSON from Ollama");
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

        (remaining.trim().to_string(), tool_calls)
    }

    /// Try the Ollama /api/chat endpoint with native tool support.
    async fn chat_native(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<LLMResponse> {
        let url = format!("{}/api/chat", self.api_base);
        let model = Self::normalize_model(&self.model);
        let ollama_messages = Self::convert_messages(messages);
        let ollama_tools = Self::convert_tools(tools);

        let mut request = serde_json::json!({
            "model": model,
            "messages": ollama_messages,
            "stream": false,
            "options": {
                "temperature": self.temperature,
                "num_predict": self.max_tokens,
            }
        });

        if !ollama_tools.is_empty() {
            request["tools"] = Value::Array(ollama_tools);
        }

        info!(
            url = %url,
            model = %model,
            tools_count = tools.len(),
            messages_count = messages.len(),
            "Calling Ollama API"
        );

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("Ollama request failed: {}", e)))?;

        let status = response.status();
        let raw_body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            error!(status = %status, body = %raw_body, "Ollama API error");
            return Err(Error::Provider(format!(
                "Ollama API error {}: {}",
                status, raw_body
            )));
        }

        debug!(body_len = raw_body.len(), "Ollama raw response");

        let resp: OllamaChatResponse = serde_json::from_str(&raw_body).map_err(|e| {
            let preview_end = raw_body
                .char_indices()
                .nth(500)
                .map(|(i, _)| i)
                .unwrap_or(raw_body.len());
            Error::Provider(format!(
                "Failed to parse Ollama response: {}. Body: {}",
                e,
                &raw_body[..preview_end]
            ))
        })?;

        let content = resp.message.content.clone();

        // Extract native tool calls if present
        let mut tool_calls: Vec<ToolCallRequest> = Vec::new();
        if let Some(native_calls) = &resp.message.tool_calls {
            for (i, tc) in native_calls.iter().enumerate() {
                if let Some(func) = &tc.function {
                    tool_calls.push(ToolCallRequest {
                        id: format!("ollama_call_{}", i),
                        name: func.name.clone(),
                        arguments: func.arguments.clone(),
                        thought_signature: None,
                    });
                }
            }
        }

        // If no native tool calls but tools were requested, try parsing text-based tool calls
        let (final_content, final_tool_calls) = if tool_calls.is_empty() && !tools.is_empty() {
            let (remaining, parsed) = Self::parse_text_tool_calls(&content);
            if !parsed.is_empty() {
                (remaining, parsed)
            } else {
                (content, tool_calls)
            }
        } else {
            (content, tool_calls)
        };

        let usage = serde_json::json!({
            "prompt_tokens": resp.prompt_eval_count,
            "completion_tokens": resp.eval_count,
        });

        let finish_reason = if !final_tool_calls.is_empty() {
            "tool_calls".to_string()
        } else if resp.done.unwrap_or(true) {
            "stop".to_string()
        } else {
            "length".to_string()
        };

        info!(
            content_len = final_content.len(),
            tool_calls_count = final_tool_calls.len(),
            finish_reason = %finish_reason,
            "Ollama response parsed"
        );

        Ok(LLMResponse {
            content: if final_content.is_empty() {
                None
            } else {
                Some(final_content)
            },
            reasoning_content: None,
            tool_calls: final_tool_calls,
            finish_reason,
            usage,
        })
    }

    /// Fallback: inject tools into system prompt as text for models without native tool support.
    async fn chat_text_tools(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<LLMResponse> {
        let tools_prompt = Self::build_tools_prompt(tools);

        // Inject tools into system message
        let mut modified_messages = messages.to_vec();
        if let Some(sys_msg) = modified_messages.first_mut() {
            if sys_msg.role == "system" {
                if let Some(text) = sys_msg.content.as_str() {
                    sys_msg.content = Value::String(format!("{}{}", text, tools_prompt));
                }
            }
        } else {
            modified_messages.insert(0, ChatMessage::system(&tools_prompt));
        }

        // Call without tools parameter
        let url = format!("{}/api/chat", self.api_base);
        let model = Self::normalize_model(&self.model);
        let ollama_messages = Self::convert_messages(&modified_messages);

        let request = serde_json::json!({
            "model": model,
            "messages": ollama_messages,
            "stream": false,
            "options": {
                "temperature": self.temperature,
                "num_predict": self.max_tokens,
            }
        });

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("Ollama request failed: {}", e)))?;

        let status = response.status();
        let raw_body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(Error::Provider(format!(
                "Ollama API error {}: {}",
                status, raw_body
            )));
        }

        let resp: OllamaChatResponse = serde_json::from_str(&raw_body).map_err(|e| {
            Error::Provider(format!("Failed to parse Ollama response: {}", e))
        })?;

        let (remaining, tool_calls) = Self::parse_text_tool_calls(&resp.message.content);

        let usage = serde_json::json!({
            "prompt_tokens": resp.prompt_eval_count,
            "completion_tokens": resp.eval_count,
        });

        Ok(LLMResponse {
            content: if remaining.is_empty() {
                None
            } else {
                Some(remaining)
            },
            reasoning_content: None,
            tool_calls,
            finish_reason: "stop".to_string(),
            usage,
        })
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<LLMResponse> {
        // First try native tool calling
        match self.chat_native(messages, tools).await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                // If native fails and we have tools, try text-based fallback
                if !tools.is_empty() {
                    warn!(error = %e, "Ollama native tool call failed, trying text-based fallback");
                    self.chat_text_tools(messages, tools).await
                } else {
                    Err(e)
                }
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct OllamaMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: OllamaResponseMessage,
    #[serde(default)]
    done: Option<bool>,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseMessage {
    #[allow(dead_code)]
    role: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OllamaToolCall {
    function: Option<OllamaFunctionCall>,
}

#[derive(Debug, Deserialize)]
struct OllamaFunctionCall {
    name: String,
    arguments: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_model() {
        assert_eq!(OllamaProvider::normalize_model("ollama/llama3"), "llama3");
        assert_eq!(
            OllamaProvider::normalize_model("qwen2.5:7b"),
            "qwen2.5:7b"
        );
    }

    #[test]
    fn test_convert_messages() {
        let messages = vec![
            ChatMessage::system("You are helpful"),
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi there"),
        ];

        let converted = OllamaProvider::convert_messages(&messages);
        assert_eq!(converted.len(), 3);
        assert_eq!(converted[0].role, "system");
        assert_eq!(converted[0].content, "You are helpful");
        assert_eq!(converted[1].role, "user");
        assert_eq!(converted[2].role, "assistant");
    }

    #[test]
    fn test_parse_text_tool_calls() {
        let content = "I'll read that file.\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"/tmp/test\"}}\n</tool_call>\nDone.";
        let (remaining, calls) = OllamaProvider::parse_text_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert!(remaining.contains("I'll read that file."));
        assert!(remaining.contains("Done."));
    }

    #[test]
    fn test_parse_response() {
        let json = r#"{
            "model": "llama3",
            "message": {
                "role": "assistant",
                "content": "Hello! How can I help?"
            },
            "done": true,
            "prompt_eval_count": 50,
            "eval_count": 20
        }"#;

        let resp: OllamaChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.message.content, "Hello! How can I help?");
        assert_eq!(resp.done, Some(true));
        assert_eq!(resp.prompt_eval_count, Some(50));
    }

    #[test]
    fn test_parse_response_with_tool_calls() {
        let json = r#"{
            "model": "llama3",
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    {
                        "function": {
                            "name": "read_file",
                            "arguments": {"path": "/tmp/test"}
                        }
                    }
                ]
            },
            "done": true
        }"#;

        let resp: OllamaChatResponse = serde_json::from_str(json).unwrap();
        let tool_calls = resp.message.tool_calls.unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.as_ref().unwrap().name, "read_file");
    }
}
