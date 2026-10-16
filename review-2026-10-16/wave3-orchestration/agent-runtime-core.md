# agent runtime 核心 Review 报告（Wave 3 · 编排层）

> 只读审查 · 2026-10-16 · 子代理 w3-runtime-core
> 审查对象：`crates/agent/src/runtime.rs`（顶层单文件）+ 其直接依赖的辅助代码（为核对不变量，只读参照了 `runtime/` 子模块与 `context.rs`/`crates/providers/src/anthropic.rs`/`crates/tools/src/shell_session.rs` 的相关片段；这些文件的深度审查归其他子代理，本报告仅记录与 runtime.rs 强相关的结论）。

## 规模（文件数/LOC/入口）

| 项 | 值 |
|---|---|
| 主文件 | `crates/agent/src/runtime.rs`，**2703 行 / 98,737 B**（注：任务描述称"约 10K+ LOC"有误；实际 2.7K 行。"8.5 万行变更"是近半年 churn，非当前体量） |
| 文件内声明的子模块 | 21 个（`agent_loop / compaction / event_listeners / fork_spawn / ghost_boundary / lightweight_handle / message_dispatch / message_task / prefetch / pre_step / process_message_inner / process_message_phases / run_loop / skill_session / subagent / subagent_provider / tool_exec / tool_pipeline / turn_flow / wiring / learning / path_security`）——runtime.rs 已从巨石文件演变为"脊柱 + 21 子模块"结构 |
| 单测 | `runtime/tests.rs`（`mod tests;`，runtime.rs:2702-2703），规模约 7.5K+ 行，覆盖 fork/不变量的测试密集（见下） |
| 入口 | `AgentRuntime::new`（runtime.rs:2296）→ `run_loop`（runtime/run_loop.rs:4，长生命周期循环：inbound/tick/steering 监管）→ `fork_for_message_task`（runtime.rs:2477）→ `run_message_task`（runtime/message_task.rs:110）→ `process_message`（runtime/message_dispatch.rs:330）；取消树 `AbortToken`、steering 每消息新建（run_loop.rs:390-407） |

## 模块地图（这个文件的职责结构、主要类型与调用链）

**主文件（runtime.rs）的职责结构**（按行段）：

| 行段 | 职责 |
|---|---|
| 1-99 | imports + 21 个子模块声明 + 重导出 |
| 101-185 | `ToolCallOutput` / `ToolLoopGuard`（回合内工具调用上限 + 同一调用连续失败熔断） |
| 186-244 | `ReviewMode`（**死代码，见问题 #8**）、三个 Review prompt 常量 |
| 246-449 | `CompactContext` / `ProviderLLMAdapter` / `RuntimeSpawnHandle`（子代理 spawning）/ `ConfirmRequest` / 截断与错误分类 helper |
| 450-592 | prompt 注入：`strip_fake_tool_calls`、`inject_skill_cards_into_system_prompt`、`append_ephemeral_user_context`（`<runtime-context>` 合成 user 消息，去重合并逻辑正确） |
| 594-768 | `inject_running_tasks_into_system_prompt`（后台任务可见性注入）、token 用量提取 |
| 795-1149 | skill 历史持久化 helper、run mode/cron 投递解析 |
| 1151-1215 | `expand_history_stubs_with_cache`（response_cache ref 展开）、spawn task 强制 skill 解析（`__SKILL_EXEC__:` 编解码） |
| 1217-1532 | skill 结果呈现、`RuntimeSessionSearch`（**同步 fs 扫描，见问题 #6**）、system event 渲染、ghost schema 规范化 |
| 1557-1696 | `resolve_effective_tool_names`（intent router → 工具集）、路径判定、工具失败软判定 |
| 1731-1799 | `InteractionDecision`、`persist_session_key_for`（cron 投递归属，与事件日志一致） |
| 1803-1931 | `RuntimeSessionSearch` impl + 图片路径拾取（异步、正确） |
| 1994-2105 | 假 tool_call 剥离（**问题 #1**）、薄搜索结果检测、危险操作文本确认（**问题 #2**）、`load_path_policy` |
| 2107-2184 | toggles 加载（含未被热路径使用的 `load_disabled_toggles_pair`，问题 #4）、`load_disabled_toggles_pair` |
| 2186-2293 | **`AgentRuntime` 结构体（40+ 字段）**：shared（Arc/Mutex/clone）与 per-message 隔离字段混排 |
| 2295-2468 | `new()`：组装 event_bus/hooks、system event store（**持久化**，与文档 12 声称矛盾，问题 #11）、response cache、learning coordinator、budget trackers、steering |
| 2470-2554 | **`fork_for_message_task`**：每消息执行器派生（核心不变量所在） |
| 2556-2699 | `init_runtime_handle`、`install_shared_learning_coordinator`、`create_evolution_deploy_callback`、`Drop`（MemorySystem 归还共享池） |
| 2702 | `mod tests;` |

