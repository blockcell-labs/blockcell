use anyhow::{anyhow, bail, Context};
use blockcell_core::config::{ModelEntry, ToolCallMode};
use blockcell_core::{Config, Paths};
use std::io::{self, Write};

/// Interactive first-run setup wizard for provider + optional channel.
pub async fn run(
    force: bool,
    provider: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    channel: Option<String>,
    skip_provider_test: bool,
) -> anyhow::Result<()> {
    let paths = Paths::new();
    paths.ensure_dirs()?;

    let config_path = paths.config_file();
    let mut config = if config_path.exists() && !force {
        Config::load(&config_path)
            .with_context(|| format!("Failed to load {}", config_path.display()))?
    } else {
        if config_path.exists() && force {
            println!("⚠ --force enabled: reset existing config to defaults before setup.");
        }
        Config::default()
    };

    if config.intent_router.is_none() {
        config.intent_router = Config::default().intent_router;
    }

    println!("blockcell setup");
    println!("==============");
    println!("Config file: {}", config_path.display());
    println!();

    let selected_provider = resolve_provider(provider.as_deref())?;
    let provider_name = if let Some(name) = selected_provider {
        name.to_string()
    } else {
        prompt_provider(&config)?
    };

    if provider_name != "skip" {
        configure_provider(
            &mut config,
            &provider_name,
            api_key.as_deref(),
            model.as_deref(),
        )?;
    } else {
        println!("Skipped LLM provider setup.");
    }

    let selected_channel = resolve_channel(channel.as_deref())?;
    let channel_name = if let Some(name) = selected_channel {
        name.to_string()
    } else {
        prompt_channel()?
    };

    if channel_name != "skip" {
        configure_channel(&mut config, &channel_name)?;
        ensure_channel_owner(&mut config, &channel_name);
    } else {
        println!("Skipped channel setup (WebUI only).");
    }

    config
        .save(&config_path)
        .with_context(|| format!("Failed to save {}", config_path.display()))?;

    println!();
    println!("✓ Setup completed");
    println!("  Config: {}", config_path.display());
    if provider_name != "skip" {
        let selected = config
            .agents
            .defaults
            .model_pool
            .iter()
            .find(|e| e.provider == provider_name)
            .or_else(|| primary_pool_entry(&config));
        let selected_model = selected
            .map(|e| e.model.as_str())
            .unwrap_or(config.agents.defaults.model.as_str());
        println!("  Provider: {}  Model: {}", provider_name, selected_model);
        if !skip_provider_test {
            match blockcell_providers::create_provider(
                &config,
                selected_model,
                Some(&provider_name),
            ) {
                Ok(_) => println!("  ✓ Provider config validation passed"),
                Err(e) => println!("  ⚠ Provider validation failed: {}", e),
            }
        }
    }
    if channel_name != "skip" {
        println!("  Channel: {} (enabled)", channel_name);
    } else {
        println!("  Channel: none (use WebUI only)");
    }
    println!();
    println!("Next steps:");
    println!("  1. blockcell status");
    println!("  2. blockcell gateway");
    println!("  3. Open WebUI: http://localhost:18791/");

    Ok(())
}

