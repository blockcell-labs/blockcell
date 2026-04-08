//! Circuit Breaker - Protects against cascading failures.
//!
//! Implements the circuit breaker pattern with three states:
//! - **Closed**: Normal operation, all requests pass through
//! - **Open**: Failing fast, all requests are rejected
//! - **HalfOpen**: Testing recovery, limited requests pass through

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Circuit breaker states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CircuitState {
    /// Normal state - requests pass through.
    Closed,
    /// Open state - requests are rejected.
    Open,
    /// Half-open state - testing recovery.
    HalfOpen,
}

/// Circuit breaker configuration.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Maximum consecutive failures before opening.
    pub max_failures: u64,
    /// Time to wait before transitioning to half-open.
    pub reset_timeout: Duration,
    /// Maximum calls allowed in half-open state.
    pub half_open_max_calls: u64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            max_failures: 3,
            reset_timeout: Duration::from_secs(60),
            half_open_max_calls: 1,
        }
    }
}

/// Circuit breaker with lock-free implementation.
///
/// Uses atomic operations for high-performance concurrent access.
pub struct CircuitBreaker {
    /// Current state: 0=Closed, 1=Open, 2=HalfOpen
    state: AtomicU8,
    /// Consecutive failure count.
    failure_count: AtomicU64,
    /// Last failure time as Unix nanoseconds.
    last_failure_time_ns: AtomicU64,
    /// Number of calls in half-open state.
    half_open_calls: AtomicU64,
    /// Configuration.
    config: CircuitBreakerConfig,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with the given configuration.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: AtomicU8::new(0),
            failure_count: AtomicU64::new(0),
            last_failure_time_ns: AtomicU64::new(0),
            half_open_calls: AtomicU64::new(0),
            config,
        }
    }

    /// Get current Unix nanoseconds timestamp.
    fn current_time_ns() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    }

    /// Check if a request should be allowed.
    ///
    /// Returns `true` if the request should proceed, `false` if it should be rejected.
    pub fn allow(&self) -> bool {
        let state = self.state.load(Ordering::Relaxed);
        match state {
            0 => true, // Closed
            1 => {
                // Open - check if timeout has passed
                let last_ns = self.last_failure_time_ns.load(Ordering::Relaxed);
                if last_ns > 0 {
                    let now_ns = Self::current_time_ns();
                    let elapsed_ns = now_ns.saturating_sub(last_ns);
                    let timeout_ns = self.config.reset_timeout.as_nanos() as u64;

                    if elapsed_ns >= timeout_ns {
                        // Transition to half-open
                        self.state.store(2, Ordering::Relaxed);
                        self.half_open_calls.store(0, Ordering::Relaxed);
                        tracing::info!(
                            target: "blockcell.session_metrics.circuit_breaker",
                            "Circuit breaker transitioned to HALF_OPEN"
                        );
                        return true;
                    }
                }
                false
            }
            2 => {
                // Half-open - allow limited calls
                let calls = self.half_open_calls.fetch_add(1, Ordering::Relaxed);
                calls < self.config.half_open_max_calls
            }
            _ => false,
        }
    }

    /// Record a successful operation.
    ///
    /// If in half-open state, transitions to closed.
    pub fn record_success(&self) {
        let state = self.state.load(Ordering::Relaxed);
        if state == 2 {
            // Half-open -> Closed
            self.state.store(0, Ordering::Relaxed);
            self.failure_count.store(0, Ordering::Relaxed);
            self.last_failure_time_ns.store(0, Ordering::Relaxed);
            tracing::info!(
                target: "blockcell.session_metrics.circuit_breaker",
                "Circuit breaker recovered to CLOSED state"
            );
        } else if state == 0 {
            // Reset failure count on success in closed state
            self.failure_count.store(0, Ordering::Relaxed);
        }
    }

    /// Record a failed operation.
    ///
    /// Increments failure count and may transition to open state.
    pub fn record_failure(&self) {
        let failures = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
        let state = self.state.load(Ordering::Relaxed);

        if state == 2 {
            // Half-open -> Open (failed during recovery)
            self.state.store(1, Ordering::Relaxed);
            self.last_failure_time_ns.store(Self::current_time_ns(), Ordering::Relaxed);
            tracing::warn!(
                target: "blockcell.session_metrics.circuit_breaker",
                "Circuit breaker returned to OPEN state after half-open failure"
            );
        } else if failures >= self.config.max_failures {
            // Closed -> Open
            self.state.store(1, Ordering::Relaxed);
            self.last_failure_time_ns.store(Self::current_time_ns(), Ordering::Relaxed);
            tracing::error!(
                target: "blockcell.session_metrics.circuit_breaker",
                failures = failures,
                max_failures = self.config.max_failures,
                "Circuit breaker tripped to OPEN state"
            );
        }
    }

    /// Get the current state.
    pub fn state(&self) -> CircuitState {
        match self.state.load(Ordering::Relaxed) {
            0 => CircuitState::Closed,
            1 => CircuitState::Open,
            2 => CircuitState::HalfOpen,
            _ => CircuitState::Closed,
        }
    }

    /// Get the current failure count.
    pub fn failure_count(&self) -> u64 {
        self.failure_count.load(Ordering::Relaxed)
    }

    /// Reset the circuit breaker to closed state.
    pub fn reset(&self) {
        self.state.store(0, Ordering::Relaxed);
        self.failure_count.store(0, Ordering::Relaxed);
        self.last_failure_time_ns.store(0, Ordering::Relaxed);
        self.half_open_calls.store(0, Ordering::Relaxed);
    }
}

