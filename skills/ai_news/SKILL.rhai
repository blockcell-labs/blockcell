// ai_news skill — 直接抓取权威AI科技媒体，去重排序，获取最新AI资讯
// 数据源: 36氪、机器之心、量子位、InfoQ AI

let input = ctx.user_input;

// 识别用户指定来源
let want_36kr      = input.contains("36氪") || input.contains("36kr");
let want_jiqizhixin = input.contains("机器之心") || input.contains("jiqizhixin");
let want_qbitai    = input.contains("量子位") || input.contains("qbitai");
let want_english   = input.contains("英文") || input.contains("国际") || input.contains("global") || input.contains("english");

// 构建抓取列表 (默认全部抓，用户指定时只抓对应源)
let sources = [];
if want_36kr || (!want_jiqizhixin && !want_qbitai) {
    sources += [#{
        name: "36氪",
        url: "https://36kr.com/information/AI"
    }];
}
if want_jiqizhixin || (!want_36kr && !want_qbitai) {
    sources += [#{
        name: "机器之心",
        url: "https://www.jiqizhixin.com/"
    }];
}
if want_qbitai || (!want_36kr && !want_jiqizhixin) {
    sources += [#{
        name: "量子位",
        url: "https://www.qbitai.com/"
    }];
}

// 抓取各来源页面
let fetched = [];
let failed_sources = [];

for source in sources {
    let result = call_tool("web_fetch", #{
        url: source.url,
        extractMode: "markdown",
        maxChars: 30000
    });

    // 检查是否成功（内容不为空且有实质内容）
    let text = "";
    if result.is_map() && result["text"] != () {
        text = result["text"].to_string();
    }

    if text.len() > 300 {
        fetched += [#{
            name: source.name,
            url: source.url,
            content: text
        }];
    } else {
        failed_sources += [source.name];
    }
}

// 如果用户想要英文来源，额外抓 TechCrunch AI 板块
if want_english {
    let tc_result = call_tool("web_fetch", #{
        url: "https://techcrunch.com/category/artificial-intelligence/",
        extractMode: "markdown",
        maxChars: 20000
    });
    let tc_text = "";
    if tc_result.is_map() && tc_result["text"] != () {
        tc_text = tc_result["text"].to_string();
    }
    if tc_text.len() > 300 {
        fetched += [#{
            name: "TechCrunch AI",
            url: "https://techcrunch.com/category/artificial-intelligence/",
            content: tc_text
        }];
    }
}

// 如果全部失败，返回错误
if fetched.len() == 0 {
    return #{
        success: false,
        error: "所有新闻源均无法访问，请稍后重试。失败来源: " + failed_sources.join(", "),
        instruction: "告知用户所有AI新闻来源均无法访问，可能是网络问题或网站反爬，建议稍后重试。"
    }
}

// 组装结果，交给LLM整理
let fetch_summary = [];
for item in fetched {
    fetch_summary += [#{
        source: item.name,
        url: item.url,
        content_length: item.content.len(),
        content: item.content
    }];
}

let failed_msg = "";
if failed_sources.len() > 0 {
    failed_msg = "（以下来源抓取失败，已跳过: " + failed_sources.join(", ") + "）";
}

return #{
    success: true,
    sources_fetched: fetched.len(),
    sources_failed: failed_sources,
    data: fetch_summary,
    instruction: "你已获取以下AI科技媒体的最新页面内容（Markdown格式）。请完成以下任务：\n1. 从每个来源的内容中提取文章列表（标题 + 链接 + 日期/时间 + 一句话摘要）\n2. 合并所有来源的文章，按日期从新到旧排序\n3. 去重：标题前20字相同的文章只保留一条（优先保留有日期的那条）\n4. 选取最新的15-20条展示\n5. 按来源分组输出，格式：\n   📰 AI 最新资讯 (当前时间)\n   ════════════════════\n   **来源：{name}**\n   1. [标题](链接) — 日期\n      摘要：...\n   ...\n   ---\n   数据来源：{来源列表} | 抓取时间：{时间}\n6. " + failed_msg + "\n注意：只列出真实存在的文章，不要捏造内容。如果某段内容无法解析出文章列表，跳过该部分。"
}
