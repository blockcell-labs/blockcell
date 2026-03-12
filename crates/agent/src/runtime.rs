use blockcell_core::path_policy::{PathOp, PathPolicy, PolicyAction};
use blockcell_core::system_event::{EventPriority, EventScope, SessionSummary, SystemEvent};
use blockcell_core::types::{ChatMessage, LLMResponse, StreamChunk, ToolCallAccumulator, ToolCallRequest};
use blockcell_core::{Config, InboundMessage, OutboundMessage, Paths, Result};
use blockcell_providers::{CallResult, Provider, ProviderPool};
use blockcell_storage::{AuditLogger, SessionStore};
use blockcell_tools::{
    CapabilityRegistryHandle, CoreEvolutionHandle, EventEmitterHandle, MemoryStoreHandle,
    SpawnHandle, SystemEventEmitter, TaskManagerHandle, ToolRegistry,
};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

use crate::context::{ActiveSkillContext, ContextBuilder, InteractionMode};
use crate::intent::{IntentCategory, IntentToolResolver};
use crate::summary_queue::MainSessionSummaryQueue;
use crate::system_event_orchestrator::{
    HeartbeatDecision, NotificationRequest, SystemEventOrchestrator,
};
use crate::system_event_store::{InMemorySystemEventStore, SystemEventStoreOps};
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
            ChatMessage::system(
                "You are a skill evolution assistant. Follow instructions precisely.",
            ),
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
    provider_pool: Arc<ProviderPool>,
    agent_id: Option<String>,
    event_emitter: EventEmitterHandle,
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

        // Reuse the shared pool for the subagent (pool is Arc, cheap to clone)
        let provider_pool = Arc::clone(&self.provider_pool);

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
        let agent_id = self.agent_id.clone();

        // Spawn the background task. Task registration (create_task) happens inside
        // run_subagent_task before set_running(), eliminating the race condition.
        tokio::spawn(run_subagent_task(
            config,
            paths,
            provider_pool,
            task_manager,
            outbound_tx,
            task_str,
            task_id_clone,
            label_clone,
            origin_channel,
            origin_chat_id,
            agent_id,
            self.event_emitter.clone(),
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
    /// The channel the originating message came from (e.g. "ws", "lark", "telegram").
    pub channel: String,
    /// The chat_id of the originating message, used to route the confirmation
    /// prompt back to the correct conversation.
    pub chat_id: String,
}

/// Truncate a string at a safe char boundary.
fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => s[..idx].to_string(),
        None => s.to_string(),
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

/// Compact JSON value for presentation.
fn compact_json_value(value: &serde_json::Value, depth: usize) -> serde_json::Value {
    const MAX_DEPTH: usize = 4;
    const MAX_ARRAY_ITEMS: usize = 8;
    const MAX_STRING_CHARS: usize = 400;

    if depth >= MAX_DEPTH {
        return match value {
            serde_json::Value::String(s) => serde_json::Value::String(truncate_str(s, 160)),
            serde_json::Value::Array(arr) => serde_json::json!({
                "kind": "array",
                "len": arr.len()
            }),
            serde_json::Value::Object(map) => serde_json::json!({
                "kind": "object",
                "keys": map.keys().take(12).cloned().collect::<Vec<_>>()
            }),
            other => other.clone(),
        };
    }

    match value {
        serde_json::Value::Null => serde_json::Value::Null,
        serde_json::Value::Bool(v) => serde_json::Value::Bool(*v),
        serde_json::Value::Number(v) => serde_json::Value::Number(v.clone()),
        serde_json::Value::String(s) => {
            serde_json::Value::String(truncate_str(s, MAX_STRING_CHARS))
        }
        serde_json::Value::Array(arr) => {
            let items = arr
                .iter()
                .take(MAX_ARRAY_ITEMS)
                .map(|item| compact_json_value(item, depth + 1))
                .collect::<Vec<_>>();
            if arr.len() > MAX_ARRAY_ITEMS {
                serde_json::json!({
                    "items": items,
                    "truncated": true,
                    "total": arr.len()
                })
            } else {
                serde_json::Value::Array(items)
            }
        }
        serde_json::Value::Object(map) => {
            let heavy_keys = [
                "content",
                "body",
                "html",
                "markdown",
                "raw",
                "text",
                "full_text",
            ];
            let mut result = serde_json::Map::new();

            for (key, value) in map.iter() {
                if heavy_keys.contains(&key.as_str()) {
                    match value {
                        serde_json::Value::String(s) => {
                            result.insert(
                                key.clone(),
                                serde_json::json!({
                                    "preview": truncate_str(s, 240),
                                    "truncated": s.chars().count() > 240,
                                    "length": s.chars().count()
                                }),
                            );
                        }
                        other => {
                            result.insert(key.clone(), compact_json_value(other, depth + 1));
                        }
                    }
                } else {
                    result.insert(key.clone(), compact_json_value(value, depth + 1));
                }
            }

            serde_json::Value::Object(result)
        }
    }
}

/// Prepare skill result for presentation.
struct SkillResultPresentation {
    direct_text: Option<String>,
    llm_payload: Option<String>,
    fallback_text: String,
}

fn prepare_skill_result_for_presentation(
    skill_name: &str,
    output: &str,
) -> SkillResultPresentation {
    let raw_fallback = format!(
        "[{}] 定时任务执行完成:\n\n{}",
        skill_name,
        truncate_str(output, 4000)
    );

    let parsed: serde_json::Value = match serde_json::from_str(output) {
        Ok(value) => value,
        Err(_) => {
            return SkillResultPresentation {
                direct_text: None,
                llm_payload: Some(truncate_str(output, 4000)),
                fallback_text: raw_fallback,
            };
        }
    };

    let Some(obj) = parsed.as_object() else {
        return SkillResultPresentation {
            direct_text: None,
            llm_payload: Some(truncate_str(output, 4000)),
            fallback_text: raw_fallback,
        };
    };

    if let Some(display_text) = obj.get("display_text").and_then(|v| v.as_str()) {
        let text = display_text.trim();
        if !text.is_empty() {
            return SkillResultPresentation {
                direct_text: Some(text.to_string()),
                llm_payload: None,
                fallback_text: text.to_string(),
            };
        }
    }

    let instruction = obj
        .get("instruction")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("请把结果整理成清晰、简洁、用户可读的回复，不要编造未提供的信息。");

    let llm_source = if let Some(summary) = obj.get("summary_data") {
        serde_json::json!({
            "instruction": instruction,
            "summary_data": compact_json_value(summary, 0)
        })
    } else {
        let mut compact = serde_json::Map::new();
        for (key, value) in obj {
            if key == "raw_data" {
                continue;
            }
            compact.insert(key.clone(), compact_json_value(value, 0));
        }
        serde_json::Value::Object(compact)
    };

    let llm_payload =
        serde_json::to_string_pretty(&llm_source).unwrap_or_else(|_| truncate_str(output, 4000));

    let fallback_text = if let Some(summary) = obj.get("summary_data") {
        let compact = serde_json::to_string_pretty(&compact_json_value(summary, 0))
            .unwrap_or_else(|_| "{}".to_string());
        format!(
            "[{}] 定时任务执行完成（摘要整理失败，以下为结构化摘要）:\n\n{}",
            skill_name,
            truncate_str(&compact, 4000)
        )
    } else {
        raw_fallback
    };

    SkillResultPresentation {
        direct_text: None,
        llm_payload: Some(truncate_str(&llm_payload, 16000)),
        fallback_text,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MainSessionTarget {
    channel: String,
    account_id: Option<String>,
    chat_id: String,
    session_key: String,
}

#[derive(Clone)]
struct RuntimeSystemEventEmitter {
    store: InMemorySystemEventStore,
}

impl SystemEventEmitter for RuntimeSystemEventEmitter {
    fn emit(&self, event: SystemEvent) {
        self.store.emit(event);
    }
}

fn is_main_session_candidate(msg: &InboundMessage) -> bool {
    if matches!(
        msg.channel.as_str(),
        "system" | "cron" | "subagent" | "ghost"
    ) {
        return false;
    }
    if matches!(msg.sender_id.as_str(), "system" | "cron") {
        return false;
    }
    if msg
        .metadata
        .get("cancel")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return false;
    }
    true
}

fn render_system_notification_text(request: &NotificationRequest) -> String {
    match request.priority {
        EventPriority::Critical => format!("🚨 {}\n{}", request.title, request.body),
        EventPriority::High => format!("⚠️ {}\n{}", request.title, request.body),
        _ => format!("ℹ️ {}\n{}", request.title, request.body),
    }
}

fn render_session_summary_text(summary: &SessionSummary) -> String {
    if summary.compact_text.trim().is_empty() {
        summary.title.clone()
    } else {
        format!("🗂️ {}\n{}", summary.title, summary.compact_text)
    }
}

fn is_im_channel(channel: &str) -> bool {
    matches!(
        channel,
        "wecom" | "feishu" | "lark" | "telegram" | "slack" | "discord" | "dingtalk" | "whatsapp"
    )
}

fn resolve_routed_agent_id(metadata: &serde_json::Value) -> Option<String> {
    metadata
        .get("route_agent_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn build_subagent_metadata(agent_id: Option<&str>) -> serde_json::Value {
    match agent_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(agent_id) => serde_json::json!({
            "route_agent_id": agent_id,
        }),
        None => serde_json::Value::Null,
    }
}

fn global_core_tool_names() -> Vec<String> {
    blockcell_tools::registry::global_core_tool_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect()
}

fn resolve_effective_tool_names(
    config: &Config,
    mode: InteractionMode,
    agent_id: Option<&str>,
    active_skill: Option<&ActiveSkillContext>,
    intents: &[IntentCategory],
    available_tools: &HashSet<String>,
) -> Vec<String> {
    let mut tool_names = global_core_tool_names();

    let mut profile_tools = match mode {
        InteractionMode::Chat => {
            resolve_profile_tool_names(config, agent_id, &[IntentCategory::Chat], available_tools)
        }
        InteractionMode::General | InteractionMode::Skill => {
            resolve_profile_tool_names(config, agent_id, intents, available_tools)
        }
    };
    tool_names.append(&mut profile_tools);

    if let Some(skill) = active_skill {
        tool_names.extend(skill.tools.iter().cloned());
    }

    tool_names.retain(|name| available_tools.contains(name));
    tool_names.sort();
    tool_names.dedup();
    tool_names
}

fn resolve_profile_tool_names(
    config: &Config,
    agent_id: Option<&str>,
    intents: &[IntentCategory],
    available_tools: &HashSet<String>,
) -> Vec<String> {
    IntentToolResolver::new(config)
        .resolve_tool_names(agent_id, intents, Some(available_tools))
        .unwrap_or_default()
}

fn scoped_tool_denied_result(tool_name: &str) -> String {
    format!(
        "Error: Tool '{}' is not available in the current built-in/skill scope.",
        tool_name
    )
}

fn normalize_path_for_check(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push("..");
                }
            }
            Component::Normal(seg) => normalized.push(seg),
        }
    }
    normalized
}

fn canonical_or_normalized(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| normalize_path_for_check(path))
}

fn is_path_within_base(base: &Path, candidate: &Path) -> bool {
    let base_norm = canonical_or_normalized(base);
    let candidate_norm = canonical_or_normalized(candidate);
    candidate_norm.starts_with(&base_norm)
}

fn tool_result_indicates_error(result: &str) -> bool {
    if result.starts_with("Tool error:")
        || result.starts_with("Error:")
        || result.starts_with("Validation error:")
        || result.starts_with("Config error:")
        || result.starts_with("Permission denied:")
    {
        return true;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(result) {
        if value.get("error").is_some() {
            return true;
        }
        if value.get("status").and_then(|v| v.as_str()) == Some("error") {
            return true;
        }
    }

    false
}

fn should_supplement_tool_schema(result: &str) -> bool {
    let lower = result.to_ascii_lowercase();
    lower.contains("unknown tool:")
        || lower.contains("validation error:")
        || lower.contains("config error:")
        || lower.contains("missing required parameter")
        || lower.contains("' is required for")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillScriptKind {
    Rhai,
    Python,
    Markdown,
}

impl SkillScriptKind {
    fn as_str(self) -> &'static str {
        match self {
            SkillScriptKind::Rhai => "rhai",
            SkillScriptKind::Python => "python",
            SkillScriptKind::Markdown => "markdown",
        }
    }
}

fn user_wants_send_image(text: &str) -> bool {
    let t = text.to_lowercase();
    let has_send =
        t.contains("发") || t.contains("发送") || t.contains("发给") || t.contains("send");
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
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if matches!(
            ext.as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp"
        ) {
            return Some(p.display().to_string());
        }
    }

    None
}

/// Strip fake tool call blocks from LLM responses.
/// Some LLMs output pseudo-tool-call syntax in plain text instead of using the
/// real function calling mechanism. Remove these before sending to user.
fn strip_fake_tool_calls(text: &str) -> String {
    let mut result = text.to_string();

    // Remove [TOOL_CALL]...[/TOOL_CALL] blocks (case-insensitive)
    while let Some(start) = result.to_lowercase().find("[tool_call]") {
        if let Some(end_tag) = result.to_lowercase()[start..].find("[/tool_call]") {
            let end = start + end_tag + "[/tool_call]".len();
            result = format!("{}{}", &result[..start], &result[end..]);
        } else {
            // No closing tag — remove from [TOOL_CALL] to end
            result = result[..start].to_string();
            break;
        }
    }

    // Remove ```tool_call...``` blocks
    while let Some(start) = result.find("```tool_call") {
        if let Some(end_tag) = result[start + 3..].find("```") {
            let end = start + 3 + end_tag + 3;
            result = format!("{}{}", &result[..start], &result[end..]);
        } else {
            result = result[..start].to_string();
            break;
        }
    }

    result.trim().to_string()
}

fn is_tool_trace_content(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    t.contains("[Called:")
        || t.contains("<tool_call")
        || t.contains("[TOOL_CALL]")
        || t.contains("[/TOOL_CALL]")
}

fn condense_web_search_result(raw: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(raw).ok()?;
    let results = val.get("results")?.as_array()?;

    let mut out = String::new();
    let mut idx = 1usize;
    for r in results.iter().take(8) {
        let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let url = r.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let snippet = r
            .get("snippet")
            .and_then(|v| v.as_str())
            .or_else(|| r.get("description").and_then(|v| v.as_str()))
            .unwrap_or("");

        if title.is_empty() && url.is_empty() && snippet.is_empty() {
            continue;
        }

        out.push_str(&format!("{}. {}\n{}\n{}\n\n", idx, title, url, {
            let s: String = snippet.chars().take(240).collect();
            if snippet.chars().count() > 240 {
                format!("{}...", s)
            } else {
                s
            }
        }));
        idx += 1;
    }

    if out.trim().is_empty() {
        None
    } else {
        Some(out.trim().to_string())
    }
}

/// Detect if a web_search result is "thin" — only contains titles/URLs with no actual content.
/// This happens when the search engine returns page titles but the snippets are empty or near-empty.
/// In this case the LLM should be directed to web_fetch specific URLs instead of giving up.
fn is_thin_search_result(raw: &str) -> bool {
    let val: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let results = match val.get("results").and_then(|v| v.as_array()) {
        Some(r) => r,
        None => return false,
    };
    if results.is_empty() {
        return false;
    }
    // Count results that have meaningful snippet content (>30 chars)
    let rich_count = results
        .iter()
        .filter(|r| {
            let snippet = r
                .get("snippet")
                .and_then(|v| v.as_str())
                .or_else(|| r.get("description").and_then(|v| v.as_str()))
                .unwrap_or("");
            snippet.chars().count() > 30
        })
        .count();
    // Thin if fewer than half the results have meaningful snippets
    rich_count * 2 < results.len()
}

