# Discord 机器人配置指南

Blockcell 支持通过 Discord 机器人与智能体进行交互。Discord 渠道使用 WebSocket 模式接收消息，无需配置公网 Webhook 地址，非常适合本地开发和部署。

## 1. 申请 Discord Bot

1. 登录并进入 [Discord Developer Portal](https://discord.com/developers/applications)。
2. 点击右上角的 **New Application** 创建一个新应用。
3. 输入应用的名称（Name）并确认。
4. 在左侧菜单中选择 **Bot**，然后点击 **Add Bot** 或确认重置 Token（Reset Token）。
5. 复制并保存你的 **Token**（如 `MTEy...`）。这个 Token 只能查看一次，请妥善保管。

## 2. 开启机器人权限 (Privileged Gateway Intents)

在同一个 **Bot** 页面，向下滚动找到 **Privileged Gateway Intents**。
必须开启以下选项，机器人才能接收到消息：
- **MESSAGE CONTENT INTENT** (允许机器人读取消息内容)

*(如果还需要监听状态或成员变动，可以根据需要开启其他 Intent，但读取消息必须开启 MESSAGE CONTENT INTENT)*。

## 3. 将机器人邀请到你的服务器

1. 在左侧菜单中选择 **OAuth2** -> **URL Generator**。
2. 在 **Scopes** 中勾选 `bot`。
3. 在下方出现的 **Bot Permissions** 中，勾选以下权限：
   - Read Messages/View Channels
   - Send Messages
   - Send Messages in Threads (可选，如果你使用线程)
4. 复制页面最下方的 **Generated URL**。

```
// 将下方的 YOUR_CLIENT_ID 替换成你的
https://discord.com/api/oauth2/authorize?client_id=YOUR_CLIENT_ID&permissions=68608&scope=bot
```

5. 在浏览器中打开这个 URL，选择你要将机器人邀请到的服务器，并点击授权。

## 4. 获取频道或用户 ID（用于白名单和频道限制）

你需要获取目标频道（Channel）或用户（User）的 ID。

1. 在 Discord 客户端中，进入 **User Settings** (用户设置) -> **Advanced** (高级)。
2. 开启 **Developer Mode** (开发者模式)。
3. **获取频道 ID**：右键点击你希望机器人响应的频道，选择 **Copy Channel ID** (复制频道 ID)。
4. **获取用户 ID**：右键点击你的头像或用户名，选择 **Copy User ID** (复制用户 ID)。

## 5. 配置 Blockcell

在 Blockcell 的配置文件中，修改 `discord` 部分：

```json
{
  "channels": {
    "discord": {
      "enabled": true,
      "botToken": "你的_BOT_TOKEN",
      "channels": ["允许的_频道_ID_1", "允许的_频道_ID_2"],
      "allowFrom": ["你的_用户_ID"]
    }
  }
}
```

### 配置项说明

- `enabled`: 是否启用 Discord 渠道（`true` 或 `false`）。
- `botToken`: 机器人的 Token。
- `channels`: 允许机器人响应的频道 ID 列表（字符串数组）。如果留空 `[]`，则机器人在所有有权限的频道中都会响应。
- `allowFrom`: 允许访问的用户 ID 列表（字符串数组）。如果留空 `[]`，则允许任何人在群聊或私聊中调用机器人。

## 6. 交互方式

- **私聊 (DM)**：右键机器人的头像，选择发送消息。
- **频道群聊**：在配置允许的频道内，发送消息或 `@机器人`。

## 7. 注意事项

- Discord API 可能会在部分地区受限，如有网络问题请配置全局代理。
- 文本消息最大长度为 2000 字符，超长消息 Blockcell 会自动切片发送。
- 目前的实现基于 `tokio-tungstenite` 直连 Discord Gateway v10，并处理了心跳保活。
