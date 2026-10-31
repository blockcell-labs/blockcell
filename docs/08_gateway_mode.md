# 第08篇：Gateway 模式 —— 把 AI 变成一个服务

> 系列文章：《blockcell 开源项目深度解析》第 8 篇
---

## 两种运行模式

blockcell 有两种运行模式：

**`blockcell agent`** — 交互模式
- 在终端里和 AI 对话
- 适合个人使用、开发调试
- 你在，AI 才工作

**`blockcell gateway`** — 守护进程模式
- 在后台持续运行
- 提供 HTTP API、WebSocket 和 WebUI
- 维护一个按 agent 划分的运行时池
- 监听消息渠道（Telegram/Slack/Discord 等）
- 执行定时任务（Cron）
- 你不在，AI 也在工作

本篇介绍 Gateway 模式。

---

## Gateway 启动

```bash
blockcell gateway
```

启动后，你会看到：

```
[2025-02-18 08:00:00] Gateway starting...
[2025-02-18 08:00:00] API server: http://localhost:18790
[2025-02-18 08:00:00] WebUI: http://localhost:18791
[2025-02-18 08:00:00] Telegram: connected (polling)
[2025-02-18 08:00:00] Discord: connected (WebSocket)
[2025-02-18 08:00:00] Cron: 3 jobs scheduled
[2025-02-18 08:00:00] Gateway ready.
```

默认端口：
- **18790**：API 服务器（HTTP）
- **18791**：WebUI（浏览器界面）

默认路由规则：
- CLI / WebSocket / WebUI 内部请求默认进入 `default` agent
- 外部渠道消息优先按 `channelAccountOwners.<channel>.<accountId>` 路由到目标 agent，未命中时回退到 `channelOwners.<channel>`
- 已启用的外部渠道如果没有 owner，Gateway 会在启动时直接报错

例如，**2 个 Telegram bot / 2 个 agent** 可以这样路由：

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

这样同一个 `telegram` 渠道下，Gateway 会把 `bot1` 的消息分发到 `default` runtime，把 `bot2` 的消息分发到 `ops` runtime。

---

## Gateway 与外部渠道里的斜杠命令

从 v0.1.6 开始，Gateway/WebSocket 和外部 Channel 与 CLI 共用统一斜杠命令处理器。用户在 Telegram、Slack、Discord、飞书、钉钉等渠道里发送 `/help`、`/tasks`、`/skills`、`/tools`、`/clear` 等命令时，Gateway 会优先本地处理，再把结果回发到原渠道。

这带来两个效果：

- 常见查询命令不需要进入 LLM，因此响应更快，也不会消耗 token。
- CLI、WebUI/WebSocket 和外部渠道看到的命令行为保持一致。

当前内置命令包括：

| 命令 | 说明 |
|------|------|
| `/help` | 显示命令列表 |
| `/tasks [status]` | 查看后台任务 |
| `/skills` | 查看已加载技能 |
| `/tools` | 查看已加载工具 |
| `/learn <描述>` | 让 Agent 学习新技能，会调用 LLM |
| `/clear` | 清空当前会话历史 |
| `/compact` | 手动触发会话历史压缩 |
| `/session-metrics` | 查看 7 层记忆系统指标 |
| `/log ...` | 动态控制日志输出 |

`/quit` 和 `/exit` 仅在 CLI 模式可用。命令名必须完整匹配，带多余参数的无参命令会被当作普通消息处理。

---

## HTTP API

Gateway 提供了一个简洁的 REST API：

### `POST /v1/chat` — 发送消息

```bash
curl -X POST http://localhost:18790/v1/chat \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer 你的token" \
  -d '{
    "content": "帮我查一下茅台今天的股价",
    "chat_id": "finance-demo"
  }'
```

Gateway 会异步排队处理消息，因此成功响应状态码是 `202 Accepted`：
```json
{
  "status": "accepted",
  "message": "Message queued for processing",
  "session_id": "finance-demo"
}
```

最终回复通过 WebSocket 或原始消息渠道异步返回，不会直接出现在这个 HTTP 响应中。

### `GET /v1/health` — 健康检查

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

这个接口不需要认证，专门给 Kubernetes/负载均衡器的健康探针用。

### `GET /v1/tasks` — 查看任务列表

```bash
curl http://localhost:18790/v1/tasks \
  -H "Authorization: Bearer 你的token"
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

### `GET /v1/ws` — WebSocket 连接

WebSocket 接口支持实时双向通信：

```javascript
const token = '你的token';
const tokenHex = Array.from(new TextEncoder().encode(token), byte =>
  byte.toString(16).padStart(2, '0')
).join('');
const ws = new WebSocket(
  'ws://localhost:18790/v1/ws',
  [`blockcell-auth.${tokenHex}`],
);

