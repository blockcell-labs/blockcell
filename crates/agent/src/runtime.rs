use blockcell_core::config::ApprovalPolicy;
use blockcell_core::path_policy::{resolve_for_policy, PathPolicy};
use blockcell_core::system_event::{EventPriority, EventScope, SessionSummary, SystemEvent};
use blockcell_core::tool_policy::{
    PolicyEvalResult, ToolCallContext, ToolPolicy, ToolPolicyDecision,
};
use blockcell_core::types::{
    ChatMessage, LLMResponse, StreamChunk, ToolCallAccumulator, ToolCallRequest,
};
use blockcell_core::{
    scope_abort_token, scope_agent_context, AbortToken, AgentIdentity, BudgetExhaustedError,
    BudgetTracker, BudgetTrackerHandle, Config, FailureKind, Idempotency, InboundMessage,
    OutboundMessage, Paths, RequestRetry, Result, ToolOutcome,
};
use blockcell_providers::{CallResult, Provider, ProviderPool, RoutingContext, ServedBy};
use blockcell_skills::SkillCard;
use blockcell_storage::ghost_ledger::{GhostEpisodeSource, NewGhostEpisode};
use blockcell_storage::{AuditLogger, GhostLedger, SessionStore};
use blockcell_tools::sandbox::{native_backend, SandboxBackend, SandboxPolicy};
use blockcell_tools::tool_guard::{dangerous_operation_items, is_dangerous_exec_command};
use blockcell_tools::{
    CapabilityRegistryHandle, CoreEvolutionHandle, EventEmitterHandle, MemoryStoreHandle,
    SessionSearchOps, SpawnHandle, SystemEventEmitter, TaskManagerHandle, ToolRegistry,
};
use regex::Regex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

use crate::capability_adapter::EvolutionWorkflowStoreAdapter;
use crate::context::{ActiveSkillContext, ContextBuilder, InteractionMode};
use crate::error::{
    classify_tool_failure, dangerous_exec_denied, dangerous_file_ops_denied, disabled_skill_result,
    disabled_tool_result, llm_exhausted_error, scoped_tool_denied_result, ToolFailureKind,
};
use crate::ghost_learning::{
    estimate_turn_complexity_score, GhostEpisodeSnapshot, GhostLearningBoundary,
    GhostLearningBoundaryKind, GhostLearningPolicy, LearningDecision,
};
use crate::ghost_recall::should_inject_ghost_recall;
use crate::history_projector::{HistoryProjector, TimeBasedMCConfig};
use crate::hooks::{HookAdapter, HookManager};
use crate::intent::{IntentCategory, IntentToolResolver};
use crate::learning_coordinator::LearningReviewReservationGuard;
use crate::memory_event;
use crate::memory_file_store::MemoryFileStore;
use crate::response_cache::{cleanup_tool_results, sanitize_session_key, sanitize_tool_use_id};
use crate::runtime_kernel::{AgentEvent, AgentEventContext, EventBus, EventDecision};
use crate::session_metrics::{ProcessingMetrics, ScopedTimer};
use crate::skill_executor::{determine_manual_load_mode, SkillExecutionResult};
use crate::skill_file_store::SkillFileStore;
use crate::skill_kernel::SkillRunMode;
use crate::steering::{
    ActiveConversationKey, SteeringChannel, SteeringMessage, SteeringRegistry,
    SteeringRouteOutcome, SteeringSender,
};
use crate::summary_queue::MainSessionSummaryQueue;
use crate::system_event_orchestrator::{
    HeartbeatDecision, NotificationRequest, SystemEventOrchestrator,
};
use crate::system_event_store::{InMemorySystemEventStore, SystemEventStoreOps};
use crate::task_manager::{OriginScopedTaskManager, TaskManager, TaskStatus};
use crate::token::estimate_messages_tokens;
use pre_step::PreStepDecision;

// 学习与记忆协调方法 — 已移至 runtime/learning.rs
pub mod learning;
// 路径安全检查方法 — 已移至 runtime/path_security.rs
pub mod path_security;

// --- submodules extracted from the original monolithic runtime.rs ---
pub mod agent_loop;
mod compaction;
mod event_listeners;
mod fork_spawn;
mod ghost_boundary;
mod lightweight_handle;
mod message_dispatch;
mod message_task;
pub mod pre_step;
mod prefetch;
mod process_message_inner;
mod process_message_phases;
mod run_loop;
mod skill_session;
mod subagent;
mod subagent_provider;
mod tool_exec;
mod tool_pipeline;
mod turn_flow;
mod wiring;

pub(crate) use fork_spawn::{normalize_typed_agent_result, resolve_typed_workspace_scope};
pub(crate) use ghost_boundary::*;
pub use lightweight_handle::*;
pub(crate) use message_task::*;
pub(crate) use subagent::*;
pub use subagent_provider::InProcessSubagentProvider;

const TOOL_ROUND_THROTTLE_MS: u64 = 600;
const TOOL_ROUND_THROTTLE_AFTER_RATE_LIMIT_MS: u64 = 2_500;
const MAX_IDENTICAL_TOOL_FAILURES: u32 = 3;

/// 一次工具调用交还给主循环的结果：返回给模型的内容与 Runtime 判定的执行结论。
pub(crate) struct ToolCallOutput {
    pub(crate) text: String,
    pub(crate) outcome: ToolOutcome,
}

impl ToolCallOutput {
    /// 未执行（被拒绝、超出上限、不在作用域内）的调用。
    fn rejected(text: String) -> Self {
        Self {
            text,
            outcome: ToolOutcome::Rejected,
        }
    }
}

/// 回合内的工具调用守卫，主循环与技能循环共用：
/// - 单工具调用上限（`maxToolIterations` / `maxToolIterationsByTool`）；
/// - 回合总调用上限：取 `max(default_limit, 按工具上限的最大值)`，
///   `maxToolIterations` 由此同时构成单回合成本硬上限，
///   而显式放宽某工具的上限（如 exec: 500）仍被尊重；
/// - 同一调用（工具名 + 参数）连续未成功达到阈值即停止。
struct ToolLoopGuard {
    default_limit: u32,
    limits_by_tool: HashMap<String, u32>,
    global_limit: u32,
    call_counts: HashMap<String, u32>,
    total_calls: u32,
    last_signature: Option<String>,
    consecutive_failures: u32,
}

impl ToolLoopGuard {
    fn new(default_limit: u32, limits_by_tool: HashMap<String, u32>) -> Self {
        let global_limit = limits_by_tool
            .values()
            .copied()
            .max()
            .unwrap_or(0)
            .max(default_limit);
        Self {
            default_limit,
            limits_by_tool,
            global_limit,
            call_counts: HashMap::new(),
            total_calls: 0,
            last_signature: None,
            consecutive_failures: 0,
        }
    }

    fn call_counts(&self) -> &HashMap<String, u32> {
        &self.call_counts
    }

    /// 计入一次调用；超过该工具的调用上限或回合总上限时返回作为工具结果的拒绝内容。
    fn admit(&mut self, tool_name: &str) -> std::result::Result<(), String> {
        let limit = self
            .limits_by_tool
            .get(tool_name)
            .copied()
            .unwrap_or(self.default_limit);
        let count = self.call_counts.entry(tool_name.to_string()).or_insert(0);
        *count += 1;
        self.total_calls += 1;
        if *count <= limit && self.total_calls <= self.global_limit {
            return Ok(());
        }
        if *count > limit {
            return Err(serde_json::json!({
                "error": format!("Tool '{}' execeeded max call limit ({}).", tool_name, limit),
                "tool": tool_name,
                "hint": "Reduce repeated tool calls or adjust maxToolIterationsByTool."
            })
            .to_string());
        }
        Err(serde_json::json!({
            "error": format!("Exceeded max total tool iterations ({}).", self.global_limit),
            "tool": tool_name,
            "hint": "Reduce total tool calls in one turn or raise maxToolIterations."
        })
        .to_string())
    }

    /// Non-mutating counterpart of [`Self::admit`], used by the read-only
    /// prefetch gate: a call that the loop guard would reject must not be
    /// executed eagerly (its result would be discarded).
    fn would_admit(&self, tool_name: &str) -> bool {
        let limit = self
            .limits_by_tool
            .get(tool_name)
            .copied()
            .unwrap_or(self.default_limit);
        let count = self.call_counts.get(tool_name).copied().unwrap_or(0);
        count < limit && self.total_calls < self.global_limit
    }

    /// Check a candidate against the guard plus calls already reserved by a
    /// single prefetch batch. The batch executes concurrently, so it cannot
    /// call `admit` one-by-one; keeping these reservations prevents a batch
    /// from overshooting the per-tool or global budget.
    fn would_admit_batch(
        &self,
        tool_name: &str,
        pending: &HashMap<String, u32>,
        pending_total: u32,
    ) -> bool {
        if !self.would_admit(tool_name)
            || self.total_calls.saturating_add(pending_total) >= self.global_limit
        {
            return false;
        }
        let limit = self
            .limits_by_tool
            .get(tool_name)
            .copied()
            .unwrap_or(self.default_limit);
        self.call_counts
            .get(tool_name)
            .copied()
            .unwrap_or(0)
            .saturating_add(pending.get(tool_name).copied().unwrap_or(0))
            < limit
    }

    /// 记录调用结论；同一调用连续未成功达到阈值时返回 `true`。
    fn record(&mut self, call: &ToolCallRequest, failed: bool) -> bool {
        if !failed {
            self.last_signature = None;
            self.consecutive_failures = 0;
            return false;
        }

        let signature = format!("{}\0{}", call.name, call.arguments);
        if self.last_signature.as_deref() == Some(signature.as_str()) {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        } else {
            self.last_signature = Some(signature);
            self.consecutive_failures = 1;
        }

        self.consecutive_failures >= MAX_IDENTICAL_TOOL_FAILURES
    }
}
const ACTIVATE_SKILL_TOOL_NAME: &str = "activate_skill";

/// Review 模式枚举
///
/// 用于 NudgeEngine 触发后台 Review 时决定审查范围：
/// - Skill: 仅审查 Skill 库
/// - Memory: 仅审查用户记忆
/// - Combined: 同时审查 Skill 库和用户记忆
#[derive(Debug, Clone)]
enum ReviewMode {
    /// 审查 Skill 库，判断是否需要创建/修补 Skill
    Skill,
    /// 审查对话历史，保存用户偏好和重要信息
    Memory,
    /// 同时审查 Skill 库和用户记忆
    Combined,
}

