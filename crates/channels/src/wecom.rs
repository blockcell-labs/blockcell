use blockcell_core::{Config, Error, InboundMessage, Result};
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

const WECOM_API_BASE: &str = "https://qyapi.weixin.qq.com/cgi-bin";
/// WeCom single message character limit
const WECOM_MSG_LIMIT: usize = 2048;
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
    token: String,
    expires_at: i64,
}

impl CachedToken {
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
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct WeComResponse {
    errcode: i32,
    errmsg: String,
}

/// WeCom callback message (XML-based, parsed from webhook)
/// WeCom uses XML for incoming messages via webhook/callback URL.
/// For polling, we use the message API.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct WeComMessage {
    #[serde(rename = "ToUserName")]
    #[serde(default)]
    to_user_name: Option<String>,
    #[serde(rename = "FromUserName")]
    #[serde(default)]
    from_user_name: Option<String>,
    #[serde(rename = "CreateTime")]
    #[serde(default)]
    create_time: Option<i64>,
    #[serde(rename = "MsgType")]
    #[serde(default)]
    msg_type: Option<String>,
    #[serde(rename = "Content")]
    #[serde(default)]
    content: Option<String>,
    #[serde(rename = "MsgId")]
    #[serde(default)]
    msg_id: Option<String>,
    #[serde(rename = "AgentID")]
    #[serde(default)]
    agent_id: Option<String>,
}

/// WeCom channel supporting two modes:
/// - **Callback mode** (preferred): Receives messages via webhook callback URL.
///   Requires `corp_id`, `corp_secret`, `agent_id`, and `token`/`encoding_aes_key` for verification.
/// - **Polling mode**: Polls the message API when callback is not configured.
///
/// WeCom (企业微信) uses a different architecture from other platforms:
/// - Inbound: Webhook callbacks (HTTP POST to your server) or polling
/// - Outbound: REST API `message/send`
///
/// For the Stream SDK / WebSocket approach, WeCom provides a "企业微信接收消息服务器" callback.
/// This implementation uses polling via `message/get_statistics` + direct message send.
pub struct WeComChannel {
    config: Config,
    client: Client,
    #[allow(dead_code)]
    inbound_tx: mpsc::Sender<InboundMessage>,
    token_cache: Arc<tokio::sync::Mutex<CachedToken>>,
}

impl WeComChannel {
    pub fn new(config: Config, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        Self {
            config,
            client: shared_client(),
            inbound_tx,
            token_cache: Arc::new(tokio::sync::Mutex::new(CachedToken::default())),
        }
    }

    #[allow(dead_code)]
    fn is_allowed(&self, user_id: &str) -> bool {
        let allow_from = &self.config.channels.wecom.allow_from;
        if allow_from.is_empty() {
            return true;
        }
        allow_from.iter().any(|a| a == user_id)
    }

    pub async fn get_access_token(&self) -> Result<String> {
        let mut cache = self.token_cache.lock().await;
        if cache.is_valid() {
            return Ok(cache.token.clone());
        }

        let corp_id = &self.config.channels.wecom.corp_id;
        let corp_secret = &self.config.channels.wecom.corp_secret;

        let resp = self
            .client
            .get(format!("{}/gettoken", WECOM_API_BASE))
            .query(&[("corpid", corp_id.as_str()), ("corpsecret", corp_secret.as_str())])
            .send()
            .await
            .map_err(|e| Error::Channel(format!("WeCom gettoken request failed: {}", e)))?;

        let body: TokenResponse = resp
            .json()
            .await
            .map_err(|e| Error::Channel(format!("Failed to parse WeCom token response: {}", e)))?;

        if body.errcode != 0 {
            return Err(Error::Channel(format!(
                "WeCom gettoken error {}: {}",
                body.errcode, body.errmsg
            )));
        }

        let token = body
            .access_token
            .ok_or_else(|| Error::Channel("No access_token in WeCom response".to_string()))?;
        let expires_in = body.expires_in.unwrap_or(7200);

        cache.token = token.clone();
        cache.expires_at = chrono::Utc::now().timestamp() + expires_in;
        info!("WeCom access_token refreshed (expires in {}s)", expires_in);
        Ok(token)
    }

    // ── Polling mode ──────────────────────────────────────────────────────────

