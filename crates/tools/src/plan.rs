use async_trait::async_trait;
use blockcell_core::{Error, Result};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use crate::{Tool, ToolContext, ToolSchema};

const MAX_PLAN_SESSIONS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

impl PlanStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PlanItem {
    step: String,
    status: PlanStatus,
}

#[derive(Debug, Clone)]
struct StoredPlan {
    items: Vec<PlanItem>,
    revision: u64,
    injected_revision: u64,
}

#[derive(Default)]
struct SessionPlanStore {
    plans: HashMap<String, StoredPlan>,
    recency: VecDeque<String>,
}

impl SessionPlanStore {
    fn touch(&mut self, session_key: &str) {
        self.recency.retain(|key| key != session_key);
        self.recency.push_back(session_key.to_string());
        while self.recency.len() > MAX_PLAN_SESSIONS {
            if let Some(evicted) = self.recency.pop_front() {
                self.plans.remove(&evicted);
            }
        }
    }
}

static PLAN_STORE: Lazy<Mutex<SessionPlanStore>> =
    Lazy::new(|| Mutex::new(SessionPlanStore::default()));

pub struct UpdatePlanTool;

fn parse_plan(params: &Value) -> Result<Vec<PlanItem>> {
    crate::evidence::ExecutionAnnotations::parse(params)?;
    let plan = params
        .get("plan")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Validation("Missing required array parameter: plan".to_string()))?;
    let mut in_progress = 0usize;
    let mut items = Vec::with_capacity(plan.len());
    for (index, item) in plan.iter().enumerate() {
        let step = item
            .get("step")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|step| !step.is_empty())
            .ok_or_else(|| {
                Error::Validation(format!("plan item {} requires a non-empty step", index + 1))
            })?;
        let status_value = item
            .get("status")
            .cloned()
            .ok_or_else(|| Error::Validation(format!("plan item {} requires status", index + 1)))?;
        let status: PlanStatus = serde_json::from_value(status_value).map_err(|_| {
            Error::Validation(format!(
                "plan item {} status must be pending, in_progress, or completed",
                index + 1
            ))
        })?;
        if status == PlanStatus::InProgress {
            in_progress += 1;
        }
        items.push(PlanItem {
            step: step.to_string(),
            status,
        });
    }
    if in_progress > 1 {
        return Err(Error::Validation(
            "plan may contain at most one in_progress step".to_string(),
        ));
    }
    Ok(items)
}

fn render_plan(items: &[PlanItem], heading: &str) -> String {
    let mut rendered = format!("## {heading}\n");
    if items.is_empty() {
        rendered.push_str("(no active plan)");
        return rendered;
    }
    for item in items {
        rendered.push_str(&format!("- [{}] {}\n", item.status.as_str(), item.step));
    }
    rendered.trim_end().to_string()
}

fn update_plan(session_key: &str, items: Vec<PlanItem>) -> (bool, u64) {
    let mut store = PLAN_STORE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.touch(session_key);
    if let Some(stored) = store.plans.get(session_key) {
        if stored.items == items {
            return (false, stored.revision);
        }
    }
    let revision = store
        .plans
        .get(session_key)
        .map(|stored| stored.revision.saturating_add(1))
        .unwrap_or(1);
    store.plans.insert(
        session_key.to_string(),
        StoredPlan {
            items,
            revision,
            injected_revision: 0,
        },
    );
    (true, revision)
}

/// Replace a session plan from the same JSON shape accepted by `update_plan`.
pub fn replace_plan_for_session(session_key: &str, params: &Value) -> Result<bool> {
    let items = parse_plan(params)?;
    Ok(update_plan(session_key, items).0)
}

/// Return the current plan once after each change for incremental runtime-context injection.
pub fn take_changed_plan_context(session_key: &str) -> Option<String> {
    let mut store = PLAN_STORE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.touch(session_key);
    let stored = store.plans.get_mut(session_key)?;
    if stored.injected_revision == stored.revision {
        return None;
    }
    stored.injected_revision = stored.revision;
    Some(render_plan(&stored.items, "Current Plan"))
}

