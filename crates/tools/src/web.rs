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
            description: "Search the web. REQUIRED: always provide a non-empty string parameter `query`; do not call this tool with `{}`. Optional parameters: `count` (number of results) and `freshness` (time filter). Supports two backends: Brave Search API (`api_key`) and Baidu AI Search API (`baidu_api_key`). For Chinese queries, Baidu is preferred. Configure keys in `tools.web.search` or env var `BAIDU_API_KEY`.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query"
                    },
                    "count": {
                        "type": "integer",
                        "description": "Number of results (1-20, default 5)"
                    },
                    "freshness": {
                        "type": "string",
                        "description": "Recency filter. Brave: day/week/month/year. Baidu: week/month/semiyear/year.",
                        "enum": ["day", "week", "month", "semiyear", "year"]
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- **Web search**: Use `web_search` for discovery. Supports Brave Search API and Baidu AI Search API. Chinese queries prefer Baidu; non-Chinese prefer Brave. For 'latest/最近/24小时/今天' news queries, set `freshness=day`. **If `web_search` returns a config/API-key error, you MUST tell the user to configure the API key** (tools.web.search.apiKey for Brave, tools.web.search.baiduApiKey or env BAIDU_API_KEY for Baidu) — do NOT answer from memory as if search succeeded. **If results are irrelevant**: retry with rephrased query (shorter, different keywords) before concluding no results exist — never give up after just one failed search.".to_string())
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("query").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: query".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let query = params["query"].as_str().unwrap();
        let count = params
            .get("count")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(20) as usize;

        let freshness = params
            .get("freshness")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Resolve Baidu API key: config > env
        let baidu_key = {
            let k = &ctx.config.tools.web.search.baidu_api_key;
            if !k.is_empty() {
                Some(k.clone())
            } else {
                std::env::var("BAIDU_API_KEY")
                    .ok()
                    .filter(|v| !v.is_empty())
            }
        };

        let brave_key = &ctx.config.tools.web.search.api_key;

        // Detect if query is primarily Chinese
        let is_chinese = query.chars().any(|c| {
            let cp = c as u32;
            (0x4E00..=0x9FFF).contains(&cp)
        });

        // Strategy: Chinese → Baidu first, then Brave; non-Chinese → Brave first, then Baidu
        if is_chinese {
            // 1. Try Baidu API
            if let Some(ref key) = baidu_key {
                match baidu_search(key, query, count, freshness.as_deref()).await {
                    Ok(results) if !results.is_empty() => {
                        return Ok(
                            json!({ "query": query, "results": results, "source": "baidu" }),
                        );
                    }
                    Ok(_) => tracing::warn!("Baidu API returned empty results, trying Brave"),
                    Err(e) => tracing::warn!(error = %e, "Baidu API failed, trying Brave"),
                }
            }
            // 2. Fallback to Brave
            if !brave_key.is_empty() {
                match brave_search(brave_key, query, count, freshness.as_deref()).await {
                    Ok(results) => {
                        return Ok(json!({ "query": query, "results": results, "source": "brave" }))
                    }
                    Err(e) => tracing::warn!(error = %e, "Brave search also failed"),
                }
            }
        } else {
            // 1. Try Brave
            if !brave_key.is_empty() {
                match brave_search(brave_key, query, count, freshness.as_deref()).await {
                    Ok(results) if !results.is_empty() => {
                        return Ok(
                            json!({ "query": query, "results": results, "source": "brave" }),
                        );
                    }
                    Ok(_) => tracing::warn!("Brave returned empty results, trying Baidu"),
                    Err(e) => tracing::warn!(error = %e, "Brave search failed, trying Baidu"),
                }
            }
            // 2. Fallback to Baidu
            if let Some(ref key) = baidu_key {
                match baidu_search(key, query, count, freshness.as_deref()).await {
                    Ok(results) if !results.is_empty() => {
                        return Ok(
                            json!({ "query": query, "results": results, "source": "baidu" }),
                        );
                    }
                    Ok(_) => tracing::warn!("Baidu API returned empty results"),
                    Err(e) => tracing::warn!(error = %e, "Baidu API also failed"),
                }
            }
        }

        // If we get here, all attempts failed or no keys configured
        if brave_key.is_empty() && baidu_key.is_none() {
            return Err(Error::Tool(
                "No search API configured. Set tools.web.search.api_key (Brave) or tools.web.search.baidu_api_key / env BAIDU_API_KEY (Baidu).".to_string()
            ));
        }

        Err(Error::Tool(format!(
            "All search backends failed for query '{}'. Check API keys and network connectivity.",
            query
        )))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Brave Search API
// ─────────────────────────────────────────────────────────────────────────────

async fn brave_search(
    api_key: &str,
    query: &str,
    count: usize,
    freshness: Option<&str>,
) -> Result<Vec<Value>> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| Error::Tool(format!("HTTP client error: {}", e)))?;

    let mut req = client
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("X-Subscription-Token", api_key)
        .query(&[("q", query), ("count", &count.to_string())]);

    if let Some(f) = freshness {
        req = req.query(&[("freshness", f)]);
    }

    let response = req
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Brave search request failed: {}", e)))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Tool(format!("Brave API error {}: {}", status, text)));
    }

    let data: Value = response
        .json()
        .await
        .map_err(|e| Error::Tool(format!("Failed to parse Brave response: {}", e)))?;

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

