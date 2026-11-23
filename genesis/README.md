# Blockcell Genesis v0.1.4

Genesis 是 Blockcell 的仓库级自进化实验框架。Blockcell 既是执行修改的 Agent，也是被修改和评测的系统。

## 什么是 RSI

RSI（Recursive Self-Improvement，递归自改进）指一个系统能够产生改进后的版本，并让这个版本参与下一轮改进。在 Genesis 中，当前父代 Blockcell 根据失败证据提出假设、修改源码。独立的 Python Supervisor 构建候选版本，比较候选与父代的表现。符合继承条件的候选可成为后续轮次的父代，继续诊断和修改自己的源码。

## Blockcell 的进化目标

- **提升任务完成能力。** 用 coding_eval 和 EAB 开发集发现可复现的失败，从小补丁入手验证修复，并记录逐题得失。
- **控制改进代价。** 晋级时检查安全题、保留评测集、性能和标准修改自检。补丁受范围约束，Supervisor 和冻结评测源码受保护。
- **验证收益能否跨代延续。** 保存父代和候选的 Git 提交、补丁、评测结果与判定理由，追踪晋级版本在后续轮次的表现。当前版本已实现源码级实验闭环，真实模型下的持续多代收益仍待验证。

面向开发者的系统介绍见 [开发者文档](docs/README.md)：从项目定位、首次运行到状态机、评测门禁、配置、扩展和故障恢复。本文保留运行参数与版本说明。

仅依赖 Python 3.10+ 标准库的仓库级自进化 Supervisor，不修改宿主 Rust
进程，不自动 push，不需要安装 Python 第三方包。请从 Blockcell 仓库根目录运行。
本机验收环境为 macOS 15.7.3 / Intel x86_64；Apple Silicon 尚未实机验收。
Linux bubblewrap 后端保留为实验性支持，需在 Linux 独立验收。

## 环境与配置

v0.1.4：PATCH 后先持久化 `FORMAT` 状态，Supervisor 用 `rustfmt --edition
2021 --config skip_children=true` 仅格式化候选已改动的 Rust 文件，再进入快照和门槛。
L0 在 Parent 上先运行并记录到 `.genesis/L0-baselines/`；fmt 只检查候选改动的文件。
若 Parent 的严格 clippy 失败，会补跑 `--cap-lints=warn` 收集整个 workspace 的诊断，
候选与这份完整诊断按类别、路径、源码片段及数量比较，拒绝新增警告/错误，不忽略整个失败命令。
无法解析的失败一律拒绝。首次运行前需安装 `cargo-audit`，在宿主执行
`cargo audit update-db`；配置默认 `--no-fetch`，在无外网的构建沙箱内只读审计库。

PATCH 沙箱仅挂载 Cargo 的 bin/registry/git 缓存（只读）和 Rustup 工具链，不挂载
Cargo credentials/config。独立可写 Cargo HOME 和共享 target 映射到 workspace
内部，让工具的嵌套 workspace-write 沙箱也可执行离线 `cargo check/test`；候选和
Supervisor 编译仍串行，cargo 使用 2 个 build jobs。提示词明确 8 文件/400 增删行硬限制。
运行产物名单持久化在沙箱外，强制中断后重试仍会归档同一批新生成的日志/数据库；
PATCH 的 cargo 和 L0 新产生的嵌套 `.blockcell/` 测试状态分别归档，保留原有目录及源码。

晋级要求 coding_eval + EAB dev 的配对净修复数至少 2，候选的安全/权限题每次评分均通过，
holdout 总通过数零回退，保留性能与自检。有效但不足的改进（如净 +1）ARCHIVE。
评测缓存 hash 只包含评测输入；门槛、美元预算、审计命令变化不影响有效能力结果。
当前预算估值上限 $30，并发上限 5；参考价格不代表实际账户结算。

需要 Git、仓库指定的 Rust 工具链；Linux 使用 bubblewrap，macOS 使用系统自带的 `sandbox-exec`。以下安装命令仅适用于 Linux：

```sh
sudo apt-get update
sudo apt-get install bubblewrap
export OPENAI_BASE_URL="https://your-provider.example/v1"
export OPENAI_MODEL="deepseek-v4.1-flash"
# 输入 API Key 后回车（不回显）
read -rs OPENAI_API_KEY
export OPENAI_API_KEY
```

真实 Key 只在 Supervisor 环境中读取，不写入配置、报告或候选 HOME。
默认读取 `OPENAI_API_KEY`；`agent.key_env` 可以改为其他环境变量名。
`OPENAI_BASE_URL` 和 `OPENAI_MODEL` 的非空值分别覆盖配置中的接口地址和模型，
覆盖后的值参与评测缓存哈希。先编辑 `genesis/genesis.json`：

* `genesis.repository` 可以是本地仓库路径或 GitHub URL；两个 SHA 均应锁定。
* `agent.model/provider/upstream` 指定全程模型和 OpenAI 兼容接口地址。
  默认 provider 为 `openai`，可连接任何兼容 Chat Completions 的服务。
  输入 $0.15/M、输出 $0.60/M 仅为估价示例，**不是实际账单价格**。
* `agent.upstream_stream=false`（默认）向上游请求一次完整 JSON 响应，校验 usage 后转换为
  客户端 SSE，保留文本、推理内容、工具调用和结束原因。设为 true 可直通上游流；
  不会在异常响应后自动改模式重放请求。此设置参与评测 hash，旧模式缓存不能混用。
