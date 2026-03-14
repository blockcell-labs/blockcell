use std::sync::Arc;

use serde_json::Value;
use tracing::{info, warn};

use crate::mcp::client::{McpClient, McpTool};
use crate::{Tool, ToolContext, ToolSchema};
use blockcell_core::Result;

fn summarize_json(value: &Value, max_len: usize) -> String {
    let raw = serde_json::to_string(value).unwrap_or_else(|_| "<json-serialize-error>".to_string());
    if raw.chars().count() <= max_len {
        return raw;
    }
    raw.chars().take(max_len).collect::<String>() + "..."
}

/// A single MCP tool exposed as a local `Tool` implementation.
/// The qualified tool name uses `<server>__<tool>` format.
pub struct McpToolWrapper {
    /// Qualified name, leaked once at construction time.
    schema_name: &'static str,
    /// Description, leaked once at construction time.
    schema_desc: &'static str,
    /// Original (unqualified) tool name used when calling the MCP server.
    mcp_tool_name: String,
    input_schema: Value,
    client: Arc<McpClient>,
}

impl McpToolWrapper {
    pub fn new(server_name: &str, tool: McpTool, client: Arc<McpClient>) -> Self {
        let qualified = format!("{}__{}", server_name, tool.name);
        let schema_name: &'static str = Box::leak(qualified.into_boxed_str());
        let desc = tool.description.unwrap_or_default();
        let schema_desc: &'static str = Box::leak(desc.into_boxed_str());
        Self {
            schema_name,
            schema_desc,
            mcp_tool_name: tool.name,
            input_schema: tool.input_schema,
            client,
        }
    }
}

#[async_trait::async_trait]
impl Tool for McpToolWrapper {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.schema_name,
            description: self.schema_desc,
            parameters: self.input_schema.clone(),
        }
    }

    fn validate(&self, _params: &Value) -> Result<()> {
        Ok(())
    }

    async fn execute(&self, _ctx: ToolContext, params: Value) -> Result<Value> {
        let params_preview = summarize_json(&params, 800);
        info!(
            local_tool = %self.schema_name,
            remote_tool = %self.mcp_tool_name,
            params = %params_preview,
            "Executing MCP tool wrapper"
        );
        let result = self.client.call_tool(&self.mcp_tool_name, params).await;
        if let Err(error) = &result {
            warn!(
                local_tool = %self.schema_name,
                remote_tool = %self.mcp_tool_name,
                error = %error.to_string(),
                "MCP tool wrapper failed"
            );
        }
        result
    }
}

/// Holds a running MCP server client and all the tool wrappers it exposes.
pub struct McpToolProvider {
    pub server_name: String,
    pub client: Arc<McpClient>,
}

impl McpToolProvider {
    pub fn new(server_name: String, client: McpClient) -> Self {
        Self {
            server_name,
            client: Arc::new(client),
        }
    }

    pub fn from_shared(server_name: String, client: Arc<McpClient>) -> Self {
        Self {
            server_name,
            client,
        }
    }

    /// Return all tools from this provider as `Arc<dyn Tool>` instances.
    pub async fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mcp_tools = self.client.list_tools().await;
        mcp_tools
            .into_iter()
            .map(|t| -> Arc<dyn Tool> {
                let wrapper: Arc<dyn Tool> = Arc::new(McpToolWrapper::new(
                    &self.server_name,
                    t,
                    self.client.clone(),
                ));
                wrapper
            })
            .collect()
    }
}
