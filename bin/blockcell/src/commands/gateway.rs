use anyhow::Context;
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
#[cfg(feature = "napcat")]
use blockcell_channels::napcat::NapCatChannel;
use blockcell_channels::ChannelManager;
use blockcell_core::{Config, InboundMessage, OutboundMessage, Paths};
use blockcell_scheduler::{
    CronJob, CronService, DreamService, DreamServiceConfig, GhostService, GhostServiceConfig, HeartbeatService, JobPayload,
    JobSchedule, JobState, ScheduleKind,
};
use blockcell_skills::{new_registry_handle, CoreEvolution};
use blockcell_skills::{EvolutionService, EvolutionServiceConfig};
use blockcell_tools::mcp::manager::McpManager;
use blockcell_tools::{
    build_tool_registry_for_agent_config, build_tool_registry_with_all_mcp,
    CapabilityRegistryHandle, CoreEvolutionHandle, EventEmitterHandle, MemoryStoreHandle,
    ToolRegistry,
};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{debug, error, info, warn};

use axum::{
    extract::{
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
        Path as AxumPath, Query, State,
    },
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

use super::memory_store::open_memory_store;

mod alerts;
mod banner;
mod capabilities;
mod channels;
mod chat;
mod config_api;
mod cron;
mod files;
mod memory;
mod outbound;
mod sessions;
mod skills_install;
mod streams;
mod toggles;
mod webhooks;
mod websocket;
mod webui;

use alerts::*;
use banner::*;
use capabilities::*;
use channels::*;
use chat::*;
use config_api::*;
use cron::*;
use files::*;
use memory::*;
use outbound::*;
use sessions::*;
use skills_install::*;
use streams::*;
use toggles::*;
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
        client_chat_id: String,
        chat_id: String,
        agent_id: String,
    },
    #[serde(rename = "message_done")]
    MessageDone {
        chat_id: String,
        task_id: String,
        content: String,
        tool_calls: usize,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        media: Vec<String>,
    },
    #[serde(rename = "error")]
    Error { chat_id: String, message: String },
}

// ---------------------------------------------------------------------------
// Shared state passed to HTTP/WS handlers
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct GatewayState {
    inbound_tx: mpsc::Sender<InboundMessage>,
    task_manager: TaskManager,
    config: Config,
    paths: Paths,
    api_token: Option<String>,
    /// Broadcast channel for streaming events to WebSocket clients
    ws_broadcast: broadcast::Sender<String>,
    /// Pending path-confirmation requests waiting for WebUI user response (keyed by request_id)
    pending_confirms: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>>,
    /// Pending path-confirmation requests waiting for non-ws channel user reply (keyed by "channel:chat_id")
    #[allow(dead_code)]
    pending_channel_confirms: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>>,
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
    /// Channel manager for status reporting
    channel_manager: Arc<blockcell_channels::ChannelManager>,
    /// Shared EvolutionService for trigger/delete/status handlers
    evolution_service: Arc<Mutex<EvolutionService>>,
}

#[derive(Deserialize, Default)]
pub(super) struct AgentScopedQuery {
    #[serde(default)]
    pub agent: Option<String>,
}

fn secure_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (&x, &y) in a.as_bytes().iter().zip(b.as_bytes().iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn url_decode(input: &str) -> Option<String> {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' => {
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hi = bytes[i + 1];
                let lo = bytes[i + 2];
                let hex = |c: u8| -> Option<u8> {
                    match c {
                        b'0'..=b'9' => Some(c - b'0'),
                        b'a'..=b'f' => Some(c - b'a' + 10),
                        b'A'..=b'F' => Some(c - b'A' + 10),
                        _ => None,
                    }
                };
                let h = hex(hi)?;
                let l = hex(lo)?;
                out.push((h * 16 + l) as char);
                i += 3;
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    Some(out)
}

fn token_from_query(req: &Request<axum::body::Body>) -> Option<String> {
    let q = req.uri().query()?;
    for pair in q.split('&') {
        let (k, v) = pair.split_once('=')?;

        if k == "token" {
            return url_decode(v);
        }
    }
    None
}

fn validate_workspace_relative_path(path: &str) -> Result<std::path::PathBuf, String> {
    if path.trim().is_empty() {
        return Err("path is required".to_string());
    }
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return Err("absolute paths are not allowed".to_string());
    }
    let mut normalized = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(s) => normalized.push(s),
            std::path::Component::ParentDir => {
                return Err("path traversal (..) is not allowed".to_string());
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err("invalid path".to_string());
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err("invalid path".to_string());
    }
    Ok(normalized)
}

