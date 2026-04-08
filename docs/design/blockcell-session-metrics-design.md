# BlockCell 7层记忆系统监控方案

> BlockCell 记忆系统监控架构设计文档

---

## 目录

1. [概述](#一概述)
2. [监控架构](#二监控架构)
3. [配置常量](#三配置常量)
4. [熔断器设计](#四熔断器设计)
5. [事件定义](#五事件定义)
6. [Rust 实现](#六rust-实现)
7. [集成方案](#七集成方案)
8. [斜杠命令实现](#八斜杠命令实现-推荐方案)

**附录**:

- [附录 A: 实现清单](#附录-a-实现清单)
- [附录 B: 配置持久化](#附录-b-配置持久化)
- [附录 C: 测试策略](#附录-c-测试策略)
- [附录 D: 日志格式配置](#附录-d-日志格式配置)
- [附录 E: 与 ProcessingMetrics 的关系](#附录-e-与-processingmetrics-的关系)

---

## 一、概述

### 1.1 监控目标

| 目标 | 说明 |
|------|------|
| **可观测性** | 实时了解各层工作状态 |
| **性能追踪** | 记录 Token 消耗、延迟、缓存命中率 |
| **错误诊断** | 快速定位失败原因 |
| **容量规划** | 追踪存储增长趋势 |
| **熔断保护** | 防止级联失败 |

### 1.2 监控实践参考

参考业界最佳实践，使用 `tracing` 框架记录监控事件，关键特点：

- **Fire-and-forget**: 异步记录，不阻塞主流程
- **结构化数据**: 每个事件携带丰富的元数据
- **多维度追踪**: Token、缓存、时间、状态等
- **熔断机制**: `consecutiveFailures` 防止无限重试

---

## 二、监控架构

### 2.1 整体架构

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         BlockCell 记忆监控架构                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │ Layer 7: Forked Agent                                                │   │
│  │ └─ Event: forked_agent_spawned, forked_agent_completed              │   │
│  ├─────────────────────────────────────────────────────────────────────┤   │
│  │ Layer 6: Auto Dream                                                  │   │
│  │ └─ Event: dream_started, dream_phase_completed, dream_finished      │   │
│  ├─────────────────────────────────────────────────────────────────────┤   │
│  │ Layer 5: Extract Memories                                            │   │
│  │ └─ Event: memory_extraction_started, memory_written, cursor_updated │   │
│  ├─────────────────────────────────────────────────────────────────────┤   │
│  │ Layer 4: Full Compact                                                │   │
│  │ └─ Event: compact_started, compact_completed, compact_failed        │   │
│  ├─────────────────────────────────────────────────────────────────────┤   │
│  │ Layer 3: Session Memory                                              │   │
│  │ └─ Event: session_memory_updated, session_memory_loaded             │   │
│  ├─────────────────────────────────────────────────────────────────────┤   │
│  │ Layer 2: Micro Compact                                               │   │
│  │ └─ Event: micro_compact_triggered, content_cleared                  │   │
│  ├─────────────────────────────────────────────────────────────────────┤   │
│  │ Layer 1: Tool Result Storage                                         │   │
│  │ └─ Event: tool_result_persisted, preview_generated, budget_exceeded │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                    │                                        │
│                                    ▼                                        │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │ 监控收集层 (MemoryMetrics)                                           │   │
│  │ ├─ tracing spans for timing                                         │   │
│  │ └─ structured logs for analytics                                    │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                    │                                        │
│                                    ▼                                        │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │ 存储与导出                                                           │   │
│  │ ├─ SQLite: session_metrics 表                                       │   │
│  │ └─ File: ~/.blockcell/logs/memory-metrics.jsonl                     │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 2.2 数据流

```
事件发生
    │
    ├─► tracing::info!() ──► 日志文件 (JSON 格式)
    │
    └─► Metrics::record() ──► 内存计数器
                               │
                               └─► Session 持久化
```

---

## 三、配置常量

各层的核心配置常量（与实际代码同步）：

### 3.1 Layer 1 常量

```rust
// crates/agent/src/response_cache.rs
pub const MAX_TOOL_RESULTS_PER_MESSAGE_CHARS: usize = 150_000;
```

### 3.2 Layer 2 常量

```rust
// crates/agent/src/history_projector.rs
pub const COMPACTABLE_TOOLS: &[&str] = &[
    "read_file", "shell", "grep", "glob",
    "web_search", "web_fetch", "file_edit", "file_write",
];

pub struct TimeBasedMCConfig {
    pub enabled: bool,
    pub gap_threshold_minutes: u32,  // 默认 60
    pub keep_recent: u32,            // 默认 5
}
```

### 3.3 Layer 3 常量

```rust
// crates/agent/src/session_memory/mod.rs
pub const MAX_SECTION_LENGTH: usize = 2000;
pub const MAX_TOTAL_SESSION_MEMORY_TOKENS: usize = 12000;
pub const EXTRACTION_WAIT_TIMEOUT_MS: u64 = 15_000;
pub const EXTRACTION_STALE_THRESHOLD_MS: u64 = 60_000; // 1 分钟
```

### 3.4 Layer 4 常量

```rust
// crates/agent/src/compact/mod.rs
pub const MAX_FILE_RECOVERY_TOKENS: usize = 50_000;
pub const MAX_SINGLE_FILE_TOKENS: usize = 5_000;
pub const MAX_SKILL_RECOVERY_TOKENS: usize = 25_000;
pub const MAX_SESSION_MEMORY_RECOVERY_TOKENS: usize = 12_000;
pub const MAX_FILES_TO_RECOVER: usize = 5;
pub const NO_TOOLS_PREAMBLE: &str = r#"IMPORTANT: You are in compact mode.
You cannot use any tools. You must generate a summary based solely on the conversation history.
Do not attempt to call any tools, read files, or execute commands."#;
```

### 3.5 Layer 5 常量

```rust
// crates/agent/src/auto_memory/mod.rs
pub const MIN_MESSAGES_FOR_EXTRACTION: usize = 10;
pub const EXTRACTION_COOLDOWN_MESSAGES: usize = 5;
pub const MAX_MEMORY_FILE_TOKENS: usize = 4000;
```

### 3.6 Layer 6 常量

```rust
// crates/scheduler/src/consolidator.rs
pub const TIME_GATE_THRESHOLD_HOURS: u64 = 24;
pub const SESSION_GATE_THRESHOLD: usize = 5;
pub const SESSION_MEMORY_EXPIRY_DAYS: u64 = 7;
pub const MAX_SESSIONS_TO_PROCESS: usize = 10;
```

### 3.7 Layer 7 常量

Layer 7 (Forked Agent) 主要处理运行时逻辑，当前没有定义配置常量。相关参数通过 `ForkedAgentParams` 结构体在运行时传递。

---

## 四、熔断器设计

### 4.1 背景与问题

在 Layer 4 Full Compact 场景中，压缩操作依赖外部 LLM 调用生成摘要。当 LLM 服务出现问题时，可能导致：

```
问题场景:
用户消息 → Token 超限 → 触发压缩 → LLM 调用失败 → 重试 → 再次失败 → ...
                                         ↓
                              无限循环消耗 Token 和时间
                                         ↓
                              用户等待超长，体验极差
                                         ↓
                              可能触发更多错误（超时、资源耗尽）
```

**典型故障模式**:

| 问题类型 | 表现 | 影响 |
|---------|------|------|
| **LLM 服务不稳定** | 压缩 API 调用频繁超时/失败 | 每次消息都尝试压缩，浪费时间 |
| **Prompt 设计问题** | 压缩结果不符合预期，后续处理失败 | 反复重试，Token 浪费 |
| **资源竞争** | 多个会话同时触发压缩 | 系统负载飙升，雪崩效应 |
| **级联失败** | 压缩失败导致后续逻辑异常 | 整个消息处理链路崩溃 |

### 4.2 熔断器概念

熔断器（Circuit Breaker）是一种保护系统的设计模式，灵感来自电路中的保险丝：

```
电路熔断器原理:
正常状态 (Closed) ──电流过大──► 熔断 (Open) ──阻断电流──► 保护电路
                              │
                              └──冷却后──► 尝试恢复 (HalfOpen)
```

**核心思想**: 当检测到连续失败时，主动"熔断"停止执行，防止继续浪费资源。经过冷却期后，允许"试探性"恢复。

### 4.3 三态模型

```
                    ┌─────────────────────────────────────┐
                    │                                     │
                    ▼                                     │
            ┌───────────────┐                      ┌───────────────┐
            │    CLOSED     │  连续失败 >= N 次    │     OPEN      │
            │   (正常状态)   │ ─────────────────────►│   (熔断状态)   │
            │               │                      │               │
            │  允许所有请求  │                      │  拒绝所有请求  │
            └───────┬───────┘                      └───────┬───────┘
                    │                                      │
                    │                                      │ 冷却期结束
                    │ 成功                                 │
                    │                                      ▼
                    │                              ┌───────────────┐
                    │                              │   HALF_OPEN   │
                    │                              │   (半开状态)   │
                    │                              │               │
                    │                              │ 允许试探请求  │
                    │                              └───────┬───────┘
                    │                                      │
                    │                 ┌────────────────────┼────────────────────┐
                    │                 │                                         │
                    │                 │ 成功                                    │ 失败
                    │                 ▼                                         ▼
                    └─────────────────┘                              返回 OPEN
```

| 状态 | 行为 | 转换条件 |
|------|------|---------|
| **Closed (关闭)** | 正常执行所有请求 | 连续失败次数 ≥ 阈值 → Open |
| **Open (开启)** | 快速失败，不执行请求 | 冷却期结束 → HalfOpen |
| **HalfOpen (半开)** | 允许有限请求试探 | 成功 → Closed；失败 → Open |

### 4.4 BlockCell 熔断器设计

#### 4.4.1 应用场景

| 场景 | 熔断目标 | 默认配置 |
|------|---------|---------|
| **Layer 4 Compact** | 防止压缩无限重试 | 连续 3 次失败 → 熔断 60 秒 |
| **Layer 5 Memory Extraction** | 防止提取循环 | 连续 3 次失败 → 熔断 300 秒 |
| **Layer 6 Dream Consolidation** | 防止后台任务堆积 | 连续 2 次失败 → 熔断 900 秒 |

#### 4.4.2 配置参数

```rust
/// 熔断器配置
pub struct CircuitBreakerConfig {
    /// 最大连续失败次数 - 触发熔断的阈值
    /// 默认: 3 次
    pub max_failures: u64,

    /// 熔断冷却期 - Open 状态持续时间
    /// 默认: 60 秒
    /// 设置建议: 根据底层服务的恢复时间调整
    pub reset_timeout: Duration,

    /// 半开状态最大尝试次数
    /// 默认: 1 次
    /// 设置建议: 保持 1 次，避免半开状态下过载
    pub half_open_max_calls: u64,
}
```

**参数调优建议**:

| 场景 | max_failures | reset_timeout | 说明 |
|------|-------------|---------------|------|
| 快速恢复服务 | 2-3 | 30-60s | API 网关、微服务 |
| 慢恢复服务 | 1-2 | 300-600s | 数据库、LLM |
| 后台任务 | 2-3 | 900-1800s | 批处理、清理任务 |

#### 4.4.3 状态追踪

```rust
/// 熔断器状态 (存储在全局 MemoryMetrics 中)
pub struct CircuitBreakerState {
    /// 当前状态: 0=Closed, 1=Open, 2=HalfOpen
    pub state: AtomicU64,

    /// 连续失败计数
    pub failure_count: AtomicU64,

    /// 最后失败时间 (用于计算冷却期)
    pub last_failure_time: Mutex<Option<Instant>>,

    /// 半开状态尝试次数
    pub half_open_calls: AtomicU64,
}
```

### 4.5 多层熔断器设计

#### 4.5.1 为什么需要多层熔断器？

不同的记忆层有不同的故障模式和恢复需求：

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           熔断器分层设计原理                                  │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Layer 4 Compact (用户同步操作)                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │ 触发方式: 用户发送消息时自动触发                                       │   │
│  │ 失败影响: 用户等待超长，体验极差                                       │   │
│  │ 恢复策略: 快速熔断 + 快速恢复                                          │   │
│  │ 配置建议: max_failures=3, reset_timeout=60s                          │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  Layer 5 Memory Extraction (后台异步操作)                                    │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │ 触发方式: 对话过程中异步提取记忆                                       │   │
│  │ 失败影响: 不影响当前对话，但记忆不更新                                  │   │
│  │ 恢复策略: 中等熔断 + 后台恢复                                          │   │
│  │ 配置建议: max_failures=3, reset_timeout=300s                         │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  Layer 6 Dream Consolidation (定时后台任务)                                  │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │ 触发方式: 定时触发，用户无感知                                         │   │
│  │ 失败影响: 长期影响记忆质量，但不阻塞任何操作                            │   │
│  │ 恢复策略: 慢熔断 + 长冷却期                                            │   │
│  │ 配置建议: max_failures=2, reset_timeout=900s                         │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

#### 4.5.2 三层熔断器配置对比

| 层 | 熔断器名称 | max_failures | reset_timeout | half_open_max_calls | 说明 |
|----|-----------|-------------|---------------|---------------------|------|
| **Layer 4** | `COMPACT_CIRCUIT_BREAKER` | 3 | 60s | 1 | 用户在等待，需要快速恢复 |
| **Layer 5** | `MEMORY_EXTRACTION_CIRCUIT_BREAKER` | 3 | 300s | 1 | 后台操作，可接受较长恢复时间 |
| **Layer 6** | `DREAM_CIRCUIT_BREAKER` | 2 | 900s | 1 | 定时任务，失败可能意味着系统性问题 |

#### 4.5.3 熔断器独立性原则

每个熔断器独立运行，互不影响：

```
Layer 4 熔断器 OPEN ──► 不影响 Layer 5/6 的正常执行
Layer 5 熔断器 OPEN ──► 不影响 Layer 4 的压缩操作
Layer 6 熔断器 OPEN ──► 不影响任何用户操作

原因:
1. 不同层的故障原因可能不同（网络、API、资源等）
2. 某一层失败不应阻塞其他层的功能
3. 用户应该始终获得最佳可用服务
```

### 4.6 熔断器与业务流程集成

#### 4.6.1 压缩流程中的熔断 (Layer 4)

```
消息处理循环
       │
       ▼
┌─────────────────────────────────────┐
│ 1. 检查 Token 是否超限              │
│    estimate_tokens > threshold?     │
└──────────────┬──────────────────────┘
               │ 是
               ▼
┌─────────────────────────────────────┐
│ 2. 熔断器检查                       │
│    circuit_breaker.allow()?         │
│                                     │
│    ┌─ Closed  → 允许执行压缩        │
│    ├─ Open    → 返回错误，跳过压缩  │
│    └─ HalfOpen → 允许一次试探       │
└──────────────┬──────────────────────┘
               │ 允许
               ▼
┌─────────────────────────────────────┐
│ 3. 执行压缩 (LLM 调用)              │
└──────────────┬──────────────────────┘
               │
       ┌───────┴───────┐
       │               │
    成功             失败
       │               │
       ▼               ▼
┌──────────────┐ ┌──────────────────────┐
│ 熔断器重置   │ │ 熔断器记录失败       │
│ state=Closed │ │ failure_count++      │
│ failure=0    │ │                      │
│              │ │ if failure >= max:   │
│              │ │   state=Open         │
│              │ │   last_failure=now   │
└──────────────┘ └──────────────────────┘
```

#### 4.6.2 Memory Extraction 熔断 (Layer 5)

```
记忆提取流程 (auto_memory)
       │
       ▼
┌─────────────────────────────────────┐
│ 1. 检查是否需要提取                  │
│    messages > threshold?            │
└──────────────┬──────────────────────┘
               │ 是
               ▼
┌─────────────────────────────────────┐
│ 2. 熔断器检查                       │
│    extraction_cb.allow()?           │
│    ├─ Closed  → 允许执行            │
│    ├─ Open    → 跳过，不阻塞主流程  │
│    └─ HalfOpen → 尝试一次           │
└──────────────┬──────────────────────┘
               │ 允许
               ▼
┌─────────────────────────────────────┐
│ 3. 执行 LLM 记忆提取                │
└──────────────┬──────────────────────┘
               │
       ┌───────┴───────┐
    成功             失败
       │               │
       ▼               ▼
  熔断器重置       熔断器记录失败
  记录指标         跳过本次提取

特点: 失败不阻塞主对话流程，用户无感知
```

#### 4.6.3 Dream Consolidation 熔断 (Layer 6)

```
定时整合任务 (scheduler/consolidator)
       │
       ▼
┌─────────────────────────────────────┐
│ 1. 门控检查                         │
│    time_gate + session_gate         │
└──────────────┬──────────────────────┘
               │ 通过
               ▼
┌─────────────────────────────────────┐
│ 2. 熔断器检查                       │
│    dream_cb.allow()?                │
│    ├─ Closed  → 执行整合            │
│    ├─ Open    → 跳过本次运行        │
│    └─ HalfOpen → 尝试一次           │
└──────────────┬──────────────────────┘
               │ 允许
               ▼
┌─────────────────────────────────────┐
│ 3. 执行梦境整合 (LLM 调用)          │
└──────────────┬──────────────────────┘
               │
       ┌───────┴───────┐
    成功             失败
       │               │
       ▼               ▼
  熔断器重置       熔断器记录失败
  记录指标         900秒后再试

特点: 完全后台任务，失败无用户影响，最长冷却期
```

#### 4.6.4 熔断后的降级策略

当熔断器处于 Open 状态时，系统需要有降级策略：

```rust
/// 压缩熔断后的降级处理
async fn handle_compact_circuit_open(
    &self,
    messages: &[ChatMessage],
) -> CompactResult {
    tracing::warn!(
        target: "blockcell.session_metrics.layer4",
        "Compact circuit breaker is OPEN, using fallback strategy"
    );

    // 策略 1: 简单截断 (丢弃最早的非系统消息)
    let truncated = self.truncate_oldest_messages(messages, KEEP_RECENT);

    // 策略 2: 返回错误，让上层决定
    // return CompactResult::failed("Circuit breaker open");

    // 策略 3: 使用本地快速摘要 (不调用 LLM)
    // let summary = self.local_quick_summary(messages);

    CompactResult {
        success: true,
        summary_message: truncated,
        recovery_message: String::new(),
        pre_compact_tokens: estimate_messages_tokens(messages),
        post_compact_tokens: estimate_messages_tokens(&truncated),
        is_fallback: true,  // 标记为降级结果
    }
}
```

**降级策略选择**:

| 策略 | 优点 | 缺点 | 适用场景 |
|------|------|------|---------|
| **简单截断** | 快速、无 Token 消耗 | 可能丢失重要上下文 | 临时应急 |
| **返回错误** | 明确告知用户 | 用户体验差 | 关键操作 |
| **本地摘要** | 保留部分上下文 | 质量不如 LLM | 可接受的降级 |

### 4.6 监控与告警

#### 4.6.1 熔断器指标

```rust
/// 熔断器相关指标
pub struct CircuitBreakerMetrics {
    /// 当前状态
    pub state: CircuitState,

    /// 连续失败次数
    pub consecutive_failures: u64,

    /// 累计熔断次数
    pub total_trips: u64,

    /// 累计成功恢复次数
    pub total_recoveries: u64,

    /// 最后熔断时间
    pub last_trip_time: Option<SystemTime>,

    /// 熔断总持续时间 (秒)
    pub total_open_duration_secs: u64,
}
```

#### 4.6.2 熔断事件

| 事件 | 触发时机 | 日志级别 |
|------|---------|---------|
| `circuit_breaker_trip` | Closed → Open | `WARN` |
| `circuit_breaker_reset` | HalfOpen → Closed | `INFO` |
| `circuit_breaker_half_open` | Open → HalfOpen | `INFO` |
| `circuit_breaker_reject` | Open 状态拒绝请求 | `DEBUG` |

#### 4.6.3 告警规则

```yaml
# 告警配置示例
alerts:
  - name: "Compact Circuit Breaker Tripped"
    condition: "circuit_breaker.state == OPEN && layer == 4"
    severity: "warning"
    message: "Layer 4 Compact 熔断器已触发，压缩操作暂停"

  - name: "Circuit Breaker Frequent Trips"
    condition: "circuit_breaker.total_trips > 5 within 1h"
    severity: "critical"
    message: "熔断器频繁触发，请检查 LLM 服务状态"
```

### 4.7 与其他模式的配合

```
┌─────────────────────────────────────────────────────────────────┐
│                     故障处理模式组合                              │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐        │
│  │   超时控制   │───►│   重试策略   │───►│   熔断器    │        │
│  │  (Timeout)  │    │  (Retry)    │    │(Circuit Brk)│        │
│  └─────────────┘    └─────────────┘    └─────────────┘        │
│         │                  │                  │               │
│         ▼                  ▼                  ▼               │
│   单次请求超时         指数退避重试        快速失败保护         │
│   防止长时间阻塞       增加成功概率        防止级联失败         │
│                                                                 │
│  建议组合:                                                       │
│  1. 超时: 30-60 秒 (压缩 API 调用)                              │
│  2. 重试: 最多 2 次，指数退避 (100ms, 500ms)                    │
│  3. 熔断: 连续 3 次失败后触发                                    │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## 五、事件定义

### 5.1 事件命名规范

```
blockcell.session_metrics.{layer}.{action}

layer: layer1, layer2, layer3, layer4, layer5, layer6, layer7
action: started, completed, failed, triggered, updated
```

### 5.2 Layer 1 事件

| 事件名 | 触发时机 | 字段 |
|--------|---------|------|
| `blockcell.session_metrics.layer1.persisted` | 工具结果持久化 | `tool_use_id`, `original_size`, `preview_size`, `filepath`, `session_key`, `is_image`, `truncated` |
| `blockcell.session_metrics.layer1.budget_exceeded` | 消息预算超限 | `total_size`, `budget`, `candidates_count` |
| `blockcell.session_metrics.layer1.replacement_frozen` | 状态冻结决策 | `seen_ids_count`, `replacements_count` |
| `blockcell.session_metrics.layer1.preview_generated` | 预览生成完成 | `tool_use_id`, `original_size`, `preview_size`, `compression_ratio` |

### 5.3 Layer 2 事件

> **⚠️ 注意**: `history_projector.rs` 当前**没有任何 tracing 调用**，是唯一缺少监控的模块。
> 本节定义的事件需要在集成时**新增** tracing 日志。

| 事件名 | 触发时机 | 字段 |
|--------|---------|------|
| `blockcell.session_metrics.layer2.triggered` | 时间触发检查 | `gap_minutes`, `threshold_minutes` |
| `blockcell.session_metrics.layer2.cleared` | 内容清理 | `cleared_count`, `kept_count` |
| `blockcell.session_metrics.layer2.evaluated` | 时间间隔评估 | `time_since_last_tool_call`, `should_trigger` |

### 5.4 Layer 3 事件

| 事件名 | 触发时机 | 字段 |
|--------|---------|------|
| `blockcell.session_metrics.layer3.extraction_started` | Session Memory 提取开始 | `session_id`, `message_count`, `token_estimate` |
| `blockcell.session_metrics.layer3.extraction_completed` | 提取完成 | `input_tokens`, `output_tokens`, `cache_read_tokens`, `sections_updated` |
| `blockcell.session_metrics.layer3.loaded` | Session Memory 加载 | `content_length`, `line_count`, `sections_count` |

### 5.5 Layer 4 事件

| 事件名 | 触发时机 | 字段 |
|--------|---------|------|
| `blockcell.session_metrics.layer4.compact_started` | 压缩开始 | `pre_compact_tokens`, `threshold`, `is_auto` |
| `blockcell.session_metrics.layer4.compact_completed` | 压缩完成 | `pre_compact_tokens`, `post_compact_tokens`, `compression_ratio`, `recovery_tokens`, `cache_hit_rate` |
| `blockcell.session_metrics.layer4.compact_failed` | 压缩失败 | `reason`, `pre_compact_tokens`, `attempt`, `consecutive_failures` |
| `blockcell.session_metrics.layer4.ptl_retry` | Prompt Too Long 重试 | `attempt`, `dropped_messages`, `remaining_messages` |
| `blockcell.session_metrics.layer4.cache_break` | Prompt Cache 失效 | `system_prompt_changed`, `tools_changed`, `model_changed` |

### 5.6 Layer 5 事件

| 事件名 | 触发时机 | 字段 |
|--------|---------|------|
| `blockcell.session_metrics.layer5.extraction_started` | 记忆提取开始 | `session_id`, `turns_since_last` |
| `blockcell.session_metrics.layer5.memory_written` | 记忆写入 | `memory_type`, `filepath`, `content_length` |
| `blockcell.session_metrics.layer5.cursor_updated` | 游标更新 | `old_cursor`, `new_cursor`, `messages_processed` |
| `blockcell.session_metrics.layer5.injection_completed` | 记忆注入完成 | `user_memories`, `project_memories`, `feedback_memories`, `reference_memories` |

### 5.7 Layer 6 事件

| 事件名 | 触发时机 | 字段 |
|--------|---------|------|
| `blockcell.session_metrics.layer6.dream_started` | 梦境整合开始 | `sessions_count`, `hours_since_last` |
| `blockcell.session_metrics.layer6.phase_completed` | 阶段完成 | `phase` (orient/gather/consolidate/prune), `duration_ms` |
| `blockcell.session_metrics.layer6.dream_finished` | 整合完成 | `memories_created`, `memories_updated`, `memories_deleted`, `sessions_pruned` |
| `blockcell.session_metrics.layer6.gate_passed` | 门控检查通过 | `time_gate`, `session_gate`, `lock_gate` |

### 5.8 Layer 7 事件

| 事件名 | 触发时机 | 字段 |
|--------|---------|------|
| `blockcell.session_metrics.layer7.agent_spawned` | Forked Agent 创建 | `fork_label`, `max_turns`, `parent_agent_id` |
| `blockcell.session_metrics.layer7.agent_completed` | Forked Agent 完成 | `fork_label`, `turns_used`, `total_tokens`, `cache_hit_rate` |
| `blockcell.session_metrics.layer7.agent_failed` | Forked Agent 失败 | `fork_label`, `error`, `turns_used` |
| `blockcell.session_metrics.layer7.tool_denied` | 工具权限拒绝 | `tool_name`, `reason` |

---

## 六、Rust 实现

### 6.0 现有 metrics.rs 集成方案

> **⚠️ 重要说明**：本节描述的是**推荐的模块拆分方案**，而非现有结构。
>
> **现有结构**：
> - `ProcessingMetrics` 位于 `crates/agent/src/metrics.rs`
> - 文件约 90 行，包含 `ProcessingMetrics` 和 `ScopedTimer`
>
> **实现时应**：
> - 创建 `crates/agent/src/session_metrics/` 目录
> - 将现有 `metrics.rs` 移动到 `session_metrics/mod.rs`
> - 新增 `memory.rs`、`circuit_breaker.rs`、`summary.rs`
>
> **📋 现有文件**: `crates/agent/src/metrics.rs` 已包含 `ProcessingMetrics` 结构体，
> 用于追踪单次消息处理的时序指标（LLM 调用、工具执行、压缩次数等）。

**集成策略**: 扩展现有 `metrics.rs`，而非创建独立的 `memory_metrics.rs`。

```rust
// crates/agent/src/session_metrics/mod.rs (扩展后)

use std::time::Instant;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::info;

/// 全局记忆系统指标 (新增)
/// 使用 AtomicU64 实现无锁并发访问
#[derive(Debug, Default)]
pub struct MemoryMetrics {
    pub layer1: Layer1Metrics,
    pub layer2: Layer2Metrics,
    pub layer3: Layer3Metrics,
    pub layer4: Layer4Metrics,
    pub layer5: Layer5Metrics,
    pub layer6: Layer6Metrics,
    pub layer7: Layer7Metrics,
    pub circuit_breaker_state: AtomicU64,
    pub circuit_breaker_failures: AtomicU64,
}

// 全局实例 - 使用 std::sync::OnceLock (Rust 1.70+)
pub static MEMORY_METRICS: std::sync::OnceLock<MemoryMetrics> = std::sync::OnceLock::new();

fn get_memory_metrics() -> &'static MemoryMetrics {
    MEMORY_METRICS.get_or_init(MemoryMetrics::default)
}

/// Tracks timing for various stages of message processing.
/// (现有结构体，保持不变)
#[derive(Debug)]
pub(crate) struct ProcessingMetrics {
    start: Instant,
    decision_duration_ms: Option<u64>,
    llm_calls: Vec<u64>,
    tool_executions: Vec<(String, u64)>,
    compression_count: u32,
    finalized: bool,
    // 新增: 关联记忆指标引用
    memory_metrics: Option<&'static MemoryMetrics>,
}

impl ProcessingMetrics {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            decision_duration_ms: None,
            llm_calls: Vec::new(),
            tool_executions: Vec::new(),
            compression_count: 0,
            finalized: false,
            memory_metrics: Some(get_memory_metrics()),
        }
    }

    // 现有方法保持不变...

    /// 记录压缩事件 (扩展: 同时更新全局记忆指标)
    pub fn record_compression(&mut self) {
        self.compression_count += 1;
        // 同时更新全局 Layer 4 指标
        if let Some(m) = self.memory_metrics {
            m.layer4.compact_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Log a summary of all collected metrics. (扩展: 包含记忆指标摘要)
    pub fn log_summary(&mut self) {
        if self.finalized {
            return;
        }
        self.finalized = true;

        // 现有日志...
        let total_ms = self.total_elapsed_ms();
        // ...

        // 新增: 记忆指标摘要
        if let Some(m) = self.memory_metrics {
            let compact_count = m.layer4.compact_count.load(Ordering::Relaxed);
            let cache_hit_rate = m.layer4.cache_hit_rate();
            info!(
                compact_count,
                cache_hit_rate = format!("{:.1}%", cache_hit_rate * 100.0),
                "📊 Memory metrics snapshot"
            );
        }
    }
}
```

**设计原则**:
1. **复用现有结构**: `ProcessingMetrics` 继续追踪单次处理的时序指标
2. **全局累积指标**: `MemoryMetrics` 追踪跨会话的累积统计数据
3. **低开销**: 使用 `AtomicU64` 实现无锁并发，适合高频更新
4. **生命周期分离**: `ProcessingMetrics` 是 RAII 计时器，`MemoryMetrics` 是全局持久状态

**推荐文件组织** (基于代码审查建议):

由于新增代码较多 (~600 行)，建议拆分为子模块以提高可维护性：

```text
crates/agent/src/
├── session_metrics/
│   ├── mod.rs              # ProcessingMetrics (现有) + 导出
│   ├── memory.rs           # MemoryMetrics + Layer*Metrics
│   ├── circuit_breaker.rs  # CircuitBreaker 实现
│   └── summary.rs          # MetricsSummary + CLI 输出函数
└── lib.rs                  # pub mod session_metrics;
```

**优点**:
- 模块边界清晰，职责分离
- 便于单独测试和维护
- 避免单个文件过大

> **注意**: 以下示例代码假设使用上述目录结构。如选择直接扩展 `metrics.rs`，
> 请将 `session_metrics::` 替换为 `metrics::`。

### 6.1 核心数据结构

```rust
// crates/agent/src/session_metrics/memory.rs

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 记忆系统监控指标
#[derive(Debug, Default)]
pub struct MemoryMetrics {
    // Layer 1 指标
    pub layer1: Layer1Metrics,
    // Layer 2 指标
    pub layer2: Layer2Metrics,
    // Layer 3 指标
    pub layer3: Layer3Metrics,
    // Layer 4 指标
    pub layer4: Layer4Metrics,
    // Layer 5 指标
    pub layer5: Layer5Metrics,
    // Layer 6 指标
    pub layer6: Layer6Metrics,
    // Layer 7 指标
    pub layer7: Layer7Metrics,
    /// 熔断器状态 (0=关闭, 1=开启, 2=半开)
    pub circuit_breaker_state: AtomicU64,
    /// 熔断器连续失败计数
    pub circuit_breaker_failures: AtomicU64,
}

/// Layer 1: 工具结果存储指标
#[derive(Debug, Default)]
pub struct Layer1Metrics {
    /// 持久化的工具结果数量
    pub persisted_count: AtomicU64,
    /// 总原始大小 (bytes)
    pub total_original_size: AtomicU64,
    /// 总预览大小 (bytes)
    pub total_preview_size: AtomicU64,
    /// 预算超限次数
    pub budget_exceeded_count: AtomicU64,
    /// 当前 seen_ids 数量
    pub seen_ids_count: AtomicU64,
    /// 当前 replacements 数量
    pub replacements_count: AtomicU64,
}

/// Layer 2: Micro Compact 指标
#[derive(Debug, Default)]
pub struct Layer2Metrics {
    /// 时间触发清理次数
    pub trigger_count: AtomicU64,
    /// 清理的内容块数量
    pub cleared_count: AtomicU64,
    /// 保留的内容块数量
    pub kept_count: AtomicU64,
}

/// Layer 3: Session Memory 指标
#[derive(Debug, Default)]
pub struct Layer3Metrics {
    /// 提取次数
    pub extraction_count: AtomicU64,
    /// 加载次数
    pub load_count: AtomicU64,
    /// 总 Token 估计
    pub total_token_estimate: AtomicU64,
    /// 当前 Session Memory 大小 (bytes)
    pub current_size: AtomicU64,
}

/// Layer 4: 压缩指标
#[derive(Debug, Default)]
pub struct Layer4Metrics {
    /// 压缩总次数
    pub compact_count: AtomicU64,
    /// 自动压缩次数
    pub auto_compact_count: AtomicU64,
    /// 手动压缩次数
    pub manual_compact_count: AtomicU64,
    /// 压缩失败次数
    pub compact_failed_count: AtomicU64,
    /// 连续失败次数
    pub consecutive_failures: AtomicU64,
    /// 总压缩前 Token
    pub total_pre_compact_tokens: AtomicU64,
    /// 总压缩后 Token
    pub total_post_compact_tokens: AtomicU64,
    /// 总缓存读取 Token
    pub total_cache_read_tokens: AtomicU64,
    /// 总缓存创建 Token
    pub total_cache_creation_tokens: AtomicU64,
}

impl Layer4Metrics {
    /// 计算平均压缩率
    pub fn average_compression_ratio(&self) -> f64 {
        let pre = self.total_pre_compact_tokens.load(Ordering::Relaxed);
        let post = self.total_post_compact_tokens.load(Ordering::Relaxed);
        if pre == 0 {
            return 0.0;
        }
        1.0 - (post as f64 / pre as f64)
    }

    /// 计算缓存命中率
    pub fn cache_hit_rate(&self) -> f64 {
        let read = self.total_cache_read_tokens.load(Ordering::Relaxed);
        let creation = self.total_cache_creation_tokens.load(Ordering::Relaxed);
        let total = read + creation;
        if total == 0 {
            return 0.0;
        }
        read as f64 / total as f64
    }
}

/// Layer 5: 记忆提取指标
#[derive(Debug, Default)]
pub struct Layer5Metrics {
    /// 提取次数
    pub extraction_count: AtomicU64,
    /// 用户记忆数量
    pub user_memories: AtomicU64,
    /// 项目记忆数量
    pub project_memories: AtomicU64,
    /// 反馈记忆数量
    pub feedback_memories: AtomicU64,
    /// 引用记忆数量
    pub reference_memories: AtomicU64,
    /// 总写入字节数
    pub total_bytes_written: AtomicU64,
    /// 游标推进次数
    pub cursor_advances: AtomicU64,
}

/// Layer 6: Auto Dream 指标
#[derive(Debug, Default)]
pub struct Layer6Metrics {
    /// 梦境整合运行次数
    pub dream_count: AtomicU64,
    /// 创建的记忆数量
    pub memories_created: AtomicU64,
    /// 更新的记忆数量
    pub memories_updated: AtomicU64,
    /// 删除的记忆数量
    pub memories_deleted: AtomicU64,
    /// 清理的会话数量
    pub sessions_pruned: AtomicU64,
    /// 上次运行时间戳
    pub last_run_timestamp: AtomicU64,
}

/// Layer 7: Forked Agent 指标
#[derive(Debug, Default)]
pub struct Layer7Metrics {
    /// Forked Agent 创建次数
    pub spawned_count: AtomicU64,
    /// 成功完成次数
    pub completed_count: AtomicU64,
    /// 失败次数
    pub failed_count: AtomicU64,
    /// 工具权限拒绝次数
    pub tool_denied_count: AtomicU64,
    /// 总使用 Token
    pub total_tokens_used: AtomicU64,
    /// 总使用轮次
    pub total_turns_used: AtomicU64,
}

/// 全局指标实例
/// 使用 std::sync::OnceLock 替代 lazy_static (Rust 1.70+)
pub static MEMORY_METRICS: std::sync::OnceLock<MemoryMetrics> = std::sync::OnceLock::new();

/// 获取全局指标实例
pub fn get_memory_metrics() -> &'static MemoryMetrics {
    MEMORY_METRICS.get_or_init(MemoryMetrics::default)
}
```

### 6.2 事件记录宏

```rust
// crates/agent/src/session_metrics/mod.rs (继续添加)

/// 记录记忆系统事件的宏
#[macro_export]
macro_rules! memory_event {
    // Layer 1 事件
    (layer1, persisted, $tool_use_id:expr, $original_size:expr, $preview_size:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer1",
            event = "persisted",
            tool_use_id = %$tool_use_id,
            original_size = $original_size,
            preview_size = $preview_size,
            "Tool result persisted to disk"
        );
        $crate::session_metrics::MEMORY_METRICS.layer1.persisted_count.fetch_add(1, Ordering::Relaxed);
        $crate::session_metrics::MEMORY_METRICS.layer1.total_original_size.fetch_add($original_size as u64, Ordering::Relaxed);
        $crate::session_metrics::MEMORY_METRICS.layer1.total_preview_size.fetch_add($preview_size as u64, Ordering::Relaxed);
    };

    // Layer 2 事件
    (layer2, triggered, $gap_minutes:expr, $threshold_minutes:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer2",
            event = "triggered",
            gap_minutes = $gap_minutes,
            threshold_minutes = $threshold_minutes,
            "Micro compact time check triggered"
        );
        $crate::session_metrics::MEMORY_METRICS.layer2.trigger_count.fetch_add(1, Ordering::Relaxed);
    };

    (layer2, cleared, $cleared:expr, $kept:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer2",
            event = "cleared",
            cleared_count = $cleared,
            kept_count = $kept,
            "Micro compact content cleared"
        );
        $crate::session_metrics::MEMORY_METRICS.layer2.cleared_count.fetch_add($cleared as u64, Ordering::Relaxed);
        $crate::session_metrics::MEMORY_METRICS.layer2.kept_count.fetch_add($kept as u64, Ordering::Relaxed);
    };

    // Layer 3 事件
    (layer3, extraction_started, $session_id:expr, $message_count:expr, $token_estimate:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer3",
            event = "extraction_started",
            session_id = %$session_id,
            message_count = $message_count,
            token_estimate = $token_estimate,
            "Session memory extraction started"
        );
        $crate::session_metrics::MEMORY_METRICS.layer3.extraction_count.fetch_add(1, Ordering::Relaxed);
    };

    (layer3, loaded, $content_length:expr, $line_count:expr, $sections_count:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer3",
            event = "loaded",
            content_length = $content_length,
            line_count = $line_count,
            sections_count = $sections_count,
            "Session memory loaded"
        );
        $crate::session_metrics::MEMORY_METRICS.layer3.load_count.fetch_add(1, Ordering::Relaxed);
        $crate::session_metrics::MEMORY_METRICS.layer3.current_size.store($content_length as u64, Ordering::Relaxed);
    };

    // Layer 4 事件
    (layer4, compact_started, $pre_tokens:expr, $threshold:expr, $is_auto:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer4",
            event = "compact_started",
            pre_compact_tokens = $pre_tokens,
            threshold = $threshold,
            is_auto = $is_auto,
            "Compact started"
        );
    };

    (layer4, compact_completed, $pre:expr, $post:expr, $cache_read:expr, $cache_creation:expr) => {
        let ratio = if $pre > 0 { 1.0 - ($post as f64 / $pre as f64) } else { 0.0 };
        let hit_rate = if $cache_read + $cache_creation > 0 {
            $cache_read as f64 / ($cache_read + $cache_creation) as f64
        } else { 0.0 };

        tracing::info!(
            target: "blockcell.session_metrics.layer4",
            event = "compact_completed",
            pre_compact_tokens = $pre,
            post_compact_tokens = $post,
            compression_ratio = format!("{:.2}%", ratio * 100.0),
            cache_read_tokens = $cache_read,
            cache_creation_tokens = $cache_creation,
            cache_hit_rate = format!("{:.2}%", hit_rate * 100.0),
            "Compact completed successfully"
        );

        let metrics = &$crate::session_metrics::MEMORY_METRICS.layer4;
        metrics.compact_count.fetch_add(1, Ordering::Relaxed);
        metrics.total_pre_compact_tokens.fetch_add($pre as u64, Ordering::Relaxed);
        metrics.total_post_compact_tokens.fetch_add($post as u64, Ordering::Relaxed);
        metrics.total_cache_read_tokens.fetch_add($cache_read as u64, Ordering::Relaxed);
        metrics.total_cache_creation_tokens.fetch_add($cache_creation as u64, Ordering::Relaxed);
        metrics.consecutive_failures.store(0, Ordering::Relaxed);
    };

    (layer4, compact_failed, $reason:expr, $pre_tokens:expr, $attempt:expr) => {
        tracing::warn!(
            target: "blockcell.session_metrics.layer4",
            event = "compact_failed",
            reason = $reason,
            pre_compact_tokens = $pre_tokens,
            attempt = $attempt,
            "Compact failed"
        );

        let metrics = &$crate::session_metrics::MEMORY_METRICS.layer4;
        metrics.compact_failed_count.fetch_add(1, Ordering::Relaxed);
        let failures = metrics.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;

        // 熔断检查
        const MAX_CONSECUTIVE_FAILURES: u64 = 3;
        if failures >= MAX_CONSECUTIVE_FAILURES {
            tracing::error!(
                target: "blockcell.session_metrics.layer4",
                consecutive_failures = failures,
                "Compact circuit breaker tripped - skipping future attempts"
            );
        }
    };

    // Layer 5 事件
    (layer5, memory_written, $memory_type:expr, $filepath:expr, $content_len:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer5",
            event = "memory_written",
            memory_type = $memory_type,
            filepath = %$filepath,
            content_length = $content_len,
            "Memory written to file"
        );
        $crate::session_metrics::MEMORY_METRICS.layer5.extraction_count.fetch_add(1, Ordering::Relaxed);
        $crate::session_metrics::MEMORY_METRICS.layer5.total_bytes_written.fetch_add($content_len as u64, Ordering::Relaxed);
    };

    // Layer 6 事件
    (layer6, dream_started, $sessions_count:expr, $hours_since_last:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer6",
            event = "dream_started",
            sessions_count = $sessions_count,
            hours_since_last = $hours_since_last,
            "Dream consolidation started"
        );
        $crate::session_metrics::MEMORY_METRICS.layer6.dream_count.fetch_add(1, Ordering::Relaxed);
    };

    (layer6, dream_finished, $created:expr, $updated:expr, $deleted:expr, $pruned:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer6",
            event = "dream_finished",
            memories_created = $created,
            memories_updated = $updated,
            memories_deleted = $deleted,
            sessions_pruned = $pruned,
            "Dream consolidation completed"
        );
        let metrics = &$crate::session_metrics::MEMORY_METRICS.layer6;
        metrics.memories_created.fetch_add($created as u64, Ordering::Relaxed);
        metrics.memories_updated.fetch_add($updated as u64, Ordering::Relaxed);
        metrics.memories_deleted.fetch_add($deleted as u64, Ordering::Relaxed);
        metrics.sessions_pruned.fetch_add($pruned as u64, Ordering::Relaxed);
        metrics.last_run_timestamp.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            Ordering::Relaxed
        );
    };

    // Layer 7 事件
    (layer7, agent_spawned, $fork_label:expr, $max_turns:expr, $parent_id:expr) => {
        tracing::debug!(
            target: "blockcell.session_metrics.layer7",
            event = "agent_spawned",
            fork_label = $fork_label,
            max_turns = $max_turns,
            parent_agent_id = %$parent_id,
            "Forked agent spawned"
        );
        $crate::session_metrics::MEMORY_METRICS.layer7.spawned_count.fetch_add(1, Ordering::Relaxed);
    };

    (layer7, agent_completed, $fork_label:expr, $turns:expr, $tokens:expr, $hit_rate:expr) => {
        tracing::info!(
            target: "blockcell.session_metrics.layer7",
            event = "agent_completed",
            fork_label = $fork_label,
            turns_used = $turns,
            total_tokens = $tokens,
            cache_hit_rate = format!("{:.2}%", $hit_rate * 100.0),
            "Forked agent completed"
        );
        let metrics = &$crate::session_metrics::MEMORY_METRICS.layer7;
        metrics.completed_count.fetch_add(1, Ordering::Relaxed);
        metrics.total_turns_used.fetch_add($turns as u64, Ordering::Relaxed);
        metrics.total_tokens_used.fetch_add($tokens as u64, Ordering::Relaxed);
    };

    (layer7, agent_failed, $fork_label:expr, $error:expr, $turns:expr) => {
        tracing::warn!(
            target: "blockcell.session_metrics.layer7",
            event = "agent_failed",
            fork_label = $fork_label,
            error = $error,
            turns_used = $turns,
            "Forked agent failed"
        );
        $crate::session_metrics::MEMORY_METRICS.layer7.failed_count.fetch_add(1, Ordering::Relaxed);
    };

    (layer7, tool_denied, $tool_name:expr, $reason:expr) => {
        tracing::warn!(
            target: "blockcell.session_metrics.layer7",
            event = "tool_denied",
            tool_name = $tool_name,
            reason = $reason,
            "Tool permission denied"
        );
        $crate::session_metrics::MEMORY_METRICS.layer7.tool_denied_count.fetch_add(1, Ordering::Relaxed);
    };
}
```

### 6.3 获取监控摘要

```rust
// crates/agent/src/memory_metrics.rs

/// 获取监控摘要 (供 /session_metrics 命令使用)
pub fn get_metrics_summary() -> MetricsSummary {
    let m = &*MEMORY_METRICS;

    MetricsSummary {
        layer1: Layer1Summary {
            persisted_count: m.layer1.persisted_count.load(Ordering::Relaxed),
            total_original_size: m.layer1.total_original_size.load(Ordering::Relaxed),
            total_preview_size: m.layer1.total_preview_size.load(Ordering::Relaxed),
            budget_exceeded_count: m.layer1.budget_exceeded_count.load(Ordering::Relaxed),
            average_compression: {
                let orig = m.layer1.total_original_size.load(Ordering::Relaxed);
                let prev = m.layer1.total_preview_size.load(Ordering::Relaxed);
                if orig > 0 { 1.0 - (prev as f64 / orig as f64) } else { 0.0 }
            },
        },
        layer2: Layer2Summary {
            trigger_count: m.layer2.trigger_count.load(Ordering::Relaxed),
            cleared_count: m.layer2.cleared_count.load(Ordering::Relaxed),
            kept_count: m.layer2.kept_count.load(Ordering::Relaxed),
        },
        layer3: Layer3Summary {
            extraction_count: m.layer3.extraction_count.load(Ordering::Relaxed),
            load_count: m.layer3.load_count.load(Ordering::Relaxed),
            current_size: m.layer3.current_size.load(Ordering::Relaxed),
        },
        layer4: Layer4Summary {
            compact_count: m.layer4.compact_count.load(Ordering::Relaxed),
            auto_compact_count: m.layer4.auto_compact_count.load(Ordering::Relaxed),
            manual_compact_count: m.layer4.manual_compact_count.load(Ordering::Relaxed),
            failed_count: m.layer4.compact_failed_count.load(Ordering::Relaxed),
            average_compression_ratio: {
                let pre = m.layer4.total_pre_compact_tokens.load(Ordering::Relaxed);
                let post = m.layer4.total_post_compact_tokens.load(Ordering::Relaxed);
                if pre > 0 { 1.0 - (post as f64 / pre as f64) } else { 0.0 }
            },
            cache_hit_rate: {
                let read = m.layer4.total_cache_read_tokens.load(Ordering::Relaxed);
                let creation = m.layer4.total_cache_creation_tokens.load(Ordering::Relaxed);
                let total = read + creation;
                if total > 0 { read as f64 / total as f64 } else { 0.0 }
            },
        },
        layer5: Layer5Summary {
            extraction_count: m.layer5.extraction_count.load(Ordering::Relaxed),
            user_memories: m.layer5.user_memories.load(Ordering::Relaxed),
            project_memories: m.layer5.project_memories.load(Ordering::Relaxed),
            feedback_memories: m.layer5.feedback_memories.load(Ordering::Relaxed),
            reference_memories: m.layer5.reference_memories.load(Ordering::Relaxed),
            total_bytes_written: m.layer5.total_bytes_written.load(Ordering::Relaxed),
        },
        layer6: Layer6Summary {
            dream_count: m.layer6.dream_count.load(Ordering::Relaxed),
            memories_created: m.layer6.memories_created.load(Ordering::Relaxed),
            memories_updated: m.layer6.memories_updated.load(Ordering::Relaxed),
            memories_deleted: m.layer6.memories_deleted.load(Ordering::Relaxed),
            sessions_pruned: m.layer6.sessions_pruned.load(Ordering::Relaxed),
            last_run_timestamp: m.layer6.last_run_timestamp.load(Ordering::Relaxed),
        },
        layer7: Layer7Summary {
            spawned_count: m.layer7.spawned_count.load(Ordering::Relaxed),
            completed_count: m.layer7.completed_count.load(Ordering::Relaxed),
            failed_count: m.layer7.failed_count.load(Ordering::Relaxed),
            tool_denied_count: m.layer7.tool_denied_count.load(Ordering::Relaxed),
            total_tokens_used: m.layer7.total_tokens_used.load(Ordering::Relaxed),
            total_turns_used: m.layer7.total_turns_used.load(Ordering::Relaxed),
        },
        circuit_breaker_state: match m.circuit_breaker_state.load(Ordering::Relaxed) {
            0 => CircuitState::Closed,
            1 => CircuitState::Open,
            2 => CircuitState::HalfOpen,
            _ => CircuitState::Closed,
        },
        circuit_breaker_failures: m.circuit_breaker_failures.load(Ordering::Relaxed),
    }
}

/// 重置指标
pub fn reset_metrics() {
    let m = &*MEMORY_METRICS;
    // Layer 1
    m.layer1.persisted_count.store(0, Ordering::Relaxed);
    m.layer1.total_original_size.store(0, Ordering::Relaxed);
    m.layer1.total_preview_size.store(0, Ordering::Relaxed);
    m.layer1.budget_exceeded_count.store(0, Ordering::Relaxed);
    // Layer 2
    m.layer2.trigger_count.store(0, Ordering::Relaxed);
    m.layer2.cleared_count.store(0, Ordering::Relaxed);
    m.layer2.kept_count.store(0, Ordering::Relaxed);
    // Layer 3
    m.layer3.extraction_count.store(0, Ordering::Relaxed);
    m.layer3.load_count.store(0, Ordering::Relaxed);
    m.layer3.current_size.store(0, Ordering::Relaxed);
    // Layer 4
    m.layer4.compact_count.store(0, Ordering::Relaxed);
    m.layer4.auto_compact_count.store(0, Ordering::Relaxed);
    m.layer4.manual_compact_count.store(0, Ordering::Relaxed);
    m.layer4.compact_failed_count.store(0, Ordering::Relaxed);
    m.layer4.consecutive_failures.store(0, Ordering::Relaxed);
    // Layer 5
    m.layer5.extraction_count.store(0, Ordering::Relaxed);
    m.layer5.user_memories.store(0, Ordering::Relaxed);
    m.layer5.project_memories.store(0, Ordering::Relaxed);
    m.layer5.feedback_memories.store(0, Ordering::Relaxed);
    m.layer5.reference_memories.store(0, Ordering::Relaxed);
    m.layer5.total_bytes_written.store(0, Ordering::Relaxed);
    // Layer 6
    m.layer6.dream_count.store(0, Ordering::Relaxed);
    m.layer6.memories_created.store(0, Ordering::Relaxed);
    m.layer6.memories_updated.store(0, Ordering::Relaxed);
    m.layer6.memories_deleted.store(0, Ordering::Relaxed);
    m.layer6.sessions_pruned.store(0, Ordering::Relaxed);
    // Layer 7
    m.layer7.spawned_count.store(0, Ordering::Relaxed);
    m.layer7.completed_count.store(0, Ordering::Relaxed);
    m.layer7.failed_count.store(0, Ordering::Relaxed);
    m.layer7.tool_denied_count.store(0, Ordering::Relaxed);
    m.layer7.total_tokens_used.store(0, Ordering::Relaxed);
    m.layer7.total_turns_used.store(0, Ordering::Relaxed);
    // 熔断器
    m.circuit_breaker_state.store(0, Ordering::Relaxed);
    m.circuit_breaker_failures.store(0, Ordering::Relaxed);
}

/// 用于 CLI 输出的摘要结构
#[derive(Debug, serde::Serialize)]
pub struct MetricsSummary {
    pub layer1: Layer1Summary,
    pub layer2: Layer2Summary,
    pub layer3: Layer3Summary,
    pub layer4: Layer4Summary,
    pub layer5: Layer5Summary,
    pub layer6: Layer6Summary,
    pub layer7: Layer7Summary,
    /// 熔断器状态
    pub circuit_breaker_state: CircuitState,
    /// 熔断器连续失败次数
    pub circuit_breaker_failures: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct Layer1Summary {
    pub persisted_count: u64,
    pub total_original_size: u64,
    pub total_preview_size: u64,
    pub budget_exceeded_count: u64,
    pub average_compression: f64,
}

#[derive(Debug, serde::Serialize)]
pub struct Layer2Summary {
    pub trigger_count: u64,
    pub cleared_count: u64,
    pub kept_count: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct Layer3Summary {
    pub extraction_count: u64,
    pub load_count: u64,
    pub current_size: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct Layer4Summary {
    pub compact_count: u64,
    pub auto_compact_count: u64,
    pub manual_compact_count: u64,
    pub failed_count: u64,
    pub average_compression_ratio: f64,
    pub cache_hit_rate: f64,
}

#[derive(Debug, serde::Serialize)]
pub struct Layer5Summary {
    pub extraction_count: u64,
    pub user_memories: u64,
    pub project_memories: u64,
    pub feedback_memories: u64,
    pub reference_memories: u64,
    pub total_bytes_written: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct Layer6Summary {
    pub dream_count: u64,
    pub memories_created: u64,
    pub memories_updated: u64,
    pub memories_deleted: u64,
    pub sessions_pruned: u64,
    pub last_run_timestamp: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct Layer7Summary {
    pub spawned_count: u64,
    pub completed_count: u64,
    pub failed_count: u64,
    pub tool_denied_count: u64,
    pub total_tokens_used: u64,
    pub total_turns_used: u64,
}
```

### 6.4 熔断器实现

```rust
// crates/agent/src/session_metrics/circuit_breaker.rs

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 熔断器状态
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub enum CircuitState {
    /// 正常状态
    Closed,
    /// 开启状态（拒绝请求）
    Open,
    /// 半开状态（尝试恢复）
    HalfOpen,
}

/// 熔断器配置
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// 最大连续失败次数
    pub max_failures: u64,
    /// 开启后等待时间
    pub reset_timeout: Duration,
    /// 半开状态最大尝试次数
    pub half_open_max_calls: u64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            max_failures: 3,
            reset_timeout: Duration::from_secs(60),
            half_open_max_calls: 1,
        }
    }
}

/// 熔断器 (无锁实现)
///
/// 使用 AtomicU64 存储 Unix 纳秒时间戳，避免 Mutex 竞态。
/// 在高并发场景下性能更好。
pub struct CircuitBreaker {
    state: AtomicU8, // 0=Closed, 1=Open, 2=HalfOpen
    failure_count: AtomicU64,
    /// 最后失败时间 (Unix 纳秒时间戳)
    /// 使用 0 表示"无记录"
    last_failure_time_ns: AtomicU64,
    half_open_calls: AtomicU64,
    config: CircuitBreakerConfig,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: AtomicU8::new(0),
            failure_count: AtomicU64::new(0),
            last_failure_time_ns: AtomicU64::new(0),
            half_open_calls: AtomicU64::new(0),
            config,
        }
    }

    /// 获取当前 Unix 纳秒时间戳
    fn current_time_ns() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    }

    /// 检查是否允许执行
    pub fn allow(&self) -> bool {
        let state = self.state.load(Ordering::Relaxed);
        match state {
            0 => true, // Closed
            1 => {     // Open
                let last_ns = self.last_failure_time_ns.load(Ordering::Relaxed);
                if last_ns > 0 {
                    let now_ns = Self::current_time_ns();
                    let elapsed_ns = now_ns.saturating_sub(last_ns);
                    let timeout_ns = self.config.reset_timeout.as_nanos() as u64;

                    if elapsed_ns >= timeout_ns {
                        // 转换到半开状态
                        self.state.store(2, Ordering::Relaxed);
                        self.half_open_calls.store(0, Ordering::Relaxed);
                        return true;
                    }
                }
                false
            }
            2 => {     // HalfOpen
                let calls = self.half_open_calls.fetch_add(1, Ordering::Relaxed);
                calls < self.config.half_open_max_calls
            }
            _ => false,
        }
    }

    /// 记录成功
    pub fn record_success(&self) {
        let state = self.state.load(Ordering::Relaxed);
        if state == 2 {
            // 半开状态下成功，恢复到关闭状态
            self.state.store(0, Ordering::Relaxed);
            self.failure_count.store(0, Ordering::Relaxed);
            self.last_failure_time_ns.store(0, Ordering::Relaxed);
            tracing::info!(
                target: "blockcell.session_metrics.circuit_breaker",
                "Circuit breaker recovered to closed state"
            );
        }
    }

    /// 记录失败
    pub fn record_failure(&self) {
        let failures = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
        let state = self.state.load(Ordering::Relaxed);

        if state == 2 {
            // 半开状态下失败，回到开启状态
            self.state.store(1, Ordering::Relaxed);
            self.last_failure_time_ns.store(Self::current_time_ns(), Ordering::Relaxed);
            tracing::warn!(
                target: "blockcell.session_metrics.circuit_breaker",
                "Circuit breaker returned to open state after half-open failure"
            );
        } else if failures >= self.config.max_failures {
            // 达到阈值，进入开启状态
            self.state.store(1, Ordering::Relaxed);
            self.last_failure_time_ns.store(Self::current_time_ns(), Ordering::Relaxed);
            tracing::error!(
                target: "blockcell.session_metrics.circuit_breaker",
                failures = failures,
                max_failures = self.config.max_failures,
                "Circuit breaker tripped to open state"
            );
        }
    }

    /// 获取当前状态
    pub fn state(&self) -> CircuitState {
        match self.state.load(Ordering::Relaxed) {
            0 => CircuitState::Closed,
            1 => CircuitState::Open,
            2 => CircuitState::HalfOpen,
            _ => CircuitState::Closed,
        }
    }
}

