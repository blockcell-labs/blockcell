use super::*;

impl MemoryStore {
    /// Import from existing MEMORY.md file.
    pub fn import_long_term_md(&self, content: &str) -> Result<usize> {
        let sections = parse_markdown_sections(content);
        let mut count = 0;

        for (heading, body) in &sections {
            let body = body.trim();
            if body.is_empty() || body.starts_with('(') {
                continue; // Skip placeholder sections
            }

            // Each non-empty section becomes a long-term memory item
            let dedup_key = format!(
                "import.long_term.{}",
                heading.to_lowercase().replace(' ', "_")
            );
            let _ = self.upsert(UpsertParams {
                scope: "long_term".to_string(),
                item_type: classify_section(heading),
                title: Some(heading.clone()),
                content: body.to_string(),
                summary: None,
                tags: vec!["imported".to_string()],
                source: "import".to_string(),
                channel: None,
                session_key: None,
                importance: 0.7,
                dedup_key: Some(dedup_key),
                expires_at: None,
            })?;
            count += 1;
        }

        info!(count, "Imported long-term memory items from MEMORY.md");
        Ok(count)
    }

    /// Import from a daily note file.
    pub fn import_daily_md(&self, date: &str, content: &str) -> Result<usize> {
        let content = content.trim();
        if content.is_empty() {
            return Ok(0);
        }

        // Parse into sections or treat as one item
        let sections = parse_markdown_sections(content);
        let mut count = 0;

        if sections.is_empty() {
            // No sections, import as single note
            let dedup_key = format!("import.daily.{}", date);
            let expires_at = compute_daily_expiry(date, 30);
            let _ = self.upsert(UpsertParams {
                scope: "short_term".to_string(),
                item_type: "note".to_string(),
                title: Some(format!("Daily notes {}", date)),
                content: content.to_string(),
                summary: None,
                tags: vec!["daily".to_string(), "imported".to_string()],
                source: "import".to_string(),
                channel: None,
                session_key: None,
                importance: 0.4,
                dedup_key: Some(dedup_key),
                expires_at,
            })?;
            count += 1;
        } else {
            for (heading, body) in &sections {
                let body = body.trim();
                if body.is_empty() {
                    continue;
                }
                let dedup_key = format!(
                    "import.daily.{}.{}",
                    date,
                    heading.to_lowercase().replace(' ', "_")
                );
                let expires_at = compute_daily_expiry(date, 30);
                let _ = self.upsert(UpsertParams {
                    scope: "short_term".to_string(),
                    item_type: classify_section(heading),
                    title: Some(format!("{} ({})", heading, date)),
                    content: body.to_string(),
                    summary: None,
                    tags: vec!["daily".to_string(), "imported".to_string()],
                    source: "import".to_string(),
                    channel: None,
                    session_key: None,
                    importance: 0.4,
                    dedup_key: Some(dedup_key),
                    expires_at,
                })?;
                count += 1;
            }
        }

        info!(date, count, "Imported daily memory items");
        Ok(count)
    }

    /// Check if migration has already been done.
    ///
    /// Returns an error for real database failures (e.g. poisoned lock) so the
    /// caller can distinguish "not migrated" from "could not check" instead of
    /// silently re-running the migration.
    pub fn is_migrated(&self) -> Result<bool> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        match conn.query_row(
            "SELECT value FROM memory_meta WHERE key = 'migrated_from_md'",
            [],
            |row| row.get::<_, String>(0),
        ) {
            Ok(_) => Ok(true),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(error) => Err(blockcell_core::Error::Storage(format!(
                "Failed to read migration marker: {}",
                error
            ))),
        }
    }

    /// Mark migration as done.
    pub fn mark_migrated(&self) -> Result<()> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| blockcell_core::Error::Storage(format!("Lock error: {}", e)))?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO memory_meta (key, value) VALUES ('migrated_from_md', ?1)",
            params![now],
        )
        .map_err(|e| blockcell_core::Error::Storage(format!("Mark migrated error: {}", e)))?;
        Ok(())
    }

    /// Run the full migration from MEMORY.md and daily files.
    pub fn migrate_from_files(&self, memory_dir: &Path) -> Result<usize> {
        if self.is_migrated()? {
            debug!("Memory migration already done, skipping");
            return Ok(0);
        }

        let mut total = 0;

        // Import MEMORY.md
        let memory_md = memory_dir.join("MEMORY.md");
        if memory_md.exists() {
            let content = std::fs::read_to_string(&memory_md)?;
            total += self.import_long_term_md(&content)?;
        }

        // Import daily notes
        for entry in std::fs::read_dir(memory_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            // Match YYYY-MM-DD.md pattern
            if name.len() == 13 && name.ends_with(".md") && name != "MEMORY.md" {
                let date = &name[..10];
                let content = std::fs::read_to_string(entry.path())?;
                total += self.import_daily_md(date, &content)?;
            }
        }

        self.mark_migrated()?;
        info!(total, "Memory migration from files completed");
        Ok(total)
    }
}
