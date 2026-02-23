use blockcell_storage::memory::{MemoryStore, QueryParams, UpsertParams};
use blockcell_tools::MemoryStoreOps;
use blockcell_core::Result;
use serde_json::Value;

/// Adapter that implements the tools crate's `MemoryStoreOps` trait
/// by delegating to the storage crate's `MemoryStore`.
pub struct MemoryStoreAdapter {
    store: MemoryStore,
}

impl MemoryStoreAdapter {
    pub fn new(store: MemoryStore) -> Self {
        Self { store }
    }

    /// Parse comma-separated tags from JSON value
    fn parse_tags(value: &Value, key: &str) -> Vec<String> {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
            .unwrap_or_default()
    }

    /// Get string value from JSON, converting to owned String
    fn get_string(value: &Value, key: &str) -> Option<String> {
        value.get(key).and_then(|v| v.as_str()).map(String::from)
    }

    /// Get string value with default
    fn get_string_or(value: &Value, key: &str, default: &str) -> String {
        Self::get_string(value, key).unwrap_or_else(|| default.to_string())
    }
}

impl MemoryStoreOps for MemoryStoreAdapter {
    fn upsert_json(&self, params_json: Value) -> Result<Value> {
        let params = UpsertParams {
            scope: Self::get_string_or(&params_json, "scope", "short_term"),
            item_type: Self::get_string_or(&params_json, "type", "note"),
            title: Self::get_string(&params_json, "title"),
            content: Self::get_string_or(&params_json, "content", ""),
            summary: Self::get_string(&params_json, "summary"),
            tags: Self::parse_tags(&params_json, "tags"),
            source: Self::get_string_or(&params_json, "source", "user"),
            channel: Self::get_string(&params_json, "channel"),
            session_key: Self::get_string(&params_json, "session_key"),
            importance: params_json.get("importance").and_then(|v| v.as_f64()).unwrap_or(0.5),
            dedup_key: Self::get_string(&params_json, "dedup_key"),
            expires_at: Self::get_string(&params_json, "expires_at"),
        };

        let item = self.store.upsert(params)?;
        serde_json::to_value(item).map_err(|e| blockcell_core::Error::Storage(format!("Failed to serialize memory item: {}", e)))
    }

    fn query_json(&self, params_json: Value) -> Result<Value> {
        let tags = Self::parse_tags(&params_json, "tags");
        let tags = if tags.is_empty() { None } else { Some(tags) };

        let params = QueryParams {
            query: Self::get_string(&params_json, "query"),
            scope: Self::get_string(&params_json, "scope"),
            item_type: Self::get_string(&params_json, "type"),
            tags,
            time_range_days: params_json.get("time_range_days").and_then(|v| v.as_i64()),
            top_k: params_json.get("top_k").and_then(|v| v.as_i64()).unwrap_or(20) as usize,
            include_deleted: params_json.get("include_deleted").and_then(|v| v.as_bool()).unwrap_or(false),
        };

        let results = self.store.query(&params)?;
        serde_json::to_value(results).map_err(|e| blockcell_core::Error::Storage(format!("Failed to serialize query results: {}", e)))
    }

    fn soft_delete(&self, id: &str) -> Result<bool> {
        self.store.soft_delete(id)
    }

    fn batch_soft_delete_json(&self, params_json: Value) -> Result<usize> {
        let scope = params_json.get("scope").and_then(|v| v.as_str());
        let item_type = params_json.get("type").and_then(|v| v.as_str());
        
        let tags = Self::parse_tags(&params_json, "tags");
        let tags_ref = if tags.is_empty() { None } else { Some(tags.as_slice()) };

        let time_before = params_json
            .get("before_days")
            .and_then(|v| v.as_i64())
            .map(|days| (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339());

        self.store.batch_soft_delete(scope, item_type, tags_ref, time_before.as_deref())
    }

    fn restore(&self, id: &str) -> Result<bool> {
        self.store.restore(id)
    }

    fn stats_json(&self) -> Result<Value> {
        self.store.stats()
    }

    fn generate_brief(&self, long_term_max: usize, short_term_max: usize) -> Result<String> {
        self.store.generate_brief(long_term_max, short_term_max)
    }

    fn generate_brief_for_query(&self, query: &str, max_items: usize) -> Result<String> {
        self.store.generate_brief_for_query(query, max_items)
    }

    fn upsert_session_summary(&self, session_key: &str, summary: &str) -> Result<()> {
        self.store.upsert_session_summary(session_key, summary)
    }

    fn get_session_summary(&self, session_key: &str) -> Result<Option<String>> {
        self.store.get_session_summary(session_key)
    }

    fn maintenance(&self, recycle_days: i64) -> Result<(usize, usize)> {
        self.store.maintenance(recycle_days)
    }
}
