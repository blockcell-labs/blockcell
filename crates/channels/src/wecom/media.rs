use super::*;

/// Download a WeCom media file (image/voice/video/file) by media_id.
/// Saves to `~/.blockcell/media/wecom_{media_id}.{ext}` and returns the local path.
pub(crate) async fn download_wecom_media(
    config: &Config,
    media_id: &str,
    media_type: &str,
    ext_hint: Option<&str>,
    media_dir: &Path,
) -> Result<String> {
    let client = shared_client();
    let mut retried = false;
    let resp = loop {
        let token = fetch_access_token_static(&client, config).await?;
        let url = format!(
            "{}/media/get?access_token={}&media_id={}",
            WECOM_API_BASE, token, media_id
        );
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("WeCom media/get request failed: {}", e)))?;

        if let Some(ct) = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
        {
            if ct.contains("application/json") {
                let err: WeComResponse = resp
                    .json()
                    .await
                    .map_err(|e| Error::Channel(format!("WeCom media/get parse failed: {}", e)))?;
                if err.is_invalid_token() && !retried {
                    warn!(errcode = err.errcode, errmsg = %err.errmsg, "WeCom media/get token invalid, refreshing and retrying once");
                    clear_access_token_cache().await;
                    retried = true;
                    continue;
                }
                return Err(Error::Channel(format!(
                    "WeCom media/get error {}: {}",
                    err.errcode, err.errmsg
                )));
            }
        }

        break resp;
    };

    if !resp.status().is_success() {
        return Err(Error::Channel(format!(
            "WeCom media/get HTTP {}",
            resp.status()
        )));
    }

    // Determine file extension from Content-Type or hint
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let ext = ext_hint
        .map(|s| s.to_string())
        .unwrap_or_else(|| ext_from_content_type(&content_type, media_type).to_string());

    // 使用调用方传入的 media_dir，避免并发环境下环境变量竞争
    tokio::fs::create_dir_all(media_dir)
        .await
        .map_err(|e| Error::Channel(format!("Failed to create media dir: {}", e)))?;

    // media_id is caller/platform supplied: truncate on a char boundary and
    // sanitize every component so it cannot escape media_dir or panic on slicing.
    let id_prefix: String = media_id.chars().take(16).collect();
    let filename = format!(
        "wecom_{}_{}.{}",
        crate::security::safe_path_component(media_type),
        crate::security::safe_path_component(&id_prefix),
        crate::security::safe_path_component(&ext)
    );
    let file_path = media_dir.join(&filename);

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| Error::Channel(format!("WeCom media/get read body failed: {}", e)))?;

    tokio::fs::write(&file_path, &bytes)
        .await
        .map_err(|e| Error::Channel(format!("WeCom media/get write failed: {}", e)))?;

    let path_str = file_path.to_string_lossy().to_string();
    info!(path = %path_str, bytes = bytes.len(), "WeCom: media downloaded");
    Ok(path_str)
}

pub(crate) fn ext_from_content_type(content_type: &str, media_type: &str) -> &'static str {
    if content_type.contains("jpeg") || content_type.contains("jpg") {
        return "jpg";
    }
    if content_type.contains("png") {
        return "png";
    }
    if content_type.contains("gif") {
        return "gif";
    }
    if content_type.contains("mp4") {
        return "mp4";
    }
    if content_type.contains("amr") {
        return "amr";
    }
    if content_type.contains("speex") {
        return "speex";
    }
    match media_type {
        "image" => "jpg",
        "voice" => "amr",
        "video" => "mp4",
        _ => "bin",
    }
}

/// Extract the text content of an XML tag (simple, no namespace support needed for WeCom).
pub(crate) fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let content = &xml[start..end];
    // Strip CDATA if present
    let content = if content.starts_with("<![CDATA[") && content.ends_with("]]>") {
        &content[9..content.len() - 3]
    } else {
        content
    };
    Some(content.to_string())
}

/// Verify WeCom 4-param signature: SHA1(sort(token, timestamp, nonce, msg_encrypt))
/// This is the correct signature for both GET (echostr) and POST (Encrypt) callbacks.
pub(crate) fn verify_signature_4(
    token: &str,
    timestamp: &str,
    nonce: &str,
    msg_encrypt: &str,
    expected: &str,
) -> bool {
    let mut parts = [token, timestamp, nonce, msg_encrypt];
    parts.sort_unstable();
    let combined = parts.join("");
    let hash = sha1_hex(combined.as_bytes());
    hash == expected
}

