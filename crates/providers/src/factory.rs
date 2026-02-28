use blockcell_core::Config;

use crate::{AnthropicProvider, GeminiProvider, OllamaProvider, OpenAIProvider, Provider};

/// 默认的 OpenAI 兼容 provider 的 api_base
fn default_api_base(provider_name: &str) -> &'static str {
    match provider_name {
        "openrouter" => "https://openrouter.ai/api/v1",
        "openai" => "https://api.openai.com/v1",
        "deepseek" => "https://api.deepseek.com/v1",
        "groq" => "https://api.groq.com/openai/v1",
        "zhipu" => "https://open.bigmodel.cn/api/paas/v4",
        "kimi" | "moonshot" => "https://api.moonshot.cn/v1",
        _ => "https://api.openai.com/v1",
    }
}

/// 从 model 字符串前缀推断 provider 名字
/// 返回 None 表示无法从前缀推断（需要 fallback）
pub fn infer_provider_from_model(model: &str) -> Option<&'static str> {
    if model.starts_with("anthropic/") || model.starts_with("claude-") {
        Some("anthropic")
    } else if model.starts_with("gemini/") || model.starts_with("gemini-") {
        Some("gemini")
    } else if model.starts_with("ollama/") {
        Some("ollama")
    } else if model.starts_with("kimi") || model.starts_with("moonshot") {
        Some("kimi")
    } else if model.starts_with("openai/") || model.starts_with("gpt-") || model.starts_with("o1") || model.starts_with("o3") {
        Some("openai")
    } else if model.starts_with("deepseek") {
        Some("deepseek")
    } else if model.starts_with("groq/") {
        Some("groq")
    } else {
        None
    }
}

/// 从 config 中找到第一个配置了 api_key 的 provider 名字作为 fallback
/// 这比旧的 get_api_key() 更合理：先按 model 前缀确认优先 provider
fn fallback_provider_name(config: &Config) -> Option<&str> {
    let priority = [
        "anthropic", "openai", "openrouter", "deepseek", "kimi",
        "gemini", "zhipu", "groq", "vllm", "ollama",
    ];
    for name in priority {
        if let Some(p) = config.providers.get(name) {
            if !p.api_key.is_empty() && p.api_key != "dummy" {
                return Some(name);
            }
        }
    }
    // ollama 特殊处理：不需要真实 api_key
    if config.providers.contains_key("ollama") {
        return Some("ollama");
    }
    None
}

/// 统一的 provider 创建入口。
///
/// 解析优先级：
/// 1. `explicit_provider` 参数（来自 config.agents.defaults.provider 或 evolution_provider）
/// 2. model 字符串前缀推断（如 "anthropic/claude-..." → anthropic）
/// 3. config 中第一个有效 api_key 的 provider（fallback）
///
/// 对于 explicit_provider：
/// - 如果 config.providers 中找不到对应配置，返回 Err
/// - ollama 不需要 api_key，不报错
pub fn create_provider(
    config: &Config,
    model: &str,
    explicit_provider: Option<&str>,
) -> anyhow::Result<Box<dyn Provider>> {
    let max_tokens = config.agents.defaults.max_tokens;
    let temperature = config.agents.defaults.temperature;

    // 优先级1：显式指定
    // 优先级2：model 前缀推断
    // 优先级3：config fallback
    let effective_provider: &str = if let Some(ep) = explicit_provider {
        ep
    } else if let Some(inferred) = infer_provider_from_model(model) {
        inferred
    } else if let Some(fallback) = fallback_provider_name(config) {
        fallback
    } else {
        return Err(anyhow::anyhow!(
            "No LLM provider configured. Set 'provider' in config, use a recognized model prefix \
             (e.g. 'anthropic/claude-...', 'gpt-4o', 'gemini-...'), or add an API key to providers section."
        ));
    };

    // 获取 provider 配置
    let provider_cfg = config.providers.get(effective_provider);

    // 对于显式指定的 provider（非 ollama），必须有配置且有 api_key
    if explicit_provider.is_some() && effective_provider != "ollama" {
        match provider_cfg {
            None => {
                return Err(anyhow::anyhow!(
                    "Provider '{}' is explicitly configured but not found in providers section",
                    effective_provider
                ));
            }
            Some(cfg) if cfg.api_key.is_empty() || cfg.api_key == "dummy" => {
                return Err(anyhow::anyhow!(
                    "Provider '{}' is explicitly configured but has no API key",
                    effective_provider
                ));
            }
            _ => {}
        }
    }

    let empty_cfg = blockcell_core::config::ProviderConfig::default();
    let resolved_cfg = provider_cfg.unwrap_or(&empty_cfg);

    // 计算代理参数：
    //   provider_proxy = providers.<name>.proxy（None=未配置，Some("")=强制直连，Some(url)=该provider专用）
    //   global_proxy   = network.proxy
    //   no_proxy       = network.no_proxy
    let provider_proxy = resolved_cfg.proxy.as_deref();
    let global_proxy = config.network.proxy.as_deref();
    let no_proxy = &config.network.no_proxy;

    match effective_provider {
        "anthropic" => {
            Ok(Box::new(AnthropicProvider::new_with_proxy(
                &resolved_cfg.api_key,
                resolved_cfg.api_base.as_deref(),
                model,
                max_tokens,
                temperature,
                provider_proxy,
                global_proxy,
                no_proxy,
            )) as Box<dyn Provider>)
        }
        "gemini" => {
            Ok(Box::new(GeminiProvider::new_with_proxy(
                &resolved_cfg.api_key,
                resolved_cfg.api_base.as_deref(),
                model,
                max_tokens,
                temperature,
                provider_proxy,
                global_proxy,
                no_proxy,
            )) as Box<dyn Provider>)
        }
        "ollama" => {
            let api_base = resolved_cfg.api_base.as_deref()
                .or(Some("http://localhost:11434"));
            Ok(Box::new(OllamaProvider::new_with_proxy(
                api_base,
                model,
                max_tokens,
                temperature,
                provider_proxy,
                global_proxy,
                no_proxy,
            )) as Box<dyn Provider>)
        }
        _ => {
            // OpenAI 兼容：openrouter, openai, deepseek, groq, zhipu, vllm, kimi, moonshot 等
            let api_base = resolved_cfg.api_base.as_deref()
                .unwrap_or_else(|| default_api_base(effective_provider));
            Ok(Box::new(OpenAIProvider::new_with_proxy(
                &resolved_cfg.api_key,
                Some(api_base),
                model,
                max_tokens,
                temperature,
                provider_proxy,
                global_proxy,
                no_proxy,
            )) as Box<dyn Provider>)
        }
    }
}

