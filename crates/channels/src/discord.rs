use blockcell_core::{Config, Error, InboundMessage, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
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
}

impl DiscordChannel {
    pub fn new(config: Config, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            config,
            client,
            inbound_tx,
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
            .ok_or_else(|| Error::Channel("No gateway URL in response".to_string()))
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
                        Some(Ok(WsMessage::Close(_))) => {
                            info!("Discord Gateway closed connection");
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

    async fn handle_message_create(&self, data: serde_json::Value) -> Result<()> {
        let msg: DiscordMessage = serde_json::from_value(data)
            .map_err(|e| Error::Channel(format!("Failed to parse Discord message: {}", e)))?;

        // Skip bot messages
        if msg.author.bot.unwrap_or(false) {
            return Ok(());
        }

        if !self.is_allowed(&msg.author.id) {
            debug!(user_id = %msg.author.id, "Discord user not in allowlist, ignoring");
            return Ok(());
        }

        if !self.is_monitored_channel(&msg.channel_id) {
            return Ok(());
        }

        if msg.content.is_empty() && msg.attachments.is_empty() {
            return Ok(());
        }

        let inbound = InboundMessage {
            channel: "discord".to_string(),
            sender_id: msg.author.id.clone(),
            chat_id: msg.channel_id.clone(),
            content: msg.content,
            media: vec![],
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
    let client = Client::new();
    let token = &config.channels.discord.bot_token;

    #[derive(Serialize)]
    struct CreateMessage<'a> {
        content: &'a str,
    }

    // Split long messages at 2000 char boundary (Discord limit)
    let chunks = split_message(text, 2000);

    for chunk in &chunks {
        let request = CreateMessage { content: chunk };

        let response = client
            .post(&format!("{}/channels/{}/messages", DISCORD_API_BASE, chat_id))
            .header("Authorization", format!("Bot {}", token))
            .json(&request)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Failed to send Discord message: {}", e)))?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Channel(format!("Discord API error: {}", body)));
        }

        // Small delay between chunks to avoid rate limiting
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
