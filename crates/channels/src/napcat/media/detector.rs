//! URL type detector for media download.
//!
//! This module provides URL detection functionality to classify URLs
//! and determine the appropriate download strategy.

use blockcell_core::config::NapCatConfig;
use tracing::debug;

use super::types::{
    DownloadStrategy, NapCatMediaApi, UrlDetectionResult, UrlType,
};

/// QQ image CDN domain patterns.
const QQ_IMAGE_CDN_PATTERNS: &[&str] = &[
    "gchat.qpic.cn",
    "multimedia.nt.qq.com.cn",
    "c2cpicdw.qpic.cn",
    "gxh.photo.qq.com",
];

/// QQ file CDN domain patterns.
const QQ_FILE_CDN_PATTERNS: &[&str] = &[
    "tjc-download.ftn.qq.com",
    "ftn.qq.com",
    "download.ftn.qq.com",
];

/// Detect URL type and suggest download strategy.
///
/// This function analyzes a URL string and determines:
/// 1. The type of URL (QQ CDN, NapCat proxy, external, etc.)
/// 2. The recommended download strategy
/// 3. Whether NapCat API call is needed first
///
/// # Arguments
///
/// * `url` - The URL string to analyze
/// * `config` - NapCat configuration for checking proxy settings
///
/// # Returns
///
/// A `UrlDetectionResult` containing the detected type and suggested strategy.
pub fn detect_url_type(url: &str, config: &NapCatConfig) -> UrlDetectionResult {
    // 1. Check for Base64 data
    if url.starts_with("base64://") {
        debug!(url_prefix = &url[..20.min(url.len())], "Detected Base64 data URL");
        return UrlDetectionResult::simple(UrlType::Base64Data, DownloadStrategy::DecodeBase64);
    }

    // 2. Check for local file path
    if url.starts_with("file:///") || url.starts_with("file://") {
        debug!(url_prefix = &url[..20.min(url.len())], "Detected local file URL");
        return UrlDetectionResult::simple(UrlType::LocalPath, DownloadStrategy::ReadLocalFile);
    }

    // 3. Check for data URI (data:image/png;base64,...)
    if url.starts_with("data:") {
        debug!(url_prefix = &url[..20.min(url.len())], "Detected data URI");
        return UrlDetectionResult::simple(UrlType::Base64Data, DownloadStrategy::DecodeBase64);
    }

    // 4. Extract domain for further analysis
    let domain = extract_domain(url);

    // 5. Check if this is a NapCat proxy URL
    if is_napcat_proxy_url(url, config) {
        debug!(url = %url, domain = %domain, "Detected NapCat proxy URL");
        return UrlDetectionResult::simple(UrlType::NapCatProxy, DownloadStrategy::NapCatStream);
    }

    // 6. Check for QQ image CDN
    if is_qq_image_cdn(&domain) {
        debug!(url = %url, domain = %domain, "Detected QQ image CDN URL");
        // IMPORTANT: QQ image CDN URLs CANNOT use NapCat stream download!
        // They must use direct HTTP download instead.
        // The NapCat get_image API might return a proxied URL, but often fails
        // for QQ official URLs. Use DirectHttp as the primary strategy.
        return UrlDetectionResult::simple(UrlType::QqImageCdn, DownloadStrategy::DirectHttp);
    }

    // 7. Check for QQ file CDN
    if is_qq_file_cdn(&domain) {
        debug!(url = %url, domain = %domain, "Detected QQ file CDN URL");
        // IMPORTANT: QQ file CDN URLs CANNOT use NapCat stream download!
        // They require direct HTTP download. NapCat's download_file_stream API
        // is designed for NapCat-proxied URLs, not QQ official URLs.
        return UrlDetectionResult::simple(UrlType::QqFileCdn, DownloadStrategy::DirectHttp);
    }

    // 8. Check for HTTP/HTTPS URL
    if url.starts_with("http://") || url.starts_with("https://") {
        debug!(url = %url, domain = %domain, "Detected external HTTP URL");
        return UrlDetectionResult::simple(UrlType::ExternalHttp, DownloadStrategy::DirectHttp);
    }

    // 9. Unknown type - might be a file ID
    debug!(url = %url, "Unknown URL type, treating as potential file ID");
    UrlDetectionResult::with_api(
        UrlType::Unknown,
        DownloadStrategy::NapCatStream,
        NapCatMediaApi::GetFile,
    )
}

