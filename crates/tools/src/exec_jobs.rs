use async_trait::async_trait;
use blockcell_core::{Error, Result};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

use crate::sandbox::{SandboxPolicy, SandboxProvider};
use crate::shell_session::{
    close_exec_shell_session, poll_exec_shell_session, start_exec_shell_session,
};
use crate::{Tool, ToolContext, ToolSchema};

const MAX_RUNNING_JOBS_PER_OWNER: usize = 16;
const MAX_JOB_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_JOB_READ_BYTES: usize = 64 * 1024;
const INITIAL_OUTPUT_TAIL_BYTES: usize = 4 * 1024;
const COMPLETION_OUTPUT_TAIL_BYTES: usize = 2 * 1024;

struct Job {
    shell_session_id: Option<String>,
    owner_agent_id: String,
    owner_session_id: String,
    running: bool,
    exit_code: Option<i32>,
    output_since_read: Vec<u8>,
    output_truncated: bool,
    output_tail: VecDeque<u8>,
    notify: Arc<Notify>,
}

#[derive(Default)]
struct RegistryState {
    jobs: HashMap<String, Job>,
    active_turns: HashMap<String, usize>,
    notification_targets: HashMap<String, HashMap<String, usize>>,
    completed_notifications: HashMap<String, VecDeque<String>>,
}

static JOBS: Lazy<Mutex<RegistryState>> = Lazy::new(|| Mutex::new(RegistryState::default()));

pub struct JobTurnGuard {
    owner_session_key: String,
    notification_session_key: String,
}

impl JobTurnGuard {
    pub fn new(owner_session_key: &str, notification_session_key: &str) -> Self {
        if let Ok(mut state) = JOBS.lock() {
            *state
                .active_turns
                .entry(owner_session_key.to_string())
                .or_default() += 1;
            *state
                .notification_targets
                .entry(owner_session_key.to_string())
                .or_default()
                .entry(notification_session_key.to_string())
                .or_default() += 1;
        }
        Self {
            owner_session_key: owner_session_key.to_string(),
            notification_session_key: notification_session_key.to_string(),
        }
    }
}

impl Drop for JobTurnGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = JOBS.lock() {
            if let Some(active) = state.active_turns.get_mut(&self.owner_session_key) {
                *active = active.saturating_sub(1);
                if *active == 0 {
                    state.active_turns.remove(&self.owner_session_key);
                    state
                        .completed_notifications
                        .remove(&self.notification_session_key);
                }
            }
            let mut notification_target_ended = false;
            let mut owner_targets_empty = false;
            if let Some(targets) = state.notification_targets.get_mut(&self.owner_session_key) {
                if let Some(count) = targets.get_mut(&self.notification_session_key) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        targets.remove(&self.notification_session_key);
                        notification_target_ended = true;
                    }
                }
                owner_targets_empty = targets.is_empty();
            }
            if notification_target_ended {
                state
                    .completed_notifications
                    .remove(&self.notification_session_key);
            }
            if owner_targets_empty {
                state.notification_targets.remove(&self.owner_session_key);
            }
        }
    }
}

pub fn take_finished_job_notifications(session_key: &str) -> Vec<String> {
    JOBS.lock()
        .ok()
        .and_then(|mut state| state.completed_notifications.remove(session_key))
        .map(|messages| messages.into_iter().collect())
        .unwrap_or_default()
}

fn owner_agent_id(ctx: &ToolContext) -> String {
    ctx.agent_identity
        .as_ref()
        .map(|identity| identity.agent_id.as_str())
        .filter(|id| !id.is_empty())
        .unwrap_or("default")
        .to_string()
}

fn append_tail(tail: &mut VecDeque<u8>, bytes: &[u8], limit: usize) {
    tail.extend(bytes.iter().copied());
    while tail.len() > limit {
        tail.pop_front();
    }
}

