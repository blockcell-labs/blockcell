use async_trait::async_trait;
use blockcell_core::{Error, Paths, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{Tool, ToolContext, ToolSchema};

pub struct KnowledgeForgetTool;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ForgetMatch {
    kind: String,
    key: String,
    target: Option<String>,
    scope: Option<String>,
    content: String,
}

#[async_trait]
impl Tool for KnowledgeForgetTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "knowledge_forget".to_string(),
            description: "Preview and confirm deletion of matching knowledge across canonical files, current-session memory, and SQLite short-term memory. Confirmation requires the exact preview token.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["preview", "confirm"]
                    },
                    "query": {
                        "type": "string",
                        "description": "Knowledge text or canonical entry ID to forget."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Reason recorded in the tombstone and GhostLedger audit."
                    },
                    "preview_token": {
                        "type": "string",
                        "description": "Exact token returned by preview; required for confirm."
                    }
                },
                "required": ["action", "query"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Validation("action is required".to_string()))?;
        if !matches!(action, "preview" | "confirm") {
            return Err(Error::Validation(
                "action must be 'preview' or 'confirm'".to_string(),
            ));
        }
        let query = params
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if query.is_empty() {
            return Err(Error::Validation("query cannot be empty".to_string()));
        }
        if action == "confirm"
            && params
                .get("preview_token")
                .and_then(Value::as_str)
                .is_none()
        {
            return Err(Error::Validation(
                "preview_token is required for confirm".to_string(),
            ));
        }
        Ok(())
    }

    fn requires_confirmation(&self, params: &Value) -> bool {
        // `confirm` is the step that actually deletes across canonical files,
        // session memory and SQLite. Without a user-side gate the model can
        // approve its own deletion by re-calling with the token it just received
        // from `preview`.
        params.get("action").and_then(Value::as_str) == Some("confirm")
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        self.validate(&params)?;
        // The index rebuild, SQLite writes and file removals are blocking (and
        // `collect_matches` can walk the whole knowledge directory): keep them off the
        // async worker.
        tokio::task::spawn_blocking(move || {
            let action = params["action"].as_str().unwrap_or_default();
            let query = params["query"].as_str().unwrap_or_default().trim();
            let paths = Paths::with_base_and_workspace(ctx.base.clone(), ctx.workspace.clone());
            let matches = collect_matches(&paths, &ctx, query)?;
            let preview_token = preview_token(query, &ctx.session_key, &matches)?;

            if action == "preview" {
                return Ok(json!({
                    "action": "preview",
                    "query": query,
                    "matches": matches,
                    "preview_token": preview_token,
                    "requires_confirmation": true
                }));
            }

            let supplied = params["preview_token"].as_str().unwrap_or_default();
            if supplied != preview_token {
                return Err(Error::Validation(
                    "preview_token no longer matches the exact affected knowledge; run preview again"
                        .to_string(),
                ));
            }
            let reason = params
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("user request");
            let removed = confirm_matches(&paths, &ctx, &matches, reason)?;
            audit_forget(&paths, &ctx, query, reason, &matches)?;
            Ok(json!({
                "action": "confirm",
                "query": query,
                "removed": removed,
                "tombstoned": matches.len(),
                "audit": "GhostLedger"
            }))
        })
        .await
        .map_err(|e| Error::Tool(format!("knowledge_forget worker failed: {e}")))?
    }
}

