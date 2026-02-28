// weather skill — 直接访问 wttr.in 免费天气API获取实时天气和预报
// 零 API Key，全球覆盖，支持中文城市名

let input = ctx.user_input;

// 1. 提取城市名
// 常见中文城市 → 英文映射（wttr.in 也支持中文但英文更稳定）
let city_map = #{
    "北京": "Beijing",
    "上海": "Shanghai",
    "深圳": "Shenzhen",
    "广州": "Guangzhou",
    "成都": "Chengdu",
    "杭州": "Hangzhou",
    "武汉": "Wuhan",
    "南京": "Nanjing",
    "西安": "Xian",
    "重庆": "Chongqing",
    "苏州": "Suzhou",
    "天津": "Tianjin",
    "长沙": "Changsha",
    "郑州": "Zhengzhou",
    "青岛": "Qingdao",
    "大连": "Dalian",
    "宁波": "Ningbo",
    "厦门": "Xiamen",
    "济南": "Jinan",
    "合肥": "Hefei",
    "福州": "Fuzhou",
    "昆明": "Kunming",
    "哈尔滨": "Harbin",
    "沈阳": "Shenyang",
    "长春": "Changchun",
    "太原": "Taiyuan",
    "石家庄": "Shijiazhuang",
    "南昌": "Nanchang",
    "贵阳": "Guiyang",
    "海口": "Haikou",
    "三亚": "Sanya",
    "乌鲁木齐": "Urumqi",
    "兰州": "Lanzhou",
    "银川": "Yinchuan",
    "西宁": "Xining",
    "呼和浩特": "Hohhot",
    "香港": "Hong+Kong",
    "澳门": "Macau",
    "台北": "Taipei",
    "台湾": "Taiwan"
};

// 识别城市（简单提取：去掉常见词后剩余部分）
let city_raw = "";

// 先尝试从 city_map 中匹配
for key in city_map.keys() {
    if input.contains(key) {
        city_raw = city_map[key];
        break;
    }
}

// 如果没找到，尝试从输入里提取城市名
// 常见句式: "北京天气", "查下上海天气", "xx天气怎么样"
if city_raw == "" {
    // 移除常见干扰词，剩余部分可能是城市名
    let clean = input;
    clean = clean.replace("天气", "");
    clean = clean.replace("预报", "");
    clean = clean.replace("查", "");
    clean = clean.replace("查询", "");
    clean = clean.replace("查下", "");
    clean = clean.replace("今天", "");
    clean = clean.replace("明天", "");
    clean = clean.replace("后天", "");
    clean = clean.replace("一周", "");
    clean = clean.replace("7天", "");
    clean = clean.replace("怎么样", "");
    clean = clean.replace("如何", "");
    clean = clean.replace("？", "");
    clean = clean.replace("?", "");
    clean = clean.trim();

    if clean.len() > 0 && clean.len() <= 10 {
        city_raw = clean;
    }
}

// 默认城市
if city_raw == "" {
    city_raw = "Beijing";
}

// URL 编码（空格转+）
let city_encoded = city_raw.replace(" ", "+");

// 2. 识别查询场景
let want_aqi    = input.contains("空气") || input.contains("AQI") || input.contains("PM2.5") || input.contains("pm2.5");
let want_week   = input.contains("一周") || input.contains("7天") || input.contains("周") || input.contains("未来");
let want_advice = input.contains("带伞") || input.contains("穿") || input.contains("出行") || input.contains("建议");

// 3. 抓取 wttr.in JSON（首选，含当前天气+3天预报）
let wttr_url = "https://wttr.in/" + city_encoded + "?format=j1";
let weather_data = call_tool("web_fetch", #{
    url: wttr_url,
    extractMode: "raw",
    maxChars: 20000
});

let got_json = false;
let weather_text = "";

if weather_data.is_map() && weather_data["text"] != () {
    weather_text = weather_data["text"].to_string();
    // 简单判断是否是 JSON（以 { 开头）
    if weather_text.trim().starts_with("{") {
        got_json = true;
    }
}

