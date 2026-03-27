//! Base64 data handler for embedded media data.
//!
//! This module handles base64-encoded data URLs that are embedded directly
//! in message segments, such as `base64://...` or `data:image/png;base64,...`.

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use blockcell_core::{Error, Result};
use std::time::Instant;
use tracing::{debug, info};

use super::downloader::{MediaDownloader, UnifiedDownloadRequest, UnifiedDownloadResult};
use super::types::{DownloadStrategy, UrlDetectionResult};

/// Base64 data downloader for embedded data URLs.
///
/// Handles URLs in the format:
/// - `base64://<data>`
/// - `data:<mediatype>;base64,<data>`
pub struct Base64DataDownloader;

impl Base64DataDownloader {
    /// Create a new Base64 data downloader.
    pub fn new() -> Self {
        Self
    }

    /// Decode base64 data from a URL.
    ///
    /// Supports both `base64://` and `data:` URL formats.
    pub fn decode_data_url(url: &str) -> Result<(Vec<u8>, Option<String>)> {
        // Handle data: URLs (e.g., data:image/png;base64,....)
        if url.starts_with("data:") {
            return Self::decode_data_uri(url);
        }

        // Handle base64:// URLs
        if url.starts_with("base64://") {
            let data = url.strip_prefix("base64://").unwrap();
            let decoded = STANDARD
                .decode(data)
                .map_err(|e| Error::Channel(format!("Base64 decode failed: {}", e)))?;
            return Ok((decoded, None));
        }

        Err(Error::Channel(format!("Not a base64 data URL: {}", url)))
    }

    /// Decode a data URI (RFC 2397).
    ///
    /// Format: `data:[<mediatype>][;base64],<data>`
    fn decode_data_uri(url: &str) -> Result<(Vec<u8>, Option<String>)> {
        // Remove "data:" prefix
        let url = url.strip_prefix("data:").unwrap_or(url);

        // Find the comma separating metadata from data
        let comma_pos = url
            .find(',')
            .ok_or_else(|| Error::Channel("Invalid data URI: missing comma".to_string()))?;

        let metadata = &url[..comma_pos];
        let data = &url[comma_pos + 1..];

        // Parse metadata
        let parts: Vec<&str> = metadata.split(';').collect();
        let media_type = parts.first().map(|s| s.to_string());

        // Check if base64 encoded
        let is_base64 = parts.contains(&"base64");

        if is_base64 {
            let decoded = STANDARD
                .decode(data)
                .map_err(|e| Error::Channel(format!("Base64 decode failed: {}", e)))?;
            Ok((decoded, media_type))
        } else {
            // Simple URL percent-decoding for non-base64 data
            // Only handle common cases (no complex percent encoding)
            let decoded = Self::simple_url_decode(data);
            Ok((decoded, media_type))
        }
    }

    /// Simple URL percent-decoding.
    fn simple_url_decode(s: &str) -> Vec<u8> {
        let mut result = Vec::with_capacity(s.len());
        let mut chars = s.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '%' {
                // Try to parse the next two characters as hex
                let hex: String = chars.by_ref().take(2).collect();
                if hex.len() == 2 {
                    if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                        result.push(byte);
                        continue;
                    }
                }
                // If parsing failed, just push the original characters
                result.push(b'%');
                for c in hex.chars() {
                    result.push(c as u8);
                }
            } else {
                result.push(c as u8);
            }
        }

        result
    }

    /// Suggest file extension from media type.
    fn suggest_extension(mime_type: Option<&str>) -> Option<&'static str> {
        match mime_type {
            Some(mime) => match mime.to_lowercase().as_str() {
                "image/png" => Some("png"),
                "image/jpeg" | "image/jpg" => Some("jpg"),
                "image/gif" => Some("gif"),
                "image/webp" => Some("webp"),
                "image/svg+xml" => Some("svg"),
                "audio/mpeg" | "audio/mp3" => Some("mp3"),
                "audio/wav" | "audio/wave" => Some("wav"),
                "audio/ogg" => Some("ogg"),
                "audio/aac" => Some("aac"),
                "video/mp4" => Some("mp4"),
                "video/webm" => Some("webm"),
                "video/quicktime" => Some("mov"),
                _ => None,
            },
            None => None,
        }
    }
}

