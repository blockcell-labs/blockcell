use super::*;

pub(crate) fn persist_ghost_learning_boundary_with_config(
    config: &Config,
    paths: &Paths,
    boundary: GhostLearningBoundary,
    sources: Vec<GhostEpisodeSource>,
) -> Result<Option<String>> {
    if !config.agents.ghost.learning.capture_enabled() {
        return Ok(None);
    }

    let decision =
        GhostLearningPolicy::from_config(&config.agents.ghost.learning).decide(&boundary);
    persist_ghost_learning_boundary_with_decision(config, paths, boundary, sources, decision)
}

pub(crate) fn persist_ghost_learning_boundary_with_decision(
    config: &Config,
    paths: &Paths,
    boundary: GhostLearningBoundary,
    sources: Vec<GhostEpisodeSource>,
    decision: LearningDecision,
) -> Result<Option<String>> {
    if !config.agents.ghost.learning.capture_enabled() {
        return Ok(None);
    }

    let Some(status) = decision.episode_status() else {
        return Ok(None);
    };

    let snapshot = GhostEpisodeSnapshot::from((boundary.clone(), decision));
    let episode = NewGhostEpisode {
        boundary_kind: boundary.kind.as_str().to_string(),
        subject_key: snapshot.subject_key.clone(),
        status: status.to_string(),
        summary: snapshot.summary(),
        metadata: serde_json::to_value(&snapshot)?,
        sources,
    };

    let ledger = GhostLedger::open(&paths.ghost_ledger_db())?;
    let episode_id = ledger.insert_episode(episode)?;
    crate::ghost_metrics::get_ghost_metrics(paths).record_episode_captured();
    Ok(Some(episode_id))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn capture_delegation_end_learning_boundary_with_config(
    config: &Config,
    paths: &Paths,
    origin_channel: &str,
    origin_chat_id: &str,
    task_id: Option<&str>,
    task_goal: &str,
    child_summary: &str,
    success: bool,
) -> Result<Option<String>> {
    let task_goal = task_goal.trim();
    let child_summary = child_summary.trim();
    if task_goal.is_empty() || child_summary.is_empty() {
        return Ok(None);
    }

    let session_key = blockcell_core::build_session_key(origin_channel, origin_chat_id);
    let mut sources = vec![
        GhostEpisodeSource {
            source_type: "session".to_string(),
            source_key: session_key.clone(),
            role: "primary".to_string(),
        },
        GhostEpisodeSource {
            source_type: "chat".to_string(),
            source_key: origin_chat_id.to_string(),
            role: "context".to_string(),
        },
    ];
    if let Some(task_id) = task_id {
        sources.push(GhostEpisodeSource {
            source_type: "task".to_string(),
            source_key: task_id.to_string(),
            role: "delegation".to_string(),
        });
    }

    let boundary = GhostLearningBoundary {
        kind: GhostLearningBoundaryKind::DelegationEnd,
        session_key: Some(session_key),
        subject_key: Some(format!("chat:{}", origin_chat_id)),
        user_intent_summary: task_goal.to_string(),
        assistant_outcome_summary: child_summary.to_string(),
        tool_call_count: 0,
        memory_write_count: 0,
        correction_count: 0,
        preference_correction_count: 0,
        success,
        complexity_score: estimate_turn_complexity_score(task_goal),
        reusable_lesson: Some(truncate_str(child_summary, 240)),
        turn_facts: None,
    };

    persist_ghost_learning_boundary_with_config(config, paths, boundary, sources)
}
