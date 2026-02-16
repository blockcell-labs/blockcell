use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::info;

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for multi-channel notifications beyond the built-in message channels.
///
/// Supports:
/// - SMS via Twilio API
/// - Push notifications via Pushover / Bark / ntfy
/// - Webhook (generic POST to any URL)
/// - macOS native notification (osascript)
pub struct NotificationTool;

#[async_trait]
impl Tool for NotificationTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "notification",
            description: "Send notifications via multiple channels. Channels: 'sms' (Twilio), 'push' (Pushover/Bark/ntfy), 'webhook' (generic POST), 'desktop' (macOS native notification), 'telegram' (Telegram Bot API).",
            parameters: json!({
                "type": "object",
                "properties": {
                    "channel": {
                        "type": "string",
                        "enum": ["sms", "push", "webhook", "desktop", "telegram"],
                        "description": "Notification channel"
                    },
                    "message": {
                        "type": "string",
                        "description": "Notification message body"
                    },
                    "title": {
                        "type": "string",
                        "description": "(push/webhook/desktop) Notification title"
                    },
                    "to": {
                        "type": "string",
                        "description": "(sms) Recipient phone number in E.164 format, e.g. '+1234567890'"
                    },
                    "from": {
                        "type": "string",
                        "description": "(sms) Sender phone number (Twilio number). Falls back to config/env TWILIO_FROM_NUMBER."
                    },
                    "provider": {
                        "type": "string",
                        "enum": ["twilio", "pushover", "bark", "ntfy"],
                        "description": "(sms/push) Service provider. SMS default: twilio. Push default: auto-detect from config."
                    },
                    "url": {
                        "type": "string",
                        "description": "(webhook) Target URL for webhook POST. (push/ntfy) ntfy server URL."
                    },
                    "priority": {
                        "type": "integer",
                        "description": "(push) Priority level. Pushover: -2 to 2 (0=normal, 1=high, 2=emergency). ntfy: 1-5."
                    },
                    "sound": {
                        "type": "string",
                        "description": "(push/desktop) Notification sound. Pushover: pushover/bike/bugle/etc. Desktop: default/Basso/Blow/etc."
                    },
                    "webhook_headers": {
                        "type": "object",
                        "description": "(webhook) Custom headers for webhook request"
                    },
                    "webhook_body": {
                        "type": "object",
                        "description": "(webhook) Custom JSON body. If not set, sends {title, message, timestamp}."
                    },
                    "topic": {
                        "type": "string",
                        "description": "(ntfy) Topic name for ntfy notifications"
                    },
                    "device_token": {
                        "type": "string",
                        "description": "(bark) Bark device token/key"
                    },
                    "chat_id": {
                        "type": "string",
                        "description": "(telegram) Telegram chat ID (user, group, or channel). Falls back to config/env TELEGRAM_CHAT_ID."
                    },
                    "parse_mode": {
                        "type": "string",
                        "enum": ["Markdown", "MarkdownV2", "HTML"],
                        "description": "(telegram) Message parse mode. Default: 'Markdown'."
                    }
                },
                "required": ["channel", "message"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
        if !["sms", "push", "webhook", "desktop", "telegram"].contains(&channel) {
            return Err(Error::Tool("channel must be 'sms', 'push', 'webhook', 'desktop', or 'telegram'".into()));
        }
        if params.get("message").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
            return Err(Error::Tool("'message' is required".into()));
        }
        if channel == "sms" {
            if params.get("to").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                return Err(Error::Tool("'to' phone number is required for SMS".into()));
            }
        }
        if channel == "webhook" {
            if params.get("url").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                return Err(Error::Tool("'url' is required for webhook".into()));
            }
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");

        let result = match channel {
            "sms" => send_sms(&ctx, &params).await,
            "push" => send_push(&ctx, &params).await,
            "webhook" => send_webhook(&params).await,
            "desktop" => send_desktop(&params).await,
            "telegram" => send_telegram(&ctx, &params).await,
            _ => Err(Error::Tool(format!("Unknown channel: {}", channel))),
        };

        match result {
            Ok(data) => {
                info!(channel = %channel, "Notification sent");
                Ok(data)
            }
            Err(e) => Err(e),
        }
    }
}

// ═══════════════════════════════════════════════════════════
// SMS via Twilio
// ═══════════════════════════════════════════════════════════