fn configure_provider(
    config: &mut Config,
    provider: &str,
    api_key_flag: Option<&str>,
    model_flag: Option<&str>,
) -> anyhow::Result<()> {
    let entry = config
        .providers
        .entry(provider.to_string())
        .or_default();

    if entry.api_base.is_none() {
        entry.api_base = default_api_base_for_provider(provider).map(|s| s.to_string());
    }
    if entry.api_type.is_empty() {
        entry.api_type = default_api_type_for_provider(provider).to_string();
    }

    if provider == "ollama" {
        if entry.api_key.trim().is_empty() {
            entry.api_key = "ollama".to_string();
        }
        if entry.api_base.is_none() {
            entry.api_base = Some("http://localhost:11434".to_string());
        }
    } else {
        let has_existing = !entry.api_key.trim().is_empty() && entry.api_key != "dummy";
        let final_key = if let Some(flag) = api_key_flag {
            flag.trim().to_string()
        } else if has_existing {
            prompt_line("API key (press Enter to keep existing): ")?
        } else {
            prompt_line("API key: ")?
        };

        if !final_key.is_empty() {
            entry.api_key = final_key;
        }

        if entry.api_key.trim().is_empty() || entry.api_key == "dummy" {
            bail!("Provider '{}' requires a valid API key.", provider);
        }
    }

    let suggested_model = if let Some(entry) = config
        .agents
        .defaults
        .model_pool
        .iter()
        .find(|e| e.provider == provider)
    {
        entry.model.clone()
    } else {
        default_model_for_provider(provider).to_string()
    };

    let final_model = if let Some(m) = model_flag {
        m.trim().to_string()
    } else {
        prompt_line_with_default("Model", &suggested_model)?
    };
    if final_model.trim().is_empty() {
        bail!("Model cannot be empty.");
    }

    // Setup writes provider/model to model_pool first, and also mirrors to legacy
    // single-model fields for backward compatibility with older runtimes.
    config.agents.defaults.model_pool = vec![ModelEntry {
        model: final_model.clone(),
        provider: provider.to_string(),
        weight: 1,
        priority: 1,
        input_price: None,
        output_price: None,
        tool_call_mode: ToolCallMode::Native,
    }];
    config.agents.defaults.provider = Some(provider.to_string());
    config.agents.defaults.model = final_model;

    Ok(())
}

