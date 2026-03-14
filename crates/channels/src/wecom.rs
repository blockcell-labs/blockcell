use crate::account::{wecom_account_id, wecom_listener_configs};
use aes::cipher::{BlockDecryptMut, KeyIvInit};
use base64::{
    alphabet,
    engine::{general_purpose, DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
    Engine as _,
};
use blockcell_core::{Config, Error, InboundMessage, Result};
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use tracing::{debug, error, info, warn};

/// Global msg_id dedup set — prevents the same WeCom message from being processed twice.
/// WeCom sometimes delivers the same webhook twice (retry on timeout) or echoes bot-sent
/// messages back as callbacks. We keep the last 512 msg_ids in a ring-buffer style set.
static SEEN_MSG_IDS: std::sync::LazyLock<Mutex<HashSet<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));

/// Outbound message for the long connection WebSocket channel.
#[derive(Debug)]
enum LongConnOutbound {
    /// Plain text reply (stream msgtype).
    Text { chat_id: String, content: String },
}

/// Registry of active long connection outbound senders keyed by bot_id.
/// `send_message` uses this to route replies through the WebSocket instead of REST.
static LONGCONN_REGISTRY: std::sync::LazyLock<
    Mutex<HashMap<String, mpsc::Sender<LongConnOutbound>>>,
> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Maps chat_id -> latest req_id from aibot_msg_callback.
/// aibot_respond_msg must echo back the original req_id so WeCom routes the reply correctly.
static CHAT_REQID_REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Shared REST access_token cache for WeCom corp APIs.
static WECOM_TOKEN_CACHE: std::sync::LazyLock<tokio::sync::Mutex<CachedToken>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(CachedToken::default()));

const SEEN_MSG_IDS_MAX: usize = 512;

type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

const WECOM_API_BASE: &str = "https://qyapi.weixin.qq.com/cgi-bin";
const WECOM_LONG_WS_URL: &str = "wss://openws.work.weixin.qq.com";
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

impl WeComResponse {
    fn is_invalid_token(&self) -> bool {
        matches!(self.errcode, 40014 | 42001)
    }
}

