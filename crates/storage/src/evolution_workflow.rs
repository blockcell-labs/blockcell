//! Core Evolution Durable Workflow Store
//!
//! SQLite-backed workflow store for core evolution. Provides:
//! - Persistent workflow records with claim/lease for crash recovery
//! - Step-level checkpointing for resumable evolution
//! - Event log for audit and control commands
//!
//! Follows the GhostLedger pattern: `Arc<Mutex<Connection>>` + WAL + UUID v4 + RFC 3339.

use blockcell_core::Result;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

// ── Data types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRecord {
    pub id: String,
    pub capability_id: String,
    pub description: String,
    pub provider_kind: String,
    pub status: String,
    pub attempt: i32,
    pub max_attempts: i32,
    pub priority: i32,
    pub created_at: String,
    pub updated_at: String,
    pub lease_owner: Option<String>,
    pub lease_until: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub id: String,
    pub workflow_id: String,
    pub step_name: String,
    pub status: String,
    pub input_json: Option<String>,
    pub output_json: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub retry_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub id: String,
    pub workflow_id: String,
    pub event_type: String,
    pub payload_json: Option<String>,
    pub created_at: String,
}

// ── Store ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct EvolutionWorkflowStore {
    pub(crate) inner: Arc<Mutex<Connection>>,
    #[allow(dead_code)]
    db_path: std::path::PathBuf,
}

