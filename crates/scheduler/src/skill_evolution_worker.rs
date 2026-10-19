//! Durable workflow worker for skill evolution.
//!
//! The agent runtime only triggers records and wakes this worker. The long
//! LLM/audit/compile/deploy pipeline runs here, behind the workflow store's
//! claim/lease protocol, so runtime select loops stay responsive.

use blockcell_agent::EvolutionNotifier;
use blockcell_core::{Error, Result};
use blockcell_skills::evolution::EvolutionStatus;
use blockcell_skills::{
    EvolutionService, EvolutionServiceConfig, LLMProvider, SingleEvolutionResult,
};
use blockcell_storage::evolution_workflow::WorkflowRecord;
use blockcell_storage::EvolutionWorkflowStore;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, oneshot, Notify};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::blocking;
use crate::evolution_worker::run_with_lease_guard;

const DEFAULT_POLL_INTERVAL_SECS: u64 = 30;
const DEFAULT_LEASE_DURATION_SECS: i64 = 300;
const SKILL_PIPELINE_STEP: &str = "SkillEvolutionPipeline";

/// Outcome of `run_skill_workflow`.
enum PipelineOutcome {
    /// Pipeline finished; record reached Observing or Completed.
    Done(String),
    /// Pipeline still running after wait — not a failure, just not done yet.
    StillRunning,
}

pub struct SkillEvolutionWorker {
    store: EvolutionWorkflowStore,
    service: EvolutionService,
    worker_id: String,
    poll_interval_secs: u64,
    lease_duration_secs: i64,
    wakeup: Notify,
}

