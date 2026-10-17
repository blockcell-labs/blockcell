# Batch 6 修复清单（闭环：P0 验收完整性 + 本地门禁 + 补单测 + 文档同步）

> 基线 HEAD `926faf6`（Batch 0-5 已提交）· 开始 2026-10-16 · 来源：`review-2026-10-16/summary.md` §3-I + §7 Batch 6
> 状态标记：⬜ 待办 / 🔄 进行中 / ✅ 完成 / ⏸️ 阻塞 / ⏭️ 有意不做（附理由）

## 范围说明

Batch 6 含四块工作，体量明显大于前几批，分两轮完成：

- **第一部分（已完成）**：6A = P0-8 验收假通过；6B = 本地测试门禁。
- **第二部分（已完成）**：6C = 补单测（updater ed25519 / scheduler cron 时间计算 /
  skills evolution 部署链路 / MCP 协议级）；6D = 文档同步（docs/03、09、12、17、20）。
- **CI 有意不恢复**：尊重仓内「等 GitHub Actions 预算」的既有决策，改用本地门禁降级
  （见 6B）。

## 修复项

| # | 项 | 位置 | 状态 | 改动摘要 |
|---|---|---|---|---|
| 6A.1 | 验收可「零测试通过」（P0-8） | `tests/coding_eval/common_verify.py` | ✅ | 两道守卫：① 跑测试前从**原始 fixture** 恢复 `test_*.py`（删/改/清空验收测试无效）；② 解析 `Ran N tests`，`N < 1` 直接以独立退出码 `2` 拒绝通过（此前 `unittest discover` 在 0 测试时打印 `Ran 0 tests … OK` 且 exit 0） |
| 6A.2 | runner 通过判定无守卫 | `tests/coding_eval/runner.py` | ✅ | `passed = returncode == 0 AND 验收至少跑了 1 个测试`（独立解析验收输出，不信任 agent 自报的 metrics 文件）；结果新增 `acceptance_tests_run` / `fixture_tests_restored`；0 测试时归因为 `verification_missing` |
| 6A.3 | harness 自测避重就轻 | `tests/coding_eval/test_runner.py` | ✅ | 新增 5 例覆盖编排层：空测试目录 → 非零退出（P0 回归）、fixture 测试被删后恢复、fixture 测试被换成平凡通过版仍按原测试判定、`parse_tests_run` 解析、`_agent_argv` 的 shlex/引号行为 |
| 6B.1 | 无任何自动化门禁 | `Makefile`、`.githooks/pre-push`（新增） | ✅ | `make test` = `test-rust` + `test-python` + `test-infra`；`test-rust` 前置 `webui-dist`（dist 过期自动 `npm run build`，否则 `build.rs` 哈希断言会让整仓测试失败）；`make hooks` 安装 `core.hooksPath=.githooks`，pre-push 跑 `make test`（`BLOCKCELL_SKIP_HOOKS=1` 可单次跳过） |
| 6B.2 | 门禁与 tests 资产不可发现 | `CONTRIBUTING.md` | ✅ | 新增「本地测试门禁」与「tests/ 目录」两节（双语），说明 `make test` / `make hooks` / `make test-webui`、`webui/dist` 过期自动重建、`tests/` 两类资产（benchmark harness vs 基础设施脚本）与验收守卫语义 |
| 6B.3 | `latest-report.json` 可能被误提交 | `.gitignore` | ✅ | 忽略 `tests/coding_eval/latest-report.json`（runner 默认输出位置） |
| 6C.1 | **ed25519 验签**（供应链最关键路径）与版本比较零单测 | `crates/updater/src/verification.rs`、`manager.rs` | ✅ | +15 例：合法签名通过 / 篡改消息 / 错公钥 / base64 签名 / 畸形签名编码 / 畸形公钥；`verify_signature` 合法签名 / **篡改 url** / **篡改 version** / 错公钥 / 缺签名；`version_greater` / `version_satisfies`（语义比较非字典序、`v` 前缀、预发布、非 semver 的数字回退） |
| 6C.2 | cron 时间计算（时区/DST/6 字段）零单测 | `crates/scheduler/src/cron_service.rs` | ✅ | +10 例：`At`（过去/未来/无 deadline）、`Every`（`run_immediately` 两态 + 无间隔）、`Cron`（首次调度不立即触发、时区解释（09:00 CST == 01:00 UTC）、DST 观测区（本地正午恒为 12:00，UTC 为 16/17 时）、非法表达式）、`parse_timezone`（IANA 名 / 未知 zone） |
| 6C.3 | evolution 部署/契约/观察/回滚零单测 | `crates/skills/src/evolution/tests.rs` | ✅ | +16 例：`contract_check`（PromptTool 通过 / 缺 `## Prompt` / 缺 `## Shared` / 过短、Hybrid 缺 SKILL.md、Hybrid+PromptOnly 用 patch.diff、LocalScript 的 SKILL.md 软检查**特征化**）、`check_observation`（新窗口=仍在观察 / 超阈值=回滚 / 到期=完成 / 无窗口=错误）、`deploy_and_observe` 前置拒绝（未编译 / 审计未过 / 无审计）、staged 无 baseline 的回滚删除主目录 |
| 6C.4 | MCP 零协议级测试 | `crates/tools/src/mcp/client.rs` | ✅ | +7 例：用 python3 假 stdio MCP server 驱动**真实协议**——`initialize` 握手 + `tools/list` 缓存（含 inputSchema）、`tools/call` 文本返回、`isError` 载荷、JSON-RPC error、服务端中途退出、二进制缺失、`initialize` 超时 |
| 6D.1 | docs/03 `web_search` 后端失真 | `docs/03_tools_system.md` | ✅ | 「DuckDuckGo（无需 API Key）」→ Brave + Baidu（**均需配置 Key**，中文优先 Baidu、失败回退另一后端） |
| 6D.2 | docs/03 `exec` 超时失真 | `docs/03_tools_system.md` | ✅ | 「默认 30 秒」→ 60 秒（`default_exec_timeout()`） |
| 6D.3 | docs/09 进化链路整体漂移 | `docs/09_self_evolution.md` | ✅ | 状态机改为实际 11 态（+ legacy 状态说明）；**删除不存在的 Testing 阶段**（ShadowTestResult 是死代码）；灰度 10%/50%/100% → **全量部署 + 60min 观察窗口（错误率 >10% 即回滚）**；`tool_versions/` → 技能目录内 `versions/`；安全边界「只能改 Rhai」→ 四形态 + 非 native 能力；审计改为**双层**（静态 + LLM）；补契约检查环节 |
| 6D.4 | docs/12 自升级章节过期 | `docs/12_architecture.md` | ✅ | 签名改为 `requireSignature` **门控**（默认值 = 编译期是否嵌入公钥）；补「应用前重建信任链」（staging 元数据重新验签 + 重算 SHA）；`atomic_replace` 单备份示例 → 锁 + 多备份 + 前后健康检查 + 自动回滚 + 维护窗口；注明 `download → apply` 的手动升级/降级语义 |
| 6D.5 | docs/17 `alerts add --source` 格式失真 | `docs/17_cli_reference.md` | ✅ | `tool:action:param` → **JSON 工具调用对象**（须含字符串 `tool` 字段），示例同步修正 |
| 6D.6 | docs/17 `-v/--verbose` 是死代码 | `bin/blockcell/src/main.rs` | ✅ | 选择**真接线**而非删文档承诺：`-v/--verbose` 现在强制 debug 日志等级（覆盖配置），flag 不再是空转 |
| 6D.7 | docs/20 承诺的「冷却保底」未实现 | `docs/20_provider_pool.md`、`crates/providers/src/pool.rs` | ✅ | 「若全部条目都在冷却中则临时解除冷却」→ 「无健康条目即返回 `None`」；同步修正 `acquire` 的注释与误导性私有函数名 `healthy_or_cooling_fallback_indices` → `healthy_candidate_indices`（**不改行为**：复用限速中的后端只会继续撞限流） |

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
| V1 | `make test`（完整本地门禁） | ✅ | exit 0（第一部分 6m53s；第二部分 3m40s） |
| V2 | `cargo test --workspace` | ✅ | **2257 passed / 0 failed**（第一部分 2210） |
| V3 | `python3 -m unittest discover -s tests/coding_eval` | ✅ | **8 passed**（原 3 + 新增 5） |
| V4 | 3 个基础设施脚本 | ✅ | build supply-chain / install release / release workflow 全部 `ok` |
| V5 | `make -n test` / `make -n hooks` | ✅ | 目标展开正确（recipe 行均为真实 tab） |
| V6 | 受影响 crate 单测（6C） | ✅ | updater **37**（+15）/ scheduler **68**（+10）/ skills **147**（+15）/ tools **494**（+7）/ providers **98** |
| V7 | `cargo clippy`（改动 crate + bin） | ✅ | exit 0；改动文件零新增警告（bin 的 4 条既有告警都在未改动的 `agent.rs`/`evolve.rs`/`gateway.rs`/`tools_cmd.rs`） |
| V8 | `cargo fmt --all --check` | ✅ | 本轮改动文件 fmt 干净 |

