use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::{Tool, ToolContext, ToolSchema};

/// Blockchain RPC tool — interact with EVM-compatible chains via JSON-RPC.
///
/// Supports:
/// - **eth_call**: Read-only smart contract calls with ABI encoding/decoding
/// - **eth_getLogs**: Query event logs with topic/address filters
/// - **eth_getBalance**: Get native token balance
/// - **eth_getTransactionReceipt**: Get transaction receipt with decoded logs
/// - **eth_blockNumber**: Get latest block number
/// - **eth_getCode**: Check if address is a contract
/// - **eth_getTransactionCount**: Get nonce
/// - **abi_encode**: Encode function call data from human-readable params
/// - **abi_decode**: Decode hex data using ABI type specs
///
/// Works with any EVM chain: Ethereum, Polygon, BSC, Arbitrum, Base, etc.
pub struct BlockchainRpcTool;

#[async_trait]
impl Tool for BlockchainRpcTool {
    fn schema(&self) -> ToolSchema {
        let str_prop = |desc: &str| -> Value { json!({"type": "string", "description": desc}) };
        let int_prop = |desc: &str| -> Value { json!({"type": "integer", "description": desc}) };
        let arr_prop = |desc: &str| -> Value { json!({"type": "array", "items": {"type": "string"}, "description": desc}) };

        let mut props = serde_json::Map::new();
        props.insert("action".into(), str_prop("Action: eth_call|eth_getLogs|eth_getBalance|eth_getTransactionReceipt|eth_blockNumber|eth_getCode|eth_getTransactionCount|abi_encode|abi_decode|chain_info"));
        props.insert("rpc_url".into(), str_prop("JSON-RPC endpoint URL (e.g. 'https://eth.llamarpc.com', 'https://rpc.ankr.com/eth'). Can also use chain shorthand: 'ethereum', 'polygon', 'bsc', 'arbitrum', 'base', 'optimism', 'avalanche'"));
        props.insert("address".into(), str_prop("Contract or wallet address (0x...)"));
        props.insert("to".into(), str_prop("(eth_call) Target contract address"));
        props.insert("data".into(), str_prop("(eth_call) Hex-encoded call data, or use function_sig + args for auto-encoding"));
        props.insert("function_sig".into(), str_prop("(eth_call/abi_encode) Function signature like 'balanceOf(address)' or 'transfer(address,uint256)'. Used to auto-encode call data."));
        props.insert("args".into(), arr_prop("(eth_call/abi_encode) Function arguments as strings. Addresses as 0x..., uint256 as decimal strings, bytes as 0x hex."));
        props.insert("return_types".into(), arr_prop("(eth_call/abi_decode) Expected return types for decoding: 'uint256', 'address', 'bool', 'string', 'bytes', 'uint8', 'int256', etc."));
        props.insert("block".into(), str_prop("Block number ('latest', '0x...', or decimal). Default: 'latest'"));
        props.insert("from_block".into(), str_prop("(eth_getLogs) Start block ('latest', hex, or decimal)"));
        props.insert("to_block".into(), str_prop("(eth_getLogs) End block ('latest', hex, or decimal)"));
        props.insert("topics".into(), json!({"type": "array", "items": {"type": ["string", "null", "array"]}, "description": "(eth_getLogs) Topic filters. topics[0] = event signature hash. Use null for wildcard."}));
        props.insert("tx_hash".into(), str_prop("(eth_getTransactionReceipt) Transaction hash"));
        props.insert("hex_data".into(), str_prop("(abi_decode) Hex data to decode (0x...)"));
        props.insert("event_sig".into(), str_prop("(eth_getLogs) Event signature like 'Transfer(address,address,uint256)' — auto-generates topic[0] hash"));
        props.insert("limit".into(), int_prop("(eth_getLogs) Max logs to return. Default: 100"));

        ToolSchema {
            name: "blockchain_rpc",
            description: "Interact with EVM-compatible blockchains via JSON-RPC. Query balances, call smart contracts, \
                read event logs, decode ABI data. Works with Ethereum, Polygon, BSC, Arbitrum, Base, Optimism, Avalanche, etc. \
                Use function_sig + args for human-readable contract calls without manual ABI encoding. \
                Supports automatic ABI encoding/decoding for common Solidity types (uint256, address, bool, string, bytes).",
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
            "eth_call", "eth_getLogs", "eth_getBalance",
            "eth_getTransactionReceipt", "eth_blockNumber",
            "eth_getCode", "eth_getTransactionCount",
            "abi_encode", "abi_decode", "chain_info",
        ];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        match action {
            "eth_call" => {
                let has_to = params.get("to").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
                let has_addr = params.get("address").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
                if !has_to && !has_addr {
                    return Err(Error::Tool("'to' or 'address' is required for eth_call".into()));
                }
                let has_data = params.get("data").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
                let has_fn_sig = params.get("function_sig").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
                if !has_data && !has_fn_sig {
                    return Err(Error::Tool("'data' or 'function_sig' is required for eth_call".into()));
                }
            }
            "eth_getBalance" | "eth_getCode" | "eth_getTransactionCount" => {
                if params.get("address").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'address' is required".into()));
                }
            }
            "eth_getTransactionReceipt" => {
                if params.get("tx_hash").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'tx_hash' is required".into()));
                }
            }
            "abi_encode" => {
                if params.get("function_sig").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'function_sig' is required for abi_encode".into()));
                }
            }
            "abi_decode" => {
                if params.get("hex_data").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'hex_data' is required for abi_decode".into()));
                }
                if params.get("return_types").and_then(|v| v.as_array()).map(|a| a.is_empty()).unwrap_or(true) {
                    return Err(Error::Tool("'return_types' is required for abi_decode".into()));
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap();
        match action {
            "eth_call" => action_eth_call(&ctx, &params).await,
            "eth_getLogs" => action_eth_get_logs(&ctx, &params).await,
            "eth_getBalance" => action_eth_get_balance(&ctx, &params).await,
            "eth_getTransactionReceipt" => action_eth_get_tx_receipt(&ctx, &params).await,
            "eth_blockNumber" => action_eth_block_number(&ctx, &params).await,
            "eth_getCode" => action_eth_get_code(&ctx, &params).await,
            "eth_getTransactionCount" => action_eth_get_tx_count(&ctx, &params).await,
            "abi_encode" => action_abi_encode(&params),
            "abi_decode" => action_abi_decode(&params),
            "chain_info" => Ok(chain_info()),
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

// ── RPC helpers ──

pub(crate) fn resolve_rpc_url(ctx: &ToolContext, params: &Value) -> Result<String> {
    let raw = params.get("rpc_url").and_then(|v| v.as_str()).unwrap_or("");

    if raw.starts_with("http://") || raw.starts_with("https://") {
        return Ok(raw.to_string());
    }

    // Try config: providers.ethereum.api_base, providers.{chain}.api_base
    if !raw.is_empty() {
        if let Some(provider) = ctx.config.providers.get(raw) {
            if let Some(ref base) = provider.api_base {
                return Ok(base.clone());
            }
        }
    }

    // Chain shorthand → free public RPC
    let url = match raw.to_lowercase().as_str() {
        "ethereum" | "eth" | "" => "https://eth.llamarpc.com",
        "polygon" | "matic" => "https://polygon-rpc.com",
        "bsc" | "bnb" => "https://bsc-dataseed.binance.org",
        "arbitrum" | "arb" => "https://arb1.arbitrum.io/rpc",
        "base" => "https://mainnet.base.org",
        "optimism" | "op" => "https://mainnet.optimism.io",
        "avalanche" | "avax" => "https://api.avax.network/ext/bc/C/rpc",
        "sepolia" => "https://rpc.sepolia.org",
        "goerli" => "https://rpc.ankr.com/eth_goerli",
        _ => {
            // Try env: ETHEREUM_RPC_URL, {CHAIN}_RPC_URL
            if let Ok(url) = std::env::var(format!("{}_RPC_URL", raw.to_uppercase())) {
                return Ok(url);
            }
            if let Ok(url) = std::env::var("ETHEREUM_RPC_URL") {
                return Ok(url);
            }
            return Err(Error::Tool(format!(
                "Unknown chain '{}'. Use a full URL or one of: ethereum, polygon, bsc, arbitrum, base, optimism, avalanche, sepolia",
                raw
            )));
        }
    };
    Ok(url.to_string())
}

pub(crate) async fn json_rpc_call(rpc_url: &str, method: &str, params: Value) -> Result<Value> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Tool(format!("HTTP client error: {}", e)))?;

    let body = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 1
    });

    debug!(method = method, rpc_url = rpc_url, "JSON-RPC call");

    let resp = client.post(rpc_url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("RPC request failed: {}", e)))?;

    if !resp.status().is_success() {
        return Err(Error::Tool(format!("RPC HTTP error: {}", resp.status())));
    }

    let result: Value = resp.json().await
        .map_err(|e| Error::Tool(format!("RPC response parse error: {}", e)))?;

    if let Some(error) = result.get("error") {
        return Err(Error::Tool(format!("RPC error: {}", error)));
    }

    Ok(result.get("result").cloned().unwrap_or(Value::Null))
}

