# BlockCell

<div align="center">

**A self-evolving AI agent framework built with Rust**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.96%2B-orange.svg)](https://www.rust-lang.org)
[![GitHub stars](https://img.shields.io/github/stars/blockcell-labs/blockcell?style=social)](https://github.com/blockcell-labs/blockcell)

[Website](https://blockcell.dev) • [Documentation](https://blockcell.dev/docs) • [中文](README.md)

**Latest release**: v0.1.8 • [Download Release](https://github.com/blockcell-labs/blockcell/releases/tag/v0.1.8) • [Changelog](CHANGELOG.en.md)

</div>

---

## 🌟 What Makes BlockCell Different

BlockCell isn't just another chatbot — it's an AI agent that **actually executes tasks**. While ChatGPT can only tell you what to do, BlockCell can:

- 📁 Read and write files on your system
- 🌐 Control browsers and automate web tasks
- 📊 Analyze Excel/PDF files and generate reports
- 💰 Monitor stock prices and crypto markets
- 📧 Send emails and messages across platforms
- 🔄 **Evolve itself** — automatically fix bugs and deploy improvements

```
You: "Monitor Tesla stock and alert me if it drops below $200"
BlockCell: ✓ Sets up monitoring → ✓ Checks price every hour → ✓ Sends Telegram alert
```

---

## 🎯 The Name

> *"Simplest units, most complex whole."*

**BlockCell** is inspired by the **Replicators** from *Stargate* — mechanical life forms built from countless tiny, independent blocks. Each block is simple, but together they form ships, soldiers, and minds. They adapt instantly, evolve faster than any weapon can counter, and cannot be destroyed.

That philosophy lives in this framework:

- **Block** → Immutable Rust host: secure, stable, deterministic
- **Cell** → Mutable skills layer: living, self-repairing, endlessly evolving

Traditional software dies the moment it ships. BlockCell is meant to be **alive**.

→ [Full naming story](https://blockcell.dev/naming-story)

---

## ✨ Key Features

### 🛠️ 60+ Built-in Tools

- **Files & System**: Read/write files, execute commands, process Excel/Word/PDF
- **Web & Browser**: Web scraping, headless Chrome automation (CDP), HTTP requests
- **Finance**: Real-time stock quotes (CN/HK/US), crypto prices, DeFi data
- **Communication**: Email (SMTP/IMAP), Telegram, Slack, Discord, Feishu
- **Media**: Screenshots, speech-to-text (Whisper), chart generation, Office file creation
- **AI**: Image understanding, text-to-speech, OCR

### 🧬 Self-Evolution System

When the AI repeatedly fails at a task, BlockCell can:

1. Detect the error pattern
2. Generate improved code using LLM
3. Automatically audit, compile, and test
4. Deploy via canary rollout (10% → 50% → 100%)
5. Auto-rollback if performance degrades

```
Error detected → LLM generates fix → Audit → Test → Canary deploy → Full rollout
                                                    ↓ on failure
                                                 Auto rollback
```

### 🧠 Ghost Native Learning

BlockCell can retain durable lessons from real usage:

- Stable user preferences go into `USER.md`
- Project facts, environment conventions, and recurring lessons go into `MEMORY.md`
- Ghost background review only writes declarative memory; reusable workflows become workspace skills through the main agent or the separate Skill Learning channel
- Background review failures do not block the current answer, and automatic writes are audited, snapshotted, and undoable

### 🧭 ModelRouter and Connection-Phase Fallback

When multiple models are configured, BlockCell can choose the right provider by policy:

- `manual`: keep Provider Pool priority and weighted selection
- `cost_optimized`: prefer lower-cost models for short contexts (≤4 messages) and return to primary models for long contexts
- `quality_first`: prefer higher-priority primary models
- `latency_first`: prefer entries with the best recent fast-success signal, falling back to priority selection

Main LLM calls also support connection-phase fallback. If the preferred provider fails before streaming starts, BlockCell can try the next available provider. Once streaming has started, it will not mix partial output from one model with another model's response.

### 🪝 Hook Lifecycle Events

BlockCell can run local commands from `~/.blockcell/hooks.yaml` at key lifecycle events:

- `pre_tool_use` / `post_tool_use` around tool execution, with tool-name glob matching
- `session_start` / `session_end` / `user_prompt` / `agent_stop` / `compaction` for session, message, and compaction lifecycle events
- Template variables such as `{tool_name}`, `{session_id}`, `{cwd}`, `{command}`, `{file_path}`, and `{result}`
- Hook failures and timeouts do not block the main agent flow, making hooks useful for auditing, formatting, notifications, and external logging

### 🧩 Runtime Kernel: Observable, Interceptable, Replayable

v0.1.8 splits the agent runtime into a composable kernel, so a run is no longer just a final answer:

- **Unified event bus**: lifecycle events such as `turn/start`, `step/end`, `tool/pre-execute`, `model/response`, `memory/write`, and `subagent/completed` are dispatched by priority and scope, with listener timeouts and failure isolation
- **Session event log**: an append-only JSONL sidecar (`schema_version` + monotonic `seq`) that evolves alongside the transcript and supports retention cleanup
- **Replay and evaluation**: `blockcell session list / replay / verify` replays turns, steps, model requests, and tool calls, and reports token, duration, and retry metrics; `verify` checks that the event-log projection matches the transcript
- **Guarded Tool Pipeline**: tool toggles, policy, dangerous operations, path access, and shell escalation are centralized in one pipeline, so individual tools no longer handle permissions or auditing themselves
- **Failure semantics**: distinguishes failed / rejected / cancelled / timed-out / not-started / unknown; unknown outcomes are reconciled before deciding whether to retry
- **Capability seams**: sandbox, shell, filesystem, browser, and subagent are exposed as replaceable interfaces, with native implementations behind them

### 🛡️ Sandbox and Execution Environment

- Sandbox backends: macOS Seatbelt and Linux Landlock + seccomp, falling back to explicit approval when unavailable
- **Execution environment is decoupled from "needs confirmation"**: the `exec` family honestly declares FullAccess, only `shell` with `sandbox_policy=full-access` asks per call, and dangerous commands are still gated on content
- Persistent shell sessions are isolated per session and no longer reused across channels
- Nested tool calls inside skill scripts are subject to the same policy, dangerous-operation checks, and auditing

### 🔐 Controlled Execution and Audit

v0.1.8 converges security boundaries onto shared primitives:

- Tool Policy supports tool-name globs, channel conditions, path conditions, `allow` / `ask` / `deny` decisions, and inherited rule groups, with **Deny > Ask > Allow** precedence
- Global token and cost budgets are tracked per session, and subagent spend is folded into the originating session so background agents cannot bypass it
- Audit logs support SHA-256 hash-chain verification (cross-process exclusive) and record sessions, provider calls, budget events, plus each tool call's execution environment and outcome
- Path expansion, filename sanitization, Python string escaping, SSRF validation, and config secret masking all go through shared primitives instead of per-module copies
- Gateway API authentication is stricter: regular APIs no longer accept URL `?token=`, so use `Authorization: Bearer <token>`

### 🤖 Multi-Agent and Custom Agents

- Supports typed agents, forked subagents, checkpoints, and chained cancellation
- Loads user-level and project-level Markdown agent definitions
- Each agent type can define tool scope, model, skills, MCP servers, one-shot behavior, and permission mode
- CLI, Gateway, and WebUI can receive background task progress events

### 🌐 Multi-Channel Support

Run BlockCell as a daemon and connect it to:

- **Telegram** (long polling)
- **WhatsApp** (bridge WebSocket)
- **Feishu** (long-connection WebSocket)
- **Lark** (webhook)
- **Slack** (Socket Mode, with polling fallback when `appToken` is absent)
- **Discord** (Gateway WebSocket)
- **DingTalk** (Stream SDK)
- **WeCom** (polling / webhook)
- **QQ** (official bot Gateway / webhook)
- **NapCatQQ** (OneBot 11 WebSocket / HTTP)
- **Weixin** (iLink Bot API with QR login and long polling)

#### 📖 Channel Integration Guides

Each channel has detailed configuration documentation (bilingual):

**Chinese Docs** | **English Docs**
--- | ---
[Telegram 配置](docs/channels/zh/01_telegram.md) | [Telegram Setup](docs/channels/en/01_telegram.md)
[Discord 配置](docs/channels/zh/02_discord.md) | [Discord Setup](docs/channels/en/02_discord.md)
[Slack 配置](docs/channels/zh/03_slack.md) | [Slack Setup](docs/channels/en/03_slack.md)
[飞书配置](docs/channels/zh/04_feishu.md) | [Feishu Setup](docs/channels/en/04_feishu.md)
[钉钉配置](docs/channels/zh/05_dingtalk.md) | [DingTalk Setup](docs/channels/en/05_dingtalk.md)
[企业微信配置](docs/channels/zh/06_wecom.md) | [WeCom Setup](docs/channels/en/06_wecom.md)
[WhatsApp 配置](docs/channels/zh/07_whatsapp.md) | [WhatsApp Setup](docs/channels/en/07_whatsapp.md)
[Lark 配置](docs/channels/zh/08_lark.md) | [Lark Setup](docs/channels/en/08_lark.md)
[QQ 配置](docs/channels/zh/09_qq.md) | [QQ Setup](docs/channels/en/09_qq.md)
[NapCatQQ 配置](docs/channels/zh/10_napcatqq.md) | [NapCatQQ Setup](docs/channels/en/10_napcatqq.md)
[微信配置](docs/channels/zh/11_weixin.md) | [Weixin Setup](docs/channels/en/11_weixin.md)

Each guide includes:
- 📝 Application creation steps
- 🔑 Permission configuration
- ⚙️ Blockcell configuration examples
- 💬 Interaction methods
- ⚠️ Troubleshooting common issues

### 🏗️ Rust Host + Three Skill Forms

```
┌─────────────────────────────────────────────┐
│         Rust Host (Trusted Core)            │
│  Message bus | Tool registry | Scheduler    │
│  Storage | Auditing | Security              │
└─────────────────────────────────────────────┘
                     ↕
┌─────────────────────────────────────────────┐
│         Skills Layer (Mutable Layer)        │
│  Pure Markdown | Markdown + Rhai            │
│  Markdown + Python                          │
└─────────────────────────────────────────────┘
```

- **Rust host**: Immutable, secure, high-performance foundation
- **Pure Markdown skills**: define behavior with `SKILL.md` only, ideal for knowledge and workflow-oriented skills
- **Markdown + Rhai skills**: combine `SKILL.md` with `SKILL.rhai` for structured orchestration and tool calling
- **Markdown + Python skills**: combine `SKILL.md` with Python scripts for heavier data processing, integrations, and execution logic
- **Community skill compatibility**: recursive skill pack/category scanning, with OpenClaw and gbrain skill support

---

## 🚀 Quick Start

### Installation (Recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/main/install.sh | sh
```

This installs `blockcell` to `~/.local/bin`. To customize the location:

```bash
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/main/install.sh | BLOCKCELL_INSTALL_DIR="$HOME/bin" sh
```

### Build from Source

**Prerequisites**: Rust 1.96+

```bash
git clone https://github.com/blockcell-labs/blockcell.git
cd blockcell
cargo build --release
```

### First Run

```bash
# Recommended: interactive setup wizard
blockcell setup

# Start interactive mode
blockcell agent
```

`setup` creates `~/.blockcell/`, saves provider settings, and auto-binds newly enabled external channels to the `default` agent when no owner is set yet.

### Daemon Mode (with WebUI)

```bash
blockcell gateway
```

- **API Server**: `http://localhost:18790`
- **WebUI**: `http://localhost:18791`
- **Default routing**: CLI / WebUI / WebSocket go to the `default` agent; external channels first check `channelAccountOwners.<channel>.<accountId>` and fall back to `channelOwners.<channel>`

### Common Commands

| Command | Description |
| --- | --- |
| `blockcell setup` | Interactive wizard: initialize config, pick a provider, optionally set up channels |
| `blockcell agent` | Interactive chat; `-m <msg>` for a one-shot run |
| `blockcell gateway` | Daemon mode: API + WebUI + external channels |
| `blockcell status` / `doctor` | Show config status / run environment diagnostics |
| `blockcell config` | Inspect, edit, or open `config.json5` |
| `blockcell tools` / `mcp` | Manage built-in tool toggles and MCP servers |
| `blockcell session list/replay/verify` | List, replay, and verify session event logs |
| `blockcell memory` / `skills` / `cron` | Manage memory, skills, and scheduled tasks |
| `blockcell upgrade` | Upgrade to the latest release |

See the [CLI reference](docs/en/17_cli_reference.md) for the full command list.

---

## 📸 Screenshots

<div align="center">

### Gateway Mode
![Start Gateway](screenshot/start-gateway.png)

### WebUI Interface
![WebUI Chat](screenshot/webui-chat.png)

![WebUI Login](screenshot/webui-login.png)

</div>

---

## ⚙️ Configuration

Minimal configuration example (`~/.blockcell/config.json5`):

```json
{
  "providers": {
    "deepseek": {
      "apiKey": "YOUR_API_KEY",
      "apiBase": "https://api.deepseek.com"
    }
  },
  "agents": {
    "defaults": {
      "model": "deepseek-flash",
      "provider": "deepseek",
      "maxContextTokens": 1048576,
      "reasoningEffort": "high"
    }
  }
}
```

To enable multi-agent routing plus external channels, extend it using the structure currently supported by the codebase:

<details>
<summary>Expand the example: a complete "2 agents + 2 Telegram accounts" config</summary>

```json
{
  "agents": {
    "defaults": {
      "model": "deepseek-flash",
      "provider": "deepseek",
      "maxContextTokens": 1048576,
      "reasoningEffort": "high"
    },
    "list": [
      {
        "id": "default",
        "enabled": true,
        "name": "General Assistant",
        "intentProfile": "default"
      },
      {
        "id": "ops",
        "enabled": true,
        "name": "Operations Assistant",
        "intentProfile": "ops",
        "maxToolIterations": 12
      }
    ]
  },
  "intentRouter": {
    "enabled": true,
    "defaultProfile": "default",
    "agentProfiles": {
      "default": "default",
      "ops": "ops"
    },
    "profiles": {
      "default": {
        "coreTools": ["read_file", "write_file", "list_dir", "web_fetch", "message"],
        "intentTools": {
          "Chat": { "inheritBase": false, "tools": [] },
          "FileOps": ["read_file", "write_file", "list_dir"],
          "WebSearch": ["web_search", "web_fetch"]
        }
      },
      "ops": {
        "coreTools": ["http_request", "message", "notification", "alert_rule", "list_tasks"],
        "intentTools": {
          "DevOps": ["http_request", "notification", "alert_rule", "list_tasks"],
          "Communication": ["message", "notification"]
        },
        "denyTools": ["write_file", "exec"]
      }
    }
  },
  "channels": {
    "telegram": {
      "enabled": true,
      "accounts": {
        "main_bot": {
          "enabled": true,
          "token": "123456:MAIN_BOT_TOKEN",
          "allowFrom": ["alice"]
        },
        "ops_bot": {
          "enabled": true,
          "token": "123456:OPS_BOT_TOKEN",
          "allowFrom": ["oncall_group"]
        }
      },
      "defaultAccountId": "main_bot"
    }
  },
  "channelOwners": {
    "telegram": "default"
  },
  "channelAccountOwners": {
    "telegram": {
      "main_bot": "default",
      "ops_bot": "ops"
    }
  },
  "gateway": {
    "apiToken": "YOUR_STABLE_API_TOKEN",
    "webuiPass": "YOUR_WEBUI_PASSWORD"
  }
}
```

</details>

Notes:

- `agents.list` should use fields actually supported by the code, such as `id`, `enabled`, `name`, `intentProfile`, and `maxToolIterations`
- `intentRouter` currently supports `enabled`, `defaultProfile`, `agentProfiles`, and `profiles`
- Each `profiles.<name>` entry can define `coreTools`, `intentTools`, and `denyTools`
- Telegram multi-account config belongs under `channels.telegram.accounts`, and each account uses `enabled`, `token`, and `allowFrom`
- Channel-level routing uses `channelOwners`
- Account-level overrides use `channelAccountOwners`
- If you only need a single agent, use the minimal config above or read `QUICKSTART.md`
- If you want the full multi-agent walkthrough, read `QUICKSTART.multi-agent.md`

### Supported LLM Providers

- **OpenAI** (GPT-5.5, GPT-5.4-mini, o1, o3)
- **Anthropic** (Claude Opus 4.8, Claude Sonnet 4.6)
- **Google Gemini** (Gemini 3.1 Pro, Gemini 3.5 Flash)
- **DeepSeek** (DeepSeek V4 Pro, V3, R1)
- **Kimi/Moonshot**
- **MiniMax** ([MiniMax M3.1 Flash](https://www.minimaxi.com/))
- **Zhipu AI** ([GLM-5](https://bigmodel.cn/))
- **Qwen**
- **xAI** (Grok)
- **Mistral**
- **Groq**
- **SiliconFlow** ([SiliconFlow](https://siliconflow.cn/))
- **Ollama** (local models, fully offline)
- **OpenRouter** (unified access to 200+ models)

---

## 🔧 Optional Dependencies

For full functionality, install these tools:

- **Charts**: Python 3 + `matplotlib` / `plotly`
- **Office**: Python 3 + `python-pptx` / `openpyxl` (DOCX is generated natively, no Python needed)
- **Audio**: `ffmpeg` + `whisper` (or use API backend)
- **Browser**: Chrome/Chromium (for CDP automation)
- **macOS only**: `chrome_control`, `app_control`

---

## 📚 Documentation

- [Full series index](docs/en/00_index.md)
- [What is blockcell?](docs/en/01_what_is_blockcell.md)
- [Quick Start Guide (Single Agent)](QUICKSTART.md)
- [Quick Start Guide (Multi-Agent)](QUICKSTART.multi-agent.md)
- [Architecture Deep Dive](docs/en/12_architecture.md)
- [Tool System](docs/en/03_tools_system.md)
- [Skill System](docs/en/04_skill_system.md)
- [Memory System](docs/en/05_memory_system.md)
- [Ghost Native Learning Design (Chinese)](docs/27_ghost_learning_design.md)
- [ModelRouter and Fallback (Chinese)](docs/28_model_router.md)
- [Provider Pool](docs/en/20_provider_pool.md)
- [Intent Router and Profiles](docs/en/21_intent_router_profiles.md)
- [Hook Lifecycle Events (Chinese)](docs/29_hook_system.md)
- [MCP Servers](docs/en/19_mcp_servers.md)
- [Channel Configuration](docs/en/06_channels.md)
- [Browser Automation](docs/en/07_browser_automation.md)
- [Gateway Mode](docs/en/08_gateway_mode.md)
- [Self-Evolution](docs/en/09_self_evolution.md)
- [Path Access Policy](docs/en/22_path_access_policy.md)
- [CLI Reference](docs/en/17_cli_reference.md)

---

## 🏗️ Project Structure

```
blockcell/
├── bin/blockcell/          # CLI entry point
├── crates/
│   ├── core/               # Config, paths, shared types
│   ├── agent/              # Agent runtime and safety
│   ├── tools/              # 60+ built-in tools
│   ├── skills/             # Skill loading, OpenClaw compatibility & evolution
│   ├── storage/            # SQLite memory, sessions & RabitQ vector index
│   ├── channels/           # Messaging channel adapters
│   ├── providers/          # LLM provider clients and routing
│   ├── scheduler/          # Cron & heartbeat
│   └── updater/            # Self-upgrade system
├── skills/                 # Built-in skills (Markdown / Rhai / Python)
├── webui/                  # Web console (Vite + React)
├── docs/                   # Chinese & English docs
└── tests/                  # E2E and coding-ability evals
```

---

## 🤝 Contributing

We welcome contributions! Here's how to get started:

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

See [CONTRIBUTING.md](CONTRIBUTING.md) for detailed guidelines.

---

## 🔒 Security

- **Path safety**: Automatic validation of file system access
- **Sandboxed execution**: Rhai scripts run in isolated environment, and shells can run behind the Seatbelt / Landlock backends
- **Audit logging**: All tool executions are logged, including execution environment and outcome
- **Session replay**: `blockcell session replay` replays the full chain of any run for after-the-fact auditing
- **Gateway authentication**: Bearer token support for API access

In interactive mode, operations outside `~/.blockcell/workspace` require explicit confirmation.

---

## 📊 Use Cases

### Finance Automation
```
"Monitor AAPL stock and alert me if it drops 5%"
"Analyze my portfolio.xlsx and suggest rebalancing"
```

### Data Processing
```
"Read all PDFs in ~/Documents and create a summary spreadsheet"
"Generate a sales report with charts from data.csv"
```

### Web Automation
```
"Check my company's website every hour and alert if it's down"
"Fill out this form on example.com with data from sheet.xlsx"
```

### Communication
```
"Send daily standup summary to #team-updates on Slack"
"Forward urgent emails to my Telegram"
```

---

## 🌍 Community

- **GitHub**: [blockcell-labs/blockcell](https://github.com/blockcell-labs/blockcell)
- **Website**: [blockcell.dev](https://blockcell.dev)
- **Discord**: [Join our community](https://discord.gg/E8TXuHk9QZ)
- **Twitter/X**: [@blockcell_ai](https://x.com/blockcell_ai)

---

## 📝 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

---

## 🙏 Acknowledgments

BlockCell stands on the shoulders of giants:

- [Rust](https://www.rust-lang.org/) - Systems programming language
- [Rhai](https://rhai.rs/) - Embedded scripting engine
- [Tokio](https://tokio.rs/) - Async runtime
- [SQLite](https://www.sqlite.org/) - Embedded database
- [OpenClaw](https://github.com/openclaw/openclaw) - Source of the compatible skill format
- [NanoClaw](https://github.com/nanocoai/nanoclaw) - A lightweight OpenClaw alternative

---

<div align="center">

**If you find BlockCell useful, please consider giving it a ⭐️ on GitHub!**

[⭐ Star on GitHub](https://github.com/blockcell-labs/blockcell) • [📖 Read the Docs](https://blockcell.dev/docs) • [💬 Join Discord](https://discord.gg/E8TXuHk9QZ)

</div>
