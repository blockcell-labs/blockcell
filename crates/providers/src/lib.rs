pub mod anthropic;
pub mod client;
pub mod factory;
pub mod gemini;
pub mod ollama;
pub mod openai;
pub mod pool;

use async_trait::async_trait;
use blockcell_core::types::{ChatMessage, LLMResponse};
use blockcell_core::Result;
use serde_json::Value;

#[async_trait]
pub trait Provider: Send + Sync {
    async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<LLMResponse>;
}

pub use anthropic::AnthropicProvider;
pub use factory::{
    create_evolution_provider, create_main_provider, create_provider, infer_provider_from_model,
};
pub use gemini::GeminiProvider;
pub use ollama::OllamaProvider;
pub use openai::OpenAIProvider;
pub use pool::{CallResult, PoolEntryStatus, ProviderPool};
