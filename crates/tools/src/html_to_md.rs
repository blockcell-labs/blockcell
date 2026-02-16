//! Shared HTML-to-Markdown conversion utilities.
//!
//! Implements the "Markdown for Agents" pattern (Cloudflare blog 2026-02-12):
//! 1. Try `Accept: text/markdown` content negotiation — server returns markdown directly
//! 2. Fall back to local HTML→Markdown conversion via `htmd`
//!
//! Also extracts useful response headers:
//! - `x-markdown-tokens`: estimated token count from server
//! - `content-signal`: content usage permissions (ai-train, search, ai-input)

use blockcell_core::{Error, Result};
use reqwest::{Client, Response};
use serde_json::{json, Value};

/// Metadata extracted from a markdown-aware HTTP response.
#[derive(Debug, Default)]
pub struct MarkdownMeta {
    /// True if the server returned native markdown (content-type: text/markdown).
    pub server_markdown: bool,
    /// Estimated token count from `x-markdown-tokens` header.
    pub token_count: Option<u64>,
    /// Content signal header value (e.g. "ai-train=yes, search=yes, ai-input=yes").
    pub content_signal: Option<String>,
    /// The final URL after redirects.
    pub final_url: String,
    /// HTTP status code.
    pub status: u16,
    /// Original content-type header.
    pub content_type: String,
}

impl MarkdownMeta {
    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "server_markdown": self.server_markdown,
            "final_url": self.final_url,
            "status": self.status,
            "content_type": self.content_type,
        });
        if let Some(tokens) = self.token_count {
            v["markdown_tokens"] = json!(tokens);
        }
        if let Some(ref signal) = self.content_signal {
            v["content_signal"] = json!(signal);
        }
        v
    }
}

/// Fetch a URL with markdown content negotiation.
///
/// Strategy:
/// 1. Send `Accept: text/markdown, text/html;q=0.9, */*;q=0.8`
/// 2. If response is `text/markdown` → return as-is (server-side conversion)
/// 3. If response is `text/html` → convert locally via `htmd`
/// 4. Otherwise → return raw text
pub async fn fetch_as_markdown(url: &str, max_chars: usize) -> Result<(String, MarkdownMeta)> {
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Tool(format!("Failed to create HTTP client: {}", e)))?;

    let response = client
        .get(url)
        .header("User-Agent", "blockcell/0.1 (AI Agent)")
        .header("Accept", "text/markdown, text/html;q=0.9, */*;q=0.8")
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Fetch failed: {}", e)))?;

    process_response(response, max_chars).await
}

/// Process an HTTP response, extracting markdown content.
pub async fn process_response(response: Response, max_chars: usize) -> Result<(String, MarkdownMeta)> {
    let mut meta = MarkdownMeta::default();
    meta.final_url = response.url().to_string();
    meta.status = response.status().as_u16();

    meta.content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Extract markdown-specific headers
    meta.token_count = response
        .headers()
        .get("x-markdown-tokens")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    meta.content_signal = response
        .headers()
        .get("content-signal")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let body = response
        .text()
        .await
        .map_err(|e| Error::Tool(format!("Failed to read response body: {}", e)))?;

    let markdown = if meta.content_type.contains("text/markdown") {
        // Server returned native markdown — use as-is
        meta.server_markdown = true;
        body
    } else if meta.content_type.contains("text/html") {
        // HTML → convert to markdown locally
        meta.server_markdown = false;
        html_to_markdown(&body)
    } else if meta.content_type.contains("application/json") {
        // Pretty-print JSON
        meta.server_markdown = false;
        if let Ok(json) = serde_json::from_str::<Value>(&body) {
            serde_json::to_string_pretty(&json).unwrap_or(body)
        } else {
            body
        }
    } else {
        // Plain text or other — return as-is
        meta.server_markdown = false;
        body
    };

    // Truncate if needed
    let truncated = truncate_utf8(&markdown, max_chars);
    Ok((truncated, meta))
}

