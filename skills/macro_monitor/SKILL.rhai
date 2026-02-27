// macro_monitor skill — 宏观经济监控，东方财富数据中心，零API Key
let input = ctx.user_input;

let indicator = "cpi";
if input.contains("GDP") || input.contains("gdp") || input.contains("经济总量") { indicator = "gdp"; }
else if input.contains("PPI") || input.contains("出厂价格") { indicator = "ppi"; }
else if input.contains("制造业PMI") || input.contains("PMI制造") { indicator = "pmi_manufacturing"; }
else if input.contains("服务业PMI") || input.contains("非制造业") { indicator = "pmi_services"; }
else if input.contains("社融") || input.contains("社会融资") { indicator = "social_financing"; }
else if input.contains("M2") || input.contains("货币供应") { indicator = "m2"; }
else if input.contains("LPR") || input.contains("贷款利率") || input.contains("基准利率") { indicator = "lpr"; }
else if input.contains("存款准备金") || input.contains("RRR") || input.contains("降准") { indicator = "rrr"; }
else if input.contains("零售") || input.contains("社会消费品") { indicator = "retail_sales"; }
else if input.contains("工业") || input.contains("工业增加值") { indicator = "industrial_output"; }
else if input.contains("进出口") || input.contains("贸易") || input.contains("顺差") { indicator = "trade_balance"; }
else if input.contains("CPI") || input.contains("通胀") || input.contains("物价") { indicator = "cpi"; }

let macro_result = call_tool("finance_api", #{
    action: "macro_data",
    indicator: indicator,
    limit: 12
});

let bond_result = call_tool("finance_api", #{
    action: "bond_yield",
    bond_type: "china_treasury",
    term: "all"
});

#{
    indicator: indicator,
    macro_data: macro_result,
    bond_yield: bond_result,
    instruction: "请基于以上宏观指标数据和国债收益率曲线，分析最新经济走势：1.核心指标最新值及同比变化 2.与历史均值对比 3.政策含义（央行/财政政策走向）4.对股市/债市/汇率的影响判断"
}
