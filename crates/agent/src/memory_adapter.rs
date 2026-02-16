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
}

impl MemoryStoreOps for MemoryStoreAdapter {
    fn upsert_json(&self, params_json: Value) -> Result<Value> {
        let tags_str = params_json.get("tags")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let tags: Vec<String> = if tags_str.is_empty() {
            vec![]
        } else {
            tags_str.split(',').map(|s| s.trim().to_string()).collect()
        };

        let params = UpsertParams {
            scope: params_json.get("scope").and_then(|v| v.as_str()).unwrap_or("short_term").to_string(),
            item_type: params_json.get("type").and_then(|v| v.as_str()).unwrap_or("note").to_string(),
            title: params_json.get("title").and_then(|v| v.as_str()).map(|s| s.to_string()),
            content: params_json.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            summary: params_json.get("summary").and_then(|v| v.as_str()).map(|s| s.to_string()),
            tags,
            source: params_json.get("source").and_then(|v| v.as_str()).unwrap_or("user").to_string(),
            channel: params_json.get("channel").and_then(|v| v.as_str()).map(|s| s.to_string()),
            session_key: params_json.get("session_key").and_then(|v| v.as_str()).map(|s| s.to_string()),
            importance: params_json.get("importance").and_then(|v| v.as_f64()).unwrap_or(0.5),
            dedup_key: params_json.get("dedup_key").and_then(|v| v.as_str()).map(|s| s.to_string()),
            expires_at: params_json.get("expires_at").and_then(|v| v.as_str()).map(|s| s.to_string()),
        };

        let item = self.store.upsert(params)?;
        Ok(serde_json::to_value(item).unwrap_or_default())
    }

    fn query_json(&self, params_json: Value) -> Result<Value> {
        let tags_str = params_json.get("tags").and_then(|v| v.as_str()).unwrap_or("");
        let tags: Option<Vec<String>> = if tags_str.is_empty() {
            None
        } else {
            Some(tags_str.split(',').map(|s| s.trim().to_string()).collect())
        };

        let params = QueryParams {
            query: params_json.get("query").and_then(|v| v.as_str()).map(|s| s.to_string()),
            scope: params_json.get("scope").and_then(|v| v.as_str()).map(|s| s.to_string()),
            item_type: params_json.get("type").and_then(|v| v.as_str()).map(|s| s.to_string()),
            tags,
            time_range_days: params_json.get("time_range_days").and_then(|v| v.as_i64()),
            top_k: params_json.get("top_k").and_then(|v| v.as_i64()).unwrap_or(20) as usize,
            include_deleted: params_json.get("include_deleted").and_then(|v| v.as_bool()).unwrap_or(false),
        };

        let results = self.store.query(&params)?;
        Ok(serde_json::to_value(results).unwrap_or_default())
    }

    fn soft_delete(&self, id: &str) -> Result<bool> {
        self.store.soft_delete(id)
    }

    fn batch_soft_delete_json(&self, params_json: Value) -> Result<usize> {
        let scope = params_json.get("scope").and_then(|v| v.as_str());
        let item_type = params_json.get("type").and_then(|v| v.as_str());
        let tags_str = params_json.get("tags").and_then(|v| v.as_str()).unwrap_or("");
        let tags: Vec<String> = if tags_str.is_empty() {
            vec![]
        } else {
            tags_str.split(',').map(|s| s.trim().to_string()).collect()
        };
        let tags_ref = if tags.is_empty() { None } else { Some(tags.as_slice()) };

        let before_days = params_json.get("before_days").and_then(|v| v.as_i64());
        let time_before = before_days.map(|days| {
            (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339()
        });

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

    fn maintenance(&self, recycle_days: i64) -> Result<(usize, usize)> {
        self.store.maintenance(recycle_days)
    }
}
