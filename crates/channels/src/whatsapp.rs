use futures::{SinkExt, StreamExt};
use blockcell_core::{Config, Error, InboundMessage, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio_tungstenite::{
    connect_async,
    tungstenite::Message as WsMessage,
    WebSocketStream,
};
use tracing::{debug, error, info, warn};

type WsSink = futures::stream::SplitSink<
    WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WsMessage,
>;

#[derive(Debug, Serialize)]
struct SendMessage<'a> {
    #[serde(rename = "type")]
    msg_type: &'a str,
    to: &'a str,
    text: &'a str,
}

#[derive(Debug, Deserialize)]
struct BridgeMessage {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    sender: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(default)]
    is_group: Option<bool>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    qr: Option<String>,
    #[serde(default)]
    error: Option<String>,
    /// Media type: "image", "audio", "video", "document"
    #[serde(default)]
    media_type: Option<String>,
    /// Base64-encoded media data or a URL to download from
    #[serde(default)]
    media_data: Option<String>,
    /// Original filename for documents
    #[serde(default)]
    media_filename: Option<String>,
    /// MIME type
    #[serde(default)]
    mime_type: Option<String>,
}

pub struct WhatsAppChannel {
    config: Config,
    inbound_tx: mpsc::Sender<InboundMessage>,
    seen_messages: Arc<Mutex<HashSet<String>>>,
    /// Shared send-half of the active bridge WebSocket connection.
    shared_sink: Arc<Mutex<Option<WsSink>>>,
    media_dir: PathBuf,
}

