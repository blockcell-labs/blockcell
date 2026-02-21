use futures::{SinkExt, StreamExt};
use blockcell_core::{Config, Error, InboundMessage, Result};
use prost::Message as _;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use tracing::{debug, error, info, warn};

/// Feishu WebSocket Protobuf frame (matches pbbp2.proto)
#[derive(Clone, prost::Message)]
struct Frame {
    #[prost(uint64, tag = "1")]
    seq_id: u64,
    #[prost(uint64, tag = "2")]
    log_id: u64,
    #[prost(uint32, tag = "3")]
    service: u32,
    #[prost(uint32, tag = "4")]
    method: u32,
    #[prost(message, repeated, tag = "5")]
    headers: Vec<FrameHeader>,
    #[prost(string, tag = "6")]
    payload_encoding: String,
    #[prost(string, tag = "7")]
    payload_type: String,
    #[prost(bytes = "vec", tag = "8")]
    payload: Vec<u8>,
    #[prost(string, tag = "9")]
    log_id_new: String,
}

#[derive(Clone, prost::Message)]
struct FrameHeader {
    #[prost(string, tag = "1")]
    key: String,
    #[prost(string, tag = "2")]
    value: String,
}

/// method=0 → Control frame, method=1 → Data frame
const FRAME_METHOD_CONTROL: u32 = 0;
const FRAME_METHOD_DATA: u32 = 1;
/// type header values
const MSG_TYPE_PING: &str = "ping";
const MSG_TYPE_PONG: &str = "pong";
#[allow(dead_code)]
const MSG_TYPE_EVENT: &str = "event";

const FEISHU_OPEN_API: &str = "https://open.feishu.cn/open-apis";
const FEISHU_BASE: &str = "https://open.feishu.cn";
/// Refresh token 5 minutes before expiry.
const TOKEN_REFRESH_MARGIN_SECS: i64 = 300;

/// Cached tenant access token with expiry timestamp.
#[derive(Default)]
struct CachedToken {
    token: String,
    expires_at: i64, // Unix timestamp (seconds)
}

impl CachedToken {
    fn is_valid(&self) -> bool {
        !self.token.is_empty()
            && chrono::Utc::now().timestamp() < self.expires_at - TOKEN_REFRESH_MARGIN_SECS
    }
}

/// Process-global token cache for the free `send_message` function.
static GLOBAL_TOKEN_CACHE: OnceLock<Mutex<CachedToken>> = OnceLock::new();

fn global_token_cache() -> &'static Mutex<CachedToken> {
    GLOBAL_TOKEN_CACHE.get_or_init(|| Mutex::new(CachedToken::default()))
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    code: i32,
    msg: String,
    tenant_access_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WsEndpointResponse {
    code: i32,
    msg: String,
    data: Option<WsEndpointData>,
}

#[derive(Debug, Deserialize)]
struct WsEndpointData {
    #[serde(rename = "URL")]
    url: String,
}

#[derive(Debug, Deserialize)]
struct FeishuEvent {
    #[serde(default)]
    header: Option<EventHeader>,
    #[serde(default)]
    event: Option<EventBody>,
}

#[derive(Debug, Deserialize)]
struct EventHeader {
    event_id: String,
    event_type: String,
}

#[derive(Debug, Deserialize)]
struct EventBody {
    #[serde(default)]
    message: Option<MessageEvent>,
    #[serde(default)]
    sender: Option<SenderInfo>,
}

