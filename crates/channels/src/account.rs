use blockcell_core::config::{
    DingTalkAccountConfig, DiscordAccountConfig, FeishuAccountConfig, LarkAccountConfig,
    SlackAccountConfig, TelegramAccountConfig, WeComAccountConfig, WhatsAppAccountConfig,
};
#[cfg(feature = "qq")]
use blockcell_core::config::QQAccountConfig;
use blockcell_core::Config;
use std::collections::HashMap;

fn resolve_account_id<T>(
    accounts: &HashMap<String, T>,
    is_enabled: impl Fn(&T) -> bool,
    matches_active: impl Fn(&T) -> bool,
) -> Option<String> {
    if accounts.is_empty() {
        return None;
    }

    let enabled_ids: Vec<String> = accounts
        .iter()
        .filter(|(_, account)| is_enabled(account))
        .map(|(account_id, _)| account_id.clone())
        .collect();

    if enabled_ids.is_empty() {
        return None;
    }

    let matched_ids: Vec<String> = accounts
        .iter()
        .filter(|(_, account)| is_enabled(account) && matches_active(account))
        .map(|(account_id, _)| account_id.clone())
        .collect();

    if matched_ids.len() == 1 {
        return matched_ids.into_iter().next();
    }

    if matched_ids.is_empty() && enabled_ids.len() == 1 {
        return enabled_ids.into_iter().next();
    }

    None
}

pub(crate) fn telegram_account_id(config: &Config) -> Option<String> {
    let telegram = &config.channels.telegram;
    resolve_account_id(
        &telegram.accounts,
        |account| account.enabled,
        |account| !telegram.token.is_empty() && account.token == telegram.token,
    )
}

pub(crate) fn slack_account_id(config: &Config) -> Option<String> {
    let slack = &config.channels.slack;
    resolve_account_id(
        &slack.accounts,
        |account| account.enabled,
        |account| {
            (!slack.bot_token.is_empty() && account.bot_token == slack.bot_token)
                || (!slack.app_token.is_empty() && account.app_token == slack.app_token)
        },
    )
}

pub(crate) fn feishu_account_id(config: &Config) -> Option<String> {
    let feishu = &config.channels.feishu;
    resolve_account_id(
        &feishu.accounts,
        |account| account.enabled,
        |account| !feishu.app_id.is_empty() && account.app_id == feishu.app_id,
    )
}

pub(crate) fn discord_account_id(config: &Config) -> Option<String> {
    let discord = &config.channels.discord;
    resolve_account_id(
        &discord.accounts,
        |account| account.enabled,
        |account| !discord.bot_token.is_empty() && account.bot_token == discord.bot_token,
    )
}

pub(crate) fn dingtalk_account_id(config: &Config) -> Option<String> {
    let dingtalk = &config.channels.dingtalk;
    resolve_account_id(
        &dingtalk.accounts,
        |account| account.enabled,
        |account| !dingtalk.app_key.is_empty() && account.app_key == dingtalk.app_key,
    )
}

pub(crate) fn wecom_account_id(config: &Config) -> Option<String> {
    let wecom = &config.channels.wecom;
    resolve_account_id(
        &wecom.accounts,
        |account| account.enabled,
        |account| {
            !wecom.corp_id.is_empty()
                && account.corp_id == wecom.corp_id
                && (wecom.agent_id <= 0 || account.agent_id == wecom.agent_id)
        },
    )
}

pub(crate) fn whatsapp_account_id(config: &Config) -> Option<String> {
    let whatsapp = &config.channels.whatsapp;
    resolve_account_id(
        &whatsapp.accounts,
        |account| account.enabled,
        |account| !whatsapp.bridge_url.is_empty() && account.bridge_url == whatsapp.bridge_url,
    )
}

pub(crate) fn lark_account_id(config: &Config) -> Option<String> {
    let lark = &config.channels.lark;
    resolve_account_id(
        &lark.accounts,
        |account| account.enabled,
        |account| !lark.app_id.is_empty() && account.app_id == lark.app_id,
    )
}

#[cfg(feature = "qq")]
pub(crate) fn qq_account_id(config: &Config) -> Option<String> {
    let qq = &config.channels.qq;
    resolve_account_id(
        &qq.accounts,
        |account| account.enabled,
        |account| !qq.app_id.is_empty() && account.app_id == qq.app_id,
    )
}