## 提交

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| G1 | 提交 A：6A（P0 守卫 + harness 自测 + .gitignore） | ✅ | `9558bcc` |
| G2 | 提交 B：6B（Makefile + pre-push 钩子 + CONTRIBUTING） | ✅ | `61bf1ee` |
| G3 | 提交 C：第一部分文档 | ✅ | `17f8ca5` |
| G4 | 提交 D：6C.1 updater 验签/版本比较单测 | ✅ | 见下方 |
| G5 | 提交 E：6C.2 scheduler cron 时间计算单测 | ✅ | 见下方 |
| G6 | 提交 F：6C.3 skills evolution 部署链路单测 | ✅ | 见下方 |
| G7 | 提交 G：6C.4 MCP 协议级测试 | ✅ | 见下方 |
| G8 | 提交 H：6D 文档同步（含 `-v` 接线与 pool 注释/命名） | ✅ | 见下方 |
| G9 | 提交 I：本文件 + summary 状态更新 | ✅ | 见下方 |

## 有意不做 / 留待

- **不恢复 `ci.yml`**（用户决定）：尊重仓内「等 GitHub Actions 预算」的既有决策
  （`tests/release_workflow_infrastructure.sh:13-16` 仍断言 CI 保持禁用），改用
  `make test` + pre-push 钩子的降级方案。要恢复 CI 只需 `git mv .github/workflows/ci.yml- .github/workflows/ci.yml`
  并同步改掉那条断言。
