# Blockcell Skills 执行流程优化设计

> 日期：2026-03-13  
> 目标：结合 Blockcell 当前实现，重构 skill 决策、执行、总结三阶段，收口脚本型 skill 与纯 `MD` skill 的执行边界。  
> 结论：采用“双通道 + 三阶段”方案。统一 `Skill Decision`，在 `Execute` 阶段按 `PromptOnly` 与 `Scripted` 分叉，再用统一 `Result Summary` 收口最终回答。

---

## 1. 背景

当前代码已经具备部分结构化能力：

- `SkillMeta.execution.actions` 已存在，可表达脚本型 skill 的方法与参数 schema
- `SkillManager` 已在加载时缓存 `meta.yaml` 与 `SKILL.md`
- `runtime` 已存在脚本型 skill 的 fast path

但主链路仍有三个核心问题：

1. 脚本型 skill 的决策阶段与总结阶段职责反了
   - method 选择阶段带了 `SKILL.md`
   - 总结阶段反而没有利用 `SKILL.md`
2. 纯 `MD` skill 仍会回流到通用 `process_message()` 链路
   - skill 命中后仍可能重新进入开放式 tool loop
   - skill 边界不清，容易绕路
3. skill 类型之间没有统一 invocation 契约
   - Python skill 倾向 `argv`
   - Rhai skill 倾向上下文驱动
   - Markdown skill 倾向 prompt 驱动
   - runtime 当前仍以“入口文件 + if/else”组合判断为主

本次优化的目标不是去掉 LLM，而是收窄 LLM 参与边界：

- 决策阶段：LLM 只做受限结构化决策
- 执行阶段：runtime 或脚本做机械执行
- 总结阶段：LLM 只做结果整理

---

## 2. 现有实现基线

### 2.1 已有能力

- `crates/skills/src/manager.rs`
  - `SkillMeta.execution`
  - `SkillAction.arguments`
  - `cached_md`
  - `match_all_skills()`
- `crates/agent/src/runtime.rs`
  - 多候选 skill 的 LLM 判定
  - `dispatch_structured_skill_for_user()`
  - `run_python_script_with_argv()`
  - `run_markdown_script()`
- `crates/skills/src/dispatcher.rs`
  - `SkillDispatcher` 可执行 `SKILL.rhai`

### 2.2 当前不合理点

1. 脚本型 skill 决策 prompt 依赖 `SKILL.md`
2. 纯 `MD` skill 执行时通过修改消息后再次 `process_message()`
3. 脚本型 skill 总结阶段上下文过小，缺少 `SKILL.md`
4. `ContextBuilder` 在全局 system prompt 中注入 active skill `SKILL.md`
   - 这适合 prompt skill
   - 不适合结构化脚本 skill

---

## 3. 目标架构

统一的 skill 流程：

```text
User Input
  ↓
Phase 1: Skill Decision
  ↓
Phase 2: Skill Execute
  ├─ PromptOnly (MD)
  └─ Scripted (Python / Rhai)
  ↓
Phase 3: Result Summary
  ↓
Final Answer
```

### 3.1 Phase 1: Skill Decision

职责：

- 判断是否使用 skill
- 选择 skill
- 若为脚本型 skill，选择 method 并构造参数

输入只允许包含：

- 当前用户问题
- 最近 3 到 6 轮历史
- 候选 skill 的最小 schema
  - `name`
  - `description`
  - `runtime_kind`
  - `dispatch_kind`
  - `actions`
  - `arguments`
  - 少量 trigger 摘要

这一阶段明确不传：

- `SKILL.md`
- 全量历史
- 任意工具权限
- 目录与文件探索能力
- spawn 子代理能力

统一输出契约：

```json
{
  "use_skill": true,
  "skill": "xiaohongshu",
  "method": "search",
  "arguments": {
    "keyword": "东京旅行"
  }
}
```

或：

```json
{
  "use_skill": false
}
```

### 3.2 Phase 2: Skill Execute

#### A. PromptOnly (`MD`)

不再回流到通用 `process_message()`。

改为进入独立的 `PromptSkillExecutor`，输入：

- `SKILL.md`
- 当前用户问题
- 最近 4 到 8 轮历史
- skill 白名单工具

约束：

- 不再触发新的 skill 匹配
- 不切换回全局 tool scope
- 不允许 spawn 子代理
- 限制最大工具调用轮数
- 没有声明工具时允许纯 prompt answer

#### B. Scripted (`Python / Rhai`)

进入统一的 `StructuredSkillExecutor`，执行顺序：

1. 校验 skill/method/arguments
2. 依据 `dispatch_kind` 构造 invocation
3. 执行脚本
4. 产出结构化结果供总结阶段使用

建议引入：

```yaml
execution:
  kind: python | rhai | markdown
  dispatch_kind: argv | context | prompt
  summary_mode: none | llm | direct
```

语义：

- `python + argv`
  - 按 `argv` 模板展开
- `rhai + context`
  - 将 `{method, arguments}` 注入 `ctx.invocation`
- `markdown + prompt`
  - 进入受控 prompt executor

### 3.3 Phase 3: Result Summary

总结阶段只做结果整理，不再做路由。

输入：

