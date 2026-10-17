use blockcell_core::Paths;
use blockcell_storage::KnowledgeIndex;
use blockcell_storage::{memory::UpsertParams, MemoryStore};

fn test_paths(label: &str) -> Paths {
    Paths::with_base(std::env::temp_dir().join(format!(
        "blockcell-knowledge-index-{label}-{}",
        uuid::Uuid::new_v4()
    )))
}

#[test]
fn rebuild_indexes_canonical_files_with_metadata() {
    let paths = test_paths("metadata");
    paths.ensure_dirs().expect("ensure paths");
    std::fs::write(
        paths.user_md(),
        "- [id:pref-reply] [scope:user] [source:user_statement] [updated:2026-08-01] 用户偏好简洁回答\n",
    )
    .expect("write USER.md");
    std::fs::write(
        paths.memory_md(),
        "## Project\n\n- [id:project-deploy] [scope:workspace] [source:verified] [updated:2026-07-30] 项目使用金丝雀发布。\n",
    )
    .expect("write MEMORY.md");

    let index = KnowledgeIndex::open(&paths.knowledge_index_db()).expect("open index");
    let result = index.rebuild_from_files(&paths).expect("rebuild index");

    assert_eq!(result.indexed, 2);
    let hits = index.search("用户偏好简洁回答", 10).expect("search index");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "pref-reply");
    assert_eq!(hits[0].file, "USER.md");
    assert_eq!(hits[0].anchor, "root");
    assert_eq!(hits[0].scope, "user");
    assert_eq!(hits[0].source, "user_statement");
    assert_eq!(hits[0].updated_at, "2026-08-01");
    assert!(!hits[0].content_hash.is_empty());
    let conn = rusqlite::Connection::open(paths.knowledge_index_db()).expect("open raw index db");
    let vector_table_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='knowledge_vectors'",
            [],
            |row| row.get(0),
        )
        .expect("query vector table");
    assert_eq!(vector_table_count, 1);
}

#[test]
fn chinese_fts_recalls_related_canonical_knowledge() {
    let paths = test_paths("chinese-recall");
    paths.ensure_dirs().expect("ensure paths");
    std::fs::write(paths.memory_md(), "发布前需要检查 changelog 和版本号\n")
        .expect("write MEMORY.md");

    let index = KnowledgeIndex::open(&paths.knowledge_index_db()).expect("open index");
    index.rebuild_from_files(&paths).expect("rebuild index");
    let hits = index
        .search("发版检查什么", 10)
        .expect("search Chinese knowledge");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].content.contains("changelog"));

    drop(index);
    let conn = rusqlite::Connection::open(paths.knowledge_index_db()).expect("open raw index db");
    // user_version=0 模拟版本化之前的旧库——init_schema 仍走一次性嗅探迁移
    conn.execute_batch(
        "DROP TRIGGER knowledge_ai;
         DROP TRIGGER knowledge_ad;
         DROP TRIGGER knowledge_au;
         DROP TABLE knowledge_fts;
         CREATE VIRTUAL TABLE knowledge_fts USING fts5(
             content, file, anchor,
             content='knowledge_entries', content_rowid='rowid'
         );
         INSERT INTO knowledge_fts(knowledge_fts) VALUES('rebuild');
         PRAGMA user_version=0;",
    )
    .expect("restore legacy knowledge fts");
    drop(conn);

    let migrated = KnowledgeIndex::open(&paths.knowledge_index_db()).expect("migrate index");
    assert_eq!(
        migrated
            .search("发版检查什么", 10)
            .expect("search migrated Chinese knowledge")
            .len(),
        1
    );
    drop(migrated);

    let conn = rusqlite::Connection::open(paths.knowledge_index_db()).expect("reopen raw index db");
    let schema: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='knowledge_fts'",
            [],
            |row| row.get(0),
        )
        .expect("read knowledge fts schema");
    assert!(schema.to_lowercase().contains("tokenize='trigram'"));
}