fn collect_matches(paths: &Paths, ctx: &ToolContext, query: &str) -> Result<Vec<ForgetMatch>> {
    let index = blockcell_storage::KnowledgeIndex::open(&paths.knowledge_index_db())?;
    index.rebuild_from_files(paths)?;
    let mut canonical_entries = index.search(query, 100)?;
    if let Some(exact) = index.get_by_id(query)? {
        if !canonical_entries.iter().any(|entry| entry.id == exact.id) {
            canonical_entries.push(exact);
        }
    }
    let mut matches = canonical_entries
        .into_iter()
        .map(|entry| ForgetMatch {
            kind: "canonical_file".to_string(),
            key: entry.id,
            target: Some(if entry.file == "USER.md" {
                "user".to_string()
            } else {
                "memory".to_string()
            }),
            scope: Some(entry.scope),
            content: entry.content,
        })
        .collect::<Vec<_>>();

    matches.extend(collect_session_file_matches(
        paths,
        &ctx.session_key,
        query,
    )?);
    if let Some(store) = ctx.memory_store.as_ref() {
        let rows = store.query_json(json!({
            "query": query,
            "session_key": ctx.session_key,
            "scope": "short_term",
            "top_k": 50,
            "include_deleted": false
        }))?;
        if let Some(rows) = rows.as_array() {
            for row in rows {
                let item = row.get("item").unwrap_or(row);
                let Some(id) = item.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let Some(content) = item.get("content").and_then(Value::as_str) else {
                    continue;
                };
                matches.push(ForgetMatch {
                    kind: "sqlite_short_term".to_string(),
                    key: id.to_string(),
                    target: None,
                    scope: Some("session".to_string()),
                    content: content.to_string(),
                });
            }
        }
    }
    matches.sort_by(|left, right| left.kind.cmp(&right.kind).then(left.key.cmp(&right.key)));
    matches.dedup_by(|left, right| left.kind == right.kind && left.key == right.key);
    Ok(matches)
}

fn collect_session_file_matches(
    paths: &Paths,
    session_key: &str,
    query: &str,
) -> Result<Vec<ForgetMatch>> {
    let root = paths
        .memory_dir()
        .join("sessions")
        .join(blockcell_core::stable_hash_session_key(session_key));
    let query = query.to_lowercase();
    let mut matches = Vec::new();
    for (target, path) in [
        ("user", root.join("USER.md")),
        ("memory", root.join("MEMORY.md")),
    ] {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for (index, chunk) in content
            .split("\n\n")
            .map(str::trim)
            .filter(|chunk| !chunk.is_empty())
            .enumerate()
        {
            if chunk.to_lowercase().contains(&query) {
                matches.push(ForgetMatch {
                    kind: "session_file".to_string(),
                    key: format!("{target}:{index}"),
                    target: Some(target.to_string()),
                    scope: Some("session".to_string()),
                    content: chunk.to_string(),
                });
            }
        }
    }
    Ok(matches)
}

