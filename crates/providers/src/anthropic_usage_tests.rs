use crate::{AnthropicProvider, Provider};
use blockcell_core::types::{ChatMessage, LLMResponse, StreamChunk};
use serde_json::json;

const NATIVE_USAGE: &str = include_str!("../../../tests/fixtures/anthropic-native-usage.sse");

async fn stream_response(body: String) -> LLMResponse {
    let (url, server) = crate::test_http::serve(body).await;
    let provider = AnthropicProvider::new_with_proxy(
        "test",
        Some(&url),
        "claude-test",
        100,
        0.0,
        Some(""),
        None,
        &[],
    )
    .unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut rx = provider
            .chat_stream(&[ChatMessage::user("hi")], &[])
            .await
            .unwrap();
        while let Some(chunk) = rx.recv().await {
            match chunk {
                StreamChunk::Done { response } => return response,
                StreamChunk::Error { message } => panic!("{message}"),
                _ => {}
            }
        }
        panic!("stream ended without a final response")
    })
    .await
    .unwrap();
    server.await.unwrap();
    response
}

#[tokio::test]
async fn r4_anthropic_native_usage_survives_partial_deltas_and_matches_nonstreaming() {
    let streamed = stream_response(NATIVE_USAGE.into()).await;
    assert_eq!(streamed.content.as_deref(), Some("ok"));
    assert_eq!(
        streamed.usage,
        json!({
            "prompt_tokens": 10000,
            "completion_tokens": 10,
            "cache_creation_input_tokens": 128,
            "cache_read_input_tokens": 2000,
        })
    );

    let (url, server) = crate::test_http::serve(
        json!({
            "id": "msg_usage", "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10000, "output_tokens": 10,
                      "cache_creation_input_tokens": 128, "cache_read_input_tokens": 2000},
        })
        .to_string(),
    )
    .await;
    let provider = AnthropicProvider::new_with_proxy(
        "test",
        Some(&url),
        "claude-test",
        100,
        0.0,
        Some(""),
        None,
        &[],
    )
    .unwrap();
    let response = provider
        .chat(&[ChatMessage::user("hi")], &[])
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(response.usage, streamed.usage);
}

#[tokio::test]
async fn r4_anthropic_usage_keeps_legacy_start_and_applies_explicit_zero_fields() {
    let body = [
        json!({"type": "message_start", "usage": {"input_tokens": 10, "output_tokens": 1,
            "cache_creation_input_tokens": 8, "cache_read_input_tokens": 6}}),
        json!({"type": "message_delta", "usage": {"output_tokens": 3, "cache_creation_input_tokens": 0}}),
        json!({"type": "message_delta", "usage": {"output_tokens": 5}}),
        json!({"type": "message_stop"}),
    ].iter().map(|event| format!("data: {event}\n\n")).collect();
    let response = stream_response(body).await;
    assert_eq!(
        response.usage,
        json!({"prompt_tokens": 10, "completion_tokens": 5,
        "cache_creation_input_tokens": 0, "cache_read_input_tokens": 6})
    );
}

#[tokio::test]
async fn r4_anthropic_standard_start_takes_precedence_over_legacy_usage() {
    let body = [
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 100, "output_tokens": 1}},
            "usage": {"input_tokens": 999, "output_tokens": 999}}),
        json!({"type": "message_stop"}),
    ].iter().map(|event| format!("data: {event}\n\n")).collect();
    let response = stream_response(body).await;
    assert_eq!(response.usage["prompt_tokens"], 100);
    assert_eq!(response.usage["completion_tokens"], 1);
}
