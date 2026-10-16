# core Review 报告（Wave 1 · 基础层）
> 只读审查 · 2026-10-16 · 子代理 w1-core

## 规模（文件数/LOC/入口）
- 37 个 `.rs` 文件，11,160 LOC（src 9,733 + tests 427）。入口：`crates/core/src/lib.rs`（23 个模块 + 约 30 个 re-export），被全仓 10 个 crate 依赖。
- unsafe 共 5 处：1 处 Windows `MoveFileExW`（windows_sys FFI，wide-string 以 `\0` 结尾，用法正确）；4 处在 `config.rs` 测试中（`env::set_var/remove_var`，带 SAFETY 注释）。**非测试 unsafe 仅 1 处且合理**。

## 模块地图（核心类型/职责/被谁依赖）
- `config.rs` + `config/*`（10 子模块，约 4.5K LOC）：`Config` 全量配置模型（providers/agents/channels/memory/intent…），json5 解析 + `${ENV:-default}` 展开、原子持久化写（0600）、时区校验、agent 解析（defaults + 覆盖合并）。被所有 crate 依赖。
- `path_policy.rs`：路径访问策略引擎（read/write/list/exec × allow/confirm/deny），内置敏感路径 deny、最长前缀 deny>allow、symlink 逃逸防护（canonicalize 最长已存在祖先）。与 docs/22 一致。
- `tool_policy.rs`：工具调用策略引擎（glob + when 条件，YAML 配置，fail-closed on parse error，默认 permissive）。
- `failure.rs`：失败语义（FailureKind/Idempotency/ToolOutcome/RequestRecovery），provider/tool 错误分类与重试判定——全仓重试逻辑的事实标准。
- `abort_token.rs` + `abort_token_context.rs`：取消链（AtomicBool + Notify，parent 链）、CleanupRegistry、task-local 取消上下文。
- `budget.rs`：token/成本预算追踪（原子计数，子 tracker 归并到父链）。`paths.rs`：`~/.blockcell` 目录布局、agent 子目录安全命名、session 文件 stem。
- `scope.rs` / `capability.rs` / `session_key.rs` / `mcp_config.rs` / `file_store.rs` / `logging.rs` / `message.rs` / `types.rs` / `agent_result.rs` / `system_event.rs` 等：作用域 registry、能力描述、稳定哈希 session 键、MCP 合并配置、跨进程文件锁与原子写、可开关日志层、IM 消息与 LLM 类型。

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| P1 | abort_token.rs:141-193 | `CleanupHandle` 用 Vec 下标标识 handler，`run_all()`  drain 后下标失效：先 register(A) 得到 idx0 → `run_all()` 排空 → 再 register(B) 复用 idx0 → 之后 A 的 handle drop 时 `get_mut(0)` 执行的是 **B 的 handler**（且会把 B 的槽位换成 noop，导致 B 自己的 drop 不再执行）。清理函数在错误时机执行/错对象执行。现有测试用 `mem::forget` 恰好绕开了该路径，bug 未被覆盖。**当前仓内无消费者**（仅 lib.rs re-export），属潜伏缺陷 | register 无稳定 ID（对比 scope.rs 的 `ScopeId` 方案）；run_all:156-167 drain 后 Vec 复用下标 |
| P1 | tool_policy.rs:311-353 | 策略引擎 **first-match-wins**，无 deny 优先/特异性：若配置中一条宽 `allow` 规则排在精确 `deny` 规则之前，deny 永远不生效——安全策略的经典排序陷阱。与 `path_policy.rs` 的"最长前缀 + deny/allow 特异性"设计不一致，两个策略引擎哲学割裂 | evaluate 按 `self.compiled` 文件顺序返回首个匹配；deny 无任何优先权 |
| P2 | logging.rs:80,85,111-113,143,184,188；message_receipt.rs:21,29,37 | 非测试代码中 `Mutex::lock().unwrap()` / `.expect("...poisoned")`：锁被 poison 后（前一次 panic 时持锁）直接 panic。日志层与消息回执注册表是全局单例，panic 发生在日志路径会级联。同仓 `scope.rs:99` 已示范 `unwrap_or_else(\|p\| p.into_inner())` 恢复模式，应对齐 | 6 + 3 处 |
| P2 | path_policy.rs:25-37 | `PathOp::from_tool_name` 对未知工具默认 `Read`（fail-open）：新增写类工具若忘记加入白名单，其 Write 操作会被当作 Read 评估，写类 deny/confirm 规则不生效 | `_ => PathOp::Read`；docs/22 的 write 工具列表靠人工维护 |
| P2 | path_policy.rs:247-264, 301-305 | 评估热路径每次访问都重复 `expand_tilde` + `resolve_for_policy`（含 `std::fs::canonicalize` 系统调用）：规则数 × 路径数 × 每次文件访问。无任何预计算/缓存，规则前缀的规范化结果完全可以在 load 时算一次 | `best_match_len` 内循环逐 pattern canonicalize 双侧 |
| P2 | config.rs:341-362, 645-653 | `Config::save` 的占位符保留逻辑：`${VAR}` 在 load 之后、save 之前变为未设置时，`expand(previous)="" ≠ next(展开后的密钥)`，占位符不会被还原，**展开后的密钥被明文写回 config 文件**。虽有 0600 权限兜底，仍属凭据落盘路径 | preserve_unchanged_env_placeholders 的相等性判定 |
| P2 | config.rs:529-571 | `load_or_default` 的字段迁移用 `raw.contains("...")` 字符串探测：配置里注释中出现 `loadAllTools`/`consoleEnabled` 等字样即被判定"字段已存在"而跳过迁移（静默跳过型守卫） | needs_save 启发式 |
| P3 | config.rs:609-618 | `validate` 的 warn 日志写 "using minimum value" 但实际返回 `Err` 硬失败，日志与行为矛盾，误导排查 | 同样问题见 620-630 |
| P3 | logging.rs:1-8 vs config/log.rs:13-15 | 模块文档称"文件输出默认开启"，实际 `LogConfig.file_enabled` 默认 `false`，契约文档与代码不一致 | logging.rs:5 |
| P3 | tool_policy.rs:407-414 | `expand_tilde` 在 tool_policy.rs 重复实现（与 path_policy.rs 公开版本逐行相同），复制粘贴 | 应复用 `crate::path_policy::expand_tilde` |
| P3 | session_key.rs:52-54 | `legacy_session_file_stem` 替换 `['/','\\']` 的分支从 `session_file_stem` 第一分支不可达（`/`、`\` 不在允许字符集内，会走 `s~` 编码路径）；仅直接读取旧文件时可达 | 死分支 |
| P3 | config.rs:409-457 | `write_text_atomic_durable` 在 rename 失败时直接 `?` 返回，遗留 `.tmp` 文件不清理；`file_store.rs:108-116` 的同功能函数有清理，两者应统一 | 对比 file_store.rs:114 |
| P3 | config/memory.rs:449,468 | `MonitoringConfig` / `CompactNotificationConfig` 带 TODO 注释"尚未在业务路径中使用"——已定义的死配置面 | TODO ×2 是全 core 仅有的 TODO |
| P3 | config/channels.rs:392-395 vs 492-495 | `NapCatConfig.server_path` 文档注释写默认 `"/onebot/v11/ws"`，代码默认 `"/"`，文档与代码不符 | 差一字符的错误会误导用户配置 |
| P3 | config/intent.rs（341 行）、config/channels.rs（795 行）、logging.rs | 测试覆盖缺口：intent router 默认 profile 大表无独立单测（仅在 config.rs 间接测）；channels 配置类型无测试；logging 仅一个无意义路径断言 | tests/ 下 4 个集成测试文件覆盖 tool_policy/budget/mcp/system_event，主力模块单测充分 |
| P3 | file_store.rs:55-71 | `ExclusiveFileLock::acquire` 用 `std::thread::sleep` 轮询（最多 3s）：若调用方在 async 执行器线程上调用会阻塞 worker；建议文档标注或提供 async 变体 | 跨进程锁只能同步，属设计约束，标注即可 |
| P3 | tool_policy.rs:161-168 vs path_policy.rs:188 | 两个策略引擎默认方向相反（ToolPolicy 默认 permissive 放行，PathPolicy 默认 confirm）；ToolPolicy 默认放行虽有日志说明，仍建议在文档中显式对比 | 行为已文档化，属一致性建议 |

**已检查未发现问题的维度**：
- 正确性：`abort_token` 的取消竞态处理正确（先注册 `Notified` 再查 flag，parent 递归同理）；`budget` 原子计数 + saturating_sub 无溢出；`resolve_for_policy` 的 symlink 逃逸防护有专门测试；`evolution_boundary` 对原生代码/路径穿越的拦截严格。unwrap/expect 除上述两处外全部位于测试代码。
- 并发：`scope.rs`（RwLock + Weak guard）、`budget.rs`（纯原子）、`message_receipt.rs`（短临界区）均无死锁；core 内无跨 await 持锁代码。
- 安全：凭据默认 0600（有测试断言）；`Paths::for_agent` 对非法 agent id 哈希化防穿越（有测试）；MCP/env 展开无可疑注入面。未发现凭据进日志的路径（parse error 不含行内容，低置信不列）。
- 性能：除 path_policy 热路径外无热点问题；`atomic_write`/`write_text_atomic_durable` 均为 durable 原子写。
- 契约一致性：path_policy 实现与 docs/22_path_access_policy.md 逐条对应（优先级、最长前缀、内置路径表、missingFilePolicy 三选项由上层消费）；`Config` 字段与 docs 其他篇章未发现矛盾。

## 亮点
- `path_policy.rs` 的 symlink 逃逸防护（对"最长已存在祖先"canonicalize 再拼接剩余分量）是同类策略引擎中少见的严谨实现，且有针对性单测。
- `failure.rs` 把"Unknown ≠ Retryable"（有副作用超时归 Unknown 而非 Failed）落成了带测试的类型级语义，重试安全性设计成熟。
- `config.rs` 的原子持久化写（0600 + fsync + rename + dir sync）与 `${VAR}` 占位符回写机制工程完成度高。
- `abort_token.rs` 取消链注册先于 flag 检查的竞态注释、`scope.rs` 的 Weak RAII guard、`file_store.rs` 的 stale lock 恢复测试，均体现防御式编码。
- 测试密度高：核心模块几乎全部有单测，tests/ 有 4 个集成测试文件，path_policy 的 symlink 场景测试尤为到位。

## 建议（不执行的后续动作）
1. 修复 `CleanupHandle`：引入单调 `ScopeId` 式稳定标识（或 `Arc<Option<Box<...>>>` 槽位），并补"run_all 后 re-register + 旧 handle drop"回归测试。
2. `ToolPolicy::evaluate` 增加 deny 优先或最长前缀特异性（至少文档注明"deny 规则必须写在 allow 之前"），与 path_policy 对齐。
3. `logging.rs` / `message_receipt.rs` 的 mutex 锁全部改用 poison 恢复模式。
4. `from_tool_name` 增加未知工具告警或默认 `Write`（保守侧），并在 tools crate 加测试断言写类工具全部在白名单内。
5. path_policy 在 load 时预展开/预规范化规则前缀，evaluate 只做纯字符串前缀比较。
6. `Config::save` 前检测 `${VAR}` 当前是否仍可展开，不可展开时保留占位符而非写明文。
7. 为 intent/channels 配置类型补最小反序列化测试；logging 补 console/file 开关行为的单测。
