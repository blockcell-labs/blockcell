//! Type definitions for unified media download architecture.
//!
//! This module defines the core types used in the media download system:
//! - `UrlType`: Classification of different URL types
//! - `MediaType`: Types of media content
//! - `DownloadStrategy`: Strategies for downloading media
//! - `UnifiedDownloadRequest/Result`: Request and response types

use serde::{Deserialize, Serialize};

/// URL type enumeration for classifying different URL sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlType {
    /// NapCat proxy URL (http://napcat-host:port/files/...)
    NapCatProxy,

    /// QQ official image CDN (gchat.qpic.cn, multimedia.nt.qq.com.cn)
    QqImageCdn,

    /// QQ official file CDN (tjc-download.ftn.qq.com)
    QqFileCdn,

    /// Regular external URL (can be downloaded directly via HTTP)
    ExternalHttp,

    /// Base64 embedded data (base64://...)
    Base64Data,

    /// Local file path (file:///...)
    LocalPath,

    /// Unrecognized URL type
    Unknown,
}

impl UrlType {
    /// Get a human-readable name for this URL type.
    pub fn as_str(&self) -> &'static str {
        match self {
            UrlType::NapCatProxy => "napcat_proxy",
            UrlType::QqImageCdn => "qq_image_cdn",
            UrlType::QqFileCdn => "qq_file_cdn",
            UrlType::ExternalHttp => "external_http",
            UrlType::Base64Data => "base64_data",
            UrlType::LocalPath => "local_path",
            UrlType::Unknown => "unknown",
        }
    }
}

/// NapCat media API types for determining which API to call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NapCatMediaApi {
    /// get_image API for retrieving image URLs
    GetImage,
    /// get_record API for retrieving voice/record URLs
    GetRecord,
    /// get_video API for retrieving video URLs
    GetVideo,
    /// get_file API for retrieving file info
    GetFile,
    /// download_file API for downloading files
    DownloadFile,
    /// download_file_stream API for streaming file downloads
    DownloadFileStream,
}

impl NapCatMediaApi {
    /// Get the API action name.
    pub fn as_action(&self) -> &'static str {
        match self {
            NapCatMediaApi::GetImage => "get_image",
            NapCatMediaApi::GetRecord => "get_record",
            NapCatMediaApi::GetVideo => "get_video",
            NapCatMediaApi::GetFile => "get_file",
            NapCatMediaApi::DownloadFile => "download_file",
            NapCatMediaApi::DownloadFileStream => "download_file_stream",
        }
    }
}

/// Download strategy enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStrategy {
    /// Use NapCat streaming download (download_file_stream API)
    NapCatStream,

    /// Use NapCat regular download (download_file API)
    NapCatRegular,

    /// Direct HTTP download using reqwest
    DirectHttp,

    /// Decode Base64 data
    DecodeBase64,

    /// Read local file
    ReadLocalFile,
}

impl DownloadStrategy {
    /// Get a human-readable name for this strategy.
    pub fn as_str(&self) -> &'static str {
        match self {
            DownloadStrategy::NapCatStream => "napcat_stream",
            DownloadStrategy::NapCatRegular => "napcat_regular",
            DownloadStrategy::DirectHttp => "direct_http",
            DownloadStrategy::DecodeBase64 => "decode_base64",
            DownloadStrategy::ReadLocalFile => "read_local_file",
        }
    }
}

/// Media type enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    /// Image (jpg, png, gif, etc.)
    Image,
    /// Voice/Audio record
    Voice,
    /// Video file
    Video,
    /// General file
    File,
}

impl MediaType {
    /// Get a human-readable name for this media type.
    pub fn as_str(&self) -> &'static str {
        match self {
            MediaType::Image => "image",
            MediaType::Voice => "voice",
            MediaType::Video => "video",
            MediaType::File => "file",
        }
    }

    /// Parse from string.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "image" | "img" | "picture" | "photo" => Some(MediaType::Image),
            "voice" | "audio" | "record" => Some(MediaType::Voice),
            "video" => Some(MediaType::Video),
            "file" => Some(MediaType::File),
            _ => None,
        }
    }

    /// Get common file extensions for this media type.
    pub fn common_extensions(&self) -> &'static [&'static str] {
        match self {
            MediaType::Image => &["jpg", "jpeg", "png", "gif", "webp", "bmp", "svg"],
            MediaType::Voice => &["mp3", "wav", "ogg", "amr", "flac", "aac", "m4a"],
            MediaType::Video => &["mp4", "mov", "avi", "mkv", "flv", "webm"],
            MediaType::File => &[],
        }
    }

    /// Guess media type from file extension.
    pub fn from_extension(ext: &str) -> Self {
        let ext_lower = ext.to_lowercase();
        if Self::Image.common_extensions().contains(&ext_lower.as_str()) {
            MediaType::Image
        } else if Self::Voice.common_extensions().contains(&ext_lower.as_str()) {
            MediaType::Voice
        } else if Self::Video.common_extensions().contains(&ext_lower.as_str()) {
            MediaType::Video
        } else {
            MediaType::File
        }
    }
}

