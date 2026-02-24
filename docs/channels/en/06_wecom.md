# WeCom (WeChat Work) Bot Configuration Guide

Blockcell supports interacting with agents through WeCom (WeChat Work) custom apps. The WeCom channel supports both **Polling** and **Webhook (Callback)** modes to receive messages.

> **Deployment Recommendations**:
> - **Intranet/Local Development**: Recommended to use **Polling mode**, no need to configure public server or domain, works out of the box.
> - **Production Environment**: Recommended to use **Callback mode (Webhook)**, better message real-time performance, doesn't consume API call quota. Gateway has built-in `/webhook/wecom` route, supporting GET verification and POST message callbacks.

> ⚠️ **Important: Enterprise Trusted IP**
> In callback mode, WeCom's API (sending messages, etc.) only allows requests from **Enterprise Trusted IPs**. You must add your server's public IP to the whitelist in the admin backend, otherwise sending messages will fail with error `60020: not allow to access from your ip`.
> Configuration path: Admin Backend → App Management → Your App → **Enterprise Trusted IP** → Add server IP.

---

## Prerequisites (Only for Callback Mode)

- Blockcell gateway deployed on a **publicly accessible** server (or exposed via ngrok/frp intranet penetration tools)
- Webhook URL format: `https://your-domain.com/webhook/wecom`

---

## 1. Create a WeCom App