/// Decrypt a WeCom AES-256-CBC encrypted message.
///
/// Protocol:
/// - AES key = Base64Decode(encodingAESKey + "=")  → 32 bytes
/// - IV = first 16 bytes of AES key
/// - Ciphertext = Base64Decode(msg_encrypt)
/// - Plaintext layout: 16B random | 4B msg_len (big-endian) | msg | corpId
pub(crate) fn decrypt_wecom_msg(
    msg_encrypt: &str,
    encoding_aes_key: &str,
) -> std::result::Result<String, String> {
    if encoding_aes_key.is_empty() {
        return Err("encodingAesKey not configured".to_string());
    }

    tracing::debug!(
        encoding_aes_key_len = encoding_aes_key.len(),
        msg_encrypt_len = msg_encrypt.len(),
        "WeCom decrypt: received encrypted payload"
    );

    // AES key: WeCom's EncodingAESKey is always exactly 43 chars of standard base64
    // (no padding). Append one '=' to make it 44 chars (valid base64 group).
    // Do NOT strip existing padding first — just normalise whitespace, then pad to 44.
    let key_compact: String = encoding_aes_key
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let key_trimmed = key_compact.trim_end_matches('=');

    tracing::debug!(
        key_trimmed_len = key_trimmed.len(),
        "WeCom decrypt: key after normalisation"
    );

    let padded_key = match key_trimmed.len() % 4 {
        0 => key_trimmed.to_string(),
        2 => format!("{}==", key_trimmed),
        3 => format!("{}=", key_trimmed),
        // len % 4 == 1 is never valid base64
        _ => {
            return Err(format!(
                "Invalid EncodingAESKey length: {} (after whitespace removal / padding strip)",
                key_trimmed.len()
            ))
        }
    };

    tracing::debug!(
        padded_key_len = padded_key.len(),
        "WeCom decrypt: padded key"
    );

    // WeCom's EncodingAESKey may have non-zero trailing bits in the last base64 character
    // (e.g. '3' instead of the canonical '0'). Rust's STANDARD engine rejects this strictly,
    // so use a lenient engine that ignores trailing bits and accepts optional padding.
    const LENIENT: GeneralPurpose = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    let key_bytes = LENIENT
        .decode(&padded_key)
        .map_err(|e| format!("Failed to decode EncodingAESKey: {}", e))?;
    if key_bytes.len() != 32 {
        return Err(format!(
            "AES key length invalid after base64 decode: {} (expected 32). Please verify WeCom EncodingAESKey is correct (usually 43 chars, no '=').",
            key_bytes.len()
        ));
    }

    // IV = first 16 bytes of key
    let iv = &key_bytes[..16];

    // Decode ciphertext
    tracing::debug!(
        msg_encrypt_len = msg_encrypt.len(),
        "WeCom decrypt: decoding msg_encrypt ciphertext"
    );
    let ciphertext = general_purpose::STANDARD.decode(msg_encrypt).map_err(|e| {
        format!(
            "Failed to decode msg_encrypt (len={}): {}",
            msg_encrypt.len(),
            e
        )
    })?;

    // AES-256-CBC decrypt — WeCom uses PKCS7 with block size 32 (not 16),
    // so pad values 1-32 are valid. Use NoPadding and unpad manually.
    use aes::cipher::block_padding::NoPadding;
    let decryptor = Aes256CbcDec::new_from_slices(&key_bytes, iv)
        .map_err(|e| format!("Failed to create AES decryptor: {}", e))?;
    let mut buf = ciphertext.clone();
    let decrypted = decryptor
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map_err(|e| format!("AES decrypt failed: {}", e))?;
    // Manual PKCS7 unpad with block size 32
    let pad = *decrypted.last().ok_or("AES decrypt: empty output")? as usize;
    if pad == 0 || pad > 32 {
        return Err(format!("AES decrypt: invalid PKCS7 pad value {}", pad));
    }
    let plaintext = &decrypted[..decrypted.len() - pad];

    // Layout: 16B random | 4B msg_len (big-endian) | msg | corpId
    if plaintext.len() < 20 {
        return Err(format!(
            "Decrypted data too short: {} bytes",
            plaintext.len()
        ));
    }

    let msg_len =
        u32::from_be_bytes([plaintext[16], plaintext[17], plaintext[18], plaintext[19]]) as usize;

    let content_start = 20;
    let content_end = content_start + msg_len;
    if content_end > plaintext.len() {
        return Err(format!(
            "msg_len {} exceeds plaintext length {}",
            msg_len,
            plaintext.len()
        ));
    }

    let msg = std::str::from_utf8(&plaintext[content_start..content_end])
        .map_err(|e| format!("UTF-8 decode failed: {}", e))?;

    Ok(msg.to_string())
}

