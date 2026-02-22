# Article 10: Finance in Practice — Monitoring Stocks and Crypto with blockcell

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — 10/14

---

## Why finance is a great fit for AI agents

Financial data has several traits that make it ideal for AI agents:

1. **High volume**: thousands of quote records daily; manual processing is inefficient
2. **Real-time**: prices change every second and need continuous monitoring
3. **Multi-dimensional analysis**: technical + fundamental + news sentiment
4. **Highly repetitive**: daily workflows are consistent and easy to automate

blockcell ships with a complete financial toolchain. This article walks through real scenarios.

---

## Data source overview

blockcell’s finance tools support multiple data sources, most of which are **free and require no API key**:

| Data | Source | Free? |
|---------|--------|---------|
| CN/HK real-time quotes | Eastmoney | ✅ |
| CN A-share historical K-lines | Eastmoney | ✅ |
| US stock quotes | Yahoo Finance | ✅ |
| US stock history | Alpha Vantage | API key needed (free tier) |
| Crypto prices | CoinGecko | ✅ |
| Crypto history | CoinGecko | ✅ |
| Treasury yields | Eastmoney | ✅ |
| FX rates | open.er-api.com | ✅ |
| On-chain data | public RPC endpoints | ✅ |

---

## Scenario 1: check real-time stock prices

The simplest usage:

```
You: Check Moutai’s stock price today
```

The AI recognizes “Moutai” as `600519` and calls Eastmoney:

```
Moutai (600519) real-time quote:
Price: 1,680.00 CNY
Change: +1.23% (+20.40 CNY)
Volume: 1,234,567 lots
Turnover: 2.074B CNY
P/E: 28.5
P/B: 8.2
52w high: 1,850.00
52w low: 1,420.00
```

### Common stock symbols

blockcell has built-in mappings for common symbols:

| Name | Symbol | Market |
|---------|------|------|
| Kweichow Moutai | 600519 | CN A-share (SH) |
| Ping An | 601318 | CN A-share (SH) |
| CATL | 300750 | CN A-share (SZ) |
| Tencent | 00700 | HK |
| Alibaba | 09988 | HK |
| Apple | AAPL | US |
| NVIDIA | NVDA | US |

---

## Scenario 2: K-line analysis and technical indicators

```
You: Analyze Moutai’s last 3 months. Compute MA20 and MACD.
```

Execution steps:

```
1. finance_api stock_history symbol=600519 period=3mo
   → fetch ~60 trading days of OHLCV

2. Compute indicators locally:
   MA20 = average close over last 20 days
   MACD = EMA12 - EMA26
   Signal = 9-day EMA of MACD
   Histogram = MACD - Signal

3. chart_generate line: price + MA20
4. chart_generate bar: MACD histogram
```

Example output:

```
Moutai (600519) 3-month technical analysis:

Price:
- Current: 1,680 CNY
- 3M change: +8.5%
- High: 1,780 (2025-01-15)
- Low: 1,520 (2024-11-20)

Moving averages:
- MA5: 1,672 (short-term support)
- MA20: 1,645 (mid-term support)
- MA60: 1,598 (long-term support)
Price is above all three → bullish trend.

MACD (12,26,9):
- MACD: +12.5
- Signal: +8.3
- Histogram: +4.2 (positive)
Momentum remains strong after a bullish crossover.

Conclusion: technicals are strong, but watch 1,700 as resistance.

[Chart generated: ~/Desktop/maotai_analysis.png]
```

---

## Scenario 3: price alerts

```
You: Set an alert:
    if Moutai drops below 1600, notify me on Telegram;
    if Bitcoin goes above 100k USD, also notify me
```

The AI will create two alert rules.

**Alert 1: Moutai below 1600**

```json
{
  "name": "Moutai below 1600",
  "check_interval_secs": 300,
  "condition": {
    "tool": "finance_api",
    "params": {"action": "stock_quote", "symbol": "600519"},
    "field": "price",
    "operator": "lt",
    "threshold": 1600
  },
  "on_trigger": [{
    "tool": "notification",
    "params": {
      "channel": "telegram",
      "message": "⚠️ Moutai is below 1600! Current price: {value} CNY"
    }
  }]
}
```

**Alert 2: BTC above 100k**

```json
{
  "name": "BTC above 100k",
  "check_interval_secs": 60,
  "condition": {
    "tool": "finance_api",
    "params": {"action": "crypto_price", "symbol": "bitcoin"},
    "field": "price_usd",
    "operator": "gt",
    "threshold": 100000
  },
  "on_trigger": [{
    "tool": "notification",
    "params": {
      "channel": "telegram",
      "message": "🚀 BTC crossed 100k USD! Current price: ${value}"
    }
  }]
}
```

Alert rules are persisted and restored automatically after restart.

---

## Scenario 4: daily finance report

blockcell includes a built-in skill `daily_finance_report` that generates a daily report.

```
You: Generate today’s finance report
```

A report typically includes:

```
📊 Daily Finance Report 2025-02-18

[Indexes]
SSE: 3,350.25 (+0.85%)
SZSE: 10,890.50 (+1.20%)
ChiNext: 2,180.30 (+1.50%)

[Fund Flows]
Northbound: +4.56B CNY
Margin balance: 1.5234T CNY (+2.3B)

[Hot Sectors]
Top gainers: AI compute (+3.2%), robotics (+2.8%), semiconductors (+2.1%)
Top losers: real estate (-1.5%), banks (-0.8%), coal (-0.5%)

[Watchlist]
Moutai (600519): 1,680 (+1.23%) ✅
Ping An (601318): 42.50 (-0.35%)
CATL (300750): 185.20 (+2.10%) ✅

[Crypto]
BTC: $68,500 (+2.3%)
ETH: $3,850 (+1.8%)
Sentiment: Greed (index: 72)

[Today’s focus]
- FOMC minutes release at 03:00 Beijing time
- Policy expectations rising ahead of major meetings
```

### Schedule daily sending

```
You: Every day at 8:30, generate the daily finance report
    and send it to me via Telegram
```

The AI creates a cron job:

```bash
blockcell cron list
# daily_report  30 8 * * *  ✓ 08:30 daily
```

---

## Scenario 5: on-chain monitoring

blockcell can query blockchain data directly:

```
You: On Ethereum, what’s the current price in Uniswap V3’s ETH/USDC pool?
```

A typical flow:

```
1. blockchain_rpc eth_call
   Contract: 0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640
   Method: slot0()
   Chain: ethereum

2. Decode return values and compute the price
   ETH price: $3,852.40
   Pool liquidity: $1.2B
   24h volume: $450M
```

### Monitor a whale address

```
You: Monitor this address’s USDT balance:
    0x742d35Cc6634C0532925a3b844Bc454e4438f44e
    Tell me if it exceeds 10 million
```

The AI creates an on-chain alert rule:

```json
{
  "name": "Whale USDT monitor",
  "condition": {
    "tool": "blockchain_rpc",
    "params": {
      "action": "eth_call",
      "contract": "0xdAC17F958D2ee523a2206206994597C13D831ec7",
      "function_sig": "balanceOf(address)",
      "args": ["0x742d35Cc6634C0532925a3b844Bc454e4438f44e"]
    },
    "operator": "gt",
    "threshold": 10000000000000
  }
}
```

---

## Scenario 6: portfolio analysis

```
You: I hold:
    Moutai 10 shares, Ping An 100 shares, CATL 50 shares,
    BTC 0.5, ETH 2.
    Help me analyze my portfolio.
```

The AI will:
1. Query current prices for all assets
2. Compute total value and allocation percentages
3. Analyze sector concentration
4. Compute overall P/L change
5. Generate a pie chart for allocation

Example output:

```
Portfolio analysis report:

Total value: ~285,000 CNY

Allocation:
┌─────────────┬──────────┬──────────┬──────────┐
│ Asset       │ Amount   │ Price    │ Value    │
├─────────────┼──────────┼──────────┼──────────┤
│ Moutai      │ 10 sh    │ 1,680    │ 16,800   │
│ Ping An     │ 100 sh   │ 42.50    │ 4,250    │
│ CATL        │ 50 sh    │ 185.20   │ 9,260    │
│ BTC         │ 0.5      │ $68,500  │ ≈248,000 │
│ ETH         │ 2        │ $3,850   │ ≈55,700  │
└─────────────┴──────────┴──────────┴──────────┘

Risk notes:
- Crypto is 87% of the portfolio → high volatility risk
- CN A-share exposure is relatively small → consider increasing
```

---

## Scenario 7: real-time market subscriptions

For millisecond-level real-time data, use WebSocket subscriptions:

```
You: Subscribe to Binance BTC/USDT real-time trades
```

```
1. stream_subscribe subscribe
   url=wss://stream.binance.com:9443/ws/btcusdt@trade
   name=btc_trade

2. Read latest data every 5 seconds
   stream_subscribe read name=btc_trade count=10

3. Analyze last 10 trades:
   Last price: $68,523.50
   Buy/sell ratio: 6:4 (buyers slightly stronger)
   Largest trade: 2.3 BTC
```

Subscriptions are persisted and auto-restored after restart.

---

## A complete quant workflow

Combining the scenarios above yields a full quant analysis workflow:

```
Every day 08:30:
1. Generate daily finance report (indexes + hot sectors + watchlist)
2. Evaluate alert rules (breakouts, abnormal volatility)
3. Update portfolio analysis (P/L)
4. Send report via Telegram

Every hour:
5. Check on-chain whale movements
6. Monitor DeFi liquidation risk
7. Update technical indicators

Real-time:
8. Subscribe to key prices via WebSocket
9. Notify immediately when alerts trigger
```

This workflow can be fully automated using Cron + alerts + channels.

---

## Summary

blockcell’s finance capabilities include:

- **CN/HK/US stocks**: Eastmoney + Yahoo Finance (mostly free)
- **Crypto**: CoinGecko + exchange APIs + on-chain RPC
- **Technical analysis**: MA/MACD/RSI/KDJ/BOLL computations
- **Alerting**: breakouts, price moves, on-chain events
- **Real-time streams**: WebSocket subscriptions with persistence/reconnect
- **Automated reports**: scheduled daily reports pushed to Telegram

Whether you’re a quantitative trader, an investor, or simply tracking markets, blockcell provides powerful automation.

---

*Previous: [Self-evolution — how AI writes code to upgrade itself](./09_self_evolution.md)*
*Next: [Subagents and task concurrency — letting AI do multiple things at once](./11_subagents.md)*

*Repo: https://github.com/blockcell-labs/blockcell*
*Website: https://blockcell.dev*