#[derive(Debug, Clone)]
pub struct ListenerConfig {
    pub label: String,
    pub account_id: Option<String>,
    pub config: Config,
}

fn legacy_listener_config(channel: &str, config: &Config) -> Vec<ListenerConfig> {
    vec![ListenerConfig {
        label: channel.to_string(),
        account_id: None,
        config: config.clone(),
    }]
}

fn scoped_listener_configs<T: Clone>(
    channel: &str,
    config: &Config,
    accounts: &HashMap<String, T>,
    is_listener_ready: impl Fn(&T) -> bool,
    has_legacy_config: impl Fn(&Config) -> bool,
    apply: impl Fn(&mut Config, &str, &T),
) -> Vec<ListenerConfig> {
    let mut active_accounts: Vec<(String, T)> = accounts
        .iter()
        .filter(|(_, account)| is_listener_ready(account))
        .map(|(account_id, account)| (account_id.clone(), account.clone()))
        .collect();
    active_accounts.sort_by(|left, right| left.0.cmp(&right.0));

    if active_accounts.is_empty() {
        if has_legacy_config(config) {
            return legacy_listener_config(channel, config);
        }
        return Vec::new();
    }

    active_accounts
        .into_iter()
        .map(|(account_id, account)| {
            let mut scoped = config.clone();
            apply(&mut scoped, &account_id, &account);
            ListenerConfig {
                label: format!("{}:{}", channel, account_id),
                account_id: Some(account_id),
                config: scoped,
            }
        })
        .collect()
}

pub fn telegram_listener_configs(config: &Config) -> Vec<ListenerConfig> {
    scoped_listener_configs(
        "telegram",
        config,
        &config.channels.telegram.accounts,
        |account| account.enabled && !account.token.is_empty(),
        |cfg| !cfg.channels.telegram.token.is_empty(),
        |scoped, account_id, account: &TelegramAccountConfig| {
            scoped.channels.telegram.enabled = account.enabled;
            scoped.channels.telegram.token = account.token.clone();
            scoped.channels.telegram.allow_from = account.allow_from.clone();
            scoped.channels.telegram.proxy = account.proxy.clone();
            scoped.channels.telegram.accounts =
                HashMap::from([(account_id.to_string(), account.clone())]);
            scoped.channels.telegram.default_account_id = Some(account_id.to_string());
        },
    )
}

pub fn slack_listener_configs(config: &Config) -> Vec<ListenerConfig> {
    scoped_listener_configs(
        "slack",
        config,
        &config.channels.slack.accounts,
        |account| account.enabled && !account.bot_token.is_empty(),
        |cfg| !cfg.channels.slack.bot_token.is_empty(),
        |scoped, account_id, account: &SlackAccountConfig| {
            scoped.channels.slack.enabled = account.enabled;
            scoped.channels.slack.bot_token = account.bot_token.clone();
            scoped.channels.slack.app_token = account.app_token.clone();
            scoped.channels.slack.channels = account.channels.clone();
            scoped.channels.slack.allow_from = account.allow_from.clone();
            scoped.channels.slack.poll_interval_secs = account.poll_interval_secs;
            scoped.channels.slack.accounts =
                HashMap::from([(account_id.to_string(), account.clone())]);
            scoped.channels.slack.default_account_id = Some(account_id.to_string());
        },
    )
}

pub fn discord_listener_configs(config: &Config) -> Vec<ListenerConfig> {
    scoped_listener_configs(
        "discord",
        config,
        &config.channels.discord.accounts,
        |account| account.enabled && !account.bot_token.is_empty(),
        |cfg| !cfg.channels.discord.bot_token.is_empty(),
        |scoped, account_id, account: &DiscordAccountConfig| {
            scoped.channels.discord.enabled = account.enabled;
            scoped.channels.discord.bot_token = account.bot_token.clone();
            scoped.channels.discord.channels = account.channels.clone();
            scoped.channels.discord.allow_from = account.allow_from.clone();
            scoped.channels.discord.accounts =
                HashMap::from([(account_id.to_string(), account.clone())]);
            scoped.channels.discord.default_account_id = Some(account_id.to_string());
        },
    )
}

