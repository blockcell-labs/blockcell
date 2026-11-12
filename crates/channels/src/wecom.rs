use crate::account::{wecom_account_id, wecom_listener_configs};
use aes::cipher::{BlockDecryptMut, KeyIvInit};
use base64::{
    alphabet,
    engine::{general_purpose, DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
    Engine as _,
};
use blockcell_core::{Config, Error, InboundMessage, Result};
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use tracing::{debug, error, info, warn};

// --- submodules extracted from the original monolithic wecom.rs ---
mod dispatch;
mod longconn;
mod media;
mod polling;
mod send;
mod webhook;

pub use media::*;
pub use send::*;
pub use webhook::*;
// 所有 std::sync::Mutex 均为短时锁定且不在 .await 间持有锁，因此安全。
// 详细审计见：LONGCONN_REGISTRY、CHAT_REQID_REGISTRY、SEEN_MSG_IDS 的 lock() 调用点
// 均未跨越 .await 点。token_cache 则直接使用 tokio::sync::Mutex。

/// Global msg_id dedup set — prevents the same WeCom message from being processed twice.
/// WeCom sometimes delivers the same webhook twice (retry on timeout) or echoes bot-sent
/// messages back as callbacks. We keep the last 512 msg_ids in a ring-buffer style set.
static SEEN_MSG_IDS: std::sync::LazyLock<Mutex<HashSet<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));

/// Outbound message for the long connection WebSocket channel.
enum LongConnOutbound {
    /// Plain text reply (stream msgtype).
    Text {
        chat_id: String,
        content: String,
        result_tx: tokio::sync::oneshot::Sender<Result<()>>,
    },
    /// Media message: upload file via chunked WS protocol, then send via aibot_send_msg.
    Media {
        chat_id: String,
        file_path: String,
        /// WeCom media type: image / voice / video / file
        media_type: String,
        /// Optional title (for video messages).
        title: String,
        /// Oneshot to return the result (Ok media_id or Err) back to the caller.
        result_tx: tokio::sync::oneshot::Sender<Result<String>>,
    },
}

/// Registry of active long connection outbound senders keyed by bot_id.
/// `send_message` uses this to route replies through the WebSocket instead of REST.
static LONGCONN_REGISTRY: std::sync::LazyLock<
    Mutex<HashMap<String, mpsc::Sender<LongConnOutbound>>>,
> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Maps chat_id -> latest req_id from aibot_msg_callback.
/// aibot_respond_msg must echo back the original req_id so WeCom routes the reply correctly.
static CHAT_REQID_REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Shared REST access_token cache for WeCom corp APIs.
static WECOM_TOKEN_CACHE: std::sync::LazyLock<tokio::sync::Mutex<CachedToken>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(CachedToken::default()));

const SEEN_MSG_IDS_MAX: usize = 512;

type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

const WECOM_API_BASE: &str = "https://qyapi.weixin.qq.com/cgi-bin";
const WECOM_LONG_WS_URL: &str = "wss://openws.work.weixin.qq.com";
/// WeCom single message character limit
const WECOM_MSG_LIMIT: usize = 2048;
/// Token refresh margin: refresh 5 minutes before expiry
#[allow(dead_code)]
const TOKEN_REFRESH_MARGIN_SECS: i64 = 300;

fn shared_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to build reqwest client")
}

/// Cached access token with expiry timestamp.
#[derive(Default)]
struct CachedToken {
    token: String,
    expires_at: i64,
}

