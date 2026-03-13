use blockcell_channels::ChannelManager;
use blockcell_core::{Config, Paths};
use tokio::sync::mpsc;

const SUPPORTED_OWNER_CHANNELS: [&str; 9] = [
    "telegram", "whatsapp", "feishu", "slack", "discord", "dingtalk", "wecom", "lark", "qq",
];

fn known_account_ids(config: &Config, channel: &str) -> Vec<String> {
    let mut ids = match channel {
        "telegram" => config.channels.telegram.accounts.keys().cloned().collect::<Vec<_>>(),
        "whatsapp" => config.channels.whatsapp.accounts.keys().cloned().collect::<Vec<_>>(),
        "feishu" => config.channels.feishu.accounts.keys().cloned().collect::<Vec<_>>(),
        "slack" => config.channels.slack.accounts.keys().cloned().collect::<Vec<_>>(),
        "discord" => config.channels.discord.accounts.keys().cloned().collect::<Vec<_>>(),
        "dingtalk" => config.channels.dingtalk.accounts.keys().cloned().collect::<Vec<_>>(),
        "wecom" => config.channels.wecom.accounts.keys().cloned().collect::<Vec<_>>(),
        "lark" => config.channels.lark.accounts.keys().cloned().collect::<Vec<_>>(),
        "qq" => config.channels.qq.accounts.keys().cloned().collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    ids.sort();
    ids
}

pub async fn status() -> anyhow::Result<()> {
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;

    let (tx, _rx) = mpsc::channel(1);
    let manager = ChannelManager::new(config, paths, tx);

    println!("Channel Status");
    println!("==============");
    println!();

    for (name, enabled, info) in manager.get_status() {
        let status = if enabled { "✓" } else { "✗" };
        println!("{} {:<10} {}", status, name, info);
    }

    Ok(())
}

pub async fn login(channel: &str) -> anyhow::Result<()> {
    match channel {
        "whatsapp" => {
            println!("WhatsApp login:");
            println!("  1. Ensure the WhatsApp bridge is running");
            println!("  2. The bridge will display a QR code");
            println!("  3. Scan the QR code with WhatsApp on your phone");
            println!();
            println!("To start the bridge manually:");
            println!("  cd ~/.blockcell/bridge && npm start");
        }
        _ => {
            println!("Login not supported for channel: {}", channel);
            println!("Supported channels: whatsapp");
        }
    }

    Ok(())
}

pub async fn owner_list() -> anyhow::Result<()> {
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;

    println!("Channel Owners");
    println!("==============");
    println!();

    for channel in SUPPORTED_OWNER_CHANNELS {
        let owner = config
            .resolve_channel_owner(channel)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "-".to_string());
        let enabled = config.is_external_channel_enabled(channel);
        let mark = if enabled { "✓" } else { " " };
        println!("{} {:<10} {}", mark, channel, owner);

        if let Some(account_owners) = config.channel_account_owners.get(channel) {
            let mut entries = account_owners.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            for (account_id, agent) in entries {
                println!("    - {:<14} {}", account_id, agent);
            }
        }
    }
    Ok(())
}

pub async fn owner_set(channel: &str, account: Option<&str>, agent: &str) -> anyhow::Result<()> {
    let paths = Paths::new();
    let mut config = Config::load_or_default(&paths)?;
    if !SUPPORTED_OWNER_CHANNELS.contains(&channel) {
        anyhow::bail!("Unsupported channel '{}'", channel);
    }
    if !config.agent_exists(agent) {
        anyhow::bail!(
            "Agent '{}' does not exist. Add it under agents.list or use 'default'.",
            agent
        );
    }

    if let Some(account_id) = account.map(str::trim).filter(|value| !value.is_empty()) {
        let known_accounts = known_account_ids(&config, channel);
        if !known_accounts.iter().any(|id| id == account_id) {
            anyhow::bail!(
                "Account '{}' is not defined under channels.{}.accounts.",
                account_id,
                channel
            );
        }
        config
            .channel_account_owners
            .entry(channel.to_string())
            .or_default()
            .insert(account_id.to_string(), agent.to_string());
        config.save(&paths.config_file())?;
        println!("✓ owner set: {}:{} -> {}", channel, account_id, agent);
    } else {
        config
            .channel_owners
            .insert(channel.to_string(), agent.to_string());
        config.save(&paths.config_file())?;
        println!("✓ owner set: {} -> {}", channel, agent);
    }
    Ok(())
}

pub async fn owner_clear(channel: &str, account: Option<&str>) -> anyhow::Result<()> {
    let paths = Paths::new();
    let mut config = Config::load_or_default(&paths)?;
    if let Some(account_id) = account.map(str::trim).filter(|value| !value.is_empty()) {
        if let Some(bindings) = config.channel_account_owners.get_mut(channel) {
            bindings.remove(account_id);
            if bindings.is_empty() {
                config.channel_account_owners.remove(channel);
            }
        }
        config.save(&paths.config_file())?;
        println!("✓ owner cleared: {}:{}", channel, account_id);
    } else {
        config.channel_owners.remove(channel);
        config.save(&paths.config_file())?;
        println!("✓ owner cleared: {}", channel);
    }
    Ok(())
}
