//! Browser session management.
//!
//! Manages multiple isolated browser sessions, each with its own Chrome process
//! and CDP connection. Sessions persist between tool calls (daemon model).

use super::cdp::CdpClient;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::{Child, Command};
use tracing::{debug, info};

/// Supported browser engines.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BrowserEngine {
    Chrome,
    Edge,
    Firefox,
}

impl BrowserEngine {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "firefox" | "ff" => Self::Firefox,
            "edge" | "msedge" => Self::Edge,
            _ => Self::Chrome,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Edge => "edge",
            Self::Firefox => "firefox",
        }
    }
}

/// A single browser session with its Chrome process and CDP client.
pub struct BrowserSession {
    /// Session name (e.g., "default", "agent1").
    pub name: String,
    /// Browser engine type.
    pub browser_engine: BrowserEngine,
    /// Remote debugging port used to discover per-target WebSocket URLs.
    pub debug_port: u16,
    /// Browser child process.
    chrome_process: Child,
    /// CDP WebSocket client.
    pub cdp: CdpClient,
    /// User data directory (for persistent profiles).
    pub user_data_dir: PathBuf,
    /// Whether this is a headed (visible) session.
    pub headed: bool,
    /// Current page URL.
    pub current_url: Option<String>,
    /// Ref counter for snapshot refs.
    pub ref_counter: u32,
    /// Ref map: ref_id -> {nodeId, backendNodeId, objectId, role, name, selector}.
    pub refs: HashMap<String, Value>,
    /// Auto-accept JavaScript dialogs (alert/confirm/prompt).
    pub dialog_auto_accept: bool,
}

impl BrowserSession {
    /// Close the browser session.
    pub async fn close(&mut self) {
        // Try graceful close via CDP first
        if let Err(e) = self.cdp.send_command("Browser.close", json!({})).await {
            debug!("CDP Browser.close failed (may already be closed): {}", e);
        }
        // Kill the process if still running
        let _ = self.chrome_process.kill().await;
    }
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        // Best-effort kill on drop
        let _ = self.chrome_process.start_kill();
    }
}

/// Manages multiple browser sessions.
pub struct SessionManager {
    sessions: HashMap<String, BrowserSession>,
    /// Base directory for session data (user data dirs, profiles).
    base_dir: PathBuf,
}

