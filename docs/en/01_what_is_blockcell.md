# Article 01: What Is blockcell? A Self-Evolving AI Agent Framework

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — 1/14

---

## Start with a scenario

Have you ever run into this situation:

You ask ChatGPT a question and it gives you some code, but you still need to manually copy it into your editor and run it. Then it fails, you paste the error back to the model, it fixes the code, you copy again…

It’s tedious. The AI clearly “knows” what to do, but it can only *say* it, not *do* it.

**blockcell aims to solve exactly this problem.**

---

## What is blockcell

blockcell is an open-source AI agent framework written in **Rust**.

The name **Block + Cell** represents a stable, modular foundation (Block) plus evolvable capability units (Cell). (See the appendix: [*Name origin*](./14_name_origin.md).)

In one sentence: **it makes AI not just chat, but actually execute tasks.**

```
You: “Help me analyze sales.xlsx on my Desktop and draw a line chart.”
blockcell: read file → analyze data → call Python to plot → return the image path
```

You don’t need to do anything manually — the AI completes the whole flow by itself.

---

## How is it different from ordinary chat AI tools?

| Comparison | ChatGPT/Claude (web) | blockcell |
|--------|------------------------|-----------|
| Read/write local files | ❌ | ✅ |
| Execute command line | ❌ | ✅ |
| Control a browser | ❌ | ✅ |
| Send emails/messages | ❌ | ✅ |
| Persistent memory | Limited | ✅ SQLite full-text search |
| Scheduled tasks | ❌ | ✅ Cron scheduling |
| Telegram/Slack integration | ❌ | ✅ |
| Self-upgrade | ❌ | ✅ Self-evolution system |

---

## Core architecture: Rust host + Rhai skills

blockcell is layered into two parts:

```
┌─────────────────────────────────────────────┐
│            Rust Host (TCB)                  │
│  Message bus | Tool registry | Scheduler    │
│  Storage | Auditing                          │
└─────────────────────────────────────────────┘
                     ↕
┌─────────────────────────────────────────────┐
│            Rhai Skill Layer (mutable)       │
│  stock_monitor | bond_monitor | custom skills│
└─────────────────────────────────────────────┘
```

The **Rust host** is the stable core responsible for security, performance, and foundational capabilities. It does not change easily.

**Rhai skills** are a flexible extension layer. You can add or modify skills at any time — and even let the AI generate new skills automatically.

This design gives you a stable kernel with flexible extensions, like an operating system and apps.

---

## What capabilities are built in?

Out of the box, blockcell includes **50+ tools**, covering:

**Files & system**
- Read/write files, run commands, directory operations
- Read Excel/Word/PDF/PPT

**Web & data**
- Web fetching (Markdown-first, token-efficient)
- Browser automation (CDP-based, can control a real Chrome)
- HTTP requests, WebSocket subscriptions

**Financial data**
- Real-time quotes for CN/HK/US stocks (Eastmoney, Alpha Vantage)
- Crypto prices (CoinGecko)
- On-chain data, DeFi, NFT

**Communication**
- Email (SMTP/IMAP)
- Telegram/Slack/Discord/Feishu messaging

**Media**
- Screenshots, speech-to-text (Whisper)
- Chart generation (matplotlib/plotly)
- Generate PPT/Word/Excel

**AI enhancements**
- Image understanding (GPT-4o/Claude/Gemini)
- Text-to-speech (TTS)
- OCR

---

## What does “self-evolution” mean?

This is blockcell’s most distinctive feature.

When the AI repeatedly fails while executing a skill, the system can automatically:

1. Record the error pattern
2. Trigger an evolution workflow
3. Ask an LLM to generate a new version of the code
4. Automatically audit, compile, and test
5. Roll out via canary (10% → 50% → 100%)
6. Auto-rollback if the new version performs worse

```
Error triggers → LLM generates new code → audit → compile → test → canary rollout → full rollout
                                                         ↓ on failure
                                                      auto rollback
```

This means blockcell can get smarter over time and fix its own issues as you use it.

---

## Which AI models are supported?

blockcell supports all OpenAI-compatible APIs, and also provides native support for:

- **OpenAI** (GPT-4o, GPT-4.1, etc.)
- **Anthropic** (Claude family)
- **Google Gemini**
- **DeepSeek**
- **Kimi/Moonshot**
- **Ollama** (local models, fully offline)
- **OpenRouter** (one key for many models)

---

## Why Rust?

A common question is: shouldn’t an AI framework be written in Python?

blockcell chose Rust for a few reasons:

1. **Safety**: Rust’s memory safety guarantees reduce unexpected crashes during execution
2. **Performance**: high concurrency on a single machine without Python’s GIL bottleneck
3. **Reliability**: as the trusted computing base, the host layer must be stable and dependable
4. **Cross-platform**: compile to a single binary for macOS/Linux/Windows

---

## Get a quick feel

```bash
# Install
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/refs/heads/main/install.sh | sh

# Initialize
blockcell onboard

# Edit config and add your API key
# ~/.blockcell/config.json

# Start chatting
blockcell agent
```

Then you can say things like:
- “Search for today’s AI news.”
- “Read report.pdf on my Desktop and summarize it.”
- “Monitor Moutai’s stock price and tell me if it drops below 1500.”

---

## Series table of contents

For the full table of contents and recommended reading order, see the index:

*Index: [Series directory](./00_index.md)*

---

## Summary

blockcell is not just a chatbot — it’s an **AI agent framework that can actually execute tasks**.

Its core ideas are:
- **Tooling**: the AI interacts with the real world through tools
- **Skills**: complex tasks are packaged into reusable skills
- **Evolution**: the system can learn and improve automatically
- **Safety**: a Rust host provides a trusted execution environment

If you want an AI that truly helps you get work done rather than only chatting, blockcell is worth trying.

---

*Next: [Get started with blockcell in 5 minutes — from installation to your first chat](./02_quickstart.md)*

*Repo: https://github.com/blockcell-labs/blockcell*
*Website: https://blockcell.dev*