#[derive(Debug, Deserialize)]
struct LongConnEnvelope {
    #[serde(default)]
    cmd: String,
    #[serde(default)]
    headers: serde_json::Value,
    #[serde(default)]
    body: serde_json::Value,
    #[serde(default)]
    errcode: Option<i32>,
    #[serde(default)]
    errmsg: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LongConnHeaders {
    #[serde(default)]
    req_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LongConnFrom {
    #[serde(default)]
    userid: String,
    #[serde(default)]
    nickname: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct LongConnText {
    #[serde(default)]
    content: String,
}

#[derive(Debug, Deserialize, Default)]
struct LongConnImage {
    #[serde(default)]
    url: String,
    #[serde(default)]
    aeskey: String,
}

#[derive(Debug, Deserialize, Default)]
struct LongConnVoice {
    #[serde(default)]
    url: String,
    #[serde(default)]
    aeskey: String,
    #[serde(default)]
    recognition: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct LongConnFile {
    #[serde(default)]
    url: String,
    #[serde(default)]
    aeskey: String,
    #[serde(default)]
    filename: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct LongConnMixedItem {
    #[serde(default)]
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct LongConnMixed {
    #[serde(default)]
    items: Vec<LongConnMixedItem>,
}

#[derive(Debug, Deserialize, Default)]
struct LongConnMsgBody {
    #[serde(default)]
    msgid: String,
    #[serde(default)]
    aibotid: String,
    #[serde(default)]
    chatid: String,
    #[serde(default)]
    chattype: String,
    #[serde(default)]
    from: Option<LongConnFrom>,
    #[serde(default)]
    msgtype: String,
    #[serde(default)]
    text: Option<LongConnText>,
    #[serde(default)]
    image: Option<LongConnImage>,
    #[serde(default)]
    voice: Option<LongConnVoice>,
    #[serde(default)]
    file: Option<LongConnFile>,
    #[serde(default)]
    mixed: Option<LongConnMixed>,
}

#[derive(Debug, Serialize)]
struct LongConnCommand<'a, T> {
    cmd: &'a str,
    headers: serde_json::Value,
    body: T,
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
        let token = fetch_access_token_static(&self.client, &self.config).await?;
        let mut cache = self.token_cache.lock().await;
        cache.token = token.clone();
        cache.expires_at = chrono::Utc::now().timestamp() + 7200;
        Ok(token)
    }

    // ── Polling mode ──────────────────────────────────────────────────────────

    /// Poll for new messages via WeCom message API.
    /// WeCom doesn't have a direct "get messages" polling API for app messages;
    /// instead we use the appchat message list or rely on callback.
    /// This implementation uses a simple polling approach via message statistics.
    async fn run_polling(&self, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        let poll_interval =
            Duration::from_secs(self.config.channels.wecom.poll_interval_secs.max(5) as u64);

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
                        error!(error = %e.to_string(), "WeCom poll error");
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
        debug!(
            "WeCom token heartbeat OK (polling mode — no inbound messages without callback URL)"
        );
        Ok(())
    }

    async fn run_long_connection(
        self: Arc<Self>,
        mut shutdown: tokio::sync::broadcast::Receiver<()>,
    ) {
        info!(
            ws_url = %self.ws_url(),
            ping_interval_secs = self.config.channels.wecom.ping_interval_secs.max(10),
            "WeCom channel started (long_connection mode)"
        );

        loop {
            tokio::select! {
                result = self.connect_and_run_long_connection() => {
                    match result {
                        Ok(_) => info!("WeCom long connection closed normally"),
                        Err(e) => {
                            error!(error = %e, "WeCom long connection error, reconnecting in 5s");
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                                _ = shutdown.recv() => {
                                    info!("WeCom channel shutting down (long_connection)");
                                    break;
                                }
                            }
                        }
                    }
                }
                _ = shutdown.recv() => {
                    info!("WeCom channel shutting down (long_connection)");
                    break;
                }
            }
        }
    }

    fn ws_url(&self) -> &str {
        let ws_url = self.config.channels.wecom.ws_url.trim();
        if ws_url.is_empty() {
            WECOM_LONG_WS_URL
        } else {
            ws_url
        }
    }

    async fn connect_and_run_long_connection(&self) -> Result<()> {
        let url = url::Url::parse(self.ws_url())
            .map_err(|e| Error::Channel(format!("Invalid WeCom ws_url: {}", e)))?;
        info!(ws_url = %url, "Connecting to WeCom long connection WebSocket");
        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| Error::Channel(format!("WeCom WebSocket connection failed: {}", e)))?;

        info!("Connected to WeCom long connection WebSocket");

        // Register outbound sender so send_message() can route replies via WebSocket.
        let bot_id = self.config.channels.wecom.bot_id.clone();
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<LongConnOutbound>(64);
        {
            let mut reg = LONGCONN_REGISTRY.lock().unwrap();
            reg.insert(bot_id.clone(), outbound_tx);
        }

        let (mut write, mut read) = ws_stream.split();
        self.send_long_connection_subscribe(&mut write).await?;

        let mut ping = tokio::time::interval(Duration::from_secs(
            self.config.channels.wecom.ping_interval_secs.max(10) as u64,
        ));
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let result = loop {
            tokio::select! {
                _ = ping.tick() => {
                    if let Err(e) = write.send(WsMessage::Text("{\"cmd\":\"ping\"}".to_string())).await {
                        break Err(Error::Channel(format!("WeCom ping failed: {}", e)));
                    }
                }
                outbound = outbound_rx.recv() => {
                    if let Some(outbound_msg) = outbound {
                        let chat_id = match &outbound_msg {
                            LongConnOutbound::Text { chat_id, .. } => chat_id.clone(),
                        };
                        // Echo back the original req_id so WeCom routes the reply correctly.
                        let req_id = {
                            let reg = CHAT_REQID_REGISTRY.lock().unwrap();
                            reg.get(&chat_id)
                                .cloned()
                                .unwrap_or_else(|| format!("blockcell-out-{}", chrono::Utc::now().timestamp_millis()))
                        };
                        let stream_id = format!("blockcell-s-{}", chrono::Utc::now().timestamp_millis());
                        let msg = match outbound_msg {
                            LongConnOutbound::Text { content, .. } => {
                                info!(chat_id = %chat_id, req_id = %req_id, content_len = content.len(), "WeCom longconn: sending text reply");
                                serde_json::json!({
                                    "cmd": "aibot_respond_msg",
                                    "headers": { "req_id": req_id },
                                    "body": {
                                        "msgtype": "stream",
                                        "stream": { "id": stream_id, "finish": true, "content": content }
                                    }
                                })
                            }
                        };
                        let msg_str = msg.to_string();
                        info!(payload = %msg_str, "WeCom longconn: outbound WS payload");
                        if let Err(e) = write.send(WsMessage::Text(msg_str)).await {
                            warn!(error = %e, "WeCom longconn: failed to send outbound reply");
                        }
                    }
                }
                msg = read.next() => {
                    match msg {
                        Some(Ok(WsMessage::Text(text))) => {
                            if let Err(e) = self.handle_long_connection_message(&text, &mut write).await {
                                break Err(e);
                            }
                        }
                        Some(Ok(WsMessage::Binary(data))) => {
                            let text = String::from_utf8_lossy(&data).to_string();
                            if let Err(e) = self.handle_long_connection_message(&text, &mut write).await {
                                break Err(e);
                            }
                        }
                        Some(Ok(WsMessage::Ping(data))) => {
                            if let Err(e) = write.send(WsMessage::Pong(data)).await {
                                break Err(Error::Channel(format!("WeCom pong failed: {}", e)));
                            }
                        }
                        Some(Ok(WsMessage::Pong(_))) => {}
                        Some(Ok(WsMessage::Close(frame))) => {
                            info!(?frame, "WeCom long connection closed by server");
                            break Ok(());
                        }
                        Some(Err(e)) => {
                            break Err(Error::Channel(format!("WeCom WebSocket read failed: {}", e)));
                        }
                        None => break Ok(()),
                        _ => {}
                    }
                }
            }
        };

        // Deregister so send_message stops trying to route to a dead connection.
        {
            let mut reg = LONGCONN_REGISTRY.lock().unwrap();
            reg.remove(&bot_id);
        }
        result
    }

    async fn send_long_connection_subscribe<S>(&self, write: &mut S) -> Result<()>
    where
        S: futures::Sink<WsMessage> + Unpin,
        S::Error: std::fmt::Display,
    {
        let bot_id = self.config.channels.wecom.bot_id.trim();
        let bot_secret = self.config.channels.wecom.bot_secret.trim();
        if bot_id.is_empty() || bot_secret.is_empty() {
            return Err(Error::Channel(
                "WeCom long_connection requires bot_id and bot_secret".to_string(),
            ));
        }

        let req_id = format!("blockcell-{}", chrono::Utc::now().timestamp_millis());
        let req = LongConnCommand {
            cmd: "aibot_subscribe",
            headers: serde_json::json!({ "req_id": req_id }),
            body: serde_json::json!({
                "bot_id": bot_id,
                "secret": bot_secret
            }),
        };
        write
            .send(WsMessage::Text(serde_json::to_string(&req).map_err(
                |e| Error::Channel(format!("WeCom subscribe serialize failed: {}", e)),
            )?))
            .await
            .map_err(|e| Error::Channel(format!("WeCom subscribe send failed: {}", e)))?;
        Ok(())
    }

    async fn handle_long_connection_message<S>(&self, text: &str, write: &mut S) -> Result<()>
    where
        S: futures::Sink<WsMessage> + Unpin,
        S::Error: std::fmt::Display,
    {
        let envelope: LongConnEnvelope = serde_json::from_str(text)
            .map_err(|e| Error::Channel(format!("Failed to parse WeCom long message: {}", e)))?;

        match envelope.cmd.as_str() {
            "aibot_subscribe" => {
                if envelope.errcode.unwrap_or(0) != 0 {
                    return Err(Error::Channel(format!(
                        "WeCom subscribe error {}: {}",
                        envelope.errcode.unwrap_or(-1),
                        envelope.errmsg.unwrap_or_else(|| "unknown".to_string())
                    )));
                }
                info!("WeCom long connection subscribed successfully");
            }
            "aibot_msg_callback" => {
                let headers: LongConnHeaders = serde_json::from_value(envelope.headers.clone())
                    .unwrap_or(LongConnHeaders { req_id: None });
                // Store effective_chat_id -> req_id using the same logic as
                // build_inbound_from_long_connection so the registry key always matches.
                {
                    let chatid = envelope
                        .body
                        .get("chatid")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let from_user = envelope
                        .body
                        .get("from")
                        .and_then(|v| v.get("userid"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let effective_chat_id = if chatid.is_empty() { from_user } else { chatid };
                    if !effective_chat_id.is_empty() {
                        if let Some(req_id) = headers.req_id.as_deref() {
                            let mut reg = CHAT_REQID_REGISTRY.lock().unwrap();
                            reg.insert(effective_chat_id.to_string(), req_id.to_string());
                        }
                    }
                }
                if let Some(inbound) = self
                    .build_inbound_from_long_connection(&envelope.body)
                    .await?
                {
                    self.inbound_tx
                        .send(inbound)
                        .await
                        .map_err(|e| Error::Channel(e.to_string()))?;
                }
                if let Some(req_id) = headers.req_id {
                    let ack = serde_json::json!({
                        "headers": { "req_id": req_id },
                        "errcode": 0,
                        "errmsg": "ok"
                    });
                    write
                        .send(WsMessage::Text(ack.to_string()))
                        .await
                        .map_err(|e| {
                            Error::Channel(format!("WeCom long connection ack failed: {}", e))
                        })?;
                }
            }
            "aibot_event_callback" => {
                debug!(payload = %text, "WeCom long connection event callback received");
                let headers: LongConnHeaders = serde_json::from_value(envelope.headers.clone())
                    .unwrap_or(LongConnHeaders { req_id: None });
                if let Some(req_id) = headers.req_id {
                    let ack = serde_json::json!({
                        "headers": { "req_id": req_id },
                        "errcode": 0,
                        "errmsg": "ok"
                    });
                    write
                        .send(WsMessage::Text(ack.to_string()))
                        .await
                        .map_err(|e| {
                            Error::Channel(format!("WeCom long connection event ack failed: {}", e))
                        })?;
                }
            }
            "ping" => {
                write
                    .send(WsMessage::Text("{\"cmd\":\"pong\"}".to_string()))
                    .await
                    .map_err(|e| Error::Channel(format!("WeCom pong send failed: {}", e)))?;
            }
            "pong" => {}
            other => {
                // WeCom sends empty-cmd acks (heartbeat responses, subscribe acks, etc.).
                // Only warn if the payload actually contains an error code.
                let errcode = envelope.errcode.unwrap_or(0);
                if errcode != 0 {
                    warn!(cmd = %other, errcode, payload = %text, "WeCom long connection: received error response");
                } else {
                    debug!(cmd = %other, "WeCom long connection: received ack");
                }
            }
        }

        Ok(())
    }

    async fn build_inbound_from_long_connection(
        &self,
        body: &serde_json::Value,
    ) -> Result<Option<InboundMessage>> {
        let msg: LongConnMsgBody = serde_json::from_value(body.clone())
            .map_err(|e| Error::Channel(format!("Failed to parse WeCom long body: {}", e)))?;

        let from_ref = msg.from.as_ref();
        let from_user = from_ref.map(|v| v.userid.clone()).unwrap_or_default();
        if from_user.is_empty() || from_user.starts_with('@') {
            return Ok(None);
        }
        if !self.is_allowed(&from_user) {
            debug!(from_user = %from_user, "WeCom long connection: user not in allowlist");
            return Ok(None);
        }

        if !msg.msgid.is_empty() {
            let mut seen = SEEN_MSG_IDS.lock().unwrap();
            if seen.contains(&msg.msgid) {
                debug!(msg_id = %msg.msgid, "WeCom long connection: duplicate msg_id, skipping");
                return Ok(None);
            }
            if seen.len() >= SEEN_MSG_IDS_MAX {
                seen.clear();
            }
            seen.insert(msg.msgid.clone());
        }

        let (content, media, pending) = match msg.msgtype.as_str() {
            "text" => {
                let content = msg
                    .text
                    .as_ref()
                    .map(|t| t.content.trim().to_string())
                    .unwrap_or_default();
                if content.is_empty() {
                    return Ok(None);
                }
                (content, vec![], false)
            }
            "image" => {
                let image = msg.image.unwrap_or_default();
                let mut media = vec![];
                if !image.url.is_empty() && !image.aeskey.is_empty() {
                    match download_and_decrypt_longconn_media(
                        &self.client,
                        &image.url,
                        &image.aeskey,
                        "image",
                        None,
                    )
                    .await
                    {
                        Ok(path) => media.push(path),
                        Err(e) => {
                            warn!(error = %e, "WeCom long connection: failed to download image")
                        }
                    }
                }
                (
                    "用户发来了一张图片，请问您需要我做什么？（例如：描述图片内容、识别文字、发回给您等）".to_string(),
                    media,
                    true,
                )
            }
            "mixed" => {
                let mixed = msg.mixed.unwrap_or_default();
                let summary = build_mixed_summary(&mixed);
                if summary.is_empty() {
                    return Ok(None);
                }
                (summary, vec![], false)
            }
            "voice" => {
                let voice = msg.voice.unwrap_or_default();
                let mut media = vec![];
                if !voice.url.is_empty() && !voice.aeskey.is_empty() {
                    match download_and_decrypt_longconn_media(
                        &self.client,
                        &voice.url,
                        &voice.aeskey,
                        "voice",
                        Some("amr"),
                    )
                    .await
                    {
                        Ok(path) => media.push(path),
                        Err(e) => {
                            warn!(error = %e, "WeCom long connection: failed to download voice")
                        }
                    }
                }
                let content = if let Some(recognition) =
                    voice.recognition.filter(|s| !s.trim().is_empty())
                {
                    format!("用户发来一条语音，企业微信转写文本：{}", recognition.trim())
                } else {
                    "用户发来了一条语音消息，请先用 audio_transcribe 工具转写，然后根据转写内容回复用户。".to_string()
                };
                (content, media, false)
            }
            "file" => {
                let file = msg.file.unwrap_or_default();
                let mut media = vec![];
                if !file.url.is_empty() && !file.aeskey.is_empty() {
                    match download_and_decrypt_longconn_media(
                        &self.client,
                        &file.url,
                        &file.aeskey,
                        "file",
                        file.filename.as_deref().and_then(|n| n.rsplit('.').next()),
                    )
                    .await
                    {
                        Ok(path) => media.push(path),
                        Err(e) => {
                            warn!(error = %e, "WeCom long connection: failed to download file")
                        }
                    }
                }
                let desc = match file.filename.as_deref() {
                    Some(name) if !name.is_empty() => format!(
                        "用户发来了文件「{}」，请问您需要我做什么？（例如：读取内容、分析数据等）",
                        name
                    ),
                    _ => "用户发来了一个文件，请问您需要我做什么？（例如：读取内容、分析数据等）"
                        .to_string(),
                };
                (desc, media, true)
            }
            other => {
                debug!(msg_type = %other, "WeCom long connection: unsupported message type");
                return Ok(None);
            }
        };

        Ok(Some(InboundMessage {
            channel: "wecom".to_string(),
            account_id: wecom_account_id(&self.config),
            sender_id: from_user.clone(),
            chat_id: if msg.chatid.is_empty() {
                from_user.clone()
            } else {
                msg.chatid.clone()
            },
            content,
            media,
            metadata: serde_json::json!({
                "msg_id": msg.msgid,
                "msg_type": msg.msgtype,
                "mode": "long_connection",
                "chat_type": msg.chattype,
                "aibot_id": msg.aibotid,
                "media_pending_intent": pending,
                "sender_nick": from_ref.and_then(|f| f.nickname.clone()),
            }),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        }))
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
            account_id: wecom_account_id(&self.config),
            sender_id: from_user.clone(),
            chat_id: if to_party.is_empty() {
                from_user
            } else {
                to_party
            },
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
        let mut parts = [token, timestamp, nonce];
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

        let mode = self.config.channels.wecom.mode.trim().to_lowercase();
        info!(
            mode = %mode,
            corp_id = %self.config.channels.wecom.corp_id,
            agent_id = self.config.channels.wecom.agent_id,
            bot_id = %self.config.channels.wecom.bot_id,
            ws_url = %self.ws_url(),
            "WeCom run_loop entered"
        );
        if mode == "long_connection" || mode == "long-connection" || mode == "stream" {
            if self.config.channels.wecom.bot_id.trim().is_empty()
                || self.config.channels.wecom.bot_secret.trim().is_empty()
            {
                warn!("WeCom long_connection requires bot_id and bot_secret");
                return;
            }
            self.run_long_connection(shutdown).await;
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

        match self.get_access_token().await {
            Ok(_) => info!("WeCom access token obtained successfully"),
            Err(e) => {
                error!(error = %e.to_string(), "WeCom: failed to get access token, channel will not start");
                return;
            }
        }

        self.run_polling(shutdown).await;
    }
}

fn build_mixed_summary(mixed: &LongConnMixed) -> String {
    let parts: Vec<String> = mixed
        .items
        .iter()
        .filter_map(|item| match item.item_type.as_str() {
            "text" => item
                .content
                .as_ref()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            "image" => Some("[图片]".to_string()),
            "link" => Some("[链接]".to_string()),
            "file" => Some("[文件]".to_string()),
            other if !other.is_empty() => Some(format!("[{}]", other)),
            _ => None,
        })
        .collect();
    parts.join(" ")
}

async fn download_and_decrypt_longconn_media(
    client: &Client,
    url: &str,
    aeskey: &str,
    media_type: &str,
    ext_hint: Option<&str>,
) -> Result<String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("WeCom long media download failed: {}", e)))?;
    if !resp.status().is_success() {
        return Err(Error::Channel(format!(
            "WeCom long media download HTTP {}",
            resp.status()
        )));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Error::Channel(format!("WeCom long media read failed: {}", e)))?;
    let plain = decrypt_longconn_media_bytes(&bytes, aeskey)?;
    let media_dir = dirs::home_dir()
        .map(|h| h.join(".blockcell").join("workspace").join("media"))
        .unwrap_or_else(|| PathBuf::from(".blockcell/workspace/media"));
    tokio::fs::create_dir_all(&media_dir)
        .await
        .map_err(|e| Error::Channel(format!("Failed to create media dir: {}", e)))?;
    let ext = ext_hint
        .map(|s| s.to_string())
        .unwrap_or_else(|| ext_from_content_type(&content_type, media_type).to_string());
    let filename = format!(
        "wecom_long_{}_{}.{}",
        media_type,
        chrono::Utc::now().timestamp_millis(),
        ext
    );
    let path = media_dir.join(filename);
    tokio::fs::write(&path, &plain)
        .await
        .map_err(|e| Error::Channel(format!("WeCom long media write failed: {}", e)))?;
    Ok(path.to_string_lossy().to_string())
}

fn decrypt_longconn_media_bytes(ciphertext: &[u8], aeskey: &str) -> Result<Vec<u8>> {
    let key = general_purpose::STANDARD
        .decode(aeskey)
        .or_else(|_| {
            let padded = match aeskey.len() % 4 {
                2 => format!("{}==", aeskey),
                3 => format!("{}=", aeskey),
                _ => aeskey.to_string(),
            };
            general_purpose::STANDARD.decode(padded)
        })
        .map_err(|e| Error::Channel(format!("WeCom long media aeskey decode failed: {}", e)))?;
    if key.len() != 32 {
        return Err(Error::Channel(format!(
            "WeCom long media aeskey invalid length: {}",
            key.len()
        )));
    }
    use aes::cipher::block_padding::Pkcs7;
    let iv = &key[..16];
    let decryptor = Aes256CbcDec::new_from_slices(&key, iv)
        .map_err(|e| Error::Channel(format!("WeCom long media decryptor init failed: {}", e)))?;
    let mut buf = ciphertext.to_vec();
    let plain = decryptor
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| Error::Channel(format!("WeCom long media decrypt failed: {}", e)))?;
    Ok(plain.to_vec())
}

/// Percent-decode a URL query parameter value (%2B → +, %2F → /, %3D → =, etc.).
/// Does NOT treat '+' as space (that's form-encoding, not used by WeCom).
fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
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
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);

        #[allow(clippy::needless_range_loop)]
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
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
        result[i * 4..i * 4 + 4].copy_from_slice(&bytes);
    }
    result
}