/// Extract URLs from a web_search result JSON (top 3 results).
fn extract_urls_from_search_result(raw: &str) -> Vec<String> {
    let val: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let results = match val.get("results").and_then(|v| v.as_array()) {
        Some(r) => r,
        None => return vec![],
    };
    results
        .iter()
        .filter_map(|r| r.get("url").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .filter(|u| !u.is_empty())
        .take(3)
        .collect()
}

fn condense_web_fetch_result(raw: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(raw).ok()?;
    let content = val
        .get("content")
        .and_then(|v| v.as_str())
        .or_else(|| val.get("text").and_then(|v| v.as_str()))
        .unwrap_or("");

    if content.trim().is_empty() {
        return None;
    }

    let char_count = content.chars().count();
    if char_count <= 1600 {
        return Some(content.trim().to_string());
    }

    let head: String = content.chars().take(1100).collect();
    let tail: String = content
        .chars()
        .rev()
        .take(400)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    Some(format!(
        "{}\n...<trimmed {} chars>...\n{}",
        head.trim(),
        char_count.saturating_sub(1500),
        tail.trim()
    ))
}

fn is_dangerous_exec_command(command: &str) -> bool {
    let c = command.to_lowercase();
    let c = c.trim();
    if c.is_empty() {
        return false;
    }

    let direct_patterns = [
        r"(^|[;&|]\s*|\b(?:sudo|env)\s+)(?:rm|trash|unlink)\b",
        r"(^|[;&|]\s*|\b(?:sudo|env)\s+)rmdir\b",
        r"\bfind\b[\s\S]*\s-delete\b",
        r"\bfind\b[\s\S]*\s-exec\s+rm\b",
        r#"\bsh\s+-c\s+['"][^'"]*\brm\b"#,
        r#"\bbash\s+-c\s+['"][^'"]*\brm\b"#,
        r#"\bzsh\s+-c\s+['"][^'"]*\brm\b"#,
        r"\bpython(?:3)?\b[\s\S]*\b(?:shutil\.rmtree|os\.remove|os\.unlink|os\.rmdir)\b",
        r"\bperl\b[\s\S]*\bunlink\b",
    ];
    for pattern in direct_patterns {
        if let Ok(re) = Regex::new(pattern) {
            if re.is_match(c) {
                return true;
            }
        }
    }

    if let Ok(rm_re) = Regex::new(r"(^|[;&|]\s*|\b(?:sudo|env)\s+)rm\b([^;&|]*)") {
        for caps in rm_re.captures_iter(c) {
            let suffix = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let has_recursive = suffix.contains(" -r")
                || suffix.contains(" -rf")
                || suffix.contains(" -fr")
                || suffix.starts_with("-r")
                || suffix.starts_with("-rf")
                || suffix.starts_with("-fr");
            let has_force = suffix.contains(" -f")
                || suffix.contains(" -rf")
                || suffix.contains(" -fr")
                || suffix.starts_with("-f")
                || suffix.starts_with("-rf")
                || suffix.starts_with("-fr");
            let has_target = suffix
                .split_whitespace()
                .any(|token| !token.starts_with('-') && !token.is_empty());
            if has_target && (has_recursive || has_force) {
                return true;
            }
            if has_target && suffix.contains("../") {
                return true;
            }
        }
    }

    let dangerous = [
        "kill ",
        "pkill",
        "killall",
        "taskkill",
        "systemctl stop",
        "service stop",
        "launchctl bootout",
        "launchctl kill",
    ];

    dangerous.iter().any(|p| c.contains(p))
}

fn is_sensitive_filename(path: &str) -> bool {
    let p = path.replace('\\', "/");
    let name = p.rsplit('/').next().unwrap_or("").to_lowercase();
    matches!(
        name.as_str(),
        "config.json5" | "config.json" | "config.toml" | "config.yaml" | "config.yml"
    )
}

fn user_explicitly_confirms_dangerous_op(user_text: &str) -> bool {
    let t = user_text.trim();
    if t.is_empty() {
        return false;
    }

    // For channels without an interactive confirm prompt (confirm_tx=None),
    // require the user to explicitly confirm in text.
    // Keep this simple and language-friendly.
    t.contains("确认")
        && (t.contains("执行") || t.contains("重启") || t.contains("继续") || t.contains("允许"))
}

fn overwrite_last_assistant_message(history: &mut [ChatMessage], new_text: &str) {
    if let Some(last) = history.last_mut() {
        if last.role == "assistant" {
            last.content = serde_json::Value::String(new_text.to_string());
        }
    }
}

/// Load (or initialise) the path-access policy from the location specified
/// in `config.security.path_access`.
///
/// Side-effect: writes the default template to disk if the file doesn't exist
/// and the configured path matches the standard `~/.blockcell/path_access.json5`
/// location, so first-time users get a ready-to-edit example.
fn load_path_policy(config: &Config, paths: &Paths) -> PathPolicy {
    use blockcell_core::path_policy::{default_policy_template, expand_tilde};

    let pa = &config.security.path_access;
    if !pa.enabled {
        return PathPolicy::safe_default();
    }

    // Resolve the configured policy-file path (supports ~/ expansion)
    let policy_path = if pa.policy_file.trim().is_empty() {
        paths.path_access_file()
    } else {
        expand_tilde(pa.policy_file.trim())
    };

    // Bootstrap: if the file doesn't exist, write the starter template
    if !policy_path.exists() {
        if let Some(parent) = policy_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&policy_path, default_policy_template()) {
            warn!(path = %policy_path.display(), error = %e, "Failed to write default path_access.json5 template");
        } else {
            info!(path = %policy_path.display(), "Wrote default path_access.json5 template");
        }
    }

    PathPolicy::load(&policy_path)
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
    provider_pool: Arc<ProviderPool>,
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
    /// Agent id bound to this runtime.
    agent_id: Option<String>,
    /// Shared memory store handle for tools.
    memory_store: Option<MemoryStoreHandle>,
    /// Capability registry handle for tools.
    capability_registry: Option<CapabilityRegistryHandle>,
    /// Core evolution engine handle for tools.
    core_evolution: Option<CoreEvolutionHandle>,
    /// Broadcast sender for streaming events to WebSocket clients (gateway mode).
    event_tx: Option<broadcast::Sender<String>>,
    /// In-memory store for structured system events emitted by runtime producers.
    system_event_store: InMemorySystemEventStore,
    /// Tick orchestrator for system event delivery.
    system_event_orchestrator: SystemEventOrchestrator,
    /// Shared emitter handle used by tools, task manager, and schedulers.
    system_event_emitter: EventEmitterHandle,
    /// Last interactive main-session target for summary / notification delivery.
    main_session_target: Option<MainSessionTarget>,
    /// Cooldown tracker: capability_id → last auto-request timestamp (epoch secs).
    /// Prevents repeated auto-triggering of the same capability within 24h.
    cap_request_cooldown: HashMap<String, i64>,
    /// Persistent registry of known channel contacts for cross-channel messaging.
    channel_contacts: blockcell_storage::ChannelContacts,
    /// Loaded path-access policy engine (from `~/.blockcell/path_access.json5`).
    path_policy: PathPolicy,
}

impl AgentRuntime {
    pub fn new(
        config: Config,
        paths: Paths,
        provider_pool: Arc<ProviderPool>,
        tool_registry: ToolRegistry,
    ) -> Result<Self> {
        let mut context_builder = ContextBuilder::new(paths.clone(), config.clone());

        // 默认使用 pool 中第一个可用 provider 作为 evolution provider
        // 可以通过 set_evolution_provider() 方法覆盖
        if let Some((_, p)) = provider_pool.acquire() {
            let llm_adapter = Arc::new(ProviderLLMAdapter { provider: p });
            context_builder.set_evolution_llm_provider(llm_adapter);
            info!("🧠 [自进化] Evolution LLM provider wired from provider pool");
        } else {
            warn!("🧠 [自进化] Failed to acquire provider from pool for evolution — evolution pipeline will not auto-drive");
        }

        let session_store = SessionStore::new(paths.clone());
        let audit_logger = AuditLogger::new(paths.clone());
        let channel_contacts = blockcell_storage::ChannelContacts::new(paths.clone());
        let path_policy = load_path_policy(&config, &paths);
        let system_event_store = InMemorySystemEventStore::default();
        let summary_queue = MainSessionSummaryQueue::with_policy(
            5,
            config.tools.tick_interval_secs.clamp(10, 300) as i64 * 1000,
        );
        let system_event_orchestrator =
            SystemEventOrchestrator::new(system_event_store.clone(), summary_queue.clone());
        let system_event_emitter: EventEmitterHandle = Arc::new(RuntimeSystemEventEmitter {
            store: system_event_store.clone(),
        });

        Ok(Self {
            config,
            paths,
            context_builder,
            provider_pool,
            tool_registry,
            session_store,
            audit_logger,
            outbound_tx: None,
            inbound_tx: None,
            confirm_tx: None,
            authorized_dirs: HashSet::new(),
            task_manager: TaskManager::new(),
            agent_id: None,
            memory_store: None,
            capability_registry: None,
            core_evolution: None,
            event_tx: None,
            system_event_store,
            system_event_orchestrator,
            system_event_emitter,
            main_session_target: None,
            cap_request_cooldown: HashMap::new(),
            channel_contacts,
            path_policy,
        })
    }

    pub fn context_builder(&self) -> &ContextBuilder {
        &self.context_builder
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
        self.sync_task_manager_event_emitter();
    }

    pub fn set_agent_id(&mut self, agent_id: Option<String>) {
        self.agent_id = agent_id;
        self.sync_task_manager_event_emitter();
    }

    /// Set the broadcast sender for streaming events to WebSocket clients.
    pub fn set_event_tx(&mut self, tx: broadcast::Sender<String>) {
        self.event_tx = Some(tx);
    }

    pub fn set_event_emitter(&mut self, emitter: EventEmitterHandle) {
        self.system_event_emitter = emitter;
        self.sync_task_manager_event_emitter();
    }

    pub fn event_emitter_handle(&self) -> EventEmitterHandle {
        self.system_event_emitter.clone()
    }

    fn sync_task_manager_event_emitter(&self) {
        self.task_manager
            .register_event_emitter(self.agent_id.as_deref(), self.system_event_emitter.clone());
    }

    fn update_main_session_target(&mut self, msg: &InboundMessage) {
        if !is_main_session_candidate(msg) {
            return;
        }

        self.main_session_target = Some(MainSessionTarget {
            channel: msg.channel.clone(),
            account_id: msg.account_id.clone(),
            chat_id: msg.chat_id.clone(),
            session_key: msg.session_key(),
        });
    }

    fn resolve_event_delivery_target(&self, scope: &EventScope) -> Option<MainSessionTarget> {
        match scope {
            EventScope::Channel { channel, chat_id } => Some(MainSessionTarget {
                channel: channel.clone(),
                account_id: None,
                chat_id: chat_id.clone(),
                session_key: format!("{}:{}", channel, chat_id),
            }),
            EventScope::Session {
                channel,
                chat_id,
                session_key,
            } => Some(MainSessionTarget {
                channel: channel.clone(),
                account_id: None,
                chat_id: chat_id.clone(),
                session_key: session_key.clone(),
            }),
            EventScope::MainSession | EventScope::Global => self.main_session_target.clone(),
        }
    }

    async fn dispatch_system_event_notification(&self, request: &NotificationRequest) {
        let target = self.resolve_event_delivery_target(&request.scope);
        let target_channel = target.as_ref().map(|value| value.channel.clone());
        let target_chat_id = target.as_ref().map(|value| value.chat_id.clone());

        if let Some(ref event_tx) = self.event_tx {
            let event = serde_json::json!({
                "type": "system_event_notification",
                "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                "event_id": request.event_id.clone(),
                "priority": request.priority,
                "title": request.title.clone(),
                "body": request.body.clone(),
                "channel": target_channel,
                "chat_id": target_chat_id,
            });
            let _ = event_tx.send(event.to_string());
        }

        if let Some(target) = target {
            if target.channel == "ws" {
                return;
            }
            if let Some(tx) = &self.outbound_tx {
                let mut outbound = OutboundMessage::new(
                    &target.channel,
                    &target.chat_id,
                    &render_system_notification_text(request),
                );
                outbound.account_id = target.account_id.clone();
                let _ = tx.send(outbound).await;
            }
        }
    }

    async fn dispatch_system_event_summary(&self, summary: &SessionSummary) {
        let target = self.main_session_target.clone();
        let target_channel = target.as_ref().map(|value| value.channel.clone());
        let target_chat_id = target.as_ref().map(|value| value.chat_id.clone());

        if let Some(ref event_tx) = self.event_tx {
            let event = serde_json::json!({
                "type": "system_event_summary",
                "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                "channel": target_channel,
                "chat_id": target_chat_id,
                "title": summary.title.clone(),
                "compact_text": summary.compact_text.clone(),
                "items": summary.items.clone(),
            });
            let _ = event_tx.send(event.to_string());
        }

        if let Some(target) = target {
            if target.channel == "ws" {
                return;
            }
            if let Some(tx) = &self.outbound_tx {
                let mut outbound = OutboundMessage::new(
                    &target.channel,
                    &target.chat_id,
                    &render_session_summary_text(summary),
                );
                outbound.account_id = target.account_id.clone();
                let _ = tx.send(outbound).await;
            }
        }
    }

    async fn process_system_event_tick(&self, now_ms: i64) -> HeartbeatDecision {
        let decision = self.system_event_orchestrator.process_tick(now_ms);

        for request in &decision.immediate_notifications {
            self.dispatch_system_event_notification(request).await;
        }

        for summary in &decision.flushed_summaries {
            self.dispatch_system_event_summary(summary).await;
        }

        let _ = self.system_event_store.cleanup_expired(7 * 24 * 60 * 60);

        decision
    }

    pub fn validate_intent_router(&self) -> Result<()> {
        let resolver = crate::intent::IntentToolResolver::new(&self.config);
        let mcp = blockcell_core::mcp_config::McpResolvedConfig::load_merged(&self.paths)?;
        resolver.validate_with_mcp(&self.tool_registry, Some(&mcp))
    }

