# e2e 测试 Review 报告（Wave 5 · 周边）

> 只读审查 · 2026-10-16 · 子代理 w5-e2e

## 规模（文件数/用例数估计/入口）

| 项 | 数据 |
|---|---|
| tests/ 文件总数 | 76（67 个 .py + 3 个 .sh + README/manifest 等） |
| 测试相关代码分布 | **67 个 .py 全部在 `tests/coding_eval/`**；无 conftest.py、无 pytest 配置、无 `*e2e*` 命名文件（全仓 find 验证） |
| Python 用例估计 | 评测任务 20 个（manifest.json）+ fixture 验收测试方法 41 个 + harness 自测 3 个（`test_runner.py`）≈ 23 个顶层用例 / 41 个验收断言组 |
| 入口 | `tests/coding_eval/runner.py`（手工 CLI，默认 `--timeout 900`、`--max-tool-calls 100`）；`test_runner.py`（unittest 手工运行）；3 个 `.sh`（手工运行） |
| 唯一被 CI 执行的 | 仅 `webui` `npm test`（release.yml:87，8 文件 ~29 个用例）；`cargo test`（~2100 个 Rust 单测）在任何 workflow 中均未执行（ci.yml- 被主动禁用） |
| CI 状态 | `.github/workflows/ci.yml-` 改名禁用，且 `release_workflow_infrastructure.sh:13-16` **用测试强制 CI 保持禁用**（"until the project has Actions budget"） |

**结论先行**：`tests/` 不是产品 e2e 目录，而是 **agent 编码能力评测（benchmark）harness**（复制 fixture → 独立 git 仓库 → 跑被测 agent → `unittest discover` 验收）+ **3 个 shell 基础设施/策略测试**。docs/design/review-decomposition-2026-10-16.md:10 将 tests/ 标注为 "e2e，python" 与事实不符。

## 测试地图（harness 结构 + 覆盖矩阵：模块 → 有/无 e2e）

### Harness 结构（tests/coding_eval/）

```
runner.py          # 主编排：load_manifest 校验 → 每任务 copytree+git init/commit
                   # → 跑 --agent-command（env: BLOCKCELL_EVAL_WORKSPACE/TASK_ID）
                   # → 跑 verify.py → passed = returncode == 0 → 汇总 JSON 报告
common_verify.py   # 全部 20 个 verify.py 的共用实现：对 workspace 跑
                   # python -B -m unittest discover -p "test_*.py"（5 行 shim × 20 完全同构）
test_runner.py     # harness 自测：仅覆盖 load_manifest / classify_failure / summarize
tasks/*/repo/      # fixture：bugfix 6 + feature 6 + refactor 4 + greenfield 4
                   # （绿色字段外均自带会失败的验收测试；greenfield 另有 SPEC.md 一句话需求）
```
隔离性设计良好：每任务独立 tmprepo、自有 git baseline、`changed_files` 记录改动、无共享状态、无网络依赖（评测本体）。

### 覆盖矩阵（产品行为 → tests/ e2e）

