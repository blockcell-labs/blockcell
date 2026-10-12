use blockcell_core::Paths;
use blockcell_storage::{
    JsonlSessionEventLog, SessionEvent, SessionEventKind, SessionEventReader, SessionMetrics,
};
use serde_json::Value;

const PREVIEW_CHARS: usize = 200;

fn event_log(paths: &Paths, agent: Option<&str>) -> JsonlSessionEventLog {
    JsonlSessionEventLog::new(paths.for_agent(agent.unwrap_or("default")))
}

/// `blockcell session list`：列出有事件日志、可回放的会话。
pub fn list(paths: &Paths, agent: Option<&str>) -> anyhow::Result<()> {
    let sessions = event_log(paths, agent).list_sessions()?;
    if sessions.is_empty() {
        println!("No session event logs found.");
        return Ok(());
    }
    for info in sessions {
        println!(
            "{}  {}",
            info.modified
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S"),
            info.session_id
        );
    }
    Ok(())
}

/// `blockcell session replay <id>`：按事件日志重放会话的 turn / step / model / tool 时间线。
pub fn replay(
    paths: &Paths,
    agent: Option<&str>,
    session_id: &str,
    json: bool,
    full: bool,
) -> anyhow::Result<()> {
    let events = event_log(paths, agent).read(session_id)?;
    if events.is_empty() {
        anyhow::bail!(
            "No session events found for '{session_id}'. Use `blockcell session list` to see available sessions."
        );
    }
    let metrics = SessionMetrics::from_events(&events);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "session_id": session_id,
                "events": events,
                "metrics": metrics,
            }))?
        );
        return Ok(());
    }
    println!("Session {session_id} ({} events)\n", events.len());
    for event in &events {
        println!(
            "{:>5}  {}  {}",
            event.seq,
            event
                .timestamp
                .with_timezone(&chrono::Local)
                .format("%H:%M:%S%.3f"),
            render_event(event, full)
        );
    }
    println!("\n{}", render_metrics(&metrics));
    Ok(())
}

fn render_event(event: &SessionEvent, full: bool) -> String {
    let payload = &event.payload;
    let text = |key: &str| preview(&string_at(payload, key), full);
    let step = event.step.map(|step| step.to_string()).unwrap_or_default();
    match event.kind {
        SessionEventKind::SessionStart => "session start".to_string(),
        SessionEventKind::TurnStart => {
            format!("== turn {}", event.turn_id.as_deref().unwrap_or("?"))
        }
        SessionEventKind::TurnEnd => match payload.get("error") {
            Some(error) => format!("== turn end (error: {})", value_text(error)),
            None => "== turn end".to_string(),
        },
        SessionEventKind::UserMessage => format!("  user: {}", text("content")),
        SessionEventKind::AssistantMessage => format!("  assistant: {}", text("content")),
        SessionEventKind::StepStart => format!("  -- step {step}"),
        SessionEventKind::StepEnd => format!(
            "  -- step {step} end ({} tool calls)",
            u64_at(payload, &["tool_calls"])
        ),
        SessionEventKind::ModelRequest => format!(
            "    > model request (messages={}, tools={})",
            u64_at(payload, &["message_count"]),
            u64_at(payload, &["tool_count"])
        ),
        SessionEventKind::ModelResponse => {
            let mut line = format!(
                "    < model response {} finish={} tokens={}/{}",
                string_at(payload, "model"),
                string_at(payload, "finish_reason"),
                u64_at(payload, &["usage", "input_tokens"]),
                u64_at(payload, &["usage", "output_tokens"])
            );
            let tool_calls = tool_call_names(payload);
            if !tool_calls.is_empty() {
                line.push_str(&format!(" calls=[{}]", tool_calls.join(", ")));
            }
            let content = text("content");
            if !content.is_empty() {
                line.push_str(&format!("\n{:24}{content}", ""));
            }
            line
        }
        SessionEventKind::ModelRequestError => format!(
            "    ! model request error [{}]: {}",
            string_at(payload, "kind"),
            text("error")
        ),
        SessionEventKind::AssistantAttempt => {
            format!("    ~ discarded attempt: {}", text("error"))
        }
        SessionEventKind::ToolCall => format!(
            "    * tool call {} {}",
            string_at(payload, "tool"),
            preview(&value_text(&payload["arguments"]), full)
        ),
        SessionEventKind::ToolResult => format!(
            "    = tool result {} [{}] {}ms {}",
            string_at(payload, "tool"),
            blockcell_storage::session_metrics::tool_outcome(payload),
            u64_at(payload, &["duration_ms"]),
            preview(&value_text(&payload["result"]), full)
        ),
        SessionEventKind::SkillLoad => format!("  skill load {}", string_at(payload, "name")),
        SessionEventKind::MemoryWrite => {
            format!("    memory write via {}", string_at(payload, "tool"))
        }
        SessionEventKind::Compaction => "  compaction".to_string(),
        SessionEventKind::CompactionStart => format!(
            "  compaction start [{}] {} tokens",
            string_at(payload, "phase"),
            u64_at(payload, &["tokens_before"])
        ),
        SessionEventKind::CompactionEnd => match payload.get("error").filter(|e| !e.is_null()) {
            Some(error) => format!("  compaction failed: {}", value_text(error)),
            None => format!(
                "  compaction end {} -> {} tokens",
                u64_at(payload, &["tokens_before"]),
                u64_at(payload, &["tokens_after"])
            ),
        },
    }
}

