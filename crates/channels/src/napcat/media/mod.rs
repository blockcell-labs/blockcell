//! Unified media download architecture for NapCatQQ.
//!
//! This module provides a unified interface for downloading media files
//! from various sources, including:
//!
//! - NapCat proxy URLs
//! - QQ official CDN URLs
//! - External HTTP URLs
//! - Base64 encoded data
//! - Local file paths
//!
//! # Architecture
//!
//! ```text
//!                        ┌─────────────────────────────────────┐
//!                        │   download_media_unified()          │
//!                        │   (Unified entry point)             │
//!                        └─────────────────────────────────────┘
//!                                          │
//!                        ┌─────────────────────────────────────┐
//!                        │   detect_url_type()                 │
//!                        │   (URL type detection)              │
//!                        └─────────────────────────────────────┘
//!                                          │
//!             ┌────────────────────────────┼────────────────────────────┐
//!             │                            │                            │
//!             ▼                            ▼                            ▼
//!   ┌───────────────────┐      ┌───────────────────┐      ┌───────────────────┐
//!   │ NapCatStream      │      │ DirectHttp        │      │ Base64/Local      │
//!   │ Downloader        │      │ Downloader        │      │ Handlers          │
//!   └───────────────────┘      └───────────────────┘      └───────────────────┘
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use blockcell_channels::napcat::media::{download_media_unified, MediaType};
//!
//! let result = download_media_unified(
//!     "https://example.com/image.jpg",
//!     MediaType::Image,
//!     &config,
//!     "/workspace",
//!     "user:123",
//! ).await?;
//!
//! println!("Downloaded to: {}", result.local_path);
//! ```

pub mod base64_data;
pub mod detector;
pub mod direct_http;
pub mod downloader;
pub mod local_file;
pub mod napcat_stream;
pub mod types;

// Re-export main types
pub use base64_data::Base64DataDownloader;
pub use detector::{
    detect_url_type, extract_domain, extract_filename_from_url, is_valid_download_url,
};
pub use direct_http::{download_via_http, DirectHttpDownloader};
pub use downloader::{
    download_raw_data, DownloaderManager, MediaDownloader, UnifiedDownloadRequest,
    UnifiedDownloadResult,
};
pub use local_file::{get_file_size, is_valid_local_file, LocalFileDownloader};
pub use napcat_stream::{download_via_napcat_stream, save_to_local, NapCatStreamDownloader};
pub use types::{
    DownloadConfig, DownloadStrategy, FileType, MediaType, NapCatMediaApi, UrlDetectionResult, UrlType,
};

use blockcell_core::config::NapCatConfig;
use blockcell_core::Result;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::napcat::message::MessageSegment;
use crate::napcat::types::{ApiRequest, ApiResponse};
use crate::napcat::websocket::{call_api_via_ws, is_ws_api_available};

/// Download result containing local path and metadata.
#[derive(Debug, Clone)]
pub struct DownloadedMedia {
    /// Local file path.
    pub local_path: String,
    /// Original URL.
    pub url: String,
    /// Media type: "image", "voice", "video", "file".
    pub media_type: String,
    /// Original filename (if available).
    pub filename: Option<String>,
    /// File size in bytes.
    pub size: usize,
}

impl From<UnifiedDownloadResult> for DownloadedMedia {
    fn from(result: UnifiedDownloadResult) -> Self {
        Self {
            local_path: result.local_path,
            url: result.source_url,
            media_type: result.media_type.as_str().to_string(),
            filename: Some(result.filename),
            size: result.size,
        }
    }
}

