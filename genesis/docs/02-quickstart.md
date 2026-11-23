# 快速开始

[文档首页](README.md) · 上一篇：[概览](01-overview.md) · 下一篇：[架构](03-architecture.md)

## 先选择运行范围

只阅读、修改和运行 Python 回归测试，不需要 Provider key 或 EAB60：

```sh
python3 -m unittest discover -s genesis/tests -v
python3 -m genesis --help
```

完整回归套件会绑定本地回环端口，并按平台运行系统沙箱测试。若 Python 的站点初始化改动了 HTTPS 默认配置，可用 `python3 -S -m unittest discover -s genesis/tests -v` 排除站点初始化的影响。

完整实验还需具备以下资源。请在专用实验环境准备，先阅读 [SECURITY.md](../SECURITY.md)。

| 资源 | 要求 |
|---|---|
| Python | 3.10+，无需第三方 Python 包 |
| Git | 可 clone、创建 detached worktree；锁定提交必须可获取 |
| Rust | 仓库指定的 1.96.0，含 rustfmt、clippy；构建需要较多时间和磁盘空间 |
| 隔离后端 | macOS 为系统 `sandbox-exec`；Linux 为 bubblewrap，后端路径仍待独立验收 |
| 离线缓存 | Parent 与候选所需 Cargo 依赖，以及 cargo-audit 的 advisory 数据库 |
| 模型服务 | OpenAI 兼容 Chat Completions，支持当前 Agent 所需的工具调用与有效 usage |
| EAB60 | 外部套件，包含 `cases/blockcell_eab60.jsonl`，固定版本和来源 |

先在与 `genesis0_sha` 对应的**可信源码**目录预热依赖与审计库：

```sh
rustup toolchain install 1.96.0 --component rustfmt --component clippy
cargo fetch --locked
cargo install cargo-audit --locked
cargo audit update-db
```

这些准备操作需要联网。使用与 Supervisor 相同的 `CARGO_HOME` 准备缓存。Genesis-0 虽在宿主构建，也使用 Cargo 离线模式。候选构建和 L0 门禁同样依赖预热后的资源。

## 准备独立配置

复制默认配置再修改，避免不同实验共用一个工作区：

```sh
mkdir -p tmp
cp genesis/genesis.json tmp/genesis.local.json
```

编辑 `tmp/genesis.local.json`：

1. `genesis.repository` 指向准备使用的可信仓库，支持本地路径或 Git URL。
2. `base_sha` 与 `genesis0_sha` 设为同一个已审核的完整提交 SHA。v0.1 要求二者相等。
3. `workspace_root` 指向新的实验目录，例如 `.genesis-local`。
4. 填写模型、接口、实际输入/输出单价和可接受的预算。
5. 保持 `agent.require_bwrap=true`，配置 EAB60 套件路径。

仓库路径和配置路径是两回事：本地仓库中的**未提交改动不会进入 clone 的锁定 SHA**。要验证新版本，先形成可获取的源码版本，再设置 SHA；本文命令不会替你提交。

`workspace_root` 的相对路径按运行命令的当前目录解析，不按配置文件所在目录解析。请始终从仓库根目录运行，或使用绝对路径。

## 配置模型环境

示例使用通用 OpenAI 兼容 Provider；地址与模型请换为实际支持的组合，默认 JSON 中的组合不是可用性保证。

```sh
export OPENAI_BASE_URL='https://your-provider.example/v1'
export OPENAI_MODEL='your-model-id'
export EAB60_KIT='/absolute/path/to/eab60-kit'
```

在 macOS 默认 zsh 中不回显地输入 key：

```sh
read -rs 'OPENAI_API_KEY?API key: '
export OPENAI_API_KEY
```

在 bash 中对应命令为 `read -rs -p 'API key: ' OPENAI_API_KEY`，随后 `export OPENAI_API_KEY`。不要把真实 key 放进 JSON、命令行参数、截图或公开日志。

## 检查、初始化和基线

```sh
python3 -m genesis --config tmp/genesis.local.json doctor
python3 -m genesis --config tmp/genesis.local.json init
python3 -m genesis --config tmp/genesis.local.json baseline
```

`--config` 放在子命令之前。三个步骤作用不同：

- `doctor` 检查工具可发现、必选套件和 key 环境变量等前置条件。它不请求模型，也不证明缓存齐全或沙箱权限实测通过。
- `init` 创建独立 clone 与 frozen worktree，构建并复制 Genesis-0 二进制，执行 `--help`；不调用 LLM。初始源码被视为可信，在宿主构建。
- `baseline` 测量 Genesis-0 的能力。默认包含三次 coding_eval，以及 EAB dev 和 holdout。未命中缓存的评测可能产生模型费用，费用计入工作区总预算。检查 `baseline.json`，确认必选评测均返回 `ok`、题集完整，再启动实验。

确认基线可解释、预算足够，再启动一轮：

```sh
python3 -m genesis --config tmp/genesis.local.json run --cycles 1
python3 -m genesis --config tmp/genesis.local.json status
python3 -m genesis --config tmp/genesis.local.json report
```

默认一轮产生两个候选，并依次进行补丁与构建。`concurrency=5` 表示共享模型请求最多五个在途，不表示五个候选并行构建。

## 如何确认完成

先用 `status` 查看阶段和目标轮数。`IDLE` 且当前轮数达到目标轮数表示本次目标已完成。`STOPPED` 时查看状态 payload 中的停止原因或停滞计数。再读 `report.md`、`report.json` 和 `runs/<agent_id>/decision.json`，核对已记录候选的判定与理由。逐题报告放在对应评测目录，累计费用见 `usage.json`。

如因上游故障中断，先定位并修复原因，再执行：

```sh
python3 -m genesis --config tmp/genesis.local.json resume
```

`resume` 继续已保存的目标轮数；它不是追加一轮，也不会自动解除预算或未知用量阻断。详见[运行与排障](07-operations.md)。