fn tail_string(bytes: impl IntoIterator<Item = u8>, max_bytes: usize) -> String {
    let bytes: Vec<u8> = bytes.into_iter().collect();
    let start = bytes.len().saturating_sub(max_bytes);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

fn reserve_job(job_id: &str, owner_agent_id: &str, owner_session_id: &str) -> Result<()> {
    let mut state = JOBS
        .lock()
        .map_err(|_| Error::Tool("Exec job registry is unavailable".to_string()))?;
    let running = state
        .jobs
        .values()
        .filter(|job| {
            job.running
                && job.owner_agent_id == owner_agent_id
                && job.owner_session_id == owner_session_id
        })
        .count();
    if running >= MAX_RUNNING_JOBS_PER_OWNER {
        return Err(Error::Validation(format!(
            "At most {MAX_RUNNING_JOBS_PER_OWNER} exec jobs may run per session"
        )));
    }
    state.jobs.insert(
        job_id.to_string(),
        Job {
            shell_session_id: None,
            owner_agent_id: owner_agent_id.to_string(),
            owner_session_id: owner_session_id.to_string(),
            running: true,
            exit_code: None,
            output_since_read: Vec::new(),
            output_truncated: false,
            output_tail: VecDeque::new(),
            notify: Arc::new(Notify::new()),
        },
    );
    Ok(())
}

fn remove_job(job_id: &str) {
    if let Ok(mut state) = JOBS.lock() {
        state.jobs.remove(job_id);
    }
}

fn append_job_output(job_id: &str, output: &str) {
    if output.is_empty() {
        return;
    }
    let Ok(mut state) = JOBS.lock() else {
        return;
    };
    let Some(job) = state.jobs.get_mut(job_id) else {
        return;
    };
    let bytes = output.as_bytes();
    let remaining = MAX_JOB_OUTPUT_BYTES.saturating_sub(job.output_since_read.len());
    let take = remaining.min(bytes.len());
    job.output_since_read.extend_from_slice(&bytes[..take]);
    job.output_truncated |= take < bytes.len();
    append_tail(&mut job.output_tail, bytes, COMPLETION_OUTPUT_TAIL_BYTES);
    job.notify.notify_waiters();
}

fn finish_job(job_id: &str, exit_code: Option<i32>, output: &str) {
    if let Ok(mut state) = JOBS.lock() {
        let active_session = state.jobs.get(job_id).and_then(|job| {
            if !job.running {
                return None;
            }
            Some(job.owner_session_id.clone())
        });
        let Some(session_key) = active_session else {
            return;
        };
        let Some(job) = state.jobs.get_mut(job_id) else {
            return;
        };
        append_output_to_job(job, output);
        job.running = false;
        job.exit_code = exit_code;
        let tail = tail_string(
            job.output_tail.iter().copied(),
            COMPLETION_OUTPUT_TAIL_BYTES,
        );
        let message = format!(
            "[job {job_id} finished: exit {}] {tail}",
            exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
        job.notify.notify_waiters();
        if state
            .active_turns
            .get(&session_key)
            .copied()
            .unwrap_or_default()
            > 0
        {
            let targets = state
                .notification_targets
                .get(&session_key)
                .map(|targets| targets.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_else(|| vec![session_key]);
            for target in targets {
                state
                    .completed_notifications
                    .entry(target)
                    .or_default()
                    .push_back(message.clone());
            }
        }
    }
}

fn append_output_to_job(job: &mut Job, output: &str) {
    if output.is_empty() {
        return;
    }
    let bytes = output.as_bytes();
    let remaining = MAX_JOB_OUTPUT_BYTES.saturating_sub(job.output_since_read.len());
    let take = remaining.min(bytes.len());
    job.output_since_read.extend_from_slice(&bytes[..take]);
    job.output_truncated |= take < bytes.len();
    append_tail(&mut job.output_tail, bytes, COMPLETION_OUTPUT_TAIL_BYTES);
}

async fn monitor_job(job_id: String, shell_session_id: String, agent_id: String, owner: String) {
    loop {
        let running = JOBS
            .lock()
            .ok()
            .and_then(|state| state.jobs.get(&job_id).map(|job| job.running))
            .unwrap_or(false);
        if !running {
            break;
        }
        match poll_exec_shell_session(
            &shell_session_id,
            &agent_id,
            &owner,
            Duration::from_millis(150),
        )
        .await
        {
            Ok(output) => {
                append_job_output(&job_id, &output.output);
                if !output.running {
                    finish_job(&job_id, output.exit_code, "");
                    let _ = close_exec_shell_session(&shell_session_id, &agent_id, &owner).await;
                    break;
                }
            }
            Err(_) => {
                finish_job(&job_id, None, "");
                break;
            }
        }
    }
}

pub(crate) async fn execute_exec_job(
    ctx: ToolContext,
    command: &str,
    working_dir: PathBuf,
    policy: SandboxPolicy,
    writable_roots: Vec<PathBuf>,
    timeout_secs: u64,
    sandbox: Arc<dyn SandboxProvider>,
) -> Result<Value> {
    let agent_id = owner_agent_id(&ctx);
    let owner_session_id = ctx.session_key.clone();
    let job_id = uuid::Uuid::new_v4().to_string();
    reserve_job(&job_id, &agent_id, &owner_session_id)?;

    let started = tokio::time::Instant::now();
    let (shell_session_id, output) =
        match start_exec_shell_session(crate::shell_session::ExecShellRequest {
            working_dir: &working_dir,
            policy,
            agent_id: &agent_id,
            owner_session_id: &owner_session_id,
            task_manager: ctx.task_manager.clone(),
            writable_roots,
            sandbox,
            command,
            timeout: Duration::from_secs(timeout_secs),
        })
        .await
        {
            Ok(result) => result,
            Err(error) => {
                remove_job(&job_id);
                return Err(error);
            }
        };

    if !output.running {
        remove_job(&job_id);
        let _ = close_exec_shell_session(&shell_session_id, &agent_id, &owner_session_id).await;
        let mut stdout = output.stdout;
        let mut stderr = output.stderr;
        let mut truncated = output.truncated;
        if stdout.chars().count() > 10_000 {
            stdout = format!(
                "{}\n... (output truncated)",
                crate::safe_truncate(&stdout, 10_000)
            );
            truncated = true;
        }
        if stderr.chars().count() > 10_000 {
            stderr = format!(
                "{}\n... (output truncated)",
                crate::safe_truncate(&stderr, 10_000)
            );
            truncated = true;
        }
        return Ok(json!({
            "exit_code": output.exit_code,
            "stdout": stdout,
            "stderr": stderr,
            "truncated": truncated
        }));
    }

    if let Ok(mut state) = JOBS.lock() {
        if let Some(job) = state.jobs.get_mut(&job_id) {
            job.shell_session_id = Some(shell_session_id.clone());
            append_tail(
                &mut job.output_tail,
                output.output.as_bytes(),
                COMPLETION_OUTPUT_TAIL_BYTES,
            );
        }
    }

    tokio::spawn(monitor_job(
        job_id.clone(),
        shell_session_id,
        agent_id,
        owner_session_id,
    ));
    Ok(json!({
        "status": "running",
        "job_id": job_id,
        "elapsed_secs": started.elapsed().as_secs_f64(),
        "output_tail": tail_string(output.output.bytes(), INITIAL_OUTPUT_TAIL_BYTES),
        "hint": "Command is still running in the background. Use job_output with this job_id to wait for/read more output, or job_kill to stop it."
    }))
}

fn verify_job_owner(job_id: &str, ctx: &ToolContext) -> Result<()> {
    let owner_agent_id = owner_agent_id(ctx);
    let state = JOBS
        .lock()
        .map_err(|_| Error::Tool("Exec job registry is unavailable".to_string()))?;
    let job = state
        .jobs
        .get(job_id)
        .ok_or_else(|| Error::NotFound(format!("Exec job not found: {job_id}")))?;
    if job.owner_agent_id != owner_agent_id || job.owner_session_id != ctx.session_key {
        return Err(Error::PermissionDenied(format!(
            "Exec job {job_id} belongs to another session or agent"
        )));
    }
    Ok(())
}

async fn read_job_output(ctx: &ToolContext, job_id: &str, wait_secs: u64) -> Result<Value> {
    verify_job_owner(job_id, ctx)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait_secs);
    loop {
        let notify = {
            let state = JOBS
                .lock()
                .map_err(|_| Error::Tool("Exec job registry is unavailable".to_string()))?;
            let job = state
                .jobs
                .get(job_id)
                .ok_or_else(|| Error::NotFound(format!("Exec job not found: {job_id}")))?;
            if !job.running || tokio::time::Instant::now() >= deadline {
                break;
            }
            Arc::clone(&job.notify)
        };
        tokio::select! {
            _ = notify.notified() => {},
            _ = tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + Duration::from_millis(100))) => {},
        }
    }

    take_job_output(ctx, job_id)
}

