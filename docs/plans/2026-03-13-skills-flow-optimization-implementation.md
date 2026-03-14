# Skills Flow Optimization Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Refactor Blockcell skill routing into a controlled three-phase flow that unifies skill decision, splits execution by prompt/scripted skill type, and centralizes result summarization.

**Architecture:** Keep `SkillManager` as the cache owner for skill metadata and `SKILL.md`, but move runtime logic out of the monolithic `process_message()` path into explicit decision, execute, and summarize stages. Prompt-only skills get a dedicated scoped executor, while Python/Rhai skills share a structured invocation contract and summary path.

**Tech Stack:** Rust, Tokio, serde/serde_json, existing `blockcell-agent` runtime, existing `blockcell-skills` manager/dispatcher, cargo test.

---

### Task 0: 建立执行隔离与基线审计

**Files:**
- Read: `crates/agent/src/runtime.rs`
- Read: `crates/agent/src/context.rs`
- Read: `crates/agent/src/lib.rs`
- Read: `crates/skills/src/manager.rs`

**Step 1: 创建独立 worktree**

Run:

```bash
git worktree add ../blockcell-skills-flow-opt -b feat/skills-flow-opt
```

Expected: 新 worktree 创建成功，后续实施都在新目录中完成。

**Step 2: 搜索当前 skill 入口与测试点**

Run:

```bash
rg -n "dispatch_structured_skill_for_user|run_markdown_script|llm_disambiguate_skill|match_all_skills|cached_md" crates/agent/src crates/skills/src
```

Expected: 能定位现有 decision、execute、summary 入口，确认重构起点。

**Step 3: 记录基线测试命令**

Run:

```bash
cargo test -p blockcell-agent runtime::tests:: -- --nocapture
```

Expected: 记录当前通过/失败状态，作为后续回归对照。

**Step 4: Commit**

```bash
git add docs/plans/2026-03-13-skills-flow-optimization-design.md docs/plans/2026-03-13-skills-flow-optimization-implementation.md
git commit -m "docs: add skills flow optimization design and plan"
```

---

### Task 1: 定义统一的 skill decision / invocation 契约

**Files:**
- Modify: `crates/skills/src/manager.rs`
- Modify: `crates/agent/src/runtime.rs`
- Test: `crates/skills/src/manager.rs`
- Test: `crates/agent/src/runtime.rs`

**Step 1: 写失败测试，锁定 metadata 新字段与默认行为**

在 `crates/skills/src/manager.rs` 的测试模块添加：

- `test_skill_execution_parses_dispatch_kind_and_summary_mode`
- `test_skill_execution_defaults_dispatch_kind_by_runtime_kind`

示例断言：

```rust
let meta: SkillMeta = serde_yaml::from_str(r#"
name: demo
execution:
  kind: python
  dispatch_kind: argv
  summary_mode: llm
"#).unwrap();

assert_eq!(meta.execution.unwrap().dispatch_kind, "argv");
```

**Step 2: 运行测试，确认失败**

Run:

```bash
cargo test -p blockcell-skills manager::tests::test_skill_execution_ -- --nocapture
```

Expected: FAIL，因为字段和默认逻辑尚未实现。

**Step 3: 最小实现 metadata 契约扩展**

在 `crates/skills/src/manager.rs` 中补充：

- `dispatch_kind`
- `summary_mode`
- `runtime kind` 归一化辅助方法
- `is_prompt_only()` / `is_structured_script()` 一类的语义辅助方法

建议新增小型枚举或字符串 helper，而不是把判断逻辑分散在 `runtime.rs`。

**Step 4: 在 `runtime.rs` 中引入统一结构**

新增：

```rust
struct SkillDecision {
    use_skill: bool,
    skill: Option<String>,
    method: Option<String>,
    arguments: serde_json::Value,
}

struct SkillInvocation {
    skill_name: String,
    runtime_kind: SkillScriptKind,
    dispatch_kind: String,
    method: Option<String>,
    arguments: serde_json::Value,
}
```

先只定义，不切换主流程。

**Step 5: 运行测试确认通过**

Run:

```bash
cargo test -p blockcell-skills manager::tests::test_skill_execution_ -- --nocapture
```

Expected: PASS

**Step 6: Commit**

