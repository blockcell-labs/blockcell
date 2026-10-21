use std::path::PathBuf;
use std::sync::Mutex;

use blockcell_core::{Error, Result};
use chrono::Utc;
use rabitq_rs::{IvfRabitqIndex, Metric, RotatorType, SearchParams};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;

use crate::vector::{VectorFilter, VectorHit, VectorIndex, VectorMeta};

const DEFAULT_TOTAL_BITS: usize = 7;
const MIN_TRAIN_SIZE: usize = 32;
const DEFAULT_SEED: u64 = 42;

struct IndexState {
    cached_index: Option<IvfRabitqIndex>,
    dimensions: Option<usize>,
    dirty: bool,
    /// 写入世代：每次 mark_dirty 递增。rebuild 前后比较世代可区分
    /// "进入重建时的脏标记"与"训练期间的新写入"，避免把不完整索引误标为 clean。
    write_epoch: u64,
    /// 与 `cached_index` 同源同代的 id 顺序：第 i 个向量对应的行 id。
    ///
    /// 无 filter 的检索只需要这张映射把 IVF 命中下标翻译成 id，不必把整张表
    /// 的向量读出来再 JSON 解码（P3-138）。它只在 `dirty == false` 时有效，
    /// 任何写入都会随 `mark_dirty` 一起清空。
    id_order: Option<Vec<String>>,
}

struct StorageLayout {
    table_name: String,
    db_path: PathBuf,
    index_path: PathBuf,
}

#[derive(Debug)]
struct StoredVector {
    id: String,
    vector: Vec<f32>,
    dimension: usize,
    meta: VectorMeta,
}

pub struct RabitqIndex {
    db: Mutex<Connection>,
    layout: StorageLayout,
    state: Mutex<IndexState>,
}

impl RabitqIndex {
    pub fn open_or_create(uri: &str, table_name: &str) -> Result<Self> {
        let layout = resolve_layout(uri, table_name);
        ensure_parent_dirs(&layout)?;

        let conn = Connection::open(&layout.db_path).map_err(map_sqlite_error)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .map_err(map_sqlite_error)?;
        init_schema(&conn, &layout.table_name)?;

        let dimensions = load_dimensions(&conn, &layout.table_name)?;
        let vector_count = count_vectors(&conn, &layout.table_name)?;
        if vector_count == 0 && layout.index_path.exists() {
            std::fs::remove_file(&layout.index_path).map_err(map_io_error)?;
        }
        // The SQLite rows are the source of truth. A previous process may have
        // committed row changes and exited before rebuilding index.bin, so the
        // mere presence of that file is not proof that it matches the rows.
        // Rebuild lazily on the first search after every non-empty reopen.
        let dirty = vector_count > 0;

        Ok(Self {
            db: Mutex::new(conn),
            layout,
            state: Mutex::new(IndexState {
                cached_index: None,
                dimensions,
                dirty,
                write_epoch: 0,
                id_order: None,
            }),
        })
    }

    fn load_rows(&self) -> Result<Vec<StoredVector>> {
        let conn = self
            .db
            .lock()
            .map_err(|e| Error::Storage(format!("RaBitQ database lock error: {}", e)))?;
        load_rows_from_conn(&conn, &self.layout.table_name)
    }

    fn vector_count(&self) -> Result<usize> {
        let conn = self
            .db
            .lock()
            .map_err(|e| Error::Storage(format!("RaBitQ database lock error: {}", e)))?;
        count_vectors(&conn, &self.layout.table_name)
    }