#[derive(Debug, Deserialize)]
struct MessageEvent {
    message_id: String,
    chat_id: String,
    message_type: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct SenderInfo {
    sender_id: Option<SenderId>,
    sender_type: String,
}

#[derive(Debug, Deserialize)]
struct SenderId {
    open_id: String,
}

#[derive(Debug, Deserialize)]
struct MessageContent {
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImageContent {
    image_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileContent {
    file_key: Option<String>,
    #[serde(default)]
    file_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AudioContent {
    file_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct VideoContent {
    file_key: Option<String>,
    #[serde(default)]
    file_name: Option<String>,
}

pub struct FeishuChannel {
    config: Config,
    inbound_tx: mpsc::Sender<InboundMessage>,
    client: Client,
    seen_messages: Arc<Mutex<HashSet<String>>>,
    /// Per-instance token cache (shared across reconnects via Arc).
    token_cache: Arc<Mutex<CachedToken>>,
    /// Directory for downloaded media files.
    media_dir: PathBuf,
}

impl FeishuChannel {
    pub fn new(config: Config, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        let media_dir = std::env::var("BLOCKCELL_WORKSPACE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("workspace"))
            .join("media");

        Self {
            config,
            inbound_tx,
            client,
            seen_messages: Arc::new(Mutex::new(HashSet::new())),
            token_cache: Arc::new(Mutex::new(CachedToken::default())),
            media_dir,
        }
    }

    fn is_allowed(&self, open_id: &str) -> bool {
        let allow_from = &self.config.channels.feishu.allow_from;

        if allow_from.is_empty() {
            return true;
        }

        allow_from.iter().any(|allowed| allowed == open_id)
    }

    async fn get_tenant_access_token(&self) -> Result<String> {
        let mut cache = self.token_cache.lock().await;
        if cache.is_valid() {
            return Ok(cache.token.clone());
        }
        let token = fetch_tenant_access_token(
            &self.client,
            &self.config.channels.feishu.app_id,
            &self.config.channels.feishu.app_secret,
        )
        .await?;
        cache.token = token.clone();
        cache.expires_at = chrono::Utc::now().timestamp() + 7200; // 2h validity
        info!("Feishu tenant_access_token refreshed (cached 2h)");
        Ok(token)
    }

    async fn get_ws_endpoint(&self) -> Result<String> {
        let response = self
            .client
            .post(format!("{}/callback/ws/endpoint", FEISHU_BASE))
            .header("locale", "zh")
            .json(&serde_json::json!({
                "AppID": self.config.channels.feishu.app_id,
                "AppSecret": self.config.channels.feishu.app_secret,
            }))
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Failed to get WS endpoint: {}", e)))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| Error::Channel(format!("Failed to read endpoint response body: {}", e)))?;

        if !status.is_success() {
            return Err(Error::Channel(format!(
                "Feishu endpoint HTTP {}: {}",
                status, body
            )));
        }

        let endpoint_resp: WsEndpointResponse = serde_json::from_str(&body)
            .map_err(|e| Error::Channel(format!("Failed to parse endpoint response: {} | body: {}", e, &body[..body.len().min(500)])))?;

        if endpoint_resp.code != 0 {
            return Err(Error::Channel(format!(
                "Feishu endpoint error code={} msg={} | body: {}",
                endpoint_resp.code, endpoint_resp.msg, &body[..body.len().min(500)]
            )));
        }

        endpoint_resp
            .data
            .map(|d| d.url)
            .ok_or_else(|| Error::Channel(format!("No endpoint URL in response | body: {}", &body[..body.len().min(500)])))
    }

    pub async fn run_loop(self: Arc<Self>, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        if !self.config.channels.feishu.enabled {
            info!("Feishu channel disabled");
            return;
        }

        if self.config.channels.feishu.app_id.is_empty() {
            warn!("Feishu app_id not configured");
            return;
        }

        info!("Feishu channel starting");

        loop {
            tokio::select! {
                result = self.connect_and_run() => {
                    match result {
                        Ok(_) => {
                            info!("Feishu connection closed normally");
                        }
                        Err(e) => {
                            error!(error = %e, "Feishu connection error, reconnecting in 5s");
                            tokio::select! {
                                _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {}
                                _ = shutdown.recv() => {
                                    info!("Feishu channel shutting down");
                                    break;
                                }
                            }
                        }
                    }
                }
                _ = shutdown.recv() => {
                    info!("Feishu channel shutting down");
                    break;
                }
            }
        }
    }

    async fn connect_and_run(&self) -> Result<()> {
        let ws_url = self.get_ws_endpoint().await?;

        info!(url = %ws_url, "Connecting to Feishu WebSocket");

        let url = url::Url::parse(&ws_url)
            .map_err(|e| Error::Channel(format!("Invalid WebSocket URL: {}", e)))?;

        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| Error::Channel(format!("WebSocket connection failed: {}", e)))?;

        info!("Connected to Feishu WebSocket");

        let (mut write, mut read) = ws_stream.split();

        while let Some(msg) = read.next().await {
            match msg {
                Ok(WsMessage::Binary(data)) => {
                    // Feishu uses Protobuf binary frames
                    match Frame::decode(data.as_slice()) {
                        Ok(frame) => {
                            let msg_type = frame.headers.iter()
                                .find(|h| h.key == "type")
                                .map(|h| h.value.as_str())
                                .unwrap_or("");

                            if frame.method == FRAME_METHOD_CONTROL {
                                if msg_type == MSG_TYPE_PING {
                                    // Respond with pong
                                    let pong = Frame {
                                        seq_id: frame.seq_id,
                                        log_id: frame.log_id,
                                        service: frame.service,
                                        method: FRAME_METHOD_CONTROL,
                                        headers: vec![FrameHeader {
                                            key: "type".to_string(),
                                            value: MSG_TYPE_PONG.to_string(),
                                        }],
                                        payload: frame.payload.clone(),
                                        ..Default::default()
                                    };
                                    let mut buf = Vec::new();
                                    if prost::Message::encode(&pong, &mut buf).is_ok() {
                                        if let Err(e) = write.send(WsMessage::Binary(buf.into())).await {
                                            error!(error = %e, "Failed to send pong frame");
                                        } else {
                                            debug!("Sent pong to Feishu");
                                        }
                                    }
                                }
                            } else if frame.method == FRAME_METHOD_DATA {
                                // Parse payload as JSON event
                                debug!(method = frame.method, msg_type = %msg_type, payload_len = frame.payload.len(), "Feishu data frame");
                                match std::str::from_utf8(&frame.payload) {
                                    Ok(text) => {
                                        info!(payload = %&text[..text.len().min(500)], "Feishu raw event payload");
                                        // Send ACK frame
                                        let ack = Frame {
                                            seq_id: frame.seq_id,
                                            log_id: frame.log_id,
                                            service: frame.service,
                                            method: FRAME_METHOD_DATA,
                                            headers: frame.headers.clone(),
                                            payload: b"{\"code\":200}".to_vec(),
                                            ..Default::default()
                                        };
                                        let mut buf = Vec::new();
                                        if prost::Message::encode(&ack, &mut buf).is_ok() {
                                            if let Err(e) = write.send(WsMessage::Binary(buf.into())).await {
                                                error!(error = %e, "Failed to send ACK");
                                            }
                                        }
                                        if let Err(e) = self.handle_message(text).await {
                                            error!(error = %e, "Failed to handle Feishu event");
                                        }
                                    }
                                    Err(e) => {
                                        error!(error = %e, "Feishu frame payload is not UTF-8");
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "Failed to decode Feishu Protobuf frame");
                        }
                    }
                }
                Ok(WsMessage::Text(text)) => {
                    // Fallback: some frames may be text JSON
                    if let Err(e) = self.handle_message(&text).await {
                        error!(error = %e, "Failed to handle Feishu text message");
                    }
                }
                Ok(WsMessage::Close(_)) => {
                    info!("Feishu WebSocket closed");
                    break;
                }
                Ok(WsMessage::Ping(data)) => {
                    if let Err(e) = write.send(WsMessage::Pong(data)).await {
                        error!(error = %e, "Failed to send WS pong");
                    }
                }
                Err(e) => {
                    error!(error = %e, "WebSocket error");
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Download a Feishu media resource (image/file/audio/video) to the media dir.
    /// Returns the local file path on success.
    async fn download_media(
        &self,
        message_id: &str,
        file_key: &str,
        file_type: &str,
        file_name: Option<&str>,
    ) -> Result<String> {
        let token = self.get_tenant_access_token().await?;

        // Feishu media download endpoint
        let url = format!(
            "{}/im/v1/messages/{}/resources/{}?type={}",
            FEISHU_OPEN_API, message_id, file_key, file_type
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Feishu media download failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Channel(format!(
                "Feishu media download HTTP {}: {}",
                status, body
            )));
        }

        // Determine file extension from content-type or file_name
        let ext = file_name
            .and_then(|n| n.rsplit('.').next())
            .unwrap_or(match file_type {
                "image" => "jpg",
                "audio" => "opus",
                "video" => "mp4",
                _ => "bin",
            });

        tokio::fs::create_dir_all(&self.media_dir)
            .await
            .map_err(|e| Error::Channel(format!("Failed to create media dir: {}", e)))?;

        let filename = format!("feishu_{}_{}.{}", file_type, &file_key[..8.min(file_key.len())], ext);
        let path = self.media_dir.join(&filename);

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Channel(format!("Failed to read media bytes: {}", e)))?;

        tokio::fs::write(&path, &bytes)
            .await
            .map_err(|e| Error::Channel(format!("Failed to write media file: {}", e)))?;

        Ok(path.to_string_lossy().to_string())
    }

    async fn handle_message(&self, text: &str) -> Result<()> {
        let event: FeishuEvent = serde_json::from_str(text).map_err(|e| {
            warn!(error = %e, raw = %&text[..text.len().min(500)], "Failed to parse Feishu event");
            Error::Channel(format!("Failed to parse Feishu event: {}", e))
        })?;

        let header = match event.header {
            Some(h) => h,
            None => return Ok(()),
        };

        // Dedup by event_id
        {
            let mut seen = self.seen_messages.lock().await;
            if seen.contains(&header.event_id) {
                debug!(event_id = %header.event_id, "Duplicate event, skipping");
                return Ok(());
            }
            seen.insert(header.event_id.clone());
            if seen.len() > 1000 {
                let to_remove: Vec<_> = seen.iter().take(100).cloned().collect();
                for id in to_remove { seen.remove(&id); }
            }
        }

        if header.event_type != "im.message.receive_v1" {
            debug!(event_type = %header.event_type, "Ignoring non-message event");
            return Ok(());
        }

        let event_body = match event.event {
            Some(e) => e,
            None => return Ok(()),
        };

        if let Some(sender) = &event_body.sender {
            if sender.sender_type == "bot" {
                debug!("Skipping bot message");
                return Ok(());
            }
        }

        let message = match event_body.message {
            Some(m) => m,
            None => return Ok(()),
        };

        let sender_id = event_body
            .sender
            .and_then(|s| s.sender_id)
            .map(|id| id.open_id)
            .unwrap_or_default();

        if !self.is_allowed(&sender_id) {
            debug!(sender_id = %sender_id, "Sender not in allowlist, ignoring");
            return Ok(());
        }

        let (content_text, media_paths) = match message.message_type.as_str() {
            "text" => {
                let mc: MessageContent = serde_json::from_str(&message.content)
                    .map_err(|e| Error::Channel(format!("Failed to parse text content: {}", e)))?;
                let t = mc.text.unwrap_or_default();
                if t.is_empty() { return Ok(()); }
                (t, vec![])
            }
            "image" => {
                let mc: ImageContent = serde_json::from_str(&message.content)
                    .unwrap_or(ImageContent { image_key: None });
                let key = mc.image_key.unwrap_or_default();
                let mut paths = vec![];
                if !key.is_empty() {
                    match self.download_media(&message.message_id, &key, "image", None).await {
                        Ok(p) => paths.push(p),
                        Err(e) => error!(error = %e, "Failed to download Feishu image"),
                    }
                }
                ("[图片，已下载到本地，可直接查看或用 read_file 读取]".to_string(), paths)
            }
            "file" => {
                let mc: FileContent = serde_json::from_str(&message.content)
                    .unwrap_or(FileContent { file_key: None, file_name: None });
                let key = mc.file_key.unwrap_or_default();
                let name = mc.file_name.as_deref();
                let mut paths = vec![];
                if !key.is_empty() {
                    match self.download_media(&message.message_id, &key, "file", name).await {
                        Ok(p) => paths.push(p),
                        Err(e) => error!(error = %e, "Failed to download Feishu file"),
                    }
                }
                let desc = format!("[文件: {}，已下载到本地，可用 read_file 读取]", name.unwrap_or("unknown"));
                (desc, paths)
            }
            "audio" => {
                let mc: AudioContent = serde_json::from_str(&message.content)
                    .unwrap_or(AudioContent { file_key: None });
                let key = mc.file_key.unwrap_or_default();
                let mut paths = vec![];
                if !key.is_empty() {
                    match self.download_media(&message.message_id, &key, "audio", None).await {
                        Ok(p) => paths.push(p),
                        Err(e) => error!(error = %e, "Failed to download Feishu audio"),
                    }
                }
                ("[语音消息，已下载到本地，请用 audio_transcribe 工具转写后回复]".to_string(), paths)
            }
            "video" | "media" => {
                let mc: VideoContent = serde_json::from_str(&message.content)
                    .unwrap_or(VideoContent { file_key: None, file_name: None });
                let key = mc.file_key.unwrap_or_default();
                let name = mc.file_name.as_deref();
                let mut paths = vec![];
                if !key.is_empty() {
                    match self.download_media(&message.message_id, &key, "video", name).await {
                        Ok(p) => paths.push(p),
                        Err(e) => error!(error = %e, "Failed to download Feishu video"),
                    }
                }
                let desc = if let Some(n) = name {
                    format!("[视频: {}，已下载到本地]", n)
                } else {
                    "[视频，已下载到本地]".to_string()
                };
                (desc, paths)
            }
            other => {
                debug!(message_type = %other, "Feishu: unsupported message type, skipping");
                return Ok(());
            }
        };

        let inbound = InboundMessage {
            channel: "feishu".to_string(),
            sender_id: sender_id.clone(),
            chat_id: message.chat_id.clone(),
            content: content_text,
            media: media_paths,
            metadata: serde_json::json!({
                "message_id": message.message_id,
                "event_id": header.event_id,
                "message_type": message.message_type,
            }),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };

        self.inbound_tx
            .send(inbound)
            .await
            .map_err(|e| Error::Channel(e.to_string()))?;

        Ok(())
    }
}

/// Fetch a fresh tenant_access_token from Feishu API.
async fn fetch_tenant_access_token(client: &Client, app_id: &str, app_secret: &str) -> Result<String> {
    #[derive(Serialize)]
    struct TokenRequest<'a> { app_id: &'a str, app_secret: &'a str }

    let resp = client
        .post(format!("{}/auth/v3/tenant_access_token/internal", FEISHU_OPEN_API))
        .json(&TokenRequest { app_id, app_secret })
        .send()
        .await
        .map_err(|e| Error::Channel(format!("Failed to get Feishu access token: {}", e)))?;

    let body: TokenResponse = resp
        .json()
        .await
        .map_err(|e| Error::Channel(format!("Failed to parse Feishu token response: {}", e)))?;

    if body.code != 0 {
        return Err(Error::Channel(format!("Feishu token error: {}", body.msg)));
    }
    body.tenant_access_token
        .ok_or_else(|| Error::Channel("No access token in Feishu response".to_string()))
}

/// Get a cached tenant_access_token for the free send_message function.
async fn get_cached_token(config: &Config) -> Result<String> {
    let cache = global_token_cache();
    let mut guard = cache.lock().await;
    if guard.is_valid() {
        return Ok(guard.token.clone());
    }
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Channel(format!("Failed to build HTTP client: {}", e)))?;
    let token = fetch_tenant_access_token(
        &client,
        &config.channels.feishu.app_id,
        &config.channels.feishu.app_secret,
    )
    .await?;
    guard.token = token.clone();
    guard.expires_at = chrono::Utc::now().timestamp() + 7200;
    info!("Feishu tenant_access_token refreshed via global cache");
    Ok(token)
}

pub async fn send_message(config: &Config, chat_id: &str, text: &str) -> Result<()> {
    crate::rate_limit::feishu_limiter().acquire().await;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Channel(format!("Failed to build HTTP client: {}", e)))?;

    let token = get_cached_token(config).await?;

    do_send_message(&client, &token, chat_id, text).await
}

/// Upload a local file to Feishu and return the resource key.
/// Images → /im/v1/images (returns image_key)
/// Other  → /im/v1/files  (returns file_key)
async fn upload_feishu_media(
    client: &Client,
    token: &str,
    file_path: &str,
    file_type: &str,
) -> Result<String> {
    let path = std::path::Path::new(file_path);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| Error::Channel(format!("Failed to read file {}: {}", file_path, e)))?;

    let mime = feishu_mime_for_path(file_path);
    let is_image = file_type == "image";

    if is_image {
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name)
            .mime_str(mime)
            .map_err(|e| Error::Channel(format!("Invalid MIME: {}", e)))?;
        let form = reqwest::multipart::Form::new()
            .text("image_type", "message")
            .part("image", part);

        #[derive(Deserialize)]
        struct Resp { code: i32, msg: String, data: Option<ImgData> }
        #[derive(Deserialize)]
        struct ImgData { image_key: String }

        let resp = client
            .post(format!("{}/im/v1/images", FEISHU_OPEN_API))
            .header("Authorization", format!("Bearer {}", token))
            .multipart(form)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Feishu image upload failed: {}", e)))?;

        let r: Resp = resp.json().await
            .map_err(|e| Error::Channel(format!("Feishu image upload parse failed: {}", e)))?;
        if r.code != 0 {
            return Err(Error::Channel(format!("Feishu image upload error {}: {}", r.code, r.msg)));
        }
        return r.data.map(|d| d.image_key)
            .ok_or_else(|| Error::Channel("Feishu image upload: no image_key".to_string()));
    }

    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(file_name.clone())
        .mime_str(mime)
        .map_err(|e| Error::Channel(format!("Invalid MIME: {}", e)))?;
    let form = reqwest::multipart::Form::new()
        .text("file_type", file_type.to_string())
        .text("file_name", file_name)
        .part("file", part);

    #[derive(Deserialize)]
    struct Resp { code: i32, msg: String, data: Option<FileData> }
    #[derive(Deserialize)]
    struct FileData { file_key: String }

    let resp = client
        .post(format!("{}/im/v1/files", FEISHU_OPEN_API))
        .header("Authorization", format!("Bearer {}", token))
        .multipart(form)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("Feishu file upload failed: {}", e)))?;

    let r: Resp = resp.json().await
        .map_err(|e| Error::Channel(format!("Feishu file upload parse failed: {}", e)))?;
    if r.code != 0 {
        return Err(Error::Channel(format!("Feishu file upload error {}: {}", r.code, r.msg)));
    }
    r.data.map(|d| d.file_key)
        .ok_or_else(|| Error::Channel("Feishu file upload: no file_key".to_string()))
}

fn feishu_mime_for_path(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "opus" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "amr" => "audio/amr",
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

fn feishu_file_type_for_ext(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" => "image",
        "opus" | "amr" | "mp3" | "wav" | "m4a" => "opus",
        "mp4" | "avi" | "mov" | "mkv" => "mp4",
        "pdf" => "pdf",
        "doc" | "docx" => "doc",
        "xls" | "xlsx" => "xls",
        "ppt" | "pptx" => "ppt",
        _ => "stream",
    }
}

/// Send a media message (image/audio/video/file) to a Feishu chat.
/// Uploads the file first, then sends the appropriate message type.
pub async fn send_media_message(config: &Config, chat_id: &str, file_path: &str) -> Result<()> {
    crate::rate_limit::feishu_limiter().acquire().await;

    let ext = file_path.rsplit('.').next().unwrap_or("").to_lowercase();
    let file_type = feishu_file_type_for_ext(&ext);
    let is_image = file_type == "image";

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| Error::Channel(format!("Failed to build HTTP client: {}", e)))?;
    let token = get_cached_token(config).await?;

    info!(file_path = %file_path, file_type = %file_type, "Feishu: uploading media");
    let key = upload_feishu_media(&client, &token, file_path, file_type).await?;
    info!(key = %key, "Feishu: media uploaded");

    let (msg_type, content) = if is_image {
        ("image", serde_json::json!({ "image_key": key }).to_string())
    } else if matches!(ext.as_str(), "opus" | "amr" | "mp3" | "wav" | "m4a") {
        ("audio", serde_json::json!({ "file_key": key }).to_string())
    } else if matches!(ext.as_str(), "mp4" | "avi" | "mov" | "mkv") {
        ("media", serde_json::json!({ "file_key": key }).to_string())
    } else {
        ("file", serde_json::json!({ "file_key": key }).to_string())
    };

    #[derive(Serialize)]
    struct SendReq<'a> {
        receive_id: &'a str,
        msg_type: &'a str,
        content: String,
    }

