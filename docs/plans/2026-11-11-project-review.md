# 项目分批 Review 执行计划

> 执行方式：参考 `superpowers:writing-plans`、`superpowers:executing-plans` 的任务分解与检查点要求；独立模块按 `superpowers:dispatching-parallel-agents` 并行。用户已授权制定计划并执行全部审查，无需在批次间重复请求启动确认。

**Goal:** 审查当前项目的关键执行路径，输出可定位、可解释、可复核的缺陷与后续修复顺序。

**Architecture:** 按依赖层和信任边界拆分批次。模块审查可独立开展，跨模块调用链和发现统一串行复核；审查业务代码只读，新增计划与报告。

**Tech Stack:** Rust workspace（9 个库 crate + CLI/Gateway）、React/TypeScript、SQLite、Python 评测、Shell/GitHub Actions。

## 1. 基线与约束

- 日期：2026-11-11；分支必须为 `main`，不创建分支或 worktree。
- 基线：`5ca38be861b4a3e8f7a1e8d332e242b42ed25710`。
- 范围：当前受跟踪代码；初始未跟踪的 `docs/reviews/`、`tests/eval_kit/`、`tests/realworld48/` 仅作为背景，不修改、不作为已提交产品基线。
- 初始规模：`crates/bin/webui/src/tests/.github` 等选定目录内 624 个受跟踪文件；受跟踪 Rust/TS/TSX/Python/Shell 共约 278,827 行。
- 保留既有修改和报告，不执行部署、外部消息、真实渠道调用、自更新或模型付费评测。
- 本轮不顺带修复业务代码，不提交 Git；报告提出最小修复方向。
- 这是覆盖全部主要模块的风险导向审查，不声称逐行穷尽全部代码；每批必须说明实际精读范围和未验证部分。

## 2. 分级与证据标准

- **P0**：常见配置下可直接造成严重越权、不可逆数据损坏或全局不可用，需立即处理。
- **P1**：明确可达的安全边界失效、主要流程错误、重要数据丢失，应优先处理。
- **P2**：特定输入、配置或竞争条件下的功能错误或可靠性缺陷。
- **P3**：维护或体验改进，单独列出，不与确定缺陷混计。
- 每项必须包含：稳定 ID、级别、文件及行号、触发前提、实际行为、影响、证据类型和修复建议。
- 静态调用链证据和实际执行结果分开记录。历史报告中的问题必须重新核对当前实现，已修复的不得重复列为当前缺陷。
- 只有具体可达的缺陷进入确认清单；无法证实的风险进入待验证项。

## 3. 批次与检查点

| 批次 | 范围 | 重点检查 | 验证/产出 | 执行状态 |
| --- | --- | --- | --- | --- |
| B0 基线 | manifests、工具链、工作区、测试入口、历史报告 | 版本与构建前提、可执行测试、已有变更保护 | `00-baseline.md`、验证日志 | 完成（Rust 测试环境阻断） |
| B1 基础与存储 | `crates/core/src/`、`crates/storage/src/` | 配置默认值/合并、路径与会话身份、预算、事务、持久化及删除语义 | `01-foundation-storage.md` | 完成：P1×1 / P2×4 |
| B2 Agent | `crates/agent/src/` | 主消息生命周期、取消与子代理、工具权限管线、技能资源、验证证据、记忆/缓存隔离 | `02-agent.md` | 完成：P1×2 / P2×2 |
| B3 工具与技能 | `crates/tools/src/`、`crates/skills/src/` | shell/exec 沙箱、只读技能资源、文件/网络边界、技能发现和热更新、嵌套工具 | `03-tools-skills.md` | 完成：P1×2 / P2×3 |
| B4 外部服务 | `crates/providers/src/`、`crates/channels/src/`、`crates/scheduler/src/` | 流式解析/重试/降级、账号及会话隔离、鉴权/回调、调度幂等和停机 | `04-services.md` | 完成：P1×1 / P2×3 |
| B5 入口与前端 | `bin/blockcell/src/`、`webui/src/` | HTTP/WS 鉴权、配置读写/密钥、文件下载、会话同步、异步状态及 API 契约 | `05-entry-webui.md` | 完成：P1×1 / P2×2 |
| B6 发布与评测 | `crates/updater/`、build、`.github/`、安装入口、受跟踪 `tests/`、文档抽查 | 签名/解包/回滚、CI 是否触发、评测退出状态/隔离、说明与实现一致性 | `06-delivery-evaluation.md` | 完成：P1×2 / P2×1 |
| B7 复核与汇总 | B1–B6 的发现及跨模块调用者 | 去重、可达性、等级、历史修复回归、验证限制、修复排序 | `README.md` 和 `07-cross-check.md` | 完成：合计 P1×9 / P2×15 |

各批执行顺序：枚举模块入口 → 精读高风险函数及调用方 → 核对已有测试和历史结论 → 用无副作用的最小检查验证 → 写出报告及覆盖限制。B1/B2/B3 可并行；B4/B6 可在空闲审查单元开始；B7 依赖前面报告。

## 4. 验证命令与安全执行范围

1. 基线：`git status --short`、`git branch --show-current`、`git rev-parse HEAD`、`rustc --version`、`cargo --version`、`node --version`。
2. Rust：先尝试 `cargo test --workspace --offline --locked`；若因编译器或离线缓存被阻断，保留原始日志，不把失败归因于业务实现，不改工具链或 lockfile。能执行时按模块进一步筛选测试。
3. 前端：`npm test --prefix webui`；`webui/node_modules/.bin/tsc -b webui/tsconfig.json --pretty false`（采用项目已有检查，不发布应用）。
4. Python：先阅读 `tests/README.md` 和测试代码，再对受跟踪、无需外部模型或服务的 harness 单测执行 `python3 -m unittest ...` / 其既有运行命令。不将故意含 bug 的 coding task fixture 作为项目回归测试执行。
5. 发布/安装基础设施：先读取对应 shell 脚本，确认 mock/临时目录隔离后执行适用脚本；真实更新和安装不运行。
6. 如复现会读写真实工作目录、启动外部系统或发网络请求，改用临时目录或静态调用链验证并注明局限。

## 5. 完成标准

- B0–B7 均有记录，主要模块均进入分批审查；每批列出实际审查范围，不能以“未发现”代替覆盖说明。
- 确认清单中每项有当前源码位置、触发条件和证据，主审完成重点项复核，重复根因合并。
- 所有运行过的验证均有命令、结论和限制；环境阻断与产品缺陷分开。
- 汇总提供 P0/P1 优先项、全部发现索引、下一轮修复分组，以及真实渠道/性能/平台等未覆盖内容。
- 最终确认仍在 `main`，原有未跟踪文件保持原样，业务源码未因本轮审查修改。

## 6. 执行记录

批次状态、发现统计和验证结果以 [本轮报告索引](../reviews/2026-11-11-project-review/README.md) 为准。

2026-11-11 已执行并收口。现有单测 61 项、TypeScript 检查及 3 项基础设施脚本通过；Rust workspace 因工具链 1.92 低于要求 1.96 未进入编译/测试。24 项当前确认发现与未验证/被前置故障阻断的内容分别记录。只新增审查文档，未修复业务代码。
