//! OneBot 11 event types.
//!
//! This module defines the event types received from NapCatQQ via WebSocket,
//! including message events, notice events, and meta events.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::message::OneBotMessage;

/// Helper to deserialize numeric or string ID to String.
fn deserialize_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct IdVisitor;

    impl<'de> Visitor<'de> for IdVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string or a number")
        }

        fn visit_str<E>(self, value: &str) -> Result<String, E>
        where
            E: de::Error,
        {
            Ok(value.to_string())
        }

        fn visit_i64<E>(self, value: i64) -> Result<String, E>
        where
            E: de::Error,
        {
            Ok(value.to_string())
        }

        fn visit_u64<E>(self, value: u64) -> Result<String, E>
        where
            E: de::Error,
        {
            Ok(value.to_string())
        }
    }

    deserializer.deserialize_any(IdVisitor)
}

/// Helper to deserialize optional numeric or string ID to Option<String>.
/// Handles both null/missing and actual values (number or string).
fn deserialize_id_opt<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct OptIdVisitor;

    impl<'de> Visitor<'de> for OptIdVisitor {
        type Value = Option<String>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string, a number, or null")
        }

        fn visit_none<E>(self) -> Result<Option<String>, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Option<String>, E>
        where
            E: de::Error,
        {
            Ok(None)
        }

        fn visit_str<E>(self, value: &str) -> Result<Option<String>, E>
        where
            E: de::Error,
        {
            Ok(Some(value.to_string()))
        }

        fn visit_i64<E>(self, value: i64) -> Result<Option<String>, E>
        where
            E: de::Error,
        {
            Ok(Some(value.to_string()))
        }

        fn visit_u64<E>(self, value: u64) -> Result<Option<String>, E>
        where
            E: de::Error,
        {
            Ok(Some(value.to_string()))
        }
    }

    deserializer.deserialize_any(OptIdVisitor)
}

/// OneBot event structure (generic).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OneBotEvent {
    /// Event timestamp (Unix seconds).
    pub time: i64,
    /// Bot QQ number that received the event.
    pub self_id: i64,
    /// Event type: "message", "notice", "request", "meta_event".
    pub post_type: String,
    /// Additional event data (depends on post_type).
    #[serde(flatten)]
    pub data: Value,
}

/// Message event structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEvent {
    /// Event timestamp (Unix seconds).
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "message".
    pub post_type: String,
    /// Message type: "private" or "group".
    pub message_type: String,
    /// Sub type.
    pub sub_type: String,
    /// Sender QQ number (can be number or string in JSON).
    #[serde(deserialize_with = "deserialize_id")]
    pub user_id: String,
    /// Target QQ number (for private messages, can be number or string).
    #[serde(default, deserialize_with = "deserialize_id_opt")]
    pub target_id: Option<String>,
    /// Group ID (for group messages, can be number or string).
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "deserialize_id_opt")]
    pub group_id: Option<String>,
    /// Message ID.
    pub message_id: i64,
    /// Message sequence number.
    #[serde(default)]
    pub message_seq: i64,
    /// Real message ID.
    #[serde(default)]
    pub real_id: i64,
    /// Message content.
    pub message: OneBotMessage,
    /// Raw message text.
    #[serde(default)]
    pub raw_message: String,
    /// Font (usually 0).
    #[serde(default)]
    pub font: i32,
    /// Sender information.
    pub sender: OneBotSender,
}

impl MessageEvent {
    /// Get the plain text content of the message.
    pub fn get_text(&self) -> String {
        self.message.to_plain_text()
    }

    /// Get the plain text content, removing @mentions of the bot.
    /// Useful for group messages where the bot is mentioned directly.
    pub fn get_text_without_at(&self) -> String {
        self.message.to_plain_text_without_at(&self.self_id.to_string())
    }

    /// Check if this is a group message.
    pub fn is_group(&self) -> bool {
        self.message_type == "group"
    }

    /// Check if this is a private message.
    pub fn is_private(&self) -> bool {
        self.message_type == "private"
    }

    /// Extract media URLs from the message.
    ///
    /// Returns a list of (url, media_type) tuples where media_type is one of:
    /// "image", "voice", "video", "file"
    pub fn get_media_urls(&self) -> Vec<(String, String)> {
        self.message.get_media_urls()
    }

    /// Check if message contains media.
    pub fn has_media(&self) -> bool {
        self.message.has_media()
    }

