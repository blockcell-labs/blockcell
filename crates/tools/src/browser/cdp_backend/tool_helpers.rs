//! CDP backend 的纯辅助函数：元素目标解析、坐标换算、按键解析、CDP 错误转换。
//!
//! 从 `browser/cdp_backend.rs` 抽出的无状态辅助，供 CDP backend 的各 action 调用。

use blockcell_core::Result;
use serde_json::{json, Value};

use crate::browser::session::BrowserSession;

/// Resolve element target from params: either "ref" or "selector".
pub(super) fn resolve_element_target(
    params: &Value,
) -> std::result::Result<(&'static str, String), blockcell_core::Error> {
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
pub(super) async fn click_by_backend_node(
    session: &mut BrowserSession,
    backend_node_id: i64,
) -> Result<()> {
    // Resolve to a remote object
    let result = session
        .cdp
        .send_command("DOM.resolveNode", json!({"backendNodeId": backend_node_id}))
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
        .send_command("DOM.getBoxModel", json!({"backendNodeId": backend_node_id}))
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
pub(super) async fn click_by_selector(session: &mut BrowserSession, selector: &str) -> Result<()> {
    let escaped = selector
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace("${", "\\${");
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
pub(super) async fn focus_by_backend_node(
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
pub(super) async fn focus_by_selector(session: &mut BrowserSession, selector: &str) -> Result<()> {
    let js = format!(
        "document.querySelector('{}')?.focus()",
        selector.replace('\'', "\\'")
    );
    session.cdp.evaluate_js(&js).await.map_err(cdp_err)?;
    Ok(())
}

/// Extract center coordinates from a box model response.
pub(super) fn extract_center_from_box_model(bm: &Value) -> (f64, f64) {
    if let Some(content) = bm
        .get("model")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
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
pub(super) fn parse_key_spec(key: &str) -> (String, String, i32) {
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
                return (
                    main_key.clone(),
                    format!("Key{}", main_key.to_uppercase()),
                    modifiers,
                );
            }
            &main_key
        }
    }
    .to_string();

    (main_key, code, modifiers)
}

/// Convert CDP error string to blockcell Error.
pub(super) fn cdp_err(e: String) -> blockcell_core::Error {
    blockcell_core::Error::Tool(format!("CDP: {}", e))
}
