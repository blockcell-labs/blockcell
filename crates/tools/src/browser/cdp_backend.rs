//! 基于 Chrome DevTools Protocol 的 [`BrowserBackend`] 实现。

use async_trait::async_trait;
use blockcell_core::Result;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::backend::{BrowserBackend, BrowserRequest};
use super::session::get_target_ws_url;
use super::session::BrowserSession;
use super::session::{list_available_browsers, BrowserEngine, SessionManager};
use super::snapshot::{assign_refs, parse_ax_tree, render_tree, snapshot_to_json};

/// Global session manager (daemon model — persists across tool calls).
static SESSION_MANAGER: once_cell::sync::Lazy<Arc<Mutex<Option<SessionManager>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(None)));

async fn ensure_manager(workspace: &Path) -> Arc<Mutex<Option<SessionManager>>> {
    let mgr = SESSION_MANAGER.clone();
    {
        let mut guard = mgr.lock().await;
        if guard.is_none() {
            let base_dir = workspace.join("browser");
            *guard = Some(SessionManager::new(base_dir));
        }
    }
    mgr
}

/// 本机启动 Chrome / Edge / Firefox 并通过 CDP 驱动；会话在进程内全局共享。
#[derive(Debug, Default, Clone, Copy)]
pub struct CdpBrowserBackend;

