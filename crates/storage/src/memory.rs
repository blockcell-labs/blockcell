use blockcell_core::Result;
use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

use crate::retriever::HybridMemoryRetriever;
use crate::vector::{VectorMeta, VectorRuntime};

pub use crate::memory_contract::MemoryType;

/// 预编译的 FTS5 特殊字符正则，避免每次调用重新编译
static FTS_SPECIAL_CHARS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"[*"():^{}]"#).expect("FTS special chars regex is valid"));

const VECTOR_SYNC_OP_UPSERT: &str = "upsert";
const VECTOR_SYNC_OP_DELETE: &str = "delete";

/// Scope of a memory item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    ShortTerm,
    LongTerm,
}

impl MemoryScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryScope::ShortTerm => "short_term",
            MemoryScope::LongTerm => "long_term",
        }
    }
}

impl std::str::FromStr for MemoryScope {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "short_term" => Ok(MemoryScope::ShortTerm),
            "long_term" => Ok(MemoryScope::LongTerm),
            _ => Err(format!("Invalid memory scope: {}", s)),
        }
    }
}

/// A memory item stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: String,
    pub scope: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub title: Option<String>,
    pub content: String,
    pub summary: Option<String>,
    pub tags: Vec<String>,
    pub source: String,
    pub channel: Option<String>,
    pub session_key: Option<String>,
    pub importance: f64,
    pub created_at: String,
    pub updated_at: String,
    pub last_accessed_at: Option<String>,
    pub access_count: i64,
    pub expires_at: Option<String>,
    pub deleted_at: Option<String>,
    pub dedup_key: Option<String>,
}

/// Parameters for upserting a memory item.
pub struct UpsertParams {
    pub scope: String,
    pub item_type: String,
    pub title: Option<String>,
    pub content: String,
    pub summary: Option<String>,
    pub tags: Vec<String>,
    pub source: String,
    pub channel: Option<String>,
    pub session_key: Option<String>,
    pub importance: f64,
    pub dedup_key: Option<String>,
    pub expires_at: Option<String>,
}

/// Parameters for querying memory items.
pub struct QueryParams {
    pub query: Option<String>,
    pub scope: Option<String>,
    pub item_type: Option<String>,
    pub tags: Option<Vec<String>>,
    pub time_range_days: Option<i64>,
    pub top_k: usize,
    pub include_deleted: bool,
}

impl Default for QueryParams {
    fn default() -> Self {
        Self {
            query: None,
            scope: None,
            item_type: None,
            tags: None,
            time_range_days: None,
            top_k: 20,
            include_deleted: false,
        }
    }
}