// 发送消息
ws.send(JSON.stringify({
  type: 'chat',
  content: '帮我查一下比特币价格',
  chat_id: 'finance-demo',
}));

// 接收回复（流式）
ws.onmessage = (event) => {
  const data = JSON.parse(event.data);
  if (data.type === 'token') {
    process.stdout.write(data.delta);
  } else if (data.type === 'message_done') {
    console.log('\n完成');
  } else if (data.type === 'skills_updated') {
    console.log('技能已更新:', data.new_skills);
  }
};
```

WebSocket 支持**流式输出**，AI 的回复会一个字一个字地推送过来，体验更流畅。

另外，Gateway 还提供：

- `GET /v1/channels/status`：返回当前各渠道连接状态
- `GET /v1/channel-owners`：查看渠道级与账号级 owner 绑定
- `PUT /v1/channel-owners/:channel`：修改渠道 fallback owner
- `DELETE /v1/channel-owners/:channel`：删除渠道 fallback owner
- `PUT /v1/channel-owners/:channel/accounts/:account_id`：设置账号级 owner
- `DELETE /v1/channel-owners/:channel/accounts/:account_id`：删除账号级 owner

---

## WebUI 界面

访问 `http://localhost:18791`，你会看到一个 Web 界面：

```
┌─────────────────────────────────────────────────────┐
│  blockcell Dashboard                          [登出] │
├──────────┬──────────────────────────────────────────┤
│          │                                          │
│ 导航栏   │  主内容区                                │
│          │                                          │
│ 💬 对话  │  [对话界面 / 任务列表 / 技能管理 / ...]  │
│ 📋 任务  │                                          │
│ 🔧 工具  │                                          │
│ 🧠 技能  │                                          │
│ 📊 进化  │                                          │
│ ⚙️ 设置  │                                          │
└──────────┴──────────────────────────────────────────┘
```

WebUI 的主要功能：
- **对话界面**：在浏览器里和 AI 对话
- **任务监控**：查看后台任务的执行状态
- **技能管理**：查看、启用/禁用技能
- **进化记录**：查看 AI 的自我进化历史
- **实时推送**：通过 WebSocket 接收技能更新等事件

---

## API 认证

当前版本里，`gateway.apiToken` 如果为空，Gateway 会在首次启动时**自动生成并写回 `config.json5`**。这意味着 API 默认不会以“完全无密码”的状态运行，但在公网场景下你仍然应该手动设置一个长期稳定的 token。

```json
{
  "gateway": {
    "apiToken": "一个复杂的随机字符串，至少32位",
    "webuiPass": "单独的 WebUI 登录密码（可选）"
  }
}
```

调用 API 时，在 Header 里带上 token：

```bash
curl -H "Authorization: Bearer 你的token" http://你的服务器:18790/v1/tasks
```

浏览器 WebSocket 不能设置任意 Authorization Header。请把 token 编码成 UTF-8 十六进制，并通过 WebSocket 子协议传递：

```
blockcell-auth.<token 的 UTF-8 十六进制>
```

完整 JavaScript 连接示例见上文“WebSocket 连接”。非浏览器客户端也可以在升级请求中使用标准 `Authorization: Bearer ...` Header。不要把 token 放在 URL Query 中，当前 Gateway 不读取该参数，而且 URL 容易进入访问日志。

WebUI 登录密码与 API token 现在**分离**：

- 如果设置了 `gateway.webuiPass`，WebUI 使用这个固定密码
- 如果没设置，Gateway 会在启动时打印一个临时密码
- `apiToken` 继续只负责 API / WebSocket 鉴权

---

## 定时任务（Cron）

Gateway 模式下，定时任务会自动运行。

### 创建定时任务

```
你: 帮我创建一个定时任务，每天早上 8 点生成金融日报，
    通过 Telegram 发给我
```

AI 会创建一个 Cron 任务：

```json
{
  "schedule": "0 8 * * *",
  "task": "生成今日金融日报，包含大盘走势、热点板块、自选股情况，通过 Telegram 发送",
  "enabled": true
}
```

### 管理定时任务

```bash
# 列出所有定时任务
blockcell cron list

# 输出：
# ID          SCHEDULE    LAST_RUN              STATUS
# daily_report 0 8 * * *  2025-02-18 08:00:00   ✓ success
# price_check  */10 * * * * 2025-02-18 08:50:00  ✓ success
```