// ── Long connection chunked upload ─────────────────────────────────────────

/// Max raw bytes per chunk before base64 encoding (512 KB).
pub(crate) const LONGCONN_CHUNK_SIZE: usize = 512 * 1024;

/// Upload a file via the WeCom long connection chunked protocol and then send
/// the resulting media message.  Returns `Ok(media_id)` on success.
///
/// Protocol:
///   1. `aibot_upload_media_init`  -> get `upload_id`
///   2. `aibot_upload_media_chunk` -> send each chunk (base64-encoded, <=512 KB raw)
///   3. `aibot_upload_media_finish` -> merge chunks -> get `media_id`
///   4. `aibot_send_msg`           -> send image/voice/video/file message
pub(crate) async fn longconn_upload_and_send_media<S, R>(
    write: &mut S,
    read: &mut R,
    chat_id: &str,
    file_path: &str,
    media_type: &str,
    title: &str,
) -> Result<String>
where
    S: futures::Sink<WsMessage> + Unpin,
    S::Error: std::fmt::Display,
    R: futures::Stream<
            Item = std::result::Result<WsMessage, tokio_tungstenite::tungstenite::Error>,
        > + Unpin,
{
    use base64::Engine;

    let path = std::path::Path::new(file_path);
    let file_bytes = tokio::fs::read(path).await.map_err(|e| {
        Error::Channel(format!(
            "WeCom longconn upload: failed to read {}: {}",
            file_path, e
        ))
    })?;

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    let total_size = file_bytes.len();
    let total_chunks = total_size.div_ceil(LONGCONN_CHUNK_SIZE);
    if total_chunks > 100 {
        return Err(Error::Channel(format!(
            "WeCom longconn upload: file too large ({} bytes, {} chunks > 100 max)",
            total_size, total_chunks
        )));
    }

    let file_md5 = format!("{:x}", md5::compute(&file_bytes));
    info!(
        file_path = %file_path, total_size, total_chunks, md5 = %file_md5,
        "WeCom longconn: starting chunked media upload"
    );

    // Step 1: aibot_upload_media_init
    let init_req_id = format!("upload-init-{}", chrono::Utc::now().timestamp_millis());
    let init_msg = serde_json::json!({
        "cmd": "aibot_upload_media_init",
        "headers": { "req_id": &init_req_id },
        "body": {
            "type": media_type,
            "filename": file_name,
            "total_size": total_size,
            "total_chunks": total_chunks,
            "md5": file_md5,
        }
    });
    longconn_ws_send(write, &init_msg).await?;

    let init_resp = longconn_wait_response(write, read, &init_req_id).await?;
    let errcode = init_resp["errcode"].as_i64().unwrap_or(-1);
    if errcode != 0 {
        return Err(Error::Channel(format!(
            "WeCom longconn upload init error {}: {}",
            errcode,
            init_resp["errmsg"].as_str().unwrap_or("unknown")
        )));
    }
    let upload_id = init_resp["body"]["upload_id"]
        .as_str()
        .ok_or_else(|| Error::Channel("WeCom longconn upload init: no upload_id".to_string()))?
        .to_string();
    info!(upload_id = %upload_id, "WeCom longconn: upload initialized");

    // Step 2: aibot_upload_media_chunk (send each chunk sequentially)
    for i in 0..total_chunks {
        let start = i * LONGCONN_CHUNK_SIZE;
        let end = ((i + 1) * LONGCONN_CHUNK_SIZE).min(total_size);
        let chunk_data = &file_bytes[start..end];
        let b64 = base64::engine::general_purpose::STANDARD.encode(chunk_data);

        let chunk_req_id = format!(
            "upload-chunk-{}-{}",
            i,
            chrono::Utc::now().timestamp_millis()
        );
        let chunk_msg = serde_json::json!({
            "cmd": "aibot_upload_media_chunk",
            "headers": { "req_id": &chunk_req_id },
            "body": {
                "upload_id": &upload_id,
                "chunk_index": i,
                "base64_data": b64,
            }
        });
        longconn_ws_send(write, &chunk_msg).await?;

        let chunk_resp = longconn_wait_response(write, read, &chunk_req_id).await?;
        let chunk_err = chunk_resp["errcode"].as_i64().unwrap_or(-1);
        if chunk_err != 0 {
            return Err(Error::Channel(format!(
                "WeCom longconn chunk {} upload error {}: {}",
                i,
                chunk_err,
                chunk_resp["errmsg"].as_str().unwrap_or("unknown")
            )));
        }
        debug!(
            chunk = i,
            total = total_chunks,
            "WeCom longconn: chunk uploaded"
        );
    }

    // Step 3: aibot_upload_media_finish
    let finish_req_id = format!("upload-finish-{}", chrono::Utc::now().timestamp_millis());
    let finish_msg = serde_json::json!({
        "cmd": "aibot_upload_media_finish",
        "headers": { "req_id": &finish_req_id },
        "body": { "upload_id": &upload_id }
    });
    longconn_ws_send(write, &finish_msg).await?;

    let finish_resp = longconn_wait_response(write, read, &finish_req_id).await?;
    let finish_err = finish_resp["errcode"].as_i64().unwrap_or(-1);
    if finish_err != 0 {
        return Err(Error::Channel(format!(
            "WeCom longconn upload finish error {}: {}",
            finish_err,
            finish_resp["errmsg"].as_str().unwrap_or("unknown")
        )));
    }
    let media_id = finish_resp["body"]["media_id"]
        .as_str()
        .ok_or_else(|| Error::Channel("WeCom longconn upload finish: no media_id".to_string()))?
        .to_string();
    info!(media_id = %media_id, "WeCom longconn: upload complete");

    // Step 4: aibot_send_msg with media_id
    let send_req_id = {
        let reg = CHAT_REQID_REGISTRY.lock().unwrap();
        reg.get(chat_id)
            .cloned()
            .unwrap_or_else(|| format!("blockcell-media-{}", chrono::Utc::now().timestamp_millis()))
    };

    let media_body = match media_type {
        "image" => serde_json::json!({ "media_id": &media_id }),
        "voice" => serde_json::json!({ "media_id": &media_id }),
        "video" => serde_json::json!({
            "media_id": &media_id,
            "title": if title.is_empty() { &file_name } else { title },
            "description": ""
        }),
        _ => serde_json::json!({ "media_id": &media_id }),
    };

    let send_msg = serde_json::json!({
        "cmd": "aibot_send_msg",
        "headers": { "req_id": &send_req_id },
        "body": {
            "chatid": chat_id,
            "chat_type": 1,
            "msgtype": media_type,
            media_type: media_body,
        }
    });
    longconn_ws_send(write, &send_msg).await?;

    let send_resp = longconn_wait_response(write, read, &send_req_id).await?;
    let send_err = send_resp["errcode"].as_i64().unwrap_or(-1);
    if send_err != 0 {
        warn!(
            errcode = send_err,
            errmsg = %send_resp["errmsg"].as_str().unwrap_or("unknown"),
            "WeCom longconn: aibot_send_msg error (media may still have been delivered)"
        );
    }

    info!(media_id = %media_id, media_type = %media_type, chat_id = %chat_id, "WeCom longconn: media message sent");
    Ok(media_id)
}

