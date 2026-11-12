use super::*;

/// Send a text message to a WeCom user or group.
/// `chat_id` can be a user_id (touser) or a group chat_id (chatid).
pub async fn send_message(config: &Config, chat_id: &str, text: &str) -> Result<()> {
    // Long connection mode: route reply via the active WebSocket instead of REST API.
    let mode = config.channels.wecom.mode.trim().to_lowercase();
    if mode == "long_connection" || mode == "long-connection" || mode == "stream" {
        let bot_id = config.channels.wecom.bot_id.trim().to_string();
        let tx = LONGCONN_REGISTRY
            .lock()
            .unwrap()
            .get(&bot_id)
            .cloned()
            .ok_or_else(|| Error::Channel("WeCom long connection not active".into()))?;
        for chunk in split_message(text, WECOM_MSG_LIMIT) {
            let (result_tx, result_rx) = oneshot::channel();
            tx.try_send(LongConnOutbound::Text {
                chat_id: chat_id.to_string(),
                content: chunk,
                result_tx,
            })
            .map_err(|e| Error::Channel(format!("WeCom longconn: failed to queue text: {}", e)))?;
            result_rx.await.map_err(|_| {
                Error::Channel("WeCom longconn: text delivery channel dropped".into())
            })??;
        }
        return Ok(());
    }

    crate::rate_limit::wecom_limiter().acquire().await;

    let client = shared_client();
    let chunks = split_message(text, WECOM_MSG_LIMIT);
    for (i, chunk) in chunks.iter().enumerate() {
        do_send_message(&client, config, chat_id, chunk).await?;
        if i + 1 < chunks.len() {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
    Ok(())
}

pub(crate) async fn fetch_access_token_static(client: &Client, config: &Config) -> Result<String> {
    let mut cache = WECOM_TOKEN_CACHE.lock().await;
    if cache.is_valid() {
        return Ok(cache.token.clone());
    }

    let corp_id = &config.channels.wecom.corp_id;
    let corp_secret = &config.channels.wecom.corp_secret;

    let resp = client
        .get(format!("{}/gettoken", WECOM_API_BASE))
        .query(&[
            ("corpid", corp_id.as_str()),
            ("corpsecret", corp_secret.as_str()),
        ])
        .send()
        .await
        .map_err(|e| Error::Channel(format!("WeCom gettoken failed: {}", e)))?;

    let body: TokenResponse = resp
        .json()
        .await
        .map_err(|e| Error::Channel(format!("Failed to parse WeCom token: {}", e)))?;

    if body.errcode != 0 {
        return Err(Error::Channel(format!(
            "WeCom token error {}: {}",
            body.errcode, body.errmsg
        )));
    }

    let token = body
        .access_token
        .ok_or_else(|| Error::Channel("No access_token in WeCom response".to_string()))?;
    let expires_in = body.expires_in.unwrap_or(7200);
    cache.token = token.clone();
    cache.expires_at = chrono::Utc::now().timestamp() + expires_in;
    info!("WeCom access_token refreshed (expires in {}s)", expires_in);
    Ok(token)
}

pub(crate) async fn clear_access_token_cache() {
    let mut cache = WECOM_TOKEN_CACHE.lock().await;
    cache.token.clear();
    cache.expires_at = 0;
}

pub(crate) async fn do_send_message(
    client: &Client,
    config: &Config,
    chat_id: &str,
    text: &str,
) -> Result<()> {
    let agent_id = config.channels.wecom.agent_id;

    // Determine if chat_id is a group chat (starts with "wr" for WeCom group) or user
    // WeCom group chats use chatid, individual users use touser
    let body = if chat_id.starts_with("wr") || chat_id.starts_with("WR") {
        // Group chat (appchat)
        serde_json::json!({
            "chatid": chat_id,
            "msgtype": "text",
            "text": {
                "content": text
            },
            "safe": 0
        })
    } else {
        // Individual user or @all
        serde_json::json!({
            "touser": chat_id,
            "msgtype": "text",
            "agentid": agent_id,
            "text": {
                "content": text
            },
            "safe": 0
        })
    };

    let endpoint = if chat_id.starts_with("wr") || chat_id.starts_with("WR") {
        format!("{}/appchat/send", WECOM_API_BASE)
    } else {
        format!("{}/message/send", WECOM_API_BASE)
    };

    let mut retried = false;
    let result: WeComResponse = loop {
        let token = fetch_access_token_static(client, config).await?;
        let resp = client
            .post(&endpoint)
            .query(&[("access_token", token.as_str())])
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Failed to send WeCom message: {}", e)))?;

        let result: WeComResponse = resp
            .json()
            .await
            .map_err(|e| Error::Channel(format!("Failed to parse WeCom send response: {}", e)))?;
        if result.is_invalid_token() && !retried {
            warn!(errcode = result.errcode, errmsg = %result.errmsg, "WeCom send token invalid, refreshing and retrying once");
            clear_access_token_cache().await;
            retried = true;
            continue;
        }
        break result;
    };

    if result.errcode != 0 {
        return Err(Error::Channel(format!(
            "WeCom send error {}: {}",
            result.errcode, result.errmsg
        )));
    }

    Ok(())
}

pub(crate) fn split_message(text: &str, max_len: usize) -> Vec<String> {
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
