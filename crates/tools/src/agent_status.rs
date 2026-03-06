use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::{PromptContext, Tool, ToolContext, ToolSchema};

pub struct AgentStatusTool;

#[async_trait]
impl Tool for AgentStatusTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "agent_status",
            description: "Inspect configured agent nodes. Use this to list all agents, view one agent's effective config summary, or summarize channel-to-agent routing.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "get", "summary", "channels"],
                        "description": "Query mode: 'list' returns all agents, 'get' returns one agent, 'summary' returns compact overview, 'channels' returns channel/account to agent bindings."
                    },
                    "agent_id": {
                        "type": "string",
                        "description": "Required for action='get'. Agent ID such as 'default', 'ops', 'worker-2'."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: action".to_string()))?;

        if action == "get"
            && params
                .get("agent_id")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().is_empty())
                .unwrap_or(true)
        {
            return Err(Error::Validation(
                "Missing required parameter for action='get': agent_id".to_string(),
            ));
        }

        Ok(())
    }

    fn prompt_rule(&self, _ctx: &PromptContext) -> Option<String> {
        Some(
            "- When the user asks about current agent nodes, node status, which agent handles which channel, or to list all configured agents, call `agent_status` instead of guessing from memory."
                .to_string(),
        )
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("summary");

        match action {
            "list" => Ok(list_agents(&ctx)),
            "get" => {
                let agent_id = params
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default")
                    .trim();
                get_agent(&ctx, agent_id)
            }
            "channels" => Ok(channel_bindings(&ctx)),
            "summary" => Ok(summary(&ctx)),
            other => Err(Error::Validation(format!(
                "Unsupported action for agent_status: {}",
                other
            ))),
        }
    }
}

fn list_agents(ctx: &ToolContext) -> Value {
    let resolved = ctx.config.resolved_agents();
    let agents = resolved
        .into_iter()
        .map(|agent| build_agent_json(ctx, &agent.id))
        .collect::<Vec<_>>();

    json!({
        "default_agent": "default",
        "count": agents.len(),
        "agents": agents,
    })
}

fn get_agent(ctx: &ToolContext, agent_id: &str) -> Result<Value> {
    if !ctx.config.agent_exists(agent_id) {
        return Err(Error::Validation(format!("Unknown agent_id: {}", agent_id)));
    }
    Ok(build_agent_json(ctx, agent_id))
}

fn summary(ctx: &ToolContext) -> Value {
    let resolved = ctx.config.resolved_agents();
    let agent_ids = resolved.iter().map(|agent| agent.id.clone()).collect::<Vec<_>>();
    let channel_map = channel_binding_map(&ctx.config);
    let channel_summary = channel_map
        .iter()
        .map(|(channel, owners)| {
            json!({
                "channel": channel,
                "routes": owners,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "agent_count": agent_ids.len(),
        "agents": agent_ids,
        "channel_bindings": channel_summary,
    })
}

fn channel_bindings(ctx: &ToolContext) -> Value {
    let channel_map = channel_binding_map(&ctx.config);
    json!({
        "channels": channel_map,
    })
}

fn build_agent_json(ctx: &ToolContext, agent_id: &str) -> Value {
    let resolved = ctx
        .config
        .resolve_agent_spec(agent_id)
        .expect("agent existence already checked");

    let mut owned_channels = Vec::new();
    let mut owned_channel_accounts = Vec::new();

    for (channel, owner) in &ctx.config.channel_owners {
        if owner == agent_id {
            owned_channels.push(channel.clone());
        }
    }

    for (channel, account_owners) in &ctx.config.channel_account_owners {
        for (account_id, owner) in account_owners {
            if owner == agent_id {
                owned_channel_accounts.push(format!("{}:{}", channel, account_id));
            }
        }
    }

    owned_channels.sort();
    owned_channel_accounts.sort();

    json!({
        "id": resolved.id,
        "name": resolved.name,
        "enabled": true,
        "intent_profile": resolved.intent_profile,
        "model": resolved.defaults.model,
        "provider": resolved.defaults.provider,
        "max_context_tokens": resolved.defaults.max_context_tokens,
        "temperature": resolved.defaults.temperature,
        "channel_owners": owned_channels,
        "channel_account_owners": owned_channel_accounts,
        "is_default": agent_id == "default",
    })
}

fn channel_binding_map(config: &blockcell_core::Config) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (channel, owner) in &config.channel_owners {
        map.entry(channel.clone())
            .or_default()
            .push(format!("{} -> {}", channel, owner));
    }

    for (channel, account_owners) in &config.channel_account_owners {
        let entry = map.entry(channel.clone()).or_default();
        let mut items = account_owners
            .iter()
            .map(|(account_id, owner)| format!("{}:{} -> {}", channel, account_id, owner))
            .collect::<Vec<_>>();
        items.sort();
        entry.extend(items);
    }

    for value in map.values_mut() {
        value.sort();
        value.dedup();
    }

    map
}
