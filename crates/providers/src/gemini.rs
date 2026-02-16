use async_trait::async_trait;
use blockcell_core::types::{ChatMessage, LLMResponse, ToolCallRequest};
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, error, info};

use crate::Provider;

const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

pub struct GeminiProvider {
    client: Client,
    api_key: String,
    api_base: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
}

impl GeminiProvider {
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
                .unwrap_or(GEMINI_API_BASE)
                .trim_end_matches('/')
                .to_string(),
            model: model.to_string(),
            max_tokens,
            temperature,
        }
    }

    /// Normalize model name: strip "gemini/" prefix if present.
    /// Config may store "gemini/gemini-2.0-flash" but the API expects "gemini-2.0-flash".
    fn normalize_model(model: &str) -> &str {
        model.strip_prefix("gemini/").unwrap_or(model)
    }

    /// Convert ChatMessage list to Gemini format.
    /// Gemini uses `role: "user"/"model"`, with system instruction as a separate field.
    fn convert_messages(messages: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
        let mut system_text: Option<String> = None;
        let mut gemini_contents: Vec<Value> = Vec::new();

        for msg in messages {
            match msg.role.as_str() {
                "system" => {
                    let text = msg.content.as_str().unwrap_or("").to_string();
                    system_text = Some(match system_text {
                        Some(existing) => format!("{}\n\n{}", existing, text),
                        None => text,
                    });
                }
                "user" => {
                    // Handle multimodal content (array of content blocks)
                    if let Some(arr) = msg.content.as_array() {
                        let mut parts: Vec<Value> = Vec::new();
                        for block in arr {
                            let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            match block_type {
                                "text" => {
                                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                                        parts.push(serde_json::json!({"text": t}));
                                    }
                                }
                                "image_url" => {
                                    // Convert data:mime;base64,xxx to Gemini inlineData format
                                    if let Some(url) = block.get("image_url")
                                        .and_then(|v| v.get("url"))
                                        .and_then(|v| v.as_str())
                                    {
                                        if let Some(rest) = url.strip_prefix("data:") {
                                            if let Some(semi) = rest.find(';') {
                                                let mime = &rest[..semi];
                                                if let Some(data) = rest[semi..].strip_prefix(";base64,") {
                                                    parts.push(serde_json::json!({
                                                        "inlineData": {
                                                            "mimeType": mime,
                                                            "data": data
                                                        }
                                                    }));
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        if parts.is_empty() {
                            parts.push(serde_json::json!({"text": ""}));
                        }
                        gemini_contents.push(serde_json::json!({
                            "role": "user",
                            "parts": parts,
                        }));
                    } else {
                        let text = msg.content.as_str().unwrap_or("").to_string();
                        gemini_contents.push(serde_json::json!({
                            "role": "user",
                            "parts": [{"text": text}],
                        }));
                    }
                }
                "assistant" => {
                    let mut parts: Vec<Value> = Vec::new();

                    let text = msg.content.as_str().unwrap_or("").to_string();
                    if !text.is_empty() {
                        parts.push(serde_json::json!({"text": text}));
                    }

                    // Add function call parts for tool calls
                    if let Some(tool_calls) = &msg.tool_calls {
                        for tc in tool_calls {
                            parts.push(serde_json::json!({
                                "functionCall": {
                                    "name": tc.name,
                                    "args": tc.arguments,
                                }
                            }));
                        }
                    }

                    if parts.is_empty() {
                        parts.push(serde_json::json!({"text": ""}));
                    }

                    gemini_contents.push(serde_json::json!({
                        "role": "model",
                        "parts": parts,
                    }));
                }
                "tool" => {
                    // Gemini expects function responses as user messages with functionResponse parts
                    let tool_call_id = msg.tool_call_id.as_deref().unwrap_or("");
                    let result_text = msg.content.as_str().unwrap_or("").to_string();

                    // Try to parse result as JSON, fallback to text wrapper
                    let response_value = serde_json::from_str::<Value>(&result_text)
                        .unwrap_or_else(|_| serde_json::json!({"result": result_text}));

                    let func_response = serde_json::json!({
                        "functionResponse": {
                            "name": tool_call_id,
                            "response": response_value,
                        }
                    });

                    // Try to merge with previous user message if it's also function responses
                    if let Some(last) = gemini_contents.last_mut() {
                        if last.get("role").and_then(|v| v.as_str()) == Some("user") {
                            if let Some(parts) = last.get_mut("parts") {
                                if let Some(arr) = parts.as_array_mut() {
                                    if arr.first().and_then(|v| v.get("functionResponse")).is_some()
                                    {
                                        arr.push(func_response);
                                        continue;
                                    }
                                }
                            }
                        }
                    }

                    gemini_contents.push(serde_json::json!({
                        "role": "user",
                        "parts": [func_response],
                    }));
                }
                _ => {
                    let text = msg.content.as_str().unwrap_or("").to_string();
                    gemini_contents.push(serde_json::json!({
                        "role": "user",
                        "parts": [{"text": text}],
                    }));
                }
            }
        }

        (system_text, gemini_contents)
    }

    /// Convert OpenAI-style tool schemas to Gemini function declarations.
    fn convert_tools(tools: &[Value]) -> Vec<Value> {
        let declarations: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                let func = tool.get("function")?;
                let name = func.get("name")?.as_str()?;
                let description = func
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let parameters = func.get("parameters").cloned().unwrap_or(serde_json::json!({
                    "type": "object",
                    "properties": {}
                }));

                Some(serde_json::json!({
                    "name": name,
                    "description": description,
                    "parameters": parameters,
                }))
            })
            .collect();

        if declarations.is_empty() {
            vec![]
        } else {
            vec![serde_json::json!({
                "functionDeclarations": declarations,
            })]
        }
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<LLMResponse> {
        let model = Self::normalize_model(&self.model);
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.api_base, model, self.api_key
        );

        let (system_instruction, contents) = Self::convert_messages(messages);
        let gemini_tools = Self::convert_tools(tools);

        let mut request = serde_json::json!({
            "contents": contents,
            "generationConfig": {
                "temperature": self.temperature,
                "maxOutputTokens": self.max_tokens,
            }
        });

        if let Some(sys) = &system_instruction {
            request["systemInstruction"] = serde_json::json!({
                "parts": [{"text": sys}]
            });
        }

        if !gemini_tools.is_empty() {
            request["tools"] = Value::Array(gemini_tools);
        }

        info!(
            model = %model,
            tools_count = tools.len(),
            messages_count = messages.len(),
            "Calling Gemini API"
        );

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("Gemini request failed: {}", e)))?;

        let status = response.status();
        let raw_body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            error!(status = %status, body = %raw_body, "Gemini API error");
            return Err(Error::Provider(format!(
                "Gemini API error {}: {}",
                status, raw_body
            )));
        }

        debug!(body_len = raw_body.len(), "Gemini raw response");

        let resp: GeminiResponse = serde_json::from_str(&raw_body).map_err(|e| {
            Error::Provider(format!(
                "Failed to parse Gemini response: {}. Body: {}",
                e,
                &raw_body[..raw_body.len().min(500)]
            ))
        })?;

        // Extract content from first candidate
        let candidate = resp
            .candidates
            .and_then(|c| c.into_iter().next())
            .ok_or_else(|| Error::Provider("No candidates in Gemini response".to_string()))?;

        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCallRequest> = Vec::new();

        if let Some(content) = candidate.content {
            for (i, part) in content.parts.iter().enumerate() {
                if let Some(text) = &part.text {
                    if !text.is_empty() {
                        text_parts.push(text.clone());
                    }
                }
                if let Some(fc) = &part.function_call {
                    tool_calls.push(ToolCallRequest {
                        id: format!("gemini_call_{}", i),
                        name: fc.name.clone(),
                        arguments: fc.args.clone().unwrap_or(Value::Object(serde_json::Map::new())),
                    });
                }
            }
        }

        let content_text = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join("\n"))
        };

        let finish_reason = match candidate.finish_reason.as_deref() {
            Some("STOP") => "stop".to_string(),
            Some("MAX_TOKENS") => "length".to_string(),
            Some("SAFETY") => "content_filter".to_string(),
            Some(other) => other.to_lowercase(),
            None => {
                if !tool_calls.is_empty() {
                    "tool_calls".to_string()
                } else {
                    "stop".to_string()
                }
            }
        };

        let usage = if let Some(meta) = &resp.usage_metadata {
            serde_json::json!({
                "prompt_tokens": meta.prompt_token_count,
                "completion_tokens": meta.candidates_token_count,
            })
        } else {
            Value::Null
        };

        info!(
            content_len = content_text.as_ref().map(|c| c.len()).unwrap_or(0),
            tool_calls_count = tool_calls.len(),
            finish_reason = %finish_reason,
            "Gemini response parsed"
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
#[serde(rename_all = "camelCase")]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    content: Option<GeminiContent>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiPart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    function_call: Option<GeminiFunctionCall>,
}

