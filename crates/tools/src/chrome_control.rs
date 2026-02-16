use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;
use tracing::{debug, info};

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for controlling a visible Chrome browser window on macOS.
///
/// Uses AppleScript + JavaScript (via `osascript`) to automate a real Chrome
/// window — opening URLs, clicking elements, typing text, pressing keys,
/// scrolling, and taking screenshots. Unlike the headless `browse` tool,
/// this controls a GUI Chrome window with real mouse/keyboard interaction.
pub struct ChromeControlTool;

#[async_trait]
impl Tool for ChromeControlTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "chrome_control",
            description: "Control a visible Google Chrome browser window on macOS. Can open URLs, type text, click elements, press keyboard shortcuts, scroll, read page content, and take screenshots. Uses real GUI automation — the user can see the browser actions happening.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "open", "goto", "type", "click", "press_key",
                            "read", "screenshot", "scroll", "wait",
                            "execute_js", "get_url", "tabs", "new_tab", "close_tab",
                            "find_element"
                        ],
                        "description": "Action to perform on Chrome"
                    },
                    "url": {
                        "type": "string",
                        "description": "URL to navigate to (for 'open' and 'goto' actions)"
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to type (for 'type' action), or key combo (for 'press_key' action, e.g. 'return', 'tab', 'cmd+l', 'cmd+t')"
                    },
                    "selector": {
                        "type": "string",
                        "description": "CSS selector for the target element (for 'click', 'type', 'find_element' actions)"
                    },
                    "javascript": {
                        "type": "string",
                        "description": "JavaScript code to execute in the page (for 'execute_js' action)"
                    },
                    "direction": {
                        "type": "string",
                        "enum": ["up", "down"],
                        "description": "Scroll direction (for 'scroll' action, default: down)"
                    },
                    "amount": {
                        "type": "integer",
                        "description": "Scroll amount in pixels (for 'scroll'), or wait duration in ms (for 'wait'). Default: 500"
                    },
                    "screenshot_path": {
                        "type": "string",
                        "description": "File path to save screenshot (for 'screenshot' action)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid = [
            "open", "goto", "type", "click", "press_key",
            "read", "screenshot", "scroll", "wait",
            "execute_js", "get_url", "tabs", "new_tab", "close_tab",
            "find_element",
        ];
        if !valid.contains(&action) {
            return Err(Error::Validation(format!(
                "Invalid action '{}'. Valid: {:?}", action, valid
            )));
        }
        if (action == "open" || action == "goto") && params.get("url").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("'url' is required for open/goto actions".to_string()));
        }
        if action == "type" && params.get("text").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("'text' is required for type action".to_string()));
        }
        if action == "press_key" && params.get("text").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("'text' (key name) is required for press_key action".to_string()));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("open");

        // Ensure Chrome is available
        if !is_chrome_installed() {
            return Err(Error::Tool(
                "Google Chrome is not installed at /Applications/Google Chrome.app".to_string()
            ));
        }

        match action {
            "open" | "goto" => {
                let url = params["url"].as_str().unwrap();
                action_open(url).await
            }
            "type" => {
                let text = params["text"].as_str().unwrap();
                let selector = params.get("selector").and_then(|v| v.as_str());
                action_type(text, selector).await
            }
            "click" => {
                let selector = params.get("selector").and_then(|v| v.as_str());
                action_click(selector).await
            }
            "press_key" => {
                let key = params["text"].as_str().unwrap();
                action_press_key(key).await
            }
            "read" => {
                let selector = params.get("selector").and_then(|v| v.as_str());
                action_read(selector).await
            }
            "screenshot" => {
                let default_path = ctx.workspace.join("media").join(
                    format!("chrome_{}.png", chrono::Utc::now().format("%Y%m%d_%H%M%S"))
                );
                let path = params.get("screenshot_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| default_path.to_str().unwrap_or("screenshot.png"));
                action_screenshot(path).await
            }
            "scroll" => {
                let direction = params.get("direction").and_then(|v| v.as_str()).unwrap_or("down");
                let amount = params.get("amount").and_then(|v| v.as_i64()).unwrap_or(500);
                action_scroll(direction, amount).await
            }
            "wait" => {
                let ms = params.get("amount").and_then(|v| v.as_u64()).unwrap_or(1000);
                sleep(Duration::from_millis(ms)).await;
                Ok(json!({"action": "wait", "waited_ms": ms}))
            }
            "execute_js" => {
                let js = params.get("javascript").and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("'javascript' is required for execute_js".to_string()))?;
                action_execute_js(js).await
            }
            "get_url" => action_get_url().await,
            "tabs" => action_list_tabs().await,
            "new_tab" => {
                let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("about:blank");
                action_new_tab(url).await
            }
            "close_tab" => action_close_tab().await,
            "find_element" => {
                let selector = params.get("selector").and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("'selector' is required for find_element".to_string()))?;
                action_find_element(selector).await
            }
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