#[test]
fn rebuild_removes_entries_deleted_from_canonical_file() {
    let paths = test_paths("delete");
    paths.ensure_dirs().expect("ensure paths");
    std::fs::write(paths.user_md(), "User prefers concise release notes.\n")
        .expect("write USER.md");

    let index = KnowledgeIndex::open(&paths.knowledge_index_db()).expect("open index");
    index.rebuild_from_files(&paths).expect("initial rebuild");
    assert_eq!(
        index.search("concise", 10).expect("initial search").len(),
        1
    );

    std::fs::write(paths.user_md(), "").expect("clear USER.md");
    let result = index.rebuild_from_files(&paths).expect("second rebuild");

    assert_eq!(result.removed, 1);
    assert!(index
        .search("concise", 10)
        .expect("final search")
        .is_empty());
}

#[test]
fn legacy_long_term_rows_can_be_listed_and_retired_for_file_migration() {
    let paths = test_paths("legacy-long-term");
    paths.ensure_dirs().expect("ensure paths");
    let store = MemoryStore::open(&paths.memory_dir().join("memory.db")).expect("open memory");
    let long_term = store
        .upsert(UpsertParams {
            scope: "long_term".to_string(),
            item_type: "preference".to_string(),
            title: None,
            content: "User prefers concise replies.".to_string(),
            summary: None,
            tags: vec![],
            source: "legacy".to_string(),
            channel: None,
            session_key: None,
            importance: 0.8,
            dedup_key: None,
            expires_at: None,
        })
        .expect("seed long-term row");
    store
        .upsert(UpsertParams {
            scope: "short_term".to_string(),
            item_type: "note".to_string(),
            title: None,
            content: "Temporary task state.".to_string(),
            summary: None,
            tags: vec![],
            source: "legacy".to_string(),
            channel: None,
            session_key: Some("cli:test".to_string()),
            importance: 0.4,
            dedup_key: None,
            expires_at: None,
        })
        .expect("seed short-term row");

    let rows = store.active_long_term_items().expect("list long-term rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, long_term.id);

    assert_eq!(
        store
            .retire_long_term_items(&[long_term.id])
            .expect("retire rows"),
        1
    );
    assert!(store
        .active_long_term_items()
        .expect("list after retirement")
        .is_empty());
}

#[test]
fn knowledge_index_conflict_prefers_active_explicit_newer_entry() {
    let paths = test_paths("conflict");
    paths.ensure_dirs().expect("ensure paths");
    std::fs::write(
        paths.user_md(),
        concat!(
            "- [id:pref-old] [scope:user] [source:inferred] [updated:2026-07-01] 用户偏好详细回答\n",
            "- [id:pref-new] [scope:user] [source:user_statement] [updated:2026-08-01] [supersedes:pref-old] 用户偏好简洁回答\n",
            "- [id:pref-duplicate] [scope:user] [source:inferred] [updated:2026-07-15] 用户偏好简洁回答 <!-- migrated-from:memory/user.md -->\n",
        ),
    )
    .expect("write conflicting entries");

    let index = KnowledgeIndex::open(&paths.knowledge_index_db()).expect("open index");
    index.rebuild_from_files(&paths).expect("rebuild index");
    let hits = index
        .search("用户偏好简洁回答", 10)
        .expect("search duplicate preference");

    assert_eq!(hits.len(), 1, "duplicate content should collapse");
    assert_eq!(hits[0].id, "pref-new");
    assert_eq!(
        index
            .get_by_id("pref-new")
            .expect("lookup by ID")
            .expect("entry by ID")
            .content,
        "用户偏好简洁回答"
    );
    assert!(index
        .search("用户偏好详细回答", 10)
        .expect("search superseded preference")
        .is_empty());
}

#[test]
fn forgotten_content_tombstone_round_trip() {
    let paths = test_paths("forgotten");
    paths.ensure_dirs().expect("ensure paths");
    let index = KnowledgeIndex::open(&paths.knowledge_index_db()).expect("open index");

    assert!(!index
        .is_forgotten_content("User prefers concise replies.")
        .expect("check initial tombstone"));
    index
        .record_forgotten_content("  User prefers concise replies.  ", "user request")
        .expect("record tombstone");
    assert!(index
        .is_forgotten_content("User   prefers concise replies.")
        .expect("check normalized tombstone"));

    std::fs::write(
        paths.user_md(),
        "- [id:pref-forgotten] [scope:user] [source:user_statement] [updated:2026-08-01] User   prefers concise replies.\n",
    )
    .expect("write tombstoned canonical content");
    index.rebuild_from_files(&paths).expect("rebuild index");
    assert!(index
        .search("concise replies", 10)
        .expect("search tombstoned content")
        .is_empty());
}