    fn mark_dirty(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| Error::Storage(format!("RaBitQ state lock error: {}", e)))?;
        state.dirty = true;
        state.cached_index = None;
        state.id_order = None;
        state.write_epoch = state.write_epoch.wrapping_add(1);
        Ok(())
    }

    /// 提交重建结果。`epoch` 为重建开始前捕获的写入世代：
    /// 若训练期间有新写入（write_epoch 递增），索引不完整——仍装入缓存
    /// （好过无缓存），但保留 dirty 让下一次 ensure_index_ready 再重建，
    /// 避免把缺失新向量的索引误标为 clean。
    ///
    /// `id_order` 必须与 `index` 同源（同一次 `rows` 快照）；传 `None` 表示
    /// 这次提交没有可用的 id 映射，后续检索会退回全表读取。
    fn store_clean_index(
        &self,
        index: Option<IvfRabitqIndex>,
        dimensions: Option<usize>,
        id_order: Option<Vec<String>>,
        epoch: u64,
    ) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| Error::Storage(format!("RaBitQ state lock error: {}", e)))?;
        state.cached_index = index;
        state.dimensions = dimensions;
        state.id_order = id_order;
        state.dirty = state.write_epoch != epoch;
        Ok(())
    }

    /// 只登记 id 映射，不改动 dirty/维度。用于"索引已经就绪、这次顺手补上
    /// 映射"的路径；世代不匹配（期间有写入）时丢弃，下一次检索重建即可。
    fn store_id_order(&self, ids: Vec<String>, epoch: u64) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| Error::Storage(format!("RaBitQ state lock error: {}", e)))?;
        if state.write_epoch == epoch {
            state.id_order = Some(ids);
        }
        Ok(())
    }

    fn current_dimensions(&self) -> Result<Option<usize>> {
        let state = self
            .state
            .lock()
            .map_err(|e| Error::Storage(format!("RaBitQ state lock error: {}", e)))?;
        Ok(state.dimensions)
    }

    fn set_dimensions_if_empty(&self, dimensions: usize) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| Error::Storage(format!("RaBitQ state lock error: {}", e)))?;
        if state.dimensions.is_none() {
            state.dimensions = Some(dimensions);
        }
        Ok(())
    }

    fn ensure_query_dimensions(&self, query_dim: usize) -> Result<()> {
        match self.current_dimensions()? {
            Some(expected) if expected != query_dim => Err(Error::Storage(format!(
                "Vector dimension mismatch: expected {}, got {}",
                expected, query_dim
            ))),
            Some(_) => Ok(()),
            None => Ok(()),
        }
    }

    /// `epoch` 必须由调用方在加载 `rows` 快照之前捕获（见 search 中的注释）。
    fn ensure_index_ready(&self, rows: &[StoredVector], epoch: u64) -> Result<bool> {
        if rows.is_empty() {
            self.clear_index_file_if_exists()?;
            self.store_clean_index(None, None, None, epoch)?;
            return Ok(true);
        }

        let dimensions = rows[0].dimension;
        if rows
            .iter()
            .any(|row| row.dimension != dimensions || row.vector.len() != dimensions)
        {
            return Err(Error::Storage(
                "Stored vectors have inconsistent dimensions".to_string(),
            ));
        }

        self.set_dimensions_if_empty(dimensions)?;

        let dirty = {
            let state = self
                .state
                .lock()
                .map_err(|e| Error::Storage(format!("RaBitQ state lock error: {}", e)))?;
            state.dirty
        };

        if rows.len() < MIN_TRAIN_SIZE {
            if dirty {
                self.clear_index_file_if_exists()?;
                self.store_clean_index(None, Some(dimensions), None, epoch)?;
            }
            return Ok(true);
        }

        if !dirty {
            let mut state = self
                .state
                .lock()
                .map_err(|e| Error::Storage(format!("RaBitQ state lock error: {}", e)))?;
            if state.cached_index.is_none() {
                if self.layout.index_path.exists() {
                    match IvfRabitqIndex::load_from_path(&self.layout.index_path) {
                        Ok(index) => {
                            state.cached_index = Some(index);
                            // The on-disk index was trained on the rowid order of
                            // this very table, and `dirty == false` means no write
                            // happened since, so `rows` is that same order.
                            state.id_order =
                                Some(rows.iter().map(|row| row.id.clone()).collect());
                        }
                        Err(_) => {
                            drop(state);
                            if self.rebuild_index(rows, dimensions, epoch).is_ok() {
                                return Ok(false);
                            }
                            return Ok(true);
                        }
                    }
                } else {
                    drop(state);
                    if self.rebuild_index(rows, dimensions, epoch).is_ok() {
                        return Ok(false);
                    }
                    return Ok(true);
                }
            }
            return Ok(false);
        }

        if self.rebuild_index(rows, dimensions, epoch).is_ok() {
            Ok(false)
        } else {
            Ok(true)
        }
    }

    fn rebuild_index(&self, rows: &[StoredVector], dimensions: usize, epoch: u64) -> Result<()> {
        if rows.is_empty() {
            self.clear_index_file_if_exists()?;
            self.store_clean_index(None, None, None, epoch)?;
            return Ok(());
        }

        let dataset: Vec<Vec<f32>> = rows.iter().map(|row| row.vector.clone()).collect();
        let nlist = choose_nlist(dataset.len());
        let use_faster_config = dataset.len() > 100_000;

        let index = IvfRabitqIndex::train(
            &dataset,
            nlist,
            DEFAULT_TOTAL_BITS,
            Metric::L2,
            RotatorType::FhtKacRotator,
            DEFAULT_SEED,
            use_faster_config,
        )
        .map_err(|error| Error::Storage(format!("RaBitQ training failed: {}", error)))?;

        index
            .save_to_path(&self.layout.index_path)
            .map_err(|error| Error::Storage(format!("RaBitQ index save failed: {}", error)))?;

        self.store_clean_index(
            Some(index),
            Some(dimensions),
            Some(rows.iter().map(|row| row.id.clone()).collect()),
            epoch,
        )
    }

    fn clear_index_file_if_exists(&self) -> Result<()> {
        if self.layout.index_path.exists() {
            std::fs::remove_file(&self.layout.index_path).map_err(map_io_error)?;
        }
        Ok(())
    }

    fn exact_search(
        &self,
        rows: &[StoredVector],
        vector: &[f32],
        top_k: usize,
    ) -> Result<Vec<VectorHit>> {
        let mut hits = Vec::with_capacity(rows.len());
        for row in rows {
            let distance = l2_distance(&row.vector, vector)?;
            hits.push(VectorHit {
                id: row.id.clone(),
                score: -distance,
            });
        }

        hits.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(top_k);
        Ok(hits)
    }

    fn current_write_epoch(&self) -> Result<u64> {
        let state = self
            .state
            .lock()
            .map_err(|e| Error::Storage(format!("RaBitQ state lock error: {}", e)))?;
        Ok(state.write_epoch)
    }

    /// Unfiltered query against the cached index, without touching the rows.
    ///
    /// Returns `None` whenever the cache cannot be trusted (missing, dirty, or
    /// a different write epoch), so the caller falls back to the full path. The
    /// epoch is captured before the lock is taken: a write in that window bumps
    /// `write_epoch` and makes the check below fail closed.
    fn search_cached_index(
        &self,
        vector: &[f32],
        top_k: usize,
        epoch: u64,
    ) -> Result<Option<Vec<VectorHit>>> {
        // Lock ordering: dimension check first (it takes the state lock), then
        // the long-lived lock below. `std::sync::Mutex` is not reentrant.
        self.ensure_query_dimensions(vector.len())?;

        let state = self
            .state
            .lock()
            .map_err(|e| Error::Storage(format!("RaBitQ state lock error: {}", e)))?;
        if state.dirty || state.write_epoch != epoch {
            return Ok(None);
        }
        let (Some(index), Some(id_order)) = (state.cached_index.as_ref(), state.id_order.as_ref())
        else {
            return Ok(None);
        };

        let probe = choose_probe(id_order.len(), top_k);
        let results = index
            .search(vector, SearchParams::new(top_k, probe))
            .map_err(|error| Error::Storage(format!("RaBitQ search failed: {}", error)))?;

        let mut hits = Vec::with_capacity(results.len());
        for result in results {
            let Some(id) = id_order.get(result.id) else {
                // Index and mapping disagree — do not guess, rebuild instead.
                return Ok(None);
            };
            hits.push(VectorHit {
                id: id.clone(),
                score: -(result.score as f64),
            });
        }
        Ok(Some(hits))
    }
}