fn is_chrome_installed() -> bool {
    std::path::Path::new("/Applications/Google Chrome.app").exists()
}

/// Run an AppleScript and return stdout.
async fn run_applescript(script: &str) -> Result<String> {
    debug!(script_len = script.len(), "Running AppleScript");
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .await
        .map_err(|e| Error::Tool(format!("Failed to run osascript: {}", e)))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(Error::Tool(format!("AppleScript error: {}", stderr)))
    }
}

/// Run JavaScript in Chrome's active tab via AppleScript and return the result.
async fn run_chrome_js(js: &str) -> Result<String> {
    // Escape for AppleScript string embedding
    let escaped = js.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "Google Chrome"
    set jsResult to execute active tab of front window javascript "{}"
    return jsResult
end tell"#,
        escaped
    );
    run_applescript(&script).await
}

/// Ensure Chrome is running and frontmost.
async fn ensure_chrome_active() -> Result<()> {
    let script = r#"
if application "Google Chrome" is not running then
    tell application "Google Chrome" to activate
    delay 1
else
    tell application "Google Chrome" to activate
    delay 0.3
end if
"#;
    run_applescript(script).await?;
    Ok(())
}

// ============================================================
// Actions
// ============================================================

/// Open a URL in Chrome (activates Chrome, navigates in current tab).
async fn action_open(url: &str) -> Result<Value> {
    info!(url = %url, "🌐 Chrome: opening URL");
    let escaped_url = url.replace('"', "\\\"");
    let script = format!(
        r#"tell application "Google Chrome"
    activate
    if (count of windows) = 0 then
        make new window
        delay 0.5
    end if
    set URL of active tab of front window to "{}"
end tell"#,
        escaped_url
    );
    run_applescript(&script).await?;
    // Wait for page to start loading
    sleep(Duration::from_millis(1500)).await;

    // Get page title
    let title = run_chrome_js("document.title").await.unwrap_or_default();
    let current_url = run_chrome_js("window.location.href").await.unwrap_or_default();

    Ok(json!({
        "action": "open",
        "url": current_url,
        "title": title,
        "success": true
    }))
}

/// Type text. If selector is provided, focus that element first via JS click.
async fn action_type(text: &str, selector: Option<&str>) -> Result<Value> {
    ensure_chrome_active().await?;

    // If selector given, focus the element first
    if let Some(sel) = selector {
        let focus_js = format!(
            "(() => {{ var el = document.querySelector('{}'); if(el) {{ el.focus(); el.click(); }} return el ? 'focused' : 'not_found'; }})()",
            sel.replace('\'', "\\'")
        );
        let focus_result = run_chrome_js(&focus_js).await?;
        if focus_result.contains("not_found") {
            return Err(Error::Tool(format!("Element not found: {}", sel)));
        }
        sleep(Duration::from_millis(200)).await;
    }

    info!(text = %text, "🌐 Chrome: typing text");

    // Use AppleScript keystroke to type — this simulates real keyboard input
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "System Events"
    tell process "Google Chrome"
        keystroke "{}"
    end tell
end tell"#,
        escaped
    );
    run_applescript(&script).await?;

    Ok(json!({
        "action": "type",
        "text": text,
        "selector": selector,
        "success": true
    }))
}

