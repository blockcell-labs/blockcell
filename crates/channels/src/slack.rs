use blockcell_core::{Config, Error, InboundMessage, Result};
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use tracing::{debug, error, info, warn};

const SLACK_API_BASE: &str = "https://slack.com/api";
/// Slack single message character limit
const SLACK_MSG_LIMIT: usize = 4000;

fn shared_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to build reqwest client")
}

#[derive(Debug, Deserialize)]
struct SlackResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackConnectionsOpenResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

/// Top-level Socket Mode envelope
#[derive(Debug, Deserialize)]
struct SocketEnvelope {
    envelope_id: Option<String>,
    #[serde(rename = "type")]
    envelope_type: String,
    #[serde(default)]
    payload: Option<serde_json::Value>,
    #[serde(default)]
    #[allow(dead_code)]
    accepts_response_payload: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct SlackHistoryResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    messages: Option<Vec<SlackMessage>>,
}

#[derive(Debug, Deserialize)]
struct SlackMessage {
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    ts: Option<String>,
    #[serde(default)]
    thread_ts: Option<String>,
    #[serde(default)]
    bot_id: Option<String>,
}

/// Slack channel supporting two modes:
/// - **Socket Mode** (preferred): real-time WebSocket push via `app_token`.
///   Requires `app_token` (xapp-…) in config. Zero-latency, no polling.
/// - **Polling fallback**: HTTP `conversations.history` when `app_token` is absent.
pub struct SlackChannel {
    config: Config,
    client: Client,
    inbound_tx: mpsc::Sender<InboundMessage>,
}

impl SlackChannel {
    pub fn new(config: Config, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        Self {
            config,
            client: shared_client(),
            inbound_tx,
        }
    }

    fn is_allowed(&self, user_id: &str) -> bool {
        let allow_from = &self.config.channels.slack.allow_from;
        if allow_from.is_empty() {
            return true;
        }
        allow_from.iter().any(|allowed| allowed == user_id)
    }

    // ── Socket Mode ───────────────────────────────────────────────────────────

    async fn get_socket_url(&self) -> Result<String> {
        let app_token = &self.config.channels.slack.app_token;
        let response = self
            .client
            .post(&format!("{}/apps.connections.open", SLACK_API_BASE))
            .header("Authorization", format!("Bearer {}", app_token))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("")
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Slack connections.open failed: {}", e)))?;

        let body: SlackConnectionsOpenResponse = response
            .json()
            .await
            .map_err(|e| Error::Channel(format!("Failed to parse connections.open: {}", e)))?;

        if !body.ok {
            return Err(Error::Channel(format!(
                "Slack connections.open error: {}",
                body.error.unwrap_or_else(|| "unknown".to_string())
            )));
        }
        body.url
            .ok_or_else(|| Error::Channel("No WSS URL in connections.open response".to_string()))
    }

    async fn run_socket_mode(&self) -> Result<()> {
        let wss_url = self.get_socket_url().await?;
        let url = url::Url::parse(&wss_url)
            .map_err(|e| Error::Channel(format!("Invalid Socket Mode URL: {}", e)))?;

        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| Error::Channel(format!("Socket Mode connect failed: {}", e)))?;

        info!("Slack Socket Mode connected");
        let (mut write, mut read) = ws_stream.split();

