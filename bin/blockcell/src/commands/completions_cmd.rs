use clap_complete::{generate, Shell};

/// Generate shell completion scripts.
///
/// Note: This requires `clap_complete` to be available.
/// We re-create a minimal CLI definition here to generate completions
/// without circular dependency on the main Cli struct.
pub async fn run(shell: &str) -> anyhow::Result<()> {
    let shell = match shell.to_lowercase().as_str() {
        "bash" => Shell::Bash,
        "zsh" => Shell::Zsh,
        "fish" => Shell::Fish,
        "powershell" | "ps" => Shell::PowerShell,
        "elvish" => Shell::Elvish,
        _ => {
            anyhow::bail!(
                "Unsupported shell: {}. Options: bash, zsh, fish, powershell, elvish",
                shell
            );
        }
    };

    let mut cmd = build_cli();
    generate(shell, &mut cmd, "blockcell", &mut std::io::stdout());

    eprintln!();
    eprintln!("# Usage:");
    match shell {
        Shell::Bash => {
            eprintln!("#   blockcell completions bash > ~/.local/share/bash-completion/completions/blockcell");
            eprintln!("#   or: eval \"$(blockcell completions bash)\"");
        }
        Shell::Zsh => {
            eprintln!("#   blockcell completions zsh > ~/.zfunc/_blockcell");
            eprintln!("#   Make sure fpath includes ~/.zfunc and run compinit");
        }
        Shell::Fish => {
            eprintln!("#   blockcell completions fish > ~/.config/fish/completions/blockcell.fish");
        }
        _ => {}
    }

    Ok(())
}

/// Build a minimal CLI definition for completion generation.
fn build_cli() -> clap::Command {
    clap::Command::new("blockcell")
        .about("A self-evolving AI agent framework")
        .subcommand(clap::Command::new("onboard").about("Initialize configuration and workspace"))
        .subcommand(clap::Command::new("status").about("Show current configuration status"))
        .subcommand(clap::Command::new("agent").about("Run the agent"))
        .subcommand(clap::Command::new("gateway").about("Start the gateway daemon"))
        .subcommand(clap::Command::new("doctor").about("Run environment diagnostics"))
        .subcommand(
            clap::Command::new("config")
                .about("Manage configuration")
                .subcommand(clap::Command::new("get").about("Get a config value"))
                .subcommand(clap::Command::new("set").about("Set a config value"))
                .subcommand(clap::Command::new("edit").about("Open config in editor"))
                .subcommand(clap::Command::new("providers").about("List providers"))
                .subcommand(clap::Command::new("reset").about("Reset to defaults")),
        )
        .subcommand(
            clap::Command::new("tools")
                .about("Manage tools")
                .subcommand(clap::Command::new("list").about("List all tools"))
                .subcommand(clap::Command::new("info").about("Show tool details"))
                .subcommand(clap::Command::new("test").about("Test a tool"))
                .subcommand(clap::Command::new("toggle").about("Enable/disable a tool")),
        )
        .subcommand(
            clap::Command::new("run")
                .about("Execute a tool or message directly")
                .subcommand(clap::Command::new("tool").about("Run a tool directly"))
                .subcommand(clap::Command::new("message").about("Send a message to agent")),
        )
        .subcommand(
            clap::Command::new("channels")
                .about("Manage channels")
                .subcommand(clap::Command::new("status").about("Show channel status"))
                .subcommand(clap::Command::new("login").about("Login to a channel")),
        )
        .subcommand(
            clap::Command::new("cron")
                .about("Manage cron jobs")
                .subcommand(clap::Command::new("list").about("List cron jobs"))
                .subcommand(clap::Command::new("add").about("Add a cron job"))
                .subcommand(clap::Command::new("remove").about("Remove a cron job"))
                .subcommand(clap::Command::new("enable").about("Enable/disable a cron job"))
                .subcommand(clap::Command::new("run").about("Run a cron job now")),
        )
        .subcommand(
            clap::Command::new("upgrade")
                .about("Manage upgrades")
                .subcommand(clap::Command::new("check").about("Check for updates"))
                .subcommand(clap::Command::new("download").about("Download update"))
                .subcommand(clap::Command::new("apply").about("Apply update"))
                .subcommand(clap::Command::new("rollback").about("Rollback"))
                .subcommand(clap::Command::new("status").about("Show upgrade status")),
        )
        .subcommand(
            clap::Command::new("skills")
                .about("Manage skills")
                .subcommand(clap::Command::new("list").about("List skills"))
                .subcommand(clap::Command::new("learn").about("Learn a new skill"))
                .subcommand(clap::Command::new("clear").about("Clear records"))
                .subcommand(clap::Command::new("forget").about("Forget a skill")),
        )
        .subcommand(
            clap::Command::new("memory")
                .about("Manage memory")
                .subcommand(clap::Command::new("stats").about("Show statistics"))
                .subcommand(clap::Command::new("search").about("Search memory"))
                .subcommand(clap::Command::new("maintenance").about("Run maintenance"))
                .subcommand(clap::Command::new("clear").about("Clear memory")),
        )
        .subcommand(
            clap::Command::new("evolve")
                .about("Manage skill evolution")
                .subcommand(clap::Command::new("run").about("Trigger evolution"))
                .subcommand(clap::Command::new("watch").about("Watch progress"))
                .subcommand(clap::Command::new("status").about("Show status"))
                .subcommand(clap::Command::new("list").about("List records")),
        )
        .subcommand(
            clap::Command::new("alerts")
                .about("Manage alert rules")
                .subcommand(clap::Command::new("list").about("List rules"))
                .subcommand(clap::Command::new("history").about("Show trigger history"))
                .subcommand(clap::Command::new("evaluate").about("Evaluate rules"))
                .subcommand(clap::Command::new("add").about("Add a rule"))
                .subcommand(clap::Command::new("remove").about("Remove a rule")),
        )
        .subcommand(
            clap::Command::new("streams")
                .about("Manage data streams")
                .subcommand(clap::Command::new("list").about("List subscriptions"))
                .subcommand(clap::Command::new("status").about("Show subscription details"))
                .subcommand(clap::Command::new("stop").about("Stop a subscription"))
                .subcommand(clap::Command::new("restore").about("Restore subscriptions")),
        )
        .subcommand(
            clap::Command::new("knowledge")
                .about("Manage knowledge graphs")
                .subcommand(clap::Command::new("stats").about("Show statistics"))
                .subcommand(clap::Command::new("search").about("Search entities"))
                .subcommand(clap::Command::new("export").about("Export graph"))
                .subcommand(clap::Command::new("list-graphs").about("List all graphs")),
        )
        .subcommand(clap::Command::new("completions").about("Generate shell completions"))
        .subcommand(
            clap::Command::new("logs")
                .about("View agent logs")
                .subcommand(clap::Command::new("show").about("Show recent logs"))
                .subcommand(clap::Command::new("clear").about("Clear logs")),
        )
}
