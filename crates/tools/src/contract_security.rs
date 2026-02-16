use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// Contract & token security analysis tool.
///
/// Uses GoPlus Security API (free, no key required) to check:
/// - Token security (honeypot, tax, ownership, proxy, etc.)
/// - Address security (malicious address detection)
/// - Approval security (risky token approvals)
/// - NFT security
/// - dApp security
///
/// Also supports basic contract source verification via Etherscan-compatible APIs.
pub struct ContractSecurityTool;

/// GoPlus chain ID mapping
fn goplus_chain_id(chain: &str) -> String {
    match chain.to_lowercase().as_str() {
        "ethereum" | "eth" | "1" => "1".to_string(),
        "bsc" | "bnb" | "56" => "56".to_string(),
        "polygon" | "matic" | "137" => "137".to_string(),
        "arbitrum" | "arb" | "42161" => "42161".to_string(),
        "optimism" | "op" | "10" => "10".to_string(),
        "avalanche" | "avax" | "43114" => "43114".to_string(),
        "base" | "8453" => "8453".to_string(),
        "fantom" | "ftm" | "250" => "250".to_string(),
        "cronos" | "25" => "25".to_string(),
        "gnosis" | "xdai" | "100" => "100".to_string(),
        "linea" | "59144" => "59144".to_string(),
        "zksync" | "324" => "324".to_string(),
        "solana" | "sol" => "solana".to_string(),
        "tron" | "trx" => "tron".to_string(),
        _ => chain.to_lowercase(),
    }
}

