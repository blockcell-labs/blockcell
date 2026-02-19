use blockcell_core::Result;
use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

/// 预编译的 FTS5 特殊字符正则，避免每次调用重新编译
static FTS_SPECIAL_CHARS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"[*"():^{}]"#).expect("FTS special chars regex is valid")
});

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

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "short_term" => Some(MemoryScope::ShortTerm),
            "long_term" => Some(MemoryScope::LongTerm),
            _ => None,
        }
    }
}

/// Type classification of a memory item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    Fact,
    Preference,
    Project,
    Task,
    Glossary,
    Contact,
    Snippet,
    Policy,
    Summary,
    Note,
}

impl MemoryType {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryType::Fact => "fact",
            MemoryType::Preference => "preference",
            MemoryType::Project => "project",
            MemoryType::Task => "task",
            MemoryType::Glossary => "glossary",
            MemoryType::Contact => "contact",
            MemoryType::Snippet => "snippet",
            MemoryType::Policy => "policy",
            MemoryType::Summary => "summary",
            MemoryType::Note => "note",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "fact" => Some(MemoryType::Fact),
            "preference" => Some(MemoryType::Preference),
            "project" => Some(MemoryType::Project),
            "task" => Some(MemoryType::Task),
            "glossary" => Some(MemoryType::Glossary),
            "contact" => Some(MemoryType::Contact),
            "snippet" => Some(MemoryType::Snippet),
            "policy" => Some(MemoryType::Policy),
            "summary" => Some(MemoryType::Summary),
            "note" => Some(MemoryType::Note),
            _ => None,
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

/// SQLite-backed memory store with FTS5 full-text search.
#[derive(Clone)]
pub struct MemoryStore {
    inner: Arc<Mutex<Connection>>,
    #[allow(dead_code)]
    db_path: PathBuf,
}

impl MemoryStore {
    /// Open (or create) the memory database at the given path.
    pub fn open(db_path: &Path) -> Result<Self> {
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
        };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.inner.lock().map_err(|e| {
            blockcell_core::Error::Storage(format!("Lock error: {}", e))
        })?;

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
            "
        ).map_err(|e| {
            blockcell_core::Error::Storage(format!("Failed to init memory schema: {}", e))
        })?;