- **updater `apply` 的降级门控未加**（updater.md P1 建议 #1）：该改动会改变
  `upgrade download → apply` 的「手动升级/降级」语义，属行为变更而非测试/文档范畴，
  需要单独决策。已在 docs/12 中把该语义写明。
- **providers「冷却保底」未实现**（选择改文档而非改行为）：若全部 provider 都在冷却，
  临时解除冷却只会继续撞限流；保持返回 `None` 更安全。已把文档与代码注释、函数名
  统一到实际行为。
- **e2e 其余 P2/P3 未做**：`runner.py` 捕获 `OSError` 做逐任务错误隔离、`tests/` 根 README、
  `build_supply_chain_infrastructure.sh` 的魔法常量（`length == 10` / `version == "0.1.7"`）、
  greenfield SPEC 返回值契约、harness 并行/断点续跑。
- **未做的测试覆盖**：`evolution` 的 `deploy_and_observe` **成功路径**（需要跑通
  `create_new_version` 的 staged promote 事务链）与 `restore_previous_version` 的
  非 staged 回滚路径；`core_evolution` 的 per-record 互斥（P1-2）仍未实现。

## 中断恢复指引

1. 进度即本文件状态列；⬜ 从第一个未完成项继续。
2. 环境：`unset BASH_ENV`；PATH 用 `/bin:/usr/bin:/sbin:/usr/sbin:/usr/local/bin`
   （`/usr/bin/ls`、`/usr/bin/cat`、`/bin/grep` 均不存在；读文件用 Read 工具）。
3. `make test-rust` 会自动重建过期 `webui/dist`；若手工跑 `cargo test`，先确认
   `node webui/scripts/source-hashes.mjs --check` 退出码为 0。
4. 6A 的语义约束：`common_verify.py` 的「恢复 fixture 测试」与「`Ran N tests >= 1`」是两道**独立**守卫，
   缺一都会重新打开 P0（前者防删改，后者防「压根没有测试」）；`runner.py` 的解析是第三层，
   用于防御绕过 `common_verify` 的 verify 脚本。