fn normalize_block(block: &str) -> String {
    if block.is_empty() || block == "latest" {
        return "latest".to_string();
    }
    if block.starts_with("0x") {
        return block.to_string();
    }
    // Decimal → hex
    if let Ok(n) = block.parse::<u64>() {
        return format!("0x{:x}", n);
    }
    block.to_string()
}

// ── ABI encoding/decoding ──

/// Compute keccak256 of a byte slice. Uses a pure-Rust implementation.
pub(crate) fn keccak256(data: &[u8]) -> [u8; 32] {
    // Tiny keccak256 — we implement the sponge construction inline to avoid
    // pulling in a heavy crypto dependency just for function selector hashing.
    // This uses the standard NIST SHA-3 / Keccak parameters: rate=1088, capacity=512.
    tiny_keccak_256(data)
}

/// Minimal keccak-256 implementation (FIPS 202 / Keccak-f[1600]).
fn tiny_keccak_256(input: &[u8]) -> [u8; 32] {
    const RATE: usize = 136; // (1600 - 2*256) / 8
    let mut state = [0u64; 25];

    // Absorb
    let mut offset = 0;
    while offset + RATE <= input.len() {
        for i in 0..(RATE / 8) {
            let word = u64::from_le_bytes(input[offset + i * 8..offset + i * 8 + 8].try_into().unwrap());
            state[i] ^= word;
        }
        keccak_f1600(&mut state);
        offset += RATE;
    }

    // Pad last block (Keccak padding: 0x01 ... 0x80)
    let mut last_block = [0u8; RATE];
    let remaining = input.len() - offset;
    last_block[..remaining].copy_from_slice(&input[offset..]);
    last_block[remaining] = 0x01;
    last_block[RATE - 1] |= 0x80;

    for i in 0..(RATE / 8) {
        let word = u64::from_le_bytes(last_block[i * 8..i * 8 + 8].try_into().unwrap());
        state[i] ^= word;
    }
    keccak_f1600(&mut state);

    // Squeeze 32 bytes
    let mut output = [0u8; 32];
    for i in 0..4 {
        output[i * 8..(i + 1) * 8].copy_from_slice(&state[i].to_le_bytes());
    }
    output
}

