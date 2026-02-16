use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// Multisig wallet tool for Gnosis Safe (Safe{Wallet}) operations.
///
/// Supports:
/// - **safe_info**: Get Safe details (owners, threshold, nonce, modules)
/// - **list_transactions**: List pending/executed transactions
/// - **propose_transaction**: Propose a new transaction for signing
/// - **confirm_transaction**: Add a confirmation (signature) to a pending tx
/// - **balances**: Get token balances of a Safe
/// - **delegates**: Manage Safe delegates
/// - **info**: Show supported operations
///
/// Uses Safe Transaction Service API (https://safe-transaction-mainnet.safe.global).
pub struct MultisigTool;

#[async_trait]
impl Tool for MultisigTool {
    fn schema(&self) -> ToolSchema {
        let mut props = serde_json::Map::new();
        let str_prop = |desc: &str| -> Value { json!({"type": "string", "description": desc}) };
        let num_prop = |desc: &str| -> Value { json!({"type": "number", "description": desc}) };

        props.insert("action".into(), str_prop("Action: safe_info|list_transactions|propose_transaction|confirm_transaction|balances|delegates|estimate|info"));
        props.insert("chain".into(), str_prop("Chain: 'ethereum'|'polygon'|'arbitrum'|'optimism'|'bsc'|'base'|'avalanche'|'gnosis' (default: ethereum)"));
        props.insert("safe_address".into(), str_prop("Safe (multisig) wallet address (0x...)"));
        props.insert("to".into(), str_prop("(propose_transaction) Destination address"));
        props.insert("value".into(), str_prop("(propose_transaction) ETH value in wei (use '0' for ERC20 calls)"));
        props.insert("data".into(), str_prop("(propose_transaction) Hex-encoded call data (0x...)"));
        props.insert("operation".into(), num_prop("(propose_transaction) 0=Call, 1=DelegateCall (default: 0)"));
        props.insert("safe_tx_hash".into(), str_prop("(confirm_transaction) Safe transaction hash to confirm"));
        props.insert("signature".into(), str_prop("(confirm_transaction) Signature hex for confirmation"));
        props.insert("nonce".into(), str_prop("(propose_transaction) Transaction nonce. Auto-fetched if omitted."));
        props.insert("tx_type".into(), str_prop("(list_transactions) Filter: 'pending'|'executed'|'all' (default: pending)"));
        props.insert("limit".into(), num_prop("(list_transactions) Number of results (default: 20)"));

        ToolSchema {
            name: "multisig",
            description: "Gnosis Safe (Safe{Wallet}) multisig operations. Query Safe info, list pending transactions, \
                propose new transactions, confirm (sign) pending transactions, check balances. \
                Uses Safe Transaction Service API. Supports Ethereum, Polygon, Arbitrum, Optimism, BSC, Base, Avalanche, Gnosis Chain. \
                For proposing transactions, build calldata with blockchain_tx first, then use multisig propose_transaction.",
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid = ["safe_info", "list_transactions", "propose_transaction", "confirm_transaction", "balances", "delegates", "estimate", "info"];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        match action {
            "safe_info" | "list_transactions" | "balances" | "delegates" => {
                if params.get("safe_address").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'safe_address' is required".into()));
                }
            }
            "propose_transaction" => {
                if params.get("safe_address").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'safe_address' is required".into()));
                }
                if params.get("to").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'to' address is required for propose_transaction".into()));
                }
            }
            "confirm_transaction" => {
                if params.get("safe_tx_hash").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'safe_tx_hash' is required for confirm_transaction".into()));
                }
                if params.get("signature").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'signature' is required for confirm_transaction".into()));
                }
            }
            "estimate" => {
                if params.get("safe_address").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'safe_address' is required".into()));
                }
                if params.get("to").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'to' address is required for estimate".into()));
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn execute(&self, _ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap_or("");
        let chain = params.get("chain").and_then(|v| v.as_str()).unwrap_or("ethereum");
        let client = Client::new();

        match action {
            "safe_info" => self.safe_info(chain, &params, &client).await,
            "list_transactions" => self.list_transactions(chain, &params, &client).await,
            "propose_transaction" => self.propose_transaction(chain, &params, &client).await,
            "confirm_transaction" => self.confirm_transaction(chain, &params, &client).await,
            "balances" => self.balances(chain, &params, &client).await,
            "delegates" => self.delegates(chain, &params, &client).await,
            "estimate" => self.estimate(chain, &params, &client).await,
            "info" => Ok(self.info()),
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

impl MultisigTool {
    /// Get Safe Transaction Service base URL for a chain.
    fn api_base(chain: &str) -> &'static str {
        match chain.to_lowercase().as_str() {
            "ethereum" | "eth" | "mainnet" => "https://safe-transaction-mainnet.safe.global",
            "polygon" | "matic" => "https://safe-transaction-polygon.safe.global",
            "arbitrum" | "arb" => "https://safe-transaction-arbitrum.safe.global",
            "optimism" | "op" => "https://safe-transaction-optimism.safe.global",
            "bsc" | "bnb" => "https://safe-transaction-bsc.safe.global",
            "base" => "https://safe-transaction-base.safe.global",
            "avalanche" | "avax" => "https://safe-transaction-avalanche.safe.global",
            "gnosis" | "xdai" => "https://safe-transaction-gnosis-chain.safe.global",
            _ => "https://safe-transaction-mainnet.safe.global",
        }
    }

    // ─── Safe Info ───

    async fn safe_info(&self, chain: &str, params: &Value, client: &Client) -> Result<Value> {
        let safe_address = params["safe_address"].as_str().unwrap_or("");
        let base = Self::api_base(chain);
        let url = format!("{}/api/v1/safes/{}/", base, safe_address);

        debug!(url = %url, "Safe info");
        let resp = client.get(&url)
            .header("Accept", "application/json")
            .send().await
            .map_err(|e| Error::Tool(format!("Safe API request failed: {}", e)))?;

        let status = resp.status();
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse Safe API response: {}", e)))?;

        if !status.is_success() {
            return Err(Error::Tool(format!("Safe API error ({}): {:?}", status, body)));
        }

        Ok(json!({
            "action": "safe_info",
            "chain": chain,
            "safe_address": safe_address,
            "owners": body.get("owners"),
            "threshold": body.get("threshold"),
            "nonce": body.get("nonce"),
            "modules": body.get("modules"),
            "fallback_handler": body.get("fallbackHandler"),
            "guard": body.get("guard"),
            "version": body.get("version"),
        }))
    }

    // ─── List Transactions ───

    async fn list_transactions(&self, chain: &str, params: &Value, client: &Client) -> Result<Value> {
        let safe_address = params["safe_address"].as_str().unwrap_or("");
        let tx_type = params.get("tx_type").and_then(|v| v.as_str()).unwrap_or("pending");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20);
        let base = Self::api_base(chain);

        let url = match tx_type {
            "pending" => format!(
                "{}/api/v1/safes/{}/multisig-transactions/?executed=false&limit={}",
                base, safe_address, limit
            ),
            "executed" => format!(
                "{}/api/v1/safes/{}/multisig-transactions/?executed=true&limit={}",
                base, safe_address, limit
            ),
            _ => format!(
                "{}/api/v1/safes/{}/multisig-transactions/?limit={}",
                base, safe_address, limit
            ),
        };

        debug!(url = %url, "Safe list transactions");
        let resp = client.get(&url)
            .header("Accept", "application/json")
            .send().await
            .map_err(|e| Error::Tool(format!("Safe API request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        let results = body.get("results").and_then(|r| r.as_array()).cloned().unwrap_or_default();
        let mut txs = Vec::new();
        for tx in &results {
            txs.push(json!({
                "safe_tx_hash": tx.get("safeTxHash"),
                "to": tx.get("to"),
                "value": tx.get("value"),
                "data": tx.get("data"),
                "operation": tx.get("operation"),
                "nonce": tx.get("nonce"),
                "confirmations_required": tx.get("confirmationsRequired"),
                "confirmations": tx.get("confirmations").and_then(|c| c.as_array()).map(|a| a.len()),
                "is_executed": tx.get("isExecuted"),
                "is_successful": tx.get("isSuccessful"),
                "submission_date": tx.get("submissionDate"),
                "execution_date": tx.get("executionDate"),
                "executor": tx.get("executor"),
                "data_decoded": tx.get("dataDecoded"),
            }));
        }

        Ok(json!({
            "action": "list_transactions",
            "chain": chain,
            "safe_address": safe_address,
            "tx_type": tx_type,
            "count": txs.len(),
            "total": body.get("count"),
            "transactions": txs,
        }))
    }

    // ─── Propose Transaction ───

    async fn propose_transaction(&self, chain: &str, params: &Value, client: &Client) -> Result<Value> {
        let safe_address = params["safe_address"].as_str().unwrap_or("");
        let to = params["to"].as_str().unwrap_or("");
        let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("0");
        let data = params.get("data").and_then(|v| v.as_str()).unwrap_or("0x");
        let operation = params.get("operation").and_then(|v| v.as_u64()).unwrap_or(0);
        let base = Self::api_base(chain);

        // Get current nonce if not provided
        let nonce = if let Some(n) = params.get("nonce").and_then(|v| v.as_str()) {
            n.to_string()
        } else {
            // Fetch from Safe info
            let info_url = format!("{}/api/v1/safes/{}/", base, safe_address);
            let info_resp = client.get(&info_url)
                .header("Accept", "application/json")
                .send().await.ok();
            if let Some(resp) = info_resp {
                let info: Value = resp.json().await.unwrap_or(json!({}));
                info.get("nonce").and_then(|n| n.as_u64()).unwrap_or(0).to_string()
            } else {
                "0".to_string()
            }
        };

        // Build the transaction proposal
        // NOTE: In production, this would need to be signed by an owner.
        // The Safe Transaction Service requires a valid signature.
        let tx_body = json!({
            "to": to,
            "value": value,
            "data": data,
            "operation": operation,
            "safeTxGas": "0",
            "baseGas": "0",
            "gasPrice": "0",
            "gasToken": "0x0000000000000000000000000000000000000000",
            "refundReceiver": "0x0000000000000000000000000000000000000000",
            "nonce": nonce.parse::<u64>().unwrap_or(0),
        });

        Ok(json!({
            "action": "propose_transaction",
            "chain": chain,
            "safe_address": safe_address,
            "transaction": tx_body,
            "api_endpoint": format!("{}/api/v1/safes/{}/multisig-transactions/", base, safe_address),
            "note": "To submit this proposal, POST the transaction body to the API endpoint with a valid owner signature. \
                     Use the Safe{Wallet} web app or sign with blockchain_tx and include the signature field.",
            "steps": [
                "1. Hash the transaction using EIP-712 typed data signing",
                "2. Sign the hash with an owner's private key",
                "3. POST to the API endpoint with the signature",
                "4. Other owners confirm via confirm_transaction action"
            ]
        }))
    }

    // ─── Confirm Transaction ───

    async fn confirm_transaction(&self, chain: &str, params: &Value, client: &Client) -> Result<Value> {
        let safe_tx_hash = params["safe_tx_hash"].as_str().unwrap_or("");
        let signature = params["signature"].as_str().unwrap_or("");
        let base = Self::api_base(chain);

        let url = format!("{}/api/v1/multisig-transactions/{}/confirmations/", base, safe_tx_hash);

        debug!(url = %url, "Safe confirm transaction");
        let resp = client.post(&url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .json(&json!({"signature": signature}))
            .send().await
            .map_err(|e| Error::Tool(format!("Safe API request failed: {}", e)))?;

        let status = resp.status();
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        if !status.is_success() {
            return Err(Error::Tool(format!("Safe API error ({}): {:?}", status, body)));
        }

        Ok(json!({
            "action": "confirm_transaction",
            "chain": chain,
            "safe_tx_hash": safe_tx_hash,
            "status": "confirmed",
            "response": body,
        }))
    }

    // ─── Balances ───

    async fn balances(&self, chain: &str, params: &Value, client: &Client) -> Result<Value> {
        let safe_address = params["safe_address"].as_str().unwrap_or("");
        let base = Self::api_base(chain);
        let url = format!("{}/api/v1/safes/{}/balances/usd/", base, safe_address);

        debug!(url = %url, "Safe balances");
        let resp = client.get(&url)
            .header("Accept", "application/json")
            .send().await
            .map_err(|e| Error::Tool(format!("Safe API request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        let balances = body.as_array().cloned().unwrap_or_default();
        let mut tokens = Vec::new();
        let mut total_usd = 0.0f64;
        for bal in &balances {
            let fiat = bal.get("fiatBalance").and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
            total_usd += fiat;
            tokens.push(json!({
                "token": bal.get("token"),
                "balance": bal.get("balance"),
                "fiat_balance": bal.get("fiatBalance"),
                "fiat_conversion": bal.get("fiatConversion"),
            }));
        }

        Ok(json!({
            "action": "balances",
            "chain": chain,
            "safe_address": safe_address,
            "total_usd": total_usd,
            "token_count": tokens.len(),
            "tokens": tokens,
        }))
    }

    // ─── Delegates ───

    async fn delegates(&self, chain: &str, params: &Value, client: &Client) -> Result<Value> {
        let safe_address = params["safe_address"].as_str().unwrap_or("");
        let base = Self::api_base(chain);
        let url = format!("{}/api/v2/delegates/?safe={}", base, safe_address);

        debug!(url = %url, "Safe delegates");
        let resp = client.get(&url)
            .header("Accept", "application/json")
            .send().await
            .map_err(|e| Error::Tool(format!("Safe API request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        Ok(json!({
            "action": "delegates",
            "chain": chain,
            "safe_address": safe_address,
            "count": body.get("count"),
            "delegates": body.get("results"),
        }))
    }

    // ─── Estimate ───

    async fn estimate(&self, chain: &str, params: &Value, client: &Client) -> Result<Value> {
        let safe_address = params["safe_address"].as_str().unwrap_or("");
        let to = params["to"].as_str().unwrap_or("");
        let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("0");
        let data = params.get("data").and_then(|v| v.as_str()).unwrap_or("0x");
        let operation = params.get("operation").and_then(|v| v.as_u64()).unwrap_or(0);
        let base = Self::api_base(chain);

        let url = format!("{}/api/v1/safes/{}/multisig-transactions/estimations/", base, safe_address);

        debug!(url = %url, "Safe estimate");
        let resp = client.post(&url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .json(&json!({
                "to": to,
                "value": value,
                "data": data,
                "operation": operation,
            }))
            .send().await
            .map_err(|e| Error::Tool(format!("Safe API request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        Ok(json!({
            "action": "estimate",
            "chain": chain,
            "safe_address": safe_address,
            "safe_tx_gas": body.get("safeTxGas"),
            "response": body,
        }))
    }

    fn info(&self) -> Value {
        json!({
            "tool": "multisig",
            "description": "Gnosis Safe (Safe{Wallet}) multisig wallet operations",
            "actions": {
                "safe_info": "Get Safe details (owners, threshold, nonce, version)",
                "list_transactions": "List pending or executed multisig transactions",
                "propose_transaction": "Propose a new transaction for signing",
                "confirm_transaction": "Add a confirmation (signature) to a pending tx",
                "balances": "Get token balances with USD values",
                "delegates": "List Safe delegates",
                "estimate": "Estimate safeTxGas for a transaction",
                "info": "This help message"
            },
            "supported_chains": [
                "ethereum", "polygon", "arbitrum", "optimism", "bsc", "base", "avalanche", "gnosis"
            ],
            "workflow": [
                "1. multisig safe_info safe_address='0x...' — check owners and threshold",
                "2. blockchain_tx build_tx — build the calldata for the desired operation",
                "3. multisig propose_transaction safe_address='0x...' to='0x...' data='0x...' — propose",
                "4. multisig list_transactions safe_address='0x...' tx_type='pending' — check pending",
                "5. multisig confirm_transaction safe_tx_hash='0x...' signature='0x...' — confirm",
                "6. Once threshold reached, execute via Safe{Wallet} web app or direct contract call"
            ],
            "api_docs": "https://safe-transaction-mainnet.safe.global/"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = MultisigTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "multisig");
        assert!(schema.description.contains("Safe") || schema.description.contains("multisig"));
    }

    #[test]
    fn test_validate() {
        let tool = MultisigTool;
        assert!(tool.validate(&json!({"action": "info"})).is_ok());
        assert!(tool.validate(&json!({"action": "safe_info", "safe_address": "0x1234"})).is_ok());
        assert!(tool.validate(&json!({"action": "safe_info"})).is_err()); // missing safe_address
        assert!(tool.validate(&json!({"action": "list_transactions", "safe_address": "0x1234"})).is_ok());
        assert!(tool.validate(&json!({"action": "propose_transaction", "safe_address": "0x1234", "to": "0x5678"})).is_ok());
        assert!(tool.validate(&json!({"action": "propose_transaction", "safe_address": "0x1234"})).is_err()); // missing to
        assert!(tool.validate(&json!({"action": "confirm_transaction", "safe_tx_hash": "0xabc", "signature": "0xdef"})).is_ok());
        assert!(tool.validate(&json!({"action": "confirm_transaction"})).is_err()); // missing hash
        assert!(tool.validate(&json!({"action": "balances", "safe_address": "0x1234"})).is_ok());
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }

    #[test]
    fn test_api_base() {
        assert!(MultisigTool::api_base("ethereum").contains("mainnet"));
        assert!(MultisigTool::api_base("polygon").contains("polygon"));
        assert!(MultisigTool::api_base("arbitrum").contains("arbitrum"));
        assert!(MultisigTool::api_base("base").contains("base"));
    }

    #[test]
    fn test_info() {
        let tool = MultisigTool;
        let info = tool.info();
        assert_eq!(info["tool"], "multisig");
        assert!(info["actions"].as_object().unwrap().len() >= 7);
        assert!(info["supported_chains"].as_array().unwrap().len() >= 8);
    }
}
