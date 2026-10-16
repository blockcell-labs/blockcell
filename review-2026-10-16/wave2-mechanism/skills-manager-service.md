# skills manager/service Review 报告（Wave 2 · 机制层）

> 只读审查 · 2026-10-16 · 子代理 w2-skills-mgr
> 范围：`crates/skills/src/{lib.rs, manager.rs, manager/markdown.rs, service.rs, service/{error_tracker,layout,observation}.rs}`
> 交叉引用（只读）：`crates/agent/src/context.rs`、`skill_file_store.rs`、`runtime/run_loop.rs`、`runtime/tests.rs`、`BLOCKCELL.md`、`docs/04_skill_system.md`

## 规模

| 文件 | 行数 | 说明 |
|---|---|---|
| lib.rs | 37 | 模块导出 |
| manager.rs | 2106 | SkillMeta/Skill/SkillManager/SkillDocCache + 21 个单测 |
| manager/markdown.rs | 305 | SKILL.md 锚点解析 / 链接展开 / 路径防穿越 |
| service.rs | 2342 | EvolutionService（错误上报/pipeline/观察窗/孤儿接管）+ 19 个单测 |
| service/error_tracker.rs | 136 | 错误计数 + 冷却期 |
| service/layout.rs | 115 | 遗留脚本/本地脚本资产探测 |
| service/observation.rs | 44 | 观察期统计 |
| **合计** | **5085** | 40 个单测（全绿假设，未运行） |

## 模块地图