#[async_trait]
impl BrowserBackend for CdpBrowserBackend {
    async fn list_sessions(&self, workspace: &Path) -> Result<Vec<String>> {
        let mgr_arc = ensure_manager(workspace).await;
        let guard = mgr_arc.lock().await;
        Ok(guard
            .as_ref()
            .map(|mgr| {
                mgr.list_sessions()
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn close_session(&self, workspace: &Path, session: &str) -> Result<()> {
        let mgr_arc = ensure_manager(workspace).await;
        let mut guard = mgr_arc.lock().await;
        guard
            .as_mut()
            .unwrap()
            .close_session(session)
            .await
            .map_err(|e| blockcell_core::Error::Tool(format!("session_close: {}", e)))
    }

    fn available_browsers(&self) -> Vec<(BrowserEngine, String)> {
        list_available_browsers()
    }

    async fn perform(&self, request: BrowserRequest<'_>) -> Result<Value> {
        let BrowserRequest {
            workspace,
            session: session_name,
            headed,
            engine,
            action,
            params,
        } = request;
        let mgr_arc = ensure_manager(workspace).await;
        let mut mgr_guard = mgr_arc.lock().await;
        let mgr = mgr_guard.as_mut().unwrap();

        // Get or create session with specified engine
        let session = mgr
            .get_or_create_with_engine(session_name, headed, None, engine)
            .await
            .map_err(|e| blockcell_core::Error::Tool(format!("session error: {}", e)))?;

        match action {
            "navigate" => action_navigate(session, params).await,
            "snapshot" => action_snapshot(session, params).await,
            "click" => action_click(session, params).await,
            "fill" => action_fill(session, params).await,
            "type_text" => action_type_text(session, params).await,
            "press_key" => action_press_key(session, params).await,
            "scroll" => action_scroll(session, params).await,
            "wait" => action_wait(session, params).await,
            "screenshot" => action_screenshot(session, params, workspace).await,
            "pdf" => action_pdf(session, params, workspace).await,
            "execute_js" => action_execute_js(session, params).await,
            "get_content" => action_get_content(session).await,
            "get_url" => action_get_url(session).await,
            "cookies_get" => action_cookies_get(session).await,
            "cookies_set" => action_cookies_set(session, params).await,
            "cookies_clear" => action_cookies_clear(session).await,
            "set_viewport" => action_set_viewport(session, params).await,
            "set_headers" => action_set_headers(session, params).await,
            "back" => action_history(session, "back").await,
            "forward" => action_history(session, "forward").await,
            "reload" => action_reload(session).await,
            "tab_list" => action_tab_list(session).await,
            "tab_new" => action_tab_new(session, params).await,
            "tab_close" => action_tab_close(session, params).await,
            "tab_switch" => action_tab_switch(session, params).await,
            "upload_file" => action_upload_file(session, params).await,
            "dialog_handle" => action_dialog_handle(session, params).await,
            "network_intercept" => action_network_intercept(session, params).await,
            "network_continue" => action_network_continue(session, params).await,
            "network_block" => action_network_block(session, params).await,
            _ => Err(blockcell_core::Error::Tool(format!(
                "Unknown browse action: {}",
                action
            ))),
        }
    }
}

// ─── Action implementations ───────────────────────────────────────────

/// 等待页面加载完成。订阅 Page.loadEventFired 事件，超时后回退到固定等待。
/// 订阅 Page.loadEventFired 事件（应在导航前调用，避免竞态）
async fn subscribe_page_load(
    cdp: &super::cdp::CdpClient,
) -> tokio::sync::mpsc::Receiver<serde_json::Value> {
    cdp.subscribe_event("Page.loadEventFired").await
}

/// 等待 Page.loadEventFired 事件（使用预先订阅的 receiver）
async fn wait_for_page_load_event(mut rx: tokio::sync::mpsc::Receiver<serde_json::Value>) {
    let timeout = std::time::Duration::from_secs(30);
    match tokio::time::timeout(timeout, rx.recv()).await {
        Ok(Some(_)) => {
            tracing::debug!("Page.loadEventFired received");
        }
        Ok(None) => {
            tracing::warn!("Page.loadEventFired channel closed, falling back to fixed wait");
            tokio::time::sleep(std::time::Duration::from_millis(3000)).await;
        }
        Err(_) => {
            tracing::warn!("Page.loadEventFired timed out after 30s, falling back to fixed wait");
            tokio::time::sleep(std::time::Duration::from_millis(3000)).await;
        }
    }
}

async fn action_navigate(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let url = params["url"]
        .as_str()
        .ok_or_else(|| blockcell_core::Error::Tool("navigate requires 'url'".into()))?;

    let new_tab = params["new_tab"].as_bool().unwrap_or(false);

    tracing::info!(
        session = %session.name,
        url = %url,
        new_tab = new_tab,
        "browse.navigate start"
    );

    if new_tab {
        let target_id = session.cdp.create_target(url).await.map_err(cdp_err)?;
        let _ = session.cdp.activate_target(&target_id).await;

        let ws_url = get_target_ws_url(session.debug_port, &target_id)
            .await
            .map_err(|e| blockcell_core::Error::Tool(format!("tab ws resolve: {}", e)))?;

        session.cdp = super::cdp::CdpClient::connect(&ws_url)
            .await
            .map_err(|e| blockcell_core::Error::Tool(format!("cdp reconnect: {}", e)))?;

        session.cdp.enable_domain("Page").await.map_err(cdp_err)?;
        session
            .cdp
            .enable_domain("Runtime")
            .await
            .map_err(cdp_err)?;
        session.cdp.enable_domain("DOM").await.map_err(cdp_err)?;
        session
            .cdp
            .enable_domain("Network")
            .await
            .map_err(cdp_err)?;
        session
            .cdp
            .enable_domain("Accessibility")
            .await
            .map_err(cdp_err)?;

        // 先订阅 load 事件，再发起导航，避免快速页面漏掉事件
        let load_rx = subscribe_page_load(&session.cdp).await;
        let nav_result = session.cdp.navigate(url).await.map_err(cdp_err)?;
        // 检查 Page.navigate 返回的 errorText 字段
        if let Some(error_text) = nav_result.get("errorText").and_then(|v| v.as_str()) {
            if !error_text.is_empty() {
                tracing::warn!(url = %url, error = %error_text, "Page.navigate reported error");
            }
        }
        session.current_url = Some(url.to_string());

        wait_for_page_load_event(load_rx).await;
        let snap = take_snapshot(session, true).await?;

        tracing::info!(
            session = %session.name,
            url = %url,
            target_id = %target_id,
            ref_count = snap.get("ref_count").and_then(|v| v.as_u64()).unwrap_or(0),
            "browse.navigate success (new_tab)"
        );

        return Ok(json!({
            "status": "navigated",
            "url": url,
            "new_tab": true,
            "target_id": target_id,
            "snapshot": snap.get("snapshot"),
            "ref_count": snap.get("ref_count"),
        }));
    }

    // 先订阅 load 事件，再发起导航，避免快速页面（about:blank/data: URL）漏掉事件
    let load_rx = subscribe_page_load(&session.cdp).await;
    let nav_result = session.cdp.navigate(url).await.map_err(cdp_err)?;
    // 检查 Page.navigate 返回的 errorText 字段
    if let Some(error_text) = nav_result.get("errorText").and_then(|v| v.as_str()) {
        if !error_text.is_empty() {
            tracing::warn!(url = %url, error = %error_text, "Page.navigate reported error");
        }
    }
    session.current_url = Some(url.to_string());

    // 等待页面加载（使用预先订阅的 receiver，超时后回退到固定等待）
    wait_for_page_load_event(load_rx).await;

    // Auto-snapshot after navigation
    let snap = take_snapshot(session, true).await?;

    tracing::info!(
        session = %session.name,
        url = %url,
        ref_count = snap.get("ref_count").and_then(|v| v.as_u64()).unwrap_or(0),
        "browse.navigate success"
    );

    Ok(json!({
        "status": "navigated",
        "url": url,
        "snapshot": snap.get("snapshot"),
        "ref_count": snap.get("ref_count"),
    }))
}

async fn action_snapshot(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let compact = params["compact"].as_bool().unwrap_or(true);
    take_snapshot(session, compact).await
}

async fn action_click(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    // Resolve element: by ref or by selector
    let (method, target) = resolve_element_target(params)?;

    match method {
        "ref" => {
            let ref_data = session.refs.get(&target).cloned().ok_or_else(|| {
                blockcell_core::Error::Tool(format!(
                    "Ref '{}' not found. Take a snapshot first.",
                    target
                ))
            })?;
            let backend_node_id = ref_data["backendNodeId"]
                .as_i64()
                .ok_or_else(|| blockcell_core::Error::Tool("Ref has no backendNodeId".into()))?;

            // Use CDP to click via backendNodeId
            click_by_backend_node(session, backend_node_id).await?;
        }
        "selector" => {
            click_by_selector(session, &target).await?;
        }
        _ => unreachable!(),
    }

    // Brief wait for UI update
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    Ok(json!({"status": "clicked", "target": target}))
}

async fn action_fill(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let text = params["text"]
        .as_str()
        .ok_or_else(|| blockcell_core::Error::Tool("fill requires 'text'".into()))?;

    let (method, target) = resolve_element_target(params)?;

    // Focus the element first
    match method {
        "ref" => {
            let ref_data = session.refs.get(&target).cloned().ok_or_else(|| {
                blockcell_core::Error::Tool(format!("Ref '{}' not found", target))
            })?;
            let backend_node_id = ref_data["backendNodeId"]
                .as_i64()
                .ok_or_else(|| blockcell_core::Error::Tool("Ref has no backendNodeId".into()))?;
            focus_by_backend_node(session, backend_node_id).await?;
        }
        "selector" => {
            focus_by_selector(session, &target).await?;
        }
        _ => unreachable!(),
    }

    // Clear existing content and insert new text
    session.cdp.evaluate_js(
        "document.activeElement && (document.activeElement.value = '', document.activeElement.textContent = '')"
    ).await.map_err(cdp_err)?;

    session.cdp.insert_text(text).await.map_err(cdp_err)?;

    // Dispatch input event for frameworks
    session.cdp.evaluate_js(
        "document.activeElement && document.activeElement.dispatchEvent(new Event('input', {bubbles: true}))"
    ).await.map_err(cdp_err)?;

    Ok(json!({"status": "filled", "target": target, "text": text}))
}

async fn action_type_text(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let text = params["text"]
        .as_str()
        .ok_or_else(|| blockcell_core::Error::Tool("type_text requires 'text'".into()))?;

    // If a target is specified, focus it first
    if let Ok((method, target)) = resolve_element_target(params) {
        match method {
            "ref" => {
                if let Some(ref_data) = session.refs.get(&target) {
                    if let Some(id) = ref_data["backendNodeId"].as_i64() {
                        let _ = focus_by_backend_node(session, id).await;
                    }
                }
            }
            "selector" => {
                let _ = focus_by_selector(session, &target).await;
            }
            _ => {}
        }
    }

    // Type each character as key events
    session.cdp.insert_text(text).await.map_err(cdp_err)?;

    Ok(json!({"status": "typed", "text": text}))
}

async fn action_press_key(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let key = params["key"]
        .as_str()
        .ok_or_else(|| blockcell_core::Error::Tool("press_key requires 'key'".into()))?;

    let (key_name, code, modifiers) = parse_key_spec(key);

    session
        .cdp
        .dispatch_key_event("keyDown", &key_name, &code, modifiers)
        .await
        .map_err(cdp_err)?;
    session
        .cdp
        .dispatch_key_event("keyUp", &key_name, &code, modifiers)
        .await
        .map_err(cdp_err)?;

    Ok(json!({"status": "key_pressed", "key": key}))
}

async fn action_scroll(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let direction = params["direction"].as_str().unwrap_or("down");
    let amount = params["amount"].as_i64().unwrap_or(400);

    let (dx, dy) = match direction {
        "up" => (0, -amount),
        "down" => (0, amount),
        "left" => (-amount, 0),
        "right" => (amount, 0),
        _ => (0, amount),
    };

    // If a selector is given, scroll that element
    if let Some(selector) = params["selector"].as_str() {
        let js = format!(
            "document.querySelector('{}')?.scrollBy({}, {})",
            selector.replace('\'', "\\'"),
            dx,
            dy
        );
        session.cdp.evaluate_js(&js).await.map_err(cdp_err)?;
    } else {
        let js = format!("window.scrollBy({}, {})", dx, dy);
        session.cdp.evaluate_js(&js).await.map_err(cdp_err)?;
    }

    Ok(json!({"status": "scrolled", "direction": direction, "amount": amount}))
}

async fn action_wait(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let timeout_ms = params["timeout"].as_u64().unwrap_or(5000);

    if let Some(selector) = params["wait_for"].as_str().or(params["selector"].as_str()) {
        // Wait for element to appear
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(timeout_ms);
        let js = format!(
            "!!document.querySelector('{}')",
            selector.replace('\'', "\\'")
        );

        loop {
            if start.elapsed() > timeout {
                return Ok(json!({
                    "status": "timeout",
                    "selector": selector,
                    "waited_ms": timeout_ms,
                }));
            }
            let result = session.cdp.evaluate_js(&js).await.map_err(cdp_err)?;
            if let Some(true) = result
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_bool())
            {
                return Ok(json!({
                    "status": "found",
                    "selector": selector,
                    "waited_ms": start.elapsed().as_millis() as u64,
                }));
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    } else {
        // Simple time-based wait
        tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
        Ok(json!({"status": "waited", "ms": timeout_ms}))
    }
}

async fn action_screenshot(
    session: &mut BrowserSession,
    params: &Value,
    workspace: &std::path::Path,
) -> Result<Value> {
    let full_page = params["full_page"].as_bool().unwrap_or(false);
    let base64_data = session.cdp.screenshot(full_page).await.map_err(cdp_err)?;

    let media_dir = workspace.join("media");
    if let Err(e) = std::fs::create_dir_all(&media_dir) {
        tracing::warn!(error = %e, "Failed to create media directory for screenshot");
    }
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let workspace_path = media_dir.join(format!("screenshot_{}.png", ts));

    let user_path = params["output_path"].as_str().map(|p| {
        if p.starts_with("~/") {
            dirs::home_dir()
                .map(|h| h.join(&p[2..]))
                .unwrap_or_else(|| std::path::PathBuf::from(p))
        } else {
            std::path::PathBuf::from(p)
        }
    });

    // Decode base64 and write
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&base64_data)
        .map_err(|e| blockcell_core::Error::Tool(format!("base64 decode: {}", e)))?;

    // Always write to workspace first (for webui display)
    std::fs::write(&workspace_path, &bytes)
        .map_err(|e| blockcell_core::Error::Tool(format!("write screenshot: {}", e)))?;

    // If user specified a path outside workspace, also copy there
    let extra_path = if let Some(ref up) = user_path {
        if !up.starts_with(workspace) {
            if let Some(parent) = up.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    tracing::warn!(error = %e, path = %up.display(), "Failed to create parent directory for screenshot output");
                }
            }
            if let Err(e) = std::fs::copy(&workspace_path, up) {
                tracing::warn!(error = %e, path = %up.display(), "Failed to copy screenshot to user-specified path");
            }
            Some(up.display().to_string())
        } else {
            None
        }
    } else {
        None
    };

    let mut result = json!({
        "status": "screenshot_saved",
        "path": workspace_path.display().to_string(),
        "size_bytes": bytes.len(),
    });
    if let Some(extra) = extra_path {
        result["also_saved_to"] = json!(extra);
    }
    Ok(result)
}

async fn action_pdf(
    session: &mut BrowserSession,
    params: &Value,
    workspace: &std::path::Path,
) -> Result<Value> {
    let base64_data = session.cdp.print_to_pdf().await.map_err(cdp_err)?;

    let media_dir = workspace.join("media");
    if let Err(e) = std::fs::create_dir_all(&media_dir) {
        tracing::warn!(error = %e, "Failed to create media directory for PDF");
    }
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let workspace_path = media_dir.join(format!("page_{}.pdf", ts));

    let user_path = params["output_path"].as_str().map(|p| {
        if p.starts_with("~/") {
            dirs::home_dir()
                .map(|h| h.join(&p[2..]))
                .unwrap_or_else(|| std::path::PathBuf::from(p))
        } else {
            std::path::PathBuf::from(p)
        }
    });

    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&base64_data)
        .map_err(|e| blockcell_core::Error::Tool(format!("base64 decode: {}", e)))?;

    // Always write to workspace first
    std::fs::write(&workspace_path, &bytes)
        .map_err(|e| blockcell_core::Error::Tool(format!("write pdf: {}", e)))?;

    // If user specified a path outside workspace, also copy there
    let extra_path = if let Some(ref up) = user_path {
        if !up.starts_with(workspace) {
            if let Some(parent) = up.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    tracing::warn!(error = %e, path = %up.display(), "Failed to create parent directory for PDF output");
                }
            }
            if let Err(e) = std::fs::copy(&workspace_path, up) {
                tracing::warn!(error = %e, path = %up.display(), "Failed to copy PDF to user-specified path");
            }
            Some(up.display().to_string())
        } else {
            None
        }
    } else {
        None
    };

    let mut result = json!({
        "status": "pdf_saved",
        "path": workspace_path.display().to_string(),
        "size_bytes": bytes.len(),
    });
    if let Some(extra) = extra_path {
        result["also_saved_to"] = json!(extra);
    }
    Ok(result)
}

