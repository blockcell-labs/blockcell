# WhatsApp Bridge Configuration Guide

Blockcell’s current WhatsApp integration is **not** Meta Cloud API / webhook mode. It connects through a **WhatsApp bridge WebSocket service**.

That means:

- Blockcell connects to the bridge (default `ws://localhost:3001`)
- the bridge maintains the WhatsApp session, emits the login QR code, and forwards inbound messages
- you do not need to expose a public webhook for Blockcell
- first-time login is usually completed by scanning the QR code shown by the bridge

## 1. Prepare a WhatsApp bridge

You need a running WhatsApp bridge service that exposes a WebSocket endpoint.

Default address:

```text
ws://localhost:3001
```

If your bridge runs elsewhere, set `bridgeUrl` accordingly in config.

## 2. Configure Blockcell

Edit `~/.blockcell/config.json`:

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

### Configuration Options

- `enabled`: whether to enable the WhatsApp channel
- `bridgeUrl`: WebSocket address of the WhatsApp bridge; the default is usually `ws://localhost:3001`
- `allowFrom`: allowlist of sender phone numbers, without the `+` sign, for example `8613800138000`

> If you enable WhatsApp through `blockcell gateway`, you also need `channelOwners.whatsapp`. If the same channel carries multiple accounts, you can also add `channelAccountOwners.whatsapp.<accountId> = "ops"` to route one account to another agent. Otherwise Gateway refuses to start because the enabled external channel has no owner.

## 3. Start and login

Start the bridge first, then start Blockcell gateway:

```bash
blockcell gateway
```

If you want the CLI reminder for the login flow, run:

```bash
blockcell channels login whatsapp
```

The current CLI reminds you of the standard process:

1. make sure the WhatsApp bridge is running
2. the bridge displays a QR code
3. scan it with WhatsApp on your phone

If you manage the bridge manually, you can also use the default hint shown by the CLI:

```bash
cd ~/.blockcell/bridge && npm start
```

## 4. Interaction model

- **Direct chat**: send messages from an allowed phone number to the linked WhatsApp account
- **Allowlist first**: keep `allowFrom` configured so not every number behind the bridge can control your agent

## 5. Notes

- entries in `allowFrom` usually use international format, but **without `+`**
- if `bridgeUrl` is unreachable, Gateway logs will show bridge connection failures or disconnects
- if the bridge is logged in but messages do not arrive, check the bridge health first, then verify `allowFrom`
- for multi-account isolation, you can further use `channels.whatsapp.accounts` plus `defaultAccountId`
