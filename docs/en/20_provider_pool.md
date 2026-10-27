# Article 20: Provider Pool — Multi-Model High Availability

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — Article 20

## Overview

A traditional LLM config picks **one model + one provider**. That is simple, but it creates a single point of failure:

- provider outages interrupt all requests
- rate limits hit the whole system
- traffic cannot be distributed by cost or priority
- local models and cloud models cannot be combined cleanly

blockcell's **Provider Pool** solves this by letting you declare a list of **model + provider entries**. At runtime, blockcell selects from that pool dynamically by priority and weight, and automatically falls back when calls fail.

## Configuration format

### Legacy format (still supported)

```json
{
  "agents": {
    "defaults": {
      "model": "deepseek-v4-pro",
      "provider": "deepseek"
    }
  }
}
```

### New format: `modelPool`

```json
{
  "agents": {
    "defaults": {
      "modelPool": [
        {
          "model": "deepseek-v4-pro",
          "provider": "deepseek",
          "weight": 2,
          "priority": 1
        },
        {
          "model": "claude-opus-5-5",
          "provider": "anthropic",
          "weight": 1,
          "priority": 1
        },
        {
          "model": "gpt-6.1-sol",
          "provider": "openai",
          "weight": 1,
          "priority": 2
        }
      ]
    }
  }
}
```

### Field meanings

| Field | Type | Default | Meaning |
|------|------|---------|---------|
| `model` | string | required | Concrete model name |
| `provider` | string | required | Provider key in `providers` |
| `weight` | u32 | `1` | Weighted share within the same priority group |
| `priority` | u32 | `1` | Lower number = higher priority |

## Selection algorithm

For each LLM call:

1. filter out entries in `Cooling` or `Dead`
2. find the healthy entries with the smallest `priority`
3. pick one entry from that top group using `weight`
4. if everything is cooling (but not dead), temporarily relax cooling as a fallback

## Health state machine

```text
Healthy ──[3 consecutive failures]──► Cooling(60s) ──[cooldown expires]──► Healthy
   │
   ├──[401/403]────────────────────► Dead
   │
   └──[429/5xx]───────────────────► Cooling(60s)
```

Each completed call reports one of these outcomes back to the pool:

| Result | Effect |
|--------|--------|
| `Success` | reset failure count |
| `RateLimit` (429) | enter cooling immediately |
| `AuthError` (401/403) | mark entry dead |
| `Transient` / `ServerError` | increment failure count; enter cooling after threshold |

## Typical configuration patterns

### Pattern 1: primary + backup (cost-first)

```json
"modelPool": [
  { "model": "deepseek-v4-pro", "provider": "deepseek", "weight": 1, "priority": 1 },
  { "model": "gpt-6-luna", "provider": "openai", "weight": 1, "priority": 2 }
]
```

Use DeepSeek normally, then fall back to GPT-5.4-mini when needed.

### Pattern 2: multi-primary load balancing

```json
"modelPool": [
  { "model": "deepseek-v4-pro", "provider": "deepseek", "weight": 2, "priority": 1 },
  { "model": "claude-sonnet-5-5", "provider": "anthropic", "weight": 1, "priority": 1 },
  { "model": "gemini-3.8-flash", "provider": "gemini", "weight": 1, "priority": 1 }
]
```

Three primary models share traffic in a 2:1:1 ratio.

### Pattern 3: local-first with cloud fallback

```json
"modelPool": [
  { "model": "ollama/qwen3.8", "provider": "ollama", "weight": 1, "priority": 1 },
  { "model": "deepseek-v4-pro", "provider": "deepseek", "weight": 1, "priority": 2 }
]
```

Prefer local Ollama, then fall back to DeepSeek if local inference is unavailable.

## CLI overrides

`--model` and `--provider` on the CLI disable the inherited `modelPool` for that run and build a single-entry pool from the override:

```bash
# Override model/provider for a single run without changing config
blockcell agent --model gpt-6.1-sol --provider openai -m "Analyze this file"
```

## Evolution provider

`evolutionModel` and `evolutionProvider` still work and let you dedicate a cheaper or faster model to self-evolution work:

```json
{
  "agents": {
    "defaults": {
      "modelPool": [ ... ],
      "evolutionModel": "deepseek-v4-pro",
      "evolutionProvider": "deepseek"
    }
  }
}
```

If you do not configure them, evolution tasks use the main pool.

## Internal implementation

| File | Role |
|------|------|
| `crates/core/src/config.rs` | `ModelEntry` and `AgentDefaults.model_pool` |
| `crates/providers/src/pool.rs` | `ProviderPool` core logic: health, weighting, cooling, fallback |
| `crates/providers/src/lib.rs` | exports `ProviderPool`, `CallResult`, `PoolEntryStatus` |
| `crates/agent/src/runtime.rs` | reports LLM outcomes back to the pool during runtime |
| `bin/blockcell/src/commands/agent.rs` | builds the pool for CLI agent mode |
| `bin/blockcell/src/commands/gateway.rs` | uses the same pool model in Gateway mode |

---

*Previous: [MCP Server Integration](./19_mcp_servers.md)*

*Next: [intentRouter Multi-Profile Guide](./21_intent_router_profiles.md)*

*Index: [Series directory](./00_index.md)*
