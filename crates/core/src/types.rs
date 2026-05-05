use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::warn;

/// A tool call request that serializes to the OpenAI-compatible format:
/// `{id, type: "function", function: {name, arguments}}`
#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
    pub thought_signature: Option<String>,
}

impl Serialize for ToolCallRequest {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("type", "function")?;
        map.serialize_entry(
            "function",
            &serde_json::json!({
                "name": self.name,
                "arguments": self.arguments.to_string()
            }),
        )?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for ToolCallRequest {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let obj = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("expected object"))?;

        let id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        // New format: {id, type, function: {name, arguments}}
        if let Some(func) = obj.get("function").and_then(|v| v.as_object()) {
            let name = func
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let arguments = match func.get("arguments") {
                Some(serde_json::Value::String(s)) => {
                    serde_json::from_str(s).unwrap_or_else(|e| {
                        warn!(error = %e, raw = %s, "Failed to parse tool call arguments as JSON, using empty object");
                        serde_json::Value::Object(serde_json::Map::new())
                    })
                }
                Some(v) => v.clone(),
                None => serde_json::Value::Object(serde_json::Map::new()),
            };
            let thought_signature = obj
                .get("thought_signature")
                .or_else(|| obj.get("thoughtSignature"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            return Ok(ToolCallRequest {
                id,
                name,
                arguments,
                thought_signature,
            });
        }

        // Old flat format: {id, name, arguments}
        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let arguments = obj
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        let thought_signature = obj
            .get("thought_signature")
            .or_else(|| obj.get("thoughtSignature"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(ToolCallRequest {
            id,
            name,
            arguments,
            thought_signature,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMResponse {
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Vec<ToolCallRequest>,
    pub finish_reason: String,
    pub usage: serde_json::Value,
}

impl Default for LLMResponse {
    fn default() -> Self {
        Self {
            content: None,
            reasoning_content: None,
            tool_calls: Vec::new(),
            finish_reason: String::new(),
            usage: serde_json::Value::Null,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PermissionSet {
    pub permissions: HashSet<String>,
}

impl PermissionSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_permission(mut self, perm: &str) -> Self {
        self.permissions.insert(perm.to_string());
        self
    }

    pub fn has(&self, perm: &str) -> bool {
        self.permissions.contains(perm)
    }

    pub fn is_subset_of(&self, other: &PermissionSet) -> bool {
        self.permissions.is_subset(&other.permissions)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// 消息唯一标识符（可选，用于追踪消息位置）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub role: String,
    pub content: serde_json::Value,
    /// 推理内容 (思考过程，如 DeepSeek reasoning)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallRequest>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn system(content: &str) -> Self {
        Self {
            id: Some(uuid::Uuid::new_v4().to_string()),
            role: "system".to_string(),
            content: serde_json::Value::String(content.to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn user(content: &str) -> Self {
        Self {
            id: Some(uuid::Uuid::new_v4().to_string()),
            role: "user".to_string(),
            content: serde_json::Value::String(content.to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant(content: &str) -> Self {
        Self {
            id: Some(uuid::Uuid::new_v4().to_string()),
            role: "assistant".to_string(),
            content: serde_json::Value::String(content.to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// 创建带推理内容的助手消息（用于 DeepSeek thinking mode 等）。
    ///
    /// DeepSeek 的 thinking mode 要求：每次请求中，所有之前包含
    /// `reasoning_content` 的 assistant 消息必须携带原始的
    /// `reasoning_content` 传回 API，否则会触发 400 永久错误。
    /// 使用此构造器确保 `reasoning_content` 不会丢失。
    pub fn assistant_with_reasoning(content: &str, reasoning_content: Option<String>) -> Self {
        Self {
            id: Some(uuid::Uuid::new_v4().to_string()),
            role: "assistant".to_string(),
            content: serde_json::Value::String(content.to_string()),
            reasoning_content,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn tool_result(tool_call_id: &str, content: &str) -> Self {
        Self {
            id: Some(uuid::Uuid::new_v4().to_string()),
            role: "tool".to_string(),
            content: serde_json::Value::String(content.to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
            name: None,
        }
    }

    /// 为消息生成并设置唯一 ID
    pub fn with_id(mut self) -> Self {
        self.id = Some(uuid::Uuid::new_v4().to_string());
        self
    }

    /// 设置指定的 ID
    pub fn with_specific_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// 获取消息 ID（如果没有则生成一个）
    pub fn get_or_create_id(&mut self) -> &str {
        if self.id.is_none() {
            self.id = Some(uuid::Uuid::new_v4().to_string());
        }
        self.id.as_ref().unwrap()
    }
}

/// 流式响应的单个块
#[derive(Debug, Clone)]
pub enum StreamChunk {
    /// 文本内容增量
    TextDelta { delta: String },
    /// 推理内容增量 (思考过程，如 DeepSeek reasoning)
    ReasoningDelta { delta: String },
    /// 工具调用开始
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    /// 工具调用参数增量 (JSON 字符串片段)
    ToolCallDelta {
        index: usize,
        id: String,
        delta: String,
    },
    /// 流结束，包含完整响应
    Done { response: LLMResponse },
    /// 错误
    Error { message: String },
}

/// 用于累积工具调用参数的辅助结构
#[derive(Debug, Default, Clone)]
pub struct ToolCallAccumulator {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl ToolCallAccumulator {
    /// 构建完整的 ToolCallRequest
    pub fn to_tool_call_request(&self) -> ToolCallRequest {
        let arguments: serde_json::Value = if self.arguments.is_empty() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&self.arguments).unwrap_or_else(|e| {
                warn!(error = %e, raw = %self.arguments, "Failed to parse accumulated tool call arguments, using empty object");
                serde_json::Value::Object(serde_json::Map::new())
            })
        };
        ToolCallRequest {
            id: self.id.clone(),
            name: self.name.clone(),
            arguments,
            thought_signature: None,
        }
    }
}

/// DeepSeek thinking mode 要求：所有包含 tool_calls 的 assistant 消息
/// 必须携带 `reasoning_content` 字段回传 API，否则会触发 400 错误。
/// 此函数检查并修复缺失的 reasoning_content。
///
/// 仅在 thinking mode 开启且模型为 DeepSeek thinking 模型时生效。
pub fn sanitize_thinking_mode_messages(
    messages: &mut [ChatMessage],
    model: &str,
    reasoning_effort: Option<&str>,
) {
    // 如果 thinking mode 未开启，不做任何处理
    if reasoning_effort.is_none() || reasoning_effort == Some("off") {
        return;
    }
    // 如果模型不是 DeepSeek thinking 模型，跳过
    if !is_thinking_model(model) {
        return;
    }
    // 对于有 tool_calls 但没有 reasoning_content 的 assistant 消息，
    // 注入空 reasoning_content 占位符以避免 400 错误
    for msg in messages.iter_mut() {
        if msg.role == "assistant"
            && msg.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty())
            && msg.reasoning_content.is_none()
        {
            msg.reasoning_content = Some(String::new());
        }
    }
}

/// 判断模型是否为 DeepSeek thinking 模型（需要 reasoning_content 回传）。
pub fn is_thinking_model(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    lower.contains("deepseek-v4")
        || lower.contains("deepseek-reasoner")
        || lower.contains("deepseek-r1")
}
