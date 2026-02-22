use async_trait::async_trait;
use blockcell_core::{Error, Result};
use futures::{SinkExt, StreamExt};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error, info, warn};

use crate::{Tool, ToolContext, ToolSchema};

/// Global stream manager — holds all active subscriptions.
static STREAM_MANAGER: Lazy<Arc<Mutex<StreamManager>>> =
    Lazy::new(|| Arc::new(Mutex::new(StreamManager::new())));

/// Whether we have already restored persisted subscriptions on this process run.
static RESTORED: Lazy<Arc<Mutex<bool>>> = Lazy::new(|| Arc::new(Mutex::new(false)));

/// Serializable subscription rule — persisted to disk for auto-restore.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SubscriptionRule {
    id: String,
    url: String,
    protocol: String,
    buffer_size: usize,
    filter: Option<String>,
    headers: HashMap<String, String>,
    init_message: Option<String>,
    created_at: i64,
    /// Whether this subscription should auto-restore on process restart.
    auto_restore: bool,
    /// Max reconnect attempts before giving up (0 = unlimited).
    max_reconnect: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StreamSubscription {
    id: String,
    url: String,
    protocol: String, // "websocket" | "sse"
    status: String,    // "connecting" | "connected" | "disconnected" | "error"
    /// Number of messages received so far.
    message_count: u64,
    /// Last N messages buffered for retrieval.
    #[serde(skip)]
    buffer: Vec<StreamMessage>,
    /// Max buffer size.
    buffer_size: usize,
    /// Optional JSON path filter — only buffer messages matching this path.
    filter: Option<String>,
    /// Headers to send on connect.
    headers: HashMap<String, String>,
    /// Optional initial message to send after WebSocket connect (e.g. subscribe command).
    init_message: Option<String>,
    created_at: i64,
    last_message_at: Option<i64>,
    error: Option<String>,
    /// Whether this subscription should auto-restore on restart.
    auto_restore: bool,
    /// Current reconnect attempt count.
    reconnect_count: u32,
    /// Max reconnect attempts (0 = unlimited).
    max_reconnect: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StreamMessage {
    timestamp: i64,
    data: String,
}

struct StreamManager {
    subscriptions: HashMap<String, StreamSubscription>,
    /// Handles to cancel running stream tasks.
    cancel_handles: HashMap<String, tokio::sync::watch::Sender<bool>>,
    /// Workspace path for persistence (set on first tool execution).
    workspace: Option<PathBuf>,
}

impl StreamManager {
    fn new() -> Self {
        Self {
            subscriptions: HashMap::new(),
            cancel_handles: HashMap::new(),
            workspace: None,
        }
    }

    fn persistence_path(&self) -> Option<PathBuf> {
        self.workspace.as_ref().map(|ws| ws.join("streams").join("subscriptions.json"))
    }

    /// Save all auto_restore subscriptions to disk.
    fn save_rules(&self) {
        let path = match self.persistence_path() {
            Some(p) => p,
            None => return,
        };
        let rules: Vec<SubscriptionRule> = self.subscriptions.values()
            .filter(|s| s.auto_restore)
            .map(|s| SubscriptionRule {
                id: s.id.clone(),
                url: s.url.clone(),
                protocol: s.protocol.clone(),
                buffer_size: s.buffer_size,
                filter: s.filter.clone(),
                headers: s.headers.clone(),
                init_message: s.init_message.clone(),
                created_at: s.created_at,
                auto_restore: true,
                max_reconnect: s.max_reconnect,
            })
            .collect();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&rules) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    warn!(error = %e, "Failed to persist stream subscriptions");
                } else {
                    debug!(count = rules.len(), path = %path.display(), "Persisted stream subscriptions");
                }
            }
            Err(e) => warn!(error = %e, "Failed to serialize stream subscriptions"),
        }
    }

    /// Load persisted subscription rules from disk.
    fn load_rules(&self) -> Vec<SubscriptionRule> {
        let path = match self.persistence_path() {
            Some(p) => p,
            None => return vec![],
        };
        match std::fs::read_to_string(&path) {
            Ok(json) => {
                serde_json::from_str(&json).unwrap_or_else(|e| {
                    warn!(error = %e, "Failed to parse persisted subscriptions");
                    vec![]
                })
            }
            Err(_) => vec![], // File doesn't exist yet
        }
    }
}

pub struct StreamSubscribeTool;