---

## 部署到服务器

### 使用 systemd（Linux）

创建服务文件 `/etc/systemd/system/blockcell.service`：

```ini
[Unit]
Description=blockcell AI Gateway
After=network.target

[Service]
Type=simple
User=你的用户名
ExecStart=/home/你的用户名/.local/bin/blockcell gateway
Restart=always
RestartSec=10
Environment=HOME=/home/你的用户名

[Install]
WantedBy=multi-user.target
```

启动服务：

```bash
sudo systemctl enable blockcell
sudo systemctl start blockcell
sudo systemctl status blockcell
```

### 使用 Docker

```dockerfile
FROM --platform=linux/amd64 ubuntu:22.04
RUN apt-get update && apt-get install -y curl ca-certificates tar \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/refs/heads/main/install.sh \
    | BLOCKCELL_INSTALL_METHOD=release BLOCKCELL_INSTALL_DIR=/usr/local/bin sh
EXPOSE 18790 18791
CMD ["blockcell", "gateway"]
```

容器使用的 `~/.blockcell/config.json5` 至少需要让两个服务监听所有容器网卡，否则 Docker 端口映射无法访问：

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
# 宿主机 ~/.blockcell/config.json5 需要包含上面的容器监听配置
docker run -d \
  -p 18790:18790 \
  -p 18791:18791 \
  -v ~/.blockcell:/root/.blockcell \
  blockcell
```

### 使用 Nginx 反向代理

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

## 与其他应用集成

Gateway 模式让 blockcell 成为一个标准的 HTTP 服务，可以很方便地与其他应用集成：

### 在 Python 中调用

```python
import requests

def enqueue_ai(question: str, session_id: str) -> dict:
    response = requests.post(
        "http://localhost:18790/v1/chat",
        headers={"Authorization": "Bearer 你的token"},
        json={"content": question, "chat_id": session_id}
    )
    response.raise_for_status()
    return response.json()

# 返回排队状态；最终回复通过 WebSocket 或消息渠道接收
accepted = enqueue_ai("帮我查一下茅台今天的股价", "finance-demo")
print(accepted["session_id"])
```

### 在 Node.js 中调用

```javascript
const fetch = require('node-fetch');

async function enqueueAI(question, sessionId) {
  const response = await fetch('http://localhost:18790/v1/chat', {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'Authorization': 'Bearer 你的token'
    },
    body: JSON.stringify({ content: question, chat_id: sessionId })
  });
  if (!response.ok) throw new Error(`HTTP ${response.status}`);
  return response.json();
}
```

HTTP 接口只确认消息已进入队列；需要结合上文的 WebSocket 协议接收最终回复。

---

## Gateway vs Agent 模式对比

| 特性 | Agent 模式 | Gateway 模式 |
|------|-----------|-------------|
| 启动方式 | `blockcell agent` | `blockcell gateway` |
| 交互方式 | 命令行 | HTTP API / WebSocket / 消息渠道 |
| 定时任务 | ❌ | ✅ |
| 消息渠道 | ❌ | ✅ |
| 路径安全 | 需要确认 | 工作目录外直接拒绝 |
| 适合场景 | 开发调试 | 生产部署 |
| WebUI | ❌ | ✅ |

---

## 小结

Gateway 模式让 blockcell 从一个命令行工具变成了一个完整的 AI 服务：

- **HTTP API**：标准 REST 接口，任何语言都能调用
- **WebSocket**：实时流式输出
- **WebUI**：浏览器管理界面
- **定时任务**：Cron 调度，AI 自动工作
- **消息渠道**：Telegram/Slack/Discord 全部激活
- **安全**：Token 认证 + 路径隔离

下一篇，我们来看 blockcell 最独特的特性：自我进化系统——AI 如何自动写代码升级自己。
---

*上一篇：[浏览器自动化 —— 让 AI 帮你操控网页](./07_browser_automation.md)*
*下一篇：[自我进化 —— AI 如何自动写代码升级自己](./09_self_evolution.md)*

*项目地址：https://github.com/blockcell-labs/blockcell*
*官网：https://blockcell.dev*

## 长回答的上下文压缩不改变会话记录

模型答出长列表（≥5 项且超过阈值）时，响应缓存会把**发给模型的那份历史**压缩成
`[已缓存N条结果，ID: ref:…]` 引用，供后续轮次用 `session_recall` 取回原文。

压缩只作用于"构造模型请求"这一步：**持久化的会话记录保留完整答复**，
`GET /v1/sessions/:id` 和 WebUI 重新打开对话看到的仍是模型实际输出的内容。