    /// Check if the bot is @mentioned in this message.
    /// Uses self_id (bot's QQ number) to check.
    pub fn is_at_me(&self) -> bool {
        self.message.is_at_me(&self.self_id.to_string())
    }

    /// Check if message contains @all.
    pub fn is_at_all(&self) -> bool {
        self.message.is_at_all()
    }
}

/// Sender information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OneBotSender {
    /// Sender QQ number (can be number or string in JSON).
    #[serde(deserialize_with = "deserialize_id")]
    pub user_id: String,
    /// Sender nickname.
    pub nickname: String,
    /// Group card (display name in group).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
    /// Gender: "male", "female", "unknown".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sex: Option<String>,
    /// Age.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age: Option<i32>,
    /// Level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// Group role: "owner", "admin", "member".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Title (special badge in group).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Private message event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivateMessageEvent {
    #[serde(flatten)]
    pub base: MessageEvent,
    /// Sub type: "friend", "group", "other".
    pub sub_type: String,
    /// Temporary session source group ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_source: Option<String>,
}

/// Group message event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMessageEvent {
    #[serde(flatten)]
    pub base: MessageEvent,
    /// Group ID.
    pub group_id: String,
    /// Group name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_name: Option<String>,
    /// Anonymous info (for anonymous messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anonymous: Option<AnonymousInfo>,
}

/// Anonymous user information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnonymousInfo {
    /// Anonymous ID.
    pub id: i64,
    /// Anonymous name.
    pub name: String,
    /// Anonymous flag.
    pub flag: String,
}

/// Notice event structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoticeEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "notice".
    pub post_type: String,
    /// Notice type.
    pub notice_type: String,
    /// Additional data.
    #[serde(flatten)]
    pub data: Value,
}

// =============================================================================
// Notice 子事件结构体
// =============================================================================

/// 群消息撤回事件.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupRecallEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "notice".
    pub post_type: String,
    /// Notice type: "group_recall".
    pub notice_type: String,
    /// Group ID.
    pub group_id: String,
    /// User ID whose message was recalled.
    pub user_id: String,
    /// Operator ID who recalled the message.
    pub operator_id: String,
    /// Message ID that was recalled.
    pub message_id: i64,
}

/// 好友消息撤回事件.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendRecallEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "notice".
    pub post_type: String,
    /// Notice type: "friend_recall".
    pub notice_type: String,
    /// User ID whose message was recalled.
    pub user_id: String,
    /// Message ID that was recalled.
    pub message_id: i64,
}

/// 群成员增加事件.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupIncreaseEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "notice".
    pub post_type: String,
    /// Notice type: "group_increase".
    pub notice_type: String,
    /// Group ID.
    pub group_id: String,
    /// User ID who joined.
    pub user_id: String,
    /// Operator ID who invited/approved.
    pub operator_id: String,
    /// Sub type: "approve" (approved join) or "invite" (invited by admin).
    pub sub_type: String,
}

/// 群成员减少事件.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupDecreaseEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "notice".
    pub post_type: String,
    /// Notice type: "group_decrease".
    pub notice_type: String,
    /// Group ID.
    pub group_id: String,
    /// User ID who left/was kicked.
    pub user_id: String,
    /// Operator ID who kicked (if kicked).
    pub operator_id: String,
    /// Sub type: "leave" (voluntary), "kick" (kicked), "kick_me" (bot kicked).
    pub sub_type: String,
}

/// 群管理员变动事件.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupAdminEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "notice".
    pub post_type: String,
    /// Notice type: "group_admin".
    pub notice_type: String,
    /// Group ID.
    pub group_id: String,
    /// User ID whose admin status changed.
    pub user_id: String,
    /// Whether the user is now admin (true = set admin, false = unset admin).
    pub sub_type: String,
}

/// 群禁言事件.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupBanEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "notice".
    pub post_type: String,
    /// Notice type: "group_ban".
    pub notice_type: String,
    /// Group ID.
    pub group_id: String,
    /// User ID who was banned/unbanned.
    pub user_id: String,
    /// Operator ID who performed the ban.
    pub operator_id: String,
    /// Duration in seconds (0 = unbanned).
    pub duration: i64,
    /// Sub type: "ban" or "lift_ban".
    pub sub_type: String,
}

