use serde_json::Value;

/// 事件一致性语义分类。
///
/// - `Durable`：必须写入 Session Log、可 replay 的事实；
/// - `Live`：仅描述当前运行状态，不保证持久化；
/// - `Capability`：绑定某个能力 seam（如 tool），用于 policy / adapter 拦截。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventClass {
    Durable,
    Live,
    Capability,
}

/// 事件派发上下文：标识事件属于哪个 agent / session / turn / step 以及会话来源。
#[derive(Debug, Clone, Default)]
pub struct AgentEventContext {
    pub agent_id: String,
    pub session_id: String,
    pub channel: String,
    pub chat_id: String,
    pub turn_id: Option<String>,
    pub step: Option<u32>,
}

/// Listener 对事件的处理结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventDecision {
    Continue,
    Reject { reason: String },
}

/// Runtime 事件。事件名与语义是 runtime contract，而不是普通日志点。
///
/// 只有 [`AgentEvent::can_reject`] 为真的事件允许 listener 拒绝；
/// 只有 `ToolPreExecute.arguments` 会在派发后被 runtime 读回（即唯一可修改输入）。
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    TurnStart {
        user_input: String,
        is_new_session: bool,
    },
    TurnEnd {
        final_response: String,
    },
    AgentPreStep {
        iteration: u32,
    },
    StepStart {
        iteration: u32,
    },
    StepEnd {
        iteration: u32,
        tool_calls: usize,
    },
    AgentRequest {
        message_count: usize,
        tool_count: usize,
    },
    AgentRequestError {
        error: String,
    },
    /// 模型返回（包含模型提议但可能未被执行的 tool calls，用于对比"想做什么"与"做了什么"）。
    ModelResponse {
        content: String,
        tool_calls: Value,
        finish_reason: String,
    },
    ToolPreExecute {
        tool_name: String,
        call_id: String,
        arguments: Value,
    },
    /// 通过所有 guard 与 pre-execute 拦截后真正发起的工具调用。
    ToolCall {
        tool_name: String,
        call_id: String,
        arguments: Value,
    },
    ToolPostExecute {
        tool_name: String,
        call_id: String,
        arguments: Value,
        result: Value,
        is_error: bool,
        duration_ms: u64,
    },
    ToolResult {
        tool_name: String,
        call_id: String,
        result: Value,
        is_error: bool,
        duration_ms: u64,
    },
    SkillActivated {
        name: String,
    },
    Compaction {
        phase: String,
        messages_before: usize,
        messages_after: usize,
    },
}

impl AgentEvent {
    pub fn name(&self) -> &'static str {
        match self {
            Self::TurnStart { .. } => "turn/start",
            Self::TurnEnd { .. } => "turn/end",
            Self::AgentPreStep { .. } => "agent/pre-step",
            Self::StepStart { .. } => "step/start",
            Self::StepEnd { .. } => "step/end",
            Self::AgentRequest { .. } => "agent/request",
            Self::AgentRequestError { .. } => "agent/request-error",
            Self::ModelResponse { .. } => "model/response",
            Self::ToolPreExecute { .. } => "tool/pre-execute",
            Self::ToolCall { .. } => "tool/call",
            Self::ToolPostExecute { .. } => "tool/post-execute",
            Self::ToolResult { .. } => "tool/result",
            Self::SkillActivated { .. } => "skill/load",
            Self::Compaction { .. } => "compaction",
        }
    }

    pub fn class(&self) -> EventClass {
        match self {
            Self::TurnStart { .. }
            | Self::TurnEnd { .. }
            | Self::StepStart { .. }
            | Self::StepEnd { .. }
            | Self::AgentRequest { .. }
            | Self::ModelResponse { .. }
            | Self::ToolCall { .. }
            | Self::ToolResult { .. }
            | Self::SkillActivated { .. }
            | Self::Compaction { .. } => EventClass::Durable,
            Self::ToolPreExecute { .. } | Self::ToolPostExecute { .. } => EventClass::Capability,
            Self::AgentPreStep { .. } | Self::AgentRequestError { .. } => EventClass::Live,
        }
    }

    /// 是否允许 listener 拒绝（阻断后续执行）。
    pub fn can_reject(&self) -> bool {
        matches!(self, Self::ToolPreExecute { .. })
    }

    pub fn tool_name(&self) -> Option<&str> {
        match self {
            Self::ToolPreExecute { tool_name, .. }
            | Self::ToolCall { tool_name, .. }
            | Self::ToolPostExecute { tool_name, .. }
            | Self::ToolResult { tool_name, .. } => Some(tool_name),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_names_and_classes_follow_contract() {
        let pre = AgentEvent::ToolPreExecute {
            tool_name: "exec".into(),
            call_id: "c1".into(),
            arguments: json!({}),
        };
        assert_eq!(pre.name(), "tool/pre-execute");
        assert_eq!(pre.class(), EventClass::Capability);
        assert!(pre.can_reject());
        assert_eq!(pre.tool_name(), Some("exec"));

        let end = AgentEvent::TurnEnd {
            final_response: "ok".into(),
        };
        assert_eq!(end.class(), EventClass::Durable);
        assert!(!end.can_reject());

        let step = AgentEvent::AgentPreStep { iteration: 1 };
        assert_eq!(step.class(), EventClass::Live);
        assert_eq!(step.tool_name(), None);
    }
}