// ============================================================================
// Global Circuit Breakers for Different Layers
// ============================================================================

/// Layer 4: Compact circuit breaker (快速恢复 - 用户在等待)
///
/// 用户同步操作，需要快速响应
/// 配置: max_failures=3, reset_timeout=60s
pub static COMPACT_CIRCUIT_BREAKER: OnceLock<CircuitBreaker> = OnceLock::new();

/// Layer 5: Memory Extraction circuit breaker (中等恢复 - 后台操作)
///
/// 后台异步操作，可接受较长恢复时间
/// 配置: max_failures=3, reset_timeout=300s (5 分钟)
pub static MEMORY_EXTRACTION_CIRCUIT_BREAKER: OnceLock<CircuitBreaker> = OnceLock::new();

/// Layer 6: Dream Consolidation circuit breaker (慢恢复 - 定时任务)
///
/// 定时后台任务，最长冷却期
/// 配置: max_failures=2, reset_timeout=900s (15 分钟)
pub static DREAM_CIRCUIT_BREAKER: OnceLock<CircuitBreaker> = OnceLock::new();

// ============================================================================
// Circuit Breaker Getter Functions
// ============================================================================

/// Get the global compact circuit breaker (Layer 4).
///
/// 配置: 连续 3 次失败后熔断，60 秒后尝试恢复
pub fn get_compact_circuit_breaker() -> &'static CircuitBreaker {
    COMPACT_CIRCUIT_BREAKER.get_or_init(|| {
        CircuitBreaker::new(CircuitBreakerConfig {
            max_failures: 3,
            reset_timeout: Duration::from_secs(60),
            half_open_max_calls: 1,
        })
    })
}

/// Get the global memory extraction circuit breaker (Layer 5).
///
/// 配置: 连续 3 次失败后熔断，300 秒 (5 分钟) 后尝试恢复
pub fn get_memory_extraction_circuit_breaker() -> &'static CircuitBreaker {
    MEMORY_EXTRACTION_CIRCUIT_BREAKER.get_or_init(|| {
        CircuitBreaker::new(CircuitBreakerConfig {
            max_failures: 3,
            reset_timeout: Duration::from_secs(300),
            half_open_max_calls: 1,
        })
    })
}

/// Get the global dream consolidation circuit breaker (Layer 6).
///
/// 配置: 连续 2 次失败后熔断，900 秒 (15 分钟) 后尝试恢复
/// 失败可能意味着系统性问题，需要更长恢复时间
pub fn get_dream_circuit_breaker() -> &'static CircuitBreaker {
    DREAM_CIRCUIT_BREAKER.get_or_init(|| {
        CircuitBreaker::new(CircuitBreakerConfig {
            max_failures: 2,
            reset_timeout: Duration::from_secs(900),
            half_open_max_calls: 1,
        })
    })
}

