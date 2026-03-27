//! Local file handler for file:// URLs.
//!
//! This module handles local file paths in message segments,
//! supporting both `file:///...` URLs and direct file paths.

use async_trait::async_trait;
use blockcell_core::{Error, Result};
use std::path::Path;
use std::time::Instant;
use tracing::{debug, info};

use super::downloader::{MediaDownloader, UnifiedDownloadRequest, UnifiedDownloadResult};
use super::types::{DownloadStrategy, UrlDetectionResult};

/// Local file downloader for file:// URLs.
///
/// Handles URLs in the format:
/// - `file:///absolute/path/to/file`
/// - `file://relative/path/to/file`
pub struct LocalFileDownloader;

impl LocalFileDownloader {
    /// Create a new local file downloader.
    pub fn new() -> Self {
        Self
    }

    /// Extract file path from file:// URL.
    ///
    /// Handles both Windows and Unix paths.
    pub fn extract_path(url: &str) -> Result<String> {
        // file:///path/to/file -> /path/to/file
        if let Some(path) = url.strip_prefix("file:///") {
            Ok(format!("/{}", path))
        // file://path/to/file -> /path/to/file (preserves the second /)
        } else if let Some(path) = url.strip_prefix("file://") {
            Ok(format!("/{}", path))
        // file:path/to/file -> path/to/file
        } else if let Some(path) = url.strip_prefix("file:") {
            Ok(path.to_string())
        } else {
            Err(Error::Channel(format!("Not a file URL: {}", url)))
        }
    }

    /// Copy file to destination.
    async fn copy_file(src: &Path, dest: &Path) -> Result<u64> {
        use tokio::fs;

        // Ensure parent directory exists
        if let Some(parent) = dest.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .await
                    .map_err(|e| Error::Tool(format!("Failed to create directory: {}", e)))?;
            }
        }

        // Copy file
        fs::copy(src, dest)
            .await
            .map_err(|e| Error::Tool(format!("Failed to copy file: {}", e)))
    }
}

impl Default for LocalFileDownloader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MediaDownloader for LocalFileDownloader {
    fn can_handle(&self, url: &str, detection: &UrlDetectionResult) -> bool {
        detection.strategy == DownloadStrategy::ReadLocalFile
            && (url.starts_with("file://") || url.starts_with("file:"))
    }

    async fn download(&self, request: UnifiedDownloadRequest) -> Result<UnifiedDownloadResult> {
        let start_time = Instant::now();
        let url = &request.source;

        debug!(url = %url, "Processing local file URL");

        // Extract file path
        let file_path = Self::extract_path(url)?;

        // Check if file exists
        let src_path = Path::new(&file_path);
        if !src_path.exists() {
            return Err(Error::Channel(format!("File not found: {}", file_path)));
        }

        // Get file metadata
        let metadata = tokio::fs::metadata(src_path)
            .await
            .map_err(|e| Error::Tool(format!("Failed to read file metadata: {}", e)))?;

        let _size = metadata.len() as usize;

        // Determine filename - handle empty string case
        let filename = request.filename.clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                src_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string()
            });

        // Build destination path
        let workspace_dir = expand_workspace_path(&request.workspace);
        let date_str = chrono::Local::now().format("%Y-%m-%d").to_string();
        let id_part = request
            .chat_id
            .strip_prefix("user:")
            .or_else(|| request.chat_id.strip_prefix("group:"))
            .unwrap_or(&request.chat_id);
        let subdir_name = format!("{}_{}", date_str, id_part);

        let dest_dir = Path::new(&workspace_dir)
            .join(&request.config.media_download_dir)
            .join(&subdir_name);

        let dest_path = dest_dir.join(&filename);

        // Copy file to destination
        let copied = Self::copy_file(src_path, &dest_path).await?;

        info!(
            src = %file_path,
            dest = %dest_path.display(),
            size = copied,
            "Local file copied to workspace"
        );

        let duration_ms = start_time.elapsed().as_millis() as u64;

        Ok(UnifiedDownloadResult {
            local_path: dest_path.to_string_lossy().to_string(),
            source_url: url.clone(),
            actual_url: file_path.clone(),
            media_type: request.media_type,
            filename,
            size: copied as usize,
            strategy_used: DownloadStrategy::ReadLocalFile,
            duration_ms,
        })
    }

    fn name(&self) -> &'static str {
        "local_file"
    }
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

/// Check if a path is a valid local file.
pub fn is_valid_local_file(path: &str) -> bool {
    let path = if path.starts_with("file://") {
        &path[6..]
    } else {
        path
    };

    Path::new(path).exists()
}

/// Get file size if the path points to a valid file.
pub fn get_file_size(path: &str) -> Option<u64> {
    let path = if path.starts_with("file://") {
        &path[6..]
    } else {
        path
    };

    std::fs::metadata(path).ok().map(|m| m.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_path() {
        assert_eq!(
            LocalFileDownloader::extract_path("file:///tmp/test.txt").unwrap(),
            "/tmp/test.txt"
        );
        // file://relative/path.txt -> /relative/path.txt (the leading slash is from file:// parsing)
        assert_eq!(
            LocalFileDownloader::extract_path("file://relative/path.txt").unwrap(),
            "/relative/path.txt"
        );
        assert_eq!(
            LocalFileDownloader::extract_path("file:test.txt").unwrap(),
            "test.txt"
        );
        assert!(LocalFileDownloader::extract_path("http://example.com").is_err());
    }

    #[test]
    fn test_detect_media_type() {
        // Use MediaType::from_extension instead of the removed detect_media_type
        use super::super::types::MediaType;
        assert_eq!(MediaType::from_extension("jpg"), MediaType::Image);
        assert_eq!(MediaType::from_extension("mp3"), MediaType::Voice);
        assert_eq!(MediaType::from_extension("mp4"), MediaType::Video);
        assert_eq!(MediaType::from_extension("pdf"), MediaType::File);
    }

    #[test]
    fn test_can_handle() {
        let downloader = LocalFileDownloader::new();

        let detection = UrlDetectionResult::simple(
            super::super::types::UrlType::LocalPath,
            DownloadStrategy::ReadLocalFile,
        );

        assert!(downloader.can_handle("file:///tmp/test.txt", &detection));
        assert!(downloader.can_handle("file://relative/path.txt", &detection));
        assert!(!downloader.can_handle("http://example.com/file.txt", &detection));
    }
}