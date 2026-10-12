//! 上下文压缩策略 seam：Runtime 负责编排（hooks、熔断、通知、恢复信息、事件），
//! [`ContextCompactor`] 只负责"如何把一段对话压缩成摘要"。

use std::sync::Arc;

use async_trait::async_trait;
use blockcell_core::types::ChatMessage;
use blockcell_providers::ProviderPool;

use super::summary::{generate_compact_summary, CompactError};

/// 一次压缩请求。
pub struct CompactionRequest<'a> {
    pub messages: &'a [ChatMessage],
    pub provider_pool: &'a Arc<ProviderPool>,
    pub model: &'a str,
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactionSummary {
    pub summary_markdown: String,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

#[async_trait]
pub trait ContextCompactor: Send + Sync {
    /// 记录在 compaction 事件中的策略名。
    fn name(&self) -> &'static str;

    /// 是否调用模型生成摘要（决定事件中是否记录 model）。
    fn uses_model(&self) -> bool;

    async fn summarize(
        &self,
        request: CompactionRequest<'_>,
    ) -> Result<CompactionSummary, CompactError>;
}

/// 由模型生成 9 段结构化摘要（默认策略）。
#[derive(Debug, Default, Clone, Copy)]
pub struct ModelSummaryCompactor;

#[async_trait]
impl ContextCompactor for ModelSummaryCompactor {
    fn name(&self) -> &'static str {
        "model_summary"
    }

    fn uses_model(&self) -> bool {
        true
    }

    async fn summarize(
        &self,
        request: CompactionRequest<'_>,
    ) -> Result<CompactionSummary, CompactError> {
        let system_prompt = Arc::new(
            "你是一个对话摘要助手。请根据对话历史生成结构化摘要，保留关键信息用于后续继续工作。"
                .to_string(),
        );
        let result = generate_compact_summary(
            Arc::clone(request.provider_pool),
            system_prompt,
            request.model,
            request.messages.to_vec(),
            request.max_output_tokens,
        )
        .await?;
        Ok(CompactionSummary {
            summary_markdown: result.summary.to_markdown(),
            cache_read_tokens: result.cache_read_tokens,
            cache_creation_tokens: result.cache_creation_tokens,
        })
    }
}

/// 不调用模型：抽取用户问题与最终回答。
#[derive(Debug, Default, Clone, Copy)]
pub struct ExtractiveCompactor;

#[async_trait]
impl ContextCompactor for ExtractiveCompactor {
    fn name(&self) -> &'static str {
        "extractive"
    }

    fn uses_model(&self) -> bool {
        false
    }

    async fn summarize(
        &self,
        request: CompactionRequest<'_>,
    ) -> Result<CompactionSummary, CompactError> {
        let summary = crate::runtime::AgentRuntime::build_extractive_summary(request.messages);
        if summary.trim().is_empty() {
            return Err(CompactError::NoSummary);
        }
        Ok(CompactionSummary {
            summary_markdown: summary,
            ..Default::default()
        })
    }
}
