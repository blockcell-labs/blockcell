use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for querying memory using FTS5 full-text search + structured filters.
pub struct MemoryQueryTool;

/// Tool for upserting memory items with type/scope/tags/dedup support.
pub struct MemoryUpsertTool;

/// Tool for deleting/forgetting memory items (soft-delete, batch delete, restore).
pub struct MemoryForgetTool;

/// File memory tool for USER.md / MEMORY.md.
pub struct MemoryManageTool;

fn get_memory_store(ctx: &ToolContext) -> Result<&crate::MemoryStoreHandle> {
    ctx.memory_store
        .as_ref()
        .ok_or_else(|| Error::Tool("Memory store not available".to_string()))
}

/// Run a synchronous (SQLite-backed) memory-store operation on a blocking
/// thread so it never stalls the async runtime (see O3).
pub(crate) async fn run_store_blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Tool(format!("memory store task panicked: {}", e)))?
}

/// 判断文本是否像 Ghost Agent 维护日志
///
/// 用于阻止 Ghost Agent 将自己的维护日志写入记忆存储。
/// 使用精确匹配模式避免误判用户正常内容（如健康话题、数据 feed 等）。
fn get_memory_file_store(ctx: &ToolContext) -> Result<&crate::MemoryFileStoreHandle> {
    ctx.memory_file_store
        .as_ref()
        .ok_or_else(|| Error::Tool("Memory file store not available".to_string()))
}

/// 判断文本是否像 Ghost Agent 维护日志
///
/// 用于阻止 Ghost Agent 将自己的维护日志写入记忆存储。
/// 使用精确匹配模式避免误判用户正常内容（如健康话题、数据 feed 等）。
/// 此函数为 pub(crate) 供 memory_maintenance 模块复用。
pub(crate) fn looks_like_ghost_maintenance_log(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("ghost agent")
        || t.contains("memory garden")
        || t.contains("例行维护")
        || t.contains("维护任务")
        || t.contains("记忆整理")
        || t.contains("文件清理")
        || t.contains("社区互动")
        || t.contains("heartbeat check")  // 精确匹配，避免误判健康话题
        || t.contains("heartbeat report")
        || t.contains("feed cycle")       // 精确匹配，避免误判数据 feed
        || t.contains("feed routine")
}