    let send_client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Channel(format!("Failed to build HTTP client: {}", e)))?;

    let resp = send_client
        .post(format!("{}/im/v1/messages?receive_id_type=chat_id", FEISHU_OPEN_API))
        .header("Authorization", format!("Bearer {}", token))
        .json(&SendReq { receive_id: chat_id, msg_type, content })
        .send()
        .await
        .map_err(|e| Error::Channel(format!("Feishu send_media_message failed: {}", e)))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Channel(format!("Feishu API send media error: {}", body)));
    }
    Ok(())
}

async fn do_send_message(client: &Client, token: &str, chat_id: &str, text: &str) -> Result<()> {
    #[derive(Serialize)]
    struct SendMessageRequest<'a> {
        receive_id: &'a str,
        msg_type: &'a str,
        content: String,
    }

    let content = serde_json::json!({ "text": text }).to_string();
    let request = SendMessageRequest {
        receive_id: chat_id,
        msg_type: "text",
        content,
    };

    let response = client
        .post(format!("{}/im/v1/messages?receive_id_type=chat_id", FEISHU_OPEN_API))
        .header("Authorization", format!("Bearer {}", token))
        .json(&request)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("Failed to send Feishu message: {}", e)))?;

    if !response.status().is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Channel(format!("Feishu API error: {}", body)));
    }
    Ok(())
}