/// Click an element by CSS selector (via JS), or click at current focus.
async fn action_click(selector: Option<&str>) -> Result<Value> {
    ensure_chrome_active().await?;

    if let Some(sel) = selector {
        info!(selector = %sel, "🌐 Chrome: clicking element");
        let click_js = format!(
            r#"(() => {{
    var el = document.querySelector('{}');
    if (!el) return JSON.stringify({{found: false}});
    el.scrollIntoView({{block: 'center'}});
    el.click();
    return JSON.stringify({{found: true, tag: el.tagName, text: (el.textContent || '').substring(0, 100)}});
}})()"#,
            sel.replace('\'', "\\'")
        );
        let result_str = run_chrome_js(&click_js).await?;
        let result: Value = serde_json::from_str(&result_str).unwrap_or(json!({"raw": result_str}));

        if result.get("found") == Some(&Value::Bool(false)) {
            return Err(Error::Tool(format!("Element not found: {}", sel)));
        }

        Ok(json!({
            "action": "click",
            "selector": sel,
            "element": result,
            "success": true
        }))
    } else {
        // No selector: press Enter as a "click" on the currently focused element
        action_press_key("return").await
    }
}

/// Press a key or key combination.
/// Supports: return, tab, escape, delete, space, up, down, left, right,
/// cmd+l, cmd+t, cmd+w, cmd+a, cmd+c, cmd+v, cmd+shift+..., etc.
async fn action_press_key(key: &str) -> Result<Value> {
    ensure_chrome_active().await?;
    info!(key = %key, "🌐 Chrome: pressing key");

    let script = build_key_script(key)?;
    run_applescript(&script).await?;

    Ok(json!({
        "action": "press_key",
        "key": key,
        "success": true
    }))
}

/// Build AppleScript for a key press.
fn build_key_script(key: &str) -> Result<String> {
    let lower = key.to_lowercase();
    let parts: Vec<&str> = lower.split('+').map(|s| s.trim()).collect();

    let has_cmd = parts.contains(&"cmd") || parts.contains(&"command");
    let has_shift = parts.contains(&"shift");
    let has_alt = parts.contains(&"alt") || parts.contains(&"option");
    let has_ctrl = parts.contains(&"ctrl") || parts.contains(&"control");

    let actual_key = parts.last().unwrap_or(&"return");

    // Map key names to AppleScript key codes or keystroke
    let (use_keycode, key_value) = match *actual_key {
        "return" | "enter" => (true, "36"),
        "tab" => (true, "48"),
        "escape" | "esc" => (true, "53"),
        "delete" | "backspace" => (true, "51"),
        "space" => (false, " "),
        "up" => (true, "126"),
        "down" => (true, "125"),
        "left" => (true, "123"),
        "right" => (true, "124"),
        "f5" => (true, "96"),
        k if k.len() == 1 => (false, k),
        // For keys like "l", "t", "w", "a", "c", "v" in combos
        _ => {
            if actual_key.len() == 1 {
                (false, *actual_key)
            } else {
                return Err(Error::Tool(format!("Unknown key: {}", actual_key)));
            }
        }
    };

    let mut modifiers = Vec::new();
    if has_cmd { modifiers.push("command down"); }
    if has_shift { modifiers.push("shift down"); }
    if has_alt { modifiers.push("option down"); }
    if has_ctrl { modifiers.push("control down"); }

    let modifier_str = if modifiers.is_empty() {
        String::new()
    } else {
        format!(" using {{{}}}", modifiers.join(", "))
    };

    let action_str = if use_keycode {
        format!("key code {}{}", key_value, modifier_str)
    } else {
        format!("keystroke \"{}\"{}", key_value, modifier_str)
    };

    Ok(format!(
        r#"tell application "System Events"
    tell process "Google Chrome"
        {}
    end tell
end tell"#,
        action_str
    ))
}

