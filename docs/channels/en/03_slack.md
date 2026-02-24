# Slack Bot Configuration Guide

Blockcell supports interacting with agents through Slack bots. By default, the Slack channel uses **Socket Mode** to receive messages, requiring no public webhook URL configuration, making it ideal for local development and deployment. If Event Subscriptions is enabled without Socket Mode, a public URL is required.

## 1. Create a Slack App

1. Log in and visit [Slack API Applications](https://api.slack.com/apps).
2. Click **Create New App** -> **From scratch**.
3. Enter the **App Name** and select the workspace to install it in, then click **Create App**.

## 2. Configure Permissions (Scopes)

1. Select **OAuth & Permissions** from the left menu.
2. Scroll down to **Scopes** -> **Bot Token Scopes**.
3. Add the following required permissions:
   - `chat:write` (send messages)
   - `channels:history` (read public channel messages)
   - `groups:history` (read private channel messages)
   - `im:history` (read direct messages)
   - `mpim:history` (read multi-party direct messages)
   - `app_mentions:read` (read mentions of the bot)
4. Scroll up to **OAuth Tokens for Your Workspace** and click **Install to Workspace** to authorize.
5. Copy and save the **Bot User OAuth Token** (starts with `xoxb-`).

## 3. Enable Socket Mode

1. Select **Socket Mode** from the left menu.
2. Turn on the **Enable Socket Mode** toggle.
3. The system will prompt you to generate an App-Level Token. Enter a name (e.g., `Socket Token`) and click **Generate**.
4. Copy and save the generated **App-Level Token** (starts with `xapp-`).

## 4. Subscribe to Events (Event Subscriptions)

After enabling Socket Mode, you need to tell Slack which events should be pushed to the Socket connection:

1. Select **Event Subscriptions** from the left menu.
2. Turn on **Enable Events**.
3. Under **Subscribe to bot events**, add the following events:
   - `message.channels`
   - `message.groups`
   - `message.im`
   - `message.mpim`
   - `app_mention`
4. Make sure to click **Save Changes** at the bottom of the page, and reinstall the app if prompted at the top.

## 5. Get User ID (for Allowlist and Channel Restrictions)

You need to get the target Channel or User ID.

1. **Get User ID**: In the Slack client, click on a user's avatar, select **View profile**, then click **More** (three dots icon) and select **Copy member ID** (e.g., `U12345678`).
2. **Get Channel ID**: Right-click the channel name and select **Copy channel ID** (e.g., `C12345678`).

## 6. Configure Blockcell

In Blockcell's configuration file, modify the `slack` section:

```json
{
  "channels": {
    "slack": {
      "enabled": true,
      "botToken": "YOUR_BOT_TOKEN_xoxb",
      "appToken": "YOUR_APP_TOKEN_xapp",
      "channels": ["YOUR_CHANNEL_ID_1"],
      "allowFrom": ["YOUR_USER_ID_1"]
    }
  }
}
```

### Configuration Options

- `enabled`: Whether to enable the Slack channel (`true` or `false`).
- `botToken`: The `xoxb-` Token obtained from OAuth & Permissions.
- `appToken`: The `xapp-` Token obtained from Socket Mode.
- `channels`: List of channel IDs the bot is allowed to respond in (string array). If left empty `[]`, the bot will respond in all joined channels.
- `allowFrom`: List of allowed user IDs (string array). If left empty `[]`, anyone can call the bot in group chats or private messages.

## 7. Interaction Methods

- **Private Chat (DM)**: Find your bot in the Apps section and send messages directly.
- **Channel Group Chat**: Send messages in allowed channels and `@bot` (Note: In Slack's mechanism, if you subscribe to `message.channels`, the bot can receive all messages even without `@`. Blockcell will respond to messages where it's mentioned and direct messages in allowed channels by default).

## 8. Notes

- Maximum text message length is 4000 characters. Blockcell will automatically split longer messages into threaded replies.
- Blockcell automatically parses blocks and elements in Slack message bodies, merging text and ignoring attachments (full multimodal support is not yet implemented).