fn primary_pool_entry(config: &Config) -> Option<&blockcell_core::config::ModelEntry> {
    config
        .agents
        .defaults
        .model_pool
        .iter()
        .min_by(|a, b| a.priority.cmp(&b.priority).then(b.weight.cmp(&a.weight)))
}

fn active_model_and_provider(config: &Config) -> (String, Option<String>, &'static str) {
    if let Some(entry) = primary_pool_entry(config) {
        return (
            entry.model.clone(),
            Some(entry.provider.clone()),
            "modelPool",
        );
    }

    (
        config.agents.defaults.model.clone(),
        config.agents.defaults.provider.clone(),
        "agents.defaults",
    )
}

const EXTERNAL_CHANNELS: [&str; 11] = [
    "telegram", "whatsapp", "feishu", "slack", "discord", "dingtalk", "wecom", "lark", "qq",
    "napcat", "weixin",
];

fn known_channel_account_ids(config: &Config, channel: &str) -> Vec<String> {
    let mut ids = match channel {
        "telegram" => config
            .channels
            .telegram
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "whatsapp" => config
            .channels
            .whatsapp
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "feishu" => config
            .channels
            .feishu
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "slack" => config
            .channels
            .slack
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "discord" => config
            .channels
            .discord
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "dingtalk" => config
            .channels
            .dingtalk
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "wecom" => config
            .channels
            .wecom
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "lark" => config
            .channels
            .lark
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "weixin" => config
            .channels
            .weixin
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "qq" => config
            .channels
            .qq
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        "napcat" => config
            .channels
            .napcat
            .accounts
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    ids.sort();
    ids
}

fn enabled_channel_account_ids(config: &Config, channel: &str) -> Vec<String> {
    let mut ids = match channel {
        "telegram" => config
            .channels
            .telegram
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.token.trim().is_empty())
            .map(|(account_id, _)| account_id.clone())
            .collect::<Vec<_>>(),
        "whatsapp" => config
            .channels
            .whatsapp
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.bridge_url.trim().is_empty())
            .map(|(account_id, _)| account_id.clone())
            .collect::<Vec<_>>(),
        "feishu" => config
            .channels
            .feishu
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.app_id.trim().is_empty())
            .map(|(account_id, _)| account_id.clone())
            .collect::<Vec<_>>(),
        "slack" => config
            .channels
            .slack
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.bot_token.trim().is_empty())
            .map(|(account_id, _)| account_id.clone())
            .collect::<Vec<_>>(),
        "discord" => config
            .channels
            .discord
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.bot_token.trim().is_empty())
            .map(|(account_id, _)| account_id.clone())
            .collect::<Vec<_>>(),
        "dingtalk" => config
            .channels
            .dingtalk
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.app_key.trim().is_empty())
            .map(|(account_id, _)| account_id.clone())
            .collect::<Vec<_>>(),
        "wecom" => config
            .channels
            .wecom
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.corp_id.trim().is_empty())
            .map(|(account_id, _)| account_id.clone())
            .collect::<Vec<_>>(),
        "lark" => config
            .channels
            .lark
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.app_id.trim().is_empty())
            .map(|(account_id, _)| account_id.clone())
            .collect::<Vec<_>>(),
        "weixin" => config
            .channels
            .weixin
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.token.trim().is_empty())
            .map(|(account_id, _)| account_id.clone())
            .collect::<Vec<_>>(),
        "qq" => config
            .channels
            .qq
            .accounts
            .iter()
            .filter(|(_, account)| account.enabled && !account.app_id.trim().is_empty())
            .map(|(account_id, _)| account_id.clone())
            .collect::<Vec<_>>(),
        "napcat" => config
            .channels
            .napcat
            .accounts
            .iter()
            .filter(|(_, account)| {
                account.enabled
                    && account
                        .ws_url
                        .as_ref()
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false)
            })
            .map(|(account_id, _)| account_id.clone())
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    ids.sort();
    ids
}

