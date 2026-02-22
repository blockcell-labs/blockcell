# 第04篇：技能（Skill）系统 —— 用 Rhai 脚本扩展 AI 能力

> 系列文章：《blockcell 开源项目深度解析》第 4/14 篇

---

## 工具 vs 技能，有什么区别？

上一篇我们介绍了工具（Tool）。工具是原子操作，比如"读文件"、"搜网页"。

但实际任务往往是多步骤的：

```
监控茅台股价 = 
  每隔10分钟 → 查询股价 → 判断是否跌破阈值 → 发送告警 → 记录到日志
```

这种**多步骤、有逻辑的复合任务**，就是技能（Skill）要解决的问题。

**技能 = 封装了多个工具调用的可复用流程**

---

## 技能的组成

每个技能是一个目录，包含三个文件：

```
skills/stock_monitor/
├── meta.yaml      # 元数据：触发词、描述、权限
├── SKILL.md       # 操作手册：给 LLM 看的说明书
└── SKILL.rhai     # 编排脚本：确定性的执行逻辑
```

这三个文件各有分工：

| 文件 | 作用 | 谁来读 |
|------|------|--------|
| `meta.yaml` | 触发词匹配、权限声明 | 系统 |
| `SKILL.md` | 操作规范、参数说明、示例 | LLM |
| `SKILL.rhai` | 确定性编排逻辑 | Rhai 引擎 |

---

## meta.yaml：触发词与元数据

```yaml
name: stock_monitor
description: "A股/港股/美股实时行情监控与分析"
version: "1.0.0"
triggers:
  - "查股票"
  - "股价"
  - "行情"
  - "监控股票"
  - "stock price"
  - "stock quote"
permissions:
  - network
  - storage
```

当用户说"帮我查一下茅台的股价"，系统会匹配到 `stock_monitor` 技能，然后把 `SKILL.md` 注入到 LLM 的上下文中。

---

## SKILL.md：给 LLM 的操作手册

这是技能系统最有创意的设计之一。

`SKILL.md` 不是给人看的文档，而是**给 LLM 看的操作规范**。它告诉 LLM：
- 这个技能能做什么
- 应该调用哪些工具
- 参数怎么填
- 遇到错误怎么处理

```markdown
# 股票监控技能操作手册

## 数据源速查

| 市场 | 代码格式 | 工具调用 |
|------|---------|---------|
| A股沪市 | 6位数字，如 600519 | finance_api stock_quote source=eastmoney |
| A股深市 | 6位数字，如 000001 | finance_api stock_quote source=eastmoney |
| 港股 | 5位数字，如 00700 | finance_api stock_quote source=eastmoney |
| 美股 | 字母代码，如 AAPL | finance_api stock_quote |

## 常见股票代码

- 贵州茅台: 600519
- 中国平安: 601318
- 腾讯控股: 00700（港股）
- 苹果: AAPL

## 场景一：查询实时股价

步骤：
1. 调用 finance_api，action=stock_quote，symbol=股票代码
2. 返回：价格、涨跌幅、成交量、市盈率

## 场景二：查询历史走势

步骤：
1. 调用 finance_api，action=stock_history，symbol=股票代码，period=1mo
2. 可选：调用 chart_generate 画折线图
```

这种设计的好处是：**LLM 的行为可以通过修改 Markdown 文件来调整，不需要重新训练模型。**

---

## SKILL.rhai：确定性编排脚本

Rhai 是一个嵌入式脚本语言，语法类似 JavaScript/Rust，专为嵌入 Rust 程序设计。

`SKILL.rhai` 用于处理**确定性的逻辑**，比如：
- 参数校验
- 多步骤编排
- 错误处理和降级
- 结果格式化

```javascript
// SKILL.rhai 示例：股票监控

// 获取用户输入的股票代码
let symbol = ctx["symbol"];
if symbol == "" {
    set_output("请提供股票代码，例如：600519（茅台）");
    return;
}

// 查询实时行情
let quote_result = call_tool("finance_api", #{
    "action": "stock_quote",
    "symbol": symbol
});

if is_error(quote_result) {
    // 降级：尝试用 web_search 搜索
    log_warn("finance_api 失败，尝试 web_search");
    let search_result = call_tool("web_search", #{
        "query": `${symbol} 股价 今日`
    });
    set_output(search_result);
    return;
}

// 格式化输出
let price = get_field(quote_result, "price");
let change = get_field(quote_result, "change_pct");
set_output(`${symbol} 当前价格：${price}，涨跌幅：${change}%`);
```

Rhai 脚本里可以调用任意内置工具（通过 `call_tool` 函数），也可以做条件判断、循环、错误处理。

---

## 内置了哪些技能

blockcell 内置了 40+ 技能，主要分几类：