// ============================================================================
// 全局熔断器实例 - 每层一个独立熔断器
// ============================================================================

/// Layer 4: Compact 熔断器 (快速恢复)
/// 用户同步操作，需要快速响应
pub static COMPACT_CIRCUIT_BREAKER: std::sync::OnceLock<CircuitBreaker> =
    std::sync::OnceLock::new();

/// Layer 5: Memory Extraction 熔断器 (中等恢复)
/// 后台异步操作，可接受较长恢复时间
pub static MEMORY_EXTRACTION_CIRCUIT_BREAKER: std::sync::OnceLock<CircuitBreaker> =
    std::sync::OnceLock::new();

/// Layer 6: Dream Consolidation 熔断器 (慢恢复)
/// 定时后台任务，最长冷却期
pub static DREAM_CIRCUIT_BREAKER: std::sync::OnceLock<CircuitBreaker> =
    std::sync::OnceLock::new();

// ============================================================================
// 熔断器获取函数
// ============================================================================

/// 获取 Layer 4 Compact 熔断器
/// 配置: max_failures=3, reset_timeout=60s
pub fn get_compact_circuit_breaker() -> &'static CircuitBreaker {
    COMPACT_CIRCUIT_BREAKER.get_or_init(|| {
        CircuitBreaker::new(CircuitBreakerConfig {
            max_failures: 3,
            reset_timeout: Duration::from_secs(60),
            half_open_max_calls: 1,
        })
    })
}

