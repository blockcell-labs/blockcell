use blockcell_core::{Config, Error, InboundMessage, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

const DINGTALK_API_BASE: &str = "https://oapi.dingtalk.com";
/// DingTalk single message character limit
const DINGTALK_MSG_LIMIT: usize = 4096;
/// Token refresh margin: refresh 5 minutes before expiry
#[allow(dead_code)]
const TOKEN_REFRESH_MARGIN_SECS: i64 = 300;

fn shared_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to build reqwest client")
}

/// Cached access token with expiry timestamp.
#[derive(Default)]
struct CachedToken {
    #[allow(dead_code)]
    token: String,
    #[allow(dead_code)]
    expires_at: i64,
}

impl CachedToken {
    #[allow(dead_code)]
    fn is_valid(&self) -> bool {
        !self.token.is_empty()
            && chrono::Utc::now().timestamp() < self.expires_at - TOKEN_REFRESH_MARGIN_SECS
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    errcode: i32,
    errmsg: String,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct DingTalkResponse {
    errcode: i32,
    errmsg: String,
}

/// DingTalk Stream SDK event envelope
#[derive(Debug, Deserialize)]
struct StreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    headers: Option<StreamHeaders>,
    #[serde(default)]
    data: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct StreamHeaders {
    #[serde(rename = "eventId")]
    #[serde(default)]
    event_id: Option<String>,
    #[serde(rename = "eventType")]
    #[serde(default)]
    #[allow(dead_code)]
    event_type: Option<String>,
}

/// DingTalk message event from Stream SDK
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct MessageEvent {
    #[serde(rename = "msgtype")]
    #[serde(default)]
    msg_type: Option<String>,
    #[serde(default)]
    text: Option<TextContent>,
    #[serde(rename = "conversationId")]
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(rename = "senderId")]
    #[serde(default)]
    sender_id: Option<String>,
    #[serde(rename = "senderNick")]
    #[serde(default)]
    sender_nick: Option<String>,
    #[serde(rename = "msgId")]
    #[serde(default)]
    msg_id: Option<String>,
    #[serde(rename = "isInAtList")]
    #[serde(default)]
    is_in_at_list: Option<bool>,
    #[serde(rename = "chatbotUserId")]
    #[serde(default)]
    chatbot_user_id: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct TextContent {
    content: String,
}

/// Stream SDK connection endpoint response
#[derive(Debug, Deserialize)]
struct StreamEndpointResponse {
    endpoint: String,
    ticket: String,
}

/// DingTalk channel supporting two modes:
/// - **Stream SDK** (preferred): real-time WebSocket push via DingTalk Stream SDK.
///   Requires `app_key` and `app_secret` in config.
/// - **Polling fallback**: HTTP polling when Stream SDK is unavailable.
pub struct DingTalkChannel {
    config: Config,
    client: Client,
    inbound_tx: mpsc::Sender<InboundMessage>,
    #[allow(dead_code)]
    token_cache: Arc<tokio::sync::Mutex<CachedToken>>,
}

impl DingTalkChannel {
    pub fn new(config: Config, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        Self {
            config,
            client: shared_client(),
            inbound_tx,
            token_cache: Arc::new(tokio::sync::Mutex::new(CachedToken::default())),
        }
    }

    fn is_allowed(&self, sender_id: &str) -> bool {
        let allow_from = &self.config.channels.dingtalk.allow_from;
        if allow_from.is_empty() {
            return true;
        }
        allow_from.iter().any(|a| a == sender_id)
    }

    #[allow(dead_code)]
    async fn get_access_token(&self) -> Result<String> {
        let mut cache = self.token_cache.lock().await;
        if cache.is_valid() {
            return Ok(cache.token.clone());
        }

        let app_key = &self.config.channels.dingtalk.app_key;
        let app_secret = &self.config.channels.dingtalk.app_secret;

        let resp = self
            .client
            .get(format!("{}/gettoken", DINGTALK_API_BASE))
            .query(&[("appkey", app_key.as_str()), ("appsecret", app_secret.as_str())])
            .send()
            .await
            .map_err(|e| Error::Channel(format!("DingTalk gettoken request failed: {}", e)))?;

        let body: TokenResponse = resp
            .json()
            .await
            .map_err(|e| Error::Channel(format!("Failed to parse DingTalk token response: {}", e)))?;

        if body.errcode != 0 {
            return Err(Error::Channel(format!(
                "DingTalk gettoken error {}: {}",
                body.errcode, body.errmsg
            )));
        }

        let token = body
            .access_token
            .ok_or_else(|| Error::Channel("No access_token in DingTalk response".to_string()))?;
        let expires_in = body.expires_in.unwrap_or(7200);

        cache.token = token.clone();
        cache.expires_at = chrono::Utc::now().timestamp() + expires_in;
        info!("DingTalk access_token refreshed (expires in {}s)", expires_in);
        Ok(token)
    }

    // ── Stream SDK (WebSocket) ────────────────────────────────────────────────

    async fn get_stream_endpoint(&self) -> Result<StreamEndpointResponse> {
        let app_key = &self.config.channels.dingtalk.app_key;
        let app_secret = &self.config.channels.dingtalk.app_secret;

        #[derive(Serialize)]
        struct StreamRequest<'a> {
            #[serde(rename = "clientId")]
            client_id: &'a str,
            #[serde(rename = "clientSecret")]
            client_secret: &'a str,
            #[serde(rename = "subscriptions")]
            subscriptions: Vec<serde_json::Value>,
            #[serde(rename = "ua")]
            ua: &'a str,
        }

        let req = StreamRequest {
            client_id: app_key,
            client_secret: app_secret,
            subscriptions: vec![
                serde_json::json!({
                    "type": "EVENT",
                    "topic": "*"
                }),
                serde_json::json!({
                    "type": "CALLBACK",
                    "topic": "/v1.0/im/bot/messages/get"
                }),
            ],
            ua: "blockcell/1.0",
        };

        let resp = self
            .client
            .post("https://api.dingtalk.com/v1.0/gateway/connections/open")
            .json(&req)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("DingTalk stream endpoint request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Channel(format!(
                "DingTalk stream endpoint HTTP {}: {}",
                status, body
            )));
        }

        let endpoint: StreamEndpointResponse = resp
            .json()
            .await
            .map_err(|e| Error::Channel(format!("Failed to parse DingTalk stream endpoint: {}", e)))?;

        Ok(endpoint)
    }

    async fn run_stream_sdk(&self) -> Result<()> {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

        let endpoint = self.get_stream_endpoint().await?;
        let ws_url = format!("{}?ticket={}", endpoint.endpoint, endpoint.ticket);

        let url = url::Url::parse(&ws_url)
            .map_err(|e| Error::Channel(format!("Invalid DingTalk stream URL: {}", e)))?;

        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| Error::Channel(format!("DingTalk stream connect failed: {}", e)))?;

        info!("DingTalk Stream SDK connected");
        let (mut write, mut read) = ws_stream.split();

        while let Some(msg) = read.next().await {
            match msg {
                Ok(WsMessage::Text(text)) => {
                    // Parse the stream event
                    match serde_json::from_str::<StreamEvent>(&text) {
                        Ok(event) => {
                            // Send ACK back
                            let ack = serde_json::json!({
                                "code": 200,
                                "headers": {
                                    "messageId": event.headers.as_ref()
                                        .and_then(|h| h.event_id.as_deref())
                                        .unwrap_or("")
                                },
                                "message": "OK",
                                "data": ""
                            });
                            if let Err(e) = write.send(WsMessage::Text(ack.to_string())).await {
                                error!(error = %e, "Failed to send DingTalk stream ACK");
                            }

                            match event.event_type.as_str() {
                                "CALLBACK" => {
                                    if let Some(data) = &event.data {
                                        if let Err(e) = self.handle_callback_message(data).await {
                                            error!(error = %e, "Failed to handle DingTalk callback");
                                        }
                                    }
                                }
                                "SYSTEM" => {
                                    debug!("DingTalk stream SYSTEM event");
                                }
                                other => {
                                    debug!(event_type = %other, "DingTalk stream: unhandled event type");
                                }
                            }
                        }
                        Err(e) => {
                            debug!(error = %e, raw = %text, "Failed to parse DingTalk stream event");
                        }
                    }
                }
                Ok(WsMessage::Ping(data)) => {
                    let _ = write.send(WsMessage::Pong(data)).await;
                }
                Ok(WsMessage::Close(_)) => {
                    return Err(Error::Channel("DingTalk stream closed".to_string()));
                }
                Err(e) => {
                    return Err(Error::Channel(format!("DingTalk stream WS error: {}", e)));
                }
                _ => {}
            }
        }
        Err(Error::Channel("DingTalk stream ended".to_string()))
    }

    async fn handle_callback_message(&self, data: &serde_json::Value) -> Result<()> {
        // DingTalk callback message format
        let msg_type = data
            .get("msgtype")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if msg_type != "text" {
            debug!(msg_type = %msg_type, "DingTalk: skipping non-text message");
            return Ok(());
        }

        let text = data
            .get("text")
            .and_then(|v| v.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        if text.is_empty() {
            return Ok(());
        }

        let sender_id = data
            .get("senderId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if !self.is_allowed(&sender_id) {
            debug!(sender_id = %sender_id, "DingTalk: sender not in allowlist");
            return Ok(());
        }

        let conversation_id = data
            .get("conversationId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let sender_nick = data
            .get("senderNick")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let msg_id = data
            .get("msgId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let conversation_type = data
            .get("conversationType")
            .and_then(|v| v.as_str())
            .unwrap_or("1")
            .to_string();

        let inbound = InboundMessage {
            channel: "dingtalk".to_string(),
            sender_id: sender_id.clone(),
            chat_id: conversation_id,
            content: text,
            media: vec![],
            metadata: serde_json::json!({
                "sender_nick": sender_nick,
                "msg_id": msg_id,
                "conversation_type": conversation_type,
                "mode": "stream",
            }),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };

        self.inbound_tx
            .send(inbound)
            .await
            .map_err(|e| Error::Channel(e.to_string()))
    }

    pub async fn run_loop(self: Arc<Self>, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        if !self.config.channels.dingtalk.enabled {
            info!("DingTalk channel disabled");
            return;
        }

        if self.config.channels.dingtalk.app_key.is_empty() {
            warn!("DingTalk app_key not configured");
            return;
        }

        if self.config.channels.dingtalk.app_secret.is_empty() {
            warn!("DingTalk app_secret not configured");
            return;
        }

        info!("DingTalk channel starting (Stream SDK mode)");

        let mut backoff = Duration::from_secs(2);
        loop {
            tokio::select! {
                result = self.run_stream_sdk() => {
                    match result {
                        Ok(_) => {
                            info!("DingTalk stream exited normally");
                        }
                        Err(e) => {
                            error!(error = %e, backoff_secs = backoff.as_secs(),
                                "DingTalk stream error, reconnecting");
                            tokio::select! {
                                _ = tokio::time::sleep(backoff) => {}
                                _ = shutdown.recv() => {
                                    info!("DingTalk channel shutting down");
                                    return;
                                }
                            }
                            backoff = (backoff * 2).min(Duration::from_secs(60));
                            continue;
                        }
                    }
                }
                _ = shutdown.recv() => {
                    info!("DingTalk channel shutting down");
                    return;
                }
            }
            backoff = Duration::from_secs(2);
        }
    }
}

// ── send_message ──────────────────────────────────────────────────────────────

/// Send a text message to a DingTalk conversation or user.
/// - If `chat_id` starts with `"cid:"` or is a known group chatid format, uses `chat/send`.
/// - Otherwise treats `chat_id` as a user ID and uses the new API v1.0 robot message.
/// Long messages are split to respect the 4096-char limit.
pub async fn send_message(config: &Config, chat_id: &str, text: &str) -> Result<()> {
    crate::rate_limit::dingtalk_limiter().acquire().await;

    // DingTalk group chatids are returned by appchat/create and start with a fixed prefix
    // or are long hex strings. User IDs from Stream SDK conversationId for 1:1 chats
    // are plain alphanumeric strings that do NOT work with chat/send.
    // Heuristic: group chatids from appchat API start with "cid:" or are exactly 32 hex chars.
    if is_group_chat_id(chat_id) {
        let client = shared_client();
        let app_key = &config.channels.dingtalk.app_key;
        let app_secret = &config.channels.dingtalk.app_secret;
        let token = fetch_access_token(&client, app_key, app_secret).await?;
        let chunks = split_message(text, DINGTALK_MSG_LIMIT);
        for (i, chunk) in chunks.iter().enumerate() {
            do_send_message(&client, &token, chat_id, chunk).await?;
            if i + 1 < chunks.len() {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    } else {
        // Treat as user ID — use the new API v1.0
        send_message_to_user(config, chat_id, text).await?;
    }
    Ok(())
}

/// Returns true if the chat_id refers to a DingTalk group chat (appchat).
/// Group chatids start with "cid:" or are 32-character lowercase hex strings.
fn is_group_chat_id(chat_id: &str) -> bool {
    if chat_id.starts_with("cid:") {
        return true;
    }
    // appchat IDs are 32-char hex
    chat_id.len() == 32 && chat_id.chars().all(|c| c.is_ascii_hexdigit())
}

async fn fetch_access_token(client: &Client, app_key: &str, app_secret: &str) -> Result<String> {
    let resp = client
        .get(format!("{}/gettoken", DINGTALK_API_BASE))
        .query(&[("appkey", app_key), ("appsecret", app_secret)])
        .send()
        .await
        .map_err(|e| Error::Channel(format!("DingTalk gettoken failed: {}", e)))?;

    let body: TokenResponse = resp
        .json()
        .await
        .map_err(|e| Error::Channel(format!("Failed to parse DingTalk token: {}", e)))?;

    if body.errcode != 0 {
        return Err(Error::Channel(format!(
            "DingTalk token error {}: {}",
            body.errcode, body.errmsg
        )));
    }

    body.access_token
        .ok_or_else(|| Error::Channel("No access_token in DingTalk response".to_string()))
}

async fn do_send_message(client: &Client, token: &str, chat_id: &str, text: &str) -> Result<()> {
    #[derive(Serialize)]
    struct SendRequest<'a> {
        chatid: &'a str,
        msg: TextMsg<'a>,
    }

    #[derive(Serialize)]
    struct TextMsg<'a> {
        msgtype: &'a str,
        text: TextBody<'a>,
    }

    #[derive(Serialize)]
    struct TextBody<'a> {
        content: &'a str,
    }

    let req = SendRequest {
        chatid: chat_id,
        msg: TextMsg {
            msgtype: "text",
            text: TextBody { content: text },
        },
    };

    let resp = client
        .post(format!("{}/chat/send", DINGTALK_API_BASE))
        .query(&[("access_token", token)])
        .json(&req)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("Failed to send DingTalk message: {}", e)))?;

    let body: DingTalkResponse = resp
        .json()
        .await
        .map_err(|e| Error::Channel(format!("Failed to parse DingTalk send response: {}", e)))?;

    if body.errcode != 0 {
        return Err(Error::Channel(format!(
            "DingTalk send error {}: {}",
            body.errcode, body.errmsg
        )));
    }

    Ok(())
}

