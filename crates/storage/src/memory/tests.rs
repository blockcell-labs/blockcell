use super::*;
use crate::vector::{Embedder, VectorFilter, VectorHit, VectorIndex, VectorMeta, VectorRuntime};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
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
    fail_reset: bool,
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

    fn with_reset_failure() -> Self {
        let state = FakeVectorIndexState {
            fail_reset: true,
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
        self.state
            .lock()
            .unwrap()
            .upserts
            .push((id.to_string(), vector.to_vec(), meta.clone()));
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

    fn search(
        &self,
        _vector: &[f32],
        top_k: usize,
        filter: Option<&VectorFilter>,
    ) -> Result<Vec<VectorHit>> {
        let state = self.state.lock().unwrap();
        if state.fail_search {
            return Err(blockcell_core::Error::Storage(
                "forced vector search failure".to_string(),
            ));
        }
        let mut hits = state.search_hits.clone();
        if let Some(filter) = filter {
            hits.retain(|hit| {
                state
                    .upserts
                    .iter()
                    .find(|(id, _, _)| id == &hit.id)
                    .is_some_and(|(_, _, meta)| filter.matches(meta))
            });
        }
        hits.truncate(top_k);
        Ok(hits)
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
        if state.fail_reset {
            return Err(blockcell_core::Error::Storage(
                "forced vector reset failure".to_string(),
            ));
        }
        state.upserts.clear();
        state.deleted_ids.clear();
        Ok(())
    }
}

fn test_store_with_vector(vector: Option<Arc<VectorRuntime>>) -> (MemoryStore, TempDir) {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("memory.db");
    let store = MemoryStore::open_with_options(&db_path, MemoryStoreOptions { vector }).unwrap();
    (store, dir)
}

fn fake_vector_runtime(embedder: FakeEmbedder, index: FakeVectorIndex) -> Arc<VectorRuntime> {
    Arc::new(VectorRuntime {
        embedder: Arc::new(embedder),
        index: Arc::new(index),
    })
}

#[derive(Clone)]
struct QueueObservingVectorIndex {
    db_path: std::path::PathBuf,
    saw_upsert_intent: Arc<AtomicBool>,
    saw_delete_intent: Arc<AtomicBool>,
}

#[derive(Clone)]
struct DeleteDuringReindexVectorIndex {
    db_path: std::path::PathBuf,
    target_id: Arc<Mutex<Option<String>>>,
    armed: Arc<AtomicBool>,
    deleted_ids: Arc<Mutex<Vec<String>>>,
}

impl DeleteDuringReindexVectorIndex {
    fn new(db_path: std::path::PathBuf) -> Self {
        Self {
            db_path,
            target_id: Arc::new(Mutex::new(None)),
            armed: Arc::new(AtomicBool::new(false)),
            deleted_ids: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl VectorIndex for DeleteDuringReindexVectorIndex {
    fn upsert(&self, id: &str, _vector: &[f32], _meta: &VectorMeta) -> Result<()> {
        if self.armed.load(Ordering::SeqCst)
            && self.target_id.lock().unwrap().as_deref() == Some(id)
        {
            self.armed.store(false, Ordering::SeqCst);
            let conn = Connection::open(&self.db_path).unwrap();
            conn.execute(
                "UPDATE memory_items SET deleted_at = ?1 WHERE id = ?2",
                params![Utc::now().to_rfc3339(), id],
            )
            .unwrap();
        }
        Ok(())
    }

    fn delete_ids(&self, ids: &[String]) -> Result<()> {
        self.deleted_ids.lock().unwrap().extend_from_slice(ids);
        Ok(())
    }

    fn search(
        &self,
        _vector: &[f32],
        _top_k: usize,
        _filter: Option<&VectorFilter>,
    ) -> Result<Vec<VectorHit>> {
        Ok(Vec::new())
    }

    fn health(&self) -> Result<()> {
        Ok(())
    }

    fn stats(&self) -> Result<serde_json::Value> {
        Ok(serde_json::json!({}))
    }

    fn reset(&self) -> Result<()> {
        Ok(())
    }
}

impl QueueObservingVectorIndex {
    fn new(db_path: std::path::PathBuf) -> Self {
        Self {
            db_path,
            saw_upsert_intent: Arc::new(AtomicBool::new(false)),
            saw_delete_intent: Arc::new(AtomicBool::new(false)),
        }
    }

    fn queue_contains(&self, id: &str, operation: &str) -> bool {
        let conn = Connection::open(&self.db_path).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM memory_vector_queue WHERE id = ?1 AND operation = ?2",
            params![id, operation],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
            == 1
    }
}

impl VectorIndex for QueueObservingVectorIndex {
    fn upsert(&self, id: &str, _vector: &[f32], _meta: &VectorMeta) -> Result<()> {
        self.saw_upsert_intent.store(
            self.queue_contains(id, VECTOR_SYNC_OP_UPSERT),
            Ordering::SeqCst,
        );
        Ok(())
    }

    fn delete_ids(&self, ids: &[String]) -> Result<()> {
        self.saw_delete_intent.store(
            ids.iter()
                .all(|id| self.queue_contains(id, VECTOR_SYNC_OP_DELETE)),
            Ordering::SeqCst,
        );
        Ok(())
    }

    fn search(
        &self,
        _vector: &[f32],
        _top_k: usize,
        _filter: Option<&VectorFilter>,
    ) -> Result<Vec<VectorHit>> {
        Ok(Vec::new())
    }

    fn health(&self) -> Result<()> {
        Ok(())
    }

    fn stats(&self) -> Result<serde_json::Value> {
        Ok(serde_json::json!({}))
    }

    fn reset(&self) -> Result<()> {
        Ok(())
    }
}

#[test]
fn vector_intent_is_committed_before_external_index_mutation() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("memory.db");
    let index = QueueObservingVectorIndex::new(db_path.clone());
    let runtime = Arc::new(VectorRuntime {
        embedder: Arc::new(FakeEmbedder::new(3)),
        index: Arc::new(index.clone()),
    });
    let store = MemoryStore::open_with_options(
        &db_path,
        MemoryStoreOptions {
            vector: Some(runtime),
        },
    )
    .unwrap();

    let item = store
        .upsert(UpsertParams {
            scope: "long_term".to_string(),
            item_type: "fact".to_string(),
            title: Some("transactional outbox".to_string()),
            content: "intent must exist before vector upsert".to_string(),
            summary: None,
            tags: vec![],
            source: "user".to_string(),
            channel: None,
            session_key: None,
            importance: 0.8,
            dedup_key: None,
            expires_at: None,
        })
        .unwrap();
    assert!(index.saw_upsert_intent.load(Ordering::SeqCst));

    assert!(store.soft_delete(&item.id).unwrap());
    assert!(index.saw_delete_intent.load(Ordering::SeqCst));
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
fn query_is_scoped_to_session_and_includes_explicit_global_memory() {
    let (store, _dir) = test_store();
    for (content, session_key) in [
        ("session A private", Some("ws:a")),
        ("session B private", Some("ws:b")),
        ("global shared", None),
    ] {
        store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "fact".to_string(),
                title: None,
                content: content.to_string(),
                summary: None,
                tags: vec![],
                source: "user".to_string(),
                channel: Some("ws".to_string()),
                session_key: session_key.map(str::to_string),
                importance: 0.8,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();
    }

    let results = store
        .query(&QueryParams {
            session_key: Some("ws:a".to_string()),
            ..Default::default()
        })
        .unwrap();
    let contents: Vec<&str> = results
        .iter()
        .map(|result| result.item.content.as_str())
        .collect();

    assert!(contents.contains(&"session A private"));
    assert!(contents.contains(&"global shared"));
    assert!(!contents.contains(&"session B private"));
}

#[test]
fn equal_dedup_keys_in_different_sessions_create_distinct_rows() {
    let (store, _dir) = test_store();
    let insert = |session_key: &str, content: &str| {
        store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "preference".to_string(),
                title: None,
                content: content.to_string(),
                summary: None,
                tags: vec![],
                source: "user".to_string(),
                channel: Some("ws".to_string()),
                session_key: Some(session_key.to_string()),
                importance: 0.8,
                dedup_key: Some("pref.language".to_string()),
                expires_at: None,
            })
            .unwrap()
    };

    let first = insert("ws:a", "prefers Rust");
    let second = insert("ws:b", "prefers Go");

    assert_ne!(first.id, second.id);
    assert_eq!(store.query(&QueryParams::default()).unwrap().len(), 2);
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
fn database_rejects_duplicate_active_dedup_keys() {
    let (store, _dir) = test_store();
    let conn = store.inner.lock().unwrap();
    let insert = |id: &str| {
        conn.execute(
            "INSERT INTO memory_items (
                id, scope, type, content, tags, source, importance,
                created_at, updated_at, dedup_key
             ) VALUES (?1, 'long_term', 'fact', 'value', '', 'test', 0.5, ?2, ?2, 'unique-key')",
            params![id, Utc::now().to_rfc3339()],
        )
    };

    insert("first").unwrap();
    assert!(insert("second").is_err());
}

#[test]
fn concurrent_store_handles_upsert_one_deduplicated_row() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("memory.db");
    let first = MemoryStore::open(&db_path).unwrap();
    let second = MemoryStore::open(&db_path).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));

    let run = |store: MemoryStore, barrier: Arc<std::sync::Barrier>| {
        std::thread::spawn(move || {
            barrier.wait();
            store.upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "fact".to_string(),
                title: None,
                content: "concurrent value".to_string(),
                summary: None,
                tags: vec![],
                source: "test".to_string(),
                channel: None,
                session_key: None,
                importance: 0.5,
                dedup_key: Some("shared-key".to_string()),
                expires_at: None,
            })
        })
    };

    let first_handle = run(first, barrier.clone());
    let second_handle = run(second, barrier);
    let first_result = first_handle.join().unwrap();
    let second_result = second_handle.join().unwrap();
    let first_item = first_result.unwrap();
    let second_item = second_result.unwrap();
    assert_eq!(first_item.id, second_item.id);

    let conn = Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_items WHERE dedup_key = 'shared-key' AND deleted_at IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
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
fn test_restore_conflict_soft_deletes_active_and_restores_target() {
    let (store, _dir) = test_store();
    let params = |content: &str| UpsertParams {
        scope: "short_term".to_string(),
        item_type: "note".to_string(),
        title: None,
        content: content.to_string(),
        summary: None,
        tags: vec![],
        source: "user".to_string(),
        channel: None,
        session_key: None,
        importance: 0.5,
        dedup_key: Some("same-key".to_string()),
        expires_at: None,
    };

    // A 被软删后，upsert 同 dedup_key 会插入新行 B（A 不参与查重）
    let a = store.upsert(params("original-a")).unwrap();
    assert!(store.soft_delete(&a.id).unwrap());
    let b = store.upsert(params("replacement-b")).unwrap();
    assert_ne!(a.id, b.id);

    // 恢复 A 时应自动软删除冲突的活动项 B 并恢复 A
    assert!(store.restore(&a.id).unwrap());

    let results = store.query(&QueryParams::default()).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].item.id, a.id);
    assert_eq!(results[0].item.content, "original-a");
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
fn file_migration_does_not_mark_complete_after_read_failure() {
    let (store, dir) = test_store();
    std::fs::write(dir.path().join("2026-07-23.md"), [0xff, 0xfe, 0xfd]).unwrap();

    assert!(store.migrate_from_files(dir.path()).is_err());
    assert!(!store.is_migrated());
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
fn tag_query_requires_exact_membership() {
    let (store, _dir) = test_store();
    for tag in ["go", "mongodb"] {
        store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "fact".to_string(),
                title: None,
                content: format!("tagged {tag}"),
                summary: None,
                tags: vec![tag.to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.5,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();
    }

    let results = store
        .query(&QueryParams {
            tags: Some(vec!["go".to_string()]),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].item.tags, vec!["go"]);
}

#[test]
fn batch_delete_tag_requires_exact_membership() {
    let (store, _dir) = test_store();
    for tag in ["prod", "production"] {
        store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "fact".to_string(),
                title: None,
                content: format!("tagged {tag}"),
                summary: None,
                tags: vec![tag.to_string()],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 0.5,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();
    }

    let deleted = store
        .batch_soft_delete(None, None, Some(&["prod".to_string()]), None)
        .unwrap();
    assert_eq!(deleted, 1);

    let remaining = store.query(&QueryParams::default()).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].item.tags, vec!["production"]);
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
            "Title: favorite database\nSummary: Prefers RabitQ\nTags: vector, database".to_string()
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
fn reindex_seeds_durable_intents_before_reset() {
    let embedder = FakeEmbedder::new(3);
    let index = FakeVectorIndex::with_reset_failure();
    let runtime = fake_vector_runtime(embedder, index);
    let (store, _dir) = test_store_with_vector(Some(runtime));

    for content in ["first durable row", "second durable row"] {
        store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "fact".to_string(),
                title: None,
                content: content.to_string(),
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
    }

    assert!(store.reindex_vectors().is_err());
    let stats = store.stats().unwrap();
    assert_eq!(stats["vector"]["pending_upserts"], 2);
}

#[test]
fn reindex_deletes_item_that_becomes_inactive_during_upsert() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("memory.db");
    let index = DeleteDuringReindexVectorIndex::new(db_path.clone());
    let runtime = Arc::new(VectorRuntime {
        embedder: Arc::new(FakeEmbedder::new(3)),
        index: Arc::new(index.clone()),
    });
    let store = MemoryStore::open_with_options(
        &db_path,
        MemoryStoreOptions {
            vector: Some(runtime),
        },
    )
    .unwrap();
    let item = store
        .upsert(UpsertParams {
            scope: "long_term".to_string(),
            item_type: "fact".to_string(),
            title: None,
            content: "delete during rebuild".to_string(),
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
    *index.target_id.lock().unwrap() = Some(item.id.clone());
    index.armed.store(true, Ordering::SeqCst);

    let result = store.reindex_vectors().unwrap();
    assert_eq!(result.indexed, 1);
    assert!(index.deleted_ids.lock().unwrap().contains(&item.id));
    assert_eq!(store.stats().unwrap()["vector"]["pending_operations"], 0);
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
fn chinese_fts_recalls_related_short_term_memory() {
    let (store, dir) = test_store();
    let item = store
        .upsert(UpsertParams {
            scope: "short_term".to_string(),
            item_type: "note".to_string(),
            title: None,
            content: "发布前需要检查 changelog 和版本号".to_string(),
            summary: None,
            tags: vec![],
            source: "user".to_string(),
            channel: None,
            session_key: Some("cli:release".to_string()),
            importance: 0.8,
            dedup_key: None,
            expires_at: None,
        })
        .expect("store Chinese memory");

    let results = store
        .query(&QueryParams {
            query: Some("发版检查什么".to_string()),
            session_key: Some("cli:release".to_string()),
            top_k: 5,
            ..Default::default()
        })
        .expect("query Chinese memory");
    assert!(results.iter().any(|result| result.item.id == item.id));

    drop(store);
    let db_path = dir.path().join("memory.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open memory db");
    conn.execute_batch(
        "DROP TRIGGER memory_ai;
         DROP TRIGGER memory_ad;
         DROP TRIGGER memory_au;
         DROP TABLE memory_fts;
         CREATE VIRTUAL TABLE memory_fts USING fts5(
             title, summary, content, tags,
             content='memory_items', content_rowid='rowid'
         );
         INSERT INTO memory_fts(memory_fts) VALUES('rebuild');",
    )
    .expect("restore legacy memory fts");
    drop(conn);

    let migrated = MemoryStore::open(&db_path).expect("migrate memory fts");
    let migrated_results = migrated
        .query(&QueryParams {
            query: Some("发版检查什么".to_string()),
            session_key: Some("cli:release".to_string()),
            top_k: 5,
            ..Default::default()
        })
        .expect("query migrated Chinese memory");
    assert!(migrated_results
        .iter()
        .any(|result| result.item.id == item.id));
    drop(migrated);

    let conn = rusqlite::Connection::open(&db_path).expect("reopen memory db");
    let schema: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='memory_fts'",
            [],
            |row| row.get(0),
        )
        .expect("read memory fts schema");
    assert!(schema.to_lowercase().contains("tokenize='trigram'"));
}

#[test]
fn fts_filters_deleted_candidates_before_applying_candidate_limit() {
    let (store, _dir) = test_store();
    for index in 0..25 {
        let item = store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "fact".to_string(),
                title: Some(format!("obsolete {index}")),
                content: "quasarneedle quasarneedle quasarneedle quasarneedle obsolete memory"
                    .to_string(),
                summary: None,
                tags: vec![],
                source: "user".to_string(),
                channel: None,
                session_key: None,
                importance: 1.0,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();
        assert!(store.soft_delete(&item.id).unwrap());
    }
    let active = store
        .upsert(UpsertParams {
            scope: "long_term".to_string(),
            item_type: "fact".to_string(),
            title: Some("active".to_string()),
            content: "quasarneedle active memory".to_string(),
            summary: None,
            tags: vec![],
            source: "user".to_string(),
            channel: None,
            session_key: None,
            importance: 0.1,
            dedup_key: None,
            expires_at: None,
        })
        .unwrap();

    let results = store
        .query(&QueryParams {
            query: Some("quasarneedle".to_string()),
            top_k: 1,
            ..Default::default()
        })
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].item.id, active.id);
}