fn keccak_f1600(state: &mut [u64; 25]) {
    const RC: [u64; 24] = [
        0x0000000000000001, 0x0000000000008082, 0x800000000000808a, 0x8000000080008000,
        0x000000000000808b, 0x0000000080000001, 0x8000000080008081, 0x8000000000008009,
        0x000000000000008a, 0x0000000000000088, 0x0000000080008009, 0x000000008000000a,
        0x000000008000808b, 0x800000000000008b, 0x8000000000008089, 0x8000000000008003,
        0x8000000000008002, 0x8000000000000080, 0x000000000000800a, 0x800000008000000a,
        0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
    ];
    const ROTATIONS: [u32; 25] = [
        0, 1, 62, 28, 27, 36, 44, 6, 55, 20,
        3, 10, 43, 25, 39, 41, 45, 15, 21, 8,
        18, 2, 61, 56, 14,
    ];
    const PI: [usize; 25] = [
        0, 10, 20, 5, 15, 16, 1, 11, 21, 6,
        7, 17, 2, 12, 22, 23, 8, 18, 3, 13,
        14, 24, 9, 19, 4,
    ];

    for round in 0..24 {
        // θ step
        let mut c = [0u64; 5];
        for x in 0..5 {
            c[x] = state[x] ^ state[x + 5] ^ state[x + 10] ^ state[x + 15] ^ state[x + 20];
        }
        let mut d = [0u64; 5];
        for x in 0..5 {
            d[x] = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1);
        }
        for i in 0..25 {
            state[i] ^= d[i % 5];
        }

        // ρ and π steps
        let mut b = [0u64; 25];
        for i in 0..25 {
            b[PI[i]] = state[i].rotate_left(ROTATIONS[i]);
        }

        // χ step
        for y in 0..5 {
            for x in 0..5 {
                state[y * 5 + x] = b[y * 5 + x] ^ (!b[y * 5 + (x + 1) % 5] & b[y * 5 + (x + 2) % 5]);
            }
        }

        // ι step
        state[0] ^= RC[round];
    }
}

/// Compute 4-byte function selector from signature like "balanceOf(address)".
fn function_selector(sig: &str) -> [u8; 4] {
    let hash = keccak256(sig.as_bytes());
    [hash[0], hash[1], hash[2], hash[3]]
}

/// Compute 32-byte event topic hash from signature like "Transfer(address,address,uint256)".
fn event_topic(sig: &str) -> String {
    let hash = keccak256(sig.as_bytes());
    format!("0x{}", hex_encode(&hash))
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

pub(crate) fn hex_decode(s: &str) -> Result<Vec<u8>> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() % 2 != 0 {
        return Err(Error::Tool("Hex string must have even length".into()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|e| Error::Tool(format!("Invalid hex at position {}: {}", i, e)))
        })
        .collect()
}

/// Parse a Solidity type string and extract the param types from a function signature.
/// e.g. "balanceOf(address)" → ["address"]
/// e.g. "transfer(address,uint256)" → ["address", "uint256"]
fn parse_param_types(sig: &str) -> Vec<String> {
    if let Some(start) = sig.find('(') {
        if let Some(end) = sig.rfind(')') {
            let inner = &sig[start + 1..end];
            if inner.is_empty() {
                return vec![];
            }
            return inner.split(',').map(|s| s.trim().to_string()).collect();
        }
    }
    vec![]
}

