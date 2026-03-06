use super::*;

const SUPPORTED_OWNER_CHANNELS: [&str; 8] = [
    "telegram", "whatsapp", "feishu", "slack", "discord", "dingtalk", "wecom", "lark",
];
// ---------------------------------------------------------------------------
// Channels status endpoint
// ---------------------------------------------------------------------------

/// GET /v1/channels/status — connection status for all configured channels
pub(super) async fn handle_channels_status(State(state): State<GatewayState>) -> impl IntoResponse {
    let statuses = state.channel_manager.get_status();
    let channels: Vec<serde_json::Value> = statuses
        .into_iter()
        .map(|(name, active, detail)| {
            serde_json::json!({
                "name": name,
                "active": active,
                "detail": detail,
            })
        })
        .collect();
    Json(serde_json::json!({ "channels": channels }))
}

// ---------------------------------------------------------------------------
// Channels list — all 8 supported channels with config status
// ---------------------------------------------------------------------------

/// GET /v1/channels — list all 8 supported channels with their configuration status
pub(super) async fn handle_channels_list(State(state): State<GatewayState>) -> impl IntoResponse {
    // Read from disk each time so updates via PUT take effect immediately
    // without requiring a gateway restart.
    let config_path = state.paths.config_file();
    let loaded_config = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Config>(&s).ok())
        .unwrap_or_else(|| state.config.clone());
    let cfg = &loaded_config.channels;
    let owners = &loaded_config.channel_owners;

    let channels = serde_json::json!([
        {
            "id": "telegram",
            "name": "Telegram",
            "icon": "telegram",
            "doc": "docs/channels/zh/01_telegram.md",
            "configured": cfg.telegram.enabled && blockcell_channels::account::channel_configured(&loaded_config, "telegram"),
            "enabled": cfg.telegram.enabled,
            "ownerAgent": owners.get("telegram").cloned().unwrap_or_default(),
            "accountOwners": loaded_config.channel_account_owners.get("telegram").cloned().unwrap_or_default(),
            "defaultAccountId": cfg.telegram.default_account_id.clone().unwrap_or_default(),
            "accounts": cfg.telegram.accounts.keys().cloned().collect::<Vec<_>>(),
            "listeners": blockcell_channels::account::listener_labels(&loaded_config, "telegram"),
            "listenerCount": blockcell_channels::account::listener_labels(&loaded_config, "telegram").len(),
            "fields": [
                {"key": "token", "label": "Bot Token", "secret": true, "value": cfg.telegram.token.clone()},
                {"key": "proxy", "label": "Proxy (可选, 如 socks5://127.0.0.1:7890)", "secret": false, "value": cfg.telegram.proxy.clone().unwrap_or_default()}
            ]
        },
        {
            "id": "discord",
            "name": "Discord",
            "icon": "discord",
            "doc": "docs/channels/zh/02_discord.md",
            "configured": cfg.discord.enabled && blockcell_channels::account::channel_configured(&loaded_config, "discord"),
            "enabled": cfg.discord.enabled,
            "ownerAgent": owners.get("discord").cloned().unwrap_or_default(),
            "accountOwners": loaded_config.channel_account_owners.get("discord").cloned().unwrap_or_default(),
            "defaultAccountId": cfg.discord.default_account_id.clone().unwrap_or_default(),
            "accounts": cfg.discord.accounts.keys().cloned().collect::<Vec<_>>(),
            "listeners": blockcell_channels::account::listener_labels(&loaded_config, "discord"),
            "listenerCount": blockcell_channels::account::listener_labels(&loaded_config, "discord").len(),
            "fields": [
                {"key": "botToken", "label": "Bot Token", "secret": true, "value": cfg.discord.bot_token.clone()},
                {"key": "channels", "label": "Channel IDs (逗号分隔)", "secret": false, "value": cfg.discord.channels.join(",")}
            ]
        },
        {
            "id": "slack",
            "name": "Slack",
            "icon": "slack",
            "doc": "docs/channels/zh/03_slack.md",
            "configured": cfg.slack.enabled && blockcell_channels::account::channel_configured(&loaded_config, "slack"),
            "enabled": cfg.slack.enabled,
            "ownerAgent": owners.get("slack").cloned().unwrap_or_default(),
            "accountOwners": loaded_config.channel_account_owners.get("slack").cloned().unwrap_or_default(),
            "defaultAccountId": cfg.slack.default_account_id.clone().unwrap_or_default(),
            "accounts": cfg.slack.accounts.keys().cloned().collect::<Vec<_>>(),
            "listeners": blockcell_channels::account::listener_labels(&loaded_config, "slack"),
            "listenerCount": blockcell_channels::account::listener_labels(&loaded_config, "slack").len(),
            "fields": [
                {"key": "botToken", "label": "Bot Token", "secret": true, "value": cfg.slack.bot_token.clone()},
                {"key": "appToken", "label": "App Token", "secret": true, "value": cfg.slack.app_token.clone()},
                {"key": "channels", "label": "Channel IDs (逗号分隔)", "secret": false, "value": cfg.slack.channels.join(",")},
                {"key": "pollIntervalSecs", "label": "轮询间隔 (秒)", "secret": false, "value": cfg.slack.poll_interval_secs.to_string()}
            ]
        },
        {
            "id": "feishu",
            "name": "飞书",
            "icon": "feishu",
            "doc": "docs/channels/zh/04_feishu.md",
            "configured": cfg.feishu.enabled && blockcell_channels::account::channel_configured(&loaded_config, "feishu"),
            "enabled": cfg.feishu.enabled,
            "ownerAgent": owners.get("feishu").cloned().unwrap_or_default(),
            "accountOwners": loaded_config.channel_account_owners.get("feishu").cloned().unwrap_or_default(),
            "defaultAccountId": cfg.feishu.default_account_id.clone().unwrap_or_default(),
            "accounts": cfg.feishu.accounts.keys().cloned().collect::<Vec<_>>(),
            "listeners": blockcell_channels::account::listener_labels(&loaded_config, "feishu"),
            "listenerCount": blockcell_channels::account::listener_labels(&loaded_config, "feishu").len(),
            "fields": [
                {"key": "appId", "label": "App ID", "secret": false, "value": cfg.feishu.app_id.clone()},
                {"key": "appSecret", "label": "App Secret", "secret": true, "value": cfg.feishu.app_secret.clone()},
                {"key": "encryptKey", "label": "Encrypt Key (事件加密密钥)", "secret": true, "value": cfg.feishu.encrypt_key.clone()},
                {"key": "verificationToken", "label": "Verification Token (事件验证Token)", "secret": true, "value": cfg.feishu.verification_token.clone()}
            ]
        },
        {
            "id": "dingtalk",
            "name": "钉钉",
            "icon": "dingtalk",
            "doc": "docs/channels/zh/05_dingtalk.md",
            "configured": cfg.dingtalk.enabled && blockcell_channels::account::channel_configured(&loaded_config, "dingtalk"),
            "enabled": cfg.dingtalk.enabled,
            "ownerAgent": owners.get("dingtalk").cloned().unwrap_or_default(),
            "accountOwners": loaded_config.channel_account_owners.get("dingtalk").cloned().unwrap_or_default(),
            "defaultAccountId": cfg.dingtalk.default_account_id.clone().unwrap_or_default(),
            "accounts": cfg.dingtalk.accounts.keys().cloned().collect::<Vec<_>>(),
            "listeners": blockcell_channels::account::listener_labels(&loaded_config, "dingtalk"),
            "listenerCount": blockcell_channels::account::listener_labels(&loaded_config, "dingtalk").len(),
            "fields": [
                {"key": "appKey", "label": "App Key", "secret": false, "value": cfg.dingtalk.app_key.clone()},
                {"key": "appSecret", "label": "App Secret", "secret": true, "value": cfg.dingtalk.app_secret.clone()},
                {"key": "robotCode", "label": "Robot Code (机器人编码, 用于主动发消息)", "secret": false, "value": cfg.dingtalk.robot_code.clone()}
            ]
        },
        {
            "id": "wecom",
            "name": "企业微信",
            "icon": "wecom",
            "doc": "docs/channels/zh/06_wecom.md",
            "configured": cfg.wecom.enabled && blockcell_channels::account::channel_configured(&loaded_config, "wecom"),
            "enabled": cfg.wecom.enabled,
            "ownerAgent": owners.get("wecom").cloned().unwrap_or_default(),
            "accountOwners": loaded_config.channel_account_owners.get("wecom").cloned().unwrap_or_default(),
            "defaultAccountId": cfg.wecom.default_account_id.clone().unwrap_or_default(),
            "accounts": cfg.wecom.accounts.keys().cloned().collect::<Vec<_>>(),
            "listeners": blockcell_channels::account::listener_labels(&loaded_config, "wecom"),
            "listenerCount": blockcell_channels::account::listener_labels(&loaded_config, "wecom").len(),
            "fields": [
                {"key": "corpId", "label": "Corp ID", "secret": false, "value": cfg.wecom.corp_id.clone()},
                {"key": "corpSecret", "label": "Corp Secret", "secret": true, "value": cfg.wecom.corp_secret.clone()},
                {"key": "agentId", "label": "Agent ID", "secret": false, "value": cfg.wecom.agent_id.to_string()},
                {"key": "callbackToken", "label": "Callback Token (回调Token, 可选)", "secret": true, "value": cfg.wecom.callback_token.clone()},
                {"key": "encodingAesKey", "label": "EncodingAESKey (消息加解密密钥, 可选)", "secret": true, "value": cfg.wecom.encoding_aes_key.clone()},
                {"key": "pollIntervalSecs", "label": "轮询间隔 (秒)", "secret": false, "value": cfg.wecom.poll_interval_secs.to_string()}
            ]
        },
        {
            "id": "whatsapp",
            "name": "WhatsApp",
            "icon": "whatsapp",
            "doc": "docs/channels/zh/07_whatsapp.md",
            "configured": cfg.whatsapp.enabled && blockcell_channels::account::channel_configured(&loaded_config, "whatsapp"),
            "enabled": cfg.whatsapp.enabled,
            "ownerAgent": owners.get("whatsapp").cloned().unwrap_or_default(),
            "accountOwners": loaded_config.channel_account_owners.get("whatsapp").cloned().unwrap_or_default(),
            "defaultAccountId": cfg.whatsapp.default_account_id.clone().unwrap_or_default(),
            "accounts": cfg.whatsapp.accounts.keys().cloned().collect::<Vec<_>>(),
            "listeners": blockcell_channels::account::listener_labels(&loaded_config, "whatsapp"),
            "listenerCount": blockcell_channels::account::listener_labels(&loaded_config, "whatsapp").len(),
            "fields": [
                {"key": "bridgeUrl", "label": "Bridge URL", "secret": false, "value": cfg.whatsapp.bridge_url.clone()}
            ]
        },
        {
            "id": "lark",
            "name": "Lark (飞书国际版)",
            "icon": "lark",
            "doc": "docs/channels/zh/08_lark.md",
            "configured": cfg.lark.enabled && blockcell_channels::account::channel_configured(&loaded_config, "lark"),
            "enabled": cfg.lark.enabled,
            "ownerAgent": owners.get("lark").cloned().unwrap_or_default(),
            "accountOwners": loaded_config.channel_account_owners.get("lark").cloned().unwrap_or_default(),
            "defaultAccountId": cfg.lark.default_account_id.clone().unwrap_or_default(),
            "accounts": cfg.lark.accounts.keys().cloned().collect::<Vec<_>>(),
            "listeners": blockcell_channels::account::listener_labels(&loaded_config, "lark"),
            "listenerCount": blockcell_channels::account::listener_labels(&loaded_config, "lark").len(),
            "fields": [
                {"key": "appId", "label": "App ID", "secret": false, "value": cfg.lark.app_id.clone()},
                {"key": "appSecret", "label": "App Secret", "secret": true, "value": cfg.lark.app_secret.clone()},
                {"key": "encryptKey", "label": "Encrypt Key (Event encryption key)", "secret": true, "value": cfg.lark.encrypt_key.clone()},
                {"key": "verificationToken", "label": "Verification Token (Event verification)", "secret": true, "value": cfg.lark.verification_token.clone()}
            ]
        }
    ]);
    Json(serde_json::json!({ "channels": channels }))
}