#[async_trait]
impl Tool for ContractSecurityTool {
    fn schema(&self) -> ToolSchema {
        let str_prop = |desc: &str| -> Value { json!({"type": "string", "description": desc}) };

        let mut props = serde_json::Map::new();
        props.insert("action".into(), str_prop("Action: token_security|address_security|approval_security|nft_security|dapp_security|rugpull_detection|contract_source|info"));
        props.insert("chain".into(), str_prop("Chain: 'ethereum'|'bsc'|'polygon'|'arbitrum'|'optimism'|'avalanche'|'base'|'fantom'|'solana'|'tron' or chain ID (default: ethereum)"));
        props.insert("address".into(), str_prop("Contract or token address (0x...). For token_security, this is the token contract."));
        props.insert("addresses".into(), json!({"type": "array", "items": {"type": "string"}, "description": "Multiple addresses to check (batch mode)"}));
        props.insert("wallet".into(), str_prop("(approval_security) Wallet address to check approvals for"));
        props.insert("url".into(), str_prop("(dapp_security) dApp URL to check"));

        ToolSchema {
            name: "contract_security",
            description: "Check smart contract and token security using GoPlus Security API (free, no key needed). \
                Actions: token_security (honeypot detection, buy/sell tax, ownership renounced, proxy contract, \
                mint function, blacklist, etc.), address_security (malicious address check), \
                approval_security (risky ERC20 approvals for a wallet), nft_security (NFT contract risks), \
                dapp_security (phishing/scam URL check), rugpull_detection (rug pull risk analysis), \
                contract_source (verify source code via Etherscan), info (supported chains). \
                Supports: Ethereum, BSC, Polygon, Arbitrum, Optimism, Avalanche, Base, Fantom, Solana, Tron.",
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
            "token_security", "address_security", "approval_security",
            "nft_security", "dapp_security", "rugpull_detection",
            "contract_source", "info",
        ];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        match action {
            "token_security" | "nft_security" | "contract_source" | "rugpull_detection" => {
                let has_addr = params.get("address").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
                let has_addrs = params.get("addresses").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
                if !has_addr && !has_addrs {
                    return Err(Error::Tool(format!("'address' or 'addresses' is required for {}", action)));
                }
            }
            "address_security" => {
                let has_addr = params.get("address").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
                let has_addrs = params.get("addresses").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
                if !has_addr && !has_addrs {
                    return Err(Error::Tool("'address' or 'addresses' is required for address_security".into()));
                }
            }
            "approval_security" => {
                if params.get("wallet").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'wallet' address is required for approval_security".into()));
                }
            }
            "dapp_security" => {
                if params.get("url").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'url' is required for dapp_security".into()));
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap_or("");
        let client = Client::new();

        match action {
            "token_security" => self.token_security(&ctx, &params, &client).await,
            "address_security" => self.address_security(&params, &client).await,
            "approval_security" => self.approval_security(&params, &client).await,
            "nft_security" => self.nft_security(&params, &client).await,
            "dapp_security" => self.dapp_security(&params, &client).await,
            "rugpull_detection" => self.rugpull_detection(&params, &client).await,
            "contract_source" => self.contract_source(&ctx, &params, &client).await,
            "info" => Ok(self.info()),
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

impl ContractSecurityTool {
    /// Get addresses as comma-separated string.
    fn get_addresses(params: &Value) -> String {
        if let Some(addr) = params.get("address").and_then(|v| v.as_str()) {
            if !addr.is_empty() {
                return addr.to_lowercase();
            }
        }
        if let Some(addrs) = params.get("addresses").and_then(|v| v.as_array()) {
            return addrs.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_lowercase())
                .collect::<Vec<_>>()
                .join(",");
        }
        String::new()
    }

    /// Call GoPlus API.
    async fn goplus_get(url: &str, client: &Client) -> Result<Value> {
        debug!(url = %url, "GoPlus API call");
        let resp = client.get(url)
            .header("User-Agent", "Mozilla/5.0")
            .timeout(std::time::Duration::from_secs(30))
            .send().await
            .map_err(|e| Error::Tool(format!("GoPlus API request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse GoPlus response: {}", e)))?;

        let code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        if code != 1 {
            let msg = body.get("message").and_then(|v| v.as_str()).unwrap_or("unknown error");
            return Err(Error::Tool(format!("GoPlus API error (code {}): {}", code, msg)));
        }

        Ok(body.get("result").cloned().unwrap_or(json!({})))
    }

    // ─── Token Security ───

    async fn token_security(&self, _ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let chain = params.get("chain").and_then(|v| v.as_str()).unwrap_or("ethereum");
        let chain_id = goplus_chain_id(chain);
        let addresses = Self::get_addresses(params);

        let url = format!(
            "https://api.gopluslabs.io/api/v1/token_security/{}?contract_addresses={}",
            chain_id, addresses
        );

        let result = Self::goplus_get(&url, client).await?;

        // Parse and enrich with risk summary
        let mut tokens = Vec::new();
        if let Some(obj) = result.as_object() {
            for (addr, data) in obj {
                let mut risks = Vec::new();
                let mut warnings = Vec::new();

                // Check critical risks
                if data.get("is_honeypot").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("🚨 HONEYPOT: Cannot sell tokens");
                }
                if data.get("is_proxy").and_then(|v| v.as_str()) == Some("1") {
                    warnings.push("⚠️ Proxy contract (upgradeable)");
                }
                if data.get("is_mintable").and_then(|v| v.as_str()) == Some("1") {
                    warnings.push("⚠️ Mintable (owner can create new tokens)");
                }
                if data.get("can_take_back_ownership").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("🚨 Owner can reclaim ownership");
                }
                if data.get("owner_change_balance").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("🚨 Owner can modify balances");
                }
                if data.get("hidden_owner").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("🚨 Hidden owner detected");
                }
                if data.get("selfdestruct").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("🚨 Contract can self-destruct");
                }
                if data.get("external_call").and_then(|v| v.as_str()) == Some("1") {
                    warnings.push("⚠️ External calls detected");
                }
                if data.get("is_blacklisted").and_then(|v| v.as_str()) == Some("1") {
                    warnings.push("⚠️ Has blacklist function");
                }
                if data.get("is_whitelisted").and_then(|v| v.as_str()) == Some("1") {
                    warnings.push("⚠️ Has whitelist function");
                }
                if data.get("is_anti_whale").and_then(|v| v.as_str()) == Some("1") {
                    warnings.push("ℹ️ Anti-whale mechanism");
                }
                if data.get("trading_cooldown").and_then(|v| v.as_str()) == Some("1") {
                    warnings.push("ℹ️ Trading cooldown enabled");
                }
                if data.get("cannot_sell_all").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("🚨 Cannot sell all tokens");
                }

