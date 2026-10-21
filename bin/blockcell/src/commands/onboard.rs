use blockcell_core::{
    config::{
        parse_json5_value_preserving_env, stringify_json5_pretty, write_raw_validated_config_json5,
    },
    Paths,
};
use std::io::{self, Write};
use std::process::Command;

use super::provider_defaults::default_model_for_provider;

const AGENTS_MD: &str = r#"# Agent Guidelines

You are blockcell, a helpful AI assistant.

## Core Behaviors
- Be helpful, accurate, and concise
- Use tools when needed to accomplish tasks
- Ask for clarification when instructions are ambiguous
- Respect user privacy and security

## Tool Usage
- Use `read_file` to read file contents
- Use `write_file` to create or overwrite files
- Use `edit_file` for precise text replacements
- Use `exec` to run shell commands
- Use `web_search` and `web_fetch` for web information
"#;

const SOUL_MD: &str = r#"# Personality

I am blockcell, a thoughtful and capable AI assistant.

## Values
- Honesty and transparency
- Respect for user autonomy
- Continuous learning and improvement
- Security and privacy awareness

## Communication Style
- Clear and concise
- Professional yet friendly
- Proactive in offering help
- Patient with complex requests
"#;

const USER_MD: &str = r#"# User Preferences

<!-- Add your preferences here -->

## Language
- Preferred language: English

## Work Style
- Prefer concise responses
- Show code examples when helpful
"#;

const MEMORY_MD: &str = r#"# Long-term Memory

<!-- Important information to remember across sessions -->
"#;

const HEARTBEAT_MD: &str = r#"# Heartbeat Tasks

<!-- Add tasks here that should be checked periodically -->
<!-- Empty file or only comments = no action needed -->
"#;

const EXAMPLE_CONFIG: &str = r#"{
  "providers": {
    "openrouter": {
      "apiKey": "",
      "apiBase": "https://openrouter.ai/api/v1",
      "apiType": "openai"
    },
    "anthropic": {
      "apiKey": "",
      "apiBase": "https://api.anthropic.com",
      "apiType": "anthropic"
    },
    "openai": {
      "apiKey": "",
      "apiBase": "https://api.openai.com/v1",
      "proxy": null,
      "apiType": "openai_responses"
    },
    "deepseek": {
      "apiKey": "",
      "apiBase": "https://api.deepseek.com/v1",
      "apiType": "openai"
    },
    "gemini": {
      "apiKey": "",
      "apiBase": "https://generativelanguage.googleapis.com",
      "apiType": "openai"
    },
    "kimi": {
      "apiKey": "",
      "apiBase": "https://api.moonshot.cn/v1",
      "apiType": "openai"
    },
    "groq": {
      "apiKey": "",
      "apiBase": "https://api.groq.com/openai/v1",
      "apiType": "openai"
    },
    "zhipu": {
      "apiKey": "",
      "apiBase": "https://open.bigmodel.cn/api/paas/v4",
      "apiType": "openai"
    },
    "ollama": {
      "apiKey": "",
      "apiBase": "http://localhost:11434",
      "apiType": "ollama"
    }
  },
  "agents": {
    "defaults": {
      "maxTokens": 8192,
      "temperature": 0.7,
      "maxToolIterations": 20,
      "maxContextTokens": 1048576,
      "reasoningEffort": "high",
      "modelPool": [
        {
          "provider": "deepseek",
          "model": "deepseek-v4-pro",
          "weight": 1,
          "priority": 1
        }
      ]
    }
  },
  "gateway": {
    "host": "localhost",
    "port": 18790,
    "webuiHost": "localhost",
    "webuiPort": 18791,
    "apiToken": "",
    "webuiPass": ""
  },
  "channels": {
    "telegram": {
      "enabled": false,
      "token": "",
      "allowFrom": [],
      "proxy": null
    },
    "whatsapp": {
      "enabled": false,
      "bridgeUrl": "ws://localhost:3001",
      "allowFrom": []
    },
    "feishu": {
      "enabled": false,
      "appId": "",
      "appSecret": "",
      "encryptKey": "",
      "verificationToken": "",
      "allowFrom": []
    },
    "slack": {
      "enabled": false,
      "botToken": "",
      "appToken": "",
      "channels": [],
      "allowFrom": [],
      "pollIntervalSecs": 3
    },
    "discord": {
      "enabled": false,
      "botToken": "",
      "channels": [],
      "allowFrom": []
    },
    "dingtalk": {
      "enabled": false,
      "appKey": "",
      "appSecret": "",
      "robotCode": "",
      "allowFrom": []
    },
    "wecom": {
      "enabled": false,
      "corpId": "",
      "corpSecret": "",
      "agentId": 0,
      "callbackToken": "",
      "encodingAesKey": "",
      "allowFrom": [],
      "pollIntervalSecs": 10
    },
    "lark": {
      "enabled": false,
      "appId": "",
      "appSecret": "",
      "encryptKey": "",
      "verificationToken": "",
      "allowFrom": []
    }
  },
  "tools": {
    "web": {
      "search": {
        "apiKey": "",
        "maxResults": 5
      }
    },
    "exec": {
      "timeout": 60,
      "restrictToWorkspace": false
    },
    "tickIntervalSecs": 30
  },
  "autoUpgrade": {
    "enabled": true,
    "channel": "stable",
    "manifestUrl": "https://github.com/blockcell-labs/blockcell/releases/latest/download/manifest.json",
    "requireSignature": false,
    "maintenanceWindow": ""
  }
}
"#;

