/// Simple token-bucket rate limiter for outbound channel messages.
///
/// Each channel gets its own `RateLimiter` instance. Callers `await` on
/// `acquire()` before sending; the call returns immediately when a token is
/// available, or sleeps until the next refill tick.
///
/// Default limits (conservative, well within free-tier quotas):
/// - Telegram : 30 msg/s  (Bot API limit is ~30/s per bot)
/// - Slack    :  1 msg/s  (Tier-1 chat.postMessage)
/// - Discord  :  5 msg/s  (REST channel message endpoint)
/// - Feishu   :  5 msg/s  (Open Platform recommendation)
/// - WhatsApp :  2 msg/s  (bridge-dependent, conservative)
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub struct RateLimiter {
    /// Maximum tokens in the bucket (= burst capacity).
    capacity: u32,
    /// Current available tokens.
    tokens: f64,
    /// Tokens added per second.
    refill_rate: f64,
    /// Last time tokens were refilled.
    last_refill: Instant,
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// * `capacity`    – burst capacity (max tokens)
    /// * `per_second`  – sustained rate (tokens/second)
    pub fn new(capacity: u32, per_second: f64) -> Self {
        Self {
            capacity,
            tokens: capacity as f64,
            refill_rate: per_second,
            last_refill: Instant::now(),
        }
    }

    /// Refill tokens based on elapsed time.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity as f64);
        self.last_refill = now;
    }

    /// Try to consume one token. Returns the wait duration if no token is available.
    fn try_consume(&mut self) -> Option<Duration> {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            None
        } else {
            let needed = 1.0 - self.tokens;
            let wait_secs = needed / self.refill_rate;
            Some(Duration::from_secs_f64(wait_secs))
        }
    }
}

/// Thread-safe wrapper around `RateLimiter`.
pub struct ChannelRateLimiter(Mutex<RateLimiter>);

impl ChannelRateLimiter {
    pub fn new(capacity: u32, per_second: f64) -> Self {
        Self(Mutex::new(RateLimiter::new(capacity, per_second)))
    }

    /// Acquire one send token, sleeping if necessary.
    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut inner = self.0.lock().await;
                inner.try_consume()
            };
            match wait {
                None => return,
                Some(d) => tokio::time::sleep(d).await,
            }
        }
    }
}

/// Per-channel default rate limiters (process-global singletons).
///
/// Default limits (conservative, well within free-tier quotas):
/// - DingTalk  : 20 msg/s  (企业内部应用 limit)
/// - WeCom     : 20 msg/s  (企业微信 message API limit)
use std::sync::OnceLock;

static TELEGRAM_RL: OnceLock<ChannelRateLimiter> = OnceLock::new();
static SLACK_RL: OnceLock<ChannelRateLimiter> = OnceLock::new();
static DISCORD_RL: OnceLock<ChannelRateLimiter> = OnceLock::new();
static FEISHU_RL: OnceLock<ChannelRateLimiter> = OnceLock::new();
static WHATSAPP_RL: OnceLock<ChannelRateLimiter> = OnceLock::new();
static DINGTALK_RL: OnceLock<ChannelRateLimiter> = OnceLock::new();
static WECOM_RL: OnceLock<ChannelRateLimiter> = OnceLock::new();
static LARK_RL: OnceLock<ChannelRateLimiter> = OnceLock::new();

pub fn telegram_limiter() -> &'static ChannelRateLimiter {
    TELEGRAM_RL.get_or_init(|| ChannelRateLimiter::new(30, 30.0))
}

pub fn slack_limiter() -> &'static ChannelRateLimiter {
    SLACK_RL.get_or_init(|| ChannelRateLimiter::new(3, 1.0))
}

pub fn discord_limiter() -> &'static ChannelRateLimiter {
    DISCORD_RL.get_or_init(|| ChannelRateLimiter::new(5, 5.0))
}

pub fn feishu_limiter() -> &'static ChannelRateLimiter {
    FEISHU_RL.get_or_init(|| ChannelRateLimiter::new(5, 5.0))
}

pub fn whatsapp_limiter() -> &'static ChannelRateLimiter {
    WHATSAPP_RL.get_or_init(|| ChannelRateLimiter::new(2, 2.0))
}

pub fn dingtalk_limiter() -> &'static ChannelRateLimiter {
    DINGTALK_RL.get_or_init(|| ChannelRateLimiter::new(20, 20.0))
}

pub fn wecom_limiter() -> &'static ChannelRateLimiter {
    WECOM_RL.get_or_init(|| ChannelRateLimiter::new(20, 20.0))
}

pub fn lark_limiter() -> &'static ChannelRateLimiter {
    LARK_RL.get_or_init(|| ChannelRateLimiter::new(5, 5.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_bucket_immediate() {
        let mut rl = RateLimiter::new(5, 5.0);
        // First 5 tokens should be available immediately
        for _ in 0..5 {
            assert!(rl.try_consume().is_none());
        }
    }

    #[test]
    fn test_token_bucket_exhausted() {
        let mut rl = RateLimiter::new(2, 1.0);
        assert!(rl.try_consume().is_none());
        assert!(rl.try_consume().is_none());
        // Bucket empty — should return a wait duration
        let wait = rl.try_consume();
        assert!(wait.is_some());
        assert!(wait.unwrap().as_secs_f64() > 0.0);
    }

    #[tokio::test]
    async fn test_channel_rate_limiter_acquire() {
        let limiter = ChannelRateLimiter::new(3, 100.0); // high rate so test is fast
        // Should acquire 3 tokens without sleeping
        for _ in 0..3 {
            limiter.acquire().await;
        }
    }
}
