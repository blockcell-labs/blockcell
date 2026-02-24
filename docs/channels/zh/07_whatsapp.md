# WhatsApp 机器人配置指南

Blockcell 支持通过 WhatsApp Cloud API 与智能体进行交互。WhatsApp 渠道支持 **Webhook 回调模式** 接收消息。

> **注意**：由于 WhatsApp API 必须通过公网 HTTPS URL 进行 Webhook 验证和消息推送，你必须拥有一个具备有效 SSL 证书的公网域名，或者使用 `ngrok`/`localtunnel` 等内网穿透工具。

## 1. 申请 Meta (Facebook) 开发者账号和应用

1. 登录并访问 [Meta for Developers](https://developers.facebook.com/)。
2. 点击右上角的 **我的应用** (My Apps)。
3. 点击 **创建应用** (Create App)。
4. 选择 **其它** (Other) -> **业务** (Business)。
5. 填写应用显示名称、联系邮箱，并选择关联的业务管理后台帐户 (Business Manager Account)。如果没有，可以选择不关联。
6. 点击 **创建应用**。

## 2. 添加 WhatsApp 产品

1. 在应用控制面板中，向下滚动找到 **WhatsApp** 产品，点击 **设置** (Set up)。
2. 在左侧菜单中选择 **WhatsApp** -> **API 设置** (API Setup)。
3. 系统会为你分配一个 **测试手机号** (Test Phone Number) 和一个对应的 **测试手机号 ID** (Phone Number ID)。
4. 复制并保存你的 **临时访问口令** (Temporary Access Token，有效期 24 小时) 或生成一个 **永久访问口令**。
   - *(生成永久访问口令的方法请参考 Meta 官方文档，通常需要进入业务设置 -> 系统用户 -> 生成新口令)。*
5. 在 **发送和接收消息** 部分，添加你想用来测试接收消息的**真实手机号码**，并完成短信验证。只有在这个列表中的号码才能收到测试账号发出的消息。

## 3. 配置 Webhook

WhatsApp 使用 Webhook 推送新消息。

1. 在左侧菜单中选择 **WhatsApp** -> **配置** (Configuration)。
2. 点击 **配置 Webhook** (Configure Webhook) 或 **编辑** (Edit)。
3. 填写 **回调 URL** (Callback URL)：如 `https://your-domain.com/v1/whatsapp/webhook`
4. 填写 **验证口令** (Verify Token)：这是一个你自定义的字符串（如 `my_secret_verify_token_123`），用于验证请求是否来自 Meta。
5. 点击 **验证并保存**。此时，你的服务器必须处于运行状态且能正确响应 `hub.challenge` 校验请求。
6. 成功后，在 Webhook 字段（Webhooks fields）下的 **消息** (messages) 行，点击 **订阅** (Subscribe)。

## 4. 获取用户 ID（用于白名单）

WhatsApp 的 `sender_id` 通常是国际格式的**手机号码**（不包含 `+` 号），例如 `8613800138000` 或 `14155552671`。

## 5. 配置 Blockcell

在 Blockcell 的配置文件中，修改 `whatsapp` 部分：

```json
{
  "channels": {
    "whatsapp": {
      "enabled": true,
      "phoneNumberId": "123456789012345",
      "accessToken": "EAAxxx...（你的访问口令）",
      "verifyToken": "my_secret_verify_token_123",
      "allowFrom": ["8613800138000"]
    }
  }
}
```

### 配置项说明

- `enabled`: 是否启用 WhatsApp 渠道（`true` 或 `false`）。
- `phoneNumberId`: 在 API 设置中获取的发送方测试手机号 ID（注意不是你的真实号码，是那一长串数字 ID）。
- `accessToken`: 临时或永久访问口令。
- `verifyToken`: 你在配置 Webhook 时自定义的字符串。
- `allowFrom`: 允许访问的用户手机号码列表（字符串数组）。如果留空 `[]`，则允许任何能发消息给你的人与机器人交互。

## 6. 交互方式

- **单聊**：使用你在 Meta 后台验证过的真实手机号，向 Meta 分配的测试手机号发送消息。

## 7. 注意事项

- WhatsApp Cloud API 对未通过业务认证的测试账号有严格限制（例如只能向验证过的号码发消息，有 24 小时客服窗口限制）。
- 若用于生产环境，请务必绑定真实的手机号，并完成企业的 Business 认证。
- 文本消息最大长度为 4096 字符。
- 若接收消息的接口没有在 10 秒内响应 `200 OK`，WhatsApp 可能会重试并认为你的服务器故障，请确保应用响应迅速。
