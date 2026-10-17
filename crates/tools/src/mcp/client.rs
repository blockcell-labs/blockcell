use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

fn summarize_json(value: &Value, max_len: usize) -> String {
    let raw = serde_json::to_string(value).unwrap_or_else(|_| "<json-serialize-error>".to_string());
    if raw.chars().count() <= max_len {
        return raw;
    }
    raw.chars().take(max_len).collect::<String>() + "..."
}

fn summarize_text(text: &str, max_len: usize) -> String {
    if text.chars().count() <= max_len {
        return text.to_string();
    }
    text.chars().take(max_len).collect::<String>() + "..."
}

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

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, String>>>>>;

pub struct McpClient {
    server_name: String,
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: AtomicU64,
    pending: PendingMap,
    tools: Arc<Mutex<Vec<McpTool>>>,
    child: Arc<Mutex<Child>>,
    call_timeout: Duration,
}

impl McpClient {
    /// Launch an MCP server child process and perform the MCP initialization handshake.
    pub async fn start(
        server_name: &str,
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        cwd: Option<&str>,
        startup_timeout: Duration,
        call_timeout: Duration,
    ) -> blockcell_core::Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

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
        let server_name_owned = server_name.to_string();
        std::thread::Builder::new()
            .name(format!("mcp-reader-{}", server_name))
            .spawn(move || Self::reader_thread(stdout, pending_clone, server_name_owned))
            .map_err(|e| {
                blockcell_core::Error::Tool(format!(
                    "MCP[{}]: failed to spawn reader thread: {}",
                    server_name, e
                ))
            })?;

        let client = Self {
            server_name: server_name.to_string(),
            stdin: Arc::new(Mutex::new(stdin)),
            next_id: AtomicU64::new(1),
            pending,
            tools: Arc::new(Mutex::new(Vec::new())),
            child: Arc::new(Mutex::new(child)),
            call_timeout,
        };

        timeout(startup_timeout, async {
            client.initialize().await?;
            client.refresh_tools().await?;
            Ok::<(), blockcell_core::Error>(())
        })
        .await
        .map_err(|_| {
            blockcell_core::Error::Tool(format!(
                "MCP[{}]: startup timed out after {}s",
                server_name,
                startup_timeout.as_secs()
            ))
        })??;