#[async_trait]
impl Tool for StreamSubscribeTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "stream_subscribe",
            description: "Subscribe to real-time data streams via WebSocket or SSE (Server-Sent Events). \
                Manage persistent connections for live market data, blockchain events, news feeds, etc. \
                Actions: 'subscribe' (create new stream), 'unsubscribe' (close stream), 'read' (get buffered messages), \
                'send' (send message to WebSocket), 'list' (list active streams), 'status' (get stream status).",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["subscribe", "unsubscribe", "read", "send", "list", "status", "restore"],
                        "description": "Action to perform. 'restore' re-connects all persisted subscriptions."
                    },
                    "url": {
                        "type": "string",
                        "description": "(subscribe) WebSocket (wss://...) or SSE (https://...) URL"
                    },
                    "protocol": {
                        "type": "string",
                        "enum": ["websocket", "sse", "auto"],
                        "description": "(subscribe) Protocol type. 'auto' detects from URL scheme. Default: auto"
                    },
                    "stream_id": {
                        "type": "string",
                        "description": "(unsubscribe/read/send/status) Stream ID to operate on"
                    },
                    "init_message": {
                        "type": "string",
                        "description": "(subscribe) JSON message to send immediately after WebSocket connect (e.g. subscription command)"
                    },
                    "message": {
                        "type": "string",
                        "description": "(send) Message to send to the WebSocket stream"
                    },
                    "headers": {
                        "type": "object",
                        "description": "(subscribe) Custom headers for the connection"
                    },
                    "filter": {
                        "type": "string",
                        "description": "(subscribe) JSON path filter to select specific fields from messages (e.g. 'data.price')"
                    },
                    "buffer_size": {
                        "type": "integer",
                        "description": "(subscribe) Max messages to buffer for read. Default: 100"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "(read) Max messages to return. Default: 20"
                    },
                    "since_timestamp": {
                        "type": "integer",
                        "description": "(read) Only return messages after this Unix timestamp (ms)"
                    },
                    "auto_restore": {
                        "type": "boolean",
                        "description": "(subscribe) If true, this subscription will be persisted and auto-restored on process restart. Default: true"
                    },
                    "max_reconnect": {
                        "type": "integer",
                        "description": "(subscribe) Max reconnect attempts on disconnect (0=unlimited). Default: 0"
                    },
                    "preset": {
                        "type": "string",
                        "description": "(subscribe) CEX/blockchain stream preset. Auto-configures url+init_message. Format: '{exchange}:{stream_type}:{symbol}'. Examples: 'binance:trade:btcusdt', 'binance:kline:ethusdt:1m', 'binance:depth:btcusdt', 'binance:ticker:btcusdt', 'okx:trade:BTC-USDT', 'okx:ticker:BTC-USDT', 'bybit:trade:BTCUSDT', 'bybit:ticker:BTCUSDT', 'binance:!ticker@arr' (all tickers), 'ethereum:mempool' (pending txs via WSS RPC), 'ethereum:newHeads' (new blocks), 'ethereum:logs:{address}' (contract events)"
                    },
                    "symbol": {
                        "type": "string",
                        "description": "(subscribe with preset) Trading pair symbol, e.g. 'btcusdt'. Alternative to embedding in preset string."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "subscribe" => {
                let has_url = !params.get("url").and_then(|v| v.as_str()).unwrap_or("").is_empty();
                let has_preset = !params.get("preset").and_then(|v| v.as_str()).unwrap_or("").is_empty();
                if !has_url && !has_preset {
                    return Err(Error::Validation("'url' or 'preset' is required for subscribe".into()));
                }
            }
            "restore" => {}
            "unsubscribe" | "read" | "send" | "status" => {
                if params.get("stream_id").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Validation("'stream_id' is required".into()));
                }
            }
            "list" => {}
            _ => return Err(Error::Validation(format!("Unknown action: {}", action))),
        }
        if action == "send"
            && params.get("message").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                return Err(Error::Validation("'message' is required for send".into()));
            }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        // Set workspace path for persistence on first call
        {
            let mut mgr = STREAM_MANAGER.lock().await;
            if mgr.workspace.is_none() {
                mgr.workspace = Some(ctx.workspace.clone());
            }
        }

        // Auto-restore persisted subscriptions on first call in this process
        {
            let mut restored = RESTORED.lock().await;
            if !*restored {
                *restored = true;
                drop(restored);
                let _ = restore_all_subscriptions().await;
            }
        }

        let action = params["action"].as_str().unwrap();
        match action {
            "subscribe" => action_subscribe(&params).await,
            "unsubscribe" => action_unsubscribe(&params).await,
            "read" => action_read(&params).await,
            "send" => action_send(&params).await,
            "list" => action_list().await,
            "status" => action_status(&params).await,
            "restore" => action_restore().await,
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

/// Resolve a CEX/blockchain stream preset into (url, init_message, filter).
fn resolve_preset(preset: &str, symbol_override: Option<&str>) -> Result<(String, Option<String>, Option<String>)> {
    let parts: Vec<&str> = preset.split(':').collect();
    if parts.is_empty() {
        return Err(Error::Tool("Empty preset string".into()));
    }

    let exchange = parts[0].to_lowercase();
    let stream_type = parts.get(1).map(|s| s.to_lowercase()).unwrap_or_default();
    let symbol_part = symbol_override.map(|s| s.to_lowercase())
        .or_else(|| parts.get(2).map(|s| s.to_lowercase()))
        .unwrap_or_default();
    let extra = parts.get(3).map(|s| s.to_lowercase());

    match exchange.as_str() {
        "binance" => {
            let sym = symbol_part.to_lowercase();
            match stream_type.as_str() {
                "trade" => Ok((
                    format!("wss://stream.binance.com:9443/ws/{}@trade", sym),
                    None,
                    None,
                )),
                "kline" | "candlestick" => {
                    let interval = extra.as_deref().unwrap_or("1m");
                    Ok((
                        format!("wss://stream.binance.com:9443/ws/{}@kline_{}", sym, interval),
                        None,
                        None,
                    ))
                }
                "depth" | "orderbook" => Ok((
                    format!("wss://stream.binance.com:9443/ws/{}@depth20@100ms", sym),
                    None,
                    None,
                )),
                "ticker" => Ok((
                    format!("wss://stream.binance.com:9443/ws/{}@ticker", sym),
                    None,
                    None,
                )),
                "miniticker" => Ok((
                    format!("wss://stream.binance.com:9443/ws/{}@miniTicker", sym),
                    None,
                    None,
                )),
                "aggtrade" => Ok((
                    format!("wss://stream.binance.com:9443/ws/{}@aggTrade", sym),
                    None,
                    None,
                )),
                "!ticker@arr" | "alltickers" => Ok((
                    "wss://stream.binance.com:9443/ws/!ticker@arr".to_string(),
                    None,
                    None,
                )),
                "!miniTicker@arr" | "allminitickers" => Ok((
                    "wss://stream.binance.com:9443/ws/!miniTicker@arr".to_string(),
                    None,
                    None,
                )),
                _ => Err(Error::Tool(format!("Unknown Binance stream type '{}'. Valid: trade, kline, depth, ticker, miniticker, aggtrade, alltickers", stream_type))),
            }
        }
        "okx" => {
            let sym = if symbol_part.contains('-') { symbol_part.to_uppercase() } else if symbol_part.len() > 3 {
                // Try to split: btcusdt -> BTC-USDT
                if symbol_part.ends_with("usdt") { format!("{}-USDT", &symbol_part[..symbol_part.len()-4].to_uppercase()) }
                else { symbol_part.to_uppercase() }
            } else { symbol_part.to_uppercase() };
            let channel = match stream_type.as_str() {
                "trade" | "trades" => "trades",
                "ticker" | "tickers" => "tickers",
                "depth" | "orderbook" | "books" => "books5",
                "kline" | "candle" => {
                    let interval = extra.as_deref().unwrap_or("1m");
                    // OKX candle format: candle1m, candle5m, candle1H, candle1D
                    return Ok((
                        "wss://ws.okx.com:8443/ws/v5/public".to_string(),
                        Some(json!({"op": "subscribe", "args": [{"channel": format!("candle{}", interval), "instId": sym}]}).to_string()),
                        None,
                    ));
                }
                _ => return Err(Error::Tool(format!("Unknown OKX stream type '{}'. Valid: trade, ticker, depth, kline", stream_type))),
            };
            Ok((
                "wss://ws.okx.com:8443/ws/v5/public".to_string(),
                Some(json!({"op": "subscribe", "args": [{"channel": channel, "instId": sym}]}).to_string()),
                None,
            ))
        }
        "bybit" => {
            let sym = symbol_part.to_uppercase().replace('-', "");
            let topic = match stream_type.as_str() {
                "trade" | "trades" => format!("publicTrade.{}", sym),
                "ticker" | "tickers" => format!("tickers.{}", sym),
                "depth" | "orderbook" => format!("orderbook.50.{}", sym),
                "kline" | "candle" => {
                    let interval = extra.as_deref().unwrap_or("1");
                    format!("kline.{}.{}", interval, sym)
                }
                _ => return Err(Error::Tool(format!("Unknown Bybit stream type '{}'. Valid: trade, ticker, depth, kline", stream_type))),
            };
            Ok((
                "wss://stream.bybit.com/v5/public/spot".to_string(),
                Some(json!({"op": "subscribe", "args": [topic]}).to_string()),
                None,
            ))
        }
        "ethereum" | "eth" | "polygon" | "bsc" | "arbitrum" | "base" | "optimism" => {
            // Blockchain node WebSocket subscriptions
            let rpc_wss = match exchange.as_str() {
                "ethereum" | "eth" => "wss://ethereum-rpc.publicnode.com",
                "polygon" => "wss://polygon-bor-rpc.publicnode.com",
                "bsc" => "wss://bsc-rpc.publicnode.com",
                "arbitrum" => "wss://arbitrum-one-rpc.publicnode.com",
                "base" => "wss://base-rpc.publicnode.com",
                "optimism" => "wss://optimism-rpc.publicnode.com",
                _ => "wss://ethereum-rpc.publicnode.com",
            };
            match stream_type.as_str() {
                "mempool" | "pendingtransactions" | "pending" => Ok((
                    rpc_wss.to_string(),
                    Some(json!({"jsonrpc":"2.0","id":1,"method":"eth_subscribe","params":["newPendingTransactions"]}).to_string()),
                    None,
                )),
                "newheads" | "blocks" | "newblocks" => Ok((
                    rpc_wss.to_string(),
                    Some(json!({"jsonrpc":"2.0","id":1,"method":"eth_subscribe","params":["newHeads"]}).to_string()),
                    None,
                )),
                "logs" => {
                    let address = if !symbol_part.is_empty() { json!({"address": symbol_part}) } else { json!({}) };
                    Ok((
                        rpc_wss.to_string(),
                        Some(json!({"jsonrpc":"2.0","id":1,"method":"eth_subscribe","params":["logs", address]}).to_string()),
                        None,
                    ))
                }
                _ => Err(Error::Tool(format!("Unknown blockchain stream type '{}'. Valid: mempool, newheads, logs", stream_type))),
            }
        }
        _ => Err(Error::Tool(format!("Unknown preset exchange '{}'. Valid: binance, okx, bybit, ethereum, polygon, bsc, arbitrum, base, optimism", exchange))),
    }
}

async fn action_subscribe(params: &Value) -> Result<Value> {
    // Resolve preset if provided
    let (url, init_message, filter) = if let Some(preset) = params.get("preset").and_then(|v| v.as_str()) {
        if !preset.is_empty() {
            let symbol_override = params.get("symbol").and_then(|v| v.as_str());
            let (preset_url, preset_init, preset_filter) = resolve_preset(preset, symbol_override)?;
            // Allow explicit overrides
            let url = params.get("url").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
                .map(|s| s.to_string()).unwrap_or(preset_url);
            let init = params.get("init_message").and_then(|v| v.as_str()).map(|s| s.to_string())
                .or(preset_init);
            let filt = params.get("filter").and_then(|v| v.as_str()).map(|s| s.to_string())
                .or(preset_filter);
            (url, init, filt)
        } else {
            (
                params["url"].as_str().unwrap().to_string(),
                params.get("init_message").and_then(|v| v.as_str()).map(|s| s.to_string()),
                params.get("filter").and_then(|v| v.as_str()).map(|s| s.to_string()),
            )
        }
    } else {
        (
            params["url"].as_str().unwrap().to_string(),
            params.get("init_message").and_then(|v| v.as_str()).map(|s| s.to_string()),
            params.get("filter").and_then(|v| v.as_str()).map(|s| s.to_string()),
        )
    };

    let protocol = params.get("protocol").and_then(|v| v.as_str()).unwrap_or("auto");
    let buffer_size = params.get("buffer_size").and_then(|v| v.as_u64()).unwrap_or(100) as usize;

    let mut headers = HashMap::new();
    if let Some(h) = params.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in h {
            if let Some(val) = v.as_str() {
                headers.insert(k.clone(), val.to_string());
            }
        }
    }

    // Detect protocol
    let detected_protocol = if protocol == "auto" {
        if url.starts_with("wss://") || url.starts_with("ws://") {
            "websocket"
        } else {
            "sse"
        }
    } else {
        protocol
    };

    let stream_id = format!("stream_{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("x"));
    let now = chrono::Utc::now().timestamp_millis();

    let auto_restore = params.get("auto_restore").and_then(|v| v.as_bool()).unwrap_or(true);
    let max_reconnect = params.get("max_reconnect").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

    let sub = StreamSubscription {
        id: stream_id.clone(),
        url: url.clone(),
        protocol: detected_protocol.to_string(),
        status: "connecting".to_string(),
        message_count: 0,
        buffer: Vec::new(),
        buffer_size,
        filter: filter.clone(),
        headers: headers.clone(),
        init_message: init_message.clone(),
        created_at: now,
        last_message_at: None,
        error: None,
        auto_restore,
        reconnect_count: 0,
        max_reconnect,
    };

    // Create cancel channel
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

    {
        let mut mgr = STREAM_MANAGER.lock().await;
        mgr.subscriptions.insert(stream_id.clone(), sub);
        mgr.cancel_handles.insert(stream_id.clone(), cancel_tx);
        mgr.save_rules();
    }

    // Spawn background task
    let sid = stream_id.clone();
    let url_clone = url.clone();
    let headers_clone = headers.clone();
    match detected_protocol {
        "websocket" => {
            tokio::spawn(run_websocket_stream(sid, url_clone, headers_clone, init_message, cancel_rx));
        }
        "sse" => {
            tokio::spawn(run_sse_stream(sid, url_clone, headers_clone, cancel_rx));
        }
        _ => {
            return Err(Error::Tool(format!("Unknown protocol: {}", detected_protocol)));
        }
    }

    Ok(json!({
        "stream_id": stream_id,
        "url": url,
        "protocol": detected_protocol,
        "status": "connecting",
        "buffer_size": buffer_size,
        "filter": filter,
        "auto_restore": auto_restore,
        "max_reconnect": max_reconnect,
        "note": "Stream is connecting in the background. Use action='read' with this stream_id to get messages, or action='status' to check connection state."
    }))
}

async fn run_websocket_stream(
    stream_id: String,
    url: String,
    headers: HashMap<String, String>,
    init_message: Option<String>,
    mut cancel_rx: tokio::sync::watch::Receiver<bool>,
) {
    let mut reconnect_attempt: u32 = 0;

    'reconnect: loop {
        info!(stream_id = %stream_id, url = %url, attempt = reconnect_attempt, "WebSocket stream connecting");

        // Build request with custom headers
        let mut request = match url.parse::<tokio_tungstenite::tungstenite::http::Uri>() {
            Ok(uri) => {
                tokio_tungstenite::tungstenite::http::Request::builder()
                    .uri(uri)
                    .header("Connection", "Upgrade")
                    .header("Upgrade", "websocket")
                    .header("Sec-WebSocket-Version", "13")
                    .header("Sec-WebSocket-Key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
            }
            Err(e) => {
                set_stream_error(&stream_id, &format!("Invalid URL: {}", e)).await;
                return;
            }
        };

        for (k, v) in &headers {
            request = request.header(k.as_str(), v.as_str());
        }

        let request = match request.body(()) {
            Ok(r) => r,
            Err(e) => {
                set_stream_error(&stream_id, &format!("Failed to build request: {}", e)).await;
                return;
            }
        };

        let ws_stream = match tokio_tungstenite::connect_async(request).await {
            Ok((stream, _)) => stream,
            Err(e) => {
                warn!(stream_id = %stream_id, error = %e, attempt = reconnect_attempt, "WebSocket connect failed");
                if should_reconnect(&stream_id, reconnect_attempt).await {
                    reconnect_attempt += 1;
                    let delay = reconnect_delay(reconnect_attempt);
                    update_reconnect_count(&stream_id, reconnect_attempt).await;
                    set_stream_error(&stream_id, &format!("Connect failed (attempt {}), retrying in {}s: {}", reconnect_attempt, delay.as_secs(), e)).await;
                    tokio::time::sleep(delay).await;
                    continue 'reconnect;
                } else {
                    set_stream_error(&stream_id, &format!("WebSocket connect failed (gave up after {} attempts): {}", reconnect_attempt, e)).await;
                    return;
                }
            }
        };

        // Reset reconnect count on successful connect
        reconnect_attempt = 0;
        update_reconnect_count(&stream_id, 0).await;
        set_stream_status(&stream_id, "connected").await;
        info!(stream_id = %stream_id, "WebSocket connected");

        let (mut write, mut read) = ws_stream.split();

        // Send init message if provided
        if let Some(ref init_msg) = init_message {
            if let Err(e) = write.send(WsMessage::Text(init_msg.clone())).await {
                warn!(stream_id = %stream_id, error = %e, "Failed to send init message");
            }
        }

        // Store write half for send action
        let write_handle: Arc<Mutex<Option<futures::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
            WsMessage
        >>>> = Arc::new(Mutex::new(Some(write)));

        {
            let mut ws_writers = WS_WRITERS.lock().await;
            ws_writers.insert(stream_id.clone(), write_handle.clone());
        }

        let mut was_cancelled = false;
        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(WsMessage::Text(text))) => {
                            buffer_message(&stream_id, &text).await;
                        }
                        Some(Ok(WsMessage::Binary(data))) => {
                            if let Ok(text) = String::from_utf8(data) {
                                buffer_message(&stream_id, &text).await;
                            }
                        }
                        Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => {}
                        Some(Ok(WsMessage::Close(_))) => {
                            info!(stream_id = %stream_id, "WebSocket closed by server");
                            set_stream_status(&stream_id, "disconnected").await;
                            break;
                        }
                        Some(Err(e)) => {
                            warn!(stream_id = %stream_id, error = %e, "WebSocket error");
                            set_stream_error(&stream_id, &format!("WebSocket error: {}", e)).await;
                            break;
                        }
                        None => {
                            set_stream_status(&stream_id, "disconnected").await;
                            break;
                        }
                        _ => {}
                    }
                }
                _ = cancel_rx.changed() => {
                    if *cancel_rx.borrow() {
                        info!(stream_id = %stream_id, "WebSocket stream cancelled");
                        set_stream_status(&stream_id, "disconnected").await;
                        was_cancelled = true;
                        break;
                    }
                }
            }
        }

        // Cleanup write handle
        {
            let mut ws_writers = WS_WRITERS.lock().await;
            ws_writers.remove(&stream_id);
        }

        // If cancelled by user, don't reconnect
        if was_cancelled {
            return;
        }

        // Try to reconnect
        if should_reconnect(&stream_id, reconnect_attempt).await {
            reconnect_attempt += 1;
            let delay = reconnect_delay(reconnect_attempt);
            update_reconnect_count(&stream_id, reconnect_attempt).await;
            info!(stream_id = %stream_id, attempt = reconnect_attempt, delay_secs = delay.as_secs(), "Reconnecting WebSocket");
            tokio::time::sleep(delay).await;
        } else {
            return;
        }
    } // 'reconnect loop
}

