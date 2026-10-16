# W0 静态基线报告

> 只读审查 · 2026-10-16 · 基线 HEAD `5a19eae`（2026-10-16）
> 目的：给后续各波审查加权与定位，不作为缺陷清单

## 1. 结论摘要

| 项 | 结果 |
|---|---|
| 工作区 HEAD | `5a19eae` — 修复：对齐 WebUI 确认弹窗工具名、工具失败状态展示与斜杠命令过滤 |
| 近 6 个月提交数 | 376 |
| Rust 规模 | ≈259K LOC（crates 229K + bin 30K），440+ 文件 |
| 其他资产 | webui 57 TS ≈15K LOC；docs 主文档 29 篇（docs0 37 / docs1 14）；tests 67 py；skills 运行时资产 22 个文件 |
| `cargo clippy --workspace` | 库 crate 通过，**仅 3 个警告**（全部在 crates/core）；**bin crate 构建失败**（见 §2） |
| 危险模式 | unwrap 2,040 处 / expect 1,144 处 / unsafe 49 处 / panic 类 71 处 / TODO 类 16 处（含测试代码，仅作加权） |
| churn 重心 | crates/agent 独占 165K 行变更（全仓最高，是第二名的 6.3 倍） |

## 2. 编译 / lint 基线（`wave0-baseline/clippy.txt`）

**库 crate：零 error，3 个 warning**

| 位置 | 警告 |
|---|---|
| `crates/core/src/budget.rs:299` | MSRV 声明 1.85，但所用 API 稳定于 1.87 |
| `crates/core/src/file_store.rs:25` | `File::create` 未显式声明 truncate 行为 |
| `crates/core/src/file_store.rs:77` | MSRV 声明 1.85，但所用 API 稳定于 1.89 |

**bin crate：构建硬失败（exit 101）**

```
bin/blockcell/build.rs:101: assertion `left == right` failed:
embedded WebUI is stale because src/App.tsx changed; run `npm run build` in webui
  left:  560a4329...  (webui 源码哈希)
  right: 327438ab...  (嵌入构建产物哈希)
```

- **含义**：`webui/src` 变更后未跑 `npm run build`，则 `cargo build` / `clippy` / `test` 整仓失败。
- **影响**：CI 与本地开发硬阻塞；本次 W0 的 clippy 因此只覆盖到库 crate，bin 的 clippy 结论缺失（W4 由人工代理覆盖）。
- **评级**：P2 构建流水线问题（守卫本身有价值，但应分级降级 / feature 门控，详见 `wave4-entry/cli-core-tui.md` 的建议）。
- **附带发现**：workspace `rust-version = "1.85"` 与实际使用的 API 不一致（1.87/1.89 才稳定），与上表两条 MSRV 警告同源。

## 3. 危险模式密度（按行计，含测试代码）

| 模块 | unwrap | expect | unsafe | panic 类 | TODO 类 | LOC |
|---|---|---|---|---|---|---|
| crates/agent | 636 | 483 | 1 | 14 | 3 | 70K |
| crates/tools | 545 | 201 | 25 | 11 | 5 | 49K |
| crates/storage | 299 | 108 | 2 | 3 | 0 | 12K |
| crates/skills | 207 | 147 | 8 | 1 | 0 | 17K |
| crates/scheduler | 107 | 39 | 2 | 0 | 0 | 6.5K |
| crates/core | 51 | 82 | 5 | 0 | 2 | 11K |
| crates/channels | 53 | 10 | 1 | 0 | 0 | 24K |
| crates/updater | 58 | 2 | 3 | 1 | 0 | 2K |
| crates/providers | 37 | 7 | 0 | 0 | 0 | 8K |
| bin | 47 | 65 | 3 | 41 | 6 | 30K |

要点：
- `unsafe` 49 处，集中在 tools(25) + skills(8) + core(5)，W1/W2 已要求逐个核对。
- bin 的 panic 类 41 处偏高（CLI 断言/expect 输出路径），W4 已覆盖。
- 该表**不是缺陷计数**：unwrap 在测试与构造期属正常；只用于给各波分配注意力。

## 4. git churn 热点（近 6 个月，+/- 行数）

| 模块 | churn | 模块 | churn |
|---|---|---|---|
| crates/agent | 165,062 | docs | 10,157 |
| crates/tools | 26,239 | webui | 8,358 |
| bin/blockcell | 14,961 | crates/channels | 8,223 |
| crates/skills | 13,813 | crates/providers | 4,418 |
| crates/storage | 13,668 | crates/updater | 1,482 |
| crates/core | 12,465 | tests | 1,130 |
| crates/scheduler | 11,390 | | |

