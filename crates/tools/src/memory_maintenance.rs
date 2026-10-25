use async_trait::async_trait;
use blockcell_core::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use tracing::{debug, info};

use crate::memory::{looks_like_ghost_maintenance_log, run_store_blocking};
use crate::Tool;
use crate::ToolContext;
use crate::ToolSchema;

/// 从记忆存储中清理当前会话的 Ghost Agent 维护日志。
fn prune_ghost_maintenance_logs(
    store: &crate::MemoryStoreHandle,
    session_key: &str,
    dry_run: bool,
) -> Result<Value> {
    let query_params = json!({
        "scope": "short_term",
        "session_key": session_key,
        "time_range_days": 30,
        "top_k": 200,
        "query": "ghost OR \"Ghost Agent\" OR \"Memory Garden\" OR 例行维护 OR 维护任务 OR 记忆整理 OR 文件清理 OR 社区互动"
    });

    let results = store.query_json(query_params)?;

    let mut matched = 0usize;
    let mut deleted = 0usize;
    let mut matched_ids: Vec<String> = Vec::new();

    for r in results.as_array().unwrap_or(&vec![]) {
        let item = match r.get("item") {
            Some(i) => i,
            None => continue,
        };
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if looks_like_ghost_maintenance_log(title) || looks_like_ghost_maintenance_log(content) {
            matched += 1;
            matched_ids.push(id);
        }
    }

    matched_ids.sort();
    matched_ids.dedup();

    if !dry_run {
        for id in matched_ids {
            if store.soft_delete_in_session(&id, session_key)? {
                deleted += 1;
            }
        }
    }

    Ok(json!({
        "ghost_log_candidates_scanned": results.as_array().map(|a| a.len()).unwrap_or(0),
        "ghost_log_matched": matched,
        "ghost_log_deleted": if dry_run { 0 } else { deleted },
        "dry_run": dry_run,
    }))
}

fn looks_like_json_dump(text: &str) -> bool {
    let t = text.trim();
    if t.len() < 2 {
        return false;
    }
    // Heuristic: it starts like JSON and contains multiple key-value markers.
    let starts_like_json = t.starts_with('{') || t.starts_with('[');
    if !starts_like_json {
        return false;
    }
    let marker_count = t.matches("\":").take(6).count();
    marker_count >= 3
}

fn fingerprint_text(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        return "".to_string();
    }
    // Keep a short prefix as a cheap duplicate signal; safe for UTF-8.
    let mut end = 0usize;
    let mut chars = 0usize;
    for (idx, _) in t.char_indices() {
        if chars >= 200 {
            break;
        }
        end = idx;
        chars += 1;
    }
    let prefix = if chars >= 200 { &t[..end] } else { t };
    prefix.replace('\n', " ")
}

fn junk_score(item: &Value) -> f64 {
    // item is MemoryItem JSON.
    let source = item.get("source").and_then(|v| v.as_str()).unwrap_or("");
    let channel = item.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let importance = item
        .get("importance")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.5);
    let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");

    let mut score = 0.0;

    // Source/channel priors.
    if source == "tool" {
        score += 2.0;
    }
    if channel == "ghost" {
        score += 2.0;
    }
    if channel == "cron" {
        score += 1.0;
    }

    // Type priors: short-term notes are often ephemeral.
    if item_type == "note" {
        score += 1.0;
    }

    // Weak title signal.
    if title.trim().is_empty() {
        score += 0.5;
    }

    // Structure/content heuristics.
    let content_trim = content.trim();
    let content_len = content_trim.chars().count();
    if content_len <= 20 {
        score += 1.0;
    }
    if looks_like_json_dump(content_trim) {
        score += 1.5;
    }

    // Log-like density: many lines containing ':' or '='.
    let lines: Vec<&str> = content_trim.lines().take(80).collect();
    if lines.len() >= 8 {
        let kv_like = lines
            .iter()
            .filter(|l| l.contains(':') || l.contains('='))
            .count();
        if kv_like as f64 / (lines.len() as f64) >= 0.6 {
            score += 1.5;
        }
    }

    // Output/trace style markers (generic, not Ghost-specific).
    if content_trim.contains("test result")
        || content_trim.contains("running ")
        || content_trim.contains("finished in")
        || content_trim.contains("Exit code")
    {
        score += 2.0;
    }

    // Importance reduces junk probability.
    if importance >= 0.7 {
        score -= 2.0;
    } else if importance <= 0.3 {
        score += 1.0;
    }

    // User-authored content is less likely junk.
    if source == "user" {
        score -= 2.0;
    }

    score
}

