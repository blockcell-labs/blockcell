# NapCatQQ 机器人配置指南

BlockCell 支持通过 NapCatQQ 与智能体进行交互。NapCatQQ 是一个基于 OneBot 11 协议的 QQ 机器人实现，支持 WebSocket 和 HTTP 两种通信方式。

> **推荐部署方式**：
> - **WebSocket Client 模式**：BlockCell 主动连接 NapCatQQ 的 WebSocket 服务，适合本地开发和内网部署，无需公网 IP。
> - **WebSocket Server 模式**：NapCatQQ 主动连接 BlockCell，适合 BlockCell 有公网 IP 的场景。

---

## 1. NapCatQQ 简介

NapCatQQ 是基于 NTQQ 的 OneBot 11 协议实现，提供标准化的 QQ 机器人 API。BlockCell 通过 OneBot 11 协议与 NapCatQQ 通信，实现消息收发和群组管理功能。

**主要特性：**

- 支持 OneBot 11 标准协议
- 支持 WebSocket Client/Server 双模式
- 支持 HTTP API 调用
- 支持多种消息段类型（文本、图片、语音、视频、@、表情等）
- 支持群组管理和用户信息查询
- 完善的权限控制系统

---

## 2. 前提条件

### 2.1 安装 NapCatQQ