#[async_trait]
impl Tool for MemoryManageTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "memory_manage".to_string(),
            description: "Save durable learning directly to USER.md or MEMORY.md. Use for stable user preferences, durable constraints, project facts, environment facts, and reusable non-procedural lessons. Do not save task logs or temporary state.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["add", "replace", "remove", "undo_latest"],
                        "description": "Memory operation. Use replace/remove with old_text to update existing entries."
                    },
                    "target": {
                        "type": "string",
                        "enum": ["user", "memory"],
                        "description": "user = durable user profile/preferences. memory = project/environment facts and stable lessons."
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["session", "workspace", "user"],
                        "default": "session",
                        "description": "Write visibility. session is current-session only (default); workspace is durable project knowledge and requires target=memory; user is durable cross-session preference/profile and requires target=user."
                    },
                    "content": {
                        "type": "string",
                        "description": "Entry content for add/replace. Keep compact and durable."
                    },
                    "old_text": {
                        "type": "string",
                        "description": "Short unique text identifying the entry to replace/remove."
                    }
                },
                "required": ["action", "target"]
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- Use `memory_manage` proactively for durable user preferences, corrections, stable project facts, and environment lessons. Do not save task progress or temporary TODOs.".to_string())
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params
            .get("action")
            .and_then(|value| value.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: action".to_string()))?;
        let target = params
            .get("target")
            .and_then(|value| value.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: target".to_string()))?;
        if !matches!(target, "user" | "memory") {
            return Err(Error::Validation(
                "target must be 'user' or 'memory'".to_string(),
            ));
        }
        let scope = params
            .get("scope")
            .and_then(|value| value.as_str())
            .unwrap_or("session");
        if !matches!(scope, "session" | "workspace" | "user") {
            return Err(Error::Validation(
                "scope must be 'session', 'workspace', or 'user'".to_string(),
            ));
        }
        if scope == "workspace" && target != "memory" {
            return Err(Error::Validation(
                "scope 'workspace' requires target 'memory'".to_string(),
            ));
        }
        if scope == "user" && target != "user" {
            return Err(Error::Validation(
                "scope 'user' requires target 'user'".to_string(),
            ));
        }
        match action {
            "add" => {
                if params
                    .get("content")
                    .and_then(|value| value.as_str())
                    .is_none()
                {
                    return Err(Error::Validation("content is required for add".to_string()));
                }
            }
            "replace" => {
                if params
                    .get("content")
                    .and_then(|value| value.as_str())
                    .is_none()
                {
                    return Err(Error::Validation(
                        "content is required for replace".to_string(),
                    ));
                }
                if params
                    .get("old_text")
                    .and_then(|value| value.as_str())
                    .is_none()
                {
                    return Err(Error::Validation(
                        "old_text is required for replace".to_string(),
                    ));
                }
            }
            "remove" => {
                if params
                    .get("old_text")
                    .and_then(|value| value.as_str())
                    .is_none()
                {
                    return Err(Error::Validation(
                        "old_text is required for remove".to_string(),
                    ));
                }
            }
            "undo_latest" => {}
            _ => {
                return Err(Error::Validation(
                    "action must be add, replace, remove, or undo_latest".to_string(),
                ));
            }
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let store = get_memory_file_store(&ctx)?.clone();
        let action = params
            .get("action")
            .and_then(|value| value.as_str())
            .ok_or_else(|| Error::Tool("Missing or non-string parameter: action".to_string()))?
            .to_string();
        let target = params
            .get("target")
            .and_then(|value| value.as_str())
            .ok_or_else(|| Error::Tool("Missing or non-string parameter: target".to_string()))?
            .to_string();
        let scope = params
            .get("scope")
            .and_then(|value| value.as_str())
            .unwrap_or("session")
            .to_string();
        let content = params
            .get("content")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let old_text = params
            .get("old_text")
            .and_then(|value| value.as_str())
            .map(str::to_string);

        // File-backed memory writes snapshot, atomically rewrite and fsync the
        // canonical files, so run them on a blocking thread like the other
        // memory tools instead of stalling a tokio worker.
        let mut result = {
            let store = store.clone();
            let action = action.clone();
            let target = target.clone();
            let scope = scope.clone();
            let content = content.clone();
            let old_text = old_text.clone();
            run_store_blocking(move || match action.as_str() {
                "add" => store.add_scoped_file_memory_json(
                    &scope,
                    &target,
                    content.as_deref().ok_or_else(|| {
                        Error::Tool("Missing or non-string parameter: content".to_string())
                    })?,
                ),
                "replace" => store.replace_scoped_file_memory_json(
                    &scope,
                    &target,
                    old_text.as_deref().ok_or_else(|| {
                        Error::Tool("Missing or non-string parameter: old_text".to_string())
                    })?,
                    content.as_deref().ok_or_else(|| {
                        Error::Tool("Missing or non-string parameter: content".to_string())
                    })?,
                ),
                "remove" => store.remove_scoped_file_memory_json(
                    &scope,
                    &target,
                    old_text.as_deref().ok_or_else(|| {
                        Error::Tool("Missing or non-string parameter: old_text".to_string())
                    })?,
                ),
                "undo_latest" => store.restore_latest_scoped_file_memory_json(&scope, &target),
                _ => unreachable!("validated action"),
            })
            .await?
        };
        if let Some(result) = result.as_object_mut() {
            result.insert("scope".to_string(), Value::String(scope.clone()));
        }

        if result
            .get("success")
            .and_then(|success| success.as_bool())
            .unwrap_or(false)
        {
            if let Some(lifecycle) = ctx.ghost_memory_lifecycle.as_ref() {
                let content = content.as_deref().or(old_text.as_deref()).unwrap_or("");
                let _ = lifecycle.on_memory_write_json(&target, &action, content);
            }
        }
        Ok(result)
    }
}

#[async_trait]
impl Tool for MemoryQueryTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "memory_query".to_string(),
            description: "Search and retrieve memory items using full-text search with structured filters. Use this to recall facts, preferences, past decisions, project context, or any previously stored information. Supports filtering by scope (long_term/short_term), type, tags, and time range. Results are ranked by relevance, importance, and recency.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Full-text search query. Leave empty to browse by filters."
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["long_term", "short_term"],
                        "description": "Filter by memory scope. Omit to search all scopes."
                    },
                    "type": {
                        "type": "string",
                        "enum": ["fact", "preference", "project", "task", "glossary", "contact", "snippet", "policy", "note", "session_summary"],
                        "description": "Filter by memory type."
                    },
                    "tags": {
                        "type": "string",
                        "description": "Comma-separated tags to filter by (any match)."
                    },
                    "time_range_days": {
                        "type": "integer",
                        "description": "Only return items created within the last N days."
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 20, max: 50)."
                    },
                    "include_deleted": {
                        "type": "boolean",
                        "description": "Include soft-deleted items in results (default: false)."
                    },
                    "stats": {
                        "type": "boolean",
                        "description": "If true, return memory statistics instead of search results."
                    }
                },
                "required": []
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- Search `memory_query` before asking the user for information you might already know.".to_string())
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if let Some(top_k) = params.get("top_k") {
            let Some(top_k) = top_k.as_i64() else {
                return Err(Error::Validation("'top_k' must be an integer".to_string()));
            };
            if !(1..=50).contains(&top_k) {
                return Err(Error::Validation(
                    "'top_k' must be between 1 and 50".to_string(),
                ));
            }
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let store = get_memory_store(&ctx)?.clone();

        // Stats mode
        if params
            .get("stats")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let session_key = ctx.session_key.clone();
            return run_store_blocking(move || store.stats_in_session_json(&session_key)).await;
        }

        let query_params = json!({
            "query": params.get("query").and_then(|v| v.as_str()),
            "session_key": ctx.session_key,
            "scope": params.get("scope").and_then(|v| v.as_str()),
            "type": params.get("type").and_then(|v| v.as_str()),
            "tags": params.get("tags").and_then(|v| v.as_str()),
            "time_range_days": params.get("time_range_days").and_then(|v| v.as_i64()),
            "top_k": params.get("top_k").and_then(|v| v.as_i64()).unwrap_or(20).min(50),
            "include_deleted": params.get("include_deleted").and_then(|v| v.as_bool()).unwrap_or(false),
        });

        let results = run_store_blocking(move || store.query_json(query_params)).await?;

        // 当查询返回空数组时，添加明确提示避免 LLM 幻觉编造结果
        if results == json!([]) {
            debug!("memory_query returned empty results");
            return Ok(json!({
                "results": [],
                "total": 0,
                "note": "No matching memories found. The query returned zero results."
            }));
        }

        debug!("memory_query executed");
        Ok(results)
    }
}

