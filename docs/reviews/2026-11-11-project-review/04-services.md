# B4：Provider、渠道与调度服务审查

> 历史记录（2026-11-11）：原 24 项已于 2026-11-12 复核。当前裁决、限定、工具链验证与新增问题以 [优化指导](README.md)、[复核矩阵](08-revalidation-matrix.md) 为准；本文旧统计及 Rust 阻断说明仅代表首次审查时的结果。

基线：`5ca38be861b4a3e8f7a1e8d332e242b42ed25710`，分支 `main`。本批确认 **P1 × 1、P2 × 3**。不修改生产代码，不发起真实模型、渠道或其他外部请求。

## 模块入口与实际覆盖

| 模块 | 入口与精读范围 | 覆盖限制 |
| --- | --- | --- |
| Providers | `lib.rs` Provider trait；`factory.rs` 名称/协议/匿名本地端点选择；`pool.rs` acquire、排除已尝试实例、冷却探测、健康状态与错误分类；`client.rs` 代理与超时；`security.rs` 错误归一化/响应上限/StreamGuard。精读 OpenAI、Anthropic、Gemini、Ollama 的流式 chunk→行→事件处理、工具增量、完成和 usage；抽查 Responses 非流式返回。 | 未逐行覆盖所有提示转换/工具文本语法和 Embeddings；未接入真实供应商，未压测流量或内存。 |
| Channels | `account.rs` listener 展开、账号反向识别；`manager.rs` 出站账号选择、禁用检查、媒体和文本发送；`security.rs` 文件名/相对目录/响应限量/公共 URL；`rate_limit.rs`；Telegram、Weixin 主循环及入站 allowlist/元数据；Slack/Discord 入站账号及 chat 标识；Lark token/事件去重/投递；WeCom webhook 的 token 配置后签名关闭降级路径。 | 未逐行覆盖全部 IM 协议、NapCat、QQ、钉钉/飞书/WeCom 长连接；未做真实验签联调、DNS/redirect 测试、渠道重连或媒体往返。 |
| Scheduler | `job.rs` 持久格式；`cron_service.rs` load/merge/save、add/remove/enable、run_tick、完成回执、At/Every/Cron 下次时间、run_loop 停机；`heartbeat.rs` 触发门控和循环；`dream_service.rs` 门控调用和主循环；`consolidator/locking.rs` OS 锁归属。 | evolution/skill evolution worker、Ghost 维护和 Dream 各阶段只做入口级定位，未全量审查；未跑真实调度、崩溃恢复、DST 或并发压力测试。 |

历史报告用于核对，未将已修复的 Provider 总超时、流缺少结束标记、Gemini 默认协议路由或 cron 全程持有 operation lock 等问题直接重列。下列保留项均重新核对当前源码。

## 确认发现

### B4-01 · P1：Anthropic 正常流式响应丢失输入与缓存 Token，预算明显少计

- 位置：`crates/providers/src/anthropic.rs:713`、`:734`、`:861`；下游 `crates/agent/src/runtime/turn_flow.rs:174` / `:205`。
- 前提：使用 Anthropic Messages 原生 SSE 协议（包括相同消息格式的兼容端点），输入 usage 放在 `message_start.message.usage`，最终 `message_delta.usage` 通常只携带累计 `output_tokens`。
- 实际行为：`AnthropicStreamEvent` 没有 `message` 字段，serde 忽略整个嵌套 message；`message_start` 分支却只读取 `event.usage`。后续 `message_delta` 不是合并用量，而是整体生成新 JSON，把未提供的 `input_tokens` / cache 字段写成 null。因此最终响应的 `prompt_tokens` 为 null，读取数值时变成 0。
- 示例输入：

  ```json
  {"type":"message_start","message":{"usage":{"input_tokens":10000,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":2000}}}
  {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":10}}
  {"type":"message_stop"}
  ```

- 影响：预算 tracker 仅累计 10 个输出 token，漏掉 10,000 个输入 token；按输入/输出价格计算的费用和 prompt cache 指标也失真。反复大输入、小输出的正常回合可以消耗远超用户配置的 token/费用额度后，仍未被该 tracker 判为用尽。
- 证据类型：静态协议/结构体/分支/下游完整调用链。示例为协议格式推演，没有实际向 Anthropic 请求，也未声称预算端到端测试已运行。
- 建议：反序列化 `message_start.message.usage`，将后续 usage 按字段合并，缺失字段保持原值，输出累计值覆盖同名字段；同时测试原生完整 SSE 和只有输出字段的 delta。
- 历史对照：2026-10-16 Provider 报告已提到 usage 整体覆盖；当前进一步确认标准 `message_start` 的嵌套结构根本未进入模型，当前仍存在，而非只针对个别旧中继。

### B4-02 · P2：四个流式适配器逐网络 chunk 解码，中文或工具参数发生不可恢复乱码