fn configure_channel(config: &mut Config, channel: &str) -> anyhow::Result<()> {
    match channel {
        "telegram" => {
            let existing = config.channels.telegram.token.clone();
            let prompt = if existing.trim().is_empty() {
                "Telegram bot token: "
            } else {
                "Telegram bot token (press Enter to keep existing): "
            };
            let token = prompt_line(prompt)?;
            if !token.is_empty() {
                config.channels.telegram.token = token;
            }
            if config.channels.telegram.token.trim().is_empty() {
                bail!("Telegram token is required.");
            }
            config.channels.telegram.enabled = true;
        }
        "feishu" => {
            let app_id =
                prompt_optional_with_existing("Feishu app_id", &config.channels.feishu.app_id)?;
            let app_secret = prompt_optional_with_existing(
                "Feishu app_secret",
                &config.channels.feishu.app_secret,
            )?;

            if !app_id.is_empty() {
                config.channels.feishu.app_id = app_id;
            }
            if !app_secret.is_empty() {
                config.channels.feishu.app_secret = app_secret;
            }

            if config.channels.feishu.app_id.trim().is_empty()
                || config.channels.feishu.app_secret.trim().is_empty()
            {
                bail!("Feishu app_id and app_secret are required.");
            }
            config.channels.feishu.enabled = true;
        }
        "wecom" => {
            let corp_id =
                prompt_optional_with_existing("WeCom corp_id", &config.channels.wecom.corp_id)?;
            let corp_secret = prompt_optional_with_existing(
                "WeCom corp_secret",
                &config.channels.wecom.corp_secret,
            )?;

            if !corp_id.is_empty() {
                config.channels.wecom.corp_id = corp_id;
            }
            if !corp_secret.is_empty() {
                config.channels.wecom.corp_secret = corp_secret;
            }

            if config.channels.wecom.corp_id.trim().is_empty()
                || config.channels.wecom.corp_secret.trim().is_empty()
            {
                bail!("WeCom corp_id and corp_secret are required.");
            }

            let existing_agent = if config.channels.wecom.agent_id > 0 {
                config.channels.wecom.agent_id.to_string()
            } else {
                "1000002".to_string()
            };
            let agent_id_str = prompt_line_with_default("WeCom agent_id", &existing_agent)?;
            let agent_id = agent_id_str
                .trim()
                .parse::<i64>()
                .map_err(|_| anyhow!("agent_id must be an integer"))?;
            config.channels.wecom.agent_id = agent_id;
            config.channels.wecom.enabled = true;
        }
        "dingtalk" => {
            let app_key = prompt_optional_with_existing(
                "DingTalk app_key",
                &config.channels.dingtalk.app_key,
            )?;
            let app_secret = prompt_optional_with_existing(
                "DingTalk app_secret",
                &config.channels.dingtalk.app_secret,
            )?;
            if !app_key.is_empty() {
                config.channels.dingtalk.app_key = app_key;
            }
            if !app_secret.is_empty() {
                config.channels.dingtalk.app_secret = app_secret;
            }
            if config.channels.dingtalk.app_key.trim().is_empty()
                || config.channels.dingtalk.app_secret.trim().is_empty()
            {
                bail!("DingTalk app_key and app_secret are required.");
            }
            config.channels.dingtalk.enabled = true;
        }
        "lark" => {
            let app_id =
                prompt_optional_with_existing("Lark app_id", &config.channels.lark.app_id)?;
            let app_secret =
                prompt_optional_with_existing("Lark app_secret", &config.channels.lark.app_secret)?;
            if !app_id.is_empty() {
                config.channels.lark.app_id = app_id;
            }
            if !app_secret.is_empty() {
                config.channels.lark.app_secret = app_secret;
            }
            if config.channels.lark.app_id.trim().is_empty()
                || config.channels.lark.app_secret.trim().is_empty()
            {
                bail!("Lark app_id and app_secret are required.");
            }
            config.channels.lark.enabled = true;
        }
        "qq" => {
            let app_id =
                prompt_optional_with_existing("QQ app_id", &config.channels.qq.app_id)?;
            let app_secret =
                prompt_optional_with_existing("QQ app_secret", &config.channels.qq.app_secret)?;
            if !app_id.is_empty() {
                config.channels.qq.app_id = app_id;
            }
            if !app_secret.is_empty() {
                config.channels.qq.app_secret = app_secret;
            }
            if config.channels.qq.app_id.trim().is_empty()
                || config.channels.qq.app_secret.trim().is_empty()
            {
                bail!("QQ app_id and app_secret are required.");
            }
            let environment = prompt_optional_with_existing(
                "QQ environment (production/sandbox)",
                &config.channels.qq.environment,
            )?;
            if !environment.is_empty() {
                config.channels.qq.environment = environment;
            }
            if config.channels.qq.environment.trim().is_empty() {
                config.channels.qq.environment = "production".to_string();
            }
            config.channels.qq.enabled = true;
        }
        "napcat" => {
            let mode_options = [
                "ws-client (BlockCell connects to NapCatQQ WebSocket server)",
                "ws-server (NapCatQQ connects to BlockCell WebSocket server)",
            ];
            let mode_idx = prompt_select("Connection mode", &mode_options, 0)?;
            let mode = match mode_idx {
                0 => "ws-client",
                1 => "ws-server",
                _ => "ws-client",
            }.to_string();

            // WebSocket URL (only for ws-client mode)
            let ws_url = if mode == "ws-client" {
                prompt_line_with_default("WebSocket URL", "ws://127.0.0.1:3001")?
            } else {
                String::new()
            };

            // Server configuration for ws-server mode
            let (server_host, server_port, server_path) = if mode == "ws-server" {
                let host = prompt_line_with_default("WebSocket server host", "0.0.0.0")?;
                // NapCatQQ client 默认连接 ws://localhost:13005
                let port_str = prompt_line_with_default("WebSocket server port", "13005")?;
                let port = port_str.parse::<u16>().unwrap_or(13005);
                // NapCatQQ client 默认连接根路径 /
                let path = prompt_line_with_default("WebSocket server path", "/")?;
                (host, port, path)
            } else {
                (String::new(), 0, String::new())
            };

            let access_token = prompt_line("Access token (optional, press Enter to skip)")?;

            // Group response mode
            let mode_options = [
                "all - 响应所有群聊消息",
                "at_only - 仅响应@我的群聊消息",
                "none - 不响应任何群聊消息",
            ];
            let mode_idx = prompt_select("群聊响应模式", &mode_options, 0)?;
            let group_response_mode = match mode_idx {
                0 => "all",
                1 => "at_only",
                2 => "none",
                _ => "all",
            }
            .to_string();

            config.channels.napcat.enabled = true;
            config.channels.napcat.mode = mode;
            config.channels.napcat.ws_url = ws_url;
            config.channels.napcat.server_host = server_host;
            config.channels.napcat.server_port = server_port;
            config.channels.napcat.server_path = server_path;
            config.channels.napcat.access_token = access_token;
            config.channels.napcat.group_response_mode = group_response_mode;

            // 媒体自动下载配置
            let auto_download_options = [
                "是 - 自动下载图片、语音、视频、文件等媒体",
                "否 - 不自动下载媒体",
            ];
            let auto_idx = prompt_select("是否自动下载媒体文件", &auto_download_options, 0)?;
            config.channels.napcat.auto_download_media = auto_idx == 0;

            if config.channels.napcat.auto_download_media {
                let max_size_str = prompt_line_with_default(
                    "最大自动下载大小 (字节)",
                    "10485760",
                )?;
                if let Ok(max_size) = max_size_str.parse::<u64>() {
                    config.channels.napcat.max_auto_download_size = max_size;
                }

                let download_dir = prompt_line_with_default(
                    "媒体下载目录 (相对于 workspace)",
                    "downloads",
                )?;
                if !download_dir.trim().is_empty() {
                    config.channels.napcat.media_download_dir = download_dir;
                }
            }
        }
        _ => bail!("Unsupported channel '{}'", channel),
    }

    Ok(())
}

