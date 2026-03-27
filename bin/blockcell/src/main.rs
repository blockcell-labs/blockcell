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
        /// Run in interactive wizard mode (default)
        #[arg(long)]
        interactive: bool,
        /// LLM provider name (e.g. deepseek, openai, kimi, anthropic)
        #[arg(long)]
        provider: Option<String>,
        /// API key for the provider
        #[arg(long, name = "api-key")]
        api_key: Option<String>,
        /// Model name (e.g. deepseek-chat, kimi-k2.5)
        #[arg(long)]
        model: Option<String>,
        /// Only update channel configuration, skip provider setup
        #[arg(long)]
        channels_only: bool,
    },

    /// Interactive setup wizard (provider + channel)
    Setup {
        /// Reset existing config to defaults before setup
        #[arg(long)]
        force: bool,
        /// LLM provider name (deepseek/openai/kimi/anthropic/gemini/zhipu/minimax/ollama)
        #[arg(long)]
        provider: Option<String>,
        /// API key for selected provider
        #[arg(long, name = "api-key")]
        api_key: Option<String>,
        /// Model name override
        #[arg(long)]
        model: Option<String>,
        /// Optional channel to configure (telegram/feishu/wecom/dingtalk/lark/qq/napcat/none)
        #[arg(long)]
        channel: Option<String>,
        /// Skip provider config validation after saving config
        #[arg(long)]
        skip_provider_test: bool,
    },

    /// Show current configuration status
    Status,

    /// Run the agent
    Agent {
        /// Message to send (interactive mode if not provided)
        #[arg(short, long)]
        message: Option<String>,

        /// Target agent id (defaults to "default")
        #[arg(short = 'a', long)]
        agent: Option<String>,

        /// Session ID (defaults to cli:<agent>)
        #[arg(short, long)]
        session: Option<String>,

        /// Override LLM model for this session
        #[arg(long)]
        model: Option<String>,

        /// Override LLM provider for this session
        #[arg(long)]
        provider: Option<String>,
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

    /// Manage MCP servers
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
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

    /// Check and install upgrades
    Upgrade {
        /// Only check for updates, do not install
        #[arg(long)]
        check: bool,
        #[command(subcommand)]
        command: Option<UpgradeCommands>,
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
    /// Show current configuration
    Show,
    /// Print the JSON Schema for the config file
    Schema,
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
    /// Show detailed info for a specific tool (alias for 'info')
    Show {
        /// Tool name
        tool_name: String,
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

#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum McpCommands {
    /// List MCP servers
    List,
    /// Show one MCP server
    Show {
        /// MCP server name
        name: String,
    },
    /// Add an MCP server from template or raw config
    Add {
        /// Template name (github/sqlite/filesystem/postgres/puppeteer) or logical name for `--raw`
        template_or_name: String,
        /// Use raw command/args/env instead of template generation
        #[arg(long)]
        raw: bool,
        /// Explicit server name override
        #[arg(long)]
        name: Option<String>,
        /// Raw command executable
        #[arg(long)]
        command: Option<String>,
        /// Repeatable raw argument
        #[arg(long = "arg")]
        args: Vec<String>,
        /// Repeatable environment variable entry KEY=VALUE
        #[arg(long = "env")]
        env: Vec<String>,
        /// Working directory
        #[arg(long)]
        cwd: Option<String>,
        /// SQLite template database path
        #[arg(long)]
        db_path: Option<String>,
        /// Filesystem template root path (repeatable)
        #[arg(long = "path")]
        filesystem_paths: Vec<String>,
        /// Postgres template DSN
        #[arg(long)]
        dsn: Option<String>,
        /// Overwrite existing file if present
        #[arg(long)]
        force: bool,
        /// Create disabled
        #[arg(long)]
        disabled: bool,
        /// Disable auto-start
        #[arg(long)]
        no_auto_start: bool,
        /// Startup timeout override
        #[arg(long)]
        startup_timeout_secs: Option<u64>,
        /// Call timeout override
        #[arg(long)]
        call_timeout_secs: Option<u64>,
    },
    /// Remove an MCP server
    Remove {
        /// MCP server name
        name: String,
    },
    /// Enable an MCP server
    Enable {
        /// MCP server name
        name: String,
    },
    /// Disable an MCP server
    Disable {
        /// MCP server name
        name: String,
    },
    /// Open MCP config in editor
    Edit {
        /// Optional server name; edits mcp.d/<name>.json if present
        name: Option<String>,
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
        /// Target agent id (defaults to "default")
        #[arg(short = 'a', long)]
        agent: Option<String>,
    },
    /// Send a message through the agent (shortcut for `agent -m`)
    #[command(name = "msg")]
    Message {
        /// Message text
        message: String,
        /// Session ID
        #[arg(short, long, default_value = "cli:run")]
        session: String,
        /// Target agent id (defaults to "default")
        #[arg(short = 'a', long)]
        agent: Option<String>,
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
        /// Data source (e.g. "stream_subscribe:ticker:BTCUSDT")
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
    /// Unsubscribe (alias for 'stop')
    Unsubscribe {
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
        /// Filter by keyword (e.g. evolution, ghost, tool)
        #[arg(long)]
        filter: Option<String>,
        /// Alias for --lines
        #[arg(short = 'n')]
        last_n: Option<usize>,
        /// Filter by session ID
        #[arg(long)]
        session: Option<String>,
    },
    /// Follow logs in real-time (tail -f)
    Follow {
        /// Filter by keyword
        #[arg(long)]
        filter: Option<String>,
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
    /// Manage channel owner bindings (channel -> agent)
    Owner {
        #[command(subcommand)]
        command: ChannelOwnerCommands,
    },
}

#[derive(Subcommand)]
enum ChannelOwnerCommands {
    /// List channel owner bindings
    List,
    /// Set owner agent for a channel or channel account
    Set {
        /// Channel name
        #[arg(long)]
        channel: String,
        /// Optional account id for account-level binding
        #[arg(long)]
        account: Option<String>,
        /// Agent id
        #[arg(long)]
        agent: String,
    },
    /// Clear owner binding for a channel or channel account
    Clear {
        /// Channel name
        #[arg(long)]
        channel: String,
        /// Optional account id for account-level binding
        #[arg(long)]
        account: Option<String>,
    },
}

#[derive(Subcommand)]
enum CronCommands {
    /// List cron jobs (read-only; manage jobs via the WebUI or chat channels)
    List {
        /// Show all jobs including disabled
        #[arg(long)]
        all: bool,
        /// Agent ID to query (default: "default")
        #[arg(long, default_value = "default")]
        agent: String,
    },
}

#[derive(Subcommand, Default)]
enum UpgradeCommands {
    /// Check for available updates
    #[default]
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
    /// List all skills
    List {
        /// Show all records including built-in tool errors
        #[arg(long)]
        all: bool,
        /// Only show enabled skills
        #[arg(long)]
        enabled: bool,
    },
    /// Show details for a specific skill
    Show {
        /// Skill name
        name: String,
    },
    /// Enable a skill
    Enable {
        /// Skill name
        name: String,
    },
    /// Disable a skill
    Disable {
        /// Skill name
        name: String,
    },
    /// Hot-reload all skills from disk
    Reload,
    /// Run a skill test
    Test {
        /// Path to the skill directory (e.g. ./skills/web_search)
        path: String,
        /// Simulated user input injected as user_input variable
        #[arg(long, short)]
        input: Option<String>,
        /// Show script logs and verbose meta.yaml output
        #[arg(long, short)]
        verbose: bool,
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
    /// Batch-test all skills under a directory
    TestAll {
        /// Path to the skills directory (e.g. ./skills)
        dir: String,
        /// Simulated user input injected as user_input variable
        #[arg(long, short)]
        input: Option<String>,
        /// Show script logs
        #[arg(long, short)]
        verbose: bool,
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
    /// Manually trigger evolution for a skill (alias for 'run')
    Trigger {
        /// Skill name to evolve
        skill_name: String,
        /// Optional reason / hint for the evolution
        #[arg(long)]
        reason: Option<String>,
    },
    /// Show evolution history for a skill (alias for 'status')
    Show {
        /// Skill name or evolution ID
        skill_name: String,
    },
    /// Rollback a skill to a previous version
    Rollback {
        /// Skill name
        skill_name: String,
        /// Target version (e.g. v2)
        #[arg(long)]
        to: Option<String>,
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
    /// List recent memory items
    List {
        /// Filter by type (fact/preference/project/task/note/...)
        #[arg(long, name = "type")]
        item_type: Option<String>,
        /// Max results
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Show a specific memory item by ID
    Show {
        /// Memory item ID
        id: String,
    },
    /// Delete a memory item by ID
    Delete {
        /// Memory item ID
        id: String,
    },
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
    /// Retry queued vector sync operations
    RetryVectorSync {
        /// Max pending operations to replay in one run
        #[arg(long, default_value = "100")]
        limit: usize,
    },
    /// Rebuild the vector index from active SQLite rows
    Reindex,
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
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(filter)
        .init();

    match cli.command {
        Commands::Onboard {
            force,
            interactive: _,
            provider,
            api_key,
            model,
            channels_only,
        } => {
            commands::onboard::run(force, provider, api_key, model, channels_only).await?;
        }
        Commands::Setup {
            force,
            provider,
            api_key,
            model,
            channel,
            skip_provider_test,
        } => {
            commands::setup::run(force, provider, api_key, model, channel, skip_provider_test)
                .await?;
        }
        Commands::Status => {
            commands::status::run().await?;
        }
        Commands::Agent {
            message,
            agent,
            session,
            model,
            provider,
        } => {
            commands::agent::run(message, agent, session, model, provider).await?;
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
            ConfigCommands::Show => {
                commands::config_cmd::show().await?;
            }
            ConfigCommands::Schema => {
                commands::config_cmd::schema().await?;
            }
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
            ToolsCommands::Show { tool_name } | ToolsCommands::Info { tool_name } => {
                commands::tools_cmd::info(&tool_name).await?;
            }
            ToolsCommands::Test { tool_name, params } => {
                commands::tools_cmd::test(&tool_name, &params).await?;
            }
            ToolsCommands::Toggle {
                tool_name,
                enable: _,
                disable,
            } => {
                let enabled = !disable;
                commands::tools_cmd::toggle(&tool_name, enabled).await?;
            }
        },

        // ── P0: MCP ─────────────────────────────────────────────────────
        Commands::Mcp { command } => match command {
            McpCommands::List => {
                commands::mcp::list().await?;
            }
            McpCommands::Show { name } => {
                commands::mcp::show(&name).await?;
            }
            McpCommands::Add {
                template_or_name,
                raw,
                name,
                command,
                args,
                env,
                cwd,
                db_path,
                filesystem_paths,
                dsn,
                force,
                disabled,
                no_auto_start,
                startup_timeout_secs,
                call_timeout_secs,
            } => {
                commands::mcp::add(
                    &template_or_name,
                    raw,
                    name,
                    command,
                    args,
                    env,
                    cwd,
                    db_path,
                    filesystem_paths,
                    dsn,
                    force,
                    disabled,
                    no_auto_start,
                    startup_timeout_secs,
                    call_timeout_secs,
                )
                .await?;
            }
            McpCommands::Remove { name } => {
                commands::mcp::remove(&name).await?;
            }
            McpCommands::Enable { name } => {
                commands::mcp::set_enabled(&name, true).await?;
            }
            McpCommands::Disable { name } => {
                commands::mcp::set_enabled(&name, false).await?;
            }
            McpCommands::Edit { name } => {
                commands::mcp::edit(name.as_deref()).await?;
            }
        },

        // ── P0: Run ─────────────────────────────────────────────────────
        Commands::Run { command } => match command {
            RunCommands::Tool {
                tool_name,
                params,
                agent,
            } => {
                commands::run_cmd::tool(&tool_name, &params, agent.as_deref()).await?;
            }
            RunCommands::Message {
                message,
                session,
                agent,
            } => {
                commands::run_cmd::message(&message, &session, agent.as_deref()).await?;
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
            ChannelsCommands::Owner { command } => match command {
                ChannelOwnerCommands::List => {
                    commands::channels::owner_list().await?;
                }
                ChannelOwnerCommands::Set {
                    channel,
                    account,
                    agent,
                } => {
                    commands::channels::owner_set(&channel, account.as_deref(), &agent).await?;
                }
                ChannelOwnerCommands::Clear { channel, account } => {
                    commands::channels::owner_clear(&channel, account.as_deref()).await?;
                }
            },
        },
        Commands::Cron { command } => match command {
            CronCommands::List { all, agent } => {
                commands::cron::list(all, &agent).await?;
            }
        },
        Commands::Upgrade { check, command } => {
            if check {
                commands::upgrade::check().await?;
            } else {
                match command.unwrap_or_default() {
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
                }
            }
        }
        Commands::Skills { command } => match command {
            SkillsCommands::List { all, enabled } => {
                commands::skills::list(all, enabled).await?;
            }
            SkillsCommands::Show { name } => {
                commands::skills::show(&name).await?;
            }
            SkillsCommands::Enable { name } => {
                commands::skills::set_enabled(&name, true).await?;
            }
            SkillsCommands::Disable { name } => {
                commands::skills::set_enabled(&name, false).await?;
            }
            SkillsCommands::Reload => {
                commands::skills::reload().await?;
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
            SkillsCommands::Test {
                path,
                input,
                verbose,
            } => {
                commands::skills::test(&path, input, verbose).await?;
            }
            SkillsCommands::TestAll {
                dir,
                input,
                verbose,
            } => {
                commands::skills::test_all(&dir, input, verbose).await?;
            }
        },
        Commands::Evolve { command } => match command {
            EvolveCommands::Run { description, watch } => {
                commands::evolve::run(&description, watch).await?;
            }
            EvolveCommands::Trigger { skill_name, reason } => {
                let desc = reason.as_deref().unwrap_or(&skill_name);
                let full_desc = if reason.is_some() {
                    format!("{}: {}", skill_name, desc)
                } else {
                    skill_name.clone()
                };
                commands::evolve::run(&full_desc, false).await?;
            }
            EvolveCommands::Show { skill_name } => {
                commands::evolve::show(&skill_name).await?;
            }
            EvolveCommands::Rollback { skill_name, to } => {
                commands::evolve::rollback(&skill_name, to).await?;
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
            MemoryCommands::List { item_type, limit } => {
                commands::memory::list(item_type, limit).await?;
            }
            MemoryCommands::Show { id } => {
                commands::memory::show(&id).await?;
            }
            MemoryCommands::Delete { id } => {
                commands::memory::delete(&id).await?;
            }
            MemoryCommands::Stats => {
                commands::memory::stats().await?;
            }
            MemoryCommands::Search {
                query,
                scope,
                item_type,
                top,
            } => {
                commands::memory::search(&query, scope, item_type, top).await?;
            }
            MemoryCommands::Maintenance { recycle_days } => {
                commands::memory::maintenance(recycle_days).await?;
            }
            MemoryCommands::RetryVectorSync { limit } => {
                commands::memory::retry_vector_sync(limit).await?;
            }
            MemoryCommands::Reindex => {
                commands::memory::reindex().await?;
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
            AlertsCommands::Add {
                name,
                source,
                field,
                operator,
                threshold,
            } => {
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
            StreamsCommands::Stop { sub_id } | StreamsCommands::Unsubscribe { sub_id } => {
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
            KnowledgeCommands::Search {
                query,
                graph,
                limit,
            } => {
                commands::knowledge_cmd::search(&query, graph, limit).await?;
            }
            KnowledgeCommands::Export {
                format,
                graph,
                output,
            } => {
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
            LogsCommands::Show {
                lines,
                filter,
                last_n,
                session,
            } => {
                let n = last_n.unwrap_or(lines);
                commands::logs_cmd::show(n, filter, session).await?;
            }
            LogsCommands::Follow { filter, session } => {
                commands::logs_cmd::follow(filter, session).await?;
            }
            LogsCommands::Clear { force } => {
                commands::logs_cmd::clear(force).await?;
            }
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_agent_subcommand_accepts_agent_flag() {
        let cli = Cli::try_parse_from(["blockcell", "agent", "--agent", "ops"])
            .expect("agent flag should parse");

        match cli.command {
            Commands::Agent { agent, session, .. } => {
                assert_eq!(agent.as_deref(), Some("ops"));
                assert!(session.is_none());
            }
            other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_agent_subcommand_accepts_agent_short_flag() {
        let cli = Cli::try_parse_from(["blockcell", "agent", "-a", "ops", "-m", "hello"])
            .expect("short agent flag should parse");

        match cli.command {
            Commands::Agent { agent, message, .. } => {
                assert_eq!(agent.as_deref(), Some("ops"));
                assert_eq!(message.as_deref(), Some("hello"));
            }
            other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_run_message_subcommand_accepts_agent_flag() {
        let cli = Cli::try_parse_from(["blockcell", "run", "msg", "hello", "--agent", "ops"])
            .expect("run msg agent flag should parse");

        match cli.command {
            Commands::Run { command } => match command {
                RunCommands::Message {
                    message,
                    session,
                    agent,
                } => {
                    assert_eq!(message, "hello");
                    assert_eq!(session, "cli:run");
                    assert_eq!(agent.as_deref(), Some("ops"));
                }
                other => panic!(
                    "unexpected run command: {:?}",
                    std::mem::discriminant(&other)
                ),
            },
            other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_run_tool_subcommand_accepts_agent_flag() {
        let cli = Cli::try_parse_from([
            "blockcell",
            "run",
            "tool",
            "read_file",
            r#"{"path":"README.md"}"#,
            "--agent",
            "ops",
        ])
        .expect("run tool agent flag should parse");

        match cli.command {
            Commands::Run { command } => match command {
                RunCommands::Tool {
                    tool_name,
                    params,
                    agent,
                } => {
                    assert_eq!(tool_name, "read_file");
                    assert_eq!(params, r#"{"path":"README.md"}"#);
                    assert_eq!(agent.as_deref(), Some("ops"));
                }
                other => panic!(
                    "unexpected run command: {:?}",
                    std::mem::discriminant(&other)
                ),
            },
            other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_channels_owner_set_accepts_account_flag() {
        let cli = Cli::try_parse_from([
            "blockcell",
            "channels",
            "owner",
            "set",
            "--channel",
            "telegram",
            "--account",
            "bot2",
            "--agent",
            "ops",
        ])
        .expect("channels owner set --account should parse");

        match cli.command {
            Commands::Channels { command } => match command {
                ChannelsCommands::Owner { command } => match command {
                    ChannelOwnerCommands::Set {
                        channel,
                        account,
                        agent,
                    } => {
                        assert_eq!(channel, "telegram");
                        assert_eq!(account.as_deref(), Some("bot2"));
                        assert_eq!(agent, "ops");
                    }
                    other => panic!(
                        "unexpected owner command: {:?}",
                        std::mem::discriminant(&other)
                    ),
                },
                other => panic!(
                    "unexpected channels command: {:?}",
                    std::mem::discriminant(&other)
                ),
            },
            other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_channels_owner_clear_accepts_account_flag() {
        let cli = Cli::try_parse_from([
            "blockcell",
            "channels",
            "owner",
            "clear",
            "--channel",
            "telegram",
            "--account",
            "bot2",
        ])
        .expect("channels owner clear --account should parse");

        match cli.command {
            Commands::Channels { command } => match command {
                ChannelsCommands::Owner { command } => match command {
                    ChannelOwnerCommands::Clear { channel, account } => {
                        assert_eq!(channel, "telegram");
                        assert_eq!(account.as_deref(), Some("bot2"));
                    }
                    other => panic!(
                        "unexpected owner command: {:?}",
                        std::mem::discriminant(&other)
                    ),
                },
                other => panic!(
                    "unexpected channels command: {:?}",
                    std::mem::discriminant(&other)
                ),
            },
            other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_memory_retry_vector_sync_parses_limit() {
        let cli =
            Cli::try_parse_from(["blockcell", "memory", "retry-vector-sync", "--limit", "25"])
                .expect("memory retry-vector-sync should parse");

        match cli.command {
            Commands::Memory { command } => match command {
                MemoryCommands::RetryVectorSync { limit } => {
                    assert_eq!(limit, 25);
                }
                other => panic!(
                    "unexpected memory command: {:?}",
                    std::mem::discriminant(&other)
                ),
            },
            other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_memory_reindex_parses() {
        let cli =
            Cli::try_parse_from(["blockcell", "memory", "reindex"]).expect("reindex should parse");

        match cli.command {
            Commands::Memory { command } => match command {
                MemoryCommands::Reindex => {}
                other => panic!(
                    "unexpected memory command: {:?}",
                    std::mem::discriminant(&other)
                ),
            },
            other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
        }
    }
}
