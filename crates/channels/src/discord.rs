use blockcell_core::{Config, Error, InboundMessage, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

const DISCORD_API_BASE: &str = "https://discord.com/api/v10";

/// Discord Gateway opcodes
const GATEWAY_DISPATCH: u8 = 0;
const GATEWAY_HEARTBEAT: u8 = 1;
const GATEWAY_IDENTIFY: u8 = 2;
const GATEWAY_HELLO: u8 = 10;
const GATEWAY_HEARTBEAT_ACK: u8 = 11;

#[derive(Debug, Deserialize)]
struct GatewayPayload {
    op: u8,
    #[serde(default)]
    d: Option<serde_json::Value>,
    #[serde(default)]
    s: Option<u64>,
    #[serde(default)]
    t: Option<String>,
}

#[derive(Debug, Serialize)]
struct GatewayIdentify {
    op: u8,
    d: IdentifyData,
}

#[derive(Debug, Serialize)]
struct IdentifyData {
    token: String,
    intents: u64,
    properties: IdentifyProperties,
}

#[derive(Debug, Serialize)]
struct IdentifyProperties {
    os: String,
    browser: String,
    device: String,
}

#[derive(Debug, Serialize)]
struct GatewayHeartbeat {
    op: u8,
    d: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DiscordMessage {
    id: String,
    #[serde(default)]
    content: String,
    author: DiscordUser,
    channel_id: String,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    attachments: Vec<DiscordAttachment>,
}

#[derive(Debug, Deserialize)]
struct DiscordUser {
    id: String,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    bot: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DiscordAttachment {
    id: String,
    filename: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    content_type: Option<String>,
}

/// Discord channel using Gateway WebSocket for receiving messages
/// and REST API for sending messages.
pub struct DiscordChannel {
    config: Config,
    client: Client,
    inbound_tx: mpsc::Sender<InboundMessage>,
    media_dir: PathBuf,
}

impl DiscordChannel {
    pub fn new(config: Config, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        let media_dir = std::env::var("BLOCKCELL_WORKSPACE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("workspace"))
            .join("media");

        Self {
            config,
            client,
            inbound_tx,
            media_dir,
        }
    }

    fn is_allowed(&self, user_id: &str) -> bool {
        let allow_from = &self.config.channels.discord.allow_from;
        if allow_from.is_empty() {
            return true;
        }
        allow_from.iter().any(|allowed| allowed == user_id)
    }

    fn is_monitored_channel(&self, channel_id: &str) -> bool {
        let channels = &self.config.channels.discord.channels;
        if channels.is_empty() {
            return true; // Monitor all channels if none specified
        }
        channels.iter().any(|ch| ch == channel_id)
    }

    /// Get the Gateway WebSocket URL from Discord.
    async fn get_gateway_url(&self) -> Result<String> {
        let token = &self.config.channels.discord.bot_token;

        let response = self
            .client
            .get(&format!("{}/gateway/bot", DISCORD_API_BASE))
            .header("Authorization", format!("Bot {}", token))
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Failed to get Discord gateway: {}", e)))?;

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| Error::Channel(format!("Failed to parse gateway response: {}", e)))?;

        body.get("url")
            .and_then(|v| v.as_str())
            .map(|s| format!("{}/?v=10&encoding=json", s))
            .ok_or_else(|| Error::Channel(format!("No gateway URL in response: {}", body)))
    }

    pub async fn run_loop(self: Arc<Self>, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        if !self.config.channels.discord.enabled {
            info!("Discord channel disabled");
            return;
        }

        if self.config.channels.discord.bot_token.is_empty() {
            warn!("Discord bot token not configured");
            return;
        }

        info!("Discord channel starting");

        loop {
            tokio::select! {
                result = self.connect_and_run() => {
                    match result {
                        Ok(_) => {
                            info!("Discord connection closed normally");
                        }
                        Err(e) => {
                            let msg = e.to_string();
                            // Fatal errors — stop reconnecting
                            if msg.contains("Discord fatal close") {
                                error!("Discord channel stopped due to fatal error: {}", msg);
                                break;
                            }
                            error!(error = %e, "Discord connection error, reconnecting in 5s");
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                                _ = shutdown.recv() => {
                                    info!("Discord channel shutting down");
                                    break;
                                }
                            }
                        }
                    }
                }
                _ = shutdown.recv() => {
                    info!("Discord channel shutting down");
                    break;
                }
            }
        }
    }

    async fn connect_and_run(&self) -> Result<()> {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

        let gateway_url = self.get_gateway_url().await?;
        info!(url = %gateway_url, "Connecting to Discord Gateway");

        let url = url::Url::parse(&gateway_url)
            .map_err(|e| Error::Channel(format!("Invalid gateway URL: {}", e)))?;

        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| Error::Channel(format!("WebSocket connection failed: {}", e)))?;

        info!("Connected to Discord Gateway");

        let (mut write, mut read) = ws_stream.split();
        let sequence: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
        let mut heartbeat_interval_ms: u64 = 41250; // Default

        // Read the first message (should be Hello with heartbeat_interval)
        if let Some(Ok(WsMessage::Text(text))) = read.next().await {
            if let Ok(payload) = serde_json::from_str::<GatewayPayload>(&text) {
                if payload.op == GATEWAY_HELLO {
                    if let Some(d) = &payload.d {
                        if let Some(interval) = d.get("heartbeat_interval").and_then(|v| v.as_u64())
                        {
                            heartbeat_interval_ms = interval;
                            debug!(interval_ms = interval, "Received Hello with heartbeat interval");
                        }
                    }
                }
            }
        }

        // Send Identify
        // Intents: GUILDS (1<<0) | GUILD_MESSAGES (1<<9) | MESSAGE_CONTENT (1<<15) | DIRECT_MESSAGES (1<<12)
        let intents: u64 = (1 << 0) | (1 << 9) | (1 << 12) | (1 << 15);
        let identify = GatewayIdentify {
            op: GATEWAY_IDENTIFY as u8,
            d: IdentifyData {
                token: self.config.channels.discord.bot_token.clone(),
                intents,
                properties: IdentifyProperties {
                    os: "macos".to_string(),
                    browser: "blockcell".to_string(),
                    device: "blockcell".to_string(),
                },
            },
        };

        let identify_json = serde_json::to_string(&identify)
            .map_err(|e| Error::Channel(format!("Failed to serialize identify: {}", e)))?;
        write
            .send(WsMessage::Text(identify_json))
            .await
            .map_err(|e| Error::Channel(format!("Failed to send identify: {}", e)))?;

        info!("Sent Identify to Discord Gateway");

        // Spawn heartbeat task
        let heartbeat_interval = Duration::from_millis(heartbeat_interval_ms);
        let (heartbeat_tx, mut heartbeat_rx) = mpsc::channel::<String>(8);

        let heartbeat_handle = tokio::spawn({
            let interval = heartbeat_interval;
            let sequence = sequence.clone();
            async move {
                loop {
                    tokio::time::sleep(interval).await;

                    let seq = {
                        let guard = sequence.lock().await;
                        *guard
                    };

                    let hb = GatewayHeartbeat {
                        op: GATEWAY_HEARTBEAT as u8,
                        d: seq,
                    };
                    if let Ok(json) = serde_json::to_string(&hb) {
                        if heartbeat_tx.send(json).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(WsMessage::Text(text))) => {
                            if let Ok(payload) = serde_json::from_str::<GatewayPayload>(&text) {
                                // Update sequence number
                                if let Some(s) = payload.s {
                                    let mut guard = sequence.lock().await;
                                    *guard = Some(s);
                                }

                                match payload.op {
                                    op if op == GATEWAY_DISPATCH => {
                                        if let Some(event_type) = &payload.t {
                                            if event_type == "MESSAGE_CREATE" {
                                                if let Some(d) = payload.d {
                                                    if let Err(e) = self.handle_message_create(d).await {
                                                        error!(error = %e, "Failed to handle Discord message");
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    op if op == GATEWAY_HEARTBEAT_ACK => {
                                        debug!("Heartbeat ACK received");
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Some(Ok(WsMessage::Close(frame))) => {
                            let code = frame.as_ref().map(|f| f.code.into()).unwrap_or(0u16);
                            let reason = frame.as_ref().map(|f| f.reason.as_ref()).unwrap_or("");
                            info!(close_code = code, reason = %reason, "Discord Gateway closed connection");
                            // Fatal close codes — do not reconnect
                            if matches!(code, 4004 | 4010 | 4011 | 4012 | 4013 | 4014) {
                                let hint = match code {
                                    4004 => "Invalid bot token — check your Discord bot token",
                                    4013 => "Invalid intents — check your intent flags",
                                    4014 => "Disallowed intents — enable MESSAGE_CONTENT privileged intent in Discord Developer Portal (Bot → Privileged Gateway Intents)",
                                    _ => "Fatal Discord error — will not reconnect",
                                };
                                error!(close_code = code, hint = %hint, "Discord fatal close — stopping reconnect");
                                return Err(Error::Channel(format!("Discord fatal close {}: {}", code, hint)));
                            }
                            break;
                        }
                        Some(Err(e)) => {
                            error!(error = %e, "WebSocket error");
                            break;
                        }
                        None => {
                            info!("Discord WebSocket stream ended");
                            break;
                        }
                        _ => {}
                    }
                }
                Some(hb_json) = heartbeat_rx.recv() => {
                    if let Err(e) = write.send(WsMessage::Text(hb_json)).await {
                        error!(error = %e, "Failed to send heartbeat");
                        break;
                    }
                }
            }
        }

        heartbeat_handle.abort();
        Ok(())
    }

    /// Download a Discord attachment to the media directory.
    /// Returns the local file path on success.
    async fn download_attachment(&self, attachment: &DiscordAttachment) -> Result<String> {
        let url = match &attachment.url {
            Some(u) => u.clone(),
            None => return Err(Error::Channel("Attachment has no URL".to_string())),
        };

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Discord attachment download failed: {}", e)))?;

        if !resp.status().is_success() {
            return Err(Error::Channel(format!(
                "Discord attachment HTTP {}",
                resp.status()
            )));
        }

        tokio::fs::create_dir_all(&self.media_dir)
            .await
            .map_err(|e| Error::Channel(format!("Failed to create media dir: {}", e)))?;

        let filename = format!("discord_{}_{}", &attachment.id, &attachment.filename);
        let path = self.media_dir.join(&filename);

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Channel(format!("Failed to read attachment bytes: {}", e)))?;

        tokio::fs::write(&path, &bytes)
            .await
            .map_err(|e| Error::Channel(format!("Failed to write attachment: {}", e)))?;

        Ok(path.to_string_lossy().to_string())
    }

    async fn handle_message_create(&self, data: serde_json::Value) -> Result<()> {
        let msg: DiscordMessage = serde_json::from_value(data)
            .map_err(|e| Error::Channel(format!("Failed to parse Discord message: {}", e)))?;

        info!(
            user_id = %msg.author.id,
            username = ?msg.author.username,
            channel_id = %msg.channel_id,
            content_len = msg.content.len(),
            attachments = msg.attachments.len(),
            is_bot = msg.author.bot.unwrap_or(false),
            "Discord MESSAGE_CREATE received"
        );

        if msg.author.bot.unwrap_or(false) {
            debug!("Skipping bot message");
            return Ok(());
        }

        if !self.is_allowed(&msg.author.id) {
            info!(user_id = %msg.author.id, "Discord user not in allowlist, ignoring");
            return Ok(());
        }

        if !self.is_monitored_channel(&msg.channel_id) {
            info!(channel_id = %msg.channel_id, "Discord channel not monitored, ignoring");
            return Ok(());
        }

        if msg.content.is_empty() && msg.attachments.is_empty() {
            warn!(
                user_id = %msg.author.id,
                channel_id = %msg.channel_id,
                "Discord message has empty content and no attachments — \
                 if you sent text, MESSAGE_CONTENT privileged intent may not be enabled \
                 in Discord Developer Portal (Bot → Privileged Gateway Intents)"
            );
            return Ok(());
        }

        // Download attachments concurrently
        let mut media_paths = Vec::new();
        for attachment in &msg.attachments {
            match self.download_attachment(attachment).await {
                Ok(path) => media_paths.push(path),
                Err(e) => error!(
                    error = %e,
                    filename = %attachment.filename,
                    "Failed to download Discord attachment"
                ),
            }
        }

        // Strip leading @mention of the bot (e.g. "<@1234567890> hello" → "hello")
        let content = msg.content.trim().to_string();
        let content = if content.starts_with("<@") {
            if let Some(end) = content.find('>') {
                content[end + 1..].trim().to_string()
            } else {
                content
            }
        } else {
            content
        };

        // When content is empty but attachments exist, generate a descriptive fallback
        let content = if content.is_empty() && !media_paths.is_empty() {
            let descs: Vec<String> = msg.attachments.iter().map(|a| {
                let ct = a.content_type.as_deref().unwrap_or("");
                if ct.starts_with("image/") {
                    "[图片，已下载到本地，可直接查看或用 read_file 读取]".to_string()
                } else if ct.starts_with("audio/") || ct.starts_with("video/ogg") {
                    "[语音消息，已下载到本地，请用 audio_transcribe 工具转写后回复]".to_string()
                } else if ct.starts_with("video/") {
                    "[视频，已下载到本地]".to_string()
                } else {
                    format!("[文件: {}，已下载到本地，可用 read_file 读取]", a.filename)
                }
            }).collect();
            descs.join("\n")
        } else {
            content
        };

        info!(content = %content, channel_id = %msg.channel_id, "Forwarding Discord message to agent");

        let inbound = InboundMessage {
            channel: "discord".to_string(),
            sender_id: msg.author.id.clone(),
            chat_id: msg.channel_id.clone(),
            content,
            media: media_paths,
            metadata: serde_json::json!({
                "message_id": msg.id,
                "username": msg.author.username,
                "guild_id": msg.guild_id,
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

/// Send a message to a Discord channel via REST API.
/// Discord has a 2000 character limit per message, so long messages are split.
pub async fn send_message(config: &Config, chat_id: &str, text: &str) -> Result<()> {
    send_message_reply(config, chat_id, text, None).await
}

/// Send a message to a Discord channel, optionally replying to a specific message.
/// `reply_to_message_id` sets the `message_reference` for Discord thread replies.
pub async fn send_message_reply(
    config: &Config,
    chat_id: &str,
    text: &str,
    reply_to_message_id: Option<&str>,
) -> Result<()> {
    crate::rate_limit::discord_limiter().acquire().await;
    let client = Client::new();
    let token = &config.channels.discord.bot_token;

    let chunks = split_message(text, 2000);

    for (i, chunk) in chunks.iter().enumerate() {
        let mut body = serde_json::json!({ "content": chunk });
        // Only attach message_reference on the first chunk
        if i == 0 {
            if let Some(msg_id) = reply_to_message_id {
                body["message_reference"] = serde_json::json!({
                    "message_id": msg_id,
                    "channel_id": chat_id,
                    "fail_if_not_exists": false,
                });
            }
        }

        let response = client
            .post(&format!("{}/channels/{}/messages", DISCORD_API_BASE, chat_id))
            .header("Authorization", format!("Bot {}", token))
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Failed to send Discord message: {}", e)))?;

        if !response.status().is_success() {
            let err_body = response.text().await.unwrap_or_default();
            return Err(Error::Channel(format!("Discord API error: {}", err_body)));
        }

        if chunks.len() > 1 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    Ok(())
}

/// Split a message into chunks at newline boundaries, respecting a max length.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }

        // Try to split at a newline within the limit
        let split_at = remaining[..max_len]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(max_len);

        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];
    }

    chunks
}

/// Send a media file as an attachment to a Discord channel.
/// Discord supports any file type as an attachment via multipart form upload.
pub async fn send_media_message(config: &Config, chat_id: &str, file_path: &str) -> Result<()> {
    crate::rate_limit::discord_limiter().acquire().await;

    let path = std::path::Path::new(file_path);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| Error::Channel(format!("Failed to read file {}: {}", file_path, e)))?;

    let ext = file_path.rsplit('.').next().unwrap_or("").to_lowercase();
    let mime = discord_mime_for_ext(&ext);

    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(file_name)
        .mime_str(mime)
        .map_err(|e| Error::Channel(format!("Invalid MIME: {}", e)))?;

    let form = reqwest::multipart::Form::new().part("files[0]", part);

    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap_or_else(|_| Client::new());
    let token = &config.channels.discord.bot_token;

    info!(file_path = %file_path, "Discord: sending media attachment");

    let resp = client
        .post(&format!("{}/channels/{}/messages", DISCORD_API_BASE, chat_id))
        .header("Authorization", format!("Bot {}", token))
        .multipart(form)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("Discord send media failed: {}", e)))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Channel(format!("Discord send media error: {}", body)));
    }
    Ok(())
}

fn discord_mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ogg" | "opus" => "audio/ogg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discord_message_deserialize() {
        let json = r#"{
            "id": "123456",
            "content": "hello world",
            "author": {"id": "789", "username": "testuser"},
            "channel_id": "456",
            "attachments": []
        }"#;
        let msg: DiscordMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.id, "123456");
        assert_eq!(msg.content, "hello world");
        assert_eq!(msg.author.id, "789");
        assert!(msg.author.bot.is_none());
    }

    #[test]
    fn test_discord_bot_message_skip() {
        let json = r#"{
            "id": "123456",
            "content": "bot message",
            "author": {"id": "789", "username": "bot", "bot": true},
            "channel_id": "456",
            "attachments": []
        }"#;
        let msg: DiscordMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.author.bot, Some(true));
    }

    #[test]
    fn test_gateway_identify_serialize() {
        let identify = GatewayIdentify {
            op: 2,
            d: IdentifyData {
                token: "test-token".to_string(),
                intents: 33281,
                properties: IdentifyProperties {
                    os: "macos".to_string(),
                    browser: "blockcell".to_string(),
                    device: "blockcell".to_string(),
                },
            },
        };
        let json = serde_json::to_string(&identify).unwrap();
        assert!(json.contains("\"op\":2"));
        assert!(json.contains("test-token"));
    }
}