/// Global WebSocket write handles for the send action.
static WS_WRITERS: Lazy<Arc<Mutex<HashMap<String, Arc<Mutex<Option<futures::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WsMessage
>>>>>>>> = Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

async fn run_sse_stream(
    stream_id: String,
    url: String,
    headers: HashMap<String, String>,
    mut cancel_rx: tokio::sync::watch::Receiver<bool>,
) {
    let mut reconnect_attempt: u32 = 0;

    'reconnect: loop {
        info!(stream_id = %stream_id, url = %url, attempt = reconnect_attempt, "SSE stream connecting");

        let client = reqwest::Client::new();
        let mut req = client.get(&url)
            .header("Accept", "text/event-stream")
            .header("Cache-Control", "no-cache");

        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let mut response = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                warn!(stream_id = %stream_id, error = %e, attempt = reconnect_attempt, "SSE connect failed");
                if should_reconnect(&stream_id, reconnect_attempt).await {
                    reconnect_attempt += 1;
                    let delay = reconnect_delay(reconnect_attempt);
                    update_reconnect_count(&stream_id, reconnect_attempt).await;
                    set_stream_error(&stream_id, &format!("SSE connect failed (attempt {}), retrying in {}s: {}", reconnect_attempt, delay.as_secs(), e)).await;
                    tokio::time::sleep(delay).await;
                    continue 'reconnect;
                } else {
                    set_stream_error(&stream_id, &format!("SSE connect failed (gave up after {} attempts): {}", reconnect_attempt, e)).await;
                    return;
                }
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            if should_reconnect(&stream_id, reconnect_attempt).await {
                reconnect_attempt += 1;
                let delay = reconnect_delay(reconnect_attempt);
                update_reconnect_count(&stream_id, reconnect_attempt).await;
                set_stream_error(&stream_id, &format!("SSE HTTP {} (attempt {}), retrying in {}s", status, reconnect_attempt, delay.as_secs())).await;
                tokio::time::sleep(delay).await;
                continue 'reconnect;
            } else {
                set_stream_error(&stream_id, &format!("SSE HTTP error: {}", status)).await;
                return;
            }
        }

        reconnect_attempt = 0;
        update_reconnect_count(&stream_id, 0).await;
        set_stream_status(&stream_id, "connected").await;
        info!(stream_id = %stream_id, "SSE connected");

        let mut partial_line = String::new();
        let mut event_data = String::new();
        let mut was_cancelled = false;

        loop {
            tokio::select! {
                chunk = response.chunk() => {
                    match chunk {
                        Ok(Some(bytes)) => {
                            let text = String::from_utf8_lossy(&bytes);
                            partial_line.push_str(&text);

                            while let Some(pos) = partial_line.find('\n') {
                                let line = partial_line[..pos].trim_end_matches('\r').to_string();
                                partial_line = partial_line[pos + 1..].to_string();

                                if line.is_empty() {
                                    if !event_data.is_empty() {
                                        buffer_message(&stream_id, event_data.trim()).await;
                                        event_data.clear();
                                    }
                                } else if let Some(data) = line.strip_prefix("data: ") {
                                    if !event_data.is_empty() {
                                        event_data.push('\n');
                                    }
                                    event_data.push_str(data);
                                } else if line.starts_with("data:") {
                                    let data = line.strip_prefix("data:").unwrap_or("");
                                    if !event_data.is_empty() {
                                        event_data.push('\n');
                                    }
                                    event_data.push_str(data.trim_start());
                                }
                            }
                        }
                        Ok(None) => {
                            set_stream_status(&stream_id, "disconnected").await;
                            break;
                        }
                        Err(e) => {
                            warn!(stream_id = %stream_id, error = %e, "SSE stream error");
                            set_stream_error(&stream_id, &format!("SSE error: {}", e)).await;
                            break;
                        }
                    }
                }
                _ = cancel_rx.changed() => {
                    if *cancel_rx.borrow() {
                        info!(stream_id = %stream_id, "SSE stream cancelled");
                        set_stream_status(&stream_id, "disconnected").await;
                        was_cancelled = true;
                        break;
                    }
                }
            }
        }

        if was_cancelled {
            return;
        }

        // Try to reconnect
        if should_reconnect(&stream_id, reconnect_attempt).await {
            reconnect_attempt += 1;
            let delay = reconnect_delay(reconnect_attempt);
            update_reconnect_count(&stream_id, reconnect_attempt).await;
            info!(stream_id = %stream_id, attempt = reconnect_attempt, delay_secs = delay.as_secs(), "Reconnecting SSE");
            tokio::time::sleep(delay).await;
        } else {
            return;
        }
    } // 'reconnect loop
}