#[derive(Debug, Deserialize)]
struct GeminiFunctionCall {
    name: String,
    args: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsageMetadata {
    prompt_token_count: Option<u64>,
    candidates_token_count: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_model() {
        assert_eq!(
            GeminiProvider::normalize_model("gemini/gemini-2.0-flash"),
            "gemini-2.0-flash"
        );
        assert_eq!(
            GeminiProvider::normalize_model("gemini-1.5-pro"),
            "gemini-1.5-pro"
        );
    }

    #[test]
    fn test_convert_messages() {
        let messages = vec![
            ChatMessage::system("You are helpful"),
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi there"),
        ];

        let (system, contents) = GeminiProvider::convert_messages(&messages);
        assert_eq!(system, Some("You are helpful".to_string()));
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
    }

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
                    }
                }
            }
        })];

        let converted = GeminiProvider::convert_tools(&tools);
        assert_eq!(converted.len(), 1);
        let declarations = converted[0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0]["name"], "read_file");
    }

    #[test]
    fn test_parse_response() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "Hello!"}
                    ],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5
            }
        }"#;

        let resp: GeminiResponse = serde_json::from_str(json).unwrap();
        let candidate = resp.candidates.unwrap();
        assert_eq!(candidate.len(), 1);
        assert_eq!(candidate[0].finish_reason.as_deref(), Some("STOP"));
        let parts = &candidate[0].content.as_ref().unwrap().parts;
        assert_eq!(parts[0].text.as_deref(), Some("Hello!"));
    }

    #[test]
    fn test_parse_response_with_function_call() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "parts": [
                        {"functionCall": {"name": "read_file", "args": {"path": "/tmp/test"}}}
                    ],
                    "role": "model"
                },
                "finishReason": "STOP"
            }]
        }"#;

        let resp: GeminiResponse = serde_json::from_str(json).unwrap();
        let candidates = resp.candidates.unwrap();
        let parts = &candidates[0].content.as_ref().unwrap().parts;
        assert!(parts[0].function_call.is_some());
        assert_eq!(parts[0].function_call.as_ref().unwrap().name, "read_file");
    }

    #[test]
    fn test_convert_tool_results() {
        let mut assistant = ChatMessage::assistant("");
        assistant.tool_calls = Some(vec![ToolCallRequest {
            id: "read_file".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "/tmp/test"}),
        }]);

        let tool_result = ChatMessage::tool_result("read_file", "file contents");

        let messages = vec![
            ChatMessage::user("read /tmp/test"),
            assistant,
            tool_result,
        ];

        let (_system, contents) = GeminiProvider::convert_messages(&messages);
        assert_eq!(contents.len(), 3);
        // Last should be user with functionResponse
        assert_eq!(contents[2]["role"], "user");
        let parts = contents[2]["parts"].as_array().unwrap();
        assert!(parts[0].get("functionResponse").is_some());
    }
}
