use blockcell_agent::{
    AgentRuntime, CapabilityRegistryAdapter, CheckpointManager, ConfirmRequest,
    CoreEvolutionAdapter, MemoryStoreAdapter, MessageBus, ProviderLLMBridge, ResponseCacheConfig,
    SkillEvolutionLLMBridge, SteeringRegistry, TaskManager,
};
#[cfg(feature = "dingtalk")]
use blockcell_channels::dingtalk::DingTalkChannel;
#[cfg(feature = "discord")]
use blockcell_channels::discord::DiscordChannel;
#[cfg(feature = "feishu")]
use blockcell_channels::feishu::FeishuChannel;
#[cfg(feature = "napcat")]
use blockcell_channels::napcat::NapCatChannel;
#[cfg(feature = "slack")]
use blockcell_channels::slack::SlackChannel;
#[cfg(feature = "telegram")]
use blockcell_channels::telegram::TelegramChannel;
#[cfg(feature = "wecom")]
use blockcell_channels::wecom::WeComChannel;
#[cfg(feature = "whatsapp")]
use blockcell_channels::whatsapp::WhatsAppChannel;
use blockcell_channels::ChannelManager;
use blockcell_core::{Config, InboundMessage, OutboundMessage, Paths};
use blockcell_scheduler::{
    CronJob, CronService, DreamService, DreamServiceConfig, EvolutionWorker,
    GhostMaintenanceService, GhostMaintenanceServiceConfig, HeartbeatService, JobPayload,
    JobSchedule, JobState, ScheduleKind, SkillEvolutionWorker,
};
use blockcell_skills::{new_registry_handle, CoreEvolution};
use blockcell_skills::{EvolutionService, EvolutionServiceConfig};
use blockcell_storage::EvolutionWorkflowStore;
use blockcell_tools::mcp::manager::McpManager;
use blockcell_tools::{
    build_tool_registry_for_agent_config, build_tool_registry_with_all_mcp,
    CapabilityRegistryHandle, CoreEvolutionHandle, EventEmitterHandle, MemoryStoreHandle,
    ToolRegistry,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{debug, error, info, warn};

use axum::{
    extract::{
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Path as AxumPath, Query, State,
    },
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};

use super::memory_store::open_memory_store;

fn create_skill_evolution_llm_provider(
    config: &Config,
    provider_pool: &blockcell_providers::ProviderPool,
    agent_id: &str,
) -> Option<Arc<dyn blockcell_skills::LLMProvider>> {
    let provider: Option<Arc<dyn blockcell_providers::Provider>> = if config
        .agents
        .defaults
        .evolution_model
        .is_some()
        || config.agents.defaults.evolution_provider.is_some()
    {
        match super::provider::create_evolution_provider(config) {
            Ok(evo_provider) => {
                info!(agent_id = %agent_id, "Skill evolution provider configured with independent model");
                Some(Arc::from(evo_provider))
            }
            Err(e) => {
                warn!(agent_id = %agent_id, error = %e, "Failed to create skill evolution provider; using main provider");
                provider_pool.acquire().map(|(_, p)| p)
            }
        }
    } else {
        provider_pool.acquire().map(|(_, p)| p)
    };

    provider.map(|p| {
        Arc::new(SkillEvolutionLLMBridge::new_arc(p)) as Arc<dyn blockcell_skills::LLMProvider>
    })
}

mod alerts;
mod banner;
mod capabilities;
mod channel_ids;
mod channels;
mod chat;
mod config_api;
mod cors;
mod cron;
mod files;
mod memory;
mod outbound;
mod sessions;
mod skills_install;
mod streams;
mod toggles;
mod util;
mod webhooks;
mod websocket;
mod webui;

use alerts::*;
use banner::*;
use capabilities::*;
use channel_ids::*;
use channels::*;
use chat::*;
use config_api::*;
use cors::*;
use cron::*;
use files::*;
use memory::*;
use outbound::*;
use sessions::*;
use skills_install::*;
use streams::*;
use toggles::*;
use util::*;
use webhooks::*;
use websocket::*;
use webui::*;

// ---------------------------------------------------------------------------
// WebSocket event types for structured protocol
// ---------------------------------------------------------------------------

/// Events broadcast from runtime to all connected WebSocket clients
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum WsEvent {
    #[serde(rename = "session_bound")]
    SessionBound {
        channel: String,
        client_chat_id: String,
        chat_id: String,
        agent_id: String,
    },
    #[serde(rename = "agent_progress")]
    AgentProgress {
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        chat_id: Option<String>,
        task_id: String,
        progress_type: String,
        tokens_added: u64,
        tools_added: u64,
        total_tokens: u64,
        total_tools: u64,
    },
    /// 任务阶段进度（阶段描述 + 百分比）
    #[serde(rename = "agent_stage")]
    AgentStage {
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        chat_id: Option<String>,
        task_id: String,
        stage: String,
        percent: u8,
    },
    #[serde(rename = "error")]
    Error {
        channel: String,
        chat_id: String,
        message: String,
    },
}

impl WsEvent {
    /// Serialize the event to a JSON string.
    ///
    /// This method is safe because all WsEvent variants contain only
    /// serializable types (String, usize, u64, Vec<String>).
    /// The serialization should never fail, but we handle it gracefully
    /// by returning a fallback error JSON string.
    fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            // Fallback: return a minimal error event
            r#"{"type":"error","chat_id":"","message":"Internal serialization error"}"#.to_string()
        })
    }

    /// Create an error event with the given chat_id and message
    fn error(chat_id: impl Into<String>, message: impl Into<String>) -> Self {
        WsEvent::Error {
            channel: "ws".to_string(),
            chat_id: chat_id.into(),
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Shared state passed to HTTP/WS handlers
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct GatewayState {
    inbound_tx: mpsc::Sender<InboundMessage>,
    task_manager: TaskManager,
    checkpoint_manager: CheckpointManager,
    config: Config,
    /// Serializes all config.json5 read/modify/write transactions.
    config_write_lock: Arc<Mutex<()>>,
    paths: Paths,
    api_token: Option<String>,
    /// Broadcast channel for streaming events to WebSocket clients
    ws_broadcast: broadcast::Sender<String>,
    /// Pending path-confirmation requests waiting for WebUI user response (keyed by request_id)
    pending_confirms: Arc<Mutex<HashMap<String, PendingWsConfirm>>>,
    /// Pending path-confirmation requests waiting for a matching non-ws sender reply.
    #[allow(dead_code)]
    pending_channel_confirms:
        Arc<Mutex<HashMap<PendingChannelConfirmScope, PendingChannelConfirm>>>,
    /// Default agent memory store handle
    memory_store: Option<MemoryStoreHandle>,
    /// Agent-scoped memory store handles
    memory_stores: Arc<HashMap<String, MemoryStoreHandle>>,
    /// Agent-scoped cron services
    cron_services: Arc<HashMap<String, Arc<CronService>>>,
    /// Tool registry for listing tools
    tool_registry: Arc<ToolRegistry>,
    /// Password for WebUI login (configured or auto-generated)
    web_password: String,
    /// Per-client failed login tracking for the public WebUI login endpoint.
    login_rate_limiter: Arc<Mutex<LoginRateLimiter>>,
    /// Channel manager for status reporting
    channel_manager: Arc<blockcell_channels::ChannelManager>,
    /// Shared EvolutionService for trigger/delete/status handlers
    evolution_service: Arc<Mutex<EvolutionService>>,
    /// Shared ResponseCache for all agents (for /clear command)
    response_caches: Arc<RwLock<HashMap<String, blockcell_agent::ResponseCache>>>,
    /// Active per-chat steering senders exposed by running agent tasks.
    active_steering: SteeringRegistry,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct PendingWsConfirmScope {
    agent_id: String,
    chat_id: String,
    ws_connection_id: Option<String>,
}

struct PendingWsConfirm {
    scope: PendingWsConfirmScope,
    response_tx: tokio::sync::oneshot::Sender<bool>,
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
struct PendingChannelConfirmScope {
    channel: String,
    account_id: Option<String>,
    chat_id: String,
    sender_id: String,
}

impl PendingChannelConfirmScope {
    fn new(channel: &str, account_id: Option<&str>, chat_id: &str, sender_id: &str) -> Self {
        Self {
            channel: channel.to_string(),
            account_id: account_id.map(str::to_string),
            chat_id: chat_id.to_string(),
            sender_id: sender_id.to_string(),
        }
    }
}

struct PendingChannelConfirm {
    request_id: String,
    response_tx: tokio::sync::oneshot::Sender<bool>,
}

fn is_channel_confirm_approved(text: &str) -> bool {
    matches!(
        text.trim().to_lowercase().as_str(),
        "y" | "yes" | "允许" | "确认" | "同意" | "ok"
    )
}

#[derive(Deserialize, Default)]
pub(super) struct AgentScopedQuery {
    #[serde(default)]
    pub agent: Option<String>,
}

fn memory_store_for_agent(
    state: &GatewayState,
    requested: Option<&str>,
) -> std::result::Result<(String, MemoryStoreHandle), String> {
    let agent_id = resolve_requested_agent_id(&state.config, requested)?;
    let store = state
        .memory_stores
        .get(&agent_id)
        .cloned()
        .ok_or_else(|| format!("Memory store not available for agent '{}'", agent_id))?;
    Ok((agent_id, store))
}

fn cron_service_for_agent(
    state: &GatewayState,
    requested: Option<&str>,
) -> std::result::Result<(String, Arc<CronService>), String> {
    let agent_id = resolve_requested_agent_id(&state.config, requested)?;
    let service = state
        .cron_services
        .get(&agent_id)
        .cloned()
        .ok_or_else(|| format!("Cron service not available for agent '{}'", agent_id))?;
    Ok((agent_id, service))
}

fn open_agent_memory_store(paths: &Paths, config: &Config) -> Option<MemoryStoreHandle> {
    match open_memory_store(paths, config) {
        Ok(store) => {
            if let Err(e) = store.migrate_from_files(&paths.memory_dir()) {
                warn!(agent_base = %paths.base.display(), error = %e, "Memory migration failed");
            }
            let adapter = MemoryStoreAdapter::new(store);
            Some(Arc::new(adapter))
        }
        Err(e) => {
            warn!(
                agent_base = %paths.base.display(),
                error = %e,
                "Failed to open memory store; memory tools will be unavailable"
            );
            None
        }
    }
}

/// Create a session clear callback for CommandContext.
/// This callback is used by the /clear command to clear ResponseCache.
///
/// 使用 std::sync::RwLock 而非 tokio::sync::RwLock，使回调完全同步，
/// 避免 handle.block_on() 在 tokio worker 线程中 panic。
fn create_session_clear_callback(
    response_caches: Arc<RwLock<HashMap<String, blockcell_agent::ResponseCache>>>,
    agent_id: String,
    session_key: String,
) -> Arc<dyn Fn() -> bool + Send + Sync> {
    Arc::new(move || {
        // std::sync::RwLock 的 read() 是同步的，无需 block_on
        if let Ok(caches) = response_caches.read() {
            if let Some(cache) = caches.get(&agent_id) {
                cache.clear_session(&session_key);
                info!(
                    session_key = %session_key,
                    agent_id = %agent_id,
                    "[/clear] ResponseCache cleared"
                );
                true
            } else {
                warn!(
                    agent_id = %agent_id,
                    "[/clear] No ResponseCache found for agent"
                );
                false
            }
        } else {
            warn!("[/clear] ResponseCache RwLock poisoned");
            false
        }
    })
}

#[allow(clippy::too_many_arguments)]
async fn spawn_agent_runtime(
    config: &Config,
    paths: &Paths,
    mcp_manager: Arc<McpManager>,
    agent_id: &str,
    outbound_tx: mpsc::Sender<OutboundMessage>,
    confirm_tx: mpsc::Sender<ConfirmRequest>,
    ws_broadcast_tx: broadcast::Sender<String>,
    shutdown_tx: broadcast::Sender<()>,
    task_manager: TaskManager,
    response_caches: Arc<RwLock<HashMap<String, blockcell_agent::ResponseCache>>>,
    active_steering: SteeringRegistry,
) -> anyhow::Result<(
    mpsc::Sender<InboundMessage>,
    tokio::task::JoinHandle<()>,
    Option<MemoryStoreHandle>,
    EventEmitterHandle,
)> {
    let agent_config = config
        .config_for_agent(agent_id)
        .ok_or_else(|| anyhow::anyhow!("Unknown agent '{}'", agent_id))?;
    let agent_paths = paths.for_agent(agent_id);
    agent_paths.ensure_dirs()?;

    let provider_pool = blockcell_providers::ProviderPool::from_config(&agent_config)?;
    let memory_store_handle = open_agent_memory_store(&agent_paths, &agent_config);

    let cap_registry_dir = agent_paths.evolved_tools_dir();
    let cap_registry_raw = new_registry_handle(cap_registry_dir);
    {
        let mut reg = cap_registry_raw.lock().await;
        let _ = reg.load();
        let rehydrated = reg.rehydrate_executors();
        if rehydrated > 0 {
            info!(agent_id = %agent_id, rehydrated, "Rehydrated evolved tool executors");
        }
    }

    let llm_timeout_secs = 300u64;
    let mut core_evo = CoreEvolution::new(
        agent_paths.workspace().to_path_buf(),
        cap_registry_raw.clone(),
        llm_timeout_secs,
    );
    if let Some((_, evo_provider)) = provider_pool.acquire() {
        let llm_bridge = Arc::new(ProviderLLMBridge::new_arc(evo_provider));
        core_evo.set_llm_provider(llm_bridge);
        info!(agent_id = %agent_id, "Core evolution LLM provider configured");
    }
    let core_evo_raw = Arc::new(Mutex::new(core_evo));

    let cap_registry_adapter = CapabilityRegistryAdapter::new(cap_registry_raw.clone());
    let cap_registry_handle: CapabilityRegistryHandle = Arc::new(cap_registry_adapter);

    let core_evo_adapter = CoreEvolutionAdapter::new(core_evo_raw.clone());
    let core_evo_handle: CoreEvolutionHandle = Arc::new(Mutex::new(core_evo_adapter));

    let tool_registry =
        build_tool_registry_for_agent_config(&agent_config, Some(&mcp_manager)).await?;
    let mut runtime = AgentRuntime::new(
        agent_config.clone(),
        agent_paths.clone(),
        Arc::clone(&provider_pool),
        tool_registry,
    )?;
    runtime.validate_intent_router()?;

    if agent_config.agents.defaults.evolution_model.is_some()
        || agent_config.agents.defaults.evolution_provider.is_some()
    {
        match super::provider::create_evolution_provider(&agent_config) {
            Ok(evo_provider) => {
                runtime.set_evolution_provider(evo_provider);
                info!(agent_id = %agent_id, "Evolution provider configured with independent model");
            }
            Err(e) => {
                warn!(agent_id = %agent_id, error = %e, "Failed to create evolution provider; using main provider");
            }
        }
    }

    runtime.set_outbound(outbound_tx);
    runtime.set_confirm(confirm_tx);
    runtime.set_task_manager(task_manager);
    runtime.set_agent_id(Some(agent_id.to_string()));
    if let Some(ref store) = memory_store_handle {
        runtime.set_memory_store(store.clone());
    }
    if let Err(e) = runtime.init_memory_file_store() {
        warn!(agent_id = %agent_id, error = %e, "Failed to initialize file memory store");
    }
    if let Err(e) = runtime.init_skill_file_store() {
        warn!(agent_id = %agent_id, error = %e, "Failed to initialize skill file store");
    }

    runtime.set_capability_registry(cap_registry_handle);
    runtime.set_core_evolution(core_evo_handle);

    // 创建核心进化工作流存储和 worker
    let evo_workflow_db = agent_paths.evolution_workflow_db();
    let evo_workflow_store = EvolutionWorkflowStore::open(&evo_workflow_db)?;
    let evo_workflow_store_arc = Arc::new(evo_workflow_store);
    let evo_worker = EvolutionWorker::new((*evo_workflow_store_arc).clone(), core_evo_raw.clone());
    let evo_worker_arc = Arc::new(evo_worker);
    runtime.set_evolution_workflow_store(evo_workflow_store_arc);
    runtime.set_evolution_worker(evo_worker_arc.clone() as Arc<dyn blockcell_agent::EvolutionNotifier>);

    let skill_evo_llm_provider =
        create_skill_evolution_llm_provider(&agent_config, &provider_pool, agent_id);
    let skill_evo_workflow_db = agent_paths.skill_evolution_workflow_db();
    let skill_evo_workflow_store = EvolutionWorkflowStore::open(&skill_evo_workflow_db)?;
    // 从 Config.evolution 转换配置，而非使用默认值
    let skill_evo_config: EvolutionServiceConfig = agent_config.evolution.clone().into();
    let mut skill_evo_worker = SkillEvolutionWorker::new(
        skill_evo_workflow_store,
        agent_paths.skills_dir(),
        skill_evo_config,
        skill_evo_llm_provider,
    );
    // 为 SkillEvolutionWorker 设置部署回调，确保 scheduler worker 的进化部署路径也能触发 ghost learning boundary
    if let Some(callback) =
        blockcell_agent::create_evolution_deploy_callback(&agent_config, &agent_paths)
    {
        skill_evo_worker.set_deploy_callback(callback);
        info!("[evolution-deploy-callback] 已连接到 SkillEvolutionWorker EvolutionService");
    }
    let skill_evo_worker_arc = Arc::new(skill_evo_worker);
    runtime.set_skill_evolution_worker(
        skill_evo_worker_arc.clone() as Arc<dyn blockcell_agent::EvolutionNotifier>
    );

    // 启动核心进化 worker 后台任务
    let evo_shutdown_rx = shutdown_tx.subscribe();
    tokio::spawn(async move {
        evo_worker_arc.run_loop(evo_shutdown_rx).await;
    });
    let skill_evo_shutdown_rx = shutdown_tx.subscribe();
    tokio::spawn(async move {
        skill_evo_worker_arc.run_loop(skill_evo_shutdown_rx).await;
    });

    runtime.set_event_tx(ws_broadcast_tx);
    runtime.set_active_steering_registry(active_steering);

    // Create shared ResponseCache and register it
    let response_cache = blockcell_agent::ResponseCache::with_config(ResponseCacheConfig::from(
        &agent_config.memory.memory_system.layer1,
    ));
    runtime.set_response_cache(response_cache.clone());
    {
        // std::sync::RwLock 的 write() 是同步的，但可能因 panic 而 poisoned
        // 使用 recover 模式避免 cascade failure
        match response_caches.write() {
            Ok(mut caches) => {
                caches.insert(agent_id.to_string(), response_cache);
            }
            Err(e) => {
                // Lock poisoned - recover by clearing and re-inserting
                let mut caches = e.into_inner();
                caches.clear();
                caches.insert(agent_id.to_string(), response_cache);
                warn!("response_caches lock was poisoned, recovered");
            }
        }
    }
    info!(agent_id = %agent_id, "ResponseCache registered for /clear command");

    // Initialize Layer 5 memory injector (7-layer memory system)
    if let Err(e) = runtime.init_memory_injector().await {
        warn!(error = %e, "Failed to initialize memory injector");
    }
    runtime.init_runtime_handle();
    runtime.wire_evolution_deploy_callback();

    let event_emitter = runtime.event_emitter_handle();

    let (agent_inbound_tx, agent_inbound_rx) = mpsc::channel::<InboundMessage>(100);
    let runtime_shutdown_rx = shutdown_tx.subscribe();
    let runtime_handle = tokio::spawn(async move {
        runtime
            .run_loop(agent_inbound_rx, Some(runtime_shutdown_rx))
            .await;
    });

    Ok((
        agent_inbound_tx,
        runtime_handle,
        memory_store_handle,
        event_emitter,
    ))
}

// ---------------------------------------------------------------------------
// Bearer token authentication middleware
// ---------------------------------------------------------------------------

async fn auth_middleware(
    State(state): State<GatewayState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let token = match &state.api_token {
        Some(t) if !t.is_empty() => t,
        _ => return next.run(req).await,
    };

    let path = req.uri().path();
    if path == "/v1/health" || path == "/v1/auth/login" {
        return next.run(req).await;
    }
    // Browser WebSockets cannot set Authorization. The upgrade handler validates
    // the token carried in the negotiated subprotocol and emits close code 4401.
    if path == "/v1/ws" {
        return next.run(req).await;
    }

    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let authorized_by_header = match auth_header {
        Some(h) if h.starts_with("Bearer ") => secure_eq(&h[7..], token.as_str()),
        _ => false,
    };

    if authorized_by_header {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            "Unauthorized: invalid or missing Bearer token",
        )
            .into_response()
    }
}

#[derive(Debug, Eq, PartialEq)]
enum GatewayShutdownCause {
    Signal,
    ServerFailure(String),
}

fn report_critical_task_exit(
    failure_tx: &mpsc::UnboundedSender<String>,
    shutting_down: &std::sync::atomic::AtomicBool,
    task_name: &str,
    error: Option<String>,
) {
    use std::sync::atomic::Ordering;

    if shutting_down.load(Ordering::Acquire) {
        return;
    }
    let message = match error {
        Some(error) => format!("Critical task '{task_name}' failed: {error}"),
        None => format!("Critical task '{task_name}' stopped unexpectedly"),
    };
    error!(task = %task_name, message = %message);
    let _ = failure_tx.send(message);
}

fn supervise_critical_tasks(
    handles: Vec<(String, tokio::task::JoinHandle<()>)>,
    failure_tx: &mpsc::UnboundedSender<String>,
    shutting_down: &Arc<std::sync::atomic::AtomicBool>,
) -> Vec<(String, tokio::task::JoinHandle<()>)> {
    handles
        .into_iter()
        .map(|(name, handle)| {
            let monitor_name = name.clone();
            let task_name = name.clone();
            let failure_tx = failure_tx.clone();
            let shutting_down = Arc::clone(shutting_down);
            let monitor = tokio::spawn(async move {
                let error = handle.await.err().map(|error| error.to_string());
                report_critical_task_exit(&failure_tx, &shutting_down, &task_name, error);
            });
            (monitor_name, monitor)
        })
        .collect()
}

async fn wait_for_gateway_shutdown(
    mut server_failure_rx: mpsc::UnboundedReceiver<String>,
) -> anyhow::Result<GatewayShutdownCause> {
    tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal?;
            Ok(GatewayShutdownCause::Signal)
        }
        failure = server_failure_rx.recv() => Ok(GatewayShutdownCause::ServerFailure(
            failure.unwrap_or_else(|| "server failure monitor closed unexpectedly".to_string())
        )),
    }
}

// ---------------------------------------------------------------------------
// Main gateway entry point
// ---------------------------------------------------------------------------

pub async fn run(cli_host: Option<String>, cli_port: Option<u16>) -> anyhow::Result<()> {
    let mut paths = Paths::new();
    super::env_file::ensure_and_load_blockcell_env(&paths)?;
    let mut config = Config::load_or_default(&paths)?;
    paths.apply_workspace_config(&config.agents.defaults.workspace);

    // 同步 BLOCKCELL_WORKSPACE 环境变量，供 channel listener 等模块读取 media 目录
    std::env::set_var("BLOCKCELL_WORKSPACE", paths.workspace());

    // Ensure autoUpgrade.manifestUrl has a value (migrates old configs with empty string)
    if config.auto_upgrade.manifest_url.is_empty() {
        config.auto_upgrade.manifest_url =
            "https://github.com/blockcell-labs/blockcell/releases/latest/download/manifest.json"
                .to_string();
        let _ = config.save(&paths.config_file());
    }

    let _auto_upgrade_handle =
        super::upgrade::spawn_auto_upgrade_if_enabled(config.clone(), paths.clone());

    // Auto-generate and persist node_alias if not set (short 8-char hex, e.g. "54c6be7b").
    // This becomes the stable display name for this node in the community hub.
    if config.community_hub.node_alias.is_none() {
        let alias = uuid::Uuid::new_v4().to_string().replace('-', "")[..8].to_string();
        config.community_hub.node_alias = Some(alias.clone());
        if let Err(e) = config.save(&paths.config_file()) {
            warn!("Failed to persist node_alias to config.json5: {}", e);
        } else {
            info!(node_alias = %alias, "Generated and persisted node_alias to config.json5");
        }
    }

    // If Community Hub is configured but apiKey is missing/empty, auto-register and persist.
    if let Some(hub_url) = config.community_hub_url() {
        if config.community_hub_api_key().is_none() {
            let register_url = format!("{}/v1/auth/register", hub_url.trim_end_matches('/'));
            let name = config
                .community_hub
                .node_alias
                .clone()
                .unwrap_or_else(|| "blockcell-gateway".to_string());

            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default();

            let body = serde_json::json!({
                "name": name,
                "email": null,
                "github_id": null,
            });

            match client.post(&register_url).json(&body).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    if status.is_success() {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let Some(api_key) = v.get("api_key").and_then(|x| x.as_str()) {
                                if !api_key.trim().is_empty() {
                                    config.community_hub.api_key = Some(api_key.trim().to_string());
                                    if let Err(e) = config.save(&paths.config_file()) {
                                        warn!(error = %e, "Failed to persist community hub apiKey to config file");
                                    } else {
                                        info!("Registered with Community Hub and persisted apiKey to config");
                                    }
                                }
                            }
                        }
                    } else {
                        warn!(status = %status, body = %text, "Community Hub register failed");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to register with Community Hub");
                }
            }
        }
    }

    // Resolve host/port: CLI args override config values
    let host = cli_host.unwrap_or_else(|| config.gateway.host.clone());
    let port = cli_port.unwrap_or(config.gateway.port);

    // Auto-generate and persist api_token if not configured or empty.
    // This ensures a stable token across restarts without manual setup.
    let needs_token = config
        .gateway
        .api_token
        .as_deref()
        .map(|t| t.trim().is_empty())
        .unwrap_or(true);
    if needs_token {
        let env_token = std::env::var("BLOCKCELL_API_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty());
        if let Some(token) = env_token {
            // Use env var but don't persist — user manages it externally
            config.gateway.api_token = Some(token);
        } else {
            // Generate a 64-char token (bc_ + 4×UUID hex = 3+32*4=131 chars, take first 61 for bc_+61=64)
            let raw = format!(
                "{}{}{}{}",
                uuid::Uuid::new_v4().to_string().replace('-', ""),
                uuid::Uuid::new_v4().to_string().replace('-', ""),
                uuid::Uuid::new_v4().to_string().replace('-', ""),
                uuid::Uuid::new_v4().to_string().replace('-', ""),
            );
            let generated = format!("bc_{}", &raw[..61]);
            config.gateway.api_token = Some(generated);
            if let Err(e) = config.save(&paths.config_file()) {
                warn!(
                    "Failed to persist auto-generated apiToken to config.json5: {}",
                    e
                );
            } else {
                info!("Auto-generated apiToken persisted to config.json5");
            }
        }
    }

    // Enforce simplified routing invariant:
    // every enabled external channel must be bound to exactly one owner agent.
    validate_channel_owner_bindings(&config)?;

    info!(host = %host, port = port, "Starting blockcell gateway");

    let (server_failure_tx, server_failure_rx) = mpsc::unbounded_channel::<String>();
    let shutdown_started = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // ── Create message bus ──
    let bus = MessageBus::new(100);
    let ((inbound_tx, inbound_rx), (outbound_tx, outbound_rx)) = bus.split();

    // ── Create WebSocket broadcast channel ──
    let (ws_broadcast_tx, _) = broadcast::channel::<String>(1000);

    // ── Create shutdown channel ──
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // ── Create progress channel for agent execution updates ──
    let (progress_tx, mut progress_rx) = mpsc::channel::<blockcell_agent::AgentProgress>(100);

    // ── Create shared task manager with workspace and progress channel ──
    let task_manager = TaskManager::with_workspace_and_progress(&paths.workspace(), progress_tx);

    // Restore unfinished tasks from disk
    let restored = task_manager.restore_from_disk(&paths.workspace()).await;
    if restored > 0 {
        info!("Restored {} unfinished tasks from disk", restored);
    }

    // Start periodic cleanup of evicted tasks (with file cleanup)
    let cleanup_handle = Arc::new(task_manager.clone())
        .spawn_cleanup_loop(&paths.workspace(), shutdown_tx.subscribe());

    // ── Bridge progress events to WebSocket broadcast + Channel forwarding ──
    let ws_broadcast_for_progress = ws_broadcast_tx.clone();
    let task_manager_for_progress = task_manager.clone();
    let outbound_tx_for_progress = outbound_tx.clone();
    tokio::spawn(async move {
        use std::collections::HashMap;
        // 节流：记录每个 task_id 上次转发到 channel 的百分比
        // 仅当百分比变化 >= CHANNEL_PROGRESS_THRESHOLD 时才转发
        const CHANNEL_PROGRESS_THRESHOLD: u8 = 20;
        const THROTTLE_CLEANUP_INTERVAL: u64 = 64;
        let mut last_forwarded_percent: HashMap<String, u8> = HashMap::new();
        let mut progress_events_seen: u64 = 0;

        while let Some(progress) = progress_rx.recv().await {
            progress_events_seen = progress_events_seen.saturating_add(1);
            if progress_events_seen.is_multiple_of(THROTTLE_CLEANUP_INTERVAL)
                && !last_forwarded_percent.is_empty()
            {
                let task_ids = last_forwarded_percent.keys().cloned().collect::<Vec<_>>();
                let mut statuses = HashMap::new();
                for task_id in task_ids {
                    if let Some(task) = task_manager_for_progress.get_task(&task_id).await {
                        statuses.insert(task_id, task.status);
                    }
                }
                retain_active_progress_throttle_entries(&mut last_forwarded_percent, |task_id| {
                    statuses.get(task_id).cloned()
                });
            }

            // Convert AgentProgress to WsEvent and broadcast
            let event = match &progress {
                blockcell_agent::AgentProgress::Delta {
                    task_id,
                    tokens_added,
                    tools_added,
                    total_tokens,
                    total_tools,
                } => {
                    let task = if task_id.is_empty() {
                        None
                    } else {
                        task_manager_for_progress.get_task(task_id).await
                    };
                    WsEvent::AgentProgress {
                        channel: task.as_ref().map(|task| task.origin_channel.clone()),
                        agent_id: task
                            .as_ref()
                            .and_then(|task| task.agent_id.clone())
                            .or_else(|| Some("default".to_string())),
                        chat_id: task.as_ref().map(|task| task.origin_chat_id.clone()),
                        task_id: task_id.clone(),
                        progress_type: "delta".to_string(),
                        tokens_added: *tokens_added,
                        tools_added: *tools_added,
                        total_tokens: *total_tokens,
                        total_tools: *total_tools,
                    }
                }
                blockcell_agent::AgentProgress::Stage {
                    task_id,
                    stage,
                    percent,
                } => {
                    let task = if task_id.is_empty() {
                        None
                    } else {
                        task_manager_for_progress.get_task(task_id).await
                    };
                    let should_forward = update_stage_progress_throttle(
                        &mut last_forwarded_percent,
                        task_id,
                        *percent,
                        CHANNEL_PROGRESS_THRESHOLD,
                        task.as_ref().map(|task| &task.status),
                    );

                    if should_forward {
                        if let Some(task) = task.as_ref() {
                            // 仅转发到非 ws/cli 渠道（空渠道也跳过）
                            if !task.origin_channel.is_empty()
                                && task.origin_channel != "ws"
                                && task.origin_channel != "cli"
                            {
                                let progress_bar = format_progress_bar(*percent);
                                let mut text = format!(
                                    "📊 任务进度: {} [{}%] {}",
                                    task.label, percent, progress_bar
                                );
                                if !stage.is_empty() {
                                    // 附带阶段描述（如果非空）
                                    // 注意：部分渠道有消息长度限制，截断到 50 字符
                                    let stage_display: String = stage.chars().take(50).collect();
                                    text.push_str(&format!(" - {}", stage_display));
                                }
                                let outbound = OutboundMessage::new(
                                    &task.origin_channel,
                                    &task.origin_chat_id,
                                    &text,
                                );
                                let _ = outbound_tx_for_progress.send(outbound).await;
                            }
                        }
                    }

                    WsEvent::AgentStage {
                        channel: task.as_ref().map(|task| task.origin_channel.clone()),
                        agent_id: task
                            .as_ref()
                            .and_then(|task| task.agent_id.clone())
                            .or_else(|| Some("default".to_string())),
                        chat_id: task.as_ref().map(|task| task.origin_chat_id.clone()),
                        task_id: task_id.clone(),
                        stage: stage.clone(),
                        percent: *percent,
                    }
                }
                blockcell_agent::AgentProgress::Notification(_) => {
                    // Notifications are handled separately via system events
                    continue;
                }
                blockcell_agent::AgentProgress::ToolCallStart {
                    task_id,
                    tool,
                    call_id: _,
                    agent_type,
                    summary,
                } => {
                    // 转发工具调用开始事件到外部渠道
                    if !task_id.is_empty() {
                        if let Some(task) = task_manager_for_progress.get_task(task_id).await {
                            if !task.origin_channel.is_empty()
                                && task.origin_channel != "ws"
                                && task.origin_channel != "cli"
                            {
                                let text = if summary.is_empty() {
                                    format!("🔧 [{}] {}", agent_type, tool)
                                } else {
                                    format!("🔧 [{}] {}({})", agent_type, tool, summary)
                                };
                                let outbound = OutboundMessage::new(
                                    &task.origin_channel,
                                    &task.origin_chat_id,
                                    &text,
                                );
                                let _ = outbound_tx_for_progress.send(outbound).await;
                            }
                        }
                    }
                    // 不转发到 WebSocket（event_tx 已经处理了 WebSocket）
                    continue;
                }
                blockcell_agent::AgentProgress::ToolCallEnd {
                    task_id,
                    tool,
                    call_id: _,
                    agent_type,
                    success,
                } => {
                    // 转发工具调用完成事件到外部渠道
                    if !task_id.is_empty() {
                        if let Some(task) = task_manager_for_progress.get_task(task_id).await {
                            if !task.origin_channel.is_empty()
                                && task.origin_channel != "ws"
                                && task.origin_channel != "cli"
                            {
                                let status = if *success { "✓" } else { "✗" };
                                let text = format!("{} [{}] {} done", status, agent_type, tool);
                                let outbound = OutboundMessage::new(
                                    &task.origin_channel,
                                    &task.origin_chat_id,
                                    &text,
                                );
                                let _ = outbound_tx_for_progress.send(outbound).await;
                            }
                        }
                    }
                    continue;
                }
            };
            let _ = ws_broadcast_for_progress.send(event.to_json());
        }
    });

    let mcp_manager = Arc::new(McpManager::load(&paths).await?);

    // ── Create tool registry (shared for listing tools) ──
    let tool_registry_shared =
        Arc::new(build_tool_registry_with_all_mcp(&config, Some(&mcp_manager)).await?);

    // ── Set up path confirmation channel (channel-aware) ──
    // pending_ws_confirms: keyed by request_id, for WebUI (ws) confirmations
    let pending_ws_confirms: Arc<Mutex<HashMap<String, PendingWsConfirm>>> =
        Arc::new(Mutex::new(HashMap::new()));
    // Non-ws confirmations are scoped to one channel account, chat, and sender.
    let pending_channel_confirms: Arc<
        Mutex<HashMap<PendingChannelConfirmScope, PendingChannelConfirm>>,
    > = Arc::new(Mutex::new(HashMap::new()));
    let (confirm_tx, mut confirm_rx) = mpsc::channel::<ConfirmRequest>(16);

    // Clone outbound_tx before it is moved into runtime tasks, so the confirm
    // handler can send confirmation prompts to non-ws channels.
    let outbound_tx_for_confirm = outbound_tx.clone();

    // Spawn confirm handler: routes confirmation requests to the correct channel.
    // - ws channel → broadcast confirm_request event to WebUI
    // - non-ws channels → send text prompt via outbound_tx to originating channel
    let pending_ws_for_handler = Arc::clone(&pending_ws_confirms);
    let pending_ch_for_handler = Arc::clone(&pending_channel_confirms);
    let ws_broadcast_for_confirm = ws_broadcast_tx.clone();
    let mut confirm_handler_shutdown_rx = shutdown_tx.subscribe();
    let confirm_handler_handle = tokio::spawn(async move {
        loop {
            let req = tokio::select! {
                req = confirm_rx.recv() => match req {
                    Some(req) => req,
                    None => break,
                },
                _ = confirm_handler_shutdown_rx.recv() => break,
            };

            if req.channel == "ws" {
                let request_id = new_confirm_request_id();
                let agent_id = req.agent_id.unwrap_or_else(|| "default".to_string());
                let chat_id = req.chat_id;
                let ws_connection_id = req.ws_connection_id;
                let Some(ws_connection_id) = ws_connection_id else {
                    warn!(
                        agent_id = %agent_id,
                        chat_id = %chat_id,
                        tool = %req.tool_name,
                        "Rejecting ws confirmation request without a WebSocket connection binding"
                    );
                    let _ = req.response_tx.send(false);
                    continue;
                };
                {
                    let mut map = pending_ws_for_handler.lock().await;
                    map.insert(
                        request_id.clone(),
                        PendingWsConfirm {
                            scope: PendingWsConfirmScope {
                                agent_id: agent_id.clone(),
                                chat_id: chat_id.clone(),
                                ws_connection_id: Some(ws_connection_id.clone()),
                            },
                            response_tx: req.response_tx,
                        },
                    );
                }
                let mut event = serde_json::json!({
                    "type": "confirm_request",
                    "request_id": request_id,
                    "tool_name": req.tool_name,
                    "paths": req.paths,
                    "agent_id": agent_id,
                    "channel": "ws",
                    "chat_id": chat_id,
                });
                event["ws_connection_id"] = serde_json::json!(ws_connection_id);
                let _ = ws_broadcast_for_confirm.send(event.to_string());
            } else {
                let confirm_scope = PendingChannelConfirmScope::new(
                    &req.channel,
                    req.account_id.as_deref(),
                    &req.chat_id,
                    &req.sender_id,
                );
                let request_id = new_confirm_request_id();
                {
                    let mut map = pending_ch_for_handler.lock().await;
                    if map.contains_key(&confirm_scope) {
                        warn!(
                            ?confirm_scope,
                            "Rejecting overlapping channel confirmation request"
                        );
                        let _ = req.response_tx.send(false);
                        continue;
                    }
                    map.insert(
                        confirm_scope.clone(),
                        PendingChannelConfirm {
                            request_id: request_id.clone(),
                            response_tx: req.response_tx,
                        },
                    );
                }
                let prompt = format!(
                    "⚠️ 工具 {} 需要访问以下路径：
{}

确认编号：{}
回复 yes / y / 允许 / 同意 进行确认，其他任意内容将拒绝。",
                    req.tool_name,
                    req.paths.join("\n"),
                    request_id,
                );
                let mut outbound = OutboundMessage::new(&req.channel, &req.chat_id, &prompt);
                outbound.account_id = req.account_id;
                outbound.metadata = serde_json::json!({
                    "confirm_request": true,
                    "confirm_request_id": request_id,
                });
                if outbound_tx_for_confirm.send(outbound).await.is_err() {
                    let mut map = pending_ch_for_handler.lock().await;
                    if let Some(pending) = map.remove(&confirm_scope) {
                        let _ = pending.response_tx.send(false);
                    }
                } else {
                    info!(?confirm_scope, %request_id, tool = %req.tool_name, "Sent confirm_request to channel");
                }
            }
        }
    });

    // ── Create one runtime per resolved agent ──
    let resolved_agents = config.resolved_agents();
    // Shared ResponseCache for /clear command
    let response_caches: Arc<RwLock<HashMap<String, blockcell_agent::ResponseCache>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let active_steering: SteeringRegistry = Arc::new(Mutex::new(HashMap::new()));
    let mut runtime_senders: HashMap<String, mpsc::Sender<InboundMessage>> = HashMap::new();
    let mut runtime_handles: Vec<(String, tokio::task::JoinHandle<()>)> = Vec::new();
    let mut agent_memory_stores: HashMap<String, MemoryStoreHandle> = HashMap::new();
    let mut agent_event_emitters: HashMap<String, EventEmitterHandle> = HashMap::new();
    for agent in &resolved_agents {
        let agent_id = agent.id.clone();
        let (agent_tx, agent_handle, memory_store_handle, event_emitter) = spawn_agent_runtime(
            &config,
            &paths,
            Arc::clone(&mcp_manager),
            &agent_id,
            outbound_tx.clone(),
            confirm_tx.clone(),
            ws_broadcast_tx.clone(),
            shutdown_tx.clone(),
            task_manager.clone(),
            response_caches.clone(),
            active_steering.clone(),
        )
        .await?;
        if let Some(memory_store_handle) = memory_store_handle {
            agent_memory_stores.insert(agent_id.clone(), memory_store_handle);
        }
        task_manager.register_event_emitter(Some(&agent_id), event_emitter.clone());
        let replayed = task_manager
            .replay_restored_failure_notifications(Some(&agent_id))
            .await;
        if replayed > 0 {
            info!(replayed, agent_id = %agent_id, "Queued interrupted task failure notifications");
        }
        agent_event_emitters.insert(agent_id.clone(), event_emitter);
        runtime_senders.insert(agent_id.clone(), agent_tx);
        runtime_handles.push((format!("runtime:{}", agent_id), agent_handle));
    }
    let default_memory_store_handle = agent_memory_stores.get("default").cloned();

    // ── Create channel manager for outbound dispatch ──
    let channel_manager = ChannelManager::new(config.clone(), paths.clone(), inbound_tx.clone());

    // ── Create scheduler services ──
    let mut cron_services_map: HashMap<String, Arc<CronService>> = HashMap::new();
    let mut cron_handles: Vec<(String, tokio::task::JoinHandle<()>)> = Vec::new();
    let tick_interval_secs = config.cron_tick_interval_secs;
    let default_timezone = config.default_timezone.as_deref();
    for agent in &resolved_agents {
        let agent_id = agent.id.clone();
        let cron_service = Arc::new(CronService::new_with_options(
            paths.for_agent(&agent_id),
            inbound_tx.clone(),
            if agent_id == "default" {
                None
            } else {
                Some(agent_id.clone())
            },
            Some(tick_interval_secs),
            default_timezone,
        ));
        if let Some(emitter) = agent_event_emitters.get(&agent_id) {
            cron_service.set_event_emitter(emitter.clone());
        }
        cron_service.load().await?;
        let shutdown_rx = shutdown_tx.subscribe();
        let cron = cron_service.clone();
        cron_handles.push((
            format!("cron:{}", agent_id),
            tokio::spawn(async move {
                cron.run_loop(shutdown_rx).await;
            }),
        ));
        cron_services_map.insert(agent_id, cron_service);
    }

    let heartbeat_service = Arc::new(HeartbeatService::new(paths.clone(), inbound_tx.clone()));

    // ── Layer 6: Dream Service (跨会话知识整合) ──
    // 使用 default agent 的配置创建 provider_pool
    let dream_provider_pool = if let Some(default_config) = config.config_for_agent("default") {
        match blockcell_providers::ProviderPool::from_config(&default_config) {
            Ok(pool) => Some(Arc::new(pool)),
            Err(e) => {
                warn!("Failed to create provider pool for Dream service: {}", e);
                None
            }
        }
    } else {
        None
    };

    if let Some(ref pool) = dream_provider_pool {
        let dream_config = DreamServiceConfig {
            enabled: config.memory.memory_system.layer6.enabled,
            check_interval_secs: config.memory.memory_system.layer6.check_interval_secs,
            time_gate_threshold_hours: config.memory.memory_system.layer6.time_gate_threshold_hours
                as f64,
            session_gate_threshold: config.memory.memory_system.layer6.session_gate_threshold,
            timeout_secs: config.memory.memory_system.layer6.timeout_secs,
            provider_pool: Some(Arc::clone(pool)),
        };
        let dream_service = DreamService::new(dream_config, paths.clone());
        let dream_shutdown_rx = shutdown_tx.subscribe();
        let dream_handle = tokio::spawn(async move {
            dream_service.run_loop(dream_shutdown_rx).await;
        });
        runtime_handles.push(("dream".to_string(), dream_handle));
        info!("[dream] Dream service started for cross-session knowledge consolidation");
    } else {
        warn!("[dream] Dream service not started: no provider pool available");
    }

    // Optional: register this gateway with the configured community hub.
    // This runs in the background and does not block gateway startup.
    if let Some(hub_url) = config.community_hub_url() {
        let client = reqwest::Client::new();
        let register_url = format!("{}/v1/nodes/heartbeat", hub_url.trim_end_matches('/'));
        let api_key = config.community_hub_api_key();
        let version = env!("CARGO_PKG_VERSION").to_string();
        let public_url = if host != "0.0.0.0" {
            Some(format!("http://{}:{}", host, port))
        } else {
            None
        };
        let node_alias = config.community_hub.node_alias.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(240));
            loop {
                interval.tick().await;

                let body = serde_json::json!({
                    "name": node_alias,
                    "version": version,
                    "public_url": public_url,
                    "tags": ["gateway", "cli"],
                    "skills": [],
                });

                let mut req = client.post(&register_url).json(&body);
                if let Some(key) = &api_key {
                    req = req.header("Authorization", format!("Bearer {}", key));
                }

                if let Err(e) = req.send().await {
                    warn!("Failed to send heartbeat to hub: {}", e);
                } else {
                    debug!("Sent heartbeat to hub");
                }
            }
        });
    }

    // ── Create scheduled Ghost maintenance service ──
    // Embedded Ghost learning runs inside AgentRuntime during normal turns and
    // does not depend on this scheduled maintenance service being enabled.
    let ghost_config = GhostMaintenanceServiceConfig::from_config(&config);
    let ghost_service =
        GhostMaintenanceService::new(ghost_config, paths.clone(), inbound_tx.clone());

    // ── Inbound interceptor: check for pending channel confirm replies ──
    // Sits between channel inbound_rx and the runtime, intercepting confirm
    // replies from non-ws channels before they reach the runtime loop.
    let (filtered_inbound_tx, filtered_inbound_rx) = mpsc::channel::<InboundMessage>(100);
    let pending_ch_for_interceptor = Arc::clone(&pending_channel_confirms);
    let mut interceptor_shutdown_rx = shutdown_tx.subscribe();
    // 斜杠命令拦截需要的变量
    let slash_paths = paths.clone();
    let slash_task_manager = task_manager.clone();
    let slash_checkpoint_manager = blockcell_agent::CheckpointManager::new(&paths.workspace());
    let slash_outbound_tx = outbound_tx.clone();
    let slash_response_caches = response_caches.clone();
    let slash_config = config.clone();
    let interceptor_handle = tokio::spawn(async move {
        let mut inbound_rx = inbound_rx;
        loop {
            let mut msg = tokio::select! {
                msg = inbound_rx.recv() => match msg {
                    Some(m) => m,
                    None => break,
                },
                _ = interceptor_shutdown_rx.recv() => break,
            };
            // Check if this message is a reply to a pending channel confirm
            if !is_internal_channel(&msg.channel) {
                let confirm_scope = PendingChannelConfirmScope::new(
                    &msg.channel,
                    msg.account_id.as_deref(),
                    &msg.chat_id,
                    &msg.sender_id,
                );
                let maybe_pending = {
                    let mut map = pending_ch_for_interceptor.lock().await;
                    map.remove(&confirm_scope)
                };
                if let Some(pending) = maybe_pending {
                    // Parse the reply as a confirm response
                    let approved = is_channel_confirm_approved(&msg.content);
                    info!(
                        ?confirm_scope,
                        request_id = %pending.request_id,
                        approved = approved,
                        reply = %msg.content.trim(),
                        "Channel confirm reply intercepted"
                    );
                    let _ = pending.response_tx.send(approved);
                    continue; // Don't forward this message to the runtime
                }
            }

            // 斜杠命令拦截（在 confirm reply 检查之后，转发给 runtime 之前）
            if !is_internal_channel(&msg.channel) && msg.content.starts_with('/') {
                use crate::commands::slash_commands::{
                    CommandContext, CommandResult, SLASH_COMMAND_HANDLER,
                };

                // 解析目标 agent_id 和 session_key
                let agent_id = resolve_runtime_agent_id(&slash_config, &msg)
                    .unwrap_or_else(|| "default".to_string());
                let session_key = format!("{}:{}", msg.channel, msg.chat_id);

                let ctx = CommandContext::for_channel(
                    slash_paths.clone(),
                    slash_task_manager.clone(),
                    slash_checkpoint_manager.clone(),
                    msg.channel.clone(),
                    msg.chat_id.clone(),
                    Some(msg.sender_id.clone()),
                )
                .with_clear_callback(create_session_clear_callback(
                    slash_response_caches.clone(),
                    agent_id,
                    session_key,
                ));

                match SLASH_COMMAND_HANDLER.try_handle(&msg.content, &ctx).await {
                    CommandResult::Handled(response) => {
                        // 发送响应回原渠道
                        let reply =
                            OutboundMessage::new(&msg.channel, &msg.chat_id, &response.content);
                        if let Err(e) = slash_outbound_tx.send(reply).await {
                            warn!(error = %e, "Failed to send command response");
                        }
                        continue; // 不转发给 AgentRuntime
                    }
                    CommandResult::NotACommand => {
                        // 非斜杠命令，继续正常消息处理流程
                    }
                    CommandResult::PermissionDenied(err_msg) => {
                        let reply = OutboundMessage::new(
                            &msg.channel,
                            &msg.chat_id,
                            &format!("权限不足: {}", err_msg),
                        );
                        let _ = slash_outbound_tx.send(reply).await;
                        continue;
                    }
                    CommandResult::Error(e) => {
                        let reply = OutboundMessage::new(
                            &msg.channel,
                            &msg.chat_id,
                            &format!("命令执行错误: {}", e),
                        );
                        let _ = slash_outbound_tx.send(reply).await;
                        continue;
                    }
                    CommandResult::ExitRequested => {
                        // /quit 和 /exit 在 Channel 模式下不应该到达这里
                        let reply = OutboundMessage::new(
                            &msg.channel,
                            &msg.chat_id,
                            "此命令仅在 CLI 模式可用",
                        );
                        let _ = slash_outbound_tx.send(reply).await;
                        continue;
                    }
                    CommandResult::ForwardToRuntime {
                        transformed_content,
                        original_command,
                    } => {
                        // 命令需要转发给 AgentRuntime（如 /learn, /cancel-task, /resume）
                        info!(
                            channel = %msg.channel,
                            command = %original_command,
                            "Forwarding command to AgentRuntime"
                        );
                        // 修改消息内容为转换后的内容
                        msg.content = transformed_content;
                        // 标记来源为斜杠命令，runtime 据此验证授权
                        msg.metadata = serde_json::json!({
                            "source": "slash_command",
                            "original_command": original_command
                        });
                        // 继续正常流程，转发给 AgentRuntime
                    }
                }
            }

            // Not a confirm reply — forward to the runtime dispatcher
            if filtered_inbound_tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    // ── Spawn runtime dispatcher ──
    let config_for_dispatch = config.clone();
    let runtime_senders_for_dispatch = runtime_senders.clone();
    let mut dispatcher_shutdown_rx = shutdown_tx.subscribe();
    let dispatcher_handle = tokio::spawn(async move {
        let mut filtered_inbound_rx = filtered_inbound_rx;
        loop {
            let msg = tokio::select! {
                msg = filtered_inbound_rx.recv() => match msg {
                    Some(m) => m,
                    None => break,
                },
                _ = dispatcher_shutdown_rx.recv() => break,
            };

            let Some(agent_id) = resolve_runtime_agent_id(&config_for_dispatch, &msg) else {
                warn!(channel = %msg.channel, chat_id = %msg.chat_id, "Dropping inbound message: unable to resolve target agent");
                continue;
            };

            let Some(agent_tx) = runtime_senders_for_dispatch.get(&agent_id) else {
                warn!(agent_id = %agent_id, channel = %msg.channel, chat_id = %msg.chat_id, "Dropping inbound message: runtime sender missing");
                continue;
            };

            let routed_msg = with_route_agent_id(msg, &agent_id);
            if agent_tx.send(routed_msg).await.is_err() {
                warn!(agent_id = %agent_id, "Agent runtime channel closed; dropping inbound message");
            }
        }
    });

    // Wrap channel_manager in Arc so it can be shared between the outbound bridge and gateway state
    let channel_manager = Arc::new(channel_manager);

    // Outbound → WS broadcast bridge + external channel dispatch
    let ws_broadcast_for_bridge = ws_broadcast_tx.clone();
    let outbound_shutdown_rx = shutdown_tx.subscribe();
    let channel_manager_for_bridge = Arc::clone(&channel_manager);
    let outbound_handle = tokio::spawn(async move {
        outbound_to_ws_bridge(
            outbound_rx,
            ws_broadcast_for_bridge,
            channel_manager_for_bridge,
            outbound_shutdown_rx,
        )
        .await;
    });

    let heartbeat_handle = {
        let heartbeat = heartbeat_service.clone();
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            heartbeat.run_loop(shutdown_rx).await;
        })
    };

    let ghost_handle = {
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(async move {
            ghost_service.run_loop(shutdown_rx).await;
        })
    };

    // ── Start messaging channels ──
    let mut channel_handles: Vec<(String, tokio::task::JoinHandle<()>)> = Vec::new();

    // 辅助函数：根据 channel/account 解析所属 agent 的 workspace，设置为 BLOCKCELL_WORKSPACE。
    // 每个 listener 的 channel 创建前调用，确保 media_dir 指向正确的 agent workspace。
    let set_workspace_for_listener = |channel: &str, account_id: Option<&str>| {
        let agent_id = config
            .resolve_effective_channel_owner(channel, account_id)
            .unwrap_or("default");
        let ws = paths.for_agent(agent_id).workspace();
        std::env::set_var("BLOCKCELL_WORKSPACE", &ws);
    };

    #[cfg(feature = "telegram")]
    for listener in blockcell_channels::account::telegram_listener_configs(&config) {
        let listener_name = listener.label.clone();
        set_workspace_for_listener("telegram", listener.account_id.as_deref());
        let telegram = Arc::new(TelegramChannel::new(listener.config, inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        channel_handles.push((
            listener_name,
            tokio::spawn(async move {
                telegram.run_loop(shutdown_rx).await;
            }),
        ));
    }

    #[cfg(feature = "whatsapp")]
    for listener in blockcell_channels::account::whatsapp_listener_configs(&config) {
        let listener_name = listener.label.clone();
        set_workspace_for_listener("whatsapp", listener.account_id.as_deref());
        let whatsapp = Arc::new(WhatsAppChannel::new(listener.config, inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        let listener_failure_tx = server_failure_tx.clone();
        channel_handles.push((
            listener_name,
            tokio::spawn(async move {
                if let Err(error) = whatsapp.run_loop(shutdown_rx).await {
                    let _ = listener_failure_tx.send(format!("Channel listener failed: {error}"));
                }
            }),
        ));
    }

    #[cfg(feature = "feishu")]
    for listener in blockcell_channels::account::feishu_scoped_configs(&config) {
        let listener_name = listener.label.clone();
        set_workspace_for_listener("feishu", listener.account_id.as_deref());
        let feishu = Arc::new(FeishuChannel::new(listener.config, inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        channel_handles.push((
            listener_name,
            tokio::spawn(async move {
                feishu.run_loop(shutdown_rx).await;
            }),
        ));
    }

    #[cfg(feature = "slack")]
    for listener in blockcell_channels::account::slack_listener_configs(&config) {
        let listener_name = listener.label.clone();
        set_workspace_for_listener("slack", listener.account_id.as_deref());
        let slack = Arc::new(SlackChannel::new(listener.config, inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        channel_handles.push((
            listener_name,
            tokio::spawn(async move {
                slack.run_loop(shutdown_rx).await;
            }),
        ));
    }

    #[cfg(feature = "discord")]
    for listener in blockcell_channels::account::discord_listener_configs(&config) {
        let listener_name = listener.label.clone();
        set_workspace_for_listener("discord", listener.account_id.as_deref());
        let discord = Arc::new(DiscordChannel::new(listener.config, inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        channel_handles.push((
            listener_name,
            tokio::spawn(async move {
                discord.run_loop(shutdown_rx).await;
            }),
        ));
    }

    #[cfg(feature = "dingtalk")]
    for listener in blockcell_channels::account::dingtalk_listener_configs(&config) {
        let listener_name = listener.label.clone();
        set_workspace_for_listener("dingtalk", listener.account_id.as_deref());
        let dingtalk = Arc::new(DingTalkChannel::new(listener.config, inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        channel_handles.push((
            listener_name,
            tokio::spawn(async move {
                dingtalk.run_loop(shutdown_rx).await;
            }),
        ));
    }

    #[cfg(feature = "wecom")]
    for listener in blockcell_channels::account::wecom_listener_configs(&config) {
        let listener_name = listener.label.clone();
        info!(listener = %listener_name, "Starting WeCom listener");
        set_workspace_for_listener("wecom", listener.account_id.as_deref());
        let wecom = Arc::new(WeComChannel::new(listener.config, inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        channel_handles.push((
            listener_name,
            tokio::spawn(async move {
                wecom.run_loop(shutdown_rx).await;
            }),
        ));
    }

    #[cfg(feature = "qq")]
    for listener in blockcell_channels::account::qq_listener_configs(&config) {
        let listener_name = listener.label.clone();
        info!(listener = %listener_name, "Starting QQ listener");
        set_workspace_for_listener("qq", listener.account_id.as_deref());
        let qq = Arc::new(blockcell_channels::qq::QQChannel::new(
            listener.config,
            inbound_tx.clone(),
        ));
        let shutdown_rx = shutdown_tx.subscribe();
        channel_handles.push((
            listener_name,
            tokio::spawn(async move {
                qq.run_loop(shutdown_rx).await;
            }),
        ));
    }

    #[cfg(feature = "napcat")]
    for listener in blockcell_channels::account::napcat_listener_configs(&config) {
        let listener_name = listener.label.clone();
        info!(listener = %listener_name, "Starting NapCatQQ listener");
        set_workspace_for_listener("napcat", listener.account_id.as_deref());
        let napcat = Arc::new(NapCatChannel::new(listener.config, inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        let listener_failure_tx = server_failure_tx.clone();
        channel_handles.push((
            listener_name,
            tokio::spawn(async move {
                if let Err(error) = napcat.run_loop(shutdown_rx).await {
                    let _ = listener_failure_tx.send(format!("Channel listener failed: {error}"));
                }
            }),
        ));
    }

    #[cfg(feature = "weixin")]
    for listener in blockcell_channels::account::weixin_listener_configs(&config) {
        let listener_name = listener.label.clone();
        info!(listener = %listener_name, "Starting Weixin listener");
        set_workspace_for_listener("weixin", listener.account_id.as_deref());
        let weixin = Arc::new(blockcell_channels::weixin::WeixinChannel::new(
            listener.config,
            inbound_tx.clone(),
        ));
        let shutdown_rx = shutdown_tx.subscribe();
        channel_handles.push((
            listener_name,
            tokio::spawn(async move {
                weixin.run_loop(shutdown_rx).await;
            }),
        ));
    }

    runtime_handles =
        supervise_critical_tasks(runtime_handles, &server_failure_tx, &shutdown_started);
    cron_handles = supervise_critical_tasks(cron_handles, &server_failure_tx, &shutdown_started);
    // Channels can finish normally when disabled, unconfigured or disconnected.
    // Keep runtime/cron early exits critical, but do not shut down the gateway
    // merely because one optional listener returned normally.
    channel_handles = channel_handles
        .into_iter()
        .map(|(name, handle)| {
            let failure_tx = server_failure_tx.clone();
            let shutting_down = Arc::clone(&shutdown_started);
            let label = name.clone();
            let monitor = tokio::spawn(async move {
                match handle.await {
                    Ok(()) => info!(listener = %label, "Channel listener stopped normally"),
                    Err(error) => report_critical_task_exit(
                        &failure_tx,
                        &shutting_down,
                        &label,
                        Some(error.to_string()),
                    ),
                }
            });
            (name, monitor)
        })
        .collect();

    // ── Build HTTP/WebSocket server ──
    // Guarantee api_token is Some and non-empty — defensive fallback in case auto-gen above
    // somehow produced None or empty (e.g. env var was whitespace-only).
    if config
        .gateway
        .api_token
        .as_deref()
        .map(|t| t.trim().is_empty())
        .unwrap_or(true)
    {
        let raw = format!(
            "{}{}{}{}",
            uuid::Uuid::new_v4().to_string().replace('-', ""),
            uuid::Uuid::new_v4().to_string().replace('-', ""),
            uuid::Uuid::new_v4().to_string().replace('-', ""),
            uuid::Uuid::new_v4().to_string().replace('-', ""),
        );
        let fallback = format!("bc_{}", &raw[..61]);
        warn!("api_token was missing/empty before building GatewayState; using in-memory fallback");
        config.gateway.api_token = Some(fallback);
    }
    let api_token = config.gateway.api_token.clone();

    // Determine WebUI login password:
    // - If gateway.webuiPass is set in config → use it (stable across restarts)
    // - Otherwise → generate a random temp password printed at startup (NOT saved)
    let (web_password, webui_pass_is_temp) = match &config.gateway.webui_pass {
        Some(p) if !p.is_empty() => (p.clone(), false),
        _ => {
            let tmp = generate_temp_password();
            (tmp, true)
        }
    };

    let is_exposed = host == "0.0.0.0" || host == "::";

    // Create a shared EvolutionService for the HTTP handlers (trigger, delete, status).
    // This is separate from the one inside AgentRuntime but shares the same disk records.
    // 从 Config.evolution 转换配置，而非使用默认值
    let evo_service_config: EvolutionServiceConfig = config.evolution.clone().into();
    let shared_evo_service = Arc::new(Mutex::new(EvolutionService::new(
        paths.skills_dir(),
        evo_service_config,
    )));

    let gateway_state = GatewayState {
        inbound_tx: inbound_tx.clone(),
        task_manager,
        checkpoint_manager: CheckpointManager::new(&paths.workspace()),
        config: config.clone(),
        config_write_lock: Arc::new(Mutex::new(())),
        paths: paths.clone(),
        api_token: api_token.clone(),
        ws_broadcast: ws_broadcast_tx.clone(),
        pending_confirms: Arc::clone(&pending_ws_confirms),
        pending_channel_confirms: Arc::clone(&pending_channel_confirms),
        memory_store: default_memory_store_handle.clone(),
        memory_stores: Arc::new(agent_memory_stores),
        cron_services: Arc::new(cron_services_map),
        tool_registry: tool_registry_shared,
        web_password: web_password.clone(),
        login_rate_limiter: Arc::new(Mutex::new(LoginRateLimiter::default())),
        channel_manager: Arc::clone(&channel_manager),
        evolution_service: shared_evo_service,
        response_caches: response_caches.clone(),
        active_steering,
    };

    let app = Router::new()
        // Auth
        .route("/v1/auth/login", post(handle_login))
        // P0: Core
        .route("/v1/chat", post(handle_chat))
        .route("/v1/health", get(handle_health))
        .route("/v1/tasks", get(handle_tasks))
        .route("/v1/ws", get(handle_ws_upgrade))
        // P0: Sessions
        .route("/v1/sessions", get(handle_sessions_list))
        .route(
            "/v1/sessions/:id",
            get(handle_session_get).delete(handle_session_delete),
        )
        .route("/v1/sessions/:id/rename", put(handle_session_rename))
        // P1: Config
        .route(
            "/v1/config",
            get(handle_config_get).put(handle_config_update),
        )
        .route(
            "/v1/config/raw",
            get(handle_config_raw_get).put(handle_config_raw_put),
        )
        .route("/v1/config/reload", post(handle_config_reload))
        .route(
            "/v1/config/test-provider",
            post(handle_config_test_provider),
        )
        // Scheduled Ghost maintenance
        .route(
            "/v1/ghost/config",
            get(handle_ghost_config_get).put(handle_ghost_config_update),
        )
        .route("/v1/ghost/activity", get(handle_ghost_activity))
        .route("/v1/ghost/metrics", get(handle_ghost_metrics))
        .route(
            "/v1/ghost/model-options",
            get(handle_ghost_model_options_get),
        )
        // P1: Memory
        .route(
            "/v1/memory",
            get(handle_memory_list).post(handle_memory_create),
        )
        .route("/v1/memory/stats", get(handle_memory_stats))
        .route("/v1/memory/:id", delete(handle_memory_delete))
        // P1: Tools / Skills / Evolution / Stats
        .route("/v1/tools", get(handle_tools))
        .route("/v1/skills", get(handle_skills))
        .route("/v1/skills/search", post(handle_skills_search))
        .route("/v1/evolution", get(handle_evolution))
        .route(
            "/v1/evolution/tool-evolutions",
            get(handle_evolution_tool_evolutions),
        )
        .route("/v1/evolution/summary", get(handle_evolution_summary))
        .route("/v1/evolution/trigger", post(handle_evolution_trigger))
        .route("/v1/evolution/test", post(handle_evolution_test))
        .route(
            "/v1/evolution/test-suggest",
            post(handle_evolution_test_suggest),
        )
        .route(
            "/v1/evolution/versions/:skill",
            get(handle_evolution_versions),
        )
        .route(
            "/v1/evolution/tool-versions/:id",
            get(handle_evolution_tool_versions),
        )
        .route(
            "/v1/evolution/:id",
            get(handle_evolution_detail).delete(handle_evolution_delete),
        )
        .route("/v1/evolution/:id/stop", post(handle_evolution_stop))
        .route("/v1/evolution/:id/resume", post(handle_evolution_resume))
        .route("/v1/channels/status", get(handle_channels_status))
        .route("/v1/channels", get(handle_channels_list))
        .route("/v1/channels/:id", put(handle_channel_update))
        .route("/v1/channels/:id/config", put(handle_channel_config_update))
        .route("/v1/channel-owners", get(handle_channel_owners_get))
        .route(
            "/v1/channel-owners/:channel",
            put(handle_channel_owner_put).delete(handle_channel_owner_delete),
        )
        .route(
            "/v1/channel-owners/:channel/accounts/:account_id",
            put(handle_channel_account_owner_put).delete(handle_channel_account_owner_delete),
        )
        .route("/v1/skills/:name", delete(handle_skill_delete))
        .route("/v1/hub/skills", get(handle_hub_skills))
        .route(
            "/v1/hub/skills/:name/install",
            post(handle_hub_skill_install),
        )
        .route(
            "/v1/skills/install-external",
            post(handle_skill_install_external),
        )
        .route("/v1/stats", get(handle_stats))
        // P1: Cron
        .route("/v1/cron", get(handle_cron_list).post(handle_cron_create))
        .route("/v1/cron/:id", delete(handle_cron_delete))
        .route("/v1/cron/:id/run", post(handle_cron_run))
        // Toggles
        .route(
            "/v1/toggles",
            get(handle_toggles_get).put(handle_toggles_update),
        )
        // P2: Alerts
        .route(
            "/v1/alerts",
            get(handle_alerts_list).post(handle_alerts_create),
        )
        .route("/v1/alerts/history", get(handle_alerts_history))
        .route(
            "/v1/alerts/:id",
            put(handle_alerts_update).delete(handle_alerts_delete),
        )
        // P2: Streams
        .route("/v1/streams", get(handle_streams_list))
        .route("/v1/streams/restore", post(handle_streams_restore))
        .route("/v1/streams/:id", delete(handle_stream_delete))
        .route("/v1/streams/:id/data", get(handle_stream_data))
        // Persona files (AGENTS.md, SOUL.md, USER.md, etc.)
        .route("/v1/persona/files", get(handle_persona_list))
        .route(
            "/v1/persona/file",
            get(handle_persona_read).put(handle_persona_write),
        )
        // Pool status
        .route("/v1/pool/status", get(handle_pool_status))
        // P2: Files
        .route("/v1/files", get(handle_files_list))
        .route("/v1/files/content", get(handle_files_content))
        .route("/v1/files/download", get(handle_files_download))
        .route("/v1/files/serve", get(handle_files_serve))
        .route(
            "/v1/files/upload",
            post(handle_files_upload).layer(DefaultBodyLimit::max(MAX_FILE_UPLOAD_BODY_BYTES)),
        )
        .layer(middleware::from_fn_with_state(
            gateway_state.clone(),
            auth_middleware,
        ))
        .layer(build_api_cors_layer(&config))
        // Webhook endpoints — public (no auth), must be outside auth middleware
        .route("/webhook/lark", post(handle_lark_webhook))
        .route(
            "/webhook/wecom",
            get(handle_wecom_webhook).post(handle_wecom_webhook),
        )
        .route("/webhook/qq", post(handle_qq_webhook))
        .with_state(gateway_state);

    let bind_addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;

    let http_shutdown_rx = shutdown_tx.subscribe();
    let http_failure_tx = server_failure_tx.clone();
    let http_handle = tokio::spawn(async move {
        let result = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let mut rx = http_shutdown_rx;
            let _ = rx.recv().await;
        })
        .await;
        let message = match result {
            Ok(()) => "HTTP server stopped unexpectedly".to_string(),
            Err(error) => format!("HTTP server failed: {error}"),
        };
        if !http_failure_tx.is_closed() {
            error!(message = %message);
            let _ = http_failure_tx.send(message);
        }
    });

    // ── WebUI static file server (embedded via rust-embed) ──
    let webui_host = config.gateway.webui_host.clone();
    let webui_port = config.gateway.webui_port;
    let webui_bind = format!("{}:{}", webui_host, webui_port);
    let webui_config = config.clone();
    let webui_app = Router::new()
        .route(
            "/env.js",
            get(move || {
                let cfg = webui_config.clone();
                async move { handle_webui_env_js(cfg).await }
            }),
        )
        .fallback(handle_webui_static)
        .layer(build_webui_cors_layer(&config));
    let webui_listener = tokio::net::TcpListener::bind(&webui_bind).await?;
    let webui_shutdown_rx = shutdown_tx.subscribe();
    let webui_failure_tx = server_failure_tx.clone();
    let webui_handle = tokio::spawn(async move {
        let result = axum::serve(webui_listener, webui_app)
            .with_graceful_shutdown(async move {
                let mut rx = webui_shutdown_rx;
                let _ = rx.recv().await;
            })
            .await;
        let message = match result {
            Ok(()) => "WebUI server stopped unexpectedly".to_string(),
            Err(error) => format!("WebUI server failed: {error}"),
        };
        if !webui_failure_tx.is_closed() {
            error!(message = %message);
            let _ = webui_failure_tx.send(message);
        }
    });
    drop(server_failure_tx);

    // ── Print beautiful startup banner ──
    print_startup_banner(
        &config,
        &host,
        &webui_host,
        webui_port,
        &web_password,
        webui_pass_is_temp,
        is_exposed,
        &bind_addr,
    );

    // ── Wait for shutdown signal or a critical server failure ──
    let shutdown_error = match wait_for_gateway_shutdown(server_failure_rx).await? {
        GatewayShutdownCause::Signal => {
            info!("Shutdown signal received, draining tasks...");
            None
        }
        GatewayShutdownCause::ServerFailure(message) => {
            error!(message = %message, "Critical server stopped; shutting down gateway");
            Some(message)
        }
    };

    shutdown_started.store(true, std::sync::atomic::Ordering::Release);
    let _ = shutdown_tx.send(());
    drop(confirm_tx);
    drop(inbound_tx);
    // Drop local services that still hold inbound_tx clones so runtime can observe
    // channel closure and exit promptly.
    drop(heartbeat_service);

    let mut handles: Vec<(String, tokio::task::JoinHandle<()>)> = vec![
        ("http_server".to_string(), http_handle),
        ("webui_server".to_string(), webui_handle),
        ("confirm_handler".to_string(), confirm_handler_handle),
        ("dispatcher".to_string(), dispatcher_handle),
        ("outbound".to_string(), outbound_handle),
        ("interceptor".to_string(), interceptor_handle),
        ("heartbeat".to_string(), heartbeat_handle),
        ("ghost".to_string(), ghost_handle),
        ("cleanup_loop".to_string(), cleanup_handle),
    ];
    handles.extend(runtime_handles);
    handles.extend(cron_handles);
    handles.extend(channel_handles);

    let total = handles.len();
    let graceful_timeout = std::time::Duration::from_secs(30);
    let deadline = tokio::time::Instant::now() + graceful_timeout;

    // Wait briefly for graceful shutdown.
    loop {
        if handles.iter().all(|(_, h)| h.is_finished()) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Force-stop any stragglers so Ctrl+C returns quickly.
    let mut aborted = 0;
    for (name, handle) in &handles {
        if !handle.is_finished() {
            warn!(
                task = %name,
                "Task did not exit in graceful window, aborting"
            );
            handle.abort();
            aborted += 1;
        }
    }

    let mut failed = 0;
    for (name, handle) in handles {
        match handle.await {
            Ok(()) => {}
            Err(e) if e.is_cancelled() => {
                debug!(task = %name, "Task cancelled during shutdown");
            }
            Err(e) => {
                error!(task = %name, error = %e, "Task panicked during shutdown");
                failed += 1;
            }
        }
    }

    if failed == 0 {
        info!(total, aborted, "Gateway shutdown complete");
    } else {
        warn!(
            failed,
            total, aborted, "Gateway shutdown completed with task failures"
        );
    }

    info!("Gateway stopped");
    if let Some(message) = shutdown_error {
        return Err(anyhow::anyhow!(message));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_agent::task_manager::TaskStatus;

    #[test]
    fn test_confirm_request_id_is_random_uuid_not_timestamp() {
        let first = new_confirm_request_id();
        let second = new_confirm_request_id();

        assert_ne!(first, second);
        assert!(first.starts_with("confirm_"));
        assert_eq!("confirm_".len() + 32, first.len());
        assert!(first["confirm_".len()..]
            .chars()
            .all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn channel_confirmation_requires_an_exact_affirmative_reply() {
        for approved in ["y", "Y", "yes", " YES ", "允许", "确认", "同意", "ok"] {
            assert!(
                is_channel_confirm_approved(approved),
                "expected approval: {approved}"
            );
        }

        for rejected in [
            "n",
            "no",
            "不允许",
            "不同意",
            "不确认",
            "not ok",
            "okay",
            "yes please",
        ] {
            assert!(
                !is_channel_confirm_approved(rejected),
                "expected rejection: {rejected}"
            );
        }
    }

    #[test]
    fn channel_confirmation_scope_includes_account_and_sender() {
        let base = PendingChannelConfirmScope::new("telegram", Some("bot-a"), "chat-1", "user-1");
        let other_account =
            PendingChannelConfirmScope::new("telegram", Some("bot-b"), "chat-1", "user-1");
        let other_sender =
            PendingChannelConfirmScope::new("telegram", Some("bot-a"), "chat-1", "user-2");

        assert_ne!(base, other_account);
        assert_ne!(base, other_sender);
    }

    #[test]
    fn test_progress_throttle_cleanup_removes_terminal_or_missing_tasks() {
        let mut forwarded = HashMap::from([
            ("running-task".to_string(), 20),
            ("failed-task".to_string(), 40),
            ("missing-task".to_string(), 60),
        ]);

        retain_active_progress_throttle_entries(&mut forwarded, |task_id| match task_id {
            "running-task" => Some(TaskStatus::Running),
            "failed-task" => Some(TaskStatus::Failed),
            _ => None,
        });

        assert_eq!(forwarded.len(), 1);
        assert_eq!(forwarded.get("running-task"), Some(&20));
    }

    #[test]
    fn test_stage_progress_throttle_removes_terminal_task_before_100_percent() {
        let mut forwarded = HashMap::from([("failed-task".to_string(), 40)]);

        let should_forward = update_stage_progress_throttle(
            &mut forwarded,
            "failed-task",
            45,
            20,
            Some(&TaskStatus::Failed),
        );

        assert!(!should_forward);
        assert!(!forwarded.contains_key("failed-task"));
    }

    #[test]
    fn test_stage_progress_throttle_forwards_completed_100_percent_and_cleans() {
        let mut forwarded = HashMap::from([("done-task".to_string(), 80)]);

        let should_forward = update_stage_progress_throttle(
            &mut forwarded,
            "done-task",
            100,
            20,
            Some(&TaskStatus::Completed),
        );

        assert!(should_forward);
        assert!(!forwarded.contains_key("done-task"));
    }

    #[test]
    fn test_validate_channel_owner_bindings_requires_owner_for_enabled_channel() {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.token = "token".to_string();

        let err = validate_channel_owner_bindings(&config)
            .expect_err("enabled external channel without owner should fail");
        assert!(
            err.to_string().contains("has no owner agent"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn test_validate_channel_owner_bindings_requires_existing_agent() {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.token = "token".to_string();
        config
            .channel_owners
            .insert("telegram".to_string(), "ghost-agent".to_string());

        let err = validate_channel_owner_bindings(&config)
            .expect_err("owner must exist in agents.list or default fallback");
        assert!(
            err.to_string().contains("does not exist in agents.list"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn test_validate_channel_owner_bindings_accepts_complete_account_owner_coverage() {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.accounts.insert(
            "bot1".to_string(),
            blockcell_core::config::TelegramAccountConfig {
                enabled: true,
                token: "tg-bot1".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );
        config.channels.telegram.accounts.insert(
            "bot2".to_string(),
            blockcell_core::config::TelegramAccountConfig {
                enabled: true,
                token: "tg-bot2".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );
        config
            .agents
            .list
            .push(blockcell_core::config::AgentProfileConfig {
                id: "ops".to_string(),
                enabled: true,
                ..Default::default()
            });
        config.channel_account_owners.insert(
            "telegram".to_string(),
            std::collections::HashMap::from([
                ("bot1".to_string(), "default".to_string()),
                ("bot2".to_string(), "ops".to_string()),
            ]),
        );

        validate_channel_owner_bindings(&config)
            .expect("complete account owner coverage should pass without channel fallback owner");
    }

    #[test]
    fn test_validate_channel_owner_bindings_requires_complete_account_owner_coverage_without_fallback(
    ) {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.accounts.insert(
            "bot1".to_string(),
            blockcell_core::config::TelegramAccountConfig {
                enabled: true,
                token: "tg-bot1".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );
        config.channels.telegram.accounts.insert(
            "bot2".to_string(),
            blockcell_core::config::TelegramAccountConfig {
                enabled: true,
                token: "tg-bot2".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );
        config.channel_account_owners.insert(
            "telegram".to_string(),
            std::collections::HashMap::from([("bot1".to_string(), "default".to_string())]),
        );

        let err = validate_channel_owner_bindings(&config)
            .expect_err("missing account owner coverage should fail without fallback owner");
        assert!(
            err.to_string()
                .contains("missing owner binding for enabled account 'bot2'"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn test_validate_channel_owner_bindings_rejects_unknown_account_owner_agent() {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.accounts.insert(
            "bot1".to_string(),
            blockcell_core::config::TelegramAccountConfig {
                enabled: true,
                token: "tg-bot1".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );
        config.channel_account_owners.insert(
            "telegram".to_string(),
            std::collections::HashMap::from([("bot1".to_string(), "ghost-agent".to_string())]),
        );

        let err =
            validate_channel_owner_bindings(&config).expect_err("account owner agent must exist");
        assert!(
            err.to_string()
                .contains("account owner 'ghost-agent' does not exist"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn test_validate_channel_owner_bindings_accepts_valid_owner() {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.token = "token".to_string();
        config
            .channel_owners
            .insert("telegram".to_string(), "default".to_string());

        validate_channel_owner_bindings(&config)
            .expect("enabled channel with valid owner should pass");
    }

    #[test]
    fn test_route_agent_prefers_default_for_internal_channels() {
        let config = Config::default();
        let msg = InboundMessage::cli("hello");

        assert_eq!(
            resolve_runtime_agent_id(&config, &msg).as_deref(),
            Some("default")
        );
    }

    #[test]
    fn test_route_agent_uses_account_owner_for_external_channels() {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.accounts.insert(
            "bot2".to_string(),
            blockcell_core::config::TelegramAccountConfig {
                enabled: true,
                token: "tg-bot2".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );
        config
            .channel_owners
            .insert("telegram".to_string(), "default".to_string());
        config
            .agents
            .list
            .push(blockcell_core::config::AgentProfileConfig {
                id: "ops".to_string(),
                enabled: true,
                ..Default::default()
            });
        config.channel_account_owners.insert(
            "telegram".to_string(),
            std::collections::HashMap::from([("bot2".to_string(), "ops".to_string())]),
        );

        let msg = InboundMessage {
            channel: "telegram".to_string(),
            account_id: Some("bot2".to_string()),
            sender_id: "u1".to_string(),
            chat_id: "c1".to_string(),
            content: "hello".to_string(),
            media: vec![],
            metadata: serde_json::Value::Null,
            timestamp_ms: 1,
        };

        assert_eq!(
            resolve_runtime_agent_id(&config, &msg).as_deref(),
            Some("ops")
        );
    }

    #[test]
    fn test_route_agent_uses_channel_owner_for_external_channels() {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.token = "token".to_string();
        config
            .channel_owners
            .insert("telegram".to_string(), "ops".to_string());
        config
            .agents
            .list
            .push(blockcell_core::config::AgentProfileConfig {
                id: "ops".to_string(),
                enabled: true,
                name: None,
                intent_profile: Some("ops".to_string()),
                model: None,
                provider: None,
                model_pool: Vec::new(),
                routing_strategy: None,
                max_tokens: None,
                temperature: None,
                max_tool_iterations: None,
                max_tool_iterations_by_tool: HashMap::new(),
                llm_max_retries: None,
                llm_retry_delay_ms: None,
                llm_retry_policy: None,
                max_context_tokens: None,
                evolution_model: None,
                evolution_provider: None,
                allowed_mcp_servers: None,
                allowed_mcp_tools: None,
                tool_presentation: HashMap::new(),
            });

        let msg = InboundMessage {
            channel: "telegram".to_string(),
            account_id: None,
            sender_id: "u1".to_string(),
            chat_id: "c1".to_string(),
            content: "hello".to_string(),
            media: vec![],
            metadata: serde_json::Value::Null,
            timestamp_ms: 1,
        };

        assert_eq!(
            resolve_runtime_agent_id(&config, &msg).as_deref(),
            Some("ops")
        );
    }
    #[test]
    fn test_resolve_requested_agent_defaults_to_default() {
        let config = Config::default();
        assert_eq!(
            resolve_requested_agent_id(&config, None).as_deref(),
            Ok("default")
        );
    }

    #[test]
    fn test_resolve_requested_agent_accepts_enabled_agent() {
        let mut config = Config::default();
        config
            .agents
            .list
            .push(blockcell_core::config::AgentProfileConfig {
                id: "ops".to_string(),
                enabled: true,
                ..Default::default()
            });

        assert_eq!(
            resolve_requested_agent_id(&config, Some("ops")).as_deref(),
            Ok("ops")
        );
    }

    #[test]
    fn test_resolve_requested_agent_rejects_missing_agent() {
        let config = Config::default();
        assert!(resolve_requested_agent_id(&config, Some("ghost")).is_err());
    }

    #[tokio::test]
    async fn server_failure_wakes_gateway_shutdown_waiter() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send("http server failed: test error".to_string())
            .unwrap();

        let cause = wait_for_gateway_shutdown(rx).await.unwrap();
        assert_eq!(
            cause,
            GatewayShutdownCause::ServerFailure("http server failed: test error".to_string())
        );
    }

    #[test]
    fn unexpected_critical_task_exit_is_reported() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let shutting_down = std::sync::atomic::AtomicBool::new(false);

        report_critical_task_exit(&tx, &shutting_down, "runtime:default", None);

        assert_eq!(
            rx.try_recv().unwrap(),
            "Critical task 'runtime:default' stopped unexpectedly"
        );
    }

    #[test]
    fn critical_task_exit_during_shutdown_is_not_reported() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let shutting_down = std::sync::atomic::AtomicBool::new(true);

        report_critical_task_exit(&tx, &shutting_down, "cron:default", None);

        assert!(rx.try_recv().is_err());
    }
}
