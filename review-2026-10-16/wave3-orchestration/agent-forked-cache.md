# forked 与 response_cache Review 报告（Wave 3 · 编排层）

> 只读审查 · 2026-10-16 · 子代理 w3-forked

## 规模

| 范围 | 文件 | 行数 |
|---|---|---|
| forked/ 入口 | `crates/agent/src/forked/mod.rs` | 57 |
| forked 核心 | `forked/agent.rs` | 776 |
| forked 权限 | `forked/can_use_tool.rs` | 1356 |
| forked 隔离 | `forked/context.rs` | 387 |
| forked 缓存参数 | `forked/cache_safe.rs` | 456 |
| forked 执行-事件 | `forked/agent/event.rs` | 315 |
| forked 执行-主循环 | `forked/agent/run.rs` | 961 |
| forked 执行-工具 | `forked/agent/tool_exec.rs` | 1522 |
| forked 测试 | `forked/agent/tests.rs` | 639 |
| 响应缓存 | `src/response_cache.rs`（+ `response_cache/util.rs` 152） | 1668+152 |
| **合计** | 10 个 .rs 文件 | **~8289**（审查范围内全量通读） |

跨范围核对文件（只读引用）：`runtime/learning.rs`、`runtime/process_message_inner.rs`、`runtime/subagent_provider.rs`、`providers/src/anthropic.rs`、`providers/src/prompt_utils.rs`、`docs/11_subagents.md`、`docs/27_ghost_learning_design.md`、`docs/design/ghost-review-cache-alignment-design.md`。

## 模块地图

