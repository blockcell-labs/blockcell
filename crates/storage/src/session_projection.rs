//! Event → History 投影（Phase 5 起步）：从 Session Event Log 还原 transcript，
//! 并与现有 transcript 主存储做一致性校验。主存储暂不切换，投影只用于校验与回放。

use crate::session_event::{SessionEvent, SessionEventKind};
use blockcell_core::types::{ChatMessage, ToolCallRequest};
use serde::Serialize;
use serde_json::Value;

/// 从事件日志还原的一个回合。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProjectedTurn {
    pub turn_id: String,
    pub user: String,
    /// 以 `tool/result` 结束的工具调用 id，按发生顺序。
    pub tool_call_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 按回合首次出现的顺序还原回合；没有 turn_id 的事件（如回合外的子代理回报）被忽略。
pub fn project_turns(events: &[SessionEvent]) -> Vec<ProjectedTurn> {
    let mut turns: Vec<ProjectedTurn> = Vec::new();
    for event in events {
        let Some(turn_id) = event.turn_id.as_deref() else {
            continue;
        };
        let index = match turns.iter().position(|turn| turn.turn_id == turn_id) {
            Some(index) => index,
            None => {
                turns.push(ProjectedTurn {
                    turn_id: turn_id.to_string(),
                    ..ProjectedTurn::default()
                });
                turns.len() - 1
            }
        };
        let turn = &mut turns[index];
        let payload = &event.payload;
        match event.kind {
            SessionEventKind::UserMessage => turn.user = text_at(payload, "content"),
            SessionEventKind::ToolResult => turn.tool_call_ids.push(text_at(payload, "call_id")),
            SessionEventKind::AssistantMessage => {
                turn.final_response = Some(text_at(payload, "content"))
            }
            SessionEventKind::TurnEnd => {
                turn.error = payload.get("error").map(value_text);
            }
            _ => {}
        }
    }
    turns
}

