use blockcell_core::{Config, Paths};
use blockcell_tools::ToolRegistry;
use std::process::Command;

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
        print_ok("Config file exists", &paths.config_file().display().to_string());
        ok_count += 1;
    } else {
        print_err("Config file not found", "Run `blockcell onboard` to initialize");
        err_count += 1;
    }

    let config = Config::load_or_default(&paths)?;

    if let Some((name, _)) = config.get_api_key() {
        print_ok("API key configured", &format!("Active provider: {}", name));
        ok_count += 1;
    } else {
        print_err("No API key configured", "Edit config.json to add a provider API key");
        err_count += 1;
    }

    println!("  Model: {}", config.agents.defaults.model);
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
        print_err("Workspace directory not found", "Run `blockcell onboard` to initialize");
        err_count += 1;
    }

    // Memory DB
    let memory_db = ws.join("memory").join("memory.db");
    if memory_db.exists() {
        let size = std::fs::metadata(&memory_db).map(|m| m.len()).unwrap_or(0);
        print_ok("Memory database", &format!("{} ({} KB)", memory_db.display(), size / 1024));
        ok_count += 1;
    } else {
        print_warn("Memory database not created yet", "Will be created on first agent run");
        warn_count += 1;
    }
    println!();

    // --- 3. Tools ---
    println!("🔧 Tools");
    let registry = ToolRegistry::with_defaults();
    let tool_count = registry.tool_names().len();
    print_ok(&format!("{} tools registered", tool_count), "");
    ok_count += 1;

    // Check toggles
    let toggles_path = ws.join("toggles.json");
    if toggles_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&toggles_path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                let disabled: usize = val.get("tools")
                    .and_then(|c| c.as_object())
                    .map(|obj| obj.values().filter(|v| v == &&serde_json::json!(false)).count())
                    .unwrap_or(0);
                if disabled > 0 {
                    print_warn(&format!("{} tools disabled", disabled), "Use `blockcell tools toggle <name> --enable` to re-enable");
                    warn_count += 1;
                }
            }
        }
    }
    println!();

    // --- 4. Skills ---
    println!("🧠 Skills");
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
    // Also count builtin skills
    let builtin_dir = paths.builtin_skills_dir();
    if builtin_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&builtin_dir) {
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
    check_command("rustc", &["--version"], "Rust compiler", "Required for tool evolution", &mut ok_count, &mut warn_count);

    // Python
    check_command("python3", &["--version"], "Python3", "Required for chart/office/ocr tools", &mut ok_count, &mut warn_count);

    // Node
    check_command("node", &["--version"], "Node.js", "Required for some script tools", &mut ok_count, &mut warn_count);

    // Git
    check_command("git", &["--version"], "Git", "Required for git_api tool", &mut ok_count, &mut warn_count);

    // ffmpeg
    check_command("ffmpeg", &["-version"], "ffmpeg", "Required for audio/video processing", &mut ok_count, &mut warn_count);

    // Chrome
    let chrome_paths = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ];
    let chrome_found = chrome_paths.iter().any(|p| std::path::Path::new(p).exists());
    if chrome_found {
        print_ok("Chrome/Chromium", "browse/chrome_control tools available");
        ok_count += 1;
    } else {
        print_warn("Chrome/Chromium not found", "browse/chrome_control features limited");
        warn_count += 1;
    }

    // Docker
    check_command("docker", &["--version"], "Docker", "Required for containerized deployment", &mut ok_count, &mut warn_count);

    println!();

    // --- 6. Channels ---
    println!("📡 Channels");
    let ch = &config.channels;
    check_channel("telegram", ch.telegram.enabled, !ch.telegram.token.is_empty());
    check_channel("whatsapp", ch.whatsapp.enabled, true);
    check_channel("feishu", ch.feishu.enabled, !ch.feishu.app_id.is_empty());
    check_channel("slack", ch.slack.enabled, !ch.slack.bot_token.is_empty());
    check_channel("discord", ch.discord.enabled, !ch.discord.bot_token.is_empty());
    println!();

    // --- 7. Gateway ---
    println!("🌐 Gateway");
    println!("  Bind address: {}:{}", config.gateway.host, config.gateway.port);
    if let Some(ref token) = config.gateway.api_token {
        if !token.is_empty() {
            print_ok("API token configured", "");
            ok_count += 1;
        } else {
            print_warn("API token is empty", "Recommended for production: set gateway.apiToken");
            warn_count += 1;
        }
    } else {
        print_warn("API token not configured", "Recommended for production: set gateway.apiToken");
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

fn check_command(cmd: &str, args: &[&str], label: &str, purpose: &str, ok: &mut u32, warn: &mut u32) {
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

fn check_channel(name: &str, enabled: bool, configured: bool) {
    if enabled && configured {
        println!("  ✅ {:<12} enabled", name);
    } else if configured {
        println!("  ⚪ {:<12} configured (not enabled)", name);
    } else {
        println!("  ⚪ {:<12} not configured", name);
    }
}
