use blockcell_agent::intent::IntentToolResolver;
use blockcell_channels::account::{channel_configured, listener_labels};
use blockcell_core::{Config, Paths};
use std::sync::Arc;

use blockcell_tools::build_tool_registry_with_all_mcp;
use blockcell_tools::mcp::manager::McpManager;
use std::process::Command;

const EXTERNAL_CHANNELS: [&str; 9] = [
    "telegram", "whatsapp", "feishu", "slack", "discord", "dingtalk", "wecom", "lark", "qq",
];

fn known_account_ids(config: &Config, channel: &str) -> Vec<String> {
    let mut ids = match channel {
        "telegram" => config
            .channels
            .telegram
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "whatsapp" => config
            .channels
            .whatsapp
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "feishu" => config
            .channels
            .feishu
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "slack" => config
            .channels
            .slack
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "discord" => config
            .channels
            .discord
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "dingtalk" => config
            .channels
            .dingtalk
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "wecom" => config
            .channels
            .wecom
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "lark" => config
            .channels
            .lark
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "qq" => config
            .channels
            .qq
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    ids.sort();
    ids
}

fn enabled_account_ids(config: &Config, channel: &str) -> Vec<String> {
    let mut ids = match channel {
        "telegram" => config
            .channels
            .telegram
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.token.trim().is_empty())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        "whatsapp" => config
            .channels
            .whatsapp
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.bridge_url.trim().is_empty())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        "feishu" => config
            .channels
            .feishu
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.app_id.trim().is_empty())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        "slack" => config
            .channels
            .slack
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.bot_token.trim().is_empty())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        "discord" => config
            .channels
            .discord
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.bot_token.trim().is_empty())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        "dingtalk" => config
            .channels
            .dingtalk
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.app_key.trim().is_empty())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        "wecom" => config
            .channels
            .wecom
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.corp_id.trim().is_empty())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        "lark" => config
            .channels
            .lark
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.app_id.trim().is_empty())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        "qq" => config
            .channels
            .qq
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.app_id.trim().is_empty())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    ids.sort();
    ids
}

fn agent_owner_bindings(config: &Config, agent_id: &str) -> Vec<String> {
    let mut owners: Vec<String> = config
        .channel_owners
        .iter()
        .filter(|(_, owner)| owner.trim() == agent_id)
        .map(|(channel, _)| channel.clone())
        .collect();
    owners.extend(
        config
            .channel_account_owners
            .iter()
            .flat_map(|(channel, bindings)| {
                bindings.iter().filter(|(_, owner)| owner.trim() == agent_id).map(move |(account_id, _)| format!("{}:{}", channel, account_id))
            }),
    );
    owners.sort();
    owners
}

