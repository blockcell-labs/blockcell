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

        // Handle voice messages
        if let Some(voice) = &message.voice {
            let filename = format!("telegram_voice_{}_{}.ogg", message.message_id, voice.file_unique_id);
            match self.download_file(&voice.file_id, &filename).await {
                Ok(path) => {
                    media_files.push(path.clone());
                    // TODO: Optional voice transcription with Groq Whisper
                    // For now, just note that it's a voice message
                    if content.is_empty() {
                        content = format!("[Voice message: {}]", path);
                    } else {
                        content = format!("{} [Voice: {}]", content, path);
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
}

pub async fn send_message(config: &Config, chat_id: &str, text: &str) -> Result<()> {
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

    #[derive(Serialize)]
    struct SendMessageRequest<'a> {
        chat_id: &'a str,
        text: &'a str,
        parse_mode: &'a str,
    }

    let request = SendMessageRequest {
        chat_id,
        text,
        parse_mode: "Markdown",
    };

    let response = client
        .post(&url)
        .json(&request)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("Failed to send Telegram message: {}", e)))?;

    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Channel(format!("Telegram API error: {}", text)));
    }

    Ok(())
}
