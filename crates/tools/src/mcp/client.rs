use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, error, warn};

// ─── JSON-RPC types ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<u64>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

// ─── MCP tool schema types ────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

// ─── MCP Client ───────────────────────────────────────────────────────────────

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

pub struct McpClient {
    server_name: String,
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: Arc<AtomicU64>,
    pending: PendingMap,
    tools: Arc<Mutex<Vec<McpTool>>>,
    _child: Arc<Mutex<Child>>,
}

impl McpClient {
    /// Launch an MCP server child process and perform the MCP initialization handshake.
    pub async fn start(
        server_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        cwd: Option<&str>,
    ) -> blockcell_core::Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);

        for (k, v) in env {
            cmd.env(k, v);
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().map_err(|e| {
            blockcell_core::Error::Tool(format!(
                "MCP[{}]: failed to spawn '{}': {}",
                server_name, command, e
            ))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            blockcell_core::Error::Tool(format!("MCP[{}]: no stdin", server_name))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            blockcell_core::Error::Tool(format!("MCP[{}]: no stdout", server_name))
        })?;

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let pending_clone = pending.clone();
        let sname = server_name.to_string();

        // Background task: read newline-delimited JSON-RPC responses from stdout
        tokio::spawn(Self::reader_task(stdout, pending_clone, sname));

        let client = Self {
            server_name: server_name.to_string(),
            stdin: Arc::new(Mutex::new(stdin)),
            next_id: Arc::new(AtomicU64::new(1)),
            pending,
            tools: Arc::new(Mutex::new(Vec::new())),
            _child: Arc::new(Mutex::new(child)),
        };

        // MCP initialize handshake
        client.initialize().await?;

        // Fetch the tool list
        client.refresh_tools().await?;

        Ok(client)
    }

    /// Send a JSON-RPC request and wait for the response.
    async fn call(&self, method: &str, params: Option<Value>) -> blockcell_core::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };

        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending.lock().await;
            map.insert(id, tx);
        }

        let line = serde_json::to_string(&req).map_err(|e| {
            blockcell_core::Error::Tool(format!("MCP[{}]: serialize error: {}", self.server_name, e))
        })?;
        debug!(server = %self.server_name, id, method, "MCP → request");

        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await.map_err(|e| {
                blockcell_core::Error::Tool(format!("MCP[{}]: write error: {}", self.server_name, e))
            })?;
            stdin.write_all(b"\n").await.map_err(|e| {
                blockcell_core::Error::Tool(format!("MCP[{}]: write error: {}", self.server_name, e))
            })?;
            stdin.flush().await.map_err(|e| {
                blockcell_core::Error::Tool(format!("MCP[{}]: flush error: {}", self.server_name, e))
            })?;
        }

        rx.await
            .map_err(|_| blockcell_core::Error::Tool(format!("MCP[{}]: server closed", self.server_name)))?
            .map_err(|e| blockcell_core::Error::Tool(format!("MCP[{}]: {}", self.server_name, e)))
    }

    /// MCP initialize + initialized notification
    async fn initialize(&self) -> blockcell_core::Result<()> {
        let params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "blockcell",
                "version": "0.1.0"
            }
        });
        let result = self.call("initialize", Some(params)).await?;
        debug!(server = %self.server_name, ?result, "MCP initialized");

        // Send the notifications/initialized notification (no id, fire-and-forget)
        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let line = serde_json::to_string(&notif).unwrap_or_default();
        let mut stdin = self.stdin.lock().await;
        let _ = stdin.write_all(line.as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        let _ = stdin.flush().await;

        Ok(())
    }

    /// Fetch tools/list and cache them locally.
    pub async fn refresh_tools(&self) -> blockcell_core::Result<()> {
        let result = self.call("tools/list", None).await?;
        let tools: Vec<McpTool> = serde_json::from_value(
            result.get("tools").cloned().unwrap_or(Value::Array(vec![]))
        ).map_err(|e| {
            blockcell_core::Error::Tool(format!("MCP[{}]: parse tools: {}", self.server_name, e))
        })?;
        debug!(server = %self.server_name, count = tools.len(), "MCP tools loaded");
        *self.tools.lock().await = tools;
        Ok(())
    }

    /// Return cached tool list.
    pub async fn list_tools(&self) -> Vec<McpTool> {
        self.tools.lock().await.clone()
    }

    /// Call tools/call on the MCP server.
    pub async fn call_tool(&self, tool_name: &str, arguments: Value) -> blockcell_core::Result<Value> {
        let params = serde_json::json!({
            "name": tool_name,
            "arguments": arguments
        });
        let result = self.call("tools/call", Some(params)).await?;

        // MCP returns { content: [...], isError: bool }
        if let Some(true) = result.get("isError").and_then(|v| v.as_bool()) {
            let msg = result
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|item| item.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("MCP tool returned an error");
            return Err(blockcell_core::Error::Tool(msg.to_string()));
        }

        // Extract text content blocks into a single string result
        let content = result.get("content").cloned().unwrap_or(Value::Null);
        if let Some(arr) = content.as_array() {
            let text: String = arr.iter()
                .filter_map(|item| {
                    if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                        item.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                return Ok(Value::String(text));
            }
        }
        Ok(content)
    }

    /// Background reader task — dispatches incoming JSON-RPC responses to waiting callers.
    async fn reader_task(stdout: ChildStdout, pending: PendingMap, server_name: String) {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();

        loop {
            match lines.next_line().await {
                Ok(Some(line)) if !line.trim().is_empty() => {
                    debug!(server = %server_name, "MCP ← {}", &line[..line.len().min(200)]);
                    match serde_json::from_str::<JsonRpcResponse>(&line) {
                        Ok(resp) => {
                            if let Some(id) = resp.id {
                                let mut map = pending.lock().await;
                                if let Some(tx) = map.remove(&id) {
                                    let payload = if let Some(err) = resp.error {
                                        Err(format!("JSON-RPC error {}: {}", err.code, err.message))
                                    } else {
                                        Ok(resp.result.unwrap_or(Value::Null))
                                    };
                                    let _ = tx.send(payload);
                                }
                            }
                            // Notifications (no id) are silently ignored.
                        }
                        Err(e) => {
                            warn!(server = %server_name, "MCP: failed to parse response: {}", e);
                        }
                    }
                }
                Ok(Some(_)) => {} // blank line
                Ok(None) => {
                    error!(server = %server_name, "MCP: stdout closed");
                    // Fail all pending requests
                    let mut map = pending.lock().await;
                    for (_, tx) in map.drain() {
                        let _ = tx.send(Err("MCP server stdout closed".to_string()));
                    }
                    break;
                }
                Err(e) => {
                    error!(server = %server_name, "MCP: read error: {}", e);
                    break;
                }
            }
        }
    }
}