/// ABI-encode a single value according to its Solidity type.
/// Returns a 32-byte padded word.
fn abi_encode_value(typ: &str, value: &str) -> Result<Vec<u8>> {
    let typ = typ.trim();
    if typ == "address" {
        let bytes = hex_decode(value)?;
        let mut word = [0u8; 32];
        if bytes.len() == 20 {
            word[12..32].copy_from_slice(&bytes);
        } else if bytes.len() == 32 {
            word.copy_from_slice(&bytes);
        } else {
            return Err(Error::Tool(format!("Address must be 20 bytes, got {}", bytes.len())));
        }
        Ok(word.to_vec())
    } else if typ == "bool" {
        let mut word = [0u8; 32];
        if value == "true" || value == "1" {
            word[31] = 1;
        }
        Ok(word.to_vec())
    } else if typ.starts_with("uint") || typ.starts_with("int") {
        // Parse as decimal or hex
        let n = if value.starts_with("0x") {
            u128::from_str_radix(value.strip_prefix("0x").unwrap(), 16)
                .map_err(|e| Error::Tool(format!("Invalid uint hex '{}': {}", value, e)))?
        } else {
            value.parse::<u128>()
                .map_err(|e| Error::Tool(format!("Invalid uint '{}': {}", value, e)))?
        };
        let bytes = n.to_be_bytes();
        let mut word = [0u8; 32];
        word[16..32].copy_from_slice(&bytes);
        Ok(word.to_vec())
    } else if typ.starts_with("bytes") && typ.len() > 5 {
        // Fixed-size bytes (bytes1..bytes32)
        let size: usize = typ[5..].parse()
            .map_err(|_| Error::Tool(format!("Invalid type: {}", typ)))?;
        let bytes = hex_decode(value)?;
        let mut word = [0u8; 32];
        let len = bytes.len().min(size).min(32);
        word[..len].copy_from_slice(&bytes[..len]);
        Ok(word.to_vec())
    } else if typ == "bytes" || typ == "string" {
        // Dynamic types — encode as offset + length + data (simplified: inline for single param)
        let data = if typ == "string" {
            value.as_bytes().to_vec()
        } else {
            hex_decode(value)?
        };
        // For simplicity in multi-param encoding, we return the raw data
        // The caller handles offset encoding for dynamic types
        let mut encoded = Vec::new();
        // Length word
        let mut len_word = [0u8; 32];
        let len_bytes = (data.len() as u128).to_be_bytes();
        len_word[16..32].copy_from_slice(&len_bytes);
        encoded.extend_from_slice(&len_word);
        // Data padded to 32 bytes
        encoded.extend_from_slice(&data);
        let padding = (32 - data.len() % 32) % 32;
        encoded.extend(vec![0u8; padding]);
        Ok(encoded)
    } else {
        Err(Error::Tool(format!("Unsupported ABI type: {}", typ)))
    }
}

fn is_dynamic_type(typ: &str) -> bool {
    typ == "bytes" || typ == "string" || typ.ends_with("[]")
}

/// Full ABI encoding for a function call.
pub(crate) fn abi_encode_call(function_sig: &str, args: &[String]) -> Result<Vec<u8>> {
    let selector = function_selector(function_sig);
    let param_types = parse_param_types(function_sig);

    if param_types.len() != args.len() {
        return Err(Error::Tool(format!(
            "Function '{}' expects {} args, got {}",
            function_sig, param_types.len(), args.len()
        )));
    }

    // Separate static and dynamic parts
    let mut head = Vec::new();
    let mut tail = Vec::new();
    let head_size = param_types.len() * 32;

    for (i, typ) in param_types.iter().enumerate() {
        if is_dynamic_type(typ) {
            // Head contains offset to tail
            let offset = head_size + tail.len();
            let mut offset_word = [0u8; 32];
            let offset_bytes = (offset as u128).to_be_bytes();
            offset_word[16..32].copy_from_slice(&offset_bytes);
            head.extend_from_slice(&offset_word);
            // Tail contains the dynamic data
            let encoded = abi_encode_value(typ, &args[i])?;
            tail.extend(encoded);
        } else {
            let encoded = abi_encode_value(typ, &args[i])?;
            head.extend(encoded);
        }
    }

    let mut result = Vec::with_capacity(4 + head.len() + tail.len());
    result.extend_from_slice(&selector);
    result.extend(head);
    result.extend(tail);
    Ok(result)
}

