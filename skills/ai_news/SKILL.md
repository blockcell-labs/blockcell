# AI新闻聚合技能 (ai_news)

> **零搜索引擎** — 直接抓取权威AI科技媒体页面，按日期去重排序，获取最新AI资讯。

## 触发短语
- AI新闻、AI资讯、人工智能新闻、科技新闻、AI动态、AI热点
- 最新AI、今日AI、今天AI、最新大模型、大模型新闻、LLM新闻
- 36氪、机器之心、量子位
- AI news、latest AI、artificial intelligence news

## 数据源（按优先级）

| 来源 | URL | 特点 |
|------|-----|------|
| 36氪 AI频道 | `https://36kr.com/information/AI` | 中文AI创投新闻 |
| 机器之心 | `https://www.jiqizhixin.com/` | AI学术/产业深度 |
| 量子位 | `https://www.qbitai.com/` | AI产品/政策/研究 |
| InfoQ AI | `https://www.infoq.cn/topic/AI` | 技术向AI新闻 |
| 新智元 | `https://news.qq.com/ch/ai.htm` | AI综合资讯 |

## 工具调用顺序

1. **并发抓取多个来源**（用多次 `web_fetch`）:
   - `web_fetch` url='https://36kr.com/information/AI'
   - `web_fetch` url='https://www.jiqizhixin.com/'
   - `web_fetch` url='https://www.qbitai.com/'

2. **提取文章列表**: 从每个页面的 Markdown 内容中提取文章标题、链接、日期

3. **去重**: 按标题相似度去重（标题前20字相同视为重复）

4. **按日期排序**: 最新日期优先，无明确日期的排在末尾

5. **选取 Top 15-20 条**: 覆盖多来源，避免单一来源垄断

## 输出格式

```
📰 AI 最新资讯 (YYYY-MM-DD HH:MM)
════════════════════════════════

**来源: 36氪**
1. [文章标题](URL) — YYYY-MM-DD
   摘要：一句话简介...

2. [文章标题](URL) — YYYY-MM-DD
   摘要：...

**来源: 机器之心**
3. [文章标题](URL) — YYYY-MM-DD
   摘要：...

...（共 N 条，已去重）

---
数据来源：36氪 | 机器之心 | 量子位 | 抓取时间: HH:MM
```

## 场景识别规则
- 含「36氪/机器之心/量子位」特定来源关键词 → 优先抓对应站点
- 含「今天/今日/最新」→ 强调日期过滤，优先24小时内内容
- 含「大模型/LLM/GPT/Claude」→ 提示LLM相关新闻优先展示
- 含「国内/中文」→ 主用36氪+机器之心+量子位
- 含「英文/国际/global」→ 提示可补充 web_fetch 抓取 techcrunch.com/tag/artificial-intelligence

## 降级策略
1. 某个站点抓取失败（反爬/超时）→ 跳过该站点，继续其他站点
2. 全部站点失败 → 返回错误提示，建议用户稍后重试
3. 抓取内容为空或太短（< 200字）→ 视为失败，跳过
