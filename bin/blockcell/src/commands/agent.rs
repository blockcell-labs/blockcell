use blockcell_agent::{
    AgentRuntime, CapabilityRegistryAdapter, ConfirmRequest, CoreEvolutionAdapter,
    MemoryStoreAdapter, MessageBus, ProviderLLMBridge, TaskManager,
};
#[cfg(feature = "dingtalk")]
use blockcell_channels::dingtalk::DingTalkChannel;
#[cfg(feature = "discord")]
use blockcell_channels::discord::DiscordChannel;
#[cfg(feature = "feishu")]
use blockcell_channels::feishu::FeishuChannel;
#[cfg(feature = "slack")]
use blockcell_channels::slack::SlackChannel;
#[cfg(feature = "telegram")]
use blockcell_channels::telegram::TelegramChannel;
#[cfg(feature = "wecom")]
use blockcell_channels::wecom::WeComChannel;
#[cfg(feature = "whatsapp")]
use blockcell_channels::whatsapp::WhatsAppChannel;
use blockcell_channels::ChannelManager;
use blockcell_core::{Config, InboundMessage, Paths};
use blockcell_providers::{Provider, ProviderPool};
use blockcell_scheduler::CronService;
use blockcell_skills::{is_builtin_tool, new_registry_handle, CoreEvolution};
use blockcell_storage::MemoryStore;
use blockcell_tools::{
    CapabilityRegistryHandle, CoreEvolutionHandle, MemoryStoreHandle, ToolRegistry,
};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{info, warn};

/// Built-in tools grouped by category for /tools display.
/// This must include ALL tools registered in ToolRegistry::with_defaults().
const BUILTIN_TOOLS: &[(&str, &[(&str, &str)])] = &[
    (
        "📁 Filesystem",
        &[
            ("read_file", "Read files (text/Office/PDF)"),
            ("write_file", "Create and write files"),
            ("edit_file", "Precise file content editing"),
            ("list_dir", "Browse directory structure"),
            ("file_ops", "Delete/move/copy/compress/decompress/PDF"),
        ],
    ),
    (
        "⚡ Commands & System",
        &[
            ("exec", "Execute shell commands"),
            ("system_info", "Hardware/software/network detection"),
        ],
    ),
    (
        "🌐 Web & Browser",
        &[
            ("web_search", "Search engine queries"),
            ("web_fetch", "Fetch web page content"),
            (
                "browse",
                "CDP browser automation (35+ actions, tabs/screenshots/PDF/network)",
            ),
            ("http_request", "Generic HTTP/REST API calls"),
        ],
    ),
    (
        "🖥️ GUI Automation",
        &[("app_control", "macOS app control (System Events)")],
    ),
    (
        "🎨 Media",
        &[
            ("camera_capture", "Camera capture"),
            ("audio_transcribe", "Speech-to-text (Whisper/API)"),
            ("tts", "Text-to-speech (say/piper/edge-tts/OpenAI)"),
            ("ocr", "Image text recognition (Tesseract/Vision/API)"),
            (
                "image_understand",
                "Multimodal image understanding (GPT-4o/Claude/Gemini)",
            ),
            (
                "video_process",
                "Video processing (ffmpeg cut/merge/subtitle/watermark/compress)",
            ),
            ("chart_generate", "Chart generation (matplotlib/plotly)"),
        ],
    ),
    (
        "📊 Data Processing",
        &[
            ("data_process", "CSV read/write/stats/query/transform"),
            ("office_write", "Generate PPTX/DOCX/XLSX documents"),
            (
                "knowledge_graph",
                "Knowledge graph (entities/relations/paths/export DOT/Mermaid)",
            ),
        ],
    ),
    (
        "📬 Communication",
        &[
            ("email", "Email send/receive (SMTP/IMAP, attachments)"),
            ("message", "Channel messaging (Telegram/Slack/Discord)"),
        ],
    ),
    (
        "📅 Business Integration",
        &[],
    ),
    (
        "💰 Finance",
        &[
            (
                "stream_subscribe",
                "Real-time data streams (WebSocket/SSE, CEX feeds)",
            ),
            (
                "alert_rule",
                "Conditional monitoring alerts (price/indicator/change rate)",
            ),
        ],
    ),
    (
        "⛓️ Blockchain",
        &[],
    ),
    (
        "🔒 Security & Network",
        &[
            ("encrypt", "Encrypt/decrypt/password/hash/encode"),
            (
                "network_monitor",
                "Network diagnostics (ping/traceroute/port scan/SSL/DNS/WHOIS)",
            ),
        ],
    ),
    (
        "🧠 Memory & Cognition",
        &[
            ("memory_query", "Full-text memory search (SQLite FTS5)"),
            ("memory_upsert", "Structured memory storage"),
            ("memory_forget", "Memory delete and restore"),
        ],
    ),
    (
        "🤖 Autonomy & Evolution",
        &[
            ("spawn", "Spawn sub-agents for parallel execution"),
            ("list_tasks", "View task status"),
            ("cron", "Scheduled task management"),
            ("list_skills", "Skill learning status query"),
            ("capability_evolve", "Self-learn new tools via evolution"),
        ],
    ),
];

