use crate::factory::create_provider_with_tool_mode;
use crate::{
    create_evolution_provider, create_main_provider, create_provider, Provider, ProviderPool,
};
use blockcell_core::config::{RoutingStrategy, ToolCallMode};
use blockcell_core::types::{ChatMessage, StreamChunk};
use blockcell_core::Config;
use serde_json::{json, Value};

fn config(provider: &str, api_type: &str, url: &str, entries: Value) -> Config {
    let mut config = Config::default();
    config.providers.insert(
        provider.into(),
        serde_json::from_value(json!({
            "apiKey": "test", "apiBase": url, "apiType": api_type, "proxy": "",
        }))
        .unwrap(),
    );
    config.agents.defaults.model = "primary".into();
    config.agents.defaults.provider = Some(provider.into());
    config.agents.defaults.max_tokens = 8192;
    config.agents.defaults.model_pool = serde_json::from_value(entries).unwrap();
    config
}

fn response(api_type: &str, stream: bool) -> String {
    if stream {
        match api_type {
            "openai" => {
                return format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    json!({
                        "choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}]
                    })
                )
            }
            "anthropic" => {
                return include_str!("../../../tests/fixtures/anthropic-native-usage.sse").into()
            }
            "gemini" => {
                return format!(
                    "data: {}\n\n",
                    json!({
                        "candidates": [{"content": {"parts": [{"text": "ok"}]}, "finishReason": "STOP"}]
                    })
                )
            }
            "ollama" => {
                return format!(
                    "{}\n",
                    json!({"message": {"role": "assistant", "content": "ok"}, "done": true})
                )
            }
            _ => {} // Responses uses the Provider trait's nonstreaming fallback.
        }
    }
    match api_type {
        "anthropic" => json!({"id": "msg_test", "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn"}),
        "gemini" => json!({"candidates": [{"content": {"parts": [{"text": "ok"}]}, "finishReason": "STOP"}]}),
        "ollama" => json!({"message": {"role": "assistant", "content": "ok"}, "done": true}),
        "openai_responses" => json!({"output": [{"type": "message", "content": [{"type": "output_text", "text": "ok"}]}], "status": "completed"}),
        _ => json!({"choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}]}),
    }.to_string()
}

async fn call(provider: &dyn Provider, stream: bool) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let messages = [ChatMessage::user("hi")];
        if stream {
            let mut rx = provider.chat_stream(&messages, &[]).await.unwrap();
            while let Some(chunk) = rx.recv().await {
                match chunk {
                    StreamChunk::Done { response } => {
                        assert_eq!(response.content.as_deref(), Some("ok"));
                        return;
                    }
                    StreamChunk::Error { message } => panic!("{message}"),
                    _ => {}
                }
            }
            panic!("stream ended without a final response");
        } else {
            assert_eq!(
                provider
                    .chat(&messages, &[])
                    .await
                    .unwrap()
                    .content
                    .as_deref(),
                Some("ok")
            );
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn r4_model_tokens_reach_every_protocol_in_chat_and_stream() {
    let cases = [
        ("openai", "openai", "/max_tokens"),
        ("anthropic", "anthropic", "/max_tokens"),
        ("gemini", "gemini", "/generationConfig/maxOutputTokens"),
        ("ollama", "ollama", "/options/num_predict"),
        ("custom-openai", "openai", "/max_tokens"),
        ("custom-anthropic", "anthropic", "/max_tokens"),
        (
            "custom-gemini",
            "gemini",
            "/generationConfig/maxOutputTokens",
        ),
        ("custom-ollama", "ollama", "/options/num_predict"),
        ("custom-responses", "openai_responses", "/max_output_tokens"),
    ];
    for (name, api_type, pointer) in cases {
        for stream in [false, true] {
            let (url, request) = crate::test_http::serve_capture(response(api_type, stream)).await;
            let config = config(
                name,
                api_type,
                &url,
                json!([
                    {"model": "primary", "provider": name, "maxTokens": 1234}
                ]),
            );
            let provider = create_main_provider(&config).unwrap();
            call(provider.as_ref(), stream).await;
            assert_eq!(
                request.await.unwrap().pointer(pointer),
                Some(&json!(1234)),
                "{name}, stream={stream}"
            );
        }
    }
}

#[tokio::test]
async fn r4_model_tokens_survive_save_reload_and_pool_build_for_two_models() {
    let mut servers = Vec::new();
    // The two entries share a server and endpoint; each request must still use its own limit.
    for (model, limit) in [("primary", 1111), ("secondary", 2222)] {
        let (url, request) = crate::test_http::serve_capture(response("openai", false)).await;
        let config = config(
            "service",
            "openai",
            &url,
            json!([
                {"model": "primary", "provider": "service", "maxTokens": 1111},
                {"model": "secondary", "provider": "service", "maxTokens": 2222},
            ]),
        );
        let dir = std::env::temp_dir().join(format!(
            "blockcell-r4-tokens-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json5");
        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let serialized = serde_json::to_value(&loaded).unwrap();
        assert_eq!(
            serialized["agents"]["defaults"]["modelPool"][0]["maxTokens"],
            1111
        );
        assert_eq!(
            serialized["agents"]["defaults"]["modelPool"][1]["maxTokens"],
            2222
        );
        let pool = ProviderPool::from_config(&loaded).unwrap();
        let (_, provider) = pool.acquire_by_model(model).unwrap();
        call(provider.as_ref(), false).await;
        servers.push((request, model, limit));
    }
    for (request, model, limit) in servers {
        let request = request.await.unwrap();
        assert_eq!(request["model"], model);
        assert_eq!(request["max_tokens"], limit);
    }
}

#[tokio::test]
async fn r4_model_tokens_apply_to_main_evolution_and_direct_factories() {
    for route in ["main", "evolution", "direct", "tool-mode"] {
        let (url, request) = crate::test_http::serve_capture(response("openai", false)).await;
        let mut config = config(
            "service",
            "openai",
            &url,
            json!([
                {"model": "primary", "provider": "service", "maxTokens": 3333},
                {"model": "secondary", "provider": "service", "maxTokens": 4444},
            ]),
        );
        config.agents.defaults.evolution_model = Some("secondary".into());
        let provider = match route {
            "main" => create_main_provider(&config),
            "evolution" => create_evolution_provider(&config),
            "direct" => create_provider(&config, "secondary", Some("service")),
            _ => create_provider_with_tool_mode(
                &config,
                "secondary",
                Some("service"),
                Some(ToolCallMode::None),
                None,
            ),
        }
        .unwrap();
        call(provider.as_ref(), false).await;
        assert_eq!(
            request.await.unwrap()["max_tokens"],
            if route == "main" { 3333 } else { 4444 },
            "{route}"
        );
    }
}

#[tokio::test]
async fn r4_model_tokens_match_effective_provider_and_use_first_duplicate_only_for_direct_calls() {
    for explicit in [Some("openai"), None] {
        let (url, request) = crate::test_http::serve_capture(response("openai", false)).await;
        let config = config(
            "openai",
            "openai",
            &url,
            json!([
                {"model": "gpt-test", "provider": "other", "maxTokens": 9000},
                {"model": "gpt-test", "provider": "openai", "maxTokens": 5000},
                {"model": "gpt-test", "provider": "openai", "maxTokens": 6000},
            ]),
        );
        let provider = create_provider(&config, "gpt-test", explicit).unwrap();
        call(provider.as_ref(), false).await;
        assert_eq!(request.await.unwrap()["max_tokens"], 5000);
    }
    // A first duplicate with no override selects the default; it must not search
    // ahead for the next duplicate that happens to contain a limit.
    let (url, request) = crate::test_http::serve_capture(response("openai", false)).await;
    let config = config(
        "openai",
        "openai",
        &url,
        json!([
            {"model": "gpt-test", "provider": "", "maxTokens": null},
            {"model": "gpt-test", "provider": "openai", "maxTokens": 6000},
        ]),
    );
    let provider = create_provider(&config, "gpt-test", None).unwrap();
    call(provider.as_ref(), false).await;
    assert_eq!(request.await.unwrap()["max_tokens"], 8192);
    for (idx, expected) in [(0, 5000), (1, 8192), (2, 6000)] {
        let (url, request) = crate::test_http::serve_capture(response("openai", false)).await;
        let config = self::config(
            "service",
            "openai",
            &url,
            json!([
                {"model": "primary", "provider": "service", "maxTokens": 5000},
                {"model": "primary", "provider": "service"},
                {"model": "primary", "provider": "service", "maxTokens": 6000},
            ]),
        );
        let pool = ProviderPool::from_config(&config).unwrap();
        let excluded: Vec<usize> = (0..3).filter(|candidate| *candidate != idx).collect();
        let (selected, provider) = pool
            .acquire_with_strategy_excluding(
                RoutingStrategy::Manual,
                &Default::default(),
                &excluded,
            )
            .unwrap();
        assert_eq!(selected, idx);
        call(provider.as_ref(), false).await;
        assert_eq!(
            request.await.unwrap()["max_tokens"],
            expected,
            "pool index {idx}"
        );
    }
}

#[tokio::test]
async fn r4_model_tokens_fall_back_to_resolved_agent_then_global_default() {
    for (agent, expected) in [("default", 8192), ("ops", 7777)] {
        for pool_entry in [false, true] {
            let (url, request) = crate::test_http::serve_capture(response("openai", false)).await;
            let entries = if pool_entry {
                json!([{"model": "primary", "provider": "service"}])
            } else {
                json!([])
            };
            let mut config = config("service", "openai", &url, entries);
            config.agents.list =
                serde_json::from_value(json!([{"id": "ops", "maxTokens": 7777}])).unwrap();
            let resolved = config.config_for_agent(agent).unwrap();
            let pool = ProviderPool::from_config(&resolved).unwrap();
            let (_, provider) = pool.acquire().unwrap();
            call(provider.as_ref(), false).await;
            assert_eq!(request.await.unwrap()["max_tokens"], expected);
        }
    }
}

#[test]
fn r4_model_tokens_reject_zero_when_constructed_without_config_validation() {
    let config = config(
        "service",
        "openai",
        "http://127.0.0.1:1",
        json!([
            {"model": "primary", "provider": "service", "maxTokens": 0}
        ]),
    );
    let result = create_main_provider(&config);
    assert!(
        result.is_err(),
        "zero model limit must fail before any request"
    );
    assert!(result.err().unwrap().to_string().contains("maxTokens"));
    assert!(ProviderPool::from_config(&config).is_err());
}
