use crate::job::{CronJob, ScheduleKind};
use blockcell_core::system_event::{DeliveryPolicy, EventPriority, SystemEvent};
use blockcell_core::{InboundMessage, Paths, Result};
use blockcell_tools::EventEmitterHandle;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info};

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

impl CronService {
    pub fn new(paths: Paths, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        Self::new_with_agent(paths, inbound_tx, None)
    }

    pub fn new_with_agent(
        paths: Paths,
        inbound_tx: mpsc::Sender<InboundMessage>,
        agent_id: Option<String>,
    ) -> Self {
        Self {
            paths,
            jobs: Arc::new(RwLock::new(Vec::new())),
            inbound_tx,
            agent_id: agent_id
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty()),
            event_emitter: Arc::new(StdMutex::new(None)),
        }
    }

    pub fn set_event_emitter(&self, emitter: EventEmitterHandle) {
        let mut slot = self
            .event_emitter
            .lock()
            .expect("cron service event emitter lock poisoned");
        *slot = Some(emitter);
    }

    fn emit_system_event(&self, event: SystemEvent) {
        let emitter = self
            .event_emitter
            .lock()
            .expect("cron service event emitter lock poisoned")
            .clone();
        if let Some(emitter) = emitter {
            emitter.emit(event);
        }
    }

    fn emit_cron_event(
        &self,
        job: &CronJob,
        kind: &str,
        priority: EventPriority,
        title: &str,
        summary: String,
        delivery: DeliveryPolicy,
    ) {
        let mut event = SystemEvent::new_main_session(kind, "cron", priority, title, summary);
        event.delivery = delivery;
        event.details = serde_json::json!({
            "job_id": job.id.clone(),
            "job_name": job.name.clone(),
            "payload_kind": job.payload.kind.clone(),
            "deliver": job.payload.deliver,
            "deliver_channel": job.payload.channel.clone(),
            "deliver_to": job.payload.to.clone(),
        });
        self.emit_system_event(event);
    }

    pub async fn load(&self) -> Result<()> {
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
        let path = self.paths.cron_jobs_file();

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let jobs = self.jobs.read().await;
        let store = JobStore {
            version: 1,
            jobs: jobs.clone(),
        };

        let content = serde_json::to_string_pretty(&store)?;
        tokio::fs::write(&path, content).await?;

        Ok(())
    }

    pub async fn add_job(&self, job: CronJob) -> Result<()> {
        let mut jobs = self.jobs.write().await;
        jobs.push(job);
        drop(jobs);
        self.save().await
    }

    pub async fn remove_job(&self, id: &str) -> Result<bool> {
        let mut jobs = self.jobs.write().await;
        let len_before = jobs.len();
        jobs.retain(|j| j.id != id);
        let removed = jobs.len() < len_before;
        drop(jobs);

        if removed {
            self.save().await?;
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
                self.save().await?;
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
    async fn merge_load(&self) -> Result<()> {
        let path = self.paths.cron_jobs_file();
        if !path.exists() {
            return Ok(());
        }
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
        debug!(
            count = mem_jobs.len(),
            "Loaded cron jobs (merged with in-memory state)"
        );
        Ok(())
    }

    /// Pick up any new jobs written to disk (e.g. by CronTool) since the last load.
    /// Only adds jobs whose IDs are not already in memory; never modifies existing ones.
    /// Called just before save() to close the race window.
    async fn sync_new_from_disk(
        &self,
        known_ids: &std::collections::HashSet<String>,
    ) -> Result<()> {
        let path = self.paths.cron_jobs_file();
        if !path.exists() {
            return Ok(());
        }
        let content = tokio::fs::read_to_string(&path).await?;
        let store: JobStore = serde_json::from_str(&content)?;

        let mut mem_jobs = self.jobs.write().await;
        for disk_job in store.jobs {
            if !known_ids.contains(&disk_job.id)
                && !mem_jobs.iter().any(|job| job.id == disk_job.id)
            {
                debug!(job_id = %disk_job.id, "Picked up new cron job from disk");
                mem_jobs.push(disk_job);
            }
        }
        Ok(())
    }

    pub async fn run_tick(&self) -> Result<()> {
        // Reload from disk, merging in-memory execution state for already-initialized jobs.
        // New jobs added by CronTool (disk-only) are picked up; existing job scheduling
        // state (next_run_at_ms / last_run_at_ms) is preserved to avoid double-firing.
        if let Err(e) = self.merge_load().await {
            error!(error = %e.to_string(), "Failed to reload cron jobs from disk");
        }

        let now_ms = Utc::now().timestamp_millis();
        let mut jobs = self.jobs.write().await;
        let known_ids: std::collections::HashSet<String> =
            jobs.iter().map(|job| job.id.clone()).collect();
        let mut jobs_to_run = Vec::new();

        for job in jobs.iter_mut() {
            if !job.enabled {
                continue;
            }

            // Guard: skip one-time (At) jobs that have already fired
            if job.schedule.kind == ScheduleKind::At && job.state.last_run_at_ms.is_some() {
                job.enabled = false;
                continue;
            }

            let should_run = match &job.state.next_run_at_ms {
                Some(next) => *next <= now_ms,
                None => self.calculate_next_run(job, now_ms),
            };

            if should_run {
                jobs_to_run.push(job.clone());

                // Update state
                job.state.last_run_at_ms = Some(now_ms);

                // Calculate next run
                match job.schedule.kind {
                    ScheduleKind::At => {
                        // One-time job: disable immediately
                        job.state.next_run_at_ms = None;
                        job.enabled = false;
                    }
                    ScheduleKind::Every => {
                        if let Some(every_ms) = job.schedule.every_ms {
                            job.state.next_run_at_ms = Some(now_ms + every_ms);
                        }
                    }
                    ScheduleKind::Cron => {
                        // Calculate next cron time
                        if let Some(expr) = &job.schedule.expr {
                            if let Ok(schedule) = expr.parse::<cron::Schedule>() {
                                if let Some(next) = schedule.upcoming(Utc).next() {
                                    job.state.next_run_at_ms = Some(next.timestamp_millis());
                                }
                            }
                        }
                    }
                }
            }
        }

        // 修复：delete_after_run 不依赖 enabled 状态。
        // 原逻辑 `!j.enabled` 导致 Every 类型（执行后 enabled 仍为 true）的一次性任务永远不被删除。
        // 修正为：只要执行过（last_run_at_ms.is_some()）且标记了 delete_after_run 就删除。
        let delete_ids: Vec<String> = jobs
            .iter()
            .filter(|j| j.delete_after_run && j.state.last_run_at_ms.is_some())
            .map(|j| j.id.clone())
            .collect();
        if !delete_ids.is_empty() {
            jobs.retain(|j| !delete_ids.contains(&j.id));
            info!(count = delete_ids.len(), "Deleted completed one-time jobs");
        }

        drop(jobs);

        // Pick up any new jobs written by CronTool between the merge_load above and now.
        // This closes the race window: without this, save() would overwrite those new jobs.
        if let Err(e) = self.sync_new_from_disk(&known_ids).await {
            error!(error = %e.to_string(), "Failed to sync new cron jobs from disk");
        }

        // Save state changes to disk BEFORE executing jobs
        // This ensures the next tick won't re-fire disabled/deleted jobs
        self.save().await?;

        // Execute jobs
        for job in jobs_to_run {
            self.execute_job(&job).await;
        }
        Ok(())
    }

    fn calculate_next_run(&self, job: &mut CronJob, now_ms: i64) -> bool {
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
                    // 修复：首次不立即执行，而是等待第一个完整周期后再触发。
                    // 原逻辑返回 true 导致服务启动后所有 Every 任务立即执行一次，
                    // 且若 save() 在崩溃前未完成，重启后会再次立即执行（重复触发）。
                    job.state.next_run_at_ms = Some(now_ms + every_ms);
                    false
                } else {
                    false
                }
            }
            ScheduleKind::Cron => {
                if let Some(expr) = &job.schedule.expr {
                    if let Ok(schedule) = expr.parse::<cron::Schedule>() {
                        if let Some(next) = schedule.upcoming(Utc).next() {
                            job.state.next_run_at_ms = Some(next.timestamp_millis());
                            debug!(
                                job_id = %job.id,
                                next_run_ms = next.timestamp_millis(),
                                "Cron job initialized, waiting for first scheduled time"
                            );
                        }
                    }
                }
                false
            }
        }
    }

    async fn execute_job(&self, job: &CronJob) {
        debug!(job_id = %job.id, job_name = %job.name, kind = %job.payload.kind, "Executing cron job");
        self.emit_cron_event(
            job,
            "cron.job_started",
            EventPriority::Normal,
            "定时任务开始执行",
            format!("定时任务 {} 已开始执行", job.name),
            DeliveryPolicy::default(),
        );

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
                let kind = job.payload.script_kind.as_deref().unwrap_or("rhai");
                let skill_name = job.payload.skill_name.as_deref().unwrap_or("unknown");
                let content = format!(
                    "[系统定时任务] 执行技能脚本 {} — {}",
                    skill_name, job.payload.message
                );
                let mut metadata = serde_json::json!({
                    "job_id": job.id,
                    "job_name": job.name,
                    "skill_script": true,
                    "skill_script_kind": kind,
                    "skill_name": skill_name,
                    "deliver": job.payload.deliver,
                    "deliver_channel": job.payload.channel,
                    "deliver_to": job.payload.to,
                });
                if kind == "python" {
                    metadata["skill_python"] = serde_json::json!(true);
                } else {
                    metadata["skill_rhai"] = serde_json::json!(true);
                }
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
                return;
            }
        };

        let (msg_channel, msg_chat_id) = ("cron".to_string(), job.id.clone());

        let mut metadata = metadata;
        apply_route_agent_id(&mut metadata, self.agent_id.as_deref());

        let msg = InboundMessage {
            channel: msg_channel,
            account_id: None,
            sender_id: "cron".to_string(),
            chat_id: msg_chat_id,
            content,
            media: vec![],
            metadata,
            timestamp_ms: Utc::now().timestamp_millis(),
        };

        if let Err(e) = self.inbound_tx.send(msg).await {
            error!(error = %e, "Failed to send cron job message");
            self.emit_cron_event(
                job,
                "cron.job_failed",
                EventPriority::Critical,
                "定时任务派发失败",
                format!("定时任务 {} 派发失败：{}", job.name, e),
                DeliveryPolicy::critical(),
            );
        } else {
            self.emit_cron_event(
                job,
                "cron.job_completed",
                EventPriority::Normal,
                "定时任务已派发",
                format!("定时任务 {} 已成功派发", job.name),
                DeliveryPolicy::default(),
            );
        }
    }

    pub async fn run_loop(self: Arc<Self>, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        info!("CronService started");

        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.run_tick().await {
                        error!(error = %e.to_string(), "Cron tick failed");
                    }
                }
                _ = shutdown.recv() => {
                    info!("CronService shutting down");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        service.execute_job(&test_job()).await;

        let message = rx.recv().await.expect("receive cron inbound message");
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

        service.execute_job(&test_agent_job()).await;

        let message = rx.recv().await.expect("receive cron inbound message");
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

        service.run_tick().await.expect("run tick");

        let message = tokio::time::timeout(tokio::time::Duration::from_millis(200), rx.recv())
            .await
            .expect("cron message should be sent")
            .expect("receive cron inbound message");
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

        service.run_tick().await.expect("run first tick");
        let first = tokio::time::timeout(tokio::time::Duration::from_millis(200), rx.recv())
            .await
            .expect("first cron message should be sent")
            .expect("receive first cron inbound message");
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
}
