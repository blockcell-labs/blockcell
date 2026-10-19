use super::*;

impl MemoryStore {
    /// Return active legacy durable rows that still need migration to canonical files.
    pub fn active_long_term_items(&self) -> Result<Vec<MemoryItem>> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        let mut stmt = conn
            .prepare(
                "SELECT * FROM memory_items
                 WHERE scope = 'long_term' AND deleted_at IS NULL
                 ORDER BY updated_at ASC, id ASC",
            )
            .map_err(|e| {
                blockcell_core::Error::Storage(format!(
                    "Prepare legacy long-term query error: {}",
                    e
                ))
            })?;
        let rows = stmt
            .query_map([], Self::memory_item_from_row)
            .map_err(|e| {
                blockcell_core::Error::Storage(format!("Query legacy long-term error: {}", e))
            })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row.map_err(|e| {
                blockcell_core::Error::Storage(format!("Read legacy long-term row error: {}", e))
            })?);
        }
        Ok(items)
    }

    pub(crate) fn query_sqlite_raw(&self, params: &QueryParams) -> Result<Vec<MemoryResult>> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

        let has_fts_query = params.query.as_ref().is_some_and(|q| !q.trim().is_empty());
        let mut sql = String::new();
        let mut where_clauses = Vec::new();
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut bind_idx = 1;
        let cjk_bigrams = params
            .query
            .as_deref()
            .map(crate::fts::cjk_bigrams)
            .unwrap_or_default();

        if has_fts_query {
            if cjk_bigrams.is_empty() {
                sql.push_str(
                    "SELECT m.*, bm25(memory_fts) AS fts_score
                 FROM memory_items m
                 JOIN memory_fts ON memory_fts.rowid = m.rowid
                 WHERE memory_fts MATCH ?1",
                );
                bind_values.push(Box::new(sanitize_fts_query(
                    params.query.as_deref().unwrap_or_default(),
                )));
                bind_idx = 2;
            } else {
                sql.push_str(
                    "SELECT m.*, 0.0 AS fts_score
                     FROM memory_items m
                     JOIN memory_fts ON memory_fts.rowid = m.rowid
                     WHERE (",
                );
                let conditions = cjk_bigrams
                    .iter()
                    .enumerate()
                    .map(|(offset, _)| {
                        let index = bind_idx + offset;
                        format!(
                            "memory_fts.title LIKE ?{index} OR memory_fts.summary LIKE ?{index} OR memory_fts.content LIKE ?{index} OR memory_fts.tags LIKE ?{index}"
                        )
                    })
                    .collect::<Vec<_>>();
                sql.push_str(&conditions.join(" OR "));
                sql.push(')');
                for term in &cjk_bigrams {
                    bind_values.push(Box::new(format!("%{term}%")));
                    bind_idx += 1;
                }
            }
        } else {
            sql.push_str("SELECT m.*, 0.0 AS fts_score FROM memory_items m WHERE 1=1");
        }

        if !params.include_deleted {
            where_clauses.push("m.deleted_at IS NULL".to_string());
        }

        if let Some(ref session_key) = params.session_key {
            where_clauses.push(format!(
                "(m.session_key = ?{} OR m.session_key IS NULL)",
                bind_idx
            ));
            bind_values.push(Box::new(session_key.clone()));
            bind_idx += 1;
        }

        if let Some(ref scope) = params.scope {
            where_clauses.push(format!("m.scope = ?{}", bind_idx));
            bind_values.push(Box::new(scope.clone()));
            bind_idx += 1;
        }

        if let Some(ref item_type) = params.item_type {
            where_clauses.push(format!("m.type = ?{}", bind_idx));
            bind_values.push(Box::new(item_type.clone()));
            bind_idx += 1;
        }

        if let Some(ref tags) = params.tags {
            if !tags.is_empty() {
                let tag_conditions: Vec<String> = tags
                    .iter()
                    .enumerate()
                    .map(|(offset, _)| {
                        format!(
                            "instr(',' || m.tags || ',', ',' || ?{} || ',') > 0",
                            bind_idx + offset
                        )
                    })
                    .collect();
                where_clauses.push(format!("({})", tag_conditions.join(" OR ")));
                for tag in tags {
                    bind_values.push(Box::new(tag.clone()));
                    bind_idx += 1;
                }
            }
        }

        if let Some(days) = params.time_range_days {
            let cutoff = (Utc::now() - chrono::Duration::days(days)).to_rfc3339();
            where_clauses.push(format!("m.created_at >= ?{}", bind_idx));
            bind_values.push(Box::new(cutoff));
            bind_idx += 1;
        }

        if !params.include_deleted {
            where_clauses.push(format!(
                "(m.expires_at IS NULL OR m.expires_at > ?{})",
                bind_idx
            ));
            bind_values.push(Box::new(Utc::now().to_rfc3339()));
        }

        for clause in &where_clauses {
            sql.push_str(&format!(" AND {}", clause));
        }

        sql.push_str(" ORDER BY ");
        if has_fts_query {
            sql.push_str(
                "(-fts_score * 10.0 + m.importance * 5.0 + \
                 CASE WHEN julianday('now') - julianday(m.updated_at) < 1 THEN 3.0 \
                      WHEN julianday('now') - julianday(m.updated_at) < 7 THEN 1.5 \
                      ELSE 0.0 END) DESC",
            );
        } else {
            sql.push_str("m.importance DESC, m.updated_at DESC");
        }
        sql.push_str(&format!(" LIMIT {}", params.bounded_top_k()));

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| blockcell_core::Error::Storage(format!("Prepare error: {}", e)))?;
        let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|value| value.as_ref()).collect();

        let rows = stmt
            .query_map(bind_refs.as_slice(), |row| {
                let fts_score: f64 = row.get("fts_score")?;
                let item = Self::memory_item_from_row(row)?;
                Ok(MemoryResult {
                    score: -fts_score * 10.0 + item.importance * 5.0,
                    item,
                })
            })
            .map_err(|e| blockcell_core::Error::Storage(format!("Query error: {}", e)))?;

        let mut results = Vec::new();
        for row in rows {
            match row {
                Ok(result) => results.push(result),
                Err(error) => warn!(error = %error, "Error reading memory row"),
            }
        }

        Ok(results)
    }

    pub(crate) fn search_fts_candidates(
        &self,
        raw_query: &str,
        query_params: &QueryParams,
        top_k: usize,
    ) -> Result<Vec<(String, f64)>> {
        if top_k == 0 {
            return Ok(Vec::new());
        }

        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        let cjk_bigrams = crate::fts::cjk_bigrams(raw_query);
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut bind_idx = 1;
        let mut sql = if cjk_bigrams.is_empty() {
            bind_values.push(Box::new(sanitize_fts_query(raw_query)));
            bind_idx = 2;
            String::from(
                "SELECT m.id, bm25(memory_fts) AS fts_score
             FROM memory_items m
             JOIN memory_fts ON memory_fts.rowid = m.rowid
             WHERE memory_fts MATCH ?1",
            )
        } else {
            let conditions = cjk_bigrams
                .iter()
                .enumerate()
                .map(|(offset, _)| {
                    let index = offset + 1;
                    format!(
                        "memory_fts.title LIKE ?{index} OR memory_fts.summary LIKE ?{index} OR memory_fts.content LIKE ?{index} OR memory_fts.tags LIKE ?{index}"
                    )
                })
                .collect::<Vec<_>>()
                .join(" OR ");
            for term in &cjk_bigrams {
                bind_values.push(Box::new(format!("%{term}%")));
                bind_idx += 1;
            }
            format!(
                "SELECT m.id, 0.0 AS fts_score
                 FROM memory_items m
                 JOIN memory_fts ON memory_fts.rowid = m.rowid
                 WHERE ({conditions})"
            )
        };

        if !query_params.include_deleted {
            sql.push_str(" AND m.deleted_at IS NULL");
        }
        if let Some(ref session_key) = query_params.session_key {
            sql.push_str(&format!(
                " AND (m.session_key = ?{} OR m.session_key IS NULL)",
                bind_idx
            ));
            bind_values.push(Box::new(session_key.clone()));
            bind_idx += 1;
        }
        if let Some(ref scope) = query_params.scope {
            sql.push_str(&format!(" AND m.scope = ?{}", bind_idx));
            bind_values.push(Box::new(scope.clone()));
            bind_idx += 1;
        }
        if let Some(ref item_type) = query_params.item_type {
            sql.push_str(&format!(" AND m.type = ?{}", bind_idx));
            bind_values.push(Box::new(item_type.clone()));
            bind_idx += 1;
        }
        if let Some(ref tags) = query_params.tags {
            if !tags.is_empty() {
                let conditions: Vec<String> = tags
                    .iter()
                    .enumerate()
                    .map(|(offset, _)| {
                        format!(
                            "instr(',' || m.tags || ',', ',' || ?{} || ',') > 0",
                            bind_idx + offset
                        )
                    })
                    .collect();
                sql.push_str(&format!(" AND ({})", conditions.join(" OR ")));
                for tag in tags {
                    bind_values.push(Box::new(tag.clone()));
                    bind_idx += 1;
                }
            }
        }
        if let Some(days) = query_params.time_range_days {
            sql.push_str(&format!(" AND m.created_at >= ?{}", bind_idx));
            bind_values.push(Box::new(
                (Utc::now() - chrono::Duration::days(days)).to_rfc3339(),
            ));
            bind_idx += 1;
        }
        if !query_params.include_deleted {
            sql.push_str(&format!(
                " AND (m.expires_at IS NULL OR m.expires_at > ?{})",
                bind_idx
            ));
            bind_values.push(Box::new(Utc::now().to_rfc3339()));
            bind_idx += 1;
        }
        if cjk_bigrams.is_empty() {
            sql.push_str(&format!(
                " ORDER BY bm25(memory_fts) ASC LIMIT ?{}",
                bind_idx
            ));
        } else {
            sql.push_str(&format!(" ORDER BY m.importance DESC LIMIT ?{}", bind_idx));
        }
        bind_values.push(Box::new(top_k as i64));

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| blockcell_core::Error::Storage(format!("Prepare error: {}", e)))?;
        let bind_refs: Vec<&dyn rusqlite::types::ToSql> =
            bind_values.iter().map(|value| value.as_ref()).collect();

        let rows = stmt
            .query_map(bind_refs.as_slice(), |row| {
                Ok((row.get("id")?, row.get("fts_score")?))
            })
            .map_err(|e| blockcell_core::Error::Storage(format!("FTS query error: {}", e)))?;

        let mut results = Vec::new();
        for row in rows {
            match row {
                Ok(result) => results.push(result),
                Err(error) => warn!(error = %error, "Error reading FTS candidate row"),
            }
        }
        Ok(results)
    }

    pub(crate) fn load_items_by_ids(&self, ids: &[String]) -> Result<Vec<MemoryItem>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        // Batch the lookup (candidate windows are ~200 ids): one prepared
        // statement per chunk instead of one round-trip per id. Chunked to stay
        // below SQLite's variable limit.
        const MAX_IDS_PER_QUERY: usize = 500;
        let mut found: std::collections::HashMap<String, MemoryItem> =
            std::collections::HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(MAX_IDS_PER_QUERY) {
            let placeholders = (1..=chunk.len())
                .map(|index| format!("?{}", index))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!("SELECT * FROM memory_items WHERE id IN ({})", placeholders);
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| blockcell_core::Error::Storage(format!("Load by id error: {}", e)))?;
            let rows = stmt
                .query_map(
                    rusqlite::params_from_iter(chunk.iter()),
                    Self::memory_item_from_row,
                )
                .map_err(|e| blockcell_core::Error::Storage(format!("Load by id error: {}", e)))?;
            for row in rows {
                let item = row.map_err(|e| {
                    blockcell_core::Error::Storage(format!("Load by id error: {}", e))
                })?;
                found.insert(item.id.clone(), item);
            }
        }
        // Preserve the caller's order (ranking) and drop missing ids.
        Ok(ids.iter().filter_map(|id| found.remove(id)).collect())
    }

    pub(crate) fn item_matches_query(&self, item: &MemoryItem, params: &QueryParams) -> bool {
        if !params.include_deleted && item.deleted_at.is_some() {
            return false;
        }

        if let Some(ref session_key) = params.session_key {
            if item
                .session_key
                .as_ref()
                .is_some_and(|owner| owner != session_key)
            {
                return false;
            }
        }

        if let Some(ref scope) = params.scope {
            if item.scope != *scope {
                return false;
            }
        }

        if let Some(ref item_type) = params.item_type {
            if item.item_type != *item_type {
                return false;
            }
        }

        if let Some(ref wanted_tags) = params.tags {
            if !wanted_tags.is_empty()
                && !item
                    .tags
                    .iter()
                    .any(|tag| wanted_tags.iter().any(|wanted| tag == wanted))
            {
                return false;
            }
        }

        if let Some(days) = params.time_range_days {
            let cutoff = Utc::now() - chrono::Duration::days(days);
            let created_at = match DateTime::parse_from_rfc3339(&item.created_at) {
                Ok(value) => value.with_timezone(&Utc),
                Err(_) => return false,
            };
            if created_at < cutoff {
                return false;
            }
        }

        if !params.include_deleted {
            if let Some(ref expires_at) = item.expires_at {
                match DateTime::parse_from_rfc3339(expires_at) {
                    Ok(value) if value.with_timezone(&Utc) <= Utc::now() => return false,
                    Err(_) => return false,
                    _ => {}
                }
            }
        }

        true
    }

    pub(crate) fn record_accesses(&self, results: &[MemoryResult]) -> Result<()> {
        if results.is_empty() {
            return Ok(());
        }

        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        let now = Utc::now().to_rfc3339();
        for result in results {
            conn.execute(
                "UPDATE memory_items SET access_count = access_count + 1, last_accessed_at = ?1 WHERE id = ?2",
                params![now, result.item.id],
            )
            .map_err(|e| blockcell_core::Error::Storage(format!("Access update error: {}", e)))?;
        }

        Ok(())
    }
}