fn ensure_channel_owner(config: &mut Config, channel: &str) {
    if config.resolve_channel_owner(channel).is_some() {
        return;
    }
    let owner = config
        .known_agent_ids()
        .into_iter()
        .next()
        .unwrap_or_else(|| "default".to_string());
    config
        .channel_owners
        .insert(channel.to_string(), owner.clone());
    println!("Assigned channel owner: {} -> {}", channel, owner);
}

fn prompt_provider(config: &Config) -> anyhow::Result<String> {
    let current = primary_pool_entry(config)
        .map(|e| e.provider.clone())
        .or_else(|| {
            config
                .agents
                .defaults
                .model_pool
                .iter()
                .find(|e| !e.provider.trim().is_empty())
                .map(|e| e.provider.clone())
        })
        .or_else(|| config.agents.defaults.provider.clone())
        .or_else(|| config.get_api_key().map(|(name, _)| name.to_string()))
        .unwrap_or_else(|| "none".to_string());

    println!("Configure LLM provider (current: {})", current);
    let options = [
        "deepseek (recommended)",
        "openai",
        "kimi",
        "anthropic",
        "gemini",
        "zhipu",
        "minimax",
        "ollama (local)",
        "skip",
    ];
    let idx = prompt_select("Choose provider", &options, 0)?;
    let mapped = match idx {
        0 => "deepseek",
        1 => "openai",
        2 => "kimi",
        3 => "anthropic",
        4 => "gemini",
        5 => "zhipu",
        6 => "minimax",
        7 => "ollama",
        _ => "skip",
    };
    Ok(mapped.to_string())
}

fn primary_pool_entry(config: &Config) -> Option<&ModelEntry> {
    config
        .agents
        .defaults
        .model_pool
        .iter()
        .min_by(|a, b| a.priority.cmp(&b.priority).then(b.weight.cmp(&a.weight)))
}

