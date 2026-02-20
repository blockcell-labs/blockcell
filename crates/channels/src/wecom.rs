use aes::cipher::{BlockDecryptMut, KeyIvInit};
use base64::{
    alphabet,
    engine::{general_purpose, DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
    Engine as _,
};
use blockcell_core::{Config, Error, InboundMessage, Result};
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

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

        // Only warn if callback credentials are missing — if they're configured,
        // the user is using webhook mode via gateway and polling is just a heartbeat.
        if self.config.channels.wecom.callback_token.is_empty()
            || self.config.channels.wecom.encoding_aes_key.is_empty()
        {
            warn!(
                "WeCom polling mode: WeCom requires a callback URL for real-time message reception. \
                 Configure 'callback_token' and 'encoding_aes_key' and set up your server's \
                 callback URL in the WeCom admin console for full functionality. \
                 Polling mode will only process messages sent via the agent's send_message API."
            );
        }

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

/// Percent-decode a URL query parameter value (%2B → +, %2F → /, %3D → =, etc.).
/// Does NOT treat '+' as space (that's form-encoding, not used by WeCom).
fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_nibble(bytes[i+1]), hex_nibble(bytes[i+2])) {
                out.push(char::from(h << 4 | l));
                i += 3;
                continue;
            }
        }
        out.push(char::from(bytes[i]));
        i += 1;
    }
    out
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
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