/// Extract domain from URL.
///
/// Returns the domain portion of a URL, or an empty string if parsing fails.
pub fn extract_domain(url: &str) -> String {
    // Remove protocol prefix
    let url_without_protocol = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    // Find the first '/' to separate domain from path
    let end_pos = url_without_protocol.find('/').unwrap_or(url_without_protocol.len());

    // Extract domain (might include port)
    let domain_with_port = &url_without_protocol[..end_pos];

    // Remove port if present
    if let Some(colon_pos) = domain_with_port.find(':') {
        domain_with_port[..colon_pos].to_string()
    } else {
        domain_with_port.to_string()
    }
}

/// Check if URL is a NapCat proxy URL.
///
/// A URL is considered a NapCat proxy URL if:
/// It contains common NapCat path patterns
fn is_napcat_proxy_url(url: &str, _config: &NapCatConfig) -> bool {
    // Check for common NapCat path patterns
    let napcat_patterns = ["/files/", "/download/", "/file/"];
    for pattern in napcat_patterns {
        if url.contains(pattern) {
            // Additional check: should be a URL (starts with http)
            if url.starts_with("http://") || url.starts_with("https://") {
                return true;
            }
        }
    }

    false
}

/// Check if domain matches QQ image CDN patterns.
fn is_qq_image_cdn(domain: &str) -> bool {
    QQ_IMAGE_CDN_PATTERNS.iter().any(|pattern| domain.contains(pattern))
}

/// Check if domain matches QQ file CDN patterns.
fn is_qq_file_cdn(domain: &str) -> bool {
    QQ_FILE_CDN_PATTERNS.iter().any(|pattern| domain.contains(pattern))
}

/// Check if URL looks like a valid download URL.
pub fn is_valid_download_url(url: &str) -> bool {
    // Empty or whitespace-only URLs are invalid
    if url.trim().is_empty() {
        return false;
    }

    // Base64 data
    if url.starts_with("base64://") || url.starts_with("data:") {
        return true;
    }

    // Local file
    if url.starts_with("file://") {
        return true;
    }

    // HTTP/HTTPS URLs
    if url.starts_with("http://") || url.starts_with("https://") {
        return true;
    }

    // File IDs (alphanumeric with possible underscores and dashes)
    !url.contains(' ') && url.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/')
}

