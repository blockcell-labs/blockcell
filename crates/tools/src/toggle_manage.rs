use async_trait::async_trait;
use blockcell_core::Result;
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolSchema};

pub struct ToggleManageTool;

#[async_trait]
impl Tool for ToggleManageTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "toggle_manage",
            description: "Enable or disable skills and tools. Disabled items are excluded from conversations. Use action='list' to see current toggle states, action='set' to enable/disable.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "set"],
                        "description": "list: show all toggle states. set: enable or disable a skill/capability."
                    },
                    "category": {
                        "type": "string",
                        "enum": ["skills", "tools"],
                        "description": "Category to operate on (required for 'set' action)."
                    },
                    "name": {
                        "type": "string",
                        "description": "Name of the skill or tool to toggle (required for 'set' action)."
                    },
                    "enabled": {
                        "type": "boolean",
                        "description": "true to enable, false to disable (required for 'set' action)."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "list" => Ok(()),
            "set" => {
                if params.get("category").and_then(|v| v.as_str()).is_none() {
                    return Err(blockcell_core::Error::Config("'category' is required for 'set' action".into()));
                }
                if params.get("name").and_then(|v| v.as_str()).is_none() {
                    return Err(blockcell_core::Error::Config("'name' is required for 'set' action".into()));
                }
                if params.get("enabled").and_then(|v| v.as_bool()).is_none() {
                    return Err(blockcell_core::Error::Config("'enabled' (boolean) is required for 'set' action".into()));
                }
                Ok(())
            }
            _ => Err(blockcell_core::Error::Config(format!("Unknown action: '{}'. Use 'list' or 'set'.", action))),
        }
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("list");
        let toggles_path = ctx.workspace.join("toggles.json");

        match action {
            "list" => {
                let store = load_toggles(&toggles_path);
                Ok(store)
            }
            "set" => {
                let category = params.get("category").and_then(|v| v.as_str()).unwrap_or("");
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let enabled = params.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);

                if category != "skills" && category != "tools" {
                    return Ok(json!({ "error": "category must be 'skills' or 'tools'" }));
                }

                let mut store = load_toggles(&toggles_path);

                // Ensure category object exists
                if store.get(category).is_none() {
                    store[category] = json!({});
                }

                // If enabled=true, remove the entry (default is enabled).
                // If enabled=false, store false explicitly.
                if enabled {
                    if let Some(obj) = store[category].as_object_mut() {
                        obj.remove(name);
                    }
                } else {
                    store[category][name] = json!(false);
                }

                // Write back
                let content = serde_json::to_string_pretty(&store).unwrap_or_default();
                std::fs::write(&toggles_path, &content)
                    .map_err(|e| blockcell_core::Error::Config(format!("Failed to write toggles: {}", e)))?;

                let status_str = if enabled { "enabled" } else { "disabled" };
                Ok(json!({
                    "status": "ok",
                    "message": format!("{} '{}' has been {}", category, name, status_str),
                    "category": category,
                    "name": name,
                    "enabled": enabled,
                }))
            }
            _ => Ok(json!({ "error": format!("Unknown action: {}", action) })),
        }
    }
}

fn load_toggles(path: &std::path::Path) -> Value {
    if let Ok(content) = std::fs::read_to_string(path) {
        if let Ok(val) = serde_json::from_str::<Value>(&content) {
            return val;
        }
    }
    json!({ "skills": {}, "tools": {} })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_toggle_manage_schema() {
        let tool = ToggleManageTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "toggle_manage");
    }

    #[test]
    fn test_toggle_manage_validate_list() {
        let tool = ToggleManageTool;
        assert!(tool.validate(&json!({"action": "list"})).is_ok());
    }

    #[test]
    fn test_toggle_manage_validate_set() {
        let tool = ToggleManageTool;
        assert!(tool.validate(&json!({
            "action": "set", "category": "skills", "name": "test", "enabled": false
        })).is_ok());
    }

    #[test]
    fn test_toggle_manage_validate_set_missing_fields() {
        let tool = ToggleManageTool;
        assert!(tool.validate(&json!({"action": "set"})).is_err());
        assert!(tool.validate(&json!({"action": "set", "category": "skills"})).is_err());
        assert!(tool.validate(&json!({"action": "set", "category": "skills", "name": "x"})).is_err());
    }

    #[test]
    fn test_toggle_manage_validate_unknown_action() {
        let tool = ToggleManageTool;
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }

    #[test]
    fn test_load_toggles_missing_file() {
        let val = load_toggles(std::path::Path::new("/nonexistent/toggles.json"));
        assert_eq!(val, json!({"skills": {}, "tools": {}}));
    }
}