        while let Some(msg) = read.next().await {
            match msg {
                Ok(WsMessage::Text(text)) => {
                    if let Ok(envelope) = serde_json::from_str::<SocketEnvelope>(&text) {
                        // ACK immediately to prevent Slack from retrying
                        if let Some(eid) = &envelope.envelope_id {
                            let ack = serde_json::json!({ "envelope_id": eid });
                            if let Err(e) = write.send(WsMessage::Text(ack.to_string())).await {
                                error!(error = %e, "Failed to send Socket Mode ACK");
                            }
                        }
                        match envelope.envelope_type.as_str() {
                            "events_api" => {
                                if let Some(payload) = &envelope.payload {
                                    if let Err(e) = self.handle_events_api(payload).await {
                                        error!(error = %e, "Failed to handle Slack events_api");
                                    }
                                }
                            }
                            "hello" => info!("Slack Socket Mode hello received"),
                            "disconnect" => {
                                info!("Slack Socket Mode disconnect requested");
                                return Err(Error::Channel("Slack requested disconnect".to_string()));
                            }
                            other => debug!(envelope_type = %other, "Slack: unknown envelope type"),
                        }
                    }
                }
                Ok(WsMessage::Ping(data)) => { let _ = write.send(WsMessage::Pong(data)).await; }
                Ok(WsMessage::Close(_)) => {
                    return Err(Error::Channel("Slack Socket Mode closed".to_string()));
                }
                Err(e) => {
                    return Err(Error::Channel(format!("Slack Socket Mode WS error: {}", e)));
                }
                _ => {}
            }
        }
        Err(Error::Channel("Slack Socket Mode stream ended".to_string()))
    }

    async fn handle_events_api(&self, payload: &serde_json::Value) -> Result<()> {
        let event = match payload.get("event") {
            Some(e) => e,
            None => return Ok(()),
        };
        if event.get("type").and_then(|v| v.as_str()).unwrap_or("") != "message" {
            return Ok(());
        }
        if event.get("bot_id").is_some() { return Ok(()); }
        if let Some(sub) = event.get("subtype").and_then(|v| v.as_str()) {
            if sub != "file_share" {
                debug!(subtype = %sub, "Slack: skipping message subtype");
                return Ok(());
            }
        }
        let user = event.get("user").and_then(|v| v.as_str()).unwrap_or("");
        if user.is_empty() || !self.is_allowed(user) {
            debug!(user = %user, "Slack: user empty or not in allowlist");
            return Ok(());
        }
        let text = event.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if text.is_empty() { return Ok(()); }
        let channel_id = event.get("channel").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let monitored = &self.config.channels.slack.channels;
        if !monitored.is_empty() && !monitored.iter().any(|c| c == &channel_id) {
            return Ok(());
        }
        let ts = event.get("ts").and_then(|v| v.as_str()).map(|s| s.to_string());
        let thread_ts = event.get("thread_ts").and_then(|v| v.as_str()).map(|s| s.to_string());
        let inbound = InboundMessage {
            channel: "slack".to_string(),
            sender_id: user.to_string(),
            chat_id: channel_id,
            content: text,
            media: vec![],
            metadata: serde_json::json!({ "ts": ts, "thread_ts": thread_ts, "mode": "socket" }),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };
        self.inbound_tx.send(inbound).await
            .map_err(|e| Error::Channel(e.to_string()))
    }

    // ── Polling fallback ──────────────────────────────────────────────────────

    /// Poll conversations.history for new messages in configured channels.
    async fn poll_messages(&self, channel_id: &str, oldest: &str) -> Result<Vec<SlackMessage>> {
        let token = &self.config.channels.slack.bot_token;

        let response = self
            .client
            .get(&format!("{}/conversations.history", SLACK_API_BASE))
            .header("Authorization", format!("Bearer {}", token))
            .query(&[
                ("channel", channel_id),
                ("oldest", oldest),
                ("limit", "20"),
            ])
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Slack request failed: {}", e)))?;

        let body: SlackHistoryResponse = response
            .json()
            .await
            .map_err(|e| Error::Channel(format!("Failed to parse Slack response: {}", e)))?;

        if !body.ok {
            return Err(Error::Channel(format!(
                "Slack API error: {}",
                body.error.unwrap_or_else(|| "unknown".to_string())
            )));
        }

        Ok(body.messages.unwrap_or_default())
    }

    async fn run_polling(&self, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        let channels = self.config.channels.slack.channels.clone();
        if channels.is_empty() {
            warn!("No Slack channels configured to monitor (polling mode)");
            return;
        }
        let now = format!("{}.000000", chrono::Utc::now().timestamp());
        let mut latest_ts: std::collections::HashMap<String, String> =
            channels.iter().map(|c| (c.clone(), now.clone())).collect();
        let poll_interval = Duration::from_secs(
            self.config.channels.slack.poll_interval_secs.max(2) as u64,
        );
        info!(interval_secs = poll_interval.as_secs(), "Slack channel started (polling fallback mode)");
        loop {
            tokio::select! {
                _ = tokio::time::sleep(poll_interval) => {
                    for channel_id in &channels {
                        let oldest = latest_ts.get(channel_id).cloned().unwrap_or_else(|| now.clone());
                        match self.poll_messages(channel_id, &oldest).await {
                            Ok(messages) => {
                                // conversations.history returns messages newest-first.
                                // Track the maximum ts seen so we don't re-fetch old messages.
                                let mut max_ts: Option<String> = None;
                                for msg in messages {
                                    if let Some(ts) = &msg.ts {
                                        match &max_ts {
                                            None => max_ts = Some(ts.clone()),
                                            Some(cur) if ts > cur => max_ts = Some(ts.clone()),
                                            _ => {}
                                        }
                                    }
                                    if msg.bot_id.is_some() { continue; }
                                    let user = msg.user.as_deref().unwrap_or("");
                                    if user.is_empty() || !self.is_allowed(user) { continue; }
                                    let content = msg.text.clone().unwrap_or_default();
                                    if content.is_empty() { continue; }
                                    let inbound = InboundMessage {
                                        channel: "slack".to_string(),
                                        sender_id: user.to_string(),
                                        chat_id: channel_id.clone(),
                                        content,
                                        media: vec![],
                                        metadata: serde_json::json!({
                                            "ts": msg.ts,
                                            "thread_ts": msg.thread_ts,
                                            "mode": "polling",
                                        }),
                                        timestamp_ms: chrono::Utc::now().timestamp_millis(),
                                    };
                                    if let Err(e) = self.inbound_tx.send(inbound).await {
                                        error!(error = %e, "Failed to send Slack inbound message");
                                    }
                                }
                                // Update cursor to the newest message seen
                                if let Some(ts) = max_ts {
                                    latest_ts.insert(channel_id.clone(), ts);
                                }
                            }
                            Err(e) => {
                                error!(error = %e, channel = %channel_id, "Failed to poll Slack messages");
                            }
                        }
                    }
                }
                _ = shutdown.recv() => {
                    info!("Slack channel shutting down (polling)");
                    break;
                }
            }
        }
    }

    pub async fn run_loop(self: Arc<Self>, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        if !self.config.channels.slack.enabled {
            info!("Slack channel disabled");
            return;
        }
        if self.config.channels.slack.bot_token.is_empty() {
            warn!("Slack bot_token not configured");
            return;
        }
        // Prefer Socket Mode when app_token is set
        if !self.config.channels.slack.app_token.is_empty() {
            info!("Slack channel starting in Socket Mode");
            let mut backoff = Duration::from_secs(2);
            loop {
                tokio::select! {
                    result = self.run_socket_mode() => {
                        match result {
                            Ok(_) => { info!("Slack Socket Mode exited normally"); }
                            Err(e) => {
                                error!(error = %e, backoff_secs = backoff.as_secs(),
                                    "Slack Socket Mode error, reconnecting");
                                tokio::select! {
                                    _ = tokio::time::sleep(backoff) => {}
                                    _ = shutdown.recv() => {
                                        info!("Slack channel shutting down");
                                        return;
                                    }
                                }
                                backoff = (backoff * 2).min(Duration::from_secs(30));
                                continue;
                            }
                        }
                    }
                    _ = shutdown.recv() => {
                        info!("Slack channel shutting down");
                        return;
                    }
                }
                backoff = Duration::from_secs(2);
            }
        } else {
            self.run_polling(shutdown).await;
        }
    }
}

