use std::sync::Arc;

use blockcell_core::types::{ChatMessage, PermissionSet, ToolCallRequest};
use blockcell_core::{Config, Error, Paths, Result};
use blockcell_providers::{CallResult, ProviderPool};
use blockcell_storage::ghost_ledger::NewGhostReviewRun;
use blockcell_storage::GhostLedger;
use serde::Serialize;
use tracing::{info, warn};

use crate::memory_file_store::MemoryFileStore;
use crate::skill_file_store::SkillFileStore;
use blockcell_tools::memory::MemoryManageTool;
use blockcell_tools::session_search::SessionSearchTool;
use blockcell_tools::skills::SkillViewTool;
use blockcell_tools::{SessionSearchOps, ToolContext, ToolRegistry};

use crate::ghost_learning::GhostEpisodeSnapshot;

const GHOST_BACKGROUND_REVIEWER: &str = "embedded_ghost_background_review_v1";
const REVIEW_TOOL_LOOP_MAX_ROUNDS: usize = 8;
const REVIEW_ALLOWED_TOOLS: &[&str] = &["memory_manage", "session_search", "skill_view"];

#[derive(Debug, Clone, PartialEq)]
pub struct GhostBackgroundReviewOutcome {
    pub run_id: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhostReviewToolAction {
    pub tool: String,
    pub success: bool,
    pub result: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
struct GhostReviewToolLoopOutcome {
    actions: Vec<GhostReviewToolAction>,
    stopped: bool,
    rounds_used: usize,
    stop_reason: String,
}

pub async fn run_background_review_for_episode(
    paths: &Paths,
    provider_pool: Arc<ProviderPool>,
    episode_id: &str,
    config: &Config,
    ledger: &GhostLedger,
) -> Result<GhostBackgroundReviewOutcome> {
    let Some(episode) = ledger.get_episode(episode_id)? else {
        return Err(Error::NotFound(format!(
            "Ghost episode not found for background review: {}",
            episode_id
        )));
    };
    let metrics = crate::ghost_metrics::get_ghost_metrics(paths);
    metrics.record_review_started();
    let snapshot: GhostEpisodeSnapshot = serde_json::from_value(episode.metadata.clone())?;

    let Some((provider_idx, provider)) = provider_pool.acquire() else {
        metrics.record_review_failed();
        return record_failed_review_run(
            ledger,
            episode_id,
            "No provider available for ghost background review",
            None,
            None,
        );
    };

    match run_restricted_review_tool_loop(paths, provider.as_ref(), &snapshot, config).await {
        Ok(loop_outcome) if loop_outcome.stopped && all_tool_actions_succeeded(&loop_outcome) => {
            provider_pool.report(provider_idx, CallResult::Success);
            let learning_feedback = summarize_learning_feedback(&loop_outcome.actions);
            let run_id = ledger.insert_review_run(NewGhostReviewRun {
                episode_id: episode_id.to_string(),
                reviewer: GHOST_BACKGROUND_REVIEWER.to_string(),
                status: "completed".to_string(),
                result: serde_json::json!({
                    "mode": "restricted_tool_loop",
                    "maxRounds": REVIEW_TOOL_LOOP_MAX_ROUNDS,
                    "roundsUsed": loop_outcome.rounds_used,
                    "stopReason": loop_outcome.stop_reason,
                    "actionCount": loop_outcome.actions.len(),
                    "learningFeedback": learning_feedback,
                    "actions": loop_outcome.actions,
                }),
            })?;
            ledger.update_episode_status(episode_id, "reviewed")?;
            Ok(GhostBackgroundReviewOutcome {
                run_id,
                status: "completed".to_string(),
            })
        }
        Ok(loop_outcome) => {
            provider_pool.report(provider_idx, CallResult::Success);
            metrics.record_review_failed();
            let error = if loop_outcome.stopped {
                "Restricted ghost review tool loop had failed tool actions"
            } else {
                "Restricted ghost review tool loop exceeded max rounds"
            };
            record_failed_review_run(
                ledger,
                episode_id,
                error,
                None,
                Some(serde_json::json!({
                    "mode": "restricted_tool_loop",
                    "maxRounds": REVIEW_TOOL_LOOP_MAX_ROUNDS,
                    "roundsUsed": loop_outcome.rounds_used,
                    "stopReason": loop_outcome.stop_reason,
                    "actionCount": loop_outcome.actions.len(),
                    "actions": loop_outcome.actions,
                })),
            )
        }
        Err(err) => {
            warn!(
                episode_id = %episode_id,
                error = %err,
                "Restricted ghost review tool loop failed"
            );
            provider_pool.report(provider_idx, ProviderPool::classify_error(&err.to_string()));
            metrics.record_review_failed();
            record_failed_review_run(ledger, episode_id, &err.to_string(), None, None)
        }
    }
}

pub fn spawn_background_review_for_episode(
    paths: Paths,
    provider_pool: Arc<ProviderPool>,
    episode_id: String,
    config: Config,
) {
    tokio::spawn(async move {
        let ledger = match GhostLedger::open(&paths.ghost_ledger_db()) {
            Ok(l) => l,
            Err(err) => {
                warn!(
                    episode_id = %episode_id,
                    error = %err,
                    "Failed to open GhostLedger for background review"
                );
                return;
            }
        };
        match run_background_review_for_episode(
            &paths,
            provider_pool,
            &episode_id,
            &config,
            &ledger,
        )
        .await
        {
            Ok(outcome) => {
                info!(
                    episode_id = %episode_id,
                    review_run_id = %outcome.run_id,
                    status = %outcome.status,
                    "Ghost background review completed"
                );
            }
            Err(err) => {
                warn!(
                    episode_id = %episode_id,
                    error = %err,
                    "Ghost background review failed before recording a review run"
                );
            }
        }
    });
}

pub async fn run_pending_background_reviews(
    paths: &Paths,
    provider_pool: Arc<ProviderPool>,
    limit: usize,
    config: &Config,
) -> Result<Vec<GhostBackgroundReviewOutcome>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let ledger = GhostLedger::open(&paths.ghost_ledger_db())?;
    let episodes = ledger.claim_reviewable_episodes(limit)?;
    let mut outcomes = Vec::with_capacity(episodes.len());
    for episode in episodes {
        match run_background_review_for_episode(
            paths,
            Arc::clone(&provider_pool),
            &episode.id,
            config,
            &ledger,
        )
        .await
        {
            Ok(outcome) => outcomes.push(outcome),
            Err(err) => {
                tracing::warn!(
                    episode_id = %episode.id,
                    error = %err,
                    "Background review for episode failed, continuing with remaining episodes"
                );
            }
        }
    }
    Ok(outcomes)
}

pub fn spawn_pending_background_reviews(
    paths: Paths,
    provider_pool: Arc<ProviderPool>,
    limit: usize,
    config: Config,
) {
    if limit == 0 {
        return;
    }

    tokio::spawn(async move {
        match run_pending_background_reviews(&paths, provider_pool, limit, &config).await {
            Ok(outcomes) if !outcomes.is_empty() => {
                info!(
                    reviewed_count = outcomes.len(),
                    "Ghost pending background reviews completed"
                );
            }
            Ok(_) => {}
            Err(err) => {
                warn!(
                    error = %err,
                    "Ghost pending background reviews failed"
                );
            }
        }
    });
}

async fn run_restricted_review_tool_loop(
    paths: &Paths,
    provider: &dyn blockcell_providers::Provider,
    snapshot: &GhostEpisodeSnapshot,
    config: &Config,
) -> Result<GhostReviewToolLoopOutcome> {
    let registry = restricted_review_tool_registry();
    let tools = registry.get_filtered_schemas(REVIEW_ALLOWED_TOOLS);
    let mut messages = build_restricted_review_messages(snapshot);
    let mut actions = Vec::new();
    let mut stopped = false;
    let mut rounds_used = 0usize;
    let mut stop_reason = "max_rounds".to_string();

    // Create stores once before the loop — avoids re-opening per tool call
    let memory_file_store: blockcell_tools::MemoryFileStoreHandle =
        Arc::new(MemoryFileStore::open(paths)?);
    let skill_file_store: blockcell_tools::SkillFileStoreHandle =
        Arc::new(SkillFileStore::open(paths)?);

    for _round in 0..REVIEW_TOOL_LOOP_MAX_ROUNDS {
        rounds_used += 1;
        let response = provider.chat(&messages, &tools).await?;
        if response.tool_calls.is_empty() {
            stopped = true;
            stop_reason = "model_stopped".to_string();
            break;
        }

        let mut assistant = ChatMessage::assistant(response.content.as_deref().unwrap_or(""));
        assistant.tool_calls = Some(response.tool_calls.clone());
        messages.push(assistant);

        for call in response.tool_calls {
            if !REVIEW_ALLOWED_TOOLS.contains(&call.name.as_str()) {
                let result = serde_json::json!({
                    "error": format!("tool '{}' is not allowed in ghost review", call.name),
                });
                messages.push(tool_result_message(&call, &result));
                actions.push(GhostReviewToolAction {
                    tool: call.name,
                    success: false,
                    result,
                });
                continue;
            }
            let result = registry
                .execute(
                    &call.name,
                    review_tool_context_with_stores(
                        paths,
                        snapshot,
                        config,
                        &memory_file_store,
                        &skill_file_store,
                    )?,
                    call.arguments.clone(),
                )
                .await;
            let (success, result_json) = match result {
                Ok(value) => (true, value),
                Err(err) => (false, serde_json::json!({"error": err.to_string()})),
            };
            messages.push(tool_result_message(&call, &result_json));
            actions.push(GhostReviewToolAction {
                tool: call.name,
                success,
                result: result_json,
            });
        }
    }

    Ok(GhostReviewToolLoopOutcome {
        actions,
        stopped,
        rounds_used,
        stop_reason,
    })
}

fn all_tool_actions_succeeded(loop_outcome: &GhostReviewToolLoopOutcome) -> bool {
    loop_outcome.actions.iter().all(|action| action.success)
}

fn summarize_learning_feedback(actions: &[GhostReviewToolAction]) -> Vec<String> {
    let mut feedback = Vec::new();
    for action in actions.iter().filter(|action| action.success) {
        let Some(summary) = summarize_learning_action(action) else {
            continue;
        };
        if !feedback.iter().any(|existing| existing == &summary) {
            feedback.push(summary);
        }
    }
    feedback
}

fn summarize_learning_action(action: &GhostReviewToolAction) -> Option<String> {
    match action.tool.as_str() {
        "memory_manage" => match action.result.get("target").and_then(|value| value.as_str()) {
            Some("user") => Some("User profile updated".to_string()),
            Some("memory") => Some("Memory updated".to_string()),
            _ => Some("Memory updated".to_string()),
        },
        "skill_manage" => {
            // skill_manage is no longer allowed in ghost review;
            // this arm is kept for backward compat with any stale tool results
            let skill_name = action
                .result
                .get("skillName")
                .and_then(|value| value.as_str())?;
            let verb = match action.result.get("action").and_then(|value| value.as_str()) {
                Some("create") => "created",
                Some("patch") => "patched",
                Some("edit") => "edited",
                Some("delete") => "deleted",
                Some("write_file") => "file updated",
                Some("remove_file") => "file removed",
                Some("undo_latest") => "restored",
                _ => "updated",
            };
            Some(format!("Skill '{}' {}", skill_name, verb))
        }
        _ => None,
    }
}

fn restricted_review_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(MemoryManageTool));
    registry.register(Arc::new(SessionSearchTool));
    registry.register(Arc::new(SkillViewTool));
    // NOTE: skill_manage is intentionally excluded from ghost review.
    // Ghost review only writes declarative knowledge (USER.md/MEMORY.md).
    // Skill creation/modification is handled by SkillLearningChannel.
    registry
}