async fn action_execute_js(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let expression = params["text"].as_str().ok_or_else(|| {
        blockcell_core::Error::Tool("execute_js requires 'text' (JS expression)".into())
    })?;

    let result = session.cdp.evaluate_js(expression).await.map_err(cdp_err)?;

    // Extract the return value
    let value = result
        .get("result")
        .and_then(|r| r.get("value"))
        .cloned()
        .unwrap_or(Value::Null);

    let exception = result
        .get("exceptionDetails")
        .and_then(|e| e.get("text"))
        .and_then(|t| t.as_str());

    if let Some(err) = exception {
        Ok(json!({"status": "error", "error": err}))
    } else {
        Ok(json!({"status": "ok", "result": value}))
    }
}

async fn action_get_content(session: &mut BrowserSession) -> Result<Value> {
    let current_url = session
        .current_url
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    tracing::info!(
        session = %session.name,
        url = %current_url,
        "browse.get_content start"
    );

    // First try to get the full HTML for markdown conversion
    let html_result = session
        .cdp
        .evaluate_js("document.documentElement.outerHTML")
        .await
        .map_err(cdp_err)?;

    let html = html_result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    tracing::info!(
        session = %session.name,
        url = %current_url,
        html_len = html.len(),
        "browse.get_content html extracted"
    );

    if !html.is_empty() {
        // Convert HTML to markdown for much better token efficiency
        let markdown = crate::html_to_md::html_to_markdown(html);

        tracing::info!(
            session = %session.name,
            url = %current_url,
            html_len = html.len(),
            markdown_len = markdown.len(),
            "browse.get_content markdown converted"
        );

        let truncated = if markdown.len() > 50000 {
            format!(
                "{}...\n[truncated, {} total chars]",
                crate::safe_truncate(&markdown, 50000),
                markdown.len()
            )
        } else {
            markdown.clone()
        };

        return Ok(json!({
            "content": truncated,
            "format": "markdown",
            "length": markdown.len(),
        }));
    }

    // Fallback: plain text extraction
    let result = session
        .cdp
        .evaluate_js("document.body ? document.body.innerText : document.documentElement.innerText")
        .await
        .map_err(cdp_err)?;

    let text = result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    tracing::info!(
        session = %session.name,
        url = %current_url,
        text_len = text.len(),
        "browse.get_content text fallback extracted"
    );

    let truncated = if text.len() > 50000 {
        format!(
            "{}...\n[truncated, {} total chars]",
            crate::safe_truncate(text, 50000),
            text.len()
        )
    } else {
        text.to_string()
    };

    Ok(json!({
        "content": truncated,
        "format": "text",
        "length": text.len(),
    }))
}

