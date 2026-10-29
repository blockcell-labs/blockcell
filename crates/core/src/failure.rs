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
    /// 请求本身不合法（参数、格式），原样重试不会成功。
    InvalidRequest,
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

/// Runtime 对一次工具调用执行状态的判断，与返回给模型的内容（ToolResult）是两个概念。
///
/// Retry / Audit / Evaluation / Ghost 都依赖这一稳定语义，而不是解析结果文本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    Succeeded,
    /// 工具已执行并明确返回失败。
    Failed,
    /// 执行前被拒绝（作用域、策略、校验、权限、调用上限），确定没有副作用。
    Rejected,
    Cancelled,
    /// 超时且调用可安全重试（只读 / 幂等）。
    TimedOut,
    /// 模型提议了该调用，但 Runtime 没有开始执行（同批次中断、被其他调用取代）。
    NotStarted,
    /// 无法确定调用是否已生效。
    Unknown,
}

impl ToolOutcome {
    /// `None` 表示执行成功。
    pub fn from_failure(failure: Option<FailureKind>) -> Self {
        match failure {
            None => Self::Succeeded,
            Some(FailureKind::ToolRejected) => Self::Rejected,
            Some(FailureKind::ToolTimedOut) => Self::TimedOut,
            Some(FailureKind::Cancelled) => Self::Cancelled,
            Some(FailureKind::Unknown) => Self::Unknown,
            Some(_) => Self::Failed,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::NotStarted => "not_started",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_error(self) -> bool {
        self != Self::Succeeded
    }

    /// 失败后能否自动重试同一调用；成功的调用返回 `None`。
    ///
    /// 未开始的调用没有任何副作用，总是可以重试；其余按 [`FailureKind::retry_allowed`]。
    pub fn retry_allowed(self, idempotency: Idempotency) -> Option<bool> {
        let failure = match self {
            Self::Succeeded => return None,
            Self::NotStarted => return Some(true),
            Self::Failed => FailureKind::ToolFailed,
            Self::Rejected => FailureKind::ToolRejected,
            Self::Cancelled => FailureKind::Cancelled,
            Self::TimedOut => FailureKind::ToolTimedOut,
            Self::Unknown => FailureKind::Unknown,
        };
        Some(failure.retry_allowed(idempotency))
    }
}

/// 模型请求失败后的恢复策略。Provider 只报告错误，由 Runtime 依据失败类型决定是否继续。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestRecovery {
    /// 本轮尝试内换下一个 provider（ModelRouter fallback）。
    pub switch_provider: bool,
    pub retry: RequestRetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestRetry {
    /// 原样重试不会成功；压缩上下文后重新发起本步。
    CompactFirst,
    /// 指数退避后重试。
    Backoff,
    /// 原因不明，保守地最多再试一次。
    Once,
    Never,
}

impl FailureKind {
    /// 模型请求失败时的恢复策略。
    pub fn request_recovery(self) -> RequestRecovery {
        let (switch_provider, retry) = match self {
            Self::ContextOverflow => (false, RequestRetry::CompactFirst),
            Self::RateLimited | Self::ProviderUnavailable => (true, RequestRetry::Backoff),
            // 凭据只对当前 provider 失效：换 provider，但不在退避后重试。
            Self::AuthenticationFailed => (true, RequestRetry::Never),
            Self::Unknown => (false, RequestRetry::Once),
            Self::InvalidRequest
            | Self::Cancelled
            | Self::ToolRejected
            | Self::ToolFailed
            | Self::ToolTimedOut => (false, RequestRetry::Never),
        };
        RequestRecovery {
            switch_provider,
            retry,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContextOverflow => "context_overflow",
            Self::RateLimited => "rate_limited",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::AuthenticationFailed => "authentication_failed",
            Self::InvalidRequest => "invalid_request",
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
            Self::ContextOverflow
            | Self::AuthenticationFailed
            | Self::InvalidRequest
            | Self::ToolRejected => false,
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
            "http_status=400",
            "http 400",
            "invalid_request",
            "invalid request",
            "bad request",
        ]) {
            Self::InvalidRequest
        } else if has(&[
            "http_status=5",
            "http 5",
            "llm content filter rejected the request",
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
            (
                "http_status=400 invalid_request_error: messages.0.content is required",
                FailureKind::InvalidRequest,
            ),
            (
                "http_status=400 invalid_request_error: prompt is too long",
                FailureKind::ContextOverflow,
            ),
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
    fn content_filter_uses_backoff_and_provider_fallback() {
        let kind = FailureKind::classify_provider_message(
            "Provider error: LLM content filter rejected the request",
        );
        assert_eq!(kind, FailureKind::ProviderUnavailable);
        let recovery = kind.request_recovery();
        assert!(recovery.switch_provider);
        assert_eq!(recovery.retry, RequestRetry::Backoff);
    }

    #[test]
    fn request_recovery_follows_failure_kind() {
        use FailureKind::*;
        let recovery = |kind: FailureKind| {
            let r = kind.request_recovery();
            (r.switch_provider, r.retry)
        };
        assert_eq!(
            recovery(ContextOverflow),
            (false, RequestRetry::CompactFirst)
        );
        assert_eq!(recovery(RateLimited), (true, RequestRetry::Backoff));
        assert_eq!(recovery(ProviderUnavailable), (true, RequestRetry::Backoff));
        assert_eq!(recovery(AuthenticationFailed), (true, RequestRetry::Never));
        assert_eq!(recovery(InvalidRequest), (false, RequestRetry::Never));
        assert_eq!(recovery(Cancelled), (false, RequestRetry::Never));
        assert_eq!(recovery(Unknown), (false, RequestRetry::Once));
    }

    #[test]
    fn retry_depends_on_kind_and_idempotency() {
        use FailureKind::*;
        for kind in [
            ContextOverflow,
            AuthenticationFailed,
            InvalidRequest,
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
    fn tool_outcome_maps_failures_and_retry_semantics() {
        use FailureKind as F;
        let cases = [
            (None, ToolOutcome::Succeeded),
            (Some(F::ToolRejected), ToolOutcome::Rejected),
            (Some(F::ToolFailed), ToolOutcome::Failed),
            (Some(F::ToolTimedOut), ToolOutcome::TimedOut),
            (Some(F::Cancelled), ToolOutcome::Cancelled),
            (Some(F::Unknown), ToolOutcome::Unknown),
            (Some(F::ProviderUnavailable), ToolOutcome::Failed),
        ];
        for (failure, expected) in cases {
            assert_eq!(ToolOutcome::from_failure(failure), expected, "{failure:?}");
        }
        assert!(!ToolOutcome::Succeeded.is_error());
        assert_eq!(
            ToolOutcome::Succeeded.retry_allowed(Idempotency::Safe),
            None
        );
        assert_eq!(
            ToolOutcome::NotStarted.retry_allowed(Idempotency::Required),
            Some(true)
        );
        assert_eq!(
            ToolOutcome::Unknown.retry_allowed(Idempotency::Required),
            Some(false)
        );
        assert_eq!(
            ToolOutcome::Rejected.retry_allowed(Idempotency::Safe),
            Some(false)
        );
        assert_eq!(
            serde_json::to_value(ToolOutcome::NotStarted).unwrap(),
            "not_started"
        );
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
