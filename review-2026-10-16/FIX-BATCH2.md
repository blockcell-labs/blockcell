# Batch 2 修复清单（数据与配置类）

> 基线 HEAD `3a2e371`（Batch 0/1 已提交）· 开始 2026-10-16 · 来源：`review-2026-10-16/summary.md` §7 Batch 2
> 状态标记：⬜ 待办 / 🔄 进行中 / ✅ 完成 / ⏸️ 阻塞（附原因）

## 修复项

| # | 项 | 位置 | 状态 | 改动摘要 |
|---|---|---|---|---|
| 2.1 | P0-7 Gemini 默认配置开箱 404 | `crates/providers/src/factory.rs` | ✅ | `gemini` 名只在 `api_type == "gemini"` 时走原生实现，否则按 `api_type` 路由；同时补 `default_api_base("gemini")` = OpenAI 兼容端点 |
| 2.2 | P0-9 `config set` 静默丢未建模键/注释 | `bin/blockcell/src/commands/config_cmd.rs` | ✅ | 改为直接编辑原始 JSON5 文件（`parse_json5_value_preserving_env` + `write_json5_pretty`）：未建模键保留、其他键的 `${ENV}` 占位符不再被展开回写；写入前用 `Config` 反序列化校验，避免写坏配置 |
| 2.3 | encrypt 三重缺陷 | `crates/tools/src/encrypt.rs` | ✅ | ① key 模式 IV 落盘（头部 `BLOCKCELL-IV-V1\n` + 32 hex IV），解密可逆；② `algorithm` 只接受 `aes-256-cbc`，AEAD 取值明确拒绝；③ 密码与数据均走 stdin（`-pass stdin`），不再进 argv |
| 2.4 | alerts `rules.json` 读-改-写无锁 | `bin/blockcell/src/commands/gateway/alerts.rs` | ✅ | create / update / delete 三个 handler 全部加 `state.config_write_lock` |
| 2.5 | audit 哈希链缺跨进程互斥 | `crates/storage/src/audit.rs` | ✅ | `write_event` 在进程内 Mutex 之外补 `file_lock::lock_exclusive`（fs2 sidecar 锁），覆盖 `recover_prev_hash` → 追加的整段 |
| 2.6 | `merge_episode_metadata` 丢失更新 | `crates/storage/src/ghost_ledger.rs` | ✅ | 读-改-写合并进 `Immediate` 事务，SELECT 与 UPDATE 同事务，不再跨两次加锁 |

## 验证

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| V0 | `npm run build`（webui）——bin crate 编译前置 | ✅ | exit 0（vite 构建 5m31s，重写 `dist/` 与 `dist/source-hashes.txt`） |
| V1 | `cargo test -p blockcell-providers` | ✅ | 98 passed / 0 failed |
| V2 | `cargo test -p blockcell-tools`（encrypt） | ✅ | 484 passed / 0 failed（含新增 3 个：key 模式往返、密码模式往返、AEAD 拒绝） |
| V3 | `cargo test -p blockcell-storage`（audit / ghost_ledger） | ✅ | 84 passed / 0 failed |
| V4 | `cargo test -p blockcell`（bin） | ✅ | 167 passed / 0 failed（webui 重建后 bin crate 恢复可编译） |
| V5 | `cargo clippy`（各 crate，无新增警告） | ✅ | exit 0；本次改动文件零警告（其余为 `tools_cmd.rs` / `memory.rs` 既有告警） |

## 提交

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| G0 | 提交 0：重建 webui 嵌入产物（`webui/dist/**`） | ✅ | `59601f3`（74 个 dist 文件）——使 `build.rs` 哈希断言恢复通过，是 bin crate 可编译的前提 |
| G1 | 提交 A：providers + storage（2.1 / 2.5 / 2.6） | ✅ | `4cfb64b`（3 文件） |
| G2 | 提交 B：tools encrypt（2.3） | ✅ | `a67e5e7`（1 文件，含 3 个新测试） |
| G3 | 提交 C：bin（2.2 / 2.4） | ✅ | `6c93115`（2 文件） |

## 中断恢复指引

1. 进度即本文件状态列；⬜ 从第一个未完成项继续。
2. 环境：`unset BASH_ENV`；PATH 用 `/bin:/usr/bin:/sbin:/usr/sbin:/usr/local/bin`（**`/usr/bin/ls` 不存在，用 `/bin/ls`**）；cargo 用 `~/.rustup/toolchains/stable-x86_64-apple-darwin/bin/cargo`。
3. **bin crate 编译前置**：先在 `webui/` 跑 `npm run build`（node 26.4.0 / npm 11.17.0 在 `/usr/local/bin`），否则 `build.rs` 因嵌入 webui 哈希过期直接 panic。
4. 2.3 的兼容性说明：旧版本 key 模式产出的密文**本来就无法解密**（IV 丢失），新格式用文件头 `BLOCKCELL-IV-V1\n` + 32 位 hex IV 标识；解密遇到无头部的文件会给出明确错误而非静默产出垃圾。