async fn action_get_url(session: &mut BrowserSession) -> Result<Value> {
    let result = session
        .cdp
        .evaluate_js("window.location.href")
        .await
        .map_err(cdp_err)?;

    let url = result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    Ok(json!({"url": url}))
}

async fn action_cookies_get(session: &mut BrowserSession) -> Result<Value> {
    let result = session.cdp.get_cookies().await.map_err(cdp_err)?;
    Ok(json!({"cookies": result.get("cookies").cloned().unwrap_or(Value::Null)}))
}

async fn action_cookies_set(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let name = params["cookie_name"]
        .as_str()
        .ok_or_else(|| blockcell_core::Error::Tool("cookies_set requires 'cookie_name'".into()))?;
    let value = params["cookie_value"]
        .as_str()
        .ok_or_else(|| blockcell_core::Error::Tool("cookies_set requires 'cookie_value'".into()))?;
    let domain = params["cookie_domain"].as_str().ok_or_else(|| {
        blockcell_core::Error::Tool("cookies_set requires 'cookie_domain'".into())
    })?;

    session
        .cdp
        .set_cookie(name, value, domain)
        .await
        .map_err(cdp_err)?;
    Ok(json!({"status": "cookie_set", "name": name}))
}

async fn action_cookies_clear(session: &mut BrowserSession) -> Result<Value> {
    session.cdp.clear_cookies().await.map_err(cdp_err)?;
    Ok(json!({"status": "cookies_cleared"}))
}