/// Reset all circuit breakers to closed state.
pub fn reset_all_circuit_breakers() {
    get_compact_circuit_breaker().reset();
    get_memory_extraction_circuit_breaker().reset();
    get_dream_circuit_breaker().reset();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_circuit_breaker_closed_state() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::default());

        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow());

        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_open_state() {
        let config = CircuitBreakerConfig {
            max_failures: 2,
            reset_timeout: Duration::from_millis(100),
            half_open_max_calls: 1,
        };
        let cb = CircuitBreaker::new(config);

        // First failure
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        // Second failure -> Open
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allow());
    }

    #[test]
    fn test_circuit_breaker_half_open_state() {
        let config = CircuitBreakerConfig {
            max_failures: 1,
            reset_timeout: Duration::from_millis(50),
            half_open_max_calls: 1,
        };
        let cb = CircuitBreaker::new(config);

        // Trigger open
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for timeout
        thread::sleep(Duration::from_millis(100));

        // Should transition to half-open on next allow
        assert!(cb.allow());
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn test_circuit_breaker_recovery() {
        let config = CircuitBreakerConfig {
            max_failures: 1,
            reset_timeout: Duration::from_millis(50),
            half_open_max_calls: 1,
        };
        let cb = CircuitBreaker::new(config);

        // Trigger open
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for timeout
        thread::sleep(Duration::from_millis(100));

        // Allow and succeed -> Closed
        assert!(cb.allow());
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_half_open_failure() {
        let config = CircuitBreakerConfig {
            max_failures: 1,
            reset_timeout: Duration::from_millis(50),
            half_open_max_calls: 1,
        };
        let cb = CircuitBreaker::new(config);

        // Trigger open
        cb.record_failure();

        // Wait for timeout
        thread::sleep(Duration::from_millis(100));

        // Allow and fail -> Open
        assert!(cb.allow());
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    // ============================================================================
    // Multi-layer Circuit Breaker Tests
    // ============================================================================

    #[test]
    fn test_global_circuit_breakers_configurations() {
        // 验证所有全局熔断器初始状态为 Closed
        let compact_cb = get_compact_circuit_breaker();
        assert_eq!(compact_cb.state(), CircuitState::Closed);

        let extraction_cb = get_memory_extraction_circuit_breaker();
        assert_eq!(extraction_cb.state(), CircuitState::Closed);

        let dream_cb = get_dream_circuit_breaker();
        assert_eq!(dream_cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_reset_all_circuit_breakers() {
        // 触发所有熔断器
        for _ in 0..3 {
            get_compact_circuit_breaker().record_failure();
            get_memory_extraction_circuit_breaker().record_failure();
        }
        for _ in 0..2 {
            get_dream_circuit_breaker().record_failure();
        }

        // 验证全部进入 Open 状态
        assert_eq!(get_compact_circuit_breaker().state(), CircuitState::Open);
        assert_eq!(get_memory_extraction_circuit_breaker().state(), CircuitState::Open);
        assert_eq!(get_dream_circuit_breaker().state(), CircuitState::Open);

        // 重置所有熔断器
        reset_all_circuit_breakers();

        // 验证全部回到 Closed 状态
        assert_eq!(get_compact_circuit_breaker().state(), CircuitState::Closed);
        assert_eq!(get_memory_extraction_circuit_breaker().state(), CircuitState::Closed);
        assert_eq!(get_dream_circuit_breaker().state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_independence() {
        // 首先重置所有熔断器确保初始状态
        reset_all_circuit_breakers();

        // 仅触发 Layer 4 Compact 熔断器 (max_failures=3)
        let compact_cb = get_compact_circuit_breaker();
        compact_cb.record_failure();
        compact_cb.record_failure();
        compact_cb.record_failure();

        // 验证 Layer 4 进入 Open 状态
        assert_eq!(compact_cb.state(), CircuitState::Open);

        // 验证 Layer 5/6 保持 Closed 状态（独立性）
        assert_eq!(
            get_memory_extraction_circuit_breaker().state(),
            CircuitState::Closed
        );
        assert_eq!(get_dream_circuit_breaker().state(), CircuitState::Closed);

        // 清理：重置所有熔断器
        reset_all_circuit_breakers();
    }
}