#[async_trait]
impl Tool for MemoryUpsertTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "memory_upsert".to_string(),
            description: "Save or update short-term session memory with structured metadata. Durable knowledge must be written through memory_manage to the canonical USER.md or MEMORY.md files.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "The content to remember (markdown text)."
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["short_term"],
                        "description": "Memory scope. Only 'short_term' is accepted; durable knowledge belongs in canonical files via memory_manage."
                    },
                    "type": {
                        "type": "string",
                        "enum": ["fact", "preference", "project", "task", "glossary", "contact", "snippet", "policy", "note"],
                        "description": "Type classification. Default: 'note'."
                    },
                    "title": {
                        "type": "string",
                        "description": "Optional short title/label for the memory item."
                    },
                    "summary": {
                        "type": "string",
                        "description": "Optional 1-2 line summary (used in brief injection to save prompt tokens)."
                    },
                    "tags": {
                        "type": "string",
                        "description": "Comma-separated tags for categorization and filtering."
                    },
                    "importance": {
                        "type": "number",
                        "description": "Importance score 0.0-1.0. Higher = more likely to appear in brief. Default: 0.5."
                    },
                    "dedup_key": {
                        "type": "string",
                        "description": "Deduplication key. If an item with the same dedup_key exists, it will be updated instead of creating a duplicate. Use for preferences (e.g. 'pref.language'), facts (e.g. 'user.name'), etc."
                    },
                    "expires_in_days": {
                        "type": "integer",
                        "description": "Auto-expire after N days. Useful for short-term items. Omit for no expiry."
                    }
                },
                "required": ["content"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("content").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: content".to_string(),
            ));
        }
        // 验证 type 字段与 schema enum 一致
        if let Some(item_type) = params.get("type").and_then(|v| v.as_str()) {
            const VALID_TYPES: &[&str] = &[
                "fact",
                "preference",
                "project",
                "task",
                "glossary",
                "contact",
                "snippet",
                "policy",
                "note",
            ];
            if !VALID_TYPES.contains(&item_type) {
                return Err(Error::Validation(format!(
                    "Invalid memory type '{}'. Must be one of: {}",
                    item_type,
                    VALID_TYPES.join(", ")
                )));
            }
        }
        // 验证 scope 字段
        if let Some(scope) = params.get("scope").and_then(|v| v.as_str()) {
            if scope == "long_term" {
                return Err(Error::Validation(
                    "long_term memory must be written through memory_manage to canonical files"
                        .to_string(),
                ));
            }
            if scope != "short_term" {
                return Err(Error::Validation(format!(
                    "Invalid scope '{}'. Must be 'short_term'",
                    scope
                )));
            }
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let store = get_memory_store(&ctx)?.clone();

        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Tool("Missing or non-string parameter: content".to_string()))?;
        let scope = params
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("short_term");
        let item_type = params
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("note");
        let title = params.get("title").and_then(|v| v.as_str());
        let summary = params.get("summary").and_then(|v| v.as_str());
        let tags_str = params.get("tags").and_then(|v| v.as_str()).unwrap_or("");
        let importance = params
            .get("importance")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5);
        let dedup_key = params.get("dedup_key").and_then(|v| v.as_str());
        let expires_in_days = params.get("expires_in_days").and_then(|v| v.as_i64());

        // Guardrail: Ghost maintenance routine must not write its own logs into memory.
        // We allow Ghost to upsert only meaningful long-term items (facts/preferences/projects/tasks).
        if ctx.channel == "ghost" {
            if scope != "long_term" {
                return Err(Error::Validation(
                    "Ghost channel is not allowed to save short_term memory. Extract only genuine long_term facts/preferences/projects/tasks.".to_string(),
                ));
            }

            match item_type {
                "fact" | "preference" | "project" | "task" => {}
                _ => {
                    return Err(Error::Validation(
                        "Ghost channel may only save long_term memory of type: fact/preference/project/task.".to_string(),
                    ));
                }
            }

            let title_text = title.unwrap_or("");
            if looks_like_ghost_maintenance_log(title_text)
                || looks_like_ghost_maintenance_log(content)
            {
                return Err(Error::Validation(
                    "Refusing to save Ghost maintenance logs into memory.".to_string(),
                ));
            }
        }

        let expires_at = expires_in_days
            .map(|days| (chrono::Utc::now() + chrono::Duration::days(days)).to_rfc3339());

        let upsert_params = json!({
            "scope": scope,
            "type": item_type,
            "title": title,
            "content": content,
            "summary": summary,
            "tags": tags_str,
            "source": "tool",
            "channel": ctx.channel,
            "session_key": ctx.session_key,
            "importance": importance,
            "dedup_key": dedup_key,
            "expires_at": expires_at,
        });

        let result = run_store_blocking(move || store.upsert_json(upsert_params)).await?;

        debug!(
            scope = scope,
            item_type = item_type,
            "memory_upsert executed"
        );
        Ok(json!({
            "status": "saved",
            "item": result,
        }))
    }
}

