use super::*;

impl MemoryStore {
    /// Upsert a session summary for prompt injection.
    /// Uses dedup_key = "session_summary:{session_key}" so each session has exactly one summary.
    pub fn upsert_session_summary(&self, session_key: &str, summary: &str) -> Result<()> {
        let dedup_key = format!("session_summary:{}", session_key);
        let params = UpsertParams {
            scope: "short_term".to_string(),
            item_type: "session_summary".to_string(),
            title: Some(format!("Session: {}", session_key)),
            content: summary.to_string(),
            summary: None,
            tags: vec!["session_summary".to_string()],
            source: "ghost".to_string(),
            channel: None,
            session_key: Some(session_key.to_string()),
            importance: 0.8,
            dedup_key: Some(dedup_key),
            expires_at: None,
        };
        self.upsert(params)?;
        Ok(())
    }

    /// Get the session summary for a given session key, if one exists.
    pub fn get_session_summary(&self, session_key: &str) -> Result<Option<String>> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

        let dedup_key = format!("session_summary:{}", session_key);
        let result: Option<String> = conn
            .query_row(
                "SELECT content FROM memory_items WHERE dedup_key = ?1 AND deleted_at IS NULL",
                params![dedup_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| blockcell_core::Error::Storage(format!("Query error: {}", e)))?;

        Ok(result)
    }

    /// Generate a brief summary for prompt injection.
    /// Returns up to `long_term_max` long-term summaries and `short_term_max` short-term summaries.
    pub fn generate_brief(&self, long_term_max: usize, short_term_max: usize) -> Result<String> {
        self.generate_brief_with_session(None, long_term_max, short_term_max)
    }

    pub fn generate_brief_in_session(
        &self,
        session_key: &str,
        long_term_max: usize,
        short_term_max: usize,
    ) -> Result<String> {
        self.generate_brief_with_session(Some(session_key), long_term_max, short_term_max)
    }

    fn generate_brief_with_session(
        &self,
        session_key: Option<&str>,
        long_term_max: usize,
        short_term_max: usize,
    ) -> Result<String> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

        let now = Utc::now().to_rfc3339();
        let mut brief = String::new();

        let lt_items = collect_brief_items(
            &conn,
            BriefScope::LongTerm,
            session_key,
            &now,
            long_term_max,
        )?;
        if !lt_items.is_empty() {
            brief.push_str("### Long-term Memory\n");
            for item in &lt_items {
                brief.push_str(item);
                brief.push('\n');
            }
            brief.push('\n');
        }

        let st_items = collect_brief_items(
            &conn,
            BriefScope::ShortTerm,
            session_key,
            &now,
            short_term_max,
        )?;
        if !st_items.is_empty() {
            brief.push_str("### Recent Notes\n");
            for item in &st_items {
                brief.push_str(item);
                brief.push('\n');
            }
        }