/// Detailed file type for downloaded files.
/// This provides more specific type information than MediaType.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileType {
    /// PDF document
    Pdf,
    /// Word document (doc, docx)
    Word,
    /// Excel spreadsheet (xls, xlsx)
    Excel,
    /// PowerPoint presentation (ppt, pptx)
    PowerPoint,
    /// Text file (txt, md, log, etc.)
    Text,
    /// Archive file (zip, rar, 7z, tar, gz)
    Archive,
    /// Code file (rs, py, js, ts, java, etc.)
    Code,
    /// Image file
    Image,
    /// Audio file
    Audio,
    /// Video file
    Video,
    /// Unknown or other file type
    Other,
}

impl FileType {
    /// Get a human-readable description for this file type.
    pub fn description(&self) -> &'static str {
        match self {
            FileType::Pdf => "PDF文档",
            FileType::Word => "Word文档",
            FileType::Excel => "Excel表格",
            FileType::PowerPoint => "PowerPoint演示文稿",
            FileType::Text => "文本文件",
            FileType::Archive => "压缩文件",
            FileType::Code => "代码文件",
            FileType::Image => "图片",
            FileType::Audio => "音频文件",
            FileType::Video => "视频文件",
            FileType::Other => "其他文件",
        }
    }

    /// Detect file type from file extension.
    pub fn from_extension(ext: &str) -> Self {
        let ext_lower = ext.to_lowercase();

        // PDF
        if ext_lower == "pdf" {
            return FileType::Pdf;
        }

        // Word documents
        if ["doc", "docx", "rtf", "odt"].contains(&ext_lower.as_str()) {
            return FileType::Word;
        }

        // Excel spreadsheets
        if ["xls", "xlsx", "csv", "ods"].contains(&ext_lower.as_str()) {
            return FileType::Excel;
        }

        // PowerPoint presentations
        if ["ppt", "pptx", "odp"].contains(&ext_lower.as_str()) {
            return FileType::PowerPoint;
        }

        // Text files
        if ["txt", "md", "markdown", "log", "json", "yaml", "yml", "toml", "ini", "cfg", "conf"]
            .contains(&ext_lower.as_str())
        {
            return FileType::Text;
        }

        // Archive files
        if ["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "tgz"].contains(&ext_lower.as_str()) {
            return FileType::Archive;
        }

        // Code files
        if [
            "rs", "py", "js", "ts", "jsx", "tsx", "java", "c", "cpp", "h", "hpp", "cs", "go",
            "rb", "php", "swift", "kt", "scala", "lua", "sh", "bat", "ps1", "sql", "html", "css",
            "scss", "less", "vue", "svelte",
        ]
        .contains(&ext_lower.as_str())
        {
            return FileType::Code;
        }

        // Image files
        if ["jpg", "jpeg", "png", "gif", "webp", "bmp", "svg", "ico", "tiff", "tif", "heic", "heif"]
            .contains(&ext_lower.as_str())
        {
            return FileType::Image;
        }

        // Audio files
        if ["mp3", "wav", "ogg", "flac", "aac", "m4a", "wma", "opus", "amr"]
            .contains(&ext_lower.as_str())
        {
            return FileType::Audio;
        }

        // Video files
        if ["mp4", "mov", "avi", "mkv", "flv", "webm", "wmv", "m4v", "3gp", "ts"]
            .contains(&ext_lower.as_str())
        {
            return FileType::Video;
        }

        FileType::Other
    }

    /// Detect file type from filename.
    pub fn from_filename(filename: &str) -> Self {
        // Extract extension from filename
        if let Some(ext) = filename.rsplit('.').next() {
            Self::from_extension(ext)
        } else {
            FileType::Other
        }
    }
}

/// URL type detection result.
#[derive(Debug, Clone)]
pub struct UrlDetectionResult {
    /// Detected URL type.
    pub url_type: UrlType,

    /// Suggested download strategy.
    pub strategy: DownloadStrategy,

    /// Whether NapCat API call is needed first to get the actual URL.
    pub needs_napcat_api: bool,

    /// Which NapCat API to call if needed.
    pub napcat_api: Option<NapCatMediaApi>,
}

impl UrlDetectionResult {
    /// Create a simple detection result without API requirement.
    pub fn simple(url_type: UrlType, strategy: DownloadStrategy) -> Self {
        Self {
            url_type,
            strategy,
            needs_napcat_api: false,
            napcat_api: None,
        }
    }

    /// Create a detection result that requires NapCat API call.
    pub fn with_api(url_type: UrlType, strategy: DownloadStrategy, api: NapCatMediaApi) -> Self {
        Self {
            url_type,
            strategy,
            needs_napcat_api: true,
            napcat_api: Some(api),
        }
    }
}

/// Download configuration for retry and timeout settings.
#[derive(Debug, Clone)]
pub struct DownloadConfig {
    /// Maximum number of retry attempts.
    pub max_retries: u32,

