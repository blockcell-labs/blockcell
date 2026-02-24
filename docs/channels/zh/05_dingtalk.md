# 钉钉 (DingTalk) 机器人配置指南

Blockcell 支持通过钉钉企业内部机器人与智能体进行交互。钉钉渠道使用 **Stream SDK (长连接 WebSocket)** 模式接收消息，无需配置公网 Webhook 地址，非常适合本地开发、内网部署和调试。

## 1. 创建钉钉企业内部应用

1. 登录并访问 [钉钉开发者后台](https://open-dev.dingtalk.com/)。
2. 在 **应用开发** -> **企业内部开发** 中，点击 **创建应用**。
3. 填写应用名称（如：Blockcell Bot）和应用描述，选择应用图标，点击 **保存**。
4. 创建成功后，在 **基础信息** 页面，复制并保存 **AppKey** 和 **AppSecret**。

## 2. 添加机器人能力

1. 在左侧菜单中选择 **应用功能** -> **添加应用能力**。
2. 找到 **机器人**，点击 **添加**。
3. 配置机器人信息（名称、简介、头像等）。
4. **消息接收模式**：必须选择 **Stream 模式**（这是实现内网穿透、免配置 Webhook 的关键）。
5. 点击 **发布**（或保存）。

## 3. 申请接口权限

机器人需要申请相应的接口权限才能读取和发送消息。

1. 在左侧菜单中选择 **开发配置** -> **权限管理**。
2. 在权限搜索框中，搜索并申请以下权限（或确保其已开启）：
   - `企业内机器人发送消息`
   - （如需获取人员信息等，请根据需要申请通讯录只读权限）

## 4. 获取机器人 Code (Robot Code)

在部分新版钉钉 API 中（如 `v1.0/robot/oToMessages/batchSend` 单聊发送），需要提供 `robotCode`。
- `robotCode` 通常与应用的 `AppKey` 相同。
- 如果你的应用配置了单独的 `robotCode`，请在机器人详情页查看。

## 5. 版本发布

1. 在左侧菜单中选择 **应用发布** -> **版本管理与发布**。
2. 点击 **创建新版本**。
3. 填写版本信息，选择可见范围（如：全员可见或指定人员），点击 **发布**。
4. 只有在可见范围内的人员才能在钉钉中搜索并使用该机器人。

## 6. 获取用户 ID（用于白名单）

钉钉的 `sender_id` 通常是企业内的 `staffId`（员工 ID）。
如果你不知道自己的 staffId，可以先将 `allowFrom` 留空，然后在后台日志中观察发送者 ID，或者联系企业管理员在通讯录中查询。

## 7. 配置 Blockcell

在 Blockcell 的配置文件中，修改 `dingtalk` 部分：

```json
{
  "channels": {
    "dingtalk": {
      "enabled": true,
      "appKey": "ding_xxxxxxxxxxxxxxx",
      "appSecret": "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
      "robotCode": "ding_xxxxxxxxxxxxxxx",
      "allowFrom": ["manager_staff_id"]
    }
  }
}
```

### 配置项说明

- `enabled`: 是否启用钉钉渠道（`true` 或 `false`）。
- `appKey`: 在基础信息中获取的 AppKey。
- `appSecret`: 在基础信息中获取的 AppSecret。
- `robotCode`: 机器人的唯一标识（通常与 appKey 相同）。
- `allowFrom`: 允许访问的用户 `staffId` 列表（字符串数组）。如果留空 `[]`，则允许企业内可见范围的任何人与机器人交互。

## 8. 交互方式

- **单聊**：在钉钉搜索框中输入你的机器人名称，点击进入对话框直接发送消息。
- **群聊**：打开群组设置 -> **群机器人** -> **添加机器人** -> 搜索你的企业内部机器人并添加。在群里发送 `@机器人 你的消息`。

## 9. 注意事项

- 钉钉单条文本消息最大长度为 4096 字符，超长消息 Blockcell 会自动切片并分条发送。
- Blockcell 内部实现了指数退避重连机制，当钉钉 Stream 服务端断开时会自动恢复。
- 群聊和单聊使用了不同的钉钉 API 接口（群聊使用老版 `chat/send`，单聊使用新版 `v1.0/robot/oToMessages/batchSend`），确保你的应用具有相关发送权限。