// 4. 如果 JSON 失败，回退到文本格式
if !got_json || weather_text.len() < 100 {
    let fallback_url = "https://wttr.in/" + city_encoded + "?lang=zh&format=4";
    let fallback_data = call_tool("web_fetch", #{
        url: fallback_url,
        extractMode: "raw",
        maxChars: 5000
    });

    if fallback_data.is_map() && fallback_data["text"] != () {
        let fb_text = fallback_data["text"].to_string();
        if fb_text.len() > 20 {
            // 文本格式成功
            return #{
                success: true,
                city: city_raw,
                source: "wttr.in (text)",
                format: "text",
                data: fb_text,
                instruction: "以下是城市 " + city_raw + " 的天气数据（文本格式）。请解析并以友好格式展示天气信息，包括：当前气温、天气状况、最高/最低温。如果用户询问出行建议（带伞/穿衣），根据天气状况给出实用建议。格式：\n🌤️ {城市} 天气\n当前：{天气} {温度}°C\n最高/最低：{max}/{min}°C\n💡 建议：{根据天气给出穿衣带伞建议}"
            }
        }
    }

    // 全部失败
    return #{
        success: false,
        city: city_raw,
        error: "无法获取 " + city_raw + " 的天气数据，请检查城市名是否正确，或稍后重试。",
        instruction: "告知用户无法获取天气数据。如果城市名是中文，建议提供拼音或英文城市名重试。"
    }
}

// 5. 如果需要 AQI，额外抓取
let aqi_data = "";
if want_aqi {
    let aqi_city = city_raw.replace("+", "-").to_lower();
    let aqi_url = "https://aqicn.org/city/" + aqi_city + "/";
    let aqi_result = call_tool("web_fetch", #{
        url: aqi_url,
        extractMode: "markdown",
        maxChars: 5000
    });
    if aqi_result.is_map() && aqi_result["text"] != () {
        aqi_data = aqi_result["text"].to_string();
    }
}

// 6. 返回数据给 LLM 处理
let instruction = "以下是城市 \"" + city_raw + "\" 的 wttr.in JSON 天气数据。请解析 JSON 并按以下格式输出：\n\n";
instruction += "🌤️ {城市} 天气 — {当前日期时间}\n";
instruction += "════════════════════════════\n\n";
instruction += "☀️ 当前天气\n";
instruction += "  天气：{weatherDesc} | 气温：{temp_C}°C（体感 {FeelsLikeC}°C）\n";
instruction += "  湿度：{humidity}% | 风速：{windspeedKmph}km/h {winddir16Point}\n";
instruction += "  能见度：{visibility}km | 紫外线：{uvIndex} | 气压：{pressure}hPa\n\n";
instruction += "📅 未来3天预报\n";
instruction += "  今天({date})：{天气描述} 最低{mintempC}°C ~ 最高{maxtempC}°C\n";
instruction += "  明天({date})：...\n";
instruction += "  后天({date})：...\n\n";
instruction += "💡 出行建议（基于以下规则）：\n";
instruction += "  - 气温<10°C → 穿厚外套\n";
instruction += "  - 气温10-20°C → 薄外套/长袖\n";
instruction += "  - 气温>30°C → 清凉衣物，注意防暑\n";
instruction += "  - 有雨(Rain/Drizzle/Shower) → 建议带伞\n";
instruction += "  - 有雪(Snow) → 防滑保暖\n";
instruction += "  - 紫外线>5 → 建议防晒\n";
instruction += "  - 风速>40km/h → 注意大风\n";

if aqi_data.len() > 50 {
    instruction += "\n\n另附空气质量数据（来自 aqicn.org）：\n" + aqi_data + "\n请同时展示 AQI 和 PM2.5 信息。";
}

return #{
    success: true,
    city: city_raw,
    source: "wttr.in",
    format: "json",
    data: weather_text,
    aqi: aqi_data,
    instruction: instruction
}
