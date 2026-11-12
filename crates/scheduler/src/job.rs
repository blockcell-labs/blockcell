use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronJob {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub schedule: JobSchedule,
    pub payload: JobPayload,
    #[serde(default)]
    pub state: JobState,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default)]
    pub delete_after_run: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSchedule {
    pub kind: ScheduleKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub every_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
    /// For Every jobs: if true, execute immediately on first tick instead of waiting one cycle.
    /// Default: false (wait for first complete cycle).
    #[serde(default)]
    pub run_immediately: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleKind {
    At,
    Every,
    Cron,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobPayload {
    #[serde(default = "default_payload_kind")]
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub deliver: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Source channel account used for delivery; absent for legacy jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// For kind="script": detected skill file kind metadata ("rhai" | "python" | "markdown").
    /// Currently stored for bookkeeping/UI only; runtime dispatch still routes through
    /// the unified skill entry instead of branching on this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_kind: Option<String>,
    /// For kind="script": the skill directory name (e.g. "stock_monitor")
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "skillName")]
    pub skill_name: Option<String>,
}

fn default_payload_kind() -> String {
    "reminder".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct JobState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<JobStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Ok,
    Error,
    Skipped,
}
