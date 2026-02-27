// news_digest skill — 新闻摘要，web_search + finance_api stock_news，零API Key
let input = ctx.user_input;
let topic = params.get("topic");
if topic == () { topic = ""; }

let is_stock_news = input.contains("股票") || input.contains("个股") || topic.contains("股票");
let is_finance = input.contains("财经") || input.contains("金融") || input.contains("上市公司") || topic.contains("财经");

if is_stock_news {
    // 个股新闻：尝试从输入识别股票代码
    let code_match = input.match_regex(r"\b(\d{5,6})\b");
    if code_match.len() > 0 {
        call_tool("finance_api", #{
            action: "stock_news",
            symbol: code_match[0],
            limit: 20
        })
    } else {
        let search_query = if topic != "" { topic + " 新闻 最新" } else { input + " 新闻" };
        call_tool("web_search", #{ query: search_query })
    }
} else if is_finance {
    // 财经新闻：搜索 + 北向资金动向
    let search_query = if topic != "" { topic + " 财经新闻 今日" } else { "财经新闻 今日要闻" };
    let search_result = call_tool("web_search", #{ query: search_query });
    let north_result = call_tool("finance_api", #{
        action: "northbound_flow",
        period: "today"
    });
    #{
        news: search_result,
        northbound: north_result,
        instruction: "请整理以上财经新闻，总结今日重要财经动态并结合北向资金趋势给出市场判断。"
    }
} else {
    // 通用新闻搜索
    let search_query = if topic != "" { topic + " 最新新闻 今日" } else { "今日头条新闻" };
    call_tool("web_search", #{ query: search_query })
}