fn take_job_output(ctx: &ToolContext, job_id: &str) -> Result<Value> {
    verify_job_owner(job_id, ctx)?;
    let mut state = JOBS
        .lock()
        .map_err(|_| Error::Tool("Exec job registry is unavailable".to_string()))?;
    let job = state
        .jobs
        .get_mut(job_id)
        .ok_or_else(|| Error::NotFound(format!("Exec job not found: {job_id}")))?;
    let mut output = std::mem::take(&mut job.output_since_read);
    let mut truncated = std::mem::take(&mut job.output_truncated);
    if output.len() > MAX_JOB_READ_BYTES {
        output.drain(..output.len() - MAX_JOB_READ_BYTES);
        truncated = true;
    }
    Ok(json!({
        "job_id": job_id,
        "output": String::from_utf8_lossy(&output),
        "truncated": truncated,
        "running": job.running,
        "exit_code": job.exit_code
    }))
}

async fn kill_job(ctx: &ToolContext, job_id: &str) -> Result<Value> {
    verify_job_owner(job_id, ctx)?;
    let (shell_session_id, agent_id, owner_session_id, was_running) = {
        let mut state = JOBS
            .lock()
            .map_err(|_| Error::Tool("Exec job registry is unavailable".to_string()))?;
        let job = state
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| Error::NotFound(format!("Exec job not found: {job_id}")))?;
        let was_running = job.running;
        job.running = false;
        job.notify.notify_waiters();
        (
            job.shell_session_id.clone(),
            job.owner_agent_id.clone(),
            job.owner_session_id.clone(),
            was_running,
        )
    };
    if let Some(session_id) = shell_session_id {
        if was_running {
            close_exec_shell_session(&session_id, &agent_id, &owner_session_id).await?;
        }
    }
    Ok(json!({"job_id": job_id, "killed": was_running}))
}

