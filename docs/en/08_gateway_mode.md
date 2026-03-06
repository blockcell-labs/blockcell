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
[2025-02-18 08:00:00] API server: http://0.0.0.0:18790
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

## HTTP API

Gateway provides a concise REST API:

### `POST /v1/chat` — send a message

```bash
curl -X POST http://localhost:18790/v1/chat \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer YOUR_TOKEN" \
  -d '{
    "message": "Check Moutai’s stock price today"
  }'
```

Response:

```json
{
  "reply": "Moutai (600519) today: 1,680.00 CNY, change: +1.23%",
  "task_id": "msg_abc123",
  "tools_used": ["finance_api"]
}
```

### `GET /v1/health` — health check

```bash
curl http://localhost:18790/v1/health
```

```json
{
  "status": "ok",
  "uptime": 3600,
  "version": "0.x.x"
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
  "summary": {
    "running": 1,
    "completed": 42,
    "failed": 0
  },
  "tasks": [
    {
      "id": "task_xyz",
      "label": "Analyze Moutai earnings",
      "status": "running",
      "started_at": "2025-02-18T08:30:00Z"
    }
  ]
}
```

### `GET /v1/ws` — WebSocket

The WebSocket endpoint supports real-time, bidirectional communication:

```javascript
const ws = new WebSocket('ws://localhost:18790/v1/ws');

// send a message
ws.send(JSON.stringify({
  "message": "Check Bitcoin price"
}));

// receive streaming replies
ws.onmessage = (event) => {
  const data = JSON.parse(event.data);
  if (data.type === 'chunk') {
    process.stdout.write(data.content);
  } else if (data.type === 'done') {
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

In the current implementation, if `gateway.apiToken` is empty, Gateway **auto-generates one on first startup and persists it to `config.json`**. That means the API is not left fully open by default, but for public deployments you should still set a deliberate long-lived token yourself.

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
curl -H "Authorization: Bearer YOUR_TOKEN" http://YOUR_HOST:18790/v1/chat
```

Or use a query parameter (useful for WebSocket):

```
ws://YOUR_HOST:18790/v1/ws?token=YOUR_TOKEN
```

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
FROM ubuntu:22.04
RUN apt-get update && apt-get install -y curl
RUN curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/refs/heads/main/install.sh | sh
COPY config.json /root/.blockcell/config.json
EXPOSE 18790 18791
CMD ["blockcell", "gateway"]
```

```bash
docker build -t blockcell .
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

def ask_ai(question: str) -> str:
    response = requests.post(
        "http://localhost:18790/v1/chat",
        headers={"Authorization": "Bearer YOUR_TOKEN"},
        json={"message": question}
    )
    return response.json()["reply"]

answer = ask_ai("Check Moutai’s stock price today")
print(answer)
```

### Call from Node.js

```javascript
const fetch = require('node-fetch');

async function askAI(question) {
  const response = await fetch('http://localhost:18790/v1/chat', {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': 'Bearer YOUR_TOKEN'
    },
    body: JSON.stringify({ message: question })
  });
  const data = await response.json();
  return data.reply;
}
```

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