### 金融类（16 个）
```
stock_monitor      - A股/港股/美股行情
bond_monitor       - 债券市场监控
futures_monitor    - 期货衍生品
crypto_research    - 加密货币研究
token_security     - 代币安全检测
whale_tracker      - 巨鲸追踪
address_monitor    - 链上地址监控
nft_analysis       - NFT 分析
defi_analysis      - DeFi 分析
contract_audit     - 合约审计
wallet_security    - 钱包安全
crypto_sentiment   - 市场情绪
dao_analysis       - DAO 分析
crypto_tax         - 加密税务
quant_crypto       - 量化策略
treasury_management - 资金管理
```

### 系统控制类（3 个）
```
camera             - 摄像头拍照
app_control        - macOS 应用控制
chrome_control     - Chrome 浏览器控制
```

### 综合类
```
daily_finance_report - 每日金融日报
stock_screener       - 股票筛选
portfolio_advisor    - 投资组合建议
```

---

## 如何创建自己的技能

### 方法一：直接告诉 AI

```
你: 帮我创建一个技能，每天早上 8 点查询茅台和平安的股价，
    如果任何一个跌超 3%，发 Telegram 消息给我
```

AI 会自动生成 `meta.yaml`、`SKILL.md`、`SKILL.rhai` 三个文件，保存到 `~/.blockcell/workspace/skills/` 目录。

### 方法二：手动创建

```bash
mkdir -p ~/.blockcell/workspace/skills/my_monitor
```

创建 `meta.yaml`：
```yaml
name: my_monitor
description: "我的自定义监控"
version: "1.0.0"
triggers:
  - "我的监控"
  - "自定义监控"
```

创建 `SKILL.md`：
```markdown
# 我的监控技能

## 功能
监控指定股票，跌超阈值时发送通知

## 参数
- symbol: 股票代码
- threshold: 跌幅阈值（百分比）
```

创建 `SKILL.rhai`：
```javascript
let symbol = ctx["symbol"] ?? "600519";
let threshold = ctx["threshold"] ?? 3.0;

let quote = call_tool("finance_api", #{
    "action": "stock_quote",
    "symbol": symbol
});

let change = get_field(quote, "change_pct");
if change < -threshold {
    call_tool("notification", #{
        "channel": "telegram",
        "message": `⚠️ ${symbol} 跌幅 ${change}%，超过阈值 ${threshold}%`
    });
}
```

### 方法三：从社区仓库安装

```
你: 帮我从社区仓库搜索并安装一个 DeFi 监控技能
```

AI 会调用 `community_hub` 工具搜索并下载技能。

---

## 技能热重载

当你通过 AI 对话创建或修改技能文件时，blockcell 会**自动检测文件变化并热重载**，不需要重启。

```
你: 帮我修改 my_monitor 技能，把阈值改成 5%
AI: 修改 SKILL.rhai 文件...
    [系统自动检测到技能更新，已热重载 my_monitor]
```

这个功能在 `runtime.rs` 中实现：每次 `write_file` 或 `edit_file` 成功后，如果路径在 skills 目录内，就触发重载并通过 WebSocket 通知 Dashboard。

---

## 技能 vs 工具：什么时候用哪个

| 场景 | 用工具 | 用技能 |
|------|--------|--------|
| 一次性操作 | ✅ | |
| 多步骤流程 | | ✅ |
| 需要复用 | | ✅ |
| 需要降级策略 | | ✅ |
| 需要定时执行 | | ✅ |
| 简单查询 | ✅ | |

---

## Rhai 语言简介

如果你没用过 Rhai，这里是一个快速入门：

```javascript
// 变量
let x = 42;
let name = "blockcell";

// 条件
if x > 10 {
    print("大于10");
} else {
    print("不大于10");
}

// 循环
for i in 0..5 {
    print(i);
}

// Map（类似 JSON 对象）
let params = #{
    "action": "stock_quote",
    "symbol": "600519"
};

// 调用工具（blockcell 特有）
let result = call_tool("finance_api", params);

// 错误处理
if is_error(result) {
    log_warn("调用失败");
    return;
}

// 获取字段
let price = get_field(result, "price");
```

Rhai 的语法非常简单，即使没有编程经验也能快速上手。

---

## 小结

技能系统是 blockcell 的"软件层"：

- **`meta.yaml`** 定义触发条件
- **`SKILL.md`** 给 LLM 提供操作规范
- **`SKILL.rhai`** 实现确定性编排逻辑

这三层设计让技能既灵活（LLM 可以自由发挥）又可控（关键逻辑由脚本保证）。

下一篇，我们来看记忆系统——blockcell 如何用 SQLite + FTS5 让 AI 拥有持久记忆。
---

*上一篇：[blockcell 的工具系统 —— 让 AI 真正能干活](./03_tools_system.md)*
*下一篇：[记忆系统 —— 让 AI 记住你说过的话](./05_memory_system.md)*

*项目地址：https://github.com/blockcell-labs/blockcell*
*官网：https://blockcell.dev*
