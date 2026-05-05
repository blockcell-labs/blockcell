//! 学习节流器 — 防止 review 风暴和并发 review 过载

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// 学习 review 操作的节流器
///
/// 防止：
/// - 并发 review 过多 (max_concurrent_reviews)
/// - review 触发过于频繁 (min_review_interval_secs)
pub struct LearningThrottle {
    last_review_completed: Mutex<Option<Instant>>,
    active_reviews: AtomicU32,
    max_concurrent_reviews: u32,
    min_review_interval_secs: u64,
}

impl LearningThrottle {
    pub fn new(max_concurrent_reviews: u32, min_review_interval_secs: u64) -> Self {
        Self {
            last_review_completed: Mutex::new(None),
            active_reviews: AtomicU32::new(0),
            max_concurrent_reviews,
            min_review_interval_secs,
        }
    }

    /// 原子性地尝试启动 review
    ///
    /// 当以下条件同时满足时返回 `true` 并递增活跃计数：
    /// - 未达到并发上限，且
    /// - 距上次 review 完成已超过冷却期
    ///
    /// 任一条件不满足时返回 `false`，不修改状态
    ///
    /// 替代了原先分离的 `can_start_review()` + `review_started()` 组合，
    /// 该组合存在 TOCTOU 竞态：两个线程可能同时看到 `can_start_review() == true`
    /// 然后都调用 `review_started()`，导致超过并发上限
    pub fn try_start_review(&self) -> bool {
        // 先检查冷却（代价低，Mutex 保护 — 无竞态）
        if let Ok(guard) = self.last_review_completed.lock() {
            if let Some(last) = *guard {
                if last.elapsed().as_secs() < self.min_review_interval_secs {
                    return false;
                }
            }
        } else {
            // Mutex 中毒 — 保守地阻止
            return false;
        }

        // 仅在未达上限时原子递增（CAS 循环）
        let mut current = self.active_reviews.load(Ordering::Acquire);
        loop {
            if current >= self.max_concurrent_reviews {
                return false;
            }
            match self.active_reviews.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    /// 检查是否可以启动新 review（只读）
    ///
    /// 注意：这是非原子检查。并发场景请使用 `try_start_review()`，
    /// 它原子地检查并递增
    pub fn can_start_review(&self) -> bool {
        // 检查并发上限
        if self.active_reviews.load(Ordering::Acquire) >= self.max_concurrent_reviews {
            return false;
        }

        // 检查冷却期
        if let Ok(guard) = self.last_review_completed.lock() {
            if let Some(last) = *guard {
                if last.elapsed().as_secs() < self.min_review_interval_secs {
                    return false;
                }
            }
        }

        true
    }

    /// 记录 review 已启动
    ///
    /// 注意：并发场景请优先使用 `try_start_review()`
    /// 此方法无条件递增计数器
    pub fn review_started(&self) {
        self.active_reviews.fetch_add(1, Ordering::AcqRel);
    }

    /// 记录 review 已完成
    pub fn review_completed(&self) {
        let mut current = self.active_reviews.load(Ordering::Acquire);
        while current > 0 {
            match self.active_reviews.compare_exchange_weak(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    if let Ok(mut guard) = self.last_review_completed.lock() {
                        *guard = Some(Instant::now());
                    }
                    return;
                }
                Err(actual) => current = actual,
            }
        }
    }

    /// 回滚 try_start_review 的递增（未实际启动 review 时调用）
    ///
    /// 仅递减计数器，不启动冷却计时器。
    /// 与 review_completed() 的区别：review_completed 表示一个真实的
    /// review 已完成，应启动冷却；rollback 表示获取了槽位但未使用，
    /// 不应影响后续 review 的启动时机。
    pub fn rollback_review(&self) {
        let mut current = self.active_reviews.load(Ordering::Acquire);
        while current > 0 {
            match self.active_reviews.compare_exchange_weak(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    /// 获取当前活跃 review 数量
    pub fn active_count(&self) -> u32 {
        self.active_reviews.load(Ordering::Acquire)
    }
}

impl std::fmt::Debug for LearningThrottle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LearningThrottle")
            .field(
                "active_reviews",
                &self.active_reviews.load(Ordering::Acquire),
            )
            .field("max_concurrent_reviews", &self.max_concurrent_reviews)
            .field("min_review_interval_secs", &self.min_review_interval_secs)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_can_start_when_idle() {
        let throttle = LearningThrottle::new(2, 0);
        assert!(throttle.can_start_review());
        assert!(throttle.try_start_review());
    }

    #[test]
    fn test_blocks_when_concurrent_limit_reached() {
        let throttle = LearningThrottle::new(2, 0);
        assert!(throttle.try_start_review());
        assert!(throttle.try_start_review());
        assert!(!throttle.try_start_review()); // 第三个被阻止
        assert_eq!(throttle.active_count(), 2);
    }

    #[test]
    fn test_allows_after_completion() {
        let throttle = LearningThrottle::new(1, 0);
        assert!(throttle.try_start_review());
        assert!(!throttle.try_start_review()); // 达到上限
        throttle.review_completed();
        assert!(throttle.try_start_review()); // 现在允许
    }

    #[test]
    fn test_cooldown_prevents_rapid_fire() {
        let throttle = LearningThrottle::new(2, 300);
        assert!(throttle.try_start_review());
        throttle.review_completed();
        // 完成后立即触发冷却
        assert!(!throttle.try_start_review());
    }

    #[test]
    fn test_completion_without_start_does_not_underflow() {
        let throttle = LearningThrottle::new(2, 300);
        throttle.review_completed();
        assert_eq!(throttle.active_count(), 0);
        assert!(throttle.can_start_review());
    }

    #[test]
    fn test_try_start_is_atomic() {
        let throttle = LearningThrottle::new(1, 0);
        // 第一次成功
        assert!(throttle.try_start_review());
        // 第二次失败（上限 = 1）
        assert!(!throttle.try_start_review());
        // 计数器恰好为 1
        assert_eq!(throttle.active_count(), 1);
    }
}