fn validate_channel_owner_bindings(config: &Config) -> anyhow::Result<()> {
    for channel in EXTERNAL_CHANNELS {
        let account_owner_bindings = config.channel_account_owners.get(channel);
        let known_account_ids = known_channel_account_ids(config, channel);

        if let Some(bindings) = account_owner_bindings {
            for (account_id, owner) in bindings {
                let account_id = account_id.trim();
                let owner = owner.trim();
                if account_id.is_empty() {
                    return Err(anyhow::anyhow!(
                        "Channel '{}' has an empty account id in channelAccountOwners.",
                        channel
                    ));
                }
                if owner.is_empty() {
                    return Err(anyhow::anyhow!(
                        "Channel '{}' account '{}' has a blank owner agent.",
                        channel,
                        account_id
                    ));
                }
                if !known_account_ids.iter().any(|id| id == account_id) {
                    return Err(anyhow::anyhow!(
                        "Channel '{}' account '{}' is not defined under channels.{}.accounts.",
                        channel,
                        account_id,
                        channel
                    ));
                }
                if !config.agent_exists(owner) {
                    return Err(anyhow::anyhow!(
                        "Channel '{}' account owner '{}' does not exist in agents.list.",
                        channel,
                        owner
                    ));
                }
            }
        }

        if !config.is_external_channel_enabled(channel) {
            continue;
        }

        if let Some(owner) = config.resolve_channel_owner(channel) {
            if !config.agent_exists(owner) {
                return Err(anyhow::anyhow!(
                    "Channel '{}' owner '{}' does not exist in agents.list.",
                    channel,
                    owner
                ));
            }
            continue;
        }

        let enabled_account_ids = enabled_channel_account_ids(config, channel);
        if enabled_account_ids.is_empty() {
            return Err(anyhow::anyhow!(
                "Channel '{}' is enabled but has no owner agent. Set channelOwners.{} in config.",
                channel,
                channel
            ));
        }

        for account_id in enabled_account_ids {
            if config
                .resolve_channel_account_owner(channel, &account_id)
                .is_none()
            {
                return Err(anyhow::anyhow!(
                    "Channel '{}' is enabled but missing owner binding for enabled account '{}'. Set channelAccountOwners.{}.{} or channelOwners.{}.",
                    channel,
                    account_id,
                    channel,
                    account_id,
                    channel
                ));
            }
        }
    }
    Ok(())
}

fn is_internal_channel(channel: &str) -> bool {
    matches!(
        channel,
        "ws" | "cli" | "cron" | "system" | "subagent" | "heartbeat" | "ghost"
    )
}

fn metadata_route_agent_id(msg: &InboundMessage) -> Option<String> {
    msg.metadata
        .get("route_agent_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn resolve_runtime_agent_id(config: &Config, msg: &InboundMessage) -> Option<String> {
    if let Some(agent_id) = metadata_route_agent_id(msg) {
        return config.agent_exists(&agent_id).then_some(agent_id);
    }

    if is_internal_channel(&msg.channel) {
        return Some("default".to_string());
    }

    let owner = config.resolve_effective_channel_owner(&msg.channel, msg.account_id.as_deref())?;
    config.agent_exists(owner).then(|| owner.to_string())
}

fn resolve_requested_agent_id(
    config: &Config,
    requested: Option<&str>,
) -> std::result::Result<String, String> {
    let agent_id = requested
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default");

    if config.agent_exists(agent_id) {
        Ok(agent_id.to_string())
    } else {
        Err(format!("Unknown agent '{}'", agent_id))
    }
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

fn with_route_agent_id(mut msg: InboundMessage, agent_id: &str) -> InboundMessage {
    let mut metadata = if msg.metadata.is_object() {
        msg.metadata
    } else {
        serde_json::json!({})
    };

    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("route_agent_id".to_string(), serde_json::json!(agent_id));
        if !is_internal_channel(&msg.channel) {
            obj.entry("route_match_level".to_string())
                .or_insert_with(|| serde_json::json!("channel_owner"));
        }
    }

    msg.metadata = metadata;
    msg
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
    let cap_registry_handle: CapabilityRegistryHandle = Arc::new(Mutex::new(cap_registry_adapter));

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
    runtime.set_capability_registry(cap_registry_handle);
    runtime.set_core_evolution(core_evo_handle);
    runtime.set_event_tx(ws_broadcast_tx);

    // Initialize Layer 5 memory injector (7-layer memory system)
    if let Err(e) = runtime.init_memory_injector().await {
        warn!(error = %e, "Failed to initialize memory injector");
    }

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

    if req.uri().path() == "/v1/health" || req.uri().path() == "/v1/auth/login" {
        return next.run(req).await;
    }

    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let authorized = match auth_header {
        Some(h) if h.starts_with("Bearer ") => secure_eq(&h[7..], token.as_str()),
        _ => false,
    };

    let authorized = authorized
        || token_from_query(&req)
            .map(|v| secure_eq(&v, token.as_str()))
            .unwrap_or(false);

    if authorized {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            "Unauthorized: invalid or missing Bearer token",
        )
            .into_response()
    }
}