                // Parse tax
                let buy_tax = data.get("buy_tax").and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                let sell_tax = data.get("sell_tax").and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                if buy_tax > 0.1 { risks.push("🚨 High buy tax (>10%)"); }
                else if buy_tax > 0.05 { warnings.push("⚠️ Moderate buy tax (>5%)"); }
                if sell_tax > 0.1 { risks.push("🚨 High sell tax (>10%)"); }
                else if sell_tax > 0.05 { warnings.push("⚠️ Moderate sell tax (>5%)"); }

                let risk_level = if !risks.is_empty() { "HIGH" }
                    else if !warnings.is_empty() { "MEDIUM" }
                    else { "LOW" };

                tokens.push(json!({
                    "address": addr,
                    "name": data.get("token_name"),
                    "symbol": data.get("token_symbol"),
                    "risk_level": risk_level,
                    "risks": risks,
                    "warnings": warnings,
                    "buy_tax": format!("{:.1}%", buy_tax * 100.0),
                    "sell_tax": format!("{:.1}%", sell_tax * 100.0),
                    "is_honeypot": data.get("is_honeypot"),
                    "is_open_source": data.get("is_open_source"),
                    "is_proxy": data.get("is_proxy"),
                    "is_mintable": data.get("is_mintable"),
                    "owner_address": data.get("owner_address"),
                    "creator_address": data.get("creator_address"),
                    "holder_count": data.get("holder_count"),
                    "total_supply": data.get("total_supply"),
                    "lp_holder_count": data.get("lp_holder_count"),
                    "lp_total_supply": data.get("lp_total_supply"),
                    "is_true_token": data.get("is_true_token"),
                    "is_airdrop_scam": data.get("is_airdrop_scam"),
                    "trust_list": data.get("trust_list"),
                    "other_potential_risks": data.get("other_potential_risks"),
                    "note": data.get("note"),
                    "raw": data,
                }));
            }
        }

        Ok(json!({
            "action": "token_security",
            "chain": chain,
            "chain_id": chain_id,
            "count": tokens.len(),
            "tokens": tokens,
            "source": "goplus"
        }))
    }

    // ─── Address Security ───

    async fn address_security(&self, params: &Value, client: &Client) -> Result<Value> {
        let addresses = Self::get_addresses(params);

        let url = format!(
            "https://api.gopluslabs.io/api/v1/address_security/{}",
            addresses
        );

        let result = Self::goplus_get(&url, client).await?;

        let mut analysis = Vec::new();
        // GoPlus returns data keyed by address
        if let Some(obj) = result.as_object() {
            for (addr, data) in obj {
                let mut risks = Vec::new();

                if data.get("honeypot_related_address").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Associated with honeypot contracts");
                }
                if data.get("phishing_activities").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Phishing activities detected");
                }
                if data.get("blackmail_activities").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Blackmail activities detected");
                }
                if data.get("stealing_attack").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Stealing attack detected");
                }
                if data.get("fake_kyc").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Fake KYC detected");
                }
                if data.get("malicious_mining_activities").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Malicious mining activities");
                }
                if data.get("darkweb_transactions").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Darkweb transactions detected");
                }
                if data.get("cybercrime").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Cybercrime associated");
                }
                if data.get("money_laundering").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Money laundering associated");
                }
                if data.get("financial_crime").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Financial crime associated");
                }
                if data.get("blacklist_doubt").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Blacklist doubt");
                }
                if data.get("mixer_address").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Mixer/tumbler address");
                }
                if data.get("sanctioned").and_then(|v| v.as_str()) == Some("1") {
                    risks.push("OFAC sanctioned address");
                }

                let risk_level = if risks.is_empty() { "SAFE" } else { "DANGEROUS" };

                analysis.push(json!({
                    "address": addr,
                    "risk_level": risk_level,
                    "risks": risks,
                    "risk_count": risks.len(),
                    "raw": data,
                }));
            }
        }

        Ok(json!({
            "action": "address_security",
            "count": analysis.len(),
            "addresses": analysis,
            "source": "goplus"
        }))
    }

    // ─── Approval Security ───

    async fn approval_security(&self, params: &Value, client: &Client) -> Result<Value> {
        let chain = params.get("chain").and_then(|v| v.as_str()).unwrap_or("ethereum");
        let chain_id = goplus_chain_id(chain);
        let wallet = params.get("wallet").and_then(|v| v.as_str()).unwrap_or("");

        let url = format!(
            "https://api.gopluslabs.io/api/v2/token_approval_security/{}?addresses={}",
            chain_id, wallet.to_lowercase()
        );

        let result = Self::goplus_get(&url, client).await?;

        let mut risky_approvals = Vec::new();
        let mut safe_count = 0u32;

        if let Some(arr) = result.as_array() {
            for item in arr {
                let is_risky = item.get("is_contract_dangerous").and_then(|v| v.as_str()) == Some("1")
                    || item.get("is_open_source").and_then(|v| v.as_str()) == Some("0");

                if is_risky {
                    risky_approvals.push(json!({
                        "token_address": item.get("token_address"),
                        "token_name": item.get("token_name"),
                        "token_symbol": item.get("token_symbol"),
                        "approved_spender": item.get("approved_spender"),
                        "approved_amount": item.get("approved_amount"),
                        "is_contract_dangerous": item.get("is_contract_dangerous"),
                        "is_open_source": item.get("is_open_source"),
                    }));
                } else {
                    safe_count += 1;
                }
            }
        }

        Ok(json!({
            "action": "approval_security",
            "chain": chain,
            "wallet": wallet,
            "risky_approvals": risky_approvals,
            "risky_count": risky_approvals.len(),
            "safe_count": safe_count,
            "recommendation": if !risky_approvals.is_empty() {
                "Revoke risky approvals using blockchain_tx revoke_approval action"
            } else {
                "No risky approvals found"
            },
            "source": "goplus"
        }))
    }

    // ─── NFT Security ───

    async fn nft_security(&self, params: &Value, client: &Client) -> Result<Value> {
        let chain = params.get("chain").and_then(|v| v.as_str()).unwrap_or("ethereum");
        let chain_id = goplus_chain_id(chain);
        let address = Self::get_addresses(params);

        let url = format!(
            "https://api.gopluslabs.io/api/v1/nft_security/{}?contract_addresses={}",
            chain_id, address
        );

        let result = Self::goplus_get(&url, client).await?;

        let mut nfts = Vec::new();
        if let Some(obj) = result.as_object() {
            for (addr, data) in obj {
                let mut risks = Vec::new();

                if data.get("privileged_burn").and_then(|v| v.as_object())
                    .and_then(|o| o.get("value")).and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Owner can burn NFTs");
                }
                if data.get("transfer_without_approval").and_then(|v| v.as_object())
                    .and_then(|o| o.get("value")).and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Transfer without approval possible");
                }
                if data.get("privileged_minting").and_then(|v| v.as_object())
                    .and_then(|o| o.get("value")).and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Privileged minting enabled");
                }
                if data.get("self_destruct").and_then(|v| v.as_object())
                    .and_then(|o| o.get("value")).and_then(|v| v.as_str()) == Some("1") {
                    risks.push("Contract can self-destruct");
                }

                nfts.push(json!({
                    "address": addr,
                    "nft_name": data.get("nft_name"),
                    "nft_symbol": data.get("nft_symbol"),
                    "risks": risks,
                    "risk_count": risks.len(),
                    "raw": data,
                }));
            }
        }

        Ok(json!({
            "action": "nft_security",
            "chain": chain,
            "count": nfts.len(),
            "nfts": nfts,
            "source": "goplus"
        }))
    }

    // ─── dApp Security ───

    async fn dapp_security(&self, params: &Value, client: &Client) -> Result<Value> {
        let url_to_check = params.get("url").and_then(|v| v.as_str()).unwrap_or("");

        let api_url = format!(
            "https://api.gopluslabs.io/api/v1/dapp_security?url={}",
            urlencoding::encode(url_to_check)
        );

        // GoPlus dApp security might not be available for all URLs
        // Fall back to phishing site detection
        let phishing_url = format!(
            "https://api.gopluslabs.io/api/v1/phishing_site?url={}",
            urlencoding::encode(url_to_check)
        );

        let dapp_result = Self::goplus_get(&api_url, client).await;
        let phishing_result = Self::goplus_get(&phishing_url, client).await;

        let is_phishing = phishing_result.as_ref().ok()
            .and_then(|v| v.get("phishing_site").and_then(|p| p.as_i64()))
            .map(|v| v == 1)
            .unwrap_or(false);

        Ok(json!({
            "action": "dapp_security",
            "url": url_to_check,
            "is_phishing": is_phishing,
            "dapp_data": dapp_result.ok(),
            "phishing_data": phishing_result.ok(),
            "risk_level": if is_phishing { "DANGEROUS" } else { "UNKNOWN" },
            "source": "goplus"
        }))
    }

    // ─── Rug Pull Detection ───

    async fn rugpull_detection(&self, params: &Value, client: &Client) -> Result<Value> {
        let chain = params.get("chain").and_then(|v| v.as_str()).unwrap_or("ethereum");
        let chain_id = goplus_chain_id(chain);
        let address = Self::get_addresses(params);

        // Use token_security + additional heuristics
        let url = format!(
            "https://api.gopluslabs.io/api/v1/token_security/{}?contract_addresses={}",
            chain_id, address
        );

        let result = Self::goplus_get(&url, client).await?;

        let mut analysis = Vec::new();
        if let Some(obj) = result.as_object() {
            for (addr, data) in obj {
                let mut rug_indicators = Vec::new();
                let mut score: u32 = 0;

                // High-risk indicators
                if data.get("is_honeypot").and_then(|v| v.as_str()) == Some("1") {
                    rug_indicators.push("Honeypot detected");
                    score += 30;
                }
                if data.get("owner_change_balance").and_then(|v| v.as_str()) == Some("1") {
                    rug_indicators.push("Owner can modify balances");
                    score += 25;
                }
                if data.get("hidden_owner").and_then(|v| v.as_str()) == Some("1") {
                    rug_indicators.push("Hidden owner");
                    score += 20;
                }
                if data.get("can_take_back_ownership").and_then(|v| v.as_str()) == Some("1") {
                    rug_indicators.push("Can reclaim ownership");
                    score += 20;
                }
                if data.get("selfdestruct").and_then(|v| v.as_str()) == Some("1") {
                    rug_indicators.push("Self-destruct capability");
                    score += 15;
                }
                if data.get("cannot_sell_all").and_then(|v| v.as_str()) == Some("1") {
                    rug_indicators.push("Cannot sell all tokens");
                    score += 25;
                }

                // Medium-risk indicators
                if data.get("is_open_source").and_then(|v| v.as_str()) == Some("0") {
                    rug_indicators.push("Not open source");
                    score += 10;
                }
                if data.get("is_proxy").and_then(|v| v.as_str()) == Some("1") {
                    rug_indicators.push("Proxy contract (upgradeable)");
                    score += 10;
                }
                if data.get("is_mintable").and_then(|v| v.as_str()) == Some("1") {
                    rug_indicators.push("Mintable");
                    score += 10;
                }

                // Tax analysis
                let sell_tax = data.get("sell_tax").and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                if sell_tax > 0.5 {
                    rug_indicators.push("Extremely high sell tax (>50%)");
                    score += 25;
                } else if sell_tax > 0.1 {
                    rug_indicators.push("High sell tax (>10%)");
                    score += 10;
                }

                // LP analysis
                let lp_holders = data.get("lp_holder_count").and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                if lp_holders <= 1 {
                    rug_indicators.push("Single LP holder (high rug risk)");
                    score += 20;
                }

                let holders = data.get("holder_count").and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                if holders < 10 {
                    rug_indicators.push("Very few holders (<10)");
                    score += 10;
                }

                let rug_risk = if score >= 50 { "CRITICAL" }
                    else if score >= 30 { "HIGH" }
                    else if score >= 15 { "MEDIUM" }
                    else { "LOW" };

                analysis.push(json!({
                    "address": addr,
                    "name": data.get("token_name"),
                    "symbol": data.get("token_symbol"),
                    "rug_risk": rug_risk,
                    "rug_score": score,
                    "indicators": rug_indicators,
                    "holder_count": holders,
                    "lp_holder_count": lp_holders,
                    "sell_tax": format!("{:.1}%", sell_tax * 100.0),
                    "recommendation": match rug_risk {
                        "CRITICAL" => "DO NOT INVEST. Multiple critical rug pull indicators detected.",
                        "HIGH" => "EXTREME CAUTION. High probability of rug pull.",
                        "MEDIUM" => "CAUTION. Some risk indicators present. DYOR.",
                        _ => "Lower risk, but always DYOR.",
                    },
                }));
            }
        }

        Ok(json!({
            "action": "rugpull_detection",
            "chain": chain,
            "count": analysis.len(),
            "tokens": analysis,
            "source": "goplus"
        }))
    }

    // ─── Contract Source ───

    async fn contract_source(&self, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let chain = params.get("chain").and_then(|v| v.as_str()).unwrap_or("ethereum");
        let address = Self::get_addresses(params);

        // Determine Etherscan-compatible API
        let (api_base, api_key_env) = match chain {
            "ethereum" | "eth" => ("https://api.etherscan.io/api", "ETHERSCAN_API_KEY"),
            "bsc" | "bnb" => ("https://api.bscscan.com/api", "BSCSCAN_API_KEY"),
            "polygon" | "matic" => ("https://api.polygonscan.com/api", "POLYGONSCAN_API_KEY"),
            "arbitrum" | "arb" => ("https://api.arbiscan.io/api", "ARBISCAN_API_KEY"),
            "optimism" | "op" => ("https://api-optimistic.etherscan.io/api", "OPTIMISM_API_KEY"),
            "base" => ("https://api.basescan.org/api", "BASESCAN_API_KEY"),
            "avalanche" | "avax" => ("https://api.snowtrace.io/api", "SNOWTRACE_API_KEY"),
            _ => ("https://api.etherscan.io/api", "ETHERSCAN_API_KEY"),
        };

        let api_key = ctx.config.providers.get("etherscan")
            .map(|p| p.api_key.clone())
            .filter(|k| !k.is_empty())
            .or_else(|| std::env::var(api_key_env).ok())
            .unwrap_or_default();

        let url = format!(
            "{}?module=contract&action=getsourcecode&address={}&apikey={}",
            api_base, address, api_key
        );

        let resp = client.get(&url)
            .timeout(std::time::Duration::from_secs(15))
            .send().await
            .map_err(|e| Error::Tool(format!("Etherscan request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse Etherscan response: {}", e)))?;

        let result = body.get("result").and_then(|r| r.as_array())
            .and_then(|a| a.first()).cloned().unwrap_or(json!({}));

        let is_verified = result.get("SourceCode").and_then(|v| v.as_str())
            .map(|s| !s.is_empty()).unwrap_or(false);

        Ok(json!({
            "action": "contract_source",
            "chain": chain,
            "address": address,
            "is_verified": is_verified,
            "contract_name": result.get("ContractName"),
            "compiler_version": result.get("CompilerVersion"),
            "optimization_used": result.get("OptimizationUsed"),
            "license_type": result.get("LicenseType"),
            "proxy": result.get("Proxy"),
            "implementation": result.get("Implementation"),
            "source_available": is_verified,
            "source": "etherscan"
        }))
    }

    fn info(&self) -> Value {
        json!({
            "tool": "contract_security",
            "source": "GoPlus Security API (free, no key needed)",
            "actions": {
                "token_security": "Check token for honeypot, tax, ownership risks, proxy, mint, blacklist",
                "address_security": "Check if address is associated with scams, phishing, sanctions",
                "approval_security": "Check wallet's ERC20 approvals for risky contracts",
                "nft_security": "Check NFT contract for privileged burn, transfer, minting risks",
                "dapp_security": "Check if a URL is a phishing/scam site",
                "rugpull_detection": "Comprehensive rug pull risk analysis with scoring",
                "contract_source": "Check if contract source is verified (Etherscan)",
                "info": "This help message",
            },
            "supported_chains": [
                "ethereum (1)", "bsc (56)", "polygon (137)", "arbitrum (42161)",
                "optimism (10)", "avalanche (43114)", "base (8453)", "fantom (250)",
                "cronos (25)", "gnosis (100)", "linea (59144)", "zksync (324)",
                "solana", "tron"
            ],
            "risk_levels": {
                "token_security": "HIGH / MEDIUM / LOW based on honeypot, tax, ownership analysis",
                "rugpull_detection": "CRITICAL / HIGH / MEDIUM / LOW with numeric score (0-100+)",
                "address_security": "DANGEROUS / SAFE",
            }
        })
    }
}

