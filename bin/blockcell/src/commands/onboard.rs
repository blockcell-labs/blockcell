use blockcell_core::Paths;
use std::io::{self, Write};
use std::process::Command;

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

const TOOLS_MD: &str = r#"# Available Tools

This document describes the tools available to the agent.

## File System Tools

### read_file
Read the contents of a file.
- **path**: Path to the file to read

### write_file
Write content to a file, creating parent directories if needed.
- **path**: Path to the file to write
- **content**: Content to write to the file

### edit_file
Edit a file by replacing old_text with new_text. old_text must match exactly and appear only once.
- **path**: Path to the file to edit
- **old_text**: Text to find and replace (must match exactly)
- **new_text**: Text to replace old_text with

### list_dir
List contents of a directory.
- **path**: Path to the directory to list

## Command Execution

### exec
Execute a shell command.
- **command**: The command to execute
- **working_dir**: Working directory for the command (optional)

**Safety**: Dangerous commands (rm -rf, dd, format, shutdown, etc.) are blocked.

## Web Tools

### web_search
Search the web using Brave Search API.
- **query**: Search query
- **count**: Number of results (1-10, default 5)

**Note**: Requires Brave Search API key in config.

### web_fetch
Fetch and extract content from a URL.
- **url**: URL to fetch (must be http or https)
- **extractMode**: Content extraction mode (markdown or text, default: markdown)
- **maxChars**: Maximum characters to return (default: 50000)

## Communication

### message
Send a message to a specific channel/chat.
- **content**: Message content
- **channel**: Target channel (optional)
- **chat_id**: Target chat ID (optional)

**Note**: Only use this for sending to specific channels. For normal conversation, respond directly.

### spawn
Start a background task (subagent).
- **task**: Task description
- **label**: Task label (optional)

**Note**: Subagents run in isolation and report back when complete.
"#;