fn prompt_channel() -> anyhow::Result<String> {
    let options = [
        "skip (WebUI only)",
        "telegram",
        "feishu",
        "wecom",
        "dingtalk",
        "lark",
        "qq",
        "napcat",
    ];
    let idx = prompt_select("Configure one channel (optional)", &options, 0)?;
    let mapped = match idx {
        1 => "telegram",
        2 => "feishu",
        3 => "wecom",
        4 => "dingtalk",
        5 => "lark",
        6 => "qq",
        7 => "napcat",
        _ => "skip",
    };
    Ok(mapped.to_string())
}

fn resolve_provider(input: Option<&str>) -> anyhow::Result<Option<&'static str>> {
    match input {
        Some(v) => normalize_provider(v)
            .ok_or_else(|| anyhow!("Unsupported provider '{}'", v))
            .map(Some),
        None => Ok(None),
    }
}

fn resolve_channel(input: Option<&str>) -> anyhow::Result<Option<&'static str>> {
    match input {
        Some(v) => normalize_channel(v)
            .ok_or_else(|| anyhow!("Unsupported channel '{}'", v))
            .map(Some),
        None => Ok(None),
    }
}

fn normalize_provider(input: &str) -> Option<&'static str> {
    match input.trim().to_lowercase().as_str() {
        "deepseek" => Some("deepseek"),
        "openai" => Some("openai"),
        "kimi" | "moonshot" => Some("kimi"),
        "anthropic" | "claude" => Some("anthropic"),
        "gemini" => Some("gemini"),
        "zhipu" => Some("zhipu"),
        "minimax" => Some("minimax"),
        "ollama" => Some("ollama"),
        "none" | "skip" => Some("skip"),
        _ => None,
    }
}

fn normalize_channel(input: &str) -> Option<&'static str> {
    match input.trim().to_lowercase().as_str() {
        "telegram" => Some("telegram"),
        "feishu" => Some("feishu"),
        "wecom" | "wechatwork" => Some("wecom"),
        "dingtalk" => Some("dingtalk"),
        "lark" => Some("lark"),
        "qq" => Some("qq"),
        "napcat" | "napcatqq" => Some("napcat"),
        "none" | "skip" => Some("skip"),
        _ => None,
    }
}

fn default_model_for_provider(provider: &str) -> &'static str {
    match provider {
        "deepseek" => "deepseek-chat",
        "openai" => "gpt-4o",
        "anthropic" => "claude-sonnet-4-20250514",
        "kimi" => "moonshot-v1-8k",
        "gemini" => "gemini-2.0-flash",
        "zhipu" => "glm-4",
        "minimax" => "minimax-text-01",
        "ollama" => "llama3",
        _ => "gpt-4o",
    }
}

fn default_api_base_for_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "deepseek" => Some("https://api.deepseek.com/v1"),
        "openai" => Some("https://api.openai.com/v1"),
        "anthropic" => Some("https://api.anthropic.com"),
        "kimi" => Some("https://api.moonshot.cn/v1"),
        "gemini" => Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        "zhipu" => Some("https://open.bigmodel.cn/api/paas/v4"),
        "minimax" => Some("https://api.minimaxi.com/v1"),
        "ollama" => Some("http://localhost:11434"),
        _ => None,
    }
}

fn default_api_type_for_provider(provider: &str) -> &'static str {
    match provider {
        "anthropic" | "minimax" => "anthropic",
        "gemini" => "openai",
        "ollama" => "ollama",
        _ => "openai",
    }
}

fn prompt_select(title: &str, options: &[&str], default_index: usize) -> anyhow::Result<usize> {
    println!("{}", title);
    for (i, opt) in options.iter().enumerate() {
        println!("  {}. {}", i + 1, opt);
    }

    loop {
        let input = prompt_line(&format!(
            "Enter choice [1-{}] (default {}): ",
            options.len(),
            default_index + 1
        ))?;

        if input.trim().is_empty() {
            return Ok(default_index);
        }

        if let Ok(n) = input.trim().parse::<usize>() {
            if n >= 1 && n <= options.len() {
                return Ok(n - 1);
            }
        }
        println!("Invalid choice, please try again.");
    }
}

