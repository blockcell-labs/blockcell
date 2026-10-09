use async_trait::async_trait;
use blockcell_core::types::ChatMessage;
use blockcell_core::{Error, Result};
use blockcell_providers::Provider;
use blockcell_skills::{CapabilityRegistry, CoreEvolution, LLMProvider};
use blockcell_storage::EvolutionWorkflowStore;
use blockcell_tools::{
    CapabilityRegistryHandle, CapabilityRegistryOps, CoreEvolutionOps, EvolutionWorkflowStoreOps,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Bridge: wraps a `Provider` (providers crate) to implement `LLMProvider` (skills crate).
/// This allows CoreEvolution to use the same LLM as the rest of the agent.
pub struct ProviderLLMBridge {
    provider: Arc<dyn Provider>,
}

impl ProviderLLMBridge {
    pub fn new(provider: Box<dyn Provider>) -> Self {
        Self {
            provider: Arc::from(provider),
        }
    }

    pub fn new_arc(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl LLMProvider for ProviderLLMBridge {
    async fn generate(&self, prompt: &str) -> Result<String> {
        let messages = vec![
            ChatMessage::system("You are a capability code generator. Output ONLY the requested code in a fenced code block. No explanations."),
            ChatMessage::user(prompt),
        ];
        let response = self.provider.chat(&messages, &[]).await?;
        Ok(response.content.unwrap_or_default())
    }
}

/// Adapter: bridges `CapabilityRegistry` (skills crate) → `CapabilityRegistryOps` (tools crate)
/// Bridge for skill evolution prompts. Skill evolution can request patches,
/// audits, and structured feedback, so it must not force code-only output.
pub struct SkillEvolutionLLMBridge {
    provider: Arc<dyn Provider>,
}

impl SkillEvolutionLLMBridge {
    pub fn new_arc(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl LLMProvider for SkillEvolutionLLMBridge {
    async fn generate(&self, prompt: &str) -> Result<String> {
        let messages = vec![
            ChatMessage::system(
                "You are a skill evolution assistant. Follow the requested output format exactly.",
            ),
            ChatMessage::user(prompt),
        ];
        let response = self.provider.chat(&messages, &[]).await?;
        Ok(response.content.unwrap_or_default())
    }
}

pub struct CapabilityRegistryAdapter {
    inner: Arc<Mutex<CapabilityRegistry>>,
}

impl CapabilityRegistryAdapter {
    pub fn new(registry: Arc<Mutex<CapabilityRegistry>>) -> Self {
        Self { inner: registry }
    }
}

pub fn capability_registry_ops_handle(
    adapter: CapabilityRegistryAdapter,
) -> CapabilityRegistryHandle {
    Arc::new(adapter)
}

#[async_trait]
impl CapabilityRegistryOps for CapabilityRegistryAdapter {
    async fn list_all_json(&self) -> Value {
        let registry = self.inner.lock().await;
        let all = registry.list_all();
        let caps: Vec<Value> = all
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "name": c.name,
                    "description": c.description,
                    "type": format!("{:?}", c.capability_type),
                    "provider": format!("{:?}", c.provider_kind),
                    "status": format!("{:?}", c.status),
                    "version": c.version,
                })
            })
            .collect();
        json!(caps)
    }

    async fn get_descriptor_json(&self, id: &str) -> Option<Value> {
        let registry = self.inner.lock().await;
        registry.get_descriptor(id).map(|c| {
            json!({
                "id": c.id,
                "name": c.name,
                "description": c.description,
                "type": format!("{:?}", c.capability_type),
                "provider": format!("{:?}", c.provider_kind),
                "status": format!("{:?}", c.status),
                "version": c.version,
                "privilege": format!("{:?}", c.privilege),
            })
        })
    }

    async fn stats_json(&self) -> Value {
        let registry = self.inner.lock().await;
        let stats = registry.stats();
        json!({
            "total": stats.total,
            "active": stats.active,
            "available": stats.available,
            "evolving": stats.evolving,
        })
    }

    async fn execute_capability(&self, id: &str, input: Value) -> Result<Value> {
        blockcell_skills::capability_provider::execute_registered_capability(&self.inner, id, input)
            .await
    }

    async fn generate_brief(&self) -> String {
        let registry = self.inner.lock().await;
        registry.generate_brief()
    }

    async fn list_available_ids(&self) -> Vec<String> {
        let registry = self.inner.lock().await;
        registry
            .list_available()
            .iter()
            .map(|d| d.id.clone())
            .collect()
    }
}

/// Adapter: bridges `CoreEvolution` (skills crate) → `CoreEvolutionOps` (tools crate)
pub struct CoreEvolutionAdapter {
    inner: Arc<Mutex<CoreEvolution>>,
}

impl CoreEvolutionAdapter {
    pub fn new(core_evo: Arc<Mutex<CoreEvolution>>) -> Self {
        Self { inner: core_evo }
    }
}

#[async_trait]
impl CoreEvolutionOps for CoreEvolutionAdapter {
    async fn request_capability(
        &self,
        capability_id: &str,
        description: &str,
        provider_kind_str: &str,
    ) -> Result<Value> {
        let provider_kind =
            blockcell_core::evolution_boundary::parse_evolvable_provider_kind(provider_kind_str)?;

        let core_evo = self.inner.lock().await;
        let evolution_id = core_evo
            .request_capability(capability_id, description, provider_kind)
            .await?;

        Ok(json!({
            "status": "requested",
            "evolution_id": evolution_id,
            "capability_id": capability_id,
            "description": description,
            "note": "Capability evolution has been queued. The system will generate, compile, validate, and load the new capability."
        }))
    }

    async fn list_records_json(&self) -> Result<Value> {
        let core_evo = self.inner.lock().await;
        let records = core_evo.list_records()?;
        let items: Vec<Value> = records
            .iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "capability_id": r.capability_id,
                    "description": r.description,
                    "status": format!("{:?}", r.status),
                    "attempt": r.attempt,
                    "created_at": r.created_at,
                    "updated_at": r.updated_at,
                })
            })
            .collect();
        Ok(json!(items))
    }

    async fn get_record_json(&self, evolution_id: &str) -> Result<Value> {
        let core_evo = self.inner.lock().await;
        let record = core_evo.load_record(evolution_id)?;
        Ok(json!({
            "id": record.id,
            "capability_id": record.capability_id,
            "description": record.description,
            "status": format!("{:?}", record.status),
            "provider_kind": format!("{:?}", record.provider_kind),
            "attempt": record.attempt,
            "source_code": record.source_code,
            "artifact_path": record.artifact_path,
            "compile_output": record.compile_output,
            "feedback_history": record.feedback_history.iter().map(|f| {
                json!({
                    "attempt": f.attempt,
                    "stage": f.stage,
                    "feedback": f.feedback,
                })
            }).collect::<Vec<Value>>(),
            "created_at": record.created_at,
            "updated_at": record.updated_at,
        }))
    }

    async fn unblock_capability(&self, capability_id: &str) -> Result<Value> {
        let core_evo = self.inner.lock().await;
        let unblocked = core_evo.unblock_capability(capability_id)?;
        Ok(json!({
            "capability_id": capability_id,
            "unblocked": unblocked,
            "message": if unblocked > 0 {
                format!("Capability '{}' has been unblocked ({} records). It can now be auto-triggered again.", capability_id, unblocked)
            } else {
                format!("Capability '{}' was not blocked.", capability_id)
            }
        }))
    }
}

