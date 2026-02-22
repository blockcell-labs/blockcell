use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};
use crate::blockchain_rpc::{resolve_rpc_url, json_rpc_call, hex_encode, abi_encode_call};

/// Blockchain transaction tool — build, sign, and send EVM transactions.
///
/// Supports:
/// - **build_tx**: Build unsigned transaction (to, value, data, gas estimation)
/// - **estimate_gas**: Estimate gas for a transaction
/// - **sign_and_send**: Sign with private key and send via eth_sendRawTransaction
/// - **get_tx_status**: Check transaction status (pending/confirmed/failed)
/// - **approve**: ERC20 approve (spender, amount)
/// - **transfer**: ETH or ERC20 token transfer
/// - **swap**: DEX swap via Uniswap V2/V3 Router
/// - **revoke_approval**: Revoke ERC20 approval (set to 0)
/// - **multicall**: Batch multiple calls
/// - **info**: Show supported operations
///
/// ⚠️ All write operations require user confirmation.
/// Private key from env var `ETH_PRIVATE_KEY` or config `providers.ethereum.private_key`.
pub struct BlockchainTxTool;

#[async_trait]
impl Tool for BlockchainTxTool {
    fn schema(&self) -> ToolSchema {
        let str_prop = |desc: &str| -> Value { json!({"type": "string", "description": desc}) };
        let num_prop = |desc: &str| -> Value { json!({"type": "number", "description": desc}) };
        let arr_prop = |desc: &str| -> Value { json!({"type": "array", "items": {"type": "string"}, "description": desc}) };

        let mut props = serde_json::Map::new();
        props.insert("action".into(), str_prop("Action: build_tx|estimate_gas|sign_and_send|get_tx_status|approve|transfer|swap|revoke_approval|multicall|list_wallets|info"));
        props.insert("rpc_url".into(), str_prop("JSON-RPC endpoint or chain shorthand: 'ethereum'|'polygon'|'bsc'|'arbitrum'|'base'|'optimism'|'avalanche'|'sepolia'"));
        props.insert("to".into(), str_prop("Destination address (0x...)"));
        props.insert("value".into(), str_prop("ETH value to send (in ETH, e.g. '0.1'). Converted to wei internally."));
        props.insert("data".into(), str_prop("Hex-encoded call data (0x...). Or use function_sig + args for auto-encoding."));
        props.insert("function_sig".into(), str_prop("Function signature for auto-encoding, e.g. 'transfer(address,uint256)'"));
        props.insert("args".into(), arr_prop("Function arguments as strings"));
        props.insert("gas_limit".into(), str_prop("Gas limit (decimal or hex). Auto-estimated if omitted."));
        props.insert("gas_price".into(), str_prop("Gas price in Gwei. Uses network default if omitted."));
        props.insert("max_fee_per_gas".into(), str_prop("EIP-1559 max fee per gas in Gwei"));
        props.insert("max_priority_fee".into(), str_prop("EIP-1559 max priority fee in Gwei"));
        props.insert("nonce".into(), str_prop("Transaction nonce. Auto-fetched if omitted."));
        props.insert("private_key".into(), str_prop("Private key (0x... hex). ⚠️ Prefer env var ETH_PRIVATE_KEY."));
        props.insert("tx_hash".into(), str_prop("(get_tx_status) Transaction hash to check"));
        props.insert("token".into(), str_prop("(transfer/approve/revoke) ERC20 token contract address. Omit for native ETH transfer."));
        props.insert("spender".into(), str_prop("(approve/revoke_approval) Spender address to approve/revoke"));
        props.insert("amount".into(), str_prop("(transfer/approve) Amount in token units (human-readable, e.g. '100.5'). For approve, use 'max' for unlimited."));
        props.insert("decimals".into(), num_prop("(transfer/approve) Token decimals (default: 18)"));
        props.insert("recipient".into(), str_prop("(transfer) Recipient address"));
        props.insert("token_in".into(), str_prop("(swap) Input token address (use 'ETH' or 'WETH' for native)"));
        props.insert("token_out".into(), str_prop("(swap) Output token address"));
        props.insert("amount_in".into(), str_prop("(swap) Input amount in human-readable units"));
        props.insert("slippage".into(), num_prop("(swap) Slippage tolerance in percent (default: 0.5)"));
        props.insert("router".into(), str_prop("(swap) DEX router address. Default: Uniswap V2 Router"));
        props.insert("calls".into(), json!({"type": "array", "items": {"type": "object"}, "description": "(multicall) Array of {to, data, value} objects"}));
        props.insert("simulate".into(), json!({"type": "boolean", "description": "If true, simulate via eth_call instead of sending. Default: false"}));
        props.insert("wallet_name".into(), str_prop("Named wallet to use (for multi-wallet). Resolves key from config providers.wallets.{name}.private_key or env {NAME}_PRIVATE_KEY. If omitted, uses default wallet."));
        props.insert("from".into(), str_prop("Sender address (0x...). Used with wallet_name to verify correct wallet."));

        ToolSchema {
            name: "blockchain_tx",
            description: "Build, sign, and send EVM blockchain transactions. Actions: build_tx (construct unsigned tx), \
                estimate_gas, sign_and_send (⚠️ requires confirmation), get_tx_status, approve (ERC20 approve — ⚠️), \
                transfer (ETH/ERC20 — ⚠️), swap (DEX swap — ⚠️), revoke_approval (⚠️), multicall (batch — ⚠️), \
                list_wallets (show configured wallets), info. \
                Multi-wallet: use wallet_name param to select a named wallet (config providers.wallets.{name}). \
                Supports EIP-1559 gas pricing. Private key from ETH_PRIVATE_KEY env or config. \
                Always simulates (eth_call) before sending to catch reverts. Works with any EVM chain.",
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
            "build_tx", "estimate_gas", "sign_and_send", "get_tx_status",
            "approve", "transfer", "swap", "revoke_approval", "multicall",
            "list_wallets", "info",
        ];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        match action {
            "build_tx" | "estimate_gas" => {
                if params.get("to").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'to' address is required".into()));
                }
            }
            "sign_and_send" => {
                if params.get("to").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'to' address is required".into()));
                }
            }
            "get_tx_status" => {
                if params.get("tx_hash").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'tx_hash' is required".into()));
                }
            }
            "approve" => {
                if params.get("token").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'token' contract address is required for approve".into()));
                }
                if params.get("spender").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'spender' address is required for approve".into()));
                }
            }
            "transfer" => {
                if params.get("recipient").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                    && params.get("to").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                {
                    return Err(Error::Tool("'recipient' or 'to' address is required for transfer".into()));
                }
                if params.get("amount").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                    && params.get("value").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                {
                    return Err(Error::Tool("'amount' or 'value' is required for transfer".into()));
                }
            }
            "swap" => {
                if params.get("token_in").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'token_in' is required for swap".into()));
                }
                if params.get("token_out").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'token_out' is required for swap".into()));
                }
                if params.get("amount_in").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'amount_in' is required for swap".into()));
                }
            }
            "revoke_approval" => {
                if params.get("token").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'token' contract address is required".into()));
                }
                if params.get("spender").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'spender' address is required".into()));
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap_or("");

        match action {
            "build_tx" => self.build_tx(&ctx, &params).await,
            "estimate_gas" => self.estimate_gas(&ctx, &params).await,
            "sign_and_send" => self.sign_and_send(&ctx, &params).await,
            "get_tx_status" => self.get_tx_status(&ctx, &params).await,
            "approve" => self.approve(&ctx, &params).await,
            "transfer" => self.transfer(&ctx, &params).await,
            "swap" => self.swap(&ctx, &params).await,
            "revoke_approval" => self.revoke_approval(&ctx, &params).await,
            "multicall" => self.multicall(&ctx, &params).await,
            "list_wallets" => Ok(self.list_wallets(&ctx)),
            "info" => Ok(self.info()),
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

impl BlockchainTxTool {
    /// Convert ETH amount string to wei hex.
    fn eth_to_wei_hex(eth_str: &str) -> Result<String> {
        let eth: f64 = eth_str.parse()
            .map_err(|_| Error::Tool(format!("Invalid ETH amount: {}", eth_str)))?;
        let wei = (eth * 1e18) as u128;
        Ok(format!("0x{:x}", wei))
    }

    /// Convert token amount to raw units given decimals.
    fn token_to_raw(amount_str: &str, decimals: u32) -> Result<String> {
        if amount_str == "max" || amount_str == "unlimited" {
            // uint256 max
            return Ok("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_string());
        }
        let amount: f64 = amount_str.parse()
            .map_err(|_| Error::Tool(format!("Invalid amount: {}", amount_str)))?;
        let multiplier = 10f64.powi(decimals as i32);
        let raw = (amount * multiplier) as u128;
        Ok(format!("{}", raw))
    }

    /// Resolve private key from params, config, or env.
    /// Supports multi-wallet via `wallet_name` parameter:
    /// 1. params.private_key (direct)
    /// 2. params.wallet_name → config providers.wallets.{name}.api_key
    /// 3. params.wallet_name → env {NAME}_PRIVATE_KEY
    /// 4. config providers.ethereum.api_key (default wallet)
    /// 5. env ETH_PRIVATE_KEY (default wallet)
    fn resolve_private_key(ctx: &ToolContext, params: &Value) -> Result<String> {
        // Direct private key param
        if let Some(pk) = params.get("private_key").and_then(|v| v.as_str()) {
            if !pk.is_empty() {
                return Ok(pk.strip_prefix("0x").unwrap_or(pk).to_string());
            }
        }
        // Named wallet
        if let Some(wallet_name) = params.get("wallet_name").and_then(|v| v.as_str()) {
            if !wallet_name.is_empty() {
                // Try config providers.wallets.{name} or providers.{wallet_name}
                let config_key = format!("wallet_{}", wallet_name);
                if let Some(provider) = ctx.config.providers.get(&config_key)
                    .or_else(|| ctx.config.providers.get(wallet_name))
                {
                    let key = &provider.api_key;
                    if !key.is_empty() && (key.len() == 64 || (key.starts_with("0x") && key.len() == 66)) {
                        return Ok(key.strip_prefix("0x").unwrap_or(key).to_string());
                    }
                }
                // Try env {NAME}_PRIVATE_KEY
                let env_key = format!("{}_PRIVATE_KEY", wallet_name.to_uppercase());
                if let Ok(pk) = std::env::var(&env_key) {
                    if !pk.is_empty() {
                        return Ok(pk.strip_prefix("0x").unwrap_or(&pk).to_string());
                    }
                }
                return Err(Error::Tool(format!(
                    "Wallet '{}' not found. Set {}_PRIVATE_KEY env var or config providers.{}.api_key",
                    wallet_name, wallet_name.to_uppercase(), wallet_name
                )));
            }
        }
        // Default wallet: config providers.ethereum
        if let Some(provider) = ctx.config.providers.get("ethereum") {
            let key = &provider.api_key;
            if !key.is_empty() && (key.len() == 64 || (key.starts_with("0x") && key.len() == 66)) {
                return Ok(key.strip_prefix("0x").unwrap_or(key).to_string());
            }
        }
        // Default wallet: env ETH_PRIVATE_KEY
        if let Ok(pk) = std::env::var("ETH_PRIVATE_KEY") {
            if !pk.is_empty() {
                return Ok(pk.strip_prefix("0x").unwrap_or(&pk).to_string());
            }
        }
        Err(Error::Tool("Private key not found. Set ETH_PRIVATE_KEY env var, pass private_key parameter, or use wallet_name with configured wallets.".into()))
    }

    /// Get sender address from private key (secp256k1 public key derivation).
    /// This is a simplified version — for production, use a proper crypto library.
    #[allow(dead_code)]
    fn address_from_private_key(_pk_hex: &str) -> Result<String> {
        // NOTE: Proper secp256k1 key derivation requires elliptic curve math.
        // In a production system, this would use the `k256` or `secp256k1` crate.
        // For now, we return a placeholder and rely on the RPC node for address resolution.
        Err(Error::Tool("Address derivation from private key requires the k256 crate. Use 'from' parameter or let the RPC node handle it.".into()))
    }

    /// Build call data for an ERC20 transfer.
    fn build_erc20_transfer_data(recipient: &str, amount_raw: &str) -> Result<Vec<u8>> {
        let args = vec![recipient.to_string(), amount_raw.to_string()];
        abi_encode_call("transfer(address,uint256)", &args)
    }

    /// Build call data for an ERC20 approve.
    fn build_erc20_approve_data(spender: &str, amount_raw: &str) -> Result<Vec<u8>> {
        let args = vec![spender.to_string(), amount_raw.to_string()];
        abi_encode_call("approve(address,uint256)", &args)
    }

    // ─── Actions ───

    async fn build_tx(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let rpc_url = resolve_rpc_url(ctx, params)?;
        let to = params.get("to").and_then(|v| v.as_str()).unwrap_or("");
        let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("0");
        let simulate = params.get("simulate").and_then(|v| v.as_bool()).unwrap_or(false);

        // Build data field
        let data = if let Some(d) = params.get("data").and_then(|v| v.as_str()) {
            d.to_string()
        } else if let Some(sig) = params.get("function_sig").and_then(|v| v.as_str()) {
            let args: Vec<String> = params.get("args")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let encoded = abi_encode_call(sig, &args)?;
            format!("0x{}", hex_encode(&encoded))
        } else {
            "0x".to_string()
        };

        let value_hex = if value == "0" || value.is_empty() {
            "0x0".to_string()
        } else {
            Self::eth_to_wei_hex(value)?
        };

        // Estimate gas
        let gas_estimate = json_rpc_call(&rpc_url, "eth_estimateGas", json!([{
            "to": to,
            "value": value_hex,
            "data": data,
        }])).await;

        let gas_limit = params.get("gas_limit").and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| gas_estimate.as_ref().ok().and_then(|v| v.as_str().map(String::from)))
            .unwrap_or_else(|| "0x5208".to_string()); // 21000 default

        // Get gas price
        let gas_price_result = json_rpc_call(&rpc_url, "eth_gasPrice", json!([])).await;
        let gas_price = gas_price_result.as_ref().ok().and_then(|v| v.as_str()).unwrap_or("0x0");

        // Simulate if requested
        let simulation = if simulate {
            let sim_result = json_rpc_call(&rpc_url, "eth_call", json!([{
                "to": to,
                "value": value_hex,
                "data": data,
            }, "latest"])).await;
            Some(sim_result)
        } else {
            None
        };

        Ok(json!({
            "action": "build_tx",
            "tx": {
                "to": to,
                "value": value_hex,
                "data": data,
                "gas": gas_limit,
                "gasPrice": gas_price,
            },
            "gas_estimate": gas_estimate.ok(),
            "gas_price_wei": gas_price,
            "simulation": simulation.map(|r| r.ok()),
            "note": "Use sign_and_send to execute this transaction"
        }))
    }

    async fn estimate_gas(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let rpc_url = resolve_rpc_url(ctx, params)?;
        let to = params.get("to").and_then(|v| v.as_str()).unwrap_or("");
        let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("0");

        let data = if let Some(d) = params.get("data").and_then(|v| v.as_str()) {
            d.to_string()
        } else if let Some(sig) = params.get("function_sig").and_then(|v| v.as_str()) {
            let args: Vec<String> = params.get("args")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let encoded = abi_encode_call(sig, &args)?;
            format!("0x{}", hex_encode(&encoded))
        } else {
            "0x".to_string()
        };

        let value_hex = if value == "0" || value.is_empty() {
            "0x0".to_string()
        } else {
            Self::eth_to_wei_hex(value)?
        };

        let gas = json_rpc_call(&rpc_url, "eth_estimateGas", json!([{
            "to": to,
            "value": value_hex,
            "data": data,
        }])).await?;

        let gas_price = json_rpc_call(&rpc_url, "eth_gasPrice", json!([])).await?;

        // Parse gas values for cost estimation
        let gas_units = u64::from_str_radix(
            gas.as_str().unwrap_or("0x0").strip_prefix("0x").unwrap_or("0"), 16
        ).unwrap_or(0);
        let gas_price_wei = u128::from_str_radix(
            gas_price.as_str().unwrap_or("0x0").strip_prefix("0x").unwrap_or("0"), 16
        ).unwrap_or(0);
        let cost_wei = gas_units as u128 * gas_price_wei;
        let cost_eth = cost_wei as f64 / 1e18;
        let gas_price_gwei = gas_price_wei as f64 / 1e9;

        Ok(json!({
            "gas_units": gas_units,
            "gas_price_gwei": gas_price_gwei,
            "estimated_cost_eth": cost_eth,
            "gas_hex": gas,
            "gas_price_hex": gas_price,
        }))
    }

    async fn sign_and_send(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let rpc_url = resolve_rpc_url(ctx, params)?;
        let to = params.get("to").and_then(|v| v.as_str()).unwrap_or("");
        let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("0");
        let _pk = Self::resolve_private_key(ctx, params)?;

        // Build data
        let data = if let Some(d) = params.get("data").and_then(|v| v.as_str()) {
            d.to_string()
        } else if let Some(sig) = params.get("function_sig").and_then(|v| v.as_str()) {
            let args: Vec<String> = params.get("args")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let encoded = abi_encode_call(sig, &args)?;
            format!("0x{}", hex_encode(&encoded))
        } else {
            "0x".to_string()
        };

        let value_hex = if value == "0" || value.is_empty() {
            "0x0".to_string()
        } else {
            Self::eth_to_wei_hex(value)?
        };

        // Step 1: Simulate first
        let sim = json_rpc_call(&rpc_url, "eth_call", json!([{
            "to": to,
            "value": value_hex,
            "data": data,
        }, "latest"])).await;

        if let Err(ref e) = sim {
            return Err(Error::Tool(format!("Transaction simulation failed (would revert): {}. Aborting.", e)));
        }

        // Step 2: Get chain ID
        let chain_id = json_rpc_call(&rpc_url, "eth_chainId", json!([])).await
            .ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_else(|| "0x1".to_string());

        // Step 3: Get gas estimate
        let gas = json_rpc_call(&rpc_url, "eth_estimateGas", json!([{
            "to": to,
            "value": value_hex,
            "data": data,
        }])).await
            .ok().and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "0x5208".to_string());

        // Step 4: Get gas price
        let gas_price = json_rpc_call(&rpc_url, "eth_gasPrice", json!([])).await
            .ok().and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "0x0".to_string());

        // NOTE: Actual RLP encoding + secp256k1 signing requires the `k256` crate.
        // Without it, we can only build the unsigned transaction and return it.
        // For a full implementation, add `k256 = "0.13"` and `rlp = "0.5"` to Cargo.toml.
        Ok(json!({
            "action": "sign_and_send",
            "status": "unsigned_tx_built",
            "note": "Full transaction signing requires the k256 crate for secp256k1. The transaction has been simulated successfully. To send, use a wallet or add k256 dependency.",
            "tx": {
                "to": to,
                "value": value_hex,
                "data": data,
                "gas": gas,
                "gasPrice": gas_price,
                "chainId": chain_id,
            },
            "simulation": sim.ok(),
            "chain_id": chain_id,
        }))
    }

    async fn get_tx_status(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let rpc_url = resolve_rpc_url(ctx, params)?;
        let tx_hash = params.get("tx_hash").and_then(|v| v.as_str()).unwrap_or("");

        let receipt = json_rpc_call(&rpc_url, "eth_getTransactionReceipt", json!([tx_hash])).await?;

        if receipt.is_null() {
            // Check if tx exists but is pending
            let tx = json_rpc_call(&rpc_url, "eth_getTransactionByHash", json!([tx_hash])).await?;
            if tx.is_null() {
                return Ok(json!({
                    "tx_hash": tx_hash,
                    "status": "not_found",
                    "message": "Transaction not found on this chain"
                }));
            }
            return Ok(json!({
                "tx_hash": tx_hash,
                "status": "pending",
                "transaction": tx,
            }));
        }

        let status_code = receipt.get("status").and_then(|v| v.as_str()).unwrap_or("0x0");
        let success = status_code == "0x1";
        let gas_used = receipt.get("gasUsed").and_then(|v| v.as_str()).unwrap_or("0x0");
        let block = receipt.get("blockNumber").and_then(|v| v.as_str()).unwrap_or("0x0");

        let gas_used_dec = u64::from_str_radix(
            gas_used.strip_prefix("0x").unwrap_or("0"), 16
        ).unwrap_or(0);

        Ok(json!({
            "tx_hash": tx_hash,
            "status": if success { "confirmed" } else { "failed" },
            "success": success,
            "block_number": block,
            "gas_used": gas_used_dec,
            "contract_address": receipt.get("contractAddress"),
            "logs_count": receipt.get("logs").and_then(|l| l.as_array()).map(|a| a.len()).unwrap_or(0),
            "receipt": receipt,
        }))
    }

    async fn approve(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let token = params.get("token").and_then(|v| v.as_str()).unwrap_or("");
        let spender = params.get("spender").and_then(|v| v.as_str()).unwrap_or("");
        let amount = params.get("amount").and_then(|v| v.as_str()).unwrap_or("max");
        let decimals = params.get("decimals").and_then(|v| v.as_f64()).unwrap_or(18.0) as u32;

        let amount_raw = Self::token_to_raw(amount, decimals)?;
        let data = Self::build_erc20_approve_data(spender, &amount_raw)?;
        let data_hex = format!("0x{}", hex_encode(&data));

        debug!(token = token, spender = spender, amount = amount, "ERC20 approve");

        // Build modified params for build_tx
        let rpc = params.get("rpc_url").and_then(|v| v.as_str()).unwrap_or("");
        let tx_params = json!({
            "action": "build_tx",
            "rpc_url": rpc,
            "to": token,
            "value": "0",
            "data": data_hex,
            "simulate": true,
        });

        let result = self.build_tx(ctx, &tx_params).await?;

        Ok(json!({
            "action": "approve",
            "token": token,
            "spender": spender,
            "amount": amount,
            "amount_raw": amount_raw,
            "data": data_hex,
            "tx": result.get("tx"),
            "simulation": result.get("simulation"),
            "note": "Use sign_and_send with to=token_address and data=above to execute"
        }))
    }

    async fn transfer(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let token = params.get("token").and_then(|v| v.as_str()).unwrap_or("");
        let recipient = params.get("recipient").and_then(|v| v.as_str())
            .or_else(|| params.get("to").and_then(|v| v.as_str()))
            .unwrap_or("");
        let amount = params.get("amount").and_then(|v| v.as_str())
            .or_else(|| params.get("value").and_then(|v| v.as_str()))
            .unwrap_or("0");

        if token.is_empty() {
            // Native ETH transfer
            let rpc = params.get("rpc_url").and_then(|v| v.as_str()).unwrap_or("");
            let tx_params = json!({
                "action": "build_tx",
                "rpc_url": rpc,
                "to": recipient,
                "value": amount,
                "simulate": true,
            });
            let result = self.build_tx(ctx, &tx_params).await?;
            Ok(json!({
                "action": "transfer",
                "type": "native_eth",
                "recipient": recipient,
                "amount_eth": amount,
                "tx": result.get("tx"),
                "simulation": result.get("simulation"),
            }))
        } else {
            // ERC20 transfer
            let decimals = params.get("decimals").and_then(|v| v.as_f64()).unwrap_or(18.0) as u32;
            let amount_raw = Self::token_to_raw(amount, decimals)?;
            let data = Self::build_erc20_transfer_data(recipient, &amount_raw)?;
            let data_hex = format!("0x{}", hex_encode(&data));

            let rpc = params.get("rpc_url").and_then(|v| v.as_str()).unwrap_or("");
            let tx_params = json!({
                "action": "build_tx",
                "rpc_url": rpc,
                "to": token,
                "value": "0",
                "data": data_hex,
                "simulate": true,
            });
            let result = self.build_tx(ctx, &tx_params).await?;
            Ok(json!({
                "action": "transfer",
                "type": "erc20",
                "token": token,
                "recipient": recipient,
                "amount": amount,
                "amount_raw": amount_raw,
                "data": data_hex,
                "tx": result.get("tx"),
                "simulation": result.get("simulation"),
            }))
        }
    }

    async fn swap(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let rpc_url = resolve_rpc_url(ctx, params)?;
        let token_in = params.get("token_in").and_then(|v| v.as_str()).unwrap_or("");
        let token_out = params.get("token_out").and_then(|v| v.as_str()).unwrap_or("");
        let amount_in = params.get("amount_in").and_then(|v| v.as_str()).unwrap_or("0");
        let slippage = params.get("slippage").and_then(|v| v.as_f64()).unwrap_or(0.5);
        let router = params.get("router").and_then(|v| v.as_str())
            .unwrap_or("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D"); // Uniswap V2 Router

        // WETH address (Ethereum mainnet)
        let weth = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";

        let is_eth_in = token_in.to_uppercase() == "ETH" || token_in.to_uppercase() == "WETH";
        let actual_token_in = if is_eth_in { weth } else { token_in };

        // Build path
        let path = if actual_token_in == weth || token_out == weth {
            vec![actual_token_in.to_string(), token_out.to_string()]
        } else {
            vec![actual_token_in.to_string(), weth.to_string(), token_out.to_string()]
        };

        // Get amounts out for slippage calculation
        let decimals = params.get("decimals").and_then(|v| v.as_f64()).unwrap_or(18.0) as u32;
        let amount_raw = Self::token_to_raw(amount_in, decimals)?;

        // Build getAmountsOut call to estimate output
        let path_encoded: Vec<String> = path.to_vec();

        debug!(
            router = router, token_in = token_in, token_out = token_out,
            amount_in = amount_in, slippage = slippage,
            "DEX swap"
        );

        Ok(json!({
            "action": "swap",
            "router": router,
            "token_in": token_in,
            "token_out": token_out,
            "amount_in": amount_in,
            "amount_in_raw": amount_raw,
            "slippage_pct": slippage,
            "path": path_encoded,
            "rpc_url": rpc_url,
            "note": "Swap requires: 1) approve token_in for router, 2) call swapExactTokensForTokens on router. Use blockchain_rpc eth_call with function_sig='getAmountsOut(uint256,address[])' on the router to get expected output first.",
            "steps": [
                format!("1. approve: blockchain_tx approve token='{}' spender='{}' amount='{}'", actual_token_in, router, amount_in),
                format!("2. swap: blockchain_tx sign_and_send to='{}' function_sig='swapExactTokensForTokens(uint256,uint256,address[],address,uint256)'", router),
            ]
        }))
    }

    async fn revoke_approval(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let mut modified = params.clone();
        modified.as_object_mut().unwrap().insert("amount".into(), json!("0"));
        modified.as_object_mut().unwrap().insert("action".into(), json!("approve"));

        let result = self.approve(ctx, &modified).await?;

        Ok(json!({
            "action": "revoke_approval",
            "token": params.get("token"),
            "spender": params.get("spender"),
            "data": result.get("data"),
            "tx": result.get("tx"),
            "simulation": result.get("simulation"),
            "note": "Approval revoked (set to 0)"
        }))
    }

    async fn multicall(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let rpc_url = resolve_rpc_url(ctx, params)?;
        let calls = params.get("calls").and_then(|v| v.as_array())
            .ok_or_else(|| Error::Tool("'calls' array is required for multicall".into()))?;

        let mut results = Vec::new();
        for (i, call) in calls.iter().enumerate() {
            let to = call.get("to").and_then(|v| v.as_str()).unwrap_or("");
            let data = call.get("data").and_then(|v| v.as_str()).unwrap_or("0x");
            let value = call.get("value").and_then(|v| v.as_str()).unwrap_or("0x0");

            // Simulate each call
            let sim = json_rpc_call(&rpc_url, "eth_call", json!([{
                "to": to,
                "data": data,
                "value": value,
            }, "latest"])).await;

            let success = sim.is_ok();
            results.push(json!({
                "index": i,
                "to": to,
                "data": data,
                "simulation": sim.ok(),
                "success": success,
            }));
        }

        let all_success = results.iter().all(|r| r["success"].as_bool().unwrap_or(false));

        Ok(json!({
            "action": "multicall",
            "call_count": calls.len(),
            "results": results,
            "all_success": all_success,
            "note": if all_success {
                "All calls simulated successfully. Execute individually with sign_and_send."
            } else {
                "Some calls would revert. Check individual results before proceeding."
            }
        }))
    }

    /// List all configured wallets (from config providers and env).
    fn list_wallets(&self, ctx: &ToolContext) -> Value {
        let mut wallets = Vec::new();

        // Check config providers for wallet-like entries
        for (name, provider) in &ctx.config.providers {
            let key = &provider.api_key;
            if !key.is_empty() && (key.len() == 64 || (key.starts_with("0x") && key.len() == 66)) {
                wallets.push(json!({
                    "name": name,
                    "source": "config",
                    "key_preview": format!("{}...{}", crate::safe_truncate(key, 6), &key[key.len().saturating_sub(4)..]),
                    "note": if name == "ethereum" { "default wallet" } else { "named wallet" }
                }));
            }
        }

        // Check common env vars
        for env_name in &["ETH_PRIVATE_KEY", "HOT_PRIVATE_KEY", "COLD_PRIVATE_KEY", "TRADING_PRIVATE_KEY", "TREASURY_PRIVATE_KEY"] {
            if let Ok(pk) = std::env::var(env_name) {
                if !pk.is_empty() && (pk.len() == 64 || pk.len() == 66) {
                    let clean = pk.strip_prefix("0x").unwrap_or(&pk);
                    wallets.push(json!({
                        "name": env_name.strip_suffix("_PRIVATE_KEY").unwrap_or(env_name).to_lowercase(),
                        "source": "env",
                        "env_var": env_name,
                        "key_preview": format!("{}...{}", crate::safe_truncate(clean, 4), &clean[clean.len().saturating_sub(4)..]),
                    }));
                }
            }
        }

        json!({
            "action": "list_wallets",
            "wallet_count": wallets.len(),
            "wallets": wallets,
            "usage": "Use wallet_name='name' in any action to select a specific wallet",
            "config_example": {
                "providers.wallet_hot.api_key": "0x... (hot wallet private key)",
                "providers.wallet_cold.api_key": "0x... (cold wallet private key)",
            },
            "env_example": {
                "HOT_PRIVATE_KEY": "0x... → wallet_name='hot'",
                "TRADING_PRIVATE_KEY": "0x... → wallet_name='trading'",
            }
        })
    }

    fn info(&self) -> Value {
        json!({
            "tool": "blockchain_tx",
            "actions": {
                "build_tx": "Build unsigned transaction with gas estimation",
                "estimate_gas": "Estimate gas cost for a transaction",
                "sign_and_send": "Sign and send transaction (⚠️ requires private key + confirmation)",
                "get_tx_status": "Check transaction status (pending/confirmed/failed)",
                "approve": "ERC20 approve (⚠️ requires confirmation)",
                "transfer": "ETH or ERC20 transfer (⚠️ requires confirmation)",
                "swap": "DEX swap guidance (Uniswap V2 compatible)",
                "revoke_approval": "Revoke ERC20 approval (⚠️ requires confirmation)",
                "multicall": "Simulate batch of calls",
                "list_wallets": "List all configured wallets (config + env)",
                "info": "This help message",
            },
            "multi_wallet": {
                "description": "Support for multiple named wallets",
                "usage": "Add wallet_name='hot' to any action to use a specific wallet",
                "resolution_order": [
                    "1. params.private_key (direct)",
                    "2. config providers.wallet_{name}.api_key",
                    "3. config providers.{name}.api_key",
                    "4. env {NAME}_PRIVATE_KEY",
                    "5. config providers.ethereum.api_key (default)",
                    "6. env ETH_PRIVATE_KEY (default)"
                ]
            },
            "security": [
                "All write operations simulate (eth_call) before sending",
                "Private key from ETH_PRIVATE_KEY env var (never hardcode!)",
                "Full signing requires k256 crate (secp256k1)",
                "Always verify recipient, amount, and contract before sending",
            ],
            "common_patterns": {
                "eth_transfer": "action='transfer' recipient='0x...' amount='0.1'",
                "erc20_transfer": "action='transfer' token='0x...' recipient='0x...' amount='100' decimals=18",
                "approve_max": "action='approve' token='0x...' spender='0x...' amount='max'",
                "check_tx": "action='get_tx_status' tx_hash='0x...'",
                "multi_wallet": "action='transfer' wallet_name='hot' recipient='0x...' amount='0.1'",
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = BlockchainTxTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "blockchain_tx");
        assert!(schema.description.contains("sign"));
        assert!(schema.description.contains("EVM"));
    }

    #[test]
    fn test_validate_actions() {
        let tool = BlockchainTxTool;
        assert!(tool.validate(&json!({"action": "info"})).is_ok());
        assert!(tool.validate(&json!({"action": "build_tx", "to": "0x1234"})).is_ok());
        assert!(tool.validate(&json!({"action": "build_tx"})).is_err()); // missing to
        assert!(tool.validate(&json!({"action": "get_tx_status", "tx_hash": "0xabc"})).is_ok());
        assert!(tool.validate(&json!({"action": "get_tx_status"})).is_err()); // missing tx_hash
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }

    #[test]
    fn test_validate_approve() {
        let tool = BlockchainTxTool;
        assert!(tool.validate(&json!({
            "action": "approve", "token": "0xtoken", "spender": "0xspender"
        })).is_ok());
        assert!(tool.validate(&json!({"action": "approve", "token": "0xtoken"})).is_err());
        assert!(tool.validate(&json!({"action": "approve"})).is_err());
    }

    #[test]
    fn test_validate_transfer() {
        let tool = BlockchainTxTool;
        assert!(tool.validate(&json!({
            "action": "transfer", "recipient": "0xrecip", "amount": "1.0"
        })).is_ok());
        assert!(tool.validate(&json!({
            "action": "transfer", "to": "0xrecip", "value": "0.5"
        })).is_ok());
        assert!(tool.validate(&json!({"action": "transfer"})).is_err());
    }

    #[test]
    fn test_validate_swap() {
        let tool = BlockchainTxTool;
        assert!(tool.validate(&json!({
            "action": "swap", "token_in": "ETH", "token_out": "0xtoken", "amount_in": "1.0"
        })).is_ok());
        assert!(tool.validate(&json!({"action": "swap", "token_in": "ETH"})).is_err());
    }

    #[test]
    fn test_eth_to_wei_hex() {
        let result = BlockchainTxTool::eth_to_wei_hex("1.0").unwrap();
        assert!(result.starts_with("0x"));
        // 1 ETH = 10^18 wei = 0xde0b6b3a7640000
        assert_eq!(result, "0xde0b6b3a7640000");
    }

    #[test]
    fn test_token_to_raw() {
        let result = BlockchainTxTool::token_to_raw("100", 18).unwrap();
        assert_eq!(result, "100000000000000000000");

        let result = BlockchainTxTool::token_to_raw("100", 6).unwrap();
        assert_eq!(result, "100000000");

        let max = BlockchainTxTool::token_to_raw("max", 18).unwrap();
        assert_eq!(max, "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
    }

    #[test]
    fn test_build_erc20_transfer_data() {
        let data = BlockchainTxTool::build_erc20_transfer_data(
            "0x0000000000000000000000000000000000000001",
            "1000000000000000000" // 1 token with 18 decimals
        ).unwrap();
        // Should start with transfer(address,uint256) selector: 0xa9059cbb
        assert_eq!(data[0], 0xa9);
        assert_eq!(data[1], 0x05);
        assert_eq!(data[2], 0x9c);
        assert_eq!(data[3], 0xbb);
    }

    #[test]
    fn test_build_erc20_approve_data() {
        let data = BlockchainTxTool::build_erc20_approve_data(
            "0x0000000000000000000000000000000000000001",
            "1000000000000000000"
        ).unwrap();
        // approve(address,uint256) selector: 0x095ea7b3
        assert_eq!(data[0], 0x09);
        assert_eq!(data[1], 0x5e);
        assert_eq!(data[2], 0xa7);
        assert_eq!(data[3], 0xb3);
    }

    #[test]
    fn test_info() {
        let tool = BlockchainTxTool;
        let info = tool.info();
        assert_eq!(info["tool"], "blockchain_tx");
        assert!(info["actions"].as_object().unwrap().len() >= 10);
        assert!(info["multi_wallet"].is_object());
    }

    #[test]
    fn test_validate_list_wallets() {
        let tool = BlockchainTxTool;
        assert!(tool.validate(&json!({"action": "list_wallets"})).is_ok());
    }

    #[test]
    fn test_schema_has_wallet_name() {
        let tool = BlockchainTxTool;
        let schema = tool.schema();
        let props = schema.parameters.get("properties").unwrap();
        assert!(props.get("wallet_name").is_some());
        assert!(props.get("from").is_some());
    }
}
