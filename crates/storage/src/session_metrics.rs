//! 从 Session Event Log 计算的会话指标（Evaluation 的基础数据）。
//!
//! 只依赖事件日志，不读取 transcript 或业务数据库；旧日志缺失的字段按 0 / `unknown` 计。

use crate::session_event::{SessionEvent, SessionEventKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SessionMetrics {
    pub turns: usize,
    /// 异常终止（`turn/end` 带 error）的回合数。
    pub failed_turns: usize,
    pub steps: usize,
    pub model_requests: usize,
    pub model_request_errors: usize,
    /// 被丢弃的模型输出（流式中断后重试）。
    pub discarded_attempts: usize,
    /// Legacy count of tool/result events, including pre-execution rejections.
    pub tool_calls: usize,
    /// Model proposals from model/response, before admission checks.
    pub proposed_tool_calls: usize,
    /// Calls admitted for execution, from tool/call.
    pub executed_tool_calls: usize,
    /// Calls with a terminal tool/result, including rejected/not-started calls.
    pub completed_tool_calls: usize,
    pub rejected_tool_calls: usize,
    pub succeeded_tool_calls: usize,
    pub failed_tool_calls: usize,
    pub not_started_tool_calls: usize,
    /// 按 ToolOutcome 统计的工具调用数。
    pub tool_outcomes: BTreeMap<String, usize>,
    pub tool_duration_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 实际服务过的模型，按首次出现顺序。
    pub models: Vec<String>,
    pub compactions: usize,
    pub memory_writes: usize,
    /// 第一条与最后一条事件之间的时长。
    pub wall_duration_ms: u64,
    /// 按 `技能@版本`（无版本时为技能名）统计加载了该技能的已结束回合，用于比较技能版本效果。
    pub skill_turns: BTreeMap<String, SkillTurnStats>,
}

/// 加载了某个技能版本的已结束回合数与其中异常终止的回合数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SkillTurnStats {
    pub turns: usize,
    pub failed_turns: usize,
}

/// `skill/load` 事件对应的统计键：`name@version`，无版本时为 `name`。
pub fn skill_key(payload: &Value) -> Option<String> {
    let name = payload.get("name").and_then(Value::as_str)?;
    Some(match payload.get("version").and_then(Value::as_str) {
        Some(version) if !version.is_empty() => format!("{name}@{version}"),
        _ => name.to_string(),
    })
}