fn build_restricted_review_messages(snapshot: &GhostEpisodeSnapshot) -> Vec<ChatMessage> {
    vec![
        ChatMessage::system(
            "You are a quiet Ghost learning reviewer. Learn only durable user preferences, stable project facts, and reusable non-procedural memory. Use only the provided tools to update final memory files directly. Do not create, edit, or request skills. If no durable memory is useful, make no tool calls.",
        ),
        ChatMessage::user(
            &serde_json::json!({
                "task": "Review this learning episode and directly update durable memory when useful. Do not create or modify skills.",
                "episode": snapshot,
                "allowedTools": REVIEW_ALLOWED_TOOLS,
            })
            .to_string(),
        ),
    ]
}

fn review_tool_context_with_stores(
    paths: &Paths,
    snapshot: &GhostEpisodeSnapshot,
    config: &Config,
    memory_file_store: &blockcell_tools::MemoryFileStoreHandle,
    skill_file_store: &blockcell_tools::SkillFileStoreHandle,
) -> Result<ToolContext> {
    Ok(ToolContext {
        workspace: paths.workspace(),
        builtin_skills_dir: Some(paths.builtin_skills_dir()),
        active_skill_dir: None,
        session_key: "ghost_background_review".to_string(),
        channel: "ghost".to_string(),
        account_id: None,
        sender_id: None,
        chat_id: "ghost_background_review".to_string(),
        config: config.clone(),
        permissions: PermissionSet::new(),
        task_manager: None,
        memory_store: None,
        memory_file_store: Some(Arc::clone(memory_file_store)),
        ghost_memory_lifecycle: None,
        skill_file_store: Some(Arc::clone(skill_file_store)),
        session_search: Some(Arc::new(EpisodeSessionSearch::new(snapshot)?)),
        outbound_tx: None,
        spawn_handle: None,
        capability_registry: None,
        core_evolution: None,
        event_emitter: None,
        channel_contacts_file: Some(paths.channel_contacts_file()),
        response_cache: None,
        skill_mutex: None,
        agent_type_registry: None,
        runtime_handle: None,
        agent_identity: None,
    })
}

