use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use blockcell_core::{Error, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone)]
pub struct GhostLedger {
    pub(crate) inner: Arc<Mutex<Connection>>,
    #[allow(dead_code)]
    db_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GhostEpisodeSource {
    pub source_type: String,
    pub source_key: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NewGhostEpisode {
    pub boundary_kind: String,
    pub subject_key: Option<String>,
    pub status: String,
    pub summary: String,
    #[serde(default)]
    pub metadata: Value,
    #[serde(default)]
    pub sources: Vec<GhostEpisodeSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GhostEpisodeRecord {
    pub id: String,
    pub boundary_kind: String,
    pub subject_key: Option<String>,
    pub status: String,
    pub summary: String,
    pub metadata: Value,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NewGhostReviewRun {
    pub episode_id: String,
    pub reviewer: String,
    pub status: String,
    #[serde(default)]
    pub result: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GhostReviewRunRecord {
    pub id: String,
    pub episode_id: String,
    pub reviewer: String,
    pub status: String,
    pub result: Value,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GhostDeadLetterRecord {
    pub id: String,
    pub stage: String,
    pub ref_id: Option<String>,
    pub error_message: String,
    pub payload: Value,
    pub created_at: String,
}

impl GhostLedger {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Storage(format!("Failed to create ghost ledger dir: {}", e)))?;
        }

        let conn = Connection::open(db_path).map_err(map_sqlite_error)?;
        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA foreign_keys=ON;
            ",
        )
        .map_err(map_sqlite_error)?;

        let ledger = Self {
            inner: Arc::new(Mutex::new(conn)),
            db_path: db_path.to_path_buf(),
        };
        ledger.init_schema()?;
        Ok(ledger)
    }

    pub fn insert_episode(&self, episode: NewGhostEpisode) -> Result<String> {
        let episode_id = Uuid::new_v4().to_string();
        let now = now_rfc3339();
        let metadata_json = encode_json(&episode.metadata)?;
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction().map_err(map_sqlite_error)?;

        tx.execute(
            "
            INSERT INTO episodes (
                id, boundary_kind, subject_key, status, summary, metadata_json, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ",
            params![
                episode_id,
                episode.boundary_kind,
                episode.subject_key,
                episode.status,
                episode.summary,
                metadata_json,
                now,
                now,
            ],
        )
        .map_err(map_sqlite_error)?;

        for source in episode.sources {
            tx.execute(
                "
                INSERT INTO episode_sources (
                    episode_id, source_type, source_key, role, created_at
                ) VALUES (?1, ?2, ?3, ?4, ?5)
                ",
                params![
                    episode_id,
                    source.source_type,
                    source.source_key,
                    source.role,
                    now,
                ],
            )
            .map_err(map_sqlite_error)?;
        }

        tx.commit().map_err(map_sqlite_error)?;
        Ok(episode_id)
    }

    pub fn get_episode(&self, episode_id: &str) -> Result<Option<GhostEpisodeRecord>> {
        let conn = self.lock_conn()?;
        let row = conn
            .query_row(
                "
                SELECT id, boundary_kind, subject_key, status, summary, metadata_json, created_at, updated_at
                FROM episodes
                WHERE id = ?1
                ",
                params![episode_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .optional()
            .map_err(map_sqlite_error)?;

        row.map(
            |(
                id,
                boundary_kind,
                subject_key,
                status,
                summary,
                metadata_json,
                created_at,
                updated_at,
            )| {
                Ok(GhostEpisodeRecord {
                    id,
                    boundary_kind,
                    subject_key,
                    status,
                    summary,
                    metadata: decode_json(&metadata_json)?,
                    created_at,
                    updated_at,
                })
            },
        )
        .transpose()
    }

    pub fn episode_count(&self) -> Result<usize> {
        let conn = self.lock_conn()?;
        let count = conn
            .query_row("SELECT COUNT(*) FROM episodes", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(map_sqlite_error)?;
        Ok(count as usize)
    }

    pub fn episode_count_by_boundary_kind(&self, boundary_kind: &str) -> Result<usize> {
        let conn = self.lock_conn()?;
        let count = conn
            .query_row(
                "SELECT COUNT(*) FROM episodes WHERE boundary_kind = ?1",
                params![boundary_kind],
                |row| row.get::<_, i64>(0),
            )
            .map_err(map_sqlite_error)?;
        Ok(count as usize)
    }

    pub fn latest_episode_status(&self) -> Result<Option<String>> {
        self.latest_episode_field("status")
    }

    pub fn latest_boundary_kind(&self) -> Result<Option<String>> {
        self.latest_episode_field("boundary_kind")
    }

    pub fn latest_episode_status_by_boundary_kind(
        &self,
        boundary_kind: &str,
    ) -> Result<Option<String>> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "
            SELECT status
            FROM episodes
            WHERE boundary_kind = ?1
            ORDER BY created_at DESC, id DESC
            LIMIT 1
            ",
            params![boundary_kind],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_sqlite_error)
    }

    pub fn latest_episode_by_boundary_kind(
        &self,
        boundary_kind: &str,
    ) -> Result<Option<GhostEpisodeRecord>> {
        let conn = self.lock_conn()?;
        let row = conn
            .query_row(
                "
                SELECT id, boundary_kind, subject_key, status, summary, metadata_json, created_at, updated_at
                FROM episodes
                WHERE boundary_kind = ?1
                ORDER BY created_at DESC, id DESC
                LIMIT 1
                ",
                params![boundary_kind],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .optional()
            .map_err(map_sqlite_error)?;

        row.map(
            |(
                id,
                boundary_kind,
                subject_key,
                status,
                summary,
                metadata_json,
                created_at,
                updated_at,
            )| {
                Ok(GhostEpisodeRecord {
                    id,
                    boundary_kind,
                    subject_key,
                    status,
                    summary,
                    metadata: decode_json(&metadata_json)?,
                    created_at,
                    updated_at,
                })
            },
        )
        .transpose()
    }

    pub fn list_episode_sources(&self, episode_id: &str) -> Result<Vec<GhostEpisodeSource>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn
            .prepare(
                "
                SELECT source_type, source_key, role
                FROM episode_sources
                WHERE episode_id = ?1
                ORDER BY id ASC
                ",
            )
            .map_err(map_sqlite_error)?;
        let rows = stmt
            .query_map(params![episode_id], |row| {
                Ok(GhostEpisodeSource {
                    source_type: row.get(0)?,
                    source_key: row.get(1)?,
                    role: row.get(2)?,
                })
            })
            .map_err(map_sqlite_error)?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)
    }

    pub fn update_episode_status(&self, episode_id: &str, status: &str) -> Result<()> {
        let now = now_rfc3339();
        let conn = self.lock_conn()?;
        conn.execute(
            "
            UPDATE episodes
            SET status = ?2, updated_at = ?3
            WHERE id = ?1
            ",
            params![episode_id, status, now],
        )
        .map_err(map_sqlite_error)?;
        Ok(())
    }

    pub fn claim_reviewable_episodes(&self, limit: usize) -> Result<Vec<GhostEpisodeRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let now = now_rfc3339();
        let mut conn = self.lock_conn()?;
        let tx = conn.transaction().map_err(map_sqlite_error)?;
        let rows = {
            let mut stmt = tx
                .prepare(
                    "
                    SELECT id, boundary_kind, subject_key, status, summary, metadata_json, created_at, updated_at
                    FROM episodes
                    WHERE status IN ('pending_review', 'review_failed')
                    ORDER BY created_at ASC, id ASC
                    LIMIT ?1
                    ",
                )
                .map_err(map_sqlite_error)?;
            let rows = stmt
                .query_map(params![limit as i64], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                })
                .map_err(map_sqlite_error)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(map_sqlite_error)?
        };

        let mut claimed = Vec::new();
        for (
            id,
            boundary_kind,
            subject_key,
            _status,
            summary,
            metadata_json,
            created_at,
            _updated_at,
        ) in rows
        {
            let affected = tx
                .execute(
                    "
                UPDATE episodes
                SET status = 'reviewing', updated_at = ?2
                WHERE id = ?1 AND status IN ('pending_review', 'review_failed')
                ",
                    params![id, now],
                )
                .map_err(map_sqlite_error)?;
            // Only add to claimed if the UPDATE actually affected a row.
            // Without this check, concurrent processes could both SELECT the same
            // episode but only one UPDATE succeeds — both would incorrectly add it
            // to their claimed vector.
            if affected == 0 {
                tracing::debug!(
                    episode_id = %id,
                    "Episode already claimed by another process, skipping"
                );
                continue;
            }
            claimed.push(GhostEpisodeRecord {
                id,
                boundary_kind,
                subject_key,
                status: "reviewing".to_string(), // 返回事务提交后的实际状态
                summary,
                metadata: decode_json(&metadata_json)?,
                created_at,
                updated_at: now.clone(), // 更新为当前时间
            });
        }

        tx.commit().map_err(map_sqlite_error)?;
        Ok(claimed)
    }

    pub fn insert_review_run(&self, run: NewGhostReviewRun) -> Result<String> {
        let run_id = Uuid::new_v4().to_string();
        let now = now_rfc3339();
        let result_json = encode_json(&run.result)?;
        let conn = self.lock_conn()?;

        conn.execute(
            "
            INSERT INTO review_runs (
                id, episode_id, reviewer, status, result_json, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            params![
                run_id,
                run.episode_id,
                run.reviewer,
                run.status,
                result_json,
                now,
                now,
            ],
        )
        .map_err(map_sqlite_error)?;

        Ok(run_id)
    }

    pub fn get_review_run(&self, run_id: &str) -> Result<Option<GhostReviewRunRecord>> {
        let conn = self.lock_conn()?;
        let row = conn
            .query_row(
                "
                SELECT id, episode_id, reviewer, status, result_json, created_at, updated_at
                FROM review_runs
                WHERE id = ?1
                ",
                params![run_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(map_sqlite_error)?;

        row.map(
            |(id, episode_id, reviewer, status, result_json, created_at, updated_at)| {
                Ok(GhostReviewRunRecord {
                    id,
                    episode_id,
                    reviewer,
                    status,
                    result: decode_json(&result_json)?,
                    created_at,
                    updated_at,
                })
            },
        )
        .transpose()
    }

    pub fn review_run_count(&self) -> Result<usize> {
        let conn = self.lock_conn()?;
        let count = conn
            .query_row("SELECT COUNT(*) FROM review_runs", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(map_sqlite_error)?;
        Ok(count as usize)
    }

    pub fn latest_review_run_status(&self) -> Result<Option<String>> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "
            SELECT status
            FROM review_runs
            ORDER BY created_at DESC, id DESC
            LIMIT 1
            ",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_sqlite_error)
    }

    pub fn save_checkpoint(
        &self,
        episode_id: &str,
        checkpoint_key: &str,
        payload: Value,
    ) -> Result<()> {
        let now = now_rfc3339();
        let payload_json = encode_json(&payload)?;
        let conn = self.lock_conn()?;
        conn.execute(
            "
            INSERT INTO review_checkpoints (
                episode_id, checkpoint_key, payload_json, updated_at
            ) VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(episode_id, checkpoint_key)
            DO UPDATE SET payload_json = excluded.payload_json, updated_at = excluded.updated_at
            ",
            params![episode_id, checkpoint_key, payload_json, now],
        )
        .map_err(map_sqlite_error)?;
        Ok(())
    }

    pub fn load_checkpoint(&self, episode_id: &str, checkpoint_key: &str) -> Result<Option<Value>> {
        let conn = self.lock_conn()?;
        let payload_json = conn
            .query_row(
                "
                SELECT payload_json
                FROM review_checkpoints
                WHERE episode_id = ?1 AND checkpoint_key = ?2
                ",
                params![episode_id, checkpoint_key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(map_sqlite_error)?;

        payload_json.map(|value| decode_json(&value)).transpose()
    }

    pub fn insert_dead_letter(
        &self,
        stage: &str,
        ref_id: Option<&str>,
        payload: Value,
        error_message: &str,
    ) -> Result<String> {
        let dead_letter_id = Uuid::new_v4().to_string();
        let now = now_rfc3339();
        let payload_json = encode_json(&payload)?;
        let conn = self.lock_conn()?;
        conn.execute(
            "
            INSERT INTO dead_letters (
                id, stage, ref_id, error_message, payload_json, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ",
            params![
                dead_letter_id,
                stage,
                ref_id,
                error_message,
                payload_json,
                now,
            ],
        )
        .map_err(map_sqlite_error)?;
        Ok(dead_letter_id)
    }

    pub fn get_dead_letter(&self, dead_letter_id: &str) -> Result<Option<GhostDeadLetterRecord>> {
        let conn = self.lock_conn()?;
        let row = conn
            .query_row(
                "
                SELECT id, stage, ref_id, error_message, payload_json, created_at
                FROM dead_letters
                WHERE id = ?1
                ",
                params![dead_letter_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(map_sqlite_error)?;

        row.map(
            |(id, stage, ref_id, error_message, payload_json, created_at)| {
                Ok(GhostDeadLetterRecord {
                    id,
                    stage,
                    ref_id,
                    error_message,
                    payload: decode_json(&payload_json)?,
                    created_at,
                })
            },
        )
        .transpose()
    }

    pub fn dead_letter_count(&self) -> Result<usize> {
        let conn = self.lock_conn()?;
        let count = conn
            .query_row("SELECT COUNT(*) FROM dead_letters", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(map_sqlite_error)?;
        Ok(count as usize)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS episodes (
                id TEXT PRIMARY KEY,
                boundary_kind TEXT NOT NULL,
                subject_key TEXT,
                status TEXT NOT NULL,
                summary TEXT NOT NULL,
                metadata_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_ghost_episodes_status ON episodes(status);
            CREATE INDEX IF NOT EXISTS idx_ghost_episodes_boundary ON episodes(boundary_kind);
            CREATE INDEX IF NOT EXISTS idx_ghost_episodes_subject ON episodes(subject_key);

            CREATE TABLE IF NOT EXISTS episode_sources (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                episode_id TEXT NOT NULL,
                source_type TEXT NOT NULL,
                source_key TEXT NOT NULL,
                role TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (episode_id) REFERENCES episodes(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_ghost_episode_sources_episode_id ON episode_sources(episode_id);

            CREATE TABLE IF NOT EXISTS review_runs (
                id TEXT PRIMARY KEY,
                episode_id TEXT NOT NULL,
                reviewer TEXT NOT NULL,
                status TEXT NOT NULL,
                result_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY (episode_id) REFERENCES episodes(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_ghost_review_runs_episode_id ON review_runs(episode_id);
            CREATE INDEX IF NOT EXISTS idx_ghost_review_runs_status ON review_runs(status);

            CREATE TABLE IF NOT EXISTS review_checkpoints (
                episode_id TEXT NOT NULL,
                checkpoint_key TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (episode_id, checkpoint_key),
                FOREIGN KEY (episode_id) REFERENCES episodes(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS dead_letters (
                id TEXT PRIMARY KEY,
                stage TEXT NOT NULL,
                ref_id TEXT,
                error_message TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_ghost_dead_letters_stage ON dead_letters(stage);
            CREATE INDEX IF NOT EXISTS idx_ghost_dead_letters_ref_id ON dead_letters(ref_id);
            ",
        )
        .map_err(map_sqlite_error)?;
        Ok(())
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.inner
            .lock()
            .map_err(|e| Error::Storage(format!("Ghost ledger database lock error: {}", e)))
    }

    fn latest_episode_field(&self, field: &str) -> Result<Option<String>> {
        // Whitelist allowed field names to prevent SQL injection
        let column = match field {
            "status" => "status",
            "boundary_kind" => "boundary_kind",
            _ => {
                return Err(Error::Storage(format!(
                    "Invalid field name for latest_episode_field: {}",
                    field
                )))
            }
        };
        let conn = self.lock_conn()?;
        let sql = format!(
            "SELECT {} FROM episodes ORDER BY created_at DESC, id DESC LIMIT 1",
            column
        );
        conn.query_row(&sql, [], |row| row.get::<_, String>(0))
            .optional()
            .map_err(map_sqlite_error)
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn encode_json(value: &Value) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| Error::Storage(format!("Ghost ledger json encode error: {}", e)))
}

fn decode_json(value: &str) -> Result<Value> {
    serde_json::from_str(value)
        .map_err(|e| Error::Storage(format!("Ghost ledger json decode error: {}", e)))
}

fn map_sqlite_error(error: rusqlite::Error) -> Error {
    Error::Storage(format!("Ghost ledger sqlite error: {}", error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn test_db() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ghost_ledger.sqlite");
        (dir, db)
    }

    fn sample_episode() -> NewGhostEpisode {
        NewGhostEpisode {
            boundary_kind: "turn_end".to_string(),
            subject_key: Some("user:alice".to_string()),
            status: "pending_review".to_string(),
            summary: "User corrected the preferred deploy flow".to_string(),
            metadata: json!({
                "channel": "cli",
                "session_id": "sess-1",
            }),
            sources: vec![
                GhostEpisodeSource {
                    source_type: "session".to_string(),
                    source_key: "sess-1".to_string(),
                    role: "primary".to_string(),
                },
                GhostEpisodeSource {
                    source_type: "message".to_string(),
                    source_key: "msg-2".to_string(),
                    role: "evidence".to_string(),
                },
            ],
        }
    }

    #[test]
    fn ghost_ledger_schema_creation_and_episode_status_update_work() {
        let (_dir, db) = test_db();
        let ledger = GhostLedger::open(&db).unwrap();

        let episode_id = ledger.insert_episode(sample_episode()).unwrap();
        let episode = ledger.get_episode(&episode_id).unwrap().unwrap();
        assert_eq!(episode.status, "pending_review");
        assert_eq!(episode.boundary_kind, "turn_end");
        assert_eq!(episode.subject_key.as_deref(), Some("user:alice"));

        let sources = ledger.list_episode_sources(&episode_id).unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].source_type, "session");

        ledger
            .update_episode_status(&episode_id, "reviewed")
            .unwrap();
        let updated = ledger.get_episode(&episode_id).unwrap().unwrap();
        assert_eq!(updated.status, "reviewed");
    }

    #[test]
    fn ghost_ledger_review_checkpoint_round_trip_works() {
        let (_dir, db) = test_db();
        let ledger = GhostLedger::open(&db).unwrap();
        let episode_id = ledger.insert_episode(sample_episode()).unwrap();

        let run_id = ledger
            .insert_review_run(NewGhostReviewRun {
                episode_id: episode_id.clone(),
                reviewer: "ghost-review".to_string(),
                status: "completed".to_string(),
                result: json!({
                    "facts": 1,
                    "methods": 1,
                }),
            })
            .unwrap();
        let run = ledger.get_review_run(&run_id).unwrap().unwrap();
        assert_eq!(run.episode_id, episode_id);
        assert_eq!(run.status, "completed");

        ledger
            .save_checkpoint(&episode_id, "review_json", json!({"ok": true}))
            .unwrap();
        let checkpoint = ledger
            .load_checkpoint(&episode_id, "review_json")
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint["ok"], true);
    }

    #[test]
    fn ghost_ledger_dead_letters_are_persisted() {
        let (_dir, db) = test_db();
        let ledger = GhostLedger::open(&db).unwrap();

        let dead_letter_id = ledger
            .insert_dead_letter(
                "background_review",
                Some("episode-123"),
                json!({"target": "memory_bridge"}),
                "bridge unavailable",
            )
            .unwrap();

        let dead_letter = ledger.get_dead_letter(&dead_letter_id).unwrap().unwrap();
        assert_eq!(dead_letter.stage, "background_review");
        assert_eq!(dead_letter.ref_id.as_deref(), Some("episode-123"));
        assert_eq!(dead_letter.error_message, "bridge unavailable");
    }

    #[test]
    fn ghost_ledger_can_count_boundary_kinds() {
        let (_dir, db) = test_db();
        let ledger = GhostLedger::open(&db).unwrap();

        ledger.insert_episode(sample_episode()).unwrap();

        let mut rotated_episode = sample_episode();
        rotated_episode.boundary_kind = "session_rotate".to_string();
        rotated_episode.summary = "Previous session rotated before switching chats".to_string();
        ledger.insert_episode(rotated_episode).unwrap();

        assert_eq!(
            ledger
                .episode_count_by_boundary_kind("session_rotate")
                .unwrap(),
            1
        );
    }

    #[test]
    fn ghost_ledger_claims_reviewable_pending_and_failed_episodes() {
        let (_dir, db) = test_db();
        let ledger = GhostLedger::open(&db).unwrap();

        let pending_id = ledger.insert_episode(sample_episode()).unwrap();
        ledger
            .update_episode_status(&pending_id, "pending_review")
            .unwrap();

        let failed_id = ledger.insert_episode(sample_episode()).unwrap();
        ledger
            .update_episode_status(&failed_id, "review_failed")
            .unwrap();

        let claimed = ledger.claim_reviewable_episodes(10).unwrap();
        let claimed_ids = claimed
            .iter()
            .map(|episode| episode.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(claimed_ids, vec![pending_id.as_str(), failed_id.as_str()]);
        // After the fix, claim_reviewable_episodes returns records with
        // the post-claim status ("reviewing"), not the pre-claim status.
        assert!(claimed
            .iter()
            .all(|episode| { episode.status == "reviewing" }));
        assert_eq!(
            ledger.get_episode(&pending_id).unwrap().unwrap().status,
            "reviewing"
        );
        assert_eq!(
            ledger.get_episode(&failed_id).unwrap().unwrap().status,
            "reviewing"
        );
    }
}
