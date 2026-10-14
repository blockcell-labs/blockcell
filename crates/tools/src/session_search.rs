use crate::{Tool, ToolContext, ToolSchema};
use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};

pub struct SessionSearchTool;

#[async_trait]
impl Tool for SessionSearchTool {
    fn execution_environment(&self, _params: &Value) -> crate::ExecutionEnvironment {
        crate::ExecutionEnvironment::Local
    }

    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Safe
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "session_search".to_string(),
            description: "Search the captured session/episode context read-only during background learning review before deciding what durable memory or skill updates are justified.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search terms to find relevant prior session context."},
                    "limit": {"type": "integer", "description": "Maximum snippets to return. Defaults to 5."}
                },
                "required": ["query"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let query = params
            .get("query")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim();
        if query.is_empty() {
            return Err(Error::Validation(
                "session_search requires non-empty query".to_string(),
            ));
        }
        Ok(())
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- Use `session_search` only as read-only evidence lookup before writing durable memory or skills.".to_string())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let query = params
            .get("query")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim();
        let limit = params
            .get("limit")
            .and_then(|value| value.as_u64())
            .unwrap_or(5)
            .clamp(1, 20) as usize;
        let search = ctx
            .session_search
            .as_ref()
            .ok_or_else(|| Error::Tool("Session search not available".to_string()))?;
        search.search_session_json(query, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    struct CaptureSessionSearch;

    impl crate::SessionSearchOps for CaptureSessionSearch {
        fn search_session_json(&self, query: &str, limit: usize) -> Result<Value> {
            Ok(json!({"query": query, "limit": limit, "results": ["hit"]}))
        }
    }

    fn tool_context(search: Option<Arc<dyn crate::SessionSearchOps + Send + Sync>>) -> ToolContext {
        ToolContext {
            workspace: PathBuf::from("/tmp/workspace"),
            base: PathBuf::from("/tmp/blockcell"),
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "cli:test".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: None,
            chat_id: "chat-1".to_string(),
            config: blockcell_core::Config::default(),
            permissions: blockcell_core::types::PermissionSet::new(),
            task_manager: None,
            memory_store: None,
            memory_file_store: None,
            ghost_memory_lifecycle: None,
            skill_file_store: None,
            session_search: search,
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

    #[test]
    fn session_search_validate_requires_query() {
        assert!(SessionSearchTool.validate(&json!({})).is_err());
        assert!(SessionSearchTool
            .validate(&json!({"query": "rollback"}))
            .is_ok());
    }

    #[tokio::test]
    async fn session_search_routes_to_handle() {
        let result = SessionSearchTool
            .execute(
                tool_context(Some(Arc::new(CaptureSessionSearch))),
                json!({"query": "rollback", "limit": 3}),
            )
            .await
            .unwrap();

        assert_eq!(result["query"], json!("rollback"));
        assert_eq!(result["limit"], json!(3));
        assert_eq!(result["results"][0], json!("hit"));
    }
}
