# providers Review 报告（Wave 1 · 基础层）
> 只读审查 · 2026-10-16 · 子代理 w1-providers

## 规模（文件数/LOC/入口）

- 13 个 `.rs` 文件，约 8,003 LOC（Cargo.toml 1 个）
- 入口：`src/lib.rs`（73 行，定义 `Provider` trait：`chat` / `chat_stream`，默认实现把非流转流）

| 文件 | LOC | 职责 |
|------|-----|------|
| openai.rs | 1519 | OpenAI 兼容协议（chat/completions）、文本工具标记流式过滤、reasoning_effort 注入 |
| anthropic.rs | 1138 | Anthropic Messages 协议、prompt cache 断点（fork 对齐） |
| pool.rs | 899 | ProviderPool 健康状态机、加权选取、路由策略、错误分类 |
| gemini.rs | 897 | Gemini 原生 generateContent / streamGenerateContent |
| openai/tool_markup.rs | 817 | 文本工具调用解析（tool_call / DSML / minimax invoke / 松散参数） |
| ollama.rs | 736 | Ollama /api/chat（NDJSON 流） |
| openai_responses.rs | 685 | OpenAI Responses API（仅非流式） |
| factory.rs | 417 | provider 解析优先级（显式 > 前缀推断 > fallback）、api_type 路由 |
| client.rs | 296 | 代理解析（provider > global > env）、no_proxy 匹配、reqwest 构建 |
| security.rs | 212 | 响应限量（32MB/64KB/流 32MB/行 1MB）、redact_url、错误构造 |
| embeddings.rs | 193 | OpenAI 兼容 embedder（blocking client） |
| prompt_utils.rs | 121 | prompt_cache_key 稳定哈希、文本工具 prompt |
| lib.rs | 73 | trait 与重导出 |

## 模块地图

