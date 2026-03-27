//! NapCatQQ management tools for BlockCell.
//!
//! This module provides LLM-callable tools for NapCatQQ channel management,
//! including group management, user info, and message operations.
//!
//! # Features
//!
//! - Channel restriction: Tools only work when `channel=napcat`
//! - Permission control: Admin operations require user whitelist
//! - Multi-account support: Tools support `account_id` parameter
//! - Mode routing: Automatically routes to HTTP/WebSocket implementation
//!
//! # Tool Categories
//!
//! - **Group Management**: `napcat_set_group_kick`, `napcat_set_group_ban`, etc.
//! - **User Info**: `napcat_get_stranger_info`, `napcat_send_like`, etc.
//! - **Message Operations**: `napcat_delete_msg`, `napcat_get_msg`, etc.
//! - **Account Info**: `napcat_get_login_info`, `napcat_get_group_list`, etc.
//! - **Extended Operations**: `napcat_get_forward_msg`, `napcat_set_essence_msg`, etc.

pub mod common;
pub mod extend;
pub mod group;
pub mod message;
pub mod user;

pub use common::*;