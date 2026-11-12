use super::*;
// ---------------------------------------------------------------------------
// P2: Stream management endpoints
// ---------------------------------------------------------------------------

/// GET /v1/streams — list active stream subscriptions
pub(super) async fn handle_streams_list() -> impl IntoResponse {
    let data = blockcell_tools::stream_subscribe::list_streams().await;
    Json(data)
}

/// POST /v1/streams/restore — restore persisted streams in the running gateway.
pub(super) async fn handle_streams_restore(State(state): State<GatewayState>) -> impl IntoResponse {
    match blockcell_tools::stream_subscribe::restore_streams(&state.paths.workspace()).await {
        Ok(data) => (StatusCode::OK, Json(data)),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error.to_string()})),
        ),
    }
}

/// DELETE /v1/streams/:id — stop a stream in the running gateway.
pub(super) async fn handle_stream_delete(
    State(state): State<GatewayState>,
    AxumPath(stream_id): AxumPath<String>,
) -> impl IntoResponse {
    let workspaces: Vec<_> = state
        .config
        .known_agent_ids()
        .iter()
        .map(|agent_id| state.paths.for_agent(agent_id).workspace())
        .collect();
    match blockcell_tools::stream_subscribe::unsubscribe_stream_in_workspaces(
        &workspaces,
        &stream_id,
    )
    .await
    {
        Ok(data) => (StatusCode::OK, Json(data)),
        Err(error) => {
            let status = match error {
                blockcell_core::Error::Io(_) | blockcell_core::Error::Json(_) => {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
                _ => StatusCode::NOT_FOUND,
            };
            (
                status,
                Json(serde_json::json!({"error": error.to_string()})),
            )
        }
    }
}

#[derive(Deserialize)]
pub(super) struct StreamDataQuery {
    #[serde(default = "default_stream_limit")]
    limit: usize,
}

fn default_stream_limit() -> usize {
    50
}

/// GET /v1/streams/:id/data — get buffered data for a stream
pub(super) async fn handle_stream_data(
    AxumPath(stream_id): AxumPath<String>,
    Query(params): Query<StreamDataQuery>,
) -> impl IntoResponse {
    match blockcell_tools::stream_subscribe::get_stream_data(&stream_id, params.limit).await {
        Ok(data) => Json(data),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}
