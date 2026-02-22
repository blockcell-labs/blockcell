# 第06篇：多渠道接入 —— Telegram/Slack/Discord/飞书/钉钉/企业微信等都能用

> 系列文章：《blockcell 开源项目深度解析》第 6/16 篇

---

## 为什么需要多渠道

在命令行里和 AI 对话很方便，但有些场景下你希望：

- 在手机上通过 Telegram 发消息，AI 帮你查股价
- 在公司 Slack 频道里 @AI，让它帮团队回答问题
- 股价跌了，AI 主动推送消息给你，而不是你去问它

这就是 blockcell 多渠道系统要解决的问题：**让 AI 在你常用的通讯工具里工作。**

---

## 支持的渠道

blockcell 目前支持 8 个消息渠道：

| 渠道 | 协议 | 适用场景 |
|------|------|---------|
| Telegram | Bot API 轮询 | 个人使用，手机端 |
| WhatsApp | Bridge WebSocket | 个人/商业 |
| 飞书（Feishu） | 长连接 WebSocket | 企业内部 |
| Lark（国际版飞书） | Webhook 回调 | 海外团队 |
| Slack | Web API 轮询 | 团队协作 |
| Discord | Gateway WebSocket | 社区/开发者 |
| 钉钉（DingTalk） | Stream SDK（WebSocket） | 国内企业 |
| 企业微信（WeCom） | Webhook 回调 / 轮询 | 国内企业 |

---

## 架构设计

```
外部消息 → 渠道适配器 → InboundMessage → Agent Runtime → 处理
                                                          ↓
外部消息 ← 渠道管理器 ← OutboundMessage ← Agent Runtime ← 结果
```

每个渠道都实现了统一的接口：
- **接收**：把平台消息转换为 `InboundMessage`
- **发送**：把 `OutboundMessage` 转换为平台格式发出

这样 Agent Runtime 不需要关心具体是哪个渠道，处理逻辑完全统一。

---

## 配置 Telegram

### 第一步：创建 Bot

1. 在 Telegram 里搜索 `@BotFather`
2. 发送 `/newbot`，按提示创建 Bot
3. 获得 Bot Token（格式：`1234567890:ABCdefGHIjklMNOpqrsTUVwxyz`）

### 第二步：获取你的用户 ID

1. 搜索 `@userinfobot`，发送任意消息
2. 它会回复你的用户 ID（纯数字）

### 第三步：配置 blockcell

编辑 `~/.blockcell/config.json`：

```json
{
  "channels": {
    "telegram": {
      "botToken": "1234567890:ABCdefGHIjklMNOpqrsTUVwxyz",
      "allowFrom": ["你的用户ID"],
      "pollIntervalSecs": 2
    }
  }
}
```

`allowFrom` 是白名单，只有列表里的用户 ID 才能控制你的 AI。**一定要设置，否则任何人都能控制你的 AI。**

### 第四步：启动 Gateway

```bash
blockcell gateway
```

现在打开 Telegram，给你的 Bot 发消息，它会回复你！

---

## 配置 Slack

### 第一步：创建 Slack App

1. 访问 https://api.slack.com/apps
2. 点击 "Create New App" → "From scratch"
3. 填写 App 名称和工作区

### 第二步：配置权限

在 "OAuth & Permissions" 页面，添加以下 Bot Token Scopes：
- `channels:history`
- `chat:write`
- `users:read`

### 第三步：安装到工作区

点击 "Install to Workspace"，获得 Bot User OAuth Token（`xoxb-...`）。

### 第四步：配置 blockcell

```json
{
  "channels": {
    "slack": {
      "botToken": "xoxb-你的Bot-Token",
      "channels": ["C0123456789"],
      "allowFrom": ["U0123456789"],
      "pollIntervalSecs": 5
    }
  }
}
```

`channels` 是要监听的频道 ID（在频道详情里可以看到）。

---

## 配置 Discord

### 第一步：创建 Discord Bot

1. 访问 https://discord.com/developers/applications
2. 点击 "New Application"
3. 进入 "Bot" 页面，点击 "Add Bot"
4. 复制 Bot Token

### 第二步：配置权限

在 "Bot" 页面，开启以下 Privileged Gateway Intents：
- `MESSAGE CONTENT INTENT`

### 第三步：邀请 Bot 到服务器

在 "OAuth2 → URL Generator" 页面：
- Scopes: `bot`
- Bot Permissions: `Send Messages`, `Read Message History`

复制生成的 URL，在浏览器打开，选择你的服务器。

### 第四步：配置 blockcell

```json
{
  "channels": {
    "discord": {
      "botToken": "你的Bot-Token",
      "channels": ["1234567890123456789"],
      "allowFrom": ["你的Discord用户ID"]
    }
  }
}
```

Discord 使用 **WebSocket Gateway** 连接，实时性更好（不需要轮询）。

---

## 配置飞书

飞书的配置稍微复杂一些，需要企业管理员权限：

```json
{
  "channels": {
    "feishu": {
      "appId": "cli_xxx",
      "appSecret": "你的AppSecret",
      "verificationToken": "验证Token",
      "encryptKey": "加密Key（可选）",
      "allowFrom": ["user_open_id_1"]
    }
  }
}
```

飞书这里使用的是 **开放平台长连接（WebSocket）** 来接收消息（不需要你暴露公网回调地址）。

