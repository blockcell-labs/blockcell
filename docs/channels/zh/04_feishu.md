# 飞书 (Feishu) 机器人配置指南

Blockcell 支持通过飞书（或 Lark）机器人与智能体进行交互。飞书渠道使用 **长连接 (WebSocket)** 模式接收消息，无需配置公网 Webhook 地址，非常适合本地开发、内网部署和调试。

## 配置顺序总览

> ⚠️ **重要：请严格按照以下顺序操作。**
>
> 飞书长连接存在"先有鸡还是先有蛋"的问题：
> - 飞书开放平台后台要求**应用先成功建立长连接**，才能保存"使用长连接接收事件"配置
> - 而应用只有在后台正确配置并发布后，才能成功建立长连接
>
> **正确顺序：** 创建应用 → 开启机器人能力 → 申请权限 → 添加事件订阅（先不改接收方式）→ 发布应用 → **启动 Blockcell gateway** → 再回后台切换为长连接接收 → 保存

## 1. 申请飞书自建应用

1. 登录并访问 [飞书开放平台 - 开发者后台](https://open.feishu.cn/app)。
2. 点击 **创建企业自建应用**。
3. 输入 **应用名称**（如：Blockcell Bot）和描述，点击 **创建**。
4. 创建成功后，在左侧菜单 **凭证与基础信息** 中，复制并保存你的 **App ID** 和 **App Secret**。

## 2. 开启机器人能力

> ⚠️ **这是长连接能正常工作的前提条件，不可跳过。**

1. 在左侧菜单中选择 **添加应用能力**。
2. 找到 **机器人**，点击 **添加**。
3. 确认机器人能力已出现在已添加能力列表中。

## 3. 申请应用权限

机器人需要相应的权限才能读取和发送消息。

1. 在左侧菜单中选择 **权限管理**。
2. 切换到 **API 权限** 标签。
3. 搜索并申请以下权限（至少）：
   - `im:message` — 获取单聊、群组消息
   - `im:message.group_at_msg` — 获取群组中 @机器人 的消息
   - `im:message.p2p_msg` — 获取用户发给机器人的单聊消息
   - `im:message:send_as_bot` — 以应用身份发送消息
   - `im:resource` — 获取消息中的资源文件（图片、文件等）

*注意：修改权限后，必须发布新版本才能生效。*

## 4. 添加事件订阅

1. 在左侧菜单中选择 **事件订阅**。
2. 此时**暂时保持默认的 HTTP 回调方式**（不要切换为长连接，因为应用还未启动）。
3. 点击 **添加事件**，搜索并添加以下事件：
   - `im.message.receive_v1` — 接收消息（必须）
4. 点击 **保存**。

## 5. 发布应用

1. 在左侧菜单中选择 **应用发布** → **版本管理与发布**。
2. 点击 **创建版本**。
3. 输入版本号（如 `1.0.0`）和更新说明。
4. 点击 **保存**，然后点击 **申请发布**。
5. 企业管理员（或你自己，如果是管理员）在飞书管理后台审核通过后，应用即可在企业内使用。

## 6. 配置 Blockcell

在 `~/.blockcell/config.json` 中修改 `feishu` 部分：

```json
{
  "channels": {
    "feishu": {
      "enabled": true,
      "appId": "cli_a1b2c3d4e5f6g7h8",
      "appSecret": "A1B2C3D4E5F6G7H8I9J0K1L2M3N4O5P6",
      "allowFrom": ["ou_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"]
    }
  }
}
```

### 配置项说明

| 字段 | 说明 |
|------|------|
| `enabled` | 是否启用飞书渠道（`true` 或 `false`） |
| `appId` | 凭证与基础信息中的 App ID |
| `appSecret` | 凭证与基础信息中的 App Secret |
| `allowFrom` | 允许访问的用户 `open_id` 列表。留空 `[]` 则允许企业内所有人 |

## 7. 启动 Blockcell 并切换为长连接

1. 启动 Blockcell gateway：
   ```bash
   blockcell gateway
   ```
2. 观察日志，确认出现以下内容（表示长连接已成功建立）：
   ```
   INFO blockcell_channels::feishu: Feishu tenant_access_token refreshed (cached 2h)
   INFO blockcell_channels::feishu: Connecting to Feishu WebSocket url=wss://...
   INFO blockcell_channels::feishu: Connected to Feishu WebSocket
   ```
3. **应用成功连接后**，回到飞书开放平台后台 → **事件订阅**。
4. 将接收方式切换为 **使用长连接接收事件**，点击 **保存**。
   - 此时后台不再报"应用未建立长连接"，因为应用已经在线。

## 8. 获取用户 ID（用于白名单）

飞书的 `sender_id` 使用 `open_id` 格式（如 `ou_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx`）。

获取方式：
- **方法一**：暂时将 `allowFrom` 设为 `[]`，启动后向机器人发一条消息，在 Blockcell 日志中会打印 `sender_id`，复制后填入白名单。
- **方法二**：在飞书开放平台 → **API 调试工具** → 调用 `获取用户信息` 接口查询。

## 9. 交互方式

- **单聊**：在飞书搜索框中输入机器人名称，点击进入对话框直接发送消息。
- **群聊**：将机器人添加到群组中，在群里发送 `@机器人 你的消息`。

## 10. 常见错误排查

| 错误日志 | 原因 | 解决方案 |
|----------|------|----------|
| `Feishu endpoint error code=1161001` | 应用未开启机器人能力 | 开放平台 → 添加应用能力 → 机器人 → 添加，然后重新发布 |
| `Feishu endpoint error code=1161002` | 应用未发布 | 创建版本并申请发布，管理员审批后生效 |
| `Feishu endpoint error code=99991663` | App ID 或 App Secret 错误 | 检查配置文件中的 `appId` 和 `appSecret` |
| `Feishu endpoint error code=10003` | 缺少权限 | 权限管理中申请所需权限，重新发布版本 |
| `Failed to parse endpoint response` | 响应体非 JSON（网络或配置问题） | 检查网络连通性，日志中会打印原始响应体供诊断 |
| 后台报"应用未建立长连接" | 应用尚未启动或连接失败 | 先启动 `blockcell gateway` 确认连接成功，再回后台保存配置 |

## 11. 注意事项

- 长连接 (WebSocket) 在网络不稳定时会自动重连（5 秒后重试），确保服务高可用。
- 飞书富文本消息最大长度约 30,000 字符，Blockcell 会自动处理超长回复的截断。
- 消息中的图片、文件、音频、视频会自动下载到本地 `workspace/media/` 目录，支持多模态交互。
- 长连接模式**不需要**公网 IP 或域名，适合本地和内网部署。
- `tenant_access_token` 有效期 2 小时，Blockcell 会在过期前 5 分钟自动刷新，无需手动干预。