async fn buffer_message(stream_id: &str, data: &str) {
    let now = chrono::Utc::now().timestamp_millis();
    let mut mgr = STREAM_MANAGER.lock().await;
    if let Some(sub) = mgr.subscriptions.get_mut(stream_id) {
        // Apply filter if set
        let should_buffer = if let Some(ref filter_path) = sub.filter {
            // Simple JSON path filter: try to parse as JSON and extract field
            if let Ok(parsed) = serde_json::from_str::<Value>(data) {
                let parts: Vec<&str> = filter_path.split('.').collect();
                let mut current = &parsed;
                let mut found = true;
                for part in &parts {
                    if let Some(next) = current.get(part) {
                        current = next;
                    } else {
                        found = false;
                        break;
                    }
                }
                found
            } else {
                true // Non-JSON data always passes
            }
        } else {
            true
        };

        if should_buffer {
            sub.message_count += 1;
            sub.last_message_at = Some(now);
            sub.buffer.push(StreamMessage {
                timestamp: now,
                data: data.to_string(),
            });
            // Trim buffer to max size
            while sub.buffer.len() > sub.buffer_size {
                sub.buffer.remove(0);
            }
        }
    }
}

/// Check if a stream should attempt reconnection.
async fn should_reconnect(stream_id: &str, current_attempt: u32) -> bool {
    let mgr = STREAM_MANAGER.lock().await;
    if let Some(sub) = mgr.subscriptions.get(stream_id) {
        if !sub.auto_restore {
            return false;
        }
        if sub.max_reconnect > 0 && current_attempt >= sub.max_reconnect {
            return false;
        }
        true
    } else {
        false // subscription was removed
    }
}

