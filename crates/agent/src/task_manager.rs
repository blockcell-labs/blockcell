use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use tokio::sync::Mutex;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use blockcell_tools::TaskManagerOps;

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
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Queued => write!(f, "queued"),
            TaskStatus::Running => write!(f, "running"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Failed => write!(f, "failed"),
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
}

/// Thread-safe task registry for tracking background subagent tasks.
#[derive(Clone)]
pub struct TaskManager {
    tasks: Arc<Mutex<HashMap<String, TaskInfo>>>,
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a new task and return its ID.
    pub async fn create_task(
        &self,
        task_id: &str,
        label: &str,
        description: &str,
        origin_channel: &str,
        origin_chat_id: &str,
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
        };
        let mut tasks = self.tasks.lock().await;
        tasks.insert(task_id.to_string(), info.clone());
        info
    }

    /// Mark a task as running.
    pub async fn set_running(&self, task_id: &str) {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.get_mut(task_id) {
            task.status = TaskStatus::Running;
            task.started_at = Some(Utc::now());
        }
    }

    /// Update the progress note for a running task.
    pub async fn set_progress(&self, task_id: &str, progress: &str) {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.get_mut(task_id) {
            task.progress = Some(progress.to_string());
        }
    }

    /// Mark a task as completed with a result summary.
    pub async fn set_completed(&self, task_id: &str, result: &str) {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.get_mut(task_id) {
            task.status = TaskStatus::Completed;
            task.completed_at = Some(Utc::now());
            // Truncate result to 2000 chars for storage
            let truncated = if result.chars().count() > 2000 {
                let end = result.char_indices().nth(2000).map(|(i, _)| i).unwrap_or(result.len());
                format!("{}... (truncated)", &result[..end])
            } else {
                result.to_string()
            };
            task.result = Some(truncated);
        }
    }

    /// Mark a task as failed with an error message.
    pub async fn set_failed(&self, task_id: &str, error: &str) {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.get_mut(task_id) {
            task.status = TaskStatus::Failed;
            task.completed_at = Some(Utc::now());
            task.error = Some(error.to_string());
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
        // Sort by created_at descending (newest first)
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
                TaskStatus::Failed => failed += 1,
            }
        }
        (queued, running, completed, failed)
    }

    /// Remove completed/failed tasks older than the given duration.
    pub async fn cleanup_old_tasks(&self, max_age: std::time::Duration) {
        let cutoff = Utc::now() - chrono::Duration::from_std(max_age).unwrap_or_default();
        let mut tasks = self.tasks.lock().await;
        let before = tasks.len();
        tasks.retain(|_, t| {
            match t.status {
                TaskStatus::Completed | TaskStatus::Failed => {
                    t.completed_at.is_none_or(|c| c > cutoff)
                }
                _ => true,
            }
        });
        let removed = before - tasks.len();
        if removed > 0 {
            tracing::debug!(removed, "Cleaned up old tasks");
        }
    }

    /// Remove a specific task by ID.
    pub async fn remove_task(&self, task_id: &str) {
        let mut tasks = self.tasks.lock().await;
        tasks.remove(task_id);
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
