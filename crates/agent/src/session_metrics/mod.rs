//! Session Memory Metrics Module
//!
//! Provides metrics collection for the 7-layer memory system:
//! - Layer 1: Tool Result Storage
//! - Layer 2: Micro Compact
//! - Layer 3: Session Memory
//! - Layer 4: Full Compact
//! - Layer 5: Memory Extraction
//! - Layer 6: Auto Dream
//! - Layer 7: Forked Agent

mod memory;
mod circuit_breaker;
mod summary;

pub use memory::{MemoryMetrics, get_memory_metrics, Layer1Metrics, Layer2Metrics,
    Layer3Metrics, Layer4Metrics, Layer5Metrics, Layer6Metrics, Layer7Metrics};
pub use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState,
    get_compact_circuit_breaker, get_memory_extraction_circuit_breaker,
    get_dream_circuit_breaker, reset_all_circuit_breakers};
pub use summary::{MetricsSummary, get_metrics_summary, reset_metrics, format_metrics_table};

use std::time::Instant;
use tracing::info;

/// Tracks timing for various stages of message processing.
#[derive(Debug)]
pub(crate) struct ProcessingMetrics {
    start: Instant,
    decision_duration_ms: Option<u64>,
    llm_calls: Vec<u64>,
    tool_executions: Vec<(String, u64)>,
    compression_count: u32,
    finalized: bool,
}

impl ProcessingMetrics {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            decision_duration_ms: None,
            llm_calls: Vec::new(),
            tool_executions: Vec::new(),
            compression_count: 0,
            finalized: false,
        }
    }

    /// Record first-stage interaction decision duration.
    pub fn record_decision(&mut self, duration_ms: u64) {
        self.decision_duration_ms = Some(duration_ms);
    }

    /// Record an LLM call duration.
    pub fn record_llm_call(&mut self, duration_ms: u64) {
        self.llm_calls.push(duration_ms);
    }

    /// Record a tool execution duration.
    pub fn record_tool_execution(&mut self, tool_name: &str, duration_ms: u64) {
        self.tool_executions
            .push((tool_name.to_string(), duration_ms));
    }

    /// Record a mid-loop compression event.
    pub fn record_compression(&mut self) {
        self.compression_count += 1;
    }

    /// Total elapsed time since processing started.
    pub fn total_elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Log a summary of all collected metrics.
    pub fn log_summary(&mut self) {
        if self.finalized {
            return;
        }
        self.finalized = true;

        let total_ms = self.total_elapsed_ms();
        let decision_ms = self.decision_duration_ms.unwrap_or(0);
        let llm_total_ms: u64 = self.llm_calls.iter().sum();
        let tool_total_ms: u64 = self.tool_executions.iter().map(|(_, d)| d).sum();

        info!(
            total_ms,
            decision_ms,
            llm_calls = self.llm_calls.len(),
            llm_total_ms,
            tool_calls = self.tool_executions.len(),
            tool_total_ms,
            compressions = self.compression_count,
            "📊 Message processing metrics"
        );

        // Log slow tool executions (> 5 seconds)
        for (name, ms) in &self.tool_executions {
            if *ms > 5000 {
                info!(tool = %name, duration_ms = ms, "🐢 Slow tool execution");
            }
        }
    }
}

impl Drop for ProcessingMetrics {
    fn drop(&mut self) {
        self.log_summary();
    }
}

/// A simple RAII timer that records duration on drop.
pub(crate) struct ScopedTimer {
    start: Instant,
}

impl ScopedTimer {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Elapsed time in milliseconds.
    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

/// Record memory system events with structured logging and metrics updates.
///
/// # Usage
///
/// ```ignore
/// // Layer 1: Tool result persisted
/// memory_event!(layer1, persisted, "tool-123", 1000, 100);
///
/// // Layer 4: Compact completed
/// memory_event!(layer4, compact_completed, 85000, 15000, 12000, 3000);
///
/// // Layer 4: Compact failed
/// memory_event!(layer4, compact_failed, "LLM timeout", 50000, 1);
/// ```
#[macro_export]
macro_rules! memory_event {
    // ========== Layer 1: Tool Result Storage ==========

    (layer1, persisted, $tool_use_id:expr, $original_size:expr, $preview_size:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer1",
            event = "persisted",
            tool_use_id = %$tool_use_id,
            original_size = $original_size,
            preview_size = $preview_size,
            "Tool result persisted to disk"
        );
        $crate::session_metrics::get_memory_metrics().layer1.record_persisted(
            $original_size as u64,
            $preview_size as u64
        );
    };

