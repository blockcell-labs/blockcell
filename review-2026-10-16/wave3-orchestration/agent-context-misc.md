# agent 其余部分 Review 报告（Wave 3 · 编排层）

> 只读审查 · 2026-10-16 · 子代理 w3-agent-misc
> 范围：crates/agent/src/{compact/, intent/, runtime_kernel/, session_metrics/, skill_file_store/, task_manager/, context.rs, ghost_background_review.rs, agent_loader.rs, lib.rs, 其余小型顶层文件}；另按不变量要求核对了 runtime/learning.rs:61（Nudge Review 对齐链）。runtime.rs、runtime/ 其余部分、forked/、记忆家族由其他代理负责，本报告仅在交叉验证处引用。

## 规模

| 范围 | 文件数 | 行数（约） |
|---|---|---|
| context.rs | 1 | 2,583（其中测试 ~1,100） |
| ghost_background_review.rs | 1 | 1,676（其中测试 ~800） |
| agent_loader.rs | 1 | 1,034 |
| runtime/learning.rs（不变量核对） | 1 | 1,228（读前 220 行） |
| compact/ | 7 | 2,348 |
| session_metrics/（含 memory/layer1-7） | 11 | 3,649 |
| runtime_kernel/ | 5 | 836 |
| intent/ | 1 | 325（+ 顶层 intent.rs 1,443，已读核心逻辑） |
| skill_file_store/ | 1 | 200 |
| task_manager/ | 1 | 255 |
| 小型顶层文件（bus/error/fs_util/health/token/steering/write_guard/agent_loader 周边等） | ~17 | ~3,600 |
| lib.rs | 1 | 177 |

合计审查约 15.5K 行（含测试），命中 ~15K 上下文预算。

## 模块地图