fn sweep_junk_short_term(
    store: &crate::MemoryStoreHandle,
    session_key: &str,
    time_range_days: i64,
    dry_run: bool,
) -> Result<Value> {
    // Broad query: we prefer recall over precision; the score threshold controls deletion.
    let query_params = json!({
        "scope": "short_term",
        "session_key": session_key,
        "time_range_days": time_range_days,
        "top_k": 500,
    });

    let results = store.query_json(query_params)?;
    let arr = results.as_array().cloned().unwrap_or_default();

    let mut scanned = 0usize;
    let mut scored_as_junk = 0usize;
    let mut deleted = 0usize;

    // Duplicate compression: group by fingerprint, keep latest created_at.
    let mut groups: HashMap<String, Vec<(String, String)>> = HashMap::new();

    // Candidates to delete by scoring.
    let mut delete_ids: Vec<String> = Vec::new();

    for r in &arr {
        let item = match r.get("item") {
            Some(i) => i,
            None => continue,
        };
        scanned += 1;

        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let created_at = item
            .get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let fp = format!("{}|{}", fingerprint_text(title), fingerprint_text(content));
        groups.entry(fp).or_default().push((id.clone(), created_at));

        // Score-based deletion.
        let score = junk_score(item);
        // Threshold rationale: designed to avoid deleting user-written items unless they
        // look strongly like tool output/logs.
        if score >= 4.0 {
            scored_as_junk += 1;
            delete_ids.push(id);
        }
    }

    // Duplicate rule: for groups with many near-identical items, keep the latest.
    // This is a strong generic growth control even when the content isn't obviously junk.
    let mut duplicate_deleted = 0usize;
    for (_fp, mut ids) in groups {
        if ids.len() <= 3 {
            continue;
        }
        // Keep lexicographically max created_at (RFC3339 sorts correctly).
        ids.sort_by(|a, b| a.1.cmp(&b.1));
        let keep_id = ids.last().map(|x| x.0.clone());
        for (id, _) in ids {
            if Some(id.clone()) == keep_id {
                continue;
            }
            duplicate_deleted += 1;
            delete_ids.push(id);
        }
    }

    delete_ids.sort();
    delete_ids.dedup();

    if !dry_run {
        for id in &delete_ids {
            if store.soft_delete_in_session(id, session_key)? {
                deleted += 1;
            }
        }
    }

    Ok(json!({
        "scanned": scanned,
        "junk_by_score": scored_as_junk,
        "junk_by_duplicates": duplicate_deleted,
        "deleted": if dry_run { 0 } else { deleted },
        "dry_run": dry_run,
        "time_range_days": time_range_days,
    }))
}

/// MemoryMaintenanceTool — specialized tool for Ghost Agent memory gardening.
/// Provides higher-level memory operations: garden (compress/merge), cleanup, stats.
pub struct MemoryMaintenanceTool;