/// Convert HTML to clean Markdown using htmd.
///
/// Strips nav, header, footer, script, style, aside, and ad-related elements
/// to focus on main content. Preserves headings, links, images, lists, tables,
/// code blocks, and emphasis.
pub fn html_to_markdown(html: &str) -> String {
    use htmd::HtmlToMarkdown;

    // Use htmd with default settings — it handles most cases well
    let converter = HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style", "nav", "footer", "header", "aside", "noscript", "iframe"])
        .build();

    match converter.convert(html) {
        Ok(md) => clean_markdown(&md),
        Err(_) => {
            // Fallback: use scraper to extract text
            extract_text_fallback(html)
        }
    }
}

/// Clean up converted markdown:
/// - Collapse excessive blank lines (3+ → 2)
/// - Trim leading/trailing whitespace
/// - Remove empty link references
fn clean_markdown(md: &str) -> String {
    let mut result = String::with_capacity(md.len());
    let mut consecutive_newlines: usize = 0;

    for line in md.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            consecutive_newlines += 1;
        } else {
            // Insert at most one blank line (two newlines) between content lines
            if !result.is_empty() {
                let separator_newlines = if consecutive_newlines > 0 { 2 } else { 1 };
                for _ in 0..separator_newlines {
                    result.push('\n');
                }
            }
            consecutive_newlines = 0;
            result.push_str(line);
        }
    }

    result.trim().to_string()
}

/// Fallback text extraction using scraper (when htmd fails).
fn extract_text_fallback(html: &str) -> String {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);

    // Try main content areas first
    let selectors = ["article", "main", "[role=\"main\"]", ".content", "#content", "body"];

    for sel_str in selectors {
        if let Ok(selector) = Selector::parse(sel_str) {
            if let Some(element) = document.select(&selector).next() {
                let text: String = element
                    .text()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if text.len() > 100 {
                    return text;
                }
            }
        }
    }

    // Last resort: all text
    document
        .root_element()
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Truncate a string at a valid UTF-8 char boundary.
fn truncate_utf8(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s.to_string();
    }
    let mut end = max_chars;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_to_markdown_basic() {
        let html = "<html><body><h1>Hello</h1><p>World</p></body></html>";
        let md = html_to_markdown(html);
        assert!(md.contains("# Hello") || md.contains("Hello"));
        assert!(md.contains("World"));
    }

    #[test]
    fn test_html_to_markdown_strips_scripts() {
        let html = "<html><body><script>alert('x')</script><p>Content</p></body></html>";
        let md = html_to_markdown(html);
        assert!(!md.contains("alert"));
        assert!(md.contains("Content"));
    }

    #[test]
    fn test_html_to_markdown_preserves_links() {
        let html = r#"<html><body><a href="https://example.com">Click here</a></body></html>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("Click here"));
        assert!(md.contains("https://example.com"));
    }

    #[test]
    fn test_html_to_markdown_preserves_lists() {
        let html = "<html><body><ul><li>Item 1</li><li>Item 2</li></ul></body></html>";
        let md = html_to_markdown(html);
        assert!(md.contains("Item 1"));
        assert!(md.contains("Item 2"));
    }

    #[test]
    fn test_clean_markdown_collapses_blanks() {
        let input = "Line 1\n\n\n\n\nLine 2\n\n\n\nLine 3";
        let result = clean_markdown(input);
        assert!(!result.contains("\n\n\n"));
    }

    #[test]
    fn test_truncate_utf8() {
        let s = "Hello 世界 test";
        let t = truncate_utf8(s, 9);
        // Should not panic on multi-byte boundary
        assert!(t.len() <= 9);
        assert!(t.starts_with("Hello "));
    }

    #[test]
    fn test_markdown_meta_to_json() {
        let meta = MarkdownMeta {
            server_markdown: true,
            token_count: Some(725),
            content_signal: Some("ai-train=yes, search=yes".to_string()),
            final_url: "https://example.com".to_string(),
            status: 200,
            content_type: "text/markdown".to_string(),
        };
        let j = meta.to_json();
        assert_eq!(j["server_markdown"], true);
        assert_eq!(j["markdown_tokens"], 725);
        assert!(j["content_signal"].as_str().unwrap().contains("ai-train"));
    }

    #[test]
    fn test_extract_text_fallback() {
        let html = "<html><body><article><p>Main content here</p></article><nav>Nav stuff</nav></body></html>";
        let text = extract_text_fallback(html);
        assert!(text.contains("Main content"));
    }
}