/// A query result with score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryResult {
    pub item: MemoryItem,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VectorSyncRetryResult {
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VectorReindexResult {
    pub indexed: usize,
    pub failed: usize,
}

#[derive(Debug, Clone)]
struct PendingVectorSync {
    id: String,
    operation: String,
}

#[derive(Clone, Default)]
pub struct MemoryStoreOptions {
    pub vector: Option<Arc<VectorRuntime>>,
}

/// SQLite-backed memory store with FTS5 full-text search.
#[derive(Clone)]
pub struct MemoryStore {
    pub(crate) inner: Arc<Mutex<Connection>>,
    #[allow(dead_code)]
    db_path: PathBuf,
    pub(crate) vector: Option<Arc<VectorRuntime>>,
}

impl MemoryStore {
    /// Open (or create) the memory database at the given path.
    pub fn open(db_path: &Path) -> Result<Self> {
        self::MemoryStore::open_with_options(db_path, MemoryStoreOptions::default())
    }

    pub fn open_with_options(db_path: &Path, options: MemoryStoreOptions) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                blockcell_core::Error::Storage(format!("Failed to create db directory: {}", e))
            })?;
        }

        let conn = Connection::open(db_path).map_err(|e| {
            blockcell_core::Error::Storage(format!("Failed to open memory db: {}", e))
        })?;

        // Enable WAL mode for better concurrent read performance
        conn.execute_batch("PRAGMA journal_mode=WAL;").ok();

        let store = Self {
            inner: Arc::new(Mutex::new(conn)),
            db_path: db_path.to_path_buf(),
            vector: options.vector,
        };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS memory_items (
                id TEXT PRIMARY KEY,
                scope TEXT NOT NULL DEFAULT 'short_term',
                type TEXT NOT NULL DEFAULT 'note',
                title TEXT,
                content TEXT NOT NULL,
                summary TEXT,
                tags TEXT NOT NULL DEFAULT '',
                source TEXT NOT NULL DEFAULT 'user',
                channel TEXT,
                session_key TEXT,
                importance REAL NOT NULL DEFAULT 0.5,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                last_accessed_at TEXT,
                access_count INTEGER NOT NULL DEFAULT 0,
                expires_at TEXT,
                deleted_at TEXT,
                dedup_key TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_memory_scope ON memory_items(scope);
            CREATE INDEX IF NOT EXISTS idx_memory_type ON memory_items(type);
            CREATE INDEX IF NOT EXISTS idx_memory_deleted ON memory_items(deleted_at);
            CREATE INDEX IF NOT EXISTS idx_memory_expires ON memory_items(expires_at);
            CREATE INDEX IF NOT EXISTS idx_memory_dedup ON memory_items(dedup_key);
            CREATE INDEX IF NOT EXISTS idx_memory_importance ON memory_items(importance);

            CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
                title,
                summary,
                content,
                tags,
                content='memory_items',
                content_rowid='rowid'
            );

            -- Triggers to keep FTS in sync
            CREATE TRIGGER IF NOT EXISTS memory_ai AFTER INSERT ON memory_items BEGIN
                INSERT INTO memory_fts(rowid, title, summary, content, tags)
                VALUES (new.rowid, new.title, new.summary, new.content, new.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS memory_ad AFTER DELETE ON memory_items BEGIN
                INSERT INTO memory_fts(memory_fts, rowid, title, summary, content, tags)
                VALUES ('delete', old.rowid, old.title, old.summary, old.content, old.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS memory_au AFTER UPDATE ON memory_items BEGIN
                INSERT INTO memory_fts(memory_fts, rowid, title, summary, content, tags)
                VALUES ('delete', old.rowid, old.title, old.summary, old.content, old.tags);
                INSERT INTO memory_fts(rowid, title, summary, content, tags)
                VALUES (new.rowid, new.title, new.summary, new.content, new.tags);
            END;

            -- Migration tracking
            CREATE TABLE IF NOT EXISTS memory_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS memory_vector_queue (
                id TEXT PRIMARY KEY,
                operation TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                last_error TEXT,
                updated_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_memory_vector_queue_operation
            ON memory_vector_queue(operation);
            ",
        )
        .map_err(|e| {
            blockcell_core::Error::Storage(format!("Failed to init memory schema: {}", e))
        })?;

        debug!("Memory store schema initialized");
        Ok(())
    }

    /// Upsert a memory item. If dedup_key is set and a matching non-deleted item exists,
    /// update it instead of inserting a new one.
    pub fn upsert(&self, params: UpsertParams) -> Result<MemoryItem> {
        let item = {
            let conn = self
                .inner
                .lock()
                .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

            let now = Utc::now().to_rfc3339();
            let tags_str = params.tags.join(",");

            if let Some(ref dk) = params.dedup_key {
                if !dk.is_empty() {
                    let existing_id: Option<String> = conn
                        .query_row(
                            "SELECT id FROM memory_items WHERE dedup_key = ?1 AND deleted_at IS NULL LIMIT 1",
                            params![dk],
                            |row| row.get(0),
                        )
                        .optional()
                        .map_err(|e| {
                            blockcell_core::Error::Storage(format!("Query error: {}", e))
                        })?;

                    if let Some(id) = existing_id {
                        conn.execute(
                            "UPDATE memory_items SET
                                content = ?1, summary = ?2, title = ?3, tags = ?4,
                                importance = ?5, updated_at = ?6, scope = ?7, type = ?8,
                                expires_at = ?9
                             WHERE id = ?10",
                            params![
                                params.content,
                                params.summary,
                                params.title,
                                tags_str,
                                params.importance,
                                now,
                                params.scope,
                                params.item_type,
                                params.expires_at,
                                id
                            ],
                        )
                        .map_err(|e| {
                            blockcell_core::Error::Storage(format!("Update error: {}", e))
                        })?;

                        debug!(id = %id, dedup_key = %dk, "Memory item updated via dedup_key");
                        self.get_by_id_inner(&conn, &id)?
                    } else {
                        let id = uuid::Uuid::new_v4().to_string();
                        conn.execute(
                            "INSERT INTO memory_items (id, scope, type, title, content, summary, tags, source,
                                channel, session_key, importance, created_at, updated_at, expires_at, dedup_key)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                            params![
                                id,
                                params.scope,
                                params.item_type,
                                params.title,
                                params.content,
                                params.summary,
                                tags_str,
                                params.source,
                                params.channel,
                                params.session_key,
                                params.importance,
                                now,
                                now,
                                params.expires_at,
                                params.dedup_key
                            ],
                        )
                        .map_err(|e| {
                            blockcell_core::Error::Storage(format!("Insert error: {}", e))
                        })?;

                        debug!(id = %id, scope = %params.scope, "Memory item inserted");
                        self.get_by_id_inner(&conn, &id)?
                    }
                } else {
                    let id = uuid::Uuid::new_v4().to_string();
                    conn.execute(
                        "INSERT INTO memory_items (id, scope, type, title, content, summary, tags, source,
                            channel, session_key, importance, created_at, updated_at, expires_at, dedup_key)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                        params![
                            id,
                            params.scope,
                            params.item_type,
                            params.title,
                            params.content,
                            params.summary,
                            tags_str,
                            params.source,
                            params.channel,
                            params.session_key,
                            params.importance,
                            now,
                            now,
                            params.expires_at,
                            params.dedup_key
                        ],
                    )
                    .map_err(|e| blockcell_core::Error::Storage(format!("Insert error: {}", e)))?;

                    debug!(id = %id, scope = %params.scope, "Memory item inserted");
                    self.get_by_id_inner(&conn, &id)?
                }
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                conn.execute(
                    "INSERT INTO memory_items (id, scope, type, title, content, summary, tags, source,
                        channel, session_key, importance, created_at, updated_at, expires_at, dedup_key)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    params![
                        id,
                        params.scope,
                        params.item_type,
                        params.title,
                        params.content,
                        params.summary,
                        tags_str,
                        params.source,
                        params.channel,
                        params.session_key,
                        params.importance,
                        now,
                        now,
                        params.expires_at,
                        params.dedup_key
                    ],
                )
                .map_err(|e| blockcell_core::Error::Storage(format!("Insert error: {}", e)))?;

                debug!(id = %id, scope = %params.scope, "Memory item inserted");
                self.get_by_id_inner(&conn, &id)?
            }
        };

        self.sync_vector_upsert(&item);
        Ok(item)
    }

    /// Query memory items using FTS5 + structured filters + scoring.
    pub fn query(&self, params: &QueryParams) -> Result<Vec<MemoryResult>> {
        let results = HybridMemoryRetriever::new(self).search(params)?;
        self.record_accesses(&results)?;
        Ok(results)
    }

    /// Get a single item by ID.
    pub fn get_by_id(&self, id: &str) -> Result<Option<MemoryItem>> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        match self.get_by_id_inner(&conn, id) {
            Ok(item) => Ok(Some(item)),
            // 仅当记录不存在时返回 None，其他数据库错误向上传播
            Err(blockcell_core::Error::Storage(ref msg)) if msg.contains("QueryReturnedNoRows") => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn get_by_id_inner(&self, conn: &Connection, id: &str) -> Result<MemoryItem> {
        conn.query_row(
            "SELECT * FROM memory_items WHERE id = ?1",
            params![id],
            Self::memory_item_from_row,
        )
        .map_err(|e| blockcell_core::Error::Storage(format!("Get by id error: {}", e)))
    }

    fn memory_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryItem> {
        let tags_str: String = row.get("tags")?;
        Ok(MemoryItem {
            id: row.get("id")?,
            scope: row.get("scope")?,
            item_type: row.get("type")?,
            title: row.get("title")?,
            content: row.get("content")?,
            summary: row.get("summary")?,
            tags: if tags_str.is_empty() {
                vec![]
            } else {
                tags_str.split(',').map(|s| s.trim().to_string()).collect()
            },
            source: row.get("source")?,
            channel: row.get("channel")?,
            session_key: row.get("session_key")?,
            importance: row.get("importance")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            last_accessed_at: row.get("last_accessed_at")?,
            access_count: row.get("access_count")?,
            expires_at: row.get("expires_at")?,
            deleted_at: row.get("deleted_at")?,
            dedup_key: row.get("dedup_key")?,
        })
    }

    pub(crate) fn query_sqlite_raw(&self, params: &QueryParams) -> Result<Vec<MemoryResult>> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

        let has_fts_query = params.query.as_ref().is_some_and(|q| !q.trim().is_empty());
        let mut sql = String::new();
        let mut where_clauses = Vec::new();
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut bind_idx = 1;

        if has_fts_query {
            sql.push_str(
                "SELECT m.*, bm25(memory_fts) AS fts_score
                 FROM memory_items m
                 JOIN memory_fts ON memory_fts.rowid = m.rowid
                 WHERE memory_fts MATCH ?1",
            );
            bind_values.push(Box::new(sanitize_fts_query(
                params.query.as_deref().unwrap_or_default(),
            )));
            bind_idx = 2;
        } else {
            sql.push_str("SELECT m.*, 0.0 AS fts_score FROM memory_items m WHERE 1=1");
        }

        if !params.include_deleted {
            where_clauses.push("m.deleted_at IS NULL".to_string());
        }

        if let Some(ref scope) = params.scope {
            where_clauses.push(format!("m.scope = ?{}", bind_idx));
            bind_values.push(Box::new(scope.clone()));
            bind_idx += 1;
        }

        if let Some(ref item_type) = params.item_type {
            where_clauses.push(format!("m.type = ?{}", bind_idx));
            bind_values.push(Box::new(item_type.clone()));
            bind_idx += 1;
        }

        if let Some(ref tags) = params.tags {
            if !tags.is_empty() {
                let tag_conditions: Vec<String> = tags
                    .iter()
                    .enumerate()
                    .map(|(offset, _)| format!("m.tags LIKE '%' || ?{} || '%'", bind_idx + offset))
                    .collect();
                where_clauses.push(format!("({})", tag_conditions.join(" OR ")));
                for tag in tags {
                    bind_values.push(Box::new(tag.clone()));
                    bind_idx += 1;
                }
            }
        }

        if let Some(days) = params.time_range_days {
            let cutoff = (Utc::now() - chrono::Duration::days(days)).to_rfc3339();
            where_clauses.push(format!("m.created_at >= ?{}", bind_idx));
            bind_values.push(Box::new(cutoff));
            bind_idx += 1;
        }

        if !params.include_deleted {
            where_clauses.push(format!(
                "(m.expires_at IS NULL OR m.expires_at > ?{})",
                bind_idx
            ));
            bind_values.push(Box::new(Utc::now().to_rfc3339()));
        }

        for clause in &where_clauses {
            sql.push_str(&format!(" AND {}", clause));
        }

        sql.push_str(" ORDER BY ");
        if has_fts_query {
            sql.push_str(
                "(-fts_score * 10.0 + m.importance * 5.0 + \
                 CASE WHEN julianday('now') - julianday(m.updated_at) < 1 THEN 3.0 \
                      WHEN julianday('now') - julianday(m.updated_at) < 7 THEN 1.5 \
                      ELSE 0.0 END) DESC",
            );
        } else {
            sql.push_str("m.importance DESC, m.updated_at DESC");
        }
        sql.push_str(&format!(" LIMIT {}", params.top_k));

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| blockcell_core::Error::Storage(format!("Prepare error: {}", e)))?;
        let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|value| value.as_ref()).collect();

        let rows = stmt
            .query_map(bind_refs.as_slice(), |row| {
                let fts_score: f64 = row.get("fts_score")?;
                let item = Self::memory_item_from_row(row)?;
                Ok(MemoryResult {
                    score: -fts_score * 10.0 + item.importance * 5.0,
                    item,
                })
            })
            .map_err(|e| blockcell_core::Error::Storage(format!("Query error: {}", e)))?;

        let mut results = Vec::new();
        for row in rows {
            match row {
                Ok(result) => results.push(result),
                Err(error) => warn!(error = %error, "Error reading memory row"),
            }
        }

        Ok(results)
    }

    pub(crate) fn search_fts_candidates(
        &self,
        fts_query: &str,
        top_k: usize,
    ) -> Result<Vec<(String, f64)>> {
        if top_k == 0 {
            return Ok(Vec::new());
        }

        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        let mut stmt = conn
            .prepare(
                "SELECT m.id, bm25(memory_fts) AS fts_score
                 FROM memory_items m
                 JOIN memory_fts ON memory_fts.rowid = m.rowid
                 WHERE memory_fts MATCH ?1
                 ORDER BY bm25(memory_fts) ASC
                 LIMIT ?2",
            )
            .map_err(|e| blockcell_core::Error::Storage(format!("Prepare error: {}", e)))?;

        let rows = stmt
            .query_map(params![fts_query, top_k as i64], |row| {
                Ok((row.get("id")?, row.get("fts_score")?))
            })
            .map_err(|e| blockcell_core::Error::Storage(format!("FTS query error: {}", e)))?;

        let mut results = Vec::new();
        for row in rows {
            match row {
                Ok(result) => results.push(result),
                Err(error) => warn!(error = %error, "Error reading FTS candidate row"),
            }
        }
        Ok(results)
    }

    pub(crate) fn load_items_by_ids(&self, ids: &[String]) -> Result<Vec<MemoryItem>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        let mut items = Vec::with_capacity(ids.len());
        for id in ids {
            let item = conn
                .query_row(
                    "SELECT * FROM memory_items WHERE id = ?1",
                    params![id],
                    Self::memory_item_from_row,
                )
                .optional()
                .map_err(|e| blockcell_core::Error::Storage(format!("Load by id error: {}", e)))?;
            if let Some(item) = item {
                items.push(item);
            }
        }
        Ok(items)
    }

    pub(crate) fn item_matches_query(&self, item: &MemoryItem, params: &QueryParams) -> bool {
        if !params.include_deleted && item.deleted_at.is_some() {
            return false;
        }

        if let Some(ref scope) = params.scope {
            if item.scope != *scope {
                return false;
            }
        }

        if let Some(ref item_type) = params.item_type {
            if item.item_type != *item_type {
                return false;
            }
        }

        if let Some(ref wanted_tags) = params.tags {
            if !wanted_tags.is_empty()
                && !item.tags.iter().any(|tag| {
                    wanted_tags
                        .iter()
                        .any(|wanted| tag.contains(wanted.as_str()))
                })
            {
                return false;
            }
        }

        if let Some(days) = params.time_range_days {
            let cutoff = Utc::now() - chrono::Duration::days(days);
            let created_at = match DateTime::parse_from_rfc3339(&item.created_at) {
                Ok(value) => value.with_timezone(&Utc),
                Err(_) => return false,
            };
            if created_at < cutoff {
                return false;
            }
        }

        if !params.include_deleted {
            if let Some(ref expires_at) = item.expires_at {
                match DateTime::parse_from_rfc3339(expires_at) {
                    Ok(value) if value.with_timezone(&Utc) <= Utc::now() => return false,
                    Err(_) => return false,
                    _ => {}
                }
            }
        }

        true
    }

    fn record_accesses(&self, results: &[MemoryResult]) -> Result<()> {
        if results.is_empty() {
            return Ok(());
        }

        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        let now = Utc::now().to_rfc3339();
        for result in results {
            conn.execute(
                "UPDATE memory_items SET access_count = access_count + 1, last_accessed_at = ?1 WHERE id = ?2",
                params![now, result.item.id],
            )
            .map_err(|e| blockcell_core::Error::Storage(format!("Access update error: {}", e)))?;
        }

        Ok(())
    }

    fn enqueue_vector_sync(&self, id: &str, operation: &str, error: &str) {
        let now = Utc::now().to_rfc3339();
        let result = self
            .inner
            .lock()
            .map_err(|lock_error| {
                blockcell_core::Error::Storage(format!("Lock error: {}", lock_error))
            })
            .and_then(|conn| {
                conn.execute(
                    "INSERT INTO memory_vector_queue (id, operation, attempts, last_error, updated_at)
                     VALUES (?1, ?2, 1, ?3, ?4)
                     ON CONFLICT(id) DO UPDATE SET
                        operation = excluded.operation,
                        attempts = memory_vector_queue.attempts + 1,
                        last_error = excluded.last_error,
                        updated_at = excluded.updated_at",
                    params![id, operation, error, now],
                )
                .map_err(|db_error| {
                    blockcell_core::Error::Storage(format!(
                        "Failed to enqueue vector sync: {}",
                        db_error
                    ))
                })?;
                Ok(())
            });

        if let Err(queue_error) = result {
            warn!(
                id,
                operation,
                error = %queue_error,
                "Failed to persist pending vector sync operation"
            );
        }
    }

    fn clear_vector_sync(&self, id: &str) {
        let result = self
            .inner
            .lock()
            .map_err(|lock_error| {
                blockcell_core::Error::Storage(format!("Lock error: {}", lock_error))
            })
            .and_then(|conn| {
                conn.execute("DELETE FROM memory_vector_queue WHERE id = ?1", params![id])
                    .map_err(|db_error| {
                        blockcell_core::Error::Storage(format!(
                            "Failed to clear vector sync queue: {}",
                            db_error
                        ))
                    })?;
                Ok(())
            });

        if let Err(queue_error) = result {
            warn!(id, error = %queue_error, "Failed to clear vector sync queue entry");
        }
    }

    fn clear_all_vector_sync(&self) -> Result<()> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        conn.execute("DELETE FROM memory_vector_queue", [])
            .map_err(|e| {
                blockcell_core::Error::Storage(format!("Failed to clear vector queue: {}", e))
            })?;
        Ok(())
    }

    fn pending_vector_counts(&self) -> Result<(i64, i64, i64)> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_vector_queue", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);
        let upserts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_vector_queue WHERE operation = ?1",
                params![VECTOR_SYNC_OP_UPSERT],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let deletes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_vector_queue WHERE operation = ?1",
                params![VECTOR_SYNC_OP_DELETE],
                |row| row.get(0),
            )
            .unwrap_or(0);

        Ok((total, upserts, deletes))
    }

    fn load_pending_vector_sync(&self, limit: usize) -> Result<Vec<PendingVectorSync>> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, operation
                 FROM memory_vector_queue
                 ORDER BY updated_at ASC
                 LIMIT ?1",
            )
            .map_err(|e| {
                blockcell_core::Error::Storage(format!("Prepare pending vector sync error: {}", e))
            })?;

        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(PendingVectorSync {
                    id: row.get("id")?,
                    operation: row.get("operation")?,
                })
            })
            .map_err(|e| {
                blockcell_core::Error::Storage(format!("Query pending vector sync error: {}", e))
            })?;

        let mut pending = Vec::new();
        for row in rows {
            pending.push(row.map_err(|e| {
                blockcell_core::Error::Storage(format!("Pending vector sync row error: {}", e))
            })?);
        }
        Ok(pending)
    }

    fn load_reindexable_items(&self) -> Result<Vec<MemoryItem>> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        let now = Utc::now().to_rfc3339();
        let mut stmt = conn
            .prepare(
                "SELECT *
                 FROM memory_items
                 WHERE deleted_at IS NULL
                   AND (expires_at IS NULL OR expires_at > ?1)
                 ORDER BY updated_at DESC",
            )
            .map_err(|e| {
                blockcell_core::Error::Storage(format!("Prepare reindex query error: {}", e))
            })?;

        let rows = stmt
            .query_map(params![now], Self::memory_item_from_row)
            .map_err(|e| blockcell_core::Error::Storage(format!("Reindex query error: {}", e)))?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row.map_err(|e| {
                blockcell_core::Error::Storage(format!("Reindex row error: {}", e))
            })?);
        }
        Ok(items)
    }

    fn try_vector_upsert(&self, item: &MemoryItem) -> Result<()> {
        let runtime = self.vector.as_ref().ok_or_else(|| {
            blockcell_core::Error::Storage("Vector runtime is not enabled".to_string())
        })?;

        let text = build_embedding_text(item);
        let vector = runtime.embedder.embed_document(&text)?;
        let meta = VectorMeta {
            scope: item.scope.clone(),
            item_type: item.item_type.clone(),
            tags: item.tags.clone(),
        };
        runtime.index.upsert(&item.id, &vector, &meta)
    }

    fn try_vector_delete_ids(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let runtime = self.vector.as_ref().ok_or_else(|| {
            blockcell_core::Error::Storage("Vector runtime is not enabled".to_string())
        })?;
        runtime.index.delete_ids(ids)
    }

    fn sync_vector_upsert(&self, item: &MemoryItem) {
        if self.vector.is_none() {
            return;
        }

        match self.try_vector_upsert(item) {
            Ok(()) => self.clear_vector_sync(&item.id),
            Err(error) => {
                warn!(id = %item.id, error = %error, "Failed to upsert vector index");
                self.enqueue_vector_sync(&item.id, VECTOR_SYNC_OP_UPSERT, &error.to_string());
            }
        }
    }

    fn sync_vector_delete_ids(&self, ids: &[String]) {
        if ids.is_empty() || self.vector.is_none() {
            return;
        }

        match self.try_vector_delete_ids(ids) {
            Ok(()) => {
                for id in ids {
                    self.clear_vector_sync(id);
                }
            }
            Err(error) => {
                warn!(error = %error, count = ids.len(), "Failed to delete vector index entries");
                for id in ids {
                    self.enqueue_vector_sync(id, VECTOR_SYNC_OP_DELETE, &error.to_string());
                }
            }
        }
    }

    fn sync_vector_delete(&self, id: &str) {
        self.sync_vector_delete_ids(&[id.to_string()]);
    }

    /// Soft-delete a memory item.
    pub fn soft_delete(&self, id: &str) -> Result<bool> {
        let deleted = {
            let conn = self
                .inner
                .lock()
                .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
            let now = Utc::now().to_rfc3339();
            let affected = conn
                .execute(
                    "UPDATE memory_items SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
                    params![now, id],
                )
                .map_err(|e| blockcell_core::Error::Storage(format!("Soft delete error: {}", e)))?;
            affected > 0
        };

        if deleted {
            self.sync_vector_delete(id);
        }

        Ok(deleted)
    }

    /// Batch soft-delete by filter criteria.
    pub fn batch_soft_delete(
        &self,
        scope: Option<&str>,
        item_type: Option<&str>,
        tags: Option<&[String]>,
        time_before: Option<&str>,
    ) -> Result<usize> {
        let ids = {
            let conn = self
                .inner
                .lock()
                .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

            let mut sql = "SELECT id FROM memory_items WHERE deleted_at IS NULL".to_string();
            let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            let mut idx = 1;

            if let Some(s) = scope {
                sql.push_str(&format!(" AND scope = ?{}", idx));
                bind_values.push(Box::new(s.to_string()));
                idx += 1;
            }
            if let Some(t) = item_type {
                sql.push_str(&format!(" AND type = ?{}", idx));
                bind_values.push(Box::new(t.to_string()));
                idx += 1;
            }
            if let Some(tag_list) = tags {
                if !tag_list.is_empty() {
                    let mut tag_conditions = Vec::new();
                    for tag in tag_list {
                        tag_conditions.push(format!("tags LIKE '%' || ?{} || '%'", idx));
                        bind_values.push(Box::new(tag.clone()));
                        idx += 1;
                    }
                    sql.push_str(&format!(" AND ({})", tag_conditions.join(" OR ")));
                }
            }
            if let Some(before) = time_before {
                sql.push_str(&format!(" AND created_at < ?{}", idx));
                bind_values.push(Box::new(before.to_string()));
            }

            let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
                bind_values.iter().map(|b| b.as_ref()).collect();
            let mut stmt = conn.prepare(&sql).map_err(|e| {
                blockcell_core::Error::Storage(format!("Batch delete prepare error: {}", e))
            })?;
            let rows = stmt
                .query_map(bind_refs.as_slice(), |row| row.get::<_, String>(0))
                .map_err(|e| {
                    blockcell_core::Error::Storage(format!("Batch delete select error: {}", e))
                })?;

            let mut ids = Vec::new();
            for row in rows {
                ids.push(row.map_err(|e| {
                    blockcell_core::Error::Storage(format!("Batch delete id row error: {}", e))
                })?);
            }

            if ids.is_empty() {
                return Ok(0);
            }

            let now = Utc::now().to_rfc3339();
            let placeholders = (0..ids.len())
                .map(|offset| format!("?{}", offset + 2))
                .collect::<Vec<_>>()
                .join(", ");
            let update_sql = format!(
                "UPDATE memory_items SET deleted_at = ?1 WHERE id IN ({})",
                placeholders
            );

            let mut update_values: Vec<Box<dyn rusqlite::types::ToSql>> =
                Vec::with_capacity(ids.len() + 1);
            update_values.push(Box::new(now));
            for id in &ids {
                update_values.push(Box::new(id.clone()));
            }
            let update_refs: Vec<&dyn rusqlite::types::ToSql> =
                update_values.iter().map(|value| value.as_ref()).collect();

            conn.execute(&update_sql, update_refs.as_slice())
                .map_err(|e| {
                    blockcell_core::Error::Storage(format!("Batch delete update error: {}", e))
                })?;

            ids
        };

        self.sync_vector_delete_ids(&ids);
        info!(count = ids.len(), "Batch soft-deleted memory items");
        Ok(ids.len())
    }

    /// Restore a soft-deleted item.
    pub fn restore(&self, id: &str) -> Result<bool> {
        let restored_item = {
            let conn = self
                .inner
                .lock()
                .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
            let affected = conn
                .execute(
                    "UPDATE memory_items SET deleted_at = NULL WHERE id = ?1 AND deleted_at IS NOT NULL",
                    params![id],
                )
                .map_err(|e| blockcell_core::Error::Storage(format!("Restore error: {}", e)))?;

            if affected == 0 {
                None
            } else {
                Some(self.get_by_id_inner(&conn, id)?)
            }
        };

        if let Some(item) = restored_item {
            self.sync_vector_upsert(&item);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Clean up expired items (set deleted_at) and hard-delete items that have been
    /// soft-deleted for more than `recycle_days` days.
    pub fn maintenance(&self, recycle_days: i64) -> Result<(usize, usize)> {
        let (expired_ids, purged_ids) = {
            let conn = self
                .inner
                .lock()
                .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

            let now = Utc::now().to_rfc3339();
            let cutoff = (Utc::now() - chrono::Duration::days(recycle_days)).to_rfc3339();

            let expired_ids = {
                let mut stmt = conn
                    .prepare(
                        "SELECT id FROM memory_items
                         WHERE expires_at IS NOT NULL
                           AND expires_at <= ?1
                           AND deleted_at IS NULL",
                    )
                    .map_err(|e| {
                        blockcell_core::Error::Storage(format!("TTL cleanup prepare error: {}", e))
                    })?;
                let rows = stmt
                    .query_map(params![now], |row| row.get::<_, String>(0))
                    .map_err(|e| {
                        blockcell_core::Error::Storage(format!("TTL cleanup query error: {}", e))
                    })?;
                let mut ids = Vec::new();
                for row in rows {
                    ids.push(row.map_err(|e| {
                        blockcell_core::Error::Storage(format!("TTL cleanup id row error: {}", e))
                    })?);
                }
                ids
            };

            let purged_ids = {
                let mut stmt = conn
                    .prepare(
                        "SELECT id FROM memory_items
                         WHERE deleted_at IS NOT NULL
                           AND deleted_at < ?1",
                    )
                    .map_err(|e| {
                        blockcell_core::Error::Storage(format!("Purge prepare error: {}", e))
                    })?;
                let rows = stmt
                    .query_map(params![cutoff], |row| row.get::<_, String>(0))
                    .map_err(|e| {
                        blockcell_core::Error::Storage(format!("Purge query error: {}", e))
                    })?;
                let mut ids = Vec::new();
                for row in rows {
                    ids.push(row.map_err(|e| {
                        blockcell_core::Error::Storage(format!("Purge id row error: {}", e))
                    })?);
                }
                ids
            };

            if !expired_ids.is_empty() {
                let placeholders = (0..expired_ids.len())
                    .map(|offset| format!("?{}", offset + 2))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "UPDATE memory_items SET deleted_at = ?1 WHERE id IN ({})",
                    placeholders
                );
                let mut values: Vec<Box<dyn rusqlite::types::ToSql>> =
                    Vec::with_capacity(expired_ids.len() + 1);
                values.push(Box::new(now));
                for id in &expired_ids {
                    values.push(Box::new(id.clone()));
                }
                let refs: Vec<&dyn rusqlite::types::ToSql> =
                    values.iter().map(|value| value.as_ref()).collect();
                conn.execute(&sql, refs.as_slice()).map_err(|e| {
                    blockcell_core::Error::Storage(format!("TTL cleanup update error: {}", e))
                })?;
            }

            if !purged_ids.is_empty() {
                let placeholders = (0..purged_ids.len())
                    .map(|offset| format!("?{}", offset + 1))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!("DELETE FROM memory_items WHERE id IN ({})", placeholders);
                let values: Vec<&dyn rusqlite::types::ToSql> = purged_ids
                    .iter()
                    .map(|id| id as &dyn rusqlite::types::ToSql)
                    .collect();
                conn.execute(&sql, values.as_slice()).map_err(|e| {
                    blockcell_core::Error::Storage(format!("Purge delete error: {}", e))
                })?;
            }

            (expired_ids, purged_ids)
        };

        let mut deleted_ids = expired_ids.clone();
        deleted_ids.extend(purged_ids.iter().cloned());
        self.sync_vector_delete_ids(&deleted_ids);

        if !expired_ids.is_empty() || !purged_ids.is_empty() {
            info!(
                expired = expired_ids.len(),
                purged = purged_ids.len(),
                "Memory maintenance completed"
            );
        }

        Ok((expired_ids.len(), purged_ids.len()))
    }

    pub fn retry_vector_sync(&self, limit: usize) -> Result<VectorSyncRetryResult> {
        if self.vector.is_none() {
            return Err(blockcell_core::Error::Storage(
                "Vector runtime is not enabled".to_string(),
            ));
        }

        let pending = self.load_pending_vector_sync(limit)?;
        let mut result = VectorSyncRetryResult {
            attempted: pending.len(),
            succeeded: 0,
            failed: 0,
        };

        for entry in pending {
            let sync_result = match entry.operation.as_str() {
                VECTOR_SYNC_OP_DELETE => {
                    self.try_vector_delete_ids(std::slice::from_ref(&entry.id))
                }
                VECTOR_SYNC_OP_UPSERT => match self.get_by_id(&entry.id)? {
                    Some(item) if is_item_active_for_vector(&item) => self.try_vector_upsert(&item),
                    _ => self.try_vector_delete_ids(std::slice::from_ref(&entry.id)),
                },
                other => Err(blockcell_core::Error::Storage(format!(
                    "Unknown vector sync operation: {}",
                    other
                ))),
            };

            match sync_result {
                Ok(()) => {
                    self.clear_vector_sync(&entry.id);
                    result.succeeded += 1;
                }
                Err(error) => {
                    self.enqueue_vector_sync(&entry.id, &entry.operation, &error.to_string());
                    warn!(
                        id = %entry.id,
                        operation = %entry.operation,
                        error = %error,
                        "Retrying vector sync failed"
                    );
                    result.failed += 1;
                }
            }
        }

        Ok(result)
    }

    pub fn reindex_vectors(&self) -> Result<VectorReindexResult> {
        let runtime = self.vector.as_ref().ok_or_else(|| {
            blockcell_core::Error::Storage("Vector runtime is not enabled".to_string())
        })?;

        runtime.index.reset()?;
        self.clear_all_vector_sync()?;

        let items = self.load_reindexable_items()?;
        let mut result = VectorReindexResult {
            indexed: 0,
            failed: 0,
        };

        for item in items {
            match self.try_vector_upsert(&item) {
                Ok(()) => {
                    self.clear_vector_sync(&item.id);
                    result.indexed += 1;
                }
                Err(error) => {
                    self.enqueue_vector_sync(&item.id, VECTOR_SYNC_OP_UPSERT, &error.to_string());
                    warn!(id = %item.id, error = %error, "Failed to reindex vector entry");
                    result.failed += 1;
                }
            }
        }

        Ok(result)
    }

    /// Upsert a session summary for prompt injection.
    /// Uses dedup_key = "session_summary:{session_key}" so each session has exactly one summary.
    pub fn upsert_session_summary(&self, session_key: &str, summary: &str) -> Result<()> {
        let dedup_key = format!("session_summary:{}", session_key);
        let params = UpsertParams {
            scope: "short_term".to_string(),
            item_type: "session_summary".to_string(),
            title: Some(format!("Session: {}", session_key)),
            content: summary.to_string(),
            summary: None,
            tags: vec!["session_summary".to_string()],
            source: "ghost".to_string(),
            channel: None,
            session_key: Some(session_key.to_string()),
            importance: 0.8,
            dedup_key: Some(dedup_key),
            expires_at: None,
        };
        self.upsert(params)?;
        Ok(())
    }

    /// Get the session summary for a given session key, if one exists.
    pub fn get_session_summary(&self, session_key: &str) -> Result<Option<String>> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

        let dedup_key = format!("session_summary:{}", session_key);
        let result: Option<String> = conn
            .query_row(
                "SELECT content FROM memory_items WHERE dedup_key = ?1 AND deleted_at IS NULL",
                params![dedup_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| blockcell_core::Error::Storage(format!("Query error: {}", e)))?;

        Ok(result)
    }

    /// Generate a brief summary for prompt injection.
    /// Returns up to `long_term_max` long-term summaries and `short_term_max` short-term summaries.
    pub fn generate_brief(&self, long_term_max: usize, short_term_max: usize) -> Result<String> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

        let mut brief = String::new();

        // Long-term items: highest importance, use summary if available
        let mut stmt = conn
            .prepare(
                "SELECT id, title, summary, content, type, importance FROM memory_items
             WHERE scope = 'long_term' AND deleted_at IS NULL
               AND (expires_at IS NULL OR expires_at > ?1)
             ORDER BY importance DESC, access_count DESC, updated_at DESC
             LIMIT ?2",
            )
            .map_err(|e| blockcell_core::Error::Storage(format!("Brief query error: {}", e)))?;

        let now = Utc::now().to_rfc3339();
        let now_s = now.as_str();
        let lt_max = long_term_max as i64;
        let lt_rows = stmt
            .query_map(params![now_s, lt_max], |row| {
                let title: Option<String> = row.get("title")?;
                let summary: Option<String> = row.get("summary")?;
                let content: String = row.get("content")?;
                let item_type: String = row.get("type")?;
                Ok((title, summary, content, item_type))
            })
            .map_err(|e| blockcell_core::Error::Storage(format!("Brief query error: {}", e)))?;

        let mut lt_items = Vec::new();
        for (title, summary, content, item_type) in lt_rows.flatten() {
            let display = if let Some(s) = summary {
                s
            } else if let Some(t) = title {
                let first_line = content.lines().next().unwrap_or("").to_string();
                let fl_truncated: String = first_line.chars().take(100).collect();
                if first_line.chars().count() > 100 {
                    format!("{}: {}...", t, fl_truncated)
                } else {
                    format!("{}: {}", t, first_line)
                }
            } else {
                let truncated: String = content.chars().take(120).collect();
                if content.chars().count() > 120 {
                    format!("{}...", truncated)
                } else {
                    truncated
                }
            };
            lt_items.push(format!("- [{}] {}", item_type, display));
        }

        if !lt_items.is_empty() {
            brief.push_str("### Long-term Memory\n");
            for item in &lt_items {
                brief.push_str(item);
                brief.push('\n');
            }
            brief.push('\n');
        }

        // Short-term items: recent, high importance
        let mut stmt = conn
            .prepare(
                "SELECT id, title, summary, content, type, importance FROM memory_items
             WHERE scope = 'short_term' AND deleted_at IS NULL
               AND (expires_at IS NULL OR expires_at > ?1)
             ORDER BY updated_at DESC, importance DESC
             LIMIT ?2",
            )
            .map_err(|e| blockcell_core::Error::Storage(format!("Brief query error: {}", e)))?;

        let st_max = short_term_max as i64;
        let st_rows = stmt
            .query_map(params![now_s, st_max], |row| {
                let title: Option<String> = row.get("title")?;
                let summary: Option<String> = row.get("summary")?;
                let content: String = row.get("content")?;
                let item_type: String = row.get("type")?;
                Ok((title, summary, content, item_type))
            })
            .map_err(|e| blockcell_core::Error::Storage(format!("Brief query error: {}", e)))?;

        let mut st_items = Vec::new();
        for (title, summary, content, item_type) in st_rows.flatten() {
            let display = if let Some(s) = summary {
                s
            } else if let Some(t) = title {
                let first_line = content.lines().next().unwrap_or("").to_string();
                let fl_truncated: String = first_line.chars().take(100).collect();
                if first_line.chars().count() > 100 {
                    format!("{}: {}...", t, fl_truncated)
                } else {
                    format!("{}: {}", t, first_line)
                }
            } else {
                let truncated: String = content.chars().take(120).collect();
                if content.chars().count() > 120 {
                    format!("{}...", truncated)
                } else {
                    truncated
                }
            };
            st_items.push(format!("- [{}] {}", item_type, display));
        }

        if !st_items.is_empty() {
            brief.push_str("### Recent Notes\n");
            for item in &st_items {
                brief.push_str(item);
                brief.push('\n');
            }
        }

        Ok(brief)
    }

    /// Generate a brief summary for prompt injection, filtered by relevance to a query.
    /// Uses FTS5 to find memories related to the current user input.
    /// Falls back to generate_brief() when query is empty.
    pub fn generate_brief_for_query(&self, query: &str, max_items: usize) -> Result<String> {
        let query = query.trim();
        if query.is_empty() || max_items == 0 {
            // Fallback: return a small general brief
            return self.generate_brief(5, 3);
        }

        let items = HybridMemoryRetriever::new(self).search(&QueryParams {
            query: Some(query.to_string()),
            top_k: max_items,
            ..Default::default()
        })?;

        if items.is_empty() {
            // No relevant matches — return a minimal general brief.
            return self.generate_brief(3, 2);
        }

        let mut brief = String::new();
        brief.push_str("### Relevant Memory\n");
        for result in &items {
            brief.push_str(&format_relevant_brief_item(&result.item));
            brief.push('\n');
        }
        Ok(brief)
    }

    /// Get statistics about the memory store.
    pub fn stats(&self) -> Result<serde_json::Value> {
        let (total, long_term, short_term, deleted) = {
            let conn = self
                .inner
                .lock()
                .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

            let total: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memory_items WHERE deleted_at IS NULL",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            let long_term: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memory_items WHERE scope = 'long_term' AND deleted_at IS NULL",
                [], |row| row.get(0),
            ).unwrap_or(0);

            let short_term: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memory_items WHERE scope = 'short_term' AND deleted_at IS NULL",
                [], |row| row.get(0),
            ).unwrap_or(0);

            let deleted: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memory_items WHERE deleted_at IS NOT NULL",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            (total, long_term, short_term, deleted)
        };

        let (pending_total, pending_upserts, pending_deletes) = self.pending_vector_counts()?;
        let (vector_enabled, vector_healthy, vector_backend) = if let Some(runtime) = &self.vector {
            let healthy = runtime.index.health().is_ok();
            let backend = runtime
                .index
                .stats()
                .unwrap_or_else(|error| serde_json::json!({ "error": error.to_string() }));
            (true, serde_json::Value::Bool(healthy), backend)
        } else {
            (false, serde_json::Value::Null, serde_json::Value::Null)
        };

        Ok(serde_json::json!({
            "total_active": total,
            "long_term": long_term,
            "short_term": short_term,
            "deleted_in_recycle_bin": deleted,
            "vector": {
                "enabled": vector_enabled,
                "healthy": vector_healthy,
                "pending_operations": pending_total,
                "pending_upserts": pending_upserts,
                "pending_deletes": pending_deletes,
                "backend": vector_backend,
            }
        }))
    }

    /// Import from existing MEMORY.md file.
    pub fn import_long_term_md(&self, content: &str) -> Result<usize> {
        let sections = parse_markdown_sections(content);
        let mut count = 0;

        for (heading, body) in &sections {
            let body = body.trim();
            if body.is_empty() || body.starts_with('(') {
                continue; // Skip placeholder sections
            }

            // Each non-empty section becomes a long-term memory item
            let dedup_key = format!(
                "import.long_term.{}",
                heading.to_lowercase().replace(' ', "_")
            );
            let _ = self.upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: classify_section(heading),
                title: Some(heading.clone()),
                content: body.to_string(),
                summary: None,
                tags: vec!["imported".to_string()],
                source: "import".to_string(),
                channel: None,
                session_key: None,
                importance: 0.7,
                dedup_key: Some(dedup_key),
                expires_at: None,
            })?;
            count += 1;
        }

        info!(count, "Imported long-term memory items from MEMORY.md");
        Ok(count)
    }

    /// Import from a daily note file.
    pub fn import_daily_md(&self, date: &str, content: &str) -> Result<usize> {
        let content = content.trim();
        if content.is_empty() {
            return Ok(0);
        }

        // Parse into sections or treat as one item
        let sections = parse_markdown_sections(content);
        let mut count = 0;

        if sections.is_empty() {
            // No sections, import as single note
            let dedup_key = format!("import.daily.{}", date);
            let expires_at = compute_daily_expiry(date, 30);
            let _ = self.upsert(UpsertParams {
                scope: "short_term".to_string(),
                item_type: "note".to_string(),
                title: Some(format!("Daily notes {}", date)),
                content: content.to_string(),
                summary: None,
                tags: vec!["daily".to_string(), "imported".to_string()],
                source: "import".to_string(),
                channel: None,
                session_key: None,
                importance: 0.4,
                dedup_key: Some(dedup_key),
                expires_at,
            })?;
            count += 1;
        } else {
            for (heading, body) in &sections {
                let body = body.trim();
                if body.is_empty() {
                    continue;
                }
                let dedup_key = format!(
                    "import.daily.{}.{}",
                    date,
                    heading.to_lowercase().replace(' ', "_")
                );
                let expires_at = compute_daily_expiry(date, 30);
                let _ = self.upsert(UpsertParams {
                    scope: "short_term".to_string(),
                    item_type: classify_section(heading),
                    title: Some(format!("{} ({})", heading, date)),
                    content: body.to_string(),
                    summary: None,
                    tags: vec!["daily".to_string(), "imported".to_string()],
                    source: "import".to_string(),
                    channel: None,
                    session_key: None,
                    importance: 0.4,
                    dedup_key: Some(dedup_key),
                    expires_at,
                })?;
                count += 1;
            }
        }

        info!(date, count, "Imported daily memory items");
        Ok(count)
    }

    /// Check if migration has already been done.
    pub fn is_migrated(&self) -> bool {
        let conn = match self.inner.lock() {
            Ok(c) => c,
            Err(_) => return false,
        };
        conn.query_row(
            "SELECT value FROM memory_meta WHERE key = 'migrated_from_md'",
            [],
            |row| row.get::<_, String>(0),
        )
        .is_ok()
    }

    /// Mark migration as done.
    pub fn mark_migrated(&self) -> Result<()> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO memory_meta (key, value) VALUES ('migrated_from_md', ?1)",
            params![now],
        )
        .map_err(|e| blockcell_core::Error::Storage(format!("Mark migrated error: {}", e)))?;
        Ok(())
    }

    /// Run the full migration from MEMORY.md and daily files.
    pub fn migrate_from_files(&self, memory_dir: &Path) -> Result<usize> {
        if self.is_migrated() {
            debug!("Memory migration already done, skipping");
            return Ok(0);
        }

        let mut total = 0;

        // Import MEMORY.md
        let memory_md = memory_dir.join("MEMORY.md");
        if memory_md.exists() {
            if let Ok(content) = std::fs::read_to_string(&memory_md) {
                match self.import_long_term_md(&content) {
                    Ok(n) => total += n,
                    Err(e) => warn!(error = %e, "Failed to import MEMORY.md"),
                }
            }
        }

        // Import daily notes
        if let Ok(entries) = std::fs::read_dir(memory_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                // Match YYYY-MM-DD.md pattern
                if name.len() == 13 && name.ends_with(".md") && name != "MEMORY.md" {
                    let date = &name[..10];
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        match self.import_daily_md(date, &content) {
                            Ok(n) => total += n,
                            Err(e) => warn!(date, error = %e, "Failed to import daily note"),
                        }
                    }
                }
            }
        }

        self.mark_migrated()?;
        info!(total, "Memory migration from files completed");
        Ok(total)
    }
}

