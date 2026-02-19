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
}

impl Serialize for ToolCallRequest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("type", "function")?;
        map.serialize_entry("function", &serde_json::json!({
            "name": self.name,
            "arguments": self.arguments.to_string()
        }))?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for ToolCallRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let obj = value.as_object().ok_or_else(|| serde::de::Error::custom("expected object"))?;

        let id = obj.get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        // New format: {id, type, function: {name, arguments}}
        if let Some(func) = obj.get("function").and_then(|v| v.as_object()) {
            let name = func.get("name")
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
            return Ok(ToolCallRequest { id, name, arguments });
        }

        // Old flat format: {id, name, arguments}
        let name = obj.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let arguments = obj.get("arguments")
            .cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        Ok(ToolCallRequest { id, name, arguments })
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
    pub role: String,
    pub content: serde_json::Value,
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
            role: "assistant".to_string(),
            content: serde_json::Value::String(content.to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn tool_result(tool_call_id: &str, content: &str) -> Self {
        Self {
            role: "tool".to_string(),
            content: serde_json::Value::String(content.to_string()),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
            name: None,
        }
    }
}