// ── send_message ──────────────────────────────────────────────────────────────

/// Handle a WeCom webhook request.
///
/// WeCom sends two types of requests to the callback URL:
fn resolve_wecom_webhook_config(
    config: &Config,
    method: &str,
    query: &std::collections::HashMap<String, String>,
    body: &str,
) -> Config {
    let listeners = wecom_listener_configs(config);
    if listeners.is_empty() {
        return config.clone();
    }
    if listeners.len() == 1 {
        return listeners[0].config.clone();
    }

    let timestamp = query.get("timestamp").map(|s| s.as_str()).unwrap_or("");
    let nonce = query.get("nonce").map(|s| s.as_str()).unwrap_or("");
    let msg_signature = query
        .get("msg_signature")
        .or_else(|| query.get("signature"))
        .map(|s| s.as_str())
        .unwrap_or("");

    let signed_payload = if method == "GET" {
        query
            .get("echostr")
            .map(|s| percent_decode(s))
            .unwrap_or_default()
    } else {
        extract_xml_tag(body, "Encrypt").unwrap_or_default()
    };

    if !msg_signature.is_empty() && !signed_payload.is_empty() {
        for listener in &listeners {
            let token = listener.config.channels.wecom.callback_token.as_str();
            if token.is_empty() {
                continue;
            }
            if verify_signature_4(token, timestamp, nonce, &signed_payload, msg_signature) {
                return listener.config.clone();
            }
        }
    }

    config.clone()
}

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
    let resolved_config = resolve_wecom_webhook_config(config, method, query, body);
    let wecom_cfg = &resolved_config.channels.wecom;

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
        let msg_signature = query
            .get("msg_signature")
            .or_else(|| query.get("signature"))
            .map(|s| s.as_str())
            .unwrap_or("");
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
            let mut parts = [
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
    let msg_signature = query
        .get("msg_signature")
        .or_else(|| query.get("signature"))
        .map(|s| s.as_str())
        .unwrap_or("");

    if !wecom_cfg.callback_token.is_empty()
        && !msg_encrypt.is_empty()
        && !verify_signature_4(
            &wecom_cfg.callback_token,
            timestamp,
            nonce,
            &msg_encrypt,
            msg_signature,
        )
    {
        tracing::warn!("WeCom webhook: POST signature verification failed");
        return (403, "Forbidden: invalid signature".to_string());
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

    // Filter out messages sent by the bot itself — WeCom echoes bot-sent messages back
    // as callbacks. Bot messages have FromUserName starting with '@' (e.g. @app, @all)
    // or are event-type messages with no real user sender.
    if from_user.starts_with('@') {
        tracing::debug!(from_user = %from_user, "WeCom webhook: skipping bot/system message");
        return (200, "success".to_string());
    }

    // msg_id dedup — WeCom may retry the same webhook on timeout, or echo bot messages.
    // Events (msg_type=event) have no MsgId; only deduplicate real messages.
    if let Some(ref id) = msg_id {
        if !id.is_empty() {
            let mut seen = SEEN_MSG_IDS.lock().unwrap();
            if seen.contains(id.as_str()) {
                tracing::debug!(msg_id = %id, "WeCom webhook: duplicate msg_id, skipping");
                return (200, "success".to_string());
            }
            // Evict oldest entries if set is too large
            if seen.len() >= SEEN_MSG_IDS_MAX {
                seen.clear();
            }
            seen.insert(id.clone());
        }
    }

    // Allowlist check (applies to all message types)
    let allow_from = &wecom_cfg.allow_from;
    if !allow_from.is_empty() && !allow_from.iter().any(|a| a == &from_user) {
        tracing::debug!(from_user = %from_user, "WeCom webhook: user not in allowlist");
        return (200, "success".to_string());
    }

    // Determine text content, optional media paths, and whether to await user intent
    // before processing (true = channel already sent ack, agent should ask what to do)
    let (final_content, media_paths, media_pending_intent) = match msg_type.as_str() {
        "text" => {
            let c = content.trim().to_string();
            if c.is_empty() {
                return (200, "success".to_string());
            }
            (c, vec![], false)
        }
        "image" => {
            let media_id = extract_xml_tag(&decrypted_body, "MediaId").unwrap_or_default();
            let pic_url = extract_xml_tag(&decrypted_body, "PicUrl").unwrap_or_default();
            info!(media_id = %media_id, "WeCom webhook: received image");
            let paths = if !media_id.is_empty() {
                match download_wecom_media(&resolved_config, &media_id, "image", None).await {
                    Ok(p) => vec![p],
                    Err(e) => {
                        warn!(error = %e.to_string(), "WeCom: failed to download image, using PicUrl");
                        if !pic_url.is_empty() {
                            vec![pic_url]
                        } else {
                            vec![]
                        }
                    }
                }
            } else if !pic_url.is_empty() {
                vec![pic_url]
            } else {
                vec![]
            };
            ("用户发来了一张图片，请问您需要我做什么？（例如：描述图片内容、识别文字、发回给您等）".to_string(), paths, true)
        }
        "voice" => {
            let media_id = extract_xml_tag(&decrypted_body, "MediaId").unwrap_or_default();
            let format =
                extract_xml_tag(&decrypted_body, "Format").unwrap_or_else(|| "amr".to_string());
            info!(media_id = %media_id, format = %format, "WeCom webhook: received voice");
            let paths = if !media_id.is_empty() {
                match download_wecom_media(config, &media_id, "voice", Some(&format)).await {
                    Ok(p) => vec![p],
                    Err(e) => {
                        warn!(error = %e.to_string(), "WeCom: failed to download voice");
                        vec![]
                    }
                }
            } else {
                vec![]
            };
            // Send immediate ack
            if !from_user.is_empty() {
                let _ = send_message(config, &from_user, "🎤 语音已收到，正在转写...").await;
            }
            // Voice: always transcribe immediately, no pending intent needed
            ("用户发来了一条语音消息，请先用 audio_transcribe 工具转写，然后根据转写内容回复用户。".to_string(), paths, false)
        }
        "video" | "shortvideo" => {
            let media_id = extract_xml_tag(&decrypted_body, "MediaId").unwrap_or_default();
            info!(media_id = %media_id, "WeCom webhook: received video");
            let paths = if !media_id.is_empty() {
                match download_wecom_media(&resolved_config, &media_id, "video", Some("mp4")).await
                {
                    Ok(p) => vec![p],
                    Err(e) => {
                        warn!(error = %e.to_string(), "WeCom: failed to download video");
                        vec![]
                    }
                }
            } else {
                vec![]
            };
            (
                "用户发来了一个视频，请问您需要我做什么？（例如：提取音频、截取片段等）"
                    .to_string(),
                paths,
                true,
            )
        }
        "file" => {
            let media_id = extract_xml_tag(&decrypted_body, "MediaId").unwrap_or_default();
            let file_name = extract_xml_tag(&decrypted_body, "FileName").unwrap_or_default();
            let ext = file_name.rsplit('.').next().map(|s| s.to_string());
            info!(media_id = %media_id, file_name = %file_name, "WeCom webhook: received file");
            let paths = if !media_id.is_empty() {
                match download_wecom_media(&resolved_config, &media_id, "file", ext.as_deref())
                    .await
                {
                    Ok(p) => vec![p],
                    Err(e) => {
                        warn!(error = %e.to_string(), "WeCom: failed to download file");
                        vec![]
                    }
                }
            } else {
                vec![]
            };
            let desc = if file_name.is_empty() {
                "用户发来了一个文件，请问您需要我做什么？（例如：读取内容、分析数据等）".to_string()
            } else {
                format!(
                    "用户发来了文件「{}」，请问您需要我做什么？（例如：读取内容、分析数据等）",
                    file_name
                )
            };
            (desc, paths, true)
        }
        "location" => {
            let lat = extract_xml_tag(&decrypted_body, "Location_X").unwrap_or_default();
            let lon = extract_xml_tag(&decrypted_body, "Location_Y").unwrap_or_default();
            let label = extract_xml_tag(&decrypted_body, "Label").unwrap_or_default();
            let c = if label.is_empty() {
                format!("[位置] 纬度:{} 经度:{}", lat, lon)
            } else {
                format!("[位置] {} (纬度:{} 经度:{})", label, lat, lon)
            };
            (c, vec![], false)
        }
        "link" => {
            let title = extract_xml_tag(&decrypted_body, "Title").unwrap_or_default();
            let url = extract_xml_tag(&decrypted_body, "Url").unwrap_or_default();
            let c = format!("[链接] {} {}", title, url);
            (c, vec![], false)
        }
        other => {
            info!(msg_type = %other, "WeCom webhook: unsupported message type, skipping");
            return (200, "success".to_string());
        }
    };

    if let Some(tx) = inbound_tx {
        let inbound = blockcell_core::InboundMessage {
            channel: "wecom".to_string(),
            account_id: wecom_account_id(&resolved_config),
            sender_id: from_user.clone(),
            chat_id: from_user.clone(),
            content: final_content,
            media: media_paths,
            metadata: serde_json::json!({
                "msg_id": msg_id,
                "msg_type": msg_type,
                "mode": "webhook",
                "media_pending_intent": media_pending_intent,
            }),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };
        if let Err(e) = tx.send(inbound).await {
            tracing::error!(error = %e, "WeCom webhook: failed to forward inbound message");
        }
    }

    (200, "success".to_string())
}

/// Download a WeCom media file (image/voice/video/file) by media_id.
/// Saves to `~/.blockcell/media/wecom_{media_id}.{ext}` and returns the local path.
async fn download_wecom_media(
    config: &Config,
    media_id: &str,
    media_type: &str,
    ext_hint: Option<&str>,
) -> Result<String> {
    let client = shared_client();
    let mut retried = false;
    let resp = loop {
        let token = fetch_access_token_static(&client, config).await?;
        let url = format!(
            "{}/media/get?access_token={}&media_id={}",
            WECOM_API_BASE, token, media_id
        );
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("WeCom media/get request failed: {}", e)))?;

        if let Some(ct) = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
        {
            if ct.contains("application/json") {
                let err: WeComResponse = resp
                    .json()
                    .await
                    .map_err(|e| Error::Channel(format!("WeCom media/get parse failed: {}", e)))?;
                if err.is_invalid_token() && !retried {
                    warn!(errcode = err.errcode, errmsg = %err.errmsg, "WeCom media/get token invalid, refreshing and retrying once");
                    clear_access_token_cache().await;
                    retried = true;
                    continue;
                }
                return Err(Error::Channel(format!(
                    "WeCom media/get error {}: {}",
                    err.errcode, err.errmsg
                )));
            }
        }

        break resp;
    };

    if !resp.status().is_success() {
        return Err(Error::Channel(format!(
            "WeCom media/get HTTP {}",
            resp.status()
        )));
    }

    // Determine file extension from Content-Type or hint
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let ext = ext_hint
        .map(|s| s.to_string())
        .unwrap_or_else(|| ext_from_content_type(&content_type, media_type).to_string());

    let media_dir = dirs::home_dir()
        .map(|h| h.join(".blockcell").join("workspace").join("media"))
        .unwrap_or_else(|| PathBuf::from(".blockcell/workspace/media"));
    tokio::fs::create_dir_all(&media_dir)
        .await
        .map_err(|e| Error::Channel(format!("Failed to create media dir: {}", e)))?;

    let filename = format!(
        "wecom_{}_{}.{}",
        media_type,
        &media_id[..media_id.len().min(16)],
        ext
    );
    let file_path = media_dir.join(&filename);

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Error::Channel(format!("WeCom media/get read body failed: {}", e)))?;

    tokio::fs::write(&file_path, &bytes)
        .await
        .map_err(|e| Error::Channel(format!("WeCom media/get write failed: {}", e)))?;

    let path_str = file_path.to_string_lossy().to_string();
    info!(path = %path_str, bytes = bytes.len(), "WeCom: media downloaded");
    Ok(path_str)
}

