use blockcell_core::{Config, InboundMessage, OutboundMessage, Paths, Result};
use tokio::sync::mpsc;
use tracing::{error, info};

pub struct ChannelManager {
    config: Config,
    #[allow(dead_code)]
    paths: Paths,
    #[allow(dead_code)]
    inbound_tx: mpsc::Sender<InboundMessage>,
}

impl ChannelManager {
    pub fn new(
        config: Config,
        paths: Paths,
        inbound_tx: mpsc::Sender<InboundMessage>,
    ) -> Self {
        Self {
            config,
            paths,
            inbound_tx,
        }
    }

    pub async fn start_outbound_dispatcher(
        &self,
        mut outbound_rx: mpsc::Receiver<OutboundMessage>,
    ) {
        info!("Outbound dispatcher started");
        
        while let Some(msg) = outbound_rx.recv().await {
            if let Err(e) = self.dispatch_outbound_msg(&msg).await {
                error!(error = %e, channel = %msg.channel, "Failed to dispatch outbound message");
            }
        }
        
        info!("Outbound dispatcher stopped");
    }

    pub async fn dispatch_outbound_msg(&self, msg: &OutboundMessage) -> Result<()> {
        match msg.channel.as_str() {
            "telegram" => {
                #[cfg(feature = "telegram")]
                {
                    crate::telegram::send_message(&self.config, &msg.chat_id, &msg.content).await?;
                }
            }
            "whatsapp" => {
                #[cfg(feature = "whatsapp")]
                {
                    crate::whatsapp::send_message(&self.config, &msg.chat_id, &msg.content).await?;
                }
            }
            "feishu" => {
                #[cfg(feature = "feishu")]
                {
                    crate::feishu::send_message(&self.config, &msg.chat_id, &msg.content).await?;
                }
            }
            "slack" => {
                #[cfg(feature = "slack")]
                {
                    crate::slack::send_message(&self.config, &msg.chat_id, &msg.content).await?;
                }
            }
            "discord" => {
                #[cfg(feature = "discord")]
                {
                    crate::discord::send_message(&self.config, &msg.chat_id, &msg.content).await?;
                }
            }
            "cli" | "cron" | "ws" => {
                // Internal channels — handled directly, not through external channel dispatch
            }
            _ => {
                tracing::warn!(channel = %msg.channel, "Unknown channel for outbound message");
            }
        }
        Ok(())
    }

    pub fn get_status(&self) -> Vec<(String, bool, String)> {
        let mut status = Vec::new();

        // Telegram
        let telegram_enabled = self.config.channels.telegram.enabled;
        let telegram_configured = !self.config.channels.telegram.token.is_empty();
        status.push((
            "telegram".to_string(),
            telegram_enabled && telegram_configured,
            if telegram_configured {
                "configured".to_string()
            } else {
                "token not set".to_string()
            },
        ));

        // WhatsApp
        let whatsapp_enabled = self.config.channels.whatsapp.enabled;
        status.push((
            "whatsapp".to_string(),
            whatsapp_enabled,
            format!("bridge: {}", self.config.channels.whatsapp.bridge_url),
        ));

        // Feishu
        let feishu_enabled = self.config.channels.feishu.enabled;
        let feishu_configured = !self.config.channels.feishu.app_id.is_empty();
        status.push((
            "feishu".to_string(),
            feishu_enabled && feishu_configured,
            if feishu_configured {
                "configured".to_string()
            } else {
                "app_id not set".to_string()
            },
        ));

        // Slack
        let slack_enabled = self.config.channels.slack.enabled;
        let slack_configured = !self.config.channels.slack.bot_token.is_empty();
        status.push((
            "slack".to_string(),
            slack_enabled && slack_configured,
            if slack_configured {
                format!("configured ({} channels)", self.config.channels.slack.channels.len())
            } else {
                "bot_token not set".to_string()
            },
        ));

        // Discord
        let discord_enabled = self.config.channels.discord.enabled;
        let discord_configured = !self.config.channels.discord.bot_token.is_empty();
        status.push((
            "discord".to_string(),
            discord_enabled && discord_configured,
            if discord_configured {
                "configured".to_string()
            } else {
                "bot_token not set".to_string()
            },
        ));

        status
    }
}
