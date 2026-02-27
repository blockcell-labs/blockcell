use blockcell_core::{Config, Paths};
use blockcell_tools::ToolRegistry;
use serde_json::Value;

/// Run a direct tool call, bypassing the LLM.
pub async fn tool(tool_name: &str, params_json: &str) -> anyhow::Result<()> {
    let registry = ToolRegistry::with_defaults();
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;

    let tool = registry.get(tool_name)
        .ok_or_else(|| anyhow::anyhow!("Tool '{}' not found. Use `blockcell tools list` to see available tools.", tool_name))?;

    let params: Value = serde_json::from_str(params_json)
        .map_err(|e| anyhow::anyhow!("Failed to parse JSON params: {}\nInput: {}", e, params_json))?;

    if let Err(e) = tool.validate(&params) {
        anyhow::bail!("Parameter validation failed: {}\nUse `blockcell tools info {}` for parameter details.", e, tool_name);
    }

    let ctx = blockcell_tools::ToolContext {
        workspace: paths.workspace(),
        builtin_skills_dir: Some(paths.builtin_skills_dir()),
        config,
        session_key: "cli:run".to_string(),
        channel: String::new(),
        chat_id: String::new(),
        permissions: blockcell_core::types::PermissionSet::new(),
        outbound_tx: None,
        spawn_handle: None,
        task_manager: None,
        memory_store: None,
        capability_registry: None,
        core_evolution: None,
    };

    let result: serde_json::Value = tool.execute(ctx, params).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

/// Run a message through the agent (shortcut for `agent -m`).
pub async fn message(msg: &str, session: &str) -> anyhow::Result<()> {
    // Delegate to agent command with message mode
    super::agent::run(Some(msg.to_string()), session.to_string(), None, None).await
}