/// 获取 Layer 5 Memory Extraction 熔断器
/// 配置: max_failures=3, reset_timeout=300s (5分钟)
pub fn get_memory_extraction_circuit_breaker() -> &'static CircuitBreaker {
    MEMORY_EXTRACTION_CIRCUIT_BREAKER.get_or_init(|| {
        CircuitBreaker::new(CircuitBreakerConfig {
            max_failures: 3,
            reset_timeout: Duration::from_secs(300),
            half_open_max_calls: 1,
        })
    })
}

/// 获取 Layer 6 Dream Consolidation 熔断器
/// 配置: max_failures=2, reset_timeout=900s (15分钟)
pub fn get_dream_circuit_breaker() -> &'static CircuitBreaker {
    DREAM_CIRCUIT_BREAKER.get_or_init(|| {
        CircuitBreaker::new(CircuitBreakerConfig {
            max_failures: 2,
            reset_timeout: Duration::from_secs(900),
            half_open_max_calls: 1,
        })
    })
}
```

---

## 七、集成方案

### 7.0 集成概述

**关键集成点**:

| 模块 | 文件 | 集成内容 |
|------|------|---------|
| Layer 4 Compact | `crates/agent/src/runtime.rs` | 熔断器检查、事件记录、用户通知 |
| Layer 5 Memory Extraction | `crates/agent/src/auto_memory/mod.rs` | 熔断器检查、事件记录 |
| Layer 6 Dream Consolidation | `crates/scheduler/src/consolidator.rs` | 熔断器检查、事件记录 |
| Layer 1 Storage | `crates/agent/src/response_cache.rs` | 事件记录 |
| Layer 2 MicroCompact | `crates/agent/src/history_projector.rs` | 新增 tracing |
| 全局指标 | `crates/agent/src/session_metrics/mod.rs` | MemoryMetrics 扩展 |

### 7.1 Layer 4 Compact 熔断器集成

> **📋 背景**: Layer 4 Full Compact 在 `runtime.rs` 的 `process_message()` 循环中被触发。
> 现有实现使用 `compact/hooks.rs` 中的 `PreCompactHook` 和 `PostCompactHook` 进行扩展。

**集成时机流程图**:

```
process_message() 循环
       │
       ▼