        debug!("Memory store schema initialized");
        Ok(())
    }

    /// Upsert a memory item. If dedup_key is set and a matching non-deleted item exists,
    /// update it instead of inserting a new one.
    pub fn upsert(&self, params: UpsertParams) -> Result<MemoryItem> {
        let conn = self.inner.lock().map_err(|e| {
            blockcell_core::Error::Storage(format!("Lock error: {}", e))
        })?;

        let now = Utc::now().to_rfc3339();
        let tags_str = params.tags.join(",");

        // Check for existing item by dedup_key
        if let Some(ref dk) = params.dedup_key {
            if !dk.is_empty() {
                let existing_id: Option<String> = conn.query_row(
                    "SELECT id FROM memory_items WHERE dedup_key = ?1 AND deleted_at IS NULL LIMIT 1",
                    params![dk],
                    |row| row.get(0),
                ).optional().map_err(|e| {
                    blockcell_core::Error::Storage(format!("Query error: {}", e))
                })?;

                if let Some(id) = existing_id {
                    // Update existing
                    conn.execute(
                        "UPDATE memory_items SET
                            content = ?1, summary = ?2, title = ?3, tags = ?4,
                            importance = ?5, updated_at = ?6, scope = ?7, type = ?8,
                            expires_at = ?9
                         WHERE id = ?10",
                        params![
                            params.content, params.summary, params.title, tags_str,
                            params.importance, now, params.scope, params.item_type,
                            params.expires_at, id
                        ],
                    ).map_err(|e| {
                        blockcell_core::Error::Storage(format!("Update error: {}", e))
                    })?;

                    debug!(id = %id, dedup_key = %dk, "Memory item updated via dedup_key");
                    return self.get_by_id_inner(&conn, &id);
                }
            }
        }

        // Insert new
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO memory_items (id, scope, type, title, content, summary, tags, source,
                channel, session_key, importance, created_at, updated_at, expires_at, dedup_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                id, params.scope, params.item_type, params.title, params.content,
                params.summary, tags_str, params.source, params.channel,
                params.session_key, params.importance, now, now,
                params.expires_at, params.dedup_key
            ],
        ).map_err(|e| {
            blockcell_core::Error::Storage(format!("Insert error: {}", e))
        })?;

        debug!(id = %id, scope = %params.scope, "Memory item inserted");
        self.get_by_id_inner(&conn, &id)
    }

    /// Query memory items using FTS5 + structured filters + scoring.
    pub fn query(&self, params: &QueryParams) -> Result<Vec<MemoryResult>> {
        let conn = self.inner.lock().map_err(|e| {
            blockcell_core::Error::Storage(format!("Lock error: {}", e))
        })?;

        let has_fts_query = params.query.as_ref().map_or(false, |q| !q.is_empty());

        // Build the query dynamically
        let mut sql = String::new();
        let mut where_clauses = Vec::new();
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut bind_idx = 1;

        if has_fts_query {
            sql.push_str(
                "SELECT m.*, bm25(memory_fts) AS fts_score
                 FROM memory_items m
                 JOIN memory_fts ON memory_fts.rowid = m.rowid
                 WHERE memory_fts MATCH ?1"
            );
            // FTS5 query: escape special chars, use implicit AND
            let fts_query = sanitize_fts_query(params.query.as_ref().unwrap());
            bind_values.push(Box::new(fts_query));
            bind_idx = 2;
        } else {
            sql.push_str(
                "SELECT m.*, 0.0 AS fts_score FROM memory_items m WHERE 1=1"
            );
        }

        // Soft-delete filter
        if !params.include_deleted {
            where_clauses.push("m.deleted_at IS NULL".to_string());
        }

        // Scope filter
        if let Some(ref scope) = params.scope {
            where_clauses.push(format!("m.scope = ?{}", bind_idx));
            bind_values.push(Box::new(scope.clone()));
            bind_idx += 1;
        }

        // Type filter
        if let Some(ref item_type) = params.item_type {
            where_clauses.push(format!("m.type = ?{}", bind_idx));
            bind_values.push(Box::new(item_type.clone()));
            bind_idx += 1;
        }

        // Tags filter (any match)
        if let Some(ref tags) = params.tags {
            if !tags.is_empty() {
                let tag_conditions: Vec<String> = tags.iter().enumerate().map(|(i, _)| {
                    let idx = bind_idx + i;
                    format!("m.tags LIKE '%' || ?{} || '%'", idx)
                }).collect();
                where_clauses.push(format!("({})", tag_conditions.join(" OR ")));
                for tag in tags {
                    bind_values.push(Box::new(tag.clone()));
                    bind_idx += 1;
                }
            }
        }

        // Time range filter
        if let Some(days) = params.time_range_days {
            let cutoff = (Utc::now() - chrono::Duration::days(days)).to_rfc3339();
            where_clauses.push(format!("m.created_at >= ?{}", bind_idx));
            bind_values.push(Box::new(cutoff));
            bind_idx += 1;
        }

        // Expired items filter (don't return expired items unless include_deleted)
        if !params.include_deleted {
            let now_str = Utc::now().to_rfc3339();
            where_clauses.push(format!(
                "(m.expires_at IS NULL OR m.expires_at > ?{})", bind_idx
            ));
            bind_values.push(Box::new(now_str));
            #[allow(unused_assignments)]
            { bind_idx += 1; }
        }

        for clause in &where_clauses {
            sql.push_str(&format!(" AND {}", clause));
        }

        // Scoring: combine FTS score with recency and importance
        // bm25 returns negative values (more negative = better match)
        sql.push_str(" ORDER BY ");
        if has_fts_query {
            // Combined score: -bm25 (higher is better) + importance + recency bonus
            sql.push_str(
                "(-fts_score * 10.0 + m.importance * 5.0 + \
                 CASE WHEN julianday('now') - julianday(m.updated_at) < 1 THEN 3.0 \
                      WHEN julianday('now') - julianday(m.updated_at) < 7 THEN 1.5 \
                      ELSE 0.0 END) DESC"
            );
        } else {
            sql.push_str("m.importance DESC, m.updated_at DESC");
        }

        sql.push_str(&format!(" LIMIT {}", params.top_k));

        let mut stmt = conn.prepare(&sql).map_err(|e| {
            blockcell_core::Error::Storage(format!("Prepare error: {}", e))
        })?;

        let bind_refs: Vec<&dyn rusqlite::types::ToSql> = bind_values.iter().map(|b| b.as_ref()).collect();

        // 先收集所有结果，释放 stmt 对 conn 的借用，再更新 access_count
        let results: Vec<MemoryResult> = {
            let rows = stmt.query_map(bind_refs.as_slice(), |row| {
                let fts_score: f64 = row.get("fts_score")?;
                let importance: f64 = row.get("importance")?;
                let tags_str: String = row.get("tags")?;

                Ok(MemoryResult {
                    item: MemoryItem {
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
                        importance,
                        created_at: row.get("created_at")?,
                        updated_at: row.get("updated_at")?,
                        last_accessed_at: row.get("last_accessed_at")?,
                        access_count: row.get("access_count")?,
                        expires_at: row.get("expires_at")?,
                        deleted_at: row.get("deleted_at")?,
                        dedup_key: row.get("dedup_key")?,
                    },
                    score: -fts_score * 10.0 + importance * 5.0,
                })
            }).map_err(|e| {
                blockcell_core::Error::Storage(format!("Query error: {}", e))
            })?;

            let mut collected = Vec::new();
            for row in rows {
                match row {
                    Ok(r) => collected.push(r),
                    Err(e) => warn!(error = %e, "Error reading memory row"),
                }
            }
            collected
        };
        // stmt 在此处已 drop，conn 的不可变借用已释放，可以安全地执行写操作

        // 更新访问统计
        if !results.is_empty() {
            let now = Utc::now().to_rfc3339();
            for r in &results {
                let _ = conn.execute(
                    "UPDATE memory_items SET access_count = access_count + 1, last_accessed_at = ?1 WHERE id = ?2",
                    params![now, r.item.id],
                );
            }
        }

        Ok(results)
    }

    /// Get a single item by ID.
    pub fn get_by_id(&self, id: &str) -> Result<Option<MemoryItem>> {
        let conn = self.inner.lock().map_err(|e| {
            blockcell_core::Error::Storage(format!("Lock error: {}", e))
        })?;
        match self.get_by_id_inner(&conn, id) {
            Ok(item) => Ok(Some(item)),
            // 仅当记录不存在时返回 None，其他数据库错误向上传播
            Err(blockcell_core::Error::Storage(ref msg)) if msg.contains("QueryReturnedNoRows") => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn get_by_id_inner(&self, conn: &Connection, id: &str) -> Result<MemoryItem> {
        conn.query_row(
            "SELECT * FROM memory_items WHERE id = ?1",
            params![id],
            |row| {
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
            },
        ).map_err(|e| {
            blockcell_core::Error::Storage(format!("Get by id error: {}", e))
        })
    }

    /// Soft-delete a memory item.
    pub fn soft_delete(&self, id: &str) -> Result<bool> {
        let conn = self.inner.lock().map_err(|e| {
            blockcell_core::Error::Storage(format!("Lock error: {}", e))
        })?;
        let now = Utc::now().to_rfc3339();
        let affected = conn.execute(
            "UPDATE memory_items SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            params![now, id],
        ).map_err(|e| {
            blockcell_core::Error::Storage(format!("Soft delete error: {}", e))
        })?;
        Ok(affected > 0)
    }

    /// Batch soft-delete by filter criteria.
    pub fn batch_soft_delete(
        &self,
        scope: Option<&str>,
        item_type: Option<&str>,
        tags: Option<&[String]>,
        time_before: Option<&str>,
    ) -> Result<usize> {
        let conn = self.inner.lock().map_err(|e| {
            blockcell_core::Error::Storage(format!("Lock error: {}", e))
        })?;

        let now = Utc::now().to_rfc3339();
        let mut sql = "UPDATE memory_items SET deleted_at = ?1 WHERE deleted_at IS NULL".to_string();
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        bind_values.push(Box::new(now));
        let mut idx = 2;

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
            for tag in tag_list {
                sql.push_str(&format!(" AND tags LIKE '%' || ?{} || '%'", idx));
                bind_values.push(Box::new(tag.clone()));
                idx += 1;
            }
        }
        if let Some(before) = time_before {
            sql.push_str(&format!(" AND created_at < ?{}", idx));
            bind_values.push(Box::new(before.to_string()));
            #[allow(unused_assignments)]
            { idx += 1; }
        }

        let bind_refs: Vec<&dyn rusqlite::types::ToSql> = bind_values.iter().map(|b| b.as_ref()).collect();
        let affected = conn.execute(&sql, bind_refs.as_slice()).map_err(|e| {
            blockcell_core::Error::Storage(format!("Batch delete error: {}", e))
        })?;

        info!(count = affected, "Batch soft-deleted memory items");
        Ok(affected)
    }

    /// Restore a soft-deleted item.
    pub fn restore(&self, id: &str) -> Result<bool> {
        let conn = self.inner.lock().map_err(|e| {
            blockcell_core::Error::Storage(format!("Lock error: {}", e))
        })?;
        let affected = conn.execute(
            "UPDATE memory_items SET deleted_at = NULL WHERE id = ?1 AND deleted_at IS NOT NULL",
            params![id],
        ).map_err(|e| {
            blockcell_core::Error::Storage(format!("Restore error: {}", e))
        })?;
        Ok(affected > 0)
    }

    /// Clean up expired items (set deleted_at) and hard-delete items that have been
    /// soft-deleted for more than `recycle_days` days.
    pub fn maintenance(&self, recycle_days: i64) -> Result<(usize, usize)> {
        let conn = self.inner.lock().map_err(|e| {
            blockcell_core::Error::Storage(format!("Lock error: {}", e))
        })?;

        let now = Utc::now().to_rfc3339();

        // Soft-delete expired items
        let expired = conn.execute(
            "UPDATE memory_items SET deleted_at = ?1 WHERE expires_at IS NOT NULL AND expires_at <= ?1 AND deleted_at IS NULL",
            params![now],
        ).map_err(|e| {
            blockcell_core::Error::Storage(format!("TTL cleanup error: {}", e))
        })?;

        // Hard-delete items in recycle bin for too long
        let cutoff = (Utc::now() - chrono::Duration::days(recycle_days)).to_rfc3339();
        let purged = conn.execute(
            "DELETE FROM memory_items WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            params![cutoff],
        ).map_err(|e| {
            blockcell_core::Error::Storage(format!("Purge error: {}", e))
        })?;

        if expired > 0 || purged > 0 {
            info!(expired, purged, "Memory maintenance completed");
        }

        Ok((expired, purged))
    }

    /// Generate a brief summary for prompt injection.
    /// Returns up to `long_term_max` long-term summaries and `short_term_max` short-term summaries.
    pub fn generate_brief(&self, long_term_max: usize, short_term_max: usize) -> Result<String> {
        let conn = self.inner.lock().map_err(|e| {
            blockcell_core::Error::Storage(format!("Lock error: {}", e))
        })?;

        let mut brief = String::new();

        // Long-term items: highest importance, use summary if available
        let mut stmt = conn.prepare(
            "SELECT id, title, summary, content, type, importance FROM memory_items
             WHERE scope = 'long_term' AND deleted_at IS NULL
               AND (expires_at IS NULL OR expires_at > ?1)
             ORDER BY importance DESC, access_count DESC, updated_at DESC
             LIMIT ?2"
        ).map_err(|e| {
            blockcell_core::Error::Storage(format!("Brief query error: {}", e))
        })?;

        let now = Utc::now().to_rfc3339();
        let now_s = now.as_str();
        let lt_max = long_term_max as i64;
        let lt_rows = stmt.query_map(params![now_s, lt_max], |row| {
            let title: Option<String> = row.get("title")?;
            let summary: Option<String> = row.get("summary")?;
            let content: String = row.get("content")?;
            let item_type: String = row.get("type")?;
            Ok((title, summary, content, item_type))
        }).map_err(|e| {
            blockcell_core::Error::Storage(format!("Brief query error: {}", e))
        })?;

        let mut lt_items = Vec::new();
        for row in lt_rows {
            if let Ok((title, summary, content, item_type)) = row {
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
        let mut stmt = conn.prepare(
            "SELECT id, title, summary, content, type, importance FROM memory_items
             WHERE scope = 'short_term' AND deleted_at IS NULL
               AND (expires_at IS NULL OR expires_at > ?1)
             ORDER BY updated_at DESC, importance DESC
             LIMIT ?2"
        ).map_err(|e| {
            blockcell_core::Error::Storage(format!("Brief query error: {}", e))
        })?;

        let st_max = short_term_max as i64;
        let st_rows = stmt.query_map(params![now_s, st_max], |row| {
            let title: Option<String> = row.get("title")?;
            let summary: Option<String> = row.get("summary")?;
            let content: String = row.get("content")?;
            let item_type: String = row.get("type")?;
            Ok((title, summary, content, item_type))
        }).map_err(|e| {
            blockcell_core::Error::Storage(format!("Brief query error: {}", e))
        })?;

        let mut st_items = Vec::new();
        for row in st_rows {
            if let Ok((title, summary, content, item_type)) = row {
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

    /// Get statistics about the memory store.
    pub fn stats(&self) -> Result<serde_json::Value> {
        let conn = self.inner.lock().map_err(|e| {
            blockcell_core::Error::Storage(format!("Lock error: {}", e))
        })?;

        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_items WHERE deleted_at IS NULL",
            [], |row| row.get(0),
        ).unwrap_or(0);

        let long_term: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_items WHERE scope = 'long_term' AND deleted_at IS NULL",
            [], |row| row.get(0),
        ).unwrap_or(0);

        let short_term: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_items WHERE scope = 'short_term' AND deleted_at IS NULL",
            [], |row| row.get(0),
        ).unwrap_or(0);

        let deleted: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_items WHERE deleted_at IS NOT NULL",
            [], |row| row.get(0),
        ).unwrap_or(0);

        Ok(serde_json::json!({
            "total_active": total,
            "long_term": long_term,
            "short_term": short_term,
            "deleted_in_recycle_bin": deleted,
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
            let dedup_key = format!("import.long_term.{}", heading.to_lowercase().replace(' ', "_"));
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
        ).is_ok()
    }

    /// Mark migration as done.
    pub fn mark_migrated(&self) -> Result<()> {
        let conn = self.inner.lock().map_err(|e| {
            blockcell_core::Error::Storage(format!("Lock error: {}", e))
        })?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO memory_meta (key, value) VALUES ('migrated_from_md', ?1)",
            params![now],
        ).map_err(|e| {
            blockcell_core::Error::Storage(format!("Mark migrated error: {}", e))
        })?;
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

/// Sanitize a user query for FTS5 (escape special characters, use implicit AND).
fn sanitize_fts_query(query: &str) -> String {
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
            let dt: DateTime<Utc> = DateTime::from_naive_utc_and_offset(
                expiry.and_hms_opt(0, 0, 0).unwrap(),
                Utc,
            );
            dt.to_rfc3339()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (MemoryStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("memory.db");
        let store = MemoryStore::open(&db_path).unwrap();
        (store, dir)
    }

    #[test]
    fn test_upsert_and_query() {
        let (store, _dir) = test_store();

        // Insert
        let item = store.upsert(UpsertParams {
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
        }).unwrap();

        assert_eq!(item.scope, "long_term");
        assert_eq!(item.content, "The user's name is Alice");

        // Query by FTS
        let results = store.query(&QueryParams {
            query: Some("Alice".to_string()),
            ..Default::default()
        }).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item.id, item.id);

        // Query with scope filter
        let results = store.query(&QueryParams {
            scope: Some("short_term".to_string()),
            ..Default::default()
        }).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_dedup_key_update() {
        let (store, _dir) = test_store();

        // Insert first
        let item1 = store.upsert(UpsertParams {
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
        }).unwrap();

        // Upsert with same dedup_key
        let item2 = store.upsert(UpsertParams {
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
        }).unwrap();

        // Same ID, updated content
        assert_eq!(item1.id, item2.id);
        assert_eq!(item2.content, "User prefers Chinese");
    }

    #[test]
    fn test_soft_delete_and_restore() {
        let (store, _dir) = test_store();

        let item = store.upsert(UpsertParams {
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
        }).unwrap();

        // Soft delete
        assert!(store.soft_delete(&item.id).unwrap());

        // Should not appear in normal query
        let results = store.query(&QueryParams::default()).unwrap();
        assert_eq!(results.len(), 0);

        // Should appear with include_deleted
        let results = store.query(&QueryParams {
            include_deleted: true,
            ..Default::default()
        }).unwrap();
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

        store.upsert(UpsertParams {
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
        }).unwrap();

        store.upsert(UpsertParams {
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
        }).unwrap();

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

        let results = store.query(&QueryParams {
            query: Some("Bob".to_string()),
            ..Default::default()
        }).unwrap();
        assert_eq!(results.len(), 1);
    }
}