fn build_embedding_text(item: &MemoryItem) -> String {
    let mut parts = Vec::new();

    if let Some(title) = item
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("Title: {}", title));
    }

    let summary = item
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| truncate_chars(item.content.trim(), 240));
    if !summary.is_empty() {
        parts.push(format!("Summary: {}", summary));
    }

    let tags: Vec<&str> = item
        .tags
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .take(3)
        .collect();
    if !tags.is_empty() {
        parts.push(format!("Tags: {}", tags.join(", ")));
    }

    if parts.is_empty() {
        truncate_chars(item.content.trim(), 240)
    } else {
        parts.join("\n")
    }
}

fn is_item_active_for_vector(item: &MemoryItem) -> bool {
    if item.deleted_at.is_some() {
        return false;
    }

    match item.expires_at.as_deref() {
        Some(expires_at) => match DateTime::parse_from_rfc3339(expires_at) {
            Ok(value) => value.with_timezone(&Utc) > Utc::now(),
            Err(_) => false,
        },
        None => true,
    }
}

fn format_relevant_brief_item(item: &MemoryItem) -> String {
    let display = item
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            item.title
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|title| {
                    let first_line = item.content.lines().next().unwrap_or("").trim();
                    if first_line.is_empty() {
                        title.to_string()
                    } else {
                        format!("{}: {}", title, truncate_chars(first_line, 100))
                    }
                })
        })
        .unwrap_or_else(|| truncate_chars(item.content.trim(), 120));
    let scope_tag = if item.scope == "long_term" {
        "LT"
    } else {
        "ST"
    };
    format!("- [{}|{}] {}", item.item_type, scope_tag, display)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let truncated: String = value.chars().take(max_chars).collect();
    if value.chars().count() > max_chars {
        format!("{}...", truncated)
    } else {
        truncated
    }
}

