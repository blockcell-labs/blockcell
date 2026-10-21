# Multi-Agent Quick Start

This guide shows a **multi-agent** BlockCell deployment using:

- 2 agents: `default` and `ops`
- 2 Telegram bot accounts: `main_bot` and `ops_bot`
- channel-level fallback plus account-level routing

If you want the simplest setup, use `QUICKSTART.md` instead. That guide is the recommended **single-agent** path.

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

For first-time setup, you can still start with:

```bash
blockcell setup
```

Then edit `~/.blockcell/config.json5` into a multi-agent layout like this:

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
      "model": "deepseek-chat"
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
  "channels": {
    "telegram": {
      "enabled": true,
      "accounts": {
        "main_bot": {
          "enabled": true,
          "token": "123456:MAIN_BOT_TOKEN",
          "allowFrom": ["alice", "team_group"]
        },
        "ops_bot": {
          "enabled": true,
          "token": "123456:OPS_BOT_TOKEN",
          "allowFrom": ["oncall_group", "sre_team"]
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

## 3) How this routing works

- `default` is your general-purpose agent.
- `ops` is your operations-focused agent.
- `channels.telegram.accounts` defines the two Telegram bot accounts that will actually connect.
- `channelOwners.telegram = default` means Telegram falls back to `default` when no account override exists.
- `channelAccountOwners.telegram.ops_bot = ops` sends messages from `ops_bot` to the `ops` agent.

Routing order:

- CLI / WebUI / WebSocket requests go to `default` unless you explicitly choose another agent.
- External channels first check `channelAccountOwners.<channel>.<accountId>`.
- If no account-level match exists, they fall back to `channelOwners.<channel>`.
- Every enabled external channel must have either a channel-level owner or owner mappings for all enabled accounts. Otherwise, the Gateway rejects the configuration at startup.

## 4) About tool separation

Agent config does not currently expose a direct per-agent `tools` field.

In practice, separate agents by:

- `intentProfile`
- model / provider overrides
- MCP allowlists (`allowedMcpServers`, `allowedMcpTools`)
- account and channel routing

If you need stricter tool boundaries, enforce them through intent profiles and runtime policy rather than adding unsupported config fields.

## 5) Run a specific agent in interactive mode

```bash
blockcell status
blockcell agent -a default
blockcell agent -a ops
```

Tips:

- Use `-a <agent_id>` to enter a specific agent in interactive mode.
- The default CLI session id is `cli:<agent>`.

## 6) Run the daemon + WebUI

```bash
blockcell gateway
```

Default ports:

- API server: `http://localhost:18790`
- WebUI: `http://localhost:18791`

If `gateway.apiToken` is set, use it as:

- HTTP: `Authorization: Bearer <token>`
- WebSocket: `?token=<token>` also works

## Recommended rollout order

- Start with only `main_bot` enabled and routed to `default`.
- Confirm the gateway works correctly.
- Enable `ops_bot` only after the first bot is stable.
- Keep the `ops` intent profile narrower than `default`.
