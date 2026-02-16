use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::{Tool, ToolContext, ToolSchema};

/// CEX (Centralized Exchange) trading tool.
///
/// Supports Binance, OKX, and Bybit REST APIs.
/// All trading operations require API key + secret configured in providers section.
/// HMAC-SHA256 signature for authentication.
///
/// **Security**: Trading actions (place_order, cancel_order, transfer) require
/// user confirmation via the agent's confirm mechanism.
pub struct ExchangeApiTool;

#[async_trait]
impl Tool for ExchangeApiTool {
    fn schema(&self) -> ToolSchema {
        let str_prop = |desc: &str| -> Value { json!({"type": "string", "description": desc}) };
        let num_prop = |desc: &str| -> Value { json!({"type": "number", "description": desc}) };
        let int_prop = |desc: &str| -> Value { json!({"type": "integer", "description": desc}) };

        let mut props = serde_json::Map::new();
        props.insert("action".into(), str_prop("Action: get_account|place_order|cancel_order|get_order|list_orders|get_ticker|get_depth|get_klines|get_funding_rate|transfer|get_positions|info"));
        props.insert("exchange".into(), str_prop("Exchange: 'binance'|'okx'|'bybit' (default: binance)"));
        props.insert("symbol".into(), str_prop("Trading pair (e.g. 'BTCUSDT', 'ETH-USDT'). Format auto-normalized per exchange."));
        props.insert("side".into(), str_prop("Order side: 'buy'|'sell'"));
        props.insert("order_type".into(), str_prop("Order type: 'market'|'limit'|'stop_loss'|'take_profit' (default: market)"));
        props.insert("quantity".into(), num_prop("Order quantity (base asset amount)"));
        props.insert("price".into(), num_prop("Limit price (required for limit orders)"));
        props.insert("stop_price".into(), num_prop("Stop/trigger price (for stop_loss/take_profit)"));
        props.insert("order_id".into(), str_prop("Order ID (for cancel_order/get_order)"));
        props.insert("interval".into(), str_prop("Kline interval: '1m'|'5m'|'15m'|'1h'|'4h'|'1d' (default: 1h)"));
        props.insert("limit".into(), int_prop("Number of results (default: 20)"));
        props.insert("from_account".into(), str_prop("(transfer) Source account: 'spot'|'futures'|'margin'"));
        props.insert("to_account".into(), str_prop("(transfer) Target account: 'spot'|'futures'|'margin'"));
        props.insert("amount".into(), num_prop("(transfer) Transfer amount"));
        props.insert("asset".into(), str_prop("Asset/currency code (e.g. 'USDT', 'BTC') for transfer/get_account"));
        props.insert("api_key".into(), str_prop("API key override (default: from config/env)"));
        props.insert("api_secret".into(), str_prop("API secret override (default: from config/env)"));
        props.insert("passphrase".into(), str_prop("(OKX only) API passphrase"));
        props.insert("account_type".into(), str_prop("Account type: 'spot'|'futures'|'margin' (default: spot)"));

        ToolSchema {
            name: "exchange_api",
            description: "Trade on centralized exchanges (Binance/OKX/Bybit). Actions: get_account (balances), \
                place_order (market/limit/stop orders — requires confirmation), cancel_order, get_order, \
                list_orders (open/history), get_ticker (24h stats), get_depth (orderbook), get_klines (candlesticks), \
                get_funding_rate (perpetual futures), transfer (between spot/futures/margin — requires confirmation), \
                get_positions (futures positions), info (exchange capabilities). \
                Requires API key+secret in config providers.{exchange}.api_key / api_secret, or env vars \
                (BINANCE_API_KEY, OKX_API_KEY, BYBIT_API_KEY etc.). \
                ⚠️ Trading operations require user confirmation. Always verify symbol, side, quantity before placing orders.",
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid = [
            "get_account", "place_order", "cancel_order", "get_order",
            "list_orders", "get_ticker", "get_depth", "get_klines",
            "get_funding_rate", "transfer", "get_positions", "info",
        ];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        match action {
            "place_order" => {
                if params.get("symbol").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'symbol' is required for place_order".into()));
                }
                if params.get("side").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'side' (buy/sell) is required for place_order".into()));
                }
                if params.get("quantity").and_then(|v| v.as_f64()).is_none() {
                    return Err(Error::Tool("'quantity' is required for place_order".into()));
                }
                let order_type = params.get("order_type").and_then(|v| v.as_str()).unwrap_or("market");
                if order_type == "limit" && params.get("price").and_then(|v| v.as_f64()).is_none() {
                    return Err(Error::Tool("'price' is required for limit orders".into()));
                }
            }
            "cancel_order" | "get_order" => {
                if params.get("order_id").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                    && params.get("symbol").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                {
                    return Err(Error::Tool("'order_id' or 'symbol' is required".into()));
                }
            }
            "get_ticker" | "get_depth" | "get_klines" | "get_funding_rate" => {
                if params.get("symbol").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool(format!("'symbol' is required for {}", action)));
                }
            }
            "transfer" => {
                if params.get("asset").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'asset' is required for transfer".into()));
                }
                if params.get("amount").and_then(|v| v.as_f64()).is_none() {
                    return Err(Error::Tool("'amount' is required for transfer".into()));
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap_or("");
        let exchange = params.get("exchange").and_then(|v| v.as_str()).unwrap_or("binance");
        let client = Client::new();

        match action {
            "get_account" => self.get_account(&ctx, &params, exchange, &client).await,
            "place_order" => self.place_order(&ctx, &params, exchange, &client).await,
            "cancel_order" => self.cancel_order(&ctx, &params, exchange, &client).await,
            "get_order" => self.get_order(&ctx, &params, exchange, &client).await,
            "list_orders" => self.list_orders(&ctx, &params, exchange, &client).await,
            "get_ticker" => self.get_ticker(&params, exchange, &client).await,
            "get_depth" => self.get_depth(&params, exchange, &client).await,
            "get_klines" => self.get_klines(&params, exchange, &client).await,
            "get_funding_rate" => self.get_funding_rate(&params, exchange, &client).await,
            "transfer" => self.transfer(&ctx, &params, exchange, &client).await,
            "get_positions" => self.get_positions(&ctx, &params, exchange, &client).await,
            "info" => self.info(exchange),
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

impl ExchangeApiTool {
    // ─── Credential Resolution ───

    fn resolve_credentials(ctx: &ToolContext, params: &Value, exchange: &str) -> (String, String, String) {
        let api_key = params.get("api_key").and_then(|v| v.as_str()).map(String::from)
            .or_else(|| ctx.config.providers.get(exchange).map(|p| p.api_key.clone()))
            .or_else(|| std::env::var(format!("{}_API_KEY", exchange.to_uppercase())).ok())
            .unwrap_or_default();

        let api_secret = params.get("api_secret").and_then(|v| v.as_str()).map(String::from)
            .or_else(|| ctx.config.providers.get(exchange).and_then(|p| p.api_base.clone()))
            .or_else(|| std::env::var(format!("{}_API_SECRET", exchange.to_uppercase())).ok())
            .unwrap_or_default();

        let passphrase = params.get("passphrase").and_then(|v| v.as_str()).map(String::from)
            .or_else(|| std::env::var(format!("{}_PASSPHRASE", exchange.to_uppercase())).ok())
            .unwrap_or_default();

        (api_key, api_secret, passphrase)
    }

    /// HMAC-SHA256 signature (pure Rust, no external crypto crate).
    /// Uses the same approach as blockchain_rpc's keccak — minimal implementation.
    fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
        // SHA-256 constants
        const BLOCK_SIZE: usize = 64;
        const HASH_SIZE: usize = 32;

        // Pad or hash the key
        let mut k = vec![0u8; BLOCK_SIZE];
        if key.len() > BLOCK_SIZE {
            let hashed = Self::sha256(key);
            k[..HASH_SIZE].copy_from_slice(&hashed);
        } else {
            k[..key.len()].copy_from_slice(key);
        }

        // Inner padding
        let mut ipad = vec![0x36u8; BLOCK_SIZE];
        for i in 0..BLOCK_SIZE {
            ipad[i] ^= k[i];
        }

        // Outer padding
        let mut opad = vec![0x5cu8; BLOCK_SIZE];
        for i in 0..BLOCK_SIZE {
            opad[i] ^= k[i];
        }

        // HMAC = H(opad || H(ipad || message))
        let mut inner = ipad;
        inner.extend_from_slice(message);
        let inner_hash = Self::sha256(&inner);

        let mut outer = opad;
        outer.extend_from_slice(&inner_hash);
        Self::sha256(&outer)
    }

    /// Pure Rust SHA-256 implementation.
    fn sha256(data: &[u8]) -> Vec<u8> {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
            0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
            0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
            0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
            0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
            0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
            0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
        ];

        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
            0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
        ];

        // Pre-processing: padding
        let bit_len = (data.len() as u64) * 8;
        let mut msg = data.to_vec();
        msg.push(0x80);
        while (msg.len() % 64) != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_be_bytes());

        // Process each 512-bit block
        for chunk in msg.chunks(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([chunk[i*4], chunk[i*4+1], chunk[i*4+2], chunk[i*4+3]]);
            }
            for i in 16..64 {
                let s0 = w[i-15].rotate_right(7) ^ w[i-15].rotate_right(18) ^ (w[i-15] >> 3);
                let s1 = w[i-2].rotate_right(17) ^ w[i-2].rotate_right(19) ^ (w[i-2] >> 10);
                w[i] = w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1);
            }

            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let temp1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let temp2 = s0.wrapping_add(maj);

                hh = g; g = f; f = e;
                e = d.wrapping_add(temp1);
                d = c; c = b; b = a;
                a = temp1.wrapping_add(temp2);
            }

            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }

        let mut result = Vec::with_capacity(32);
        for &val in &h {
            result.extend_from_slice(&val.to_be_bytes());
        }
        result
    }

    fn hex_encode(data: &[u8]) -> String {
        data.iter().map(|b| format!("{:02x}", b)).collect()
    }

    fn timestamp_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// Normalize symbol to exchange format.
    /// Binance: BTCUSDT, OKX: BTC-USDT, Bybit: BTCUSDT
    fn normalize_symbol(symbol: &str, exchange: &str) -> String {
        let s = symbol.to_uppercase().replace(' ', "");
        match exchange {
            "okx" => {
                // Convert BTCUSDT → BTC-USDT
                if !s.contains('-') {
                    if s.ends_with("USDT") {
                        format!("{}-USDT", &s[..s.len()-4])
                    } else if s.ends_with("USDC") {
                        format!("{}-USDC", &s[..s.len()-4])
                    } else if s.ends_with("BTC") && s.len() > 3 {
                        format!("{}-BTC", &s[..s.len()-3])
                    } else {
                        s
                    }
                } else {
                    s
                }
            }
            _ => {
                // Binance/Bybit: remove dashes
                s.replace('-', "")
            }
        }
    }

    fn base_url(exchange: &str, account_type: &str) -> &'static str {
        match (exchange, account_type) {
            ("binance", "futures") => "https://fapi.binance.com",
            ("binance", _) => "https://api.binance.com",
            ("okx", _) => "https://www.okx.com",
            ("bybit", _) => "https://api.bybit.com",
            _ => "https://api.binance.com",
        }
    }

    // ─── Binance Signed Request Helper ───

    async fn binance_signed_get(&self, ctx: &ToolContext, params: &Value, exchange: &str, path: &str, query: &str, client: &Client) -> Result<Value> {
        let (api_key, api_secret, _) = Self::resolve_credentials(ctx, params, exchange);
        if api_key.is_empty() || api_secret.is_empty() {
            return Err(Error::Tool(format!("{} API key and secret are required. Set in config providers.{}.api_key/api_base or {}_API_KEY/{}_API_SECRET env vars.",
                exchange, exchange, exchange.to_uppercase(), exchange.to_uppercase())));
        }

        let timestamp = Self::timestamp_ms();
        let query_with_ts = if query.is_empty() {
            format!("timestamp={}", timestamp)
        } else {
            format!("{}&timestamp={}", query, timestamp)
        };

        let signature = Self::hex_encode(&Self::hmac_sha256(api_secret.as_bytes(), query_with_ts.as_bytes()));
        let account_type = params.get("account_type").and_then(|v| v.as_str()).unwrap_or("spot");
        let base = Self::base_url(exchange, account_type);
        let url = format!("{}{}?{}&signature={}", base, path, query_with_ts, signature);

        debug!(url = %url, exchange = exchange, "CEX signed GET");
        let resp = client.get(&url)
            .header("X-MBX-APIKEY", &api_key)
            .send().await
            .map_err(|e| Error::Tool(format!("{} request failed: {}", exchange, e)))?;

        let status = resp.status();
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse {} response: {}", exchange, e)))?;

        if !status.is_success() {
            return Err(Error::Tool(format!("{} API error ({}): {:?}", exchange, status, body)));
        }
        Ok(body)
    }

    async fn binance_signed_post(&self, ctx: &ToolContext, params: &Value, exchange: &str, path: &str, query: &str, client: &Client) -> Result<Value> {
        let (api_key, api_secret, _) = Self::resolve_credentials(ctx, params, exchange);
        if api_key.is_empty() || api_secret.is_empty() {
            return Err(Error::Tool(format!("{} API key and secret are required.", exchange)));
        }

        let timestamp = Self::timestamp_ms();
        let query_with_ts = if query.is_empty() {
            format!("timestamp={}", timestamp)
        } else {
            format!("{}&timestamp={}", query, timestamp)
        };

        let signature = Self::hex_encode(&Self::hmac_sha256(api_secret.as_bytes(), query_with_ts.as_bytes()));
        let account_type = params.get("account_type").and_then(|v| v.as_str()).unwrap_or("spot");
        let base = Self::base_url(exchange, account_type);
        let url = format!("{}{}?{}&signature={}", base, path, query_with_ts, signature);

        debug!(url = %url, exchange = exchange, "CEX signed POST");
        let resp = client.post(&url)
            .header("X-MBX-APIKEY", &api_key)
            .send().await
            .map_err(|e| Error::Tool(format!("{} request failed: {}", exchange, e)))?;

        let status = resp.status();
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse {} response: {}", exchange, e)))?;

        if !status.is_success() {
            return Err(Error::Tool(format!("{} API error ({}): {:?}", exchange, status, body)));
        }
        Ok(body)
    }

    async fn binance_signed_delete(&self, ctx: &ToolContext, params: &Value, exchange: &str, path: &str, query: &str, client: &Client) -> Result<Value> {
        let (api_key, api_secret, _) = Self::resolve_credentials(ctx, params, exchange);
        if api_key.is_empty() || api_secret.is_empty() {
            return Err(Error::Tool(format!("{} API key and secret are required.", exchange)));
        }

        let timestamp = Self::timestamp_ms();
        let query_with_ts = format!("{}&timestamp={}", query, timestamp);
        let signature = Self::hex_encode(&Self::hmac_sha256(api_secret.as_bytes(), query_with_ts.as_bytes()));
        let account_type = params.get("account_type").and_then(|v| v.as_str()).unwrap_or("spot");
        let base = Self::base_url(exchange, account_type);
        let url = format!("{}{}?{}&signature={}", base, path, query_with_ts, signature);

        let resp = client.delete(&url)
            .header("X-MBX-APIKEY", &api_key)
            .send().await
            .map_err(|e| Error::Tool(format!("{} request failed: {}", exchange, e)))?;

        let status = resp.status();
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse {} response: {}", exchange, e)))?;

        if !status.is_success() {
            return Err(Error::Tool(format!("{} API error ({}): {:?}", exchange, status, body)));
        }
        Ok(body)
    }

    // ─── Public API (no auth needed) ───

    async fn get_ticker(&self, params: &Value, exchange: &str, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let sym = Self::normalize_symbol(symbol, exchange);

        let url = match exchange {
            "binance" => format!("https://api.binance.com/api/v3/ticker/24hr?symbol={}", sym),
            "okx" => format!("https://www.okx.com/api/v5/market/ticker?instId={}", sym),
            "bybit" => format!("https://api.bybit.com/v5/market/tickers?category=spot&symbol={}", sym),
            _ => return Err(Error::Tool(format!("Unsupported exchange: {}", exchange))),
        };

        debug!(url = %url, "CEX ticker");
        let resp = client.get(&url).send().await
            .map_err(|e| Error::Tool(format!("{} ticker request failed: {}", exchange, e)))?;
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        match exchange {
            "okx" => {
                let data = body.get("data").and_then(|d| d.as_array())
                    .and_then(|a| a.first()).cloned().unwrap_or(json!({}));
                Ok(json!({
                    "exchange": "okx",
                    "symbol": sym,
                    "last": data.get("last"),
                    "bid": data.get("bidPx"),
                    "ask": data.get("askPx"),
                    "high_24h": data.get("high24h"),
                    "low_24h": data.get("low24h"),
                    "volume_24h": data.get("vol24h"),
                    "change_24h": data.get("sodUtc0"),
                    "timestamp": data.get("ts"),
                }))
            }
            "bybit" => {
                let data = body.get("result").and_then(|r| r.get("list"))
                    .and_then(|l| l.as_array()).and_then(|a| a.first()).cloned().unwrap_or(json!({}));
                Ok(json!({
                    "exchange": "bybit",
                    "symbol": sym,
                    "last": data.get("lastPrice"),
                    "bid": data.get("bid1Price"),
                    "ask": data.get("ask1Price"),
                    "high_24h": data.get("highPrice24h"),
                    "low_24h": data.get("lowPrice24h"),
                    "volume_24h": data.get("volume24h"),
                    "turnover_24h": data.get("turnover24h"),
                    "change_24h_pct": data.get("price24hPcnt"),
                }))
            }
            _ => {
                // Binance format
                Ok(json!({
                    "exchange": "binance",
                    "symbol": body.get("symbol"),
                    "last": body.get("lastPrice"),
                    "bid": body.get("bidPrice"),
                    "ask": body.get("askPrice"),
                    "high_24h": body.get("highPrice"),
                    "low_24h": body.get("lowPrice"),
                    "volume_24h": body.get("volume"),
                    "quote_volume_24h": body.get("quoteVolume"),
                    "change_24h": body.get("priceChange"),
                    "change_24h_pct": body.get("priceChangePercent"),
                    "trades_count": body.get("count"),
                }))
            }
        }
    }

    async fn get_depth(&self, params: &Value, exchange: &str, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let sym = Self::normalize_symbol(symbol, exchange);
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20);

        let url = match exchange {
            "binance" => format!("https://api.binance.com/api/v3/depth?symbol={}&limit={}", sym, limit),
            "okx" => format!("https://www.okx.com/api/v5/market/books?instId={}&sz={}", sym, limit),
            "bybit" => format!("https://api.bybit.com/v5/market/orderbook?category=spot&symbol={}&limit={}", sym, limit),
            _ => return Err(Error::Tool(format!("Unsupported exchange: {}", exchange))),
        };

        let resp = client.get(&url).send().await
            .map_err(|e| Error::Tool(format!("{} depth request failed: {}", exchange, e)))?;
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        Ok(json!({
            "exchange": exchange,
            "symbol": sym,
            "data": body,
        }))
    }

    async fn get_klines(&self, params: &Value, exchange: &str, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let sym = Self::normalize_symbol(symbol, exchange);
        let interval = params.get("interval").and_then(|v| v.as_str()).unwrap_or("1h");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(100);

        let url = match exchange {
            "binance" => format!("https://api.binance.com/api/v3/klines?symbol={}&interval={}&limit={}", sym, interval, limit),
            "okx" => {
                let bar = match interval {
                    "1m" => "1m", "5m" => "5m", "15m" => "15m", "1h" => "1H", "4h" => "4H", "1d" => "1D",
                    _ => interval,
                };
                format!("https://www.okx.com/api/v5/market/candles?instId={}&bar={}&limit={}", sym, bar, limit)
            }
            "bybit" => {
                let bi = match interval {
                    "1m" => "1", "5m" => "5", "15m" => "15", "1h" => "60", "4h" => "240", "1d" => "D",
                    _ => interval,
                };
                format!("https://api.bybit.com/v5/market/kline?category=spot&symbol={}&interval={}&limit={}", sym, bi, limit)
            }
            _ => return Err(Error::Tool(format!("Unsupported exchange: {}", exchange))),
        };

        let resp = client.get(&url).send().await
            .map_err(|e| Error::Tool(format!("{} klines request failed: {}", exchange, e)))?;
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        Ok(json!({
            "exchange": exchange,
            "symbol": sym,
            "interval": interval,
            "data": body,
        }))
    }

    async fn get_funding_rate(&self, params: &Value, exchange: &str, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let sym = Self::normalize_symbol(symbol, exchange);

        let url = match exchange {
            "binance" => format!("https://fapi.binance.com/fapi/v1/fundingRate?symbol={}&limit=10", sym),
            "okx" => format!("https://www.okx.com/api/v5/public/funding-rate?instId={}-SWAP", sym),
            "bybit" => format!("https://api.bybit.com/v5/market/funding/history?category=linear&symbol={}&limit=10", sym),
            _ => return Err(Error::Tool(format!("Unsupported exchange: {}", exchange))),
        };

        let resp = client.get(&url).send().await
            .map_err(|e| Error::Tool(format!("{} funding rate request failed: {}", exchange, e)))?;
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        Ok(json!({
            "exchange": exchange,
            "symbol": sym,
            "data": body,
        }))
    }

    // ─── Authenticated API ───

    async fn get_account(&self, ctx: &ToolContext, params: &Value, exchange: &str, client: &Client) -> Result<Value> {
        match exchange {
            "binance" => {
                let result = self.binance_signed_get(ctx, params, exchange, "/api/v3/account", "", client).await?;
                let balances = result.get("balances").and_then(|b| b.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter(|b| {
                                let free = b.get("free").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                                let locked = b.get("locked").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                                free > 0.0 || locked > 0.0
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Ok(json!({
                    "exchange": "binance",
                    "balances": balances,
                    "permissions": result.get("permissions"),
                }))
            }
            _ => {
                warn!(exchange = exchange, "get_account: using Binance-compatible API");
                self.binance_signed_get(ctx, params, exchange, "/api/v3/account", "", client).await
            }
        }
    }

    async fn place_order(&self, ctx: &ToolContext, params: &Value, exchange: &str, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let side = params.get("side").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        let order_type = params.get("order_type").and_then(|v| v.as_str()).unwrap_or("market").to_uppercase();
        let quantity = params.get("quantity").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let price = params.get("price").and_then(|v| v.as_f64());
        let sym = Self::normalize_symbol(symbol, exchange);

        // Build order query
        let mut query = format!("symbol={}&side={}&type={}&quantity={}", sym, side, order_type, quantity);
        if let Some(p) = price {
            query.push_str(&format!("&price={}&timeInForce=GTC", p));
        }
        if let Some(sp) = params.get("stop_price").and_then(|v| v.as_f64()) {
            query.push_str(&format!("&stopPrice={}", sp));
        }

        match exchange {
            "binance" => {
                self.binance_signed_post(ctx, params, exchange, "/api/v3/order", &query, client).await
            }
            _ => {
                self.binance_signed_post(ctx, params, exchange, "/api/v3/order", &query, client).await
            }
        }
    }

    async fn cancel_order(&self, ctx: &ToolContext, params: &Value, exchange: &str, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let order_id = params.get("order_id").and_then(|v| v.as_str()).unwrap_or("");
        let sym = Self::normalize_symbol(symbol, exchange);

        let query = format!("symbol={}&orderId={}", sym, order_id);
        self.binance_signed_delete(ctx, params, exchange, "/api/v3/order", &query, client).await
    }

    async fn get_order(&self, ctx: &ToolContext, params: &Value, exchange: &str, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let order_id = params.get("order_id").and_then(|v| v.as_str()).unwrap_or("");
        let sym = Self::normalize_symbol(symbol, exchange);

        let query = format!("symbol={}&orderId={}", sym, order_id);
        self.binance_signed_get(ctx, params, exchange, "/api/v3/order", &query, client).await
    }

    async fn list_orders(&self, ctx: &ToolContext, params: &Value, exchange: &str, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20);

        if !symbol.is_empty() {
            let sym = Self::normalize_symbol(symbol, exchange);
            let query = format!("symbol={}&limit={}", sym, limit);
            self.binance_signed_get(ctx, params, exchange, "/api/v3/openOrders", &query, client).await
        } else {
            self.binance_signed_get(ctx, params, exchange, "/api/v3/openOrders", "", client).await
        }
    }

    async fn get_positions(&self, ctx: &ToolContext, params: &Value, exchange: &str, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let query = if !symbol.is_empty() {
            let sym = Self::normalize_symbol(symbol, exchange);
            format!("symbol={}", sym)
        } else {
            String::new()
        };

        // Use futures API for positions
        let mut modified_params = params.clone();
        modified_params.as_object_mut().unwrap().insert("account_type".into(), json!("futures"));
        self.binance_signed_get(ctx, &modified_params, exchange, "/fapi/v2/positionRisk", &query, client).await
    }

    async fn transfer(&self, ctx: &ToolContext, params: &Value, exchange: &str, client: &Client) -> Result<Value> {
        let asset = params.get("asset").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        let amount = params.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let from = params.get("from_account").and_then(|v| v.as_str()).unwrap_or("spot");
        let to = params.get("to_account").and_then(|v| v.as_str()).unwrap_or("futures");

        // Binance transfer types: 1=SPOT→USDT-M, 2=USDT-M→SPOT, 3=SPOT→COIN-M, 4=COIN-M→SPOT
        let transfer_type = match (from, to) {
            ("spot", "futures") => 1,
            ("futures", "spot") => 2,
            _ => 1,
        };

        let query = format!("asset={}&amount={}&type={}", asset, amount, transfer_type);
        self.binance_signed_post(ctx, params, exchange, "/sapi/v1/futures/transfer", &query, client).await
    }

    fn info(&self, exchange: &str) -> Result<Value> {
        Ok(json!({
            "exchange": exchange,
            "supported_exchanges": ["binance", "okx", "bybit"],
            "public_actions": ["get_ticker", "get_depth", "get_klines", "get_funding_rate", "info"],
            "authenticated_actions": ["get_account", "place_order", "cancel_order", "get_order", "list_orders", "get_positions", "transfer"],
            "order_types": ["market", "limit", "stop_loss", "take_profit"],
            "account_types": ["spot", "futures", "margin"],
            "credential_config": {
                "binance": "providers.binance.api_key + providers.binance.api_base (=secret) or BINANCE_API_KEY + BINANCE_API_SECRET env",
                "okx": "providers.okx.api_key + providers.okx.api_base (=secret) + OKX_PASSPHRASE env",
                "bybit": "providers.bybit.api_key + providers.bybit.api_base (=secret) or BYBIT_API_KEY + BYBIT_API_SECRET env",
            },
            "security_notes": [
                "Trading operations (place_order, cancel_order, transfer) require user confirmation",
                "Always verify symbol, side, and quantity before placing orders",
                "Use limit orders with explicit price for safety",
                "Start with small amounts to verify API connectivity",
            ]
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = ExchangeApiTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "exchange_api");
        assert!(schema.description.contains("Binance"));
        assert!(schema.description.contains("OKX"));
        assert!(schema.description.contains("Bybit"));
    }

    #[test]
    fn test_validate_place_order() {
        let tool = ExchangeApiTool;
        // Valid market order
        assert!(tool.validate(&json!({
            "action": "place_order", "symbol": "BTCUSDT", "side": "buy", "quantity": 0.001
        })).is_ok());
        // Valid limit order
        assert!(tool.validate(&json!({
            "action": "place_order", "symbol": "BTCUSDT", "side": "sell",
            "quantity": 0.001, "order_type": "limit", "price": 50000.0
        })).is_ok());
        // Missing symbol
        assert!(tool.validate(&json!({
            "action": "place_order", "side": "buy", "quantity": 0.001
        })).is_err());
        // Missing side
        assert!(tool.validate(&json!({
            "action": "place_order", "symbol": "BTCUSDT", "quantity": 0.001
        })).is_err());
        // Missing quantity
        assert!(tool.validate(&json!({
            "action": "place_order", "symbol": "BTCUSDT", "side": "buy"
        })).is_err());
        // Limit order without price
        assert!(tool.validate(&json!({
            "action": "place_order", "symbol": "BTCUSDT", "side": "buy",
            "quantity": 0.001, "order_type": "limit"
        })).is_err());
    }

    #[test]
    fn test_validate_other_actions() {
        let tool = ExchangeApiTool;
        assert!(tool.validate(&json!({"action": "get_account"})).is_ok());
        assert!(tool.validate(&json!({"action": "get_ticker", "symbol": "BTCUSDT"})).is_ok());
        assert!(tool.validate(&json!({"action": "get_ticker"})).is_err()); // missing symbol
        assert!(tool.validate(&json!({"action": "get_depth", "symbol": "ETHUSDT"})).is_ok());
        assert!(tool.validate(&json!({"action": "info"})).is_ok());
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
        assert!(tool.validate(&json!({"action": "transfer", "asset": "USDT", "amount": 100})).is_ok());
        assert!(tool.validate(&json!({"action": "transfer"})).is_err()); // missing asset
    }

    #[test]
    fn test_normalize_symbol() {
        // Binance format
        assert_eq!(ExchangeApiTool::normalize_symbol("BTC-USDT", "binance"), "BTCUSDT");
        assert_eq!(ExchangeApiTool::normalize_symbol("btcusdt", "binance"), "BTCUSDT");
        // OKX format
        assert_eq!(ExchangeApiTool::normalize_symbol("BTCUSDT", "okx"), "BTC-USDT");
        assert_eq!(ExchangeApiTool::normalize_symbol("ETHUSDC", "okx"), "ETH-USDC");
        assert_eq!(ExchangeApiTool::normalize_symbol("BTC-USDT", "okx"), "BTC-USDT");
        // Bybit format
        assert_eq!(ExchangeApiTool::normalize_symbol("BTC-USDT", "bybit"), "BTCUSDT");
    }

    #[test]
    fn test_hmac_sha256() {
        // Test vector from RFC 4231
        let key = b"key";
        let msg = b"The quick brown fox jumps over the lazy dog";
        let result = ExchangeApiTool::hex_encode(&ExchangeApiTool::hmac_sha256(key, msg));
        assert_eq!(result, "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8");
    }

    #[test]
    fn test_sha256() {
        let result = ExchangeApiTool::hex_encode(&ExchangeApiTool::sha256(b""));
        assert_eq!(result, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");

        let result = ExchangeApiTool::hex_encode(&ExchangeApiTool::sha256(b"hello"));
        assert_eq!(result, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
    }

    #[test]
    fn test_info() {
        let tool = ExchangeApiTool;
        let result = tool.info("binance").unwrap();
        assert_eq!(result["exchange"], "binance");
        assert!(result["supported_exchanges"].as_array().unwrap().len() == 3);
    }
}
