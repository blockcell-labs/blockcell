# Article 08: Gateway Mode — Turning AI into a Service

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — Article 8
---

## Two runtime modes

blockcell has two ways to run:

**`blockcell agent`** — interactive mode
- Chat with the AI in your terminal
- Best for personal use and development/debugging
- The AI works while you’re there

**`blockcell gateway`** — daemon mode
- Runs continuously in the background
- Provides HTTP API, WebSocket, and a WebUI
- Maintains a runtime pool segmented by agent
- Listens on external channels (Telegram/Slack/Discord/etc.)
- Runs scheduled tasks (Cron)
- The AI keeps working even when you’re not present

This article introduces Gateway mode.

---

## Starting the Gateway

```bash
blockcell gateway
```

After it starts, you’ll see logs like:

```
[2025-02-18 08:00:00] Gateway starting...
[2025-02-18 08:00:00] API server: http://localhost:18790
[2025-02-18 08:00:00] WebUI: http://localhost:18791
[2025-02-18 08:00:00] Telegram: connected (polling)
[2025-02-18 08:00:00] Discord: connected (WebSocket)
[2025-02-18 08:00:00] Cron: 3 jobs scheduled
[2025-02-18 08:00:00] Gateway ready.
```

Default ports:
- **18790**: API server (HTTP)
- **18791**: WebUI (browser UI)

Default routing rules:
- Internal requests from CLI / WebSocket / WebUI go to the `default` agent
- External channel traffic first checks `channelAccountOwners.<channel>.<accountId>` and falls back to `channelOwners.<channel>`
- Any enabled external channel without an owner makes Gateway fail fast at startup

For example, a **2-bot / 2-agent Telegram** setup can be routed like this:

```json
{
  "channelAccountOwners": {
    "telegram": {
      "bot1": "default",
      "bot2": "ops"
    }
  }
}
```

In that case, Gateway dispatches messages from `bot1` to the `default` runtime and messages from `bot2` to the `ops` runtime, even though both belong to the same `telegram` channel.

---

## Slash Commands in Gateway and Channels

Since v0.1.6, Gateway/WebSocket and external channels share the same slash-command handler as the CLI. When a user sends `/help`, `/tasks`, `/skills`, `/tools`, `/clear`, and similar commands from Telegram, Slack, Discord, Feishu, DingTalk, or another channel, Gateway handles the command locally first and replies back to the original channel.

This has two practical effects:

- Common status/query commands do not enter the LLM, so they are faster and token-free.
- CLI, WebUI/WebSocket, and external channels keep the same command behavior.

Built-in commands include:

| Command | Description |
|------|------|
| `/help` | Show command list |
| `/tasks [status]` | List background tasks |
| `/skills` | List loaded skills |
| `/tools` | List loaded tools |
| `/learn <description>` | Ask the Agent to learn a skill; uses the LLM |
| `/clear` | Clear current session history |
| `/compact` | Manually trigger history compression |
| `/session-metrics` | Show 7-layer memory-system metrics |
| `/log ...` | Control logging output at runtime |

`/quit` and `/exit` are CLI-only. Command names must match exactly; a no-argument command with extra text is treated as a normal message.

---

## HTTP API

Gateway provides a concise REST API:

### `POST /v1/chat` — send a message

```bash
curl -X POST http://localhost:18790/v1/chat \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer YOUR_TOKEN" \
  -d '{
    "content": "Check Moutai’s stock price today",
    "chat_id": "finance-demo"
  }'
```

Gateway queues the message asynchronously, so a successful request returns `202 Accepted`:

```json
{
  "status": "accepted",
  "message": "Message queued for processing",
  "session_id": "finance-demo"
}
```

The final reply is delivered asynchronously through WebSocket or the originating message channel; it is not returned in this HTTP response.

### `GET /v1/health` — health check

```bash
curl http://localhost:18790/v1/health
```

```json
{
  "status": "ok",
  "model": "deepseek-v4-pro",
  "uptime_secs": 3600,
  "version": "0.1.8"
}
```

This endpoint does not require auth and is meant for Kubernetes/load balancer health probes.

### `GET /v1/tasks` — list tasks

```bash
curl http://localhost:18790/v1/tasks \
  -H "Authorization: Bearer YOUR_TOKEN"
```

```json
{
  "queued": 0,
  "running": 1,
  "completed": 42,
  "failed": 0,
  "tasks": []
}
```