```
forked/
├── mod.rs            公开 API 再导出（run_forked_agent / roster / CacheSafeParams / context）
├── agent.rs          ForkedAgentParams（~45 字段）+ Builder + ForkedAgentResult/Error
│                     WorkspaceScopeEntry、常量（MAX_FILE_SIZE 等）、DANGEROUS_COMMAND_PATTERNS
├── can_use_tool.rs   ToolPermission / CanUseToolFn；
│                     TOOL_ROSTERS 单一来源（4 模式 roster + schema_order + deny message 三派生）
│                     + 5 个一致性测试；auto_mem/dream/memory-file 路径级白名单；只读 shell 命令过滤
├── context.rs        SubagentOverrides / SubagentContext / FileStateCache / AbortController /
│                     create_subagent_context（abort 链式继承 + 断链告警）
├── cache_safe.rs     CacheSafeParams（Anthropic 缓存键要素）+ is_compatible_with（排除 volatile id）
│                     全局 LAST_CACHE_SAFE_PARAMS（save/get）
└── agent/
    ├── run.rs        run_forked_agent 主循环：abort select、usage 累计、进度事件、provider 重试
    │                 acquire_provider_with_retry（acquire_by_model pin + miss 指标）、
    │                 build_initial_forked_messages / filter_tool_schemas / inject_preloaded_skills
    ├── tool_exec.rs  execute_forked_tool：13 个工具（read/list/edit/write/grep/glob/shell/
    │                 memory_*/skill_manage/list_skills）；路径解析 normalize→canonicalize→
    │                 working_dir 前缀→workspace_scope→can_use_tool 四道门；原子写+回滚
    ├── event.rs      extract_tool_summary / truncate_path / default_read_only_fork_disallowed_tools
    │                 / build_forked_tool_schemas
    └── tests.rs      取消（provider 悬挂 / shell 进程杀死）、路径归一化、schema 过滤、builder 校验

response_cache.rs
├── ResponseCache     会话级大列表缓存：maybe_cache_and_stub / recall / clear_session；
│                     淘汰 = 每会话 max_per_session  oldest-evict；零容量禁用；has_tool_results 守卫
├── ContentReplacementState  tool 结果替换决策状态（seen_ids/replacements/insertion_order + LRU prune）
├── persist_tool_result / process_tool_result / apply_budget(_async)  Layer-1 两层预算
├── cleanup_tool_results  .tool_results TTL + 每会话上限 + 空目录清理
└── util.rs            generate_preview / sanitize_session_key（哈希后缀）/ sanitize_tool_use_id
```

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| P1 | `forked/agent.rs:188`、`agent.rs:601-603`；`runtime/subagent_provider.rs:149-162,368-381` | **Fork/Typed 路径执行期权限门失效**：`ForkedAgentParams::new()` 与 `Builder::build()` 都把 `can_use_tool` 默认设为 `Arc::new(\|_,_\| Allow)`。`spawn_fork`/`spawn_typed` 均不显式设置 `can_use_tool`，唯一执行期 gate 只剩 `disallowed_tools` 黑名单。而 `default_read_only_fork_disallowed_tools()`（`event.rs:114-128`）只含 agent/spawn/exec/shell/edit_file/file_edit/write_file/file_write——**不含 `memory_manage`/`memory_upsert`/`memory_query`/`memory_forget`/`skill_manage`/`list_skills`**，这些工具 `execute_forked_tool` 全部实现（`tool_exec.rs:694-858`）。因此一个 `ToolGrant::ReadOnly` 的 fork，若模型幻觉出 `skill_manage{action:"delete"}` 或 `memory_manage{action:"add"}`，将通过 disallowed 检查 + Allow-all 权限检查并真实执行写/删 skill。`filter_tool_schemas`（run.rs:685）只过滤发给模型的 schema，**不构成执行期 gate**。这违反了模块头声称的"Permission is checked by CanUseToolFn"与 docs/11_subagents.md"tools 白名单"语义；TOOL_ROSTERS 单一来源原则在 fork/typed 路径也不成立（存在第二套事实白名单=deny 列表）。触发条件需模型输出未提供 schema 的工具名，故未列 P0，但属于明确的安全边界缺陷，建议按 must-fix 处理 | `tool_exec.rs:360-366`（deny 列表→Ok）、`tool_exec.rs:370-382`（权限检查 Allow-all 放行）、`subagent_provider.rs:78-103`（ReadOnly 解析出的 deny 列表） |
| P1 | `crates/agent/src/response_cache.rs`（不变体核对：`forked/cache_safe.rs:162`） | **`is_compatible_with` + `fork_alignment_miss` 生产校验只覆盖 1/5 条派生路径**。全仓 run_forked_agent 生产调用点共 6 处：`runtime/learning.rs:186`（唯一有 C10 校验+指标，`learning.rs:146-157`）、`runtime/subagent_provider.rs:175`（fork）、`:402`（typed）、`session_memory/extractor.rs:350`、`auto_memory/extractor.rs:294/713`、`compact/summary.rs:219`。后 5 处既无 `is_compatible_with` 校验也无 `record_fork_alignment_miss`，system prompt / 模型 / tools 与父轮漂移时静默冷启动，仅损失缓存无告警——与"生产期 is_compatible_with 校验 + fork_alignment_misses 指标在所有派生路径生效"的不变量**部分成立（仅 nudge review）**。附带缺口：`learning.rs:52-54` 在 `parent_served_model` 为空时回退 `config.agents.defaults.model`，而 C10 比较两侧都用同一个 `model` 变量构造（`learning.rs:121-135`），**模型层面的真实漂移（相对父轮实际模型）检测不到**，只覆盖 system/tools/messages | `learning.rs:146-157`；`subagent_provider.rs:149-151,368-371`（CacheSafeParams 均不带 tools，fork_context_messages 来自 SessionStore 或空） |
| P2 | `providers/src/anthropic.rs:368-388`（已知遗留核对） | **Anthropic `len-4` 算术对齐成立但脆弱**：父轮 L 个（转换后）消息断点 `L-2`；review fork 首轮 = review_snapshot（父末轮消息 + 补入的 final assistant，`process_message_inner.rs:1604-1625` 显式保证末尾是 assistant 防 user 合并）+ `[user_review]` = `L+2`，故 `(L+2)-4 == L-2` 算术正确。但成立依赖三个隐含假设：① `convert_messages` 的同角色合并不改变相对位置（snapshot 以 assistant 结尾+user_review 恰好安全）；② snapshot 恰好=父末轮+1；③ fork 第 2 轮起追加 `[assistant, tool]` 后长度 L+4，`len-4` 不再对齐父断点且**无任何指标/日志记录这次静默错位**。结论：已知遗留确认存在，fork 首轮对齐正确，多轮 fork 的断点退化不可观测（纯性能损失，非正确性问题） | `anthropic.rs:372-388`；`process_message_inner.rs:1604-1626` |
| P2 | `forked/agent/run.rs:457-460`、`tool_exec.rs:361-366,408-412,748` | **`had_tool_error` 漏报"软失败"**：`skill_manage` 的 `{"success": false}`（安全扫描失败/fuzzy 失败/action 非法）、`memory_manage`/`memory_upsert` store 不可用的 `Ok("... not available")`、`disallowed_tools` 拒绝的 `Ok("not allowed...")` 都走 `Ok` 分支，`had_tool_error` 不置位。`ForkedAgentResult` 文档声称覆盖"权限拒绝、old_string not found 等"，实际只覆盖 `Err` 类。上游 `session_memory/extractor.rs`、`auto_memory/extractor.rs` 仅凭 `had_tool_error` 决定是否推进游标/record_success，软失败会被误判为提取成功（如 review fork 里 memory_manage store 缺失时"成功"完成）。建议：对 `success:false` payload / "not available" / denied 文本也计数，或返回结构化 tool outcome | `run.rs:419-460`；`tool_exec.rs:942-1012` 等多处 `Ok(json!({"success": false...}))` |
| P2 | `forked/agent/run.rs:642-667`、`subagent_provider.rs:137-148` | **`build_initial_forked_messages` 只处理"首条是 system"**：若快照的 system 消息不在首位（或 snapshot 为空而 `prompt_messages` 的 system 在链尾），插入逻辑会生成**双 system 消息**（OpenAI 侧合法但不规范；Anthropic 侧被并入 system 顶层参数，位置语义丢失）。`spawn_fork` 就是 `parent_history + [system("You are a forked agent..."), user]` 的形态——fork 自有 system 落在父历史之后的中段，当前"恰好不出问题"（Anthropic 合并 / OpenAI 容忍中段 system），但这是未定义行为且无测试覆盖。建议显式规范化：把所有 system 消息收敛到首位并加测试 | `run.rs:656-664`；`subagent_provider.rs:137-148` |
| P2 | `forked/context.rs:19-47,229-302`、`forked/agent.rs:115,323-328` | **大面死配置**：`SubagentOverrides.file_state / content_replacement_state / messages / max_output_tokens / require_can_use_tool / critical_system_reminder` 及 `SubagentContext` 对应字段、`query_tracking`、`abort_controller`（legacy）在生产无消费者——全仓仅 2 处构造 `SubagentOverrides`（`subagent_provider.rs:153,372`），只用 `abort_token`/`working_dir`；`create_subagent_context` 在 run.rs:48-54 三个 parent 参数恒传 `None`。`overrides.messages` 被塞进 `context.messages` 但 `build_initial_forked_messages` 从不读取。`ForkedAgentParams.max_output_tokens` 注释称"注意：会改变缓存键"但**从未传给 `provider.chat`**（签名只收 messages+tools）——文档误导。可维护性：~10 个字段的假配置面，读者会误以为生效 | `run.rs:72-76`（messages 来源不含 overrides）；`agent.rs:114-115` |
| P2 | `forked/agent/run.rs:419-454` vs `461-478` | **`files_modified` 双逻辑块重复**：工具执行后先按 `tool_result.is_ok()` 记一轮（file_* + skill_manage 的 `skills/{name}/`），`Ok` 分支内又记一轮（file_* 且兼容 "path" key）。两块职责重叠、键名不一致（前者只认 `file_path`），后者无 skill_manage 覆盖——合并为单处并统一键优先级 | run.rs:419-478 |
| P3 | `forked/agent/run.rs:227-231,293-297` | `max_turns.checked_div(1)` 是无操作（除以 1 永不失败），`turn * 100 / mt` 可直接写；进度百分比在 max_turns=0 时静默为 0 | run.rs:228-231 |
| P3 | `forked/agent/run.rs:245-267,395-416` | LLM 调用失败（provider error）立即 `return Err`，已产出的 `output_messages`/usage 全部丢弃，后台 memory 提取类 fork 整轮工作清零；`final_content` 在截断时可能是含 tool_calls 的中间"思考"内容而非最终答复。建议错误路径返回部分结果 | run.rs:254-267 |
| P3 | `response_cache.rs:181-216` | `maybe_cache_and_stub` 先做 `extract_list_items`/拼 preview/拼 stub，**之后**才检查 `cacheable_min_chars`——低于阈值的内容白做一次全文解析与字符串构造；把 min_chars 检查前置到 items 提取前 | response_cache.rs:181-216 |
| P3 | `response_cache.rs:222-230` | 容量淘汰 `min_by_key(created_at)` 在秒级时间戳并列时任取一条（HashMap 序），且**无"插满后淘汰最旧"的回归测试**（仅 `zero_capacity_disables_response_cache` 覆盖 0 容量）、无 `maybe_cache_and_stub → recall` round-trip 测试、`generate_ref_id` 格式/碰撞无测试 | response_cache.rs:316-332 |
| P3 | `response_cache.rs:564-575` | `ContentReplacementState.prune_if_needed` 淘汰后，同一 tool_use_id 若再次出现会**重新决策**（重新 persist 生成新 tool_ref），违反"决策一旦做出永不改变"的注释前提；靠 `max_replacement_entries`（默认 1000）兜底，超长会话存在理论上的缓存前缀漂移。建议淘汰时保留 replacements 映射或改为不淘汰已替换项 | response_cache.rs:409-415（原则声明）vs 564-575 |
| P3 | `providers/src/prompt_utils.rs:9-32` | `prompt_cache_key_with_tools` 用 `DefaultHasher`（SipHash 固定 key，**不保证跨 Rust 版本稳定**）——作为进程内 OpenAI 路由键可接受，但 cache_safe 的 `compute_hash`（cache_safe.rs:138-151）同样用法，建议注释声明"仅进程内稳定"。另 model 为空串时 model 指纹不入键 | prompt_utils.rs:23-25 |
| P3 | `response_cache/util.rs:110-112` | `floor_char_boundary` 是较新 std API；因前置 ASCII 过滤边界恒对齐故无实际风险，但锁定了最低工具链版本，值得注释说明 | util.rs:110-115 |
| P3 | `forked/agent/tool_exec.rs:214-221` | `workspace_scope` 为空且 `command_working_dir` 为 None 时 `writable_roots` 为空 Vec，semantics 完全依赖 `ShellExecutor` 实现（可能解读为"禁止写"或"不限制"）；建议显式语义 | tool_exec.rs:214-243 |

