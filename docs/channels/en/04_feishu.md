# Feishu Bot Configuration Guide

Blockcell supports interacting with agents through Feishu (or Lark) bots. The Feishu channel uses **Long Connection (WebSocket)** mode to receive messages, requiring no public webhook URL configuration, making it ideal for local development, intranet deployment, and debugging.

## Configuration Order Overview

> ⚠️ **Important: Please follow these steps strictly.**
>
> Feishu's long connection has a "chicken-and-egg" problem:
> - The Feishu Open Platform backend requires the **app to successfully establish a long connection first** before saving the "use long connection to receive events" configuration
> - But the app can only successfully establish a long connection after being properly configured and published in the backend
>
> **Correct order:** Create app → Enable bot capability → Apply for permissions → Add event subscriptions (don't change receiving method yet) → Publish app → **Start Blockcell gateway** → Go back to backend and switch to long connection reception → Save

## 1. Create a Feishu Custom App

1. Log in and visit [Feishu Open Platform - Developer Console](https://open.feishu.cn/app).
2. Click **Create Enterprise Custom App**.
3. Enter the **App Name** (e.g., Blockcell Bot) and description, then click **Create**.
4. After successful creation, in the left menu **Credentials & Basic Info**, copy and save your **App ID** and **App Secret**.

## 2. Enable Bot Capability

> ⚠️ **This is a prerequisite for long connection to work properly and cannot be skipped.**

1. Select **Add App Capability** from the left menu.
2. Find **Bot** and click **Add**.
3. Confirm that the bot capability appears in the added capabilities list.

## 3. Apply for App Permissions

The bot needs appropriate permissions to read and send messages.

1. Select **Permission Management** from the left menu.
2. Switch to the **API Permissions** tab.
3. Search for and apply for the following permissions (at minimum):
   - `im:message` — Get single chat and group messages
   - `im:message.group_at_msg` — Get messages where the bot is @mentioned in groups
   - `im:message.p2p_msg` — Get direct messages sent to the bot
   - `im:message:send_as_bot` — Send messages as the app
   - `im:resource` — Get resource files in messages (images, files, etc.)

*Note: After modifying permissions, you must publish a new version for them to take effect.*

## 4. Add Event Subscriptions

1. Select **Event Subscriptions** from the left menu.
2. **Keep the default HTTP callback method for now** (don't switch to long connection yet, as the app hasn't started).
3. Click **Add Event**, search for and add the following events:
   - `im.message.receive_v1` — Receive messages (required)
4. Click **Save**.

## 5. Publish the App

1. Select **App Release** → **Version Management & Release** from the left menu.
2. Click **Create Version**.
3. Enter the version number (e.g., `1.0.0`) and update notes.
4. Click **Save**, then click **Apply for Release**.
5. After the enterprise administrator (or yourself, if you're an admin) approves in the Feishu admin backend, the app can be used within the enterprise.

## 6. Configure Blockcell

In `~/.blockcell/config.json`, modify the `feishu` section:

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

### Configuration Options

| Field | Description |
|------|------|
| `enabled` | Whether to enable the Feishu channel (`true` or `false`) |
| `appId` | App ID from Credentials & Basic Info |
| `appSecret` | App Secret from Credentials & Basic Info |
| `allowFrom` | List of allowed user `open_id`s. Leave empty `[]` to allow all enterprise users |

## 7. Start Blockcell and Switch to Long Connection

1. Start Blockcell gateway:
   ```bash
   blockcell gateway
   ```
2. Check the logs to confirm the following content appears (indicating successful long connection):
   ```
   INFO blockcell_channels::feishu: Feishu tenant_access_token refreshed (cached 2h)
   INFO blockcell_channels::feishu: Connecting to Feishu WebSocket url=wss://...
   INFO blockcell_channels::feishu: Connected to Feishu WebSocket
   ```
3. **After the app successfully connects**, go back to Feishu Open Platform backend → **Event Subscriptions**.
4. Switch the receiving method to **Use long connection to receive events** and click **Save**.
   - The backend will no longer report "app has not established long connection" because the app is now online.

## 8. Get User ID (for Allowlist)

Feishu's `sender_id` uses the `open_id` format (e.g., `ou_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx`).

How to get it:
- **Method 1**: Temporarily set `allowFrom` to `[]`, start the app, send a message to the bot, and the `sender_id` will be printed in Blockcell logs. Copy it and add to the allowlist.
- **Method 2**: In Feishu Open Platform → **API Debugging Tool** → Call the `Get User Info` interface to query.

## 9. Interaction Methods

- **Private Chat**: Search for the bot name in Feishu's search box, click to enter the conversation and send messages directly.
- **Group Chat**: Add the bot to a group and send `@bot your message` in the group.

## 10. Common Error Troubleshooting

| Error Log | Cause | Solution |
|----------|------|----------|
| `Feishu endpoint error code=1161001` | App has not enabled bot capability | Open Platform → Add App Capability → Bot → Add, then republish |
| `Feishu endpoint error code=1161002` | App not published | Create version and apply for release, effective after admin approval |
| `Feishu endpoint error code=99991663` | Incorrect App ID or App Secret | Check `appId` and `appSecret` in config file |
| `Feishu endpoint error code=10003` | Missing permissions | Apply for required permissions in Permission Management, republish version |
| `Failed to parse endpoint response` | Response body not JSON (network or config issue) | Check network connectivity, raw response body will be printed in logs for diagnosis |
| Backend reports "app has not established long connection" | App not started or connection failed | Start `blockcell gateway` first to confirm successful connection, then save config in backend |

## 11. Notes

- Long connection (WebSocket) will automatically reconnect when network is unstable (retry after 5 seconds), ensuring high service availability.
- Feishu rich text message maximum length is approximately 30,000 characters. Blockcell will automatically handle truncation of overly long replies.
- Images, files, audio, and video in messages will be automatically downloaded to the local `workspace/media/` directory, supporting multimodal interaction.
- Long connection mode **does not require** a public IP or domain name, suitable for local and intranet deployment.
- `tenant_access_token` is valid for 2 hours. Blockcell will automatically refresh it 5 minutes before expiration, no manual intervention needed.
