//! OneBot 11 message segment types.
//!
//! This module defines the message segment types used in OneBot 11 protocol,
//! including text, image, at, reply, and other message types.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

/// Helper module for deserializing string or number fields.
mod string_or_number {
    use serde::{Deserialize, Deserializer, Serializer};
    use serde_json::Value;

    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: std::str::FromStr + serde::Deserialize<'de>,
        T::Err: std::fmt::Debug,
    {
        let opt = Option::<Value>::deserialize(deserializer)?;
        match opt {
            None => Ok(None),
            Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => s.parse::<T>().map(Some).map_err(|_| {
                serde::de::Error::custom(format!("Failed to parse string '{}' as number", s))
            }),
            Some(Value::Number(n)) => {
                if let Some(i) = n.as_i64() {
                    // Try to convert i64 to T
                    T::from_str(&i.to_string()).map(Some).map_err(|_| {
                        serde::de::Error::custom("Failed to convert number")
                    })
                } else if let Some(f) = n.as_f64() {
                    T::from_str(&f.to_string()).map(Some).map_err(|_| {
                        serde::de::Error::custom("Failed to convert float")
                    })
                } else {
                    Err(serde::de::Error::custom("Invalid number"))
                }
            }
            Some(other) => Err(serde::de::Error::custom(format!(
                "Expected string or number, got {:?}",
                other
            ))),
        }
    }

    pub fn serialize<S, T>(value: &Option<T>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: std::fmt::Display + serde::Serialize,
    {
        match value {
            Some(v) => serializer.serialize_str(&v.to_string()),
            None => serializer.serialize_none(),
        }
    }
}

/// OneBot message type (supports text or message segment array).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneBotMessage {
    /// Plain text message.
    Text(String),
    /// Message segment array.
    Segments(Vec<MessageSegment>),
}

impl OneBotMessage {
    /// Create a plain text message.
    pub fn from_text(text: &str) -> Self {
        OneBotMessage::Text(text.to_string())
    }

    /// Create a message from segments.
    pub fn from_segments(segments: Vec<MessageSegment>) -> Self {
        OneBotMessage::Segments(segments)
    }

    /// Convert to plain text representation.
    pub fn to_plain_text(&self) -> String {
        match self {
            OneBotMessage::Text(s) => s.clone(),
            OneBotMessage::Segments(segs) => {
                segs.iter()
                    .filter_map(|seg| seg.to_plain_text())
                    .collect::<Vec<_>>()
                    .join("")
            }
        }
    }