/// PUT /v1/channels/:id — update channel config fields
#[derive(Deserialize)]
pub(super) struct ChannelUpdateRequest {
    fields: serde_json::Map<String, serde_json::Value>,
    enabled: Option<bool>,
}

pub(super) async fn handle_channel_update(
    State(state): State<GatewayState>,
    AxumPath(channel_id): AxumPath<String>,
    Json(req): Json<ChannelUpdateRequest>,
) -> impl IntoResponse {
    let config_path = state.paths.config_file();
    let result: anyhow::Result<serde_json::Value> = (|| async {
        let content = std::fs::read_to_string(&config_path)?;
        let mut root: serde_json::Value = serde_json::from_str(&content)?;

        let channels = root
            .get_mut("channels")
            .and_then(|v| v.as_object_mut())
            .ok_or_else(|| anyhow::anyhow!("no channels section in config"))?;

        let ch_key = channel_id.as_str();
        let ch = channels
            .entry(ch_key)
            .or_insert_with(|| serde_json::json!({}));

        if let Some(obj) = ch.as_object_mut() {
            // Insert fields with type coercion for non-string config fields
            for (k, v) in &req.fields {
                let coerced = match k.as_str() {
                    // Option<String>: empty string → null
                    "proxy" => {
                        let s = v.as_str().unwrap_or("");
                        if s.is_empty() {
                            serde_json::Value::Null
                        } else {
                            v.clone()
                        }
                    }
                    // Vec<String>: comma-separated string → JSON array
                    "channels" => {
                        let s = v.as_str().unwrap_or("");
                        let arr: Vec<&str> = if s.is_empty() {
                            vec![]
                        } else {
                            s.split(',')
                                .map(|x| x.trim())
                                .filter(|x| !x.is_empty())
                                .collect()
                        };
                        serde_json::json!(arr)
                    }
                    // u32/i64 numeric fields: string → number
                    "pollIntervalSecs" | "agentId" => {
                        let s = v.as_str().unwrap_or("0");
                        let n: i64 = s.parse().unwrap_or(0);
                        serde_json::json!(n)
                    }
                    _ => v.clone(),
                };
                obj.insert(k.clone(), coerced);
            }
            if let Some(en) = req.enabled {
                obj.insert("enabled".to_string(), serde_json::json!(en));
            }
            // Clean up stale snake_case keys from previous buggy saves
            let stale: &[&str] = &[
                "bot_token",
                "app_token",
                "app_id",
                "app_secret",
                "app_key",
                "corp_id",
                "corp_secret",
                "agent_id",
                "bridge_url",
                "allow_from",
                "poll_interval_secs",
                "encrypt_key",
                "verification_token",
                "robot_code",
                "callback_token",
                "encoding_aes_key",
            ];
            for key in stale {
                obj.remove(*key);
            }
        }

        std::fs::write(&config_path, serde_json::to_string_pretty(&root)?)?;
        Ok(serde_json::json!({ "status": "ok", "channel": ch_key }))
    })()
    .await;

    match result {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({ "status": "error", "message": e.to_string() })),
    }
}