/// Exponential backoff: 2s, 4s, 8s, 16s, 30s (capped)
fn reconnect_delay(attempt: u32) -> std::time::Duration {
    let secs = (2u64.pow(attempt.min(5))).min(30);
    std::time::Duration::from_secs(secs)
}

/// Update the reconnect_count field on a subscription.
async fn update_reconnect_count(stream_id: &str, count: u32) {
    let mut mgr = STREAM_MANAGER.lock().await;
    if let Some(sub) = mgr.subscriptions.get_mut(stream_id) {
        sub.reconnect_count = count;
    }
}

async fn set_stream_status(stream_id: &str, status: &str) {
    let mut mgr = STREAM_MANAGER.lock().await;
    if let Some(sub) = mgr.subscriptions.get_mut(stream_id) {
        sub.status = status.to_string();
        if status == "disconnected" || status == "error" {
            sub.error = None;
        }
    }
}

async fn set_stream_error(stream_id: &str, error: &str) {
    let mut mgr = STREAM_MANAGER.lock().await;
    if let Some(sub) = mgr.subscriptions.get_mut(stream_id) {
        sub.status = "error".to_string();
        sub.error = Some(error.to_string());
    }
    error!(stream_id = %stream_id, error = %error, "Stream error");
}

async fn action_unsubscribe(params: &Value) -> Result<Value> {
    let stream_id = params["stream_id"].as_str().unwrap();
    let mut mgr = STREAM_MANAGER.lock().await;

    if let Some(cancel_tx) = mgr.cancel_handles.remove(stream_id) {
        let _ = cancel_tx.send(true);
    }

    let removed = mgr.subscriptions.remove(stream_id).is_some();
    if removed {
        mgr.save_rules();
    }

    // Also remove WS writer if any
    drop(mgr);
    {
        let mut ws_writers = WS_WRITERS.lock().await;
        ws_writers.remove(stream_id);
    }

    Ok(json!({
        "stream_id": stream_id,
        "removed": removed,
        "status": "disconnected"
    }))
}

