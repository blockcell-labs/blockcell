//! Helpers for running blocking storage work off the async runtime.
//!
//! `EvolutionWorkflowStore` and the cross-process file locks are synchronous
//! (rusqlite + `std::thread::sleep` retry loops). Calling them directly from an
//! `async fn` parks a tokio worker thread for the duration of the lock wait
//! (up to ~5s for SQLite `busy_timeout`, ~6s for `CrossProcessLock`), which
//! stalls unrelated tasks on the same executor. Route them through these
//! helpers instead.

use blockcell_core::{Error, Result};
use blockcell_storage::evolution_workflow::{EventRecord, StepRecord, WorkflowRecord};
use blockcell_storage::EvolutionWorkflowStore;

/// Run a `blockcell_core::Result`-returning blocking operation on a blocking
/// thread. Mirrors `blockcell_tools::memory::run_store_blocking`.
pub(crate) async fn run_blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Other(format!("blocking storage task panicked: {e}")))?
}

/// Run a `std::io::Result`-returning blocking operation on a blocking thread.
pub(crate) async fn run_blocking_io<T, F>(f: F) -> std::io::Result<T>
where
    F: FnOnce() -> std::io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| std::io::Error::other(format!("blocking lock task panicked: {e}")))?
}

// ── EvolutionWorkflowStore async wrappers ──────────────────────────────
//
// Each wrapper clones the (cheap, `Arc`-backed) store handle, moves owned
// arguments into a closure and runs it on a blocking thread, so worker
// `tick()` loops never hold a tokio worker while SQLite waits on a lock.

pub(crate) async fn recover_expired_leases(
    store: &EvolutionWorkflowStore,
) -> Result<Vec<WorkflowRecord>> {
    let store = store.clone();
    run_blocking(move || store.recover_expired_leases()).await
}

pub(crate) async fn claim_next(
    store: &EvolutionWorkflowStore,
    worker_id: &str,
    lease_secs: i64,
    provider_kind: Option<&str>,
    exclude_provider_kind: Option<&str>,
) -> Result<Option<WorkflowRecord>> {
    let store = store.clone();
    let worker_id = worker_id.to_string();
    let provider_kind = provider_kind.map(str::to_string);
    let exclude_provider_kind = exclude_provider_kind.map(str::to_string);
    run_blocking(move || {
        store.claim_next(
            &worker_id,
            lease_secs,
            provider_kind.as_deref(),
            exclude_provider_kind.as_deref(),
        )
    })
    .await
}

pub(crate) async fn renew_lease(
    store: &EvolutionWorkflowStore,
    workflow_id: &str,
    worker_id: &str,
    lease_secs: i64,
) -> Result<bool> {
    let store = store.clone();
    let workflow_id = workflow_id.to_string();
    let worker_id = worker_id.to_string();
    run_blocking(move || store.renew_lease(&workflow_id, &worker_id, lease_secs)).await
}

pub(crate) async fn release_lease(
    store: &EvolutionWorkflowStore,
    workflow_id: &str,
    worker_id: &str,
) -> Result<bool> {
    let store = store.clone();
    let workflow_id = workflow_id.to_string();
    let worker_id = worker_id.to_string();
    run_blocking(move || store.release_lease(&workflow_id, &worker_id)).await
}

pub(crate) async fn read_pending_events(
    store: &EvolutionWorkflowStore,
    workflow_id: &str,
) -> Result<Vec<EventRecord>> {
    let store = store.clone();
    let workflow_id = workflow_id.to_string();
    run_blocking(move || store.read_pending_events(&workflow_id)).await
}

pub(crate) async fn insert_step(
    store: &EvolutionWorkflowStore,
    workflow_id: &str,
    step_name: &str,
    input_json: Option<&str>,
) -> Result<String> {
    let store = store.clone();
    let workflow_id = workflow_id.to_string();
    let step_name = step_name.to_string();
    let input_json = input_json.map(str::to_string);
    run_blocking(move || store.insert_step(&workflow_id, &step_name, input_json.as_deref())).await
}

pub(crate) async fn complete_step_if_owned(
    store: &EvolutionWorkflowStore,
    workflow_id: &str,
    step_id: &str,
    worker_id: &str,
    output_json: Option<&str>,
) -> Result<bool> {
    let store = store.clone();
    let workflow_id = workflow_id.to_string();
    let step_id = step_id.to_string();
    let worker_id = worker_id.to_string();
    let output_json = output_json.map(str::to_string);
    run_blocking(move || {
        store.complete_step_if_owned(&workflow_id, &step_id, &worker_id, output_json.as_deref())
    })
    .await
}