/// Trait for notifying the evolution worker to wake up and process pending workflows.
///
/// This trait breaks the circular dependency: `agent` crate defines it,
/// `scheduler` crate's `EvolutionWorker` implements it, and the `bin` crate
/// passes `Arc<EvolutionWorker>` as `Arc<dyn EvolutionNotifier + Send + Sync>`.
pub trait EvolutionNotifier: Send + Sync {
    /// Wake up the worker — lightweight, non-blocking call.
    fn notify(&self);
}

/// Adapter: bridges `EvolutionWorkflowStore` (storage crate) → `EvolutionWorkflowStoreOps` (tools crate)
pub struct EvolutionWorkflowStoreAdapter {
    inner: EvolutionWorkflowStore,
}

impl EvolutionWorkflowStoreAdapter {
    pub fn new(store: EvolutionWorkflowStore) -> Self {
        Self { inner: store }
    }
}

impl EvolutionWorkflowStoreOps for EvolutionWorkflowStoreAdapter {
    fn list_workflows_json(&self, status_filter: Option<&str>) -> Result<Value> {
        let workflows = self.inner.list_workflows(status_filter)?;
        serde_json::to_value(&workflows).map_err(Error::from)
    }

    fn get_workflow_json(&self, workflow_id: &str) -> Result<Value> {
        let workflow = self.inner.get_workflow(workflow_id)?;
        match workflow {
            Some(w) => Ok(serde_json::to_value(&w).unwrap_or(json!(null))),
            None => Ok(json!({"error": "workflow not found", "id": workflow_id})),
        }
    }