/// Sanitize a user query for FTS5 (escape special characters, use implicit AND).
pub(crate) fn sanitize_fts_query(query: &str) -> String {
    // 使用预编译的静态正则，避免每次调用重新编译
    let cleaned = FTS_SPECIAL_CHARS.replace_all(query, " ");
    // Split into tokens and wrap each in quotes for exact matching
    let tokens: Vec<String> = cleaned
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t))
        .collect();
    if tokens.is_empty() {
        "\"\"".to_string()
    } else {
        tokens.join(" ")
    }
}

/// Parse markdown content into (heading, body) sections.
/// 识别 ## 和 ### 级别的标题（# 为文档标题，跳过）。
fn parse_markdown_sections(content: &str) -> Vec<(String, String)> {
    let mut sections = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_body = String::new();

    for line in content.lines() {
        if line.starts_with("## ") || line.starts_with("### ") {
            // 保存上一个 section
            if let Some(heading) = current_heading.take() {
                sections.push((heading, current_body.clone()));
            }
            // 剥离前缀 # 字符和空格
            let heading_text = line.trim_start_matches('#').trim().to_string();
            current_heading = Some(heading_text);
            current_body.clear();
        } else if line.starts_with("# ") && current_heading.is_none() {
            // 顶级标题为文档标题，跳过
            continue;
        } else if current_heading.is_some() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }

    // 保存最后一个 section
    if let Some(heading) = current_heading {
        sections.push((heading, current_body));
    }

    sections
}