async fn send_sms(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let to = params.get("to").and_then(|v| v.as_str()).unwrap_or("");
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");

    let (account_sid, auth_token) = resolve_twilio_credentials(ctx, params)?;
    let from = params.get("from").and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| std::env::var("TWILIO_FROM_NUMBER").ok())
        .ok_or_else(|| Error::Tool("Twilio 'from' number not found. Set via param or TWILIO_FROM_NUMBER env.".into()))?;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("https://api.twilio.com/2010-04-01/Accounts/{}/Messages.json", account_sid))
        .basic_auth(&account_sid, Some(&auth_token))
        .form(&[
            ("To", to),
            ("From", &from),
            ("Body", message),
        ])
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Twilio API failed: {}", e)))?;

    let status = response.status();
    let data: Value = response.json().await
        .map_err(|e| Error::Tool(format!("Failed to parse Twilio response: {}", e)))?;

    if !status.is_success() {
        let err_msg = data["message"].as_str().unwrap_or("Unknown error");
        return Err(Error::Tool(format!("Twilio error {}: {}", status, err_msg)));
    }

    Ok(json!({
        "status": "sent",
        "channel": "sms",
        "provider": "twilio",
        "sid": data["sid"],
        "to": to,
        "from": from
    }))
}

fn resolve_twilio_credentials(ctx: &ToolContext, _params: &Value) -> Result<(String, String)> {
    // Config: api_key stores auth_token, api_base stores account_sid
    if let Some(p) = ctx.config.providers.get("twilio") {
        let sid = p.api_base.as_deref().unwrap_or("");
        let token = &p.api_key;
        if !sid.is_empty() && !token.is_empty() {
            return Ok((sid.to_string(), token.clone()));
        }
    }
    // Env
    let sid = std::env::var("TWILIO_ACCOUNT_SID").unwrap_or_default();
    let token = std::env::var("TWILIO_AUTH_TOKEN").unwrap_or_default();
    if !sid.is_empty() && !token.is_empty() {
        return Ok((sid, token));
    }
    Err(Error::Tool("Twilio credentials not found. Set TWILIO_ACCOUNT_SID + TWILIO_AUTH_TOKEN or config providers.twilio.".into()))
}

// ═══════════════════════════════════════════════════════════
// Push notifications (Pushover / Bark / ntfy)
// ═══════════════════════════════════════════════════════════

async fn send_push(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let provider = params.get("provider").and_then(|v| v.as_str()).unwrap_or("auto");

    match provider {
        "pushover" => send_pushover(ctx, params).await,
        "bark" => send_bark(ctx, params).await,
        "ntfy" => send_ntfy(params).await,
        "auto" => {
            // Try providers in order
            if has_config(ctx, "pushover") || std::env::var("PUSHOVER_TOKEN").is_ok() {
                send_pushover(ctx, params).await
            } else if has_config(ctx, "bark") || std::env::var("BARK_KEY").is_ok() {
                send_bark(ctx, params).await
            } else if params.get("topic").and_then(|v| v.as_str()).is_some() {
                send_ntfy(params).await
            } else {
                Err(Error::Tool("No push provider configured. Set pushover/bark credentials or provide ntfy topic.".into()))
            }
        }
        _ => Err(Error::Tool(format!("Unknown push provider: {}", provider))),
    }
}

async fn send_pushover(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("blockcell");
    let priority = params.get("priority").and_then(|v| v.as_i64()).unwrap_or(0);
    let sound = params.get("sound").and_then(|v| v.as_str());

    let (token, user_key) = resolve_pushover_credentials(ctx)?;

    let mut form = vec![
        ("token", token.as_str()),
        ("user", user_key.as_str()),
        ("message", message),
        ("title", title),
    ];

    let priority_str = priority.to_string();
    form.push(("priority", &priority_str));

    if let Some(s) = sound {
        form.push(("sound", s));
    }

    // Emergency priority requires retry and expire
    let retry_str;
    let expire_str;
    if priority == 2 {
        retry_str = "60".to_string();
        expire_str = "3600".to_string();
        form.push(("retry", &retry_str));
        form.push(("expire", &expire_str));
    }

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.pushover.net/1/messages.json")
        .form(&form)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Pushover API failed: {}", e)))?;

    let status = response.status();
    let data: Value = response.json().await
        .map_err(|e| Error::Tool(format!("Failed to parse Pushover response: {}", e)))?;

    if !status.is_success() {
        return Err(Error::Tool(format!("Pushover error: {:?}", data)));
    }

    Ok(json!({
        "status": "sent",
        "channel": "push",
        "provider": "pushover",
        "request": data["request"]
    }))
}