fn tool_result_message(call: &ToolCallRequest, result: &serde_json::Value) -> ChatMessage {
    let mut message = ChatMessage::tool_result(&call.id, &result.to_string());
    message.name = Some(call.name.clone());
    message
}

struct EpisodeSessionSearch {
    chunks: Vec<String>,
}

impl EpisodeSessionSearch {
    fn new(snapshot: &GhostEpisodeSnapshot) -> Result<Self> {
        let value = serde_json::to_value(snapshot)?;
        let mut chunks = Vec::new();
        collect_search_chunks(&value, &mut chunks);
        Ok(Self { chunks })
    }
}

impl SessionSearchOps for EpisodeSessionSearch {
    fn search_session_json(&self, query: &str, limit: usize) -> Result<serde_json::Value> {
        let tokens = normalize_search_tokens(query);
        let mut results = self
            .chunks
            .iter()
            .filter_map(|chunk| {
                let score = search_score(chunk, &tokens);
                (score > 0).then(|| (score, chunk.clone()))
            })
            .collect::<Vec<_>>();
        results.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
        results.truncate(limit.clamp(1, 20));
        Ok(serde_json::json!({
            "query": query,
            "count": results.len(),
            "results": results
                .into_iter()
                .map(|(score, text)| serde_json::json!({
                    "score": score,
                    "text": truncate_chars(&text, 500),
                }))
                .collect::<Vec<_>>()
        }))
    }
}

