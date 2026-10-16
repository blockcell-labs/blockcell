# Batch 6 修复清单（闭环 · 第一部分：P0 验收完整性 + 本地门禁）

> 基线 HEAD `926faf6`（Batch 0-5 已提交）· 开始 2026-10-16 · 来源：`review-2026-10-16/summary.md` §3-I + §7 Batch 6
> 状态标记：⬜ 待办 / 🔄 进行中 / ✅ 完成 / ⏸️ 阻塞 / ⏭️ 有意不做（附理由）

## 范围说明

Batch 6 含四块工作，体量明显大于前几批，经确认拆为两部分：

- **本部分（已完成）**：6A = P0-8 验收假通过；6B = 本地测试门禁。
- **下一部分（待办）**：6C = 补单测（scheduler cron 时间计算 / updater ed25519 验签 /
  skills evolution 部署链路 / MCP 协议级）；6D = 文档同步（docs/03、09、12、17、20）。

## 修复项

| # | 项 | 位置 | 状态 | 改动摘要 |
|---|---|---|---|---|
| 6A.1 | 验收可「零测试通过」（P0-8） | `tests/coding_eval/common_verify.py` | ✅ | 两道守卫：① 跑测试前从**原始 fixture** 恢复 `test_*.py`（删/改/清空验收测试无效）；② 解析 `Ran N tests`，`N < 1` 直接以独立退出码 `2` 拒绝通过（此前 `unittest discover` 在 0 测试时打印 `Ran 0 tests … OK` 且 exit 0） |
| 6A.2 | runner 通过判定无守卫 | `tests/coding_eval/runner.py` | ✅ | `passed = returncode == 0 AND 验收至少跑了 1 个测试`（独立解析验收输出，不信任 agent 自报的 metrics 文件）；结果新增 `acceptance_tests_run` / `fixture_tests_restored`；0 测试时归因为 `verification_missing` |
| 6A.3 | harness 自测避重就轻 | `tests/coding_eval/test_runner.py` | ✅ | 新增 5 例覆盖编排层：空测试目录 → 非零退出（P0 回归）、fixture 测试被删后恢复、fixture 测试被换成平凡通过版仍按原测试判定、`parse_tests_run` 解析、`_agent_argv` 的 shlex/引号行为 |
| 6B.1 | 无任何自动化门禁 | `Makefile`、`.githooks/pre-push`（新增） | ✅ | `make test` = `test-rust` + `test-python` + `test-infra`；`test-rust` 前置 `webui-dist`（dist 过期自动 `npm run build`，否则 `build.rs` 哈希断言会让整仓测试失败）；`make hooks` 安装 `core.hooksPath=.githooks`，pre-push 跑 `make test`（`BLOCKCELL_SKIP_HOOKS=1` 可单次跳过） |
| 6B.2 | 门禁与 tests 资产不可发现 | `CONTRIBUTING.md` | ✅ | 新增「本地测试门禁」与「tests/ 目录」两节（双语），说明 `make test` / `make hooks` / `make test-webui`、`webui/dist` 过期自动重建、`tests/` 两类资产（benchmark harness vs 基础设施脚本）与验收守卫语义 |
| 6B.3 | `latest-report.json` 可能被误提交 | `.gitignore` | ✅ | 忽略 `tests/coding_eval/latest-report.json`（runner 默认输出位置） |

## 反证（P0 修复有效性的直接证据）

用真实 runner + 一个「作弊 agent」（`--agent-command 'sh -c "rm -f test_math_utils.py"'`）：

| 场景 | `passed` | `acceptance_tests_run` | `fixture_tests_restored` | 验收尾部 |
|---|---|---|---|---|
| 基线（无 agent，fixture 未修） | `false` | 2 | 0 | `FAILED (errors=1)` |
| 作弊：**删除** fixture 测试 | `false` | 2 | **1** | `FAILED (errors=1)` |
| 作弊：**清空** fixture 测试 | `false` | 2 | **1** | `FAILED (errors=1)` |
| 空 workspace 且 fixture 无测试 | — | 0 | 0 | 守卫以退出码 `2` 拒绝（`refusing to certify`） |

即：修复前「删掉验收测试」= 0 测试 = exit 0 = 满分；修复后测试被恢复、真实跑 2 个并失败。

## 验证

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| V1 | `make test`（完整本地门禁） | ✅ | exit 0，6m53s |
| V2 | `cargo test --workspace` | ✅ | 31 个测试目标，**2210 passed / 0 failed** |
| V3 | `python3 -m unittest discover -s tests/coding_eval` | ✅ | **8 passed**（原 3 + 新增 5） |
| V4 | 3 个基础设施脚本 | ✅ | build supply-chain / install release / release workflow 全部 `ok` |
| V5 | `make -n test` / `make -n hooks` | ✅ | 目标展开正确（recipe 行均为真实 tab） |

## 提交

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| G1 | 提交 A：6A（P0 守卫 + harness 自测 + .gitignore） | ✅ | 见下方 |
| G2 | 提交 B：6B（Makefile + pre-push 钩子 + CONTRIBUTING） | ✅ | 见下方 |
| G3 | 提交 C：本文件 + summary 状态更新 | ✅ | 见下方 |

## 有意不做 / 留待

- **不恢复 `ci.yml`**（用户决定）：尊重仓内「等 GitHub Actions 预算」的既有决策
  （`tests/release_workflow_infrastructure.sh:13-16` 仍断言 CI 保持禁用），改用
  `make test` + pre-push 钩子的降级方案。要恢复 CI 只需 `git mv .github/workflows/ci.yml- .github/workflows/ci.yml`
  并同步改掉那条断言。
- **6C / 6D 下一轮**：cron 时间计算（时区/DST/6 字段）、ed25519 验签（合法/篡改字节/篡改载荷字段/缺签名/错公钥）、
  evolution 部署-契约-观察-回滚、MCP 协议级测试；docs/03、09、12、17、20 同步。
- **e2e 其余 P2/P3 未做**：`runner.py` 捕获 `OSError` 做逐任务错误隔离、`tests/` 根 README、
  `build_supply_chain_infrastructure.sh` 的魔法常量（`length == 10` / `version == "0.1.7"`）、
  greenfield SPEC 返回值契约、harness 并行/断点续跑。

## 中断恢复指引

1. 进度即本文件状态列；⬜ 从第一个未完成项继续。
2. 环境：`unset BASH_ENV`；PATH 用 `/bin:/usr/bin:/sbin:/usr/sbin:/usr/local/bin`
   （`/usr/bin/ls`、`/usr/bin/cat`、`/bin/grep` 均不存在；读文件用 Read 工具）。
3. `make test-rust` 会自动重建过期 `webui/dist`；若手工跑 `cargo test`，先确认
   `node webui/scripts/source-hashes.mjs --check` 退出码为 0。
4. 6A 的语义约束：`common_verify.py` 的「恢复 fixture 测试」与「`Ran N tests >= 1`」是两道**独立**守卫，
   缺一都会重新打开 P0（前者防删改，后者防「压根没有测试」）；`runner.py` 的解析是第三层，
   用于防御绕过 `common_verify` 的 verify 脚本。
