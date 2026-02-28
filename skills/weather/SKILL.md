# 天气查询技能 (weather)

> **零 API Key** — 使用 wttr.in（全球免费天气API）+ 中国天气网，直接获取实时天气和预报，无需配置任何密钥。

## 触发短语
- 天气、天气预报、今天天气、明天天气、后天天气、一周天气
- 气温、温度、下雨、下雪、晴天、阴天、湿度、风力
- 空气质量、AQI、PM2.5、紫外线
- 要带伞吗、穿什么衣服、会下雨吗、出行建议
- weather、forecast、rain、snow、temperature

## 数据源（零 API Key）

| 来源 | URL格式 | 特点 |
|------|---------|------|
| wttr.in JSON | `https://wttr.in/{城市}?format=j1` | 全球覆盖，JSON格式，含3天预报 |
| wttr.in 文本 | `https://wttr.in/{城市}?lang=zh` | 中文显示，含图形天气 |
| 中国天气网 | `https://www.weather.com.cn/weather/{城市code}.shtml` | 国内更精准 |

## 核心策略

**首选 wttr.in JSON API**（最可靠、零 Key、全球覆盖）：
```
https://wttr.in/{城市英文名或中文}?format=j1
```

**城市名处理规则**：
- 中文城市 → 直接传中文（wttr.in 支持）或转拼音
  - 北京 → Beijing，上海 → Shanghai，深圳 → Shenzhen
  - 广州 → Guangzhou，成都 → Chengdu，杭州 → Hangzhou
  - 武汉 → Wuhan，南京 → Nanjing，西安 → Xian
- 外国城市 → 直接传英文名

## 工具调用顺序

### 场景A: 国内城市天气（默认）
1. `web_fetch` url=`https://wttr.in/{城市}?format=j1` → JSON格式天气数据
2. 如果 wttr.in 失败，备用：`web_fetch` url=`https://wttr.in/{城市}?lang=zh` → 文本格式

### 场景B: 空气质量查询
1. `web_fetch` url=`https://wttr.in/{城市}?format=j1` → 获取基础天气
2. `web_fetch` url=`https://aqicn.org/city/{城市}/` → AQI/PM2.5数据（免费）

### 场景C: 多天预报（未来一周）
1. `web_fetch` url=`https://wttr.in/{城市}?format=j1` → 包含3天详细预报
2. 从JSON中提取 weather[0]/[1]/[2] 对应今天/明天/后天

## wttr.in JSON 字段说明
```json
{
  "current_condition": [{
    "temp_C": "当前气温(摄氏)",
    "FeelsLikeC": "体感温度",
    "humidity": "湿度%",
    "windspeedKmph": "风速km/h",
    "winddir16Point": "风向",
    "weatherDesc": [{"value": "天气描述"}],
    "uvIndex": "紫外线指数",
    "visibility": "能见度km",
    "pressure": "气压hPa"
  }],
  "weather": [
    {
      "date": "YYYY-MM-DD",
      "maxtempC": "最高温",
      "mintempC": "最低温",
      "hourly": [...],
      "astronomy": [{"sunrise": "日出", "sunset": "日落"}]
    }
  ],
  "nearest_area": [{"areaName": [{"value": "区域名"}]}]
}
```

## 输出格式

```
🌤️ {城市} 天气 — {日期} {时间}
════════════════════════════

☀️ 当前天气
  天气：{描述} | 气温：{temp}°C（体感 {feels_like}°C）
  湿度：{humidity}% | 风速：{wind}km/h {wind_dir}
  能见度：{vis}km | 紫外线：{uv} | 气压：{pressure}hPa

📅 未来3天预报
  今天({date})：{desc} {min}°C ~ {max}°C
  明天({date})：{desc} {min}°C ~ {max}°C
  后天({date})：{desc} {min}°C ~ {max}°C

💡 出行建议
  {根据天气给出穿衣/带伞/防晒建议}
```

## 出行建议规则（本地计算）
- 温度 < 10°C → 建议穿厚外套
- 温度 10-20°C → 建议穿薄外套或长袖
- 温度 > 30°C → 建议穿清凉衣物，注意防暑
- weatherDesc 含 Rain/Drizzle/Shower/雨 → 建议带伞
- weatherDesc 含 Snow/雪 → 建议防滑保暖
- uvIndex > 5 → 建议防晒（涂防晒霜/带帽/遮阳）
- windspeedKmph > 40 → 提醒大风天气

## 降级策略
1. wttr.in JSON 失败 → 尝试 `https://wttr.in/{城市}?lang=zh` 文本格式
2. 文本格式也失败 → 返回错误，提示用户检查城市名拼写
3. 城市名无法识别 → 提示用户提供城市的英文名或拼音