- **context.rs** — ContextBuilder：7 层记忆注入、PromptBudgetAllocator 预算装配、按 session 的冻结快照（file memory / system prompt / retrieval 去重 / 项目环境 / plan 增量）与 256 条 LRU，手写 Clone 共享三个 Arc（session_cache / skill_index_summary / shared_memory_injector）。安全历史起点 `find_safe_history_start` 处理截断后的孤儿工具消息。
- **compact/** — compactor.rs（ModelSummary/Extractive 双策略 seam）、summary.rs（9 段结构化摘要 + LLM 生成 + Markdown 解析）、hooks.rs（Pre/Post 注册表 + SessionMemory 恢复 hook）、mod.rs（预算恢复消息 + 二分截断）、recovery.rs（**生产未接线的恢复上下文，见 P2-1**）、file_tracker.rs / skill_tracker.rs（读文件/技能追踪）。
- **ghost_background_review.rs** — Ghost 回合后台 Review：lease 租约（600s）+ 心跳（180s）+ AbortToken 取消（写前检查）、受限工具循环（memory_manage/session_search/skill_view 白名单）、owner-aware 落库、RAII ShutdownGuard。
- **session_metrics/** — 7 层记忆系统全原子指标（OnceLock 全局 + AtomicU64）、memory_event! 宏（结构化日志 + 计数双轨）、三档熔断器（compact 60s / extraction 300s / dream 900s）、CLI 摘要快照。
- **runtime_kernel/** — EventBus（优先级、作用域过滤、per-listener 超时、panic 隔离、fail-open/closed）、Event 语义分类（Durable/Live/Capability）、Waterfall 串行拦截链、scope 原语（复用 blockcell_core::scope）。
- **intent/**（+ intent.rs） — 16 类意图分类（关键词+正则+否定词，预小写）、配置扩展规则（无效正则降级跳过）、IntentToolResolver（profile→工具集，deny 过滤）、完备性校验（Unknown 强制、工具名必须已注册/已声明 MCP/napcat 可选）。
- **agent_loader.rs** — Markdown+YAML frontmatter Agent 定义加载（内置→用户级→项目级覆盖），路径遍历防护、类型名格式校验、部分失败容错。
- **token.rs / fs_util.rs** — tiktoken + 保守回退 + 指纹记忆化（16K 上限 wholesale 清理）；跨平台原子写（Windows backup 策略 / Unix rename）。
- **小型顶层文件** — skill_kernel（6 行枚举）、skill_decision（技能名归一化）、task_notification（payload）、bus.rs、error.rs（工具失败分类）、steering.rs（lib.rs 内有测试）、write_guard.rs（跨进程 lockdir + 进程内 RwLock 统一写守卫）、unified_security_scanner.rs（learned 内容按 TrustLevel 扫描）、skill_summary.rs、skill_nudge.rs（阈值配置）、agent_prompts/agent_progress/health/prompt_skill_executor/skill_executor 等。

## 问题清单

| 级别 | 位置 | 问题 | 证据/说明 |
|---|---|---|---|
| P1 | ghost_metrics.rs:175-200, 217-229 | `record_fork_prompt_cache_usage_from_fork` / `record_fork_alignment_miss_from_fork` 在 path_hint 不匹配任何已注册 base 时，把事件**扇出记录到所有已注册 GhostMetrics 实例** | 设计缺陷：fallback 应为"告警+丢弃"而非广播。实际 blast radius 有限——生产单工作区进程 path_hint 必命中；仅测试或多工作区网关场景会跨实例重复计数（污染所有实例的 token 聚合） |
| P2 | compact/recovery.rs:123-257 | `create_recovery_context` / `generate_recovery_message` / `CompactRecoveryContext` 全套（~200 行）**生产零调用**（仅自身 tests 引用；live 路径是 mod.rs 的 `build_recovery_message` + FileTracker/SkillTracker） | 死代码内含潜在 bug：`was_modified: false` 硬编码（:147，注释自认"需要更精确的检测"）使 `truncate_files_to_budget` 的排序成为 no-op；`create_recovery_context` 遍历 `HashMap`（:138）顺序不确定后再 truncate(5)，同尺寸文件选择不确定。一旦有人接线即引入不确定性 |
| P2 | compact/hooks.rs + runtime/compaction.rs:283,503 | Compact hook seam 已接线但**生产零注册**：`CompactHookRegistry` 在 MemorySystem 中存在且 execute_pre/post_hooks 被调用，但无任何生产代码调 `compact_hooks_mut()` 注册 hook（仅 tests/memory_integration_test.rs:642-655）；`DefaultPreCompactHook`（compact 前等待后台任务）与 `SessionMemoryRecoveryHook` 全程 `#[allow(dead_code)]` | seam 就绪但预期行为未激活。对安全性无影响（review fork 用消息快照，compact 不破坏进行中的 fork），影响的是"compact 前等待后台任务完成"的摘要质量策略 |
| P2 | ghost_background_review.rs:335-357 | `run_pending_background_reviews` 串行处理 episode，且循环内 `GhostLedger::open(&paths.ghost_ledger_db())?`（:357）一旦失败即 `?` 返回，**剩余已 claim 的 episode  lease 不被释放**，只能等 600s 过期 + `cleanup_expired_leases` 回收 | 吞吐与故障恢复问题；已处理的 episode 不受影响（:411/438 正常 release） |
| P2 | context.rs:441-477 | `incremental_project_environment` 每次消息构建（含工具循环内**每次 LLM 调用**）无条件执行 2 次 git 子进程（`branch --show-current` + `status --porcelain`），快照未变化也照跑；20 轮工具循环 = 40 次子进程 spawn | 性能设计问题。commands 已有缓存（project_command_cache）但 git 状态查询没有；正确性不受影响（快照比较避免重复注入） |
| P2 | 顶层布局 | crates/agent 124 文件中 **49 个在顶层**；task_manager（80KB .rs + 255 行 task_manager/）、skill_file_store（60KB .rs + 200 行 /）、intent（52KB .rs + 325 行 /）三组**同模块双位置并存** | seam 抽取方向正确且子模块内聚（persistence/patch/builtin_rules 都是单一职责），但 x.rs + x/ 两名并存损害可导航性。task_manager/ 255 行本身不过薄——它是内聚的持久化单元，问题在于与 80KB 主文件割裂而非目录本身太薄 |
| P3 | context.rs:1203 | `_is_image_path` 死代码（下划线前缀、无任何调用） | 应删除 |
| P3 | context.rs:135 | 注释过时："Skill 索引摘要…注入到系统提示词"已不成立——skill_index_summary 现仅作为 retrieval 输入（:973-977 及 retrieval.rs:214），不进 frozen system prompt，其内容变化也不触发快照失效（因不在指纹中，正确） | 注释与实现漂移，易误导后来者以为改 skill 列表会影响系统提示词前缀 |
| P3 | docs/13:73 | "预设的 14 种意图" vs `IntentCategory` 15 个变体（14 + Unknown） | 文档漂移（可能 Blockchain 后补时未更新） |
| P3 | circuit_breaker.rs:128-133 | `allow()` 的 Open→HalfOpen 为 check-then-act（无 CAS），极端并发下两线程可同时转换，half_open_max_calls=1 可能多放行 1 个请求 | 良性竞争，metrics 熔断场景可接受；如需严格可用 compare_exchange |
| P3 | compact/summary.rs:297 | `else if !line.starts_with("*") \|\| !line.ends_with("*")` 会把正文中本身以 `*` 开头且以 `*` 结尾的内容行误判为描述行丢弃 | 罕见（模型输出以斜体行开头的正文段落）；可在 add_section 前对空内容加保护 |
| P3 | context.rs:664-675 + runtime/learning.rs | ① `frozen_file_memory_snapshot` 会话内永不失效——用户会话中途编辑 USER.md/MEMORY.md 对本会话不可见（有测试 :2182 锁定为"有意冻结"，仅记录备查）；② `take_changed_plan_context` 为破坏性 take，构建中途放弃则该轮 plan 增量丢失 | 均为设计权衡，非缺陷，记录供决策 |
| P3 | session_metrics/memory/layer7.rs:37-56 | `record_spawned` 递增 active_count，若 fork 因 abort 等原因既不 completed 也不 failed（无终止记录），active_count 泄漏 | 指标漂移；建议 abort 路径统一 record_failed |

## 亮点

- **ghost_background_review.rs 是并发正确性标杆**：lease 租约 + 周期心跳 + 连续失败取消 + AbortToken 在每次 provider 调用/工具执行前检查（写前校验，测试 `ghost_review_stops_after_lease_loss_before_memory_write` 验证不落盘）；owner-aware 落库（`insert_review_run_if_review_owned` / `update_episode_status_if_review_owned`）防止跨 worker 覆盖；claim 前清理过期 lease；RAII `ShutdownGuard` 防 `?` 提前返回泄漏 provider。禁用工具在执行前拦截并记 failed（测试验证 `write_file` 未执行）。10+ 测试覆盖全部分支。
- **context.rs 冻结快照体系严谨**：system prompt 指纹含 mode/工具集/规则/capability_brief/prompt_registry.fingerprint/agent_id/candidate_id/active skill 字段；`reload_skills` 发现新技能时清 `system_prompt_snapshots`；agent-scoped 段注册后冻结提示失效（测试 :2547）；`find_safe_history_start` 正确处理孤儿 tool 消息与不完整 tool_calls 组；runtime-context 以 user 消息插在最后一条 user 之前（稳定前缀利于 prompt cache，测试 :2405 锁定）。20+ 测试。
- **compact seam 干净**：ContextCompactor 只负责"如何摘要"，编排（hooks/熔断/恢复/事件）在 runtime；二分截断 UTF-8 安全（char 边界）；恢复预算分层强制（测试验证总量不超预算）；跨边界确认 runtime/compaction.rs 的 `select_recent_messages` 与 context.rs 同构处理工具配对——压缩后消息语义保持链完整。
- **token.rs**：tiktoken + 保守回退（过估防溢出）+ 消息指纹记忆化（单锁整批、16K 上限），并有"缓存=未缓存"一致性测试。
- **intent 路由注入面收敛良好**：配置正则编译失败仅跳过+告警（无 panic）；工具解析恒定经 deny_tools 过滤；validate_with_mcp 校验完备（Unknown 强制、profile 引用、工具名已注册/已声明 MCP/napcat 可选豁免）。
- **EventBus**：panic 隔离（catch_unwind）+ per-listener 超时（默认 30s）+ fail-open/fail-closed + 作用域过滤 + RAII scoped 注册，行为测试齐备。
- **session_metrics**：全原子无锁（concurrent access 测试），`reset` 用 SeqCst 并文档化同步点理由；memory_event! 宏结构化日志与计数解耦。
- **agent_loader**：路径遍历检查、类型名 3-50 字符白名单、CRLF 归一化、部分成功容错（成功定义+错误分别上报）。

## 建议

1. **删除或接线 compact/recovery.rs 死代码**：若确认 `build_recovery_message` 为唯一 live 路径，整文件移除；若保留，至少实现 `was_modified` 真实检测并将 `read_files` 改为有序结构（Vec/BTreeMap）消除选择不确定性。
2. **ghost_metrics 扇出 fallback 改为"告警+不记录"**（或 path_hint 为空时显式指定单实例），杜绝跨实例污染。
3. **incremental_project_environment 增加节流**：如 30s 时间窗内复用上次 git 输出做增量比较，或改用 `.git` 目录 mtime 触发，削减工具循环内的子进程开销。
4. **收敛顶层布局**：对 task_manager / skill_file_store / intent 三组统一策略——要么薄目录内容合回主文件，要么主文件继续拆分，避免 x.rs + x/ 长期并存；lib.rs 已按模块字母序维护，可维持。
5. 清理 `_is_image_path` 死代码与 context.rs:135 过时注释；docs/13 意图数量同步为 15。

## 已知不变量逐条核对

| 不变量 | 结论 | 证据 |
|---|---|---|
| ContextBuilder 手写 Clone 共享三个 Arc（session_cache/skill_index_summary/shared_memory_injector），快照失效逻辑 | ✅ 成立 | context.rs:392-425 Clone 中三个字段均 `Arc::clone`；`skill_manager` 字段从 shared 重新同步（防 clone 后各自 set 漂移）。失效：`reload_skills` 新技能时清 `system_prompt_snapshots`（:641-645）；`frozen_system_prompt` 指纹含 `prompt_registry.fingerprint()`/agent_id/candidate_id（:686-701），测试 `agent_scoped_prompt_section_overrides_global_and_invalidates_frozen_prompt`（:2547）验证 |
| Nudge Review 对齐链 learning.rs:61（父快照首条 system 原样进 fork） | ✅ 成立 | runtime/learning.rs:61-68：`messages.first().filter(role=="system")` 原样取出，注释"与 Hermes 一致：review_agent 直接复用父快照首条已冻结/已注入的 system message"；仅当首条非 system 时回退 `build_system_prompt()`。fork label 三档（skill/memory/combined_nudge_review）与 ghost_metrics 分桶一致 |
| fork metrics 已按 label 分桶 | ✅ 成立 | ghost_metrics.rs:26 `fork_prompt_cache_usage: Mutex<HashMap<String, ForkPromptCacheUsageSnapshot>>`，:78-98 按 fork_label 累加；:166-167 文档注释列明 review/auto_memory/compact_summary/session_memory/fork/typed subagent 全按 label 汇入；测试 :256-293 验证路径前缀匹配路由。注意：分桶职责在 ghost_metrics，session_metrics Layer7 仅计数——与 docs/27 描述不冲突（27 未要求 Layer7 分桶） |
| Episode Review「人格不一致」未立项（已知遗留） | 未发现新证据，状态不变 | ghost_background_review.rs 受限 review 按 docs/27:124 设计为 memory-only（`REVIEW_ALLOWED_TOOLS` 明确排除 skill_manage，测试 `restricted_review_prompt_is_memory_only` 锁定 "Do not create, edit, or request skills"）；全文件无 persona/人格相关代码路径。遗留问题仍为开放状态，本范围无新增线索 |

## 契约一致性（docs 核对）

- **docs/21_intent_router_profiles.md ↔ intent.rs：完全一致**。文档 9 条规则逐条对得上实现：① agent.intentProfile > agentProfiles > defaultProfile 解析优先级；② enabled=false+loadAllTools=true 返回全量工具再扣 denyTools（:310-328）；③ enabled=false+loadAllTools=false 走 Unknown profile（:338-342）；④ enabled=true 忽略 loadAllTools；⑤ 每 profile 必须配 Unknown 否则校验失败（:428-436）；⑥ intentRules 追加不覆盖、无效正则跳过告警（:189-201）；⑦ category 未知/重复均跳过+告警（:174-188）。
- **docs/27_ghost_learning_design.md ↔ ghost_background_review.rs：一致**。skill_manage 排除（:684-687，docs:124/458）、memory-only reviewer（:690-704）、shadow/durable 写入路径与测试断言吻合（user scope 写 USER.md + KnowledgeIndex，测试 :1359 验证新会话可召回）。
- **docs/13_message_processing_and_evolution.md ↔ 实现：一致（一处漂移）**。消息分发/意图/工具循环/自进化四阶段描述与 runtime 编排相符；仅"14 种意图"与 15 个枚举变体漂移（P3）。

## 维度覆盖说明

- ① 正确性：context 构建边界（token 预算/截断策略）与 compact 语义保持（工具调用配对）已重点核对，工具配对在 compact 前后两处（context.rs find_safe_history_start、runtime/compaction.rs select_recent_messages）同构正确；无 P0/P1。
- ② 并发：metrics 锁（全原子，无锁竞争）、ghost 后台任务生命周期/取消（租约+心跳+abort）均已核对；ghost_metrics fallback 扇出为 P1。
- ③ 安全：intent 路由注入面（配置正则降级、deny 恒定过滤）、agent_loader 动态加载（路径遍历/名校验）、ghost review 工具白名单执行前拦截、write_guard 跨进程锁——均已核对，未发现安全漏洞。
- ④ 性能：git 子进程节流缺失（P2）、active_count 泄漏（P3）、token 记忆化与预算二分截断为正面项。
- ⑤ 测试覆盖：范围内各模块内联测试齐备（context 20+、gbr 10+、compact 每文件、metrics 宏级、event_bus 行为、agent_loader、token、write_guard、ghost_metrics），集成测试覆盖 compact hooks；未发现明显缺口。
- ⑥ 可维护性：顶层文件过多与双位置布局（P2）、recovery/hooks 死代码（P2）、注释/文档漂移（P3）——task_manager/ 255 行本身内聚不过薄。
- ⑦ 契约一致性：三份文档逐条核对，docs/21 与 docs/27 完全一致，docs/13 一处数量漂移。
