# blockcell

A self-evolving AI agent framework in Rust.

- Website: https://blockcell.dev
- 中文说明: [README.zh-CN.md](README.zh-CN.md)

## The Name

> *"Simplest units, most complex whole."*

**Blockcell** is named after the **Replicators** from the sci-fi series *Stargate* — mechanical life forms built from countless tiny, independent **Blocks**. Each Block is simple on its own, but together they form ships, soldiers, and minds. They adapt instantly, evolve faster than any weapon can counter, and cannot be destroyed by breaking them apart — scattered Blocks simply find each other and reassemble.

That philosophy lives in this framework:

- **Block** → the Host and Tools: immutable, Rust-hard, deterministic.
- **Cell** → the Skills and Self-Evolution: living, self-repairing, endlessly proliferating.

Traditional software is dead the moment it ships. Blockcell is meant to be alive.

→ [Full naming story](https://blockcell.dev/naming-story)

## Screenshots

![Start gateway](screenshot/start-gateway.png)

![WebUI login](screenshot/webui-login.png)

![WebUI chat](screenshot/webui-chat.png)

## What it is

blockcell uses a "Rust host + skills" architecture:

- The Rust host (TCB) provides strong boundaries: message bus, tool registry, scheduler, storage, audit logs, and upgrade/rollback.
- Skills (Rhai scripts) are the mutable layer and can be learned/evolved/rolled out.
- The agent connects to OpenAI-compatible LLM providers (OpenRouter / Anthropic / OpenAI / DeepSeek / ...).

## Key features (current code)

- CLI + gateway daemon: `blockcell onboard|status|agent|gateway|doctor|config|tools|run|channels|cron|memory|skills|evolve|alerts|streams|knowledge|upgrade|logs|completions`
- Tool-calling with JSON Schema validation (`blockcell-tools`)
  - File/exec/web search & fetch
  - Headless browser automation via CDP, with accessibility snapshots + deterministic element refs (`@e1`, `@e2`, ...)
  - Email (SMTP/IMAP), audio transcription (Whisper), charts (matplotlib/plotly), Office generation (PPTX/DOCX/XLSX)
- Persistent state under `~/.blockcell/` (config, workspace, sessions, audit, cron, media, updates)
- Memory store backed by SQLite + FTS5 full-text search (`blockcell-storage`)
- Background subagents + task tracking (`spawn`, `/tasks`)
- Scheduler: cron jobs + heartbeat tasks injected as messages (`blockcell-scheduler`)
- Upgrade system skeleton: manifest + verification + atomic switch + rollback (`blockcell-updater`)
- Gateway API + WebUI: HTTP `/v1/chat` + WebSocket `/v1/ws`, embedded WebUI server

## Repository layout

- `bin/blockcell` - CLI entry
- `crates/core` - config, paths, shared types
- `crates/agent` - agent runtime loop + safety confirmations
- `crates/tools` - built-in tools + tool registry
- `crates/skills` - Rhai engine, skill manager/evolution service, capability registry/core evolution
- `crates/storage` - sessions, audit, memory (SQLite)
- `crates/scheduler` - cron + heartbeat
- `crates/channels` - Telegram/WhatsApp/Feishu adapters (feature-gated)
- `crates/providers` - OpenAI-compatible provider client
- `crates/updater` - self-upgrade utilities
- `docs/` - design docs (architecture, memory, skill sharing)
- `refs/` - reference implementation snapshots (for behavior alignment)

## Quick start

For a step-by-step guide, see: [QUICKSTART.md](QUICKSTART.md)

### Install (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/refs/heads/main/install.sh | sh
```

By default, this installs `blockcell` to `~/.local/bin`. You can override the location:

```bash
BLOCKCELL_INSTALL_DIR="$HOME/bin" \
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/refs/heads/main/install.sh | sh
```

### Build from source

Prereqs: Rust 1.75+.

Optional tool deps:

- Charts: Python 3 + `matplotlib` / `plotly`
- Office: Python 3 + `python-pptx` / `python-docx` / `openpyxl`
- Audio: `ffmpeg` + `whisper` (or whisper.cpp), or use an API backend
- Browser automation: Chrome/Chromium (CDP)
- macOS-only tools: `chrome_control`, `app_control`

Run:

```bash
blockcell onboard
# Edit ~/.blockcell/config.json and set one provider apiKey (e.g. providers.openrouter.apiKey)
blockcell status
blockcell agent
```

Daemon mode (channels + cron + heartbeat):

```bash
blockcell gateway
```

Default ports (from config defaults):

- API server: `0.0.0.0:18790`
- WebUI: `localhost:18791`

## Configuration

`blockcell onboard` creates `~/.blockcell/config.json`. Usually you only need to fill `providers.<name>.apiKey`.

Minimal example (key fields only):

```json
{
  "providers": {
    "openrouter": {
      "apiKey": "YOUR_KEY",
      "apiBase": "https://openrouter.ai/api/v1"
    }
  },
  "agents": {
    "defaults": {
      "model": "anthropic/claude-sonnet-4-20250514"
    }
  }
}
```

## Notes

- In interactive mode, file/exec tools that touch paths outside `~/.blockcell/workspace` require explicit confirmation.
- Gateway mode does not prompt for path access; paths outside workspace are denied by default.
- Gateway authentication:
  - If `gateway.apiToken` is set, call APIs with `Authorization: Bearer <token>` (or `?token=<token>`).
  - WebUI login uses the same token as password.
- Channel modules are behind Cargo features (enabled by default in `bin/blockcell`): `telegram`, `whatsapp`, `feishu`, `slack`, `discord`.

## License

MIT
