pub mod anthropic;
pub mod client;
pub mod factory;
pub mod gemini;
pub mod ollama;
pub mod openai;
pub mod openai_responses;
pub mod pool;

use async_trait::async_trait;
use blockcell_core::types::{ChatMessage, LLMResponse, StreamChunk};
use blockcell_core::Result;
use serde_json::Value;
use tokio::sync::mpsc;

#[async_trait]
pub trait Provider: Send + Sync {
    /// 非流式调用
    async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<LLMResponse>;

    /// 流式调用（默认实现：将非流式转换为流式）
    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
    ) -> Result<mpsc::Receiver<StreamChunk>> {
        // 默认实现：调用非流式方法并转换为流式
        let response = self.chat(messages, tools).await?;
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            // 发送文本内容
            if let Some(content) = &response.content {
                if !content.is_empty() {
                    let _ = tx.send(StreamChunk::TextDelta { delta: content.clone() }).await;
                }
            }
            // 发送推理内容
            if let Some(reasoning) = &response.reasoning_content {
                if !reasoning.is_empty() {
                    let _ = tx.send(StreamChunk::ReasoningDelta { delta: reasoning.clone() }).await;
                }
            }
            // 发送完成事件
            let _ = tx.send(StreamChunk::Done { response }).await;
        });
        Ok(rx)
    }
}

pub use anthropic::AnthropicProvider;
pub use factory::{
    create_evolution_provider, create_main_provider, create_provider, infer_provider_from_model,
};
pub use gemini::GeminiProvider;
pub use ollama::OllamaProvider;
pub use openai::OpenAIProvider;
pub use openai_responses::OpenAIResponsesProvider;
pub use pool::{CallResult, PoolEntryStatus, ProviderPool};