/// Decode ABI return data according to type specs.
fn abi_decode_data(data: &[u8], types: &[String]) -> Result<Vec<Value>> {
    let mut results = Vec::new();
    let mut offset = 0;

    for typ in types {
        let typ = typ.trim();
        if offset + 32 > data.len() {
            warn!(expected_type = typ, offset = offset, data_len = data.len(), "ABI decode: insufficient data");
            results.push(json!({"type": typ, "error": "insufficient data"}));
            break;
        }

        let word = &data[offset..offset + 32];

        if typ == "address" {
            let addr = format!("0x{}", hex_encode(&word[12..32]));
            results.push(json!({"type": "address", "value": addr}));
            offset += 32;
        } else if typ == "bool" {
            let val = word[31] != 0;
            results.push(json!({"type": "bool", "value": val}));
            offset += 32;
        } else if typ.starts_with("uint") {
            // Read as big-endian u128 (covers up to uint128; for uint256 we use hex)
            let bits: usize = typ[4..].parse().unwrap_or(256);
            if bits <= 128 {
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&word[16..32]);
                let val = u128::from_be_bytes(buf);
                results.push(json!({"type": typ, "value": val.to_string()}));
            } else {
                // uint256 — return as hex and decimal string
                let hex_val = format!("0x{}", hex_encode(word));
                // Try to parse as u128 if high bytes are zero
                let high_zero = word[..16].iter().all(|&b| b == 0);
                if high_zero {
                    let mut buf = [0u8; 16];
                    buf.copy_from_slice(&word[16..32]);
                    let val = u128::from_be_bytes(buf);
                    results.push(json!({"type": typ, "value": val.to_string(), "hex": hex_val}));
                } else {
                    results.push(json!({"type": typ, "hex": hex_val}));
                }
            }
            offset += 32;
        } else if typ.starts_with("int") {
            let hex_val = format!("0x{}", hex_encode(word));
            let high_zero = word[..16].iter().all(|&b| b == 0);
            if high_zero {
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&word[16..32]);
                let val = u128::from_be_bytes(buf);
                results.push(json!({"type": typ, "value": val.to_string(), "hex": hex_val}));
            } else {
                results.push(json!({"type": typ, "hex": hex_val}));
            }
            offset += 32;
        } else if typ.starts_with("bytes") && typ.len() > 5 {
            let size: usize = typ[5..].parse().unwrap_or(32);
            let val = format!("0x{}", hex_encode(&word[..size.min(32)]));
            results.push(json!({"type": typ, "value": val}));
            offset += 32;
        } else if typ == "bytes" || typ == "string" {
            // Dynamic type — word contains offset
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&word[16..32]);
            let data_offset = u128::from_be_bytes(buf) as usize;
            if data_offset + 32 <= data.len() {
                let mut len_buf = [0u8; 16];
                len_buf.copy_from_slice(&data[data_offset + 16..data_offset + 32]);
                let length = u128::from_be_bytes(len_buf) as usize;
                let start = data_offset + 32;
                let end = (start + length).min(data.len());
                if typ == "string" {
                    let s = String::from_utf8_lossy(&data[start..end]).to_string();
                    results.push(json!({"type": "string", "value": s}));
                } else {
                    let hex_val = format!("0x{}", hex_encode(&data[start..end]));
                    results.push(json!({"type": "bytes", "value": hex_val, "length": length}));
                }
            } else {
                results.push(json!({"type": typ, "error": "invalid offset"}));
            }
            offset += 32;
        } else {
            // Unknown type — return raw hex
            let hex_val = format!("0x{}", hex_encode(word));
            results.push(json!({"type": typ, "raw": hex_val}));
            offset += 32;
        }
    }

    Ok(results)
}

// ── Actions ──

async fn action_eth_call(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let rpc_url = resolve_rpc_url(ctx, params)?;
    let to = params.get("to").and_then(|v| v.as_str())
        .or_else(|| params.get("address").and_then(|v| v.as_str()))
        .unwrap_or("");
    let block = params.get("block").and_then(|v| v.as_str()).unwrap_or("latest");

    // Build call data
    let call_data = if let Some(data) = params.get("data").and_then(|v| v.as_str()) {
        if !data.is_empty() {
            data.to_string()
        } else {
            build_call_data(params)?
        }
    } else {
        build_call_data(params)?
    };

    let result = json_rpc_call(&rpc_url, "eth_call", json!([
        {"to": to, "data": call_data},
        normalize_block(block)
    ])).await?;

    let result_hex = result.as_str().unwrap_or("0x");

    // Try to decode if return_types specified
    if let Some(return_types) = params.get("return_types").and_then(|v| v.as_array()) {
        let types: Vec<String> = return_types.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        if !types.is_empty() {
            let data = hex_decode(result_hex)?;
            let decoded = abi_decode_data(&data, &types)?;
            return Ok(json!({
                "raw": result_hex,
                "decoded": decoded,
                "to": to,
            }));
        }
    }

    Ok(json!({
        "raw": result_hex,
        "to": to,
        "note": "Set 'return_types' to auto-decode the result (e.g. ['uint256'] or ['address', 'uint256'])"
    }))
}

fn build_call_data(params: &Value) -> Result<String> {
    let function_sig = params.get("function_sig").and_then(|v| v.as_str()).unwrap_or("");
    if function_sig.is_empty() {
        return Err(Error::Tool("'data' or 'function_sig' is required".into()));
    }
    let args: Vec<String> = params.get("args")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    let encoded = abi_encode_call(function_sig, &args)?;
    Ok(format!("0x{}", hex_encode(&encoded)))
}

