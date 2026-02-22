use blockcell_core::{Config, InboundMessage, OutboundMessage, Paths, Result};
use blockcell_core::types::{ChatMessage, ToolCallRequest};
use blockcell_providers::{Provider, OpenAIProvider, AnthropicProvider, GeminiProvider, OllamaProvider};
use blockcell_storage::{SessionStore, AuditLogger};
use blockcell_tools::{ToolRegistry, TaskManagerHandle, MemoryStoreHandle, SpawnHandle, CapabilityRegistryHandle, CoreEvolutionHandle};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};
use regex::Regex;

use crate::context::ContextBuilder;
use crate::task_manager::TaskManager;

/// Adapter that wraps a Provider to implement the skills::LLMProvider trait.
/// This allows EvolutionService to call the LLM for code generation without
/// depending on the full provider stack.
struct ProviderLLMAdapter {
    provider: Arc<dyn blockcell_providers::Provider>,
}

#[async_trait::async_trait]
impl blockcell_skills::LLMProvider for ProviderLLMAdapter {
    async fn generate(&self, prompt: &str) -> blockcell_core::Result<String> {
        let messages = vec![
            ChatMessage::system("You are a skill evolution assistant. Follow instructions precisely."),
            ChatMessage::user(prompt),
        ];
        let response = self.provider.chat(&messages, &[]).await?;
        Ok(response.content.unwrap_or_default())
    }
}

/// A SpawnHandle implementation that captures everything needed to spawn
/// subagents, without requiring a reference to AgentRuntime.
#[derive(Clone)]
pub struct RuntimeSpawnHandle {
    config: Config,
    paths: Paths,
    task_manager: TaskManager,
    outbound_tx: Option<mpsc::Sender<OutboundMessage>>,
}

impl SpawnHandle for RuntimeSpawnHandle {
    fn spawn(
        &self,
        task: &str,
        label: &str,
        origin_channel: &str,
        origin_chat_id: &str,
    ) -> Result<serde_json::Value> {
        let task_id = uuid::Uuid::new_v4().to_string();

        info!(
            task_id = %task_id,
            label = %label,
            "Spawning subagent via SpawnHandle"
        );

        // Create isolated provider for the subagent
        let provider = AgentRuntime::create_subagent_provider(&self.config)
            .ok_or_else(|| blockcell_core::Error::Config("No provider configured".to_string()))?;

        // Gather everything the background task needs
        let config = self.config.clone();
        let paths = self.paths.clone();
        let task_manager = self.task_manager.clone();
        let outbound_tx = self.outbound_tx.clone();
        let task_str = task.to_string();
        let task_id_clone = task_id.clone();
        let label_clone = label.to_string();
        let origin_channel = origin_channel.to_string();
        let origin_chat_id = origin_chat_id.to_string();

        // Spawn the background task. Task registration (create_task) happens inside
        // run_subagent_task before set_running(), eliminating the race condition.
        tokio::spawn(run_subagent_task(
            config,
            paths,
            provider,
            task_manager,
            outbound_tx,
            task_str,
            task_id_clone,
            label_clone,
            origin_channel,
            origin_chat_id,
        ));

        Ok(serde_json::json!({
            "task_id": task_id,
            "label": label,
            "status": "running",
            "note": "Subagent is now processing this task in the background. Use list_tasks to check progress."
        }))
    }
}

/// A request sent from the runtime to the UI layer asking the user to confirm
/// an operation that accesses paths outside the safe workspace directory.
pub struct ConfirmRequest {
    pub tool_name: String,
    pub paths: Vec<String>,
    pub response_tx: tokio::sync::oneshot::Sender<bool>,
}

/// Truncate a string at a safe char boundary.
fn truncate_str(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Summarize a result to 1-2 sentences
#[allow(dead_code)]
fn summarize_result(result: &str) -> String {
    let max_chars = 200;
    if result.chars().count() <= max_chars {
        result.to_string()
    } else {
        format!("{}... (truncated)", truncate_str(result, max_chars))
    }
}

fn is_im_channel(channel: &str) -> bool {
    matches!(
        channel,
        "wecom" | "feishu" | "lark" | "telegram" | "slack" | "discord" | "dingtalk" | "whatsapp"
    )
}

fn user_wants_send_image(text: &str) -> bool {
    let t = text.to_lowercase();
    let has_send = t.contains("发") || t.contains("发送") || t.contains("发给") || t.contains("send");
    let has_image = t.contains("图片")
        || t.contains("照片")
        || t.contains("相片")
        || t.contains("截图")
        || t.contains("图像")
        || t.contains("image")
        || t.contains("photo");
    has_send && has_image
}

fn chat_message_text(msg: &ChatMessage) -> String {
    match &msg.content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

async fn pick_image_path(paths: &Paths, history: &[ChatMessage]) -> Option<String> {
    let re_abs = Regex::new(r#"(/[^\s`"']+\.(?i:jpg|jpeg|png|gif|webp|bmp))"#).ok()?;
    let re_name = Regex::new(r#"([A-Za-z0-9._-]+\.(?i:jpg|jpeg|png|gif|webp|bmp))"#).ok()?;

    let media_dir = paths.media_dir();

    for msg in history.iter().rev() {
        let text = chat_message_text(msg);

        for cap in re_abs.captures_iter(&text) {
            let p = cap.get(1)?.as_str().to_string();
            if tokio::fs::metadata(&p).await.is_ok() {
                let ok_under_media_dir = std::fs::canonicalize(&p)
                    .ok()
                    .and_then(|cp| std::fs::canonicalize(&media_dir).ok().map(|md| (cp, md)))
                    .map(|(cp, md)| cp.starts_with(md))
                    .unwrap_or(false);
                if ok_under_media_dir {
                    return Some(p);
                }
            }
        }

        for cap in re_name.captures_iter(&text) {
            let file_name = cap.get(1)?.as_str();
            let p = media_dir.join(file_name);
            if tokio::fs::metadata(&p).await.is_ok() {
                return Some(p.display().to_string());
            }
        }
    }

    let mut rd = tokio::fs::read_dir(&media_dir).await.ok()?;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp") {
            return Some(p.display().to_string());
        }
    }

    None
}

fn overwrite_last_assistant_message(history: &mut [ChatMessage], new_text: &str) {
    if let Some(last) = history.last_mut() {
        if last.role == "assistant" {
            last.content = serde_json::Value::String(new_text.to_string());
        }
    }
}

/// Read toggles.json and return the set of disabled item names for a category.
/// Returns an empty set if the file doesn't exist or can't be parsed.
fn load_disabled_toggles(paths: &Paths, category: &str) -> HashSet<String> {
    let path = paths.toggles_file();
    let mut disabled = HashSet::new();
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(obj) = val.get(category).and_then(|v| v.as_object()) {
                for (name, enabled) in obj {
                    if enabled == false {
                        disabled.insert(name.clone());
                    }
                }
            }
        }
    }
    disabled
}

pub struct AgentRuntime {
    config: Config,
    paths: Paths,
    context_builder: ContextBuilder,
    provider: Arc<dyn Provider>,
    tool_registry: ToolRegistry,
    session_store: SessionStore,
    audit_logger: AuditLogger,
    outbound_tx: Option<mpsc::Sender<OutboundMessage>>,
    inbound_tx: Option<mpsc::Sender<InboundMessage>>,
    confirm_tx: Option<mpsc::Sender<ConfirmRequest>>,
    /// Directories that the user has already authorized access to.
    /// Files within these directories will not require separate confirmation.
    authorized_dirs: HashSet<PathBuf>,
    /// Shared task manager for tracking background subagent tasks.
    task_manager: TaskManager,
    /// Shared memory store handle for tools.
    memory_store: Option<MemoryStoreHandle>,
    /// Capability registry handle for tools.
    capability_registry: Option<CapabilityRegistryHandle>,
    /// Core evolution engine handle for tools.
    core_evolution: Option<CoreEvolutionHandle>,
    /// Broadcast sender for streaming events to WebSocket clients (gateway mode).
    event_tx: Option<broadcast::Sender<String>>,
    /// Cooldown tracker: capability_id → last auto-request timestamp (epoch secs).
    /// Prevents repeated auto-triggering of the same capability within 24h.
    cap_request_cooldown: HashMap<String, i64>,
}

