# 给执行测试的 agent：48 题评测简明说明

你的任务是接入用户提供的 API，运行这套48题测试，并交付真实结果。题库和自动判分已准备好；**API wrapper 尚需根据实际接口实现**。不要把题库参考解自检当作被测 agent 成绩。

## 1. 确认环境与 API

在仓库根目录工作，本机路径为 `/Users/apple/rustdev/magicbot/blockcell`。只使用 `main` 分支，不创建新分支，不修改题目、判分器或用户已有代码。其他机器可先解压 `blockcell_realworld48_eval_kit.tgz`，在解压目录运行以下命令。

需要 Python 3.11+，以及用户提供的 API 地址、接口类型、模型名和 key；key 从环境变量读取，不写入源码、命令行或报告。不要使用 Python 的 `-O`、`-OO` 或 `PYTHONOPTIMIZE`。

先检查题库可用：

```bash
python3 tests/realworld48/runner.py list
python3 tests/realworld48/runner.py selfcheck --output tmp/realworld48-selfcheck.json
```

应列出48题，自检48题全部通过。自检不调用模型；HTTP题需要允许监听和访问本机127.0.0.1端口。若环境不允许，先解决权限或部署问题。

## 2. 实现一个 API wrapper

根据用户实际接口编写一个独立脚本，例如 `/absolute/path/eval_wrapper.py`，不要修改题库。wrapper 从下列环境变量读取本题信息：

- `BLOCKCELL_EVAL_WORKSPACE`：任务目录，在此读写交付物。
- `BLOCKCELL_EVAL_PROMPT`：完整任务提示。
- `BLOCKCELL_EVAL_TASK_ID`：题号。
- `BLOCKCELL_EVAL_ENDPOINT`：仅HTTP题提供的测试服务地址。

wrapper 负责启动被测 agent、等待任务结束并退出。若接入的是模型推理 API，须通过具备文件、命令和HTTP工具的 BlockCell 或等效 agent 执行，不能只发一次聊天请求就视为完成。若接入远程/异步 agent API，须上传本题工作区、轮询完成、下载交付物，超时后取消远程任务。远程 agent 必须能连通本次 runner 提供的 HTTP 服务，例如使用隧道转发；不可直接把宿主机 localhost 当作远程可访问地址。

执行评测的你可以阅读评测器；**被测 agent 只能收到当前题的输入和提示，不能收到 `task_*.py`、参考解或隐藏校验**。每题使用独立会话，不携带其他题答案。

如果接口提供用量，wrapper 将其写入任务目录 `.eval-metrics.json`，例如：

```json
{"input_tokens": 1200, "output_tokens": 300, "tool_calls": 8, "tool_errors": 0}
```

未知字段省略，不要填造数据。API协议不明确或缺少文件执行能力时，向用户说明缺少什么，不得编造测试结果。

## 3. 先跑3题，再跑剩余45题

将下方 wrapper 绝对路径和模型标签替换为实际值。运行目录使用一个未使用过的新名字；runner 会自动准备48题，无需另跑 prepare。

```bash
python3 tests/realworld48/runner.py run \
  --run-dir tmp/realworld48-run-001 \
  --agent-command 'python3 /absolute/path/eval_wrapper.py' \
  --model-label '实际模型及版本' \
  --task t01 --task t13 --task t43
```

检查这3题的接口调用、文件回传和HTTP连通是否正常。正常交付未通过题目验收属于真实失败，不必为取得通过而修改题库。适配层若需修复，修复后用新运行目录重新开始，避免混合配置。

然后沿用完全相同的 wrapper、模型和运行目录，完成剩余题目：

```bash
python3 tests/realworld48/runner.py run \
  --run-dir tmp/realworld48-run-001 \
  --agent-command 'python3 /absolute/path/eval_wrapper.py' \
  --model-label '实际模型及版本' --resume
```

默认串行，每题最多600秒。只在同一运行目录启动一个 runner。环境变量中的实际模型和API地址在冒烟及全量阶段也应保持一致。

## 4. 少量干预与结果交付

以自动运行和最终校验为主，不需要逐步阅读所有 agent 对话。允许少量环境排障；不要代被测 agent 写答案或调低验收标准。已记录失败的题不会被普通 `--resume` 重跑。需要重跑某题时，使用上述 run 命令，去掉 `--resume`，加入 `--task tXX --assisted --intervention-note '实际干预说明'`。首次独立成绩保留，干预后成绩另列。运行中断后的自动重试也会保守地列为 assisted。

最终向用户交付运行目录中的 `report.md`、`summary.json`、`attempts.jsonl`，并简述：独立通过数/48、失败与未运行数、按类别表现、耗时、已知token用量及其缺失情况、主要失败原因、人工干预记录。不能把未运行项、接口错误或干预后通过计入独立成功数。报告和日志不得包含API key。

完整选项和边界见 [README.md](README.md)。准备阶段验证见 [VALIDATION.md](VALIDATION.md)，它们不代表真实agent实测结果。