impl SkillEvolutionWorker {
    fn workflow_status_for_evolution(status: &EvolutionStatus) -> Option<&'static str> {
        match *status.normalize() {
            EvolutionStatus::Completed => Some("Promoted"),
            EvolutionStatus::RolledBack | EvolutionStatus::Failed => Some("Failed"),
            _ => None,
        }
    }

    async fn finalize_observation_workflows(&self) -> Result<()> {
        for workflow in blocking::list_workflows(&self.store, Some("Observing")).await? {
            let Ok(record) = self.service.evolution().load_record(&workflow.description) else {
                continue;
            };
            let Some(status) = Self::workflow_status_for_evolution(&record.status) else {
                continue;
            };
            let detail = if status == "Failed" {
                Some(format!("Skill evolution ended as {:?}", record.status))
            } else {
                None
            };
            blocking::update_workflow_status(&self.store, &workflow.id, status, detail.as_deref())
                .await?;
        }
        Ok(())
    }

    pub fn new(
        store: EvolutionWorkflowStore,
        skills_dir: PathBuf,
        config: EvolutionServiceConfig,
        llm_provider: Option<Arc<dyn LLMProvider>>,
    ) -> Self {
        let mut service = EvolutionService::new(skills_dir, config);
        if let Some(provider) = llm_provider {
            service.set_llm_provider(provider);
        }

        Self {
            store,
            service,
            worker_id: uuid::Uuid::new_v4().to_string(),
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
            lease_duration_secs: DEFAULT_LEASE_DURATION_SECS,
            wakeup: Notify::new(),
        }
    }

    /// 设置技能部署成功后的回调（例如记录 EvolutionSuccess ghost learning boundary）。
    ///
    /// 必须在 `run_loop` 之前调用。deploy_callback 在观察期最终成功后触发
    ///（即 tick_single_observation 确认错误率正常且窗口到期时），
    /// 而非 process_pending_evolution 返回 Completed（进入 Observing）时触发。
    pub fn set_deploy_callback(&mut self, callback: Arc<dyn Fn(&str) + Send + Sync>) {
        self.service.set_deploy_callback(callback);
    }

    pub fn notify(&self) {
        self.wakeup.notify_one();
    }

    pub async fn run_loop(self: Arc<Self>, mut shutdown: broadcast::Receiver<()>) {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(self.poll_interval_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        info!(worker_id = %self.worker_id, "SkillEvolutionWorker started");

        loop {
            tokio::select! {
                _ = shutdown.recv() => {
                    info!(worker_id = %self.worker_id, "SkillEvolutionWorker shutting down");
                    return;
                }
                _ = self.wakeup.notified() => {
                    debug!(worker_id = %self.worker_id, "SkillEvolutionWorker woken up");
                    self.tick_guarded().await;
                }
                _ = interval.tick() => {
                    debug!(worker_id = %self.worker_id, "SkillEvolutionWorker poll tick");
                    self.tick_guarded().await;
                }
            }
        }
    }

    /// 带 panic 隔离的 tick：单步 panic 不能静默杀死 worker 任务（启动点无监督者重启）。
    async fn tick_guarded(&self) {
        use futures::FutureExt;
        if std::panic::AssertUnwindSafe(self.tick())
            .catch_unwind()
            .await
            .is_err()
        {
            error!(worker_id = %self.worker_id, "SkillEvolutionWorker tick panicked; continuing");
        }
    }

    /// 驱动一次 skill evolution pipeline tick（单次模式使用）
    /// 返回 true 表示本次 claim 到了 workflow，false 表示无可 claim 的 workflow。
    pub async fn tick(&self) -> bool {
        if let Err(e) = self.recover().await {
            warn!(error = %e, "Failed to recover skill evolution workflows");
        }

        if let Err(e) = self.enqueue_pending_records().await {
            warn!(error = %e, "Failed to sync pending skill evolution records");
        }

        let workflow = match blocking::claim_next(
            &self.store,
            &self.worker_id,
            self.lease_duration_secs,
            Some("skill"),
            None,
        )
        .await
        {
            Ok(Some(workflow)) => workflow,
            Ok(None) => {
                if let Err(e) = self.service.tick_observations().await {
                    warn!(error = %e, "Skill evolution observation tick failed");
                }
                if let Err(e) = self.finalize_observation_workflows().await {
                    warn!(error = %e, "Failed to finalize skill observation workflows");
                }
                return false;
            }
            Err(e) => {
                error!(error = %e, "Failed to claim skill evolution workflow");
                return false;
            }
        };

        info!(
            worker_id = %self.worker_id,
            workflow_id = %workflow.id,
            skill = %workflow.capability_id,
            evolution_id = %workflow.description,
            "Claimed skill evolution workflow"
        );

        if self.cancelled(&workflow).await {
            let _ = blocking::update_workflow_status_if_owned(
                &self.store,
                &workflow.id,
                &self.worker_id,
                "Cancelled",
                None,
            )
            .await;
            let _ = blocking::release_lease(&self.store, &workflow.id, &self.worker_id).await;
            return true;
        }

        match blocking::renew_lease(
            &self.store,
            &workflow.id,
            &self.worker_id,
            self.lease_duration_secs,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                warn!(workflow_id = %workflow.id, "Lost skill evolution lease before pipeline");
                return true;
            }
            Err(e) => {
                warn!(workflow_id = %workflow.id, error = %e, "Failed to renew skill evolution lease");
                return true;
            }
        }

        let step_id = match blocking::insert_step(
            &self.store,
            &workflow.id,
            SKILL_PIPELINE_STEP,
            None,
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                error!(workflow_id = %workflow.id, error = %e, "Failed to insert skill evolution step");
                let _ = blocking::release_lease(&self.store, &workflow.id, &self.worker_id).await;
                return true;
            }
        };

        let (heartbeat_stop, lease_lost, heartbeat_handle) =
            self.spawn_lease_heartbeat(&workflow.id);
        let result = run_with_lease_guard(self.run_skill_workflow(&workflow), lease_lost).await;
        let _ = heartbeat_stop.send(());
        if let Err(e) = heartbeat_handle.await {
            if !e.is_cancelled() {
                warn!(workflow_id = %workflow.id, error = %e, "Skill evolution lease heartbeat failed");
            }
        }
        let Some(result) = result else {
            warn!(workflow_id = %workflow.id, "Lost skill evolution lease while pipeline was running; cancelled pipeline future");
            return true;
        };

        match blocking::renew_lease(
            &self.store,
            &workflow.id,
            &self.worker_id,
            self.lease_duration_secs,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                warn!(workflow_id = %workflow.id, "Lost skill evolution lease after pipeline");
                return true;
            }
            Err(e) => {
                warn!(workflow_id = %workflow.id, error = %e, "Failed to renew skill evolution lease after pipeline");
                return true;
            }
        }

        match result {
            Ok(PipelineOutcome::Done(output_json)) => {
                if !self
                    .complete_step(&workflow, &step_id, Some(&output_json))
                    .await
                {
                    return true;
                }
                // Use "Observing" instead of "Promoted" — the skill is in
                // observation window and may still be rolled back if error
                // rate exceeds threshold. "Promoted" would prevent rollback.
                let _ = blocking::update_workflow_status_if_owned(
                    &self.store,
                    &workflow.id,
                    &self.worker_id,
                    "Observing",
                    None,
                )
                .await;
            }
            Ok(PipelineOutcome::StillRunning) => {
                // Pipeline 仍在运行 — 不是失败也不是完成。
                // 不立即 notify_one()，避免忙轮询。
                // 使用 defer_workflow_if_owned 设置 RetryScheduled + backoff lease_until，
                // claim_next 只在 lease_until < now 时领取，所以 60 秒内不会重新领取。
                // 不增加 attempt，避免重试计数膨胀。
                info!(
                    workflow_id = %workflow.id,
                    "Skill evolution pipeline still running, deferring with backoff"
                );
                if !self
                    .complete_step(&workflow, &step_id, Some("{\"status\":\"StillRunning\"}"))
                    .await
                {
                    return true;
                }
                match blocking::defer_workflow_if_owned(
                    &self.store,
                    &workflow.id,
                    &self.worker_id,
                    60, // 60 秒 backoff
                    Some("StillRunning — pipeline in progress, deferring"),
                )
                .await
                {
                    Ok(true) => {
                        // 不调用 notify_one() — 让 poll interval 自然触发下次检查
                    }
                    Ok(false) => {
                        warn!(workflow_id = %workflow.id, "Lost lease before deferring StillRunning workflow");
                    }
                    Err(e) => {
                        warn!(workflow_id = %workflow.id, error = %e, "Failed to defer StillRunning workflow");
                    }
                }
            }
            Err(e) => {
                let message = e.to_string();
                let _ = blocking::fail_step_if_owned(
                    &self.store,
                    &workflow.id,
                    &step_id,
                    &self.worker_id,
                    &message,
                )
                .await;
                if self.skill_record_is_terminal(&workflow) {
                    let _ = blocking::update_workflow_status_if_owned(
                        &self.store,
                        &workflow.id,
                        &self.worker_id,
                        "Failed",
                        Some(&message),
                    )
                    .await;
                } else {
                    let _ = blocking::schedule_retry_if_owned(
                        &self.store,
                        &workflow.id,
                        &self.worker_id,
                        Some(&message),
                    )
                    .await;
                }
            }
        }

        let _ = blocking::release_lease(&self.store, &workflow.id, &self.worker_id).await;

        if let Err(e) = self.service.tick_observations().await {
            warn!(error = %e, "Skill evolution observation tick failed");
        }
        if let Err(e) = self.finalize_observation_workflows().await {
            warn!(error = %e, "Failed to finalize skill observation workflows");
        }

        true
    }

    async fn enqueue_pending_records(&self) -> Result<()> {
        let pending = self.service.list_pending_ids().await;
        if pending.is_empty() {
            return Ok(());
        }

        let existing = blocking::list_active_workflows(&self.store).await?;
        for (skill_name, evolution_id) in pending {
            if existing.iter().any(|workflow| {
                workflow.description == evolution_id && Self::workflow_blocks_enqueue(workflow)
            }) {
                continue;
            }

            let workflow_id =
                blocking::enqueue(&self.store, &skill_name, &evolution_id, "skill").await?;
            info!(
                workflow_id = %workflow_id,
                skill = %skill_name,
                evolution_id = %evolution_id,
                "Enqueued durable skill evolution workflow"
            );
            self.wakeup.notify_one();
        }

        Ok(())
    }

    fn workflow_blocks_enqueue(workflow: &WorkflowRecord) -> bool {
        !matches!(
            workflow.status.as_str(),
            "Promoted" | "Failed" | "Cancelled"
        )
    }

    async fn run_skill_workflow(&self, workflow: &WorkflowRecord) -> Result<PipelineOutcome> {
        let skill_name = workflow.capability_id.as_str();
        let evolution_id = workflow.description.as_str();

        // Atomically run the pipeline — process_pending_evolution now returns
        // SingleEvolutionResult which distinguishes Completed/Failed/AlreadyRunning
        // without the TOCTOU race that existed when we checked
        // is_evolution_pipeline_active() before calling it.
        let result = self
            .service
            .process_pending_evolution(skill_name, evolution_id)
            .await;

        match result {
            Ok(SingleEvolutionResult::Completed) => {
                let record = self.service.evolution().load_record(evolution_id)?;
                Ok(PipelineOutcome::Done(
                    serde_json::json!({
                        "skill_name": skill_name,
                        "evolution_id": evolution_id,
                        "status": format!("{:?}", record.status),
                    })
                    .to_string(),
                ))
            }
            Ok(SingleEvolutionResult::Failed) => Err(Error::Evolution(
                "skill evolution failed (all retries exhausted)".into(),
            )),
            Ok(SingleEvolutionResult::AlreadyRunning) => {
                // Pipeline 已由其他 worker/tick 驱动 — 不是失败也不是完成。
                // 释放 lease，下次 tick 重新检查。
                info!(
                    evolution_id = %evolution_id,
                    "Skill evolution pipeline already running via another caller, deferring"
                );
                Ok(PipelineOutcome::StillRunning)
            }
            Ok(SingleEvolutionResult::WaitingForProvider) => {
                // 无 LLM provider，record 仅标记为 Generating。
                // workflow 不应进入 Observing，走 defer/release 路径。
                info!(
                    evolution_id = %evolution_id,
                    "Skill evolution waiting for LLM provider, deferring workflow"
                );
                Ok(PipelineOutcome::StillRunning)
            }
            Ok(SingleEvolutionResult::NotRunnable(status)) => match *status.normalize() {
                EvolutionStatus::Failed | EvolutionStatus::RolledBack => {
                    warn!(
                        evolution_id = %evolution_id,
                        status = ?status,
                        "Skill evolution reached terminal status, failing workflow"
                    );
                    Err(Error::Evolution(format!(
                        "skill evolution reached terminal status: {:?}",
                        status
                    )))
                }
                _ => {
                    info!(
                        evolution_id = %evolution_id,
                        status = ?status,
                        "Skill evolution record not runnable (waitable status), deferring workflow"
                    );
                    Ok(PipelineOutcome::StillRunning)
                }
            },
            Err(e) => {
                let record = self.service.evolution().load_record(evolution_id).ok();
                let status = record.as_ref().map(|r| r.status.normalize());
                match status {
                    Some(s)
                        if matches!(
                            *s,
                            EvolutionStatus::Observing | EvolutionStatus::Completed
                        ) =>
                    {
                        Ok(PipelineOutcome::Done(
                            serde_json::json!({
                                "skill_name": skill_name,
                                "evolution_id": evolution_id,
                                "status": format!("{:?}", s),
                            })
                            .to_string(),
                        ))
                    }
                    _ => Err(e),
                }
            }
        }
    }

    fn skill_record_is_terminal(&self, workflow: &WorkflowRecord) -> bool {
        self.service
            .evolution()
            .load_record(&workflow.description)
            .map(|record| {
                matches!(
                    *record.status.normalize(),
                    EvolutionStatus::Completed
                        | EvolutionStatus::Failed
                        | EvolutionStatus::RolledBack
                        | EvolutionStatus::Observing
                )
            })
            .unwrap_or(false)
    }

    async fn complete_step(
        &self,
        workflow: &WorkflowRecord,
        step_id: &str,
        output_json: Option<&str>,
    ) -> bool {
        match blocking::complete_step_if_owned(
            &self.store,
            &workflow.id,
            step_id,
            &self.worker_id,
            output_json,
        )
        .await
        {
            Ok(true) => true,
            Ok(false) => {
                warn!(workflow_id = %workflow.id, "Lost skill evolution lease before completing step");
                false
            }
            Err(e) => {
                warn!(workflow_id = %workflow.id, error = %e, "Failed to complete skill evolution step");
                false
            }
        }
    }

    async fn cancelled(&self, workflow: &WorkflowRecord) -> bool {
        match blocking::read_pending_events(&self.store, &workflow.id).await {
            Ok(events) => events.iter().any(|event| event.event_type == "cancel"),
            Err(e) => {
                warn!(workflow_id = %workflow.id, error = %e, "Failed to read skill evolution events");
                false
            }
        }
    }

    fn spawn_lease_heartbeat(
        &self,
        workflow_id: &str,
    ) -> (
        oneshot::Sender<()>,
        tokio::sync::watch::Receiver<bool>,
        JoinHandle<()>,
    ) {
        let store = self.store.clone();
        let workflow_id = workflow_id.to_string();
        let worker_id = self.worker_id.clone();
        let lease_duration_secs = self.lease_duration_secs;
        let heartbeat_secs = (lease_duration_secs / 3).clamp(5, 60) as u64;
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let (lost_tx, lost_rx) = tokio::sync::watch::channel(false);

        let handle = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(heartbeat_secs));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut consecutive_failures = 0u32;
            const MAX_CONSECUTIVE_FAILURES: u32 = 3;

            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = interval.tick() => {
                        match blocking::renew_lease(&store, &workflow_id, &worker_id, lease_duration_secs).await {
                            Ok(true) => {
                                consecutive_failures = 0;
                            }
                            Ok(false) => {
                                warn!(workflow_id = %workflow_id, worker_id = %worker_id, "Lost skill evolution lease during heartbeat");
                                let _ = lost_tx.send(true);
                                break;
                            }
                            Err(e) => {
                                consecutive_failures += 1;
                                warn!(workflow_id = %workflow_id, worker_id = %worker_id, error = %e, consecutive_failures = consecutive_failures, max_retries = MAX_CONSECUTIVE_FAILURES, "Failed to heartbeat skill evolution lease (will retry)");
                                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                                    warn!(workflow_id = %workflow_id, worker_id = %worker_id, "Too many consecutive heartbeat failures, giving up lease");
                                    let _ = lost_tx.send(true);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        });

        (stop_tx, lost_rx, handle)
    }

    async fn recover(&self) -> Result<()> {
        let recovered = blocking::recover_expired_leases(&self.store).await?;
        if !recovered.is_empty() {
            info!(
                count = recovered.len(),
                "Recovered skill evolution workflows"
            );
            self.wakeup.notify_one();
        }
        Ok(())
    }
}

impl EvolutionNotifier for SkillEvolutionWorker {
    fn notify(&self) {
        self.wakeup.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_workflow_maps_record_terminal_status() {
        assert_eq!(
            SkillEvolutionWorker::workflow_status_for_evolution(&EvolutionStatus::Completed),
            Some("Promoted")
        );
        assert_eq!(
            SkillEvolutionWorker::workflow_status_for_evolution(&EvolutionStatus::RolledBack),
            Some("Failed")
        );
        assert_eq!(
            SkillEvolutionWorker::workflow_status_for_evolution(&EvolutionStatus::Observing),
            None
        );
    }
}
