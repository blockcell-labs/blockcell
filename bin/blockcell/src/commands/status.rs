use blockcell_agent::intent::IntentToolResolver;
use blockcell_channels::account::{channel_configured, listener_labels};
use blockcell_core::{Config, Paths};
use blockcell_tools::ToolRegistry;

fn agent_owner_bindings(config: &Config, agent_id: &str) -> Vec<String> {
    let mut owners: Vec<String> = config
        .channel_owners
        .iter()
        .filter(|(_, owner)| owner.trim() == agent_id)
        .map(|(channel, _)| channel.clone())
        .collect();

    owners.extend(config.channel_account_owners.iter().flat_map(|(channel, bindings)| {
        bindings.iter().filter_map(move |(account_id, owner)| {
            (owner.trim() == agent_id).then(|| format!("{}:{}", channel, account_id))
        })
    }));

    owners.sort();
    owners
}

fn channel_account_owner_suffix(config: &Config, channel: &str) -> String {
    let entries = config
        .channel_account_owners
        .get(channel)
        .map(|bindings| {
            let mut items = bindings
                .iter()
                .map(|(account_id, owner)| format!("{}→{}", account_id, owner))
                .collect::<Vec<_>>();
            items.sort();
            items
        })
        .unwrap_or_default();

    if entries.is_empty() {
        String::new()
    } else {
        format!("; account owners: {}", entries.join(", "))
    }
}