`allowFrom` 建议务必配置为白名单（飞书的用户 OpenID）。

---

## 配置 Lark（国际版飞书）

Lark（海外版）这里使用 **Webhook 回调** 的方式接收消息，通常需要：

- 你有一个可公网访问的地址（或内网穿透）
- 在 Lark 开放平台里配置回调 URL

配置示例：

```json
{
  "channels": {
    "lark": {
      "appId": "cli_xxx",
      "appSecret": "YOUR_APP_SECRET",
      "verificationToken": "验证Token",
      "encryptKey": "加密Key（可选）",
      "allowFrom": ["user_open_id_1"]
    }
  }
}
```

如果开启了 `encryptKey`，Lark 会把回调 body 加密后再发送，blockcell 会自动解密。

---

## 配置钉钉（DingTalk）

钉钉这里使用 **Stream SDK（WebSocket 推送）** 来收消息，实时性好。

配置示例：

```json
{
  "channels": {
    "dingtalk": {
      "appKey": "你的AppKey",
      "appSecret": "你的AppSecret",
      "robotCode": "可选：用于给用户发消息",
      "allowFrom": ["用户ID"]
    }
  }
}
```

---

## 配置企业微信（WeCom）

企业微信支持两种方式：

- **Webhook 回调（推荐）**：企业微信把消息推到你的回调 URL（需要公网地址/内网穿透）
- **轮询（降级）**：没有配置回调时，只做 token 心跳检查，基本拿不到真正的收消息能力

配置示例：

```json
{
  "channels": {
    "wecom": {
      "corpId": "你的CorpId",
      "corpSecret": "你的Secret",
      "agentId": 1000002,
      "callbackToken": "回调Token（用于签名校验）",
      "encodingAesKey": "EncodingAESKey（用于消息解密）",
      "allowFrom": ["成员UserId"],
      "pollIntervalSecs": 10
    }
  }
}
```

---

## 多渠道同时运行

blockcell 支持同时运行多个渠道：

```json
{
  "channels": {
    "telegram": { "botToken": "..." },
    "slack": { "botToken": "..." },
    "discord": { "botToken": "..." }
  }
}
```

启动 `blockcell gateway` 后，所有配置的渠道会同时运行。

---

## 主动推送消息

这是多渠道系统最强大的功能之一：**AI 可以主动给你发消息。**

结合告警系统，你可以设置：

```
你: 帮我设置一个告警，茅台股价跌破 1500 时，
    通过 Telegram 发消息给我
```

AI 会创建一个告警规则：

```json
{
  "name": "茅台跌破1500",
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
        "message": "⚠️ 茅台跌破1500！当前价格：{value}"
      }
    }
  ]
}
```

当条件触发时，blockcell 自动通过 Telegram 发消息给你。

---

## 渠道状态检查

```bash
blockcell channels status
```

输出：
```
Channels status:
  telegram  ✓ running (polled 1234 times, last: 2s ago)
  slack     ✓ running (polled 567 times, last: 5s ago)
  discord   ✓ connected (WebSocket, latency: 45ms)
  whatsapp  ✗ not configured
  feishu    ✗ not configured
```

---

## 安全注意事项

### 1. 一定要设置白名单

```json
"allowFrom": ["你的用户ID"]
```

不设置白名单意味着任何人都能控制你的 AI，这是非常危险的。

### 2. Gateway 模式的路径限制

在 Gateway 模式下（`blockcell gateway`），AI 无法访问工作目录外的文件。这是设计上的安全限制，防止通过消息渠道访问你的私人文件。

### 3. API Token 保护

如果你的 Gateway 有公网地址，一定要设置 API Token：

```json
{
  "gateway": {
    "apiToken": "一个复杂的随机字符串"
  }
}
```

---

## 实际使用场景

### 场景一：手机远程控制

出门在外，通过 Telegram 发消息：
```
你: 帮我看看服务器的 CPU 使用率
AI: [执行 exec "top -bn1 | head -20"]
    当前 CPU 使用率：23%，内存：4.2GB/16GB
```

### 场景二：团队 AI 助手

在 Slack 频道里：
```
@blockcell 帮我总结一下今天的 GitHub PR 列表
AI: [调用 git_api list_prs]
    今天有 5 个 PR...
```

### 场景三：自动化报告

每天早上 8 点，blockcell 自动生成金融日报，通过 Telegram 发给你：
```
[定时任务触发]
AI: 生成今日金融日报...
    [发送到 Telegram]
📊 今日金融日报 2025-02-18
大盘：沪指 +0.5%，深指 +0.8%
...
```

---

## 小结

blockcell 的多渠道系统让 AI 真正融入你的日常工作流：

- **被动响应**：在 Telegram/Slack/Discord 里发消息，AI 立即回复
- **主动推送**：结合告警系统，AI 主动通知你重要事件
- **统一处理**：所有渠道共享同一个 Agent Runtime，行为一致
- **安全隔离**：白名单机制 + Gateway 路径限制

下一篇，我们来看 blockcell 最酷的功能之一：浏览器自动化——让 AI 帮你操控网页。
---

*上一篇：[记忆系统 —— 让 AI 记住你说过的话](./05_memory_system.md)*
*下一篇：[浏览器自动化 —— 让 AI 帮你操控网页](./07_browser_automation.md)*

*项目地址：https://github.com/blockcell-labs/blockcell*
*官网：https://blockcell.dev*