async fn action_eth_get_logs(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let rpc_url = resolve_rpc_url(ctx, params)?;
    let address = params.get("address").and_then(|v| v.as_str());
    let from_block = params.get("from_block").and_then(|v| v.as_str()).unwrap_or("latest");
    let to_block = params.get("to_block").and_then(|v| v.as_str()).unwrap_or("latest");
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;

    let mut filter = json!({
        "fromBlock": normalize_block(from_block),
        "toBlock": normalize_block(to_block),
    });

    if let Some(addr) = address {
        if !addr.is_empty() {
            filter["address"] = json!(addr);
        }
    }

    // Handle topics
    if let Some(topics) = params.get("topics").and_then(|v| v.as_array()) {
        filter["topics"] = json!(topics);
    } else if let Some(event_sig) = params.get("event_sig").and_then(|v| v.as_str()) {
        if !event_sig.is_empty() {
            let topic0 = event_topic(event_sig);
            filter["topics"] = json!([topic0]);
        }
    }

    let result = json_rpc_call(&rpc_url, "eth_getLogs", json!([filter])).await?;

    let empty = vec![];
    let logs = result.as_array().unwrap_or(&empty);
    let truncated = logs.len() > limit;
    let logs: Vec<&Value> = logs.iter().take(limit).collect();

    // Enrich logs with decoded topic0 if possible
    let enriched: Vec<Value> = logs.iter().map(|log| {
        let mut l = (*log).clone();
        // Add block number as decimal
        if let Some(bn) = l.get("blockNumber").and_then(|v| v.as_str()) {
            if let Some(stripped) = bn.strip_prefix("0x") {
                if let Ok(n) = u64::from_str_radix(stripped, 16) {
                    l["blockNumber_decimal"] = json!(n);
                }
            }
        }
        l
    }).collect();

    Ok(json!({
        "logs": enriched,
        "count": enriched.len(),
        "truncated": truncated,
        "filter": filter,
    }))
}

async fn action_eth_get_balance(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let rpc_url = resolve_rpc_url(ctx, params)?;
    let address = params["address"].as_str().unwrap();
    let block = params.get("block").and_then(|v| v.as_str()).unwrap_or("latest");

    let result = json_rpc_call(&rpc_url, "eth_getBalance", json!([address, normalize_block(block)])).await?;

    let hex_val = result.as_str().unwrap_or("0x0");
    let stripped = hex_val.strip_prefix("0x").unwrap_or(hex_val);
    let wei = u128::from_str_radix(stripped, 16).unwrap_or(0);
    let eth = wei as f64 / 1e18;

    Ok(json!({
        "address": address,
        "balance_wei": wei.to_string(),
        "balance_eth": format!("{:.6}", eth),
        "raw": hex_val,
    }))
}

async fn action_eth_get_tx_receipt(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let rpc_url = resolve_rpc_url(ctx, params)?;
    let tx_hash = params["tx_hash"].as_str().unwrap();

    let result = json_rpc_call(&rpc_url, "eth_getTransactionReceipt", json!([tx_hash])).await?;

    if result.is_null() {
        return Ok(json!({"tx_hash": tx_hash, "status": "not_found"}));
    }

    // Decode status
    let status_hex = result.get("status").and_then(|v| v.as_str()).unwrap_or("0x0");
    let success = status_hex == "0x1";

    // Decode gas used
    let gas_hex = result.get("gasUsed").and_then(|v| v.as_str()).unwrap_or("0x0");
    let gas_stripped = gas_hex.strip_prefix("0x").unwrap_or(gas_hex);
    let gas_used = u64::from_str_radix(gas_stripped, 16).unwrap_or(0);

    // Block number
    let block_hex = result.get("blockNumber").and_then(|v| v.as_str()).unwrap_or("0x0");
    let block_stripped = block_hex.strip_prefix("0x").unwrap_or(block_hex);
    let block_number = u64::from_str_radix(block_stripped, 16).unwrap_or(0);

    let logs_count = result.get("logs").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);

    Ok(json!({
        "tx_hash": tx_hash,
        "success": success,
        "status": if success { "success" } else { "reverted" },
        "gas_used": gas_used,
        "block_number": block_number,
        "logs_count": logs_count,
        "from": result.get("from"),
        "to": result.get("to"),
        "contract_address": result.get("contractAddress"),
        "logs": result.get("logs"),
    }))
}

async fn action_eth_block_number(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let rpc_url = resolve_rpc_url(ctx, params)?;
    let result = json_rpc_call(&rpc_url, "eth_blockNumber", json!([])).await?;

    let hex_val = result.as_str().unwrap_or("0x0");
    let stripped = hex_val.strip_prefix("0x").unwrap_or(hex_val);
    let block_number = u64::from_str_radix(stripped, 16).unwrap_or(0);

    Ok(json!({
        "block_number": block_number,
        "hex": hex_val,
    }))
}

