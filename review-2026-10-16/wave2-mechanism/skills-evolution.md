# 自进化链路 Review 报告（Wave 2 · 机制层）

> 只读审查 · 2026-10-16 · 子代理 w2-skills-evo
> 审查范围：`crates/skills/src/evolution/`（含 `audit.rs`、`service.rs` pipeline 编排的交叉验证）、`crates/skills/src/core_evolution/`；对照 `docs/09_self_evolution.md`、`BLOCKCELL.md` 不变量、`crates/core/src/evolution_boundary.rs`。

## 规模

| 目录 | 文件数 | 行数 |
|---|---|---|
| `evolution/`（10 文件） | audit 123 · compile 235 · compile_deploy 444 · context 357 · generate 212 · lifecycle 141 · prompts_audit 418 · prompts_gen 619 · tests 108 · versioning 488 | 3145 |
| `evolution.rs`（入口/类型） | 1 | 427 |
| `src/audit.rs`（静态审计） | 1 | 847 |
| `core_evolution.rs` + `generation.rs` | 2 | 1328 |
| **合计（深度阅读）** | | **~5750 LOC** |

辅助交叉验证（只读引用，不属本代理产出）：`service.rs` pipeline 编排、`file_owner_lock.rs`、`capability_provider.rs` 注册事务、`core/src/evolution_boundary.rs`。

## 模块地图

**Skill 进化（`evolution/`）**
- `evolution.rs`：类型与状态机。`EvolutionStatus` 11 态（Triggered→Generating→Generated→Auditing→AuditPassed/Failed→CompilePassed/Failed→Observing→Completed/RolledBack/Failed + 5 个 legacy 态仅 serde 兼容），`normalize()`/`is_compile_passed()` 做旧记录映射。`SkillLayout` 四形态（PromptTool/LocalScript/Hybrid/RhaiOrchestration）。`ObservationWindow`（60min / 10% 阈值）。
- `lifecycle.rs`：构造、trigger、staging 目录路径穿越校验（canonicalize + workspace 前缀检查）。
- `generate.rs`：`generate_patch`（LLM→extract_diff→存 record）、`regenerate_with_feedback`（attempt+1、清 audit/shadow/observation）。
- `audit.rs`：LLM 审计（独立会话，基于**应用后完整脚本**而非 diff，注释标 P0-1 fix），按 layout 选 audit prompt。
- `compile.rs`：`compile_check` 各分支（Rhai 引擎编译 / python3 py_compile / 本地脚本语法检查， TempFileGuard RAII）、`extract_diff_from_response`、`parse_audit_response`。
- `compile_deploy.rs`：**部署核心**。`compile_check` 状态落盘、`deploy_and_observe`（前置校验→ensure_baseline→create_new_version→观察窗口）、`check_observation`（error_rate 超阈值即回滚优先于时长判定）、`contract_check`（SKILL.md 的 `## Shared`/`## Prompt` 强制 + meta.yaml name/description）、`rollback`（staged 无 baseline 时删主目录）。
- `versioning.rs`：`create_new_version`（全量覆写 + staged promote 的 .bak 备份/失败恢复链 + 版本快照失败回退）、`record_observation_call`（**FileOwnerLock 保护**的观察计数）、`save_record`（tmp+rename 原子写）、`restore_previous_version`、`cleanup_skill_dir`。
- `context.rs` / `prompts_gen.rs` / `prompts_audit.rs`：富上下文（BLOCKCELL.md/SKILL.md/相邻技能/历史进化）与四形态生成/修复/审计 prompt；`sanitize_for_code_fence` 防围栏逃逸。

**核心进化（`core_evolution/`）**
- `core_evolution.rs`：能力进化（非技能）。`request_capability`（幂等 + 连续失败 3 次自动 Blocked，7 天过期）、`run_evolution`（生成→编译/审计→验证→加载注册，max_retries=3）、`run_step`/`run_step_with_context`（EvolutionWorker 单步驱动）、`run_one_pending_evolution` / `run_pending_evolutions`、`rollback_capability`（文件级回滚 + executor 重建）、`compile_artifact`（先静态审计后落盘；bash -n / py_compile / Rhai 编译）、`validate_artifact`（**不执行**生成代码，有注释化理由）、`load_capability`（**先版本快照后注册**，失败不留 live-only tool）。
- `generation.rs`：LLM 生成 prompt（按 ProviderKind 分 bash/python 要求 + schema 输出要求）、`extract_code_from_response`。