fn resolve_pushover_credentials(ctx: &ToolContext) -> Result<(String, String)> {
    // Config: api_key stores app token, api_base stores user key
    if let Some(p) = ctx.config.providers.get("pushover") {
        let token = &p.api_key;
        let user = p.api_base.as_deref().unwrap_or("");
        if !token.is_empty() && !user.is_empty() {
            return Ok((token.clone(), user.to_string()));
        }
    }
    let token = std::env::var("PUSHOVER_TOKEN").unwrap_or_default();
    let user = std::env::var("PUSHOVER_USER").unwrap_or_default();
    if !token.is_empty() && !user.is_empty() {
        return Ok((token, user));
    }
    Err(Error::Tool("Pushover credentials not found. Set PUSHOVER_TOKEN + PUSHOVER_USER or config providers.pushover.".into()))
}

async fn send_bark(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("blockcell");
    let sound = params.get("sound").and_then(|v| v.as_str());

    let device_key = params.get("device_token").and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| ctx.config.providers.get("bark").map(|p| p.api_key.clone()).filter(|k| !k.is_empty()))
        .or_else(|| std::env::var("BARK_KEY").ok())
        .ok_or_else(|| Error::Tool("Bark device key not found. Set device_token param, config providers.bark.api_key, or BARK_KEY env.".into()))?;

    let bark_server = ctx.config.providers.get("bark")
        .and_then(|p| p.api_base.as_deref())
        .unwrap_or("https://api.day.app");

    let mut body = json!({
        "title": title,
        "body": message,
        "device_key": device_key
    });
    if let Some(s) = sound {
        body["sound"] = json!(s);
    }

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/push", bark_server))
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Bark API failed: {}", e)))?;

    let status = response.status();
    let data: Value = response.json().await
        .map_err(|e| Error::Tool(format!("Failed to parse Bark response: {}", e)))?;

    if !status.is_success() {
        return Err(Error::Tool(format!("Bark error: {:?}", data)));
    }

    Ok(json!({
        "status": "sent",
        "channel": "push",
        "provider": "bark"
    }))
}

async fn send_ntfy(params: &Value) -> Result<Value> {
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let title = params.get("title").and_then(|v| v.as_str());
    let topic = params.get("topic").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("'topic' is required for ntfy".into()))?;
    let priority = params.get("priority").and_then(|v| v.as_i64());
    let server = params.get("url").and_then(|v| v.as_str()).unwrap_or("https://ntfy.sh");

    let client = reqwest::Client::new();
    let mut req = client
        .post(format!("{}/{}", server, topic))
        .body(message.to_string());

    if let Some(t) = title {
        req = req.header("Title", t);
    }
    if let Some(p) = priority {
        req = req.header("Priority", p.to_string());
    }

    let response = req.send().await
        .map_err(|e| Error::Tool(format!("ntfy failed: {}", e)))?;

    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Tool(format!("ntfy error: {}", text)));
    }

    Ok(json!({
        "status": "sent",
        "channel": "push",
        "provider": "ntfy",
        "topic": topic
    }))
}

// ═══════════════════════════════════════════════════════════
// Webhook (generic POST)
// ═══════════════════════════════════════════════════════════

async fn send_webhook(params: &Value) -> Result<Value> {
    let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("blockcell");

    let body = if let Some(custom_body) = params.get("webhook_body") {
        custom_body.clone()
    } else {
        json!({
            "title": title,
            "message": message,
            "timestamp": chrono::Utc::now().to_rfc3339()
        })
    };

    let client = reqwest::Client::new();
    let mut req = client.post(url).json(&body);

    if let Some(headers) = params.get("webhook_headers").and_then(|v| v.as_object()) {
        for (key, value) in headers {
            if let Some(v) = value.as_str() {
                req = req.header(key.as_str(), v);
            }
        }
    }

    let response = req.send().await
        .map_err(|e| Error::Tool(format!("Webhook failed: {}", e)))?;

    let status = response.status();
    let response_text = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return Err(Error::Tool(format!("Webhook error {}: {}", status, response_text)));
    }

    Ok(json!({
        "status": "sent",
        "channel": "webhook",
        "url": url,
        "http_status": status.as_u16()
    }))
}

// ═══════════════════════════════════════════════════════════
// macOS Desktop notification
// ═══════════════════════════════════════════════════════════

