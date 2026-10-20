use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use blockcell_core::{Error, Paths, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeIndexEntry {
    pub id: String,
    pub file: String,
    pub anchor: String,
    pub content: String,
    pub content_hash: String,
    pub scope: String,
    pub source: String,
    pub updated_at: String,
    pub supersedes: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeIndexRebuildResult {
    pub indexed: usize,
    pub removed: usize,
    pub unchanged_files: usize,
}

#[derive(Clone)]
pub struct KnowledgeIndex {
    inner: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for KnowledgeIndex {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KnowledgeIndex")
            .finish_non_exhaustive()
    }
}

impl KnowledgeIndex {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)
            .map_err(|error| Error::Storage(format!("Failed to open knowledge index: {error}")))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .map_err(map_sqlite_error)?;
        let index = Self {
            inner: Arc::new(Mutex::new(conn)),
        };
        index.init_schema()?;
        Ok(index)
    }

    pub fn rebuild_from_files(&self, paths: &Paths) -> Result<KnowledgeIndexRebuildResult> {
        let mut result = KnowledgeIndexRebuildResult::default();
        for (label, path, scope) in [
            ("USER.md", paths.user_md(), "user"),
            ("memory/MEMORY.md", paths.memory_md(), "workspace"),
        ] {
            let file_result = self.rebuild_file(label, &path, scope)?;
            result.indexed += file_result.indexed;
            result.removed += file_result.removed;
            result.unchanged_files += file_result.unchanged_files;
        }
        Ok(result)
    }

    pub fn rebuild_file(
        &self,
        file: &str,
        path: &Path,
        default_scope: &str,
    ) -> Result<KnowledgeIndexRebuildResult> {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.into()),
        };
        let file_hash = sha256_hex(content.as_bytes());
        let entries = parse_entries(file, &content, default_scope);
        let mut conn = self
            .inner
            .lock()
            .map_err(|_| Error::Storage("Knowledge index lock poisoned".to_string()))?;
        let previous_hash = conn
            .query_row(
                "SELECT content_hash FROM knowledge_files WHERE file = ?1",
                params![file],
                |row| row.get::<_, String>(0),
            )
            .ok();
        if previous_hash.as_deref() == Some(file_hash.as_str()) {
            return Ok(KnowledgeIndexRebuildResult {
                unchanged_files: 1,
                ..KnowledgeIndexRebuildResult::default()
            });
        }

        let tx = conn.transaction().map_err(map_sqlite_error)?;
        let previous_count: usize = tx
            .query_row(
                "SELECT COUNT(*) FROM knowledge_entries WHERE file = ?1",
                params![file],
                |row| row.get(0),
            )
            .map_err(map_sqlite_error)?;
        tx.execute(
            "DELETE FROM knowledge_entries WHERE file = ?1",
            params![file],
        )
        .map_err(map_sqlite_error)?;
        for entry in &entries {
            tx.execute(
                "INSERT INTO knowledge_entries (
                    id, file, anchor, content, content_hash, scope, source, updated_at, supersedes
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    entry.id,
                    entry.file,
                    entry.anchor,
                    entry.content,
                    entry.content_hash,
                    entry.scope,
                    entry.source,
                    entry.updated_at,
                    entry.supersedes,
                ],
            )
            .map_err(map_sqlite_error)?;
        }
        tx.execute(
            "INSERT INTO knowledge_files (file, content_hash, indexed_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(file) DO UPDATE SET content_hash=excluded.content_hash, indexed_at=excluded.indexed_at",
            params![file, file_hash, Utc::now().to_rfc3339()],
        )
        .map_err(map_sqlite_error)?;
        tx.commit().map_err(map_sqlite_error)?;

        Ok(KnowledgeIndexRebuildResult {
            indexed: entries.len(),
            removed: previous_count.saturating_sub(entries.len()),
            unchanged_files: 0,
        })
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<KnowledgeIndexEntry>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        // Bound the number of repeated SQL placeholders for adversarially long
        // CJK queries. Each term appears twice in the predicate, so 400 terms
        // remain below SQLite's default variable limit.
        let mut cjk_bigrams = crate::fts::cjk_bigrams(query);
        cjk_bigrams.truncate(400);
        let conn = self
            .inner
            .lock()
            .map_err(|_| Error::Storage("Knowledge index lock poisoned".to_string()))?;
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = if cjk_bigrams.is_empty() {
            bind_values.push(Box::new(crate::fts::build_fts_query(query)));
            bind_values.push(Box::new(500_i64));
            "SELECT e.id, e.file, e.anchor, e.content, e.content_hash,
                        e.scope, e.source, e.updated_at, e.supersedes,
                        bm25(knowledge_fts) AS relevance
                 FROM knowledge_entries e
                 JOIN knowledge_fts f ON f.rowid = e.rowid
                 WHERE knowledge_fts MATCH ?1
                 ORDER BY relevance ASC
                 LIMIT ?2"
                .to_string()
        } else {
            let conditions = cjk_bigrams
                .iter()
                .enumerate()
                .map(|(offset, _)| format!("f.content LIKE ?{}", offset + 1))
                .collect::<Vec<_>>()
                .join(" OR ");
            let match_score = cjk_bigrams
                .iter()
                .enumerate()
                .map(|(offset, _)| {
                    format!("CASE WHEN f.content LIKE ?{} THEN 1 ELSE 0 END", offset + 1)
                })
                .collect::<Vec<_>>()
                .join(" + ");
            for term in &cjk_bigrams {
                bind_values.push(Box::new(format!("%{term}%")));
            }
            bind_values.push(Box::new(500_i64));
            format!(
                "SELECT e.id, e.file, e.anchor, e.content, e.content_hash,
                        e.scope, e.source, e.updated_at, e.supersedes,
                        -({match_score}) AS relevance
                 FROM knowledge_entries e
                 JOIN knowledge_fts f ON f.rowid = e.rowid
                 WHERE {conditions}
                 ORDER BY relevance ASC
                 LIMIT ?{}",
                cjk_bigrams.len() + 1
            )
        };
        let mut stmt = conn.prepare(&sql).map_err(map_sqlite_error)?;
        let bind_refs = bind_values
            .iter()
            .map(|value| value.as_ref())
            .collect::<Vec<_>>();
        let rows = stmt
            .query_map(bind_refs.as_slice(), |row| {
                Ok((
                    KnowledgeIndexEntry {
                        id: row.get(0)?,
                        file: row.get(1)?,
                        anchor: row.get(2)?,
                        content: row.get(3)?,
                        content_hash: row.get(4)?,
                        scope: row.get(5)?,
                        source: row.get(6)?,
                        updated_at: row.get(7)?,
                        supersedes: row.get(8)?,
                    },
                    row.get::<_, f64>(9)?,
                ))
            })
            .map_err(map_sqlite_error)?;
        let candidates = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        // superseded / forgotten 过滤限定到本次候选集（≤500），
        // 不再每次搜索都全量加载两张表。CJK 候选按命中数排序，保留
        // 部分命中结果，避免多字查询被错误收窄为“必须命中所有 bigram”。
        let mut superseded = HashSet::new();
        if !candidates.is_empty() {
            let placeholders = candidates
                .iter()
                .enumerate()
                .map(|(offset, _)| format!("?{}", offset + 1))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT supersedes FROM knowledge_entries WHERE supersedes IN ({placeholders})"
            );
            let params = candidates
                .iter()
                .map(|(entry, _)| entry.id.clone())
                .collect::<Vec<_>>();
            let mut stmt = conn.prepare(&sql).map_err(map_sqlite_error)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                    row.get::<_, String>(0)
                })
                .map_err(map_sqlite_error)?;
            for id in rows {
                superseded.insert(id.map_err(map_sqlite_error)?);
            }
        }
        let mut forgotten = HashSet::new();
        if !candidates.is_empty() {
            let placeholders = candidates
                .iter()
                .enumerate()
                .map(|(offset, _)| format!("?{}", offset + 1))
                .collect::<Vec<_>>()
                .join(",");
            let sql =
                format!("SELECT dedup_key FROM forgotten WHERE dedup_key IN ({placeholders})");
            let params = candidates
                .iter()
                .map(|(entry, _)| entry.content_hash.clone())
                .collect::<Vec<_>>();
            let mut stmt = conn.prepare(&sql).map_err(map_sqlite_error)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                    row.get::<_, String>(0)
                })
                .map_err(map_sqlite_error)?;
            for key in rows {
                forgotten.insert(key.map_err(map_sqlite_error)?);
            }
        }
        let mut by_hash: HashMap<String, (KnowledgeIndexEntry, f64)> = HashMap::new();
        for (entry, relevance) in candidates {
            if superseded.contains(&entry.id) || forgotten.contains(&entry.content_hash) {
                continue;
            }
            match by_hash.get(&entry.content_hash) {
                Some(existing)
                    if !entry_precedes((&entry, relevance), (&existing.0, existing.1)) => {}
                _ => {
                    by_hash.insert(entry.content_hash.clone(), (entry, relevance));
                }
            }
        }
        let mut results = by_hash.into_values().collect::<Vec<_>>();
        results.sort_by(|left, right| compare_ranked_entries(right, left));
        results.truncate(limit.min(100));
        Ok(results.into_iter().map(|(entry, _)| entry).collect())
    }

    pub fn record_forgotten_content(&self, content: &str, reason: &str) -> Result<String> {
        let dedup_key = knowledge_dedup_key(content);
        let conn = self
            .inner
            .lock()
            .map_err(|_| Error::Storage("Knowledge index lock poisoned".to_string()))?;
        conn.execute(
            "INSERT INTO forgotten (dedup_key, reason, forgotten_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(dedup_key) DO UPDATE SET
                reason = excluded.reason,
                forgotten_at = excluded.forgotten_at",
            params![dedup_key, reason, Utc::now().to_rfc3339()],
        )
        .map_err(map_sqlite_error)?;
        Ok(dedup_key)
    }

    /// Remove a newly prepared tombstone when the associated deletion fails.
    pub fn remove_forgotten_content(&self, content: &str) -> Result<()> {
        let conn = self
            .inner
            .lock()
            .map_err(|_| Error::Storage("Knowledge index lock poisoned".into()))?;
        conn.execute(
            "DELETE FROM forgotten WHERE dedup_key = ?1",
            params![knowledge_dedup_key(content)],
        )
        .map_err(map_sqlite_error)?;
        Ok(())
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<KnowledgeIndexEntry>> {
        let conn = self
            .inner
            .lock()
            .map_err(|_| Error::Storage("Knowledge index lock poisoned".to_string()))?;
        conn.query_row(
            "SELECT id, file, anchor, content, content_hash, scope, source, updated_at, supersedes
             FROM knowledge_entries WHERE id = ?1",
            params![id],
            |row| {
                Ok(KnowledgeIndexEntry {
                    id: row.get(0)?,
                    file: row.get(1)?,
                    anchor: row.get(2)?,
                    content: row.get(3)?,
                    content_hash: row.get(4)?,
                    scope: row.get(5)?,
                    source: row.get(6)?,
                    updated_at: row.get(7)?,
                    supersedes: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(map_sqlite_error)
    }

    pub fn is_forgotten_content(&self, content: &str) -> Result<bool> {
        let dedup_key = knowledge_dedup_key(content);
        let conn = self
            .inner
            .lock()
            .map_err(|_| Error::Storage("Knowledge index lock poisoned".to_string()))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM forgotten WHERE dedup_key = ?1",
                params![dedup_key],
                |row| row.get(0),
            )
            .map_err(map_sqlite_error)?;
        Ok(count > 0)
    }

    fn init_schema(&self) -> Result<()> {
        const KNOWLEDGE_SCHEMA_VERSION: i64 = 2;
        let conn = self
            .inner
            .lock()
            .map_err(|_| Error::Storage("Knowledge index lock poisoned".to_string()))?;
        // 版本戳门控：user_version >= 2 说明 FTS 已由本版本（或更新）创建，
        // 直接跳过 DDL 子串嗅探——后者对空格/引号变体不匹配会导致每次开库
        // DROP+重建。user_version < 2 的存量库仍走一次性嗅探迁移。
        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(map_sqlite_error)?;
        let rebuild_fts = if user_version >= KNOWLEDGE_SCHEMA_VERSION {
            false
        } else {
            crate::fts::prepare_trigram_fts(
                &conn,
                "knowledge_fts",
                &["knowledge_ai", "knowledge_ad", "knowledge_au"],
            )?
        };
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS knowledge_entries (
                id TEXT PRIMARY KEY,
                file TEXT NOT NULL,
                anchor TEXT NOT NULL,
                content TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                scope TEXT NOT NULL,
                source TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                supersedes TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_knowledge_file ON knowledge_entries(file);
            CREATE INDEX IF NOT EXISTS idx_knowledge_hash ON knowledge_entries(content_hash);
            CREATE INDEX IF NOT EXISTS idx_knowledge_supersedes ON knowledge_entries(supersedes);
            CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_fts USING fts5(
                content,
                file,
                anchor,
                content='knowledge_entries',
                content_rowid='rowid',
                tokenize='trigram'
            );
            CREATE TRIGGER IF NOT EXISTS knowledge_ai AFTER INSERT ON knowledge_entries BEGIN
                INSERT INTO knowledge_fts(rowid, content, file, anchor)
                VALUES (new.rowid, new.content, new.file, new.anchor);
            END;
            CREATE TRIGGER IF NOT EXISTS knowledge_ad AFTER DELETE ON knowledge_entries BEGIN
                INSERT INTO knowledge_fts(knowledge_fts, rowid, content, file, anchor)
                VALUES ('delete', old.rowid, old.content, old.file, old.anchor);
            END;
            CREATE TRIGGER IF NOT EXISTS knowledge_au AFTER UPDATE ON knowledge_entries BEGIN
                INSERT INTO knowledge_fts(knowledge_fts, rowid, content, file, anchor)
                VALUES ('delete', old.rowid, old.content, old.file, old.anchor);
                INSERT INTO knowledge_fts(rowid, content, file, anchor)
                VALUES (new.rowid, new.content, new.file, new.anchor);
            END;
            CREATE TABLE IF NOT EXISTS knowledge_files (
                file TEXT PRIMARY KEY,
                content_hash TEXT NOT NULL,
                indexed_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS knowledge_vectors (
                entry_id TEXT PRIMARY KEY,
                embedding BLOB NOT NULL,
                dimensions INTEGER NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY(entry_id) REFERENCES knowledge_entries(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS forgotten (
                dedup_key TEXT PRIMARY KEY,
                reason TEXT NOT NULL,
                forgotten_at TEXT NOT NULL
            );
            CREATE TRIGGER IF NOT EXISTS knowledge_vector_ad AFTER DELETE ON knowledge_entries BEGIN
                DELETE FROM knowledge_vectors WHERE entry_id = old.id;
            END;",
        )
        .map_err(map_sqlite_error)?;
        if rebuild_fts {
            conn.execute(
                "INSERT INTO knowledge_fts(knowledge_fts) VALUES('rebuild')",
                [],
            )
            .map_err(map_sqlite_error)?;
        }
        conn.execute_batch(&format!("PRAGMA user_version={KNOWLEDGE_SCHEMA_VERSION}"))
            .map_err(map_sqlite_error)?;
        Ok(())
    }
}

pub fn knowledge_dedup_key(content: &str) -> String {
    let mut semantic = content.trim().trim_start_matches('-').trim();
    while semantic.starts_with('[') {
        let Some(end) = semantic.find(']') else {
            break;
        };
        semantic = semantic[end + 1..].trim_start();
    }
    semantic = strip_migration_provenance(semantic);
    sha256_hex(
        semantic
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .as_bytes(),
    )
}

fn source_priority(source: &str) -> u8 {
    match source {
        "user_statement" => 3,
        "verified" => 2,
        "inferred" => 1,
        _ => 0,
    }
}

fn compare_ranked_entries(
    left: &(KnowledgeIndexEntry, f64),
    right: &(KnowledgeIndexEntry, f64),
) -> Ordering {
    source_priority(&left.0.source)
        .cmp(&source_priority(&right.0.source))
        .then_with(|| left.0.updated_at.cmp(&right.0.updated_at))
        .then_with(|| right.1.partial_cmp(&left.1).unwrap_or(Ordering::Equal))
        .then_with(|| right.0.id.cmp(&left.0.id))
}

fn entry_precedes(left: (&KnowledgeIndexEntry, f64), right: (&KnowledgeIndexEntry, f64)) -> bool {
    compare_ranked_entries(&(left.0.clone(), left.1), &(right.0.clone(), right.1))
        == Ordering::Greater
}

fn parse_entries(file: &str, content: &str, default_scope: &str) -> Vec<KnowledgeIndexEntry> {
    let mut entries = Vec::new();
    let mut anchor = "root".to_string();
    let mut paragraph = Vec::new();

    let flush_paragraph =
        |entries: &mut Vec<KnowledgeIndexEntry>, paragraph: &mut Vec<String>, anchor: &str| {
            let content = paragraph.join("\n").trim().to_string();
            paragraph.clear();
            if !content.is_empty() && content != "---" {
                entries.push(build_entry(file, anchor, &content, default_scope));
            }
        };

    for line in content.lines() {
        if line.starts_with('#') {
            flush_paragraph(&mut entries, &mut paragraph, &anchor);
            anchor = slugify(line.trim_start_matches('#').trim());
        } else if line.trim().is_empty() {
            flush_paragraph(&mut entries, &mut paragraph, &anchor);
        } else if line.trim_start().starts_with("- [") {
            flush_paragraph(&mut entries, &mut paragraph, &anchor);
            entries.push(build_entry(file, &anchor, line.trim(), default_scope));
        } else {
            paragraph.push(line.to_string());
        }
    }
    flush_paragraph(&mut entries, &mut paragraph, &anchor);
    entries
}

fn build_entry(file: &str, anchor: &str, raw: &str, default_scope: &str) -> KnowledgeIndexEntry {
    let mut metadata = HashMap::new();
    let mut content = raw.trim().trim_start_matches('-').trim().to_string();
    while content.starts_with('[') {
        let Some(end) = content.find(']') else {
            break;
        };
        let token = &content[1..end];
        if let Some((key, value)) = token.split_once(':') {
            metadata.insert(key.trim().to_string(), value.trim().to_string());
        }
        content = content[end + 1..].trim_start().to_string();
    }
    content = strip_migration_provenance(&content).to_string();
    let content_hash = knowledge_dedup_key(&content);
    let id = metadata.remove("id").unwrap_or_else(|| {
        format!(
            "legacy-{}",
            &sha256_hex(format!("{file}:{anchor}:{content}").as_bytes())[..16]
        )
    });
    KnowledgeIndexEntry {
        id,
        file: file.to_string(),
        anchor: if anchor.is_empty() {
            "root".to_string()
        } else {
            anchor.to_string()
        },
        content,
        content_hash,
        scope: metadata
            .remove("scope")
            .unwrap_or_else(|| default_scope.to_string()),
        source: metadata
            .remove("source")
            .unwrap_or_else(|| "verified".to_string()),
        updated_at: metadata
            .remove("updated")
            .unwrap_or_else(|| Utc::now().date_naive().to_string()),
        supersedes: metadata.remove("supersedes"),
    }
}

fn strip_migration_provenance(content: &str) -> &str {
    let trimmed = content.trim();
    if trimmed.ends_with("-->") {
        if let Some(start) = trimmed.rfind("<!-- migrated-from:") {
            return trimmed[..start].trim_end();
        }
    }
    trimmed
}

fn slugify(value: &str) -> String {
    let slug = value
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "root".to_string()
    } else {
        slug
    }
}

fn sha256_hex(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn map_sqlite_error(error: rusqlite::Error) -> Error {
    Error::Storage(format!("Knowledge index SQLite error: {error}"))
}