/// Run full environment diagnostics.
pub async fn run() -> anyhow::Result<()> {
    let paths = Paths::new();

    println!();
    println!("🩺 blockcell doctor — Environment Diagnostics");
    println!("================================");
    println!();

    let mut ok_count = 0u32;
    let mut warn_count = 0u32;
    let mut err_count = 0u32;

    // --- 1. Config ---
    println!("📋 Configuration");
    let config_exists = paths.config_file().exists();
    if config_exists {
        print_ok(
            "Config file exists",
            &paths.config_file().display().to_string(),
        );
        ok_count += 1;
    } else {
        print_err(
            "Config file not found",
            "Run `blockcell onboard` to initialize",
        );
        err_count += 1;
    }

    let config = Config::load_or_default(&paths)?;

    if let Some((provider, model, source)) = active_provider_and_model(&config) {
        let ready = provider_ready(&config, &provider);
        if ready {
            print_ok(
                "API key configured",
                &format!("Active provider: {} ({})", provider, source),
            );
            ok_count += 1;
        } else {
            print_err(
                "Active provider credentials incomplete",
                &format!(
                    "Provider '{}' selected by {} has no valid API key",
                    provider, source
                ),
            );
            err_count += 1;
        }
        println!("  Model: {} ({})", model, source);
    } else {
        print_err(
            "No API key configured",
            "Edit config.json5 to add a provider API key",
        );
        err_count += 1;
        println!("  Model: {}", config.agents.defaults.model);
    }
    println!();

    // --- 2. Workspace ---
    println!("📁 Workspace");
    let ws = paths.workspace();
    if ws.exists() {
        print_ok("Workspace directory exists", &ws.display().to_string());
        ok_count += 1;

        // Check writable
        let test_file = ws.join(".doctor_test");
        match std::fs::write(&test_file, "test") {
            Ok(_) => {
                let _ = std::fs::remove_file(&test_file);
                print_ok("Workspace writable", "");
                ok_count += 1;
            }
            Err(e) => {
                print_err("Workspace not writable", &e.to_string());
                err_count += 1;
            }
        }
    } else {
        print_err(
            "Workspace directory not found",
            "Run `blockcell onboard` to initialize",
        );
        err_count += 1;
    }

    // Memory DB
    let memory_db = ws.join("memory").join("memory.db");
    if memory_db.exists() {
        let size = std::fs::metadata(&memory_db).map(|m| m.len()).unwrap_or(0);
        print_ok(
            "Memory database",
            &format!("{} ({} KB)", memory_db.display(), size / 1024),
        );
        ok_count += 1;
    } else {
        print_warn(
            "Memory database not created yet",
            "Will be created on first agent run",
        );
        warn_count += 1;
    }
    println!();

    // --- 3. Tools ---
    println!("🔧 Tools");
    let mcp_manager = Arc::new(McpManager::load(&paths).await?);
    let registry = build_tool_registry_with_all_mcp(Some(&mcp_manager)).await?;
    let tool_count = registry.tool_names().len();
    print_ok(&format!("{} tools registered", tool_count), "");
    ok_count += 1;

    match config.intent_router.as_ref() {
        Some(router) if router.enabled => {
            print_ok(
                "Intent router enabled",
                &format!("default profile: {}", router.default_profile),
            );
            ok_count += 1;
            for agent in config.resolved_agents() {
                let profile = agent
                    .intent_profile
                    .clone()
                    .unwrap_or_else(|| router.default_profile.clone());
                println!("  Agent {} -> {}", agent.id, profile);
            }
            let mcp = blockcell_core::mcp_config::McpResolvedConfig::load_merged(&paths)?;
            match IntentToolResolver::new(&config).validate_with_mcp(&registry, Some(&mcp)) {
                Ok(_) => {
                    print_ok("Intent router validation", "tools and MCP config ok");
                    ok_count += 1;
                }
                Err(err) => {
                    print_err("Intent router validation failed", &err.to_string());
                    err_count += 1;
                }
            }
        }
        Some(_) => {
            print_warn(
                "Intent router disabled",
                "Using Unknown profile toolset from config",
            );
            warn_count += 1;
        }
        None => {
            print_ok(
                "Intent router defaulted",
                "Missing config will use built-in default router",
            );
            ok_count += 1;
        }
    }

    // Check toggles
    let toggles_path = ws.join("toggles.json");
    if toggles_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&toggles_path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                let disabled: usize = val
                    .get("tools")
                    .and_then(|c| c.as_object())
                    .map(|obj| {
                        obj.values()
                            .filter(|v| v == &&serde_json::json!(false))
                            .count()
                    })
                    .unwrap_or(0);
                if disabled > 0 {
                    print_warn(
                        &format!("{} tools disabled", disabled),
                        "Use `blockcell tools toggle <name> --enable` to re-enable",
                    );
                    warn_count += 1;
                }
            }
        }
    }
    println!();

    // --- 4. Resolved agents ---
    println!("🤖 Resolved Agents");
    let resolved_agents = config.resolved_agents();
    print_ok(
        &format!("{} runtime specs resolved", resolved_agents.len()),
        "",
    );
    ok_count += 1;
    for agent in resolved_agents {
        let agent_paths = paths.for_agent(&agent.id);
        let (provider, model, source) = resolved_agent_active_provider_and_model(&config, &agent);
        let mut owners = agent_owner_bindings(&config, &agent.id);
        if agent.id == "default" {
            owners.insert(
                0,
                "internal(cli/ws/system/cron/subagent/ghost/heartbeat)".to_string(),
            );
        }
        println!("  Agent {}", agent.id);
        println!("    root: {}", agent_paths.base.display());
        println!(
            "    profile: {}",
            agent.intent_profile.as_deref().unwrap_or("-")
        );
        println!("    model: {} ({})", model, source);
        println!("    provider: {}", provider);
        println!(
            "    owners: {}",
            if owners.is_empty() {
                "-".to_string()
            } else {
                owners.join(", ")
            }
        );
    }
    println!();

    // --- 4. Skills ---
    println!("🧠 Skills");
    // Skills are extracted to workspace/skills/ on first run/onboard — only scan there.
    let skills_dir = paths.skills_dir();
    let mut skill_count = 0usize;
    if skills_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() && (p.join("SKILL.rhai").exists() || p.join("SKILL.md").exists()) {
                    skill_count += 1;
                }
            }
        }
    }
    print_ok(&format!("{} skills loaded", skill_count), "");
    ok_count += 1;
    println!();

    // --- 5. External dependencies ---
    println!("🖥️  External Dependencies");

    // Rust compiler
    check_command(
        "rustc",
        &["--version"],
        "Rust compiler",
        "Required for tool evolution",
        &mut ok_count,
        &mut warn_count,
    );

    // Python
    check_command(
        "python3",
        &["--version"],
        "Python3",
        "Required for chart/office/ocr tools",
        &mut ok_count,
        &mut warn_count,
    );

    // Node
    check_command(
        "node",
        &["--version"],
        "Node.js",
        "Required for some script tools",
        &mut ok_count,
        &mut warn_count,
    );

    // Git
    check_command(
        "git",
        &["--version"],
        "Git",
        "Required for local Git workflows",
        &mut ok_count,
        &mut warn_count,
    );

    // ffmpeg
    check_command(
        "ffmpeg",
        &["-version"],
        "ffmpeg",
        "Required for audio/video processing",
        &mut ok_count,
        &mut warn_count,
    );

    // Chrome
    let chrome_paths = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ];
    let chrome_found = chrome_paths
        .iter()
        .any(|p| std::path::Path::new(p).exists());
    if chrome_found {
        print_ok("Chrome/Chromium", "browse tool available");
        ok_count += 1;
    } else {
        print_warn("Chrome/Chromium not found", "browse tool features limited");
        warn_count += 1;
    }

    // Docker
    check_command(
        "docker",
        &["--version"],
        "Docker",
        "Required for containerized deployment",
        &mut ok_count,
        &mut warn_count,
    );

    println!();

    // --- 6. Channels ---
    println!("📡 Channels");
    let ch = &config.channels;
    check_channel(
        &config,
        "telegram",
        ch.telegram.enabled,
        channel_configured(&config, "telegram"),
    );
    check_channel(
        &config,
        "whatsapp",
        ch.whatsapp.enabled,
        channel_configured(&config, "whatsapp"),
    );
    check_channel(
        &config,
        "feishu",
        ch.feishu.enabled,
        channel_configured(&config, "feishu"),
    );
    check_channel(
        &config,
        "slack",
        ch.slack.enabled,
        channel_configured(&config, "slack"),
    );
    check_channel(
        &config,
        "discord",
        ch.discord.enabled,
        channel_configured(&config, "discord"),
    );
    check_channel(
        &config,
        "dingtalk",
        ch.dingtalk.enabled,
        channel_configured(&config, "dingtalk"),
    );
    check_channel(
        &config,
        "wecom",
        ch.wecom.enabled,
        channel_configured(&config, "wecom"),
    );
    check_channel(
        &config,
        "lark",
        ch.lark.enabled,
        channel_configured(&config, "lark"),
    );
    for channel in EXTERNAL_CHANNELS {
        if let Some(bindings) = config.channel_account_owners.get(channel) {
            let known_accounts = known_account_ids(&config, channel);
            for (account_id, owner) in bindings {
                if !known_accounts.iter().any(|id| id == account_id) {
                    print_err(
                        &format!("{}:{} account owner invalid", channel, account_id),
                        "account_id not found under channels.<channel>.accounts",
                    );
                    err_count += 1;
                } else if !config.agent_exists(owner) {
                    print_err(
                        &format!("{}:{} account owner invalid", channel, account_id),
                        &format!("agent '{}' does not exist", owner),
                    );
                    err_count += 1;
                } else {
                    print_ok(
                        &format!("{}:{} account owner", channel, account_id),
                        &format!("agent: {}", owner),
                    );
                    ok_count += 1;
                }
            }
        }

        if config.is_external_channel_enabled(channel) {
            match config.resolve_channel_owner(channel) {
                Some(owner) => {
                    print_ok(
                        &format!("{} owner binding", channel),
                        &format!("agent: {}", owner),
                    );
                    ok_count += 1;
                }
                None => {
                    let enabled_accounts = enabled_account_ids(&config, channel);
                    let missing_accounts = enabled_accounts
                        .iter()
                        .filter(|account_id| {
                            config
                                .resolve_channel_account_owner(channel, account_id)
                                .is_none()
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    if enabled_accounts.is_empty() || !missing_accounts.is_empty() {
                        let detail = if missing_accounts.is_empty() {
                            "enabled channel must be bound in channelOwners.<channel> or covered by channelAccountOwners".to_string()
                        } else {
                            format!(
                                "missing account owners for: {}",
                                missing_accounts.join(", ")
                            )
                        };
                        print_err(&format!("{} owner binding missing", channel), &detail);
                        err_count += 1;
                    } else {
                        print_ok(
                            &format!("{} account owner coverage", channel),
                            &format!("{} account override(s)", enabled_accounts.len()),
                        );
                        ok_count += 1;
                    }
                }
            }
        }
    }
    println!();

    // --- 7. Gateway ---
    println!("🌐 Gateway");
    println!(
        "  Bind address: {}:{}",
        config.gateway.host, config.gateway.port
    );
    if let Some(ref token) = config.gateway.api_token {
        if !token.is_empty() {
            print_ok("API token configured", "");
            ok_count += 1;
        } else {
            print_warn(
                "API token is empty",
                "Recommended for production: set gateway.apiToken",
            );
            warn_count += 1;
        }
    } else {
        print_warn(
            "API token not configured",
            "Recommended for production: set gateway.apiToken",
        );
        warn_count += 1;
    }
    println!();

    // --- Summary ---
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!(
        "  ✅ {} passed  ⚠️  {} warnings  ❌ {} errors",
        ok_count, warn_count, err_count
    );

    if err_count > 0 {
        println!();
        println!("  {} error(s) must be fixed before normal use.", err_count);
    } else if warn_count > 0 {
        println!();
        println!("  Core features OK. Some optional features not ready.");
    } else {
        println!();
        println!("  🎉 All good!");
    }
    println!();

    Ok(())
}

fn print_ok(label: &str, detail: &str) {
    if detail.is_empty() {
        println!("  ✅ {}", label);
    } else {
        println!("  ✅ {} — {}", label, detail);
    }
}

fn print_warn(label: &str, hint: &str) {
    if hint.is_empty() {
        println!("  ⚠️  {}", label);
    } else {
        println!("  ⚠️  {} — {}", label, hint);
    }
}

fn print_err(label: &str, hint: &str) {
    if hint.is_empty() {
        println!("  ❌ {}", label);
    } else {
        println!("  ❌ {} — {}", label, hint);
    }
}

fn check_command(
    cmd: &str,
    args: &[&str],
    label: &str,
    purpose: &str,
    ok: &mut u32,
    warn: &mut u32,
) {
    match Command::new(cmd).args(args).output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            let ver_line = version.lines().next().unwrap_or("").trim().to_string();
            let short: String = ver_line.chars().take(40).collect();
            print_ok(label, &short);
            *ok += 1;
        }
        _ => {
            print_warn(&format!("{} not found", label), purpose);
            *warn += 1;
        }
    }
}

