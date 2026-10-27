# 代理与 LLM Provider 配置

本文档说明如何配置 LLM provider（模型服务商）、API key，以及网络代理设置。

## 配置文件位置

```
~/.blockcell/config.json5
```

首次运行 `blockcell onboard` 会自动生成此文件。也可以用 `blockcell config edit` 直接编辑。

WebUI 的完整配置编辑器也会直接读取/保存原始 `config.json5` 文本；保存时由后端校验 JSON5，因此注释和排版可以保留在原始编辑路径中。

---

## 一、Provider 配置

### 配置结构

配置文件的 `providers` 字段是一个 map，key 是 provider 名称，value 包含三个字段：

```json
{
  "providers": {
    "<provider-name>": {
      "apiKey": "your-api-key",
      "apiBase": "https://api.example.com/v1",
      "proxy": "http://127.0.0.1:7890"
    }
  }
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `apiKey` | string | API 密钥 |
| `apiBase` | string（可选） | 自定义 API 地址（覆盖内置默认值） |
| `proxy` | string（可选） | 该 provider 专用代理，优先级高于全局 `network.proxy`。设为 `""` 可强制该 provider 直连（跳过全局代理） |

### 内置 Provider 列表

以下 provider 在默认配置中已预置，只需填入 `apiKey` 即可使用；其中一部分是 OpenAI 兼容接口，一部分使用专用 provider 实现：

| Provider 名称 | 默认 apiBase | 说明 |
|---------------|-------------|------|
| `anthropic` | `https://api.anthropic.com/v1` | Anthropic Claude 系列 |
| `openai` | `https://api.openai.com/v1` | OpenAI GPT/o1/o3 系列 |
| `deepseek` | `https://api.deepseek.com/v1` | DeepSeek 系列 |
| `openrouter` | `https://openrouter.ai/api/v1` | OpenRouter 聚合 |
| `groq` | `https://api.groq.com/openai/v1` | Groq 加速推理 |
| `zhipu` | `https://open.bigmodel.cn/api/paas/v4` | 智谱 ChatGLM 系列 |
| `vllm` | `http://localhost:8000/v1` | 本地 vLLM 服务 |
| `gemini` | `https://generativelanguage.googleapis.com/v1beta/openai` | Google Gemini 系列（OpenAI-compatible 配置） |
| `kimi` | `https://api.moonshot.cn/v1` | Kimi / Moonshot 系列 |
| `xai` | `https://api.x.ai/v1` | xAI / Grok 系列 |
| `mistral` | `https://api.mistral.ai/v1` | Mistral 系列 |
| `minimax` | `https://api.minimaxi.com/v1` | MiniMax 系列 |
| `qwen` | `https://dashscope.aliyuncs.com/compatible-mode/v1` | Qwen 系列 |
| `glm` | `https://api.z.ai/v1` | GLM 系列 |
| `siliconflow` | `https://api.siliconflow.cn/v1` | SiliconFlow 聚合 |
| `ollama` | `http://localhost:11434` | 本地 Ollama 服务（不需要 apiKey） |

### 常用配置示例

**DeepSeek：**
```json
{
  "providers": {
    "deepseek": {
      "apiKey": "sk-xxxxxxxx"
    }
  },
  "agents": {
    "defaults": {
      "model": "deepseek-flash",
      "provider": "deepseek",
      "maxContextTokens": 1048576,
      "reasoningEffort": "high"
    }
  }
}
```

**Anthropic Claude：**
```json
{
  "providers": {
    "anthropic": {
      "apiKey": "sk-ant-xxxxxxxx"
    }
  },
  "agents": {
    "defaults": {
      "model": "claude-sonnet-5-5"
    }
  }
}
```

**Kimi（Moonshot）：**
```json
{
  "providers": {
    "kimi": {
      "apiKey": "sk-xxxxxxxx"
    }
  },
  "agents": {
    "defaults": {
      "model": "kimi-k3.1-pro"
    }
  }
}
```

**OpenRouter（访问多家模型）：**
```json
{
  "providers": {
    "openrouter": {
      "apiKey": "sk-or-xxxxxxxx"
    }
  },
  "agents": {
    "defaults": {
      "model": "anthropic/claude-sonnet-5-5"
    }
  }
}
```

