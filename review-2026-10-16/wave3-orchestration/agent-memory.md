# 记忆家族 Review 报告（Wave 3 · 编排层）

> 只读审查 · 2026-10-16 · 子代理 w3-memory
> 范围：crates/agent/src/{auto_memory/, memory_system/, session_memory/, memory_file_store.rs, skill_index.rs} + 关联读取 context.rs / runtime.rs / runtime/process_message_inner.rs / runtime/learning.rs（不变量核对所需）

## 规模

| 模块 | 文件数 | 行数 |
|---|---|---|
| auto_memory/ | 6 | 3070（cursor 731 / extractor 902 / injector 450 / memory_type 486 / scanner 448 / mod 53） |
| memory_system/ | 1 | 1558 |
| session_memory/ | 4 | 1557（extractor 718 / template 438 / recovery 339 / mod 62） |
| memory_file_store.rs | 1 | 871 |
| skill_index.rs | 1 | 1039 |
| **合计** | **13** | **8095** |

测试：各文件均有 `#[cfg(test)]`，含并发写（12 线程）、prompt 注入拒绝、遗忘 tombstone、panic 清理、冷却/内容变更等用例，覆盖面良好。

## 模块地图（Layer1-4 映射到文件）

| 层 | 职责 | 文件 |
|---|---|---|
| Layer 1 内容替换 | 状态容器（实现块在 response_cache，范围外） | memory_system/mod.rs:91（`content_replacement` 字段） |
| Layer 3 Session Memory | 触发判断 + Forked Agent 提取 + 模板校验 + Post-Compact 恢复等待 | session_memory/{mod,extractor,recovery,template}.rs；状态机在 memory_system/mod.rs |
| Layer 4 Compact 恢复 + 集成器 | MemorySystem（游标管理器、FileTracker/SkillTracker、journal/marker 互斥、Post-Sampling 编排 evaluate_memory_hooks） | memory_system/mod.rs（单文件 1558 行） |
| Layer 5 Auto Memory | 游标三重冷却、forked 提取、威胁扫描、注入器（死代码，见 P1-1） | auto_memory/{mod,cursor,extractor,injector,memory_type,scanner}.rs |
| 持久文件层（规范事实源） | USER.md/MEMORY.md 原子写、快照、tombstone、KnowledgeIndex 同步 | memory_file_store.rs |
| 提示词共享机制 | Skill 索引摘要 + frozen 快照缓存（与记忆注入共用 session_cache/快照失效逻辑） | skill_index.rs；context.rs（snapshot/fingerprint） |

