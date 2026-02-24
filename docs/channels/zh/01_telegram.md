# Telegram 机器人配置指南

Blockcell 支持通过 Telegram 机器人与智能体进行交互。Telegram 渠道使用长轮询（Long Polling）模式接收消息，无需配置公网 Webhook 地址，非常适合本地开发和部署。

## 1. 申请 Telegram Bot

1. 在 Telegram 中搜索并添加官方机器人 [BotFather](https://t.me/BotFather)。
2. 发送 `/newbot` 命令创建新机器人。
3. 按照提示输入机器人的 **Name**（显示名称）和 **Username**（用户名，必须以 `bot` 结尾）。
4. 创建成功后，BotFather 会返回一段 **API Token**（如 `123456789:ABCdefGhIJKlmNoPQRsTUVwxyZ`）。

## 2. 获取用户 ID（用于白名单）

为了安全起见，建议配置白名单（`allowFrom`），只允许特定的用户与机器人交互。

1. 在 Telegram 中搜索并添加 [userinfo_bot](https://t.me/userinfo_bot) 或类似机器人。
2. 发送任意消息，它会返回你的 Telegram ID（一串纯数字，如 `12345678`）。

## 3. 配置 Blockcell

在 Blockcell 的配置文件（如 `~/.blockcell/config.json` 或项目目录下的 `config.json`）中，找到 `channels` 配置块，修改 `telegram` 部分：

```json
{
  "channels": {
    "telegram": {
      "enabled": true,
      "botToken": "你的_BOT_TOKEN",
      "allowFrom": ["你的_USER_ID", "其他_USER_ID"]
    }
  }
}
```

### 配置项说明

- `enabled`: 是否启用 Telegram 渠道（`true` 或 `false`）。
- `botToken`: 在 BotFather 处获取的 API Token。
- `allowFrom`: 允许访问的用户 ID 列表（字符串数组）。如果留空 `[]`，则允许任何人在群聊或私聊中调用机器人。建议在生产环境中配置特定的 ID。

## 4. 交互方式

- **私聊**：直接搜索你的机器人 Username，发送消息即可。
- **群聊**：
  1. 在 BotFather 中发送 `/setjoingroups` 允许机器人被拉入群组。
  2. 发送 `/setprivacy` 并设置为 `Disable`（如果希望机器人能读取群内所有消息），或者保持 `Enable`（仅响应 `@机器人` 的消息）。
  3. 将机器人拉入群组，通过 `@机器人` 的方式与其交互。

## 5. 注意事项

- Telegram 在中国大陆等地区可能需要配置系统代理（如 `HTTP_PROXY` / `HTTPS_PROXY`）才能正常连接其 API 服务器。
- 文本消息最大长度为 4096 字符，超长消息 Blockcell 会自动切片发送。