fn ext_from_content_type(content_type: &str, media_type: &str) -> &'static str {
    if content_type.contains("jpeg") || content_type.contains("jpg") {
        return "jpg";
    }
    if content_type.contains("png") {
        return "png";
    }
    if content_type.contains("gif") {
        return "gif";
    }
    if content_type.contains("mp4") {
        return "mp4";
    }
    if content_type.contains("amr") {
        return "amr";
    }
    if content_type.contains("speex") {
        return "speex";
    }
    match media_type {
        "image" => "jpg",
        "voice" => "amr",
        "video" => "mp4",
        _ => "bin",
    }
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
        &content[9..content.len() - 3]
    } else {
        content
    };
    Some(content.to_string())
}

/// Verify WeCom 4-param signature: SHA1(sort(token, timestamp, nonce, msg_encrypt))
/// This is the correct signature for both GET (echostr) and POST (Encrypt) callbacks.
fn verify_signature_4(
    token: &str,
    timestamp: &str,
    nonce: &str,
    msg_encrypt: &str,
    expected: &str,
) -> bool {
    let mut parts = [token, timestamp, nonce, msg_encrypt];
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
fn decrypt_wecom_msg(
    msg_encrypt: &str,
    encoding_aes_key: &str,
) -> std::result::Result<String, String> {
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
    let key_bytes = LENIENT.decode(&padded_key).map_err(|e| {
        format!(
            "Failed to decode EncodingAESKey: {}. Key was: '{}'",
            e, padded_key
        )
    })?;
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
    let ciphertext = general_purpose::STANDARD.decode(msg_encrypt).map_err(|e| {
        format!(
            "Failed to decode msg_encrypt (len={}): {}. Value was: '{}'",
            msg_encrypt.len(),
            e,
            msg_encrypt
        )
    })?;

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
        return Err(format!(
            "Decrypted data too short: {} bytes",
            plaintext.len()
        ));
    }

    let msg_len =
        u32::from_be_bytes([plaintext[16], plaintext[17], plaintext[18], plaintext[19]]) as usize;

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

/// Upload a local file to WeCom as a temporary media asset.
/// Returns the `media_id` (valid for 3 days).
/// `media_type` must be one of: image / voice / video / file
pub async fn upload_media(config: &Config, file_path: &str, media_type: &str) -> Result<String> {
    let client = shared_client();
    let path = std::path::Path::new(file_path);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| Error::Channel(format!("Failed to read media file {}: {}", file_path, e)))?;

    let mime = mime_for_path(file_path);
    #[derive(Deserialize)]
    struct UploadResp {
        errcode: i32,
        errmsg: String,
        #[serde(default)]
        media_id: Option<String>,
    }

    let mut retried = false;
    let result: UploadResp = loop {
        let token = fetch_access_token_static(&client, config).await?;
        let url = format!(
            "{}/media/upload?access_token={}&type={}",
            WECOM_API_BASE, token, media_type
        );
        let part = reqwest::multipart::Part::bytes(bytes.clone())
            .file_name(file_name.clone())
            .mime_str(mime)
            .map_err(|e| Error::Channel(format!("Invalid MIME type: {}", e)))?;
        let form = reqwest::multipart::Form::new().part("media", part);
        let resp = client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("WeCom media/upload failed: {}", e)))?;
        let result: UploadResp = resp
            .json()
            .await
            .map_err(|e| Error::Channel(format!("WeCom media/upload parse failed: {}", e)))?;
        if matches!(result.errcode, 40014 | 42001) && !retried {
            warn!(errcode = result.errcode, errmsg = %result.errmsg, "WeCom media/upload token invalid, refreshing and retrying once");
            clear_access_token_cache().await;
            retried = true;
            continue;
        }
        break result;
    };

    if result.errcode != 0 {
        return Err(Error::Channel(format!(
            "WeCom media/upload error {}: {}",
            result.errcode, result.errmsg
        )));
    }

    result
        .media_id
        .ok_or_else(|| Error::Channel("WeCom media/upload: no media_id in response".to_string()))
}

