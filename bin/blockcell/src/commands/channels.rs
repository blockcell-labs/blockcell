use blockcell_channels::ChannelManager;
use blockcell_core::{Config, Paths};
use qrcode::render::unicode;
use qrcode::QrCode;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const SUPPORTED_OWNER_CHANNELS: [&str; 11] = [
    "telegram", "whatsapp", "feishu", "slack", "discord", "dingtalk", "wecom", "lark", "qq", "napcat",
    "weixin",
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
        "napcat" => config.channels.napcat.accounts.keys().cloned().collect::<Vec<_>>(),
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
        "weixin" => login_weixin().await?,
        _ => {
            println!("Login not supported for channel: {}", channel);
            println!("Supported channels: whatsapp, weixin");
        }
    }

    Ok(())
}

async fn login_weixin() -> anyhow::Result<()> {
    let paths = Paths::new();
    let mut config = Config::load_or_default(&paths)?;

    if !config.channels.weixin.token.trim().is_empty() {
        println!("当前已存在 Weixin token；本次扫码会覆盖并重新绑定。\n");
    }

    const MAX_REFRESHES: u32 = 3;
    const TOTAL_TIMEOUT_SECS: u64 = 300;

    let mut refresh_count = 0u32;

    loop {
        let qr = blockcell_channels::weixin::fetch_login_qrcode(&config)
            .await
            .map_err(anyhow::Error::msg)?;

        println!("\nWeixin 扫码登录");
        println!("================");
        println!("请使用微信扫码下方二维码完成登录：\n");
        render_weixin_qr(&qr.qrcode_img_content)?;
        println!("\n如果终端二维码不可读，也可以复制这个内容：");
        println!("{}\n", qr.qrcode_img_content);
        println!("开始轮询扫码状态，最多等待 5 分钟...\n");

        let started_at = Instant::now();
        loop {
            if started_at.elapsed() >= Duration::from_secs(TOTAL_TIMEOUT_SECS) {
                println!("二维码已超时。\n");
                break;
            }

            let status = blockcell_channels::weixin::poll_login_status(&config, &qr.qrcode)
                .await
                .map_err(anyhow::Error::msg)?;

            match status.status.as_str() {
                "wait" => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                "scaned" => {
                    println!("已扫码，请在手机上确认登录...");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                "confirmed" => {
                    let bot_token = status.bot_token.clone().ok_or_else(|| {
                        anyhow::anyhow!("Weixin 登录成功，但响应里没有 bot_token")
                    })?;

                    config.channels.weixin.enabled = true;
                    config.channels.weixin.token = bot_token;
                    config.save(&paths.config_file())?;

                    println!("\n✓ Weixin 登录成功，token 已保存到：{}", paths.config_file().display());
                    println!("  {}", paths.config_file().display());
                    if let Some(bot_id) = status.ilink_bot_id {
                        println!("  bot_id: {}", bot_id);
                    }
                    if let Some(user_id) = status.ilink_user_id {
                        println!("  user_id: {}", user_id);
                    }
                    if let Some(baseurl) = status.baseurl {
                        println!("  baseurl: {}", baseurl);
                    }
                    return Ok(());
                }
                "expired" => {
                    println!("二维码已过期，准备刷新...");
                    break;
                }
                other => {
                    println!("当前状态：{}", other);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }

        refresh_count += 1;
        if refresh_count >= MAX_REFRESHES {
            anyhow::bail!("Weixin 二维码已过期次数过多，请稍后重试");
        }

        println!("正在重新获取二维码（{}/{}）...\n", refresh_count, MAX_REFRESHES);
    }
}

fn render_weixin_qr(content: &str) -> anyhow::Result<()> {
    let code = QrCode::new(content.as_bytes())?;
    let rendered = code
        .render::<unicode::Dense1x2>()
        .quiet_zone(true)
        .build();
    println!("{}", rendered);
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