    /// Convert to plain text, removing @mentions of the bot.
    /// This is useful for group messages where the bot is mentioned,
    /// so it can respond naturally without seeing the @mention in the content.
    pub fn to_plain_text_without_at(&self, bot_qq: &str) -> String {
        match self {
            OneBotMessage::Text(s) => s.clone(),
            OneBotMessage::Segments(segs) => {
                segs.iter()
                    .filter_map(|seg| {
                        match seg {
                            // Skip @mentions of the bot
                            MessageSegment::At { qq, .. } if qq == bot_qq => None,
                            // Keep all other segments
                            _ => seg.to_plain_text(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("")
                    .trim()
                    .to_string()
            }
        }
    }

    /// Extract media URLs from message segments.
    ///
    /// Returns a list of (url, media_type) tuples where media_type is one of:
    /// "image", "voice", "video", "file"
    pub fn get_media_urls(&self) -> Vec<(String, String)> {
        match self {
            OneBotMessage::Text(_) => vec![],
            OneBotMessage::Segments(segs) => {
                segs.iter()
                    .filter_map(|seg| seg.get_media_url())
                    .collect()
            }
        }
    }

    /// Check if message contains media.
    pub fn has_media(&self) -> bool {
        match self {
            OneBotMessage::Text(_) => false,
            OneBotMessage::Segments(segs) => segs.iter().any(|seg| seg.is_media()),
        }
    }

    /// Convert to JSON value for API calls.
    pub fn to_json(&self) -> Value {
        match self {
            OneBotMessage::Text(s) => Value::String(s.clone()),
            OneBotMessage::Segments(segs) => {
                serde_json::to_value(segs).unwrap_or(Value::Null)
            }
        }
    }

    /// Check if the bot is @mentioned in this message.
    /// Returns true if there's an At segment with the bot's QQ number.
    pub fn is_at_me(&self, bot_qq: &str) -> bool {
        match self {
            OneBotMessage::Text(_) => false,
            OneBotMessage::Segments(segs) => {
                segs.iter().any(|seg| {
                    matches!(seg, MessageSegment::At { qq, .. } if qq == bot_qq)
                })
            }
        }
    }

    /// Check if message contains @all.
    pub fn is_at_all(&self) -> bool {
        match self {
            OneBotMessage::Text(_) => false,
            OneBotMessage::Segments(segs) => {
                segs.iter().any(|seg| matches!(seg, MessageSegment::AtAll {}))
            }
        }
    }

    /// Get message segments as a slice.
    ///
    /// Returns an empty slice for text-only messages.
    pub fn as_segments(&self) -> &[MessageSegment] {
        match self {
            OneBotMessage::Text(_) => &[],
            OneBotMessage::Segments(segs) => segs,
        }
    }
}

impl Default for OneBotMessage {
    fn default() -> Self {
        OneBotMessage::Text(String::new())
    }
}

/// Message segment type enumeration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
#[serde(rename_all = "snake_case")]
pub enum MessageSegment {
    /// Plain text.
    Text { text: String },
    /// QQ emoji.
    Face {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chain_count: Option<i32>,
    },
    /// Mall emoji (sticker store).
    Mface {
        emoji_id: String,
        emoji_package_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// Image.
    Image {
        file: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sub_type: Option<i32>,
        #[serde(default, with = "string_or_number", skip_serializing_if = "Option::is_none")]
        file_size: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proxy: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<i64>,
    },
    /// Voice/audio.
    Record {
        file: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, with = "string_or_number", skip_serializing_if = "Option::is_none")]
        file_size: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proxy: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<i64>,
    },
    /// Video.
    Video {
        file: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, with = "string_or_number", skip_serializing_if = "Option::is_none")]
        file_size: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thumb: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proxy: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout: Option<i64>,
    },
    /// @someone.
    At {
        qq: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// @all members.
    AtAll {},
    /// Rock-paper-scissors magic emoji.
    Rps {
        #[serde(rename = "result")]
        result_value: String,
    },
    /// Dice magic emoji.
    Dice {
        #[serde(rename = "result")]
        result_value: String,
    },
    /// Poke.
    Poke {
        #[serde(rename = "type")]
        poke_type: String,
        id: String,
    },
    /// Music share.
    Music {
        #[serde(rename = "type")]
        music_type: String,
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audio: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
    },
    /// Link share.
    Share {
        url: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
    },
    /// Reply to a message.
    Reply {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        qq: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        time: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<i64>,
    },
    /// Forward message (combined).
    Forward { id: String },
    /// Custom forward node.
    Node {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nickname: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
    },
    /// XML message.
    Xml { data: String },
    /// JSON message.
    Json { data: String },
    /// Card message.
    Card { data: String },
    /// File.
    File {
        file: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, with = "string_or_number", skip_serializing_if = "Option::is_none")]
        size: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        busid: Option<i64>,
    },
    /// Unknown message segment type.
    #[serde(other)]
    Unknown,
}

impl MessageSegment {
    /// Convert to plain text representation.
    pub fn to_plain_text(&self) -> Option<String> {
        match self {
            MessageSegment::Text { text } => Some(text.clone()),
            MessageSegment::At { qq, .. } => Some(format!("[@{}]", qq)),
            MessageSegment::AtAll {} => Some("[@all]".to_string()),
            MessageSegment::Image { .. } => Some("[image]".to_string()),
            MessageSegment::Record { .. } => Some("[voice]".to_string()),
            MessageSegment::Video { .. } => Some("[video]".to_string()),
            MessageSegment::Face { id, .. } => Some(format!("[face:{}]", id)),
            MessageSegment::Mface { emoji_id, summary, .. } => {
                let desc = summary.clone().unwrap_or_else(|| format!("emoji:{}", emoji_id));
                Some(format!("[mface:{}]", desc))
            }
            MessageSegment::Reply { .. } => Some("[reply]".to_string()),
            MessageSegment::File { name, .. } => {
                Some(format!("[file:{}]", name.clone().unwrap_or_default()))
            }
            MessageSegment::Music { title, .. } => {
                Some(format!("[music:{}]", title.clone().unwrap_or_default()))
            }
            MessageSegment::Share { title, .. } => Some(format!("[share:{}]", title)),
            MessageSegment::Poke { .. } => Some("[poke]".to_string()),
            MessageSegment::Dice { .. } => Some("[dice]".to_string()),
            MessageSegment::Rps { .. } => Some("[rps]".to_string()),
            MessageSegment::Forward { .. } => Some("[forward]".to_string()),
            MessageSegment::Node { .. } => Some("[node]".to_string()),
            MessageSegment::Xml { .. } => Some("[xml]".to_string()),
            MessageSegment::Json { .. } => Some("[json]".to_string()),
            MessageSegment::Card { .. } => Some("[card]".to_string()),
            MessageSegment::Unknown => {
                warn!("Encountered unknown message segment type during text conversion");
                None
            }
        }
    }