impl Default for Base64DataDownloader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MediaDownloader for Base64DataDownloader {
    fn can_handle(&self, url: &str, detection: &UrlDetectionResult) -> bool {
        detection.strategy == DownloadStrategy::DecodeBase64
            && (url.starts_with("base64://") || url.starts_with("data:"))
    }

    async fn download(&self, request: UnifiedDownloadRequest) -> Result<UnifiedDownloadResult> {
        let start_time = Instant::now();
        let url = &request.source;

        debug!(url_prefix = &url[..50.min(url.len())], "Processing base64 data URL");

        // Decode the base64 data
        let (data, mime_type) = Self::decode_data_url(url)?;

        info!(
            size = data.len(),
            mime_type = ?mime_type,
            "Base64 data decoded"
        );

        // Determine filename - handle empty string case
        let filename = request.filename.clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                let ext = Self::suggest_extension(mime_type.as_deref())
                    .unwrap_or("bin");
                format!("media_{}.{}", chrono::Utc::now().format("%Y%m%d_%H%M%S"), ext)
            });

        // Save to local file
        let local_path = super::napcat_stream::save_to_local(
            &data,
            &filename,
            &request.workspace,
            &request.chat_id,
            &request.config.media_download_dir,
        )
        .await?;

        let duration_ms = start_time.elapsed().as_millis() as u64;

        Ok(UnifiedDownloadResult {
            local_path,
            source_url: url.clone(),
            actual_url: url.clone(),
            media_type: request.media_type,
            filename,
            size: data.len(),
            strategy_used: DownloadStrategy::DecodeBase64,
            duration_ms,
        })
    }

    fn name(&self) -> &'static str {
        "base64_data"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_base64_url() {
        let url = "base64://SGVsbG8gV29ybGQ=";
        let (data, mime_type) = Base64DataDownloader::decode_data_url(url).unwrap();
        assert_eq!(data, b"Hello World");
        assert!(mime_type.is_none());
    }

    #[test]
    fn test_decode_data_uri() {
        let url = "data:text/plain;base64,SGVsbG8gV29ybGQ=";
        let (data, mime_type) = Base64DataDownloader::decode_data_url(url).unwrap();
        assert_eq!(data, b"Hello World");
        assert_eq!(mime_type, Some("text/plain".to_string()));
    }

    #[test]
    fn test_decode_data_uri_image() {
        // Minimal PNG header in base64
        let url = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let (data, mime_type) = Base64DataDownloader::decode_data_url(url).unwrap();
        assert!(!data.is_empty());
        assert_eq!(mime_type, Some("image/png".to_string()));
    }

    #[test]
    fn test_suggest_extension() {
        assert_eq!(Base64DataDownloader::suggest_extension(Some("image/png")), Some("png"));
        assert_eq!(Base64DataDownloader::suggest_extension(Some("image/jpeg")), Some("jpg"));
        assert_eq!(Base64DataDownloader::suggest_extension(Some("audio/mpeg")), Some("mp3"));
        assert_eq!(Base64DataDownloader::suggest_extension(Some("video/mp4")), Some("mp4"));
        assert_eq!(Base64DataDownloader::suggest_extension(Some("application/unknown")), None);
        assert_eq!(Base64DataDownloader::suggest_extension(None), None);
    }

    #[test]
    fn test_can_handle() {
        let downloader = Base64DataDownloader::new();

        let detection = UrlDetectionResult::simple(
            super::super::types::UrlType::Base64Data,
            DownloadStrategy::DecodeBase64,
        );

        assert!(downloader.can_handle("base64://data", &detection));
        assert!(downloader.can_handle("data:image/png;base64,abc", &detection));
        assert!(!downloader.can_handle("http://example.com/file.jpg", &detection));
    }
}