fn preview_token(query: &str, session_key: &str, matches: &[ForgetMatch]) -> Result<String> {
    let payload = serde_json::to_vec(&(query, session_key, matches))
        .map_err(|error| Error::Tool(format!("failed to encode forget preview: {error}")))?;
    let digest = Sha256::digest(payload);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn confirm_matches(
    paths: &Paths,
    ctx: &ToolContext,
    matches: &[ForgetMatch],
    reason: &str,
) -> Result<usize> {
    let file_store = ctx.memory_file_store.as_ref();
    let index = blockcell_storage::KnowledgeIndex::open(&paths.knowledge_index_db())?;
    let mut removed = 0usize;
    for item in matches {
        // Tombstone each entry only after its deletion succeeded, so a mid-loop
        // failure leaves "removed entries tombstoned, rest untouched" instead of
        // "everything tombstoned but only partially removed" — with the old
        // ordering, undeleted entries could never be re-learned.
        match item.kind.as_str() {
            "canonical_file" => {
                let store = file_store.ok_or_else(|| {
                    Error::Tool("Memory file store not available for canonical forget".to_string())
                })?;
                store.remove_scoped_file_memory_json(
                    item.scope.as_deref().unwrap_or("workspace"),
                    item.target.as_deref().unwrap_or("memory"),
                    &if item.key.starts_with("legacy-") {
                        item.content.clone()
                    } else {
                        format!("[id:{}]", item.key)
                    },
                )?;
                index.record_forgotten_content(&item.content, reason)?;
                removed += 1;
            }
            "session_file" => {
                let store = file_store.ok_or_else(|| {
                    Error::Tool("Memory file store not available for session forget".to_string())
                })?;
                store.remove_scoped_file_memory_json(
                    "session",
                    item.target.as_deref().unwrap_or("memory"),
                    &item.content,
                )?;
                index.record_forgotten_content(&item.content, reason)?;
                removed += 1;
            }
            "sqlite_short_term" => {
                let store = ctx.memory_store.as_ref().ok_or_else(|| {
                    Error::Tool("Memory store not available for short-term forget".to_string())
                })?;
                if store.soft_delete_in_session(&item.key, &ctx.session_key)? {
                    index.record_forgotten_content(&item.content, reason)?;
                    removed += 1;
                }
            }
            _ => {}
        }
    }

    index.rebuild_from_files(paths)?;
    Ok(removed)
}

fn audit_forget(
    paths: &Paths,
    ctx: &ToolContext,
    query: &str,
    reason: &str,
    matches: &[ForgetMatch],
) -> Result<()> {
    let ledger = blockcell_storage::GhostLedger::open(&paths.ghost_ledger_db())?;
    ledger.insert_episode(blockcell_storage::ghost_ledger::NewGhostEpisode {
        boundary_kind: "knowledge_forget".to_string(),
        subject_key: Some(ctx.session_key.clone()),
        status: "completed".to_string(),
        summary: format!("Forgot {} knowledge matches", matches.len()),
        metadata: json!({
            "query": query,
            "reason": reason,
            "callerSession": ctx.session_key,
            "affectedSources": matches,
            "timestamp": chrono::Utc::now().to_rfc3339()
        }),
        sources: matches
            .iter()
            .map(|item| blockcell_storage::ghost_ledger::GhostEpisodeSource {
                source_type: item.kind.clone(),
                source_key: item.key.clone(),
                role: "forgotten".to_string(),
            })
            .collect(),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryFileStoreOps, MemoryStoreOps, Tool, ToolContext};
    use blockcell_core::types::PermissionSet;
    use blockcell_core::{Config, Paths, Result};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    struct TestMemoryStore {
        active: Mutex<bool>,
    }

    impl MemoryStoreOps for TestMemoryStore {
        fn upsert_json(&self, _params_json: Value) -> Result<Value> {
            Ok(Value::Null)
        }
        fn query_json(&self, _params_json: Value) -> Result<Value> {
            if *self.active.lock().unwrap() {
                Ok(json!([{
                    "score": 1.0,
                    "item": {
                        "id": "short-1",
                        "content": "Temporary concise reply preference."
                    }
                }]))
            } else {
                Ok(json!([]))
            }
        }
        fn soft_delete(&self, _id: &str) -> Result<bool> {
            Ok(false)
        }
        fn soft_delete_in_session(&self, id: &str, _session_key: &str) -> Result<bool> {
            if id == "short-1" {
                *self.active.lock().unwrap() = false;
                Ok(true)
            } else {
                Ok(false)
            }
        }
        fn batch_soft_delete_json(&self, _params_json: Value) -> Result<usize> {
            Ok(0)
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

    struct TestFileStore {
        paths: Paths,
        session_key: String,
    }

    impl MemoryFileStoreOps for TestFileStore {
        fn add_file_memory_json(&self, _target: &str, _content: &str) -> Result<Value> {
            Ok(json!({"success": true}))
        }
        fn replace_file_memory_json(
            &self,
            _target: &str,
            _old_text: &str,
            _content: &str,
        ) -> Result<Value> {
            Ok(json!({"success": true}))
        }
        fn remove_file_memory_json(&self, target: &str, old_text: &str) -> Result<Value> {
            let path = if target == "user" {
                self.paths.user_md()
            } else {
                self.paths.memory_md()
            };
            remove_from_path(&path, old_text)
        }
        fn remove_scoped_file_memory_json(
            &self,
            scope: &str,
            target: &str,
            old_text: &str,
        ) -> Result<Value> {
            if scope != "session" {
                return self.remove_file_memory_json(target, old_text);
            }
            let root = self
                .paths
                .memory_dir()
                .join("sessions")
                .join(blockcell_core::stable_hash_session_key(&self.session_key));
            let path = if target == "user" {
                root.join("USER.md")
            } else {
                root.join("MEMORY.md")
            };
            remove_from_path(&path, old_text)
        }
        fn restore_latest_file_memory_json(&self, _target: &str) -> Result<Value> {
            Ok(json!({"success": true}))
        }
    }

    fn remove_from_path(path: &std::path::Path, old_text: &str) -> Result<Value> {
        let content = std::fs::read_to_string(&path)?;
        let retained = content
            .lines()
            .filter(|line| !line.contains(old_text))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(path, retained)?;
        Ok(json!({"success": true}))
    }

    fn test_context(
        paths: &Paths,
        memory_store: Arc<TestMemoryStore>,
        file_store: Arc<TestFileStore>,
    ) -> ToolContext {
        ToolContext {
            workspace: paths.workspace(),
            base: paths.base.clone(),
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "cli:test".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: Some("user".to_string()),
            sender_role: None,
            chat_id: "test".to_string(),
            config: Config::default(),
            permissions: PermissionSet::new(),
            task_manager: None,
            memory_store: Some(memory_store),
            memory_file_store: Some(file_store),
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

    #[tokio::test]
    async fn knowledge_forget_preview_then_confirm_removes_all_matches_and_tombstones() {
        let paths = Paths::with_base(std::env::temp_dir().join(format!(
            "blockcell-knowledge-forget-{}",
            uuid::Uuid::new_v4()
        )));
        paths.ensure_dirs().unwrap();
        std::fs::write(
            paths.user_md(),
            "- [id:pref-concise] [scope:user] [source:user_statement] [updated:2026-08-01] User prefers concise replies.\n",
        )
        .unwrap();
        let index = blockcell_storage::KnowledgeIndex::open(&paths.knowledge_index_db()).unwrap();
        index.rebuild_from_files(&paths).unwrap();
        let session_root = paths
            .memory_dir()
            .join("sessions")
            .join(blockcell_core::stable_hash_session_key("cli:test"));
        std::fs::create_dir_all(&session_root).unwrap();
        std::fs::write(
            session_root.join("MEMORY.md"),
            "Session concise replies preference.\n",
        )
        .unwrap();
        let memory_store = Arc::new(TestMemoryStore {
            active: Mutex::new(true),
        });
        let file_store = Arc::new(TestFileStore {
            paths: paths.clone(),
            session_key: "cli:test".to_string(),
        });
        let ctx = test_context(&paths, memory_store.clone(), file_store);

        let preview = KnowledgeForgetTool
            .execute(
                ctx.clone(),
                json!({"action": "preview", "query": "concise replies"}),
            )
            .await
            .expect("preview forgetting");
        assert_eq!(preview["matches"].as_array().unwrap().len(), 3);
        assert!(std::fs::read_to_string(paths.user_md())
            .unwrap()
            .contains("pref-concise"));
        assert!(*memory_store.active.lock().unwrap());

        let confirmed = KnowledgeForgetTool
            .execute(
                ctx,
                json!({
                    "action": "confirm",
                    "query": "concise replies",
                    "reason": "user request",
                    "preview_token": preview["preview_token"]
                }),
            )
            .await
            .expect("confirm forgetting");
        assert_eq!(confirmed["removed"], 3);
        assert!(!std::fs::read_to_string(paths.user_md())
            .unwrap()
            .contains("pref-concise"));
        assert!(!*memory_store.active.lock().unwrap());
        assert!(!std::fs::read_to_string(session_root.join("MEMORY.md"))
            .unwrap()
            .contains("concise"));
        index.rebuild_from_files(&paths).unwrap();
        assert!(index.search("concise replies", 10).unwrap().is_empty());
        assert!(index
            .is_forgotten_content("User prefers concise replies.")
            .unwrap());
        let ledger = blockcell_storage::GhostLedger::open(&paths.ghost_ledger_db()).unwrap();
        assert_eq!(
            ledger
                .episode_count_by_boundary_kind("knowledge_forget")
                .unwrap(),
            1
        );
        assert!(crate::ToolRegistry::with_defaults()
            .get("knowledge_forget")
            .is_some());
    }
}