/// Send a JSON payload over the WebSocket.
pub(crate) async fn longconn_ws_send<S>(write: &mut S, msg: &serde_json::Value) -> Result<()>
where
    S: futures::Sink<WsMessage> + Unpin,
    S::Error: std::fmt::Display,
{
    let text = msg.to_string();
    write
        .send(WsMessage::Text(text))
        .await
        .map_err(|e| Error::Channel(format!("WeCom longconn WS send failed: {}", e)))
}

/// Wait for a WS response whose `headers.req_id` matches `expected_req_id`.
/// While waiting, handle ping/pong and log any unrelated messages.
/// Times out after 60 seconds.
pub(crate) async fn longconn_wait_response<S, R>(
    write: &mut S,
    read: &mut R,
    expected_req_id: &str,
) -> Result<serde_json::Value>
where
    S: futures::Sink<WsMessage> + Unpin,
    S::Error: std::fmt::Display,
    R: futures::Stream<
            Item = std::result::Result<WsMessage, tokio_tungstenite::tungstenite::Error>,
        > + Unpin,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);

    loop {
        let msg = tokio::time::timeout_at(deadline, read.next())
            .await
            .map_err(|_| {
                Error::Channel(format!(
                    "WeCom longconn: timeout waiting for response to {}",
                    expected_req_id
                ))
            })?;

        match msg {
            Some(Ok(WsMessage::Text(text))) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    let rid = v["headers"]["req_id"].as_str().unwrap_or("");
                    if rid == expected_req_id {
                        return Ok(v);
                    }
                    debug!(
                        req_id = %rid,
                        expected = %expected_req_id,
                        "WeCom longconn: received unrelated message while waiting for upload response"
                    );
                }
            }
            Some(Ok(WsMessage::Binary(data))) => {
                let text = String::from_utf8_lossy(&data);
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    let rid = v["headers"]["req_id"].as_str().unwrap_or("");
                    if rid == expected_req_id {
                        return Ok(v);
                    }
                }
            }
            Some(Ok(WsMessage::Ping(data))) => {
                let _ = write.send(WsMessage::Pong(data)).await;
            }
            Some(Ok(WsMessage::Pong(_))) => {}
            Some(Ok(WsMessage::Close(frame))) => {
                return Err(Error::Channel(format!(
                    "WeCom longconn: connection closed while waiting for upload response: {:?}",
                    frame
                )));
            }
            Some(Err(e)) => {
                return Err(Error::Channel(format!(
                    "WeCom longconn: WS read error while waiting for upload response: {}",
                    e
                )));
            }
            None => {
                return Err(Error::Channel(
                    "WeCom longconn: connection ended while waiting for upload response"
                        .to_string(),
                ));
            }
            _ => {}
        }
    }
}