pub(crate) enum PolicyOutcome {
    Proceed,
    ProceedConfirmed,
    Denied(String),
}

/// Memory Review 提示词
/// Memory Review 提示词 (与 Hermes _MEMORY_REVIEW_PROMPT 一致)
const MEMORY_REVIEW_PROMPT: &str = "\
Review the conversation above and consider saving to memory if appropriate.\n\n\
Focus on:\n\
1. Has the user revealed things about themselves — their persona, desires, \
preferences, or personal details worth remembering?\n\
2. Has the user expressed expectations about how you should behave, their work \
style, or ways they want you to operate?\n\n\
If something stands out, save it using the memory tool. \
If nothing is worth saving, just say 'Nothing to save.' and stop.";

/// Skill Review 提示词 (与 Hermes _SKILL_REVIEW_PROMPT 一致)
const SKILL_REVIEW_PROMPT: &str = "\
Review the conversation above and consider saving or updating a skill if appropriate.\n\n\
Focus on: was a non-trivial approach used to complete a task that required trial \
and error, or changing course due to experiential findings along the way, or did \
the user expect or desire a different method or outcome?\n\n\
If a relevant skill already exists, update it with what you learned. \
Otherwise, create a new skill if the approach is reusable.\n\
If nothing is worth saving, just say 'Nothing to save.' and stop.";

/// Combined Review 提示词 (与 Hermes _COMBINED_REVIEW_PROMPT 一致)
const COMBINED_REVIEW_PROMPT: &str = "\
Review the conversation above and consider two things:\n\n\
**Memory**: Has the user revealed things about themselves — their persona, \
desires, preferences, or personal details? Has the user expressed expectations \
about how you should behave, their work style, or ways they want you to operate? \
If so, save using the memory tool.\n\n\
**Skills**: Was a non-trivial approach used to complete a task that required trial \
and error, or changing course due to experiential findings along the way, or did \
the user expect or desire a different method or outcome? If a relevant skill \
already exists, update it. Otherwise, create a new one if the approach is reusable.\n\n\
Only act if there's something genuinely worth saving. \
If nothing stands out, just say 'Nothing to save.' and stop.";

/// Compact execution context - contains info needed for notifications.
///
/// Used to send user notifications before/after compression operations.
pub struct CompactContext<'a> {
    /// Channel to send notification to.
    pub channel: &'a str,
    /// Chat ID to send notification to.
    pub chat_id: &'a str,
    /// Account ID for multi-tenant scenarios.
    pub account_id: Option<&'a str>,
}

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
    memory_file_store: Option<blockcell_tools::MemoryFileStoreHandle>,
    config: Config,
    paths: Paths,
    task_manager: TaskManager,
    outbound_tx: Option<mpsc::Sender<OutboundMessage>>,
    provider_pool: Arc<ProviderPool>,
    agent_id: Option<String>,
    event_tx: Option<broadcast::Sender<String>>,
    origin_account_id: Option<String>,
    origin_session_key: String,
    response_cache: crate::response_cache::ResponseCache,
    event_emitter: EventEmitterHandle,
    abort_token: Option<AbortToken>,
    completion_reporter: subagent::SubagentCompletionReporter,
    /// 发起会话的预算 tracker；子 Agent 的消耗归并到它。
    parent_budget: Option<BudgetTrackerHandle>,
}

impl SpawnHandle for RuntimeSpawnHandle {
    fn spawn(
        &self,
        task: &str,
        label: &str,
        origin_channel: &str,
        origin_chat_id: &str,
        agent_type: Option<&str>,
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
        let normalized_task = normalize_spawn_task(task);
        let task_id_clone = task_id.clone();
        let label_clone = label.to_string();
        let origin_channel = origin_channel.to_string();
        let origin_chat_id = origin_chat_id.to_string();
        let agent_id = self.agent_id.clone();
        let event_tx = self.event_tx.clone();
        let session_store = SessionStore::new(self.paths.clone());
        let origin_history = session_store
            .load(&self.origin_session_key)
            .unwrap_or_default();
        let origin_history_seed = expand_history_stubs_with_cache(
            &self.response_cache,
            &self.origin_session_key,
            &origin_history,
        );

        // Spawn the background task. Task registration (create_task) happens inside
        // run_subagent_task before set_running(), eliminating the race condition.
        let agent_type_str = agent_type.map(|s| s.to_string());
        // Create child abort token for the subagent (chain propagation)
        let child_abort_token = self.abort_token.as_ref().map(|t| t.child());
        let join_handle = tokio::spawn(run_subagent_task(
            self.memory_file_store.clone(),
            config,
            paths,
            provider_pool,
            task_manager.clone(),
            outbound_tx,
            normalized_task,
            task_id_clone,
            label_clone,
            origin_channel,
            origin_chat_id,
            self.origin_account_id.clone(),
            agent_id,
            event_tx,
            origin_history_seed,
            self.origin_session_key.clone(),
            self.event_emitter.clone(),
            agent_type_str,
            child_abort_token,
            Some(self.completion_reporter.clone()),
            self.parent_budget.clone(),
        ));

        // Guard: if tokio::spawn fails or task panics, mark as Failed to prevent stuck Running
        let guard_tm = task_manager;
        let guard_id = task_id.clone();
        tokio::spawn(async move {
            if let Err(e) = join_handle.await {
                if e.is_panic() {
                    tracing::error!(task_id = %guard_id, "Subagent task panicked");
                    guard_tm
                        .set_failed(&guard_id, "Subagent task panicked")
                        .await;
                } else {
                    tracing::warn!(task_id = %guard_id, "Subagent task was cancelled/aborted");
                }
            }
        });

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
    /// Agent that owns the originating message.
    pub agent_id: Option<String>,
    /// The channel the originating message came from (e.g. "ws", "lark", "telegram").
    pub channel: String,
    /// Channel account that received the originating message.
    pub account_id: Option<String>,
    /// Sender that must answer the confirmation prompt.
    pub sender_id: String,
    /// The chat_id of the originating message, used to route the confirmation
    /// prompt back to the correct conversation.
    pub chat_id: String,
    /// Server-generated id for the originating WebSocket connection, when the
    /// request came from an interactive WS client.
    pub ws_connection_id: Option<String>,
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

/// agent/request-error：错误分类与恢复策略由 Runtime 决定，Provider 只提供错误信息。
fn request_error_event(error: &blockcell_core::Error) -> AgentEvent {
    let error = error.to_string();
    let kind = FailureKind::classify_provider_message(&error);
    AgentEvent::AgentRequestError {
        error,
        kind,
        recovery: kind.request_recovery(),
    }
}

fn model_response_event(response: &LLMResponse, served_by: &ServedBy) -> AgentEvent {
    AgentEvent::ModelResponse {
        content: response.content.clone().unwrap_or_default(),
        tool_calls: serde_json::to_value(&response.tool_calls).unwrap_or_default(),
        finish_reason: response.finish_reason.clone(),
        model: served_by.model.clone(),
        provider: served_by.provider.clone(),
        usage: response.usage.clone(),
    }
}

fn tool_round_throttle_delay(saw_rate_limit_this_turn: bool) -> std::time::Duration {
    if saw_rate_limit_this_turn {
        std::time::Duration::from_millis(TOOL_ROUND_THROTTLE_AFTER_RATE_LIMIT_MS)
    } else {
        std::time::Duration::from_millis(TOOL_ROUND_THROTTLE_MS)
    }
}

fn build_activate_skill_tool_schema(skill_cards: &[SkillCard]) -> Option<serde_json::Value> {
    if skill_cards.is_empty() {
        return None;
    }

    let skill_names = skill_cards
        .iter()
        .map(|card| serde_json::Value::String(card.name.clone()))
        .collect::<Vec<_>>();

    Some(serde_json::json!({
        "type": "function",
        "function": {
            "name": ACTIVATE_SKILL_TOOL_NAME,
            "description": "Activate one installed skill when it is a better fit than general tools. Do not combine this with other tool calls in the same assistant turn.",
            "parameters": {
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "enum": skill_names,
                        "description": "The installed skill name to activate."
                    },
                    "goal": {
                        "type": "string",
                        "description": "A short execution goal for the selected skill."
                    }
                },
                "required": ["skill_name", "goal"],
                "additionalProperties": false
            }
        }
    }))
}

fn inject_skill_cards_into_system_prompt(
    messages: &mut Vec<ChatMessage>,
    skill_cards: &[SkillCard],
    recent_skill_name: Option<&str>,
) {
    if skill_cards.is_empty() {
        return;
    }

    let mut section = String::from(
        "## Installed Skills\nUse `activate_skill` when one installed skill is a better fit than general tools.\nIf you call `activate_skill`, do not call any other tools in the same assistant turn.\n",
    );
    section.push_str(
        "If a skill is relevant but you need to inspect the learned procedure before using or patching it, inspect it with `skill_view`. If a loaded skill is stale, incomplete, or wrong, patch it with `skill_manage(action=\"patch\")` before finishing.\n",
    );
    section.push_str(
        "If a skill card shows local execution entries, you may use `exec_local` only for those relative paths and only inside the active skill scope. Do not auto-run local scripts unless the skill is active.\n",
    );

    if let Some(skill_name) = recent_skill_name {
        section.push_str(&format!(
            "Recent active skill: `{}`. If the user is continuing that workflow, prefer re-entering the same skill.\n",
            skill_name
        ));
    }

    for card in skill_cards {
        let local_exec_note = if card.supports_local_exec {
            if card.local_exec_entrypoints.is_empty() {
                " | 本地入口: active skill 目录内的相对脚本".to_string()
            } else {
                format!(" | 本地入口: {}", card.local_exec_entrypoints.join(", "))
            }
        } else {
            String::new()
        };

        let card_text = format!(
            "- `{}`: {} | 布局: {}{} | 适合: {} | 输出: {}\n",
            card.name,
            card.description,
            card.execution_layout,
            local_exec_note,
            card.when_to_use,
            card.outputs
        );
        section.push_str(&crate::retrieval::truncate_to_token_budget(&card_text, 300));
        if !section.ends_with('\n') {
            section.push('\n');
        }
    }

    append_ephemeral_user_context(messages, &section);
}

