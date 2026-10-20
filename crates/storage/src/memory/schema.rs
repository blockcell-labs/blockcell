use super::*;

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

        // Enable WAL mode and wait briefly for other store handles instead of
        // immediately failing ordinary concurrent writes.
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .map_err(|e| {
                blockcell_core::Error::Storage(format!("Failed to configure memory db: {}", e))
            })?;

        let store = Self {
            inner: Arc::new(Mutex::new(conn)),
            db_path: db_path.to_path_buf(),
            vector: options.vector,
        };
        store.init_schema()?;
        Ok(store)
    }

    pub(crate) fn init_schema(&self) -> Result<()> {
        let mut conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

        let rebuild_fts = crate::fts::prepare_trigram_fts(
            &conn,
            "memory_fts",
            &["memory_ai", "memory_ad", "memory_au"],
        )?;

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
                content_rowid='rowid',
                tokenize='trigram'
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

        if rebuild_fts {
            conn.execute("INSERT INTO memory_fts(memory_fts) VALUES('rebuild')", [])
                .map_err(|error| {
                    blockcell_core::Error::Storage(format!(
                        "Failed to rebuild trigram memory FTS: {error}"
                    ))
                })?;
        }

        ensure_dedup_unique_index(&mut conn)?;

        debug!("Memory store schema initialized");
        Ok(())
    }
}

fn ensure_dedup_unique_index(conn: &mut Connection) -> Result<()> {
    let version: Option<String> = conn
        .query_row(
            "SELECT value FROM memory_meta WHERE key = 'dedup_unique_index_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| {
            blockcell_core::Error::Storage(format!("Failed to read memory schema version: {error}"))
        })?;
    let index_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = 'idx_memory_active_dedup_unique')",
            [],
            |row| row.get(0),
        )
        .map_err(|error| {
            blockcell_core::Error::Storage(format!("Failed to inspect memory indexes: {error}"))
        })?;
    if version.as_deref() == Some("1") && index_exists {
        return Ok(());
    }

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| {
            blockcell_core::Error::Storage(format!(
                "Failed to begin memory dedup migration: {error}"
            ))
        })?;
    tx.execute_batch(
        "DROP INDEX IF EXISTS idx_memory_active_dedup_unique;
         UPDATE memory_items
         SET dedup_key = NULL
         WHERE dedup_key IS NOT NULL
           AND dedup_key != ''
           AND deleted_at IS NULL
           AND rowid NOT IN (
               SELECT MAX(rowid)
               FROM memory_items
               WHERE dedup_key IS NOT NULL
                 AND dedup_key != ''
                 AND deleted_at IS NULL
               GROUP BY dedup_key, COALESCE(session_key, '')
           );
         CREATE UNIQUE INDEX idx_memory_active_dedup_unique
         ON memory_items(dedup_key, COALESCE(session_key, ''))
         WHERE dedup_key IS NOT NULL AND dedup_key != '' AND deleted_at IS NULL;
         INSERT INTO memory_meta(key, value)
         VALUES ('dedup_unique_index_version', '1')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
    )
    .map_err(|error| {
        blockcell_core::Error::Storage(format!("Failed to migrate memory dedup index: {error}"))
    })?;
    tx.commit().map_err(|error| {
        blockcell_core::Error::Storage(format!("Failed to commit memory dedup migration: {error}"))
    })?;
    Ok(())
}