impl WhatsAppChannel {
    pub fn new(config: Config, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        let media_dir = std::env::var("BLOCKCELL_WORKSPACE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("workspace"))
            .join("media");
        Self {
            config,
            inbound_tx,
            seen_messages: Arc::new(Mutex::new(HashSet::new())),
            shared_sink: Arc::new(Mutex::new(None)),
            media_dir,
        }
    }

    fn is_allowed(&self, sender: &str) -> bool {
        let allow_from = &self.config.channels.whatsapp.allow_from;

        if allow_from.is_empty() {
            return true;
        }

        // Extract phone number from JID (e.g., "1234567890@s.whatsapp.net" -> "1234567890")
        let phone = sender.split('@').next().unwrap_or(sender);

        allow_from.iter().any(|allowed| {
            allowed == sender || allowed == phone
        })
    }

    pub async fn run_loop(self: Arc<Self>, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        if !self.config.channels.whatsapp.enabled {
            info!("WhatsApp channel disabled");
            return;
        }

        let bridge_url = &self.config.channels.whatsapp.bridge_url;
        if bridge_url.is_empty() {
            warn!("WhatsApp bridge URL not configured");
            return;
        }

        info!(bridge_url = %bridge_url, "WhatsApp channel starting");

        loop {
            tokio::select! {
                result = self.connect_and_run() => {
                    match result {
                        Ok(_) => {
                            info!("WhatsApp connection closed normally");
                        }
                        Err(e) => {
                            error!(error = %e, "WhatsApp connection error, reconnecting in 5s");
                            tokio::select! {
                                _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {}
                                _ = shutdown.recv() => {
                                    info!("WhatsApp channel shutting down");
                                    break;
                                }
                            }
                        }
                    }
                }
                _ = shutdown.recv() => {
                    info!("WhatsApp channel shutting down");
                    break;
                }
            }
        }
    }

    async fn connect_and_run(&self) -> Result<()> {
        let bridge_url = &self.config.channels.whatsapp.bridge_url;
        let url = url::Url::parse(bridge_url)
            .map_err(|e| Error::Channel(format!("Invalid bridge URL: {}", e)))?;

        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| Error::Channel(format!("WebSocket connection failed: {}", e)))?;

        info!("Connected to WhatsApp bridge");

        let (write, mut read) = ws_stream.split();
        // Store the write half so send_message can reuse this connection.
        *self.shared_sink.lock().await = Some(write);

        loop {
            match read.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    if let Err(e) = self.handle_message(&text).await {
                        error!(error = %e, "Failed to handle WhatsApp message");
                    }
                }
                Some(Ok(WsMessage::Close(_))) => {
                    info!("WhatsApp bridge closed connection");
                    break;
                }
                Some(Ok(WsMessage::Ping(data))) => {
                    let mut guard = self.shared_sink.lock().await;
                    if let Some(ref mut write) = *guard {
                        if let Err(e) = write.send(WsMessage::Pong(data)).await {
                            error!(error = %e, "Failed to send pong");
                        }
                    }
                }
                Some(Err(e)) => {
                    error!(error = %e, "WebSocket error");
                    break;
                }
                None => break,
                _ => {}
            }
        }

        // Clear the shared sink on disconnect.
        *self.shared_sink.lock().await = None;
        Ok(())
    }

    async fn handle_message(&self, text: &str) -> Result<()> {
        let msg: BridgeMessage = serde_json::from_str(text)
            .map_err(|e| Error::Channel(format!("Failed to parse bridge message: {}", e)))?;

        match msg.msg_type.as_str() {
            "message" | "media" => {
                let sender = msg.sender.as_deref().unwrap_or("");
                if sender.is_empty() { return Ok(()); }

                if !self.is_allowed(sender) {
                    debug!(sender = %sender, "Sender not in allowlist, ignoring");
                    return Ok(());
                }

                let content_raw = msg.content.as_deref().unwrap_or("");
                let has_media = msg.media_type.is_some() && msg.media_data.is_some();
                if content_raw.is_empty() && !has_media { return Ok(()); }

                // Dedup by message id
                let dedup_key = if let Some(id) = msg.id.as_deref() {
                    format!("id:{}", id)
                } else {
                    let ts = msg.timestamp.unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
                    format!("fallback:{}:{}:{}", sender, ts, content_raw)
                };
                {
                    let mut seen = self.seen_messages.lock().await;
                    if seen.contains(&dedup_key) {
                        debug!(key = %dedup_key, "Duplicate WhatsApp message, skipping");
                        return Ok(());
                    }
                    seen.insert(dedup_key);
                    if seen.len() > 1000 {
                        let to_remove: Vec<_> = seen.iter().take(100).cloned().collect();
                        for k in to_remove { seen.remove(&k); }
                    }
                }

                // Download media if present
                let mut media_paths = vec![];
                if let (Some(media_type), Some(media_data)) =
                    (msg.media_type.as_deref(), msg.media_data.as_deref())
                {
                    let filename = msg.media_filename.as_deref();
                    let mime = msg.mime_type.as_deref();
                    match self.save_media_base64(media_type, media_data, filename, mime).await {
                        Ok(path) => media_paths.push(path),
                        Err(e) => error!(error = %e, "Failed to save WhatsApp media"),
                    }
                }

                let content_text = if content_raw.is_empty() {
                    match msg.media_type.as_deref().unwrap_or("media") {
                        "image" => "[图片，已下载到本地，可直接查看或用 read_file 读取]".to_string(),
                        "audio" | "ptt" => "[语音消息，已下载到本地，请用 audio_transcribe 工具转写后回复]".to_string(),
                        "video" => "[视频，已下载到本地]".to_string(),
                        "document" => format!("[文件: {}，已下载到本地，可用 read_file 读取]", msg.media_filename.as_deref().unwrap_or("unknown")),
                        other => format!("[{}，已下载到本地]", other),
                    }
                } else {
                    content_raw.to_string()
                };

                let chat_id = sender.to_string();
                let inbound = InboundMessage {
                    channel: "whatsapp".to_string(),
                    sender_id: sender.to_string(),
                    chat_id,
                    content: content_text,
                    media: media_paths,
                    metadata: serde_json::json!({
                        "message_id": msg.id,
                        "is_group": msg.is_group.unwrap_or(false),
                        "media_type": msg.media_type,
                    }),
                    timestamp_ms: msg.timestamp.unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
                };

                self.inbound_tx
                    .send(inbound)
                    .await
                    .map_err(|e| Error::Channel(e.to_string()))?;
            }
            "status" => {
                if let Some(status) = &msg.status {
                    info!(status = %status, "WhatsApp bridge status");
                }
            }
            "qr" => {
                if let Some(qr) = &msg.qr {
                    info!("WhatsApp QR code received (use 'channels login' to display)");
                    debug!(qr = %qr, "QR code data");
                }
            }
            "error" => {
                if let Some(error) = &msg.error {
                    error!(error = %error, "WhatsApp bridge error");
                }
            }
            _ => {
                debug!(msg_type = %msg.msg_type, "Unknown message type from bridge");
            }
        }

        Ok(())
    }

    /// Save base64-encoded media data from the bridge to the media directory.
    /// Returns the local file path.
    async fn save_media_base64(
        &self,
        media_type: &str,
        data: &str,
        filename: Option<&str>,
        mime: Option<&str>,
    ) -> Result<String> {
        use std::io::Write;

        // Decode base64
        let bytes = base64_decode(data)
            .map_err(|e| Error::Channel(format!("Failed to decode WhatsApp media: {}", e)))?;

        tokio::fs::create_dir_all(&self.media_dir)
            .await
            .map_err(|e| Error::Channel(format!("Failed to create media dir: {}", e)))?;

        // Determine extension
        let ext = filename
            .and_then(|n| n.rsplit('.').next())
            .or_else(|| mime_to_ext(mime.unwrap_or("")))
            .unwrap_or(match media_type {
                "image" => "jpg",
                "audio" | "ptt" => "ogg",
                "video" => "mp4",
                "document" => "bin",
                _ => "bin",
            });

        let stem = filename
            .map(|n| n.rsplit('.').nth(1).unwrap_or(n).to_string())
            .unwrap_or_else(|| format!("whatsapp_{}", media_type));

        let fname = format!("{}_{}.{}", stem, chrono::Utc::now().timestamp_millis(), ext);
        let path = self.media_dir.join(&fname);

        tokio::task::spawn_blocking({
            let path = path.clone();
            move || {
                let mut f = std::fs::File::create(&path)
                    .map_err(|e| Error::Channel(format!("Failed to create media file: {}", e)))?;
                f.write_all(&bytes)
                    .map_err(|e| Error::Channel(format!("Failed to write media file: {}", e)))?;
                Ok::<(), Error>(())
            }
        })
        .await
        .map_err(|e| Error::Channel(format!("spawn_blocking error: {}", e)))??;

        Ok(path.to_string_lossy().to_string())
    }
}