impl AgentRuntime {
    pub fn new(
        config: Config,
        paths: Paths,
        provider: Box<dyn Provider>,
        tool_registry: ToolRegistry,
    ) -> Result<Self> {
        let mut context_builder = ContextBuilder::new(paths.clone(), config.clone());

        // Wrap the provider in an Arc so it can be shared with the EvolutionService.
        // This enables tick() to automatically drive the full evolution pipeline.
        let provider_arc: Arc<dyn Provider> = Arc::from(provider);
        let llm_adapter = Arc::new(ProviderLLMAdapter {
            provider: provider_arc.clone(),
        });
        context_builder.set_evolution_llm_provider(llm_adapter);

        let session_store = SessionStore::new(paths.clone());
        let audit_logger = AuditLogger::new(paths.clone());

        Ok(Self {
            config,
            paths,
            context_builder,
            provider: provider_arc,
            tool_registry,
            session_store,
            audit_logger,
            outbound_tx: None,
            inbound_tx: None,
            confirm_tx: None,
            authorized_dirs: HashSet::new(),
            task_manager: TaskManager::new(),
            memory_store: None,
            capability_registry: None,
            core_evolution: None,
            event_tx: None,
            cap_request_cooldown: HashMap::new(),
        })
    }

    pub fn set_outbound(&mut self, tx: mpsc::Sender<OutboundMessage>) {
        self.outbound_tx = Some(tx);
    }

    pub fn set_inbound(&mut self, tx: mpsc::Sender<InboundMessage>) {
        self.inbound_tx = Some(tx);
    }

    pub fn set_confirm(&mut self, tx: mpsc::Sender<ConfirmRequest>) {
        self.confirm_tx = Some(tx);
    }

    /// Get a reference to the task manager.
    pub fn task_manager(&self) -> &TaskManager {
        &self.task_manager
    }

    /// Set a shared task manager (e.g. from the command layer).
    pub fn set_task_manager(&mut self, tm: TaskManager) {
        self.task_manager = tm;
    }

    /// Set the broadcast sender for streaming events to WebSocket clients.
    pub fn set_event_tx(&mut self, tx: broadcast::Sender<String>) {
        self.event_tx = Some(tx);
    }

    /// Set the memory store handle for tools and context builder.
    pub fn set_memory_store(&mut self, store: MemoryStoreHandle) {
        self.memory_store = Some(store.clone());
        self.context_builder.set_memory_store(store);
    }

    /// Set the capability registry handle for tools.
    pub fn set_capability_registry(&mut self, registry: CapabilityRegistryHandle) {
        self.capability_registry = Some(registry);
    }

    /// Set the core evolution engine handle for tools.
    pub fn set_core_evolution(&mut self, core_evo: CoreEvolutionHandle) {
        self.core_evolution = Some(core_evo);
    }