// ── send_message ──────────────────────────────────────────────────────────────

/// Send a message to a Slack channel, optionally as a thread reply.
/// Long messages are split at newline boundaries to respect Slack's 4000-char
/// limit. A 1.1s delay between chunks stays within Tier 1 rate limit (1 req/s).
pub async fn send_message(config: &Config, chat_id: &str, text: &str) -> Result<()> {
    send_message_threaded(config, chat_id, text, None).await
}

/// Send a message to a Slack channel, replying in a thread if `thread_ts` is provided.
pub async fn send_message_threaded(
    config: &Config,
    chat_id: &str,
    text: &str,
    thread_ts: Option<&str>,
) -> Result<()> {
    crate::rate_limit::slack_limiter().acquire().await;
    let client = shared_client();
    let token = &config.channels.slack.bot_token;

    let chunks = split_message(text, SLACK_MSG_LIMIT);
    for (i, chunk) in chunks.iter().enumerate() {
        let mut body = serde_json::json!({
            "channel": chat_id,
            "text": chunk,
        });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }

        let response = client
            .post(&format!("{}/chat.postMessage", SLACK_API_BASE))
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Failed to send Slack message: {}", e)))?;

        let resp: SlackResponse = response
            .json()
            .await
            .map_err(|e| Error::Channel(format!("Failed to parse Slack response: {}", e)))?;

        if !resp.ok {
            return Err(Error::Channel(format!(
                "Slack API error: {}",
                resp.error.unwrap_or_else(|| "unknown".to_string())
            )));
        }
        if i + 1 < chunks.len() {
            tokio::time::sleep(Duration::from_millis(1100)).await;
        }
    }
    Ok(())
}

/// Split text into chunks at newline boundaries, each at most `max_len` chars.
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
    fn test_slack_message_deserialize() {
        let json = r#"{"user":"U123","text":"hello","ts":"1234567890.123456"}"#;
        let msg: SlackMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.user.as_deref(), Some("U123"));
        assert_eq!(msg.text.as_deref(), Some("hello"));
        assert!(msg.bot_id.is_none());
    }

    #[test]
    fn test_slack_history_response_deserialize() {
        let json = r#"{"ok":true,"messages":[{"user":"U123","text":"hi","ts":"1234567890.000001"}]}"#;
        let resp: SlackHistoryResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.messages.unwrap().len(), 1);
    }

    #[test]
    fn test_split_message_short() {
        let chunks = split_message("hello world", 4000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello world");
    }

    #[test]
    fn test_split_message_long() {
        let line = "a".repeat(100);
        let text = (0..50).map(|_| line.clone()).collect::<Vec<_>>().join("\n");
        let chunks = split_message(&text, 4000);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 4000);
        }
    }

    #[test]
    fn test_split_message_chinese() {
        // Each Chinese char is 3 bytes; 5000 chars = 15000 bytes
        let text = "Slack消息".repeat(1000);
        let chunks = split_message(&text, 4000);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 4000, "chunk too long: {} chars", chunk.chars().count());
        }
    }

    #[test]
    fn test_socket_envelope_deserialize() {
        let json = r#"{"envelope_id":"abc123","type":"events_api","payload":{"event":{"type":"message"}}}"#;
        let env: SocketEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(env.envelope_id.as_deref(), Some("abc123"));
        assert_eq!(env.envelope_type, "events_api");
        assert!(env.payload.is_some());
    }
}