**与 BLOCKCELL.md 固定流程对照**（service.rs pipeline 编排，本代理交叉验证）：生成补丁 → 静态审计（service.rs:748）→ LLM 审计（audit.rs）→ 编译检查（compile_deploy.rs:7）→ 契约检查（compile_deploy.rs:232）→ 部署观察窗口（compile_deploy.rs:141），失败均带 FeedbackEntry 重试。顺序与文档一致；但顺序由 service 调用次序保证，各方法自身只校验部分前置（见 P2-3）。

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| P1 | `src/audit.rs:486-543`（shell 黑名单）、`src/audit.rs:348-367`（Python 黑名单） | 静态审计可被 interpreter one-liner 绕过：shell 分支只认命令名（rm/curl/sudo/eval），`python3 -c "…"`、`node -e`、`perl -e` 均不在清单；Python 清单不含 `open()`/`socket`/`urllib.request`，而生成 prompt 恰恰建议用 urllib（`prompts_gen.rs:124`）。自动部署前仅两层 LLM 侧检查（静态规则可绕过 + LLM 审计同为 LLM advisory），无人审门 | `check_shell_dangerous_commands` 按 `shell_command_name` 匹配固定名单；`is_dangerous_python_target` 为 14 个固定点号路径。利用前提是 LLM 输出被污染（如社区导入路径 `crates/tools/src/community_hub.rs:117` 同样仅依赖 static_audit）。影响：以用户权限做任意文件读/网络外发 |
| P1 | `core_evolution.rs:274-315`、`core_evolution.rs:319-362`、`core_evolution.rs:692-775` | core_evolution 缺 per-record 互斥：`run_one_pending_evolution` 与 `run_pending_evolutions` 两入口无锁（对比 `service.rs:660-667` 的 pipeline_locks），并发驱动可重复处理同一记录（双份 LLM 成本、重复 register）；`request_capability` 幂等检查是 check-then-act（find_active_record→save_record），并发请求可能生成相同 `evolution_id`（`core_evo_{id}_{秒级时间戳}`）互相覆盖记录 | `run_step` 被 EvolutionWorker 逐步驱动时同理：worker 与手动 `run_pending_evolutions` 并发即双跑 |
| P2 | `compile_deploy.rs:141-153` | 契约检查结果不持久化到 record，`deploy_and_observe` 只校验 `is_compile_passed()` + audit.passed。契约检查是 BLOCKCELL.md 固定流程的强制环节，但任何不经 service 的调用方（或未来 resume 路径改动）都可在未做契约检查时部署 | contract_check 无状态写入，无法在 deploy 前置条件中校验 |
| P2 | `compile_deploy.rs:305-314`、`335-344` | 契约检查对 LocalScript/RhaiOrchestration/Hybrid 的 SKILL.md 是软检查：`skill_md_content` 读磁盘旧文件，`if let Some(content)` 包裹，文件缺失直接跳过——"保持 SKILL.md 完整"不变量对这三种布局无强制（仅 PromptTool 强制，:274-304） | 与不变量"保持 SKILL.md 完整"不对应 |
| P2 | `audit.rs:566-613` | prompt 注入检测为词共现启发式（三组词同时出现才报错），可被 paraphrase 绕过：如 "Disregard instructions to use X"（bucket1=disregard、bucket2=instructions，但缺 bucket3 的 previous/all/system 等词）。仅 exact-phrase 清单（:548-555）可靠 | 测试 `evolution_rejects_paraphrased_prompt_injection` 只覆盖了凑齐三组词的例句 |
| P2 | `compile_deploy.rs`（全文件 444 行）、`contract_check`、`check_observation`、`rollback`、`create_new_version` | 进化链路核心路径零单元测试；`crates/skills/tests/` 唯一集成测试 `builtin_skill_regressions.rs` 不涉及进化。静态审计（audit.rs 12 测）与 core_evolution 边界/事务（9 测）覆盖好，但部署/回滚/观察判定/契约检查这最复杂的部分无任何测试 | grep 确认 evolution/ 内仅 tests.rs 3 个 prompt 相关测试 |
| P2 | `versioning.rs:94-97`、`225-228` | meta.yaml 从 LLM response 原样落盘，无 schema 校验；`contract_check` 只查 `name:`/`description:` 子串。`trigger_rules_prompt`（lifecycle.rs:42）声称"禁止 legacy routing/formatting 字段"但代码不强制——prompt 约束与代码执行脱节 | LLM 控制的 yaml 直接成为技能元数据，可能携带额外字段影响下游解析 |
| P2 | `core_evolution.rs:375-535` | `run_step` 无状态前置校验：可任意顺序调用（未 Compiled 即 LoadCapability、重复 GenerateCode 重复消耗 LLM 额度），状态机正确性完全依赖外部 EvolutionWorker（对比 SkillEvolution 各方法均有严格状态前置检查，generate.rs:13-21 / audit.rs:15-23） | 设计不对称 |
| P2 | `core_evolution.rs:1053-1122` | `validate_artifact` 近乎空检查：Check1 文件存在、Check2 非空、Check3 恒真（`non_executing_validation` 恒 passed）。"验证"环节名不副实。有注释化理由（不执行不可信生成代码），但应与 audit 职责合并或更名 | 实际上安全依赖 compile_artifact 内的静态审计（:956），而非 validate |
| P3 | `versioning.rs:69` | skill 文件直写非原子（`std::fs::write` 覆盖 SKILL.rhai/SKILL.py），进程崩溃可留损坏文件；record 文件已有 tmp+rename，skill 文件没有。代码注释（:64-68）承认此限制 | 与 record 原子写标准不一致 |
| P3 | `versioning.rs:94-97` vs `225-228` | staged 路径 meta.yaml 写两次：第一次写入 staging 目录，promote（rename）后该写入即失效，第二次才写 dest。冗余 | 无功能危害 |
| P3 | `compile_deploy.rs:22` vs `audit.rs:616` vs `contract_check` | PromptTool 长度阈值不一致：compile_check ≥50、static audit ≥100、contract ≥100、生成 prompt 建议 ≥200。同一内容在不同关卡标准不同 | 阈值收敛即可 |
| P3 | `compile.rs:183`、`core_evolution.rs:1042` | 编译检查用 `rhai::Engine::new()` 默认限额（max_operations 无限），无法拦截语法合法但资源耗尽的脚本（如 `while true { x = x + 1; }` 虽有 break 检查兜底但可变形绕过）；实际限额依赖运行时 engine.rs 配置（另一代理范围） | compile 阶段不做限额设定 |
| P3 | `evolution.rs:250-257`、`341`、`generate.rs:197` | `ShadowTestResult` 已成死代码：字段仅被 reset，全仓无任何写入点。docs 09 的 Testing 阶段实际不存在 | 与文档漂移联动（见⑦） |
| P3 | `versioning.rs:463-467` | `save_record` 跨进程 .bak rename 竞态：并发写同一 record 时两次 `rename(record→bak)` 会互相覆盖备份；但两个写入均为完整 record JSON，最终结果为某一次完整写入（丢更新不损数据）。`record_observation_call` 有 FileOwnerLock，其余写路径无 | 低风险 |
| P3 | `versioning.rs:422-426`、`context.rs:220-272` | 每次 tick 全量扫描 records 目录（`list_records`、`load_recent_evolution_summaries` 读全部 JSON 解析）；记录量增长后 O(n)/次 | 当前规模无感 |
| P3 | `versioning.rs:30-355` | `create_new_version` 成功后若进程崩溃于 `save_record(Observing)` 之前，重启后 pipeline 会再次部署同一补丁（内容幂等，但多产生一个版本号） | 边界情形 |