pub async fn run(
    force: bool,
    provider: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    channels_only: bool,
) -> anyhow::Result<()> {
    let paths = Paths::new();

    if channels_only {
        // Only update channel config — open in editor or print hint
        if !paths.config_file().exists() {
            println!("Config file not found. Run `blockcell onboard` first to create it.");
            return Ok(());
        }
        println!("✓ Config file: {}", paths.config_file().display());
        println!();
        println!("To configure channels, edit the config file and set:");
        println!("  channels.telegram.enabled = true");
        println!("  channels.telegram.token    = \"<your-bot-token>\"");
        println!();
        println!("Run `blockcell config edit` to open the config file in your editor.");
        return Ok(());
    }

    // Check if config exists
    if paths.config_file().exists()
        && !force
        && provider.is_none()
        && api_key.is_none()
        && model.is_none()
    {
        print!("Config already exists. Overwrite? [y/N] ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Create directories
    paths.ensure_dirs()?;

    // If quick-setup flags are provided, patch existing or new config
    if let Some(ref prov) = provider {
        // Load existing config or start from example
        let config_str = if paths.config_file().exists() {
            std::fs::read_to_string(paths.config_file())?
        } else {
            EXAMPLE_CONFIG.to_string()
        };

        let selected_model = if let Some(ref m) = model {
            m.clone()
        } else {
            default_model_for_provider(prov).to_string()
        };
        let patched =
            patch_provider_config(&config_str, prov, api_key.as_deref(), Some(&selected_model))?;

        if let Some(parent) = paths.config_file().parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_raw_validated_config_json5(&paths.config_file(), &patched)?;

        println!("✓ Provider configured: {}", prov);
        if api_key.is_some() {
            println!("  ✓ API key set");
        }
        println!("  ✓ Model: {}", selected_model);
        println!("✓ Config: {}", paths.config_file().display());
        println!();
        println!("Run `blockcell agent` to start chatting.");
        return Ok(());
    }

    // Full onboard: write the annotated example config
    if let Some(parent) = paths.config_file().parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_raw_validated_config_json5(&paths.config_file(), EXAMPLE_CONFIG)?;
    println!("✓ Created config: {}", paths.config_file().display());

    // Create workspace files
    write_if_not_exists(&paths.agents_md(), AGENTS_MD)?;
    write_if_not_exists(&paths.soul_md(), SOUL_MD)?;
    write_if_not_exists(&paths.user_md(), USER_MD)?;
    write_if_not_exists(&paths.memory_md(), MEMORY_MD)?;
    write_if_not_exists(&paths.heartbeat_md(), HEARTBEAT_MD)?;

    // Probe environment and write self-knowledge to MEMORY.md
    let memory_path = paths.memory_md();
    if !memory_path.exists() {
        let env_snapshot = probe_environment();
        write_if_not_exists(&memory_path, &env_snapshot)?;
    } else {
        // Already exists — update the hardware section in place
        let existing = std::fs::read_to_string(&memory_path).unwrap_or_default();
        if !existing.contains("## Hardware") {
            let env_snapshot = probe_environment();
            let updated = format!(
                "{}

---
{}",
                existing.trim_end(),
                env_snapshot.trim_start()
            );
            std::fs::write(&memory_path, updated)?;
        }
    }
    println!("  ✓ Environment snapshot written to MEMORY.md");

    // Extract builtin skills to workspace/skills/ (skip existing files)
    let skills_dir = paths.skills_dir();
    match super::embedded_skills::extract_to_workspace(&skills_dir) {
        Ok(new_skills) if !new_skills.is_empty() => {
            println!(
                "  ✓ Installed {} builtin skill(s): {}",
                new_skills.len(),
                new_skills.join(", ")
            );
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("  ⚠️  Failed to extract builtin skills: {}", e);
        }
    }

    println!("✓ Created workspace: {}", paths.workspace().display());
    println!();
    println!("Next steps:");
    println!(
        "  1. Edit {} to add your API keys",
        paths.config_file().display()
    );
    println!("  2. Run `blockcell status` to verify configuration");
    println!("  3. Run `blockcell agent` to start chatting");
    println!();
    println!("Quick setup examples:");
    println!(
        "  blockcell onboard --provider deepseek --api-key YOUR_API_KEY --model deepseek-v4-pro"
    );
    println!("  blockcell onboard --provider kimi --api-key YOUR_API_KEY --model kimi-k2.6");
    println!("  blockcell onboard --provider openai --api-key YOUR_API_KEY");

    Ok(())
}

fn patch_provider_config(
    config_str: &str,
    provider: &str,
    api_key: Option<&str>,
    model: Option<&str>,
) -> anyhow::Result<String> {
    let mut json: serde_json::Value = parse_json5_value_preserving_env(config_str)?;
    ensure_auto_upgrade_defaults(&mut json);

    if let Some(key) = api_key {
        json["providers"][provider]["apiKey"] = serde_json::json!(key);
    }

    let selected_model = model
        .map(str::to_string)
        .unwrap_or_else(|| default_model_for_provider(provider).to_string());
    json["agents"]["defaults"]["modelPool"] = serde_json::json!([
        {
            "provider": provider,
            "model": selected_model,
            "weight": 1,
            "priority": 1
        }
    ]);
    json["agents"]["defaults"]["maxContextTokens"] =
        serde_json::json!(default_max_context_tokens_for_provider(provider));
    if let Some(defaults) = json["agents"]["defaults"].as_object_mut() {
        defaults.remove("model");
        defaults.remove("provider");
    }

    Ok(stringify_json5_pretty(&json)?)
}

fn ensure_auto_upgrade_defaults(json: &mut serde_json::Value) {
    let defaults = blockcell_core::config::AutoUpgradeConfig::default();
    if json.get("autoUpgrade").is_none() || json["autoUpgrade"].is_null() {
        json["autoUpgrade"] = serde_json::json!({});
    }

    if json["autoUpgrade"].get("enabled").is_none() {
        json["autoUpgrade"]["enabled"] = serde_json::json!(true);
    }
    if json["autoUpgrade"].get("channel").is_none() {
        json["autoUpgrade"]["channel"] = serde_json::json!("stable");
    }
    if json["autoUpgrade"].get("manifestUrl").is_none()
        || json["autoUpgrade"]["manifestUrl"]
            .as_str()
            .unwrap_or("")
            .is_empty()
    {
        json["autoUpgrade"]["manifestUrl"] = serde_json::json!(
            "https://github.com/blockcell-labs/blockcell/releases/latest/download/manifest.json"
        );
    }
    if json["autoUpgrade"].get("requireSignature").is_none() {
        json["autoUpgrade"]["requireSignature"] = serde_json::json!(defaults.require_signature);
    }
    if json["autoUpgrade"].get("publicKey").is_none() {
        json["autoUpgrade"]["publicKey"] = serde_json::json!(defaults.public_key);
    }
    if json["autoUpgrade"].get("maintenanceWindow").is_none() {
        json["autoUpgrade"]["maintenanceWindow"] = serde_json::json!("");
    }
}

#[cfg(test)]
mod auto_upgrade_tests {
    use super::*;

    #[test]
    fn example_config_does_not_require_an_unconfigured_signature_key() {
        let config = blockcell_core::config::parse_json5_value(EXAMPLE_CONFIG)
            .expect("example config should parse");

        assert_eq!(config["autoUpgrade"]["requireSignature"], false);
    }

    #[test]
    fn migration_uses_the_runtime_signature_default_when_missing() {
        let mut config = serde_json::json!({});

        ensure_auto_upgrade_defaults(&mut config);

        assert_eq!(
            config["autoUpgrade"]["requireSignature"],
            blockcell_core::config::AutoUpgradeConfig::default().require_signature
        );
    }

    /// 迁移必须保持 fail-closed：没有可信公钥时仍然要求签名校验，否则
    /// `autoUpgrade.enabled` 会接受仅 SHA256 的更新（`default_require_signature`
    /// 在 P2 批次已改为 true）。想要不签名更新，用户显式写 requireSignature=false。
    ///
    /// 显式把公钥写成 null，避免依赖编译期的 BLOCKCELL_UPDATER_PUBLIC_KEY。
    #[test]
    fn migration_stays_fail_closed_without_a_trusted_key() {
        let mut config = serde_json::json!({ "autoUpgrade": { "publicKey": null } });

        ensure_auto_upgrade_defaults(&mut config);

        assert_eq!(
            config["autoUpgrade"]["requireSignature"], true,
            "没有可信公钥时迁移必须保持 fail-closed"
        );
    }
}

#[cfg(test)]
mod provider_patch_tests {
    use super::*;

    #[test]
    fn provider_patch_rejects_malformed_existing_config() {
        assert!(patch_provider_config("{ broken", "openai", None, None).is_err());
    }

    #[test]
    fn provider_patch_preserves_environment_placeholders() {
        let patched =
            patch_provider_config(EXAMPLE_CONFIG, "openai", Some("${OPENAI_API_KEY}"), None)
                .expect("patch provider config");

        assert!(patched.contains("${OPENAI_API_KEY}"));
    }
}

fn default_max_context_tokens_for_provider(provider: &str) -> u32 {
    match provider.to_lowercase().as_str() {
        "deepseek" => 1_048_576,        // 1M — DeepSeek V4 Pro
        "anthropic" => 200_000,         // 200K — Claude Sonnet 4
        "gemini" => 1_048_576,          // 1M — Gemini 3.5
        "kimi" | "moonshot" => 262_144, // 256K — Kimi K2.6
        "openai" => 131_072,            // 128K — GPT-5.5
        "groq" => 131_072,              // 128K
        "zhipu" => 131_072,             // 128K — GLM-5
        "ollama" => 8_192,              // 8K — depends on local model
        _ => 131_072,                   // 128K default
    }
}

fn write_if_not_exists(path: &std::path::Path, content: &str) -> io::Result<()> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
        println!(
            "  ✓ Created {}",
            path.file_name().unwrap().to_string_lossy()
        );
    }
    Ok(())
}

