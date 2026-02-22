# Article 06: Multi-Channel Access — Telegram/Slack/Discord/Feishu/DingTalk/WeCom and more

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — 6/16

---

## Why multi-channel matters

Chatting with an AI in a terminal is convenient, but in many scenarios you want:

- Send a message from your phone via Telegram and have the AI check stock prices
- @mention an AI in a company Slack channel so it can answer team questions
- Have the AI proactively push messages when a stock drops, instead of you asking

That’s what blockcell’s multi-channel system is for: **make the AI work inside the communication tools you already use.**

---

## Supported channels

blockcell currently supports 8 messaging channels:

| Channel | Protocol | Typical usage |
|------|------|---------|
| Telegram | Bot API polling | personal use, mobile |
| WhatsApp | bridge WebSocket | personal/business |
| Feishu (Feishu CN) | long-connection WebSocket | enterprise/internal |
| Lark (international) | webhook callback | global teams |
| Slack | Web API polling | team collaboration |
| Discord | Gateway WebSocket | community/developers |
| DingTalk | Stream SDK (WebSocket) | CN enterprises |
| WeCom (WeChat Work) | webhook callback / polling | CN enterprises |

---

## Architecture

```
External message → channel adapter → InboundMessage → Agent Runtime → processing
                                                         ↓
External message ← channel manager ← OutboundMessage ← Agent Runtime ← result
```

Each channel implements the same interface:
- **Receive**: convert platform messages into `InboundMessage`
- **Send**: convert `OutboundMessage` into platform-specific format and deliver

This way, Agent Runtime does not need to care which channel is used — behavior stays consistent.

---

## Configure Telegram

### Step 1: create a bot

1. Search `@BotFather` in Telegram
2. Send `/newbot` and follow the prompts
3. Get the Bot Token (format: `1234567890:ABCdefGHIjklMNOpqrsTUVwxyz`)

### Step 2: get your user ID

1. Search `@userinfobot` and send any message
2. It replies with your numeric user ID

### Step 3: configure blockcell

Edit `~/.blockcell/config.json`:

```json
{
  "channels": {
    "telegram": {
      "botToken": "1234567890:ABCdefGHIjklMNOpqrsTUVwxyz",
      "allowFrom": ["YOUR_USER_ID"],
      "pollIntervalSecs": 2
    }
  }
}
```

`allowFrom` is an allowlist. Only user IDs in this list can control your AI. **You must set it — otherwise anyone can control your AI bot.**

### Step 4: start the gateway

```bash
blockcell gateway
```

Now open Telegram and send a message to your bot — it will reply.

---

## Configure Slack

### Step 1: create a Slack app

1. Visit https://api.slack.com/apps
2. Click “Create New App” → “From scratch”
3. Choose an app name and workspace

### Step 2: configure permissions

In “OAuth & Permissions”, add these Bot Token Scopes:
- `channels:history`
- `chat:write`
- `users:read`

### Step 3: install into the workspace

Click “Install to Workspace” and obtain the Bot User OAuth Token (`xoxb-...`).

### Step 4: configure blockcell

```json
{
  "channels": {
    "slack": {
      "botToken": "xoxb-YOUR-BOT-TOKEN",
      "channels": ["C0123456789"],
      "allowFrom": ["U0123456789"],
      "pollIntervalSecs": 5
    }
  }
}
```

`channels` are the channel IDs you want to monitor (visible in channel details).

---

## Configure Discord

### Step 1: create a Discord bot

1. Visit https://discord.com/developers/applications
2. Click “New Application”
3. Go to the “Bot” page and click “Add Bot”
4. Copy the Bot Token

### Step 2: enable intents

On the “Bot” page, enable the privileged Gateway intent:
- `MESSAGE CONTENT INTENT`

### Step 3: invite the bot to your server

In “OAuth2 → URL Generator”:
- Scopes: `bot`
- Bot Permissions: `Send Messages`, `Read Message History`

Copy the generated URL, open it in a browser, and select your server.

### Step 4: configure blockcell

```json
{
  "channels": {
    "discord": {
      "botToken": "YOUR_BOT_TOKEN",
      "channels": ["1234567890123456789"],
      "allowFrom": ["YOUR_DISCORD_USER_ID"]
    }
  }
}
```

Discord uses a **WebSocket Gateway**, providing better real-time performance (no polling required).

---

## Configure Feishu (Feishu CN)

Feishu configuration is slightly more complex and typically requires enterprise admin privileges:

```json
{
  "channels": {
    "feishu": {
      "appId": "cli_xxx",
      "appSecret": "YOUR_APP_SECRET",
      "verificationToken": "VERIFICATION_TOKEN",
      "encryptKey": "ENCRYPTION_KEY (optional)",
      "allowFrom": ["user_open_id_1"]
    }
  }
}
```

In this implementation, Feishu receives messages via the **Open Platform long-connection WebSocket** (so you typically do not need a public callback URL).

It is strongly recommended to configure `allowFrom` (Feishu OpenID allowlist).

---

## Configure Lark (international)

For international Lark, inbound messages are received via **webhook callbacks**. Typically you need:

- A publicly reachable callback URL (or a tunnel)
- Configure the webhook URL in Lark developer console

