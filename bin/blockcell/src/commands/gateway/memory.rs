use super::*;
// ---------------------------------------------------------------------------
// P1: Memory management endpoints
// ---------------------------------------------------------------------------

const MEMORY_GATEWAY_CHANNEL: &str = "webui";

fn memory_gateway_chat_id(agent_id: &str) -> String {
    format!("memory-manager:{}", agent_id)
}

async fn execute_memory_create_via_tool(
    state: &GatewayState,
    agent_id: &str,
    store: MemoryStoreHandle,
    req: serde_json::Value,
) -> blockcell_core::Result<serde_json::Value> {
    let agent_paths = state.paths.for_agent(agent_id);
    let chat_id = memory_gateway_chat_id(agent_id);
    let session_key = blockcell_core::build_session_key(MEMORY_GATEWAY_CHANNEL, &chat_id);

    let ctx = blockcell_tools::ToolContext {
        workspace: agent_paths.workspace(),
        builtin_skills_dir: Some(state.paths.builtin_skills_dir()),
        active_skill_dir: None,
        session_key,
        channel: MEMORY_GATEWAY_CHANNEL.to_string(),
        account_id: None,
        sender_id: None,
        chat_id,
        config: state.config.clone(),
        permissions: blockcell_core::types::PermissionSet::new(),
        task_manager: Some(Arc::new(state.task_manager.clone())),
        memory_store: Some(store),
        outbound_tx: None,
        spawn_handle: None,
        capability_registry: None,
        core_evolution: None,
        event_emitter: None,
        channel_contacts_file: Some(agent_paths.channel_contacts_file()),
        response_cache: None,
    };

    state.tool_registry.execute("memory_upsert", ctx, req).await
}

#[derive(Deserialize)]
pub(super) struct MemoryQueryParams {
    q: Option<String>,
    scope: Option<String>,
    #[serde(rename = "type")]
    mem_type: Option<String>,
    limit: Option<usize>,
    agent: Option<String>,
}