┌─────────────────────────────────────┐
│ 1. Token 估算超过阈值?              │
│    estimate_tokens > threshold?     │
└──────────────┬──────────────────────┘
               │ 是
               ▼
┌─────────────────────────────────────┐
│ 2. 熔断器检查                       │
│    circuit_breaker.allow()?         │
│    ├─ Closed → 允许执行             │
│    ├─ Open → 拒绝，返回错误         │
│    └─ HalfOpen → 尝试一次           │
└──────────────┬──────────────────────┘
               │ 允许
               ▼
┌─────────────────────────────────────┐
│ 3. 区分自动/手动压缩                │
│    is_auto = !user_triggered        │
└──────────────┬──────────────────────┘
               │
               ▼
┌─────────────────────────────────────┐
│ 4. 执行 PreCompactHook              │
│    (现有 hooks.rs)                  │
└──────────────┬──────────────────────┘
               │
               ▼
┌─────────────────────────────────────┐
│ 5. 执行压缩                         │
│    generate_compact_summary()       │
└──────────────┬──────────────────────┘
               │
       ┌───────┴───────┐
       │               │
    成功             失败
       │               │
       ▼               ▼
┌──────────────┐ ┌──────────────┐
│ 记录成功     │ │ 记录失败     │
│ circuit_breaker.record_success()│
│ metrics.layer4.compact_count++  │
└──────┬───────┘ │ circuit_breaker.record_failure()│
       │         │ metrics.layer4.compact_failed_count++│
       ▼         └──────┬───────┘
