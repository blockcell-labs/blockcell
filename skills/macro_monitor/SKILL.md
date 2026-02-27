# 宏观经济监控技能 (macro_monitor)

## 触发短语
- 宏观经济、经济数据、GDP、CPI、利率、国债收益率、央行、货币政策
- macro economy、economic data、interest rate、bond yield、treasury、inflation

## 核心能力
通过 QVeris 获取宏观经济指标（GDP/CPI/PMI/失业率）、央行利率决议、国债收益率曲线。

## 工具调用顺序

### 场景 1: 国债收益率
1. `qveris` action='search_and_execute' query='中国/美国国债收益率曲线'
2. 降级: `finance_api` action='bond_yield' bond_type='china_treasury'

### 场景 2: 宏观经济数据
1. `qveris` action='search_and_execute' query='中国最新GDP CPI PMI数据'
2. 降级: `qveris` action='search' query='macroeconomic indicators China'

### 场景 3: 汇率影响分析
1. `finance_api` action='forex_rate' from_currency='USD' to_currency='CNY'
2. `qveris` action='search_and_execute' query='人民币汇率走势分析'

## 输出格式
```
📊 宏观经济速览

🏦 利率 & 债券
- 中国10年期国债: {cn_10y}% | 美国10年期: {us_10y}%
- 中美利差: {spread}bp

📈 经济指标
- GDP增长: {gdp}% | CPI: {cpi}% | PMI: {pmi}
- 失业率: {unemployment}%

💱 汇率
- USD/CNY: {usdcny} | EUR/USD: {eurusd}

⚠️ 数据仅供参考。
```
