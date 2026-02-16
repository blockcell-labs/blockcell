use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for controlling any macOS application via AppleScript + System Events.
///
/// Uses AppleScript accessibility APIs to automate any visible application —
/// activating windows, typing text, pressing keys, clicking menu items,
/// reading the UI element tree, and taking screenshots.
///
/// This is a generalized version of `chrome_control` that works with any app.
pub struct AppControlTool;

#[async_trait]
impl Tool for AppControlTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "app_control",
            description: "Control any macOS application via AppleScript + System Events. Can activate apps, type text, press keyboard shortcuts, click menu items, read the accessibility UI tree, take screenshots, and list windows. Works with any application (Windsurf, VS Code, Finder, Terminal, etc.).",
            parameters: json!({
                "type": "object",
                "properties": {
                    "app": {
                        "type": "string",
                        "description": "Application name or process name (e.g. 'Windsurf', 'Finder', 'Terminal', 'Google Chrome', 'Safari')"
                    },
                    "action": {
                        "type": "string",
                        "enum": [
                            "activate", "screenshot", "type", "press_key",
                            "read_ui", "click_menu", "get_windows",
                            "click_ui_element", "list_apps", "get_frontmost",
                            "wait"
                        ],
                        "description": "Action to perform"
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to type (for 'type'), key combo (for 'press_key', e.g. 'return', 'cmd+p', 'cmd+shift+p'), or menu path (for 'click_menu', e.g. 'File > Save')"
                    },
                    "ui_path": {
                        "type": "string",
                        "description": "Accessibility UI element path for 'click_ui_element' (e.g. 'button \"Run\"', 'text field 1', 'group 1 > button 2')"
                    },
                    "screenshot_path": {
                        "type": "string",
                        "description": "File path to save screenshot (for 'screenshot' action)"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Depth of UI tree traversal for 'read_ui' (default: 3, max: 6). Lower is faster."
                    },
                    "amount": {
                        "type": "integer",
                        "description": "Wait duration in ms (for 'wait' action, default: 1000)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid = [
            "activate", "screenshot", "type", "press_key",
            "read_ui", "click_menu", "get_windows",
            "click_ui_element", "list_apps", "get_frontmost",
            "wait",
        ];
        if !valid.contains(&action) {
            return Err(Error::Validation(format!(
                "Invalid action '{}'. Valid: {:?}", action, valid
            )));
        }

        // Actions that need an app name
        let needs_app = [
            "activate", "screenshot", "type", "press_key",
            "read_ui", "click_menu", "get_windows", "click_ui_element",
        ];
        if needs_app.contains(&action) && params.get("app").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(format!(
                "'app' is required for '{}' action", action
            )));
        }

        if action == "type" && params.get("text").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("'text' is required for type action".to_string()));
        }
        if action == "press_key" && params.get("text").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("'text' (key combo) is required for press_key action".to_string()));
        }
        if action == "click_menu" && params.get("text").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("'text' (menu path) is required for click_menu action".to_string()));
        }
        if action == "click_ui_element" && params.get("ui_path").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation("'ui_path' is required for click_ui_element action".to_string()));
        }

        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("list_apps");
        let app = params.get("app").and_then(|v| v.as_str()).unwrap_or("");

        match action {
            "activate" => action_activate(app).await,
            "screenshot" => {
                let default_path = ctx.workspace.join("media").join(
                    format!("app_{}.png", chrono::Utc::now().format("%Y%m%d_%H%M%S"))
                );
                let path = params.get("screenshot_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| default_path.to_str().unwrap_or("screenshot.png"));
                action_screenshot(app, path).await
            }
            "type" => {
                let text = params["text"].as_str().unwrap();
                action_type(app, text).await
            }
            "press_key" => {
                let key = params["text"].as_str().unwrap();
                action_press_key(app, key).await
            }
            "read_ui" => {
                let depth = params.get("depth").and_then(|v| v.as_i64()).unwrap_or(3) as usize;
                let depth = depth.min(6);
                action_read_ui(app, depth).await
            }
            "click_menu" => {
                let menu_path = params["text"].as_str().unwrap();
                action_click_menu(app, menu_path).await
            }
            "get_windows" => action_get_windows(app).await,
            "click_ui_element" => {
                let ui_path = params["ui_path"].as_str().unwrap();
                action_click_ui_element(app, ui_path).await
            }
            "list_apps" => action_list_apps().await,
            "get_frontmost" => action_get_frontmost().await,
            "wait" => {
                let ms = params.get("amount").and_then(|v| v.as_u64()).unwrap_or(1000);
                sleep(Duration::from_millis(ms)).await;
                Ok(json!({"action": "wait", "waited_ms": ms}))
            }
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

// ============================================================
// Helpers
// ============================================================

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

/// Resolve the process name for System Events.
/// Some apps have different bundle names vs process names (e.g. "Windsurf" might be "Electron").
async fn resolve_process_name(app_name: &str) -> Result<String> {
    // Use explicit linefeed delimiter — AppleScript's default `as text` concatenates without separator
    let script = r#"tell application "System Events"
    set procList to name of every process whose visible is true
    set output to ""
    repeat with p in procList
        set output to output & p & linefeed
    end repeat
    return output
end tell"#;
    let result = run_applescript(script).await.unwrap_or_default();

    let procs: Vec<&str> = result.lines().filter(|l| !l.is_empty()).collect();

    // Check if the app name matches directly (case-insensitive)
    for proc in &procs {
        if proc.eq_ignore_ascii_case(app_name) {
            return Ok(proc.to_string());
        }
    }

    // Try partial match — but only match individual process names, not the whole list
    let lower_app = app_name.to_lowercase();
    for proc in &procs {
        let lower_proc = proc.to_lowercase();
        if lower_proc.contains(&lower_app) || lower_app.contains(&lower_proc) {
            info!(app = %app_name, resolved = %proc, "Resolved process name via partial match");
            return Ok(proc.to_string());
        }
    }

    // Fallback: just use the given name (AppleScript will error if wrong)
    warn!(app = %app_name, "Could not resolve process name, using as-is");
    Ok(app_name.to_string())
}

/// Escape a string for embedding in AppleScript.
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// ============================================================
// Actions
// ============================================================

/// Activate (bring to front) an application.
async fn action_activate(app: &str) -> Result<Value> {
    info!(app = %app, "🖥️ App: activating");
    let escaped = escape_applescript(app);
    let script = format!(
        r#"tell application "{}" to activate
delay 0.3"#,
        escaped
    );
    run_applescript(&script).await?;

    Ok(json!({
        "action": "activate",
        "app": app,
        "success": true
    }))
}

/// Take a screenshot of a specific application window.
async fn action_screenshot(app: &str, path: &str) -> Result<Value> {
    info!(app = %app, path = %path, "🖥️ App: taking screenshot");

    // Ensure output directory exists
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Activate the app first
    let escaped = escape_applescript(app);
    let activate_script = format!(
        r#"tell application "{}" to activate
delay 0.5"#,
        escaped
    );
    run_applescript(&activate_script).await?;

    // Try to get window ID via AppleScript
    let process_name = resolve_process_name(app).await?;
    let win_id_script = format!(
        r#"tell application "System Events"
    tell process "{}"
        if (count of windows) > 0 then
            set frontWin to window 1
            -- Get the window's position and size for identification
            set winPos to position of frontWin
            set winSize to size of frontWin
            return (item 1 of winPos as text) & "," & (item 2 of winPos as text) & "," & (item 1 of winSize as text) & "," & (item 2 of winSize as text)
        else
            return "no_windows"
        end if
    end tell
end tell"#,
        escape_applescript(&process_name)
    );

    let win_info = run_applescript(&win_id_script).await.unwrap_or_default();

    // Try to get the CGWindowID for targeted screencapture
    let cg_win_id = get_cg_window_id(app).await;

    let output = if let Some(wid) = cg_win_id {
        debug!(window_id = wid, "Using targeted window capture");
        Command::new("screencapture")
            .args(["-l", &wid.to_string(), "-x", path])
            .output()
            .await
    } else {
        // Fallback: capture the frontmost window
        debug!("Falling back to frontmost window capture");
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
                "app": app,
                "path": path,
                "file_size_bytes": size,
                "window_info": win_info,
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

/// Get the CGWindowID for an application's frontmost window.
/// Uses the `CGWindowListCopyWindowInfo` API via a small Python snippet.
async fn get_cg_window_id(app: &str) -> Option<u32> {
    let script = format!(
        r#"import Quartz, sys
app_name = "{}"
wl = Quartz.CGWindowListCopyWindowInfo(
    Quartz.kCGWindowListOptionOnScreenOnly | Quartz.kCGWindowListExcludeDesktopElements,
    Quartz.kCGNullWindowID
)
for w in wl:
    owner = w.get(Quartz.kCGWindowOwnerName, "")
    if app_name.lower() in owner.lower() or owner.lower() in app_name.lower():
        layer = w.get(Quartz.kCGWindowLayer, 999)
        if layer == 0:
            print(w.get(Quartz.kCGWindowNumber, 0))
            sys.exit(0)
print("")"#,
        app.replace('"', "\\\"")
    );

    let output = Command::new("python3")
        .arg("-c")
        .arg(&script)
        .output()
        .await
        .ok()?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        stdout.parse::<u32>().ok()
    } else {
        None
    }
}

/// Type text into the currently focused element of an application.
async fn action_type(app: &str, text: &str) -> Result<Value> {
    info!(app = %app, text = %text, "🖥️ App: typing text");

    // Activate the app
    let escaped_app = escape_applescript(app);
    let activate = format!(r#"tell application "{}" to activate
delay 0.2"#, escaped_app);
    run_applescript(&activate).await?;

    let process_name = resolve_process_name(app).await?;
    let escaped_text = escape_applescript(text);
    let script = format!(
        r#"tell application "System Events"
    tell process "{}"
        keystroke "{}"
    end tell
end tell"#,
        escape_applescript(&process_name),
        escaped_text
    );
    run_applescript(&script).await?;

    Ok(json!({
        "action": "type",
        "app": app,
        "text": text,
        "success": true
    }))
}

/// Press a key or key combination in an application.
async fn action_press_key(app: &str, key: &str) -> Result<Value> {
    info!(app = %app, key = %key, "🖥️ App: pressing key");

    // Activate the app
    let escaped_app = escape_applescript(app);
    let activate = format!(r#"tell application "{}" to activate
delay 0.2"#, escaped_app);
    run_applescript(&activate).await?;

    let process_name = resolve_process_name(app).await?;
    let key_action = build_key_action(key)?;

    let script = format!(
        r#"tell application "System Events"
    tell process "{}"
        {}
    end tell
end tell"#,
        escape_applescript(&process_name),
        key_action
    );
    run_applescript(&script).await?;

    Ok(json!({
        "action": "press_key",
        "app": app,
        "key": key,
        "success": true
    }))
}

/// Build the AppleScript key action string (keystroke or key code with modifiers).
fn build_key_action(key: &str) -> Result<String> {
    let lower = key.to_lowercase();
    let parts: Vec<&str> = lower.split('+').map(|s| s.trim()).collect();

    let has_cmd = parts.contains(&"cmd") || parts.contains(&"command");
    let has_shift = parts.contains(&"shift");
    let has_alt = parts.contains(&"alt") || parts.contains(&"option");
    let has_ctrl = parts.contains(&"ctrl") || parts.contains(&"control");

    let actual_key = parts.last().unwrap_or(&"return");

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
        "f1" => (true, "122"),
        "f2" => (true, "120"),
        "f3" => (true, "99"),
        "f4" => (true, "118"),
        "f5" => (true, "96"),
        "f6" => (true, "97"),
        "f7" => (true, "98"),
        "f8" => (true, "100"),
        "f9" => (true, "101"),
        "f10" => (true, "109"),
        "f11" => (true, "103"),
        "f12" => (true, "111"),
        "home" => (true, "115"),
        "end" => (true, "119"),
        "pageup" => (true, "116"),
        "pagedown" => (true, "121"),
        "forwarddelete" => (true, "117"),
        k if k.len() == 1 => (false, k),
        _ => {
            return Err(Error::Tool(format!("Unknown key: {}", actual_key)));
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

    if use_keycode {
        Ok(format!("key code {}{}", key_value, modifier_str))
    } else {
        Ok(format!("keystroke \"{}\"{}", key_value, modifier_str))
    }
}

/// Read the accessibility UI tree of an application.
/// Returns a structured representation of visible UI elements.
async fn action_read_ui(app: &str, depth: usize) -> Result<Value> {
    info!(app = %app, depth = depth, "🖥️ App: reading UI tree");

    let process_name = resolve_process_name(app).await?;
    let escaped = escape_applescript(&process_name);

    // Build a recursive AppleScript to traverse the UI tree
    // We limit depth to avoid massive output
    let script = format!(
        r#"on describeElement(el, indent, maxDepth, currentDepth)
    if currentDepth > maxDepth then return ""
    set desc to ""
    try
        set elRole to role of el
        set elDesc to description of el
        set elTitle to ""
        try
            set elTitle to title of el
        end try
        set elValue to ""
        try
            set elValue to value of el as text
            if length of elValue > 200 then
                set elValue to text 1 thru 200 of elValue & "..."
            end if
        end try
        set elFocused to ""
        try
            set elFocused to focused of el as text
        end try
        
        set line to indent & elRole
        if elTitle is not "" then set line to line & " \"" & elTitle & "\""
        if elDesc is not "" and elDesc is not elTitle then set line to line & " [" & elDesc & "]"
        if elValue is not "" then set line to line & " = " & elValue
        if elFocused is "true" then set line to line & " *FOCUSED*"
        set desc to desc & line & linefeed
        
        if currentDepth < maxDepth then
            set subEls to UI elements of el
            if (count of subEls) > 0 then
                set childIndent to indent & "  "
                repeat with subEl in subEls
                    set desc to desc & describeElement(subEl, childIndent, maxDepth, currentDepth + 1)
                end repeat
            end if
        end if
    on error errMsg
        -- skip inaccessible elements
    end try
    return desc
end describeElement

tell application "System Events"
    tell process "{}"
        set output to ""
        if (count of windows) > 0 then
            set frontWin to window 1
            set winTitle to ""
            try
                set winTitle to title of frontWin
            end try
            set output to "Window: " & winTitle & linefeed
            set output to output & describeElement(frontWin, "  ", {}, 1)
        else
            set output to "No windows found for process {}"
        end if
        return output
    end tell
end tell"#,
        escaped, depth, escaped
    );

    let result = run_applescript(&script).await?;

    // Parse the result into structured data
    let lines: Vec<&str> = result.lines().collect();
    let line_count = lines.len();

    // If output is too large, truncate with a note
    let truncated = line_count > 500;
    let display_lines: Vec<&str> = if truncated {
        lines[..500].to_vec()
    } else {
        lines
    };

    Ok(json!({
        "action": "read_ui",
        "app": app,
        "depth": depth,
        "ui_tree": display_lines.join("\n"),
        "total_elements": line_count,
        "truncated": truncated,
        "success": true
    }))
}

/// Click a menu item by path (e.g. "File > Save", "Edit > Find > Find...").
async fn action_click_menu(app: &str, menu_path: &str) -> Result<Value> {
    info!(app = %app, menu = %menu_path, "🖥️ App: clicking menu");

    // Activate the app first
    let escaped_app = escape_applescript(app);
    let activate = format!(r#"tell application "{}" to activate
delay 0.3"#, escaped_app);
    run_applescript(&activate).await?;

    let process_name = resolve_process_name(app).await?;
    let parts: Vec<&str> = menu_path.split('>').map(|s| s.trim()).collect();

    if parts.is_empty() {
        return Err(Error::Validation("Menu path cannot be empty".to_string()));
    }

    // Build the nested menu click AppleScript
    let escaped_process = escape_applescript(&process_name);
    let script = match parts.len() {
        1 => format!(
            r#"tell application "System Events"
    tell process "{}"
        click menu item "{}" of menu bar 1
    end tell
end tell"#,
            escaped_process,
            escape_applescript(parts[0])
        ),
        2 => format!(
            r#"tell application "System Events"
    tell process "{}"
        click menu item "{}" of menu 1 of menu bar item "{}" of menu bar 1
    end tell
end tell"#,
            escaped_process,
            escape_applescript(parts[1]),
            escape_applescript(parts[0])
        ),
        3 => format!(
            r#"tell application "System Events"
    tell process "{}"
        click menu item "{}" of menu 1 of menu item "{}" of menu 1 of menu bar item "{}" of menu bar 1
    end tell
end tell"#,
            escaped_process,
            escape_applescript(parts[2]),
            escape_applescript(parts[1]),
            escape_applescript(parts[0])
        ),
        _ => {
            return Err(Error::Validation(
                "Menu path too deep (max 3 levels: 'Menu > Submenu > Item')".to_string()
            ));
        }
    };

    run_applescript(&script).await?;

    Ok(json!({
        "action": "click_menu",
        "app": app,
        "menu_path": menu_path,
        "success": true
    }))
}

/// Get all windows of an application.
async fn action_get_windows(app: &str) -> Result<Value> {
    info!(app = %app, "🖥️ App: listing windows");

    let process_name = resolve_process_name(app).await?;
    let escaped = escape_applescript(&process_name);

    let script = format!(
        r#"tell application "System Events"
    tell process "{}"
        set winList to ""
        set winCount to count of windows
        repeat with i from 1 to winCount
            set w to window i
            set winTitle to ""
            try
                set winTitle to title of w
            end try
            set winPos to position of w
            set winSize to size of w
            set winList to winList & i & "|" & winTitle & "|" & (item 1 of winPos) & "," & (item 2 of winPos) & "|" & (item 1 of winSize) & "," & (item 2 of winSize) & linefeed
        end repeat
        return winList
    end tell
end tell"#,
        escaped
    );

    let result = run_applescript(&script).await?;
    let mut windows = Vec::new();

    for line in result.lines() {
        let parts: Vec<&str> = line.splitn(4, '|').collect();
        if parts.len() >= 4 {
            let pos_parts: Vec<&str> = parts[2].split(',').collect();
            let size_parts: Vec<&str> = parts[3].split(',').collect();
            windows.push(json!({
                "index": parts[0],
                "title": parts[1],
                "position": {
                    "x": pos_parts.first().unwrap_or(&"0"),
                    "y": pos_parts.get(1).unwrap_or(&"0")
                },
                "size": {
                    "width": size_parts.first().unwrap_or(&"0"),
                    "height": size_parts.get(1).unwrap_or(&"0")
                }
            }));
        }
    }

    Ok(json!({
        "action": "get_windows",
        "app": app,
        "windows": windows,
        "count": windows.len(),
        "success": true
    }))
}

/// Click a UI element by accessibility path.
/// Path format: "button \"Run\"", "text field 1", "group 1 > button 2"
async fn action_click_ui_element(app: &str, ui_path: &str) -> Result<Value> {
    info!(app = %app, ui_path = %ui_path, "🖥️ App: clicking UI element");

    // Activate the app first
    let escaped_app = escape_applescript(app);
    let activate = format!(r#"tell application "{}" to activate
delay 0.2"#, escaped_app);
    run_applescript(&activate).await?;

    let process_name = resolve_process_name(app).await?;
    let escaped_process = escape_applescript(&process_name);

    // Parse the path: "group 1 > button 2" → nested tell blocks
    let parts: Vec<&str> = ui_path.split('>').map(|s| s.trim()).collect();

    // Build nested element reference
    // e.g. "group 1 > button 2" → "button 2 of group 1 of window 1"
    let mut element_ref = String::new();
    for (i, part) in parts.iter().rev().enumerate() {
        if i > 0 {
            element_ref.push_str(" of ");
        }
        element_ref.push_str(part);
    }
    element_ref.push_str(" of window 1");

    let script = format!(
        r#"tell application "System Events"
    tell process "{}"
        set targetEl to {}
        -- Try click
        click targetEl
        -- Get info about what we clicked
        set elRole to role of targetEl
        set elDesc to ""
        try
            set elDesc to description of targetEl
        end try
        return elRole & "|" & elDesc
    end tell
end tell"#,
        escaped_process,
        element_ref
    );

    let result = run_applescript(&script).await?;
    let result_parts: Vec<&str> = result.splitn(2, '|').collect();

    Ok(json!({
        "action": "click_ui_element",
        "app": app,
        "ui_path": ui_path,
        "element_role": result_parts.first().unwrap_or(&""),
        "element_description": result_parts.get(1).unwrap_or(&""),
        "success": true
    }))
}

/// List all running visible applications.
async fn action_list_apps() -> Result<Value> {
    info!("🖥️ App: listing running applications");

    let script = r#"tell application "System Events"
    set appList to ""
    set procs to every process whose visible is true
    repeat with p in procs
        set procName to name of p
        set procId to unix id of p
        set winCount to 0
        try
            set winCount to count of windows of p
        end try
        set bundleId to ""
        try
            set bundleId to bundle identifier of p
        end try
        set appList to appList & procName & "|" & procId & "|" & winCount & "|" & bundleId & linefeed
    end repeat
    return appList
end tell"#;

    let result = run_applescript(script).await?;
    let mut apps = Vec::new();

    for line in result.lines() {
        let parts: Vec<&str> = line.splitn(4, '|').collect();
        if parts.len() >= 4 {
            apps.push(json!({
                "name": parts[0],
                "pid": parts[1],
                "window_count": parts[2],
                "bundle_id": parts[3]
            }));
        }
    }

    Ok(json!({
        "action": "list_apps",
        "apps": apps,
        "count": apps.len(),
        "success": true
    }))
}

/// Get the frontmost (active) application.
async fn action_get_frontmost() -> Result<Value> {
    let script = r#"tell application "System Events"
    set frontApp to first process whose frontmost is true
    set appName to name of frontApp
    set appId to unix id of frontApp
    set bundleId to ""
    try
        set bundleId to bundle identifier of frontApp
    end try
    set winTitle to ""
    try
        set winTitle to title of window 1 of frontApp
    end try
    return appName & "|" & appId & "|" & bundleId & "|" & winTitle
end tell"#;

    let result = run_applescript(script).await?;
    let parts: Vec<&str> = result.splitn(4, '|').collect();

    Ok(json!({
        "action": "get_frontmost",
        "app": parts.first().unwrap_or(&""),
        "pid": parts.get(1).unwrap_or(&""),
        "bundle_id": parts.get(2).unwrap_or(&""),
        "window_title": parts.get(3).unwrap_or(&""),
        "success": true
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_control_schema() {
        let tool = AppControlTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "app_control");
    }

    #[test]
    fn test_app_control_validate() {
        let tool = AppControlTool;
        // Actions that don't need app
        assert!(tool.validate(&json!({"action": "list_apps"})).is_ok());
        assert!(tool.validate(&json!({"action": "get_frontmost"})).is_ok());
        assert!(tool.validate(&json!({"action": "wait"})).is_ok());

        // Actions that need app
        assert!(tool.validate(&json!({"action": "activate", "app": "Finder"})).is_ok());
        assert!(tool.validate(&json!({"action": "activate"})).is_err());
        assert!(tool.validate(&json!({"action": "screenshot"})).is_err());
        assert!(tool.validate(&json!({"action": "screenshot", "app": "Windsurf"})).is_ok());

        // Actions that need text
        assert!(tool.validate(&json!({"action": "type", "app": "Windsurf", "text": "hello"})).is_ok());
        assert!(tool.validate(&json!({"action": "type", "app": "Windsurf"})).is_err());
        assert!(tool.validate(&json!({"action": "press_key", "app": "Windsurf", "text": "cmd+p"})).is_ok());
        assert!(tool.validate(&json!({"action": "press_key", "app": "Windsurf"})).is_err());
        assert!(tool.validate(&json!({"action": "click_menu", "app": "Windsurf", "text": "File > Save"})).is_ok());
        assert!(tool.validate(&json!({"action": "click_menu", "app": "Windsurf"})).is_err());

        // click_ui_element needs ui_path
        assert!(tool.validate(&json!({"action": "click_ui_element", "app": "Windsurf", "ui_path": "button 1"})).is_ok());
        assert!(tool.validate(&json!({"action": "click_ui_element", "app": "Windsurf"})).is_err());

        // Invalid action
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }

    #[test]
    fn test_build_key_action_simple() {
        let action = build_key_action("return").unwrap();
        assert!(action.contains("key code 36"));
    }

    #[test]
    fn test_build_key_action_combo() {
        let action = build_key_action("cmd+p").unwrap();
        assert!(action.contains("keystroke \"p\""));
        assert!(action.contains("command down"));
    }

    #[test]
    fn test_build_key_action_multi_modifier() {
        let action = build_key_action("cmd+shift+p").unwrap();
        assert!(action.contains("keystroke \"p\""));
        assert!(action.contains("command down"));
        assert!(action.contains("shift down"));
    }

    #[test]
    fn test_build_key_action_function_keys() {
        let action = build_key_action("f5").unwrap();
        assert!(action.contains("key code 96"));
        let action = build_key_action("f12").unwrap();
        assert!(action.contains("key code 111"));
    }

    #[test]
    fn test_escape_applescript() {
        assert_eq!(escape_applescript(r#"hello "world""#), r#"hello \"world\""#);
        assert_eq!(escape_applescript(r#"back\slash"#), r#"back\\slash"#);
    }
}
