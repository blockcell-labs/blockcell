# Article 18: Proxy and Provider Configuration

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — Article 18

This article explains how blockcell currently handles LLM providers, model selection, and proxy settings.

The code-backed source of truth lives mainly in:

- `crates/core/src/config.rs`
- `crates/providers/src/factory.rs`
- `bin/blockcell/src/commands/config_cmd.rs`
- `bin/blockcell/src/commands/gateway/config_api.rs`
- `webui/src/components/config/config-page.tsx`

## Config file locations

blockcell currently uses these config files under `~/.blockcell/`:

- `config.json5` — main runtime config
- `mcp.json` — root MCP config
- `mcp.d/*.json` — one file per MCP server override / definition

Important separation:

- Provider, agent, channel, gateway, memory, and intent router settings live in `config.json5`
- MCP servers do **not** live in `config.json5`; they are configured separately in `mcp.json` and `mcp.d/*.json`
- `config.json5` supports JSON5 features such as comments and trailing commas
- MCP files are currently strict JSON

## Raw config editing in WebUI

The current WebUI full-config editor reads and writes the **raw `config.json5` text**:

- `GET /v1/config/raw` returns the raw file content
- `PUT /v1/config/raw` validates JSON5 and writes the text back as-is
- `POST /v1/config/reload` validates the file from disk, but some settings still require a gateway restart to fully apply

That means the current full editor path is not a structured JSON reformatter. It is a raw JSON5 editor, which is the right place to preserve comments or deliberate formatting.

---

## 1. Provider configuration

Provider definitions live under `providers` in `config.json5`.

Example:

```json5
{
  providers: {
    deepseek: {
      apiKey: "sk-xxxxxxxx",
    },
    openai: {
      apiKey: "sk-xxxxxxxx",
    },
    anthropic: {
      apiKey: "sk-ant-xxxxxxxx",
      proxy: "http://127.0.0.1:7890",
    },
    openrouter: {
      apiKey: "sk-or-xxxxxxxx",
      apiBase: "https://openrouter.ai/api/v1",
    },
    ollama: {
      apiBase: "http://localhost:11434",
      proxy: "",
    },
  },
}
```

### Provider fields

`ProviderConfig` currently supports these main fields:

| Field | Type | Description |
|------|------|------|
| `apiKey` | string | Provider API key |
| `apiBase` | string \/ null | Base URL override |
| `proxy` | string \/ null | Provider-specific proxy override |
| `apiType` | string | Compatibility marker for request formatting / frontend display |

### Built-in provider defaults

The default config currently pre-populates common providers such as:

- `openrouter`
- `anthropic`
- `openai`
- `deepseek`
- `groq`
- `zhipu`
- `vllm`
- `gemini`
- `kimi`
- `xai`
- `mistral`
- `minimax`
- `qwen`
- `glm`
- `siliconflow`
- `ollama`

Some of these also ship with a default `apiBase`.

### Provider selection logic

At runtime, blockcell selects the provider in this order:

1. `agents.defaults.provider` or the resolved agent's explicit `provider`
2. infer from the `model` prefix
3. fall back to the first configured provider with a usable API key

Current inference rules include:

- `anthropic/...` or `claude-...` → `anthropic`
- `gemini/...` or `gemini-...` → `gemini`
- `ollama/...` → `ollama`
- `kimi...` or `moonshot...` → `kimi`
- `openai/...`, `gpt-...`, `o1...`, `o3...` → `openai`
- `deepseek...` → `deepseek`
- `groq/...` → `groq`

### Real examples

```json5
// Let blockcell infer Anthropic from the model name
{ model: "claude-sonnet-5-5" }

// Use an anthropic-prefixed model string, but force routing through OpenRouter
{ model: "anthropic/claude-sonnet-5-5", provider: "openrouter" }

// Local Ollama model
{ model: "ollama/qwen3.8" }
```

---

## 2. Agent model configuration

The main per-agent defaults live under `agents.defaults`.

Example:

```json5
{
  agents: {
    defaults: {
      model: "deepseek-flash",
      provider: null,
      maxTokens: 8192,
      temperature: 0.7,
      maxToolIterations: 30,
      llmMaxRetries: 3,
      llmRetryDelayMs: 2000,
      maxContextTokens: 1048576,
      reasoningEffort: null,
      evolutionModel: null,
      evolutionProvider: null,
    },
  },
}
```

| Field | Default | Description |
|------|--------|------|
| `model` | `deepseek-flash` | Primary model; `setup` writes a provider-specific recommended default |
| `provider` | `null` | Explicit provider override |
| `maxTokens` | `8192` | Max output tokens per LLM call |
| `temperature` | `0.7` | Sampling temperature |
| `maxToolIterations` | `30` | Max tool-call loops per message |
| `llmMaxRetries` | `3` | Max retry count for failed LLM calls |
| `llmRetryDelayMs` | `2000` | Retry delay in milliseconds |
| `maxContextTokens` | `1048576` | Context window used for history management; long-context providers such as DeepSeek/Gemini can use higher values |
| `reasoningEffort` | `null` | Reasoning control; DeepSeek thinking mode supports `off`, `low`, `medium`, `high`, and `max` |
| `evolutionModel` | `null` | Dedicated model for self-evolution |
| `evolutionProvider` | `null` | Dedicated provider for self-evolution |

