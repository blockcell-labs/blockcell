use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// Cross-chain bridge tool for querying bridge routes, fees, and status.
///
/// Supports:
/// - **Stargate (LayerZero)**: Cross-chain token transfers via Stargate Finance
/// - **LI.FI**: Aggregated bridge routes across 20+ bridges
/// - **Socket (Bungee)**: Bridge aggregator with optimal route finding
///
/// Read-only by default. Actual bridging requires blockchain_tx for on-chain execution.
pub struct BridgeApiTool;

#[async_trait]
impl Tool for BridgeApiTool {
    fn schema(&self) -> ToolSchema {
        let mut props = serde_json::Map::new();
        let str_prop = |desc: &str| -> Value { json!({"type": "string", "description": desc}) };
        let num_prop = |desc: &str| -> Value { json!({"type": "number", "description": desc}) };

        props.insert("action".into(), str_prop("Action: quote|routes|status|supported_chains|supported_tokens|gas_estimate|info"));
        props.insert("provider".into(), str_prop("Bridge provider: 'lifi'|'socket'|'stargate' (default: lifi). lifi aggregates 20+ bridges."));
        props.insert("from_chain".into(), str_prop("Source chain: 'ethereum'|'polygon'|'arbitrum'|'optimism'|'bsc'|'avalanche'|'base' or chain ID"));
        props.insert("to_chain".into(), str_prop("Destination chain (same options as from_chain)"));
        props.insert("from_token".into(), str_prop("Source token address or symbol (e.g. 'USDC', '0xA0b86991...')"));
        props.insert("to_token".into(), str_prop("Destination token address or symbol"));
        props.insert("amount".into(), str_prop("Amount to bridge (human-readable, e.g. '100' for 100 USDC)"));
        props.insert("from_address".into(), str_prop("Sender wallet address"));
        props.insert("to_address".into(), str_prop("Recipient wallet address (if different from sender)"));
        props.insert("slippage".into(), num_prop("Max slippage percentage (default: 0.5)"));
        props.insert("tx_hash".into(), str_prop("(status) Transaction hash to check bridge status"));
        props.insert("bridge".into(), str_prop("(status) Bridge name for status check (e.g. 'stargate', 'hop', 'across')"));

        ToolSchema {
            name: "bridge_api",
            description: "Cross-chain bridge operations. Query bridge routes, compare fees, estimate gas, \
                and check transfer status across chains. Supports LI.FI (aggregator for 20+ bridges), \
                Stargate (LayerZero), and Socket (Bungee). Use 'quote' to get best route with fees, \
                'routes' for all available options, 'status' to track pending transfers. \
                Actual bridging execution requires blockchain_tx tool with the returned calldata.",
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid = ["quote", "routes", "status", "supported_chains", "supported_tokens", "gas_estimate", "info"];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        match action {
            "quote" | "routes" | "gas_estimate" => {
                if params.get("from_chain").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'from_chain' is required".into()));
                }
                if params.get("to_chain").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'to_chain' is required".into()));
                }
                if params.get("from_token").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'from_token' is required".into()));
                }
                if params.get("amount").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'amount' is required".into()));
                }
            }
            "status" => {
                if params.get("tx_hash").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'tx_hash' is required for status".into()));
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap_or("");
        let provider = params.get("provider").and_then(|v| v.as_str()).unwrap_or("lifi");
        let client = Client::new();

        match action {
            "quote" => self.quote(provider, &ctx, &params, &client).await,
            "routes" => self.routes(provider, &ctx, &params, &client).await,
            "status" => self.status(provider, &params, &client).await,
            "supported_chains" => self.supported_chains(provider, &client).await,
            "supported_tokens" => self.supported_tokens(provider, &params, &client).await,
            "gas_estimate" => self.gas_estimate(provider, &ctx, &params, &client).await,
            "info" => Ok(self.info()),
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

impl BridgeApiTool {
    /// Resolve chain name to chain ID.
    fn chain_id(chain: &str) -> u64 {
        match chain.to_lowercase().as_str() {
            "ethereum" | "eth" | "mainnet" => 1,
            "polygon" | "matic" => 137,
            "arbitrum" | "arb" => 42161,
            "optimism" | "op" => 10,
            "bsc" | "bnb" => 56,
            "avalanche" | "avax" => 43114,
            "base" => 8453,
            "fantom" | "ftm" => 250,
            "gnosis" | "xdai" => 100,
            "zksync" => 324,
            "linea" => 59144,
            "scroll" => 534352,
            _ => chain.parse::<u64>().unwrap_or(1),
        }
    }

    /// Resolve common token symbols to addresses per chain.
    fn resolve_token(symbol: &str, chain_id: u64) -> String {
        let s = symbol.to_uppercase();
        // Native token
        if s == "ETH" || s == "NATIVE" {
            return "0x0000000000000000000000000000000000000000".to_string();
        }
        // USDC addresses by chain
        if s == "USDC" {
            return match chain_id {
                1 => "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
                137 => "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359",
                42161 => "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
                10 => "0x0b2C639c533813f4Aa9D7837CAf62653d097Ff85",
                56 => "0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d",
                43114 => "0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E",
                8453 => "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
                _ => symbol,
            }.to_string();
        }
        // USDT
        if s == "USDT" {
            return match chain_id {
                1 => "0xdAC17F958D2ee523a2206206994597C13D831ec7",
                137 => "0xc2132D05D31c914a87C6611C10748AEb04B58e8F",
                42161 => "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9",
                10 => "0x94b008aA00579c1307B0EF2c499aD98a8ce58e58",
                56 => "0x55d398326f99059fF775485246999027B3197955",
                _ => symbol,
            }.to_string();
        }
        // If it looks like an address, return as-is
        if symbol.starts_with("0x") && symbol.len() == 42 {
            return symbol.to_string();
        }
        symbol.to_string()
    }

    /// Resolve LI.FI API key from config or env.
    fn resolve_lifi_key(ctx: &ToolContext) -> String {
        ctx.config.providers.get("lifi").map(|p| p.api_key.clone())
            .or_else(|| std::env::var("LIFI_API_KEY").ok())
            .unwrap_or_default()
    }

    // ─── Quote ───

    async fn quote(&self, provider: &str, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let from_chain_id = Self::chain_id(params["from_chain"].as_str().unwrap_or(""));
        let to_chain_id = Self::chain_id(params["to_chain"].as_str().unwrap_or(""));
        let from_token = Self::resolve_token(params["from_token"].as_str().unwrap_or(""), from_chain_id);
        let to_token_str = params.get("to_token").and_then(|v| v.as_str()).unwrap_or("");
        let to_token = if to_token_str.is_empty() { from_token.clone() } else { Self::resolve_token(to_token_str, to_chain_id) };
        let amount = params["amount"].as_str().unwrap_or("0");
        let from_address = params.get("from_address").and_then(|v| v.as_str()).unwrap_or("0x0000000000000000000000000000000000000001");
        let slippage = params.get("slippage").and_then(|v| v.as_f64()).unwrap_or(0.5);

        // Convert amount to raw (assume 6 decimals for stablecoins, 18 for ETH)
        let decimals = if from_token.to_uppercase().contains("USDC") || from_token.to_uppercase().contains("USDT") { 6 } else { 18 };
        let amount_f: f64 = amount.parse().unwrap_or(0.0);
        let amount_raw = format!("{:.0}", amount_f * 10f64.powi(decimals));

        match provider {
            "lifi" => {
                let api_key = Self::resolve_lifi_key(ctx);
                let url = format!(
                    "https://li.quest/v1/quote?fromChain={}&toChain={}&fromToken={}&toToken={}&fromAmount={}&fromAddress={}&slippage={}",
                    from_chain_id, to_chain_id, from_token, to_token, amount_raw, from_address, slippage / 100.0
                );
                debug!(url = %url, "LI.FI quote");
                let mut req = client.get(&url)
                    .header("User-Agent", "blockcell-agent");
                if !api_key.is_empty() {
                    req = req.header("x-lifi-api-key", &api_key);
                }
                let resp = req.send().await
                    .map_err(|e| Error::Tool(format!("LI.FI quote request failed: {}", e)))?;
                let body: Value = resp.json().await
                    .map_err(|e| Error::Tool(format!("Failed to parse LI.FI response: {}", e)))?;

                if body.get("message").is_some() && body.get("action").is_none() {
                    return Err(Error::Tool(format!("LI.FI error: {}", body["message"])));
                }

                Ok(json!({
                    "action": "quote",
                    "provider": "lifi",
                    "route": body.get("action"),
                    "estimate": body.get("estimate"),
                    "tool_used": body.get("tool"),
                    "transaction_request": body.get("transactionRequest"),
                    "from": { "chain_id": from_chain_id, "token": from_token, "amount": amount },
                    "to": { "chain_id": to_chain_id, "token": to_token },
                    "note": "Use blockchain_tx sign_and_send with the transactionRequest to execute"
                }))
            }
            "stargate" => {
                // Stargate V2 uses LayerZero endpoint IDs
                let lz_from = Self::to_lz_eid(from_chain_id);
                let lz_to = Self::to_lz_eid(to_chain_id);

                Ok(json!({
                    "action": "quote",
                    "provider": "stargate",
                    "from": { "chain_id": from_chain_id, "lz_eid": lz_from, "token": from_token, "amount": amount },
                    "to": { "chain_id": to_chain_id, "lz_eid": lz_to, "token": to_token },
                    "note": "Stargate V2 bridging: 1) Call quoteLayerZeroFee on Stargate router contract via blockchain_rpc eth_call, 2) Call sendTokens on router via blockchain_tx sign_and_send",
                    "stargate_routers": {
                        "ethereum": "0x8731d54E9D02c286767d56ac03e8037C07e01e98",
                        "polygon": "0x45A01E4e04F14f7A4a6702c74187c5F6222033cd",
                        "arbitrum": "0x53Bf833A5d6c4ddA888F69c22C88C9f356a41614",
                        "optimism": "0xB0D502E938ed5f4df2E681fE6E419ff29631d62b",
                        "bsc": "0x4a364f8c717cAAD9A442737Eb7b8A55cc6cf18D8",
                        "avalanche": "0x45A01E4e04F14f7A4a6702c74187c5F6222033cd",
                        "base": "0x45f1A95A4D3f3836523F5c83673c797f4d4d263B",
                    }
                }))
            }
            _ => {
                // Default: use LI.FI — inline to avoid async recursion
                let api_key = Self::resolve_lifi_key(ctx);
                let url = format!(
                    "https://li.quest/v1/quote?fromChain={}&toChain={}&fromToken={}&toToken={}&fromAmount={}&fromAddress={}&slippage={}",
                    from_chain_id, to_chain_id, from_token, to_token, amount_raw, from_address, slippage / 100.0
                );
                debug!(url = %url, "LI.FI quote (default)");
                let mut req = client.get(&url)
                    .header("User-Agent", "blockcell-agent");
                if !api_key.is_empty() {
                    req = req.header("x-lifi-api-key", &api_key);
                }
                let resp = req.send().await
                    .map_err(|e| Error::Tool(format!("LI.FI quote request failed: {}", e)))?;
                let body: Value = resp.json().await
                    .map_err(|e| Error::Tool(format!("Failed to parse LI.FI response: {}", e)))?;
                if body.get("message").is_some() && body.get("action").is_none() {
                    return Err(Error::Tool(format!("LI.FI error: {}", body["message"])));
                }
                Ok(json!({
                    "action": "quote",
                    "provider": "lifi",
                    "route": body.get("action"),
                    "estimate": body.get("estimate"),
                    "tool_used": body.get("tool"),
                    "transaction_request": body.get("transactionRequest"),
                    "from": { "chain_id": from_chain_id, "token": from_token, "amount": amount },
                    "to": { "chain_id": to_chain_id, "token": to_token },
                    "note": "Use blockchain_tx sign_and_send with the transactionRequest to execute"
                }))
            }
        }
    }

    // ─── Routes ───

    async fn routes(&self, _provider: &str, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let from_chain_id = Self::chain_id(params["from_chain"].as_str().unwrap_or(""));
        let to_chain_id = Self::chain_id(params["to_chain"].as_str().unwrap_or(""));
        let from_token = Self::resolve_token(params["from_token"].as_str().unwrap_or(""), from_chain_id);
        let to_token_str = params.get("to_token").and_then(|v| v.as_str()).unwrap_or("");
        let to_token = if to_token_str.is_empty() { from_token.clone() } else { Self::resolve_token(to_token_str, to_chain_id) };
        let amount = params["amount"].as_str().unwrap_or("0");
        let from_address = params.get("from_address").and_then(|v| v.as_str()).unwrap_or("0x0000000000000000000000000000000000000001");
        let slippage = params.get("slippage").and_then(|v| v.as_f64()).unwrap_or(0.5);

        let decimals = if from_token.to_uppercase().contains("USDC") || from_token.to_uppercase().contains("USDT") { 6 } else { 18 };
        let amount_f: f64 = amount.parse().unwrap_or(0.0);
        let amount_raw = format!("{:.0}", amount_f * 10f64.powi(decimals));

        let api_key = Self::resolve_lifi_key(ctx);
        let body = json!({
            "fromChainId": from_chain_id,
            "toChainId": to_chain_id,
            "fromTokenAddress": from_token,
            "toTokenAddress": to_token,
            "fromAmount": amount_raw,
            "fromAddress": from_address,
            "options": {
                "slippage": slippage / 100.0,
                "order": "RECOMMENDED"
            }
        });

        let url = "https://li.quest/v1/advanced/routes";
        debug!(url = %url, "LI.FI routes");
        let mut req = client.post(url)
            .header("Content-Type", "application/json")
            .header("User-Agent", "blockcell-agent")
            .json(&body);
        if !api_key.is_empty() {
            req = req.header("x-lifi-api-key", &api_key);
        }
        let resp = req.send().await
            .map_err(|e| Error::Tool(format!("LI.FI routes request failed: {}", e)))?;
        let result: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse LI.FI routes response: {}", e)))?;

        let routes = result.get("routes").and_then(|r| r.as_array()).cloned().unwrap_or_default();
        let mut simplified = Vec::new();
        for route in &routes {
            simplified.push(json!({
                "id": route.get("id"),
                "from_amount": route.pointer("/fromAmount"),
                "to_amount": route.pointer("/toAmount"),
                "to_amount_min": route.pointer("/toAmountMin"),
                "gas_cost_usd": route.pointer("/gasCostUSD"),
                "steps": route.get("steps").and_then(|s| s.as_array()).map(|steps| {
                    steps.iter().map(|step| json!({
                        "tool": step.get("tool"),
                        "type": step.get("type"),
                        "estimate": step.get("estimate"),
                    })).collect::<Vec<_>>()
                }),
                "tags": route.get("tags"),
            }));
        }

        Ok(json!({
            "action": "routes",
            "provider": "lifi",
            "route_count": simplified.len(),
            "routes": simplified,
            "from": { "chain_id": from_chain_id, "token": from_token, "amount": amount },
            "to": { "chain_id": to_chain_id, "token": to_token },
        }))
    }

    // ─── Status ───

    async fn status(&self, _provider: &str, params: &Value, client: &Client) -> Result<Value> {
        let tx_hash = params["tx_hash"].as_str().unwrap_or("");
        let bridge = params.get("bridge").and_then(|v| v.as_str()).unwrap_or("");
        let from_chain = params.get("from_chain").and_then(|v| v.as_str()).unwrap_or("");
        let to_chain = params.get("to_chain").and_then(|v| v.as_str()).unwrap_or("");

        let mut url = format!("https://li.quest/v1/status?txHash={}", tx_hash);
        if !bridge.is_empty() {
            url.push_str(&format!("&bridge={}", bridge));
        }
        if !from_chain.is_empty() {
            url.push_str(&format!("&fromChain={}", Self::chain_id(from_chain)));
        }
        if !to_chain.is_empty() {
            url.push_str(&format!("&toChain={}", Self::chain_id(to_chain)));
        }

        debug!(url = %url, "LI.FI status");
        let resp = client.get(&url)
            .header("User-Agent", "blockcell-agent")
            .send().await
            .map_err(|e| Error::Tool(format!("LI.FI status request failed: {}", e)))?;
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse LI.FI status response: {}", e)))?;

        Ok(json!({
            "action": "status",
            "provider": "lifi",
            "tx_hash": tx_hash,
            "status": body.get("status"),
            "substatus": body.get("substatus"),
            "sending": body.get("sending"),
            "receiving": body.get("receiving"),
            "tool": body.get("tool"),
            "bridge_exploration_url": body.get("bridgeExplorationUrl"),
        }))
    }

    // ─── Supported Chains ───

    async fn supported_chains(&self, _provider: &str, client: &Client) -> Result<Value> {
        let url = "https://li.quest/v1/chains";
        let resp = client.get(url)
            .header("User-Agent", "blockcell-agent")
            .send().await
            .map_err(|e| Error::Tool(format!("LI.FI chains request failed: {}", e)))?;
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        let chains = body.get("chains").and_then(|c| c.as_array()).cloned().unwrap_or_default();
        let simplified: Vec<Value> = chains.iter().map(|c| json!({
            "id": c.get("id"),
            "name": c.get("name"),
            "key": c.get("key"),
            "native_token": c.get("nativeToken"),
        })).collect();

        Ok(json!({
            "action": "supported_chains",
            "provider": "lifi",
            "count": simplified.len(),
            "chains": simplified
        }))
    }

    // ─── Supported Tokens ───

    async fn supported_tokens(&self, _provider: &str, params: &Value, client: &Client) -> Result<Value> {
        let chain = params.get("from_chain").and_then(|v| v.as_str()).unwrap_or("ethereum");
        let chain_id = Self::chain_id(chain);

        let url = format!("https://li.quest/v1/tokens?chains={}", chain_id);
        let resp = client.get(&url)
            .header("User-Agent", "blockcell-agent")
            .send().await
            .map_err(|e| Error::Tool(format!("LI.FI tokens request failed: {}", e)))?;
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        let tokens = body.get("tokens").and_then(|t| t.get(chain_id.to_string()))
            .and_then(|t| t.as_array()).cloned().unwrap_or_default();

        let limited: Vec<Value> = tokens.into_iter().take(50).map(|t| json!({
            "symbol": t.get("symbol"),
            "name": t.get("name"),
            "address": t.get("address"),
            "decimals": t.get("decimals"),
            "logo": t.get("logoURI"),
        })).collect();

        Ok(json!({
            "action": "supported_tokens",
            "provider": "lifi",
            "chain": chain,
            "chain_id": chain_id,
            "count": limited.len(),
            "tokens": limited
        }))
    }

    // ─── Gas Estimate ───

    async fn gas_estimate(&self, provider: &str, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        // For gas estimation, we use the quote endpoint and extract gas info
        let quote = self.quote(provider, ctx, params, client).await?;
        let gas_cost = quote.pointer("/estimate/gasCosts").cloned();
        let fee_costs = quote.pointer("/estimate/feeCosts").cloned();

        Ok(json!({
            "action": "gas_estimate",
            "gas_costs": gas_cost,
            "fee_costs": fee_costs,
            "from_amount": quote.pointer("/from/amount"),
            "estimated_output": quote.pointer("/estimate/toAmount"),
        }))
    }

    /// Convert chain ID to LayerZero V2 endpoint ID.
    fn to_lz_eid(chain_id: u64) -> u32 {
        match chain_id {
            1 => 30101,      // Ethereum
            137 => 30109,    // Polygon
            42161 => 30110,  // Arbitrum
            10 => 30111,     // Optimism
            56 => 30102,     // BSC
            43114 => 30106,  // Avalanche
            8453 => 30184,   // Base
            250 => 30112,    // Fantom
            _ => 0,
        }
    }

    fn info(&self) -> Value {
        json!({
            "tool": "bridge_api",
            "description": "Cross-chain bridge route finder and status tracker",
            "providers": {
                "lifi": "LI.FI aggregator — finds best route across 20+ bridges (Stargate, Hop, Across, Celer, etc.)",
                "stargate": "Stargate Finance (LayerZero) — direct bridge with deep liquidity for stablecoins",
                "socket": "Socket/Bungee — bridge aggregator with gas refuel"
            },
            "actions": {
                "quote": "Get best bridge quote with calldata for execution",
                "routes": "Get all available bridge routes sorted by recommendation",
                "status": "Check status of a pending bridge transfer",
                "supported_chains": "List all supported chains",
                "supported_tokens": "List supported tokens on a chain",
                "gas_estimate": "Estimate gas and fees for a bridge transfer",
                "info": "This help message"
            },
            "workflow": [
                "1. bridge_api quote from_chain='ethereum' to_chain='arbitrum' from_token='USDC' amount='100'",
                "2. Review the quote (fees, estimated output, time)",
                "3. blockchain_tx sign_and_send with the returned transactionRequest"
            ],
            "common_tokens": {
                "USDC": "Circle USD — most liquid bridge token",
                "USDT": "Tether USD",
                "ETH": "Native Ether (use 0x0000...0000 as address)",
                "WETH": "Wrapped Ether"
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = BridgeApiTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "bridge_api");
        assert!(schema.description.contains("cross-chain") || schema.description.contains("Cross-chain"));
    }

    #[test]
    fn test_validate() {
        let tool = BridgeApiTool;
        assert!(tool.validate(&json!({"action": "info"})).is_ok());
        assert!(tool.validate(&json!({"action": "supported_chains"})).is_ok());
        assert!(tool.validate(&json!({"action": "quote", "from_chain": "ethereum", "to_chain": "arbitrum", "from_token": "USDC", "amount": "100"})).is_ok());
        assert!(tool.validate(&json!({"action": "quote"})).is_err()); // missing params
        assert!(tool.validate(&json!({"action": "status", "tx_hash": "0xabc"})).is_ok());
        assert!(tool.validate(&json!({"action": "status"})).is_err()); // missing tx_hash
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }

    #[test]
    fn test_chain_id() {
        assert_eq!(BridgeApiTool::chain_id("ethereum"), 1);
        assert_eq!(BridgeApiTool::chain_id("polygon"), 137);
        assert_eq!(BridgeApiTool::chain_id("arbitrum"), 42161);
        assert_eq!(BridgeApiTool::chain_id("base"), 8453);
        assert_eq!(BridgeApiTool::chain_id("42161"), 42161);
    }

    #[test]
    fn test_resolve_token() {
        let usdc_eth = BridgeApiTool::resolve_token("USDC", 1);
        assert!(usdc_eth.starts_with("0x"));
        let usdc_arb = BridgeApiTool::resolve_token("USDC", 42161);
        assert!(usdc_arb.starts_with("0x"));
        assert_ne!(usdc_eth, usdc_arb); // Different addresses per chain
        let native = BridgeApiTool::resolve_token("ETH", 1);
        assert_eq!(native, "0x0000000000000000000000000000000000000000");
    }

    #[test]
    fn test_lz_eid() {
        assert_eq!(BridgeApiTool::to_lz_eid(1), 30101);
        assert_eq!(BridgeApiTool::to_lz_eid(42161), 30110);
        assert_eq!(BridgeApiTool::to_lz_eid(8453), 30184);
    }

    #[test]
    fn test_info() {
        let tool = BridgeApiTool;
        let info = tool.info();
        assert_eq!(info["tool"], "bridge_api");
        assert!(info["providers"].as_object().unwrap().len() >= 3);
    }
}