/// Handle a WeCom webhook request.
///
/// WeCom sends two types of requests to the callback URL:
/// - **GET**: URL verification — responds with `echostr` query param if signature is valid
/// - **POST**: Message/event callback — parses XML body and forwards to inbound channel
///
/// Returns `(status_code, body_string)`.
pub async fn process_webhook(
    config: &Config,
    method: &str,
    query: &std::collections::HashMap<String, String>,
    body: &str,
    inbound_tx: Option<&tokio::sync::mpsc::Sender<blockcell_core::InboundMessage>>,
) -> (u16, String) {
    let wecom_cfg = &config.channels.wecom;

    let has_wecom_params = query.contains_key("msg_signature")
        || query.contains_key("signature")
        || query.contains_key("echostr");

    if method == "GET" {
        if !has_wecom_params {
            // Plain connectivity probe (e.g. wget/curl health check) — return 200
            return (200, "ok".to_string());
        }

        // WeCom URL verification:
        // 1. echostr is AES-encrypted Base64
        // 2. Signature = SHA1(sort(token, timestamp, nonce, echostr_encrypted))
        let msg_signature = query.get("msg_signature").or_else(|| query.get("signature")).map(|s| s.as_str()).unwrap_or("");
        let timestamp = query.get("timestamp").map(|s| s.as_str()).unwrap_or("");
        let nonce = query.get("nonce").map(|s| s.as_str()).unwrap_or("");
        // URL-decode the echostr: WeCom percent-encodes '+' as '%2B' etc. in the query string,
        // but signs and encrypts the plain base64 string. Decode before both sig check and decrypt.
        let echostr_raw = query.get("echostr").map(|s| s.as_str()).unwrap_or("");
        let echostr_enc_owned = percent_decode(echostr_raw);
        let echostr_enc = echostr_enc_owned.as_str();

        // ── 原始数据诊断日志（INFO级别，方便复制调试）──────────────────────
        tracing::info!(
            token        = %wecom_cfg.callback_token,
            timestamp    = %timestamp,
            nonce        = %nonce,
            msg_signature= %msg_signature,
            echostr      = %echostr_enc,
            echostr_len  = echostr_enc.len(),
            encoding_aes_key = %wecom_cfg.encoding_aes_key,
            encoding_aes_key_len = wecom_cfg.encoding_aes_key.len(),
            "WeCom GET 原始参数"
        );

        if !wecom_cfg.callback_token.is_empty() {
            // 计算签名并打印，方便对比
            let mut parts = vec![
                wecom_cfg.callback_token.as_str(),
                timestamp,
                nonce,
                echostr_enc,
            ];
            parts.sort_unstable();
            let combined = parts.join("");
            let computed = sha1_hex(combined.as_bytes());
            tracing::info!(
                computed_signature = %computed,
                expected_signature = %msg_signature,
                sort_input         = %combined,
                "WeCom GET 签名计算"
            );

            // 4-param signature: sort(token, timestamp, nonce, msg_encrypt)
            if computed != msg_signature {
                tracing::warn!(
                    computed  = %computed,
                    expected  = %msg_signature,
                    "WeCom webhook: GET 签名不匹配"
                );
                return (403, "Forbidden: invalid signature".to_string());
            }
        }

        // Decrypt echostr to get plaintext msg
        match decrypt_wecom_msg(echostr_enc, &wecom_cfg.encoding_aes_key) {
            Ok(plain) => {
                tracing::info!("WeCom webhook: URL verification OK, returning echostr plaintext");
                return (200, plain);
            }
            Err(e) => {
                tracing::error!("WeCom webhook: failed to decrypt echostr: {}", e);
                return (500, "decrypt error".to_string());
            }
        }
    }

    // POST: parse XML body
    if body.is_empty() {
        return (200, "success".to_string());
    }

    // POST messages use <Encrypt> field (AES encrypted)
    // Verify signature: SHA1(sort(token, timestamp, nonce, msg_encrypt))
    let msg_encrypt = extract_xml_tag(body, "Encrypt").unwrap_or_default();
    let timestamp = query.get("timestamp").map(|s| s.as_str()).unwrap_or("");
    let nonce = query.get("nonce").map(|s| s.as_str()).unwrap_or("");
    let msg_signature = query.get("msg_signature").or_else(|| query.get("signature")).map(|s| s.as_str()).unwrap_or("");

    if !wecom_cfg.callback_token.is_empty() && !msg_encrypt.is_empty() {
        if !verify_signature_4(&wecom_cfg.callback_token, timestamp, nonce, &msg_encrypt, msg_signature) {
            tracing::warn!("WeCom webhook: POST signature verification failed");
            return (403, "Forbidden: invalid signature".to_string());
        }
    }

    // Decrypt the message body
    let decrypted_body = if !msg_encrypt.is_empty() && !wecom_cfg.encoding_aes_key.is_empty() {
        match decrypt_wecom_msg(&msg_encrypt, &wecom_cfg.encoding_aes_key) {
            Ok(plain) => plain,
            Err(e) => {
                tracing::error!("WeCom webhook: failed to decrypt POST message: {}", e);
                return (200, "success".to_string());
            }
        }
    } else {
        // No encryption configured — treat body as plain XML
        body.to_string()
    };

    // Extract fields from decrypted XML
    let from_user = extract_xml_tag(&decrypted_body, "FromUserName").unwrap_or_default();
    let msg_type = extract_xml_tag(&decrypted_body, "MsgType").unwrap_or_default();
    let content = extract_xml_tag(&decrypted_body, "Content").unwrap_or_default();
    let _to_user = extract_xml_tag(&decrypted_body, "ToUserName").unwrap_or_default();
    let msg_id = extract_xml_tag(&decrypted_body, "MsgId");

    tracing::debug!(
        from_user = %from_user,
        msg_type = %msg_type,
        content = %content,
        "WeCom webhook: received message"
    );

    if msg_type != "text" {
        return (200, "success".to_string());
    }

    let content = content.trim().to_string();
    if content.is_empty() {
        return (200, "success".to_string());
    }

    // Allowlist check
    let allow_from = &wecom_cfg.allow_from;
    if !allow_from.is_empty() && !allow_from.iter().any(|a| a == &from_user) {
        tracing::debug!(from_user = %from_user, "WeCom webhook: user not in allowlist");
        return (200, "success".to_string());
    }

    if let Some(tx) = inbound_tx {
        let inbound = blockcell_core::InboundMessage {
            channel: "wecom".to_string(),
            sender_id: from_user.clone(),
            chat_id: from_user.clone(),
            content,
            media: vec![],
            metadata: serde_json::json!({
                "msg_id": msg_id,
                "msg_type": msg_type,
                "mode": "webhook",
            }),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };
        if let Err(e) = tx.send(inbound).await {
            tracing::error!(error = %e, "WeCom webhook: failed to forward inbound message");
        }
    }

    (200, "success".to_string())
}

/// Extract the text content of an XML tag (simple, no namespace support needed for WeCom).
fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let content = &xml[start..end];
    // Strip CDATA if present
    let content = if content.starts_with("<![CDATA[") && content.ends_with("]]>") {
        &content[9..content.len()-3]
    } else {
        content
    };
    Some(content.to_string())
}

/// Verify WeCom 4-param signature: SHA1(sort(token, timestamp, nonce, msg_encrypt))
/// This is the correct signature for both GET (echostr) and POST (Encrypt) callbacks.
fn verify_signature_4(token: &str, timestamp: &str, nonce: &str, msg_encrypt: &str, expected: &str) -> bool {
    let mut parts = vec![token, timestamp, nonce, msg_encrypt];
    parts.sort_unstable();
    let combined = parts.join("");
    let hash = sha1_hex(combined.as_bytes());
    hash == expected
}