数据流：auto_memory 提取 → 旧 Layer5 文件（config_dir/memory/*.md）→ `consolidate_legacy_layer5`（memory_type.rs:219-293，经 MemoryFileStore 带索引同步）→ USER.md/MEMORY.md → ①MemoryInjector（**无人消费**）②FrozenFileMemorySnapshot（**会话内不失效**）③KnowledgeIndex（每次 consolidate 经 store.add 同步，新鲜）。

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| P1 | auto_memory/injector.rs:128,154；context.rs:498-509；runtime/learning.rs:419-505 | **MemoryInjector 全链路死代码**：加载、热更（signal→reload→set_memory_injector）、Arc 跨消息共享一应俱全，但 `inject_into_system_prompt`/`build_injection_content` 全仓无生产调用方（仅 injector.rs 自身测试 + memory_type.rs:473 测试）。实际注入走 RetrievalOrchestrator + FrozenFileMemorySnapshot。docs/05:224 称"运行时注入器只读取两个规范长期文件"，暗示注入器就是运行时注入路径——**契约与实现不符，Layer 5 注入功能实际静默失效** | grep 全 crates 仅 context.rs set/get、learning.rs init/reload；`memory_injector()` 唯一生产用途是 `is_none()` 检查（process_message_inner.rs:433） |
| P1 | context.rs:664-675, 210-216 | **FrozenFileMemorySnapshot 无失效钩子**：USER.md/MEMORY.md 每会话读一次后冻结，自动记忆 consolidate、memory_manage 写入均不刷新；检索注入（retrieve_with_snapshot）因此使用旧内容直到 LRU 淘汰或重启。近期修复为技能热更清了 system_prompt_snapshots，但记忆文件无对应失效机制——正是"记忆注入变化时快照是否失效"的缺口 | `file_memory_snapshots` 仅在 touch/evict 处变更（context.rs:210）；无任何 invalidate 调用点 |
| P1 | runtime.rs:2265, 2692-2700 | **session_memory_systems 池无上限**：每个会话累积一个 MemorySystem（cursor_manager、watch 通道、trackers），父 runtime 生命周期内永不释放。与容量受限的 SessionContextCache（MAX_CACHED_SESSIONS + LRU 淘汰）不对称；Gateway 长生命周期场景下随会话数线性增长 | 池仅 insert/remove，无 evict/cap |
| P2 | session_memory/extractor.rs:395-505 | **Session Memory 提取无安全扫描**：Layer 5 提取后有 `scan_learned_memory_content` + 回滚，Layer 3 完全没有；session memory.md 内容经 `generate_recovery_message`（recovery.rs:254-272）进入 compact 恢复消息回流模型上下文——同一注入面上不对称的防护 | 对比 extractor.rs:326-357（Layer 5 扫描+回滚） |
| P2 | cursor.rs:59-101 | **CrossProcessLock::acquire 在 async fn 中同步阻塞**：`std::thread::sleep(200ms)` 最多 30 次（≈6s）阻塞 tokio worker；进程内 Mutex 先行串行化使命中率低，但跨进程持锁时会在 current_thread runtime 上拖死无关任务 | merge_and_save/save 均为 async fn 直接调用 |
| P2 | runtime/process_message_inner.rs:408-430 | **池借用在并发同会话消息下降级**：消息处理期间实例被 remove 出池（互斥正确），但第二个同会话 per-message runtime 取空后会 `init_memory_system` 新建第二个 MemorySystem；双实例仅靠文件 marker（.extraction_pending + journal owner_pid）互斥，watch 通道/cursor_reload_flag 按实例隔离——"池保证单实例"只在串行消息下成立 | 414-430 行逻辑 |
| P2 | runtime.rs:2694-2697 | **Drop 归还的毒化路径**：`if let Ok(mut map)` 失败（mutex poison）时 MemorySystem 被直接丢弃而非归还——后台提取任务 abort + .active 清除，仅 debug 日志，静默降级 | 2694-2697 |
| P2 | extractor.rs:595-812 | **废弃 extract_auto_memory ~200 行重复实现**：与 extract() 逻辑重复，且**完全不推进游标**（若误用将导致无限重复提取）；已有 #[deprecated]+#[allow(dead_code)] 缓解，但仍是 footgun | 595-812 行 |
| P2 | 测试缺口 | 池语义（借用/归还/毒化）、快照失效、consolidation→检索可见性均无测试；memory_system/mod.rs 测试集中于 config/compact，Pool 逻辑零覆盖 | tests.rs:7388 仅断言字段存在 |
| P2 | context.rs:896-902 | **PromptBudget 契约疑点**：`build_system_prompt_inner` 向 PromptBudgetAllocator 传入空 user_profile/retrieved/active_skill/session_recovery，而 docs/05:196-200 声称检索内容"服从全局 Prompt Budget"；检索段在 build_runtime_context 单独组装（上限 20 条）。预算是否真正全局执行需与 retrieval 模块交叉核对（retrieval.rs 不在本次范围，标记待核） | 896-902 vs docs/05 |
| P3 | memory_system/mod.rs:1179-1193 | **is_pid_alive 平台不一致**：memory_system 版用 `/proc/{pid}`（macOS 无 /proc → 恒 false，journal owner 存活检测退化为纯 3x 时间阈值）；cursor.rs 版用 `kill -0`（macOS 正常）。本机开发平台恰是 macOS，静默降级 | 1183-1186 |
| P3 | cursor.rs:288-307 | **check_content_change 边界**：等长改写（length delta=0 < 阈值）永不触发提取；先缩后长的净变化相互抵消 | 301-303 |
| P3 | memory_system/mod.rs:922 + extractor.rs:21-34 | **build_message_content_signature 每轮全量拼接**：evaluate_memory_hooks 每次评估拼接全部消息内容成字符串再哈希，超长会话下为 O(历史字节) 热点 | 922；extractor 内再算一次 |
| P3 | auto_memory/scanner.rs | **命名误导**：scanner.rs 实为威胁模式扫描器（injection/jailbreak/exfil 关键词），与"文件扫描"直觉相反；扫描职责与 `unified_security_scanner` 并存两套 | 150-190 |
| P3 | 代码 vs docs/05 | **术语漂移**：代码用 Layer3/4/5 配置与事件、注释引用"7 层记忆系统设计文档"；docs/05 用三子系统模型（Context/Knowledge/Learning）且无 Layer 分层描述，所引设计文档不在 docs/ 内 | docs/05 全文无 "Layer 1-4" 描述 |
| P3 | memory_type.rs:219-293 | **auto_memory 仍以旧 Layer5 文件为主要写入目标**，靠 consolidate 汇入规范文件；docs 把旧文件定位为迁移源、未描述这条"提取→旧文件→consolidate→规范文件"写路径，链路依赖隐式排序（consolidate 必须先于注入读取） | 289 行重置 legacy 为模板 |

## 已知不变量逐条核对结论

1. **Layer 4 会话级共享池（session_memory_systems + Drop 归还）——基本成立，三处瑕疵**。借用语义正确：消息开始时 `remove`（process_message_inner.rs:420-424）保证处理期间独占；归还在 `AgentRuntime::drop`（runtime.rs:2692-2700）insert 回池，池中实例不触发 MemorySystem::drop（.active 标记保留、后台任务不误 abort），设计自洽。瑕疵：①毒化时丢弃而非归还（P2）；②并发同会话降级为双实例（P2）；③池无上限（P1）。
2. **ContextBuilder 手写 Clone 共享三个 Arc——共享正确，记忆侧快照失效缺失**。session_cache/shared_memory_injector/skill_index_summary（+shared_skill_manager）均 Arc::clone（context.rs:409-419），frozen_system_prompt 指纹覆盖 mode/tools/rules/capability_brief/prompt_registry/agent_id/candidate_id/active_skill——**prompt 可见输入的失效是完备的**；但 `capability_brief` 已入指纹（热更可失效），唯独记忆内容不走指纹、走 FrozenFileMemorySnapshot，而后者无失效钩子（P1-2）。另注意 `memory_injector`/`skill_manager` 本地字段是 clone 时点快照，父级热更后已 fork 的子 runtime 本地副本陈旧——因 injector 无人消费、skill_index_summary 走共享 Arc，实际影响限于 skill_manager 的非提示词用途，P3。
3. **reload_skills 清 system_prompt_snapshots——已落实且完整**。新技能时刷新 skill_index_summary + 全局清空快照（context.rs:639-646），配合 shared_skill_manager 热更，技能→提示词链路联动完整。残留小口：`set_skill_index_summary`/`sync_capabilities` 不清快照（当前无 prompt 可见性影响，P3）。

## 亮点

- **memory_file_store.rs 是工程质量标杆**：原子写（tmp+rename+fsync+目录 sync）、写前快照、tombstone 防复活、会话隔离（stable_hash）、shadow 只读模式、WriteGuard 跨文件协调、12 线程并发测试 + 注入拒绝测试，与 docs/05 宣称的写入口保证逐条对应。
- **跨 runtime 提取互斥设计扎实**：create_new 原子 marker、journal owner_pid + 3x 阈值双保险、ExtractionMarkerGuard panic 安全（带 catch_unwind 测试）、stale 清理分级日志。
- **可观测性完整**：熔断器（cursor 保存失败也计入）+ session_metrics + memory_event（layer3/5 全事件）贯通。
- **cursor 三重冷却 + merge_and_save read-merge-write（进程内 Mutex + 跨进程锁文件）**，注释详尽说明设计取舍（Instant vs SystemTime、为何不加 TTL 缓存）。
- Post-Sampling 编排（evaluate_memory_hooks）正确处理 Layer 3/5 先于 Layer 4 的顺序约束并支持同轮组合触发，有测试。
- Compact 后基线重置（reset_baselines_after_compact）考虑周到，防止压缩后提取永久沉寂。

## 建议

1. **接线或删除 MemoryInjector**（P1-1）：若按 docs 意图恢复注入，在 frozen 提示词尾部调用 `inject_into_system_prompt`（内容已在写入时扫描，注入面安全）；若确实以 RetrievalOrchestrator 为唯一路径，则删除 injector 死代码并修正 docs/05:224 表述，避免 450 行"看起来在工作"的代码误导后续维护者。
2. **给记忆文件加快照失效**（P1-2）：在 consolidate_legacy_layer5 / MemoryFileStore 写入 / reload_memory_injector_if_needed 路径上按 session_key 清除 `file_memory_snapshots`（或改为 mtime 校验的懒加载），复用 reload_skills 已有的清快照模式。
3. **给 session_memory_systems 池加上限**（P1-3）：借用 SessionContextCache 的 capacity+LRU 模式，超出时归还最久未用实例（触发 MemorySystem::drop，由 marker/journal 机制兜底在途提取）。
4. Layer 3 提取后补 `scan_learned_memory_content`（对齐 Layer 5，P2）；CrossProcessLock 改 `tokio::task::spawn_blocking`（P2）；删除废弃 extract_auto_memory 或至少移除其游标不推进的隐患（P2）。
5. 统一 is_pid_alive 实现（kill -0 优先，/proc 作 Unix 优化）（P3）；docs/05 补充 Layer1-4 分层描述与代码术语对齐（P3）。

---
**核对结论汇总**：无 P0。P1 ×3（注入器死代码/契约不符、记忆快照无失效、池无上限），P2 ×7，P3 ×6。