// We need urlencoding for dapp_security URL parameter
mod urlencoding {
    pub fn encode(input: &str) -> String {
        let mut result = String::with_capacity(input.len() * 3);
        for byte in input.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    result.push(byte as char);
                }
                _ => {
                    result.push_str(&format!("%{:02X}", byte));
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = ContractSecurityTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "contract_security");
        assert!(schema.description.contains("GoPlus"));
        assert!(schema.description.contains("honeypot"));
    }

    #[test]
    fn test_validate_token_security() {
        let tool = ContractSecurityTool;
        assert!(tool.validate(&json!({"action": "token_security", "address": "0x1234"})).is_ok());
        assert!(tool.validate(&json!({"action": "token_security"})).is_err());
    }

    #[test]
    fn test_validate_address_security() {
        let tool = ContractSecurityTool;
        assert!(tool.validate(&json!({"action": "address_security", "address": "0xabc"})).is_ok());
        assert!(tool.validate(&json!({"action": "address_security", "addresses": ["0xa", "0xb"]})).is_ok());
        assert!(tool.validate(&json!({"action": "address_security"})).is_err());
    }

    #[test]
    fn test_validate_approval_security() {
        let tool = ContractSecurityTool;
        assert!(tool.validate(&json!({"action": "approval_security", "wallet": "0xwallet"})).is_ok());
        assert!(tool.validate(&json!({"action": "approval_security"})).is_err());
    }

    #[test]
    fn test_validate_dapp_security() {
        let tool = ContractSecurityTool;
        assert!(tool.validate(&json!({"action": "dapp_security", "url": "https://example.com"})).is_ok());
        assert!(tool.validate(&json!({"action": "dapp_security"})).is_err());
    }

    #[test]
    fn test_validate_info() {
        let tool = ContractSecurityTool;
        assert!(tool.validate(&json!({"action": "info"})).is_ok());
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }

    #[test]
    fn test_goplus_chain_id() {
        assert_eq!(goplus_chain_id("ethereum"), "1");
        assert_eq!(goplus_chain_id("bsc"), "56");
        assert_eq!(goplus_chain_id("polygon"), "137");
        assert_eq!(goplus_chain_id("arbitrum"), "42161");
        assert_eq!(goplus_chain_id("base"), "8453");
        assert_eq!(goplus_chain_id("solana"), "solana");
    }

    #[test]
    fn test_info() {
        let tool = ContractSecurityTool;
        let info = tool.info();
        assert_eq!(info["tool"], "contract_security");
        assert!(info["supported_chains"].as_array().unwrap().len() >= 10);
    }

    #[test]
    fn test_urlencoding() {
        assert_eq!(urlencoding::encode("https://example.com/path?q=1&b=2"),
            "https%3A%2F%2Fexample.com%2Fpath%3Fq%3D1%26b%3D2");
        assert_eq!(urlencoding::encode("hello"), "hello");
    }

    #[test]
    fn test_get_addresses() {
        let params = json!({"address": "0xABC"});
        assert_eq!(ContractSecurityTool::get_addresses(&params), "0xabc");

        let params = json!({"addresses": ["0xA", "0xB"]});
        assert_eq!(ContractSecurityTool::get_addresses(&params), "0xa,0xb");
    }
}
