# Lark (International Version) Bot Configuration Guide

Blockcell receives Lark international version messages via **HTTP Webhook (Callback)** method.

> **Important Note**
>
> - **International Lark** (`open.larksuite.com`) — Only supports HTTP Webhook, requires publicly accessible URL
> - **Feishu** (`open.feishu.cn`) — Supports WebSocket long connection, no public IP needed, see [04_feishu.md](./04_feishu.md)
>
> If your app is created on the Feishu platform, please use the `feishu` channel instead of the `lark` channel.

---

## Prerequisites

- Blockcell gateway deployed on a **publicly accessible** server (or exposed via ngrok/frp intranet penetration tools)
- Webhook URL format: `https://your-domain.com/webhook/lark`

---

## Configuration Steps

### 1. Create a Lark App

1. Visit [Lark Developer Console](https://open.larksuite.com/app)
2. Click **Create App** → **Custom App**
3. Fill in app name and description, click **Create**
4. Record **App ID** and **App Secret** (on Credentials & Basic Info page)

### 2. Enable Bot Capability

1. Go to app → **Features** → **Bot**
2. Click **Enable Bot**

### 3. Apply for Permissions

Go to **Permissions & Scopes**, search for and add the following permissions:

| Permission | Description |
|------|------|
| `im:message` | Receive messages |
| `im:message:send_as_bot` | Send messages |
| `im:chat` | Access group info |

### 4. Configure Event Subscriptions

1. Go to **Event Subscriptions**
2. **Encrypt Key** (optional but recommended): Fill in a random string for encrypting webhook content
3. **Verification Token**: Record this value (for verifying request source)
4. **Request URL**: Fill in `https://your-domain.com/webhook/lark`
5. Click **Verify** — Lark will send a challenge request, Blockcell will automatically respond
6. Add event: Click **Add Event** → Search for `im.message.receive_v1` → Add

### 5. Publish App

1. Go to **App Release** → **Version Management & Release**
2. Click **Create Version**, fill in version info
3. Submit for review (custom apps usually take effect immediately)

### 6. Configure Blockcell

Edit `~/.blockcell/config.json`:

```json
{
  "channels": {
    "lark": {
      "enabled": true,
      "appId": "cli_xxxxxxxxxxxxxxxxx",
      "appSecret": "your_app_secret_here",
      "encryptKey": "your_encrypt_key_here",
      "verificationToken": "your_verification_token_here",
      "allowFrom": []
    }
  }
}
```

| Field | Description |
|------|------|
| `appId` | App ID (required) |
| `appSecret` | App Secret (required) |
| `encryptKey` | Encryption key, must match Encrypt Key in Lark backend (recommended) |
| `verificationToken` | Verification Token (currently for recording, will be used for request verification in future versions) |
| `allowFrom` | Allowed user open_id allowlist, empty array allows everyone |

### 7. Start Gateway

```bash
blockcell gateway
```

After starting, you'll see output like:

```
  Server
  HTTP/WS:  http://0.0.0.0:18790
  WebUI:   http://localhost:18791/
  API:     POST http://0.0.0.0:18790/v1/chat  |  GET /v1/health  |  GET /v1/ws

  ✓ Gateway ready. Press Ctrl+C to stop.
```

Where `0.0.0.0:18790` is the internal address Blockcell listens on, Lark webhook path is:

```
http://0.0.0.0:18790/webhook/lark   ← Internal address (Lark cannot access directly)
https://your-domain.com/webhook/lark ← Needs to be exposed to public via Nginx reverse proxy
```

---

## Network Configuration: Expose Webhook to Public

Lark servers need to actively access your `/webhook/lark` endpoint, so you must have a publicly accessible HTTPS URL.

### Option 1: Nginx Reverse Proxy (Recommended for Production)

Assuming your server's public domain is `bot.example.com` and Blockcell gateway runs on local port `18790`.

**Nginx Configuration File** `/etc/nginx/sites-available/blockcell`:

```nginx
server {
    listen 80;
    server_name bot.example.com;
    # Force redirect to HTTPS (Lark requires HTTPS)
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl;
    server_name bot.example.com;

    # SSL certificate (recommended Let's Encrypt)
    ssl_certificate     /etc/letsencrypt/live/bot.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/bot.example.com/privkey.pem;
    ssl_protocols       TLSv1.2 TLSv1.3;
    ssl_ciphers         HIGH:!aNULL:!MD5;

    # Only proxy Lark webhook path (other paths not exposed)
    location /webhook/lark {
        proxy_pass         http://127.0.0.1:18790/webhook/lark;
        proxy_http_version 1.1;
        proxy_set_header   Host              $host;
        proxy_set_header   X-Real-IP         $remote_addr;
        proxy_set_header   X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto $scheme;
        proxy_read_timeout 30s;
    }

    # If you also want to expose API (optional, recommend adding api_token protection)
    # location /v1/ {
    #     proxy_pass http://127.0.0.1:18790/v1/;
    #     proxy_http_version 1.1;
    #     proxy_set_header Host $host;
    # }
}
```

Enable configuration and reload Nginx:

```bash
# Create symlink
sudo ln -s /etc/nginx/sites-available/blockcell /etc/nginx/sites-enabled/

# Apply for SSL certificate (first time)
sudo certbot --nginx -d bot.example.com

# Test configuration
sudo nginx -t

# Reload
sudo systemctl reload nginx
```

After configuration, fill in Lark developer backend:

```
https://bot.example.com/webhook/lark
```

**Verify accessibility:**

```bash
curl -X POST https://bot.example.com/webhook/lark \
  -H "Content-Type: application/json" \
  -d '{"type":"url_verification","challenge":"test123"}'
# Expected return: {"challenge":"test123"}
```

---

### Option 2: ngrok (Local Development / Temporary Testing)

No server needed, suitable for local debugging.

```bash
# Install ngrok (https://ngrok.com)
brew install ngrok

# Start tunnel (pointing to Blockcell gateway port)
ngrok http 18790
```

ngrok will output something like:

```
Forwarding  https://abc123.ngrok-free.app -> http://localhost:18790
```

Fill in Lark developer backend:

```
https://abc123.ngrok-free.app/webhook/lark
```

> ⚠️ **Note**: Free ngrok address changes on restart, need to update URL in Lark backend each time. Use fixed domain solution for production.

---

### Option 3: frp Intranet Penetration

Suitable for scenarios without public IP but with a public server.

**Public Server** `frps.ini`:

```ini
[common]
bind_port = 7000
```

**Local Machine** `frpc.ini`:

```ini
[common]
server_addr = your-public-server-ip
server_port = 7000

[blockcell-lark]
type = http
local_ip = 127.0.0.1
local_port = 18790
custom_domains = bot.example.com
```

Then use Nginx on the public server to proxy frp's HTTP port, configuration same as Option 1.

---

### Port Description

Blockcell gateway default ports:

| Port | Purpose | Public Exposure |
|------|------|----------|
| `18790` | API + Webhook (HTTP) | Expose `/webhook/lark` via Nginx/ngrok proxy |
| `18791` | WebUI (local access only) | Not recommended for public exposure |

To modify ports, configure in `~/.blockcell/config.json`:

```json
{
  "gateway": {
    "host": "127.0.0.1",
    "port": 18790,
    "webuiHost": "127.0.0.1",
    "webuiPort": 18791
  }
}
```

> Changing `host` to `127.0.0.1` (instead of `0.0.0.0`) makes gateway only listen on local loopback, more secure — Nginx can still access via `proxy_pass http://127.0.0.1:18790`.

---

## Get User open_id (for allowFrom Allowlist)

Send a message to the bot in Lark and check Blockcell logs:

```
INFO blockcell_channels::lark: Lark webhook: inbound message chat_id=oc_xxx open_id=ou_yyy len=5
```

Fill the `open_id` (starts with `ou_`) into the `allowFrom` list.

---

## Interact with Bot

### Private Chat
Search for the bot name directly in Lark and start a private chat.

### Group
1. Add bot to group
2. **@bot** to send messages (Lark will automatically route @ messages to the bot)

---

## Webhook Encryption Description

When `encryptKey` is configured, Lark will encrypt the webhook body:

- Encryption algorithm: AES-256-CBC + PKCS7 padding
- Key: `SHA-256(encryptKey)` → 32 bytes
- IV: First 16 bytes after base64 decoding
- Blockcell handles decryption automatically, no manual operation needed

**Strongly recommend configuring `encryptKey`** to prevent webhook content from being intercepted by man-in-the-middle.

---

## Common Issues

### Webhook URL Verification Failed
- Confirm Blockcell gateway is started and publicly accessible
- Confirm URL format is correct: `https://your-domain.com/webhook/lark` (note HTTPS)
- Check if firewall/reverse proxy allows this path

### Not Receiving Messages
- Confirm `im.message.receive_v1` event subscription is added
- Confirm app is published
- Confirm bot is added to conversation/group

### Sending Messages Failed
- Check if `appId` and `appSecret` are correct
- Confirm app has `im:message:send_as_bot` permission
- Check specific error info in Blockcell logs

### Encryption/Decryption Failed
- Confirm `encryptKey` matches exactly what's filled in Lark backend (case-sensitive)
- If encryption not needed, clear Encrypt Key in Lark backend and set `encryptKey` to empty string