/// Read page content or a specific element's text.
async fn action_read(selector: Option<&str>) -> Result<Value> {
    ensure_chrome_active().await?;

    let js = if let Some(sel) = selector {
        format!(
            r#"(() => {{
    var el = document.querySelector('{}');
    if (!el) return JSON.stringify({{found: false}});
    return JSON.stringify({{
        found: true,
        tag: el.tagName,
        text: el.innerText.substring(0, 10000),
        html: el.innerHTML.substring(0, 5000),
        value: el.value || ''
    }});
}})()"#,
            sel.replace('\'', "\\'")
        )
    } else {
        r#"(() => {
    var title = document.title;
    var url = window.location.href;
    var text = document.body ? document.body.innerText.substring(0, 20000) : '';
    return JSON.stringify({title: title, url: url, text: text, text_length: text.length});
})()"#.to_string()
    };

    let result_str = run_chrome_js(&js).await?;
    let result: Value = serde_json::from_str(&result_str).unwrap_or(json!({"raw": result_str}));

    Ok(json!({
        "action": "read",
        "selector": selector,
        "content": result,
        "success": true
    }))
}

/// Take a screenshot of the Chrome window using screencapture.
async fn action_screenshot(path: &str) -> Result<Value> {
    ensure_chrome_active().await?;
    sleep(Duration::from_millis(300)).await;

    // Ensure output directory exists
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    info!(path = %path, "🌐 Chrome: taking screenshot");

    // Get Chrome window ID for targeted screenshot
    let script = r#"tell application "Google Chrome"
    set winId to id of front window
    return winId
end tell"#;
    let win_id = run_applescript(script).await.unwrap_or_default();

    // Use screencapture -l <windowID> for window-specific capture
    let output = if !win_id.is_empty() {
        Command::new("screencapture")
            .args(["-l", &win_id, "-x", path])
            .output()
            .await
    } else {
        // Fallback: capture frontmost window
        Command::new("screencapture")
            .args(["-w", "-x", path])
            .output()
            .await
    };

    match output {
        Ok(out) if out.status.success() => {
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            Ok(json!({
                "action": "screenshot",
                "path": path,
                "file_size_bytes": size,
                "success": true
            }))
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(Error::Tool(format!("Screenshot failed: {}", stderr)))
        }
        Err(e) => Err(Error::Tool(format!("screencapture error: {}", e))),
    }
}

/// Scroll the page.
async fn action_scroll(direction: &str, amount: i64) -> Result<Value> {
    ensure_chrome_active().await?;
    let scroll_y = if direction == "up" { -amount } else { amount };
    let js = format!("window.scrollBy(0, {}); window.scrollY", scroll_y);
    let pos = run_chrome_js(&js).await.unwrap_or_default();

    Ok(json!({
        "action": "scroll",
        "direction": direction,
        "amount": amount,
        "scroll_position": pos,
        "success": true
    }))
}

/// Execute arbitrary JavaScript in the active tab.
async fn action_execute_js(js: &str) -> Result<Value> {
    ensure_chrome_active().await?;
    let result = run_chrome_js(js).await?;

    // Try to parse as JSON
    let parsed: Value = serde_json::from_str(&result).unwrap_or(Value::String(result.clone()));

    Ok(json!({
        "action": "execute_js",
        "result": parsed,
        "success": true
    }))
}

/// Get the current URL of the active tab.
async fn action_get_url() -> Result<Value> {
    let script = r#"tell application "Google Chrome"
    set tabUrl to URL of active tab of front window
    set tabTitle to title of active tab of front window
    return tabUrl & "|SPLIT|" & tabTitle
end tell"#;
    let result = run_applescript(script).await?;
    let parts: Vec<&str> = result.splitn(2, "|SPLIT|").collect();
    let url = parts.first().unwrap_or(&"");
    let title = parts.get(1).unwrap_or(&"");

    Ok(json!({
        "action": "get_url",
        "url": url,
        "title": title,
        "success": true
    }))
}

/// List all open tabs.
async fn action_list_tabs() -> Result<Value> {
    let script = r#"tell application "Google Chrome"
    set tabList to ""
    set winCount to count of windows
    repeat with w from 1 to winCount
        set tabCount to count of tabs of window w
        repeat with t from 1 to tabCount
            set tabUrl to URL of tab t of window w
            set tabTitle to title of tab t of window w
            set tabList to tabList & w & "," & t & "," & tabTitle & "," & tabUrl & linefeed
        end repeat
    end repeat
    return tabList
end tell"#;
    let result = run_applescript(script).await?;

    let mut tabs = Vec::new();
    for line in result.lines() {
        let parts: Vec<&str> = line.splitn(4, ',').collect();
        if parts.len() >= 4 {
            tabs.push(json!({
                "window": parts[0],
                "tab": parts[1],
                "title": parts[2],
                "url": parts[3],
            }));
        }
    }

    Ok(json!({
        "action": "tabs",
        "tabs": tabs,
        "count": tabs.len(),
        "success": true
    }))
}