## 亮点

1. **TOOL_ROSTERS 单一来源落地彻底**（`can_use_tool.rs:153-374`）：四模式 roster/schema_order/deny message 三派生，配套 4 个断言测试（`roster_and_schemas_agree_per_mode` 含顺序、`guards_allow_exactly_their_roster`、`deny_message_enumerates_full_roster` 含"反向不多列"、`public_apis_are_roster_backed` 防公开入口绕开），正是设计文档 P1-0 的验收形态；历史上"Deny 漏列 list_dir"的三份名单漂移已被结构性消灭。
2. **Nudge Review 缓存对齐链完整**：父快照首条 system 原样透传（learning.rs:58-61）、tools 整体透传（`resolve_review_tool_schemas` learning.rs:244-273，父工具齐全时逐字节复用、缺失才回退窄 schema）、model 取 ServedBy.model 并经 `acquire_by_model` pin（run.rs:146-158,802-814）、review 指令仅作 user 消息；`nudge_review_request_aligns_system_tools_and_prefix_with_parent_turn`（learning.rs:1154）断言 C1/C2/C3/C6 且含"system 漂移即红"的防回归断言。C10 不兼容时平滑降级为冷请求并记录 `fork_alignment_miss`（learning.rs:146-157）。
3. **路径安全纵深极佳**（tool_exec.rs:19-147）：`validate_path_safety` 词法归一化后判逃逸（`src/../lib` 放行、`../x` 拒绝）→ `resolve_to_existing_ancestor` canonicalize（**canonicalize 失败即拒，不回退词法路径**，防 symlink/junction 绕过）→ working_dir 前缀同策略比对（规避 Windows `\\?\` 前缀混比）→ `ensure_write_in_workspace_scope`。配套 normalize 边界测试 5 个。
4. **取消链路扎实**：provider 等待与工具执行均 `tokio::select!` biased 监听 abort token；两个实测——pending provider 可中断、取消后 shell 子进程确实被 kill（`kill_on_drop`+等待 1.1s 后断言 marker 不存在）；`AbortController::abort` 先写 reason 再置 flag 的顺序注释与 poison 恢复；`create_subagent_context` 对断链场景主动 `tracing::warn`（context.rs:256-264）。
5. **写入安全**：文件/skill 全部 atomic_write（temp+rename），skill create 失败回滚删目录、edit 的 meta.json 失败回滚 SKILL.md、目录级 scan 失败删文件；并发 skill 修改经 SkillMutex try_acquire（含 TOCTOU 注释）与 SkillFileStore 内部 WriteGuard 分工明确。
6. **cache_safe 比较语义正确**：`is_compatible_with` 对 fork_context_messages 序列化后剔除 volatile `id` 再比较（避免 `ChatMessage::user()` 随机 UUID 造成假不兼容），tools 空/非空三态处理严密。
7. **ResponseCache 细节讲究**：零容量显式禁用（不留悬空 stub）、`has_tool_results` 防"幻觉列表被缓存"、锁 poison 恢复+告警、TOCTOU 双锁修复（单锁内完成 cacheability 判定）、`sanitize_session_key` 用 SHA-256 后缀防跨会话串读（并注释与 session_recall 同源）、`sanitize_tool_use_id` 处理 Windows 保留名。
8. **auto_mem/dream 的 shell 白名单**（can_use_tool.rs:723-899）是全程最严格的执行期 gate：只读命令前缀 + 禁管道/替换/重定向/后台 + 禁绝对路径/环境变量/`..` + working_dir 强制落目录内，`is_auto_mem_path` 明确移除了"文件名 fallback"（注释记录了这是曾修的漏洞），测试覆盖路径遍历攻击向量。

## 建议

1. **[P1-1] 收紧 fork/typed 权限默认**：`ForkedAgentParams` 默认 `can_use_tool` 改为 Deny-all（或编译期强制显式设置），`spawn_fork`/`spawn_typed` 按 `params.tools`/`disallowed_tools` 派生 roster guard（复用 `make_roster_guard` 思路），使"执行期白名单唯一来源"覆盖全部派生路径；同时把 `memory_*`/`skill_manage`/`list_skills` 纳入 ReadOnly 的 deny 列表做双保险。
2. **[P1-2] 校验下沉为公共 helper**：把 `is_compatible_with` + `record_fork_alignment_miss` 包装成 `check_fork_alignment(parent_opt, params)`，在 5 个 `run_forked_agent` 调用点统一调用（低风险路径可传 None 跳过）；C10 比较应使用**父轮真实 served model**而非两侧同源的 `model` 变量。
3. **[P2-1] 断点可观测**：fork 第 2 轮起补一条 debug 日志/指标（如 `fork_alignment_miss: "multi_turn_breakpoint_drift"`），长期可将 `len-4` 改为语义边界（"父快照最后一条消息"位置显式传入）。
4. **[P2-2] had_tool_error 语义补全**：工具结果 JSON 含 `success:false` 或文本匹配 denied/not available 时也置位（或返回 `ForkedToolOutcome` 枚举），保证 memory extraction 的失败语义对齐真实失败面。
5. **[P2-3] 清理假配置面**：删除或接通 `SubagentOverrides/SubagentContext` 死字段（file_state、content_replacement_state、messages、max_output_tokens、require_can_use_tool、critical_system_reminder、query_tracking、legacy abort_controller）与 `ForkedAgentParams.max_output_tokens`；`build_initial_forked_messages` 对"非首位 system"显式收敛（提升到首位）并加双-system 回归测试。
6. **[P3] 小项**：合并 files_modified 双块；min_chars 前置；补 ResponseCache 淘汰/round-trip 测试；`checked_div(1)` 直接删除；provider 错误路径返回部分结果；注释 DefaultHasher 仅进程内稳定。

## 已知不变量核对结论

| 不变量 | 结论 | 证据 |
|---|---|---|
| Nudge Review 缓存对齐链（父快照首条 system 原样进 fork / tools 整体透传 / model=ServedBy.model 经 acquire_by_model pin / review 指令仅 user 消息） | **成立** | learning.rs:52-61,120-135,244-273；run.rs:146-158；nudge_review_request_aligns 测试 learning.rs:1154 |
| TOOL_ROSTERS 是工具白名单唯一来源，无绕过路径 | **部分成立**：review 三模式成立且测试完备；fork/typed 路径被 Allow-all 默认 `can_use_tool` + deny 列表绕过（见 P1-1），存在第二套事实白名单 | can_use_tool.rs:153-374 + 测试 987-1105 vs agent.rs:188,601-603 |
| 已知遗留：Anthropic 断点为算术对齐（fork len-4）而非通用逻辑边界 | **确认存在且当前算术正确**（fork 首轮 L+2 → L-2 对齐），依赖 snapshot 构造与同角色不合并两个假设；fork 多轮后退化且不可观测 | anthropic.rs:368-388；process_message_inner.rs:1604-1626 |
| 已知遗留：C11 routed 显式概念未做 | **确认未做**：agent  crate 无 model-routing 概念；`parent_served_model` 为空时静默回退默认模型，且 C10 两侧同源变量导致该漂移检测不到 | learning.rs:52-54,146-157 |
| 生产期 `is_compatible_with` 校验 + `fork_alignment_misses` 指标在所有派生路径生效 | **仅 nudge review 一条路径生效**（learning.rs:150）；fork/typed/session_memory/auto_memory/compact 5 处无校验无指标 | 全仓 run_forked_agent 调用点枚举 |
| fork_for_message_task / nudge_review_request_aligns / roster 系列测试覆盖 | **充分**：roster 4+1 个一致性测试、nudge 对齐含防回归断言、fork 取消 2 个实测；缺口在 fork/typed 执行期权限、双 system、ResponseCache 淘汰路径无测试 | can_use_tool.rs:1007-1105；learning.rs:1154-1233；tests.rs:43-157 |
| 契约一致性（docs/11_subagents.md、docs/27_ghost_learning_design.md、design/ghost-review-cache-alignment-design.md） | **基本一致，两处偏差**：docs/11 "tools 白名单限制工具"在执行期不强制（P1-1）；设计文档验收 5"review 只能改 USER.md/MEMORY.md/技能"在 review 路径成立（skill_manage 限 skills_dir、memory_manage 限 store），但 review fork 未设 working_dir 且 roster guard **不做路径限制**——read_file/grep/glob/list_dir 可访问宿主任意路径（纵深防御缺口，建议 roster+路径 allowlist 双层，参见 auto_mem 的 `allow_read_only_path` 范式） | docs/design/ghost-review-cache-alignment-design.md:366-373；can_use_tool.rs:364-374 |

## 派生执行器与父的状态隔离（并发维度小结）

- **可变状态**：fork 不继承父 FileStateCache/ContentReplacementState（生产恒默认）；forks 自身也不使用 Layer-1 预算（工具结果直接进 `current_messages`，单 read 上限 50K chars × max_turns≤5 量级可控）。共享的只有 Arc 化 store（memory/skill，内部锁/WriteGuard 保护）与 abort token（子 token 链式取消）——隔离模型清晰。
- **abort/steering**：run_forked_agent 从 task-local 取父 token（run.rs:45），cancel/abort 在 turn 边界、provider 等待、工具执行三处 select 检查；cancel 导致 shell 子进程被杀有测试背书。
- **注意**：forked 循环内工具结果未过 `apply_budget`，长会话 fork（max_turns=10 的 fork/typed）的理论上下文增长略高于父循环，属可接受的量级差异。

**统计：P0 × 0 · P1 × 2 · P2 × 6 · P3 × 8**
