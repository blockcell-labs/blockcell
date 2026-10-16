use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WecomVerificationDiagnostics {
    timestamp_present: bool,
    nonce_len: usize,
    signature_present: bool,
    encrypted_payload_len: usize,
    token_configured: bool,
    aes_key_configured: bool,
}

impl WecomVerificationDiagnostics {
    fn new(
        callback_token: &str,
        encoding_aes_key: &str,
        timestamp: &str,
        nonce: &str,
        signature: &str,
        encrypted_payload: &str,
    ) -> Self {
        Self {
            timestamp_present: !timestamp.is_empty(),
            nonce_len: nonce.len(),
            signature_present: !signature.is_empty(),
            encrypted_payload_len: encrypted_payload.len(),
            token_configured: !callback_token.is_empty(),
            aes_key_configured: !encoding_aes_key.is_empty(),
        }
    }
}

pub(crate) fn build_mixed_summary(mixed: &LongConnMixed) -> String {
    let parts: Vec<String> = mixed
        .items
        .iter()
        .filter_map(|item| match item.item_type.as_str() {
            "text" => item
                .content
                .as_ref()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            "image" => Some("[图片]".to_string()),
            "link" => Some("[链接]".to_string()),
            "file" => Some("[文件]".to_string()),
            other if !other.is_empty() => Some(format!("[{}]", other)),
            _ => None,
        })
        .collect();
    parts.join(" ")
}

pub(crate) async fn download_and_decrypt_longconn_media(
    client: &Client,
    url: &str,
    aeskey: &str,
    media_type: &str,
    ext_hint: Option<&str>,
    media_dir: &Path,
) -> Result<String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("WeCom long media download failed: {}", e)))?;
    if !resp.status().is_success() {
        return Err(Error::Channel(format!(
            "WeCom long media download HTTP {}",
            resp.status()
        )));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Error::Channel(format!("WeCom long media read failed: {}", e)))?;
    let plain = decrypt_longconn_media_bytes(&bytes, aeskey)?;
    tokio::fs::create_dir_all(media_dir)
        .await
        .map_err(|e| Error::Channel(format!("Failed to create media dir: {}", e)))?;
    let ext = ext_hint
        .map(|s| s.to_string())
        .unwrap_or_else(|| ext_from_content_type(&content_type, media_type).to_string());
    let filename = format!(
        "wecom_long_{}_{}.{}",
        crate::security::safe_path_component(media_type),
        chrono::Utc::now().timestamp_millis(),
        crate::security::safe_path_component(&ext)
    );
    let path = media_dir.join(filename);
    tokio::fs::write(&path, &plain)
        .await
        .map_err(|e| Error::Channel(format!("WeCom long media write failed: {}", e)))?;
    Ok(path.to_string_lossy().to_string())
}

pub(crate) fn decrypt_longconn_media_bytes(ciphertext: &[u8], aeskey: &str) -> Result<Vec<u8>> {
    let key = general_purpose::STANDARD
        .decode(aeskey)
        .or_else(|_| {
            let padded = match aeskey.len() % 4 {
                2 => format!("{}==", aeskey),
                3 => format!("{}=", aeskey),
                _ => aeskey.to_string(),
            };
            general_purpose::STANDARD.decode(padded)
        })
        .map_err(|e| Error::Channel(format!("WeCom long media aeskey decode failed: {}", e)))?;
    if key.len() != 32 {
        return Err(Error::Channel(format!(
            "WeCom long media aeskey invalid length: {}",
            key.len()
        )));
    }
    use aes::cipher::block_padding::Pkcs7;
    let iv = &key[..16];
    let decryptor = Aes256CbcDec::new_from_slices(&key, iv)
        .map_err(|e| Error::Channel(format!("WeCom long media decryptor init failed: {}", e)))?;
    let mut buf = ciphertext.to_vec();
    let plain = decryptor
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| Error::Channel(format!("WeCom long media decrypt failed: {}", e)))?;
    Ok(plain.to_vec())
}

/// Percent-decode a URL query parameter value (%2B → +, %2F → /, %3D → =, etc.).
/// Does NOT treat '+' as space (that's form-encoding, not used by WeCom).
pub(crate) fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push(char::from(h << 4 | l));
                i += 3;
                continue;
            }
        }
        out.push(char::from(bytes[i]));
        i += 1;
    }
    out
}

pub(crate) fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Simple SHA1 implementation for WeCom signature verification.
pub(crate) fn sha1_hex(data: &[u8]) -> String {
    let hash = sha1_digest(data);
    hash.iter().fold(String::new(), |mut acc, b| {
        acc.push_str(&format!("{:02x}", b));
        acc
    })
}