/// Upload a local file to WeCom as a temporary media asset.
/// Returns the `media_id` (valid for 3 days).
/// `media_type` must be one of: image / voice / video / file
pub async fn upload_media(config: &Config, file_path: &str, media_type: &str) -> Result<String> {
    let client = shared_client();
    let path = std::path::Path::new(file_path);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| Error::Channel(format!("Failed to read media file {}: {}", file_path, e)))?;

    let mime = mime_for_path(file_path);
    #[derive(Deserialize)]
    struct UploadResp {
        errcode: i32,
        errmsg: String,
        #[serde(default)]
        media_id: Option<String>,
    }

    let mut retried = false;
    let result: UploadResp = loop {
        let token = fetch_access_token_static(&client, config).await?;
        let url = format!(
            "{}/media/upload?access_token={}&type={}",
            WECOM_API_BASE, token, media_type
        );
        let part = reqwest::multipart::Part::bytes(bytes.clone())
            .file_name(file_name.clone())
            .mime_str(mime)
            .map_err(|e| Error::Channel(format!("Invalid MIME type: {}", e)))?;
        let form = reqwest::multipart::Form::new().part("media", part);
        let resp = client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("WeCom media/upload failed: {}", e)))?;
        let result: UploadResp = resp
            .json()
            .await
            .map_err(|e| Error::Channel(format!("WeCom media/upload parse failed: {}", e)))?;
        if matches!(result.errcode, 40014 | 42001) && !retried {
            warn!(errcode = result.errcode, errmsg = %result.errmsg, "WeCom media/upload token invalid, refreshing and retrying once");
            clear_access_token_cache().await;
            retried = true;
            continue;
        }
        break result;
    };

    if result.errcode != 0 {
        return Err(Error::Channel(format!(
            "WeCom media/upload error {}: {}",
            result.errcode, result.errmsg
        )));
    }

    result
        .media_id
        .ok_or_else(|| Error::Channel("WeCom media/upload: no media_id in response".to_string()))
}