/// Extract image file paths from user input.
/// Supports:
/// - Inline absolute paths: `/path/to/image.png what is this image`
/// - @-prefixed paths: `@/path/to/image.png recognize this`
/// - ~ home dir paths: `~/Desktop/photo.jpg take a look`
/// Returns (cleaned_text, media_paths).
fn extract_media_from_input(input: &str) -> (String, Vec<String>) {
    let image_extensions = ["jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "heic"];
    let mut media = Vec::new();
    let mut text_parts = Vec::new();

    for token in input.split_whitespace() {
        let path_str = token.strip_prefix('@').unwrap_or(token);
        // Expand ~ to home dir
        let expanded: String = if path_str.starts_with("~/") {
            if let Some(home) = dirs::home_dir() {
                home.join(&path_str[2..]).to_string_lossy().into_owned()
            } else {
                path_str.to_string()
            }
        } else {
            path_str.to_string()
        };

        let path = std::path::Path::new(&expanded);
        let is_image = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| image_extensions.contains(&e.to_lowercase().as_str()))
            .unwrap_or(false);

        if is_image && path.exists() {
            media.push(expanded);
        } else {
            text_parts.push(token.to_string());
        }
    }

    let text = text_parts.join(" ");
    (text, media)
}

#[allow(dead_code)]
fn create_provider(config: &Config) -> anyhow::Result<Box<dyn Provider>> {
    super::provider::create_provider(config)
}

fn build_pool_with_overrides(
    config: &mut Config,
    model_override: Option<String>,
    provider_override: Option<String>,
) -> anyhow::Result<std::sync::Arc<ProviderPool>> {
    if let Some(ref m) = model_override {
        // If model_pool is already configured, clear it and use the override as a single entry
        if !config.agents.defaults.model_pool.is_empty() {
            config.agents.defaults.model_pool.clear();
        }
        config.agents.defaults.model = m.clone();
    }
    if let Some(ref p) = provider_override {
        config.agents.defaults.provider = Some(p.clone());
    }
    ProviderPool::from_config(config)
}

#[derive(Debug)]
struct AgentCliContext {
    agent_id: String,
    session: String,
    config: Config,
    paths: Paths,
}

fn resolve_agent_context(
    config: &Config,
    paths: &Paths,
    requested_agent: Option<&str>,
    requested_session: Option<&str>,
) -> anyhow::Result<AgentCliContext> {
    let agent_id = requested_agent
        .map(str::trim)
        .filter(|agent_id| !agent_id.is_empty())
        .unwrap_or("default");

    if !config.agent_exists(agent_id) {
        anyhow::bail!("Unknown agent '{}'", agent_id);
    }

    let agent_config = config
        .config_for_agent(agent_id)
        .ok_or_else(|| anyhow::anyhow!("Unknown agent '{}'", agent_id))?;
    let agent_paths = paths.for_agent(agent_id);
    let session = requested_session
        .map(str::trim)
        .filter(|session| !session.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("cli:{}", agent_id));

    Ok(AgentCliContext {
        agent_id: agent_id.to_string(),
        session,
        config: agent_config,
        paths: agent_paths,
    })
}

