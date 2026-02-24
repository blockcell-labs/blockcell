# WhatsApp Bot Configuration Guide

Blockcell supports interacting with agents through WhatsApp Cloud API. The WhatsApp channel supports **Webhook callback mode** to receive messages.

> **Note**: Since WhatsApp API requires webhook verification and message push via public HTTPS URL, you must have a public domain with a valid SSL certificate, or use intranet penetration tools like `ngrok`/`localtunnel`.

## 1. Apply for Meta (Facebook) Developer Account and App

1. Log in and visit [Meta for Developers](https://developers.facebook.com/).
2. Click **My Apps** in the top right.
3. Click **Create App**.
4. Select **Other** -> **Business**.
5. Fill in the app display name, contact email, and select the associated Business Manager Account. If you don't have one, you can choose not to associate.
6. Click **Create App**.

## 2. Add WhatsApp Product

1. In the app dashboard, scroll down to find the **WhatsApp** product and click **Set up**.
2. In the left menu, select **WhatsApp** -> **API Setup**.
3. The system will assign you a **Test Phone Number** and a corresponding **Phone Number ID**.
4. Copy and save your **Temporary Access Token** (valid for 24 hours) or generate a **Permanent Access Token**.
   - *(For generating permanent access tokens, refer to Meta's official documentation. Usually requires going to Business Settings -> System Users -> Generate New Token).*
5. In the **Send and receive messages** section, add the **real phone number** you want to use for testing message reception and complete SMS verification. Only numbers in this list can receive messages from the test account.

## 3. Configure Webhook

WhatsApp uses Webhook to push new messages.

1. In the left menu, select **WhatsApp** -> **Configuration**.
2. Click **Configure Webhook** or **Edit**.
3. Fill in the **Callback URL**: e.g., `https://your-domain.com/v1/whatsapp/webhook`
4. Fill in the **Verify Token**: This is a custom string you define (e.g., `my_secret_verify_token_123`) used to verify requests are from Meta.
5. Click **Verify and Save**. At this point, your server must be running and able to correctly respond to the `hub.challenge` verification request.
6. After success, in the Webhook fields section under **messages**, click **Subscribe**.

## 4. Get User ID (for Allowlist)

WhatsApp's `sender_id` is usually the international format **phone number** (without the `+` sign), e.g., `8613800138000` or `14155552671`.

## 5. Configure Blockcell

In Blockcell's configuration file, modify the `whatsapp` section:

```json
{
  "channels": {
    "whatsapp": {
      "enabled": true,
      "phoneNumberId": "123456789012345",
      "accessToken": "EAAxxx...(your access token)",
      "verifyToken": "my_secret_verify_token_123",
      "allowFrom": ["8613800138000"]
    }
  }
}
```

### Configuration Options

- `enabled`: Whether to enable the WhatsApp channel (`true` or `false`).
- `phoneNumberId`: The sender test phone number ID obtained in API Setup (note this is not your real number, it's a long numeric ID).
- `accessToken`: Temporary or permanent access token.
- `verifyToken`: The custom string you defined when configuring Webhook.
- `allowFrom`: List of allowed user phone numbers (string array). If left empty `[]`, anyone who can send messages to you can interact with the bot.

## 6. Interaction Methods

- **Private Chat**: Use your real phone number verified in Meta backend to send messages to the test phone number assigned by Meta.

## 7. Notes

- WhatsApp Cloud API has strict limitations for test accounts that haven't completed business verification (e.g., can only send messages to verified numbers, 24-hour customer service window restriction).
- For production use, be sure to bind a real phone number and complete Business verification.
- Maximum text message length is 4096 characters.
- If the message receiving interface doesn't respond with `200 OK` within 10 seconds, WhatsApp may retry and consider your server faulty. Ensure your app responds quickly.