fn known_account_ids(cfg: &Config, channel: &str) -> Vec<String> {
    let mut ids = match channel {
        "telegram" => cfg.channels.telegram.accounts.keys().cloned().collect::<Vec<_>>(),
        "whatsapp" => cfg.channels.whatsapp.accounts.keys().cloned().collect::<Vec<_>>(),
        "feishu" => cfg.channels.feishu.accounts.keys().cloned().collect::<Vec<_>>(),
        "slack" => cfg.channels.slack.accounts.keys().cloned().collect::<Vec<_>>(),
        "discord" => cfg.channels.discord.accounts.keys().cloned().collect::<Vec<_>>(),
        "dingtalk" => cfg.channels.dingtalk.accounts.keys().cloned().collect::<Vec<_>>(),
        "wecom" => cfg.channels.wecom.accounts.keys().cloned().collect::<Vec<_>>(),
        "lark" => cfg.channels.lark.accounts.keys().cloned().collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    ids.sort();
    ids
}

fn channel_owner_bindings_payload(cfg: &Config) -> serde_json::Value {
    serde_json::json!({
        "channelOwners": &cfg.channel_owners,
        "channelAccountOwners": &cfg.channel_account_owners,
    })
}

fn set_owner_binding(
    cfg: &mut Config,
    channel: &str,
    account_id: Option<&str>,
    agent: &str,
) -> anyhow::Result<serde_json::Value> {
    if !SUPPORTED_OWNER_CHANNELS.contains(&channel) {
        anyhow::bail!("Unsupported channel '{}'", channel);
    }
    if !cfg.agent_exists(agent) {
        anyhow::bail!(
            "Agent '{}' does not exist in agents.list (or default fallback)",
            agent
        );
    }

    if let Some(account_id) = account_id.map(str::trim).filter(|value| !value.is_empty()) {
        let known_accounts = known_account_ids(cfg, channel);
        if !known_accounts.iter().any(|id| id == account_id) {
            anyhow::bail!(
                "Account '{}' is not defined under channels.{}.accounts.",
                account_id,
                channel
            );
        }
        cfg.channel_account_owners
            .entry(channel.to_string())
            .or_default()
            .insert(account_id.to_string(), agent.to_string());
        Ok(serde_json::json!({
            "status": "ok",
            "channel": channel,
            "accountId": account_id,
            "agent": agent,
        }))
    } else {
        cfg.channel_owners.insert(channel.to_string(), agent.to_string());
        Ok(serde_json::json!({
            "status": "ok",
            "channel": channel,
            "agent": agent,
        }))
    }
}

fn clear_owner_binding(
    cfg: &mut Config,
    channel: &str,
    account_id: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    if !SUPPORTED_OWNER_CHANNELS.contains(&channel) {
        anyhow::bail!("Unsupported channel '{}'", channel);
    }

    if let Some(account_id) = account_id.map(str::trim).filter(|value| !value.is_empty()) {
        if let Some(bindings) = cfg.channel_account_owners.get_mut(channel) {
            bindings.remove(account_id);
            if bindings.is_empty() {
                cfg.channel_account_owners.remove(channel);
            }
        }
        Ok(serde_json::json!({
            "status": "ok",
            "channel": channel,
            "accountId": account_id,
        }))
    } else {
        cfg.channel_owners.remove(channel);
        Ok(serde_json::json!({ "status": "ok", "channel": channel }))
    }
}

/// GET /v1/channel-owners — list channel -> agent owner bindings
pub(super) async fn handle_channel_owners_get(
    State(state): State<GatewayState>,
) -> impl IntoResponse {
    let config_path = state.paths.config_file();
    let cfg = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Config>(&s).ok())
        .unwrap_or_else(|| state.config.clone());
    Json(channel_owner_bindings_payload(&cfg))
}

