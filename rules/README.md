# Skill Development Rules

## 先判断用哪一类

### 1. Prompt-Only

适合：

- 主要靠 `SKILL.md` 约束 LLM 行为
- 主要使用 blockcell 内置工具
- 不需要强确定性脚本编排

看这里：

- [01-prompt-only-skill-development.md](01-prompt-only-skill-development.md)

### 2. Rhai

适合：

- 需要确定性工具编排
- 有分支、循环、重试、降级
- 不需要 Python 第三方库

看这里：

- [02-rhai-skill-development.md](02-rhai-skill-development.md)

### 3. Python

适合：

- 需要 Python 生态、外部 SDK、网页解析、HTTP 客户端
- 需要把复杂协议和数据清洗封装进脚本

看这里：

- [03-python-skill-development.md](03-python-skill-development.md)

## 当前推荐原则

- 只靠提示词和工具约束就能完成的，用 Prompt-Only
- 需要确定性编排，但仍主要调用 blockcell 内置工具的，用 Rhai
- 需要 Python 依赖、第三方 SDK 或复杂外部交互的，用 Python

## 不建议的做法

- 新聊天技能继续走 legacy `SKILL.rhai` / `SKILL.py` 兼容路径
- 在 `SKILL.md` 里塞完整实现细节，而不是把逻辑写进脚本或 `meta.yaml`
- 不区分 skill 类型，统一按一种模板开发
