use blockcell_core::{Config, Paths};
use serde_json::Value;

/// Show the current configuration as pretty-printed JSON.
pub async fn show() -> anyhow::Result<()> {
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;
    let json = serde_json::to_value(&config)?;

    println!();
    println!("📋 Current Configuration");
    println!("  File: {}", paths.config_file().display());
    println!();
    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}

/// Print the JSON Schema for the configuration file.
pub async fn schema() -> anyhow::Result<()> {
    let schema = serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "BlockcellConfig",
        "description": "blockcell configuration file (~/.blockcell/config.json)",
        "type": "object",
        "properties": {
            "providers": {
                "type": "object",
                "description": "LLM provider configurations keyed by name (openai, deepseek, kimi, anthropic, gemini, ollama, ...)",
                "additionalProperties": {
                    "type": "object",
                    "properties": {
                        "apiKey": { "type": "string", "description": "API key for this provider" },
                        "apiBase": { "type": "string", "description": "Base URL override (e.g. for proxy or self-hosted)" }
                    }
                }
            },
            "agents": {
                "type": "object",
                "properties": {
                    "defaults": {
                        "type": "object",
                        "properties": {
                            "model": { "type": "string", "description": "Default model name, e.g. deepseek-chat" },
                            "provider": { "type": "string", "description": "Explicit provider name override" },
                            "evolutionModel": { "type": "string", "description": "Model for self-evolution pipeline" },
                            "evolutionProvider": { "type": "string", "description": "Provider for self-evolution pipeline" },
                            "maxContextTokens": { "type": "integer", "default": 32000 }
                        }
                    }
                }
            },
            "channels": {
                "type": "object",
                "description": "Messaging channel configurations",
                "properties": {
                    "whatsapp": {
                        "type": "object",
                        "properties": {
                            "enabled": { "type": "boolean" },
                            "bridgeUrl": { "type": "string" },
                            "allowFrom": { "type": "array", "items": { "type": "string" } }
                        }
                    },
                    "telegram": {
                        "type": "object",
                        "properties": {
                            "enabled": { "type": "boolean" },
                            "token": { "type": "string" },
                            "allowFrom": { "type": "array", "items": { "type": "string" } },
                            "proxy": { "type": "string" }
                        }
                    },
                    "feishu": {
                        "type": "object",
                        "properties": {
                            "enabled": { "type": "boolean" },
                            "appId": { "type": "string" },
                            "appSecret": { "type": "string" },
                            "encryptKey": { "type": "string" },
                            "verificationToken": { "type": "string" },
                            "allowFrom": { "type": "array", "items": { "type": "string" } }
                        }
                    },
                    "lark": {
                        "type": "object",
                        "properties": {
                            "enabled": { "type": "boolean" },
                            "appId": { "type": "string" },
                            "appSecret": { "type": "string" },
                            "encryptKey": { "type": "string" },
                            "verificationToken": { "type": "string" },
                            "allowFrom": { "type": "array", "items": { "type": "string" } }
                        }
                    },
                    "slack": {
                        "type": "object",
                        "properties": {
                            "enabled": { "type": "boolean" },
                            "botToken": { "type": "string" },
                            "appToken": { "type": "string" },
                            "channels": { "type": "array", "items": { "type": "string" } },
                            "allowFrom": { "type": "array", "items": { "type": "string" } },
                            "pollIntervalSecs": { "type": "integer", "default": 3 }
                        }
                    },
                    "discord": {
                        "type": "object",
                        "properties": {
                            "enabled": { "type": "boolean" },
                            "botToken": { "type": "string" },
                            "channels": { "type": "array", "items": { "type": "string" } },
                            "allowFrom": { "type": "array", "items": { "type": "string" } }
                        }
                    },
                    "dingtalk": {
                        "type": "object",
                        "properties": {
                            "enabled": { "type": "boolean" },
                            "appKey": { "type": "string" },
                            "appSecret": { "type": "string" },
                            "robotCode": { "type": "string" },
                            "allowFrom": { "type": "array", "items": { "type": "string" } }
                        }
                    },
                    "wecom": {
                        "type": "object",
                        "properties": {
                            "enabled": { "type": "boolean" },
                            "corpId": { "type": "string" },
                            "corpSecret": { "type": "string" },
                            "agentId": { "type": "integer" },
                            "callbackToken": { "type": "string" },
                            "encodingAesKey": { "type": "string" },
                            "allowFrom": { "type": "array", "items": { "type": "string" } },
                            "pollIntervalSecs": { "type": "integer", "default": 10 }
                        }
                    }
                }
            },
            "exec": {
                "type": "object",
                "properties": {
                    "timeout": { "type": "integer", "default": 60 },
                    "restrictToWorkspace": { "type": "boolean", "default": false }
                }
            }
        }
    });

    println!("{}", serde_json::to_string_pretty(&schema)?);
    Ok(())
}

