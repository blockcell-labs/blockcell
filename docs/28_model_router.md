# ModelRouter 智能路由与自动降级

> 适合读者：已经了解 Provider Pool，想在多个模型之间做成本优化、质量优先或高可用降级的用户。

---

## 这个特性解决什么问题

传统配置只指定一个 `model` 和一个 `provider`。这很简单，但会带来几个问题：

- 短对话也可能调用昂贵的强模型，成本不可控
- 主 Provider 连接失败时，当前请求只能失败
- 本地模型、便宜模型、强模型之间缺少统一调度入口
- 多 agent 场景下，不同 agent 很难使用不同模型路由策略

ModelRouter 在现有 Provider Pool 上增加两类能力：

1. **路由策略**：根据 `routingStrategy` 和当前请求上下文选择模型。
2. **连接阶段自动降级**：首选 Provider 在建立流之前失败时，立即尝试同一 pool 中的下一个可用 Provider。

---

## 和 Provider Pool 的关系

Provider Pool 负责维护多个模型条目的健康状态、优先级、权重和熔断冷却。

ModelRouter 使用 Provider Pool 的条目，但选择方式不再只有“最高优先级 + 权重随机”。你可以通过 `agents.defaults.routingStrategy` 指定策略。

如果不配置 `routingStrategy`，默认是：

```json
"routingStrategy": "manual"
```

也就是继续沿用原有 Provider Pool 行为。

---

## 配置示例

### 成本优化：短对话走便宜模型，复杂任务走强模型

```json
{
  "agents": {
    "defaults": {
      "routingStrategy": "cost_optimized",
      "modelPool": [
        {
          "model": "deepseek-v4-pro",
          "provider": "deepseek",
          "priority": 1,
          "weight": 3
        },
        {
          "model": "gpt-6-luna",
          "provider": "openai",
          "priority": 5,
          "weight": 1
        }
      ]
    }
  },
  "providers": {
    "deepseek": { "apiKey": "YOUR_DEEPSEEK_KEY" },
    "openai": { "apiKey": "YOUR_OPENAI_KEY" }
  }
}
```

在 `cost_optimized` 下：

- 当前消息数 `<= 4` 时，选择最低优先级组，也就是 `priority` 数字最大的条目。上例会优先选 `gpt-6-luna`。
- 当前消息数 `> 4` 时，回到正常 Provider Pool 选择，也就是优先选 `priority` 数字最小的主力模型。上例会优先选 `deepseek-v4-pro`。

> 注意：Provider Pool 中 `priority` 的含义是“小数字 = 高优先级”。`cost_optimized` 利用低优先级条目表达“便宜/简单任务模型”。

---

## 支持的路由策略

| 策略 | 行为 | 适合场景 |
|------|------|----------|
| `manual` | 原有 Provider Pool 行为：最高优先级组内按 `weight` 随机 | 你想完全手工控制优先级和权重 |
| `cost_optimized` | 短上下文选择低优先级便宜模型，长上下文选择高优先级主力模型 | 降低日常短对话成本 |
| `quality_first` | 继续优先选择最高优先级模型 | 质量优先，成本不是主要问题 |
| `latency_first` | 预留低延迟策略入口；当前按可用条目的成功历史做稳定性近似 | 实验性策略，不建议作为唯一生产策略 |

实际选择前仍会过滤不可用条目：

- `Dead` 条目不会被选择
- `Cooling` 条目默认跳过
- 如果所有非 Dead 条目都在冷却中，会临时解除冷却作为保底

---

## 多 agent 覆盖

`routingStrategy` 可以配置在默认 agent 上：

```json
{
  "agents": {
    "defaults": {
      "routingStrategy": "cost_optimized"
    }
  }
}
```

也可以在 `agents.list` 的具体 agent 中覆盖：

```json
{
  "agents": {
    "defaults": {
      "routingStrategy": "cost_optimized"
    },
    "list": [
      {
        "id": "ops",
        "enabled": true,
        "routingStrategy": "quality_first"
      }
    ]
  }
}
```

上例中：

- 默认 agent 使用 `cost_optimized`
- `ops` agent 使用 `quality_first`

这适合把日常聊天、低成本自动化和关键运维任务分开配置。

---

## 连接阶段自动降级

主对话 LLM 调用使用流式接口。连接阶段自动降级只发生在流还没有建立时：

```text
选择 Provider A
  ├─ chat_stream() 建立失败：connection / timeout / DNS / refused / reset
  │     └─ 标记 A 的调用结果，排除 A，尝试 Provider B
  │
  └─ chat_stream() 已返回 stream receiver
        ├─ 正常收到 token：继续使用 A
        └─ 中途 StreamChunk::Error / 超时：不切换 Provider，走原有 retry/reset 流程
```

