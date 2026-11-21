# Blockcell Genesis v0.1

仅依赖 Python 3.10+ 标准库的仓库级自进化 Supervisor，不修改宿主 Rust
进程，不自动 push，不需要安装 Python 第三方包。请从 Blockcell 仓库根目录运行。

## 环境与配置

需要 Git、仓库指定的 Rust 工具链和 Linux bubblewrap：

```sh
sudo apt-get update
sudo apt-get install bubblewrap
export SF_KEY='你的 SiliconFlow Key'
```

真实 Key 只在 Supervisor 环境中读取，不写入配置、报告或候选 HOME。
`agent.key_env` 可以改为其他环境变量名。先编辑 `genesis/genesis.json`：

* `genesis.repository` 可以是本地仓库路径或 GitHub URL；两个 SHA 均应锁定。
* `agent.model/provider/upstream` 指定全程模型和 OpenAI 兼容接口地址。
* `input_usd_per_million/output_usd_per_million` 是**示例价格**，运行前必须按
  上游实际模型价格填写；费用由 usage 的输入/输出 token 计量，并非上游账单查询。
* 每轮、累计美元预算，超时、门槛、规模、停滞和 selector 阈值均在 JSON 中。
* `agent.command` 默认 `blockcell agent -m`，支持 `{binary}`、`{prompt}`、
  `{workspace}`、`{task_id}`。测试可换成假 agent；不能用 shell 字符串解释 prompt。
* `build.command` 和 `benchmarks.L0` 可配置，默认 release build 与 fmt/clippy/test。
  `cargo audit` 需另安装 cargo-audit；缺失会失败关闭而不是跳过安全检查。
  audit 与 Parent 比较新增告警；采取保守策略，任何等级新增漏洞均拒绝。
* EAB60 需设置 `EAB60_KIT` 或 `benchmarks.L3.eab60.kit`，套件需固定版本。
  dev/holdout 题号互斥；安全题全部放 holdout。未配置套件会明确 skipped，
  必选评测 skipped 导致 INCONCLUSIVE，不允许晋级。
* BFCL 默认关闭。开启时配置外部命令 argv，支持 `{binary}`、`{output}`。
  外部命令必须是可信评测器，输出 `{"results":[{"task_id":"t1","passed":true}]}`。
  外部命令与二进制在同一沙箱内运行；额外题库/模块目录可在 BFCL 的
  `readonly_paths` 中列出，只读挂载，不能依赖宿主 HOME。
* `require_bwrap=false` **仅供无凭据假 agent 测试**。真实候选必须保持 true。
  bubblewrap 缺失时明确报错。运行时使用独立 HOME、文件系统白名单、PID 和网络
  namespace；网络 namespace 内仅有桥接到 Supervisor 计量代理的 Unix socket，
  EAB gateway 与评测客户端在同一 namespace 中运行。

## 命令

```sh
python -m genesis init
python -m genesis baseline
python -m genesis run --cycles 3
python -m genesis resume
python -m genesis status
python -m genesis report
python -m genesis --config /path/to/genesis.json run --cycles 3
python -m unittest discover genesis/tests -v
```

`init` 创建独立 clone、锁定 SHA 的 frozen worktree，串行编译 Genesis-0，
复制 release 二进制，然后经 AgentAdapter 执行 `--help`；不调用 LLM。
`baseline`、`run` 会调用配置的模型并产生费用。`resume` 继续已持久化的目标轮数。
不要同时运行多个 Supervisor；共享编译缓存与预算计量设计为串行。

每轮 Parent 的 diagnose/patch/review 使用该 Parent 自己的二进制。
Parent 启动或标准修改自检失败时标为 UNFIT 并回退 Genesis-0；连续三轮全部
候选编译失败也会淘汰该 Parent。高风险路径最多 ARCHIVE，不可自动内部晋级。
PROMOTE 仅在独立 clone/worktree 内提交到本地 `genesis/gen-N` 分支，使用
命令级 Genesis 身份，不修改全局 Git 身份。原仓库不提交、不 push。

## 目录

`source.py` 管 clone/worktree/二进制副本；`archive.py` 保存 SQLite 四表；
`supervisor.py` 保存状态机；`agent_adapter.py` 与 `isolation.py` 管隔离和计量代理；
`diagnosis.py` 提取结构化假设、过滤相似失败经验；`evaluator.py` 只调用 frozen
评测器；`gates.py` 实现 L0–L4 和 paired 四态判定；`selector.py` 实现 70/30；
`report.py` 输出 JSON/Markdown；`adapters/` 放 cargo/coding_eval/eab60/bfcl；
`fixtures/selfcheck/` 是固定修改任务；`tests/` 是 unittest 和失败注入。

运行产物（默认仓库根目录 `.genesis/`）：

```text
.genesis/
  repository/              # 独立 clone，本地晋级分支
  frozen/                  # Genesis-0 SHA，冻结评测器
  worktrees/<agent_id>/    # 候选源码
  target/                  # 共享 CARGO_TARGET_DIR，串行构建
  agents/<agent_id>/blockcell
  agents/<agent_id>/build.log
  archive.sqlite3          # agents/evaluations/task_results/cycle_state
  runs/<agent_id>/         # Diff、逐题报告、判定、PR 草稿
  runs/agent_calls/        # 每次 HOME、session_events/audit、日志
  runs/selfcheck/          # 标准修改任务
  baseline.json
  usage.json
  report.json
  report.md
```

诊断只接收 coding/dev/BFCL 的失败频次，不接收 holdout 题目或轨迹。
失败假设与拒绝原因沿谱系回灌。每次状态先入库后执行；晋级通过 agent_id
和本地分支去重。意外中断的外部工具步骤可能重跑，但不重复创建晋级分支。

## 安全边界与实验限制

不可信原生代码具有广泛攻击面。这里的隔离不等同虚拟机；请在专用、无其他
秘密的实验机运行。Genesis-0 的锁定源码在宿主构建，候选构建和 cargo 门槛使用
去凭据、无网络 namespace、只读 Rust 工具链与 registry、可写共享 target。
因此新依赖须预先下载，cargo audit 的数据库也须预先准备；离线资源缺失会拒绝。
不要把本工具用于不可信第三方仓库或共享生产机。
上游请求发出后实际费用可能在响应 usage 返回时超过剩余预算一个请求；随后请求
会返回预算错误。无 usage 的响应按 token 额度耗尽处理，防止无计量继续调用。
仅完成模拟闭环不代表真实 G-06 改进已获证实；真实进化效果必须以实验记录为准。
