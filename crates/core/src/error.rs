use thiserror::Error;

/// BlockCell 核心错误类型。
///
/// ## 设计说明
/// Provider 适配器使用强类型恢复信息；字符串 Provider 变体保留给旧适配器。
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

    #[error("Provider error: {0}")]
    ProviderFailure(ProviderFailure),

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

/// Provider adapters retain machine-readable recovery information separately from
/// the sanitized message shown in logs. Legacy external adapters may use Provider.
#[derive(Debug, Clone)]
pub struct ProviderFailure {
    pub kind: crate::FailureKind,
    pub status: Option<u16>,
    pub code: Option<String>,
    pub retry_after_ms: Option<u64>,
    pub message: String,
}

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error {
    pub fn provider_failure_kind(&self) -> crate::FailureKind {
        match self {
            Self::ProviderFailure(failure) => failure.kind,
            _ => crate::FailureKind::classify_provider_message(&self.to_string()),
        }
    }
}