pub async fn run(
    message: Option<String>,
    agent: Option<String>,
    session: Option<String>,
    model: Option<String>,
    provider: Option<String>,
) -> anyhow::Result<()> {
    let root_paths = Paths::new();
    let root_config = Config::load_or_default(&root_paths)?;
    let resolved = resolve_agent_context(
        &root_config,
        &root_paths,
        agent.as_deref(),
        session.as_deref(),
    )?;
    let agent_id = resolved.agent_id.clone();
    let session = resolved.session;
    let paths = resolved.paths;
    paths.ensure_dirs()?;
    let mut config = resolved.config;
    let provider_pool = build_pool_with_overrides(&mut config, model, provider)?;

    // Ensure builtin skills are extracted to workspace/skills/ (silent, skips existing)
    let _ = super::embedded_skills::extract_to_workspace(&paths.skills_dir());

    // Initialize memory store (SQLite + FTS5)
    let memory_db_path = paths.memory_dir().join("memory.db");
    let memory_store_handle: Option<MemoryStoreHandle> = match MemoryStore::open(&memory_db_path) {
        Ok(store) => {
            // Run migration from MEMORY.md/daily files on first startup
            if let Err(e) = store.migrate_from_files(&paths.memory_dir()) {
                eprintln!("Warning: memory migration failed: {}", e);
            }
            let adapter = MemoryStoreAdapter::new(store);
            Some(Arc::new(adapter))
        }
        Err(e) => {
            eprintln!(
                "Warning: failed to open memory store: {}. Memory tools will be unavailable.",
                e
            );
            None
        }
    };

    // Initialize tool evolution registry and core evolution engine
    let cap_registry_dir = paths.evolved_tools_dir();
    let cap_registry_raw = new_registry_handle(cap_registry_dir);
    {
        let mut reg = cap_registry_raw.lock().await;
        let _ = reg.load(); // Load persisted evolved tools from disk
        let rehydrated = reg.rehydrate_executors(); // Rebuild executors for persisted evolved tools
        if rehydrated > 0 {
            info!("Rehydrated {} evolved tool executors from disk", rehydrated);
        }
    }

    // 使用配置中的 LLM 超时设置，默认 300 秒
    let llm_timeout_secs = 300u64;
    let mut core_evo = CoreEvolution::new(
        paths.workspace().to_path_buf(),
        cap_registry_raw.clone(),
        llm_timeout_secs,
    );

    // Create an LLM provider bridge so CoreEvolution can generate code autonomously
    if let Some((_, evo_p)) = provider_pool.acquire() {
        let llm_bridge = Arc::new(ProviderLLMBridge::new_arc(evo_p));
        core_evo.set_llm_provider(llm_bridge);
        info!("Core evolution LLM provider configured");
    }

    let core_evo_raw = Arc::new(Mutex::new(core_evo));

    // Create adapter handles for the tools crate trait objects
    let cap_registry_adapter = CapabilityRegistryAdapter::new(cap_registry_raw.clone());
    let cap_registry_handle: CapabilityRegistryHandle = Arc::new(Mutex::new(cap_registry_adapter));

    let core_evo_adapter = CoreEvolutionAdapter::new(core_evo_raw.clone());
    let core_evo_handle: CoreEvolutionHandle = Arc::new(Mutex::new(core_evo_adapter));

    if let Some(msg) = message {
        // Single message mode — no need for CronService
        let tool_registry = ToolRegistry::with_defaults();
        let mut runtime = AgentRuntime::new(
            config.clone(),
            paths.clone(),
            Arc::clone(&provider_pool),
            tool_registry,
        )?;
        runtime.mount_mcp_servers().await;
        runtime.validate_intent_router()?;
        runtime.set_agent_id(Some(agent_id.clone()));
        runtime.set_task_manager(TaskManager::with_persistence(
            root_paths.workspace().join("tasks.json"),
        ));

        // 如果配置了独立的 evolution_model 或 evolution_provider，创建独立的 evolution provider
        if config.agents.defaults.evolution_model.is_some()
            || config.agents.defaults.evolution_provider.is_some()
        {
            match super::provider::create_evolution_provider(&config) {
                Ok(evo_provider) => {
                    runtime.set_evolution_provider(evo_provider);
                    info!("Evolution provider configured with independent model");
                }
                Err(e) => {
                    warn!(
                        "Failed to create evolution provider: {}, using main provider",
                        e
                    );
                }
            }
        }

        if let Some(ref store) = memory_store_handle {
            runtime.set_memory_store(store.clone());
        }
        runtime.set_capability_registry(cap_registry_handle.clone());
        runtime.set_core_evolution(core_evo_handle.clone());

        let inbound = InboundMessage {
            channel: "cli".to_string(),
            account_id: None,
            sender_id: "user".to_string(),
            chat_id: session.split(':').nth(1).unwrap_or("default").to_string(),
            content: msg,
            media: vec![],
            metadata: serde_json::Value::Null,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };

        let response = runtime.process_message(inbound).await?;
        println!("{}", response);
    } else {
        // Interactive mode with CronService
        println!("blockcell interactive mode (Ctrl+C to exit)");
        println!("Agent: {}", agent_id);
        println!("Session: {}", session);
        println!("Type /help to see all available commands.");
        println!();

        // Create message bus
        let bus = MessageBus::new(100);
        let ((inbound_tx, inbound_rx), (outbound_tx, mut outbound_rx)) = bus.split();

        // Create shutdown channel
        let (shutdown_tx, _) = broadcast::channel::<()>(1);

        // Create confirmation channel for path safety checks
        let (confirm_tx, mut confirm_rx) = mpsc::channel::<ConfirmRequest>(8);

        // Create shared task manager
        let task_manager = TaskManager::with_persistence(root_paths.workspace().join("tasks.json"));

        // Create channel manager for outbound message dispatch (before config is moved)
        let channel_manager =
            ChannelManager::new(config.clone(), paths.clone(), inbound_tx.clone());

        // Start messaging channels (before config is moved into runtime)
        let mut channel_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        #[cfg(feature = "telegram")]
        for listener in blockcell_channels::account::telegram_listener_configs(&config) {
            let telegram = Arc::new(TelegramChannel::new(listener.config, inbound_tx.clone()));
            let shutdown_rx = shutdown_tx.subscribe();
            channel_handles.push(tokio::spawn(async move {
                telegram.run_loop(shutdown_rx).await;
            }));
        }

        #[cfg(feature = "whatsapp")]
        for listener in blockcell_channels::account::whatsapp_listener_configs(&config) {
            let whatsapp = Arc::new(WhatsAppChannel::new(listener.config, inbound_tx.clone()));
            let shutdown_rx = shutdown_tx.subscribe();
            channel_handles.push(tokio::spawn(async move {
                whatsapp.run_loop(shutdown_rx).await;
            }));
        }

        #[cfg(feature = "feishu")]
        for listener in blockcell_channels::account::feishu_scoped_configs(&config) {
            let feishu = Arc::new(FeishuChannel::new(listener.config, inbound_tx.clone()));
            let shutdown_rx = shutdown_tx.subscribe();
            channel_handles.push(tokio::spawn(async move {
                feishu.run_loop(shutdown_rx).await;
            }));
        }

        #[cfg(feature = "slack")]
        for listener in blockcell_channels::account::slack_listener_configs(&config) {
            let slack = Arc::new(SlackChannel::new(listener.config, inbound_tx.clone()));
            let shutdown_rx = shutdown_tx.subscribe();
            channel_handles.push(tokio::spawn(async move {
                slack.run_loop(shutdown_rx).await;
            }));
        }

        #[cfg(feature = "discord")]
        for listener in blockcell_channels::account::discord_listener_configs(&config) {
            let discord = Arc::new(DiscordChannel::new(listener.config, inbound_tx.clone()));
            let shutdown_rx = shutdown_tx.subscribe();
            channel_handles.push(tokio::spawn(async move {
                discord.run_loop(shutdown_rx).await;
            }));
        }

        #[cfg(feature = "dingtalk")]
        for listener in blockcell_channels::account::dingtalk_listener_configs(&config) {
            let dingtalk = Arc::new(DingTalkChannel::new(listener.config, inbound_tx.clone()));
            let shutdown_rx = shutdown_tx.subscribe();
            channel_handles.push(tokio::spawn(async move {
                dingtalk.run_loop(shutdown_rx).await;
            }));
        }

        #[cfg(feature = "wecom")]
        for listener in blockcell_channels::account::wecom_listener_configs(&config) {
            let wecom = Arc::new(WeComChannel::new(listener.config, inbound_tx.clone()));
            let shutdown_rx = shutdown_tx.subscribe();
            channel_handles.push(tokio::spawn(async move {
                wecom.run_loop(shutdown_rx).await;
            }));
        }

        // Create agent runtime with outbound channel (consumes config)
        let tool_registry = ToolRegistry::with_defaults();
        let mut runtime = AgentRuntime::new(
            config.clone(),
            paths.clone(),
            Arc::clone(&provider_pool),
            tool_registry,
        )?;
        runtime.mount_mcp_servers().await;
        runtime.validate_intent_router()?;

        // 如果配置了独立的 evolution_model 或 evolution_provider，创建独立的 evolution provider
        if config.agents.defaults.evolution_model.is_some()
            || config.agents.defaults.evolution_provider.is_some()
        {
            match super::provider::create_evolution_provider(&config) {
                Ok(evo_provider) => {
                    runtime.set_evolution_provider(evo_provider);
                    info!("Evolution provider configured with independent model");
                }
                Err(e) => {
                    warn!(
                        "Failed to create evolution provider: {}, using main provider",
                        e
                    );
                }
            }
        }

        runtime.set_outbound(outbound_tx);
        runtime.set_confirm(confirm_tx);
        runtime.set_task_manager(task_manager.clone());
        runtime.set_agent_id(Some(agent_id.clone()));
        if let Some(ref store) = memory_store_handle {
            runtime.set_memory_store(store.clone());
        }
        runtime.set_capability_registry(cap_registry_handle.clone());
        runtime.set_core_evolution(core_evo_handle.clone());

        // Create and start CronService
        let cron_service = Arc::new(CronService::new(paths.clone(), inbound_tx.clone()));
        cron_service.load().await?;

        let cron_handle = {
            let cron = cron_service.clone();
            let shutdown_rx = shutdown_tx.subscribe();
            tokio::spawn(async move {
                cron.run_loop(shutdown_rx).await;
            })
        };

        // Spawn runtime loop
        let runtime_handle = tokio::spawn(async move {
            runtime.run_loop(inbound_rx, None).await;
        });

        // Split outbound: channel messages go to ChannelManager, CLI messages go to printer
        let (printer_tx, mut printer_rx) = mpsc::channel(100);
        let outbound_dispatch_handle = tokio::spawn(async move {
            while let Some(msg) = outbound_rx.recv().await {
                match msg.channel.as_str() {
                    "cli" | "cron" => {
                        let _ = printer_tx.send(msg).await;
                    }
                    _ => {
                        // Dispatch to external channel (Telegram/Slack/Discord/etc.)
                        if let Err(e) = channel_manager.dispatch_outbound_msg(&msg).await {
                            tracing::error!(error = %e, channel = %msg.channel, "Failed to dispatch outbound message");
                        }
                    }
                }
            }
        });

        // Spawn outbound printer — prints responses from CLI and cron jobs
        let printer_handle = tokio::spawn(async move {
            while let Some(msg) = printer_rx.recv().await {
                if msg.channel == "cron" {
                    println!("\n[cron] {}", msg.content);
                } else {
                    println!("\n{}", msg.content);
                }
                println!();
                print!("> ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
        });

        // Channel for the confirm handler to send a oneshot::Sender to the stdin thread,
        // so the stdin thread can route the next line of input as a confirmation response.
        let (confirm_answer_tx, confirm_answer_rx) =
            std::sync::mpsc::channel::<tokio::sync::oneshot::Sender<bool>>();

        // Spawn confirmation handler — receives ConfirmRequest from runtime,
        // prints the prompt, and delegates the actual stdin read to the stdin thread.
        let confirm_handle = tokio::spawn(async move {
            while let Some(request) = confirm_rx.recv().await {
                // Print confirmation prompt
                eprintln!();
                eprintln!("⚠️  Security confirmation: tool `{}` requests access to paths outside workspace:", request.tool_name);
                for p in &request.paths {
                    eprintln!("   📁 {}", p);
                }
                eprint!("Allow? (y/n): ");
                let _ = std::io::Write::flush(&mut std::io::stderr());

                // Send the response channel to the stdin thread so it can answer
                if confirm_answer_tx.send(request.response_tx).is_err() {
                    break;
                }
            }
        });

        // Single stdin reader thread — routes input to either message or confirmation.
        // The confirm handler prints the prompt and sends a oneshot::Sender here.
        // After each read_line, we check if a confirmation is pending and route accordingly.
        // Clone paths for the stdin thread (needed for skill management commands)
        let stdin_paths = paths.clone();

        let stdin_tx = inbound_tx.clone();
        let session_clone = session.clone();
        let stdin_task_manager = task_manager.clone();
        let stdin_handle = tokio::task::spawn_blocking(move || {
            use std::io::{BufRead, Write};
            let stdin = std::io::stdin();
            let mut stdout = std::io::stdout();
            // Create a small tokio runtime for blocking task manager queries
            let local_rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to create local runtime for stdin");

            loop {
                print!("> ");
                let _ = stdout.flush();

                let mut raw_input = String::new();
                match stdin.lock().read_line(&mut raw_input) {
                    Ok(0) => break, // EOF (Ctrl+D)
                    Ok(_) => {}
                    Err(_) => continue, // Non-UTF-8 or other read error — skip and re-prompt
                }

                // After reading a line, check if a confirmation request arrived
                // (it may have arrived while we were blocked on read_line)
                if let Ok(response_tx) = confirm_answer_rx.try_recv() {
                    let answer = raw_input.trim().to_lowercase();
                    let allowed = answer == "y" || answer == "yes";
                    if allowed {
                        eprintln!("✅ Access granted");
                    } else {
                        eprintln!("❌ Access denied");
                    }
                    eprintln!();
                    let _ = response_tx.send(allowed);
                    continue;
                }

                let input = raw_input.trim().to_string();
                if input.is_empty() {
                    continue;
                }

                if input == "/quit" || input == "/exit" {
                    break;
                }

                // /help — print all available slash commands
                if input == "/help" {
                    println!();
                    println!("Available commands:");
                    println!("  /help               Show this help");
                    println!("  /tasks              List background tasks");
                    println!("  /skills             List skills and evolution status");
                    println!("  /tools              List all registered tools");
                    println!("  /learn <desc>       Learn a new skill by description");
                    println!("  /clear              Clear current session history");
                    println!("  /clear-skills       Clear all skill evolution records");
                    println!("  /forget-skill <n>   Delete records for a specific skill");
                    println!("  /quit  /exit        Exit interactive mode");
                    println!();
                    continue;
                }

                // /clear — clear current session history (acknowledged locally)
                if input == "/clear" {
                    println!("  Session history cleared. (Note: server-side history persists in memory store)");
                    println!();
                    continue;
                }

                // Local /tasks command — query TaskManager directly, no LLM needed
                if input == "/tasks" || input.starts_with("/tasks ") {
                    let tm = &stdin_task_manager;
                    let (queued, running, completed, failed, tasks) = local_rt.block_on(async {
                        let (q, r, c, f) = tm.summary().await;
                        let tasks = tm.list_tasks(None).await;
                        (q, r, c, f, tasks)
                    });
                    println!();
                    println!(
                        "📋 Task overview: {} queued | {} running | {} completed | {} failed",
                        queued, running, completed, failed
                    );
                    if tasks.is_empty() {
                        println!("  (No tasks)");
                    } else {
                        for t in &tasks {
                            let status_icon = match t.status.to_string().as_str() {
                                "queued" => "⏳",
                                "running" => "🔄",
                                "completed" => "✅",
                                "failed" => "❌",
                                _ => "•",
                            };
                            let short_id_str: String = t.id.chars().take(12).collect();
                            let short_id = short_id_str.as_str();
                            println!(
                                "  {} [{}] {} - {}",
                                status_icon, short_id, t.status, t.label
                            );
                            if let Some(ref progress) = t.progress {
                                println!("    Progress: {}", progress);
                            }
                            if let Some(ref result) = t.result {
                                let preview = if result.chars().count() > 100 {
                                    let truncated: String = result.chars().take(100).collect();
                                    format!("{}...", truncated)
                                } else {
                                    result.clone()
                                };
                                println!("    Result: {}", preview);
                            }
                            if let Some(ref err) = t.error {
                                println!("    Error: {}", err);
                            }
                        }
                    }
                    println!();
                    continue;
                }

                // /skills — list skill evolution status (local, no LLM)
                if input == "/skills" || input.starts_with("/skills ") {
                    print_skills_status(&stdin_paths);
                    continue;
                }

                // /tools — list registered tools (local, no LLM)
                if input == "/tools" {
                    print_tools_status(&stdin_paths);
                    continue;
                }

                // /clear-skills — clear all evolution records
                if input == "/clear-skills" {
                    clear_all_skill_records(&stdin_paths);
                    continue;
                }

                // /forget-skill <name> — delete records for a specific skill
                if input.starts_with("/forget-skill ") {
                    let skill_name = input.trim_start_matches("/forget-skill ").trim();
                    if skill_name.is_empty() {
                        println!("  Usage: /forget-skill <skill_name>");
                    } else {
                        delete_skill_records(&stdin_paths, skill_name);
                    }
                    println!();
                    continue;
                }

                // /learn <description> — send a learn request to the LLM
                if input.starts_with("/learn ") {
                    let description = input.trim_start_matches("/learn ").trim();
                    if description.is_empty() {
                        println!("  Usage: /learn <skill description>");
                        println!();
                        continue;
                    }
                    // Frame the message so the LLM understands it's a skill learning request
                    let learn_msg = format!(
                        "Please learn the following skill: {}\n\n\
                        If this skill is already learned (has a record in list_skills query=learned), just tell me it's done.\n\
                        Otherwise, start learning this skill and report progress.",
                        description
                    );
                    let inbound = InboundMessage {
                        channel: "cli".to_string(),
                        account_id: None,
                        sender_id: "user".to_string(),
                        chat_id: session_clone
                            .split(':')
                            .nth(1)
                            .unwrap_or("default")
                            .to_string(),
                        content: learn_msg,
                        media: vec![],
                        metadata: serde_json::Value::Null,
                        timestamp_ms: chrono::Utc::now().timestamp_millis(),
                    };
                    if stdin_tx.blocking_send(inbound).is_err() {
                        break;
                    }
                    continue;
                }

                // Extract image paths from input for multimodal support
                let (text, media) = extract_media_from_input(&input);
                if !media.is_empty() {
                    eprintln!("  📎 Detected {} image(s)", media.len());
                }
                let inbound = InboundMessage {
                    channel: "cli".to_string(),
                    account_id: None,
                    sender_id: "user".to_string(),
                    chat_id: session_clone
                        .split(':')
                        .nth(1)
                        .unwrap_or("default")
                        .to_string(),
                    content: if media.is_empty() { input } else { text },
                    media,
                    metadata: serde_json::Value::Null,
                    timestamp_ms: chrono::Utc::now().timestamp_millis(),
                };

                if stdin_tx.blocking_send(inbound).is_err() {
                    break;
                }
            }
        });

        // Wait for stdin to finish (user typed /quit or Ctrl+D)
        let _ = stdin_handle.await;

        info!("Shutting down agent...");
        let _ = shutdown_tx.send(());

        // Drop inbound_tx to close the channel and stop runtime
        drop(inbound_tx);

        let mut handles: Vec<tokio::task::JoinHandle<()>> = vec![
            runtime_handle,
            cron_handle,
            printer_handle,
            confirm_handle,
            outbound_dispatch_handle,
        ];
        handles.extend(channel_handles);

        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            futures::future::join_all(handles),
        )
        .await;
    }

    Ok(())
}

/// Scan a directory for skill subdirectories and collect (name, description) pairs.
fn scan_skill_dirs(dir: &std::path::Path) -> Vec<(String, String)> {
    let mut skills = Vec::new();
    if !dir.is_dir() {
        return skills;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            // Must have SKILL.rhai or SKILL.md
            if !p.join("SKILL.rhai").exists() && !p.join("SKILL.md").exists() {
                continue;
            }
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            // Try to read description from meta.yaml
            let desc = p
                .join("meta.yaml")
                .exists()
                .then(|| std::fs::read_to_string(p.join("meta.yaml")).ok())
                .flatten()
                .and_then(|content| {
                    // Simple extraction: look for "description:" line
                    for line in content.lines() {
                        let trimmed = line.trim();
                        if trimmed.starts_with("description:") {
                            let val = trimmed.trim_start_matches("description:").trim();
                            // Strip surrounding quotes
                            let val = val.trim_matches('"').trim_matches('\'');
                            if !val.is_empty() {
                                return Some(val.to_string());
                            }
                        }
                    }
                    None
                })
                .unwrap_or_default();
            skills.push((name, desc));
        }
    }
    skills.sort_by(|a, b| a.0.cmp(&b.0));
    skills
}

