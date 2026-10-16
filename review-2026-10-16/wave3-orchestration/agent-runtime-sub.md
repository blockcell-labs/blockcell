# agent runtime/ 子目录 Review 报告（Wave 3 · 编排层）

> 只读审查 · 2026-10-16 · 子代理 w3-runtime-sub

## 规模（文件数/LOC/入口）

- **范围**：`crates/agent/src/runtime/`（不含 `runtime.rs` 顶层与 `forked/`）
- **文件数**：23 个 `.rs` 文件，合计 **18,958 LOC**（其中 `tests.rs` 7,503 行，占 40%；生产代码约 11.5K LOC）
- **模块树入口**：`crates/agent/src/runtime.rs:68-99`（`#[path]` 式 mod 声明 + re-export），`runtime.rs:2703` 挂载 `mod tests`
- **最大文件**：`tests.rs`(7.5K) > `process_message_inner.rs`(2.1K) > `learning.rs`(1.2K) > `tool_pipeline.rs`(879) > `path_security.rs`(821)
- **测试量**：176 个 `#[test]`/`#[tokio::test]`

## 模块地图（管线各阶段/文件职责表）

| 文件 | LOC | 职责 |
|---|---|---|
| `agent_loop.rs` | 34 | `AgentLoop` trait + `DefaultLoop`（contract/driver 分离） |
| `message_task.rs` | 215 | 消息任务驱动：`run_message_task`（进程/失败投递）、`supervise_message_task`（panic→Failed） |
| `run_loop.rs` | 608 | 主 select 循环：入站分发、steering 路由、取消（AbortToken+abort）、resume、维护 tick（清理/热加载/能力进化） |
| `message_dispatch.rs` | 398 | `call_llm_with_retry`（流式累积、provider 切换、退避策略）、`process_message`（turn/end 兜底关闭）、steering drain |
| `process_message_inner.rs` | 2123 | **核心 turn 编排**：早返回快路径、历史加载、意图决策、工具集解析、主循环（LLM→工具→提交→提示注入）、子代理等待汇总、延迟 Review、post-sampling 记忆/压缩 |
| `process_message_phases.rs` | 203 | cron 提醒快路径、手动 `/compact` |
| `pre_step.rs` | 217 | 每步决策门：budget → compaction → context 注入/节流 → 可注册策略链（fail-open） |
| `tool_pipeline.rs` | 879 | **Guarded Tool Pipeline**：6 阶段准入（Toggle→PreExecute→Policy→DangerousOperation→PathAccess→ExplicitConfirmation）→ 执行（sandbox 提权重试）→ post-execute → 归类 → 结果/审计/事件 |
| `tool_exec.rs` | 229 | 工具执行入口（ghost 生命周期优先）、Layer1 大结果持久化、ToolPolicy 评估与审计、shell 提级判定 |
| `path_security.rs` | 821 | 路径提取（按工具/动作/字段）、workspace/authorized/policy 三级判定、确认通道、shell/exec 命令内嵌路径启发式提取 |
| `prefetch.rs` | 117 | 只读工具并发预取（白名单+幂等+policy Allow+路径安全才预取） |
| `turn_flow.rs` | 761 | provider 调用与预算、技能内循环 `run_prompt_skill_steps`、意图决策、最终响应持久化与多渠道投递 |
| `skill_session.rs` | 401 | 技能会话构建、summary-only 收尾、legacy Rhai 兼容执行（含独立安全门） |
| `compaction.rs` | 661 | Layer4 压缩：recent 消息工具组完整性、熔断、hook、恢复信息、事件 |
| `learning.rs` | 1228 | Nudge Review（fork 缓存对齐）、Ghost learning 边界捕获（turn-end/pre-compress/session-end/rotate）、负反馈自动禁用技能、subagent 受限工具集 |
| `subagent.rs` | 424 | `run_subagent_task`（隔离 runtime+受限 registry）、结果回传、ghost delegation 学习 |
| `subagent_provider.rs` | 792 | `InProcessSubagentProvider`：Fork（前台、句柄丢弃即取消）/ Typed（后台、TaskManager、worktree 隔离） |
| `wiring.rs` | 765 | setter/依赖注入、权限集、step/turn 生命周期、系统事件 tick、memory store 初始化 |
| `event_listeners.rs` | 329 | WS 工具事件转发、Durable Session Event Log 落盘（spawn_blocking） |
| `ghost_boundary.rs` | 104 | ghost 学习边界持久化（纯函数） |
| `lightweight_handle.rs` | 46 | 避免 Arc<Self> 循环的轻量运行时句柄 |
| `fork_spawn.rs` | 100 | typed workspace scope 解析、typed 结果归一化、fork prompt 消毒 |
| `tests.rs` | 7503 | 见测试覆盖评估 |

