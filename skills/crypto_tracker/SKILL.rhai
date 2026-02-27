// crypto_tracker skill — CoinGecko免费数据，零API Key
let input = ctx.user_input;
let symbol = params.get("symbol");
if symbol == () { symbol = params.get("coin"); }
if symbol == () { symbol = "bitcoin"; }

let is_list = input.contains("列表") || input.contains("排行") || input.contains("top") || input.contains("Top");
let is_market = input.contains("市场") || input.contains("总市值") || input.contains("大盘") || input.contains("overview");
let is_history = input.contains("历史") || input.contains("走势") || input.contains("K线") || input.contains("history");

if is_market {
    call_tool("finance_api", #{
        action: "market_overview"
    })
} else if is_list {
    call_tool("finance_api", #{
        action: "crypto_list",
        vs_currency: "usd"
    })
} else if is_history {
    call_tool("finance_api", #{
        action: "crypto_history",
        symbol: symbol,
        vs_currency: "usd",
        interval: "7d"
    })
} else {
    call_tool("finance_api", #{
        action: "crypto_price",
        symbol: symbol,
        vs_currency: "usd"
    })
}
