//! NapCat streaming downloader for media download.
//!
//! This module provides the NapCat stream-based download implementation
//! that supports WebSocket connection modes:
//! - ws-client: WebSocket client mode
//! - ws-server: WebSocket server mode

use async_trait::async_trait;
use blockcell_core::config::NapCatConfig;
use blockcell_core::{Error, Result};
use std::time::Instant;
use tracing::{debug, info};
use super::downloader::{MediaDownloader, UnifiedDownloadRequest, UnifiedDownloadResult};
use super::types::{DownloadStrategy, UrlDetectionResult};

/// NapCat streaming downloader.
///
/// Downloads media through NapCat's `download_file_stream` API,
/// which returns file content in Base64-encoded chunks.
pub struct NapCatStreamDownloader {
    config: NapCatConfig,
}

impl NapCatStreamDownloader {
    /// Create a new NapCat stream downloader.
    pub fn new(config: NapCatConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl MediaDownloader for NapCatStreamDownloader {
    fn can_handle(&self, url: &str, detection: &UrlDetectionResult) -> bool {
        matches!(
            detection.strategy,
            DownloadStrategy::NapCatStream | DownloadStrategy::NapCatRegular
        ) && !url.starts_with("base64://")
            && !url.starts_with("file://")
    }

    async fn download(&self, request: UnifiedDownloadRequest) -> Result<UnifiedDownloadResult> {
        let start_time = Instant::now();

        // Get the URL to download
        let url = &request.source;

        info!(
            url = %url,
            media_type = request.media_type.as_str(),
            "Starting NapCat stream download"
        );

        // Download via NapCat stream API
        let data = download_via_napcat_stream(url, &self.config).await?;

        // Determine filename - handle empty string case
        let filename = request.filename.clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                super::detector::extract_filename_from_url(url)
                    .unwrap_or_else(|| format!("media_{}", chrono::Utc::now().format("%Y%m%d_%H%M%S")))
            });

        // Save to local file
        let local_path = save_to_local(
            &data,
            &filename,
            &request.workspace,
            &request.chat_id,
            &self.config.media_download_dir,
        )
        .await?;

        let duration_ms = start_time.elapsed().as_millis() as u64;

        Ok(UnifiedDownloadResult {
            local_path,
            source_url: request.source.clone(),
            actual_url: url.clone(),
            media_type: request.media_type,
            filename,
            size: data.len(),
            strategy_used: DownloadStrategy::NapCatStream,
            duration_ms,
        })
    }

    fn name(&self) -> &'static str {
        "napcat_stream"
    }
}

/// Download file data via NapCat streaming API.
///
/// This function uses WebSocket streaming API for all modes.
pub async fn download_via_napcat_stream(url: &str, _config: &NapCatConfig) -> Result<Vec<u8>> {
    debug!(url = %url, "Starting NapCat stream download");

    download_via_ws_stream(url).await
}

/// Download via WebSocket streaming API.
async fn download_via_ws_stream(url: &str) -> Result<Vec<u8>> {
    use crate::napcat::types::ApiRequest;
    use crate::napcat::websocket::{call_stream_api_via_ws, is_ws_stream_available};

    if !is_ws_stream_available() {
        return Err(Error::Channel(
            "WebSocket stream not available".to_string(),
        ));
    }

    let request = ApiRequest::download_file_stream(url, Some(3), None, None);

    call_stream_api_via_ws(request)
        .await
        .map_err(|e| Error::Channel(format!("WebSocket stream download failed: {}", e)))
}

/// Save downloaded data to a local file.
///
/// Creates the necessary directory structure and writes the file.
/// Returns the absolute path to the saved file.
pub async fn save_to_local(
    data: &[u8],
    filename: &str,
    workspace: &str,
    chat_id: &str,
    media_download_dir: &str,
) -> Result<String> {
    use chrono::Local;
    use std::path::Path;
    use tokio::io::AsyncWriteExt;

    // Expand workspace path
    let workspace_dir = expand_workspace_path(workspace);

    // Build download directory: downloads/YYYY-MM-DD_USER_or_GROUP_id/
    let date_str = Local::now().format("%Y-%m-%d").to_string();

    // Keep the prefix (user: or group:) for directory naming
    let subdir_name = if chat_id.starts_with("group:") {
        // Group chat: 2026-03-23_GROUP_1083997779
        let id = chat_id.strip_prefix("group:").unwrap_or(chat_id);
        format!("{}_GROUP_{}", date_str, id)
    } else if chat_id.starts_with("user:") {
        // Private chat: 2026-03-23_USER_123456
        let id = chat_id.strip_prefix("user:").unwrap_or(chat_id);
        format!("{}_USER_{}", date_str, id)
    } else {
        // Fallback: use chat_id as-is
        format!("{}_{}", date_str, chat_id)
    };

    let downloads_dir = Path::new(&workspace_dir)
        .join(media_download_dir)
        .join(&subdir_name);

    // Create directory if needed
    if !downloads_dir.exists() {
        std::fs::create_dir_all(&downloads_dir)
            .map_err(|e| Error::Tool(format!("Failed to create downloads directory: {}", e)))?;
    }

    // Build file path
    let file_path = downloads_dir.join(filename);

    // Write file
    let mut file = tokio::fs::File::create(&file_path)
        .await
        .map_err(|e| Error::Tool(format!("Failed to create file: {}", e)))?;

    file.write_all(data)
        .await
        .map_err(|e| Error::Tool(format!("Failed to write file: {}", e)))?;

    info!(
        file_path = %file_path.display(),
        size = data.len(),
        "File saved successfully"
    );

    Ok(file_path.to_string_lossy().to_string())
}

/// Expand workspace path (~ to home directory).
fn expand_workspace_path(workspace: &str) -> String {
    if workspace.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            return workspace.replacen('~', home.to_str().unwrap_or(""), 1);
        }
    }
    workspace.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_workspace_path() {
        // Test with ~ expansion
        let expanded = expand_workspace_path("~/Downloads");
        assert!(!expanded.starts_with('~'));

        // Test without expansion
        let not_expanded = expand_workspace_path("/tmp/downloads");
        assert_eq!(not_expanded, "/tmp/downloads");
    }

    #[test]
    fn test_napcat_stream_downloader_can_handle() {
        let config = NapCatConfig::default();
        let downloader = NapCatStreamDownloader::new(config);

        // Can handle HTTP URLs with NapCat stream strategy
        let detection = UrlDetectionResult::simple(
            super::super::types::UrlType::NapCatProxy,
            DownloadStrategy::NapCatStream,
        );
        assert!(downloader.can_handle("http://example.com/file.jpg", &detection));

        // Cannot handle base64 URLs
        let detection = UrlDetectionResult::simple(
            super::super::types::UrlType::Base64Data,
            DownloadStrategy::DecodeBase64,
        );
        assert!(!downloader.can_handle("base64://data", &detection));

        // Cannot handle local file URLs
        let detection = UrlDetectionResult::simple(
            super::super::types::UrlType::LocalPath,
            DownloadStrategy::ReadLocalFile,
        );
        assert!(!downloader.can_handle("file:///tmp/file.jpg", &detection));
    }
}