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
        if config_exists { "✓" } else { "✗ (not found)" }
    );

    // Workspace
    let workspace_path = paths.workspace();
    let workspace_exists = workspace_path.exists();
    println!(
        "Workspace: {} {}",
        workspace_path.display(),
        if workspace_exists { "✓" } else { "✗ (not found)" }
    );

    if !config_exists {
        println!();
        println!("Run `blockcell onboard` to initialize.");
        return Ok(());
    }

    let config = Config::load(&config_path)?;

    // Model
    println!("Model:     {}", config.agents.defaults.model);
    println!();

    // Providers
    println!("Providers:");
    let provider_names = [
        "openrouter",
        "anthropic",
        "openai",
        "deepseek",
        "gemini",
        "groq",
        "zhipu",
        "vllm",
    ];

    for name in provider_names {
        let status = if let Some(provider) = config.providers.get(name) {
            if !provider.api_key.is_empty() {
                "✓ configured"
            } else {
                "✗ no key"
            }
        } else {
            "✗ not found"
        };
        println!("  {:<12} {}", name, status);
    }

    // Active provider
    if let Some((name, _)) = config.get_api_key() {
        println!();
        println!("Active provider: {}", name);
    } else {
        println!();
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
            format!("✓ enabled ({} channels)", config.channels.slack.channels.len())
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
