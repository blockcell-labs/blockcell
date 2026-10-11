use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use blockcell_storage::{NewSessionEvent, SessionEventKind, SessionEventWriter};
use tokio::sync::broadcast;

use crate::runtime_kernel::{
    AgentEvent, AgentEventContext, AgentEventListener, EventClass, EventDecision, ListenerSpec,
};

pub(crate) type EventTxSlot = Arc<RwLock<Option<broadcast::Sender<String>>>>;

/// 将工具生命周期事件转发给 WebSocket 客户端（`tool_call_start` / `tool_call_result`）。
///
/// 以最低优先级运行，确保推送的参数是所有拦截器处理后的最终值。
pub(crate) struct WsToolEventListener {
    event_tx: EventTxSlot,
}

impl WsToolEventListener {
    pub(crate) fn new(event_tx: EventTxSlot) -> Self {
        Self { event_tx }
    }
}

#[async_trait]
impl AgentEventListener for WsToolEventListener {
    fn spec(&self) -> ListenerSpec {
        ListenerSpec::new("ws_tool_events").priority(i32::MAX)
    }

    async fn handle(
        &self,
        ctx: &AgentEventContext,
        event: &mut AgentEvent,
    ) -> blockcell_core::Result<EventDecision> {
        let payload = match event {
            AgentEvent::ToolPreExecute {
                tool_name,
                call_id,
                arguments,
            } => serde_json::json!({
                "type": "tool_call_start",
                "channel": ctx.channel,
                "agent_id": ctx.agent_id,
                "chat_id": ctx.chat_id,
                "task_id": "",
                "tool": tool_name,
                "call_id": call_id,
                "params": arguments,
            }),
            AgentEvent::ToolResult {
                tool_name,
                call_id,
                result,
                duration_ms,
                ..
            } => serde_json::json!({
                "type": "tool_call_result",
                "channel": ctx.channel,
                "agent_id": ctx.agent_id,
                "chat_id": ctx.chat_id,
                "task_id": "",
                "tool": tool_name,
                "call_id": call_id,
                "result": result,
                "duration_ms": duration_ms,
            }),
            _ => return Ok(EventDecision::Continue),
        };
        if let Some(tx) = self
            .event_tx
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            let _ = tx.send(payload.to_string());
        }
        Ok(EventDecision::Continue)
    }
}

/// 将 Durable 事件追加到会话事件日志（Session Event Log）。
///
/// 只记录事实，不参与控制流；写入失败按 fail-open 记录 warning。
/// 文件 I/O（加锁、读尾部 seq、追加）在 blocking 线程池执行，不占用 async worker。
pub(crate) struct SessionEventRecorder {
    writer: Arc<dyn SessionEventWriter>,
}

impl SessionEventRecorder {
    pub(crate) fn new(writer: Arc<dyn SessionEventWriter>) -> Self {
        Self { writer }
    }
}

const MEMORY_WRITE_TOOLS: [&str; 4] = [
    "memory_manage",
    "memory_upsert",
    "memory_forget",
    "auto_memory",
];