impl EvolutionWorkflowStore {
    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        match self.inner.lock() {
            Ok(guard) => Ok(guard),
            Err(poisoned) => {
                // Recover from mutex poisoning: the Connection is still valid,
                // we just need to regain access. into_inner() gives us the
                // Connection regardless of poison state.
                tracing::warn!("evolution workflow mutex was poisoned, recovering");
                Ok(poisoned.into_inner())
            }
        }
    }

    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path).map_err(map_sqlite_error)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
        )
        .map_err(map_sqlite_error)?;
        let store = Self {
            inner: Arc::new(Mutex::new(conn)),
            db_path: db_path.to_path_buf(),
        };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS evo_workflows (
                id TEXT PRIMARY KEY,
                capability_id TEXT NOT NULL,
                description TEXT NOT NULL,
                provider_kind TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'Requested',
                attempt INTEGER NOT NULL DEFAULT 0,
                max_attempts INTEGER NOT NULL DEFAULT 3,
                priority INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                lease_owner TEXT,
                lease_until TEXT,
                last_error TEXT
            );

            CREATE TABLE IF NOT EXISTS evo_workflow_steps (
                id TEXT PRIMARY KEY,
                workflow_id TEXT NOT NULL REFERENCES evo_workflows(id),
                step_name TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'Pending',
                input_json TEXT,
                output_json TEXT,
                error TEXT,
                started_at TEXT,
                finished_at TEXT,
                retry_count INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS evo_workflow_events (
                id TEXT PRIMARY KEY,
                workflow_id TEXT NOT NULL REFERENCES evo_workflows(id),
                event_type TEXT NOT NULL,
                payload_json TEXT,
                created_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_evo_workflows_status
                ON evo_workflows(status);
            CREATE INDEX IF NOT EXISTS idx_evo_workflows_capability
                ON evo_workflows(capability_id);
            CREATE INDEX IF NOT EXISTS idx_evo_workflow_steps_workflow
                ON evo_workflow_steps(workflow_id);
            CREATE INDEX IF NOT EXISTS idx_evo_workflow_events_workflow
                ON evo_workflow_events(workflow_id);",
        )
        .map_err(map_sqlite_error)?;
        Ok(())
    }

    // ── Enqueue (fast write, called from runtime) ───────────────────────

    /// Enqueue a new workflow. Returns the workflow ID.
    pub fn enqueue(
        &self,
        capability_id: &str,
        description: &str,
        provider_kind: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = now_rfc3339();
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO evo_workflows (id, capability_id, description, provider_kind, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'Requested', ?5, ?6)",
            params![id, capability_id, description, provider_kind, now, now],
        )
        .map_err(map_sqlite_error)?;
        Ok(id)
    }

    /// Check if a capability already has an active or blocked workflow.
    pub fn is_active_or_blocked(&self, capability_id: &str) -> Result<bool> {
        let conn = self.lock_conn()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM evo_workflows
                 WHERE capability_id = ?1
                   AND status IN ('Requested', 'Claimed', 'RetryScheduled', 'Generating', 'Compiling', 'Validating', 'Loading', 'Blocked')",
                params![capability_id],
                |row| row.get(0),
            )
            .map_err(map_sqlite_error)?;
        Ok(count > 0)
    }

    // ── Claim / Lease ───────────────────────────────────────────────────

    /// Atomically claim the next available workflow.
    /// Returns None if no workflow is available.
    ///
    /// If `provider_kind` is `Some`, only claims workflows with that provider_kind.
    /// If `exclude_provider_kind` is `Some`, excludes workflows with that provider_kind.
    /// Both filters can be combined to prevent cross-contamination between
    /// core evolution and skill evolution workers.
    pub fn claim_next(
        &self,
        worker_id: &str,
        lease_duration_secs: i64,
        provider_kind: Option<&str>,
        exclude_provider_kind: Option<&str>,
    ) -> Result<Option<WorkflowRecord>> {
        let mut conn = self.lock_conn()?;
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let lease_until = (now + chrono::Duration::seconds(lease_duration_secs)).to_rfc3339();

        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_error)?;

        let workflow_id: Option<String> = tx
            .query_row(
                "SELECT id
                 FROM evo_workflows
                 WHERE status IN ('Requested', 'RetryScheduled')
                   AND attempt < max_attempts
                   AND (lease_until IS NULL OR lease_until < ?1)
                   AND (?2 IS NULL OR provider_kind = ?2)
                   AND (?3 IS NULL OR provider_kind != ?3)
                 ORDER BY priority DESC, created_at ASC
                 LIMIT 1",
                params![now_str, provider_kind, exclude_provider_kind],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sqlite_error)?;

        let Some(workflow_id) = workflow_id else {
            tx.commit().map_err(map_sqlite_error)?;
            return Ok(None);
        };

        let updated = tx
            .execute(
                "UPDATE evo_workflows
                 SET status = 'Claimed', lease_owner = ?1, lease_until = ?2, updated_at = ?3
                 WHERE id = ?4
                   AND status IN ('Requested', 'RetryScheduled')
                   AND attempt < max_attempts
                   AND (lease_until IS NULL OR lease_until < ?3)",
                params![worker_id, lease_until, now_str, workflow_id],
            )
            .map_err(map_sqlite_error)?;

        if updated != 1 {
            tx.commit().map_err(map_sqlite_error)?;
            return Ok(None);
        }

        let mut claimed = tx
            .query_row(
                "SELECT id, capability_id, description, provider_kind, status, attempt, max_attempts,
                        priority, created_at, updated_at, lease_owner, lease_until, last_error
                 FROM evo_workflows
                 WHERE id = ?1",
                params![workflow_id],
                workflow_from_row,
            )
            .map_err(map_sqlite_error)?;

        tx.commit().map_err(map_sqlite_error)?;
        claimed.status = "Claimed".to_string();
        claimed.lease_owner = Some(worker_id.to_string());
        claimed.lease_until = Some(lease_until);
        Ok(Some(claimed))
    }

    /// Renew the lease on a claimed workflow.
    pub fn renew_lease(
        &self,
        workflow_id: &str,
        worker_id: &str,
        lease_duration_secs: i64,
    ) -> Result<bool> {
        let conn = self.lock_conn()?;
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let lease_until = (now + chrono::Duration::seconds(lease_duration_secs)).to_rfc3339();
        let updated = conn
            .execute(
                "UPDATE evo_workflows SET lease_until = ?1, updated_at = ?2
             WHERE id = ?3 AND lease_owner = ?4",
                params![lease_until, now_str, workflow_id, worker_id],
            )
            .map_err(map_sqlite_error)?;
        Ok(updated == 1)
    }

    /// Release the lease on a workflow (after completion or error).
    pub fn release_lease(&self, workflow_id: &str, worker_id: &str) -> Result<bool> {
        let conn = self.lock_conn()?;
        let now_str = now_rfc3339();
        let updated = conn
            .execute(
                "UPDATE evo_workflows SET lease_owner = NULL, lease_until = NULL, updated_at = ?1
             WHERE id = ?2 AND lease_owner = ?3",
                params![now_str, workflow_id, worker_id],
            )
            .map_err(map_sqlite_error)?;
        Ok(updated == 1)
    }

    /// Recover workflows with expired leases (crash recovery).
    ///
    /// Resets Claimed workflows with expired leases back to Requested,
    /// and also recovers RetryScheduled workflows whose backoff has elapsed.
    pub fn recover_expired_leases(&self) -> Result<Vec<WorkflowRecord>> {
        let mut conn = self.lock_conn()?;
        let now_str = now_rfc3339();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite_error)?;

        // 1. Reset expired leases back to Requested
        tx.execute(
            "UPDATE evo_workflows
             SET status = 'Requested', lease_owner = NULL, lease_until = NULL, updated_at = ?1
             WHERE status = 'Claimed'
               AND lease_until IS NOT NULL
               AND lease_until < ?1",
            params![now_str],
        )
        .map_err(map_sqlite_error)?;

        // 2. Recover RetryScheduled workflows whose backoff has elapsed
        tx.execute(
            "UPDATE evo_workflows
             SET lease_owner = NULL, lease_until = NULL, updated_at = ?1
             WHERE status = 'RetryScheduled'
               AND (lease_until IS NULL OR lease_until < ?1)",
            params![now_str],
        )
        .map_err(map_sqlite_error)?;

        // Return recoverable workflows
        let mut stmt = tx
            .prepare(
                "SELECT id, capability_id, description, provider_kind, status, attempt, max_attempts,
                        priority, created_at, updated_at, lease_owner, lease_until, last_error
                 FROM evo_workflows
                 WHERE status IN ('Requested', 'RetryScheduled')
                   AND attempt < max_attempts
                   AND (lease_until IS NULL OR lease_until < ?1)",
            )
            .map_err(map_sqlite_error)?;

        let rows = stmt
            .query_map(params![now_str], |row| {
                Ok(WorkflowRecord {
                    id: row.get(0)?,
                    capability_id: row.get(1)?,
                    description: row.get(2)?,
                    provider_kind: row.get(3)?,
                    status: row.get(4)?,
                    attempt: row.get(5)?,
                    max_attempts: row.get(6)?,
                    priority: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                    lease_owner: row.get(10)?,
                    lease_until: row.get(11)?,
                    last_error: row.get(12)?,
                })
            })
            .map_err(map_sqlite_error)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(map_sqlite_error)?);
        }
        drop(stmt);
        tx.commit().map_err(map_sqlite_error)?;

        Ok(records)
    }

    /// Schedule a retry for a failed workflow with exponential backoff.
    ///
    /// Backoff schedule (after increment): attempt 1→1min, 2→5min, 3→30min, then Blocked.
    /// Sets status to RetryScheduled and updates updated_at to now
    /// (the worker will use updated_at + backoff to determine when to retry).
    pub fn schedule_retry(&self, workflow_id: &str, last_error: Option<&str>) -> Result<()> {
        let mut conn = self.lock_conn()?;
        let now_str = now_rfc3339();
        let tx = conn.transaction().map_err(map_sqlite_error)?;

        // Increment attempt counter first
        tx.execute(
            "UPDATE evo_workflows SET attempt = attempt + 1, updated_at = ?1 WHERE id = ?2",
            params![now_str, workflow_id],
        )
        .map_err(map_sqlite_error)?;

        // Read incremented attempt and max_attempts to determine backoff.
        // After increment: attempt 1→60s, 2→300s, 3→1800s.
        // attempt >= max_attempts → 'Failed'（终态，可被 retry_workflow 重新拉起），
        // 不能停留为 RetryScheduled：claim_next 要求 attempt < max_attempts，
        // 否则该记录永远不可领、也不可恢复，形成死路。
        let (attempt, max_attempts): (i32, i32) = tx
            .query_row(
                "SELECT attempt, max_attempts FROM evo_workflows WHERE id = ?1",
                params![workflow_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(map_sqlite_error)?;

        let (status, backoff_secs): (&str, i64) = if attempt >= max_attempts {
            ("Failed", 0)
        } else {
            match attempt {
                1 => ("RetryScheduled", 60),
                2 => ("RetryScheduled", 300),
                _ => ("RetryScheduled", 1800),
            }
        };

        if status == "Failed" {
            tx.execute(
                "UPDATE evo_workflows SET status = 'Failed', last_error = ?1, updated_at = ?2
                 WHERE id = ?3",
                params![last_error, now_str, workflow_id],
            )
            .map_err(map_sqlite_error)?;
        } else {
            // Set lease_until to now + backoff so the worker won't claim it
            // until the backoff period has elapsed
            let lease_until = (Utc::now() + chrono::Duration::seconds(backoff_secs)).to_rfc3339();
            tx.execute(
                "UPDATE evo_workflows SET status = 'RetryScheduled', last_error = ?1,
                        lease_owner = NULL, lease_until = ?2, updated_at = ?3
                 WHERE id = ?4",
                params![last_error, lease_until, now_str, workflow_id],
            )
            .map_err(map_sqlite_error)?;
        }

        tx.commit().map_err(map_sqlite_error)?;
        Ok(())
    }

    /// Cancel a workflow by ID. Sets status to Cancelled and releases lease.
    pub fn cancel_workflow(&self, workflow_id: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        let now_str = now_rfc3339();
        conn.execute(
            "UPDATE evo_workflows
             SET status = 'Cancelled', lease_owner = NULL, lease_until = NULL, updated_at = ?1
             WHERE id = ?2 AND status NOT IN ('Promoted', 'Failed', 'Cancelled', 'Blocked')",
            params![now_str, workflow_id],
        )
        .map_err(map_sqlite_error)?;
        Ok(())
    }

    /// Retry a failed or blocked workflow. Resets attempt and sets status to Requested.
    pub fn retry_workflow(&self, workflow_id: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        let now_str = now_rfc3339();
        conn.execute(
            "UPDATE evo_workflows
             SET status = 'Requested', attempt = 0, last_error = NULL,
                 lease_owner = NULL, lease_until = NULL, updated_at = ?1
             WHERE id = ?2 AND status IN ('Failed', 'Blocked')",
            params![now_str, workflow_id],
        )
        .map_err(map_sqlite_error)?;
        Ok(())
    }

    // ── Step recording ──────────────────────────────────────────────────

    /// Insert a new step record. Returns the step ID.
    pub fn insert_step(
        &self,
        workflow_id: &str,
        step_name: &str,
        input_json: Option<&str>,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let now = now_rfc3339();
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO evo_workflow_steps (id, workflow_id, step_name, status, input_json, started_at)
             VALUES (?1, ?2, ?3, 'Running', ?4, ?5)",
            params![id, workflow_id, step_name, input_json, now],
        )
        .map_err(map_sqlite_error)?;
        Ok(id)
    }

    /// Mark a step as completed.
    pub fn complete_step(&self, step_id: &str, output_json: Option<&str>) -> Result<()> {
        let conn = self.lock_conn()?;
        let now = now_rfc3339();
        conn.execute(
            "UPDATE evo_workflow_steps SET status = 'Completed', output_json = ?1, finished_at = ?2
             WHERE id = ?3",
            params![output_json, now, step_id],
        )
        .map_err(map_sqlite_error)?;
        Ok(())
    }

    /// Mark a step as completed only if the worker still owns the workflow lease.
    pub fn complete_step_if_owned(
        &self,
        workflow_id: &str,
        step_id: &str,
        worker_id: &str,
        output_json: Option<&str>,
    ) -> Result<bool> {
        let conn = self.lock_conn()?;
        let now = now_rfc3339();
        let updated = conn
            .execute(
                "UPDATE evo_workflow_steps
                 SET status = 'Completed', output_json = ?1, finished_at = ?2
                 WHERE id = ?3
                   AND workflow_id = ?4
                   AND EXISTS (
                       SELECT 1 FROM evo_workflows
                       WHERE id = ?4 AND lease_owner = ?5
                   )",
                params![output_json, now, step_id, workflow_id, worker_id],
            )
            .map_err(map_sqlite_error)?;
        Ok(updated == 1)
    }

    /// Mark a step as failed.
    pub fn fail_step(&self, step_id: &str, error: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        let now = now_rfc3339();
        conn.execute(
            "UPDATE evo_workflow_steps SET status = 'Failed', error = ?1, finished_at = ?2
             WHERE id = ?3",
            params![error, now, step_id],
        )
        .map_err(map_sqlite_error)?;
        Ok(())
    }

    /// Mark a step as failed only if the worker still owns the workflow lease.
    pub fn fail_step_if_owned(
        &self,
        workflow_id: &str,
        step_id: &str,
        worker_id: &str,
        error: &str,
    ) -> Result<bool> {
        let conn = self.lock_conn()?;
        let now = now_rfc3339();
        let updated = conn
            .execute(
                "UPDATE evo_workflow_steps
                 SET status = 'Failed', error = ?1, finished_at = ?2
                 WHERE id = ?3
                   AND workflow_id = ?4
                   AND EXISTS (
                       SELECT 1 FROM evo_workflows
                       WHERE id = ?4 AND lease_owner = ?5
                   )",
                params![error, now, step_id, workflow_id, worker_id],
            )
            .map_err(map_sqlite_error)?;
        Ok(updated == 1)
    }

    /// Get the last completed step for a workflow.
    pub fn get_last_completed_step(&self, workflow_id: &str) -> Result<Option<StepRecord>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, workflow_id, step_name, status, input_json, output_json,
                        error, started_at, finished_at, retry_count
                 FROM evo_workflow_steps
                 WHERE workflow_id = ?1 AND status = 'Completed'
                 ORDER BY finished_at DESC
                 LIMIT 1",
            )
            .map_err(map_sqlite_error)?;

        let record = stmt
            .query_row(params![workflow_id], |row| {
                Ok(StepRecord {
                    id: row.get(0)?,
                    workflow_id: row.get(1)?,
                    step_name: row.get(2)?,
                    status: row.get(3)?,
                    input_json: row.get(4)?,
                    output_json: row.get(5)?,
                    error: row.get(6)?,
                    started_at: row.get(7)?,
                    finished_at: row.get(8)?,
                    retry_count: row.get(9)?,
                })
            })
            .optional()
            .map_err(map_sqlite_error)?;
        Ok(record)
    }

    // ── Status updates ──────────────────────────────────────────────────

    /// Update workflow status.
    pub fn update_workflow_status(
        &self,
        workflow_id: &str,
        status: &str,
        last_error: Option<&str>,
    ) -> Result<()> {
        let conn = self.lock_conn()?;
        let now = now_rfc3339();
        conn.execute(
            "UPDATE evo_workflows SET status = ?1, last_error = ?2, updated_at = ?3
             WHERE id = ?4",
            params![status, last_error, now, workflow_id],
        )
        .map_err(map_sqlite_error)?;
        Ok(())
    }

    /// Update workflow status only if the worker still owns the workflow lease.
    pub fn update_workflow_status_if_owned(
        &self,
        workflow_id: &str,
        worker_id: &str,
        status: &str,
        last_error: Option<&str>,
    ) -> Result<bool> {
        let conn = self.lock_conn()?;
        let now = now_rfc3339();
        let updated = conn
            .execute(
                "UPDATE evo_workflows
                 SET status = ?1, last_error = ?2, updated_at = ?3
                 WHERE id = ?4 AND lease_owner = ?5",
                params![status, last_error, now, workflow_id, worker_id],
            )
            .map_err(map_sqlite_error)?;
        Ok(updated == 1)
    }

    /// Increment the attempt counter.
    pub fn increment_attempt(&self, workflow_id: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        let now = now_rfc3339();
        conn.execute(
            "UPDATE evo_workflows SET attempt = attempt + 1, updated_at = ?1
             WHERE id = ?2",
            params![now, workflow_id],
        )
        .map_err(map_sqlite_error)?;
        Ok(())
    }

    /// Increment attempt and schedule retry only if the worker still owns the workflow lease.
    pub fn schedule_retry_if_owned(
        &self,
        workflow_id: &str,
        worker_id: &str,
        last_error: Option<&str>,
    ) -> Result<bool> {
        let mut conn = self.lock_conn()?;
        let now_str = now_rfc3339();
        let tx = conn.transaction().map_err(map_sqlite_error)?;

        let updated = tx
            .execute(
                "UPDATE evo_workflows
                 SET attempt = attempt + 1, updated_at = ?1
                 WHERE id = ?2 AND lease_owner = ?3",
                params![now_str, workflow_id, worker_id],
            )
            .map_err(map_sqlite_error)?;

        if updated != 1 {
            tx.commit().map_err(map_sqlite_error)?;
            return Ok(false);
        }

        let (attempt, max_attempts): (i32, i32) = tx
            .query_row(
                "SELECT attempt, max_attempts FROM evo_workflows WHERE id = ?1 AND lease_owner = ?2",
                params![workflow_id, worker_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(map_sqlite_error)?;

        // After increment, attempt values are 1,2,3 (pre-increment was 0,1,2).
        // Backoff schedule: pre-increment 0→60s, 1→300s, 2→1800s。
        // attempt >= max_attempts → 'Failed'：RetryScheduled 在 claim_next 的
        // attempt < max_attempts 条件下永远不可领，会形成既不可重试又不可见的死路。
        let (status, backoff_secs): (&str, i64) = if attempt >= max_attempts {
            ("Failed", 0)
        } else {
            match attempt {
                1 => ("RetryScheduled", 60),
                2 => ("RetryScheduled", 300),
                _ => ("RetryScheduled", 1800),
            }
        };

        if status == "Failed" {
            tx.execute(
                "UPDATE evo_workflows
                 SET status = 'Failed', last_error = ?1, lease_owner = NULL,
                     lease_until = NULL, updated_at = ?2
                 WHERE id = ?3 AND lease_owner = ?4",
                params![last_error, now_str, workflow_id, worker_id],
            )
            .map_err(map_sqlite_error)?;
        } else {
            let lease_until = (Utc::now() + chrono::Duration::seconds(backoff_secs)).to_rfc3339();
            tx.execute(
                "UPDATE evo_workflows
                 SET status = 'RetryScheduled', last_error = ?1, lease_owner = NULL,
                     lease_until = ?2, updated_at = ?3
                 WHERE id = ?4 AND lease_owner = ?5",
                params![last_error, lease_until, now_str, workflow_id, worker_id],
            )
            .map_err(map_sqlite_error)?;
        }

        tx.commit().map_err(map_sqlite_error)?;
        Ok(true)
    }

    /// 将 workflow 延迟（defer）而不增加 attempt。
    ///
    /// 用于 StillRunning / WaitingForProvider 等非失败场景：
    /// 设置状态为 RetryScheduled，lease_until 为 now + backoff_secs，
    /// 清空 lease_owner，不增加 attempt。
    /// claim_next 只在 lease_until < now 时领取，所以 backoff 期间不会被抢回。
    pub fn defer_workflow_if_owned(
        &self,
        workflow_id: &str,
        worker_id: &str,
        backoff_secs: i64,
        note: Option<&str>,
    ) -> Result<bool> {
        let conn = self.lock_conn()?;
        let now_str = now_rfc3339();
        let lease_until = (Utc::now() + chrono::Duration::seconds(backoff_secs)).to_rfc3339();
        let updated = conn
            .execute(
                "UPDATE evo_workflows
                 SET status = 'RetryScheduled', last_error = ?1, lease_owner = NULL,
                     lease_until = ?2, updated_at = ?3
                 WHERE id = ?4 AND lease_owner = ?5",
                params![note, lease_until, now_str, workflow_id, worker_id],
            )
            .map_err(map_sqlite_error)?;
        Ok(updated == 1)
    }

    // ── Events ──────────────────────────────────────────────────────────

    /// Append an event to the workflow event log.
    pub fn append_event(
        &self,
        workflow_id: &str,
        event_type: &str,
        payload_json: Option<&str>,
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let now = now_rfc3339();
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO evo_workflow_events (id, workflow_id, event_type, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, workflow_id, event_type, payload_json, now],
        )
        .map_err(map_sqlite_error)?;
        Ok(())
    }

    /// Read pending control events for a workflow.
    pub fn read_pending_events(&self, workflow_id: &str) -> Result<Vec<EventRecord>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, workflow_id, event_type, payload_json, created_at
                 FROM evo_workflow_events
                 WHERE workflow_id = ?1 AND event_type IN ('cancel', 'retry', 'unblock', 'approve')
                 ORDER BY created_at ASC",
            )
            .map_err(map_sqlite_error)?;

        let records = stmt
            .query_map(params![workflow_id], |row| {
                Ok(EventRecord {
                    id: row.get(0)?,
                    workflow_id: row.get(1)?,
                    event_type: row.get(2)?,
                    payload_json: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .map_err(map_sqlite_error)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(records)
    }

    // ── Queries ─────────────────────────────────────────────────────────

    /// List workflows, optionally filtered by status.
    pub fn list_workflows(&self, status_filter: Option<&str>) -> Result<Vec<WorkflowRecord>> {
        let conn = self.lock_conn()?;
        let sql = match status_filter {
            Some(_) => "SELECT id, capability_id, description, provider_kind, status, attempt, max_attempts,
                               priority, created_at, updated_at, lease_owner, lease_until, last_error
                        FROM evo_workflows WHERE status = ?1 ORDER BY created_at DESC",
            None => "SELECT id, capability_id, description, provider_kind, status, attempt, max_attempts,
                            priority, created_at, updated_at, lease_owner, lease_until, last_error
                     FROM evo_workflows ORDER BY created_at DESC",
        };

        let mut stmt = conn.prepare(sql).map_err(map_sqlite_error)?;

        let map_row = |row: &rusqlite::Row| -> rusqlite::Result<WorkflowRecord> {
            Ok(WorkflowRecord {
                id: row.get(0)?,
                capability_id: row.get(1)?,
                description: row.get(2)?,
                provider_kind: row.get(3)?,
                status: row.get(4)?,
                attempt: row.get(5)?,
                max_attempts: row.get(6)?,
                priority: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                lease_owner: row.get(10)?,
                lease_until: row.get(11)?,
                last_error: row.get(12)?,
            })
        };

        let records: Vec<WorkflowRecord> = match status_filter {
            Some(s) => stmt
                .query_map(params![s], map_row)
                .map_err(map_sqlite_error)?
                .filter_map(|r| r.ok())
                .collect(),
            None => stmt
                .query_map([], map_row)
                .map_err(map_sqlite_error)?
                .filter_map(|r| r.ok())
                .collect(),
        };
        Ok(records)
    }

    /// Get a specific workflow by ID.
    pub fn get_workflow(&self, workflow_id: &str) -> Result<Option<WorkflowRecord>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, capability_id, description, provider_kind, status, attempt, max_attempts,
                        priority, created_at, updated_at, lease_owner, lease_until, last_error
                 FROM evo_workflows WHERE id = ?1",
            )
            .map_err(map_sqlite_error)?;

        let record = stmt
            .query_row(params![workflow_id], |row| {
                Ok(WorkflowRecord {
                    id: row.get(0)?,
                    capability_id: row.get(1)?,
                    description: row.get(2)?,
                    provider_kind: row.get(3)?,
                    status: row.get(4)?,
                    attempt: row.get(5)?,
                    max_attempts: row.get(6)?,
                    priority: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                    lease_owner: row.get(10)?,
                    lease_until: row.get(11)?,
                    last_error: row.get(12)?,
                })
            })
            .optional()
            .map_err(map_sqlite_error)?;
        Ok(record)
    }

    /// Unblock a capability (set Blocked workflows back to Requested).
    pub fn unblock_capability(&self, capability_id: &str) -> Result<u32> {
        let conn = self.lock_conn()?;
        let now_str = now_rfc3339();
        let count = conn
            .execute(
                "UPDATE evo_workflows SET status = 'Requested', last_error = NULL, updated_at = ?1
                 WHERE capability_id = ?2 AND status = 'Blocked'",
                params![now_str, capability_id],
            )
            .map_err(map_sqlite_error)?;
        Ok(count as u32)
    }

    /// Get steps for a workflow.
    pub fn get_steps(&self, workflow_id: &str) -> Result<Vec<StepRecord>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, workflow_id, step_name, status, input_json, output_json,
                        error, started_at, finished_at, retry_count
                 FROM evo_workflow_steps
                 WHERE workflow_id = ?1
                 ORDER BY started_at ASC",
            )
            .map_err(map_sqlite_error)?;

        let records = stmt
            .query_map(params![workflow_id], |row| {
                Ok(StepRecord {
                    id: row.get(0)?,
                    workflow_id: row.get(1)?,
                    step_name: row.get(2)?,
                    status: row.get(3)?,
                    input_json: row.get(4)?,
                    output_json: row.get(5)?,
                    error: row.get(6)?,
                    started_at: row.get(7)?,
                    finished_at: row.get(8)?,
                    retry_count: row.get(9)?,
                })
            })
            .map_err(map_sqlite_error)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(records)
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn workflow_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowRecord> {
    Ok(WorkflowRecord {
        id: row.get(0)?,
        capability_id: row.get(1)?,
        description: row.get(2)?,
        provider_kind: row.get(3)?,
        status: row.get(4)?,
        attempt: row.get(5)?,
        max_attempts: row.get(6)?,
        priority: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        lease_owner: row.get(10)?,
        lease_until: row.get(11)?,
        last_error: row.get(12)?,
    })
}

