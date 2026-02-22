use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// Financial data tool for stocks, cryptocurrency, and forex.
///
/// Data sources:
/// - **Stocks (A股/港股)**: 东方财富 API (free, no key required, real-time)
/// - **Stocks (US/global)**: Alpha Vantage API, Yahoo Finance (via unofficial endpoints)
/// - **Crypto**: CoinGecko API (free, no key required for basic usage)
/// - **Forex**: Exchange rate APIs
/// - **Portfolio**: Local calculation based on holdings
pub struct FinanceApiTool;

#[async_trait]
impl Tool for FinanceApiTool {
    fn schema(&self) -> ToolSchema {
        let str_prop = |desc: &str| -> Value { json!({"type": "string", "description": desc}) };
        let _num_prop = |desc: &str| -> Value { json!({"type": "number", "description": desc}) };
        let int_prop = |desc: &str| -> Value { json!({"type": "integer", "description": desc}) };
        let arr_prop = |desc: &str| -> Value { json!({"type": "array", "description": desc}) };

        let mut props = serde_json::Map::new();
        props.insert("action".into(), str_prop("Action: stock_quote|stock_history|stock_search|stock_screen|financial_statement|dividend_history|top_list|bond_yield|bond_info|convertible_bond|futures_position|futures_contract_info|institutional_holdings|analyst_ratings|crypto_price|crypto_history|crypto_list|forex_rate|forex_history|portfolio_value|market_overview"));
        props.insert("symbol".into(), str_prop("Stock ticker (e.g. 'AAPL', 'MSFT', '601318', '000001', '601318.SH', '00700.HK') or crypto ID (e.g. 'bitcoin', 'ethereum')"));
        props.insert("symbols".into(), json!({"type": "array", "items": {"type": "string"}, "description": "Multiple symbols for batch queries"}));
        props.insert("from_currency".into(), str_prop("Source currency code (e.g. 'USD', 'BTC')"));
        props.insert("to_currency".into(), str_prop("Target currency code (e.g. 'CNY', 'EUR')"));
        props.insert("interval".into(), str_prop("Time interval: '1d'|'5d'|'1mo'|'3mo'|'6mo'|'1y'|'5y'|'max' (stock_history) or '1h'|'24h'|'7d'|'30d'|'90d'|'1y' (crypto_history)"));
        props.insert("start_date".into(), str_prop("Start date (YYYY-MM-DD) for history queries"));
        props.insert("end_date".into(), str_prop("End date (YYYY-MM-DD) for history queries"));
        props.insert("holdings".into(), arr_prop("(portfolio_value) Array of {symbol, quantity, cost_basis} objects"));
        props.insert("vs_currency".into(), str_prop("(crypto) Quote currency: 'usd', 'eur', 'cny', etc. Default: 'usd'"));
        props.insert("category".into(), str_prop("(crypto_list) Category filter: 'defi', 'nft', 'layer-1', etc."));
        props.insert("limit".into(), int_prop("Number of results (default: 20)"));
        props.insert("api_key".into(), str_prop("Alpha Vantage API key (for stock data). Overrides config/env."));
        props.insert("source".into(), str_prop("Data source preference: 'eastmoney'|'alpha_vantage'|'yahoo'|'auto' (default: auto). Chinese stocks (A股/港股) auto-use eastmoney."));
        props.insert("screen_filters".into(), json!({"type": "object", "description": "(stock_screen) Screening filters. Keys: industry (行业, e.g. '银行'), pe_max, pe_min, pb_max, pb_min, market_cap_min (亿), market_cap_max (亿), change_pct_min, change_pct_max, dividend_yield_min, price_min, price_max, market ('sh'|'sz'|'bj'|'all'), board ('主板'|'创业板'|'科创板'|'北交所'|'all')"}));
        props.insert("report_type".into(), str_prop("(financial_statement) Report type: 'income'|'balance'|'cashflow'|'indicator' (default: indicator). indicator=核心指标(ROE/毛利率/净利率等)"));
        props.insert("years".into(), int_prop("(financial_statement/dividend_history) Number of years of data (default: 3)"));
        props.insert("list_type".into(), str_prop("(top_list) List type: 'gainers'|'losers'|'volume'|'turnover'|'money_flow'|'north_flow'|'dragon_tiger'|'limit_up'|'limit_down' (default: gainers)"));
        props.insert("market_filter".into(), str_prop("(top_list/stock_screen) Market filter: 'sh'|'sz'|'bj'|'all' (default: all)"));
        props.insert("bond_type".into(), str_prop("(bond_yield) Bond type: 'china_treasury'|'us_treasury'|'china_corporate' (default: china_treasury). (bond_info) Bond code e.g. '019666'. (convertible_bond) Filter: 'all'|'in_progress'|'upcoming'"));
        props.insert("term".into(), str_prop("(bond_yield) Term: '1y'|'2y'|'3y'|'5y'|'7y'|'10y'|'30y'|'all' (default: all). Returns yield curve data."));
        props.insert("bond_code".into(), str_prop("(bond_info) Specific bond code, e.g. '019666', '127045'. (convertible_bond) Convertible bond code."));
        props.insert("futures_exchange".into(), str_prop("(futures_position/futures_contract_info) Exchange: 'shfe'|'dce'|'czce'|'cffex'|'ine'|'gfex'|'all' (default: all)"));
        props.insert("futures_symbol".into(), str_prop("(futures_position/futures_contract_info) Futures symbol, e.g. 'rb2505' (螺纹钢), 'au2506' (黄金), 'IF2503' (沪深300股指)"));

        ToolSchema {
            name: "finance_api",
            description: "Query financial market data. Chinese stocks (A股/港股) use 东方财富 API (free, real-time). \
                NEW: bond_yield (国债收益率曲线, 中美国债), bond_info (债券详情: 票面利率/到期日/信用评级), \
                convertible_bond (可转债列表: 转股价/溢价率/到期收益率), futures_position (期货持仓量/多空比), \
                futures_contract_info (期货合约规格: 保证金/交割日/涨跌停), institutional_holdings (机构持仓变化: 基金/QFII/社保), \
                analyst_ratings (券商研报评级: 目标价/评级变化). \
                Also: stock_quote, stock_history, stock_search, stock_screen, financial_statement, dividend_history, top_list, \
                crypto_price/history/list, forex_rate/history, portfolio_value, market_overview.",
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
            "stock_quote", "stock_history", "stock_search",
            "stock_screen", "financial_statement", "dividend_history", "top_list",
            "bond_yield", "bond_info", "convertible_bond",
            "futures_position", "futures_contract_info",
            "institutional_holdings", "analyst_ratings",
            "crypto_price", "crypto_history", "crypto_list",
            "forex_rate", "forex_history",
            "portfolio_value", "market_overview",
        ];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        match action {
            "stock_quote" | "stock_history" | "financial_statement" | "dividend_history"
            | "institutional_holdings" | "analyst_ratings" => {
                if params.get("symbol").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                    && params.get("symbols").and_then(|v| v.as_array()).map(|a| a.is_empty()).unwrap_or(true)
                {
                    return Err(Error::Tool("'symbol' or 'symbols' is required".into()));
                }
            }
            "crypto_price" | "crypto_history" => {
                if params.get("symbol").and_then(|v| v.as_str()).unwrap_or("").is_empty()
                    && params.get("symbols").and_then(|v| v.as_array()).map(|a| a.is_empty()).unwrap_or(true)
                {
                    return Err(Error::Tool("'symbol' (crypto ID like 'bitcoin') is required".into()));
                }
            }
            "forex_rate" | "forex_history" => {
                if params.get("from_currency").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'from_currency' is required".into()));
                }
                if params.get("to_currency").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'to_currency' is required".into()));
                }
            }
            "portfolio_value" => {
                if params.get("holdings").and_then(|v| v.as_array()).map(|a| a.is_empty()).unwrap_or(true) {
                    return Err(Error::Tool("'holdings' array is required".into()));
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
            "stock_quote" => self.stock_quote(&ctx, &params, &client).await,
            "stock_history" => self.stock_history(&ctx, &params, &client).await,
            "stock_search" => self.stock_search(&ctx, &params, &client).await,
            "stock_screen" => self.stock_screen(&params, &client).await,
            "financial_statement" => self.financial_statement(&params, &client).await,
            "dividend_history" => self.dividend_history(&params, &client).await,
            "top_list" => self.top_list(&params, &client).await,
            "crypto_price" => self.crypto_price(&params, &client).await,
            "crypto_history" => self.crypto_history(&params, &client).await,
            "crypto_list" => self.crypto_list(&params, &client).await,
            "forex_rate" => self.forex_rate(&ctx, &params, &client).await,
            "forex_history" => self.forex_history(&ctx, &params, &client).await,
            "portfolio_value" => self.portfolio_value(&ctx, &params, &client).await,
            "bond_yield" => self.bond_yield(&params, &client).await,
            "bond_info" => self.bond_info(&params, &client).await,
            "convertible_bond" => self.convertible_bond(&params, &client).await,
            "futures_position" => self.futures_position(&params, &client).await,
            "futures_contract_info" => self.futures_contract_info(&params, &client).await,
            "institutional_holdings" => self.institutional_holdings(&params, &client).await,
            "analyst_ratings" => self.analyst_ratings(&params, &client).await,
            "market_overview" => self.market_overview(&params, &client).await,
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

impl FinanceApiTool {
    fn resolve_av_key(ctx: &ToolContext, params: &Value) -> String {
        params.get("api_key").and_then(|v| v.as_str()).map(String::from)
            .or_else(|| ctx.config.providers.get("alpha_vantage").map(|p| p.api_key.clone()))
            .or_else(|| std::env::var("ALPHA_VANTAGE_API_KEY").ok())
            .unwrap_or_default()
    }

    // ─── Chinese Stock Helpers (东方财富) ───

    /// Detect if a symbol is a Chinese stock (A股/港股).
    /// Matches: 6-digit codes (600xxx, 000xxx, 300xxx, 002xxx, 688xxx, 003xxx, 001xxx),
    /// codes with .SH/.SZ/.SS suffix, HK codes (0xxxx.HK), or codes with market prefix (sh/sz/hk).
    fn is_chinese_stock(symbol: &str) -> bool {
        let s = symbol.trim().to_uppercase();
        // Explicit suffix: .SH, .SS, .SZ, .HK
        if s.ends_with(".SH") || s.ends_with(".SS") || s.ends_with(".SZ") || s.ends_with(".HK") {
            return true;
        }
        // Prefix: SH, SZ, HK
        if (s.starts_with("SH") || s.starts_with("SZ") || s.starts_with("HK")) && s.len() >= 8 {
            let rest = &s[2..];
            if rest.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
        // Pure 6-digit A股 code
        if s.len() == 6 && s.chars().all(|c| c.is_ascii_digit()) {
            let prefix = &s[..3];
            return matches!(prefix, "600" | "601" | "603" | "605" | "000" | "001" | "002" | "003" | "300" | "688" | "689" | "830" | "831" | "832" | "833" | "834" | "835" | "836" | "837" | "838" | "839" | "870" | "871");
        }
        // Pure 5-digit HK code
        if s.len() == 5 && s.chars().all(|c| c.is_ascii_digit()) {
            return true; // Likely HK stock
        }
        false
    }

    /// Convert a stock symbol to 东方财富 secid format.
    /// Returns (secid, market_code, pure_code).
    /// secid format: "1.601318" (1=沪市), "0.000001" (0=深市), "116.01318" (116=港股)
    fn to_eastmoney_secid(symbol: &str) -> (String, &'static str, String) {
        let s = symbol.trim().to_uppercase();

        // Strip suffix
        let (code, explicit_market) = if s.ends_with(".SH") || s.ends_with(".SS") {
            (s[..s.len()-3].to_string(), Some("sh"))
        } else if s.ends_with(".SZ") {
            (s[..s.len()-3].to_string(), Some("sz"))
        } else if s.ends_with(".HK") {
            (s[..s.len()-3].to_string(), Some("hk"))
        } else if s.starts_with("SH") && s.len() >= 8 && s[2..].chars().all(|c| c.is_ascii_digit()) {
            (s[2..].to_string(), Some("sh"))
        } else if s.starts_with("SZ") && s.len() >= 8 && s[2..].chars().all(|c| c.is_ascii_digit()) {
            (s[2..].to_string(), Some("sz"))
        } else if s.starts_with("HK") && s.len() >= 7 && s[2..].chars().all(|c| c.is_ascii_digit()) {
            (s[2..].to_string(), Some("hk"))
        } else {
            (s.clone(), None)
        };

        if explicit_market == Some("hk") {
            return (format!("116.{}", code), "hk", code);
        }
        if explicit_market == Some("sh") {
            return (format!("1.{}", code), "sh", code);
        }
        if explicit_market == Some("sz") {
            return (format!("0.{}", code), "sz", code);
        }

        // Auto-detect by code prefix
        if code.len() == 6 {
            let prefix = &code[..1];
            let prefix3 = &code[..3];
            match prefix {
                "6" => return (format!("1.{}", code), "sh", code),  // 沪市主板/科创板
                "0" | "3" => return (format!("0.{}", code), "sz", code),  // 深市主板/创业板
                "8" => {
                    // 北交所 or 新三板
                    if prefix3 == "688" || prefix3 == "689" {
                        return (format!("1.{}", code), "sh", code);  // 科创板
                    }
                    return (format!("0.{}", code), "sz", code);  // 北交所
                }
                _ => return (format!("1.{}", code), "sh", code),
            }
        }
        // 5-digit: likely HK
        if code.len() == 5 && code.chars().all(|c| c.is_ascii_digit()) {
            return (format!("116.{}", code), "hk", code);
        }

        (format!("1.{}", code), "sh", code)
    }

    /// Fetch real-time quote from 东方财富 push2 API.
    async fn eastmoney_quote(&self, symbol: &str, client: &Client) -> Result<Value> {
        let (secid, market, code) = Self::to_eastmoney_secid(symbol);
        let url = format!(
            "https://push2.eastmoney.com/api/qt/stock/get?secid={}&fields=f43,f44,f45,f46,f47,f48,f50,f51,f52,f55,f57,f58,f60,f116,f117,f162,f167,f168,f169,f170,f171,f292",
            secid
        );
        debug!(url = %url, secid = %secid, "东方财富 quote");
        let resp = client.get(&url)
            .header("Referer", "https://quote.eastmoney.com")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .send()
            .await
            .map_err(|e| Error::Tool(format!("东方财富 request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse 东方财富 response: {}", e)))?;

        if let Some(data) = body.get("data") {
            if data.is_null() {
                return Err(Error::Tool(format!("东方财富: no data for '{}' (secid={})", symbol, secid)));
            }
            // f43=最新价(分), f44=最高, f45=最低, f46=今开, f47=成交量(手), f48=成交额
            // f50=量比, f51=涨停价, f52=跌停价, f55=收盘价(盘中=最新价)
            // f57=代码, f58=名称, f60=昨收, f116=总市值, f117=流通市值
            // f162=市盈率PE, f167=市净率PB, f168=换手率, f169=涨跌额, f170=涨跌幅, f171=振幅
            // f292=板块(1=沪A, 0=深A, 2=北A, 128=港股)
            let divisor = if market == "hk" { 1000.0 } else { 100.0 };
            let get_price = |field: &str| -> Option<f64> {
                data.get(field).and_then(|v| v.as_f64()).map(|v| v / divisor)
            };
            let get_f64 = |field: &str| -> Option<f64> {
                data.get(field).and_then(|v| v.as_f64())
            };
            let get_str = |field: &str| -> Option<&str> {
                data.get(field).and_then(|v| v.as_str())
            };

            Ok(json!({
                "symbol": code,
                "name": get_str("f58").unwrap_or(""),
                "price": get_price("f43"),
                "change": get_f64("f169").map(|v| v / divisor),
                "change_percent": get_f64("f170").map(|v| v / 100.0),
                "open": get_price("f46"),
                "high": get_price("f44"),
                "low": get_price("f45"),
                "previous_close": get_price("f60"),
                "volume": get_f64("f47"),
                "amount": get_f64("f48"),
                "turnover_rate": get_f64("f168").map(|v| v / 100.0),
                "pe_ratio": get_f64("f162").map(|v| v / 100.0),
                "pb_ratio": get_f64("f167").map(|v| v / 100.0),
                "market_cap": get_f64("f116"),
                "float_market_cap": get_f64("f117"),
                "amplitude": get_f64("f171").map(|v| v / 100.0),
                "limit_up": get_price("f51"),
                "limit_down": get_price("f52"),
                "volume_ratio": get_f64("f50").map(|v| v / 100.0),
                "market": market,
                "currency": if market == "hk" { "HKD" } else { "CNY" },
                "source": "eastmoney"
            }))
        } else {
            Err(Error::Tool(format!("东方财富 API error: {:?}", body)))
        }
    }

    /// Fetch K-line history from 东方财富 push2his API.
    /// klt: 101=日K, 102=周K, 103=月K
    async fn eastmoney_kline(&self, symbol: &str, klt: &str, limit: u64, client: &Client) -> Result<Value> {
        let (secid, market, code) = Self::to_eastmoney_secid(symbol);
        let end_date = chrono::Utc::now().format("%Y%m%d").to_string();
        let url = format!(
            "https://push2his.eastmoney.com/api/qt/stock/kline/get?secid={}&fields1=f1,f2,f3,f4,f5,f6&fields2=f51,f52,f53,f54,f55,f56,f57,f58,f59,f60,f61&klt={}&fqt=1&lmt={}&end={}&ut=fa5fd1943c7b386f172d6893dbfba10b",
            secid, klt, limit, end_date
        );
        debug!(url = %url, "东方财富 kline");
        let resp = client.get(&url)
            .header("Referer", "https://quote.eastmoney.com")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .send()
            .await
            .map_err(|e| Error::Tool(format!("东方财富 kline request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse 东方财富 kline response: {}", e)))?;

        if let Some(data) = body.get("data") {
            if data.is_null() {
                return Err(Error::Tool(format!("东方财富: no kline data for '{}'", symbol)));
            }
            let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("");
            // klines format: ["2025-02-10,67.80,68.19,68.68,67.32,233645,1585432064.00,1.36,-0.97,-0.66,1.07"]
            // fields: date,open,close,high,low,volume,amount,amplitude%,change%,change,turnover%
            let klines = data.get("klines").and_then(|v| v.as_array());
            let mut records = Vec::new();
            if let Some(klines) = klines {
                for kline in klines {
                    if let Some(line) = kline.as_str() {
                        let parts: Vec<&str> = line.split(',').collect();
                        if parts.len() >= 11 {
                            records.push(json!({
                                "date": parts[0],
                                "open": parts[1].parse::<f64>().ok(),
                                "close": parts[2].parse::<f64>().ok(),
                                "high": parts[3].parse::<f64>().ok(),
                                "low": parts[4].parse::<f64>().ok(),
                                "volume": parts[5].parse::<f64>().ok(),
                                "amount": parts[6].parse::<f64>().ok(),
                                "amplitude": parts[7].parse::<f64>().ok(),
                                "change_percent": parts[8].parse::<f64>().ok(),
                                "change": parts[9].parse::<f64>().ok(),
                                "turnover_rate": parts[10].parse::<f64>().ok()
                            }));
                        }
                    }
                }
            }
            Ok(json!({
                "symbol": code,
                "name": name,
                "market": market,
                "kline_type": match klt { "101" => "daily", "102" => "weekly", "103" => "monthly", _ => klt },
                "count": records.len(),
                "klines": records,
                "source": "eastmoney"
            }))
        } else {
            Err(Error::Tool(format!("东方财富 kline API error: {:?}", body)))
        }
    }

    // ─── Stocks ───

    async fn stock_quote(&self, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let source = params.get("source").and_then(|v| v.as_str()).unwrap_or("auto");
        let av_key = Self::resolve_av_key(ctx, params);

        // For Chinese stocks (A股/港股), use 东方财富 API first (free, real-time, no key needed)
        if (source == "eastmoney" || source == "auto") && Self::is_chinese_stock(symbol) {
            match self.eastmoney_quote(symbol, client).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    if source == "eastmoney" {
                        return Err(e);
                    }
                    debug!(error = %e, "东方财富 quote failed, trying fallback");
                }
            }
        }

        // Try Alpha Vantage if key available
        if (source == "alpha_vantage" || source == "auto") && !av_key.is_empty() {
            let url = format!(
                "https://www.alphavantage.co/query?function=GLOBAL_QUOTE&symbol={}&apikey={}",
                symbol, av_key
            );
            debug!(url = %url, "Alpha Vantage quote");
            let resp = client.get(&url).send().await
                .map_err(|e| Error::Tool(format!("Alpha Vantage request failed: {}", e)))?;
            let body: Value = resp.json().await
                .map_err(|e| Error::Tool(format!("Failed to parse Alpha Vantage response: {}", e)))?;

            if body.get("Global Quote").is_some() {
                return Ok(body);
            }
            if source == "alpha_vantage" {
                return Err(Error::Tool(format!("Alpha Vantage error: {}", body)));
            }
        }

        // Yahoo Finance fallback (unofficial)
        let url = format!(
            "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1d&range=1d",
            symbol
        );
        debug!(url = %url, "Yahoo Finance quote");
        let resp = client.get(&url)
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await
            .map_err(|e| Error::Tool(format!("Yahoo Finance request failed: {}", e)))?;

        let status = resp.status();
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse Yahoo response: {}", e)))?;

        if !status.is_success() {
            return Err(Error::Tool(format!("Yahoo Finance error ({}): {:?}", status, body)));
        }

        // Extract key data from Yahoo response
        if let Some(result) = body.pointer("/chart/result/0") {
            let meta = result.get("meta").cloned().unwrap_or(json!({}));
            Ok(json!({
                "symbol": meta.get("symbol"),
                "price": meta.get("regularMarketPrice"),
                "previous_close": meta.get("previousClose"),
                "currency": meta.get("currency"),
                "exchange": meta.get("exchangeName"),
                "market_time": meta.get("regularMarketTime"),
                "source": "yahoo"
            }))
        } else {
            Err(Error::Tool(format!("No data found for symbol '{}'", symbol)))
        }
    }

    async fn stock_history(&self, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let interval = params.get("interval").and_then(|v| v.as_str()).unwrap_or("1mo");
        let source = params.get("source").and_then(|v| v.as_str()).unwrap_or("auto");
        let av_key = Self::resolve_av_key(ctx, params);

        // For Chinese stocks, use 东方财富 K-line API first
        if (source == "eastmoney" || source == "auto") && Self::is_chinese_stock(symbol) {
            let (klt, limit) = match interval {
                "1d" | "5d" => ("101", 5),
                "1mo" => ("101", 22),
                "3mo" => ("101", 66),
                "6mo" => ("101", 132),
                "1y" => ("102", 52),   // weekly for 1y
                "5y" => ("103", 60),   // monthly for 5y
                "max" => ("103", 240), // monthly for max
                _ => ("101", 30),
            };
            match self.eastmoney_kline(symbol, klt, limit as u64, client).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    if source == "eastmoney" {
                        return Err(e);
                    }
                    debug!(error = %e, "东方财富 kline failed, trying fallback");
                }
            }
        }

        if (source == "alpha_vantage" || source == "auto") && !av_key.is_empty() {
            let function = match interval {
                "1d" | "5d" => "TIME_SERIES_DAILY",
                _ => "TIME_SERIES_WEEKLY",
            };
            let url = format!(
                "https://www.alphavantage.co/query?function={}&symbol={}&apikey={}",
                function, symbol, av_key
            );
            let resp = client.get(&url).send().await
                .map_err(|e| Error::Tool(format!("Alpha Vantage request failed: {}", e)))?;
            let body: Value = resp.json().await
                .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
            if body.get("Error Message").is_none() && body.get("Note").is_none() {
                return Ok(body);
            }
            if source == "alpha_vantage" {
                return Err(Error::Tool(format!("Alpha Vantage error: {}", body)));
            }
        }

        // Yahoo Finance history
        let range = match interval {
            "1d" => "1d", "5d" => "5d", "1mo" => "1mo", "3mo" => "3mo",
            "6mo" => "6mo", "1y" => "1y", "5y" => "5y", "max" => "max",
            _ => "1mo",
        };
        let url = format!(
            "https://query1.finance.yahoo.com/v8/finance/chart/{}?interval=1d&range={}",
            symbol, range
        );
        let resp = client.get(&url)
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await
            .map_err(|e| Error::Tool(format!("Yahoo Finance request failed: {}", e)))?;
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        if let Some(result) = body.pointer("/chart/result/0") {
            let timestamps = result.pointer("/timestamp").cloned().unwrap_or(json!([]));
            let closes = result.pointer("/indicators/quote/0/close").cloned().unwrap_or(json!([]));
            let opens = result.pointer("/indicators/quote/0/open").cloned().unwrap_or(json!([]));
            let highs = result.pointer("/indicators/quote/0/high").cloned().unwrap_or(json!([]));
            let lows = result.pointer("/indicators/quote/0/low").cloned().unwrap_or(json!([]));
            let volumes = result.pointer("/indicators/quote/0/volume").cloned().unwrap_or(json!([]));
            Ok(json!({
                "symbol": symbol,
                "range": range,
                "timestamps": timestamps,
                "close": closes,
                "open": opens,
                "high": highs,
                "low": lows,
                "volume": volumes,
                "source": "yahoo"
            }))
        } else {
            Err(Error::Tool(format!("No history data for '{}'", symbol)))
        }
    }