        Ok(brief)
    }

    /// Generate a brief summary for prompt injection, filtered by relevance to a query.
    /// Uses FTS5 to find memories related to the current user input.
    /// Falls back to generate_brief() when query is empty.
    pub fn generate_brief_for_query(&self, query: &str, max_items: usize) -> Result<String> {
        self.generate_brief_for_query_with_session(None, query, max_items)
    }

    pub fn generate_brief_for_query_in_session(
        &self,
        session_key: &str,
        query: &str,
        max_items: usize,
    ) -> Result<String> {
        self.generate_brief_for_query_with_session(Some(session_key), query, max_items)
    }

    fn generate_brief_for_query_with_session(
        &self,
        session_key: Option<&str>,
        query: &str,
        max_items: usize,
    ) -> Result<String> {
        let query = query.trim();
        if query.is_empty() || max_items == 0 {
            // Fallback: return a small general brief
            return match session_key {
                Some(session_key) => self.generate_brief_in_session(session_key, 5, 3),
                None => self.generate_brief(5, 3),
            };
        }

        let items = HybridMemoryRetriever::new(self).search(&QueryParams {
            query: Some(query.to_string()),
            session_key: session_key.map(str::to_string),
            top_k: max_items,
            ..Default::default()
        })?;

        if items.is_empty() {
            // No relevant matches — return a minimal general brief.
            return match session_key {
                Some(session_key) => self.generate_brief_in_session(session_key, 3, 2),
                None => self.generate_brief(3, 2),
            };
        }

        let mut brief = String::new();
        brief.push_str("### Relevant Memory\n");
        for result in &items {
            brief.push_str(&format_relevant_brief_item(&result.item));
            brief.push('\n');
        }
        Ok(brief)
    }

    /// Get statistics about the memory store.
    pub fn stats(&self) -> Result<serde_json::Value> {
        let (total, long_term, short_term, deleted) = {
            let conn = self
                .inner
                .lock()
                .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;

            let total: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memory_items WHERE deleted_at IS NULL",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            let long_term: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memory_items WHERE scope = 'long_term' AND deleted_at IS NULL",
                [], |row| row.get(0),
            ).unwrap_or(0);

            let short_term: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memory_items WHERE scope = 'short_term' AND deleted_at IS NULL",
                [], |row| row.get(0),
            ).unwrap_or(0);

            let deleted: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memory_items WHERE deleted_at IS NOT NULL",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);

            (total, long_term, short_term, deleted)
        };

        let (pending_total, pending_upserts, pending_deletes) = self.pending_vector_counts()?;
        let (vector_enabled, vector_healthy, vector_backend) = if let Some(runtime) = &self.vector {
            let healthy = runtime.index.health().is_ok();
            let backend = runtime
                .index
                .stats()
                .unwrap_or_else(|error| serde_json::json!({ "error": error.to_string() }));
            (true, serde_json::Value::Bool(healthy), backend)
        } else {
            (false, serde_json::Value::Null, serde_json::Value::Null)
        };

        Ok(serde_json::json!({
            "total_active": total,
            "long_term": long_term,
            "short_term": short_term,
            "deleted_in_recycle_bin": deleted,
            "vector": {
                "enabled": vector_enabled,
                "healthy": vector_healthy,
                "pending_operations": pending_total,
                "pending_upserts": pending_upserts,
                "pending_deletes": pending_deletes,
                "backend": vector_backend,
            }
        }))
    }

    /// Memory statistics for a session.
    ///
    /// Counts the same visible set as [`MemoryStore::query`]: rows owned by the
    /// session **plus** global rows (`session_key IS NULL`). Counting only the
    /// exact session key made `stats` disagree with `query` and could make an
    /// LLM conclude "no memories" while global memories were still injected.
    pub fn stats_in_session(&self, session_key: &str) -> Result<serde_json::Value> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        let count = |predicate: &str| -> i64 {
            conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM memory_items
                     WHERE (session_key = ?1 OR session_key IS NULL) AND {}",
                    predicate
                ),
                params![session_key],
                |row| row.get(0),
            )
            .unwrap_or(0)
        };

        Ok(serde_json::json!({
            "total_active": count("deleted_at IS NULL"),
            "long_term": count("scope = 'long_term' AND deleted_at IS NULL"),
            "short_term": count("scope = 'short_term' AND deleted_at IS NULL"),
            "deleted_in_recycle_bin": count("deleted_at IS NOT NULL"),
            "vector": {
                "enabled": self.vector.is_some(),
                "healthy": self.vector.as_ref().map(|runtime| runtime.index.health().is_ok()),
            }
        }))
    }
}

/// Which brief section is being collected. Keeps the SQL ordering choice in one
/// place instead of duplicating the whole query per scope (P3-128).
#[derive(Clone, Copy)]
enum BriefScope {
    LongTerm,
    ShortTerm,
}

impl BriefScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::LongTerm => "long_term",
            Self::ShortTerm => "short_term",
        }
    }

    /// Ordering is a fixed internal literal, never user input.
    fn order_by(self) -> &'static str {
        match self {
            Self::LongTerm => "importance DESC, access_count DESC, updated_at DESC",
            Self::ShortTerm => "updated_at DESC, importance DESC",
        }
    }
}

/// Render `- [type] display` lines for one scope.
///
/// `display` prefers the explicit summary, then `title: first line`, then a
/// truncated body — identical for both scopes, which is why it lives here.
fn collect_brief_items(
    conn: &rusqlite::Connection,
    scope: BriefScope,
    session_key: Option<&str>,
    now: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let sql = format!(
        "SELECT title, summary, content, type FROM memory_items
         WHERE scope = ?4 AND deleted_at IS NULL
           AND (expires_at IS NULL OR expires_at > ?1)
           AND (?2 IS NULL OR session_key = ?2 OR session_key IS NULL)
         ORDER BY {}
         LIMIT ?3",
        scope.order_by()
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| blockcell_core::Error::Storage(format!("Brief query error: {}", e)))?;
    let rows = stmt
        .query_map(
            params![now, session_key, limit as i64, scope.as_str()],
            |row| {
                let title: Option<String> = row.get("title")?;
                let summary: Option<String> = row.get("summary")?;
                let content: String = row.get("content")?;
                let item_type: String = row.get("type")?;
                Ok((title, summary, content, item_type))
            },
        )
        .map_err(|e| blockcell_core::Error::Storage(format!("Brief query error: {}", e)))?;

    let mut items = Vec::new();
    for (title, summary, content, item_type) in rows.flatten() {
        let display = if let Some(s) = summary {
            s
        } else if let Some(t) = title {
            let first_line = content.lines().next().unwrap_or("");
            let truncated: String = first_line.chars().take(100).collect();
            if first_line.chars().count() > 100 {
                format!("{}: {}...", t, truncated)
            } else {
                format!("{}: {}", t, first_line)
            }
        } else {
            let truncated: String = content.chars().take(120).collect();
            if content.chars().count() > 120 {
                format!("{}...", truncated)
            } else {
                truncated
            }
        };
        items.push(format!("- [{}] {}", item_type, display));
    }
    Ok(items)
}
