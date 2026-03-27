use std::sync::Arc;

use blockcell_core::{Config, Paths};
use blockcell_tools::build_tool_registry_for_agent_config;
use blockcell_tools::mcp::manager::McpManager;
use serde_json::Value;

#[derive(Debug)]
struct ToolCliContext {
    agent_id: String,
    session_key: String,
    config: Config,
    paths: Paths,
}

fn resolve_tool_context(
    config: &Config,
    paths: &Paths,
    requested_agent: Option<&str>,
) -> anyhow::Result<ToolCliContext> {
    let agent_id = requested_agent
        .map(str::trim)
        .filter(|agent_id| !agent_id.is_empty())
        .unwrap_or("default");

    if !config.agent_exists(agent_id) {
        anyhow::bail!("Unknown agent '{}'", agent_id);
    }

    let agent_config = config
        .config_for_agent(agent_id)
        .ok_or_else(|| anyhow::anyhow!("Unknown agent '{}'", agent_id))?;
    let agent_paths = paths.for_agent(agent_id);
    let session_key = if agent_id == "default" {
        "cli:run".to_string()
    } else {
        format!("cli:run:{}", agent_id)
    };

    Ok(ToolCliContext {
        agent_id: agent_id.to_string(),
        session_key,
        config: agent_config,
        paths: agent_paths,
    })
}

/// Run a direct tool call, bypassing the LLM.
pub async fn tool(tool_name: &str, params_json: &str, agent: Option<&str>) -> anyhow::Result<()> {
    let root_paths = Paths::new();
    let root_config = Config::load_or_default(&root_paths)?;
    let resolved = resolve_tool_context(&root_config, &root_paths, agent)?;
    let mcp_manager = Arc::new(McpManager::load(&root_paths).await?);
    let registry =
        build_tool_registry_for_agent_config(&resolved.config, Some(&mcp_manager)).await?;
    let _agent_id = resolved.agent_id.clone();
    let session_key = resolved.session_key;
    let config = resolved.config;
    let paths = resolved.paths;
    paths.ensure_dirs()?;

    let tool = registry.get(tool_name).ok_or_else(|| {
        anyhow::anyhow!(
            "Tool '{}' not found. Use `blockcell tools list` to see available tools.",
            tool_name
        )
    })?;

    let params: Value = serde_json::from_str(params_json).map_err(|e| {
        anyhow::anyhow!("Failed to parse JSON params: {}\nInput: {}", e, params_json)
    })?;

    if let Err(e) = tool.validate(&params) {
        anyhow::bail!(
            "Parameter validation failed: {}\nUse `blockcell tools info {}` for parameter details.",
            e,
            tool_name
        );
    }

    let ctx = blockcell_tools::ToolContext {
        workspace: paths.workspace(),
        builtin_skills_dir: Some(paths.builtin_skills_dir()),
        active_skill_dir: None,
        config,
        session_key,
        channel: String::new(),
        account_id: None,
        sender_id: None, // CLI tool execution has no sender
        chat_id: String::new(),
        permissions: blockcell_core::types::PermissionSet::new(),
        outbound_tx: None,
        spawn_handle: None,
        task_manager: None,
        memory_store: None,
        capability_registry: None,
        core_evolution: None,
        event_emitter: None,
        channel_contacts_file: Some(paths.channel_contacts_file()),
        response_cache: None,
    };

    let result: serde_json::Value = tool.execute(ctx, params).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

/// Run a message through the agent (shortcut for `agent -m`).
pub async fn message(msg: &str, session: &str, agent: Option<&str>) -> anyhow::Result<()> {
    // Delegate to agent command with message mode
    super::agent::run(
        Some(msg.to_string()),
        agent.map(str::to_string),
        Some(session.to_string()),
        None,
        None,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::config::AgentProfileConfig;
    use std::path::PathBuf;

    #[test]
    fn test_resolve_tool_context_defaults_to_default_agent() {
        let config = Config::default();
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell"));

        let resolved = resolve_tool_context(&config, &paths, None)
            .expect("default tool context should resolve");

        assert_eq!(resolved.agent_id, "default");
        assert_eq!(resolved.session_key, "cli:run");
        assert_eq!(
            resolved.paths.workspace(),
            PathBuf::from("/tmp/blockcell/workspace")
        );
    }

    #[test]
    fn test_resolve_tool_context_uses_named_agent_paths() {
        let mut config = Config::default();
        config.agents.list.push(AgentProfileConfig {
            id: "ops".to_string(),
            enabled: true,
            provider: Some("deepseek".to_string()),
            model: Some("deepseek-chat".to_string()),
            ..AgentProfileConfig::default()
        });
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell"));

        let resolved = resolve_tool_context(&config, &paths, Some("ops"))
            .expect("named tool context should resolve");

        assert_eq!(resolved.agent_id, "ops");
        assert_eq!(resolved.session_key, "cli:run:ops");
        assert_eq!(
            resolved.paths.workspace(),
            PathBuf::from("/tmp/blockcell/agents/ops/workspace")
        );
        assert_eq!(
            resolved.config.agents.defaults.provider.as_deref(),
            Some("deepseek")
        );
    }

    #[test]
    fn test_resolve_tool_context_rejects_unknown_agent() {
        let config = Config::default();
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell"));

        let err = resolve_tool_context(&config, &paths, Some("ops"))
            .expect_err("unknown tool agent should fail");

        assert!(err.to_string().contains("Unknown agent 'ops'"));
    }
}