/// Get a config value by dot-separated key path.
pub async fn get(key: &str) -> anyhow::Result<()> {
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;
    let json = serde_json::to_value(&config)?;

    let value = resolve_json_path(&json, key);
    match value {
        Some(v) => {
            if v.is_string() {
                println!("{}", v.as_str().unwrap());
            } else {
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
        }
        None => {
            eprintln!("Key '{}' not found in config.", key);
            std::process::exit(1);
        }
    }
    Ok(())
}

/// Set a config value by dot-separated key path.
pub async fn set(key: &str, value: &str) -> anyhow::Result<()> {
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;
    let mut json = serde_json::to_value(&config)?;

    // Try to parse value as JSON, fall back to string
    let parsed: Value = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()));

    set_json_path(&mut json, key, parsed.clone());

    // Write back
    let new_config: Config = serde_json::from_value(json)?;
    new_config.save(&paths.config_file())?;

    if parsed.is_string() {
        println!("✓ Set {} = {}", key, parsed.as_str().unwrap());
    } else {
        println!("✓ Set {} = {}", key, serde_json::to_string(&parsed)?);
    }
    Ok(())
}

/// Open config file in $EDITOR.
pub async fn edit() -> anyhow::Result<()> {
    let paths = Paths::new();
    let config_path = paths.config_file();

    if !config_path.exists() {
        eprintln!("Config file not found. Run `blockcell onboard` first.");
        std::process::exit(1);
    }

    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| {
            // macOS default
            if cfg!(target_os = "macos") {
                "open -t".to_string()
            } else {
                "vi".to_string()
            }
        });

    let parts: Vec<&str> = editor.split_whitespace().collect();
    let (cmd, args) = parts.split_first().unwrap();

    let status = std::process::Command::new(cmd)
        .args(args)
        .arg(&config_path)
        .status()?;

    if !status.success() {
        eprintln!("Editor exited with status: {}", status);
    }
    Ok(())
}

/// Show all providers and their status.
pub async fn providers() -> anyhow::Result<()> {
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;

    println!();
    println!("📡 Provider Configuration");
    println!();

    let active = config.get_api_key().map(|(name, _)| name.to_string());

    let mut names: Vec<&String> = config.providers.keys().collect();
    names.sort();

    for name in &names {
        let provider = &config.providers[*name];
        let has_key = !provider.api_key.is_empty() && provider.api_key != "dummy";
        let is_active = active.as_deref() == Some(name.as_str());

        let status_icon = if is_active {
            "⭐"
        } else if has_key {
            "✓"
        } else {
            "✗"
        };

        let key_display = if has_key {
            let key = &provider.api_key;
            if key.len() > 8 {
                format!("{}...{}", &key[..4], &key[key.len()-4..])
            } else {
                "(set)".to_string()
            }
        } else {
            "(empty)".to_string()
        };

        let base = provider.api_base.as_deref().unwrap_or("(default)");

        println!(
            "  {} {:<14} key: {:<16} base: {}",
            status_icon, name, key_display, base
        );
    }

    println!();
    println!("  Current model: {}", config.agents.defaults.model);
    if let Some((name, _)) = config.get_api_key() {
        println!("  Active provider: {}", name);
    } else {
        println!("  ⚠ No API key configured");
    }
    println!();
    Ok(())
}

/// Reset config to defaults.
pub async fn reset(force: bool) -> anyhow::Result<()> {
    let paths = Paths::new();

    if !force {
        print!("⚠ Reset config to defaults? Current config will be lost. [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let config = Config::default();
    config.save(&paths.config_file())?;
    println!("✓ Config reset to defaults: {}", paths.config_file().display());
    Ok(())
}

/// Navigate a JSON value by dot-separated path.
fn resolve_json_path(json: &Value, path: &str) -> Option<Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = json;
    for part in &parts {
        // Try camelCase conversion (e.g. "api_key" -> "apiKey")
        let camel = to_camel_case(part);
        if let Some(v) = current.get(&camel) {
            current = v;
        } else if let Some(v) = current.get(*part) {
            current = v;
        } else {
            return None;
        }
    }
    Some(current.clone())
}

/// Set a value in a JSON object by dot-separated path.
fn set_json_path(json: &mut Value, path: &str, value: Value) {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = json;
    for (i, part) in parts.iter().enumerate() {
        let camel = to_camel_case(part);
        let key = if current.get(&camel).is_some() {
            camel
        } else {
            part.to_string()
        };

        if i == parts.len() - 1 {
            current[&key] = value;
            return;
        }

        if current.get(&key).is_none() || !current[&key].is_object() {
            current[&key] = serde_json::json!({});
        }
        current = &mut current[&key];
    }
}

/// Convert snake_case to camelCase.
fn to_camel_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = false;
    for ch in s.chars() {
        if ch == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(ch.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(ch);
        }
    }
    result
}
