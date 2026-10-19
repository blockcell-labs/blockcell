use thiserror::Error;

/// BlockCell 核心错误类型。
///
/// ## 设计说明
/// 大部分变体使用 `String` 承载自由文本消息，而非强类型字段。
/// 这是有意为之——灵活的消息格式便于快速迭代和动态错误构造。
/// 未来版本可视稳定性需求逐步引入 typed 错误字段。
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    #[error("Config error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("Provider error: {0}")]
    Provider(String),

    #[error("Tool error: {0}")]
    Tool(String),

    #[error("Session error: {0}")]
    Session(String),

    #[error("Channel error: {0}")]
    Channel(String),

    #[error("Skill error: {0}")]
    Skill(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Evolution error: {0}")]
    Evolution(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