impl VectorIndex for RabitqIndex {
    fn upsert(&self, id: &str, vector: &[f32], meta: &VectorMeta) -> Result<()> {
        if vector.is_empty() {
            return Err(Error::Storage("Vector must not be empty".to_string()));
        }

        self.ensure_query_dimensions(vector.len())?;
        self.set_dimensions_if_empty(vector.len())?;

        let vector_blob = serde_json::to_vec(vector)
            .map_err(|error| Error::Storage(format!("Failed to serialize vector: {}", error)))?;
        let now = Utc::now().to_rfc3339();
        let table = self.layout.table_name.clone();
        let sql = format!(
            "INSERT INTO \"{}\" (id, vector, scope, item_type, tags, session_key, dimension, updated_at)\n             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)\n             ON CONFLICT(id) DO UPDATE SET\n                vector = excluded.vector,\n                scope = excluded.scope,\n                item_type = excluded.item_type,\n                tags = excluded.tags,\n                session_key = excluded.session_key,\n                dimension = excluded.dimension,\n                updated_at = excluded.updated_at",
            table
        );

        let conn = self
            .db
            .lock()
            .map_err(|e| Error::Storage(format!("RaBitQ database lock error: {}", e)))?;
        conn.execute(
            &sql,
            params![
                id,
                vector_blob,
                meta.scope.clone(),
                meta.item_type.clone(),
                meta.tags.join(","),
                meta.session_key.clone(),
                vector.len() as i64,
                now,
            ],
        )
        .map_err(map_sqlite_error)?;

        self.mark_dirty()?;
        Ok(())
    }

