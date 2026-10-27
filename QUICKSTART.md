# Quick Start

This repo contains **blockcell**, a self-evolving AI agent framework in Rust.

- It runs as an interactive CLI (`blockcell agent`) or a daemon (`blockcell gateway`).
- It supports tool-calling, a built-in tool registry, background tasks/subagents, and a WebUI.

This guide is the recommended **single-agent best practice**.

If you want multi-agent routing with multiple channel accounts, use `QUICKSTART.multi-agent.md` instead.

## 1) Install

### Option A: Install script (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/refs/heads/main/install.sh | sh
```

By default, this installs `blockcell` to `~/.local/bin`.

### Option B: Build from source

Prereqs: Rust 1.96+

```bash
cargo build -p blockcell --release
```

The binary will be at `target/release/blockcell`.

## 2) Create config

For first-time setup, the recommended flow is:

```bash
blockcell setup
```

That creates `~/.blockcell/`, saves provider settings, and can also enable one external channel for you.

Minimal example (`~/.blockcell/config.json5`):

```json
{
  "providers": {
    "deepseek": {
      "apiKey": "YOUR_DEEPSEEK_API_KEY",
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

Single-agent best practices:

- Keep everything on the implicit `default` agent first.
- Do not add `agents.list` unless you actually need separate routing or behavior.
- Start without external channels, or enable only one channel until the core workflow is stable.
- Keep `gateway.apiToken` and `gateway.webuiPass` set before exposing the daemon beyond localhost.

Optional single Telegram channel example:

```json
{
  "providers": {
    "deepseek": {
      "apiKey": "YOUR_DEEPSEEK_API_KEY",
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
  },
  "channels": {
    "telegram": {
      "enabled": true,
      "token": "123456:SINGLE_BOT_TOKEN",
      "allowFrom": ["alice"]
    }
  },
  "channelOwners": {
    "telegram": "default"
  },
  "gateway": {
    "apiToken": "YOUR_STABLE_API_TOKEN",
    "webuiPass": "YOUR_WEBUI_PASSWORD"
  }
}
```

## 3) Run (interactive)

```bash
blockcell status
blockcell agent
```

Tips:

- `blockcell agent` enters the implicit `default` agent.
- Type `/tasks` to see background tasks.
- Type `/quit` to exit.

## 4) Run (daemon + WebUI)

Start the gateway:

```bash
blockcell gateway
```

Default ports:

- API server: `http://localhost:18790`
- WebUI: `http://localhost:18791`

If `gateway.apiToken` is set, use it as:

- HTTP: `Authorization: Bearer <token>`
- WebSocket: `?token=<token>` also works

WebUI authentication is now separate from the API token:

- if `gateway.webuiPass` is set, WebUI uses that stable password
- otherwise Gateway prints a temporary password at startup
- if `gateway.apiToken` is empty, Gateway auto-generates and persists one

## Screenshots

![Start gateway](screenshot/start-gateway.png)

![WebUI login](screenshot/webui-login.png)

![WebUI chat](screenshot/webui-chat.png)
