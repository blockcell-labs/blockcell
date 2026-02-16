pub mod openai;
pub mod anthropic;
pub mod ollama;
pub mod gemini;

use async_trait::async_trait;
use blockcell_core::types::{ChatMessage, LLMResponse};
use blockcell_core::Result;
use serde_json::Value;

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<LLMResponse>;
}

pub use openai::OpenAIProvider;
pub use anthropic::AnthropicProvider;
pub use ollama::OllamaProvider;
pub use gemini::GeminiProvider;
