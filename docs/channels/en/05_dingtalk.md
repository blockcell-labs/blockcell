# DingTalk Bot Configuration Guide

Blockcell supports interacting with agents through DingTalk enterprise internal bots. The DingTalk channel uses **Stream SDK (Long Connection WebSocket)** mode to receive messages, requiring no public webhook URL configuration, making it ideal for local development, intranet deployment, and debugging.

## 1. Create a DingTalk Enterprise Internal App

1. Log in and visit [DingTalk Developer Console](https://open-dev.dingtalk.com/).
2. In **App Development** -> **Enterprise Internal Development**, click **Create App**.
3. Fill in the app name (e.g., Blockcell Bot) and description, select an app icon, and click **Save**.
4. After successful creation, on the **Basic Info** page, copy and save the **AppKey** and **AppSecret**.

## 2. Add Bot Capability

1. Select **App Features** -> **Add App Capability** from the left menu.
2. Find **Bot** and click **Add**.
3. Configure bot information (name, description, avatar, etc.).
4. **Message Receiving Mode**: Must select **Stream Mode** (this is key to implementing intranet penetration and webhook-free configuration).
5. Click **Publish** (or Save).

## 3. Apply for Interface Permissions

The bot needs to apply for appropriate interface permissions to read and send messages.

1. Select **Development Config** -> **Permission Management** from the left menu.
2. In the permission search box, search for and apply for the following permissions (or ensure they are enabled):
   - `Enterprise internal bot send messages`
   - (If you need to get personnel information, apply for address book read-only permissions as needed)

## 4. Get Robot Code

In some newer DingTalk APIs (e.g., `v1.0/robot/oToMessages/batchSend` for private chat sending), you need to provide `robotCode`.
- `robotCode` is usually the same as the app's `AppKey`.
- If your app has a separate `robotCode` configured, check it on the bot details page.

## 5. Version Release

1. Select **App Release** -> **Version Management & Release** from the left menu.
2. Click **Create New Version**.
3. Fill in version information, select visibility range (e.g., visible to all or specific personnel), and click **Publish**.
4. Only personnel within the visibility range can search for and use this bot in DingTalk.

## 6. Get User ID (for Allowlist)

DingTalk's `sender_id` is usually the enterprise's `staffId` (employee ID).
If you don't know your staffId, you can leave `allowFrom` empty first, then observe the sender ID in the backend logs, or contact the enterprise administrator to check in the address book.

## 7. Configure Blockcell

In Blockcell's configuration file, modify the `dingtalk` section:

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

### Configuration Options

- `enabled`: Whether to enable the DingTalk channel (`true` or `false`).
- `appKey`: AppKey obtained from Basic Info.
- `appSecret`: AppSecret obtained from Basic Info.
- `robotCode`: Unique identifier for the bot (usually the same as appKey).
- `allowFrom`: List of allowed user `staffId`s (string array). If left empty `[]`, anyone within the enterprise's visibility range can interact with the bot.

## 8. Interaction Methods

- **Private Chat**: Search for your bot name in DingTalk's search box, click to enter the conversation and send messages directly.
- **Group Chat**: Open group settings -> **Group Bots** -> **Add Bot** -> Search for your enterprise internal bot and add it. Send `@bot your message` in the group.

## 9. Notes

- DingTalk single text message maximum length is 4096 characters. Blockcell will automatically split and send longer messages in multiple parts.
- Blockcell has implemented exponential backoff reconnection mechanism. When DingTalk Stream server disconnects, it will automatically recover.
- Group chat and private chat use different DingTalk API interfaces (group chat uses old `chat/send`, private chat uses new `v1.0/robot/oToMessages/batchSend`). Ensure your app has the relevant sending permissions.