**本地 Ollama（无需 apiKey）：**
```json
{
  "providers": {
    "ollama": {
      "apiBase": "http://localhost:11434"
    }
  },
  "agents": {
    "defaults": {
      "model": "ollama/qwen3.8"
    }
  }
}
```

**自托管 vLLM / OpenAI 兼容服务：**
```json
{
  "providers": {
    "vllm": {
      "apiKey": "dummy",
      "apiBase": "http://192.168.1.100:8000/v1"
    }
  },
  "agents": {
    "defaults": {
      "model": "Qwen/Qwen3.6-35B-A3B",
      "provider": "vllm"
    }
  }
}
```

---

## 二、Agent 模型配置

`agents.defaults` 控制 Agent 使用的模型和行为参数：

```json
{
  "agents": {
    "defaults": {
      "model": "deepseek-flash",
      "provider": null,
      "maxTokens": 8192,
      "temperature": 0.7,
      "maxToolIterations": 30,
      "llmMaxRetries": 3,
      "llmRetryDelayMs": 2000,
      "maxContextTokens": 1048576,
      "reasoningEffort": null,
      "evolutionModel": null,
      "evolutionProvider": null
    }
  }
}
```

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `model` | `deepseek-flash` | 主模型名称；`setup` 会按所选 provider 写入推荐默认模型 |
| `provider` | null | 显式指定 provider（不指定则从 model 前缀推断） |
| `maxTokens` | 8192 | 每次 LLM 调用的最大输出 token 数 |
| `temperature` | 0.7 | 采样温度（0.0 ~ 1.0） |
| `maxToolIterations` | 30 | 单次消息处理的最大工具调用轮数 |
| `llmMaxRetries` | 3 | LLM 调用失败时的最大重试次数 |
| `llmRetryDelayMs` | 2000 | 重试间隔（毫秒） |
| `maxContextTokens` | 1048576 | 上下文窗口大小（影响历史压缩）；DeepSeek/Gemini 等长上下文模型可使用更高值 |
| `reasoningEffort` | null | 推理强度控制；DeepSeek thinking mode 支持 `off`、`low`、`medium`、`high`、`max` |
| `evolutionModel` | null | 技能自进化专用模型（为 null 则使用主模型） |
| `evolutionProvider` | null | 技能自进化专用 provider |

### Provider 选择逻辑

系统按以下优先级选择 provider：

1. **`agents.defaults.provider` 显式指定**（最高优先级）
2. **从 `model` 字段前缀推断**：
   - `anthropic/...` 或 `claude-` 开头 → `anthropic`
   - `gemini/...` 或 `gemini-` 开头 → `gemini`
   - `ollama/...` 开头 → `ollama`
   - `kimi` 或 `moonshot` 开头 → `kimi`
   - `openai/...`、`gpt-`、`o1`、`o3` 开头 → `openai`
   - `deepseek` 开头 → `deepseek`
   - `groq/...` 开头 → `groq`
3. **从 `providers` 中按内置优先顺序找第一个配置了有效 apiKey 的 provider**（fallback）

内置 fallback 顺序目前是：`anthropic` → `openai` → `openrouter` → `deepseek` → `kimi` → `gemini` → `zhipu` → `groq` → `vllm` → `ollama`。

`xai`、`mistral`、`minimax`、`qwen`、`glm`、`siliconflow` 这些 provider 也在默认配置里预置了，但如果你想优先使用它们，通常需要显式指定 `provider` 或在 `model` 上使用能推断出对应 provider 的前缀。

**实际示例：**

```json
// 情形1：model 前缀自动推断（推荐，无需设置 provider）
{ "model": "claude-sonnet-5-5" }
// → 自动推断使用 anthropic provider

// 情形2：使用 openrouter 调用 Claude（model 前缀是 anthropic/，但想走 openrouter）
{ "model": "anthropic/claude-sonnet-5-5", "provider": "openrouter" }

// 情形3：本地 Ollama 模型
{ "model": "ollama/qwen3.8" }
// → 自动推断使用 ollama provider
```

---

## 三、全局网络代理配置

`network` 字段控制全局 HTTP 代理，适用于所有 LLM provider 请求：