        Ok(client)
    }

    async fn write_line(&self, line: String) -> blockcell_core::Result<()> {
        let stdin = self.stdin.clone();
        let server_name = self.server_name.clone();

        tokio::task::spawn_blocking(move || -> blockcell_core::Result<()> {
            let mut stdin = stdin.lock().map_err(|_| {
                blockcell_core::Error::Tool(format!("MCP[{}]: stdin lock poisoned", server_name))
            })?;
            stdin.write_all(line.as_bytes()).map_err(|e| {
                blockcell_core::Error::Tool(format!("MCP[{}]: write error: {}", server_name, e))
            })?;
            stdin.write_all(b"\n").map_err(|e| {
                blockcell_core::Error::Tool(format!("MCP[{}]: write error: {}", server_name, e))
            })?;
            stdin.flush().map_err(|e| {
                blockcell_core::Error::Tool(format!("MCP[{}]: flush error: {}", server_name, e))
            })?;
            Ok(())
        })
        .await
        .map_err(|e| {
            blockcell_core::Error::Tool(format!(
                "MCP[{}]: write task failed: {}",
                self.server_name, e
            ))
        })?
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
            let mut map = self.pending.lock().map_err(|_| {
                blockcell_core::Error::Tool(format!(
                    "MCP[{}]: pending map lock poisoned",
                    self.server_name
                ))
            })?;
            map.insert(id, tx);
        }

        let line = serde_json::to_string(&req).map_err(|e| {
            blockcell_core::Error::Tool(format!(
                "MCP[{}]: serialize error: {}",
                self.server_name, e
            ))
        })?;
        debug!(server = %self.server_name, id, method, "MCP → request");
        self.write_line(line).await?;

        let response = timeout(self.call_timeout, rx).await.map_err(|_| {
            if let Ok(mut map) = self.pending.lock() {
                map.remove(&id);
            }
            blockcell_core::Error::Tool(format!(
                "MCP[{}]: call '{}' timed out after {}s",
                self.server_name,
                method,
                self.call_timeout.as_secs()
            ))
        })?;

        response
            .map_err(|_| {
                blockcell_core::Error::Tool(format!("MCP[{}]: server closed", self.server_name))
            })?
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

        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let line = serde_json::to_string(&notif).unwrap_or_default();
        let _ = self.write_line(line).await;

        Ok(())
    }

    /// Fetch tools/list and cache them locally.
    pub async fn refresh_tools(&self) -> blockcell_core::Result<()> {
        let result = self.call("tools/list", None).await?;
        let tools: Vec<McpTool> =
            serde_json::from_value(result.get("tools").cloned().unwrap_or(Value::Array(vec![])))
                .map_err(|e| {
                    blockcell_core::Error::Tool(format!(
                        "MCP[{}]: parse tools: {}",
                        self.server_name, e
                    ))
                })?;
        debug!(server = %self.server_name, count = tools.len(), "MCP tools loaded");
        *self.tools.lock().map_err(|_| {
            blockcell_core::Error::Tool(format!("MCP[{}]: tools lock poisoned", self.server_name))
        })? = tools;
        Ok(())
    }

    /// Return cached tool list.
    pub async fn list_tools(&self) -> Vec<McpTool> {
        self.tools
            .lock()
            .map(|tools| tools.clone())
            .unwrap_or_default()
    }

    /// Call tools/call on the MCP server.
    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> blockcell_core::Result<Value> {
        let args_preview = summarize_json(&arguments, 800);
        info!(
            server = %self.server_name,
            tool = %tool_name,
            args = %args_preview,
            "MCP tool call start"
        );
        let params = serde_json::json!({
            "name": tool_name,
            "arguments": arguments
        });
        let result = self.call("tools/call", Some(params)).await?;

        if let Some(true) = result.get("isError").and_then(|v| v.as_bool()) {
            let msg = result
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|item| item.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("MCP tool returned an error");
            warn!(
                server = %self.server_name,
                tool = %tool_name,
                error = %summarize_text(msg, 800),
                "MCP tool call failed"
            );
            return Err(blockcell_core::Error::Tool(msg.to_string()));
        }

        let content = result.get("content").cloned().unwrap_or(Value::Null);
        if let Some(arr) = content.as_array() {
            let text: String = arr
                .iter()
                .filter_map(|item| {
                    if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                        item.get("text")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                info!(
                    server = %self.server_name,
                    tool = %tool_name,
                    result = %summarize_text(&text, 800),
                    "MCP tool call success"
                );
                return Ok(Value::String(text));
            }
        }
        info!(
            server = %self.server_name,
            tool = %tool_name,
            result = %summarize_json(&content, 800),
            "MCP tool call success"
        );
        Ok(content)
    }

    fn reader_thread(stdout: ChildStdout, pending: PendingMap, server_name: String) {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();

        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    let truncate_at = line
                        .char_indices()
                        .take_while(|(i, _)| *i < 200)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(0);
                    debug!(server = %server_name, "MCP ← {}", &line[..truncate_at.min(line.len())]);
                    match serde_json::from_str::<JsonRpcResponse>(&line) {
                        Ok(resp) => {
                            if let Some(id) = resp.id {
                                let tx = pending.lock().ok().and_then(|mut map| map.remove(&id));
                                if let Some(tx) = tx {
                                    let payload = if let Some(err) = resp.error {
                                        Err(format!("JSON-RPC error {}: {}", err.code, err.message))
                                    } else {
                                        Ok(resp.result.unwrap_or(Value::Null))
                                    };
                                    let _ = tx.send(payload);
                                }
                            }
                        }
                        Err(e) => {
                            warn!(server = %server_name, "MCP: failed to parse response: {}", e);
                        }
                    }
                }
                Err(e) => {
                    error!(server = %server_name, "MCP: read error: {}", e);
                    break;
                }
            }
        }

        error!(server = %server_name, "MCP: stdout closed");
        if let Ok(mut map) = pending.lock() {
            for (_, tx) in map.drain() {
                let _ = tx.send(Err("MCP server stdout closed".to_string()));
            }
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Minimal stdio MCP server used to exercise the real protocol path.
    const FAKE_SERVER: &str = r#"
import json
import sys


def send(payload):
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()


for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    try:
        req = json.loads(raw)
    except json.JSONDecodeError:
        continue
    method = req.get("method")
    rid = req.get("id")
    if rid is None:
        # Notification (notifications/initialized): no response is expected.
        continue
    params = req.get("params") or {}
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake", "version": "0.0.1"},
        }})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": rid, "result": {"tools": [{
            "name": "echo",
            "description": "echo the provided text",
            "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
        }]}})
    elif method == "tools/call":
        name = params.get("name")
        if name == "echo":
            text = (params.get("arguments") or {}).get("text", "")
            send({"jsonrpc": "2.0", "id": rid,
                  "result": {"content": [{"type": "text", "text": text}]}})
        elif name == "boom":
            send({"jsonrpc": "2.0", "id": rid, "result": {
                "isError": True,
                "content": [{"type": "text", "text": "boom failed"}],
            }})
        elif name == "rpc_error":
            send({"jsonrpc": "2.0", "id": rid,
                  "error": {"code": -32601, "message": "method not found"}})
        elif name == "exit":
            sys.exit(0)
        else:
            send({"jsonrpc": "2.0", "id": rid,
                  "error": {"code": -32602, "message": "unknown tool"}})
    else:
        send({"jsonrpc": "2.0", "id": rid,
              "error": {"code": -32601, "message": "unknown method"}})