fn mime_for_path(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "amr" => "audio/amr",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "zip" => "application/zip",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Send a media message (image/voice/video/file) to a WeCom user or group.
/// `file_path` is a local file path.  `caption` is an optional text shown alongside the image
/// (long_connection mode only; ignored for REST API mode).
pub async fn send_media_message(
    config: &Config,
    chat_id: &str,
    file_path: &str,
    _caption: &str,
) -> Result<()> {
    // Long connection (AI bot) mode does not support sending any media files.
    let mode = config.channels.wecom.mode.trim().to_lowercase();
    if mode == "long_connection" || mode == "long-connection" || mode == "stream" {
        return send_message(
            config,
            chat_id,
            "企业微信长连接机器人暂不支持发送图片或文件，请使用文字回复。",
        )
        .await;
    }

    crate::rate_limit::wecom_limiter().acquire().await;

    let ext = file_path.rsplit('.').next().unwrap_or("").to_lowercase();
    let (mut media_type, mut msg_type) = media_type_for_ext(&ext);

    let upload_path = if media_type == "voice" {
        let amr_path = ensure_wecom_voice_amr(file_path).await?;
        // WeCom voice messages have a 60-second limit. If the audio is longer,
        // fall back to sending as a file so the user still gets the full audio.
        let duration = probe_audio_duration(&amr_path).await.unwrap_or(0.0);
        if duration > 60.0 {
            info!(duration = %duration, "WeCom: voice too long (>60s), sending as file instead");
            media_type = "file";
            msg_type = "file";
            file_path.to_string() // send original file (mp3), not the AMR
        } else {
            amr_path
        }
    } else {
        file_path.to_string()
    };

    info!(file_path = %upload_path, media_type = %media_type, "WeCom: uploading media");
    let media_id = upload_media(config, &upload_path, media_type).await?;
    info!(media_id = %media_id, "WeCom: media uploaded");

    let client = shared_client();
    let token = fetch_access_token_static(&client, config).await?;
    let agent_id = config.channels.wecom.agent_id;

    let is_group = chat_id.starts_with("wr") || chat_id.starts_with("WR");

    let body = if is_group {
        build_media_body_group(chat_id, msg_type, &media_id)
    } else {
        build_media_body_user(chat_id, agent_id, msg_type, &media_id)
    };

    let endpoint = if is_group {
        format!("{}/appchat/send", WECOM_API_BASE)
    } else {
        format!("{}/message/send", WECOM_API_BASE)
    };

    let resp = client
        .post(&endpoint)
        .query(&[("access_token", token.as_str())])
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("WeCom send media failed: {}", e)))?;

    let result: WeComResponse = resp
        .json()
        .await
        .map_err(|e| Error::Channel(format!("WeCom send media parse failed: {}", e)))?;

    if result.errcode != 0 {
        return Err(Error::Channel(format!(
            "WeCom send media error {}: {}",
            result.errcode, result.errmsg
        )));
    }

    Ok(())
}

