use super::*;

impl MemoryStore {
    pub(crate) fn enqueue_vector_sync_on_conn(
        conn: &Connection,
        id: &str,
        operation: &str,
        attempts: i64,
        error: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO memory_vector_queue (id, operation, attempts, last_error, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                operation = excluded.operation,
                attempts = excluded.attempts,
                last_error = excluded.last_error,
                updated_at = excluded.updated_at",
            params![id, operation, attempts, error, now],
        )
        .map_err(|db_error| {
            blockcell_core::Error::Storage(format!(
                "Failed to persist vector sync intent: {}",
                db_error
            ))
        })?;
        Ok(())
    }

    pub(crate) fn enqueue_vector_sync(&self, id: &str, operation: &str, error: &str) {
        let now = Utc::now().to_rfc3339();
        let result = self
            .inner
            .lock()
            .map_err(|lock_error| {
                blockcell_core::Error::Storage(format!("Lock error: {}", lock_error))
            })
            .and_then(|conn| {
                conn.execute(
                    "INSERT INTO memory_vector_queue (id, operation, attempts, last_error, updated_at)
                     VALUES (?1, ?2, 1, ?3, ?4)
                     ON CONFLICT(id) DO UPDATE SET
                        operation = excluded.operation,
                        attempts = memory_vector_queue.attempts + 1,
                        last_error = excluded.last_error,
                        updated_at = excluded.updated_at",
                    params![id, operation, error, now],
                )
                .map_err(|db_error| {
                    blockcell_core::Error::Storage(format!(
                        "Failed to enqueue vector sync: {}",
                        db_error
                    ))
                })?;
                Ok(())
            });

        if let Err(queue_error) = result {
            warn!(
                id,
                operation,
                error = %queue_error,
                "Failed to persist pending vector sync operation"
            );
        }
    }

    pub(crate) fn clear_vector_sync(&self, id: &str) {
        let result = self
            .inner
            .lock()
            .map_err(|lock_error| {
                blockcell_core::Error::Storage(format!("Lock error: {}", lock_error))
            })
            .and_then(|conn| {
                conn.execute("DELETE FROM memory_vector_queue WHERE id = ?1", params![id])
                    .map_err(|db_error| {
                        blockcell_core::Error::Storage(format!(
                            "Failed to clear vector sync queue: {}",
                            db_error
                        ))
                    })?;
                Ok(())
            });

        if let Err(queue_error) = result {
            warn!(id, error = %queue_error, "Failed to clear vector sync queue entry");
        }
    }

    pub(crate) fn seed_reindex_vector_sync(&self, items: &[MemoryItem]) -> Result<()> {
        let mut conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| {
            blockcell_core::Error::Storage(format!("Begin reindex intent transaction error: {}", e))
        })?;
        for item in items {
            Self::enqueue_vector_sync_on_conn(&tx, &item.id, VECTOR_SYNC_OP_UPSERT, 0, None)?;
        }
        tx.commit().map_err(|e| {
            blockcell_core::Error::Storage(format!("Commit reindex intents error: {}", e))
        })?;
        Ok(())
    }

    pub(crate) fn pending_vector_counts(&self) -> Result<(i64, i64, i64)> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_vector_queue", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);
        let upserts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_vector_queue WHERE operation = ?1",
                params![VECTOR_SYNC_OP_UPSERT],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let deletes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_vector_queue WHERE operation = ?1",
                params![VECTOR_SYNC_OP_DELETE],
                |row| row.get(0),
            )
            .unwrap_or(0);

        Ok((total, upserts, deletes))
    }

    pub(crate) fn load_pending_vector_sync(&self, limit: usize) -> Result<Vec<PendingVectorSync>> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        let mut stmt = conn
            .prepare(
                "SELECT id, operation
                 FROM memory_vector_queue
                 ORDER BY updated_at ASC
                 LIMIT ?1",
            )
            .map_err(|e| {
                blockcell_core::Error::Storage(format!("Prepare pending vector sync error: {}", e))
            })?;

        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(PendingVectorSync {
                    id: row.get("id")?,
                    operation: row.get("operation")?,
                })
            })
            .map_err(|e| {
                blockcell_core::Error::Storage(format!("Query pending vector sync error: {}", e))
            })?;

        let mut pending = Vec::new();
        for row in rows {
            pending.push(row.map_err(|e| {
                blockcell_core::Error::Storage(format!("Pending vector sync row error: {}", e))
            })?);
        }
        Ok(pending)
    }

    pub(crate) fn load_reindexable_items(&self) -> Result<Vec<MemoryItem>> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        let now = Utc::now().to_rfc3339();
        let mut stmt = conn
            .prepare(
                "SELECT *
                 FROM memory_items
                 WHERE deleted_at IS NULL
                   AND (expires_at IS NULL OR expires_at > ?1)
                 ORDER BY updated_at DESC",
            )
            .map_err(|e| {
                blockcell_core::Error::Storage(format!("Prepare reindex query error: {}", e))
            })?;

        let rows = stmt
            .query_map(params![now], Self::memory_item_from_row)
            .map_err(|e| blockcell_core::Error::Storage(format!("Reindex query error: {}", e)))?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row.map_err(|e| {
                blockcell_core::Error::Storage(format!("Reindex row error: {}", e))
            })?);
        }
        Ok(items)
    }

    pub(crate) fn try_vector_upsert(&self, item: &MemoryItem) -> Result<()> {
        let runtime = self.vector.as_ref().ok_or_else(|| {
            blockcell_core::Error::Storage("Vector runtime is not enabled".to_string())
        })?;

        let text = build_embedding_text(item);
        let vector = runtime.embedder.embed_document(&text)?;
        let meta = VectorMeta {
            scope: item.scope.clone(),
            item_type: item.item_type.clone(),
            tags: item.tags.clone(),
            session_key: item.session_key.clone(),
        };
        runtime.index.upsert(&item.id, &vector, &meta)
    }

    pub(crate) fn try_vector_delete_ids(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let runtime = self.vector.as_ref().ok_or_else(|| {
            blockcell_core::Error::Storage("Vector runtime is not enabled".to_string())
        })?;
        runtime.index.delete_ids(ids)
    }

    pub(crate) fn sync_vector_upsert(&self, item: &MemoryItem) {
        if self.vector.is_none() {
            return;
        }

        match self.try_vector_upsert(item) {
            Ok(()) => self.clear_vector_sync(&item.id),
            Err(error) => {
                warn!(id = %item.id, error = %error, "Failed to upsert vector index");
                self.enqueue_vector_sync(&item.id, VECTOR_SYNC_OP_UPSERT, &error.to_string());
            }
        }
    }

    pub(crate) fn sync_vector_delete_ids(&self, ids: &[String]) {
        if ids.is_empty() || self.vector.is_none() {
            return;
        }

        match self.try_vector_delete_ids(ids) {
            Ok(()) => {
                for id in ids {
                    self.clear_vector_sync(id);
                }
            }
            Err(error) => {
                warn!(error = %error, count = ids.len(), "Failed to delete vector index entries");
                for id in ids {
                    self.enqueue_vector_sync(id, VECTOR_SYNC_OP_DELETE, &error.to_string());
                }
            }
        }
    }

    pub(crate) fn sync_vector_delete(&self, id: &str) {
        self.sync_vector_delete_ids(&[id.to_string()]);
    }
}