┌─────────────────────────────────────┐
│ 6. 执行 PostCompactHook             │
│    (现有 hooks.rs)                  │
└─────────────────────────────────────┘
```

**自动压缩 vs 手动压缩区分**:

```rust
// crates/agent/src/runtime.rs

/// 压缩触发来源
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CompactTrigger {
    /// 自动触发 (Token 阈值超限)
    Auto,
    /// 手动触发 (用户请求或 /compact 命令)
    Manual,
}

impl AgentRuntime {
    /// 检查是否需要压缩并执行
    async fn maybe_compact(
        &mut self,
        messages: &[ChatMessage],
        trigger: CompactTrigger,
        ctx: Option<CompactContext<'_>>,
    ) -> Option<CompactResult> {
        use crate::compact::CompactConfig;

        // 1. 熔断器检查
        let circuit_breaker = get_compact_circuit_breaker();
        if !circuit_breaker.allow() {
            tracing::warn!(
                target: "blockcell.session_metrics.layer4",
                trigger = ?trigger,
                "Compact skipped due to circuit breaker"
            );
            return None;
        }

        // 2. 记录开始事件 (区分自动/手动)
        let pre_tokens = estimate_messages_tokens(messages);
        memory_event!(layer4, compact_started, pre_tokens,
            self.config.memory.compact_threshold, trigger == CompactTrigger::Auto);

        // 3. 执行压缩 (调用现有逻辑)
        let result = self.execute_compact_internal(messages).await;

        // 4. 记录结果
        match &result {
            Some(r) if r.success => {
                circuit_breaker.record_success();
                get_memory_metrics().layer4.record_success(
                    r.pre_compact_tokens,
                    r.post_compact_tokens,
                    trigger == CompactTrigger::Auto,
                );
            }
            Some(r) if !r.success => {
                circuit_breaker.record_failure();
                get_memory_metrics().layer4.record_failure();
            }
            None => {}
        }

        result
    }
}
```

**与现有 hooks.rs 集成**:

```rust
// crates/agent/src/compact/hooks.rs (扩展示例)

use crate::metrics::get_memory_metrics;

/// Post-Compact 钩子 - 在压缩完成后更新指标
pub struct MetricsUpdateHook;

impl PostCompactHook for MetricsUpdateHook {
    fn execute(&self, result: &CompactResult) {
        let metrics = get_memory_metrics();

        // 更新 Layer 4 指标
        metrics.layer4.compact_count.fetch_add(1, Ordering::Relaxed);

        if result.success {
            metrics.layer4.total_pre_compact_tokens.fetch_add(
                result.pre_compact_tokens as u64,
                Ordering::Relaxed
            );
            metrics.layer4.total_post_compact_tokens.fetch_add(
                result.post_compact_tokens as u64,
                Ordering::Relaxed
            );
        } else {
            metrics.layer4.compact_failed_count.fetch_add(1, Ordering::Relaxed);
        }
    }
}
```

### 7.2 集成到 runtime.rs

```rust
// crates/agent/src/runtime.rs (修改示例)

impl AgentRuntime {
    /// Execute Layer 4 Full Compact
    async fn execute_layer4_compact(
        &self,
        messages: &[ChatMessage],
        _session_key: &str,
    ) -> crate::compact::CompactResult {
        use crate::memory_metrics::{COMPACT_CIRCUIT_BREAKER, memory_event};

        // 熔断检查
        if !COMPACT_CIRCUIT_BREAKER.allow() {
            tracing::warn!(
                target: "blockcell.session_metrics.layer4",
                "Compact skipped due to circuit breaker"
            );
            return CompactResult::failed("Circuit breaker open - too many recent failures");
        }

        let pre_compact_tokens = estimate_messages_tokens(messages);
        let is_auto = true; // 根据调用上下文判断

        memory_event!(layer4, compact_started, pre_compact_tokens, 
            self.memory_system.as_ref().map(|m| m.config().compact_threshold).unwrap_or(0), 
            is_auto
        );

        let start_time = std::time::Instant::now();

        // ... 执行压缩逻辑 ...

        match result {
            Ok(summary) => {
                COMPACT_CIRCUIT_BREAKER.record_success();

                let duration = start_time.elapsed();
                memory_event!(layer4, compact_completed, 
                    pre_compact_tokens,
                    post_compact_tokens,
                    usage.cache_readInputTokens,
                    usage.cacheCreationInputTokens
                );

                CompactResult { success: true, ... }
            }
            Err(e) => {
                COMPACT_CIRCUIT_BREAKER.record_failure();
                memory_event!(layer4, compact_failed, &e.to_string(), pre_compact_tokens, 1);
                CompactResult::failed(&e.to_string())
            }
        }
    }
}
```

### 7.3 集成到 response_cache.rs

```rust
// crates/agent/src/response_cache.rs (修改示例)

