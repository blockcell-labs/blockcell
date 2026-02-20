pub mod manager;
pub mod rate_limit;

#[cfg(feature = "telegram")]
pub mod telegram;

#[cfg(feature = "whatsapp")]
pub mod whatsapp;

#[cfg(feature = "feishu")]
pub mod feishu;

#[cfg(feature = "slack")]
pub mod slack;

#[cfg(feature = "discord")]
pub mod discord;

#[cfg(feature = "dingtalk")]
pub mod dingtalk;

#[cfg(feature = "wecom")]
pub mod wecom;

#[cfg(feature = "lark")]
pub mod lark;

pub use manager::ChannelManager;