#[async_trait]
impl Tool for MemoryForgetTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "memory_forget".to_string(),
            description: "Delete or restore memory items. Supports single item deletion by ID, batch deletion by filters (scope, type, tags, time), and restoration of soft-deleted items.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["delete", "batch_delete", "restore"],
                        "description": "Action to perform. 'delete' soft-deletes one item by ID, 'batch_delete' soft-deletes by filters, 'restore' recovers a soft-deleted item."
                    },
                    "id": {
                        "type": "string",
                        "description": "Memory item ID (required for 'delete' and 'restore' actions)."
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["long_term", "short_term"],
                        "description": "Filter for batch_delete: only delete items in this scope."
                    },
                    "type": {
                        "type": "string",
                        "description": "Filter for batch_delete: only delete items of this type."
                    },
                    "tags": {
                        "type": "string",
                        "description": "Filter for batch_delete: comma-separated tags (items matching any tag)."
                    },
                    "before_days": {
                        "type": "integer",
                        "description": "Filter for batch_delete: only delete items created more than N days ago."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str());
        match action {
            Some("delete") | Some("restore") => {
                if params.get("id").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation(
                        "'id' is required for delete/restore actions".to_string(),
                    ));
                }
            }
            Some("batch_delete") => {
                let has_filter = params.get("scope").and_then(|v| v.as_str()).is_some()
                    || params.get("type").and_then(|v| v.as_str()).is_some()
                    || params.get("tags").and_then(|v| v.as_str()).is_some()
                    || params.get("before_days").and_then(|v| v.as_i64()).is_some();
                if !has_filter {
                    return Err(Error::Validation(
                        "batch_delete requires at least one filter".to_string(),
                    ));
                }
            }
            _ => {
                return Err(Error::Validation(
                    "'action' must be 'delete', 'batch_delete', or 'restore'".to_string(),
                ));
            }
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let store = get_memory_store(&ctx)?.clone();
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Tool("Missing or non-string parameter: action".to_string()))?;

        match action {
            "delete" => {
                let id = params.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
                    Error::Tool("Missing or non-string parameter: id".to_string())
                })?;
                let id = id.to_string();
                let deleted = {
                    let id = id.clone();
                    let session_key = ctx.session_key.clone();
                    run_store_blocking(move || store.soft_delete_in_session(&id, &session_key))
                        .await?
                };
                Ok(json!({
                    "action": "delete",
                    "id": id,
                    "deleted": deleted,
                    "note": if deleted { "Item moved to recycle bin. Use action='restore' to recover." } else { "Item not found or already deleted." },
                }))
            }
            "batch_delete" => {
                let batch_params = json!({
                    "scope": params.get("scope").and_then(|v| v.as_str()),
                    "type": params.get("type").and_then(|v| v.as_str()),
                    "tags": params.get("tags").and_then(|v| v.as_str()),
                    "before_days": params.get("before_days").and_then(|v| v.as_i64()),
                    "session_key": ctx.session_key,
                });
                let count = run_store_blocking(move || {
                    store.batch_soft_delete_in_session_json(batch_params)
                })
                .await?;
                Ok(json!({
                    "action": "batch_delete",
                    "deleted_count": count,
                    "note": format!("{} items moved to recycle bin.", count),
                }))
            }
            "restore" => {
                let id = params.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
                    Error::Tool("Missing or non-string parameter: id".to_string())
                })?;
                let id = id.to_string();
                let restored = {
                    let id = id.clone();
                    let session_key = ctx.session_key.clone();
                    run_store_blocking(move || store.restore_in_session(&id, &session_key)).await?
                };
                Ok(json!({
                    "action": "restore",
                    "id": id,
                    "restored": restored,
                    "note": if restored { "Item restored from recycle bin." } else { "Item not found in recycle bin." },
                }))
            }
            _ => Err(Error::Validation("Invalid action".to_string())),
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
        last_upsert: Mutex<Option<Value>>,
        last_batch_delete: Mutex<Option<Value>>,
    }

    impl CaptureMemoryStore {
        fn new() -> Self {
            Self {
                last_upsert: Mutex::new(None),
                last_batch_delete: Mutex::new(None),
            }
        }

        fn last_upsert(&self) -> Value {
            self.last_upsert
                .lock()
                .expect("last_upsert lock")
                .clone()
                .expect("captured upsert params")
        }

        fn last_batch_delete(&self) -> Value {
            self.last_batch_delete
                .lock()
                .expect("last_batch_delete lock")
                .clone()
                .expect("captured batch delete params")
        }
    }

    impl crate::MemoryStoreOps for CaptureMemoryStore {
        fn upsert_json(&self, params_json: Value) -> Result<Value> {
            *self.last_upsert.lock().expect("last_upsert lock") = Some(params_json);
            Ok(json!({
                "id": "mem-1",
                "scope": "short_term",
                "type": "note",
                "content": "remember this",
                "summary": null,
                "tags": [],
                "source": "tool",
                "channel": "cli",
                "session_key": "cli:test",
                "importance": 0.5,
                "created_at": "2026-03-25T00:00:00Z",
                "updated_at": "2026-03-25T00:00:00Z",
                "last_accessed_at": null,
                "access_count": 0,
                "expires_at": null,
                "deleted_at": null,
                "dedup_key": null,
            }))
        }

        fn query_json(&self, _params_json: Value) -> Result<Value> {
            Ok(json!([]))
        }

        fn soft_delete(&self, _id: &str) -> Result<bool> {
            Ok(false)
        }

        fn batch_soft_delete_json(&self, params_json: Value) -> Result<usize> {
            *self
                .last_batch_delete
                .lock()
                .expect("last_batch_delete lock") = Some(params_json);
            Ok(0)
        }

        fn batch_soft_delete_in_session_json(&self, params_json: Value) -> Result<usize> {
            self.batch_soft_delete_json(params_json)
        }

        fn soft_delete_in_session(&self, _id: &str, _session_key: &str) -> Result<bool> {
            Ok(false)
        }

        fn restore_in_session(&self, _id: &str, _session_key: &str) -> Result<bool> {
            Ok(false)
        }

        fn stats_in_session_json(&self, _session_key: &str) -> Result<Value> {
            Ok(json!({}))
        }

        fn restore(&self, _id: &str) -> Result<bool> {
            Ok(false)
        }

        fn stats_json(&self) -> Result<Value> {
            Ok(json!({}))
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
    struct CaptureMemoryFileStore {
        calls: Mutex<Vec<Value>>,
    }

    impl CaptureMemoryFileStore {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }

        fn last_call(&self) -> Value {
            self.calls
                .lock()
                .expect("calls lock")
                .last()
                .cloned()
                .expect("captured file memory call")
        }
    }

    impl crate::MemoryFileStoreOps for CaptureMemoryFileStore {
        fn add_file_memory_json(&self, target: &str, content: &str) -> Result<Value> {
            self.calls.lock().expect("calls lock").push(json!({
                "action": "add",
                "target": target,
                "content": content,
            }));
            Ok(json!({
                "success": true,
                "target": target,
                "action": "add",
                "snapshotRef": null,
                "message": "memory updated",
            }))
        }

        fn replace_file_memory_json(
            &self,
            target: &str,
            old_text: &str,
            content: &str,
        ) -> Result<Value> {
            self.calls.lock().expect("calls lock").push(json!({
                "action": "replace",
                "target": target,
                "old_text": old_text,
                "content": content,
            }));
            Ok(json!({
                "success": true,
                "target": target,
                "action": "replace",
                "snapshotRef": "snapshot.md",
                "message": "memory updated",
            }))
        }

        fn remove_file_memory_json(&self, target: &str, old_text: &str) -> Result<Value> {
            self.calls.lock().expect("calls lock").push(json!({
                "action": "remove",
                "target": target,
                "old_text": old_text,
            }));
            Ok(json!({
                "success": true,
                "target": target,
                "action": "remove",
                "snapshotRef": "snapshot.md",
                "message": "memory updated",
            }))
        }

        fn restore_latest_file_memory_json(&self, target: &str) -> Result<Value> {
            self.calls.lock().expect("calls lock").push(json!({
                "action": "undo_latest",
                "target": target,
            }));
            Ok(json!({
                "success": true,
                "target": target,
                "action": "restore_latest",
                "snapshotRef": "snapshot.md",
                "message": "memory restored",
            }))
        }
    }

    struct CaptureGhostMemoryLifecycle {
        calls: Mutex<Vec<Value>>,
    }

    impl CaptureGhostMemoryLifecycle {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl crate::GhostMemoryLifecycleOps for CaptureGhostMemoryLifecycle {
        fn on_memory_write_json(&self, target: &str, action: &str, content: &str) -> Result<Value> {
            self.calls.lock().expect("calls lock").push(json!({
                "target": target,
                "action": action,
                "content": content,
            }));
            Ok(json!({"success": true}))
        }
    }

    fn test_context_with_file_store(
        memory_file_store: Arc<dyn crate::MemoryFileStoreOps + Send + Sync>,
    ) -> ToolContext {
        test_context_with_file_store_and_lifecycle(memory_file_store, None)
    }

    fn test_context_with_file_store_and_lifecycle(
        memory_file_store: Arc<dyn crate::MemoryFileStoreOps + Send + Sync>,
        ghost_memory_lifecycle: Option<Arc<dyn crate::GhostMemoryLifecycleOps + Send + Sync>>,
    ) -> ToolContext {
        ToolContext {
            workspace: PathBuf::from("/tmp/workspace"),
            base: PathBuf::from("/tmp/blockcell"),
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "cli:test".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: None,
            sender_role: None,
            chat_id: "chat-1".to_string(),
            config: Config::default(),
            permissions: blockcell_core::types::PermissionSet::new(),
            task_manager: None,
            memory_store: None,
            memory_file_store: Some(memory_file_store),
            ghost_memory_lifecycle,
            skill_file_store: None,
            session_search: None,
            outbound_tx: None,
            spawn_handle: None,
            capability_registry: None,
            core_evolution: None,
            event_emitter: None,
            channel_contacts_file: None,
            response_cache: None,
            skill_mutex: None,
            agent_type_registry: None,
            evolution_workflow_store: None,
            runtime_handle: None,
            agent_identity: None,
        }
    }

    fn test_context(memory_store: Arc<dyn crate::MemoryStoreOps + Send + Sync>) -> ToolContext {
        ToolContext {
            workspace: PathBuf::from("/tmp/workspace"),
            base: PathBuf::from("/tmp/blockcell"),
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "cli:test".to_string(),
            channel: "cli".to_string(),
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

    fn schema_enum_values(parameters: &Value, field: &str) -> Vec<String> {
        parameters["properties"][field]["enum"]
            .as_array()
            .expect("enum array")
            .iter()
            .filter_map(|value| value.as_str().map(ToString::to_string))
            .collect()
    }
    #[test]
    fn test_memory_manage_validate_requires_fields() {
        let tool = MemoryManageTool;

        assert!(tool.validate(&json!({})).is_err());
        assert!(tool
            .validate(
                &json!({"action": "add", "target": "user", "content": "Prefer Chinese replies."})
            )
            .is_ok());
        assert!(tool
            .validate(&json!({"action": "replace", "target": "memory", "content": "New fact"}))
            .is_err());
        assert!(tool
            .validate(&json!({"action": "remove", "target": "memory"}))
            .is_err());
        assert!(tool
            .validate(&json!({
                "action": "add",
                "target": "user",
                "scope": "workspace",
                "content": "Prefer Chinese replies."
            }))
            .is_err());
        assert!(tool
            .validate(&json!({
                "action": "add",
                "target": "memory",
                "scope": "invalid",
                "content": "Stable project fact."
            }))
            .is_err());
    }

    #[test]
    fn test_memory_manage_schema_declares_scope_with_session_default() {
        let schema = MemoryManageTool.schema();
        let scope = &schema.parameters["properties"]["scope"];

        assert_eq!(scope["default"], "session");
        assert_eq!(
            schema_enum_values(&schema.parameters, "scope"),
            vec!["session", "workspace", "user"]
        );
    }

    #[tokio::test]
    async fn test_memory_manage_execute_add_routes_to_file_store() {
        let store = Arc::new(CaptureMemoryFileStore::new());
        let tool = MemoryManageTool;

        let result = tool
            .execute(
                test_context_with_file_store(store.clone()),
                json!({
                    "action": "add",
                    "target": "user",
                    "content": "User prefers concise Chinese updates."
                }),
            )
            .await
            .expect("memory_manage add should succeed");

        assert_eq!(result["success"], true);
        assert_eq!(store.last_call()["action"], "add");
        assert_eq!(store.last_call()["target"], "user");
    }

    #[tokio::test]
    async fn test_memory_manage_execute_add_notifies_ghost_memory_lifecycle() {
        let store = Arc::new(CaptureMemoryFileStore::new());
        let lifecycle = Arc::new(CaptureGhostMemoryLifecycle::new());

        MemoryManageTool
            .execute(
                test_context_with_file_store_and_lifecycle(store, Some(lifecycle.clone())),
                json!({
                    "action": "add",
                    "target": "user",
                    "content": "User prefers concise Chinese updates."
                }),
            )
            .await
            .expect("memory_manage add should succeed");

        assert_eq!(
            lifecycle.calls.lock().expect("calls lock").as_slice(),
            &[json!({
                "target": "user",
                "action": "add",
                "content": "User prefers concise Chinese updates."
            })]
        );
    }

    #[tokio::test]
    async fn test_memory_manage_execute_replace_routes_old_text() {
        let store = Arc::new(CaptureMemoryFileStore::new());
        let tool = MemoryManageTool;

        tool.execute(
            test_context_with_file_store(store.clone()),
            json!({
                "action": "replace",
                "target": "memory",
                "old_text": "blue-green",
                "content": "Project deploys use canary checks first."
            }),
        )
        .await
        .expect("memory_manage replace should succeed");

        let call = store.last_call();
        assert_eq!(call["action"], "replace");
        assert_eq!(call["old_text"], "blue-green");
    }

    #[tokio::test]
    async fn test_memory_manage_execute_undo_latest_routes_to_file_store() {
        let store = Arc::new(CaptureMemoryFileStore::new());
        let result = MemoryManageTool
            .execute(
                test_context_with_file_store(store.clone()),
                json!({
                    "action": "undo_latest",
                    "target": "user"
                }),
            )
            .await
            .expect("memory_manage undo_latest should succeed");

        assert_eq!(result["action"], json!("restore_latest"));
        let call = store.last_call();
        assert_eq!(call["action"], "undo_latest");
        assert_eq!(call["target"], "user");
    }

    #[tokio::test]
    async fn test_memory_manage_execute_requires_file_store() {
        let tool = MemoryManageTool;
        let store = Arc::new(CaptureMemoryStore::new());

        let err = tool
            .execute(
                test_context(store),
                json!({
                    "action": "add",
                    "target": "user",
                    "content": "User prefers concise Chinese updates."
                }),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("Memory file store not available"));
    }

    #[test]
    fn test_memory_query_schema() {
        let tool = MemoryQueryTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "memory_query");
    }

    #[test]
    fn test_memory_query_validate() {
        let tool = MemoryQueryTool;
        assert!(tool.validate(&json!({})).is_ok());
        assert!(tool.validate(&json!({"query": "test"})).is_ok());
        assert!(tool.validate(&json!({"top_k": -1})).is_err());
        assert!(tool.validate(&json!({"top_k": 0})).is_err());
        assert!(tool.validate(&json!({"top_k": 51})).is_err());
        assert!(tool.validate(&json!({"top_k": 50})).is_ok());
    }

    #[test]
    fn test_memory_query_schema_uses_canonical_types() {
        let tool = MemoryQueryTool;
        let schema = tool.schema();
        let types = schema_enum_values(&schema.parameters, "type");

        assert!(!types.contains(&"summary".to_string()));
        assert!(types.contains(&"session_summary".to_string()));
    }

    #[test]
    fn test_memory_upsert_schema() {
        let tool = MemoryUpsertTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "memory_upsert");
    }

    #[test]
    fn test_memory_upsert_schema_uses_allowed_types_only() {
        let tool = MemoryUpsertTool;
        let schema = tool.schema();
        let types = schema_enum_values(&schema.parameters, "type");

        assert!(!types.contains(&"summary".to_string()));
        assert!(!types.contains(&"session_summary".to_string()));
    }

    #[test]
    fn test_memory_upsert_validate() {
        let tool = MemoryUpsertTool;
        assert!(tool.validate(&json!({"content": "remember this"})).is_ok());
        assert!(tool.validate(&json!({})).is_err());
    }

    #[test]
    fn memory_upsert_rejects_long_term_in_favor_of_canonical_files() {
        let error = MemoryUpsertTool
            .validate(&json!({
                "content": "User prefers concise replies.",
                "scope": "long_term",
                "type": "preference"
            }))
            .expect_err("long-term SQLite writes must be rejected");

        assert!(error.to_string().contains("memory_manage"));
        assert!(error.to_string().contains("canonical"));
    }

    #[tokio::test]
    async fn test_memory_upsert_execute_leaves_default_ttl_to_storage_service() {
        let store = Arc::new(CaptureMemoryStore::new());
        let tool = MemoryUpsertTool;

        tool.execute(
            test_context(store.clone()),
            json!({
                "content": "remember this",
                "scope": "short_term",
                "type": "note"
            }),
        )
        .await
        .expect("memory_upsert should succeed");

        let captured = store.last_upsert();
        assert!(captured["expires_at"].is_null());
    }

    #[test]
    fn test_memory_forget_schema() {
        let tool = MemoryForgetTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "memory_forget");
    }

    #[test]
    fn test_memory_forget_validate() {
        let tool = MemoryForgetTool;
        assert!(tool
            .validate(&json!({"action": "delete", "id": "abc"}))
            .is_ok());
        assert!(tool
            .validate(&json!({"action": "restore", "id": "abc"}))
            .is_ok());
        assert!(tool.validate(&json!({"action": "batch_delete"})).is_err());
        assert!(tool
            .validate(&json!({"action": "batch_delete", "scope": "short_term"}))
            .is_ok());
        assert!(tool.validate(&json!({"action": "delete"})).is_err());
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }

    #[tokio::test]
    async fn memory_forget_batch_delete_forwards_caller_session() {
        let store = Arc::new(CaptureMemoryStore::new());

        MemoryForgetTool
            .execute(
                test_context(store.clone()),
                json!({"action": "batch_delete", "scope": "short_term"}),
            )
            .await
            .expect("batch delete should execute");

        assert_eq!(store.last_batch_delete()["session_key"], "cli:test");
    }
}
