# Provider Pool — 多模型高可用配置

> 版本要求：blockcell ≥ 0.1.3

## 概述

Provider Pool 允许你在配置文件中声明一个**模型+供应商列表**，运行时按优先级和权重动态选取，并在调用失败时自动降级到备用条目，避免所有请求集中到单一模型。

如果你需要在 Provider Pool 之上按任务复杂度、成本或质量自动选择模型，可以继续阅读：[ModelRouter 智能路由与自动降级](./28_model_router.md)。

主要特性：

- **加权随机选取**：同优先级条目按 weight 随机分发，分散请求压力
- **优先级分层**：priority 小的条目优先；主力模型不可用时才降级到备用
- **熔断冷却**：连续失败 3 次或收到 429/5xx 后自动冷却 60 秒，到期后自动恢复
- **永久标记死亡**：401/403 认证错误直接标记 Dead，不再重试
- **向后兼容**：不配置 `modelPool` 时，继续使用旧的 `model` + `provider` 字段

---

## 配置格式

### 旧格式（仍然支持）

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

### 新格式：modelPool

```json
{
  "agents": {
    "defaults": {
      "modelPool": [
        {
          "model": "deepseek-v4-pro",
          "provider": "deepseek",
          "weight": 3,
          "priority": 1
        },
        {
          "model": "claude-sonnet-5-5",
          "provider": "anthropic",
          "weight": 2,
          "priority": 1
        },
        {
          "model": "gemini-3.8-flash",
          "provider": "gemini",
          "weight": 1,
          "priority": 2
        }
      ]
    }
  },
  "providers": {
    "deepseek": { "apiKey": "sk-xxx" },
    "anthropic": { "apiKey": "sk-ant-xxx" },
    "gemini":    { "apiKey": "AIza-xxx" }
  }
}
```

### 字段说明

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `model` | string | — | 模型名称，如 `deepseek-v4-pro`、`claude-sonnet-5-5` |
| `provider` | string | — | 对应 `providers` 表中的 key，如 `deepseek`、`anthropic` |
| `weight` | u32 | `1` | 同优先级内的加权随机权重，越大被选中概率越高 |
| `priority` | u32 | `1` | 优先级，**小数字 = 高优先级**，仅在高优先级全部不可用时才选低优先级 |

---

## 选取算法

每次 LLM 调用时：

1. **过滤健康条目**：排除 `Cooling` 和 `Dead` 状态的条目（冷却已到期的会先被恢复为 `Healthy`）
2. **取最高优先级组**：选出所有健康条目中 `priority` 最小的一组
3. **加权随机选取**：在该组内按 `weight` 比例随机选一个
4. **无可用条目即返回空**：若没有健康条目（全部处于冷却 / Dead / 被排除），`acquire` 返回 `None`，
   不会临时解除冷却去继续打限速中的后端

---

## 健康状态机

```
Healthy ──[3次连续失败]──► Cooling(60s) ──[冷却到期]──► Healthy
   │                           │
   │──[401/403]───────────────► Dead（永久）
   │
   │──[429/5xx]──────────────► Cooling(60s)
```

每次调用结束后，调用方上报结果：

| 结果 | 触发行为 |
|------|----------|
| `Success` | 重置连续失败计数 |
| `RateLimit` (429) | 立即进入冷却 |
| `AuthError` (401/403) | 标记 Dead |
| `Transient` / `ServerError` | 累计失败 +1，达 3 次后冷却 |

---

## 典型场景配置

### 场景 1：主备切换（成本优先）

```json
"modelPool": [
  { "model": "deepseek-v4-pro",           "provider": "deepseek",   "weight": 1, "priority": 1 },
  { "model": "gpt-6-luna",               "provider": "openai",     "weight": 1, "priority": 2 }
]
```

正常使用 DeepSeek（便宜），不可用时自动切换到 GPT-5.4-mini。

### 场景 2：多主均衡（高并发）

```json
"modelPool": [
  { "model": "deepseek-v4-pro",           "provider": "deepseek",   "weight": 2, "priority": 1 },
  { "model": "claude-sonnet-5-5","provider": "anthropic",  "weight": 1, "priority": 1 },
  { "model": "gemini-3.8-flash",          "provider": "gemini",     "weight": 1, "priority": 1 }
]
```

三个主力模型按 2:1:1 分配请求，任一失败其余继续服务。

### 场景 3：本地优先 + 云端兜底

```json
"modelPool": [
  { "model": "ollama/qwen3.8",  "provider": "ollama",    "weight": 1, "priority": 1 },
  { "model": "deepseek-v4-pro",    "provider": "deepseek",  "weight": 1, "priority": 2 }
]
```

优先走本地 Ollama，本地不可用时自动走 DeepSeek。

---

## CLI 覆盖

命令行 `--model` 和 `--provider` 参数会在本次运行中把 `modelPool` 清空，并以指定值创建单条目 pool：

```bash
# 覆盖单次运行的模型，不影响配置文件
blockcell agent --model gpt-6.1-sol --provider openai -m "分析这个文件"
```

也就是说，CLI 覆盖只影响当前进程，不会回写 `config.json5`。

---

## 自进化（Evolution）Provider

`evolutionModel` / `evolutionProvider` 仍然支持，用于为自进化任务指定独立模型，避免与主对话争抢资源：

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

若不配置，自进化任务将从主 pool 中取一个可用 provider。

如果主 pool 中没有任何健康条目（全部处于冷却 / Dead / 不可用），选取会返回空结果——**不会**临时解除冷却去复用冷却中的条目。冷却到期的条目会在下一次选取时自动恢复为 `Healthy`。

---

## 内部实现说明

| 文件 | 说明 |
|------|------|
| `crates/core/src/config.rs` | `ModelEntry` 结构体、`AgentDefaults.model_pool` 字段 |
| `crates/providers/src/pool.rs` | `ProviderPool` 核心逻辑：健康管理、加权随机、熔断 |
| `crates/providers/src/lib.rs` | 导出 `ProviderPool`、`CallResult`、`PoolEntryStatus` |
| `crates/agent/src/runtime.rs` | `AgentRuntime` 持有 `Arc<ProviderPool>`，每次 LLM 调用 acquire + report |
| `bin/blockcell/src/commands/agent.rs` | `build_pool_with_overrides()` 构建 pool，替代旧 `create_provider_with_overrides()` |
| `bin/blockcell/src/commands/gateway.rs` | Gateway 模式同样使用 `ProviderPool` |