    /// 设置独立的自进化 LLM provider（可选覆盖，不影响主 pool）
    pub fn set_evolution_provider(&mut self, provider: Box<dyn Provider>) {
        let provider_arc: Arc<dyn Provider> = Arc::from(provider);
        let llm_adapter = Arc::new(ProviderLLMAdapter {
            provider: provider_arc,
        });
        self.context_builder.set_evolution_llm_provider(llm_adapter);
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

    /// Deprecated: MCP tools are now injected before runtime construction via the shared MCP manager.
    pub async fn mount_mcp_servers(&mut self) {}

    /// Create a restricted tool registry for subagents (no spawn, no message, no cron).
    pub(crate) fn subagent_tool_registry() -> ToolRegistry {
        use blockcell_tools::alert_rule::AlertRuleTool;
        use blockcell_tools::app_control::AppControlTool;
        use blockcell_tools::audio_transcribe::AudioTranscribeTool;
        use blockcell_tools::browser::BrowseTool;
        use blockcell_tools::camera::CameraCaptureTool;
        use blockcell_tools::chart_generate::ChartGenerateTool;
        use blockcell_tools::community_hub::CommunityHubTool;
        use blockcell_tools::data_process::DataProcessTool;
        use blockcell_tools::email::EmailTool;
        use blockcell_tools::encrypt::EncryptTool;
        use blockcell_tools::exec::ExecTool;
        use blockcell_tools::file_ops::FileOpsTool;
        use blockcell_tools::fs::*;
        use blockcell_tools::http_request::HttpRequestTool;
        use blockcell_tools::image_understand::ImageUnderstandTool;
        use blockcell_tools::knowledge_graph::KnowledgeGraphTool;
        use blockcell_tools::memory::{MemoryForgetTool, MemoryQueryTool, MemoryUpsertTool};
        use blockcell_tools::memory_maintenance::MemoryMaintenanceTool;
        use blockcell_tools::network_monitor::NetworkMonitorTool;
        use blockcell_tools::ocr::OcrTool;
        use blockcell_tools::office_write::OfficeWriteTool;
        use blockcell_tools::skills::ListSkillsTool;
        use blockcell_tools::stream_subscribe::StreamSubscribeTool;
        use blockcell_tools::system_info::{CapabilityEvolveTool, SystemInfoTool};
        use blockcell_tools::tasks::ListTasksTool;
        use blockcell_tools::termux_api::TermuxApiTool;
        use blockcell_tools::toggle_manage::ToggleManageTool;
        use blockcell_tools::tts::TtsTool;
        use blockcell_tools::video_process::VideoProcessTool;
        use blockcell_tools::web::*;

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
        registry.register(Arc::new(AppControlTool));
        registry.register(Arc::new(FileOpsTool));
        registry.register(Arc::new(DataProcessTool));
        registry.register(Arc::new(HttpRequestTool));
        registry.register(Arc::new(EmailTool));
        registry.register(Arc::new(AudioTranscribeTool));
        registry.register(Arc::new(ChartGenerateTool));
        registry.register(Arc::new(OfficeWriteTool));
        registry.register(Arc::new(TtsTool));
        registry.register(Arc::new(OcrTool));
        registry.register(Arc::new(ImageUnderstandTool));
        registry.register(Arc::new(VideoProcessTool));
        registry.register(Arc::new(EncryptTool));
        registry.register(Arc::new(NetworkMonitorTool));
        registry.register(Arc::new(KnowledgeGraphTool));
        registry.register(Arc::new(StreamSubscribeTool));
        registry.register(Arc::new(AlertRuleTool));
        registry.register(Arc::new(CommunityHubTool));
        registry.register(Arc::new(MemoryMaintenanceTool));
        registry.register(Arc::new(ToggleManageTool));
        registry.register(Arc::new(TermuxApiTool));
        // No SpawnTool, MessageTool, CronTool — subagents can't spawn or send messages
        registry
    }

    /// 返回当前 provider pool（供外部检查状态）
    pub fn provider_pool(&self) -> &Arc<ProviderPool> {
        &self.provider_pool
    }

    /// Build an extractive summary from session history (no LLM call).
    /// Extracts user questions and final assistant answers, truncated to fit.
    fn build_extractive_summary(history: &[ChatMessage]) -> String {
        let mut summary_parts: Vec<String> = Vec::new();
        let mut i = 0;
        while i < history.len() {
            let msg = &history[i];
            if msg.role == "user" {
                let user_text = match &msg.content {
                    serde_json::Value::String(s) => {
                        let chars: String = s.chars().take(100).collect();
                        if s.chars().count() > 100 {
                            format!("{}...", chars)
                        } else {
                            chars
                        }
                    }
                    _ => "(media)".to_string(),
                };
                // Find the last assistant text reply in this round
                let mut assistant_text = String::new();
                let mut j = i + 1;
                while j < history.len() && history[j].role != "user" {
                    if history[j].role == "assistant" && history[j].tool_calls.is_none() {
                        assistant_text = match &history[j].content {
                            serde_json::Value::String(s) => {
                                let chars: String = s.chars().take(150).collect();
                                if s.chars().count() > 150 {
                                    format!("{}...", chars)
                                } else {
                                    chars
                                }
                            }
                            _ => String::new(),
                        };
                    }
                    j += 1;
                }
                if !assistant_text.is_empty() {
                    summary_parts.push(format!("Q: {} → A: {}", user_text, assistant_text));
                } else {
                    summary_parts.push(format!("Q: {} → (tool interaction)", user_text));
                }
                i = j;
            } else {
                i += 1;
            }
        }

        // Cap total summary length
        let mut summary = summary_parts.join("\n");
        if summary.chars().count() > 800 {
            // Keep only the most recent entries
            while summary.chars().count() > 800 && summary_parts.len() > 1 {
                summary_parts.remove(0);
                summary = summary_parts.join("\n");
            }
        }
        summary
    }

    /// Compress older tool interaction rounds in current_messages during the tool call loop.
    /// Keeps: system message (index 0) + last 10 messages intact.
    /// Middle messages: assistant tool_call messages are summarized, tool results are condensed.
    fn compress_mid_loop(messages: &mut Vec<ChatMessage>) {
        if messages.len() <= 12 {
            return;
        }

        let keep_tail = 10;
        let mut split_point = messages.len().saturating_sub(keep_tail);
        if split_point <= 1 {
            return; // Only system message before the tail
        }

        // Adjust split_point backward so the tail doesn't start with orphaned tool messages
        // or an assistant-with-tool_calls whose tool responses are in the tail.
        // Walk split_point back until the tail starts cleanly.
        while split_point > 1 {
            let tail_start_role = messages[split_point].role.as_str();
            if tail_start_role == "tool" {
                // Orphaned tool message — include its assistant message too
                split_point -= 1;
                continue;
            }
            if tail_start_role == "assistant" {
                if let Some(ref tcs) = messages[split_point].tool_calls {
                    if !tcs.is_empty() {
                        // Assistant with tool_calls at the boundary — include it in tail
                        // (it's fine as-is; the tool responses follow in the tail)
                        break;
                    }
                }
            }
            break;
        }

        // Keep system message (index 0) and tail messages
        let system_msg = messages[0].clone();
        let tail: Vec<ChatMessage> = messages[split_point..].to_vec();

        // Compress middle section (indices 1..split_point)
        let mut compressed_middle: Vec<ChatMessage> = Vec::new();
        let mut i = 1;
        while i < split_point {
            let msg = &messages[i];
            if msg.role == "user" {
                // Keep user messages but trim them
                let text = match &msg.content {
                    serde_json::Value::String(s) => {
                        let chars: String = s.chars().take(150).collect();
                        if s.chars().count() > 150 {
                            format!("{}...", chars)
                        } else {
                            chars
                        }
                    }
                    _ => "(media)".to_string(),
                };
                compressed_middle.push(ChatMessage::user(&text));
            } else if msg.role == "assistant" {
                // For assistant messages with tool_calls, summarize to just the tool names
                if let Some(ref tool_calls) = msg.tool_calls {
                    let tool_names: Vec<&str> =
                        tool_calls.iter().map(|tc| tc.name.as_str()).collect();
                    let summary = format!("[Called: {}]", tool_names.join(", "));
                    let mut compressed_assistant = ChatMessage::assistant(&summary);
                    compressed_assistant.tool_calls = Some(tool_calls.clone());
                    compressed_middle.push(compressed_assistant);
                    // Skip subsequent tool result messages for these calls
                    let expected_ids: std::collections::HashSet<&str> =
                        tool_calls.iter().map(|tc| tc.id.as_str()).collect();
                    let mut j = i + 1;
                    while j < split_point {
                        if messages[j].role == "tool" {
                            if let Some(ref id) = messages[j].tool_call_id {
                                if expected_ids.contains(id.as_str()) {
                                    // Condense tool result to a short summary
                                    let tool_name = messages[j].name.as_deref().unwrap_or("tool");
                                    let result_text = match &messages[j].content {
                                        serde_json::Value::String(s) => {
                                            let chars: String = s.chars().take(80).collect();
                                            if s.chars().count() > 80 {
                                                format!("{}...", chars)
                                            } else {
                                                chars
                                            }
                                        }
                                        _ => "ok".to_string(),
                                    };
                                    let mut tool_msg = ChatMessage::tool_result(
                                        id,
                                        &format!("[{}: {}]", tool_name, result_text),
                                    );
                                    tool_msg.name = Some(tool_name.to_string());
                                    compressed_middle.push(tool_msg);
                                    j += 1;
                                    continue;
                                }
                            }
                        }
                        break;
                    }
                    i = j;
                    continue;
                } else {
                    // Regular assistant text — trim it
                    let text = match &msg.content {
                        serde_json::Value::String(s) => {
                            let chars: String = s.chars().take(200).collect();
                            if s.chars().count() > 200 {
                                format!("{}...", chars)
                            } else {
                                chars
                            }
                        }
                        _ => String::new(),
                    };
                    compressed_middle.push(ChatMessage::assistant(&text));
                }
            } else if msg.role == "tool" {
                // Orphaned tool message (not consumed by assistant handler above) — keep condensed
                let tool_name = msg.name.as_deref().unwrap_or("tool");
                let id = msg.tool_call_id.as_deref().unwrap_or("");
                let result_text = match &msg.content {
                    serde_json::Value::String(s) => {
                        if tool_name == "web_search" {
                            condense_web_search_result(s).unwrap_or_else(|| {
                                let chars: String = s.chars().take(800).collect();
                                if s.chars().count() > 800 {
                                    format!("{}...", chars)
                                } else {
                                    chars
                                }
                            })
                        } else if tool_name == "web_fetch" {
                            condense_web_fetch_result(s).unwrap_or_else(|| {
                                let chars: String = s.chars().take(1000).collect();
                                if s.chars().count() > 1000 {
                                    format!("{}...", chars)
                                } else {
                                    chars
                                }
                            })
                        } else {
                            let chars: String = s.chars().take(160).collect();
                            if s.chars().count() > 160 {
                                format!("{}...", chars)
                            } else {
                                chars
                            }
                        }
                    }
                    _ => "ok".to_string(),
                };
                let mut tool_msg =
                    ChatMessage::tool_result(id, &format!("[{}: {}]", tool_name, result_text));
                tool_msg.name = Some(tool_name.to_string());
                compressed_middle.push(tool_msg);
            }
            // else: skip unknown roles
            i += 1;
        }

        // Rebuild messages: system + compressed middle + tail
        *messages = Vec::with_capacity(1 + compressed_middle.len() + tail.len());
        messages.push(system_msg);
        messages.extend(compressed_middle);
        messages.extend(tail);
    }

    pub async fn process_message(&mut self, msg: InboundMessage) -> Result<String> {
        let session_key = msg.session_key();
        let cron_deliver_target = if msg.channel == "cron"
            && msg
                .metadata
                .get("cron_agent")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        {
            if let Some(true) = msg.metadata.get("deliver").and_then(|v| v.as_bool()) {
                if let (Some(channel), Some(to)) = (
                    msg.metadata.get("deliver_channel").and_then(|v| v.as_str()),
                    msg.metadata.get("deliver_to").and_then(|v| v.as_str()),
                ) {
                    if !channel.is_empty() && !to.is_empty() {
                        Some((channel.to_string(), to.to_string()))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        let persist_session_key = if let Some((channel, to)) = &cron_deliver_target {
            blockcell_core::build_session_key(channel, to)
        } else {
            session_key.clone()
        };
        info!(session_key = %session_key, "Processing message");
        self.update_main_session_target(&msg);

        // ── Record sender as a known channel contact (for cross-channel lookup) ──
        if msg.channel != "ws" && msg.channel != "cli" && msg.channel != "system" {
            let sender_name = msg
                .metadata
                .get("sender_nick")
                .and_then(|v| v.as_str())
                .or_else(|| msg.metadata.get("username").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            let chat_type = match msg
                .metadata
                .get("conversation_type")
                .and_then(|v| v.as_str())
            {
                Some("1") => "private",
                Some("2") => "group",
                _ => {
                    if msg
                        .metadata
                        .get("is_group")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        "group"
                    } else if msg.sender_id == msg.chat_id {
                        "private"
                    } else {
                        "group"
                    }
                }
            };
            self.channel_contacts
                .upsert(blockcell_storage::ChannelContact {
                    channel: msg.channel.clone(),
                    chat_id: msg.chat_id.clone(),
                    sender_id: msg.sender_id.clone(),
                    name: sender_name,
                    chat_type: chat_type.to_string(),
                    last_active: chrono::Utc::now().to_rfc3339(),
                });
        }

        // ── skill script fast path: execute SKILL.rhai / SKILL.py directly without LLM ──
        let scripted_kind = if msg
            .metadata
            .get("skill_rhai")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            Some(SkillScriptKind::Rhai)
        } else if msg
            .metadata
            .get("skill_python")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            Some(SkillScriptKind::Python)
        } else if msg
            .metadata
            .get("skill_markdown")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            Some(SkillScriptKind::Markdown)
        } else if msg
            .metadata
            .get("skill_script")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            match msg
                .metadata
                .get("skill_script_kind")
                .and_then(|v| v.as_str())
            {
                Some("python") => Some(SkillScriptKind::Python),
                Some("markdown") => Some(SkillScriptKind::Markdown),
                _ => Some(SkillScriptKind::Rhai),
            }
        } else {
            None
        };

        if let Some(script_kind) = scripted_kind {
            let skill_name = msg
                .metadata
                .get("skill_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            info!(
                skill = %skill_name,
                script_kind = %script_kind.as_str(),
                "Cron skill script dispatch"
            );

            let result = self
                .execute_skill_script(&skill_name, &msg, script_kind)
                .await;

            let final_response = match result {
                Ok(output) => {
                    let presentation = prepare_skill_result_for_presentation(&skill_name, &output);
                    if let Some(direct_text) = presentation.direct_text {
                        info!(
                            skill = %skill_name,
                            "Skill script returned display_text, skipping LLM summarization"
                        );
                        direct_text
                    } else {
                        // Script succeeded — make a dedicated LLM call to polish the compact
                        // result into a user-friendly format. We bypass intent classification,
                        // skill matching and the full tool loop entirely to avoid the script
                        // data being misrouted to an unrelated skill.
                        let skill_markdown = self
                            .resolve_skill_script_path(&skill_name, "SKILL.md")
                            .ok()
                            .and_then(|path| std::fs::read_to_string(path).ok())
                            .unwrap_or_default();
                        let summarize_input = presentation
                            .llm_payload
                            .clone()
                            .unwrap_or_else(|| truncate_str(&output, 4000));
                        info!(
                            skill = %skill_name,
                            output_len = output.len(),
                            summarize_input_len = summarize_input.len(),
                            "Cron skill script succeeded, calling LLM to summarize"
                        );

                        let summarize_messages = vec![
                            ChatMessage::system(
                                "你是一个定时任务结果整理助手。技能脚本已经执行完成。\
                                请优先依据输入中的技能说明、instruction 和 summary_data 整理结果。\
                                如果已经是轻量结构化摘要，就直接整理成清晰友好的最终回复。\
                                不要再调用任何工具，不要编造未提供的信息。"
                            ),
                            ChatMessage::user(&format!(
                                "[定时任务·{}] 技能脚本执行完成。以下是该技能的 SKILL.md 指导：\n\n{}\n\n以下是脚本产出的待整理数据：\n\n{}",
                                skill_name, skill_markdown, summarize_input
                            )),
                        ];

                        let llm_result = if let Some((pidx, provider)) =
                            self.provider_pool.acquire()
                        {
                            let r = provider.chat(&summarize_messages, &[]).await;
                            match &r {
                                Ok(_) => self.provider_pool.report(pidx, CallResult::Success),
                                Err(e) => self
                                    .provider_pool
                                    .report(pidx, ProviderPool::classify_error(&format!("{}", e))),
                            }
                            r
                        } else {
                            Err(blockcell_core::Error::Config(
                                "ProviderPool: no healthy providers".to_string(),
                            ))
                        };

                        match llm_result {
                            Ok(resp) => resp.content.unwrap_or_else(|| {
                                format!("[{}] 定时任务执行完成（LLM 未返回内容）", skill_name)
                            }),
                            Err(e) => {
                                warn!(skill = %skill_name, error = %e, "LLM summarization failed for cron skill result, returning fallback output");
                                presentation.fallback_text.clone()
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        skill = %skill_name,
                        error = %e,
                        "SKILL.rhai cron execution failed"
                    );
                    format!("[{}] 定时任务执行失败: {}", skill_name, e)
                }
            };

            let deliver_target =
                if let Some(true) = msg.metadata.get("deliver").and_then(|v| v.as_bool()) {
                    if let (Some(channel), Some(to)) = (
                        msg.metadata.get("deliver_channel").and_then(|v| v.as_str()),
                        msg.metadata.get("deliver_to").and_then(|v| v.as_str()),
                    ) {
                        if !channel.is_empty() && !to.is_empty() {
                            Some((channel.to_string(), to.to_string()))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

            let persist_session_key = if let Some((channel, to)) = &deliver_target {
                blockcell_core::build_session_key(channel, to)
            } else {
                session_key.clone()
            };

            let skill_history_message = ChatMessage::assistant(&final_response);
            let _ = self
                .session_store
                .append(&persist_session_key, &skill_history_message);

            if let Some((channel, to)) = deliver_target {
                if channel == "ws" {
                    if let Some(ref event_tx) = self.event_tx {
                        let event = serde_json::json!({
                            "type": "message_done",
                            "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                            "chat_id": to,
                            "task_id": "",
                            "content": final_response,
                            "tool_calls": 0,
                            "duration_ms": 0,
                            "media": [],
                            "background_delivery": true,
                            "delivery_kind": "cron",
                            "cron_kind": "script",
                        });
                        let _ = event_tx.send(event.to_string());
                    }
                } else if let Some(tx) = &self.outbound_tx {
                    let mut outbound = OutboundMessage::new(&channel, &to, &final_response);
                    outbound.account_id = msg.account_id.clone();
                    let _ = tx.send(outbound).await;
                }
            } else if let Some(tx) = &self.outbound_tx {
                let mut outbound =
                    OutboundMessage::new(&msg.channel, &msg.chat_id, &final_response);
                outbound.account_id = msg.account_id.clone();
                let _ = tx.send(outbound).await;
            }

            return Ok(final_response);
        }

        // ── Cron reminder fast path: deliver directly without LLM ──
        if msg
            .metadata
            .get("reminder")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let reminder_msg = msg
                .metadata
                .get("reminder_message")
                .and_then(|v| v.as_str())
                .unwrap_or(&msg.content);
            let job_name = msg
                .metadata
                .get("job_name")
                .and_then(|v| v.as_str())
                .unwrap_or("提醒");
            let final_response = format!("⏰ [{}] {}", job_name, reminder_msg);
            info!(job_name = %job_name, "Cron reminder delivered directly (bypassing LLM)");

            let persist_session_key =
                if let Some(true) = msg.metadata.get("deliver").and_then(|v| v.as_bool()) {
                    if let (Some(channel), Some(to)) = (
                        msg.metadata.get("deliver_channel").and_then(|v| v.as_str()),
                        msg.metadata.get("deliver_to").and_then(|v| v.as_str()),
                    ) {
                        if !channel.is_empty() && !to.is_empty() {
                            blockcell_core::build_session_key(channel, to)
                        } else {
                            session_key.clone()
                        }
                    } else {
                        session_key.clone()
                    }
                } else {
                    session_key.clone()
                };

            let reminder_history_message = ChatMessage::assistant(&final_response);
            let _ = self
                .session_store
                .append(&persist_session_key, &reminder_history_message);

            // Send to outbound (CLI printer + gateway's outbound_to_ws_bridge)
            if let Some(tx) = &self.outbound_tx {
                let mut outbound =
                    OutboundMessage::new(&msg.channel, &msg.chat_id, &final_response);
                outbound.account_id = msg.account_id.clone();
                let _ = tx.send(outbound).await;
            }

            // Deliver to external channel if configured
            if let Some(true) = msg.metadata.get("deliver").and_then(|v| v.as_bool()) {
                if let (Some(channel), Some(to)) = (
                    msg.metadata.get("deliver_channel").and_then(|v| v.as_str()),
                    msg.metadata.get("deliver_to").and_then(|v| v.as_str()),
                ) {
                    if channel == "ws" {
                        if let Some(ref event_tx) = self.event_tx {
                            let event = serde_json::json!({
                                "type": "message_done",
                                "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                                "chat_id": to,
                                "task_id": "",
                                "content": final_response,
                                "tool_calls": 0,
                                "duration_ms": 0,
                                "media": [],
                                "background_delivery": true,
                                "delivery_kind": "cron",
                                "cron_kind": "reminder",
                            });
                            let _ = event_tx.send(event.to_string());
                        }
                    }
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

        // Auto-set session display name from first user message
        if history.is_empty() {
            if let Some(new_name) = self
                .session_store
                .set_session_name_if_new(&session_key, &msg.content)
            {
                if msg.channel == "ws" {
                    if let Some(ref event_tx) = self.event_tx {
                        let event = serde_json::json!({
                            "type": "session_renamed",
                            "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                            "chat_id": msg.chat_id,
                            "name": new_name,
                        });
                        let _ = event_tx.send(event.to_string());
                    }
                }
            }
        }

        let classifier = crate::intent::IntentClassifier::new();

        // Load disabled toggles for filtering
        let disabled_tools = load_disabled_toggles(&self.paths, "tools");
        let disabled_skills = load_disabled_toggles(&self.paths, "skills");

        let forced_skill_name = msg
            .metadata
            .get("forced_skill_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let active_skill = self
            .context_builder
            .resolve_active_skill_by_name(forced_skill_name, &disabled_skills)
            .or_else(|| {
                self.context_builder
                    .resolve_active_skill(&msg.content, &disabled_skills)
            });
        let chat_intents = classifier.classify(&msg.content);
        let is_chat = active_skill.is_none()
            && chat_intents.len() == 1
            && matches!(chat_intents[0], crate::intent::IntentCategory::Chat);
        let mode = if active_skill.is_some() {
            InteractionMode::Skill
        } else if is_chat {
            InteractionMode::Chat
        } else {
            InteractionMode::General
        };
        info!(
            mode = ?mode,
            active_skill = active_skill.as_ref().map(|s| s.name.as_str()),
            "Interaction mode resolved"
        );

        let available_tools: HashSet<String> =
            self.tool_registry.tool_names().into_iter().collect();
        let routed_agent_id = self.agent_id.as_deref();
        let mut tool_names = resolve_effective_tool_names(
            &self.config,
            mode,
            routed_agent_id,
            active_skill.as_ref(),
            &chat_intents,
            &available_tools,
        );

        if tool_names.is_empty() && !matches!(mode, InteractionMode::Chat) {
            tool_names = global_core_tool_names();
            tool_names.retain(|name| available_tools.contains(name));
        }

        // Ghost routine: ensure required tools are always available.
        // Rationale: intent classification may treat the routine prompt as Chat, producing zero tools,
        // which would cause the LLM to think tools are unavailable.
        if msg.metadata.get("ghost").and_then(|v| v.as_bool()) == Some(true) {
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
                if !tool_names.iter().any(|tool_name| tool_name == name) {
                    tool_names.push(name.to_string());
                }
            }
        }

        tool_names.sort();
        tool_names.dedup();

        // Collect tool-specific prompt rules from the registry for actually loaded tools.
        let mode_names: Vec<String> = match mode {
            InteractionMode::Skill => active_skill
                .as_ref()
                .map(|skill| vec![format!("Skill:{}", skill.name)])
                .unwrap_or_else(|| vec!["Skill".to_string()]),
            InteractionMode::Chat => vec!["Chat".to_string()],
            InteractionMode::General => vec!["General".to_string()],
        };
        let prompt_ctx = blockcell_tools::PromptContext {
            channel: &msg.channel,
            intents: &mode_names,
        };
        let tool_name_refs: Vec<&str> = tool_names.iter().map(|s| s.as_str()).collect();
        let mut tool_prompt_rules = self
            .tool_registry
            .get_prompt_rules(&tool_name_refs, &prompt_ctx);
        // MCP meta-rule: inject if any loaded tool is an MCP tool (name contains "__")
        if tool_names.iter().any(|t| t.contains("__")) {
            tool_prompt_rules.push("- **MCP (Model Context Protocol)**: blockcell **已内置 MCP 客户端支持**，可连接任意 MCP 服务器（SQLite、GitHub、文件系统、数据库等）。MCP 工具会以 `<serverName>__<toolName>` 格式出现在工具列表中。若用户询问 MCP 功能或当前工具列表中无 MCP 工具，说明尚未配置 MCP 服务器，请引导用户使用 `blockcell mcp add <template>` 快捷添加，或直接编辑 `~/.blockcell/mcp.json` / `~/.blockcell/mcp.d/*.json`。例如：`blockcell mcp add sqlite --db-path /tmp/test.db`，重启后即可使用。".to_string());
        }

        // Build messages for LLM with skill-first mode prompt.
        // Note: build_messages_for_mode_with_channel appends the current user message from user_content,
        // so we pass history WITHOUT the current user message to avoid duplication.
        let pending_intent = msg
            .metadata
            .get("media_pending_intent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let messages = self.context_builder.build_messages_for_mode_with_channel(
            &history,
            &msg.content,
            &msg.media,
            mode,
            active_skill.as_ref(),
            &disabled_skills,
            &disabled_tools,
            &msg.channel,
            pending_intent,
            &tool_names,
            &tool_prompt_rules,
        );

        // Now add user message to history for session persistence
        history.push(ChatMessage::user(&msg.content));

        // Get tool schemas from resolved tool names
        let mut tools = if tool_names.is_empty() {
            // Chat mode: no tools
            vec![]
        } else {
            let tool_name_refs: Vec<&str> = tool_names.iter().map(String::as_str).collect();
            let mut schemas = self.tool_registry.get_tiered_schemas(
                &tool_name_refs,
                blockcell_tools::registry::global_core_tool_names(),
            );
            if !disabled_tools.is_empty() {
                schemas.retain(|schema| {
                    let name = schema
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    !disabled_tools.contains(name)
                });
            }
            schemas
        };
        info!(
            mode = ?mode,
            active_skill = active_skill.as_ref().map(|s| s.name.as_str()),
            tool_count = tools.len(),
            disabled_tools = disabled_tools.len(),
            disabled_skills = disabled_skills.len(),
            "Tools loaded for interaction mode"
        );

        // Main loop with max iterations
        let max_iterations = self.config.agents.defaults.max_tool_iterations;
        let mut current_messages = messages;
        let mut final_response = String::new();
        let mut message_tool_sent_media = false;
        let mut tool_fail_counts: HashMap<String, u32> = HashMap::new();
        // Collect media paths produced by tools (screenshots, generated images, etc.)
        let mut collected_media: Vec<String> = Vec::new();

        for iteration in 0..max_iterations {
            debug!(iteration, "LLM call iteration");
            debug!(
                iteration,
                current_messages_len = current_messages.len(),
                tool_schema_count = tools.len(),
                "LLM loop state"
            );

            // Call LLM with retry on transient errors
            let max_retries = self.config.agents.defaults.llm_max_retries;
            let base_delay_ms = self.config.agents.defaults.llm_retry_delay_ms;
            let mut last_error = None;
            let mut response_opt = None;

            for attempt in 0..=max_retries {
                if attempt > 0 {
                    let delay_ms = base_delay_ms * (1u64 << (attempt - 1).min(4));
                    warn!(
                        attempt,
                        max_retries, delay_ms, iteration, "Retrying LLM call after transient error"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                // 从 pool 中选取一个可用 provider（每次可能不同）
                let (pool_idx, provider) = match self.provider_pool.acquire() {
                    Some(p) => p,
                    None => {
                        last_error = Some(blockcell_core::Error::Config(
                            "ProviderPool: no healthy providers available".to_string(),
                        ));
                        break;
                    }
                };

                // 使用流式调用
                match provider.chat_stream(&current_messages, &tools).await {
                    Ok(mut stream_rx) => {
                        if attempt > 0 {
                            info!(
                                attempt,
                                iteration, pool_idx, "LLM stream call succeeded after retry"
                            );
                        }
                        self.provider_pool.report(pool_idx, CallResult::Success);

                        // 处理流式响应
                        let mut accumulated_content = String::new();
                        let mut accumulated_reasoning = String::new();
                        let mut tool_call_accumulators: HashMap<String, ToolCallAccumulator> = HashMap::new();

                        // 流接收超时：5分钟，防止恶意或 buggy provider 无限挂起
                        const STREAM_TIMEOUT_SECS: u64 = 300;

                        loop {
                            let recv_result = tokio::time::timeout(
                                std::time::Duration::from_secs(STREAM_TIMEOUT_SECS),
                                stream_rx.recv()
                            ).await;

                            match recv_result {
                                Ok(Some(chunk)) => {
                                    match chunk {
                                        StreamChunk::TextDelta { delta } => {
                                            accumulated_content.push_str(&delta);
                                            // 发送 token 事件
                                            if let Some(ref event_tx) = self.event_tx {
                                                let event = serde_json::json!({
                                                    "type": "token",
                                                    "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                                                    "chat_id": msg.chat_id.clone(),
                                                    "delta": delta,
                                                });
                                                let _ = event_tx.send(event.to_string());
                                            }
                                        }
                                        StreamChunk::ReasoningDelta { delta } => {
                                            accumulated_reasoning.push_str(&delta);
                                            // 发送 thinking 事件
                                            if let Some(ref event_tx) = self.event_tx {
                                                let event = serde_json::json!({
                                                    "type": "thinking",
                                                    "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                                                    "chat_id": msg.chat_id.clone(),
                                                    "content": delta,
                                                });
                                                let _ = event_tx.send(event.to_string());
                                            }
                                        }
                                        StreamChunk::ToolCallStart { index: _, id, name } => {
                                            let acc = tool_call_accumulators.entry(id.clone()).or_default();
                                            acc.id = id.clone();
                                            acc.name = name.clone();
                                            // 发送 tool_call_start 事件
                                            if let Some(ref event_tx) = self.event_tx {
                                                let event = serde_json::json!({
                                                    "type": "tool_call_start",
                                                    "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                                                    "chat_id": msg.chat_id.clone(),
                                                    "call_id": id,
                                                    "tool": name,
                                                    "params": {},
                                                });
                                                let _ = event_tx.send(event.to_string());
                                            }
                                        }
                                        StreamChunk::ToolCallDelta { index: _, id, delta } => {
                                            if let Some(acc) = tool_call_accumulators.get_mut(&id) {
                                                acc.arguments.push_str(&delta);
                                            }
                                        }
                                        StreamChunk::Done { response } => {
                                            // 始终优先使用累积的值，响应值仅作为后备
                                            // content 和 reasoning_content 来自流式累积
                                            // tool_calls 来自累积器（如果有），否则用响应值
                                            // finish_reason 和 usage 始终来自响应

                                            // 构建最终的 tool_calls：优先使用累积的
                                            let final_tool_calls = if !tool_call_accumulators.is_empty() {
                                                tool_call_accumulators
                                                    .drain()
                                                    .map(|(_, acc)| acc.to_tool_call_request())
                                                    .collect()
                                            } else {
                                                response.tool_calls.clone()
                                            };

                                            // 优先使用累积的 content，否则用响应值
                                            let final_content = if !accumulated_content.is_empty() {
                                                Some(accumulated_content.clone())
                                            } else {
                                                response.content.clone()
                                            };

                                            // 优先使用累积的 reasoning，否则用响应值
                                            let final_reasoning = if !accumulated_reasoning.is_empty() {
                                                Some(accumulated_reasoning.clone())
                                            } else {
                                                response.reasoning_content.clone()
                                            };

                                            response_opt = Some(LLMResponse {
                                                content: final_content,
                                                reasoning_content: final_reasoning,
                                                tool_calls: final_tool_calls,
                                                finish_reason: response.finish_reason.clone(),
                                                usage: response.usage.clone(),
                                            });

                                            // 发送 message_done 事件表示消息完成
                                            if let Some(ref event_tx) = self.event_tx {
                                                let event = serde_json::json!({
                                                    "type": "message_done",
                                                    "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                                                    "chat_id": msg.chat_id.clone(),
                                                });
                                                let _ = event_tx.send(event.to_string());
                                            }
                                            break;
                                        }
                                        StreamChunk::Error { message } => {
                                            warn!(error = %message, "Stream error");
                                            last_error = Some(blockcell_core::Error::Provider(message));
                                            break;
                                        }
                                    }
                                }
                                Ok(None) => {
                                    // 流正常结束（channel 关闭）
                                    break;
                                }
                                Err(_) => {
                                    // 流接收超时
                                    warn!("Stream receive timeout after {} seconds", STREAM_TIMEOUT_SECS);
                                    last_error = Some(blockcell_core::Error::Provider(
                                        format!("Stream timeout after {} seconds", STREAM_TIMEOUT_SECS)
                                    ));
                                    break;
                                }
                            }
                        }

                        // 将累积的工具调用转换为完整请求
                        if response_opt.is_none() && !tool_call_accumulators.is_empty() {
                            let final_tool_calls: Vec<ToolCallRequest> = tool_call_accumulators
                                .into_iter()
                                .map(|(_, acc)| acc.to_tool_call_request())
                                .collect();

                            response_opt = Some(LLMResponse {
                                content: if accumulated_content.is_empty() {
                                    None
                                } else {
                                    Some(accumulated_content)
                                },
                                reasoning_content: if accumulated_reasoning.is_empty() {
                                    None
                                } else {
                                    Some(accumulated_reasoning)
                                },
                                tool_calls: final_tool_calls,
                                finish_reason: "stop".to_string(),
                                usage: serde_json::Value::Null,
                            });
                        }

                        break;
                    }
                    Err(e) => {
                        let err_str = format!("{}", e);
                        warn!(error = %err_str, attempt, max_retries, iteration, pool_idx, "LLM stream call failed");
                        self.provider_pool
                            .report(pool_idx, ProviderPool::classify_error(&err_str));
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
                        let _ = evo_service
                            .report_error("__llm_provider__", &format!("{}", e), None, vec![])
                            .await;
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
                let assistant_content = response.content.as_deref().unwrap_or("");
                let assistant_content = if is_tool_trace_content(assistant_content) {
                    ""
                } else {
                    assistant_content
                };
                let mut assistant_msg = ChatMessage::assistant(assistant_content);
                assistant_msg.reasoning_content = response.reasoning_content.clone();
                assistant_msg.tool_calls = Some(response.tool_calls.clone());
                current_messages.push(assistant_msg.clone());
                history.push(assistant_msg);

                // Execute each tool call, with dynamic tool supplement for intent misclassification
                let mut supplemented_tools = false;
                let mut tool_results: Vec<ChatMessage> = Vec::new();
                let mut wants_forced_answer = false;
                let mut web_search_thin_results: Vec<String> = Vec::new(); // URLs from thin search results
                for tool_call in &response.tool_calls {
                    if tool_call.name == "web_search" || tool_call.name == "web_fetch" {
                        wants_forced_answer = true;
                    }
                    // Check message tool has media BEFORE execution (for message_tool_sent_media flag only)
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
                    let result = if tool_names.iter().any(|allowed| allowed == &tool_call.name) {
                        self.execute_tool_call(tool_call, &msg).await
                    } else {
                        scoped_tool_denied_result(&tool_call.name)
                    };

                    // Collect media paths from tool results for WebUI display.
                    // Skip the "message" tool — it already dispatches its own OutboundMessage
                    // with media; collecting here would cause a duplicate send.
                    if tool_call.name != "message" {
                        if let Ok(ref rv) = serde_json::from_str::<serde_json::Value>(&result) {
                            let media_exts = [
                                "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "mp3", "wav",
                                "m4a", "mp4", "webm", "mov",
                            ];
                            // Scalar fields: output_path, path, file_path, etc.
                            for key in &[
                                "output_path",
                                "path",
                                "file_path",
                                "screenshot_path",
                                "image_path",
                            ] {
                                if let Some(p) = rv.get(key).and_then(|v| v.as_str()) {
                                    let ext = p.rsplit('.').next().unwrap_or("").to_lowercase();
                                    if media_exts.contains(&ext.as_str()) {
                                        collected_media.push(p.to_string());
                                    }
                                }
                            }
                            // Array field: "media"
                            if let Some(arr) = rv.get("media").and_then(|v| v.as_array()) {
                                for mv in arr {
                                    if let Some(p) = mv.as_str() {
                                        let ext = p.rsplit('.').next().unwrap_or("").to_lowercase();
                                        if media_exts.contains(&ext.as_str()) {
                                            collected_media.push(p.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Detect thin web_search results (only titles/URLs, no actual content).
                    // When this happens, extract the top URLs so the next hint can suggest web_fetch.
                    if tool_call.name == "web_search" && !result.starts_with("Tool error:") {
                        if is_thin_search_result(&result) {
                            let urls = extract_urls_from_search_result(&result);
                            if !urls.is_empty() {
                                web_search_thin_results.extend(urls);
                            }
                        }
                    }

                    // Track tool failures for fallback hint injection
                    let is_error = tool_result_indicates_error(&result);
                    if is_error {
                        let count = tool_fail_counts.entry(tool_call.name.clone()).or_insert(0);
                        *count += 1;
                    } else {
                        // Reset on success
                        tool_fail_counts.remove(&tool_call.name);
                    }

                    // Dynamic tool supplement: if tool was not found or validation failed
                    // (e.g. lightweight schema had no params), inject full schema and retry.
                    let needs_supplement = should_supplement_tool_schema(&result);
                    if needs_supplement {
                        if let Some(schema) = self.tool_registry.get(&tool_call.name) {
                            // Check if we need to upgrade from lightweight to full schema
                            let already_full = tools.iter().any(|t| {
                                t.get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|n| n.as_str())
                                    == Some(&tool_call.name)
                                    && t.get("function")
                                        .and_then(|f| f.get("parameters"))
                                        .and_then(|p| p.get("properties"))
                                        .map(|props| {
                                            props.as_object().map_or(false, |o| !o.is_empty())
                                        })
                                        .unwrap_or(false)
                            });
                            if !already_full {
                                let schema_val = serde_json::json!({
                                    "type": "function",
                                    "function": {
                                        "name": schema.schema().name,
                                        "description": schema.schema().description,
                                        "parameters": schema.schema().parameters
                                    }
                                });
                                // Replace lightweight schema with full schema
                                tools.retain(|t| {
                                    t.get("function")
                                        .and_then(|f| f.get("name"))
                                        .and_then(|n| n.as_str())
                                        != Some(&tool_call.name)
                                });
                                tools.push(schema_val);
                                supplemented_tools = true;
                                info!(tool = %tool_call.name, "Dynamically supplemented tool with full schema");
                            }
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

                // Normal path: commit tool results to messages and history,
                // trimming each tool result to prevent unbounded growth.
                for mut tool_msg in tool_results {
                    // Trim tool result content (tool results can be very large,
                    // e.g. web_fetch markdown, finance_api JSON arrays)
                    if let serde_json::Value::String(ref s) = tool_msg.content {
                        let char_count = s.chars().count();
                        if char_count > 2400 {
                            let head: String = s.chars().take(1600).collect();
                            let tail: String = s
                                .chars()
                                .rev()
                                .take(800)
                                .collect::<String>()
                                .chars()
                                .rev()
                                .collect();
                            tool_msg.content = serde_json::Value::String(format!(
                                "{}\n...<trimmed {} chars>...\n{}",
                                head,
                                char_count - 2400,
                                tail
                            ));
                        }
                    }
                    current_messages.push(tool_msg.clone());
                    history.push(tool_msg);
                }

                if wants_forced_answer && iteration + 1 < max_iterations {
                    if !web_search_thin_results.is_empty() {
                        // Thin results: guide LLM to fetch actual page content instead of giving up
                        let urls_hint = web_search_thin_results
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n- ");
                        let hint = format!(
                            "搜索结果只包含链接标题，没有具体内容。**不要直接返回\"未找到\"，请立即改用 `web_fetch` 直接抓取以下页面获取真实数据**：\n- {}\n\n抓取后给出最终答案。",
                            urls_hint
                        );
                        current_messages.push(ChatMessage::user(&hint));
                    } else {
                        current_messages.push(ChatMessage::user(
                            "请基于刚才工具返回的结果直接给出最终答案（例如：整理成要点/列表/摘要）。除非结果明显不足，否则不要继续调用 web_search/web_fetch。",
                        ));
                    }
                }

                // Fallback hint: when a tool has failed 2+ times, tell the LLM to switch
                // to alternative tools. This prevents infinite retry loops (e.g. qveris without API key).
                let repeated_failures: Vec<String> = tool_fail_counts
                    .iter()
                    .filter(|(_, count)| **count >= 2)
                    .map(|(name, count)| format!("{} ({}x)", name, count))
                    .collect();
                if !repeated_failures.is_empty() {
                    let hint = format!(
                        "⚠️ 以下工具连续失败: {}。请不要继续重试，改用其他可用工具完成任务。对于金融数据查询失败，可降级使用 `web_search` 搜索相关新闻。",
                        repeated_failures.join(", ")
                    );
                    warn!(failures = ?repeated_failures, "Injecting fallback hint due to repeated tool failures");
                    current_messages.push(ChatMessage::user(&hint));
                }

                // Mid-loop compression: if accumulated messages are getting large,
                // compress older tool interaction rounds (keep system + recent 2 rounds).
                // This prevents multi-round tool calling from blowing up context.
                if current_messages.len() > 20 {
                    Self::compress_mid_loop(&mut current_messages);
                }

                if short_circuit_after_tools {
                    final_response.clear();
                    break;
                }

                if iteration == max_iterations - 1 {
                    warn!(
                        iteration,
                        max_iterations, "Reached max iterations; forcing a final no-tools answer"
                    );
                    let mut final_messages = current_messages.clone();
                    final_messages.push(ChatMessage::user(
                        "请基于以上工具调用的结果，直接给出最终答案。不要再调用任何工具，也不要输出类似[Called: ...]的过程信息。",
                    ));

                    let chat_result = if let Some((pidx, p)) = self.provider_pool.acquire() {
                        let r = p.chat(&final_messages, &[]).await;
                        match &r {
                            Ok(_) => self.provider_pool.report(pidx, CallResult::Success),
                            Err(e) => self
                                .provider_pool
                                .report(pidx, ProviderPool::classify_error(&format!("{}", e))),
                        }
                        r
                    } else {
                        Err(blockcell_core::Error::Config(
                            "ProviderPool: no healthy providers".to_string(),
                        ))
                    };
                    match chat_result {
                        Ok(r) => {
                            final_response = r.content.unwrap_or_default();
                            history.push(ChatMessage::assistant(&final_response));
                        }
                        Err(e) => {
                            warn!(error = %e, "Final no-tools LLM call failed");
                            final_response =
                                "I've reached the maximum number of tool iterations.".to_string();
                            history.push(ChatMessage::assistant(&final_response));
                        }
                    }
                    break;
                }
            } else {
                // No tool calls, we have the final response
                final_response = response.content.unwrap_or_default();

                // Add to history
                history.push(ChatMessage::assistant(&final_response));
                break;
            }
        }

        if is_im_channel(&msg.channel)
            && user_wants_send_image(&msg.content)
            && !message_tool_sent_media
        {
            if let Some(image_path) = pick_image_path(&self.paths, &history).await {
                info!(
                    image_path = %image_path,
                    channel = %msg.channel,
                    "Auto-sending image fallback (LLM did not call message tool)"
                );
                if let Some(tx) = &self.outbound_tx {
                    let mut outbound = OutboundMessage::new(&msg.channel, &msg.chat_id, "");
                    outbound.account_id = msg.account_id.clone();
                    outbound.media = vec![image_path.clone()];
                    let _ = tx.send(outbound).await;
                }

                final_response.clear();
                overwrite_last_assistant_message(&mut history, "");
            }
        }

        // Trim leading/trailing whitespace — LLMs often return "\n\nContent..."
        let final_response = final_response.trim().to_string();

        // Strip fake tool call blocks — some LLMs output pseudo-tool-call syntax
        // in plain text (e.g. [TOOL_CALL]...[/TOOL_CALL]) instead of using the
        // real function calling mechanism. Remove these before sending to user.
        let final_response = strip_fake_tool_calls(&final_response);

        // Save session
        self.session_store.save(&persist_session_key, &history)?;

        // L2 incremental session summary (P2-1):
        // When history is long enough, build an extractive summary and store it.
        // This is picked up by generate_brief_for_query for future context injection.
        if history.len() >= 6 {
            if let Some(ref store) = self.memory_store {
                let summary = Self::build_extractive_summary(&history);
                if !summary.is_empty() {
                    if let Err(e) = store.upsert_session_summary(&persist_session_key, &summary) {
                        debug!(error = %e, "Failed to upsert session summary");
                    }
                }
            }
        }

        if msg.channel == "cron"
            && msg
                .metadata
                .get("cron_agent")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        {
            if let Some(tx) = &self.outbound_tx {
                let mut outbound = OutboundMessage::new(&msg.channel, &msg.chat_id, &final_response);
                outbound.account_id = msg.account_id.clone();
                outbound.media = collected_media.clone();
                outbound.metadata = extract_reply_metadata(&msg);
                let _ = tx.send(outbound).await;
            }

            if let Some((channel, to)) = cron_deliver_target {
                if channel == "ws" {
                    if let Some(ref event_tx) = self.event_tx {
                        let event = serde_json::json!({
                            "type": "message_done",
                            "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                            "chat_id": to,
                            "task_id": "",
                            "content": final_response,
                            "tool_calls": 0,
                            "duration_ms": 0,
                            "media": collected_media,
                            "background_delivery": true,
                            "delivery_kind": "cron",
                            "cron_kind": "agent",
                        });
                        let _ = event_tx.send(event.to_string());
                    }
                    if let Some(tx) = &self.outbound_tx {
                        let mut outbound = OutboundMessage::new(&channel, &to, &final_response);
                        outbound.account_id = msg.account_id.clone();
                        outbound.media = collected_media.clone();
                        let _ = tx.send(outbound).await;
                    }
                } else if let Some(tx) = &self.outbound_tx {
                    let mut outbound = OutboundMessage::new(&channel, &to, &final_response);
                    outbound.account_id = msg.account_id.clone();
                    outbound.media = collected_media.clone();
                    let _ = tx.send(outbound).await;
                }
            }

            return Ok(final_response);
        }

        // Emit message_done event to WebSocket clients.
        // Only for "ws" channel — the bridge's outbound_to_ws_bridge skips ws-channel
        // messages (to avoid duplicate), so we must emit directly via event_tx.
        // For all other channels (cron, subagent, cli, etc.), the bridge will create
        // the WS event from the outbound_tx message, preventing double-send.
        if msg.channel == "ws" {
            if let Some(ref event_tx) = self.event_tx {
                let event = serde_json::json!({
                    "type": "message_done",
                    "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                    "chat_id": msg.chat_id,
                    "task_id": "",
                    "content": final_response,
                    "tool_calls": 0,
                    "duration_ms": 0,
                    "media": collected_media,
                });
                let _ = event_tx.send(event.to_string());
            }
        }

        // Send response to outbound for all channels (including CLI and cron).
        // Skip ghost channel — ghost responses don't need CLI printing or external
        // channel dispatch, and the ws event_tx above already handles ws display.
        if msg.channel != "ghost" {
            if let Some(tx) = &self.outbound_tx {
                let mut outbound =
                    OutboundMessage::new(&msg.channel, &msg.chat_id, &final_response);
                outbound.account_id = msg.account_id.clone();
                outbound.media = collected_media.clone();
                outbound.metadata = extract_reply_metadata(&msg);
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
            "file_ops" | "data_process" | "audio_transcribe" | "chart_generate"
            | "office_write" | "video_process" | "health_api" | "encrypt" => {
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
        is_path_within_base(&self.paths.workspace(), resolved)
    }

    /// Check whether a resolved path falls within an already-authorized directory.
    fn is_path_authorized(&self, resolved: &std::path::Path) -> bool {
        let rp = canonical_or_normalized(resolved);
        self.authorized_dirs
            .iter()
            .any(|dir| rp.starts_with(canonical_or_normalized(dir.as_path())))
    }

    /// Record a directory as authorized so future accesses within it are auto-approved.
    fn authorize_directory(&mut self, resolved: &std::path::Path) {
        // If the path is a directory, authorize it directly.
        // If it's a file, authorize its parent directory.
        let dir = if resolved.is_dir() {
            resolved.to_path_buf()
        } else {
            resolved
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| resolved.to_path_buf())
        };
        let dir = canonical_or_normalized(&dir);
        if self.authorized_dirs.insert(dir.clone()) {
            info!(dir = %dir.display(), "Directory authorized for future access");
        }
    }

    /// For tools that access the filesystem, check if any paths are outside the
    /// workspace. Applies the path-access policy first; only paths whose policy
    /// outcome is `Confirm` are forwarded to the user for interactive approval.
    ///
    /// Priority (highest → lowest):
    /// 1. Workspace-safe paths  → always allowed
    /// 2. Session-authorized dirs → allowed (cached from prior confirmation)
    /// 3. Policy `Deny`         → rejected immediately, no confirmation sent
    /// 4. Policy `Allow`        → allowed immediately, cached for this session
    /// 5. Policy `Confirm`      → user confirmation required
    async fn check_path_permission(
        &mut self,
        tool_name: &str,
        args: &serde_json::Value,
        msg: &InboundMessage,
    ) -> bool {
        let raw_paths = self.extract_paths(tool_name, args);
        if raw_paths.is_empty() {
            return true;
        }

        let op = PathOp::from_tool_name(tool_name);

        // Classify each path by policy outcome
        let mut deny_paths: Vec<String> = Vec::new();
        let mut confirm_paths: Vec<String> = Vec::new();

        for p in &raw_paths {
            let resolved = self.resolve_path(p);

            // 1. Workspace-safe → always OK
            if self.is_path_safe(&resolved) {
                continue;
            }

            // 2. Already authorized by user this session → OK
            if self.is_path_authorized(&resolved) {
                continue;
            }

            // 3. Evaluate policy
            let action = self.path_policy.evaluate(&resolved, op);
            match action {
                PolicyAction::Deny => {
                    warn!(
                        tool = tool_name,
                        path = %resolved.display(),
                        "Path access denied by policy"
                    );
                    deny_paths.push(p.clone());
                }
                PolicyAction::Allow => {
                    // Policy explicitly allows — cache for this session
                    info!(
                        tool = tool_name,
                        path = %resolved.display(),
                        "Path access allowed by policy"
                    );
                    if self.path_policy.cache_confirmed_dirs() {
                        self.authorize_directory(&resolved);
                    }
                }
                PolicyAction::Confirm => {
                    confirm_paths.push(p.clone());
                }
            }
        }

        // Any hard-deny → reject the whole operation
        if !deny_paths.is_empty() {
            return false;
        }

        // All paths were allowed (workspace / session-cache / policy-allow)
        if confirm_paths.is_empty() {
            return true;
        }

        // Need user confirmation for the remaining paths
        if let Some(confirm_tx) = &self.confirm_tx {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let request = ConfirmRequest {
                tool_name: tool_name.to_string(),
                paths: confirm_paths.clone(),
                response_tx,
                channel: msg.channel.clone(),
                chat_id: msg.chat_id.clone(),
            };

            if confirm_tx.send(request).await.is_err() {
                warn!("Failed to send confirmation request, denying access");
                return false;
            }

            match response_rx.await {
                Ok(allowed) => {
                    if allowed && self.path_policy.cache_confirmed_dirs() {
                        for p in &confirm_paths {
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
            warn!(
                tool = tool_name,
                "No confirmation channel, denying access to paths outside workspace"
            );
            false
        }
    }

    async fn confirm_dangerous_operation(
        &mut self,
        tool_name: &str,
        items: Vec<String>,
        msg: &InboundMessage,
    ) -> bool {
        if items.is_empty() {
            return true;
        }
        if let Some(confirm_tx) = &self.confirm_tx {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let request = ConfirmRequest {
                tool_name: tool_name.to_string(),
                paths: items,
                response_tx,
                channel: msg.channel.clone(),
                chat_id: msg.chat_id.clone(),
            };
            if confirm_tx.send(request).await.is_err() {
                warn!(
                    tool = tool_name,
                    "Failed to send dangerous-operation confirmation request, denying"
                );
                return false;
            }
            match response_rx.await {
                Ok(allowed) => allowed,
                Err(_) => {
                    warn!(
                        tool = tool_name,
                        "Dangerous-operation confirmation channel closed, denying"
                    );
                    false
                }
            }
        } else {
            warn!(
                tool = tool_name,
                "No confirmation channel, denying dangerous operation"
            );
            false
        }
    }

    async fn execute_tool_call(
        &mut self,
        tool_call: &ToolCallRequest,
        msg: &InboundMessage,
    ) -> String {
        // Hard block: reject disabled tools at execution level (not just prompt filtering)
        let disabled_tools = load_disabled_toggles(&self.paths, "tools");
        if disabled_tools.contains(&tool_call.name) {
            return serde_json::json!({
                "error": format!("Tool '{}' is currently disabled via toggles.", tool_call.name),
                "tool": tool_call.name,
                "hint": "This tool has been disabled by the user. Use toggle_manage to re-enable it, or use an alternative tool."
            }).to_string();
        }
        // Also block disabled skills invoked as tools (skill scripts registered as tools)
        let disabled_skills = load_disabled_toggles(&self.paths, "skills");
        if disabled_skills.contains(&tool_call.name) {
            return serde_json::json!({
                "error": format!("Skill '{}' is currently disabled via toggles.", tool_call.name),
                "tool": tool_call.name,
                "hint": "This skill has been disabled by the user. Use toggle_manage to re-enable it."
            }).to_string();
        }

        // Dangerous-operation gate: require explicit user confirmation before executing
        // self-destructive commands or destructive file operations.
        if tool_call.name == "exec" {
            if let Some(cmd) = tool_call.arguments.get("command").and_then(|v| v.as_str()) {
                if is_dangerous_exec_command(cmd) {
                    let items = vec![format!("command: {}", cmd)];
                    if self.confirm_tx.is_none() {
                        if !user_explicitly_confirms_dangerous_op(&msg.content) {
                            return serde_json::json!({
                                "error": "Permission denied: dangerous exec command requires explicit user confirmation.",
                                "tool": "exec",
                                "hint": "This channel cannot show an interactive confirm prompt. Reply with '确认执行' (or '确认重启') to proceed, otherwise I will not run kill/pkill/killall/service-stop commands."
                            }).to_string();
                        }
                    } else if !self.confirm_dangerous_operation("exec", items, msg).await {
                        return serde_json::json!({
                            "error": "Permission denied: dangerous exec command requires explicit user confirmation.",
                            "tool": "exec",
                            "hint": "The command looks dangerous (e.g. kill/pkill/killall/service stop). Ask the user to confirm explicitly before running it."
                        }).to_string();
                    }
                }
            }
        }

        if tool_call.name == "file_ops" {
            let action = tool_call
                .arguments
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let path = tool_call
                .arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let destination = tool_call
                .arguments
                .get("destination")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let recursive = tool_call
                .arguments
                .get("recursive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let mut items = Vec::new();
            if action == "delete" && recursive {
                items.push(format!("file_ops delete recursive=true path={}", path));
            }
            if (action == "delete" || action == "rename" || action == "move")
                && (is_sensitive_filename(path) || is_sensitive_filename(destination))
            {
                items.push(format!(
                    "file_ops {} sensitive file (config*) path={} destination={}",
                    action, path, destination
                ));
            }

            if !items.is_empty() {
                if self.confirm_tx.is_none() {
                    if !user_explicitly_confirms_dangerous_op(&msg.content) {
                        return serde_json::json!({
                            "error": "Permission denied: destructive file operation requires explicit user confirmation.",
                            "tool": "file_ops",
                            "hint": "This channel cannot show an interactive confirm prompt. Reply with '确认执行' to proceed with recursive delete / config file modifications."
                        }).to_string();
                    }
                } else if !self
                    .confirm_dangerous_operation("file_ops", items, msg)
                    .await
                {
                    return serde_json::json!({
                        "error": "Permission denied: destructive file operation requires explicit user confirmation.",
                        "tool": "file_ops",
                        "hint": "Deleting recursively or modifying config files is considered dangerous. Ask the user to confirm before proceeding."
                    }).to_string();
                }
            }
        }

        // Check path safety before executing filesystem/exec tools
        if !self
            .check_path_permission(&tool_call.name, &tool_call.arguments, msg)
            .await
        {
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
            provider_pool: Arc::clone(&self.provider_pool),
            agent_id: resolve_routed_agent_id(&msg.metadata).or_else(|| self.agent_id.clone()),
            event_emitter: self.system_event_emitter.clone(),
        });

        let ctx = blockcell_tools::ToolContext {
            workspace: self.paths.workspace(),
            builtin_skills_dir: Some(self.paths.builtin_skills_dir()),
            session_key: msg.session_key(),
            channel: msg.channel.clone(),
            account_id: msg.account_id.clone(),
            chat_id: msg.chat_id.clone(),
            config: self.config.clone(),
            permissions: blockcell_core::types::PermissionSet::new(), // TODO: Load from skill meta
            task_manager: Some(tm_handle),
            memory_store: self.memory_store.clone(),
            outbound_tx: self.outbound_tx.clone(),
            spawn_handle: Some(spawn_handle),
            capability_registry: self.capability_registry.clone(),
            core_evolution: self.core_evolution.clone(),
            event_emitter: Some(self.system_event_emitter.clone()),
            channel_contacts_file: Some(self.paths.channel_contacts_file()),
        };

        // Emit tool_call_start event to WebSocket clients
        if let Some(ref event_tx) = self.event_tx {
            let event = serde_json::json!({
                "type": "tool_call_start",
                "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                "chat_id": msg.chat_id,
                "task_id": "",
                "tool": tool_call.name,
                "call_id": tool_call.id,
                "params": tool_call.arguments,
            });
            let _ = event_tx.send(event.to_string());
        }

        let start = std::time::Instant::now();
        let result = self
            .tool_registry
            .execute(&tool_call.name, ctx, tool_call.arguments.clone())
            .await;
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
                        skills_dir
                            .canonicalize()
                            .ok()
                            .is_some_and(|sd| c.starts_with(&sd))
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
                let source_snippet = self
                    .context_builder
                    .skill_manager()
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
            let mut reported_name = tool_call.name.clone();
            if let Some(sm) = self.context_builder.skill_manager() {
                if let Some(skill) = sm.match_skill(&msg.content, &HashSet::new()) {
                    reported_name = skill.name.clone();
                }
            }
            evo_service
                .report_skill_call(&reported_name, is_error)
                .await;
        }

        // Emit tool_call_result event to WebSocket clients
        if let Some(ref event_tx) = self.event_tx {
            let event = serde_json::json!({
                "type": "tool_call_result",
                "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
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

    /// Execute a skill script directly (for cron skill jobs and WebUI skill tests).
    pub async fn execute_skill_script(
        &mut self,
        skill_name: &str,
        msg: &InboundMessage,
        kind: SkillScriptKind,
    ) -> Result<String> {
        match kind {
            SkillScriptKind::Rhai => self.execute_skill_rhai(skill_name, msg).await,
            SkillScriptKind::Python => self.execute_skill_python(skill_name, msg).await,
            SkillScriptKind::Markdown => self.execute_skill_markdown(skill_name, msg).await,
        }
    }

    fn resolve_skill_script_path(
        &self,
        skill_name: &str,
        file_name: &str,
    ) -> Result<std::path::PathBuf> {
        let user_path = self.paths.skills_dir().join(skill_name).join(file_name);
        if user_path.exists() {
            return Ok(user_path);
        }

        let builtin_path = self
            .paths
            .builtin_skills_dir()
            .join(skill_name)
            .join(file_name);
        if builtin_path.exists() {
            return Ok(builtin_path);
        }

        Err(blockcell_core::Error::Skill(format!(
            "{} not found for skill '{}' (checked {} and {})",
            file_name,
            skill_name,
            user_path.display(),
            builtin_path.display()
        )))
    }

    /// Execute a SKILL.rhai script directly.
    async fn execute_skill_rhai(
        &mut self,
        skill_name: &str,
        msg: &InboundMessage,
    ) -> Result<String> {
        let rhai_path = self.resolve_skill_script_path(skill_name, "SKILL.rhai")?;
        self.run_rhai_script(&rhai_path, skill_name, msg).await
    }

    /// Execute a SKILL.py script directly.
    async fn execute_skill_python(
        &mut self,
        skill_name: &str,
        msg: &InboundMessage,
    ) -> Result<String> {
        let py_path = self.resolve_skill_script_path(skill_name, "SKILL.py")?;
        self.run_python_script(&py_path, skill_name, msg).await
    }

    /// Execute a SKILL.md script directly.
    async fn execute_skill_markdown(
        &mut self,
        skill_name: &str,
        msg: &InboundMessage,
    ) -> Result<String> {
        let md_path = self.resolve_skill_script_path(skill_name, "SKILL.md")?;
        self.run_markdown_script(&md_path, skill_name, msg).await
    }

    /// Helper: run a SKILL.md-only skill by routing the message back through the
    /// normal LLM pipeline, but forcing skill matching via the skill's first trigger.
    async fn run_markdown_script(
        &mut self,
        md_path: &std::path::Path,
        skill_name: &str,
        msg: &InboundMessage,
    ) -> Result<String> {
        let skill = self
            .context_builder
            .skill_manager()
            .and_then(|sm| sm.get(skill_name))
            .ok_or_else(|| {
                blockcell_core::Error::Skill(format!("Skill '{}' not found", skill_name))
            })?;

        if !md_path.exists() {
            return Err(blockcell_core::Error::Skill(format!(
                "SKILL.md not found for skill '{}'",
                skill_name
            )));
        }

        let trigger = skill
            .meta
            .triggers
            .first()
            .cloned()
            .unwrap_or_else(|| skill_name.to_string());

        let mut routed_msg = msg.clone();
        routed_msg.content = if msg.content.trim().is_empty() {
            trigger
        } else {
            format!("{}\n{}", trigger, msg.content)
        };

        let metadata = if routed_msg.metadata.is_object() {
            routed_msg.metadata.as_object_mut()
        } else {
            routed_msg.metadata = serde_json::json!({});
            routed_msg.metadata.as_object_mut()
        };
        if let Some(obj) = metadata {
            obj.remove("skill_script");
            obj.remove("skill_script_kind");
            obj.remove("skill_markdown");
            obj.insert(
                "forced_skill_name".to_string(),
                serde_json::json!(skill_name),
            );
        }

        info!(skill = %skill_name, "SKILL.md execution routed through normal LLM skill flow");
        Box::pin(self.process_message(routed_msg)).await
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
        let event_emitter = self.system_event_emitter.clone();

        let tool_executor =
            move |tool_name: &str, params: serde_json::Value| -> Result<serde_json::Value> {
                // Security gate: block disabled tools/skills in skill scripts
                let disabled_tools = load_disabled_toggles(&paths, "tools");
                if disabled_tools.contains(tool_name) {
                    return Err(blockcell_core::Error::Tool(format!(
                        "Tool '{}' is disabled via toggles",
                        tool_name
                    )));
                }
                let disabled_skills = load_disabled_toggles(&paths, "skills");
                if disabled_skills.contains(tool_name) {
                    return Err(blockcell_core::Error::Tool(format!(
                        "Skill '{}' is disabled via toggles",
                        tool_name
                    )));
                }

                // Security gate: block dangerous exec commands from skill scripts
                if tool_name == "exec" {
                    if let Some(cmd) = params.get("command").and_then(|v| v.as_str()) {
                        if is_dangerous_exec_command(cmd) {
                            return Err(blockcell_core::Error::Tool(format!(
                                "Dangerous command blocked in skill script: {}",
                                cmd
                            )));
                        }
                    }
                }

                // Security gate: validate filesystem paths are within workspace
                let fs_tools = [
                    "read_file",
                    "write_file",
                    "edit_file",
                    "list_dir",
                    "file_ops",
                ];
                if fs_tools.contains(&tool_name) {
                    let workspace = paths.workspace();
                    for key in &["path", "destination", "output_path"] {
                        if let Some(p) = params.get(*key).and_then(|v| v.as_str()) {
                            let resolved = if std::path::Path::new(p).is_absolute() {
                                std::path::PathBuf::from(p)
                            } else {
                                workspace.join(p)
                            };
                            if !is_path_within_base(&workspace, &resolved) {
                                return Err(blockcell_core::Error::Tool(format!(
                                    "Path '{}' is outside workspace — blocked in skill script",
                                    p
                                )));
                            }
                        }
                    }
                }

                let ctx = blockcell_tools::ToolContext {
                    workspace: paths.workspace(),
                    builtin_skills_dir: Some(paths.builtin_skills_dir()),
                    session_key: session_key.clone(),
                    channel: channel.clone(),
                    account_id: None,
                    chat_id: chat_id.clone(),
                    config: config.clone(),
                    permissions: blockcell_core::types::PermissionSet::new(),
                    task_manager: Some(Arc::new(task_manager.clone())),
                    memory_store: memory_store.clone(),
                    outbound_tx: outbound_tx.clone(),
                    spawn_handle: None, // No spawning from cron skill scripts
                    capability_registry: capability_registry.clone(),
                    core_evolution: core_evolution.clone(),
                    event_emitter: Some(event_emitter.clone()),
                    channel_contacts_file: Some(paths.channel_contacts_file()),
                };

                // Execute tool synchronously via a new tokio runtime handle
                let rt = tokio::runtime::Handle::current();
                let tool_name_owned = tool_name.to_string();
                std::thread::scope(|s| {
                    s.spawn(|| {
                        rt.block_on(async { registry.execute(&tool_name_owned, ctx, params).await })
                    })
                    .join()
                    .unwrap_or_else(|_| {
                        Err(blockcell_core::Error::Tool(
                            "Tool execution panicked".into(),
                        ))
                    })
                })
            };

        // Context variables for the script
        let mut context_vars = HashMap::new();
        context_vars.insert("skill_name".to_string(), serde_json::json!(skill_name));
        context_vars.insert("trigger".to_string(), serde_json::json!("cron"));

        // Build a `ctx` map so SKILL.rhai scripts can use `ctx.user_input`, `ctx.channel`, etc.
        context_vars.insert(
            "ctx".to_string(),
            serde_json::json!({
                "user_input": msg.content,
                "skill_name": skill_name,
                "trigger": "cron",
                "channel": msg.channel,
                "chat_id": msg.chat_id,
                "message": msg.content,
                "metadata": msg.metadata,
            }),
        );

        // Execute the Rhai script in a blocking task
        let dispatcher = SkillDispatcher::new();
        let user_input = msg.content.clone();

        let result = tokio::task::spawn_blocking(move || {
            dispatcher.execute_sync(&script, &user_input, context_vars, tool_executor)
        })
        .await
        .map_err(|e| {
            blockcell_core::Error::Skill(format!("Skill execution join error: {}", e))
        })??;

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

    /// Helper: run a single .py script file and return stdout as output.
    async fn run_python_script(
        &self,
        py_path: &std::path::Path,
        skill_name: &str,
        msg: &InboundMessage,
    ) -> Result<String> {
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;

        let python_bin = if which::which("python3").is_ok() {
            "python3"
        } else if which::which("python").is_ok() {
            "python"
        } else {
            return Err(blockcell_core::Error::Skill(
                "Python runtime not found (python3/python)".to_string(),
            ));
        };

        let timeout_secs = self.config.tools.exec.timeout.clamp(1, 600) as u64;
        let mut cmd = tokio::process::Command::new(python_bin);
        cmd.arg(py_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(parent) = py_path.parent() {
            cmd.current_dir(parent);
        }

        let context = serde_json::json!({
            "skill_name": skill_name,
            "trigger": "cron",
            "user_input": msg.content,
            "channel": msg.channel,
            "chat_id": msg.chat_id,
            "metadata": msg.metadata,
        });
        cmd.env("BLOCKCELL_SKILL_CONTEXT", context.to_string());

        let mut child = cmd.spawn().map_err(|e| {
            blockcell_core::Error::Skill(format!(
                "Failed to spawn {} for {}: {}",
                python_bin,
                py_path.display(),
                e
            ))
        })?;

        // Write stdin in a separate task so it doesn't block wait_with_output.
        // The task drops stdin after writing, sending EOF to the Python process.
        let stdin_content = msg.content.clone();
        if let Some(mut stdin) = child.stdin.take() {
            tokio::spawn(async move {
                let _ = stdin.write_all(stdin_content.as_bytes()).await;
                // stdin dropped here → EOF sent to child
            });
        }

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| {
            blockcell_core::Error::Skill(format!(
                "SKILL.py execution timed out after {}s",
                timeout_secs
            ))
        })?
        .map_err(|e| blockcell_core::Error::Skill(format!("SKILL.py execution failed: {}", e)))?;

        let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let max_output_chars = 10_000;
        if stdout.len() > max_output_chars {
            stdout = format!(
                "{}\n... (output truncated)",
                truncate_str(&stdout, max_output_chars)
            );
        }
        if stderr.len() > max_output_chars {
            stderr = format!(
                "{}\n... (stderr truncated)",
                truncate_str(&stderr, max_output_chars)
            );
        }

        if !output.status.success() {
            let code = output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "terminated".to_string());
            let err = if stderr.trim().is_empty() {
                format!("SKILL.py exited with status {}", code)
            } else {
                format!("SKILL.py exited with status {}: {}", code, stderr.trim())
            };
            return Err(blockcell_core::Error::Skill(err));
        }

        let output_text = if stdout.trim().is_empty() {
            "Python skill executed successfully (no output)".to_string()
        } else {
            stdout.trim().to_string()
        };

        info!(
            skill = %skill_name,
            script = %py_path.display(),
            "SKILL.py cron execution succeeded"
        );
        Ok(output_text)
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
        let mut active_chat_tasks: HashMap<String, String> = HashMap::new();
        let mut active_message_tasks: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
        let (task_done_tx, mut task_done_rx) = mpsc::unbounded_channel::<(String, String)>();

        async fn abort_active_message_tasks(
            task_manager: &TaskManager,
            active_chat_tasks: &mut HashMap<String, String>,
            active_message_tasks: &mut HashMap<String, tokio::task::JoinHandle<()>>,
        ) {
            let active_task_ids: Vec<String> = active_message_tasks.keys().cloned().collect();
            for task_id in active_task_ids {
                if let Some(handle) = active_message_tasks.remove(&task_id) {
                    handle.abort();
                }
                task_manager.remove_task(&task_id).await;
            }
            active_chat_tasks.clear();
        }

        loop {
            tokio::select! {
                _ = async {
                    if let Some(ref mut rx) = shutdown_rx {
                        let _ = rx.recv().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    abort_active_message_tasks(
                        &self.task_manager,
                        &mut active_chat_tasks,
                        &mut active_message_tasks,
                    ).await;
                    break;
                }
                done = task_done_rx.recv() => {
                    if let Some((task_id, chat_id)) = done {
                        active_message_tasks.remove(&task_id);
                        if active_chat_tasks.get(&chat_id).is_some_and(|id| id == &task_id) {
                            active_chat_tasks.remove(&chat_id);
                        }
                    }
                }
                msg = inbound_rx.recv() => {
                    match msg {
                        Some(msg) => {
                            if msg.metadata.get("cancel").and_then(|v| v.as_bool()).unwrap_or(false) {
                                let chat_id = msg.chat_id.clone();
                                let mut cancelled = false;
                                if let Some(task_id) = active_chat_tasks.remove(&chat_id) {
                                    if let Some(handle) = active_message_tasks.remove(&task_id) {
                                        handle.abort();
                                        cancelled = true;
                                        self.task_manager.remove_task(&task_id).await;
                                        info!(chat_id = %chat_id, task_id = %task_id, "Cancelled running chat task");
                                    }
                                }
                                if cancelled {
                                    if let Some(ref event_tx) = self.event_tx {
                                        let _ = event_tx.send(
                                            serde_json::json!({
                                                "type": "message_done",
                                                "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                                                "chat_id": chat_id,
                                                "task_id": "",
                                                "content": "⏹️ 当前对话已终止",
                                                "tool_calls": 0,
                                                "duration_ms": 0
                                            }).to_string()
                                        );
                                    }
                                }
                                continue;
                            }

                            self.update_main_session_target(&msg);

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
                            let agent_id = self.agent_id.clone();
                            let event_emitter = self.system_event_emitter.clone();
                            let tool_registry = self.tool_registry.clone();
                            let task_id_clone = task_id.clone();
                            let provider_pool = Arc::clone(&self.provider_pool);
                            let chat_id_for_task = msg.chat_id.clone();
                            let task_done_tx = task_done_tx.clone();
                            let done_task_id = task_id.clone();
                            let done_chat_id = chat_id_for_task.clone();

                            // Register task
                            task_manager.create_task(
                                &task_id,
                                &label,
                                &msg.content,
                                &msg.channel,
                                &msg.chat_id,
                                self.agent_id.as_deref(),
                                false,
                            ).await;

                            if let Some(prev_task_id) = active_chat_tasks.remove(&chat_id_for_task) {
                                if let Some(prev_handle) = active_message_tasks.remove(&prev_task_id) {
                                    prev_handle.abort();
                                    self.task_manager.remove_task(&prev_task_id).await;
                                    info!(
                                        chat_id = %chat_id_for_task,
                                        task_id = %prev_task_id,
                                        "Cancelled previous running chat task"
                                    );
                                }
                            }

                            active_chat_tasks.insert(chat_id_for_task, task_id.clone());
                            let handle = tokio::spawn(async move {
                                run_message_task(
                                    config,
                                    paths,
                                    provider_pool,
                                    tool_registry,
                                    task_manager,
                                    outbound_tx,
                                    confirm_tx,
                                    memory_store,
                                    capability_registry,
                                    core_evolution,
                                    event_tx,
                                    agent_id,
                                    event_emitter,
                                    msg,
                                    task_id_clone,
                                ).await;
                                let _ = task_done_tx.send((done_task_id, done_chat_id));
                            });
                            active_message_tasks.insert(task_id, handle);
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

                    let _ = self
                        .process_system_event_tick(chrono::Utc::now().timestamp_millis())
                        .await;

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
        abort_active_message_tasks(
            &self.task_manager,
            &mut active_chat_tasks,
            &mut active_message_tasks,
        )
        .await;
        info!("AgentRuntime stopped");
    }
}

/// Free async function that runs a user message in the background.
/// Each message gets its own AgentRuntime so the main loop stays responsive.
async fn run_message_task(
    config: Config,
    paths: Paths,
    provider_pool: Arc<ProviderPool>,
    tool_registry: ToolRegistry,
    task_manager: TaskManager,
    outbound_tx: Option<mpsc::Sender<OutboundMessage>>,
    confirm_tx: Option<mpsc::Sender<ConfirmRequest>>,
    memory_store: Option<MemoryStoreHandle>,
    capability_registry: Option<CapabilityRegistryHandle>,
    core_evolution: Option<CoreEvolutionHandle>,
    event_tx: Option<broadcast::Sender<String>>,
    agent_id: Option<String>,
    event_emitter: EventEmitterHandle,
    msg: InboundMessage,
    task_id: String,
) {
    task_manager.set_running(&task_id).await;

    let mut runtime = match AgentRuntime::new(config, paths, provider_pool, tool_registry) {
        Ok(r) => r,
        Err(e) => {
            task_manager.set_failed(&task_id, &format!("{}", e)).await;
            if let Some(tx) = &outbound_tx {
                let mut outbound =
                    OutboundMessage::new(&msg.channel, &msg.chat_id, &format!("❌ {}", e));
                outbound.account_id = msg.account_id.clone();
                let _ = tx.send(outbound).await;
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
    runtime.set_agent_id(agent_id);
    runtime.set_event_emitter(event_emitter);
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
    provider_pool: Arc<ProviderPool>,
    task_manager: TaskManager,
    outbound_tx: Option<mpsc::Sender<OutboundMessage>>,
    task_str: String,
    task_id: String,
    label: String,
    origin_channel: String,
    origin_chat_id: String,
    agent_id: Option<String>,
    event_emitter: EventEmitterHandle,
) {
    // Create the task entry first, then immediately mark it running.
    // This ensures set_running() never operates on a non-existent task ID.
    task_manager
        .create_task(
            &task_id,
            &label,
            &task_str,
            &origin_channel,
            &origin_chat_id,
            agent_id.as_deref(),
            true,
        )
        .await;
    task_manager.set_running(&task_id).await;
    task_manager.set_progress(&task_id, "Processing...").await;

    // Create isolated runtime with restricted tools
    let tool_registry = AgentRuntime::subagent_tool_registry();
    let mut sub_runtime = match AgentRuntime::new(config, paths, provider_pool, tool_registry) {
        Ok(r) => r,
        Err(e) => {
            task_manager.set_failed(&task_id, &format!("{}", e)).await;
            return;
        }
    };
    sub_runtime.set_task_manager(task_manager.clone());
    sub_runtime.set_agent_id(agent_id.clone());
    sub_runtime.set_event_emitter(event_emitter);

    // Create a unique session key for this subagent
    let session_key = format!("subagent:{}", task_id);

    let mut subagent_metadata = build_subagent_metadata(agent_id.as_deref());
    if !subagent_metadata.is_object() {
        subagent_metadata = serde_json::json!({});
    }
    if let Some(obj) = subagent_metadata.as_object_mut() {
        obj.insert(
            "origin_channel".to_string(),
            serde_json::json!(origin_channel.clone()),
        );
        obj.insert(
            "origin_chat_id".to_string(),
            serde_json::json!(origin_chat_id.clone()),
        );
    }

    // Detect skill script execution prefix from spawn(skill_name=...)
    let result = if task_str.starts_with("__SKILL_EXEC__:") {
        // Parse: __SKILL_EXEC__:<skill_name>:<params_json>:<user_query>
        let rest = &task_str["__SKILL_EXEC__:".len()..];
        let parts: Vec<&str> = rest.splitn(3, ':').collect();
        let skill_name = parts.first().unwrap_or(&"");
        let user_query = parts.get(2).unwrap_or(&"");

        let inbound = InboundMessage {
            channel: origin_channel.clone(),
            account_id: None,
            sender_id: "system".to_string(),
            chat_id: origin_chat_id.clone(),
            content: user_query.to_string(),
            media: vec![],
            metadata: {
                let mut metadata = subagent_metadata.clone();
                if let Some(obj) = metadata.as_object_mut() {
                    obj.insert("skill_script".to_string(), serde_json::json!(true));
                    obj.insert("skill_name".to_string(), serde_json::json!(skill_name));
                    obj.insert(
                        "subagent_session_key".to_string(),
                        serde_json::json!(session_key.clone()),
                    );
                }
                metadata
            },
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };
        // process_message will detect skill_script metadata and use the fast path
        sub_runtime.process_message(inbound).await
    } else {
        let inbound = InboundMessage {
            channel: origin_channel.clone(),
            account_id: None,
            sender_id: "system".to_string(),
            chat_id: origin_chat_id.clone(),
            content: task_str,
            media: vec![],
            metadata: {
                let mut metadata = subagent_metadata.clone();
                if let Some(obj) = metadata.as_object_mut() {
                    obj.insert(
                        "subagent_session_key".to_string(),
                        serde_json::json!(session_key.clone()),
                    );
                }
                metadata
            },
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };
        sub_runtime.process_message(inbound).await
    };

    match result {
        Ok(result) => {
            task_manager.set_completed(&task_id, &result).await;
            info!(task_id = %task_id, label = %label, "Subagent completed");

            // Send the sub-agent's result directly to the origin channel.
            if let Some(tx) = &outbound_tx {
                let notification = OutboundMessage::new(&origin_channel, &origin_chat_id, &result);
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
                        label, short_id, err_msg
                    ),
                );
                let _ = tx.send(notification).await;
            }
        }
    }
}

/// Build outbound metadata containing reply-to information from an inbound message.
/// Only applies to group chats — single/DM chats return Null so no quoting is added.
fn extract_reply_metadata(msg: &InboundMessage) -> serde_json::Value {
    match msg.channel.as_str() {
        "telegram" => {
            // Telegram group/supergroup chat_ids are negative integers
            let is_group = msg.chat_id.parse::<i64>().unwrap_or(0) < 0;
            if is_group {
                if let Some(mid) = msg.metadata.get("message_id") {
                    return serde_json::json!({ "reply_to_message_id": mid });
                }
            }
            serde_json::Value::Null
        }
        "feishu" | "lark" => {
            // Use chat_type from metadata: "group" = group chat, "p2p" = direct message
            let is_group = msg.metadata.get("chat_type").and_then(|v| v.as_str()) == Some("group");
            if is_group {
                if let Some(mid) = msg.metadata.get("message_id").and_then(|v| v.as_str()) {
                    return serde_json::json!({ "reply_to_message_id": mid });
                }
            }
            serde_json::Value::Null
        }
        "discord" => {
            // Discord server messages carry a non-empty guild_id; DMs do not
            let in_guild = msg
                .metadata
                .get("guild_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .is_some();
            if in_guild {
                if let Some(mid) = msg.metadata.get("message_id").and_then(|v| v.as_str()) {
                    return serde_json::json!({ "reply_to_message_id": mid });
                }
            }
            serde_json::Value::Null
        }
        "slack" => {
            // Slack DM channel IDs start with 'D'; public/private channels start with 'C'/'G'
            let is_dm = msg.chat_id.starts_with('D');
            if !is_dm {
                if let Some(ts) = msg.metadata.get("ts").and_then(|v| v.as_str()) {
                    return serde_json::json!({ "thread_ts": ts });
                }
            }
            serde_json::Value::Null
        }
        "dingtalk" => {
            // DingTalk group chats have conversation_type "2"
            let is_group = msg
                .metadata
                .get("conversation_type")
                .and_then(|v| v.as_str())
                == Some("2");
            if is_group {
                if let Some(mid) = msg.metadata.get("msg_id").and_then(|v| v.as_str()) {
                    return serde_json::json!({ "reply_to_message_id": mid });
                }
            }
            serde_json::Value::Null
        }
        _ => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_core_tools_contains_toggle_manage() {
        assert!(global_core_tool_names()
            .iter()
            .any(|name| name == "toggle_manage"));
    }

    #[test]
    fn test_path_within_base_allows_normal_child_path() {
        let base = PathBuf::from("/tmp/workspace");
        let candidate = base.join("skills/new/SKILL.py");
        assert!(is_path_within_base(&base, &candidate));
    }

    #[test]
    fn test_path_within_base_blocks_nonexistent_traversal() {
        let base = PathBuf::from("/tmp/workspace");
        let candidate = base.join("../../etc/passwd");
        assert!(!is_path_within_base(&base, &candidate));
    }

    #[test]
    fn test_tool_result_indicates_error_for_json_error_field() {
        let result = r#"{"error":"Permission denied: blocked"}"#;
        assert!(tool_result_indicates_error(result));
    }

    #[test]
    fn test_tool_result_indicates_error_does_not_use_failed_substring() {
        let result = "Task succeeded, previous attempt failed but recovered.";
        assert!(!tool_result_indicates_error(result));
    }

    #[test]
    fn test_should_supplement_tool_schema_for_validation_error() {
        let result = "Error: Validation error: Missing required parameter: path";
        assert!(should_supplement_tool_schema(result));
    }

    #[test]
    fn test_should_supplement_tool_schema_for_config_error() {
        let result = "Error: Config error: 'enabled' (boolean) is required for 'set' action";
        assert!(should_supplement_tool_schema(result));
    }

    #[test]
    fn test_should_supplement_tool_schema_ignores_permission_denied() {
        let result = "Error: Tool error: Permission denied: path blocked";
        assert!(!should_supplement_tool_schema(result));
    }

    #[test]
    fn test_resolve_routed_agent_id_from_metadata() {
        let metadata = serde_json::json!({
            "route_agent_id": "ops"
        });

        assert_eq!(resolve_routed_agent_id(&metadata).as_deref(), Some("ops"));
        assert_eq!(resolve_routed_agent_id(&serde_json::Value::Null), None);
    }

    #[test]
    fn test_subagent_metadata_preserves_route_agent_id() {
        let metadata = build_subagent_metadata(Some("ops"));

        assert_eq!(
            metadata.get("route_agent_id").and_then(|v| v.as_str()),
            Some("ops")
        );
    }

    #[test]
    fn test_global_core_tool_names_excludes_email() {
        let names = global_core_tool_names();

        assert!(names.iter().any(|name| name == "toggle_manage"));
        assert!(names.iter().any(|name| name == "memory_query"));
        assert!(names.iter().any(|name| name == "list_skills"));
        assert!(!names.iter().any(|name| name == "email"));
        assert!(!names.iter().any(|name| name == "finance_api"));
        assert!(!names.iter().any(|name| name == "read_file"));
    }

    #[test]
    fn test_active_tool_names_for_skill_include_kernel_and_declared_tools() {
        use crate::context::ActiveSkillContext;

        let available: HashSet<String> = [
            "memory_query",
            "memory_upsert",
            "memory_forget",
            "spawn",
            "list_tasks",
            "list_skills",
            "toggle_manage",
            "finance_api",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();

        let skill = ActiveSkillContext {
            name: "stock_analysis".to_string(),
            prompt_md: String::new(),
            tools: vec!["finance_api".to_string()],
            fallback_message: None,
        };

        let tool_names = resolve_effective_tool_names(
            &Config::default(),
            InteractionMode::Skill,
            None,
            Some(&skill),
            &[IntentCategory::Unknown],
            &available,
        );

        assert!(tool_names.contains(&"finance_api".to_string()));
        assert!(tool_names.contains(&"memory_query".to_string()));
        assert!(tool_names.contains(&"toggle_manage".to_string()));
        assert_eq!(
            tool_names
                .iter()
                .filter(|name| name.as_str() == "finance_api")
                .count(),
            1
        );
    }

    #[test]
    fn test_tool_context_supports_optional_event_emitter() {
        use blockcell_core::system_event::{EventPriority, SystemEvent};
        use blockcell_tools::{SystemEventEmitter, ToolContext};
        use std::path::PathBuf;
        use std::sync::Arc;

        struct NoopEmitter;

        impl SystemEventEmitter for NoopEmitter {
            fn emit(&self, _event: SystemEvent) {}

            fn emit_simple(
                &self,
                kind: &str,
                source: &str,
                priority: EventPriority,
                title: &str,
                summary: &str,
            ) {
                let _ = SystemEvent::new_main_session(kind, source, priority, title, summary);
            }
        }

        let ctx = ToolContext {
            workspace: PathBuf::from("/tmp/workspace"),
            builtin_skills_dir: None,
            session_key: "cli:test".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            chat_id: "chat-1".to_string(),
            config: Config::default(),
            permissions: blockcell_core::types::PermissionSet::new(),
            task_manager: None,
            memory_store: None,
            outbound_tx: None,
            spawn_handle: None,
            capability_registry: None,
            core_evolution: None,
            event_emitter: Some(Arc::new(NoopEmitter)),
            channel_contacts_file: None,
        };

        assert!(ctx.event_emitter.is_some());
    }

    fn test_runtime() -> AgentRuntime {
        let mut config = Config::default();
        config.agents.defaults.model = "ollama/llama3".to_string();
        config.agents.defaults.provider = Some("ollama".to_string());

        let base = std::env::temp_dir().join(format!(
            "blockcell-system-event-runtime-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&base).expect("create temp runtime dir");
        let paths = Paths::with_base(base);
        let provider_pool =
            blockcell_providers::ProviderPool::from_config(&config).expect("build provider pool");

        let mut runtime = AgentRuntime::new(
            config,
            paths,
            provider_pool,
            blockcell_tools::ToolRegistry::new(),
        )
        .expect("create runtime");
        runtime.set_agent_id(Some("default".to_string()));
        runtime
    }

    fn test_main_session_inbound(channel: &str, chat_id: &str) -> InboundMessage {
        InboundMessage {
            channel: channel.to_string(),
            account_id: None,
            sender_id: "user".to_string(),
            chat_id: chat_id.to_string(),
            content: "hello".to_string(),
            media: vec![],
            metadata: serde_json::Value::Null,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        }
    }

    #[tokio::test]
    async fn test_orchestrator_tick_emits_event_tx_for_immediate_notifications() {
        let mut runtime = test_runtime();
        let (event_tx, mut event_rx) = broadcast::channel(8);
        runtime.set_event_tx(event_tx);
        runtime.update_main_session_target(&test_main_session_inbound("cli", "chat-1"));

        let mut event = SystemEvent::new_main_session(
            "task.failed",
            "task_manager",
            EventPriority::Critical,
            "Task failed",
            "Background report failed",
        );
        event.delivery.immediate = true;
        runtime.event_emitter_handle().emit(event);

        let decision = runtime
            .process_system_event_tick(chrono::Utc::now().timestamp_millis())
            .await;

        assert_eq!(decision.immediate_notifications.len(), 1);
        let payload = event_rx.recv().await.expect("receive ws event");
        let json: serde_json::Value = serde_json::from_str(&payload).expect("parse ws event");
        assert_eq!(json["type"], "system_event_notification");
        assert_eq!(json["chat_id"], "chat-1");
        assert_eq!(json["title"], "Task failed");
    }

    #[tokio::test]
    async fn test_orchestrator_tick_flushes_summary_to_main_session_outbound() {
        let mut runtime = test_runtime();
        let (outbound_tx, mut outbound_rx) = mpsc::channel(8);
        runtime.set_outbound(outbound_tx);
        runtime.update_main_session_target(&test_main_session_inbound("cli", "chat-1"));

        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut event = SystemEvent::new_main_session(
            "task.completed",
            "task_manager",
            EventPriority::Normal,
            "Report ready",
            "Background report finished",
        );
        event.created_at_ms = now_ms - 60_000;
        runtime.event_emitter_handle().emit(event);

        let decision = runtime.process_system_event_tick(now_ms).await;

        assert_eq!(decision.flushed_summaries.len(), 1);
        let outbound = outbound_rx.recv().await.expect("receive outbound summary");
        assert_eq!(outbound.channel, "cli");
        assert_eq!(outbound.chat_id, "chat-1");
        assert!(outbound.content.contains("Report ready"));
        assert!(outbound.content.contains("System updates") || outbound.content.contains("🗂️"));
    }

    #[tokio::test]
    async fn test_cron_agent_delivery_emits_ws_event_for_deliver_target() {
        let mut runtime = test_runtime();
        let (event_tx, mut event_rx) = broadcast::channel(8);
        runtime.set_event_tx(event_tx);

        let msg = InboundMessage {
            channel: "cron".to_string(),
            account_id: None,
            sender_id: "cron".to_string(),
            chat_id: "job-123".to_string(),
            content: "任务完成摘要".to_string(),
            media: vec![],
            metadata: serde_json::json!({
                "deliver": true,
                "deliver_channel": "ws",
                "deliver_to": "webui-chat-1",
                "cron_agent": true,
            }),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };

        let result = runtime.process_message(msg).await.expect("process cron message");
        assert!(!result.is_empty());

        let payload = event_rx.recv().await.expect("receive ws event");
        let json: serde_json::Value = serde_json::from_str(&payload).expect("parse ws event");
        assert_eq!(json["type"], "message_done");
        assert_eq!(json["chat_id"], "webui-chat-1");
        assert_eq!(json["content"], result);
        assert_eq!(json["background_delivery"], true);
        assert_eq!(json["delivery_kind"], "cron");
        assert_eq!(json["cron_kind"], "agent");
    }

    #[tokio::test]
    async fn test_cron_agent_persists_to_deliver_session_not_cron_job_session() {
        let mut runtime = test_runtime();

        let msg = InboundMessage {
            channel: "cron".to_string(),
            account_id: None,
            sender_id: "cron".to_string(),
            chat_id: "job-456".to_string(),
            content: "搜索美伊战争最新消息，并将结果发给用户。".to_string(),
            media: vec![],
            metadata: serde_json::json!({
                "deliver": true,
                "deliver_channel": "ws",
                "deliver_to": "webui-chat-2",
                "cron_agent": true,
            }),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };

        let result = runtime.process_message(msg).await.expect("process cron message");
        assert!(!result.is_empty());

        let ws_session_key = blockcell_core::build_session_key("ws", "webui-chat-2");
        let cron_session_key = blockcell_core::build_session_key("cron", "job-456");

        let ws_history = runtime
            .session_store
            .load(&ws_session_key)
            .expect("load ws session history");
        assert!(!ws_history.is_empty());
        assert!(ws_history.iter().any(|m| match &m.content {
            serde_json::Value::String(s) => s.contains("搜索美伊战争最新消息"),
            _ => false,
        }));

        let cron_path = runtime.paths.session_file(&cron_session_key);
        assert!(!cron_path.exists(), "cron job session file should not be created");
    }

    #[tokio::test]
    async fn test_orchestrator_tick_gracefully_handles_missing_dispatchers() {
        let runtime = test_runtime();

        let event = SystemEvent::new_main_session(
            "task.failed",
            "task_manager",
            EventPriority::Critical,
            "Task failed",
            "No dispatcher configured",
        );
        runtime.event_emitter_handle().emit(event);

        let decision = runtime
            .process_system_event_tick(chrono::Utc::now().timestamp_millis())
            .await;

        assert_eq!(decision.immediate_notifications.len(), 1);
    }

    #[test]
    fn test_resolve_profile_tool_names_uses_agent_profile_for_unknown_intent() {
        let raw = r#"{
  "agents": {
    "list": [
      { "id": "ops", "enabled": true, "intentProfile": "ops" }
    ]
  },
  "intentRouter": {
    "enabled": true,
    "defaultProfile": "default",
    "profiles": {
      "default": {
        "coreTools": ["read_file"],
        "intentTools": {
          "Chat": { "inheritBase": false, "tools": [] },
          "Unknown": ["browse"]
        }
      },
      "ops": {
        "coreTools": ["read_file", "exec", "file_ops"],
        "intentTools": {
          "Chat": { "inheritBase": false, "tools": [] },
          "Unknown": ["browse", "http_request"],
          "DevOps": ["git_api", "network_monitor"]
        }
      }
    }
  }
}"#;
        let config: Config = serde_json::from_str(raw).unwrap();
        let available: HashSet<String> = [
            "read_file",
            "exec",
            "file_ops",
            "browse",
            "http_request",
            "git_api",
            "network_monitor",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();

        let tool_names = resolve_profile_tool_names(
            &config,
            Some("ops"),
            &[IntentCategory::Unknown],
            &available,
        );

        assert!(tool_names.contains(&"read_file".to_string()));
        assert!(tool_names.contains(&"exec".to_string()));
        assert!(tool_names.contains(&"file_ops".to_string()));
        assert!(tool_names.contains(&"browse".to_string()));
        assert!(tool_names.contains(&"http_request".to_string()));
        assert!(!tool_names.contains(&"git_api".to_string()));
    }

    #[test]
    fn test_resolve_profile_tool_names_returns_empty_for_chat_when_profile_configures_none() {
        let config: Config = serde_json::from_str("{}").unwrap();
        let available: HashSet<String> = ["read_file", "browse"]
            .into_iter()
            .map(str::to_string)
            .collect();

        let tool_names =
            resolve_profile_tool_names(&config, None, &[IntentCategory::Chat], &available);

        assert!(tool_names.is_empty());
    }
    #[test]
    fn test_is_sensitive_filename_matches_json5_config() {
        assert!(is_sensitive_filename("config.json5"));
        assert!(is_sensitive_filename("/tmp/.blockcell/config.json5"));
    }
}