维度结论：
- **① 正确性**：状态机实现与 BLOCKCELL.md 固定流程一致（生成→静态审计→LLM审计→编译→契约→部署观察）；补丁模型为"全量覆写"而非 diff-apply，天然规避 apply 半状态；观察窗口判定合理（error_rate 超阈值优先回滚，`compile_deploy.rs:202-210`）；回滚/事务有较完整的失败恢复链。存在 P2-3/P2-2/P2-4 等缺陷（见上表）。
- **② 并发**：`record_observation_call` 用 FileOwnerLock 串行化观察计数（正确）；`SkillEvolution` 无可变内存状态，天然线程安全；core_evolution 侧缺互斥（P1-2）。
- **③ 安全（主维度）**：两层审计 + evolution_boundary（禁 native code、单路径分量）+ shell/python 危险模式 tokenizer 级检查（注释/字符串不误报，alias/getattr 变体有测试）+ 围栏逃逸 sanitize，防线层次好；但静态清单可绕过（P1-1）、注入检测可 paraphrase 绕过（P2-5）、meta.yaml 无校验落盘（P2-7）。
- **④ 性能**：已检查未发现实质问题（全量目录扫描为 P3 级）。
- **⑤ 测试覆盖**：静态审计与 core_evolution 事务/边界测试质量高（含危险代码拒绝、产物名碰撞、激活失败不留注册等针对性用例）；但部署/契约/观察/回滚与整链路无测试（P2-6）。
- **⑥ 可维护性**：模块拆分清晰（evolution.rs 薄入口 + 按阶段分子模块，注释详尽）；死代码 shadow_test 应清；状态机与布局双枚举（SkillType×SkillLayout）存在表达冗余（skill_type 默认 Rhai，若上下文未显式设置且 layout=PromptTool 会把 markdown 写入 SKILL.rhai——当前 service 的 detect_skill_layout 总会同时设置二者，故为潜在不一致而非现存 bug）。
- **⑦ 契约一致性（docs/09_self_evolution.md 漂移）**：文档基于 legacy 实现，具体漂移：
  1. **阶段名**：文档 `Triggered→Generating→Auditing→Compiling→Testing→RollingOut`（6 阶段）→ 实际 `Triggered→Generating→Generated→Auditing→AuditPassed/Failed→CompilePassed/Failed→Observing→Completed/RolledBack`（原 DryRun/Test 合并为单一 Compile；legacy 态仅 serde 兼容，`evolution.rs:366-416`）。
  2. **灰度发布**：文档第 101-115 行 10%/50%/100% 灰度分流 → 实际 `ObservationWindow` 单次全量部署 + 错误率监控回滚（`compile_deploy.rs:141-214`），无任何百分比路由代码。
  3. **只能改 Rhai**：文档安全边界 #1 "只能修改 Rhai 脚本" → 实际覆盖四形态（PromptTool/LocalScript/Hybrid/RhaiOrchestration），且 core_evolution 生成 bash/Python/Rhai 能力（`core_evolution.rs:961-1049`）。
  4. **Testing 阶段**：文档第 88-99 行 tests/ 目录干跑测试 → 实际不存在（ShadowTestResult 死代码）。
  5. **审计**：文档仅描述"静态审计" → 实际静态 + 独立 LLM 审计两层（`audit.rs`）。
  6. **版本目录**：文档 `~/.blockcell/workspace/tool_versions/` → 实际版本快照在技能目录内 `versions/`（VersionManager，versioning.rs 管辖）。

