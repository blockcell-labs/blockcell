// stock_analysis skill — 股票分析 via finance_api (东方财富免费数据，零API Key)

let input = ctx.user_input;
let stock_name = "";
let stock_code = "";

// 1. 识别股票代码
let code_match = input.match_regex(r"\b(\d{5,6})\b");
if code_match.len() > 0 {
    stock_code = code_match[0];
    stock_name = stock_code;
} else {
    stock_name = input;
}

// 2. 场景识别
let is_limit_up = input.contains("涨停") || input.contains("龙虎榜") || input.contains("涨停板");
let is_capital = input.contains("资金") || input.contains("主力") || input.contains("北向");
let is_news = input.contains("新闻") || input.contains("研报") || input.contains("消息") || input.contains("原因") || input.contains("为什么");
let is_macro = input.contains("宏观") || input.contains("CPI") || input.contains("PMI") || input.contains("GDP") || input.contains("社融") || input.contains("M2") || input.contains("利率") || input.contains("准备金");
let is_market = input.contains("大盘") || input.contains("行业") || input.contains("板块") || input.contains("市场");
let is_fundamental = input.contains("财务") || input.contains("基本面") || input.contains("年报") || input.contains("季报") || input.contains("机构") || input.contains("分红") || input.contains("ROE");

// 3. 执行分析
if is_macro {
    // 场景4: 宏观数据
    let indicator = "cpi";
    if input.contains("GDP") || input.contains("gdp") { indicator = "gdp"; }
    else if input.contains("PPI") || input.contains("ppi") { indicator = "ppi"; }
    else if input.contains("PMI") || input.contains("制造业") { indicator = "pmi_manufacturing"; }
    else if input.contains("服务业PMI") { indicator = "pmi_services"; }
    else if input.contains("社融") { indicator = "social_financing"; }
    else if input.contains("M2") { indicator = "m2"; }
    else if input.contains("LPR") || input.contains("贷款利率") { indicator = "lpr"; }
    else if input.contains("准备金") { indicator = "rrr"; }
    else if input.contains("零售") || input.contains("消费") { indicator = "retail_sales"; }
    else if input.contains("工业") { indicator = "industrial_output"; }
    else if input.contains("贸易") || input.contains("进出口") { indicator = "trade_balance"; }

    let macro_result = call_tool("finance_api", #{
        action: "macro_data",
        indicator: indicator,
        limit: 12
    });

    return #{
        scenario: "macro_data",
        indicator: indicator,
        data: macro_result,
        instruction: "请基于以上宏观经济数据，分析最新趋势、同比变化、政策含义，并给出市场影响判断。"
    }

} else if is_market && stock_code == "" {
    // 场景5: 大盘/行业分析
    let industry_result = call_tool("finance_api", #{
        action: "industry_fund_flow"
    });

    let north_result = call_tool("finance_api", #{
        action: "northbound_flow",
        period: "10d"
    });

    let top_result = call_tool("finance_api", #{
        action: "top_list",
        list_type: "money_flow"
    });

    return #{
        scenario: "market_overview",
        industry_flow: industry_result,
        northbound: north_result,
        top_stocks: top_result,
        instruction: "请综合行业资金流向、北向资金和资金流向排名数据，分析当前市场结构：哪些行业获得资金青睐？外资动向如何？给出整体市场判断。"
    }

} else if is_limit_up && stock_code != "" {
    // 场景1: 涨停原因分析
    let quote_result = call_tool("finance_api", #{
        action: "stock_quote",
        symbol: stock_code
    });

    let news_result = call_tool("finance_api", #{
        action: "stock_news",
        symbol: stock_code,
        limit: 10
    });

    let dragon_result = call_tool("finance_api", #{
        action: "top_list",
        list_type: "dragon_tiger"
    });

    let capital_result = call_tool("finance_api", #{
        action: "capital_flow",
        symbol: stock_code
    });

    return #{
        scenario: "limit_up_analysis",
        stock: stock_name,
        code: stock_code,
        quote: quote_result,
        news: news_result,
        dragon_tiger: dragon_result,
        capital: capital_result,
        instruction: "请综合以上数据，分析" + stock_name + "今日涨停的原因，包括：触发涨停的核心催化剂（来自新闻）、龙虎榜主要买卖方及意图、主力资金净流入情况、后市展望。最后加风险提示。"
    }

} else if is_capital && stock_code != "" {
    // 场景2: 资金流向分析
    let capital_result = call_tool("finance_api", #{
        action: "capital_flow",
        symbol: stock_code,
        period: "5d"
    });

    let north_result = call_tool("finance_api", #{
        action: "northbound_flow",
        period: "10d"
    });

    let industry_result = call_tool("finance_api", #{
        action: "industry_fund_flow"
    });

    return #{
        scenario: "capital_flow",
        stock: stock_name,
        code: stock_code,
        capital: capital_result,
        northbound: north_result,
        industry: industry_result,
        instruction: "请分析" + stock_name + "5日资金流向（主力/超大单/大单净流入）、北向资金近10日趋势、所属行业资金排名，判断主力动向和市场情绪。"
    }

} else if is_news && stock_code != "" {
    // 场景3: 新闻研报
    let news_result = call_tool("finance_api", #{
        action: "stock_news",
        symbol: stock_code,
        limit: 20
    });

    return #{
        scenario: "news_report",
        stock: stock_name,
        code: stock_code,
        news: news_result,
        instruction: "请基于以上新闻数据，总结" + stock_name + "的最新动态、市场情绪和主要关注点。如新闻不足，建议 web_search 补充。"
    }

} else if is_fundamental && stock_code != "" {
    // 场景6: 财务基本面
    let fin_result = call_tool("finance_api", #{
        action: "financial_statement",
        symbol: stock_code,
        report_type: "indicator",
        years: 3
    });

    let inst_result = call_tool("finance_api", #{
        action: "institutional_holdings",
        symbol: stock_code
    });

    let div_result = call_tool("finance_api", #{
        action: "dividend_history",
        symbol: stock_code
    });

    return #{
        scenario: "fundamental",
        stock: stock_name,
        code: stock_code,
        financial: fin_result,
        institutional: inst_result,
        dividends: div_result,
        instruction: "请基于财务指标（ROE/毛利率/净利率/资产负债率）、机构持仓变化、分红历史，给出" + stock_name + "的基本面综合评价。"
    }

} else if stock_code != "" {
    // 场景0: 默认 — 实时行情 + K线技术分析
    let quote_result = call_tool("finance_api", #{
        action: "stock_quote",
        symbol: stock_code
    });

    let history_result = call_tool("finance_api", #{
        action: "stock_history",
        symbol: stock_code,
        interval: "1d"
    });

    return #{
        scenario: "quote_and_kline",
        stock: stock_name,
        code: stock_code,
        quote: quote_result,
        history: history_result,
        instruction: "请基于实时行情和60日K线数据完成分析：1.行情概览(现价/涨跌/换手率/PE/PB/市值) 2.技术指标(MA5/MA20/MA60/MACD(DIF/DEA/BAR)/RSI(14)) 3.趋势判断 4.风险提示:以上数据仅供参考，不构成投资建议。"
    }

} else {
    // 未识别到代码，搜索
    let search_result = call_tool("finance_api", #{
        action: "stock_search",
        query: stock_name
    });

    return #{
        scenario: "search",
        query: stock_name,
        results: search_result,
        instruction: "请根据搜索结果，告知用户找到的股票列表，询问用户确认具体是哪只股票，然后再进行分析。"
    }
}
