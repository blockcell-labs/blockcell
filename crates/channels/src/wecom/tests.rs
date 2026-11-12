use super::*;

#[test]
fn test_split_message_short() {
    let chunks = split_message("hello world", 2048);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0], "hello world");
}

#[test]
fn test_split_message_long() {
    let line = "a".repeat(100);
    let text = (0..25).map(|_| line.clone()).collect::<Vec<_>>().join("\n");
    let chunks = split_message(&text, 2048);
    assert!(chunks.len() > 1);
    for chunk in &chunks {
        assert!(chunk.chars().count() <= 2048);
    }
}

#[test]
fn test_split_message_chinese() {
    // Each Chinese char is 3 bytes; 1000 chars = 3000 bytes
    let text = "中".repeat(3000);
    let chunks = split_message(&text, 2048);
    assert!(chunks.len() > 1);
    for chunk in &chunks {
        assert!(
            chunk.chars().count() <= 2048,
            "chunk too long: {} chars",
            chunk.chars().count()
        );
    }
}

#[test]
fn test_token_response_deserialize() {
    let json = r#"{"errcode":0,"errmsg":"ok","access_token":"test_token","expires_in":7200}"#;
    let resp: TokenResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.errcode, 0);
    assert_eq!(resp.access_token.as_deref(), Some("test_token"));
}

#[test]
fn test_wecom_response_error() {
    let json = r#"{"errcode":40014,"errmsg":"invalid access_token"}"#;
    let resp: WeComResponse = serde_json::from_str(json).unwrap();
    assert_eq!(resp.errcode, 40014);
}

#[test]
fn test_sha1_known_value() {
    // SHA1("abc") = a9993e364706816aba3e25717850c26c9cd0d89d
    let result = sha1_hex(b"abc");
    assert_eq!(result, "a9993e364706816aba3e25717850c26c9cd0d89d");
}

#[test]
fn test_resolve_wecom_webhook_config_matches_signed_account() {
    let mut config = Config::default();
    config.channels.wecom.enabled = true;
    config.channels.wecom.accounts.insert(
        "default".to_string(),
        blockcell_core::config::WeComAccountConfig {
            enabled: true,
            mode: "webhook".to_string(),
            corp_id: "corp-a".to_string(),
            corp_secret: "secret-a".to_string(),
            agent_id: 1,
            bot_id: String::new(),
            bot_secret: String::new(),
            callback_token: "token-a".to_string(),
            encoding_aes_key: "aes-a".to_string(),
            allow_from: vec![],
            poll_interval_secs: 30,
            ws_url: String::new(),
            ping_interval_secs: 30,
        },
    );
    config.channels.wecom.accounts.insert(
        "ops".to_string(),
        blockcell_core::config::WeComAccountConfig {
            enabled: true,
            mode: "webhook".to_string(),
            corp_id: "corp-b".to_string(),
            corp_secret: "secret-b".to_string(),
            agent_id: 2,
            bot_id: String::new(),
            bot_secret: String::new(),
            callback_token: "token-b".to_string(),
            encoding_aes_key: "aes-b".to_string(),
            allow_from: vec![],
            poll_interval_secs: 30,
            ws_url: String::new(),
            ping_interval_secs: 30,
        },
    );

    let timestamp = "1710000000";
    let nonce = "nonce-1";
    let encrypt = "ciphertext";
    let mut parts = ["token-b", timestamp, nonce, encrypt];
    parts.sort_unstable();
    let signature = sha1_hex(parts.join("").as_bytes());
    let query = std::collections::HashMap::from([
        ("timestamp".to_string(), timestamp.to_string()),
        ("nonce".to_string(), nonce.to_string()),
        ("msg_signature".to_string(), signature),
    ]);
    let body = format!("<xml><Encrypt>{}</Encrypt></xml>", encrypt);

    let resolved = resolve_wecom_webhook_config(&config, "POST", &query, &body);
    assert_eq!(
        resolved.channels.wecom.default_account_id.as_deref(),
        Some("ops")
    );
    assert_eq!(resolved.channels.wecom.callback_token, "token-b");
}