    fn get_workflow_steps_json(&self, workflow_id: &str) -> Result<Value> {
        let steps = self.inner.get_steps(workflow_id)?;
        serde_json::to_value(&steps).map_err(Error::from)
    }

    fn cancel_workflow(&self, workflow_id: &str) -> Result<Value> {
        self.inner.cancel_workflow(workflow_id)?;
        Ok(json!({"workflow_id": workflow_id, "status": "Cancelled"}))
    }

    fn retry_workflow(&self, workflow_id: &str) -> Result<Value> {
        self.inner.retry_workflow(workflow_id)?;
        Ok(json!({"workflow_id": workflow_id, "status": "Requested"}))
    }

    fn unblock_capability(&self, capability_id: &str) -> Result<Value> {
        let count = self.inner.unblock_capability(capability_id)?;
        Ok(json!({
            "capability_id": capability_id,
            "unblocked": count,
            "message": if count > 0 {
                format!("Capability '{}' has been unblocked ({} workflows).", capability_id, count)
            } else {
                format!("Capability '{}' was not blocked.", capability_id)
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::{
        CapabilityDescriptor, CapabilityStatus, CapabilityType, PrivilegeLevel, ProviderKind,
    };
    use blockcell_skills::CapabilityExecutor;
    use std::time::Duration;

    struct SlowExecutor;

    #[async_trait]
    impl CapabilityExecutor for SlowExecutor {
        async fn execute(&self, input: Value) -> Result<Value> {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(input)
        }

        async fn health_check(&self) -> Result<bool> {
            Ok(true)
        }

        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn capability_adapter_does_not_block_registry_reads_during_execution() {
        let temp = tempfile::tempdir().unwrap();
        let registry = blockcell_skills::new_registry_handle(temp.path().to_path_buf());
        let descriptor = CapabilityDescriptor::new(
            "test.slow_adapter",
            "Slow adapter",
            "Slow adapter concurrency test",
            CapabilityType::Internal,
            ProviderKind::BuiltIn,
        )
        .with_privilege(PrivilegeLevel::Limited)
        .with_status(CapabilityStatus::Available);
        registry
            .lock()
            .await
            .register_with_executor(descriptor, Arc::new(SlowExecutor));

        let handle = capability_registry_ops_handle(CapabilityRegistryAdapter::new(registry));
        let execution_handle = handle.clone();
        let execution = tokio::spawn(async move {
            execution_handle
                .execute_capability("test.slow_adapter", json!({}))
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let listed = tokio::time::timeout(Duration::from_millis(200), handle.list_all_json()).await;
        assert!(
            listed.is_ok(),
            "outer registry handle blocked list operation"
        );
        execution.await.unwrap().unwrap();
    }
}