/// Open a new tab.
async fn action_new_tab(url: &str) -> Result<Value> {
    let escaped = url.replace('"', "\\\"");
    let script = format!(
        r#"tell application "Google Chrome"
    activate
    if (count of windows) = 0 then
        make new window
    end if
    tell front window
        set newTab to make new tab with properties {{URL:"{}"}}
    end tell
end tell"#,
        escaped
    );
    run_applescript(&script).await?;
    sleep(Duration::from_millis(1000)).await;

    Ok(json!({
        "action": "new_tab",
        "url": url,
        "success": true
    }))
}

/// Close the active tab.
async fn action_close_tab() -> Result<Value> {
    let script = r#"tell application "Google Chrome"
    close active tab of front window
end tell"#;
    run_applescript(script).await?;

    Ok(json!({
        "action": "close_tab",
        "success": true
    }))
}

/// Find an element by CSS selector and return its properties.
async fn action_find_element(selector: &str) -> Result<Value> {
    ensure_chrome_active().await?;
    let js = format!(
        r#"(() => {{
    var els = document.querySelectorAll('{}');
    var results = [];
    for (var i = 0; i < Math.min(els.length, 20); i++) {{
        var el = els[i];
        var rect = el.getBoundingClientRect();
        results.push({{
            index: i,
            tag: el.tagName,
            id: el.id || '',
            className: el.className || '',
            text: (el.innerText || '').substring(0, 200),
            value: el.value || '',
            href: el.href || '',
            type: el.type || '',
            visible: rect.width > 0 && rect.height > 0,
            rect: {{x: Math.round(rect.x), y: Math.round(rect.y), w: Math.round(rect.width), h: Math.round(rect.height)}}
        }});
    }}
    return JSON.stringify({{count: els.length, elements: results}});
}})()"#,
        selector.replace('\'', "\\'")
    );
    let result_str = run_chrome_js(&js).await?;
    let result: Value = serde_json::from_str(&result_str).unwrap_or(json!({"raw": result_str}));

    Ok(json!({
        "action": "find_element",
        "selector": selector,
        "result": result,
        "success": true
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chrome_control_schema() {
        let tool = ChromeControlTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "chrome_control");
    }

    #[test]
    fn test_chrome_control_validate() {
        let tool = ChromeControlTool;
        assert!(tool.validate(&json!({"action": "open", "url": "https://baidu.com"})).is_ok());
        assert!(tool.validate(&json!({"action": "type", "text": "hello"})).is_ok());
        assert!(tool.validate(&json!({"action": "press_key", "text": "return"})).is_ok());
        assert!(tool.validate(&json!({"action": "read"})).is_ok());
        assert!(tool.validate(&json!({"action": "tabs"})).is_ok());
        // Missing required params
        assert!(tool.validate(&json!({"action": "open"})).is_err());
        assert!(tool.validate(&json!({"action": "type"})).is_err());
        assert!(tool.validate(&json!({"action": "invalid_action"})).is_err());
    }

    #[test]
    fn test_build_key_script_simple() {
        let script = build_key_script("return").unwrap();
        assert!(script.contains("key code 36"));
    }

    #[test]
    fn test_build_key_script_combo() {
        let script = build_key_script("cmd+l").unwrap();
        assert!(script.contains("keystroke \"l\""));
        assert!(script.contains("command down"));
    }

    #[test]
    fn test_build_key_script_multi_modifier() {
        let script = build_key_script("cmd+shift+t").unwrap();
        assert!(script.contains("keystroke \"t\""));
        assert!(script.contains("command down"));
        assert!(script.contains("shift down"));
    }
}