/// Skill domain categories for grouping skills in /skills display.
const SKILL_CATEGORIES: &[(&str, &[&str])] = &[
    (
        "💰 Finance",
        &[
            "stock_monitor",
            "stock_screener",
            "bond_monitor",
            "futures_monitor",
            "futures_strategy",
            "portfolio_advisor",
            "macro_monitor",
            "daily_finance_report",
        ],
    ),
    (
        "⛓️ Blockchain/DeFi",
        &[
            "crypto_research",
            "crypto_onchain",
            "crypto_sentiment",
            "crypto_tax",
            "quant_crypto",
            "defi_analysis",
            "nft_analysis",
            "dao_analysis",
            "token_security",
            "contract_audit",
            "wallet_security",
            "whale_tracker",
            "address_monitor",
            "treasury_management",
        ],
    ),
    (
        "📧 Email",
        &[
            "email_digest",
            "email_auto_reply",
            "email_cleanup",
            "email_backup",
            "email_report",
            "email_to_tasks",
        ],
    ),
    ("🖥️ GUI Automation", &["app_control", "camera"]),
    (
        "📅 Productivity",
        &[
            "daily_digest",
            "weekly_review",
            "calendar_manager",
            "calendar_reminders",
            "personal_life",
            "smart_home",
            "learning_assistant",
        ],
    ),
    (
        "🔧 DevOps",
        &[
            "dev_workflow",
            "dev_security",
            "devops_monitor",
            "log_monitor",
            "site_monitor",
            "security_privacy",
        ],
    ),
    ("📰 Content", &["news_monitor", "content_creator"]),
    ("🏢 Business", &["business_ops"]),
];

