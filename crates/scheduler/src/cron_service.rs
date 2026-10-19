use crate::job::{CronJob, JobStatus, ScheduleKind};
use blockcell_core::file_store::{atomic_write, ExclusiveFileLock};
use blockcell_core::message_receipt::{
    cancel_message_receipt, register_message_receipt, MESSAGE_RECEIPT_ID,
};
use blockcell_core::system_event::{DeliveryPolicy, EventPriority, SystemEvent};
use blockcell_core::{InboundMessage, Paths, Result};
use blockcell_tools::EventEmitterHandle;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::SystemTime;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, error, info};

#[cfg(not(test))]
const CRON_EXECUTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);
// Keep the test budget well above the 500 ms `add_job` assertions below: with
// a 500 ms receipt timeout on both sides, a slow disk could let the tick time
// out before the test completes the receipt (flaky on loaded machines).
#[cfg(test)]
const CRON_EXECUTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, Serialize, Deserialize)]
pub struct JobStore {
    pub version: u32,
    pub jobs: Vec<CronJob>,
}

impl Default for JobStore {
    fn default() -> Self {
        Self {
            version: 1,
            jobs: Vec::new(),
        }
    }
}

pub struct CronService {
    paths: Paths,
    jobs: Arc<RwLock<Vec<CronJob>>>,
    inbound_tx: mpsc::Sender<InboundMessage>,
    agent_id: Option<String>,
    event_emitter: Arc<StdMutex<Option<EventEmitterHandle>>>,
    /// Last known modification time of cron_jobs.json file.
    /// Used to skip unnecessary disk reads when file hasn't changed.
    last_file_mtime: Arc<RwLock<Option<SystemTime>>>,
    /// Flag to track if in-memory state has changes that need to be saved.
    has_unsaved_changes: Arc<RwLock<bool>>,
    /// Membership changes that must be applied while holding the disk-store lock.
    pending_added_ids: Arc<RwLock<std::collections::HashSet<String>>>,
    pending_deleted_ids: Arc<RwLock<std::collections::HashSet<String>>>,
    /// Serializes each in-process read/modify/write transaction.
    operation_lock: Arc<Mutex<()>>,
    /// Tick interval in seconds for checking due jobs.
    tick_interval_secs: u64,
    /// Default timezone for jobs without a specified timezone or with invalid timezone.
    default_timezone: Option<Tz>,
}

fn apply_route_agent_id(metadata: &mut serde_json::Value, agent_id: Option<&str>) {
    if let Some(agent_id) = agent_id.map(str::trim).filter(|id| !id.is_empty()) {
        if !metadata.is_object() {
            *metadata = serde_json::json!({});
        }
        if let Some(obj) = metadata.as_object_mut() {
            obj.insert("route_agent_id".to_string(), serde_json::json!(agent_id));
        }
    }
}

/// Parse a timezone string (e.g., "Asia/Shanghai") into a Tz.
/// Returns None and logs a warning if the timezone string is invalid.
fn parse_timezone(tz_str: &str) -> Option<Tz> {
    match tz_str.parse::<Tz>() {
        Ok(tz) => Some(tz),
        Err(e) => {
            tracing::warn!(tz = %tz_str, error = %e, "Invalid timezone string");
            None
        }
    }
}

impl CronService {
    /// Create a new CronService with default tick interval (1 second).
    pub fn new(paths: Paths, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        Self::new_with_options(paths, inbound_tx, None, None, None)
    }

    /// Create a new CronService with optional agent_id (uses default tick interval).
    pub fn new_with_agent(
        paths: Paths,
        inbound_tx: mpsc::Sender<InboundMessage>,
        agent_id: Option<String>,
    ) -> Self {
        Self::new_with_options(paths, inbound_tx, agent_id, None, None)
    }

