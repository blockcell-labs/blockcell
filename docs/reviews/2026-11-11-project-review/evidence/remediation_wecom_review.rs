use blockcell_channels::wecom::{WeComChannel, send_message};
use blockcell_core::Config;
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, broadcast};
use tokio_tungstenite::tungstenite::Message;
use std::time::Duration;

#[tokio::test]
async fn rejected_text_must_not_report_success() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = Config::default();
    config.channels.wecom.enabled = true;
    config.channels.wecom.mode = "long_connection".into();
    config.channels.wecom.bot_id = format!("review-{}", listener.local_addr().unwrap().port());
    config.channels.wecom.bot_secret = "fixture".into();
    config.channels.wecom.ws_url = format!("ws://{}", listener.local_addr().unwrap());
    let (ready_tx, ready_rx) = oneshot::channel();
    let (rejected_tx, rejected_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut ready = Some(ready_tx);
        let mut rejected = Some(rejected_tx);
        while let Some(Ok(Message::Text(text))) = ws.next().await {
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            match value["cmd"].as_str().unwrap() {
                "aibot_subscribe" => { ready.take().unwrap().send(()).unwrap(); }
                "aibot_respond_msg" => {
                    ws.send(Message::Text(serde_json::json!({"headers":value["headers"],"errcode":40001,"errmsg":"rejected by fixture"}).to_string())).await.unwrap();
                    rejected.take().unwrap().send(()).unwrap();
                }
                _ => {}
            }
        }
    });
    let channel = Arc::new(WeComChannel::new(config.clone(), mpsc::channel(1).0));
    let (shutdown, rx) = broadcast::channel(1);
    let connection = tokio::spawn(channel.run_loop(rx));
    tokio::time::timeout(Duration::from_secs(2), ready_rx).await.unwrap().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), send_message(&config, "chat", "review text")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), rejected_rx).await.unwrap().unwrap();
    let _ = shutdown.send(());
    tokio::time::timeout(Duration::from_secs(2), connection).await.unwrap().unwrap();
    server.abort();
    assert!(result.is_err(), "WeCom text returned Ok despite service errcode 40001");
}