/// Run a shell command and return trimmed stdout, or None on failure.
fn sh(cmd: &str, args: &[&str]) -> Option<String> {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn check_bin(name: &str) -> &'static str {
    if std::process::Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        "yes"
    } else {
        "no"
    }
}

/// Probe hardware/software environment synchronously at onboard time.
/// Returns a Markdown string suitable for writing into MEMORY.md.
fn probe_environment() -> String {
    let mut out = String::from("# Long-term Memory\n\n");

    // ── Hardware ────────────────────────────────────────────────────────────
    out.push_str("## Hardware\n\n");

    // OS / arch
    out.push_str(&format!(
        "- **OS**: {} ({}) {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        sh("sw_vers", &["-productVersion"])
            .or_else(|| sh("uname", &["-r"]))
            .unwrap_or_default()
    ));

    // CPU cores
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    out.push_str(&format!("- **CPU cores**: {}\n", cores));

    // CPU model
    if let Some(cpu) = sh("sysctl", &["-n", "machdep.cpu.brand_string"])
        .or_else(|| sh("grep", &["-m1", "model name", "/proc/cpuinfo"]))
    {
        out.push_str(&format!("- **CPU model**: {}\n", cpu));
    }

    // RAM
    if let Some(mem_bytes) = sh("sysctl", &["-n", "hw.memsize"]) {
        if let Ok(bytes) = mem_bytes.parse::<u64>() {
            out.push_str(&format!(
                "- **RAM**: {:.1} GB\n",
                bytes as f64 / 1_073_741_824.0
            ));
        }
    } else if let Some(meminfo) = sh("grep", &["MemTotal", "/proc/meminfo"]) {
        out.push_str(&format!("- **RAM**: {}\n", meminfo));
    }

    // GPU
    let gpu = sh("system_profiler", &["SPDisplaysDataType"])
        .and_then(|s| {
            s.lines()
                .find(|l| l.trim().starts_with("Chipset Model:") || l.trim().starts_with("Chip:"))
                .map(|l| {
                    l.trim()
                        .trim_start_matches("Chipset Model:")
                        .trim_start_matches("Chip:")
                        .trim()
                        .to_string()
                })
        })
        .or_else(|| {
            sh("lspci", &[]).and_then(|s| {
                s.lines()
                    .find(|l| l.contains("VGA") || l.contains("3D"))
                    .map(|l| l.to_string())
            })
        });
    if let Some(g) = gpu {
        out.push_str(&format!("- **GPU**: {}\n", g));
    }

    // Disk
    if let Some(disk) = sh("df", &["-h", "."]) {
        let summary = disk
            .lines()
            .nth(1)
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>();
        if summary.len() >= 4 {
            out.push_str(&format!(
                "- **Disk** (workspace): total={} used={} free={}\n",
                summary[1], summary[2], summary[3]
            ));
        }
    }

    // Camera
    let has_camera = sh("system_profiler", &["SPCameraDataType"])
        .map(|s| s.contains("Camera") || s.contains("FaceTime"))
        .unwrap_or(false)
        || std::path::Path::new("/dev/video0").exists();
    out.push_str(&format!(
        "- **Camera**: {}\n",
        if has_camera {
            "available"
        } else {
            "not detected"
        }
    ));

    // Microphone
    let has_mic = sh("system_profiler", &["SPAudioDataType"])
        .map(|s| s.contains("Input") || s.contains("Microphone"))
        .unwrap_or(false);
    out.push_str(&format!(
        "- **Microphone**: {}\n",
        if has_mic { "available" } else { "not detected" }
    ));

    // ── Software & Runtimes ──────────────────────────────────────────────────
    out.push_str("\n## Software & Runtimes\n\n");

    let binaries = [
        ("python3", &["--version"][..]),
        ("node", &["--version"][..]),
        ("rustc", &["--version"][..]),
        ("git", &["--version"][..]),
        ("docker", &["--version"][..]),
        ("ffmpeg", &["-version"][..]),
    ];
    for (bin, args) in &binaries {
        let ver = sh(bin, args)
            .map(|v| v.lines().next().unwrap_or("").to_string())
            .unwrap_or_else(|| "not installed".to_string());
        out.push_str(&format!("- **{}**: {}\n", bin, ver));
    }

    // Package managers
    out.push_str(&format!(
        "- **brew**: {} | **pip3**: {} | **npm**: {}\n",
        check_bin("brew"),
        check_bin("pip3"),
        check_bin("npm")
    ));

    // Chrome
    let chrome_paths = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ];
    let has_chrome = chrome_paths
        .iter()
        .any(|p| std::path::Path::new(p).exists())
        || sh("which", &["google-chrome"]).is_some()
        || sh("which", &["chromium"]).is_some();
    out.push_str(&format!(
        "- **Chrome/Chromium**: {}\n",
        if has_chrome { "available" } else { "not found" }
    ));

    // ── Network ──────────────────────────────────────────────────────────────
    out.push_str("\n## Network\n\n");

    let hostname = sh("hostname", &[]).unwrap_or_else(|| "unknown".to_string());
    out.push_str(&format!("- **Hostname**: {}\n", hostname));

    // Quick internet check via DNS
    use std::net::ToSocketAddrs;
    let internet = "1.1.1.1:53"
        .to_socket_addrs()
        .map(|mut a| a.next().is_some())
        .unwrap_or(false);
    out.push_str(&format!(
        "- **Internet**: {}\n",
        if internet { "reachable" } else { "unreachable" }
    ));

    // ── Capabilities Summary ─────────────────────────────────────────────────
    out.push_str("\n## Agent Capabilities (at onboard time)\n\n");
    out.push_str("- Can read/write files, execute shell commands\n");
    out.push_str("- Can search web and fetch URLs\n");
    if sh("which", &["rustc"]).is_some() {
        out.push_str("- Can compile Rust code (rustc available) → can evolve new tools\n");
    }
    if sh("which", &["python3"]).is_some() {
        out.push_str("- Can run Python scripts (charts, office docs, ML tasks)\n");
    }
    if sh("which", &["node"]).is_some() {
        out.push_str("- Can run Node.js scripts\n");
    }
    if sh("which", &["ffmpeg"]).is_some() {
        out.push_str("- Can process audio/video (ffmpeg available)\n");
    }
    if has_camera {
        out.push_str("- Camera available (can capture images)\n");
    }
    if has_mic {
        out.push_str("- Microphone available (can record/transcribe audio)\n");
    }

    out.push_str("\n<!-- Updated by: blockcell onboard -->\n");
    out
}
