use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::runtime_kernel::{
    AgentEvent, AgentEventContext, AgentEventListener, EventDecision, ListenerSpec,
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