```json
{
  "network": {
    "proxy": "http://127.0.0.1:7890",
    "noProxy": ["localhost", "127.0.0.1", "::1", "*.local"]
  }
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `proxy` | string（可选） | 全局代理地址，格式：`http://host:port` 或 `socks5://host:port`。留空则直连 |
| `noProxy` | string[]（可选） | 不走代理的域名/IP 列表，支持前缀通配符 `*.example.com` |

### 代理优先级规则

| 优先级 | 配置 | 说明 |
|--------|------|------|
| 最高 | `providers.<name>.proxy = "http://..."` | 该 provider 使用专用代理 |
| 最高 | `providers.<name>.proxy = ""` | 该 provider **强制直连**，跳过全局代理 |
| 中 | `network.proxy` | 全局代理（所有未单独配置的 provider 都走此代理） |
| 最低 | 不配置 | 直连 |

### 代理配置场景示例

**场景1：全局代理，所有请求都走代理**

```json
{
  "network": {
    "proxy": "http://127.0.0.1:7890"
  }
}
```

**场景2：全局代理，但 Ollama 本地服务直连**

```json
{
  "network": {
    "proxy": "http://127.0.0.1:7890",
    "noProxy": ["localhost", "127.0.0.1"]
  },
  "providers": {
    "ollama": {
      "apiBase": "http://localhost:11434",
      "proxy": ""
    }
  }
}
```

**场景3：全局不代理，仅 Anthropic 走代理（例如需要翻墙）**

```json
{
  "providers": {
    "anthropic": {
      "apiKey": "sk-ant-xxxxxxxx",
      "proxy": "http://127.0.0.1:7890"
    },
    "deepseek": {
      "apiKey": "sk-xxxxxxxx"
    }
  }
}
```

**场景4：SOCKS5 代理**

```json
{
  "network": {
    "proxy": "socks5://127.0.0.1:1080"
  }
}
```

---

## 四、Telegram 渠道代理

Telegram 渠道有独立的代理配置，与 LLM provider 代理分开设置：

```json
{
  "channels": {
    "telegram": {
      "enabled": true,
      "token": "your-bot-token",
      "allowFrom": ["123456789"],
      "proxy": "http://127.0.0.1:7890"
    }
  }
}
```

---

## 五、使用 CLI 快速修改配置

无需手动编辑 JSON5，可用 `config set` 命令修改：

```bash
# 设置全局代理
blockcell config set network.proxy "http://127.0.0.1:7890"

# 设置模型
blockcell config set agents.defaults.model "deepseek-flash"

# 设置 DeepSeek API Key
blockcell config set providers.deepseek.apiKey "sk-xxxxxxxx"

# 为 Anthropic 设置专用代理
blockcell config set providers.anthropic.proxy "http://127.0.0.1:7890"

# 强制 vllm 直连（跳过全局代理）
blockcell config set providers.vllm.proxy ""

# 查看当前代理设置
blockcell config get network.proxy

# 查看所有 provider 配置
blockcell config providers
```

---

## 六、完整配置示例

以下是一个生产环境常用的完整配置示例：

```json
{
  "providers": {
    "anthropic": {
      "apiKey": "sk-ant-xxxxxxxx"
    },
    "deepseek": {
      "apiKey": "sk-xxxxxxxx"
    },
    "openrouter": {
      "apiKey": "sk-or-xxxxxxxx",
      "apiBase": "https://openrouter.ai/api/v1"
    },
    "ollama": {
      "apiBase": "http://localhost:11434",
      "proxy": ""
    }
  },
  "network": {
    "proxy": "http://127.0.0.1:7890",
    "noProxy": ["localhost", "127.0.0.1", "::1"]
  },
  "agents": {
    "defaults": {
      "model": "claude-sonnet-5-5",
      "maxTokens": 8192,
      "temperature": 0.7,
      "maxToolIterations": 20,
      "evolutionModel": "deepseek-flash",
      "evolutionProvider": "deepseek"
    }
  },
  "gateway": {
    "host": "0.0.0.0",
    "port": 18790,
    "apiToken": "your-secret-token"
  }
}
```

> **说明：** 此配置使用 Claude 作为主模型（走全局代理），DeepSeek 专门用于技能自进化（更便宜），本地 Ollama 强制直连。
