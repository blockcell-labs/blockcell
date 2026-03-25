//! Outbound message handling for NapCatQQ.
//!
//! This module provides functions for sending messages via NapCatQQ.
//! Messages are sent via WebSocket (ws-client or ws-server mode).

use blockcell_core::{Config, Result};

use super::message::{build_message, OneBotMessage};
use super::websocket::send_via_ws;
use super::types::ApiRequest;

/// Send a message via NapCatQQ.
///
/// Uses WebSocket for sending.
///
/// # Arguments
///
/// * `config` - The configuration containing NapCatQQ settings
/// * `chat_id` - The target chat ID (format: "group:123456" or "user:123456")
/// * `text` - The text message to send
pub async fn send_message(_config: &Config, chat_id: &str, text: &str) -> Result<()> {
    // Parse chat_id to determine target type and ID
    let (target_type, target_id) = parse_chat_id(chat_id);

    let message = OneBotMessage::from_text(text);
    let message_json = message.to_json();

    send_via_ws_internal(target_type, &target_id, &message_json).await
}

/// Send a message with media via NapCatQQ.
///
/// Uses WebSocket for sending.
///
/// # Arguments
///
/// * `config` - The configuration containing NapCatQQ settings
/// * `chat_id` - The target chat ID (format: "group:123456" or "user:123456")
/// * `text` - The text message to send
/// * `media` - List of media URLs or file paths to attach
pub async fn send_media_message(
    _config: &Config,
    chat_id: &str,
    text: &str,
    media: &[String],
) -> Result<()> {
    // Parse chat_id to determine target type and ID
    let (target_type, target_id) = parse_chat_id(chat_id);

    let message = build_message(text, media);
    let message_json = message.to_json();

    send_via_ws_internal(target_type, &target_id, &message_json).await
}

/// Parse chat_id to determine target type and ID.
fn parse_chat_id(chat_id: &str) -> (&'static str, String) {
    if let Some(group_id) = chat_id.strip_prefix("group:") {
        ("group", group_id.to_string())
    } else if let Some(user_id) = chat_id.strip_prefix("user:") {
        ("private", user_id.to_string())
    } else {
        // Default to private message
        ("private", chat_id.to_string())
    }
}

/// Send message via WebSocket (internal helper).
async fn send_via_ws_internal(
    target_type: &str,
    target_id: &str,
    message_json: &serde_json::Value,
) -> Result<()> {
    let request = match target_type {
        "group" => ApiRequest::send_group_msg(target_id, message_json, None, None),
        "private" => ApiRequest::send_private_msg(target_id, message_json, None, None),
        _ => return Ok(()),
    };

    send_via_ws(request)
        .await
        .map_err(blockcell_core::Error::Channel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_message_text_only() {
        let msg = build_message("hello", &[]);
        assert_eq!(msg.to_plain_text(), "hello");
    }

    #[test]
    fn test_parse_chat_id() {
        assert_eq!(parse_chat_id("group:123456"), ("group", "123456".to_string()));
        assert_eq!(parse_chat_id("user:789012"), ("private", "789012".to_string()));
        assert_eq!(parse_chat_id("plain_id"), ("private", "plain_id".to_string()));
    }
}