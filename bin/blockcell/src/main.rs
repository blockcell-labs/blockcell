mod commands;

use clap::{Parser, Subcommand};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[derive(Parser)]
#[command(name = "blockcell")]
#[command(about = "A self-evolving AI agent framework", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize blockcell configuration and workspace
    Onboard {
        /// Force overwrite existing configuration
        #[arg(long)]
        force: bool,
    },

    /// Show current configuration status
    Status,

    /// Run the agent
    Agent {
        /// Message to send (interactive mode if not provided)
        #[arg(short, long)]
        message: Option<String>,

        /// Session ID
        #[arg(short, long, default_value = "cli:default")]
        session: String,
    },

    /// Start the gateway (long-running daemon)
    Gateway {
        /// Port to listen on (overrides config gateway.port)
        #[arg(short, long)]
        port: Option<u16>,

        /// Host to bind to (overrides config gateway.host)
        #[arg(long)]
        host: Option<String>,
    },

    /// Run environment diagnostics
    Doctor,

    /// Manage configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Manage registered tools
    Tools {
        #[command(subcommand)]
        command: ToolsCommands,
    },

    /// Execute a tool or agent message directly
    Run {
        #[command(subcommand)]
        command: RunCommands,
    },

    /// Manage channels
    Channels {
        #[command(subcommand)]
        command: ChannelsCommands,
    },

    /// Manage cron jobs
    Cron {
        #[command(subcommand)]
        command: CronCommands,
    },

    /// Manage upgrades
    Upgrade {
        #[command(subcommand)]
        command: UpgradeCommands,
    },

    /// Manage skill evolution records
    #[command(alias = "skill")]
    Skills {
        #[command(subcommand)]
        command: SkillsCommands,
    },

    /// Manage memory store
    Memory {
        #[command(subcommand)]
        command: MemoryCommands,
    },

    /// Trigger and observe skill evolution
    Evolve {
        #[command(subcommand)]
        command: EvolveCommands,
    },

    /// Manage alert rules
    Alerts {
        #[command(subcommand)]
        command: AlertsCommands,
    },

    /// Manage real-time data stream subscriptions
    Streams {
        #[command(subcommand)]
        command: StreamsCommands,
    },

    /// Manage knowledge graphs
    Knowledge {
        #[command(subcommand)]
        command: KnowledgeCommands,
    },

    /// Generate shell completion scripts
    Completions {
        /// Shell type (bash, zsh, fish, powershell, elvish)
        shell: String,
    },

    /// View and manage agent logs
    Logs {
        #[command(subcommand)]
        command: LogsCommands,
    },
}

// ── P0: Config ──────────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum ConfigCommands {
    /// Get a config value by dot-separated key (e.g. agents.defaults.model)
    Get {
        /// Config key path (e.g. "agents.defaults.model", "providers.openai.api_key")
        key: String,
    },
    /// Set a config value by dot-separated key
    Set {
        /// Config key path
        key: String,
        /// Value to set (auto-detects JSON types)
        value: String,
    },
    /// Open config file in $EDITOR
    Edit,
    /// Show all provider configurations
    Providers,
    /// Reset config to defaults
    Reset {
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
}

// ── P0: Tools ───────────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum ToolsCommands {
    /// List all registered tools
    List {
        /// Filter by category name
        #[arg(long)]
        category: Option<String>,
    },
    /// Show detailed info for a specific tool
    Info {
        /// Tool name
        tool_name: String,
    },
    /// Test a tool by calling it directly with JSON params
    Test {
        /// Tool name
        tool_name: String,
        /// JSON parameters (e.g. '{"action":"info"}')
        params: String,
    },
    /// Enable or disable a tool
    Toggle {
        /// Tool name
        tool_name: String,
        /// Enable the tool
        #[arg(long)]
        enable: bool,
        /// Disable the tool
        #[arg(long)]
        disable: bool,
    },
}

// ── P0: Run ─────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum RunCommands {
    /// Run a tool directly, bypassing the LLM
    Tool {
        /// Tool name
        tool_name: String,
        /// JSON parameters
        params: String,
    },
    /// Send a message through the agent (shortcut for `agent -m`)
    #[command(name = "msg")]
    Message {
        /// Message text
        message: String,
        /// Session ID
        #[arg(short, long, default_value = "cli:run")]
        session: String,
    },
}