/// Print skill status (local filesystem operation, no LLM needed).
/// Shows skill directories grouped by domain + evolution records.
fn print_skills_status(paths: &Paths) {
    use blockcell_skills::evolution::EvolutionRecord;

    let records_dir = paths.workspace().join("evolution_records");

    // Collect all skills from built-in and workspace dirs
    let builtin_skills = scan_skill_dirs(&paths.builtin_skills_dir());
    let workspace_skills = scan_skill_dirs(&paths.skills_dir());

    // Merge: workspace overrides built-in
    let mut skill_map: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for (name, desc) in &builtin_skills {
        skill_map.insert(name.clone(), desc.clone());
    }
    for (name, desc) in &workspace_skills {
        skill_map.insert(name.clone(), desc.clone());
    }

    println!();
    println!("🧠 Skills ({} total)", skill_map.len());

    // Group skills by category
    let mut categorized = std::collections::HashSet::new();

    for (category, skill_names) in SKILL_CATEGORIES {
        let mut items: Vec<(&str, &str)> = Vec::new();
        for &sn in *skill_names {
            if let Some(desc) = skill_map.get(sn) {
                items.push((sn, desc.as_str()));
                categorized.insert(sn.to_string());
            }
        }
        if !items.is_empty() {
            println!();
            println!("  {} ({})", category, items.len());
            for (name, desc) in &items {
                if desc.is_empty() {
                    println!("    • {}", name);
                } else {
                    // Truncate long descriptions
                    let char_count = desc.chars().count();
                    if char_count > 40 {
                        let short: String = desc.chars().take(40).collect();
                        println!("    • {} — {}…", name, short);
                    } else {
                        println!("    • {} — {}", name, desc);
                    }
                }
            }
        }
    }

    // Show uncategorized skills (user-created or newly added)
    let uncategorized: Vec<_> = skill_map
        .iter()
        .filter(|(name, _)| !categorized.contains(name.as_str()))
        .collect();
    if !uncategorized.is_empty() {
        println!();
        println!("  📦 Other ({})", uncategorized.len());
        for (name, desc) in &uncategorized {
            if desc.is_empty() {
                println!("    • {}", name);
            } else {
                let char_count = desc.chars().count();
                if char_count > 40 {
                    let short: String = desc.chars().take(40).collect();
                    println!("    • {} — {}…", name, short);
                } else {
                    println!("    • {} — {}", name, desc);
                }
            }
        }
    }

    // --- Evolution records (learned / learning / failed) ---
    let mut records: Vec<EvolutionRecord> = Vec::new();
    if records_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&records_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(record) = serde_json::from_str::<EvolutionRecord>(&content) {
                            records.push(record);
                        }
                    }
                }
            }
        }
    }
    records.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    let mut seen = std::collections::HashSet::new();
    let mut learning = Vec::new();
    let mut learned = Vec::new();
    let mut failed = Vec::new();

    for r in &records {
        if is_builtin_tool(&r.skill_name) {
            continue;
        }
        if !seen.insert(r.skill_name.clone()) {
            continue;
        }
        let status_str = format!("{:?}", r.status);
        match status_str.as_str() {
            "Completed" => learned.push(r),
            "Failed" | "RolledBack" | "AuditFailed" | "DryRunFailed" | "TestFailed" => {
                failed.push(r)
            }
            _ => learning.push(r),
        }
    }

    if !learned.is_empty() || !learning.is_empty() || !failed.is_empty() {
        println!();
        println!("  ── Evolution Status ──");
    }

    if !learned.is_empty() {
        println!("  ✅ Learned ({}):", learned.len());
        for r in &learned {
            println!(
                "    • {} ({})",
                r.skill_name,
                format_timestamp(r.created_at)
            );
        }
    }

    if !learning.is_empty() {
        println!("  🔄 Learning ({}):", learning.len());
        for r in &learning {
            let status_desc = match format!("{:?}", r.status).as_str() {
                "Triggered" => "pending",
                "Generating" => "generating",
                "Generated" => "generated",
                "Auditing" => "auditing",
                "AuditPassed" => "audit passed",
                "CompilePassed" | "DryRunPassed" | "TestPassed" => "compile passed",
                "CompileFailed" | "DryRunFailed" | "TestFailed" | "Testing" => "compile failed",
                "Observing" | "RollingOut" => "observing",
                _ => "in progress",
            };
            println!(
                "    • {} [{}] ({})",
                r.skill_name,
                status_desc,
                format_timestamp(r.created_at)
            );
        }
    }

    if !failed.is_empty() {
        println!("  ❌ Failed ({}):", failed.len());
        for r in &failed {
            println!(
                "    • {} ({})",
                r.skill_name,
                format_timestamp(r.created_at)
            );
        }
    }

    let builtin_err_count = records
        .iter()
        .filter(|r| is_builtin_tool(&r.skill_name))
        .count();
    if builtin_err_count > 0 {
        println!();
        println!(
            "  ℹ️  {} built-in tool error records hidden (/clear-skills to clean up)",
            builtin_err_count
        );
    }

    println!();
    println!("  💡 /tools view tools | /learn <desc> learn new skill");
    println!();
}

