use blockcell_core::{Config, Error, InboundMessage, Result};
use reqwest::Client;
use reqwest::Proxy;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};

const TELEGRAM_API_BASE: &str = "https://api.telegram.org";

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    message_id: i64,
    from: Option<User>,
    chat: Chat,
    text: Option<String>,
    caption: Option<String>,
    photo: Option<Vec<PhotoSize>>,
    voice: Option<Voice>,
    document: Option<Document>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct PhotoSize {
    file_id: String,
    file_unique_id: String,
    width: i32,
    height: i32,
    file_size: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Voice {
    file_id: String,
    file_unique_id: String,
    duration: i32,
    file_size: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Document {
    file_id: String,
    file_unique_id: String,
    file_name: Option<String>,
    file_size: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FileResponse {
    file_id: String,
    file_unique_id: String,
    file_size: Option<i64>,
    file_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct User {
    id: i64,
    username: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Chat {
    id: i64,
}

pub struct TelegramChannel {
    config: Config,
    client: Client,
    inbound_tx: mpsc::Sender<InboundMessage>,
    media_dir: PathBuf,
}

impl TelegramChannel {
    pub fn new(config: Config, inbound_tx: mpsc::Sender<InboundMessage>) -> Self {
        let mut builder = Client::builder().timeout(Duration::from_secs(60));

        if let Some(proxy) = config.channels.telegram.proxy.as_deref() {
            match Proxy::all(proxy) {
                Ok(p) => {
                    builder = builder.proxy(p);
                    info!(proxy = %proxy, "Telegram proxy configured");
                }
                Err(e) => {
                    warn!(error = %e, proxy = %proxy, "Invalid Telegram proxy, ignoring");
                }
            }
        }

        let client = builder.build().expect("Failed to create HTTP client");

        let media_dir = dirs::home_dir()
            .map(|h| h.join(".blockcell/media"))
            .unwrap_or_else(|| PathBuf::from(".blockcell/media"));

        // Ensure media directory exists
        let _ = std::fs::create_dir_all(&media_dir);

        Self {
            config,
            client,
            inbound_tx,
            media_dir,
        }
    }

    fn api_url(&self, method: &str) -> String {
        format!(
            "{}/bot{}/{}",
            TELEGRAM_API_BASE, self.config.channels.telegram.token, method
        )
    }

    fn is_allowed(&self, user: &User) -> bool {
        let allow_from = &self.config.channels.telegram.allow_from;
        
        if allow_from.is_empty() {
            return true;
        }

        let user_id = user.id.to_string();
        let username = user.username.as_deref().unwrap_or("");

        allow_from.iter().any(|allowed| {
            if allowed.contains('|') {
                let parts: Vec<&str> = allowed.split('|').collect();
                parts.contains(&user_id.as_str()) || parts.contains(&username)
            } else {
                allowed == &user_id || allowed == username
            }
        })
    }

    async fn get_updates(&self, offset: Option<i64>) -> Result<Vec<Update>> {
        let mut params = vec![("timeout", "30".to_string())];
        if let Some(off) = offset {
            params.push(("offset", off.to_string()));
        }

        let response = self
            .client
            .get(&self.api_url("getUpdates"))
            .query(&params)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Telegram request failed: {}", e)))?;

        let telegram_response: TelegramResponse<Vec<Update>> = response
            .json()
            .await
            .map_err(|e| Error::Channel(format!("Failed to parse Telegram response: {}", e)))?;

        if !telegram_response.ok {
            return Err(Error::Channel(
                telegram_response
                    .description
                    .unwrap_or_else(|| "Unknown error".to_string()),
            ));
        }

        Ok(telegram_response.result.unwrap_or_default())
    }

    pub async fn run_loop(self: Arc<Self>, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        if !self.config.channels.telegram.enabled {
            info!("Telegram channel disabled");
            return;
        }

        if self.config.channels.telegram.token.is_empty() {
            warn!("Telegram token not configured");
            return;
        }

        info!("Telegram channel started");
        let mut offset: Option<i64> = None;

        loop {
            tokio::select! {
                result = self.get_updates(offset) => {
                    match result {
                        Ok(updates) => {
                            for update in updates {
                                offset = Some(update.update_id + 1);
                                
                                if let Some(message) = update.message {
                                    if let Err(e) = self.handle_message(message).await {
                                        error!(error = %e, "Failed to handle Telegram message");
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "Failed to get Telegram updates");
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                                _ = shutdown.recv() => {
                                    info!("Telegram channel shutting down");
                                    break;
                                }
                            }
                        }
                    }
                }
                _ = shutdown.recv() => {
                    info!("Telegram channel shutting down");
                    break;
                }
            }
        }
    }

    async fn download_file(&self, file_id: &str, filename: &str) -> Result<String> {
        // Get file path from Telegram
        let file_info_url = self.api_url("getFile");
        let response = self
            .client
            .get(&file_info_url)
            .query(&[("file_id", file_id)])
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Failed to get file info: {}", e)))?;

        let file_response: TelegramResponse<FileResponse> = response
            .json()
            .await
            .map_err(|e| Error::Channel(format!("Failed to parse file response: {}", e)))?;

        if !file_response.ok {
            return Err(Error::Channel(
                file_response
                    .description
                    .unwrap_or_else(|| "Failed to get file".to_string()),
            ));
        }

        let file_path = file_response
            .result
            .and_then(|r| r.file_path)
            .ok_or_else(|| Error::Channel("No file path in response".to_string()))?;

        // Download file
        let download_url = format!(
            "{}/file/bot{}/{}",
            TELEGRAM_API_BASE, self.config.channels.telegram.token, file_path
        );

        let file_data = self
            .client
            .get(&download_url)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Failed to download file: {}", e)))?
            .bytes()
            .await
            .map_err(|e| Error::Channel(format!("Failed to read file data: {}", e)))?;

        // Save to media directory
        let local_path = self.media_dir.join(filename);
        let mut file = tokio::fs::File::create(&local_path)
            .await
            .map_err(|e| Error::Channel(format!("Failed to create file: {}", e)))?;

        file.write_all(&file_data)
            .await
            .map_err(|e| Error::Channel(format!("Failed to write file: {}", e)))?;

        Ok(local_path.to_string_lossy().to_string())
    }

    async fn handle_message(&self, message: Message) -> Result<()> {
        let user = match &message.from {
            Some(u) => u,
            None => return Ok(()),
        };

        if !self.is_allowed(user) {
            debug!(user_id = user.id, "User not in allowlist, ignoring");
            return Ok(());
        }

        let mut content = message
            .text
            .or(message.caption)
            .unwrap_or_default();

        let mut media_files = vec![];

        // Handle photos
        if let Some(photos) = &message.photo {
            if let Some(largest) = photos.iter().max_by_key(|p| p.width * p.height) {
                let filename = format!("telegram_photo_{}_{}.jpg", message.message_id, largest.file_unique_id);
                match self.download_file(&largest.file_id, &filename).await {
                    Ok(path) => {
                        media_files.push(path);
                        debug!("Downloaded photo: {}", filename);
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to download photo");
                    }
                }
            }
        }

        // Handle voice messages — download then auto-transcribe
        if let Some(voice) = &message.voice {
            let filename = format!("telegram_voice_{}_{}.ogg", message.message_id, voice.file_unique_id);
            match self.download_file(&voice.file_id, &filename).await {
                Ok(path) => {
                    media_files.push(path.clone());
                    let transcript = self.transcribe_voice(&path).await;
                    match transcript {
                        Some(text) => {
                            info!(path = %path, "Voice transcribed");
                            if content.is_empty() {
                                content = text;
                            } else {
                                content = format!("{}\n[Voice transcript: {}]", content, text);
                            }
                        }
                        None => {
                            if content.is_empty() {
                                content = format!("[Voice message: {}]", path);
                            } else {
                                content = format!("{} [Voice: {}]", content, path);
                            }
                        }
                    }
                    debug!("Downloaded voice: {}", filename);
                }
                Err(e) => {
                    error!(error = %e, "Failed to download voice");
                }
            }
        }

        // Handle documents
        if let Some(doc) = &message.document {
            let filename = doc.file_name.clone().unwrap_or_else(|| {
                format!("telegram_doc_{}_{}", message.message_id, doc.file_unique_id)
            });
            match self.download_file(&doc.file_id, &filename).await {
                Ok(path) => {
                    media_files.push(path);
                    debug!("Downloaded document: {}", filename);
                }
                Err(e) => {
                    error!(error = %e, "Failed to download document");
                }
            }
        }

        // Skip if no content and no media
        if content.is_empty() && media_files.is_empty() {
            return Ok(());
        }

        let inbound = InboundMessage {
            channel: "telegram".to_string(),
            sender_id: user.id.to_string(),
            chat_id: message.chat.id.to_string(),
            content,
            media: media_files,
            metadata: serde_json::json!({
                "message_id": message.message_id,
                "username": user.username,
            }),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        };

        self.inbound_tx
            .send(inbound)
            .await
            .map_err(|e| Error::Channel(e.to_string()))?;

        Ok(())
    }

    /// Attempt to transcribe a voice file.
    /// Priority: local `whisper` CLI → OpenAI Whisper API → None (caller shows raw path).
    async fn transcribe_voice(&self, path: &str) -> Option<String> {
        // 1. Try local whisper CLI
        if let Ok(output) = tokio::process::Command::new("whisper")
            .args([path, "--model", "base", "--output_format", "txt", "--output_dir", "/tmp"])
            .output()
            .await
        {
            if output.status.success() {
                // whisper writes <filename>.txt in output_dir
                let txt_path = format!(
                    "/tmp/{}.txt",
                    std::path::Path::new(path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("voice")
                );
                if let Ok(text) = tokio::fs::read_to_string(&txt_path).await {
                    let trimmed = text.trim().to_string();
                    if !trimmed.is_empty() {
                        return Some(trimmed);
                    }
                }
            }
        }

        // 2. Try OpenAI Whisper API
        let api_key = self
            .config
            .providers
            .get("openai")
            .map(|p| p.api_key.clone())
            .filter(|k| !k.is_empty())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok().filter(|k| !k.is_empty()));

        let api_key = match api_key {
            Some(k) => k,
            None => {
                debug!("No OpenAI API key for voice transcription");
                return None;
            }
        };

        let file_bytes = match tokio::fs::read(path).await {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "Failed to read voice file for transcription");
                return None;
            }
        };

        let filename = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("voice.ogg")
            .to_string();

        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(filename)
            .mime_str("audio/ogg")
            .ok()?;

        let form = reqwest::multipart::Form::new()
            .text("model", "whisper-1")
            .part("file", part);

        match self
            .client
            .post("https://api.openai.com/v1/audio/transcriptions")
            .header("Authorization", format!("Bearer {}", api_key))
            .multipart(form)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                #[derive(serde::Deserialize)]
                struct TranscribeResp { text: String }
                if let Ok(body) = resp.json::<TranscribeResp>().await {
                    let trimmed = body.text.trim().to_string();
                    if !trimmed.is_empty() {
                        return Some(trimmed);
                    }
                }
                None
            }
            Ok(resp) => {
                warn!(status = %resp.status(), "OpenAI Whisper API error");
                None
            }
            Err(e) => {
                warn!(error = %e, "OpenAI Whisper API request failed");
                None
            }
        }
    }
}

/// Escape special characters for Telegram MarkdownV2 parse mode.
/// All characters outside code spans that have special meaning must be escaped.
pub fn escape_markdown_v2(text: &str) -> String {
    // Characters that must be escaped in MarkdownV2 outside code/pre blocks
    const SPECIAL: &[char] = &[
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
    ];
    let mut out = String::with_capacity(text.len() + 32);
    for ch in text.chars() {
        if SPECIAL.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

pub async fn send_message(config: &Config, chat_id: &str, text: &str) -> Result<()> {
    crate::rate_limit::telegram_limiter().acquire().await;
    let mut builder = Client::builder().timeout(Duration::from_secs(30));
    if let Some(proxy) = config.channels.telegram.proxy.as_deref() {
        if let Ok(p) = Proxy::all(proxy) {
            builder = builder.proxy(p);
        }
    }
    let client = builder.build().unwrap_or_else(|_| Client::new());
    let url = format!(
        "{}/bot{}/sendMessage",
        TELEGRAM_API_BASE, config.channels.telegram.token
    );

    let chunks = split_message(text, 4096);
    for (i, chunk) in chunks.iter().enumerate() {
        do_send_message(&client, &url, chat_id, chunk).await?;
        if i + 1 < chunks.len() {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    Ok(())
}

async fn do_send_message(client: &Client, url: &str, chat_id: &str, text: &str) -> Result<()> {
    #[derive(Serialize)]
    struct SendMessageRequest {
        chat_id: String,
        text: String,
        parse_mode: String,
    }

    // Try MarkdownV2 first; fall back to plain text if Telegram rejects the formatting.
    let escaped = escape_markdown_v2(text);
    let request = SendMessageRequest {
        chat_id: chat_id.to_string(),
        text: escaped,
        parse_mode: "MarkdownV2".to_string(),
    };

    let response = client
        .post(url)
        .json(&request)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("Failed to send Telegram message: {}", e)))?;

    if response.status().is_success() {
        return Ok(());
    }

    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    // 400 with "can't parse entities" → retry as plain text
    if status.as_u16() == 400 && body.contains("parse") {
        warn!("Telegram MarkdownV2 parse error, retrying as plain text");
        let plain_request = SendMessageRequest {
            chat_id: chat_id.to_string(),
            text: text.to_string(),
            parse_mode: String::new(),
        };
        // Send without parse_mode by using a plain JSON body
        let plain_body = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
        });
        let retry = client
            .post(url)
            .json(&plain_body)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("Failed to send Telegram plain message: {}", e)))?;
        if !retry.status().is_success() {
            let err = retry.text().await.unwrap_or_default();
            return Err(Error::Channel(format!("Telegram API error (plain): {}", err)));
        }
        let _ = plain_request; // suppress unused warning
        return Ok(());
    }

    Err(Error::Channel(format!("Telegram API error: {}", body)))
}