async fn ensure_wecom_voice_amr(file_path: &str) -> Result<String> {
    let ext = file_path.rsplit('.').next().unwrap_or("").to_lowercase();
    if ext == "amr" {
        return Ok(file_path.to_string());
    }

    let input = std::path::Path::new(file_path);
    if !input.exists() {
        return Err(Error::Channel(format!(
            "WeCom voice: input file not found: {}",
            file_path
        )));
    }

    let media_dir = dirs::home_dir()
        .map(|h| h.join(".blockcell").join("workspace").join("media"))
        .unwrap_or_else(|| PathBuf::from(".blockcell/workspace/media"));
    tokio::fs::create_dir_all(&media_dir)
        .await
        .map_err(|e| Error::Channel(format!("Failed to create media dir: {}", e)))?;

    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("voice");
    let ts = chrono::Utc::now().timestamp_millis();
    let output = media_dir.join(format!("{}_{}.amr", stem, ts));

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .arg("-i")
        .arg(input)
        .arg("-ac")
        .arg("1")
        .arg("-ar")
        .arg("8000")
        .arg("-c:a")
        .arg("amr_nb")
        .arg(&output);

    let out = cmd.output().await.map_err(|e| {
        Error::Channel(format!(
            "WeCom voice: ffmpeg not available or failed to start: {}",
            e
        ))
    })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return Err(Error::Channel(format!(
            "WeCom voice: failed to convert to amr (WeCom voice only supports .amr). ffmpeg stderr: {}",
            stderr
        )));
    }

    let output_str = output.to_string_lossy().to_string();
    if !std::path::Path::new(&output_str).exists() {
        return Err(Error::Channel(
            "WeCom voice: conversion succeeded but output file missing".to_string(),
        ));
    }

    Ok(output_str)
}