#[test]
fn test_resolve_wecom_webhook_config_keeps_legacy_when_ambiguous() {
    let mut config = Config::default();
    config.channels.wecom.enabled = true;
    config.channels.wecom.corp_id = "legacy-corp".to_string();
    config.channels.wecom.accounts.insert(
        "default".to_string(),
        blockcell_core::config::WeComAccountConfig {
            enabled: true,
            mode: "webhook".to_string(),
            corp_id: "corp-a".to_string(),
            corp_secret: "secret-a".to_string(),
            agent_id: 1,
            bot_id: String::new(),
            bot_secret: String::new(),
            callback_token: "token-a".to_string(),
            encoding_aes_key: "aes-a".to_string(),
            allow_from: vec![],
            poll_interval_secs: 30,
            ws_url: String::new(),
            ping_interval_secs: 30,
        },
    );
    config.channels.wecom.accounts.insert(
        "ops".to_string(),
        blockcell_core::config::WeComAccountConfig {
            enabled: true,
            mode: "webhook".to_string(),
            corp_id: "corp-b".to_string(),
            corp_secret: "secret-b".to_string(),
            agent_id: 2,
            bot_id: String::new(),
            bot_secret: String::new(),
            callback_token: "token-b".to_string(),
            encoding_aes_key: "aes-b".to_string(),
            allow_from: vec![],
            poll_interval_secs: 30,
            ws_url: String::new(),
            ping_interval_secs: 30,
        },
    );

    let resolved = resolve_wecom_webhook_config(
        &config,
        "POST",
        &std::collections::HashMap::new(),
        "<xml></xml>",
    );
    assert_eq!(resolved.channels.wecom.corp_id, "legacy-corp");
    assert_eq!(resolved.channels.wecom.default_account_id, None);
}

#[test]
fn test_verify_signature() {
    // WeCom signature: SHA1(sort(token, timestamp, nonce))
    // token="test", timestamp="1409735669", nonce="xxxxxx"
    // sorted: ["1409735669", "test", "xxxxxx"] → "1409735669testxxxxxx"
    let token = "test";
    let timestamp = "1409735669";
    let nonce = "xxxxxx";
    let mut parts = [token, timestamp, nonce];
    parts.sort_unstable();
    let combined = parts.join("");
    let expected = sha1_hex(combined.as_bytes());
    assert!(WeComChannel::verify_signature(
        token, timestamp, nonce, &expected
    ));
}

#[test]
fn test_build_mixed_summary() {
    let mixed = LongConnMixed {
        items: vec![
            LongConnMixedItem {
                item_type: "text".to_string(),
                content: Some("你好".to_string()),
            },
            LongConnMixedItem {
                item_type: "image".to_string(),
                content: None,
            },
            LongConnMixedItem {
                item_type: "file".to_string(),
                content: None,
            },
        ],
    };
    assert_eq!(build_mixed_summary(&mixed), "你好 [图片] [文件]");
}

#[test]
fn test_decrypt_longconn_media_bytes_rejects_short_key() {
    let err = decrypt_longconn_media_bytes(b"abc", "short").unwrap_err();
    assert!(err.to_string().contains("aeskey"));
}

#[tokio::test]
async fn test_build_inbound_from_long_connection_text() {
    let config = Config::default();
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let ch = WeComChannel::new(config, tx);
    let body = serde_json::json!({
        "msgid": "m1",
        "aibotid": "bot1",
        "chatid": "chat1",
        "chattype": "single",
        "from": { "userid": "u1", "nickname": "U1" },
        "msgtype": "text",
        "text": { "content": "hello" }
    });
    let inbound = ch
        .build_inbound_from_long_connection(&body)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(inbound.sender_id, "u1");
    assert_eq!(inbound.chat_id, "chat1");
    assert_eq!(inbound.content, "hello");
    assert_eq!(inbound.metadata["mode"], "long_connection");
}

#[tokio::test]
async fn test_build_inbound_from_long_connection_allowlist() {
    let mut config = Config::default();
    config.channels.wecom.allow_from = vec!["allowed".to_string()];
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let ch = WeComChannel::new(config, tx);
    let body = serde_json::json!({
        "msgid": "m2",
        "aibotid": "bot1",
        "chatid": "chat1",
        "chattype": "single",
        "from": { "userid": "denied" },
        "msgtype": "text",
        "text": { "content": "hello" }
    });
    let inbound = ch.build_inbound_from_long_connection(&body).await.unwrap();
    assert!(inbound.is_none());
}

