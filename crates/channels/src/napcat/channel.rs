//! NapCatQQ channel main entry point.
//!
//! This module provides the main NapCatChannel struct that coordinates
//! WebSocket client/server modes and message handling.

use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

use blockcell_core::{Config, InboundMessage};

use super::websocket::{NapCatWsClient, NapCatWsServer};

/// NapCatQQ channel implementation.
///
/// This is the main entry point for the NapCatQQ channel.
/// It coordinates between ws-client and ws-server modes based on configuration.
pub struct NapCatChannel {
    config: Config,
    inbound_tx: mpsc::Sender<InboundMessage>,
}

impl NapCatChannel {
    /// Create a new NapCatChannel instance.
    pub fn new(config: Config, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        Self {
            config,
            inbound_tx,
        }
    }

    /// Get the connection mode from config.
    fn get_mode(&self) -> String {
        self.config.channels.napcat.mode.clone()
    }

    /// Run the channel main loop.
    ///
    /// This method starts the appropriate mode (ws-client or ws-server)
    /// based on the configuration.
    pub async fn run_loop(self: Arc<Self>, shutdown: tokio::sync::broadcast::Receiver<()>) {
        let napcat = &self.config.channels.napcat;

        if !napcat.enabled {
            info!("NapCatQQ channel disabled");
            return;
        }

        let mode = self.get_mode();

        match mode.as_str() {
            "ws-client" | "client" => {
                // "client" for backward compatibility
                if napcat.ws_url.is_empty() {
                    warn!("NapCatQQ ws_url not configured for ws-client mode");
                    return;
                }
                info!("NapCatQQ channel started in WebSocket client mode");
                let client = Arc::new(NapCatWsClient::new(
                    self.config.clone(),
                    self.inbound_tx.clone(),
                ));
                client.run(shutdown).await;
            }
            "ws-server" | "server" => {
                // "server" for backward compatibility
                info!(
                    "NapCatQQ WebSocket server starting on {}:{}",
                    napcat.server_host, napcat.server_port
                );
                let server = Arc::new(NapCatWsServer::new(
                    self.config.clone(),
                    self.inbound_tx.clone(),
                ));
                server.run(shutdown).await;
            }
            _ => {
                warn!(
                    "Unknown NapCatQQ mode: {}, defaulting to ws-client",
                    mode
                );
                if napcat.ws_url.is_empty() {
                    warn!("NapCatQQ ws_url not configured");
                    return;
                }
                let client = Arc::new(NapCatWsClient::new(
                    self.config.clone(),
                    self.inbound_tx.clone(),
                ));
                client.run(shutdown).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_new() {
        let config = Config::default();
        let (tx, _rx) = mpsc::channel(1);
        let channel = NapCatChannel::new(config, tx);
        assert_eq!(channel.get_mode(), "ws-client");
    }

    #[test]
    fn test_channel_mode_from_config() {
        let mut config = Config::default();
        config.channels.napcat.mode = "ws-server".to_string();

        let (tx, _rx) = mpsc::channel(1);
        let channel = NapCatChannel::new(config, tx);
        assert_eq!(channel.get_mode(), "ws-server");
    }

    #[test]
    fn test_backward_compatible_client_mode() {
        let mut config = Config::default();
        // Old "client" mode should still work
        config.channels.napcat.mode = "client".to_string();

        let (tx, _rx) = mpsc::channel(1);
        let channel = NapCatChannel::new(config, tx);
        assert_eq!(channel.get_mode(), "client"); // Mode is preserved, but matched as ws-client
    }

    #[test]
    fn test_backward_compatible_server_mode() {
        let mut config = Config::default();
        // Old "server" mode should still work
        config.channels.napcat.mode = "server".to_string();

        let (tx, _rx) = mpsc::channel(1);
        let channel = NapCatChannel::new(config, tx);
        assert_eq!(channel.get_mode(), "server"); // Mode is preserved, but matched as ws-server
    }
}