fn prompt_optional_with_existing(label: &str, existing: &str) -> anyhow::Result<String> {
    let prompt = if existing.trim().is_empty() {
        format!("{}: ", label)
    } else {
        format!("{} (press Enter to keep existing): ", label)
    };
    prompt_line(&prompt)
}

fn prompt_line_with_default(label: &str, default: &str) -> anyhow::Result<String> {
    let input = prompt_line(&format!("{} [{}]: ", label, default))?;
    if input.trim().is_empty() {
        Ok(default.to_string())
    } else {
        Ok(input)
    }
}

fn prompt_line(prompt: &str) -> anyhow::Result<String> {
    print!("{}", prompt);
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_provider_aliases() {
        assert_eq!(normalize_provider("moonshot"), Some("kimi"));
        assert_eq!(normalize_provider("claude"), Some("anthropic"));
        assert_eq!(normalize_provider("zhipu"), Some("zhipu"));
        assert_eq!(normalize_provider("minimax"), Some("minimax"));
        assert_eq!(normalize_provider("none"), Some("skip"));
        assert_eq!(normalize_provider("unknown"), None);
    }

    #[test]
    fn test_normalize_channel_aliases() {
        assert_eq!(normalize_channel("wechatwork"), Some("wecom"));
        assert_eq!(normalize_channel("dingtalk"), Some("dingtalk"));
        assert_eq!(normalize_channel("qq"), Some("qq"));
        assert_eq!(normalize_channel("napcat"), Some("napcat"));
        assert_eq!(normalize_channel("napcatqq"), Some("napcat"));
        assert_eq!(normalize_channel("skip"), Some("skip"));
        assert_eq!(normalize_channel("unknown"), None);
    }

    #[test]
    fn test_default_model() {
        assert_eq!(default_model_for_provider("deepseek"), "deepseek-chat");
        assert_eq!(default_model_for_provider("openai"), "gpt-4o");
        assert_eq!(default_model_for_provider("kimi"), "moonshot-v1-8k");
        assert_eq!(default_model_for_provider("zhipu"), "glm-4");
        assert_eq!(default_model_for_provider("minimax"), "minimax-text-01");
    }

    #[test]
    fn test_configure_provider_populates_legacy_single_model_fields_for_compat() {
        let mut config = Config::default();
        config.agents.defaults.model = "legacy-model".to_string();
        config.agents.defaults.provider = Some("legacy-provider".to_string());

        configure_provider(
            &mut config,
            "deepseek",
            Some("test-deepseek-key"),
            Some("deepseek-chat"),
        )
        .expect("configure_provider should succeed");

        assert_eq!(
            config.agents.defaults.provider,
            Some("deepseek".to_string())
        );
        assert_eq!(config.agents.defaults.model, "deepseek-chat");
        assert_eq!(config.agents.defaults.model_pool.len(), 1);
        let entry = &config.agents.defaults.model_pool[0];
        assert_eq!(entry.provider, "deepseek");
        assert_eq!(entry.model, "deepseek-chat");
        assert_eq!(entry.weight, 1);
        assert_eq!(entry.priority, 1);
    }

    #[test]
    fn test_ensure_channel_owner_defaults_to_default_agent() {
        let mut config = Config::default();
        ensure_channel_owner(&mut config, "telegram");
        assert_eq!(
            config.channel_owners.get("telegram").map(|s| s.as_str()),
            Some("default")
        );
    }

    #[test]
    fn test_ensure_channel_owner_keeps_existing() {
        let mut config = Config::default();
        config
            .channel_owners
            .insert("telegram".to_string(), "ops".to_string());
        ensure_channel_owner(&mut config, "telegram");
        assert_eq!(
            config.channel_owners.get("telegram").map(|s| s.as_str()),
            Some("ops")
        );
    }
}