fn append_ephemeral_user_context(messages: &mut Vec<ChatMessage>, section: &str) {
    let Some(current_user_index) = messages.iter().rposition(|message| message.role == "user")
    else {
        return;
    };
    if current_user_index > 0 {
        let ephemeral = &mut messages[current_user_index - 1];
        if ephemeral.role == "user" {
            if let Some(existing) = ephemeral.content.as_str() {
                if existing.starts_with("<runtime-context>") {
                    let updated = if let Some(prefix) = existing.strip_suffix("</runtime-context>")
                    {
                        format!(
                            "{}\n{}\n</runtime-context>",
                            prefix.trim_end(),
                            section.trim()
                        )
                    } else {
                        format!("{}\n{}", existing, section.trim())
                    };
                    ephemeral.content = serde_json::Value::String(updated);
                    return;
                }
            }
        }
    }
    messages.insert(
        current_user_index,
        ChatMessage::user(&format!(
            "<runtime-context>\n{}\n</runtime-context>",
            section.trim()
        )),
    );
}

/// Inject current running typed-agent tasks into the system prompt.
///
/// This gives the LLM real-time awareness of background tasks, preventing it from
/// making incorrect judgments based on stale conversation history.
/// Only typed agent tasks (explore, plan, verification, viper, general) are included;
/// message tasks (msg_*) are excluded since they are just conversation sessions,
/// not actual background work.
async fn inject_running_tasks_into_system_prompt(
    messages: &mut Vec<ChatMessage>,
    task_manager: &TaskManager,
) -> Vec<String> {
    let task_list = task_manager.list_tasks(Some(&TaskStatus::Running)).await;

    // 只保留 typed agent 任务（有 agent_type 的），排除 msg_ 会话任务
    // 限制最多注入 10 个运行中任务，防止 system prompt 过长导致 LLM API 调用失败
    const MAX_INJECT_TASKS: usize = 10;
    let running_agents: Vec<_> = task_list
        .iter()
        .filter(|t| t.agent_type.is_some())
        .take(MAX_INJECT_TASKS)
        .collect();
    let running_truncated =
        task_list.iter().filter(|t| t.agent_type.is_some()).count() > MAX_INJECT_TASKS;

    // 查找已完成但结果尚未注入到LLM对话的子agent任务
    let completed_tasks = task_manager.list_tasks(Some(&TaskStatus::Completed)).await;
    let uninject_completed: Vec<_> = completed_tasks
        .iter()
        .filter(|t| t.agent_type.is_some() && !t.result_injected && t.result.is_some())
        .collect();

    if running_agents.is_empty() && uninject_completed.is_empty() {
        return Vec::new();
    }

    let mut section = String::from("## Background Tasks\n");
    let injected_completed_task_ids: Vec<String> =
        uninject_completed.iter().map(|t| t.id.clone()).collect();

    if !running_agents.is_empty() {
        section.push_str("The following agent tasks are currently running in the background:\n\n");
        for t in &running_agents {
            let short_id = {
                let meaningful = if let Some(rest) = t.id.strip_prefix("task-") {
                    rest
                } else {
                    &t.id
                };
                meaningful.chars().take(8).collect::<String>()
            };
            let agent_type = t.agent_type.as_deref().unwrap_or("unknown");
            let label = if t.label.is_empty() {
                agent_type
            } else {
                &t.label
            };
            section.push_str(&format!(
                "- `[{}]` **{}** agent: {}\n",
                short_id, agent_type, label
            ));
            if let Some(ref progress) = t.progress {
                section.push_str(&format!("  - Progress: {}\n", progress));
            }
        }
        section.push_str("\n- If the user asks to start a new task of the same type, ask whether to cancel the existing task first or wait for it to complete.\n- Use `/tasks` to check task status, `/tasks cancel <id>` to cancel a running task.\n");
        if running_truncated {
            section.push_str(&format!(
                "\n- (Showing {} of {} running tasks. Use `/tasks` to see all.)\n",
                MAX_INJECT_TASKS,
                task_list.iter().filter(|t| t.agent_type.is_some()).count()
            ));
        }
    }

    // 注入已完成的子agent结果
    if !uninject_completed.is_empty() {
        section.push_str("\n## Completed Agent Results\nThe following background agent tasks have completed. Use their results to answer the user's question:\n\n");
        for t in &uninject_completed {
            let short_id = {
                let meaningful = if let Some(rest) = t.id.strip_prefix("task-") {
                    rest
                } else {
                    &t.id
                };
                meaningful.chars().take(8).collect::<String>()
            };
            let agent_type = t.agent_type.as_deref().unwrap_or("unknown");
            let label = if t.label.is_empty() {
                agent_type
            } else {
                &t.label
            };
            section.push_str(&format!(
                "### `[{}]` **{}** agent: {}\n\n",
                short_id, agent_type, label
            ));
            if let Some(ref result) = t.result {
                // 截断过长的结果，避免 system prompt 过大
                let display = if result.chars().count() > 3000 {
                    let truncated: String = result.chars().take(3000).collect();
                    format!(
                        "{}...\n\n(Result truncated. Use `/tasks {}` to see full result)",
                        truncated, short_id
                    )
                } else {
                    result.clone()
                };
                section.push_str(&display);
                section.push('\n');
            }
            section.push('\n');
        }
        section.push_str("- You should integrate and summarize these results for the user.\n- If the user asks for details, reference the specific task_id.\n");

        // 只返回已注入的任务 ID，调用方在主响应成功后再标记 result_injected。
    }

    append_ephemeral_user_context(messages, &section);
    injected_completed_task_ids
}

fn normalize_selected_skill_name(
    raw_skill_name: &str,
    skill_cards: &[SkillCard],
) -> Option<String> {
    let candidates = skill_cards
        .iter()
        .map(|card| (card.name.clone(), card.description.clone()))
        .collect::<Vec<_>>();
    crate::skill_decision::SkillDecisionEngine::normalize_selected_skill_name(
        raw_skill_name,
        &candidates,
    )
}

fn extract_llm_usage_tokens(usage: &serde_json::Value) -> (u64, u64) {
    fn read_u64(usage: &serde_json::Value, keys: &[&str]) -> u64 {
        keys.iter()
            .find_map(|key| usage.get(*key))
            .and_then(|value| {
                value
                    .as_u64()
                    .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
                    .or_else(|| {
                        value
                            .as_str()
                            .and_then(|text| text.trim().parse::<u64>().ok())
                    })
            })
            .unwrap_or(0)
    }

    let input = read_u64(
        usage,
        &[
            "input_tokens",
            "inputTokens",
            "prompt_tokens",
            "promptTokens",
            "promptTokenCount",
        ],
    );
    let output = read_u64(
        usage,
        &[
            "output_tokens",
            "outputTokens",
            "completion_tokens",
            "completionTokens",
            "candidatesTokenCount",
        ],
    );

    (input, output)
}

fn extract_llm_cache_tokens(usage: &serde_json::Value) -> (u64, u64) {
    fn read_u64(usage: &serde_json::Value, key: &str) -> u64 {
        usage
            .get(key)
            .and_then(|value| {
                value
                    .as_u64()
                    .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
                    .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
            })
            .unwrap_or(0)
    }

    (
        read_u64(usage, "cache_read_input_tokens"),
        read_u64(usage, "cache_creation_input_tokens"),
    )
}

fn summarize_tool_args(args: &serde_json::Value) -> String {
    let summary = compact_json_value(args, 0);
    let text = serde_json::to_string(&summary).unwrap_or_else(|_| "<unserializable>".to_string());
    truncate_str(&text, 500)
}

#[allow(clippy::too_many_arguments)]
fn append_activated_skill_history(
    history: &mut Vec<ChatMessage>,
    current_messages: &mut Vec<ChatMessage>,
    activation_call_id: &str,
    skill_name: &str,
    goal: &str,
    allowed_tools: &[String],
    trace_messages: &[ChatMessage],
    final_response: &str,
) {
    let mut activation_result = ChatMessage::tool_result(
        activation_call_id,
        &serde_json::json!({
            "skill_name": skill_name,
            "goal": goal,
            "status": "completed"
        })
        .to_string(),
    );
    activation_result.name = Some(ACTIVATE_SKILL_TOOL_NAME.to_string());
    history.push(activation_result.clone());
    // Deferred review snapshots are built from `current_messages`; without the
    // activation tool_result here the assistant message's activate_skill
    // tool_use stays unpaired and fails provider-side validation.
    current_messages.push(activation_result);

    push_internal_skill_trace(
        history,
        "skill_enter",
        serde_json::json!({
            "skill_name": skill_name,
            "allowed_tools": allowed_tools,
            "goal": goal,
        }),
        &serde_json::json!({
            "skill_name": skill_name,
            "kind": "prompt",
            "allowed_tools": allowed_tools,
            "goal": goal,
        })
        .to_string(),
    );
    history.extend(trace_messages.iter().cloned());
    history.push(ChatMessage::assistant(final_response));
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

fn build_internal_skill_tool_call(
    tool_name: &str,
    arguments: serde_json::Value,
) -> ToolCallRequest {
    ToolCallRequest {
        id: format!("{}-{}", tool_name, uuid::Uuid::new_v4()),
        name: tool_name.to_string(),
        arguments,
        thought_signature: None,
    }
}

fn push_internal_skill_trace(
    history: &mut Vec<ChatMessage>,
    tool_name: &str,
    arguments: serde_json::Value,
    result: &str,
) {
    let tool_call = build_internal_skill_tool_call(tool_name, arguments);
    history.push(ChatMessage {
        id: None,
        role: "assistant".to_string(),
        content: serde_json::Value::String(String::new()),
        reasoning_content: None,
        tool_calls: Some(vec![tool_call.clone()]),
        tool_call_id: None,
        name: None,
    });

    let mut tool_result = ChatMessage::tool_result(&tool_call.id, result);
    tool_result.name = Some(tool_name.to_string());
    history.push(tool_result);
}

fn persist_prompt_skill_history(
    history: &mut Vec<ChatMessage>,
    user_input: &str,
    skill_name: &str,
    allowed_tools: &[String],
    trace_messages: &[ChatMessage],
    final_response: &str,
) {
    history.push(ChatMessage::user(user_input));
    push_internal_skill_trace(
        history,
        "skill_enter",
        serde_json::json!({
            "skill_name": skill_name,
            "allowed_tools": allowed_tools,
        }),
        &serde_json::json!({
            "skill_name": skill_name,
            "kind": "prompt",
            "allowed_tools": allowed_tools,
        })
        .to_string(),
    );
    history.extend(trace_messages.iter().cloned());
    history.push(ChatMessage::assistant(final_response));
}

#[allow(dead_code)]
fn persist_script_skill_history(
    history: &mut Vec<ChatMessage>,
    user_input: &str,
    skill_name: &str,
    internal_tool_name: &str,
    argv: &[String],
    raw_result: &str,
    final_response: &str,
) {
    history.push(ChatMessage::user(user_input));
    push_internal_skill_trace(
        history,
        internal_tool_name,
        serde_json::json!({
            "skill_name": skill_name,
            "argv": argv,
        }),
        raw_result,
    );
    history.push(ChatMessage::assistant(final_response));
}

fn find_recent_skill_name_from_history(history: &[ChatMessage]) -> Option<String> {
    HistoryProjector::new(history).analyze().latest_skill_name
}

const SESSION_ACTIVE_SKILL_NAME_KEY: &str = "active_skill_name";
const SESSION_ACTIVE_SKILL_CORRECTIONS_KEY: &str = "active_skill_correction_count";
const LEARNED_SKILL_DISABLE_THRESHOLD: u32 = 2;

fn active_skill_name_from_metadata(metadata: &serde_json::Value) -> Option<String> {
    metadata
        .get(SESSION_ACTIVE_SKILL_NAME_KEY)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn continued_skill_name(metadata: &serde_json::Value, history: &[ChatMessage]) -> Option<String> {
    active_skill_name_from_metadata(metadata)
        .or_else(|| find_recent_skill_name_from_history(history))
}

fn record_active_skill_name(metadata: &mut serde_json::Value, skill_name: &str) {
    let trimmed = skill_name.trim();
    if trimmed.is_empty() {
        return;
    }

    if !metadata.is_object() {
        *metadata = serde_json::Value::Object(serde_json::Map::new());
    }

    if let Some(map) = metadata.as_object_mut() {
        map.insert(
            SESSION_ACTIVE_SKILL_NAME_KEY.to_string(),
            serde_json::Value::String(trimmed.to_string()),
        );
        map.insert(
            SESSION_ACTIVE_SKILL_CORRECTIONS_KEY.to_string(),
            serde_json::Value::Number(0.into()),
        );
    }
}

fn disable_skill_toggle(paths: &Paths, skill_name: &str) -> Result<()> {
    let path = paths.toggles_file();
    let mut store = std::fs::read_to_string(&path)
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .unwrap_or_else(|| serde_json::json!({"skills": {}, "tools": {}}));
    if !store.is_object() {
        store = serde_json::json!({"skills": {}, "tools": {}});
    }
    if store
        .get("skills")
        .and_then(|value| value.as_object())
        .is_none()
    {
        store["skills"] = serde_json::json!({});
    }
    store["skills"][skill_name] = serde_json::json!(false);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&store)?)?;
    Ok(())
}