/// Probe audio duration in seconds using ffprobe. Returns None on failure.
async fn probe_audio_duration(file_path: &str) -> Option<f64> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(file_path)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    text.trim().parse::<f64>().ok()
}

fn media_type_for_ext(ext: &str) -> (&'static str, &'static str) {
    match ext {
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" => ("image", "image"),
        "amr" | "mp3" | "wav" | "m4a" | "speex" => ("voice", "voice"),
        "mp4" | "avi" | "mov" | "mkv" => ("video", "video"),
        _ => ("file", "file"),
    }
}

fn build_media_body_user(
    to_user: &str,
    agent_id: i64,
    msg_type: &str,
    media_id: &str,
) -> serde_json::Value {
    match msg_type {
        "image" => serde_json::json!({
            "touser": to_user,
            "msgtype": "image",
            "agentid": agent_id,
            "image": { "media_id": media_id },
            "safe": 0
        }),
        "voice" => serde_json::json!({
            "touser": to_user,
            "msgtype": "voice",
            "agentid": agent_id,
            "voice": { "media_id": media_id },
            "safe": 0
        }),
        "video" => serde_json::json!({
            "touser": to_user,
            "msgtype": "video",
            "agentid": agent_id,
            "video": { "media_id": media_id, "title": "", "description": "" },
            "safe": 0
        }),
        _ => serde_json::json!({
            "touser": to_user,
            "msgtype": "file",
            "agentid": agent_id,
            "file": { "media_id": media_id },
            "safe": 0
        }),
    }
}

