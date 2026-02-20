//! Lark (international) channel — HTTP Webhook mode.
//!
//! International Lark only supports HTTP callback (webhook) for receiving events.
//! This module provides:
//!   - `handle_webhook`: axum handler for POST /webhook/lark
//!   - `send_message`: outbound message via Lark REST API
//!
//! Webhook flow:
//!   1. URL verification: Lark sends `{"type":"url_verification","challenge":"..."}` → reply `{"challenge":"..."}`
//!   2. Encrypted events: body is `{"encrypt":"<base64>"}`, decrypt with AES-256-CBC using encrypt_key
//!   3. Plain events: body is the event JSON directly (when no encrypt_key configured)

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use blockcell_core::{Config, Error, InboundMessage, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::sync::OnceLock;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info};

const LARK_OPEN_API: &str = "https://open.larksuite.com/open-apis";
const TOKEN_REFRESH_MARGIN_SECS: i64 = 300;

// ---------------------------------------------------------------------------
// Token cache
// ---------------------------------------------------------------------------

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

static GLOBAL_TOKEN_CACHE: OnceLock<Mutex<CachedToken>> = OnceLock::new();

fn global_token_cache() -> &'static Mutex<CachedToken> {
    GLOBAL_TOKEN_CACHE.get_or_init(|| Mutex::new(CachedToken::default()))
}

// ---------------------------------------------------------------------------
// Webhook request/response types
// ---------------------------------------------------------------------------

/// Top-level webhook body — may be encrypted or plain.
#[derive(Debug, Deserialize)]
pub struct WebhookBody {
    /// Present when Lark encryption is enabled.
    #[serde(default)]
    pub encrypt: Option<String>,
    /// Present for url_verification (plain mode).
    #[serde(rename = "type", default)]
    pub event_type: Option<String>,
    /// Present for url_verification.
    #[serde(default)]
    pub challenge: Option<String>,
    /// Present for plain (non-encrypted) events.
    #[serde(default)]
    pub header: Option<EventHeader>,
    #[serde(default)]
    pub event: Option<EventBody>,
}

#[derive(Debug, Deserialize)]
pub struct EventHeader {
    pub event_id: String,
    pub event_type: String,
}

#[derive(Debug, Deserialize)]
pub struct EventBody {
    #[serde(default)]
    pub message: Option<MessageEvent>,
    #[serde(default)]
    pub sender: Option<SenderInfo>,
}

#[derive(Debug, Deserialize)]
pub struct MessageEvent {
    pub message_id: String,
    pub chat_id: String,
    pub message_type: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct SenderInfo {
    pub sender_id: Option<SenderId>,
    pub sender_type: String,
}

#[derive(Debug, Deserialize)]
pub struct SenderId {
    pub open_id: String,
}

#[derive(Debug, Deserialize)]
struct MessageContent {
    text: Option<String>,
}

/// Response for url_verification challenge.
#[derive(Serialize)]
pub struct ChallengeResponse {
    pub challenge: String,
}

/// Generic success response.
#[derive(Serialize)]
pub struct OkResponse {
    pub code: i32,
}

// ---------------------------------------------------------------------------
// Decryption
// ---------------------------------------------------------------------------

type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

/// Decrypt a Lark encrypted webhook body.
///
/// Lark encryption scheme:
///   key  = SHA-256(encrypt_key_string)          → 32 bytes
///   iv   = first 16 bytes of the base64-decoded ciphertext
///   data = remaining bytes (AES-256-CBC + PKCS7)
fn decrypt_lark(encrypt_key: &str, encrypted_b64: &str) -> Result<String> {
    let key_bytes: [u8; 32] = Sha256::digest(encrypt_key.as_bytes()).into();

    let raw = B64
        .decode(encrypted_b64)
        .map_err(|e| Error::Channel(format!("Lark webhook base64 decode failed: {}", e)))?;

    if raw.len() < 16 {
        return Err(Error::Channel("Lark webhook encrypted payload too short".to_string()));
    }

    let (iv, ciphertext) = raw.split_at(16);
    let iv: [u8; 16] = iv.try_into()
        .map_err(|_| Error::Channel("Lark webhook IV length error".to_string()))?;

    let mut buf = ciphertext.to_vec();
    let plaintext = Aes256CbcDec::new(&key_bytes.into(), &iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| Error::Channel(format!("Lark webhook AES decrypt failed: {}", e)))?;

    String::from_utf8(plaintext.to_vec())
        .map_err(|e| Error::Channel(format!("Lark webhook plaintext UTF-8 error: {}", e)))
}

// ---------------------------------------------------------------------------
// Dedup cache (process-global)
// ---------------------------------------------------------------------------

static SEEN_EVENTS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn seen_events() -> &'static Mutex<HashSet<String>> {
    SEEN_EVENTS.get_or_init(|| Mutex::new(HashSet::new()))
}

