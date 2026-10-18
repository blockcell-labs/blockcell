use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const MICRO_USD_PER_CENT: u64 = 10_000;
const MICRO_USD_PER_USD: u64 = 1_000_000;

/// Snapshot of current token and cost budget usage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BudgetSnapshot {
    pub input_tokens_used: u64,
    pub output_tokens_used: u64,
    pub total_tokens_used: u64,
    pub cost_used_micro_usd: u64,
    /// 0 means unlimited or not configured.
    pub tokens_remaining: u64,
    /// 0 means unlimited or not configured.
    pub cost_remaining_micro_usd: u64,
    /// Highest active budget usage ratio. 0.0 means no active limits.
    pub usage_ratio: f64,
}

/// Snapshot of the currently assembled model context, independent from
/// cumulative billed token usage across the session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextWindowSnapshot {
    pub tokens_used: u64,
    pub token_limit: u64,
    pub tokens_remaining: u64,
    pub compact_threshold_tokens: u64,
}

/// Error returned when a token or cost budget has been exhausted.
#[derive(Debug, Clone)]
pub struct BudgetExhaustedError {
    pub message: String,
    pub snapshot: BudgetSnapshot,
}

impl std::fmt::Display for BudgetExhaustedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for BudgetExhaustedError {}

/// Per-session token and cost budget configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BudgetConfig {
    /// Max input + output tokens per session. 0 means unlimited.
    #[serde(default)]
    pub max_tokens_per_session: u64,
    /// Max cost per session, in cents. 0 means unlimited.
    #[serde(default)]
    pub max_cost_cents_per_session: u64,
    /// Warning ratio before exhaustion. Defaults to 0.8.
    #[serde(default = "default_warning_threshold")]
    pub warning_threshold: f64,
}

fn default_warning_threshold() -> f64 {
    0.8
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_tokens_per_session: 0,
            max_cost_cents_per_session: 0,
            warning_threshold: default_warning_threshold(),
        }
    }
}

/// Thread-safe tracker for per-session token and cost budget usage.
///
/// 子 Agent 的 tracker 通过 [`BudgetTracker::with_parent`] 挂到父会话 tracker 上：
/// 记录用量时同时累加到自身与所有祖先，从而让"总预算"同时约束父与所有子 Agent，
/// 同时子 tracker 自身的计数仍然保留，可用于按子任务归因。
pub struct BudgetTracker {
    max_tokens: u64,
    max_cost_micro_usd: u64,
    input_tokens: AtomicU64,
    output_tokens: AtomicU64,
    cost_micro_usd: AtomicU64,
    context_tokens: AtomicU64,
    context_token_limit: AtomicU64,
    compact_threshold_tokens: AtomicU64,
    warning_threshold: f64,
    /// 上级预算（子 Agent 归并）。`None` 表示顶层会话预算。
    parent: Option<BudgetTrackerHandle>,
}

impl std::fmt::Debug for BudgetTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 手动实现，避免沿 parent 链递归打印。
        f.debug_struct("BudgetTracker")
            .field("max_tokens", &self.max_tokens)
            .field("max_cost_micro_usd", &self.max_cost_micro_usd)
            .field("has_parent", &self.parent.is_some())
            .finish()
    }
}

impl BudgetTracker {
    pub fn new(config: &BudgetConfig) -> Self {
        Self::with_parent(config, None)
    }

    /// 构造一个挂到 `parent` 下的 tracker；用量会同时累加到 `parent`。
    pub fn with_parent(config: &BudgetConfig, parent: Option<BudgetTrackerHandle>) -> Self {
        let warning_threshold =
            if config.warning_threshold.is_finite() && config.warning_threshold > 0.0 {
                config.warning_threshold
            } else {
                default_warning_threshold()
            };

        Self {
            max_tokens: config.max_tokens_per_session,
            max_cost_micro_usd: config
                .max_cost_cents_per_session
                .saturating_mul(MICRO_USD_PER_CENT),
            input_tokens: AtomicU64::new(0),
            output_tokens: AtomicU64::new(0),
            cost_micro_usd: AtomicU64::new(0),
            context_tokens: AtomicU64::new(0),
            context_token_limit: AtomicU64::new(0),
            compact_threshold_tokens: AtomicU64::new(0),
            warning_threshold,
            parent,
        }
    }

    /// 该 tracker 是否归并到上级预算。
    pub fn has_parent(&self) -> bool {
        self.parent.is_some()
    }

    /// Record usage for one LLM call and return the current snapshot.
    ///
    /// 归并到父预算时，同一笔用量会向上累加，使父会话的 `snapshot` / `is_exhausted`
    /// 同时反映所有子 Agent 的消耗。
    pub fn record_usage(&self, input: u64, output: u64, cost_micro_usd: u64) -> BudgetSnapshot {
        self.input_tokens.fetch_add(input, Ordering::Relaxed);
        self.output_tokens.fetch_add(output, Ordering::Relaxed);
        self.cost_micro_usd
            .fetch_add(cost_micro_usd, Ordering::Relaxed);
        if let Some(parent) = &self.parent {
            parent.record_usage(input, output, cost_micro_usd);
        }
        self.snapshot()
    }