**主要调用链**（主路径）：
```
channels/CLI → inbound_tx → run_loop
  ├─ tick_interval → process_system_event_tick / workers.notify() / reload_skills / cap cooldown 入队
  ├─ steering 路由（同会话进行中消息转 steering，Full 时拒新）
  └─ 每条消息: update_main_session_target → create_and_start_task → fork_for_message_task
       → tokio::spawn(run_message_task) + supervise_message_task（panic→set_failed，cancel→receipt cancel）
            → process_message → pre_step → agent_loop → execute_tool_call → tool_pipeline 六段准入
```

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| **P1** | runtime.rs:2000-2010 | `strip_fake_tool_calls` 用 `to_lowercase()` 副本上的字节偏移切割原字符串，`to_lowercase()` 的 Unicode 折叠可能改变字节长度（如 `İ`→`i̇` 2B→3B），偏移错位可触发切片 panic（越界或非 char 边界） | `let start = result.to_lowercase().find("[tool_call]")` 后在 `result` 上切；同文件 `should_supplement_tool_schema`(1699) 用的是安全的 `to_ascii_lowercase`。模型输出含 `[TOOL_CALL]` + 特定 Unicode 即复现；tokio task panic 被 `supervise_message_task`(message_task.rs:13-20) 兜住标记 failed，不会拖垮进程，但整回合失败。另每次循环整体 `to_lowercase()` 拷贝，多块时为 O(n²)。修法：`to_ascii_lowercase` + find 同源，或 regex case-insensitive |
| **P2** | runtime.rs:2086-2097（`user_explicitly_confirms_dangerous_op`，经 tool_pipeline.rs:299-305 `approve_dangerous_operation` 消费） | 非交互通道（confirm_tx=None）的危险操作确认是**纯文本关键词匹配，不与具体操作/回合绑定**：任何含"确认"+(执行/重启/继续/允许) 的用户消息会批发放行该回合内**所有**危险操作（rm -rf 也放行）；反之用户上一轮说"确认"、下一轮才出现危险调用则又不认 | 这是 confirm_tx 缺失时的设计回退（注释写明），但缺少操作绑定与一次性消费（如确认后作废/仅绑最近一条待确认操作）。建议最小修复：要求确认词与操作在同一消息且确认后一次性失效 |
| **P2** | runtime.rs:2265（`session_memory_systems`，插入点 runtime.rs:2695 Drop、process_message_inner.rs:415-424） | **共享 session→MemorySystem 池只进不出、无驱逐**：每个出现过的 session key 永久驻留一份 MemorySystem（含 FileTracker/SkillTracker/cursor），gateway 长跑随会话数单调增长；同会话并发 fork 互相覆盖还会丢一份 Layer4 状态 | 对照 `SessionContextCache` 有 `MAX_CACHED_SESSIONS` 容量（context.rs:355,379）。建议加容量上限 + LRU 驱逐，驱逐前 flush |
| **P2** | runtime.rs:2284（`budget_trackers`，插入点 turn_flow.rs:29-43） | 同一问题：`budget_trackers` 共享 HashMap 只插入不驱逐，每会话一个 `BudgetTrackerHandle` 永久驻留 | 单条目小（计数器），严重度低于上一个，但同属无界共享态。建议随 session 生命周期清理或设上限 |
| **P2** | runtime/pipeline 调用方 tool_pipeline.rs:308-318 + runtime.rs:2145-2184 | `guard_disabled_toggles` 每次工具调用 **2 次读盘+JSON 解析** toggles.json（tools、skills 各一次）；专为热路径准备的 `load_disabled_toggles_pair`（runtime.rs:2169，注释自述"避免同一文件连续读两次"）在这条路径上**未被使用**（只在 skill_session.rs:13、process_message_inner.rs:216 使用） | 每个工具调用 2 次同步 fs；改调 pair 即可零风险修复 |
| **P2（latent）** | runtime.rs:2523（fork 中 `candidate_scope: None`）+ 2551（`sync_tool_scope_view("")`） | fork **静默丢弃** candidate_scope：进化候选作用域（candidate-scoped 工具/listener/prompt 段，wiring.rs:257-263、test tests.rs:6896）在消息执行器上全部不可见。当前 `set_candidate_scope` 仅测试调用（"None in production"），无现实危害；但一旦候选评估走消息路径，评估将静默运行在生产作用域 | fork 文档（runtime.rs:2470-2476）列了共享/隔离字段，candidate_scope 两者都不属，行为未定义。建议：显式传播 `self.candidate_scope`（并同步 context_builder 的 candidate_id，context.rs:437） |
| **P3** | runtime.rs:1818-1900（`RuntimeSessionSearch::search_session_json`，tool_pipeline.rs:518 注入） | 同步 fs：一次搜索遍历 sessions 目录全部 jsonl 全量读入；`results` 向量先无界累积（每条命中最多 500 字符）后才 sort+truncate，多会话长历史下内存与阻塞都很可观；且是阻塞 IO 跑在 async 工具路径里 | 建议 `tokio::fs` + 文件数/命中数双重上限 + 流式淘汰 |
| **P3** | runtime.rs:332-340（`RuntimeSpawnHandle::spawn` 内 `SessionStore::load` 同步读盘） | spawn 子代理时同步加载 origin 历史全量 JSONL + `expand_history_stubs_with_cache` 逐条 clone，阻塞 IO 在 async 工具执行路径 | 单文件量级可忍，但应改 tokio::fs 或移出热路径 |
| **P3** | runtime.rs:1757-1775（`determine_interaction_mode`，`#[cfg(test)]`） | 测试专用副本与生产判定（turn_flow.rs:418-422：仅 `Coding contains→Coding else General`；forced skill 分支在 turn_flow.rs:388-415）**已漂移**：helper 里的 `Chat`/`Skill` 分支生产并不产生（该函数还漏掉 forced_skill 路径）。用它写的新测试会验证过期语义 | 建议删除 helper，测试直接对生产 `resolve_interaction_decision` 断言 |
| **P3** | runtime.rs:194-202 | `enum ReviewMode`（私有）是 `learning_coordinator::ReviewMode`（lib.rs:107 重导出）的**死重复**：本文件零引用（生产消费方 runtime/learning.rs:8 显式 import crate 根版本，遮蔽 glob）。属废弃残留，且两个同名类型是维护陷阱 | 删除本枚举，统一用 `learning_coordinator::ReviewMode` |
| **P3** | runtime.rs:160 | 工具拒绝语拼写 `execeeded`（用户可见文案） | 改 `exceeded` |
| **P3** | docs/12_architecture.md:88-89 | 文档称 system event store 与 summary queue"还是进程内内存，重启后不会恢复"——**代码已持久化**（runtime.rs:2334-2341 `with_persistence(.system_events.json/.system_summary_queue.json)`，wiring.rs:705 还有 7 天过期清理） | 文档滞后于代码（代码方向正确），更新文档 |
| **P3** | docs/12_architecture.md:26,32 | `RuntimePool` / `AgentRouter` 在代码中不存在（全仓仅 docs 命中）；多 agent 实际由 gateway 持有多个 AgentRuntime + `channelAccountOwners` 路由实现 | 架构图组件名与实现脱节，建议改名为实际组件 |
| **P3** | docs/12_architecture.md:362 | 文档称 `authorized_dirs: HashSet<PathBuf>`——实际为 `HashSet<(PathBuf, PathOp)>`，按操作类型（读/写）分别授权（runtime.rs:2203、path_security.rs:191） | 文档与类型漂移 |
| **P3** | docs/13_message_processing_and_evolution.md:105-168 伪代码 | 文档描述"同步自进化：report_error 计数超阈值→切断执行流→内联管线→hot reload→带提示重试"；实际是**后台 workflow store 入队 + worker notify**（run_loop.rs:530-590、capability_adapter），无同步切断与内联重试 | 伪代码与现行 worker 架构脱节，重写为 worker 模型 |
| **P3** | runtime.rs:2233-2235 注释 | 注释称 `shared_session_target` "also held by LightweightRuntimeHandle"——实际持有者是 `InProcessSubagentProvider`（subagent_provider.rs:17,37）；`LightweightRuntimeHandle` 不持有它（lightweight_handle.rs:10-13） | 注释指向错误类型，小改动 |