impl SessionMetrics {
    pub fn from_events(events: &[SessionEvent]) -> Self {
        let mut metrics = Self::default();
        let mut turn_skills: HashMap<&str, BTreeSet<String>> = HashMap::new();
        for event in events {
            let payload = &event.payload;
            match event.kind {
                SessionEventKind::TurnStart => metrics.turns += 1,
                SessionEventKind::TurnEnd => {
                    let failed = payload.get("error").is_some();
                    if failed {
                        metrics.failed_turns += 1;
                    }
                    let skills = event
                        .turn_id
                        .as_deref()
                        .and_then(|turn_id| turn_skills.remove(turn_id))
                        .unwrap_or_default();
                    for skill in skills {
                        let stats = metrics.skill_turns.entry(skill).or_default();
                        stats.turns += 1;
                        stats.failed_turns += usize::from(failed);
                    }
                }
                SessionEventKind::SkillLoad => {
                    if let (Some(turn_id), Some(skill)) =
                        (event.turn_id.as_deref(), skill_key(payload))
                    {
                        turn_skills.entry(turn_id).or_default().insert(skill);
                    }
                }
                SessionEventKind::StepStart => metrics.steps += 1,
                SessionEventKind::ModelRequest => metrics.model_requests += 1,
                SessionEventKind::ModelRequestError => metrics.model_request_errors += 1,
                SessionEventKind::AssistantAttempt => metrics.discarded_attempts += 1,
                SessionEventKind::ModelResponse => {
                    metrics.proposed_tool_calls += payload
                        .get("tool_calls")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    metrics.input_tokens += u64_at(payload, &["usage", "input_tokens"]);
                    metrics.output_tokens += u64_at(payload, &["usage", "output_tokens"]);
                    if let Some(model) = payload.get("model").and_then(Value::as_str) {
                        if !model.is_empty() && !metrics.models.iter().any(|m| m == model) {
                            metrics.models.push(model.to_string());
                        }
                    }
                }
                SessionEventKind::ToolCall => metrics.executed_tool_calls += 1,
                SessionEventKind::ToolResult => {
                    metrics.tool_calls += 1;
                    metrics.completed_tool_calls += 1;
                    match tool_outcome(payload) {
                        "rejected" => metrics.rejected_tool_calls += 1,
                        "succeeded" => metrics.succeeded_tool_calls += 1,
                        "not_started" => metrics.not_started_tool_calls += 1,
                        _ => metrics.failed_tool_calls += 1,
                    }
                    metrics.tool_duration_ms += u64_at(payload, &["duration_ms"]);
                    *metrics
                        .tool_outcomes
                        .entry(tool_outcome(payload).to_string())
                        .or_default() += 1;
                }
                SessionEventKind::Compaction | SessionEventKind::CompactionEnd => {
                    metrics.compactions += 1
                }
                SessionEventKind::MemoryWrite => metrics.memory_writes += 1,
                _ => {}
            }
        }
        if let (Some(first), Some(last)) = (events.first(), events.last()) {
            metrics.wall_duration_ms =
                (last.timestamp - first.timestamp).num_milliseconds().max(0) as u64;
        }
        metrics
    }

    /// 结果不是 `succeeded` 的工具调用数。
    pub fn unsuccessful_tool_calls(&self) -> usize {
        self.tool_outcomes
            .iter()
            .filter(|(outcome, _)| outcome.as_str() != "succeeded")
            .map(|(_, count)| count)
            .sum()
    }
}

/// 单个回合在事件日志中记录的事实，供 Ghost 复盘直接消费，而不是从 transcript 推断。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnFacts {
    pub turn_id: String,
    pub steps: usize,
    /// 按发生顺序的工具调用结论（每个模型提议的调用一条）。
    pub tool_calls: Vec<ToolCallFact>,
    pub memory_writes: usize,
    pub model_request_errors: usize,
    pub compactions: usize,
    pub skills: Vec<String>,
    /// `turn/end` 记录的异常终止原因；回合尚未结束时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallFact {
    pub tool: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_safe: Option<bool>,
}

impl TurnFacts {
    /// 从会话事件中提取指定回合的事实；该回合没有任何事件时返回 `None`。
    pub fn from_events(events: &[SessionEvent], turn_id: &str) -> Option<Self> {
        let mut facts = Self {
            turn_id: turn_id.to_string(),
            ..Self::default()
        };
        let mut seen = false;
        for event in events
            .iter()
            .filter(|event| event.turn_id.as_deref() == Some(turn_id))
        {
            seen = true;
            let payload = &event.payload;
            match event.kind {
                SessionEventKind::StepStart => facts.steps += 1,
                SessionEventKind::ToolResult => facts.tool_calls.push(ToolCallFact {
                    tool: payload
                        .get("tool")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    outcome: tool_outcome(payload).to_string(),
                    retry_safe: payload.get("retry_safe").and_then(Value::as_bool),
                }),
                SessionEventKind::MemoryWrite => facts.memory_writes += 1,
                SessionEventKind::ModelRequestError => facts.model_request_errors += 1,
                SessionEventKind::Compaction | SessionEventKind::CompactionEnd => {
                    facts.compactions += 1
                }
                SessionEventKind::SkillLoad => {
                    if let Some(name) = payload.get("name").and_then(Value::as_str) {
                        facts.skills.push(name.to_string());
                    }
                }
                SessionEventKind::TurnEnd => {
                    facts.error = payload.get("error").map(|error| match error {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    });
                }
                _ => {}
            }
        }
        seen.then_some(facts)
    }

