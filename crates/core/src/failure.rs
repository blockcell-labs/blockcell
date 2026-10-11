//! 失败语义：failure is part of protocol。
//!
//! - [`FailureKind`] 区分模型调用与工具调用的失败类型，而不是把所有错误都当作 `retry = true`；
//! - [`Idempotency`] 由工具声明，Runtime 结合两者决定能否自动重试；
//! - `Unknown ≠ Failed` 且 `Unknown ≠ Retryable`：结果未知时必须先对账（查询状态 / 用户确认），
//!   不能直接重试，以免产生重复副作用。

use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    ContextOverflow,
    RateLimited,
    ProviderUnavailable,
    AuthenticationFailed,
    /// 调用在执行前被拒绝（策略、校验、权限、未注册），确定没有产生副作用。
    ToolRejected,
    /// 工具已执行并明确返回失败。
    ToolFailed,
    ToolTimedOut,
    Cancelled,
    /// 无法确定调用是否已生效。
    Unknown,
}

/// 工具调用的幂等性声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Idempotency {
    /// 重复执行没有额外副作用（只读或天然幂等）。
    Safe,
    /// 有副作用，重试前必须先对账确认上一次是否已生效。
    Required,
    /// 未声明。按有副作用对待。
    Unknown,
}

impl FailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContextOverflow => "context_overflow",
            Self::RateLimited => "rate_limited",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::AuthenticationFailed => "authentication_failed",
            Self::ToolRejected => "tool_rejected",
            Self::ToolFailed => "tool_failed",
            Self::ToolTimedOut => "tool_timed_out",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }

    /// 是否允许 Runtime 自动重试同一调用。
    ///
    /// 限流 / 服务不可用表示请求未被接受，可以重试；上下文超限、认证失败、被拒绝、取消
    /// 重试同一请求不会成功；其余失败只有在调用声明为 [`Idempotency::Safe`] 时才可重试。
    pub fn retry_allowed(self, idempotency: Idempotency) -> bool {
        match self {
            Self::ContextOverflow | Self::AuthenticationFailed | Self::ToolRejected => false,
            Self::Cancelled => false,
            Self::RateLimited | Self::ProviderUnavailable => true,
            Self::ToolFailed | Self::ToolTimedOut | Self::Unknown => {
                idempotency == Idempotency::Safe
            }
        }
    }

    /// 对模型服务返回的错误信息分类。
    pub fn classify_provider_message(message: &str) -> Self {
        let lower = message.to_lowercase();
        let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
        if has(&["cancelled", "canceled", "aborted"]) {
            Self::Cancelled
        } else if has(&[
            "context_length_exceeded",
            "maximum context length",
            "context length",
            "context window",
            "prompt is too long",
            "too many tokens",
            "reduce the length",
        ]) {
            Self::ContextOverflow
        } else if has(&[
            "http_status=429",
            "http 429",
            "rate limit",
            "too many requests",
        ]) {
            Self::RateLimited
        } else if has(&[
            "http_status=401",
            "http_status=403",
            "http 401",
            "http 403",
            "unauthorized",
            "invalid api key",
            "authentication failed",
        ]) {
            Self::AuthenticationFailed
        } else if has(&[
            "http_status=5",
            "http 5",
            "server error",
            "overloaded",
            "unavailable",
            "no healthy providers",
            "timeout",
            "timed out",
            "connection",
            "connect",
            "dns",
            "refused",
            "reset",
            "network",
            "unreachable",
            "stream",
        ]) {
            Self::ProviderUnavailable
        } else {
            Self::Unknown
        }
    }

    /// 对工具执行返回的错误分类。超时的有副作用调用无法确认是否已生效，归为 [`FailureKind::Unknown`]。
    pub fn classify_tool_error(error: &Error, idempotency: Idempotency) -> Self {
        let message = error.to_string().to_lowercase();
        match error {
            Error::Validation(_) | Error::PermissionDenied(_) => Self::ToolRejected,
            Error::Tool(_) if message.contains("unknown tool") => Self::ToolRejected,
            Error::Tool(_) if message.contains("permission denied") => Self::ToolRejected,
            _ if message.contains("cancelled") || message.contains("aborted") => Self::Cancelled,
            Error::Timeout(_) => Self::timed_out(idempotency),
            _ if message.contains("timed out") => Self::timed_out(idempotency),
            _ => Self::ToolFailed,
        }
    }

    fn timed_out(idempotency: Idempotency) -> Self {
        if idempotency == Idempotency::Safe {
            Self::ToolTimedOut
        } else {
            Self::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_messages_map_to_distinct_kinds() {
        let cases = [
            ("HTTP 429 Too Many Requests", FailureKind::RateLimited),
            (
                "http_status=401 unauthorized",
                FailureKind::AuthenticationFailed,
            ),
            (
                "This model's maximum context length is 128000 tokens",
                FailureKind::ContextOverflow,
            ),
            (
                "http_status=503 overloaded",
                FailureKind::ProviderUnavailable,
            ),
            (
                "Stream timeout after 300 seconds",
                FailureKind::ProviderUnavailable,
            ),
            (
                "Operation cancelled: Agent was cancelled",
                FailureKind::Cancelled,
            ),
            ("model returned malformed payload", FailureKind::Unknown),
        ];
        for (message, expected) in cases {
            assert_eq!(
                FailureKind::classify_provider_message(message),
                expected,
                "{message}"
            );
        }
    }

    #[test]
    fn retry_depends_on_kind_and_idempotency() {
        use FailureKind::*;
        for kind in [
            ContextOverflow,
            AuthenticationFailed,
            ToolRejected,
            Cancelled,
        ] {
            assert!(!kind.retry_allowed(Idempotency::Safe), "{kind:?}");
        }
        for kind in [RateLimited, ProviderUnavailable] {
            assert!(kind.retry_allowed(Idempotency::Unknown), "{kind:?}");
        }
        for kind in [ToolFailed, ToolTimedOut, Unknown] {
            assert!(kind.retry_allowed(Idempotency::Safe), "{kind:?}");
            assert!(!kind.retry_allowed(Idempotency::Required), "{kind:?}");
            assert!(!kind.retry_allowed(Idempotency::Unknown), "{kind:?}");
        }
    }

    #[test]
    fn side_effecting_timeout_is_unknown_not_failed() {
        let timeout = Error::Timeout("Request timed out after 30 seconds".into());
        assert_eq!(
            FailureKind::classify_tool_error(&timeout, Idempotency::Required),
            FailureKind::Unknown
        );
        assert_eq!(
            FailureKind::classify_tool_error(&timeout, Idempotency::Safe),
            FailureKind::ToolTimedOut
        );
        assert_eq!(
            FailureKind::classify_tool_error(
                &Error::Tool("Unknown tool: nope".into()),
                Idempotency::Unknown
            ),
            FailureKind::ToolRejected
        );
        assert_eq!(
            FailureKind::classify_tool_error(&Error::Tool("boom".into()), Idempotency::Unknown),
            FailureKind::ToolFailed
        );
    }
}
