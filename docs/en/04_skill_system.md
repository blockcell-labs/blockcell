# Article 04: The Skill System — Extending AI Capabilities with Rhai Scripts

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — 4/14

---

## Tools vs skills — what’s the difference?

In the previous article we covered tools. Tools are atomic actions like “read a file” or “search the web”.

But real tasks are often multi-step:

```
Monitor Moutai stock price =
  every 10 minutes → query price → check threshold → send alert → write logs
```

These **multi-step tasks with logic and branching** are what skills are meant to handle.

**A skill = a reusable workflow that encapsulates multiple tool calls**

---

## What a skill contains

Each skill is a directory containing three files:

```
skills/stock_monitor/
├── meta.yaml      # metadata: triggers, description, permissions
├── SKILL.md       # playbook: instructions for the LLM
└── SKILL.rhai     # orchestration: deterministic execution logic
```

These files have different responsibilities:

| File | Purpose | Read by |
|------|---------|---------|
| `meta.yaml` | trigger matching, permission declarations | system |
| `SKILL.md` | operating rules, parameters, examples | LLM |
| `SKILL.rhai` | deterministic orchestration logic | Rhai engine |

---

## meta.yaml: triggers and metadata

```yaml
name: stock_monitor
description: "Real-time quote monitoring and analysis for CN/HK/US stocks"
version: "1.0.0"
triggers:
  - "check stocks"
  - "stock price"
  - "quote"
  - "monitor stocks"
permissions:
  - network
  - storage
```

When a user says “check Moutai’s stock price”, the system matches the `stock_monitor` skill and injects its `SKILL.md` into the LLM context.

---

## SKILL.md: an operating manual for the LLM

This is one of the most creative parts of the design.

`SKILL.md` is not documentation for humans — it’s an **operating playbook for the LLM**. It tells the model:
- What the skill can do
- Which tools to call
- How to fill parameters
- How to handle errors

```markdown
# Stock monitoring skill playbook

## Quick data source guide

| Market | Code format | Tool calls |
|------|---------|---------|
| CN A-share (Shanghai) | 6 digits, e.g. 600519 | finance_api stock_quote source=eastmoney |
| CN A-share (Shenzhen) | 6 digits, e.g. 000001 | finance_api stock_quote source=eastmoney |
| HK stocks | 5 digits, e.g. 00700 | finance_api stock_quote source=eastmoney |
| US stocks | symbols, e.g. AAPL | finance_api stock_quote |

## Common symbols

- Kweichow Moutai: 600519
- Ping An: 601318
- Tencent: 00700 (HK)
- Apple: AAPL

## Scenario 1: real-time quote

Steps:
1. Call finance_api, action=stock_quote, symbol=code
2. Return: price, change %, volume, PE

## Scenario 2: historical trend

Steps:
1. Call finance_api, action=stock_history, symbol=code, period=1mo
2. Optional: call chart_generate to draw a line chart
```

The advantage: **you can shape LLM behavior by editing a Markdown file — without retraining the model.**

---

## SKILL.rhai: deterministic orchestration scripts

Rhai is an embedded scripting language with a JavaScript/Rust-like syntax, designed for embedding into Rust programs.

`SKILL.rhai` handles **deterministic logic**, such as:
- Parameter validation
- Multi-step orchestration
- Error handling and graceful degradation
- Result formatting

```javascript
// Example SKILL.rhai: stock monitoring

// Get the stock symbol from user context
let symbol = ctx["symbol"];
if symbol == "" {
    set_output("Please provide a stock symbol, e.g. 600519 (Moutai)");
    return;
}

// Fetch real-time quote
let quote_result = call_tool("finance_api", #{
    "action": "stock_quote",
    "symbol": symbol
});

if is_error(quote_result) {
    // Degrade: try searching the web
    log_warn("finance_api failed, trying web_search");
    let search_result = call_tool("web_search", #{
        "query": `${symbol} stock price today`
    });
    set_output(search_result);
    return;
}

// Format output
let price = get_field(quote_result, "price");
let change = get_field(quote_result, "change_pct");
set_output(`${symbol} price: ${price}, change: ${change}%`);
```

In Rhai scripts, you can call any built-in tool (via `call_tool`), and you can implement branching, loops, and error handling.

---

## What skills are built in?

blockcell includes 40+ skills, broadly grouped as:

### Finance (16)
```
stock_monitor       - CN/HK/US stock quotes
bond_monitor        - bond market monitoring
futures_monitor     - futures & derivatives
crypto_research     - crypto research
token_security      - token security checks
whale_tracker       - whale tracking
address_monitor     - on-chain address monitoring
nft_analysis        - NFT analysis
defi_analysis       - DeFi analysis
contract_audit      - smart contract auditing
wallet_security     - wallet security
crypto_sentiment    - market sentiment
dao_analysis        - DAO analysis
crypto_tax          - crypto taxation
quant_crypto        - quantitative strategies
treasury_management - treasury management
```

### System control (3)
```
camera              - take a photo via camera
app_control          - macOS application control
chrome_control       - Chrome browser control
```

### General
```
daily_finance_report - daily finance report
stock_screener       - stock screening
portfolio_advisor    - portfolio advice
```

---

## How to create your own skill

### Method 1: just tell the AI

```
You: Create a skill that checks Moutai and Ping An every day at 8am.
    If either drops more than 3%, send me a Telegram message.
```

The AI will generate `meta.yaml`, `SKILL.md`, and `SKILL.rhai`, and save them under `~/.blockcell/workspace/skills/`.

### Method 2: create manually

```bash
mkdir -p ~/.blockcell/workspace/skills/my_monitor
```

Create `meta.yaml`:
```yaml
name: my_monitor
description: "My custom monitor"
version: "1.0.0"
triggers:
  - "my monitor"
  - "custom monitor"
```

Create `SKILL.md`:
```markdown
# My monitoring skill

## Function
Monitor a specified stock and send a notification when it drops beyond a threshold.

## Parameters
- symbol: stock symbol
- threshold: drop threshold (percentage)
```

Create `SKILL.rhai`:
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
        "message": `⚠️ ${symbol} dropped ${change}%, beyond threshold ${threshold}%`
    });
}
```

### Method 3: install from the community hub

```
You: Search and install a DeFi monitoring skill from the community hub
```

The AI will call the `community_hub` tool to search and download the skill.

---

## Skill hot reload

When you create or modify skill files via chat, blockcell will **auto-detect changes and hot-reload** — no restart required.

```
You: Modify my_monitor so the threshold becomes 5%
AI: edits SKILL.rhai...
    [system detected skill updates and hot-reloaded my_monitor]
```

This is implemented in `runtime.rs`: after `write_file` or `edit_file` succeeds, if the path is under the skills directory, it triggers a reload and notifies the Dashboard via WebSocket.

---

## Skills vs tools: when to use which

| Scenario | Use tools | Use skills |
|------|--------|--------|
| One-off operations | ✅ | |
| Multi-step workflows | | ✅ |
| Reusability needed | | ✅ |
| Degradation/fallback strategy | | ✅ |
| Scheduled execution | | ✅ |
| Simple queries | ✅ | |

---

## A quick Rhai language primer

If you haven’t used Rhai before, here’s a quick intro:

```javascript
// Variables
let x = 42;
let name = "blockcell";

// Conditions
if x > 10 {
    print("greater than 10");
} else {
    print("not greater than 10");
}

// Loops
for i in 0..5 {
    print(i);
}

// Map (like a JSON object)
let params = #{
    "action": "stock_quote",
    "symbol": "600519"
};

// Call a tool (blockcell-specific)
let result = call_tool("finance_api", params);

// Error handling
if is_error(result) {
    log_warn("tool call failed");
    return;
}

// Get fields
let price = get_field(result, "price");
```

Rhai syntax is simple — even without prior programming experience, you can pick it up quickly.

---

## Summary

The Skill system is blockcell’s “software layer”:

- **`meta.yaml`** defines the trigger conditions
- **`SKILL.md`** provides operating guidance for the LLM
- **`SKILL.rhai`** implements deterministic orchestration logic

This three-layer design keeps skills flexible (LLMs can improvise) yet controlled (critical logic is enforced by scripts).

Next, we’ll look at the memory system — how blockcell uses SQLite + FTS5 to give AI persistent memory.

---

*Previous: [blockcell’s tool system — enabling AI to really execute tasks](./03_tools_system.md)*
*Next: [The memory system — letting AI remember what you said](./05_memory_system.md)*

*Repo: https://github.com/blockcell-labs/blockcell*
*Website: https://blockcell.dev*