async fn action_set_viewport(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let width = params["width"].as_i64().unwrap_or(1280) as i32;
    let height = params["height"].as_i64().unwrap_or(720) as i32;
    session
        .cdp
        .set_viewport(width, height, 1.0)
        .await
        .map_err(cdp_err)?;
    Ok(json!({"status": "viewport_set", "width": width, "height": height}))
}

async fn action_set_headers(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let headers = params["headers"].clone();
    session
        .cdp
        .set_extra_headers(headers.clone())
        .await
        .map_err(cdp_err)?;
    Ok(json!({"status": "headers_set", "headers": headers}))
}

async fn action_history(session: &mut BrowserSession, direction: &str) -> Result<Value> {
    let js = if direction == "back" {
        "history.back()"
    } else {
        "history.forward()"
    };
    session.cdp.evaluate_js(js).await.map_err(cdp_err)?;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    Ok(json!({"status": direction}))
}

async fn action_reload(session: &mut BrowserSession) -> Result<Value> {
    session
        .cdp
        .send_command("Page.reload", json!({}))
        .await
        .map_err(cdp_err)?;
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    Ok(json!({"status": "reloaded"}))
}

// ─── Tab management ───────────────────────────────────────────────────

async fn action_tab_list(session: &mut BrowserSession) -> Result<Value> {
    let targets = session.cdp.get_targets().await.map_err(cdp_err)?;
    let pages: Vec<Value> = targets
        .iter()
        .enumerate()
        .filter(|(_, t)| t.get("type").and_then(|v| v.as_str()) == Some("page"))
        .map(|(i, t)| {
            json!({
                "index": i,
                "target_id": t.get("targetId").and_then(|v| v.as_str()).unwrap_or(""),
                "url": t.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                "title": t.get("title").and_then(|v| v.as_str()).unwrap_or(""),
            })
        })
        .collect();
    Ok(json!({"tabs": pages, "count": pages.len()}))
}