"#;

    fn write_server(tag: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "blockcell_mcp_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create mcp test dir");
        let path = dir.join("fake_server.py");
        std::fs::write(&path, body).expect("write fake server");
        path
    }

    async fn start_server(tag: &str, body: &str, startup: Duration) -> McpClient {
        let script = write_server(tag, body);
        McpClient::start(
            "fake",
            "python3",
            &[script.to_string_lossy().to_string()],
            &HashMap::new(),
            None,
            startup,
            Duration::from_secs(10),
        )
        .await
        .expect("fake MCP server should start")
    }

    fn expect_start_error(result: blockcell_core::Result<McpClient>) -> blockcell_core::Error {
        match result {
            Ok(_) => panic!("expected MCP startup to fail"),
            Err(error) => error,
        }
    }

    #[tokio::test]
    async fn initialize_handshake_caches_the_server_tool_list() {
        let client = start_server("handshake", FAKE_SERVER, Duration::from_secs(10)).await;

        let tools = client.list_tools().await;

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(
            tools[0].description.as_deref(),
            Some("echo the provided text")
        );
        assert_eq!(tools[0].input_schema["type"], "object");
    }

    #[tokio::test]
    async fn call_tool_returns_the_text_content() {
        let client = start_server("call", FAKE_SERVER, Duration::from_secs(10)).await;

        let result = client
            .call_tool("echo", serde_json::json!({"text": "hello mcp"}))
            .await
            .expect("echo succeeds");

        assert_eq!(result, Value::String("hello mcp".to_string()));
    }

    #[tokio::test]
    async fn call_tool_surfaces_an_is_error_payload_as_failure() {
        let client = start_server("iserror", FAKE_SERVER, Duration::from_secs(10)).await;

        let error = client
            .call_tool("boom", serde_json::json!({}))
            .await
            .expect_err("isError payload must not be reported as success");

        assert!(error.to_string().contains("boom failed"), "{error}");
    }

    #[tokio::test]
    async fn call_tool_surfaces_a_json_rpc_error() {
        let client = start_server("rpcerr", FAKE_SERVER, Duration::from_secs(10)).await;

        let error = client
            .call_tool("rpc_error", serde_json::json!({}))
            .await
            .expect_err("JSON-RPC errors must propagate");

        let message = error.to_string();
        assert!(message.contains("JSON-RPC error -32601"), "{message}");
        assert!(message.contains("method not found"), "{message}");
    }

    #[tokio::test]
    async fn call_tool_fails_when_the_server_exits_mid_call() {
        let client = start_server("exit", FAKE_SERVER, Duration::from_secs(10)).await;

        let error = client
            .call_tool("exit", serde_json::json!({}))
            .await
            .expect_err("a closed server must not hang the caller");

        assert!(error.to_string().contains("closed"), "{error}");
    }

    #[tokio::test]
    async fn start_reports_a_missing_server_binary() {
        let error = expect_start_error(
            McpClient::start(
                "missing",
                "blockcell-definitely-not-a-real-binary",
                &[],
                &HashMap::new(),
                None,
                Duration::from_secs(5),
                Duration::from_secs(5),
            )
            .await,
        );

        assert!(error.to_string().contains("failed to spawn"), "{error}");
    }

    #[tokio::test]
    async fn start_times_out_when_the_server_never_answers_initialize() {
        let error = expect_start_error(
            McpClient::start(
                "silent",
                "python3",
                &[
                    write_server("silent", "import time\nwhile True:\n    time.sleep(1)\n")
                        .to_string_lossy()
                        .to_string(),
                ],
                &HashMap::new(),
                None,
                Duration::from_millis(500),
                Duration::from_secs(5),
            )
            .await,
        );

        assert!(error.to_string().contains("startup timed out"), "{error}");
    }
}
