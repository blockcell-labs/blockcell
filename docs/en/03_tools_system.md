# Article 03: blockcell’s Tool System — Enabling AI to Really Execute Tasks

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — 3/14

---

## What is a tool?

In the AI agent world, “tools” are a core concept.

Put simply: **a tool is a function the AI can call.**

Without tools, an AI can only chat. With tools, an AI can:
- Read files → analyze content
- Search the web → retrieve up-to-date information
- Run commands → operate the system
- Send messages → notify you

blockcell includes **50+ built-in tools**, covering most common development and work scenarios.

---

## How do tools work?

When you tell the AI “read report.txt”, what happens under the hood?

```
1. Your input → LLM
2. The LLM decides it needs the read_file tool
3. The LLM generates tool parameters: {"path": "~/Desktop/report.txt"}
4. blockcell executes read_file and reads the file
5. The file content is returned to the LLM
6. The LLM generates a response based on the content
7. You see the answer
```

This is **tool calling**, the foundational mechanism behind modern AI agents.

Each tool exposes a **JSON Schema** describing its parameters. The LLM uses that schema to generate correct tool-call arguments.

---

## Tool categories in detail

### File system tools

**`read_file`** — read a file
```
Formats: txt/md/json/csv/xlsx/docx/pptx/pdf
Feature: Office files are automatically converted to Markdown text
```

**`write_file`** — write a file
```
Supports: create new files and overwrite existing files
```

**`edit_file`** — precise file editing
```
Supports: exact string replacement (exact match)
Best for: modifying code and configuration files
```

**`list_dir`** — list directory contents
```
Returns: names, sizes, modification times
```

**`file_ops`** — a collection of file operations
```
Supports: delete, rename/move, copy, compress (zip/tar.gz), decompress
```

**Example:**
```
You: Compress all .log files on my Desktop into a zip
AI: list_dir → find all .log files → file_ops compress
```

---

### Network tools

**`web_search`** — web search
```
Backend: DuckDuckGo (no API key required)
Returns: title, snippet, URL
```

**`web_fetch`** — fetch web page content
```
Features: auto-converts to Markdown
          supports Cloudflare “Markdown for Agents”
          saves up to ~80% tokens
```

**`http_request`** — generic HTTP requests
```
Methods: GET/POST/PUT/PATCH/DELETE
Features: custom headers, bearer tokens, JSON/form bodies
Best for: calling any REST API
```

**`stream_subscribe`** — real-time streams
```
Protocols: WebSocket, SSE (Server-Sent Events)
Actions: subscribe, unsubscribe, read buffered messages
Best for: real-time quotes, log streams
```

**Example:**
```
You: Check today’s Bitcoin price
AI: web_search "bitcoin price today" → web_fetch a result page
```

---

### System execution tool

**`exec`** — run command line commands
```
Supports: arbitrary shell commands
Safety: requires user confirmation outside the workspace
Timeout: 30 seconds by default
```

**Example:**
```
You: See which process is using port 8080
AI: exec "lsof -i :8080"
```

---

### Browser tool

**`browse`** — CDP browser automation (heavyweight; covered in detail next article)
```
Engines: Chrome/Edge/Firefox
Actions: navigate, click, fill, screenshot, execute_js, tab management...
Feature: can fetch the accessibility tree to locate elements precisely
```

---

### Data processing tools

**`data_process`** — CSV/JSON processing
```
Actions: read_csv, write_csv, query (filter/sort), stats, transform
Best for: data analysis and report generation
```

**`chart_generate`** — chart generation
```
Types: line, bar, pie, scatter, heatmap...
Backends: matplotlib (PNG/SVG), plotly (interactive HTML)
```

**Example:**
```
You: Analyze sales.csv and draw a monthly sales line chart
AI: data_process read_csv → stats → chart_generate line
```

---

### Financial data tools

**`finance_api`** — stocks/funds/forex/bonds
```
CN/HK stocks: Eastmoney (free, no key)
US stocks: Alpha Vantage + Yahoo Finance
Crypto: CoinGecko (free)
Bonds: Eastmoney treasury yield curves
```

**`exchange_api`** — crypto exchanges
```
Supports: Binance, OKX, Bybit
Features: quotes, k-lines, accounts, order placement (API key required)
```

**`blockchain_rpc`** — on-chain queries
```
Chains: Ethereum, Polygon, BSC, Arbitrum and 14+ more
Features: balance, tx lookup, contract calls, ABI encode/decode
```