**汇总：P0 × 0，P1 × 1，P2 × 5，P3 × 10。**

## 已知不变量逐条核对结论

1. **fork 共享/隔离名副其实——✅ 成立（有一处 latent 缺口，见 P2 latant 条）**。
   - 共享（Arc::clone/Clone 语义共享）：`response_cache`（测试 tests.rs:7289-7309 证明父 `/clear` 对 fork 可见）、`evolution workers`（skill_evolution_worker/evolution_workflow_store，tests.rs:7271-7277）、`learning_coordinator`（runtime.rs:2531）、`authorized_dirs: Arc<Mutex>`（runtime.rs:2496，tests.rs:7311-7314）、`budget_trackers: Arc<Mutex>`（runtime.rs:2535，tests.rs:7316-7323）、`session_memory_systems`（runtime.rs:2527，tests.rs:7363-7396 归还池）、`write_guard`/`skill_mutex`（2532-2534）。
   - 隔离：`abort_token` 为调用方传入的父 token child（runtime.rs:2529，run_loop.rs:401-407，tests.rs:7266-7269 验证取消树）、`steering/steering_sender` 每消息新建（run_loop.rs:390）、`current_turn_id/current_step/turn_step_counter` 归零（runtime.rs:2520-2522，tests.rs:7264-7265）。
   - 状态泄漏排查：`cap_request_cooldown` fork 重置为**正确**（tick 只在长生命周期 runtime 上跑，run_loop.rs:532-590 用的是父实例的 map，fork 永不跑 tick）；`main_session_target` 值拷贝 + `shared_session_target` Arc 双写（wiring.rs:531-541），长生命周期侧在 fork 前已更新（run_loop.rs:323），两侧各自新鲜，未见陈旧读取；`memory_file_store/skill_file_store` 为 None 时 fork 内惰性重建（runtime.rs:2541-2550），仅首条消息多一次初始化，非泄漏。
   - 锁安全：`authorized_dirs`/`budget_trackers`/`session_memory_systems` 均为 std Mutex、guard 只覆盖同步 O(1)/O(n) 操作，**未发现跨 await 持锁**（process_message_inner.rs:414-424 的 lock 与 `init_memory_system().await` 分属不同语句）；poison 均 `unwrap_or_else(into_inner)` 恢复。