| 产品链路 | tests/ e2e | Rust 单测（参考，供 W0-W4 定位缺口） |
|---|---|---|
| 消息处理链路 | ❌ 无 | crates/agent（785 例）/channels（171 例） |
| 工具调用 | ❌ 无 | crates/tools（482 例） |
| 沙箱确认（sandbox-exec 等） | ❌ 无（tests/ 内无任何 skip 守卫可查） | tools/agent 内单测（另有 macOS 环境限制，属 W0-W4 范围） |
| Gateway API / WS | ❌ 无 | bin/blockcell（167 例，单测级） |
| skill 加载 | ❌ 无 | crates/skills（132 例） |
| fork runtime / subagent | ❌ 无 | crates/agent |
| provider 调用 | ❌ 无 | crates/providers（98 例） |
| scheduler / storage / updater | ❌ 无 | 57 / 84 / 22 例 |
| install.sh / release.yml / 构建产物 | ⚠️ 有、且质量高（见亮点），但**仅手工运行** | — |
| agent 的 Python 代码编辑能力 | ✅ 20 任务 × 41 验收断言（评测产品本身，非产品行为） | — |

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| **P0** | tests/coding_eval/common_verify.py:8-13 | **验收可"零测试通过"（假通过）**：`unittest discover` 在发现 0 个测试时打印 `Ran 0 tests … OK` 并 **exit 0**（已在 /tmp 空目录实验复证）。agent 删掉/清空 workspace 里的 fixture 测试文件即可让 `passed = True` 拿满分。greenfield 任务的 prompt 还明确要求"自己补测试"，自写平凡测试同样可过 | runner.py:164 `passed = verification.returncode == 0` 无任何"至少跑了 N 个测试/测试文件未被篡改"的守卫；verify 前无 fixture 测试完整性校验 |
| **P1** | tests/（整体） | **产品 e2e 覆盖为零**：消息处理、工具调用、沙箱确认、gateway API、WS、skill 加载、fork runtime 在 tests/ 中均无 e2e；bin/blockcell 的 167 个单测是 gateway/CLI 唯一防线，且均为进程内单测，不起真实 axum server / WS 连接 | 全仓无 conftest.py / pytest 配置 / e2e 文件；唯一 CI 执行的测试是 webui npm test |
| **P1** | tests/coding_eval（整体）+ .github/workflows/ci.yml- | **无任何自动化门禁**：2100+ cargo 单测、python harness、3 个 .sh 全部手工触发；CI 被主动禁用且有测试**强制其保持禁用**；Makefile 无 test 目标；CONTRIBUTING.md:25-67 只教 `cargo test`，完全未提及 tests/ 资产，新人不可发现 | release.yml 仅 87 行 `npm test`；release_workflow_infrastructure.sh:13-16 |
| **P2** | tasks/greenfield-0{1..4}/repo/SPEC.md:1 + tasks/feature-01-batch prompt | **绿色字段/部分任务规格欠定义 → 评分有效性受损**：SPEC 一句话未规定测试强契约（如 `main` 返回计数值还是打印、`add` 返回带 `id` 的 dict、`batched` 返回 tuple 而非 list——test_iter_utils.py:5 钉死 tuple 但 prompt 未提）。合理替代实现会被误判失败 | SPEC 与隐藏测试契约不一致；manifest prompt 同样未提返回值 |
| **P2** | tests/build_supply_chain_infrastructure.sh:25-28 | **魔法常量导致版本演进即红**：硬编码 `length == 10`、`version == "0.1.7"`、`rust_version == "1.85"`、`authors == ["blockcell"]`。每次发版/Rust 升级都会误报；且与 release_workflow_infrastructure.sh:18 的动态 `workspace_version=` 读取手法不一致 | 两脚本对同一关切（版本一致性）采用静态/动态两种策略 |
| **P2** | runner.py:127-189 vs test_runner.py:12-87 | **harness 自测避重就轻**：`test_runner.py` 只测 `load_manifest`/`classify_failure`/`summarize` 三个纯函数；真正决定通过与否的编排（`run_task`/`_prepare_workspace`/`_load_metrics`/`_agent_argv`）零覆盖——恰好是 P0 假通过路径所在层 | 自测函数清单见 test_runner.py:13,28,58 |
| **P3** | runner.py:144-158 | `--agent-command` 二进制不存在时 `FileNotFoundError` 未捕获 → 整个 20 任务 run 崩溃（traceback），无逐任务错误隔离；此时 metrics 全默认 0，即使恢复也会把所有失败归因为 `verification_missing` | 仅捕获 `TimeoutExpired` |
| **P3** | runner.py:214-229 + README.md:12 | 默认 `--output` 落在仓库内（`tests/coding_eval/latest-report.json`），未被 .gitignore 忽略（grep 验证），有误提交大报告风险 | .gitignore 无 eval 条目 |
| **P3** | runner.py:200 + README.md:27 | 全量时长风险：20 任务 × 900s agent 超时（+ 验收）≈ 最坏 5h+；README 建议每周一次可接受，但无并行/断点续跑能力 | `--timeout 900` 默认 |
| **P3** | tests/ 根目录 | 目录名"tests/"与其内容（benchmark + 基础设施测试）错位，根目录无 README 指引；发现性差 | coding_eval 有自有 README，但 tests/ 根无说明 |
| **P3** | install_release_infrastructure.sh:73,79 | fake `cp`/`mv` 包装硬编码 `/bin/cp`、`/bin/mv`；在现代合并不是 /bin symlink 到 /usr/bin 的发行版上会失效（当前 macOS/Linux 主流环境无碍） | 见脚本文内 fake |