- **provider 抽象**：`lib.rs:Provider`（async_trait，`chat` 非流 + `chat_stream` 流，默认实现降级为整段转换）；`factory.rs` 按 显式 provider > model 前缀推断 > 首个有效 api_key 的顺序实例化，并对内置名（anthropic/gemini/ollama）**按名称优先**路由，其余按 `api_type` 路由到 anthropic/gemini/ollama/openai_responses/openai 四种协议实现。
- **流式协议**：OpenAI 兼容（`data: ` SSE，[DONE] 终止，index 键 tool call 增量累积）、Anthropic（`event:`/`data:` 双行 SSE，content_block_start/delta/stop + message_stop）、Gemini（`alt=sse`，NDJSON-ish SSE）、Ollama（NDJSON，done 标志）；统一经 `security.rs:StreamGuard` 限量并在无完成标记时报错。
- **prompt cache**：Anthropic 侧最多 4 个 `cache_control: ephemeral` 断点（system + 末 tool + 消息 len-2 + len-4，其中 len-4 专为上层 fork 对齐设计，L 父轮断点 L-2，fork 后 L+2 轮断点 (L+2)-4 = L-2，已有单测锁定）；OpenAI 兼容与 Responses 侧经 `prompt_cache_key`（system+model+tools 稳定哈希）。
- **健康与路由**：`pool.rs` 状态机 Healthy/Cooling/Dead，`report()` 上报驱动，`classify_error()` 按错误文本分类（429/401/403/5xx），`acquire_with_strategy` 支持 manual/cost_optimized/quality_first/latency_first。
- **被谁依赖**：agent runtime（acquire+report+usage 统计）、bin CLI（pool 构建与覆盖）、storage（`Embedder` trait 由 embeddings 实现）、skills（evolution provider）。

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|------|-------------|------|-----------|
| P0 | factory.rs:195-204 + core config.rs:145-155 | **Gemini 默认配置开箱即挂**：默认 `providers.gemini` 是 `api_type:"openai"` + base `https://generativelanguage.googleapis.com/v1beta/openai`，但 factory 顶层按名称 `"gemini"` 路由到原生 `GeminiProvider`，用该 base 拼 `/models/{model}:generateContent` → 请求 404。docs/18 的预置表同样写的是 `/v1beta/openai`，用户无从得知要改 base | 名称匹配优先于 api_type；GeminiProvider::generate_content_url 拼出 `.../v1beta/openai/models/...`，非端点 |
| P1 | anthropic.rs:605, openai.rs:859, gemini.rs:538, ollama.rs:474 | **SSE 半包 UTF-8 损坏**：四个 provider 均 `buffer.push_str(&String::from_utf8_lossy(&bytes))`。多字节字符（中文/emoji）跨 TCP chunk 拆分时 lossy 注入 U+FFFD：若落在 JSON 行内 → 解析失败 → "Malformed stream event" 整流中断；若落在文本增量 → 用户可见乱码。属间歇性、取决于分包边界 | 无跨 chunk 的字节缓冲/`from_utf8` 完整性校验；建议保留 `Vec<u8>` 残包或只在行完整时解码 |
| P1 | client.rs:116-149 + anthropic.rs:81/openai.rs:231/gemini.rs:81 等 | **流式请求使用总超时 120s**：reqwest 的 `timeout` 覆盖"连接建立→响应体读完"，而 SSE 流的 body 直到流结束才"读完"。长回答/深度思考/长工具循环超过 120s 会被静默切断，mid-stream 报错。与"主对话走 chat_stream"的用法直接冲突 | 应改用 `read_timeout`（空闲读超时）或去掉总超时；ollama 用 300s 同理 |
| P1 | pool.rs:399-415（及 267-270） | **文档承诺的"冷却保底"未实现**：`healthy_or_cooling_fallback_indices` 在没有健康条目时直接 `None`，函数名带 cooling_fallback 但无任何降级逻辑；docs/20:90、:184 与 docs/28:93 均承诺"全部冷却时临时解除冷却保底" | 契约不一致；且 doc20:15、:101 的状态图暗示 5xx 立即冷却，代码是累计 3 次才冷却（pool.rs:500-518） |
| P1 | ollama.rs:114-131 | **Ollama 丢弃 assistant 历史 tool_calls**：`convert_messages` 对所有消息硬编码 `tool_calls: None`，原生工具多轮对话回传时 assistant 的 tool_calls 全丢失，后续 role:"tool" 消息失去依托，上下文错乱甚至被 Ollama 拒绝 | `OllamaMessage { tool_calls: None, ... }` 无分支处理 |
| P1 | core config.rs:183-191 + anthropic.rs:65-80 | **MiniMax 默认配置疑似端点错误**：默认 `api_type:"anthropic"` + base `https://api.minimaxi.com/v1` → AnthropicProvider POST `/v1/messages`；而本仓测试注释与 MiniMax Anthropic 兼容端点均为 `.../anthropic/v1/messages`。默认配置下 MiniMax 大概率 404（需按 MiniMax 现行文档最终确认） | anthropic.rs:1111-1137 的测试自己用的 base 是 `/anthropic` |
| P2 | gemini.rs:579-588, 713-720 | **Gemini thinking part 未识别**：`GeminiPart` 无 `thought` 字段，带 `thought:true` 的思考文本按普通 text 推送，污染用户可见内容；`reasoning_content` 永远为 None，Gemini 2.5+ 思考模型内容受损 | 应识别 thought 标记并转 ReasoningDelta |
| P2 | gemini.rs:441-448, 554-558 | **Gemini prompt cache 指标缺失**：`usageMetadata.cached_content_token_count` 未映射为 `cache_read_input_tokens`（Anthropic/OpenAI 均映射），上游缓存命中统计对 Gemini 恒为 0 | ⑥ provider 抽象漏字段 |
| P2 | anthropic.rs:728-735 | **Anthropic 流式 usage 整体覆盖**：`message_delta` 的 usage 直接替换 `message_start` 的 usage；若该事件缺 `input_tokens`（旧版 API/中继如 OpenRouter），prompt_tokens 归零 → 成本与上下文统计失真。应字段级合并 | message_start/message_delta 两处均整体构造 json! |
| P2 | anthropic.rs:129-286 | **Anthropic 缺历史 tool round 清洗**：OpenAI 侧有 `sanitize_messages_for_native_tools`（丢弃孤立/不完整 tool 轮次），Anthropic 侧直接透传，历史中不完整的 tool_use/tool_result 会触发 API 400 | 对照 openai.rs:119-184 |
| P2 | anthropic.rs:631-641 | **Anthropic thinking_delta 被丢弃**：`content_block_delta` 仅匹配 `text_delta`，扩展思考的 `thinking_delta` 落入 `_ => {}`；请求侧也未发送 `thinking` 参数，Claude 扩展思考整体不可用 | 功能缺口，非崩溃 |
| P2 | openai.rs:463, 598 | **max_tokens 与 o 系列模型不兼容**：o1/o3/o4 需 `max_completion_tokens` 且 temperature 须省略（仅默认 1），而 factory 正好把 o1/o3/o4 前缀路由到 OpenAIProvider → 400 | 应随模型族切换字段 |
| P2 | openai.rs:602-605, 786-793 | **无条件发送 `stream_options` 与 `prompt_cache_key`**：对字段校验严格的 OpenAI 兼容服务（旧版 vLLM、部分网关）会 400；这两个参数本是 OpenAI 新特性，不应广播给所有兼容 provider | 应按 provider 能力开关 |
| P2 | openai_responses.rs:502-508 | **Responses API 无流式**：走 trait 默认实现整段转换，用户零增量体验；且 `status:"incomplete"`（截断）未映射为 `length`，上层无法识别截断 |  |
| P2 | ollama.rs:397-411 | **Ollama 原生失败即整体回退文本模式**：401/429/500 也会触发 text fallback 重试，双倍请求且掩盖错误语义（pool 的 AuthError/RateLimit 分类被吃掉） | 应仅对"不支持 tools"类错误回退 |
| P2 | openai.rs:57-101, tool_markup.rs:324-340 | **文本工具标记过滤器 O(n²)**：抑制期对持续增长的 pending（上限 1MB）每个 delta 全量扫描找闭合/候选后缀；`find_text_tool_marker_start` 对每字符做 15 个 marker 的 lowercase starts_with。大工具参数流下 CPU 浪费明显 | 可缓存扫描位置/增量 KMP |
| P2 | factory.rs:138-174 | **前缀推断路径缺配置时静默空 key**：model="claude-..." 但 `providers.anthropic` 不存在时，用 `ProviderConfig::default()`（空 api_key）继续构造 provider，直到运行时 401；显式路径有清晰报错，推断路径没有 | 错误应前移到配置期 |
| P2 | factory.rs:184-296 | **名称匹配优先于 api_type 的设计缺陷**（P0 同源）：名为 gemini/anthropic/ollama 的自定义 provider 无法通过 api_type 选择其他协议，用户配了 `api_type:"openai"` 也不生效，行为与字段语义相反 | api_type 仅在 `_` 分支生效 |
| P2 | factory.rs:309-326 | **doc20 契约不一致**：文档称 evolution 未配置时"从主 pool 取一个可用 provider"，代码用 `defaults.model` 单条创建（model_pool 配置下 defaults.model 可能是陈旧值），不经 pool |  |
| P2 | embeddings.rs:41-47, 95-97 | **blocking HTTP client 的运行时风险**：`reqwest::blocking` 若在 tokio worker 线程被调用会 panic；`dimensions()` 在首次 embed 前返回 0，向量库若在初始化期读取维度会得到 0 | 需确认调用点是否在 spawn_blocking |
| P2 | （测试覆盖）全 crate | **缺流式边界集成测试**：单测覆盖了消息/tool/usage 转换与错误分类，但四个 SSE 解析器均无"半包/断连/非法 UTF-8/[DONE] 缺失/完成标记缺失"的测试；120s 总超时、Ollama tool_calls 丢失等问题本可被此类测试拦截 | 建议用 chunk 边界模糊的 fixture 测试 |
| P3 | anthropic.rs:355-387 | 断点数守卫 `cacheable + 1 <= 4` 恒真（死逻辑），实际靠 Anthropic ≤4 断点上限侥幸合规；建议显式计数并在超限时跳过或告警 |  |
| P3 | anthropic.rs:619, openai.rs:866, gemini.rs:545 | `strip_prefix("data: ")` 不兼容 SSE 规范允许的 `data:`（无空格）；三大 API 均发带空格，低风险 |  |
| P3 | anthropic.rs:758-768 | SSE error 事件丢弃 `error.type/message`（可用于分类），mid-stream 401/429 无法被 pool 标 Dead/冷却；`input_json_delta` 在 index 未注册时静默丢弃 |  |
| P3 | anthropic.rs:739-742 | `tool_calls.into_values()`（HashMap）迭代顺序不确定，多个并行 tool_use 的最终顺序不确定（有 id 可兜底） |  |
| P3 | 各 SSE 循环 | `buffer = buffer[pos+1..].to_string()` 每行 O(剩余长度) 拷贝，宜 `drain` |  |
| P3 | tool_markup.rs:287-293 | `strip_text_tool_wrappers` 前两个 tag 是空字符串（疑似全角标签被吞），`replace("", "")` 为空操作，`<tool_calls>` 包装实际不会被剥离 |  |
| P3 | openai.rs:1508-1519 | 测试函数游离在 `mod tests` 之外（文件级裸 fn） |  |
| P3 | pool.rs:523-554 | `classify_error` 子串匹配风险：body 回显 "server error" 误分类；`insufficient_quota`(429) 未识别为 RateLimit；Anthropic 529 overloaded 未显式分类（落入 Transient 阈值，行为可接受） |  |
| P3 | docs/18:222 vs factory.rs:75-91 | fallback 顺序文档缺 qwen/xai/mistral/minimax/siliconflow；docs/18 未提及 `api_type` 字段；docs/18 glm 默认 base 与 factory `default_api_base`（bigmodel）不一致（仅配置缺省时生效） |  |
| P3 | core config/provider.rs:136-147 | `no_proxy` 默认为空：用户设全局代理后，localhost Ollama/vLLM 也被代理，只能靠 doc18 场景2 手写 noProxy 缓解；建议默认含 localhost/127.0.0.1 |  |