    /// Create a new CronService with all options.
    pub fn new_with_options(
        paths: Paths,
        inbound_tx: mpsc::Sender<InboundMessage>,
        agent_id: Option<String>,
        tick_interval_secs: Option<u64>,
        default_timezone: Option<&str>,
    ) -> Self {
        // Parse default timezone string to Tz
        let default_tz = default_timezone.and_then(|tz_str| match tz_str.parse::<Tz>() {
            Ok(tz) => {
                tracing::info!(default_timezone = %tz_str, "CronService using default timezone");
                Some(tz)
            }
            Err(e) => {
                tracing::warn!(
                    default_timezone = %tz_str,
                    error = %e,
                    "Invalid default timezone string, falling back to UTC"
                );
                None
            }
        });

        Self {
            paths,
            jobs: Arc::new(RwLock::new(Vec::new())),
            inbound_tx,
            agent_id: agent_id
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty()),
            event_emitter: Arc::new(StdMutex::new(None)),
            last_file_mtime: Arc::new(RwLock::new(None)),
            has_unsaved_changes: Arc::new(RwLock::new(false)),
            pending_added_ids: Arc::new(RwLock::new(std::collections::HashSet::new())),
            pending_deleted_ids: Arc::new(RwLock::new(std::collections::HashSet::new())),
            operation_lock: Arc::new(Mutex::new(())),
            tick_interval_secs: tick_interval_secs.unwrap_or(1),
            default_timezone: default_tz,
        }
    }

    pub fn set_event_emitter(&self, emitter: EventEmitterHandle) {
        let mut slot = self
            .event_emitter
            .lock()
            .expect("cron service event emitter lock poisoned");
        *slot = Some(emitter);
    }

    pub async fn load(&self) -> Result<()> {
        let _operation_guard = self.operation_lock.lock().await;
        self.load_unlocked().await
    }

    async fn load_unlocked(&self) -> Result<()> {
        let path = self.paths.cron_jobs_file();
        if !path.exists() {
            return Ok(());
        }

        let content = tokio::fs::read_to_string(&path).await?;
        let store: JobStore = serde_json::from_str(&content)?;

        let mut jobs = self.jobs.write().await;
        // Keep overdue one-time jobs in memory so the next tick can execute them.
        // Dropping them here makes At jobs impossible to fire because every execution
        // happens after crossing `at_ms`.
        *jobs = store.jobs;

        debug!(count = jobs.len(), "Loaded cron jobs");
        Ok(())
    }

    pub async fn save(&self) -> Result<()> {
        let _operation_guard = self.operation_lock.lock().await;
        self.save_unlocked().await
    }

    async fn save_unlocked(&self) -> Result<()> {
        // Check if there are unsaved changes
        if !*self.has_unsaved_changes.read().await {
            debug!("No unsaved changes, skipping cron save");
            return Ok(());
        }

        let path = self.paths.cron_jobs_file();
        let lock_path = self.paths.cron_jobs_lock_file();
        let mut snapshot = self.jobs.read().await.clone();
        let added_ids = self.pending_added_ids.read().await.clone();
        let deleted_ids = self.pending_deleted_ids.read().await.clone();

        let _store_lock =
            tokio::task::spawn_blocking(move || ExclusiveFileLock::acquire(&lock_path))
                .await
                .map_err(|error| {
                    blockcell_core::Error::Other(format!("cron lock task failed: {error}"))
                })??;

        let disk_existed = path.exists();
        let disk_store = if disk_existed {
            let content = tokio::fs::read_to_string(&path).await?;
            serde_json::from_str::<JobStore>(&content)?
        } else {
            JobStore::default()
        };
        let memory_by_id: std::collections::HashMap<String, CronJob> = snapshot
            .drain(..)
            .map(|job| (job.id.clone(), job))
            .collect();
        let mut merged = Vec::new();
        let mut persisted_ids = std::collections::HashSet::new();
        for disk_job in disk_store.jobs {
            if deleted_ids.contains(&disk_job.id) {
                continue;
            }
            let job = memory_by_id.get(&disk_job.id).cloned().unwrap_or(disk_job);
            persisted_ids.insert(job.id.clone());
            merged.push(job);
        }
        for (id, job) in memory_by_id {
            if !persisted_ids.contains(&id)
                && !deleted_ids.contains(&id)
                && (!disk_existed || added_ids.contains(&id))
            {
                persisted_ids.insert(id);
                merged.push(job);
            }
        }

        let store = JobStore {
            version: 1,
            jobs: merged.clone(),
        };
        let content = serde_json::to_string_pretty(&store)?;
        let write_path = path.clone();
        tokio::task::spawn_blocking(move || atomic_write(&write_path, content.as_bytes()))
            .await
            .map_err(|error| {
                blockcell_core::Error::Other(format!("cron write task failed: {error}"))
            })??;

        *self.jobs.write().await = merged;

        // Update the recorded modification time (our own write)
        let mtime = tokio::fs::metadata(&path)
            .await
            .ok()
            .and_then(|m| m.modified().ok());
        *self.last_file_mtime.write().await = mtime;

        // Clear the unsaved changes flag
        *self.has_unsaved_changes.write().await = false;
        self.pending_added_ids.write().await.clear();
        self.pending_deleted_ids.write().await.clear();

        debug!("Cron jobs saved to disk");
        Ok(())
    }

    pub async fn add_job(&self, job: CronJob) -> Result<()> {
        let _operation_guard = self.operation_lock.lock().await;
        let added_id = job.id.clone();
        let mut jobs = self.jobs.write().await;
        // Check for duplicate ID
        if jobs.iter().any(|j| j.id == job.id) {
            tracing::warn!(job_id = %job.id, "Duplicate job ID, replacing existing job");
            jobs.retain(|j| j.id != job.id);
        }
        jobs.push(job);
        drop(jobs);
        self.pending_added_ids
            .write()
            .await
            .insert(added_id.clone());
        self.pending_deleted_ids.write().await.remove(&added_id);
        *self.has_unsaved_changes.write().await = true;
        self.save_unlocked().await
    }

    pub async fn remove_job(&self, id: &str) -> Result<bool> {
        let _operation_guard = self.operation_lock.lock().await;
        let mut jobs = self.jobs.write().await;
        let len_before = jobs.len();
        jobs.retain(|j| j.id != id);
        let removed = jobs.len() < len_before;
        drop(jobs);

        if removed {
            self.pending_deleted_ids
                .write()
                .await
                .insert(id.to_string());
            self.pending_added_ids.write().await.remove(id);
            *self.has_unsaved_changes.write().await = true;
            self.save_unlocked().await?;
        }
        Ok(removed)
    }

    pub async fn list_jobs(&self) -> Vec<CronJob> {
        self.jobs.read().await.clone()
    }

    /// Update the enabled state of a job by ID prefix. Returns the job name if found.
    pub async fn update_job_enabled(
        &self,
        id_prefix: &str,
        enabled: bool,
    ) -> Result<Option<String>> {
        let _operation_guard = self.operation_lock.lock().await;
        let mut jobs = self.jobs.write().await;
        let matching: Vec<usize> = jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| j.id.starts_with(id_prefix))
            .map(|(i, _)| i)
            .collect();

        match matching.len() {
            0 => Ok(None),
            1 => {
                let job = &mut jobs[matching[0]];
                job.enabled = enabled;
                job.updated_at_ms = chrono::Utc::now().timestamp_millis();
                let name = job.name.clone();
                drop(jobs);
                *self.has_unsaved_changes.write().await = true;
                self.save_unlocked().await?;
                Ok(Some(name))
            }
            _ => {
                // Multiple matches — return Err with disambiguation hint
                let names: Vec<String> = matching
                    .iter()
                    .map(|&i| {
                        format!(
                            "{} ({})",
                            &jobs[i].id.chars().take(8).collect::<String>(),
                            jobs[i].name
                        )
                    })
                    .collect();
                Err(blockcell_core::Error::Other(format!(
                    "Multiple jobs match '{}': {}",
                    id_prefix,
                    names.join(", ")
                )))
            }
        }
    }

    /// Reload from disk while preserving in-memory execution state (next_run_at_ms /
    /// last_run_at_ms) for jobs that have already been initialized this session.
    /// This avoids the old `load()` bug where a full replace would clobber in-memory
    /// scheduling state and could cause jobs to re-fire or never fire.
    ///
    /// Returns `true` if the file was actually read (had changes), `false` if skipped.
    async fn merge_load(&self) -> Result<bool> {
        let path = self.paths.cron_jobs_file();
        if !path.exists() {
            return Ok(false);
        }

        // Check file modification time to skip unnecessary reads
        let current_mtime = tokio::fs::metadata(&path)
            .await
            .ok()
            .and_then(|m| m.modified().ok());

        let last_mtime = *self.last_file_mtime.read().await;

        // If file hasn't changed, skip the read
        if let (Some(current), Some(last)) = (current_mtime, last_mtime) {
            if current == last {
                debug!("Cron jobs file unchanged, skipping reload");
                return Ok(false);
            }
        }

        // File has changed (or no previous mtime), read it
        let content = tokio::fs::read_to_string(&path).await?;
        let store: JobStore = serde_json::from_str(&content)?;

        let mut mem_jobs = self.jobs.write().await;
        // Capture execution state for existing jobs by ID.
        let mem_state: std::collections::HashMap<String, (Option<i64>, Option<i64>)> = mem_jobs
            .iter()
            .map(|j| {
                (
                    j.id.clone(),
                    (j.state.next_run_at_ms, j.state.last_run_at_ms),
                )
            })
            .collect();

        let mut new_jobs = store.jobs;

        // Replace with disk state, restoring in-memory scheduling state where present.
        for job in new_jobs.iter_mut() {
            if let Some((next_run, last_run)) = mem_state.get(&job.id) {
                if next_run.is_some() {
                    job.state.next_run_at_ms = *next_run;
                }
                if last_run.is_some() {
                    job.state.last_run_at_ms = *last_run;
                }
            }
        }
        *mem_jobs = new_jobs;

        // Update the recorded modification time
        *self.last_file_mtime.write().await = current_mtime;

        debug!(
            count = mem_jobs.len(),
            "Loaded cron jobs (merged with in-memory state)"
        );
        Ok(true)
    }

    pub async fn run_tick(&self) -> Result<()> {
        // The operation lock only guards the short schedule/commit critical sections
        // below — NOT job delivery. `execute_job_internal` waits for the runtime
        // receipt (production timeout 15 min), so holding the lock across it would
        // stall `add_job`/`remove_job`/`save` (and the gateway write API) for the
        // whole wait, and delay every other due job.
        let operation_guard = self.operation_lock.lock().await;
        // Reload from disk, merging in-memory execution state for already-initialized jobs.
        // New jobs added by CronTool (disk-only) are picked up; existing job scheduling
        // state (next_run_at_ms / last_run_at_ms) is preserved to avoid double-firing.
        let _file_changed = match self.merge_load().await {
            Ok(changed) => changed,
            Err(e) => {
                error!(error = %e.to_string(), "Failed to reload cron jobs from disk");
                false
            }
        };

        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut jobs = self.jobs.write().await;
        let mut jobs_to_run = Vec::new();
        let mut state_changed = false;

        for job in jobs.iter_mut() {
            if !job.enabled {
                continue;
            }

            // Guard: skip one-time (At) jobs that have already fired
            if job.schedule.kind == ScheduleKind::At && job.state.last_run_at_ms.is_some() {
                job.enabled = false;
                state_changed = true;
                continue;
            }

            // Parse timezone for this job
            let tz: Option<Tz> = job
                .schedule
                .tz
                .as_ref()
                .and_then(|tz_str| {
                    let parsed = parse_timezone(tz_str);
                    if parsed.is_none() {
                        tracing::warn!(
                            job_id = %job.id,
                            tz = %tz_str,
                            default_tz = ?self.default_timezone,
                            "Invalid timezone string, falling back to default timezone or UTC"
                        );
                    }
                    parsed
                })
                .or(self.default_timezone);

            let previous_next_run = job.state.next_run_at_ms;
            let should_run = match &job.state.next_run_at_ms {
                Some(next) => *next <= now_ms,
                None => self.calculate_next_run(job, now_ms, tz.as_ref()),
            };
            if job.state.next_run_at_ms != previous_next_run {
                state_changed = true;
            }

            if should_run {
                jobs_to_run.push(job.clone());
            }
        }
        drop(jobs);

        // Persist schedule initialization before delivery. Completion is not
        // persisted until the inbound channel acknowledges the message.
        if state_changed {
            *self.has_unsaved_changes.write().await = true;
            self.save_unlocked().await?;
        }

        // Deliver due jobs, then commit completion only for acknowledged sends.
        // Release the operation lock first: delivery can block for minutes.
        drop(operation_guard);
        let inbound_tx = self.inbound_tx.clone();
        let event_emitter = self.event_emitter.clone();
        let agent_id = self.agent_id.clone();
        let mut delivery_results = Vec::with_capacity(jobs_to_run.len());
        for job in &jobs_to_run {
            let result = Self::execute_job_internal(
                job,
                inbound_tx.clone(),
                event_emitter.clone(),
                agent_id.clone(),
            )
            .await;
            delivery_results.push((job.id.clone(), result));
        }

        if delivery_results.is_empty() {
            return Ok(());
        }

        // Re-acquire the operation lock only for the short commit section so the
        // result merge stays serialized against concurrent add/remove/save.
        let _operation_guard = self.operation_lock.lock().await;
        let mut jobs = self.jobs.write().await;
        let mut delete_ids = Vec::new();
        for (job_id, delivery) in delivery_results {
            let Some(job) = jobs.iter_mut().find(|job| job.id == job_id) else {
                continue;
            };
            match delivery {
                Ok(()) => {
                    job.state.last_run_at_ms = Some(now_ms);
                    job.state.last_status = Some(JobStatus::Ok);
                    job.state.last_error = None;
                    match job.schedule.kind {
                        ScheduleKind::At => {
                            job.state.next_run_at_ms = None;
                            job.enabled = false;
                        }
                        ScheduleKind::Every => {
                            if let Some(every_ms) = job.schedule.every_ms {
                                job.state.next_run_at_ms = now_ms.checked_add(every_ms);
                            }
                        }
                        ScheduleKind::Cron => {
                            let tz = job
                                .schedule
                                .tz
                                .as_deref()
                                .and_then(parse_timezone)
                                .or(self.default_timezone);
                            if let Some(expr) = &job.schedule.expr {
                                job.state.next_run_at_ms =
                                    self.calculate_next_cron_run_ms(expr, tz.as_ref());
                            }
                        }
                    }
                    if job.delete_after_run {
                        delete_ids.push(job.id.clone());
                    }
                }
                Err(error) => {
                    job.state.last_status = Some(JobStatus::Error);
                    job.state.last_error = Some(error);
                    if job.schedule.kind != ScheduleKind::At {
                        match job.schedule.kind {
                            ScheduleKind::Every => {
                                job.state.next_run_at_ms = job
                                    .schedule
                                    .every_ms
                                    .and_then(|every_ms| now_ms.checked_add(every_ms));
                            }
                            ScheduleKind::Cron => {
                                let tz = job
                                    .schedule
                                    .tz
                                    .as_deref()
                                    .and_then(parse_timezone)
                                    .or(self.default_timezone);
                                if let Some(expr) = &job.schedule.expr {
                                    job.state.next_run_at_ms =
                                        self.calculate_next_cron_run_ms(expr, tz.as_ref());
                                }
                            }
                            ScheduleKind::At => {}
                        }
                    }
                }
            }
        }
        if !delete_ids.is_empty() {
            jobs.retain(|job| !delete_ids.contains(&job.id));
            info!(count = delete_ids.len(), "Deleted completed cron jobs");
        }
        drop(jobs);
        self.pending_deleted_ids.write().await.extend(delete_ids);
        *self.has_unsaved_changes.write().await = true;
        self.save_unlocked().await?;
        Ok(())
    }

    /// Internal execute function that can be called from spawned tasks
    async fn execute_job_internal(
        job: &CronJob,
        inbound_tx: mpsc::Sender<InboundMessage>,
        event_emitter: Arc<StdMutex<Option<EventEmitterHandle>>>,
        agent_id: Option<String>,
    ) -> std::result::Result<(), String> {
        debug!(job_id = %job.id, job_name = %job.name, kind = %job.payload.kind, "Executing cron job");

        // Emit start event
        if let Some(emitter) = event_emitter.lock().ok().and_then(|e| e.clone()) {
            let mut event = SystemEvent::new_main_session(
                "cron.job_started",
                "cron",
                EventPriority::Normal,
                "定时任务开始执行",
                format!("定时任务 {} 已开始执行", job.name),
            );
            event.delivery = DeliveryPolicy::default();
            event.details = serde_json::json!({
                "job_id": job.id.clone(),
                "job_name": job.name.clone(),
                "payload_kind": job.payload.kind.clone(),
                "deliver": job.payload.deliver,
                "deliver_channel": job.payload.channel.clone(),
                "deliver_to": job.payload.to.clone(),
            });
            emitter.emit(event);
        }

        let (content, metadata) = match job.payload.kind.as_str() {
            "reminder" => {
                let content = job.payload.message.clone();
                let metadata = serde_json::json!({
                    "job_id": job.id,
                    "job_name": job.name,
                    "reminder": true,
                    "reminder_message": job.payload.message,
                    "deliver": job.payload.deliver,
                    "deliver_channel": job.payload.channel,
                    "deliver_to": job.payload.to,
                });
                (content, metadata)
            }
            "script" => {
                let skill_name = job.payload.skill_name.as_deref().unwrap_or("unknown");
                let content = job.payload.message.clone();
                let metadata = serde_json::json!({
                    "job_id": job.id,
                    "job_name": job.name,
                    "skill_name": skill_name,
                    "forced_skill_name": skill_name,
                    "skill_run_mode": "cron",
                    "deliver": job.payload.deliver,
                    "deliver_channel": job.payload.channel,
                    "deliver_to": job.payload.to,
                });
                (content, metadata)
            }
            "agent" => {
                let content = job.payload.message.clone();
                let metadata = serde_json::json!({
                    "job_id": job.id,
                    "job_name": job.name,
                    "cron_agent": true,
                    "deliver": job.payload.deliver,
                    "deliver_channel": job.payload.channel,
                    "deliver_to": job.payload.to,
                });
                (content, metadata)
            }
            _ => {
                error!(job_id = %job.id, kind = %job.payload.kind, "Unknown cron payload kind");
                return Err(format!("unknown cron payload kind: {}", job.payload.kind));
            }
        };

        let (msg_channel, msg_chat_id) = ("cron".to_string(), job.id.clone());

        let mut metadata = metadata;
        apply_route_agent_id(&mut metadata, agent_id.as_deref());
        let (receipt_id, receipt_rx) = register_message_receipt();
        if let Some(metadata) = metadata.as_object_mut() {
            metadata.insert(
                MESSAGE_RECEIPT_ID.to_string(),
                serde_json::Value::String(receipt_id.clone()),
            );
        }

        let msg = InboundMessage {
            channel: msg_channel,
            account_id: None,
            sender_id: "cron".to_string(),
            chat_id: msg_chat_id,
            content,
            media: vec![],
            metadata,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };

        if let Err(e) = inbound_tx.send(msg).await {
            cancel_message_receipt(&receipt_id);
            error!(error = %e, "Failed to send cron job message");

            // Emit failure event
            if let Some(emitter) = event_emitter.lock().ok().and_then(|e| e.clone()) {
                let mut event = SystemEvent::new_main_session(
                    "cron.job_failed",
                    "cron",
                    EventPriority::Critical,
                    "定时任务派发失败",
                    format!("定时任务 {} 派发失败：{}", job.name, e),
                );
                event.delivery = DeliveryPolicy::critical();
                event.details = serde_json::json!({
                    "job_id": job.id.clone(),
                    "job_name": job.name.clone(),
                    "error": e.to_string(),
                });
                emitter.emit(event);
            }
            return Err(e.to_string());
        }

        let completion = match tokio::time::timeout(CRON_EXECUTION_TIMEOUT, receipt_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("agent runtime dropped the cron completion receipt".to_string()),
            Err(_) => {
                cancel_message_receipt(&receipt_id);
                Err(format!(
                    "agent runtime did not complete cron job within {} seconds",
                    CRON_EXECUTION_TIMEOUT.as_secs_f64()
                ))
            }
        };

        match completion {
            Ok(()) => {
                if let Some(emitter) = event_emitter.lock().ok().and_then(|e| e.clone()) {
                    let mut event = SystemEvent::new_main_session(
                        "cron.job_completed",
                        "cron",
                        EventPriority::Normal,
                        "定时任务执行完成",
                        format!("定时任务 {} 已执行完成", job.name),
                    );
                    event.delivery = DeliveryPolicy::default();
                    event.details = serde_json::json!({
                        "job_id": job.id.clone(),
                        "job_name": job.name.clone(),
                    });
                    emitter.emit(event);
                }
                Ok(())
            }
            Err(error) => {
                if let Some(emitter) = event_emitter.lock().ok().and_then(|e| e.clone()) {
                    let mut event = SystemEvent::new_main_session(
                        "cron.job_failed",
                        "cron",
                        EventPriority::Critical,
                        "定时任务执行失败",
                        format!("定时任务 {} 执行失败：{}", job.name, error),
                    );
                    event.delivery = DeliveryPolicy::critical();
                    event.details = serde_json::json!({
                        "job_id": job.id.clone(),
                        "job_name": job.name.clone(),
                        "error": error.clone(),
                    });
                    emitter.emit(event);
                }
                Err(error)
            }
        }
    }

    /// Execute a cron job (wrapper for testing and internal use)
    #[allow(dead_code)]
    async fn execute_job(&self, job: &CronJob) {
        let _ = Self::execute_job_internal(
            job,
            self.inbound_tx.clone(),
            self.event_emitter.clone(),
            self.agent_id.clone(),
        )
        .await;
    }

    /// Calculate the next cron run time with timezone support.
    /// Returns the next run time as milliseconds since epoch.
    fn calculate_next_cron_run_ms(&self, expr: &str, tz: Option<&Tz>) -> Option<i64> {
        match expr.parse::<cron::Schedule>() {
            Ok(schedule) => match tz {
                Some(tz_ref) => schedule
                    .upcoming(*tz_ref)
                    .next()
                    .map(|dt| dt.timestamp_millis()),
                None => schedule
                    .upcoming(chrono::Utc)
                    .next()
                    .map(|dt| dt.timestamp_millis()),
            },
            Err(e) => {
                tracing::error!(expr = %expr, error = %e, "Invalid cron expression");
                None
            }
        }
    }

    fn calculate_next_run(&self, job: &mut CronJob, now_ms: i64, tz: Option<&Tz>) -> bool {
        match job.schedule.kind {
            ScheduleKind::At => {
                if let Some(at_ms) = job.schedule.at_ms {
                    job.state.next_run_at_ms = Some(at_ms);
                    at_ms <= now_ms
                } else {
                    false
                }
            }
            ScheduleKind::Every => {
                if let Some(every_ms) = job.schedule.every_ms {
                    // Set next_run_at_ms for the NEXT scheduled run (after potential immediate execution).
                    // This is stored now so that after immediate execution, the next run time is already set.
                    // run_immediately controls whether THIS tick should trigger execution:
                    // - true: return true to trigger immediate execution, next_run_at_ms is already set for next cycle
                    // - false: return false, job will wait until next_run_at_ms
                    job.state.next_run_at_ms = Some(now_ms + every_ms);
                    job.schedule.run_immediately
                } else {
                    false
                }
            }
            ScheduleKind::Cron => {
                if let Some(expr) = &job.schedule.expr {
                    if let Some(next_ms) = self.calculate_next_cron_run_ms(expr, tz) {
                        job.state.next_run_at_ms = Some(next_ms);
                        debug!(
                            job_id = %job.id,
                            next_run_ms = next_ms,
                            tz = job.schedule.tz.as_deref().unwrap_or("UTC"),
                            "Cron job initialized, waiting for first scheduled time"
                        );
                    }
                }
                false
            }
        }
    }

    pub async fn run_loop(self: Arc<Self>, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        info!(
            tick_interval_secs = self.tick_interval_secs,
            "CronService started"
        );

        // Use configurable tick interval
        let mut interval =
            tokio::time::interval(tokio::time::Duration::from_secs(self.tick_interval_secs));

        // Skip accumulated ticks when the service was paused/blocked
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Skip the first immediate tick (tokio interval returns immediately on first tick)
        interval.tick().await;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.run_tick().await {
                        error!(error = %e.to_string(), "Cron tick failed");
                    }
                }
                _ = shutdown.recv() => {
                    info!("CronService shutting down");

                    // Save any unsaved state before shutting down
                    if *self.has_unsaved_changes.read().await {
                        if let Err(e) = self.save().await {
                            error!(error = %e.to_string(), "Failed to save cron state on shutdown");
                        }
                    }
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike, Utc};

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
                .expect("recording emitter lock poisoned")
                .iter()
                .map(|event| event.kind.clone())
                .collect()
        }

        fn priorities(&self) -> Vec<EventPriority> {
            self.events
                .lock()
                .expect("recording emitter lock poisoned")
                .iter()
                .map(|event| event.priority)
                .collect()
        }
    }

    impl blockcell_tools::SystemEventEmitter for RecordingEmitter {
        fn emit(&self, event: SystemEvent) {
            self.events
                .lock()
                .expect("recording emitter lock poisoned")
                .push(event);
        }
    }

    fn test_job() -> CronJob {
        let now_ms = Utc::now().timestamp_millis();
        CronJob {
            id: "job-1".to_string(),
            name: "daily sync".to_string(),
            enabled: true,
            schedule: crate::job::JobSchedule {
                kind: ScheduleKind::Every,
                at_ms: None,
                every_ms: Some(60_000),
                expr: None,
                tz: None,
                run_immediately: false,
            },
            payload: crate::job::JobPayload {
                kind: "reminder".to_string(),
                message: "sync status".to_string(),
                deliver: false,
                channel: None,
                to: None,
                script_kind: None,
                skill_name: None,
            },
            state: crate::job::JobState::default(),
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            delete_after_run: false,
        }
    }

    fn test_agent_job() -> CronJob {
        let now_ms = Utc::now().timestamp_millis();
        CronJob {
            id: "job-agent-1".to_string(),
            name: "news digest".to_string(),
            enabled: true,
            schedule: crate::job::JobSchedule {
                kind: ScheduleKind::Every,
                at_ms: None,
                every_ms: Some(60_000),
                expr: None,
                tz: None,
                run_immediately: false,
            },
            payload: crate::job::JobPayload {
                kind: "agent".to_string(),
                message: "请搜索美国伊朗最新新闻并整理摘要".to_string(),
                deliver: true,
                channel: Some("telegram".to_string()),
                to: Some("12345".to_string()),
                script_kind: None,
                skill_name: None,
            },
            state: crate::job::JobState::default(),
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            delete_after_run: false,
        }
    }

    fn test_due_at_job() -> CronJob {
        let now_ms = Utc::now().timestamp_millis();
        CronJob {
            id: "job-due-at-1".to_string(),
            name: "bedtime reminder".to_string(),
            enabled: true,
            schedule: crate::job::JobSchedule {
                kind: ScheduleKind::At,
                at_ms: Some(now_ms - 1_000),
                every_ms: None,
                expr: None,
                tz: None,
                run_immediately: false,
            },
            payload: crate::job::JobPayload {
                kind: "reminder".to_string(),
                message: "time to sleep".to_string(),
                deliver: true,
                channel: Some("ws".to_string()),
                to: Some("ws:test-reminder".to_string()),
                script_kind: None,
                skill_name: None,
            },
            state: crate::job::JobState::default(),
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            delete_after_run: true,
        }
    }

    fn complete_cron_message(message: &InboundMessage, result: std::result::Result<(), String>) {
        let receipt_id = message.metadata[MESSAGE_RECEIPT_ID]
            .as_str()
            .expect("cron completion receipt id");
        assert!(blockcell_core::message_receipt::complete_message_receipt(
            receipt_id, result
        ));
    }

    #[test]
    fn test_apply_route_agent_id_inserts_metadata() {
        let mut metadata = serde_json::json!({"job_id":"1"});
        apply_route_agent_id(&mut metadata, Some("ops"));
        assert_eq!(
            metadata.get("route_agent_id").and_then(|v| v.as_str()),
            Some("ops")
        );
    }

    #[test]
    fn test_apply_route_agent_id_skips_empty_agent() {
        let mut metadata = serde_json::json!({"job_id":"1"});
        apply_route_agent_id(&mut metadata, Some("   "));
        assert!(metadata.get("route_agent_id").is_none());
    }

    #[tokio::test]
    async fn test_cron_event_execute_job_emits_started_and_completed() {
        let paths = Paths::with_base(
            std::env::temp_dir().join(format!("blockcell-cron-service-{}", uuid::Uuid::new_v4())),
        );
        let (tx, mut rx) = mpsc::channel(1);
        let service = CronService::new(paths, tx);
        let emitter = RecordingEmitter::default();
        service.set_event_emitter(emitter.handle());

        let job = test_job();
        let execution = service.execute_job(&job);
        let receive = async {
            let message = rx.recv().await.expect("receive cron inbound message");
            complete_cron_message(&message, Ok(()));
            message
        };
        let (_, message) = tokio::join!(execution, receive);
        assert_eq!(message.sender_id, "cron");
        assert_eq!(
            emitter.kinds(),
            vec![
                "cron.job_started".to_string(),
                "cron.job_completed".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn test_cron_job_waits_for_runtime_completion_receipt() {
        let (tx, mut rx) = mpsc::channel(1);
        let job = test_job();
        let event_emitter = Arc::new(StdMutex::new(None));
        let execution = tokio::spawn(async move {
            CronService::execute_job_internal(&job, tx, event_emitter, None).await
        });

        let message = rx.recv().await.expect("receive cron message");
        assert!(
            !execution.is_finished(),
            "enqueue alone must not complete the job"
        );
        let receipt_id = message.metadata[blockcell_core::message_receipt::MESSAGE_RECEIPT_ID]
            .as_str()
            .expect("completion receipt id");
        assert!(blockcell_core::message_receipt::complete_message_receipt(
            receipt_id,
            Ok(())
        ));

        assert!(execution.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn test_cron_event_execute_job_emits_failed_on_send_error() {
        let paths = Paths::with_base(
            std::env::temp_dir().join(format!("blockcell-cron-service-{}", uuid::Uuid::new_v4())),
        );
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let service = CronService::new(paths, tx);
        let emitter = RecordingEmitter::default();
        service.set_event_emitter(emitter.handle());

        service.execute_job(&test_job()).await;

        assert_eq!(
            emitter.kinds(),
            vec![
                "cron.job_started".to_string(),
                "cron.job_failed".to_string(),
            ]
        );
        assert_eq!(
            emitter.priorities().last().copied(),
            Some(EventPriority::Critical)
        );
    }

    #[tokio::test]
    async fn test_execute_agent_job_sends_plain_cron_message_without_fast_path_flags() {
        let paths = Paths::with_base(
            std::env::temp_dir().join(format!("blockcell-cron-service-{}", uuid::Uuid::new_v4())),
        );
        let (tx, mut rx) = mpsc::channel(1);
        let service = CronService::new(paths, tx);

        let job = test_agent_job();
        let execution = service.execute_job(&job);
        let receive = async {
            let message = rx.recv().await.expect("receive cron inbound message");
            complete_cron_message(&message, Ok(()));
            message
        };
        let (_, message) = tokio::join!(execution, receive);
        assert_eq!(message.channel, "cron");
        assert_eq!(message.content, "请搜索美国伊朗最新新闻并整理摘要");
        assert_eq!(
            message.metadata.get("cron_agent").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(message.metadata.get("reminder").is_none());
        assert!(message.metadata.get("skill_script").is_none());
    }

    #[tokio::test]
    async fn test_run_tick_executes_due_at_job_loaded_from_disk() {
        let paths = Paths::with_base(
            std::env::temp_dir().join(format!("blockcell-cron-service-{}", uuid::Uuid::new_v4())),
        );
        tokio::fs::create_dir_all(paths.cron_dir())
            .await
            .expect("create cron dir");
        let store = JobStore {
            version: 1,
            jobs: vec![test_due_at_job()],
        };
        let content = serde_json::to_string_pretty(&store).expect("serialize cron store");
        tokio::fs::write(paths.cron_jobs_file(), content)
            .await
            .expect("write cron store");

        let (tx, mut rx) = mpsc::channel(1);
        let service = CronService::new(paths, tx);

        let tick = service.run_tick();
        let receive = async {
            let message = rx.recv().await.expect("receive cron inbound message");
            complete_cron_message(&message, Ok(()));
            message
        };
        let (tick_result, message) = tokio::join!(tick, receive);
        tick_result.expect("run tick");
        assert_eq!(message.content, "time to sleep");
        assert_eq!(
            message.metadata.get("reminder").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[tokio::test]
    async fn test_run_tick_does_not_readd_delete_after_run_job_from_disk() {
        let paths = Paths::with_base(
            std::env::temp_dir().join(format!("blockcell-cron-service-{}", uuid::Uuid::new_v4())),
        );
        tokio::fs::create_dir_all(paths.cron_dir())
            .await
            .expect("create cron dir");
        let store = JobStore {
            version: 1,
            jobs: vec![test_due_at_job()],
        };
        let content = serde_json::to_string_pretty(&store).expect("serialize cron store");
        tokio::fs::write(paths.cron_jobs_file(), content)
            .await
            .expect("write cron store");

        let (tx, mut rx) = mpsc::channel(4);
        let service = CronService::new(paths.clone(), tx);

        let first_tick = service.run_tick();
        let receive = async {
            let message = rx.recv().await.expect("receive first cron inbound message");
            complete_cron_message(&message, Ok(()));
            message
        };
        let (first_result, first) = tokio::join!(first_tick, receive);
        first_result.expect("run first tick");
        assert_eq!(first.content, "time to sleep");

        service.run_tick().await.expect("run second tick");
        let second = tokio::time::timeout(tokio::time::Duration::from_millis(200), rx.recv()).await;
        assert!(
            second.is_err(),
            "delete_after_run job should not fire twice"
        );

        let saved = tokio::fs::read_to_string(paths.cron_jobs_file())
            .await
            .expect("read saved cron store");
        let saved: JobStore = serde_json::from_str(&saved).expect("parse saved cron store");
        assert!(
            saved.jobs.is_empty(),
            "delete_after_run job should be removed from disk"
        );
    }

    #[tokio::test]
    async fn test_run_tick_releases_operation_lock_during_job_delivery() {
        let paths = Paths::with_base(
            std::env::temp_dir().join(format!("blockcell-cron-service-{}", uuid::Uuid::new_v4())),
        );
        tokio::fs::create_dir_all(paths.cron_dir())
            .await
            .expect("create cron dir");
        let store = JobStore {
            version: 1,
            jobs: vec![test_due_at_job()],
        };
        tokio::fs::write(
            paths.cron_jobs_file(),
            serde_json::to_string_pretty(&store).expect("serialize cron store"),
        )
        .await
        .expect("write cron store");

        let (tx, mut rx) = mpsc::channel(2);
        let service = Arc::new(CronService::new(paths, tx));

        // Start a tick. It delivers the due job and then blocks waiting for the
        // runtime completion receipt (in production up to 15 min).
        let tick = tokio::spawn({
            let service = service.clone();
            async move { service.run_tick().await }
        });

        // Receipt is now pending: the tick is parked inside `execute_job_internal`.
        let message = rx.recv().await.expect("receive cron inbound message");

        // A concurrent write must not wait on the in-flight delivery.
        let added = tokio::time::timeout(
            tokio::time::Duration::from_millis(500),
            service.add_job(test_job()),
        )
        .await;
        assert!(
            added.is_ok(),
            "add_job blocked on an in-flight tick delivery: run_tick must not hold operation_lock while awaiting the receipt"
        );
        added.expect("timeout arm").expect("add_job");

        complete_cron_message(&message, Ok(()));
        tick.await.expect("join tick task").expect("run tick");
    }

    #[tokio::test]
    async fn test_failed_due_at_runtime_execution_remains_retryable_and_records_error() {
        let paths = Paths::with_base(
            std::env::temp_dir().join(format!("blockcell-cron-service-{}", uuid::Uuid::new_v4())),
        );
        tokio::fs::create_dir_all(paths.cron_dir())
            .await
            .expect("create cron dir");
        let store = JobStore {
            version: 1,
            jobs: vec![test_due_at_job()],
        };
        tokio::fs::write(
            paths.cron_jobs_file(),
            serde_json::to_string_pretty(&store).unwrap(),
        )
        .await
        .expect("write cron store");
        let (tx, mut rx) = mpsc::channel(1);
        let service = CronService::new(paths.clone(), tx);

        let tick = service.run_tick();
        let fail_runtime = async {
            let message = rx.recv().await.expect("receive cron message");
            complete_cron_message(&message, Err("provider failed".to_string()));
        };
        let (tick_result, ()) = tokio::join!(tick, fail_runtime);
        tick_result.expect("run failed runtime tick");

        let saved: JobStore = serde_json::from_str(
            &tokio::fs::read_to_string(paths.cron_jobs_file())
                .await
                .expect("read saved store"),
        )
        .expect("parse saved store");
        assert_eq!(saved.jobs.len(), 1, "failed one-shot must not be deleted");
        assert!(saved.jobs[0].enabled, "failed one-shot must remain enabled");
        assert_eq!(
            saved.jobs[0].state.last_status,
            Some(crate::job::JobStatus::Error)
        );
        assert!(saved.jobs[0].state.last_error.is_some());
        assert!(saved.jobs[0].state.last_run_at_ms.is_none());
    }

    #[tokio::test]
    async fn test_concurrent_adds_preserve_both_jobs() {
        let paths = Paths::with_base(
            std::env::temp_dir().join(format!("blockcell-cron-service-{}", uuid::Uuid::new_v4())),
        );
        let (tx, _rx) = mpsc::channel(1);
        let service = Arc::new(CronService::new(paths.clone(), tx));
        let mut first = test_job();
        first.id = "first".to_string();
        let mut second = test_job();
        second.id = "second".to_string();

        let first_service = service.clone();
        let second_service = service.clone();
        let (first_result, second_result) =
            tokio::join!(first_service.add_job(first), second_service.add_job(second));
        first_result.expect("add first job");
        second_result.expect("add second job");

        let saved: JobStore = serde_json::from_str(
            &tokio::fs::read_to_string(paths.cron_jobs_file())
                .await
                .expect("read cron store"),
        )
        .expect("parse cron store");
        let ids: std::collections::HashSet<_> = saved.jobs.into_iter().map(|job| job.id).collect();
        assert_eq!(
            ids,
            std::collections::HashSet::from(["first".to_string(), "second".to_string()])
        );
    }

    // ── Schedule computation (Every / At / Cron, timezone, DST) ────────────

    fn job_with_schedule(schedule: crate::job::JobSchedule) -> CronJob {
        let now_ms = Utc::now().timestamp_millis();
        CronJob {
            id: "job-schedule".to_string(),
            name: "schedule test".to_string(),
            enabled: true,
            schedule,
            payload: crate::job::JobPayload {
                kind: "reminder".to_string(),
                message: "tick".to_string(),
                deliver: false,
                channel: None,
                to: None,
                script_kind: None,
                skill_name: None,
            },
            state: crate::job::JobState::default(),
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            delete_after_run: false,
        }
    }

    fn at_schedule(at_ms: Option<i64>) -> crate::job::JobSchedule {
        crate::job::JobSchedule {
            kind: ScheduleKind::At,
            at_ms,
            every_ms: None,
            expr: None,
            tz: None,
            run_immediately: false,
        }
    }

    fn every_schedule(every_ms: Option<i64>, run_immediately: bool) -> crate::job::JobSchedule {
        crate::job::JobSchedule {
            kind: ScheduleKind::Every,
            at_ms: None,
            every_ms,
            expr: None,
            tz: None,
            run_immediately,
        }
    }

    fn cron_schedule(expr: &str) -> crate::job::JobSchedule {
        crate::job::JobSchedule {
            kind: ScheduleKind::Cron,
            at_ms: None,
            every_ms: None,
            expr: Some(expr.to_string()),
            tz: None,
            run_immediately: false,
        }
    }

    fn schedule_service() -> CronService {
        let (tx, _rx) = mpsc::channel(1);
        let base =
            std::env::temp_dir().join(format!("blockcell-cron-sched-{}", uuid::Uuid::new_v4()));
        CronService::new(Paths::with_base(base), tx)
    }

    #[test]
    fn at_job_in_the_past_is_due_and_keeps_its_deadline() {
        let service = schedule_service();
        let now_ms = Utc::now().timestamp_millis();
        let mut job = job_with_schedule(at_schedule(Some(now_ms - 1_000)));

        assert!(service.calculate_next_run(&mut job, now_ms, None));
        assert_eq!(job.state.next_run_at_ms, Some(now_ms - 1_000));
    }

    #[test]
    fn at_job_in_the_future_is_not_due_yet() {
        let service = schedule_service();
        let now_ms = Utc::now().timestamp_millis();
        let mut job = job_with_schedule(at_schedule(Some(now_ms + 60_000)));

        assert!(!service.calculate_next_run(&mut job, now_ms, None));
        assert_eq!(job.state.next_run_at_ms, Some(now_ms + 60_000));
    }

    #[test]
    fn at_job_without_a_deadline_never_runs() {
        let service = schedule_service();
        let now_ms = Utc::now().timestamp_millis();
        let mut job = job_with_schedule(at_schedule(None));

        assert!(!service.calculate_next_run(&mut job, now_ms, None));
        assert!(job.state.next_run_at_ms.is_none());
    }

    #[test]
    fn every_job_honours_run_immediately_and_advances_the_clock() {
        let service = schedule_service();
        let now_ms = Utc::now().timestamp_millis();

        let mut immediate = job_with_schedule(every_schedule(Some(60_000), true));
        assert!(service.calculate_next_run(&mut immediate, now_ms, None));
        assert_eq!(immediate.state.next_run_at_ms, Some(now_ms + 60_000));

        let mut deferred = job_with_schedule(every_schedule(Some(60_000), false));
        assert!(!service.calculate_next_run(&mut deferred, now_ms, None));
        assert_eq!(deferred.state.next_run_at_ms, Some(now_ms + 60_000));
    }

    #[test]
    fn every_job_without_an_interval_never_runs() {
        let service = schedule_service();
        let now_ms = Utc::now().timestamp_millis();
        let mut job = job_with_schedule(every_schedule(None, true));

        assert!(!service.calculate_next_run(&mut job, now_ms, None));
        assert!(job.state.next_run_at_ms.is_none());
    }

    #[test]
    fn cron_job_waits_for_its_first_scheduled_time() {
        let service = schedule_service();
        let now_ms = Utc::now().timestamp_millis();
        let mut job = job_with_schedule(cron_schedule("0 0 9 * * *"));

        assert!(!service.calculate_next_run(&mut job, now_ms, None));

        let next_ms = job
            .state
            .next_run_at_ms
            .expect("next run must be scheduled");
        assert!(next_ms > now_ms, "cron jobs wait for the next occurrence");
        let utc = Utc.timestamp_millis_opt(next_ms).unwrap();
        assert_eq!((utc.hour(), utc.minute(), utc.second()), (9, 0, 0));
    }

    #[test]
    fn cron_expression_is_evaluated_in_the_configured_timezone() {
        let service = schedule_service();
        let shanghai = parse_timezone("Asia/Shanghai").unwrap();

        let next_ms = service
            .calculate_next_cron_run_ms("0 0 9 * * *", Some(&shanghai))
            .expect("next 09:00 in Shanghai");

        let local = shanghai.timestamp_millis_opt(next_ms).unwrap();
        assert_eq!((local.hour(), local.minute(), local.second()), (9, 0, 0));

        // 09:00 in UTC+8 is 01:00 UTC, so the timezone really drove the result.
        let utc = Utc.timestamp_millis_opt(next_ms).unwrap();
        assert_eq!(utc.hour(), 1);
    }

    #[test]
    fn cron_schedule_tracks_dst_transitions_in_an_observing_zone() {
        let service = schedule_service();
        let new_york = parse_timezone("America/New_York").unwrap();

        let next_ms = service
            .calculate_next_cron_run_ms("0 0 12 * * *", Some(&new_york))
            .expect("next noon in New York");

        // Noon is never skipped by DST, so it must stay local noon.
        let local = new_york.timestamp_millis_opt(next_ms).unwrap();
        assert_eq!((local.hour(), local.minute()), (12, 0));

        // New York is UTC-4 (EDT) or UTC-5 (EST) depending on the date.
        let utc = Utc.timestamp_millis_opt(next_ms).unwrap();
        assert!(
            matches!(utc.hour(), 16 | 17),
            "expected 16:00 or 17:00 UTC, got {}:00",
            utc.hour()
        );
    }

    #[test]
    fn invalid_cron_expression_yields_no_schedule() {
        let service = schedule_service();

        assert!(service
            .calculate_next_cron_run_ms("not a cron expression", None)
            .is_none());
        assert!(service.calculate_next_cron_run_ms("", None).is_none());
    }

    #[test]
    fn parse_timezone_accepts_iana_names_and_rejects_unknown_zones() {
        assert!(parse_timezone("UTC").is_some());
        assert!(parse_timezone("Asia/Shanghai").is_some());
        assert!(parse_timezone("America/New_York").is_some());
        assert!(parse_timezone("Not/AZone").is_none());
        assert!(parse_timezone("").is_none());
    }
}
