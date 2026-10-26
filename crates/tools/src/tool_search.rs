use async_trait::async_trait;
use blockcell_core::{Error, Result};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

use crate::{Tool, ToolContext, ToolSchema};

#[derive(Clone)]
pub struct ToolSearchEntry {
    pub name: String,
    pub description: String,
}

#[derive(Default)]
struct ToolSearchState {
    catalogs: HashMap<String, Vec<ToolSearchEntry>>,
    activated: HashMap<String, Vec<String>>,
}

static TOOL_SEARCH_STATE: Lazy<Mutex<ToolSearchState>> =
    Lazy::new(|| Mutex::new(ToolSearchState::default()));

pub fn set_tool_search_catalog(session_key: &str, entries: Vec<ToolSearchEntry>) {
    if let Ok(mut state) = TOOL_SEARCH_STATE.lock() {
        state.catalogs.insert(session_key.to_string(), entries);
    }
}

pub fn activate_tool_schema(session_key: &str, name: &str) {
    if let Ok(mut state) = TOOL_SEARCH_STATE.lock() {
        let names = state.activated.entry(session_key.to_string()).or_default();
        if !names.iter().any(|current| current == name) {
            names.push(name.to_string());
            names.sort();
        }
    }
}

pub fn activated_tool_schemas(session_key: &str) -> Vec<String> {
    TOOL_SEARCH_STATE
        .lock()
        .ok()
        .and_then(|state| state.activated.get(session_key).cloned())
        .unwrap_or_default()
}

pub struct ToolSearchTool;

#[async_trait]
impl Tool for ToolSearchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "tool_search".to_string(),
            description: "Find tools.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("query").and_then(Value::as_str).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: query".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let query = params["query"].as_str().unwrap().trim().to_lowercase();
        let state = TOOL_SEARCH_STATE
            .lock()
            .map_err(|_| Error::Tool("Tool search catalog is unavailable".to_string()))?;
        let matches = search_entries(
            state
                .catalogs
                .get(&ctx.session_key)
                .map(Vec::as_slice)
                .unwrap_or_default(),
            &query,
        );
        Ok(json!({"query": query, "matches": matches}))
    }
}

fn search_entries(entries: &[ToolSearchEntry], query: &str) -> Vec<Value> {
    let mut matches: Vec<_> = entries
        .iter()
        .filter(|entry| {
            query.is_empty()
                || entry.name.to_lowercase().contains(query)
                || entry.description.to_lowercase().contains(query)
        })
        .take(10)
        .map(|entry| json!({"name": entry.name, "description": entry.description}))
        .collect();
    matches.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_returns_sorted_allowed_tool_summaries() {
        let entries = vec![
            ToolSearchEntry {
                name: "web_fetch".to_string(),
                description: "Fetch a web page".to_string(),
            },
            ToolSearchEntry {
                name: "video_process".to_string(),
                description: "Process videos".to_string(),
            },
        ];
        let matches = search_entries(&entries, "video");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["name"], "video_process");
    }

    #[test]
    fn searched_tools_remain_activated_for_session() {
        activate_tool_schema("tool-search-test-session", "video_process");
        assert_eq!(
            activated_tool_schemas("tool-search-test-session"),
            vec!["video_process"]
        );
    }
}