pub async fn persist_tool_result(
    content: &str,
    tool_use_id: &str,
    session_key: &str,
    workspace_dir: &std::path::Path,
) -> Result<PersistedToolResult, PersistToolResultError> {
    // ... 现有逻辑 ...

    // 成功后记录事件
    let preview_size = result.preview.len();
    let original_size = content.len();

    crate::memory_event!(
        layer1, persisted, 
        tool_use_id, 
        original_size, 
        preview_size
    );

    Ok(result)
}
```

### 7.4 Layer 5 Memory Extraction 熔断器集成

```rust
// crates/agent/src/auto_memory/mod.rs (修改示例)

use crate::session_metrics::{get_memory_extraction_circuit_breaker, memory_event};

impl AutoMemory {
    /// 执行记忆提取
    pub async fn extract_memories(&mut self) -> Result<(), AutoMemoryError> {
        // 1. 熔断器检查
        let circuit_breaker = get_memory_extraction_circuit_breaker();
        if !circuit_breaker.allow() {
            tracing::warn!(
                target: "blockcell.session_metrics.layer5",
                "Memory extraction skipped - circuit breaker OPEN"
            );
            // 不阻塞主流程，静默跳过
            return Ok(());
        }

        // 2. 执行提取逻辑
        let result = self.do_extraction().await;

        // 3. 记录结果
        match result {
            Ok(stats) => {
                circuit_breaker.record_success();
                memory_event!(layer5, memory_written, stats.memory_type, stats.filepath, stats.content_len);
                Ok(())
            }
            Err(e) => {
                circuit_breaker.record_failure();
                tracing::error!(error = %e, "Memory extraction failed");
                // 失败也不阻塞，返回 Ok
                Ok(())
            }
        }
    }
}
```

**关键设计点**:
- 失败不阻塞主对话流程
- 用户无感知，静默跳过
- 熔断后继续正常对话，只是记忆不更新

### 7.5 Layer 6 Dream Consolidation 熔断器集成

```rust
// crates/scheduler/src/consolidator.rs (修改示例)

use blockcell_agent::session_metrics::{get_dream_circuit_breaker, memory_event};

impl Consolidator {
    /// 执行梦境整合
    pub async fn run_consolidation(&mut self) -> Result<(), ConsolidationError> {
        // 1. 门控检查 (现有逻辑)
        if !self.check_gates() {
            return Ok(());
        }

        // 2. 熔断器检查
        let circuit_breaker = get_dream_circuit_breaker();
        if !circuit_breaker.allow() {
            tracing::warn!(
                target: "blockcell.session_metrics.layer6",
                "Dream consolidation skipped - circuit breaker OPEN"
            );
            // 静默跳过，等待下一个周期
            return Ok(());
        }

        // 3. 记录开始
        let sessions_count = self.pending_sessions.len();
        memory_event!(layer6, dream_started, sessions_count, self.hours_since_last());

        // 4. 执行整合
        let result = self.do_consolidation().await;

        // 5. 记录结果
        match result {
            Ok(stats) => {
                circuit_breaker.record_success();
                memory_event!(layer6, dream_finished, stats.created, stats.updated, stats.deleted, stats.pruned);
                Ok(())
            }
            Err(e) => {
                circuit_breaker.record_failure();
                tracing::error!(error = %e, "Dream consolidation failed");
                Err(e)
            }
        }
    }
}
```

**关键设计点**:
- 完全后台任务，无用户感知
- 最长冷却期 (900s)，避免频繁重试
- 失败可能是系统性问题，需要更长恢复时间

### 7.6 日志配置

```rust
// 配置 tracing 以输出结构化日志

use tracing_subscriber::{
    fmt, EnvFilter, layer::SubscriberExt, util::SubscriberInitExt
};

pub fn init_logging() {
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .json() // JSON 格式输出
                .with_target(true)
                .with_current_span(false)
        )
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| {
                    // 默认配置
                    "blockcell.memory=info,blockcell_agent=debug".into()
                })
        )
        .init();
}
```

### 7.5 用户通知机制

**背景**: 当前压缩是静默执行的，用户不知道发生了什么。这可能导致困惑：
- 为什么对话历史突然变短了？
- 为什么之前的上下文好像丢失了？
- Agent 在做什么？

**解决方案**: 在压缩前后向用户发送通知消息。

#### 7.5.1 通知时机

| 时机 | 消息内容 | 目的 |
|------|---------|------|
| 压缩开始前 | "🔄 对话历史较长，正在压缩以保持性能..." | 告知用户正在进行操作 |
| 压缩成功后 | "✅ 已压缩对话历史，保留关键信息。Token 从 X 降至 Y。" | 确认操作完成，展示效果 |
| 压缩失败后 | "⚠️ 压缩失败，继续使用当前历史。原因：..." | 告知失败，不影响使用 |

#### 7.5.2 实现方案

通过 `OutboundMessage` 发送通知，自动支持所有 Channel：

```rust
// crates/agent/src/runtime.rs (修改)
//
// 注意: AgentRuntime 没有 current_channel()/current_chat_id() 方法
// 需要从消息上下文 (msg: &InboundMessage) 中获取 channel 和 chat_id

/// 压缩执行上下文 - 包含发送通知所需的信息
pub struct CompactContext<'a> {
    pub channel: &'a str,
    pub chat_id: &'a str,
    pub account_id: Option<&'a str>,
}

async fn execute_layer4_compact(
    &self,
    messages: &[ChatMessage],
    session_key: &str,
    ctx: Option<CompactContext<'_>>,  // 新增: 通知上下文
) -> Option<crate::compact::CompactResult> {
    use crate::compact::{CompactResult, generate_compact_summary};

    let pre_compact_tokens = estimate_messages_tokens(messages);

    // 1. 发送压缩开始通知
    if let (Some(ref tx), Some(ref ctx)) = (&self.outbound_tx, &ctx) {
        let mut notification = OutboundMessage::new(
            ctx.channel,
            ctx.chat_id,
            "🔄 对话历史较长，正在压缩以保持性能..."
        );
        if let Some(aid) = ctx.account_id {
            notification.account_id = Some(aid.to_string());
        }
        let _ = tx.send(notification).await;
    }

    // 2. 执行压缩
    let system_prompt = Arc::new(
        "你是一个对话摘要助手。请根据对话历史生成结构化摘要，保留关键信息用于后续继续工作。".to_string()
    );
    let model = self.config.agents.defaults.model.clone();

    let summary_result = generate_compact_summary(
        Arc::clone(&self.provider_pool),
        system_prompt,
        &model,
        messages.to_vec(),
    ).await;

    let summary_message = match summary_result {
        Ok(summary) => summary.to_markdown(),
        Err(e) => {
            warn!(error = %e, "[layer4] Failed to generate compact summary");

            // 发送失败通知
            if let (Some(ref tx), Some(ref ctx)) = (&self.outbound_tx, &ctx) {
                let mut notification = OutboundMessage::new(
                    ctx.channel,
                    ctx.chat_id,
                    "⚠️ 压缩失败，继续使用当前历史。"
                );
                if let Some(aid) = ctx.account_id {
                    notification.account_id = Some(aid.to_string());
                }
                let _ = tx.send(notification).await;
            }
            return None;
        }
    };

    // 3. 收集恢复信息 (省略...)
    let recovery_message = String::new();

    let post_compact_tokens = estimate_messages_tokens(&[
        ChatMessage::system(&summary_message),
        ChatMessage::user(&recovery_message),
    ]);

    // 4. 发送压缩成功通知
    if let (Some(ref tx), Some(ref ctx)) = (&self.outbound_tx, &ctx) {
        let compression_ratio = (pre_compact_tokens - post_compact_tokens)
            as f64 / pre_compact_tokens as f64 * 100.0;
        let mut notification = OutboundMessage::new(
            ctx.channel,
            ctx.chat_id,
            &format!(
                "✅ 已压缩对话历史，保留关键信息。\n📊 Token: {} → {} (压缩 {:.0}%)",
                pre_compact_tokens,
                post_compact_tokens,
                compression_ratio
            )
        );
        if let Some(aid) = ctx.account_id {
            notification.account_id = Some(aid.to_string());
        }
        let _ = tx.send(notification).await;
    }

    Some(CompactResult {
        summary_message,
        recovery_message,
        pre_compact_tokens,
        post_compact_tokens,
        success: true,
        error: None,
    })
}

// 调用示例 (在 process_message 中):
// let compact_ctx = CompactContext {
//     channel: &msg.channel,
//     chat_id: &msg.chat_id,
//     account_id: msg.account_id.as_deref(),
// };
// let result = self.execute_layer4_compact(&messages, &session_key, Some(compact_ctx)).await;
```

**优点**：
- 一次调用，自动支持所有 Channel (Telegram/Slack/Discord/WebSocket)
- 无需修改 Channel 层代码
- 用户在任何地方都能看到通知

#### 7.5.3 用户体验示例

```
用户: [发送消息]
Agent: 🔄 对话历史较长，正在压缩以保持性能...
        [几秒后]
Agent: ✅ 已压缩对话历史，保留关键信息。
       📊 Token: 85000 → 15000 (压缩 82%)
        [继续正常对话]
Agent: [实际回复内容]
```

#### 7.5.4 通知配置

```json
// config.json5 中配置通知行为
{
    "memorySystem": {
        "compactNotifications": {
            "enabled": true,
            "showTokenDetails": true,  // 显示 Token 详情
            "silentMode": false        // 静默模式（不发送通知）
        }
    }
}
```

#### 7.5.5 隐私考虑

- 通知消息不包含压缩的具体内容
- 只显示统计信息（Token 数、压缩率）
- 不泄露对话内容的任何细节

---

## 八、斜杠命令实现 (推荐方案)

> **📋 相关文档**: 斜杠命令的完整实现方案请参考 [slash-commands-unified-design.md](slash-commands-unified-design.md)。
> 本节仅描述 `/session_metrics` 命令的具体实现，框架集成已在上述文档中详细定义。

### 8.1 命令设计

斜杠命令命名考虑：
- ❌ `/metrics` — 太宽泛，可能与其他系统指标混淆
- ❌ `/memstat` — 不够明确，session 概念更清晰
- ✅ `/session_metrics` — 明确表达"会话记忆指标"语义

**推荐使用 `/session_metrics`**：语义清晰，明确表达监控的是会话记忆系统。

```bash
# 在 agent 交互模式或 gateway 对话框中使用
/session_metrics              # 查看完整记忆系统状态
/session_metrics --layer 4    # 只看 Layer 4 压缩统计
/session_metrics --json       # JSON 格式输出
/session_metrics --reset      # 重置计数器
```

### 8.2 适用范围

| 模式 | 支持情况 | 说明 |
|------|----------|------|
| `blockcell agent` | ✅ 支持 | stdin 线程本地处理 |
| `blockcell gateway` (WebSocket) | ✅ 支持 | 通过 SlashCommandHandler 拦截 |
| Channel (Telegram/Slack/...) | ✅ 支持 | 通过 SlashCommandHandler 拦截 |

> **实现详情**: 参见 [slash-commands-unified-design.md](slash-commands-unified-design.md) 的"五、实现方案"章节。

### 8.3 命令处理流程

```
用户输入: "/session_metrics"
       │
       ├─► 检查是否为斜杠命令
       │       │
       │       ├─► 是: 本地执行，返回结果
       │       │       (不经过 LLM，零 Token 消耗)
       │       │
       │       └─► 否: 正常发送给 AgentRuntime → LLM
       │
       └─► 输出结果到当前 channel
```

### 8.3.1 CommandResult 枚举

斜杠命令处理器返回 `CommandResult` 枚举，包含以下变体：

| 变体 | 用途 | 示例命令 |
|------|------|----------|
| `Handled(CommandResponse)` | 正常响应，包含内容和格式标记 | `/help`, `/skills`, `/tools`, `/session_metrics` |
| `NotACommand` | 非命令输入，交给下游处理 | 普通消息 |
| `PermissionDenied(String)` | 权限不足，拒绝执行 | 渠道限制命令 |
| `Error(String)` | 命令执行错误 | 超时、内部错误 |
| `ExitRequested` | 请求退出交互模式 | `/quit`, `/exit` |
| `ForwardToRuntime` | 需转发给 AgentRuntime 处理 | `/learn` |

**CommandResponse 结构**:

```rust
pub struct CommandResponse {
    /// 响应内容
    pub content: String,
    /// 是否为 Markdown 格式
    pub is_markdown: bool,
}
```

> **注意**: 所有命令响应应使用 `CommandResponse::markdown(content)` 以确保 WebUI 正确渲染。

### 8.4 输出示例

```text
╔═══════════════════════════════════════════════════════════════╗
║              BlockCell Memory Metrics Summary                 ║
╠═══════════════════════════════════════════════════════════════╣
║                                                               ║
║  Layer 1: Tool Result Storage                                 ║
║  ├─ Persisted: 127 files                                      ║
║  ├─ Original: 45.2 MB → Preview: 234 KB                       ║
║  ├─ Budget exceeded: 3 times                                  ║
║  └─ Compression: 99.5%                                        ║
║                                                               ║
║  Layer 2: Micro Compact                                       ║
║  └─ Time-triggered cleanups: 12                               ║
║                                                               ║
║  Layer 3: Session Memory                                      ║
║  ├─ Extractions: 8                                            ║
║  └─ Current size: 12.3 KB                                     ║
║                                                               ║
║  Layer 4: Full Compact                                        ║
║  ├─ Total: 23 (auto: 21, manual: 2)                           ║
║  ├─ Failed: 2 (8.7%)                                          ║
║  ├─ Avg compression: 67.2%                                    ║
║  └─ Cache hit rate: 89.1%                                     ║
║                                                               ║
║  Layer 5: Memory Extraction                                   ║
║  ├─ Extractions: 15                                           ║
║  └─ Memories: user(5), project(8), feedback(2), ref(2)        ║
║                                                               ║
║  Layer 6: Dream                                               ║
║  ├─ Last run: 2 hours ago                                     ║
║  └─ Sessions consolidated: 7                                  ║
║                                                               ║
║  Circuit Breaker: ● CLOSED (正常)                             ║
║                                                               ║
╚═══════════════════════════════════════════════════════════════╝
```

### 8.5 CLI 命令实现

```rust
// bin/blockcell/src/commands/metrics.rs

