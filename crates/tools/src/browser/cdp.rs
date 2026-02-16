//! Low-level Chrome DevTools Protocol (CDP) client over WebSocket.
//!
//! Communicates with a Chrome/Chromium instance via its debugging WebSocket endpoint.
//! Supports sending commands, receiving responses, and handling events.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, warn};

/// A CDP WebSocket client that can send commands and receive responses/events.
pub struct CdpClient {
    /// Sender to write messages to the WebSocket.
    ws_tx: mpsc::Sender<String>,
    /// Pending command responses, keyed by request ID.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    /// Auto-incrementing command ID.
    next_id: AtomicU64,
    /// Event listeners (domain.event -> channel).
    event_listeners: Arc<Mutex<HashMap<String, Vec<mpsc::Sender<Value>>>>>,
    /// Handle to the reader task so we can abort on close.
    _reader_handle: tokio::task::JoinHandle<()>,
    /// Handle to the writer task.
    _writer_handle: tokio::task::JoinHandle<()>,
}

impl CdpClient {
    /// Connect to a Chrome CDP WebSocket endpoint.
    pub async fn connect(ws_url: &str) -> Result<Self, String> {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::Message;

        let (ws_stream, _) = connect_async(ws_url)
            .await
            .map_err(|e| format!("Failed to connect to CDP endpoint {}: {}", ws_url, e))?;

        let (mut ws_sink, mut ws_stream_read) = ws_stream.split();

        // Channel for outgoing messages
        let (ws_tx, mut ws_rx) = mpsc::channel::<String>(256);

        // Pending responses
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_clone = pending.clone();

        // Event listeners
        let event_listeners: Arc<Mutex<HashMap<String, Vec<mpsc::Sender<Value>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let events_clone = event_listeners.clone();

        // Writer task: owns the sink, forwards messages from channel
        let writer_handle = tokio::spawn(async move {
            while let Some(msg) = ws_rx.recv().await {
                if let Err(e) = ws_sink.send(Message::Text(msg)).await {
                    error!("CDP WebSocket write error: {}", e);
                    break;
                }
            }
        });

        // Reader task: reads from WebSocket, dispatches responses and events
        let reader_handle = tokio::spawn(async move {
            while let Some(msg_result) = ws_stream_read.next().await {
                match msg_result {
                    Ok(Message::Text(text)) => {
                        if let Ok(val) = serde_json::from_str::<Value>(&text) {
                            if let Some(id) = val.get("id").and_then(|v| v.as_u64()) {
                                // This is a command response
                                let mut pending = pending_clone.lock().await;
                                if let Some(tx) = pending.remove(&id) {
                                    let _ = tx.send(val);
                                }
                            } else if let Some(method) = val.get("method").and_then(|v| v.as_str())
                            {
                                // This is an event
                                let listeners = events_clone.lock().await;
                                if let Some(senders) = listeners.get(method) {
                                    let params = val
                                        .get("params")
                                        .cloned()
                                        .unwrap_or(Value::Null);
                                    for tx in senders {
                                        let _ = tx.try_send(params.clone());
                                    }
                                }
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        debug!("CDP WebSocket closed by server");
                        break;
                    }
                    Err(e) => {
                        warn!("CDP WebSocket read error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        });

        Ok(Self {
            ws_tx,
            pending,
            next_id: AtomicU64::new(1),
            event_listeners,
            _reader_handle: reader_handle,
            _writer_handle: writer_handle,
        })
    }

    /// Send a CDP command and wait for the response.
    pub async fn send_command(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let msg = json!({
            "id": id,
            "method": method,
            "params": params,
        });

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        self.ws_tx
            .send(msg.to_string())
            .await
            .map_err(|e| format!("Failed to send CDP command: {}", e))?;

        // Wait for response with timeout
        let timeout = tokio::time::timeout(std::time::Duration::from_secs(30), rx);
        match timeout.await {
            Ok(Ok(response)) => {
                if let Some(error) = response.get("error") {
                    Err(format!("CDP error: {}", error))
                } else {
                    Ok(response.get("result").cloned().unwrap_or(Value::Null))
                }
            }
            Ok(Err(_)) => Err("CDP response channel closed".to_string()),
            Err(_) => {
                // Remove from pending
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                Err(format!("CDP command '{}' timed out after 30s", method))
            }
        }
    }

    /// Subscribe to a CDP event. Returns a receiver that will get event params.
    pub async fn subscribe_event(&self, method: &str) -> mpsc::Receiver<Value> {
        let (tx, rx) = mpsc::channel(64);
        let mut listeners = self.event_listeners.lock().await;
        listeners
            .entry(method.to_string())
            .or_insert_with(Vec::new)
            .push(tx);
        rx
    }

    /// Enable a CDP domain (e.g., "Page", "Runtime", "Network", "DOM", "Accessibility").
    pub async fn enable_domain(&self, domain: &str) -> Result<(), String> {
        self.send_command(&format!("{}.enable", domain), json!({}))
            .await?;
        Ok(())
    }

    /// Navigate to a URL and wait for load.
    pub async fn navigate(&self, url: &str) -> Result<Value, String> {
        self.send_command("Page.navigate", json!({"url": url}))
            .await
    }

    /// Evaluate JavaScript in the page context.
    pub async fn evaluate_js(&self, expression: &str) -> Result<Value, String> {
        let result = self
            .send_command(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        Ok(result)
    }

    /// Take a screenshot and return base64-encoded PNG data.
    pub async fn screenshot(&self, full_page: bool) -> Result<String, String> {
        let mut params = json!({"format": "png"});
        if full_page {
            params["captureBeyondViewport"] = json!(true);
        }
        let result = self.send_command("Page.captureScreenshot", params).await?;
        result
            .get("data")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "No screenshot data returned".to_string())
    }

    /// Get the full accessibility tree via CDP.
    pub async fn get_accessibility_tree(&self) -> Result<Value, String> {
        self.send_command("Accessibility.getFullAXTree", json!({}))
            .await
    }

    /// Get the document root node.
    pub async fn get_document(&self) -> Result<Value, String> {
        self.send_command("DOM.getDocument", json!({"depth": -1}))
            .await
    }

    /// Query a CSS selector and return node IDs.
    pub async fn query_selector_all(
        &self,
        node_id: i64,
        selector: &str,
    ) -> Result<Vec<i64>, String> {
        let result = self
            .send_command(
                "DOM.querySelectorAll",
                json!({
                    "nodeId": node_id,
                    "selector": selector,
                }),
            )
            .await?;
        let ids = result
            .get("nodeIds")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_i64())
                    .collect()
            })
            .unwrap_or_default();
        Ok(ids)
    }

    /// Resolve a DOM node to a Runtime object for JS interaction.
    pub async fn resolve_node(&self, node_id: i64) -> Result<String, String> {
        let result = self
            .send_command("DOM.resolveNode", json!({"nodeId": node_id}))
            .await?;
        result
            .get("object")
            .and_then(|o| o.get("objectId"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "Failed to resolve node".to_string())
    }

    /// Call a function on a remote object.
    pub async fn call_function_on(
        &self,
        object_id: &str,
        function_declaration: &str,
    ) -> Result<Value, String> {
        self.send_command(
            "Runtime.callFunctionOn",
            json!({
                "objectId": object_id,
                "functionDeclaration": function_declaration,
                "returnByValue": true,
            }),
        )
        .await
    }

    /// Dispatch a mouse event via Input domain.
    pub async fn dispatch_mouse_event(
        &self,
        event_type: &str,
        x: f64,
        y: f64,
        button: &str,
        click_count: i32,
    ) -> Result<(), String> {
        self.send_command(
            "Input.dispatchMouseEvent",
            json!({
                "type": event_type,
                "x": x,
                "y": y,
                "button": button,
                "clickCount": click_count,
            }),
        )
        .await?;
        Ok(())
    }

    /// Dispatch a key event via Input domain.
    pub async fn dispatch_key_event(
        &self,
        event_type: &str,
        key: &str,
        code: &str,
        modifiers: i32,
    ) -> Result<(), String> {
        let mut params = json!({
            "type": event_type,
            "key": key,
            "code": code,
        });
        if modifiers != 0 {
            params["modifiers"] = json!(modifiers);
        }
        // For printable characters, set text
        if event_type == "keyDown" && key.len() == 1 {
            params["text"] = json!(key);
        }
        self.send_command("Input.dispatchKeyEvent", params).await?;
        Ok(())
    }

    /// Insert text (bypasses key events, good for filling forms).
    pub async fn insert_text(&self, text: &str) -> Result<(), String> {
        self.send_command(
            "Input.insertText",
            json!({"text": text}),
        )
        .await?;
        Ok(())
    }

    /// Set cookies.
    pub async fn set_cookie(
        &self,
        name: &str,
        value: &str,
        domain: &str,
    ) -> Result<(), String> {
        self.send_command(
            "Network.setCookie",
            json!({
                "name": name,
                "value": value,
                "domain": domain,
            }),
        )
        .await?;
        Ok(())
    }

    /// Get all cookies.
    pub async fn get_cookies(&self) -> Result<Value, String> {
        self.send_command("Network.getCookies", json!({})).await
    }

    /// Clear browser cookies.
    pub async fn clear_cookies(&self) -> Result<(), String> {
        self.send_command("Network.clearBrowserCookies", json!({}))
            .await?;
        Ok(())
    }

    /// Set viewport/device metrics.
    pub async fn set_viewport(&self, width: i32, height: i32, device_scale_factor: f64) -> Result<(), String> {
        self.send_command(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": width,
                "height": height,
                "deviceScaleFactor": device_scale_factor,
                "mobile": false,
            }),
        )
        .await?;
        Ok(())
    }

    /// Print page to PDF and return base64 data.
    pub async fn print_to_pdf(&self) -> Result<String, String> {
        let result = self
            .send_command("Page.printToPDF", json!({"printBackground": true}))
            .await?;
        result
            .get("data")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "No PDF data returned".to_string())
    }

    /// Set extra HTTP headers.
    pub async fn set_extra_headers(&self, headers: Value) -> Result<(), String> {
        self.send_command("Network.setExtraHTTPHeaders", json!({"headers": headers}))
            .await?;
        Ok(())
    }

    /// Emulate media features (e.g., prefers-color-scheme).
    pub async fn emulate_media(&self, features: Value) -> Result<(), String> {
        self.send_command("Emulation.setEmulatedMedia", json!({"features": features}))
            .await?;
        Ok(())
    }

    // ─── Tab / Target management ──────────────────────────────────────

    /// Get all browser targets (pages, iframes, workers, etc.).
    pub async fn get_targets(&self) -> Result<Vec<Value>, String> {
        let result = self.send_command("Target.getTargets", json!({})).await?;
        Ok(result
            .get("targetInfos")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Create a new page target (tab) with the given URL.
    pub async fn create_target(&self, url: &str) -> Result<String, String> {
        let result = self
            .send_command("Target.createTarget", json!({"url": url}))
            .await?;
        result
            .get("targetId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "No targetId returned from createTarget".to_string())
    }

    /// Close a target by its targetId.
    pub async fn close_target(&self, target_id: &str) -> Result<(), String> {
        self.send_command("Target.closeTarget", json!({"targetId": target_id}))
            .await?;
        Ok(())
    }

    /// Activate (bring to front) a target by its targetId.
    pub async fn activate_target(&self, target_id: &str) -> Result<(), String> {
        self.send_command("Target.activateTarget", json!({"targetId": target_id}))
            .await?;
        Ok(())
    }

    // ─── File upload ──────────────────────────────────────────────────

    /// Set files on a file input element identified by backendNodeId.
    pub async fn set_file_input_files(
        &self,
        files: Vec<String>,
        backend_node_id: i64,
    ) -> Result<(), String> {
        self.send_command(
            "DOM.setFileInputFiles",
            json!({
                "files": files,
                "backendNodeId": backend_node_id,
            }),
        )
        .await?;
        Ok(())
    }

    /// Set files on a file input element identified by objectId.
    pub async fn set_file_input_files_by_object(
        &self,
        files: Vec<String>,
        object_id: &str,
    ) -> Result<(), String> {
        self.send_command(
            "DOM.setFileInputFiles",
            json!({
                "files": files,
                "objectId": object_id,
            }),
        )
        .await?;
        Ok(())
    }

    // ─── Dialog handling ──────────────────────────────────────────────

    /// Handle a JavaScript dialog (alert/confirm/prompt/beforeunload).
    pub async fn handle_dialog(&self, accept: bool, prompt_text: Option<&str>) -> Result<(), String> {
        let mut params = json!({"accept": accept});
        if let Some(text) = prompt_text {
            params["promptText"] = json!(text);
        }
        self.send_command("Page.handleJavaScriptDialog", params).await?;
        Ok(())
    }

    // ─── Network interception (Fetch domain) ──────────────────────────

    /// Enable the Fetch domain for network interception.
    /// `patterns` is an array of RequestPattern objects, e.g.:
    /// [{"urlPattern": "*", "requestStage": "Request"}]
    pub async fn enable_fetch(&self, patterns: Vec<Value>) -> Result<(), String> {
        self.send_command(
            "Fetch.enable",
            json!({"patterns": patterns, "handleAuthRequests": false}),
        )
        .await?;
        Ok(())
    }

    /// Disable the Fetch domain.
    pub async fn disable_fetch(&self) -> Result<(), String> {
        self.send_command("Fetch.disable", json!({})).await?;
        Ok(())
    }

    /// Continue a paused request (optionally modify URL, method, headers, postData).
    pub async fn fetch_continue(
        &self,
        request_id: &str,
        url: Option<&str>,
        method: Option<&str>,
        headers: Option<Vec<Value>>,
        post_data: Option<&str>,
    ) -> Result<(), String> {
        let mut params = json!({"requestId": request_id});
        if let Some(u) = url {
            params["url"] = json!(u);
        }
        if let Some(m) = method {
            params["method"] = json!(m);
        }
        if let Some(h) = headers {
            params["headers"] = json!(h);
        }
        if let Some(pd) = post_data {
            params["postData"] = json!(pd);
        }
        self.send_command("Fetch.continueRequest", params).await?;
        Ok(())
    }

    /// Fail a paused request with a specific error reason.
    pub async fn fetch_fail(&self, request_id: &str, reason: &str) -> Result<(), String> {
        self.send_command(
            "Fetch.failRequest",
            json!({"requestId": request_id, "errorReason": reason}),
        )
        .await?;
        Ok(())
    }

    /// Fulfill a paused request with a custom response.
    pub async fn fetch_fulfill(
        &self,
        request_id: &str,
        response_code: i32,
        headers: Option<Vec<Value>>,
        body: Option<&str>,
    ) -> Result<(), String> {
        let mut params = json!({
            "requestId": request_id,
            "responseCode": response_code,
        });
        if let Some(h) = headers {
            params["responseHeaders"] = json!(h);
        }
        if let Some(b) = body {
            // Body must be base64-encoded
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(b.as_bytes());
            params["body"] = json!(encoded);
        }
        self.send_command("Fetch.fulfillRequest", params).await?;
        Ok(())
    }
}

impl Drop for CdpClient {
    fn drop(&mut self) {
        self._reader_handle.abort();
        self._writer_handle.abort();
    }
}