/// Decrypt a WeCom AES-256-CBC encrypted message.
///
/// Protocol:
/// - AES key = Base64Decode(encodingAESKey + "=")  → 32 bytes
/// - IV = first 16 bytes of AES key
/// - Ciphertext = Base64Decode(msg_encrypt)
/// - Plaintext layout: 16B random | 4B msg_len (big-endian) | msg | corpId
fn decrypt_wecom_msg(msg_encrypt: &str, encoding_aes_key: &str) -> std::result::Result<String, String> {
    if encoding_aes_key.is_empty() {
        return Err("encodingAesKey not configured".to_string());
    }

    tracing::info!(
        encoding_aes_key_raw = %encoding_aes_key,
        msg_encrypt_raw = %msg_encrypt,
        encoding_aes_key_len = encoding_aes_key.len(),
        msg_encrypt_len = msg_encrypt.len(),
        "WeCom decrypt: raw inputs"
    );

    // AES key: WeCom's EncodingAESKey is always exactly 43 chars of standard base64
    // (no padding). Append one '=' to make it 44 chars (valid base64 group).
    // Do NOT strip existing padding first — just normalise whitespace, then pad to 44.
    let key_compact: String = encoding_aes_key
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let key_trimmed = key_compact.trim_end_matches('=');

    tracing::info!(
        key_trimmed = %key_trimmed,
        key_trimmed_len = key_trimmed.len(),
        "WeCom decrypt: key after normalisation"
    );

    let padded_key = match key_trimmed.len() % 4 {
        0 => key_trimmed.to_string(),
        2 => format!("{}==", key_trimmed),
        3 => format!("{}=", key_trimmed),
        // len % 4 == 1 is never valid base64
        _ => {
            return Err(format!(
                "Invalid EncodingAESKey length: {} (after whitespace removal / padding strip)",
                key_trimmed.len()
            ))
        }
    };

    tracing::info!(
        padded_key = %padded_key,
        padded_key_len = padded_key.len(),
        "WeCom decrypt: padded key"
    );

    // WeCom's EncodingAESKey may have non-zero trailing bits in the last base64 character
    // (e.g. '3' instead of the canonical '0'). Rust's STANDARD engine rejects this strictly,
    // so use a lenient engine that ignores trailing bits and accepts optional padding.
    const LENIENT: GeneralPurpose = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    let key_bytes = LENIENT
        .decode(&padded_key)
        .map_err(|e| format!("Failed to decode EncodingAESKey: {}. Key was: '{}'", e, padded_key))?;
    if key_bytes.len() != 32 {
        return Err(format!(
            "AES key length invalid after base64 decode: {} (expected 32). Please verify WeCom EncodingAESKey is correct (usually 43 chars, no '=').",
            key_bytes.len()
        ));
    }

    // IV = first 16 bytes of key
    let iv = &key_bytes[..16];

    // Decode ciphertext
    tracing::info!(
        msg_encrypt = %msg_encrypt,
        msg_encrypt_len = msg_encrypt.len(),
        "WeCom decrypt: decoding msg_encrypt ciphertext"
    );
    let ciphertext = general_purpose::STANDARD
        .decode(msg_encrypt)
        .map_err(|e| format!("Failed to decode msg_encrypt (len={}): {}. Value was: '{}'", msg_encrypt.len(), e, msg_encrypt))?;

    // AES-256-CBC decrypt — WeCom uses PKCS7 with block size 32 (not 16),
    // so pad values 1-32 are valid. Use NoPadding and unpad manually.
    use aes::cipher::block_padding::NoPadding;
    let decryptor = Aes256CbcDec::new_from_slices(&key_bytes, iv)
        .map_err(|e| format!("Failed to create AES decryptor: {}", e))?;
    let mut buf = ciphertext.clone();
    let decrypted = decryptor
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map_err(|e| format!("AES decrypt failed: {}", e))?;
    // Manual PKCS7 unpad with block size 32
    let pad = *decrypted.last().ok_or("AES decrypt: empty output")? as usize;
    if pad == 0 || pad > 32 {
        return Err(format!("AES decrypt: invalid PKCS7 pad value {}", pad));
    }
    let plaintext = &decrypted[..decrypted.len() - pad];

    // Layout: 16B random | 4B msg_len (big-endian) | msg | corpId
    if plaintext.len() < 20 {
        return Err(format!("Decrypted data too short: {} bytes", plaintext.len()));
    }

    let msg_len = u32::from_be_bytes([
        plaintext[16], plaintext[17], plaintext[18], plaintext[19],
    ]) as usize;

    let content_start = 20;
    let content_end = content_start + msg_len;
    if content_end > plaintext.len() {
        return Err(format!(
            "msg_len {} exceeds plaintext length {}",
            msg_len,
            plaintext.len()
        ));
    }

    let msg = std::str::from_utf8(&plaintext[content_start..content_end])
        .map_err(|e| format!("UTF-8 decode failed: {}", e))?;

    Ok(msg.to_string())
}

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
