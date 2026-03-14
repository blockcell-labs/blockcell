use super::*;
// ---------------------------------------------------------------------------
// Startup banner — colored, boxed output for key information
// ---------------------------------------------------------------------------

/// ANSI color helpers
mod ansi {
    pub const RESET: &str = "\x1b[0m";
    pub const BOLD: &str = "\x1b[1m";
    pub const DIM: &str = "\x1b[2m";
    pub const GREEN: &str = "\x1b[32m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const CYAN: &str = "\x1b[36m";
    pub const WHITE: &str = "\x1b[97m";
    pub const BG_YELLOW: &str = "\x1b[43m";
    // 24-bit true-color matching the Logo.tsx palette
    pub const ORANGE: &str = "\x1b[38;2;234;88;12m"; // #ea580c
    pub const NEON_GREEN: &str = "\x1b[38;2;0;255;157m"; // #00ff9d
}

pub(super) fn rand_u32() -> u32 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut h);
    std::process::id().hash(&mut h);
    h.finish() as u32
}

#[allow(clippy::too_many_arguments)]
pub(super) fn print_startup_banner(
    config: &Config,
    host: &str,
    webui_host: &str,
    webui_port: u16,
    web_password: &str,
    webui_pass_is_temp: bool,
    is_exposed: bool,
    bind_addr: &str,
) {
    let ver = env!("CARGO_PKG_VERSION");
    let (model, provider, source) = active_model_and_provider(config);
    let model_label = if source == "modelPool" {
        match provider {
            Some(p) => format!("{} (modelPool: {})", model, p),
            None => format!("{} (modelPool)", model),
        }
    } else {
        model
    };

    // ── Logo + Header ──
    eprintln!();
    //  Layered hexagon logo — bold & colorful (matches Logo.tsx)
    let o = ansi::ORANGE;
    let g = ansi::NEON_GREEN;
    let r = ansi::RESET;

    eprintln!("           {o}▄▄▄▄▄▄▄{r}");
    eprintln!("       {o}▄█████████████▄{r}");
    eprintln!("     {o}▄████▀▀     ▀▀████▄{r}      {o}▄▄{r}");
    eprintln!("    {o}▐███▀{r}   {g}█████{r}   {o}▀███▌{r}    {o}████{r}");
    eprintln!("    {o}▐███{r}    {g}█████{r}    {o}███▌{r}     {o}▀▀{r}");
    eprintln!("    {o}▐███{r}    {g}█████{r}    {o}███▌{r}");
    eprintln!("    {o}▐███{r}    {g}█████{r}    {o}███▌{r}");
    eprintln!("    {o}▐███▄{r}   {g}▀▀▀▀▀{r}   {o}▄███▌{r}");
    eprintln!("     {o}▀████▄▄     ▄▄████▀{r}");
    eprintln!("   {o}▄▄{r}  {o}▀█████████████▀{r}");
    eprintln!("  {o}████{r}     {o}▀▀▀▀▀▀▀{r}");
    eprintln!("   {o}▀▀{r}");
    eprintln!();
    eprintln!(
        "  {}{}  BLOCKCELL GATEWAY v{}  {}",
        ansi::BOLD,
        ansi::CYAN,
        ver,
        ansi::RESET
    );
    eprintln!("  {}Model: {}{}", ansi::DIM, model_label, ansi::RESET);
    eprintln!();

    // ── WebUI Password box ──
    let box_w = 62;
    if webui_pass_is_temp {
        // Temp password — show prominently, warn it changes each restart
        eprintln!("  {}┌{}┐{}", ansi::YELLOW, "─".repeat(box_w), ansi::RESET);
        let pw_label = "🔑 WebUI Password: ";
        let pw_visible = 2 + display_width(pw_label) + web_password.len();
        let pw_pad = box_w.saturating_sub(pw_visible);
        eprintln!(
            "  {}│{}  {}{}{}{}{}{}│",
            ansi::YELLOW,
            ansi::RESET,
            ansi::BOLD,
            ansi::YELLOW,
            pw_label,
            web_password,
            ansi::RESET,
            " ".repeat(pw_pad),
        );
        let hint1 = "  Temporary — changes every restart. Set gateway.webuiPass";
        let hint1_pad = box_w.saturating_sub(hint1.len());
        eprintln!(
            "  {}│{}  {}Temporary — changes every restart. Set gateway.webuiPass{}{}{}│{}",
            ansi::YELLOW,
            ansi::RESET,
            ansi::DIM,
            ansi::RESET,
            " ".repeat(hint1_pad),
            ansi::YELLOW,
            ansi::RESET,
        );
        let hint2 = "  in config.json5 for a stable password.";
        let hint2_pad = box_w.saturating_sub(hint2.len());
        eprintln!(
            "  {}│{}  {}in config.json5 for a stable password.{}{}{}│{}",
            ansi::YELLOW,
            ansi::RESET,
            ansi::DIM,
            ansi::RESET,
            " ".repeat(hint2_pad),
            ansi::YELLOW,
            ansi::RESET,
        );
        eprintln!("  {}└{}┘{}", ansi::YELLOW, "─".repeat(box_w), ansi::RESET);
    } else {
        // Configured stable password
        eprintln!("  {}┌{}┐{}", ansi::GREEN, "─".repeat(box_w), ansi::RESET);
        let pw_label = "🔑 WebUI Password: ";
        let pw_visible = 2 + display_width(pw_label) + web_password.len();
        let pw_pad = box_w.saturating_sub(pw_visible);
        eprintln!(
            "  {}│{}  {}{}{}{}{}{}│",
            ansi::GREEN,
            ansi::RESET,
            ansi::BOLD,
            ansi::GREEN,
            pw_label,
            web_password,
            ansi::RESET,
            " ".repeat(pw_pad),
        );
        let hint = "  Configured via gateway.webuiPass in config.json5";
        let hint_pad = box_w.saturating_sub(hint.len());
        eprintln!(
            "  {}│{}  {}Configured via gateway.webuiPass in config.json5{}{}{}│{}",
            ansi::GREEN,
            ansi::RESET,
            ansi::DIM,
            ansi::RESET,
            " ".repeat(hint_pad),
            ansi::GREEN,
            ansi::RESET,
        );
        eprintln!("  {}└{}┘{}", ansi::GREEN, "─".repeat(box_w), ansi::RESET);
    }
    eprintln!();

    // ── Security warning ──
    if is_exposed && webui_pass_is_temp {
        eprintln!(
            "  {}{}⚠  SECURITY: Binding to {} with an auto-generated token.{}",
            ansi::BG_YELLOW,
            ansi::BOLD,
            host,
            ansi::RESET
        );
        eprintln!(
            "  {}   Review gateway.apiToken in config.json5 before exposing to the network.{}",
            ansi::YELLOW,
            ansi::RESET
        );
        eprintln!();
    }

    // ── Channels status ──
    eprintln!("  {}{}Channels{}", ansi::BOLD, ansi::WHITE, ansi::RESET);

    let ch = &config.channels;

    struct ChannelInfo {
        id: &'static str,
        name: &'static str,
        enabled: bool,
        configured: bool,
        detail: String,
    }

    #[derive(Clone)]
    struct ChannelRouteLine {
        text: String,
    }

    fn owner_for(config: &Config, channel: &str, account_id: Option<&str>) -> String {
        if let Some(account_id) = account_id {
            if let Some(owner) = config
                .channel_account_owners
                .get(channel)
                .and_then(|m| m.get(account_id))
                .filter(|s| !s.trim().is_empty())
            {
                return owner.clone();
            }
        }

        config
            .channel_owners
            .get(channel)
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| "default".to_string())
    }

    fn default_marker(
        default_account_id: Option<&String>,
        account_id: Option<&str>,
    ) -> &'static str {
        if default_account_id.map(|s| s.as_str()) == account_id {
            " [default]"
        } else {
            ""
        }
    }

    fn route_text(
        channel_name: &str,
        account_id: Option<&str>,
        owner: &str,
        detail: &str,
        suffix: &str,
    ) -> String {
        match account_id {
            Some(account) => format!(
                "● {}:{}{} -> agent={}{}{}",
                channel_name,
                account,
                suffix,
                owner,
                if detail.is_empty() { "" } else { "  " },
                detail
            ),
            None => format!(
                "● {} -> agent={}{}{}",
                channel_name,
                owner,
                if detail.is_empty() { "" } else { "  " },
                detail
            ),
        }
    }

    let channels = vec![
        ChannelInfo {
            id: "telegram",
            name: "Telegram",
            enabled: ch.telegram.enabled,
            configured: blockcell_channels::account::channel_configured(config, "telegram"),
            detail: if !ch.telegram.token.is_empty() {
                format!("allow_from: {:?}", ch.telegram.allow_from)
            } else {
                "no token configured".into()
            },
        },
        ChannelInfo {
            id: "slack",
            name: "Slack",
            enabled: ch.slack.enabled,
            configured: blockcell_channels::account::channel_configured(config, "slack"),
            detail: if !ch.slack.bot_token.is_empty() {
                format!("channels: {:?}", ch.slack.channels)
            } else {
                "no bot_token configured".into()
            },
        },
        ChannelInfo {
            id: "discord",
            name: "Discord",
            enabled: ch.discord.enabled,
            configured: blockcell_channels::account::channel_configured(config, "discord"),
            detail: if !ch.discord.bot_token.is_empty() {
                format!("channels: {:?}", ch.discord.channels)
            } else {
                "no bot_token configured".into()
            },
        },
        ChannelInfo {
            id: "feishu",
            name: "Feishu",
            enabled: ch.feishu.enabled,
            configured: blockcell_channels::account::channel_configured(config, "feishu"),
            detail: if !ch.feishu.app_id.is_empty() {
                format!("app_id: {}", ch.feishu.app_id)
            } else {
                "no app_id configured".into()
            },
        },
        ChannelInfo {
            id: "lark",
            name: "Lark",
            enabled: ch.lark.enabled,
            configured: blockcell_channels::account::channel_configured(config, "lark"),
            detail: if !ch.lark.app_id.is_empty() {
                format!("app_id: {}", ch.lark.app_id)
            } else {
                "no app_id configured".into()
            },
        },
        ChannelInfo {
            id: "dingtalk",
            name: "DingTalk",
            enabled: ch.dingtalk.enabled,
            configured: blockcell_channels::account::channel_configured(config, "dingtalk"),
            detail: if !ch.dingtalk.app_key.is_empty() {
                format!("robot_code: {}", ch.dingtalk.robot_code)
            } else {
                "no app_key configured".into()
            },
        },
        ChannelInfo {
            id: "wecom",
            name: "WeCom",
            enabled: ch.wecom.enabled,
            configured: blockcell_channels::account::channel_configured(config, "wecom"),
            detail: if ch.wecom.mode == "long_connection"
                || ch.wecom.mode == "long-connection"
                || ch.wecom.mode == "stream"
            {
                if !ch.wecom.bot_id.is_empty() {
                    format!("mode: {}  bot_id: {}", ch.wecom.mode, ch.wecom.bot_id)
                } else {
                    "no bot_id configured".into()
                }
            } else if !ch.wecom.corp_id.is_empty() {
                format!("mode: {}  agent_id: {}", ch.wecom.mode, ch.wecom.agent_id)
            } else {
                "no corp_id configured".into()
            },
        },
        ChannelInfo {
            id: "whatsapp",
            name: "WhatsApp",
            enabled: ch.whatsapp.enabled,
            configured: blockcell_channels::account::channel_configured(config, "whatsapp"),
            detail: if !ch.whatsapp.bridge_url.is_empty() {
                format!("bridge: {}", ch.whatsapp.bridge_url)
            } else {
                "not enabled".into()
            },
        },
        ChannelInfo {
            id: "qq",
            name: "QQ",
            enabled: ch.qq.enabled,
            configured: blockcell_channels::account::channel_configured(config, "qq"),
            detail: if !ch.qq.app_id.is_empty() {
                format!("app_id: {}  env: {}", ch.qq.app_id, if ch.qq.environment.is_empty() { "production" } else { &ch.qq.environment })
            } else {
                "no app_id configured".into()
            },
        },
    ];

    let mut enabled_routes: Vec<ChannelRouteLine> = Vec::new();
    let mut disabled: Vec<String> = Vec::new();

    for ch_info in &channels {
        let listener_labels = blockcell_channels::account::listener_labels(config, ch_info.id);
        if ch_info.enabled && ch_info.configured {
            if listener_labels.is_empty() {
                let owner = owner_for(config, ch_info.id, None);
                enabled_routes.push(ChannelRouteLine {
                    text: route_text(ch_info.name, None, &owner, &ch_info.detail, ""),
                });
            } else {
                for label in listener_labels {
                    let account_id = label.split_once(':').map(|(_, account)| account);
                    let owner = owner_for(config, ch_info.id, account_id);
                    let detail = match (ch_info.id, account_id) {
                        ("telegram", Some(account)) => config
                            .channels
                            .telegram
                            .accounts
                            .get(account)
                            .map(|acc| format!("allow_from: {:?}", acc.allow_from))
                            .unwrap_or_else(|| ch_info.detail.clone()),
                        ("slack", Some(account)) => config
                            .channels
                            .slack
                            .accounts
                            .get(account)
                            .map(|acc| format!("channels: {:?}", acc.channels))
                            .unwrap_or_else(|| ch_info.detail.clone()),
                        ("discord", Some(account)) => config
                            .channels
                            .discord
                            .accounts
                            .get(account)
                            .map(|acc| format!("channels: {:?}", acc.channels))
                            .unwrap_or_else(|| ch_info.detail.clone()),
                        ("feishu", Some(account)) => config
                            .channels
                            .feishu
                            .accounts
                            .get(account)
                            .map(|acc| format!("app_id: {}", acc.app_id))
                            .unwrap_or_else(|| ch_info.detail.clone()),
                        ("lark", Some(account)) => config
                            .channels
                            .lark
                            .accounts
                            .get(account)
                            .map(|acc| format!("app_id: {}", acc.app_id))
                            .unwrap_or_else(|| ch_info.detail.clone()),
                        ("dingtalk", Some(account)) => config
                            .channels
                            .dingtalk
                            .accounts
                            .get(account)
                            .map(|acc| format!("robot_code: {}", acc.robot_code))
                            .unwrap_or_else(|| ch_info.detail.clone()),
                        ("wecom", Some(account)) => config
                            .channels
                            .wecom
                            .accounts
                            .get(account)
                            .map(|acc| format!("agent_id: {}", acc.agent_id))
                            .unwrap_or_else(|| ch_info.detail.clone()),
                        ("whatsapp", Some(account)) => config
                            .channels
                            .whatsapp
                            .accounts
                            .get(account)
                            .map(|acc| format!("bridge: {}", acc.bridge_url))
                            .unwrap_or_else(|| ch_info.detail.clone()),
                        ("qq", Some(account)) => config
                            .channels
                            .qq
                            .accounts
                            .get(account)
                            .map(|acc| format!("app_id: {}  env: {}", acc.app_id, if acc.environment.is_empty() { "production" } else { &acc.environment }))
                            .unwrap_or_else(|| ch_info.detail.clone()),
                        _ => ch_info.detail.clone(),
                    };
                    let suffix = match ch_info.id {
                        "telegram" => default_marker(
                            config.channels.telegram.default_account_id.as_ref(),
                            account_id,
                        ),
                        "slack" => default_marker(
                            config.channels.slack.default_account_id.as_ref(),
                            account_id,
                        ),
                        "discord" => default_marker(
                            config.channels.discord.default_account_id.as_ref(),
                            account_id,
                        ),
                        "feishu" => default_marker(
                            config.channels.feishu.default_account_id.as_ref(),
                            account_id,
                        ),
                        "lark" => default_marker(
                            config.channels.lark.default_account_id.as_ref(),
                            account_id,
                        ),
                        "dingtalk" => default_marker(
                            config.channels.dingtalk.default_account_id.as_ref(),
                            account_id,
                        ),
                        "wecom" => default_marker(
                            config.channels.wecom.default_account_id.as_ref(),
                            account_id,
                        ),
                        "whatsapp" => default_marker(
                            config.channels.whatsapp.default_account_id.as_ref(),
                            account_id,
                        ),
                        "qq" => default_marker(
                            config.channels.qq.default_account_id.as_ref(),
                            account_id,
                        ),
                        _ => "",
                    };
                    enabled_routes.push(ChannelRouteLine {
                        text: route_text(ch_info.name, account_id, &owner, &detail, suffix),
                    });
                }
            }
        } else {
            disabled.push(format!("○ {}  — {}", ch_info.name, ch_info.detail));
        }
    }

    // Enabled channels box (green)
    if !enabled_routes.is_empty() {
        let content_width = enabled_routes
            .iter()
            .map(|line| display_width(&format!("  {}", line.text)))
            .max()
            .unwrap_or(0);
        let box_w = content_width.max(62);
        eprintln!("  {}┌{}┐{}", ansi::GREEN, "─".repeat(box_w), ansi::RESET);
        for line in &enabled_routes {
            let rendered = format!("  {}", line.text);
            let pad = box_w.saturating_sub(display_width(&rendered));
            eprintln!(
                "  {}│{} {}{}{}{}│{}",
                ansi::GREEN,
                ansi::RESET,
                ansi::BOLD,
                line.text,
                ansi::RESET,
                " ".repeat(pad),
                ansi::RESET,
            );
        }
        eprintln!("  {}└{}┘{}", ansi::GREEN, "─".repeat(box_w), ansi::RESET);
    }

    // Disabled/unconfigured channels (dim, no box)
    if !disabled.is_empty() {
        for line in &disabled {
            eprintln!("  {}  {}{}", ansi::DIM, line, ansi::RESET);
        }
    }

    if channels.iter().all(|c| !c.enabled) {
        eprintln!(
            "  {}  No channels enabled. WebSocket is the only input.{}",
            ansi::DIM,
            ansi::RESET,
        );
    }
    eprintln!();

    // ── Server info ──
    eprintln!("  {}{}Server{}", ansi::BOLD, ansi::WHITE, ansi::RESET);

    eprintln!(
        "  {}HTTP/WS:{}  http://{}",
        ansi::CYAN,
        ansi::RESET,
        bind_addr,
    );
    eprintln!(
        "  {}WebUI:{}   http://{}:{}/",
        ansi::CYAN,
        ansi::RESET,
        webui_host,
        webui_port,
    );

    let api_base = config
        .gateway
        .public_api_base
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| format!("http://{}", bind_addr));
    eprintln!(
        "  {}API:{}     POST {}/v1/chat  |  GET {}/v1/health  |  GET {}/v1/ws",
        ansi::CYAN,
        ansi::RESET,
        api_base,
        api_base,
        api_base,
    );
    eprintln!();

    // ── Ready ──
    eprintln!(
        "  {}{}✓ Gateway ready.{} Press {}Ctrl+C{} to stop.",
        ansi::BOLD,
        ansi::GREEN,
        ansi::RESET,
        ansi::BOLD,
        ansi::RESET,
    );
    eprintln!();
}

/// Calculate the visible display width of a string (ignoring ANSI escape codes).
/// This is a simplified version — counts ASCII printable chars.
fn display_width(s: &str) -> usize {
    let mut w = 0usize;
    let mut in_escape = false;
    for ch in s.chars() {
        if in_escape {
            if ch.is_ascii_alphabetic() {
                in_escape = false;
            }
            continue;
        }
        if ch == '\x1b' {
            in_escape = true;
            continue;
        }
        // CJK characters are typically 2 columns wide
        if ch as u32 >= 0x4E00 && ch as u32 <= 0x9FFF {
            w += 2;
        } else {
            w += 1;
        }
    }
    w
}
