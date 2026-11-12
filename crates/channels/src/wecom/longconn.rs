use super::*;

impl WeComChannel {
    pub(crate) async fn run_long_connection(
        self: Arc<Self>,
        mut shutdown: tokio::sync::broadcast::Receiver<()>,
    ) {
        info!(
            ws_url = %self.ws_url(),
            ping_interval_secs = self.config.channels.wecom.ping_interval_secs.max(10),
            "WeCom channel started (long_connection mode)"
        );

        loop {
            tokio::select! {
                result = self.connect_and_run_long_connection() => {
                    match result {
                        Ok(_) => info!("WeCom long connection closed normally"),
                        Err(e) => {
                            error!(error = %e, "WeCom long connection error, reconnecting in 5s");
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                                _ = shutdown.recv() => {
                                    info!("WeCom channel shutting down (long_connection)");
                                    break;
                                }
                            }
                        }
                    }
                }
                _ = shutdown.recv() => {
                    info!("WeCom channel shutting down (long_connection)");
                    break;
                }
            }
        }
    }

    pub(crate) fn ws_url(&self) -> &str {
        let ws_url = self.config.channels.wecom.ws_url.trim();
        if ws_url.is_empty() {
            WECOM_LONG_WS_URL
        } else {
            ws_url
        }
    }

    pub(crate) async fn connect_and_run_long_connection(&self) -> Result<()> {
        let url = url::Url::parse(self.ws_url())
            .map_err(|e| Error::Channel(format!("Invalid WeCom ws_url: {}", e)))?;
        info!(ws_url = %url, "Connecting to WeCom long connection WebSocket");
        let (ws_stream, _) = connect_async(url)
            .await
            .map_err(|e| Error::Channel(format!("WeCom WebSocket connection failed: {}", e)))?;

        info!("Connected to WeCom long connection WebSocket");

        // Register outbound sender so send_message() can route replies via WebSocket.
        let bot_id = self.config.channels.wecom.bot_id.clone();
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<LongConnOutbound>(64);
        {
            let mut reg = LONGCONN_REGISTRY.lock().unwrap();
            reg.insert(bot_id.clone(), outbound_tx);
        }

        let (mut write, mut read) = ws_stream.split();
        self.send_long_connection_subscribe(&mut write).await?;

        let mut ping = tokio::time::interval(Duration::from_secs(
            self.config.channels.wecom.ping_interval_secs.max(10) as u64,
        ));
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // Media uploads need exclusive access to both write+read, which conflicts
        // with the tokio::select! borrow rules. We use a LoopAction enum to break out
        // of the select when a media upload is needed, handle it with full access to
        // both halves, then re-enter the select loop.
        enum LoopAction {
            Continue,
            Break(Result<()>),
            MediaUpload {
                chat_id: String,
                file_path: String,
                media_type: String,
                title: String,
                result_tx: oneshot::Sender<Result<String>>,
            },
        }

        let result = loop {
            let action: LoopAction = tokio::select! {
                _ = ping.tick() => {
                    if let Err(e) = write.send(WsMessage::Text("{\"cmd\":\"ping\"}".to_string())).await {
                        LoopAction::Break(Err(Error::Channel(format!("WeCom ping failed: {}", e))))
                    } else {
                        LoopAction::Continue
                    }
                }
                outbound = outbound_rx.recv() => {
                    match outbound {
                        Some(LongConnOutbound::Text { chat_id, content, result_tx }) => {
                            let req_id = {
                                let reg = CHAT_REQID_REGISTRY.lock().unwrap();
                                reg.get(&chat_id)
                                    .cloned()
                                    .unwrap_or_else(|| format!("blockcell-out-{}", chrono::Utc::now().timestamp_millis()))
                            };
                            let stream_id = format!("blockcell-s-{}", chrono::Utc::now().timestamp_millis());
                            info!(chat_id = %chat_id, req_id = %req_id, content_len = content.len(), "WeCom longconn: sending text reply");
                            let msg = serde_json::json!({
                                "cmd": "aibot_respond_msg",
                                "headers": { "req_id": req_id.clone() },
                                "body": {
                                    "msgtype": "stream",
                                    "stream": { "id": stream_id, "finish": true, "content": content }
                                }
                            });
                            let msg_str = msg.to_string();
                            debug!(req_id = %req_id, payload_len = msg_str.len(), "WeCom longconn: outbound WS payload ready");
                            let result = write.send(WsMessage::Text(msg_str)).await
                                .map_err(|e| Error::Channel(format!("WeCom longconn: failed to send text: {}", e)));
                            let _ = result_tx.send(result);
                            LoopAction::Continue
                        }
                        Some(LongConnOutbound::Media { chat_id, file_path, media_type, title, result_tx }) => {
                            LoopAction::MediaUpload { chat_id, file_path, media_type, title, result_tx }
                        }
                        None => LoopAction::Continue,
                    }
                }
                msg = read.next() => {
                    match msg {
                        Some(Ok(WsMessage::Text(text))) => {
                            match self.handle_long_connection_message(&text, &mut write).await {
                                Ok(()) => LoopAction::Continue,
                                Err(e) => LoopAction::Break(Err(e)),
                            }
                        }
                        Some(Ok(WsMessage::Binary(data))) => {
                            let text = String::from_utf8_lossy(&data).to_string();
                            match self.handle_long_connection_message(&text, &mut write).await {
                                Ok(()) => LoopAction::Continue,
                                Err(e) => LoopAction::Break(Err(e)),
                            }
                        }
                        Some(Ok(WsMessage::Ping(data))) => {
                            match write.send(WsMessage::Pong(data)).await {
                                Ok(()) => LoopAction::Continue,
                                Err(e) => LoopAction::Break(Err(Error::Channel(format!("WeCom pong failed: {}", e)))),
                            }
                        }
                        Some(Ok(WsMessage::Pong(_))) => LoopAction::Continue,
                        Some(Ok(WsMessage::Close(frame))) => {
                            info!(?frame, "WeCom long connection closed by server");
                            LoopAction::Break(Ok(()))
                        }
                        Some(Err(e)) => {
                            LoopAction::Break(Err(Error::Channel(format!("WeCom WebSocket read failed: {}", e))))
                        }
                        None => LoopAction::Break(Ok(())),
                        _ => LoopAction::Continue,
                    }
                }
            };

            match action {
                LoopAction::Continue => continue,
                LoopAction::Break(r) => break r,
                LoopAction::MediaUpload {
                    chat_id,
                    file_path,
                    media_type,
                    title,
                    result_tx,
                } => {
                    let upload_result = longconn_upload_and_send_media(
                        &mut write,
                        &mut read,
                        &chat_id,
                        &file_path,
                        &media_type,
                        &title,
                    )
                    .await;
                    let _ = result_tx.send(upload_result);
                }
            }
        };

        // Deregister so send_message stops trying to route to a dead connection.
        {
            let mut reg = LONGCONN_REGISTRY.lock().unwrap();
            reg.remove(&bot_id);
        }
        result
    }

    pub(crate) async fn send_long_connection_subscribe<S>(&self, write: &mut S) -> Result<()>
    where
        S: futures::Sink<WsMessage> + Unpin,
        S::Error: std::fmt::Display,
    {
        let bot_id = self.config.channels.wecom.bot_id.trim();
        let bot_secret = self.config.channels.wecom.bot_secret.trim();
        if bot_id.is_empty() || bot_secret.is_empty() {
            return Err(Error::Channel(
                "WeCom long_connection requires bot_id and bot_secret".to_string(),
            ));
        }

        let req_id = format!("blockcell-{}", chrono::Utc::now().timestamp_millis());
        let req = LongConnCommand {
            cmd: "aibot_subscribe",
            headers: serde_json::json!({ "req_id": req_id }),
            body: serde_json::json!({
                "bot_id": bot_id,
                "secret": bot_secret
            }),
        };
        write
            .send(WsMessage::Text(serde_json::to_string(&req).map_err(
                |e| Error::Channel(format!("WeCom subscribe serialize failed: {}", e)),
            )?))
            .await
            .map_err(|e| Error::Channel(format!("WeCom subscribe send failed: {}", e)))?;
        Ok(())
    }

    pub(crate) async fn handle_long_connection_message<S>(
        &self,
        text: &str,
        write: &mut S,
    ) -> Result<()>
    where
        S: futures::Sink<WsMessage> + Unpin,
        S::Error: std::fmt::Display,
    {
        let envelope: LongConnEnvelope = serde_json::from_str(text)
            .map_err(|e| Error::Channel(format!("Failed to parse WeCom long message: {}", e)))?;

        match envelope.cmd.as_str() {
            "aibot_subscribe" => {
                if envelope.errcode.unwrap_or(0) != 0 {
                    return Err(Error::Channel(format!(
                        "WeCom subscribe error {}: {}",
                        envelope.errcode.unwrap_or(-1),
                        envelope.errmsg.unwrap_or_else(|| "unknown".to_string())
                    )));
                }
                info!("WeCom long connection subscribed successfully");
            }
            "aibot_msg_callback" => {
                let headers: LongConnHeaders = serde_json::from_value(envelope.headers.clone())
                    .unwrap_or(LongConnHeaders { req_id: None });
                // Store effective_chat_id -> req_id using the same logic as
                // build_inbound_from_long_connection so the registry key always matches.
                {
                    let chatid = envelope
                        .body
                        .get("chatid")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let from_user = envelope
                        .body
                        .get("from")
                        .and_then(|v| v.get("userid"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let effective_chat_id = if chatid.is_empty() { from_user } else { chatid };
                    if !effective_chat_id.is_empty() {
                        if let Some(req_id) = headers.req_id.as_deref() {
                            let mut reg = CHAT_REQID_REGISTRY.lock().unwrap();
                            reg.insert(effective_chat_id.to_string(), req_id.to_string());
                        }
                    }
                }
                if let Some(inbound) = self
                    .build_inbound_from_long_connection(&envelope.body)
                    .await?
                {
                    self.inbound_tx
                        .send(inbound)
                        .await
                        .map_err(|e| Error::Channel(e.to_string()))?;
                }
                if let Some(req_id) = headers.req_id {
                    let ack = serde_json::json!({
                        "headers": { "req_id": req_id },
                        "errcode": 0,
                        "errmsg": "ok"
                    });
                    write
                        .send(WsMessage::Text(ack.to_string()))
                        .await
                        .map_err(|e| {
                            Error::Channel(format!("WeCom long connection ack failed: {}", e))
                        })?;
                }
            }
            "aibot_event_callback" => {
                debug!(
                    payload_len = text.len(),
                    "WeCom long connection event callback received"
                );
                let headers: LongConnHeaders = serde_json::from_value(envelope.headers.clone())
                    .unwrap_or(LongConnHeaders { req_id: None });
                if let Some(req_id) = headers.req_id {
                    let ack = serde_json::json!({
                        "headers": { "req_id": req_id },
                        "errcode": 0,
                        "errmsg": "ok"
                    });
                    write
                        .send(WsMessage::Text(ack.to_string()))
                        .await
                        .map_err(|e| {
                            Error::Channel(format!("WeCom long connection event ack failed: {}", e))
                        })?;
                }
            }
            "ping" => {
                write
                    .send(WsMessage::Text("{\"cmd\":\"pong\"}".to_string()))
                    .await
                    .map_err(|e| Error::Channel(format!("WeCom pong send failed: {}", e)))?;
            }
            "pong" => {}
            other => {
                // WeCom sends empty-cmd acks (heartbeat responses, subscribe acks, etc.).
                // Only warn if the payload actually contains an error code.
                let errcode = envelope.errcode.unwrap_or(0);
                if errcode != 0 {
                    warn!(cmd = %other, errcode, payload_len = text.len(), "WeCom long connection: received error response");
                } else {
                    debug!(cmd = %other, "WeCom long connection: received ack");
                }
            }
        }

        Ok(())
    }

    pub(crate) async fn build_inbound_from_long_connection(
        &self,
        body: &serde_json::Value,
    ) -> Result<Option<InboundMessage>> {
        let msg: LongConnMsgBody = serde_json::from_value(body.clone())
            .map_err(|e| Error::Channel(format!("Failed to parse WeCom long body: {}", e)))?;

        let from_ref = msg.from.as_ref();
        let from_user = from_ref.map(|v| v.userid.clone()).unwrap_or_default();
        if from_user.is_empty() || from_user.starts_with('@') {
            return Ok(None);
        }
        if !self.is_allowed(&from_user) {
            debug!(from_user = %from_user, "WeCom long connection: user not in allowlist");
            return Ok(None);
        }

        if !msg.msgid.is_empty() {
            let mut seen = SEEN_MSG_IDS.lock().unwrap();
            if seen.contains(&msg.msgid) {
                debug!(msg_id = %msg.msgid, "WeCom long connection: duplicate msg_id, skipping");
                return Ok(None);
            }
            if seen.len() >= SEEN_MSG_IDS_MAX {
                seen.clear();
            }
            seen.insert(msg.msgid.clone());
        }

        let (content, media, pending) = match msg.msgtype.as_str() {
            "text" => {
                let content = msg
                    .text
                    .as_ref()
                    .map(|t| t.content.trim().to_string())
                    .unwrap_or_default();
                if content.is_empty() {
                    return Ok(None);
                }
                (content, vec![], false)
            }
            "image" => {
                let image = msg.image.unwrap_or_default();
                let mut media = vec![];
                if !image.url.is_empty() && !image.aeskey.is_empty() {
                    match download_and_decrypt_longconn_media(
                        &self.client,
                        &image.url,
                        &image.aeskey,
                        "image",
                        None,
                        &self.media_dir,
                    )
                    .await
                    {
                        Ok(path) => media.push(path),
                        Err(e) => {
                            warn!(error = %e, "WeCom long connection: failed to download image")
                        }
                    }
                }
                (
                    "用户发来了一张图片，请问您需要我做什么？（例如：描述图片内容、识别文字、发回给您等）".to_string(),
                    media,
                    true,
                )
            }
            "mixed" => {
                let mixed = msg.mixed.unwrap_or_default();
                let summary = build_mixed_summary(&mixed);
                if summary.is_empty() {
                    return Ok(None);
                }
                (summary, vec![], false)
            }
            "voice" => {
                let voice = msg.voice.unwrap_or_default();
                let mut media = vec![];
                if !voice.url.is_empty() && !voice.aeskey.is_empty() {
                    match download_and_decrypt_longconn_media(
                        &self.client,
                        &voice.url,
                        &voice.aeskey,
                        "voice",
                        Some("amr"),
                        &self.media_dir,
                    )
                    .await
                    {
                        Ok(path) => media.push(path),
                        Err(e) => {
                            warn!(error = %e, "WeCom long connection: failed to download voice")
                        }
                    }
                }
                let content = if let Some(recognition) =
                    voice.recognition.filter(|s| !s.trim().is_empty())
                {
                    format!("用户发来一条语音，企业微信转写文本：{}", recognition.trim())
                } else {
                    "用户发来了一条语音消息，请先用 audio_transcribe 工具转写，然后根据转写内容回复用户。".to_string()
                };
                (content, media, false)
            }
            "file" => {
                let file = msg.file.unwrap_or_default();
                let mut media = vec![];
                if !file.url.is_empty() && !file.aeskey.is_empty() {
                    match download_and_decrypt_longconn_media(
                        &self.client,
                        &file.url,
                        &file.aeskey,
                        "file",
                        file.filename.as_deref().and_then(|n| n.rsplit('.').next()),
                        &self.media_dir,
                    )
                    .await
                    {
                        Ok(path) => media.push(path),
                        Err(e) => {
                            warn!(error = %e, "WeCom long connection: failed to download file")
                        }
                    }
                }
                let desc = match file.filename.as_deref() {
                    Some(name) if !name.is_empty() => format!(
                        "用户发来了文件「{}」，请问您需要我做什么？（例如：读取内容、分析数据等）",
                        name
                    ),
                    _ => "用户发来了一个文件，请问您需要我做什么？（例如：读取内容、分析数据等）"
                        .to_string(),
                };
                (desc, media, true)
            }
            other => {
                debug!(msg_type = %other, "WeCom long connection: unsupported message type");
                return Ok(None);
            }
        };

        Ok(Some(InboundMessage {
            channel: "wecom".to_string(),
            account_id: wecom_account_id(&self.config),
            sender_id: from_user.clone(),
            chat_id: if msg.chatid.is_empty() {
                from_user.clone()
            } else {
                msg.chatid.clone()
            },
            content,
            media,
            metadata: serde_json::json!({
                "msg_id": msg.msgid,
                "msg_type": msg.msgtype,
                "mode": "long_connection",
                "chat_type": msg.chattype,
                "aibot_id": msg.aibotid,
                "media_pending_intent": pending,
                "sender_nick": from_ref.and_then(|f| f.nickname.clone()),
            }),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        }))
    }
}
