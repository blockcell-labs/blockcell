# Batch 1 修复清单（准入与权限门）

> 基线 HEAD `e1d0864`（Batch 0 已提交）· 开始 2026-10-16 · 来源：`review-2026-10-16/summary.md` §3-B + §7 Batch 1
> 状态标记：⬜ 待办 / 🔄 进行中 / ✅ 完成 / ⏸️ 阻塞 / ⏭️ 有意不做（附理由）

## 目标

把「机制正确、接线不全」的准入体系补齐：让 `Tool::requires_confirmation` 在真正需要用户点头的工具上生效，让 fork 路径的默认权限从 fail-open 变 fail-closed，并修掉策略引擎的排序陷阱。

## 修复项

| # | 项 | 位置 | 状态 | 改动摘要 |
|---|---|---|---|---|
| 1.1 | `knowledge_forget` 准入接线 | `crates/tools/src/knowledge_forget.rs` | ✅ | `action == "confirm"`（真正删除的那一步）需用户确认，堵住「LLM 用自己拿到的 preview token 自我批准」 |
| 1.2 | `termux_api` 敏感动作准入 | `crates/tools/src/termux_api.rs` | ✅ | 覆写 `requires_confirmation`：sms_send / telephony_call / clipboard_set / download / infrared_transmit / keystore / wifi_enable |
| 1.3 | napcat 群管理「需确认」当放行 | `crates/tools/src/napcat/group.rs` | ✅ | `PermissionResult::NeedsConfirmation` 由 `Ok(())` 改为拒绝（fail-closed），不再静默放行 |
| 1.4 | `ToolPolicy` deny 优先 | `crates/core/src/tool_policy.rs` | ✅ | `evaluate` 由 first-match-wins 改为 **Deny > Ask > Allow** 优先级（同级别内仍按声明顺序），宽 allow 无法再遮蔽精确 deny |
| 1.5 | fork 路径 `can_use_tool` 默认 Allow → Deny | `crates/agent/src/forked/agent.rs`（`new()` / `Builder::build()`） | ✅ | 默认策略改为**由该 fork 自己的 tool schema 名单派生 allowlist**（fail-closed）；`new()` 默认空名单=全拒 |
| 1.6 | **[1.5 配套]** allowlist 策略构造 + schema 名单提取 | `crates/agent/src/forked/can_use_tool.rs` | ✅ | 新增 `create_allowlist_can_use_tool` / `tool_names_from_schemas`（兼容 `function.name` 与 `name` 两种形状） |
| 1.7 | **[1.5 配套]** subagent 两条路径接线 | `crates/agent/src/runtime/subagent_provider.rs` | ⏭️ | **无需改动**：`spawn_fork` / `spawn_typed` 本来就传 `tool_schemas`，默认策略自动派生出与 schema 一致的可执行集，行为不变而执行面收窄（原先可执行任何 executor 支持的工具，即使从未在 schema 里声明） |
| 1.8 | `Tool::requires_confirmation` trait 默认值 | `crates/tools/src/lib.rs:641` | ⏭️ | **不改**：trait 默认翻成 `true` 会让全部工具逐次弹确认（含纯读工具），破坏面远大于收益。正确做法是在有副作用的工具上显式实现（即 1.1–1.2），并把默认值语义写进文档 |
| 1.9 | app_control / email / message 的准入声明 | `crates/tools/src/{app_control,email,message}.rs` | ⏭️ | **本批不做**：review 中列为 P2/建议级；开启后会改变所有交互式/网关会话的行为（网关 `/v1/chat` 的确认链路本身还有缺陷），需与 UI 确认流一起评估。已在 summary.md 保留 |

## 验证

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| V1 | `cargo check -p blockcell-tools -p blockcell-core` | ✅ | exit 0 |
| V2 | `cargo test -p blockcell-tools -p blockcell-core` | ✅ | exit 0：tools 481 passed；core 各套件全绿（含新增 deny/ask 优先级回归用例） |
| V3 | `cargo clippy -p blockcell-tools -p blockcell-core -p blockcell-agent --all-targets` | ✅ | exit 0；新增代码零警告（仅 core/storage/agent 既有告警） |
| V4 | `cargo test -p blockcell-agent`（fork/subagent 回归） | ✅ | exit 0：**842 passed / 0 failed**（lib 782 + 集成 43 + 其余 17），含新增 3 个 allowlist 用例 |
| V5 | 同类漏点自查：还有哪些「有副作用但未声明准入」的工具 | ✅ | 工具侧声明了 `execution_environment` 的共 15 个工具实现中，仅 `shell_session` 原本有准入；本批补上 knowledge_forget / termux_api。**剩余候选 3 个**：`email`（SMTP 外发）、`message`（向任意渠道/联系人外发）、`app_control`（UI 自动化 + 脚本执行，且未声明 environment）→ 见 1.9 的有意延后 |

### 1.5 引发的测试调整（需人工确认）

`crates/agent/src/forked/agent/tests.rs::forked_agent_cancellation_kills_running_shell_tool` 原先依赖 Allow-all 默认值才能跑到 shell 工具；默认值改 fail-closed 后，该测试显式声明了 **test-only allow-all** 策略（其意图是验证取消链路，不是验证权限），并附注释说明。这是本批唯一被改动的既有测试。

## 提交

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| G1 | 提交 A：准入接线 + ToolPolicy（1.1–1.4） | ✅ | commit `2f1766c`（5 文件，+108/−23） |
| G2 | 提交 B：fork 默认 fail-closed + allowlist（1.5–1.7） | ✅ | commit `289330a`（4 文件，+105/−7） |

## 中断恢复指引

1. **进度即本文件状态列**；⬜ 项从 1.5 起继续，先看 `git diff` 确认落到哪一步。
2. 环境：`unset BASH_ENV`；PATH 含 `/usr/bin:/bin:/usr/sbin:/sbin` + rustup toolchain bin；cargo 用绝对路径。
3. 不要跑整仓 cargo（`bin/blockcell/build.rs` 的 webui 哈希断言会失败）；按 `-p <crate>` 限定。
4. 1.5 的**风险点**：subagent 路径原先依赖 Allow-all 默认值，翻默认值时必须同时给 `spawn_fork`/`spawn_typed` 传显式 allowlist，否则子代理所有工具调用会被拒。改完必须跑 `cargo test -p blockcell-agent`。
