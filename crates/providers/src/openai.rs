use async_trait::async_trait;
use blockcell_core::config::ToolCallMode;
use blockcell_core::types::{
    ChatMessage, LLMResponse, StreamChunk, ToolCallAccumulator, ToolCallRequest,
};
use blockcell_core::{Error, Result};
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::client::build_http_client;
use crate::security::{
    consume_error_body, http_status_error_with_retry_after, read_response_limited, redact_url,
    request_error, StreamGuard, MAX_PROVIDER_RESPONSE_BYTES,
};
use crate::Provider;

mod tool_markup;

pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    api_base: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
    tool_call_mode: AtomicU8,
    /// Provider 名称（如 "deepseek"、"openrouter"、"nvidia-nim" 等），
    /// 用于判断 reasoning_effort 参数的注入方式。
    provider_name: String,
    /// 推理强度配置（如 "off"、"high"、"max"），
    /// None 表示不注入任何 thinking/reasoning_effort 参数。
    reasoning_effort: Option<String>,
}

struct TextToolMarkupStreamFilter {
    enabled: bool,
    suppressing: bool,
    pending: String,
}

impl TextToolMarkupStreamFilter {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            suppressing: false,
            pending: String::new(),
        }
    }

    fn push(&mut self, delta: &str) -> Option<String> {
        if !self.enabled {
            return Some(delta.to_string());
        }

        self.pending.push_str(delta);

        if self.suppressing {
            if OpenAIProvider::text_tool_markup_delta_closes(&self.pending) {
                self.pending.clear();
                self.suppressing = false;
            }
            return None;
        }

        if let Some(marker_start) = OpenAIProvider::find_text_tool_marker_start(&self.pending) {
            let visible = self.pending[..marker_start].to_string();
            let hidden = self.pending[marker_start..].to_string();
            self.pending = hidden;
            self.suppressing = true;
            if OpenAIProvider::text_tool_markup_delta_closes(&self.pending) {
                self.pending.clear();
                self.suppressing = false;
            }
            return if visible.is_empty() {
                None
            } else {
                Some(visible)
            };
        }

        let held_suffix_len = OpenAIProvider::text_tool_marker_candidate_suffix_len(&self.pending);
        if held_suffix_len == self.pending.len() {
            return None;
        }

        let emit_len = self.pending.len() - held_suffix_len;
        let visible = self.pending[..emit_len].to_string();
        self.pending = self.pending[emit_len..].to_string();
        if visible.is_empty() {
            None
        } else {
            Some(visible)
        }
    }

    fn flush(&mut self) -> Option<String> {
        if !self.enabled || !self.suppressing {
            let visible = std::mem::take(&mut self.pending);
            if visible.is_empty() {
                None
            } else {
                Some(visible)
            }
        } else {
            self.pending.clear();
            None
        }
    }
}

