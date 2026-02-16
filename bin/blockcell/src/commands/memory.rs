use blockcell_core::Paths;
use blockcell_storage::MemoryStore;
use blockcell_storage::memory::QueryParams;

/// Show memory statistics.
pub async fn stats() -> anyhow::Result<()> {
    let paths = Paths::default();
    let db_path = paths.workspace().join("memory").join("memory.db");

    if !db_path.exists() {
        println!("(Memory database not created yet)");
        return Ok(());
    }

    let store = MemoryStore::open(&db_path)
        .map_err(|e| anyhow::anyhow!("Failed to open memory db: {}", e))?;

    let stats = store.stats()
        .map_err(|e| anyhow::anyhow!("Failed to get stats: {}", e))?;

    println!();
    println!("🧠 Memory Statistics");
    println!("  Total records: {}", stats["total_active"]);
    println!("  Long-term:     {}", stats["long_term"]);
    println!("  Short-term:    {}", stats["short_term"]);
    println!("  Recycle bin:   {}", stats["deleted_in_recycle_bin"]);
    println!();
    Ok(())
}

/// Search memory items.
pub async fn search(query: &str, scope: Option<String>, item_type: Option<String>, top_k: usize) -> anyhow::Result<()> {
    let paths = Paths::default();
    let db_path = paths.workspace().join("memory").join("memory.db");

    if !db_path.exists() {
        println!("(Memory database not created yet)");
        return Ok(());
    }

    let store = MemoryStore::open(&db_path)
        .map_err(|e| anyhow::anyhow!("Failed to open memory db: {}", e))?;

    let params = QueryParams {
        query: if query.is_empty() { None } else { Some(query.to_string()) },
        scope,
        item_type,
        tags: None,
        time_range_days: None,
        top_k,
        include_deleted: false,
    };

    let results = store.query(&params)
        .map_err(|e| anyhow::anyhow!("Failed to query: {}", e))?;

    println!();
    if results.is_empty() {
        println!("(No matching memories found)");
    } else {
        println!("🔍 Search results ({} found)", results.len());
        println!();
        for (i, r) in results.iter().enumerate() {
            let title = r.item.title.as_deref().unwrap_or("(untitled)");
            let scope_icon = if r.item.scope == "long_term" { "📌" } else { "💬" };
            println!("  {}. {} [{}] {} (score: {:.2})", i + 1, scope_icon, r.item.item_type, title, r.score);

            // Show truncated content
            let content = &r.item.content;
            let preview: String = content.chars().take(120).collect();
            if content.chars().count() > 120 {
                println!("     {}...", preview);
            } else {
                println!("     {}", preview);
            }

            if !r.item.tags.is_empty() {
                let tags: Vec<&str> = r.item.tags.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect();
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
    let paths = Paths::default();
    let db_path = paths.workspace().join("memory").join("memory.db");

    if !db_path.exists() {
        println!("(Memory database not created yet)");
        return Ok(());
    }

    let store = MemoryStore::open(&db_path)
        .map_err(|e| anyhow::anyhow!("Failed to open memory db: {}", e))?;

    let (expired, purged) = store.maintenance(recycle_days)
        .map_err(|e| anyhow::anyhow!("Failed to run maintenance: {}", e))?;

    println!("✅ Maintenance complete: {} expired records cleaned, {} recycle bin records purged", expired, purged);
    Ok(())
}

/// Clear all memory (soft-delete everything).
pub async fn clear(scope: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::default();
    let db_path = paths.workspace().join("memory").join("memory.db");

    if !db_path.exists() {
        println!("(Memory database not created yet)");
        return Ok(());
    }

    let store = MemoryStore::open(&db_path)
        .map_err(|e| anyhow::anyhow!("Failed to open memory db: {}", e))?;

    let count = store.batch_soft_delete(scope.as_deref(), None, None, None)
        .map_err(|e| anyhow::anyhow!("Failed to clear: {}", e))?;

    let scope_desc = scope.as_deref().unwrap_or("all");
    println!("✅ Deleted {} memories (scope: {})", count, scope_desc);
    println!("   Memories moved to recycle bin. Use `maintenance` to permanently purge.");
    Ok(())
}