* `agent.concurrency` 为 1–5，所有代理共享上游并发上限；coding_eval 的独立题目
  并行执行，各有 HOME、workspace 与输出目录。EAB、候选和共享 target 编译仍串行。
  `retry_429_seconds=300` 支持 nginx HTML 429 的数分钟退避；上游使用浏览器式 UA。
  402/401/403、上游 5xx 和畸形响应会阻断并保留恢复点，不算候选能力失败。PATCH 超时为 1800 秒。
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
  此字段为兼容旧配置保留：macOS 自动选择 Seatbelt，Linux 选择 bubblewrap，
  后端缺失时失败关闭。macOS 限制文件读写目录，网络仅允许本地计量代理；
  EAB 另开放固定的本地服务监听端口及 18789 流中断故障服务；仅向独立 hook 采集脚本和 hook 日志文件授权，不开放评分器或评分文件。Linux 使用文件系统、PID 和网络 namespace。
  macOS 的 shell 使用继承外层 Seatbelt 的执行模式，不能重复安装嵌套 Seatbelt；
  子进程仍受外层约束。编码验收在单独的离线沙箱中执行，EAB 网关不可读取题库和评分目录。

## 首次准备

在锁定的可信 Blockcell 源码目录中安装 Rust 1.96.0（含 rustfmt/clippy），执行
`cargo fetch --locked` 预热依赖，并安装 `cargo-audit` 后执行 `cargo audit update-db`。
候选与基线构建都使用离线 Cargo；空缓存或缺少审计库会明确失败。
另外准备 EAB60 套件，将 `EAB60_KIT` 指向包含 `cases/blockcell_eab60.jsonl` 的目录。
完成接口、模型、单价与预算配置后先执行 `doctor`，再执行 `init`。

本发布不附带已审计的 Rust Agent 二进制。运行前请阅读 [SECURITY.md](SECURITY.md)
并独立评估所选 Parent 的依赖审计；“候选没有新增漏洞”不等于 Parent 没有漏洞。

## 命令

```sh
python -m genesis doctor
python -m genesis init
python -m genesis baseline
python -m genesis run --cycles 3
python -m genesis resume
python -m genesis status
python -m genesis report
python -m genesis reserve-usage --usd 1 --reason "依据请求上限保守预留，实际账单仍待核对"
python -m genesis --config /path/to/genesis.json run --cycles 3
python -m unittest discover genesis/tests -v
```

`init` 创建独立 clone、锁定 SHA 的 frozen worktree，串行编译 Genesis-0，
复制 release 二进制，然后经 AgentAdapter 执行 `--help`；不调用 LLM。
`baseline`、`run` 会调用配置的模型并产生费用。`resume` 继续已持久化的目标轮数。
同一 workspace 的变更命令使用跨进程排他锁，重复启动会明确失败；共享编译缓存仍串行使用。
coding 和 EAB 多题评测逐题保存可信侧 checkpoint；模型、评测配置或二进制改变后旧结果失效。
上游故障恢复后重跑 baseline 或 resume，只继续未完成题目，不将基础设施中断缓存为能力失败。

每轮 Parent 的 diagnose/patch/review 使用该 Parent 自己的二进制。
Parent 启动或标准修改自检失败时标为 UNFIT 并回退 Genesis-0；连续三轮全部
候选编译失败也会淘汰该 Parent。高风险路径最多 ARCHIVE，不可自动内部晋级。
PROMOTE 仅在独立 clone 的 detached worktree 中保存提交和谱系 SHA，不创建分支，
使用命令级 Genesis 身份，不修改全局 Git 身份。原仓库不提交、不 push。
高风险修改和自检失败的 ARCHIVE 不进入后续 Parent 抽样，包括探索抽样。

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
  repository/              # 独立 clone，detached 提交谱系
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
和提交 SHA 去重。意外中断的外部工具步骤可能重跑，恢复时重新应用每轮预算上限。

## 安全边界与实验限制

不可信原生代码具有广泛攻击面。这里的隔离不等同虚拟机；请在专用、无其他
秘密的实验机运行。Genesis-0 的锁定源码在宿主构建，候选构建和 cargo 门槛使用
去凭据、无网络 namespace、只读 Rust 工具链与 registry、可写共享 target。
因此新依赖须预先下载，cargo audit 的数据库也须预先准备；离线资源缺失会拒绝。
不要把本工具用于不可信第三方仓库或共享生产机。
上游请求发出后，最多 `agent.concurrency` 个已在途请求的 usage 可能使费用超过
剩余预算；随后请求会返回预算错误。无 usage 的响应单独记录 `missing_usage_requests` 并持久化阻断后续请求；
已知 token 和估算费用不会虚增。未知请求费用需先核对，不能作为零费用忽略。
若暂时无法拿到账单，可由操作者用 `reserve-usage` 明确预留足够的美元预算及说明后继续；
预留不是已知实际费用。`usage.json` 的 `reservations` 保留请求数、时间、原因和金额，
`reserved_cost` 与已知 `cost` 一起受累计及每轮预算约束。若无法确定足够的预留额，应保持阻断。
仅完成模拟闭环不代表真实 G-06 改进已获证实；真实进化效果必须以实验记录为准。

可选原生 HTTP 传输：当宿主 Python 的 TLS/DNS 环境不可用时，可运行
`cargo build --release -p blockcell-providers --example genesis_http`，将
`agent.http_transport_command` 设置为该可执行文件的 argv 列表。凭据仅通过 stdin
传递；HTTPS 仍验证证书，DNS 系统解析失败时使用 `dig`。此设置不改变模型和计量规则。