    /// Return current budget usage.
    pub fn snapshot(&self) -> BudgetSnapshot {
        let input = self.input_tokens.load(Ordering::Relaxed);
        let output = self.output_tokens.load(Ordering::Relaxed);
        let total = input.saturating_add(output);
        let cost = self.cost_micro_usd.load(Ordering::Relaxed);

        let tokens_remaining = if self.max_tokens > 0 {
            self.max_tokens.saturating_sub(total)
        } else {
            0
        };
        let cost_remaining_micro_usd = if self.max_cost_micro_usd > 0 {
            self.max_cost_micro_usd.saturating_sub(cost)
        } else {
            0
        };
        let token_ratio = if self.max_tokens > 0 {
            total as f64 / self.max_tokens as f64
        } else {
            0.0
        };
        let cost_ratio = if self.max_cost_micro_usd > 0 {
            cost as f64 / self.max_cost_micro_usd as f64
        } else {
            0.0
        };

        BudgetSnapshot {
            input_tokens_used: input,
            output_tokens_used: output,
            total_tokens_used: total,
            cost_used_micro_usd: cost,
            tokens_remaining,
            cost_remaining_micro_usd,
            usage_ratio: token_ratio.max(cost_ratio),
        }
    }

    /// Record the latest assembled context estimate for model self-inspection.
    pub fn record_context_window(
        &self,
        tokens_used: u64,
        token_limit: u64,
        compact_threshold_tokens: u64,
    ) -> ContextWindowSnapshot {
        self.context_tokens.store(tokens_used, Ordering::Relaxed);
        self.context_token_limit
            .store(token_limit, Ordering::Relaxed);
        self.compact_threshold_tokens
            .store(compact_threshold_tokens, Ordering::Relaxed);
        self.context_window_snapshot()
    }

    /// Return the latest current-context estimate without changing cumulative usage.
    pub fn context_window_snapshot(&self) -> ContextWindowSnapshot {
        let tokens_used = self.context_tokens.load(Ordering::Relaxed);
        let token_limit = self.context_token_limit.load(Ordering::Relaxed);
        ContextWindowSnapshot {
            tokens_used,
            token_limit,
            tokens_remaining: token_limit.saturating_sub(tokens_used),
            compact_threshold_tokens: self.compact_threshold_tokens.load(Ordering::Relaxed),
        }
    }

    /// 仅本 tracker 自身的额度是否已用尽（不含父预算）。
    fn local_exhausted(&self) -> bool {
        let snapshot = self.snapshot();
        (self.max_tokens > 0 && snapshot.total_tokens_used >= self.max_tokens)
            || (self.max_cost_micro_usd > 0
                && snapshot.cost_used_micro_usd >= self.max_cost_micro_usd)
    }

    /// Return true if this tracker's own limit or any ancestor's limit has been reached.
    pub fn is_exhausted(&self) -> bool {
        self.local_exhausted()
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.is_exhausted())
    }

    /// Return true when usage has crossed the warning threshold but is not exhausted.
    pub fn should_warn(&self) -> bool {
        let snapshot = self.snapshot();
        snapshot.usage_ratio >= self.warning_threshold && !self.is_exhausted()
    }

    /// Check the active budget and return an error if it has been exhausted.
    ///
    /// 子 Agent 的额度耗尽既可能来自自身，也可能来自父会话的总预算；两种情况
    /// 分别给出可区分的错误信息，便于调用方决定是终止子任务还是整体停止。
    pub fn check_budget(&self) -> Result<BudgetSnapshot, BudgetExhaustedError> {
        let snapshot = self.snapshot();
        if !self.is_exhausted() {
            return Ok(snapshot);
        }

        if !self.local_exhausted() {
            return Err(BudgetExhaustedError {
                message: "Shared session budget exhausted (parent budget reached)".to_string(),
                snapshot,
            });
        }

        let mut parts = Vec::new();
        if self.max_tokens > 0 {
            parts.push(format!(
                "tokens: {}/{}",
                snapshot.total_tokens_used, self.max_tokens
            ));
        }
        if self.max_cost_micro_usd > 0 {
            parts.push(format!(
                "cost: {}/{}",
                format_micro_usd(snapshot.cost_used_micro_usd),
                format_micro_usd(self.max_cost_micro_usd)
            ));
        }

        Err(BudgetExhaustedError {
            message: format!("Budget exhausted ({})", parts.join(", ")),
            snapshot,
        })
    }

    /// Reset all accumulated usage.
    /// 仅重置本 tracker 的原子量，不沿 parent 链向上重置。
    pub fn reset(&self) {
        self.input_tokens.store(0, Ordering::Relaxed);
        self.output_tokens.store(0, Ordering::Relaxed);
        self.cost_micro_usd.store(0, Ordering::Relaxed);
        self.context_tokens.store(0, Ordering::Relaxed);
        self.context_token_limit.store(0, Ordering::Relaxed);
        self.compact_threshold_tokens.store(0, Ordering::Relaxed);
    }
}

/// Shareable budget tracker handle.
pub type BudgetTrackerHandle = Arc<BudgetTracker>;

fn format_micro_usd(value: u64) -> String {
    let usd = value / MICRO_USD_PER_USD;
    let micros = value % MICRO_USD_PER_USD;
    if micros.is_multiple_of(MICRO_USD_PER_CENT) {
        format!("${}.{:02}", usd, micros / MICRO_USD_PER_CENT)
    } else {
        format!("${}.{:06}", usd, micros)
    }
}
