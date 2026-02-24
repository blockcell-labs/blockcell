use blockcell_core::Config;
use blockcell_providers::{AnthropicProvider, GeminiProvider, OllamaProvider, OpenAIProvider, Provider};

/// 为自进化创建独立的 LLM provider
/// 如果配置了 evolution_model，使用独立模型；否则使用主模型
pub fn create_evolution_provider(config: &Config) -> anyhow::Result<Box<dyn Provider>> {
    let model = config.agents.defaults.evolution_model.as_ref()
        .unwrap_or(&config.agents.defaults.model);
    let explicit_provider = config.agents.defaults.evolution_provider.as_deref()
        .or(config.agents.defaults.provider.as_deref());
    create_provider_with_model(config, model, explicit_provider)
}

pub fn create_provider(config: &Config) -> anyhow::Result<Box<dyn Provider>> {
    let model = &config.agents.defaults.model;
    let explicit_provider = config.agents.defaults.provider.as_deref();
    create_provider_with_model(config, model, explicit_provider)
}

/// 解析优先级：
/// 1. explicit_provider 参数（显式指定）
/// 2. model 字符串前缀（如 "anthropic/claude-..."）
/// 3. config.get_api_key() 返回的默认 provider
fn create_provider_with_model(
    config: &Config,
    model: &str,
    explicit_provider: Option<&str>,
) -> anyhow::Result<Box<dyn Provider>> {
    let max_tokens = config.agents.defaults.max_tokens;
    let temperature = config.agents.defaults.temperature;

    // Determine provider from model prefix or configured provider name
    let (provider_name, provider_config) = config
        .get_api_key()
        .ok_or_else(|| anyhow::anyhow!("No provider configured with API key"))?;

    // 优先级1: 显式指定的 provider
    let effective_provider = if let Some(explicit) = explicit_provider {
        explicit
    // 优先级2: model 前缀推断
    } else if model.starts_with("anthropic/") || model.starts_with("claude-") {
        "anthropic"
    } else if model.starts_with("gemini/") || model.starts_with("gemini-") {
        "gemini"
    } else if model.starts_with("ollama/") {
        "ollama"
    } else if model.starts_with("kimi") || model.starts_with("moonshot") {
        "kimi"
    // 优先级3: 配置文件中的默认 provider
    } else {
        provider_name
    };

    // 解析 provider 配置：
    // - 如果 effective_provider 与 get_api_key() 返回的 provider 不同，
    //   尝试获取对应 provider 的配置
    // - 对于显式指定的 provider，如果找不到配置则报错（避免用错 API key）
    // - 对于前缀推断的 provider，找不到配置时回退到默认 provider
    let resolved_config = if effective_provider != provider_name {
        match config.get_provider(effective_provider) {
            Some(cfg) => cfg,
            None if explicit_provider.is_some() => {
                return Err(anyhow::anyhow!(
                    "Provider '{}' is explicitly configured but has no API key in providers section",
                    effective_provider
                ));
            }
            None => provider_config,
        }
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
            )) as Box<dyn Provider>)
        }
        "gemini" => {
            let api_base = resolved_config.api_base.as_deref();
            Ok(Box::new(GeminiProvider::new(
                &resolved_config.api_key,
                api_base,
                model,
                max_tokens,
                temperature,
            )) as Box<dyn Provider>)
        }
        "ollama" => {
            let api_base = resolved_config.api_base.as_deref()
                .or(Some("http://localhost:11434"));
            Ok(Box::new(OllamaProvider::new(
                api_base,
                model,
                max_tokens,
                temperature,
            )) as Box<dyn Provider>)
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
            )) as Box<dyn Provider>)
        }
    }
}