    fn delete_ids(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let table = self.layout.table_name.clone();
        let sql = format!("DELETE FROM \"{}\" WHERE id = ?1", table);
        let mut conn = self
            .db
            .lock()
            .map_err(|e| Error::Storage(format!("RaBitQ database lock error: {}", e)))?;
        let tx = conn.transaction().map_err(map_sqlite_error)?;
        {
            let mut stmt = tx.prepare(&sql).map_err(map_sqlite_error)?;
            for id in ids {
                stmt.execute(params![id]).map_err(map_sqlite_error)?;
            }
        }
        tx.commit().map_err(map_sqlite_error)?;

        self.mark_dirty()?;
        Ok(())
    }

    fn search(
        &self,
        vector: &[f32],
        top_k: usize,
        filter: Option<&VectorFilter>,
    ) -> Result<Vec<VectorHit>> {
        if top_k == 0 {
            return Ok(Vec::new());
        }
        if vector.is_empty() {
            return Err(Error::Storage("Query vector must not be empty".to_string()));
        }

        // epoch 必须先于 load_rows 捕获：upsert 是先写行后 mark_dirty。
        // 若先读行再读 epoch，落在两者之间的写入会让"陈旧行快照"配"新 epoch"
        // 在重建提交时被误标 clean，消费掉本该保留的 dirty 标记。
        let epoch = self.current_write_epoch()?;

        // Fast path: a clean cached index plus its id mapping is everything an
        // unfiltered query needs. Skipping `load_rows` here avoids reading and
        // JSON-decoding every stored vector on each search (P3-138).
        if filter.is_none() {
            if let Some(hits) = self.search_cached_index(vector, top_k, epoch)? {
                return Ok(hits);
            }
        }

        let mut rows = self.load_rows()?;
        if let Some(filter) = filter {
            rows.retain(|row| filter.matches(&row.meta));
        }
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        self.ensure_query_dimensions(vector.len())?;
        if filter.is_some() {
            return self.exact_search(&rows, vector, top_k);
        }
        let exact_only = self.ensure_index_ready(&rows, epoch)?;
        if exact_only {
            return self.exact_search(&rows, vector, top_k);
        }

        let id_order: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
        // Warm the fast path for subsequent queries. `epoch` guards against a
        // write landing between the row snapshot and this call.
        self.store_id_order(id_order.clone(), epoch)?;

        let state = self
            .state
            .lock()
            .map_err(|e| Error::Storage(format!("RaBitQ state lock error: {}", e)))?;
        let Some(index) = state.cached_index.as_ref() else {
            return self.exact_search(&rows, vector, top_k);
        };

        let probe = choose_probe(rows.len(), top_k);
        let results = index
            .search(vector, SearchParams::new(top_k, probe))
            .map_err(|error| Error::Storage(format!("RaBitQ search failed: {}", error)))?;

        let mut hits = Vec::with_capacity(results.len());
        for result in results {
            let idx = result.id;
            let Some(id) = id_order.get(idx) else {
                return self.exact_search(&rows, vector, top_k);
            };
            hits.push(VectorHit {
                id: id.clone(),
                score: -(result.score as f64),
            });
        }
        Ok(hits)
    }

