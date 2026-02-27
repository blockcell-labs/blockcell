# 股票分析技能 (stock_analysis)

> **零 API Key** — 所有数据来自东方财富免费接口，无需配置任何密钥即可运行。

## 触发短语
- 股票分析、分析股票、看看股票、股价、行情、K线、基本面、研报
- 涨停、跌停、涨停原因、龙虎榜、涨停板、主力资金、资金流向
- 大盘、板块、市值、财报、年报、季报、机构持仓、北向资金
- stock analysis、stock quote、stock price、analyze stock
- 查询股票、股票走势、个股分析、技术分析、A股、港股、美股
- 宏观经济、CPI、PMI、GDP、社融、M2

## ⚠️ 第一步: 未知代码先搜索
用户给出公司名（非数字代码）时，**必须先调用**:
```
finance_api action='stock_search' query='公司名'
```
- **找到**: 使用返回的代码继续分析
- **未找到/未上市**: 告知用户该公司尚未上市，然后:
  1. `web_search` 搜索该公司行业、背景、融资信息
  2. `finance_api` action='stock_screen' 筛选同行业已上市概念股
  3. 分析相关概念股，作为投资参考

## 数据源速查 (全部免费)
| 数据 | 工具调用 |
|------|----------|
| 搜索股票代码 | `finance_api` action='stock_search' query='摩尔线程' |
| 实时行情 | `finance_api` action='stock_quote' symbol='601318' |
| K线历史 | `finance_api` action='stock_history' symbol='601318' interval='1d' |
| 资金流向 | `finance_api` action='capital_flow' symbol='601318' |
| 北向资金 | `finance_api` action='northbound_flow' period='10d' |
| 行业资金 | `finance_api` action='industry_fund_flow' |
| 龙虎榜 | `finance_api` action='top_list' list_type='dragon_tiger' |
| 涨停板 | `finance_api` action='top_list' list_type='limit_up' |
| 个股新闻 | `finance_api` action='stock_news' symbol='601318' |
| 财务报表 | `finance_api` action='financial_statement' symbol='601318' report_type='indicator' |
| 机构持仓 | `finance_api` action='institutional_holdings' symbol='601318' |
| 宏观数据 | `finance_api` action='macro_data' indicator='cpi' |
| 选股 | `finance_api` action='stock_screen' screen_filters={pe_max:20, board:'创业板'} |

## 股票代码
- A股 6位: 601318=中国平安, 600519=贵州茅台, 300750=宁德时代, 002594=比亚迪, 600036=招商银行
- 港股 5位+.HK: 00700.HK=腾讯, 09988.HK=阿里, 03690.HK=美团
- 美股: AAPL, MSFT, TSLA, NVDA, AMZN

## 场景识别规则
- 含「涨停/龙虎榜/涨停板」→ **场景1: 涨停分析**
- 含「资金/主力/北向」→ **场景2: 资金流向分析**
- 含「新闻/消息/研报/原因/为什么」→ **场景3: 新闻研报**
- 含「宏观/CPI/PMI/GDP/社融」→ **场景4: 宏观数据**
- 含「大盘/行业/板块」→ **场景5: 大盘/行业分析**
- 含「财务/基本面/年报/季报/机构」→ **场景6: 财务基本面**
- 其他 → **场景0: 实时行情+技术分析**

## 工具调用顺序

### 场景0: 实时行情+技术分析 (默认)
1. `finance_api` action='stock_quote' symbol='{code}' → 获取实时价格、PE/PB、市值
2. `finance_api` action='stock_history' symbol='{code}' interval='1d' → 获取60日K线
3. 基于K线数据本地计算技术指标:
   - MA5/MA10/MA20/MA60: 近N日收盘价均值
   - MACD(12,26,9): EMA12-EMA26=DIF, EMA9(DIF)=DEA, DIF-DEA=BAR
   - RSI(14): 100 - 100/(1 + avg_gain/avg_loss)
4. 组合输出行情+技术分析报告