fn check_channel(config: &Config, name: &str, enabled: bool, configured: bool) {
    let listeners = listener_labels(config, name);
    let detail = if listeners.is_empty() {
        String::new()
    } else {
        format!(" — listeners: {}", listeners.join(", "))
    };

    if enabled && configured {
        println!("  ✅ {:<12} enabled{}", name, detail);
    } else if configured {
        println!("  ⚪ {:<12} configured (not enabled){}", name, detail);
    } else {
        println!("  ⚪ {:<12} not configured", name);
    }
}

fn provider_ready(config: &Config, provider: &str) -> bool {
    if provider == "ollama" {
        return true;
    }
    config
        .providers
        .get(provider)
        .map(|p| {
            let key = p.api_key.trim();
            !key.is_empty() && key != "dummy"
        })
        .unwrap_or(false)
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

fn active_provider_and_model(config: &Config) -> Option<(String, String, &'static str)> {
    if let Some(entry) = config
        .agents
        .defaults
        .model_pool
        .iter()
        .min_by(|a, b| a.priority.cmp(&b.priority).then(b.weight.cmp(&a.weight)))
    {
        return Some((entry.provider.clone(), entry.model.clone(), "modelPool"));
    }

    if let Some(provider) = config.agents.defaults.provider.as_ref() {
        return Some((
            provider.clone(),
            config.agents.defaults.model.clone(),
            "agents.defaults",
        ));
    }

    config.get_api_key().map(|(name, _)| {
        (
            name.to_string(),
            config.agents.defaults.model.clone(),
            "auto-selected",
        )
    })
}