// ---------------------------------------------------------------------------
// Main gateway entry point
// ---------------------------------------------------------------------------

fn default_env_template() -> &'static str {
    "BLOCKCELL_API_TOKEN=\nOPENAI_API_KEY=\nANTHROPIC_API_KEY=\nGEMINI_API_KEY=\n"
}

fn parse_env_assignment(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let body = trimmed.strip_prefix("export ").unwrap_or(trimmed).trim();
    let (key, value) = body.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }

    let mut value = value.trim().to_string();
    if value.len() >= 2 {
        let quoted = (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''));
        if quoted {
            value = value[1..value.len() - 1].to_string();
        }
    }

    Some((key.to_string(), value))
}

fn ensure_and_load_gateway_env(paths: &Paths) -> anyhow::Result<()> {
    paths.ensure_dirs().with_context(|| {
        format!(
            "failed to create blockcell dirs at {}",
            paths.base.display()
        )
    })?;

    let env_path = paths.env_file();
    if !env_path.exists() {
        fs::write(&env_path, default_env_template()).with_context(|| {
            format!(
                "failed to create default env file at {}",
                env_path.display()
            )
        })?;
        info!(path = %env_path.display(), "Created default gateway .env file");
    }

    load_env_file(&env_path)
}

fn load_env_file(path: &Path) -> anyhow::Result<()> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read env file {}", path.display()))?;

    let mut loaded = 0usize;
    for line in content.lines() {
        if let Some((key, value)) = parse_env_assignment(line) {
            unsafe {
                std::env::set_var(key, value);
            }
            loaded += 1;
        }
    }

    info!(path = %path.display(), loaded, "Loaded gateway env file");
    Ok(())
}