### `GET /v1/ws` — WebSocket

The WebSocket endpoint supports real-time, bidirectional communication:

```javascript
const token = 'YOUR_TOKEN';
const tokenHex = Array.from(new TextEncoder().encode(token), byte =>
  byte.toString(16).padStart(2, '0')
).join('');
const ws = new WebSocket(
  'ws://localhost:18790/v1/ws',
  [`blockcell-auth.${tokenHex}`],
);

// send a message
ws.send(JSON.stringify({
  type: 'chat',
  content: 'Check Bitcoin price',
  chat_id: 'finance-demo',
}));

// receive streaming replies
ws.onmessage = (event) => {
  const data = JSON.parse(event.data);
  if (data.type === 'token') {
    process.stdout.write(data.delta);
  } else if (data.type === 'message_done') {
    console.log('\nDone');
  } else if (data.type === 'skills_updated') {
    console.log('Skills updated:', data.new_skills);
  }
};
```

WebSocket supports **streaming output**, so the AI’s reply arrives chunk by chunk for a smoother experience.

Gateway also exposes:

- `GET /v1/channels/status` — current channel connection status
- `GET /v1/channel-owners` — inspect channel and account-level owner bindings
- `PUT /v1/channel-owners/:channel` — change a channel fallback owner
- `DELETE /v1/channel-owners/:channel` — remove a channel fallback owner
- `PUT /v1/channel-owners/:channel/accounts/:account_id` — set an account-level owner
- `DELETE /v1/channel-owners/:channel/accounts/:account_id` — clear an account-level owner

---

## WebUI

Visit `http://localhost:18791` to access the Web dashboard.

```
┌─────────────────────────────────────────────────────┐
│  blockcell Dashboard                           [Logout]│
├──────────┬──────────────────────────────────────────┤
│          │                                          │
│ Sidebar  │  Main Content                            │
│          │                                          │
│ 💬 Chat  │  [Chat / Tasks / Skills / ...]           │
│ 📋 Tasks │                                          │
│ 🔧 Tools │                                          │
│ 🧠 Skills│                                          │
│ 📊 Evo   │                                          │
│ ⚙️ Settings │                                       │
└──────────┴──────────────────────────────────────────┘
```

Main features:
- Chat UI in the browser
- Task monitoring
- Skill management (enable/disable)
- Evolution history
- Real-time events via WebSocket (e.g., skills updates)

---

## API authentication

In the current implementation, if `gateway.apiToken` is empty, Gateway **auto-generates one on first startup and persists it to `config.json5`**. That means the API is not left fully open by default, but for public deployments you should still set a deliberate long-lived token yourself.

```json
{
  "gateway": {
    "apiToken": "a long random string (at least 32 chars)",
    "webuiPass": "optional dedicated WebUI password"
  }
}
```

Include the token in the `Authorization` header:

```bash
curl -H "Authorization: Bearer YOUR_TOKEN" http://YOUR_HOST:18790/v1/tasks
```

Browser WebSockets cannot set an arbitrary Authorization header. Encode the token as UTF-8 hexadecimal and carry it in the negotiated WebSocket subprotocol:

```
blockcell-auth.<UTF-8 hexadecimal token>
```

See the complete JavaScript connection example in the WebSocket section above. Non-browser clients may instead send the standard `Authorization: Bearer ...` header in the upgrade request. Do not put the token in the URL query: Gateway does not read it, and URLs are commonly recorded in access logs.

WebUI authentication is now separate from the API token:

- if `gateway.webuiPass` is set, WebUI uses that stable password
- otherwise Gateway prints a temporary password at startup
- `apiToken` continues to protect API and WebSocket access

---

## Scheduled tasks (Cron)

In Gateway mode, scheduled tasks run automatically.

### Create a scheduled task

```
You: Create a cron job that generates the daily finance report at 8am
    and sends it to me via Telegram
```

The AI will create a cron entry like:

```json
{
  "schedule": "0 8 * * *",
  "task": "Generate today’s finance report (indexes, hot sectors, watchlist) and send via Telegram",
  "enabled": true
}
```

### Manage cron jobs

```bash
# List all cron jobs
blockcell cron list

# Example output:
# ID          SCHEDULE        LAST_RUN              STATUS
# daily_report 0 8 * * *      2025-02-18 08:00:00   ✓ success
# price_check  */10 * * * *   2025-02-18 08:50:00   ✓ success
```