impl SessionManager {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            sessions: HashMap::new(),
            base_dir,
        }
    }

    /// Get or create a session by name. If the session doesn't exist, launch a browser.
    pub async fn get_or_create(
        &mut self,
        session_name: &str,
        headed: bool,
        profile_path: Option<&str>,
    ) -> Result<&mut BrowserSession, String> {
        self.get_or_create_with_engine(session_name, headed, profile_path, BrowserEngine::Chrome).await
    }

    /// Get or create a session with a specific browser engine.
    pub async fn get_or_create_with_engine(
        &mut self,
        session_name: &str,
        headed: bool,
        profile_path: Option<&str>,
        engine: BrowserEngine,
    ) -> Result<&mut BrowserSession, String> {
        if self.sessions.contains_key(session_name) {
            return Ok(self.sessions.get_mut(session_name).unwrap());
        }

        let session = self.launch_browser(session_name, headed, profile_path, engine).await?;
        self.sessions.insert(session_name.to_string(), session);
        Ok(self.sessions.get_mut(session_name).unwrap())
    }

    /// Get an existing session by name.
    pub fn get_session(&mut self, name: &str) -> Option<&mut BrowserSession> {
        self.sessions.get_mut(name)
    }

    /// List all active sessions.
    pub fn list_sessions(&self) -> Vec<&str> {
        self.sessions.keys().map(|s| s.as_str()).collect()
    }

    /// Close a specific session.
    pub async fn close_session(&mut self, name: &str) -> Result<(), String> {
        if let Some(mut session) = self.sessions.remove(name) {
            session.close().await;
            Ok(())
        } else {
            Err(format!("Session '{}' not found", name))
        }
    }

    /// Close all sessions.
    pub async fn close_all(&mut self) {
        let names: Vec<String> = self.sessions.keys().cloned().collect();
        for name in names {
            if let Some(mut session) = self.sessions.remove(&name) {
                session.close().await;
            }
        }
    }

    /// Launch a browser instance and connect via CDP.
    async fn launch_browser(
        &self,
        session_name: &str,
        headed: bool,
        profile_path: Option<&str>,
        engine: BrowserEngine,
    ) -> Result<BrowserSession, String> {
        let browser_path = find_browser_binary(engine)
            .ok_or_else(|| format!("{} not found. Please install it.", engine.name()))?;

        // Determine user data directory
        let user_data_dir = if let Some(profile) = profile_path {
            PathBuf::from(profile)
        } else {
            self.base_dir
                .join("sessions")
                .join(session_name)
        };

        // Ensure directory exists
        std::fs::create_dir_all(&user_data_dir)
            .map_err(|e| format!("Failed to create user data dir: {}", e))?;

        // Find a free port for CDP
        let debug_port = find_free_port().await?;

        let args = build_browser_args(engine, debug_port, &user_data_dir, headed);

        info!(
            session = session_name,
            port = debug_port,
            headed = headed,
            browser = engine.name(),
            "Launching browser for session"
        );

        let child = Command::new(&browser_path)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Failed to launch {}: {}", engine.name(), e))?;

        // Wait for CDP to be ready (browser-level)
        let _browser_ws_url = wait_for_cdp_ready(debug_port, 15).await?;

        // Connect to the page target (not browser-level) so Page.enable etc. work
        let page_ws_url = get_page_ws_url(debug_port).await?;

        // Connect CDP client to the page target
        let cdp = CdpClient::connect(&page_ws_url).await?;

        // Enable essential domains
        cdp.enable_domain("Page").await?;
        cdp.enable_domain("Runtime").await?;
        cdp.enable_domain("DOM").await?;
        cdp.enable_domain("Network").await?;
        cdp.enable_domain("Accessibility").await?;
        cdp.enable_domain("Target").await.ok(); // optional, may not be available on all browsers

        info!(
            session = session_name,
            ws_url = %page_ws_url,
            "CDP connection established (page target)"
        );

        Ok(BrowserSession {
            name: session_name.to_string(),
            browser_engine: engine,
            debug_port,
            chrome_process: child,
            cdp,
            user_data_dir,
            headed,
            current_url: None,
            ref_counter: 0,
            refs: HashMap::new(),
            dialog_auto_accept: true,
        })
    }
}

/// Build browser-specific command line arguments.
fn build_browser_args(
    engine: BrowserEngine,
    debug_port: u16,
    user_data_dir: &std::path::Path,
    headed: bool,
) -> Vec<String> {
    match engine {
        BrowserEngine::Firefox => {
            let mut args = vec![
                "--remote-debugging-port".to_string(),
                debug_port.to_string(),
                "--profile".to_string(),
                user_data_dir.display().to_string(),
                "--no-remote".to_string(),
            ];
            if !headed {
                args.push("--headless".to_string());
            }
            args.push("about:blank".to_string());
            args
        }
        BrowserEngine::Chrome | BrowserEngine::Edge => {
            let mut args = vec![
                format!("--remote-debugging-port={}", debug_port),
                format!("--user-data-dir={}", user_data_dir.display()),
                "--no-first-run".to_string(),
                "--no-default-browser-check".to_string(),
                "--disable-background-networking".to_string(),
                "--disable-extensions".to_string(),
                "--disable-sync".to_string(),
                "--disable-translate".to_string(),
                "--metrics-recording-only".to_string(),
                "--safebrowsing-disable-auto-update".to_string(),
                "--password-store=basic".to_string(),
            ];
            if !headed {
                args.push("--headless=new".to_string());
            }
            args.push("--window-size=1280,720".to_string());
            args.push("about:blank".to_string());
            args
        }
    }
}

