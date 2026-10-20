use super::*;

impl MemoryStore {
    /// Upsert a memory item. If dedup_key is set and a matching non-deleted item exists,
    /// update it instead of inserting a new one.
    pub fn upsert(&self, params: UpsertParams) -> Result<MemoryItem> {
        if params.content.chars().count() > MAX_MEMORY_CONTENT_CHARS {
            return Err(blockcell_core::Error::Validation(format!(
                "memory content exceeds the {MAX_MEMORY_CONTENT_CHARS} character limit"
            )));
        }
        // `importance` feeds the ranking formula directly and tags are stored
        // as a comma-separated CSV column, so both must be normalized at the
        // single write entry point rather than trusting callers.
        if !(0.0..=1.0).contains(&params.importance) {
            return Err(blockcell_core::Error::Validation(format!(
                "importance must be within 0.0..=1.0, got {}",
                params.importance
            )));
        }
        let mut normalized_tags = Vec::with_capacity(params.tags.len());
        for tag in &params.tags {
            let tag = tag.trim();
            if tag.is_empty() {
                continue;
            }
            if tag.contains(',') {
                return Err(blockcell_core::Error::Validation(
                    "tags must not contain ',' (the column is comma-separated)".to_string(),
                ));
            }
            normalized_tags.push(tag.to_string());
        }

        let item = {
            let mut conn = self
                .inner
                .lock()
                .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| {
                    blockcell_core::Error::Storage(format!("Begin upsert transaction error: {}", e))
                })?;

            let now = Utc::now().to_rfc3339();
            let tags_str = normalized_tags.join(",");

            let item = if let Some(ref dk) = params.dedup_key {
                if !dk.is_empty() {
                    let existing_id: Option<String> = tx
                        .query_row(
                            "SELECT id FROM memory_items
                             WHERE dedup_key = ?1
                               AND COALESCE(session_key, '') = COALESCE(?2, '')
                               AND deleted_at IS NULL
                             LIMIT 1",
                            params![dk, params.session_key],
                            |row| row.get(0),
                        )
                        .optional()
                        .map_err(|e| {
                            blockcell_core::Error::Storage(format!("Query error: {}", e))
                        })?;

                    if let Some(id) = existing_id {
                        tx.execute(
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
                        self.get_by_id_inner(&tx, &id)?
                    } else {
                        let id = uuid::Uuid::new_v4().to_string();
                        tx.execute(
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
                        self.get_by_id_inner(&tx, &id)?
                    }
                } else {
                    let id = uuid::Uuid::new_v4().to_string();
                    tx.execute(
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
                    self.get_by_id_inner(&tx, &id)?
                }
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                tx.execute(
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
                self.get_by_id_inner(&tx, &id)?
            };

            if self.vector.is_some() {
                Self::enqueue_vector_sync_on_conn(&tx, &item.id, VECTOR_SYNC_OP_UPSERT, 0, None)?;
            }
            tx.commit().map_err(|e| {
                blockcell_core::Error::Storage(format!("Commit upsert transaction error: {}", e))
            })?;
            item
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

    pub(crate) fn get_by_id_inner(&self, conn: &Connection, id: &str) -> Result<MemoryItem> {
        conn.query_row(
            "SELECT * FROM memory_items WHERE id = ?1",
            params![id],
            Self::memory_item_from_row,
        )
        .map_err(|e| blockcell_core::Error::Storage(format!("Get by id error: {}", e)))
    }

    pub(crate) fn memory_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryItem> {
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
}
