use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::{Tool, ToolContext, ToolSchema};

// ============ web_search ============

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_search",
            description: "Search the web. Uses Brave Search API if configured. For Chinese queries, automatically uses Baidu first (best quality for Chinese content, especially from overseas servers), then falls back to Bing (zh-CN locale). Tip: set freshness=day for 'last 24 hours' news.",
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

        // Detect if query is primarily Chinese — use Baidu first for better results
        let is_chinese_query = query.chars().any(|c| {
            let cp = c as u32;
            (0x4E00..=0x9FFF).contains(&cp)  // CJK Unified Ideographs
        });

        if is_chinese_query {
            // Baidu: best quality for Chinese queries, especially from overseas servers
            match baidu_search(query, count).await {
                Ok(results) if !results.is_empty() => {
                    return Ok(json!({ "query": query, "results": results, "source": "baidu" }));
                }
                Ok(_) => {
                    tracing::warn!("Baidu returned empty results, trying Bing");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Baidu search failed, trying Bing");
                }
            }
        }

        // Bing with Chinese locale (works globally, zh-CN results)
        match bing_search(query, count, Some(&ctx.workspace)).await {
            Ok(results) => return Ok(json!({ "query": query, "results": results, "source": "bing" })),
            Err(e) => {
                tracing::warn!(error = %e, "Bing search failed");
                // Last resort for Chinese queries: try Baidu if not already tried
                if !is_chinese_query {
                    return Err(e);
                }
                match baidu_search(query, count).await {
                    Ok(results) if !results.is_empty() => {
                        return Ok(json!({ "query": query, "results": results, "source": "baidu" }));
                    }
                    _ => return Err(e),
                }
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

async fn bing_search(query: &str, count: usize, workspace: Option<&std::path::Path>) -> Result<Vec<Value>> {
    use scraper::{Html, Selector};

    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| Error::Tool(format!("Failed to create HTTP client: {}", e)))?;

    // Use mobile Bing — smaller page, faster, simpler DOM than desktop
    let response = client
        .get("https://www.bing.com/search")
        .query(&[
            ("q", query),
            ("count", &count.to_string()),
            ("mkt", "zh-CN"),
            ("setlang", "zh-Hans"),
            ("cc", "CN"),
            ("FORM", "HDRSC1"),
        ])
        .header("User-Agent", "Mozilla/5.0 (Linux; Android 13; Pixel 7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.6099.144 Mobile Safari/537.36")
        .header("Accept-Language", "zh-CN,zh;q=0.9")
        .header("Accept", "text/html,application/xhtml+xml,*/*;q=0.8")
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

    // If Bing returned a bot-check/captcha page, HTML scraping will fail.
    // Mark it explicitly so we can fall back to CDP-based search.
    let looks_blocked = bing_html_looks_blocked(&html);

    // IMPORTANT: Html (scraper) is not Send; keep it in a tight scope so it doesn't live across awaits.
    let results: Vec<Value> = {
        let document = Html::parse_document(&html);

        // Bing organic results: li.b_algo works on both desktop and mobile Bing
        // Mobile Bing may also use li.b_ans or div.b_algo as fallback
        let container_selectors = ["li.b_algo", "li.b_ans", "div.b_algo"];
        let title_sel = Selector::parse("h2 a, .b_title a").unwrap();
        let snippet_sel =
            Selector::parse(".b_caption p, .b_lineclamp2, .b_lineclamp3, .b_lineclamp4, .b_dList")
                .unwrap();

        let mut results = Vec::new();
        let mut seen_urls = std::collections::HashSet::new();

        'outer: for sel_str in &container_selectors {
            let sel = match Selector::parse(sel_str) {
                Ok(s) => s,
                Err(_) => continue,
            };

            for el in document.select(&sel) {
                if results.len() >= count {
                    break 'outer;
                }

                let title_el = el.select(&title_sel).next();
                let title = title_el
                    .map(|e| e.text().collect::<Vec<_>>().join("").trim().to_string())
                    .unwrap_or_default();

                let url = title_el
                    .and_then(|e| e.value().attr("href").map(|h| h.to_string()))
                    .unwrap_or_default();

                if title.is_empty() || url.is_empty() || !url.starts_with("http") {
                    continue;
                }

                if !seen_urls.insert(url.clone()) {
                    continue;
                }

                let snippet = el
                    .select(&snippet_sel)
                    .next()
                    .map(|e| e.text().collect::<Vec<_>>().join("").trim().to_string())
                    .unwrap_or_default();

                results.push(json!({
                    "title": title,
                    "url": url,
                    "snippet": snippet
                }));
            }

            if !results.is_empty() {
                break;
            }
        }

        results
    };

    tracing::debug!(count = results.len(), query, "Bing scrape results");

    if results.is_empty() {
        // CDP fallback: execute JS in a real browser to bypass JS challenges / dynamic DOM.
        if let Some(workspace) = workspace {
            tracing::debug!(query, looks_blocked, "Bing scrape empty; trying CDP fallback");
            if let Ok(cdp_results) = bing_search_cdp(query, count, workspace).await {
                if !cdp_results.is_empty() {
                    return Ok(cdp_results);
                }
            }
        }

        if looks_blocked {
            return Err(Error::Tool(
                "Bing returned a bot-check/captcha page (no parseable results). Consider configuring Brave Search API or try again later.".to_string(),
            ));
        }

        return Err(Error::Tool("Bing returned no parseable results. Try a different query.".to_string()));
    }

    Ok(results)
}

