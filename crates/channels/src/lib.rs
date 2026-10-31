/// A benign reason for a listener to stop. Operational failures return Err.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerExit {
    Disabled,
    NotConfigured,
    Shutdown,
}

pub mod account;
pub mod manager;
pub mod rate_limit;
pub(crate) mod security;

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

#[cfg(feature = "qq")]
pub mod qq;

#[cfg(feature = "napcat")]
pub mod napcat;

#[cfg(feature = "weixin")]
pub mod weixin;

pub use manager::ChannelManager;