/// Clear all evolution records from disk.
fn clear_all_skill_records(paths: &Paths) {
    let records_dir = paths.workspace().join("evolution_records");
    let mut count = 0;

    if records_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&records_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json")
                    && std::fs::remove_file(&path).is_ok()
                {
                    count += 1;
                }
            }
        }
    }

    println!();
    if count > 0 {
        println!("  ✅ Cleared all skill evolution records ({} total)", count);
    } else {
        println!("  (No records to clear)");
    }
    println!();
}

/// Delete evolution records for a specific skill name.
fn delete_skill_records(paths: &Paths, skill_name: &str) {
    use blockcell_skills::evolution::EvolutionRecord;

    let records_dir = paths.workspace().join("evolution_records");
    let mut count = 0;

    if records_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&records_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(record) = serde_json::from_str::<EvolutionRecord>(&content) {
                            if record.skill_name == skill_name
                                && std::fs::remove_file(&path).is_ok()
                            {
                                count += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    println!();
    if count > 0 {
        println!(
            "  ✅ Deleted all records for skill `{}` ({} total)",
            skill_name, count
        );
    } else {
        println!("  ⚠️  No records found for skill `{}`", skill_name);
    }
}

/// Print registered tools status (local, no LLM needed).
/// Shows ALL built-in tools grouped by category + dynamic evolved tools.
fn print_tools_status(paths: &Paths) {
    use blockcell_core::CapabilityDescriptor;

    // Count total tools
    let total_tools: usize = BUILTIN_TOOLS.iter().map(|(_, items)| items.len()).sum();

    println!();
    println!(
        "🔌 Built-in tools ({} total, {} categories)",
        total_tools,
        BUILTIN_TOOLS.len()
    );

    for (category, items) in BUILTIN_TOOLS {
        println!();
        println!("  {} ({})", category, items.len());
        for (name, desc) in *items {
            println!("    ✅ {} — {}", name, desc);
        }
    }

    // Dynamic evolved tools from evolved_tools.json
    let cap_file = paths
        .workspace()
        .join("evolved_tools")
        .join("evolved_tools.json");
    if cap_file.exists() {
        if let Ok(content) = std::fs::read_to_string(&cap_file) {
            if let Ok(caps) = serde_json::from_str::<Vec<CapabilityDescriptor>>(&content) {
                if !caps.is_empty() {
                    let active = caps.iter().filter(|c| c.is_available()).count();
                    println!();
                    println!(
                        "  🧬 Dynamic evolved tools ({}, {} available)",
                        caps.len(),
                        active
                    );
                    for cap in &caps {
                        let icon = match format!("{:?}", cap.status).as_str() {
                            "Active" => "✅",
                            "Available" | "Discovered" => "�",
                            "Loading" | "Evolving" => "⏳",
                            _ => "❌",
                        };
                        println!(
                            "    {} {} v{} — {}",
                            icon, cap.id, cap.version, cap.description
                        );
                    }
                }
            }
        }
    }

    // Core evolution records
    let evo_dir = paths.workspace().join("tool_evolution_records");
    if evo_dir.exists() {
        let mut evo_count = 0;
        let mut active_count = 0;
        if let Ok(entries) = std::fs::read_dir(&evo_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|e| e == "json") {
                    evo_count += 1;
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        if content.contains("\"Active\"") {
                            active_count += 1;
                        }
                    }
                }
            }
        }
        if evo_count > 0 {
            println!();
            println!(
                "  🧬 Core evolution: {} records ({} active)",
                evo_count, active_count
            );
        }
    }

    println!();
    println!("  💡 /skills view skills | capability_evolve tool to learn new tools");
    println!();
}

