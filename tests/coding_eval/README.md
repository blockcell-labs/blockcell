# BlockCell Coding Eval

该基准包含 20 个隔离的真实小任务：修 bug 6 题、加功能 6 题、跨文件重构 4 题、从零项目 4 题。Runner 会复制 fixture、初始化独立 Git 仓库、运行 agent，再执行题目自己的验收脚本。

```bash
# 只验证 fixture 当前基线（预期任务尚未完成，命令返回非零）
python3 tests/coding_eval/runner.py --task bugfix-01-divide

# 运行被测 agent；模板按 argv 解析，三个占位符不会经过 shell 展开
python3 tests/coding_eval/runner.py \
  --agent-command '/path/to/eval-wrapper {workspace} {task_id} {prompt}' \
  --output tests/coding_eval/latest-report.json
```

Wrapper 应在 `BLOCKCELL_EVAL_WORKSPACE` 指向的仓库中执行 BlockCell（默认不传 `--background`），并可选写入 `.blockcell-eval-metrics.json`：

```json
{
  "tool_calls": 18,
  "tests_run": 2,
  "input_tokens": 12000,
  "output_tokens": 3400,
  "subagent_errors": 0,
  "tool_errors": 1,
  "background_usage": 0,
  "verified_by_tests": true
}
```

报告包含完成率、平均工具调用数、平均 token、工具错误率和已验证任务数，以及失败归因：`editing_failure`、`navigation`、`verification_missing`、`subagent_coordination`。可用 `--max-tool-error-rate 0.02` 和 `--baseline summary.json --max-token-ratio 1.2` 设置回归门槛。建议固定模型与配置每周运行一次，并将 JSON 报告纳入版本化的实验记录。