/// Process message segments and auto-download media if configured.
///
/// This function iterates through message segments, downloads media files,
/// and returns the download results.
pub async fn process_media_segments(
    config: &NapCatConfig,
    segments: &[MessageSegment],
    chat_id: &str,
    workspace: &str,
) -> Result<Vec<DownloadedMedia>> {
    if !config.auto_download_media {
        return Ok(vec![]);
    }

    info!(
        segments_count = segments.len(),
        auto_download = config.auto_download_media,
        max_size = config.max_auto_download_size,
        "Processing media segments for auto-download"
    );

    let mut downloaded = Vec::new();

    for segment in segments.iter() {
        let (url, media_type, filename, size) = match segment {
            MessageSegment::Image { file, file_size, .. } => {
                let request = ApiRequest::get_image(Some(file), None, None);
                match get_media_url_from_api(config, request).await {
                    Some((url, api_filename)) => (url, MediaType::Image, api_filename, file_size.unwrap_or(0) as usize),
                    None => {
                        warn!(file = %file, "Failed to get image URL from API, skipping");
                        continue;
                    }
                }
            }
            MessageSegment::Record { file, file_size, .. } => {
                // For get_record, out_format is required. Use "mp3" as default.
                let request = ApiRequest::get_record(Some(file), None, "mp3", None);
                match get_media_url_from_api(config, request).await {
                    Some((url, api_filename)) => (url, MediaType::Voice, api_filename, file_size.unwrap_or(0) as usize),
                    None => {
                        warn!(file = %file, "Failed to get record URL from API, skipping");
                        continue;
                    }
                }
            }
            MessageSegment::Video { file, file_size, .. } => {
                let request = ApiRequest::get_video(file, None);
                match get_media_url_from_api(config, request).await {
                    Some((url, api_filename)) => (url, MediaType::Video, api_filename, file_size.unwrap_or(0) as usize),
                    None => {
                        warn!(file = %file, "Failed to get video URL from API, skipping");
                        continue;
                    }
                }
            }
            MessageSegment::File { file, file_id, url: msg_url, name, size, .. } => {
                // Determine if this is a private chat
                let is_private = chat_id.starts_with("user:");
                let user_id = if is_private {
                    chat_id.strip_prefix("user:")
                } else {
                    None
                };

                // Get the actual download URL
                let file_url = if is_private {
                    // Private chat: need to call get_private_file_url API first
                    // Use file_id if available, fallback to file field
                    let actual_file_id = file_id.as_ref().unwrap_or(file);

                    match user_id {
                        Some(uid) => {
                            match get_private_file_url_from_api(config, actual_file_id, uid).await {
                                Some(url) => url,
                                None => {
                                    warn!(
                                        file_id = %actual_file_id,
                                        user_id = %uid,
                                        "Failed to get private file URL from API, skipping"
                                    );
                                    continue;
                                }
                            }
                        }
                        None => {
                            warn!(
                                file = %file,
                                chat_id = %chat_id,
                                "Invalid private chat ID format, skipping"
                            );
                            continue;
                        }
                    }
                } else {
                    // Group chat or other: use the URL directly
                    let url = msg_url.clone().unwrap_or_else(|| file.clone());
                    if url.is_empty() {
                        warn!(file = %file, "File message has no URL, skipping");
                        continue;
                    }
                    url
                };

                // Use original filename: prefer name field, fallback to file field
                let original_filename = name
                    .clone()
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| file.clone());

                match download_media_with_filename(
                    &file_url,
                    MediaType::File,
                    &original_filename,
                    config,
                    workspace,
                    chat_id,
                )
                .await
                {
                    Ok(result) => {
                        downloaded.push(DownloadedMedia {
                            local_path: result.local_path,
                            url: file_url,
                            media_type: "file".to_string(),
                            filename: Some(result.filename), // Use the actual saved filename
                            size: size.unwrap_or(0) as usize,
                        });
                    }
                    Err(e) => {
                        warn!(url = %file_url, error = %e, "Failed to auto-download file");
                    }
                }
                continue;
            }
            _ => continue,
        };

        // Check file size limit
        if size > config.max_auto_download_size as usize {
            warn!(
                url = %url,
                size = size,
                max_size = config.max_auto_download_size,
                "Skipping auto-download: file too large"
            );
            continue;
        }

        // Download using unified interface
        match download_media_with_filename(
            &url,
            media_type,
            filename.as_deref().unwrap_or(""),
            config,
            workspace,
            chat_id,
        )
        .await
        {
            Ok(result) => {
                downloaded.push(DownloadedMedia {
                    local_path: result.local_path,
                    url,
                    media_type: media_type.as_str().to_string(),
                    filename,
                    size,
                });
            }
            Err(e) => {
                warn!(url = %url, error = %e, "Failed to auto-download media");
            }
        }
    }

    Ok(downloaded)
}