/// Format a Unix timestamp to a human-readable string.
fn format_timestamp(ts: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%m-%d %H:%M").to_string(),
        _ => "unknown".to_string(),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::config::AgentProfileConfig;
    use std::path::PathBuf;

    #[test]
    fn test_resolve_agent_context_defaults_to_default_agent() {
        let config = Config::default();
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell"));

        let resolved = resolve_agent_context(&config, &paths, None, None)
            .expect("default agent should resolve");

        assert_eq!(resolved.agent_id, "default");
        assert_eq!(resolved.session, "cli:default");
        assert_eq!(resolved.paths.workspace(), PathBuf::from("/tmp/blockcell/workspace"));
    }

    #[test]
    fn test_resolve_agent_context_uses_named_agent_paths_and_session() {
        let mut config = Config::default();
        config.agents.list.push(AgentProfileConfig {
            id: "ops".to_string(),
            enabled: true,
            model: Some("deepseek-chat".to_string()),
            provider: Some("deepseek".to_string()),
            ..AgentProfileConfig::default()
        });
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell"));

        let resolved = resolve_agent_context(&config, &paths, Some("ops"), None)
            .expect("named agent should resolve");

        assert_eq!(resolved.agent_id, "ops");
        assert_eq!(resolved.session, "cli:ops");
        assert_eq!(resolved.paths.workspace(), PathBuf::from("/tmp/blockcell/agents/ops/workspace"));
        assert_eq!(resolved.config.agents.defaults.provider.as_deref(), Some("deepseek"));
        assert_eq!(resolved.config.agents.defaults.model, "deepseek-chat");
    }

    #[test]
    fn test_resolve_agent_context_preserves_explicit_session() {
        let mut config = Config::default();
        config.agents.list.push(AgentProfileConfig {
            id: "ops".to_string(),
            enabled: true,
            ..AgentProfileConfig::default()
        });
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell"));

        let resolved = resolve_agent_context(&config, &paths, Some("ops"), Some("custom:thread"))
            .expect("named agent with explicit session should resolve");

        assert_eq!(resolved.session, "custom:thread");
    }

    #[test]
    fn test_resolve_agent_context_rejects_unknown_agent() {
        let config = Config::default();
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell"));

        let err = resolve_agent_context(&config, &paths, Some("ops"), None)
            .expect_err("unknown agent should fail");

        assert!(err.to_string().contains("Unknown agent 'ops'"));
    }
}
