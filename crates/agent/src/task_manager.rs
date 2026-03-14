use async_trait::async_trait;
use blockcell_core::system_event::{DeliveryPolicy, EventPriority, SystemEvent};
use blockcell_tools::{EventEmitterHandle, TaskManagerOps};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;

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
    /// Agent that owns this task. Missing values are treated as the default agent.
    pub agent_id: Option<String>,
    #[serde(default)]
    emit_system_events: bool,
}

/// Thread-safe task registry for tracking background subagent tasks.
#[derive(Clone)]
pub struct TaskManager {
    tasks: Arc<Mutex<HashMap<String, TaskInfo>>>,
    event_emitters: Arc<StdMutex<HashMap<String, EventEmitterHandle>>>,
}

fn normalized_agent_key(agent_id: Option<&str>) -> String {
    agent_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
        .to_string()
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            event_emitters: Arc::new(StdMutex::new(HashMap::new())),
        }
    }

    pub fn register_event_emitter(&self, agent_id: Option<&str>, emitter: EventEmitterHandle) {
        let mut emitters = self
            .event_emitters
            .lock()
            .expect("task manager event emitter lock poisoned");
        emitters.insert(normalized_agent_key(agent_id), emitter);
    }

    fn event_emitter_for_agent(&self, agent_id: Option<&str>) -> Option<EventEmitterHandle> {
        let emitters = self
            .event_emitters
            .lock()
            .expect("task manager event emitter lock poisoned");
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
                    .map(|result| format!("{} 已完成：{}", task.label, result))
                    .unwrap_or_else(|| format!("{} 已完成", task.label)),
                DeliveryPolicy::default(),
            ),
            "failed" => (
                EventPriority::Critical,
                "后台任务失败".to_string(),
                task.error
                    .as_ref()
                    .map(|error| format!("{} 执行失败：{}", task.label, error))
                    .unwrap_or_else(|| format!("{} 执行失败", task.label)),
                DeliveryPolicy::critical(),
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
            agent_id: agent_id.map(str::to_string),
            emit_system_events,
        };
        {
            let mut tasks = self.tasks.lock().await;
            tasks.insert(task_id.to_string(), info.clone());
        }
        self.emit_lifecycle_event(&info, "created");
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
        }
    }

    /// Update the progress note for a running task.
    pub async fn set_progress(&self, task_id: &str, progress: &str) {
        {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.get_mut(task_id) {
                task.progress = Some(progress.to_string());
            }
        }
    }

    /// Mark a task as completed with a result summary.
    pub async fn set_completed(&self, task_id: &str, result: &str) {
        let updated = {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.get_mut(task_id) {
                task.status = TaskStatus::Completed;
                task.completed_at = Some(Utc::now());
                let truncated = if result.chars().count() > 2000 {
                    let end = result
                        .char_indices()
                        .nth(2000)
                        .map(|(i, _)| i)
                        .unwrap_or(result.len());
                    format!("{}... (truncated)", &result[..end])
                } else {
                    result.to_string()
                };
                task.result = Some(truncated);
                Some(task.clone())
            } else {
                None
            }
        };

        if let Some(task) = updated {
            self.emit_lifecycle_event(&task, "completed");
        }
    }

    /// Mark a task as failed with an error message.
    pub async fn set_failed(&self, task_id: &str, error: &str) {
        let updated = {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.get_mut(task_id) {
                task.status = TaskStatus::Failed;
                task.completed_at = Some(Utc::now());
                task.error = Some(error.to_string());
                Some(task.clone())
            } else {
                None
            }
        };

        if let Some(task) = updated {
            self.emit_lifecycle_event(&task, "failed");
        }
    }

    /// Get info for a specific task.
    pub async fn get_task(&self, task_id: &str) -> Option<TaskInfo> {
        let tasks = self.tasks.lock().await;
        tasks.get(task_id).cloned()
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
        result.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        result
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
                TaskStatus::Failed | TaskStatus::Cancelled => failed += 1,
            }
        }
        (queued, running, completed, failed)
    }

    /// Remove completed/failed tasks older than the given duration.
    pub async fn cleanup_old_tasks(&self, max_age: std::time::Duration) {
        let cutoff = Utc::now() - chrono::Duration::from_std(max_age).unwrap_or_default();
        let removed = {
            let mut tasks = self.tasks.lock().await;
            let before = tasks.len();
            tasks.retain(|_, t| match t.status {
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled => {
                    t.completed_at.is_none_or(|c| c > cutoff)
                }
                _ => true,
            });
            before - tasks.len()
        };
        if removed > 0 {
            tracing::debug!(removed, "Cleaned up old tasks");
        }
    }

    /// Remove a specific task by ID.
    pub async fn remove_task(&self, task_id: &str) {
        {
            let mut tasks = self.tasks.lock().await;
            tasks.remove(task_id);
        }
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TaskManagerOps for TaskManager {
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
        let items: Vec<serde_json::Value> = tasks
            .iter()
            .map(|t| {
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
                })
            })
            .collect();
        serde_json::Value::Array(items)
    }

    async fn get_task_json(&self, task_id: &str) -> Option<serde_json::Value> {
        self.get_task(task_id).await.map(|t| {
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
                "origin_channel": t.origin_channel,
                "origin_chat_id": t.origin_chat_id,
                "agent_id": t.agent_id,
            })
        })
    }

    async fn summary_json(&self) -> serde_json::Value {
        let (queued, running, completed, failed) = self.summary().await;
        json!({
            "queued": queued,
            "running": running,
            "completed": completed,
            "failed": failed,
            "total": queued + running + completed + failed
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            )
            .await;

        let tasks = manager.list_tasks(None).await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].agent_id.as_deref(), Some("ops"));
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
            )
            .await;
        manager.set_running("task-1").await;
        manager.set_failed("task-1", "boom").await;

        assert!(emitter.kinds().is_empty());
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
            )
            .await;

        manager.remove_task("task-1").await;
        let tasks = manager.list_tasks(None).await;
        assert!(tasks.is_empty());
    }
}