---

## Deploying to a server

### With systemd (Linux)

Create `/etc/systemd/system/blockcell.service`:

```ini
[Unit]
Description=blockcell AI Gateway
After=network.target

[Service]
Type=simple
User=YOUR_USER
ExecStart=/home/YOUR_USER/.local/bin/blockcell gateway
Restart=always
RestartSec=10
Environment=HOME=/home/YOUR_USER

[Install]
WantedBy=multi-user.target
```

Start the service:

```bash
sudo systemctl enable blockcell
sudo systemctl start blockcell
sudo systemctl status blockcell
```

### With Docker

```dockerfile
FROM --platform=linux/amd64 ubuntu:22.04
RUN apt-get update && apt-get install -y curl ca-certificates tar \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/refs/heads/main/install.sh \
    | BLOCKCELL_INSTALL_METHOD=release BLOCKCELL_INSTALL_DIR=/usr/local/bin sh
EXPOSE 18790 18791
CMD ["blockcell", "gateway"]
```

The container's `~/.blockcell/config.json5` must bind both services to all container interfaces; otherwise Docker port publishing cannot reach them:

```json5
{
  gateway: {
    host: "0.0.0.0",
    port: 18790,
    webuiHost: "0.0.0.0",
    webuiPort: 18791,
  },
}
```

```bash
docker build -t blockcell .
# The host ~/.blockcell/config.json5 must contain the container binding settings above
docker run -d \
  -p 18790:18790 \
  -p 18791:18791 \
  -v ~/.blockcell:/root/.blockcell \
  blockcell
```

### With Nginx reverse proxy

```nginx
server {
    listen 443 ssl;
    server_name ai.yourdomain.com;

    ssl_certificate /path/to/cert.pem;
    ssl_certificate_key /path/to/key.pem;

    location /v1/ {
        proxy_pass http://localhost:18790;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
    }

    location / {
        proxy_pass http://localhost:18791;
    }
}
```

---

## Integrating with other apps

Gateway mode turns blockcell into a standard HTTP service, making integration straightforward.

### Call from Python

```python
import requests

def enqueue_ai(question: str, session_id: str) -> dict:
    response = requests.post(
        "http://localhost:18790/v1/chat",
        headers={"Authorization": "Bearer YOUR_TOKEN"},
        json={"content": question, "chat_id": session_id}
    )
    response.raise_for_status()
    return response.json()

# This returns queue status; receive the final reply over WebSocket or a message channel
accepted = enqueue_ai("Check Moutai’s stock price today", "finance-demo")
print(accepted["session_id"])
```

### Call from Node.js

```javascript
const fetch = require('node-fetch');

async function enqueueAI(question, sessionId) {
  const response = await fetch('http://localhost:18790/v1/chat', {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': 'Bearer YOUR_TOKEN'
    },
    body: JSON.stringify({ content: question, chat_id: sessionId })
  });
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return response.json();
}
```

The HTTP endpoint only confirms that the message was queued. Use the WebSocket protocol above to receive the final reply.

---

## Gateway vs Agent mode

| Feature | Agent mode | Gateway mode |
|------|-----------|-------------|
| Start command | `blockcell agent` | `blockcell gateway` |
| Interaction | CLI | HTTP API / WebSocket / message channels |
| Scheduled tasks | ❌ | ✅ |
| Message channels | ❌ | ✅ |
| Path safety | prompts for confirmation | denies outside-workspace access |
| Best for | development/debugging | production deployment |
| WebUI | ❌ | ✅ |

---

## Summary

Gateway mode turns blockcell from a CLI tool into a complete AI service:

- **HTTP API**: standard REST interfaces
- **WebSocket**: real-time streaming output
- **WebUI**: browser dashboard
- **Scheduled tasks**: Cron scheduling for automation
- **Message channels**: Telegram/Slack/Discord
- **Security**: token auth + path isolation

Next, we’ll look at blockcell’s most unique feature: the self-evolution system — how the AI writes code to upgrade itself.

---

*Previous: [Browser automation — let AI control the web for you](./07_browser_automation.md)*
*Next: [Self-evolution — how AI writes code to upgrade itself](./09_self_evolution.md)*

*Repo: https://github.com/blockcell-labs/blockcell*
*Website: https://blockcell.dev*