    /// Check if this segment is a media type (image, voice, video, file).
    pub fn is_media(&self) -> bool {
        matches!(
            self,
            MessageSegment::Image { .. }
                | MessageSegment::Record { .. }
                | MessageSegment::Video { .. }
                | MessageSegment::File { .. }
        )
    }

    /// Get media URL and type if this is a media segment.
    ///
    /// Returns Some((url, media_type)) where media_type is one of:
    /// "image", "voice", "video", "file"
    pub fn get_media_url(&self) -> Option<(String, String)> {
        match self {
            MessageSegment::Image { file, url, .. } => {
                // Prefer URL if available, otherwise use file
                let media_url = url.clone().unwrap_or_else(|| file.clone());
                Some((media_url, "image".to_string()))
            }
            MessageSegment::Record { file, url, .. } => {
                let media_url = url.clone().unwrap_or_else(|| file.clone());
                Some((media_url, "voice".to_string()))
            }
            MessageSegment::Video { file, url, .. } => {
                let media_url = url.clone().unwrap_or_else(|| file.clone());
                Some((media_url, "video".to_string()))
            }
            MessageSegment::File { file, url, .. } => {
                let media_url = url.clone().unwrap_or_else(|| file.clone());
                Some((media_url, "file".to_string()))
            }
            _ => None,
        }
    }

    /// Create a text segment.
    pub fn text(text: &str) -> Self {
        MessageSegment::Text {
            text: text.to_string(),
        }
    }

    /// Create an image segment.
    pub fn image(file: &str) -> Self {
        MessageSegment::Image {
            file: file.to_string(),
            url: None,
            summary: None,
            sub_type: None,
            file_size: None,
            cache: None,
            proxy: None,
            timeout: None,
        }
    }

    /// Create an image segment with URL.
    pub fn image_with_url(file: &str, url: &str) -> Self {
        MessageSegment::Image {
            file: file.to_string(),
            url: Some(url.to_string()),
            summary: None,
            sub_type: None,
            file_size: None,
            cache: None,
            proxy: None,
            timeout: None,
        }
    }

    /// Create an at segment.
    pub fn at(qq: &str) -> Self {
        MessageSegment::At {
            qq: qq.to_string(),
            name: None,
        }
    }

    /// Create an at-all segment.
    pub fn at_all() -> Self {
        MessageSegment::AtAll {}
    }

    /// Create a reply segment.
    pub fn reply(id: &str) -> Self {
        MessageSegment::Reply {
            id: id.to_string(),
            text: None,
            qq: None,
            time: None,
            seq: None,
        }
    }

    /// Create a voice segment.
    pub fn record(file: &str) -> Self {
        MessageSegment::Record {
            file: file.to_string(),
            url: None,
            file_size: None,
            path: None,
            cache: None,
            proxy: None,
            timeout: None,
        }
    }

    /// Create a video segment.
    pub fn video(file: &str) -> Self {
        MessageSegment::Video {
            file: file.to_string(),
            url: None,
            file_size: None,
            thumb: None,
            cache: None,
            proxy: None,
            timeout: None,
        }
    }

    /// Create a file segment.
    pub fn file(file: &str, name: Option<&str>) -> Self {
        MessageSegment::File {
            file: file.to_string(),
            file_id: None,
            url: None,
            name: name.map(|s| s.to_string()),
            size: None,
            busid: None,
        }
    }
}

