use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use blockcell_core::Paths;
use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkPromptCacheUsageSnapshot {
    pub prompt_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

#[derive(Debug, Default)]
pub struct GhostMetrics {
    episodes_captured: AtomicU64,
    reviews_started: AtomicU64,
    reviews_failed: AtomicU64,
    dead_letters: AtomicU64,
    prompt_input_tokens: AtomicU64,
    cache_read_input_tokens: AtomicU64,
    cache_creation_input_tokens: AtomicU64,
    fork_alignment_misses: AtomicU64,
    fork_prompt_cache_usage: Mutex<HashMap<String, ForkPromptCacheUsageSnapshot>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhostMetricsSnapshot {
    pub episodes_captured: u64,
    pub reviews_started: u64,
    pub reviews_failed: u64,
    pub dead_letters: u64,
    pub prompt_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub fork_alignment_misses: u64,
    pub fork_prompt_cache_usage: HashMap<String, ForkPromptCacheUsageSnapshot>,
}

impl GhostMetrics {
    pub fn record_episode_captured(&self) {
        self.episodes_captured.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_review_started(&self) {
        self.reviews_started.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_review_failed(&self) {
        self.reviews_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_dead_letter(&self) {
        self.dead_letters.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_fork_alignment_miss(&self) {
        self.fork_alignment_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_prompt_cache_usage(
        &self,
        prompt_input_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
    ) {
        self.prompt_input_tokens
            .fetch_add(prompt_input_tokens, Ordering::Relaxed);
        self.cache_read_input_tokens
            .fetch_add(cache_read_input_tokens, Ordering::Relaxed);
        self.cache_creation_input_tokens
            .fetch_add(cache_creation_input_tokens, Ordering::Relaxed);
    }

    pub fn record_fork_prompt_cache_usage(
        &self,
        fork_label: &str,
        prompt_input_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
    ) {
        self.record_prompt_cache_usage(
            prompt_input_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        );
        let mut guard = self
            .fork_prompt_cache_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let entry = guard.entry(fork_label.to_string()).or_default();
        entry.prompt_input_tokens += prompt_input_tokens;
        entry.cache_read_input_tokens += cache_read_input_tokens;
        entry.cache_creation_input_tokens += cache_creation_input_tokens;
    }

    pub fn snapshot(&self) -> GhostMetricsSnapshot {
        let fork_prompt_cache_usage = self
            .fork_prompt_cache_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        GhostMetricsSnapshot {
            episodes_captured: self.episodes_captured.load(Ordering::Relaxed),
            reviews_started: self.reviews_started.load(Ordering::Relaxed),
            reviews_failed: self.reviews_failed.load(Ordering::Relaxed),
            dead_letters: self.dead_letters.load(Ordering::Relaxed),
            prompt_input_tokens: self.prompt_input_tokens.load(Ordering::Relaxed),
            cache_read_input_tokens: self.cache_read_input_tokens.load(Ordering::Relaxed),
            cache_creation_input_tokens: self.cache_creation_input_tokens.load(Ordering::Relaxed),
            fork_alignment_misses: self.fork_alignment_misses.load(Ordering::Relaxed),
            fork_prompt_cache_usage,
        }
    }

    /// Reset all counters. Uses SeqCst ordering to establish a synchronization
    /// point, ensuring concurrent snapshot() calls see either fully-reset or
    /// fully-not-reset state (not partial reset).
    pub fn reset(&self) {
        self.episodes_captured.store(0, Ordering::SeqCst);
        self.reviews_started.store(0, Ordering::SeqCst);
        self.reviews_failed.store(0, Ordering::SeqCst);
        self.dead_letters.store(0, Ordering::SeqCst);
        self.prompt_input_tokens.store(0, Ordering::SeqCst);
        self.cache_read_input_tokens.store(0, Ordering::SeqCst);
        self.cache_creation_input_tokens.store(0, Ordering::SeqCst);
        self.fork_alignment_misses.store(0, Ordering::SeqCst);
        self.fork_prompt_cache_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

fn metrics_registry() -> &'static Mutex<HashMap<String, Arc<GhostMetrics>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<GhostMetrics>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn registry_key(paths: &Paths) -> String {
    paths.base.display().to_string()
}

pub fn get_ghost_metrics(paths: &Paths) -> Arc<GhostMetrics> {
    let key = registry_key(paths);
    let mut registry = metrics_registry()
        .lock()
        .expect("ghost metrics registry lock poisoned");
    registry
        .entry(key)
        .or_insert_with(|| Arc::new(GhostMetrics::default()))
        .clone()
}

pub fn ghost_metrics_summary(paths: &Paths) -> GhostMetricsSnapshot {
    get_ghost_metrics(paths).snapshot()
}

pub fn reset_ghost_metrics_for_paths(paths: &Paths) {
    get_ghost_metrics(paths).reset();
}

/// 由 `run_forked_agent` 统一调用，将所有类型的 Fork（review / auto_memory / compact_summary /
/// session_memory / fork / typed subagent）缓存与输入 token 按 `fork_label` 汇入 GhostMetrics。
pub fn record_fork_prompt_cache_usage_from_fork(
    path_hint: Option<&std::path::Path>,
    fork_label: &str,
    prompt_input_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
) {
    let registry = metrics_registry()
        .lock()
        .expect("ghost metrics registry lock poisoned");
    if let Some(hint) = path_hint {
        if let Some((_, metrics)) = registry
            .iter()
            .filter(|(base, _)| hint.starts_with(std::path::Path::new(base.as_str())))
            .max_by_key(|(base, _)| base.len())
        {
            metrics.record_fork_prompt_cache_usage(
                fork_label,
                prompt_input_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
            );
            return;
        }
    }
    for metrics in registry.values() {
        metrics.record_fork_prompt_cache_usage(
            fork_label,
            prompt_input_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
        );
    }
}

/// 记录 Fork 缓存对齐未命中/降级事件（C10：降级而非静默）。
pub fn record_fork_alignment_miss_from_fork(
    path_hint: Option<&std::path::Path>,
    fork_label: &str,
    reason: &str,
) {
    tracing::warn!(
        fork_label,
        reason,
        "[forked_agent] Prompt cache alignment miss, degrading to cold request"
    );
    let registry = metrics_registry()
        .lock()
        .expect("ghost metrics registry lock poisoned");
    if let Some(hint) = path_hint {
        if let Some((_, metrics)) = registry
            .iter()
            .filter(|(base, _)| hint.starts_with(std::path::Path::new(base.as_str())))
            .max_by_key(|(base, _)| base.len())
        {
            metrics.record_fork_alignment_miss();
            return;
        }
    }
    for metrics in registry.values() {
        metrics.record_fork_alignment_miss();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_cache_usage_is_exposed_in_snapshot() {
        let metrics = GhostMetrics::default();
        metrics.record_prompt_cache_usage(200, 140, 20);
        metrics.record_fork_prompt_cache_usage("skill_nudge_review", 100, 80, 10);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.prompt_input_tokens, 300);
        assert_eq!(snapshot.cache_read_input_tokens, 220);
        assert_eq!(snapshot.cache_creation_input_tokens, 30);
        let fork_bucket = snapshot
            .fork_prompt_cache_usage
            .get("skill_nudge_review")
            .expect("fork bucket should exist");
        assert_eq!(fork_bucket.prompt_input_tokens, 100);
        assert_eq!(fork_bucket.cache_read_input_tokens, 80);
        assert_eq!(fork_bucket.cache_creation_input_tokens, 10);
    }

    #[test]
    fn fork_usage_helper_routes_all_fork_labels_to_matching_paths_metrics() {
        let base = std::env::temp_dir().join(format!(
            "blockcell-ghost-metrics-fork-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = Paths::with_base(base.clone());
        reset_ghost_metrics_for_paths(&paths);

        let memory_dir = base.join("memory");
        record_fork_prompt_cache_usage_from_fork(Some(&memory_dir), "auto_memory", 150, 120, 10);
        record_fork_prompt_cache_usage_from_fork(
            Some(&paths.skills_dir()),
            "compact_summary",
            300,
            250,
            0,
        );

        let snap = ghost_metrics_summary(&paths);
        assert_eq!(
            snap.fork_prompt_cache_usage["auto_memory"].cache_read_input_tokens,
            120
        );
        assert_eq!(
            snap.fork_prompt_cache_usage["compact_summary"].cache_read_input_tokens,
            250
        );
        assert_eq!(snap.prompt_input_tokens, 450);
        assert_eq!(snap.cache_read_input_tokens, 370);

        record_fork_alignment_miss_from_fork(
            Some(&paths.skills_dir()),
            "skill_nudge_review",
            "preferred_cache_model_unavailable",
        );
        let snap = ghost_metrics_summary(&paths);
        assert_eq!(snap.fork_alignment_misses, 1);
    }
}