pub fn dingtalk_listener_configs(config: &Config) -> Vec<ListenerConfig> {
    scoped_listener_configs(
        "dingtalk",
        config,
        &config.channels.dingtalk.accounts,
        |account| account.enabled && !account.app_key.is_empty(),
        |cfg| !cfg.channels.dingtalk.app_key.is_empty(),
        |scoped, account_id, account: &DingTalkAccountConfig| {
            scoped.channels.dingtalk.enabled = account.enabled;
            scoped.channels.dingtalk.app_key = account.app_key.clone();
            scoped.channels.dingtalk.app_secret = account.app_secret.clone();
            scoped.channels.dingtalk.robot_code = account.robot_code.clone();
            scoped.channels.dingtalk.allow_from = account.allow_from.clone();
            scoped.channels.dingtalk.accounts =
                HashMap::from([(account_id.to_string(), account.clone())]);
            scoped.channels.dingtalk.default_account_id = Some(account_id.to_string());
        },
    )
}

pub fn wecom_listener_configs(config: &Config) -> Vec<ListenerConfig> {
    scoped_listener_configs(
        "wecom",
        config,
        &config.channels.wecom.accounts,
        |account| {
            account.enabled
                && (!account.corp_id.is_empty()
                    || (account.mode == "long_connection"
                        || account.mode == "long-connection"
                        || account.mode == "stream")
                        && !account.bot_id.is_empty())
        },
        |cfg| {
            let mode = cfg.channels.wecom.mode.as_str();
            !cfg.channels.wecom.corp_id.is_empty()
                || ((mode == "long_connection" || mode == "long-connection" || mode == "stream")
                    && !cfg.channels.wecom.bot_id.is_empty())
        },
        |scoped, account_id, account: &WeComAccountConfig| {
            scoped.channels.wecom.enabled = account.enabled;
            scoped.channels.wecom.mode = account.mode.clone();
            scoped.channels.wecom.corp_id = account.corp_id.clone();
            scoped.channels.wecom.corp_secret = account.corp_secret.clone();
            scoped.channels.wecom.agent_id = account.agent_id;
            scoped.channels.wecom.bot_id = account.bot_id.clone();
            scoped.channels.wecom.bot_secret = account.bot_secret.clone();
            scoped.channels.wecom.callback_token = account.callback_token.clone();
            scoped.channels.wecom.encoding_aes_key = account.encoding_aes_key.clone();
            scoped.channels.wecom.allow_from = account.allow_from.clone();
            scoped.channels.wecom.poll_interval_secs = account.poll_interval_secs;
            scoped.channels.wecom.ws_url = account.ws_url.clone();
            scoped.channels.wecom.ping_interval_secs = account.ping_interval_secs;
            scoped.channels.wecom.accounts =
                HashMap::from([(account_id.to_string(), account.clone())]);
            scoped.channels.wecom.default_account_id = Some(account_id.to_string());
        },
    )
}

pub fn feishu_scoped_configs(config: &Config) -> Vec<ListenerConfig> {
    scoped_listener_configs(
        "feishu",
        config,
        &config.channels.feishu.accounts,
        |account| account.enabled && !account.app_id.is_empty(),
        |cfg| !cfg.channels.feishu.app_id.is_empty(),
        |scoped, account_id, account: &FeishuAccountConfig| {
            scoped.channels.feishu.enabled = account.enabled;
            scoped.channels.feishu.app_id = account.app_id.clone();
            scoped.channels.feishu.app_secret = account.app_secret.clone();
            scoped.channels.feishu.encrypt_key = account.encrypt_key.clone();
            scoped.channels.feishu.verification_token = account.verification_token.clone();
            scoped.channels.feishu.allow_from = account.allow_from.clone();
            scoped.channels.feishu.accounts =
                HashMap::from([(account_id.to_string(), account.clone())]);
            scoped.channels.feishu.default_account_id = Some(account_id.to_string());
        },
    )
}

pub fn lark_scoped_configs(config: &Config) -> Vec<ListenerConfig> {
    scoped_listener_configs(
        "lark",
        config,
        &config.channels.lark.accounts,
        |account| account.enabled && !account.app_id.is_empty(),
        |cfg| !cfg.channels.lark.app_id.is_empty(),
        |scoped, account_id, account: &LarkAccountConfig| {
            scoped.channels.lark.enabled = account.enabled;
            scoped.channels.lark.app_id = account.app_id.clone();
            scoped.channels.lark.app_secret = account.app_secret.clone();
            scoped.channels.lark.encrypt_key = account.encrypt_key.clone();
            scoped.channels.lark.verification_token = account.verification_token.clone();
            scoped.channels.lark.allow_from = account.allow_from.clone();
            scoped.channels.lark.accounts =
                HashMap::from([(account_id.to_string(), account.clone())]);
            scoped.channels.lark.default_account_id = Some(account_id.to_string());
        },
    )
}