2. **fork 与 response_cache/context 缓存算术对齐——✅ 与已知遗留一致**。ContextBuilder 手写 Clone 共享 `session_cache`/`skill_index_summary`/`shared_memory_injector` 三个 Arc（context.rs:414-419，测试 tests.rs:7325-7359）；Anthropic 断点为算术对齐：`messages[len-2]` 常态断点 + `messages[len-4]` fork 对齐断点（anthropic.rs:370-386，注释明确"父轮 L-2 与 fork (L+2)-4 对齐"，测试 anthropic.rs:977-1002）。确认这是**算术而非逻辑边界**：若 fork 历史未恰好追加 2 条消息，len-4 断点落在任意消息上（ API 允许但缓存前缀最优性失效）；`messages.len()==4` 时标记 index 0。与已知遗留描述完全吻合，无新增偏差。
3. **`execution_environment`（描述性）vs `requires_confirmation`（准入性）正交——✅ 成立**。准入判定只读 `requires_confirmation`（tool_pipeline.rs:390-399），环境仅用于确认文案与事件/审计记录（tool_pipeline.rs:400-411、174-177、275-288）；两者不得互相推导有测试锁定（registry.rs:1097 `test_requires_confirmation_is_declared_per_call_not_by_environment`、tests.rs:5422"同一 Shell 环境下只有 requires_confirmation=true 才触发确认"）。确认状态 `policy_confirmed/dangerous_confirmed` 按**单次调用**作用域正确隔离（tool_pipeline.rs:28-33、156-162 每次调用重建），且 dangerous 批准不写会话级授权缓存（path_security.rs:245-249），未发现跨调用确认绕过。
4. **shell 会话一次 poll 累积所有轮询输出——✅ 成立**。`poll` 跨多个 `poll_once` 周期用 `append_capped` 累积（shell_session.rs:455-480、917-933），UTF-8 安全截断；回归测试覆盖"先到批次不丢"（shell_session.rs:1131-1170）、"累积上限不劈开 UTF-8"（1172-1182）、"超时后进程仍可 poll"（1190+）、"reap 后未读输出仍可取"（1499-1514）。