#[tokio::test]
async fn media_delivery_without_active_connection_fails() {
    let mut config = Config::default();
    config.channels.wecom.mode = "long_connection".into();
    config.channels.wecom.bot_id = "r5-no-active-connection".into();
    assert!(
        send_media_message(&config, "chat", "/missing.png", "caption")
            .await
            .is_err()
    );
    assert!(send_message(&config, "chat", "text").await.is_err());
}

#[tokio::test]
async fn media_delivery_preserves_caption_on_real_long_connection() {
    check_media_delivery(false, false).await;
}

#[tokio::test]
async fn media_delivery_server_rejection_is_not_success() {
    check_media_delivery(true, false).await;
}

#[tokio::test]
async fn media_delivery_failed_attachment_still_sends_text() {
    check_media_delivery(false, true).await;
}

async fn check_media_delivery(reject_media: bool, missing_file: bool) {
    use futures::{SinkExt, StreamExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = Config::default();
    config.channels.wecom.enabled = true;
    config.channels.wecom.mode = "long_connection".into();
    config.channels.wecom.bot_id = format!("r5-caption-{}", listener.local_addr().unwrap().port());
    config.channels.wecom.bot_secret = "test".into();
    config.channels.wecom.ws_url = format!("ws://{}", listener.local_addr().unwrap());
    let (ready_tx, ready_rx) = oneshot::channel();
    let (caption_tx, caption_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut ready_tx = Some(ready_tx);
        let mut caption_tx = Some(caption_tx);
        while let Some(Ok(WsMessage::Text(text))) = ws.next().await {
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            let command = value["cmd"].as_str().unwrap();
            if command == "aibot_subscribe" {
                ready_tx.take().unwrap().send(()).unwrap();
            }
            if command == "aibot_respond_msg" {
                caption_tx
                    .take()
                    .unwrap()
                    .send(
                        value["body"]["stream"]["content"]
                            .as_str()
                            .unwrap()
                            .to_string(),
                    )
                    .unwrap();
            }
            if command.starts_with("aibot_upload_") || command == "aibot_send_msg" {
                ws.send(WsMessage::Text(serde_json::json!({
                    "headers": value["headers"],
                    "errcode": if reject_media && command == "aibot_send_msg" { 40001 } else { 0 },
                    "errmsg": "test response",
                    "body": {"upload_id": "upload", "media_id": "media"}
                }).to_string())).await.unwrap();
            }
        }
    });
    let channel = WeComChannel::new(config.clone(), mpsc::channel(1).0);
    let connection = tokio::spawn(async move { channel.connect_and_run_long_connection().await });
    tokio::time::timeout(Duration::from_secs(2), ready_rx)
        .await
        .unwrap()
        .unwrap();
    let file = std::env::temp_dir().join(format!("{}.png", config.channels.wecom.bot_id));
    if !missing_file {
        std::fs::write(&file, b"test image bytes").unwrap();
    }
    let manager = crate::manager::ChannelManager::new(
        config.clone(),
        blockcell_core::Paths::with_base(std::env::temp_dir()),
        mpsc::channel(1).0,
    );
    let mut msg = blockcell_core::OutboundMessage::new("wecom", "chat", "中文 caption");
    msg.media.push(file.to_string_lossy().into_owned());
    let sent =
        tokio::time::timeout(Duration::from_secs(2), manager.dispatch_outbound_msg(&msg)).await;
    let caption = tokio::time::timeout(Duration::from_secs(2), caption_rx).await;
    connection.abort();
    server.abort();
    LONGCONN_REGISTRY
        .lock()
        .unwrap()
        .remove(&config.channels.wecom.bot_id);
    if !missing_file {
        std::fs::remove_file(file).unwrap();
    }
    assert_eq!(sent.unwrap().is_err(), reject_media || missing_file);
    assert_eq!(
        caption
            .expect("media success must also deliver the text")
            .unwrap(),
        "中文 caption"
    );
}