**管线主干**（对应 docs/13）：`run_loop` 收消息 → `tokio::spawn` per-message forked runtime → `process_message`（turn/end 兜底）→ `process_message_inner` 主循环（`pre_step` 门 → `call_llm_with_retry` → 工具批次 `execute_tool_call_prefetched` → 准入 6 阶段 → 执行 → 结果提交/截断/提示注入）→ post-sampling（Layer3/5 提取 + Layer4 压缩）→ 持久化投递。

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| **P1** | `process_message_inner.rs:806-807,809-829` + `runtime.rs:795-834` | **activate_skill 与同批其他调用时，被跳过调用的 tool_use 在持久化 history 中悬挂**（无配对 tool_result） | 模型同批返回 `activate_skill`+其他调用时：assistant msg（含全部 tool_calls）先 push 进 `history`（806-807），其余调用仅 `record_unexecuted_tool_call`（820-826：只写审计+事件，不写 history），随后 `append_activated_skill_history` 只为 activation_call_id 补 tool_result。结果 session 文件中遗留"assistant 声明了 N 个 tool_use、只有 1 个 tool_result"的畸变历史。`crates/providers/src/anthropic.rs:210-218` 逐条发送 tool_use 且无配对过滤，下一轮 Anthropic 请求会 400（tool_use 无 tool_result）；OpenAI 系同样要求配对。`context.rs:1236-1270` 的 safe-start 只清理**前缀**悬挂组，救不了中段。触发条件不罕见（代码专门 warn 处理），且会话会持续中毒直到压缩。无测试覆盖（grep 确认 tests.rs 无对应场景）。 |
| **P1** | `learning.rs:583-607` | `coordinate_boundary_job` 用 `expect("boundary job was just enqueued")` 收尾，存在 panic 路径 | enqueue 4 个 output 后立即 `take_boundary_job`；若同一 `LearningCoordinator` 被并发使用（`fork_for_message_task` 共享 coordinator，见 tests.rs:2942 用例；跨 runtime/session 同 key 场景），take 可能返回 None → panic。该函数在 `capture_pre_compress/session_end/rotate` 边界被调用，panic 会 unwound 整个 turn future（由 `supervise_message_task` 兜底为任务失败，但回合已崩）。应改为 `unwrap_or_else`/返回 Option。生产代码中 `runtime/` 唯一的裸 expect（其余 unwrap 均在 tests）。 |
| **P2** | `turn_flow.rs:267-320` + `runtime.rs:816-833`（对照 `process_message_inner.rs:1198-1253`） | **技能内循环工具结果无 Layer1 预算/截断**：主循环对超限结果先落盘再截断（50k 字符上限），技能循环（`run_prompt_skill_steps`）直接把 `tool_output.text` push 进 `current_messages` 和 `trace_messages`，而 trace 会经 `append_activated_skill_history`/`persist_prompt_skill_history` 原样持久化进 session history | 技能跑多轮大输出（web_fetch/grep）时，单轮即可把 MB 级文本写进会话文件；且 `pre_step.rs:75-87` 对技能循环传 `history: None`，Layer4 压缩也被跳过，上下文只受 `max_tool_iterations` 约束。主循环有完整三层防护（预算→落盘→截断→压缩），技能循环零防护——明显的设计缺口。 |
| **P2** | `path_security.rs:154` | MCP 工具（`server__tool` 命名）完全不经 `extract_path_accesses`，PathAccess 阶段对 MCP filesystem 类服务器形同虚设 | match 的 `_ => {}` 兜底；MCP 工具由用户配置的服务器提供，信任边界虽在配置文件，但运行时应保持与内置工具一致的 workspace/策略约束，目前仅能依赖 ToolPolicy 规则逐条封堵。建议至少对已知文件类 MCP 服务器做 path 参数提取或文档明示豁免。 |
| **P2** | `message_dispatch.rs:140-148,206-209` | 流式汇总的 `tool_calls` 从 `HashMap<String, ToolCallAccumulator>` drain/into_values，**批内工具执行顺序不确定** | 主循环按 `response.tool_calls` 顺序串行执行（`process_message_inner.rs:941`）。HashMap 迭代序随机 → 同一对话重放时工具执行顺序不可复现；对同批内有顺序依赖的副作用（如先写后读、两条 message 发送）构成隐患。Provider 流里的 `index` 字段被显式忽略（`index: _`），本可用来保序。 |
| **P2** | `run_loop.rs:100-139,145-187,370-388` | 取消/顶替路径只 abort tokio task，不主动终止该任务已启动的 shell 会话 | cancel 指令、同会话新消息顶替旧任务时：`token.cancel()+handle.abort()+remove_task`，无 `shutdown_shell_processes` 式清理（仅循环退出时调用，`run_loop.rs:81`）。`shell_session.rs:240` 有 `kill_on_drop(true)`，但 abort 只 drop 任务 future，session 条目仍存活于全局 manager，子进程可能继续运行到会话被清理。属用户显式取消后的资源泄漏。 |
| **P2** | `path_security.rs:456-559` + `runtime.rs:2086`（邻接文件，引用说明） | exec/shell 命令内嵌路径提取是启发式，可被 `eval`、二次展开、编码等手段绕过；非交互通道的危险操作确认是关键词匹配（"确认"∩("执行"/"重启"/"继续"/"允许")） | 前者代码注释已自认 best-effort（sandbox 才是主 containment，可接受，但应文档化边界）；后者对否定句（"不要确认执行"）会误判为同意。均为安全边界收窄项而非直接漏洞（有 sandbox + 工具层二次校验兜底）。 |
| **P3** | `learning.rs:1612-1618` | deferred review snapshot 选取用"长度比较"推断新旧：压缩使 `current_messages` 变短时会回退到旧快照 | 启发式在绝大多数情况正确；压缩后 current 更短 → 用旧 snapshot（偏安全），但压缩+新增混合场景可能选错。建议显式版本号/代数标记。 |
| **P3** | `process_message_inner.rs:1186-1194` | schema 补齐回滚靠 `current_messages.pop(); history.pop();` 假设"最后一条一定是刚 push 的 assistant msg" | 当前控制流下成立，但属于隐式结构契约，未来在批次循环内插入 history 写入即静默错位。建议记录索引或校验 role。 |
| **P3** | `subagent_provider.rs:608-631` | `TypedHandle::result()` 无限轮询（200ms）无超时、无取消感知 | typed 子代理卡死时调用方永久挂起；建议加 deadline 或 AbortToken 检查。 |
| **P3** | `process_message_inner.rs:1417-1467` | 等待子代理完成用 500ms 轮询 + 硬编码 300s 上限 | busy-poll 可用 Notify/oneshot 替代；硬编码值应进配置。 |
| **P3** | `compaction.rs:479-482` | `post_compact_tokens` 只统计 summary+recovery，不含 `recent_messages`，压缩率指标系统性低估 | 仅影响指标展示与 `compact_completed` 事件。 |
| **P3** | `message_dispatch.rs:135-137` | `ToolCallDelta` 在对应 `ToolCallStart` 之前到达被静默丢弃 | 部分 provider 乱序分片时参数丢失且无告警。 |
| **P3** | `message_dispatch.rs:50,274` | `excluded_provider_indices` 跨 retry attempt 累积不重置，attempt 2 可能直接无可用 provider | 收敛行为正确但重试质量下降。 |
| **P3** | `run_loop.rs:276-321,370-388` | 带媒体消息/斜杠命令源绕过 steering 直接顶替（abort）在途任务 | 有日志但无用户侧提示；媒体跟发是常见场景，建议至少在投递端说明"已终止上一任务"。 |

