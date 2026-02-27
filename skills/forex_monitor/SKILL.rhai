// forex_monitor skill — 汇率监控，finance_api免费数据，零API Key
let input = ctx.user_input;
let from = params.get("from_currency");
if from == () { from = "USD"; }
let to = params.get("to_currency");
if to == () { to = "CNY"; }

// 从输入中识别货币对
if input.contains("美元") || input.contains("USD") { from = "USD"; }
if input.contains("人民币") || input.contains("CNY") { to = "CNY"; }
if input.contains("欧元") || input.contains("EUR") {
    if from == "USD" { to = "EUR"; } else { from = "EUR"; }
}
if input.contains("日元") || input.contains("JPY") {
    if from == "USD" { to = "JPY"; } else { from = "JPY"; }
}
if input.contains("英镑") || input.contains("GBP") {
    if from == "USD" { to = "GBP"; } else { from = "GBP"; }
}
if input.contains("港元") || input.contains("HKD") {
    if from == "USD" { to = "HKD"; } else { from = "HKD"; }
}

let is_history = input.contains("历史") || input.contains("走势") || input.contains("趋势");

if is_history {
    let rate = call_tool("finance_api", #{
        action: "forex_rate",
        from_currency: from,
        to_currency: to
    });
    let history = call_tool("finance_api", #{
        action: "forex_history",
        from_currency: from,
        to_currency: to,
        interval: "1mo"
    });
    #{
        rate: rate,
        history: history,
        instruction: "请分析" + from + "/" + to + "汇率走势：当前汇率、近期涨跌幅、技术面支撑/阻力位、影响因素。"
    }
} else {
    let rate = call_tool("finance_api", #{
        action: "forex_rate",
        from_currency: from,
        to_currency: to
    });
    #{
        rate: rate,
        instruction: "请展示" + from + "/" + to + "当前汇率，并简要分析近期走势和主要影响因素。"
    }
}