## 亮点

- **prompt cache 设计是最大亮点**：Anthropic 四断点中 len-4 断点专为上层 fork 对齐设计（父轮 L-2 ↔ fork 轮 L+2-4），并用单测锁定行为；OpenAI 侧 `prompt_cache_key` 用 system+model+tools 的稳定哈希做路由键，只随稳定前缀变化——正是缓存对齐需要的语义。
- **安全基线扎实**：`redact_url`（去 userinfo + 敏感 query）、错误体只记长度不记内容、Gemini key 走 `x-goog-api-key` header 且有"不进 query"回归测试、workspace 锁 `rustls-tls`（证书校验开启）、响应/流/行三级限量（32MB/32MB/1MB）与错误体 64KB 上限。
- **pool 状态机工程细致**：`report` 成功后重置连续失败、限流立即冷却、认证永久死亡、Mutex 污染恢复、`classify_error` 有针对性的误报防护测试（"API key service unavailable"不得判 AuthError）。
- 文本工具调用解析的健壮（DSML/minimax invoke/松散 map/dash args 回收 + 单测丰富），`sanitize_messages_for_native_tools` 对不完整 tool 轮次的清洗有测试。

## 建议（不执行的后续动作）

1. **P0 立即修**：factory 中 `"gemini"` 分支改为与 `_` 一致按 `api_type` 路由（或把 core 默认配置的 gemini api_base 改为 `.../v1beta`、api_type 改 `"gemini"`），并同步修正 docs/18 表格；同时复核 MiniMax 默认 api_type/base。
2. SSE 解析统一改为字节级缓冲（保留不完整 UTF-8 残包），四个 parser 共用；补半包/断连模糊测试。
3. 流式 client 从总 `timeout` 切换到 `read_timeout`（如 120s 空闲），ollama 可适当放大。
4. 二选一：实现 pool 的"冷却保底"（文档承诺），或改文档去掉承诺；顺带统一 5xx 冷却语义的文档表述。
5. 修 Ollama `convert_messages` 透传 assistant tool_calls；把原生→文本回退限定为"能力不支持"类错误。
6. Anthropic 侧补齐：历史 tool round 清洗、thinking_delta→ReasoningDelta、usage 合并、error 事件带类型分类。
7. OpenAI 兼容层：o 系列模型切 `max_completion_tokens`；`stream_options`/`prompt_cache_key` 按 provider 能力开关；Responses 补流式（或将"仅非流式"写入文档）。
8. Gemini：识别 `thought:true` parts、映射 `cached_content_token_count`。
9. embeddings：确认调用点非 async 线程（必要时 spawn_blocking）；`dimensions()` 提供维度探测或明确契约。
10. 契约层面把 api_type 优先级、evolution provider 来源、fallback 顺序在 docs/18、20、28 中与代码对齐。

