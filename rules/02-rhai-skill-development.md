# Rhai Skill Development

## 适用范围

适用于需要确定性编排、但仍希望直接调用 blockcell 内置工具的技能。

适合：

- 多步骤工具编排
- 条件分支、循环、降级逻辑
- 对工具调用顺序和错误处理有明确要求
- 希望保留在 Rust 宿主内执行，不引入 Python 依赖

不适合：

- 依赖 Python 第三方库
- 主要只是提示词约束
- 复杂外部 API 客户端已经更适合独立 Python 脚本

## 推荐形态

新 Rhai skill 推荐使用“结构化 Rhai”：

```text
skills/<skill_name>/
├── meta.yaml
├── SKILL.md
└── SKILL.rhai
```

`meta.yaml` 中显式声明：

```yaml
execution:
  kind: rhai
  entry: SKILL.rhai
  dispatch_kind: context
  summary_mode: llm
  actions:
    - name: run
      description: "执行主流程"
      arguments:
        query:
          type: string
          required: true
      argv: []
```

## 不推荐的新写法

不要再新增“legacy Rhai”技能，即：

- 有 `SKILL.rhai`
- 但没有 `execution.actions`

当前实现里，这种技能会走兼容路径，不是新系统的最佳实践。

## 运行时链路

### 结构化 Rhai

当前实现里的主路径：

1. 用户输入命中 skill
2. runtime 根据 `meta.yaml.execution.actions` 选择具体 action
3. LLM 只负责“选方法 + 填参数”
4. runtime 构造 `ctx.invocation`
5. 执行 `SKILL.rhai`
6. 脚本结果再进入最终回复整理

关键点：

- action 选择主要看 `meta.yaml`，不是 `SKILL.md`
- `SKILL.md` 在这条路径里主要用于“最后结果怎么输出”
- 当前实现里，structured Rhai 仍会经过最后一次总结 LLM
- `summary_mode` 目前更多是提示信息，不应当假设它已经是严格运行时开关

### legacy Rhai

兼容路径仍存在，但只建议维护旧技能，不建议新增。

## meta.yaml 规范

规则：

- `execution.kind` 必须写 `rhai`
- 新技能必须声明 `actions`
- `arguments` 要写清 required、type、description
- 如果一个 skill 有多个用户意图，拆成多个 action，不要把所有逻辑塞进一个模糊 action
- `tools` 只声明真实会在脚本中调用到的工具

## SKILL.rhai 规范

### 脚本职责

- 做确定性编排
- 明确处理成功/失败/降级
- 尽量把复杂判断放进脚本，不要留给总结 LLM

### 推荐输出协议

如果这个 skill 需要 follow-up 或结构化展示，推荐输出 JSON 对象，而不是只返回一段 prose：

```json
{
  "display_text": "给用户直接看的简短结果",
  "data": {},
  "continuation_context": {}
}
```

规则：

- `display_text`：适合直接发给用户的结果
- `data`：用户可见的结构化数据
- `continuation_context`：仅供后续追问续接

不要：

- 把大块原始日志塞进 `data`
- 把内部参数暴露给用户
- 把脚本调试信息当结果返回

### 脚本编排建议

- 先校验参数，再调用工具
- 每个工具调用后立刻判断错误
- 降级逻辑写在脚本内，不要交给 LLM 猜
- 能在脚本里定格式的结果，尽量在脚本里定

## SKILL.md 写法

对 Rhai skill，`SKILL.md` 建议只保留：

- 最终输出规则
- 不允许展示的内部字段
- follow-up 解释规则
- 2 到 3 个输出示例

不要把 `SKILL.md` 写成完整命令手册，因为它不参与 action 选择。

## 测试要求

至少覆盖：

1. action 选择
   - 用户输入能否命中正确 action
2. 成功分支
   - 正常工具调用链是否跑通
3. 失败分支
   - 降级是否生效
4. follow-up
   - 是否有足够 `continuation_context`

推荐：

- 为关键输入准备最少 3 个真实回归样例
- 变更后至少走一遍 WebUI / gateway 实测

## 常见错误

- 不写 `actions`，退回 legacy 路径
- 把业务逻辑写进 `SKILL.md`，而不是写进 `SKILL.rhai`
- `continuation_context` 太少，导致后续追问接不住
- 输出里混入内部字段、调试字段、诊断字段
- 工具声明过宽，脚本可调用面太大

## 推荐实践

- 需要确定性工具编排时优先选 Rhai
- 需要第三方 Python 库或外部 SDK 时改用 Python skill
- 参考 [ai_news script](../skills/ai_news/SKILL.rhai) 和 [ai_news meta](../skills/ai_news/meta.yaml)