    fn health(&self) -> Result<()> {
        let _ = self.load_rows()?;
        Ok(())
    }

    fn stats(&self) -> Result<serde_json::Value> {
        let count = self.vector_count()?;
        let state = self
            .state
            .lock()
            .map_err(|e| Error::Storage(format!("RaBitQ state lock error: {}", e)))?;
        Ok(json!({
            "backend": "rabitq",
            "table": self.layout.table_name,
            "index_path": self.layout.index_path.display().to_string(),
            "vectors": count,
            "dirty": state.dirty,
            "dimensions": state.dimensions,
            // 无 filter 的检索能否走缓存快路径（P3-138）。
            "id_order_cached": state.id_order.is_some(),
        }))
    }

    fn reset(&self) -> Result<()> {
        let table = self.layout.table_name.clone();
        let sql = format!("DELETE FROM \"{}\"", table);
        {
            let conn = self
                .db
                .lock()
                .map_err(|e| Error::Storage(format!("RaBitQ database lock error: {}", e)))?;
            conn.execute(&sql, []).map_err(map_sqlite_error)?;
        }

        self.clear_index_file_if_exists()?;
        let epoch = self.current_write_epoch()?;
        self.store_clean_index(None, None, None, epoch)?;
        Ok(())
    }
}

fn resolve_layout(uri: &str, table_name: &str) -> StorageLayout {
    let base = PathBuf::from(uri);
    let table_name = sanitize_identifier(table_name);

    if base.exists() && base.is_dir() {
        return StorageLayout {
            table_name,
            db_path: base.join("vectors.sqlite"),
            index_path: base.join("index.bin"),
        };
    }

    if base.extension().is_some() {
        return StorageLayout {
            table_name,
            db_path: base.with_extension("sqlite"),
            index_path: base,
        };
    }

    StorageLayout {
        table_name,
        db_path: base.join("vectors.sqlite"),
        index_path: base.join("index.bin"),
    }
}

fn ensure_parent_dirs(layout: &StorageLayout) -> Result<()> {
    if let Some(parent) = layout.db_path.parent() {
        std::fs::create_dir_all(parent).map_err(map_io_error)?;
    }
    if let Some(parent) = layout.index_path.parent() {
        std::fs::create_dir_all(parent).map_err(map_io_error)?;
    }
    Ok(())
}

fn sanitize_identifier(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect();
    if sanitized.is_empty() {
        "rabitq_vectors".to_string()
    } else {
        sanitized
    }
}