fn render_metrics(metrics: &SessionMetrics) -> String {
    let outcomes = metrics
        .tool_outcomes
        .iter()
        .map(|(outcome, count)| format!("{outcome}={count}"))
        .collect::<Vec<_>>()
        .join(" ");
    [
        "Summary".to_string(),
        format!(
            "  turns: {} ({} failed)   steps: {}",
            metrics.turns, metrics.failed_turns, metrics.steps
        ),
        format!(
            "  model requests: {}   request errors: {}   discarded attempts: {}",
            metrics.model_requests, metrics.model_request_errors, metrics.discarded_attempts
        ),
        format!(
            "  tool calls: {} ({})   tool time: {}ms",
            metrics.tool_calls, outcomes, metrics.tool_duration_ms
        ),
        format!(
            "  tokens: {} in / {} out   models: {}",
            metrics.input_tokens,
            metrics.output_tokens,
            if metrics.models.is_empty() {
                "-".to_string()
            } else {
                metrics.models.join(", ")
            }
        ),
        format!(
            "  compactions: {}   memory writes: {}   duration: {}ms",
            metrics.compactions, metrics.memory_writes, metrics.wall_duration_ms
        ),
    ]
    .join("\n")
}

fn tool_call_names(payload: &Value) -> Vec<String> {
    payload["tool_calls"]
        .as_array()
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| call["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn string_at(payload: &Value, key: &str) -> String {
    payload.get(key).map(value_text).unwrap_or_default()
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn u64_at(value: &Value, path: &[&str]) -> u64 {
    path.iter()
        .try_fold(value, |value, key| value.get(key))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn preview(text: &str, full: bool) -> String {
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if full || single_line.chars().count() <= PREVIEW_CHARS {
        return single_line;
    }
    let kept: String = single_line.chars().take(PREVIEW_CHARS).collect();
    format!("{kept}...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(kind: SessionEventKind, payload: Value) -> SessionEvent {
        SessionEvent {
            schema_version: 1,
            seq: 1,
            session_id: "cli:test".into(),
            timestamp: chrono::Utc::now(),
            turn_id: Some("t1".into()),
            step: Some(2),
            kind,
            payload,
        }
    }

    #[test]
    fn renders_model_and_tool_events_with_outcome_and_usage() {
        let response = render_event(
            &event(
                SessionEventKind::ModelResponse,
                json!({
                    "model": "deepseek-chat",
                    "finish_reason": "tool_calls",
                    "usage": { "input_tokens": 120, "output_tokens": 8 },
                    "tool_calls": [{ "name": "read_file" }],
                    "content": "",
                }),
            ),
            false,
        );
        assert_eq!(
            response,
            "    < model response deepseek-chat finish=tool_calls tokens=120/8 calls=[read_file]"
        );

        let result = render_event(
            &event(
                SessionEventKind::ToolResult,
                json!({ "tool": "exec", "outcome": "rejected", "duration_ms": 0, "result": {"error": "denied"} }),
            ),
            false,
        );
        assert_eq!(
            result,
            r#"    = tool result exec [rejected] 0ms {"error":"denied"}"#
        );

        let step = render_event(&event(SessionEventKind::StepStart, json!({})), false);
        assert_eq!(step, "  -- step 2");
    }

    #[test]
    fn previews_are_single_line_and_truncated_unless_full() {
        let long = "a\nb ".repeat(200);
        assert!(!preview(&long, false).contains('\n'));
        assert!(preview(&long, false).ends_with("..."));
        assert_eq!(preview(&long, true).chars().count(), 799);
    }
}