async fn action_read(params: &Value) -> Result<Value> {
    let stream_id = params["stream_id"].as_str().unwrap();
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let since = params.get("since_timestamp").and_then(|v| v.as_i64());

    let mgr = STREAM_MANAGER.lock().await;
    let sub = mgr.subscriptions.get(stream_id)
        .ok_or_else(|| Error::Tool(format!("Stream '{}' not found", stream_id)))?;

    // Collect filtered messages, then take last N
    let filtered: Vec<&StreamMessage> = sub.buffer.iter()
        .filter(|m| since.is_none_or(|ts| m.timestamp > ts))
        .collect();
    let skip = filtered.len().saturating_sub(limit);
    let messages: Vec<Value> = filtered.into_iter()
        .skip(skip)
        .map(|m| {
            // Try to parse as JSON for cleaner output
            let data = serde_json::from_str::<Value>(&m.data)
                .unwrap_or_else(|_| Value::String(m.data.clone()));
            json!({
                "timestamp": m.timestamp,
                "data": data
            })
        })
        .collect();

    Ok(json!({
        "stream_id": stream_id,
        "status": sub.status,
        "total_received": sub.message_count,
        "buffered": sub.buffer.len(),
        "returned": messages.len(),
        "messages": messages
    }))
}

async fn action_send(params: &Value) -> Result<Value> {
    let stream_id = params["stream_id"].as_str().unwrap();
    let message = params["message"].as_str().unwrap();

    // Check protocol
    {
        let mgr = STREAM_MANAGER.lock().await;
        let sub = mgr.subscriptions.get(stream_id)
            .ok_or_else(|| Error::Tool(format!("Stream '{}' not found", stream_id)))?;
        if sub.protocol != "websocket" {
            return Err(Error::Tool("Can only send messages to WebSocket streams".into()));
        }
        if sub.status != "connected" {
            return Err(Error::Tool(format!("Stream is not connected (status: {})", sub.status)));
        }
    }

    let ws_writers = WS_WRITERS.lock().await;
    let writer_handle = ws_writers.get(stream_id)
        .ok_or_else(|| Error::Tool("WebSocket writer not available".into()))?
        .clone();
    drop(ws_writers);

    let mut writer_guard = writer_handle.lock().await;
    if let Some(ref mut writer) = *writer_guard {
        writer.send(WsMessage::Text(message.to_string())).await
            .map_err(|e| Error::Tool(format!("Failed to send: {}", e)))?;
        Ok(json!({
            "stream_id": stream_id,
            "sent": true,
            "message_length": message.len()
        }))
    } else {
        Err(Error::Tool("WebSocket writer closed".into()))
    }
}