// ─────────────────────────────────────────────────────────────────────────────
// Baidu AI Search API (qianfan.baidubce.com)
// ─────────────────────────────────────────────────────────────────────────────

async fn baidu_search(
    api_key: &str,
    query: &str,
    count: usize,
    freshness: Option<&str>,
) -> Result<Vec<Value>> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| Error::Tool(format!("HTTP client error: {}", e)))?;

    // Build request body per Baidu AI Search API spec
    let top_k = count.min(50);
    let mut body = json!({
        "messages": [{ "content": query, "role": "user" }],
        "search_source": "baidu_search_v2",
        "resource_type_filter": [{ "type": "web", "top_k": top_k }],
        "safe_search": false,
    });

    // Map freshness filter to Baidu's search_recency_filter
    if let Some(f) = freshness {
        let baidu_recency = match f {
            "day" => "week", // Baidu has no "day", use closest
            "week" => "week",
            "month" => "month",
            "semiyear" => "semiyear",
            "year" => "year",
            _ => "year",
        };
        body["search_recency_filter"] = json!(baidu_recency);
    }

    let response = client
        .post("https://qianfan.baidubce.com/v2/ai_search/web_search")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Baidu search request failed: {}", e)))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Tool(format!("Baidu API error {}: {}", status, text)));
    }

    let data: Value = response
        .json()
        .await
        .map_err(|e| Error::Tool(format!("Failed to parse Baidu response: {}", e)))?;

    // Check for API-level error
    if let Some(code) = data.get("code") {
        let msg = data["message"].as_str().unwrap_or("Unknown error");
        return Err(Error::Tool(format!("Baidu API error {}: {}", code, msg)));
    }

    // Parse references array
    let references = data["references"]
        .as_array()
        .ok_or_else(|| Error::Tool("Baidu API returned no references field".to_string()))?;

    let results: Vec<Value> = references
        .iter()
        .take(count)
        .map(|r| {
            json!({
                "title": r["title"].as_str().unwrap_or(""),
                "url": r["url"].as_str().unwrap_or(""),
                "snippet": r["content"].as_str().or_else(|| r["abstract"].as_str()).unwrap_or(""),
                "site_name": r["site_name"].as_str().unwrap_or(""),
                "publish_time": r["publish_time"].as_str().unwrap_or("")
            })
        })
        .filter(|r| {
            let url = r["url"].as_str().unwrap_or("");
            let title = r["title"].as_str().unwrap_or("");
            !url.is_empty() && !title.is_empty()
        })
        .collect();

    tracing::debug!(count = results.len(), query, "Baidu API results");
    Ok(results)
}

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_fetch",
            description: "Fetch a web page and return its content as clean Markdown. REQUIRED: always provide a valid `http` or `https` URL in parameter `url`; do not call this tool with `{}`. Optional parameters include `extractMode` and `maxChars`. Uses `Accept: text/markdown` content negotiation (Cloudflare Markdown for Agents) for optimal results — if the server supports it, markdown is returned directly with ~80% token savings. Otherwise, HTML is converted to markdown locally. Returns `markdown_tokens` estimate and `content_signal` when available.",
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

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some(concat!(
            "- **Web content**: `web_fetch` returns markdown by default (Cloudflare Markdown for Agents content negotiation, ~80% token savings). Use `browse` for JS-heavy sites or interactive automation.\n",
            "- **信息充足性原则（避免过度抓取）**: 每次 `web_fetch` 后先评估已有信息是否满足任务需求，**够用就停止**，不要贪婪地抓取所有搜索结果。判断标准：(1) 用户要求[找N篇/N个] -> 已收集到N个独立来源即可停止；(2) 用户要求[总结/汇总] -> 有2-3个高质量来源即可，无需穷举；(3) 用户要求[最新/最全] -> 才需要多源验证。**错误做法**：搜到10条结果就逐一fetch全部。**正确做法**：fetch前几条最相关的，判断内容是否满足需求，满足则直接执行后续任务（写文件/输出等）。"
        ).to_string())
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

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
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
            _ => fetch_markdown(url, max_chars, Some(&ctx.workspace)).await,
        }
    }
}