**Example:**
```
You: Check Moutai’s price today and its trend over the last month
AI: finance_api stock_quote "600519" → finance_api stock_history
```

---

### Communication tools

**`email`** — email send/receive
```
Send: SMTP (attachments, HTML, CC)
Receive: IMAP (search, read attachments)
```

**`notification`** — notifications
```
Channels: SMS (Twilio), push (Pushover/Bark/ntfy), webhook, desktop
```

---

### Media tools

**`audio_transcribe`** — speech-to-text
```
Backends: Whisper (local), whisper.cpp, OpenAI API
Formats: mp3/wav/m4a/flac/mp4/mkv
```

**`tts`** — text-to-speech
```
Backends: macOS say, Piper (local), Edge TTS (free), OpenAI TTS
```

**`ocr`** — OCR
```
Backends: Tesseract (local), macOS Vision, OpenAI Vision
```

**`image_understand`** — image understanding
```
Providers: GPT-4o, Claude, Gemini
Actions: analyze, describe, compare, extract
```

**`video_process`** — video processing
```
Actions: clip, merge, burn subtitles, thumbnail, transcode, extract audio, compress
Backend: ffmpeg
```

**`camera_capture`** — take a photo
```
Actions: list cameras, capture (jpg/png)
Backends: imagecapture/ffmpeg/screencapture (3-level fallback)
```

---

### Office tool

**`office_write`** — generate Office files
```
Formats: PPTX, DOCX, XLSX
Backend: Python (python-pptx/python-docx/openpyxl)
```

**Example:**
```
You: Turn this market analysis into a 10-slide PPT with titles and bullet points
AI: office_write create_pptx → generate report.pptx
```

---

### Memory & knowledge tools

**`memory_query`** — search memory
```
Backend: SQLite + FTS5 full-text search
Filters: scope (short/long), type, tags, time range
```

**`memory_upsert`** — save memory
```
Supports: dedup_key deduplication, expires_in_days
```

**`knowledge_graph`** — knowledge graph
```
Backend: SQLite + FTS5
Features: entity/relationship management, path finding, subgraph extraction,
          export (JSON/DOT/Mermaid)
```

---

### Scheduling & monitoring tools

**`alert_rule`** — alert rules
```
Operators: gt/lt/gte/lte/eq/ne/change_pct/cross_above/cross_below
Actions: can automatically trigger other tools (e.g., notifications)
Persistence: workspace/alerts/rules.json
```

**`cron`** — scheduled tasks
```
Format: standard cron expressions
Actions: create, list, delete scheduled tasks
```

---

### System information tool

**`system_info`** — system probe
```
Detects: hardware (CPU/memory/GPU/camera/mic)
          software (rustc/python3/node/git/docker/ffmpeg)
          network (connectivity/interfaces)
          current capability status
```

---

## Tool safety mechanisms

blockcell includes a path safety system:

```
Inside workspace (~/.blockcell/workspace): allowed automatically
Outside workspace: prompts for confirmation
Authorized directories: approve once, then allow automatically
Gateway mode: always denies outside-workspace access
```

This prevents the AI from modifying important files without your awareness.

---

## How to list all tools

```bash
blockcell tools
```

Example output:

```
Available tools (52):
  read_file        - Read file contents
  write_file       - Write content to file
  edit_file        - Edit file with string replacement
  web_search       - Search the web
  web_fetch        - Fetch web page content
  exec             - Execute shell command
  browse           - Browser automation via CDP
  finance_api      - Stock/crypto/forex data
  ...
```

---

## Extensibility

Besides built-in tools, blockcell also supports:

1. **Calling any API via `http_request`** — no dedicated tool required
2. **Executing any script via `exec`** — Python/Node/Shell all work
3. **Packaging complex flows as skills** — covered in the next article

---

## Summary

blockcell’s tool system is its core strength:

- **Broad coverage**: 50+ tools from files to finance, browsers to IoT
- **Works out of the box**: most tools need little to no extra configuration
- **Safety**: path confirmation protects your filesystem
- **Extensible**: expand endlessly via HTTP/exec/skills

Next, we’ll look at the Skill system — the key mechanism that makes capabilities reusable and evolvable.

---

*Previous: [Get started with blockcell in 5 minutes](./02_quickstart.md)*
*Next: [The Skill system — extending AI capabilities with Rhai scripts](./04_skill_system.md)*

*Repo: https://github.com/blockcell-labs/blockcell*
*Website: https://blockcell.dev*
