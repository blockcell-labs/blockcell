//! Downloader trait and manager for media download.
//!
//! This module defines the `MediaDownloader` trait and `DownloaderManager`
//! that coordinates multiple download strategies.

use async_trait::async_trait;
use blockcell_core::config::NapCatConfig;
use blockcell_core::{Error, Result};
use std::time::Instant;
use tracing::{debug, warn};

use super::detector::detect_url_type;
use super::types::{
    DownloadConfig, DownloadStrategy, MediaType, STRATEGY_FALLBACK_ORDER,
    UrlDetectionResult, UrlType,
};

/// Unified download request.
#[derive(Debug, Clone)]
pub struct UnifiedDownloadRequest {
    /// Original URL or file identifier.
    pub source: String,

    /// Media type.
    pub media_type: MediaType,

    /// Optional filename.
    pub filename: Option<String>,

    /// NapCat configuration.
    pub config: NapCatConfig,

    /// Workspace directory.
    pub workspace: String,

    /// Chat ID for organizing downloads.
    pub chat_id: String,

    /// Optional segment data for additional info.
    pub segment_data: Option<serde_json::Value>,
}

impl UnifiedDownloadRequest {
    /// Create a new download request.
    pub fn new(
        source: impl Into<String>,
        media_type: MediaType,
        config: NapCatConfig,
        workspace: impl Into<String>,
        chat_id: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            media_type,
            filename: None,
            config,
            workspace: workspace.into(),
            chat_id: chat_id.into(),
            segment_data: None,
        }
    }

    /// Set the filename.
    pub fn with_filename(mut self, filename: impl Into<String>) -> Self {
        self.filename = Some(filename.into());
        self
    }

    /// Set the segment data.
    pub fn with_segment_data(mut self, data: serde_json::Value) -> Self {
        self.segment_data = Some(data);
        self
    }
}

/// Unified download result.
#[derive(Debug, Clone)]
pub struct UnifiedDownloadResult {
    /// Local file path.
    pub local_path: String,

    /// Original URL.
    pub source_url: String,

    /// Actual download URL (may differ from original).
    pub actual_url: String,

    /// Media type.
    pub media_type: MediaType,

    /// Filename.
    pub filename: String,

    /// File size in bytes.
    pub size: usize,

    /// Strategy used for download.
    pub strategy_used: DownloadStrategy,

    /// Download duration in milliseconds.
    pub duration_ms: u64,
}

/// Media downloader trait.
///
/// Implement this trait to provide a download strategy for specific URL types.
#[async_trait]
pub trait MediaDownloader: Send + Sync {
    /// Check if this downloader can handle the given URL and detection result.
    fn can_handle(&self, url: &str, detection: &UrlDetectionResult) -> bool;

    /// Execute the download.
    async fn download(&self, request: UnifiedDownloadRequest) -> Result<UnifiedDownloadResult>;

    /// Get the name of this downloader for logging.
    fn name(&self) -> &'static str;
}

/// Downloader manager that coordinates multiple download strategies.
pub struct DownloaderManager {
    downloaders: Vec<Box<dyn MediaDownloader>>,
    download_config: DownloadConfig,
}

impl DownloaderManager {
    /// Create a new downloader manager with the given configuration.
    pub fn new(config: &NapCatConfig) -> Self {
        Self::with_download_config(config, DownloadConfig::default())
    }

    /// Create a new downloader manager with custom download configuration.
    pub fn with_download_config(config: &NapCatConfig, download_config: DownloadConfig) -> Self {
        // Import downloaders at runtime to avoid circular dependencies
        let downloaders: Vec<Box<dyn MediaDownloader>> = vec![
            Box::new(super::base64_data::Base64DataDownloader::new()),
            Box::new(super::local_file::LocalFileDownloader::new()),
            Box::new(super::napcat_stream::NapCatStreamDownloader::new(config.clone())),
            Box::new(super::direct_http::DirectHttpDownloader::new()),
        ];

        Self {
            downloaders,
            download_config,
        }
    }

