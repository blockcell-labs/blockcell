use std::collections::{BTreeMap, HashSet};

use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::mcp::client::McpTool;
use crate::{PromptContext, Tool, ToolContext, ToolSchema};

pub const MCP_SEARCH_TOOL_NAME: &str = "mcp_search_tools";

#[derive(Debug, Clone, Serialize)]
struct ToolIndexEntry {
    name: String,
    server: String,
    description: String,
    keywords: Vec<String>,
    schema: Value,
}

pub struct McpSearchTool {
    tool_index: Vec<ToolIndexEntry>,
    server_counts: BTreeMap<String, usize>,
}

impl McpSearchTool {
    pub fn new(tools: Vec<(String, McpTool)>) -> Self {
        let mut server_counts = BTreeMap::new();
        let tool_index = tools
            .into_iter()
            .map(|(server, tool)| {
                *server_counts.entry(server.clone()).or_insert(0) += 1;
                let name = format!("{}__{}", server, tool.name);
                let description = tool.description.unwrap_or_default();
                ToolIndexEntry {
                    name: name.clone(),
                    server,
                    keywords: extract_keywords(&format!("{} {}", name, description)),
                    schema: json!({
                        "type": "function",
                        "function": {
                            "name": name,
                            "description": description,
                            "parameters": tool.input_schema
                        }
                    }),
                    description,
                }
            })
            .collect();

        Self {
            tool_index,
            server_counts,
        }
    }

    pub fn search_value(&self, query: &str, limit: usize) -> Value {
        self.search_value_filtered(query, limit, |_| true)
    }

    fn search_value_filtered(
        &self,
        query: &str,
        limit: usize,
        allowed: impl Fn(&str) -> bool,
    ) -> Value {
        let limit = limit.clamp(1, 20);
        let query_lower = query.trim().to_lowercase();
        let query_words = extract_keywords(&query_lower);

        let mut scored = self
            .tool_index
            .iter()
            .filter(|entry| allowed(&entry.name))
            .filter_map(|entry| {
                let haystack = format!(
                    "{} {} {}",
                    entry.name.to_lowercase(),
                    entry.server.to_lowercase(),
                    entry.description.to_lowercase()
                );
                let mut score = 0u32;
                if !query_lower.is_empty() && haystack.contains(&query_lower) {
                    score += 20;
                }
                for word in &query_words {
                    if entry.name.to_lowercase().contains(word) {
                        score += 10;
                    }
                    if entry.description.to_lowercase().contains(word) {
                        score += 6;
                    }
                    if entry.keywords.iter().any(|keyword| keyword.contains(word)) {
                        score += 3;
                    }
                }
                (score > 0).then_some((score, entry))
            })
            .collect::<Vec<_>>();

        scored.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.name.cmp(&right.name))
        });

        let tools = scored
            .into_iter()
            .take(limit)
            .map(|(_, entry)| {
                json!({
                    "name": entry.name,
                    "server": entry.server,
                    "description": entry.description,
                    "schema": entry.schema
                })
            })
            .collect::<Vec<_>>();

        json!({
            "query": query,
            "count": tools.len(),
            "tools": tools,
            "hint": "Use one of the returned tool names in the next tool call when it matches the task."
        })
    }

    fn prompt_index(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "- You have access to {} MCP tools across {} servers. Use `{}` to find the right MCP tool before calling a specific `<server>__<tool>` tool.",
            self.tool_index.len(),
            self.server_counts.len(),
            MCP_SEARCH_TOOL_NAME
        ));
        lines.push("- Available MCP servers:".to_string());
        for (server, count) in &self.server_counts {
            lines.push(format!("  - {}: {} tools", server, count));
        }
        lines.push(format!(
            "- Example: `{}` with `{{\"query\":\"database query\",\"limit\":3}}`.",
            MCP_SEARCH_TOOL_NAME
        ));
        lines.join("\n")
    }
}

#[async_trait]
impl Tool for McpSearchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: MCP_SEARCH_TOOL_NAME.to_string(),
            description: format!(
                "Search {} available MCP tools by name, server, or capability and return matching full tool definitions.",
                self.tool_index.len()
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query describing the MCP capability you need."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of matching tools to return.",
                        "default": 5
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let query = params.get("query").and_then(Value::as_str).unwrap_or("");
        if query.trim().is_empty() {
            return Err(Error::Tool(
                "mcp_search_tools requires a non-empty query".to_string(),
            ));
        }
        Ok(())
    }

    fn prompt_rule(&self, _ctx: &PromptContext) -> Option<String> {
        Some(self.prompt_index())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let query = params
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(5);

        let profile_id = ctx.config.resolve_intent_profile_id(
            ctx.agent_identity
                .as_ref()
                .map(|identity| identity.agent_id.as_str()),
        );
        let profile = ctx
            .config
            .intent_router
            .as_ref()
            .and_then(|router| profile_id.as_ref().and_then(|id| router.profiles.get(id)));
        // Filter before ranking/limit: denied tools must neither expose their
        // schemas nor occupy all result slots ahead of authorized tools.
        Ok(self.search_value_filtered(query, limit, |name| {
            !profile.is_some_and(|profile| profile.deny_tools.iter().any(|denied| denied == name))
        }))
    }
}

fn extract_keywords(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| word.chars().count() > 2)
        .filter_map(|word| {
            let word = word.to_string();
            seen.insert(word.clone()).then_some(word)
        })
        .collect()
}
