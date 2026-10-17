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
            } => {
                // 凭据类参数（password/api_key/Authorization 头）不能明文进
                // dashboard 事件流；与 storage audit 同口径掩码。
                let mut masked = arguments.clone();
                blockcell_core::config::mask_secret_values(&mut masked);
                serde_json::json!({
                    "type": "tool_call_start",
                    "channel": ctx.channel,
                    "agent_id": ctx.agent_id,
                    "chat_id": ctx.chat_id,
                    "task_id": "",
                    "tool": tool_name,
                    "call_id": call_id,
                    "params": masked,
                })
            }
            AgentEvent::ToolResult {
                tool_name,
                call_id,
                result,
                duration_ms,
                outcome,
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
                "is_error": outcome.is_error(),
                "outcome": outcome,
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
        AgentEvent::AgentRequestError {
            error,
            kind,
            recovery,
        } => vec![(
            SessionEventKind::ModelRequestError,
            json!({ "error": error, "kind": kind, "recovery": recovery }),
        )],
        AgentEvent::ModelResponse {
            content,
            tool_calls,
            finish_reason,
            model,
            provider,
            usage,
        } => {
            let (input_tokens, output_tokens) = super::extract_llm_usage_tokens(usage);
            let (cache_read_tokens, cache_creation_tokens) = super::extract_llm_cache_tokens(usage);
            vec![(
                SessionEventKind::ModelResponse,
                json!({
                    "content": content,
                    "tool_calls": tool_calls,
                    "finish_reason": finish_reason,
                    "model": model,
                    "provider": provider,
                    "usage": {
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens,
                        "cache_read_tokens": cache_read_tokens,
                        "cache_creation_tokens": cache_creation_tokens,
                    },
                }),
            )]
        }
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
            environment,
        } => vec![(
            SessionEventKind::ToolCall,
            json!({
                "tool": tool_name,
                "call_id": call_id,
                "arguments": arguments,
                "environment": environment.as_str(),
            }),
        )],
        AgentEvent::ToolResult {
            tool_name,
            call_id,
            result,
            duration_ms,
            outcome,
            idempotency,
        } => vec![(
            SessionEventKind::ToolResult,
            json!({
                "tool": tool_name,
                "call_id": call_id,
                "result": result,
                "is_error": outcome.is_error(),
                "duration_ms": duration_ms,
                "outcome": outcome,
                "idempotency": idempotency,
                "retry_safe": outcome.retry_allowed(*idempotency),
            }),
        )],
        AgentEvent::SkillActivated { name, version } => vec![(
            SessionEventKind::SkillLoad,
            json!({ "name": name, "version": version }),
        )],
        AgentEvent::MemoryWrite { tool_name, call_id } => vec![(
            SessionEventKind::MemoryWrite,
            json!({ "tool": tool_name, "call_id": call_id }),
        )],
        AgentEvent::SubagentSpawn {
            task_id,
            tool_name,
            call_id,
        } => vec![(
            SessionEventKind::SubagentSpawn,
            json!({ "task_id": task_id, "tool": tool_name, "call_id": call_id }),
        )],
        AgentEvent::SubagentCompleted {
            task_id,
            success,
            summary,
        } => vec![(
            SessionEventKind::SubagentCompleted,
            json!({ "task_id": task_id, "success": success, "summary": summary }),
        )],
        AgentEvent::CompactionStart {
            phase,
            compactor,
            tokens_before,
            messages_before,
        } => vec![(
            SessionEventKind::CompactionStart,
            json!({
                "phase": phase,
                "compactor": compactor,
                "tokens_before": tokens_before,
                "messages_before": messages_before,
            }),
        )],
        AgentEvent::CompactionEnd {
            phase,
            compactor,
            model,
            tokens_before,
            tokens_after,
            messages_before,
            retained_messages,
            summarized_messages,
            error,
        } => vec![(
            SessionEventKind::CompactionEnd,
            json!({
                "phase": phase,
                "compactor": compactor,
                "model": model,
                "tokens_before": tokens_before,
                "tokens_after": tokens_after,
                "messages_before": messages_before,
                "retained_messages": retained_messages,
                "summarized_messages": summarized_messages,
                "error": error,
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