    /// 结论不是 `succeeded` 的工具调用数。
    pub fn unsuccessful_tool_calls(&self) -> usize {
        self.tool_calls
            .iter()
            .filter(|call| call.outcome != "succeeded")
            .count()
    }
}

/// `tool/result` 的执行结论；早于 ToolOutcome 的日志按 `is_error` 推断。
pub fn tool_outcome(payload: &Value) -> &str {
    if let Some(outcome) = payload.get("outcome").and_then(Value::as_str) {
        return outcome;
    }
    match payload.get("is_error").and_then(Value::as_bool) {
        Some(true) => "failed",
        Some(false) => "succeeded",
        None => "unknown",
    }
}

fn u64_at(value: &Value, path: &[&str]) -> u64 {
    path.iter()
        .try_fold(value, |value, key| value.get(key))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_event::SESSION_EVENT_SCHEMA_VERSION;
    use chrono::{Duration, Utc};
    use serde_json::json;

    fn events(items: Vec<(SessionEventKind, Value)>) -> Vec<SessionEvent> {
        let start = Utc::now();
        items
            .into_iter()
            .enumerate()
            .map(|(index, (kind, payload))| SessionEvent {
                schema_version: SESSION_EVENT_SCHEMA_VERSION,
                seq: index as u64 + 1,
                session_id: "cli:test".into(),
                timestamp: start + Duration::milliseconds(index as i64 * 100),
                turn_id: Some("t1".into()),
                step: None,
                kind,
                payload,
            })
            .collect()
    }

    #[test]
    fn metrics_summarize_turns_models_tools_and_cost() {
        use SessionEventKind::*;
        let log = events(vec![
            (TurnStart, json!({})),
            (StepStart, json!({ "iteration": 1 })),
            (ModelRequest, json!({})),
            (AssistantAttempt, json!({ "error": "stream reset" })),
            (
                ModelResponse,
                json!({ "model": "m1", "usage": { "input_tokens": 100, "output_tokens": 20 } }),
            ),
            (
                ToolResult,
                json!({ "outcome": "succeeded", "duration_ms": 30 }),
            ),
            (
                ToolResult,
                json!({ "outcome": "rejected", "duration_ms": 0 }),
            ),
            (ToolResult, json!({ "is_error": true, "duration_ms": 5 })),
            (MemoryWrite, json!({})),
            (StepEnd, json!({})),
            (StepStart, json!({ "iteration": 2 })),
            (ModelRequest, json!({})),
            (ModelRequestError, json!({ "kind": "rate_limit" })),
            (ModelRequest, json!({})),
            (
                ModelResponse,
                json!({ "model": "m2", "usage": { "input_tokens": 50, "output_tokens": 5 } }),
            ),
            (CompactionEnd, json!({})),
            (StepEnd, json!({})),
            (TurnEnd, json!({})),
            (TurnStart, json!({})),
            (TurnEnd, json!({ "error": "cancelled" })),
        ]);

        let metrics = SessionMetrics::from_events(&log);

        assert_eq!(metrics.turns, 2);
        assert_eq!(metrics.failed_turns, 1);
        assert_eq!(metrics.steps, 2);
        assert_eq!(metrics.model_requests, 3);
        assert_eq!(metrics.model_request_errors, 1);
        assert_eq!(metrics.discarded_attempts, 1);
        assert_eq!(metrics.tool_calls, 3);
        assert_eq!(metrics.tool_outcomes["succeeded"], 1);
        assert_eq!(metrics.tool_outcomes["rejected"], 1);
        assert_eq!(metrics.tool_outcomes["failed"], 1);
        assert_eq!(metrics.unsuccessful_tool_calls(), 2);
        assert_eq!(metrics.tool_duration_ms, 35);
        assert_eq!(metrics.input_tokens, 150);
        assert_eq!(metrics.output_tokens, 25);
        assert_eq!(metrics.models, vec!["m1", "m2"]);
        assert_eq!(metrics.compactions, 1);
        assert_eq!(metrics.memory_writes, 1);
        assert_eq!(metrics.wall_duration_ms, 1_900);
    }

    #[test]
    fn turn_facts_only_include_events_of_that_turn() {
        use SessionEventKind::*;
        let mut log = events(vec![
            (TurnStart, json!({})),
            (StepStart, json!({})),
            (SkillLoad, json!({ "name": "deploy" })),
            (
                ToolResult,
                json!({ "tool": "exec", "outcome": "unknown", "retry_safe": false }),
            ),
            (
                ToolResult,
                json!({ "tool": "memory_manage", "is_error": false }),
            ),
            (MemoryWrite, json!({})),
            (ModelRequestError, json!({})),
            (CompactionEnd, json!({})),
            (TurnEnd, json!({ "error": "cancelled" })),
            (TurnStart, json!({})),
            (
                ToolResult,
                json!({ "tool": "read_file", "outcome": "succeeded" }),
            ),
        ]);
        for event in log.iter_mut().skip(9) {
            event.turn_id = Some("t2".into());
        }

        let facts = TurnFacts::from_events(&log, "t1").unwrap();
        assert_eq!(facts.steps, 1);
        assert_eq!(
            facts.tool_calls,
            vec![
                ToolCallFact {
                    tool: "exec".into(),
                    outcome: "unknown".into(),
                    retry_safe: Some(false),
                },
                ToolCallFact {
                    tool: "memory_manage".into(),
                    outcome: "succeeded".into(),
                    retry_safe: None,
                },
            ]
        );
        assert_eq!(facts.unsuccessful_tool_calls(), 1);
        assert_eq!(facts.memory_writes, 1);
        assert_eq!(facts.model_request_errors, 1);
        assert_eq!(facts.compactions, 1);
        assert_eq!(facts.skills, vec!["deploy"]);
        assert_eq!(facts.error.as_deref(), Some("cancelled"));

        let second = TurnFacts::from_events(&log, "t2").unwrap();
        assert_eq!(second.tool_calls.len(), 1);
        assert!(second.error.is_none());
        assert!(TurnFacts::from_events(&log, "missing").is_none());
    }

    #[test]
    fn metrics_group_finished_turns_by_skill_version() {
        use SessionEventKind::*;
        let mut log = events(vec![
            (TurnStart, json!({})),
            (SkillLoad, json!({ "name": "deploy", "version": "v2" })),
            (SkillLoad, json!({ "name": "deploy", "version": "v2" })),
            (TurnEnd, json!({})),
            (TurnStart, json!({})),
            (SkillLoad, json!({ "name": "deploy", "version": "v2" })),
            (SkillLoad, json!({ "name": "notes" })),
            (TurnEnd, json!({ "error": "boom" })),
            (TurnStart, json!({})),
            (SkillLoad, json!({ "name": "deploy", "version": "v3" })),
        ]);
        for (event, turn) in log.iter_mut().zip([1, 1, 1, 1, 2, 2, 2, 2, 3, 3]) {
            event.turn_id = Some(format!("t{turn}"));
        }

        let metrics = SessionMetrics::from_events(&log);

        let stats = |turns, failed_turns| SkillTurnStats {
            turns,
            failed_turns,
        };
        assert_eq!(
            metrics.skill_turns,
            BTreeMap::from([
                ("deploy@v2".to_string(), stats(2, 1)),
                ("notes".to_string(), stats(1, 1)),
            ])
        );
    }

    #[test]
    fn empty_log_has_zero_metrics() {
        assert_eq!(SessionMetrics::from_events(&[]), SessionMetrics::default());
    }
}