```bash
git add crates/skills/src/manager.rs crates/agent/src/runtime.rs
git commit -m "refactor(skills): add unified skill decision metadata"
```

---

### Task 2: 抽离 SkillDecisionEngine，统一第一阶段决策

**Files:**
- Create: `crates/agent/src/skill_decision.rs`
- Modify: `crates/agent/src/lib.rs`
- Modify: `crates/agent/src/runtime.rs`
- Test: `crates/agent/src/runtime.rs`

**Step 1: 写失败测试，锁定决策输入输出**

在 `crates/agent/src/runtime.rs` 现有测试模块中新增：

- `test_extract_json_from_text_handles_markdown_wrapped_json`
- `test_skill_candidate_schema_excludes_skill_md`
- `test_multi_candidate_skill_decision_falls_back_to_first_candidate`

示例：

```rust
let text = "```json\n{\"use_skill\":true}\n```";
assert_eq!(extract_json_from_text(text), "{\"use_skill\":true}");
```

**Step 2: 运行测试确认失败**

Run:

```bash
cargo test -p blockcell-agent runtime::tests::test_extract_json_from_text_handles_markdown_wrapped_json -- --nocapture
```

Expected: FAIL 或缺少目标函数/行为。

**Step 3: 创建 `skill_decision.rs`**

实现：

- `SkillDecisionEngine`
- 候选 skill schema 构造
- 多候选 skill 判定
- 单 skill method/arguments JSON 判定

要求：

- 决策阶段只使用 `meta.yaml` 结构化 schema
- 不把 `SKILL.md` 放进 decision prompt
- prompt 仅包含最近少量历史与候选 schema

**Step 4: runtime 接入新决策器**

在 `crates/agent/src/runtime.rs` 中：

- 用 `SkillDecisionEngine` 替代散落的 `llm_disambiguate_skill()` 与 method selection prompt 组装
- 保留现有 helper，后续任务再删除旧逻辑

**Step 5: 运行相关测试**

Run:

```bash
cargo test -p blockcell-agent runtime::tests::test_skill_ -- --nocapture
```

Expected: PASS

**Step 6: Commit**

```bash
git add crates/agent/src/skill_decision.rs crates/agent/src/lib.rs crates/agent/src/runtime.rs
git commit -m "refactor(agent): extract skill decision engine"
```

---

### Task 3: 收口结构化脚本 skill 执行链路

**Files:**
- Create: `crates/agent/src/structured_skill_executor.rs`
- Modify: `crates/agent/src/lib.rs`
- Modify: `crates/agent/src/runtime.rs`
- Modify: `crates/skills/src/manager.rs`
- Test: `crates/agent/src/runtime.rs`

**Step 1: 写失败测试，锁定 decision/summary 上下文边界**

在 `crates/agent/src/runtime.rs` 中新增：

- `test_structured_skill_decision_prompt_does_not_include_skill_md`
- `test_structured_skill_summary_uses_skill_md_brief`
- `test_structured_skill_missing_required_argument_returns_error`

**Step 2: 运行测试确认失败**

Run:

```bash
cargo test -p blockcell-agent runtime::tests::test_structured_skill_ -- --nocapture
```

Expected: FAIL

**Step 3: 创建 `StructuredSkillExecutor`**

在新文件中实现：

- `build_invocation_from_decision()`
- `validate_arguments()`
- `execute_python_with_argv()`
- `execute_rhai_with_context()`

其中 Python 先复用现有 `run_python_script_with_argv()` 逻辑，Rhai 先准备 invocation 上下文：

```rust
ctx["invocation"] = json!({
    "method": method,
    "arguments": arguments,
});
```

**Step 4: 从 runtime 中下沉旧逻辑**

把 `dispatch_structured_skill_for_user()` 的职责拆开：

- runtime 负责调度
- executor 负责校验与执行

同时修复两点：

- decision 阶段不带 `SKILL.md`
- summary 阶段带 `SKILL.md` brief

**Step 5: 运行测试确认通过**

Run:

```bash
cargo test -p blockcell-agent runtime::tests::test_structured_skill_ -- --nocapture
```

Expected: PASS

**Step 6: Commit**

```bash
git add crates/agent/src/structured_skill_executor.rs crates/agent/src/lib.rs crates/agent/src/runtime.rs crates/skills/src/manager.rs
git commit -m "refactor(agent): isolate structured skill execution"
```

---

### Task 4: 新增 PromptSkillExecutor，替换 Markdown skill 回流

**Files:**
- Create: `crates/agent/src/prompt_skill_executor.rs`
- Modify: `crates/agent/src/lib.rs`
- Modify: `crates/agent/src/context.rs`
- Modify: `crates/agent/src/runtime.rs`
- Test: `crates/agent/src/runtime.rs`

**Step 1: 写失败测试，锁定 Markdown skill 不再回流通用 loop**

在 `crates/agent/src/runtime.rs` 中新增：

- `test_markdown_skill_executor_does_not_reenter_process_message`
- `test_markdown_skill_executor_limits_tools_to_skill_scope`
- `test_structured_skill_does_not_inject_active_skill_md_into_global_prompt`

**Step 2: 运行测试确认失败**

Run:

```bash
cargo test -p blockcell-agent runtime::tests::test_markdown_skill_ -- --nocapture
```

Expected: FAIL

**Step 3: 创建 `PromptSkillExecutor`**

实现一个小型受控执行器：

- 输入 `SKILL.md`、history、user question、allowed tools
- 禁止再次 skill 匹配
- 禁止 spawn 子代理
- 限制最大工具轮数
- 输出最终 answer 或结构化 summary result

第一版可以复用现有 provider/tool registry，不需要另起新 agent runtime。

**Step 4: 调整 `ContextBuilder`**

将 active skill `SKILL.md` 的全局 system prompt 注入收口：

- 只对 `PromptSkillExecutor` 内部保留
- 对结构化脚本 skill 不再从 `build_system_prompt_for_mode_with_channel()` 注入

**Step 5: 替换 `run_markdown_script()`**

不再通过改写消息重新调用 `process_message()`，而是改为调用 `PromptSkillExecutor`。

**Step 6: 运行测试确认通过**

Run:

```bash
cargo test -p blockcell-agent runtime::tests::test_markdown_skill_ -- --nocapture
```

Expected: PASS

**Step 7: Commit**

```bash
git add crates/agent/src/prompt_skill_executor.rs crates/agent/src/lib.rs crates/agent/src/context.rs crates/agent/src/runtime.rs
git commit -m "refactor(agent): add prompt skill executor"
```

---

### Task 5: 补齐 Rhai 结构化 invocation 与统一 summary formatter

**Files:**
- Create: `crates/agent/src/skill_summary.rs`
- Modify: `crates/agent/src/lib.rs`
- Modify: `crates/agent/src/runtime.rs`
- Modify: `crates/skills/src/dispatcher.rs`
- Test: `crates/agent/src/runtime.rs`
- Test: `crates/skills/src/dispatcher.rs`

**Step 1: 写失败测试，锁定 Rhai invocation 和 summary 统一入口**

新增测试：

- `test_rhai_structured_skill_receives_invocation_context`
- `test_skill_summary_formatter_uses_brief_md_and_result`
- `test_prompt_and_script_skills_share_summary_formatter`

**Step 2: 运行测试确认失败**

Run:

```bash
cargo test -p blockcell-agent runtime::tests::test_rhai_structured_skill_receives_invocation_context -- --nocapture
```

Expected: FAIL

**Step 3: 在 `dispatcher.rs` 中补 Rhai 上下文注入测试**

给 `SkillDispatcher` 的现有测试体系增加一个最小脚本，断言可读到：

```rhai
ctx.invocation.method
ctx.invocation.arguments
```

**Step 4: 创建 `SkillSummaryFormatter`**

统一所有 skill 总结路径的输入：

- original question
- skill name
- method
- brief skill md
- execution result

让 cron script summary、structured skill summary、prompt skill summary 共用一套构造逻辑。

**Step 5: 运行测试确认通过**

Run:

```bash
cargo test -p blockcell-agent runtime::tests::test_skill_summary_ -- --nocapture
cargo test -p blockcell-skills dispatcher::tests:: -- --nocapture
```

Expected: PASS

**Step 6: Commit**

```bash
git add crates/agent/src/skill_summary.rs crates/agent/src/lib.rs crates/agent/src/runtime.rs crates/skills/src/dispatcher.rs
git commit -m "refactor(skills): unify rhai invocation and skill summaries"
```

---

### Task 6: 收口 runtime 主入口并保留 legacy 兼容

**Files:**
- Modify: `crates/agent/src/runtime.rs`
- Modify: `crates/skills/src/manager.rs`
- Test: `crates/agent/src/runtime.rs`

**Step 1: 写失败测试，锁定新主流程**

新增：

- `test_process_message_routes_prompt_skill_to_prompt_executor`
- `test_process_message_routes_structured_python_skill_to_executor`
- `test_legacy_script_skill_still_uses_compat_path`
- `test_non_skill_message_still_falls_back_to_general_agent`

**Step 2: 运行测试确认失败**

Run:

```bash
cargo test -p blockcell-agent runtime::tests::test_process_message_routes_ -- --nocapture
```

Expected: FAIL

**Step 3: 重写 runtime 主流程**

将 `process_message()` 中 skill 相关路径改成：

1. `decide_skill_invocation()`
2. `execute_skill_invocation()`
3. `format_skill_result()`
4. `fallback_to_general_agent()`

兼容要求：

- `Tier 1`: `SKILL.md` only -> Prompt executor
- `Tier 2`: legacy script skill -> 旧兼容路径 + warning
- `Tier 3`: structured skill -> 新路径

**Step 4: 运行测试确认通过**

Run:

```bash
cargo test -p blockcell-agent runtime::tests::test_process_message_routes_ -- --nocapture
```

Expected: PASS

**Step 5: Commit**

```bash
git add crates/agent/src/runtime.rs crates/skills/src/manager.rs
git commit -m "refactor(agent): split skill decision execute summary flow"
```

---

### Task 7: 更新开发规范与迁移文档

**Files:**
- Modify: `rules/skills_development_guide.md`
- Modify: `docs/plans/2026-03-13-skills-flow-optimization-design.md`
- Test: n/a

**Step 1: 更新 skill 规范**

在 `rules/skills_development_guide.md` 中明确：

- decision 阶段只用 `meta.yaml`
- summary 阶段使用 `SKILL.md`
- `dispatch_kind` / `summary_mode` 的含义
- PromptOnly / Python / Rhai 三类 skill 的目标写法

**Step 2: 补迁移说明**

在设计文档中追加 migration checklist：

- 如何从 legacy Python skill 迁到 `execution.actions`
- 如何从 `SKILL.md` 中移除 runtime 契约
- 如何给 Rhai skill 添加 `ctx.invocation`

**Step 3: Commit**

```bash
git add rules/skills_development_guide.md docs/plans/2026-03-13-skills-flow-optimization-design.md
git commit -m "docs(skills): document new skill decision and execution model"
```

---

### Task 8: 最终验证

**Files:**
- Verify: `crates/agent/src/runtime.rs`
- Verify: `crates/agent/src/context.rs`
- Verify: `crates/skills/src/manager.rs`
- Verify: `crates/skills/src/dispatcher.rs`
- Verify: `rules/skills_development_guide.md`

**Step 1: 运行技能相关单测**

Run:

```bash
cargo test -p blockcell-skills manager::tests:: -- --nocapture
cargo test -p blockcell-skills dispatcher::tests:: -- --nocapture
cargo test -p blockcell-agent runtime::tests::test_skill_ -- --nocapture
cargo test -p blockcell-agent runtime::tests::test_markdown_skill_ -- --nocapture
cargo test -p blockcell-agent runtime::tests::test_process_message_routes_ -- --nocapture
```

Expected: PASS

**Step 2: 运行相关 crate 全量测试**

Run:

```bash
cargo test -p blockcell-skills -- --nocapture
cargo test -p blockcell-agent -- --nocapture
```

Expected: PASS；若失败，逐条记录与本次改动的关联性。

**Step 3: 手工回归 smoke check**

验证以下场景：

- 普通聊天消息不触发 skill
- 单候选 `MD` skill 进入 prompt executor
- 单候选 `Python` structured skill 进入 structured executor
- 单候选 `Rhai` structured skill 进入 structured executor
- legacy script skill 仍可运行
- 最终 summary 不会再次调用工具

**Step 4: Commit**

```bash
git add -A
git commit -m "test(skills): verify controlled skill flow end to end"
```