/// Build a message from text and media paths.
pub fn build_message(text: &str, media_paths: &[String]) -> OneBotMessage {
    if media_paths.is_empty() {
        return OneBotMessage::from_text(text);
    }

    let mut segments = Vec::new();

    // Add text segment if not empty
    if !text.is_empty() {
        segments.push(MessageSegment::text(text));
    }

    // Add media segments based on file extension
    for path in media_paths {
        let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();

        let segment = if ["jpg", "jpeg", "png", "gif", "webp", "bmp"].contains(&ext.as_str()) {
            MessageSegment::image(path)
        } else if ["mp3", "wav", "ogg", "amr", "flac", "aac"]
            .contains(&ext.as_str())
        {
            MessageSegment::record(path)
        } else if ["mp4", "mov", "avi", "mkv", "flv"].contains(&ext.as_str()) {
            MessageSegment::video(path)
        } else {
            let filename = path.rsplit(['/', '\\']).next().unwrap_or(path);
            MessageSegment::file(path, Some(filename))
        };

        segments.push(segment);
    }

    OneBotMessage::from_segments(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_one_bot_message_from_text() {
        let msg = OneBotMessage::from_text("hello");
        assert_eq!(msg.to_plain_text(), "hello");
    }

    #[test]
    fn test_one_bot_message_from_segments() {
        let msg = OneBotMessage::from_segments(vec![
            MessageSegment::text("hello "),
            MessageSegment::at("12345"),
        ]);
        assert_eq!(msg.to_plain_text(), "hello [@12345]");
    }

    #[test]
    fn test_message_segment_text() {
        let seg = MessageSegment::text("hello");
        assert_eq!(seg.to_plain_text(), Some("hello".to_string()));
    }

    #[test]
    fn test_message_segment_image() {
        let seg = MessageSegment::image("test.jpg");
        assert_eq!(seg.to_plain_text(), Some("[image]".to_string()));
    }

    #[test]
    fn test_message_segment_at() {
        let seg = MessageSegment::at("12345");
        assert_eq!(seg.to_plain_text(), Some("[@12345]".to_string()));
    }

    #[test]
    fn test_message_segment_at_all() {
        let seg = MessageSegment::at_all();
        assert_eq!(seg.to_plain_text(), Some("[@all]".to_string()));
    }

    #[test]
    fn test_build_message_text_only() {
        let msg = build_message("hello world", &[]);
        assert_eq!(msg.to_plain_text(), "hello world");
    }

    #[test]
    fn test_build_message_with_image() {
        let msg = build_message("look at this", &["photo.jpg".to_string()]);
        let text = msg.to_plain_text();
        assert!(text.contains("look at this"));
        assert!(text.contains("[image]"));
    }

    #[test]
    fn test_message_to_json() {
        let msg = OneBotMessage::from_text("hello");
        let json = msg.to_json();
        assert_eq!(json, Value::String("hello".to_string()));
    }

    #[test]
    fn test_get_media_urls_from_text() {
        let msg = OneBotMessage::from_text("hello");
        assert!(msg.get_media_urls().is_empty());
        assert!(!msg.has_media());
    }

    #[test]
    fn test_get_media_urls_from_image() {
        let seg = MessageSegment::Image {
            file: "test.jpg".to_string(),
            url: Some("http://example.com/test.jpg".to_string()),
            summary: None,
            sub_type: None,
            file_size: None,
            cache: None,
            proxy: None,
            timeout: None,
        };
        let msg = OneBotMessage::from_segments(vec![seg]);
        let media = msg.get_media_urls();
        assert_eq!(media.len(), 1);
        assert_eq!(media[0], ("http://example.com/test.jpg".to_string(), "image".to_string()));
        assert!(msg.has_media());
    }

    #[test]
    fn test_get_media_urls_multiple() {
        let msg = OneBotMessage::from_segments(vec![
            MessageSegment::text("check this out"),
            MessageSegment::image("photo1.jpg"),
            MessageSegment::image("photo2.jpg"),
            MessageSegment::record("voice.mp3"),
        ]);
        let media = msg.get_media_urls();
        assert_eq!(media.len(), 3);
        assert_eq!(media[0].1, "image");
        assert_eq!(media[1].1, "image");
        assert_eq!(media[2].1, "voice");
        assert!(msg.has_media());
    }

    #[test]
    fn test_is_media() {
        assert!(MessageSegment::image("test.jpg").is_media());
        assert!(MessageSegment::record("test.mp3").is_media());
        assert!(MessageSegment::video("test.mp4").is_media());
        assert!(MessageSegment::file("test.pdf", None).is_media());
        assert!(!MessageSegment::text("hello").is_media());
        assert!(!MessageSegment::at("12345").is_media());
    }

    #[test]
    fn test_get_media_url_with_url() {
        let seg = MessageSegment::image_with_url("file.jpg", "http://example.com/file.jpg");
        let (url, media_type) = seg.get_media_url().unwrap();
        assert_eq!(url, "http://example.com/file.jpg");
        assert_eq!(media_type, "image");
    }

    #[test]
    fn test_get_media_url_without_url() {
        let seg = MessageSegment::image("local_file.jpg");
        let (url, media_type) = seg.get_media_url().unwrap();
        assert_eq!(url, "local_file.jpg");
        assert_eq!(media_type, "image");
    }
}