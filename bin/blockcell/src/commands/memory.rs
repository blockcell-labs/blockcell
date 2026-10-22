use blockcell_core::{Config, Paths};
use blockcell_storage::memory::QueryParams;
use blockcell_storage::MemoryStore;
use std::collections::BTreeMap;
use std::sync::Arc;

use super::memory_store::open_memory_store;

fn open_cli_memory_store(paths: &Paths) -> anyhow::Result<MemoryStore> {
    let config = Config::load_or_default(paths)?;
    open_memory_store(paths, &config)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CanonicalMigrationResult {
    pub migrated: usize,
    pub deduplicated: usize,
    pub retired: usize,
}

fn normalize_migration_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn migrate_canonical_at(
    paths: &Paths,
    store: &MemoryStore,
) -> anyhow::Result<CanonicalMigrationResult> {
    let rows = store.active_long_term_items()?;
    if rows.is_empty() {
        return Ok(CanonicalMigrationResult::default());
    }

    let mut unique = BTreeMap::new();
    for row in &rows {
        let content = normalize_migration_content(&row.content);
        let hash = blockcell_core::stable_hash_session_key(&content);
        unique.entry(hash).or_insert((content, row.id.as_str()));
    }

    let index = Arc::new(blockcell_storage::KnowledgeIndex::open(
        &paths.knowledge_index_db(),
    )?);
    index.rebuild_from_files(paths)?;
    let mut file_store = blockcell_agent::MemoryFileStore::open(paths)?;
    file_store.set_knowledge_index(index, "USER.md", "memory/MEMORY.md");
    let updated = chrono::Utc::now().format("%Y-%m-%d");
    for (hash, (content, legacy_id)) in &unique {
        let entry = format!(
            "- [id:migrated-{hash}] [scope:workspace] [source:verified] [updated:{updated}] {content} <!-- migrated-from:memory.db:{legacy_id} -->"
        );
        file_store.add(blockcell_agent::MemoryFileTarget::Memory, &entry)?;
    }

    let ids = rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
    let retired = store.retire_long_term_items(&ids)?;
    Ok(CanonicalMigrationResult {
        migrated: unique.len(),
        deduplicated: rows.len().saturating_sub(unique.len()),
        retired,
    })
}

pub async fn migrate_canonical() -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let db_path = paths.memory_dir().join("memory.db");
    if !db_path.exists() {
        println!("(Memory database not created yet)");
        return Ok(());
    }
    let store = open_cli_memory_store(&paths)?;
    let result = migrate_canonical_at(&paths, &store)?;
    println!(
        "✅ 规范知识迁移完成：写入 {} 条，去重 {} 条，退役 {} 条。",
        result.migrated, result.deduplicated, result.retired
    );
    Ok(())
}

/// List recent memory items.
pub async fn list(item_type: Option<String>, limit: usize) -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let db_path = paths.workspace().join("memory").join("memory.db");

    if !db_path.exists() {
        println!("(Memory database not created yet)");
        return Ok(());
    }

    let store = open_cli_memory_store(&paths)?;

    let params = QueryParams {
        session_key: None,
        query: None,
        scope: None,
        item_type: item_type.clone(),
        tags: None,
        time_range_days: None,
        top_k: limit,
        include_deleted: false,
    };

    let results = store
        .query(&params)
        .map_err(|e| anyhow::anyhow!("Failed to query: {}", e))?;

    println!();
    if results.is_empty() {
        let type_hint = item_type.as_deref().unwrap_or("any");
        println!("(No memories found, type={})", type_hint);
    } else {
        println!("🧠 Memory items ({} found)", results.len());
        println!();
        for (i, r) in results.iter().enumerate() {
            let title = r.item.title.as_deref().unwrap_or("(untitled)");
            let scope_icon = if r.item.scope == "long_term" {
                "📌"
            } else {
                "💬"
            };
            println!(
                "  {}. {} [{}] {} #{}",
                i + 1,
                scope_icon,
                r.item.item_type,
                title,
                &r.item.id.chars().take(8).collect::<String>()
            );
            let preview: String = r.item.content.chars().take(100).collect();
            if r.item.content.chars().count() > 100 {
                println!("     {}...", preview);
            } else {
                println!("     {}", preview);
            }
            if !r.item.tags.is_empty() {
                let tags: Vec<&str> = r.item.tags.iter().map(|s| s.as_str()).collect();
                println!("     🏷️  {}", tags.join(", "));
            }
            println!();
        }
    }
    Ok(())
}

/// Show a specific memory item by ID.
pub async fn show(id: &str) -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let db_path = paths.workspace().join("memory").join("memory.db");

    if !db_path.exists() {
        println!("(Memory database not created yet)");
        return Ok(());
    }

    let store = open_cli_memory_store(&paths)?;

    match store.get_by_id(id) {
        Ok(Some(item)) => {
            println!();
            println!("🧠 Memory Item");
            println!("  ID:    {}", item.id);
            println!("  Type:  {}", item.item_type);
            println!("  Scope: {}", item.scope);
            if let Some(ref title) = item.title {
                println!("  Title: {}", title);
            }
            if !item.tags.is_empty() {
                println!("  Tags:  {}", item.tags.join(", "));
            }
            println!();
            println!("  Content:");
            for line in item.content.lines() {
                println!("    {}", line);
            }
            println!();
        }
        Ok(None) => {
            println!("No memory item found with ID: {}", id);
        }
        Err(e) => {
            println!("Failed to lookup memory: {}", e);
        }
    }
    Ok(())
}