fn bing_html_looks_blocked(html: &str) -> bool {
    let s = html.to_lowercase();
    // Heuristics for Bing bot/captcha / consent pages.
    // Keep this conservative to avoid false positives.
    s.contains("captcha")
        || s.contains("unusual traffic")
        || s.contains("our systems have detected unusual traffic")
        || s.contains("verify")
        || (s.contains("sorry") && s.contains("bing"))
}

// ─────────────────────────────────────────────────────────────────────────────
// CDP fallback (real browser)
// ─────────────────────────────────────────────────────────────────────────────

static BING_CDP_MANAGER: once_cell::sync::Lazy<Arc<Mutex<Option<crate::browser::session::SessionManager>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(None)));

async fn ensure_bing_manager(
    workspace: &std::path::Path,
) -> Arc<Mutex<Option<crate::browser::session::SessionManager>>> {
    let mgr = BING_CDP_MANAGER.clone();
    {
        let mut guard = mgr.lock().await;
        if guard.is_none() {
            let base_dir = workspace.join("browser");
            *guard = Some(crate::browser::session::SessionManager::new(base_dir));
        }
    }
    mgr
}

async fn bing_search_cdp(query: &str, count: usize, workspace: &std::path::Path) -> Result<Vec<Value>> {
    use crate::browser::session::BrowserEngine;

    let encoded = urlencoding::encode(query);
    let url = format!(
        "https://www.bing.com/search?q={}&mkt=zh-CN&setlang=zh-Hans&cc=CN&FORM=HDRSC1",
        encoded
    );

    let mgr_arc = ensure_bing_manager(workspace).await;
    let mut mgr_guard = mgr_arc.lock().await;
    let mgr = mgr_guard
        .as_mut()
        .ok_or_else(|| Error::Tool("CDP session manager not initialized".to_string()))?;

    let session = mgr
        .get_or_create_with_engine("web_search_bing", false, None, BrowserEngine::Chrome)
        .await
        .map_err(|e| Error::Tool(format!("CDP launch failed: {}", e)))?;

    // Set headers to reduce bot blocking.
    let headers = json!({
        "Accept-Language": "zh-CN,zh;q=0.9,en;q=0.8",
        "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36",
    });
    let _ = session.cdp.set_extra_headers(headers).await;

    session
        .cdp
        .navigate(&url)
        .await
        .map_err(|e| Error::Tool(format!("CDP navigate failed: {}", e)))?;

    // Wait a bit for dynamic content / potential interstitial.
    tokio::time::sleep(std::time::Duration::from_millis(1800)).await;

    let js = format!(
        r#"(() => {{
  const max = {};
  const out = [];
  const nodes = document.querySelectorAll('li.b_algo h2 a, div.b_algo h2 a, .b_algo h2 a');
  for (const a of nodes) {{
    if (out.length >= max) break;
    const title = (a.textContent || '').trim();
    const href = (a.getAttribute('href') || '').trim();
    if (!title || !href || !href.startsWith('http')) continue;
    let snippet = '';
    const item = a.closest('li.b_algo') || a.closest('div.b_algo') || a.closest('.b_algo');
    if (item) {{
      const sn = item.querySelector('.b_caption p, .b_lineclamp2, .b_lineclamp3, .b_lineclamp4, .b_dList');
      if (sn) snippet = (sn.textContent || '').trim();
    }}
    out.push({{ title, url: href, snippet }});
  }}
  return out;
}})()"#,
        count
    );

    let eval = session
        .cdp
        .evaluate_js(&js)
        .await
        .map_err(|e| Error::Tool(format!("CDP evaluate failed: {}", e)))?;

    let arr = eval
        .get("result")
        .and_then(|v| v.get("value"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let results: Vec<Value> = arr
        .into_iter()
        .filter_map(|v| {
            let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let snippet = v
                .get("snippet")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            Some(json!({"title": title, "url": url, "snippet": snippet}))
        })
        .collect();

    tracing::debug!(count = results.len(), query, "Bing CDP results");
    Ok(results)
}

/// Baidu search scraper — best for Chinese queries, especially from overseas servers.
async fn baidu_search(query: &str, count: usize) -> Result<Vec<Value>> {
    use scraper::{Html, Selector};

    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| Error::Tool(format!("Failed to create HTTP client: {}", e)))?;

    // Use mobile Baidu (m.baidu.com) — smaller page (~50KB vs ~300KB), faster, simpler DOM
    let response = client
        .get("https://m.baidu.com/s")
        .query(&[("word", query), ("rn", &count.to_string())])
        .header("User-Agent", "Mozilla/5.0 (Linux; Android 13; Pixel 7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.6099.144 Mobile Safari/537.36")
        .header("Accept-Language", "zh-CN,zh;q=0.9")
        .header("Accept", "text/html,application/xhtml+xml,*/*;q=0.8")
        .header("Referer", "https://m.baidu.com/")
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Baidu search failed: {}", e)))?;

    if !response.status().is_success() {
        return Err(Error::Tool(format!("Baidu returned status {}", response.status())));
    }

    let html = response
        .text()
        .await
        .map_err(|e| Error::Tool(format!("Failed to read Baidu response: {}", e)))?;

    let document = Html::parse_document(&html);

    // Mobile Baidu DOM is simpler and more stable than desktop.
    // Each result is an <article> or <div class="c-result"> with a title link and summary.
    // Try multiple selectors from most to least specific.
    let container_selectors = [
        "article",
        "div.c-result",
        "div[data-log]",
        "div.result",
        "#page-bd > div",
    ];

    // Mobile Baidu title link patterns
    let title_sel = Selector::parse("h3 a, .c-title a, .c-result-title a").unwrap();
    // Mobile Baidu snippet patterns
    let snippet_sel = Selector::parse(
        ".c-abstract, .c-summary, .c-gap-top-small, p[class], [class*='abstract'], [class*='summary']"
    ).unwrap();

    let mut results: Vec<Value> = Vec::new();
    let mut seen_urls = std::collections::HashSet::new();

    'outer: for sel_str in &container_selectors {
        if results.len() >= count {
            break;
        }
        let sel = match Selector::parse(sel_str) {
            Ok(s) => s,
            Err(_) => continue,
        };

        for el in document.select(&sel) {
            if results.len() >= count {
                break 'outer;
            }

            let title_el = el.select(&title_sel).next();
            let title = match title_el {
                Some(e) => {
                    let t = e.text().collect::<Vec<_>>().join("").trim().to_string();
                    if t.is_empty() { continue; }
                    t
                }
                None => continue,
            };

            let url = title_el
                .and_then(|e| e.value().attr("href").map(|h| h.to_string()))
                .unwrap_or_default();

            if url.is_empty() {
                continue;
            }

            let display_url = if url.starts_with("http") {
                url.clone()
            } else if url.starts_with("/link") || url.starts_with("/s?") {
                format!("https://www.baidu.com{}", url)
            } else {
                continue;
            };

            if !seen_urls.insert(display_url.clone()) {
                continue;
            }

            let snippet = el
                .select(&snippet_sel)
                .next()
                .map(|e| e.text().collect::<Vec<_>>().join("").trim().to_string())
                .unwrap_or_default();

            results.push(json!({
                "title": title,
                "url": display_url,
                "snippet": snippet
            }));
        }

        if !results.is_empty() {
            break;
        }
    }

    tracing::debug!(count = results.len(), query, "Baidu scrape results");

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
async fn fetch_markdown(url: &str, max_chars: usize, workspace: Option<&std::path::Path>) -> Result<Value> {
    let (content, meta) = crate::html_to_md::fetch_as_markdown(url, max_chars).await?;

    // If the result looks like a JS challenge page, try CDP.
    let is_challenge = html_looks_like_challenge(&content)
        || (content.trim().len() < 500 && meta.status == 200);

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
    use crate::browser::session::BrowserEngine;

    let mgr_arc = ensure_bing_manager(workspace).await;
    let mut mgr_guard = mgr_arc.lock().await;
    let mgr = mgr_guard
        .as_mut()
        .ok_or_else(|| Error::Tool("CDP session manager not initialized".to_string()))?;

    let session = mgr
        .get_or_create_with_engine("web_fetch_cdp", false, None, BrowserEngine::Chrome)
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
        .and_then(|_| None::<String>)
        .unwrap_or_else(|| url.to_string());

    let markdown = crate::html_to_md::html_to_markdown(&html);
    let markdown = if markdown.len() > max_chars {
        let mut end = max_chars;
        while end > 0 && !markdown.is_char_boundary(end) { end -= 1; }
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
