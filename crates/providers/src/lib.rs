pub mod openai;
pub mod anthropic;
pub mod ollama;
pub mod gemini;
pub mod factory;

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
pub use factory::{create_provider, create_main_provider, create_evolution_provider, infer_provider_from_model};
