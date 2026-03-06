use super::*;
// ---------------------------------------------------------------------------
// P1: Memory management endpoints
// ---------------------------------------------------------------------------

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
    let (_, store) = match memory_store_for_agent(&state, agent.agent.as_deref()) {
        Ok(value) => value,
        Err(err) => return Json(serde_json::json!({ "error": err })),
    };

    match store.upsert_json(req) {
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