pub async fn run(cli_host: Option<String>, cli_port: Option<u16>) -> anyhow::Result<()> {
    let paths = Paths::new();
    ensure_and_load_gateway_env(&paths)?;
    let mut config = Config::load_or_default(&paths)?;

    // Ensure autoUpgrade.manifestUrl has a value (migrates old configs with empty string)
    if config.auto_upgrade.manifest_url.is_empty() {
        config.auto_upgrade.manifest_url =
            "https://github.com/blockcell-labs/blockcell/releases/latest/download/manifest.json"
                .to_string();
        let _ = config.save(&paths.config_file());
    }

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

    // ── Create message bus ──
    let bus = MessageBus::new(100);
    let ((inbound_tx, inbound_rx), (outbound_tx, outbound_rx)) = bus.split();

    // ── Create WebSocket broadcast channel ──
    let (ws_broadcast_tx, _) = broadcast::channel::<String>(1000);

    // ── Create shutdown channel ──
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // ── Create shared task manager ──
    let task_manager = TaskManager::new();
    let mcp_manager = Arc::new(McpManager::load(&paths).await?);

    // ── Create tool registry (shared for listing tools) ──
    let tool_registry_shared =
        Arc::new(build_tool_registry_with_all_mcp(Some(&mcp_manager)).await?);

    // ── Set up path confirmation channel (channel-aware) ──
    // pending_ws_confirms: keyed by request_id, for WebUI (ws) confirmations
    let pending_ws_confirms: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    // pending_channel_confirms: keyed by "channel:chat_id", for non-ws channel confirmations
    let pending_channel_confirms: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>> =
        Arc::new(Mutex::new(HashMap::new()));
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
                let request_id = format!("confirm_{}", chrono::Utc::now().timestamp_millis());
                {
                    let mut map = pending_ws_for_handler.lock().await;
                    map.insert(request_id.clone(), req.response_tx);
                }
                let event = serde_json::json!({
                    "type": "confirm_request",
                    "request_id": request_id,
                    "tool_name": req.tool_name,
                    "paths": req.paths,
                    "channel": req.channel,
                    "chat_id": req.chat_id,
                });
                let _ = ws_broadcast_for_confirm.send(event.to_string());
            } else {
                let confirm_key = format!("{}:{}", req.channel, req.chat_id);
                {
                    let mut map = pending_ch_for_handler.lock().await;
                    map.insert(confirm_key.clone(), req.response_tx);
                }
                let prompt = format!(
                    "⚠️ 工具 {} 需要访问以下路径：
{}

回复 yes / y / 允许 / 同意 进行确认，其他任意内容将拒绝。",
                    req.tool_name,
                    req.paths.join("\n")
                );
                let mut outbound = OutboundMessage::new(&req.channel, &req.chat_id, &prompt);
                outbound.metadata = serde_json::json!({"confirm_request": true});
                if outbound_tx_for_confirm.send(outbound).await.is_err() {
                    let mut map = pending_ch_for_handler.lock().await;
                    if let Some(tx) = map.remove(&confirm_key) {
                        let _ = tx.send(false);
                    }
                } else {
                    info!(confirm_key = %confirm_key, tool = %req.tool_name, "Sent confirm_request to channel");
                }
            }
        }
    });

    // ── Create one runtime per resolved agent ──
    let resolved_agents = config.resolved_agents();
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
        )
        .await?;
        if let Some(memory_store_handle) = memory_store_handle {
            agent_memory_stores.insert(agent_id.clone(), memory_store_handle);
        }
        task_manager.register_event_emitter(Some(&agent_id), event_emitter.clone());
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
            enabled: true,
            check_interval_secs: 10 * 60, // 10 分钟检查一次
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

    // ── Create Ghost Agent service ──
    let ghost_config = GhostServiceConfig::from_config(&config);
    let ghost_service = GhostService::new(ghost_config, paths.clone(), inbound_tx.clone());

    // ── Inbound interceptor: check for pending channel confirm replies ──
    // Sits between channel inbound_rx and the runtime, intercepting confirm
    // replies from non-ws channels before they reach the runtime loop.
    let (filtered_inbound_tx, filtered_inbound_rx) = mpsc::channel::<InboundMessage>(100);
    let pending_ch_for_interceptor = Arc::clone(&pending_channel_confirms);
    let mut interceptor_shutdown_rx = shutdown_tx.subscribe();
    // 斜杠命令拦截需要的变量
    let slash_paths = paths.clone();
    let slash_task_manager = task_manager.clone();
    let slash_outbound_tx = outbound_tx.clone();
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
                let confirm_key = format!("{}:{}", msg.channel, msg.chat_id);
                let maybe_tx = {
                    let mut map = pending_ch_for_interceptor.lock().await;
                    map.remove(&confirm_key)
                };
                if let Some(tx) = maybe_tx {
                    // Parse the reply as a confirm response
                    let text = msg.content.trim().to_lowercase();
                    let approved = text == "y"
                        || text == "yes"
                        || text.contains("允许")
                        || text.contains("确认")
                        || text.contains("同意")
                        || text.contains("ok");
                    info!(
                        confirm_key = %confirm_key,
                        approved = approved,
                        reply = %msg.content.trim(),
                        "Channel confirm reply intercepted"
                    );
                    let _ = tx.send(approved);
                    continue; // Don't forward this message to the runtime
                }
            }

            // 斜杠命令拦截（在 confirm reply 检查之后，转发给 runtime 之前）
            if !is_internal_channel(&msg.channel) && msg.content.starts_with('/') {
                use crate::commands::slash_commands::{
                    CommandContext, CommandResult, SLASH_COMMAND_HANDLER,
                };

                let ctx = CommandContext::for_channel(
                    slash_paths.clone(),
                    slash_task_manager.clone(),
                    msg.channel.clone(),
                    msg.chat_id.clone(),
                    Some(msg.sender_id.clone()),
                );

                match SLASH_COMMAND_HANDLER.try_handle(&msg.content, &ctx).await {
                    CommandResult::Handled(response) => {
                        // 发送响应回原渠道
                        let reply = OutboundMessage::new(&msg.channel, &msg.chat_id, &response.content);
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
                        // 命令需要转发给 AgentRuntime（如 /learn）
                        info!(
                            channel = %msg.channel,
                            command = %original_command,
                            "Forwarding command to AgentRuntime"
                        );
                        // 修改消息内容为转换后的内容
                        msg.content = transformed_content;
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

    #[cfg(feature = "telegram")]
    for listener in blockcell_channels::account::telegram_listener_configs(&config) {
        let listener_name = listener.label.clone();
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
        let whatsapp = Arc::new(WhatsAppChannel::new(listener.config, inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        channel_handles.push((
            listener_name,
            tokio::spawn(async move {
                whatsapp.run_loop(shutdown_rx).await;
            }),
        ));
    }

    #[cfg(feature = "feishu")]
    for listener in blockcell_channels::account::feishu_scoped_configs(&config) {
        let listener_name = listener.label.clone();
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
        let napcat = Arc::new(NapCatChannel::new(listener.config, inbound_tx.clone()));
        let shutdown_rx = shutdown_tx.subscribe();
        channel_handles.push((
            listener_name,
            tokio::spawn(async move {
                napcat.run_loop(shutdown_rx).await;
            }),
        ));
    }

    #[cfg(feature = "weixin")]
    for listener in blockcell_channels::account::weixin_listener_configs(&config) {
        let listener_name = listener.label.clone();
        info!(listener = %listener_name, "Starting Weixin listener");
        let weixin = Arc::new(
            blockcell_channels::weixin::WeixinChannel::new(listener.config, inbound_tx.clone()),
        );
        let shutdown_rx = shutdown_tx.subscribe();
        channel_handles.push((
            listener_name,
            tokio::spawn(async move {
                weixin.run_loop(shutdown_rx).await;
            }),
        ));
    }

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
            let tmp = format!("{:08x}", rand_u32());
            (tmp, true)
        }
    };

    let is_exposed = host == "0.0.0.0" || host == "::";

    // Create a shared EvolutionService for the HTTP handlers (trigger, delete, status).
    // This is separate from the one inside AgentRuntime but shares the same disk records.
    let shared_evo_service = Arc::new(Mutex::new(EvolutionService::new(
        paths.skills_dir(),
        EvolutionServiceConfig::default(),
    )));

    let gateway_state = GatewayState {
        inbound_tx: inbound_tx.clone(),
        task_manager,
        config: config.clone(),
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
        channel_manager: Arc::clone(&channel_manager),
        evolution_service: shared_evo_service,
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
        // Ghost Agent
        .route(
            "/v1/ghost/config",
            get(handle_ghost_config_get).put(handle_ghost_config_update),
        )
        .route("/v1/ghost/activity", get(handle_ghost_activity))
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
        .route("/v1/files/upload", post(handle_files_upload))
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
    let http_handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let mut rx = http_shutdown_rx;
                let _ = rx.recv().await;
            })
            .await
            .ok();
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
    let webui_handle = tokio::spawn(async move {
        axum::serve(webui_listener, webui_app)
            .with_graceful_shutdown(async move {
                let mut rx = webui_shutdown_rx;
                let _ = rx.recv().await;
            })
            .await
            .ok();
    });

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

    // ── Wait for shutdown signal ──
    tokio::signal::ctrl_c().await?;
    info!("Shutdown signal received, draining tasks...");

    let _ = shutdown_tx.send(());
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
    Ok(())
}

fn build_api_cors_layer(config: &Config) -> CorsLayer {
    let _ = config;
    CorsLayer::permissive().allow_credentials(false)
}

fn build_webui_cors_layer(config: &Config) -> CorsLayer {
    let _ = config;
    CorsLayer::permissive().allow_credentials(false)
}

#[cfg(test)]
mod tests {
    use super::*;

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
                max_tokens: None,
                temperature: None,
                max_tool_iterations: None,
                max_tool_iterations_by_tool: HashMap::new(),
                llm_max_retries: None,
                llm_retry_delay_ms: None,
                max_context_tokens: None,
                evolution_model: None,
                evolution_provider: None,
                allowed_mcp_servers: None,
                allowed_mcp_tools: None,
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
}