/// Call NapCat API to get the correct media URL.
/// Returns (url, filename) where filename is the original filename if available.
async fn get_media_url_from_api(_config: &NapCatConfig, request: ApiRequest) -> Option<(String, Option<String>)> {
    let action = request.action.clone();

    let response: std::result::Result<ApiResponse, String> = if is_ws_api_available() {
        call_api_via_ws(request).await
    } else {
        warn!(action = %action, "WebSocket API not available for getting media URL");
        return None;
    };

    match response {
        Ok(resp) if resp.is_success() => {
            let url = resp.data.get("url").and_then(|v| v.as_str()).map(|s| s.to_string());
            // Try to get filename from file_name field (NapCat specific)
            let filename = resp.data.get("file_name").and_then(|v| v.as_str()).map(|s| s.to_string());
            url.map(|u| (u, filename))
        }
        Ok(resp) => {
            warn!(
                action = %action,
                status = %resp.status,
                error = %resp.error_message(),
                "API call failed when getting media URL"
            );
            None
        }
        Err(e) => {
            warn!(
                action = %action,
                error = %e,
                "Failed to call API for media URL"
            );
            None
        }
    }
}

/// Get private file URL from NapCat API.
///
/// 私聊文件需要先调用 get_private_file_url API 获取实际下载 URL。
async fn get_private_file_url_from_api(
    config: &NapCatConfig,
    file_id: &str,
    user_id: &str,
) -> Option<String> {
    let request = ApiRequest::get_private_file_url(file_id, Some(user_id), None);
    get_media_url_from_api(config, request).await.map(|(url, _)| url)
}

/// Build enhanced message content with downloaded media info.
pub fn build_enhanced_content(
    original_content: &str,
    downloaded: &[DownloadedMedia],
    _chat_id: &str,
) -> String {
    if downloaded.is_empty() {
        return original_content.to_string();
    }

    let mut content = if original_content.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", original_content)
    };

    // Group by media type
    let mut images: Vec<&DownloadedMedia> = Vec::new();
    let mut voices: Vec<&DownloadedMedia> = Vec::new();
    let mut videos: Vec<&DownloadedMedia> = Vec::new();
    let mut files: Vec<&DownloadedMedia> = Vec::new();

    for media in downloaded {
        match media.media_type.as_str() {
            "image" => images.push(media),
            "voice" => voices.push(media),
            "video" => videos.push(media),
            "file" => files.push(media),
            _ => {}
        }
    }

    content.push_str("已下载的媒体文件：\n\n");

    if !images.is_empty() {
        content.push_str(&format!("图片 ({} 个)：\n", images.len()));
        for img in &images {
            content.push_str(&format!("- {}\n", img.local_path));
        }
        content.push('\n');
    }

    if !voices.is_empty() {
        content.push_str(&format!("语音消息 ({} 个)：\n", voices.len()));
        for voice in &voices {
            content.push_str(&format!("- {}\n", voice.local_path));
        }
        content.push('\n');
    }

    if !videos.is_empty() {
        content.push_str(&format!("视频 ({} 个)：\n", videos.len()));
        for video in &videos {
            content.push_str(&format!("- {}\n", video.local_path));
        }
        content.push('\n');
    }

    if !files.is_empty() {
        content.push_str(&format!("文件 ({} 个)：\n", files.len()));
        for file in &files {
            let name = file.filename.as_deref().unwrap_or("未知文件");
            // Detect and report file type
            let file_type = FileType::from_filename(name);
            let type_desc = file_type.description();
            content.push_str(&format!("- {} [{}] (路径: {})\n", name, type_desc, file.local_path));
        }
        content.push('\n');
    }

    content.push_str("---\n");

    // 强调：除非用户显式要求，否则不要发送媒体回群聊
    content.push_str("\n【重要提示 - 媒体文件处理规则】\n");
    content.push_str("以上文件是用户刚刚发送到聊天中的媒体，已在聊天中展示。\n");
    content.push_str("⚠️ 除非用户明确要求你「发送」「转发」「传给某人」这些文件，否则：\n");
    content.push_str("  - 不要使用任何 napcat 发送工具（如 send_image、upload_file 等）\n");
    content.push_str("  - 不要将文件路径放入任何发送消息的工具参数中\n");
    content.push_str("  - 只需用文字回复用户即可，不要重复发送已有媒体\n");
    content.push_str("用户如果需要你处理这些文件（如分析、识别、转写等），请使用相应的工具处理。\n");

    content
}

