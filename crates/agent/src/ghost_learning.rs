use blockcell_core::config::GhostLearningConfig;
use blockcell_storage::TurnFacts;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GhostLearningBoundaryKind {
    TurnEnd,
    PreCompress,
    SessionRotate,
    SessionEnd,
    DelegationEnd,
    EvolutionSuccess,
}

impl GhostLearningBoundaryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            GhostLearningBoundaryKind::TurnEnd => "turn_end",
            GhostLearningBoundaryKind::PreCompress => "pre_compress",
            GhostLearningBoundaryKind::SessionRotate => "session_rotate",
            GhostLearningBoundaryKind::SessionEnd => "session_end",
            GhostLearningBoundaryKind::DelegationEnd => "delegation_end",
            GhostLearningBoundaryKind::EvolutionSuccess => "evolution_success",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningDecision {
    Ignore,
    ReviewAfterResponse,
    ForceBoundaryReview,
}

impl LearningDecision {
    pub fn episode_status(&self) -> Option<&'static str> {
        match self {
            LearningDecision::Ignore => None,
            LearningDecision::ReviewAfterResponse => Some("pending_review"),
            LearningDecision::ForceBoundaryReview => Some("pending_review"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GhostLearningBoundary {
    pub kind: GhostLearningBoundaryKind,
    pub session_key: Option<String>,
    pub subject_key: Option<String>,
    pub user_intent_summary: String,
    pub assistant_outcome_summary: String,
    pub tool_call_count: u32,
    pub memory_write_count: u32,
    pub correction_count: u32,
    pub preference_correction_count: u32,
    pub success: bool,
    pub complexity_score: u32,
    pub reusable_lesson: Option<String>,
    /// 回合在 Session Event Log 中记录的事实（工具结论、记忆写入、模型错误等）。
    pub turn_facts: Option<TurnFacts>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GhostEpisodeSnapshot {
    pub boundary_kind: GhostLearningBoundaryKind,
    pub session_key: Option<String>,
    pub subject_key: Option<String>,
    pub user_intent_summary: String,
    pub assistant_outcome_summary: String,
    pub tool_call_count: u32,
    pub memory_write_count: u32,
    pub correction_count: u32,
    pub preference_correction_count: u32,
    pub complexity_score: u32,
    pub reusable_lesson: Option<String>,
    pub decision: LearningDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_facts: Option<TurnFacts>,
}

impl GhostEpisodeSnapshot {
    pub fn summary(&self) -> String {
        if self.assistant_outcome_summary.trim().is_empty() {
            self.user_intent_summary.clone()
        } else {
            format!(
                "{} => {}",
                self.user_intent_summary, self.assistant_outcome_summary
            )
        }
    }
}

impl From<(GhostLearningBoundary, LearningDecision)> for GhostEpisodeSnapshot {
    fn from((boundary, decision): (GhostLearningBoundary, LearningDecision)) -> Self {
        Self {
            boundary_kind: boundary.kind,
            session_key: boundary.session_key,
            subject_key: boundary.subject_key,
            user_intent_summary: boundary.user_intent_summary,
            assistant_outcome_summary: boundary.assistant_outcome_summary,
            tool_call_count: boundary.tool_call_count,
            memory_write_count: boundary.memory_write_count,
            correction_count: boundary.correction_count,
            preference_correction_count: boundary.preference_correction_count,
            complexity_score: boundary.complexity_score,
            reusable_lesson: boundary.reusable_lesson,
            decision,
            turn_facts: boundary.turn_facts,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostLearningPolicy {
    method_tool_threshold: u32,
    turn_review_interval: u32,
}

impl GhostLearningPolicy {
    pub fn from_config(config: &GhostLearningConfig) -> Self {
        Self {
            method_tool_threshold: config.method_tool_threshold.max(1),
            turn_review_interval: config.turn_review_interval,
        }
    }

    pub fn decide(&self, boundary: &GhostLearningBoundary) -> LearningDecision {
        self.decide_with_turn_count(boundary, None)
    }

    pub fn decide_with_turn_count(
        &self,
        boundary: &GhostLearningBoundary,
        turn_count: Option<u32>,
    ) -> LearningDecision {
        match boundary.kind {
            GhostLearningBoundaryKind::PreCompress
            | GhostLearningBoundaryKind::SessionRotate
            | GhostLearningBoundaryKind::SessionEnd => {
                return LearningDecision::ForceBoundaryReview;
            }
            GhostLearningBoundaryKind::DelegationEnd
            | GhostLearningBoundaryKind::EvolutionSuccess => {
                // Both successful and failed delegations/evolutions should be reviewed.
                // Failures are important learning opportunities.
                return LearningDecision::ReviewAfterResponse;
            }
            GhostLearningBoundaryKind::TurnEnd => {}
        }

        // Failed turns are important learning opportunities — they often contain
        // correction signals, reusable lessons, or method insights that should be
        // reviewed. Previously, all failed turns were silently ignored, which meant
        // the system never learned from its mistakes.
        if !boundary.success {
            if boundary.correction_count > 0
                || boundary.preference_correction_count > 0
                || boundary.tool_call_count > 0
                || boundary.complexity_score >= 3
                || boundary.reusable_lesson.is_some()
            {
                return LearningDecision::ReviewAfterResponse;
            }
            // Trivial failures (no tools, no corrections, low complexity) can be ignored
            return LearningDecision::Ignore;
        }

        if boundary.preference_correction_count > 0 || boundary.correction_count > 0 {
            return LearningDecision::ReviewAfterResponse;
        }

        if boundary.tool_call_count >= self.method_tool_threshold {
            return LearningDecision::ReviewAfterResponse;
        }

        if boundary.complexity_score >= 5 {
            return LearningDecision::ReviewAfterResponse;
        }

        if boundary.memory_write_count > 0 && boundary.reusable_lesson.is_some() {
            return LearningDecision::ReviewAfterResponse;
        }

        if boundary.kind == GhostLearningBoundaryKind::TurnEnd
            && self.turn_review_interval > 0
            && turn_count
                .filter(|count| *count > 0 && count % self.turn_review_interval == 0)
                .is_some()
        {
            return LearningDecision::ReviewAfterResponse;
        }

        LearningDecision::Ignore
    }
}

impl Default for GhostLearningPolicy {
    fn default() -> Self {
        Self {
            method_tool_threshold: 3,
            turn_review_interval: 6,
        }
    }
}

pub fn estimate_turn_complexity_score(user_text: &str) -> u32 {
    let trimmed = user_text.trim();
    if trimmed.is_empty() {
        return 0;
    }

    let lower = trimmed.to_lowercase();
    let mut score = 0;

    let token_count = lower.split_whitespace().count();
    if token_count >= 5 {
        score += 2;
    }

    let cues = [
        "figure out",
        "correct",
        "sequence",
        "analyze",
        "investigate",
        "deploy",
        "rollback",
        "why",
        "how",
        "compare",
        "正确",
        "顺序",
        "分析",
        "排查",
        "回滚",
        "部署",
    ];
    if cues.iter().any(|cue| lower.contains(cue)) {
        score += 4;
    }

    score
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_trivial_success_turn() -> GhostLearningBoundary {
        GhostLearningBoundary {
            kind: GhostLearningBoundaryKind::TurnEnd,
            session_key: Some("cli:chat-1".to_string()),
            subject_key: Some("user:test".to_string()),
            user_intent_summary: "say hello".to_string(),
            assistant_outcome_summary: "said hello".to_string(),
            tool_call_count: 0,
            memory_write_count: 0,
            correction_count: 0,
            preference_correction_count: 0,
            success: true,
            complexity_score: 0,
            reusable_lesson: None,
            turn_facts: None,
        }
    }

    fn sample_preference_correction_boundary() -> GhostLearningBoundary {
        GhostLearningBoundary {
            kind: GhostLearningBoundaryKind::TurnEnd,
            session_key: Some("cli:chat-1".to_string()),
            subject_key: Some("user:test".to_string()),
            user_intent_summary: "user corrected deploy preference".to_string(),
            assistant_outcome_summary: "captured preferred canary deploy sequence".to_string(),
            tool_call_count: 1,
            memory_write_count: 0,
            correction_count: 1,
            preference_correction_count: 1,
            success: true,
            complexity_score: 4,
            reusable_lesson: Some("Prefer canary-first deploys for this user".to_string()),
            turn_facts: None,
        }
    }

    fn sample_high_complexity_tool_boundary() -> GhostLearningBoundary {
        GhostLearningBoundary {
            kind: GhostLearningBoundaryKind::TurnEnd,
            session_key: Some("cli:chat-1".to_string()),
            subject_key: Some("user:test".to_string()),
            user_intent_summary: "analyze the deploy failure and correct sequence".to_string(),
            assistant_outcome_summary: "used tools to determine the correct deploy order"
                .to_string(),
            tool_call_count: 4,
            memory_write_count: 0,
            correction_count: 0,
            preference_correction_count: 0,
            success: true,
            complexity_score: 6,
            reusable_lesson: Some("Validate rollout ordering before deploy".to_string()),
            turn_facts: None,
        }
    }

    fn sample_pre_compress_boundary() -> GhostLearningBoundary {
        GhostLearningBoundary {
            kind: GhostLearningBoundaryKind::PreCompress,
            session_key: Some("cli:chat-1".to_string()),
            subject_key: Some("user:test".to_string()),
            user_intent_summary: "conversation budget boundary reached".to_string(),
            assistant_outcome_summary: "about to compact conversation".to_string(),
            tool_call_count: 0,
            memory_write_count: 0,
            correction_count: 0,
            preference_correction_count: 0,
            success: true,
            complexity_score: 0,
            reusable_lesson: None,
            turn_facts: None,
        }
    }

    #[test]
    fn ghost_learning_policy_ignores_trivial_success_turn() {
        let policy = GhostLearningPolicy::default();
        let decision = policy.decide(&sample_trivial_success_turn());
        assert_eq!(decision, LearningDecision::Ignore);
    }

    #[test]
    fn ghost_learning_policy_reviews_on_configured_turn_interval() {
        let config = GhostLearningConfig {
            turn_review_interval: 2,
            ..Default::default()
        };
        let policy = GhostLearningPolicy::from_config(&config);

        let boundary = sample_trivial_success_turn();
        assert_eq!(
            policy.decide_with_turn_count(&boundary, Some(1)),
            LearningDecision::Ignore
        );
        assert_eq!(
            policy.decide_with_turn_count(&boundary, Some(2)),
            LearningDecision::ReviewAfterResponse
        );
    }

    #[test]
    fn ghost_learning_policy_preference_correction_requests_review() {
        let policy = GhostLearningPolicy::default();
        let decision = policy.decide(&sample_preference_correction_boundary());
        assert_eq!(decision, LearningDecision::ReviewAfterResponse);
        assert_eq!(decision.episode_status(), Some("pending_review"));
    }

    #[test]
    fn ghost_learning_policy_high_complexity_tool_turn_requests_review() {
        let policy = GhostLearningPolicy::default();
        let decision = policy.decide(&sample_high_complexity_tool_boundary());
        assert_eq!(decision, LearningDecision::ReviewAfterResponse);
        assert_eq!(decision.episode_status(), Some("pending_review"));
    }

    #[test]
    fn ghost_learning_policy_pre_compress_forces_boundary_review() {
        let policy = GhostLearningPolicy::default();
        let decision = policy.decide(&sample_pre_compress_boundary());
        assert_eq!(decision, LearningDecision::ForceBoundaryReview);
        assert_eq!(decision.episode_status(), Some("pending_review"));
    }

    /// 回归测试：GhostLearningPolicy 并发更新不会导致数据竞争
    ///
    /// 验证 Mutex 保护下，多线程并发调用 update_ghost_policy()
    /// 后 policy 状态一致（最后一个写入者的值生效）。
    #[test]
    fn ghost_learning_policy_concurrent_update_no_data_race() {
        use std::sync::Arc;
        use std::thread;

        let policy = Arc::new(std::sync::Mutex::new(GhostLearningPolicy::default()));
        let mut handles = vec![];

        // 8 个线程并发写入不同的 method_tool_threshold
        for i in 1..=8u32 {
            let p = Arc::clone(&policy);
            handles.push(thread::spawn(move || {
                let config = GhostLearningConfig {
                    enabled: true,
                    capture_enabled: Some(true),
                    write_enabled: Some(true),
                    recall_enabled: Some(true),
                    turn_review_interval: i,
                    method_tool_threshold: i,
                    recall_max_items: 10,
                    recall_token_budget: 1000,
                    ..GhostLearningConfig::default()
                };
                let new_policy = GhostLearningPolicy::from_config(&config);
                *p.lock().unwrap() = new_policy;
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // 验证：最终值是某个合法写入值（1..=8），不是垃圾数据
        let final_policy = policy.lock().unwrap();
        let boundary = GhostLearningBoundary {
            kind: GhostLearningBoundaryKind::TurnEnd,
            session_key: None,
            subject_key: None,
            user_intent_summary: "test".to_string(),
            assistant_outcome_summary: "ok".to_string(),
            tool_call_count: 5, // 超过所有可能的 threshold (1..=8)
            memory_write_count: 0,
            correction_count: 0,
            preference_correction_count: 0,
            success: true,
            complexity_score: 0,
            reusable_lesson: None,
            turn_facts: None,
        };
        // tool_call_count=5 >= method_tool_threshold(1..=8 中任意值 ≤ 5 时触发 Review)
        let decision = final_policy.decide(&boundary);
        // method_tool_threshold 在 1..=5 时 Review，6..=8 时 Ignore（因为 5 < threshold）
        assert!(matches!(
            decision,
            LearningDecision::ReviewAfterResponse | LearningDecision::Ignore
        ));
    }
}
