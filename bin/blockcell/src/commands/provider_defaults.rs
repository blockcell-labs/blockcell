/// 各 provider 的推荐默认模型。
///
/// 维护说明（2026-10 校正）：模型名与接口地址变动很快，建议每季度复核一次。
/// 复核来源：
/// - OpenAI  https://developers.openai.com/api/docs/models
/// - Anthropic https://platform.claude.com/docs/en/about-claude/models/overview
/// - Gemini  https://ai.google.dev/gemini-api/docs/models
/// - DeepSeek https://api-docs.deepseek.com/zh-cn/quick_start/pricing
/// - Kimi    https://platform.moonshot.cn/docs
/// - Zhipu/Z.AI https://docs.bigmodel.cn
/// - xAI     https://docs.x.ai/developers/models
/// - Mistral https://docs.mistral.ai/getting-started/models/models_overview/
/// - MiniMax https://platform.minimaxi.com
/// - Qwen    https://help.aliyun.com/zh/model-studio/models
pub(crate) fn default_model_for_provider(provider: &str) -> &'static str {
    match provider.trim().to_lowercase().as_str() {
        // 默认 deepseek-flash（V4.1-Flash-1017，输入 1-2 元 / 输出 4-8 元每百万 tokens，空闲时段再半价）；
        // 需要更强推理时再切 deepseek-v4-pro（V4-Pro-0813，价格约为 Flash 的 4 倍）
        "deepseek" => "deepseek-flash",
        // gpt-6.1-sol：接近旗舰 gpt-6-astra 的能力，价格低一个量级
        "openai" => "gpt-6.1-sol",
        // claude-opus-5-5：Anthropic 官方推荐的大多数场景起点（Fable 5.1 为最强旗舰，价格翻倍）
        "anthropic" | "claude" => "claude-opus-5-5",
        "kimi" | "moonshot" => "kimi-k3.1-pro",
        "gemini" => "gemini-4-argon",
        "zhipu" | "glm" => "glm-5.3",
        "xai" | "grok" => "grok-4.7",
        // mistral-large-4 仍是 Public Preview，默认用稳定的 medium-3.5
        "mistral" => "mistral-medium-2604",
        "minimax" => "MiniMax-M3.1-Flash",
        "qwen" => "qwen3.8-max",
        "groq" => "openai/gpt-oss-120b",
        "siliconflow" => "deepseek-ai/DeepSeek-V4.1-Flash",
        "openrouter" => "openai/gpt-6.1-sol",
        "ollama" => "qwen3.8",
        _ => "gpt-6.1-sol",
    }
}
