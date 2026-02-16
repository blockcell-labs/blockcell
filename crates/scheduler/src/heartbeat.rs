use chrono::Utc;
use blockcell_core::{InboundMessage, Paths, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

const HEARTBEAT_PROMPT: &str = r#"Read HEARTBEAT.md in your workspace (if it exists).
Follow any instructions or tasks listed there.
If nothing needs attention, reply with just: HEARTBEAT_OK"#;

pub struct HeartbeatService {
    paths: Paths,
    interval: Duration,
    inbound_tx: mpsc::Sender<InboundMessage>,
}

impl HeartbeatService {
    pub fn new(paths: Paths, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        Self {
            paths,
            interval: Duration::from_secs(30 * 60), // 30 minutes
            inbound_tx,
        }
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    fn is_heartbeat_empty(&self) -> bool {
        let path = self.paths.heartbeat_md();
        
        if !path.exists() {
            return true;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return true,
        };

        // Check if content is effectively empty
        for line in content.lines() {
            let trimmed = line.trim();
            
            // Skip empty lines
            if trimmed.is_empty() {
                continue;
            }
            
            // Skip markdown headers
            if trimmed.starts_with('#') {
                continue;
            }
            
            // Skip HTML comments
            if trimmed.starts_with("<!--") && trimmed.ends_with("-->") {
                continue;
            }
            
            // Skip empty checkboxes
            if trimmed == "- [ ]" || trimmed == "- [x]" {
                continue;
            }
            
            // Found actual content
            return false;
        }

        true
    }

    async fn trigger(&self) -> Result<()> {
        if self.is_heartbeat_empty() {
            debug!("Heartbeat file is empty, skipping");
            return Ok(());
        }

        info!("Triggering heartbeat");

        let msg = InboundMessage {
            channel: "heartbeat".to_string(),
            sender_id: "heartbeat".to_string(),
            chat_id: "heartbeat".to_string(),
            content: HEARTBEAT_PROMPT.to_string(),
            media: vec![],
            metadata: serde_json::Value::Null,
            timestamp_ms: Utc::now().timestamp_millis(),
        };

        self.inbound_tx
            .send(msg)
            .await
            .map_err(|e| blockcell_core::Error::Channel(e.to_string()))?;

        Ok(())
    }

    pub async fn run_loop(self: Arc<Self>, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        info!(interval_secs = self.interval.as_secs(), "HeartbeatService started");
        
        let mut interval = tokio::time::interval(self.interval);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.trigger().await {
                        error!(error = %e, "Heartbeat trigger failed");
                    }
                }
                _ = shutdown.recv() => {
                    info!("HeartbeatService shutting down");
                    break;
                }
            }
        }
    }
}