- 原始用户问题
- skill 名
- method 名
- 精简版 `SKILL.md`
- 脚本输出或 prompt executor 输出
- 最近 2 到 4 轮历史

总结阶段明确不开放：

- 工具
- 新 skill 匹配
- 目录探索
- 子代理

---

## 4. 职责边界

### 4.1 `meta.yaml`

`meta.yaml` 面向 runtime，只负责结构化执行契约与决策 schema。

必须承载：

- skill 元信息
- triggers
- fallback
- runtime kind
- dispatch kind
- actions
- arguments schema
- execution template

不再依赖 runtime 从 `SKILL.md` 中推断命令行参数或 action。

### 4.2 `SKILL.md`

`SKILL.md` 面向人和 LLM，只负责：

- skill 用途
- method 语义解释
- 输出组织方式
- 失败解释
- fallback 表达方式
- 结果整理规则

`SKILL.md` 不再承担 runtime 执行契约职责。

---

## 5. 新的内部抽象

建议引入两个统一结构：

```rust
struct SkillDecision {
    use_skill: bool,
    skill: Option<String>,
    method: Option<String>,
    arguments: serde_json::Value,
}

struct SkillInvocation {
    skill_name: String,
    runtime_kind: SkillRuntimeKind,
    dispatch_kind: SkillDispatchKind,
    method: Option<String>,
    arguments: serde_json::Value,
}
```

并拆出四个内部模块：

1. `SkillDecisionEngine`
   - 候选 skill 粗筛后的受限 JSON 决策
2. `PromptSkillExecutor`
   - 执行纯 `MD` skill
3. `StructuredSkillExecutor`
   - 执行 `Python / Rhai` skill
4. `SkillSummaryFormatter`
   - 统一结果整理

---

## 6. 对现有代码的重构方向

### 6.1 复用

- `SkillMeta.execution`、`SkillAction`、`SkillArgument`
- `Skill.cached_md`
- `SkillManager.match_all_skills()`
- `run_python_script_with_argv()`
- `SkillDispatcher`

### 6.2 必须调整

1. `runtime.process_message()`
   - 从“边判断边执行”改成：
     - `decide_skill_invocation()`
     - `execute_skill_invocation()`
     - `fallback_to_general_agent()`
2. `dispatch_structured_skill_for_user()`
   - 决策阶段去掉 `SKILL.md`
   - 总结阶段补入 `SKILL.md`
3. `run_markdown_script()`
   - 不再通过改写消息重进 `process_message()`
4. `ContextBuilder`
   - 仅对 prompt skill executor 注入 `SKILL.md`
   - 不再对结构化脚本 skill 在全局 system prompt 中注入 `SKILL.md`

---

## 7. 三类 skill 的目标状态

### 7.1 PromptOnly

```yaml
execution:
  kind: markdown
  dispatch_kind: prompt
  summary_mode: direct
```

执行路径：

```text
Decision(meta schema) -> PromptSkillExecutor(SKILL.md + scoped tools) -> direct/summary
```

### 7.2 Python

```yaml
execution:
  kind: python
  dispatch_kind: argv
  summary_mode: llm
```

执行路径：

```text
Decision(meta schema) -> argv execution -> summary(SKILL.md + result)
```

### 7.3 Rhai

```yaml
execution:
  kind: rhai
  dispatch_kind: context
  summary_mode: llm
```

执行路径：

```text
Decision(meta schema) -> ctx.invocation execution -> summary(SKILL.md + result)
```

---

## 8. 兼容与迁移策略

不做一次性切换，按能力分层兼容：

- Tier 1
  - 只有 `SKILL.md`
  - 作为 `PromptOnly` 运行
- Tier 2
  - 有脚本，但没有完整 `execution.actions`
  - 作为 legacy script skill 兼容运行，并给出 migration warning
- Tier 3
  - 有完整 `execution.actions`
  - 进入新结构化快路径

迁移优先级：

1. 先收口脚本型 skill
2. 再替换纯 `MD` skill 的回流路径
3. 再统一 Rhai 与 Summary
4. 最后清理 legacy 路径

---

## 9. 风险点

### 风险 1：候选 skill 太多导致决策 prompt 膨胀

缓解：

- 先做 trigger 粗筛
- 决策阶段只带 3 到 5 个候选
- 只带 schema 摘要，不带全量 `SKILL.md`

### 风险 2：参数抽取不稳定

缓解：

- required 校验
- enum 校验
- 类型收敛
- 后续补充 `pattern/default/source`

### 风险 3：PromptOnly skill 重新失控

缓解：

- 独立 executor
- skill 白名单工具
- 禁止 skill 递归匹配
- 限制最大工具回合

### 风险 4：legacy skill 行为回归

缓解：

- 保留 Tier 2 兼容路径
- 先加覆盖测试，再切换执行入口

---

## 10. 最终结论

本次 skill 流程优化的核心原则是：

- 决策阶段只看 `meta.yaml` 结构化 schema
- 执行阶段按 skill 类型进入受控执行器
- 总结阶段才使用 `SKILL.md`
- runtime 只做机械执行与校验
- LLM 不再输出命令，只输出结构化 invocation

落地后，Blockcell 的 skill 执行路径会更短、更稳，也更适合多 method skill 与后续自动化测试。
