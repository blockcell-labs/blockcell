//! BrowseTool — unified browser automation tool with CDP backend.

use async_trait::async_trait;
use blockcell_core::Result;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::{Tool, ToolContext, ToolSchema};
use super::session::{SessionManager, BrowserEngine, list_available_browsers};
use super::snapshot::{assign_refs, parse_ax_tree, render_tree, snapshot_to_json};

/// Global session manager (daemon model — persists across tool calls).
static SESSION_MANAGER: once_cell::sync::Lazy<Arc<Mutex<Option<SessionManager>>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(None)));

async fn ensure_manager(workspace: &std::path::Path) -> Arc<Mutex<Option<SessionManager>>> {
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

pub struct BrowseTool;

#[async_trait]
impl Tool for BrowseTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browse",
            description: "Browser automation via Chrome DevTools Protocol. Supports persistent sessions, accessibility snapshots with element refs (@e1, @e2...), click, fill, type, scroll, wait, screenshot, PDF, cookies, tabs, and more.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "navigate", "snapshot", "click", "fill", "type_text",
                            "press_key", "scroll", "wait", "screenshot", "pdf",
                            "execute_js", "get_content", "get_url",
                            "cookies_get", "cookies_set", "cookies_clear",
                            "tab_list", "tab_new", "tab_close", "tab_switch",
                            "session_list", "session_close",
                            "set_viewport", "set_headers",
                            "back", "forward", "reload",
                            "upload_file", "dialog_handle",
                            "network_intercept", "network_continue", "network_block",
                            "list_browsers"
                        ],
                        "description": "Browser action: 'navigate'=open URL (requires url param); 'snapshot'=get accessibility tree of current page (read page structure/links/text); 'get_content'=get full page text as markdown; 'screenshot'=capture page image (requires output_path); 'click'=click element (requires ref or selector); 'fill'=fill input field (requires ref/selector + text); 'type_text'=type into focused element; 'press_key'=press keyboard key; 'scroll'=scroll page; 'wait'=wait for element or time; 'execute_js'=run JavaScript; 'get_url'=get current URL; 'tab_list'=list open tabs; 'tab_new'=open new tab; 'tab_close'=close tab; 'tab_switch'=switch tab; 'back'/'forward'/'reload'=navigation; 'cookies_get'/'cookies_set'/'cookies_clear'=cookie ops; 'session_list'/'session_close'=session management; 'upload_file'=file upload; 'dialog_handle'=handle JS dialogs; 'network_intercept'/'network_continue'/'network_block'=network control; 'pdf'=save page as PDF; 'set_viewport'=set window size; 'set_headers'=set HTTP headers; 'list_browsers'=list available browsers. ALWAYS specify action explicitly."
                    },
                    "url": {
                        "type": "string",
                        "description": "URL to navigate to (for 'navigate' action)"
                    },
                    "new_tab": {
                        "type": "boolean",
                        "description": "For 'navigate': open in a new tab and switch CDP session to it (default: false)"
                    },
                    "ref": {
                        "type": "string",
                        "description": "Element ref from snapshot (e.g. 'e1', 'e5') for click/fill/type actions"
                    },
                    "selector": {
                        "type": "string",
                        "description": "CSS selector (fallback if no ref). For click/fill/type/scroll/wait actions"
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to fill/type, or JS expression for execute_js"
                    },
                    "key": {
                        "type": "string",
                        "description": "Key to press (e.g. 'Enter', 'Tab', 'Escape', 'ArrowDown')"
                    },
                    "direction": {
                        "type": "string",
                        "enum": ["up", "down", "left", "right"],
                        "description": "Scroll direction (default: 'down')"
                    },
                    "amount": {
                        "type": "integer",
                        "description": "Scroll amount in pixels (default: 400)"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Wait timeout in ms (for 'wait' action, default: 5000)"
                    },
                    "wait_for": {
                        "type": "string",
                        "description": "CSS selector to wait for (for 'wait' action)"
                    },
                    "session": {
                        "type": "string",
                        "description": "Session name (default: 'default'). Each session is an isolated browser."
                    },
                    "headed": {
                        "type": "boolean",
                        "description": "Launch visible browser (default: false = headless)"
                    },
                    "full_page": {
                        "type": "boolean",
                        "description": "Full page screenshot (default: false)"
                    },
                    "output_path": {
                        "type": "string",
                        "description": "File path for screenshot/PDF output"
                    },
                    "compact": {
                        "type": "boolean",
                        "description": "Compact snapshot (skip empty structural nodes, default: true)"
                    },
                    "cookie_name": { "type": "string" },
                    "cookie_value": { "type": "string" },
                    "cookie_domain": { "type": "string" },
                    "width": { "type": "integer", "description": "Viewport width" },
                    "height": { "type": "integer", "description": "Viewport height" },
                    "headers": {
                        "type": "object",
                        "description": "Extra HTTP headers to set"
                    },
                    "tab_index": {
                        "type": "integer",
                        "description": "Tab index for tab_switch/tab_close"
                    },
                    "tab_id": {
                        "type": "string",
                        "description": "Target ID for tab_switch/tab_close (alternative to tab_index)"
                    },
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "File paths for upload_file action"
                    },
                    "accept": {
                        "type": "boolean",
                        "description": "Accept or dismiss dialog (for dialog_handle, default: true)"
                    },
                    "prompt_text": {
                        "type": "string",
                        "description": "Text to enter in prompt dialog"
                    },
                    "url_pattern": {
                        "type": "string",
                        "description": "URL pattern for network_intercept (glob, e.g. '*api*')"
                    },
                    "request_id": {
                        "type": "string",
                        "description": "Request ID for network_continue/network_block"
                    },
                    "response_code": {
                        "type": "integer",
                        "description": "HTTP response code for network_continue fulfill"
                    },
                    "body": {
                        "type": "string",
                        "description": "Response body for network_continue fulfill"
                    },
                    "browser": {
                        "type": "string",
                        "enum": ["chrome", "edge", "firefox"],
                        "description": "Browser engine to use (default: chrome)"
                    }
                },
                "required": []
            }),
        }
    }

    fn validate(&self, _params: &Value) -> Result<()> {
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap_or_else(|| {
            if params.get("url").and_then(|v| v.as_str()).is_some() { "navigate" } else { "snapshot" }
        });
        let session_name = params["session"].as_str().unwrap_or("default");
        let headed = params["headed"].as_bool().unwrap_or(false);
        let engine = params["browser"].as_str()
            .map(BrowserEngine::from_str)
            .unwrap_or(BrowserEngine::Chrome);

        let workspace = ctx.workspace.clone();
        let mgr_arc = ensure_manager(&workspace).await;
        let mut mgr_guard = mgr_arc.lock().await;
        let mgr = mgr_guard.as_mut().unwrap();

        // Actions that don't need a session
        match action {
            "session_list" => {
                let sessions = mgr.list_sessions();
                return Ok(json!({
                    "sessions": sessions,
                    "count": sessions.len(),
                }));
            }
            "session_close" => {
                mgr.close_session(session_name).await.map_err(|e| {
                    blockcell_core::Error::Tool(format!("session_close: {}", e))
                })?;
                return Ok(json!({"status": "closed", "session": session_name}));
            }
            "list_browsers" => {
                let browsers = list_available_browsers();
                let list: Vec<Value> = browsers.iter().map(|(e, p)| json!({"engine": e.name(), "path": p})).collect();
                return Ok(json!({"browsers": list, "count": list.len()}));
            }
            _ => {}
        }

        // Get or create session with specified engine
        let session = mgr
            .get_or_create_with_engine(session_name, headed, None, engine)
            .await
            .map_err(|e| blockcell_core::Error::Tool(format!("session error: {}", e)))?;

        match action {
            "navigate" => action_navigate(session, &params).await,
            "snapshot" => action_snapshot(session, &params).await,
            "click" => action_click(session, &params).await,
            "fill" => action_fill(session, &params).await,
            "type_text" => action_type_text(session, &params).await,
            "press_key" => action_press_key(session, &params).await,
            "scroll" => action_scroll(session, &params).await,
            "wait" => action_wait(session, &params).await,
            "screenshot" => action_screenshot(session, &params, &workspace).await,
            "pdf" => action_pdf(session, &params, &workspace).await,
            "execute_js" => action_execute_js(session, &params).await,
            "get_content" => action_get_content(session).await,
            "get_url" => action_get_url(session).await,
            "cookies_get" => action_cookies_get(session).await,
            "cookies_set" => action_cookies_set(session, &params).await,
            "cookies_clear" => action_cookies_clear(session).await,
            "set_viewport" => action_set_viewport(session, &params).await,
            "set_headers" => action_set_headers(session, &params).await,
            "back" => action_history(session, "back").await,
            "forward" => action_history(session, "forward").await,
            "reload" => action_reload(session).await,
            "tab_list" => action_tab_list(session).await,
            "tab_new" => action_tab_new(session, &params).await,
            "tab_close" => action_tab_close(session, &params).await,
            "tab_switch" => action_tab_switch(session, &params).await,
            "upload_file" => action_upload_file(session, &params).await,
            "dialog_handle" => action_dialog_handle(session, &params).await,
            "network_intercept" => action_network_intercept(session, &params).await,
            "network_continue" => action_network_continue(session, &params).await,
            "network_block" => action_network_block(session, &params).await,
            _ => Err(blockcell_core::Error::Tool(format!(
                "Unknown browse action: {}",
                action
            ))),
        }
    }
}

