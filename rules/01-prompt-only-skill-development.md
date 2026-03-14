# Prompt-Only Skill Development

## 适用范围

适用于只有 `SKILL.md` 驱动、主要依赖内置工具组合完成任务的技能。

适合这种类型的场景：

- 主要是流程引导、参数澄清、工具选择、输出格式控制
- 不需要强确定性脚本编排
- 不依赖 Python 第三方库或外部运行时
- 允许由 LLM 在技能作用域内自主决定工具调用顺序

不适合：

- 多分支、强状态、强确定性流程
- 需要保存和操作复杂内部参数
- 需要直接接第三方 HTTP/API/浏览器自动化逻辑且想严格控流程

## 推荐目录结构

```text
skills/<skill_name>/
├── meta.yaml
└── SKILL.md
```

`SKILL.rhai`、`SKILL.py` 不应存在。

## 运行时链路

当前实现里，prompt-only skill 的核心路径是：

1. 用户输入命中 `meta.yaml.triggers`
2. runtime 识别为 `PromptOnly`
3. `SKILL.md` 被直接注入系统提示词
4. 只加载该 skill 声明过的工具作用域
5. LLM 在该作用域内决定是否提问、调用工具、整理最终回复

这意味着：

- `SKILL.md` 是主执行说明书，不是附属文档
- `SKILL.md` 内容会直接吃 prompt token
- 写得过长、过散、过像人类说明文，会直接降低效果

## meta.yaml 规范

建议最少包含：

```yaml
name: camera
description: "拍照技能"
triggers:
  - "拍照"
  - "拍张照"
tools:
  - "camera_capture"
output_format: "markdown"
fallback:
  strategy: "degrade"
  message: "当前无法完成拍照，请稍后重试。"
```

规则：

- `triggers` 要覆盖用户真实说法，不要只写内部术语
- `tools`/`capabilities` 只声明真正需要的工具，越少越好
- `output_format` 只写结果形态提示，不要拿它代替格式规则
- 新技能优先使用 `tools`，不要继续扩散旧字段 `capabilities`
- 不要为 prompt-only skill 配 `execution.actions`

## SKILL.md 写法

### 必写内容

- 这个 skill 解决什么问题
- 遇到什么输入先澄清，什么输入可以直接执行
- 工具调用顺序或决策原则
- 输出格式要求
- 失败时如何降级或提示用户
- 2 到 5 个高质量示例

### 应该避免

- 大段背景介绍
- 长篇参数表
- 复制工具 schema
- 和 `meta.yaml` 重复的触发词清单
- 纯人类读物风格的叙述

### 推荐结构

```markdown
# <Skill Name>

## 任务
- 说明 skill 目标

## 澄清规则
- 什么情况下先问
- 什么情况下直接做

## 工具使用
- 先调用什么
- 失败怎么降级

## 输出格式
- 最终回复如何排版

## 示例
- 示例 1
- 示例 2
```

## 开发建议

- 把 `SKILL.md` 当作“执行手册”，不是“产品说明”
- 优先写规则和反规则，不要写空泛目标
- 明确“不要做什么”，例如：
  - 不要编造字段
  - 不要调用作用域外工具
  - 不要把工具错误原样甩给用户
- 如果 follow-up 很重要，直接在 `SKILL.md` 写清：
  - 用户说“第 2 个”“刚才那个”时该如何理解

## 测试要求

至少做这几类验证：

1. 触发验证
   - 用户常见说法能否命中 skill
2. 澄清验证
   - 参数不全时是否先提问
3. 工具作用域验证
   - 是否只使用声明过的工具
4. 输出验证
   - 最终格式是否稳定

推荐做法：

- 在 WebUI / gateway 里走真实对话验证
- 至少准备 3 组真实用户输入作为回归样例

## 常见错误

- `SKILL.md` 写成百科或产品介绍，真正可执行规则太少
- `tools` 声明过多，导致 LLM 漫游
- 该先澄清时没澄清
- 输出格式要求只写“清晰友好”，没有给具体例子
- 想做强确定性流程却还在用 prompt-only

## 推荐实践

- 参考 [camera](../skills/camera/SKILL.md) 和 [camera meta](../skills/camera/meta.yaml)
- 需要强编排时，升级成 Rhai skill
- 需要第三方运行时或外部库时，升级成 Python skill
