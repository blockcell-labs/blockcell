use super::*;

impl MemoryStore {
    /// Soft-delete only active legacy long-term rows after a successful file migration.
    pub fn retire_long_term_items(&self, ids: &[String]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }

        let retired_ids = {
            let mut conn = self
                .inner
                .lock()
                .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| {
                blockcell_core::Error::Storage(format!(
                    "Begin long-term retirement transaction error: {}",
                    e
                ))
            })?;
            let now = Utc::now().to_rfc3339();
            let mut retired_ids = Vec::new();
            for id in ids {
                let affected = tx
                    .execute(
                        "UPDATE memory_items
                         SET deleted_at = ?1
                         WHERE id = ?2 AND scope = 'long_term' AND deleted_at IS NULL",
                        params![now, id],
                    )
                    .map_err(|e| {
                        blockcell_core::Error::Storage(format!(
                            "Retire legacy long-term row error: {}",
                            e
                        ))
                    })?;
                if affected > 0 {
                    if self.vector.is_some() {
                        Self::enqueue_vector_sync_on_conn(&tx, id, VECTOR_SYNC_OP_DELETE, 0, None)?;
                    }
                    retired_ids.push(id.clone());
                }
            }
            tx.commit().map_err(|e| {
                blockcell_core::Error::Storage(format!(
                    "Commit long-term retirement transaction error: {}",
                    e
                ))
            })?;
            retired_ids
        };

        self.sync_vector_delete_ids(&retired_ids);
        Ok(retired_ids.len())
    }

    /// Soft-delete a memory item.
    pub fn soft_delete(&self, id: &str) -> Result<bool> {
        self.soft_delete_with_owner(id, None)
    }

    pub fn soft_delete_in_session(&self, id: &str, session_key: &str) -> Result<bool> {
        self.soft_delete_with_owner(id, Some(session_key))
    }

    fn soft_delete_with_owner(&self, id: &str, session_key: Option<&str>) -> Result<bool> {
        let deleted = {
            let mut conn = self
                .inner
                .lock()
                .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| {
                blockcell_core::Error::Storage(format!(
                    "Begin soft-delete transaction error: {}",
                    e
                ))
            })?;
            let now = Utc::now().to_rfc3339();
            let affected = tx
                .execute(
                    "UPDATE memory_items
                     SET deleted_at = ?1
                     WHERE id = ?2
                       AND deleted_at IS NULL
                       AND (?3 IS NULL OR session_key = ?3)",
                    params![now, id, session_key],
                )
                .map_err(|e| blockcell_core::Error::Storage(format!("Soft delete error: {}", e)))?;
            if affected > 0 && self.vector.is_some() {
                Self::enqueue_vector_sync_on_conn(&tx, id, VECTOR_SYNC_OP_DELETE, 0, None)?;
            }
            tx.commit().map_err(|e| {
                blockcell_core::Error::Storage(format!(
                    "Commit soft-delete transaction error: {}",
                    e
                ))
            })?;
            affected > 0
        };

        if deleted {
            self.sync_vector_delete(id);
        }

        Ok(deleted)
    }

    /// Batch soft-delete by filter criteria.
    pub fn batch_soft_delete(
        &self,
        scope: Option<&str>,
        item_type: Option<&str>,
        tags: Option<&[String]>,
        time_before: Option<&str>,
    ) -> Result<usize> {
        self.batch_soft_delete_with_owner(None, scope, item_type, tags, time_before)
    }

    pub fn batch_soft_delete_in_session(
        &self,
        session_key: &str,
        scope: Option<&str>,
        item_type: Option<&str>,
        tags: Option<&[String]>,
        time_before: Option<&str>,
    ) -> Result<usize> {
        self.batch_soft_delete_with_owner(Some(session_key), scope, item_type, tags, time_before)
    }

    fn batch_soft_delete_with_owner(
        &self,
        session_key: Option<&str>,
        scope: Option<&str>,
        item_type: Option<&str>,
        tags: Option<&[String]>,
        time_before: Option<&str>,
    ) -> Result<usize> {
        let ids = {
            let mut conn = self
                .inner
                .lock()
                .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| {
                blockcell_core::Error::Storage(format!(
                    "Begin batch-delete transaction error: {}",
                    e
                ))
            })?;

            let mut sql = "SELECT id FROM memory_items WHERE deleted_at IS NULL".to_string();
            let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            let mut idx = 1;

            if let Some(session_key) = session_key {
                sql.push_str(&format!(" AND session_key = ?{}", idx));
                bind_values.push(Box::new(session_key.to_string()));
                idx += 1;
            }

            if let Some(s) = scope {
                sql.push_str(&format!(" AND scope = ?{}", idx));
                bind_values.push(Box::new(s.to_string()));
                idx += 1;
            }
            if let Some(t) = item_type {
                sql.push_str(&format!(" AND type = ?{}", idx));
                bind_values.push(Box::new(t.to_string()));
                idx += 1;
            }
            if let Some(tag_list) = tags {
                if !tag_list.is_empty() {
                    let mut tag_conditions = Vec::new();
                    for tag in tag_list {
                        tag_conditions.push(format!(
                            "instr(',' || memory_items.tags || ',', ',' || ?{} || ',') > 0",
                            idx
                        ));
                        bind_values.push(Box::new(tag.clone()));
                        idx += 1;
                    }
                    sql.push_str(&format!(" AND ({})", tag_conditions.join(" OR ")));
                }
            }
            if let Some(before) = time_before {
                sql.push_str(&format!(" AND created_at < ?{}", idx));
                bind_values.push(Box::new(before.to_string()));
            }

            let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
                bind_values.iter().map(|b| b.as_ref()).collect();
            let mut stmt = tx.prepare(&sql).map_err(|e| {
                blockcell_core::Error::Storage(format!("Batch delete prepare error: {}", e))
            })?;
            let rows = stmt
                .query_map(bind_refs.as_slice(), |row| row.get::<_, String>(0))
                .map_err(|e| {
                    blockcell_core::Error::Storage(format!("Batch delete select error: {}", e))
                })?;

            let mut ids = Vec::new();
            for row in rows {
                ids.push(row.map_err(|e| {
                    blockcell_core::Error::Storage(format!("Batch delete id row error: {}", e))
                })?);
            }

            if ids.is_empty() {
                drop(stmt);
                tx.commit().map_err(|e| {
                    blockcell_core::Error::Storage(format!(
                        "Commit batch-delete transaction error: {}",
                        e
                    ))
                })?;
                return Ok(0);
            }

            drop(stmt);

            let now = Utc::now().to_rfc3339();
            let placeholders = (0..ids.len())
                .map(|offset| format!("?{}", offset + 2))
                .collect::<Vec<_>>()
                .join(", ");
            let update_sql = format!(
                "UPDATE memory_items SET deleted_at = ?1 WHERE id IN ({})",
                placeholders
            );

            let mut update_values: Vec<Box<dyn rusqlite::types::ToSql>> =
                Vec::with_capacity(ids.len() + 1);
            update_values.push(Box::new(now));
            for id in &ids {
                update_values.push(Box::new(id.clone()));
            }
            let update_refs: Vec<&dyn rusqlite::types::ToSql> =
                update_values.iter().map(|value| value.as_ref()).collect();

            tx.execute(&update_sql, update_refs.as_slice())
                .map_err(|e| {
                    blockcell_core::Error::Storage(format!("Batch delete update error: {}", e))
                })?;

            if self.vector.is_some() {
                for id in &ids {
                    Self::enqueue_vector_sync_on_conn(&tx, id, VECTOR_SYNC_OP_DELETE, 0, None)?;
                }
            }
            tx.commit().map_err(|e| {
                blockcell_core::Error::Storage(format!(
                    "Commit batch-delete transaction error: {}",
                    e
                ))
            })?;

            ids
        };

        self.sync_vector_delete_ids(&ids);
        info!(count = ids.len(), "Batch soft-deleted memory items");
        Ok(ids.len())
    }

    /// Restore a soft-deleted item.
    pub fn restore(&self, id: &str) -> Result<bool> {
        self.restore_with_owner(id, None)
    }

    pub fn restore_in_session(&self, id: &str, session_key: &str) -> Result<bool> {
        self.restore_with_owner(id, Some(session_key))
    }

    fn restore_with_owner(&self, id: &str, session_key: Option<&str>) -> Result<bool> {
        let (restored_item, displaced_ids) = {
            let mut conn = self
                .inner
                .lock()
                .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| {
                blockcell_core::Error::Storage(format!("Begin restore transaction error: {}", e))
            })?;
            // 同 (dedup_key, session_key) 若已有活动行，直接恢复会触发部分唯一索引冲突：
            // 在同一事务内先将冲突的旧活动项软删除（并入队向量删除），再恢复目标项。
            let displaced_ids: Vec<String> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT t2.id FROM memory_items t1
                         JOIN memory_items t2
                           ON t1.dedup_key = t2.dedup_key
                          AND COALESCE(t1.session_key, '') = COALESCE(t2.session_key, '')
                         WHERE t1.id = ?1
                           AND t1.deleted_at IS NOT NULL
                           AND t1.dedup_key IS NOT NULL
                           AND t1.dedup_key != ''
                           AND t2.deleted_at IS NULL
                           AND t2.id != t1.id
                           AND (?2 IS NULL OR t1.session_key = ?2)",
                    )
                    .map_err(|e| {
                        blockcell_core::Error::Storage(format!(
                            "Restore conflict query prepare error: {}",
                            e
                        ))
                    })?;
                let rows = stmt
                    .query_map(params![id, session_key], |row| row.get::<_, String>(0))
                    .map_err(|e| {
                        blockcell_core::Error::Storage(format!(
                            "Restore conflict query error: {}",
                            e
                        ))
                    })?;
                rows.filter_map(|r| r.ok()).collect()
            };
            if !displaced_ids.is_empty() {
                let now = Utc::now().to_rfc3339();
                for conflict_id in &displaced_ids {
                    tx.execute(
                        "UPDATE memory_items SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
                        params![now, conflict_id],
                    )
                    .map_err(|e| {
                        blockcell_core::Error::Storage(format!(
                            "Displace conflicting active item error: {}",
                            e
                        ))
                    })?;
                    if self.vector.is_some() {
                        Self::enqueue_vector_sync_on_conn(
                            &tx,
                            conflict_id,
                            VECTOR_SYNC_OP_DELETE,
                            0,
                            None,
                        )?;
                    }
                }
            }
            let affected = tx
                .execute(
                    "UPDATE memory_items
                     SET deleted_at = NULL
                     WHERE id = ?1
                       AND deleted_at IS NOT NULL
                       AND (?2 IS NULL OR session_key = ?2)",
                    params![id, session_key],
                )
                .map_err(|e| blockcell_core::Error::Storage(format!("Restore error: {}", e)))?;

            if affected == 0 {
                tx.commit().map_err(|e| {
                    blockcell_core::Error::Storage(format!(
                        "Commit restore transaction error: {}",
                        e
                    ))
                })?;
                (None, Vec::new())
            } else {
                let item = self.get_by_id_inner(&tx, id)?;
                if self.vector.is_some() {
                    Self::enqueue_vector_sync_on_conn(&tx, id, VECTOR_SYNC_OP_UPSERT, 0, None)?;
                }
                tx.commit().map_err(|e| {
                    blockcell_core::Error::Storage(format!(
                        "Commit restore transaction error: {}",
                        e
                    ))
                })?;
                (Some(item), displaced_ids)
            }
        };

        if !displaced_ids.is_empty() {
            self.sync_vector_delete_ids(&displaced_ids);
        }
        if let Some(item) = restored_item {
            self.sync_vector_upsert(&item);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Clean up expired items (set deleted_at) and hard-delete items that have been
    /// soft-deleted for more than `recycle_days` days.
    pub fn maintenance(&self, recycle_days: i64) -> Result<(usize, usize)> {
        let (expired_ids, purged_ids) = {
            let mut conn = self
                .inner
                .lock()
                .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| {
                blockcell_core::Error::Storage(format!(
                    "Begin maintenance transaction error: {}",
                    e
                ))
            })?;

            let now = Utc::now().to_rfc3339();
            let cutoff = (Utc::now() - chrono::Duration::days(recycle_days)).to_rfc3339();

            let expired_ids = {
                let mut stmt = tx
                    .prepare(
                        "SELECT id FROM memory_items
                         WHERE expires_at IS NOT NULL
                           AND expires_at <= ?1
                           AND deleted_at IS NULL",
                    )
                    .map_err(|e| {
                        blockcell_core::Error::Storage(format!("TTL cleanup prepare error: {}", e))
                    })?;
                let rows = stmt
                    .query_map(params![now], |row| row.get::<_, String>(0))
                    .map_err(|e| {
                        blockcell_core::Error::Storage(format!("TTL cleanup query error: {}", e))
                    })?;
                let mut ids = Vec::new();
                for row in rows {
                    ids.push(row.map_err(|e| {
                        blockcell_core::Error::Storage(format!("TTL cleanup id row error: {}", e))
                    })?);
                }
                ids
            };

            let purged_ids = {
                let mut stmt = tx
                    .prepare(
                        "SELECT id FROM memory_items
                         WHERE deleted_at IS NOT NULL
                           AND deleted_at < ?1",
                    )
                    .map_err(|e| {
                        blockcell_core::Error::Storage(format!("Purge prepare error: {}", e))
                    })?;
                let rows = stmt
                    .query_map(params![cutoff], |row| row.get::<_, String>(0))
                    .map_err(|e| {
                        blockcell_core::Error::Storage(format!("Purge query error: {}", e))
                    })?;
                let mut ids = Vec::new();
                for row in rows {
                    ids.push(row.map_err(|e| {
                        blockcell_core::Error::Storage(format!("Purge id row error: {}", e))
                    })?);
                }
                ids
            };

            if !expired_ids.is_empty() {
                let placeholders = (0..expired_ids.len())
                    .map(|offset| format!("?{}", offset + 2))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "UPDATE memory_items SET deleted_at = ?1 WHERE id IN ({})",
                    placeholders
                );
                let mut values: Vec<Box<dyn rusqlite::types::ToSql>> =
                    Vec::with_capacity(expired_ids.len() + 1);
                values.push(Box::new(now));
                for id in &expired_ids {
                    values.push(Box::new(id.clone()));
                }
                let refs: Vec<&dyn rusqlite::types::ToSql> =
                    values.iter().map(|value| value.as_ref()).collect();
                tx.execute(&sql, refs.as_slice()).map_err(|e| {
                    blockcell_core::Error::Storage(format!("TTL cleanup update error: {}", e))
                })?;
            }

            if !purged_ids.is_empty() {
                let placeholders = (0..purged_ids.len())
                    .map(|offset| format!("?{}", offset + 1))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!("DELETE FROM memory_items WHERE id IN ({})", placeholders);
                let values: Vec<&dyn rusqlite::types::ToSql> = purged_ids
                    .iter()
                    .map(|id| id as &dyn rusqlite::types::ToSql)
                    .collect();
                tx.execute(&sql, values.as_slice()).map_err(|e| {
                    blockcell_core::Error::Storage(format!("Purge delete error: {}", e))
                })?;
            }

            if self.vector.is_some() {
                for id in expired_ids.iter().chain(purged_ids.iter()) {
                    Self::enqueue_vector_sync_on_conn(&tx, id, VECTOR_SYNC_OP_DELETE, 0, None)?;
                }
            }
            tx.commit().map_err(|e| {
                blockcell_core::Error::Storage(format!(
                    "Commit maintenance transaction error: {}",
                    e
                ))
            })?;

            (expired_ids, purged_ids)
        };

        let mut deleted_ids = expired_ids.clone();
        deleted_ids.extend(purged_ids.iter().cloned());
        self.sync_vector_delete_ids(&deleted_ids);

        if !expired_ids.is_empty() || !purged_ids.is_empty() {
            info!(
                expired = expired_ids.len(),
                purged = purged_ids.len(),
                "Memory maintenance completed"
            );
        }

        Ok((expired_ids.len(), purged_ids.len()))
    }

    pub fn retry_vector_sync(&self, limit: usize) -> Result<VectorSyncRetryResult> {
        if self.vector.is_none() {
            return Err(blockcell_core::Error::Storage(
                "Vector runtime is not enabled".to_string(),
            ));
        }

        let pending = self.load_pending_vector_sync(limit)?;
        let mut result = VectorSyncRetryResult {
            attempted: pending.len(),
            succeeded: 0,
            failed: 0,
        };

        for entry in pending {
            let sync_result = match entry.operation.as_str() {
                VECTOR_SYNC_OP_DELETE => {
                    self.try_vector_delete_ids(std::slice::from_ref(&entry.id))
                }
                VECTOR_SYNC_OP_UPSERT => match self.get_by_id(&entry.id)? {
                    Some(item) if is_item_active_for_vector(&item) => self.try_vector_upsert(&item),
                    _ => self.try_vector_delete_ids(std::slice::from_ref(&entry.id)),
                },
                other => Err(blockcell_core::Error::Storage(format!(
                    "Unknown vector sync operation: {}",
                    other
                ))),
            };

            match sync_result {
                Ok(()) => {
                    self.clear_vector_sync(&entry.id);
                    result.succeeded += 1;
                }
                Err(error) => {
                    self.enqueue_vector_sync(&entry.id, &entry.operation, &error.to_string());
                    warn!(
                        id = %entry.id,
                        operation = %entry.operation,
                        error = %error,
                        "Retrying vector sync failed"
                    );
                    result.failed += 1;
                }
            }
        }

        Ok(result)
    }

    pub fn reindex_vectors(&self) -> Result<VectorReindexResult> {
        let runtime = self.vector.as_ref().ok_or_else(|| {
            blockcell_core::Error::Storage("Vector runtime is not enabled".to_string())
        })?;

        let items = self.load_reindexable_items()?;
        self.seed_reindex_vector_sync(&items)?;
        runtime.index.reset()?;
        let mut result = VectorReindexResult {
            indexed: 0,
            failed: 0,
        };

        for item in items {
            match self.reindex_vector_item(&item.id) {
                Ok(()) => {
                    self.clear_vector_sync(&item.id);
                    result.indexed += 1;
                }
                Err(error) => {
                    self.enqueue_vector_sync(&item.id, VECTOR_SYNC_OP_UPSERT, &error.to_string());
                    warn!(id = %item.id, error = %error, "Failed to reindex vector entry");
                    result.failed += 1;
                }
            }
        }

        Ok(result)
    }

    fn reindex_vector_item(&self, id: &str) -> Result<()> {
        for _ in 0..3 {
            let Some(item) = self.get_by_id(id)? else {
                self.try_vector_delete_ids(&[id.to_string()])?;
                return Ok(());
            };
            if !is_item_active_for_vector(&item) {
                self.try_vector_delete_ids(&[id.to_string()])?;
                return Ok(());
            }

            self.try_vector_upsert(&item)?;
            match self.get_by_id(id)? {
                Some(current)
                    if is_item_active_for_vector(&current)
                        && current.updated_at == item.updated_at =>
                {
                    return Ok(())
                }
                Some(current) if is_item_active_for_vector(&current) => continue,
                _ => {
                    self.try_vector_delete_ids(&[id.to_string()])?;
                    return Ok(());
                }
            }
        }

        Err(blockcell_core::Error::Storage(format!(
            "Memory item {} kept changing during vector reindex",
            id
        )))
    }
}