// ── P1: Alerts ──────────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum AlertsCommands {
    /// List all alert rules
    List,
    /// Show alert trigger history
    History {
        /// Max entries to show
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Evaluate all alert rules
    Evaluate,
    /// Add a new alert rule
    Add {
        /// Rule name
        #[arg(long)]
        name: String,
        /// Data source (e.g. "finance_api:stock_quote:600519")
        #[arg(long)]
        source: String,
        /// Field to monitor (e.g. "price", "change_pct")
        #[arg(long)]
        field: String,
        /// Comparison operator (gt/lt/gte/lte/eq/ne/change_pct/cross_above/cross_below)
        #[arg(long)]
        operator: String,
        /// Threshold value
        #[arg(long)]
        threshold: String,
    },
    /// Remove an alert rule by ID prefix
    Remove {
        /// Rule ID (prefix match)
        rule_id: String,
    },
}

// ── P1: Streams ─────────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum StreamsCommands {
    /// List all stream subscriptions
    List,
    /// Show details for a specific subscription
    Status {
        /// Subscription ID (prefix match)
        sub_id: String,
    },
    /// Stop and remove a subscription
    Stop {
        /// Subscription ID (prefix match)
        sub_id: String,
    },
    /// Show restorable subscriptions
    Restore,
}

// ── P2: Knowledge ───────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum KnowledgeCommands {
    /// Show knowledge graph statistics
    Stats {
        /// Graph name (default: "default")
        #[arg(long)]
        graph: Option<String>,
    },
    /// Search entities in a knowledge graph
    Search {
        /// Search query
        query: String,
        /// Graph name
        #[arg(long)]
        graph: Option<String>,
        /// Max results
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Export a knowledge graph
    Export {
        /// Output format (json, dot, mermaid)
        #[arg(long, default_value = "json")]
        format: String,
        /// Graph name
        #[arg(long)]
        graph: Option<String>,
        /// Output file path (prints to stdout if omitted)
        #[arg(long)]
        output: Option<String>,
    },
    /// List all knowledge graphs
    ListGraphs,
}