pub(crate) async fn fail_step_if_owned(
    store: &EvolutionWorkflowStore,
    workflow_id: &str,
    step_id: &str,
    worker_id: &str,
    error: &str,
) -> Result<bool> {
    let store = store.clone();
    let workflow_id = workflow_id.to_string();
    let step_id = step_id.to_string();
    let worker_id = worker_id.to_string();
    let error = error.to_string();
    run_blocking(move || store.fail_step_if_owned(&workflow_id, &step_id, &worker_id, &error)).await
}

pub(crate) async fn schedule_retry_if_owned(
    store: &EvolutionWorkflowStore,
    workflow_id: &str,
    worker_id: &str,
    last_error: Option<&str>,
) -> Result<bool> {
    let store = store.clone();
    let workflow_id = workflow_id.to_string();
    let worker_id = worker_id.to_string();
    let last_error = last_error.map(str::to_string);
    run_blocking(move || {
        store.schedule_retry_if_owned(&workflow_id, &worker_id, last_error.as_deref())
    })
    .await
}

pub(crate) async fn defer_workflow_if_owned(
    store: &EvolutionWorkflowStore,
    workflow_id: &str,
    worker_id: &str,
    backoff_secs: i64,
    note: Option<&str>,
) -> Result<bool> {
    let store = store.clone();
    let workflow_id = workflow_id.to_string();
    let worker_id = worker_id.to_string();
    let note = note.map(str::to_string);
    run_blocking(move || {
        store.defer_workflow_if_owned(&workflow_id, &worker_id, backoff_secs, note.as_deref())
    })
    .await
}

pub(crate) async fn update_workflow_status_if_owned(
    store: &EvolutionWorkflowStore,
    workflow_id: &str,
    worker_id: &str,
    status: &str,
    last_error: Option<&str>,
) -> Result<bool> {
    let store = store.clone();
    let workflow_id = workflow_id.to_string();
    let worker_id = worker_id.to_string();
    let status = status.to_string();
    let last_error = last_error.map(str::to_string);
    run_blocking(move || {
        store.update_workflow_status_if_owned(
            &workflow_id,
            &worker_id,
            &status,
            last_error.as_deref(),
        )
    })
    .await
}

pub(crate) async fn update_workflow_status(
    store: &EvolutionWorkflowStore,
    workflow_id: &str,
    status: &str,
    last_error: Option<&str>,
) -> Result<()> {
    let store = store.clone();
    let workflow_id = workflow_id.to_string();
    let status = status.to_string();
    let last_error = last_error.map(str::to_string);
    run_blocking(move || store.update_workflow_status(&workflow_id, &status, last_error.as_deref()))
        .await
}

pub(crate) async fn get_last_completed_step(
    store: &EvolutionWorkflowStore,
    workflow_id: &str,
) -> Result<Option<StepRecord>> {
    let store = store.clone();
    let workflow_id = workflow_id.to_string();
    run_blocking(move || store.get_last_completed_step(&workflow_id)).await
}

pub(crate) async fn list_workflows(
    store: &EvolutionWorkflowStore,
    status_filter: Option<&str>,
) -> Result<Vec<WorkflowRecord>> {
    let store = store.clone();
    let status_filter = status_filter.map(str::to_string);
    run_blocking(move || store.list_workflows(status_filter.as_deref())).await
}

/// List non-terminal workflows (excludes Promoted/Failed/Cancelled).
pub(crate) async fn list_active_workflows(
    store: &EvolutionWorkflowStore,
) -> Result<Vec<WorkflowRecord>> {
    let store = store.clone();
    run_blocking(move || store.list_active_workflows()).await
}

pub(crate) async fn enqueue(
    store: &EvolutionWorkflowStore,
    capability_id: &str,
    description: &str,
    provider_kind: &str,
) -> Result<String> {
    let store = store.clone();
    let capability_id = capability_id.to_string();
    let description = description.to_string();
    let provider_kind = provider_kind.to_string();
    run_blocking(move || store.enqueue(&capability_id, &description, &provider_kind)).await
}