fn init_schema(conn: &Connection, table_name: &str) -> Result<()> {
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS \"{}\" (\n            id TEXT PRIMARY KEY,\n            vector BLOB NOT NULL,\n            scope TEXT NOT NULL DEFAULT '',\n            item_type TEXT NOT NULL DEFAULT '',\n            tags TEXT NOT NULL DEFAULT '',\n            session_key TEXT,\n            dimension INTEGER NOT NULL,\n            updated_at TEXT NOT NULL\n        )",
        table_name
    );
    conn.execute_batch(&sql).map_err(map_sqlite_error)?;
    let columns_sql = format!("PRAGMA table_info(\"{}\")", table_name);
    let mut stmt = conn.prepare(&columns_sql).map_err(map_sqlite_error)?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(map_sqlite_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    drop(stmt);
    if !columns.iter().any(|column| column == "session_key") {
        conn.execute(
            &format!("ALTER TABLE \"{}\" ADD COLUMN session_key TEXT", table_name),
            [],
        )
        .map_err(map_sqlite_error)?;
    }
    Ok(())
}

fn load_dimensions(conn: &Connection, table_name: &str) -> Result<Option<usize>> {
    let sql = format!(
        "SELECT dimension FROM \"{}\" ORDER BY rowid ASC LIMIT 1",
        table_name
    );
    let dimension = conn
        .query_row(&sql, [], |row| row.get::<_, i64>(0))
        .optional()
        .map_err(map_sqlite_error)?
        .map(|value| value as usize);
    Ok(dimension)
}

fn count_vectors(conn: &Connection, table_name: &str) -> Result<usize> {
    let sql = format!("SELECT COUNT(*) FROM \"{}\"", table_name);
    let count = conn
        .query_row(&sql, [], |row| row.get::<_, i64>(0))
        .map_err(map_sqlite_error)?;
    Ok(count.max(0) as usize)
}

fn load_rows_from_conn(conn: &Connection, table_name: &str) -> Result<Vec<StoredVector>> {
    let sql = format!(
        "SELECT id, vector, dimension, scope, item_type, tags, session_key FROM \"{}\" ORDER BY rowid ASC",
        table_name
    );
    let mut stmt = conn.prepare(&sql).map_err(map_sqlite_error)?;
    let mut rows = stmt.query([]).map_err(map_sqlite_error)?;
    let mut items = Vec::new();

    while let Some(row) = rows.next().map_err(map_sqlite_error)? {
        let id: String = row.get(0).map_err(map_sqlite_error)?;
        let vector_blob: Vec<u8> = row.get(1).map_err(map_sqlite_error)?;
        let dimension = row.get::<_, i64>(2).map_err(map_sqlite_error)? as usize;
        let scope: String = row.get(3).map_err(map_sqlite_error)?;
        let item_type: String = row.get(4).map_err(map_sqlite_error)?;
        let tags: String = row.get(5).map_err(map_sqlite_error)?;
        let session_key: Option<String> = row.get(6).map_err(map_sqlite_error)?;
        let vector: Vec<f32> = serde_json::from_slice(&vector_blob)
            .map_err(|error| Error::Storage(format!("Failed to decode vector blob: {}", error)))?;
        if vector.len() != dimension {
            return Err(Error::Storage(format!(
                "Stored vector dimension mismatch for id {}: expected {}, got {}",
                id,
                dimension,
                vector.len()
            )));
        }
        items.push(StoredVector {
            id,
            vector,
            dimension,
            meta: VectorMeta {
                scope,
                item_type,
                tags: tags
                    .split(',')
                    .filter(|tag| !tag.is_empty())
                    .map(str::to_string)
                    .collect(),
                session_key,
            },
        });
    }

    Ok(items)
}

fn choose_nlist(count: usize) -> usize {
    let base = count.saturating_div(4).max(1);
    base.min(256)
}

fn choose_probe(count: usize, top_k: usize) -> usize {
    let nlist = choose_nlist(count);
    top_k.saturating_mul(4).clamp(1, 64).min(nlist.max(1))
}

fn l2_distance(left: &[f32], right: &[f32]) -> Result<f64> {
    if left.len() != right.len() {
        return Err(Error::Storage(format!(
            "Vector dimension mismatch: expected {}, got {}",
            left.len(),
            right.len()
        )));
    }

    let sum = left
        .iter()
        .zip(right.iter())
        .map(|(a, b)| {
            let delta = f64::from(*a) - f64::from(*b);
            delta * delta
        })
        .sum::<f64>();
    Ok(sum.sqrt())
}