### 已检查未发现问题的维度

- **① 正确性（状态机/吞错/panic）**：准入阶段机与 `ADMISSION_STAGES` 常量一一对应；`ToolOutcome` 六态分类与 `evolution_signal`（仅 Succeeded/Failed 计入进化观察窗）清晰；Unknown 结局有对账提示（`tool_pipeline.rs:118-123`）。未执行调用全部以 durable tool/result 收尾（6359 用例验证）。除上述 P1 expect 外无生产 unwrap/panic 点。槽点集中在 P1/P2/P3 列表。
- **② 并发**：消息级 forked runtime + per-message AbortToken 链式取消设计清晰；`authorized_dirs`/`budget_trackers` 等 Mutex 均用 poison 恢复；event listener 文件 I/O 走 spawn_blocking；`spawn_review` fire-and-forget 且包在子 AbortToken 内。未见锁跨越 await、未见 async 中阻塞调用（legacy Rhai 路径正确包在 spawn_blocking + 独立线程 `block_on`）。
- **④ 性能**：Box::pin 防主循环 future 膨胀、prefetch 并发只读、Cow 避免每轮 clone 全 history、`append_ephemeral_context` 零拷贝优化、schema 缓存标志防冗余 reload——优化点密集且到位。主要缺口是 P2 技能循环无预算。
- **⑥ 可维护性**：模块拆分合理（管线/路径/学习/子代理/事件边界清晰），模块头文档齐全，中文注释解释了关键设计决策（cache 对齐、dangerous_confirmed 语义、persist_session_key cron 归属）。扣分项：`process_message_inner` 仍 2.1K 行单函数，超长方法内注释密度高；`run_subagent_task` 27 参数（已 `#[allow]`）。