pub(crate) fn mime_for_path(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "amr" => "audio/amr",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "zip" => "application/zip",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Send a media message (image/voice/video/file) to a WeCom user or group.
/// `file_path` is a local file path.  `caption` is an optional text shown alongside the image
/// (long_connection mode only; ignored for REST API mode).
pub async fn send_media_message(
    config: &Config,
    chat_id: &str,
    file_path: &str,
    _caption: &str,
) -> Result<()> {
    // Long connection mode: upload via WS chunked protocol, then send via aibot_send_msg.
    let mode = config.channels.wecom.mode.trim().to_lowercase();
    if mode == "long_connection" || mode == "long-connection" || mode == "stream" {
        let ext = file_path.rsplit('.').next().unwrap_or("").to_lowercase();
        let (media_type, _msg_type) = media_type_for_ext(&ext);

        let upload_path = if media_type == "voice" {
            let amr_path = ensure_wecom_voice_amr(file_path).await?;
            let duration = probe_audio_duration(&amr_path).await.unwrap_or(0.0);
            if duration > 60.0 {
                info!(duration = %duration, "WeCom longconn: voice too long (>60s), sending as file");
                file_path.to_string()
            } else {
                amr_path
            }
        } else {
            file_path.to_string()
        };

        let actual_ext = upload_path.rsplit('.').next().unwrap_or("").to_lowercase();
        let (actual_media_type, _) = media_type_for_ext(&actual_ext);

        let bot_id = config.channels.wecom.bot_id.trim().to_string();
        // Clone the sender out of the lock so the MutexGuard is dropped before .await
        let tx_clone = {
            let registry = LONGCONN_REGISTRY.lock().unwrap();
            registry.get(&bot_id).cloned()
        };
        if let Some(tx) = tx_clone {
            let (result_tx, result_rx) = oneshot::channel();
            let msg = LongConnOutbound::Media {
                chat_id: chat_id.to_string(),
                file_path: upload_path,
                media_type: actual_media_type.to_string(),
                title: String::new(),
                result_tx,
            };
            if let Err(e) = tx.try_send(msg) {
                return Err(Error::Channel(format!(
                    "WeCom longconn: failed to queue media upload: {}",
                    e
                )));
            }
            match result_rx.await {
                Ok(Ok(_media_id)) => return Ok(()),
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    return Err(Error::Channel(
                        "WeCom longconn: media upload channel dropped".to_string(),
                    ))
                }
            }
        } else {
            warn!(bot_id = %bot_id, "WeCom long connection not active; media message dropped");
            return Ok(());
        }
    }

    crate::rate_limit::wecom_limiter().acquire().await;

    let ext = file_path.rsplit('.').next().unwrap_or("").to_lowercase();
    let (mut media_type, mut msg_type) = media_type_for_ext(&ext);

    let upload_path = if media_type == "voice" {
        let amr_path = ensure_wecom_voice_amr(file_path).await?;
        // WeCom voice messages have a 60-second limit. If the audio is longer,
        // fall back to sending as a file so the user still gets the full audio.
        let duration = probe_audio_duration(&amr_path).await.unwrap_or(0.0);
        if duration > 60.0 {
            info!(duration = %duration, "WeCom: voice too long (>60s), sending as file instead");
            media_type = "file";
            msg_type = "file";
            file_path.to_string() // send original file (mp3), not the AMR
        } else {
            amr_path
        }
    } else {
        file_path.to_string()
    };

    info!(file_path = %upload_path, media_type = %media_type, "WeCom: uploading media");
    let media_id = upload_media(config, &upload_path, media_type).await?;
    info!(media_id = %media_id, "WeCom: media uploaded");

    let client = shared_client();
    let token = fetch_access_token_static(&client, config).await?;
    let agent_id = config.channels.wecom.agent_id;

    let is_group = chat_id.starts_with("wr") || chat_id.starts_with("WR");

    let body = if is_group {
        build_media_body_group(chat_id, msg_type, &media_id)
    } else {
        build_media_body_user(chat_id, agent_id, msg_type, &media_id)
    };

    let endpoint = if is_group {
        format!("{}/appchat/send", WECOM_API_BASE)
    } else {
        format!("{}/message/send", WECOM_API_BASE)
    };

    let resp = client
        .post(&endpoint)
        .query(&[("access_token", token.as_str())])
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("WeCom send media failed: {}", e)))?;

    let result: WeComResponse = resp
        .json()
        .await
        .map_err(|e| Error::Channel(format!("WeCom send media parse failed: {}", e)))?;

    if result.errcode != 0 {
        return Err(Error::Channel(format!(
            "WeCom send media error {}: {}",
            result.errcode, result.errmsg
        )));
    }

    Ok(())
}