## 亮点

- **不变量测试化程度高**：fork 的每一项共享/隔离都有对应断言（tests.rs:7242-7360 一个测试覆盖 6 条不变量），MemorySystem 归还池、skill 四路径跨 fork 可见性均有专测；这是本仓最热文件能维持正确性的关键投资。
- **工具管线分层清晰且审计完备**：六段准入（Toggle→PreExecute→Policy→Dangerous→PathAccess→ExplicitConfirmation），被拒调用也产生 durable 的 tool/result + audit（tool_pipeline.rs:215-220、235-261）；pre-execute 先于所有安全检查，保证检查的是 listener 改写后的最终参数。
- **Unknown ≠ Failed 的对账提示**（tool_pipeline.rs:117-123）：超时/中断不盲目重试，先查状态——避免重复副作用，这是容易被忽略的正确性细节。
- **取消/监管闭环**：消息任务 panic→`set_failed`、cancel→receipt cancel（message_task.rs:11-29）；同会话新消息自动取消旧任务并清理 token 防泄漏（run_loop.rs:370-388）；子代理 abort token 为 fork 的子节点（subagent_provider.rs:42），级联取消名副其实。
- **无 panic 面控制良好**：主文件 4 处 expect 全在 LazyLock 正则编译期与测试；所有 Mutex poison 均恢复而非 unwrap；`Box::pin` 包住大 future 防主循环栈溢出（tool_pipeline.rs:145-147）。
- **UTF-8 安全意识**：`truncate_str`、`append_capped` 均按 char 边界切割。

## 建议（按优先级）