pub struct JobOutputTool;

#[async_trait]
impl Tool for JobOutputTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "job_output".to_string(),
            description: "Wait for an exec job and read output since the last read.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "job_id": {"type": "string"},
                    "wait_secs": {"type": "integer", "minimum": 0, "maximum": 600}
                },
                "required": ["job_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("job_id").and_then(Value::as_str).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: job_id".to_string(),
            ));
        }
        if let Some(wait_secs) = params.get("wait_secs") {
            if wait_secs.as_u64().is_none_or(|wait| wait > 600) {
                return Err(Error::Validation(
                    "wait_secs must be an integer from 0 to 600".to_string(),
                ));
            }
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let job_id = params["job_id"].as_str().unwrap();
        let wait_secs = params
            .get("wait_secs")
            .and_then(Value::as_u64)
            .unwrap_or(30);
        read_job_output(&ctx, job_id, wait_secs).await
    }
}

pub struct JobKillTool;

#[async_trait]
impl Tool for JobKillTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "job_kill".to_string(),
            description: "Stop a running exec job.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {"job_id": {"type": "string"}},
                "required": ["job_id"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("job_id").and_then(Value::as_str).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: job_id".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        kill_job(&ctx, params["job_id"].as_str().unwrap()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::NativeSandboxProvider;
    use crate::ToolContext;

    fn context(session_key: &str) -> ToolContext {
        let workspace = std::env::temp_dir().join(format!("blockcell-job-test-{session_key}"));
        std::fs::create_dir_all(&workspace).unwrap();
        ToolContext {
            workspace: workspace.clone(),
            sandbox_writable_roots: Some(vec![workspace.clone()]),
            base: workspace,
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: session_key.to_string(),
            channel: "test".to_string(),
            account_id: None,
            sender_id: None,
            sender_role: None,
            chat_id: session_key.to_string(),
            config: blockcell_core::config::Config::default(),
            permissions: blockcell_core::types::PermissionSet::default(),
            task_manager: None,
            memory_store: None,
            memory_file_store: None,
            ghost_memory_lifecycle: None,
            skill_file_store: None,
            session_search: None,
            outbound_tx: None,
            spawn_handle: None,
            capability_registry: None,
            core_evolution: None,
            event_emitter: None,
            channel_contacts_file: None,
            response_cache: None,
            runtime_handle: None,
            agent_identity: None,
            skill_mutex: None,
            agent_type_registry: None,
            evolution_workflow_store: None,
        }
    }

    #[tokio::test]
    async fn timeout_returns_running_job_and_output_is_incremental() {
        let ctx = context("exec-jobs-live");
        let workspace = ctx.workspace.clone();
        let result = execute_exec_job(
            ctx.clone(),
            "sleep 0.2; printf first; sleep 2; printf second",
            workspace,
            SandboxPolicy::FullAccess,
            Vec::new(),
            0,
            Arc::new(NativeSandboxProvider),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "running");
        let job_id = result["job_id"].as_str().unwrap();

        let first = read_job_output(&ctx, job_id, 1).await.unwrap();
        assert!(first["output"].as_str().unwrap().contains("first"));
        assert_eq!(first["running"], true);

        let second = read_job_output(&ctx, job_id, 4).await.unwrap();
        assert!(second["output"].as_str().unwrap().contains("second"));
        assert_eq!(second["running"], false);
        assert_eq!(second["exit_code"], 0);
    }

    #[tokio::test]
    async fn completed_exec_preserves_stdout_and_stderr() {
        let ctx = context("exec-jobs-streams");
        let result = execute_exec_job(
            ctx.clone(),
            "printf out; printf err >&2",
            ctx.workspace.clone(),
            SandboxPolicy::FullAccess,
            Vec::new(),
            5,
            Arc::new(NativeSandboxProvider),
        )
        .await
        .unwrap();
        assert_eq!(result["stdout"], "out");
        assert_eq!(result["stderr"], "err");
        assert_eq!(result["exit_code"], 0);
    }

    #[tokio::test]
    async fn job_kill_and_owner_scope_are_enforced() {
        let owner = context("exec-jobs-owner");
        let result = execute_exec_job(
            owner.clone(),
            "sleep 30",
            owner.workspace.clone(),
            SandboxPolicy::FullAccess,
            Vec::new(),
            0,
            Arc::new(NativeSandboxProvider),
        )
        .await
        .unwrap();
        let job_id = result["job_id"].as_str().unwrap();
        let other = context("exec-jobs-other");
        assert!(matches!(
            read_job_output(&other, job_id, 0).await,
            Err(Error::PermissionDenied(_))
        ));
        assert_eq!(kill_job(&owner, job_id).await.unwrap()["killed"], true);
    }

    #[test]
    fn completion_is_queued_for_the_next_active_pre_step() {
        let guard = JobTurnGuard::new("exec-jobs-notify", "exec-jobs-notify");
        let job_id = "exec-jobs-notify-test";
        reserve_job(job_id, "agent", "exec-jobs-notify").unwrap();
        finish_job(job_id, Some(0), "finished output");
        let notifications = take_finished_job_notifications("exec-jobs-notify");
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0].contains("[job exec-jobs-notify-test finished: exit 0]"));
        assert!(notifications[0].contains("finished output"));
        remove_job(job_id);
        drop(guard);
    }
}
