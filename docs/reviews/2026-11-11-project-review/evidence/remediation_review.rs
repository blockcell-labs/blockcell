use blockcell_scheduler::cron_service::{CronService, JobStore};
use blockcell_scheduler::{CronJob, ScheduleKind};
use blockcell_core::{Paths, InboundMessage};
use blockcell_core::message_receipt::{MESSAGE_RECEIPT_ID, register_message_receipt, claim_message_receipt, cancel_message_receipt, cancel_message_receipt_and_wait};
use std::sync::Arc;
use tokio::sync::mpsc;
use blockcell_scheduler::job::{JobPayload, JobSchedule, JobState};
use blockcell_core::message_receipt::complete_message_receipt;

struct TestPaths {
    base: std::path::PathBuf,
    paths: Paths,
}

impl Drop for TestPaths {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn due_job(index: usize) -> CronJob {
    CronJob {
        id: format!("r5-job-{index}"),
        name: format!("job {index}"),
        enabled: true,
        schedule: JobSchedule {
            kind: ScheduleKind::At,
            at_ms: Some(1),
            every_ms: None,
            expr: None,
            tz: None,
            run_immediately: false,
        },
        payload: JobPayload {
            kind: "agent".into(),
            message: format!("perform job {index}"),
            deliver: false,
            channel: None,
            to: None,
            account_id: None,
            script_kind: None,
            skill_name: None,
        },
        state: JobState {
            next_run_at_ms: Some(1),
            ..Default::default()
        },
        created_at_ms: 1,
        updated_at_ms: 1,
        delete_after_run: false,
    }
}

async fn fixture(count: usize) -> (TestPaths, Arc<CronService>, mpsc::Receiver<InboundMessage>) {
    let base =
        std::env::temp_dir().join(format!("blockcell-cron-lifecycle-{}", uuid::Uuid::new_v4()));
    let paths = Paths::with_base(base.clone());
    tokio::fs::create_dir_all(paths.cron_dir()).await.unwrap();
    let store = JobStore {
        version: 1,
        jobs: (0..count).map(due_job).collect(),
    };
    tokio::fs::write(paths.cron_jobs_file(), serde_json::to_vec(&store).unwrap())
        .await
        .unwrap();
    let (tx, rx) = mpsc::channel(32);
    let service = Arc::new(CronService::new_with_options(
        paths.clone(),
        tx,
        None,
        Some(1),
        None,
    ));
    (TestPaths { base, paths }, service, rx)
}

async fn receive(rx: &mut mpsc::Receiver<InboundMessage>) -> InboundMessage {
    tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await
        .expect("due job was blocked by another pending receipt")
        .expect("cron sender")
}

fn complete(message: &InboundMessage) {
    let _ = complete_message_receipt(
        message.metadata[MESSAGE_RECEIPT_ID].as_str().unwrap(),
        Ok(()),
    );
}


#[tokio::test]
async fn review_dropped_tick_must_keep_claimed_job_leased_until_ack() {
    let (_paths, service, mut rx) = fixture(1).await;
    let tick = tokio::spawn({let service = service.clone(); async move {service.run_tick().await}});
    let old = receive(&mut rx).await;
    let old_id = old.metadata[MESSAGE_RECEIPT_ID].as_str().unwrap();
    let active_claim = claim_message_receipt(old_id).unwrap();
    tick.abort(); let _ = tick.await;
    let retry = tokio::spawn({let service = service.clone(); async move {service.run_tick().await}});
    let redelivered = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
    retry.abort(); let _ = retry.await;
    drop(active_claim);
    assert!(redelivered.is_err(), "same job re-dispatched before the old runtime claim acknowledged cancellation");
}

#[tokio::test]
async fn review_receiver_reclaim_must_cancel_the_active_claim() {
    let (id, rx) = register_message_receipt();
    let claim = claim_message_receipt(&id).unwrap();
    drop(rx);
    let (other, _rx) = register_message_receipt();
    cancel_message_receipt_and_wait(&id).await;
    let notified = tokio::time::timeout(std::time::Duration::from_millis(100), claim.cancelled()).await;
    cancel_message_receipt(&other);
    assert!(notified.is_ok(), "receiver cleanup removed the only cancellation handle while runtime work remains active");
}