fn map_sqlite_error(error: rusqlite::Error) -> Error {
    Error::Storage(format!("RaBitQ sqlite error: {}", error))
}

fn map_io_error(error: std::io::Error) -> Error {
    Error::Storage(format!("RaBitQ IO error: {}", error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_rabitq_index_smoke() {
        let dir = TempDir::new().unwrap();
        let uri = dir.path().join("vectors.rabitq");
        let index = RabitqIndex::open_or_create(uri.to_str().unwrap(), "memory_vectors").unwrap();

        let meta = VectorMeta {
            scope: "long_term".to_string(),
            item_type: "fact".to_string(),
            tags: vec!["vector".to_string()],
            session_key: None,
        };

        for i in 0..48 {
            let value = i as f32;
            let vector = vec![value, value + 0.1, value + 0.2];
            index
                .upsert(&format!("memory-{}", i), &vector, &meta)
                .unwrap();
        }

        let hits = index.search(&[1.0, 1.1, 1.2], 5, None).unwrap();
        assert!(!hits.is_empty());

        index.delete_ids(&["memory-1".to_string()]).unwrap();
        let stats = index.stats().unwrap();
        assert_eq!(stats["backend"], "rabitq");

        index.reset().unwrap();
        let hits = index.search(&[1.0, 1.1, 1.2], 5, None).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn rabitq_rebuilds_after_reopen_when_rows_changed() {
        let dir = TempDir::new().unwrap();
        let uri = dir.path().join("vectors.rabitq");
        let meta = VectorMeta {
            scope: "long_term".to_string(),
            item_type: "fact".to_string(),
            tags: vec!["vector".to_string()],
            session_key: None,
        };

        {
            let index =
                RabitqIndex::open_or_create(uri.to_str().unwrap(), "memory_vectors").unwrap();
            for i in 0..48 {
                let value = i as f32;
                index
                    .upsert(
                        &format!("memory-{i}"),
                        &[value, value + 0.1, value + 0.2],
                        &meta,
                    )
                    .unwrap();
            }

            index.search(&[1.0, 1.1, 1.2], 5, None).unwrap();
            index
                .upsert("memory-0", &[1000.0, 1000.1, 1000.2], &meta)
                .unwrap();
        }

        let reopened =
            RabitqIndex::open_or_create(uri.to_str().unwrap(), "memory_vectors").unwrap();
        let hits = reopened.search(&[1000.0, 1000.1, 1000.2], 1, None).unwrap();
        assert_eq!(hits.first().map(|hit| hit.id.as_str()), Some("memory-0"));
    }

    #[test]
    fn stale_rebuild_commit_preserves_dirty_flag() {
        // 重建期间有新写入时，旧 epoch 的提交不能把 dirty 清掉——
        // 否则不含新向量的索引会被误标为 clean，最新向量对搜索不可见。
        let dir = TempDir::new().unwrap();
        let uri = dir.path().join("vectors.rabitq");
        let index = RabitqIndex::open_or_create(uri.to_str().unwrap(), "memory_vectors").unwrap();

        let epoch = index.current_write_epoch().unwrap();
        index.store_clean_index(None, None, None, epoch).unwrap();
        assert!(!index.state.lock().unwrap().dirty);

        // 模拟训练期间的并发 upsert：mark_dirty 递增 epoch 后，
        // 用重建前的旧 epoch 提交结果，dirty 必须保留。
        let epoch = index.current_write_epoch().unwrap();
        index.mark_dirty().unwrap();
        index.store_clean_index(None, None, None, epoch).unwrap();
        assert!(index.state.lock().unwrap().dirty);
    }

    #[test]
    fn rabitq_filters_session_before_result_limit() {
        let dir = TempDir::new().unwrap();
        let uri = dir.path().join("vectors.rabitq");
        let index = RabitqIndex::open_or_create(uri.to_str().unwrap(), "memory_vectors").unwrap();
        for (id, session_key, value) in [
            ("session-b", Some("ws:b"), 0.0_f32),
            ("session-a", Some("ws:a"), 10.0_f32),
            ("global", None, 20.0_f32),
        ] {
            index
                .upsert(
                    id,
                    &[value, value, value],
                    &VectorMeta {
                        scope: "long_term".to_string(),
                        item_type: "fact".to_string(),
                        tags: vec![],
                        session_key: session_key.map(str::to_string),
                    },
                )
                .unwrap();
        }

        let hits = index
            .search(
                &[0.0, 0.0, 0.0],
                2,
                Some(&VectorFilter {
                    session_key: Some("ws:a".to_string()),
                    ..Default::default()
                }),
            )
            .unwrap();
        let ids: Vec<&str> = hits.iter().map(|hit| hit.id.as_str()).collect();
        assert_eq!(ids, vec!["session-a", "global"]);
    }

    fn seeded_index(dir: &TempDir) -> RabitqIndex {
        let uri = dir.path().join("vectors.rabitq");
        let index = RabitqIndex::open_or_create(uri.to_str().unwrap(), "memory_vectors").unwrap();
        let meta = VectorMeta {
            scope: "long_term".to_string(),
            item_type: "fact".to_string(),
            tags: vec!["vector".to_string()],
            session_key: None,
        };
        // Above MIN_TRAIN_SIZE so the IVF path (not exact search) is exercised.
        for i in 0..48 {
            let value = i as f32;
            index
                .upsert(&format!("memory-{i}"), &[value, value + 0.1, value + 0.2], &meta)
                .unwrap();
        }
        index
    }

    /// P3-138: an unfiltered query must be answerable from the cached id mapping
    /// alone. Deleting the rows behind the index's back proves the second search
    /// did not read them — if it had, the result would be empty.
    #[test]
    fn rabitq_unfiltered_search_reuses_cached_id_order() {
        let dir = TempDir::new().unwrap();
        let index = seeded_index(&dir);

        let query = [1.0, 1.1, 1.2];
        let first = index.search(&query, 3, None).unwrap();
        assert!(!first.is_empty());
        assert!(
            index.state.lock().unwrap().id_order.is_some(),
            "first search must warm the id mapping"
        );
        assert_eq!(
            index.stats().unwrap()["id_order_cached"],
            serde_json::Value::Bool(true)
        );

        // Bypass mark_dirty on purpose: the cache must not depend on the rows.
        {
            let conn = index.db.lock().unwrap();
            conn.execute("DELETE FROM memory_vectors", []).unwrap();
        }

        let second = index.search(&query, 3, None).unwrap();
        let first_ids: Vec<&str> = first.iter().map(|hit| hit.id.as_str()).collect();
        let second_ids: Vec<&str> = second.iter().map(|hit| hit.id.as_str()).collect();
        assert_eq!(
            second_ids, first_ids,
            "warm cache must answer without reading rows"
        );
    }

    /// The cache must never outlive a write: a new vector has to be visible on
    /// the very next search, which means `mark_dirty` clears the mapping.
    #[test]
    fn rabitq_write_invalidates_cached_id_order() {
        let dir = TempDir::new().unwrap();
        let index = seeded_index(&dir);

        let _ = index.search(&[1.0, 1.1, 1.2], 3, None).unwrap();
        assert!(index.state.lock().unwrap().id_order.is_some());

        index
            .upsert(
                "memory-new",
                &[1000.0, 1000.1, 1000.2],
                &VectorMeta {
                    scope: "long_term".to_string(),
                    item_type: "fact".to_string(),
                    tags: Vec::new(),
                    session_key: None,
                },
            )
            .unwrap();
        assert!(
            index.state.lock().unwrap().id_order.is_none(),
            "a write must invalidate the cached mapping"
        );

        let hits = index.search(&[1000.0, 1000.1, 1000.2], 1, None).unwrap();
        assert_eq!(hits.first().map(|hit| hit.id.as_str()), Some("memory-new"));
    }
}