    async fn stock_search(&self, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let query = params.get("query").or(params.get("symbol")).and_then(|v| v.as_str()).unwrap_or("");
        let av_key = Self::resolve_av_key(ctx, params);

        if !av_key.is_empty() {
            let url = format!(
                "https://www.alphavantage.co/query?function=SYMBOL_SEARCH&keywords={}&apikey={}",
                urlencoding::encode(query), av_key
            );
            let resp = client.get(&url).send().await
                .map_err(|e| Error::Tool(format!("Alpha Vantage request failed: {}", e)))?;
            let body: Value = resp.json().await
                .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
            return Ok(body);
        }

        // Fallback: Yahoo Finance search
        let url = format!(
            "https://query1.finance.yahoo.com/v1/finance/search?q={}&quotesCount=10",
            urlencoding::encode(query)
        );
        let resp = client.get(&url)
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await
            .map_err(|e| Error::Tool(format!("Yahoo search failed: {}", e)))?;
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
        Ok(body)
    }

    // ─── Stock Screening (东方财富 条件选股) ───

    /// Screen stocks by conditions using 东方财富 data center API.
    async fn stock_screen(&self, params: &Value, client: &Client) -> Result<Value> {
        let filters = params.get("screen_filters").cloned().unwrap_or(json!({}));
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(30);
        let market_filter = params.get("market_filter").and_then(|v| v.as_str())
            .or_else(|| filters.get("market").and_then(|v| v.as_str()))
            .unwrap_or("all");

        // Build 东方财富 stock list API query
        // fs parameter controls market: m:0+t:6,m:0+t:80 = 深市, m:1+t:2,m:1+t:23 = 沪市
        let fs = match market_filter {
            "sh" => "m:1+t:2,m:1+t:23",
            "sz" => "m:0+t:6,m:0+t:80",
            "bj" => "m:0+t:81",
            _ => "m:0+t:6,m:0+t:80,m:1+t:2,m:1+t:23",
        };

        // Determine sort field based on filters
        let (sort_field, sort_order) = if filters.get("change_pct_min").is_some() || filters.get("change_pct_max").is_some() {
            ("f3", -1) // sort by change%
        } else if filters.get("pe_min").is_some() || filters.get("pe_max").is_some() {
            ("f9", 1) // sort by PE ascending
        } else if filters.get("dividend_yield_min").is_some() {
            ("f20", -1) // sort by market cap
        } else {
            ("f3", -1) // default: sort by change%
        };

        let url = format!(
            "https://push2.eastmoney.com/api/qt/clist/get?pn=1&pz={}&po={}&np=1&fltt=2&invt=2&fid={}&fs={}&fields=f2,f3,f4,f5,f6,f7,f8,f9,f10,f12,f14,f15,f16,f17,f18,f20,f21,f23,f24,f25,f62,f115,f128,f140,f141,f136",
            limit, sort_order, sort_field, fs
        );
        debug!(url = %url, "东方财富 stock screen");

        let resp = client.get(&url)
            .header("Referer", "https://quote.eastmoney.com")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .send()
            .await
            .map_err(|e| Error::Tool(format!("东方财富 screen request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse 东方财富 screen response: {}", e)))?;

        let data = body.get("data").and_then(|d| d.get("diff"))
            .and_then(|d| d.as_array())
            .ok_or_else(|| Error::Tool("东方财富 screen: no data returned".into()))?;

        let mut results = Vec::new();
        let pe_min = filters.get("pe_min").and_then(|v| v.as_f64());
        let pe_max = filters.get("pe_max").and_then(|v| v.as_f64());
        let pb_min = filters.get("pb_min").and_then(|v| v.as_f64());
        let pb_max = filters.get("pb_max").and_then(|v| v.as_f64());
        let change_pct_min = filters.get("change_pct_min").and_then(|v| v.as_f64());
        let change_pct_max = filters.get("change_pct_max").and_then(|v| v.as_f64());
        let price_min = filters.get("price_min").and_then(|v| v.as_f64());
        let price_max = filters.get("price_max").and_then(|v| v.as_f64());
        let market_cap_min = filters.get("market_cap_min").and_then(|v| v.as_f64()).map(|v| v * 1e8);
        let market_cap_max = filters.get("market_cap_max").and_then(|v| v.as_f64()).map(|v| v * 1e8);
        let _dividend_yield_min = filters.get("dividend_yield_min").and_then(|v| v.as_f64());
        let industry = filters.get("industry").and_then(|v| v.as_str());

        for item in data {
            let price = item.get("f2").and_then(|v| v.as_f64());
            let change_pct = item.get("f3").and_then(|v| v.as_f64());
            let pe = item.get("f9").and_then(|v| v.as_f64());
            let pb = item.get("f23").and_then(|v| v.as_f64());
            let market_cap = item.get("f20").and_then(|v| v.as_f64());
            let name = item.get("f14").and_then(|v| v.as_str()).unwrap_or("");

            // Apply filters
            if let Some(min) = pe_min { if pe.map(|v| v < min).unwrap_or(true) { continue; } }
            if let Some(max) = pe_max { if pe.map(|v| v > max || v <= 0.0).unwrap_or(true) { continue; } }
            if let Some(min) = pb_min { if pb.map(|v| v < min).unwrap_or(true) { continue; } }
            if let Some(max) = pb_max { if pb.map(|v| v > max || v <= 0.0).unwrap_or(true) { continue; } }
            if let Some(min) = change_pct_min { if change_pct.map(|v| v < min).unwrap_or(true) { continue; } }
            if let Some(max) = change_pct_max { if change_pct.map(|v| v > max).unwrap_or(true) { continue; } }
            if let Some(min) = price_min { if price.map(|v| v < min).unwrap_or(true) { continue; } }
            if let Some(max) = price_max { if price.map(|v| v > max).unwrap_or(true) { continue; } }
            if let Some(min) = market_cap_min { if market_cap.map(|v| v < min).unwrap_or(true) { continue; } }
            if let Some(max) = market_cap_max { if market_cap.map(|v| v > max).unwrap_or(true) { continue; } }
            if let Some(ind) = industry { if !name.is_empty() && !name.contains(ind) { /* industry filter is best-effort */ } }

            results.push(json!({
                "code": item.get("f12").and_then(|v| v.as_str()).unwrap_or(""),
                "name": name,
                "price": price,
                "change_percent": change_pct,
                "change_amount": item.get("f4").and_then(|v| v.as_f64()),
                "volume": item.get("f5").and_then(|v| v.as_f64()),
                "amount": item.get("f6").and_then(|v| v.as_f64()),
                "amplitude": item.get("f7").and_then(|v| v.as_f64()),
                "turnover_rate": item.get("f8").and_then(|v| v.as_f64()),
                "pe_ratio": pe,
                "pb_ratio": pb,
                "open": item.get("f17").and_then(|v| v.as_f64()),
                "high": item.get("f15").and_then(|v| v.as_f64()),
                "low": item.get("f16").and_then(|v| v.as_f64()),
                "previous_close": item.get("f18").and_then(|v| v.as_f64()),
                "market_cap": market_cap,
                "float_market_cap": item.get("f21").and_then(|v| v.as_f64()),
                "money_flow": item.get("f62").and_then(|v| v.as_f64()),
            }));
        }

        Ok(json!({
            "action": "stock_screen",
            "filters": filters,
            "market": market_filter,
            "count": results.len(),
            "stocks": results,
            "source": "eastmoney"
        }))
    }