    /// Poll for new messages via WeCom message API.
    /// WeCom doesn't have a direct "get messages" polling API for app messages;
    /// instead we use the appchat message list or rely on callback.
    /// This implementation uses a simple polling approach via message statistics.
    async fn run_polling(&self, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        let poll_interval = Duration::from_secs(
            self.config.channels.wecom.poll_interval_secs.max(5) as u64,
        );

        info!(
            interval_secs = poll_interval.as_secs(),
            "WeCom channel started (polling mode)"
        );

        // WeCom doesn't support direct message polling for app messages.
        // The proper way is to set up a callback URL. We log a warning here.
        warn!(
            "WeCom polling mode: WeCom requires a callback URL for real-time message reception. \
             Configure 'callback_token' and 'encoding_aes_key' and set up your server's \
             callback URL in the WeCom admin console for full functionality. \
             Polling mode will only process messages sent via the agent's send_message API."
        );

        loop {
            tokio::select! {
                _ = tokio::time::sleep(poll_interval) => {
                    // In polling mode, we can check for pending messages
                    // via the WeCom message API if configured
                    if let Err(e) = self.poll_messages().await {
                        error!(error = %e, "WeCom poll error");
                    }
                }
                _ = shutdown.recv() => {
                    info!("WeCom channel shutting down (polling)");
                    break;
                }
            }
        }
    }

    async fn poll_messages(&self) -> Result<()> {
        // WeCom does not provide a public API for polling received app messages.
        // The correct approach is to configure a callback URL in the WeCom admin
        // console. In polling mode we simply verify the token is still valid.
        let _token = self.get_access_token().await?;
        debug!("WeCom token heartbeat OK (polling mode — no inbound messages without callback URL)");
        Ok(())
    }