pub async fn run() -> anyhow::Result<()> {
    let paths = Paths::new();

    println!("blockcell status");
    println!("===============");
    println!();

    // Config
    let config_path = paths.config_file();
    let config_exists = config_path.exists();
    println!(
        "Config:    {} {}",
        config_path.display(),
        if config_exists {
            "✓"
        } else {
            "✗ (not found)"
        }
    );

    // Workspace
    let workspace_path = paths.workspace();
    let workspace_exists = workspace_path.exists();
    println!(
        "Workspace: {} {}",
        workspace_path.display(),
        if workspace_exists {
            "✓"
        } else {
            "✗ (not found)"
        }
    );

    if !config_exists {
        println!();
        println!("Run `blockcell onboard` to initialize.");
        return Ok(());
    }

    let config = Config::load(&config_path)?;

    let pool_primary = primary_pool_entry(&config);
    let model_display = pool_primary
        .map(|e| format!("{} (modelPool)", e.model))
        .unwrap_or_else(|| config.agents.defaults.model.clone());
    let active_provider =
        pool_primary
            .map(|e| e.provider.as_str())
            .or(config.agents.defaults.provider.as_deref());

    // Model
    println!("Model:     {}", model_display);
    println!();

    // Providers
    println!("Providers:");
    let mut provider_names: Vec<&str> = config.providers.keys().map(|k| k.as_str()).collect();
    provider_names.sort_unstable();

    for name in provider_names {
        let provider = &config.providers[name];
        let selected = active_provider == Some(name);
        let marker = if selected { "*" } else { " " };
        let status =
            if name == "ollama" && !provider_ready(&config, name, provider.api_key.as_str()) {
                "not selected"
            } else if provider_ready(&config, name, provider.api_key.as_str()) {
                "✓ configured"
            } else {
                "✗ no key"
            };
        println!("{} {:<12} {}", marker, name, status);
    }

    // Active provider
    println!();
    if let Some(entry) = pool_primary {
        let name = entry.provider.as_str();
        if let Some(provider) = config.providers.get(name) {
            if provider_ready(&config, name, provider.api_key.as_str()) {
                println!(
                    "Active provider: {} (from modelPool, model: {})",
                    name, entry.model
                );
            } else {
                println!(
                    "⚠ Active provider '{}' is referenced by modelPool (model: {}), but credentials are incomplete",
                    name, entry.model
                );
            }
        } else {
            println!(
                "⚠ Active provider '{}' is referenced by modelPool (model: {}), but not found in providers",
                name, entry.model
            );
        }
    } else if let Some(name) = config.agents.defaults.provider.as_deref() {
        if let Some(provider) = config.providers.get(name) {
            if provider_ready(&config, name, provider.api_key.as_str()) {
                println!("Active provider: {} (from agents.defaults.provider)", name);
            } else {
                println!(
                    "⚠ Active provider '{}' is configured in agents.defaults.provider, but credentials are incomplete",
                    name
                );
            }
        } else {
            println!(
                "⚠ Active provider '{}' is configured in agents.defaults.provider, but not found in providers",
                name
            );
        }
    } else if let Some((name, _)) = config.get_api_key() {
        println!("Active provider: {} (auto-selected)", name);
    } else {
        println!("⚠ No provider configured with API key");
    }

    println!();
    println!("Intent Router:");
    match config.intent_router.as_ref() {
        Some(router) if router.enabled => {
            println!("  status:    ✓ enabled");
            println!("  default:   {}", router.default_profile);

            let default_profile = config
                .resolve_intent_profile_id(Some("default"))
                .unwrap_or_else(|| router.default_profile.clone());
            println!("  agent default -> {}", default_profile);

            for agent in config.resolved_agents() {
                let profile = agent
                    .intent_profile
                    .clone()
                    .unwrap_or_else(|| router.default_profile.clone());
                println!("  agent {} -> {}", agent.id, profile);
            }

            let registry = ToolRegistry::with_defaults();
            match IntentToolResolver::new(&config).validate(&registry) {
                Ok(_) => println!("  validate:  ✓ builtin tools ok"),
                Err(err) => println!("  validate:  ✗ {}", err),
            }
        }
        Some(_) => println!("  status:    disabled (uses Unknown profile toolset)"),
        None => println!("  status:    defaulted from built-in config"),
    }

    println!();
    println!("Resolved Agents:");
    for agent in config.resolved_agents() {
        let agent_paths = paths.for_agent(&agent.id);
        let (provider, model, source) = resolved_agent_active_provider_and_model(&config, &agent);
        let mut owners = agent_owner_bindings(&config, &agent.id);
        if agent.id == "default" {
            owners.insert(
                0,
                "internal(cli/ws/system/cron/subagent/ghost/heartbeat)".to_string(),
            );
        }
        println!("  {}:", agent.id);
        println!("    root:     {}", agent_paths.base.display());
        println!(
            "    profile:  {}",
            agent.intent_profile.as_deref().unwrap_or("-")
        );
        println!("    model:    {} ({})", model, source);
        println!("    provider: {}", provider);
        println!(
            "    owners:   {}",
            if owners.is_empty() {
                "-".to_string()
            } else {
                owners.join(", ")
            }
        );
    }

    // Channels
    println!();
    println!("Channels:");
    let owner_suffix = |channel: &str, enabled: bool| -> String {
        if !enabled {
            return String::new();
        }
        let account_suffix = channel_account_owner_suffix(&config, channel);
        match config.resolve_channel_owner(channel) {
            Some(owner) => format!(" (owner: {}{})", owner, account_suffix),
            None if !account_suffix.is_empty() => format!(" ({} )", account_suffix.trim_start_matches(';').trim()),
            None => " ⚠ owner not set".to_string(),
        }
    };
    println!(
        "  telegram:  {}",
        if config.channels.telegram.enabled && channel_configured(&config, "telegram") {
            format!(
                "✓ enabled{}{}",
                owner_suffix("telegram", config.channels.telegram.enabled),
                channel_listener_suffix(&config, "telegram")
            )
        } else if channel_configured(&config, "telegram") {
            "configured (disabled)".to_string()
        } else {
            "✗ not configured".to_string()
        }
    );
    println!(
        "  whatsapp:  {}",
        if config.channels.whatsapp.enabled {
            format!(
                "✓ enabled ({}){}{}",
                config.channels.whatsapp.bridge_url,
                owner_suffix("whatsapp", config.channels.whatsapp.enabled),
                channel_listener_suffix(&config, "whatsapp")
            )
        } else {
            "disabled".to_string()
        }
    );
    println!(
        "  feishu:    {}",
        if config.channels.feishu.enabled && channel_configured(&config, "feishu") {
            format!(
                "✓ enabled{}{}",
                owner_suffix("feishu", config.channels.feishu.enabled),
                channel_listener_suffix(&config, "feishu")
            )
        } else {
            "✗ not configured".to_string()
        }
    );
    println!(
        "  slack:     {}",
        if config.channels.slack.enabled && channel_configured(&config, "slack") {
            format!(
                "✓ enabled ({} channels){}{}",
                config.channels.slack.channels.len(),
                owner_suffix("slack", config.channels.slack.enabled),
                channel_listener_suffix(&config, "slack")
            )
        } else if channel_configured(&config, "slack") {
            "configured (disabled)".to_string()
        } else {
            "✗ not configured".to_string()
        }
    );
    println!(
        "  discord:   {}",
        if config.channels.discord.enabled && channel_configured(&config, "discord") {
            format!(
                "✓ enabled{}{}",
                owner_suffix("discord", config.channels.discord.enabled),
                channel_listener_suffix(&config, "discord")
            )
        } else if channel_configured(&config, "discord") {
            "configured (disabled)".to_string()
        } else {
            "✗ not configured".to_string()
        }
    );
    println!(
        "  dingtalk:  {}",
        if config.channels.dingtalk.enabled && channel_configured(&config, "dingtalk") {
            format!(
                "✓ enabled{}{}",
                owner_suffix("dingtalk", config.channels.dingtalk.enabled),
                channel_listener_suffix(&config, "dingtalk")
            )
        } else if channel_configured(&config, "dingtalk") {
            "configured (disabled)".to_string()
        } else {
            "✗ not configured".to_string()
        }
    );
    println!(
        "  wecom:     {}",
        if config.channels.wecom.enabled && channel_configured(&config, "wecom") {
            format!(
                "✓ enabled (agent_id: {}){}",
                config.channels.wecom.agent_id,
                owner_suffix("wecom", config.channels.wecom.enabled)
            )
        } else if channel_configured(&config, "wecom") {
            "configured (disabled)".to_string()
        } else {
            "✗ not configured".to_string()
        }
    );
    println!(
        "  lark:      {}",
        if config.channels.lark.enabled && channel_configured(&config, "lark") {
            format!(
                "✓ enabled (webhook: POST /webhook/lark){}",
                owner_suffix("lark", config.channels.lark.enabled)
            )
        } else if channel_configured(&config, "lark") {
            "configured (disabled)".to_string()
        } else {
            "✗ not configured".to_string()
        }
    );

    Ok(())
}

