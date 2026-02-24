# Lark (国际版) 机器人配置指南

Blockcell 通过 **HTTP Webhook（回调）** 方式接收 Lark 国际版消息。

> **重要说明**
>
> - **国际版 Lark** (`open.larksuite.com`) — 仅支持 HTTP Webhook，需要公网可访问的 URL
> - **飞书** (`open.feishu.cn`) — 支持 WebSocket 长连接，无需公网 IP，请参考 [04_feishu.md](./04_feishu.md)
>
> 如果你的应用创建在飞书平台，请使用 `feishu` 渠道而非 `lark` 渠道。

---

## 前提条件

- Blockcell gateway 部署在**公网可访问**的服务器上（或通过 ngrok/frp 等内网穿透工具暴露）
- Webhook URL 格式：`https://your-domain.com/webhook/lark`

---

## 配置步骤

### 1. 创建 Lark 应用

1. 访问 [Lark Developer Console](https://open.larksuite.com/app)
2. 点击 **Create App** → **Custom App**
3. 填写应用名称和描述，点击 **Create**
4. 记录 **App ID** 和 **App Secret**（在 Credentials & Basic Info 页面）

### 2. 开启机器人能力

1. 进入应用 → **Features** → **Bot**
2. 点击 **Enable Bot**

### 3. 申请权限

进入 **Permissions & Scopes**，搜索并添加以下权限：

| 权限 | 说明 |
|------|------|
| `im:message` | 接收消息 |
| `im:message:send_as_bot` | 发送消息 |
| `im:chat` | 访问群组信息 |

### 4. 配置事件订阅

1. 进入 **Event Subscriptions**
2. **Encrypt Key**（可选但推荐）：填写一个随机字符串，用于加密 webhook 内容
3. **Verification Token**：记录此值（用于验证请求来源）
4. **Request URL**：填写 `https://your-domain.com/webhook/lark`
5. 点击 **Verify** — Lark 会发送 challenge 请求，Blockcell 会自动响应
6. 添加事件：点击 **Add Event** → 搜索 `im.message.receive_v1` → 添加

### 5. 发布应用

1. 进入 **App Release** → **Version Management & Release**
2. 点击 **Create Version**，填写版本信息
3. 提交审核（自建应用通常立即生效）

### 6. 配置 Blockcell

编辑 `~/.blockcell/config.json`：

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

| 字段 | 说明 |
|------|------|
| `appId` | App ID（必填） |
| `appSecret` | App Secret（必填） |
| `encryptKey` | 加密密钥，与 Lark 后台 Encrypt Key 一致（推荐填写） |
| `verificationToken` | 验证 Token（当前版本用于记录，后续版本将用于请求验证） |
| `allowFrom` | 允许的用户 open_id 白名单，空数组表示允许所有人 |

### 7. 启动 Gateway

```bash
blockcell gateway
```

启动后会看到如下输出：

```
  Server
  HTTP/WS:  http://0.0.0.0:18790
  WebUI:   http://localhost:18791/
  API:     POST http://0.0.0.0:18790/v1/chat  |  GET /v1/health  |  GET /v1/ws

  ✓ Gateway ready. Press Ctrl+C to stop.
```

其中 `0.0.0.0:18790` 是 Blockcell 监听的内网地址，Lark webhook 路径为：

```
http://0.0.0.0:18790/webhook/lark   ← 内网地址（Lark 无法直接访问）
https://your-domain.com/webhook/lark ← 需要通过 Nginx 反向代理暴露到公网
```

---

## 网络配置：将 Webhook 暴露到公网

Lark 服务器需要能主动访问你的 `/webhook/lark` 端点，因此必须有公网可访问的 HTTPS URL。

### 方案一：Nginx 反向代理（推荐生产环境）

假设你的服务器公网域名为 `bot.example.com`，Blockcell gateway 运行在本机 `18790` 端口。

**Nginx 配置文件** `/etc/nginx/sites-available/blockcell`：

```nginx
server {
    listen 80;
    server_name bot.example.com;
    # 强制跳转 HTTPS（Lark 要求 HTTPS）
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl;
    server_name bot.example.com;

    # SSL 证书（推荐使用 Let's Encrypt）
    ssl_certificate     /etc/letsencrypt/live/bot.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/bot.example.com/privkey.pem;
    ssl_protocols       TLSv1.2 TLSv1.3;
    ssl_ciphers         HIGH:!aNULL:!MD5;

    # 只代理 Lark webhook 路径（其余路径不对外暴露）
    location /webhook/lark {
        proxy_pass         http://127.0.0.1:18790/webhook/lark;
        proxy_http_version 1.1;
        proxy_set_header   Host              $host;
        proxy_set_header   X-Real-IP         $remote_addr;
        proxy_set_header   X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto $scheme;
        proxy_read_timeout 30s;
    }

    # 如需同时暴露 API（可选，建议加 api_token 保护）
    # location /v1/ {
    #     proxy_pass http://127.0.0.1:18790/v1/;
    #     proxy_http_version 1.1;
    #     proxy_set_header Host $host;
    # }
}
```

启用配置并重载 Nginx：

```bash
# 创建软链接
sudo ln -s /etc/nginx/sites-available/blockcell /etc/nginx/sites-enabled/

# 申请 SSL 证书（首次）
sudo certbot --nginx -d bot.example.com

# 测试配置
sudo nginx -t

# 重载
sudo systemctl reload nginx
```

配置完成后，在 Lark 开发者后台填写：

```
https://bot.example.com/webhook/lark
```

**验证是否可访问：**

```bash
curl -X POST https://bot.example.com/webhook/lark \
  -H "Content-Type: application/json" \
  -d '{"type":"url_verification","challenge":"test123"}'
# 期望返回：{"challenge":"test123"}
```

---

### 方案二：ngrok（本地开发 / 临时测试）

无需服务器，适合本地调试。

```bash
# 安装 ngrok（https://ngrok.com）
brew install ngrok

# 启动隧道（指向 Blockcell gateway 端口）
ngrok http 18790
```

ngrok 会输出类似：

```
Forwarding  https://abc123.ngrok-free.app -> http://localhost:18790
```

在 Lark 开发者后台填写：

```
https://abc123.ngrok-free.app/webhook/lark
```

> ⚠️ **注意**：免费版 ngrok 每次重启地址会变化，需要重新在 Lark 后台更新 URL。生产环境请使用固定域名方案。

---

### 方案三：frp 内网穿透

适合没有公网 IP 但有一台公网服务器的场景。

**公网服务器** `frps.ini`：

```ini
[common]
bind_port = 7000
```

**本地机器** `frpc.ini`：

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

然后在公网服务器上用 Nginx 代理 frp 的 HTTP 端口，配置同方案一。

---

### 端口说明

Blockcell gateway 默认端口：

| 端口 | 用途 | 对外暴露 |
|------|------|----------|
| `18790` | API + Webhook（HTTP） | 通过 Nginx/ngrok 代理后暴露 `/webhook/lark` |
| `18791` | WebUI（仅本地访问） | 不建议对外暴露 |

修改端口可在 `~/.blockcell/config.json` 中配置：

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

> 将 `host` 改为 `127.0.0.1`（而非 `0.0.0.0`）可以让 gateway 只监听本地回环，更安全——Nginx 仍可通过 `proxy_pass http://127.0.0.1:18790` 访问。

---

## 获取用户 open_id（用于 allowFrom 白名单）

在 Lark 中给机器人发送一条消息，查看 Blockcell 日志：

```
INFO blockcell_channels::lark: Lark webhook: inbound message chat_id=oc_xxx open_id=ou_yyy len=5
```

将 `open_id`（`ou_` 开头）填入 `allowFrom` 列表。

---

## 与机器人交互

### 私聊
直接在 Lark 中搜索机器人名称，发起私聊即可。

### 群组
1. 将机器人添加到群组
2. **@机器人** 发送消息（Lark 会自动将 @ 消息路由到机器人）

---

## Webhook 加密说明

当配置了 `encryptKey` 时，Lark 会对 webhook body 进行加密：

- 加密算法：AES-256-CBC + PKCS7 padding
- 密钥：`SHA-256(encryptKey)` → 32 字节
- IV：base64 解码后的前 16 字节
- Blockcell 自动处理解密，无需手动操作

**强烈建议配置 `encryptKey`**，防止 webhook 内容被中间人截获。

---

## 常见问题

### Webhook URL 验证失败
- 确认 Blockcell gateway 已启动且公网可访问
- 确认 URL 格式正确：`https://your-domain.com/webhook/lark`（注意 HTTPS）
- 检查防火墙/反向代理是否放行了该路径

### 收不到消息
- 确认已添加 `im.message.receive_v1` 事件订阅
- 确认应用已发布
- 确认机器人已被添加到对话/群组

### 发送消息失败
- 检查 `appId` 和 `appSecret` 是否正确
- 确认应用有 `im:message:send_as_bot` 权限
- 查看 Blockcell 日志中的具体错误信息

### 加密解密失败
- 确认 `encryptKey` 与 Lark 后台填写的完全一致（区分大小写）
- 如果不需要加密，在 Lark 后台清空 Encrypt Key，同时将 `encryptKey` 设为空字符串
