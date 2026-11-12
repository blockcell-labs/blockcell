use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn r4_anthropic_native_stream_counts_input_and_output_in_session_and_parent_budget() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0, "request ended before body");
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|value| value == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let content_len = headers.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                    }).unwrap();
                    if request.len() >= end + 4 + content_len { break; }
                }
                assert!(request.len() < 1024 * 1024, "request too large");
            }
            let body = include_str!("../../../../../tests/fixtures/anthropic-native-usage.sse");
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        }).await.expect("Anthropic fixture server timed out");
    });
    let provider = Arc::new(
        blockcell_providers::AnthropicProvider::new_with_proxy(
            "test",
            Some(&url),
            "claude-test",
            100,
            0.0,
            Some(""),
            None,
            &[],
        )
        .unwrap(),
    );
    let mut config = Config::default();
    config.agents.ghost.learning.enabled = false;
    let parent = Arc::new(BudgetTracker::new(&config.budget));
    let dir = tempfile::Builder::new()
        .prefix("blockcell-r4-usage-")
        .tempdir_in("/tmp")
        .unwrap();
    let mut runtime =
        test_runtime_with_provider_and_paths(Paths::with_base(dir.path().into()), provider, config);
    runtime.parent_budget = Some(parent.clone());
    let msg = test_main_session_inbound("cli", "anthropic-usage");
    let session_key = persist_session_key_for(&msg);
    let answer = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        runtime.process_message(msg),
    )
    .await
    .unwrap()
    .unwrap();
    server.await.unwrap();
    assert_eq!(answer, "ok");
    let session = runtime.budget_tracker_for_session(&session_key).snapshot();
    for snapshot in [session, parent.snapshot()] {
        assert_eq!(snapshot.input_tokens_used, 10000);
        assert_eq!(snapshot.output_tokens_used, 10);
        assert_eq!(snapshot.total_tokens_used, 10010);
    }
}