## 亮点

- **record 原子写入**：tmp + rename + .bak 三段式（`versioning.rs:433-473`、`core_evolution.rs:1199-1229`），Windows 兼容处理。
- **TempFileGuard RAII**（`evolution.rs:70-96`）：编译临时文件 panic/取消也不泄漏。
- **staged promote 事务链**（`versioning.rs:100-254`）：.bak 备份 → 原子 rename → 版本历史迁移 → 失败逐级恢复，且拒绝"无备份 remove_dir_all fallback"。
- **core_evolution 激活顺序**（`core_evolution.rs:1167-1177`）：先建版本快照再注册，失败不留 live-only tool，并有专门测试 `capability_activation_failure_does_not_leave_registered_executor`。
- **静态审计质量**：Python 用轻量 tokenizer 区分注释/字符串、解析 import alias 与 `from x import y as z`、覆盖 `getattr(os,'system')` 动态调用（`audit.rs:184-410`）；shell tokenizer 引号感知；测试覆盖变体（大小写、引号内文本放行）。
- **evolution_boundary**（core crate）：`parse_evolvable_provider_kind` 拒绝 native/dylib，`ensure_single_path_component` 防路径穿越，core_evolution 全链路调用。
- **FileOwnerLock**（`file_owner_lock.rs`）：create_new + owner 内容校验释放 + 15 分钟 stale 回收，实现正确。
- **产物名碰撞防护**：`a.b` 与 `a_b` 归一化后冲突检测（`core_evolution.rs:203-224`），有测试。
- **失败熔断**：连续 3 次失败自动 Blocked + 7 天时间衰减 + 手动解除（`core_evolution.rs:109-112`、`605-661`）。
- **prompt 工程对齐 BLOCKCELL.md 不变量**：最小改动（prompts_gen.rs:70/144/321/504/578 "Preserve… only change what is necessary"）、display_text 优先（多处 "prefer returning display_text"）、优雅降级（"Handle errors gracefully with try/except"）、SKILL.md 契约（Shared/Prompt section 强制 + Hybrid 契约提示）。
- **审计基于应用后完整脚本**（audit.rs:36 注释 P0-1 fix），非 diff 文本——避免 diff 与最终内容不一致的审计盲区。
- **legacy 状态 normalize**（`evolution.rs:392-416`）：旧记录可被新管线安全处理。
- **prompt 围栏逃逸防护**：`sanitize_for_code_fence` 把 ``` 替换为零宽字符分隔（prompts_audit.rs:39-41），防生成内容注入审计 prompt。

## 建议

1. （P1-1）扩展静态审计：shell 黑名单增加解释器 one-liner 模式（`python3 -c`/`python -c`/`node -e`/`perl -e`/`ruby -e` 等）；Python 清单评估 `open(`（限定写模式/敏感路径）、`socket`、`urllib.request.urlopen`、`__builtins__` 等。至少对 core_evolution 自动部署路径加"无人审时禁用解释器转义"的硬规则。
2. （P1-2）core_evolution 引入与 service 对等的 per-record 锁（可复用 FileOwnerLock，namespace 区分）；`request_capability` 的幂等检查与记录创建放进同一临界区，evolution_id 加随机分量防撞。
3. （P2-3）契约检查结果写入 record（如 `contract_passed: bool`），`deploy_and_observe` 将其纳入前置条件，使 BLOCKCELL.md 流程强制环节不可绕过。
4. （P2-4）LocalScript/Hybrid/RhaiOrchestration 的 SKILL.md 缺失应与 PromptTool 同等按 error 处理（至少 staged 新技能导入时强制生成）。
5. （P2-6）为 `compile_deploy.rs` 补测试：deploy_and_observe 前置拒绝（audit 未过/状态不符）、check_observation 三态（超阈值回滚/到期完成/仍在观察）、rollback 的 staged 删目录路径、contract_check 四布局分支。集成测试用一个假 LLMProvider 驱动 generate→audit→compile→contract→deploy→observe→rollback 全链路。
6. （P2-7）meta.yaml 落盘前做结构校验（YAML 解析 + 白名单字段），并把 `trigger_rules_prompt` 的"禁遗留字段"从 prompt 约束升级为 contract_check 的确定性检查。
7. （⑦）docs/09_按现状重写：Compile 单阶段、Observing 观察窗口（非灰度）、四形态覆盖、双层审计、删除 Testing 阶段；或加"legacy 说明"标注。
8. （P3）清理 ShadowTestResult 死代码；统一 PromptTool 长度阈值为单一口径；skill 文件写入改 tmp+rename；`run_step` 加状态前置校验。