use clap::Subcommand;
use blockcell_agent::memory_metrics::{get_metrics_summary, MetricsSummary};

#[derive(Subcommand)]
pub enum MetricsCommands {
    /// Show memory system metrics
    Memory {
        /// Filter by layer (1-7)
        #[arg(short, long)]
        layer: Option<u8>,
        
        /// Output as JSON
        #[arg(short, long)]
        json: bool,
        
        /// Reset counters
        #[arg(short, long)]
        reset: bool,
    },
}

pub fn handle_metrics_memory(layer: Option<u8>, json: bool, reset: bool) {
    if reset {
        // 重置计数器
        blockcell_agent::memory_metrics::reset_metrics();
        println!("Metrics counters reset.");
        return;
    }
    
    let summary = get_metrics_summary();
    
    if json {
        println!("{}", serde_json::to_string_pretty(&summary).unwrap());
        return;
    }
    
    // 美化输出
    print_metrics_summary(&summary, layer);
}

fn print_metrics_summary(summary: &MetricsSummary, layer_filter: Option<u8>) {
    use console::{style, Emoji};
    use indicatif::HumanBytes;

    let check = Emoji("●", "*");
    let cross = Emoji("○", "o");

    println!();
    println!("╔═══════════════════════════════════════════════════════════════╗");
    println!("║{: ^63}║", style("BlockCell Memory Metrics Summary").bold());
    println!("╠═══════════════════════════════════════════════════════════════╣");

    // Layer 1
    if layer_filter.is_none() || layer_filter == Some(1) {
        println!("║                                                               ║");
        println!("║  {} Layer 1: Tool Result Storage", style("📁").dim());
        println!("║  ├─ Persisted: {} files", summary.layer1.persisted_count);
        println!("║  ├─ Original: {} → Preview: {}",
            HumanBytes(summary.layer1.total_original_size),
            HumanBytes(summary.layer1.total_preview_size)
        );
        println!("║  ├─ Budget exceeded: {} times", summary.layer1.budget_exceeded_count);
        println!("║  └─ Compression: {:.1}%", summary.layer1.average_compression * 100.0);
    }

    // Layer 2
    if layer_filter.is_none() || layer_filter == Some(2) {
        println!("║                                                               ║");
        println!("║  {} Layer 2: Micro Compact", style("⚡").dim());
        println!("║  ├─ Triggered: {} times", summary.layer2.trigger_count);
        println!("║  ├─ Cleared: {} items", summary.layer2.cleared_count);
        println!("║  └─ Kept: {} items", summary.layer2.kept_count);
    }

    // Layer 3
    if layer_filter.is_none() || layer_filter == Some(3) {
        println!("║                                                               ║");
        println!("║  {} Layer 3: Session Memory", style("📝").dim());
        println!("║  ├─ Extractions: {}", summary.layer3.extraction_count);
        println!("║  ├─ Loads: {}", summary.layer3.load_count);
        println!("║  └─ Current size: {}", HumanBytes(summary.layer3.current_size));
    }

    // Layer 4
    if layer_filter.is_none() || layer_filter == Some(4) {
        let success_rate = if summary.layer4.compact_count > 0 {
            1.0 - (summary.layer4.failed_count as f64 / summary.layer4.compact_count as f64)
        } else {
            1.0
        };

        println!("║                                                               ║");
        println!("║  {} Layer 4: Full Compact", style("🗜️ ").dim());
        println!("║  ├─ Total: {} (auto: {}, manual: {})",
            summary.layer4.compact_count,
            summary.layer4.auto_compact_count,
            summary.layer4.manual_compact_count
        );
        println!("║  ├─ Failed: {} ({:.1}%)",
            summary.layer4.failed_count,
            (1.0 - success_rate) * 100.0
        );
        println!("║  ├─ Avg compression: {:.1}%", summary.layer4.average_compression_ratio * 100.0);
        println!("║  └─ Cache hit rate: {:.1}%", summary.layer4.cache_hit_rate * 100.0);
    }

    // Layer 5
    if layer_filter.is_none() || layer_filter == Some(5) {
        println!("║                                                               ║");
        println!("║  {} Layer 5: Memory Extraction", style("🧠").dim());
        println!("║  ├─ Extractions: {}", summary.layer5.extraction_count);
        println!("║  └─ Storage: {}", HumanBytes(summary.layer5.total_bytes_written));
    }

    // Layer 6
    if layer_filter.is_none() || layer_filter == Some(6) {
        println!("║                                                               ║");
        println!("║  {} Layer 6: Auto Dream", style("💤").dim());
        println!("║  ├─ Dream runs: {}", summary.layer6.dream_count);
        println!("║  ├─ Memories: +{}/~{}/-{}",
            summary.layer6.memories_created,
            summary.layer6.memories_updated,
            summary.layer6.memories_deleted
        );
        println!("║  └─ Sessions pruned: {}", summary.layer6.sessions_pruned);
    }

    // Layer 7
    if layer_filter.is_none() || layer_filter == Some(7) {
        println!("║                                                               ║");
        println!("║  {} Layer 7: Forked Agent", style("🤖").dim());
        println!("║  ├─ Spawned: {}", summary.layer7.spawned_count);
        println!("║  ├─ Completed: {} / Failed: {}",
            summary.layer7.completed_count,
            summary.layer7.failed_count
        );
        println!("║  ├─ Tool denied: {}", summary.layer7.tool_denied_count);
        println!("║  └─ Tokens used: {}", HumanBytes(summary.layer7.total_tokens_used));
    }

    // Circuit Breaker
    println!("║                                                               ║");
    let cb_state = match summary.circuit_breaker_state {
        CircuitState::Open => (cross, "OPEN", "熔断中"),
        CircuitState::HalfOpen => (check, "HALF_OPEN", "半开"),
        CircuitState::Closed => (check, "CLOSED", "正常"),
    };
    println!("║  Circuit Breaker: {} {} ({})",
        cb_state.0,
        style(cb_state.1).bold(),
        cb_state.2
    );

    println!("║                                                               ║");
    println!("╚═══════════════════════════════════════════════════════════════╝");
    println!();
}
```

---

## 附录 A: 实现清单

### A.1 Phase 1: 核心指标结构 (P0) ✅ 已完成

> **📁 文件变更**：
> - 现有文件：`crates/agent/src/metrics.rs`
> - 创建完成：`crates/agent/src/session_metrics/` 目录
> - 现有 `metrics.rs` 已移动到 `session_metrics/mod.rs`

- [x] 扩展 `crates/agent/src/session_metrics/mod.rs` 添加 `MemoryMetrics` 结构体
- [x] 实现 `get_memory_metrics()` 全局访问函数
- [x] 添加 `Layer1Metrics` ~ `Layer7Metrics` 结构体
- [x] 添加 `circuit_breaker_state` 和 `circuit_breaker_failures` 字段
- [x] 添加 `ProcessingMetrics` 和 `ScopedTimer` 结构体

### A.2 Phase 2: 事件记录 (P0) ✅ 已完成

- [x] 实现 `memory_event!` 宏
- [x] Layer 1 事件: `persisted`, `budget_exceeded`
- [x] Layer 2 事件: `triggered`, `cleared`
- [x] Layer 3 事件: `extraction_started`, `loaded`
- [x] Layer 4 事件: `compact_started`, `compact_completed`, `compact_failed`
- [x] Layer 5 事件: `memory_written`, `injection_completed`
- [x] Layer 6 事件: `dream_started`, `dream_finished`
- [x] Layer 7 事件: `agent_spawned`, `agent_completed`, `agent_failed`, `tool_denied`

### A.3 Phase 3: 熔断器 (P1) ✅ 已完成

- [x] 实现 `CircuitBreaker` 结构体（无锁原子实现）
- [x] 实现 `CircuitBreakerConfig` 配置
- [x] 实现 `Closed` → `Open` → `HalfOpen` 状态转换
- [x] 集成到 `compact/mod.rs` 或 `runtime.rs`

#### A.3.1 多层熔断器 ✅ 已完成 (2026-04-08)

- [x] 实现 `COMPACT_CIRCUIT_BREAKER` (Layer 4: 快速恢复 - 60s)
- [x] 实现 `MEMORY_EXTRACTION_CIRCUIT_BREAKER` (Layer 5: 中等恢复 - 300s)
- [x] 实现 `DREAM_CIRCUIT_BREAKER` (Layer 6: 慢恢复 - 900s)
- [x] 添加 `get_compact_circuit_breaker()` 函数
- [x] 添加 `get_memory_extraction_circuit_breaker()` 函数
- [x] 添加 `get_dream_circuit_breaker()` 函数
- [x] 添加 `reset_all_circuit_breakers()` 函数
- [x] 更新 `summary.rs` 显示三层熔断器状态
- [x] 更新 `MetricsSummary` 包含三个 `CircuitBreakerSummary`
- [x] 更新 `format_metrics_table()` 显示所有熔断器状态

**相关文件**：
- `crates/agent/src/session_metrics/circuit_breaker.rs`: 三层熔断器实现
- `crates/agent/src/session_metrics/mod.rs`: 导出函数
- `crates/agent/src/session_metrics/summary.rs`: 状态汇总和显示
- `crates/agent/src/session_metrics/memory.rs`: 指标存储

### A.4 Phase 4: 斜杠命令 (P1) ✅ 已完成

- [x] 实现 `/session_metrics` 命令处理器
- [x] 集成到 `SlashCommandHandler` (参见 [slash-commands-unified-design.md](slash-commands-unified-design.md))
- [x] 支持 `--layer N` 过滤
- [x] 支持 `--json` 输出
- [x] 支持 `--reset` 重置

### A.5 Phase 5: 用户通知 (P2) ⏳ 未实现

- [ ] 实现 `CompactContext` 结构体
- [ ] 压缩开始通知
- [ ] 压缩成功通知
- [ ] 压缩失败通知

---

## 附录 B: 配置持久化

### B.1 配置结构定义

```rust
// crates/core/src/config.rs (扩展)