use super::session::BrowserSession;
use super::session::get_target_ws_url;

// ─── Action implementations ───────────────────────────────────────────

async fn action_navigate(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
    let url = params["url"]
        .as_str()
        .ok_or_else(|| blockcell_core::Error::Tool("navigate requires 'url'".into()))?;

    let new_tab = params["new_tab"].as_bool().unwrap_or(false);

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
        session.cdp.enable_domain("Runtime").await.map_err(cdp_err)?;
        session.cdp.enable_domain("DOM").await.map_err(cdp_err)?;
        session.cdp.enable_domain("Network").await.map_err(cdp_err)?;
        session.cdp.enable_domain("Accessibility").await.map_err(cdp_err)?;
        let _ = session.cdp.enable_domain("Target").await;

        session.cdp.navigate(url).await.map_err(cdp_err)?;
        session.current_url = Some(url.to_string());

        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let snap = take_snapshot(session, true).await?;

        return Ok(json!({
            "status": "navigated",
            "url": url,
            "new_tab": true,
            "target_id": target_id,
            "snapshot": snap.get("snapshot"),
            "ref_count": snap.get("ref_count"),
        }));
    }

    session.cdp.navigate(url).await.map_err(cdp_err)?;
    session.current_url = Some(url.to_string());

    // Wait for page load
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    // Auto-snapshot after navigation
    let snap = take_snapshot(session, true).await?;

    Ok(json!({
        "status": "navigated",
        "url": url,
        "snapshot": snap.get("snapshot"),
        "ref_count": snap.get("ref_count"),
    }))
}