async fn action_list() -> Result<Value> {
    let mgr = STREAM_MANAGER.lock().await;
    let streams: Vec<Value> = mgr.subscriptions.values().map(|sub| {
        json!({
            "stream_id": sub.id,
            "url": sub.url,
            "protocol": sub.protocol,
            "status": sub.status,
            "message_count": sub.message_count,
            "buffered": sub.buffer.len(),
            "created_at": sub.created_at,
            "last_message_at": sub.last_message_at,
            "error": sub.error,
            "auto_restore": sub.auto_restore,
            "reconnect_count": sub.reconnect_count,
        })
    }).collect();

    Ok(json!({
        "streams": streams,
        "count": streams.len()
    }))
}

async fn action_status(params: &Value) -> Result<Value> {
    let stream_id = params["stream_id"].as_str().unwrap();
    let mgr = STREAM_MANAGER.lock().await;
    let sub = mgr.subscriptions.get(stream_id)
        .ok_or_else(|| Error::Tool(format!("Stream '{}' not found", stream_id)))?;

    Ok(json!({
        "stream_id": sub.id,
        "url": sub.url,
        "protocol": sub.protocol,
        "status": sub.status,
        "message_count": sub.message_count,
        "buffered": sub.buffer.len(),
        "buffer_size": sub.buffer_size,
        "filter": sub.filter,
        "created_at": sub.created_at,
        "last_message_at": sub.last_message_at,
        "error": sub.error,
        "auto_restore": sub.auto_restore,
        "reconnect_count": sub.reconnect_count,
        "max_reconnect": sub.max_reconnect,
    }))
}

/// Restore all persisted subscriptions from disk.
async fn restore_all_subscriptions() -> Result<Value> {
    let rules = {
        let mgr = STREAM_MANAGER.lock().await;
        mgr.load_rules()
    };

    if rules.is_empty() {
        return Ok(json!({ "restored": 0, "note": "No persisted subscriptions found" }));
    }

    let mut restored = 0;
    for rule in &rules {
        // Skip if already active
        {
            let mgr = STREAM_MANAGER.lock().await;
            if mgr.subscriptions.contains_key(&rule.id) {
                continue;
            }
        }

        let now = chrono::Utc::now().timestamp_millis();
        let sub = StreamSubscription {
            id: rule.id.clone(),
            url: rule.url.clone(),
            protocol: rule.protocol.clone(),
            status: "connecting".to_string(),
            message_count: 0,
            buffer: Vec::new(),
            buffer_size: rule.buffer_size,
            filter: rule.filter.clone(),
            headers: rule.headers.clone(),
            init_message: rule.init_message.clone(),
            created_at: now,
            last_message_at: None,
            error: None,
            auto_restore: true,
            reconnect_count: 0,
            max_reconnect: rule.max_reconnect,
        };

        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);

        {
            let mut mgr = STREAM_MANAGER.lock().await;
            mgr.subscriptions.insert(rule.id.clone(), sub);
            mgr.cancel_handles.insert(rule.id.clone(), cancel_tx);
        }

        let sid = rule.id.clone();
        let url = rule.url.clone();
        let headers = rule.headers.clone();
        let init_message = rule.init_message.clone();
        let protocol = rule.protocol.clone();

        match protocol.as_str() {
            "websocket" => {
                tokio::spawn(run_websocket_stream(sid, url, headers, init_message, cancel_rx));
            }
            "sse" => {
                tokio::spawn(run_sse_stream(sid, url, headers, cancel_rx));
            }
            _ => {
                warn!(protocol = %protocol, "Unknown protocol in persisted rule, skipping");
                continue;
            }
        }

        restored += 1;
        info!(stream_id = %rule.id, url = %rule.url, "Restored persisted subscription");
    }

    Ok(json!({
        "restored": restored,
        "total_rules": rules.len()
    }))
}

async fn action_restore() -> Result<Value> {
    restore_all_subscriptions().await
}

// ---------------------------------------------------------------------------
// Public accessors for gateway API
// ---------------------------------------------------------------------------

/// List all active stream subscriptions (for gateway /v1/streams endpoint).
pub async fn list_streams() -> Value {
    action_list().await.unwrap_or_else(|_| json!({"streams": [], "count": 0}))
}

