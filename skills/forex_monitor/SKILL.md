# 外汇监控技能 (forex_monitor)

## 触发短语
- 汇率、外汇、换汇、美元、人民币、forex、exchange rate、currency
- USD、CNY、EUR、JPY、GBP

## 核心能力
通过 QVeris 获取实时汇率，支持货币换算和汇率走势查看。

## 工具调用顺序

### 场景 1: 查询汇率
1. `qveris` action='search_and_execute' query='{from_currency} to {to_currency} exchange rate'
2. 降级: `finance_api` action='forex_rate' from_currency='{from}' to_currency='{to}'

### 场景 2: 汇率走势
1. `qveris` action='search_and_execute' query='{from}/{to} exchange rate history'
2. 降级: `finance_api` action='forex_history' from_currency='{from}' to_currency='{to}'

### 场景 3: 货币换算
1. 获取汇率 (同场景1)
2. 计算: amount * rate

## 常用货币代码
| 用户说的 | 代码 |
|---------|------|
| 美元 | USD |
| 人民币 | CNY |
| 欧元 | EUR |
| 日元 | JPY |
| 英镑 | GBP |
| 港币 | HKD |
| 韩元 | KRW |

## 输出格式
```
💱 汇率查询: {from} → {to}
📊 当前汇率: 1 {from} = {rate} {to}
🕐 更新时间: {update_time}
💰 换算: {amount} {from} = {result} {to}
```
