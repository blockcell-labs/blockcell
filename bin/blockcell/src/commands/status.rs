use blockcell_core::{Config, Paths};

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
        let status = if name == "ollama" && !provider_ready(&config, name, provider.api_key.as_str()) {
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

    // Channels
    println!();
    println!("Channels:");
    println!(
        "  telegram:  {}",
        if config.channels.telegram.enabled && !config.channels.telegram.token.is_empty() {
            "✓ enabled"
        } else if !config.channels.telegram.token.is_empty() {
            "configured (disabled)"
        } else {
            "✗ not configured"
        }
    );
    println!(
        "  whatsapp:  {}",
        if config.channels.whatsapp.enabled {
            format!("✓ enabled ({})", config.channels.whatsapp.bridge_url)
        } else {
            "disabled".to_string()
        }
    );
    println!(
        "  feishu:    {}",
        if config.channels.feishu.enabled && !config.channels.feishu.app_id.is_empty() {
            "✓ enabled"
        } else {
            "✗ not configured"
        }
    );
    println!(
        "  slack:     {}",
        if config.channels.slack.enabled && !config.channels.slack.bot_token.is_empty() {
            format!(
                "✓ enabled ({} channels)",
                config.channels.slack.channels.len()
            )
        } else if !config.channels.slack.bot_token.is_empty() {
            "configured (disabled)".to_string()
        } else {
            "✗ not configured".to_string()
        }
    );
    println!(
        "  discord:   {}",
        if config.channels.discord.enabled && !config.channels.discord.bot_token.is_empty() {
            "✓ enabled"
        } else if !config.channels.discord.bot_token.is_empty() {
            "configured (disabled)"
        } else {
            "✗ not configured"
        }
    );
    println!(
        "  dingtalk:  {}",
        if config.channels.dingtalk.enabled && !config.channels.dingtalk.app_key.is_empty() {
            "✓ enabled"
        } else if !config.channels.dingtalk.app_key.is_empty() {
            "configured (disabled)"
        } else {
            "✗ not configured"
        }
    );
    println!(
        "  wecom:     {}",
        if config.channels.wecom.enabled && !config.channels.wecom.corp_id.is_empty() {
            format!("✓ enabled (agent_id: {})", config.channels.wecom.agent_id)
        } else if !config.channels.wecom.corp_id.is_empty() {
            "configured (disabled)".to_string()
        } else {
            "✗ not configured".to_string()
        }
    );
    println!(
        "  lark:      {}",
        if config.channels.lark.enabled && !config.channels.lark.app_id.is_empty() {
            "✓ enabled (webhook: POST /webhook/lark)"
        } else if !config.channels.lark.app_id.is_empty() {
            "configured (disabled)"
        } else {
            "✗ not configured"
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