pub(crate) async fn ensure_wecom_voice_amr(file_path: &str) -> Result<String> {
    let ext = file_path.rsplit('.').next().unwrap_or("").to_lowercase();
    if ext == "amr" {
        return Ok(file_path.to_string());
    }

    let input = std::path::Path::new(file_path);
    if !input.exists() {
        return Err(Error::Channel(format!(
            "WeCom voice: input file not found: {}",
            file_path
        )));
    }

    // 输出目录与输入文件同目录，避免依赖全局环境变量
    let media_dir = input.parent().unwrap_or_else(|| std::path::Path::new("."));
    tokio::fs::create_dir_all(&media_dir)
        .await
        .map_err(|e| Error::Channel(format!("Failed to create media dir: {}", e)))?;

    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("voice");
    let ts = chrono::Utc::now().timestamp_millis();
    let output = media_dir.join(format!("{}_{}.amr", stem, ts));

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .arg("-i")
        .arg(input)
        .arg("-ac")
        .arg("1")
        .arg("-ar")
        .arg("8000")
        .arg("-c:a")
        .arg("amr_nb")
        .arg(&output);

    let out = cmd.output().await.map_err(|e| {
        Error::Channel(format!(
            "WeCom voice: ffmpeg not available or failed to start: {}",
            e
        ))
    })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return Err(Error::Channel(format!(
            "WeCom voice: failed to convert to amr (WeCom voice only supports .amr). ffmpeg stderr: {}",
            stderr
        )));
    }

    let output_str = output.to_string_lossy().to_string();
    if !std::path::Path::new(&output_str).exists() {
        return Err(Error::Channel(
            "WeCom voice: conversion succeeded but output file missing".to_string(),
        ));
    }

    Ok(output_str)
}

/// Probe audio duration in seconds using ffprobe. Returns None on failure.
pub(crate) async fn probe_audio_duration(file_path: &str) -> Option<f64> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(file_path)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    text.trim().parse::<f64>().ok()
}

pub(crate) fn media_type_for_ext(ext: &str) -> (&'static str, &'static str) {
    match ext {
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" => ("image", "image"),
        "amr" | "mp3" | "wav" | "m4a" | "speex" => ("voice", "voice"),
        "mp4" | "avi" | "mov" | "mkv" => ("video", "video"),
        _ => ("file", "file"),
    }
}

pub(crate) fn build_media_body_user(
    to_user: &str,
    agent_id: i64,
    msg_type: &str,
    media_id: &str,
) -> serde_json::Value {
    match msg_type {
        "image" => serde_json::json!({
            "touser": to_user,
            "msgtype": "image",
            "agentid": agent_id,
            "image": { "media_id": media_id },
            "safe": 0
        }),
        "voice" => serde_json::json!({
            "touser": to_user,
            "msgtype": "voice",
            "agentid": agent_id,
            "voice": { "media_id": media_id },
            "safe": 0
        }),
        "video" => serde_json::json!({
            "touser": to_user,
            "msgtype": "video",
            "agentid": agent_id,
            "video": { "media_id": media_id, "title": "", "description": "" },
            "safe": 0
        }),
        _ => serde_json::json!({
            "touser": to_user,
            "msgtype": "file",
            "agentid": agent_id,
            "file": { "media_id": media_id },
            "safe": 0
        }),
    }
}

pub(crate) fn build_media_body_group(
    chat_id: &str,
    msg_type: &str,
    media_id: &str,
) -> serde_json::Value {
    match msg_type {
        "image" => serde_json::json!({
            "chatid": chat_id,
            "msgtype": "image",
            "image": { "media_id": media_id },
            "safe": 0
        }),
        "voice" => serde_json::json!({
            "chatid": chat_id,
            "msgtype": "voice",
            "voice": { "media_id": media_id },
            "safe": 0
        }),
        "video" => serde_json::json!({
            "chatid": chat_id,
            "msgtype": "video",
            "video": { "media_id": media_id, "title": "", "description": "" },
            "safe": 0
        }),
        _ => serde_json::json!({
            "chatid": chat_id,
            "msgtype": "file",
            "file": { "media_id": media_id },
            "safe": 0
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decrypt_errors_do_not_echo_secrets() {
        let key = "TOP_SECRET_INVALID_KEY";
        let ciphertext = "TOP_SECRET_INVALID_CIPHERTEXT";
        let error = decrypt_wecom_msg(ciphertext, key).unwrap_err();
        assert!(!error.contains(key));
        assert!(!error.contains(ciphertext));
    }
}