fn collect_search_chunks(value: &serde_json::Value, chunks: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                chunks.push(trimmed.to_string());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_search_chunks(item, chunks);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values() {
                collect_search_chunks(value, chunks);
            }
        }
        _ => {}
    }
}

fn normalize_search_tokens(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric())
        .map(|token| token.trim().to_lowercase())
        .filter(|token| token.len() >= 2)
        .collect()
}

fn search_score(chunk: &str, tokens: &[String]) -> usize {
    let lower = chunk.to_lowercase();
    tokens
        .iter()
        .map(|token| {
            if lower.contains(token) {
                token.len()
            } else {
                0
            }
        })
        .sum()
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((idx, _)) => format!("{}...", &text[..idx]),
        None => text.to_string(),
    }
}

fn record_failed_review_run(
    ledger: &GhostLedger,
    episode_id: &str,
    error_message: &str,
    raw: Option<&str>,
    details: Option<serde_json::Value>,
) -> Result<GhostBackgroundReviewOutcome> {
    let run_id = ledger.insert_review_run(NewGhostReviewRun {
        episode_id: episode_id.to_string(),
        reviewer: GHOST_BACKGROUND_REVIEWER.to_string(),
        status: "failed".to_string(),
        result: serde_json::json!({
            "error": error_message,
            "raw": raw,
            "details": details,
        }),
    })?;
    ledger.update_episode_status(episode_id, "review_failed")?;
    Ok(GhostBackgroundReviewOutcome {
        run_id,
        status: "failed".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };
    use std::time::Duration;

    use blockcell_core::types::{ChatMessage, LLMResponse};
    use blockcell_core::{Config, InboundMessage, Paths};
    use blockcell_providers::{Provider, ProviderPool};
    use blockcell_storage::ghost_ledger::{GhostEpisodeSource, NewGhostEpisode};
    use blockcell_storage::GhostLedger;
    use blockcell_tools::ToolRegistry;

    use crate::runtime::AgentRuntime;

    #[derive(Debug, Clone, Copy)]
    enum ToolLoopMode {
        WriteFiles,
        SearchThenWrite,
        Noop,
        Fail,
        InvalidToolArgs,
        DisallowedTool,
        NeverStop,
        StopOnEighthRound,
    }

    struct ToolLoopReviewProvider {
        seen_tools: Mutex<Vec<Vec<String>>>,
        review_calls: AtomicUsize,
        mode: ToolLoopMode,
    }

    impl ToolLoopReviewProvider {
        fn new(mode: ToolLoopMode) -> Self {
            Self {
                seen_tools: Mutex::new(Vec::new()),
                review_calls: AtomicUsize::new(0),
                mode,
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for ToolLoopReviewProvider {
        async fn chat(
            &self,
            messages: &[ChatMessage],
            tools: &[serde_json::Value],
        ) -> blockcell_core::Result<LLMResponse> {
            if tools.is_empty() {
                return Ok(LLMResponse {
                    content: Some("mock answer: learn my preferred deploy style".to_string()),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    finish_reason: "stop".to_string(),
                    usage: serde_json::Value::Null,
                });
            }

            let tool_names = tools
                .iter()
                .filter_map(|tool| {
                    tool.get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(|name| name.as_str())
                        .map(str::to_string)
                })
                .collect::<Vec<_>>();
            self.seen_tools.lock().unwrap().push(tool_names);
            let call_idx = self.review_calls.fetch_add(1, Ordering::SeqCst);

            match self.mode {
                ToolLoopMode::Fail => Err(Error::Provider("review tool loop failed".to_string())),
                ToolLoopMode::Noop => Ok(LLMResponse {
                    content: Some("no durable learning".to_string()),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    finish_reason: "stop".to_string(),
                    usage: serde_json::Value::Null,
                }),
                ToolLoopMode::InvalidToolArgs if call_idx == 0 => Ok(LLMResponse {
                    content: None,
                    reasoning_content: None,
                    tool_calls: vec![ToolCallRequest {
                        id: "review-invalid-memory".to_string(),
                        name: "memory_manage".to_string(),
                        arguments: serde_json::json!({
                            "action": "add",
                            "target": "user"
                        }),
                        thought_signature: None,
                    }],
                    finish_reason: "tool_calls".to_string(),
                    usage: serde_json::Value::Null,
                }),
                ToolLoopMode::InvalidToolArgs => Ok(LLMResponse {
                    content: Some("invalid action acknowledged".to_string()),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    finish_reason: "stop".to_string(),
                    usage: serde_json::Value::Null,
                }),
                ToolLoopMode::DisallowedTool if call_idx == 0 => Ok(LLMResponse {
                    content: None,
                    reasoning_content: None,
                    tool_calls: vec![ToolCallRequest {
                        id: "review-disallowed".to_string(),
                        name: "write_file".to_string(),
                        arguments: serde_json::json!({
                            "path": "USER.md",
                            "content": "unsafe"
                        }),
                        thought_signature: None,
                    }],
                    finish_reason: "tool_calls".to_string(),
                    usage: serde_json::Value::Null,
                }),
                ToolLoopMode::DisallowedTool => Ok(LLMResponse {
                    content: Some("disallowed action acknowledged".to_string()),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    finish_reason: "stop".to_string(),
                    usage: serde_json::Value::Null,
                }),
                ToolLoopMode::NeverStop => Ok(LLMResponse {
                    content: None,
                    reasoning_content: None,
                    tool_calls: vec![ToolCallRequest {
                        id: format!("review-loop-{call_idx}"),
                        name: "skill_view".to_string(),
                        arguments: serde_json::json!({
                            "name": "release_verification"
                        }),
                        thought_signature: None,
                    }],
                    finish_reason: "tool_calls".to_string(),
                    usage: serde_json::Value::Null,
                }),
                ToolLoopMode::StopOnEighthRound if call_idx < 7 => Ok(LLMResponse {
                    content: None,
                    reasoning_content: None,
                    tool_calls: vec![ToolCallRequest {
                        id: format!("review-search-{call_idx}"),
                        name: "session_search".to_string(),
                        arguments: serde_json::json!({
                            "query": "rollback order",
                            "limit": 1
                        }),
                        thought_signature: None,
                    }],
                    finish_reason: "tool_calls".to_string(),
                    usage: serde_json::Value::Null,
                }),
                ToolLoopMode::StopOnEighthRound => Ok(LLMResponse {
                    content: Some("review complete".to_string()),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    finish_reason: "stop".to_string(),
                    usage: serde_json::Value::Null,
                }),
                ToolLoopMode::WriteFiles if call_idx == 0 => {
                    let system = match &messages[0].content {
                        serde_json::Value::String(text) => text.clone(),
                        other => other.to_string(),
                    };
                    assert!(system.contains("quiet Ghost learning reviewer"));
                    Ok(LLMResponse {
                        content: None,
                        reasoning_content: None,
                        tool_calls: vec![ToolCallRequest {
                            id: "review-memory".to_string(),
                            name: "memory_manage".to_string(),
                            arguments: serde_json::json!({
                                "action": "add",
                                "target": "user",
                                "content": "User prefers canary-first rollout."
                            }),
                            thought_signature: None,
                        }],
                        finish_reason: "tool_calls".to_string(),
                        usage: serde_json::Value::Null,
                    })
                }
                ToolLoopMode::WriteFiles => Ok(LLMResponse {
                    content: Some("review complete".to_string()),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    finish_reason: "stop".to_string(),
                    usage: serde_json::Value::Null,
                }),
                ToolLoopMode::SearchThenWrite if call_idx == 0 => Ok(LLMResponse {
                    content: None,
                    reasoning_content: None,
                    tool_calls: vec![ToolCallRequest {
                        id: "review-session-search".to_string(),
                        name: "session_search".to_string(),
                        arguments: serde_json::json!({
                            "query": "rollback order",
                            "limit": 3
                        }),
                        thought_signature: None,
                    }],
                    finish_reason: "tool_calls".to_string(),
                    usage: serde_json::Value::Null,
                }),
                ToolLoopMode::SearchThenWrite if call_idx == 1 => Ok(LLMResponse {
                    content: None,
                    reasoning_content: None,
                    tool_calls: vec![ToolCallRequest {
                        id: "review-memory-after-search".to_string(),
                        name: "memory_manage".to_string(),
                        arguments: serde_json::json!({
                            "action": "add",
                            "target": "memory",
                            "content": "Check rollback order before release verification."
                        }),
                        thought_signature: None,
                    }],
                    finish_reason: "tool_calls".to_string(),
                    usage: serde_json::Value::Null,
                }),
                ToolLoopMode::SearchThenWrite => Ok(LLMResponse {
                    content: Some("review complete".to_string()),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    finish_reason: "stop".to_string(),
                    usage: serde_json::Value::Null,
                }),
            }
        }
    }

    fn temp_paths(label: &str) -> Paths {
        let base = std::env::temp_dir().join(format!(
            "blockcell-ghost-background-review-{}-{}",
            label,
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&base).expect("create temp ghost background review dir");
        Paths::with_base(base)
    }

    fn insert_sample_episode(paths: &Paths) -> String {
        let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
        ledger
            .insert_episode(NewGhostEpisode {
                boundary_kind: "turn_end".to_string(),
                subject_key: Some("chat:ghost-review".to_string()),
                status: "pending_review".to_string(),
                summary: "learn deploy preference => prefer canary-first rollout".to_string(),
                metadata: serde_json::json!({
                    "boundaryKind": "turn_end",
                    "subjectKey": "chat:ghost-review",
                    "userIntentSummary": "learn deploy preference",
                    "assistantOutcomeSummary": "prefer canary-first rollout",
                    "toolCallCount": 0,
                    "memoryWriteCount": 0,
                    "correctionCount": 1,
                    "preferenceCorrectionCount": 1,
                    "complexityScore": 5,
                    "reusableLesson": "Prefer canary-first rollout",
                    "decision": "review_after_response"
                }),
                sources: vec![GhostEpisodeSource {
                    source_type: "session".to_string(),
                    source_key: "cli:ghost-review".to_string(),
                    role: "primary".to_string(),
                }],
            })
            .expect("insert ghost episode")
    }

    #[test]
    fn restricted_review_prompt_is_memory_only() {
        let snapshot = GhostEpisodeSnapshot {
            boundary_kind: crate::ghost_learning::GhostLearningBoundaryKind::TurnEnd,
            session_key: Some("cli:ghost-review".to_string()),
            subject_key: Some("chat:ghost-review".to_string()),
            user_intent_summary: "learn deploy preference".to_string(),
            assistant_outcome_summary: "prefer canary-first rollout".to_string(),
            tool_call_count: 0,
            memory_write_count: 0,
            correction_count: 0,
            preference_correction_count: 0,
            complexity_score: 1,
            reusable_lesson: None,
            decision: crate::ghost_learning::LearningDecision::ReviewAfterResponse,
        };

        let messages = build_restricted_review_messages(&snapshot);
        let prompt = messages
            .iter()
            .filter_map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(prompt.contains("durable memory"));
        assert!(prompt.contains("Do not create, edit, or request skills"));
        assert!(!prompt.contains("prompt-only skills"));
        assert!(!REVIEW_ALLOWED_TOOLS.contains(&"skill_manage"));
    }

    fn test_runtime_with_background_review(
        paths: Paths,
        provider: Arc<dyn Provider>,
    ) -> AgentRuntime {
        let mut config = Config::default();
        config.agents.defaults.model = "test/mock".to_string();
        config.agents.defaults.provider = Some("test".to_string());
        config.agents.ghost.learning.enabled = true;
        config.agents.ghost.learning.shadow_mode = true;

        let provider_pool = ProviderPool::from_single_provider("test/mock", "test", provider);
        let mut runtime = AgentRuntime::new(config, paths, provider_pool, ToolRegistry::new())
            .expect("create runtime");
        runtime.set_agent_id(Some("default".to_string()));
        runtime
    }

    async fn wait_for_review_runs(paths: &Paths, expected: usize) {
        for _ in 0..50 {
            let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
            if ledger.review_run_count().expect("count review runs") >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for ghost review runs");
    }

    #[tokio::test]
    async fn background_review_runs_after_turn_response() {
        let paths = temp_paths("after-turn");
        let provider = Arc::new(ToolLoopReviewProvider::new(ToolLoopMode::WriteFiles));
        let mut runtime = test_runtime_with_background_review(paths.clone(), provider);

        runtime
            .process_message(InboundMessage {
                channel: "cli".to_string(),
                account_id: None,
                sender_id: "user".to_string(),
                chat_id: "ghost-review".to_string(),
                content: "learn my preferred deploy style".to_string(),
                media: vec![],
                metadata: serde_json::Value::Null,
                timestamp_ms: chrono::Utc::now().timestamp_millis(),
            })
            .await
            .unwrap();

        wait_for_review_runs(&paths, 1).await;

        let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
        assert_eq!(ledger.review_run_count().unwrap(), 1);
    }

    #[tokio::test]
    async fn background_review_uses_restricted_tool_loop_only() {
        let paths = temp_paths("tool-loop");
        let episode_id = insert_sample_episode(&paths);
        let provider = Arc::new(ToolLoopReviewProvider::new(ToolLoopMode::WriteFiles));
        let provider_pool =
            ProviderPool::from_single_provider("test/mock", "test", provider.clone());

        let outcome = run_background_review_for_episode(
            &paths,
            provider_pool,
            &episode_id,
            &Config::default(),
            &GhostLedger::open(&paths.ghost_ledger_db()).expect("open ledger"),
        )
        .await
        .expect("run background review");

        assert_eq!(outcome.status, "completed");
        let mut first_seen = provider.seen_tools.lock().unwrap()[0].clone();
        first_seen.sort();
        assert_eq!(
            first_seen,
            vec![
                "memory_manage".to_string(),
                "session_search".to_string(),
                "skill_view".to_string(),
            ]
        );

        let user_memory = std::fs::read_to_string(paths.user_md()).expect("read USER.md");
        assert!(user_memory.contains("canary-first rollout"));
        assert!(!paths
            .skills_dir()
            .join("release_verification")
            .join("SKILL.md")
            .exists());

        let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
        assert_eq!(ledger.review_run_count().unwrap(), 1);
        let run = ledger
            .get_review_run(&outcome.run_id)
            .expect("review run query")
            .expect("review run exists");
        assert_eq!(
            run.result["mode"],
            serde_json::json!("restricted_tool_loop")
        );
        assert_eq!(run.result["actionCount"], serde_json::json!(1));
        assert_eq!(
            run.result["learningFeedback"],
            serde_json::json!(["User profile updated"])
        );
    }

    #[tokio::test]
    async fn background_review_noop_does_not_fallback_to_json() {
        let paths = temp_paths("noop");
        let episode_id = insert_sample_episode(&paths);
        let provider = Arc::new(ToolLoopReviewProvider::new(ToolLoopMode::Noop));
        let provider_pool =
            ProviderPool::from_single_provider("test/mock", "test", provider.clone());

        let outcome = run_background_review_for_episode(
            &paths,
            provider_pool,
            &episode_id,
            &Config::default(),
            &GhostLedger::open(&paths.ghost_ledger_db()).expect("open ledger"),
        )
        .await
        .expect("run background review");

        assert_eq!(outcome.status, "completed");
        assert_eq!(provider.review_calls.load(Ordering::SeqCst), 1);
        let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
        let run = ledger
            .get_review_run(&outcome.run_id)
            .expect("review run query")
            .expect("review run exists");
        assert_eq!(
            run.result["mode"],
            serde_json::json!("restricted_tool_loop")
        );
        assert_eq!(run.result["actionCount"], serde_json::json!(0));
        assert_eq!(run.result["learningFeedback"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn background_review_can_search_session_before_writing_memory() {
        let paths = temp_paths("session-search");
        let episode_id = insert_sample_episode(&paths);
        let provider = Arc::new(ToolLoopReviewProvider::new(ToolLoopMode::SearchThenWrite));
        let provider_pool =
            ProviderPool::from_single_provider("test/mock", "test", provider.clone());

        let outcome = run_background_review_for_episode(
            &paths,
            provider_pool,
            &episode_id,
            &Config::default(),
            &GhostLedger::open(&paths.ghost_ledger_db()).expect("open ledger"),
        )
        .await
        .expect("run background review");

        assert_eq!(outcome.status, "completed");
        let durable_memory = std::fs::read_to_string(paths.memory_md()).expect("read MEMORY.md");
        assert!(durable_memory.contains("Check rollback order before release verification"));
        let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
        let run = ledger
            .get_review_run(&outcome.run_id)
            .expect("review run query")
            .expect("review run exists");
        assert_eq!(run.result["actionCount"], serde_json::json!(2));
        assert_eq!(
            run.result["learningFeedback"],
            serde_json::json!(["Memory updated"])
        );
        assert_eq!(
            run.result["actions"][0]["tool"],
            serde_json::json!("session_search")
        );
        assert_eq!(run.result["actions"][0]["success"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn background_review_failure_is_recorded_without_json_fallback() {
        let paths = temp_paths("failure");
        let episode_id = insert_sample_episode(&paths);
        let provider = Arc::new(ToolLoopReviewProvider::new(ToolLoopMode::Fail));
        let provider_pool =
            ProviderPool::from_single_provider("test/mock", "test", provider.clone());

        let outcome = run_background_review_for_episode(
            &paths,
            provider_pool,
            &episode_id,
            &Config::default(),
            &GhostLedger::open(&paths.ghost_ledger_db()).expect("open ledger"),
        )
        .await
        .expect("record failed review");

        assert_eq!(outcome.status, "failed");
        assert_eq!(provider.review_calls.load(Ordering::SeqCst), 1);
        let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
        let episode = ledger
            .get_episode(&episode_id)
            .expect("get episode")
            .expect("episode exists");
        assert_eq!(episode.status, "review_failed");
        let run = ledger
            .get_review_run(&outcome.run_id)
            .expect("review run query")
            .expect("review run exists");
        assert_eq!(run.status, "failed");
    }

    #[tokio::test]
    async fn background_review_failed_tool_action_marks_review_failed() {
        let paths = temp_paths("failed-tool-action");
        let episode_id = insert_sample_episode(&paths);
        let provider = Arc::new(ToolLoopReviewProvider::new(ToolLoopMode::InvalidToolArgs));
        let provider_pool =
            ProviderPool::from_single_provider("test/mock", "test", provider.clone());

        let outcome = run_background_review_for_episode(
            &paths,
            provider_pool,
            &episode_id,
            &Config::default(),
            &GhostLedger::open(&paths.ghost_ledger_db()).expect("open ledger"),
        )
        .await
        .expect("record failed review");

        assert_eq!(outcome.status, "failed");
        let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
        let episode = ledger
            .get_episode(&episode_id)
            .expect("get episode")
            .expect("episode exists");
        assert_eq!(episode.status, "review_failed");
        let run = ledger
            .get_review_run(&outcome.run_id)
            .expect("review run query")
            .expect("review run exists");
        assert_eq!(
            run.result["error"],
            serde_json::json!("Restricted ghost review tool loop had failed tool actions")
        );
        assert_eq!(run.result["details"]["actionCount"], serde_json::json!(1));
        assert!(run.result["details"]["actions"]
            .as_array()
            .expect("actions array")
            .iter()
            .all(|action| action["success"] == serde_json::json!(false)));
    }

    #[tokio::test]
    async fn background_review_disallowed_tool_is_not_executed_and_marks_failed() {
        let paths = temp_paths("disallowed-tool");
        let episode_id = insert_sample_episode(&paths);
        let provider = Arc::new(ToolLoopReviewProvider::new(ToolLoopMode::DisallowedTool));
        let provider_pool =
            ProviderPool::from_single_provider("test/mock", "test", provider.clone());

        let outcome = run_background_review_for_episode(
            &paths,
            provider_pool,
            &episode_id,
            &Config::default(),
            &GhostLedger::open(&paths.ghost_ledger_db()).expect("open ledger"),
        )
        .await
        .expect("record failed review");

        assert_eq!(outcome.status, "failed");
        assert!(!paths.user_md().exists());
        let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
        let run = ledger
            .get_review_run(&outcome.run_id)
            .expect("review run query")
            .expect("review run exists");
        let actions = run.result["details"]["actions"]
            .as_array()
            .expect("actions array");
        assert_eq!(actions.len(), 1);
        assert!(actions.iter().all(|action| action["tool"] == "write_file"));
        assert!(actions.iter().all(|action| action["success"] == false));
    }

    #[tokio::test]
    async fn background_review_max_round_exhaustion_marks_failed() {
        let paths = temp_paths("max-rounds");
        let episode_id = insert_sample_episode(&paths);
        let provider = Arc::new(ToolLoopReviewProvider::new(ToolLoopMode::NeverStop));
        let provider_pool =
            ProviderPool::from_single_provider("test/mock", "test", provider.clone());

        let outcome = run_background_review_for_episode(
            &paths,
            provider_pool,
            &episode_id,
            &Config::default(),
            &GhostLedger::open(&paths.ghost_ledger_db()).expect("open ledger"),
        )
        .await
        .expect("record failed review");

        assert_eq!(outcome.status, "failed");
        assert_eq!(provider.review_calls.load(Ordering::SeqCst), 8);
        let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
        let run = ledger
            .get_review_run(&outcome.run_id)
            .expect("review run query")
            .expect("review run exists");
        assert_eq!(
            run.result["error"],
            serde_json::json!("Restricted ghost review tool loop exceeded max rounds")
        );
        assert_eq!(run.result["details"]["maxRounds"], serde_json::json!(8));
        assert_eq!(run.result["details"]["roundsUsed"], serde_json::json!(8));
        assert_eq!(
            run.result["details"]["stopReason"],
            serde_json::json!("max_rounds")
        );
        assert_eq!(run.result["details"]["actionCount"], serde_json::json!(8));
    }

    #[tokio::test]
    async fn background_review_can_stop_on_eighth_round() {
        let paths = temp_paths("stop-on-eighth");
        let episode_id = insert_sample_episode(&paths);
        let provider = Arc::new(ToolLoopReviewProvider::new(ToolLoopMode::StopOnEighthRound));
        let provider_pool =
            ProviderPool::from_single_provider("test/mock", "test", provider.clone());

        let outcome = run_background_review_for_episode(
            &paths,
            provider_pool,
            &episode_id,
            &Config::default(),
            &GhostLedger::open(&paths.ghost_ledger_db()).expect("open ledger"),
        )
        .await
        .expect("run background review");

        assert_eq!(outcome.status, "completed");
        assert_eq!(provider.review_calls.load(Ordering::SeqCst), 8);
        let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
        let run = ledger
            .get_review_run(&outcome.run_id)
            .expect("review run query")
            .expect("review run exists");
        assert_eq!(run.result["maxRounds"], serde_json::json!(8));
        assert_eq!(run.result["roundsUsed"], serde_json::json!(8));
        assert_eq!(run.result["stopReason"], serde_json::json!("model_stopped"));
        assert_eq!(run.result["actionCount"], serde_json::json!(7));
    }
}