```
lib.rs ─┬─ manager.rs          SkillManager 生命周期：扫描/发现/加载(BlockCell+OpenClaw)/
         │                     热重载 reload_skills / 快照 write_prompt_snapshot / SkillCard 构建
         │  └─ manager/markdown.rs  MarkdownSection 解析、`{#anchor}` 提取、链接展开、
         │                         resolve_markdown_path（canonicalize 防穿越）
         └─ service.rs         EvolutionService：report_error → trigger_evolution →
                               run_sending pipeline（锁 + 重试 + 审计/编译/契约门禁）→
                               deploy_and_observe → tick 观察窗（错误率回滚）→
                               adopt_orphaned_records（磁盘对账）
            ├─ service/error_tracker.rs   阈值/窗口/冷却期
            ├─ service/layout.rs          SKILL.rhai/SKILL.py/scripts 探测
            └─ service/observation.rs     观察期调用计数
```

外部协作（跨 crate 只读核对）：
- `ContextBuilder`（agent/context.rs:120-137, 369, 392-424, 621-648）：`shared_skill_manager: Arc<RwLock<Option<SkillManager>>>` 跨消息共享；`reload_skills()` 写回共享槽并（有条件）清 `system_prompt_snapshots`。
- `SkillFileStore`（agent/skill_file_store.rs:561-582）：create/edit/patch/delete 后删除 `.skills_prompt_snapshot.json`。
- run_loop（agent/runtime/run_loop.rs:508-519）：周期性 tick 内调 `reload_skills()`。

## 问题清单

| 级别 | 位置 | 问题 | 证据/说明 |
|---|---|---|---|
| P1 | crates/agent/src/context.rs:639-646（交互层，核对"近期修复"范围） | `reload_skills` 清 `system_prompt_snapshots` **不彻底**：仅当 `new_skills` 非空时清理；skill **修改/删除**路径（四路径回归测试路径 3）返回空列表 → 快照与 `skill_index_summary` 均不刷新 | 修改既有 skill 后，`frozen_system_prompt` 的 fingerprint（context.rs:686-701）不含 `skill_index_summary`，缓存命中返回旧 prompt；skill 描述变更在系统提示中长期陈旧，直到其他 fingerprint 输入偶然变化。四路径回归测试（tests.rs:7398-7503）路径 3 只断言 `resolve_active_skill` 新鲜，未覆盖 frozen prompt 失效 |
| P2 | manager.rs:1162-1188 | `get_summary_xml` 对 `skill.name` / `meta.description` 直接插值进 XML，**无转义** | description 来自 meta.yaml（含 OpenClaw frontmatter / 导入技能），含 `</skills>` 或 XML 元字符可注入 LLM 系统提示上下文（prompt injection 面）。无对应测试 |
| P2 | BLOCKCELL.md:14 vs docs/04_skill_system.md:250-251 vs manager.rs:36-48,184-190 | **遗留路由字段未被拒绝**，且两份契约自相矛盾：BLOCKCELL.md 明令"禁止 `triggers`/`capabilities`/`always`/`output_format`"；docs 却写"兼容但不建议新增"；实现完全遵循 docs——`capabilities` 作为 tools 回退生效、`always` 驱动 `get_always_skills`、`output_format` 驱动卡片输出，`triggers` 被 serde 静默忽略 | `effective_tools()`（manager.rs:184-190）使 legacy 技能继续按旧路由语义加载；`test_skill_meta_serializes_without_legacy_fields` 反而把 legacy 字段固化进序列化键集合。契约执行点为零（serde 无 `deny_unknown_fields`） |
| P2 | manager.rs:498, 866-912 + 全仓 grep | `.skills_prompt_snapshot.json` **全仓无读者**（仅 manager 写/删、skill_file_store 删、测试断言）；每次周期性全量 reload 都把全部 skill 的 prompt/summary bundle 序列化落盘，属死重量 I/O | `reload_skills` 先删后写（manager.rs:839→830）；若 `load_from_paths` 失败，快照文件已被删且不回补（内存回滚成功、磁盘留空洞）。未来若有消费者需先解决读侧契约 |
| P3 | service.rs:288-292（report_error→detect_skill_layout） | `skill_name` 未先校验即 `skills_dir.join(skill_name)` 后 `read_to_string` | `trigger_manual_evolution` 对 ".." 有校验（测试 tests.rs:1973-1985，实际校验在 evolution crate），但 `report_error` 路径先探测布局再触发，LLM 上下文可控的失败工具名（如 `../某文件`）可把 skills 目录外文件内容读入 evolution context 送入 LLM。属纵深防御缺口 |
| P3 | run_loop.rs:509 + manager.rs:837-855 | 全量重载开销：周期性 tick 触发全量重扫——重读全部 SKILL.md、重解析锚点、重展开链接、对每个 `requires.bins` 执行 `which::which`、全量重写快照 | 无 notify 文件监听（skills  crate 无 notify 依赖）；无增量 diff。技能数少时可接受，但每 tick 固定成本随技能数线性增长 |
| P3 | manager.rs:36（`permissions`） | `permissions` 字段解析、序列化、有测试，但**全仓无任何执行点** | 契约（BLOCKCELL.md:13）列为可选字段，运行时既不校验也不门禁；skill 无法通过声明 permissions 提权（无提权面），但字段是装饰性的，契约名不副实 |
| P3 | manager.rs:1112-1129 | `load_meta` 对 meta.yaml/meta.json 全文读入后 `serde_yaml::from_str`，**无大小上限**；深嵌套 YAML 依赖 serde_yaml 0.9.34 自身防护 | serde_yaml 0.9.34 已较新；解析失败有隔离（malformed skill warn+skip，有测试），但超大文件无上限 |
| P3 | service.rs:471-472, 548-553 | `report_error` 的 resolve→trigger 之间存在竞态：两个并发 report_error 均通过"无进行中进化"检查后各自 trigger_evolution | 后续 adopt 逻辑会接管多余记录，`active_evolutions` 后者覆盖前者，orphan record 可能被重新接管导致重复 pipeline（pipeline_locks 按 evolution_id 互斥，挡不住不同 id）。低概率 |
| P3 | service.rs:1426-1439 | `persist_evolution_experience` 对 `manual/evolution.md` 无锁 append | 多实例并发部署同技能时可能交错写入（append 通常原子，但 check-then-create header 非原子） |
| P3 | manager.rs:914-979 | `scan_directory_with_priority` 对 pack 递归无深度限制；`load_blockcell_skill` 对 `meta.name` 不校验即作 HashMap key 与版本查询入参 | 目录纵深由真实文件系统约束；恶意 `name: a/b/../c` 会被 `validate_skill_name`（versioning.rs:291）挡在版本层，skill 仍以原名注册——当前无路径构造点，属隐患 |
| P3 | tests.rs:7398-7503 等 | 测试缺口：① 无 built-in/workspace 同名覆盖的单测（优先级逻辑 manager.rs:961 仅手工推理）；② 无 `get_summary_xml` 转义测试；③ 四路径回归未覆盖 modify 路径的 frozen prompt 失效（即 P1 场景） | manager 侧单测覆盖较扎实（格式检测 BOM/CRLF、symlink 拒绝、路径穿越、快照写/失效、malformed 隔离、card 构建） |

**已检查未发现问题的维度：**
- 并发（服务内部）：`EvolutionService` 用 tokio Mutex，锁序一致（`pipeline_locks → active_evolutions`，adopt_orphaned_records:1627-1628；无反向获取），`SingleEvolutionResult` 消除了 `is_evolution_pipeline_active` 的 TOCTOU（service.rs:18-39, 654-680）；锁粒度合理（快照后即 drop）。`SkillManager` 本身无内部锁，依赖调用方 `&mut` + `Arc<RwLock>` 共享（context.rs），职责清晰。
- 正确性（meta.yaml 解析边界）：malformed 隔离有测试（manager.rs:1794-1818）；meta.json 回退、空 meta 兜底（dir name 回退）行为明确。
- 安全（路径穿越/恶意目录）：markdown.rs:202-228 `resolve_markdown_path` 用 canonicalize + `starts_with` 双保险且有测试（manager.rs:1647-1671）；扫描层与脚本探测层均跳过 symlink（manager.rs:924, 362-363，各有测试）；`SkillDocCache` 链接展开仅限 skill 目录内。
- 安全（permissions 提升）：无执行点，不存在提权路径（见 P3 装饰化问题）。
- 可维护性（本范围）：子模块拆分合理（markdown/error_tracker/layout/observation），注释与文档注释充分。

## 亮点

1. **路径穿越防护到位**：`resolve_markdown_path`（markdown.rs:202-228）先判绝对路径、再 canonicalize 双方、再前缀校验，配专门的逃逸测试；扫描与脚本检测双 symlink 防护（manager.rs:924/362）。
2. **失败隔离彻底**：单个 skill 加载失败 warn+continue（manager.rs:945-955）；`reload_skills` 用 `mem::take` + 失败回滚旧 skills（manager.rs:840-844），避免半加载状态。
3. **孤儿记录磁盘对账**：`adopt_orphaned_records`（service.rs:1578-1661）+ `resolve_in_progress_evolution_id`（service.rs:266-279）以磁盘为事实来源，解决多实例/重启内存漂移，并有针对性测试。
4. **快照原子写**：tmp + rename，Windows rename 失败回退 copy+remove（manager.rs:897-910）。
5. **ErrorTracker 冷却期**（error_tracker.rs:121-125）：进化→回滚→再进化死循环防护，与清理逻辑配合（cleanup_evolution_rollback）。
6. **四路径热更新回归测试**（tests.rs:7398-7503）覆盖 skill_manage / 后台 Review / 进化部署 / 手工磁盘新增，验证跨消息派生可见性。
7. **OpenClaw 兼容细节**：256KB 上限、OS 过滤、BOM/CRLF 检测、gbrain manifest 豁免（manager.rs:635-660, 1042-1051）。

## 建议

1. **P1 修复方向**（agent/context.rs，需与本报告联动）：`reload_skills` 无条件清 `system_prompt_snapshots`（或把 `skill_index_summary` 的哈希纳入 `frozen_system_prompt` fingerprint，context.rs:686-701），并在四路径回归测试路径 3 补 frozen prompt 失效断言。
2. **get_summary_xml 转义**：对 name/description 做 XML 转义（`& < > " '`），补注入测试。
3. **契约收敛**：二选一——要么 BLOCKCELL.md 与 docs/04 统一口径并在 `SkillMeta` 反序列化处对 legacy 字段告警/拒绝（建议 serde 层 warn + 迁移期容忍），要么从 BLOCKCELL.md 删除"禁止"表述；同时考虑对 `meta.yaml` 必填 name/description、SKILL.md 必含两个 section 在**进化部署/契约检查**入口强制（当前仅 `evolution.contract_check` 管结构，加载器侧全宽松）。
4. **快照文件**：既然无读者，要么删除写盘逻辑（消除每 tick 全量序列化），要么在 gateway/文档中明确读侧契约；`reload_skills` 失败时应回补磁盘快照。
5. **纵深防御**：`report_error` 在 `detect_skill_layout` 前复用 `validate_skill_name` 校验。
6. **性能**：热重载改增量（按目录 mtime/entry diff），或至少跳过快照重写当 skills 指纹未变。
7. 补单测：built-in/workspace 同名覆盖、XML 转义、meta.yaml 超限。

## 契约逐条核对（BLOCKCELL.md）

| 契约条目 | 结论 |
|---|---|
| 每个 skill 必须有 meta.yaml + SKILL.md（:11） | **不满足（宽松）**：`scan_directory_with_priority` 三者任一存在即加载（manager.rs:938-943），缺 meta.yaml 用默认 meta、缺 SKILL.md 无 prompt；与 docs/04:129"任意一个"一致但与 BLOCKCELL.md 冲突 |
| meta.yaml 必填 name/description（:12） | **不强制**：空 name 回退目录名（manager.rs:1023-1027），空 description 放行 |
| 可选 tools/requires/permissions/fallback（:13） | **实现**：四字段均解析生效；`permissions` 装饰化无执行点（见 P3） |
| 禁止遗留路由字段 triggers/capabilities/always/output_format（:14） | **未拒绝**：capabilities/always/output_format 仍生效，triggers 静默忽略（见 P2）；与 docs/04:250-251"兼容但不建议"表述冲突 |
| SKILL.md 必须含 `## Shared {#shared}` + `## Prompt {#prompt}`（:15） | **不强制**：无 reserved section 时整文回退为 prompt/planning/summary bundle（manager.rs:225-231）；含 anchor 时按段编译且有测试 |
| 自进化五原则（最小改动/保持完整/禁危险操作/优雅降级/display_text，:29-35） | **本范围无执行点**：由 evolution/audit（`contract_check`、`static_audit`）负责，另一子代理范围；manager/service 侧仅通过 pipeline 重试与回滚间接保障"优雅降级" |