    /// Initial retry delay in milliseconds.
    pub initial_retry_delay_ms: u64,

    /// Maximum retry delay in milliseconds.
    pub max_retry_delay_ms: u64,

    /// Download timeout in seconds.
    pub timeout_secs: u64,

    /// Maximum file size in bytes.
    pub max_file_size: usize,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_retry_delay_ms: 500,
            max_retry_delay_ms: 5000,
            timeout_secs: 120,
            max_file_size: 100 * 1024 * 1024, // 100MB
        }
    }
}

/// Strategy fallback order when the primary strategy fails.
pub const STRATEGY_FALLBACK_ORDER: &[DownloadStrategy] = &[
    DownloadStrategy::NapCatStream,
    DownloadStrategy::NapCatRegular,
    DownloadStrategy::DirectHttp,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_type_as_str() {
        assert_eq!(UrlType::NapCatProxy.as_str(), "napcat_proxy");
        assert_eq!(UrlType::QqImageCdn.as_str(), "qq_image_cdn");
        assert_eq!(UrlType::Unknown.as_str(), "unknown");
    }

    #[test]
    fn test_media_type_parse() {
        assert_eq!(MediaType::parse("image"), Some(MediaType::Image));
        assert_eq!(MediaType::parse("VOICE"), Some(MediaType::Voice));
        assert_eq!(MediaType::parse("unknown"), None);
    }

    #[test]
    fn test_media_type_from_extension() {
        assert_eq!(MediaType::from_extension("jpg"), MediaType::Image);
        assert_eq!(MediaType::from_extension("mp3"), MediaType::Voice);
        assert_eq!(MediaType::from_extension("mp4"), MediaType::Video);
        assert_eq!(MediaType::from_extension("pdf"), MediaType::File);
    }

    #[test]
    fn test_file_type_from_extension() {
        assert_eq!(FileType::from_extension("pdf"), FileType::Pdf);
        assert_eq!(FileType::from_extension("doc"), FileType::Word);
        assert_eq!(FileType::from_extension("docx"), FileType::Word);
        assert_eq!(FileType::from_extension("xls"), FileType::Excel);
        assert_eq!(FileType::from_extension("xlsx"), FileType::Excel);
        assert_eq!(FileType::from_extension("ppt"), FileType::PowerPoint);
        assert_eq!(FileType::from_extension("pptx"), FileType::PowerPoint);
        assert_eq!(FileType::from_extension("txt"), FileType::Text);
        assert_eq!(FileType::from_extension("md"), FileType::Text);
        assert_eq!(FileType::from_extension("zip"), FileType::Archive);
        assert_eq!(FileType::from_extension("rs"), FileType::Code);
        assert_eq!(FileType::from_extension("py"), FileType::Code);
        assert_eq!(FileType::from_extension("jpg"), FileType::Image);
        assert_eq!(FileType::from_extension("png"), FileType::Image);
        assert_eq!(FileType::from_extension("mp3"), FileType::Audio);
        assert_eq!(FileType::from_extension("mp4"), FileType::Video);
        assert_eq!(FileType::from_extension("unknown"), FileType::Other);
    }

    #[test]
    fn test_file_type_from_filename() {
        assert_eq!(FileType::from_filename("document.pdf"), FileType::Pdf);
        assert_eq!(FileType::from_filename("report.docx"), FileType::Word);
        assert_eq!(FileType::from_filename("KioCafe VQM QR Code Scanning Requirement_V20260302.pdf"), FileType::Pdf);
        assert_eq!(FileType::from_filename("archive.tar.gz"), FileType::Archive);
        assert_eq!(FileType::from_filename("no_extension"), FileType::Other);
    }

    #[test]
    fn test_file_type_description() {
        assert_eq!(FileType::Pdf.description(), "PDF文档");
        assert_eq!(FileType::Word.description(), "Word文档");
        assert_eq!(FileType::Image.description(), "图片");
        assert_eq!(FileType::Video.description(), "视频文件");
    }

    #[test]
    fn test_download_config_default() {
        let config = DownloadConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.timeout_secs, 120);
        assert_eq!(config.max_file_size, 100 * 1024 * 1024);
    }

    #[test]
    fn test_url_detection_result_simple() {
        let result = UrlDetectionResult::simple(UrlType::ExternalHttp, DownloadStrategy::DirectHttp);
        assert_eq!(result.url_type, UrlType::ExternalHttp);
        assert_eq!(result.strategy, DownloadStrategy::DirectHttp);
        assert!(!result.needs_napcat_api);
        assert!(result.napcat_api.is_none());
    }

    #[test]
    fn test_url_detection_result_with_api() {
        let result = UrlDetectionResult::with_api(
            UrlType::QqImageCdn,
            DownloadStrategy::NapCatStream,
            NapCatMediaApi::GetImage,
        );
        assert_eq!(result.url_type, UrlType::QqImageCdn);
        assert!(result.needs_napcat_api);
        assert_eq!(result.napcat_api, Some(NapCatMediaApi::GetImage));
    }
}