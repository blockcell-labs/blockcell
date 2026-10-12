use std::sync::Arc;

use blockcell_core::{Config, Result};

use crate::mcp::manager::McpManager;
use crate::ToolRegistry;

pub async fn build_tool_registry_for_agent_config(
    config: &Config,
    mcp_manager: Option<&Arc<McpManager>>,
) -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::with_defaults();
    if let Some(manager) = mcp_manager {
        manager
            .extend_registry_for_rules(
                &mut registry,
                &config.agents.defaults.allowed_mcp_servers,
                &config.agents.defaults.allowed_mcp_tools,
            )
            .await?;
    }
    registry.set_presentation_overrides(config.agents.defaults.tool_presentation.clone());
    Ok(registry)
}

pub async fn build_tool_registry_with_all_mcp(
    mcp_manager: Option<&Arc<McpManager>>,
) -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::with_defaults();
    if let Some(manager) = mcp_manager {
        manager.extend_registry_all(&mut registry).await?;
    }
    Ok(registry)
}
