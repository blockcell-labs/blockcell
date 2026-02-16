use async_trait::async_trait;
use blockcell_core::{Result, ProviderKind};
use blockcell_core::types::ChatMessage;
use blockcell_providers::Provider;
use blockcell_skills::{CapabilityRegistry, CoreEvolution, LLMProvider};
use blockcell_tools::{CapabilityRegistryOps, CoreEvolutionOps};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Bridge: wraps a `Provider` (providers crate) to implement `LLMProvider` (skills crate).
/// This allows CoreEvolution to use the same LLM as the rest of the agent.
pub struct ProviderLLMBridge {
    provider: Box<dyn Provider>,
}

impl ProviderLLMBridge {
    pub fn new(provider: Box<dyn Provider>) -> Self {
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
pub struct CapabilityRegistryAdapter {
    inner: Arc<Mutex<CapabilityRegistry>>,
}

impl CapabilityRegistryAdapter {
    pub fn new(registry: Arc<Mutex<CapabilityRegistry>>) -> Self {
        Self { inner: registry }
    }
}

#[async_trait]
impl CapabilityRegistryOps for CapabilityRegistryAdapter {
    async fn list_all_json(&self) -> Value {
        let registry = self.inner.lock().await;
        let all = registry.list_all();
        let caps: Vec<Value> = all.iter().map(|c| {
            json!({
                "id": c.id,
                "name": c.name,
                "description": c.description,
                "type": format!("{:?}", c.capability_type),
                "provider": format!("{:?}", c.provider_kind),
                "status": format!("{:?}", c.status),
                "version": c.version,
            })
        }).collect();
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
        let mut registry = self.inner.lock().await;
        registry.execute(id, input).await
    }

    async fn generate_brief(&self) -> String {
        let registry = self.inner.lock().await;
        registry.generate_brief()
    }

    async fn list_available_ids(&self) -> Vec<String> {
        let registry = self.inner.lock().await;
        registry.list_available().iter().map(|d| d.id.clone()).collect()
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
        let provider_kind = match provider_kind_str {
            "python" => ProviderKind::ExternalApi,
            "process" => ProviderKind::Process,
            "rust" | "dylib" => ProviderKind::DynamicLibrary,
            "rhai" => ProviderKind::RhaiScript,
            _ => ProviderKind::Process, // "script" / default → bash
        };

        let core_evo = self.inner.lock().await;
        let evolution_id = core_evo.request_capability(capability_id, description, provider_kind).await?;

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
        let items: Vec<Value> = records.iter().map(|r| {
            json!({
                "id": r.id,
                "capability_id": r.capability_id,
                "description": r.description,
                "status": format!("{:?}", r.status),
                "attempt": r.attempt,
                "created_at": r.created_at,
                "updated_at": r.updated_at,
            })
        }).collect();
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

    async fn run_pending_evolutions(&self) -> Result<usize> {
        let core_evo = self.inner.lock().await;
        core_evo.run_pending_evolutions().await
    }

    async fn unblock_capability(&self, capability_id: &str) -> Result<Value> {
        let core_evo = self.inner.lock().await;
        let unblocked = core_evo.unblock_capability(capability_id)?;
        Ok(json!({
            "capability_id": capability_id,
            "unblocked": unblocked,
            "message": if unblocked {
                format!("Capability '{}' has been unblocked. It can now be auto-triggered again.", capability_id)
            } else {
                format!("Capability '{}' was not blocked.", capability_id)
            }
        }))
    }
}
