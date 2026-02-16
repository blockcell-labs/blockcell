use futures::{SinkExt, StreamExt};
use blockcell_core::{Config, Error, InboundMessage, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use tracing::{debug, error, info, warn};

const FEISHU_OPEN_API: &str = "https://open.feishu.cn/open-apis";

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

pub struct FeishuChannel {
    config: Config,
    inbound_tx: mpsc::Sender<InboundMessage>,
    client: Client,
    seen_messages: Arc<Mutex<HashSet<String>>>,
}

impl FeishuChannel {
    pub fn new(config: Config, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            config,
            inbound_tx,
            client,
            seen_messages: Arc::new(Mutex::new(HashSet::new())),
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
        #[derive(Serialize)]
        struct TokenRequest<'a> {
            app_id: &'a str,
            app_secret: &'a str,
        }

        let request = TokenRequest {
            app_id: &self.config.channels.feishu.app_id,
            app_secret: &self.config.channels.feishu.app_secret,
        };

        let response = self
            .client
            .post(format!("{}/auth/v3/tenant_access_token/internal", FEISHU_OPEN_API))
            .json(&request)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Failed to get access token: {}", e)))?;

        let token_resp: TokenResponse = response
            .json()
            .await
            .map_err(|e| Error::Channel(format!("Failed to parse token response: {}", e)))?;

        if token_resp.code != 0 {
            return Err(Error::Channel(format!(
                "Feishu token error: {}",
                token_resp.msg
            )));
        }

        token_resp
            .tenant_access_token
            .ok_or_else(|| Error::Channel("No access token in response".to_string()))
    }

    async fn get_ws_endpoint(&self, token: &str) -> Result<String> {
        let response = self
            .client
            .post(format!("{}/callback/ws/endpoint", FEISHU_OPEN_API))
            .header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Failed to get WS endpoint: {}", e)))?;

        let endpoint_resp: WsEndpointResponse = response
            .json()
            .await
            .map_err(|e| Error::Channel(format!("Failed to parse endpoint response: {}", e)))?;

        if endpoint_resp.code != 0 {
            return Err(Error::Channel(format!(
                "Feishu endpoint error: {}",
                endpoint_resp.msg
            )));
        }

        endpoint_resp
            .data
            .map(|d| d.url)
            .ok_or_else(|| Error::Channel("No endpoint URL in response".to_string()))
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
        let token = self.get_tenant_access_token().await?;
        let ws_url = self.get_ws_endpoint(&token).await?;

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
                Ok(WsMessage::Text(text)) => {
                    if let Err(e) = self.handle_message(&text).await {
                        error!(error = %e, "Failed to handle Feishu message");
                    }
                }
                Ok(WsMessage::Close(_)) => {
                    info!("Feishu WebSocket closed");
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
        let event: FeishuEvent = serde_json::from_str(text)
            .map_err(|e| Error::Channel(format!("Failed to parse Feishu event: {}", e)))?;

        let header = match event.header {
            Some(h) => h,
            None => return Ok(()),
        };

        // Dedup by message_id
        {
            let mut seen = self.seen_messages.lock().await;
            if seen.contains(&header.event_id) {
                debug!(event_id = %header.event_id, "Duplicate event, skipping");
                return Ok(());
            }
            seen.insert(header.event_id.clone());
            // Keep only last 1000 message IDs
            if seen.len() > 1000 {
                let to_remove: Vec<_> = seen.iter().take(100).cloned().collect();
                for id in to_remove {
                    seen.remove(&id);
                }
            }
        }

        // Only handle message events
        if header.event_type != "im.message.receive_v1" {
            debug!(event_type = %header.event_type, "Ignoring non-message event");
            return Ok(());
        }

        let event_body = match event.event {
            Some(e) => e,
            None => return Ok(()),
        };

        // Skip bot messages
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

        // Only handle text messages for now
        if message.message_type != "text" {
            debug!(message_type = %message.message_type, "Ignoring non-text message");
            return Ok(());
        }

        // Parse message content
        let content: MessageContent = serde_json::from_str(&message.content)
            .map_err(|e| Error::Channel(format!("Failed to parse message content: {}", e)))?;

        let text = match content.text {
            Some(t) => t,
            None => return Ok(()),
        };

        // Get sender open_id
        let sender_id = event_body
            .sender
            .and_then(|s| s.sender_id)
            .map(|id| id.open_id)
            .unwrap_or_default();

        if !self.is_allowed(&sender_id) {
            debug!(sender_id = %sender_id, "Sender not in allowlist, ignoring");
            return Ok(());
        }

        let inbound = InboundMessage {
            channel: "feishu".to_string(),
            sender_id: sender_id.clone(),
            chat_id: message.chat_id.clone(),
            content: text,
            media: vec![],
            metadata: serde_json::json!({
                "message_id": message.message_id,
                "event_id": header.event_id,
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

pub async fn send_message(config: &Config, chat_id: &str, text: &str) -> Result<()> {
    let client = Client::new();

    // Get access token
    #[derive(Serialize)]
    struct TokenRequest<'a> {
        app_id: &'a str,
        app_secret: &'a str,
    }

    let token_request = TokenRequest {
        app_id: &config.channels.feishu.app_id,
        app_secret: &config.channels.feishu.app_secret,
    };

    let token_response = client
        .post(format!("{}/auth/v3/tenant_access_token/internal", FEISHU_OPEN_API))
        .json(&token_request)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("Failed to get access token: {}", e)))?;

    let token_resp: TokenResponse = token_response
        .json()
        .await
        .map_err(|e| Error::Channel(format!("Failed to parse token response: {}", e)))?;

    if token_resp.code != 0 {
        return Err(Error::Channel(format!(
            "Feishu token error: {}",
            token_resp.msg
        )));
    }

    let token = token_resp
        .tenant_access_token
        .ok_or_else(|| Error::Channel("No access token in response".to_string()))?;

    // Send message
    #[derive(Serialize)]
    struct SendMessageRequest<'a> {
        receive_id: &'a str,
        msg_type: &'a str,
        content: String,
    }

    let content = serde_json::json!({ "text": text }).to_string();

    let send_request = SendMessageRequest {
        receive_id: chat_id,
        msg_type: "text",
        content,
    };

    let response = client
        .post(format!(
            "{}/im/v1/messages?receive_id_type=chat_id",
            FEISHU_OPEN_API
        ))
        .header("Authorization", format!("Bearer {}", token))
        .json(&send_request)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("Failed to send Feishu message: {}", e)))?;

    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Channel(format!("Feishu API error: {}", text)));
    }

    Ok(())
}
