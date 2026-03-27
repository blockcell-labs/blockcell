//! WebSocket modules for NapCatQQ.
//!
//! This module provides both client and server mode WebSocket implementations
//! for communicating with NapCatQQ using the OneBot 11 protocol.

pub mod client;
pub mod sender;
pub mod server;

pub use client::NapCatWsClient;
pub use sender::{
    get_sender, init_sender, is_ws_mode, send_via_ws, send_via_ws_with_self_id,
    OutboundMessage, is_ws_api_available, call_api_via_ws,
    is_ws_stream_available, call_stream_api_via_ws,
};
pub use server::NapCatWsServer;