use std::path::PathBuf;
use std::sync::Mutex;

use blockcell_core::{Error, Result};
use chrono::Utc;
use rabitq_rs::{IvfRabitqIndex, Metric, RotatorType, SearchParams};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;

use crate::vector::{VectorHit, VectorIndex, VectorMeta};

const DEFAULT_TOTAL_BITS: usize = 7;
const MIN_TRAIN_SIZE: usize = 32;
const DEFAULT_SEED: u64 = 42;

struct IndexState {
    cached_index: Option<IvfRabitqIndex>,
    dimensions: Option<usize>,
    dirty: bool,
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
        init_schema(&conn, &layout.table_name)?;

        let dimensions = load_dimensions(&conn, &layout.table_name)?;
        let vector_count = count_vectors(&conn, &layout.table_name)?;
        if vector_count == 0 && layout.index_path.exists() {
            std::fs::remove_file(&layout.index_path).map_err(map_io_error)?;
        }
        let dirty = vector_count > 0 && !layout.index_path.exists();

        Ok(Self {
            db: Mutex::new(conn),
            layout,
            state: Mutex::new(IndexState {
                cached_index: None,
                dimensions,
                dirty,
            }),
        })
    }

    fn load_rows(&self) -> Result<Vec<StoredVector>> {
        let conn = self.db.lock().map_err(|e| {
            Error::Storage(format!("RaBitQ database lock error: {}", e))
        })?;
        load_rows_from_conn(&conn, &self.layout.table_name)
    }

    fn vector_count(&self) -> Result<usize> {
        let conn = self.db.lock().map_err(|e| {
            Error::Storage(format!("RaBitQ database lock error: {}", e))
        })?;
        count_vectors(&conn, &self.layout.table_name)
    }

    fn mark_dirty(&self) -> Result<()> {
        let mut state = self.state.lock().map_err(|e| {
            Error::Storage(format!("RaBitQ state lock error: {}", e))
        })?;
        state.dirty = true;
        state.cached_index = None;
        Ok(())
    }

    fn store_clean_index(
        &self,
        index: Option<IvfRabitqIndex>,
        dimensions: Option<usize>,
    ) -> Result<()> {
        let mut state = self.state.lock().map_err(|e| {
            Error::Storage(format!("RaBitQ state lock error: {}", e))
        })?;
        state.cached_index = index;
        state.dimensions = dimensions;
        state.dirty = false;
        Ok(())
    }

    fn current_dimensions(&self) -> Result<Option<usize>> {
        let state = self.state.lock().map_err(|e| {
            Error::Storage(format!("RaBitQ state lock error: {}", e))
        })?;
        Ok(state.dimensions)
    }

    fn set_dimensions_if_empty(&self, dimensions: usize) -> Result<()> {
        let mut state = self.state.lock().map_err(|e| {
            Error::Storage(format!("RaBitQ state lock error: {}", e))
        })?;
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

    fn ensure_index_ready(&self, rows: &[StoredVector]) -> Result<bool> {
        if rows.is_empty() {
            self.clear_index_file_if_exists()?;
            self.store_clean_index(None, None)?;
            return Ok(true);
        }

        let dimensions = rows[0].dimension;
        if rows.iter().any(|row| row.dimension != dimensions || row.vector.len() != dimensions) {
            return Err(Error::Storage(
                "Stored vectors have inconsistent dimensions".to_string(),
            ));
        }

        self.set_dimensions_if_empty(dimensions)?;

        let dirty = {
            let state = self.state.lock().map_err(|e| {
                Error::Storage(format!("RaBitQ state lock error: {}", e))
            })?;
            state.dirty
        };

        if rows.len() < MIN_TRAIN_SIZE {
            if dirty {
                self.clear_index_file_if_exists()?;
                self.store_clean_index(None, Some(dimensions))?;
            }
            return Ok(true);
        }

        if !dirty {
            let mut state = self.state.lock().map_err(|e| {
                Error::Storage(format!("RaBitQ state lock error: {}", e))
            })?;
            if state.cached_index.is_none() {
                if self.layout.index_path.exists() {
                    match IvfRabitqIndex::load_from_path(&self.layout.index_path) {
                        Ok(index) => {
                            state.cached_index = Some(index);
                        }
                        Err(_) => {
                            drop(state);
                            if self.rebuild_index(rows, dimensions).is_ok() {
                                return Ok(false);
                            }
                            return Ok(true);
                        }
                    }
                } else {
                    drop(state);
                    if self.rebuild_index(rows, dimensions).is_ok() {
                        return Ok(false);
                    }
                    return Ok(true);
                }
            }
            return Ok(false);
        }

        if self.rebuild_index(rows, dimensions).is_ok() {
            Ok(false)
        } else {
            Ok(true)
        }
    }

    fn rebuild_index(&self, rows: &[StoredVector], dimensions: usize) -> Result<()> {
        if rows.is_empty() {
            self.clear_index_file_if_exists()?;
            self.store_clean_index(None, None)?;
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

        let mut state = self.state.lock().map_err(|e| {
            Error::Storage(format!("RaBitQ state lock error: {}", e))
        })?;
        state.cached_index = Some(index);
        state.dimensions = Some(dimensions);
        state.dirty = false;
        Ok(())
    }

    fn clear_index_file_if_exists(&self) -> Result<()> {
        if self.layout.index_path.exists() {
            std::fs::remove_file(&self.layout.index_path).map_err(map_io_error)?;
        }
        Ok(())
    }

    fn exact_search(&self, rows: &[StoredVector], vector: &[f32], top_k: usize) -> Result<Vec<VectorHit>> {
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
            "INSERT INTO \"{}\" (id, vector, scope, item_type, tags, dimension, updated_at)\n             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)\n             ON CONFLICT(id) DO UPDATE SET\n                vector = excluded.vector,\n                scope = excluded.scope,\n                item_type = excluded.item_type,\n                tags = excluded.tags,\n                dimension = excluded.dimension,\n                updated_at = excluded.updated_at",
            table
        );

        let conn = self.db.lock().map_err(|e| {
            Error::Storage(format!("RaBitQ database lock error: {}", e))
        })?;
        conn.execute(
            &sql,
            params![
                id,
                vector_blob,
                meta.scope.clone(),
                meta.item_type.clone(),
                meta.tags.join(","),
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
        let mut conn = self.db.lock().map_err(|e| {
            Error::Storage(format!("RaBitQ database lock error: {}", e))
        })?;
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

    fn search(&self, vector: &[f32], top_k: usize) -> Result<Vec<VectorHit>> {
        if top_k == 0 {
            return Ok(Vec::new());
        }
        if vector.is_empty() {
            return Err(Error::Storage("Query vector must not be empty".to_string()));
        }

        let rows = self.load_rows()?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        self.ensure_query_dimensions(vector.len())?;
        let exact_only = self.ensure_index_ready(&rows)?;
        if exact_only {
            return self.exact_search(&rows, vector, top_k);
        }

        let id_order: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
        let state = self.state.lock().map_err(|e| {
            Error::Storage(format!("RaBitQ state lock error: {}", e))
        })?;
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
        let state = self.state.lock().map_err(|e| {
            Error::Storage(format!("RaBitQ state lock error: {}", e))
        })?;
        Ok(json!({
            "backend": "rabitq",
            "table": self.layout.table_name,
            "index_path": self.layout.index_path.display().to_string(),
            "vectors": count,
            "dirty": state.dirty,
            "dimensions": state.dimensions,
        }))
    }

    fn reset(&self) -> Result<()> {
        let table = self.layout.table_name.clone();
        let sql = format!("DELETE FROM \"{}\"", table);
        {
            let conn = self.db.lock().map_err(|e| {
                Error::Storage(format!("RaBitQ database lock error: {}", e))
            })?;
            conn.execute(&sql, []).map_err(map_sqlite_error)?;
        }

        self.clear_index_file_if_exists()?;
        self.store_clean_index(None, None)?;
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
        "CREATE TABLE IF NOT EXISTS \"{}\" (\n            id TEXT PRIMARY KEY,\n            vector BLOB NOT NULL,\n            scope TEXT NOT NULL DEFAULT '',\n            item_type TEXT NOT NULL DEFAULT '',\n            tags TEXT NOT NULL DEFAULT '',\n            dimension INTEGER NOT NULL,\n            updated_at TEXT NOT NULL\n        )",
        table_name
    );
    conn.execute_batch(&sql).map_err(map_sqlite_error)?;
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
        "SELECT id, vector, dimension FROM \"{}\" ORDER BY rowid ASC",
        table_name
    );
    let mut stmt = conn.prepare(&sql).map_err(map_sqlite_error)?;
    let mut rows = stmt.query([]).map_err(map_sqlite_error)?;
    let mut items = Vec::new();

    while let Some(row) = rows.next().map_err(map_sqlite_error)? {
        let id: String = row.get(0).map_err(map_sqlite_error)?;
        let vector_blob: Vec<u8> = row.get(1).map_err(map_sqlite_error)?;
        let dimension = row.get::<_, i64>(2).map_err(map_sqlite_error)? as usize;
        let vector: Vec<f32> = serde_json::from_slice(&vector_blob).map_err(|error| {
            Error::Storage(format!("Failed to decode vector blob: {}", error))
        })?;
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
        };

        for i in 0..48 {
            let value = i as f32;
            let vector = vec![value, value + 0.1, value + 0.2];
            index.upsert(&format!("memory-{}", i), &vector, &meta).unwrap();
        }

        let hits = index.search(&[1.0, 1.1, 1.2], 5).unwrap();
        assert!(!hits.is_empty());

        index.delete_ids(&["memory-1".to_string()]).unwrap();
        let stats = index.stats().unwrap();
        assert_eq!(stats["backend"], "rabitq");

        index.reset().unwrap();
        let hits = index.search(&[1.0, 1.1, 1.2], 5).unwrap();
        assert!(hits.is_empty());
    }
}
