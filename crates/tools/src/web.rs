use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolSchema};

// ============ web_search ============

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_search",
            description: "Search the web. Uses Brave Search API if configured, otherwise falls back to Bing (free, no API key needed, works in China). Tip: set freshness=day for 'last 24 hours' news.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query"
                    },
                    "count": {
                        "type": "integer",
                        "description": "Number of results (1-10, default 5)"
                    },
                    "freshness": {
                        "type": "string",
                        "description": "Recency filter. Only applied when using Brave Search API.",
                        "enum": ["day", "week", "month", "year"]
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("query").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("Missing required parameter: query".to_string()));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let query = params["query"].as_str().unwrap();
        let count = params
            .get("count")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(10) as usize;

        let freshness = params
            .get("freshness")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let api_key = &ctx.config.tools.web.search.api_key;
        if !api_key.is_empty() {
            // Brave Search API (preferred when configured)
            match brave_search(api_key, query, count, freshness.as_deref()).await {
                Ok(results) => return Ok(json!({ "query": query, "results": results, "source": "brave" })),
                Err(e) => {
                    tracing::warn!(error = %e, "Brave search failed, falling back to Bing");
                }
            }
        }

        // Free fallback: Bing HTML scraping (accessible from China, no API key needed)
        match bing_search(query, count).await {
            Ok(results) => return Ok(json!({ "query": query, "results": results, "source": "bing" })),
            Err(e) => {
                tracing::warn!(error = %e, "Bing search failed");
                return Err(e);
            }
        }
    }
}

async fn brave_search(api_key: &str, query: &str, count: usize, freshness: Option<&str>) -> Result<Vec<Value>> {
    let client = Client::new();
    let mut req = client
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("X-Subscription-Token", api_key)
        .query(&[("q", query), ("count", &count.to_string())]);

    if let Some(f) = freshness {
        // Brave Search API supports freshness: day|week|month|year
        req = req.query(&[("freshness", f)]);
    }

    let response = req
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Search request failed: {}", e)))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Tool(format!("Search API error {}: {}", status, text)));
    }

    let data: Value = response
        .json()
        .await
        .map_err(|e| Error::Tool(format!("Failed to parse search response: {}", e)))?;

    let results: Vec<Value> = data["web"]["results"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|r| {
            json!({
                "title": r["title"],
                "url": r["url"],
                "snippet": r["description"]
            })
        })
        .collect();

    Ok(results)
}

async fn bing_search(query: &str, count: usize) -> Result<Vec<Value>> {
    use scraper::{Html, Selector};

    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| Error::Tool(format!("Failed to create HTTP client: {}", e)))?;

    let response = client
        .get("https://www.bing.com/search")
        .query(&[("q", query), ("count", &count.to_string())])
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Bing search failed: {}", e)))?;

    if !response.status().is_success() {
        return Err(Error::Tool(format!("Bing returned status {}", response.status())));
    }

    let html = response
        .text()
        .await
        .map_err(|e| Error::Tool(format!("Failed to read Bing response: {}", e)))?;

    let document = Html::parse_document(&html);

    // Bing organic results are in <li class="b_algo">
    let result_sel = Selector::parse("li.b_algo").unwrap();
    let title_sel = Selector::parse("h2 a").unwrap();
    let snippet_sel = Selector::parse(".b_caption p, .b_lineclamp2, .b_lineclamp3, .b_lineclamp4").unwrap();

    let mut results = Vec::new();

    for el in document.select(&result_sel) {
        if results.len() >= count {
            break;
        }

        let title_el = el.select(&title_sel).next();
        let title = title_el.map(|e| {
            e.text().collect::<Vec<_>>().join("").trim().to_string()
        }).unwrap_or_default();

        let url = title_el.and_then(|e| {
            e.value().attr("href").map(|h| h.to_string())
        }).unwrap_or_default();

        let snippet = el.select(&snippet_sel).next().map(|e| {
            e.text().collect::<Vec<_>>().join("").trim().to_string()
        }).unwrap_or_default();

        if title.is_empty() || url.is_empty() {
            continue;
        }

        results.push(json!({
            "title": title,
            "url": url,
            "snippet": snippet
        }));
    }

    if results.is_empty() {
        return Err(Error::Tool("Bing returned no parseable results. Try a different query.".to_string()));
    }

    Ok(results)
}