async fn action_snapshot(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
    let compact = params["compact"].as_bool().unwrap_or(true);
    take_snapshot(session, compact).await
}

async fn action_click(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
    // Resolve element: by ref or by selector
    let (method, target) = resolve_element_target(params)?;

    match method {
        "ref" => {
            let ref_data = session.refs.get(&target).cloned().ok_or_else(|| {
                blockcell_core::Error::Tool(format!("Ref '{}' not found. Take a snapshot first.", target))
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

async fn action_fill(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
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
            let backend_node_id = ref_data["backendNodeId"].as_i64().ok_or_else(|| {
                blockcell_core::Error::Tool("Ref has no backendNodeId".into())
            })?;
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

async fn action_type_text(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
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

async fn action_press_key(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
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

async fn action_scroll(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
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

async fn action_wait(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
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
            if let Some(true) = result.get("result").and_then(|r| r.get("value")).and_then(|v| v.as_bool()) {
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
    std::fs::create_dir_all(&media_dir).ok();
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let workspace_path = media_dir.join(format!("screenshot_{}.png", ts));

    let user_path = params["output_path"].as_str().map(|p| {
        let pb = if p.starts_with("~/") {
            dirs::home_dir().map(|h| h.join(&p[2..])).unwrap_or_else(|| std::path::PathBuf::from(p))
        } else {
            std::path::PathBuf::from(p)
        };
        pb
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
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::copy(&workspace_path, up).ok();
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
    std::fs::create_dir_all(&media_dir).ok();
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let workspace_path = media_dir.join(format!("page_{}.pdf", ts));

    let user_path = params["output_path"].as_str().map(|p| {
        if p.starts_with("~/") {
            dirs::home_dir().map(|h| h.join(&p[2..])).unwrap_or_else(|| std::path::PathBuf::from(p))
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
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::copy(&workspace_path, up).ok();
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

async fn action_execute_js(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
    let expression = params["text"]
        .as_str()
        .ok_or_else(|| blockcell_core::Error::Tool("execute_js requires 'text' (JS expression)".into()))?;

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

    if !html.is_empty() {
        // Convert HTML to markdown for much better token efficiency
        let markdown = crate::html_to_md::html_to_markdown(html);

        let truncated = if markdown.len() > 50000 {
            format!("{}...\n[truncated, {} total chars]", crate::safe_truncate(&markdown, 50000), markdown.len())
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

    let truncated = if text.len() > 50000 {
        format!("{}...\n[truncated, {} total chars]", crate::safe_truncate(text, 50000), text.len())
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

async fn action_cookies_set(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
    let name = params["cookie_name"]
        .as_str()
        .ok_or_else(|| blockcell_core::Error::Tool("cookies_set requires 'cookie_name'".into()))?;
    let value = params["cookie_value"]
        .as_str()
        .ok_or_else(|| blockcell_core::Error::Tool("cookies_set requires 'cookie_value'".into()))?;
    let domain = params["cookie_domain"]
        .as_str()
        .ok_or_else(|| blockcell_core::Error::Tool("cookies_set requires 'cookie_domain'".into()))?;

    session.cdp.set_cookie(name, value, domain).await.map_err(cdp_err)?;
    Ok(json!({"status": "cookie_set", "name": name}))
}

async fn action_cookies_clear(session: &mut BrowserSession) -> Result<Value> {
    session.cdp.clear_cookies().await.map_err(cdp_err)?;
    Ok(json!({"status": "cookies_cleared"}))
}

async fn action_set_viewport(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
    let width = params["width"].as_i64().unwrap_or(1280) as i32;
    let height = params["height"].as_i64().unwrap_or(720) as i32;
    session.cdp.set_viewport(width, height, 1.0).await.map_err(cdp_err)?;
    Ok(json!({"status": "viewport_set", "width": width, "height": height}))
}

async fn action_set_headers(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
    let headers = params["headers"].clone();
    session.cdp.set_extra_headers(headers.clone()).await.map_err(cdp_err)?;
    Ok(json!({"status": "headers_set", "headers": headers}))
}

async fn action_history(
    session: &mut BrowserSession,
    direction: &str,
) -> Result<Value> {
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

async fn action_tab_new(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
    let url = params["url"].as_str().unwrap_or("about:blank");
    let target_id = session.cdp.create_target(url).await.map_err(cdp_err)?;
    // Brief wait for tab to load
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    Ok(json!({"status": "tab_created", "target_id": target_id, "url": url}))
}

async fn action_tab_close(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
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
                "Tab index {} out of range (have {} tabs)", idx, pages.len()
            )));
        }
        pages[idx].get("targetId").and_then(|v| v.as_str())
            .ok_or_else(|| blockcell_core::Error::Tool("No targetId for tab".into()))?
            .to_string()
    } else {
        return Err(blockcell_core::Error::Tool("tab_close requires 'tab_id' or 'tab_index'".into()));
    };

    session.cdp.close_target(&target_id).await.map_err(cdp_err)?;
    Ok(json!({"status": "tab_closed", "target_id": target_id}))
}

async fn action_tab_switch(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
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
                "Tab index {} out of range (have {} tabs)", idx, pages.len()
            )));
        }
        pages[idx].get("targetId").and_then(|v| v.as_str())
            .ok_or_else(|| blockcell_core::Error::Tool("No targetId for tab".into()))?
            .to_string()
    } else {
        return Err(blockcell_core::Error::Tool("tab_switch requires 'tab_id' or 'tab_index'".into()));
    };

    session.cdp.activate_target(&target_id).await.map_err(cdp_err)?;
    Ok(json!({"status": "tab_switched", "target_id": target_id}))
}

// ─── File upload ──────────────────────────────────────────────────────

async fn action_upload_file(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
    let files: Vec<String> = params["files"]
        .as_array()
        .ok_or_else(|| blockcell_core::Error::Tool("upload_file requires 'files' array".into()))?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    if files.is_empty() {
        return Err(blockcell_core::Error::Tool("upload_file: 'files' array is empty".into()));
    }

    // Verify files exist
    for f in &files {
        if !std::path::Path::new(f).exists() {
            return Err(blockcell_core::Error::Tool(format!("File not found: {}", f)));
        }
    }

    let (method, target) = resolve_element_target(params)?;

    match method {
        "ref" => {
            let ref_data = session.refs.get(&target).cloned().ok_or_else(|| {
                blockcell_core::Error::Tool(format!("Ref '{}' not found", target))
            })?;
            let backend_node_id = ref_data["backendNodeId"].as_i64().ok_or_else(|| {
                blockcell_core::Error::Tool("Ref has no backendNodeId".into())
            })?;
            session.cdp.set_file_input_files(files.clone(), backend_node_id)
                .await.map_err(cdp_err)?;
        }
        "selector" => {
            // Find the file input element by selector, resolve to backendNodeId
            let doc = session.cdp.get_document().await.map_err(cdp_err)?;
            let root_id = doc.get("root").and_then(|r| r.get("nodeId")).and_then(|v| v.as_i64()).unwrap_or(1);
            let node_ids = session.cdp.query_selector_all(root_id, &target).await.map_err(cdp_err)?;
            if node_ids.is_empty() {
                return Err(blockcell_core::Error::Tool(format!("File input not found: {}", target)));
            }
            // Resolve nodeId to objectId, then use setFileInputFiles
            let object_id = session.cdp.resolve_node(node_ids[0]).await.map_err(cdp_err)?;
            session.cdp.set_file_input_files_by_object(files.clone(), &object_id)
                .await.map_err(cdp_err)?;
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

async fn action_dialog_handle(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
    let accept = params["accept"].as_bool().unwrap_or(true);
    let prompt_text = params["prompt_text"].as_str();

    session.cdp.handle_dialog(accept, prompt_text).await.map_err(cdp_err)?;

    Ok(json!({
        "status": if accept { "dialog_accepted" } else { "dialog_dismissed" },
        "accept": accept,
        "prompt_text": prompt_text,
    }))
}

// ─── Network interception ─────────────────────────────────────────────

async fn action_network_intercept(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
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

async fn action_network_continue(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
    let request_id = params["request_id"]
        .as_str()
        .ok_or_else(|| blockcell_core::Error::Tool("network_continue requires 'request_id'".into()))?;

    // Check if this is a fulfill (has response_code) or a simple continue
    if let Some(response_code) = params["response_code"].as_i64() {
        let headers = params["headers"].as_array().map(|arr| arr.to_vec());
        let body = params["body"].as_str();
        session.cdp.fetch_fulfill(request_id, response_code as i32, headers, body)
            .await.map_err(cdp_err)?;
        Ok(json!({"status": "request_fulfilled", "request_id": request_id, "response_code": response_code}))
    } else {
        let url = params["url"].as_str();
        let method = params["method"].as_str();
        let headers = params["headers"].as_array().map(|arr| arr.to_vec());
        let post_data = params["body"].as_str();
        session.cdp.fetch_continue(request_id, url, method, headers, post_data)
            .await.map_err(cdp_err)?;
        Ok(json!({"status": "request_continued", "request_id": request_id}))
    }
}

async fn action_network_block(
    session: &mut BrowserSession,
    params: &Value,
) -> Result<Value> {
    let request_id = params["request_id"]
        .as_str()
        .ok_or_else(|| blockcell_core::Error::Tool("network_block requires 'request_id'".into()))?;

    let reason = params["reason"].as_str().unwrap_or("BlockedByClient");
    session.cdp.fetch_fail(request_id, reason).await.map_err(cdp_err)?;

    Ok(json!({"status": "request_blocked", "request_id": request_id, "reason": reason}))
}

// ─── Helper functions ─────────────────────────────────────────────────

/// Take an accessibility snapshot, assign refs, return structured result.
async fn take_snapshot(
    session: &mut BrowserSession,
    compact: bool,
) -> Result<Value> {
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

/// Resolve element target from params: either "ref" or "selector".
fn resolve_element_target(params: &Value) -> std::result::Result<(&'static str, String), blockcell_core::Error> {
    if let Some(r) = params["ref"].as_str() {
        // Strip leading '@' or 'e' prefix normalization
        let ref_id = r.trim_start_matches('@');
        Ok(("ref", ref_id.to_string()))
    } else if let Some(s) = params["selector"].as_str() {
        Ok(("selector", s.to_string()))
    } else {
        Err(blockcell_core::Error::Tool(
            "Action requires 'ref' (from snapshot) or 'selector' (CSS)".into(),
        ))
    }
}

/// Click an element by its backendNodeId.
async fn click_by_backend_node(
    session: &mut BrowserSession,
    backend_node_id: i64,
) -> Result<()> {
    // Resolve to a remote object
    let result = session
        .cdp
        .send_command(
            "DOM.resolveNode",
            json!({"backendNodeId": backend_node_id}),
        )
        .await
        .map_err(cdp_err)?;

    let object_id = result
        .get("object")
        .and_then(|o| o.get("objectId"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| blockcell_core::Error::Tool("Failed to resolve node for click".into()))?;

    // Scroll into view and get coordinates
    let box_result = session
        .cdp
        .send_command(
            "DOM.getBoxModel",
            json!({"backendNodeId": backend_node_id}),
        )
        .await;

    let (x, y) = if let Ok(bm) = box_result {
        extract_center_from_box_model(&bm)
    } else {
        // Fallback: scroll into view via JS and use a default click
        session
            .cdp
            .call_function_on(
                object_id,
                "function() { this.scrollIntoView({block: 'center'}); this.click(); }",
            )
            .await
            .map_err(cdp_err)?;
        return Ok(());
    };

    // Dispatch mouse events at center of element
    session
        .cdp
        .dispatch_mouse_event("mousePressed", x, y, "left", 1)
        .await
        .map_err(cdp_err)?;
    session
        .cdp
        .dispatch_mouse_event("mouseReleased", x, y, "left", 1)
        .await
        .map_err(cdp_err)?;

    Ok(())
}

/// Click an element by CSS selector.
async fn click_by_selector(
    session: &mut BrowserSession,
    selector: &str,
) -> Result<()> {
    let escaped = selector.replace('\\', "\\\\").replace('\'', "\\'");
    let js = format!(
        concat!(
            "(function() {{ var el = document.querySelector('{}');",
            " if (!el) return false;",
            " el.scrollIntoView({{block: 'center'}});",
            " el.click(); return true; }})()"
        ),
        escaped
    );

    let result = session.cdp.evaluate_js(&js).await.map_err(cdp_err)?;
    let clicked = result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !clicked {
        return Err(blockcell_core::Error::Tool(format!(
            "Element not found: {}",
            selector
        )));
    }
    Ok(())
}

/// Focus an element by backendNodeId.
async fn focus_by_backend_node(
    session: &mut BrowserSession,
    backend_node_id: i64,
) -> Result<()> {
    session
        .cdp
        .send_command("DOM.focus", json!({"backendNodeId": backend_node_id}))
        .await
        .map_err(cdp_err)?;
    Ok(())
}

/// Focus an element by CSS selector.
async fn focus_by_selector(
    session: &mut BrowserSession,
    selector: &str,
) -> Result<()> {
    let js = format!(
        "document.querySelector('{}')?.focus()",
        selector.replace('\'', "\\'")
    );
    session.cdp.evaluate_js(&js).await.map_err(cdp_err)?;
    Ok(())
}

/// Extract center coordinates from a box model response.
fn extract_center_from_box_model(bm: &Value) -> (f64, f64) {
    if let Some(content) = bm.get("model").and_then(|m| m.get("content")).and_then(|c| c.as_array()) {
        if content.len() >= 8 {
            let x1 = content[0].as_f64().unwrap_or(0.0);
            let y1 = content[1].as_f64().unwrap_or(0.0);
            let x2 = content[4].as_f64().unwrap_or(0.0);
            let y2 = content[5].as_f64().unwrap_or(0.0);
            return ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
        }
    }
    (0.0, 0.0)
}

/// Parse a key specification like "Enter", "Tab", "Ctrl+A", etc.
fn parse_key_spec(key: &str) -> (String, String, i32) {
    let parts: Vec<&str> = key.split('+').collect();
    let mut modifiers = 0i32;
    let mut main_key = key.to_string();

    if parts.len() > 1 {
        for &part in &parts[..parts.len() - 1] {
            match part.to_lowercase().as_str() {
                "ctrl" | "control" => modifiers |= 2,
                "alt" | "option" => modifiers |= 1,
                "shift" => modifiers |= 8,
                "meta" | "cmd" | "command" => modifiers |= 4,
                _ => {}
            }
        }
        main_key = parts.last().unwrap_or(&key).to_string();
    }

    let code = match main_key.as_str() {
        "Enter" | "Return" => "Enter",
        "Tab" => "Tab",
        "Escape" | "Esc" => "Escape",
        "Backspace" => "Backspace",
        "Delete" => "Delete",
        "ArrowUp" | "Up" => "ArrowUp",
        "ArrowDown" | "Down" => "ArrowDown",
        "ArrowLeft" | "Left" => "ArrowLeft",
        "ArrowRight" | "Right" => "ArrowRight",
        "Home" => "Home",
        "End" => "End",
        "PageUp" => "PageUp",
        "PageDown" => "PageDown",
        "Space" | " " => "Space",
        _ => {
            if main_key.len() == 1 {
                // Single character
                return (main_key.clone(), format!("Key{}", main_key.to_uppercase()), modifiers);
            }
            &main_key
        }
    }
    .to_string();

    (main_key, code, modifiers)
}

/// Convert CDP error string to blockcell Error.
fn cdp_err(e: String) -> blockcell_core::Error {
    blockcell_core::Error::Tool(format!("CDP: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = BrowseTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "browse");
        assert!(schema.description.contains("Chrome DevTools Protocol"));
    }

    #[test]
    fn test_validate() {
        let tool = BrowseTool;
        assert!(tool.validate(&json!({"action": "navigate"})).is_ok());
        assert!(tool.validate(&json!({})).is_ok());
    }

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
    fn test_schema_has_new_actions() {
        let tool = BrowseTool;
        let schema = tool.schema();
        let params = &schema.parameters;
        let actions = params["properties"]["action"]["enum"].as_array().unwrap();
        let action_strs: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();

        // Tab management
        assert!(action_strs.contains(&"tab_list"));
        assert!(action_strs.contains(&"tab_new"));
        assert!(action_strs.contains(&"tab_close"));
        assert!(action_strs.contains(&"tab_switch"));
        // File upload
        assert!(action_strs.contains(&"upload_file"));
        // Dialog
        assert!(action_strs.contains(&"dialog_handle"));
        // Network interception
        assert!(action_strs.contains(&"network_intercept"));
        assert!(action_strs.contains(&"network_continue"));
        assert!(action_strs.contains(&"network_block"));
        // Multi-browser
        assert!(action_strs.contains(&"list_browsers"));
    }

    #[test]
    fn test_schema_has_new_params() {
        let tool = BrowseTool;
        let schema = tool.schema();
        let props = &schema.parameters["properties"];

        assert!(props.get("tab_id").is_some());
        assert!(props.get("files").is_some());
        assert!(props.get("accept").is_some());
        assert!(props.get("prompt_text").is_some());
        assert!(props.get("url_pattern").is_some());
        assert!(props.get("request_id").is_some());
        assert!(props.get("response_code").is_some());
        assert!(props.get("body").is_some());
        assert!(props.get("browser").is_some());
    }

    #[test]
    fn test_validate_new_actions() {
        let tool = BrowseTool;
        assert!(tool.validate(&json!({"action": "tab_list"})).is_ok());
        assert!(tool.validate(&json!({"action": "upload_file"})).is_ok());
        assert!(tool.validate(&json!({"action": "dialog_handle"})).is_ok());
        assert!(tool.validate(&json!({"action": "network_intercept"})).is_ok());
        assert!(tool.validate(&json!({"action": "list_browsers"})).is_ok());
    }

    #[test]
    fn test_browser_engine_from_str() {
        assert_eq!(BrowserEngine::from_str("chrome"), BrowserEngine::Chrome);
        assert_eq!(BrowserEngine::from_str("Chrome"), BrowserEngine::Chrome);
        assert_eq!(BrowserEngine::from_str("firefox"), BrowserEngine::Firefox);
        assert_eq!(BrowserEngine::from_str("ff"), BrowserEngine::Firefox);
        assert_eq!(BrowserEngine::from_str("edge"), BrowserEngine::Edge);
        assert_eq!(BrowserEngine::from_str("msedge"), BrowserEngine::Edge);
        assert_eq!(BrowserEngine::from_str("unknown"), BrowserEngine::Chrome); // default
    }

    #[test]
    fn test_browser_engine_name() {
        assert_eq!(BrowserEngine::Chrome.name(), "chrome");
        assert_eq!(BrowserEngine::Firefox.name(), "firefox");
        assert_eq!(BrowserEngine::Edge.name(), "edge");
    }
}
