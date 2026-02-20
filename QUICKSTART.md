# Quick Start

This repo contains **blockcell**, a self-evolving AI agent framework in Rust.

- It runs as an interactive CLI (`blockcell agent`) or a daemon (`blockcell gateway`).
- It supports tool-calling, a built-in tool registry, background tasks/subagents, and a WebUI.

## 1) Install

### Option A: Install script (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/refs/heads/main/install.sh | sh
```

By default, this installs `blockcell` to `~/.local/bin`.

### Option B: Build from source

Prereqs: Rust 1.75+

```bash
cargo build -p blockcell --release
```

The binary will be at `target/release/blockcell`.

## 2) Create config

Run onboarding once:

```bash
blockcell onboard
```

Then edit `~/.blockcell/config.json` and set **one** provider API key (for example `providers.openrouter.apiKey`).

## 3) Run (interactive)

```bash
blockcell status
blockcell agent
```

Tips:

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

- HTTP: `Authorization: Bearer <token>` (or `?token=<token>`)
- WebUI: login password is the same token

## Screenshots

![Start gateway](screenshot/start-gateway.png)

![WebUI login](screenshot/webui-login.png)

![WebUI chat](screenshot/webui-chat.png)