const EXAMPLE_CONFIG: &str = r#"{
  "providers": {
    "openrouter": {
      "apiKey": "",
      "apiBase": "https://openrouter.ai/api/v1"
    },
    "anthropic": {
      "apiKey": "",
      "apiBase": "https://api.anthropic.com"
    },
    "openai": {
      "apiKey": "",
      "apiBase": "https://api.openai.com/v1"
    },
    "deepseek": {
      "apiKey": "",
      "apiBase": "https://api.deepseek.com/v1"
    },
    "gemini": {
      "apiKey": "",
      "apiBase": "https://generativelanguage.googleapis.com"
    },
    "kimi": {
      "apiKey": "",
      "apiBase": "https://api.moonshot.cn/v1"
    },
    "groq": {
      "apiKey": "",
      "apiBase": "https://api.groq.com/openai/v1"
    },
    "zhipu": {
      "apiKey": "",
      "apiBase": "https://open.bigmodel.cn/api/paas/v4"
    },
    "ollama": {
      "apiKey": "",
      "apiBase": "http://localhost:11434"
    }
  },
  "agents": {
    "defaults": {
      "model": "anthropic/claude-sonnet-4-20250514",
      "maxTokens": 8192,
      "temperature": 0.7,
      "maxToolIterations": 20
    }
  },
  "gateway": {
    "host": "0.0.0.0",
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
    if paths.config_file().exists() && !force && provider.is_none() && api_key.is_none() && model.is_none() {
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

        let mut json: serde_json::Value = serde_json::from_str(&config_str)
            .unwrap_or_else(|_| serde_json::from_str(EXAMPLE_CONFIG).unwrap());

        ensure_auto_upgrade_defaults(&mut json);

        // Set api_key in providers.<provider>
        if let Some(ref key) = api_key {
            json["providers"][prov]["apiKey"] = serde_json::json!(key);
        }

        // Set model
        if let Some(ref m) = model {
            json["agents"]["defaults"]["model"] = serde_json::json!(m);
        } else {
            // Set a sensible default model for the provider
            let default_model = default_model_for_provider(prov);
            json["agents"]["defaults"]["model"] = serde_json::json!(default_model);
        }

        if let Some(parent) = paths.config_file().parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(paths.config_file(), serde_json::to_string_pretty(&json)?)?;

        println!("✓ Provider configured: {}", prov);
        if api_key.is_some() {
            println!("  ✓ API key set");
        }
        println!("  ✓ Model: {}", json["agents"]["defaults"]["model"].as_str().unwrap_or("?"));
        println!("✓ Config: {}", paths.config_file().display());
        println!();
        println!("Run `blockcell agent` to start chatting.");
        return Ok(());
    }

    // Full onboard: write the annotated example config
    if let Some(parent) = paths.config_file().parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(paths.config_file(), EXAMPLE_CONFIG)?;
    println!("✓ Created config: {}", paths.config_file().display());

    // Create workspace files
    write_if_not_exists(&paths.agents_md(), AGENTS_MD)?;
    write_if_not_exists(&paths.soul_md(), SOUL_MD)?;
    write_if_not_exists(&paths.user_md(), USER_MD)?;
    write_if_not_exists(&paths.tools_md(), TOOLS_MD)?;
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
            let updated = format!("{}

---
{}", existing.trim_end(), env_snapshot.trim_start());
            std::fs::write(&memory_path, updated)?;
        }
    }
    println!("  ✓ Environment snapshot written to MEMORY.md");

    // Extract builtin skills to workspace/skills/ (skip existing files)
    let skills_dir = paths.skills_dir();
    match super::embedded_skills::extract_to_workspace(&skills_dir) {
        Ok(new_skills) if !new_skills.is_empty() => {
            println!("  ✓ Installed {} builtin skill(s): {}", new_skills.len(), new_skills.join(", "));
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("  ⚠️  Failed to extract builtin skills: {}", e);
        }
    }

    println!("✓ Created workspace: {}", paths.workspace().display());
    println!();
    println!("Next steps:");
    println!("  1. Edit {} to add your API keys", paths.config_file().display());
    println!("  2. Run `blockcell status` to verify configuration");
    println!("  3. Run `blockcell agent` to start chatting");
    println!();
    println!("Quick setup examples:");
    println!("  blockcell onboard --provider deepseek --api-key sk-xxx --model deepseek-chat");
    println!("  blockcell onboard --provider kimi --api-key sk-xxx --model moonshot-v1-8k");
    println!("  blockcell onboard --provider openai --api-key sk-xxx");

    Ok(())
}

fn ensure_auto_upgrade_defaults(json: &mut serde_json::Value) {
    if json.get("autoUpgrade").is_none() || json["autoUpgrade"].is_null() {
        json["autoUpgrade"] = serde_json::json!({});
    }

    if json["autoUpgrade"].get("enabled").is_none() {
        json["autoUpgrade"]["enabled"] = serde_json::json!(true);
    }
    if json["autoUpgrade"].get("channel").is_none() {
        json["autoUpgrade"]["channel"] = serde_json::json!("stable");
    }
    if json["autoUpgrade"].get("manifestUrl").is_none() {
        json["autoUpgrade"]["manifestUrl"] = serde_json::json!(
            "https://github.com/blockcell-labs/blockcell/releases/latest/download/manifest.json"
        );
    }
    if json["autoUpgrade"].get("requireSignature").is_none() {
        json["autoUpgrade"]["requireSignature"] = serde_json::json!(false);
    }
    if json["autoUpgrade"].get("maintenanceWindow").is_none() {
        json["autoUpgrade"]["maintenanceWindow"] = serde_json::json!("");
    }
}

fn default_model_for_provider(provider: &str) -> &'static str {
    match provider.to_lowercase().as_str() {
        "deepseek" => "deepseek-chat",
        "openai" => "gpt-4o",
        "anthropic" => "claude-sonnet-4-20250514",
        "kimi" | "moonshot" => "moonshot-v1-8k",
        "gemini" => "gemini-1.5-flash",
        "groq" => "llama-3.1-70b-versatile",
        "zhipu" => "glm-4",
        "ollama" => "llama3",
        _ => "gpt-4o",
    }
}