- 位置：`crates/providers/src/openai.rs:871`、`anthropic.rs:611`、`gemini.rs:544`、`ollama.rs:490`。
- 前提：UTF-8 字符的多字节序列跨 HTTP body chunk 边界。HTTP chunk 边界与字符、JSON 行和 SSE 事件边界没有对齐保证。
- 实际行为：每次都先 `String::from_utf8_lossy(&bytes)`，再拼接 String 缓冲。分开的 UTF-8 残片立即变成 U+FFFD，之后即使收到剩余字节也不能还原。
- 影响：中文/emoji 回复、含非 ASCII 的工具参数被损坏；损坏发生在 JSON 字符串内部时，JSON 本身仍可能解析成功，从而把错误路径/内容传给工具，流式完成标志和字节上限不能识别这种错误。
- 动态证据：在临时目录运行与当前四个适配器相同的两行缓冲代码，将 SSE 文本中 `中` 的 3 字节在第 1 字节后拆开。结果：

  ```text
  Reconstructed SSE: data: {"choices":[{"delta":{"content":"���"}}]}
  Parsed content: '���'
  ```

  独立 Rust 程序的 `assert_ne!(buffer, event)` / U+FFFD 断言通过，随后 Python `json.loads` 也成功解析并确认内容已变化。没有运行 HTTP 请求或完整适配器。
- 建议：先缓存原始字节，在收到完整行/事件后进行严格 UTF-8 解码，或使用保留残片的增量解码器；对所有可能的字符拆包边界做协议测试。
- 历史对照：2026-10-16 同项已存在，目前四个位置仍未改为字节残片缓冲。按本轮等级定义，分包条件下的数据正确性问题归 P2。

### B4-03 · P2：cron 等待前一任务执行完成才派发后续任务，并推迟停机

- 位置：`crates/scheduler/src/cron_service.rs:503`、`:723`、`:865`；生产完成回执超时为 `:17` 的 15 分钟。
- 前提：两个任务同时到期，前一个 Agent/skill 任务执行较慢或一直没有完成回执。
- 实际行为：`run_tick` 对每个 job 串行调用并 `await execute_job_internal`；该函数的返回条件包含 runtime 完成回执，远超“成功入队”。第二个任务因此不会立即投递。`run_loop` 在 `select!` 的 tick 分支中直接 await 整个 run_tick，期间无法选择 shutdown 分支。
- 影响：单个慢任务可延误同服务全部定时任务，多个无回执任务串联时延误可累计；正常停机也要等这些等待结束。这里不再声称写 API 被 operation lock 阻塞：当前代码已在投递前释放该锁，`add_job/remove_job` 的这部分历史缺陷已修复。
- 证据类型：静态 async 控制流及 15 分钟超时常量；没有等待真实 15 分钟或执行生产任务。
- 建议：有界并发派发任务并独立处理完成回执，对每个 job 保持 in-flight/幂等状态；在停机时停止派发并显式取消/回收等待。不要只把所有 future 放开并发而留下同一 job 可重复触发的窗口。
- 历史对照：2026-10-16 的 cron 长任务问题已部分修复（释放 operation lock），但串行执行及等待期间无法响应 shutdown 的部分仍存在。

### B4-04 · P2：出站媒体发送失败被吞掉，企业微信长连接连文字也一起丢失

- 位置：`crates/channels/src/manager.rs:334`、`:507`、`:517`、`:522`、`:627`。
- 前提：配置相应渠道且媒体发送返回错误，例如附件本地文件已删除，或上传接口失败。企业微信长连接下，消息同时含文字 caption 与附件。
- 实际行为：多个渠道把 `send_media_message` 的 Err 只写日志，最后仍返回 `Ok(())`。企业微信长连接把文字作为第一个附件 caption，附件发送失败后仍清空 `remaining_caption`，再仅因 `media` 非空而 `skip_text=true`，没有发送文字的补救路径。
- 影响：媒体全部失败时调度调用者仍收到成功；企业微信上述情况可能完全没有任何内容到达收件人，文字也静默丢失。上层 `gateway/outbound.rs:24` / CLI 出站循环依赖 Err 判断失败，无法得知本次分发失败。
- 证据类型：静态明确错误返回分支，无真实上传测试。此项不声称消息存在持久投递保证；问题是现有 `Result<()>` 已把确定失败转换成成功，且主动跳过正文。
- 建议：返回失败或明确的部分成功结果，并在长连接附件失败时保留/回退发送 caption。覆盖“media-only 全失败”和“有 caption 的第一个附件失败”两个用例。

## 跨批次核对与待验证

- 已向 B2 审查单元确认多账号同 chat_id 的真实可达场景：Telegram 同一用户私聊两个 bot、Discord 同频道两个 bot；入站 `account_id` 不同、`chat_id` 相同，`core/message.rs:21` 将 account 编入 session key。TaskInfo 归属检查若只看 channel/chat 会丢失隔离，此项由 **B2** 报告计数，B4 不重复列入。
- Lark `is_duplicate` 在 `process_webhook` 成功投递前即写入进程级 seen 集合，且 key 仅 event_id；失败后的重试以及多账号 key 作用域需进一步做受控 handler 测试，本批未计入确认数。
- WeCom 当前在配置 callback token 后会拒绝未签名/未加密的 POST，已核对该历史降级绕过已修复；未配置 token 时的兼容模式仍需按产品要求明确，不把配置允许的兼容行为直接断言为同一已修问题回归。
- Provider Pool 的冷却、主动 probe、重试退避需要结合 Agent 的实际调用节奏验证，本批没有把独立 `acquire_probe_excluding` 的设计直接判为限流绕过。

## 验证与后续顺序

本批只运行无依赖的 UTF-8 分包最小程序，未运行 Cargo（工具链阻断详见 B0）、真实 SSE/IM 或调度器测试。建议先补齐 Anthropic 计量契约，随后统一流式字节解码，再处理 cron 等待/停机和出站失败传播；每组均需针对上述具体行为补回归测试。