// ── P2: Logs ────────────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum LogsCommands {
    /// Show recent log entries
    Show {
        /// Number of lines to show
        #[arg(long, default_value = "50")]
        lines: usize,
        /// Filter by session ID
        #[arg(long)]
        session: Option<String>,
    },
    /// Follow logs in real-time (tail -f)
    Follow {
        /// Filter by session ID
        #[arg(long)]
        session: Option<String>,
    },
    /// Clear all log files
    Clear {
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum ChannelsCommands {
    /// Show channels status
    Status,
    /// Login to a channel (e.g., WhatsApp QR)
    Login {
        /// Channel name
        channel: String,
    },
}

#[derive(Subcommand)]
enum CronCommands {
    /// List all cron jobs
    List {
        /// Show all jobs including disabled
        #[arg(long)]
        all: bool,
    },
    /// Add a new cron job
    Add {
        /// Job name
        #[arg(long)]
        name: String,
        /// Message to send
        #[arg(long)]
        message: String,
        /// Run every N seconds
        #[arg(long)]
        every: Option<u64>,
        /// Cron expression
        #[arg(long)]
        cron: Option<String>,
        /// Run at specific time (ISO format)
        #[arg(long)]
        at: Option<String>,
        /// Deliver output to channel
        #[arg(long)]
        deliver: bool,
        /// Target chat ID for delivery
        #[arg(long)]
        to: Option<String>,
        /// Target channel for delivery
        #[arg(long)]
        channel: Option<String>,
    },
    /// Remove a cron job
    Remove {
        /// Job ID
        job_id: String,
    },
    /// Enable or disable a cron job
    Enable {
        /// Job ID
        job_id: String,
        /// Disable instead of enable
        #[arg(long)]
        disable: bool,
    },
    /// Run a cron job immediately
    Run {
        /// Job ID
        job_id: String,
        /// Force run even if disabled
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum UpgradeCommands {
    /// Check for available updates
    Check,
    /// Download available update
    Download,
    /// Apply downloaded update
    Apply,
    /// Rollback to previous version
    Rollback {
        /// Specific version to rollback to
        #[arg(long)]
        to: Option<String>,
    },
    /// Show upgrade status
    Status,
}

#[derive(Subcommand)]
enum SkillsCommands {
    /// List skill evolution records
    List {
        /// Show all records including built-in tool errors
        #[arg(long)]
        all: bool,
    },
    /// Learn a new skill by description
    Learn {
        /// Skill description (e.g. "增加网页搜索功能")
        description: String,
    },
    /// Install a skill from the Community Hub
    Install {
        /// Skill name
        name: String,
        /// Specific version (optional)
        #[arg(long)]
        version: Option<String>,
    },
    /// Clear all skill evolution records
    Clear,
    /// Forget (delete) records for a specific skill
    Forget {
        /// Skill name to forget
        name: String,
    },
}

#[derive(Subcommand)]
enum EvolveCommands {
    /// Trigger a new evolution by description
    Run {
        /// Skill evolution description (e.g. "增加网页翻译功能")
        description: String,
        /// Watch progress after triggering
        #[arg(long, short)]
        watch: bool,
    },
    /// Watch evolution progress in real-time
    Watch {
        /// Evolution ID (optional, watches all if omitted)
        evolution_id: Option<String>,
    },
    /// Show evolution status
    Status {
        /// Evolution ID (optional, shows all if omitted)
        evolution_id: Option<String>,
    },
    /// List all evolution records
    List {
        /// Show all records including built-in tool errors
        #[arg(long)]
        all: bool,
        /// Show verbose details (patches, audit, tests)
        #[arg(long, short)]
        verbose: bool,
    },
}

#[derive(Subcommand)]
enum MemoryCommands {
    /// Show memory statistics
    Stats,
    /// Search memory items
    Search {
        /// Search query
        query: String,
        /// Filter by scope (short_term / long_term)
        #[arg(long)]
        scope: Option<String>,
        /// Filter by type (fact/preference/project/task/note/...)
        #[arg(long, name = "type")]
        item_type: Option<String>,
        /// Max results
        #[arg(long, default_value = "10")]
        top: usize,
    },
    /// Run maintenance (clean expired + purge recycle bin)
    Maintenance {
        /// Days to keep soft-deleted items before permanent removal
        #[arg(long, default_value = "30")]
        recycle_days: i64,
    },
    /// Clear memory (soft-delete)
    Clear {
        /// Only clear a specific scope (short_term / long_term)
        #[arg(long)]
        scope: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Setup tracing
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(filter)
        .init();

    match cli.command {
        Commands::Onboard { force } => {
            commands::onboard::run(force).await?;
        }
        Commands::Status => {
            commands::status::run().await?;
        }
        Commands::Agent { message, session } => {
            commands::agent::run(message, session).await?;
        }
        Commands::Gateway { port, host } => {
            commands::gateway::run(host, port).await?;
        }

        // ── P0: Doctor ──────────────────────────────────────────────────
        Commands::Doctor => {
            commands::doctor::run().await?;
        }

        // ── P0: Config ──────────────────────────────────────────────────
        Commands::Config { command } => match command {
            ConfigCommands::Get { key } => {
                commands::config_cmd::get(&key).await?;
            }
            ConfigCommands::Set { key, value } => {
                commands::config_cmd::set(&key, &value).await?;
            }
            ConfigCommands::Edit => {
                commands::config_cmd::edit().await?;
            }
            ConfigCommands::Providers => {
                commands::config_cmd::providers().await?;
            }
            ConfigCommands::Reset { force } => {
                commands::config_cmd::reset(force).await?;
            }
        },

        // ── P0: Tools ───────────────────────────────────────────────────
        Commands::Tools { command } => match command {
            ToolsCommands::List { category } => {
                commands::tools_cmd::list(category).await?;
            }
            ToolsCommands::Info { tool_name } => {
                commands::tools_cmd::info(&tool_name).await?;
            }
            ToolsCommands::Test { tool_name, params } => {
                commands::tools_cmd::test(&tool_name, &params).await?;
            }
            ToolsCommands::Toggle { tool_name, enable, disable } => {
                let enabled = if disable { false } else { enable || true };
                commands::tools_cmd::toggle(&tool_name, enabled).await?;
            }
        },

        // ── P0: Run ─────────────────────────────────────────────────────
        Commands::Run { command } => match command {
            RunCommands::Tool { tool_name, params } => {
                commands::run_cmd::tool(&tool_name, &params).await?;
            }
            RunCommands::Message { message, session } => {
                commands::run_cmd::message(&message, &session).await?;
            }
        },

        // ── Existing: Channels ──────────────────────────────────────────
        Commands::Channels { command } => match command {
            ChannelsCommands::Status => {
                commands::channels::status().await?;
            }
            ChannelsCommands::Login { channel } => {
                commands::channels::login(&channel).await?;
            }
        },
        Commands::Cron { command } => match command {
            CronCommands::List { all } => {
                commands::cron::list(all).await?;
            }
            CronCommands::Add {
                name,
                message,
                every,
                cron,
                at,
                deliver,
                to,
                channel,
            } => {
                commands::cron::add(name, message, every, cron, at, deliver, to, channel).await?;
            }
            CronCommands::Remove { job_id } => {
                commands::cron::remove(&job_id).await?;
            }
            CronCommands::Enable { job_id, disable } => {
                commands::cron::enable(&job_id, !disable).await?;
            }
            CronCommands::Run { job_id, force } => {
                commands::cron::run_job(&job_id, force).await?;
            }
        },
        Commands::Upgrade { command } => match command {
            UpgradeCommands::Check => {
                commands::upgrade::check().await?;
            }
            UpgradeCommands::Download => {
                commands::upgrade::download().await?;
            }
            UpgradeCommands::Apply => {
                commands::upgrade::apply().await?;
            }
            UpgradeCommands::Rollback { to } => {
                commands::upgrade::rollback(to).await?;
            }
            UpgradeCommands::Status => {
                commands::upgrade::status().await?;
            }
        },
        Commands::Skills { command } => match command {
            SkillsCommands::List { all } => {
                commands::skills::list(all).await?;
            }
            SkillsCommands::Learn { description } => {
                commands::skills::learn(&description).await?;
            }
            SkillsCommands::Install { name, version } => {
                commands::skills::install(&name, version).await?;
            }
            SkillsCommands::Clear => {
                commands::skills::clear().await?;
            }
            SkillsCommands::Forget { name } => {
                commands::skills::forget(&name).await?;
            }
        },
        Commands::Evolve { command } => match command {
            EvolveCommands::Run { description, watch } => {
                commands::evolve::run(&description, watch).await?;
            }
            EvolveCommands::Watch { evolution_id } => {
                commands::evolve::watch(evolution_id).await?;
            }
            EvolveCommands::Status { evolution_id } => {
                commands::evolve::status(evolution_id).await?;
            }
            EvolveCommands::List { all, verbose } => {
                commands::evolve::list(all, verbose).await?;
            }
        },
        Commands::Memory { command } => match command {
            MemoryCommands::Stats => {
                commands::memory::stats().await?;
            }
            MemoryCommands::Search { query, scope, item_type, top } => {
                commands::memory::search(&query, scope, item_type, top).await?;
            }
            MemoryCommands::Maintenance { recycle_days } => {
                commands::memory::maintenance(recycle_days).await?;
            }
            MemoryCommands::Clear { scope } => {
                commands::memory::clear(scope).await?;
            }
        },

        // ── P1: Alerts ──────────────────────────────────────────────────
        Commands::Alerts { command } => match command {
            AlertsCommands::List => {
                commands::alerts_cmd::list().await?;
            }
            AlertsCommands::History { limit } => {
                commands::alerts_cmd::history(limit).await?;
            }
            AlertsCommands::Evaluate => {
                commands::alerts_cmd::evaluate().await?;
            }
            AlertsCommands::Add { name, source, field, operator, threshold } => {
                commands::alerts_cmd::add(&name, &source, &field, &operator, &threshold).await?;
            }
            AlertsCommands::Remove { rule_id } => {
                commands::alerts_cmd::remove(&rule_id).await?;
            }
        },

        // ── P1: Streams ─────────────────────────────────────────────────
        Commands::Streams { command } => match command {
            StreamsCommands::List => {
                commands::streams_cmd::list().await?;
            }
            StreamsCommands::Status { sub_id } => {
                commands::streams_cmd::status(&sub_id).await?;
            }
            StreamsCommands::Stop { sub_id } => {
                commands::streams_cmd::stop(&sub_id).await?;
            }
            StreamsCommands::Restore => {
                commands::streams_cmd::restore().await?;
            }
        },

        // ── P2: Knowledge ───────────────────────────────────────────────
        Commands::Knowledge { command } => match command {
            KnowledgeCommands::Stats { graph } => {
                commands::knowledge_cmd::stats(graph).await?;
            }
            KnowledgeCommands::Search { query, graph, limit } => {
                commands::knowledge_cmd::search(&query, graph, limit).await?;
            }
            KnowledgeCommands::Export { format, graph, output } => {
                commands::knowledge_cmd::export(graph, &format, output).await?;
            }
            KnowledgeCommands::ListGraphs => {
                commands::knowledge_cmd::list_graphs().await?;
            }
        },

        // ── P2: Completions ─────────────────────────────────────────────
        Commands::Completions { shell } => {
            commands::completions_cmd::run(&shell).await?;
        }

        // ── P2: Logs ────────────────────────────────────────────────────
        Commands::Logs { command } => match command {
            LogsCommands::Show { lines, session } => {
                commands::logs_cmd::show(lines, session).await?;
            }
            LogsCommands::Follow { session } => {
                commands::logs_cmd::follow(session).await?;
            }
            LogsCommands::Clear { force } => {
                commands::logs_cmd::clear(force).await?;
            }
        },
    }

    Ok(())
}
