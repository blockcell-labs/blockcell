//! NapCatQQ channel implementation for BlockCell.
//!
//! This module provides support for NapCatQQ, a community-driven QQ bot protocol
//! implementation based on the OneBot 11 standard.
//!
//! # Features
//!
//! - **ws-client**: BlockCell connects to NapCatQQ WebSocket server
//! - **ws-server**: NapCatQQ connects to BlockCell WebSocket server
//! - Message deduplication
//! - User and group allowlisting/blocklisting
//!
//! # Configuration
//!
//! ## WebSocket Client Mode (ws-client, default)
//!
//! BlockCell acts as WebSocket client, connects to NapCatQQ WebSocket server.
//! Add to your `~/.blockcell/config.json5`:
//!
//! ```json5
//! {
//!   "channels": {
//!     "napcat": {
//!       "enabled": true,
//!       "mode": "ws-client",
//!       "wsUrl": "ws://127.0.0.1:3001",
//!       "accessToken": "your-token"
//!     }
//!   }
//! }
//! ```
//!
//! ## WebSocket Server Mode (ws-server)
//!
//! BlockCell acts as WebSocket server, NapCatQQ connects to it.
//! Configure NapCatQQ's `websocketClients` to point to BlockCell.
//!
//! ```json5
//! {
//!   "channels": {
//!     "napcat": {
//!       "enabled": true,
//!       "mode": "ws-server",
//!       "serverHost": "0.0.0.0",
//!       "serverPort": 8080,
//!       "serverPath": "/onebot/v11/ws",
//!       "accessToken": "your-token"
//!     }
//!   }
//! }
//! ```
//}()

pub mod channel;
pub mod event;
pub mod media;
pub mod message;
pub mod outbound;
pub mod types;
pub mod websocket;

pub use channel::NapCatChannel;
pub use event::*;
pub use message::*;
pub use outbound::{send_media_message, send_message};
pub use types::*;
pub use websocket::{
    get_sender, init_sender, is_ws_mode, send_via_ws, send_via_ws_with_self_id,
    is_ws_api_available, call_api_via_ws,
    is_ws_stream_available, call_stream_api_via_ws,
};