fn suppress_prompt_reinjection_for_continued_skill(
    mut active_skill: ActiveSkillContext,
    continued_skill_name: Option<&str>,
) -> ActiveSkillContext {
    if continued_skill_name == Some(active_skill.name.as_str()) {
        active_skill.inject_prompt_md = false;
    }
    active_skill
}

fn apply_skill_fallback_response(final_response: String, fallback_message: Option<&str>) -> String {
    let trimmed_response = final_response.trim();
    if !trimmed_response.is_empty() {
        return trimmed_response.to_string();
    }

    fallback_message
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_string)
        .unwrap_or_default()
}

pub(crate) struct PromptSkillLoopOutput {
    final_response: String,
    trace_messages: Vec<ChatMessage>,
}

fn resolve_skill_run_mode(msg: &InboundMessage) -> SkillRunMode {
    match msg
        .metadata
        .get("skill_run_mode")
        .and_then(|value| value.as_str())
    {
        Some("test") => SkillRunMode::Test,
        Some("cron") => SkillRunMode::Cron,
        Some("chat") => SkillRunMode::Chat,
        _ if msg.channel == "cron" => SkillRunMode::Cron,
        _ if msg
            .metadata
            .get("skill_test")
            .and_then(|value| value.as_bool())
            .unwrap_or(false) =>
        {
            SkillRunMode::Test
        }
        _ => SkillRunMode::Chat,
    }
}

fn resolve_cron_deliver_target(msg: &InboundMessage) -> Option<(String, String)> {
    if resolve_skill_run_mode(msg) != SkillRunMode::Cron {
        return None;
    }

    if !msg
        .metadata
        .get("deliver")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return None;
    }

    let channel = msg
        .metadata
        .get("deliver_channel")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let to = msg
        .metadata
        .get("deliver_to")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())?;

    Some((channel.to_string(), to.to_string()))
}

fn expand_history_stubs_with_cache(
    response_cache: &crate::response_cache::ResponseCache,
    session_key: &str,
    history: &[ChatMessage],
) -> Vec<ChatMessage> {
    history
        .iter()
        .map(|msg| {
            let content_str = msg.content.as_str().unwrap_or("");
            if content_str.contains("ref:") {
                if let Some(ref_pos) = content_str.find("ref:") {
                    let after = &content_str[ref_pos + 4..];
                    let ref_id: String = after
                        .chars()
                        .take_while(|c| c.is_ascii_hexdigit())
                        .collect();
                    if !ref_id.is_empty() {
                        if let Some(full) = response_cache.recall(session_key, &ref_id) {
                            let mut expanded = msg.clone();
                            expanded.content = serde_json::Value::String(full);
                            return expanded;
                        }
                    }
                }
            }
            msg.clone()
        })
        .collect()
}

static FORCED_SKILL_REQUEST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:使用(?:已安装的)?|用|调用|执行|use|using|run|call)\s*([A-Za-z0-9_.@-]+)\s*(?:技能|skill)\s*[：:\-，,]?\s*(.*)",
    )
    .expect("forced skill request regex is valid")
});

fn parse_spawn_task_forced_skill_request(task: &str) -> Option<(String, String)> {
    let trimmed = task.trim();
    if trimmed.is_empty() {
        return None;
    }

    let captures = FORCED_SKILL_REQUEST_RE.captures(trimmed)?;
    let skill_name = captures.get(1)?.as_str().trim().to_string();
    if skill_name.is_empty() {
        return None;
    }
    let remainder = captures
        .get(2)
        .map(|m| m.as_str().trim())
        .filter(|text| !text.is_empty())
        .unwrap_or(trimmed)
        .to_string();

    Some((skill_name, remainder))
}

fn normalize_spawn_task(task: &str) -> String {
    if let Some((skill_name, user_query)) = parse_spawn_task_forced_skill_request(task) {
        format!("__SKILL_EXEC__:{}:{}", skill_name, user_query)
    } else {
        task.to_string()
    }
}

/// Prepare skill result for presentation.
#[allow(dead_code)]
struct SkillResultPresentation {
    direct_text: Option<String>,
    llm_payload: Option<String>,
    fallback_text: String,
}

#[allow(dead_code)]
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

    let direct_text = obj
        .get("display_text")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
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

    let fallback_text = if let Some(text) = direct_text.as_ref() {
        text.clone()
    } else if let Some(summary) = obj.get("summary_data") {
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
        direct_text,
        llm_payload: Some(truncate_str(&llm_payload, 16000)),
        fallback_text,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MainSessionTarget {
    channel: String,
    account_id: Option<String>,
    chat_id: String,
    session_key: String,
    agent_id: Option<String>,
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

fn parse_structured_skill_task(task: &str) -> Option<(&str, &str)> {
    let rest = task.strip_prefix("__SKILL_EXEC__:")?;
    let (skill_name, user_query) = rest.split_once(':')?;
    let skill_name = skill_name.trim();
    if skill_name.is_empty() {
        return None;
    }
    Some((skill_name, user_query))
}

fn build_subagent_inbound_message(
    task: &str,
    origin_channel: &str,
    origin_chat_id: &str,
    base_metadata: &serde_json::Value,
    session_key: &str,
) -> InboundMessage {
    let mut metadata = if let Some(obj) = base_metadata.as_object() {
        serde_json::Value::Object(obj.clone())
    } else {
        serde_json::json!({})
    };

    if let Some(obj) = metadata.as_object_mut() {
        obj.insert(
            "subagent_session_key".to_string(),
            serde_json::json!(session_key),
        );

        if let Some((skill_name, _)) = parse_structured_skill_task(task) {
            obj.insert(
                "forced_skill_name".to_string(),
                serde_json::json!(skill_name),
            );
        }
    }

    let content = parse_structured_skill_task(task)
        .map(|(_, user_query)| user_query.to_string())
        .unwrap_or_else(|| task.to_string());

    InboundMessage {
        channel: origin_channel.to_string(),
        account_id: None,
        sender_id: "system".to_string(),
        chat_id: origin_chat_id.to_string(),
        content,
        media: vec![],
        metadata,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    }
}

fn global_core_tool_names() -> Vec<String> {
    blockcell_tools::registry::GLOBAL_CORE_TOOL_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .chain(std::iter::once(
            blockcell_tools::mcp::search::MCP_SEARCH_TOOL_NAME.to_string(),
        ))
        .chain(std::iter::once("tool_search".to_string()))
        .collect()
}

const CODING_DEFAULT_TOOL_NAMES: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "list_dir",
    "grep",
    "glob",
    "shell_run",
    "shell_poll",
    "shell_input",
    "shell_close",
    "job_output",
    "job_kill",
    "tool_search",
    "update_plan",
    "file_ops",
    "get_context_remaining",
    "web_search",
];

fn replace_legacy_shell_tool(tool_names: &mut Vec<String>, available_tools: &HashSet<String>) {
    if !tool_names.iter().any(|name| name == "shell") {
        return;
    }
    tool_names.retain(|name| name != "shell");
    for name in ["shell_run", "shell_poll", "shell_input", "shell_close"] {
        if available_tools.contains(name) && !tool_names.iter().any(|current| current == name) {
            tool_names.push(name.to_string());
        }
    }
}

fn shell_runs_unattended_for_backend(
    backend: SandboxBackend,
    approval_policy: ApprovalPolicy,
) -> bool {
    match backend {
        SandboxBackend::FullAccess => approval_policy == ApprovalPolicy::WorkspaceAuto,
        SandboxBackend::MacOsSeatbelt
        | SandboxBackend::LinuxBubblewrap
        | SandboxBackend::LinuxLandlockSeccomp => true,
        SandboxBackend::ApprovalRequired => false,
    }
}

fn shell_runs_unattended(config: &Config) -> bool {
    let Ok(policy) = SandboxPolicy::parse(config.tools.exec.sandbox_policy.as_deref()) else {
        return false;
    };
    let Ok(approval_policy) = ApprovalPolicy::parse(config.tools.exec.approval_policy.as_deref())
    else {
        return false;
    };
    shell_runs_unattended_for_backend(native_backend(policy), approval_policy)
}

fn apply_coding_tool_defaults(
    config: &Config,
    mode: InteractionMode,
    agent_id: Option<&str>,
    available_tools: &HashSet<String>,
    tool_names: &mut Vec<String>,
    user_message: &str,
) {
    if !matches!(mode, InteractionMode::Coding) {
        return;
    }
    if shell_runs_unattended(config) {
        tool_names.retain(|name| name != "exec");
    }
    let denied = config
        .intent_router
        .as_ref()
        .and_then(|router| {
            config
                .resolve_intent_profile_id(agent_id)
                .and_then(|profile_id| router.profiles.get(&profile_id))
        })
        .map(|profile| profile.deny_tools.as_slice())
        .unwrap_or(&[]);
    for name in CODING_DEFAULT_TOOL_NAMES {
        if available_tools.contains(*name)
            && !denied.iter().any(|denied_name| denied_name == name)
            && !tool_names.iter().any(|tool_name| tool_name == name)
        {
            tool_names.push((*name).to_string());
        }
    }
    const NICHE_TOOLS: &[&str] = &[
        "termux_api",
        "alert_rule",
        "video_process",
        "knowledge_graph",
        "stream_subscribe",
        "encrypt",
        "email",
        "camera_capture",
        "app_control",
        "tts",
        "ocr",
        "audio_transcribe",
        "image_understand",
        "chart_generate",
        "network_monitor",
        "community_hub",
        "office_write",
        "browse",
    ];
    for name in NICHE_TOOLS {
        if !user_message.contains(name) {
            tool_names.retain(|tool_name| tool_name != name);
        }
    }
}

fn normalize_ghost_memory_provider_tool_schema(
    schema: serde_json::Value,
) -> Option<serde_json::Value> {
    if schema.get("type").and_then(|value| value.as_str()) == Some("function") {
        let name = schema
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(|value| value.as_str())?;
        if !name.trim().is_empty() {
            return Some(schema);
        }
        return None;
    }

    let name = schema.get("name").and_then(|value| value.as_str())?.trim();
    if name.is_empty() {
        return None;
    }
    let description = schema
        .get("description")
        .and_then(|value| value.as_str())
        .unwrap_or("Ghost memory provider tool.");
    let parameters = schema
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));

    Some(serde_json::json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters,
        }
    }))
}