fn build_media_body_group(chat_id: &str, msg_type: &str, media_id: &str) -> serde_json::Value {
    match msg_type {
        "image" => serde_json::json!({
            "chatid": chat_id,
            "msgtype": "image",
            "image": { "media_id": media_id },
            "safe": 0
        }),
        "voice" => serde_json::json!({
            "chatid": chat_id,
            "msgtype": "voice",
            "voice": { "media_id": media_id },
            "safe": 0
        }),
        "video" => serde_json::json!({
            "chatid": chat_id,
            "msgtype": "video",
            "video": { "media_id": media_id, "title": "", "description": "" },
            "safe": 0
        }),
        _ => serde_json::json!({
            "chatid": chat_id,
            "msgtype": "file",
            "file": { "media_id": media_id },
            "safe": 0
        }),
    }
}

/// Send a text message to a WeCom user or group.
/// `chat_id` can be a user_id (touser) or a group chat_id (chatid).
pub async fn send_message(config: &Config, chat_id: &str, text: &str) -> Result<()> {
    // Long connection mode: route reply via the active WebSocket instead of REST API.
    let mode = config.channels.wecom.mode.trim().to_lowercase();
    if mode == "long_connection" || mode == "long-connection" || mode == "stream" {
        let bot_id = config.channels.wecom.bot_id.trim().to_string();
        let registry = LONGCONN_REGISTRY.lock().unwrap();
        if let Some(tx) = registry.get(&bot_id) {
            let chunks = split_message(text, WECOM_MSG_LIMIT);
            for chunk in chunks {
                let msg = LongConnOutbound::Text {
                    chat_id: chat_id.to_string(),
                    content: chunk,
                };
                if let Err(e) = tx.try_send(msg) {
                    warn!(error = %e, bot_id = %bot_id, "WeCom longconn: failed to queue outbound message");
                }
            }
        } else {
            warn!(bot_id = %bot_id, "WeCom long connection not active; outbound message dropped");
        }
        return Ok(());
    }

    crate::rate_limit::wecom_limiter().acquire().await;

    let client = shared_client();
    let chunks = split_message(text, WECOM_MSG_LIMIT);
    for (i, chunk) in chunks.iter().enumerate() {
        do_send_message(&client, config, chat_id, chunk).await?;
        if i + 1 < chunks.len() {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
    Ok(())
}

async fn fetch_access_token_static(client: &Client, config: &Config) -> Result<String> {
    let mut cache = WECOM_TOKEN_CACHE.lock().await;
    if cache.is_valid() {
        return Ok(cache.token.clone());
    }

    let corp_id = &config.channels.wecom.corp_id;
    let corp_secret = &config.channels.wecom.corp_secret;

    let resp = client
        .get(format!("{}/gettoken", WECOM_API_BASE))
        .query(&[
            ("corpid", corp_id.as_str()),
            ("corpsecret", corp_secret.as_str()),
        ])
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

    let token = body
        .access_token
        .ok_or_else(|| Error::Channel("No access_token in WeCom response".to_string()))?;
    let expires_in = body.expires_in.unwrap_or(7200);
    cache.token = token.clone();
    cache.expires_at = chrono::Utc::now().timestamp() + expires_in;
    info!("WeCom access_token refreshed (expires in {}s)", expires_in);
    Ok(token)
}

async fn clear_access_token_cache() {
    let mut cache = WECOM_TOKEN_CACHE.lock().await;
    cache.token.clear();
    cache.expires_at = 0;
}

async fn do_send_message(
    client: &Client,
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

    let mut retried = false;
    let result: WeComResponse = loop {
        let token = fetch_access_token_static(client, config).await?;
        let resp = client
            .post(&endpoint)
            .query(&[("access_token", token.as_str())])
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Failed to send WeCom message: {}", e)))?;

        let result: WeComResponse = resp
            .json()
            .await
            .map_err(|e| Error::Channel(format!("Failed to parse WeCom send response: {}", e)))?;
        if result.is_invalid_token() && !retried {
            warn!(errcode = result.errcode, errmsg = %result.errmsg, "WeCom send token invalid, refreshing and retrying once");
            clear_access_token_cache().await;
            retried = true;
            continue;
        }
        break result;
    };

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
}
