# Provider 配置方案3 - 向后兼容性测试

## 测试场景

### 场景1：旧格式（model 前缀）
```json
{
  "agents": {
    "defaults": {
      "model": "anthropic/claude-sonnet-4-20250514"
    }
  }
}
```
**预期行为**：
- 从 model 前缀推断 provider = "anthropic"
- 使用 AnthropicProvider
- model 传递给 provider 时保留前缀

### 场景2：新格式（显式 provider）
```json
{
  "agents": {
    "defaults": {
      "provider": "anthropic",
      "model": "claude-sonnet-4-20250514"
    }
  }
}
```
**预期行为**：
- 使用显式 provider = "anthropic"
- 使用 AnthropicProvider
- model 不需要前缀

### 场景3：混合格式（显式优先）
```json
{
  "agents": {
    "defaults": {
      "provider": "openai",
      "model": "anthropic/claude-sonnet-4-20250514"
    }
  }
}
```
**预期行为**：
- 显式 provider = "openai" 优先
- 使用 OpenAIProvider（而不是 Anthropic）
- 允许通过 OpenAI 兼容接口调用其他模型

### 场景4：自进化独立配置（新格式）
```json
{
  "agents": {
    "defaults": {
      "provider": "anthropic",
      "model": "claude-sonnet-4-20250514",
      "evolutionProvider": "openai",
      "evolutionModel": "gpt-4o-mini"
    }
  }
}
```
**预期行为**：
- 对话使用 AnthropicProvider + claude-sonnet-4
- 自进化使用 OpenAIProvider + gpt-4o-mini
- 两者完全独立

### 场景5：自进化独立配置（旧格式）
```json
{
  "agents": {
    "defaults": {
      "model": "anthropic/claude-sonnet-4-20250514",
      "evolutionModel": "openai/gpt-4o-mini"
    }
  }
}
```
**预期行为**：
- 对话从前缀推断 AnthropicProvider
- 自进化从前缀推断 OpenAIProvider
- 向后兼容

### 场景6：部分显式配置
```json
{
  "agents": {
    "defaults": {
      "provider": "anthropic",
      "model": "claude-sonnet-4-20250514",
      "evolutionModel": "gpt-4o-mini"
    }
  }
}
```
**预期行为**：
- 对话使用显式 provider = "anthropic"
- 自进化从 model 前缀推断 provider = "openai"
- 如果推断失败，回退到主 provider = "anthropic"

### 场景7：Ollama 本地模型
```json
{
  "agents": {
    "defaults": {
      "provider": "ollama",
      "model": "llama3",
      "evolutionModel": "qwen2.5-coder"
    }
  }
}
```
**预期行为**：
- 对话和自进化都使用 OllamaProvider
- 不需要 API key
- 使用本地 http://localhost:11434

## 解析优先级验证

| 配置项 | 优先级1（最高） | 优先级2 | 优先级3（最低） |
|--------|----------------|---------|----------------|
| 主 provider | `provider` 字段 | model 前缀 | config.get_api_key() |
| 自进化 provider | `evolutionProvider` 字段 | evolution_model 前缀 | `provider` 字段 |

## 编译验证

✅ `cargo build --release` 成功
✅ 所有现有配置格式保持兼容
✅ 新配置格式提供更好的可读性

## 迁移建议

**对于新用户**：
- 推荐使用新格式（显式 provider 字段）
- 更清晰、更易维护

**对于现有用户**：
- 旧配置继续工作，无需修改
- 可以渐进式迁移到新格式
- 混合使用也支持