fn map_sqlite_error(e: rusqlite::Error) -> blockcell_core::Error {
    blockcell_core::Error::Storage(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open_temp_store() -> (TempDir, EvolutionWorkflowStore) {
        let dir = TempDir::new().expect("temp dir");
        let db_path = dir.path().join("evolution.db");
        let store = EvolutionWorkflowStore::open(&db_path).expect("open store");
        (dir, store)
    }

    #[test]
    fn claim_next_claims_workflow_once_across_store_handles() {
        let (dir, first_store) = open_temp_store();
        let db_path = dir.path().join("evolution.db");
        let second_store = EvolutionWorkflowStore::open(&db_path).expect("open second store");

        let workflow_id = first_store
            .enqueue("cap.test", "test capability", "process")
            .expect("enqueue");

        let claimed = first_store
            .claim_next("worker-a", 60, None, None)
            .expect("claim")
            .expect("claimed workflow");
        assert_eq!(claimed.id, workflow_id);
        assert_eq!(claimed.lease_owner.as_deref(), Some("worker-a"));

        let second_claim = second_store
            .claim_next("worker-b", 60, None, None)
            .expect("claim");
        assert!(second_claim.is_none());

        let stored = first_store
            .get_workflow(&workflow_id)
            .expect("get workflow")
            .expect("workflow exists");
        assert_eq!(stored.lease_owner.as_deref(), Some("worker-a"));
    }

    #[test]
    fn owned_step_updates_reject_stale_workers() {
        let (_dir, store) = open_temp_store();
        let workflow_id = store
            .enqueue("cap.test", "test capability", "process")
            .expect("enqueue");
        let claimed = store
            .claim_next("worker-a", 60, None, None)
            .expect("claim")
            .expect("claimed workflow");
        let step_id = store
            .insert_step(&claimed.id, "BuildPrompt", None)
            .expect("insert step");

        let stale_update = store
            .complete_step_if_owned(&workflow_id, &step_id, "worker-b", Some("{}"))
            .expect("complete as stale worker");
        assert!(!stale_update);

        let owner_update = store
            .complete_step_if_owned(&workflow_id, &step_id, "worker-a", Some("{}"))
            .expect("complete as owner");
        assert!(owner_update);

        let steps = store.get_steps(&workflow_id).expect("steps");
        assert_eq!(steps[0].status, "Completed");
    }

    #[test]
    fn completed_non_terminal_step_can_be_requeued_and_reclaimed() {
        let (_dir, store) = open_temp_store();
        let workflow_id = store
            .enqueue("cap.test", "test capability", "process")
            .expect("enqueue");
        let claimed = store
            .claim_next("worker-a", 60, None, None)
            .expect("claim")
            .expect("claimed workflow");
        let step_id = store
            .insert_step(&claimed.id, "BuildPrompt", None)
            .expect("insert step");

        assert!(store
            .complete_step_if_owned(&workflow_id, &step_id, "worker-a", Some("{}"))
            .expect("complete step"));
        assert!(store
            .update_workflow_status_if_owned(&workflow_id, "worker-a", "Requested", None)
            .expect("requeue workflow"));
        assert!(store
            .release_lease(&workflow_id, "worker-a")
            .expect("release lease"));

        let reclaimed = store
            .claim_next("worker-b", 60, None, None)
            .expect("reclaim")
            .expect("workflow should be claimable again");
        assert_eq!(reclaimed.id, workflow_id);
        assert_eq!(reclaimed.lease_owner.as_deref(), Some("worker-b"));
    }

    #[test]
    fn claim_next_excludes_provider_kind() {
        let (_dir, store) = open_temp_store();
        // Enqueue a skill workflow and a core workflow
        let skill_id = store
            .enqueue("cap.skill", "skill evolution", "skill")
            .expect("enqueue skill");
        let core_id = store
            .enqueue("cap.core", "core evolution", "process")
            .expect("enqueue core");

        // Core worker excludes "skill" — should only claim the core workflow
        let claimed = store
            .claim_next("core-worker", 60, None, Some("skill"))
            .expect("claim")
            .expect("claimed workflow");
        assert_eq!(claimed.id, core_id);
        assert_eq!(claimed.provider_kind, "process");

        // Skill worker with provider_kind="skill" — should only claim the skill workflow
        let skill_claimed = store
            .claim_next("skill-worker", 60, Some("skill"), None)
            .expect("claim")
            .expect("claimed workflow");
        assert_eq!(skill_claimed.id, skill_id);
        assert_eq!(skill_claimed.provider_kind, "skill");
    }

    #[test]
    fn workflow_children_require_an_existing_workflow() {
        let (_dir, store) = open_temp_store();

        assert!(store.insert_step("missing", "BuildPrompt", None).is_err());
        assert!(store.append_event("missing", "cancel", None).is_err());
    }

    #[test]
    fn schedule_retry_marks_failed_when_attempts_exhausted() {
        let (_dir, store) = open_temp_store();
        let workflow_id = store
            .enqueue("cap.test", "test capability", "process")
            .expect("enqueue");

        // 默认 max_attempts=3：第 3 次失败后必须进入终态 Failed，
        // 不能停留为永远不可领取的 RetryScheduled（attempt < max_attempts 不成立）。
        for _ in 0..3 {
            store
                .schedule_retry(&workflow_id, Some("boom"))
                .expect("schedule retry");
        }

        let stored = store
            .get_workflow(&workflow_id)
            .expect("get workflow")
            .expect("workflow exists");
        assert_eq!(stored.status, "Failed");
        assert_eq!(stored.attempt, 3);

        // 终态不出现在 claim 集合中
        assert!(store
            .claim_next("worker-a", 60, None, None)
            .expect("claim")
            .is_none());

        // 但可被 retry_workflow 重新拉起（attempt 归零、回到 Requested）
        store.retry_workflow(&workflow_id).expect("retry workflow");
        let stored = store
            .get_workflow(&workflow_id)
            .expect("get workflow")
            .expect("workflow exists");
        assert_eq!(stored.status, "Requested");
        assert_eq!(stored.attempt, 0);
    }

    #[test]
    fn schedule_retry_if_owned_marks_failed_when_attempts_exhausted() {
        let (_dir, store) = open_temp_store();
        let workflow_id = store
            .enqueue("cap.test", "test capability", "process")
            .expect("enqueue");

        for round in 0..3 {
            // 每轮先由 worker 领取，再以持有者身份记录失败
            let claimed = store
                .claim_next("worker-a", 0, None, None)
                .expect("claim")
                .expect("workflow should be claimable");
            assert_eq!(claimed.id, workflow_id);
            assert!(store
                .schedule_retry_if_owned(&workflow_id, "worker-a", Some("boom"))
                .expect("schedule owned retry"), "round {round} should update");

            if round < 2 {
                // 前两次进 RetryScheduled + 退避；测试里直接把 lease_until 拨到过去以便下一轮领取
                let conn = store.lock_conn().expect("conn");
                conn.execute(
                    "UPDATE evo_workflows SET lease_until = '2000-01-01T00:00:00Z' WHERE id = ?1",
                    params![workflow_id],
                )
                .expect("expire backoff for test");
            }
        }

        let stored = store
            .get_workflow(&workflow_id)
            .expect("get workflow")
            .expect("workflow exists");
        assert_eq!(stored.status, "Failed");
        assert_eq!(stored.attempt, 3);
        assert!(stored.lease_owner.is_none());
    }
}
