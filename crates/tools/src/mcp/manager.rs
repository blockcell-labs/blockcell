use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{info, warn};

use blockcell_core::mcp_config::McpResolvedConfig;
use blockcell_core::{Error, Paths, Result};

use crate::mcp::client::McpClient;
use crate::mcp::provider::McpToolWrapper;
use crate::mcp::search::McpSearchTool;
use crate::ToolRegistry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolTarget {
    pub server_name: String,
    pub tool_name: String,
}

impl McpToolTarget {
    pub fn parse(qualified_tool_name: &str) -> Option<Self> {
        let (server_name, tool_name) = qualified_tool_name.split_once("__")?;
        let server_name = server_name.trim();
        let tool_name = tool_name.trim();
        if server_name.is_empty() || tool_name.is_empty() {
            return None;
        }
        Some(Self {
            server_name: server_name.to_string(),
            tool_name: tool_name.to_string(),
        })
    }
}

pub fn mcp_access_allows(
    allowed_servers: &[String],
    allowed_tools: &[String],
    qualified_tool_name: &str,
) -> bool {
    if allowed_tools.iter().any(|tool| tool == qualified_tool_name) {
        return true;
    }

    let Some(target) = McpToolTarget::parse(qualified_tool_name) else {
        return false;
    };

    allowed_servers
        .iter()
        .any(|server_name| server_name == &target.server_name)
}

pub struct McpManager {
    resolved: McpResolvedConfig,
    clients: Mutex<HashMap<String, Arc<McpClient>>>,
}

impl McpManager {
    pub const PROGRESSIVE_DISCOVERY_THRESHOLD: usize = 20;

    pub async fn load(paths: &Paths) -> Result<Self> {
        let resolved = McpResolvedConfig::load_merged(paths)?;
        let manager = Self {
            resolved,
            clients: Mutex::new(HashMap::new()),
        };
        manager.start_auto_start_servers().await;
        Ok(manager)
    }

    pub fn resolved_config(&self) -> &McpResolvedConfig {
        &self.resolved
    }

    pub fn enabled_server_names(&self) -> Vec<String> {
        self.resolved
            .servers
            .iter()
            .filter_map(|(name, cfg)| cfg.enabled.then_some(name.clone()))
            .collect()
    }

    async fn start_auto_start_servers(&self) {
        for server_name in self.enabled_server_names() {
            let Some(server_cfg) = self.resolved.servers.get(&server_name) else {
                continue;
            };
            if !server_cfg.auto_start {
                continue;
            }
            if let Err(error) = self.client_for(&server_name).await {
                warn!(server = %server_name, error = %error, "Failed to auto-start MCP server");
            }
        }
    }

    pub async fn client_for(&self, server_name: &str) -> Result<Arc<McpClient>> {
        // 第一遍检查（无锁快速路径）
        {
            let clients = self.clients.lock().await;
            if let Some(client) = clients.get(server_name) {
                return Ok(client.clone());
            }
        }

        let server_cfg =
            self.resolved.servers.get(server_name).ok_or_else(|| {
                Error::NotFound(format!("MCP server '{}' not found", server_name))
            })?;

        if !server_cfg.enabled {
            return Err(Error::Config(format!(
                "MCP server '{}' is disabled",
                server_name
            )));
        }

        // 双重检查锁定：再次获取锁并检查，防止在第一次检查和启动之间被其他线程创建
        {
            let clients = self.clients.lock().await;
            if let Some(client) = clients.get(server_name) {
                return Ok(client.clone());
            }
        }

        info!(server = %server_name, command = %server_cfg.command, "Starting MCP server");
        let client = Arc::new(
            McpClient::start(
                server_name,
                &server_cfg.command,
                &server_cfg.args,
                &server_cfg.env,
                server_cfg.cwd.as_deref(),
                std::time::Duration::from_secs(server_cfg.startup_timeout_secs),
                std::time::Duration::from_secs(server_cfg.call_timeout_secs),
            )
            .await?,
        );

        // 第三次检查（插入时原子操作）
        let mut clients = self.clients.lock().await;
        Ok(clients
            .entry(server_name.to_string())
            .or_insert_with(|| client.clone())
            .clone())
    }

    pub async fn extend_registry_all(&self, registry: &mut ToolRegistry) -> Result<()> {
        let allowed_servers = self.enabled_server_names();
        self.extend_registry_for_rules(registry, &allowed_servers, &[])
            .await
    }

    pub async fn extend_registry_for_rules(
        &self,
        registry: &mut ToolRegistry,
        allowed_servers: &[String],
        allowed_tools: &[String],
    ) -> Result<()> {
        let mut allowed_mcp_tools = Vec::new();

        for server_name in self.enabled_server_names() {
            let client = match self.client_for(&server_name).await {
                Ok(client) => client,
                Err(error) => {
                    warn!(server = %server_name, error = %error, "Skipping unavailable MCP server");
                    continue;
                }
            };

            for tool in client.list_tools().await {
                let qualified_tool_name = format!("{}__{}", server_name, tool.name);
                if mcp_access_allows(allowed_servers, allowed_tools, &qualified_tool_name) {
                    allowed_mcp_tools.push((server_name.clone(), tool, client.clone()));
                }
            }
        }

        if allowed_mcp_tools.len() > Self::PROGRESSIVE_DISCOVERY_THRESHOLD {
            let search_index = allowed_mcp_tools
                .iter()
                .map(|(server_name, tool, _)| (server_name.clone(), tool.clone()))
                .collect();
            registry.register(Arc::new(McpSearchTool::new(search_index)));

            for (server_name, tool, client) in allowed_mcp_tools {
                registry.register_mcp(
                    Arc::new(McpToolWrapper::new(&server_name, tool, client)),
                    true,
                );
            }
        } else {
            for (server_name, tool, client) in allowed_mcp_tools {
                registry.register_mcp(
                    Arc::new(McpToolWrapper::new(&server_name, tool, client)),
                    false,
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::mcp::client::McpTool;
    use crate::mcp::search::McpSearchTool;
    use serde_json::json;

    fn mcp_tool(name: &str, description: &str) -> McpTool {
        McpTool {
            name: name.to_string(),
            description: Some(description.to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "q": { "type": "string" }
                },
                "required": ["q"]
            }),
        }
    }

    #[tokio::test]
    async fn mcp_search_tool_returns_ranked_full_tool_definitions() {
        let search = McpSearchTool::new(vec![
            (
                "postgres".to_string(),
                mcp_tool("query", "Run SQL database queries"),
            ),
            (
                "github".to_string(),
                mcp_tool("create_issue", "Create GitHub issues"),
            ),
        ]);

        let result = search.search_value("database query", 5);

        let tools = result["tools"]
            .as_array()
            .expect("tools should be an array");
        assert_eq!(tools[0]["name"], "postgres__query");
        assert_eq!(tools[0]["server"], "postgres");
        assert_eq!(tools[0]["schema"]["function"]["name"], "postgres__query");
        assert_eq!(
            tools[0]["schema"]["function"]["parameters"]["required"][0],
            "q"
        );
    }
}