## 亮点

1. **安全管线的 orthogonal 设计落实彻底**：`requires_confirmation`（准入）与 `execution_environment`（描述/审计）分离，`dangerous_confirmed` 明确"本次放行但不写会话缓存"（`tool_pipeline.rs:341-364`、`path_security.rs:245-257`），且有 3 个专门测试锁定该语义（tests.rs:5380/5421/5460）。
2. **可审计性**：每个提议的 tool call（含被拒/未开始）都有 tool/result 事件 + 审计 + session event log（tests.rs:6359 逐条断言）；审计带 turn/step/call_id 追踪。
3. **pre-execute listener 先于所有 guard**，保证检查的是改写后参数（tests.rs:6121 专项验证）。
4. **path_security 分层判定**（Deny > workspace > session cache > Allow/Confirm）+ 命令内嵌路径启发式提取（含 `$VAR`/反引号/内联脚本的细致处理，17 个单测覆盖）。
5. **取消安全体系完整**：AbortToken 树（runtime→message task→subagent→review）、`scope_abort_token`/`scope_agent_context` 包裹，panic 有 supervisor 兜底为 Failed。
6. **测试质量高**：176 用例覆盖管线各阶段、ghost learning 四类边界、cron 投递归属、熔断、流式重试/回退、fork 缓存兼容（learning.rs:1154 防回归断言），且无宿主状态前置跳过的反模式。

## 建议

