# 新闻摘要技能 (news_digest)

## 触发短语
- 新闻、资讯、热点、头条、最新消息、今日要闻、财经新闻、科技新闻
- news、headlines

## 核心能力
通过 QVeris 获取各类新闻资讯，支持按主题、地区、时间筛选。

## 工具调用顺序

### 场景 1: 通用新闻
1. `qveris` action='search_and_execute' query='today top news headlines'
2. 降级: `web_search` query='今日头条新闻'

### 场景 2: 财经新闻
1. `qveris` action='search_and_execute' query='financial news today stock market'
2. 降级: `web_search` query='财经新闻 今日'

### 场景 3: 特定主题新闻
1. `qveris` action='search_and_execute' query='{topic} latest news'
2. 降级: `web_search` query='{topic} 最新新闻'

## 输出格式
```
📰 新闻摘要 ({date})

1. **{title_1}**
   {summary_1}
   🔗 来源: {source_1}

2. **{title_2}**
   {summary_2}
   🔗 来源: {source_2}

...
```

## 降级策略
1. QVeris 不可用 → web_search 搜索新闻
2. 无法获取完整内容 → web_fetch 抓取新闻页面