这样做是为了避免“半个回答来自模型 A，后半个回答来自模型 B”的状态污染。

### 会触发连接阶段降级的错误关键词

当前实现会把错误文本中包含以下词的建立前失败视为连接阶段错误：

- `connection`
- `connect`
- `timeout`
- `timed out`
- `dns`
- `refused`
- `reset`
- `network`
- `unreachable`

这些错误通常来自网络、DNS、连接拒绝、请求建立超时或远端连接重置。

---

## 与 retry / 熔断的关系

ModelRouter 不替代原有 retry 和熔断，而是补充它们：

1. 每个 LLM retry attempt 内，如果连接阶段失败，会尝试同一 Provider Pool 中尚未尝试过的其他条目。
2. 每次失败仍会通过 `ProviderPool::report()` 更新健康状态。
3. 429、401/403、5xx、Transient 仍按 Provider Pool 原状态机处理：
   - 429 进入冷却
   - 401/403 标记 Dead
   - 连续 transient/server error 达阈值后冷却
4. 如果流式输出已经开始，错误不会触发 Provider 切换，只会进入原有 retry 逻辑。

---

## 配置模式建议

### 1. 默认推荐：主力模型 + 便宜模型

```json
"routingStrategy": "cost_optimized",
"modelPool": [
  { "model": "deepseek-v4-pro", "provider": "deepseek", "priority": 1, "weight": 3 },
  { "model": "gpt-6-luna", "provider": "openai", "priority": 5, "weight": 1 }
]
```

适合大多数个人和团队使用。短消息省钱，复杂上下文仍走强模型。

### 2. 质量优先：多个强模型互备

```json
"routingStrategy": "quality_first",
"modelPool": [
  { "model": "claude-opus-5-5", "provider": "anthropic", "priority": 1, "weight": 1 },
  { "model": "gpt-6.1-sol", "provider": "openai", "priority": 1, "weight": 1 },
  { "model": "deepseek-v4-pro", "provider": "deepseek", "priority": 2, "weight": 1 }
]
```

适合关键任务、代码生成、长文档分析。优先在最高质量组内分流。

### 3. 本地优先 + 云端兜底

```json
"routingStrategy": "manual",
"modelPool": [
  { "model": "ollama/qwen3.8", "provider": "ollama", "priority": 1, "weight": 1 },
  { "model": "deepseek-v4-pro", "provider": "deepseek", "priority": 2, "weight": 1 }
]
```

适合离线优先或隐私优先场景。本地 Ollama 不可用时，连接阶段失败可以降级到云端。

---

## CLI 覆盖行为

如果你在 CLI 使用 `--model` 或 `--provider`：

```bash
blockcell agent --model gpt-6.1-sol --provider openai -m "分析这个项目"
```

本次运行会使用指定模型/Provider，并清空继承的 `modelPool`。这适合临时指定模型，不会修改配置文件。

---

## 排查建议

### 没有按预期选择便宜模型

检查：

- 是否配置了 `"routingStrategy": "cost_optimized"`
- 短上下文判断是按消息数量，当前阈值是 `<= 4`
- 便宜模型是否放在更低优先级组，也就是更大的 `priority` 数字
- 便宜模型对应 provider 是否健康，是否处于 `Dead`

### 首选模型失败后没有切换

检查失败发生在哪个阶段：

- 如果 `chat_stream()` 建立前返回连接错误，会尝试其他 Provider
- 如果已经开始输出 token，后续错误不会切换 Provider

这属于预期行为，目的是避免混合不同模型的半截输出。

### 所有模型都不可用

检查：

- `providers.<name>.apiKey` 是否有效
- `modelPool[].provider` 是否和 `providers` key 一致
- 是否有条目因为 401/403 被标记为 `Dead`
- 是否所有条目都在冷却中

---

## 内部实现位置

| 文件 | 说明 |
|------|------|
| `crates/core/src/config.rs` | `RoutingStrategy`、`AgentDefaults.routing_strategy`、per-agent override |
| `crates/providers/src/pool.rs` | `RoutingContext`、`ProviderPoolEntry`、`acquire_with_strategy*` |
| `crates/agent/src/runtime.rs` | 主 LLM 调用接入 routing strategy 和连接阶段 fallback |
| `bin/blockcell/src/commands/config_cmd.rs` | 配置 schema 中暴露 `routingStrategy` |

---

## 相关文档

- [Provider Pool — 多模型高可用配置](./20_provider_pool.md)
- [代理与 LLM Provider 配置](./18_proxy_and_provider_config.md)
- [intentRouter 多智能体配置指南](./21_intent_router_profiles.md)
- [Gateway 模式](./08_gateway_mode.md)
