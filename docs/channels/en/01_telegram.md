# Telegram Bot Configuration Guide

Blockcell supports interacting with agents through Telegram bots. The Telegram channel uses Long Polling mode to receive messages, requiring no public webhook URL configuration, making it ideal for local development and deployment.

## 1. Create a Telegram Bot

1. Search for and add the official bot [BotFather](https://t.me/BotFather) in Telegram.
2. Send the `/newbot` command to create a new bot.
3. Follow the prompts to enter the bot's **Name** (display name) and **Username** (must end with `bot`).
4. Upon successful creation, BotFather will return an **API Token** (e.g., `123456789:ABCdefGhIJKlmNoPQRsTUVwxyZ`).

## 2. Get User ID (for Allowlist)

For security reasons, it's recommended to configure an allowlist (`allowFrom`) to allow only specific users to interact with the bot.

1. Search for and add [userinfo_bot](https://t.me/userinfo_bot) or similar bots in Telegram.
2. Send any message, and it will return your Telegram ID (a numeric string, e.g., `12345678`).

## 3. Configure Blockcell

In Blockcell's configuration file (e.g., `~/.blockcell/config.json` or `config.json` in the project directory), find the `channels` configuration block and modify the `telegram` section:

```json
{
  "channels": {
    "telegram": {
      "enabled": true,
      "botToken": "YOUR_BOT_TOKEN",
      "allowFrom": ["YOUR_USER_ID", "OTHER_USER_ID"]
    }
  }
}
```

### Configuration Options

- `enabled`: Whether to enable the Telegram channel (`true` or `false`).
- `botToken`: The API Token obtained from BotFather.
- `allowFrom`: List of allowed user IDs (string array). If left empty `[]`, anyone can call the bot in group chats or private messages. It's recommended to configure specific IDs in production environments.

## 4. Interaction Methods

- **Private Chat**: Search for your bot's Username directly and send messages.
- **Group Chat**:
  1. Send `/setjoingroups` in BotFather to allow the bot to be added to groups.
  2. Send `/setprivacy` and set to `Disable` (if you want the bot to read all messages in the group), or keep it `Enable` (only respond to `@bot` messages).
  3. Add the bot to the group and interact with it via `@bot`.

## 5. Notes

- Telegram may require system proxy configuration (e.g., `HTTP_PROXY` / `HTTPS_PROXY`) in regions like mainland China to connect to its API servers.
- Maximum text message length is 4096 characters. Blockcell will automatically split longer messages.
