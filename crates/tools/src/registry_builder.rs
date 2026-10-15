use std::sync::Arc;

use blockcell_core::{Config, Result};

use crate::mcp::manager::McpManager;
use crate::ToolRegistry;

pub async fn build_tool_registry_for_agent_config(
    config: &Config,
    mcp_manager: Option<&Arc<McpManager>>,
) -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::with_defaults();
    registry.register(Arc::new(crate::exec::ExecTool::from_config(
        &config.tools.exec,
    )));
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

/// 构建包含全部 MCP 工具的 registry（doctor/status/tools/gateway 等命令使用）。
/// 与 [`build_tool_registry_for_agent_config`] 保持一致：`exec` 的沙箱策略同样来自
/// `config.tools.exec`，否则展示的 execution environment / 确认声明会与运行时执行不符。
pub async fn build_tool_registry_with_all_mcp(
    config: &Config,
    mcp_manager: Option<&Arc<McpManager>>,
) -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::with_defaults();
    registry.register(Arc::new(crate::exec::ExecTool::from_config(
        &config.tools.exec,
    )));
    if let Some(manager) = mcp_manager {
        manager.extend_registry_all(&mut registry).await?;
    }
    Ok(registry)
}