1. 访问 [NapCatQQ GitHub](https://github.com/NapNeko/NapCatQQ) 下载最新版本。
2. 按照官方文档安装并配置 NapCatQQ。
3. 启动 NapCatQQ 并确保其正常运行。

### 2.2 获取 access_token

1. 在 NapCatQQ 配置文件中设置 `access_token`。
2. 确保 BlockCell 配置中的 `access_token` 与 NapCatQQ 一致。

### 2.3 网络要求

- **Client 模式**：BlockCell 需要能访问 NapCatQQ 的 WebSocket 端口。
- **Server 模式**：NapCatQQ 需要能访问 BlockCell 的 WebSocket 端口。

---

## 3. 连接模式

### 3.1 WebSocket Client 模式（推荐）

BlockCell 作为 WebSocket 客户端，主动连接 NapCatQQ 的 WebSocket 服务。

```json5
{
  "channels": {
    "napcat": {
      "enabled": true,
      "mode": "ws-client",
      "wsUrl": "ws://127.0.0.1:3001",
      "accessToken": "your-access-token"
    }
  }
}
```

### 3.2 WebSocket Server 模式

BlockCell 作为 WebSocket 服务端，等待 NapCatQQ 连接。

```json5
{
  "channels": {
    "napcat": {
      "enabled": true,
      "mode": "ws-server",
      "serverHost": "0.0.0.0",
      "serverPort": 13005,
      "serverPath": "/",
      "accessToken": "your-access-token"
    }
  }
}
```

### 3.3 模式对比

| 特性 | Client 模式 | Server 模式 |
|------|-------------|-------------|
| 公网 IP | 不需要 | BlockCell 需要 |
| 适用场景 | 本地开发、内网部署 | 生产环境、有公网服务器 |
| 连接方向 | BlockCell → NapCatQQ | NapCatQQ → BlockCell |
| 配置复杂度 | 低 | 中 |
| 稳定性 | 依赖网络质量 | 依赖服务端稳定性 |

---

## 4. 配置 BlockCell

### 4.1 单账户配置

编辑 `~/.blockcell/config.json5`：

```json5
{
  "channels": {
    "napcat": {
      "enabled": true,
      "mode": "ws-client",
      "wsUrl": "ws://127.0.0.1:3001",
      "accessToken": "your-access-token",
      "allowFrom": [],
      "allowGroups": [],
      "blockFrom": [],
      "heartbeatIntervalSecs": 30,
      "reconnectDelaySecs": 5,
      "groupResponseMode": "all",
      "autoDownloadMedia": true,
      "mediaDownloadDir": "downloads",
      "maxAutoDownloadSize": 10485760
    }
  },
  "channelOwners": {
    "napcat": "default"
  }
}
```

> **重要**：必须配置 `channelOwners`，否则 Gateway 会因 "enabled channel has no owner" 而拒绝启动。

### 4.2 多账户配置

支持管理多个 QQ 账户，每个账户独立配置：

```json5
{
  "channels": {
    "napcat": {
      "enabled": true,
      "mode": "ws-client",
      "wsUrl": "ws://127.0.0.1:3001",
      "accessToken": "default-token",
      "accounts": {
        "bot1": {
          "enabled": true,
          "wsUrl": "ws://192.168.1.100:3001",
          "accessToken": "bot1-token"
        },
        "bot2": {
          "enabled": true,
          "wsUrl": "ws://192.168.1.101:3001",
          "accessToken": "bot2-token"
        }
      },
      "defaultAccountId": "bot1"
    }
  },
  "channelOwners": {
    "napcat": "default"
  },
  "channelAccountOwners": {
    "napcat": {
      "bot1": "default",
      "bot2": "ops"
    }
  }
}
```

### 4.3 多账户模式详解

Client 模式和 Server 模式都支持多账户，但实现方式有所不同：

#### 多账户支持对比

| 特性 | Client 模式 | Server 模式 |
|------|-------------|-------------|
| 多账户支持 | ✅ 支持 | ✅ 支持 |
| 公网 IP 要求 | 不需要 | BlockCell 需要 |
| NapCatQQ 分布 | 可在不同机器/网络 | 需要能访问 BlockCell |
| 账户区分方式 | 每个账户配置独立 `wsUrl` | 通过 `self_id` (QQ号) 自动识别 |
| 连接管理 | 每账户独立 WebSocket 连接 | Server 统一管理多连接 |

#### Client 模式多账户

Client 模式下，BlockCell 为每个账户创建独立的 WebSocket 连接。每个账户可以连接到不同 NapCatQQ 实例（可在不同机器/网络）：

```json5
{
  "channels": {
    "napcat": {
      "enabled": true,
      "mode": "ws-client",
      "accounts": {
        "bot1": {
          "enabled": true,
          "wsUrl": "ws://192.168.1.100:3001"  // NapCatQQ 实例1
        },
        "bot2": {
          "enabled": true,
          "wsUrl": "ws://192.168.1.101:3001"  // NapCatQQ 实例2（不同机器）
        }
      },
      "defaultAccountId": "bot1"
    }
  }
}
```

> **适用场景**：本地开发、内网部署、多个分散的 NapCatQQ 实例。

#### Server 模式多账户

Server 模式下，多个 NapCatQQ 连接到同一个 BlockCell 服务端。系统通过 OneBot 协议中的 `self_id` 字段（即机器人 QQ 号）自动识别不同账户：

```json5
{
  "channels": {
    "napcat": {
      "enabled": true,
      "mode": "ws-server",
      "serverHost": "0.0.0.0",
      "serverPort": 13005,
      "serverPath": "/"
      // 多个 NapCatQQ 连接此处，通过 self_id 自动区分
    }
  }
}
```

> **适用场景**：生产环境、BlockCell 有公网 IP、NapCatQQ 实例分散在不同位置。

#### 如何选择

| 场景 | 推荐模式 |
|------|----------|
| 本地开发/测试 | Client 模式 |
| 内网部署，无公网 IP | Client 模式 |
| BlockCell 有公网 IP | Server 模式 |
| NapCatQQ 分布在多个内网 | Server 模式 |
| 需要精确控制每个账户配置 | Client 模式 |

### 4.4 媒体自动下载配置

BlockCell 支持接收消息时自动下载媒体文件：

```json5
{
  "channels": {
    "napcat": {
      "enabled": true,
      "autoDownloadMedia": true,        // 自动下载媒体（默认: true）
      "mediaDownloadDir": "downloads",  // 下载目录（默认: "downloads"）
      "maxAutoDownloadSize": 10485760   // 最大文件大小，字节（默认: 10MB）
    }
  }
}
```

启用后，媒体文件（图片、语音、视频、文件）会在消息到达 LLM 之前自动下载，并将本地路径附加到消息内容中。

### 4.5 群消息响应模式

控制机器人如何响应群消息：

```json5
{
  "channels": {
    "napcat": {
      "enabled": true,
      "groupResponseMode": "all"  // "none"、"at_only" 或 "all"（默认: "all"）
    }
  }
}
```

| 模式 | 说明 |
|------|------|
| `all` | 响应所有群消息（默认） |
| `at_only` | 仅在被 @时响应 |
| `none` | 不响应任何群消息 |

### 4.6 配置项说明

#### 主配置项

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `enabled` | bool | 否 | `false` | 是否启用 NapCatQQ 渠道 |
| `mode` | string | 否 | `"ws-client"` | 连接模式：`"ws-client"` 或 `"ws-server"` |
| `wsUrl` | string | 否 | `""` | WebSocket URL（Client 模式） |
| `accessToken` | string | 否 | `""` | 访问令牌，需与 NapCatQQ 一致 |
| `allowFrom` | string[] | 否 | `[]` | 用户白名单（QQ 号），空=允许所有，支持 `"*"` 通配符 |
| `allowGroups` | string[] | 否 | `[]` | 群白名单，空=允许所有群 |
| `blockFrom` | string[] | 否 | `[]` | 用户黑名单，优先级高于 allowFrom |
| `serverHost` | string | 否 | `"0.0.0.0"` | WebSocket 服务端主机 |
| `serverPort` | u16 | 否 | `13005` | WebSocket 服务端端口 |
| `serverPath` | string | 否 | `"/"` | WebSocket 服务端路径 |
| `heartbeatIntervalSecs` | u32 | 否 | `30` | 心跳间隔（秒） |
| `reconnectDelaySecs` | u32 | 否 | `5` | 重连延迟基数（秒，指数退避） |
| `groupResponseMode` | string | 否 | `"all"` | 群消息响应模式：`"all"`、`"at_only"`、`"none"` |
| `autoDownloadMedia` | bool | 否 | `true` | 接收消息时自动下载媒体文件 |
| `mediaDownloadDir` | string | 否 | `"downloads"` | 下载媒体的保存目录 |
| `maxAutoDownloadSize` | u64 | 否 | `10485760` | 自动下载的最大文件大小，字节（默认: 10MB） |

#### 管理员权限配置

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `allowedAdmins` | string[] | 否 | `[]` | 允许执行管理操作的用户，空则继承 allowFrom |
| `allowedGroups` | string[] | 否 | `[]` | 允许执行管理操作的群，空=所有群 |
| `defaultPolicy` | string | 否 | `"deny"` | 默认策略：`"allow"` 或 `"deny"` |
| `toolOverrides` | object | 否 | `{}` | 工具特定权限覆盖 |
| `requireConfirmation` | string[] | 否 | `[]` | 需要确认的工具列表 |

#### 工具权限覆盖配置

| 字段 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `allowedAdmins` | string[]? | 否 | `null` | 覆盖允许的管理员 |
| `allowedGroups` | string[]? | 否 | `null` | 覆盖允许的群 |
| `defaultPolicy` | string? | 否 | `null` | 覆盖默认策略 |
| `requireConfirmation` | bool | 否 | `false` | 是否需要确认 |
| `requireRole` | string? | 否 | `null` | 需要的角色：`owner`/`admin`/`member` |

---

## 5. 权限控制

### 5.1 用户白名单/黑名单

```json5
{
  "allowFrom": ["123456789", "987654321"],  // 只允许这些用户
  "blockFrom": ["999999999"]                 // 黑名单，优先级更高
}
```

- `allowFrom` 为空时允许所有用户
- `blockFrom` 优先级高于 `allowFrom`
- 支持 `"*"` 通配符表示所有用户

### 5.2 群白名单

```json5
{
  "allowGroups": ["111111111", "222222222"]  // 只处理这些群的消息
}
```

### 5.3 管理员权限系统

管理工具（踢人、禁言等）需要额外的权限验证：

```json5
{
  "adminPermissions": {
    "allowedAdmins": ["123456789"],
    "defaultPolicy": "deny",
    "requireConfirmation": ["napcat_set_group_kick"]
  }
}
```

#### 权限模型详解

BlockCell NapCatQQ 采用**分级权限模型**，根据工具风险等级和用户身份授予不同权限：

```
┌─────────────────────────────────────────────────────────────┐
│                     权限授予流程                             │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  用户权限 (授予)                工具权限 (要求)               │
│  ─────────────                 ─────────────                │
│  channel:napcat        ←──┐                               │
│  napcat:read_only      ←──┼── get_group_list, get_msg     │
│  napcat:low_risk       ←──┼── send_like, set_group_card   │
│  napcat:medium_risk    ←──┼── download_file, set_group_ban│
│  napcat:high_risk      ←──┴── set_group_kick, delete_friend│
│                                                             │
│  权限来源:                                                   │
│  1. allow_from 白名单 → read_only + low_risk + medium_risk │
│  2. admin 白名单       → + high_risk                        │
│  3. allow_groups       → 群组限制                           │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

#### 权限检查流程

1. **黑名单检查**: 用户在 `blockFrom` 中 → 直接拒绝
2. **用户白名单**: `allowFrom` 为空或用户在列表中 → 允许
3. **群组白名单**: `allowGroups` 为空或群在列表中 → 允许
4. **权限授予**:
   - 允许的用户获得 `read_only` + `low_risk` + `medium_risk`
   - 管理员用户额外获得 `high_risk`
5. **工具匹配**: 工具要求的权限 ≤ 用户拥有的权限 → 执行

#### 配置示例

**允许所有用户使用基础功能：**
```json5
{
  "allowFrom": [],           // 空 = 允许所有
  "allowGroups": [],         // 空 = 所有群
  "blockFrom": [],           // 无黑名单
  "adminPermissions": {
    "allowedAdmins": ["你的QQ号"],  // 只有你能执行高风险操作
    "defaultPolicy": "deny"
  }
}
```

**严格限制模式：**
```json5
{
  "allowFrom": ["用户1", "用户2"],     // 只允许这两个用户
  "allowGroups": ["管理群"],           // 只响应这个群的消息
  "adminPermissions": {
    "allowedAdmins": ["用户1"],        // 只有用户1是管理员
    "allowedGroups": ["管理群"],       // 只在这个群执行管理操作
    "defaultPolicy": "deny"
  }
}
```

#### adminPermissions.allowedGroups 说明

`adminPermissions.allowedGroups` 用于限制**哪些群可以执行管理操作**：

| 配置 | 效果 |
|------|------|
| `"allowedGroups": []` | **空数组** = 所有群都允许管理操作 |
| `"allowedGroups": ["123456"]` | **只有**这个群可以执行管理操作 |
| `"allowedGroups": ["*"]` | 通配符，所有群都允许 |

**与 allowGroups 的区别：**

| 字段 | 用途 |
|------|------|
| `allowGroups` | 控制**普通用户**可以在哪些群使用机器人 |
| `adminPermissions.allowedGroups` | 控制**管理员操作**可以在哪些群执行 |

### 5.4 工具级权限覆盖

为特定工具配置独立权限：

```json5
{
  "adminPermissions": {
    "toolOverrides": {
      "napcat_set_group_kick": {
        "allowedAdmins": ["123456789"],
        "requireRole": "admin",
        "requireConfirmation": true
      }
    }
  }
}
```

### 5.5 角色要求

| 角色 | 说明 |
|------|------|
| `owner` | 群主 |
| `admin` | 群管理员（含群主） |
| `member` | 普通成员 |

---

## 6. 工具使用说明

### 6.1 群组管理工具

| 工具名称 | 风险等级 | 必填参数 | 功能描述 |
|----------|----------|----------|----------|
| `napcat_get_group_list` | 只读 | - | 获取机器人加入的群列表 |
| `napcat_get_group_info` | 只读 | `group_id` | 获取指定群的详细信息 |
| `napcat_get_group_member_list` | 只读 | `group_id` | 获取群成员列表 |
| `napcat_get_group_member_info` | 只读 | `group_id`, `user_id` | 获取群成员详细信息 |
| `napcat_set_group_kick` | **高风险** | `group_id`, `user_id` | 踢出群成员 |
| `napcat_set_group_ban` | 中风险 | `group_id`, `user_id` | 禁言群成员（`duration=0` 解禁） |
| `napcat_set_group_whole_ban` | 中风险 | `group_id`, `enable` | 全群禁言开关 |
| `napcat_set_group_admin` | 中风险 | `group_id`, `user_id`, `enable` | 设置/取消群管理员 |
| `napcat_set_group_card` | 低风险 | `group_id`, `user_id`, `card` | 设置群名片 |
| `napcat_set_group_name` | 中风险 | `group_id`, `group_name` | 设置群名称 |
| `napcat_set_group_special_title` | 中风险 | `group_id`, `user_id`, `special_title` | 设置群头衔 |
| `napcat_set_group_leave` | **高风险** | `group_id` | 退出群（`is_dismiss=true` 解散群） |

**示例：踢出群成员**

```json
{
  "tool": "napcat_set_group_kick",
  "params": {
    "group_id": "123456789",
    "user_id": "987654321",
    "reject_add_request": false
  }
}
```

### 6.2 用户信息工具

| 工具名称 | 风险等级 | 必填参数 | 功能描述 |
|----------|----------|----------|----------|
| `napcat_get_login_info` | 只读 | - | 获取机器人登录信息 |
| `napcat_get_status` | 只读 | - | 获取 NapCatQQ 运行状态 |
| `napcat_get_version_info` | 只读 | - | 获取 NapCatQQ 版本信息 |
| `napcat_get_stranger_info` | 只读 | `user_id` | 获取用户资料 |
| `napcat_get_friend_list` | 只读 | - | 获取好友列表 |
| `napcat_send_like` | 低风险 | `user_id` | 发送赞（`times`: 1-10） |
| `napcat_set_friend_remark` | 低风险 | `user_id`, `remark` | 设置好友备注 |
| `napcat_delete_friend` | **高风险** | `user_id` | 删除好友 |
| `napcat_set_qq_profile` | 低风险 | - | 设置机器人资料 |

### 6.3 消息操作工具

| 工具名称 | 风险等级 | 必填参数 | 功能描述 |
|----------|----------|----------|----------|
| `napcat_delete_msg` | 中风险 | `message_id` | 撤回消息 |
| `napcat_get_msg` | 只读 | `message_id` | 获取消息详情 |
| `napcat_set_friend_add_request` | 中风险 | `flag`, `approve` | 处理好友请求 |
| `napcat_set_group_add_request` | 中风险 | `flag`, `sub_type`, `approve` | 处理加群请求 |
| `napcat_get_cookies` | 只读 | `domain` | 获取指定域的 Cookies |
| `napcat_get_csrf_token` | 只读 | - | 获取 CSRF Token |

### 6.4 扩展功能工具

| 工具名称 | 风险等级 | 必填参数 | 功能描述 |
|----------|----------|----------|----------|
| `napcat_get_forward_msg` | 只读 | `message_id` | 解析合并转发消息 |
| `napcat_set_msg_emoji_like` | 中风险 | `message_id`, `emoji_id` | 添加表情回应 |
| `napcat_mark_msg_as_read` | 低风险 | `message_id` | 标记消息已读 |
| `napcat_set_essence_msg` | 中风险 | `message_id` | 设置精华消息 |
| `napcat_delete_essence_msg` | 中风险 | `message_id` | 移除精华消息 |
| `napcat_get_essence_msg_list` | 只读 | `group_id` | 获取精华消息列表 |
| `napcat_get_group_at_all_remain` | 只读 | `group_id` | 获取 @全体成员 剩余次数 |
| `napcat_get_image` | 只读 | `file` | 获取图片信息和下载链接 |
| `napcat_get_record` | 只读 | `file` | 获取语音信息和下载链接 |
| `napcat_download_file` | 中风险 | `url` | 通过 NapCat 下载文件 |

**所有工具通用可选参数：**

- `account_id`: 多账户场景下的账户 ID

---

## 7. 事件处理

### 7.1 消息事件

| 事件类型 | 说明 |
|----------|------|
| `private` | 私聊消息 |
| `group` | 群消息 |
| `group_private` | 群临时会话 |
| `message_sent` | 消息发送事件 |

### 7.2 通知事件

| 事件类型 | 说明 |
|----------|------|
| `group_recall` | 群消息撤回 |
| `friend_recall` | 好友消息撤回 |
| `group_increase` | 群成员增加 |
| `group_decrease` | 群成员减少 |
| `group_admin` | 群管理员变动 |
| `group_ban` | 群禁言 |
| `group_upload` | 群文件上传 |
| `poke` | 戳一戳 |
| `friend_add` | 好友添加 |
| `group_card` | 群名片变更 |
| `essence` | 精华消息 |

### 7.3 请求事件

| 事件类型 | 说明 |
|----------|------|
| `friend` | 好友请求 |
| `group` | 群邀请/加群请求 |

### 7.4 元事件

| 事件类型 | 说明 |
|----------|------|
| `lifecycle` | 生命周期事件（enable/disable/connect） |
| `heartbeat` | 心跳事件 |

---

## 8. 消息段支持

| 类型 | 说明 | 关键字段 |
|------|------|----------|
| `text` | 纯文本 | `text` |
| `face` | QQ 表情 | `id` |
| `mface` | 商城表情 | `emoji_id`, `emoji_package_id` |
| `image` | 图片 | `file`, `url`, `summary` |
| `record` | 语音 | `file`, `url` |
| `video` | 视频 | `file`, `url`, `thumb` |
| `at` | @某人 | `qq`, `name` |
| `at_all` | @全体成员 | - |
| `rps` | 石头剪刀布 | `result` |
| `dice` | 骰子 | `result` |
| `poke` | 戳一戳 | `type`, `id` |
| `music` | 音乐分享 | `type`, `id`, `url`, `title` |
| `share` | 链接分享 | `url`, `title`, `content`, `image` |
| `reply` | 回复消息 | `id`, `text`, `qq`, `time` |
| `forward` | 合并转发 | `id` |
| `node` | 自定义转发节点 | `id`, `user_id`, `nickname`, `content` |
| `xml` | XML 消息 | `data` |
| `json` | JSON 消息 | `data` |
| `card` | 卡片消息 | `data` |
| `file` | 文件 | `file`, `url`, `name`, `size` |

---

## 9. 启动与测试

### 9.1 启动 Gateway

```bash
blockcell gateway
```

### 9.2 检查连接状态

启动后，日志应显示：

```
NapCatQQ channel started (client mode)
Connecting to NapCatQQ WebSocket server: ws://127.0.0.1:3001
NapCatQQ WebSocket connected
```

### 9.3 验证配置

使用工具测试连接：

```bash
# 获取登录信息
curl -X POST http://127.0.0.1:18790/api/tool \
  -H "Content-Type: application/json" \
  -d '{"tool": "napcat_get_login_info", "params": {}}'
```

---

## 10. 常见问题排查

| 问题 | 可能原因 | 解决方法 |
|------|----------|----------|
| WebSocket 连接失败 | NapCatQQ 未启动或地址错误 | 检查 NapCatQQ 状态和 wsUrl 配置 |
| 认证失败 | access_token 不匹配 | 确保 BlockCell 和 NapCatQQ 的 token 一致 |
| 权限被拒绝 | 用户不在白名单或权限不足 | 检查 allowFrom、adminPermissions 配置 |
| 消息无响应 | 未配置 channelOwners | 添加 `"channelOwners": {"napcat": "default"}` |
| 工具调用失败 | 参数错误或权限不足 | 检查工具参数和权限配置 |

---

## 11. 注意事项

### 11.1 风险等级说明

| 等级 | 说明 | 示例工具 |
|------|------|----------|
| 只读 | 无副作用，仅查询信息 | `get_group_list`, `get_login_info` |
| 低风险 | 可逆修改，影响小 | `set_group_card`, `send_like` |
| 中风险 | 显著但可逆的修改 | `set_group_ban`, `delete_msg` |
| 高风险 | 不可逆或重大影响 | `set_group_kick`, `delete_friend` |

### 11.2 安全建议

- 生产环境务必配置 `access_token`
- 使用 `allowFrom` 和 `blockFrom` 限制用户访问
- 管理工具配置 `adminPermissions` 权限验证
- 高风险工具配置 `requireConfirmation` 确认机制

### 11.3 API 限制

- QQ 对消息发送有频率限制，BlockCell 内置了限流机制
- 消息长度有限制，超长消息会自动处理
- 部分功能需要特定权限（如群管理需要管理员权限）