async fn action_tab_new(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let url = params["url"].as_str().unwrap_or("about:blank");
    let target_id = session.cdp.create_target(url).await.map_err(cdp_err)?;
    // Brief wait for tab to load
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    Ok(json!({"status": "tab_created", "target_id": target_id, "url": url}))
}

async fn action_tab_close(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let target_id = if let Some(id) = params["tab_id"].as_str() {
        id.to_string()
    } else if let Some(index) = params["tab_index"].as_u64() {
        // Resolve index to target_id
        let targets = session.cdp.get_targets().await.map_err(cdp_err)?;
        let pages: Vec<&Value> = targets
            .iter()
            .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            .collect();
        let idx = index as usize;
        if idx >= pages.len() {
            return Err(blockcell_core::Error::Tool(format!(
                "Tab index {} out of range (have {} tabs)",
                idx,
                pages.len()
            )));
        }
        pages[idx]
            .get("targetId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| blockcell_core::Error::Tool("No targetId for tab".into()))?
            .to_string()
    } else {
        return Err(blockcell_core::Error::Tool(
            "tab_close requires 'tab_id' or 'tab_index'".into(),
        ));
    };

    session
        .cdp
        .close_target(&target_id)
        .await
        .map_err(cdp_err)?;
    Ok(json!({"status": "tab_closed", "target_id": target_id}))
}

async fn action_tab_switch(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let target_id = if let Some(id) = params["tab_id"].as_str() {
        id.to_string()
    } else if let Some(index) = params["tab_index"].as_u64() {
        let targets = session.cdp.get_targets().await.map_err(cdp_err)?;
        let pages: Vec<&Value> = targets
            .iter()
            .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            .collect();
        let idx = index as usize;
        if idx >= pages.len() {
            return Err(blockcell_core::Error::Tool(format!(
                "Tab index {} out of range (have {} tabs)",
                idx,
                pages.len()
            )));
        }
        pages[idx]
            .get("targetId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| blockcell_core::Error::Tool("No targetId for tab".into()))?
            .to_string()
    } else {
        return Err(blockcell_core::Error::Tool(
            "tab_switch requires 'tab_id' or 'tab_index'".into(),
        ));
    };

    session
        .cdp
        .activate_target(&target_id)
        .await
        .map_err(cdp_err)?;
    Ok(json!({"status": "tab_switched", "target_id": target_id}))
}

// ─── File upload ──────────────────────────────────────────────────────

async fn action_upload_file(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let files: Vec<String> = params["files"]
        .as_array()
        .ok_or_else(|| blockcell_core::Error::Tool("upload_file requires 'files' array".into()))?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    if files.is_empty() {
        return Err(blockcell_core::Error::Tool(
            "upload_file: 'files' array is empty".into(),
        ));
    }

    // Verify files exist
    for f in &files {
        if !std::path::Path::new(f).exists() {
            return Err(blockcell_core::Error::Tool(format!(
                "File not found: {}",
                f
            )));
        }
    }

    let (method, target) = resolve_element_target(params)?;

    match method {
        "ref" => {
            let ref_data = session.refs.get(&target).cloned().ok_or_else(|| {
                blockcell_core::Error::Tool(format!("Ref '{}' not found", target))
            })?;
            let backend_node_id = ref_data["backendNodeId"]
                .as_i64()
                .ok_or_else(|| blockcell_core::Error::Tool("Ref has no backendNodeId".into()))?;
            session
                .cdp
                .set_file_input_files(files.clone(), backend_node_id)
                .await
                .map_err(cdp_err)?;
        }
        "selector" => {
            // Find the file input element by selector, resolve to backendNodeId
            let doc = session.cdp.get_document().await.map_err(cdp_err)?;
            let root_id = doc
                .get("root")
                .and_then(|r| r.get("nodeId"))
                .and_then(|v| v.as_i64())
                .unwrap_or(1);
            let node_ids = session
                .cdp
                .query_selector_all(root_id, &target)
                .await
                .map_err(cdp_err)?;
            if node_ids.is_empty() {
                return Err(blockcell_core::Error::Tool(format!(
                    "File input not found: {}",
                    target
                )));
            }
            // Resolve nodeId to objectId, then use setFileInputFiles
            let object_id = session
                .cdp
                .resolve_node(node_ids[0])
                .await
                .map_err(cdp_err)?;
            session
                .cdp
                .set_file_input_files_by_object(files.clone(), &object_id)
                .await
                .map_err(cdp_err)?;
        }
        _ => unreachable!(),
    }

    // Dispatch change event
    session.cdp.evaluate_js(
        "document.activeElement && document.activeElement.dispatchEvent(new Event('change', {bubbles: true}))"
    ).await.map_err(cdp_err)?;

    Ok(json!({"status": "files_uploaded", "files": files, "count": files.len()}))
}

// ─── Dialog handling ──────────────────────────────────────────────────

async fn action_dialog_handle(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let accept = params["accept"].as_bool().unwrap_or(true);
    let prompt_text = params["prompt_text"].as_str();

    session
        .cdp
        .handle_dialog(accept, prompt_text)
        .await
        .map_err(cdp_err)?;

    Ok(json!({
        "status": if accept { "dialog_accepted" } else { "dialog_dismissed" },
        "accept": accept,
        "prompt_text": prompt_text,
    }))
}

// ─── Network interception ─────────────────────────────────────────────

async fn action_network_intercept(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let url_pattern = params["url_pattern"].as_str().unwrap_or("*");

    let patterns = vec![json!({
        "urlPattern": url_pattern,
        "requestStage": "Request",
    })];

    session.cdp.enable_fetch(patterns).await.map_err(cdp_err)?;

    Ok(json!({
        "status": "interception_enabled",
        "url_pattern": url_pattern,
        "note": "Paused requests will appear as events. Use network_continue or network_block with the request_id to handle them.",
    }))
}

async fn action_network_continue(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let request_id = params["request_id"].as_str().ok_or_else(|| {
        blockcell_core::Error::Tool("network_continue requires 'request_id'".into())
    })?;

    // Check if this is a fulfill (has response_code) or a simple continue
    if let Some(response_code) = params["response_code"].as_i64() {
        let headers = params["headers"].as_array().map(|arr| arr.to_vec());
        let body = params["body"].as_str();
        session
            .cdp
            .fetch_fulfill(request_id, response_code as i32, headers, body)
            .await
            .map_err(cdp_err)?;
        Ok(
            json!({"status": "request_fulfilled", "request_id": request_id, "response_code": response_code}),
        )
    } else {
        let url = params["url"].as_str();
        let method = params["method"].as_str();
        let headers = params["headers"].as_array().map(|arr| arr.to_vec());
        let post_data = params["body"].as_str();
        session
            .cdp
            .fetch_continue(request_id, url, method, headers, post_data)
            .await
            .map_err(cdp_err)?;
        Ok(json!({"status": "request_continued", "request_id": request_id}))
    }
}

async fn action_network_block(session: &mut BrowserSession, params: &Value) -> Result<Value> {
    let request_id = params["request_id"]
        .as_str()
        .ok_or_else(|| blockcell_core::Error::Tool("network_block requires 'request_id'".into()))?;

    let reason = params["reason"].as_str().unwrap_or("BlockedByClient");
    session
        .cdp
        .fetch_fail(request_id, reason)
        .await
        .map_err(cdp_err)?;

    Ok(json!({"status": "request_blocked", "request_id": request_id, "reason": reason}))
}

// ─── Helper functions ─────────────────────────────────────────────────

/// Take an accessibility snapshot, assign refs, return structured result.
async fn take_snapshot(session: &mut BrowserSession, compact: bool) -> Result<Value> {
    let ax_tree = session
        .cdp
        .get_accessibility_tree()
        .await
        .map_err(cdp_err)?;

    let mut nodes = parse_ax_tree(&ax_tree);

    // Reset ref counter for fresh snapshot
    session.ref_counter = 0;
    session.refs.clear();

    let (new_counter, ref_map) = assign_refs(&mut nodes, 0, false);
    session.ref_counter = new_counter;
    session.refs = ref_map.clone();

    let tree_text = render_tree(&nodes, compact, Some(15));
    let result = snapshot_to_json(&tree_text, &ref_map);

    Ok(result)
}

mod tool_helpers;
use tool_helpers::{
    cdp_err, click_by_backend_node, click_by_selector, focus_by_backend_node, focus_by_selector,
    parse_key_spec, resolve_element_target,
};

#[cfg(test)]
mod tests {
    use super::tool_helpers::extract_center_from_box_model;
    use super::*;

    #[test]
    fn test_parse_key_spec() {
        let (key, code, mods) = parse_key_spec("Enter");
        assert_eq!(key, "Enter");
        assert_eq!(code, "Enter");
        assert_eq!(mods, 0);

        let (key, code, mods) = parse_key_spec("Ctrl+A");
        assert_eq!(key, "A");
        assert_eq!(code, "KeyA");
        assert_eq!(mods, 2); // Ctrl

        let (key, code, mods) = parse_key_spec("Ctrl+Shift+Tab");
        assert_eq!(key, "Tab");
        assert_eq!(code, "Tab");
        assert_eq!(mods, 10); // Ctrl(2) + Shift(8)
    }

    #[test]
    fn test_resolve_element_target() {
        let params = json!({"ref": "e5"});
        let (method, target) = resolve_element_target(&params).unwrap();
        assert_eq!(method, "ref");
        assert_eq!(target, "e5");

        let params = json!({"selector": "#search"});
        let (method, target) = resolve_element_target(&params).unwrap();
        assert_eq!(method, "selector");
        assert_eq!(target, "#search");

        let params = json!({});
        assert!(resolve_element_target(&params).is_err());
    }

    #[test]
    fn test_extract_center() {
        let bm = json!({
            "model": {
                "content": [10.0, 20.0, 110.0, 20.0, 110.0, 60.0, 10.0, 60.0]
            }
        });
        let (x, y) = extract_center_from_box_model(&bm);
        assert!((x - 60.0).abs() < 0.01);
        assert!((y - 40.0).abs() < 0.01);
    }

    #[test]
    fn test_browser_engine_from_str() {
        assert_eq!(
            "chrome".parse::<BrowserEngine>().unwrap(),
            BrowserEngine::Chrome
        );
        assert_eq!(
            "Chrome".parse::<BrowserEngine>().unwrap(),
            BrowserEngine::Chrome
        );
        assert_eq!(
            "firefox".parse::<BrowserEngine>().unwrap(),
            BrowserEngine::Firefox
        );
        assert_eq!(
            "ff".parse::<BrowserEngine>().unwrap(),
            BrowserEngine::Firefox
        );
        assert_eq!(
            "edge".parse::<BrowserEngine>().unwrap(),
            BrowserEngine::Edge
        );
        assert_eq!(
            "msedge".parse::<BrowserEngine>().unwrap(),
            BrowserEngine::Edge
        );
        assert_eq!(
            "unknown".parse::<BrowserEngine>().unwrap(),
            BrowserEngine::Chrome
        ); // default
    }

    #[test]
    fn test_browser_engine_name() {
        assert_eq!(BrowserEngine::Chrome.name(), "chrome");
        assert_eq!(BrowserEngine::Firefox.name(), "firefox");
        assert_eq!(BrowserEngine::Edge.name(), "edge");
    }
}