impl CachedToken {
    fn is_valid(&self) -> bool {
        !self.token.is_empty()
            && chrono::Utc::now().timestamp() < self.expires_at - TOKEN_REFRESH_MARGIN_SECS
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    errcode: i32,
    errmsg: String,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct WeComResponse {
    errcode: i32,
    errmsg: String,
}

impl WeComResponse {
    fn is_invalid_token(&self) -> bool {
        matches!(self.errcode, 40014 | 42001)
    }
}

#[derive(Debug, Deserialize)]
struct LongConnEnvelope {
    #[serde(default)]
    cmd: String,
    #[serde(default)]
    headers: serde_json::Value,
    #[serde(default)]
    body: serde_json::Value,
    #[serde(default)]
    errcode: Option<i32>,
    #[serde(default)]
    errmsg: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LongConnHeaders {
    #[serde(default)]
    req_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LongConnFrom {
    #[serde(default)]
    userid: String,
    #[serde(default)]
    nickname: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct LongConnText {
    #[serde(default)]
    content: String,
}

#[derive(Debug, Deserialize, Default)]
struct LongConnImage {
    #[serde(default)]
    url: String,
    #[serde(default)]
    aeskey: String,
}

#[derive(Debug, Deserialize, Default)]
struct LongConnVoice {
    #[serde(default)]
    url: String,
    #[serde(default)]
    aeskey: String,
    #[serde(default)]
    recognition: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct LongConnFile {
    #[serde(default)]
    url: String,
    #[serde(default)]
    aeskey: String,
    #[serde(default)]
    filename: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct LongConnMixedItem {
    #[serde(default)]
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct LongConnMixed {
    #[serde(default)]
    items: Vec<LongConnMixedItem>,
}

#[derive(Debug, Deserialize, Default)]
struct LongConnMsgBody {
    #[serde(default)]
    msgid: String,
    #[serde(default)]
    aibotid: String,
    #[serde(default)]
    chatid: String,
    #[serde(default)]
    chattype: String,
    #[serde(default)]
    from: Option<LongConnFrom>,
    #[serde(default)]
    msgtype: String,
    #[serde(default)]
    text: Option<LongConnText>,
    #[serde(default)]
    image: Option<LongConnImage>,
    #[serde(default)]
    voice: Option<LongConnVoice>,
    #[serde(default)]
    file: Option<LongConnFile>,
    #[serde(default)]
    mixed: Option<LongConnMixed>,
}

#[derive(Debug, Serialize)]
struct LongConnCommand<'a, T> {
    cmd: &'a str,
    headers: serde_json::Value,
    body: T,
}

/// WeCom callback message (XML-based, parsed from webhook)
/// WeCom uses XML for incoming messages via webhook/callback URL.
/// For polling, we use the message API.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct WeComMessage {
    #[serde(rename = "ToUserName")]
    #[serde(default)]
    to_user_name: Option<String>,
    #[serde(rename = "FromUserName")]
    #[serde(default)]
    from_user_name: Option<String>,
    #[serde(rename = "CreateTime")]
    #[serde(default)]
    create_time: Option<i64>,
    #[serde(rename = "MsgType")]
    #[serde(default)]
    msg_type: Option<String>,
    #[serde(rename = "Content")]
    #[serde(default)]
    content: Option<String>,
    #[serde(rename = "MsgId")]
    #[serde(default)]
    msg_id: Option<String>,
    #[serde(rename = "AgentID")]
    #[serde(default)]
    agent_id: Option<String>,
}

/// WeCom channel supporting two modes:
/// - **Callback mode** (preferred): Receives messages via webhook callback URL.
///   Requires `corp_id`, `corp_secret`, `agent_id`, and `token`/`encoding_aes_key` for verification.
/// - **Polling mode**: Polls the message API when callback is not configured.
///
/// WeCom (企业微信) uses a different architecture from other platforms:
/// - Inbound: Webhook callbacks (HTTP POST to your server) or polling
/// - Outbound: REST API `message/send`
///
/// For the Stream SDK / WebSocket approach, WeCom provides a "企业微信接收消息服务器" callback.
/// This implementation uses polling via `message/get_statistics` + direct message send.
pub struct WeComChannel {
    config: Config,
    client: Client,
    #[allow(dead_code)]
    inbound_tx: mpsc::Sender<InboundMessage>,
    token_cache: Arc<tokio::sync::Mutex<CachedToken>>,
    /// 媒体文件下载目录，在 new() 中从 BLOCKCELL_WORKSPACE 环境变量解析并缓存
    media_dir: PathBuf,
}

#[cfg(test)]
mod tests;