async fn send_desktop(params: &Value) -> Result<Value> {
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("blockcell");
    let sound = params.get("sound").and_then(|v| v.as_str()).unwrap_or("default");

    let escaped_msg = message.replace('\\', "\\\\").replace('"', "\\\"");
    let escaped_title = title.replace('\\', "\\\\").replace('"', "\\\"");

    let script = format!(
        r#"display notification "{}" with title "{}" sound name "{}""#,
        escaped_msg, escaped_title, sound
    );

    let output = tokio::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .await
        .map_err(|e| Error::Tool(format!("Desktop notification failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!("osascript error: {}", stderr)));
    }

    Ok(json!({
        "status": "sent",
        "channel": "desktop",
        "title": title
    }))
}

// ═══════════════════════════════════════════════════════════
// Telegram Bot API
// ═══════════════════════════════════════════════════════════

async fn send_telegram(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let message = params.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let title = params.get("title").and_then(|v| v.as_str());
    let parse_mode = params.get("parse_mode").and_then(|v| v.as_str()).unwrap_or("Markdown");

    let bot_token = resolve_telegram_bot_token(ctx)?;
    let chat_id = params.get("chat_id").and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| ctx.config.providers.get("telegram").and_then(|p| p.api_base.as_ref()).cloned().filter(|s| !s.is_empty()))
        .or_else(|| std::env::var("TELEGRAM_CHAT_ID").ok())
        .ok_or_else(|| Error::Tool("Telegram chat_id not found. Set chat_id param, config providers.telegram.api_base, or TELEGRAM_CHAT_ID env.".into()))?;

    // Build message text with optional title
    let text = if let Some(t) = title {
        format!("*{}*\n\n{}", t, message)
    } else {
        message.to_string()
    };

    let body = json!({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": parse_mode
    });

    let client = reqwest::Client::new();
    let response = client
        .post(format!("https://api.telegram.org/bot{}/sendMessage", bot_token))
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Telegram API failed: {}", e)))?;

    let status = response.status();
    let data: Value = response.json().await
        .map_err(|e| Error::Tool(format!("Failed to parse Telegram response: {}", e)))?;

    if !status.is_success() {
        let err_desc = data["description"].as_str().unwrap_or("Unknown error");
        return Err(Error::Tool(format!("Telegram error {}: {}", status, err_desc)));
    }

    Ok(json!({
        "status": "sent",
        "channel": "telegram",
        "chat_id": chat_id,
        "message_id": data["result"]["message_id"]
    }))
}

fn resolve_telegram_bot_token(ctx: &ToolContext) -> Result<String> {
    // Config: providers.telegram.api_key stores bot token
    if let Some(p) = ctx.config.providers.get("telegram") {
        if !p.api_key.is_empty() {
            return Ok(p.api_key.clone());
        }
    }
    // Env
    if let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN") {
        if !token.is_empty() {
            return Ok(token);
        }
    }
    Err(Error::Tool("Telegram bot token not found. Set config providers.telegram.api_key or TELEGRAM_BOT_TOKEN env.".into()))
}

fn has_config(ctx: &ToolContext, provider: &str) -> bool {
    ctx.config.providers.get(provider)
        .map(|p| !p.api_key.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_notification_schema() {
        let tool = NotificationTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "notification");
    }

    #[test]
    fn test_validate_sms() {
        let tool = NotificationTool;
        assert!(tool.validate(&json!({"channel": "sms", "message": "hello", "to": "+1234567890"})).is_ok());
        assert!(tool.validate(&json!({"channel": "sms", "message": "hello"})).is_err()); // missing to
    }

    #[test]
    fn test_validate_push() {
        let tool = NotificationTool;
        assert!(tool.validate(&json!({"channel": "push", "message": "hello"})).is_ok());
    }

    #[test]
    fn test_validate_webhook() {
        let tool = NotificationTool;
        assert!(tool.validate(&json!({"channel": "webhook", "message": "hello", "url": "https://example.com/hook"})).is_ok());
        assert!(tool.validate(&json!({"channel": "webhook", "message": "hello"})).is_err()); // missing url
    }

    #[test]
    fn test_validate_desktop() {
        let tool = NotificationTool;
        assert!(tool.validate(&json!({"channel": "desktop", "message": "hello"})).is_ok());
    }

    #[test]
    fn test_validate_invalid_channel() {
        let tool = NotificationTool;
        assert!(tool.validate(&json!({"channel": "fax", "message": "hello"})).is_err());
    }

    #[test]
    fn test_validate_telegram() {
        let tool = NotificationTool;
        assert!(tool.validate(&json!({"channel": "telegram", "message": "hello"})).is_ok());
    }

    #[test]
    fn test_validate_empty_message() {
        let tool = NotificationTool;
        assert!(tool.validate(&json!({"channel": "desktop", "message": ""})).is_err());
    }
}
