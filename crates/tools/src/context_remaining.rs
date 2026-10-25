use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolSchema};

pub struct GetContextRemainingTool;

#[async_trait]
impl Tool for GetContextRemainingTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "get_context_remaining".to_string(),
            description: "Report the current session's token usage and remaining context budget so long coding tasks can decide when to summarize or finish.".to_string(),
            parameters: json!({"type": "object", "properties": {}}),
        }
    }

    fn validate(&self, _params: &Value) -> Result<()> {
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, _params: Value) -> Result<Value> {
        let runtime = ctx.runtime_handle.ok_or_else(|| {
            Error::Tool("get_context_remaining requires an active agent runtime".to_string())
        })?;
        let snapshot = runtime.context_window_snapshot(&ctx.session_key);
        let token_limit = snapshot.token_limit;
        let unlimited = token_limit == 0;
        let remaining_ratio = if unlimited {
            Value::Null
        } else {
            json!(snapshot.tokens_remaining as f64 / token_limit as f64)
        };
        Ok(json!({
            "session_key": ctx.session_key,
            "token_limit": if unlimited { Value::Null } else { json!(token_limit) },
            "tokens_used": snapshot.tokens_used,
            "tokens_remaining": if unlimited { Value::Null } else { json!(snapshot.tokens_remaining) },
            "remaining_ratio": remaining_ratio,
            "unlimited": unlimited,
            "compact_threshold_tokens": snapshot.compact_threshold_tokens,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::{Config, ContextWindowSnapshot};
    use std::sync::Arc;

    struct RuntimeBudget;

    impl crate::RuntimeHandle for RuntimeBudget {
        fn subagents(&self) -> Arc<dyn crate::subagent::SubagentProvider> {
            unreachable!()
        }

        fn context_window_snapshot(&self, session_key: &str) -> ContextWindowSnapshot {
            assert_eq!(session_key, "cli:budget");
            ContextWindowSnapshot {
                tokens_used: 40,
                token_limit: 100,
                tokens_remaining: 60,
                compact_threshold_tokens: 80,
            }
        }
    }

    fn context(config: Config) -> crate::ToolContext {
        crate::ToolContext {
            workspace: std::env::temp_dir(),
            sandbox_writable_roots: None,
            base: std::env::temp_dir(),
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "cli:budget".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: Some("user".to_string()),
            sender_role: None,
            chat_id: "budget".to_string(),
            config,
            permissions: blockcell_core::types::PermissionSet::new(),
            task_manager: None,
            memory_store: None,
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
            runtime_handle: Some(Arc::new(RuntimeBudget)),
            agent_identity: None,
            skill_mutex: None,
            agent_type_registry: None,
            evolution_workflow_store: None,
        }
    }

    #[tokio::test]
    async fn context_remaining_reports_current_session_budget() {
        let value = GetContextRemainingTool
            .execute(context(Config::default()), json!({}))
            .await
            .expect("context remaining");

        assert_eq!(value["token_limit"], 100);
        assert_eq!(value["tokens_used"], 40);
        assert_eq!(value["tokens_remaining"], 60);
        assert_eq!(value["remaining_ratio"], 0.6);
        assert_eq!(value["compact_threshold_tokens"], 80);
        assert_eq!(value["unlimited"], false);
    }

    #[tokio::test]
    async fn unlimited_budget_is_reported_without_fake_remaining_count() {
        struct UnlimitedRuntime;
        impl crate::RuntimeHandle for UnlimitedRuntime {
            fn subagents(&self) -> Arc<dyn crate::subagent::SubagentProvider> {
                unreachable!()
            }
            fn context_window_snapshot(&self, _session_key: &str) -> ContextWindowSnapshot {
                ContextWindowSnapshot {
                    tokens_used: 0,
                    token_limit: 0,
                    tokens_remaining: 0,
                    compact_threshold_tokens: 0,
                }
            }
        }
        let mut ctx = context(Config::default());
        ctx.runtime_handle = Some(Arc::new(UnlimitedRuntime));
        let value = GetContextRemainingTool
            .execute(ctx, json!({}))
            .await
            .expect("context remaining");

        assert!(value["token_limit"].is_null());
        assert!(value["tokens_remaining"].is_null());
        assert!(value["remaining_ratio"].is_null());
        assert_eq!(value["unlimited"], true);
    }
}
