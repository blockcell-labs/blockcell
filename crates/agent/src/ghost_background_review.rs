use std::sync::Arc;

use blockcell_core::types::{ChatMessage, PermissionSet, ToolCallRequest};
use blockcell_core::{AbortToken, Config, Error, Paths, Result};
use blockcell_providers::{CallResult, ProviderPool};
use blockcell_storage::ghost_ledger::NewGhostReviewRun;
use blockcell_storage::GhostLedger;
use serde::Serialize;
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::memory_file_store::MemoryFileStoreRouter;
use crate::skill_file_store::SkillFileStore;
use blockcell_tools::memory::MemoryManageTool;
use blockcell_tools::session_search::SessionSearchTool;
use blockcell_tools::skills::SkillViewTool;
use blockcell_tools::GhostMemoryLifecycleOps;
use blockcell_tools::{SessionSearchOps, ToolContext, ToolRegistry};

use crate::ghost_learning::GhostEpisodeSnapshot;

const GHOST_BACKGROUND_REVIEWER: &str = "embedded_ghost_background_review_v1";
const REVIEW_TOOL_LOOP_MAX_ROUNDS: usize = 2;
const REVIEW_ALLOWED_TOOLS: &[&str] = &["memory_manage", "session_search", "skill_view"];
const REVIEW_LEASE_DURATION_SECS: i64 = 600;
const REVIEW_HEARTBEAT_INTERVAL_SECS: u64 = 180;

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

/// RAII guard：确保 GhostMemoryProviderManager 在 Drop 时执行 shutdown_all()，
/// 防止函数内部 ? 提前返回时跳过 shutdown 导致 provider 缓冲区/连接泄漏。
struct ShutdownGuard<'a> {
    manager: &'a Arc<crate::ghost_memory_provider::GhostMemoryProviderManager>,
    armed: bool,
}

impl<'a> ShutdownGuard<'a> {
    fn new(manager: &'a Arc<crate::ghost_memory_provider::GhostMemoryProviderManager>) -> Self {
        Self {
            manager,
            armed: true,
        }
    }

    /// 显式解除 shutdown 职责（用于需要手动控制 shutdown 时机的场景）
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ShutdownGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.manager.shutdown_all();
        }
    }
}

pub async fn run_background_review_for_episode(
    paths: &Paths,
    provider_pool: Arc<ProviderPool>,
    episode_id: &str,
    config: &Config,
    ledger: &GhostLedger,
    review_worker_id: Option<&str>,
) -> Result<GhostBackgroundReviewOutcome> {
    run_background_review_for_episode_with_lease_abort(
        paths,
        provider_pool,
        episode_id,
        config,
        ledger,
        review_worker_id,
        None,
    )
    .await
}