    /// Create a restricted tool registry for subagents (no spawn, no message, no cron).
    pub(crate) fn subagent_tool_registry() -> ToolRegistry {
        use blockcell_tools::fs::*;
        use blockcell_tools::exec::ExecTool;
        use blockcell_tools::web::*;
        use blockcell_tools::tasks::ListTasksTool;
        use blockcell_tools::browser::BrowseTool;
        use blockcell_tools::memory::{MemoryQueryTool, MemoryUpsertTool, MemoryForgetTool};
        use blockcell_tools::skills::ListSkillsTool;
        use blockcell_tools::system_info::{SystemInfoTool, CapabilityEvolveTool};
        use blockcell_tools::camera::CameraCaptureTool;
        use blockcell_tools::chrome_control::ChromeControlTool;
        use blockcell_tools::app_control::AppControlTool;
        use blockcell_tools::file_ops::FileOpsTool;
        use blockcell_tools::data_process::DataProcessTool;
        use blockcell_tools::http_request::HttpRequestTool;
        use blockcell_tools::email::EmailTool;
        use blockcell_tools::audio_transcribe::AudioTranscribeTool;
        use blockcell_tools::chart_generate::ChartGenerateTool;
        use blockcell_tools::office_write::OfficeWriteTool;
        use blockcell_tools::calendar_api::CalendarApiTool;
        use blockcell_tools::iot_control::IotControlTool;
        use blockcell_tools::tts::TtsTool;
        use blockcell_tools::ocr::OcrTool;
        use blockcell_tools::image_understand::ImageUnderstandTool;
        use blockcell_tools::social_media::SocialMediaTool;
        use blockcell_tools::notification::NotificationTool;
        use blockcell_tools::cloud_api::CloudApiTool;
        use blockcell_tools::git_api::GitApiTool;
        use blockcell_tools::finance_api::FinanceApiTool;
        use blockcell_tools::video_process::VideoProcessTool;
        use blockcell_tools::health_api::HealthApiTool;
        use blockcell_tools::map_api::MapApiTool;
        use blockcell_tools::contacts::ContactsTool;
        use blockcell_tools::encrypt::EncryptTool;
        use blockcell_tools::network_monitor::NetworkMonitorTool;
        use blockcell_tools::knowledge_graph::KnowledgeGraphTool;
        use blockcell_tools::stream_subscribe::StreamSubscribeTool;
        use blockcell_tools::alert_rule::AlertRuleTool;
        use blockcell_tools::blockchain_rpc::BlockchainRpcTool;
        use blockcell_tools::exchange_api::ExchangeApiTool;
        use blockcell_tools::blockchain_tx::BlockchainTxTool;
        use blockcell_tools::contract_security::ContractSecurityTool;
        use blockcell_tools::bridge_api::BridgeApiTool;
        use blockcell_tools::nft_market::NftMarketTool;
        use blockcell_tools::multisig::MultisigTool;
        use blockcell_tools::community_hub::CommunityHubTool;
        use blockcell_tools::memory_maintenance::MemoryMaintenanceTool;
        use blockcell_tools::toggle_manage::ToggleManageTool;
        use blockcell_tools::termux_api::TermuxApiTool;

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadFileTool));
        registry.register(Arc::new(WriteFileTool));
        registry.register(Arc::new(EditFileTool));
        registry.register(Arc::new(ListDirTool));
        registry.register(Arc::new(ExecTool));
        registry.register(Arc::new(WebSearchTool));
        registry.register(Arc::new(WebFetchTool));
        registry.register(Arc::new(ListTasksTool));
        registry.register(Arc::new(BrowseTool));
        registry.register(Arc::new(MemoryQueryTool));
        registry.register(Arc::new(MemoryUpsertTool));
        registry.register(Arc::new(MemoryForgetTool));
        registry.register(Arc::new(ListSkillsTool));
        registry.register(Arc::new(SystemInfoTool));
        registry.register(Arc::new(CapabilityEvolveTool));
        registry.register(Arc::new(CameraCaptureTool));
        registry.register(Arc::new(ChromeControlTool));
        registry.register(Arc::new(AppControlTool));
        registry.register(Arc::new(FileOpsTool));
        registry.register(Arc::new(DataProcessTool));
        registry.register(Arc::new(HttpRequestTool));
        registry.register(Arc::new(EmailTool));
        registry.register(Arc::new(AudioTranscribeTool));
        registry.register(Arc::new(ChartGenerateTool));
        registry.register(Arc::new(OfficeWriteTool));
        registry.register(Arc::new(CalendarApiTool));
        registry.register(Arc::new(IotControlTool));
        registry.register(Arc::new(TtsTool));
        registry.register(Arc::new(OcrTool));
        registry.register(Arc::new(ImageUnderstandTool));
        registry.register(Arc::new(SocialMediaTool));
        registry.register(Arc::new(NotificationTool));
        registry.register(Arc::new(CloudApiTool));
        registry.register(Arc::new(GitApiTool));
        registry.register(Arc::new(FinanceApiTool));
        registry.register(Arc::new(VideoProcessTool));
        registry.register(Arc::new(HealthApiTool));
        registry.register(Arc::new(MapApiTool));
        registry.register(Arc::new(ContactsTool));
        registry.register(Arc::new(EncryptTool));
        registry.register(Arc::new(NetworkMonitorTool));
        registry.register(Arc::new(KnowledgeGraphTool));
        registry.register(Arc::new(StreamSubscribeTool));
        registry.register(Arc::new(AlertRuleTool));
        registry.register(Arc::new(BlockchainRpcTool));
        registry.register(Arc::new(ExchangeApiTool));
        registry.register(Arc::new(BlockchainTxTool));
        registry.register(Arc::new(ContractSecurityTool));
        registry.register(Arc::new(BridgeApiTool));
        registry.register(Arc::new(NftMarketTool));
        registry.register(Arc::new(MultisigTool));
        registry.register(Arc::new(CommunityHubTool));
        registry.register(Arc::new(MemoryMaintenanceTool));
        registry.register(Arc::new(ToggleManageTool));
        registry.register(Arc::new(TermuxApiTool));
        // No SpawnTool, MessageTool, CronTool — subagents can't spawn or send messages
        registry
    }

    /// Create a new provider instance for a subagent.
    /// Dispatches to the correct provider based on model prefix or configured provider name.
    pub fn create_subagent_provider(config: &Config) -> Option<Box<dyn Provider>> {
        let model = &config.agents.defaults.model;
        let max_tokens = config.agents.defaults.max_tokens;
        let temperature = config.agents.defaults.temperature;

        let (provider_name, provider_config) = config.get_api_key()?;

        // Determine effective provider from model prefix
        let effective_provider = if model.starts_with("anthropic/") || model.starts_with("claude-") {
            "anthropic"
        } else if model.starts_with("gemini/") || model.starts_with("gemini-") {
            "gemini"
        } else if model.starts_with("ollama/") {
            "ollama"
        } else if model.starts_with("kimi") || model.starts_with("moonshot") {
            "kimi"
        } else {
            provider_name
        };

        let resolved_config = if effective_provider != provider_name {
            config.get_provider(effective_provider).unwrap_or(provider_config)
        } else {
            provider_config
        };

        match effective_provider {
            "anthropic" => {
                Some(Box::new(AnthropicProvider::new(
                    &resolved_config.api_key,
                    resolved_config.api_base.as_deref(),
                    model,
                    max_tokens,
                    temperature,
                )))
            }
            "gemini" => {
                Some(Box::new(GeminiProvider::new(
                    &resolved_config.api_key,
                    resolved_config.api_base.as_deref(),
                    model,
                    max_tokens,
                    temperature,
                )))
            }
            "ollama" => {
                let api_base = resolved_config.api_base.as_deref()
                    .or(Some("http://localhost:11434"));
                Some(Box::new(OllamaProvider::new(
                    api_base,
                    model,
                    max_tokens,
                    temperature,
                )))
            }
            _ => {
                // OpenAI-compatible providers
                let api_base = resolved_config.api_base.as_deref().unwrap_or({
                    match effective_provider {
                        "openrouter" => "https://openrouter.ai/api/v1",
                        "openai" => "https://api.openai.com/v1",
                        "deepseek" => "https://api.deepseek.com/v1",
                        "groq" => "https://api.groq.com/openai/v1",
                        "zhipu" => "https://open.bigmodel.cn/api/paas/v4",
                        "kimi" | "moonshot" => "https://api.moonshot.cn/v1",
                        _ => "https://api.openai.com/v1",
                    }
                });
                Some(Box::new(OpenAIProvider::new(
                    &resolved_config.api_key,
                    Some(api_base),
                    model,
                    max_tokens,
                    temperature,
                )))
            }
        }
    }

    pub async fn process_message(&mut self, msg: InboundMessage) -> Result<String> {
        let session_key = msg.session_key();
        info!(session_key = %session_key, "Processing message");

        // ── skill_rhai fast path: execute SKILL.rhai directly without LLM ──
        if msg.metadata.get("skill_rhai").and_then(|v| v.as_bool()).unwrap_or(false) {
            let skill_name = msg.metadata.get("skill_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            info!(skill = %skill_name, "Cron skill_rhai dispatch");

            let result = self.execute_skill_rhai(&skill_name, &msg).await;
            let final_response = match &result {
                Ok(output) => format!("[{}] 定时任务执行完成:\n\n{}", skill_name, output),
                Err(e) => format!("[{}] 定时任务执行失败: {}", skill_name, e),
            };

            // Send response to outbound
            if let Some(tx) = &self.outbound_tx {
                let outbound = OutboundMessage::new(&msg.channel, &msg.chat_id, &final_response);
                let _ = tx.send(outbound).await;
            }

            // Deliver to external channel if configured
            if let Some(true) = msg.metadata.get("deliver").and_then(|v| v.as_bool()) {
                if let (Some(channel), Some(to)) = (
                    msg.metadata.get("deliver_channel").and_then(|v| v.as_str()),
                    msg.metadata.get("deliver_to").and_then(|v| v.as_str()),
                ) {
                    if let Some(tx) = &self.outbound_tx {
                        let outbound = OutboundMessage::new(channel, to, &final_response);
                        let _ = tx.send(outbound).await;
                    }
                }
            }

            return Ok(final_response);
        }

        // ── Cron reminder fast path: deliver directly without LLM ──
        if msg.metadata.get("reminder").and_then(|v| v.as_bool()).unwrap_or(false) {
            let reminder_msg = msg.metadata.get("reminder_message")
                .and_then(|v| v.as_str())
                .unwrap_or(&msg.content);
            let job_name = msg.metadata.get("job_name")
                .and_then(|v| v.as_str())
                .unwrap_or("提醒");
            let final_response = format!("⏰ [{}] {}", job_name, reminder_msg);
            info!(job_name = %job_name, "Cron reminder delivered directly (bypassing LLM)");

            // Send to outbound (CLI printer + gateway's outbound_to_ws_bridge)
            if let Some(tx) = &self.outbound_tx {
                let outbound = OutboundMessage::new(&msg.channel, &msg.chat_id, &final_response);
                let _ = tx.send(outbound).await;
            }

            // Deliver to external channel if configured
            if let Some(true) = msg.metadata.get("deliver").and_then(|v| v.as_bool()) {
                if let (Some(channel), Some(to)) = (
                    msg.metadata.get("deliver_channel").and_then(|v| v.as_str()),
                    msg.metadata.get("deliver_to").and_then(|v| v.as_str()),
                ) {
                    if let Some(tx) = &self.outbound_tx {
                        let outbound = OutboundMessage::new(channel, to, &final_response);
                        let _ = tx.send(outbound).await;
                    }
                }
            }

            return Ok(final_response);
        }

        // Load session history
        let mut history = self.session_store.load(&session_key)?;

        // ── Intent classification (Method A) ──
        let classifier = crate::intent::IntentClassifier::new();
        let intents = classifier.classify(&msg.content);
        let intent_names: Vec<String> = intents.iter().map(|i| format!("{:?}", i)).collect();
        info!(intents = ?intent_names, "Intent classified");

        // Load disabled toggles for filtering
        let disabled_tools = load_disabled_toggles(&self.paths, "tools");
        let disabled_skills = load_disabled_toggles(&self.paths, "skills");

        // Build messages for LLM with intent-filtered system prompt (Methods A+B+C+D+E+F)
        // Note: build_messages_for_intents appends the current user message from user_content,
        // so we pass history WITHOUT the current user message to avoid duplication.
        let pending_intent = msg.metadata.get("media_pending_intent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let messages = self.context_builder.build_messages_for_intents_with_channel(
            &history, &msg.content, &msg.media, &intents, &disabled_skills, &disabled_tools, &msg.channel, pending_intent,
        );

        // Now add user message to history for session persistence
        history.push(ChatMessage::user(&msg.content));

        // Get tool schemas filtered by intent (Method A) + disabled tools
        let mut tool_names = crate::intent::tools_for_intents(&intents);

        // Ghost routine: ensure required tools are always available.
        // Rationale: intent classification may treat the routine prompt as Chat, producing zero tools,
        // which would cause the LLM to think tools are unavailable.
        if msg.metadata.get("ghost").and_then(|v| v.as_bool()) == Some(true) {
            // Keep this list minimal and safe. All sensitive connection settings are resolved
            // internally by tools; do not expose any config details in the prompt.
            let required = [
                "community_hub",
                "memory_maintenance",
                "memory_query",
                "memory_upsert",
                "list_dir",
                "read_file",
                "file_ops",
                "notification",
            ];
            for name in required {
                if !tool_names.contains(&name) {
                    tool_names.push(name);
                }
            }
        }

        tool_names.sort();
        tool_names.dedup();
        let mut tools = if tool_names.is_empty() {
            // Chat intent: no tools
            vec![]
        } else {
            let mut schemas = self.tool_registry.get_filtered_schemas(&tool_names);
            if !disabled_tools.is_empty() {
                schemas.retain(|schema| {
                    let name = schema.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    !disabled_tools.contains(name)
                });
            }
            schemas
        };
        info!(tool_count = tools.len(), disabled_tools = disabled_tools.len(), disabled_skills = disabled_skills.len(), "Tools loaded for intent");

        // Main loop with max iterations
        let max_iterations = self.config.agents.defaults.max_tool_iterations;
        let mut current_messages = messages;
        let mut final_response = String::new();
        let mut message_tool_sent_media = false;

        for iteration in 0..max_iterations {
            debug!(iteration, "LLM call iteration");

            // Call LLM with retry on transient errors
            let max_retries = self.config.agents.defaults.llm_max_retries;
            let base_delay_ms = self.config.agents.defaults.llm_retry_delay_ms;
            let mut last_error = None;
            let mut response_opt = None;

            for attempt in 0..=max_retries {
                if attempt > 0 {
                    let delay_ms = base_delay_ms * (1u64 << (attempt - 1).min(4));
                    warn!(attempt, max_retries, delay_ms, iteration, "Retrying LLM call after transient error");
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                match self.provider.chat(&current_messages, &tools).await {
                    Ok(r) => {
                        if attempt > 0 {
                            info!(attempt, iteration, "LLM call succeeded after retry");
                        }
                        response_opt = Some(r);
                        break;
                    }
                    Err(e) => {
                        warn!(error = %e, attempt, max_retries, iteration, "LLM call failed");
                        last_error = Some(e);
                    }
                }
            }

            let response = match response_opt {
                Some(r) => r,
                None => {
                    let e = last_error.unwrap();
                    warn!(error = %e, iteration, retries = max_retries, "LLM call failed after all retries");
                    final_response = format!(
                        "抱歉，我在处理你的请求时遇到了问题（已重试 {} 次）。\n\n\
                        错误信息：{}\n\n\
                        这可能是临时的网络或服务问题，请稍后再试。如果问题持续，我会自动学习并改进。",
                        max_retries, e
                    );
                    // 报告错误给进化服务
                    if let Some(evo_service) = self.context_builder.evolution_service() {
                        let _ = evo_service.report_error(
                            "__llm_provider__",
                            &format!("{}", e),
                            None,
                            vec![],
                        ).await;
                    }
                    history.push(ChatMessage::assistant(&final_response));
                    break;
                }
            };

            info!(
                content_len = response.content.as_ref().map(|c| c.len()).unwrap_or(0),
                tool_calls_count = response.tool_calls.len(),
                finish_reason = %response.finish_reason,
                "LLM response received"
            );

            // Handle tool calls
            if !response.tool_calls.is_empty() {
                let short_circuit_after_tools = is_im_channel(&msg.channel)
                    && response.tool_calls.iter().all(|c| c.name == "message")
                    && response.tool_calls.iter().all(|c| {
                        let ch = c.arguments.get("channel").and_then(|v| v.as_str());
                        let to = c.arguments.get("chat_id").and_then(|v| v.as_str());
                        ch.map(|s| s == msg.channel).unwrap_or(true)
                            && to.map(|s| s == msg.chat_id).unwrap_or(true)
                    });

                // Add assistant message with tool calls
                let mut assistant_msg = ChatMessage::assistant(response.content.as_deref().unwrap_or(""));
                assistant_msg.reasoning_content = response.reasoning_content.clone();
                assistant_msg.tool_calls = Some(response.tool_calls.clone());
                current_messages.push(assistant_msg.clone());
                history.push(assistant_msg);

                // Execute each tool call, with dynamic tool supplement for intent misclassification
                let mut supplemented_tools = false;
                let mut tool_results: Vec<ChatMessage> = Vec::new();
                for tool_call in &response.tool_calls {
                    if tool_call.name == "message" {
                        let has_media = tool_call
                            .arguments
                            .get("media")
                            .and_then(|v| v.as_array())
                            .map(|a| !a.is_empty())
                            .unwrap_or(false);
                        if has_media {
                            message_tool_sent_media = true;
                        }
                    }
                    let result = self.execute_tool_call(tool_call, &msg).await;

                    // If tool was not found, try to supplement it dynamically
                    if result.contains("Unknown tool:") {
                        if let Some(schema) = self.tool_registry.get(&tool_call.name) {
                            let schema_val = serde_json::json!({
                                "type": "function",
                                "function": {
                                    "name": schema.schema().name,
                                    "description": schema.schema().description,
                                    "parameters": schema.schema().parameters
                                }
                            });
                            tools.push(schema_val);
                            supplemented_tools = true;
                            info!(tool = %tool_call.name, "Dynamically supplemented missing tool");
                        }
                    }

                    let mut tool_msg = ChatMessage::tool_result(&tool_call.id, &result);
                    tool_msg.name = Some(tool_call.name.clone());
                    tool_results.push(tool_msg);
                }

                // If we supplemented tools, roll back the assistant message and tool results
                // so the LLM retries with the full tool schema available.
                if supplemented_tools {
                    // Remove the assistant message we just pushed (last element)
                    current_messages.pop();
                    history.pop();
                    // Do NOT push tool results — the LLM will retry from scratch
                    continue;
                }

                // Normal path: commit tool results to messages and history
                for tool_msg in tool_results {
                    current_messages.push(tool_msg.clone());
                    history.push(tool_msg);
                }

                if short_circuit_after_tools {
                    final_response.clear();
                    break;
                }
            } else {
                // No tool calls, we have the final response
                final_response = response.content.unwrap_or_default();
                
                // Add to history
                history.push(ChatMessage::assistant(&final_response));
                break;
            }

            if iteration == max_iterations - 1 {
                warn!("Reached max iterations");
                final_response = response.content.unwrap_or_else(|| {
                    "I've reached the maximum number of tool iterations.".to_string()
                });
            }
        }

        if is_im_channel(&msg.channel) && user_wants_send_image(&msg.content) && !message_tool_sent_media {
            if let Some(image_path) = pick_image_path(&self.paths, &history).await {
                info!(
                    image_path = %image_path,
                    channel = %msg.channel,
                    "Auto-sending image fallback (LLM did not call message tool)"
                );
                if let Some(tx) = &self.outbound_tx {
                    let mut outbound = OutboundMessage::new(&msg.channel, &msg.chat_id, "");
                    outbound.media = vec![image_path.clone()];
                    let _ = tx.send(outbound).await;
                }

                final_response.clear();
                overwrite_last_assistant_message(&mut history, "");
            }
        }

        // Save session
        self.session_store.save(&session_key, &history)?;

        // Emit message_done event to WebSocket clients
        if let Some(ref event_tx) = self.event_tx {
            let event = serde_json::json!({
                "type": "message_done",
                "chat_id": msg.chat_id,
                "task_id": "",
                "content": final_response,
                "tool_calls": 0,
                "duration_ms": 0,
            });
            let _ = event_tx.send(event.to_string());
        }

        // Send response to outbound for all channels (including CLI and cron).
        // Skip ghost channel — the event_tx broadcast above already notifies WebSocket
        // clients, and ghost responses don't need CLI printing or external channel dispatch.
        // Sending via outbound_tx would cause a duplicate message_done in the gateway's
        // outbound_to_ws_bridge.
        if msg.channel != "ghost" {
            if let Some(tx) = &self.outbound_tx {
                let outbound = OutboundMessage::new(&msg.channel, &msg.chat_id, &final_response);
                let _ = tx.send(outbound).await;
            }
        }

        // For cron jobs with deliver=true, also forward to the specified external channel
        if msg.channel == "cron" {
            if let Some(deliver) = msg.metadata.get("deliver").and_then(|v| v.as_bool()) {
                if deliver {
                    if let (Some(channel), Some(to)) = (
                        msg.metadata.get("deliver_channel").and_then(|v| v.as_str()),
                        msg.metadata.get("deliver_to").and_then(|v| v.as_str()),
                    ) {
                        if let Some(tx) = &self.outbound_tx {
                            let outbound = OutboundMessage::new(channel, to, &final_response);
                            let _ = tx.send(outbound).await;
                        }
                    }
                }
            }
        }

        Ok(final_response)
    }

    /// Extract filesystem paths from tool call parameters.
    fn extract_paths(&self, tool_name: &str, args: &serde_json::Value) -> Vec<String> {
        let mut paths = Vec::new();
        match tool_name {
            "read_file" | "write_file" | "edit_file" | "list_dir" => {
                if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                    paths.push(p.to_string());
                }
            }
            "file_ops" | "data_process" | "audio_transcribe" | "chart_generate" | "office_write" | "video_process" | "health_api" | "encrypt" => {
                if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                    paths.push(p.to_string());
                }
                if let Some(d) = args.get("destination").and_then(|v| v.as_str()) {
                    paths.push(d.to_string());
                }
                if let Some(o) = args.get("output_path").and_then(|v| v.as_str()) {
                    paths.push(o.to_string());
                }
                if let Some(arr) = args.get("paths").and_then(|v| v.as_array()) {
                    for p in arr {
                        if let Some(s) = p.as_str() {
                            paths.push(s.to_string());
                        }
                    }
                }
            }
            "message" => {
                if let Some(arr) = args.get("media").and_then(|v| v.as_array()) {
                    for p in arr {
                        if let Some(s) = p.as_str() {
                            paths.push(s.to_string());
                        }
                    }
                }
            }
            "browse" => {
                if let Some(o) = args.get("output_path").and_then(|v| v.as_str()) {
                    paths.push(o.to_string());
                }
            }
            "exec" => {
                if let Some(wd) = args.get("working_dir").and_then(|v| v.as_str()) {
                    paths.push(wd.to_string());
                }
            }
            _ => {}
        }
        paths
    }

    /// Resolve a path string the same way tools do (expand ~ and relative paths).
    fn resolve_path(&self, path_str: &str) -> PathBuf {
        if path_str.starts_with("~/") {
            dirs::home_dir()
                .map(|h| h.join(&path_str[2..]))
                .unwrap_or_else(|| PathBuf::from(path_str))
        } else if path_str.starts_with('/') {
            PathBuf::from(path_str)
        } else {
            self.paths.workspace().join(path_str)
        }
    }

    /// Check if a resolved path is inside the safe workspace directory.
    fn is_path_safe(&self, resolved: &std::path::Path) -> bool {
        let workspace = self.paths.workspace();
        // Canonicalize both if possible, otherwise use starts_with on the raw paths
        let ws = workspace.canonicalize().unwrap_or(workspace);
        let rp = resolved.canonicalize().unwrap_or_else(|_| resolved.to_path_buf());
        rp.starts_with(&ws)
    }

    /// Check whether a resolved path falls within an already-authorized directory.
    fn is_path_authorized(&self, resolved: &std::path::Path) -> bool {
        let rp = resolved.canonicalize().unwrap_or_else(|_| resolved.to_path_buf());
        self.authorized_dirs.iter().any(|dir| rp.starts_with(dir))
    }

    /// Record a directory as authorized so future accesses within it are auto-approved.
    fn authorize_directory(&mut self, resolved: &std::path::Path) {
        // If the path is a directory, authorize it directly.
        // If it's a file, authorize its parent directory.
        let dir = if resolved.is_dir() {
            resolved.canonicalize().unwrap_or_else(|_| resolved.to_path_buf())
        } else {
            resolved
                .parent()
                .map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()))
                .unwrap_or_else(|| resolved.to_path_buf())
        };
        if self.authorized_dirs.insert(dir.clone()) {
            info!(dir = %dir.display(), "Directory authorized for future access");
        }
    }

    /// For tools that access the filesystem, check if any paths are outside the
    /// workspace. If so, send a confirmation request to the user and wait for
    /// their response. Returns Ok(true) if access is allowed, Ok(false) if denied.
    async fn check_path_permission(&mut self, tool_name: &str, args: &serde_json::Value) -> bool {
        let raw_paths = self.extract_paths(tool_name, args);
        if raw_paths.is_empty() {
            return true; // No filesystem paths to check
        }

        let unsafe_paths: Vec<String> = raw_paths
            .iter()
            .filter(|p| {
                let resolved = self.resolve_path(p);
                // Safe if inside workspace OR inside an already-authorized directory
                !self.is_path_safe(&resolved) && !self.is_path_authorized(&resolved)
            })
            .cloned()
            .collect();

        if unsafe_paths.is_empty() {
            return true; // All paths are within workspace or authorized dirs
        }

        // Need user confirmation
        if let Some(confirm_tx) = &self.confirm_tx {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let request = ConfirmRequest {
                tool_name: tool_name.to_string(),
                paths: unsafe_paths.clone(),
                response_tx,
            };

            if confirm_tx.send(request).await.is_err() {
                warn!("Failed to send confirmation request, denying access");
                return false;
            }

            match response_rx.await {
                Ok(allowed) => {
                    if allowed {
                        // Cache the authorized directories so files within don't need re-confirmation
                        for p in &unsafe_paths {
                            let resolved = self.resolve_path(p);
                            self.authorize_directory(&resolved);
                        }
                    }
                    allowed
                }
                Err(_) => {
                    warn!("Confirmation channel closed, denying access");
                    false
                }
            }
        } else {
            // No confirmation channel available (e.g. single message mode), deny
            warn!(tool = tool_name, "No confirmation channel, denying access to paths outside workspace");
            false
        }
    }

    async fn execute_tool_call(&mut self, tool_call: &ToolCallRequest, msg: &InboundMessage) -> String {
        // Check path safety before executing filesystem/exec tools
        if !self.check_path_permission(&tool_call.name, &tool_call.arguments).await {
            return serde_json::json!({
                "error": "Permission denied: user rejected access to paths outside the safe workspace directory.",
                "tool": tool_call.name,
                "hint": "The requested path is outside the workspace. The user has denied this operation. Please inform the user and suggest an alternative within the workspace, or ask the user to confirm."
            }).to_string();
        }

        // Build TaskManager handle for tools
        let tm_handle: TaskManagerHandle = Arc::new(self.task_manager.clone());

        // Build spawn handle for tools
        let spawn_handle = Arc::new(RuntimeSpawnHandle {
            config: self.config.clone(),
            paths: self.paths.clone(),
            task_manager: self.task_manager.clone(),
            outbound_tx: self.outbound_tx.clone(),
        });

        let ctx = blockcell_tools::ToolContext {
            workspace: self.paths.workspace(),
            builtin_skills_dir: Some(self.paths.builtin_skills_dir()),
            session_key: msg.session_key(),
            channel: msg.channel.clone(),
            chat_id: msg.chat_id.clone(),
            config: self.config.clone(),
            permissions: blockcell_core::types::PermissionSet::new(), // TODO: Load from skill meta
            task_manager: Some(tm_handle),
            memory_store: self.memory_store.clone(),
            outbound_tx: self.outbound_tx.clone(),
            spawn_handle: Some(spawn_handle),
            capability_registry: self.capability_registry.clone(),
            core_evolution: self.core_evolution.clone(),
        };

        // Emit tool_call_start event to WebSocket clients
        if let Some(ref event_tx) = self.event_tx {
            let event = serde_json::json!({
                "type": "tool_call_start",
                "chat_id": msg.chat_id,
                "task_id": "",
                "tool": tool_call.name,
                "call_id": tool_call.id,
                "params": tool_call.arguments,
            });
            let _ = event_tx.send(event.to_string());
        }

        let start = std::time::Instant::now();
        let result = self.tool_registry.execute(&tool_call.name, ctx, tool_call.arguments.clone()).await;
        let duration_ms = start.elapsed().as_millis() as u64;

        let is_error = result.is_err();
        let (result_str, result_json) = match &result {
            Ok(val) => (val.to_string(), val.clone()),
            Err(e) => {
                let err_str = format!("Error: {}", e);
                (err_str.clone(), serde_json::json!({"error": err_str}))
            }
        };

        // Detect writes to the skills directory and trigger hot-reload + Dashboard refresh
        if !is_error && (tool_call.name == "write_file" || tool_call.name == "edit_file") {
            if let Some(path_str) = tool_call.arguments.get("path").and_then(|v| v.as_str()) {
                let resolved = self.resolve_path(path_str);
                let skills_dir = self.paths.skills_dir();
                let in_skills = resolved.starts_with(&skills_dir)
                    || resolved.canonicalize().ok().is_some_and(|c| {
                        skills_dir.canonicalize().ok().is_some_and(|sd| c.starts_with(&sd))
                    });
                if in_skills {
                    info!(path = %path_str, "🔄 Detected write to skills directory, reloading...");
                    let new_skills = self.context_builder.reload_skills();
                    if !new_skills.is_empty() {
                        info!(skills = ?new_skills, "🔄 Hot-reloaded new skills");
                    }
                    // Always broadcast so Dashboard refreshes (even for updates to existing skills)
                    if let Some(ref event_tx) = self.event_tx {
                        let event = serde_json::json!({
                            "type": "skills_updated",
                            "new_skills": new_skills,
                        });
                        let _ = event_tx.send(event.to_string());
                    }
                }
            }
        }

        let mut learning_hint: Option<String> = None;
        if is_error {
            let is_unknown_tool = result_str.contains("Unknown tool:");

            if is_unknown_tool {
                learning_hint = Some(format!(
                    "[系统] 工具 `{}` 未注册/不可用（Unknown tool）。这不是可通过技能自进化修复的问题。\
                    请改用已存在的工具完成任务，或提示用户安装/启用对应工具。",
                    tool_call.name
                ));
            } else if let Some(evo_service) = self.context_builder.evolution_service() {
                // Try to load the current SKILL.rhai source for context
                let source_snippet = self.context_builder.skill_manager()
                    .and_then(|sm| sm.get(&tool_call.name))
                    .and_then(|skill| skill.load_rhai());
                match evo_service
                    .report_error(&tool_call.name, &result_str, source_snippet, vec![])
                    .await
                {
                    Ok(report) => {
                        if report.evolution_triggered.is_some() {
                            learning_hint = Some(format!(
                                "[系统] 技能 `{}` 执行失败，已自动触发进化学习。\
                                请向用户坦诚说明：你暂时还不具备这个技能，但已经开始学习，\
                                学会后会自动生效。同时尝试用其他方式帮助用户解决当前问题。",
                                tool_call.name
                            ));
                        } else if report.evolution_in_progress {
                            learning_hint = Some(format!(
                                "[系统] 技能 `{}` 执行失败，该技能正在学习改进中。\
                                请告诉用户：这个技能正在学习中，请稍后再试。",
                                tool_call.name
                            ));
                        }
                    }
                    Err(e) => {
                        debug!(error = %e, "Evolution report_error failed");
                    }
                }
            }
        }
        // 报告调用结果给灰度统计
        if let Some(evo_service) = self.context_builder.evolution_service() {
            evo_service.report_skill_call(&tool_call.name, is_error).await;
        }

        // Emit tool_call_result event to WebSocket clients
        if let Some(ref event_tx) = self.event_tx {
            let event = serde_json::json!({
                "type": "tool_call_result",
                "chat_id": msg.chat_id,
                "task_id": "",
                "tool": tool_call.name,
                "call_id": tool_call.id,
                "result": result_json,
                "duration_ms": duration_ms,
            });
            let _ = event_tx.send(event.to_string());
        }

        // Log to audit
        let _ = self.audit_logger.log_tool_call(
            &tool_call.name,
            tool_call.arguments.clone(),
            result_json,
            &msg.session_key(),
            None, // trace_id can be added later
            Some(duration_ms),
        );

        // 在工具结果中追加学习提示，让 LLM 自然地回复用户
        match learning_hint {
            Some(hint) => format!("{}\n\n{}", result_str, hint),
            None => result_str,
        }
    }

    /// Execute a SKILL.rhai script directly (for cron skill_rhai jobs).
    /// Loads the script from skills/{skill_name}/SKILL.rhai, executes it via
    /// SkillDispatcher with a synchronous tool executor, and returns the output.
    async fn execute_skill_rhai(&mut self, skill_name: &str, msg: &InboundMessage) -> Result<String> {
        // Locate SKILL.rhai file
        let skill_dir = self.paths.skills_dir().join(skill_name);
        let rhai_path = skill_dir.join("SKILL.rhai");

        if !rhai_path.exists() {
            // Try builtin skills dir
            let builtin_path = self.paths.builtin_skills_dir().join(skill_name).join("SKILL.rhai");
            if !builtin_path.exists() {
                return Err(blockcell_core::Error::Skill(format!(
                    "SKILL.rhai not found for skill '{}' (checked {} and {})",
                    skill_name,
                    rhai_path.display(),
                    builtin_path.display()
                )));
            }
            // Use builtin path
            return self.run_rhai_script(&builtin_path, skill_name, msg).await;
        }

        self.run_rhai_script(&rhai_path, skill_name, msg).await
    }

    /// Helper: run a single .rhai script file with tool execution support.
    async fn run_rhai_script(
        &self,
        rhai_path: &std::path::Path,
        skill_name: &str,
        msg: &InboundMessage,
    ) -> Result<String> {
        use blockcell_skills::dispatcher::SkillDispatcher;
        use std::collections::HashMap;

        let script = std::fs::read_to_string(rhai_path).map_err(|e| {
            blockcell_core::Error::Skill(format!("Failed to read {}: {}", rhai_path.display(), e))
        })?;

        // Build a synchronous tool executor that uses the tool registry
        let registry = self.tool_registry.clone();
        let config = self.config.clone();
        let paths = self.paths.clone();
        let session_key = msg.session_key();
        let channel = msg.channel.clone();
        let chat_id = msg.chat_id.clone();
        let task_manager = self.task_manager.clone();
        let memory_store = self.memory_store.clone();
        let outbound_tx = self.outbound_tx.clone();
        let capability_registry = self.capability_registry.clone();
        let core_evolution = self.core_evolution.clone();

        let tool_executor = move |tool_name: &str, params: serde_json::Value| -> Result<serde_json::Value> {
            let ctx = blockcell_tools::ToolContext {
                workspace: paths.workspace(),
                builtin_skills_dir: Some(paths.builtin_skills_dir()),
                session_key: session_key.clone(),
                channel: channel.clone(),
                chat_id: chat_id.clone(),
                config: config.clone(),
                permissions: blockcell_core::types::PermissionSet::new(),
                task_manager: Some(Arc::new(task_manager.clone())),
                memory_store: memory_store.clone(),
                outbound_tx: outbound_tx.clone(),
                spawn_handle: None, // No spawning from cron skill scripts
                capability_registry: capability_registry.clone(),
                core_evolution: core_evolution.clone(),
            };

            // Execute tool synchronously via a new tokio runtime handle
            let rt = tokio::runtime::Handle::current();
            let tool_name_owned = tool_name.to_string();
            std::thread::scope(|s| {
                s.spawn(|| {
                    rt.block_on(async {
                        registry.execute(&tool_name_owned, ctx, params).await
                    })
                }).join().unwrap_or_else(|_| Err(blockcell_core::Error::Tool("Tool execution panicked".into())))
            })
        };

        // Context variables for the script
        let mut context_vars = HashMap::new();
        context_vars.insert("skill_name".to_string(), serde_json::json!(skill_name));
        context_vars.insert("trigger".to_string(), serde_json::json!("cron"));

        // Execute the Rhai script in a blocking task
        let dispatcher = SkillDispatcher::new();
        let user_input = msg.content.clone();

        let result = tokio::task::spawn_blocking(move || {
            dispatcher.execute_sync(&script, &user_input, context_vars, tool_executor)
        })
        .await
        .map_err(|e| blockcell_core::Error::Skill(format!("Skill execution join error: {}", e)))??;

        if result.success {
            // Format output as string
            let output_str = match &result.output {
                serde_json::Value::String(s) => s.clone(),
                other => serde_json::to_string_pretty(other).unwrap_or_default(),
            };
            info!(
                skill = %skill_name,
                tool_calls = result.tool_calls.len(),
                "SKILL.rhai cron execution succeeded"
            );
            Ok(output_str)
        } else {
            let err = result.error.unwrap_or_else(|| "Unknown error".to_string());
            warn!(skill = %skill_name, error = %err, "SKILL.rhai cron execution failed");
            Err(blockcell_core::Error::Skill(err))
        }
    }

    pub async fn run_loop(
        &mut self,
        mut inbound_rx: mpsc::Receiver<InboundMessage>,
        mut shutdown_rx: Option<broadcast::Receiver<()>>,
    ) {
        info!("AgentRuntime started");

        // 启动灰度发布调度器（每 60 秒 tick 一次）
        let has_evolution = self.context_builder.evolution_service().is_some();
        if has_evolution {
            info!("Evolution rollout scheduler enabled");
        }

        let tick_secs = self.config.tools.tick_interval_secs.clamp(10, 300) as u64;
        info!(tick_secs = tick_secs, "Tick interval configured");
        let mut tick_interval = tokio::time::interval(std::time::Duration::from_secs(tick_secs));
        tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = async {
                    if let Some(ref mut rx) = shutdown_rx {
                        let _ = rx.recv().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    break;
                }
                msg = inbound_rx.recv() => {
                    match msg {
                        Some(msg) => {
                            // Spawn each message as a background task so the loop
                            // stays responsive for new user input.
                            let task_id = format!("msg_{}", uuid::Uuid::new_v4());
                            let label = if msg.content.chars().count() > 40 {
                                format!("{}...", truncate_str(&msg.content, 40))
                            } else {
                                msg.content.clone()
                            };

                            let task_manager = self.task_manager.clone();
                            let config = self.config.clone();
                            let paths = self.paths.clone();
                            let outbound_tx = self.outbound_tx.clone();
                            let confirm_tx = self.confirm_tx.clone();
                            let memory_store = self.memory_store.clone();
                            let capability_registry = self.capability_registry.clone();
                            let core_evolution = self.core_evolution.clone();
                            let event_tx = self.event_tx.clone();
                            let task_id_clone = task_id.clone();

                            // Register task
                            task_manager.create_task(
                                &task_id,
                                &label,
                                &msg.content,
                                &msg.channel,
                                &msg.chat_id,
                            ).await;

                            tokio::spawn(run_message_task(
                                config,
                                paths,
                                task_manager,
                                outbound_tx,
                                confirm_tx,
                                memory_store,
                                capability_registry,
                                core_evolution,
                                event_tx,
                                msg,
                                task_id_clone,
                            ));
                        }
                        None => break, // channel closed
                    }
                }
                _ = tick_interval.tick() => {
                    // Auto-cleanup completed/failed tasks older than 5 minutes
                    self.task_manager.cleanup_old_tasks(
                        std::time::Duration::from_secs(300)
                    ).await;

                    // Memory maintenance (TTL cleanup, recycle bin purge)
                    if let Some(ref store) = self.memory_store {
                        if let Err(e) = store.maintenance(30) {
                            warn!(error = %e, "Memory maintenance error");
                        }
                    }

                    // Evolution rollout tick
                    if has_evolution {
                        if let Some(evo_service) = self.context_builder.evolution_service() {
                            if let Err(e) = evo_service.tick().await {
                                warn!(error = %e, "Evolution rollout tick error");
                            }
                        }
                    }

                    // Process pending core evolutions
                    if let Some(ref core_evo_handle) = self.core_evolution {
                        let core_evo = core_evo_handle.lock().await;
                        match core_evo.run_pending_evolutions().await {
                            Ok(n) if n > 0 => {
                                info!(count = n, "🧬 [核心进化] 处理了 {} 个待处理进化", n);
                            }
                            Err(e) => {
                                warn!(error = %e, "🧬 [核心进化] 处理待处理进化出错");
                            }
                            _ => {}
                        }
                    }

                    // Periodic skill hot-reload (picks up skills created by chat)
                    let new_skills = self.context_builder.reload_skills();
                    if !new_skills.is_empty() {
                        info!(skills = ?new_skills, "🔄 Tick: hot-reloaded new skills");
                        if let Some(ref event_tx) = self.event_tx {
                            let event = serde_json::json!({
                                "type": "skills_updated",
                                "new_skills": new_skills,
                            });
                            let _ = event_tx.send(event.to_string());
                        }
                    }

                    // Refresh capability brief for prompt injection + sync capability IDs to SkillManager
                    if let Some(ref registry_handle) = self.capability_registry {
                        let registry = registry_handle.lock().await;
                        let brief = registry.generate_brief().await;
                        self.context_builder.set_capability_brief(brief);
                        // Sync available capability IDs so SkillManager can validate skill dependencies
                        let cap_ids = registry.list_available_ids().await;
                        self.context_builder.sync_capabilities(cap_ids);
                    }

                    // Auto-trigger Capability evolution for missing skill dependencies
                    // With 24h cooldown per capability to prevent repeated requests
                    if let Some(ref core_evo_handle) = self.core_evolution {
                        let missing = self.context_builder.get_missing_capabilities();
                        let now = chrono::Utc::now().timestamp();
                        const COOLDOWN_SECS: i64 = 86400; // 24 hours

                        for (skill_name, cap_id) in missing {
                            // Cooldown check: skip if requested within 24h
                            if let Some(&last_request) = self.cap_request_cooldown.get(&cap_id) {
                                if now - last_request < COOLDOWN_SECS {
                                    continue;
                                }
                            }

                            let description = format!(
                                "Auto-requested: required by skill '{}'",
                                skill_name
                            );
                            let core_evo = core_evo_handle.lock().await;
                            match core_evo.request_capability(&cap_id, &description, "script").await {
                                Ok(_) => {
                                    self.cap_request_cooldown.insert(cap_id.clone(), now);
                                    info!(
                                        capability_id = %cap_id,
                                        skill = %skill_name,
                                        "🧬 Auto-requested missing capability '{}' for skill '{}'",
                                        cap_id, skill_name
                                    );
                                }
                                Err(e) => {
                                    // Also record cooldown on error (blocked/failed) to avoid retrying immediately
                                    self.cap_request_cooldown.insert(cap_id.clone(), now);
                                    debug!(
                                        capability_id = %cap_id,
                                        error = %e,
                                        "Failed to auto-request capability (cooldown set)"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        info!("AgentRuntime stopped");
    }
}

/// Free async function that runs a user message in the background.
/// Each message gets its own AgentRuntime so the main loop stays responsive.
async fn run_message_task(
    config: Config,
    paths: Paths,
    task_manager: TaskManager,
    outbound_tx: Option<mpsc::Sender<OutboundMessage>>,
    confirm_tx: Option<mpsc::Sender<ConfirmRequest>>,
    memory_store: Option<MemoryStoreHandle>,
    capability_registry: Option<CapabilityRegistryHandle>,
    core_evolution: Option<CoreEvolutionHandle>,
    event_tx: Option<broadcast::Sender<String>>,
    msg: InboundMessage,
    task_id: String,
) {
    task_manager.set_running(&task_id).await;

    // Create a fresh provider for this task
    let provider = match AgentRuntime::create_subagent_provider(&config) {
        Some(p) => p,
        None => {
            let err = "No provider configured";
            task_manager.set_failed(&task_id, err).await;
            if let Some(tx) = &outbound_tx {
                let _ = tx.send(OutboundMessage::new(&msg.channel, &msg.chat_id, &format!("❌ {}", err))).await;
            }
            return;
        }
    };

    let tool_registry = ToolRegistry::with_defaults();
    let mut runtime = match AgentRuntime::new(config, paths, provider, tool_registry) {
        Ok(r) => r,
        Err(e) => {
            task_manager.set_failed(&task_id, &format!("{}", e)).await;
            if let Some(tx) = &outbound_tx {
                let _ = tx.send(OutboundMessage::new(&msg.channel, &msg.chat_id, &format!("❌ {}", e))).await;
            }
            return;
        }
    };

    // Wire up channels
    if let Some(tx) = outbound_tx.clone() {
        runtime.set_outbound(tx);
    }
    if let Some(tx) = confirm_tx {
        runtime.set_confirm(tx);
    }
    runtime.set_task_manager(task_manager.clone());
    if let Some(store) = memory_store {
        runtime.set_memory_store(store);
    }
    if let Some(registry) = capability_registry {
        runtime.set_capability_registry(registry);
    }
    if let Some(core_evo) = core_evolution {
        runtime.set_core_evolution(core_evo);
    }
    if let Some(tx) = event_tx {
        runtime.set_event_tx(tx);
    }

    match runtime.process_message(msg).await {
        Ok(response) => {
            debug!(task_id = %task_id, response_len = response.len(), "Message task completed");
            // Remove completed message tasks immediately — the response was already
            // sent via outbound_tx. Only subagent tasks persist in the task list.
            task_manager.remove_task(&task_id).await;
        }
        Err(e) => {
            let err_msg = format!("{}", e);
            error!(task_id = %task_id, error = %e, "Message task failed");
            // Keep failed tasks briefly for visibility, then let tick cleanup handle them
            task_manager.set_failed(&task_id, &err_msg).await;
        }
    }
}

/// Free async function that runs a subagent task in the background.
/// This is separate from `AgentRuntime` methods to break the recursive async type
/// chain that would otherwise prevent the future from being `Send`.
async fn run_subagent_task(
    config: Config,
    paths: Paths,
    provider: Box<dyn Provider>,
    task_manager: TaskManager,
    outbound_tx: Option<mpsc::Sender<OutboundMessage>>,
    task_str: String,
    task_id: String,
    label: String,
    origin_channel: String,
    origin_chat_id: String,
) {
    // Create the task entry first, then immediately mark it running.
    // This ensures set_running() never operates on a non-existent task ID.
    task_manager.create_task(&task_id, &label, &task_str, &origin_channel, &origin_chat_id).await;
    task_manager.set_running(&task_id).await;
    task_manager.set_progress(&task_id, "Processing...").await;

    // Create isolated runtime with restricted tools
    let tool_registry = AgentRuntime::subagent_tool_registry();
    let mut sub_runtime = match AgentRuntime::new(config, paths, provider, tool_registry) {
        Ok(r) => r,
        Err(e) => {
            task_manager.set_failed(&task_id, &format!("{}", e)).await;
            return;
        }
    };
    sub_runtime.set_task_manager(task_manager.clone());

    // Create a unique session key for this subagent
    let session_key = format!("subagent:{}", task_id);
    let inbound = InboundMessage {
        channel: "subagent".to_string(),
        sender_id: "system".to_string(),
        chat_id: session_key,
        content: task_str,
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    match sub_runtime.process_message(inbound).await {
        Ok(result) => {
            task_manager.set_completed(&task_id, &result).await;
            info!(task_id = %task_id, label = %label, "Subagent completed");

            // Notify the origin channel that the task is done
            if let Some(tx) = &outbound_tx {
                let short_id = truncate_str(&task_id, 8);
                let result_preview = if result.chars().count() > 500 {
                    format!("{}...", truncate_str(&result, 500))
                } else {
                    result
                };
                let notification = OutboundMessage::new(
                    &origin_channel,
                    &origin_chat_id,
                    &format!(
                        "\n📋 后台任务完成: **{}** (ID: {})\n\n{}",
                        label,
                        short_id,
                        result_preview,
                    ),
                );
                let _ = tx.send(notification).await;
            }
        }
        Err(e) => {
            let err_msg = format!("{}", e);
            task_manager.set_failed(&task_id, &err_msg).await;
            error!(task_id = %task_id, error = %e, "Subagent failed");

            if let Some(tx) = &outbound_tx {
                let short_id = truncate_str(&task_id, 8);
                let notification = OutboundMessage::new(
                    &origin_channel,
                    &origin_chat_id,
                    &format!(
                        "\n❌ 后台任务失败: **{}** (ID: {})\n错误: {}",
                        label,
                        short_id,
                        err_msg
                    ),
                );
                let _ = tx.send(notification).await;
            }
        }
    }
}