impl OpenAIProvider {
    fn sanitize_messages_for_native_tools(messages: &[ChatMessage]) -> Vec<ChatMessage> {
        let mut sanitized = Vec::with_capacity(messages.len());
        let mut i = 0;

        while i < messages.len() {
            let msg = &messages[i];

            // 孤立的 tool 消息（没有前置 assistant tool_calls 轮次）直接跳过
            if msg.role == "tool" {
                i += 1;
                continue;
            }

            if msg.role == "assistant" {
                if let Some(tool_calls) = &msg.tool_calls {
                    if !tool_calls.is_empty() {
                        // 保存 tool_call ID 用于验证
                        let round_ids: std::collections::HashSet<String> =
                            tool_calls.iter().map(|tc| tc.id.clone()).collect();
                        let id_count = round_ids.len();

                        // 临时缓冲：先收集后续 tool 消息，验证完整后再 push
                        let mut temp_tool_msgs: Vec<&ChatMessage> = Vec::new();
                        let mut matched_count = 0usize;
                        let mut j = i + 1;
                        while j < messages.len() && messages[j].role == "tool" {
                            if let Some(id) = &messages[j].tool_call_id {
                                if round_ids.contains(id.as_str()) {
                                    matched_count += 1;
                                    temp_tool_msgs.push(&messages[j]);
                                } else {
                                    warn!(
                                        tool_call_id = %id,
                                        "丢弃 tool 消息：ID 与当前轮的 tool_call 不匹配"
                                    );
                                }
                            }
                            j += 1;
                        }

                        // 只有所有 tool_call 都有对应的 tool 结果时，才保留完整轮次
                        if matched_count == id_count {
                            sanitized.push(msg.clone());
                            for tm in temp_tool_msgs {
                                sanitized.push(tm.clone());
                            }
                        } else {
                            warn!(
                                tool_call_count = id_count,
                                matched = matched_count,
                                "跳过不完整的 tool round：不是所有 tool_call 都有对应的结果"
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
            "",
            None,
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
        provider_name: &str,
        reasoning_effort: Option<&str>,
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
            tool_call_mode: AtomicU8::new(Self::mode_to_u8(tool_call_mode)),
            provider_name: provider_name.to_string(),
            reasoning_effort: reasoning_effort.map(|s| s.to_string()),
        })
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

    /// 根据 reasoning_effort 配置和 provider 名称，向请求 body 注入
    /// thinking / reasoning_effort / chat_template_kwargs 参数。
    ///
    /// 仅对已知支持 thinking/reasoning 的 provider 注入参数：
    /// - DeepSeek 原生及 OpenRouter/Novita/Fireworks/SGLang 等中继使用
    ///   顶层 `thinking` 和 `reasoning_effort` 字段
    /// - NVIDIA NIM 使用 `chat_template_kwargs` 包装
    /// - 其他 provider（OpenAI、Groq、Kimi、Zhipu 等）不支持，不注入
    pub fn apply_reasoning_effort(body: &mut Value, effort: Option<&str>, provider_name: &str) {
        let Some(effort) = effort else { return };
        // 仅对支持 thinking/reasoning 的 provider 生效
        if !supports_thinking_mode(provider_name) {
            return;
        }
        let normalized = effort.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "off" | "disabled" | "none" | "false" => {
                if is_nvidia_nim(provider_name) {
                    body["chat_template_kwargs"] = json!({"thinking": false});
                } else {
                    body["thinking"] = json!({"type": "disabled"});
                }
            }
            "low" | "minimal" | "medium" | "mid" | "high" | "" => {
                if is_nvidia_nim(provider_name) {
                    body["chat_template_kwargs"] =
                        json!({"thinking": true, "reasoning_effort": "high"});
                } else {
                    body["reasoning_effort"] = json!("high");
                    body["thinking"] = json!({"type": "enabled"});
                }
            }
            "xhigh" | "max" | "highest" => {
                if is_nvidia_nim(provider_name) {
                    body["chat_template_kwargs"] =
                        json!({"thinking": true, "reasoning_effort": "max"});
                } else {
                    body["reasoning_effort"] = json!("max");
                    body["thinking"] = json!({"type": "enabled"});
                }
            }
            _ => {
                // 未知值不修改，由 provider 自行决定
            }
        }
    }

    fn normalize_usage(usage: Option<Value>) -> Value {
        let mut usage = usage.unwrap_or(Value::Null);
        let cached_tokens = usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .or_else(|| {
                usage
                    .get("input_tokens_details")
                    .and_then(|details| details.get("cached_tokens"))
                    .and_then(Value::as_u64)
            });
        if let (Some(cached_tokens), Some(object)) = (cached_tokens, usage.as_object_mut()) {
            object.insert(
                "cache_read_input_tokens".to_string(),
                Value::from(cached_tokens),
            );
        }
        usage
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
            prompt_cache_key: crate::prompt_utils::prompt_cache_key_with_tools(
                messages,
                tools,
                &self.model,
            ),
        };

        let mode = if use_native_tools && !tools.is_empty() {
            "native"
        } else if !tools.is_empty() {
            "text"
        } else {
            "no-tools"
        };
        info!(url = %redact_url(&url), model = %self.model, tools_count = tools.len(), messages_count = messages.len(), mode = %mode, "Calling LLM");

        // 序列化为 Value 以便注入 reasoning_effort/thinking/chat_template_kwargs
        let mut body: Value = serde_json::to_value(&request)
            .map_err(|e| Error::Provider(format!("Failed to serialize request: {}", e)))?;
        Self::apply_reasoning_effort(
            &mut body,
            self.reasoning_effort.as_deref(),
            &self.provider_name,
        );
        let request_body = serde_json::to_string(&body)
            .map_err(|e| Error::Provider(format!("Failed to serialize request: {}", e)))?;
        debug!(body_len = request_body.len(), "Request body prepared");
        debug!(body_len = request_body.len(), "LLM request detail redacted");

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .body(request_body)
            .send()
            .await
            .map_err(|e| request_error("LLM request failed", e))?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body_len = consume_error_body(response).await;
            error!(status = %status, body_len, "LLM API error");
            return Err(http_status_error_with_retry_after(
                "LLM",
                status,
                body_len,
                retry_after.as_deref(),
            ));
        }
        let raw_bytes = read_response_limited(response, MAX_PROVIDER_RESPONSE_BYTES).await?;
        let raw_body = String::from_utf8_lossy(&raw_bytes).into_owned();

        debug!(body_len = raw_body.len(), "LLM response received");

        let trimmed_body = raw_body.trim();
        if trimmed_body.is_empty() {
            return Err(Error::Provider(format!(
                "LLM returned an empty response body (status {}, len {})",
                status,
                raw_body.len()
            )));
        }

        let chat_response: ChatResponse = serde_json::from_str(trimmed_body).map_err(|e| {
            Error::Provider(format!(
                "Failed to parse response: {} (body_len={})",
                e,
                raw_body.len()
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
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
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
#[allow(clippy::too_many_arguments)]
fn finalize_stream_response(
    mode: ToolCallMode,
    tools_available: bool,
    raw_content_for_parsing: String,
    accumulated_content: String,
    accumulated_reasoning: String,
    mut tool_calls: Vec<ToolCallRequest>,
    finish_reason: String,
    usage: Value,
) -> LLMResponse {
    let mut content = accumulated_content;

    // 使用 raw_content 解析文本工具调用（包含 <tool_call> 标记）
    // filtered_content 已过滤掉工具标记，只用于对用户展示
    if tools_available
        && !matches!(mode, ToolCallMode::None)
        && tool_calls.is_empty()
        && !raw_content_for_parsing.is_empty()
    {
        let (remaining_text, parsed_calls) =
            OpenAIProvider::parse_text_tool_calls(&raw_content_for_parsing);
        if !parsed_calls.is_empty() {
            info!(
                count = parsed_calls.len(),
                "Parsed text-based tool calls from streaming response (raw content)"
            );
            // 使用 remaining_text 作为展示内容（已剥离工具调用标记）
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
        usage: OpenAIProvider::normalize_usage(Some(usage)),
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
    /// 流式请求中包含 token 用量统计（DeepSeek V4 等）。
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
}

/// 流式请求选项：让服务端在最后一个 chunk 中返回 usage 统计。
#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
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
                            error_kind = %err,
                            arguments_len = tc.function.arguments.len(),
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
                    usage: Self::normalize_usage(chat_response.usage),
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
                    usage: Self::normalize_usage(chat_response.usage),
                });
            }

            if !content.is_empty() || matches!(mode, ToolCallMode::Native) {
                return Ok(LLMResponse {
                    content: Some(content),
                    reasoning_content,
                    tool_calls: vec![],
                    finish_reason: choice.finish_reason.unwrap_or_else(|| "stop".to_string()),
                    usage: Self::normalize_usage(chat_response.usage),
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
            usage: Self::normalize_usage(chat_response.usage),
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
                (
                    Self::sanitize_messages_for_native_tools(messages),
                    tools.to_vec(),
                )
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
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            prompt_cache_key: crate::prompt_utils::prompt_cache_key_with_tools(
                messages,
                tools,
                &self.model,
            ),
        };

        // 序列化为 Value 以便注入 reasoning_effort/thinking/chat_template_kwargs
        let mut body: Value = serde_json::to_value(&request)
            .map_err(|e| Error::Provider(format!("Failed to serialize stream request: {}", e)))?;
        Self::apply_reasoning_effort(
            &mut body,
            self.reasoning_effort.as_deref(),
            &self.provider_name,
        );

        info!(url = %redact_url(&url), model = %self.model, "Starting streaming LLM call");
        debug!(
            body_len = serde_json::to_string(&body)
                .map(|value| value.len())
                .unwrap_or(0),
            "Streaming request detail redacted"
        );

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| request_error("LLM stream request failed", e))?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body_len = consume_error_body(response).await;
            return Err(http_status_error_with_retry_after(
                "LLM stream",
                status,
                body_len,
                retry_after.as_deref(),
            ));
        }

        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut tool_calls: HashMap<usize, ToolCallAccumulator> = HashMap::new();
            // raw_content 保留完整内容（含 <tool_call> 标记），用于文本工具调用解析
            // accumulated_content 保存过滤后的内容，用于最终响应对用户展示
            let mut raw_content = String::new();
            let mut accumulated_content = String::new();
            let mut accumulated_reasoning = String::new();
            let mut finish_reason = "stop".to_string();
            let mut usage = Value::Null;
            let mut completed = false;
            let mut text_tool_markup_filter = TextToolMarkupStreamFilter::new(
                tools_available && !matches!(mode, ToolCallMode::None),
            );
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

                            if let Some(data) = line.strip_prefix("data: ") {
                                if data == "[DONE]" {
                                    if let Some(delta) = text_tool_markup_filter.flush() {
                                        let _ = tx.send(StreamChunk::TextDelta { delta }).await;
                                    }
                                    // 构建最终响应
                                    let final_tool_calls: Vec<ToolCallRequest> = tool_calls
                                        .into_values()
                                        .map(|acc| acc.to_tool_call_request())
                                        .collect();

                                    let response = finalize_stream_response(
                                        mode,
                                        tools_available,
                                        raw_content,
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
                                        // raw_content 保留完整内容（含工具调用标记），用于后续 parse_text_tool_calls
                                        // filtered_content 只用于对用户流式展示
                                        if let Some(content) = &choice.delta.content {
                                            raw_content.push_str(content);
                                            if let Some(delta) =
                                                text_tool_markup_filter.push(content)
                                            {
                                                accumulated_content.push_str(&delta);
                                                let _ =
                                                    tx.send(StreamChunk::TextDelta { delta }).await;
                                            }
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
                                            completed = true;
                                            finish_reason = fr.clone();
                                        }
                                    }

                                    if let Some(u) = &chunk.usage {
                                        usage = u.clone();
                                    }
                                } else {
                                    let _ = tx
                                        .send(StreamChunk::Error {
                                            message: "Malformed OpenAI stream event".to_string(),
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
                                message: request_error("OpenAI stream read failed", e).to_string(),
                            })
                            .await;
                        return;
                    }
                }
            }

            if let Err(error) = stream_guard.finish(completed, "OpenAI stream") {
                let _ = tx
                    .send(StreamChunk::Error {
                        message: error.to_string(),
                    })
                    .await;
                return;
            }

            if let Some(delta) = text_tool_markup_filter.flush() {
                let _ = tx.send(StreamChunk::TextDelta { delta }).await;
            }
            let final_tool_calls: Vec<ToolCallRequest> = tool_calls
                .into_values()
                .map(|acc| acc.to_tool_call_request())
                .collect();
            let response = finalize_stream_response(
                mode,
                tools_available,
                raw_content,
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

/// 判断 provider 是否为 NVIDIA NIM 类型。
/// NVIDIA NIM 使用 `chat_template_kwargs` 而非顶层 `thinking`/`reasoning_effort`。
fn is_nvidia_nim(provider_name: &str) -> bool {
    let lower = provider_name.to_ascii_lowercase();
    lower.contains("nvidia")
        || lower
            .split(|c: char| !c.is_alphanumeric())
            .any(|token| token == "nim")
}

/// 判断 provider 是否支持 thinking/reasoning 模式参数。
/// 仅 DeepSeek 系列及其中继（OpenRouter/Novita/Fireworks/SGLang）和 NVIDIA NIM 支持。
/// OpenAI、Groq、Kimi、Zhipu 等不支持，注入这些参数会导致 API 报错。
fn supports_thinking_mode(provider_name: &str) -> bool {
    let lower = provider_name.to_ascii_lowercase();
    lower.contains("deepseek")
        || lower.contains("openrouter")
        || lower.contains("novita")
        || lower.contains("fireworks")
        || lower.contains("sglang")
        || is_nvidia_nim(provider_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::types::ChatMessage;

    #[test]
    fn test_apply_reasoning_effort_deepseek_max() {
        let mut body = json!({"model": "deepseek-v4-pro", "messages": []});
        OpenAIProvider::apply_reasoning_effort(&mut body, Some("max"), "deepseek");
        assert_eq!(body["reasoning_effort"].as_str(), Some("max"));
        assert_eq!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("enabled")
        );
    }

    #[test]
    fn test_apply_reasoning_effort_deepseek_off() {
        let mut body = json!({"model": "deepseek-v4-pro", "messages": []});
        OpenAIProvider::apply_reasoning_effort(&mut body, Some("off"), "deepseek");
        assert_eq!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("disabled")
        );
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn test_apply_reasoning_effort_nvidia_nim_max() {
        let mut body = json!({"model": "deepseek-v4-pro", "messages": []});
        OpenAIProvider::apply_reasoning_effort(&mut body, Some("max"), "nvidia-nim");
        assert_eq!(
            body.pointer("/chat_template_kwargs/thinking")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            body.pointer("/chat_template_kwargs/reasoning_effort")
                .and_then(Value::as_str),
            Some("max")
        );
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn test_apply_reasoning_effort_nvidia_nim_off() {
        let mut body = json!({"model": "deepseek-v4-pro", "messages": []});
        OpenAIProvider::apply_reasoning_effort(&mut body, Some("off"), "nvidia-nim");
        assert_eq!(
            body.pointer("/chat_template_kwargs/thinking")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert!(body
            .pointer("/chat_template_kwargs/reasoning_effort")
            .is_none());
    }

    #[test]
    fn test_apply_reasoning_effort_none_does_nothing() {
        let mut body = json!({"model": "deepseek-v4-pro", "messages": []});
        OpenAIProvider::apply_reasoning_effort(&mut body, None, "deepseek");
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn test_apply_reasoning_effort_unsupported_provider_not_affected() {
        // OpenAI、Groq、Kimi 等不支持 thinking 模式，不应注入任何参数
        let mut body = json!({"model": "gpt-5.5", "messages": []});
        OpenAIProvider::apply_reasoning_effort(&mut body, Some("high"), "openai");
        assert!(body.get("thinking").is_none());
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("chat_template_kwargs").is_none());

        let mut body2 = json!({"model": "kimi-k2.6", "messages": []});
        OpenAIProvider::apply_reasoning_effort(&mut body2, Some("max"), "kimi");
        assert!(body2.get("thinking").is_none());
        assert!(body2.get("reasoning_effort").is_none());

        let mut body3 = json!({"model": "qwen/qwen3-32b", "messages": []});
        OpenAIProvider::apply_reasoning_effort(&mut body3, Some("high"), "groq");
        assert!(body3.get("thinking").is_none());
        assert!(body3.get("reasoning_effort").is_none());
    }

    #[test]
    fn test_apply_reasoning_effort_openrouter_supported() {
        // OpenRouter 作为 DeepSeek 中继，应支持 thinking 模式
        let mut body = json!({"model": "deepseek-v4-pro", "messages": []});
        OpenAIProvider::apply_reasoning_effort(&mut body, Some("high"), "openrouter");
        assert_eq!(body["reasoning_effort"].as_str(), Some("high"));
        assert_eq!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("enabled")
        );
    }

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
    fn test_parse_deepseek_dsml_tool_calls() {
        let content = r#"<｜DSML｜tool_calls>
<｜DSML｜invoke name="list_dir">
<｜DSML｜parameter name="path" string="true">/home/blockcell/.blockcell/workspace/skills</｜DSML｜parameter>
</｜DSML｜invoke>
<｜DSML｜invoke name="list_dir">
<｜DSML｜parameter name="path" string="true">/home/blockcell/.blockcell/skills</｜DSML｜parameter>
</｜DSML｜invoke>
</｜DSML｜tool_calls>"#;

        let (remaining, calls) = OpenAIProvider::parse_text_tool_calls(content);

        assert!(remaining.is_empty(), "remaining: {:?}", remaining);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "list_dir");
        assert_eq!(
            calls[0].arguments["path"],
            "/home/blockcell/.blockcell/workspace/skills"
        );
        assert_eq!(calls[1].name, "list_dir");
        assert_eq!(
            calls[1].arguments["path"],
            "/home/blockcell/.blockcell/skills"
        );
    }

    #[test]
    fn test_stream_filter_suppresses_fragmented_dsml_markup() {
        let mut filter = TextToolMarkupStreamFilter::new(true);

        assert_eq!(
            filter.push("Let me check.\n"),
            Some("Let me check.\n".to_string())
        );
        assert_eq!(filter.push("<"), None);
        assert_eq!(filter.push("｜DS"), None);
        assert_eq!(
            filter.push("ML｜tool_calls><｜DSML｜invoke name=\"list_dir\">"),
            None
        );
        assert_eq!(
            filter.push("<｜DSML｜parameter name=\"path\">/tmp</｜DSML｜parameter>"),
            None
        );
        assert_eq!(filter.push("</｜DSML｜invoke></｜DSML｜tool_calls>"), None);
        assert_eq!(filter.flush(), None);
    }

    #[test]
    fn test_stream_filter_releases_plain_text_suffix_candidate() {
        let mut filter = TextToolMarkupStreamFilter::new(true);

        assert_eq!(filter.push("1 <"), Some("1 ".to_string()));
        assert_eq!(filter.push(" 2"), Some("< 2".to_string()));
        assert_eq!(filter.flush(), None);
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
        // raw_content_for_parsing 包含 <tool_call> 标记，accumulated_content 已过滤
        let raw_text = "我来帮您查看上一级目录。\n<tool_call>\n<function=list_dir>\n<parameter=path>/Users/apple/.blockcell</parameter>\n</function>\n</tool_call>".to_string();
        let response = finalize_stream_response(
            ToolCallMode::Text,
            true,
            raw_text,
            String::new(), // accumulated_content（对用户展示的过滤后文本）
            String::new(), // accumulated_reasoning
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
    fn test_finalize_stream_response_parses_dsml_tool_call_blocks() {
        // raw_content_for_parsing 包含 DSML 工具调用标记
        let raw_text = "<｜DSML｜tool_calls><｜DSML｜invoke name=\"list_dir\"><｜DSML｜parameter name=\"path\" string=\"true\">/tmp</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>".to_string();
        let response = finalize_stream_response(
            ToolCallMode::Text,
            true,
            raw_text,
            String::new(), // accumulated_content（对用户展示的过滤后文本）
            String::new(), // accumulated_reasoning
            vec![],
            "stop".to_string(),
            Value::Null,
        );

        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "list_dir");
        assert_eq!(response.tool_calls[0].arguments["path"], "/tmp");
        assert!(response.content.is_none());
        assert_eq!(response.finish_reason, "tool_calls");
    }

    #[test]
    fn test_sanitize_messages_for_native_tools_drops_incomplete_tool_round() {
        let messages = vec![
            ChatMessage::user("先写一个故事"),
            ChatMessage {
                id: None,
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
                id: None,
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
#[test]
fn openai_usage_normalizes_cached_prompt_tokens() {
    let usage = serde_json::json!({
        "prompt_tokens": 120,
        "completion_tokens": 30,
        "prompt_tokens_details": {"cached_tokens": 90}
    });

    let normalized = OpenAIProvider::normalize_usage(Some(usage));
    assert_eq!(normalized["cache_read_input_tokens"], 90);
    assert_eq!(normalized["prompt_tokens"], 120);
}