/// Build metadata with downloaded media info.
pub fn build_media_metadata(downloaded: &[DownloadedMedia]) -> Value {
    let paths: Vec<String> = downloaded.iter().map(|m| m.local_path.clone()).collect();

    json!({
        "downloaded_media": downloaded.iter().map(|m| {
            let name = m.filename.as_deref().unwrap_or("未知文件");
            let file_type = FileType::from_filename(name);
            json!({
                "local_path": m.local_path,
                "url": m.url,
                "type": m.media_type,
                "file_type": file_type.description(),
                "filename": m.filename,
                "size": m.size
            })
        }).collect::<Vec<_>>(),
        "media_paths": paths,
    })
}

/// Unified media download function.
///
/// This is the main entry point for downloading media files.
/// It automatically detects the URL type and selects the appropriate download strategy.
///
/// # Arguments
///
/// * `source` - The URL or file identifier to download
/// * `media_type` - The type of media (Image, Voice, Video, File)
/// * `config` - NapCat configuration
/// * `workspace` - Workspace directory for saving files
/// * `chat_id` - Chat ID for organizing downloads
///
/// # Returns
///
/// A `UnifiedDownloadResult` containing the local path and metadata.
pub async fn download_media_unified(
    source: &str,
    media_type: MediaType,
    config: &NapCatConfig,
    workspace: &str,
    chat_id: &str,
) -> Result<UnifiedDownloadResult> {
    let request = UnifiedDownloadRequest::new(source, media_type, config.clone(), workspace, chat_id);

    let manager = DownloaderManager::new(config);
    manager.download(request).await
}

/// Download media with filename hint.
///
/// Similar to `download_media_unified` but allows specifying a filename.
pub async fn download_media_with_filename(
    source: &str,
    media_type: MediaType,
    filename: &str,
    config: &NapCatConfig,
    workspace: &str,
    chat_id: &str,
) -> Result<UnifiedDownloadResult> {
    let request = UnifiedDownloadRequest::new(source, media_type, config.clone(), workspace, chat_id)
        .with_filename(filename);

    let manager = DownloaderManager::new(config);
    manager.download(request).await
}

/// Check if auto-download is enabled and file size is within limit.
pub fn should_auto_download(config: &NapCatConfig, size: usize) -> bool {
    if !config.auto_download_media {
        return false;
    }

    size <= config.max_auto_download_size as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_auto_download() {
        let config = NapCatConfig {
            auto_download_media: true,
            max_auto_download_size: 1024 * 1024, // 1MB
            ..Default::default()
        };

        assert!(should_auto_download(&config, 100));
        assert!(should_auto_download(&config, 1024 * 1024));
        assert!(!should_auto_download(&config, 1024 * 1024 + 1));
    }

    #[test]
    fn test_should_auto_download_disabled() {
        let config = NapCatConfig {
            auto_download_media: false,
            max_auto_download_size: 1024 * 1024,
            ..Default::default()
        };

        assert!(!should_auto_download(&config, 100));
    }
}