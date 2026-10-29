use blockcell_core::{FailureKind, Idempotency, RequestRecovery, ToolOutcome};
use blockcell_tools::ExecutionEnvironment;
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
    /// 处于进化候选评估时的候选 id，用于命中 Candidate 作用域的 listener。
    pub candidate_id: Option<String>,
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
    /// 回合结束。`error` 非空表示回合异常终止（此时没有最终呈现的 assistant 消息）。
    TurnEnd {
        final_response: String,
        error: Option<String>,
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
        tool_schema_bytes: usize,
    },
    /// 模型请求在 provider 回退与重试后仍然失败；`recovery` 是 Runtime 据 `kind` 决定的后续策略。
    AgentRequestError {
        error: String,
        kind: FailureKind,
        recovery: RequestRecovery,
    },
    /// 模型返回（包含模型提议但可能未被执行的 tool calls，用于对比"想做什么"与"做了什么"）。
    ModelResponse {
        content: String,
        tool_calls: Value,
        finish_reason: String,
        /// 实际服务本次调用的模型与 provider（provider 池回退后可能不是默认模型）。
        model: String,
        provider: String,
        /// provider 返回的原始 usage。
        usage: Value,
    },
    /// 模型产生但被丢弃的输出（流式中断后重试），用于对比"模型想做什么"与"最终做了什么"。
    AssistantAttempt {
        content: String,
        tool_calls: Vec<String>,
        error: String,
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
        /// 工具声明的执行环境（P2-3），用于审计与可观测性。
        environment: ExecutionEnvironment,
    },
    ToolPostExecute {
        tool_name: String,
        call_id: String,
        arguments: Value,
        result: Value,
        is_error: bool,
        duration_ms: u64,
    },
    /// 每个模型提议的调用都以一条 tool/result 结束（包括被拒绝、未开始的调用）。
    /// `outcome` 是 Runtime 对执行状态的判断，`result` 是返回给模型的内容；
    /// `idempotency` 与 `outcome` 一起决定该调用能否安全重试。
    ToolResult {
        tool_name: String,
        call_id: String,
        result: Value,
        duration_ms: u64,
        outcome: ToolOutcome,
        idempotency: Idempotency,
    },
    SkillActivated {
        name: String,
        /// 技能版本管理中的当前版本；未纳入版本管理的技能为空。
        version: Option<String>,
    },
    /// 记忆写入类工具成功执行。
    MemoryWrite {
        tool_name: String,
        call_id: String,
    },
    /// 工具在后台启动了子代理（成功返回 `task_id`）。
    SubagentSpawn {
        task_id: String,
        tool_name: String,
        call_id: String,
    },
    /// 后台子代理结束，回报到发起会话；通常发生在发起回合之外，没有 turn / step。
    SubagentCompleted {
        task_id: String,
        success: bool,
        summary: String,
    },
    /// 技能进化产物已部署；不属于任何会话，只通知 listener。
    EvolutionDeployed {
        skill: String,
    },
    /// 压缩开始。`phase` 标识触发点（pre-loop / in-loop / request-error / post-sampling / manual）。
    CompactionStart {
        phase: String,
        compactor: String,
        tokens_before: usize,
        messages_before: usize,
    },
    /// 压缩结束（成功或失败）。失败时 `error` 非空且上下文保持不变。
    CompactionEnd {
        phase: String,
        compactor: String,
        /// 生成摘要所用模型；不调用模型的策略为空。
        model: Option<String>,
        tokens_before: usize,
        tokens_after: usize,
        messages_before: usize,
        retained_messages: usize,
        summarized_messages: usize,
        error: Option<String>,
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
            Self::AssistantAttempt { .. } => "assistant/attempt",
            Self::ToolPreExecute { .. } => "tool/pre-execute",
            Self::ToolCall { .. } => "tool/call",
            Self::ToolPostExecute { .. } => "tool/post-execute",
            Self::ToolResult { .. } => "tool/result",
            Self::SkillActivated { .. } => "skill/load",
            Self::MemoryWrite { .. } => "memory/write",
            Self::SubagentSpawn { .. } => "subagent/spawn",
            Self::SubagentCompleted { .. } => "subagent/completed",
            Self::EvolutionDeployed { .. } => "evolution/deployed",
            Self::CompactionStart { .. } => "compaction/start",
            Self::CompactionEnd { .. } => "compaction/end",
        }
    }

    pub fn class(&self) -> EventClass {
        match self {
            Self::TurnStart { .. }
            | Self::TurnEnd { .. }
            | Self::StepStart { .. }
            | Self::StepEnd { .. }
            | Self::AgentRequest { .. }
            | Self::AgentRequestError { .. }
            | Self::ModelResponse { .. }
            | Self::AssistantAttempt { .. }
            | Self::ToolCall { .. }
            | Self::ToolResult { .. }
            | Self::SkillActivated { .. }
            | Self::MemoryWrite { .. }
            | Self::SubagentSpawn { .. }
            | Self::SubagentCompleted { .. }
            | Self::CompactionStart { .. }
            | Self::CompactionEnd { .. } => EventClass::Durable,
            Self::ToolPreExecute { .. } | Self::ToolPostExecute { .. } => EventClass::Capability,
            Self::AgentPreStep { .. } | Self::EvolutionDeployed { .. } => EventClass::Live,
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
    fn request_error_is_durable() {
        let error = AgentEvent::AgentRequestError {
            error: "http_status=400".into(),
            kind: FailureKind::InvalidRequest,
            recovery: FailureKind::InvalidRequest.request_recovery(),
        };
        assert_eq!(error.name(), "agent/request-error");
        assert_eq!(error.class(), EventClass::Durable);
    }

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
            error: None,
        };
        assert_eq!(end.class(), EventClass::Durable);
        assert!(!end.can_reject());

        let step = AgentEvent::AgentPreStep { iteration: 1 };
        assert_eq!(step.class(), EventClass::Live);
        assert_eq!(step.tool_name(), None);
    }
}