### 场景1: 涨停原因分析
1. `finance_api` action='stock_quote' symbol='{code}' → 确认涨停状态
2. `finance_api` action='stock_news' symbol='{code}' limit=10 → 最新新闻
3. `finance_api` action='top_list' list_type='dragon_tiger' → 龙虎榜
4. `finance_api` action='capital_flow' symbol='{code}' → 主力资金
5. `web_search` query='{stock_name} 涨停原因 {today}' → 补充最新消息
6. 综合分析给出涨停原因报告

### 场景2: 资金流向分析
1. `finance_api` action='capital_flow' symbol='{code}' period='5d' → 个股5日资金
2. `finance_api` action='northbound_flow' period='10d' → 北向资金趋势
3. `finance_api` action='industry_fund_flow' → 所属行业资金排名

### 场景3: 新闻研报
1. `finance_api` action='stock_news' symbol='{code}' limit=20
2. `web_search` query='{stock_name} 研报 最新分析' freshness=week
3. `web_fetch` 获取重要文章正文

### 场景4: 宏观数据
1. `finance_api` action='macro_data' indicator='{indicator}' limit=12
2. 可用 indicator: gdp, cpi, ppi, pmi_manufacturing, pmi_services, social_financing, m2, lpr, rrr, retail_sales, industrial_output, trade_balance

### 场景5: 大盘/行业分析
1. `finance_api` action='market_overview' → 大盘概览
2. `finance_api` action='top_list' list_type='money_flow' → 资金流向排名
3. `finance_api` action='industry_fund_flow' → 行业资金流向
4. `finance_api` action='northbound_flow' → 北向资金

### 场景6: 财务基本面
1. `finance_api` action='financial_statement' symbol='{code}' report_type='indicator' years=3
2. `finance_api` action='institutional_holdings' symbol='{code}'
3. `finance_api` action='dividend_history' symbol='{code}'

## 技术指标计算公式 (本地计算，无需额外工具)
```
# MA
ma_n = sum(closes[-n:]) / n

# MACD (12,26,9)
ema12 = EMA(closes, 12)  # 12日指数均线
ema26 = EMA(closes, 26)
dif = ema12[-1] - ema26[-1]
dea = EMA(dif_series, 9)
macd_bar = (dif - dea) * 2

# RSI (14)
gains = [max(c-p, 0) for c,p in zip(closes[1:], closes)]
losses = [max(p-c, 0) for c,p in zip(closes[1:], closes)]
rs = avg(gains[-14:]) / avg(losses[-14:])
rsi = 100 - 100 / (1 + rs)
```

## 输出格式

### 场景0: 常规行情分析
```
📊 {股票名} ({代码}) 分析报告
═══════════════════════════
💰 实时行情
  现价: {price} | 涨跌: {change}% ({change_amount})
  今开: {open} | 最高: {high} | 最低: {low}
  成交量: {volume}手 | 成交额: {amount}亿
  换手率: {turnover}% | 市盈率PE: {pe} | 市净率PB: {pb}
  总市值: {market_cap}亿 | 涨停: {limit_up} | 跌停: {limit_down}

� 技术面 (基于近60日K线)
  MA5={ma5} MA20={ma20} MA60={ma60}
  MACD: DIF={dif:.2f} DEA={dea:.2f} BAR={bar:.2f} ({trend})
  RSI(14): {rsi:.1f} ({overbought_oversold})
  趋势判断: {trend_summary}

⚠️ 风险提示: 以上数据仅供参考，不构成投资建议。
```

### 场景1: 涨停分析
```
� {股票名} ({代码}) 涨停分析
═══════════════════════════
� 今日行情: +10.00% (涨停) | 成交额: {amount}亿

� 涨停原因 (来源: 财联社/东方财富)
  {reason_1}
  {reason_2}

🐉 龙虎榜席位
  买方: {buyers}
  卖方: {sellers}

💰 主力资金: 净流入 {main_net}亿

⚠️ 风险提示: 涨停板存在封死风险，高溢价追板需谨慎。
```

## 降级策略
1. `finance_api` 网络超时 → 重试1次，再失败则用 `web_search` 搜索
2. `stock_news` 返回空 → 补充 `web_search` query='{股票名} 最新消息'
3. 美股/港股 → `finance_api` 自动路由 (Yahoo/Alpha Vantage)