#[async_trait]
impl Tool for MemoryMaintenanceTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "memory_maintenance".to_string(),
            description: "Memory maintenance operations for Ghost Agent. Actions: garden (clean short-term memory and promote durable facts to canonical files), cleanup (remove expired/trivial entries), stats (memory health report), compact (inspect legacy long-term duplicates).".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["garden", "cleanup", "stats", "compact"],
                        "description": "garden: scan recent short-term memories, clean junk, and maintain durable facts with memory_manage/memory_forget. cleanup: remove expired entries and purge recycle bin. stats: get memory health report. compact: inspect legacy duplicates before canonical migration."
                    },
                    "days": {
                        "type": "integer",
                        "description": "Number of days to look back (for garden action). Default: 1"
                    },
                    "dry_run": {
                        "type": "boolean",
                        "description": "If true, report what would be done without making changes. Default: false"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "garden" | "cleanup" | "stats" | "compact" => Ok(()),
            _ => Err(blockcell_core::Error::Tool(format!(
                "Unknown action: {}",
                action
            ))),
        }
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let dry_run = params
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let store = ctx
            .memory_store
            .as_ref()
            .ok_or_else(|| blockcell_core::Error::Tool("Memory store not available".into()))?
            .clone();

        let session_key = ctx.session_key.clone();

        match action {
            "garden" => {
                let days = params.get("days").and_then(|v| v.as_i64()).unwrap_or(1);
                info!(
                    days = days,
                    dry_run = dry_run,
                    "Memory garden: scanning recent memories"
                );

                run_store_blocking(move || {
                    let prune_report =
                        prune_ghost_maintenance_logs(&store, &session_key, dry_run)?;
                    let junk_report = sweep_junk_short_term(&store, &session_key, 30, dry_run)?;

                    // Query recent short-term memories for batch long-term maintenance.
                    let query_params = json!({
                        "scope": "short_term",
                        "session_key": session_key,
                        "time_range_days": days,
                        "top_k": 100
                    });
                    let recent = store.query_json(query_params)?;
                    let items = recent.as_array().map(|a| a.len()).unwrap_or(0);

                    // Query legacy long-term rows so the model can avoid promoting duplicates.
                    let lt_params = json!({
                        "scope": "long_term",
                        "top_k": 50
                    });
                    let long_term = store.query_json(lt_params)?;
                    let lt_count = long_term.as_array().map(|a| a.len()).unwrap_or(0);

                    // Get stats
                    let stats = store.stats_json()?;

                    if dry_run {
                        return Ok(json!({
                            "action": "garden",
                            "dry_run": true,
                            "prune": prune_report,
                            "junk_sweep": junk_report,
                            "recent_short_term_count": items,
                            "existing_long_term_count": lt_count,
                            "stats": stats,
                            "note": "Would scan recent short-term memory, maintain long-term memory, and remove trivial entries."
                        }));
                    }

                    // Run maintenance (TTL cleanup + recycle bin purge)
                    let (expired, purged) = store.maintenance(30)?;

                    debug!(
                        recent = items,
                        expired = expired,
                        purged = purged,
                        "Memory garden complete"
                    );

                    Ok(json!({
                        "action": "garden",
                        "prune": prune_report,
                        "junk_sweep": junk_report,
                        "recent_memories_scanned": items,
                        "existing_long_term": lt_count,
                        "expired_cleaned": expired,
                        "recycle_purged": purged,
                        "recent_memories": recent,
                        "long_term_memories": long_term,
                        "instruction": "Review recent_memories and long_term_memories. Promote only stable user preferences, project facts, recurring patterns, and durable lessons by calling memory_manage with scope='user' or scope='workspace' so they are written to canonical files. Delete trivial, expired, or duplicate short-term entries with memory_forget. Do not create skills here; skills are handled by Ghost Learning."
                    }))
                })
                .await
            }

            "cleanup" => {
                info!(
                    dry_run = dry_run,
                    "Memory cleanup: removing expired entries"
                );

                run_store_blocking(move || {
                    let prune_report =
                        prune_ghost_maintenance_logs(&store, &session_key, dry_run)?;
                    let junk_report = sweep_junk_short_term(&store, &session_key, 30, dry_run)?;

                    if dry_run {
                        let stats = store.stats_json()?;
                        return Ok(json!({
                            "action": "cleanup",
                            "dry_run": true,
                            "prune": prune_report,
                            "junk_sweep": junk_report,
                            "stats": stats,
                            "note": "Would run TTL cleanup and purge recycle bin entries older than 30 days."
                        }));
                    }

                    let (expired, purged) = store.maintenance(30)?;

                    Ok(json!({
                        "action": "cleanup",
                        "prune": prune_report,
                        "junk_sweep": junk_report,
                        "expired_cleaned": expired,
                        "recycle_purged": purged,
                        "status": "ok"
                    }))
                })
                .await
            }

            "stats" => {
                run_store_blocking(move || {
                    let stats = store.stats_json()?;
                    let brief = store.generate_brief(10, 5)?;

                    Ok(json!({
                        "action": "stats",
                        "stats": stats,
                        "brief_preview": brief,
                        "health": "ok"
                    }))
                })
                .await
            }

            "compact" => {
                info!(dry_run = dry_run, "Memory compact: merging duplicates");

                run_store_blocking(move || {
                    // Query all long-term memories to find duplicates
                    let lt_params = json!({
                        "scope": "long_term",
                        "top_k": 200
                    });
                    let long_term = store.query_json(lt_params)?;
                    let items = long_term.as_array().map(|a| a.len()).unwrap_or(0);

                    if dry_run {
                        return Ok(json!({
                            "action": "compact",
                            "dry_run": true,
                            "long_term_count": items,
                            "note": "Would scan for duplicate/similar long-term memories and merge them."
                        }));
                    }

                    // The actual merging is best done by the LLM (Ghost Agent) which can
                    // understand semantic similarity. We return the data for it to process.
                    Ok(json!({
                        "action": "compact",
                        "long_term_memories": long_term,
                        "count": items,
                        "instruction": "These are legacy SQLite long-term rows. Run `blockcell memory migrate-canonical` to move them into canonical files, then use memory_manage replace/remove for semantic consolidation. Do not create new long-term SQLite rows."
                    }))
                })
                .await
            }

            _ => Err(blockcell_core::Error::Tool(format!(
                "Unknown action: {}",
                action
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::Config;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    struct CaptureMemoryStore {
        query_calls: Mutex<Vec<Value>>,
    }

    impl CaptureMemoryStore {
        fn new() -> Self {
            Self {
                query_calls: Mutex::new(Vec::new()),
            }
        }

        fn query_calls(&self) -> Vec<Value> {
            self.query_calls.lock().expect("query_calls lock").clone()
        }
    }

    impl crate::MemoryStoreOps for CaptureMemoryStore {
        fn upsert_json(&self, _params_json: Value) -> Result<Value> {
            Ok(json!({}))
        }

        fn query_json(&self, params_json: Value) -> Result<Value> {
            self.query_calls
                .lock()
                .expect("query_calls lock")
                .push(params_json.clone());
            Ok(json!([]))
        }

        fn soft_delete(&self, _id: &str) -> Result<bool> {
            Ok(false)
        }

        fn batch_soft_delete_json(&self, _params_json: Value) -> Result<usize> {
            Ok(0)
        }

        fn restore(&self, _id: &str) -> Result<bool> {
            Ok(false)
        }

        fn stats_json(&self) -> Result<Value> {
            Ok(json!({
                "long_term": 0,
                "short_term": 0
            }))
        }

        fn generate_brief(&self, _long_term_max: usize, _short_term_max: usize) -> Result<String> {
            Ok(String::new())
        }

        fn generate_brief_for_query(&self, _query: &str, _max_items: usize) -> Result<String> {
            Ok(String::new())
        }

        fn upsert_session_summary(&self, _session_key: &str, _summary: &str) -> Result<()> {
            Ok(())
        }

        fn get_session_summary(&self, _session_key: &str) -> Result<Option<String>> {
            Ok(None)
        }

        fn maintenance(&self, _recycle_days: i64) -> Result<(usize, usize)> {
            Ok((0, 0))
        }
    }

    fn test_context(memory_store: Arc<dyn crate::MemoryStoreOps + Send + Sync>) -> ToolContext {
        ToolContext {
            workspace: PathBuf::from("/tmp/workspace"),
            sandbox_writable_roots: None,
            base: PathBuf::from("/tmp/blockcell"),
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "cli:test".to_string(),
            channel: "ghost".to_string(),
            account_id: None,
            sender_id: None,
            sender_role: None,
            chat_id: "chat-1".to_string(),
            config: Config::default(),
            permissions: blockcell_core::types::PermissionSet::new(),
            task_manager: None,
            memory_store: Some(memory_store),
            memory_file_store: None,
            ghost_memory_lifecycle: None,
            skill_file_store: None,
            session_search: None,
            outbound_tx: None,
            spawn_handle: None,
            capability_registry: None,
            core_evolution: None,
            event_emitter: None,
            channel_contacts_file: None,
            response_cache: None,
            runtime_handle: None,
            agent_identity: None,
            skill_mutex: None,
            agent_type_registry: None,
            evolution_workflow_store: None,
        }
    }

    #[test]
    fn test_schema() {
        let tool = MemoryMaintenanceTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "memory_maintenance");
    }

    #[test]
    fn test_validate() {
        let tool = MemoryMaintenanceTool;
        assert!(tool.validate(&json!({"action": "garden"})).is_ok());
        assert!(tool.validate(&json!({"action": "cleanup"})).is_ok());
        assert!(tool.validate(&json!({"action": "stats"})).is_ok());
        assert!(tool.validate(&json!({"action": "compact"})).is_ok());
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }

    #[test]
    fn test_junk_score_does_not_special_case_removed_summary_type() {
        let note_item = json!({
            "source": "tool",
            "channel": "ghost",
            "type": "note",
            "importance": 0.5,
            "title": "",
            "content": "short"
        });
        let legacy_summary_item = json!({
            "source": "tool",
            "channel": "ghost",
            "type": "summary",
            "importance": 0.5,
            "title": "",
            "content": "short"
        });

        assert!(junk_score(&note_item) > junk_score(&legacy_summary_item));
    }

    #[tokio::test]
    async fn test_memory_maintenance_garden_uses_time_range_days() {
        let store = Arc::new(CaptureMemoryStore::new());
        let tool = MemoryMaintenanceTool;

        tool.execute(
            test_context(store.clone()),
            json!({
                "action": "garden",
                "days": 7,
                "dry_run": true
            }),
        )
        .await
        .expect("garden should succeed");

        let calls = store.query_calls();
        let recent_query = calls
            .iter()
            .find(|params| {
                params.get("scope").and_then(|v| v.as_str()) == Some("short_term")
                    && params.get("top_k").and_then(|v| v.as_i64()) == Some(100)
            })
            .expect("recent short-term query");

        assert_eq!(
            recent_query.get("time_range_days").and_then(|v| v.as_i64()),
            Some(7)
        );
        assert_eq!(
            recent_query.get("session_key").and_then(|v| v.as_str()),
            Some("cli:test")
        );
        assert!(
            calls
                .iter()
                .filter(|params| params.get("scope").and_then(|v| v.as_str()) == Some("short_term"))
                .all(
                    |params| params.get("session_key").and_then(|v| v.as_str()) == Some("cli:test")
                ),
            "all short-term prune/sweep/garden queries must be scoped to the caller session_key"
        );
        assert!(recent_query.get("days").is_none());
    }

    #[tokio::test]
    async fn test_memory_maintenance_garden_routes_durable_writes_to_canonical_files() {
        let store = Arc::new(CaptureMemoryStore::new());
        let tool = MemoryMaintenanceTool;

        let result = tool
            .execute(
                test_context(store),
                json!({
                    "action": "garden",
                    "days": 7,
                    "dry_run": false
                }),
            )
            .await
            .expect("garden should succeed");

        assert!(result.get("recent_memories").is_some());
        assert!(result.get("long_term_memories").is_some());
        assert_eq!(result.get("existing_long_term"), Some(&json!(0)));
        let rendered = serde_json::to_string(&result).expect("render garden result");
        assert!(rendered.contains("memory_manage"));
        assert!(rendered.contains("canonical"));
        assert!(!rendered.contains(&format!("memory_{}", "upsert")));
        assert!(rendered.contains("memory_forget"));
    }
}
