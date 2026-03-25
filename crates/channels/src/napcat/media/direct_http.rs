//! Direct HTTP downloader for external URLs.
//!
//! This module provides direct HTTP download capability using `reqwest`,
//! serving as a fallback when NapCat proxy is not available or for
//! external URLs that don't need NapCat proxying.

use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use super::downloader::{MediaDownloader, UnifiedDownloadRequest, UnifiedDownloadResult};
use super::types::{DownloadStrategy, UrlDetectionResult};

/// Direct HTTP downloader using reqwest.
///
/// Downloads files directly via HTTP without going through NapCat proxy.
/// This is useful for:
/// - External URLs that are directly accessible
/// - Fallback when NapCat proxy fails
/// - URLs that NapCat cannot handle
pub struct DirectHttpDownloader {
    client: Client,
}

impl DirectHttpDownloader {
    /// Create a new direct HTTP downloader.
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .user_agent(format!(
                "blockcell/{}",
                option_env!("CARGO_PKG_VERSION").unwrap_or("unknown")
            ))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self { client }
    }

    /// Create a new direct HTTP downloader with custom timeout.
    pub fn with_timeout(timeout_secs: u64) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .user_agent(format!(
                "blockcell/{}",
                option_env!("CARGO_PKG_VERSION").unwrap_or("unknown")
            ))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self { client }
    }
}

impl Default for DirectHttpDownloader {
    fn default() -> Self {
        Self::new()
    }
}

impl DirectHttpDownloader {
    /// Download data using the stored client.
    async fn download_with_client(&self, url: &str) -> Result<Vec<u8>> {
        debug!(url = %url, "Starting HTTP download with client");

        let response = self.client
            .get(url)
            .send()
            .await
            .map_err(|e| Error::Channel(format!("HTTP request failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            return Err(Error::Channel(format!(
                "HTTP download failed: status {}",
                status
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| Error::Channel(format!("Failed to read response body: {}", e)))?;

        info!(url = %url, size = bytes.len(), "HTTP download completed");
        Ok(bytes.to_vec())
    }
}

#[async_trait]
impl MediaDownloader for DirectHttpDownloader {
    fn can_handle(&self, url: &str, detection: &UrlDetectionResult) -> bool {
        // Can handle HTTP URLs with DirectHttp strategy
        // or as fallback for other strategies
        (detection.strategy == DownloadStrategy::DirectHttp
            || detection.strategy == DownloadStrategy::NapCatStream)
            && (url.starts_with("http://") || url.starts_with("https://"))
    }

    async fn download(&self, request: UnifiedDownloadRequest) -> Result<UnifiedDownloadResult> {
        let start_time = Instant::now();
        let url = &request.source;

        info!(
            url = %url,
            media_type = request.media_type.as_str(),
            "Starting direct HTTP download"
        );

        // Download via HTTP using the stored client
        let data = self.download_with_client(url).await?;

        // Determine filename - handle empty string case
        let filename = request.filename.clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                super::detector::extract_filename_from_url(url)
                    .unwrap_or_else(|| format!("media_{}", chrono::Utc::now().format("%Y%m%d_%H%M%S")))
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
            source_url: request.source.clone(),
            actual_url: url.clone(),
            media_type: request.media_type,
            filename,
            size: data.len(),
            strategy_used: DownloadStrategy::DirectHttp,
            duration_ms,
        })
    }

    fn name(&self) -> &'static str {
        "direct_http"
    }
}

/// Download data directly via HTTP.
///
/// A lower-level function that performs HTTP GET and returns raw bytes.
pub async fn download_via_http(url: &str) -> Result<Vec<u8>> {
    debug!(url = %url, "Starting HTTP download");

    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent(format!(
            "blockcell/{}",
            option_env!("CARGO_PKG_VERSION").unwrap_or("unknown")
        ))
        .build()
        .unwrap_or_else(|_| Client::new());

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::Channel(format!("HTTP request failed: {}", e)))?;

    let status = response.status();
    if !status.is_success() {
        return Err(Error::Channel(format!(
            "HTTP download failed: status {}",
            status
        )));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| Error::Channel(format!("Failed to read response: {}", e)))?;

    info!(url = %url, size = bytes.len(), "HTTP download completed");
    Ok(bytes.to_vec())
}

/// Download with retry support.
///
/// Retries the download up to `max_retries` times with exponential backoff.
pub async fn download_with_retry(
    url: &str,
    max_retries: u32,
    initial_delay_ms: u64,
) -> Result<Vec<u8>> {
    let mut last_error = None;
    let mut delay = initial_delay_ms;

    for attempt in 0..max_retries {
        match download_via_http(url).await {
            Ok(data) => {
                if attempt > 0 {
                    info!(
                        url = %url,
                        attempt = attempt + 1,
                        "Download succeeded after retry"
                    );
                }
                return Ok(data);
            }
            Err(e) => {
                warn!(
                    url = %url,
                    attempt = attempt + 1,
                    max_retries = max_retries,
                    error = %e,
                    "Download attempt failed"
                );
                last_error = Some(e);

                if attempt < max_retries - 1 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    delay = (delay * 2).min(5000); // Cap at 5 seconds
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        Error::Channel(format!("All {} download attempts failed", max_retries))
    }))
}

/// Check if a URL is reachable via HTTP HEAD request.
pub async fn is_url_reachable(url: &str) -> bool {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| Client::new());

    match client.head(url).send().await {
        Ok(response) => response.status().is_success() || response.status().as_u16() == 302,
        Err(_) => false,
    }
}

/// Get content length from HTTP HEAD request.
pub async fn get_content_length(url: &str) -> Option<u64> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| Client::new());

    match client.head(url).send().await {
        Ok(response) => response.headers().get("content-length").and_then(|v| {
            v.to_str()
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
        }),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_direct_http_downloader_new() {
        let downloader = DirectHttpDownloader::new();
        assert_eq!(downloader.name(), "direct_http");
    }

    #[test]
    fn test_direct_http_downloader_can_handle() {
        let downloader = DirectHttpDownloader::new();

        // Can handle HTTP URLs with DirectHttp strategy
        let detection = UrlDetectionResult::simple(
            super::super::types::UrlType::ExternalHttp,
            DownloadStrategy::DirectHttp,
        );
        assert!(downloader.can_handle("http://example.com/file.jpg", &detection));
        assert!(downloader.can_handle("https://example.com/file.jpg", &detection));

        // Cannot handle non-HTTP URLs
        assert!(!downloader.can_handle("base64://data", &detection));
        assert!(!downloader.can_handle("file:///tmp/file.jpg", &detection));

        // Can also handle NapCat stream strategy as fallback
        let detection = UrlDetectionResult::simple(
            super::super::types::UrlType::NapCatProxy,
            DownloadStrategy::NapCatStream,
        );
        assert!(downloader.can_handle("http://example.com/file.jpg", &detection));
    }

    #[tokio::test]
    async fn test_is_url_reachable() {
        // This test makes actual network requests
        // In CI, this might fail due to network restrictions
        let result = is_url_reachable("https://www.google.com").await;
        // Just verify the function doesn't panic
        let _ = result;
    }
}