1. **（P1）修复 activate_skill 批处理历史畸变**：在 `append_activated_skill_history` 前，把同批被跳过的调用从 assistant msg 的 `tool_calls` 中移除（或为其补 `{"error": NotStarted}` tool_result），并加一个持久化 history 配对完整性测试。
2. **（P1）去掉 `learning.rs:606` 的 expect**：改为 `if let Some(job) = take(...)` 或返回 Option 向上传递。
3. **（P2）技能循环接入 Layer1 预算/截断 + Layer4 压缩**：把主循环 1198-1253 的截断逻辑提取为共享函数供 `run_prompt_skill_steps` 复用；或让 pre_step 对技能循环也允许压缩（传入 history）。
4. **（P2）流式 tool_calls 保序**：用 `Vec<(id, accumulator)>` 或按 `index` 排序替代 HashMap drain。
5. **（P2）MCP 工具路径策略**：至少在 `extract_path_accesses` 为已知 MCP 文件服务器补充分支，或在 `docs/03_tools_system.md` 明示 MCP 豁免边界。
6. **（P2）取消路径杀 shell**：message task abort 前调用 task_manager 的 shell 清理（或注册 per-task 清理回调）。
7. **契约补充**：`docs/13` 伪代码暗示"进化修复后自动重试"，实际是注入 learning_hint 让 LLM 自行决策——建议文档加一句说明，避免读者预期偏差；`docs/03` 安全机制一节可补充 ExplicitConfirmation/sandbox 提级描述，与代码对齐。

## 已知不变量逐条核对结论

| 不变量 | 结论 |
|---|---|
| `execution_environment`（描述/审计）vs `requires_confirmation`（准入）正交 | ✅ **符合**。`tool_pipeline.rs:390-429` 仅按 `requires_confirmation` 拦截；environment 只用于确认项展示与审计（`tool_pipeline.rs:400-411,275-288`）。tests.rs:5310/5345/5380/5421/5460 五用例锁定。 |
| ExplicitConfirmation 阶段位于 `runtime/tool_pipeline.rs` | ✅ **符合**。`AdmissionStage::ExplicitConfirmation` 定义于 `tool_pipeline.rs:63`，为 6 阶段准入最后一道（67-74）。 |
| shell 会话一次 poll 累积所有轮询输出（delivered_offset）且受 MAX_SESSION_OUTPUT_BYTES 限制 | ✅ **符合**（实现位于 `crates/tools/src/shell_session.rs`，超出本审查范围，抽查核对）：`poll` 以 `delivered_offset.max(start_offset)` 为起点返回增量（508-551），截断时置 `truncated`（526/546）；累积逻辑在 917-928 受 1MiB 上限约束并有单测（1174-1178）。 |
| fork 缓存算术对齐（fork len-4 ↔ Anthropic 断点，遗留待办） | ⚠️ **本范围内不存在该算术**（runtime/ 与 agent 包 grep 无 `len()-4`/断点相关代码），属 `forked/` 或其他代理范围，无法在本报告确认；但 `learning.rs:124-156` 的 CacheSafeParams 兼容性校验 + `record_fork_alignment_miss_from_fork` 度量表明对齐机制在 forked/=层，请 forked 审查代理核对。 |
| tests 不用宿主状态做前置判断（如 `native_backend != ApprovalRequired` 静默跳过） | ✅ **符合**。runtime/tests.rs 无 `native_backend`/`ApprovalRequired`/平台 cfg 跳过模式，176 用例全部无条件执行。 |

## 契约一致性（docs/03、docs/13）

- **无矛盾**。docs/13 §1"每消息 task_id + tokio::spawn 后台执行、主循环不卡顿"与 `run_loop.rs:327-424` 一致；§2 意图→按需工具与 `process_message_inner.rs:243-310` 一致；§4 `report_error` 触发进化与 `tool_pipeline.rs:707-784` 一致。docs/03 的路径安全四级模型（workspace 自动/工作外确认/已授权缓存/Gateway 拒绝）与 `path_security.rs:224-394` 一致。
- 两篇文档为系列科普文，属"简化子集"而非规格说明；工具管线 6 阶段、ghost learning、steering、prefetch 等新机制未入文档属正常滞后，已在建议中列明需补充的点。
