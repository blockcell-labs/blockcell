# Python Skill Development

## 适用范围

适用于需要 Python 生态、第三方库、外部服务 SDK、复杂数据处理或独立 HTTP 客户端的技能。

适合：

- 依赖 `requests`、`pydantic`、网页解析、外部 SDK
- 已有成熟 Python 客户端逻辑
- 需要把复杂外部 API 协议封装在脚本内部

不适合：

- 只是简单的工具编排
- 主要靠 blockcell 内置工具就能完成的流程

如果只是工具编排，优先用 Rhai，不要上 Python。

## 推荐形态

新 Python skill 推荐使用“结构化 Python”：

```text
skills/<skill_name>/
├── meta.yaml
├── SKILL.md
├── SKILL.py
└── tests/
```

`meta.yaml` 推荐：

```yaml
execution:
  kind: python
  entry: SKILL.py
  dispatch_kind: argv
  summary_mode: llm
  actions:
    - name: search
      description: "搜索"
      arguments:
        keyword:
          type: string
          required: true
      argv:
        - "search"
        - "{keyword}"
        - "--json"
```

## 不推荐的新写法

不要再新增“legacy Python”技能，即：

- 有 `SKILL.py`
- 但没有 `execution.actions`

当前实现里，这类技能只适合兼容旧逻辑或定时任务脚本，不适合作为新的聊天技能标准写法。

## 运行时链路

### 结构化 Python

当前实现里的主路径：

1. 用户输入命中 skill
2. runtime 根据 `meta.yaml.execution.actions` 做方法选择
3. LLM 只负责选 action 和填 arguments
4. runtime 按 `argv` 模板调用 `python3 SKILL.py ...`
5. `SKILL.py` 输出 JSON
6. runtime 合并 `continuation_context`
7. 最终再做一次结果整理

关键点：

- action 选择主要依赖 `meta.yaml`
- `SKILL.md` 在这条路径里主要负责最后输出规则
- 当前实现里 structured Python 仍会经过最后一次总结 LLM
- 不要把 `summary_mode` 当成已经严格生效的运行时开关

### legacy Python

兼容路径下：

- runtime 会把用户输入写入 stdin
- 通过 `BLOCKCELL_SKILL_CONTEXT` 传上下文
- 读取 stdout 作为结果

这个模式仍可用，但不建议新技能采用。

## SKILL.py 规范

### 结构化 Python 的推荐输出协议

当使用 `--json` 时，stdout 应只输出一个 JSON 对象：

```json
{
  "success": true,
  "action": "search",
  "display_text": "给用户直接看的结果",
  "data": {},
  "continuation_context": {}
}
```

建议字段职责：

- `display_text`：用户可直接看到的内容
- `data`：用户可见结构化数据
- `continuation_context`：仅供后续追问使用

推荐：

- 日志和错误写 stderr
- stdout 只放最终 JSON
- 如果需要 follow-up，一定给足 `continuation_context`

不要：

- stdout 混杂调试日志
- 把完整 HTML、大块 raw JSON 塞进 `summary_data`
- 把内部 token、id、cookie 暴露给用户

### 脚本结构建议

- 明确的 CLI 入口：`argparse`
- 独立的错误类型
- 明确的 `build_result(...)`
- 每个 action 独立函数
- 网络请求、数据清洗、结果构造分层

## SKILL.md 写法

对 Python structured skill，`SKILL.md` 建议只保留：

- 最终输出格式
- 哪些字段可见，哪些字段不可见
- 列表/详情/发布等结果的展示规则
- follow-up 时如何理解“第 N 条”“刚才那篇”
- 少量高质量示例

不要在 `SKILL.md` 里重复：

- 命令行参数说明
- 详细 action schema
- 大段背景介绍

这些应放在 `meta.yaml` 和 `SKILL.py` 里。

## 测试要求

Python skill 必须带脚本级测试。

最低要求：

1. 每个 action 至少 1 个成功用例
2. 至少 1 个失败用例
3. 至少 1 个 `continuation_context` 用例
4. 验证 stdout 是单个 JSON，有无污染

推荐形式：

- `tests/test_skill.py`
- 使用 `unittest` + `mock`
- 外部 HTTP/API 一律 mock，不要依赖线上服务

推荐验证命令：

```bash
python3 -m unittest discover -s skills/<skill_name>/tests -p 'test_*.py'
```

## 常见错误

- 新技能不写 `actions`，导致只能走 legacy 兼容路径
- 把人类可读 prose 和调试日志一起打到 stdout
- 只返回 `display_text`，没有结构化 `continuation_context`
- `SKILL.md` 写成完整命令说明书，实际对运行时帮助不大
- 把外部 API 错误原样暴露给用户

## 推荐实践

- 外部 API/SDK/网页解析优先选 Python
- 纯工具编排优先选 Rhai
- 参考 [xiaohongshu meta](../skills/xiaohongshu/meta.yaml)、[xiaohongshu script](../skills/xiaohongshu/SKILL.py)、[xiaohongshu tests](../skills/xiaohongshu/tests/test_skill.py)