/// Find a browser binary on the system for the given engine.
pub fn find_browser_binary(engine: BrowserEngine) -> Option<String> {
    let candidates = match engine {
        BrowserEngine::Chrome => {
            if cfg!(target_os = "macos") {
                vec![
                    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                    "/Applications/Chromium.app/Contents/MacOS/Chromium",
                    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
                ]
            } else if cfg!(target_os = "linux") {
                vec![
                    "google-chrome", "google-chrome-stable",
                    "chromium", "chromium-browser",
                    "/usr/bin/google-chrome", "/usr/bin/chromium",
                ]
            } else {
                vec![
                    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                    r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
                ]
            }
        }
        BrowserEngine::Edge => {
            if cfg!(target_os = "macos") {
                vec!["/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"]
            } else if cfg!(target_os = "linux") {
                vec!["microsoft-edge", "microsoft-edge-stable", "/usr/bin/microsoft-edge"]
            } else {
                vec![
                    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
                    r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
                ]
            }
        }
        BrowserEngine::Firefox => {
            if cfg!(target_os = "macos") {
                vec!["/Applications/Firefox.app/Contents/MacOS/firefox"]
            } else if cfg!(target_os = "linux") {
                vec!["firefox", "/usr/bin/firefox"]
            } else {
                vec![
                    r"C:\Program Files\Mozilla Firefox\firefox.exe",
                    r"C:\Program Files (x86)\Mozilla Firefox\firefox.exe",
                ]
            }
        }
    };

    for candidate in candidates {
        if std::path::Path::new(candidate).exists() {
            return Some(candidate.to_string());
        }
        if !candidate.contains('/') && !candidate.contains('\\') {
            if which::which(candidate).is_ok() {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

/// List all available browser engines on the system.
pub fn list_available_browsers() -> Vec<(BrowserEngine, String)> {
    let mut result = Vec::new();
    for engine in [BrowserEngine::Chrome, BrowserEngine::Edge, BrowserEngine::Firefox] {
        if let Some(path) = find_browser_binary(engine) {
            result.push((engine, path));
        }
    }
    result
}

/// Find a free TCP port.
async fn find_free_port() -> Result<u16, String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Failed to bind to find free port: {}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get local addr: {}", e))?
        .port();
    drop(listener);
    Ok(port)
}

/// Wait for Chrome's CDP endpoint to become available.
/// Polls /json/version until it responds, up to `timeout_secs`.
async fn wait_for_cdp_ready(port: u16, timeout_secs: u64) -> Result<String, String> {
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let url = format!("http://127.0.0.1:{}/json/version", port);

    loop {
        if start.elapsed() > timeout {
            return Err(format!(
                "Chrome CDP not ready after {}s on port {}",
                timeout_secs, port
            ));
        }

        match reqwest::get(&url).await {
            Ok(resp) => {
                if let Ok(body) = resp.json::<Value>().await {
                    if let Some(ws_url) = body.get("webSocketDebuggerUrl").and_then(|v| v.as_str())
                    {
                        return Ok(ws_url.to_string());
                    }
                }
            }
            Err(_) => {}
        }

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// Connect to a specific page/tab's WebSocket URL.
/// Chrome exposes /json/list which lists all targets (pages).
/// Retries a few times since the page target may not appear immediately.
pub async fn get_page_ws_url(port: u16) -> Result<String, String> {
    let url = format!("http://127.0.0.1:{}/json/list", port);

    for attempt in 0..10 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }

        let resp = match reqwest::get(&url).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        let targets: Vec<Value> = match resp.json().await {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Find the first "page" type target
        for target in &targets {
            if target.get("type").and_then(|v| v.as_str()) == Some("page") {
                if let Some(ws_url) = target.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) {
                    return Ok(ws_url.to_string());
                }
            }
        }
    }

    Err("No page target found after retries".to_string())
}

/// Resolve a targetId to its WebSocket debugger URL via /json/list.
pub async fn get_target_ws_url(port: u16, target_id: &str) -> Result<String, String> {
    let url = format!("http://127.0.0.1:{}/json/list", port);

    for attempt in 0..10 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }

        let resp = match reqwest::get(&url).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        let targets: Vec<Value> = match resp.json().await {
            Ok(t) => t,
            Err(_) => continue,
        };

        for target in &targets {
            if target.get("targetId").and_then(|v| v.as_str()) == Some(target_id) {
                if let Some(ws_url) = target
                    .get("webSocketDebuggerUrl")
                    .and_then(|v| v.as_str())
                {
                    return Ok(ws_url.to_string());
                }
            }
        }
    }

    Err(format!(
        "No WebSocket URL found for targetId '{}' after retries",
        target_id
    ))
}