#[test]
fn vector_candidates_are_session_filtered_before_limit() {
    let embedder = FakeEmbedder::new(3);
    let index = FakeVectorIndex::new();
    let runtime = fake_vector_runtime(embedder, index.clone());
    let (store, _dir) = test_store_with_vector(Some(runtime));

    let mut hits = Vec::new();
    for rank in 0..20 {
        let item = store
            .upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: "fact".to_string(),
                title: None,
                content: format!("session b item {rank}"),
                summary: None,
                tags: vec![],
                source: "user".to_string(),
                channel: Some("ws".to_string()),
                session_key: Some("ws:b".to_string()),
                importance: 0.5,
                dedup_key: None,
                expires_at: None,
            })
            .unwrap();
        hits.push(VectorHit {
            id: item.id,
            score: 1.0 - rank as f64 / 100.0,
        });
    }
    let session_a = store
        .upsert(UpsertParams {
            scope: "long_term".to_string(),
            item_type: "fact".to_string(),
            title: None,
            content: "session a semantic target".to_string(),
            summary: None,
            tags: vec![],
            source: "user".to_string(),
            channel: Some("ws".to_string()),
            session_key: Some("ws:a".to_string()),
            importance: 0.5,
            dedup_key: None,
            expires_at: None,
        })
        .unwrap();
    hits.push(VectorHit {
        id: session_a.id.clone(),
        score: 0.5,
    });
    {
        let mut state = index.state.lock().unwrap();
        state.search_hits = hits;
        for (_, _, meta) in &mut state.upserts {
            if meta.session_key.as_deref() == Some("ws:b") {
                // Simulate rows written before vector ownership metadata existed.
                meta.session_key = None;
            }
        }
    }

    let results = store
        .query(&QueryParams {
            query: Some("semantic-only-needle".to_string()),
            session_key: Some("ws:a".to_string()),
            top_k: 5,
            ..Default::default()
        })
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].item.id, session_a.id);
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
