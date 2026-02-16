use blockcell_core::{Config, Error, InboundMessage, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

const SLACK_API_BASE: &str = "https://slack.com/api";

#[derive(Debug, Deserialize)]
struct SlackResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SlackEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    challenge: Option<String>,
    #[serde(default)]
    event: Option<SlackEventPayload>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SlackEventPayload {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    ts: Option<String>,
    #[serde(default)]
    bot_id: Option<String>,
    #[serde(default)]
    files: Option<Vec<SlackFile>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SlackFile {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    url_private_download: Option<String>,
    #[serde(default)]
    mimetype: Option<String>,
}

/// Slack RTM (Real Time Messaging) / Socket Mode / Polling-based channel.
///
/// Uses Slack Web API conversations.history polling approach for simplicity.
/// For production, consider upgrading to Socket Mode (wss) or Events API (webhook).
pub struct SlackChannel {
    config: Config,
    client: Client,
    inbound_tx: mpsc::Sender<InboundMessage>,
}

impl SlackChannel {
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
        let allow_from = &self.config.channels.slack.allow_from;
        if allow_from.is_empty() {
            return true;
        }
        allow_from.iter().any(|allowed| allowed == user_id)
    }

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

    pub async fn run_loop(self: Arc<Self>, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        if !self.config.channels.slack.enabled {
            info!("Slack channel disabled");
            return;
        }

        if self.config.channels.slack.bot_token.is_empty() {
            warn!("Slack bot token not configured");
            return;
        }

        info!("Slack channel started (polling mode)");

        // Track latest timestamp per channel to avoid duplicates
        let mut latest_ts: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        let channels = self.config.channels.slack.channels.clone();
        if channels.is_empty() {
            warn!("No Slack channels configured to monitor");
            return;
        }

        // Initialize latest_ts to "now" so we only get new messages
        // Slack ts is typically a string like "1700000000.123456".
        // Use second-level timestamp but include a fractional part for compatibility.
        let now = format!("{}.000000", chrono::Utc::now().timestamp());
        for ch in &channels {
            latest_ts.insert(ch.clone(), now.clone());
        }

        let poll_interval = Duration::from_secs(
            self.config.channels.slack.poll_interval_secs.max(2) as u64,
        );

        loop {
            tokio::select! {
                _ = tokio::time::sleep(poll_interval) => {
                    for channel_id in &channels {
                        let oldest = latest_ts
                            .get(channel_id)
                            .cloned()
                            .unwrap_or_else(|| now.clone());

                        match self.poll_messages(channel_id, &oldest).await {
                            Ok(messages) => {
                                for msg in messages {
                                    // Skip bot messages
                                    if msg.bot_id.is_some() {
                                        continue;
                                    }

                                    let user = msg.user.as_deref().unwrap_or("");
                                    if user.is_empty() {
                                        continue;
                                    }

                                    if !self.is_allowed(user) {
                                        debug!(user = %user, "Slack user not in allowlist, ignoring");
                                        continue;
                                    }

                                    let content = msg.text.unwrap_or_default();
                                    if content.is_empty() {
                                        continue;
                                    }

                                    // Update latest timestamp
                                    if let Some(ts) = &msg.ts {
                                        latest_ts.insert(channel_id.clone(), ts.clone());
                                    }

                                    let inbound = InboundMessage {
                                        channel: "slack".to_string(),
                                        sender_id: user.to_string(),
                                        chat_id: channel_id.clone(),
                                        content,
                                        media: vec![],
                                        metadata: serde_json::json!({
                                            "ts": msg.ts,
                                            "thread_ts": msg.thread_ts,
                                        }),
                                        timestamp_ms: chrono::Utc::now().timestamp_millis(),
                                    };

                                    if let Err(e) = self.inbound_tx.send(inbound).await {
                                        error!(error = %e, "Failed to send Slack inbound message");
                                    }
                                }
                            }
                            Err(e) => {
                                error!(error = %e, channel = %channel_id, "Failed to poll Slack messages");
                            }
                        }
                    }
                }
                _ = shutdown.recv() => {
                    info!("Slack channel shutting down");
                    break;
                }
            }
        }
    }
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

/// Send a message to a Slack channel.
pub async fn send_message(config: &Config, chat_id: &str, text: &str) -> Result<()> {
    let client = Client::new();
    let token = &config.channels.slack.bot_token;

    #[derive(Serialize)]
    struct PostMessage<'a> {
        channel: &'a str,
        text: &'a str,
    }

    let request = PostMessage {
        channel: chat_id,
        text,
    };

    let response = client
        .post(&format!("{}/chat.postMessage", SLACK_API_BASE))
        .header("Authorization", format!("Bearer {}", token))
        .json(&request)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("Failed to send Slack message: {}", e)))?;

    let body: SlackResponse = response
        .json()
        .await
        .map_err(|e| Error::Channel(format!("Failed to parse Slack response: {}", e)))?;

    if !body.ok {
        return Err(Error::Channel(format!(
            "Slack API error: {}",
            body.error.unwrap_or_else(|| "unknown".to_string())
        )));
    }

    Ok(())
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
}
