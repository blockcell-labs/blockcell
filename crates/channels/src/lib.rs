pub mod manager;

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

pub use manager::ChannelManager;