fn provider_ready(config: &Config, name: &str, api_key: &str) -> bool {
    // ollama has a built-in default entry, so consider it configured only when
    // actually selected by modelPool or legacy single-model fields.
    if name == "ollama" {
        let in_pool = config
            .agents
            .defaults
            .model_pool
            .iter()
            .any(|e| e.provider == "ollama");
        let selected_by_legacy = config.agents.defaults.provider.as_deref() == Some("ollama")
            && !config.agents.defaults.model.trim().is_empty();
        return in_pool || selected_by_legacy;
    }
    let key = api_key.trim();
    !key.is_empty() && key != "dummy"
}

fn resolved_agent_active_provider_and_model(
    config: &Config,
    agent: &blockcell_core::config::ResolvedAgentConfig,
) -> (String, String, &'static str) {
    if let Some(entry) = agent
        .defaults
        .model_pool
        .iter()
        .min_by(|a, b| a.priority.cmp(&b.priority).then(b.weight.cmp(&a.weight)))
    {
        return (entry.provider.clone(), entry.model.clone(), "modelPool");
    }

    if let Some(provider) = agent.defaults.provider.clone() {
        return (provider, agent.defaults.model.clone(), "agent/defaults");
    }

    if let Some((provider, _)) = config.get_api_key() {
        return (
            provider.to_string(),
            agent.defaults.model.clone(),
            "auto-selected",
        );
    }

    ("-".to_string(), agent.defaults.model.clone(), "unresolved")
}

fn channel_listener_suffix(config: &Config, channel: &str) -> String {
    let listeners = listener_labels(config, channel);
    if listeners.is_empty() {
        return String::new();
    }
    format!(" [listeners: {}]", listeners.join(", "))
}

fn primary_pool_entry(config: &Config) -> Option<&blockcell_core::config::ModelEntry> {
    config
        .agents
        .defaults
        .model_pool
        .iter()
        .min_by(|a, b| a.priority.cmp(&b.priority).then(b.weight.cmp(&a.weight)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_listener_suffix_formats_summary() {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.accounts.insert(
            "main".to_string(),
            blockcell_core::config::TelegramAccountConfig {
                enabled: true,
                token: "tg-main".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );
        config.channels.telegram.accounts.insert(
            "ops".to_string(),
            blockcell_core::config::TelegramAccountConfig {
                enabled: true,
                token: "tg-ops".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );

        assert_eq!(
            channel_listener_suffix(&config, "telegram"),
            " [listeners: telegram:main, telegram:ops]"
        );
    }

    #[test]
    fn test_ollama_not_marked_configured_when_not_selected() {
        let mut config = Config::default();
        config
            .providers
            .get_mut("deepseek")
            .expect("deepseek provider should exist")
            .api_key = "sk-test".to_string();
        config.agents.defaults.model_pool = vec![blockcell_core::config::ModelEntry {
            model: "deepseek-chat".to_string(),
            provider: "deepseek".to_string(),
            weight: 1,
            priority: 1,
            input_price: None,
            output_price: None,
        }];
        config.agents.defaults.provider = Some("deepseek".to_string());
        config.agents.defaults.model = "deepseek-chat".to_string();

        let ollama_key = config
            .providers
            .get("ollama")
            .expect("ollama provider should exist")
            .api_key
            .clone();

        assert!(!provider_ready(&config, "ollama", &ollama_key));
        assert!(provider_ready(&config, "deepseek", "sk-test"));
    }

    #[test]
    fn test_ollama_marked_configured_when_selected_in_pool() {
        let mut config = Config::default();
        config.agents.defaults.model_pool = vec![blockcell_core::config::ModelEntry {
            model: "llama3".to_string(),
            provider: "ollama".to_string(),
            weight: 1,
            priority: 1,
            input_price: None,
            output_price: None,
        }];
        config.agents.defaults.provider = Some("ollama".to_string());
        config.agents.defaults.model = "llama3".to_string();

        let ollama_key = config
            .providers
            .get("ollama")
            .expect("ollama provider should exist")
            .api_key
            .clone();
        assert!(provider_ready(&config, "ollama", &ollama_key));
    }
}