#[cfg(feature = "qq")]
pub fn qq_listener_configs(config: &Config) -> Vec<ListenerConfig> {
    scoped_listener_configs(
        "qq",
        config,
        &config.channels.qq.accounts,
        |account| account.enabled && !account.app_id.is_empty(),
        |cfg| !cfg.channels.qq.app_id.is_empty(),
        |scoped, account_id, account: &QQAccountConfig| {
            scoped.channels.qq.enabled = account.enabled;
            scoped.channels.qq.app_id = account.app_id.clone();
            scoped.channels.qq.app_secret = account.app_secret.clone();
            scoped.channels.qq.environment = account.environment.clone();
            scoped.channels.qq.mode = account.mode.clone();
            scoped.channels.qq.allow_from = account.allow_from.clone();
            scoped.channels.qq.accounts = HashMap::from([(account_id.to_string(), account.clone())]);
            scoped.channels.qq.default_account_id = Some(account_id.to_string());
        },
    )
}

fn has_enabled_account<T>(accounts: &HashMap<String, T>, is_enabled: impl Fn(&T) -> bool) -> bool {
    accounts.values().any(is_enabled)
}

pub fn channel_configured(config: &Config, channel: &str) -> bool {
    match channel {
        "telegram" => {
            !config.channels.telegram.token.is_empty()
                || has_enabled_account(&config.channels.telegram.accounts, |account| {
                    account.enabled && !account.token.is_empty()
                })
        }
        "whatsapp" => {
            !config.channels.whatsapp.bridge_url.is_empty()
                || has_enabled_account(&config.channels.whatsapp.accounts, |account| {
                    account.enabled && !account.bridge_url.is_empty()
                })
        }
        "feishu" => {
            !config.channels.feishu.app_id.is_empty()
                || has_enabled_account(&config.channels.feishu.accounts, |account| {
                    account.enabled && !account.app_id.is_empty()
                })
        }
        "slack" => {
            !config.channels.slack.bot_token.is_empty()
                || has_enabled_account(&config.channels.slack.accounts, |account| {
                    account.enabled && !account.bot_token.is_empty()
                })
        }
        "discord" => {
            !config.channels.discord.bot_token.is_empty()
                || has_enabled_account(&config.channels.discord.accounts, |account| {
                    account.enabled && !account.bot_token.is_empty()
                })
        }
        "dingtalk" => {
            !config.channels.dingtalk.app_key.is_empty()
                || has_enabled_account(&config.channels.dingtalk.accounts, |account| {
                    account.enabled && !account.app_key.is_empty()
                })
        }
        "wecom" => {
            !config.channels.wecom.corp_id.is_empty()
                || has_enabled_account(&config.channels.wecom.accounts, |account| {
                    account.enabled && !account.corp_id.is_empty()
                })
        }
        "lark" => {
            !config.channels.lark.app_id.is_empty()
                || has_enabled_account(&config.channels.lark.accounts, |account| {
                    account.enabled && !account.app_id.is_empty()
                })
        }
        "qq" => {
            !config.channels.qq.app_id.is_empty()
                || has_enabled_account(&config.channels.qq.accounts, |account| {
                    account.enabled && !account.app_id.is_empty()
                })
        }
        _ => false,
    }
}

pub fn listener_labels(config: &Config, channel: &str) -> Vec<String> {
    if !config.is_external_channel_enabled(channel) || !channel_configured(config, channel) {
        return Vec::new();
    }

    #[cfg(feature = "qq")]
    if channel == "qq" {
        let mut labels: Vec<String> = qq_listener_configs(config)
            .into_iter()
            .map(|listener| listener.label)
            .collect();
        labels.sort();
        return labels;
    }

    let mut labels = match channel {
        "telegram" => telegram_listener_configs(config),
        "whatsapp" => whatsapp_listener_configs(config),
        "feishu" => feishu_scoped_configs(config),
        "slack" => slack_listener_configs(config),
        "discord" => discord_listener_configs(config),
        "dingtalk" => dingtalk_listener_configs(config),
        "wecom" => wecom_listener_configs(config),
        "lark" => lark_scoped_configs(config),
        "qq" => qq_listener_configs(config),
        _ => Vec::new(),
    }
    .into_iter()
    .map(|listener| listener.label)
    .collect::<Vec<_>>();

    labels.sort();
    labels
}

