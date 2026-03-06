# WhatsApp 桥接配置指南

Blockcell 当前的 WhatsApp 渠道不是 Meta Cloud API / Webhook 模式，而是通过一个 **WhatsApp bridge WebSocket 服务** 接入。

这意味着：

- Blockcell 负责连接 bridge（默认 `ws://localhost:3001`）
- bridge 负责维护 WhatsApp 会话、输出登录二维码、转发入站消息
- 你不需要给 Blockcell 配置公网 Webhook
- 首次登录通常通过扫描 bridge 提供的二维码完成

## 1. 准备 WhatsApp bridge

你需要先准备一个可运行的 WhatsApp bridge 服务，并确保它能通过 WebSocket 提供连接。

默认地址是：

```text
ws://localhost:3001
```

如果你的 bridge 跑在其它地址，后面把 `bridgeUrl` 改成对应值即可。

## 2. 配置 Blockcell

编辑 `~/.blockcell/config.json`：

```json
{
  "channelOwners": {
    "whatsapp": "default"
  },
  "channelAccountOwners": {
    "whatsapp": {
      "bot2": "ops"
    }
  },
  "channels": {
    "whatsapp": {
      "enabled": true,
      "bridgeUrl": "ws://localhost:3001",
      "allowFrom": ["8613800138000"]
    }
  }
}
```

### 配置项说明

- `enabled`：是否启用 WhatsApp 渠道
- `bridgeUrl`：WhatsApp bridge 的 WebSocket 地址；默认值通常为 `ws://localhost:3001`
- `allowFrom`：允许访问的发送方号码列表（不带 `+` 号），例如 `8613800138000`

> 如果你通过 `blockcell gateway` 启用 WhatsApp，还需要配置 `channelOwners.whatsapp`。如果同一渠道下有多个账号，也可以继续加 `channelAccountOwners.whatsapp.<accountId> = "ops"`，把指定账号路由给其它 agent。否则 Gateway 会因缺少 owner 而拒绝启动。

## 3. 启动与登录

先启动 bridge，再启动 Blockcell gateway：

```bash
blockcell gateway
```

如果你需要查看登录流程，可执行：

```bash
blockcell channels login whatsapp
```

当前 CLI 会提示标准流程：

1. 确保 WhatsApp bridge 已运行
2. bridge 会显示二维码
3. 用手机上的 WhatsApp 扫码登录

如果你手动管理 bridge，也可以参考提示中的默认启动方式：

```bash
cd ~/.blockcell/bridge && npm start
```

## 4. 交互方式

- **单聊**：从允许的号码直接给关联账号发消息
- **白名单控制**：建议始终配置 `allowFrom`，避免任何能连上 bridge 的号码都能控制你的 agent

## 5. 注意事项

- `allowFrom` 中的号码通常使用国际区号格式，但**不带 `+`**
- 如果 `bridgeUrl` 不可达，Gateway 中会看到连接失败或 bridge 断开的日志
- 如果 bridge 已登录但收不到消息，优先检查 bridge 自身状态，再检查 Blockcell 的 `allowFrom`
- 如果你要做多账号隔离，可以进一步使用 `channels.whatsapp.accounts` 与 `defaultAccountId`
