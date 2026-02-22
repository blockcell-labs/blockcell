use blockcell_core::Config;
use blockcell_providers::{AnthropicProvider, GeminiProvider, OllamaProvider, OpenAIProvider, Provider};

pub fn create_provider(config: &Config) -> anyhow::Result<Box<dyn Provider>> {
    let model = &config.agents.defaults.model;
    let max_tokens = config.agents.defaults.max_tokens;
    let temperature = config.agents.defaults.temperature;

    // Determine provider from model prefix or configured provider name
    let (provider_name, provider_config) = config
        .get_api_key()
        .ok_or_else(|| anyhow::anyhow!("No provider configured with API key"))?;

    // Check if the model name has a provider prefix (e.g. "anthropic/claude-...")
    let effective_provider = if model.starts_with("anthropic/") || model.starts_with("claude-") {
        "anthropic"
    } else if model.starts_with("gemini/") || model.starts_with("gemini-") {
        "gemini"
    } else if model.starts_with("ollama/") {
        "ollama"
    } else if model.starts_with("kimi") || model.starts_with("moonshot") {
        "kimi"
    } else {
        provider_name
    };

    // For providers with a prefix in the model, try to get that provider's config;
    // fall back to the auto-detected provider_config if not found.
    let resolved_config = if effective_provider != provider_name {
        config.get_provider(effective_provider).unwrap_or(provider_config)
    } else {
        provider_config
    };

    match effective_provider {
        "anthropic" => {
            let api_base = resolved_config.api_base.as_deref();
            Ok(Box::new(AnthropicProvider::new(
                &resolved_config.api_key,
                api_base,
                model,
                max_tokens,
                temperature,
            )))
        }
        "gemini" => {
            let api_base = resolved_config.api_base.as_deref();
            Ok(Box::new(GeminiProvider::new(
                &resolved_config.api_key,
                api_base,
                model,
                max_tokens,
                temperature,
            )))
        }
        "ollama" => {
            let api_base = resolved_config.api_base.as_deref()
                .or(Some("http://localhost:11434"));
            Ok(Box::new(OllamaProvider::new(
                api_base,
                model,
                max_tokens,
                temperature,
            )))
        }
        _ => {
            // OpenAI-compatible: openrouter, openai, deepseek, groq, zhipu, vllm, etc.
            let api_base = resolved_config.api_base.as_deref().unwrap_or({
                match effective_provider {
                    "openrouter" => "https://openrouter.ai/api/v1",
                    "openai" => "https://api.openai.com/v1",
                    "deepseek" => "https://api.deepseek.com/v1",
                    "groq" => "https://api.groq.com/openai/v1",
                    "zhipu" => "https://open.bigmodel.cn/api/paas/v4",
                    "kimi" | "moonshot" => "https://api.moonshot.cn/v1",
                    _ => "https://api.openai.com/v1",
                }
            });
            Ok(Box::new(OpenAIProvider::new(
                &resolved_config.api_key,
                Some(api_base),
                model,
                max_tokens,
                temperature,
            )))
        }
    }
}