async fn action_eth_get_code(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let rpc_url = resolve_rpc_url(ctx, params)?;
    let address = params["address"].as_str().unwrap();
    let block = params.get("block").and_then(|v| v.as_str()).unwrap_or("latest");

    let result = json_rpc_call(&rpc_url, "eth_getCode", json!([address, normalize_block(block)])).await?;

    let code = result.as_str().unwrap_or("0x");
    let is_contract = code.len() > 2; // "0x" means no code = EOA

    Ok(json!({
        "address": address,
        "is_contract": is_contract,
        "code_size": if is_contract { (code.len() - 2) / 2 } else { 0 },
        "code_prefix": if code.len() > 66 { &code[..66] } else { code },
    }))
}

async fn action_eth_get_tx_count(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let rpc_url = resolve_rpc_url(ctx, params)?;
    let address = params["address"].as_str().unwrap();
    let block = params.get("block").and_then(|v| v.as_str()).unwrap_or("latest");

    let result = json_rpc_call(&rpc_url, "eth_getTransactionCount", json!([address, normalize_block(block)])).await?;

    let hex_val = result.as_str().unwrap_or("0x0");
    let stripped = hex_val.strip_prefix("0x").unwrap_or(hex_val);
    let nonce = u64::from_str_radix(stripped, 16).unwrap_or(0);

    Ok(json!({
        "address": address,
        "nonce": nonce,
        "hex": hex_val,
    }))
}

fn action_abi_encode(params: &Value) -> Result<Value> {
    let function_sig = params["function_sig"].as_str().unwrap();
    let args: Vec<String> = params.get("args")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    let encoded = abi_encode_call(function_sig, &args)?;
    let selector = &encoded[..4];
    let calldata = &encoded[4..];

    Ok(json!({
        "function_sig": function_sig,
        "selector": format!("0x{}", hex_encode(selector)),
        "calldata": format!("0x{}", hex_encode(&encoded)),
        "args_encoded": format!("0x{}", hex_encode(calldata)),
        "args": args,
    }))
}

fn action_abi_decode(params: &Value) -> Result<Value> {
    let hex_data = params["hex_data"].as_str().unwrap();
    let return_types: Vec<String> = params["return_types"].as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    let data = hex_decode(hex_data)?;

    // If data starts with a 4-byte selector, skip it
    let decode_data = if data.len() > 4 && data.len() % 32 == 4 {
        &data[4..]
    } else {
        &data
    };

    let decoded = abi_decode_data(decode_data, &return_types)?;

    Ok(json!({
        "hex_data": hex_data,
        "decoded": decoded,
        "types": return_types,
    }))
}