/// Delete (soft-delete) a memory item by ID.
pub async fn delete(id: &str) -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let db_path = paths.workspace().join("memory").join("memory.db");

    if !db_path.exists() {
        println!("(Memory database not created yet)");
        return Ok(());
    }

    let store = open_cli_memory_store(&paths)?;

    match store.soft_delete(id) {
        Ok(true) => {
            println!("✅ Memory item {} deleted (moved to recycle bin).", id);
            println!("   Run `blockcell memory maintenance` to permanently purge.");
        }
        Ok(false) => {
            println!("No memory item found with ID: {}", id);
        }
        Err(e) => {
            println!("Failed to delete memory: {}", e);
        }
    }
    Ok(())
}

/// Show memory statistics.
pub async fn stats() -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let db_path = paths.workspace().join("memory").join("memory.db");

    if !db_path.exists() {
        println!("(Memory database not created yet)");
        return Ok(());
    }

    let store = open_cli_memory_store(&paths)?;

    let stats = store
        .stats()
        .map_err(|e| anyhow::anyhow!("Failed to get stats: {}", e))?;

    println!();
    println!("🧠 Memory Statistics");
    println!("  Total records: {}", stats["total_active"]);
    println!("  Long-term:     {}", stats["long_term"]);
    println!("  Short-term:    {}", stats["short_term"]);
    println!("  Recycle bin:   {}", stats["deleted_in_recycle_bin"]);
    if let Some(vector) = stats.get("vector") {
        println!();
        println!("  Vector enabled:   {}", vector["enabled"]);
        match vector.get("healthy").and_then(|value| value.as_bool()) {
            Some(healthy) => println!("  Vector healthy:   {}", healthy),
            None => println!("  Vector healthy:   n/a"),
        }
        println!("  Pending vector ops: {}", vector["pending_operations"]);
        println!("  Pending upserts:    {}", vector["pending_upserts"]);
        println!("  Pending deletes:    {}", vector["pending_deletes"]);

        if let Some(backend) = vector.get("backend") {
            if let Some(rows) = backend.get("rows").and_then(|value| value.as_u64()) {
                println!("  Vector rows:        {}", rows);
            }
            if let Some(indices) = backend.get("indices").and_then(|value| value.as_u64()) {
                println!("  Vector indices:     {}", indices);
            }
            if let Some(error) = backend.get("error").and_then(|value| value.as_str()) {
                println!("  Vector backend err: {}", error);
            }
        }
    }
    println!();
    Ok(())
}

/// Search memory items.
pub async fn search(
    query: &str,
    scope: Option<String>,
    item_type: Option<String>,
    top_k: usize,
) -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let db_path = paths.workspace().join("memory").join("memory.db");

    if !db_path.exists() {
        println!("(Memory database not created yet)");
        return Ok(());
    }

    let store = open_cli_memory_store(&paths)?;

    let params = QueryParams {
        session_key: None,
        query: if query.is_empty() {
            None
        } else {
            Some(query.to_string())
        },
        scope,
        item_type,
        tags: None,
        time_range_days: None,
        top_k,
        include_deleted: false,
    };

    let results = store
        .query(&params)
        .map_err(|e| anyhow::anyhow!("Failed to query: {}", e))?;

    println!();
    if results.is_empty() {
        println!("(No matching memories found)");
    } else {
        println!("🔍 Search results ({} found)", results.len());
        println!();
        for (i, r) in results.iter().enumerate() {
            let title = r.item.title.as_deref().unwrap_or("(untitled)");
            let scope_icon = if r.item.scope == "long_term" {
                "📌"
            } else {
                "💬"
            };
            println!(
                "  {}. {} [{}] {} (score: {:.2})",
                i + 1,
                scope_icon,
                r.item.item_type,
                title,
                r.score
            );

            // Show truncated content
            let content = &r.item.content;
            let preview: String = content.chars().take(120).collect();
            if content.chars().count() > 120 {
                println!("     {}...", preview);
            } else {
                println!("     {}", preview);
            }

            if !r.item.tags.is_empty() {
                let tags: Vec<&str> = r
                    .item
                    .tags
                    .iter()
                    .map(|s| s.as_str())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !tags.is_empty() {
                    println!("     🏷️  {}", tags.join(", "));
                }
            }
            println!();
        }
    }
    Ok(())
}