    // ─── Financial Statement (东方财富 财务数据) ───

    /// Fetch financial statement data from 东方财富 datacenter API.
    async fn financial_statement(&self, params: &Value, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let report_type = params.get("report_type").and_then(|v| v.as_str()).unwrap_or("indicator");
        let years = params.get("years").and_then(|v| v.as_u64()).unwrap_or(3);

        let (secid, _market, code) = Self::to_eastmoney_secid(symbol);

        // Determine report API based on type
        let (report_name, columns) = match report_type {
            "income" => (
                "RPT_DMSK_FN_INCOME",
                "SECUCODE,SECURITY_NAME_ABBR,REPORT_DATE,TOTAL_OPERATE_INCOME,OPERATE_INCOME,OPERATE_COST,OPERATE_EXPENSE,MANAGE_EXPENSE,FINANCE_EXPENSE,RESEARCH_EXPENSE,OPERATE_PROFIT,TOTAL_PROFIT,NETPROFIT,PARENT_NETPROFIT,BASIC_EPS"
            ),
            "balance" => (
                "RPT_DMSK_FN_BALANCE",
                "SECUCODE,SECURITY_NAME_ABBR,REPORT_DATE,TOTAL_ASSETS,TOTAL_LIABILITIES,TOTAL_EQUITY,MONETARYFUNDS,ACCOUNTS_RECE,INVENTORY,FIXED_ASSET,INTANGIBLE_ASSET,GOODWILL,SHORT_LOAN,LONG_LOAN,BOND_PAYABLE"
            ),
            "cashflow" => (
                "RPT_DMSK_FN_CASHFLOW",
                "SECUCODE,SECURITY_NAME_ABBR,REPORT_DATE,NETCASH_OPERATE,NETCASH_INVEST,NETCASH_FINANCE,CASH_EQUIVALENT_INCREASE,CCE_ADD_BEGINNING,CCE_ADD_END,SALES_SERVICES,BUY_SERVICES,PAY_STAFF_CASH,PAY_ALL_TAX"
            ),
            _ => (
                "RPT_DMSK_FN_INDICATOR",
                "SECUCODE,SECURITY_NAME_ABBR,REPORT_DATE,BASIC_EPS,BPS,WEIGHTAVG_ROE,MGJYXJJE,XSMLL,JLRL,YYZSR,YYZSRTBZZ,GSJLR,GSJLRTBZZ,KCFJCXSYJLR,KCFJCXSYJLRTBZZ,ZZCJLL,TOTALOPERATEREVE,PARENTNETPROFIT"
            ),
        };

        // Build secucode: 601318.SH format
        let secucode = if Self::is_chinese_stock(symbol) {
            let parts: Vec<&str> = secid.split('.').collect();
            if parts.len() == 2 {
                let market_prefix = parts[0];
                let stock_code = parts[1];
                match market_prefix {
                    "1" => format!("{}.SH", stock_code),
                    "0" => format!("{}.SZ", stock_code),
                    "116" => format!("{}.HK", stock_code),
                    _ => format!("{}.SH", stock_code),
                }
            } else {
                format!("{}.SH", code)
            }
        } else {
            code.clone()
        };

        let page_size = years * 4; // quarterly reports
        let url = format!(
            "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName={}&columns={}&filter=(SECUCODE=\"{}\")\
            &pageSize={}&sortColumns=REPORT_DATE&sortTypes=-1&source=WEB&client=DATACENTER",
            report_name, columns, secucode, page_size
        );
        debug!(url = %url, "东方财富 financial statement");

        let resp = client.get(&url)
            .header("Referer", "https://data.eastmoney.com")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .send()
            .await
            .map_err(|e| Error::Tool(format!("东方财富 financial request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse 东方财富 financial response: {}", e)))?;

        let success = body.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
        if !success {
            return Err(Error::Tool(format!("东方财富 financial API error: {:?}", body.get("message"))));
        }

        let data = body.get("result").and_then(|r| r.get("data"))
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        let name = data.first()
            .and_then(|d| d.get("SECURITY_NAME_ABBR"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        Ok(json!({
            "symbol": symbol,
            "name": name,
            "report_type": report_type,
            "count": data.len(),
            "reports": data,
            "source": "eastmoney"
        }))
    }

    // ─── Dividend History (东方财富 分红数据) ───

    /// Fetch dividend history from 东方财富 datacenter API.
    async fn dividend_history(&self, params: &Value, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let years = params.get("years").and_then(|v| v.as_u64()).unwrap_or(10);

        let (secid, _market, code) = Self::to_eastmoney_secid(symbol);

        // Build secucode
        let secucode = {
            let parts: Vec<&str> = secid.split('.').collect();
            if parts.len() == 2 {
                match parts[0] {
                    "1" => format!("{}.SH", parts[1]),
                    "0" => format!("{}.SZ", parts[1]),
                    "116" => format!("{}.HK", parts[1]),
                    _ => format!("{}.SH", parts[1]),
                }
            } else {
                format!("{}.SH", code)
            }
        };

        let url = format!(
            "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_SHAREBONUS_DET\
            &columns=SECUCODE,SECURITY_NAME_ABBR,BONUS_IT_RATIO,PRETAX_BONUS_RMB,PLAN_NOTICE_DATE,\
            EX_DIVIDEND_DATE,EQUITY_RECORD_DATE,PROGRESS,REPORT_DATE,ASSIGN_DETAIL\
            &filter=(SECUCODE=\"{}\")&pageSize={}&sortColumns=EX_DIVIDEND_DATE&sortTypes=-1\
            &source=WEB&client=DATACENTER",
            secucode, years * 4 // some companies pay multiple times per year
        );
        debug!(url = %url, "东方财富 dividend history");

        let resp = client.get(&url)
            .header("Referer", "https://data.eastmoney.com")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .send()
            .await
            .map_err(|e| Error::Tool(format!("东方财富 dividend request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse 东方财富 dividend response: {}", e)))?;

        let success = body.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
        if !success {
            return Err(Error::Tool(format!("东方财富 dividend API error: {:?}", body.get("message"))));
        }

        let data = body.get("result").and_then(|r| r.get("data"))
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        let name = data.first()
            .and_then(|d| d.get("SECURITY_NAME_ABBR"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Parse into cleaner format
        let mut dividends = Vec::new();
        for item in &data {
            dividends.push(json!({
                "report_date": item.get("REPORT_DATE"),
                "plan_notice_date": item.get("PLAN_NOTICE_DATE"),
                "ex_dividend_date": item.get("EX_DIVIDEND_DATE"),
                "record_date": item.get("EQUITY_RECORD_DATE"),
                "bonus_ratio": item.get("BONUS_IT_RATIO"),
                "cash_dividend_per_share": item.get("PRETAX_BONUS_RMB"),
                "progress": item.get("PROGRESS"),
                "detail": item.get("ASSIGN_DETAIL"),
            }));
        }

        Ok(json!({
            "symbol": symbol,
            "name": name,
            "count": dividends.len(),
            "dividends": dividends,
            "source": "eastmoney"
        }))
    }

    // ─── Top List / Rankings (东方财富 排行榜) ───

    /// Fetch various ranking lists from 东方财富 APIs.
    async fn top_list(&self, params: &Value, client: &Client) -> Result<Value> {
        let list_type = params.get("list_type").and_then(|v| v.as_str()).unwrap_or("gainers");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(30);
        let market_filter = params.get("market_filter").and_then(|v| v.as_str()).unwrap_or("all");

        let fs = match market_filter {
            "sh" => "m:1+t:2,m:1+t:23",
            "sz" => "m:0+t:6,m:0+t:80",
            "bj" => "m:0+t:81",
            _ => "m:0+t:6,m:0+t:80,m:1+t:2,m:1+t:23",
        };

        match list_type {
            "gainers" | "losers" => {
                let sort_order = if list_type == "gainers" { -1 } else { 1 };
                let url = format!(
                    "https://push2.eastmoney.com/api/qt/clist/get?pn=1&pz={}&po={}&np=1&fltt=2&invt=2&fid=f3&fs={}&fields=f2,f3,f4,f5,f6,f7,f8,f9,f12,f14,f15,f16,f17,f18,f20,f21,f23",
                    limit, sort_order, fs
                );
                self.fetch_stock_list(&url, list_type, client).await
            }
            "volume" => {
                let url = format!(
                    "https://push2.eastmoney.com/api/qt/clist/get?pn=1&pz={}&po=-1&np=1&fltt=2&invt=2&fid=f5&fs={}&fields=f2,f3,f4,f5,f6,f7,f8,f9,f12,f14,f15,f16,f17,f18,f20,f21,f23",
                    limit, fs
                );
                self.fetch_stock_list(&url, list_type, client).await
            }
            "turnover" => {
                let url = format!(
                    "https://push2.eastmoney.com/api/qt/clist/get?pn=1&pz={}&po=-1&np=1&fltt=2&invt=2&fid=f8&fs={}&fields=f2,f3,f4,f5,f6,f7,f8,f9,f12,f14,f15,f16,f17,f18,f20,f21,f23",
                    limit, fs
                );
                self.fetch_stock_list(&url, list_type, client).await
            }
            "money_flow" => {
                let url = format!(
                    "https://push2.eastmoney.com/api/qt/clist/get?pn=1&pz={}&po=-1&np=1&fltt=2&invt=2&fid=f62&fs={}&fields=f2,f3,f12,f14,f62,f184,f66,f69,f72,f75,f78,f81,f84,f87",
                    limit, fs
                );
                let resp = client.get(&url)
                    .header("Referer", "https://quote.eastmoney.com")
                    .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                    .send().await
                    .map_err(|e| Error::Tool(format!("东方财富 money_flow request failed: {}", e)))?;
                let body: Value = resp.json().await
                    .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
                let data = body.get("data").and_then(|d| d.get("diff"))
                    .and_then(|d| d.as_array()).cloned().unwrap_or_default();
                let mut results = Vec::new();
                for item in &data {
                    results.push(json!({
                        "code": item.get("f12").and_then(|v| v.as_str()),
                        "name": item.get("f14").and_then(|v| v.as_str()),
                        "price": item.get("f2").and_then(|v| v.as_f64()),
                        "change_percent": item.get("f3").and_then(|v| v.as_f64()),
                        "main_net_inflow": item.get("f62").and_then(|v| v.as_f64()),
                        "main_net_ratio": item.get("f184").and_then(|v| v.as_f64()),
                        "super_large_inflow": item.get("f66").and_then(|v| v.as_f64()),
                        "large_inflow": item.get("f72").and_then(|v| v.as_f64()),
                        "medium_inflow": item.get("f78").and_then(|v| v.as_f64()),
                        "small_inflow": item.get("f84").and_then(|v| v.as_f64()),
                    }));
                }
                Ok(json!({
                    "list_type": "money_flow",
                    "count": results.len(),
                    "stocks": results,
                    "source": "eastmoney"
                }))
            }
            "north_flow" => {
                let url = "https://push2.eastmoney.com/api/qt/kamt.rtmin/get?fields1=f1,f2,f3,f4&fields2=f51,f52,f53,f54,f55,f56";
                let resp = client.get(url)
                    .header("Referer", "https://quote.eastmoney.com")
                    .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                    .send().await
                    .map_err(|e| Error::Tool(format!("东方财富 north_flow request failed: {}", e)))?;
                let body: Value = resp.json().await
                    .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
                let data = body.get("data").cloned().unwrap_or(json!({}));
                Ok(json!({
                    "list_type": "north_flow",
                    "data": data,
                    "description": "f1=沪股通净流入, f2=深股通净流入, f3=北向资金合计净流入, f4=南向资金合计净流入 (单位:万元). f51-f56=分钟级时间序列",
                    "source": "eastmoney"
                }))
            }
            "dragon_tiger" => {
                let url = format!(
                    "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_DAILYBILLBOARD_DETAILSNEW\
                    &columns=SECUCODE,SECURITY_NAME_ABBR,TRADE_DATE,CHANGE_RATE,CLOSE_PRICE,TURNOVERVALUE,\
                    BILLBOARD_NET_AMT,BILLBOARD_BUY_AMT,BILLBOARD_SELL_AMT,BILLBOARD_DEAL_AMT,ACCUM_AMOUNT,\
                    DEAL_NET_RATIO,DEAL_AMOUNT_RATIO,EXPLANATION\
                    &pageSize={}&sortColumns=TRADE_DATE,TURNOVERVALUE&sortTypes=-1,-1\
                    &source=WEB&client=DATACENTER",
                    limit
                );
                let resp = client.get(&url)
                    .header("Referer", "https://data.eastmoney.com")
                    .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                    .send().await
                    .map_err(|e| Error::Tool(format!("东方财富 dragon_tiger request failed: {}", e)))?;
                let body: Value = resp.json().await
                    .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
                let data = body.get("result").and_then(|r| r.get("data"))
                    .and_then(|d| d.as_array()).cloned().unwrap_or_default();
                Ok(json!({
                    "list_type": "dragon_tiger",
                    "count": data.len(),
                    "records": data,
                    "source": "eastmoney"
                }))
            }
            "limit_up" => {
                let url = format!(
                    "https://push2.eastmoney.com/api/qt/clist/get?pn=1&pz={}&po=-1&np=1&fltt=2&invt=2&fid=f3&fs={}&fields=f2,f3,f4,f5,f6,f7,f8,f9,f12,f14,f15,f16,f17,f18,f20,f21,f23&f3=10",
                    limit, fs
                );
                self.fetch_stock_list(&url, "limit_up", client).await
            }
            "limit_down" => {
                let url = format!(
                    "https://push2.eastmoney.com/api/qt/clist/get?pn=1&pz={}&po=1&np=1&fltt=2&invt=2&fid=f3&fs={}&fields=f2,f3,f4,f5,f6,f7,f8,f9,f12,f14,f15,f16,f17,f18,f20,f21,f23&f3=-10",
                    limit, fs
                );
                self.fetch_stock_list(&url, "limit_down", client).await
            }
            _ => Err(Error::Tool(format!("Unknown list_type '{}'. Valid: gainers, losers, volume, turnover, money_flow, north_flow, dragon_tiger, limit_up, limit_down", list_type))),
        }
    }

    /// Helper: fetch a stock list from 东方财富 clist API and format results.
    async fn fetch_stock_list(&self, url: &str, list_type: &str, client: &Client) -> Result<Value> {
        let resp = client.get(url)
            .header("Referer", "https://quote.eastmoney.com")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .send().await
            .map_err(|e| Error::Tool(format!("东方财富 {} request failed: {}", list_type, e)))?;
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
        let data = body.get("data").and_then(|d| d.get("diff"))
            .and_then(|d| d.as_array()).cloned().unwrap_or_default();
        let mut results = Vec::new();
        for item in &data {
            results.push(json!({
                "code": item.get("f12").and_then(|v| v.as_str()),
                "name": item.get("f14").and_then(|v| v.as_str()),
                "price": item.get("f2").and_then(|v| v.as_f64()),
                "change_percent": item.get("f3").and_then(|v| v.as_f64()),
                "change_amount": item.get("f4").and_then(|v| v.as_f64()),
                "volume": item.get("f5").and_then(|v| v.as_f64()),
                "amount": item.get("f6").and_then(|v| v.as_f64()),
                "amplitude": item.get("f7").and_then(|v| v.as_f64()),
                "turnover_rate": item.get("f8").and_then(|v| v.as_f64()),
                "pe_ratio": item.get("f9").and_then(|v| v.as_f64()),
                "pb_ratio": item.get("f23").and_then(|v| v.as_f64()),
                "market_cap": item.get("f20").and_then(|v| v.as_f64()),
            }));
        }
        Ok(json!({
            "list_type": list_type,
            "count": results.len(),
            "stocks": results,
            "source": "eastmoney"
        }))
    }

    // ─── Bond Data (债券数据) ───

    /// Fetch treasury bond yield curve data from 东方财富.
    /// Supports China treasury (中国国债), US treasury (美国国债).
    async fn bond_yield(&self, params: &Value, client: &Client) -> Result<Value> {
        let bond_type = params.get("bond_type").and_then(|v| v.as_str()).unwrap_or("china_treasury");
        let term = params.get("term").and_then(|v| v.as_str()).unwrap_or("all");

        match bond_type {
            "china_treasury" | "us_treasury" => {
                // 东方财富 treasury yield API
                // China: secid format 1.ZZGZSYL (中债国债收益率)
                // US: via datacenter API
                let is_china = bond_type == "china_treasury";
                let url = if is_china {
                    "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_BOND_TREASURY_YIELD\
                    &columns=ALL&sortColumns=SOLAR_DATE&sortTypes=-1&pageSize=30\
                    &source=WEB&client=DATACENTER".to_string()
                } else {
                    "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_BOND_TREASURY_YIELD_US\
                    &columns=ALL&sortColumns=SOLAR_DATE&sortTypes=-1&pageSize=30\
                    &source=WEB&client=DATACENTER".to_string()
                };

                debug!(url = %url, bond_type = bond_type, "东方财富 bond yield");
                let resp = client.get(&url)
                    .header("Referer", "https://data.eastmoney.com")
                    .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                    .send().await
                    .map_err(|e| Error::Tool(format!("Bond yield request failed: {}", e)))?;

                let body: Value = resp.json().await
                    .map_err(|e| Error::Tool(format!("Failed to parse bond yield response: {}", e)))?;

                let data = body.get("result").and_then(|r| r.get("data"))
                    .and_then(|d| d.as_array()).cloned().unwrap_or_default();

                // Filter by term if specified
                let filtered: Vec<Value> = if term == "all" {
                    data
                } else {
                    // Take only the latest date's data
                    let date = data.first()
                        .and_then(|f| f.get("SOLAR_DATE").and_then(|v| v.as_str()))
                        .unwrap_or("").to_string();
                    if date.is_empty() {
                        data
                    } else {
                        data.into_iter().filter(|d| {
                            d.get("SOLAR_DATE").and_then(|v| v.as_str()).unwrap_or("") == date
                        }).collect()
                    }
                };

                Ok(json!({
                    "action": "bond_yield",
                    "bond_type": bond_type,
                    "term_filter": term,
                    "count": filtered.len(),
                    "yields": filtered,
                    "description": if is_china {
                        "中国国债收益率. Fields: SOLAR_DATE=日期, EMM00588704=1年期, EMM00166462=2年期, EMM00166466=5年期, EMM00166469=7年期, EMM00166470=10年期, EMM00166471=30年期"
                    } else {
                        "美国国债收益率. Fields: SOLAR_DATE=日期, EMM00588704=1M, EMM01276014=3M, EMM00166462=6M, EMM00166466=1Y, EMM00166469=2Y, EMM00166470=5Y, EMM00166471=10Y, EMM01276015=30Y"
                    },
                    "source": "eastmoney"
                }))
            }
            _ => Err(Error::Tool(format!("Unknown bond_type '{}'. Valid: china_treasury, us_treasury", bond_type))),
        }
    }

    /// Fetch bond detail info from 东方财富.
    async fn bond_info(&self, params: &Value, client: &Client) -> Result<Value> {
        let bond_code = params.get("bond_code").and_then(|v| v.as_str())
            .or_else(|| params.get("symbol").and_then(|v| v.as_str()))
            .unwrap_or("");

        if bond_code.is_empty() {
            return Err(Error::Tool("'bond_code' or 'symbol' is required for bond_info".into()));
        }

        // 东方财富 bond detail API
        let url = format!(
            "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_BOND_CB_LIST\
            &columns=ALL&filter=(SECURITY_CODE=\"{}\")\
            &pageSize=1&source=WEB&client=DATACENTER",
            bond_code
        );

        debug!(url = %url, "东方财富 bond info");
        let resp = client.get(&url)
            .header("Referer", "https://data.eastmoney.com")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .send().await
            .map_err(|e| Error::Tool(format!("Bond info request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse bond info response: {}", e)))?;

        let data = body.get("result").and_then(|r| r.get("data"))
            .and_then(|d| d.as_array()).cloned().unwrap_or_default();

        if data.is_empty() {
            // Try general bond search
            let url2 = format!(
                "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_BOND_BS_QUOTATION\
                &columns=ALL&filter=(SECURITY_CODE=\"{}\")\
                &pageSize=1&source=WEB&client=DATACENTER",
                bond_code
            );
            let resp2 = client.get(&url2)
                .header("Referer", "https://data.eastmoney.com")
                .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                .send().await
                .map_err(|e| Error::Tool(format!("Bond info request failed: {}", e)))?;
            let body2: Value = resp2.json().await
                .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
            let data2 = body2.get("result").and_then(|r| r.get("data"))
                .and_then(|d| d.as_array()).cloned().unwrap_or_default();

            return Ok(json!({
                "action": "bond_info",
                "bond_code": bond_code,
                "count": data2.len(),
                "bonds": data2,
                "source": "eastmoney"
            }));
        }

        Ok(json!({
            "action": "bond_info",
            "bond_code": bond_code,
            "count": data.len(),
            "bonds": data,
            "source": "eastmoney"
        }))
    }

    /// Fetch convertible bond (可转债) list from 东方财富.
    async fn convertible_bond(&self, params: &Value, client: &Client) -> Result<Value> {
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(30);
        let bond_code = params.get("bond_code").and_then(|v| v.as_str()).unwrap_or("");

        let url = if !bond_code.is_empty() {
            format!(
                "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_BOND_CB_LIST\
                &columns=ALL&filter=(SECURITY_CODE=\"{}\")\
                &pageSize=1&source=WEB&client=DATACENTER",
                bond_code
            )
        } else {
            format!(
                "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_BOND_CB_LIST\
                &columns=ALL&sortColumns=PUBLIC_START_DATE&sortTypes=-1\
                &pageSize={}&source=WEB&client=DATACENTER",
                limit
            )
        };

        debug!(url = %url, "东方财富 convertible bond");
        let resp = client.get(&url)
            .header("Referer", "https://data.eastmoney.com")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .send().await
            .map_err(|e| Error::Tool(format!("Convertible bond request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse convertible bond response: {}", e)))?;

        let data = body.get("result").and_then(|r| r.get("data"))
            .and_then(|d| d.as_array()).cloned().unwrap_or_default();

        // Extract key fields into cleaner format
        let mut bonds = Vec::new();
        for item in &data {
            bonds.push(json!({
                "bond_code": item.get("SECURITY_CODE"),
                "bond_name": item.get("SECURITY_NAME_ABBR"),
                "stock_code": item.get("CONVERT_STOCK_CODE"),
                "stock_name": item.get("SECURITY_SHORT_NAME"),
                "current_price": item.get("TRADE_PRICE"),
                "convert_price": item.get("CONVERT_STOCK_PRICE"),
                "convert_value": item.get("CONVERT_VALUE"),
                "premium_rate": item.get("PREMIUM_RATE"),
                "ytm": item.get("YIELD_TO_MATURITY"),
                "issue_size": item.get("ISSUE_SIZE"),
                "remain_size": item.get("REMAIN_SIZE"),
                "maturity_date": item.get("CEASE_DATE"),
                "credit_rating": item.get("RATING"),
                "listing_date": item.get("LISTING_DATE"),
            }));
        }

        Ok(json!({
            "action": "convertible_bond",
            "count": bonds.len(),
            "bonds": bonds,
            "description": "可转债列表. convert_price=转股价, convert_value=转股价值, premium_rate=溢价率(%), ytm=到期收益率(%)",
            "source": "eastmoney"
        }))
    }

    // ─── Futures Data (期货数据) ───

    /// Fetch futures position/open interest data from 东方财富.
    async fn futures_position(&self, params: &Value, client: &Client) -> Result<Value> {
        let futures_symbol = params.get("futures_symbol").and_then(|v| v.as_str())
            .or_else(|| params.get("symbol").and_then(|v| v.as_str()))
            .unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20);

        if futures_symbol.is_empty() {
            // Return overview of all major futures
            let url = format!(
                "https://push2.eastmoney.com/api/qt/clist/get?pn=1&pz={}&po=-1&np=1&fltt=2&invt=2\
                &fid=f3&fs=m:113,m:114,m:115,m:8&fields=f2,f3,f4,f5,f6,f12,f14,f15,f16,f17,f18",
                limit
            );
            let resp = client.get(&url)
                .header("Referer", "https://quote.eastmoney.com")
                .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                .send().await
                .map_err(|e| Error::Tool(format!("Futures position request failed: {}", e)))?;
            let body: Value = resp.json().await
                .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
            let data = body.get("data").and_then(|d| d.get("diff"))
                .and_then(|d| d.as_array()).cloned().unwrap_or_default();

            let mut results = Vec::new();
            for item in &data {
                results.push(json!({
                    "code": item.get("f12").and_then(|v| v.as_str()),
                    "name": item.get("f14").and_then(|v| v.as_str()),
                    "price": item.get("f2").and_then(|v| v.as_f64()),
                    "change_percent": item.get("f3").and_then(|v| v.as_f64()),
                    "volume": item.get("f5").and_then(|v| v.as_f64()),
                    "amount": item.get("f6").and_then(|v| v.as_f64()),
                    "open": item.get("f17").and_then(|v| v.as_f64()),
                    "high": item.get("f15").and_then(|v| v.as_f64()),
                    "low": item.get("f16").and_then(|v| v.as_f64()),
                    "previous_close": item.get("f18").and_then(|v| v.as_f64()),
                }));
            }
            return Ok(json!({
                "action": "futures_position",
                "count": results.len(),
                "futures": results,
                "source": "eastmoney"
            }));
        }

        // Specific futures contract position data
        let url = format!(
            "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_FUTU_POSITIONDETAILS\
            &columns=ALL&filter=(TRADE_CODE=\"{}\")\
            &sortColumns=TRADE_DATE&sortTypes=-1&pageSize={}\
            &source=WEB&client=DATACENTER",
            futures_symbol.to_uppercase(), limit
        );

        debug!(url = %url, "东方财富 futures position");
        let resp = client.get(&url)
            .header("Referer", "https://data.eastmoney.com")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .send().await
            .map_err(|e| Error::Tool(format!("Futures position request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        let data = body.get("result").and_then(|r| r.get("data"))
            .and_then(|d| d.as_array()).cloned().unwrap_or_default();

        Ok(json!({
            "action": "futures_position",
            "symbol": futures_symbol,
            "count": data.len(),
            "positions": data,
            "description": "期货持仓数据. TRADE_DATE=日期, LONG_OPENINTEREST=多头持仓, SHORT_OPENINTEREST=空头持仓, LONG_CHANGE=多头变化, SHORT_CHANGE=空头变化",
            "source": "eastmoney"
        }))
    }

    /// Fetch futures contract specification from 东方财富.
    async fn futures_contract_info(&self, params: &Value, client: &Client) -> Result<Value> {
        let futures_symbol = params.get("futures_symbol").and_then(|v| v.as_str())
            .or_else(|| params.get("symbol").and_then(|v| v.as_str()))
            .unwrap_or("");
        let futures_exchange = params.get("futures_exchange").and_then(|v| v.as_str()).unwrap_or("all");

        // Map exchange to 东方财富 market codes
        let fs = match futures_exchange {
            "shfe" => "m:113",   // 上期所
            "dce" => "m:114",    // 大商所
            "czce" => "m:115",   // 郑商所
            "cffex" => "m:8",    // 中金所
            "ine" => "m:142",    // 上海能源
            "gfex" => "m:225",   // 广期所
            _ => "m:113,m:114,m:115,m:8,m:142",
        };

        if futures_symbol.is_empty() {
            // List all main contracts
            let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(30);
            let url = format!(
                "https://push2.eastmoney.com/api/qt/clist/get?pn=1&pz={}&po=-1&np=1&fltt=2&invt=2\
                &fid=f3&fs={}&fields=f2,f3,f4,f5,f6,f7,f8,f12,f14,f15,f16,f17,f18,f20",
                limit, fs
            );
            let resp = client.get(&url)
                .header("Referer", "https://quote.eastmoney.com")
                .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
                .send().await
                .map_err(|e| Error::Tool(format!("Futures contract request failed: {}", e)))?;
            let body: Value = resp.json().await
                .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
            let data = body.get("data").and_then(|d| d.get("diff"))
                .and_then(|d| d.as_array()).cloned().unwrap_or_default();

            let mut results = Vec::new();
            for item in &data {
                results.push(json!({
                    "code": item.get("f12").and_then(|v| v.as_str()),
                    "name": item.get("f14").and_then(|v| v.as_str()),
                    "price": item.get("f2").and_then(|v| v.as_f64()),
                    "change_percent": item.get("f3").and_then(|v| v.as_f64()),
                    "volume": item.get("f5").and_then(|v| v.as_f64()),
                    "amount": item.get("f6").and_then(|v| v.as_f64()),
                    "amplitude": item.get("f7").and_then(|v| v.as_f64()),
                    "turnover_rate": item.get("f8").and_then(|v| v.as_f64()),
                    "open": item.get("f17").and_then(|v| v.as_f64()),
                    "high": item.get("f15").and_then(|v| v.as_f64()),
                    "low": item.get("f16").and_then(|v| v.as_f64()),
                    "previous_close": item.get("f18").and_then(|v| v.as_f64()),
                }));
            }
            return Ok(json!({
                "action": "futures_contract_info",
                "exchange": futures_exchange,
                "count": results.len(),
                "contracts": results,
                "source": "eastmoney"
            }));
        }

        // Specific contract info via quote API
        let symbol_upper = futures_symbol.to_uppercase();
        // Try to determine market for secid
        let secid = if symbol_upper.starts_with("IF") || symbol_upper.starts_with("IC") || symbol_upper.starts_with("IH") || symbol_upper.starts_with("IM") || symbol_upper.starts_with("TS") || symbol_upper.starts_with("TF") || symbol_upper.starts_with("T2") {
            format!("8.{}", symbol_upper)  // 中金所
        } else if symbol_upper.starts_with("SC") || symbol_upper.starts_with("NR") || symbol_upper.starts_with("BC") || symbol_upper.starts_with("LU") {
            format!("142.{}", symbol_upper) // 上海能源
        } else if symbol_upper.starts_with("A") || symbol_upper.starts_with("B") || symbol_upper.starts_with("C") || symbol_upper.starts_with("I") || symbol_upper.starts_with("J") || symbol_upper.starts_with("L") || symbol_upper.starts_with("M") || symbol_upper.starts_with("P") || symbol_upper.starts_with("V") || symbol_upper.starts_with("Y") || symbol_upper.starts_with("EG") || symbol_upper.starts_with("EB") || symbol_upper.starts_with("PG") || symbol_upper.starts_with("RR") || symbol_upper.starts_with("LH") {
            format!("114.{}", symbol_upper) // 大商所
        } else if symbol_upper.starts_with("CF") || symbol_upper.starts_with("SR") || symbol_upper.starts_with("TA") || symbol_upper.starts_with("MA") || symbol_upper.starts_with("FG") || symbol_upper.starts_with("OI") || symbol_upper.starts_with("RM") || symbol_upper.starts_with("ZC") || symbol_upper.starts_with("AP") || symbol_upper.starts_with("CJ") || symbol_upper.starts_with("UR") || symbol_upper.starts_with("SA") || symbol_upper.starts_with("PF") || symbol_upper.starts_with("PK") || symbol_upper.starts_with("SH") {
            format!("115.{}", symbol_upper) // 郑商所
        } else {
            format!("113.{}", symbol_upper) // 上期所 default
        };

        let url = format!(
            "https://push2.eastmoney.com/api/qt/stock/get?secid={}&fields=f43,f44,f45,f46,f47,f48,f50,f51,f52,f57,f58,f60,f168,f169,f170,f171",
            secid
        );
        let resp = client.get(&url)
            .header("Referer", "https://quote.eastmoney.com")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .send().await
            .map_err(|e| Error::Tool(format!("Futures contract info request failed: {}", e)))?;
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        if let Some(data) = body.get("data") {
            if data.is_null() {
                return Err(Error::Tool(format!("No data for futures contract '{}'", futures_symbol)));
            }
            Ok(json!({
                "action": "futures_contract_info",
                "symbol": futures_symbol,
                "code": data.get("f57"),
                "name": data.get("f58"),
                "price": data.get("f43"),
                "high": data.get("f44"),
                "low": data.get("f45"),
                "open": data.get("f46"),
                "volume": data.get("f47"),
                "amount": data.get("f48"),
                "volume_ratio": data.get("f50"),
                "limit_up": data.get("f51"),
                "limit_down": data.get("f52"),
                "previous_close": data.get("f60"),
                "turnover_rate": data.get("f168"),
                "change": data.get("f169"),
                "change_percent": data.get("f170"),
                "amplitude": data.get("f171"),
                "source": "eastmoney"
            }))
        } else {
            Err(Error::Tool(format!("Futures contract API error for '{}'", futures_symbol)))
        }
    }

    // ─── Institutional Holdings (机构持仓) ───

    /// Fetch institutional holdings changes from 东方财富 datacenter.
    async fn institutional_holdings(&self, params: &Value, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20);

        let (secid, _market, code) = Self::to_eastmoney_secid(symbol);
        let secucode = {
            let parts: Vec<&str> = secid.split('.').collect();
            if parts.len() == 2 {
                match parts[0] {
                    "1" => format!("{}.SH", parts[1]),
                    "0" => format!("{}.SZ", parts[1]),
                    "116" => format!("{}.HK", parts[1]),
                    _ => format!("{}.SH", parts[1]),
                }
            } else {
                format!("{}.SH", code)
            }
        };

        // 东方财富 institutional holdings API (基金持仓)
        let url = format!(
            "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_MAIN_ORGHOLD\
            &columns=ALL&filter=(SECURITY_CODE=\"{}\")\
            &sortColumns=REPORT_DATE&sortTypes=-1&pageSize={}\
            &source=WEB&client=DATACENTER",
            code, limit
        );

        debug!(url = %url, "东方财富 institutional holdings");
        let resp = client.get(&url)
            .header("Referer", "https://data.eastmoney.com")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .send().await
            .map_err(|e| Error::Tool(format!("Institutional holdings request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        let data = body.get("result").and_then(|r| r.get("data"))
            .and_then(|d| d.as_array()).cloned().unwrap_or_default();

        // Also try to get top 10 fund holders
        let url2 = format!(
            "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_F10_EH_FREEHOLDERS\
            &columns=ALL&filter=(SECUCODE=\"{}\")\
            &sortColumns=REPORT_DATE,HOLDER_RANK&sortTypes=-1,1&pageSize={}\
            &source=WEB&client=DATACENTER",
            secucode, limit
        );
        let resp2 = client.get(&url2)
            .header("Referer", "https://data.eastmoney.com")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .send().await.ok();
        let holders = if let Some(resp) = resp2 {
            let body2: Value = resp.json().await.unwrap_or(json!({}));
            body2.get("result").and_then(|r| r.get("data"))
                .and_then(|d| d.as_array()).cloned().unwrap_or_default()
        } else {
            Vec::new()
        };

        Ok(json!({
            "action": "institutional_holdings",
            "symbol": symbol,
            "institutional_summary": data,
            "top_holders": holders,
            "description": "机构持仓数据. institutional_summary: REPORT_DATE=报告期, TOTAL_SHARES=持股总数, HOLD_RATIO=持股比例, ORG_NUM=机构数. top_holders: HOLDER_NAME=持有人, HOLD_NUM=持股数, FREE_HOLDNUM_RATIO=占流通股比例, HOLDER_RANK=排名",
            "source": "eastmoney"
        }))
    }

    // ─── Analyst Ratings (研报评级) ───

    /// Fetch analyst ratings and research reports from 东方财富.
    async fn analyst_ratings(&self, params: &Value, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20);

        let (_secid, _market, code) = Self::to_eastmoney_secid(symbol);

        // 东方财富 research report ratings API
        let url = format!(
            "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_CUSTOM_STOCK_RESEARCH\
            &columns=ALL&filter=(SECURITY_CODE=\"{}\")\
            &sortColumns=REPORT_DATE&sortTypes=-1&pageSize={}\
            &source=WEB&client=DATACENTER",
            code, limit
        );

        debug!(url = %url, "东方财富 analyst ratings");
        let resp = client.get(&url)
            .header("Referer", "https://data.eastmoney.com")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .send().await
            .map_err(|e| Error::Tool(format!("Analyst ratings request failed: {}", e)))?;

        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        let data = body.get("result").and_then(|r| r.get("data"))
            .and_then(|d| d.as_array()).cloned().unwrap_or_default();

        // Extract key fields
        let mut ratings = Vec::new();
        for item in &data {
            ratings.push(json!({
                "report_date": item.get("REPORT_DATE"),
                "org_name": item.get("ORG_NAME"),
                "researcher": item.get("RESEARCHER"),
                "rating": item.get("RATING"),
                "rating_change": item.get("RATING_CHANGE"),
                "target_price": item.get("PREDICT_NEXT_TWO_YEAR_EPS"),
                "title": item.get("TITLE"),
                "predict_year": item.get("PREDICT_YEAR"),
                "predict_eps": item.get("PREDICT_NEXT_TWO_YEAR_EPS"),
                "predict_pe": item.get("PREDICT_NEXT_TWO_YEAR_PE"),
            }));
        }

        // Also get consensus rating summary
        let url2 = format!(
            "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_CUSTOM_STOCK_RESEARCH_STAT\
            &columns=ALL&filter=(SECURITY_CODE=\"{}\")\
            &pageSize=1&source=WEB&client=DATACENTER",
            code
        );
        let resp2 = client.get(&url2)
            .header("Referer", "https://data.eastmoney.com")
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
            .send().await.ok();
        let consensus = if let Some(resp) = resp2 {
            let body2: Value = resp.json().await.unwrap_or(json!({}));
            body2.get("result").and_then(|r| r.get("data"))
                .and_then(|d| d.as_array())
                .and_then(|a| a.first())
                .cloned()
        } else {
            None
        };

        Ok(json!({
            "action": "analyst_ratings",
            "symbol": symbol,
            "count": ratings.len(),
            "ratings": ratings,
            "consensus": consensus,
            "description": "券商研报评级. RATING=评级(买入/增持/中性/减持/卖出), RATING_CHANGE=评级变化(上调/维持/下调), ORG_NAME=机构, RESEARCHER=分析师, TITLE=研报标题",
            "source": "eastmoney"
        }))
    }

    // ─── Crypto ───

    async fn crypto_price(&self, params: &Value, client: &Client) -> Result<Value> {
        let vs = params.get("vs_currency").and_then(|v| v.as_str()).unwrap_or("usd");

        // Support single symbol or multiple
        let ids = if let Some(symbol) = params.get("symbol").and_then(|v| v.as_str()) {
            symbol.to_string()
        } else if let Some(symbols) = params.get("symbols").and_then(|v| v.as_array()) {
            symbols.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>().join(",")
        } else {
            return Err(Error::Tool("'symbol' or 'symbols' is required".into()));
        };

        let url = format!(
            "https://api.coingecko.com/api/v3/simple/price?ids={}&vs_currencies={}&include_24hr_change=true&include_24hr_vol=true&include_market_cap=true",
            urlencoding::encode(&ids), vs
        );
        debug!(url = %url, "CoinGecko price");
        let resp = client.get(&url)
            .header("User-Agent", "blockcell-agent")
            .send()
            .await
            .map_err(|e| Error::Tool(format!("CoinGecko request failed: {}", e)))?;

        let status = resp.status();
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse CoinGecko response: {}", e)))?;

        if !status.is_success() {
            return Err(Error::Tool(format!("CoinGecko error ({}): {:?}", status, body)));
        }
        Ok(body)
    }

    async fn crypto_history(&self, params: &Value, client: &Client) -> Result<Value> {
        let symbol = params.get("symbol").and_then(|v| v.as_str()).unwrap_or("bitcoin");
        let vs = params.get("vs_currency").and_then(|v| v.as_str()).unwrap_or("usd");
        let interval = params.get("interval").and_then(|v| v.as_str()).unwrap_or("30d");

        let days = match interval {
            "1h" => "1", "24h" => "1", "7d" => "7", "30d" => "30",
            "90d" => "90", "1y" => "365", "max" => "max",
            _ => "30",
        };

        let url = format!(
            "https://api.coingecko.com/api/v3/coins/{}/market_chart?vs_currency={}&days={}",
            symbol, vs, days
        );
        let resp = client.get(&url)
            .header("User-Agent", "blockcell-agent")
            .send()
            .await
            .map_err(|e| Error::Tool(format!("CoinGecko request failed: {}", e)))?;

        let status = resp.status();
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        if !status.is_success() {
            return Err(Error::Tool(format!("CoinGecko error ({}): {:?}", status, body)));
        }
        Ok(json!({
            "symbol": symbol,
            "vs_currency": vs,
            "days": days,
            "prices": body.get("prices"),
            "market_caps": body.get("market_caps"),
            "total_volumes": body.get("total_volumes")
        }))
    }

    async fn crypto_list(&self, params: &Value, client: &Client) -> Result<Value> {
        let vs = params.get("vs_currency").and_then(|v| v.as_str()).unwrap_or("usd");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20);
        let category = params.get("category").and_then(|v| v.as_str()).unwrap_or("");

        let mut url = format!(
            "https://api.coingecko.com/api/v3/coins/markets?vs_currency={}&order=market_cap_desc&per_page={}&page=1&sparkline=false",
            vs, limit
        );
        if !category.is_empty() {
            url.push_str(&format!("&category={}", urlencoding::encode(category)));
        }

        let resp = client.get(&url)
            .header("User-Agent", "blockcell-agent")
            .send()
            .await
            .map_err(|e| Error::Tool(format!("CoinGecko request failed: {}", e)))?;

        let status = resp.status();
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        if !status.is_success() {
            return Err(Error::Tool(format!("CoinGecko error ({}): {:?}", status, body)));
        }
        Ok(body)
    }

    // ─── Forex ───

    async fn forex_rate(&self, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let from = params.get("from_currency").and_then(|v| v.as_str()).unwrap_or("USD");
        let to = params.get("to_currency").and_then(|v| v.as_str()).unwrap_or("CNY");
        let av_key = Self::resolve_av_key(ctx, params);

        if !av_key.is_empty() {
            let url = format!(
                "https://www.alphavantage.co/query?function=CURRENCY_EXCHANGE_RATE&from_currency={}&to_currency={}&apikey={}",
                from, to, av_key
            );
            let resp = client.get(&url).send().await
                .map_err(|e| Error::Tool(format!("Alpha Vantage request failed: {}", e)))?;
            let body: Value = resp.json().await
                .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
            if body.get("Realtime Currency Exchange Rate").is_some() {
                return Ok(body);
            }
        }

        // Fallback: free exchange rate API
        let url = format!("https://open.er-api.com/v6/latest/{}", from);
        let resp = client.get(&url).send().await
            .map_err(|e| Error::Tool(format!("Exchange rate API failed: {}", e)))?;
        let body: Value = resp.json().await
            .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

        if let Some(rate) = body.pointer(&format!("/rates/{}", to)) {
            Ok(json!({
                "from": from,
                "to": to,
                "rate": rate,
                "time_last_update": body.get("time_last_update_utc"),
                "source": "open.er-api.com"
            }))
        } else {
            Err(Error::Tool(format!("No exchange rate found for {} → {}", from, to)))
        }
    }

    async fn forex_history(&self, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let from = params.get("from_currency").and_then(|v| v.as_str()).unwrap_or("USD");
        let to = params.get("to_currency").and_then(|v| v.as_str()).unwrap_or("CNY");
        let av_key = Self::resolve_av_key(ctx, params);

        if !av_key.is_empty() {
            let url = format!(
                "https://www.alphavantage.co/query?function=FX_DAILY&from_symbol={}&to_symbol={}&apikey={}",
                from, to, av_key
            );
            let resp = client.get(&url).send().await
                .map_err(|e| Error::Tool(format!("Alpha Vantage request failed: {}", e)))?;
            let body: Value = resp.json().await
                .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;
            return Ok(body);
        }

        Err(Error::Tool("Forex history requires an Alpha Vantage API key. Set ALPHA_VANTAGE_API_KEY or pass api_key parameter.".into()))
    }

    // ─── Portfolio ───

    async fn portfolio_value(&self, ctx: &ToolContext, params: &Value, client: &Client) -> Result<Value> {
        let holdings = params.get("holdings").and_then(|v| v.as_array())
            .ok_or_else(|| Error::Tool("'holdings' array is required".into()))?;

        let mut results = Vec::new();
        let mut total_value = 0.0_f64;
        let mut total_cost = 0.0_f64;

        for holding in holdings {
            let symbol = holding.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
            let quantity = holding.get("quantity").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let cost_basis = holding.get("cost_basis").and_then(|v| v.as_f64()).unwrap_or(0.0);

            if symbol.is_empty() || quantity == 0.0 {
                continue;
            }

            // Try to get current price
            let price = self.get_price(ctx, params, client, symbol).await.unwrap_or(0.0);
            let value = price * quantity;
            let cost = cost_basis * quantity;
            let pnl = value - cost;
            let pnl_pct = if cost > 0.0 { (pnl / cost) * 100.0 } else { 0.0 };

            total_value += value;
            total_cost += cost;

            results.push(json!({
                "symbol": symbol,
                "quantity": quantity,
                "cost_basis": cost_basis,
                "current_price": price,
                "value": (value * 100.0).round() / 100.0,
                "pnl": (pnl * 100.0).round() / 100.0,
                "pnl_percent": (pnl_pct * 100.0).round() / 100.0
            }));
        }

        let total_pnl = total_value - total_cost;
        let total_pnl_pct = if total_cost > 0.0 { (total_pnl / total_cost) * 100.0 } else { 0.0 };

        Ok(json!({
            "holdings": results,
            "total_value": (total_value * 100.0).round() / 100.0,
            "total_cost": (total_cost * 100.0).round() / 100.0,
            "total_pnl": (total_pnl * 100.0).round() / 100.0,
            "total_pnl_percent": (total_pnl_pct * 100.0).round() / 100.0
        }))
    }

    async fn get_price(&self, ctx: &ToolContext, _params: &Value, client: &Client, symbol: &str) -> Result<f64> {
        // Try stock quote first
        let quote_params = json!({"action": "stock_quote", "symbol": symbol, "source": "auto"});
        if let Ok(result) = self.stock_quote(ctx, &quote_params, client).await {
            // Alpha Vantage format
            if let Some(price_str) = result.pointer("/Global Quote/05. price").and_then(|v| v.as_str()) {
                if let Ok(p) = price_str.parse::<f64>() {
                    return Ok(p);
                }
            }
            // Yahoo format
            if let Some(p) = result.get("price").and_then(|v| v.as_f64()) {
                return Ok(p);
            }
        }

        // Try crypto
        let crypto_params = json!({"action": "crypto_price", "symbol": symbol, "vs_currency": "usd"});
        if let Ok(result) = self.crypto_price(&crypto_params, client).await {
            if let Some(p) = result.pointer(&format!("/{}/usd", symbol)).and_then(|v| v.as_f64()) {
                return Ok(p);
            }
        }

        Err(Error::Tool(format!("Could not get price for '{}'", symbol)))
    }

    // ─── Market Overview ───

    async fn market_overview(&self, params: &Value, client: &Client) -> Result<Value> {
        let vs = params.get("vs_currency").and_then(|v| v.as_str()).unwrap_or("usd");

        // Get global crypto market data
        let global_url = "https://api.coingecko.com/api/v3/global";
        let global_resp = client.get(global_url)
            .header("User-Agent", "blockcell-agent")
            .send()
            .await
            .map_err(|e| Error::Tool(format!("CoinGecko request failed: {}", e)))?;
        let global: Value = global_resp.json().await.unwrap_or(json!({}));

        // Get top cryptos
        let top_url = format!(
            "https://api.coingecko.com/api/v3/coins/markets?vs_currency={}&order=market_cap_desc&per_page=10&page=1&sparkline=false",
            vs
        );
        let top_resp = client.get(&top_url)
            .header("User-Agent", "blockcell-agent")
            .send()
            .await
            .map_err(|e| Error::Tool(format!("CoinGecko request failed: {}", e)))?;
        let top_coins: Value = top_resp.json().await.unwrap_or(json!([]));

        // Get trending
        let trending_url = "https://api.coingecko.com/api/v3/search/trending";
        let trending_resp = client.get(trending_url)
            .header("User-Agent", "blockcell-agent")
            .send()
            .await
            .map_err(|e| Error::Tool(format!("CoinGecko request failed: {}", e)))?;
        let trending: Value = trending_resp.json().await.unwrap_or(json!({}));

        Ok(json!({
            "global": global.get("data"),
            "top_coins": top_coins,
            "trending": trending.get("coins")
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = FinanceApiTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "finance_api");
        assert!(schema.description.contains("eastmoney") || schema.description.contains("东方财富"));
        assert!(schema.description.contains("Chinese") || schema.description.contains("A股"));
    }

    #[test]
    fn test_validate_valid() {
        let tool = FinanceApiTool;
        assert!(tool.validate(&json!({"action": "stock_quote", "symbol": "AAPL"})).is_ok());
        assert!(tool.validate(&json!({"action": "stock_quote", "symbol": "601318"})).is_ok());
        assert!(tool.validate(&json!({"action": "crypto_price", "symbol": "bitcoin"})).is_ok());
        assert!(tool.validate(&json!({"action": "forex_rate", "from_currency": "USD", "to_currency": "CNY"})).is_ok());
        assert!(tool.validate(&json!({"action": "market_overview"})).is_ok());
        assert!(tool.validate(&json!({"action": "crypto_list"})).is_ok());
    }

    #[test]
    fn test_validate_missing_symbol() {
        let tool = FinanceApiTool;
        assert!(tool.validate(&json!({"action": "stock_quote"})).is_err());
        assert!(tool.validate(&json!({"action": "crypto_price"})).is_err());
    }

    #[test]
    fn test_validate_missing_currency() {
        let tool = FinanceApiTool;
        assert!(tool.validate(&json!({"action": "forex_rate", "from_currency": "USD"})).is_err());
        assert!(tool.validate(&json!({"action": "forex_rate"})).is_err());
    }

    #[test]
    fn test_validate_portfolio() {
        let tool = FinanceApiTool;
        assert!(tool.validate(&json!({"action": "portfolio_value", "holdings": [{"symbol": "AAPL", "quantity": 10, "cost_basis": 150.0}]})).is_ok());
        assert!(tool.validate(&json!({"action": "portfolio_value"})).is_err());
        assert!(tool.validate(&json!({"action": "portfolio_value", "holdings": []})).is_err());
    }

    #[test]
    fn test_validate_invalid_action() {
        let tool = FinanceApiTool;
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }

    #[test]
    fn test_is_chinese_stock() {
        // A股 6-digit codes
        assert!(FinanceApiTool::is_chinese_stock("601318"));  // 中国平安 沪市
        assert!(FinanceApiTool::is_chinese_stock("000001"));  // 平安银行 深市
        assert!(FinanceApiTool::is_chinese_stock("300750"));  // 宁德时代 创业板
        assert!(FinanceApiTool::is_chinese_stock("688981"));  // 中芯国际 科创板
        assert!(FinanceApiTool::is_chinese_stock("600519"));  // 贵州茅台
        assert!(FinanceApiTool::is_chinese_stock("002594"));  // 比亚迪

        // With suffix
        assert!(FinanceApiTool::is_chinese_stock("601318.SH"));
        assert!(FinanceApiTool::is_chinese_stock("000001.SZ"));
        assert!(FinanceApiTool::is_chinese_stock("601318.SS"));
        assert!(FinanceApiTool::is_chinese_stock("00700.HK"));

        // With prefix
        assert!(FinanceApiTool::is_chinese_stock("SH601318"));
        assert!(FinanceApiTool::is_chinese_stock("SZ000001"));

        // HK 5-digit
        assert!(FinanceApiTool::is_chinese_stock("00700"));  // 腾讯
        assert!(FinanceApiTool::is_chinese_stock("09988"));  // 阿里巴巴

        // NOT Chinese stocks
        assert!(!FinanceApiTool::is_chinese_stock("AAPL"));
        assert!(!FinanceApiTool::is_chinese_stock("MSFT"));
        assert!(!FinanceApiTool::is_chinese_stock("TSLA"));
        assert!(!FinanceApiTool::is_chinese_stock("bitcoin"));
        assert!(!FinanceApiTool::is_chinese_stock("123"));  // too short
        assert!(!FinanceApiTool::is_chinese_stock("999999"));  // not a valid prefix
    }

    #[test]
    fn test_to_eastmoney_secid() {
        // 沪市主板
        let (secid, market, code) = FinanceApiTool::to_eastmoney_secid("601318");
        assert_eq!(secid, "1.601318");
        assert_eq!(market, "sh");
        assert_eq!(code, "601318");

        // 深市主板
        let (secid, market, _) = FinanceApiTool::to_eastmoney_secid("000001");
        assert_eq!(secid, "0.000001");
        assert_eq!(market, "sz");

        // 创业板
        let (secid, market, _) = FinanceApiTool::to_eastmoney_secid("300750");
        assert_eq!(secid, "0.300750");
        assert_eq!(market, "sz");

        // 科创板
        let (secid, market, _) = FinanceApiTool::to_eastmoney_secid("688981");
        assert_eq!(secid, "1.688981");
        assert_eq!(market, "sh");

        // With .SH suffix
        let (secid, market, code) = FinanceApiTool::to_eastmoney_secid("601318.SH");
        assert_eq!(secid, "1.601318");
        assert_eq!(market, "sh");
        assert_eq!(code, "601318");

        // With .SZ suffix
        let (secid, _, _) = FinanceApiTool::to_eastmoney_secid("000001.SZ");
        assert_eq!(secid, "0.000001");

        // HK stock
        let (secid, market, code) = FinanceApiTool::to_eastmoney_secid("00700.HK");
        assert_eq!(secid, "116.00700");
        assert_eq!(market, "hk");
        assert_eq!(code, "00700");

        // HK 5-digit without suffix
        let (secid, market, _) = FinanceApiTool::to_eastmoney_secid("00700");
        assert_eq!(secid, "116.00700");
        assert_eq!(market, "hk");

        // With SH prefix
        let (secid, market, code) = FinanceApiTool::to_eastmoney_secid("SH601318");
        assert_eq!(secid, "1.601318");
        assert_eq!(market, "sh");
        assert_eq!(code, "601318");
    }

    #[test]
    fn test_validate_new_actions() {
        let tool = FinanceApiTool;
        // stock_screen: no symbol required
        assert!(tool.validate(&json!({"action": "stock_screen"})).is_ok());
        assert!(tool.validate(&json!({"action": "stock_screen", "screen_filters": {"pe_max": 15}})).is_ok());
        // financial_statement: symbol required
        assert!(tool.validate(&json!({"action": "financial_statement", "symbol": "601318"})).is_ok());
        assert!(tool.validate(&json!({"action": "financial_statement"})).is_err());
        // dividend_history: symbol required
        assert!(tool.validate(&json!({"action": "dividend_history", "symbol": "600519"})).is_ok());
        assert!(tool.validate(&json!({"action": "dividend_history"})).is_err());
        // top_list: no symbol required
        assert!(tool.validate(&json!({"action": "top_list"})).is_ok());
        assert!(tool.validate(&json!({"action": "top_list", "list_type": "gainers"})).is_ok());
        assert!(tool.validate(&json!({"action": "top_list", "list_type": "north_flow"})).is_ok());
    }

    #[test]
    fn test_schema_new_actions() {
        let tool = FinanceApiTool;
        let schema = tool.schema();
        let desc = schema.description;
        assert!(desc.contains("stock_screen"));
        assert!(desc.contains("financial_statement"));
        assert!(desc.contains("dividend_history"));
        assert!(desc.contains("top_list"));
        assert!(desc.contains("bond_yield"));
        assert!(desc.contains("convertible_bond"));
        assert!(desc.contains("futures_position"));
        assert!(desc.contains("institutional_holdings"));
        assert!(desc.contains("analyst_ratings"));
        // Check new params exist
        let params = &schema.parameters;
        assert!(params.get("properties").unwrap().get("screen_filters").is_some());
        assert!(params.get("properties").unwrap().get("report_type").is_some());
        assert!(params.get("properties").unwrap().get("list_type").is_some());
        assert!(params.get("properties").unwrap().get("years").is_some());
        assert!(params.get("properties").unwrap().get("market_filter").is_some());
        assert!(params.get("properties").unwrap().get("bond_type").is_some());
        assert!(params.get("properties").unwrap().get("term").is_some());
        assert!(params.get("properties").unwrap().get("bond_code").is_some());
        assert!(params.get("properties").unwrap().get("futures_exchange").is_some());
        assert!(params.get("properties").unwrap().get("futures_symbol").is_some());
    }

    #[test]
    fn test_validate_bond_actions() {
        let tool = FinanceApiTool;
        // bond_yield: no symbol required
        assert!(tool.validate(&json!({"action": "bond_yield"})).is_ok());
        assert!(tool.validate(&json!({"action": "bond_yield", "bond_type": "china_treasury"})).is_ok());
        assert!(tool.validate(&json!({"action": "bond_yield", "bond_type": "us_treasury", "term": "10y"})).is_ok());
        // bond_info: no symbol required at validate level (checked in execute)
        assert!(tool.validate(&json!({"action": "bond_info"})).is_ok());
        // convertible_bond: no symbol required
        assert!(tool.validate(&json!({"action": "convertible_bond"})).is_ok());
    }

    #[test]
    fn test_validate_futures_actions() {
        let tool = FinanceApiTool;
        // futures_position: no symbol required (returns overview)
        assert!(tool.validate(&json!({"action": "futures_position"})).is_ok());
        assert!(tool.validate(&json!({"action": "futures_position", "futures_symbol": "rb2505"})).is_ok());
        // futures_contract_info: no symbol required (returns list)
        assert!(tool.validate(&json!({"action": "futures_contract_info"})).is_ok());
        assert!(tool.validate(&json!({"action": "futures_contract_info", "futures_exchange": "shfe"})).is_ok());
    }

    #[test]
    fn test_validate_institutional_analyst() {
        let tool = FinanceApiTool;
        // institutional_holdings: symbol required
        assert!(tool.validate(&json!({"action": "institutional_holdings", "symbol": "601318"})).is_ok());
        assert!(tool.validate(&json!({"action": "institutional_holdings"})).is_err());
        // analyst_ratings: symbol required
        assert!(tool.validate(&json!({"action": "analyst_ratings", "symbol": "600519"})).is_ok());
        assert!(tool.validate(&json!({"action": "analyst_ratings"})).is_err());
    }
}