fn write_if_not_exists(path: &std::path::Path, content: &str) -> io::Result<()> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
        println!("  ✓ Created {}", path.file_name().unwrap().to_string_lossy());
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
    if std::process::Command::new("which").arg(name).output()
        .map(|o| o.status.success()).unwrap_or(false) { "yes" } else { "no" }
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
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
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
            out.push_str(&format!("- **RAM**: {:.1} GB\n", bytes as f64 / 1_073_741_824.0));
        }
    } else if let Some(meminfo) = sh("grep", &["MemTotal", "/proc/meminfo"]) {
        out.push_str(&format!("- **RAM**: {}\n", meminfo));
    }

    // GPU
    let gpu = sh("system_profiler", &["SPDisplaysDataType"])
        .and_then(|s| {
            s.lines()
                .find(|l| l.trim().starts_with("Chipset Model:") || l.trim().starts_with("Chip:"))
                .map(|l| l.trim().trim_start_matches("Chipset Model:").trim_start_matches("Chip:").trim().to_string())
        })
        .or_else(|| sh("lspci", &[]).and_then(|s| {
            s.lines().find(|l| l.contains("VGA") || l.contains("3D")).map(|l| l.to_string())
        }));
    if let Some(g) = gpu {
        out.push_str(&format!("- **GPU**: {}\n", g));
    }

    // Disk
    if let Some(disk) = sh("df", &["-h", "."]) {
        let summary = disk.lines().nth(1).unwrap_or("").split_whitespace()
            .collect::<Vec<_>>();
        if summary.len() >= 4 {
            out.push_str(&format!("- **Disk** (workspace): total={} used={} free={}\n",
                summary[1], summary[2], summary[3]));
        }
    }

    // Camera
    let has_camera = sh("system_profiler", &["SPCameraDataType"])
        .map(|s| s.contains("Camera") || s.contains("FaceTime"))
        .unwrap_or(false)
        || std::path::Path::new("/dev/video0").exists();
    out.push_str(&format!("- **Camera**: {}\n", if has_camera { "available" } else { "not detected" }));

    // Microphone
    let has_mic = sh("system_profiler", &["SPAudioDataType"])
        .map(|s| s.contains("Input") || s.contains("Microphone"))
        .unwrap_or(false);
    out.push_str(&format!("- **Microphone**: {}\n", if has_mic { "available" } else { "not detected" }));

    // ── Software & Runtimes ──────────────────────────────────────────────────
    out.push_str("\n## Software & Runtimes\n\n");

    let binaries = [
        ("python3", &["--version"][..]),
        ("node",    &["--version"][..]),
        ("rustc",   &["--version"][..]),
        ("git",     &["--version"][..]),
        ("docker",  &["--version"][..]),
        ("ffmpeg",  &["-version"][..]),
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
        check_bin("brew"), check_bin("pip3"), check_bin("npm")
    ));

    // Chrome
    let chrome_paths = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ];
    let has_chrome = chrome_paths.iter().any(|p| std::path::Path::new(p).exists())
        || sh("which", &["google-chrome"]).is_some()
        || sh("which", &["chromium"]).is_some();
    out.push_str(&format!("- **Chrome/Chromium**: {}\n", if has_chrome { "available" } else { "not found" }));

    // ── Network ──────────────────────────────────────────────────────────────
    out.push_str("\n## Network\n\n");

    let hostname = sh("hostname", &[]).unwrap_or_else(|| "unknown".to_string());
    out.push_str(&format!("- **Hostname**: {}\n", hostname));

    // Quick internet check via DNS
    use std::net::ToSocketAddrs;
    let internet = "1.1.1.1:53".to_socket_addrs()
        .map(|mut a| a.next().is_some())
        .unwrap_or(false);
    out.push_str(&format!("- **Internet**: {}\n", if internet { "reachable" } else { "unreachable" }));

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