fn ghost_memory_provider_tool_schemas(
    manager: Option<&crate::ghost_memory_provider::GhostMemoryProviderManager>,
    disabled_tools: &HashSet<String>,
) -> Vec<serde_json::Value> {
    manager
        .map(|manager| {
            manager
                .get_all_tool_schemas()
                .into_iter()
                .filter_map(normalize_ghost_memory_provider_tool_schema)
                .filter(|schema| {
                    let name = schema
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(|value| value.as_str())
                        .unwrap_or_default();
                    !disabled_tools.contains(name)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn resolve_effective_tool_names(
    config: &Config,
    mode: InteractionMode,
    agent_id: Option<&str>,
    active_skill: Option<&ActiveSkillContext>,
    intents: &[IntentCategory],
    available_tools: &HashSet<String>,
    user_message: &str,
) -> Vec<String> {
    // 1. 先检查 intent_router.enabled
    let router_enabled = config
        .intent_router
        .as_ref()
        .map(|r| r.enabled)
        .unwrap_or(true);

    if !router_enabled {
        // 2. enabled=false 时，检查 load_all_tools
        let load_all = config
            .intent_router
            .as_ref()
            .map(|r| r.load_all_tools)
            .unwrap_or(false);

        if load_all {
            // 全量加载模式：返回所有可用工具（扣除 deny_tools）
            let mut tool_names: Vec<String> = available_tools.iter().cloned().collect();
            // 应用 deny_tools 过滤
            if let Some(router) = config.intent_router.as_ref() {
                let profile_id = config.resolve_intent_profile_id(agent_id);
                if let Some(profile_id) = profile_id {
                    if let Some(profile) = router.profiles.get(&profile_id) {
                        for tool in &profile.deny_tools {
                            tool_names.retain(|name| name != tool);
                        }
                    } else {
                        warn!(
                            profile_id = %profile_id,
                            "Profile not found in load_all_tools mode, deny_tools filtering skipped"
                        );
                    }
                }
            }
            // 应用 napcat 过滤
            if !config.channels.napcat.enabled {
                tool_names.retain(|name| !name.starts_with("napcat_"));
            }
            // 应用 skill 工具（如果有 active skill）
            if let Some(skill) = active_skill {
                tool_names.extend(skill.tools.iter().cloned());
            }
            apply_coding_tool_defaults(
                config,
                mode,
                agent_id,
                available_tools,
                &mut tool_names,
                user_message,
            );
            replace_legacy_shell_tool(&mut tool_names, available_tools);
            tool_names.sort();
            tool_names.dedup();
            return tool_names;
        }
        // load_all_tools=false: 走 Unknown profile（原有逻辑会处理）
    }

    // enabled=true 或 load_all_tools=false: 原有意图分类逻辑不变
    let mut tool_names = global_core_tool_names();

    let mut profile_tools = match mode {
        InteractionMode::Chat => {
            resolve_profile_tool_names(config, agent_id, &[IntentCategory::Chat], available_tools)
        }
        InteractionMode::General | InteractionMode::Coding | InteractionMode::Skill => {
            resolve_profile_tool_names(config, agent_id, intents, available_tools)
        }
    };

    tool_names.append(&mut profile_tools);

    if let Some(skill) = active_skill {
        tool_names.extend(skill.tools.iter().cloned());
    }

    // Filter by available tools (registry)
    tool_names.retain(|name| available_tools.contains(name));

    // Filter napcat tools by config enabled state
    if !config.channels.napcat.enabled {
        tool_names.retain(|name| !name.starts_with("napcat_"));
    }

    apply_coding_tool_defaults(
        config,
        mode,
        agent_id,
        available_tools,
        &mut tool_names,
        user_message,
    );

    replace_legacy_shell_tool(&mut tool_names, available_tools);

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

// scoped_tool_denied_result moved to crate::error

pub(super) fn is_path_within_base(base: &Path, candidate: &Path) -> bool {
    let base_norm = resolve_for_policy(base);
    let candidate_norm = resolve_for_policy(candidate);
    candidate_norm.starts_with(&base_norm)
}

/// 工具调用是否未成功：以 Runtime 判定的 [`ToolOutcome`] 为准；工具以 `Ok` 返回
/// `{"error": ...}` / `{"status": "error"}` 的软失败同样计为未成功。
fn tool_call_failed(output: &ToolCallOutput) -> bool {
    output.outcome.is_error() || tool_result_indicates_error(&output.text)
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
        if value
            .get("error")
            .is_some_and(|error| !error.is_null() && error != &serde_json::Value::Bool(false))
        {
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

fn extract_mcp_search_revealed_tools(result: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(result) else {
        return Vec::new();
    };

    let mut seen = HashSet::new();
    value
        .get("tools")
        .and_then(|tools| tools.as_array())
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.get("name").and_then(|name| name.as_str()))
        .map(str::trim)
        .filter(|name| {
            let Some((server, tool)) = name.split_once("__") else {
                return false;
            };
            !server.trim().is_empty() && !tool.trim().is_empty()
        })
        .map(str::to_string)
        .filter(|name| seen.insert(name.clone()))
        .collect()
}

#[derive(Debug, Clone)]
pub(crate) struct InteractionDecision {
    active_skill: Option<ActiveSkillContext>,
    chat_intents: Vec<IntentCategory>,
    mode: InteractionMode,
}

/// 会话持久化归属：cron 投递消息归属到投递目标会话，其余归属到来源会话。
/// 历史、事件日志与 hooks 都以它作为会话标识，保证同一回合的数据落在同一会话。
pub(crate) fn persist_session_key_for(msg: &InboundMessage) -> String {
    match resolve_cron_deliver_target(msg) {
        Some((channel, to)) => blockcell_core::build_session_key(&channel, &to),
        None => msg.session_key(),
    }
}

pub(crate) struct FinalResponseContext<'a> {
    msg: &'a InboundMessage,
    persist_session_key: &'a str,
    history: &'a mut [ChatMessage],
    session_metadata: &'a serde_json::Value,
    final_response: &'a str,
    collected_media: Vec<String>,
    cron_deliver_target: Option<(String, String)>,
}

#[cfg(test)]
fn determine_interaction_mode(
    has_active_skill: bool,
    chat_intents: &[IntentCategory],
) -> InteractionMode {
    if has_active_skill {
        return InteractionMode::Skill;
    }

    if chat_intents.len() == 1 && matches!(chat_intents[0], IntentCategory::Chat) {
        return InteractionMode::Chat;
    }

    if chat_intents.contains(&IntentCategory::Coding) {
        return InteractionMode::Coding;
    }

    InteractionMode::General
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

#[derive(Clone)]
struct RuntimeSessionSearch {
    paths: Paths,
    current_session_key: Option<String>,
}

impl RuntimeSessionSearch {
    fn new(paths: Paths, current_session_key: Option<String>) -> Self {
        Self {
            paths,
            current_session_key,
        }
    }
}

impl SessionSearchOps for RuntimeSessionSearch {
    fn search_session_json(&self, query: &str, limit: usize) -> Result<serde_json::Value> {
        let tokens = normalize_runtime_session_search_tokens(query);
        if tokens.is_empty() {
            return Ok(serde_json::json!({
                "query": query,
                "count": 0,
                "results": []
            }));
        }

        let mut results = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.paths.sessions_dir()) else {
            return Ok(serde_json::json!({
                "query": query,
                "count": 0,
                "results": []
            }));
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            let session_key = path
                .file_stem()
                .and_then(|value| value.to_str())
                .map(|stem| stem.replace('_', ":"))
                .unwrap_or_else(|| "unknown".to_string());
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            for line in BufReader::new(file).lines().map_while(|line| line.ok()) {
                let Ok(message) = serde_json::from_str::<ChatMessage>(&line) else {
                    continue;
                };
                if !matches!(message.role.as_str(), "user" | "assistant") {
                    continue;
                }
                let text = chat_message_text(&message);
                let score = runtime_session_search_score(&text, &tokens);
                if score == 0 {
                    continue;
                }
                let current_boost = self
                    .current_session_key
                    .as_ref()
                    .is_some_and(|current| current == &session_key)
                    as usize;
                results.push((
                    score,
                    current_boost,
                    session_key.clone(),
                    message.role,
                    truncate_runtime_session_search_text(&text, 500),
                ));
            }
        }

        results.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.2.cmp(&right.2))
        });
        results.truncate(limit.clamp(1, 20));
        Ok(serde_json::json!({
            "query": query,
            "count": results.len(),
            "results": results
                .into_iter()
                .map(|(score, _current_boost, session_key, role, text)| serde_json::json!({
                    "score": score,
                    "sessionKey": session_key,
                    "role": role,
                    "text": text,
                }))
                .collect::<Vec<_>>()
        }))
    }
}

fn normalize_runtime_session_search_tokens(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric())
        .map(|token| token.trim().to_lowercase())
        .filter(|token| token.len() >= 2)
        .collect()
}

fn runtime_session_search_score(text: &str, tokens: &[String]) -> usize {
    let lower = text.to_lowercase();
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

fn truncate_runtime_session_search_text(text: &str, max_chars: usize) -> String {
    let truncated = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        format!("{truncated}...")
    } else {
        truncated
    }
}

static IMAGE_ABS_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(/[^\s`"']+\.(?i:jpg|jpeg|png|gif|webp|bmp))"#)
        .expect("image abs path regex is valid")
});
static IMAGE_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"([A-Za-z0-9._-]+\.(?i:jpg|jpeg|png|gif|webp|bmp))"#)
        .expect("image name regex is valid")
});

async fn pick_image_path(paths: &Paths, history: &[ChatMessage]) -> Option<String> {
    let re_abs = &*IMAGE_ABS_PATH_RE;
    let re_name = &*IMAGE_NAME_RE;

    let media_dir = paths.media_dir();

    for msg in history.iter().rev() {
        let text = chat_message_text(msg);

        for cap in re_abs.captures_iter(&text) {
            let p = cap.get(1)?.as_str().to_string();
            if tokio::fs::metadata(&p).await.is_ok() {
                // 使用异步 canonicalize 避免阻塞 tokio runtime
                let cp = tokio::fs::canonicalize(&p).await.ok()?;
                let md = tokio::fs::canonicalize(&media_dir).await.ok()?;
                if cp.starts_with(md) {
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

type TogglesCache = HashMap<PathBuf, (std::time::SystemTime, u64, serde_json::Value)>;

#[derive(Default)]
struct FileObservationState {
    entries: HashMap<(String, PathBuf), FileVersion>,
    order: VecDeque<(String, PathBuf)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileVersion {
    len: u64,
    modified: SystemTime,
}

/// Cached parsed toggles.json keyed by file mtime+len: per-call sites on the tool
/// hot path only pay a `stat` syscall unless the file actually changed.
fn cached_toggles_json(path: &Path) -> Option<serde_json::Value> {
    static CACHE: LazyLock<Mutex<TogglesCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let len = meta.len();
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, l, v)) = cache.get(path) {
        if *t == mtime && *l == len {
            return Some(v.clone());
        }
    }
    let val =
        serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(path).ok()?).ok()?;
    if cache.len() >= 16 {
        cache.clear();
    }
    cache.insert(path.to_path_buf(), (mtime, len, val.clone()));
    Some(val)
}

/// Read toggles.json and return the set of disabled item names for a category.
/// Returns an empty set if the file doesn't exist or can't be parsed.
#[cfg(test)]
fn load_disabled_toggles(paths: &Paths, category: &str) -> HashSet<String> {
    let mut disabled = HashSet::new();
    if let Some(val) = cached_toggles_json(&paths.toggles_file()) {
        collect_disabled(&val, category, &mut disabled);
    }
    disabled
}

/// Collect the names disabled under `category` from a parsed toggles document.
fn collect_disabled(val: &serde_json::Value, category: &str, out: &mut HashSet<String>) {
    if let Some(obj) = val.get(category).and_then(|v| v.as_object()) {
        for (name, enabled) in obj {
            if enabled == false {
                out.insert(name.clone());
            }
        }
    }
}

/// Read toggles.json once and return the disabled item sets for two categories.
/// Avoids reading and parsing the same file twice back-to-back on the message hot path.
fn load_disabled_toggles_pair(
    paths: &Paths,
    category_a: &str,
    category_b: &str,
) -> (HashSet<String>, HashSet<String>) {
    let mut a = HashSet::new();
    let mut b = HashSet::new();
    if let Some(val) = cached_toggles_json(&paths.toggles_file()) {
        collect_disabled(&val, category_a, &mut a);
        collect_disabled(&val, category_b, &mut b);
    }
    (a, b)
}

pub struct AgentRuntime {
    config: Config,
    paths: Paths,
    context_builder: ContextBuilder,
    provider_pool: Arc<ProviderPool>,
    /// Layer4 压缩时生成摘要的策略。
    context_compactor: Arc<dyn crate::compact::ContextCompactor>,
    /// agent/pre-step 中内置策略之后运行的可注册策略链。
    pre_step_policies: crate::runtime_kernel::WaterfallChain<pre_step::PreStepState>,
    tool_registry: ToolRegistry,
    session_store: SessionStore,
    audit_logger: Arc<Mutex<AuditLogger>>,
    outbound_tx: Option<mpsc::Sender<OutboundMessage>>,
    inbound_tx: Option<mpsc::Sender<InboundMessage>>,
    confirm_tx: Option<mpsc::Sender<ConfirmRequest>>,
    /// Directories that the user has already authorized access to.
    /// Files within these directories will not require separate confirmation.
    /// Entries are keyed by session key so an authorization granted in one
    /// session cannot leak into another session/user on a shared runtime.
    authorized_dirs: Arc<Mutex<HashSet<(String, PathBuf, blockcell_core::path_policy::PathOp)>>>,
    file_observations: Arc<Mutex<FileObservationState>>,
    /// 非交互通道的文本确认消费记录：用户原文的"确认执行"只允许放行**一个**
    /// 危险操作，而非本回合所有危险操作（逐操作确认在无交互通道的最强近似）。
    /// 键 = channel + sender + timestamp_ms，按消息去重；跨 clone 共享。
    consumed_text_confirmations: Arc<Mutex<HashSet<String>>>,
    /// Shared task manager for tracking background subagent tasks.
    task_manager: TaskManager,
    /// Agent id bound to this runtime.
    agent_id: Option<String>,
    /// Shared memory store handle for tools.
    memory_store: Option<MemoryStoreHandle>,
    memory_file_store: Option<blockcell_tools::MemoryFileStoreHandle>,
    ghost_memory_lifecycle: Option<Arc<crate::ghost_memory_provider::GhostMemoryProviderManager>>,
    skill_file_store: Option<blockcell_tools::SkillFileStoreHandle>,
    /// Capability registry handle for tools.
    capability_registry: Option<CapabilityRegistryHandle>,
    /// Core evolution engine handle for tools.
    core_evolution: Option<CoreEvolutionHandle>,
    /// 核心进化工作流 worker — 后台独立运行，tick 只做轻量 notify
    evolution_worker: Option<Arc<dyn crate::capability_adapter::EvolutionNotifier>>,
    /// Skill evolution worker — runtime only triggers and notifies it.
    skill_evolution_worker: Option<Arc<dyn crate::capability_adapter::EvolutionNotifier>>,
    /// 核心进化工作流存储 — 快速入队，不拿 engine mutex
    evolution_workflow_store: Option<Arc<blockcell_storage::EvolutionWorkflowStore>>,
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
    /// Shared reference to main_session_target, also held by LightweightRuntimeHandle.
    /// When update_main_session_target() is called, both are updated so the handle
    /// always sees the current session info (not the stale None from init time).
    shared_session_target: Arc<std::sync::RwLock<Option<MainSessionTarget>>>,
    /// Cooldown tracker: capability_id → last auto-request timestamp (epoch secs).
    /// Prevents repeated auto-triggering of the same capability within 24h.
    cap_request_cooldown: HashMap<String, i64>,
    /// Persistent registry of known channel contacts for cross-channel messaging.
    channel_contacts: blockcell_storage::ChannelContacts,
    /// Loaded path-access policy engine (from `~/.blockcell/path_access.json5`).
    path_policy: PathPolicy,
    /// Loaded tool-call policy engine (from `~/.blockcell/tool_policy.yaml`).
    tool_policy: ToolPolicy,
    /// Runtime event bus: hooks (`~/.blockcell/hooks.yaml`) and other listeners subscribe here.
    event_bus: EventBus,
    /// Shared WebSocket sender slot read by the WS tool event listener.
    event_tx_slot: event_listeners::EventTxSlot,
    /// Current turn / step identity attached to every dispatched runtime event.
    current_turn_id: Option<String>,
    current_step: Option<u32>,
    /// Steps already started in the current turn; nested skill loops share it so step ids stay unique.
    turn_step_counter: u32,
    /// Evolution candidate scope this runtime is evaluating (`None` in production).
    candidate_scope: Option<String>,
    /// Loop driver for each turn; defaults to the built-in loop.
    agent_loop: Arc<dyn agent_loop::AgentLoop>,
    /// Per-session cache for large list/table responses (prevents history token explosion).
    response_cache: crate::response_cache::ResponseCache,
    /// 7-Layer Memory System integration.
    memory_system: Option<crate::memory_system::MemorySystem>,
    /// Shared session-indexed MemorySystem pool so per-message forked runtimes
    /// preserve Layer 4 trackers (FileTracker/SkillTracker) and avoid re-running init_memory_system I/O.
    session_memory_systems: Arc<Mutex<MemorySystemPool>>,
    /// Flag to signal that memory injector cache needs refresh after Layer 5 extraction.
    /// Uses Arc<AtomicBool> because background tasks need to set this flag.
    memory_injector_needs_reload: Arc<std::sync::atomic::AtomicBool>,
    /// Prevent repeated Layer 5 initialization attempts after a persistent
    /// load failure; an explicit reload signal clears this circuit breaker.
    memory_injector_init_failed: Arc<std::sync::atomic::AtomicBool>,
    /// AbortToken for cancelling this runtime and its sub-agents.
    abort_token: AbortToken,
    /// Self-referential handle for the agent tool (RuntimeHandle trait object).
    /// Set via `set_runtime_handle()` after construction.
    runtime_handle: Option<blockcell_tools::AgentRuntimeHandle>,
    /// Skill Nudge 引擎 — 跟踪工具使用次数并在阈值到达时触发 Skill Review
    /// Unified learning coordinator — replaces scattered skill_nudge_engine + ghost_policy calls
    learning_coordinator: Arc<crate::learning_coordinator::LearningCoordinator>,
    /// Skill 操作互斥锁 — 通过 WriteGuard 提供，防止 Skill 并发修改冲突
    skill_mutex: blockcell_tools::SkillMutexHandle,
    /// Agent type registry — 共享的 agent 类型定义，避免每次调用重建
    agent_type_registry: crate::agent_types::AgentTypeRegistry,
    /// Unified write guard for coordinated write protection across memory + skill files
    write_guard: Arc<crate::write_guard::WriteGuard>,
    /// Token and cost budget trackers keyed by persisted session key.
    budget_trackers: Arc<Mutex<HashMap<String, BudgetTrackerHandle>>>,
    /// 子 Agent 运行时挂载的父会话预算；`None` 表示顶层会话。挂载后子消耗归并到父。
    parent_budget: Option<BudgetTrackerHandle>,
    /// Receives real-time user interjections for the currently running message task.
    steering: SteeringChannel,
    /// Cloneable send handle exposed to gateway/WS routing.
    steering_sender: SteeringSender,
    /// Shared gateway-visible registry of active steering senders.
    active_steering_registry: Option<SteeringRegistry>,
    background_tasks_enabled: bool,
    background_task_handles: Vec<tokio::task::JoinHandle<()>>,
    last_verification_command: Option<String>,
}

impl AgentRuntime {
    pub fn set_background_tasks_enabled(&mut self, enabled: bool) {
        self.background_tasks_enabled = enabled;
        self.context_builder.set_ghost_learning_enabled(
            enabled
                && self.config.memory.learning_system.enabled()
                && self.config.agents.ghost.learning.capture_enabled(),
        );
    }

    pub fn set_shell_execution_unavailable(&mut self, unavailable: bool) {
        self.context_builder
            .set_shell_execution_unavailable(unavailable);
    }

    pub fn last_verification_command(&self) -> Option<&str> {
        self.last_verification_command.as_deref()
    }

    pub async fn wait_for_background_tasks(&mut self) {
        let handles = std::mem::take(&mut self.background_task_handles);
        let wait = async move {
            for handle in handles {
                let _ = handle.await;
            }
        };
        if tokio::time::timeout(std::time::Duration::from_secs(30), wait)
            .await
            .is_err()
        {
            warn!("Timed out waiting for background tasks after 30 seconds");
        }
    }

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
        let audit_logger = Arc::new(Mutex::new(AuditLogger::new(paths.clone())));
        let channel_contacts = blockcell_storage::ChannelContacts::new(paths.clone());
        let path_policy = load_path_policy(&config, &paths);
        let tool_policy = ToolPolicy::load(&paths.base.join("tool_policy.yaml"));
        let event_bus = EventBus::new();
        let hook_manager = HookManager::load(&paths.base.join("hooks.yaml"));
        if !hook_manager.is_empty() {
            event_bus.register(Arc::new(HookAdapter::new(
                hook_manager,
                paths.workspace().display().to_string(),
            )));
        }
        event_bus.register(Arc::new(event_listeners::SessionEventRecorder::new(
            Arc::new(blockcell_storage::JsonlSessionEventLog::new(paths.clone())),
        )));
        let event_tx_slot: event_listeners::EventTxSlot = Arc::default();
        event_bus.register(Arc::new(event_listeners::WsToolEventListener::new(
            event_tx_slot.clone(),
        )));
        let system_event_store = InMemorySystemEventStore::with_persistence(
            paths.workspace().join(".system_events.json"),
        )?;
        let summary_queue = MainSessionSummaryQueue::with_persistence(
            5,
            config.tools.tick_interval_secs.clamp(10, 300) as i64 * 1000,
            paths.workspace().join(".system_summary_queue.json"),
        )?;
        let system_event_orchestrator =
            SystemEventOrchestrator::new(system_event_store.clone(), summary_queue.clone());
        let system_event_emitter: EventEmitterHandle = Arc::new(RuntimeSystemEventEmitter {
            store: system_event_store.clone(),
        });
        let ghost_memory_lifecycle = Arc::new(
            crate::ghost_memory_provider::GhostMemoryProviderManager::with_local_file(
                paths.clone(),
            ),
        );
        ghost_memory_lifecycle.initialize_all("runtime", "primary");

        // 构建 Skill 索引摘要并注入到系统提示词
        let skills_dir = paths.skills_dir();
        if skills_dir.exists() {
            let index = crate::skill_index::SkillIndex::build_from_dir(&skills_dir);
            if !index.entries().is_empty() {
                context_builder.set_skill_index_summary(index.to_prompt_summary());
            }
        }

        // 从 config 中提取 nudge 配置 (在 config 被 move 之前)
        let nudge_config = crate::skill_nudge::NudgeConfig::from_config(&config.self_improve.nudge);

        let response_cache_config =
            crate::response_cache::ResponseCacheConfig::from(&config.memory.memory_system.layer1);

        // Extract config values before config is moved into Self
        let ghost_learning_enabled = config.agents.ghost.learning.capture_enabled();
        let self_improve_review_enabled = config.self_improve.review.enabled;
        let ghost_learning_config = config.agents.ghost.learning.clone();

        // Create unified write guard for coordinated write protection
        let write_guard = Arc::new(crate::write_guard::WriteGuard::new(paths.base.clone()));

        // 加载 Agent 类型注册表 (从多种来源: Built-in → User-level → Project-level)
        let agent_type_registry = {
            let workspace = paths.workspace();
            let loader = crate::agent_loader::AgentDefinitionLoader::new(
                &paths.base,
                Some(&workspace),
                Some(&workspace),
            );
            loader.load_all()
        };

        let budget_trackers = Arc::new(Mutex::new(HashMap::new()));
        let (steering, steering_sender) = SteeringChannel::new(16);

        let mut runtime = Self {
            config,
            paths,
            context_builder,
            provider_pool,
            context_compactor: Arc::new(crate::compact::ModelSummaryCompactor),
            pre_step_policies: crate::runtime_kernel::WaterfallChain::new(),
            tool_registry,
            session_store,
            audit_logger,
            outbound_tx: None,
            inbound_tx: None,
            confirm_tx: None,
            authorized_dirs: Arc::new(Mutex::new(HashSet::new())),
            file_observations: Arc::new(Mutex::new(FileObservationState::default())),
            consumed_text_confirmations: Arc::new(Mutex::new(HashSet::new())),
            task_manager: TaskManager::new(),
            agent_id: None,
            memory_store: None,
            memory_file_store: None,
            ghost_memory_lifecycle: Some(ghost_memory_lifecycle),
            skill_file_store: None,
            capability_registry: None,
            core_evolution: None,
            evolution_worker: None,
            skill_evolution_worker: None,
            evolution_workflow_store: None,
            event_tx: None,
            system_event_store,
            system_event_orchestrator,
            system_event_emitter,
            main_session_target: None,
            shared_session_target: Arc::new(std::sync::RwLock::new(None)),
            cap_request_cooldown: HashMap::new(),
            channel_contacts,
            path_policy,
            tool_policy,
            event_bus,
            event_tx_slot,
            current_turn_id: None,
            current_step: None,
            turn_step_counter: 0,
            candidate_scope: None,
            agent_loop: Arc::new(agent_loop::DefaultLoop),
            response_cache: crate::response_cache::ResponseCache::with_config(
                response_cache_config,
            ),
            memory_system: None,
            session_memory_systems: Arc::new(Mutex::new(MemorySystemPool::new())),
            memory_injector_needs_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            memory_injector_init_failed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            abort_token: AbortToken::new(),
            runtime_handle: None,
            agent_type_registry,
            learning_coordinator: Arc::new({
                let nudge_engine = crate::skill_nudge::SkillNudgeEngine::new(nudge_config);
                let ghost_policy =
                    crate::ghost_learning::GhostLearningPolicy::from_config(&ghost_learning_config);
                let throttle = crate::learning_throttle::LearningThrottle::new(2, 300);
                let dedup = crate::learning_dedup::LearningDedup::new(600);
                crate::learning_coordinator::LearningCoordinator::new(
                    nudge_engine,
                    ghost_policy,
                    throttle,
                    dedup,
                    ghost_learning_enabled,
                    self_improve_review_enabled,
                )
            }),
            // 使用 WriteGuard 作为 SkillMutexHandle，替换已废弃的 SkillMutex
            skill_mutex: write_guard.clone() as blockcell_tools::SkillMutexHandle,
            write_guard,
            budget_trackers,
            parent_budget: None,
            steering,
            steering_sender,
            active_steering_registry: None,
            background_tasks_enabled: true,
            background_task_handles: Vec::new(),
            last_verification_command: None,
        };
        runtime.sync_tool_scope_view("");
        Ok(runtime)
    }

    /// 从长生命周期父 `AgentRuntime` 派生每消息执行器。
    ///
    /// 共享所有昂贵构造产物、有状态缓存（`SessionContextCache`、`ResponseCache`、
    /// `skill_index_summary`、`authorized_dirs`、`budget_trackers`）与已装配能力
    /// （`skill_evolution_worker`、`evolution_workflow_store`、`evolution_provider`、
    /// `memory_file_store`、`skill_file_store`、`memory_injector`），
    /// 同时保持单消息执行状态（`abort_token`、`steering`、`current_turn_id`/`current_step`）严格隔离。
    pub(crate) fn fork_for_message_task(
        &self,
        steering: SteeringChannel,
        steering_sender: SteeringSender,
        abort_token: AbortToken,
    ) -> Self {
        let mut runtime = Self {
            config: self.config.clone(),
            paths: self.paths.clone(),
            context_builder: self.context_builder.clone(),
            provider_pool: Arc::clone(&self.provider_pool),
            context_compactor: Arc::clone(&self.context_compactor),
            pre_step_policies: self.pre_step_policies.clone(),
            tool_registry: self.tool_registry.clone(),
            session_store: SessionStore::new(self.paths.clone()),
            audit_logger: Arc::new(Mutex::new(AuditLogger::new(self.paths.clone()))),
            outbound_tx: self.outbound_tx.clone(),
            inbound_tx: self.inbound_tx.clone(),
            confirm_tx: self.confirm_tx.clone(),
            authorized_dirs: Arc::clone(&self.authorized_dirs),
            file_observations: Arc::clone(&self.file_observations),
            consumed_text_confirmations: Arc::clone(&self.consumed_text_confirmations),
            task_manager: self.task_manager.clone(),
            agent_id: self.agent_id.clone(),
            memory_store: self.memory_store.clone(),
            memory_file_store: self.memory_file_store.clone(),
            ghost_memory_lifecycle: self.ghost_memory_lifecycle.clone(),
            skill_file_store: self.skill_file_store.clone(),
            capability_registry: self.capability_registry.clone(),
            core_evolution: self.core_evolution.clone(),
            evolution_worker: self.evolution_worker.clone(),
            skill_evolution_worker: self.skill_evolution_worker.clone(),
            evolution_workflow_store: self.evolution_workflow_store.clone(),
            event_tx: self.event_tx.clone(),
            system_event_store: self.system_event_store.clone(),
            system_event_orchestrator: self.system_event_orchestrator.clone(),
            system_event_emitter: self.system_event_emitter.clone(),
            main_session_target: self.main_session_target.clone(),
            shared_session_target: Arc::clone(&self.shared_session_target),
            cap_request_cooldown: HashMap::new(),
            channel_contacts: self.channel_contacts.clone(),
            path_policy: self.path_policy.clone(),
            tool_policy: self.tool_policy.clone(),
            event_bus: self.event_bus.clone(),
            event_tx_slot: self.event_tx_slot.clone(),
            current_turn_id: None,
            current_step: None,
            turn_step_counter: 0,
            candidate_scope: None,
            agent_loop: Arc::clone(&self.agent_loop),
            response_cache: self.response_cache.clone(),
            memory_system: None,
            session_memory_systems: Arc::clone(&self.session_memory_systems),
            memory_injector_needs_reload: Arc::clone(&self.memory_injector_needs_reload),
            memory_injector_init_failed: Arc::clone(&self.memory_injector_init_failed),
            abort_token,
            runtime_handle: None,
            learning_coordinator: Arc::clone(&self.learning_coordinator),
            skill_mutex: self.skill_mutex.clone(),
            agent_type_registry: self.agent_type_registry.clone(),
            write_guard: Arc::clone(&self.write_guard),
            budget_trackers: Arc::clone(&self.budget_trackers),
            parent_budget: self.parent_budget.clone(),
            steering,
            steering_sender,
            active_steering_registry: None,
            background_tasks_enabled: self.background_tasks_enabled,
            background_task_handles: Vec::new(),
            last_verification_command: None,
        };
        if runtime.memory_file_store.is_none() {
            if let Err(e) = runtime.init_memory_file_store() {
                tracing::warn!(error = %e, "Failed to initialize file memory store");
            }
        }
        if runtime.skill_file_store.is_none() {
            if let Err(e) = runtime.init_skill_file_store() {
                tracing::warn!(error = %e, "Failed to initialize skill file store");
            }
        }
        runtime.sync_tool_scope_view("");
        runtime.init_runtime_handle();
        runtime
    }

    /// Set the self-referential runtime handle for the agent tool.
    /// Creates a `LightweightRuntimeHandle` from current runtime state.
    pub fn init_runtime_handle(&mut self) {
        let handle = Arc::new(LightweightRuntimeHandle::from_runtime(self))
            as blockcell_tools::AgentRuntimeHandle;
        self.runtime_handle = Some(handle);
    }

    /// Test-only hook: production runtimes share the coordinator through
    /// `fork_for_message_task`, which clones the `Arc` directly. Keeping this
    /// setter out of production builds avoids a dead public method.
    #[cfg(test)]
    pub(crate) fn install_shared_learning_coordinator(
        &mut self,
        coordinator: Arc<crate::learning_coordinator::LearningCoordinator>,
    ) {
        self.learning_coordinator = coordinator;
    }
}

/// 创建技能部署成功后的回调，记录 EvolutionSuccess ghost learning boundary。
///
/// 此函数可被不同运行路径（run_message_task、CLI interactive、gateway、scheduler worker）共享，
/// 确保所有部署路径都能触发 ghost learning boundary 记录。
#[allow(clippy::type_complexity)]
pub fn create_evolution_deploy_callback(
    config: &Config,
    paths: &Paths,
) -> Option<Arc<dyn Fn(&str) + Send + Sync>> {
    if !config.agents.ghost.learning.capture_enabled() {
        return None;
    }

    let config = config.clone();
    let paths = paths.clone();

    Some(Arc::new(move |skill_name: &str| {
        // 失效 prompt snapshot 文件，确保下一轮 reload_skills() 重新生成
        if let Err(e) = blockcell_skills::SkillManager::invalidate_prompt_snapshot(&paths) {
            tracing::warn!(
                skill = %skill_name,
                error = %e,
                "[evolution] Failed to invalidate prompt snapshot after deploy"
            );
        }

        let boundary = GhostLearningBoundary {
            kind: GhostLearningBoundaryKind::EvolutionSuccess,
            session_key: None,
            subject_key: Some(format!("skill:{}", skill_name)),
            user_intent_summary: format!("Skill '{}' evolution deployed successfully", skill_name),
            assistant_outcome_summary: String::new(),
            tool_call_count: 0,
            memory_write_count: 0,
            correction_count: 0,
            preference_correction_count: 0,
            success: true,
            complexity_score: 0,
            reusable_lesson: None,
            turn_facts: None,
        };

        let policy = GhostLearningPolicy::from_config(&config.agents.ghost.learning);
        let decision = policy.decide(&boundary);

        if let Err(e) = persist_ghost_learning_boundary_with_decision(
            &config,
            &paths,
            boundary,
            vec![],
            decision,
        ) {
            tracing::warn!(
                skill = %skill_name,
                error = %e,
                "[evolution] Failed to persist EvolutionSuccess ghost boundary"
            );
        }
    }))
}

#[cfg(test)]
impl AgentRuntime {
    fn test_ghost_ledger(&self) -> GhostLedger {
        GhostLedger::open(&self.paths.ghost_ledger_db()).expect("open ghost ledger")
    }

    fn test_ghost_metrics(&self) -> crate::GhostMetricsSnapshot {
        crate::ghost_metrics_summary(&self.paths)
    }

    async fn test_trigger_pre_compress(&mut self) -> Result<()> {
        let session_key = blockcell_core::build_session_key("cli", "ghost-pre-compress");
        let history = vec![
            ChatMessage::user("figure out the correct deploy sequence"),
            ChatMessage::assistant("captured deploy analysis before compact"),
        ];
        self.capture_pre_compress_learning_boundary(&session_key, &history)
            .await
            .map(|_| ())
    }

    async fn test_trigger_session_end(&mut self) -> Result<()> {
        self.capture_main_session_end_learning_boundary()
            .await
            .map(|_| ())
    }

    async fn test_complete_delegated_task(
        &self,
        task_goal: &str,
        child_summary: &str,
    ) -> Result<Option<String>> {
        capture_delegation_end_learning_boundary_with_config(
            &self.config,
            &self.paths,
            "cli",
            "ghost-delegation",
            None,
            task_goal,
            child_summary,
            true,
        )
    }
}

/// RuntimeHandle trait implementation for AgentRuntime
///
/// Allows tools to spawn subagents and read context-window estimates.
impl blockcell_tools::RuntimeHandle for AgentRuntime {
    fn subagents(&self) -> Arc<dyn blockcell_tools::subagent::SubagentProvider> {
        Arc::new(InProcessSubagentProvider::from_runtime(self))
    }

    fn context_window_snapshot(&self, session_key: &str) -> blockcell_core::ContextWindowSnapshot {
        self.budget_tracker_for_session(session_key)
            .context_window_snapshot()
    }
}

/// Cap on the shared session MemorySystem pool. Entries are reconstructible (the pool
/// only avoids re-running Layer 4 init I/O), so eviction need not be LRU-exact; without a
/// cap a long-lived gateway process grows with every session it has ever served.
const MAX_SESSION_MEMORY_SYSTEMS: usize = 64;

/// Bounded pool of idle values keyed by session id.
///
/// Generic (and separate from `MemorySystem`) so the eviction policy can be
/// regression-tested without constructing a real one. Eviction picks the entry
/// with the oldest `seq`, i.e. the one returned to the pool longest ago: pooled
/// systems are reconstructible, so a wrong guess costs an init round-trip but is
/// never fatal. This is what makes it LRU-ish instead of arbitrary (P3-026) —
/// `HashMap::keys().next()` used to evict a hot session as readily as a cold one.
struct IdlePool<T> {
    entries: HashMap<String, (u64, T)>,
    next_tick: u64,
}

impl<T> IdlePool<T> {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            next_tick: 0,
        }
    }

    /// Remove and return the pooled value, marking the session as in use.
    fn take(&mut self, session_id: &str) -> Option<T> {
        self.entries.remove(session_id).map(|(_, value)| value)
    }

    /// Return a value to the pool, evicting the least-recently-returned entry
    /// when full. Re-putting an existing key only refreshes its recency.
    fn put(&mut self, session_id: String, value: T) {
        if self.entries.len() >= MAX_SESSION_MEMORY_SYSTEMS
            && !self.entries.contains_key(&session_id)
        {
            if let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, (seq, _))| *seq)
                .map(|(key, _)| key.clone())
            {
                self.entries.remove(&victim);
            }
        }
        self.next_tick += 1;
        self.entries.insert(session_id, (self.next_tick, value));
    }
}

type MemorySystemPool = IdlePool<crate::memory_system::MemorySystem>;

impl AgentRuntime {
    /// Return a MemorySystem to the shared pool, evicting the least recently
    /// returned session when full.
    fn return_memory_system_to_pool(&self, ms: crate::memory_system::MemorySystem) {
        // Pooled systems are idle until reused; do not leave their `.active`
        // marker behind and prevent Dream pruning from reclaiming the session.
        ms.clear_session_active_sync();
        let Ok(mut pool) = self.session_memory_systems.lock() else {
            return;
        };
        let session_id = ms.session_id().to_string();
        pool.put(session_id, ms);
    }
}

impl Drop for AgentRuntime {
    fn drop(&mut self) {
        if let Some(ms) = self.memory_system.take() {
            self.return_memory_system_to_pool(ms);
        }
    }
}

#[cfg(test)]
mod tests;