## 维度结论速览

- ① 正确性：**问题集中**——半包 UTF-8（P1）、流式总超时（P1）、Gemini 默认路由（P0）、Ollama 历史 tool_calls（P1）、usage 覆盖（P2）；错误映射对 agent 重试的驱动总体正确（429→冷却、401→Dead 有测试）。
- ② 并发：客户端复用无问题（每 provider 一个 Client，内建连接池）；扣分项为流式总超时与 blocking embedder 风险。
- ③ 安全：**已检查未发现高级别问题**（key 不进日志/错误、TLS 校验开启、错误体丢弃）；仅 P3 备注 reqwest 错误链可能含代理 URL。
- ④ 性能：markup 过滤器 O(n²)（P2）、每行 buffer 拷贝（P3）；其余分配合理（raw/filtered 双缓冲系文本工具模式必需）。
- ⑤ 测试覆盖：转换/解析/分类单测充分；**缺 SSE 边界与故障注入测试**（P2）。
- ⑥ 可维护性：Responses usage 字段不归一（靠 consumer 别名兜底）、Gemini/Anthropic 缺 thinking 字段、factory name>api_type 是系统性设计问题。
- ⑦ 契约一致性：**3 处文档-代码冲突**（冷却保底、5xx 冷却语义、evolution pool 来源）+ gemini/minimax 默认配置与文档互相踩坑（P0/P1）。