pub(crate) fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];

    let msg_len = data.len();
    let bit_len = (msg_len as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0x00);
    }
    for i in (0..8).rev() {
        msg.push(((bit_len >> (i * 8)) & 0xFF) as u8);
    }

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);

        #[allow(clippy::needless_range_loop)]
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut result = [0u8; 20];
    for (i, &val) in h.iter().enumerate() {
        let bytes = val.to_be_bytes();
        result[i * 4..i * 4 + 4].copy_from_slice(&bytes);
    }
    result
}

// ── send_message ──────────────────────────────────────────────────────────────

/// Handle a WeCom webhook request.
///
/// WeCom sends two types of requests to the callback URL:
pub(crate) fn resolve_wecom_webhook_config(
    config: &Config,
    method: &str,
    query: &std::collections::HashMap<String, String>,
    body: &str,
) -> Config {
    let listeners = wecom_listener_configs(config);
    if listeners.is_empty() {
        return config.clone();
    }
    if listeners.len() == 1 {
        return listeners[0].config.clone();
    }

    let timestamp = query.get("timestamp").map(|s| s.as_str()).unwrap_or("");
    let nonce = query.get("nonce").map(|s| s.as_str()).unwrap_or("");
    let msg_signature = query
        .get("msg_signature")
        .or_else(|| query.get("signature"))
        .map(|s| s.as_str())
        .unwrap_or("");

    let signed_payload = if method == "GET" {
        query
            .get("echostr")
            .map(|s| percent_decode(s))
            .unwrap_or_default()
    } else {
        extract_xml_tag(body, "Encrypt").unwrap_or_default()
    };

    if !msg_signature.is_empty() && !signed_payload.is_empty() {
        for listener in &listeners {
            let token = listener.config.channels.wecom.callback_token.as_str();
            if token.is_empty() {
                continue;
            }
            if verify_signature_4(token, timestamp, nonce, &signed_payload, msg_signature) {
                return listener.config.clone();
            }
        }
    }

    config.clone()
}

/// 根据 webhook 参数解析匹配的 account_id，供 gateway 计算 agent 级别的 media_dir。
pub fn resolve_wecom_webhook_account_id(
    config: &Config,
    method: &str,
    query: &std::collections::HashMap<String, String>,
    body: &str,
) -> Option<String> {
    let listeners = wecom_listener_configs(config);
    if listeners.is_empty() {
        return None;
    }
    if listeners.len() == 1 {
        return listeners[0].account_id.clone();
    }
    let timestamp = query.get("timestamp").map(|s| s.as_str()).unwrap_or("");
    let nonce = query.get("nonce").map(|s| s.as_str()).unwrap_or("");
    let msg_signature = query
        .get("msg_signature")
        .or_else(|| query.get("signature"))
        .map(|s| s.as_str())
        .unwrap_or("");
    let signed_payload = if method == "GET" {
        query
            .get("echostr")
            .map(|s| percent_decode(s))
            .unwrap_or_default()
    } else {
        extract_xml_tag(body, "Encrypt").unwrap_or_default()
    };
    if !msg_signature.is_empty() && !signed_payload.is_empty() {
        for listener in &listeners {
            let token = listener.config.channels.wecom.callback_token.as_str();
            if token.is_empty() {
                continue;
            }
            if verify_signature_4(token, timestamp, nonce, &signed_payload, msg_signature) {
                return listener.account_id.clone();
            }
        }
    }
    None
}

