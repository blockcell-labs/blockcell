use async_trait::async_trait;
use blockcell_core::types::{
    ChatMessage, LLMResponse, StreamChunk, ToolCallAccumulator, ToolCallRequest,
};
use blockcell_core::{Error, Result};
use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::client::build_http_client;
use crate::security::{
    consume_error_body, http_status_error_with_retry_after, read_response_limited, redact_url,
    request_error, StreamGuard, MAX_PROVIDER_RESPONSE_BYTES,
};
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
    ) -> Result<Self> {
        let resolved_base = api_base
            .unwrap_or(ANTHROPIC_API_BASE)
            .trim_end_matches('/')
            .to_string();
        // Anthropic SDK convention: base_url should end with /v1 so that
        // {base_url}/messages resolves to the correct endpoint.
        // When using custom api_base (e.g. MiniMax's "https://api.minimaxi.com/anthropic"),
        // the user follows the ANTHROPIC_BASE_URL convention which does NOT include /v1,
        // so we auto-append it. The default ANTHROPIC_API_BASE already includes /v1.
        let resolved_base = if !resolved_base.ends_with("/v1") {
            format!("{}/v1", resolved_base)
        } else {
            resolved_base
        };
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
        })
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
                let description = func
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let parameters = func
                    .get("parameters")
                    .cloned()
                    .unwrap_or(serde_json::json!({
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
                            let block_type =
                                block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            match block_type {
                                "text" => {
                                    blocks.push(block.clone());
                                }
                                "image_url" => {
                                    // Convert OpenAI image_url format to Anthropic image format
                                    if let Some(url) = block
                                        .get("image_url")
                                        .and_then(|v| v.get("url"))
                                        .and_then(|v| v.as_str())
                                    {
                                        if let Some(rest) = url.strip_prefix("data:") {
                                            if let Some(semi) = rest.find(';') {
                                                let mime = &rest[..semi];
                                                if let Some(data) =
                                                    rest[semi..].strip_prefix(";base64,")
                                                {
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
                                    if arr
                                        .first()
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

    fn mark_content_cacheable(content: &mut Value) {
        let cache_control = serde_json::json!({"type": "ephemeral"});
        match content {
            Value::String(text) => {
                *content = serde_json::json!([{
                    "type": "text",
                    "text": text,
                    "cache_control": cache_control,
                }]);
            }
            Value::Array(blocks) => {
                if let Some(last) = blocks.last_mut().and_then(Value::as_object_mut) {
                    last.insert("cache_control".to_string(), cache_control);
                }
            }
            _ => {}
        }
    }

    fn apply_prompt_cache_breakpoints(request: &mut Value) {
        if let Some(system) = request.get_mut("system") {
            Self::mark_content_cacheable(system);
        }
        if let Some(last_tool) = request
            .get_mut("tools")
            .and_then(Value::as_array_mut)
            .and_then(|tools| tools.last_mut())
            .and_then(Value::as_object_mut)
        {
            last_tool.insert(
                "cache_control".to_string(),
                serde_json::json!({"type": "ephemeral"}),
            );
        }
        if let Some(messages) = request.get_mut("messages").and_then(Value::as_array_mut) {
            if messages.len() >= 2 {
                let cache_index = messages.len() - 2;
                if let Some(content) = messages[cache_index].get_mut("content") {
                    Self::mark_content_cacheable(content);
                }
            }
            // 第 4 个 Anthropic 缓存断点：当消息数 >= 4 时同时标记 len - 4。
            // 父轮长度为 L 时缓存断点在 L - 2；Fork Review 追加 [assistant_final, user_review] 后
            // 长度为 L + 2，此时 (L + 2) - 4 == L - 2，恰好与父轮的历史缓存断点对齐。
            if messages.len() >= 4 {
                let fork_aligned_index = messages.len() - 4;
                if let Some(content) = messages[fork_aligned_index].get_mut("content") {
                    Self::mark_content_cacheable(content);
                }
            }
        }
    }

    fn normalize_usage(usage: Option<&AnthropicUsage>) -> Value {
        serde_json::json!({
            "prompt_tokens": usage.and_then(|value| value.input_tokens),
            "completion_tokens": usage.and_then(|value| value.output_tokens),
            "cache_creation_input_tokens": usage.and_then(|value| value.cache_creation_input_tokens),
            "cache_read_input_tokens": usage.and_then(|value| value.cache_read_input_tokens),
        })
    }

    /// Strip the "anthropic/" or "claude-" prefix from model names for the API.
    /// Config may store "anthropic/claude-opus-4-8" but the API expects "claude-opus-4-8".
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
        Self::apply_prompt_cache_breakpoints(&mut request);

        info!(
            url = %redact_url(&url),
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
            .map_err(|e| request_error("Anthropic request failed", e))?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body_len = consume_error_body(response).await;
            error!(status = %status, body_len, "Anthropic API error");
            return Err(http_status_error_with_retry_after(
                "Anthropic",
                status,
                body_len,
                retry_after.as_deref(),
            ));
        }
        let raw_bytes = read_response_limited(response, MAX_PROVIDER_RESPONSE_BYTES).await?;
        let raw_body = String::from_utf8_lossy(&raw_bytes).into_owned();

        debug!(body_len = raw_body.len(), "Anthropic raw response");

        let resp: AnthropicResponse = serde_json::from_str(&raw_body).map_err(|e| {
            Error::Provider(format!(
                "Failed to parse Anthropic response: {} (body_len={})",
                e,
                raw_body.len()
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
                        let arguments = block
                            .input
                            .clone()
                            .unwrap_or(Value::Object(serde_json::Map::new()));
                        tool_calls.push(ToolCallRequest {
                            id: id.clone(),
                            name: name.clone(),
                            arguments,
                            thought_signature: None,
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

        let usage = Self::normalize_usage(resp.usage.as_ref());

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

    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
    ) -> Result<mpsc::Receiver<StreamChunk>> {
        let url = format!("{}/messages", self.api_base);
        let model = Self::normalize_model(&self.model);

        let (system, anthropic_messages) = Self::convert_messages(messages);
        let anthropic_tools = Self::convert_tools(tools);

        let mut request = serde_json::json!({
            "model": model,
            "max_tokens": self.max_tokens,
            "temperature": self.temperature,
            "messages": anthropic_messages,
            "stream": true,
        });

        if let Some(sys) = &system {
            request["system"] = Value::String(sys.clone());
        }

        if !anthropic_tools.is_empty() {
            request["tools"] = Value::Array(anthropic_tools);
        }
        Self::apply_prompt_cache_breakpoints(&mut request);

        info!(url = %redact_url(&url), model = %model, "Starting Anthropic streaming call");

        let response = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .json(&request)
            .send()
            .await
            .map_err(|e| request_error("Anthropic stream request failed", e))?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body_len = consume_error_body(response).await;
            return Err(http_status_error_with_retry_after(
                "Anthropic stream",
                status,
                body_len,
                retry_after.as_deref(),
            ));
        }

        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut tool_calls: HashMap<String, ToolCallAccumulator> = HashMap::new();
            // index -> tool_id 映射，用于正确路由 delta 事件
            let mut tool_index_to_id: HashMap<usize, String> = HashMap::new();
            let mut accumulated_content = String::new();
            let mut finish_reason = "stop".to_string();
            let mut usage = Value::Null;
            // 记录上一个 SSE event 类型，用于跨行/跨 chunk 的 fallback 解析
            let mut last_event_type: Option<String> = None;
            let mut stream_guard = StreamGuard::new(MAX_PROVIDER_RESPONSE_BYTES);

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        if let Err(error) = stream_guard.observe(bytes.len()) {
                            let _ = tx
                                .send(StreamChunk::Error {
                                    message: error.to_string(),
                                })
                                .await;
                            return;
                        }
                        buffer.push_str(&String::from_utf8_lossy(&bytes));

                        // 处理 SSE 行
                        while let Some(pos) = buffer.find('\n') {
                            let line = buffer[..pos].trim().to_string();
                            buffer = buffer[pos + 1..].to_string();

                            // 解析 event 类型（SSE 规范要求）
                            if let Some(event_type) = line.strip_prefix("event: ") {
                                last_event_type = Some(event_type.to_string());
                                continue;
                            }

                            // 解析 data
                            if let Some(data) = line.strip_prefix("data: ") {
                                if data.is_empty() {
                                    continue;
                                }

                                // 解析 Anthropic 流式事件
                                if let Ok(event) =
                                    serde_json::from_str::<AnthropicStreamEvent>(data)
                                {
                                    match event.event_type.as_str() {
                                        "content_block_delta" => {
                                            if let Some(delta) = &event.delta {
                                                match delta.delta_type.as_str() {
                                                    "text_delta" => {
                                                        if let Some(text) = &delta.text {
                                                            accumulated_content.push_str(text);
                                                            let _ = tx
                                                                .send(StreamChunk::TextDelta {
                                                                    delta: text.clone(),
                                                                })
                                                                .await;
                                                        }
                                                    }
                                                    "input_json_delta" => {
                                                        // 工具调用参数增量 - 使用 event.index 直接定位
                                                        if let (Some(partial), Some(idx)) =
                                                            (&delta.partial_json, event.index)
                                                        {
                                                            // 使用事件的 index 字段直接查找对应的工具
                                                            if let Some(tool_id) =
                                                                tool_index_to_id.get(&idx)
                                                            {
                                                                if let Some(acc) =
                                                                    tool_calls.get_mut(tool_id)
                                                                {
                                                                    acc.arguments.push_str(partial);
                                                                    let _ = tx
                                                                        .send(StreamChunk::ToolCallDelta {
                                                                            index: idx,
                                                                            id: tool_id.clone(),
                                                                            delta: partial.clone(),
                                                                        })
                                                                        .await;
                                                                }
                                                            }
                                                        }
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        }
                                        "content_block_start" => {
                                            if let Some(content_block) = &event.content_block {
                                                if content_block.block_type.as_str() == "tool_use" {
                                                    if let (Some(id), Some(name), Some(idx)) = (
                                                        &content_block.id,
                                                        &content_block.name,
                                                        event.index,
                                                    ) {
                                                        // 建立 index -> id 映射
                                                        tool_index_to_id.insert(idx, id.clone());

                                                        let acc = tool_calls
                                                            .entry(id.clone())
                                                            .or_insert_with(|| {
                                                                ToolCallAccumulator {
                                                                    id: id.clone(),
                                                                    name: name.clone(),
                                                                    arguments: String::new(),
                                                                }
                                                            });
                                                        acc.id = id.clone();
                                                        acc.name = name.clone();

                                                        let _ = tx
                                                            .send(StreamChunk::ToolCallStart {
                                                                index: idx,
                                                                id: id.clone(),
                                                                name: name.clone(),
                                                            })
                                                            .await;
                                                    }
                                                }
                                            }
                                        }
                                        "content_block_stop" => {
                                            // 内容块结束
                                        }
                                        "message_start" => {
                                            if let Some(u) = &event.usage {
                                                usage = serde_json::json!({
                                                    "prompt_tokens": u.input_tokens,
                                                    "completion_tokens": u.output_tokens,
                                                    "cache_creation_input_tokens": u.cache_creation_input_tokens,
                                                    "cache_read_input_tokens": u.cache_read_input_tokens,
                                                });
                                            }
                                        }
                                        "message_delta" => {
                                            if let Some(delta) = &event.delta {
                                                if let Some(fr) = &delta.stop_reason {
                                                    finish_reason = match fr.as_str() {
                                                        "end_turn" => "stop".to_string(),
                                                        "tool_use" => "tool_calls".to_string(),
                                                        "max_tokens" => "length".to_string(),
                                                        other => other.to_string(),
                                                    };
                                                }
                                            }
                                            if let Some(u) = &event.usage {
                                                usage = serde_json::json!({
                                                    "prompt_tokens": u.input_tokens,
                                                    "completion_tokens": u.output_tokens,
                                                    "cache_creation_input_tokens": u.cache_creation_input_tokens,
                                                    "cache_read_input_tokens": u.cache_read_input_tokens,
                                                });
                                            }
                                        }
                                        "message_stop" => {
                                            // 消息结束
                                            let final_tool_calls: Vec<ToolCallRequest> = tool_calls
                                                .into_values()
                                                .map(|acc| acc.to_tool_call_request())
                                                .collect();

                                            let response = LLMResponse {
                                                content: if accumulated_content.is_empty() {
                                                    None
                                                } else {
                                                    Some(accumulated_content)
                                                },
                                                reasoning_content: None,
                                                tool_calls: final_tool_calls,
                                                finish_reason,
                                                usage,
                                            };
                                            let _ = tx.send(StreamChunk::Done { response }).await;
                                            return;
                                        }
                                        "error" => {
                                            if event.error.is_some() {
                                                let _ = tx
                                                    .send(StreamChunk::Error {
                                                        message:
                                                            "Anthropic stream returned an error"
                                                                .to_string(),
                                                    })
                                                    .await;
                                            }
                                            return;
                                        }
                                        _ => {
                                            debug!(
                                                event_type = %event.event_type,
                                                last_event_type = ?last_event_type,
                                                "收到意外的 SSE 事件类型"
                                            );
                                        }
                                    }
                                } else {
                                    let _ = tx
                                        .send(StreamChunk::Error {
                                            message: "Malformed Anthropic stream event".to_string(),
                                        })
                                        .await;
                                    return;
                                }
                            }
                        }
                        if let Err(error) = stream_guard.check_buffer(buffer.len()) {
                            let _ = tx
                                .send(StreamChunk::Error {
                                    message: error.to_string(),
                                })
                                .await;
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(StreamChunk::Error {
                                message: request_error("Anthropic stream read failed", e)
                                    .to_string(),
                            })
                            .await;
                        return;
                    }
                }
            }

            if let Err(error) = stream_guard.finish(false, "Anthropic stream") {
                let _ = tx
                    .send(StreamChunk::Error {
                        message: error.to_string(),
                    })
                    .await;
            }
        });

        Ok(rx)
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
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

/// Anthropic 流式事件
#[derive(Debug, Deserialize)]
struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    delta: Option<AnthropicStreamDelta>,
    #[serde(default)]
    content_block: Option<AnthropicStreamContentBlock>,
    #[serde(default)]
    usage: Option<AnthropicStreamUsage>,
    #[serde(default)]
    error: Option<AnthropicStreamError>,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamDelta {
    #[serde(rename = "type", default)]
    delta_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamError {
    #[serde(rename = "type")]
    #[allow(dead_code)] // 用于 serde 反序列化，API 返回但当前未使用
    error_type: String,
    #[allow(dead_code)] // 用于 serde 反序列化，用于错误处理
    message: String,
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
    fn prompt_cache_breakpoints_cover_system_tools_and_recent_history() {
        let mut request = serde_json::json!({
            "system": "stable system",
            "tools": [
                {"name": "read_file", "input_schema": {"type": "object"}},
                {"name": "write_file", "input_schema": {"type": "object"}}
            ],
            "messages": [
                {"role": "user", "content": "old history"},
                {"role": "assistant", "content": [{"type": "text", "text": "reply"}]},
                {"role": "user", "content": "current query"}
            ]
        });

        AnthropicProvider::apply_prompt_cache_breakpoints(&mut request);

        assert_eq!(request["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(request["tools"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(
            request["messages"][1]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert!(request["messages"][2]["content"].is_string());
    }

    #[test]
    fn anthropic_fourth_breakpoint_aligns_fork_with_parent_history_breakpoint() {
        // 父轮有 3 条消息 (L = 3)，断点落在 index = L - 2 = 1 ("assistant reply 1")
        // Fork Review 追加 [assistant_final, user_review] 后共 5 条消息 (L_fork = 5)，
        // L_fork - 4 = 1 与 L_fork - 2 = 3 均被打上断点，其中 index 1 与父轮完全对齐。
        let mut fork_request = serde_json::json!({
            "model": "claude-sonnet-4-5",
            "system": "System prompt",
            "tools": [{"name": "memory_manage"}],
            "messages": [
                {"role": "user", "content": "turn 1 user"},
                {"role": "assistant", "content": "turn 1 assistant"},
                {"role": "user", "content": "turn 2 user"},
                {"role": "assistant", "content": "turn 2 final response"},
                {"role": "user", "content": "Review the conversation above"}
            ]
        });

        AnthropicProvider::apply_prompt_cache_breakpoints(&mut fork_request);

        assert_eq!(
            fork_request["messages"][1]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(
            fork_request["messages"][3]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert!(fork_request["messages"][4]["content"].is_string());
    }

    #[test]
    fn anthropic_usage_preserves_cache_token_metrics() {
        let usage: AnthropicUsage = serde_json::from_value(serde_json::json!({
            "input_tokens": 100,
            "output_tokens": 25,
            "cache_creation_input_tokens": 80,
            "cache_read_input_tokens": 60
        }))
        .expect("parse usage");

        let normalized = AnthropicProvider::normalize_usage(Some(&usage));
        assert_eq!(normalized["prompt_tokens"], 100);
        assert_eq!(normalized["completion_tokens"], 25);
        assert_eq!(normalized["cache_creation_input_tokens"], 80);
        assert_eq!(normalized["cache_read_input_tokens"], 60);
    }

    #[test]
    fn test_convert_messages_tool_results() {
        let mut assistant = ChatMessage::assistant("");
        assistant.tool_calls = Some(vec![ToolCallRequest {
            id: "tc_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "/tmp/test"}),
            thought_signature: None,
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
            AnthropicProvider::normalize_model("anthropic/claude-opus-4-8"),
            "claude-opus-4-8"
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

    #[test]
    fn test_api_base_v1_auto_append() {
        // Custom api_base without /v1 should auto-append it
        let provider = AnthropicProvider::new(
            "test-key",
            Some("https://api.minimaxi.com/anthropic"),
            "test-model",
            1024,
            0.7,
        )
        .unwrap();
        assert_eq!(provider.api_base, "https://api.minimaxi.com/anthropic/v1");

        // Custom api_base already with /v1 should not double-append
        let provider2 = AnthropicProvider::new(
            "test-key",
            Some("https://api.minimaxi.com/anthropic/v1"),
            "test-model",
            1024,
            0.7,
        )
        .unwrap();
        assert_eq!(provider2.api_base, "https://api.minimaxi.com/anthropic/v1");

        // Default (no api_base) uses the built-in constant which already has /v1
        let provider3 = AnthropicProvider::new("test-key", None, "test-model", 1024, 0.7).unwrap();
        assert_eq!(provider3.api_base, "https://api.anthropic.com/v1");
    }
}