/// 群文件上传事件.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupUploadEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "notice".
    pub post_type: String,
    /// Notice type: "group_upload".
    pub notice_type: String,
    /// Group ID.
    pub group_id: String,
    /// User ID who uploaded the file.
    pub user_id: String,
    /// File information.
    pub file: Value,
}

/// 戳一戳事件.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PokeEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "notice".
    pub post_type: String,
    /// Notice type: "notify".
    pub notice_type: String,
    /// Sub type: "poke".
    pub sub_type: String,
    /// User ID who poked.
    pub user_id: String,
    /// Target user ID (who was poked).
    pub target_id: String,
    /// Group ID (for group poke).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
}

/// 好友添加事件.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendAddEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "notice".
    pub post_type: String,
    /// Notice type: "friend_add".
    pub notice_type: String,
    /// User ID who was added as friend.
    pub user_id: String,
}

/// 群名片变更事件.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupCardEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "notice".
    pub post_type: String,
    /// Notice type: "group_card".
    pub notice_type: String,
    /// Group ID.
    pub group_id: String,
    /// User ID whose card changed.
    pub user_id: String,
    /// New card name.
    pub card_new: String,
    /// Old card name.
    pub card_old: String,
}

/// 精华消息事件.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EssenceEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "notice".
    pub post_type: String,
    /// Notice type: "essence".
    pub notice_type: String,
    /// Group ID.
    pub group_id: String,
    /// Message ID.
    pub message_id: i64,
    /// Operator ID.
    pub sender_id: String,
    /// Operator ID who set/unset essence.
    pub operator_id: String,
    /// Sub type: "add" or "delete".
    pub sub_type: String,
}

/// Meta event structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "meta_event".
    pub post_type: String,
    /// Meta event type: "lifecycle", "heartbeat".
    pub meta_event_type: String,
    /// Additional data.
    #[serde(flatten)]
    pub data: Value,
}

/// Lifecycle event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "meta_event".
    pub post_type: String,
    /// Meta event type: "lifecycle".
    pub meta_event_type: String,
    /// Sub type: "enable", "disable", "connect".
    pub sub_type: String,
}

/// Heartbeat event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "meta_event".
    pub post_type: String,
    /// Meta event type: "heartbeat".
    pub meta_event_type: String,
    /// Bot status.
    pub status: Value,
    /// Heartbeat interval in milliseconds.
    pub interval: i64,
}

/// Request event (friend request or group invite).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "request".
    pub post_type: String,
    /// Request type: "friend" or "group".
    pub request_type: String,
    /// User ID of the requester.
    pub user_id: String,
    /// Comment/message from requester.
    #[serde(default)]
    pub comment: String,
    /// Flag for responding to the request.
    pub flag: String,
    /// Group ID (for group requests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
}

/// 好友请求事件.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendRequestEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "request".
    pub post_type: String,
    /// Request type: "friend".
    pub request_type: String,
    /// User ID of the requester.
    pub user_id: String,
    /// Comment/message from requester.
    #[serde(default)]
    pub comment: String,
    /// Flag for responding to the request.
    pub flag: String,
}

/// 群邀请/加群请求事件.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupRequestEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "request".
    pub post_type: String,
    /// Request type: "group".
    pub request_type: String,
    /// Sub type: "add" (user requesting to join) or "invite" (invited by admin).
    pub sub_type: String,
    /// Group ID.
    pub group_id: String,
    /// User ID of the requester.
    pub user_id: String,
    /// Comment/message from requester.
    #[serde(default)]
    pub comment: String,
    /// Flag for responding to the request.
    pub flag: String,
    /// Invitor ID (for invite type).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitor_id: Option<String>,
}

/// Message sent event (NapCat-specific).
/// Fired when the bot sends a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSentEvent {
    /// Event timestamp.
    pub time: i64,
    /// Bot QQ number.
    pub self_id: i64,
    /// Event type: "message_sent".
    pub post_type: String,
    /// Message type: "private" or "group".
    pub message_type: String,
    /// Sub type.
    pub sub_type: String,
    /// Target user ID (for private messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Target group ID (for group messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    /// Message ID.
    pub message_id: i64,
    /// Message sequence number.
    #[serde(default)]
    pub message_seq: i64,
    /// Real message ID.
    #[serde(default)]
    pub real_id: i64,
    /// Message content.
    pub message: OneBotMessage,
    /// Raw message text.
    #[serde(default)]
    pub raw_message: String,
    /// Font (usually 0).
    #[serde(default)]
    pub font: i32,
    /// Sender information (the bot itself).
    pub sender: OneBotSender,
}