/// 记忆系统配置
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct MemorySystemConfig {
    /// 压缩通知配置
    pub compact_notifications: CompactNotificationConfig,
    /// 熔断器配置
    pub circuit_breaker: CircuitBreakerConfigJson,
    /// 监控配置
    pub monitoring: MonitoringConfig,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct CompactNotificationConfig {
    /// 是否启用通知
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 显示 Token 详情
    #[serde(default = "default_true")]
    pub show_token_details: bool,
    /// 静默模式 (不发送通知)
    #[serde(default)]
    pub silent_mode: bool,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct CircuitBreakerConfigJson {
    /// 最大连续失败次数
    #[serde(default = "default_max_failures")]
    pub max_failures: u64,
    /// 开启后等待时间 (秒)
    #[serde(default = "default_reset_timeout_secs")]
    pub reset_timeout_secs: u64,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct MonitoringConfig {
    /// 是否启用详细日志
    #[serde(default)]
    pub verbose_logging: bool,
    /// 日志输出格式: "json" | "text"
    #[serde(default = "default_log_format")]
    pub log_format: String,
}

fn default_true() -> bool { true }
fn default_max_failures() -> u64 { 3 }
fn default_reset_timeout_secs() -> u64 { 60 }
fn default_log_format() -> String { "text".to_string() }

impl Default for MemorySystemConfig {
    fn default() -> Self {
        Self {
            compact_notifications: CompactNotificationConfig::default(),
            circuit_breaker: CircuitBreakerConfigJson::default(),
            monitoring: MonitoringConfig::default(),
        }
    }
}
```

### B.2 config.json5 示例

```json
{
  "providers": { ... },
  "agents": { ... },
  "memorySystem": {
    "compactNotifications": {
      "enabled": true,
      "showTokenDetails": true,
      "silentMode": false
    },
    "circuitBreaker": {
      "maxFailures": 3,
      "resetTimeoutSecs": 60
    },
    "monitoring": {
      "verboseLogging": false,
      "logFormat": "json"
    }
  }
}
```

### B.3 配置读取

```rust
// crates/agent/src/runtime.rs

impl AgentRuntime {
    fn load_memory_config(&self) -> MemorySystemConfig {
        self.config.memory_system.clone().unwrap_or_default()
    }

    fn should_send_compact_notification(&self) -> bool {
        self.config.memory_system
            .as_ref()
            .map(|m| m.compact_notifications.enabled && !m.compact_notifications.silent_mode)
            .unwrap_or(true)
    }
}
```

---

## 附录 C: 测试策略

### C.1 单元测试

```rust
// crates/agent/src/session_metrics/mod.rs (测试模块)

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn test_memory_metrics_layer4_recording() {
        let metrics = MemoryMetrics::default();

        // 模拟压缩成功
        metrics.layer4.compact_count.fetch_add(1, Ordering::Relaxed);
        metrics.layer4.total_pre_compact_tokens.fetch_add(1000, Ordering::Relaxed);
        metrics.layer4.total_post_compact_tokens.fetch_add(300, Ordering::Relaxed);

        assert_eq!(metrics.layer4.compact_count.load(Ordering::Relaxed), 1);

        // 计算压缩率
        let ratio = metrics.layer4.average_compression_ratio();
        assert!((ratio - 0.7).abs() < 0.01); // 70% 压缩率
    }

    #[test]
    fn test_circuit_breaker_state_transitions() {
        let config = CircuitBreakerConfig {
            max_failures: 2,
            reset_timeout: Duration::from_millis(100),
            half_open_max_calls: 1,
        };
        let cb = CircuitBreaker::new(config);

        // 初始状态: Closed
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow());

        // 第一次失败
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        // 第二次失败 -> Open
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allow());

        // 等待超时 -> HalfOpen
        std::thread::sleep(Duration::from_millis(150));
        assert!(cb.allow());
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // 成功 -> Closed
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_processing_metrics_integration() {
        let mut pm = ProcessingMetrics::new();
        pm.record_decision(100);
        pm.record_llm_call(200);
        pm.record_compression();
        pm.record_tool_execution("read_file", 50);

        assert_eq!(pm.decision_duration_ms, Some(100));
        assert_eq!(pm.llm_calls.len(), 1);
        assert_eq!(pm.compression_count, 1);
    }

    #[test]
    fn test_concurrent_metrics_access() {
        use std::sync::Arc;
        use std::thread;

        // 使用 Arc 替代 unsafe 指针，安全且符合 Rust 最佳实践
        let metrics = Arc::new(MemoryMetrics::default());

        let handles: Vec<_> = (0..10)
            .map(|i| {
                let metrics_clone = Arc::clone(&metrics);
                thread::spawn(move || {
                    metrics_clone.layer1.persisted_count.fetch_add(i, Ordering::Relaxed);
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // 验证并发累加正确
        let total: u64 = (0..10).sum();
        assert_eq!(metrics.layer1.persisted_count.load(Ordering::Relaxed), total);
    }

    // ========== 宏测试 ==========

    #[test]
    fn test_memory_event_macro_layer1() {
        // 重置指标
        let metrics = get_memory_metrics();
        metrics.layer1.persisted_count.store(0, Ordering::Relaxed);
        metrics.layer1.total_original_size.store(0, Ordering::Relaxed);
        metrics.layer1.total_preview_size.store(0, Ordering::Relaxed);

        // 调用宏
        memory_event!(layer1, persisted, "tool-123", 1000, 100);

        // 验证指标更新
        assert_eq!(metrics.layer1.persisted_count.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.layer1.total_original_size.load(Ordering::Relaxed), 1000);
        assert_eq!(metrics.layer1.total_preview_size.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn test_memory_event_macro_layer4_completed() {
        let metrics = get_memory_metrics();
        // 重置相关指标
        metrics.layer4.compact_count.store(0, Ordering::Relaxed);
        metrics.layer4.total_pre_compact_tokens.store(0, Ordering::Relaxed);
        metrics.layer4.total_post_compact_tokens.store(0, Ordering::Relaxed);
        metrics.layer4.consecutive_failures.store(5, Ordering::Relaxed);

        // 调用宏
        memory_event!(layer4, compact_completed, 10000, 3000, 8000, 2000);

        // 验证指标更新
        assert_eq!(metrics.layer4.compact_count.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.layer4.total_pre_compact_tokens.load(Ordering::Relaxed), 10000);
        assert_eq!(metrics.layer4.total_post_compact_tokens.load(Ordering::Relaxed), 3000);
        // 连续失败应重置为 0
        assert_eq!(metrics.layer4.consecutive_failures.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_memory_event_macro_layer4_failed() {
        let metrics = get_memory_metrics();
        metrics.layer4.compact_failed_count.store(0, Ordering::Relaxed);
        metrics.layer4.consecutive_failures.store(0, Ordering::Relaxed);

        // 调用宏
        memory_event!(layer4, compact_failed, "LLM timeout", 50000, 1);

        assert_eq!(metrics.layer4.compact_failed_count.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.layer4.consecutive_failures.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_memory_event_macro_layer6_dream_finished() {
        let metrics = get_memory_metrics();
        metrics.layer6.memories_created.store(0, Ordering::Relaxed);
        metrics.layer6.memories_updated.store(0, Ordering::Relaxed);
        metrics.layer6.memories_deleted.store(0, Ordering::Relaxed);
        metrics.layer6.sessions_pruned.store(0, Ordering::Relaxed);

        memory_event!(layer6, dream_finished, 5, 3, 1, 2);

        assert_eq!(metrics.layer6.memories_created.load(Ordering::Relaxed), 5);
        assert_eq!(metrics.layer6.memories_updated.load(Ordering::Relaxed), 3);
        assert_eq!(metrics.layer6.memories_deleted.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.layer6.sessions_pruned.load(Ordering::Relaxed), 2);
    }
}
```

### C.2 集成测试

| 测试场景 | 输入 | 预期输出 |
|---------|------|---------|
| CLI `/session_metrics` | `/session_metrics` | 显示完整指标表格 |
| WebSocket `/session_metrics --json` | `{"type":"chat","content":"/session_metrics --json"}` | 返回 JSON 格式指标 |
| 压缩触发熔断器 | 连续 3 次压缩失败 | 熔断器状态变为 Open |
| 压缩通知发送 | 触发自动压缩 | 用户收到通知消息 |
| 指标持久化 | 重启 Agent | 指标重置为 0 (内存指标) |

### C.3 性能测试

```rust
// benches/metrics_benchmark.rs

use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn benchmark_metrics_update(c: &mut Criterion) {
    let metrics = MemoryMetrics::default();

    c.bench_function("layer4_metrics_update", |b| {
        b.iter(|| {
            black_box(&metrics).layer4.compact_count.fetch_add(1, Ordering::Relaxed);
            black_box(&metrics).layer4.total_pre_compact_tokens.fetch_add(1000, Ordering::Relaxed);
            black_box(&metrics).layer4.total_post_compact_tokens.fetch_add(300, Ordering::Relaxed);
        })
    });

    c.bench_function("concurrent_metrics_update", |b| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                let m = get_memory_metrics();
                m.layer1.persisted_count.fetch_add(1, Ordering::Relaxed);
            })
    });
}

criterion_group!(benches, benchmark_metrics_update);
criterion_main!(benches);
```

**性能目标**:
- 单次指标更新: < 100ns (AtomicU64 无锁)
- 并发 100 线程更新: < 1μs 平均延迟
- `/session_metrics` 命令: < 10ms 响应时间

---

## 附录 D: 日志格式配置

### D.1 环境变量配置

```bash
# 基础配置: 启用记忆系统日志
export RUST_LOG="blockcell.memory=info,blockcell_agent=debug"

# 详细配置: 按层过滤
export RUST_LOG="blockcell.session_metrics.layer4=trace,blockcell.session_metrics.layer5=debug"

# JSON 格式输出 (用于日志聚合)
export RUST_LOG_FORMAT="json"

# 禁用特定层日志
export RUST_LOG="blockcell.memory=info,blockcell.session_metrics.layer2=off"
```

### D.2 代码配置

```rust
// crates/agent/src/lib.rs

pub fn init_logging() {
    use tracing_subscriber::{
        fmt, EnvFilter, layer::SubscriberExt, util::SubscriberInitExt
    };

    let log_format = std::env::var("RUST_LOG_FORMAT")
        .unwrap_or_else(|_| "text".to_string());

    let json_layer = if log_format == "json" {
        Some(fmt::layer()
            .json()
            .with_target(true)
            .with_current_span(false))
    } else {
        None
    };

    let text_layer = if log_format != "json" {
        Some(fmt::layer()
            .with_target(true)
            .with_thread_ids(false))
    } else {
        None
    };

    tracing_subscriber::registry()
        .with(json_layer)
        .with(text_layer)
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| {
                    // 默认配置
                    "blockcell.memory=info,blockcell_agent=debug".into()
                })
        )
        .init();
}
```

### D.3 日志输出示例

**文本格式**:

```
2026-04-07T10:15:30.123Z INFO blockcell.session_metrics.layer4: event=compact_started pre_compact_tokens=85000 threshold=80000 is_auto=true Compact started
2026-04-07T10:15:32.456Z INFO blockcell.session_metrics.layer4: event=compact_completed pre_compact_tokens=85000 post_compact_tokens=15000 compression_ratio="82.35%" cache_read_tokens=12000 cache_creation_tokens=3000 cache_hit_rate="80.00%" Compact completed successfully
```

**JSON 格式**:

```json
{"timestamp":"2026-04-07T10:15:30.123Z","level":"INFO","target":"blockcell.session_metrics.layer4","event":"compact_started","pre_compact_tokens":85000,"threshold":80000,"is_auto":true,"message":"Compact started"}
{"timestamp":"2026-04-07T10:15:32.456Z","level":"INFO","target":"blockcell.session_metrics.layer4","event":"compact_completed","pre_compact_tokens":85000,"post_compact_tokens":15000,"compression_ratio":"82.35%","cache_read_tokens":12000,"cache_creation_tokens":3000,"cache_hit_rate":"80.00%","message":"Compact completed successfully"}
```

---

## 附录 E: 与 ProcessingMetrics 的关系

### E.1 职责分离

| 指标类型 | ProcessingMetrics | MemoryMetrics |
|---------|-------------------|---------------|
| **生命周期** | 单次消息处理 | 跨会话累积 |
| **存储位置** | 栈 (RAII) | 静态全局 |
| **更新频率** | 每次工具调用 | 每次记忆操作 |
| **线程安全** | 单线程独占 | 多线程共享 (Atomic) |
| **用途** | 性能分析、调试 | 容量规划、监控 |

### E.2 协作示例

```rust
// crates/agent/src/runtime.rs

async fn process_message(&mut self, msg: InboundMessage) -> Result<()> {
    // ProcessingMetrics: RAII 计时器，自动记录本次处理耗时
    let mut pm = ProcessingMetrics::new();

    // ... 处理消息 ...

    // 压缩时同时更新两个指标
    if needs_compact {
        pm.record_compression();  // 本次处理的压缩计数
        get_memory_metrics().layer4.compact_count.fetch_add(1, Ordering::Relaxed);  // 全局累积
    }

    // ProcessingMetrics drop 时自动 log_summary()
    // 包含本次处理的 LLM 调用次数、工具执行时间等
}
```

### E.3 数据流

```
单次消息处理
       │
       ├─► ProcessingMetrics (栈)
       │    ├─ record_llm_call()
       │    ├─ record_tool_execution()
       │    └─ record_compression()
       │
       └─► MemoryMetrics (全局静态)
            ├─ layer1.persisted_count++
            ├─ layer4.compact_count++
            └─ layer5.extraction_count++
```

### E.4 ScopedTimer 与 MemoryMetrics 的关系

**现有 ScopedTimer** (位于 `metrics.rs`):

```rust
/// A simple RAII timer that records duration on drop.
pub(crate) struct ScopedTimer {
    start: Instant,
}

impl ScopedTimer {
    pub fn new() -> Self { ... }
    pub fn elapsed_ms(&self) -> u64 { ... }
}
```

**设计意图**:
- `ScopedTimer` 是一个轻量级的 RAII 计时器，用于测量代码块执行时间
- 当前设计是"创建时开始计时，drop 时由调用者读取结果"
- 与 `ProcessingMetrics` 配合使用，由调用者决定何时记录

**与 MemoryMetrics 的关系**:

| 场景 | ScopedTimer | MemoryMetrics |
| ---- | ----------- | ------------- |
| **用途** | 测量单次操作耗时 | 累积统计指标 |
| **输出方式** | 返回 `u64` 给调用者 | 直接更新 Atomic 计数器 |
| **典型使用** | LLM 调用计时、工具执行计时 | 压缩次数、缓存命中率 |

**扩展建议**: 可以考虑创建一个 `TimedOperation` 包装器，在 drop 时自动更新 MemoryMetrics：

```rust
/// 自动记录操作耗时到 MemoryMetrics
pub struct TimedCompact<'a> {
    start: Instant,
    metrics: &'a Layer4Metrics,
}

impl<'a> TimedCompact<'a> {
    pub fn new(metrics: &'a Layer4Metrics) -> Self {
        Self {
            start: Instant::now(),
            metrics,
        }
    }
}

impl<'a> Drop for TimedCompact<'a> {
    fn drop(&mut self) {
        let duration_ms = self.start.elapsed().as_millis() as u64;
        // 可选: 记录到 metrics 或日志
        tracing::debug!(
            duration_ms,
            "Compact operation completed"
        );
    }
}
```

**注意**: 这种自动记录需要谨慎使用，因为不是所有操作都需要记录耗时。对于大多数场景，保持 `ScopedTimer` 的简单设计更为灵活。

---

> **文档版本**: 2026-04-08
> **状态**: ✅ 已审查（工程审查通过，可进入实现阶段）
> **目标框架**: BlockCell Rust 多智能体框架
>
> **变更记录**:
>
> - 2026-04-08: **修复 Cache Token 估算问题**：移除硬编码比例估算 (`0.6`, `0.2`)，改为从 LLM API 响应获取真实的 `cache_read_tokens` 和 `cache_creation_tokens`。修改 `CompactSummaryResult` 返回真实 usage 数据，`CompactResult` 新增 cache 字段
> - 2026-04-08: **添加缺失的 getter 和 reset 方法**：为 `Layer3Metrics`~`Layer7Metrics` 添加 getter 方法（如 `auto_compact_count()`, `memories_created()` 等），为所有 Layer 添加 `reset()` 方法，修复 `get_metrics_summary()` 返回硬编码零值问题
> - 2026-04-08: **修复测试断言错误**：修正 `test_layer4_metrics` 中压缩率和缓存命中率的数学计算
> - 2026-04-08: 工程审查通过；修复宏路径引用 (`metrics::` → `session_metrics::`)；添加宏单元测试；移除测试中 unsafe 代码；补充 ScopedTimer 说明
> - 2026-04-08: 修正文件路径说明，区分现有和推荐结构；补充 CommandResult 枚举说明
> - 2026-04-07: 初始版本