/// Send a message to a DingTalk user (1:1) via the new API v1.0
pub async fn send_message_to_user(config: &Config, user_id: &str, text: &str) -> Result<()> {
    crate::rate_limit::dingtalk_limiter().acquire().await;

    let client = shared_client();
    let app_key = &config.channels.dingtalk.app_key;
    let app_secret = &config.channels.dingtalk.app_secret;

    let token = fetch_access_token(&client, app_key, app_secret).await?;

    let chunks = split_message(text, DINGTALK_MSG_LIMIT);
    for (i, chunk) in chunks.iter().enumerate() {
        #[derive(Serialize)]
        struct OrgMsgRequest<'a> {
            #[serde(rename = "robotCode")]
            robot_code: &'a str,
            #[serde(rename = "userIds")]
            user_ids: Vec<&'a str>,
            #[serde(rename = "msgKey")]
            msg_key: &'a str,
            #[serde(rename = "msgParam")]
            msg_param: String,
        }

        let msg_param = serde_json::json!({ "content": chunk }).to_string();
        let req = OrgMsgRequest {
            robot_code: app_key,
            user_ids: vec![user_id],
            msg_key: "sampleText",
            msg_param,
        };

        let resp = client
            .post("https://api.dingtalk.com/v1.0/robot/oToMessages/batchSend")
            .header("x-acs-dingtalk-access-token", &token)
            .json(&req)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Failed to send DingTalk user message: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Channel(format!(
                "DingTalk user message HTTP {}: {}",
                status, body
            )));
        }

        if i + 1 < chunks.len() {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    Ok(())
}

fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.chars().count() <= max_len {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.chars().count() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }
        // Find a safe byte boundary at max_len chars
        let byte_limit = remaining
            .char_indices()
            .nth(max_len)
            .map(|(i, _)| i)
            .unwrap_or(remaining.len());
        let split_at = remaining[..byte_limit]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(byte_limit);
        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_message_short() {
        let chunks = split_message("hello world", 4096);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello world");
    }

    #[test]
    fn test_split_message_long() {
        let line = "a".repeat(100);
        let text = (0..50).map(|_| line.clone()).collect::<Vec<_>>().join("\n");
        let chunks = split_message(&text, 4096);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 4096);
        }
    }

    #[test]
    fn test_split_message_chinese() {
        // Each Chinese char is 3 bytes; 5000 chars = 15000 bytes
        let text = "钉钉".repeat(2500);
        let chunks = split_message(&text, 4096);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 4096, "chunk too long: {} chars", chunk.chars().count());
        }
    }

    #[test]
    fn test_is_group_chat_id() {
        assert!(is_group_chat_id("cid:abc123"));
        assert!(is_group_chat_id("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4"));
        assert!(!is_group_chat_id("user12345"));
        assert!(!is_group_chat_id("zhangsan"));
    }

    #[test]
    fn test_token_response_deserialize() {
        let json = r#"{"errcode":0,"errmsg":"ok","access_token":"test_token","expires_in":7200}"#;
        let resp: TokenResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.errcode, 0);
        assert_eq!(resp.access_token.as_deref(), Some("test_token"));
        assert_eq!(resp.expires_in, Some(7200));
    }

    #[test]
    fn test_stream_event_deserialize() {
        let json = r#"{
            "type": "CALLBACK",
            "headers": {"eventId": "abc123", "eventType": "im.bot.message"},
            "data": {"msgtype": "text", "text": {"content": "hello"}, "senderId": "user1"}
        }"#;
        let event: StreamEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "CALLBACK");
        assert!(event.headers.is_some());
        assert!(event.data.is_some());
    }

    #[test]
    fn test_dingtalk_response_error() {
        let json = r#"{"errcode":40014,"errmsg":"invalid access_token"}"#;
        let resp: DingTalkResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.errcode, 40014);
        assert_eq!(resp.errmsg, "invalid access_token");
    }
}
