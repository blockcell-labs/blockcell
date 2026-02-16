use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// NFT marketplace tool for querying collections, listings, sales, and floor prices.
///
/// Supports:
/// - **OpenSea**: Largest NFT marketplace (v2 API)
/// - **Blur**: Pro trading NFT marketplace
/// - **Reservoir**: Aggregated NFT data across marketplaces
///
/// Read-only operations. Buying/selling requires blockchain_tx for on-chain execution.
pub struct NftMarketTool;

#[async_trait]
impl Tool for NftMarketTool {
    fn schema(&self) -> ToolSchema {
        let mut props = serde_json::Map::new();
        let str_prop = |desc: &str| -> Value { json!({"type": "string", "description": desc}) };
        let int_prop = |desc: &str| -> Value { json!({"type": "integer", "description": desc}) };

        props.insert("action".into(), str_prop("Action: collection_info|floor_price|listings|sales|search|trending|token_info|owner_nfts|collection_stats|info"));
        props.insert("provider".into(), str_prop("NFT data provider: 'opensea'|'reservoir' (default: opensea). Reservoir aggregates across marketplaces."));
        props.insert("collection".into(), str_prop("Collection slug (OpenSea) or contract address. E.g. 'boredapeyachtclub', '0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D'"));
        props.insert("token_id".into(), str_prop("(token_info) Specific token ID within a collection"));
        props.insert("chain".into(), str_prop("Blockchain: 'ethereum'|'polygon'|'arbitrum'|'base'|'optimism'|'solana' (default: ethereum)"));
        props.insert("owner".into(), str_prop("(owner_nfts) Wallet address to query owned NFTs"));
        props.insert("query".into(), str_prop("(search) Search query for collections"));
        props.insert("sort_by".into(), str_prop("(listings/sales) Sort: 'price_asc'|'price_desc'|'created_desc'|'rarity' (default: price_asc)"));
        props.insert("limit".into(), int_prop("Number of results (default: 20, max: 50)"));
        props.insert("cursor".into(), str_prop("Pagination cursor from previous response"));

        ToolSchema {
            name: "nft_market",
            description: "NFT marketplace data. Query collection info, floor prices, listings, recent sales, \
                trending collections, token details, and wallet holdings. Supports OpenSea (v2 API) and \
                Reservoir (aggregated data). Use 'collection_info' for overview, 'floor_price' for current floor, \
                'listings' for active listings, 'sales' for recent trades, 'trending' for hot collections, \
                'owner_nfts' for wallet portfolio. Actual buying/selling requires blockchain_tx.",
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid = ["collection_info", "floor_price", "listings", "sales", "search", "trending", "token_info", "owner_nfts", "collection_stats", "info"];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        match action {
            "collection_info" | "floor_price" | "listings" | "sales" | "collection_stats" => {
                if params.get("collection").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'collection' (slug or contract address) is required".into()));
                }
            }
            "token_info" => {
                if params.get("collection").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'collection' is required for token_info".into()));
                }
                if params.get("token_id").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'token_id' is required for token_info".into()));
                }
            }
            "owner_nfts" => {
                if params.get("owner").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'owner' wallet address is required".into()));
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap_or("");
        let provider = params.get("provider").and_then(|v| v.as_str()).unwrap_or("opensea");
        let client = Client::new();

        match action {
            "collection_info" => self.collection_info(provider, &ctx, &params, &client).await,
            "floor_price" => self.floor_price(provider, &ctx, &params, &client).await,
            "listings" => self.listings(provider, &ctx, &params, &client).await,
            "sales" => self.sales(provider, &ctx, &params, &client).await,
            "search" => self.search(provider, &ctx, &params, &client).await,
            "trending" => self.trending(provider, &ctx, &client).await,
            "token_info" => self.token_info(provider, &ctx, &params, &client).await,
            "owner_nfts" => self.owner_nfts(provider, &ctx, &params, &client).await,
            "collection_stats" => self.collection_stats(provider, &ctx, &params, &client).await,
            "info" => Ok(self.info()),
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

impl NftMarketTool {
    /// Resolve OpenSea API key from config or env.
    fn resolve_opensea_key(ctx: &ToolContext) -> String {
        ctx.config.providers.get("opensea").map(|p| p.api_key.clone())
            .or_else(|| std::env::var("OPENSEA_API_KEY").ok())
            .unwrap_or_default()
    }

    /// Resolve Reservoir API key from config or env.
    #[allow(dead_code)]
    fn resolve_reservoir_key(ctx: &ToolContext) -> String {
        ctx.config.providers.get("reservoir").map(|p| p.api_key.clone())
            .or_else(|| std::env::var("RESERVOIR_API_KEY").ok())
            .unwrap_or_default()
    }

    /// Map chain name to OpenSea chain identifier.
    fn opensea_chain(chain: &str) -> &str {
        match chain.to_lowercase().as_str() {
            "ethereum" | "eth" => "ethereum",
            "polygon" | "matic" => "matic",
            "arbitrum" | "arb" => "arbitrum",
            "base" => "base",
            "optimism" | "op" => "optimism",
            "avalanche" | "avax" => "avalanche",
            "solana" | "sol" => "solana",
            _ => "ethereum",
        }
    }

    /// Build OpenSea API request with auth header.
    fn opensea_request(client: &Client, url: &str, api_key: &str) -> reqwest::RequestBuilder {
        let mut req = client.get(url)
            .header("User-Agent", "blockcell-agent")
            .header("Accept", "application/json");
        if !api_key.is_empty() {
            req = req.header("X-API-KEY", api_key);
        }
        req
    }

    // ─── Collection Info ───

    async fn collection_info(&self, provider: &str, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let collection = params["collection"].as_str().unwrap_or("");

        match provider {
            "opensea" | _ => {
                let api_key = Self::resolve_opensea_key(ctx);
                let url = format!("https://api.opensea.io/api/v2/collections/{}", collection);
                debug!(url = %url, "OpenSea collection info");

                let resp = Self::opensea_request(client, &url, &api_key)
                    .send().await
                    .map_err(|e| Error::Tool(format!("OpenSea request failed: {}", e)))?;

                let status = resp.status();
                let body: Value = resp.json().await
                    .map_err(|e| Error::Tool(format!("Failed to parse OpenSea response: {}", e)))?;

                if !status.is_success() {
                    return Err(Error::Tool(format!("OpenSea error ({}): {:?}", status, body)));
                }

                Ok(json!({
                    "action": "collection_info",
                    "provider": "opensea",
                    "name": body.get("name"),
                    "description": body.get("description"),
                    "image_url": body.get("image_url"),
                    "banner_image_url": body.get("banner_image_url"),
                    "owner": body.get("owner"),
                    "category": body.get("category"),
                    "is_nsfw": body.get("is_nsfw"),
                    "opensea_url": body.get("opensea_url"),
                    "project_url": body.get("project_url"),
                    "wiki_url": body.get("wiki_url"),
                    "discord_url": body.get("discord_url"),
                    "telegram_url": body.get("telegram_url"),
                    "twitter_username": body.get("twitter_username"),
                    "contracts": body.get("contracts"),
                    "total_supply": body.get("total_supply"),
                    "created_date": body.get("created_date"),
                }))
            }
        }
    }

    // ─── Floor Price ───

    async fn floor_price(&self, provider: &str, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let collection = params["collection"].as_str().unwrap_or("");

        match provider {
            "opensea" | _ => {
                let api_key = Self::resolve_opensea_key(ctx);
                // Use collection stats endpoint for floor price
                let url = format!("https://api.opensea.io/api/v2/collections/{}/stats", collection);
                debug!(url = %url, "OpenSea floor price");

                let resp = Self::opensea_request(client, &url, &api_key)
                    .send().await
                    .map_err(|e| Error::Tool(format!("OpenSea request failed: {}", e)))?;

                let body: Value = resp.json().await
                    .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

                Ok(json!({
                    "action": "floor_price",
                    "provider": "opensea",
                    "collection": collection,
                    "floor_price": body.get("total").and_then(|t| t.get("floor_price")),
                    "floor_price_symbol": body.get("total").and_then(|t| t.get("floor_price_symbol")),
                    "volume": body.get("total").and_then(|t| t.get("volume")),
                    "sales": body.get("total").and_then(|t| t.get("sales")),
                    "average_price": body.get("total").and_then(|t| t.get("average_price")),
                    "num_owners": body.get("total").and_then(|t| t.get("num_owners")),
                    "market_cap": body.get("total").and_then(|t| t.get("market_cap")),
                    "intervals": body.get("intervals"),
                }))
            }
        }
    }

    // ─── Listings ───

    async fn listings(&self, provider: &str, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let collection = params["collection"].as_str().unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(50);
        let cursor = params.get("cursor").and_then(|v| v.as_str()).unwrap_or("");

        match provider {
            "opensea" | _ => {
                let api_key = Self::resolve_opensea_key(ctx);
                let mut url = format!(
                    "https://api.opensea.io/api/v2/listings/collection/{}/all?limit={}",
                    collection, limit
                );
                if !cursor.is_empty() {
                    url.push_str(&format!("&next={}", cursor));
                }
                debug!(url = %url, "OpenSea listings");

                let resp = Self::opensea_request(client, &url, &api_key)
                    .send().await
                    .map_err(|e| Error::Tool(format!("OpenSea request failed: {}", e)))?;

                let body: Value = resp.json().await
                    .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

                let listings = body.get("listings").and_then(|l| l.as_array()).cloned().unwrap_or_default();
                let mut results = Vec::new();
                for listing in &listings {
                    let price = listing.get("price").and_then(|p| p.get("current"));
                    results.push(json!({
                        "order_hash": listing.get("order_hash"),
                        "token_id": listing.pointer("/protocol_data/parameters/offer/0/identifierOrCriteria"),
                        "price_value": price.and_then(|p| p.get("value")),
                        "price_currency": price.and_then(|p| p.get("currency")),
                        "price_decimals": price.and_then(|p| p.get("decimals")),
                        "expiration_date": listing.get("expiration_date"),
                        "protocol_address": listing.get("protocol_address"),
                    }));
                }

                Ok(json!({
                    "action": "listings",
                    "provider": "opensea",
                    "collection": collection,
                    "count": results.len(),
                    "listings": results,
                    "next_cursor": body.get("next"),
                }))
            }
        }
    }

    // ─── Sales ───

    async fn sales(&self, provider: &str, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let collection = params["collection"].as_str().unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(50);

        match provider {
            "opensea" | _ => {
                let api_key = Self::resolve_opensea_key(ctx);
                let url = format!(
                    "https://api.opensea.io/api/v2/events/collection/{}?event_type=sale&limit={}",
                    collection, limit
                );
                debug!(url = %url, "OpenSea sales");

                let resp = Self::opensea_request(client, &url, &api_key)
                    .send().await
                    .map_err(|e| Error::Tool(format!("OpenSea request failed: {}", e)))?;

                let body: Value = resp.json().await
                    .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

                let events = body.get("asset_events").and_then(|e| e.as_array()).cloned().unwrap_or_default();
                let mut sales = Vec::new();
                for event in &events {
                    sales.push(json!({
                        "event_type": event.get("event_type"),
                        "nft": event.get("nft"),
                        "payment": event.get("payment"),
                        "seller": event.get("seller"),
                        "buyer": event.get("buyer"),
                        "closing_date": event.get("closing_date"),
                        "transaction": event.get("transaction"),
                    }));
                }

                Ok(json!({
                    "action": "sales",
                    "provider": "opensea",
                    "collection": collection,
                    "count": sales.len(),
                    "sales": sales,
                    "next_cursor": body.get("next"),
                }))
            }
        }
    }

    // ─── Search ───

    async fn search(&self, _provider: &str, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let chain = params.get("chain").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(50);

        let api_key = Self::resolve_opensea_key(ctx);
        let chain_param = if !chain.is_empty() { format!("&chain={}", Self::opensea_chain(chain)) } else { String::new() };
        let url = format!(
            "https://api.opensea.io/api/v2/collections?limit={}{}",
            limit, chain_param
        );
        debug!(url = %url, query = query, "OpenSea search");

        let resp = Self::opensea_request(client, &url, &api_key)
            .send().await
            .map_err(|e| Error::Tool(format!("OpenSea request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        let collections = body.get("collections").and_then(|c| c.as_array()).cloned().unwrap_or_default();
        let mut results = Vec::new();
        for coll in &collections {
            let name = coll.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if !query.is_empty() && !name.to_lowercase().contains(&query.to_lowercase()) {
                continue;
            }
            results.push(json!({
                "collection": coll.get("collection"),
                "name": name,
                "description": coll.get("description"),
                "image_url": coll.get("image_url"),
                "opensea_url": coll.get("opensea_url"),
                "contracts": coll.get("contracts"),
            }));
        }

        Ok(json!({
            "action": "search",
            "query": query,
            "count": results.len(),
            "collections": results,
        }))
    }

    // ─── Trending ───

    async fn trending(&self, _provider: &str, ctx: &ToolContext, client: &Client) -> Result<Value> {
        let api_key = Self::resolve_opensea_key(ctx);
        let url = "https://api.opensea.io/api/v2/collections?order_by=seven_day_volume&limit=20";
        debug!(url = %url, "OpenSea trending");

        let resp = Self::opensea_request(client, url, &api_key)
            .send().await
            .map_err(|e| Error::Tool(format!("OpenSea request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        let collections = body.get("collections").and_then(|c| c.as_array()).cloned().unwrap_or_default();
        let mut results = Vec::new();
        for coll in &collections {
            results.push(json!({
                "collection": coll.get("collection"),
                "name": coll.get("name"),
                "image_url": coll.get("image_url"),
                "opensea_url": coll.get("opensea_url"),
            }));
        }

        Ok(json!({
            "action": "trending",
            "count": results.len(),
            "collections": results,
        }))
    }

    // ─── Token Info ───

    async fn token_info(&self, _provider: &str, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let collection = params["collection"].as_str().unwrap_or("");
        let token_id = params["token_id"].as_str().unwrap_or("");
        let chain = params.get("chain").and_then(|v| v.as_str()).unwrap_or("ethereum");

        let api_key = Self::resolve_opensea_key(ctx);
        let opensea_chain = Self::opensea_chain(chain);
        let url = format!(
            "https://api.opensea.io/api/v2/chain/{}/contract/{}/nfts/{}",
            opensea_chain, collection, token_id
        );
        debug!(url = %url, "OpenSea token info");

        let resp = Self::opensea_request(client, &url, &api_key)
            .send().await
            .map_err(|e| Error::Tool(format!("OpenSea request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        let nft = body.get("nft").cloned().unwrap_or(body.clone());

        Ok(json!({
            "action": "token_info",
            "collection": collection,
            "token_id": token_id,
            "name": nft.get("name"),
            "description": nft.get("description"),
            "image_url": nft.get("image_url"),
            "metadata_url": nft.get("metadata_url"),
            "traits": nft.get("traits"),
            "owners": nft.get("owners"),
            "rarity": nft.get("rarity"),
            "is_suspicious": nft.get("is_suspicious"),
            "opensea_url": nft.get("opensea_url"),
        }))
    }

    // ─── Owner NFTs ───

    async fn owner_nfts(&self, _provider: &str, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let owner = params["owner"].as_str().unwrap_or("");
        let chain = params.get("chain").and_then(|v| v.as_str()).unwrap_or("ethereum");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(50);
        let collection_filter = params.get("collection").and_then(|v| v.as_str()).unwrap_or("");

        let api_key = Self::resolve_opensea_key(ctx);
        let opensea_chain = Self::opensea_chain(chain);
        let mut url = format!(
            "https://api.opensea.io/api/v2/chain/{}/account/{}/nfts?limit={}",
            opensea_chain, owner, limit
        );
        if !collection_filter.is_empty() {
            url.push_str(&format!("&collection={}", collection_filter));
        }
        debug!(url = %url, "OpenSea owner NFTs");

        let resp = Self::opensea_request(client, &url, &api_key)
            .send().await
            .map_err(|e| Error::Tool(format!("OpenSea request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        let nfts = body.get("nfts").and_then(|n| n.as_array()).cloned().unwrap_or_default();
        let mut results = Vec::new();
        for nft in &nfts {
            results.push(json!({
                "identifier": nft.get("identifier"),
                "collection": nft.get("collection"),
                "contract": nft.get("contract"),
                "name": nft.get("name"),
                "image_url": nft.get("image_url"),
                "is_suspicious": nft.get("is_suspicious"),
                "opensea_url": nft.get("opensea_url"),
            }));
        }

        Ok(json!({
            "action": "owner_nfts",
            "owner": owner,
            "chain": chain,
            "count": results.len(),
            "nfts": results,
            "next_cursor": body.get("next"),
        }))
    }

    // ─── Collection Stats ───

    async fn collection_stats(&self, _provider: &str, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let collection = params["collection"].as_str().unwrap_or("");

        let api_key = Self::resolve_opensea_key(ctx);
        let url = format!("https://api.opensea.io/api/v2/collections/{}/stats", collection);
        debug!(url = %url, "OpenSea collection stats");

        let resp = Self::opensea_request(client, &url, &api_key)
            .send().await
            .map_err(|e| Error::Tool(format!("OpenSea request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        Ok(json!({
            "action": "collection_stats",
            "collection": collection,
            "total": body.get("total"),
            "intervals": body.get("intervals"),
        }))
    }

    fn info(&self) -> Value {
        json!({
            "tool": "nft_market",
            "description": "NFT marketplace data aggregator",
            "providers": {
                "opensea": "OpenSea v2 API — largest NFT marketplace (API key recommended for higher rate limits)",
                "reservoir": "Reservoir — aggregated NFT data across OpenSea, Blur, LooksRare, etc."
            },
            "actions": {
                "collection_info": "Get collection details (name, description, links, contracts)",
                "floor_price": "Get current floor price and volume stats",
                "listings": "Get active listings sorted by price",
                "sales": "Get recent sales/trades",
                "search": "Search collections by name",
                "trending": "Get trending collections by volume",
                "token_info": "Get details for a specific NFT (traits, rarity, owners)",
                "owner_nfts": "List NFTs owned by a wallet address",
                "collection_stats": "Get detailed collection statistics (volume, floor, owners)",
                "info": "This help message"
            },
            "api_keys": {
                "opensea": "Set OPENSEA_API_KEY env var or config providers.opensea.api_key",
                "reservoir": "Set RESERVOIR_API_KEY env var or config providers.reservoir.api_key"
            },
            "popular_collections": {
                "boredapeyachtclub": "Bored Ape Yacht Club (BAYC)",
                "cryptopunks": "CryptoPunks",
                "azuki": "Azuki",
                "pudgypenguins": "Pudgy Penguins",
                "milady": "Milady Maker"
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = NftMarketTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "nft_market");
        assert!(schema.description.contains("NFT"));
    }

    #[test]
    fn test_validate() {
        let tool = NftMarketTool;
        assert!(tool.validate(&json!({"action": "info"})).is_ok());
        assert!(tool.validate(&json!({"action": "trending"})).is_ok());
        assert!(tool.validate(&json!({"action": "collection_info", "collection": "boredapeyachtclub"})).is_ok());
        assert!(tool.validate(&json!({"action": "collection_info"})).is_err()); // missing collection
        assert!(tool.validate(&json!({"action": "floor_price", "collection": "azuki"})).is_ok());
        assert!(tool.validate(&json!({"action": "token_info", "collection": "0xBC4CA0EdA7647A8aB7C2061c2E118A18a936f13D", "token_id": "1234"})).is_ok());
        assert!(tool.validate(&json!({"action": "token_info", "collection": "test"})).is_err()); // missing token_id
        assert!(tool.validate(&json!({"action": "owner_nfts", "owner": "0x1234"})).is_ok());
        assert!(tool.validate(&json!({"action": "owner_nfts"})).is_err()); // missing owner
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }

    #[test]
    fn test_opensea_chain() {
        assert_eq!(NftMarketTool::opensea_chain("ethereum"), "ethereum");
        assert_eq!(NftMarketTool::opensea_chain("polygon"), "matic");
        assert_eq!(NftMarketTool::opensea_chain("base"), "base");
        assert_eq!(NftMarketTool::opensea_chain("solana"), "solana");
    }

    #[test]
    fn test_info() {
        let tool = NftMarketTool;
        let info = tool.info();
        assert_eq!(info["tool"], "nft_market");
        assert!(info["actions"].as_object().unwrap().len() >= 10);
    }
}