fn chain_info() -> Value {
    json!({
        "supported_chains": [
            {"name": "Ethereum", "shorthand": "ethereum", "chain_id": 1, "default_rpc": "https://eth.llamarpc.com"},
            {"name": "Polygon", "shorthand": "polygon", "chain_id": 137, "default_rpc": "https://polygon-rpc.com"},
            {"name": "BSC", "shorthand": "bsc", "chain_id": 56, "default_rpc": "https://bsc-dataseed.binance.org"},
            {"name": "Arbitrum", "shorthand": "arbitrum", "chain_id": 42161, "default_rpc": "https://arb1.arbitrum.io/rpc"},
            {"name": "Base", "shorthand": "base", "chain_id": 8453, "default_rpc": "https://mainnet.base.org"},
            {"name": "Optimism", "shorthand": "optimism", "chain_id": 10, "default_rpc": "https://mainnet.optimism.io"},
            {"name": "Avalanche", "shorthand": "avalanche", "chain_id": 43114, "default_rpc": "https://api.avax.network/ext/bc/C/rpc"},
            {"name": "Sepolia (testnet)", "shorthand": "sepolia", "chain_id": 11155111, "default_rpc": "https://rpc.sepolia.org"},
        ],
        "common_abis": {
            "ERC20": {
                "balanceOf": "balanceOf(address) → uint256",
                "totalSupply": "totalSupply() → uint256",
                "decimals": "decimals() → uint8",
                "symbol": "symbol() → string",
                "name": "name() → string",
                "allowance": "allowance(address,address) → uint256",
                "Transfer_event": "Transfer(address,address,uint256)",
                "Approval_event": "Approval(address,address,uint256)",
            },
            "ERC721": {
                "ownerOf": "ownerOf(uint256) → address",
                "balanceOf": "balanceOf(address) → uint256",
                "tokenURI": "tokenURI(uint256) → string",
                "Transfer_event": "Transfer(address,address,uint256)",
            },
            "Uniswap_V2_Pair": {
                "getReserves": "getReserves() → uint112,uint112,uint32",
                "token0": "token0() → address",
                "token1": "token1() → address",
                "Swap_event": "Swap(address,uint256,uint256,uint256,uint256,address)",
            },
        },
        "note": "Use rpc_url param with chain shorthand (e.g. 'ethereum') or full URL. Set custom RPC via config providers.ethereum.api_base or ETHEREUM_RPC_URL env var."
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = BlockchainRpcTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "blockchain_rpc");
    }

    #[test]
    fn test_validate_eth_call() {
        let tool = BlockchainRpcTool;
        let params = json!({"action": "eth_call", "to": "0x1234", "function_sig": "balanceOf(address)", "args": ["0xabc"]});
        assert!(tool.validate(&params).is_ok());
    }

    #[test]
    fn test_validate_eth_call_missing_to() {
        let tool = BlockchainRpcTool;
        let params = json!({"action": "eth_call", "function_sig": "balanceOf(address)"});
        assert!(tool.validate(&params).is_err());
    }

    #[test]
    fn test_validate_eth_get_balance() {
        let tool = BlockchainRpcTool;
        let params = json!({"action": "eth_getBalance", "address": "0x1234"});
        assert!(tool.validate(&params).is_ok());
    }

    #[test]
    fn test_validate_abi_decode() {
        let tool = BlockchainRpcTool;
        let params = json!({"action": "abi_decode", "hex_data": "0x1234", "return_types": ["uint256"]});
        assert!(tool.validate(&params).is_ok());

        let params2 = json!({"action": "abi_decode", "hex_data": "0x1234"});
        assert!(tool.validate(&params2).is_err());
    }

    #[test]
    fn test_function_selector() {
        // Known: keccak256("balanceOf(address)") starts with 0x70a08231
        let sel = function_selector("balanceOf(address)");
        assert_eq!(format!("0x{}", hex_encode(&sel)), "0x70a08231");
    }

    #[test]
    fn test_function_selector_transfer() {
        // Known: keccak256("transfer(address,uint256)") starts with 0xa9059cbb
        let sel = function_selector("transfer(address,uint256)");
        assert_eq!(format!("0x{}", hex_encode(&sel)), "0xa9059cbb");
    }

    #[test]
    fn test_event_topic_transfer() {
        // Known: keccak256("Transfer(address,address,uint256)")
        let topic = event_topic("Transfer(address,address,uint256)");
        assert_eq!(topic, "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");
    }

    #[test]
    fn test_parse_param_types() {
        assert_eq!(parse_param_types("balanceOf(address)"), vec!["address"]);
        assert_eq!(parse_param_types("transfer(address,uint256)"), vec!["address", "uint256"]);
        assert_eq!(parse_param_types("totalSupply()"), Vec::<String>::new());
        assert_eq!(parse_param_types("foo(address, uint256, bool)"), vec!["address", "uint256", "bool"]);
    }

    #[test]
    fn test_abi_encode_address() {
        let encoded = abi_encode_value("address", "0xdead000000000000000000000000000000000000").unwrap();
        assert_eq!(encoded.len(), 32);
        assert_eq!(&encoded[12..16], &[0xde, 0xad, 0x00, 0x00]);
    }

    #[test]
    fn test_abi_encode_uint256() {
        let encoded = abi_encode_value("uint256", "1000").unwrap();
        assert_eq!(encoded.len(), 32);
        // 1000 = 0x3E8
        assert_eq!(encoded[31], 0xe8);
        assert_eq!(encoded[30], 0x03);
    }

    #[test]
    fn test_abi_encode_call_balance_of() {
        let encoded = abi_encode_call(
            "balanceOf(address)",
            &["0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045".to_string()],
        ).unwrap();
        // Should start with 0x70a08231 selector
        assert_eq!(&encoded[..4], &[0x70, 0xa0, 0x82, 0x31]);
        assert_eq!(encoded.len(), 4 + 32); // selector + 1 word
    }

    #[test]
    fn test_abi_decode_uint256() {
        let mut data = vec![0u8; 32];
        data[31] = 42;
        let decoded = abi_decode_data(&data, &["uint256".to_string()]).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0]["value"], "42");
    }

    #[test]
    fn test_abi_decode_address() {
        let mut data = vec![0u8; 32];
        data[12] = 0xde;
        data[13] = 0xad;
        let decoded = abi_decode_data(&data, &["address".to_string()]).unwrap();
        assert_eq!(decoded.len(), 1);
        assert!(decoded[0]["value"].as_str().unwrap().starts_with("0xdead"));
    }

    #[test]
    fn test_hex_encode_decode() {
        let bytes = vec![0xde, 0xad, 0xbe, 0xef];
        let hex = hex_encode(&bytes);
        assert_eq!(hex, "deadbeef");
        let decoded = hex_decode("0xdeadbeef").unwrap();
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn test_normalize_block() {
        assert_eq!(normalize_block("latest"), "latest");
        assert_eq!(normalize_block(""), "latest");
        assert_eq!(normalize_block("0x1234"), "0x1234");
        assert_eq!(normalize_block("1000"), "0x3e8");
    }

    #[test]
    fn test_chain_info() {
        let info = chain_info();
        assert!(info.get("supported_chains").is_some());
        assert!(info.get("common_abis").is_some());
    }

    #[test]
    fn test_keccak256_empty() {
        // Known: keccak256("") = c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470
        let hash = keccak256(b"");
        assert_eq!(hex_encode(&hash), "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470");
    }
}