    (layer1, budget_exceeded, $total_size:expr, $budget:expr, $candidates:expr) => {
        tracing::warn!(
            target: "blockcell.session_metrics.layer1",
            event = "budget_exceeded",
            total_size = $total_size,
            budget = $budget,
            candidates_count = $candidates,
            "Tool result budget exceeded"
        );
        $crate::session_metrics::get_memory_metrics().layer1.record_budget_exceeded();
    };

    // Layer 1: Config
    (layer1, config, $max_results:expr, $preview_limit:expr) => {
        $crate::session_metrics::get_memory_metrics().layer1.record_config(
            $max_results as u64,
            $preview_limit as u64
        );
    };

    // ========== Layer 2: Micro Compact ==========

    (layer2, triggered, $gap_minutes:expr, $threshold_minutes:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer2",
            event = "triggered",
            gap_minutes = $gap_minutes,
            threshold_minutes = $threshold_minutes,
            "Micro compact time check triggered"
        );
        $crate::session_metrics::get_memory_metrics().layer2.record_trigger();
    };

    (layer2, cleared, $cleared:expr, $kept:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer2",
            event = "cleared",
            cleared_count = $cleared,
            kept_count = $kept,
            "Micro compact content cleared"
        );
        $crate::session_metrics::get_memory_metrics().layer2.record_cleared($cleared as u64, $kept as u64);
    };

    // Layer 2: Config
    (layer2, config, $gap_minutes:expr, $keep_recent:expr) => {
        $crate::session_metrics::get_memory_metrics().layer2.record_config(
            $gap_minutes as u64,
            $keep_recent as u64
        );
    };

    // ========== Layer 3: Session Memory ==========

    (layer3, extraction_started, $session_id:expr, $message_count:expr, $token_estimate:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer3",
            event = "extraction_started",
            session_id = %$session_id,
            message_count = $message_count,
            token_estimate = $token_estimate,
            "Session memory extraction started"
        );
        $crate::session_metrics::get_memory_metrics().layer3.record_extraction($token_estimate as u64);
    };

    (layer3, loaded, $content_length:expr, $line_count:expr, $sections_count:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer3",
            event = "loaded",
            content_length = $content_length,
            line_count = $line_count,
            sections_count = $sections_count,
            "Session memory loaded"
        );
        let metrics = $crate::session_metrics::get_memory_metrics();
        metrics.layer3.record_load($content_length as u64);
        metrics.layer3.update_section_count($sections_count as u64);
    };

    // Layer 3: Config
    (layer3, config, $max_total:expr, $max_section:expr) => {
        $crate::session_metrics::get_memory_metrics().layer3.record_config(
            $max_total as u64,
            $max_section as u64
        );
    };

    // ========== Layer 4: Full Compact ==========

    (layer4, compact_started, $pre_tokens:expr, $threshold:expr, $is_auto:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer4",
            event = "compact_started",
            pre_compact_tokens = $pre_tokens,
            threshold = $threshold,
            is_auto = $is_auto,
            "Compact started"
        );
    };

    (layer4, compact_completed, $pre:expr, $post:expr, $cache_read:expr, $cache_creation:expr) => {
        {
            let ratio = if $pre > 0 { 1.0 - ($post as f64 / $pre as f64) } else { 0.0 };
            let hit_rate = if $cache_read + $cache_creation > 0 {
                $cache_read as f64 / ($cache_read + $cache_creation) as f64
            } else { 0.0 };

            tracing::info!(
                target: "blockcell.session_metrics.layer4",
                event = "compact_completed",
                pre_compact_tokens = $pre,
                post_compact_tokens = $post,
                compression_ratio = format!("{:.2}%", ratio * 100.0),
                cache_read_tokens = $cache_read,
                cache_creation_tokens = $cache_creation,
                cache_hit_rate = format!("{:.2}%", hit_rate * 100.0),
                is_auto = true,
                "Compact completed successfully"
            );

            $crate::session_metrics::get_memory_metrics().layer4.record_compact_success(
                $pre as u64,
                $post as u64,
                $cache_read as u64,
                $cache_creation as u64
            );
        }
    };

    (layer4, compact_completed, $pre:expr, $post:expr, $cache_read:expr, $cache_creation:expr, $is_auto:expr) => {
        {
            let ratio = if $pre > 0 { 1.0 - ($post as f64 / $pre as f64) } else { 0.0 };
            let hit_rate = if $cache_read + $cache_creation > 0 {
                $cache_read as f64 / ($cache_read + $cache_creation) as f64
            } else { 0.0 };

            tracing::info!(
                target: "blockcell.session_metrics.layer4",
                event = "compact_completed",
                pre_compact_tokens = $pre,
                post_compact_tokens = $post,
                compression_ratio = format!("{:.2}%", ratio * 100.0),
                cache_read_tokens = $cache_read,
                cache_creation_tokens = $cache_creation,
                cache_hit_rate = format!("{:.2}%", hit_rate * 100.0),
                is_auto = $is_auto,
                "Compact completed successfully"
            );

            $crate::session_metrics::get_memory_metrics().layer4.record_compact_success_with_type(
                $pre as u64,
                $post as u64,
                $cache_read as u64,
                $cache_creation as u64,
                $is_auto
            );
        }
    };

    (layer4, compact_failed, $reason:expr, $pre_tokens:expr, $attempt:expr) => {
        tracing::warn!(
            target: "blockcell.session_metrics.layer4",
            event = "compact_failed",
            reason = $reason,
            pre_compact_tokens = $pre_tokens,
            attempt = $attempt,
            "Compact failed"
        );
        $crate::session_metrics::get_memory_metrics().layer4.record_compact_failure();
    };

    // Layer 4: Token usage update (实时状态)
    (layer4, token_usage, $current:expr, $budget:expr, $threshold:expr) => {
        $crate::session_metrics::get_memory_metrics().layer4.update_token_usage($current as u64);
    };

    // Layer 4: Config
    (layer4, config, $budget:expr, $threshold:expr, $recovery_budget:expr) => {
        $crate::session_metrics::get_memory_metrics().layer4.record_config(
            $budget as u64,
            $threshold as f64,
            $recovery_budget as u64
        );
    };

    // ========== Layer 5: Memory Extraction ==========

    (layer5, memory_written, $memory_type:expr, $filepath:expr, $content_len:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer5",
            event = "memory_written",
            memory_type = $memory_type,
            filepath = %$filepath,
            content_length = $content_len,
            "Memory written to file"
        );
        $crate::session_metrics::get_memory_metrics().layer5.record_memory_written(
            $memory_type,
            $content_len as u64
        );
    };

    (layer5, injection_completed, $user:expr, $project:expr, $feedback:expr, $reference:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer5",
            event = "injection_completed",
            user_memories = $user,
            project_memories = $project,
            feedback_memories = $feedback,
            reference_memories = $reference,
            "Memory injection completed"
        );
        $crate::session_metrics::get_memory_metrics().layer5.record_injection(
            $user as u64,
            $project as u64,
            $feedback as u64,
            $reference as u64
        );
    };

    // Layer 5: Config
    (layer5, config, $min_msg:expr, $cooldown:expr, $max_file:expr) => {
        $crate::session_metrics::get_memory_metrics().layer5.record_config(
            $min_msg as u64,
            $cooldown as u64,
            $max_file as u64
        );
    };

    // ========== Layer 6: Auto Dream ==========

    (layer6, dream_started, $sessions_count:expr, $hours_since_last:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer6",
            event = "dream_started",
            sessions_count = $sessions_count,
            hours_since_last = $hours_since_last,
            "Dream consolidation started"
        );
        $crate::session_metrics::get_memory_metrics().layer6.record_dream_started();
    };

    // Layer 6: dream_finished (includes sessions_processed)
    (layer6, dream_finished, $created:expr, $updated:expr, $deleted:expr, $pruned:expr, $sessions:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer6",
            event = "dream_finished",
            memories_created = $created,
            memories_updated = $updated,
            memories_deleted = $deleted,
            sessions_pruned = $pruned,
            sessions_processed = $sessions,
            "Dream consolidation completed"
        );
        $crate::session_metrics::get_memory_metrics().layer6.record_dream_finished(
            $created as u64,
            $updated as u64,
            $deleted as u64,
            $pruned as u64,
            $sessions as u64
        );
    };

    // Layer 6: Config
    (layer6, config, $interval_hours:expr) => {
        $crate::session_metrics::get_memory_metrics().layer6.record_config($interval_hours as u64);
    };

    // ========== Layer 7: Forked Agent ==========

    (layer7, agent_spawned, $fork_label:expr, $max_turns:expr, $parent_id:expr) => {
        tracing::debug!(
            target: "blockcell.session_metrics.layer7",
            event = "agent_spawned",
            fork_label = $fork_label,
            max_turns = $max_turns,
            parent_agent_id = %$parent_id,
            "Forked agent spawned"
        );
        $crate::session_metrics::get_memory_metrics().layer7.record_spawned();
    };

    (layer7, agent_completed, $fork_label:expr, $turns:expr, $tokens:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer7",
            event = "agent_completed",
            fork_label = $fork_label,
            turns_used = $turns,
            total_tokens = $tokens,
            "Forked agent completed"
        );
        $crate::session_metrics::get_memory_metrics().layer7.record_completed(
            $turns as u64,
            $tokens as u64
        );
    };

    // Layer 7: agent_completed with duration
    (layer7, agent_completed_with_duration, $fork_label:expr, $turns:expr, $tokens:expr, $duration_ms:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer7",
            event = "agent_completed",
            fork_label = $fork_label,
            turns_used = $turns,
            total_tokens = $tokens,
            duration_ms = $duration_ms,
            "Forked agent completed"
        );
        $crate::session_metrics::get_memory_metrics().layer7.record_completed_with_duration(
            $turns as u64,
            $tokens as u64,
            $duration_ms as u64
        );
    };

    (layer7, agent_failed, $fork_label:expr, $error:expr, $turns:expr) => {
        tracing::warn!(
            target: "blockcell.session_metrics.layer7",
            event = "agent_failed",
            fork_label = $fork_label,
            error = $error,
            turns_used = $turns,
            "Forked agent failed"
        );
        $crate::session_metrics::get_memory_metrics().layer7.record_failed();
    };

    (layer7, tool_denied, $tool_name:expr, $reason:expr) => {
        tracing::warn!(
            target: "blockcell.session_metrics.layer7",
            event = "tool_denied",
            tool_name = $tool_name,
            reason = $reason,
            "Tool permission denied"
        );
        $crate::session_metrics::get_memory_metrics().layer7.record_tool_denied();
    };

    // Layer 7: Config
    (layer7, config, $max_turns:expr) => {
        $crate::session_metrics::get_memory_metrics().layer7.record_config($max_turns as u64);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_processing_metrics_basic() {
        let mut metrics = ProcessingMetrics::new();
        metrics.record_decision(100);
        metrics.record_llm_call(200);
        metrics.record_llm_call(150);
        metrics.record_tool_execution("web_search", 500);
        metrics.record_tool_execution("read_file", 10);
        metrics.record_compression();

        assert_eq!(metrics.decision_duration_ms, Some(100));
        assert_eq!(metrics.llm_calls.len(), 2);
        assert_eq!(metrics.tool_executions.len(), 2);
        assert_eq!(metrics.compression_count, 1);
        assert!(!metrics.finalized);
        assert!(metrics.total_elapsed_ms() < 1000);
    }

    #[test]
    fn test_scoped_timer() {
        let timer = ScopedTimer::new();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(timer.elapsed_ms() >= 5);
    }

    // ========== memory_event! 宏测试 ==========

    #[test]
    fn test_memory_event_layer1_persisted() {
        let metrics = get_memory_metrics();
        metrics.layer1.reset();

        memory_event!(layer1, persisted, "tool-123", 1000, 100);

        assert_eq!(metrics.layer1.persisted_count(), 1);
        assert_eq!(metrics.layer1.total_original_size(), 1000);
        assert_eq!(metrics.layer1.total_preview_size(), 100);
    }

    #[test]
    fn test_memory_event_layer1_budget_exceeded() {
        let metrics = get_memory_metrics();
        metrics.layer1.reset();

        memory_event!(layer1, budget_exceeded, 150000, 100000, 5);

        assert_eq!(metrics.layer1.budget_exceeded_count(), 1);
    }

    #[test]
    fn test_memory_event_layer2_triggered() {
        let metrics = get_memory_metrics();
        metrics.layer2.reset();

        memory_event!(layer2, triggered, 30, 60);

        assert_eq!(metrics.layer2.trigger_count(), 1);
    }

    #[test]
    fn test_memory_event_layer2_cleared() {
        let metrics = get_memory_metrics();
        metrics.layer2.reset();

        memory_event!(layer2, cleared, 10, 5);

        assert_eq!(metrics.layer2.cleared_count(), 10);
        assert_eq!(metrics.layer2.kept_count(), 5);
    }

    #[test]
    fn test_memory_event_layer3_extraction_started() {
        let metrics = get_memory_metrics();
        metrics.layer3.reset();

        memory_event!(layer3, extraction_started, "session-abc", 50, 5000);

        assert_eq!(metrics.layer3.extraction_count(), 1);
        assert_eq!(metrics.layer3.total_token_estimate(), 5000);
    }

    #[test]
    fn test_memory_event_layer3_loaded() {
        let metrics = get_memory_metrics();
        metrics.layer3.reset();

        memory_event!(layer3, loaded, 1234, 50, 5);

        assert_eq!(metrics.layer3.load_count(), 1);
        assert_eq!(metrics.layer3.current_size(), 1234);
        assert_eq!(metrics.layer3.section_count(), 5); // section_count 现在由宏自动更新
    }

    #[test]
    fn test_memory_event_layer4_compact_completed() {
        let metrics = get_memory_metrics();
        metrics.layer4.reset();

        memory_event!(layer4, compact_completed, 10000, 3000, 8000, 2000);

        assert_eq!(metrics.layer4.compact_count(), 1);
        assert_eq!(metrics.layer4.total_pre_compact_tokens(), 10000);
        assert_eq!(metrics.layer4.total_post_compact_tokens(), 3000);
        assert_eq!(metrics.layer4.consecutive_failures(), 0); // 成功后重置
    }

    #[test]
    fn test_memory_event_layer4_compact_failed() {
        let metrics = get_memory_metrics();
        metrics.layer4.reset();

        memory_event!(layer4, compact_failed, "LLM timeout", 50000, 1);

        assert_eq!(metrics.layer4.compact_failed_count(), 1);
        assert_eq!(metrics.layer4.consecutive_failures(), 1);
    }

    #[test]
    fn test_memory_event_layer5_memory_written() {
        let metrics = get_memory_metrics();
        metrics.layer5.reset();

        memory_event!(layer5, memory_written, "user", "/path/to/memory.md", 500);

        assert_eq!(metrics.layer5.extraction_count(), 1);
        assert_eq!(metrics.layer5.user_memories(), 1);
        assert_eq!(metrics.layer5.total_bytes_written(), 500);
    }

    #[test]
    fn test_memory_event_layer5_injection() {
        let metrics = get_memory_metrics();
        metrics.layer5.reset();

        memory_event!(layer5, injection_completed, 5, 8, 2, 3);

        assert_eq!(metrics.layer5.user_memories(), 5);
        assert_eq!(metrics.layer5.project_memories(), 8);
        assert_eq!(metrics.layer5.feedback_memories(), 2);
        assert_eq!(metrics.layer5.reference_memories(), 3);
    }

    #[test]
    fn test_memory_event_layer6_dream() {
        let metrics = get_memory_metrics();
        metrics.layer6.reset();

        memory_event!(layer6, dream_started, 10, 24);
        memory_event!(layer6, dream_finished, 5, 3, 1, 2, 10);

        assert_eq!(metrics.layer6.dream_count(), 1);
        assert_eq!(metrics.layer6.memories_created(), 5);
        assert_eq!(metrics.layer6.memories_updated(), 3);
        assert_eq!(metrics.layer6.memories_deleted(), 1);
        assert_eq!(metrics.layer6.sessions_pruned(), 2);
        assert_eq!(metrics.layer6.sessions_processed(), 10);
    }

    #[test]
    fn test_memory_event_layer7_agent() {
        let metrics = get_memory_metrics();
        metrics.layer7.reset();

        memory_event!(layer7, agent_spawned, "test-fork", 10, "parent-123");
        memory_event!(layer7, agent_completed, "test-fork", 5, 1000);

        assert_eq!(metrics.layer7.spawned_count(), 1);
        assert_eq!(metrics.layer7.completed_count(), 1);
        assert_eq!(metrics.layer7.total_turns_used(), 5);
        assert_eq!(metrics.layer7.total_tokens_used(), 1000);

        memory_event!(layer7, agent_failed, "test-fork-2", "error", 3);
        assert_eq!(metrics.layer7.failed_count(), 1);

        memory_event!(layer7, tool_denied, "exec", "security");
        assert_eq!(metrics.layer7.tool_denied_count(), 1);
    }
}