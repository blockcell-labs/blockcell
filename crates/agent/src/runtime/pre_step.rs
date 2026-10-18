//! agent/pre-step：每次模型请求前的统一决策门。
//!
//! ```text
//! begin step
//!  ↓
//! agent/pre-step → step/start
//!  ├── budget      预算已耗尽则不再发起请求
//!  ├── compaction  上下文超过阈值先压缩
//!  ├── context     注入变化的计划 / 项目环境、steering 消息，工具轮之后的节流
//!  └── policies    可注册的策略链（Waterfall）：可改写本步输入或拒绝开始
//!  ↓
//! model request
//! ```
//!
//! pre-step 只决定"本步能否开始、以什么输入开始"，不执行工具。

use super::*;
use crate::runtime_kernel::Waterfall;

/// 可注册策略看到并可改写的本步状态。
#[derive(Debug, Clone)]
pub struct PreStepState {
    pub agent_id: String,
    pub session_key: String,
    pub iteration: u32,
    /// 本步将发送给模型的消息；策略可追加或改写。
    pub messages: Vec<ChatMessage>,
    pub decision: PreStepDecision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreStepDecision {
    Continue,
    /// 不发起本步请求，以 `reason` 作为本次循环的最终回复。
    Stop {
        reason: String,
    },
}

pub(crate) struct PreStepOutcome {
    pub(crate) decision: PreStepDecision,
    pub(crate) compacted: bool,
}

impl AgentRuntime {
    /// 注册 pre-step 策略，按注册顺序在内置策略之后运行。
    pub fn register_pre_step_policy(&mut self, policy: Arc<dyn Waterfall<PreStepState>>) {
        self.pre_step_policies.push(policy);
    }

    /// 运行 pre-step。`history` 为空时（技能内循环）跳过压缩与 steering 注入；
    /// `throttle` 为工具轮之后的节流时长。返回 Stop 时本步已关闭（step/end）。
    pub(crate) async fn run_pre_step(
        &mut self,
        msg: &InboundMessage,
        session_key: &str,
        iteration: u32,
        messages: &mut Vec<ChatMessage>,
        mut history: Option<&mut Vec<ChatMessage>>,
        throttle: Option<std::time::Duration>,
    ) -> PreStepOutcome {
        self.dispatch_event(
            msg,
            session_key,
            &mut AgentEvent::AgentPreStep { iteration },
        )
        .await;
        self.dispatch_event(msg, session_key, &mut AgentEvent::StepStart { iteration })
            .await;

        let mut decision = self.budget_pre_step(session_key);

        let mut compacted = false;
        if decision == PreStepDecision::Continue {
            if let Some(history) = history.as_deref_mut() {
                if self.context_exceeds_compact_threshold(session_key, messages) {
                    compacted = self
                        .apply_layer4_compact_in_loop(
                            messages,
                            history,
                            msg,
                            session_key,
                            "pre-step",
                        )
                        .await;
                }
            }
            self.prepare_step_context(msg, session_key, messages, history, throttle)
                .await;
            decision = self
                .run_pre_step_policies(session_key, iteration, messages)
                .await;
        }

        if let PreStepDecision::Stop { reason } = &decision {
            info!(iteration, reason = %reason, "agent/pre-step stopped the step");
            self.end_step(msg, session_key, iteration, 0).await;
        }
        PreStepOutcome {
            decision,
            compacted,
        }
    }

    /// 预算在上一步已耗尽时不再发起新请求（模型调用后的超额检查仍由调用方负责）。
    fn budget_pre_step(&self, session_key: &str) -> PreStepDecision {
        match self.budget_tracker_for_session(session_key).check_budget() {
            Ok(_) => PreStepDecision::Continue,
            Err(error) => PreStepDecision::Stop {
                reason: format!("⚠️ {}", error.message),
            },
        }
    }

    /// 记录上下文窗口占用，并判断是否达到 Layer4 压缩阈值。
    pub(crate) fn context_exceeds_compact_threshold(
        &self,
        session_key: &str,
        messages: &[ChatMessage],
    ) -> bool {
        let Some(memory_system) = self.memory_system.as_ref() else {
            return false;
        };
        let estimated_tokens = estimate_messages_tokens(messages);
        let token_budget = memory_system.config().token_budget;
        let threshold = memory_system.config().layer4.compact_threshold_ratio;
        self.record_context_window_estimate(session_key, estimated_tokens, token_budget, threshold);
        crate::memory_event!(
            layer4,
            token_usage,
            estimated_tokens,
            token_budget,
            threshold
        );
        let exceeded = memory_system.should_compact(estimated_tokens);
        if exceeded {
            info!(
                estimated_tokens,
                token_budget, threshold, "[layer4] Compact threshold reached before model request"
            );
        }
        exceeded
    }

    async fn prepare_step_context(
        &mut self,
        msg: &InboundMessage,
        session_key: &str,
        messages: &mut Vec<ChatMessage>,
        history: Option<&mut Vec<ChatMessage>>,
        throttle: Option<std::time::Duration>,
    ) {
        // 计划由 plan 工具按来源会话写入。
        if let Some(plan) = blockcell_tools::plan::take_changed_plan_context(&msg.session_key()) {
            messages.push(ChatMessage::user(&format!(
                "<runtime-context>\n{plan}\n</runtime-context>"
            )));
        }
        if let Some(environment) = self
            .context_builder
            .incremental_project_environment_async(session_key)
            .await
        {
            messages.push(ChatMessage::user(&format!(
                "<runtime-context>\n{environment}\n</runtime-context>"
            )));
        }
        if let Some(delay) = throttle {
            info!(
                delay_ms = delay.as_millis() as u64,
                "Throttling next LLM call after tool round"
            );
            tokio::time::sleep(delay).await;
        }
        if let Some(history) = history {
            let injected = self.drain_steering_messages(messages, history, msg);
            if injected > 0 {
                debug!(
                    injected,
                    messages_len = messages.len(),
                    "Steering messages injected before LLM call"
                );
            }
        }
    }

    /// 可注册策略链；策略出错时 fail-open：记录告警并保留原输入继续。
    async fn run_pre_step_policies(
        &self,
        session_key: &str,
        iteration: u32,
        messages: &mut Vec<ChatMessage>,
    ) -> PreStepDecision {
        if self.pre_step_policies.is_empty() {
            return PreStepDecision::Continue;
        }
        let state = PreStepState {
            agent_id: self
                .agent_id
                .clone()
                .unwrap_or_else(|| "default".to_string()),
            session_key: session_key.to_string(),
            iteration,
            messages: messages.clone(),
            decision: PreStepDecision::Continue,
        };
        match self.pre_step_policies.run(state).await {
            Ok(state) => {
                *messages = state.messages;
                state.decision
            }
            Err(error) => {
                warn!(error = %error, iteration, "agent/pre-step policy failed; continuing with original input");
                PreStepDecision::Continue
            }
        }
    }
}