1. Log in and visit [WeCom Admin Backend](https://work.weixin.qq.com/).
2. Switch to the **App Management** tab.
3. In the **Custom** app area, click **Create App**.
4. Fill in the app name (e.g., Blockcell Bot), app logo, and description, select visibility range (e.g., all or specific departments), and click **Create App**.
5. After successful creation, on the app details page, copy and save your **AgentId** and **Secret** (viewing Secret requires confirmation in WeCom mobile app).

## 2. Get Enterprise ID (CorpId)

1. In WeCom admin backend, click **My Enterprise** in the top menu.
2. Scroll to the bottom **Enterprise Info** section, find and copy the **Enterprise ID**.

## 3. Configure Callback Mode (Webhook)

If you want to use callback mode (Webhook) instead of polling mode, you need to configure the message receiving server:

1. On the app details page, find **Receive Messages** settings and click **Set API Receiving**.
2. **URL**: Fill in your public server URL (e.g., `https://your-domain.com/webhook/wecom`).
3. **Token**: Click **Random Get** and record it.
4. **EncodingAESKey**: Click **Random Get** and record it.
5. Click **Save**. WeCom will send a `GET` request to this URL for verification. Blockcell gateway will automatically complete SHA1 signature verification and return the decrypted `echostr`.

*(If using polling mode, skip this step).*

## 4. Get User ID (for Allowlist)

WeCom's `sender_id` is usually the enterprise's account (UserID).
1. In the admin backend's **Address Book**, click on the corresponding member.
2. Find the **Account** in member details (e.g., `ZhangSan`).

## 5. Configure Blockcell

Edit `~/.blockcell/config.json` and modify the `wecom` section:

```json
{
  "channels": {
    "wecom": {
      "enabled": true,
      "corpId": "ww1a2b3c4d5e6f7g8h",
      "corpSecret": "A1B2C3D4E5F6G7H8I9J0K1L2M3N4O5P6Q7R8S9T0",
      "agentId": 1000001,
      "pollIntervalSecs": 10,
      "callbackToken": "Your callback Token (can be empty if using polling)",
      "encodingAesKey": "Your callback AESKey (can be empty if using polling)",
      "allowFrom": ["ZhangSan"]
    }
  }
}
```

### Configuration Options

| Field | Description |
|------|------|
| `enabled` | Whether to enable WeCom channel (`true` or `false`) |
| `corpId` | Enterprise ID obtained from "My Enterprise" |
| `corpSecret` | Secret obtained from app details |
| `agentId` | AgentId obtained from app details (number) |
| `pollIntervalSecs` | Polling interval (seconds). Default `10` seconds. Only effective in polling mode. Don't set too short to avoid API rate limits |
| `callbackToken` | For callback mode signature verification. Can be empty if using polling |
| `encodingAesKey` | For callback mode message encryption/decryption. Can be empty if using polling |
| `allowFrom` | List of allowed user `UserID`s. If left empty `[]`, anyone within the enterprise's visibility range can interact with the bot |

## 6. Start Gateway

```bash
blockcell gateway
```

After starting, Gateway will listen on the local port (default `18790`). If in callback mode, WeCom's Webhook path is:

```
http://0.0.0.0:18790/webhook/wecom   ← Internal address (WeCom cannot access directly)
https://your-domain.com/webhook/wecom ← Needs to be exposed to public via Nginx reverse proxy
```

---

## 7. Network Configuration: Expose Webhook to Public (Callback Mode)

WeCom servers need to actively access your `/webhook/wecom` endpoint, so you must have a publicly accessible URL (WeCom requires port 80 or 443, HTTPS recommended).

### Option 1: Nginx Reverse Proxy (Recommended for Production)

Assuming your server's public domain is `bot.example.com` and Blockcell gateway runs on local port `18790`.

**Nginx Configuration File** `/etc/nginx/sites-available/blockcell`:

```nginx
server {
    listen 80;
    server_name bot.example.com;
    # Force redirect to HTTPS
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

    # Only proxy WeCom webhook path
    location /webhook/wecom {
        proxy_pass         http://127.0.0.1:18790/webhook/wecom;
        proxy_http_version 1.1;
        proxy_set_header   Host              $host;
        proxy_set_header   X-Real-IP         $remote_addr;
        proxy_set_header   X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto $scheme;
        proxy_read_timeout 30s;
    }
}
```

Enable configuration and reload Nginx:

```bash
# Create symlink
sudo ln -s /etc/nginx/sites-available/blockcell /etc/nginx/sites-enabled/

# Apply for SSL certificate (first time)
sudo certbot --nginx -d bot.example.com

# Test configuration and reload
sudo nginx -t
sudo systemctl reload nginx
```

After configuration, fill in WeCom backend's "Receive Messages" URL: `https://bot.example.com/webhook/wecom`.

### Option 2: ngrok (Local Development / Temporary Testing)

No server needed, suitable for local debugging callback mode.

```bash
# Install ngrok
brew install ngrok

# Start tunnel (pointing to Blockcell gateway port)
ngrok http 18790
```

ngrok will output something like: `Forwarding  https://abc123.ngrok-free.app -> http://localhost:18790`

Fill in WeCom backend: `https://abc123.ngrok-free.app/webhook/wecom`.

> ⚠️ **Note**: Free ngrok address changes on restart, need to update URL in WeCom backend each time.

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

[blockcell-wecom]
type = http
local_ip = 127.0.0.1
local_port = 18790
custom_domains = bot.example.com
```

Then use Nginx on the public server to proxy frp's HTTP port, configuration same as Option 1.

---

## 8. Interaction Methods

- **Private Chat**: In WeCom client (mobile or PC), find **Workbench** -> Your app (Blockcell Bot), send messages directly.
- **Group Chat**:
  1. In WeCom mobile app, enter any internal group chat.
  2. Click group settings in top right -> **Add Group Bot** -> **Select existing bot** (if you've enabled "Anti-harassment/Bot" related features in app management), or directly add custom app to group chat (depends on your WeCom version and configuration).
  3. **Note**: WeCom has limitations on custom app performance in group chats. If sending messages to groups via app, usually requires user action trigger (and the interface for sending to groups is `appchat/send`, requires pre-created `chatid`). Blockcell will automatically recognize group chat IDs starting with `wr` and use `appchat` interface to reply.

## 9. Testing and Verification

### Connectivity Test (no parameters needed)

```bash
# Direct access, should return 200 OK, body is "ok"
curl -i http://127.0.0.1:18790/webhook/wecom

# Via public domain (needs Nginx proxy)
curl -i https://your-domain.com/webhook/wecom
```

### Simulate WeCom URL Verification (GET + signature parameters)

WeCom sends a signed GET request when saving callback URL in backend. Can simulate with:

```bash
# When callbackToken not configured (signature verification skipped), directly returns echostr value
curl -i "http://127.0.0.1:18790/webhook/wecom?msg_signature=abc&timestamp=1234567890&nonce=test&echostr=hello123"
# Expected response: 200 OK, body = hello123
```

### Simulate WeCom Message Callback (POST)

```bash
curl -i -X POST http://127.0.0.1:18790/webhook/wecom \
  -H 'Content-Type: text/xml' \
  -d '<xml>
  <ToUserName><![CDATA[ww_enterprise_ID]]></ToUserName>
  <FromUserName><![CDATA[ZhangSan]]></FromUserName>
  <CreateTime>1409735669</CreateTime>
  <MsgType><![CDATA[text]]></MsgType>
  <Content><![CDATA[Hello]]></Content>
  <MsgId>1234567890</MsgId>
  <AgentID>1000001</AgentID>
</xml>'
# Expected response: 200 OK, body = success
```

> **Note**:
> - If `callbackToken` is empty, POST messages don't do signature verification (suitable for intranet testing).
> - If `callbackToken` is configured, WeCom's requests will carry `msg_signature` parameter, Blockcell will auto-verify signature (SHA1).
> - WeCom actually sends AES-256-CBC encrypted message bodies, Blockcell will use `encodingAesKey` to auto-decrypt (supports WeCom's PKCS7-32 padding).

---

## 10. Notes

- **Message Length Limit**: WeCom text message maximum length is 2048 characters. Blockcell will automatically split and send longer messages in multiple parts.
- **API Rate Limits**: WeCom's **API call rate limits are very strict** (e.g., send message interface is usually 10000 times/month, get access_token is 2000 times/month).
  - If using **polling mode**, ensure appropriate `pollIntervalSecs` is configured (recommended 10-30 seconds).
  - If using **callback mode**, receiving messages doesn't consume API call quota, only sending messages consumes quota.
  - Blockcell internally implements Token caching and reuse (refreshes 5 minutes early) to minimize API calls.
- **Message Security**: In callback mode, all messages from WeCom are encrypted by default. Blockcell will use your configured `encodingAesKey` to auto-decrypt and use `callbackToken` for security signature verification.

---

## 11. Common Error Troubleshooting

| Error | Cause | Solution |
|------|------|----------|
| `GET signature verification failed` | `callbackToken` configured incorrectly, or URL parameters double-decoded by middleware | Confirm `callbackToken` matches admin backend |
| `60020: not allow to access from your ip` | Server IP not added to Enterprise Trusted IP whitelist | Admin Backend → App → **Enterprise Trusted IP** → Add server IP |
| `81013: user & party & tag all invalid` | Reply target user ID incorrect (mistakenly used enterprise ID as recipient) | Fixed, Blockcell uses `FromUserName` as reply target |
| `40014: invalid access_token` | `corpSecret` configured incorrectly, or token expired | Confirm `corpSecret` is correct; Blockcell will auto-refresh token |
