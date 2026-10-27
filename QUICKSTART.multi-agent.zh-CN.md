# 多 Agent 快速开始

本指南展示一个 **多 agent** 的 BlockCell 部署示例，使用：

- 2 个 agent：`default` 和 `ops`
- 2 个 Telegram bot 账号：`main_bot` 和 `ops_bot`
- 渠道级回退路由 + 账号级精确路由

如果你只想先跑通最简单的方案，请优先阅读 `QUICKSTART.zh-CN.md`。那份文档是推荐的 **单 agent 最佳实践**。

## 1）安装

### 方式 A：安装脚本（推荐）

```bash
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/refs/heads/main/install.sh | sh
```

默认安装到 `~/.local/bin`。

### 方式 B：源码编译

必需：Rust 1.96+

```bash
cargo build -p blockcell --release
```

二进制在 `target/release/blockcell`。

## 2）生成配置

首次使用时，你仍然可以先运行：

```bash
blockcell setup
```

然后把 `~/.blockcell/config.json5` 调整成类似下面的多 agent 结构：

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
      "model": "deepseek-flash"
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

## 3）这套路由如何工作

- `default` 是通用 agent。
- `ops` 是偏运维场景的 agent。
- `channels.telegram.accounts` 定义了实际接入的 2 个 Telegram bot 账号。
- `channelOwners.telegram = default` 表示 Telegram 在没有账号级覆盖时默认进入 `default`。
- `channelAccountOwners.telegram.ops_bot = ops` 表示 `ops_bot` 收到的消息会进入 `ops` agent。

路由顺序：

- CLI / WebUI / WebSocket 请求默认进入 `default`，除非你显式指定别的 agent。
- 外部渠道先检查 `channelAccountOwners.<channel>.<accountId>`。
- 若没有账号级命中，则回退到 `channelOwners.<channel>`。
- 每个已启用的外部渠道必须配置渠道级 owner，或为所有已启用账号配置完整的账号级 owner；否则 Gateway 会在启动时拒绝该配置。

## 4）关于 tools 隔离

当前 agent 配置并没有直接暴露可用的每-agent `tools` 字段。

实际项目里，建议通过以下方式区分 agent：

- `intentProfile`
- 模型 / Provider 覆盖
- MCP 白名单（`allowedMcpServers`、`allowedMcpTools`）
- 账号和渠道路由

如果你需要更严格的工具边界，建议通过意图配置和运行时策略实现，而不是增加代码当前并不支持的配置字段。

## 5）以指定 agent 进入交互模式

```bash
blockcell status
blockcell agent -a default
blockcell agent -a ops
```

说明：

- 使用 `-a <agent_id>` 进入指定 agent 的交互模式。
- CLI 默认 session id 形如 `cli:<agent>`。

## 6）启动守护进程 + WebUI

```bash
blockcell gateway
```

默认端口：

- API 服务：`http://localhost:18790`
- WebUI：`http://localhost:18791`

如果配置了 `gateway.apiToken`，调用方式是：

- HTTP：`Authorization: Bearer <token>`
- WebSocket：也支持 `?token=<token>`

## 推荐上线顺序

- 先只启用 `main_bot`，并把它路由到 `default`。
- 确认 gateway 正常工作后，再启用 `ops_bot`。
- 让 `ops` 的意图范围比 `default` 更收敛。
