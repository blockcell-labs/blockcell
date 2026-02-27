# 加密货币追踪技能 (crypto_tracker)

## 触发短语
- 币价、加密货币、比特币、以太坊、BTC、ETH、数字货币、虚拟货币
- crypto、bitcoin、ethereum、token price、coin price

## 核心能力
通过 QVeris + CoinGecko 获取加密货币实时价格、市值、历史走势、市场概览。

## 工具调用顺序

### 场景 1: 查询单个币种价格
1. `qveris` action='search_and_execute' query='{coin_name} price USD'
2. 降级: `finance_api` action='crypto_price' symbol='{coin_id}' vs_currency='usd'

### 场景 2: 市场概览
1. `qveris` action='search_and_execute' query='crypto market overview top coins'
2. 降级: `finance_api` action='market_overview'

### 场景 3: 历史走势
1. `qveris` action='search_and_execute' query='{coin} price history 30 days'
2. 降级: `finance_api` action='crypto_history' symbol='{coin_id}' interval='30d'
3. 可选: `chart_generate` 生成价格走势图

### 场景 4: 热门/趋势币
1. `finance_api` action='crypto_list' limit=20
2. `finance_api` action='market_overview' → trending coins

## 常用币种映射
| 用户说的 | CoinGecko ID |
|---------|-------------|
| 比特币/BTC | bitcoin |
| 以太坊/ETH | ethereum |
| 狗狗币/DOGE | dogecoin |
| SOL | solana |
| BNB | binancecoin |
| XRP/瑞波 | ripple |

## 输出格式
```
🪙 {coin_name} ({symbol})
💰 价格: ${price} | 24h变化: {change_24h}%
📊 市值: ${market_cap} | 24h交易量: ${volume_24h}
📈 7d变化: {change_7d}% | 30d变化: {change_30d}%
⚠️ 以上数据仅供参考，加密货币波动剧烈，请谨慎投资。
```
