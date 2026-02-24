# Slack 机器人配置指南

Blockcell 支持通过 Slack 机器人与智能体进行交互。默认情况下，Slack 渠道使用 **Socket Mode** 接收消息，无需配置公网 Webhook 地址，非常适合本地开发和部署。如果启用了 Event Subscriptions 且未开启 Socket Mode，则需要公网地址。

## 1. 申请 Slack App

1. 登录并访问 [Slack API Applications](https://api.slack.com/apps)。
2. 点击 **Create New App** -> **From scratch**。
3. 输入 **App Name** 并选择要安装到的工作区 (Workspace)，点击 **Create App**。

## 2. 配置权限 (Scopes)

1. 在左侧菜单中选择 **OAuth & Permissions**。
2. 向下滚动到 **Scopes** -> **Bot Token Scopes**。
3. 必须添加以下权限：
   - `chat:write` (发送消息)
   - `channels:history` (读取公开频道消息)
   - `groups:history` (读取私有频道消息)
   - `im:history` (读取私聊消息)
   - `mpim:history` (读取多人私聊消息)
   - `app_mentions:read` (读取提及机器人的消息)
4. 向上滚动到 **OAuth Tokens for Your Workspace**，点击 **Install to Workspace** 并授权。
5. 复制并保存 **Bot User OAuth Token**（以 `xoxb-` 开头）。

## 3. 开启 Socket Mode

1. 在左侧菜单中选择 **Socket Mode**。
2. 开启 **Enable Socket Mode** 开关。
3. 系统会提示生成 App-Level Token，输入一个名称（如 `Socket Token`），并点击 **Generate**。
4. 复制并保存生成的 **App-Level Token**（以 `xapp-` 开头）。

## 4. 订阅事件 (Event Subscriptions)

开启 Socket Mode 后，需要告诉 Slack 哪些事件应该推送到 Socket 连接：

1. 在左侧菜单中选择 **Event Subscriptions**。
2. 开启 **Enable Events**。
3. 在 **Subscribe to bot events** 中，添加以下事件：
   - `message.channels`
   - `message.groups`
   - `message.im`
   - `message.mpim`
   - `app_mention`
4. 确保在页面底部点击 **Save Changes**，并在页面顶部（如果提示）重新安装应用。

## 5. 获取用户 ID（用于白名单和频道限制）

你需要获取目标频道（Channel）或用户（User）的 ID。

1. **获取用户 ID**：在 Slack 客户端中，点击用户的头像，选择 **View profile**，然后点击 **More**（三个点图标），选择 **Copy member ID**（如 `U12345678`）。
2. **获取频道 ID**：右键点击频道名称，选择 **Copy channel ID**（如 `C12345678`）。

## 6. 配置 Blockcell

在 Blockcell 的配置文件中，修改 `slack` 部分：

```json
{
  "channels": {
    "slack": {
      "enabled": true,
      "botToken": "你的_BOT_TOKEN_xoxb",
      "appToken": "你的_APP_TOKEN_xapp",
      "channels": ["你的_频道_ID_1"],
      "allowFrom": ["你的_用户_ID_1"]
    }
  }
}
```

### 配置项说明

- `enabled`: 是否启用 Slack 渠道（`true` 或 `false`）。
- `botToken`: 在 OAuth & Permissions 中获取的 `xoxb-` Token。
- `appToken`: 在 Socket Mode 中获取的 `xapp-` Token。
- `channels`: 允许机器人响应的频道 ID 列表（字符串数组）。如果留空 `[]`，则机器人在所有已加入的频道中都会响应。
- `allowFrom`: 允许访问的用户 ID 列表（字符串数组）。如果留空 `[]`，则允许任何人在群聊或私聊中调用机器人。

## 7. 交互方式

- **私聊 (DM)**：在 Apps 栏中找到你的机器人，直接发送消息。
- **频道群聊**：在允许的频道中发送消息，并 `@机器人`（注意：Slack 机制下，如果订阅了 `message.channels`，即使不 `@` 机器人，机器人也能收到所有消息。Blockcell 默认会响应自己被提到的消息以及在允许频道内的私信）。

## 8. 注意事项

- 文本消息最大长度为 4000 字符，超长消息 Blockcell 会自动切片发送为线程回复（Threaded Replies）。
- Blockcell 会自动解析 Slack 消息体中的子快（blocks、elements），合并文本并忽略附件（目前未实现完整的多模态支持）。
