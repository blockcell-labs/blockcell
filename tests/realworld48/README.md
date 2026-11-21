# 48 个真实工作任务评测

可直接转交其他 agent 的执行步骤见 [HANDOFF.md](HANDOFF.md)。

参考 `tmp/blockcell_eval_kit.tgz`，重新构建 48 个有业务用途、可复现、可自动验收的小型任务。数据为合成数据，不含真实客户资料；本地 HTTP 任务不代表公网搜索引擎能力。Python **3.11+**（不要使用 -O/-OO 或 PYTHONOPTIMIZE），全部使用标准库，无需模型 API 即可准备和自检。

题目包含代码修复 6、功能实现 6、数据分析 6、SQL 6、运维分析 6、工程维护 6、资料整理 4、HTTP 8 项。难度标签是设计标签，简单/中等/困难各16题，尚未通过真实模型结果校准。完整提示、交付物、受保护输入见 [manifest.json](manifest.json)。

准备阶段结果见 [VALIDATION.md](VALIDATION.md)，逐题自检见 [SELF_CHECK.json](SELF_CHECK.json)。这些不是待测 agent 成绩。

## 当前准备与自检

在仓库根目录运行：

```bash
python3 tests/realworld48/runner.py list
python3 -m unittest discover -s tests/realworld48 -p 'test_*.py' -v
python3 tests/realworld48/runner.py selfcheck --output tmp/realworld48-selfcheck.json
python3 tests/realworld48/runner.py prepare --run-dir tmp/realworld48-ready
```

`prepare` 创建全部48题，拒绝覆盖已有目录；仅生成题目输入，不向 agent 放入参考解或隐藏判分脚本。每题路径是 `workspaces/tXX/attempt-001`，`TASK.md` 给出具体提示；HTTP 题的真实端点在 `run` 时启动并注入。准备后汇总应为48题全部 `not_run`。

`selfcheck` 的参考解仅用于验证题库：要求每题未完成状态失败、参考实现通过、删除交付物失败、篡改受保护输入失败。它不调用真实模型，不能当作 agent 成绩。自检报告的 kind 会明确标注 `harness_selfcheck_not_agent_evaluation`。代码题还包含未公开输入与部分变异测试；SQL 查询会用额外数据重跑。

`task_code.py`、`task_data.py`、`task_misc.py` 含完整题目生成、判分及参考解。它们属于评测端，不要上传给远程被测 agent。

## 接入 API

等 API 地址、接口类型与模型名确定后，完成一个 wrapper。协议分三种情况：

1. 模型 provider API：由有文件/命令/HTTP工具的 BlockCell 执行任务，模型 API 只负责模型推理。
2. 有工作区及工具执行能力的 agent API：wrapper 提交任务并等到结束，取回交付物。
3. 异步任务 API：还需任务轮询、超时取消与产物下载。

普通 chat completion 成功不代表完成任务。远程 agent 必须能接收工作区、执行工具并把文件同步回当前任务目录；同时需要能访问测试 HTTP 服务。远程部署时可把本题输入和 HTTP 服务一起放到 agent 运行环境，不能直接使用宿主机的 localhost。

wrapper 接收三个模板参数 `{workspace}`、`{task_id}`、`{prompt}`，由 runner 解析为 argv 后直接执行，**不经过 shell**。模板里带空格的参数须加引号。推荐 wrapper 从环境变量读取内容，以避免把长提示放进进程参数：

- `BLOCKCELL_EVAL_WORKSPACE`、`BLOCKCELL_EVAL_TASK_ID`、`BLOCKCELL_EVAL_ATTEMPT_ID`
- `BLOCKCELL_EVAL_PROMPT`：完整本题提示
- `BLOCKCELL_EVAL_ENDPOINT`：HTTP 题的本地服务地址
- API key 通过你选择的环境变量读取，例如 `EVAL_API_KEY`；不写入配置、命令行或 Git。

[config.example.json](config.example.json) 是适配信息示例，不会自动配置或调用供应商。实际 provider 配置及计费单价要在接口明确后设置。

## 冒烟、全量与断点续跑

先跑代码、数据、HTTP各一题：