/// GET /v1/memory — search/list memories
pub(super) async fn handle_memory_list(
    State(state): State<GatewayState>,
    Query(params): Query<MemoryQueryParams>,
) -> impl IntoResponse {
    let (_, store) = match memory_store_for_agent(&state, params.agent.as_deref()) {
        Ok(value) => value,
        Err(err) => return Json(serde_json::json!({ "error": err })),
    };

    let query = serde_json::json!({
        "query": params.q.unwrap_or_default(),
        "scope": params.scope,
        "type": params.mem_type,
        "top_k": params.limit.unwrap_or(20),
    });

    match store.query_json(query) {
        Ok(result) => Json(result),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// POST /v1/memory — create/update a memory
pub(super) async fn handle_memory_create(
    State(state): State<GatewayState>,
    Query(agent): Query<AgentScopedQuery>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let (agent_id, store) = match memory_store_for_agent(&state, agent.agent.as_deref()) {
        Ok(value) => value,
        Err(err) => return Json(serde_json::json!({ "error": err })),
    };

    match execute_memory_create_via_tool(&state, &agent_id, store, req).await {
        Ok(result) => Json(result),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// DELETE /v1/memory/:id — delete a memory
pub(super) async fn handle_memory_delete(
    State(state): State<GatewayState>,
    AxumPath(id): AxumPath<String>,
    Query(agent): Query<AgentScopedQuery>,
) -> impl IntoResponse {
    let (_, store) = match memory_store_for_agent(&state, agent.agent.as_deref()) {
        Ok(value) => value,
        Err(err) => return Json(serde_json::json!({ "error": err })),
    };

    match store.soft_delete(&id) {
        Ok(_) => Json(serde_json::json!({ "status": "deleted", "id": id })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

/// GET /v1/memory/stats — memory statistics
pub(super) async fn handle_memory_stats(
    State(state): State<GatewayState>,
    Query(agent): Query<AgentScopedQuery>,
) -> impl IntoResponse {
    let (_, store) = match memory_store_for_agent(&state, agent.agent.as_deref()) {
        Ok(value) => value,
        Err(err) => return Json(serde_json::json!({ "error": err })),
    };

    match store.stats_json() {
        Ok(result) => Json(result),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use blockcell_agent::TaskManager;
    use blockcell_channels::ChannelManager;
    use blockcell_core::{build_session_key, Paths};
    use blockcell_skills::{EvolutionService, EvolutionServiceConfig};
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::sync::{broadcast, mpsc, Mutex};

    struct CaptureMemoryStore {
        last_upsert: StdMutex<Option<Value>>,
        upsert_calls: StdMutex<usize>,
    }

    impl CaptureMemoryStore {
        fn new() -> Self {
            Self {
                last_upsert: StdMutex::new(None),
                upsert_calls: StdMutex::new(0),
            }
        }

        fn last_upsert(&self) -> Option<Value> {
            self.last_upsert
                .lock()
                .expect("last_upsert lock")
                .clone()
        }

        fn upsert_calls(&self) -> usize {
            *self.upsert_calls.lock().expect("upsert_calls lock")
        }
    }

    impl blockcell_tools::MemoryStoreOps for CaptureMemoryStore {
        fn upsert_json(&self, params_json: Value) -> blockcell_core::Result<Value> {
            *self.last_upsert.lock().expect("last_upsert lock") = Some(params_json.clone());
            *self.upsert_calls.lock().expect("upsert_calls lock") += 1;
            Ok(json!({
                "id": "mem-1",
                "scope": params_json.get("scope").cloned().unwrap_or(Value::Null),
                "type": params_json.get("type").cloned().unwrap_or(Value::Null),
                "title": params_json.get("title").cloned().unwrap_or(Value::Null),
                "content": params_json.get("content").cloned().unwrap_or(Value::Null),
                "summary": params_json.get("summary").cloned().unwrap_or(Value::Null),
                "tags": [],
                "source": params_json.get("source").cloned().unwrap_or(Value::Null),
                "channel": params_json.get("channel").cloned().unwrap_or(Value::Null),
                "session_key": params_json.get("session_key").cloned().unwrap_or(Value::Null),
                "importance": params_json.get("importance").cloned().unwrap_or(json!(0.5)),
                "created_at": "2026-03-25T00:00:00Z",
                "updated_at": "2026-03-25T00:00:00Z",
                "last_accessed_at": null,
                "access_count": 0,
                "expires_at": params_json.get("expires_at").cloned().unwrap_or(Value::Null),
                "deleted_at": null,
                "dedup_key": params_json.get("dedup_key").cloned().unwrap_or(Value::Null),
            }))
        }

        fn query_json(&self, _params_json: Value) -> blockcell_core::Result<Value> {
            Ok(json!([]))
        }

        fn soft_delete(&self, _id: &str) -> blockcell_core::Result<bool> {
            Ok(false)
        }

        fn batch_soft_delete_json(&self, _params_json: Value) -> blockcell_core::Result<usize> {
            Ok(0)
        }

        fn restore(&self, _id: &str) -> blockcell_core::Result<bool> {
            Ok(false)
        }

        fn stats_json(&self) -> blockcell_core::Result<Value> {
            Ok(json!({}))
        }

        fn generate_brief(
            &self,
            _long_term_max: usize,
            _short_term_max: usize,
        ) -> blockcell_core::Result<String> {
            Ok(String::new())
        }

        fn generate_brief_for_query(
            &self,
            _query: &str,
            _max_items: usize,
        ) -> blockcell_core::Result<String> {
            Ok(String::new())
        }

        fn upsert_session_summary(
            &self,
            _session_key: &str,
            _summary: &str,
        ) -> blockcell_core::Result<()> {
            Ok(())
        }

        fn get_session_summary(
            &self,
            _session_key: &str,
        ) -> blockcell_core::Result<Option<String>> {
            Ok(None)
        }

        fn maintenance(&self, _recycle_days: i64) -> blockcell_core::Result<(usize, usize)> {
            Ok((0, 0))
        }
    }

    fn test_gateway_state(memory_store: blockcell_tools::MemoryStoreHandle) -> GatewayState {
        let config = Config::default();
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell-gateway-memory-tests"));
        let (inbound_tx, _inbound_rx) = mpsc::channel(4);
        let (ws_broadcast, _) = broadcast::channel(4);
        let channel_manager = Arc::new(ChannelManager::new(
            config.clone(),
            paths.clone(),
            inbound_tx.clone(),
        ));
        let evolution_service = Arc::new(Mutex::new(EvolutionService::new(
            paths.skills_dir(),
            EvolutionServiceConfig::default(),
        )));

        GatewayState {
            inbound_tx,
            task_manager: TaskManager::new(),
            config,
            paths,
            api_token: None,
            ws_broadcast,
            pending_confirms: Arc::new(Mutex::new(HashMap::new())),
            pending_channel_confirms: Arc::new(Mutex::new(HashMap::new())),
            memory_store: Some(memory_store.clone()),
            memory_stores: Arc::new(HashMap::from([("default".to_string(), memory_store)])),
            cron_services: Arc::new(HashMap::new()),
            tool_registry: Arc::new(ToolRegistry::with_defaults()),
            web_password: "test-password".to_string(),
            channel_manager,
            evolution_service,
        }
    }

    async fn response_json(response: impl IntoResponse) -> Value {
        let response = response.into_response();
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("read response body");
        serde_json::from_slice(&body).expect("response json")
    }

    #[tokio::test]
    async fn test_handle_memory_create_ignores_request_provenance_fields() {
        let store = Arc::new(CaptureMemoryStore::new());
        let state = test_gateway_state(store.clone());

        let body = response_json(
            handle_memory_create(
                State(state),
                Query(AgentScopedQuery::default()),
                Json(json!({
                    "title": "Pinned fact",
                    "content": "User prefers concise answers",
                    "scope": "long_term",
                    "type": "fact",
                    "source": "user",
                    "channel": "ghost",
                    "session_key": "ghost:hijack"
                })),
            )
            .await,
        )
        .await;

        assert_eq!(body["status"], json!("saved"));

        let captured = store.last_upsert().expect("captured upsert");
        assert_eq!(captured["source"], json!("tool"));
        assert_eq!(captured["channel"], json!(MEMORY_GATEWAY_CHANNEL));
        assert_eq!(
            captured["session_key"],
            json!(build_session_key(
                MEMORY_GATEWAY_CHANNEL,
                &memory_gateway_chat_id("default")
            ))
        );
    }

    #[tokio::test]
    async fn test_handle_memory_create_rejects_missing_content_before_store() {
        let store = Arc::new(CaptureMemoryStore::new());
        let state = test_gateway_state(store.clone());

        let body = response_json(
            handle_memory_create(
                State(state),
                Query(AgentScopedQuery::default()),
                Json(json!({
                    "scope": "long_term",
                    "type": "fact"
                })),
            )
            .await,
        )
        .await;

        assert!(body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("Missing required parameter: content"));
        assert_eq!(store.upsert_calls(), 0);
    }
}
