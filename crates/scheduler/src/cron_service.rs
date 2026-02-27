use crate::job::{CronJob, ScheduleKind};
use chrono::Utc;
use blockcell_core::{InboundMessage, Paths, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
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
}

impl CronService {
    pub fn new(paths: Paths, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        Self {
            paths,
            jobs: Arc::new(RwLock::new(Vec::new())),
            inbound_tx,
        }
    }

    pub async fn load(&self) -> Result<()> {
        let path = self.paths.cron_jobs_file();
        if !path.exists() {
            return Ok(());
        }

        let content = tokio::fs::read_to_string(&path).await?;
        let store: JobStore = serde_json::from_str(&content)?;
        
        let mut jobs = self.jobs.write().await;
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
    pub async fn update_job_enabled(&self, id_prefix: &str, enabled: bool) -> Result<Option<String>> {
        let mut jobs = self.jobs.write().await;
        let matching: Vec<usize> = jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| j.id.starts_with(id_prefix))
            .map(|(i, _)| i)
            .collect();

        match matching.len() {
            0 => return Ok(None),
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
                let names: Vec<String> = matching.iter()
                    .map(|&i| format!("{} ({})", &jobs[i].id.chars().take(8).collect::<String>(), jobs[i].name))
                    .collect();
                Err(blockcell_core::Error::Other(format!(
                    "Multiple jobs match '{}': {}",
                    id_prefix,
                    names.join(", ")
                )))
            }
        }
    }

    pub async fn run_tick(&self) -> Result<()> {
        // Reload jobs from disk to pick up changes made by CronTool.
        // 注意：只在内存中没有 next_run_at_ms 的 job 才需要从磁盘重新初始化；
        // 已经在内存中运行过的 job 状态以内存为准，避免磁盘旧状态覆盖导致重复触发。
        if let Err(e) = self.load().await {
            error!(error = %e.to_string(), "Failed to reload cron jobs from disk");
        }

        let now_ms = Utc::now().timestamp_millis();
        let mut jobs = self.jobs.write().await;
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

        let (content, metadata) = if job.payload.kind == "skill_rhai" {
            // Direct SKILL.rhai execution — send metadata so AgentRuntime dispatches
            // to SkillDispatcher instead of going through the full LLM loop.
            let skill_name = job.payload.skill_name.as_deref().unwrap_or("unknown");
            let content = format!(
                "[系统定时任务] 执行技能脚本 {} — {}",
                skill_name, job.payload.message
            );
            let metadata = serde_json::json!({
                "job_id": job.id,
                "job_name": job.name,
                "skill_rhai": true,
                "skill_name": skill_name,
                "deliver": job.payload.deliver,
                "deliver_channel": job.payload.channel,
                "deliver_to": job.payload.to,
            });
            (content, metadata)
        } else {
            // Standard agent_turn — let the LLM handle it (search, execute skill, etc.)
            // Do NOT set reminder:true here — that bypasses the LLM entirely.
            let content = format!(
                "[系统定时任务] 你之前为用户设置的任务「{}」现在到时间了。请立即执行该任务并将结果发送给用户。",
                job.payload.message
            );
            let metadata = serde_json::json!({
                "job_id": job.id,
                "job_name": job.name,
                "agent_task": true,
                "deliver": job.payload.deliver,
                "deliver_channel": job.payload.channel,
                "deliver_to": job.payload.to,
            });
            (content, metadata)
        };

        // For agent_turn jobs, use the deliver target as channel/chat_id so that
        // ToolContext gets the real user channel — the message tool can then send
        // to the correct target without the LLM needing to specify channel/chat_id.
        // For skill_rhai and simple reminder jobs, keep "cron" as the channel.
        let (msg_channel, msg_chat_id) = if job.payload.kind == "agent_turn" && job.payload.deliver {
            let ch = job.payload.channel.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| "cron".to_string());
            let to = job.payload.to.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| job.id.clone());
            (ch, to)
        } else {
            ("cron".to_string(), job.id.clone())
        };

        let msg = InboundMessage {
            channel: msg_channel,
            sender_id: "cron".to_string(),
            chat_id: msg_chat_id,
            content,
            media: vec![],
            metadata,
            timestamp_ms: Utc::now().timestamp_millis(),
        };

        if let Err(e) = self.inbound_tx.send(msg).await {
            error!(error = %e, "Failed to send cron job message");
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