```bash
python3 tests/realworld48/runner.py run \
  --run-dir tmp/realworld48-run-001 \
  --agent-command '/absolute/path/to/agent-wrapper' \
  --model-label '实际模型固定版本' \
  --task t01 --task t13 --task t43
```

再沿用相同配置跑其余45题：

```bash
python3 tests/realworld48/runner.py run \
  --run-dir tmp/realworld48-run-001 \
  --agent-command '/absolute/path/to/agent-wrapper' \
  --model-label '实际模型固定版本' --resume
```

默认串行、每题600秒。`--timeout` 修改每题上限；`--max-wall-seconds` 限制本次调度时长；`--max-known-tokens` 在已知 token 累计达到上限后停止调度。未提供用量时无法据此保障费用上限，真实硬预算应由供应商或 wrapper 限制。相同 run 不能更换题库代码、命令、可识别的 wrapper/配置文件内容、模型标签或超时配置；变更实验配置请用新目录。运行器会对 EVAL_API_BASE、EVAL_MODEL、EVAL_API_TYPE 的值做哈希比较；wrapper 还应自行固定其他供应商配置。

命令启动失败记 `environment_error`；超时记 `timeout`；判分器异常记 `grader_error`；正常交付按内容记 `pass/fail`。非零退出码即使文件通过也不计成功。`check --run-dir ... --task t01` 可复核离线题输入工作区；HTTP请求轨迹需在同次 live run 验证，不能脱离服务重新构造。

## 人工干预与报告

默认只查看状态与最终校验，不逐步主持 agent，以节省评测主持成本。不能替 agent 写答案、修改验收使失败通过。传输或环境故障修复后重跑，或提供提示协助时，使用：

```bash
python3 tests/realworld48/runner.py run \
  --run-dir tmp/realworld48-run-001 \
  --agent-command '/absolute/path/to/agent-wrapper' \
  --model-label '实际模型固定版本' \
  --task t01 --assisted --intervention-note '说明环境修复或协助内容'
```

每次重跑使用新工作区保留所有历史；首次独立结果保持不变，干预后结果另列，不把 assisted 通过混入独立成功率。若运行器中断，续跑会将未完成尝试记为 interrupted 并保留工作区，自动从新工作区重试，保守地归为 assisted，不把已有改动当作一次全新独立成功。中断尝试的用量通常无法恢复，因此标为未知。一次只在同一运行目录启动一个 runner。

运行目录包含：

- `attempts.jsonl`：逐次尝试的结果、耗时、退出码、错误摘要与用量。
- `summary.json`、`report.md`：始终覆盖48题，包含未运行项、首次独立和干预后结果，按类别与难度分组。
- `workspaces/`：逐次输入与交付物；`state/`：受保护文件哈希；`run.json`：题库指纹和配置摘要。

wrapper 可以写本题目录下 `.eval-metrics.json`，字段为 `input_tokens`、`output_tokens`、`tool_calls`、`tool_errors`、`llm_calls`、`http429`。字段未知请省略；缺失、非法及负数值记录为 `null`。汇总同时给出每项指标覆盖的尝试数。指标来自 wrapper，不用于判定任务成功；供应商未提供信息时不估造 token 或费用。

runner 截取并脱敏输出尾部，对环境中名称含 KEY/TOKEN/SECRET/PASSWORD 的值及 Bearer 凭证做替换；不要让 wrapper 主动输出凭据。工作区是 agent 的原始产物，分享前也需检查是否含敏感内容。

## 运行边界

判分在交付物副本上执行，并检查前后受保护输入哈希；符号链接哈希记录链接本身。代码隐藏测试运行于独立子进程。这里提供进程隔离与结果校验，**不是针对恶意程序的操作系统安全沙箱**：同用户本地进程仍可能读取仓库文件。正式防作弊评测应把 agent 放到只能访问本题输入的独立容器/远程环境，评测器和参考解留在外部，并禁用不必要网络。当前本地方案适合合作式 agent 的功能评测。

不更改 BlockCell 核心实现、不套用原包 provider patch、不创建分支或额外 Git 仓库。准备工作和自检结果不等于真实模型评测；拿到 API 后才会产生真实结果。