/// 由事件日志投影出模型可见的对话历史：用户消息、带工具调用的助手消息、工具结果与最终回复。
pub fn project_history(events: &[SessionEvent]) -> Vec<ChatMessage> {
    let mut history = Vec::new();
    for event in events {
        let payload = &event.payload;
        match event.kind {
            SessionEventKind::UserMessage => {
                history.push(ChatMessage::user(&text_at(payload, "content")))
            }
            SessionEventKind::ModelResponse => {
                let tool_calls: Vec<ToolCallRequest> = payload
                    .get("tool_calls")
                    .cloned()
                    .and_then(|calls| serde_json::from_value(calls).ok())
                    .unwrap_or_default();
                // 没有工具调用的模型输出即最终回复，由 assistant/message 记录
                if !tool_calls.is_empty() {
                    let mut message = ChatMessage::assistant(&text_at(payload, "content"));
                    message.tool_calls = Some(tool_calls);
                    history.push(message);
                }
            }
            SessionEventKind::ToolResult => {
                let result = payload.get("result").map(value_text).unwrap_or_default();
                let mut message = ChatMessage::tool_result(&text_at(payload, "call_id"), &result);
                message.name = payload
                    .get("tool")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                history.push(message);
            }
            SessionEventKind::AssistantMessage => {
                history.push(ChatMessage::assistant(&text_at(payload, "content")))
            }
            _ => {}
        }
    }
    history
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionMismatchKind {
    /// transcript 中找不到该回合的用户消息（可能已被压缩或清理）。
    MissingInHistory,
    /// 回合内的工具调用与 transcript 不一致。
    ToolCallsDiffer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectionMismatch {
    pub turn_id: String,
    pub kind: ProjectionMismatchKind,
    pub detail: String,
}

/// 事件日志投影与 transcript 的一致性校验结果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProjectionCheck {
    pub projected_turns: usize,
    pub matched_turns: usize,
    pub mismatches: Vec<ProjectionMismatch>,
}

impl ProjectionCheck {
    pub fn is_consistent(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// 逐回合比对事件日志与 transcript：按用户消息对齐，再比较回合内工具调用 id 的顺序。
///
/// 只比较结构性事实（用户输入与工具调用），最终回复可能被响应缓存替换为占位，不参与比对。
pub fn verify_history(events: &[SessionEvent], history: &[ChatMessage]) -> ProjectionCheck {
    let turns = project_turns(events);
    let mut check = ProjectionCheck {
        projected_turns: turns.len(),
        ..ProjectionCheck::default()
    };
    let mut cursor = 0;
    for turn in turns.iter().filter(|turn| !turn.user.is_empty()) {
        let Some(offset) = history[cursor..]
            .iter()
            .position(|message| message.role == "user" && message_text(message) == turn.user)
        else {
            check.mismatches.push(ProjectionMismatch {
                turn_id: turn.turn_id.clone(),
                kind: ProjectionMismatchKind::MissingInHistory,
                detail: format!("user message not found: {}", preview(&turn.user)),
            });
            continue;
        };
        let start = cursor + offset + 1;
        let end = history[start..]
            .iter()
            .position(|message| message.role == "user")
            .map_or(history.len(), |offset| start + offset);
        let history_calls: Vec<String> = history[start..end]
            .iter()
            .filter_map(|message| message.tool_calls.as_ref())
            .flatten()
            .map(|call| call.id.clone())
            .collect();
        if history_calls == turn.tool_call_ids {
            check.matched_turns += 1;
        } else {
            check.mismatches.push(ProjectionMismatch {
                turn_id: turn.turn_id.clone(),
                kind: ProjectionMismatchKind::ToolCallsDiffer,
                detail: format!(
                    "events: [{}] transcript: [{}]",
                    turn.tool_call_ids.join(", "),
                    history_calls.join(", ")
                ),
            });
        }
        cursor = start;
    }
    check
}

fn text_at(payload: &Value, key: &str) -> String {
    payload.get(key).map(value_text).unwrap_or_default()
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn message_text(message: &ChatMessage) -> String {
    value_text(&message.content)
}

fn preview(text: &str) -> String {
    const MAX: usize = 80;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(MAX).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_event::SESSION_EVENT_SCHEMA_VERSION;
    use chrono::Utc;
    use serde_json::json;

    fn events(items: Vec<(&str, SessionEventKind, Value)>) -> Vec<SessionEvent> {
        items
            .into_iter()
            .enumerate()
            .map(|(index, (turn, kind, payload))| SessionEvent {
                schema_version: SESSION_EVENT_SCHEMA_VERSION,
                seq: index as u64 + 1,
                session_id: "cli:test".into(),
                timestamp: Utc::now(),
                turn_id: (!turn.is_empty()).then(|| turn.to_string()),
                step: None,
                kind,
                payload,
            })
            .collect()
    }

    fn sample_log() -> Vec<SessionEvent> {
        use SessionEventKind::*;
        events(vec![
            ("t1", TurnStart, json!({})),
            ("t1", UserMessage, json!({ "content": "list files" })),
            (
                "t1",
                ModelResponse,
                json!({
                    "content": "",
                    "tool_calls": [{
                        "id": "c1",
                        "type": "function",
                        "function": { "name": "list_dir", "arguments": "{\"path\":\".\"}" }
                    }]
                }),
            ),
            (
                "t1",
                ToolResult,
                json!({ "tool": "list_dir", "call_id": "c1", "result": { "entries": ["a"] } }),
            ),
            (
                "t1",
                ModelResponse,
                json!({ "content": "a", "tool_calls": [] }),
            ),
            ("t1", AssistantMessage, json!({ "content": "a" })),
            ("t1", TurnEnd, json!({})),
            ("", SubagentCompleted, json!({ "task_id": "x" })),
            ("t2", TurnStart, json!({})),
            ("t2", UserMessage, json!({ "content": "thanks" })),
            ("t2", TurnEnd, json!({ "error": "cancelled" })),
        ])
    }

    #[test]
    fn projects_turns_and_history_from_events() {
        let turns = project_turns(&sample_log());
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].user, "list files");
        assert_eq!(turns[0].tool_call_ids, vec!["c1"]);
        assert_eq!(turns[0].final_response.as_deref(), Some("a"));
        assert_eq!(turns[1].error.as_deref(), Some("cancelled"));

        let history = project_history(&sample_log());
        let roles: Vec<&str> = history.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(
            roles,
            vec!["user", "assistant", "tool", "assistant", "user"]
        );
        let calls = history[1].tool_calls.as_ref().unwrap();
        assert_eq!(calls[0].id, "c1");
        assert_eq!(calls[0].name, "list_dir");
        assert_eq!(history[2].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(history[2].name.as_deref(), Some("list_dir"));
        assert_eq!(history[2].content, json!("{\"entries\":[\"a\"]}"));
    }

    #[test]
    fn projected_history_is_consistent_with_itself() {
        let log = sample_log();
        let check = verify_history(&log, &project_history(&log));
        assert!(check.is_consistent(), "{check:?}");
        assert_eq!(check.projected_turns, 2);
        assert_eq!(check.matched_turns, 2);
    }

    #[test]
    fn reports_missing_turns_and_tool_call_drift() {
        let log = sample_log();
        let mut call = ChatMessage::assistant("");
        call.tool_calls = Some(vec![ToolCallRequest {
            id: "other".into(),
            name: "list_dir".into(),
            arguments: json!({}),
            thought_signature: None,
        }]);
        let history = vec![
            ChatMessage::user("list files"),
            call,
            ChatMessage::assistant("a"),
        ];

        let check = verify_history(&log, &history);
        assert_eq!(check.matched_turns, 0);
        assert_eq!(
            check
                .mismatches
                .iter()
                .map(|mismatch| (mismatch.turn_id.as_str(), mismatch.kind))
                .collect::<Vec<_>>(),
            vec![
                ("t1", ProjectionMismatchKind::ToolCallsDiffer),
                ("t2", ProjectionMismatchKind::MissingInHistory),
            ]
        );
    }
}