async fn is_duplicate(event_id: &str) -> bool {
    let mut seen = seen_events().lock().await;
    if seen.contains(event_id) {
        return true;
    }
    seen.insert(event_id.to_string());
    if seen.len() > 1000 {
        let to_remove: Vec<_> = seen.iter().take(100).cloned().collect();
        for id in to_remove {
            seen.remove(&id);
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Core webhook processing logic (shared between gateway handler and tests)
// ---------------------------------------------------------------------------

/// Process a raw webhook body string. Returns the HTTP response body JSON string.
/// `inbound_tx` is None when called in verification-only mode.
pub async fn process_webhook(
    config: &Config,
    raw_body: &str,
    inbound_tx: Option<&mpsc::Sender<InboundMessage>>,
) -> Result<String> {
    let body: WebhookBody = serde_json::from_str(raw_body)
        .map_err(|e| Error::Channel(format!("Lark webhook JSON parse error: {}", e)))?;

    // ── Encrypted body ──────────────────────────────────────────────────────
    if let Some(encrypted) = &body.encrypt {
        let encrypt_key = &config.channels.lark.encrypt_key;
        if encrypt_key.is_empty() {
            return Err(Error::Channel(
                "Lark webhook received encrypted body but encrypt_key is not configured".to_string(),
            ));
        }
        let plaintext = decrypt_lark(encrypt_key, encrypted)?;
        debug!(len = plaintext.len(), "Lark webhook decrypted payload");

        // Recurse with decrypted JSON (plain mode)
        return Box::pin(process_webhook(config, &plaintext, inbound_tx)).await;
    }

    // ── URL verification ────────────────────────────────────────────────────
    if body.event_type.as_deref() == Some("url_verification") {
        let challenge = body.challenge.unwrap_or_default();
        info!("Lark webhook URL verification challenge received");
        return Ok(serde_json::json!({ "challenge": challenge }).to_string());
    }

    // ── Event ───────────────────────────────────────────────────────────────
    let header = match body.header {
        Some(h) => h,
        None => {
            debug!("Lark webhook: no header, ignoring");
            return Ok(serde_json::json!({ "code": 0 }).to_string());
        }
    };

    if is_duplicate(&header.event_id).await {
        debug!(event_id = %header.event_id, "Lark webhook: duplicate event, skipping");
        return Ok(serde_json::json!({ "code": 0 }).to_string());
    }

    if header.event_type != "im.message.receive_v1" {
        debug!(event_type = %header.event_type, "Lark webhook: ignoring non-message event");
        return Ok(serde_json::json!({ "code": 0 }).to_string());
    }

    let event_body = match body.event {
        Some(e) => e,
        None => return Ok(serde_json::json!({ "code": 0 }).to_string()),
    };

    // Skip bot messages
    if let Some(sender) = &event_body.sender {
        if sender.sender_type == "bot" {
            debug!("Lark webhook: skipping bot message");
            return Ok(serde_json::json!({ "code": 0 }).to_string());
        }
    }

    let message = match event_body.message {
        Some(m) => m,
        None => return Ok(serde_json::json!({ "code": 0 }).to_string()),
    };

    // Allow-list check
    let open_id = event_body
        .sender
        .as_ref()
        .and_then(|s| s.sender_id.as_ref())
        .map(|id| id.open_id.as_str())
        .unwrap_or("");

    let allow_from = &config.channels.lark.allow_from;
    if !allow_from.is_empty() && !allow_from.iter().any(|a| a == open_id) {
        debug!(open_id = %open_id, "Lark webhook: sender not in allowlist");
        return Ok(serde_json::json!({ "code": 0 }).to_string());
    }

    // Parse message content
    let text = match message.message_type.as_str() {
        "text" => {
            let content: MessageContent = serde_json::from_str(&message.content)
                .unwrap_or(MessageContent { text: None });
            content.text.unwrap_or_default().trim().to_string()
        }
        other => {
            debug!(msg_type = %other, "Lark webhook: unsupported message type");
            return Ok(serde_json::json!({ "code": 0 }).to_string());
        }
    };

    if text.is_empty() {
        return Ok(serde_json::json!({ "code": 0 }).to_string());
    }

    info!(
        chat_id = %message.chat_id,
        open_id = %open_id,
        len = text.len(),
        "Lark webhook: inbound message"
    );

    if let Some(tx) = inbound_tx {
        let inbound = InboundMessage {
            channel: "lark".to_string(),
            chat_id: message.chat_id.clone(),
            sender_id: open_id.to_string(),
            content: text,
            media: vec![],
            metadata: serde_json::Value::Null,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };
        tx.send(inbound)
            .await
            .map_err(|e| Error::Channel(e.to_string()))?;
    }

    Ok(serde_json::json!({ "code": 0 }).to_string())
}

// ---------------------------------------------------------------------------
// Token management
// ---------------------------------------------------------------------------

async fn fetch_tenant_access_token(app_id: &str, app_secret: &str) -> Result<String> {
    #[derive(Serialize)]
    struct TokenRequest<'a> {
        app_id: &'a str,
        app_secret: &'a str,
    }
    #[derive(Deserialize)]
    struct TokenResponse {
        code: i32,
        msg: String,
        #[serde(default)]
        tenant_access_token: Option<String>,
    }

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Channel(format!("Failed to build HTTP client: {}", e)))?;

    let resp = client
        .post(format!("{}/auth/v3/tenant_access_token/internal", LARK_OPEN_API))
        .json(&TokenRequest { app_id, app_secret })
        .send()
        .await
        .map_err(|e| Error::Channel(format!("Lark token request failed: {}", e)))?;

    let body: TokenResponse = resp
        .json()
        .await
        .map_err(|e| Error::Channel(format!("Lark token response parse failed: {}", e)))?;

    if body.code != 0 {
        return Err(Error::Channel(format!("Lark token error: {}", body.msg)));
    }

    body.tenant_access_token
        .ok_or_else(|| Error::Channel("No tenant_access_token in Lark response".to_string()))
}

async fn get_cached_token(config: &Config) -> Result<String> {
    let cache = global_token_cache();
    let mut guard = cache.lock().await;
    if guard.is_valid() {
        return Ok(guard.token.clone());
    }
    let token = fetch_tenant_access_token(
        &config.channels.lark.app_id,
        &config.channels.lark.app_secret,
    )
    .await?;
    guard.token = token.clone();
    guard.expires_at = chrono::Utc::now().timestamp() + 7200;
    info!("Lark tenant_access_token refreshed (cached 2h)");
    Ok(token)
}

// ---------------------------------------------------------------------------
// Outbound message
// ---------------------------------------------------------------------------

pub async fn send_message(config: &Config, chat_id: &str, text: &str) -> Result<()> {
    crate::rate_limit::lark_limiter().acquire().await;

    let token = get_cached_token(config).await?;

    #[derive(Serialize)]
    struct SendRequest<'a> {
        receive_id: &'a str,
        msg_type: &'a str,
        content: String,
    }

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Channel(format!("Failed to build HTTP client: {}", e)))?;

    let content = serde_json::json!({ "text": text }).to_string();
    let response = client
        .post(format!("{}/im/v1/messages?receive_id_type=chat_id", LARK_OPEN_API))
        .header("Authorization", format!("Bearer {}", token))
        .json(&SendRequest {
            receive_id: chat_id,
            msg_type: "text",
            content,
        })
        .send()
        .await
        .map_err(|e| Error::Channel(format!("Lark send_message request failed: {}", e)))?;

    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Channel(format!("Lark API send error: {}", body)));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cached_token_invalid_when_empty() {
        let token = CachedToken::default();
        assert!(!token.is_valid());
    }

    #[test]
    fn test_cached_token_valid_when_set() {
        let token = CachedToken {
            token: "test_token".to_string(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
        };
        assert!(token.is_valid());
    }

    #[test]
    fn test_cached_token_expired() {
        let token = CachedToken {
            token: "test_token".to_string(),
            expires_at: chrono::Utc::now().timestamp() - 1,
        };
        assert!(!token.is_valid());
    }

    #[test]
    fn test_decrypt_lark() {
        // Verify the decrypt function compiles and handles bad input gracefully
        let result = decrypt_lark("testkey", "notbase64!!!");
        assert!(result.is_err());
    }
}