// ============ web_fetch ============

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_fetch",
            description: "Fetch a web page and return its content as clean Markdown. Uses 'Accept: text/markdown' content negotiation (Cloudflare Markdown for Agents) for optimal results — if the server supports it, markdown is returned directly with ~80% token savings. Otherwise, HTML is converted to markdown locally. Returns markdown_tokens estimate and content_signal when available.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to fetch (must be http or https)"
                    },
                    "extractMode": {
                        "type": "string",
                        "enum": ["markdown", "text", "raw"],
                        "description": "Content extraction mode. 'markdown' (default): returns clean markdown via content negotiation + local conversion. 'text': returns plain text only. 'raw': returns raw response body without conversion."
                    },
                    "maxChars": {
                        "type": "integer",
                        "description": "Maximum characters to return (default: 50000)"
                    }
                },
                "required": ["url"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: url".to_string()))?;

        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(Error::Validation(
                "URL must start with http:// or https://".to_string(),
            ));
        }

        Ok(())
    }

    async fn execute(&self, _ctx: ToolContext, params: Value) -> Result<Value> {
        let url = params["url"].as_str().unwrap();
        let extract_mode = params
            .get("extractMode")
            .and_then(|v| v.as_str())
            .unwrap_or("markdown");
        let max_chars = params
            .get("maxChars")
            .and_then(|v| v.as_u64())
            .unwrap_or(50000) as usize;

        match extract_mode {
            "raw" => fetch_raw(url, max_chars).await,
            "text" => fetch_text(url, max_chars).await,
            _ => fetch_markdown(url, max_chars).await,
        }
    }
}

/// Fetch with markdown content negotiation (default mode).
async fn fetch_markdown(url: &str, max_chars: usize) -> Result<Value> {
    let (content, meta) = crate::html_to_md::fetch_as_markdown(url, max_chars).await?;

    let truncated = content.len() >= max_chars;
    let mut result = json!({
        "url": url,
        "finalUrl": meta.final_url,
        "status": meta.status,
        "format": "markdown",
        "server_markdown": meta.server_markdown,
        "truncated": truncated,
        "length": content.len(),
        "text": content
    });

    if let Some(tokens) = meta.token_count {
        result["markdown_tokens"] = json!(tokens);
    }
    if let Some(ref signal) = meta.content_signal {
        result["content_signal"] = json!(signal);
    }

    Ok(result)
}

/// Fetch and extract plain text (strip all formatting).
async fn fetch_text(url: &str, max_chars: usize) -> Result<Value> {
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Tool(format!("Failed to create HTTP client: {}", e)))?;

    let user_agent = format!("blockcell/{} (AI Agent)", env!("CARGO_PKG_VERSION"));

    let response = client
        .get(url)
        .header("User-Agent", user_agent)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Fetch failed: {}", e)))?;

    let final_url = response.url().to_string();
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body = response
        .text()
        .await
        .map_err(|e| Error::Tool(format!("Failed to read response body: {}", e)))?;

    let text = if content_type.contains("text/html") {
        extract_text_from_html(&body)
    } else {
        body
    };

    let truncated = text.len() > max_chars;
    let text = if truncated {
        let mut end = max_chars;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].to_string()
    } else {
        text
    };

    Ok(json!({
        "url": url,
        "finalUrl": final_url,
        "status": status,
        "format": "text",
        "truncated": truncated,
        "length": text.len(),
        "text": text
    }))
}

/// Fetch raw response body without conversion.
async fn fetch_raw(url: &str, max_chars: usize) -> Result<Value> {
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Tool(format!("Failed to create HTTP client: {}", e)))?;

    let user_agent = format!("blockcell/{} (AI Agent)", env!("CARGO_PKG_VERSION"));

    let response = client
        .get(url)
        .header("User-Agent", user_agent)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Fetch failed: {}", e)))?;

    let final_url = response.url().to_string();
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body = response
        .text()
        .await
        .map_err(|e| Error::Tool(format!("Failed to read response body: {}", e)))?;

    let truncated = body.len() > max_chars;
    let body = if truncated {
        let mut end = max_chars;
        while end > 0 && !body.is_char_boundary(end) {
            end -= 1;
        }
        body[..end].to_string()
    } else {
        body
    };

    Ok(json!({
        "url": url,
        "finalUrl": final_url,
        "status": status,
        "content_type": content_type,
        "format": "raw",
        "truncated": truncated,
        "length": body.len(),
        "text": body
    }))
}

fn extract_text_from_html(html: &str) -> String {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    
    // Try to get main content
    let selectors = ["article", "main", "body"];
    
    for sel in selectors {
        if let Ok(selector) = Selector::parse(sel) {
            if let Some(element) = document.select(&selector).next() {
                let text: String = element
                    .text()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if !text.is_empty() {
                    return text;
                }
            }
        }
    }

    // Fallback: get all text
    document
        .root_element()
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_web_search_schema() {
        let tool = WebSearchTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "web_search");
    }

    #[test]
    fn test_web_search_validate() {
        let tool = WebSearchTool;
        assert!(tool.validate(&json!({"query": "rust lang"})).is_ok());
        assert!(tool.validate(&json!({})).is_err());
    }

    #[test]
    fn test_web_fetch_schema() {
        let tool = WebFetchTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "web_fetch");
    }

    #[test]
    fn test_web_fetch_validate() {
        let tool = WebFetchTool;
        assert!(tool.validate(&json!({"url": "https://example.com"})).is_ok());
        assert!(tool.validate(&json!({})).is_err());
    }

    #[test]
    fn test_extract_text_from_html() {
        let html = "<html><body><p>Hello World</p></body></html>";
        let text = extract_text_from_html(html);
        assert!(text.contains("Hello World"));
    }
}