/// Split a message into chunks at newline boundaries, respecting a max length.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }

        // Find the best split point within the limit.
        let mut split_at = max_len;
        
        // Ensure split_at is a char boundary before we slice
        while split_at > 0 && !remaining.is_char_boundary(split_at) {
            split_at -= 1;
        }
        
        // If we somehow couldn't find a boundary (e.g., max_len too small), fallback to first char
        if split_at == 0 {
            split_at = remaining.chars().next().map(|c| c.len_utf8()).unwrap_or(max_len);
        }

        // Try to split on a newline within the safe boundary
        if let Some(last_newline) = remaining[..split_at].rfind('\n') {
            split_at = last_newline + 1; // Include the newline in the chunk
        }

        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_plain_text() {
        assert_eq!(escape_markdown_v2("hello world"), "hello world");
    }

    #[test]
    fn test_escape_special_chars() {
        let result = escape_markdown_v2("price: $1.99 (sale!)");
        assert!(result.contains("\\."));
        assert!(result.contains("\\!"));
        assert!(result.contains("\\("));
        assert!(result.contains("\\)"));
    }

    #[test]
    fn test_escape_markdown_symbols() {
        let result = escape_markdown_v2("**bold** _italic_");
        assert!(result.contains("\\*"));
        assert!(result.contains("\\_"));
    }

    #[test]
    fn test_split_message_short() {
        let chunks = split_message("hello world", 4096);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello world");
    }

    #[test]
    fn test_split_message_long_with_newlines() {
        let line = "a".repeat(100);
        let text = (0..50).map(|_| line.clone()).collect::<Vec<_>>().join("\n");
        let chunks = split_message(&text, 4096);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.len() <= 4096);
            assert!(chunk.ends_with('\n') || chunk == chunks.last().unwrap());
        }
    }

    #[test]
    fn test_split_message_utf8_boundary() {
        // "你好" is 6 bytes (3 bytes per char).
        let text = "你好".repeat(1000); // 6000 bytes
        let chunks = split_message(&text, 4096);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].len() <= 4096);
        assert!(chunks[1].len() <= 4096);
        // Make sure it split at a valid char boundary (no panic, valid string)
        assert!(String::from_utf8(chunks[0].as_bytes().to_vec()).is_ok());
        assert!(String::from_utf8(chunks[1].as_bytes().to_vec()).is_ok());
        // 4096 / 3 = 1365 with remainder 1. So 1365 * 3 = 4095 is the closest boundary.
        assert_eq!(chunks[0].len(), 4095); 
    }
}