fn session_events_for(event: &AgentEvent) -> Vec<(SessionEventKind, serde_json::Value)> {
    use serde_json::json;
    match event {
        AgentEvent::TurnStart {
            user_input,
            is_new_session,
        } => {
            let mut events = Vec::new();
            if *is_new_session {
                events.push((SessionEventKind::SessionStart, json!({})));
            }
            events.push((SessionEventKind::TurnStart, json!({})));
            events.push((
                SessionEventKind::UserMessage,
                json!({ "content": user_input }),
            ));
            events
        }
        AgentEvent::TurnEnd {
            final_response,
            error: None,
        } => vec![
            (
                SessionEventKind::AssistantMessage,
                json!({ "content": final_response }),
            ),
            (SessionEventKind::TurnEnd, json!({})),
        ],
        AgentEvent::TurnEnd {
            error: Some(error), ..
        } => vec![(SessionEventKind::TurnEnd, json!({ "error": error }))],
        AgentEvent::StepStart { iteration } => vec![(
            SessionEventKind::StepStart,
            json!({ "iteration": iteration }),
        )],
        AgentEvent::StepEnd {
            iteration,
            tool_calls,
        } => vec![(
            SessionEventKind::StepEnd,
            json!({ "iteration": iteration, "tool_calls": tool_calls }),
        )],
        AgentEvent::AgentRequest {
            message_count,
            tool_count,
        } => vec![(
            SessionEventKind::ModelRequest,
            json!({ "message_count": message_count, "tool_count": tool_count }),
        )],
        AgentEvent::ModelResponse {
            content,
            tool_calls,
            finish_reason,
        } => vec![(
            SessionEventKind::ModelResponse,
            json!({ "content": content, "tool_calls": tool_calls, "finish_reason": finish_reason }),
        )],
        AgentEvent::AssistantAttempt {
            content,
            tool_calls,
            error,
        } => vec![(
            SessionEventKind::AssistantAttempt,
            json!({ "content": content, "tool_calls": tool_calls, "error": error }),
        )],
        AgentEvent::ToolCall {
            tool_name,
            call_id,
            arguments,
        } => vec![(
            SessionEventKind::ToolCall,
            json!({ "tool": tool_name, "call_id": call_id, "arguments": arguments }),
        )],
        AgentEvent::ToolResult {
            tool_name,
            call_id,
            result,
            is_error,
            duration_ms,
            failure,
            idempotency,
        } => {
            let mut events = vec![(
                SessionEventKind::ToolResult,
                json!({
                    "tool": tool_name,
                    "call_id": call_id,
                    "result": result,
                    "is_error": is_error,
                    "duration_ms": duration_ms,
                    "outcome": failure.map_or("succeeded", |kind| kind.as_str()),
                    "idempotency": idempotency,
                    "retry_safe": failure.map(|kind| kind.retry_allowed(*idempotency)),
                }),
            )];
            if !is_error && MEMORY_WRITE_TOOLS.contains(&tool_name.as_str()) {
                events.push((
                    SessionEventKind::MemoryWrite,
                    json!({ "tool": tool_name, "call_id": call_id }),
                ));
            }
            events
        }
        AgentEvent::SkillActivated { name } => {
            vec![(SessionEventKind::SkillLoad, json!({ "name": name }))]
        }
        AgentEvent::Compaction {
            phase,
            messages_before,
            messages_after,
        } => vec![(
            SessionEventKind::Compaction,
            json!({
                "phase": phase,
                "messages_before": messages_before,
                "messages_after": messages_after,
            }),
        )],
        _ => Vec::new(),
    }
}

#[async_trait]
impl AgentEventListener for SessionEventRecorder {
    fn spec(&self) -> ListenerSpec {
        ListenerSpec::new("session_event_log").priority(i32::MAX - 1)
    }

    async fn handle(
        &self,
        ctx: &AgentEventContext,
        event: &mut AgentEvent,
    ) -> blockcell_core::Result<EventDecision> {
        if event.class() != EventClass::Durable || ctx.session_id.is_empty() {
            return Ok(EventDecision::Continue);
        }
        let events: Vec<NewSessionEvent> = session_events_for(event)
            .into_iter()
            .map(|(kind, payload)| NewSessionEvent {
                turn_id: ctx.turn_id.clone(),
                step: ctx.step,
                kind,
                payload,
            })
            .collect();
        if events.is_empty() {
            return Ok(EventDecision::Continue);
        }
        let writer = Arc::clone(&self.writer);
        let session_id = ctx.session_id.clone();
        tokio::task::spawn_blocking(move || {
            events
                .into_iter()
                .try_for_each(|event| writer.append(&session_id, event).map(|_| ()))
        })
        .await
        .map_err(|error| blockcell_core::Error::Storage(error.to_string()))??;
        Ok(EventDecision::Continue)
    }
}