#[derive(Deserialize)]
pub(super) struct ChannelOwnerUpdateRequest {
    #[serde(alias = "agentId")]
    agent: String,
}

/// PUT /v1/channel-owners/:channel — set channel-level owner binding
pub(super) async fn handle_channel_owner_put(
    State(state): State<GatewayState>,
    AxumPath(channel): AxumPath<String>,
    Json(req): Json<ChannelOwnerUpdateRequest>,
) -> impl IntoResponse {
    let config_path = state.paths.config_file();
    let result: anyhow::Result<serde_json::Value> = (|| async {
        let mut cfg = std::fs::read_to_string(&config_path)
            .ok()
            .and_then(|s| serde_json::from_str::<Config>(&s).ok())
            .unwrap_or_else(|| state.config.clone());

        let payload = set_owner_binding(&mut cfg, &channel, None, &req.agent)?;
        cfg.save(&config_path)?;
        Ok(payload)
    })()
    .await;

    match result {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({ "status": "error", "message": e.to_string() })),
    }
}

/// PUT /v1/channel-owners/:channel/accounts/:account_id — set account-level owner binding
pub(super) async fn handle_channel_account_owner_put(
    State(state): State<GatewayState>,
    AxumPath((channel, account_id)): AxumPath<(String, String)>,
    Json(req): Json<ChannelOwnerUpdateRequest>,
) -> impl IntoResponse {
    let config_path = state.paths.config_file();
    let result: anyhow::Result<serde_json::Value> = (|| async {
        let mut cfg = std::fs::read_to_string(&config_path)
            .ok()
            .and_then(|s| serde_json::from_str::<Config>(&s).ok())
            .unwrap_or_else(|| state.config.clone());

        let payload = set_owner_binding(&mut cfg, &channel, Some(&account_id), &req.agent)?;
        cfg.save(&config_path)?;
        Ok(payload)
    })()
    .await;

    match result {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({ "status": "error", "message": e.to_string() })),
    }
}

