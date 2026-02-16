use async_trait::async_trait;
use blockcell_core::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use tracing::{debug, info};

use crate::{Tool, ToolContext, ToolSchema};

fn looks_like_ghost_maintenance_log(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("ghost agent")
        || t.contains("memory garden")
        || t.contains("例行维护")
        || t.contains("维护任务")
        || t.contains("记忆整理")
        || t.contains("文件清理")
        || t.contains("社区互动")
        || t.contains("heart")
        || t.contains("heartbeat")
        || t.contains("feed")
}

fn extract_result_ids(results: &Value) -> Vec<String> {
    results
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    r.get("item")
                        .and_then(|i| i.get("id"))
                        .and_then(|id| id.as_str())
                        .map(|s| s.to_string())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn prune_ghost_maintenance_logs(
    store: &crate::MemoryStoreHandle,
    dry_run: bool,
) -> Result<Value> {
    let query_params = json!({
        "scope": "short_term",
        "time_range_days": 30,
        "top_k": 200,
        "query": "ghost OR \"Ghost Agent\" OR \"Memory Garden\" OR 例行维护 OR 维护任务 OR 记忆整理 OR 文件清理 OR 社区互动"
    });

    let results = store.query_json(query_params)?;
    let mut ids = extract_result_ids(&results);
    ids.sort();
    ids.dedup();

    let mut matched = 0usize;
    let mut deleted = 0usize;

    for r in results.as_array().unwrap_or(&vec![]) {
        let item = match r.get("item") {
            Some(i) => i,
            None => continue,
        };
        let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if looks_like_ghost_maintenance_log(title) || looks_like_ghost_maintenance_log(content) {
            matched += 1;
        }
    }

    if !dry_run {
        for id in ids {
            if store.soft_delete(&id)? {
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
    let prefix = if chars >= 200 {
        &t[..end]
    } else {
        t
    };
    prefix.replace('\n', " ")
}

fn junk_score(item: &Value) -> f64 {
    // item is MemoryItem JSON.
    let source = item.get("source").and_then(|v| v.as_str()).unwrap_or("");
    let channel = item.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let importance = item.get("importance").and_then(|v| v.as_f64()).unwrap_or(0.5);
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

    // Type priors: short-term notes/summaries are often ephemeral.
    if item_type == "note" {
        score += 1.0;
    }
    if item_type == "summary" {
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
    time_range_days: i64,
    dry_run: bool,
) -> Result<Value> {
    // Broad query: we prefer recall over precision; the score threshold controls deletion.
    let query_params = json!({
        "scope": "short_term",
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

        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
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
            if store.soft_delete(id)? {
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
            name: "memory_maintenance",
            description: "Memory maintenance operations for Ghost Agent. Actions: garden (compress daily memories into long-term facts), cleanup (remove expired/trivial entries), stats (memory health report), compact (merge duplicate entries).",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["garden", "cleanup", "stats", "compact"],
                        "description": "garden: scan recent daily memories and extract long-term facts. cleanup: remove expired entries and purge recycle bin. stats: get memory health report. compact: merge duplicate entries."
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
            _ => Err(blockcell_core::Error::Tool(format!("Unknown action: {}", action))),
        }
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let dry_run = params.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false);

        let store = ctx.memory_store.as_ref().ok_or_else(|| {
            blockcell_core::Error::Tool("Memory store not available".into())
        })?;

        match action {
            "garden" => {
                let days = params.get("days").and_then(|v| v.as_i64()).unwrap_or(1);
                info!(days = days, dry_run = dry_run, "Memory garden: scanning recent memories");

                let prune_report = prune_ghost_maintenance_logs(store, dry_run)?;
                let junk_report = sweep_junk_short_term(store, 30, dry_run)?;

                // Query recent short-term memories
                let query_params = json!({
                    "scope": "short_term",
                    "days": days,
                    "top_k": 100
                });
                let recent = store.query_json(query_params)?;
                let items = recent.as_array().map(|a| a.len()).unwrap_or(0);

                // Query existing long-term memories for dedup check
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
                        "note": "Would scan and extract important facts from short-term to long-term memory."
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
                    "instruction": "Review the recent_memories above. For each important fact (user preferences, project details, recurring patterns), call memory_upsert with scope='long_term'. For trivial entries (weather queries, simple greetings), call memory_forget to delete them."
                }))
            }

            "cleanup" => {
                info!(dry_run = dry_run, "Memory cleanup: removing expired entries");

                let prune_report = prune_ghost_maintenance_logs(store, dry_run)?;
                let junk_report = sweep_junk_short_term(store, 30, dry_run)?;

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
            }

            "stats" => {
                let stats = store.stats_json()?;
                let brief = store.generate_brief(10, 5)?;

                Ok(json!({
                    "action": "stats",
                    "stats": stats,
                    "brief_preview": brief,
                    "health": "ok"
                }))
            }

            "compact" => {
                info!(dry_run = dry_run, "Memory compact: merging duplicates");

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
                    "instruction": "Review the long_term_memories above. Find entries with similar or overlapping content. For duplicates, keep the most complete version and call memory_forget on the others. For related entries, merge them into a single comprehensive entry using memory_upsert with the same dedup_key."
                }))
            }

            _ => Err(blockcell_core::Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
