use futures::{SinkExt, StreamExt};
use blockcell_core::{Config, Error, InboundMessage, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use tracing::{debug, error, info, warn};

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
}

pub struct WhatsAppChannel {
    config: Config,
    inbound_tx: mpsc::Sender<InboundMessage>,
    seen_messages: Arc<Mutex<HashSet<String>>>,
}

impl WhatsAppChannel {
    pub fn new(config: Config, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        Self {
            config,
            inbound_tx,
            seen_messages: Arc::new(Mutex::new(HashSet::new())),
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

        let (mut write, mut read) = ws_stream.split();

        while let Some(msg) = read.next().await {
            match msg {
                Ok(WsMessage::Text(text)) => {
                    if let Err(e) = self.handle_message(&text).await {
                        error!(error = %e, "Failed to handle WhatsApp message");
                    }
                }
                Ok(WsMessage::Close(_)) => {
                    info!("WhatsApp bridge closed connection");
                    break;
                }
                Ok(WsMessage::Ping(data)) => {
                    if let Err(e) = write.send(WsMessage::Pong(data)).await {
                        error!(error = %e, "Failed to send pong");
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

    async fn handle_message(&self, text: &str) -> Result<()> {
        let msg: BridgeMessage = serde_json::from_str(text)
            .map_err(|e| Error::Channel(format!("Failed to parse bridge message: {}", e)))?;

        match msg.msg_type.as_str() {
            "message" => {
                let sender = msg.sender.as_deref().unwrap_or("");
                let content = msg.content.as_deref().unwrap_or("");

                if sender.is_empty() || content.is_empty() {
                    return Ok(());
                }

                if !self.is_allowed(sender) {
                    debug!(sender = %sender, "Sender not in allowlist, ignoring");
                    return Ok(());
                }

                // Dedup by message id (fallback: sender+timestamp+content)
                let dedup_key = if let Some(id) = msg.id.as_deref() {
                    format!("id:{}", id)
                } else {
                    let ts = msg
                        .timestamp
                        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
                    format!("fallback:{}:{}:{}", sender, ts, content)
                };

                {
                    let mut seen = self.seen_messages.lock().await;
                    if seen.contains(&dedup_key) {
                        debug!(key = %dedup_key, "Duplicate WhatsApp message, skipping");
                        return Ok(());
                    }
                    seen.insert(dedup_key);
                    // Keep only last 1000 keys
                    if seen.len() > 1000 {
                        let to_remove: Vec<_> = seen.iter().take(100).cloned().collect();
                        for k in to_remove {
                            seen.remove(&k);
                        }
                    }
                }

                // For WhatsApp, chat_id is the sender JID for direct messages
                // For groups, it would be the group JID
                let chat_id = sender.to_string();

                let inbound = InboundMessage {
                    channel: "whatsapp".to_string(),
                    sender_id: sender.to_string(),
                    chat_id,
                    content: content.to_string(),
                    media: vec![],
                    metadata: serde_json::json!({
                        "message_id": msg.id,
                        "is_group": msg.is_group.unwrap_or(false),
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
}

pub async fn send_message(config: &Config, chat_id: &str, text: &str) -> Result<()> {
    let bridge_url = &config.channels.whatsapp.bridge_url;
    let url = url::Url::parse(bridge_url)
        .map_err(|e| Error::Channel(format!("Invalid bridge URL: {}", e)))?;

    let (ws_stream, _) = connect_async(url)
        .await
        .map_err(|e| Error::Channel(format!("WebSocket connection failed: {}", e)))?;

    let (mut write, _) = ws_stream.split();

    let msg = SendMessage {
        msg_type: "send",
        to: chat_id,
        text,
    };

    let json = serde_json::to_string(&msg)
        .map_err(|e| Error::Channel(format!("Failed to serialize message: {}", e)))?;

    write
        .send(WsMessage::Text(json))
        .await
        .map_err(|e| Error::Channel(format!("Failed to send message: {}", e)))?;

    write
        .close()
        .await
        .map_err(|e| Error::Channel(format!("Failed to close connection: {}", e)))?;

    Ok(())
}