**Top churn 文件**

- agent：`runtime.rs` 84,699（单文件占 agent 一半）· `runtime/tests.rs` 7,753 · `forked/agent.rs` 6,102 · `runtime/process_message_inner.rs` 3,495 · `context.rs` 3,014 · `auto_memory/extractor.rs` 2,977 · `skill_file_store.rs` 2,533 · `task_manager.rs` 2,407 · `auto_memory/cursor.rs` 2,204 · `ghost_background_review.rs` 2,183
- tools：`shell_session.rs` 1,899 · `skill_manage.rs` 1,888 · `browser/tool.rs` 1,561 · `termux_api.rs` 1,448 · `sandbox.rs` 1,414 · `browser/cdp_backend.rs` 1,184 · `security_scan.rs` 1,024
- bin：`commands/agent.rs` 2,690 · `gateway.rs` 1,804 · `gateway/capabilities.rs` 1,335 · `agent/tui.rs` 1,169

解读：
- agent 是绝对重心，且 churn 高度集中——`runtime.rs`（实际 2,703 行）反复改动说明这里是设计演化热点，值得优先审（W3 已拆 5 份覆盖）。
- tools 的 churn 前三全是安全敏感文件（shell 会话、技能安装、浏览器），与 W2 的安全发现吻合。
- bin 的 churn 集中在 TUI 交互与 gateway API 面。

## 5. 规模地图（分解依据）

| 区域 | 文件 | LOC | 拆分方式 |
|---|---|---|---|
| crates/agent | 124 | 70,076 | 顶层 runtime.rs 2.7K + runtime/ 19K + forked/ 6.5K + 记忆家族 ~8K + 其余顶层 ~16K + 子目录 |
| crates/tools | 86 | 49,035 | 顶层 38.5K + browser 3.6K + napcat 4.0K + mcp 1.0K + termux 1.4K + skill_manage 0.3K |
| bin/blockcell | 70 | 29,728 | main.rs 1.6K + agent/TUI 4.0K + slash_commands ~3K + gateway 9.6K + 运维子命令 ~12K |
| crates/channels | 41 | 23,563 | 顶层 11.1K + napcat 9.6K + wecom 2.9K |
| crates/skills | 29 | 17,480 | evolution 3.1K + core_evolution 0.2K + manager 0.3K + service 0.3K + 顶层其余 ~13K |
| crates/storage | 27 | 12,013 | 单层 |
| crates/core | 38 | 11,160 | 单层 |
| crates/providers | 13 | 8,003 | 单层 |
| crates/scheduler | 17 | 6,549 | 单层 |
| crates/updater | 5 | 2,097 | 单层 |
| webui/src | 57 TS | 14,961 | 单层 |
| tests | 76 | — | python e2e + harness |

## 6. 文档地图

- `docs/00_index.md` … `docs/29_hook_system.md`：29 篇主文档（工具/技能/记忆/通道/浏览器/gateway/自进化/子代理/架构/CLI/provider/MCP/路由/路径策略/微信/hook）
- `docs0/` 37 篇、`docs1/` 14 篇：历史/草稿区（W5 抽查未发现主文档级漂移）
- 归属建议：03/04/24-26 → W2；05/13 → W3；08/19 → W4；22/29 → W1/W4；09/27 → W2（自进化）

## 7. 后续加权建议

1. **优先级最高**：crates/agent（churn 16.5 万，单文件 runtime.rs 8.5 万）与 crates/tools 的安全敏感文件（shell/sandbox/skill_manage/browser）。
2. **安全面优先深读**：一切把外部输入拼进 shell/SQL/脚本的位置——W2/W3/W4 已各自发现注入类 P0，印证该判断。
3. **构建流水线需单独处置**：webui 嵌入哈希守卫 + MSRV 声明不一致，会让「改前端后跑 Rust 测试」这类日常操作直接失败。
4. 本基线数据仅用于加权；实际缺陷结论以各波报告为准。

## 8. 复现命令（本机环境注意事项）

```sh
unset BASH_ENV                                   # 宿主 shim 会污染子 shell
export PATH="/usr/bin:/bin:/usr/sbin:/sbin:/Users/apple/.rustup/toolchains/stable-x86_64-apple-darwin/bin"
CARGO=/Users/apple/.rustup/toolchains/stable-x86_64-apple-darwin/bin/cargo
$CARGO clippy --workspace --message-format short   # 需先在 webui 跑 npm run build
git log --since="6 months ago" --numstat --no-merges   # churn 统计
```