    /// Download media using the unified interface.
    ///
    /// This method:
    /// 1. Detects the URL type
    /// 2. Finds an appropriate downloader
    /// 3. Executes the download with fallback strategies
    pub async fn download(&self, request: UnifiedDownloadRequest) -> Result<UnifiedDownloadResult> {
        let _start_time = Instant::now();

        // Detect URL type
        let detection = detect_url_type(&request.source, &request.config);

        // Build strategy list: primary + fallbacks
        let mut strategies = vec![detection.strategy];
        for strategy in STRATEGY_FALLBACK_ORDER {
            if !strategies.contains(strategy) {
                strategies.push(*strategy);
            }
        }

        let mut last_error = None;

        // Try each strategy
        for strategy in strategies {
            let fake_detection = UrlDetectionResult::simple(detection.url_type, strategy);

            // Find a downloader for this strategy
            for downloader in &self.downloaders {
                if downloader.can_handle(&request.source, &fake_detection) {
                    match downloader.download(request.clone()).await {
                        Ok(result) => {
                            return Ok(result);
                        }
                        Err(e) => {
                            warn!(
                                downloader = downloader.name(),
                                strategy = strategy.as_str(),
                                error = %e,
                                "Downloader failed, trying next"
                            );
                            last_error = Some(e);
                        }
                    }
                }
            }
        }

        // All strategies failed
        let error = last_error.unwrap_or_else(|| {
            Error::Channel("No suitable downloader found for URL".to_string())
        });

        Err(error)
    }

    /// Check if a URL can be downloaded.
    pub fn can_download(&self, url: &str, config: &NapCatConfig) -> bool {
        let detection = detect_url_type(url, config);

        // Check if any downloader can handle this URL
        self.downloaders
            .iter()
            .any(|d| d.can_handle(url, &detection))
    }

    /// Get the download configuration.
    pub fn download_config(&self) -> &DownloadConfig {
        &self.download_config
    }
}

/// Download raw data from URL.
///
/// This is a lower-level function that downloads data without saving to file.
/// Returns the raw bytes.
pub async fn download_raw_data(
    url: &str,
    config: &NapCatConfig,
) -> Result<Vec<u8>> {
    let detection = detect_url_type(url, config);

    // IMPORTANT: QQ CDN URLs MUST use DirectHttp, not NapCat stream!
    // NapCat's download_file_stream API only works with NapCat-proxied URLs.
    if matches!(detection.url_type, UrlType::QqImageCdn | UrlType::QqFileCdn) {
        debug!(url = %url, url_type = ?detection.url_type, "Using DirectHttp for QQ CDN URL");
        return super::direct_http::download_via_http(url).await;
    }

    // Try NapCat stream first for NapCat proxy URLs
    if detection.url_type == UrlType::NapCatProxy {
        if let Ok(data) = super::napcat_stream::download_via_napcat_stream(url, config).await {
            return Ok(data);
        }
    }

    // Try direct HTTP for external URLs or as fallback
    if detection.url_type == UrlType::ExternalHttp || detection.strategy == DownloadStrategy::DirectHttp {
        return super::direct_http::download_via_http(url).await;
    }

    // Last resort: try NapCat stream for unknown types (might be file IDs)
    if detection.url_type == UrlType::Unknown {
        if let Ok(data) = super::napcat_stream::download_via_napcat_stream(url, config).await {
            return Ok(data);
        }
    }

    Err(Error::Channel(format!(
        "Cannot download URL: {} (type: {})",
        url,
        detection.url_type.as_str()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> NapCatConfig {
        NapCatConfig::default()
    }

    #[test]
    fn test_unified_download_request_new() {
        let config = test_config();
        let request = UnifiedDownloadRequest::new(
            "https://example.com/image.jpg",
            MediaType::Image,
            config.clone(),
            "/workspace",
            "user:123",
        );

        assert_eq!(request.source, "https://example.com/image.jpg");
        assert_eq!(request.media_type, MediaType::Image);
        assert_eq!(request.workspace, "/workspace");
        assert_eq!(request.chat_id, "user:123");
        assert!(request.filename.is_none());
    }

    #[test]
    fn test_unified_download_request_with_filename() {
        let config = test_config();
        let request = UnifiedDownloadRequest::new(
            "https://example.com/image.jpg",
            MediaType::Image,
            config,
            "/workspace",
            "user:123",
        )
        .with_filename("photo.jpg");

        assert_eq!(request.filename, Some("photo.jpg".to_string()));
    }

    #[test]
    fn test_downloader_manager_new() {
        let config = test_config();
        let manager = DownloaderManager::new(&config);
        assert!(!manager.downloaders.is_empty());
    }

    #[test]
    fn test_download_config_default() {
        let config = DownloadConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.timeout_secs, 120);
    }
}