/// Classify a section heading into a memory type.
fn classify_section(heading: &str) -> String {
    let h = heading.to_lowercase();
    if h.contains("preference") || h.contains("偏好") {
        "preference".to_string()
    } else if h.contains("project") || h.contains("项目") {
        "project".to_string()
    } else if h.contains("user") || h.contains("用户") || h.contains("info") {
        "fact".to_string()
    } else if h.contains("task") || h.contains("todo") || h.contains("任务") {
        "task".to_string()
    } else if h.contains("policy") || h.contains("rule") || h.contains("规则") {
        "policy".to_string()
    } else if h.contains("contact") || h.contains("联系") {
        "contact".to_string()
    } else {
        "note".to_string()
    }
}

/// Compute an expiry date for a daily note: date + days.
fn compute_daily_expiry(date_str: &str, days: i64) -> Option<String> {
    chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
        .ok()
        .map(|d| {
            let expiry = d + chrono::Duration::days(days);
            let dt: DateTime<Utc> =
                DateTime::from_naive_utc_and_offset(expiry.and_hms_opt(0, 0, 0).unwrap(), Utc);
            dt.to_rfc3339()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::{Embedder, VectorHit, VectorIndex, VectorMeta, VectorRuntime};
    use std::str::FromStr;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    fn test_store() -> (MemoryStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("memory.db");
        let store = MemoryStore::open(&db_path).unwrap();
        (store, dir)
    }

    #[derive(Clone)]
    struct FakeEmbedder {
        dimensions: usize,
        query_inputs: Arc<Mutex<Vec<String>>>,
        document_inputs: Arc<Mutex<Vec<String>>>,
    }

    impl FakeEmbedder {
        fn new(dimensions: usize) -> Self {
            Self {
                dimensions,
                query_inputs: Arc::new(Mutex::new(Vec::new())),
                document_inputs: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl Embedder for FakeEmbedder {
        fn model_id(&self) -> &str {
            "fake-embedder"
        }

        fn dimensions(&self) -> usize {
            self.dimensions
        }

        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            self.query_inputs.lock().unwrap().push(text.to_string());
            Ok(vec![0.25; self.dimensions])
        }

        fn embed_document(&self, text: &str) -> Result<Vec<f32>> {
            self.document_inputs.lock().unwrap().push(text.to_string());
            Ok(vec![0.5; self.dimensions])
        }
    }

    #[derive(Debug, Clone, Default)]
    struct FakeVectorIndexState {
        upserts: Vec<(String, Vec<f32>, VectorMeta)>,
        deleted_ids: Vec<String>,
        search_hits: Vec<VectorHit>,
        fail_search: bool,
        fail_upsert: bool,
        fail_delete: bool,
        health_error: Option<String>,
        reset_calls: usize,
    }

    #[derive(Clone, Default)]
    struct FakeVectorIndex {
        state: Arc<Mutex<FakeVectorIndexState>>,
    }

    impl FakeVectorIndex {
        fn new() -> Self {
            Self::default()
        }

        fn with_hits(hits: Vec<VectorHit>) -> Self {
            let state = FakeVectorIndexState {
                search_hits: hits,
                ..Default::default()
            };
            Self {
                state: Arc::new(Mutex::new(state)),
            }
        }

        fn with_search_failure() -> Self {
            let state = FakeVectorIndexState {
                fail_search: true,
                ..Default::default()
            };
            Self {
                state: Arc::new(Mutex::new(state)),
            }
        }

        fn with_upsert_failure() -> Self {
            let state = FakeVectorIndexState {
                fail_upsert: true,
                ..Default::default()
            };
            Self {
                state: Arc::new(Mutex::new(state)),
            }
        }

        fn with_delete_failure() -> Self {
            let state = FakeVectorIndexState {
                fail_delete: true,
                ..Default::default()
            };
            Self {
                state: Arc::new(Mutex::new(state)),
            }
        }
    }

    impl VectorIndex for FakeVectorIndex {
        fn upsert(&self, id: &str, vector: &[f32], meta: &VectorMeta) -> Result<()> {
            if self.state.lock().unwrap().fail_upsert {
                return Err(blockcell_core::Error::Storage(
                    "forced vector upsert failure".to_string(),
                ));
            }
            self.state.lock().unwrap().upserts.push((
                id.to_string(),
                vector.to_vec(),
                meta.clone(),
            ));
            Ok(())
        }

        fn delete_ids(&self, ids: &[String]) -> Result<()> {
            if self.state.lock().unwrap().fail_delete {
                return Err(blockcell_core::Error::Storage(
                    "forced vector delete failure".to_string(),
                ));
            }
            self.state
                .lock()
                .unwrap()
                .deleted_ids
                .extend(ids.iter().cloned());
            Ok(())
        }

        fn search(&self, _vector: &[f32], _top_k: usize) -> Result<Vec<VectorHit>> {
            let state = self.state.lock().unwrap();
            if state.fail_search {
                return Err(blockcell_core::Error::Storage(
                    "forced vector search failure".to_string(),
                ));
            }
            Ok(state.search_hits.clone())
        }

        fn health(&self) -> Result<()> {
            if let Some(message) = self.state.lock().unwrap().health_error.clone() {
                Err(blockcell_core::Error::Storage(message))
            } else {
                Ok(())
            }
        }

        fn stats(&self) -> Result<serde_json::Value> {
            let state = self.state.lock().unwrap();
            Ok(serde_json::json!({
                "rows": state.upserts.len(),
                "deleted_ids": state.deleted_ids.len(),
                "reset_calls": state.reset_calls,
            }))
        }

        fn reset(&self) -> Result<()> {
            let mut state = self.state.lock().unwrap();
            state.reset_calls += 1;
            state.upserts.clear();
            state.deleted_ids.clear();
            Ok(())
        }
    }

    fn test_store_with_vector(vector: Option<Arc<VectorRuntime>>) -> (MemoryStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("memory.db");
        let store =
            MemoryStore::open_with_options(&db_path, MemoryStoreOptions { vector }).unwrap();
        (store, dir)
    }

    fn fake_vector_runtime(embedder: FakeEmbedder, index: FakeVectorIndex) -> Arc<VectorRuntime> {
        Arc::new(VectorRuntime {
            embedder: Arc::new(embedder),
            index: Arc::new(index),
        })
    }

    #[test]
    fn test_upsert_and_query() {
        let (store, _dir) = test_store();

        // Insert
        let item = store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "fact".to_string(),
                title: Some("User name".to_string()),
                content: "The user's name is Alice".to_string(),
                summary: Some("User is Alice".to_string()),
                tags: vec!["user".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.9,
                dedup_key: Some("user.name".to_string()),
                expires_at: None,
            })
            .unwrap();

        assert_eq!(item.scope, "long_term");
        assert_eq!(item.content, "The user's name is Alice");

        // Query by FTS
        let results = store
            .query(&QueryParams {
                query: Some("Alice".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item.id, item.id);

        // Query with scope filter
        let results = store
            .query(&QueryParams {
                scope: Some("short_term".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_dedup_key_update() {
        let (store, _dir) = test_store();

        // Insert first
        let item1 = store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "preference".to_string(),
                title: Some("Language".to_string()),
                content: "User prefers English".to_string(),
                summary: None,
                tags: vec![],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.8,
                dedup_key: Some("pref.language".to_string()),
                expires_at: None,
            })
            .unwrap();

        // Upsert with same dedup_key
        let item2 = store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "preference".to_string(),
                title: Some("Language".to_string()),
                content: "User prefers Chinese".to_string(),
                summary: None,
                tags: vec![],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.8,
                dedup_key: Some("pref.language".to_string()),
                expires_at: None,
            })
            .unwrap();

        // Same ID, updated content
        assert_eq!(item1.id, item2.id);
        assert_eq!(item2.content, "User prefers Chinese");
    }

    #[test]
    fn test_soft_delete_and_restore() {
        let (store, _dir) = test_store();

        let item = store
            .upsert(UpsertParams {
                scope: "short_term".to_string(),
                item_type: "note".to_string(),
                title: None,
                content: "Temporary note".to_string(),
                summary: None,
                tags: vec![],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.5,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();

        // Soft delete
        assert!(store.soft_delete(&item.id).unwrap());

        // Should not appear in normal query
        let results = store.query(&QueryParams::default()).unwrap();
        assert_eq!(results.len(), 0);

        // Should appear with include_deleted
        let results = store
            .query(&QueryParams {
                include_deleted: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);

        // Restore
        assert!(store.restore(&item.id).unwrap());

        // Should appear again
        let results = store.query(&QueryParams::default()).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_brief_generation() {
        let (store, _dir) = test_store();

        store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "fact".to_string(),
                title: Some("User name".to_string()),
                content: "Alice".to_string(),
                summary: Some("User is Alice".to_string()),
                tags: vec![],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.9,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();

        store
            .upsert(UpsertParams {
                scope: "short_term".to_string(),
                item_type: "note".to_string(),
                title: Some("Meeting".to_string()),
                content: "Had a meeting about project X".to_string(),
                summary: None,
                tags: vec![],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.5,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();

        let brief = store.generate_brief(20, 10).unwrap();
        assert!(brief.contains("Long-term Memory"));
        assert!(brief.contains("User is Alice"));
        assert!(brief.contains("Recent Notes"));
        assert!(brief.contains("Meeting"));
    }

    #[test]
    fn test_import_markdown() {
        let (store, _dir) = test_store();

        let md = r#"# Long-term Memory

## User Information

Name: Bob
Location: Beijing

## Preferences

Prefers dark mode
Language: Chinese

## Empty Section

(placeholder)
"#;
        let count = store.import_long_term_md(md).unwrap();
        assert_eq!(count, 2); // Empty/placeholder sections skipped

        let results = store
            .query(&QueryParams {
                query: Some("Bob".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_canonical_memory_type_rejects_removed_summary_type() {
        assert!(MemoryType::from_str("summary").is_err());
    }

    #[test]
    fn test_batch_delete_tags_matches_any_tag() {
        let (store, _dir) = test_store();

        let _alpha = store
            .upsert(UpsertParams {
                scope: "short_term".to_string(),
                item_type: "note".to_string(),
                title: Some("alpha".to_string()),
                content: "tag alpha".to_string(),
                summary: None,
                tags: vec!["alpha".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.2,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();

        let _beta = store
            .upsert(UpsertParams {
                scope: "short_term".to_string(),
                item_type: "note".to_string(),
                title: Some("beta".to_string()),
                content: "tag beta".to_string(),
                summary: None,
                tags: vec!["beta".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.2,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();

        let tags = vec!["alpha".to_string(), "beta".to_string()];
        let deleted = store
            .batch_soft_delete(None, None, Some(tags.as_slice()), None)
            .unwrap();
        assert_eq!(deleted, 2);
    }

    #[test]
    fn test_short_term_write_gets_default_ttl_in_service() {
        use crate::memory_contract::MemoryUpsertRequest;
        use crate::memory_service::MemoryService;

        let (store, _dir) = test_store();
        let service = MemoryService::new(store);

        let request = MemoryUpsertRequest {
            scope: "short_term".to_string(),
            item_type: "note".to_string(),
            title: Some("ttl default".to_string()),
            content: "ttl default content".to_string(),
            summary: None,
            tags: vec![],
            source: "user".to_string(),
            channel: None,
            session_key: None,
            importance: 0.5,
            dedup_key: None,
            expires_at: None,
        };

        let item = service.upsert(request).unwrap();
        assert!(item.expires_at.is_some());
    }

    #[test]
    fn test_vector_index_called_on_insert() {
        let embedder = FakeEmbedder::new(3);
        let index = FakeVectorIndex::new();
        let runtime = fake_vector_runtime(embedder.clone(), index.clone());
        let (store, _dir) = test_store_with_vector(Some(runtime));

        let item = store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "fact".to_string(),
                title: Some("favorite database".to_string()),
                content: "The preferred vector store is RabitQ".to_string(),
                summary: Some("Prefers RabitQ".to_string()),
                tags: vec!["vector".to_string(), "database".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: Some("chat-1".to_string()),
                importance: 0.9,
                dedup_key: Some("pref.vector_store".to_string()),
                expires_at: None,
            })
            .unwrap();

        let state = index.state.lock().unwrap();
        assert_eq!(state.upserts.len(), 1);
        assert_eq!(state.upserts[0].0, item.id);
        assert_eq!(state.upserts[0].2.scope, "long_term");
        assert_eq!(state.upserts[0].2.item_type, "fact");
        assert_eq!(
            *embedder.document_inputs.lock().unwrap(),
            vec![
                "Title: favorite database\nSummary: Prefers LanceDB\nTags: vector, database"
                    .to_string()
            ]
        );
    }

    #[test]
    fn test_vector_index_overwrites_same_id_on_dedup_update() {
        let embedder = FakeEmbedder::new(3);
        let index = FakeVectorIndex::new();
        let runtime = fake_vector_runtime(embedder.clone(), index.clone());
        let (store, _dir) = test_store_with_vector(Some(runtime));

        let item1 = store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "preference".to_string(),
                title: Some("runtime".to_string()),
                content: "Use SQLite for canonical storage".to_string(),
                summary: None,
                tags: vec!["storage".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.8,
                dedup_key: Some("pref.storage".to_string()),
                expires_at: None,
            })
            .unwrap();

        let item2 = store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "preference".to_string(),
                title: Some("runtime".to_string()),
                content: "Use SQLite for canonical storage and RabitQ for vectors".to_string(),
                summary: None,
                tags: vec!["storage".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.95,
                dedup_key: Some("pref.storage".to_string()),
                expires_at: None,
            })
            .unwrap();

        let state = index.state.lock().unwrap();
        assert_eq!(item1.id, item2.id);
        assert_eq!(state.upserts.len(), 2);
        assert_eq!(state.upserts[0].0, item1.id);
        assert_eq!(state.upserts[1].0, item2.id);
        assert_eq!(
            *embedder.document_inputs.lock().unwrap(),
            vec![
                "Title: runtime\nSummary: Use SQLite for canonical storage\nTags: storage"
                    .to_string(),
                "Title: runtime\nSummary: Use SQLite for canonical storage and RabitQ for vectors\nTags: storage"
                    .to_string()
            ]
        );
    }

    #[test]
    fn test_soft_delete_removes_vector_by_id() {
        let embedder = FakeEmbedder::new(3);
        let index = FakeVectorIndex::new();
        let runtime = fake_vector_runtime(embedder, index.clone());
        let (store, _dir) = test_store_with_vector(Some(runtime));

        let item = store
            .upsert(UpsertParams {
                scope: "short_term".to_string(),
                item_type: "note".to_string(),
                title: Some("delete me".to_string()),
                content: "This memory should be removed from vector index".to_string(),
                summary: None,
                tags: vec!["tmp".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.2,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();

        assert!(store.soft_delete(&item.id).unwrap());

        let state = index.state.lock().unwrap();
        assert_eq!(state.deleted_ids, vec![item.id]);
    }

    #[test]
    fn test_vector_consistency_batch_soft_delete_removes_all_vector_ids() {
        let embedder = FakeEmbedder::new(3);
        let index = FakeVectorIndex::new();
        let runtime = fake_vector_runtime(embedder, index.clone());
        let (store, _dir) = test_store_with_vector(Some(runtime));

        let item1 = store
            .upsert(UpsertParams {
                scope: "short_term".to_string(),
                item_type: "note".to_string(),
                title: Some("alpha".to_string()),
                content: "batch delete alpha".to_string(),
                summary: None,
                tags: vec!["alpha".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.2,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();

        let item2 = store
            .upsert(UpsertParams {
                scope: "short_term".to_string(),
                item_type: "note".to_string(),
                title: Some("beta".to_string()),
                content: "batch delete beta".to_string(),
                summary: None,
                tags: vec!["beta".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.2,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();

        index.state.lock().unwrap().deleted_ids.clear();

        let tags = vec!["alpha".to_string(), "beta".to_string()];
        let deleted = store
            .batch_soft_delete(None, None, Some(tags.as_slice()), None)
            .unwrap();

        assert_eq!(deleted, 2);
        let mut deleted_ids = index.state.lock().unwrap().deleted_ids.clone();
        deleted_ids.sort();
        let mut expected = vec![item1.id, item2.id];
        expected.sort();
        assert_eq!(deleted_ids, expected);
    }

    #[test]
    fn test_vector_consistency_maintenance_removes_expired_and_purged_vector_ids() {
        let embedder = FakeEmbedder::new(3);
        let index = FakeVectorIndex::new();
        let runtime = fake_vector_runtime(embedder, index.clone());
        let (store, _dir) = test_store_with_vector(Some(runtime));

        let expired_item = store
            .upsert(UpsertParams {
                scope: "short_term".to_string(),
                item_type: "note".to_string(),
                title: Some("expired".to_string()),
                content: "expired memory".to_string(),
                summary: None,
                tags: vec!["ttl".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.2,
                dedup_key: None,
                expires_at: Some((Utc::now() - chrono::Duration::days(1)).to_rfc3339()),
            })
            .unwrap();

        let purged_item = store
            .upsert(UpsertParams {
                scope: "short_term".to_string(),
                item_type: "note".to_string(),
                title: Some("purged".to_string()),
                content: "purged memory".to_string(),
                summary: None,
                tags: vec!["recycle".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.2,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();

        {
            let conn = store.inner.lock().unwrap();
            let old_deleted_at = (Utc::now() - chrono::Duration::days(45)).to_rfc3339();
            conn.execute(
                "UPDATE memory_items SET deleted_at = ?1 WHERE id = ?2",
                params![old_deleted_at, purged_item.id],
            )
            .unwrap();
        }

        index.state.lock().unwrap().deleted_ids.clear();

        let (expired, purged) = store.maintenance(30).unwrap();
        assert_eq!(expired, 1);
        assert_eq!(purged, 1);

        let mut deleted_ids = index.state.lock().unwrap().deleted_ids.clone();
        deleted_ids.sort();
        let mut expected = vec![expired_item.id, purged_item.id];
        expected.sort();
        assert_eq!(deleted_ids, expected);
    }

    #[test]
    fn test_vector_consistency_retry_queue_persists_failed_upsert() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("memory.db");

        let failing_runtime =
            fake_vector_runtime(FakeEmbedder::new(3), FakeVectorIndex::with_upsert_failure());
        let failing_store = MemoryStore::open_with_options(
            &db_path,
            MemoryStoreOptions {
                vector: Some(failing_runtime),
            },
        )
        .unwrap();

        let item = failing_store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "fact".to_string(),
                title: Some("queued upsert".to_string()),
                content: "retry queue should persist".to_string(),
                summary: Some("retry queue upsert".to_string()),
                tags: vec!["queue".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.7,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();
        drop(failing_store);

        let stats = MemoryStore::open(&db_path).unwrap().stats().unwrap();
        assert_eq!(stats["vector"]["pending_operations"], 1);
        assert_eq!(stats["vector"]["pending_upserts"], 1);
        assert_eq!(stats["vector"]["pending_deletes"], 0);

        let retry_index = FakeVectorIndex::new();
        let retry_runtime = fake_vector_runtime(FakeEmbedder::new(3), retry_index.clone());
        let retry_store = MemoryStore::open_with_options(
            &db_path,
            MemoryStoreOptions {
                vector: Some(retry_runtime),
            },
        )
        .unwrap();

        let retried = retry_store.retry_vector_sync(10).unwrap();
        assert_eq!(retried.succeeded, 1);
        assert_eq!(retried.failed, 0);

        let final_stats = retry_store.stats().unwrap();
        assert_eq!(final_stats["vector"]["pending_operations"], 0);
        assert_eq!(
            retry_index
                .state
                .lock()
                .unwrap()
                .upserts
                .last()
                .map(|entry| entry.0.clone()),
            Some(item.id)
        );
    }

    #[test]
    fn test_vector_consistency_retry_queue_persists_failed_delete() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("memory.db");

        let failing_index = FakeVectorIndex::with_delete_failure();
        let failing_runtime = fake_vector_runtime(FakeEmbedder::new(3), failing_index);
        let failing_store = MemoryStore::open_with_options(
            &db_path,
            MemoryStoreOptions {
                vector: Some(failing_runtime),
            },
        )
        .unwrap();

        let item = failing_store
            .upsert(UpsertParams {
                scope: "short_term".to_string(),
                item_type: "note".to_string(),
                title: Some("queued delete".to_string()),
                content: "retry delete should persist".to_string(),
                summary: None,
                tags: vec!["queue".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.4,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();
        assert!(failing_store.soft_delete(&item.id).unwrap());
        drop(failing_store);

        let stats = MemoryStore::open(&db_path).unwrap().stats().unwrap();
        assert_eq!(stats["vector"]["pending_operations"], 1);
        assert_eq!(stats["vector"]["pending_upserts"], 0);
        assert_eq!(stats["vector"]["pending_deletes"], 1);

        let retry_index = FakeVectorIndex::new();
        let retry_runtime = fake_vector_runtime(FakeEmbedder::new(3), retry_index.clone());
        let retry_store = MemoryStore::open_with_options(
            &db_path,
            MemoryStoreOptions {
                vector: Some(retry_runtime),
            },
        )
        .unwrap();

        let retried = retry_store.retry_vector_sync(10).unwrap();
        assert_eq!(retried.succeeded, 1);
        assert_eq!(retried.failed, 0);

        let final_stats = retry_store.stats().unwrap();
        assert_eq!(final_stats["vector"]["pending_operations"], 0);
        assert_eq!(retry_index.state.lock().unwrap().deleted_ids, vec![item.id]);
    }

    #[test]
    fn test_vector_consistency_reindex_resets_and_rebuilds_from_active_rows() {
        let embedder = FakeEmbedder::new(3);
        let index = FakeVectorIndex::new();
        let runtime = fake_vector_runtime(embedder, index.clone());
        let (store, _dir) = test_store_with_vector(Some(runtime));

        let active = store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "fact".to_string(),
                title: Some("active".to_string()),
                content: "active memory".to_string(),
                summary: None,
                tags: vec!["keep".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.8,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();

        let deleted = store
            .upsert(UpsertParams {
                scope: "short_term".to_string(),
                item_type: "note".to_string(),
                title: Some("deleted".to_string()),
                content: "deleted memory".to_string(),
                summary: None,
                tags: vec!["drop".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.2,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();

        {
            let conn = store.inner.lock().unwrap();
            conn.execute(
                "UPDATE memory_items SET deleted_at = ?1 WHERE id = ?2",
                params![Utc::now().to_rfc3339(), deleted.id],
            )
            .unwrap();
        }

        {
            let mut state = index.state.lock().unwrap();
            state.upserts.clear();
            state.deleted_ids.clear();
        }

        let reindexed = store.reindex_vectors().unwrap();
        assert_eq!(reindexed.indexed, 1);
        assert_eq!(reindexed.failed, 0);

        let state = index.state.lock().unwrap();
        assert_eq!(state.reset_calls, 1);
        assert_eq!(state.upserts.len(), 1);
        assert_eq!(state.upserts[0].0, active.id);
    }

    #[test]
    fn test_vector_consistency_stats_report_health_and_pending_queue() {
        let embedder = FakeEmbedder::new(3);
        let index = FakeVectorIndex::new();
        index.state.lock().unwrap().health_error = Some("forced unhealthy".to_string());
        let runtime = fake_vector_runtime(embedder, index);
        let (store, _dir) = test_store_with_vector(Some(runtime));

        let stats = store.stats().unwrap();
        assert_eq!(stats["vector"]["enabled"], true);
        assert_eq!(stats["vector"]["healthy"], false);
        assert_eq!(stats["vector"]["pending_operations"], 0);
    }

    #[test]
    fn test_restore_recreates_vector_by_id() {
        let embedder = FakeEmbedder::new(3);
        let index = FakeVectorIndex::new();
        let runtime = fake_vector_runtime(embedder.clone(), index.clone());
        let (store, _dir) = test_store_with_vector(Some(runtime));

        let item = store
            .upsert(UpsertParams {
                scope: "short_term".to_string(),
                item_type: "note".to_string(),
                title: Some("restore me".to_string()),
                content: "This memory should be reindexed on restore".to_string(),
                summary: Some("reindex on restore".to_string()),
                tags: vec!["restore".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.6,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();

        assert!(store.soft_delete(&item.id).unwrap());
        assert!(store.restore(&item.id).unwrap());

        let state = index.state.lock().unwrap();
        assert_eq!(state.deleted_ids, vec![item.id.clone()]);
        assert_eq!(
            state.upserts.last().map(|entry| entry.0.clone()),
            Some(item.id)
        );
        assert_eq!(
            *embedder.document_inputs.lock().unwrap(),
            vec![
                "Title: restore me\nSummary: reindex on restore\nTags: restore".to_string(),
                "Title: restore me\nSummary: reindex on restore\nTags: restore".to_string()
            ]
        );
    }

    #[test]
    fn test_query_falls_back_to_fts_when_vector_search_fails() {
        let embedder = FakeEmbedder::new(3);
        let index = FakeVectorIndex::with_search_failure();
        let runtime = fake_vector_runtime(embedder.clone(), index);
        let (store, _dir) = test_store_with_vector(Some(runtime));

        let item = store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "fact".to_string(),
                title: Some("routing".to_string()),
                content: "BlockCell uses SQLite as the canonical memory store".to_string(),
                summary: Some("SQLite stays canonical".to_string()),
                tags: vec!["memory".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.8,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();

        let results = store
            .query(&QueryParams {
                query: Some("canonical memory".to_string()),
                top_k: 5,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item.id, item.id);
        assert_eq!(
            *embedder.query_inputs.lock().unwrap(),
            vec!["canonical memory".to_string()]
        );
    }

    #[test]
    fn test_brief_query_reuses_hybrid_retrieval() {
        let embedder = FakeEmbedder::new(3);
        let index = FakeVectorIndex::with_hits(vec![VectorHit {
            id: String::new(),
            score: 0.99,
        }]);
        let runtime = fake_vector_runtime(embedder.clone(), index.clone());
        let (store, _dir) = test_store_with_vector(Some(runtime));

        let item = store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "fact".to_string(),
                title: Some("semantic result".to_string()),
                content: "RabitQ can recover semantically related memory".to_string(),
                summary: Some("semantic retrieval works".to_string()),
                tags: vec!["vector".to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.85,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();

        index.state.lock().unwrap().search_hits = vec![VectorHit {
            id: item.id.clone(),
            score: 0.99,
        }];

        let brief = store.generate_brief_for_query("semantic match", 5).unwrap();

        assert!(brief.contains("Relevant Memory"));
        assert!(brief.contains("semantic retrieval works"));
        assert_eq!(
            *embedder.query_inputs.lock().unwrap(),
            vec!["semantic match".to_string()]
        );
    }
}
