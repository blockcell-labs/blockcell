# Discord Bot Configuration Guide

Blockcell supports interacting with agents through Discord bots. The Discord channel uses WebSocket mode to receive messages, requiring no public webhook URL configuration, making it ideal for local development and deployment.

## 1. Create a Discord Bot

1. Log in and visit [Discord Developer Portal](https://discord.com/developers/applications).
2. Click **New Application** in the top right to create a new application.
3. Enter the application name (Name) and confirm.
4. Select **Bot** from the left menu, then click **Add Bot** or confirm Reset Token.
5. Copy and save your **Token** (e.g., `MTEy...`). This token can only be viewed once, so keep it safe.

## 2. Enable Bot Permissions (Privileged Gateway Intents)

On the same **Bot** page, scroll down to find **Privileged Gateway Intents**.
You must enable the following option for the bot to receive messages:
- **MESSAGE CONTENT INTENT** (allows the bot to read message content)

*(If you need to monitor status or member changes, enable other Intents as needed, but MESSAGE CONTENT INTENT is required for reading messages)*.

## 3. Invite the Bot to Your Server

1. Select **OAuth2** -> **URL Generator** from the left menu.
2. Check `bot` under **Scopes**.
3. In the **Bot Permissions** section that appears below, check the following permissions:
   - Read Messages/View Channels
   - Send Messages
   - Send Messages in Threads (optional, if you use threads)
4. Copy the **Generated URL** at the bottom of the page.

```
// Replace YOUR_CLIENT_ID with yours
https://discord.com/api/oauth2/authorize?client_id=YOUR_CLIENT_ID&permissions=68608&scope=bot
```

5. Open this URL in a browser, select the server you want to invite the bot to, and authorize.

## 4. Get Channel or User ID (for Allowlist and Channel Restrictions)

You need to get the target Channel or User ID.

1. In the Discord client, go to **User Settings** -> **Advanced**.
2. Enable **Developer Mode**.
3. **Get Channel ID**: Right-click the channel you want the bot to respond in and select **Copy Channel ID**.
4. **Get User ID**: Right-click your avatar or username and select **Copy User ID**.

## 5. Configure Blockcell

In Blockcell's configuration file, modify the `discord` section:

```json
{
  "channels": {
    "discord": {
      "enabled": true,
      "botToken": "YOUR_BOT_TOKEN",
      "channels": ["ALLOWED_CHANNEL_ID_1", "ALLOWED_CHANNEL_ID_2"],
      "allowFrom": ["YOUR_USER_ID"]
    }
  }
}
```

### Configuration Options

- `enabled`: Whether to enable the Discord channel (`true` or `false`).
- `botToken`: The bot's Token.
- `channels`: List of channel IDs the bot is allowed to respond in (string array). If left empty `[]`, the bot will respond in all channels it has access to.
- `allowFrom`: List of allowed user IDs (string array). If left empty `[]`, anyone can call the bot in group chats or private messages.

## 6. Interaction Methods

- **Private Chat (DM)**: Right-click the bot's avatar and select send message.
- **Channel Group Chat**: In allowed channels, send messages or `@bot`.

## 7. Notes

- Discord API may be restricted in some regions. If you encounter network issues, configure a global proxy.
- Maximum text message length is 2000 characters. Blockcell will automatically split longer messages.
- The current implementation is based on `tokio-tungstenite` direct connection to Discord Gateway v10 and handles heartbeat keepalive.
