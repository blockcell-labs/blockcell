use blockcell_core::{Config, Error, InboundMessage, OutboundMessage, Paths, Result};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info};

pub struct ChannelManager {
    config: Config,
    #[allow(dead_code)]
    paths: Paths,
    #[allow(dead_code)]
    inbound_tx: mpsc::Sender<InboundMessage>,
    /// Persistent WhatsApp channel instance for connection reuse.
    #[cfg(feature = "whatsapp")]
    whatsapp_channel: Option<Arc<crate::whatsapp::WhatsAppChannel>>,
}

impl ChannelManager {
    pub fn new(config: Config, paths: Paths, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        Self {
            config,
            paths,
            inbound_tx,
            #[cfg(feature = "whatsapp")]
            whatsapp_channel: None,
        }
    }

    /// Register the running WhatsApp channel so outbound messages can reuse
    /// its persistent WebSocket connection.
    #[cfg(feature = "whatsapp")]
    pub fn set_whatsapp_channel(&mut self, ch: Arc<crate::whatsapp::WhatsAppChannel>) {
        self.whatsapp_channel = Some(ch);
    }

    fn pick_account<'a, T>(
        channel: &str,
        accounts: &'a HashMap<String, T>,
        requested: Option<&str>,
        default: Option<&str>,
    ) -> Result<Option<&'a T>> {
        if accounts.is_empty() {
            return Ok(None);
        }

        if let Some(id) = requested {
            return accounts.get(id).map(Some).ok_or_else(|| {
                Error::Channel(format!(
                    "Unknown account_id '{}' for channel '{}'",
                    id, channel
                ))
            });
        }

        if let Some(id) = default.filter(|v| !v.trim().is_empty()) {
            return accounts.get(id).map(Some).ok_or_else(|| {
                Error::Channel(format!(
                    "default_account_id '{}' not found for channel '{}'",
                    id, channel
                ))
            });
        }

        Ok(accounts.get("default"))
    }

    fn config_for_outbound(&self, msg: &OutboundMessage) -> Result<Config> {
        let mut cfg = self.config.clone();
        let req_account = msg.account_id.as_deref();
        match msg.channel.as_str() {
            "telegram" => {
                if let Some(acc) = Self::pick_account(
                    "telegram",
                    &cfg.channels.telegram.accounts,
                    req_account,
                    cfg.channels.telegram.default_account_id.as_deref(),
                )? {
                    if !acc.enabled {
                        return Err(Error::Channel(
                            "Selected telegram account is disabled".to_string(),
                        ));
                    }
                    cfg.channels.telegram.enabled = acc.enabled;
                    cfg.channels.telegram.token = acc.token.clone();
                    cfg.channels.telegram.allow_from = acc.allow_from.clone();
                    cfg.channels.telegram.proxy = acc.proxy.clone();
                }
            }
            "whatsapp" => {
                if let Some(acc) = Self::pick_account(
                    "whatsapp",
                    &cfg.channels.whatsapp.accounts,
                    req_account,
                    cfg.channels.whatsapp.default_account_id.as_deref(),
                )? {
                    if !acc.enabled {
                        return Err(Error::Channel(
                            "Selected whatsapp account is disabled".to_string(),
                        ));
                    }
                    cfg.channels.whatsapp.enabled = acc.enabled;
                    cfg.channels.whatsapp.bridge_url = acc.bridge_url.clone();
                    cfg.channels.whatsapp.allow_from = acc.allow_from.clone();
                }
            }
            "feishu" => {
                if let Some(acc) = Self::pick_account(
                    "feishu",
                    &cfg.channels.feishu.accounts,
                    req_account,
                    cfg.channels.feishu.default_account_id.as_deref(),
                )? {
                    if !acc.enabled {
                        return Err(Error::Channel(
                            "Selected feishu account is disabled".to_string(),
                        ));
                    }
                    cfg.channels.feishu.enabled = acc.enabled;
                    cfg.channels.feishu.app_id = acc.app_id.clone();
                    cfg.channels.feishu.app_secret = acc.app_secret.clone();
                    cfg.channels.feishu.encrypt_key = acc.encrypt_key.clone();
                    cfg.channels.feishu.verification_token = acc.verification_token.clone();
                    cfg.channels.feishu.allow_from = acc.allow_from.clone();
                }
            }
            "slack" => {
                if let Some(acc) = Self::pick_account(
                    "slack",
                    &cfg.channels.slack.accounts,
                    req_account,
                    cfg.channels.slack.default_account_id.as_deref(),
                )? {
                    if !acc.enabled {
                        return Err(Error::Channel(
                            "Selected slack account is disabled".to_string(),
                        ));
                    }
                    cfg.channels.slack.enabled = acc.enabled;
                    cfg.channels.slack.bot_token = acc.bot_token.clone();
                    cfg.channels.slack.app_token = acc.app_token.clone();
                    cfg.channels.slack.channels = acc.channels.clone();
                    cfg.channels.slack.allow_from = acc.allow_from.clone();
                    cfg.channels.slack.poll_interval_secs = acc.poll_interval_secs;
                }
            }
            "discord" => {
                if let Some(acc) = Self::pick_account(
                    "discord",
                    &cfg.channels.discord.accounts,
                    req_account,
                    cfg.channels.discord.default_account_id.as_deref(),
                )? {
                    if !acc.enabled {
                        return Err(Error::Channel(
                            "Selected discord account is disabled".to_string(),
                        ));
                    }
                    cfg.channels.discord.enabled = acc.enabled;
                    cfg.channels.discord.bot_token = acc.bot_token.clone();
                    cfg.channels.discord.channels = acc.channels.clone();
                    cfg.channels.discord.allow_from = acc.allow_from.clone();
                }
            }
            "dingtalk" => {
                if let Some(acc) = Self::pick_account(
                    "dingtalk",
                    &cfg.channels.dingtalk.accounts,
                    req_account,
                    cfg.channels.dingtalk.default_account_id.as_deref(),
                )? {
                    if !acc.enabled {
                        return Err(Error::Channel(
                            "Selected dingtalk account is disabled".to_string(),
                        ));
                    }
                    cfg.channels.dingtalk.enabled = acc.enabled;
                    cfg.channels.dingtalk.app_key = acc.app_key.clone();
                    cfg.channels.dingtalk.app_secret = acc.app_secret.clone();
                    cfg.channels.dingtalk.robot_code = acc.robot_code.clone();
                    cfg.channels.dingtalk.allow_from = acc.allow_from.clone();
                }
            }
            "wecom" => {
                if let Some(acc) = Self::pick_account(
                    "wecom",
                    &cfg.channels.wecom.accounts,
                    req_account,
                    cfg.channels.wecom.default_account_id.as_deref(),
                )? {
                    if !acc.enabled {
                        return Err(Error::Channel(
                            "Selected wecom account is disabled".to_string(),
                        ));
                    }
                    cfg.channels.wecom.enabled = acc.enabled;
                    cfg.channels.wecom.mode = acc.mode.clone();
                    cfg.channels.wecom.corp_id = acc.corp_id.clone();
                    cfg.channels.wecom.corp_secret = acc.corp_secret.clone();
                    cfg.channels.wecom.agent_id = acc.agent_id;
                    cfg.channels.wecom.bot_id = acc.bot_id.clone();
                    cfg.channels.wecom.bot_secret = acc.bot_secret.clone();
                    cfg.channels.wecom.callback_token = acc.callback_token.clone();
                    cfg.channels.wecom.encoding_aes_key = acc.encoding_aes_key.clone();
                    cfg.channels.wecom.allow_from = acc.allow_from.clone();
                    cfg.channels.wecom.poll_interval_secs = acc.poll_interval_secs;
                    cfg.channels.wecom.ws_url = acc.ws_url.clone();
                    cfg.channels.wecom.ping_interval_secs = acc.ping_interval_secs;
                }
            }
            "lark" => {
                if let Some(acc) = Self::pick_account(
                    "lark",
                    &cfg.channels.lark.accounts,
                    req_account,
                    cfg.channels.lark.default_account_id.as_deref(),
                )? {
                    if !acc.enabled {
                        return Err(Error::Channel(
                            "Selected lark account is disabled".to_string(),
                        ));
                    }
                    cfg.channels.lark.enabled = acc.enabled;
                    cfg.channels.lark.app_id = acc.app_id.clone();
                    cfg.channels.lark.app_secret = acc.app_secret.clone();
                    cfg.channels.lark.encrypt_key = acc.encrypt_key.clone();
                    cfg.channels.lark.verification_token = acc.verification_token.clone();
                    cfg.channels.lark.allow_from = acc.allow_from.clone();
                }
            }
            "qq" => {
                if let Some(acc) = Self::pick_account(
                    "qq",
                    &cfg.channels.qq.accounts,
                    req_account,
                    cfg.channels.qq.default_account_id.as_deref(),
                )? {
                    if !acc.enabled {
                        return Err(Error::Channel(
                            "Selected qq account is disabled".to_string(),
                        ));
                    }
                    cfg.channels.qq.enabled = acc.enabled;
                    cfg.channels.qq.app_id = acc.app_id.clone();
                    cfg.channels.qq.app_secret = acc.app_secret.clone();
                    cfg.channels.qq.environment = acc.environment.clone();
                    cfg.channels.qq.allow_from = acc.allow_from.clone();
                }
            }
            _ => {}
        }
        Ok(cfg)
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
        let send_config = self.config_for_outbound(msg)?;
        match msg.channel.as_str() {
            "telegram" => {
                #[cfg(feature = "telegram")]
                {
                    if !msg.media.is_empty() {
                        for file_path in &msg.media {
                            if let Err(e) = crate::telegram::send_media_message(
                                &send_config,
                                &msg.chat_id,
                                file_path,
                            )
                            .await
                            {
                                error!(error = %e, file = %file_path, "Telegram: failed to send media");
                            }
                        }
                    }
                    if !msg.content.is_empty() {
                        let reply_to = msg
                            .metadata
                            .get("reply_to_message_id")
                            .and_then(|v| v.as_i64());
                        crate::telegram::send_message_reply(
                            &send_config,
                            &msg.chat_id,
                            &msg.content,
                            reply_to,
                        )
                        .await?;
                    }
                }
            }
            "whatsapp" => {
                #[cfg(feature = "whatsapp")]
                {
                    let use_persistent = msg.account_id.is_none()
                        && self.config.channels.whatsapp.accounts.is_empty();
                    if use_persistent {
                        if let Some(ref ch) = self.whatsapp_channel {
                            ch.send(&msg.chat_id, &msg.content).await?;
                        } else {
                            crate::whatsapp::send_message(&send_config, &msg.chat_id, &msg.content)
                                .await?;
                        }
                    } else {
                        crate::whatsapp::send_message(&send_config, &msg.chat_id, &msg.content)
                            .await?;
                    }
                }
            }
            "feishu" => {
                #[cfg(feature = "feishu")]
                {
                    if !msg.media.is_empty() {
                        for file_path in &msg.media {
                            if let Err(e) = crate::feishu::send_media_message(
                                &send_config,
                                &msg.chat_id,
                                file_path,
                            )
                            .await
                            {
                                error!(error = %e, file = %file_path, "Feishu: failed to send media");
                            }
                        }
                    }
                    if !msg.content.is_empty() {
                        let reply_to = msg
                            .metadata
                            .get("reply_to_message_id")
                            .and_then(|v| v.as_str());
                        if let Some(parent_id) = reply_to {
                            crate::feishu::send_reply_message(
                                &send_config,
                                parent_id,
                                &msg.content,
                            )
                            .await?;
                        } else {
                            crate::feishu::send_message(&send_config, &msg.chat_id, &msg.content)
                                .await?;
                        }
                    }
                }
            }
            "slack" => {
                #[cfg(feature = "slack")]
                {
                    if !msg.media.is_empty() {
                        for file_path in &msg.media {
                            if let Err(e) = crate::slack::send_media_message(
                                &send_config,
                                &msg.chat_id,
                                file_path,
                            )
                            .await
                            {
                                error!(error = %e, file = %file_path, "Slack: failed to send media");
                            }
                        }
                    }
                    if !msg.content.is_empty() {
                        let thread_ts = msg.metadata.get("thread_ts").and_then(|v| v.as_str());
                        crate::slack::send_message_threaded(
                            &send_config,
                            &msg.chat_id,
                            &msg.content,
                            thread_ts,
                        )
                        .await?;
                    }
                }
            }
            "discord" => {
                #[cfg(feature = "discord")]
                {
                    if !msg.media.is_empty() {
                        for file_path in &msg.media {
                            if let Err(e) = crate::discord::send_media_message(
                                &send_config,
                                &msg.chat_id,
                                file_path,
                            )
                            .await
                            {
                                error!(error = %e, file = %file_path, "Discord: failed to send media");
                            }
                        }
                    }
                    if !msg.content.is_empty() {
                        let reply_to = msg
                            .metadata
                            .get("reply_to_message_id")
                            .and_then(|v| v.as_str());
                        crate::discord::send_message_reply(
                            &send_config,
                            &msg.chat_id,
                            &msg.content,
                            reply_to,
                        )
                        .await?;
                    }
                }
            }
            "dingtalk" => {
                #[cfg(feature = "dingtalk")]
                {
                    if !msg.media.is_empty() {
                        for file_path in &msg.media {
                            if let Err(e) = crate::dingtalk::send_media_message(
                                &send_config,
                                &msg.chat_id,
                                file_path,
                            )
                            .await
                            {
                                error!(error = %e, file = %file_path, "DingTalk: failed to send media");
                            }
                        }
                    }
                    if !msg.content.is_empty() {
                        crate::dingtalk::send_message(&send_config, &msg.chat_id, &msg.content)
                            .await?;
                    }
                }
            }
            "wecom" => {
                #[cfg(feature = "wecom")]
                {
                    let wecom_mode = send_config.channels.wecom.mode.trim().to_lowercase();
                    let is_long_conn = wecom_mode == "long_connection"
                        || wecom_mode == "long-connection"
                        || wecom_mode == "stream";
                    if !msg.media.is_empty() {
                        // In long_connection mode we combine text + image into one message to
                        // avoid sending two finish:true replies for the same req_id.
                        // Pass content as caption to the first media file; clear it for the rest.
                        let mut remaining_caption = msg.content.as_str();
                        for file_path in &msg.media {
                            if let Err(e) = crate::wecom::send_media_message(
                                &send_config,
                                &msg.chat_id,
                                file_path,
                                if is_long_conn { remaining_caption } else { "" },
                            )
                            .await
                            {
                                error!(error = %e, file = %file_path, "WeCom: failed to send media");
                            }
                            remaining_caption = "";
                        }
                    }
                    // For long_connection mode skip the separate text send when media was present
                    // (the caption was already included in the image message above).
                    let skip_text = is_long_conn && !msg.media.is_empty();
                    if !msg.content.is_empty() && !skip_text {
                        crate::wecom::send_message(&send_config, &msg.chat_id, &msg.content)
                            .await?;
                    }
                }
            }
            "lark" => {
                #[cfg(feature = "lark")]
                {
                    if !msg.media.is_empty() {
                        for file_path in &msg.media {
                            if let Err(e) = crate::lark::send_media_message(
                                &send_config,
                                &msg.chat_id,
                                file_path,
                            )
                            .await
                            {
                                error!(error = %e, file = %file_path, "Lark: failed to send media");
                            }
                        }
                    }
                    if !msg.content.is_empty() {
                        let reply_to = msg
                            .metadata
                            .get("reply_to_message_id")
                            .and_then(|v| v.as_str());
                        if let Some(parent_id) = reply_to {
                            crate::lark::send_reply_message(&send_config, parent_id, &msg.content)
                                .await?;
                        } else {
                            crate::lark::send_message(&send_config, &msg.chat_id, &msg.content)
                                .await?;
                        }
                    }
                }
            }
            "qq" => {
                #[cfg(feature = "qq")]
                {
                    if !msg.media.is_empty() {
                        for file_path in &msg.media {
                            if let Err(e) = crate::qq::send_media_message(
                                &send_config,
                                &msg.chat_id,
                                file_path,
                            )
                            .await
                            {
                                error!(error = %e, file = %file_path, "QQ: failed to send media");
                            }
                        }
                    }
                    if !msg.content.is_empty() {
                        crate::qq::send_message(&send_config, &msg.chat_id, &msg.content)
                            .await?;
                    }
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

    fn missing_config_detail(channel: &str) -> &'static str {
        match channel {
            "telegram" => "token not set",
            "whatsapp" => "bridge_url not set",
            "feishu" => "app_id not set",
            "slack" => "bot_token not set",
            "discord" => "bot_token not set",
            "dingtalk" => "app_key not set",
            "wecom" => "corp_id not set",
            "lark" => "app_id not set",
            "qq" => "app_id not set",
            _ => "not configured",
        }
    }

    fn channel_status(&self, channel: &str) -> (bool, String) {
        if !self.config.is_external_channel_enabled(channel) {
            return (false, "disabled".to_string());
        }

        let listeners = crate::account::listener_labels(&self.config, channel);
        if !listeners.is_empty() {
            let noun = if listeners.len() == 1 { "listener" } else { "listeners" };
            return (
                true,
                format!(
                    "{} {} active: {}",
                    listeners.len(),
                    noun,
                    listeners.join(", ")
                ),
            );
        }

        if crate::account::channel_configured(&self.config, channel) {
            return (false, "enabled but no routable listener".to_string());
        }

        (false, Self::missing_config_detail(channel).to_string())
    }

    pub fn get_status(&self) -> Vec<(String, bool, String)> {
        let channels = [
            "telegram",
            "whatsapp",
            "feishu",
            "slack",
            "discord",
            "dingtalk",
            "wecom",
            "lark",
            "qq",
        ];

        channels
            .into_iter()
            .map(|channel| {
                let (active, detail) = self.channel_status(channel);
                (channel.to_string(), active, detail)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::config::TelegramAccountConfig;

    #[test]
    fn test_pick_account_prefers_requested_id() {
        let mut accounts = HashMap::new();
        accounts.insert("a".to_string(), 1u8);
        accounts.insert("b".to_string(), 2u8);

        let picked = ChannelManager::pick_account("telegram", &accounts, Some("b"), Some("a"))
            .expect("pick account should succeed")
            .copied();
        assert_eq!(picked, Some(2));
    }

    #[test]
    fn test_pick_account_uses_default_and_named_default_fallback() {
        let mut accounts = HashMap::new();
        accounts.insert("default".to_string(), 1u8);
        accounts.insert("main".to_string(), 2u8);

        let by_default_id = ChannelManager::pick_account("telegram", &accounts, None, Some("main"))
            .expect("pick by default_account_id should succeed")
            .copied();
        assert_eq!(by_default_id, Some(2));

        let by_named_default = ChannelManager::pick_account("telegram", &accounts, None, None)
            .expect("fallback to 'default' should succeed")
            .copied();
        assert_eq!(by_named_default, Some(1));
    }

    #[test]
    fn test_pick_account_errors_for_unknown_requested_id() {
        let mut accounts = HashMap::new();
        accounts.insert("default".to_string(), 1u8);

        let err = ChannelManager::pick_account("telegram", &accounts, Some("missing"), None)
            .expect_err("unknown requested account should fail");
        assert!(
            err.to_string().contains("Unknown account_id 'missing'"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn test_config_for_outbound_telegram_account_selection() {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.accounts.insert(
            "default".to_string(),
            TelegramAccountConfig {
                enabled: true,
                token: "tg-default".to_string(),
                allow_from: vec!["u1".to_string()],
                proxy: None,
            },
        );
        config.channels.telegram.accounts.insert(
            "alt".to_string(),
            TelegramAccountConfig {
                enabled: true,
                token: "tg-alt".to_string(),
                allow_from: vec!["u2".to_string()],
                proxy: Some("http://proxy.local".to_string()),
            },
        );
        config.channels.telegram.default_account_id = Some("default".to_string());

        let (tx, _rx) = mpsc::channel(1);
        let manager = ChannelManager::new(config, Paths::new(), tx);

        let msg_default = OutboundMessage::new("telegram", "chat1", "hello");
        let send_cfg_default = manager
            .config_for_outbound(&msg_default)
            .expect("default account selection should succeed");
        assert_eq!(send_cfg_default.channels.telegram.token, "tg-default");
        assert_eq!(send_cfg_default.channels.telegram.allow_from, vec!["u1"]);

        let mut msg_alt = OutboundMessage::new("telegram", "chat1", "hello");
        msg_alt.account_id = Some("alt".to_string());
        let send_cfg_alt = manager
            .config_for_outbound(&msg_alt)
            .expect("explicit account selection should succeed");
        assert_eq!(send_cfg_alt.channels.telegram.token, "tg-alt");
        assert_eq!(send_cfg_alt.channels.telegram.allow_from, vec!["u2"]);
        assert_eq!(
            send_cfg_alt.channels.telegram.proxy.as_deref(),
            Some("http://proxy.local")
        );
    }


    #[test]
    fn test_get_status_uses_multi_account_listener_labels() {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.accounts.insert(
            "main".to_string(),
            TelegramAccountConfig {
                enabled: true,
                token: "tg-main".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );

        let (tx, _rx) = mpsc::channel(1);
        let manager = ChannelManager::new(config, Paths::new(), tx);
        let telegram = manager
            .get_status()
            .into_iter()
            .find(|(name, _, _)| name == "telegram")
            .expect("telegram status should exist");

        assert!(telegram.1, "telegram should be active when account listener exists");
        assert!(telegram.2.contains("telegram:main"), "unexpected detail: {}", telegram.2);
    }
}