pub fn whatsapp_listener_configs(config: &Config) -> Vec<ListenerConfig> {
    scoped_listener_configs(
        "whatsapp",
        config,
        &config.channels.whatsapp.accounts,
        |account| account.enabled && !account.bridge_url.is_empty(),
        |cfg| !cfg.channels.whatsapp.bridge_url.is_empty(),
        |scoped, account_id, account: &WhatsAppAccountConfig| {
            scoped.channels.whatsapp.enabled = account.enabled;
            scoped.channels.whatsapp.bridge_url = account.bridge_url.clone();
            scoped.channels.whatsapp.allow_from = account.allow_from.clone();
            scoped.channels.whatsapp.accounts =
                HashMap::from([(account_id.to_string(), account.clone())]);
            scoped.channels.whatsapp.default_account_id = Some(account_id.to_string());
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::config::{
        DingTalkAccountConfig, DiscordAccountConfig, FeishuAccountConfig, LarkAccountConfig,
        SlackAccountConfig, TelegramAccountConfig, WeComAccountConfig, WhatsAppAccountConfig,
    };

    #[test]
    fn test_telegram_listener_configs_expand_enabled_accounts() {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.token = "legacy-token".to_string();
        config.channels.telegram.accounts.insert(
            "main".to_string(),
            TelegramAccountConfig {
                enabled: true,
                token: "tg-main".to_string(),
                allow_from: vec!["u1".to_string()],
                proxy: Some("http://proxy-main".to_string()),
            },
        );
        config.channels.telegram.accounts.insert(
            "disabled".to_string(),
            TelegramAccountConfig {
                enabled: false,
                token: "tg-disabled".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );

        let listeners = telegram_listener_configs(&config);
        assert_eq!(listeners.len(), 1);
        assert_eq!(listeners[0].label, "telegram:main");
        assert_eq!(listeners[0].account_id.as_deref(), Some("main"));
        assert_eq!(listeners[0].config.channels.telegram.token, "tg-main");
        assert_eq!(
            listeners[0]
                .config
                .channels
                .telegram
                .default_account_id
                .as_deref(),
            Some("main")
        );
        assert_eq!(listeners[0].config.channels.telegram.accounts.len(), 1);
        assert!(listeners[0]
            .config
            .channels
            .telegram
            .accounts
            .contains_key("main"));
    }

    #[test]
    fn test_telegram_listener_configs_fallback_to_legacy_config() {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.token = "legacy-token".to_string();

        let listeners = telegram_listener_configs(&config);
        assert_eq!(listeners.len(), 1);
        assert_eq!(listeners[0].label, "telegram");
        assert_eq!(listeners[0].account_id, None);
        assert_eq!(listeners[0].config.channels.telegram.token, "legacy-token");
    }

    #[test]
    fn test_telegram_listener_configs_skip_incomplete_enabled_accounts() {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.accounts.insert(
            "broken".to_string(),
            TelegramAccountConfig {
                enabled: true,
                token: "".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );

        let listeners = telegram_listener_configs(&config);
        assert!(listeners.is_empty());
    }

    #[test]
    fn test_telegram_listener_configs_fallback_to_legacy_when_accounts_not_routable() {
        let mut config = Config::default();
        config.channels.telegram.enabled = true;
        config.channels.telegram.token = "legacy-token".to_string();
        config.channels.telegram.accounts.insert(
            "disabled".to_string(),
            TelegramAccountConfig {
                enabled: false,
                token: "tg-disabled".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );
        config.channels.telegram.accounts.insert(
            "broken".to_string(),
            TelegramAccountConfig {
                enabled: true,
                token: "".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );

        let listeners = telegram_listener_configs(&config);
        assert_eq!(listeners.len(), 1);
        assert_eq!(listeners[0].label, "telegram");
        assert_eq!(listeners[0].account_id, None);
        assert_eq!(listeners[0].config.channels.telegram.token, "legacy-token");
    }

    #[test]
    fn test_feishu_scoped_configs_expand_enabled_accounts() {
        let mut config = Config::default();
        config.channels.feishu.enabled = true;
        config.channels.feishu.accounts.insert(
            "office".to_string(),
            blockcell_core::config::FeishuAccountConfig {
                enabled: true,
                app_id: "cli_office".to_string(),
                app_secret: "secret_office".to_string(),
                encrypt_key: "enc_office".to_string(),
                verification_token: "verify_office".to_string(),
                allow_from: vec!["u1".to_string()],
            },
        );
        config.channels.feishu.accounts.insert(
            "disabled".to_string(),
            blockcell_core::config::FeishuAccountConfig {
                enabled: false,
                app_id: "cli_disabled".to_string(),
                app_secret: "secret_disabled".to_string(),
                encrypt_key: String::new(),
                verification_token: String::new(),
                allow_from: vec![],
            },
        );

        let listeners = feishu_scoped_configs(&config);
        assert_eq!(listeners.len(), 1);
        assert_eq!(listeners[0].label, "feishu:office");
        assert_eq!(listeners[0].account_id.as_deref(), Some("office"));
        assert_eq!(listeners[0].config.channels.feishu.app_id, "cli_office");
        assert_eq!(
            listeners[0]
                .config
                .channels
                .feishu
                .default_account_id
                .as_deref(),
            Some("office")
        );
        assert_eq!(listeners[0].config.channels.feishu.accounts.len(), 1);
    }

    #[test]
    fn test_wecom_listener_configs_apply_account_specific_credentials() {
        let mut config = Config::default();
        config.channels.wecom.enabled = true;
        config.channels.wecom.accounts.insert(
            "ops".to_string(),
            WeComAccountConfig {
                enabled: true,
                mode: "webhook".to_string(),
                corp_id: "corp-ops".to_string(),
                corp_secret: "secret-ops".to_string(),
                agent_id: 7,
                bot_id: String::new(),
                bot_secret: String::new(),
                callback_token: "token-ops".to_string(),
                encoding_aes_key: "aes-ops".to_string(),
                allow_from: vec!["alice".to_string()],
                poll_interval_secs: 99,
                ws_url: String::new(),
                ping_interval_secs: 30,
            },
        );

        let listeners = wecom_listener_configs(&config);
        assert_eq!(listeners.len(), 1);
        assert_eq!(listeners[0].label, "wecom:ops");
        assert_eq!(listeners[0].account_id.as_deref(), Some("ops"));
        assert_eq!(listeners[0].config.channels.wecom.corp_id, "corp-ops");
        assert_eq!(listeners[0].config.channels.wecom.agent_id, 7);
        assert_eq!(
            listeners[0].config.channels.wecom.callback_token,
            "token-ops"
        );
        assert_eq!(
            listeners[0]
                .config
                .channels
                .wecom
                .default_account_id
                .as_deref(),
            Some("ops")
        );
        assert_eq!(listeners[0].config.channels.wecom.accounts.len(), 1);
    }

    #[test]
    fn test_listener_labels_return_expanded_accounts_for_enabled_channel() {
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
        config.channels.telegram.accounts.insert(
            "backup".to_string(),
            TelegramAccountConfig {
                enabled: true,
                token: "tg-backup".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );

        let listeners = listener_labels(&config, "telegram");
        assert_eq!(
            listeners,
            vec!["telegram:backup".to_string(), "telegram:main".to_string()]
        );
    }

    #[test]
    fn test_listener_labels_empty_for_disabled_channel() {
        let mut config = Config::default();
        config.channels.telegram.enabled = false;
        config.channels.telegram.token = "legacy-token".to_string();

        assert!(listener_labels(&config, "telegram").is_empty());
    }

    #[test]
    fn test_telegram_account_id_matches_active_token() {
        let mut config = Config::default();
        config.channels.telegram.token = "tg-main".to_string();
        config.channels.telegram.accounts.insert(
            "main".to_string(),
            TelegramAccountConfig {
                enabled: true,
                token: "tg-main".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );

        assert_eq!(telegram_account_id(&config).as_deref(), Some("main"));
    }

    #[test]
    fn test_slack_account_id_returns_none_when_multiple_accounts_do_not_match() {
        let mut config = Config::default();
        config.channels.slack.bot_token = "legacy".to_string();
        config.channels.slack.accounts.insert(
            "a".to_string(),
            SlackAccountConfig {
                enabled: true,
                bot_token: "a-token".to_string(),
                app_token: "a-app".to_string(),
                channels: vec![],
                allow_from: vec![],
                poll_interval_secs: 3,
            },
        );
        config.channels.slack.accounts.insert(
            "b".to_string(),
            SlackAccountConfig {
                enabled: true,
                bot_token: "b-token".to_string(),
                app_token: "b-app".to_string(),
                channels: vec![],
                allow_from: vec![],
                poll_interval_secs: 3,
            },
        );

        assert_eq!(slack_account_id(&config), None);
    }

    #[test]
    fn test_feishu_account_id_falls_back_to_only_enabled_account() {
        let mut config = Config::default();
        config.channels.feishu.accounts.insert(
            "only".to_string(),
            FeishuAccountConfig {
                enabled: true,
                app_id: "app-1".to_string(),
                app_secret: "secret-1".to_string(),
                encrypt_key: String::new(),
                verification_token: String::new(),
                allow_from: vec![],
            },
        );

        assert_eq!(feishu_account_id(&config).as_deref(), Some("only"));
    }

    #[test]
    fn test_wecom_account_id_matches_corp_and_agent() {
        let mut config = Config::default();
        config.channels.wecom.corp_id = "corp-a".to_string();
        config.channels.wecom.agent_id = 42;
        config.channels.wecom.accounts.insert(
            "ops".to_string(),
            WeComAccountConfig {
                enabled: true,
                mode: "webhook".to_string(),
                corp_id: "corp-a".to_string(),
                corp_secret: "secret".to_string(),
                agent_id: 42,
                bot_id: String::new(),
                bot_secret: String::new(),
                callback_token: String::new(),
                encoding_aes_key: String::new(),
                allow_from: vec![],
                poll_interval_secs: 30,
                ws_url: String::new(),
                ping_interval_secs: 30,
            },
        );

        assert_eq!(wecom_account_id(&config).as_deref(), Some("ops"));
    }

    #[test]
    fn test_whatsapp_account_id_matches_bridge_url() {
        let mut config = Config::default();
        config.channels.whatsapp.bridge_url = "ws://bridge-main".to_string();
        config.channels.whatsapp.accounts.insert(
            "main".to_string(),
            WhatsAppAccountConfig {
                enabled: true,
                bridge_url: "ws://bridge-main".to_string(),
                allow_from: vec![],
            },
        );

        assert_eq!(whatsapp_account_id(&config).as_deref(), Some("main"));
    }

    #[test]
    fn test_dingtalk_account_id_ignores_disabled_match() {
        let mut config = Config::default();
        config.channels.dingtalk.app_key = "app-key".to_string();
        config.channels.dingtalk.accounts.insert(
            "disabled".to_string(),
            DingTalkAccountConfig {
                enabled: false,
                app_key: "app-key".to_string(),
                app_secret: "secret".to_string(),
                robot_code: String::new(),
                allow_from: vec![],
            },
        );

        assert_eq!(dingtalk_account_id(&config), None);
    }

    #[test]
    fn test_discord_account_id_matches_bot_token() {
        let mut config = Config::default();
        config.channels.discord.bot_token = "discord-main".to_string();
        config.channels.discord.accounts.insert(
            "main".to_string(),
            DiscordAccountConfig {
                enabled: true,
                bot_token: "discord-main".to_string(),
                channels: vec![],
                allow_from: vec![],
            },
        );

        assert_eq!(discord_account_id(&config).as_deref(), Some("main"));
    }

    #[test]
    fn test_lark_account_id_matches_app_id() {
        let mut config = Config::default();
        config.channels.lark.app_id = "cli_123".to_string();
        config.channels.lark.accounts.insert(
            "intl".to_string(),
            LarkAccountConfig {
                enabled: true,
                app_id: "cli_123".to_string(),
                app_secret: "secret".to_string(),
                encrypt_key: String::new(),
                verification_token: String::new(),
                allow_from: vec![],
            },
        );

        assert_eq!(lark_account_id(&config).as_deref(), Some("intl"));
    }
}