/// Get buffered data for a specific stream (for gateway /v1/streams/:id/data endpoint).
pub async fn get_stream_data(stream_id: &str, limit: usize) -> Result<Value> {
    let params = json!({"stream_id": stream_id, "limit": limit});
    action_read(&params).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = StreamSubscribeTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "stream_subscribe");
    }

    #[test]
    fn test_validate_subscribe() {
        let tool = StreamSubscribeTool;
        let params = json!({"action": "subscribe", "url": "wss://example.com/ws"});
        assert!(tool.validate(&params).is_ok());
    }

    #[test]
    fn test_validate_subscribe_missing_url() {
        let tool = StreamSubscribeTool;
        let params = json!({"action": "subscribe"});
        assert!(tool.validate(&params).is_err());
    }

    #[test]
    fn test_validate_read() {
        let tool = StreamSubscribeTool;
        let params = json!({"action": "read", "stream_id": "stream_abc"});
        assert!(tool.validate(&params).is_ok());
    }

    #[test]
    fn test_validate_send_missing_message() {
        let tool = StreamSubscribeTool;
        let params = json!({"action": "send", "stream_id": "stream_abc"});
        assert!(tool.validate(&params).is_err());
    }

    #[test]
    fn test_validate_restore() {
        let tool = StreamSubscribeTool;
        let params = json!({"action": "restore"});
        assert!(tool.validate(&params).is_ok());
    }

    #[tokio::test]
    async fn test_list_empty() {
        let result = action_list().await.unwrap();
        assert!(result.get("streams").is_some());
        assert!(result.get("count").is_some());
    }

    #[tokio::test]
    async fn test_read_nonexistent() {
        let params = json!({"stream_id": "nonexistent_stream_xyz"});
        let result = action_read(&params).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_reconnect_delay() {
        assert_eq!(reconnect_delay(1).as_secs(), 2);  // 2^1
        assert_eq!(reconnect_delay(2).as_secs(), 4);  // 2^2
        assert_eq!(reconnect_delay(3).as_secs(), 8);  // 2^3
        assert_eq!(reconnect_delay(4).as_secs(), 16); // 2^4
        assert_eq!(reconnect_delay(5).as_secs(), 30); // 2^5=32, capped at 30
        assert_eq!(reconnect_delay(10).as_secs(), 30); // capped
    }

    #[test]
    fn test_subscription_rule_serde() {
        let rule = SubscriptionRule {
            id: "stream_abc".to_string(),
            url: "wss://example.com/ws".to_string(),
            protocol: "websocket".to_string(),
            buffer_size: 100,
            filter: Some("data.price".to_string()),
            headers: HashMap::new(),
            init_message: Some("{\"subscribe\": \"btcusdt\"}".to_string()),
            created_at: 1000,
            auto_restore: true,
            max_reconnect: 5,
        };
        let json = serde_json::to_string(&rule).unwrap();
        let parsed: SubscriptionRule = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "stream_abc");
        assert_eq!(parsed.max_reconnect, 5);
        assert!(parsed.auto_restore);
        assert_eq!(parsed.filter, Some("data.price".to_string()));
    }

    #[test]
    fn test_persistence_path() {
        let mut mgr = StreamManager::new();
        assert!(mgr.persistence_path().is_none());
        mgr.workspace = Some(PathBuf::from("/tmp/test_workspace"));
        let path = mgr.persistence_path().unwrap();
        assert_eq!(path, PathBuf::from("/tmp/test_workspace/streams/subscriptions.json"));
    }

    #[test]
    fn test_validate_subscribe_with_preset() {
        let tool = StreamSubscribeTool;
        // preset alone should be valid
        assert!(tool.validate(&json!({"action": "subscribe", "preset": "binance:trade:btcusdt"})).is_ok());
        // neither url nor preset should fail
        assert!(tool.validate(&json!({"action": "subscribe"})).is_err());
    }

    #[test]
    fn test_preset_binance() {
        let (url, init, _) = resolve_preset("binance:trade:btcusdt", None).unwrap();
        assert_eq!(url, "wss://stream.binance.com:9443/ws/btcusdt@trade");
        assert!(init.is_none());

        let (url, _, _) = resolve_preset("binance:kline:ethusdt:5m", None).unwrap();
        assert_eq!(url, "wss://stream.binance.com:9443/ws/ethusdt@kline_5m");

        let (url, _, _) = resolve_preset("binance:depth:btcusdt", None).unwrap();
        assert!(url.contains("btcusdt@depth20"));

        let (url, _, _) = resolve_preset("binance:ticker:btcusdt", None).unwrap();
        assert!(url.contains("btcusdt@ticker"));

        let (url, _, _) = resolve_preset("binance:alltickers", None).unwrap();
        assert!(url.contains("!ticker@arr"));
    }

    #[test]
    fn test_preset_okx() {
        let (url, init, _) = resolve_preset("okx:trade:BTC-USDT", None).unwrap();
        assert_eq!(url, "wss://ws.okx.com:8443/ws/v5/public");
        let init_str = init.unwrap();
        assert!(init_str.contains("trades"));
        assert!(init_str.contains("BTC-USDT"));
    }

    #[test]
    fn test_preset_bybit() {
        let (url, init, _) = resolve_preset("bybit:trade:BTCUSDT", None).unwrap();
        assert_eq!(url, "wss://stream.bybit.com/v5/public/spot");
        let init_str = init.unwrap();
        assert!(init_str.contains("publicTrade.BTCUSDT"));
    }

    #[test]
    fn test_preset_ethereum() {
        let (url, init, _) = resolve_preset("ethereum:mempool", None).unwrap();
        assert!(url.contains("ethereum"));
        let init_str = init.unwrap();
        assert!(init_str.contains("newPendingTransactions"));

        let (_, init, _) = resolve_preset("ethereum:newheads", None).unwrap();
        assert!(init.unwrap().contains("newHeads"));

        let (_, init, _) = resolve_preset("ethereum:logs:0x1234", None).unwrap();
        assert!(init.unwrap().contains("0x1234"));
    }

    #[test]
    fn test_preset_symbol_override() {
        let (url, _, _) = resolve_preset("binance:trade:btcusdt", Some("ethusdt")).unwrap();
        assert!(url.contains("ethusdt@trade"));
    }

    #[test]
    fn test_preset_invalid() {
        assert!(resolve_preset("unknown:trade:btc", None).is_err());
        assert!(resolve_preset("binance:invalid:btc", None).is_err());
    }
}