/// DELETE /v1/channel-owners/:channel — clear channel-level owner binding
pub(super) async fn handle_channel_owner_delete(
    State(state): State<GatewayState>,
    AxumPath(channel): AxumPath<String>,
) -> impl IntoResponse {
    let config_path = state.paths.config_file();
    let result: anyhow::Result<serde_json::Value> = (|| async {
        let mut cfg = std::fs::read_to_string(&config_path)
            .ok()
            .and_then(|s| serde_json::from_str::<Config>(&s).ok())
            .unwrap_or_else(|| state.config.clone());
        let payload = clear_owner_binding(&mut cfg, &channel, None)?;
        cfg.save(&config_path)?;
        Ok(payload)
    })()
    .await;

    match result {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({ "status": "error", "message": e.to_string() })),
    }
}

/// DELETE /v1/channel-owners/:channel/accounts/:account_id — clear account-level owner binding
pub(super) async fn handle_channel_account_owner_delete(
    State(state): State<GatewayState>,
    AxumPath((channel, account_id)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    let config_path = state.paths.config_file();
    let result: anyhow::Result<serde_json::Value> = (|| async {
        let mut cfg = std::fs::read_to_string(&config_path)
            .ok()
            .and_then(|s| serde_json::from_str::<Config>(&s).ok())
            .unwrap_or_else(|| state.config.clone());
        let payload = clear_owner_binding(&mut cfg, &channel, Some(&account_id))?;
        cfg.save(&config_path)?;
        Ok(payload)
    })()
    .await;

    match result {
        Ok(v) => Json(v),
        Err(e) => Json(serde_json::json!({ "status": "error", "message": e.to_string() })),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_owner_bindings_payload_includes_account_overrides() {
        let mut cfg = Config::default();
        cfg.channel_owners
            .insert("telegram".to_string(), "default".to_string());
        cfg.channel_account_owners.insert(
            "telegram".to_string(),
            std::collections::HashMap::from([("bot2".to_string(), "ops".to_string())]),
        );

        let payload = channel_owner_bindings_payload(&cfg);
        assert_eq!(payload["channelOwners"]["telegram"], serde_json::json!("default"));
        assert_eq!(
            payload["channelAccountOwners"]["telegram"]["bot2"],
            serde_json::json!("ops")
        );
    }

    #[test]
    fn test_set_owner_binding_updates_account_override() {
        let mut cfg = Config::default();
        cfg.agents.list.push(blockcell_core::config::AgentProfileConfig {
            id: "ops".to_string(),
            enabled: true,
            ..Default::default()
        });
        cfg.channels.telegram.accounts.insert(
            "bot2".to_string(),
            blockcell_core::config::TelegramAccountConfig {
                enabled: true,
                token: "tg-bot2".to_string(),
                allow_from: vec![],
                proxy: None,
            },
        );

        let payload = set_owner_binding(&mut cfg, "telegram", Some("bot2"), "ops").unwrap();
        assert_eq!(cfg.resolve_channel_account_owner("telegram", "bot2"), Some("ops"));
        assert_eq!(payload["accountId"], serde_json::json!("bot2"));
        assert_eq!(payload["agent"], serde_json::json!("ops"));
    }

    #[test]
    fn test_clear_owner_binding_removes_account_override() {
        let mut cfg = Config::default();
        cfg.channel_account_owners.insert(
            "telegram".to_string(),
            std::collections::HashMap::from([("bot2".to_string(), "ops".to_string())]),
        );

        let payload = clear_owner_binding(&mut cfg, "telegram", Some("bot2")).unwrap();
        assert_eq!(cfg.resolve_channel_account_owner("telegram", "bot2"), None);
        assert_eq!(payload["accountId"], serde_json::json!("bot2"));
    }
}
