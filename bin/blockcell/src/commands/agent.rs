use blockcell_agent::{
    AgentRuntime, CapabilityRegistryAdapter, CheckpointManager, ConfirmRequest,
    CoreEvolutionAdapter, MemoryStoreAdapter, MessageBus, ProviderLLMBridge, ResponseCache,
    ResponseCacheConfig, TaskManager,
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
use blockcell_scheduler::{
    CronService, DreamService, DreamServiceConfig, EvolutionWorker, SkillEvolutionWorker,
};
use blockcell_skills::{new_registry_handle, CoreEvolution, EvolutionServiceConfig};
use blockcell_storage::EvolutionWorkflowStore;
use blockcell_tools::mcp::manager::McpManager;
use blockcell_tools::{
    build_tool_registry_for_agent_config, CapabilityRegistryHandle, CoreEvolutionHandle,
    MemoryStoreHandle,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{info, warn};

mod setup;
mod tui;

use super::memory_store::open_memory_store;
use super::slash_commands::{CommandContext, CommandResult, SLASH_COMMAND_HANDLER};
use setup::{
    build_pool_with_overrides, create_skill_evolution_llm_provider, extract_media_from_input,
    resolve_agent_context,
};
use tui::{clear_prompt_line, read_line_with_command_picker, restore_prompt_line, short_task_id};

pub async fn run(
    message: Option<String>,
    agent: Option<String>,
    session: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    background: bool,
    forced_interaction_mode: Option<blockcell_agent::InteractionMode>,
) -> anyhow::Result<()> {
    let mut root_paths = Paths::new();
    super::env_file::ensure_and_load_blockcell_env(&root_paths)?;
    let root_config = Config::load_or_default(&root_paths)?;
    root_paths.apply_workspace_config(&root_config.agents.defaults.workspace);

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

    // 同步 BLOCKCELL_WORKSPACE 环境变量，供 channel listener 等模块读取 media 目录。
    // 必须在 resolve_agent_context 之后设置：命名 agent（如 --agent ops）的 workspace
    // 与 root_paths 不同，媒体文件应下载到该 agent 自己的 workspace/media。
    std::env::set_var("BLOCKCELL_WORKSPACE", paths.workspace());
    let mut config = resolved.config;
    let sandbox_needed = config.tools.exec.sandbox_policy.as_deref() != Some("full-access");
    let sandbox_unavailable = cfg!(target_os = "linux")
        && sandbox_needed
        && !blockcell_tools::sandbox::native_sandbox_available();
    if sandbox_unavailable {
        eprintln!(
            "Warning: Linux sandbox unavailable; install bubblewrap with `sudo apt install bubblewrap` (or use dnf/pacman). Shell execution will require approval."
        );
    }
    let mcp_manager = Arc::new(McpManager::load(&root_paths).await?);
    let provider_pool = build_pool_with_overrides(&mut config, model, provider)?;

    // Ensure builtin skills are extracted outside the project (silent, skips existing)
    let _ = super::embedded_skills::extract_to_directory(&paths.managed_builtin_skills_dir());

    // Initialize memory store (SQLite + FTS5)
    let memory_store_handle: Option<MemoryStoreHandle> = match open_memory_store(&paths, &config) {
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
    let cap_registry_handle: CapabilityRegistryHandle = Arc::new(cap_registry_adapter);

    let core_evo_adapter = CoreEvolutionAdapter::new(core_evo_raw.clone());
    let core_evo_handle: CoreEvolutionHandle = Arc::new(Mutex::new(core_evo_adapter));

    // 创建核心进化工作流存储和 worker（在 if/else 之前创建，两个分支都需要）
    let evo_workflow_db = paths.evolution_workflow_db();
    let evo_workflow_store = EvolutionWorkflowStore::open(&evo_workflow_db)?;
    let evo_workflow_store_arc = Arc::new(evo_workflow_store);
    let evo_worker = EvolutionWorker::new((*evo_workflow_store_arc).clone(), core_evo_raw.clone());
    let evo_worker_arc = Arc::new(evo_worker);

    let skill_evo_llm_provider = create_skill_evolution_llm_provider(&config, &provider_pool);
    let skill_evo_workflow_db = paths.skill_evolution_workflow_db();
    let skill_evo_workflow_store = EvolutionWorkflowStore::open(&skill_evo_workflow_db)?;
    // 从 Config.evolution 转换配置，而非使用默认值
    let skill_evo_config: EvolutionServiceConfig = config.evolution.clone().into();
    let mut skill_evo_worker = SkillEvolutionWorker::new(
        skill_evo_workflow_store,
        paths.skills_dir(),
        skill_evo_config,
        skill_evo_llm_provider,
    );
    // 为 SkillEvolutionWorker 设置部署回调，确保 scheduler worker 的进化部署路径也能触发 ghost learning boundary
    if let Some(callback) = blockcell_agent::create_evolution_deploy_callback(&config, &paths) {
        skill_evo_worker.set_deploy_callback(callback);
        info!("[evolution-deploy-callback] 已连接到 SkillEvolutionWorker EvolutionService");
    }
    let skill_evo_worker_arc = Arc::new(skill_evo_worker);

    if let Some(msg) = message {
        // Single message mode — no need for CronService
        let tool_registry =
            build_tool_registry_for_agent_config(&config, Some(&mcp_manager)).await?;
        let mut runtime = AgentRuntime::new(
            config.clone(),
            paths.clone(),
            Arc::clone(&provider_pool),
            tool_registry,
        )?;
        runtime.set_background_tasks_enabled(background);
        runtime.set_shell_execution_unavailable(sandbox_unavailable);
        runtime.set_forced_interaction_mode(forced_interaction_mode);
        runtime.set_one_shot_cli_auto_coding(true);
        runtime.validate_intent_router()?;
        runtime.set_agent_id(Some(agent_id.clone()));
        runtime.set_task_manager(TaskManager::new());

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
        if let Err(e) = runtime.init_memory_file_store() {
            warn!(error = %e, "Failed to initialize file memory store");
        }
        if let Err(e) = runtime.init_skill_file_store() {
            warn!(error = %e, "Failed to initialize skill file store");
        }

        runtime.set_capability_registry(cap_registry_handle.clone());
        runtime.set_core_evolution(core_evo_handle.clone());

        // 设置核心进化工作流存储和 worker
        runtime.set_evolution_workflow_store(evo_workflow_store_arc.clone());
        runtime.set_evolution_worker(
            evo_worker_arc.clone() as Arc<dyn blockcell_agent::EvolutionNotifier>
        );
        runtime.set_skill_evolution_worker(
            skill_evo_worker_arc.clone() as Arc<dyn blockcell_agent::EvolutionNotifier>
        );

        // Initialize Layer 5 memory injector (7-layer memory system)
        if let Err(e) = runtime.init_memory_injector().await {
            warn!(error = %e, "Failed to initialize memory injector");
        }

        // Create event broadcast channel for streaming output
        // 容量 2048：避免长 streaming 响应（大量 token 事件）导致 receiver Lagged
        let (event_tx, mut event_rx) = broadcast::channel::<String>(2048);
        runtime.set_event_tx(event_tx.clone());

        // Spawn event handler for streaming token output
        let event_handler = tokio::spawn(async move {
            use std::io::Write;
            let mut stdout = std::io::stdout();
            let mut emitted_text_delta = false;
            let mut emitted_thinking = false;
            loop {
                match event_rx.recv().await {
                    Ok(event_str) => {
                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&event_str) {
                            let event_type =
                                event.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            match event_type {
                                "token" => {
                                    if let Some(delta) = event.get("delta").and_then(|v| v.as_str())
                                    {
                                        // 在 thinking 之后、第一个 token 之前插入换行
                                        if emitted_thinking {
                                            println!();
                                            emitted_thinking = false;
                                        }
                                        emitted_text_delta = true;
                                        print!("{}", delta);
                                        let _ = stdout.flush();
                                    }
                                }
                                "thinking" => {
                                    if let Some(content) =
                                        event.get("content").and_then(|v| v.as_str())
                                    {
                                        emitted_thinking = true;
                                        print!("{}", content);
                                        let _ = stdout.flush();
                                    }
                                }
                                "tool_call_start" => {
                                    if let Some(tool) = event.get("tool").and_then(|v| v.as_str()) {
                                        eprintln!("\n🔧 Calling tool: {}...", tool);
                                    }
                                }
                                "message_done" => {
                                    if !emitted_text_delta {
                                        if let Some(content) =
                                            event.get("content").and_then(|v| v.as_str())
                                        {
                                            if !content.is_empty() {
                                                println!("\n{}", content);
                                            }
                                        }
                                    }
                                    println!();
                                    emitted_text_delta = false;
                                    emitted_thinking = false;
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // Receiver 落后于发送者，跳过 n 条消息，继续接收
                        tracing::warn!(skipped = n, "Event receiver lagged, skipping messages");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        // 所有 sender 已关闭，退出循环
                        break;
                    }
                }
            }
        });

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
        runtime.wait_for_background_tasks().await;
        if let Some(error) = runtime.take_last_turn_llm_failure() {
            event_handler.abort();
            eprintln!("[blockcell] llm_failed: {error}");
            std::process::exit(2);
        }
        if let Some(command) = runtime.last_verification_command() {
            eprintln!("[blockcell] verification: verified ({command}, exit 0)");
        } else {
            eprintln!("[blockcell] verification: unverified");
        }
        // Event handler already printed streaming output, just print final newline if needed
        if !response.is_empty() {
            println!();
        }
        // Clean up event handler
        event_handler.abort();

        // 单次模式驱动 skill evolution pipeline tick，
        // 确保消息处理期间触发的 notify() 有消费者，pipeline 能推进。
        // Bounded loop：单次消息可能触发多个 evolution workflow，drain 到无可 claim 为止。
        if background {
            let skill_evo_worker_clone = skill_evo_worker_arc.clone();
            let _ = tokio::spawn(async move {
                let max_ticks = 10; // 防止无限循环
                for _ in 0..max_ticks {
                    if !skill_evo_worker_clone.tick().await {
                        break; // 无可 claim 的 workflow，退出
                    }
                }
            })
            .await;
        }
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

        // Create shared task manager with workspace and progress channel for persistence
        let (progress_tx, mut progress_rx) = mpsc::channel::<blockcell_agent::AgentProgress>(100);
        let task_manager =
            TaskManager::with_workspace_and_progress(&paths.workspace(), progress_tx);

        // Restore unfinished tasks from disk
        let restored = task_manager.restore_from_disk(&paths.workspace()).await;
        if restored > 0 {
            info!("Restored {} unfinished tasks from disk", restored);
        }

        // Start periodic cleanup of evicted tasks (with file cleanup)
        let cleanup_handle = Arc::new(task_manager.clone())
            .spawn_cleanup_loop(&paths.workspace(), shutdown_tx.subscribe());

        // 启动进度事件监听：在控制台打印任务阶段进度
        tokio::spawn(async move {
            use blockcell_agent::AgentProgress;
            while let Some(progress) = progress_rx.recv().await {
                match progress {
                    AgentProgress::Stage {
                        task_id,
                        stage,
                        percent,
                    } => {
                        let short_id = short_task_id(&task_id, 8);
                        if percent > 0 {
                            eprintln!("[{}] {} ({}%)", short_id, stage, percent);
                        } else {
                            eprintln!("[{}] {}", short_id, stage);
                        }
                    }
                    AgentProgress::Delta { .. } => {
                        // Delta 事件在控制台不打印（太频繁）
                    }
                    AgentProgress::Notification(_) => {
                        // Notification 由其他机制处理
                    }
                    AgentProgress::ToolCallStart { .. } | AgentProgress::ToolCallEnd { .. } => {
                        // 工具调用事件通过 event_tx (broadcast) 处理，此处忽略
                    }
                }
            }
        });

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
                if let Err(error) = whatsapp.run_loop(shutdown_rx).await {
                    tracing::error!(%error, "WhatsApp listener failed");
                }
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

        #[cfg(feature = "weixin")]
        for listener in blockcell_channels::account::weixin_listener_configs(&config) {
            let weixin = Arc::new(blockcell_channels::weixin::WeixinChannel::new(
                listener.config,
                inbound_tx.clone(),
            ));
            let shutdown_rx = shutdown_tx.subscribe();
            channel_handles.push(tokio::spawn(async move {
                weixin.run_loop(shutdown_rx).await;
            }));
        }

        // Create agent runtime with outbound channel (consumes config)
        let tool_registry =
            build_tool_registry_for_agent_config(&config, Some(&mcp_manager)).await?;
        let mut runtime = AgentRuntime::new(
            config.clone(),
            paths.clone(),
            Arc::clone(&provider_pool),
            tool_registry,
        )?;
        runtime.set_shell_execution_unavailable(sandbox_unavailable);
        runtime.set_forced_interaction_mode(forced_interaction_mode);
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

        // Create event broadcast channel for streaming output
        // 容量 2048：避免长 streaming 响应（大量 token 事件）导致 receiver Lagged
        let (event_tx, mut event_rx) = broadcast::channel::<String>(2048);

        runtime.set_outbound(outbound_tx);
        runtime.set_confirm(confirm_tx);
        runtime.set_task_manager(task_manager.clone());
        runtime.set_agent_id(Some(agent_id.clone()));
        let replayed = task_manager
            .replay_restored_failure_notifications(Some(&agent_id))
            .await;
        if replayed > 0 {
            info!(replayed, agent_id = %agent_id, "Queued interrupted task failure notifications");
        }
        runtime.set_event_tx(event_tx.clone());
        if let Some(ref store) = memory_store_handle {
            runtime.set_memory_store(store.clone());
        }
        if let Err(e) = runtime.init_memory_file_store() {
            warn!(error = %e, "Failed to initialize file memory store");
        }
        if let Err(e) = runtime.init_skill_file_store() {
            warn!(error = %e, "Failed to initialize skill file store");
        }

        runtime.set_capability_registry(cap_registry_handle.clone());
        runtime.set_core_evolution(core_evo_handle.clone());

        // 设置核心进化工作流存储和 worker
        runtime.set_evolution_workflow_store(evo_workflow_store_arc.clone());
        runtime.set_evolution_worker(
            evo_worker_arc.clone() as Arc<dyn blockcell_agent::EvolutionNotifier>
        );
        runtime.set_skill_evolution_worker(
            skill_evo_worker_arc.clone() as Arc<dyn blockcell_agent::EvolutionNotifier>
        );

        // Create shared ResponseCache for CLI and runtime
        // This allows the /clear command to clear the in-memory cache
        let response_cache = ResponseCache::with_config(ResponseCacheConfig::from(
            &config.memory.memory_system.layer1,
        ));
        runtime.set_response_cache(response_cache.clone());

        // Initialize Layer 5 memory injector (7-layer memory system)
        if let Err(e) = runtime.init_memory_injector().await {
            warn!(error = %e, "Failed to initialize memory injector");
        }
        runtime.init_runtime_handle();
        runtime.wire_evolution_deploy_callback();

        let event_emitter = runtime.event_emitter_handle();

        // Create and start CronService
        let tick_interval_secs = config.cron_tick_interval_secs;
        let default_timezone = config.default_timezone.as_deref();
        let cron_service = Arc::new(CronService::new_with_options(
            paths.clone(),
            inbound_tx.clone(),
            Some(agent_id.clone()),
            Some(tick_interval_secs),
            default_timezone,
        ));
        cron_service.set_event_emitter(event_emitter);
        cron_service.load().await?;

        let cron_handle = {
            let cron = cron_service.clone();
            let shutdown_rx = shutdown_tx.subscribe();
            tokio::spawn(async move {
                cron.run_loop(shutdown_rx).await;
            })
        };

        // Layer 6: 启动 Dream Service（跨会话知识整合）
        let dream_config = DreamServiceConfig {
            enabled: config.memory.memory_system.layer6.enabled,
            check_interval_secs: config.memory.memory_system.layer6.check_interval_secs,
            time_gate_threshold_hours: config.memory.memory_system.layer6.time_gate_threshold_hours
                as f64,
            session_gate_threshold: config.memory.memory_system.layer6.session_gate_threshold,
            timeout_secs: config.memory.memory_system.layer6.timeout_secs,
            provider_pool: Some(Arc::clone(&provider_pool)),
        };
        let dream_service = DreamService::new(dream_config, paths.clone());
        let dream_shutdown_rx = shutdown_tx.subscribe();
        let _dream_handle = tokio::spawn(async move {
            dream_service.run_loop(dream_shutdown_rx).await;
        });
        info!("[dream] Dream service started for cross-session knowledge consolidation");

        // 共享当前输入行和光标位置状态，用于事件处理器在打印后台结果/进度时
        // 先清除输入行和建议，打印完毕后重新渲染提示（含光标位置）
        let current_input: Arc<std::sync::Mutex<(String, usize)>> =
            Arc::new(std::sync::Mutex::new((String::new(), 0)));

        // Spawn event handler for streaming token output
        let event_handler_handle = {
            let current_input = current_input.clone();
            tokio::spawn(async move {
                use std::io::Write;
                let mut stdout = std::io::stdout();
                // Track whether streaming tokens were emitted for the current response.
                // If true, message_done should NOT reprint the content (avoid duplicate).
                // If false (non-streaming path like skill loops), message_done prints content.
                let mut emitted_text_delta = false;
                let mut emitted_thinking = false;
                loop {
                    match event_rx.recv().await {
                        Ok(event_str) => {
                            if let Ok(event) = serde_json::from_str::<serde_json::Value>(&event_str)
                            {
                                let event_type =
                                    event.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                match event_type {
                                    "token" => {
                                        // Streaming text token - print immediately
                                        if let Some(delta) =
                                            event.get("delta").and_then(|v| v.as_str())
                                        {
                                            // 在 thinking 之后、第一个 token 之前插入换行
                                            if emitted_thinking {
                                                println!();
                                                emitted_thinking = false;
                                            }
                                            emitted_text_delta = true;
                                            print!("{}", delta);
                                            let _ = stdout.flush();
                                        }
                                    }
                                    "thinking" => {
                                        // Thinking/reasoning content
                                        if let Some(content) =
                                            event.get("content").and_then(|v| v.as_str())
                                        {
                                            emitted_thinking = true;
                                            print!("{}", content);
                                            let _ = stdout.flush();
                                        }
                                    }
                                    "tool_call_start" => {
                                        // Tool call started
                                        if let Some(tool) =
                                            event.get("tool").and_then(|v| v.as_str())
                                        {
                                            let summary = event
                                                .get("summary")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            // 如果有 agent_type，说明是子agent的工具调用
                                            let agent_type = event
                                                .get("agent_type")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            let task_id_short = event
                                                .get("task_id")
                                                .and_then(|v| v.as_str())
                                                .map(|s| short_task_id(s, 4))
                                                .unwrap_or_default();
                                            // 清除当前输入行，避免与提示重叠
                                            clear_prompt_line(&current_input, &mut stdout);
                                            if agent_type.is_empty() {
                                                if summary.is_empty() {
                                                    tracing::info!(
                                                        tool = tool,
                                                        "main agent tool call start"
                                                    );
                                                    eprintln!("\n🔧 {}", tool);
                                                } else {
                                                    tracing::info!(
                                                        tool = tool,
                                                        summary = summary,
                                                        "main agent tool call start"
                                                    );
                                                    eprintln!("\n🔧 {}({})", tool, summary);
                                                }
                                            } else if task_id_short.is_empty() {
                                                if summary.is_empty() {
                                                    tracing::info!(
                                                        agent_type = agent_type,
                                                        tool = tool,
                                                        "sub-agent tool call start"
                                                    );
                                                    eprintln!("  🔧 [{}] {}", agent_type, tool);
                                                } else {
                                                    tracing::info!(
                                                        agent_type = agent_type,
                                                        tool = tool,
                                                        summary = summary,
                                                        "sub-agent tool call start"
                                                    );
                                                    eprintln!(
                                                        "  🔧 [{}] {}({})",
                                                        agent_type, tool, summary
                                                    );
                                                }
                                            } else {
                                                if summary.is_empty() {
                                                    tracing::info!(agent_type = agent_type, task_id = %task_id_short, tool = tool, "sub-agent tool call start");
                                                    eprintln!(
                                                        "  🔧 [{}:{}] {}",
                                                        agent_type, task_id_short, tool
                                                    );
                                                } else {
                                                    tracing::info!(agent_type = agent_type, task_id = %task_id_short, tool = tool, summary = summary, "sub-agent tool call start");
                                                    eprintln!(
                                                        "  🔧 [{}:{}] {}({})",
                                                        agent_type, task_id_short, tool, summary
                                                    );
                                                }
                                            }
                                            // 恢复提示行
                                            restore_prompt_line(&current_input, &mut stdout);
                                        }
                                    }
                                    "tool_call_end" => {
                                        // 子 agent 工具调用完成
                                        let agent_type = event
                                            .get("agent_type")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let task_id_short = event
                                            .get("task_id")
                                            .and_then(|v| v.as_str())
                                            .map(|s| short_task_id(s, 4))
                                            .unwrap_or_default();
                                        let tool = event
                                            .get("tool")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let success = event
                                            .get("success")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(true);
                                        if !agent_type.is_empty() && !tool.is_empty() && !success {
                                            clear_prompt_line(&current_input, &mut stdout);
                                            if task_id_short.is_empty() {
                                                tracing::info!(
                                                    agent_type = agent_type,
                                                    tool = tool,
                                                    "sub-agent tool call failed"
                                                );
                                                eprintln!("  ✗ [{}] {} failed", agent_type, tool);
                                            } else {
                                                tracing::info!(agent_type = agent_type, task_id = %task_id_short, tool = tool, "sub-agent tool call failed");
                                                eprintln!(
                                                    "  ✗ [{}:{}] {} failed",
                                                    agent_type, task_id_short, tool
                                                );
                                            }
                                            restore_prompt_line(&current_input, &mut stdout);
                                        }
                                    }
                                    "message_done" => {
                                        // Message complete
                                        // 检查是否是子agent汇总结果
                                        let is_summary = event
                                            .get("summary_for_subagents")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false);
                                        if is_summary {
                                            // 主agent汇总子agent结果，直接打印
                                            if let Some(content) =
                                                event.get("content").and_then(|v| v.as_str())
                                            {
                                                if !content.is_empty() {
                                                    clear_prompt_line(&current_input, &mut stdout);
                                                    tracing::info!("sub-agent summary delivered");
                                                    eprintln!("\n📋 **子agent结果汇总**");
                                                    println!("{}", content);
                                                    eprintln!("--- end ---");
                                                    println!();
                                                    restore_prompt_line(
                                                        &current_input,
                                                        &mut stdout,
                                                    );
                                                    // 标记已输出，防止后续 message_done 重复打印
                                                    emitted_text_delta = true;
                                                }
                                            }
                                        } else {
                                            // For subagent results (background_delivery=true), print the content
                                            // since it wasn't streamed via token events.
                                            // For normal streaming responses, just print a newline.
                                            let is_background = event
                                                .get("background_delivery")
                                                .and_then(|v| v.as_bool())
                                                .unwrap_or(false);
                                            if is_background {
                                                if let Some(content) =
                                                    event.get("content").and_then(|v| v.as_str())
                                                {
                                                    if !content.is_empty() {
                                                        // 获取 agent_type 用于标识来源
                                                        let agent_type = event
                                                            .get("agent_type")
                                                            .and_then(|v| v.as_str())
                                                            .unwrap_or("agent");
                                                        let task_id_short = event
                                                            .get("task_id")
                                                            .and_then(|v| v.as_str())
                                                            .map(|s| short_task_id(s, 8))
                                                            .unwrap_or_default();
                                                        // 清除当前输入行，打印结果，然后恢复提示
                                                        clear_prompt_line(
                                                            &current_input,
                                                            &mut stdout,
                                                        );
                                                        tracing::info!(
                                                            agent_type = agent_type,
                                                            task_id = %task_id_short,
                                                            "sub-agent background result delivered"
                                                        );
                                                        eprintln!(
                                                            "\n--- {} agent [{}] result ---",
                                                            agent_type, task_id_short
                                                        );
                                                        println!("{}", content);
                                                        eprintln!("--- end ---");
                                                        println!();
                                                        restore_prompt_line(
                                                            &current_input,
                                                            &mut stdout,
                                                        );
                                                    }
                                                }
                                            } else {
                                                // Non-streaming response: print content if not already emitted via tokens
                                                if !emitted_text_delta {
                                                    if let Some(content) = event
                                                        .get("content")
                                                        .and_then(|v| v.as_str())
                                                    {
                                                        if !content.is_empty() {
                                                            println!("\n{}", content);
                                                        }
                                                    }
                                                }
                                                println!();
                                                emitted_text_delta = false;
                                                emitted_thinking = false;
                                            }
                                        }
                                    }
                                    "agent_progress" => {
                                        // 子 agent 进度事件
                                        let agent_type = event
                                            .get("agent_type")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("agent");
                                        let task_id_short = event
                                            .get("task_id")
                                            .and_then(|v| v.as_str())
                                            .map(|s| short_task_id(s, 4))
                                            .unwrap_or_default();
                                        let stage = event
                                            .get("stage")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let percent = event
                                            .get("percent")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0);
                                        if !stage.is_empty() {
                                            clear_prompt_line(&current_input, &mut stdout);
                                            let label = if task_id_short.is_empty() {
                                                agent_type.to_string()
                                            } else {
                                                format!("{}:{}", agent_type, task_id_short)
                                            };
                                            // 同时输出到 tracing（写入日志文件）和 eprintln（终端显示）
                                            tracing::info!(
                                                label = %label,
                                                stage = %stage,
                                                percent = percent,
                                                "sub-agent progress"
                                            );
                                            if percent > 0 {
                                                eprintln!("  [{}] {} ({}%)", label, stage, percent);
                                            } else {
                                                eprintln!("  [{}] {}", label, stage);
                                            }
                                            restore_prompt_line(&current_input, &mut stdout);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // Receiver 落后于发送者，跳过 n 条消息，继续接收
                            tracing::warn!(skipped = n, "Event receiver lagged, skipping messages");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            // 所有 sender 已关闭，退出循环
                            break;
                        }
                    }
                }
            })
        };

        // 启动核心进化 worker 后台任务
        let evo_shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            evo_worker_arc.run_loop(evo_shutdown_rx).await;
        });
        let skill_evo_shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            skill_evo_worker_arc.run_loop(skill_evo_shutdown_rx).await;
        });

        // Spawn runtime loop
        let runtime_handle = tokio::spawn(async move {
            runtime.run_loop(inbound_rx, None).await;
        });

        // Split outbound: channel messages go to ChannelManager, CLI messages go to printer
        // Note: "cli" messages are already printed via streaming events (token + message_done),
        // so we skip them here to avoid duplicate output.
        let (printer_tx, mut printer_rx) = mpsc::channel(100);
        let outbound_dispatch_handle = tokio::spawn(async move {
            while let Some(msg) = outbound_rx.recv().await {
                match msg.channel.as_str() {
                    "cli" => {
                        // Print content if present (skill loops use non-streaming calls).
                        // skip_ws_echo: 对于ws渠道，流式token已通过event_tx输出，避免重复
                        // 对于CLI渠道，skip_ws_echo=true表示流式token已打印，跳过outbound重复输出
                        if !msg.content.is_empty() && !msg.skip_ws_echo {
                            let _ = printer_tx.send(msg).await;
                        }
                    }
                    "cron" => {
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
        let printer_handle = {
            let current_input = current_input.clone();
            tokio::spawn(async move {
                let mut stdout = std::io::stdout();
                while let Some(msg) = printer_rx.recv().await {
                    clear_prompt_line(&current_input, &mut stdout);
                    if msg.channel == "cron" {
                        println!("\n[cron] {}", msg.content);
                    } else {
                        println!("\n{}", msg.content);
                    }
                    println!();
                    restore_prompt_line(&current_input, &mut stdout);
                }
            })
        };

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
        let stdin_checkpoint_manager = CheckpointManager::new(&paths.workspace());

        // 创建会话清除标记（用于 /clear 命令）
        let session_clear_flag = Arc::new(AtomicBool::new(false));
        let session_clear_flag_clone = session_clear_flag.clone();
        let response_cache_for_stdin = response_cache.clone();
        let stdin_current_input = current_input.clone();
        // 创建关闭标志，用于 Ctrl+C 时优雅退出
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let stdin_shutdown_flag = shutdown_flag.clone();

        let stdin_handle = tokio::task::spawn_blocking(move || {
            let mut stdout = std::io::stdout();
            // 获取当前运行时句柄，用于在阻塞线程中执行异步命令
            let handle = tokio::runtime::Handle::current();

            loop {
                // 检查是否收到关闭信号
                if stdin_shutdown_flag.load(Ordering::SeqCst) {
                    break;
                }

                // Note: prompt is printed inside read_line_with_command_picker
                // to avoid double printing after raw mode is enabled

                // Read input character by character to detect "/" immediately
                let input = read_line_with_command_picker(
                    &stdin_paths,
                    &mut stdout,
                    &session_clone,
                    &stdin_tx,
                    &stdin_current_input,
                    &stdin_shutdown_flag,
                );

                // Check if a confirmation request arrived
                if let Ok(response_tx) = confirm_answer_rx.try_recv() {
                    let answer = input.trim().to_lowercase();
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

                let input = input.trim().to_string();
                if input.is_empty() {
                    continue;
                }

                // 使用统一的斜杠命令处理器
                if input.starts_with('/') {
                    // 构造命令上下文
                    let ctx = CommandContext::for_cli(
                        stdin_paths.clone(),
                        stdin_task_manager.clone(),
                        stdin_checkpoint_manager.clone(),
                        session_clone
                            .split(':')
                            .nth(1)
                            .unwrap_or("default")
                            .to_string(),
                    )
                    .with_clear_callback(Arc::new({
                        let flag = session_clear_flag_clone.clone();
                        move || {
                            flag.store(true, Ordering::SeqCst);
                            true
                        }
                    }));

                    // 同步执行命令处理器
                    let result = handle.block_on(SLASH_COMMAND_HANDLER.try_handle(&input, &ctx));

                    match result {
                        CommandResult::Handled(response) => {
                            print!("{}", response.content);
                            continue;
                        }
                        CommandResult::ExitRequested => {
                            println!("退出交互模式...");
                            break;
                        }
                        CommandResult::NotACommand => {
                            // 不是斜杠命令，继续正常消息处理流程
                        }
                        CommandResult::PermissionDenied(msg) => {
                            eprintln!("权限不足: {}", msg);
                            continue;
                        }
                        CommandResult::Error(e) => {
                            eprintln!("命令执行错误: {}", e);
                            continue;
                        }
                        CommandResult::ForwardToRuntime {
                            transformed_content,
                            original_command,
                        } => {
                            // 命令需要转发给 AgentRuntime（如 /learn, /cancel-task, /resume）
                            tracing::info!(
                                command = %original_command,
                                "Forwarding command to AgentRuntime"
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
                                content: transformed_content,
                                media: vec![],
                                // 标记来源为斜杠命令，runtime 据此验证授权
                                metadata: serde_json::json!({
                                    "source": "slash_command",
                                    "original_command": original_command
                                }),
                                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                            };
                            if stdin_tx.blocking_send(inbound).is_err() {
                                break;
                            }
                            continue;
                        }
                    }
                }

                // 检查会话清除标记（由 /clear 命令设置）
                if session_clear_flag_clone.load(Ordering::SeqCst) {
                    // 标记已处理，重置
                    session_clear_flag_clone.store(false, Ordering::SeqCst);
                    // 清除内存中的 ResponseCache
                    response_cache_for_stdin.clear_session(&session_clone);
                    tracing::info!(session = %session_clone, "[/clear] ResponseCache cleared");
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
            cleanup_handle,
            runtime_handle,
            cron_handle,
            printer_handle,
            confirm_handle,
            outbound_dispatch_handle,
            event_handler_handle,
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
        assert_eq!(
            resolved.paths.workspace(),
            PathBuf::from("/tmp/blockcell/workspace")
        );
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
        assert_eq!(
            resolved.paths.workspace(),
            PathBuf::from("/tmp/blockcell/agents/ops/workspace")
        );
        assert_eq!(
            resolved.config.agents.defaults.provider.as_deref(),
            Some("deepseek")
        );
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