impl WhatsAppChannel {
    /// Send a message, reusing the persistent bridge connection when available.
    pub async fn send(&self, chat_id: &str, text: &str) -> Result<()> {
        send_message_inner(&self.config, chat_id, text, Some(&self.shared_sink)).await
    }
}

fn base64_decode(data: &str) -> std::result::Result<Vec<u8>, String> {
    use std::collections::HashMap;
    // Standard base64 alphabet
    let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table: HashMap<u8, u8> = HashMap::new();
    for (i, c) in alphabet.bytes().enumerate() {
        table.insert(c, i as u8);
    }
    let data = data.trim().replace('\n', "").replace('\r', "");
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let bytes = data.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        let b0 = *table.get(&bytes[i]).ok_or("invalid base64 char")?;
        let b1 = *table.get(&bytes[i + 1]).ok_or("invalid base64 char")?;
        let b2 = if bytes[i + 2] == b'=' { 0 } else { *table.get(&bytes[i + 2]).ok_or("invalid base64 char")? };
        let b3 = if bytes[i + 3] == b'=' { 0 } else { *table.get(&bytes[i + 3]).ok_or("invalid base64 char")? };
        out.push((b0 << 2) | (b1 >> 4));
        if bytes[i + 2] != b'=' { out.push((b1 << 4) | (b2 >> 2)); }
        if bytes[i + 3] != b'=' { out.push((b2 << 6) | b3); }
        i += 4;
    }
    Ok(out)
}

fn mime_to_ext(mime: &str) -> Option<&'static str> {
    match mime {
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "audio/ogg" | "audio/ogg; codecs=opus" => Some("ogg"),
        "audio/mpeg" | "audio/mp3" => Some("mp3"),
        "audio/mp4" => Some("m4a"),
        "video/mp4" => Some("mp4"),
        "video/webm" => Some("webm"),
        "application/pdf" => Some("pdf"),
        _ => None,
    }
}

/// Send a message via the WhatsApp bridge.
///
/// Uses a short-lived connection. Prefer `WhatsAppChannel::send` when a
/// persistent channel instance is available.
pub async fn send_message(config: &Config, chat_id: &str, text: &str) -> Result<()> {
    send_message_inner(config, chat_id, text, None).await
}

/// Internal helper used by both the free function and `WhatsAppChannel`.
async fn send_message_inner(
    config: &Config,
    chat_id: &str,
    text: &str,
    sink: Option<&Mutex<Option<WsSink>>>,
) -> Result<()> {
    let json = {
        let msg = SendMessage { msg_type: "send", to: chat_id, text };
        serde_json::to_string(&msg)
            .map_err(|e| Error::Channel(format!("Failed to serialize message: {}", e)))?
    };

    // Try to reuse the persistent connection first.
    if let Some(sink_lock) = sink {
        let mut guard = sink_lock.lock().await;
        if let Some(ref mut write) = *guard {
            match write.send(WsMessage::Text(json.clone())).await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    // Connection is broken; clear it and fall through to a new one.
                    warn!(error = %e, "WhatsApp shared sink broken, falling back to new connection");
                    *guard = None;
                }
            }
        }
    }

    // No persistent connection available — open a short-lived one.
    crate::rate_limit::whatsapp_limiter().acquire().await;
    let bridge_url = &config.channels.whatsapp.bridge_url;
    let url = url::Url::parse(bridge_url)
        .map_err(|e| Error::Channel(format!("Invalid bridge URL: {}", e)))?;

    let (ws_stream, _) = connect_async(url)
        .await
        .map_err(|e| Error::Channel(format!("WhatsApp bridge connect failed: {}", e)))?;

    let (mut write, _) = ws_stream.split();
    write
        .send(WsMessage::Text(json))
        .await
        .map_err(|e| Error::Channel(format!("Failed to send WhatsApp message: {}", e)))?;
    write
        .close()
        .await
        .map_err(|e| Error::Channel(format!("Failed to close WhatsApp connection: {}", e)))?;
    Ok(())
}