impl MessageSentEvent {
    /// Check if this was sent to a group.
    pub fn is_group(&self) -> bool {
        self.message_type == "group"
    }

    /// Check if this was sent to a private chat.
    pub fn is_private(&self) -> bool {
        self.message_type == "private"
    }

    /// Get the plain text content of the message.
    pub fn get_text(&self) -> String {
        self.message.to_plain_text()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_event_is_group() {
        let event = MessageEvent {
            time: 1700000000,
            self_id: 123456789,
            post_type: "message".to_string(),
            message_type: "group".to_string(),
            sub_type: "normal".to_string(),
            user_id: "987654321".to_string(),
            target_id: None,
            group_id: Some("111111111".to_string()),
            message_id: 12345,
            message_seq: 12345,
            real_id: 12345,
            message: OneBotMessage::from_text("hello"),
            raw_message: "hello".to_string(),
            font: 0,
            sender: OneBotSender {
                user_id: "987654321".to_string(),
                nickname: "Test User".to_string(),
                card: None,
                sex: None,
                age: None,
                level: None,
                role: Some("member".to_string()),
                title: None,
            },
        };

        assert!(event.is_group());
        assert!(!event.is_private());
        assert_eq!(event.get_text(), "hello");
    }

    #[test]
    fn test_message_event_is_private() {
        let event = MessageEvent {
            time: 1700000000,
            self_id: 123456789,
            post_type: "message".to_string(),
            message_type: "private".to_string(),
            sub_type: "friend".to_string(),
            user_id: "987654321".to_string(),
            target_id: None,
            group_id: None,
            message_id: 12345,
            message_seq: 12345,
            real_id: 12345,
            message: OneBotMessage::from_text("hello"),
            raw_message: "hello".to_string(),
            font: 0,
            sender: OneBotSender {
                user_id: "987654321".to_string(),
                nickname: "Test User".to_string(),
                card: None,
                sex: None,
                age: None,
                level: None,
                role: None,
                title: None,
            },
        };

        assert!(event.is_private());
        assert!(!event.is_group());
    }

    #[test]
    fn test_parse_one_bot_event() {
        let json = r#"{
            "time": 1700000000,
            "self_id": 123456789,
            "post_type": "meta_event",
            "meta_event_type": "lifecycle",
            "sub_type": "connect"
        }"#;

        let event: OneBotEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.post_type, "meta_event");
        assert_eq!(event.time, 1700000000);
    }

    #[test]
    fn test_message_event_get_media_urls() {
        use crate::napcat::MessageSegment;

        let event = MessageEvent {
            time: 1700000000,
            self_id: 123456789,
            post_type: "message".to_string(),
            message_type: "private".to_string(),
            sub_type: "friend".to_string(),
            user_id: "987654321".to_string(),
            target_id: None,
            group_id: None,
            message_id: 12345,
            message_seq: 12345,
            real_id: 12345,
            message: OneBotMessage::from_segments(vec![
                MessageSegment::text("look at this"),
                MessageSegment::image("photo.jpg"),
            ]),
            raw_message: "look at this [image]".to_string(),
            font: 0,
            sender: OneBotSender {
                user_id: "987654321".to_string(),
                nickname: "Test User".to_string(),
                card: None,
                sex: None,
                age: None,
                level: None,
                role: None,
                title: None,
            },
        };

        assert!(event.has_media());
        let media = event.get_media_urls();
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].1, "image");
    }

    #[test]
    fn test_message_event_no_media() {
        let event = MessageEvent {
            time: 1700000000,
            self_id: 123456789,
            post_type: "message".to_string(),
            message_type: "private".to_string(),
            sub_type: "friend".to_string(),
            user_id: "987654321".to_string(),
            target_id: None,
            group_id: None,
            message_id: 12345,
            message_seq: 12345,
            real_id: 12345,
            message: OneBotMessage::from_text("hello"),
            raw_message: "hello".to_string(),
            font: 0,
            sender: OneBotSender {
                user_id: "987654321".to_string(),
                nickname: "Test User".to_string(),
                card: None,
                sex: None,
                age: None,
                level: None,
                role: None,
                title: None,
            },
        };

        assert!(!event.has_media());
        assert!(event.get_media_urls().is_empty());
    }
}