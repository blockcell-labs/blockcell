use async_trait::async_trait;
use blockcell_core::system_event::{DeliveryPolicy, EventPriority, EventScope, SystemEvent};
use blockcell_core::{AbortToken, AgentResult, Error};
use blockcell_tools::{EventEmitterHandle, TaskManagerOps};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{broadcast, mpsc, Mutex};

use crate::agent_progress::AgentProgress;

/// Status of a background task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Task is queued but not yet started.
    Queued,
    /// Task is currently being processed by a subagent.
    Running,
    /// Task completed successfully.
    Completed,
    /// Task failed with an error.
    Failed,
    /// Task was cancelled before completion.
    Cancelled,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Queued => write!(f, "queued"),
            TaskStatus::Running => write!(f, "running"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Failed => write!(f, "failed"),
            TaskStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// A background task tracked by the TaskManager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    pub id: String,
    pub label: String,
    pub task_description: String,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Short progress note updated by the subagent during execution.
    pub progress: Option<String>,
    /// Final result summary (truncated).
    pub result: Option<String>,
    /// Error message if failed.
    pub error: Option<String>,
    /// Origin channel that spawned this task.
    pub origin_channel: String,
    /// Origin chat_id that spawned this task.
    pub origin_chat_id: String,
    /// Optional channel account that owns this task's origin conversation.
    #[serde(default)]
    pub origin_account_id: Option<String>,
    /// Canonical immutable session key for origin-scoped event delivery.
    #[serde(default)]
    pub origin_session_key: Option<String>,
    /// Agent that owns this task. Missing values are treated as the default agent.
    pub agent_id: Option<String>,
    #[serde(default)]
    emit_system_events: bool,
    /// Agent type identifier (e.g., "explore", "plan", "verification", "viper", "general").
    #[serde(default)]
    pub agent_type: Option<String>,
    /// Whether this is a ONE_SHOT task (cannot receive SendMessage after completion).
    #[serde(default)]
    pub one_shot: bool,
    /// Transcript storage ID for retrieving full conversation history.
    #[serde(default)]
    pub transcript_id: Option<String>,

    // ===== 新增字段 =====
    /// 是否已发送完成通知（防止重复通知）
    #[serde(default)]
    pub notified: bool,
    /// 输出文件路径（用于断点恢复和结果持久化）
    #[serde(default)]
    pub output_file: Option<String>,
    /// 延迟清理截止时间（失败后保留一段时间供用户查看）
    #[serde(default)]
    pub evict_after: Option<DateTime<Utc>>,
    /// 结果是否已注入到主agent的LLM对话中（防止重复注入）
    #[serde(default)]
    pub result_injected: bool,
    /// Set when an unfinished persisted task is converted to Failed on startup.
    #[serde(default)]
    pub restored_after_restart: bool,
}

impl TaskInfo {
    pub fn belongs_to_origin(&self, channel: &str, chat_id: &str) -> bool {
        self.origin_channel == channel && self.origin_chat_id == chat_id
    }

    /// 是否发送系统事件
    pub fn emit_system_events(&self) -> bool {
        self.emit_system_events
    }

    /// 获取任务执行时长（毫秒）
    /// 利用现有 started_at/completed_at 计算
    /// 负值（时钟偏移/数据损坏）返回 0 而非 wrap 为巨大值
    pub fn duration_ms(&self) -> Option<u64> {
        match (self.started_at, self.completed_at) {
            (Some(start), Some(end)) => {
                let ms = (end - start).num_milliseconds();
                Some(ms.try_into().unwrap_or(0))
            }
            (Some(start), None) if self.status == TaskStatus::Running => {
                let ms = (Utc::now() - start).num_milliseconds();
                Some(ms.try_into().unwrap_or(0))
            }
            _ => None,
        }
    }
}

/// 任务完成事件，用于广播子Agent完成状态
#[derive(Debug, Clone)]
pub struct TaskCompletedEvent {
    pub task_id: String,
    pub agent_type: String,
    pub result: AgentResult,
    pub completed_at: DateTime<Utc>,
}

/// Thread-safe task registry for tracking background subagent tasks.
#[derive(Clone)]
pub struct TaskManager {
    tasks: Arc<Mutex<HashMap<String, TaskInfo>>>,
    event_emitters: Arc<StdMutex<HashMap<String, EventEmitterHandle>>>,
    /// Message queues for SendMessage support (task_id -> pending messages).
    /// Uses StdMutex (std::sync::Mutex) because drain_pending_messages is called
    /// in non-async context (from execution loop).
    message_queues: Arc<StdMutex<HashMap<String, VecDeque<String>>>>,
    /// Cooperative cancellation tokens for typed/background agent tasks.
    abort_tokens: Arc<StdMutex<HashMap<String, AbortToken>>>,
    /// Persistent shell session IDs mapped to their process-group leaders.
    shell_processes: Arc<StdMutex<HashMap<String, u32>>>,
    /// Claimed workspace paths for concurrently running coding agents.
    workspace_locks: Arc<StdMutex<HashMap<PathBuf, String>>>,
    /// Progress callback channel for reporting agent execution progress.
    progress_tx: Option<mpsc::Sender<AgentProgress>>,
    /// 任务完成事件广播通道，用于Lead Agent订阅子Agent完成事件
    completed_events_tx: broadcast::Sender<TaskCompletedEvent>,
    /// 工作目录，用于任务持久化
    workspace_dir: Option<PathBuf>,
}

fn normalized_agent_key(agent_id: Option<&str>) -> String {
    agent_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
        .to_string()
}

/// 判断任务状态是否为终止状态（不再变化）
/// 参考: Claude Code Task.ts isTerminalTaskStatus()
pub fn is_terminal_status(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
    )
}

/// 延迟清理的宽限期（秒）
pub const EVICT_GRACE_PERIOD_SECS: i64 = 30;

/// Maximum queued steering messages per task. Without a bound an unresponsive
/// task could grow its queue indefinitely.
const MAX_QUEUED_MESSAGES: usize = 64;

/// Lifecycle event summaries are delivered to users/system events; keep only a
/// bounded prefix instead of embedding a full tool result or stack trace.
const MAX_LIFECYCLE_TEXT_CHARS: usize = 500;

fn truncate_event_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max_chars).collect();
    truncated.push('…');
    truncated
}

mod persistence;

#[cfg(test)]
mod workspace_scope_tests {
    use super::*;

    #[test]
    fn overlapping_workspace_scopes_fail_fast_until_owner_releases() {
        let manager = TaskManager::new();
        let root = PathBuf::from("/tmp/blockcell-scope-test");

        manager
            .try_acquire_workspace_scope("task-a", &[root.join("src")])
            .expect("first scope should lock");
        let conflict = manager
            .try_acquire_workspace_scope("task-b", &[root.join("src/lib.rs")])
            .expect_err("nested scope should conflict");
        assert!(conflict.to_string().contains("task-a"));

        manager.release_workspace_scope("task-a");
        manager
            .try_acquire_workspace_scope("task-b", &[root.join("src/lib.rs")])
            .expect("released scope should be reusable");
    }
}

impl TaskManager {
    /// Atomically claim a set of file/directory paths for a background coder.
    pub fn try_acquire_workspace_scope(
        &self,
        task_id: &str,
        scope: &[PathBuf],
    ) -> Result<(), Error> {
        let mut locks = self
            .workspace_locks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for requested in scope {
            if let Some((path, owner)) = locks.iter().find(|(path, _)| {
                requested.as_path() == path.as_path()
                    || requested.starts_with(path)
                    || path.starts_with(requested)
            }) {
                return Err(Error::Tool(format!(
                    "workspace_scope 冲突: {} 已由任务 {} 锁定",
                    path.display(),
                    owner
                )));
            }
        }
        for path in scope {
            locks.insert(path.clone(), task_id.to_string());
        }
        Ok(())
    }

    /// Release every workspace path owned by a task.
    pub fn release_workspace_scope(&self, task_id: &str) {
        let mut locks = self
            .workspace_locks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        locks.retain(|_, owner| owner != task_id);
    }

