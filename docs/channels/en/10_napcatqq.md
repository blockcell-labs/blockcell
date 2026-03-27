# NapCatQQ Bot Configuration Guide

BlockCell supports interaction with agents through NapCatQQ. NapCatQQ is a QQ bot implementation based on the OneBot 11 protocol, supporting both WebSocket and HTTP communication methods.

> **Recommended Deployment**:
> - **WebSocket Client Mode**: BlockCell actively connects to NapCatQQ's WebSocket service, suitable for local development and intranet deployment without public IP.
> - **WebSocket Server Mode**: NapCatQQ actively connects to BlockCell, suitable for scenarios where BlockCell has a public IP.

---

## 1. Introduction to NapCatQQ

NapCatQQ is an OneBot 11 protocol implementation based on NTQQ, providing standardized QQ bot APIs. BlockCell communicates with NapCatQQ through the OneBot 11 protocol to enable message sending/receiving and group management functions.

**Key Features:**

- OneBot 11 standard protocol support
- WebSocket Client/Server dual mode support
- HTTP API support
- Multiple message segment types (text, image, voice, video, @, emoji, etc.)
- Group management and user information query
- Comprehensive permission control system

---

## 2. Prerequisites

### 2.1 Install NapCatQQ

1. Visit [NapCatQQ GitHub](https://github.com/NapNeko/NapCatQQ) to download the latest version.
2. Follow the official documentation to install and configure NapCatQQ.
3. Start NapCatQQ and ensure it is running properly.

### 2.2 Get access_token

1. Set `access_token` in the NapCatQQ configuration file.
2. Ensure the `access_token` in BlockCell configuration matches NapCatQQ.

### 2.3 Network Requirements

- **Client Mode**: BlockCell needs to access NapCatQQ's WebSocket port.
- **Server Mode**: NapCatQQ needs to access BlockCell's WebSocket port.

---

## 3. Connection Modes

### 3.1 WebSocket Client Mode (Recommended)

BlockCell acts as a WebSocket client, actively connecting to NapCatQQ's WebSocket service.

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

### 3.2 WebSocket Server Mode

BlockCell acts as a WebSocket server, waiting for NapCatQQ to connect.

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

### 3.3 Mode Comparison

| Feature | Client Mode | Server Mode |
|---------|-------------|-------------|
| Public IP | Not required | BlockCell needs it |
| Use Case | Local development, intranet | Production, public server |
| Connection Direction | BlockCell → NapCatQQ | NapCatQQ → BlockCell |
| Configuration Complexity | Low | Medium |
| Stability | Depends on network quality | Depends on server stability |

---

## 4. Configure BlockCell

### 4.1 Single Account Configuration

Edit `~/.blockcell/config.json5`:

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

> **Important**: You must configure `channelOwners`, otherwise Gateway will refuse to start with "enabled channel has no owner".

### 4.2 Multi-Account Configuration

Support for managing multiple QQ accounts, each with independent configuration:

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

### 4.3 Multi-Account Mode Details

Both Client mode and Server mode support multiple accounts, but with different implementation approaches:

#### Multi-Account Support Comparison

| Feature | Client Mode | Server Mode |
|---------|-------------|-------------|
| Multi-Account Support | ✅ Supported | ✅ Supported |
| Public IP Required | Not required | BlockCell needs it |
| NapCatQQ Distribution | Can be on different machines/networks | Must be able to access BlockCell |
| Account Identification | Independent `wsUrl` per account | Auto-identified via `self_id` (QQ number) |
| Connection Management | Independent WebSocket per account | Server manages all connections |

#### Client Mode Multi-Account

In Client mode, BlockCell creates an independent WebSocket connection for each account. Each account can connect to a different NapCatQQ instance (on different machines/networks):

```json5
{
  "channels": {
    "napcat": {
      "enabled": true,
      "mode": "ws-client",
      "accounts": {
        "bot1": {
          "enabled": true,
          "wsUrl": "ws://192.168.1.100:3001"  // NapCatQQ instance 1
        },
        "bot2": {
          "enabled": true,
          "wsUrl": "ws://192.168.1.101:3001"  // NapCatQQ instance 2 (different machine)
        }
      },
      "defaultAccountId": "bot1"
    }
  }
}
```

> **Use Case**: Local development, intranet deployment, multiple distributed NapCatQQ instances.

#### Server Mode Multi-Account

In Server mode, multiple NapCatQQ instances connect to the same BlockCell server. The system automatically identifies different accounts via the `self_id` field (the bot's QQ number) in the OneBot protocol:

```json5
{
  "channels": {
    "napcat": {
      "enabled": true,
      "mode": "ws-server",
      "serverHost": "0.0.0.0",
      "serverPort": 13005,
      "serverPath": "/"
      // Multiple NapCatQQ connect here, auto-distinguished by self_id
    }
  }
}
```

> **Use Case**: Production environment, BlockCell has public IP, NapCatQQ instances distributed across locations.

#### How to Choose

| Scenario | Recommended Mode |
|----------|------------------|
| Local development/testing | Client Mode |
| Intranet deployment, no public IP | Client Mode |
| BlockCell has public IP | Server Mode |
| NapCatQQ distributed across multiple intranets | Server Mode |
| Need precise per-account configuration | Client Mode |

### 4.4 Media Auto-Download Configuration

BlockCell supports automatic media download when receiving messages:

```json5
{
  "channels": {
    "napcat": {
      "enabled": true,
      "autoDownloadMedia": true,        // Auto-download media (default: true)
      "mediaDownloadDir": "downloads",  // Download directory (default: "downloads")
      "maxAutoDownloadSize": 10485760   // Max size in bytes (default: 10MB)
    }
  }
}
```

When enabled, media files (images, voice, video, files) are automatically downloaded before the message reaches LLM, and local paths are attached to the message content.

### 4.5 Group Message Response Mode

Control how the bot responds to group messages:

```json5
{
  "channels": {
    "napcat": {
      "enabled": true,
      "groupResponseMode": "all"  // "none", "at_only", or "all" (default: "all")
    }
  }
}
```

| Mode | Description |
|------|-------------|
| `all` | Respond to all group messages (default) |
| `at_only` | Only respond when bot is @mentioned |
| `none` | Do not respond to any group messages |

### 4.6 Configuration Reference

#### Main Configuration

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `enabled` | bool | No | `false` | Whether to enable NapCatQQ channel |
| `mode` | string | No | `"ws-client"` | Connection mode: `"ws-client"` or `"ws-server"` |
| `wsUrl` | string | No | `""` | WebSocket URL (Client mode) |
| `accessToken` | string | No | `""` | Access token, must match NapCatQQ |
| `allowFrom` | string[] | No | `[]` | User whitelist (QQ numbers), empty=all allowed, supports `"*"` wildcard |
| `allowGroups` | string[] | No | `[]` | Group whitelist, empty=all groups |
| `blockFrom` | string[] | No | `[]` | User blacklist, higher priority than allowFrom |
| `serverHost` | string | No | `"0.0.0.0"` | WebSocket server host |
| `serverPort` | u16 | No | `13005` | WebSocket server port |
| `serverPath` | string | No | `"/"` | WebSocket server path |
| `heartbeatIntervalSecs` | u32 | No | `30` | Heartbeat interval (seconds) |
| `reconnectDelaySecs` | u32 | No | `5` | Reconnect delay base (seconds, exponential backoff) |
| `groupResponseMode` | string | No | `"all"` | Group message response mode: `"all"`, `"at_only"`, `"none"` |
| `autoDownloadMedia` | bool | No | `true` | Auto-download media files when receiving messages |
| `mediaDownloadDir` | string | No | `"downloads"` | Directory to save downloaded media |
| `maxAutoDownloadSize` | u64 | No | `10485760` | Max auto-download file size in bytes (default: 10MB) |

#### Admin Permissions Configuration

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `allowedAdmins` | string[] | No | `[]` | Users allowed to execute admin operations, empty inherits allowFrom |
| `allowedGroups` | string[] | No | `[]` | Groups allowed for admin operations, empty=all groups |
| `defaultPolicy` | string | No | `"deny"` | Default policy: `"allow"` or `"deny"` |
| `toolOverrides` | object | No | `{}` | Tool-specific permission overrides |
| `requireConfirmation` | string[] | No | `[]` | Tools requiring confirmation |

#### Tool Permission Override Configuration

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `allowedAdmins` | string[]? | No | `null` | Override allowed admins |
| `allowedGroups` | string[]? | No | `null` | Override allowed groups |
| `defaultPolicy` | string? | No | `null` | Override default policy |
| `requireConfirmation` | bool | No | `false` | Whether confirmation is required |
| `requireRole` | string? | No | `null` | Required role: `owner`/`admin`/`member` |

---

## 5. Permission Control

### 5.1 User Whitelist/Blacklist

```json5
{
  "allowFrom": ["123456789", "987654321"],  // Only allow these users
  "blockFrom": ["999999999"]                 // Blacklist, higher priority
}
```

- Empty `allowFrom` allows all users
- `blockFrom` has higher priority than `allowFrom`
- Supports `"*"` wildcard for all users

### 5.2 Group Whitelist

```json5
{
  "allowGroups": ["111111111", "222222222"]  // Only process messages from these groups
}
```

### 5.3 Admin Permission System

Admin tools (kick, ban, etc.) require additional permission verification:

```json5
{
  "adminPermissions": {
    "allowedAdmins": ["123456789"],
    "defaultPolicy": "deny",
    "requireConfirmation": ["napcat_set_group_kick"]
  }
}
```

#### Permission Model Details

BlockCell NapCatQQ uses a **tiered permission model**, granting different permissions based on tool risk level and user identity:

```
┌─────────────────────────────────────────────────────────────┐
│                     Permission Grant Flow                    │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  User Permissions (Granted)      Tool Permissions (Required)│
│  ────────────────────            ─────────────────────────  │
│  channel:napcat        ←──┐                                │
│  napcat:read_only      ←──┼── get_group_list, get_msg      │
│  napcat:low_risk       ←──┼── send_like, set_group_card    │
│  napcat:medium_risk    ←──┼── download_file, set_group_ban │
│  napcat:high_risk      ←──┴── set_group_kick, delete_friend│
│                                                             │
│  Permission Sources:                                        │
│  1. allow_from whitelist → read_only + low_risk + medium_risk │
│  2. admin whitelist      → + high_risk                       │
│  3. allow_groups         → group restriction                 │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

#### Permission Check Flow

1. **Blacklist Check**: User in `blockFrom` → Deny immediately
2. **User Whitelist**: `allowFrom` empty or user in list → Allow
3. **Group Whitelist**: `allowGroups` empty or group in list → Allow
4. **Permission Grant**:
   - Allowed users get `read_only` + `low_risk` + `medium_risk`
   - Admin users additionally get `high_risk`
5. **Tool Match**: Tool required permissions ≤ User permissions → Execute

#### Configuration Examples

**Allow all users to use basic features:**
```json5
{
  "allowFrom": [],           // Empty = allow all
  "allowGroups": [],         // Empty = all groups
  "blockFrom": [],           // No blacklist
  "adminPermissions": {
    "allowedAdmins": ["your_qq_number"],  // Only you can execute high-risk operations
    "defaultPolicy": "deny"
  }
}
```

**Strict restriction mode:**
```json5
{
  "allowFrom": ["user1", "user2"],     // Only allow these users
  "allowGroups": ["admin_group"],      // Only respond to this group's messages
  "adminPermissions": {
    "allowedAdmins": ["user1"],        // Only user1 is admin
    "allowedGroups": ["admin_group"],  // Only execute admin operations in this group
    "defaultPolicy": "deny"
  }
}
```

#### adminPermissions.allowedGroups Explanation

`adminPermissions.allowedGroups` restricts **which groups can execute admin operations**:

| Configuration | Effect |
|---------------|--------|
| `"allowedGroups": []` | **Empty array** = All groups allow admin operations |
| `"allowedGroups": ["123456"]` | **Only** this group can execute admin operations |
| `"allowedGroups": ["*"]` | Wildcard, all groups allow |

**Difference from allowGroups:**

| Field | Purpose |
|-------|---------|
| `allowGroups` | Controls which groups **normal users** can use the bot |
| `adminPermissions.allowedGroups` | Controls which groups can execute **admin operations** |

### 5.4 Tool-Level Permission Override

Configure independent permissions for specific tools:

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

### 5.5 Role Requirements

| Role | Description |
|------|-------------|
| `owner` | Group owner |
| `admin` | Group admin (includes owner) |
| `member` | Regular member |

---

## 6. Tool Reference

### 6.1 Group Management Tools

| Tool Name | Risk Level | Required Params | Description |
|-----------|------------|-----------------|-------------|
| `napcat_get_group_list` | ReadOnly | - | Get list of groups the bot has joined |
| `napcat_get_group_info` | ReadOnly | `group_id` | Get detailed information of a group |
| `napcat_get_group_member_list` | ReadOnly | `group_id` | Get group member list |
| `napcat_get_group_member_info` | ReadOnly | `group_id`, `user_id` | Get group member details |
| `napcat_set_group_kick` | **HighRisk** | `group_id`, `user_id` | Kick group member |
| `napcat_set_group_ban` | MediumRisk | `group_id`, `user_id` | Ban group member (`duration=0` to unban) |
| `napcat_set_group_whole_ban` | MediumRisk | `group_id`, `enable` | Toggle whole group ban |
| `napcat_set_group_admin` | MediumRisk | `group_id`, `user_id`, `enable` | Set/remove group admin |
| `napcat_set_group_card` | LowRisk | `group_id`, `user_id`, `card` | Set group card |
| `napcat_set_group_name` | MediumRisk | `group_id`, `group_name` | Set group name |
| `napcat_set_group_special_title` | MediumRisk | `group_id`, `user_id`, `special_title` | Set group special title |
| `napcat_set_group_leave` | **HighRisk** | `group_id` | Leave group (`is_dismiss=true` to dismiss) |

**Example: Kick Group Member**

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

### 6.2 User Information Tools

| Tool Name | Risk Level | Required Params | Description |
|-----------|------------|-----------------|-------------|
| `napcat_get_login_info` | ReadOnly | - | Get bot login information |
| `napcat_get_status` | ReadOnly | - | Get NapCatQQ running status |
| `napcat_get_version_info` | ReadOnly | - | Get NapCatQQ version information |
| `napcat_get_stranger_info` | ReadOnly | `user_id` | Get user profile |
| `napcat_get_friend_list` | ReadOnly | - | Get friend list |
| `napcat_send_like` | LowRisk | `user_id` | Send like (`times`: 1-10) |
| `napcat_set_friend_remark` | LowRisk | `user_id`, `remark` | Set friend remark |
| `napcat_delete_friend` | **HighRisk** | `user_id` | Delete friend |
| `napcat_set_qq_profile` | LowRisk | - | Set bot profile |

### 6.3 Message Operation Tools

| Tool Name | Risk Level | Required Params | Description |
|-----------|------------|-----------------|-------------|
| `napcat_delete_msg` | MediumRisk | `message_id` | Recall message |
| `napcat_get_msg` | ReadOnly | `message_id` | Get message details |
| `napcat_set_friend_add_request` | MediumRisk | `flag`, `approve` | Handle friend request |
| `napcat_set_group_add_request` | MediumRisk | `flag`, `sub_type`, `approve` | Handle group join request |
| `napcat_get_cookies` | ReadOnly | `domain` | Get cookies for specified domain |
| `napcat_get_csrf_token` | ReadOnly | - | Get CSRF token |

### 6.4 Extended Feature Tools

| Tool Name | Risk Level | Required Params | Description |
|-----------|------------|-----------------|-------------|
| `napcat_get_forward_msg` | ReadOnly | `message_id` | Parse combined forward message |
| `napcat_set_msg_emoji_like` | MediumRisk | `message_id`, `emoji_id` | Add emoji reaction |
| `napcat_mark_msg_as_read` | LowRisk | `message_id` | Mark message as read |
| `napcat_set_essence_msg` | MediumRisk | `message_id` | Set essence message |
| `napcat_delete_essence_msg` | MediumRisk | `message_id` | Remove essence message |
| `napcat_get_essence_msg_list` | ReadOnly | `group_id` | Get essence message list |
| `napcat_get_group_at_all_remain` | ReadOnly | `group_id` | Get remaining @all count |
| `napcat_get_image` | ReadOnly | `file` | Get image info and download URL |
| `napcat_get_record` | ReadOnly | `file` | Get voice info and download URL |
| `napcat_download_file` | MediumRisk | `url` | Download file via NapCat |

**Common Optional Parameter for All Tools:**

- `account_id`: Account ID in multi-account scenarios

---

## 7. Event Handling

### 7.1 Message Events

| Event Type | Description |
|------------|-------------|
| `private` | Private message |
| `group` | Group message |
| `group_private` | Group temporary session |
| `message_sent` | Message sent event |

### 7.2 Notice Events

| Event Type | Description |
|------------|-------------|
| `group_recall` | Group message recall |
| `friend_recall` | Friend message recall |
| `group_increase` | Group member increase |
| `group_decrease` | Group member decrease |
| `group_admin` | Group admin change |
| `group_ban` | Group ban |
| `group_upload` | Group file upload |
| `poke` | Poke |
| `friend_add` | Friend add |
| `group_card` | Group card change |
| `essence` | Essence message |

### 7.3 Request Events

| Event Type | Description |
|------------|-------------|
| `friend` | Friend request |
| `group` | Group invite/join request |

### 7.4 Meta Events

| Event Type | Description |
|------------|-------------|
| `lifecycle` | Lifecycle event (enable/disable/connect) |
| `heartbeat` | Heartbeat event |

---

## 8. Message Segment Support

| Type | Description | Key Fields |
|------|-------------|------------|
| `text` | Plain text | `text` |
| `face` | QQ emoji | `id` |
| `mface` | Mall emoji | `emoji_id`, `emoji_package_id` |
| `image` | Image | `file`, `url`, `summary` |
| `record` | Voice | `file`, `url` |
| `video` | Video | `file`, `url`, `thumb` |
| `at` | @someone | `qq`, `name` |
| `at_all` | @all members | - |
| `rps` | Rock-paper-scissors | `result` |
| `dice` | Dice | `result` |
| `poke` | Poke | `type`, `id` |
| `music` | Music share | `type`, `id`, `url`, `title` |
| `share` | Link share | `url`, `title`, `content`, `image` |
| `reply` | Reply message | `id`, `text`, `qq`, `time` |
| `forward` | Combined forward | `id` |
| `node` | Custom forward node | `id`, `user_id`, `nickname`, `content` |
| `xml` | XML message | `data` |
| `json` | JSON message | `data` |
| `card` | Card message | `data` |
| `file` | File | `file`, `url`, `name`, `size` |

---

## 9. Startup and Testing

### 9.1 Start Gateway

```bash
blockcell gateway
```

### 9.2 Check Connection Status

After startup, the log should show:

```
NapCatQQ channel started (client mode)
Connecting to NapCatQQ WebSocket server: ws://127.0.0.1:3001
NapCatQQ WebSocket connected
```

### 9.3 Verify Configuration

Test connection using tools:

```bash
# Get login info
curl -X POST http://127.0.0.1:18790/api/tool \
  -H "Content-Type: application/json" \
  -d '{"tool": "napcat_get_login_info", "params": {}}'
```

---

## 10. Troubleshooting

| Issue | Possible Cause | Solution |
|-------|----------------|----------|
| WebSocket connection failed | NapCatQQ not started or wrong address | Check NapCatQQ status and wsUrl config |
| Authentication failed | access_token mismatch | Ensure tokens match in BlockCell and NapCatQQ |
| Permission denied | User not in whitelist or insufficient permissions | Check allowFrom, adminPermissions config |
| No message response | channelOwners not configured | Add `"channelOwners": {"napcat": "default"}` |
| Tool call failed | Wrong parameters or insufficient permissions | Check tool parameters and permission config |

---

## 11. Notes

### 11.1 Risk Level Explanation

| Level | Description | Example Tools |
|-------|-------------|---------------|
| ReadOnly | No side effects, query only | `get_group_list`, `get_login_info` |
| LowRisk | Reversible changes, minor impact | `set_group_card`, `send_like` |
| MediumRisk | Significant but reversible changes | `set_group_ban`, `delete_msg` |
| HighRisk | Irreversible or major impact | `set_group_kick`, `delete_friend` |

### 11.2 Security Recommendations

- Always configure `access_token` in production
- Use `allowFrom` and `blockFrom` to restrict user access
- Configure `adminPermissions` for admin tool verification
- Set `requireConfirmation` for high-risk tools

### 11.3 API Limitations

- QQ has rate limits on message sending, BlockCell has built-in rate limiting
- Message length is limited, long messages are handled automatically
- Some features require specific permissions (e.g., group management requires admin rights)