/// Extract filename from URL.
///
/// Attempts to extract a meaningful filename from a URL.
/// Returns None if no filename can be determined.
pub fn extract_filename_from_url(url: &str) -> Option<String> {
    // Handle base64:// URLs
    if url.starts_with("base64://") {
        return None;
    }

    // Handle file:// URLs
    if url.starts_with("file://") {
        let path = url.strip_prefix("file://").unwrap_or(url);
        return path.rsplit('/').next().map(|s| s.to_string());
    }

    // Remove query parameters
    let url_without_query = url.split('?').next().unwrap_or(url);

    // Get the last path segment
    url_without_query
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty() && s.contains('.'))
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> NapCatConfig {
        NapCatConfig {
            ..Default::default()
        }
    }

    #[test]
    fn test_detect_base64_url() {
        let config = test_config();
        let result = detect_url_type("base64://aGVsbG8gd29ybGQ=", &config);
        assert_eq!(result.url_type, UrlType::Base64Data);
        assert_eq!(result.strategy, DownloadStrategy::DecodeBase64);
    }

    #[test]
    fn test_detect_local_file_url() {
        let config = test_config();
        let result = detect_url_type("file:///tmp/test.jpg", &config);
        assert_eq!(result.url_type, UrlType::LocalPath);
        assert_eq!(result.strategy, DownloadStrategy::ReadLocalFile);
    }

    #[test]
    fn test_detect_data_uri() {
        let config = test_config();
        let result = detect_url_type("data:image/png;base64,iVBORw0KGgo=", &config);
        assert_eq!(result.url_type, UrlType::Base64Data);
        assert_eq!(result.strategy, DownloadStrategy::DecodeBase64);
    }

    #[test]
    fn test_detect_qq_image_cdn() {
        let config = test_config();
        let result = detect_url_type("https://gchat.qpic.cn/gchatpic_new/123456/0", &config);
        assert_eq!(result.url_type, UrlType::QqImageCdn);
        // QQ image CDN URLs must use DirectHttp, not NapCat stream!
        assert_eq!(result.strategy, DownloadStrategy::DirectHttp);
        assert!(!result.needs_napcat_api);
    }

    #[test]
    fn test_detect_qq_file_cdn() {
        let config = test_config();
        let result = detect_url_type("https://tjc-download.ftn.qq.com/file", &config);
        assert_eq!(result.url_type, UrlType::QqFileCdn);
        // QQ file CDN URLs must use DirectHttp, not NapCat stream!
        assert_eq!(result.strategy, DownloadStrategy::DirectHttp);
    }

    #[test]
    fn test_detect_napcat_proxy_url() {
        let config = test_config();
        let result = detect_url_type("http://127.0.0.1:3000/files/abc123", &config);
        assert_eq!(result.url_type, UrlType::NapCatProxy);
        assert_eq!(result.strategy, DownloadStrategy::NapCatStream);
    }

    #[test]
    fn test_detect_external_url() {
        let config = test_config();
        let result = detect_url_type("https://example.com/image.jpg", &config);
        assert_eq!(result.url_type, UrlType::ExternalHttp);
        assert_eq!(result.strategy, DownloadStrategy::DirectHttp);
    }

    #[test]
    fn test_extract_domain() {
        assert_eq!(extract_domain("https://example.com/path"), "example.com");
        assert_eq!(extract_domain("http://localhost:3000/api"), "localhost");
        assert_eq!(extract_domain("https://gchat.qpic.cn:443/path"), "gchat.qpic.cn");
    }

    #[test]
    fn test_extract_filename_from_url() {
        assert_eq!(
            extract_filename_from_url("https://example.com/path/to/file.jpg?param=value"),
            Some("file.jpg".to_string())
        );
        assert_eq!(
            extract_filename_from_url("https://example.com/image.png"),
            Some("image.png".to_string())
        );
        assert_eq!(extract_filename_from_url("base64://data"), None);
        assert_eq!(extract_filename_from_url("file:///tmp/test.pdf"), Some("test.pdf".to_string()));
    }

    #[test]
    fn test_is_valid_download_url() {
        assert!(is_valid_download_url("https://example.com/file.jpg"));
        assert!(is_valid_download_url("base64://data"));
        assert!(is_valid_download_url("file:///tmp/file.txt"));
        assert!(is_valid_download_url("file_id_123"));
        assert!(!is_valid_download_url(""));
        assert!(!is_valid_download_url("   "));
    }

    #[test]
    fn test_is_qq_image_cdn() {
        assert!(is_qq_image_cdn("gchat.qpic.cn"));
        assert!(is_qq_image_cdn("multimedia.nt.qq.com.cn"));
        assert!(is_qq_image_cdn("sub.gchat.qpic.cn"));
        assert!(!is_qq_image_cdn("example.com"));
    }

    #[test]
    fn test_is_qq_file_cdn() {
        assert!(is_qq_file_cdn("tjc-download.ftn.qq.com"));
        assert!(is_qq_file_cdn("ftn.qq.com"));
        assert!(!is_qq_file_cdn("example.com"));
    }
}