/// Detect JS challenge / anti-bot waiting pages (Cloudflare, etc.).
fn html_looks_like_challenge(body: &str) -> bool {
    let s = body.to_lowercase();
    // Cloudflare "Just a moment..." / "Please wait" interstitial
    (s.contains("just a moment") && s.contains("cloudflare"))
        || s.contains("please wait while we verify")
        || s.contains("checking if the site connection is secure")
        || s.contains("enable javascript and cookies to continue")
        || s.contains("ddos protection by cloudflare")
        || (s.contains("please wait") && (s.len() < 8192))
}

/// Fetch with markdown content negotiation (default mode).
/// Falls back to CDP browser fetch if the response looks like a JS challenge page.
async fn fetch_markdown(
    url: &str,
    max_chars: usize,
    workspace: Option<&std::path::Path>,
) -> Result<Value> {
    let (content, meta) = crate::html_to_md::fetch_as_markdown(url, max_chars).await?;

    // If the result looks like a JS challenge page, try CDP.
    let is_challenge =
        html_looks_like_challenge(&content) || (content.trim().len() < 500 && meta.status == 200);

    if is_challenge {
        if let Some(ws) = workspace {
            tracing::debug!(url, "web_fetch: JS challenge detected, trying CDP fallback");
            match fetch_via_cdp(url, max_chars, ws).await {
                Ok(cdp_result) => return Ok(cdp_result),
                Err(e) => tracing::warn!(error = %e, url, "CDP fetch fallback failed"),
            }
        }
    }

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

/// Fetch a page via CDP (real browser) to bypass JS challenges.
async fn fetch_via_cdp(url: &str, max_chars: usize, workspace: &std::path::Path) -> Result<Value> {
    use crate::browser::session::{find_browser_binary, BrowserEngine, SessionManager};
    use std::sync::Arc;
    use tokio::sync::Mutex;

    static FETCH_CDP_MANAGER: once_cell::sync::Lazy<Arc<Mutex<Option<SessionManager>>>> =
        once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(None)));

    let mgr_arc = FETCH_CDP_MANAGER.clone();
    {
        let mut guard = mgr_arc.lock().await;
        if guard.is_none() {
            let base_dir = workspace.join("browser");
            *guard = Some(SessionManager::new(base_dir));
        }
    }

    let mut mgr_guard = mgr_arc.lock().await;
    let mgr = mgr_guard
        .as_mut()
        .ok_or_else(|| Error::Tool("CDP session manager not initialized".to_string()))?;

    let engine = [
        BrowserEngine::Chrome,
        BrowserEngine::Edge,
        BrowserEngine::Firefox,
    ]
    .into_iter()
    .find(|e| find_browser_binary(*e).is_some())
    .ok_or_else(|| Error::Tool("No CDP browser found (chrome/edge/firefox).".to_string()))?;
    let session_name = format!("web_fetch_cdp_{}", engine.name());

    let session = mgr
        .get_or_create_with_engine(&session_name, false, None, engine)
        .await
        .map_err(|e| Error::Tool(format!("CDP launch failed: {}", e)))?;

    session
        .cdp
        .navigate(url)
        .await
        .map_err(|e| Error::Tool(format!("CDP navigate failed: {}", e)))?;

    // Wait for JS challenge to resolve (Cloudflare typically takes ~2s).
    tokio::time::sleep(std::time::Duration::from_millis(3000)).await;

    // Get the rendered HTML via outerHTML.
    let eval = session
        .cdp
        .evaluate_js("document.documentElement.outerHTML")
        .await
        .map_err(|e| Error::Tool(format!("CDP evaluate failed: {}", e)))?;

    let html = eval
        .get("result")
        .and_then(|v| v.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if html.is_empty() {
        return Err(Error::Tool("CDP returned empty page".to_string()));
    }

    let current_url = eval
        .get("result")
        .and(None::<String>)
        .unwrap_or_else(|| url.to_string());

    let markdown = crate::html_to_md::html_to_markdown(&html);
    let markdown = if markdown.len() > max_chars {
        let mut end = max_chars;
        while end > 0 && !markdown.is_char_boundary(end) {
            end -= 1;
        }
        markdown[..end].to_string()
    } else {
        markdown
    };

    let truncated = markdown.len() >= max_chars;
    Ok(json!({
        "url": url,
        "finalUrl": current_url,
        "status": 200,
        "format": "markdown",
        "server_markdown": false,
        "via_cdp": true,
        "truncated": truncated,
        "length": markdown.len(),
        "text": markdown
    }))
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
        assert!(tool
            .validate(&json!({"url": "https://example.com"}))
            .is_ok());
        assert!(tool.validate(&json!({})).is_err());
    }

    #[test]
    fn test_extract_text_from_html() {
        let html = "<html><body><p>Hello World</p></body></html>";
        let text = extract_text_from_html(html);
        assert!(text.contains("Hello World"));
    }
}