async fn run_background_review_for_episode_with_lease_abort(
    paths: &Paths,
    provider_pool: Arc<ProviderPool>,
    episode_id: &str,
    config: &Config,
    ledger: &GhostLedger,
    review_worker_id: Option<&str>,
    lease_abort: Option<AbortToken>,
) -> Result<GhostBackgroundReviewOutcome> {
    let Some(episode) = ledger.get_episode(episode_id)? else {
        return Err(Error::NotFound(format!(
            "Ghost episode not found for background review: {}",
            episode_id
        )));
    };
    let metrics = crate::ghost_metrics::get_ghost_metrics(paths);
    metrics.record_review_started();
    let requested_outputs = episode
        .metadata
        .get("requestedOutputs")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let snapshot: GhostEpisodeSnapshot = match serde_json::from_value(episode.metadata.clone()) {
        Ok(s) => s,
        Err(err) => {
            warn!(
                episode_id = %episode_id,
                error = %err,
                "Failed to deserialize episode metadata for background review"
            );
            metrics.record_review_failed();
            return record_failed_review_run(
                ledger,
                episode_id,
                &format!("Metadata deserialization error: {}", err),
                None,
                None,
                review_worker_id,
            );
        }
    };

    let Some((provider_idx, provider)) = provider_pool.acquire() else {
        metrics.record_review_failed();
        return record_failed_review_run(
            ledger,
            episode_id,
            "No provider available for ghost background review",
            None,
            None,
            review_worker_id,
        );
    };

    let manager = Arc::new(
        crate::ghost_memory_provider::GhostMemoryProviderManager::with_local_file(paths.clone()),
    );
    manager.initialize_all("ghost_background_review", "reviewer");
    let ghost_memory_lifecycle: Option<Arc<dyn GhostMemoryLifecycleOps + Send + Sync>> =
        Some(manager.clone() as Arc<dyn GhostMemoryLifecycleOps + Send + Sync>);

    // RAII guard：无论函数如何退出（包括 ? 提前返回），都会执行 shutdown_all()
    let mut _shutdown_guard = ShutdownGuard::new(&manager);

    let result = match run_restricted_review_tool_loop(
        paths,
        provider.as_ref(),
        &snapshot,
        config,
        ghost_memory_lifecycle,
        lease_abort,
    )
    .await
    {
        Ok(loop_outcome) if loop_outcome.stopped && all_tool_actions_succeeded(&loop_outcome) => {
            provider_pool.report(provider_idx, CallResult::Success);
            let learning_feedback = summarize_learning_feedback(&loop_outcome.actions);
            let run_id = ledger.insert_review_run_if_review_owned(
                NewGhostReviewRun {
                    episode_id: episode_id.to_string(),
                    reviewer: GHOST_BACKGROUND_REVIEWER.to_string(),
                    status: "completed".to_string(),
                    result: serde_json::json!({
                        "mode": "restricted_tool_loop",
                        "maxRounds": REVIEW_TOOL_LOOP_MAX_ROUNDS,
                        "roundsUsed": loop_outcome.rounds_used,
                        "classifierCallCount": usize::from(loop_outcome.rounds_used > 0),
                        "skillDeepeningCallCount": 0,
                        "providerCallCount": loop_outcome.rounds_used,
                        "requestedOutputs": requested_outputs,
                        "output": unified_output_from_actions(&loop_outcome.actions),
                        "stopReason": loop_outcome.stop_reason,
                        "actionCount": loop_outcome.actions.len(),
                        "dispatchedTargets": dispatched_targets(&loop_outcome.actions),
                        "failures": dispatch_failures(&loop_outcome.actions),
                        "learningFeedback": learning_feedback,
                        "actions": loop_outcome.actions,
                    }),
                },
                review_worker_id,
                "reviewed",
            )?;
            match run_id {
                Some(id) => Ok(GhostBackgroundReviewOutcome {
                    run_id: id,
                    status: "completed".to_string(),
                }),
                None => {
                    warn!(episode_id = %episode_id, "Lost review lease before inserting completed review_run");
                    Err(Error::Storage("Lost review lease".into()))
                }
            }
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
                    "classifierCallCount": usize::from(loop_outcome.rounds_used > 0),
                    "skillDeepeningCallCount": 0,
                    "providerCallCount": loop_outcome.rounds_used,
                    "requestedOutputs": requested_outputs,
                    "output": unified_output_from_actions(&loop_outcome.actions),
                    "stopReason": loop_outcome.stop_reason,
                    "actionCount": loop_outcome.actions.len(),
                    "dispatchedTargets": dispatched_targets(&loop_outcome.actions),
                    "failures": dispatch_failures(&loop_outcome.actions),
                    "actions": loop_outcome.actions,
                })),
                review_worker_id,
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
            record_failed_review_run(
                ledger,
                episode_id,
                &err.to_string(),
                None,
                None,
                review_worker_id,
            )
        }
    };

    // 显式 shutdown 并解除 guard 职责，避免 Drop 中重复调用
    manager.shutdown_all();
    _shutdown_guard.disarm();

    result
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
            None,
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
    // 在 claim 前清理过期 lease，释放因 worker 崩溃/超时而残留的 reviewing 状态
    if let Err(e) = ledger.cleanup_expired_leases() {
        warn!(error = %e, "Failed to cleanup expired review leases before claiming");
    }
    let worker_id = format!("ghost-review-{}", uuid::Uuid::new_v4());
    let episodes =
        ledger.claim_reviewable_episodes(limit, &worker_id, REVIEW_LEASE_DURATION_SECS)?;
    let mut outcomes = Vec::with_capacity(episodes.len());
    for episode in episodes {
        // review loop 开始前 heartbeat 延长 lease
        match ledger.heartbeat_review_lease(&episode.id, &worker_id, REVIEW_LEASE_DURATION_SECS) {
            Ok(true) => {}
            Ok(false) => {
                warn!(episode_id = %episode.id, "Lost review lease before processing episode");
                continue;
            }
            Err(e) => {
                warn!(
                    episode_id = %episode.id,
                    error = %e,
                    "Failed to heartbeat review lease before processing episode"
                );
                continue;
            }
        }

        // Spawn periodic heartbeat during review execution
        let lease_abort = AbortToken::new();
        let heartbeat_abort = lease_abort.clone();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let hb_ledger = GhostLedger::open(&paths.ghost_ledger_db())?;
        let hb_episode_id = episode.id.clone();
        let hb_worker_id = worker_id.clone();
        let heartbeat_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                REVIEW_HEARTBEAT_INTERVAL_SECS,
            ));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut consecutive_failures = 0u32;
            const MAX_CONSECUTIVE_FAILURES: u32 = 3;
            let mut stop_rx = stop_rx;

            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = interval.tick() => {
                        match hb_ledger.heartbeat_review_lease(&hb_episode_id, &hb_worker_id, REVIEW_LEASE_DURATION_SECS) {
                            Ok(true) => consecutive_failures = 0,
                            Ok(false) => {
                                warn!(episode_id = %hb_episode_id, worker_id = %hb_worker_id, "Lost review lease during heartbeat");
                                heartbeat_abort.cancel();
                                break;
                            }
                            Err(e) => {
                                consecutive_failures += 1;
                                warn!(episode_id = %hb_episode_id, worker_id = %hb_worker_id, error = %e, consecutive_failures, "Failed to heartbeat review lease (will retry)");
                                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                                    heartbeat_abort.cancel();
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        });

        let review_result = run_background_review_for_episode_with_lease_abort(
            paths,
            Arc::clone(&provider_pool),
            &episode.id,
            config,
            &ledger,
            Some(&worker_id),
            Some(lease_abort),
        )
        .await;

        // Stop heartbeat
        let _ = stop_tx.send(());
        heartbeat_handle.abort();

        match review_result {
            Ok(outcome) => {
                let _ = ledger.release_review_lease(&episode.id, &worker_id);
                outcomes.push(outcome)
            }
            Err(err) => {
                warn!(
                    episode_id = %episode.id,
                    error = %err,
                    "Background review failed for episode, marking as review_failed and continuing"
                );
                // Use owner-aware status update to avoid overwriting another worker's episode
                match ledger.update_episode_status_if_review_owned(
                    &episode.id,
                    &worker_id,
                    "review_failed",
                ) {
                    Ok(true) => {}
                    Ok(false) => {
                        warn!(episode_id = %episode.id, "Lost review lease before marking as review_failed, skipping");
                    }
                    Err(e) => {
                        warn!(
                            episode_id = %episode.id,
                            error = %e,
                            "Failed to mark episode as review_failed after review error"
                        );
                    }
                }
                let _ = ledger.release_review_lease(&episode.id, &worker_id);
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
    ghost_memory_lifecycle: Option<Arc<dyn GhostMemoryLifecycleOps + Send + Sync>>,
    lease_abort: Option<AbortToken>,
) -> Result<GhostReviewToolLoopOutcome> {
    let registry = restricted_review_tool_registry();
    let tools = registry.get_filtered_schemas(REVIEW_ALLOWED_TOOLS);
    let mut messages = build_restricted_review_messages(snapshot);
    let mut actions = Vec::new();
    let mut stopped = false;
    let mut rounds_used = 0usize;
    let mut stop_reason = "max_rounds".to_string();

    // Create stores once before the loop — avoids re-opening per tool call
    let learning = &config.agents.ghost.learning;
    let memory_file_store: blockcell_tools::MemoryFileStoreHandle =
        Arc::new(MemoryFileStoreRouter::open(
            paths,
            snapshot.session_key.as_deref(),
            learning.write_enabled(),
        )?);
    let skill_file_store: blockcell_tools::SkillFileStoreHandle =
        Arc::new(SkillFileStore::open(paths)?);

    for _round in 0..REVIEW_TOOL_LOOP_MAX_ROUNDS {
        rounds_used += 1;
        let response = if let Some(abort) = lease_abort.as_ref() {
            tokio::select! {
                _ = abort.cancelled() => {
                    return Err(Error::Storage("Lost review lease".into()));
                }
                result = provider.chat(&messages, &tools) => result?,
            }
        } else {
            provider.chat(&messages, &tools).await?
        };
        if response.tool_calls.is_empty() {
            stopped = true;
            stop_reason = "model_stopped".to_string();
            break;
        }

        let mut assistant = ChatMessage::assistant(response.content.as_deref().unwrap_or(""));
        assistant.tool_calls = Some(response.tool_calls.clone());
        messages.push(assistant);

        for call in response.tool_calls {
            if lease_abort.as_ref().is_some_and(AbortToken::is_cancelled) {
                return Err(Error::Storage("Lost review lease".into()));
            }
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
                        ghost_memory_lifecycle.clone(),
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

    if !stopped
        && actions.iter().any(|action| action.tool == "memory_manage")
        && actions.iter().all(|action| action.success)
    {
        stopped = true;
        stop_reason = "call_budget_completed".to_string();
    }

    Ok(GhostReviewToolLoopOutcome {
        actions,
        stopped,
        rounds_used,
        stop_reason,
    })
}

fn dispatched_targets(actions: &[GhostReviewToolAction]) -> Vec<String> {
    let mut targets = Vec::new();
    for action in actions.iter().filter(|action| action.success) {
        let target = action
            .result
            .get("target")
            .and_then(|value| value.as_str())
            .unwrap_or(action.tool.as_str())
            .to_string();
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
}

fn dispatch_failures(actions: &[GhostReviewToolAction]) -> Vec<serde_json::Value> {
    actions
        .iter()
        .filter(|action| !action.success)
        .map(|action| {
            serde_json::json!({
                "tool": action.tool,
                "result": action.result,
            })
        })
        .collect()
}

fn unified_output_from_actions(actions: &[GhostReviewToolAction]) -> serde_json::Value {
    let mut durable_memory = Vec::new();
    let mut user_preferences = Vec::new();
    for action in actions
        .iter()
        .filter(|action| action.success && action.tool == "memory_manage")
    {
        let summary = summarize_learning_action(action).unwrap_or_else(|| "Memory updated".into());
        match action.result.get("target").and_then(|value| value.as_str()) {
            Some("user") => user_preferences.push(summary),
            _ => durable_memory.push(summary),
        }
    }
    serde_json::json!({
        "sessionSummary": null,
        "durableMemory": durable_memory,
        "userPreferences": user_preferences,
        "skillCandidate": null,
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
            "You are a quiet Ghost learning reviewer. Learn only durable user preferences, stable project facts, and reusable non-procedural memory. Every memory_manage call must choose scope explicitly: user preferences/profile use scope=user with target=user; stable project or workspace facts use scope=workspace with target=memory; facts useful only in the current task/session use scope=session. Use only the provided tools to update final memory files directly. Do not create, edit, or request skills. If no durable memory is useful, make no tool calls.",
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
    ghost_memory_lifecycle: Option<Arc<dyn GhostMemoryLifecycleOps + Send + Sync>>,
) -> Result<ToolContext> {
    Ok(ToolContext {
        workspace: paths.workspace(),
        sandbox_writable_roots: None,
        base: paths.base.clone(),
        builtin_skills_dir: Some(paths.builtin_skills_dir()),
        active_skill_dir: None,
        session_key: snapshot
            .session_key
            .clone()
            .unwrap_or_else(|| "ghost_background_review".to_string()),
        channel: "ghost".to_string(),
        account_id: None,
        sender_id: None,
        sender_role: None,
        chat_id: "ghost_background_review".to_string(),
        config: config.clone(),
        permissions: PermissionSet::new(),
        task_manager: None,
        memory_store: None,
        memory_file_store: Some(Arc::clone(memory_file_store)),
        ghost_memory_lifecycle,
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
        evolution_workflow_store: None,
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
    review_worker_id: Option<&str>,
) -> Result<GhostBackgroundReviewOutcome> {
    let run_id = ledger.insert_review_run_if_review_owned(
        NewGhostReviewRun {
            episode_id: episode_id.to_string(),
            reviewer: GHOST_BACKGROUND_REVIEWER.to_string(),
            status: "failed".to_string(),
            result: serde_json::json!({
                "error": error_message,
                "raw": raw,
                "details": details,
            }),
        },
        review_worker_id,
        "review_failed",
    )?;
    match run_id {
        Some(id) => Ok(GhostBackgroundReviewOutcome {
            run_id: id,
            status: "failed".to_string(),
        }),
        None => {
            warn!(episode_id = %episode_id, "Lost review lease before inserting failed review_run");
            Err(Error::Storage("Lost review lease".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
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

    use crate::{
        context::{ContextBuilder, InteractionMode},
        memory_file_store::MemoryFileStore,
        runtime::AgentRuntime,
    };

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
                                "scope": "user",
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
                    "sessionKey": "cli:ghost-review",
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
            turn_facts: None,
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

    #[tokio::test]
    async fn ghost_review_stops_after_lease_loss_before_memory_write() {
        let paths = temp_paths("lease-loss");
        let snapshot = GhostEpisodeSnapshot {
            boundary_kind: crate::ghost_learning::GhostLearningBoundaryKind::TurnEnd,
            session_key: Some("cli:lease-loss".to_string()),
            subject_key: Some("chat:lease-loss".to_string()),
            user_intent_summary: "learn deploy preference".to_string(),
            assistant_outcome_summary: "prefer canary-first rollout".to_string(),
            tool_call_count: 0,
            memory_write_count: 0,
            correction_count: 1,
            preference_correction_count: 1,
            complexity_score: 5,
            reusable_lesson: Some("Prefer canary-first rollout".to_string()),
            decision: crate::ghost_learning::LearningDecision::ReviewAfterResponse,
            turn_facts: None,
        };
        let provider = ToolLoopReviewProvider::new(ToolLoopMode::WriteFiles);
        let lease_abort = blockcell_core::AbortToken::new();
        lease_abort.cancel();

        let error = run_restricted_review_tool_loop(
            &paths,
            &provider,
            &snapshot,
            &Config::default(),
            None,
            Some(lease_abort),
        )
        .await
        .expect_err("lost lease must stop review");

        assert!(error.to_string().contains("Lost review lease"));
        let scoped = MemoryFileStore::open_for_session(&paths, "cli:lease-loss").unwrap();
        assert!(scoped.load_snapshot().unwrap().user_block.is_none());
    }

    fn test_runtime_with_background_review(
        paths: Paths,
        provider: Arc<dyn Provider>,
    ) -> AgentRuntime {
        let mut config = Config::default();
        config.agents.defaults.model = "test/mock".to_string();
        config.agents.defaults.provider = Some("test".to_string());
        config.agents.ghost.learning.enabled = true;
        config.agents.ghost.learning.capture_enabled = Some(true);
        config.agents.ghost.learning.write_enabled = Some(false);
        config.agents.ghost.learning.recall_enabled = Some(false);

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
            None,
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

        assert!(!paths.user_md().exists());
        let normal_scoped = MemoryFileStore::open_for_session(&paths, "cli:ghost-review")
            .expect("open normal scoped memory")
            .load_snapshot()
            .expect("load normal scoped memory");
        assert!(normal_scoped.user_block.is_none());
        let shadow_user_path = paths.memory_dir().join("shadow").join("USER.md");
        let user_memory = std::fs::read_to_string(shadow_user_path).expect("shadow USER memory");
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
        assert_eq!(run.result["classifierCallCount"], serde_json::json!(1));
        assert!(run.result["providerCallCount"].as_u64().unwrap_or_default() <= 2);
        assert_eq!(run.result["skillDeepeningCallCount"], serde_json::json!(0));
        assert!(run.result["dispatchedTargets"].is_array());
        assert!(run.result["failures"].is_array());
        assert!(run.result["output"].is_object());
        assert_eq!(
            run.result["learningFeedback"],
            serde_json::json!(["User profile updated"])
        );
    }

    #[tokio::test]
    async fn user_scoped_review_is_recalled_by_a_new_session() {
        let paths = temp_paths("user-scope-recall");
        let episode_id = insert_sample_episode(&paths);
        let provider = Arc::new(ToolLoopReviewProvider::new(ToolLoopMode::WriteFiles));
        let provider_pool = ProviderPool::from_single_provider("test/mock", "test", provider);
        let mut config = Config::default();
        config.agents.ghost.learning.write_enabled = Some(true);
        config.agents.ghost.learning.recall_enabled = Some(true);

        run_background_review_for_episode(
            &paths,
            provider_pool,
            &episode_id,
            &config,
            &GhostLedger::open(&paths.ghost_ledger_db()).expect("open ledger"),
            None,
        )
        .await
        .expect("run user scoped review");

        let durable_user = std::fs::read_to_string(paths.user_md()).expect("global USER.md");
        assert!(durable_user.contains("canary-first rollout"));
        let index = blockcell_storage::KnowledgeIndex::open(&paths.knowledge_index_db())
            .expect("open knowledge index");
        assert_eq!(index.search("canary-first", 10).unwrap().len(), 1);
        let old_session = MemoryFileStore::open_for_session(&paths, "cli:ghost-review")
            .expect("open original session store")
            .load_snapshot()
            .expect("load original session store");
        assert!(old_session.user_block.is_none());

        let builder = ContextBuilder::new(paths, config);
        let messages = builder.build_messages_for_session_mode_with_channel(
            "cli:new-session",
            &[],
            "canary-first",
            &[],
            InteractionMode::General,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            false,
            &[],
            &[],
        );
        let runtime_context = messages[messages.len() - 2]
            .content
            .as_str()
            .expect("runtime context");
        assert!(runtime_context.contains("canary-first rollout"));
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
            None,
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
            None,
        )
        .await
        .expect("run background review");

        assert_eq!(outcome.status, "completed");
        assert!(!paths.memory_md().exists());
        let shadow_memory_path = paths
            .memory_dir()
            .join("shadow")
            .join("sessions")
            .join(blockcell_core::stable_hash_session_key("cli:ghost-review"))
            .join("MEMORY.md");
        let durable_memory =
            std::fs::read_to_string(shadow_memory_path).expect("shadow MEMORY memory");
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
            None,
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
            None,
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
            None,
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
            None,
        )
        .await
        .expect("record failed review");

        assert_eq!(outcome.status, "failed");
        assert_eq!(provider.review_calls.load(Ordering::SeqCst), 2);
        let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
        let run = ledger
            .get_review_run(&outcome.run_id)
            .expect("review run query")
            .expect("review run exists");
        assert_eq!(
            run.result["error"],
            serde_json::json!("Restricted ghost review tool loop exceeded max rounds")
        );
        assert_eq!(run.result["details"]["maxRounds"], serde_json::json!(2));
        assert_eq!(run.result["details"]["roundsUsed"], serde_json::json!(2));
        assert_eq!(
            run.result["details"]["stopReason"],
            serde_json::json!("max_rounds")
        );
        assert_eq!(run.result["details"]["actionCount"], serde_json::json!(2));
    }

    #[tokio::test]
    async fn background_review_call_budget_prevents_long_followup_loop() {
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
            None,
        )
        .await
        .expect("run background review");

        assert_eq!(outcome.status, "failed");
        assert_eq!(provider.review_calls.load(Ordering::SeqCst), 2);
        let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
        let run = ledger
            .get_review_run(&outcome.run_id)
            .expect("review run query")
            .expect("review run exists");
        assert_eq!(run.result["details"]["maxRounds"], serde_json::json!(2));
        assert_eq!(run.result["details"]["roundsUsed"], serde_json::json!(2));
        assert_eq!(
            run.result["details"]["stopReason"],
            serde_json::json!("max_rounds")
        );
        assert_eq!(run.result["details"]["actionCount"], serde_json::json!(2));
    }
}
