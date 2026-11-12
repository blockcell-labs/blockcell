use super::*;
use crate::job::{JobPayload, JobSchedule, JobState};
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
async fn r5_due_jobs_dispatch_while_first_receipt_is_pending() {
    let (_paths, service, mut rx) = fixture(2).await;
    let tick = tokio::spawn({
        let service = service.clone();
        async move { service.run_tick().await }
    });
    let first = receive(&mut rx).await;
    let second = receive(&mut rx).await;
    assert_ne!(first.chat_id, second.chat_id);
    complete(&first);
    complete(&second);
    tick.await.unwrap().unwrap();
}

#[tokio::test]
async fn r5_concurrent_ticks_do_not_dispatch_same_job_twice() {
    let (_paths, service, mut rx) = fixture(1).await;
    let first_tick = tokio::spawn({
        let service = service.clone();
        async move { service.run_tick().await }
    });
    let first = receive(&mut rx).await;
    tokio::time::timeout(std::time::Duration::from_millis(500), service.run_tick())
        .await
        .expect("a duplicate tick waited on a second execution of the same job")
        .unwrap();
    assert!(
        rx.try_recv().is_err(),
        "same job was dispatched twice while its first receipt was pending"
    );
    complete(&first);
    first_tick.await.unwrap().unwrap();
}

#[tokio::test]
async fn r5_replacement_is_not_completed_by_an_older_execution() {
    let (_paths, service, mut rx) = fixture(1).await;
    let tick = tokio::spawn({
        let service = service.clone();
        async move { service.run_tick().await }
    });
    let old = receive(&mut rx).await;
    let mut replacement = due_job(0);
    replacement.payload.message = "replacement task".into();
    replacement.updated_at_ms = 2;
    service.add_job(replacement).await.unwrap();
    complete(&old);
    tick.await.unwrap().unwrap();
    let jobs = service.list_jobs().await;
    assert_eq!(jobs.len(), 1);
    assert!(
        jobs[0].enabled,
        "old completion disabled the replacement job"
    );
    assert!(
        jobs[0].state.last_run_at_ms.is_none(),
        "old completion was attributed to a replacement"
    );
}

#[tokio::test]
async fn r5_disable_and_delete_cancel_unclaimed_execution_without_waiting() {
    for delete in [false, true] {
        let (paths, service, mut rx) = fixture(1).await;
        let mut tick = tokio::spawn({
            let service = service.clone();
            async move { service.run_tick().await }
        });
        let message = receive(&mut rx).await;
        if delete {
            service.remove_job(&message.chat_id).await.unwrap();
        } else {
            service
                .update_job_enabled(&message.chat_id, false)
                .await
                .unwrap();
        }
        let result = tokio::time::timeout(std::time::Duration::from_millis(500), &mut tick).await;
        if result.is_err() {
            tick.abort();
        }
        assert!(
            result.is_ok(),
            "mutation left an unclaimed execution waiting for its deadline"
        );
        let saved: JobStore =
            serde_json::from_slice(&tokio::fs::read(paths.paths.cron_jobs_file()).await.unwrap())
                .unwrap();
        assert!(if delete {
            saved.jobs.is_empty()
        } else {
            saved.jobs.len() == 1 && !saved.jobs[0].enabled
        });
        assert!(
            !complete_message_receipt(
                message.metadata[MESSAGE_RECEIPT_ID].as_str().unwrap(),
                Ok(())
            ),
            "cancelled unclaimed execution retained its receipt"
        );
    }
}

#[tokio::test]
async fn r5_dropped_tick_cleans_receipt_and_allows_a_fresh_execution() {
    let (_paths, service, mut rx) = fixture(1).await;
    let tick = tokio::spawn({
        let service = service.clone();
        async move { service.run_tick().await }
    });
    let message = receive(&mut rx).await;
    let old_id = message.metadata[MESSAGE_RECEIPT_ID]
        .as_str()
        .unwrap()
        .to_string();
    tick.abort();
    let _ = tick.await;
    assert!(
        !cancel_message_receipt(&old_id),
        "dropped tick orphaned its receipt sender"
    );
    let fresh = tokio::spawn({
        let service = service.clone();
        async move { service.run_tick().await }
    });
    let retry = receive(&mut rx).await;
    assert_ne!(retry.metadata[MESSAGE_RECEIPT_ID].as_str().unwrap(), old_id);
    complete(&retry);
    fresh.await.unwrap().unwrap();
}

#[tokio::test]
async fn r5_shutdown_does_not_wait_for_execution_receipt_deadline() {
    let (_paths, service, mut rx) = fixture(1).await;
    let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);
    let mut run = tokio::spawn(service.run_loop(shutdown_rx));
    let message = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    shutdown_tx.send(()).unwrap();
    let stopped = tokio::time::timeout(std::time::Duration::from_millis(500), &mut run).await;
    if stopped.is_err() {
        run.abort();
    }
    let _ = cancel_message_receipt(message.metadata[MESSAGE_RECEIPT_ID].as_str().unwrap());
    assert!(
        stopped.is_ok(),
        "shutdown waited on the cron completion deadline"
    );
}

#[tokio::test]
async fn r5_in_flight_is_bounded_to_eight_jobs_across_ticks() {
    let (_paths, service, mut rx) = fixture(10).await;
    let first_tick = tokio::spawn({
        let service = service.clone();
        async move { service.run_tick().await }
    });
    let mut pending = Vec::new();
    for _ in 0..8 {
        pending.push(receive(&mut rx).await);
    }
    service.run_tick().await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .is_err(),
        "more than eight jobs started before any receipt completed"
    );
    let completed_id = pending[0].chat_id.clone();
    complete(&pending.remove(0));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if service
                .list_jobs()
                .await
                .iter()
                .any(|job| job.id == completed_id && job.state.last_status == Some(JobStatus::Ok))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let next_tick = tokio::spawn({
        let service = service.clone();
        async move { service.run_tick().await }
    });
    pending.push(receive(&mut rx).await);
    for message in pending {
        complete(&message);
    }
    first_tick.await.unwrap().unwrap();
    next_tick.await.unwrap().unwrap();
    let last_tick = tokio::spawn({
        let service = service.clone();
        async move { service.run_tick().await }
    });
    complete(&receive(&mut rx).await);
    last_tick.await.unwrap().unwrap();
}
