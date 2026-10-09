//! Agent contract 与默认 loop driver 分离。
//!
//! `AgentRuntime` 持有能力（tools / providers / memory / event bus），
//! [`AgentLoop`] 决定一次 turn 如何驱动这些能力。替换 loop 不需要改动能力实现，
//! 未来 Self-Evolution 可以产出"行为策略"而不仅仅是 Skill。
//!
//! 当前只提供 [`DefaultLoop`]（即现有主循环）。

use async_trait::async_trait;
use blockcell_core::{InboundMessage, Result};

use super::AgentRuntime;

#[async_trait]
pub trait AgentLoop: Send + Sync {
    fn name(&self) -> &'static str;

    /// 处理一条入站消息并返回最终回复。调用前 runtime 已建立 AbortToken 与 AgentIdentity 上下文。
    async fn run(&self, runtime: &mut AgentRuntime, msg: InboundMessage) -> Result<String>;
}

/// 默认 loop：Input Claim → Context Build → (PreStep → Model Request → Tool Dispatch → Result Commit)* → Turn Close。
pub struct DefaultLoop;

#[async_trait]
impl AgentLoop for DefaultLoop {
    fn name(&self) -> &'static str {
        "default"
    }

    async fn run(&self, runtime: &mut AgentRuntime, msg: InboundMessage) -> Result<String> {
        runtime.process_message_inner(msg).await
    }
}