/// Return the complete plan without changing incremental injection state.
pub fn render_plan_for_recovery(session_key: &str) -> Option<String> {
    let mut store = PLAN_STORE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.touch(session_key);
    store
        .plans
        .get(session_key)
        .map(|stored| render_plan(&stored.items, "Current Plan (Recovery)"))
}

#[async_trait]
impl Tool for UpdatePlanTool {
    fn coding_exposure(&self) -> crate::base_packs::CodingExposure {
        crate::base_packs::CodingExposure::full()
    }

    fn accepts_execution_annotations(&self) -> bool {
        true
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "update_plan".to_string(),
            description: "Replace the current session plan with a structured list of steps. Use pending, in_progress, or completed; at most one step may be in_progress.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "step": {"type": "string"},
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["step", "status"]
                        }
                    }
                },
                "required": ["plan"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        parse_plan(params).map(|_| ())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let items = parse_plan(&params)?;
        let rendered = render_plan(&items, "Current Plan");
        let (changed, revision) = update_plan(&ctx.session_key, items);
        Ok(json!({
            "task_contract": params.get("task_contract"),
            "changed": changed,
            "revision": revision,
            "plan": rendered,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Tool, ToolContext};
    use serde_json::json;

    fn context(session_key: &str) -> ToolContext {
        let workspace = std::env::temp_dir();
        ToolContext {
            workspace: workspace.clone(),
            sandbox_writable_roots: None,
            base: workspace,
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: session_key.to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: None,
            sender_role: None,
            chat_id: "test-chat".to_string(),
            config: blockcell_core::Config::default(),
            permissions: crate::PermissionSet::new(),
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
            runtime_handle: None,
            agent_identity: None,
            skill_mutex: None,
            agent_type_registry: None,
            evolution_workflow_store: None,
        }
    }

    fn steps() -> serde_json::Value {
        json!([
            {"step": "inspect code", "status": "completed"},
            {"step": "implement fix", "status": "in_progress"},
            {"step": "run tests", "status": "pending"}
        ])
    }

    #[tokio::test]
    async fn update_plan_stores_plan_per_session() {
        let session_a = format!("plan-a-{}", uuid::Uuid::new_v4());
        let session_b = format!("plan-b-{}", uuid::Uuid::new_v4());

        UpdatePlanTool
            .execute(context(&session_a), json!({"plan": steps()}))
            .await
            .unwrap();

        assert!(render_plan_for_recovery(&session_a)
            .unwrap()
            .contains("implement fix"));
        assert!(render_plan_for_recovery(&session_b).is_none());
    }

    #[tokio::test]
    async fn changed_plan_is_injected_once_but_recovery_remains_available() {
        let session = format!("plan-incremental-{}", uuid::Uuid::new_v4());
        UpdatePlanTool
            .execute(context(&session), json!({"plan": steps()}))
            .await
            .unwrap();

        let first = take_changed_plan_context(&session).unwrap();
        assert!(first.contains("## Current Plan"));
        assert!(first.contains("[in_progress] implement fix"));
        assert!(take_changed_plan_context(&session).is_none());
        assert!(render_plan_for_recovery(&session)
            .unwrap()
            .contains("[completed] inspect code"));
    }

    #[test]
    fn update_plan_rejects_invalid_status_and_multiple_in_progress_steps() {
        let tool = UpdatePlanTool;
        assert!(tool
            .validate(&json!({"plan": [{"step": "x", "status": "unknown"}]}))
            .is_err());
        assert!(tool
            .validate(&json!({"plan": [
                {"step": "x", "status": "in_progress"},
                {"step": "y", "status": "in_progress"}
            ]}))
            .is_err());
    }

    #[test]
    fn update_plan_schema_is_structured() {
        let schema = UpdatePlanTool.schema();
        assert_eq!(schema.name, "update_plan");
        assert_eq!(schema.parameters["properties"]["plan"]["type"], "array");
        assert_eq!(
            schema.parameters["properties"]["plan"]["items"]["properties"]["status"]["enum"],
            json!(["pending", "in_progress", "completed"])
        );
    }
}