/// - **GET**: URL verification — responds with `echostr` query param if signature is valid
/// - **POST**: Message/event callback — parses XML body and forwards to inbound channel
///
/// Returns `(status_code, body_string)`.
pub async fn process_webhook(
    config: &Config,
    method: &str,
    query: &std::collections::HashMap<String, String>,
    body: &str,
    inbound_tx: Option<&tokio::sync::mpsc::Sender<blockcell_core::InboundMessage>>,
    media_dir: PathBuf,
) -> (u16, String) {
    let resolved_config = resolve_wecom_webhook_config(config, method, query, body);
    let wecom_cfg = &resolved_config.channels.wecom;

    let has_wecom_params = query.contains_key("msg_signature")
        || query.contains_key("signature")
        || query.contains_key("echostr");

    if method == "GET" {
        if !has_wecom_params {
            // Plain connectivity probe (e.g. wget/curl health check) — return 200
            return (200, "ok".to_string());
        }

        // WeCom URL verification:
        // 1. echostr is AES-encrypted Base64
        // 2. Signature = SHA1(sort(token, timestamp, nonce, echostr_encrypted))
        let msg_signature = query
            .get("msg_signature")
            .or_else(|| query.get("signature"))
            .map(|s| s.as_str())
            .unwrap_or("");
        let timestamp = query.get("timestamp").map(|s| s.as_str()).unwrap_or("");
        let nonce = query.get("nonce").map(|s| s.as_str()).unwrap_or("");
        // URL-decode the echostr: WeCom percent-encodes '+' as '%2B' etc. in the query string,
        // but signs and encrypts the plain base64 string. Decode before both sig check and decrypt.
        let echostr_raw = query.get("echostr").map(|s| s.as_str()).unwrap_or("");
        let echostr_enc_owned = percent_decode(echostr_raw);
        let echostr_enc = echostr_enc_owned.as_str();

        let diagnostics = WecomVerificationDiagnostics::new(
            &wecom_cfg.callback_token,
            &wecom_cfg.encoding_aes_key,
            timestamp,
            nonce,
            msg_signature,
            echostr_enc,
        );
        tracing::info!(
            timestamp_present = diagnostics.timestamp_present,
            nonce_len = diagnostics.nonce_len,
            signature_present = diagnostics.signature_present,
            encrypted_payload_len = diagnostics.encrypted_payload_len,
            token_configured = diagnostics.token_configured,
            aes_key_configured = diagnostics.aes_key_configured,
            "WeCom GET verification parameters received"
        );

        if !wecom_cfg.callback_token.is_empty() {
            let mut parts = [
                wecom_cfg.callback_token.as_str(),
                timestamp,
                nonce,
                echostr_enc,
            ];
            parts.sort_unstable();
            let combined = parts.join("");
            let computed = sha1_hex(combined.as_bytes());

            // 4-param signature: sort(token, timestamp, nonce, msg_encrypt)
            if computed != msg_signature {
                tracing::warn!("WeCom webhook: GET signature verification failed");
                return (403, "Forbidden: invalid signature".to_string());
            }
        }

        // Decrypt echostr to get plaintext msg
        match decrypt_wecom_msg(echostr_enc, &wecom_cfg.encoding_aes_key) {
            Ok(plain) => {
                tracing::info!("WeCom webhook: URL verification OK, returning echostr plaintext");
                return (200, plain);
            }
            Err(e) => {
                tracing::error!("WeCom webhook: failed to decrypt echostr: {}", e);
                return (500, "decrypt error".to_string());
            }
        }
    }

    // POST: parse XML body
    if body.is_empty() {
        return (200, "success".to_string());
    }

    // POST messages use <Encrypt> field (AES encrypted)
    // Verify signature: SHA1(sort(token, timestamp, nonce, msg_encrypt))
    let msg_encrypt = extract_xml_tag(body, "Encrypt").unwrap_or_default();
    let timestamp = query.get("timestamp").map(|s| s.as_str()).unwrap_or("");
    let nonce = query.get("nonce").map(|s| s.as_str()).unwrap_or("");
    let msg_signature = query
        .get("msg_signature")
        .or_else(|| query.get("signature"))
        .map(|s| s.as_str())
        .unwrap_or("");

    // Fail closed: once a callback token is configured, every POST must carry an
    // encrypted payload with a valid signature. A missing <Encrypt> tag must not
    // silently downgrade to parsing an unverified plaintext body.
    if !wecom_cfg.callback_token.is_empty() {
        if msg_encrypt.is_empty() {
            tracing::warn!("WeCom webhook: POST missing <Encrypt>, rejecting unverified body");
            return (403, "Forbidden: missing encrypted payload".to_string());
        }
        if !verify_signature_4(
            &wecom_cfg.callback_token,
            timestamp,
            nonce,
            &msg_encrypt,
            msg_signature,
        ) {
            tracing::warn!("WeCom webhook: POST signature verification failed");
            return (403, "Forbidden: invalid signature".to_string());
        }
    } else {
        tracing::warn!(
            "WeCom webhook: callback_token not configured; accepting unverified POST body"
        );
    }

    // Decrypt the message body
    let decrypted_body = if !msg_encrypt.is_empty() && !wecom_cfg.encoding_aes_key.is_empty() {
        match decrypt_wecom_msg(&msg_encrypt, &wecom_cfg.encoding_aes_key) {
            Ok(plain) => plain,
            Err(e) => {
                tracing::error!("WeCom webhook: failed to decrypt POST message: {}", e);
                return (200, "success".to_string());
            }
        }
    } else if !wecom_cfg.callback_token.is_empty() {
        // Token configured but no AES key: refuse to fall back to plaintext parsing.
        tracing::error!(
            "WeCom webhook: encoding_aes_key not configured, cannot decrypt verified payload"
        );
        return (403, "Forbidden: encryption key not configured".to_string());
    } else {
        // No encryption configured — treat body as plain XML
        body.to_string()
    };

    // Extract fields from decrypted XML
    let from_user = extract_xml_tag(&decrypted_body, "FromUserName").unwrap_or_default();
    let msg_type = extract_xml_tag(&decrypted_body, "MsgType").unwrap_or_default();
    let content = extract_xml_tag(&decrypted_body, "Content").unwrap_or_default();
    let _to_user = extract_xml_tag(&decrypted_body, "ToUserName").unwrap_or_default();
    let msg_id = extract_xml_tag(&decrypted_body, "MsgId");

    tracing::debug!(
        from_user = %from_user,
        msg_type = %msg_type,
        content = %content,
        "WeCom webhook: received message"
    );

    // Filter out messages sent by the bot itself — WeCom echoes bot-sent messages back
    // as callbacks. Bot messages have FromUserName starting with '@' (e.g. @app, @all)
    // or are event-type messages with no real user sender.
    if from_user.starts_with('@') {
        tracing::debug!(from_user = %from_user, "WeCom webhook: skipping bot/system message");
        return (200, "success".to_string());
    }

    // msg_id dedup — WeCom may retry the same webhook on timeout, or echo bot messages.
    // Events (msg_type=event) have no MsgId; only deduplicate real messages.
    if let Some(ref id) = msg_id {
        if !id.is_empty() {
            let mut seen = SEEN_MSG_IDS.lock().unwrap();
            if seen.contains(id.as_str()) {
                tracing::debug!(msg_id = %id, "WeCom webhook: duplicate msg_id, skipping");
                return (200, "success".to_string());
            }
            // Evict oldest entries if set is too large
            if seen.len() >= SEEN_MSG_IDS_MAX {
                seen.clear();
            }
            seen.insert(id.clone());
        }
    }

    // Allowlist check (applies to all message types)
    let allow_from = &wecom_cfg.allow_from;
    if !allow_from.is_empty() && !allow_from.iter().any(|a| a == &from_user) {
        tracing::debug!(from_user = %from_user, "WeCom webhook: user not in allowlist");
        return (200, "success".to_string());
    }

    // Determine text content, optional media paths, and whether to await user intent
    // before processing (true = channel already sent ack, agent should ask what to do)
    let (final_content, media_paths, media_pending_intent) = match msg_type.as_str() {
        "text" => {
            let c = content.trim().to_string();
            if c.is_empty() {
                return (200, "success".to_string());
            }
            (c, vec![], false)
        }
        "image" => {
            let media_id = extract_xml_tag(&decrypted_body, "MediaId").unwrap_or_default();
            let pic_url = extract_xml_tag(&decrypted_body, "PicUrl").unwrap_or_default();
            info!(media_id = %media_id, "WeCom webhook: received image");
            let paths = if !media_id.is_empty() {
                match download_wecom_media(&resolved_config, &media_id, "image", None, &media_dir)
                    .await
                {
                    Ok(p) => vec![p],
                    Err(e) => {
                        warn!(error = %e.to_string(), "WeCom: failed to download image, using PicUrl");
                        if !pic_url.is_empty() {
                            vec![pic_url]
                        } else {
                            vec![]
                        }
                    }
                }
            } else if !pic_url.is_empty() {
                vec![pic_url]
            } else {
                vec![]
            };
            ("用户发来了一张图片，请问您需要我做什么？（例如：描述图片内容、识别文字、发回给您等）".to_string(), paths, true)
        }
        "voice" => {
            let media_id = extract_xml_tag(&decrypted_body, "MediaId").unwrap_or_default();
            let format =
                extract_xml_tag(&decrypted_body, "Format").unwrap_or_else(|| "amr".to_string());
            info!(media_id = %media_id, format = %format, "WeCom webhook: received voice");
            let paths = if !media_id.is_empty() {
                match download_wecom_media(
                    &resolved_config,
                    &media_id,
                    "voice",
                    Some(&format),
                    &media_dir,
                )
                .await
                {
                    Ok(p) => vec![p],
                    Err(e) => {
                        warn!(error = %e.to_string(), "WeCom: failed to download voice");
                        vec![]
                    }
                }
            } else {
                vec![]
            };
            // Send immediate ack
            if !from_user.is_empty() {
                let _ =
                    send_message(&resolved_config, &from_user, "🎤 语音已收到，正在转写...").await;
            }
            // Voice: always transcribe immediately, no pending intent needed
            ("用户发来了一条语音消息，请先用 audio_transcribe 工具转写，然后根据转写内容回复用户。".to_string(), paths, false)
        }
        "video" | "shortvideo" => {
            let media_id = extract_xml_tag(&decrypted_body, "MediaId").unwrap_or_default();
            info!(media_id = %media_id, "WeCom webhook: received video");
            let paths = if !media_id.is_empty() {
                match download_wecom_media(
                    &resolved_config,
                    &media_id,
                    "video",
                    Some("mp4"),
                    &media_dir,
                )
                .await
                {
                    Ok(p) => vec![p],
                    Err(e) => {
                        warn!(error = %e.to_string(), "WeCom: failed to download video");
                        vec![]
                    }
                }
            } else {
                vec![]
            };
            (
                "用户发来了一个视频，请问您需要我做什么？（例如：提取音频、截取片段等）"
                    .to_string(),
                paths,
                true,
            )
        }
        "file" => {
            let media_id = extract_xml_tag(&decrypted_body, "MediaId").unwrap_or_default();
            let file_name = extract_xml_tag(&decrypted_body, "FileName").unwrap_or_default();
            let ext = file_name.rsplit('.').next().map(|s| s.to_string());
            info!(media_id = %media_id, file_name = %file_name, "WeCom webhook: received file");
            let paths = if !media_id.is_empty() {
                match download_wecom_media(
                    &resolved_config,
                    &media_id,
                    "file",
                    ext.as_deref(),
                    &media_dir,
                )
                .await
                {
                    Ok(p) => vec![p],
                    Err(e) => {
                        warn!(error = %e.to_string(), "WeCom: failed to download file");
                        vec![]
                    }
                }
            } else {
                vec![]
            };
            let desc = if file_name.is_empty() {
                "用户发来了一个文件，请问您需要我做什么？（例如：读取内容、分析数据等）".to_string()
            } else {
                format!(
                    "用户发来了文件「{}」，请问您需要我做什么？（例如：读取内容、分析数据等）",
                    file_name
                )
            };
            (desc, paths, true)
        }
        "location" => {
            let lat = extract_xml_tag(&decrypted_body, "Location_X").unwrap_or_default();
            let lon = extract_xml_tag(&decrypted_body, "Location_Y").unwrap_or_default();
            let label = extract_xml_tag(&decrypted_body, "Label").unwrap_or_default();
            let c = if label.is_empty() {
                format!("[位置] 纬度:{} 经度:{}", lat, lon)
            } else {
                format!("[位置] {} (纬度:{} 经度:{})", label, lat, lon)
            };
            (c, vec![], false)
        }
        "link" => {
            let title = extract_xml_tag(&decrypted_body, "Title").unwrap_or_default();
            let url = extract_xml_tag(&decrypted_body, "Url").unwrap_or_default();
            let c = format!("[链接] {} {}", title, url);
            (c, vec![], false)
        }
        other => {
            info!(msg_type = %other, "WeCom webhook: unsupported message type, skipping");
            return (200, "success".to_string());
        }
    };

    if let Some(tx) = inbound_tx {
        let inbound = blockcell_core::InboundMessage {
            channel: "wecom".to_string(),
            account_id: wecom_account_id(&resolved_config),
            sender_id: from_user.clone(),
            chat_id: from_user.clone(),
            content: final_content,
            media: media_paths,
            metadata: serde_json::json!({
                "msg_id": msg_id,
                "msg_type": msg_type,
                "mode": "webhook",
                "media_pending_intent": media_pending_intent,
            }),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };
        if let Err(e) = tx.send(inbound).await {
            tracing::error!(error = %e, "WeCom webhook: failed to forward inbound message");
        }
    }

    (200, "success".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wecom_verification_diagnostics_redact_secrets() {
        let diagnostics = WecomVerificationDiagnostics::new(
            "callback-secret",
            "aes-secret",
            "1700000000",
            "nonce-secret",
            "signature-secret",
            "encrypted-secret",
        );
        let rendered = format!("{diagnostics:?}");

        for secret in [
            "callback-secret",
            "aes-secret",
            "nonce-secret",
            "signature-secret",
            "encrypted-secret",
        ] {
            assert!(!rendered.contains(secret), "diagnostics leaked {secret}");
        }
        assert!(rendered.contains("token_configured: true"));
        assert!(rendered.contains("aes_key_configured: true"));
    }
}