    pub fn new() -> Self {
        let (completed_events_tx, _) = broadcast::channel(100);
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            event_emitters: Arc::new(StdMutex::new(HashMap::new())),
            message_queues: Arc::new(StdMutex::new(HashMap::new())),
            abort_tokens: Arc::new(StdMutex::new(HashMap::new())),
            shell_processes: Arc::new(StdMutex::new(HashMap::new())),
            workspace_locks: Arc::new(StdMutex::new(HashMap::new())),
            progress_tx: None,
            completed_events_tx,
            workspace_dir: None,
        }
    }

    /// Create TaskManager with workspace directory for task persistence.
    pub fn with_workspace(workspace_dir: &Path) -> Self {
        let (completed_events_tx, _) = broadcast::channel(100);
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            event_emitters: Arc::new(StdMutex::new(HashMap::new())),
            message_queues: Arc::new(StdMutex::new(HashMap::new())),
            abort_tokens: Arc::new(StdMutex::new(HashMap::new())),
            shell_processes: Arc::new(StdMutex::new(HashMap::new())),
            workspace_locks: Arc::new(StdMutex::new(HashMap::new())),
            progress_tx: None,
            completed_events_tx,
            workspace_dir: Some(workspace_dir.to_path_buf()),
        }
    }

    /// Create TaskManager with a progress callback channel.
    pub fn with_progress_tx(progress_tx: mpsc::Sender<AgentProgress>) -> Self {
        let (completed_events_tx, _) = broadcast::channel(100);
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            event_emitters: Arc::new(StdMutex::new(HashMap::new())),
            message_queues: Arc::new(StdMutex::new(HashMap::new())),
            abort_tokens: Arc::new(StdMutex::new(HashMap::new())),
            shell_processes: Arc::new(StdMutex::new(HashMap::new())),
            workspace_locks: Arc::new(StdMutex::new(HashMap::new())),
            progress_tx: Some(progress_tx),
            completed_events_tx,
            workspace_dir: None,
        }
    }

    /// Create TaskManager with both workspace and progress channel.
    pub fn with_workspace_and_progress(
        workspace_dir: &Path,
        progress_tx: mpsc::Sender<AgentProgress>,
    ) -> Self {
        let (completed_events_tx, _) = broadcast::channel(100);
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            event_emitters: Arc::new(StdMutex::new(HashMap::new())),
            message_queues: Arc::new(StdMutex::new(HashMap::new())),
            abort_tokens: Arc::new(StdMutex::new(HashMap::new())),
            shell_processes: Arc::new(StdMutex::new(HashMap::new())),
            workspace_locks: Arc::new(StdMutex::new(HashMap::new())),
            progress_tx: Some(progress_tx),
            completed_events_tx,
            workspace_dir: Some(workspace_dir.to_path_buf()),
        }
    }

    /// Send progress update through the progress channel.
    /// Returns true if the progress was sent successfully, false if no channel is configured.
    pub async fn send_progress(&self, progress: AgentProgress) -> bool {
        if let Some(tx) = &self.progress_tx {
            tx.send(progress).await.is_ok()
        } else {
            false
        }
    }

    /// Get a clone of the progress_tx sender, if configured.
    pub fn progress_tx(&self) -> Option<mpsc::Sender<AgentProgress>> {
        self.progress_tx.clone()
    }

    /// Register a cooperative cancellation token for a task.
    pub fn register_abort_token(&self, task_id: &str, token: AbortToken) {
        let mut tokens = match self.abort_tokens.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        tokens.insert(task_id.to_string(), token);
    }

    /// Remove a task's cancellation token once the task reaches a terminal state.
    pub fn unregister_abort_token(&self, task_id: &str) {
        let mut tokens = match self.abort_tokens.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        tokens.remove(task_id);
    }

    pub fn register_shell_process(&self, session_id: &str, pid: u32) {
        let mut processes = match self.shell_processes.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        processes.insert(session_id.to_string(), pid);
    }

    pub fn unregister_shell_process(&self, session_id: &str) {
        let mut processes = match self.shell_processes.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        processes.remove(session_id);
    }

    pub fn registered_shell_process_count(&self) -> usize {
        match self.shell_processes.lock() {
            Ok(guard) => guard.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    pub fn shutdown_shell_processes(&self) {
        let processes = {
            let mut processes = match self.shell_processes.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            processes.drain().map(|(_, pid)| pid).collect::<Vec<_>>()
        };
        #[cfg(unix)]
        for pid in processes {
            // SAFETY: shell sessions register process-group leaders created by
            // the shell tool, so a negative PID only targets that group.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        let _ = processes;
    }

    fn cancel_registered_abort_token(&self, task_id: &str) -> bool {
        let token = {
            let tokens = match self.abort_tokens.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            tokens.get(task_id).cloned()
        };

        if let Some(token) = token {
            token.cancel();
            true
        } else {
            false
        }
    }

    fn cleanup_task_runtime_state(&self, task_id: &str) {
        self.cancel_registered_abort_token(task_id);
        self.unregister_abort_token(task_id);
        self.release_workspace_scope(task_id);
        let mut queues = match self.message_queues.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        queues.remove(task_id);
    }

    /// Persist task to disk if workspace_dir is configured.
    async fn persist_if_configured(&self, task: &TaskInfo) {
        if let Some(workspace_dir) = &self.workspace_dir {
            self.persist_task_to_disk(workspace_dir, task).await;
        }
    }

    pub fn register_event_emitter(&self, agent_id: Option<&str>, emitter: EventEmitterHandle) {
        let mut emitters = match self.event_emitters.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        emitters.insert(normalized_agent_key(agent_id), emitter);
    }

    fn event_emitter_for_agent(&self, agent_id: Option<&str>) -> Option<EventEmitterHandle> {
        let emitters = match self.event_emitters.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        emitters
            .get(&normalized_agent_key(agent_id))
            .cloned()
            .or_else(|| emitters.get("default").cloned())
    }

    fn emit_lifecycle_event(&self, task: &TaskInfo, phase: &str) {
        if !task.emit_system_events {
            return;
        }

        let Some(emitter) = self.event_emitter_for_agent(task.agent_id.as_deref()) else {
            return;
        };

        let (priority, title, summary, delivery) = match phase {
            "created" => (
                EventPriority::Normal,
                "后台任务已创建".to_string(),
                format!("{} 已加入后台队列", task.label),
                DeliveryPolicy::default(),
            ),
            "running" => (
                EventPriority::Normal,
                "后台任务开始执行".to_string(),
                format!("{} 正在后台执行", task.label),
                DeliveryPolicy::default(),
            ),
            "completed" => (
                EventPriority::Normal,
                "后台任务已完成".to_string(),
                task.result
                    .as_ref()
                    .map(|result| {
                        format!(
                            "{} 已完成：{}",
                            task.label,
                            truncate_event_text(result, MAX_LIFECYCLE_TEXT_CHARS)
                        )
                    })
                    .unwrap_or_else(|| format!("{} 已完成", task.label)),
                DeliveryPolicy::default(),
            ),
            "failed" => (
                EventPriority::Critical,
                "后台任务失败".to_string(),
                task.error
                    .as_ref()
                    .map(|error| {
                        format!(
                            "{} 执行失败：{}",
                            task.label,
                            truncate_event_text(error, MAX_LIFECYCLE_TEXT_CHARS)
                        )
                    })
                    .unwrap_or_else(|| format!("{} 执行失败", task.label)),
                DeliveryPolicy::critical(),
            ),
            "cancelled" => (
                EventPriority::Normal,
                "后台任务已取消".to_string(),
                format!("{} 已被用户取消", task.label),
                DeliveryPolicy::default(),
            ),
            _ => return,
        };

        let mut event = SystemEvent::new_main_session(
            format!("task.{}", phase),
            "task_manager",
            priority,
            title,
            summary,
        );
        event.scope = if let Some(session_key) = task.origin_session_key.as_ref() {
            EventScope::Session {
                channel: task.origin_channel.clone(),
                account_id: task.origin_account_id.clone(),
                chat_id: task.origin_chat_id.clone(),
                session_key: session_key.clone(),
            }
        } else {
            EventScope::Channel {
                channel: task.origin_channel.clone(),
                chat_id: task.origin_chat_id.clone(),
            }
        };
        event.delivery = delivery;
        event.dedup_key = Some(format!("task:{}", task.id));
        event.details = json!({
            "task_id": task.id.clone(),
            "label": task.label.clone(),
            "status": task.status.to_string(),
            "origin_channel": task.origin_channel.clone(),
            "origin_chat_id": task.origin_chat_id.clone(),
            "agent_id": task.agent_id.clone(),
        });
        emitter.emit(event);
    }

    /// Register a new task and return its ID.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_task(
        &self,
        task_id: &str,
        label: &str,
        description: &str,
        origin_channel: &str,
        origin_chat_id: &str,
        agent_id: Option<&str>,
        emit_system_events: bool,
        agent_type: Option<&str>,
        one_shot: bool,
    ) -> TaskInfo {
        let info = TaskInfo {
            id: task_id.to_string(),
            label: label.to_string(),
            task_description: description.to_string(),
            status: TaskStatus::Queued,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            progress: None,
            result: None,
            error: None,
            origin_channel: origin_channel.to_string(),
            origin_chat_id: origin_chat_id.to_string(),
            origin_account_id: None,
            origin_session_key: None,
            agent_id: agent_id.map(str::to_string),
            emit_system_events,
            agent_type: agent_type.map(str::to_string),
            one_shot,
            transcript_id: None,
            // 新增字段默认值
            notified: false,
            output_file: None,
            evict_after: None,
            result_injected: false,
            restored_after_restart: false,
        };
        {
            let mut tasks = self.tasks.lock().await;
            tasks.insert(task_id.to_string(), info.clone());
        }
        self.emit_lifecycle_event(&info, "created");
        self.persist_if_configured(&info).await;
        info
    }

    /// Atomically create a task and mark it as running.
    /// This eliminates the race condition between create_task and set_running
    /// where a concurrent cleanup could remove the task between the two calls.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_and_start_task(
        &self,
        task_id: &str,
        label: &str,
        description: &str,
        origin_channel: &str,
        origin_chat_id: &str,
        agent_id: Option<&str>,
        emit_system_events: bool,
        agent_type: Option<&str>,
        one_shot: bool,
    ) -> TaskInfo {
        self.create_and_start_task_with_route(
            task_id,
            label,
            description,
            origin_channel,
            origin_chat_id,
            None,
            None,
            agent_id,
            emit_system_events,
            agent_type,
            one_shot,
        )
        .await
    }

    /// Atomically create a running task with immutable origin routing metadata.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_and_start_task_with_route(
        &self,
        task_id: &str,
        label: &str,
        description: &str,
        origin_channel: &str,
        origin_chat_id: &str,
        origin_account_id: Option<&str>,
        origin_session_key: Option<&str>,
        agent_id: Option<&str>,
        emit_system_events: bool,
        agent_type: Option<&str>,
        one_shot: bool,
    ) -> TaskInfo {
        let now = Utc::now();
        let info = TaskInfo {
            id: task_id.to_string(),
            label: label.to_string(),
            task_description: description.to_string(),
            status: TaskStatus::Running,
            created_at: now,
            started_at: Some(now),
            completed_at: None,
            progress: None,
            result: None,
            error: None,
            origin_channel: origin_channel.to_string(),
            origin_chat_id: origin_chat_id.to_string(),
            origin_account_id: origin_account_id.map(str::to_string),
            origin_session_key: origin_session_key.map(str::to_string),
            agent_id: agent_id.map(str::to_string),
            emit_system_events,
            agent_type: agent_type.map(str::to_string),
            one_shot,
            transcript_id: None,
            notified: false,
            output_file: None,
            evict_after: None,
            result_injected: false,
            restored_after_restart: false,
        };
        {
            let mut tasks = self.tasks.lock().await;
            tasks.insert(task_id.to_string(), info.clone());
        }
        self.emit_lifecycle_event(&info, "created");
        self.emit_lifecycle_event(&info, "running");
        self.persist_if_configured(&info).await;
        info
    }

    /// Mark a task as running.
    pub async fn set_running(&self, task_id: &str) {
        let updated = {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.get_mut(task_id) {
                task.status = TaskStatus::Running;
                task.started_at = Some(Utc::now());
                Some(task.clone())
            } else {
                None
            }
        };

        if let Some(task) = updated {
            self.emit_lifecycle_event(&task, "running");
            self.persist_if_configured(&task).await;
        }
    }

    /// Update the progress note for a running task.
    /// 同时通过 progress_tx 发送 Stage 事件，以便 WebSocket/控制台实时显示进度。
    /// 更新任务进度文本，同时发送 Stage 事件（percent=0 表示进行中）。
    pub async fn set_progress(&self, task_id: &str, progress: &str) {
        let exists = {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.get_mut(task_id) {
                task.progress = Some(progress.to_string());
                true
            } else {
                false
            }
        };
        if exists {
            let _ = self
                .send_progress(AgentProgress::Stage {
                    task_id: task_id.to_string(),
                    stage: progress.to_string(),
                    percent: 0,
                })
                .await;
        }
    }

    /// 更新任务进度（带百分比），同时发送 Stage 事件。
    pub async fn set_progress_with_percent(&self, task_id: &str, progress: &str, percent: u8) {
        let exists = {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.get_mut(task_id) {
                task.progress = Some(progress.to_string());
                true
            } else {
                false
            }
        };
        if exists {
            let _ = self
                .send_progress(AgentProgress::Stage {
                    task_id: task_id.to_string(),
                    stage: progress.to_string(),
                    percent: percent.min(100),
                })
                .await;
        }
    }

    /// Mark a task as completed with a result summary.
    /// 如果任务已处于终态（Cancelled/Completed/Failed），则跳过更新，防止状态回退或重复完成。
    pub async fn set_completed(&self, task_id: &str, result: &str) {
        let updated = {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.get_mut(task_id) {
                // 防止覆盖已处于终态的任务（cancel/complete/fail 的竞态保护）
                if matches!(
                    task.status,
                    TaskStatus::Cancelled | TaskStatus::Completed | TaskStatus::Failed
                ) {
                    tracing::warn!(
                        task_id = %task_id,
                        status = ?task.status,
                        "Skipping set_completed: task already in terminal state"
                    );
                    None
                } else {
                    task.status = TaskStatus::Completed;
                    task.completed_at = Some(Utc::now());
                    let truncated = if result.chars().count() > 50000 {
                        let end = result
                            .char_indices()
                            .nth(50000)
                            .map(|(i, _)| i)
                            .unwrap_or(result.len());
                        format!("{}... (truncated)", &result[..end])
                    } else {
                        result.to_string()
                    };
                    task.result = Some(truncated);
                    Some(task.clone())
                }
            } else {
                tracing::warn!(
                    task_id = %task_id,
                    "set_completed: task not found, may have been cleaned up"
                );
                None
            }
        };

        // Drain any pending messages that arrived after the task stopped reading
        let orphaned = self.drain_pending_messages(task_id);
        if !orphaned.is_empty() {
            tracing::warn!(
                task_id = %task_id,
                count = orphaned.len(),
                "Messages arrived after task completion, discarding"
            );
        }
        self.unregister_abort_token(task_id);
        self.release_workspace_scope(task_id);

        if let Some(task) = updated {
            self.emit_lifecycle_event(&task, "completed");
            self.persist_if_configured(&task).await;
        }
    }

    /// Mark a task as failed with an error message.
    /// 如果任务已处于终态（Cancelled/Completed/Failed），则跳过更新，防止状态回退。
    pub async fn set_failed(&self, task_id: &str, error: &str) {
        let updated = {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.get_mut(task_id) {
                // 防止覆盖已处于终态的任务（cancel/complete/fail 的竞态保护）
                if matches!(
                    task.status,
                    TaskStatus::Cancelled | TaskStatus::Completed | TaskStatus::Failed
                ) {
                    tracing::warn!(
                        task_id = %task_id,
                        status = ?task.status,
                        "Skipping set_failed: task already in terminal state"
                    );
                    None
                } else {
                    task.status = TaskStatus::Failed;
                    task.completed_at = Some(Utc::now());
                    task.error = Some(error.to_string());
                    Some(task.clone())
                }
            } else {
                None
            }
        };

        // 清理残留消息，防止内存泄漏
        let orphaned = self.drain_pending_messages(task_id);
        if !orphaned.is_empty() {
            tracing::warn!(
                task_id = %task_id,
                count = orphaned.len(),
                "Messages in queue when task failed, discarding"
            );
        }
        self.unregister_abort_token(task_id);
        self.release_workspace_scope(task_id);

        if let Some(task) = updated {
            self.emit_lifecycle_event(&task, "failed");
            self.persist_if_configured(&task).await;
        }
    }

    /// Mark a task's result as injected into the main agent's LLM conversation.
    /// Prevents the same result from being injected multiple times.
    pub async fn mark_result_injected(&self, task_id: &str) {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.get_mut(task_id) {
            task.result_injected = true;
        }
    }

    /// Get info for a specific task.
    pub async fn get_task(&self, task_id: &str) -> Option<TaskInfo> {
        let tasks = self.tasks.lock().await;
        tasks.get(task_id).cloned()
    }

    /// 通过 ID 前缀查找任务（支持用户输入简短 ID）
    /// 空前缀返回空列表（避免意外返回所有任务）
    pub async fn find_task_by_prefix(&self, prefix: &str) -> Vec<TaskInfo> {
        if prefix.is_empty() {
            return Vec::new();
        }
        let tasks = self.tasks.lock().await;
        tasks
            .iter()
            .filter(|(id, _)| {
                // 匹配完整ID前缀，或剥离"task-"前缀后的UUID部分前缀
                // 例如: prefix="c7a0" 能匹配 "task-c7a0b829..."
                id.starts_with(prefix)
                    || id
                        .strip_prefix("task-")
                        .is_some_and(|rest| rest.starts_with(prefix))
            })
            .map(|(_, t)| t.clone())
            .collect()
    }

    /// Find tasks by ID prefix, restricted to the originating conversation.
    pub async fn find_task_by_prefix_for_origin(
        &self,
        prefix: &str,
        channel: &str,
        chat_id: &str,
    ) -> Vec<TaskInfo> {
        self.find_task_by_prefix(prefix)
            .await
            .into_iter()
            .filter(|task| task.belongs_to_origin(channel, chat_id))
            .collect()
    }

    /// 取消运行中的任务。
    /// 将任务状态设为 Cancelled，并触发已注册的 AbortToken。
    pub async fn cancel_task(&self, task_id: &str) -> Result<(), Error> {
        self.set_cancelled(task_id, "Cancelled by user").await
    }

    /// 使用明确原因将运行中或排队中的任务置为已取消。
    ///
    /// 用户主动取消和父任务链式取消都走同一个终态入口，确保状态、
    /// AbortToken、生命周期事件和持久化记录保持一致。
    pub async fn set_cancelled(&self, task_id: &str, reason: &str) -> Result<(), Error> {
        let updated = {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.get_mut(task_id) {
                match task.status {
                    TaskStatus::Running | TaskStatus::Queued => {
                        task.status = TaskStatus::Cancelled;
                        task.completed_at = Some(Utc::now());
                        task.error = Some(reason.to_string());
                        Some(task.clone())
                    }
                    TaskStatus::Cancelled => return Ok(()),
                    _ => None, // 已完成/已失败的任务不能再取消
                }
            } else {
                None
            }
        };

        // 清理残留消息
        let orphaned = self.drain_pending_messages(task_id);
        if !orphaned.is_empty() {
            tracing::warn!(
                task_id = %task_id,
                count = orphaned.len(),
                "Messages in queue when task cancelled, discarding"
            );
        }

        if let Some(task) = updated {
            let token_cancelled = self.cancel_registered_abort_token(task_id);
            self.unregister_abort_token(task_id);
            self.release_workspace_scope(task_id);
            if token_cancelled {
                tracing::info!(task_id = %task_id, "Cancelled registered AbortToken for task");
            }
            self.emit_lifecycle_event(&task, "cancelled");
            self.persist_if_configured(&task).await;
            Ok(())
        } else {
            Err(Error::Tool(format!(
                "Task {} cannot be cancelled (not found or not in cancellable state)",
                task_id
            )))
        }
    }

    /// List all tasks, optionally filtered by status.
    pub async fn list_tasks(&self, status_filter: Option<&TaskStatus>) -> Vec<TaskInfo> {
        let tasks = self.tasks.lock().await;
        let mut result: Vec<TaskInfo> = tasks
            .values()
            .filter(|t| {
                if let Some(filter) = status_filter {
                    &t.status == filter
                } else {
                    true
                }
            })
            .cloned()
            .collect();
        result.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        result
    }

    /// List tasks restricted to the originating conversation.
    pub async fn list_tasks_for_origin(
        &self,
        channel: &str,
        chat_id: &str,
        status_filter: Option<&TaskStatus>,
    ) -> Vec<TaskInfo> {
        self.list_tasks(status_filter)
            .await
            .into_iter()
            .filter(|task| task.belongs_to_origin(channel, chat_id))
            .collect()
    }

    /// Get task counts restricted to the originating conversation.
    pub async fn summary_for_origin(
        &self,
        channel: &str,
        chat_id: &str,
    ) -> (usize, usize, usize, usize) {
        let tasks = self.list_tasks_for_origin(channel, chat_id, None).await;
        let mut counts = (0, 0, 0, 0);
        for task in tasks {
            match task.status {
                TaskStatus::Queued => counts.0 += 1,
                TaskStatus::Running => counts.1 += 1,
                TaskStatus::Completed => counts.2 += 1,
                TaskStatus::Failed => counts.3 += 1,
                TaskStatus::Cancelled => {}
            }
        }
        counts
    }

    /// Get counts of tasks by status.
    pub async fn summary(&self) -> (usize, usize, usize, usize) {
        let tasks = self.tasks.lock().await;
        let mut queued = 0;
        let mut running = 0;
        let mut completed = 0;
        let mut failed = 0;
        for task in tasks.values() {
            match task.status {
                TaskStatus::Queued => queued += 1,
                TaskStatus::Running => running += 1,
                TaskStatus::Completed => completed += 1,
                TaskStatus::Failed => failed += 1,
                TaskStatus::Cancelled => failed += 1, // 取消计入 failed（保持返回类型兼容）
            }
        }
        (queued, running, completed, failed)
    }

    /// Remove completed/failed tasks older than the given duration.
    /// Also cleans up corresponding message_queues entries and persisted JSON files to prevent memory/disk leak.
    pub async fn cleanup_old_tasks(&self, max_age: std::time::Duration) {
        let cutoff = Utc::now() - chrono::Duration::from_std(max_age).unwrap_or_default();
        let removed_ids = {
            let mut tasks = self.tasks.lock().await;
            let ids_to_remove: Vec<String> = tasks
                .iter()
                .filter(|(_, t)| match t.status {
                    TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled => {
                        t.completed_at.is_none_or(|c| c <= cutoff)
                    }
                    _ => false,
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in &ids_to_remove {
                tasks.remove(id);
            }
            ids_to_remove
        };
        // Clean up corresponding message_queues entries
        if !removed_ids.is_empty() {
            {
                let mut queues = match self.message_queues.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                for id in &removed_ids {
                    queues.remove(id);
                }
            }
            {
                let mut tokens = match self.abort_tokens.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                for id in &removed_ids {
                    tokens.remove(id);
                }
            }

            // Also delete persisted JSON files to prevent disk leak
            // (done after releasing the mutex to avoid holding lock across await)
            if let Some(ref workspace_dir) = self.workspace_dir {
                let tasks_dir = Self::tasks_dir(workspace_dir);
                for id in &removed_ids {
                    let file_path = tasks_dir.join(format!("{}.json", id));
                    if tokio::fs::remove_file(&file_path).await.is_ok() {
                        tracing::debug!(task_id = %id, "Deleted persisted task file");
                    }
                }
            }

            tracing::debug!(
                removed = removed_ids.len(),
                "Cleaned up old tasks, message queues, and persisted files"
            );
        }
    }

    /// Remove a specific task by ID.
    /// Also cleans up corresponding message_queues entry to prevent memory leak.
    pub async fn remove_task(&self, task_id: &str) {
        {
            let mut tasks = self.tasks.lock().await;
            tasks.remove(task_id);
        }
        self.cleanup_task_runtime_state(task_id);
        if let Some(ref workspace_dir) = self.workspace_dir {
            self.cleanup_task_file(workspace_dir, task_id).await;
        }
    }

    /// Send a message to a running task's message queue.
    /// Used by SendMessage tool to communicate with non-ONE_SHOT agents.
    ///
    /// This implementation avoids TOCTOU race condition by:
    /// 1. Adding message to queue first
    /// 2. Then checking task existence AND running status
    /// 3. If task doesn't exist or isn't running, removing the orphaned message
    ///
    /// Lock ordering: message_queues (StdMutex) is always acquired and released
    /// before self.tasks (tokio Mutex) to prevent deadlocks.
    pub async fn send_message(&self, task_id: &str, message: String) -> Result<(), Error> {
        // Add message to queue first (to avoid race condition where we check then add)
        {
            let mut queues = match self.message_queues.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            let queue = queues.entry(task_id.to_string()).or_default();
            if queue.len() >= MAX_QUEUED_MESSAGES {
                return Err(Error::Tool(format!(
                    "Task '{}' message queue is full ({} pending); retry after it drains.",
                    task_id, MAX_QUEUED_MESSAGES
                )));
            }
            queue.push_back(message.clone());
        }

        // Then verify task exists AND is running - if not, clean up the orphaned message.
        // NOTE: We release the async tasks lock BEFORE re-acquiring message_queues
        // to prevent potential deadlocks from lock ordering violations.
        let task_status = {
            let tasks = self.tasks.lock().await;
            tasks.get(task_id).map(|t| t.status.clone())
        };

        match task_status {
            Some(TaskStatus::Running) => Ok(()),
            Some(status) => {
                // 任务存在但不在运行状态，仅移除刚入队的消息（而非整个队列）
                // 避免丢弃其他 send_message 调用者之前入队的消息
                let mut queues = match self.message_queues.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if let Some(queue) = queues.get_mut(task_id) {
                    // 从队列尾部移除刚添加的消息
                    if queue.back() == Some(&message) {
                        queue.pop_back();
                    }
                    // 如果队列已空，移除整个 entry 防止内存泄漏
                    if queue.is_empty() {
                        queues.remove(task_id);
                    }
                }
                Err(Error::Tool(format!(
                    "Task {} is not running (status: {:?})",
                    task_id, status
                )))
            }
            None => {
                // 任务不存在，清理整个队列（所有消息都是孤立的）
                let mut queues = match self.message_queues.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                queues.remove(task_id);
                Err(Error::Tool(format!("Task not found: {}", task_id)))
            }
        }
    }

    /// Drain pending messages from a task's queue (called from execution loop).
    /// Returns all pending messages and clears the queue.
    /// Uses StdMutex lock() since message_queues uses std::sync::Mutex.
    /// Recovers from poisoned mutex to avoid panic in execution loop.
    pub fn drain_pending_messages(&self, task_id: &str) -> Vec<String> {
        let mut queues = match self.message_queues.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        queues
            .remove(task_id)
            .map(|queue| queue.into_iter().collect())
            .unwrap_or_default()
    }

    /// Check if a task is ONE_SHOT type (cannot receive SendMessage).
    pub async fn is_one_shot_task(&self, task_id: &str) -> bool {
        let tasks = self.tasks.lock().await;
        tasks.get(task_id).map(|t| t.one_shot).unwrap_or(false)
    }

    /// Subscribe to task completion events.
    /// Lead Agent can use this to receive notifications when child agents complete.
    pub fn subscribe_completed_events(&self) -> broadcast::Receiver<TaskCompletedEvent> {
        self.completed_events_tx.subscribe()
    }

    /// Mark a task as completed with structured result and broadcast completion event.
    /// This is used by child agents to report their completion status to Lead Agent.
    /// 如果任务已处于终态（Cancelled/Completed/Failed），则跳过更新，防止状态回退或重复广播。
    pub async fn complete_task(&self, task_id: &str, agent_type: String, result: AgentResult) {
        let task_info = {
            // 更新 TaskInfo 状态
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.get_mut(task_id) {
                // 防止覆盖已取消/已完成/已失败的任务状态
                if matches!(
                    task.status,
                    TaskStatus::Cancelled | TaskStatus::Completed | TaskStatus::Failed
                ) {
                    tracing::warn!(
                        task_id = %task_id,
                        status = ?task.status,
                        "Skipping complete_task: task already in terminal state"
                    );
                    None
                } else {
                    task.status = TaskStatus::Completed;
                    task.completed_at = Some(Utc::now());
                    // 将 AgentResult 转换为字符串存储
                    task.result = Some(result.to_json_string());
                    Some(task.clone())
                }
            } else {
                None
            }
        };

        // 清理残留消息，防止内存泄漏
        let orphaned = self.drain_pending_messages(task_id);
        if !orphaned.is_empty() {
            tracing::warn!(
                task_id = %task_id,
                count = orphaned.len(),
                "Messages in queue when task completed, discarding"
            );
        }
        self.unregister_abort_token(task_id);
        self.release_workspace_scope(task_id);

        // 持久化任务状态 + 发送生命周期事件
        if let Some(task) = &task_info {
            self.emit_lifecycle_event(task, "completed");
            self.persist_if_configured(task).await;

            // 广播完成事件
            let event = TaskCompletedEvent {
                task_id: task_id.to_string(),
                agent_type,
                result,
                completed_at: Utc::now(),
            };

            // Broadcast completion event (ignore no-subscriber errors)
            self.completed_events_tx.send(event).ok();
        }
    }

    /// 标记任务已通知，返回是否首次设置
    /// 防止重复发送完成通知
    pub async fn mark_notified(&self, task_id: &str) -> bool {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.get_mut(task_id) {
            if task.notified {
                return false; // 已通知过
            }
            task.notified = true;
            return true; // 首次设置
        }
        false
    }

    /// Emit one idempotent lifecycle failure for tasks interrupted by restart.
    pub async fn replay_restored_failure_notifications(&self, agent_id: Option<&str>) -> usize {
        if self.event_emitter_for_agent(agent_id).is_none() {
            return 0;
        }

        let agent_key = normalized_agent_key(agent_id);
        let tasks_to_notify = {
            let mut tasks = self.tasks.lock().await;
            tasks
                .values_mut()
                .filter(|task| {
                    task.restored_after_restart
                        && task.status == TaskStatus::Failed
                        && task.emit_system_events
                        && !task.notified
                        && normalized_agent_key(task.agent_id.as_deref()) == agent_key
                })
                .map(|task| {
                    task.notified = true;
                    task.clone()
                })
                .collect::<Vec<_>>()
        };

        for task in &tasks_to_notify {
            self.emit_lifecycle_event(task, "failed");
            self.persist_if_configured(task).await;
        }
        tasks_to_notify.len()
    }

    /// 设置失败并延迟清理（失败后保留一段时间供用户查看）
    /// 参考: Claude Code LocalAgentTask.tsx evictAfter
    pub async fn set_failed_with_grace(&self, task_id: &str, error: &str) {
        let updated = {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.get_mut(task_id) {
                // 防止覆盖已取消/已完成的任务状态
                if matches!(
                    task.status,
                    TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
                ) {
                    tracing::warn!(
                        task_id = %task_id,
                        current_status = ?task.status,
                        "set_failed_with_grace: task already in terminal state, ignoring"
                    );
                    None
                } else {
                    task.status = TaskStatus::Failed;
                    task.completed_at = Some(Utc::now());
                    task.error = Some(error.to_string());
                    // 设置延迟清理
                    task.evict_after =
                        Some(Utc::now() + chrono::Duration::seconds(EVICT_GRACE_PERIOD_SECS));
                    Some(task.clone())
                }
            } else {
                None
            }
        };

        if let Some(task) = updated {
            // 清理残留消息，防止内存泄漏（与 set_failed 保持一致）
            let orphaned = self.drain_pending_messages(task_id);
            if !orphaned.is_empty() {
                tracing::warn!(
                    task_id = %task_id,
                    count = orphaned.len(),
                    "Messages in queue when task failed (grace), discarding"
                );
            }
            self.unregister_abort_token(task_id);
            self.release_workspace_scope(task_id);

            self.emit_lifecycle_event(&task, "failed");
            self.persist_if_configured(&task).await;
        }
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct OriginScopedTaskManager {
    inner: TaskManager,
    channel: String,
    chat_id: String,
}

impl OriginScopedTaskManager {
    pub fn new(inner: TaskManager, channel: impl Into<String>, chat_id: impl Into<String>) -> Self {
        Self {
            inner,
            channel: channel.into(),
            chat_id: chat_id.into(),
        }
    }

    async fn get_owned_task(&self, task_id: &str) -> Option<TaskInfo> {
        self.inner
            .get_task(task_id)
            .await
            .filter(|task| task.belongs_to_origin(&self.channel, &self.chat_id))
    }
}

fn task_summary_json(t: &TaskInfo) -> serde_json::Value {
    json!({
        "id": t.id,
        "label": t.label,
        "task": t.task_description,
        "status": t.status.to_string(),
        "created_at": t.created_at.to_rfc3339(),
        "started_at": t.started_at.map(|d| d.to_rfc3339()),
        "completed_at": t.completed_at.map(|d| d.to_rfc3339()),
        "progress": t.progress,
        "result": t.result,
        "error": t.error,
        "agent_id": t.agent_id,
        "agent_type": t.agent_type,
        "one_shot": t.one_shot,
    })
}

fn task_detail_json(t: &TaskInfo) -> serde_json::Value {
    let mut value = task_summary_json(t);
    if let Some(object) = value.as_object_mut() {
        object.insert("origin_channel".to_string(), json!(t.origin_channel));
        object.insert("origin_chat_id".to_string(), json!(t.origin_chat_id));
    }
    value
}

#[async_trait]
impl TaskManagerOps for OriginScopedTaskManager {
    async fn cancel_task(&self, task_id: &str) -> Result<(), Error> {
        let task = self
            .get_owned_task(task_id)
            .await
            .ok_or_else(|| Error::NotFound(format!("Task not found: {}", task_id)))?;
        if task.status == TaskStatus::Cancelled {
            return Ok(());
        }
        self.inner.cancel_task(task_id).await
    }

    async fn list_tasks_json(&self, status_filter: Option<String>) -> serde_json::Value {
        let filter = status_filter.and_then(|status| match status.as_str() {
            "queued" => Some(TaskStatus::Queued),
            "running" => Some(TaskStatus::Running),
            "completed" => Some(TaskStatus::Completed),
            "failed" => Some(TaskStatus::Failed),
            "cancelled" => Some(TaskStatus::Cancelled),
            _ => None,
        });
        serde_json::Value::Array(
            self.inner
                .list_tasks_for_origin(&self.channel, &self.chat_id, filter.as_ref())
                .await
                .iter()
                .map(task_summary_json)
                .collect(),
        )
    }

    async fn get_task_json(&self, task_id: &str) -> Option<serde_json::Value> {
        self.get_owned_task(task_id)
            .await
            .as_ref()
            .map(task_detail_json)
    }

    async fn summary_json(&self) -> serde_json::Value {
        let (queued, running, completed, failed) = self
            .inner
            .summary_for_origin(&self.channel, &self.chat_id)
            .await;
        let cancelled = self
            .inner
            .list_tasks_for_origin(&self.channel, &self.chat_id, Some(&TaskStatus::Cancelled))
            .await
            .len();
        json!({
            "queued": queued,
            "running": running,
            "completed": completed,
            "failed": failed,
            "cancelled": cancelled,
            "total": queued + running + completed + failed + cancelled
        })
    }

    async fn send_message(&self, task_id: &str, message: String) -> Result<(), Error> {
        if self.get_owned_task(task_id).await.is_none() {
            return Err(Error::NotFound(format!("Task not found: {}", task_id)));
        }
        self.inner.send_message(task_id, message).await
    }

    async fn is_one_shot_task(&self, task_id: &str) -> bool {
        self.get_owned_task(task_id)
            .await
            .is_some_and(|task| task.one_shot)
    }

    fn register_shell_process(&self, session_id: &str, pid: u32) {
        self.inner.register_shell_process(session_id, pid);
    }

    fn unregister_shell_process(&self, session_id: &str) {
        self.inner.unregister_shell_process(session_id);
    }
}

#[async_trait]
impl TaskManagerOps for TaskManager {
    async fn cancel_task(&self, task_id: &str) -> Result<(), Error> {
        TaskManager::cancel_task(self, task_id).await
    }

    async fn list_tasks_json(&self, status_filter: Option<String>) -> serde_json::Value {
        let filter = status_filter.and_then(|s| match s.as_str() {
            "queued" => Some(TaskStatus::Queued),
            "running" => Some(TaskStatus::Running),
            "completed" => Some(TaskStatus::Completed),
            "failed" => Some(TaskStatus::Failed),
            "cancelled" => Some(TaskStatus::Cancelled),
            _ => None,
        });
        let tasks = self.list_tasks(filter.as_ref()).await;
        let items: Vec<serde_json::Value> = tasks.iter().map(task_summary_json).collect();
        serde_json::Value::Array(items)
    }

    async fn get_task_json(&self, task_id: &str) -> Option<serde_json::Value> {
        self.get_task(task_id).await.as_ref().map(task_detail_json)
    }

    async fn summary_json(&self) -> serde_json::Value {
        let (queued, running, completed, failed_with_cancelled) = self.summary().await;
        let cancelled = self.list_tasks(Some(&TaskStatus::Cancelled)).await.len();
        let failed = failed_with_cancelled.saturating_sub(cancelled);
        json!({
            "queued": queued,
            "running": running,
            "completed": completed,
            "failed": failed,
            "cancelled": cancelled,
            "total": queued + running + completed + failed + cancelled
        })
    }

    async fn send_message(&self, task_id: &str, message: String) -> Result<(), Error> {
        self.send_message(task_id, message).await
    }

    async fn is_one_shot_task(&self, task_id: &str) -> bool {
        self.is_one_shot_task(task_id).await
    }

    fn register_shell_process(&self, session_id: &str, pid: u32) {
        self.register_shell_process(session_id, pid);
    }

    fn unregister_shell_process(&self, session_id: &str) {
        self.unregister_shell_process(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn shell_process_registration_can_be_cleaned_up() {
        let manager = TaskManager::new();

        manager.register_shell_process("shell-session", 12345);
        assert_eq!(manager.registered_shell_process_count(), 1);
        manager.unregister_shell_process("shell-session");
        assert_eq!(manager.registered_shell_process_count(), 0);
    }

    #[tokio::test]
    async fn test_cleanup_loop_exits_on_shutdown_signal() {
        let manager = Arc::new(TaskManager::new());
        let workspace = tempfile::tempdir().expect("temp workspace");
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let handle = manager.spawn_cleanup_loop(workspace.path(), shutdown_rx);

        tokio::task::yield_now().await;
        let _ = shutdown_tx.send(());

        let result = tokio::time::timeout(Duration::from_millis(200), handle).await;
        assert!(result.is_ok(), "cleanup loop did not exit after shutdown");
        result.unwrap().expect("cleanup loop panicked");
    }

    #[tokio::test]
    async fn test_task_manager_tracks_agent_scoped_tasks_in_memory() {
        let manager = TaskManager::new();
        manager
            .create_task(
                "task-1",
                "demo",
                "do something",
                "cli",
                "chat-1",
                Some("ops"),
                false,
                None,  // agent_type
                false, // one_shot
            )
            .await;

        let tasks = manager.list_tasks(None).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].agent_id.as_deref(), Some("ops"));
    }

    #[tokio::test]
    async fn test_task_manager_origin_scope_excludes_foreign_tasks() {
        let manager = TaskManager::new();
        for (id, channel, chat_id) in [
            ("task-local", "ws", "chat-a"),
            ("task-other-chat", "ws", "chat-b"),
            ("task-other-channel", "cli", "chat-a"),
        ] {
            manager
                .create_and_start_task(
                    id,
                    id,
                    "scope test",
                    channel,
                    chat_id,
                    None,
                    false,
                    Some("explore"),
                    true,
                )
                .await;
        }

        let tasks = manager
            .list_tasks_for_origin("ws", "chat-a", Some(&TaskStatus::Running))
            .await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "task-local");

        assert!(manager
            .find_task_by_prefix_for_origin("task-other", "ws", "chat-a")
            .await
            .is_empty());
        assert_eq!(
            manager.summary_for_origin("ws", "chat-a").await,
            (0, 1, 0, 0)
        );
    }

    #[tokio::test]
    async fn test_origin_scoped_task_manager_ops_hide_foreign_tasks() {
        let manager = TaskManager::new();
        for (id, chat_id) in [("task-local", "chat-a"), ("task-foreign", "chat-b")] {
            manager
                .create_and_start_task(
                    id,
                    id,
                    "scope test",
                    "ws",
                    chat_id,
                    None,
                    false,
                    None,
                    false,
                )
                .await;
        }
        let scoped = OriginScopedTaskManager::new(manager, "ws", "chat-a");

        let listed = TaskManagerOps::list_tasks_json(&scoped, None).await;
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert!(TaskManagerOps::get_task_json(&scoped, "task-foreign")
            .await
            .is_none());
        assert!(
            TaskManagerOps::send_message(&scoped, "task-foreign", "hello".to_string())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn origin_scoped_cancel_rejects_foreign_tasks_and_cancels_owned_ones() {
        let manager = TaskManager::new();
        for (id, chat_id) in [("task-owned", "chat-a"), ("task-foreign", "chat-b")] {
            manager
                .create_and_start_task(
                    id,
                    id,
                    "cancel scope test",
                    "ws",
                    chat_id,
                    None,
                    false,
                    None,
                    false,
                )
                .await;
        }
        let scoped = OriginScopedTaskManager::new(manager.clone(), "ws", "chat-a");

        let denied = TaskManagerOps::cancel_task(&scoped, "task-foreign").await;
        assert!(denied.is_err(), "foreign task must not be cancellable");
        assert_eq!(
            manager
                .get_task("task-foreign")
                .await
                .map(|task| task.status),
            Some(TaskStatus::Running)
        );

        TaskManagerOps::cancel_task(&scoped, "task-owned")
            .await
            .expect("owned task should be cancellable");
        assert_eq!(
            manager.get_task("task-owned").await.map(|task| task.status),
            Some(TaskStatus::Cancelled)
        );

        // Cancelling twice is idempotent rather than an error.
        TaskManagerOps::cancel_task(&scoped, "task-owned")
            .await
            .expect("repeat cancel should be a no-op");
        assert!(TaskManagerOps::cancel_task(&scoped, "task-missing")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_task_manager_json_exposes_agent_type_for_duplicate_checks() {
        let manager = TaskManager::new();
        manager
            .create_and_start_task(
                "task-typed",
                "explore",
                "inspect code",
                "cli",
                "chat-1",
                Some("ops"),
                false,
                Some("explore"),
                true,
            )
            .await;

        let json = TaskManagerOps::list_tasks_json(&manager, Some("running".to_string())).await;
        let items = json.as_array().expect("task json array");
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].get("agent_type").and_then(|v| v.as_str()),
            Some("explore")
        );
        assert_eq!(
            items[0].get("one_shot").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[derive(Clone, Default)]
    struct RecordingEmitter {
        events: Arc<StdMutex<Vec<SystemEvent>>>,
    }

    impl RecordingEmitter {
        fn handle(&self) -> EventEmitterHandle {
            Arc::new(self.clone())
        }

        fn kinds(&self) -> Vec<String> {
            self.events
                .lock()
                .expect("task manager recording emitter lock poisoned")
                .iter()
                .map(|event| event.kind.clone())
                .collect()
        }

        fn events(&self) -> Vec<SystemEvent> {
            self.events
                .lock()
                .expect("task manager recording emitter lock poisoned")
                .clone()
        }
    }

    impl blockcell_tools::SystemEventEmitter for RecordingEmitter {
        fn emit(&self, event: SystemEvent) {
            self.events
                .lock()
                .expect("task manager recording emitter lock poisoned")
                .push(event);
        }
    }

    #[tokio::test]
    async fn test_task_manager_event_emits_lifecycle_updates() {
        let manager = TaskManager::new();
        let emitter = RecordingEmitter::default();
        manager.register_event_emitter(Some("ops"), emitter.handle());

        manager
            .create_task(
                "task-1",
                "demo",
                "do something",
                "cli",
                "chat-1",
                Some("ops"),
                true,
                None,  // agent_type
                false, // one_shot
            )
            .await;
        manager.set_running("task-1").await;
        manager.set_completed("task-1", "done").await;

        assert_eq!(
            emitter.kinds(),
            vec![
                "task.created".to_string(),
                "task.running".to_string(),
                "task.completed".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn task_lifecycle_event_stays_with_origin_after_target_rotation() {
        let manager = TaskManager::new();
        let emitter = RecordingEmitter::default();
        manager.register_event_emitter(Some("ops"), emitter.handle());

        manager
            .create_and_start_task_with_route(
                "task-origin",
                "private report",
                "inspect private data",
                "ws",
                "chat-a",
                Some("account-a"),
                Some("ws:account:9:account-achat-a"),
                Some("ops"),
                true,
                Some("explore"),
                false,
            )
            .await;
        manager
            .set_completed("task-origin", "secret result from chat A")
            .await;

        let completed = emitter
            .events()
            .into_iter()
            .find(|event| event.kind == "task.completed")
            .expect("completed lifecycle event");
        assert_eq!(
            completed.scope,
            blockcell_core::system_event::EventScope::Session {
                channel: "ws".to_string(),
                account_id: Some("account-a".to_string()),
                chat_id: "chat-a".to_string(),
                session_key: "ws:account:9:account-achat-a".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn test_task_manager_event_skips_non_notifying_tasks() {
        let manager = TaskManager::new();
        let emitter = RecordingEmitter::default();
        manager.register_event_emitter(Some("ops"), emitter.handle());

        manager
            .create_task(
                "task-1",
                "demo",
                "do something",
                "cli",
                "chat-1",
                Some("ops"),
                false,
                None,  // agent_type
                false, // one_shot
            )
            .await;
        manager.set_running("task-1").await;
        manager.set_failed("task-1", "boom").await;

        assert!(emitter.kinds().is_empty());
    }

    #[tokio::test]
    async fn test_cancel_task_cancels_registered_abort_token() {
        let manager = TaskManager::new();
        let token = AbortToken::new();
        manager
            .create_and_start_task(
                "task-cancel-token",
                "demo",
                "cancel token test",
                "cli",
                "chat-1",
                None,
                false,
                Some("explore"),
                true,
            )
            .await;
        manager.register_abort_token("task-cancel-token", token.clone());

        manager.cancel_task("task-cancel-token").await.unwrap();

        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn set_cancelled_records_reason_and_cancels_token() {
        let manager = TaskManager::new();
        let token = AbortToken::new();
        manager
            .create_and_start_task(
                "task-parent-cancelled",
                "demo",
                "parent cancellation test",
                "cli",
                "chat-1",
                None,
                false,
                Some("explore"),
                true,
            )
            .await;
        manager.register_abort_token("task-parent-cancelled", token.clone());

        manager
            .set_cancelled("task-parent-cancelled", "父任务已取消")
            .await
            .unwrap();

        let task = manager
            .get_task("task-parent-cancelled")
            .await
            .expect("cancelled task should remain queryable");
        assert_eq!(task.status, TaskStatus::Cancelled);
        assert_eq!(task.error.as_deref(), Some("父任务已取消"));
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn test_complete_task_does_not_broadcast_after_cancelled() {
        let manager = TaskManager::new();
        let mut completed_rx = manager.subscribe_completed_events();
        manager
            .create_and_start_task(
                "task-cancel-complete",
                "demo",
                "cancel complete test",
                "cli",
                "chat-1",
                None,
                false,
                Some("explore"),
                true,
            )
            .await;

        manager.cancel_task("task-cancel-complete").await.unwrap();
        manager
            .complete_task(
                "task-cancel-complete",
                "explore".to_string(),
                AgentResult::success("late result".to_string()),
            )
            .await;

        let recv_result =
            tokio::time::timeout(std::time::Duration::from_millis(50), completed_rx.recv()).await;
        assert!(recv_result.is_err());
        assert_eq!(
            manager
                .get_task("task-cancel-complete")
                .await
                .unwrap()
                .status,
            TaskStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn test_task_manager_removes_task() {
        let manager = TaskManager::new();
        manager
            .create_task(
                "task-1",
                "demo",
                "do something",
                "cli",
                "chat-1",
                Some("ops"),
                false,
                None,  // agent_type
                false, // one_shot
            )
            .await;

        manager.remove_task("task-1").await;
        let tasks = manager.list_tasks(None).await;
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn test_persist_task_to_disk() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let workspace = temp_dir.path();

        let manager = TaskManager::with_workspace(workspace);
        manager
            .create_task(
                "persist-test-1",
                "demo",
                "persist test",
                "cli",
                "chat-1",
                None,
                false,
                None,
                false,
            )
            .await;

        // 验证文件已创建
        let tasks_dir = workspace.join(".blockcell").join("tasks");
        let file_path = tasks_dir.join("persist-test-1.json");
        assert!(file_path.exists(), "Task file should be persisted");

        // 验证文件内容
        let content: String = tokio::fs::read_to_string(&file_path)
            .await
            .expect("Failed to read task file");
        assert!(
            content.contains("persist-test-1"),
            "File should contain task ID"
        );
        assert!(
            content.contains("persist test"),
            "File should contain prompt"
        );
    }

    #[tokio::test]
    async fn test_remove_task_deletes_persisted_file() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let workspace = temp_dir.path();
        let manager = TaskManager::with_workspace(workspace);
        manager
            .create_task(
                "remove-persisted-1",
                "demo",
                "remove persisted test",
                "cli",
                "chat-1",
                None,
                false,
                None,
                false,
            )
            .await;

        let file_path = workspace
            .join(".blockcell")
            .join("tasks")
            .join("remove-persisted-1.json");
        assert!(file_path.exists(), "Task file should exist after creation");

        manager.remove_task("remove-persisted-1").await;

        assert!(
            !file_path.exists(),
            "Task file should be deleted after remove_task"
        );
    }

    #[tokio::test]
    async fn test_cleanup_task_file() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let workspace = temp_dir.path();

        let manager = TaskManager::with_workspace(workspace);
        manager
            .create_task(
                "cleanup-test-1",
                "demo",
                "cleanup test",
                "cli",
                "chat-1",
                None,
                false,
                None,
                false,
            )
            .await;

        // 文件应该存在
        let file_path = workspace
            .join(".blockcell")
            .join("tasks")
            .join("cleanup-test-1.json");
        assert!(file_path.exists(), "Task file should exist after creation");

        // 完成任务 - 文件仍然存在（仅在 evict 后才删除）
        manager.set_completed("cleanup-test-1", "done").await;
        assert!(
            file_path.exists(),
            "Task file should still exist after completion"
        );

        // 手动调用 cleanup_task_file 模拟 eviction 清理
        manager.cleanup_task_file(workspace, "cleanup-test-1").await;

        // 现在文件应该被删除
        assert!(
            !file_path.exists(),
            "Task file should be deleted after cleanup_task_file"
        );
    }

    #[tokio::test]
    async fn test_restore_from_disk_persists_failed_state() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let workspace = temp_dir.path();
        let manager = TaskManager::with_workspace(workspace);
        manager
            .create_and_start_task(
                "restore-running-1",
                "restore",
                "restore test",
                "cli",
                "chat-1",
                None,
                false,
                Some("explore"),
                true,
            )
            .await;

        let restored_manager = TaskManager::with_workspace(workspace);
        assert_eq!(restored_manager.restore_from_disk(workspace).await, 1);

        let restored = restored_manager
            .get_task("restore-running-1")
            .await
            .expect("restored task");
        assert_eq!(restored.status, TaskStatus::Failed);

        let file_path = workspace
            .join(".blockcell")
            .join("tasks")
            .join("restore-running-1.json");
        let content = tokio::fs::read_to_string(&file_path)
            .await
            .expect("read restored task file");
        let persisted: TaskInfo = serde_json::from_str(&content).expect("parse restored task file");
        assert_eq!(persisted.status, TaskStatus::Failed);
    }

    #[tokio::test]
    async fn restored_interrupted_task_emits_failure_once() {
        let temp_dir = tempfile::TempDir::new().expect("temp workspace");
        let workspace = temp_dir.path();
        let manager = TaskManager::with_workspace(workspace);
        manager
            .create_and_start_task_with_route(
                "restore-notify-1",
                "background report",
                "restore notification test",
                "telegram",
                "chat-a",
                Some("account-a"),
                Some("telegram:account:9:account-achat-a"),
                Some("ops"),
                true,
                Some("explore"),
                false,
            )
            .await;

        let restored_manager = TaskManager::with_workspace(workspace);
        assert_eq!(restored_manager.restore_from_disk(workspace).await, 1);
        let emitter = RecordingEmitter::default();
        restored_manager.register_event_emitter(Some("ops"), emitter.handle());

        assert_eq!(
            restored_manager
                .replay_restored_failure_notifications(Some("ops"))
                .await,
            1
        );
        assert_eq!(
            restored_manager
                .replay_restored_failure_notifications(Some("ops"))
                .await,
            0
        );
        assert_eq!(
            emitter
                .events()
                .iter()
                .filter(|event| event.kind == "task.failed")
                .count(),
            1
        );

        let file_path = workspace
            .join(".blockcell")
            .join("tasks")
            .join("restore-notify-1.json");
        let content = tokio::fs::read_to_string(file_path)
            .await
            .expect("read restored task");
        let persisted: TaskInfo = serde_json::from_str(&content).expect("parse restored task");
        assert!(persisted.notified);
        assert!(persisted.restored_after_restart);
    }

    #[tokio::test]
    async fn restore_deletes_terminal_files_without_consuming_limit() {
        let temp_dir = tempfile::TempDir::new().expect("temp workspace");
        let workspace = temp_dir.path();
        let manager = TaskManager::with_workspace(workspace);
        manager
            .create_and_start_task(
                "terminal-first",
                "finished",
                "terminal task",
                "cli",
                "chat-a",
                None,
                false,
                None,
                false,
            )
            .await;
        manager.set_completed("terminal-first", "done").await;
        manager
            .create_and_start_task(
                "unfinished-second",
                "running",
                "unfinished task",
                "cli",
                "chat-a",
                None,
                false,
                None,
                false,
            )
            .await;

        let restored_manager = TaskManager::with_workspace(workspace);
        assert_eq!(
            restored_manager
                .restore_from_disk_with_limit(workspace, 1)
                .await,
            1
        );
        assert!(restored_manager
            .get_task("unfinished-second")
            .await
            .is_some());
        assert!(!workspace
            .join(".blockcell")
            .join("tasks")
            .join("terminal-first.json")
            .exists());
    }

    #[tokio::test]
    async fn test_send_progress() {
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<AgentProgress>(16);
        let manager = TaskManager::with_workspace_and_progress(
            std::path::PathBuf::new().as_path(),
            progress_tx,
        );

        // 发送进度（使用 Delta 变体）
        let progress = AgentProgress::Delta {
            task_id: "test-progress-1".to_string(),
            tokens_added: 100,
            tools_added: 2,
            total_tokens: 100,
            total_tools: 2,
        };
        let sent = manager.send_progress(progress.clone()).await;
        assert!(sent, "Progress should be sent successfully");

        // 验证接收
        let received = progress_rx.recv().await.expect("Should receive progress");
        match received {
            AgentProgress::Delta {
                task_id,
                tokens_added,
                ..
            } => {
                assert_eq!(task_id, "test-progress-1");
                assert_eq!(tokens_added, 100);
            }
            _ => panic!("Expected Delta progress"),
        }
    }

    #[tokio::test]
    async fn test_send_progress_without_channel() {
        // 无 progress channel 的 manager
        let manager = TaskManager::new();

        let progress = AgentProgress::Delta {
            task_id: "no-channel-test".to_string(),
            tokens_added: 50,
            tools_added: 1,
            total_tokens: 50,
            total_tools: 1,
        };
        let sent = manager.send_progress(progress).await;
        assert!(!sent, "Progress should not be sent without channel");
    }

    #[tokio::test]
    async fn test_set_progress_sends_stage_event() {
        // set_progress 现在也发送 Stage 事件（percent=0 表示进行中）
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<AgentProgress>(16);
        let manager = TaskManager::with_workspace_and_progress(
            std::path::PathBuf::new().as_path(),
            progress_tx,
        );

        // 创建任务
        manager
            .create_and_start_task(
                "stage-test-1",
                "stage test",
                "testing stage event",
                "cli",
                "chat-1",
                None,
                false,
                None,
                false,
            )
            .await;

        // set_progress 更新文本并发送 Stage 事件（percent=0）
        manager.set_progress("stage-test-1", "正在分析代码").await;

        // 验证内存中的 progress 字段已更新
        let tasks = manager.list_tasks(None).await;
        let task = tasks.iter().find(|t| t.id == "stage-test-1").unwrap();
        assert_eq!(task.progress.as_deref(), Some("正在分析代码"));

        // 验证收到 set_progress 的 Stage 事件
        let received = progress_rx
            .recv()
            .await
            .expect("Should receive stage event from set_progress");
        match received {
            AgentProgress::Stage {
                task_id,
                stage,
                percent,
            } => {
                assert_eq!(task_id, "stage-test-1");
                assert_eq!(stage, "正在分析代码");
                assert_eq!(percent, 0);
            }
            _ => panic!("Expected Stage progress, got {:?}", received),
        }

        // set_progress_with_percent 发送带百分比的 Stage 事件
        manager
            .set_progress_with_percent("stage-test-1", "正在执行", 50)
            .await;

        // 验证收到 Stage 事件
        let received = progress_rx
            .recv()
            .await
            .expect("Should receive stage event from set_progress_with_percent");
        match received {
            AgentProgress::Stage {
                task_id,
                stage,
                percent,
            } => {
                assert_eq!(task_id, "stage-test-1");
                assert_eq!(stage, "正在执行");
                assert_eq!(percent, 50);
            }
            _ => panic!("Expected Stage progress, got {:?}", received),
        }
    }
}