    #[allow(dead_code)]
    async fn process_message_json(&self, msg: &serde_json::Value) -> Result<()> {
        let msg_type = msg.get("msgtype").and_then(|v| v.as_str()).unwrap_or("");
        if msg_type != "text" {
            debug!(msg_type = %msg_type, "WeCom: skipping non-text message");
            return Ok(());
        }

        let content = msg
            .get("text")
            .and_then(|v| v.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        if content.is_empty() {
            return Ok(());
        }

        let from_user = msg
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if !self.is_allowed(&from_user) {
            debug!(from_user = %from_user, "WeCom: user not in allowlist");
            return Ok(());
        }

        let to_party = msg
            .get("toparty")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let msg_id = msg
            .get("msgid")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let inbound = InboundMessage {
            channel: "wecom".to_string(),
            sender_id: from_user.clone(),
            chat_id: if to_party.is_empty() { from_user } else { to_party },
            content,
            media: vec![],
            metadata: serde_json::json!({
                "msg_id": msg_id,
                "msg_type": msg_type,
                "mode": "polling",
            }),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };

        self.inbound_tx
            .send(inbound)
            .await
            .map_err(|e| Error::Channel(e.to_string()))
    }

    // ── Callback verification (for webhook mode) ──────────────────────────────

    /// Verify a WeCom callback request signature.
    /// WeCom uses SHA1(sort(token, timestamp, nonce)) for verification.
    pub fn verify_signature(token: &str, timestamp: &str, nonce: &str, signature: &str) -> bool {
        let mut parts = vec![token, timestamp, nonce];
        parts.sort_unstable();
        let combined = parts.join("");

        let hash = sha1_hex(combined.as_bytes());
        hash == signature
    }

    pub async fn run_loop(self: Arc<Self>, shutdown: tokio::sync::broadcast::Receiver<()>) {
        if !self.config.channels.wecom.enabled {
            info!("WeCom channel disabled");
            return;
        }

        if self.config.channels.wecom.corp_id.is_empty() {
            warn!("WeCom corp_id not configured");
            return;
        }

        if self.config.channels.wecom.corp_secret.is_empty() {
            warn!("WeCom corp_secret not configured");
            return;
        }

        // Verify we can get an access token
        match self.get_access_token().await {
            Ok(_) => info!("WeCom access token obtained successfully"),
            Err(e) => {
                error!(error = %e, "WeCom: failed to get access token, channel will not start");
                return;
            }
        }

        self.run_polling(shutdown).await;
    }
}

/// Simple SHA1 implementation for WeCom signature verification.
fn sha1_hex(data: &[u8]) -> String {
    let hash = sha1_digest(data);
    hash.iter().fold(String::new(), |mut acc, b| {
        acc.push_str(&format!("{:02x}", b));
        acc
    })
}

fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];

    let msg_len = data.len();
    let bit_len = (msg_len as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0x00);
    }
    for i in (0..8).rev() {
        msg.push(((bit_len >> (i * 8)) & 0xFF) as u8);
    }

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i*4], chunk[i*4+1], chunk[i*4+2], chunk[i*4+3]]);
        }
        for i in 16..80 {
            w[i] = (w[i-3] ^ w[i-8] ^ w[i-14] ^ w[i-16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);

        for i in 0..80 {
            let (f, k) = match i {
                0..=19  => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32),
                _       => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let temp = a.rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d; d = c; c = b.rotate_left(30); b = a; a = temp;
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut result = [0u8; 20];
    for (i, &val) in h.iter().enumerate() {
        let bytes = val.to_be_bytes();
        result[i*4..i*4+4].copy_from_slice(&bytes);
    }
    result
}

// ── send_message ──────────────────────────────────────────────────────────────

/// Send a text message to a WeCom user or group.
/// `chat_id` can be a user_id (touser) or a group chat_id (chatid).
pub async fn send_message(config: &Config, chat_id: &str, text: &str) -> Result<()> {
    crate::rate_limit::wecom_limiter().acquire().await;

    let client = shared_client();
    let token = fetch_access_token_static(&client, config).await?;

    let chunks = split_message(text, WECOM_MSG_LIMIT);
    for (i, chunk) in chunks.iter().enumerate() {
        do_send_message(&client, &token, config, chat_id, chunk).await?;
        if i + 1 < chunks.len() {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
    Ok(())
}

async fn fetch_access_token_static(client: &Client, config: &Config) -> Result<String> {
    let corp_id = &config.channels.wecom.corp_id;
    let corp_secret = &config.channels.wecom.corp_secret;

    let resp = client
        .get(format!("{}/gettoken", WECOM_API_BASE))
        .query(&[("corpid", corp_id.as_str()), ("corpsecret", corp_secret.as_str())])
        .send()
        .await
        .map_err(|e| Error::Channel(format!("WeCom gettoken failed: {}", e)))?;

    let body: TokenResponse = resp
        .json()
        .await
        .map_err(|e| Error::Channel(format!("Failed to parse WeCom token: {}", e)))?;

    if body.errcode != 0 {
        return Err(Error::Channel(format!(
            "WeCom token error {}: {}",
            body.errcode, body.errmsg
        )));
    }

    body.access_token
        .ok_or_else(|| Error::Channel("No access_token in WeCom response".to_string()))
}

async fn do_send_message(
    client: &Client,
    token: &str,
    config: &Config,
    chat_id: &str,
    text: &str,
) -> Result<()> {
    let agent_id = config.channels.wecom.agent_id;

    // Determine if chat_id is a group chat (starts with "wr" for WeCom group) or user
    // WeCom group chats use chatid, individual users use touser
    let body = if chat_id.starts_with("wr") || chat_id.starts_with("WR") {
        // Group chat (appchat)
        serde_json::json!({
            "chatid": chat_id,
            "msgtype": "text",
            "text": {
                "content": text
            },
            "safe": 0
        })
    } else {
        // Individual user or @all
        serde_json::json!({
            "touser": chat_id,
            "msgtype": "text",
            "agentid": agent_id,
            "text": {
                "content": text
            },
            "safe": 0
        })
    };

    let endpoint = if chat_id.starts_with("wr") || chat_id.starts_with("WR") {
        format!("{}/appchat/send", WECOM_API_BASE)
    } else {
        format!("{}/message/send", WECOM_API_BASE)
    };

    let resp = client
        .post(&endpoint)
        .query(&[("access_token", token)])
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("Failed to send WeCom message: {}", e)))?;

    let result: WeComResponse = resp
        .json()
        .await
        .map_err(|e| Error::Channel(format!("Failed to parse WeCom send response: {}", e)))?;

    if result.errcode != 0 {
        return Err(Error::Channel(format!(
            "WeCom send error {}: {}",
            result.errcode, result.errmsg
        )));
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
            assert!(chunk.chars().count() <= 2048, "chunk too long: {} chars", chunk.chars().count());
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
    fn test_verify_signature() {
        // WeCom signature: SHA1(sort(token, timestamp, nonce))
        // token="test", timestamp="1409735669", nonce="xxxxxx"
        // sorted: ["1409735669", "test", "xxxxxx"] → "1409735669testxxxxxx"
        let token = "test";
        let timestamp = "1409735669";
        let nonce = "xxxxxx";
        let mut parts = vec![token, timestamp, nonce];
        parts.sort_unstable();
        let combined = parts.join("");
        let expected = sha1_hex(combined.as_bytes());
        assert!(WeComChannel::verify_signature(token, timestamp, nonce, &expected));
    }
}