### 已验证为"非问题"的点
- **无静默 skip / 假通过守卫**：67 个 .py 全文 grep `skip|Skip` 零命中；无 `return 0` 吞错路径（唯一软失败是 `_load_metrics` 返回 `{}`，只影响归因而非通过判定）。
- **稳定性优良**：无 `time.sleep` 硬等待（TTL 缓存测试用注入 Clock，feature-03-ttl-cache/repo/test_cache.py:4-6，堪称范本）；curl/git/cargo/npm 全部 fake（install 测试零真实网络）；每任务独立 temp git repo 无共享状态；任务间无顺序依赖。
- **安全清洁**：无生产凭据、无真实 token（仅 `v9.8.7`、`coding-eval@blockcell.local` 等虚构值），符合 CONTRIBUTING.md:11 约定。
- fixture 验收测试质量普遍高：真实行为断言、覆盖边界（除零、空输入、非法值、无输入变异），refactor-02 甚至源码级检查生产调用方已迁移（test_names.py:10-11）。
- 沙箱测试在 tests/ 中不存在，用户提示的 macOS `sandbox-exec` 天然跳过问题不适用于本目录（应回到 Rust 单测侧核查）。

## 亮点

1. **benchmark harness 设计规范**：manifest 驱动 + 字段/重复 id 校验（runner.py:24-37）、每任务独立 git baseline、原子化报告、失败四分类归因（`subagent_coordination`/`verification_missing`/`navigation`/`editing_failure`）优先级明确，metrics 契约文档化（README.md:15-25）。
2. **fixture 测试本身是 TDD 范本**：先红后绿结构、注入时钟而非 sleep、`assertRaisesRegex("line 2")` 精确错误信息、deep_merge 的输入不变异断言、README 存在性也纳入验收。
3. **shell 基础设施测试超出预期地扎实**（虽是"测试测试"）：source/release/auto 三条安装路径全 fake 化，校验原子替换（tmp→mv）、checksum 篡改拒绝、tar 路径穿越拒绝、完整性失败后不回退 source（install_release_infrastructure.sh:253-281，防供应链降级攻击）；release workflow 侧校验 Action pin 40-hex SHA、无全局 permissions、版本/tag 一致性。
4. `verification` 与 agent 共用 timeout 参数、agent 输出只留 20KB 尾部，报告体积可控。

## 建议

1. **（P0，必修）给验收加完整性守卫**：verify 前将 fixture 的 `test_*.py`/源文件从原始 fixture 路径重新覆盖回 workspace，或对 fixture 测试做 sha256 快照比对；并断言 `tests_run >= 1`（DISCOVER 输出解析 `Ran N tests`，N=0 即失败）。
2. **（P1）补一条真正的产品 e2e**：优先覆盖最高风险且 Rust 单测最薄弱的跨进程链路——起真实 `blockcell gateway`（axum）实例，用 Python 驱动 HTTP API + WS 连接（python 标准库即可，无新增依赖），至少覆盖：消息收发一轮、WS 事件流、gateway 健康/版本端点。频道消息链路可作为第二步。
3. **（P1）恢复 CI 或至少加本地 `make test`**：把 `cargo test --workspace` + `python -m unittest discover -s tests/coding_eval`（排除需 key 的）+ 3 个 .sh 收进一个 Makefile 目标并写进 CONTRIBUTING；若 Actions 预算仍受限，提供 pre-push 钩子脚本降级方案。
4. **（P2）去除魔法常量**：build_supply_chain_infrastructure.sh 改为从 `cargo metadata` 读 workspace 版本并与 git tag 比对（与 release workflow 手法统一）；crate 数量/作者/license 用 `jq` 提取期望值而非硬编码。
5. **（P2）规格补齐**：greenfield SPEC 明示返回值契约（或把"测试即规格"写进 prompt）；feature-01 明确返回元素类型。
6. **（P2/P3）harness 自测补编排层**：至少覆盖"空测试目录 → 非零退出"（P0 修复的回归测试）与 `_agent_argv` 的 shlex/quoted-prompt 行为。
7. **（P3）健壮性与卫生**：runner 捕获 `OSError` 保证逐任务错误隔离；`latest-report.json` 加入 .gitignore；tests/ 根加 README 说明两类资产（benchmark vs 基础设施测试）与运行方式。