1. **P1 立即修**：`strip_fake_tool_calls` 改用 `to_ascii_lowercase`（或正则 `(?i)`），消除切片 panic 与 O(n²) 拷贝；补一条含 `İ`+`[TOOL_CALL]` 的回归测试。
2. **P2 批次**：
   - `guard_disabled_toggles` 改调 `load_disabled_toggles_pair`（一次读盘喂两个集合）；
   - `session_memory_systems` / `budget_trackers` 加容量上限 + LRU 驱逐（参照 `SessionContextCache` 的 `MAX_CACHED_SESSIONS`），同会话并发 fork 覆盖时先 flush 再弃；
   - 非交互通道文本确认改为**操作绑定 + 一次性消费**（确认仅对最近一条待批准操作生效一次）；
   - fork 显式传播 `candidate_scope`（含 context_builder 的 candidate_id），并补一条 fork 保留 candidate scope 的测试——在候选评估上线前堵住 latent 坑。
3. **可维护性（维度⑥结论：文件已从巨石拆分为脊柱+21 子模块，2.7K 行的"脊柱"仍偏大但可接受；进一步拆分建议）**：
   - prompt 注入族（450-768 行：`inject_skill_cards_into_system_prompt`/`append_ephemeral_user_context`/`inject_running_tasks_into_system_prompt`/Review prompts）→ `runtime/prompt_assembly.rs`；
   - `RuntimeSessionSearch` + `pick_image_path` + `expand_history_stubs_with_cache` → `runtime/session_search.rs`；
   - `RuntimeSpawnHandle` + `__SKILL_EXEC__` 编解码 + `normalize_spawn_task` → 并入 `runtime/subagent_spawn.rs`（`run_subagent_task` 已在 subagent.rs）；
   - `AgentRuntime` 40+ 字段建议按关注点分组为子结构体（`EvolutionHandles` / `SessionCaches` / `SteeringState` / `SecurityState`），可同时缩小 `fork_for_message_task` 的字段核对面（当前 60 行逐字段拷贝，漏字段无编译保护——分组后漏拷风险下降）；
   - 删除死枚举 `ReviewMode`（runtime.rs:194-202）与漂移的 `#[cfg(test)] determine_interaction_mode`（1757-1775）。
4. **P3 清理**：`execeeded` 拼写；`shared_session_target` 注释改指 `InProcessSubagentProvider`；同步 fs 两处（session search、spawn 历史加载）改 `tokio::fs`。
5. **文档（维度⑦结论：整体方向一致，4 处漂移需更新）**：docs/12 的"事件存储纯内存"→已持久化；`RuntimePool/AgentRouter`→改实际组件名；`authorized_dirs` 类型→`(PathBuf, PathOp)`；docs/13 自进化伪代码→改为 worker/入队模型。docs/12 的 crate 结构、SystemEventOrchestrator tick、目录级授权、意图→工具按需加载等主体描述与代码一致。

## 维度覆盖说明

- ① 正确性：已检查（问题 #1 panic、#3/#4 无界态、#9 语义漂移）；未发现状态机死分支或吞错误模式（错误均经 `record_unexecuted`/`set_failed`/`warn!` 落地）。
- ② 并发：已检查（跨 await 持锁、锁粒度、panic 兜底、取消树）；未发现死锁；阻塞 IO 问题记 #6/#7。
- ③ 安全：已检查（沙箱/确认绕过、路径穿越、prompt 注入面）；路径策略 canonicalize + skill 相对路径逃逸拦截有效；确认绑定缺陷记 #2。
- ④ 性能：已检查（问题 #4/#5/#6 重复分配与无界缓冲、#1 O(n²)）。
- ⑤ 测试覆盖：runtime 相关测试密集（fork 六不变量、shell poll、确认正交、candidate 隔离均有专测）；缺口：fork-candidate、新建议的 Unicode panic 回归。
- ⑥ 可维护性：见"建议 3"。
- ⑦ 契约一致性：见"建议 5"（4 处文档漂移，主体一致）。