/// Run maintenance (clean expired + purge recycle bin).
pub async fn maintenance(recycle_days: i64) -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let db_path = paths.workspace().join("memory").join("memory.db");

    if !db_path.exists() {
        println!("(Memory database not created yet)");
        return Ok(());
    }

    let store = open_cli_memory_store(&paths)?;

    let (expired, purged) = store
        .maintenance(recycle_days)
        .map_err(|e| anyhow::anyhow!("Failed to run maintenance: {}", e))?;

    println!(
        "✅ Maintenance complete: {} expired records cleaned, {} recycle bin records purged",
        expired, purged
    );
    Ok(())
}

/// Retry queued vector sync operations.
pub async fn retry_vector_sync(limit: usize) -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let db_path = paths.workspace().join("memory").join("memory.db");

    if !db_path.exists() {
        println!("(Memory database not created yet)");
        return Ok(());
    }

    let store = open_cli_memory_store(&paths)?;
    let result = store
        .retry_vector_sync(limit)
        .map_err(|e| anyhow::anyhow!("Failed to retry vector sync: {}", e))?;

    println!(
        "✅ Vector retry complete: attempted {}, succeeded {}, failed {}",
        result.attempted, result.succeeded, result.failed
    );
    Ok(())
}

/// Rebuild the vector index from active SQLite rows.
pub async fn reindex() -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let db_path = paths.workspace().join("memory").join("memory.db");

    if !db_path.exists() {
        println!("(Memory database not created yet)");
        return Ok(());
    }

    let store = open_cli_memory_store(&paths)?;
    let result = store
        .reindex_vectors()
        .map_err(|e| anyhow::anyhow!("Failed to reindex vectors: {}", e))?;

    println!(
        "✅ Vector reindex complete: indexed {}, failed {}",
        result.indexed, result.failed
    );
    Ok(())
}

/// Clear all memory (soft-delete everything).
pub async fn clear(scope: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::new_configured();
    let db_path = paths.workspace().join("memory").join("memory.db");

    if !db_path.exists() {
        println!("(Memory database not created yet)");
        return Ok(());
    }

    let store = open_cli_memory_store(&paths)?;

    let count = store
        .batch_soft_delete(scope.as_deref(), None, None, None)
        .map_err(|e| anyhow::anyhow!("Failed to clear: {}", e))?;

    let scope_desc = scope.as_deref().unwrap_or("all");
    println!("✅ Deleted {} memories (scope: {})", count, scope_desc);
    println!("   Memories moved to recycle bin. Use `maintenance` to permanently purge.");
    Ok(())
}

#[cfg(test)]
mod canonical_migration_tests {
    use super::*;
    use blockcell_storage::memory::UpsertParams;

    fn test_paths() -> Paths {
        Paths::with_base(std::env::temp_dir().join(format!(
            "blockcell-memory-migrate-canonical-{}",
            uuid::Uuid::new_v4()
        )))
    }

    fn seed_memory(store: &MemoryStore, scope: &str, content: &str) {
        store
            .upsert(UpsertParams {
                scope: scope.to_string(),
                item_type: "preference".to_string(),
                title: None,
                content: content.to_string(),
                summary: None,
                tags: vec![],
                source: "legacy".to_string(),
                channel: None,
                session_key: None,
                importance: 0.8,
                dedup_key: None,
                expires_at: None,
            })
            .expect("seed memory row");
    }

    #[test]
    fn migrate_canonical_deduplicates_retires_and_reindexes() {
        let paths = test_paths();
        paths.ensure_dirs().expect("ensure paths");
        let store =
            MemoryStore::open(&paths.memory_dir().join("memory.db")).expect("open memory store");
        seed_memory(&store, "long_term", "User prefers concise replies.");
        seed_memory(&store, "long_term", "  User prefers concise replies.  ");
        seed_memory(&store, "short_term", "Temporary task state.");

        let result = migrate_canonical_at(&paths, &store).expect("migrate canonical memory");

        assert_eq!(result.migrated, 1);
        assert_eq!(result.deduplicated, 1);
        assert_eq!(result.retired, 2);
        let canonical = std::fs::read_to_string(paths.memory_md()).expect("read MEMORY.md");
        assert_eq!(
            canonical.matches("User prefers concise replies.").count(),
            1
        );
        assert!(canonical.contains("[id:migrated-"));
        assert!(canonical.contains("[scope:workspace]"));
        assert!(canonical.contains("[source:verified]"));
        assert!(store
            .active_long_term_items()
            .expect("list active long-term")
            .is_empty());
        let short_term = store
            .query(&QueryParams {
                scope: Some("short_term".to_string()),
                top_k: 10,
                ..QueryParams::default()
            })
            .expect("query short-term");
        assert_eq!(short_term.len(), 1);

        let index = blockcell_storage::KnowledgeIndex::open(&paths.knowledge_index_db())
            .expect("open knowledge index");
        assert_eq!(
            index
                .search("concise", 10)
                .expect("search migrated entry")
                .len(),
            1
        );

        let rerun = migrate_canonical_at(&paths, &store).expect("rerun migration");
        assert_eq!(rerun.migrated, 0);
        assert_eq!(rerun.deduplicated, 0);
        assert_eq!(rerun.retired, 0);
    }
}