Example configuration:

```json
{
  "channels": {
    "lark": {
      "appId": "cli_xxx",
      "appSecret": "YOUR_APP_SECRET",
      "verificationToken": "VERIFICATION_TOKEN",
      "encryptKey": "ENCRYPTION_KEY (optional)",
      "allowFrom": ["user_open_id_1"]
    }
  }
}
```

When `encryptKey` is set, Lark will encrypt webhook payloads and blockcell will decrypt them automatically.

---

## Configure DingTalk

DingTalk uses **Stream SDK (WebSocket push)** for inbound messages.

Example configuration:

```json
{
  "channels": {
    "dingtalk": {
      "appKey": "YOUR_APP_KEY",
      "appSecret": "YOUR_APP_SECRET",
      "robotCode": "optional: used for sending messages to users",
      "allowFrom": ["USER_ID"]
    }
  }
}
```

---

## Configure WeCom (WeChat Work)

WeCom supports two modes:

- **Webhook callback (recommended)**: WeCom pushes messages to your callback URL (requires public address / tunnel)
- **Polling (degraded)**: without callback, it mainly performs token heartbeat and cannot reliably receive app messages

Example configuration:

```json
{
  "channels": {
    "wecom": {
      "corpId": "YOUR_CORP_ID",
      "corpSecret": "YOUR_SECRET",
      "agentId": 1000002,
      "callbackToken": "CALLBACK_TOKEN (signature verification)",
      "encodingAesKey": "ENCODING_AES_KEY (message decryption)",
      "allowFrom": ["USER_ID"],
      "pollIntervalSecs": 10
    }
  }
}
```

---

## Running multiple channels

blockcell can run multiple channels at the same time:

```json
{
  "channels": {
    "telegram": { "botToken": "..." },
    "slack": { "botToken": "..." },
    "discord": { "botToken": "..." }
  }
}
```

After starting `blockcell gateway`, all configured channels run concurrently.

---

## Proactive push notifications

This is one of the most powerful features: **the AI can proactively message you.**

Combined with the alert system, you can set rules such as:

```
You: Create an alert: when Moutai drops below 1500,
    send me a Telegram message
```

The AI creates an alert rule like:

```json
{
  "name": "Moutai below 1500",
  "condition": {
    "tool": "finance_api",
    "params": {"action": "stock_quote", "symbol": "600519"},
    "field": "price",
    "operator": "lt",
    "threshold": 1500
  },
  "on_trigger": [
    {
      "tool": "notification",
      "params": {
        "channel": "telegram",
        "message": "⚠️ Moutai is below 1500! Current price: {value}"
      }
    }
  ]
}
```

When triggered, blockcell automatically sends the notification via Telegram.

---

## Check channel status

```bash
blockcell channels status
```

Example output:

```
Channels status:
  telegram  ✓ running (polled 1234 times, last: 2s ago)
  slack     ✓ running (polled 567 times, last: 5s ago)
  discord   ✓ connected (WebSocket, latency: 45ms)
  whatsapp  ✗ not configured
  feishu    ✗ not configured
```

---

## Security notes

### 1) Always configure an allowlist

```json
"allowFrom": ["YOUR_USER_ID"]
```

Not setting an allowlist means anyone can control your AI — extremely dangerous.

### 2) Path restrictions in Gateway mode

In Gateway mode (`blockcell gateway`), the AI cannot access files outside the workspace. This is a deliberate safety boundary to prevent leaking private files through message channels.

### 3) Protect the Gateway with an API token

If your Gateway is exposed publicly, you must set an API token:

```json
{
  "gateway": {
    "apiToken": "a long random string"
  }
}
```

---

## Practical usage scenarios

### Scenario 1: remote control from your phone

When you’re away from your computer, send a Telegram message:

```
You: Check the server CPU usage
AI: [exec "top -bn1 | head -20"]
    CPU usage: 23%, memory: 4.2GB/16GB
```

### Scenario 2: an AI assistant for your team

In a Slack channel:

```
@blockcell Summarize today’s GitHub PR list
AI: [git_api list_prs]
    Today there are 5 PRs...
```

### Scenario 3: automated reports

Every day at 8am, blockcell generates a daily finance report and sends it via Telegram:

```
[cron trigger]
AI: generating today’s finance report...
    [send to Telegram]
📊 Daily Finance Report 2025-02-18
Index: SSE +0.5%, SZSE +0.8%
...
```

---

## Summary

blockcell’s multi-channel system integrates AI into your daily workflow:

- **Reactive**: message in Telegram/Slack/Discord → instant reply
- **Proactive**: combined with alerts → AI notifies you automatically
- **Unified runtime**: all channels share one Agent Runtime → consistent behavior
- **Security isolation**: allowlists + Gateway path restrictions

Next, we’ll cover one of blockcell’s coolest features: browser automation — letting AI control web pages for you.

---

*Previous: [The memory system — letting AI remember what you said](./05_memory_system.md)*
*Next: [Browser automation — letting AI control the web for you](./07_browser_automation.md)*

*Repo: https://github.com/blockcell-labs/blockcell*
*Website: https://blockcell.dev*