/// 为主对话创建 provider
pub fn create_main_provider(config: &Config) -> anyhow::Result<Box<dyn Provider>> {
    let model = &config.agents.defaults.model;
    let explicit_provider = config.agents.defaults.provider.as_deref();
    create_provider(config, model, explicit_provider)
}

/// 为自进化创建独立的 provider
/// 优先级：evolutionProvider > evolution_model 前缀 > provider > model 前缀 > fallback
pub fn create_evolution_provider(config: &Config) -> anyhow::Result<Box<dyn Provider>> {
    let model = config.agents.defaults.evolution_model.as_deref()
        .unwrap_or(&config.agents.defaults.model);

    // evolution_provider 显式 > 主 provider 显式（作为 evolution fallback）
    let explicit_provider = config.agents.defaults.evolution_provider.as_deref()
        .or(config.agents.defaults.provider.as_deref());

    create_provider(config, model, explicit_provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_infer_provider_from_model() {
        assert_eq!(infer_provider_from_model("anthropic/claude-sonnet-4"), Some("anthropic"));
        assert_eq!(infer_provider_from_model("claude-3-5-sonnet"), Some("anthropic"));
        assert_eq!(infer_provider_from_model("gemini-2.0-flash"), Some("gemini"));
        assert_eq!(infer_provider_from_model("gemini/gemini-pro"), Some("gemini"));
        assert_eq!(infer_provider_from_model("ollama/llama3"), Some("ollama"));
        assert_eq!(infer_provider_from_model("kimi-moonshot-v1"), Some("kimi"));
        assert_eq!(infer_provider_from_model("gpt-4o"), Some("openai"));
        assert_eq!(infer_provider_from_model("deepseek-coder"), Some("deepseek"));
        assert_eq!(infer_provider_from_model("some-unknown-model"), None);
    }

    #[test]
    fn test_create_provider_explicit_wins() {
        let mut config = Config::default();
        // 配置 openai provider
        config.providers.get_mut("openai").unwrap().api_key = "sk-test".to_string();
        // model 是 anthropic 前缀，但显式指定 openai
        let result = create_provider(&config, "anthropic/claude-sonnet-4", Some("openai"));
        assert!(result.is_ok(), "显式 provider 应该覆盖 model 前缀");
    }

    #[test]
    fn test_create_provider_model_prefix() {
        let mut config = Config::default();
        config.providers.get_mut("anthropic").unwrap().api_key = "sk-ant-test".to_string();
        let result = create_provider(&config, "claude-3-5-sonnet", None);
        assert!(result.is_ok(), "model 前缀应该推断 anthropic");
    }

    #[test]
    fn test_create_provider_ollama_no_key_needed() {
        let config = Config::default();
        // ollama 不需要 api_key
        let result = create_provider(&config, "llama3", Some("ollama"));
        assert!(result.is_ok(), "ollama 不需要 api_key");
    }

    #[test]
    fn test_create_provider_no_config_fails() {
        let mut config = Config::default();
        // 清空所有 api_key，并移除 ollama（ollama 不需要 api_key 会被作为 fallback）
        for p in config.providers.values_mut() {
            p.api_key = String::new();
        }
        config.providers.remove("ollama");
        let result = create_provider(&config, "some-unknown-model", None);
        assert!(result.is_err(), "没有任何配置时应报错");
    }

    #[test]
    fn test_create_provider_explicit_missing_fails() {
        let config = Config::default(); // anthropic key 为空
        let result = create_provider(&config, "gpt-4o", Some("anthropic"));
        assert!(result.is_err(), "显式指定但无 api_key 时应报错");
    }
}