If `evolutionModel` and `evolutionProvider` are not set, evolution work reuses the main agent model selection.

---

## 3. Global network proxy configuration

Global HTTP proxy settings live under `network`:

```json5
{
  network: {
    proxy: "http://127.0.0.1:7890",
    noProxy: ["localhost", "127.0.0.1", "::1", "*.local"],
  },
}
```

| Field | Type | Description |
|------|------|------|
| `proxy` | string \/ null | Global proxy for provider HTTP calls |
| `noProxy` | string[] | Hosts that should bypass the proxy |

### Proxy precedence

| Priority | Config | Meaning |
|--------|------|------|
| Highest | `providers.<name>.proxy = "http://..."` | That provider uses its own proxy |
| Highest | `providers.<name>.proxy = ""` | Force direct connection for that provider |
| Middle | `network.proxy` | Global proxy for providers without an override |
| Lowest | no proxy config | Direct connection |

### Typical proxy scenarios

**Use one global proxy for all providers:**

```json5
{
  network: {
    proxy: "http://127.0.0.1:7890",
  },
}
```

**Use a global proxy, but keep local Ollama direct:**

```json5
{
  network: {
    proxy: "http://127.0.0.1:7890",
    noProxy: ["localhost", "127.0.0.1"],
  },
  providers: {
    ollama: {
      apiBase: "http://localhost:11434",
      proxy: "",
    },
  },
}
```

**Use no global proxy, but proxy only Anthropic:**

```json5
{
  providers: {
    anthropic: {
      apiKey: "sk-ant-xxxxxxxx",
      proxy: "http://127.0.0.1:7890",
    },
    deepseek: {
      apiKey: "sk-xxxxxxxx",
    },
  },
}
```

**Use SOCKS5:**

```json5
{
  network: {
    proxy: "socks5://127.0.0.1:1080",
  },
}
```

---

## 4. Telegram channel proxy

Telegram has its own channel-level proxy setting, separate from provider HTTP proxy settings.

```json5
{
  channels: {
    telegram: {
      enabled: true,
      token: "your-bot-token",
      allowFrom: ["123456789"],
      proxy: "http://127.0.0.1:7890",
    },
  },
}
```

This proxy applies to Telegram transport. It does not replace provider proxy resolution.

---

## 5. Fast config updates from the CLI

You do not have to hand-edit `config.json5` for every change. Use `blockcell config set` for quick updates.

```bash
# Set the global proxy
blockcell config set network.proxy "http://127.0.0.1:7890"

# Set the default model
blockcell config set agents.defaults.model "deepseek-flash"

# Set the DeepSeek API key
blockcell config set providers.deepseek.apiKey "sk-xxxxxxxx"

# Give Anthropic a dedicated proxy
blockcell config set providers.anthropic.proxy "http://127.0.0.1:7890"

# Force vllm to connect directly
blockcell config set providers.vllm.proxy ""

# Set a JSON5 array value
blockcell config set network.noProxy "['localhost', '127.0.0.1', '::1']"

# Read the current value back
blockcell config get network.proxy

# Show provider summaries
blockcell config providers
```

Current CLI behavior:

- `config set` tries to parse values as JSON5 / scalar values first
- if parsing fails, it saves the value as a plain string
- `config show` prints pretty JSON, even though the underlying file is JSON5

---

## 6. Full configuration example

A practical production-style example:

```json5
{
  providers: {
    anthropic: {
      apiKey: "sk-ant-xxxxxxxx",
    },
    deepseek: {
      apiKey: "sk-xxxxxxxx",
    },
    openrouter: {
      apiKey: "sk-or-xxxxxxxx",
      apiBase: "https://openrouter.ai/api/v1",
    },
    ollama: {
      apiBase: "http://localhost:11434",
      proxy: "",
    },
  },
  network: {
    proxy: "http://127.0.0.1:7890",
    noProxy: ["localhost", "127.0.0.1", "::1"],
  },
  agents: {
    defaults: {
      model: "claude-sonnet-5-5",
      maxTokens: 8192,
      temperature: 0.7,
      maxToolIterations: 20,
      evolutionModel: "deepseek-flash",
      evolutionProvider: "deepseek",
    },
  },
  gateway: {
    host: "0.0.0.0",
    port: 18790,
    apiToken: "your-secret-token",
  },
}
```

This pattern uses Claude as the main model, a cheaper DeepSeek model for evolution tasks, a global proxy for cloud providers, and a direct local connection for Ollama.

---

*Previous: [CLI Reference](./17_cli_reference.md)*

*Next: [MCP Server Integration](./19_mcp_servers.md)*

*Index: [Series directory](./00_index.md)*
