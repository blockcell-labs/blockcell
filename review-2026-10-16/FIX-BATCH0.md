# Batch 0 修复清单（安全阻断级）

> 基线 HEAD `5a19eae` · 开始 2026-10-16 · 来源：`review-2026-10-16/summary.md` §2 + §3-A
> 状态标记：⬜ 待办 / 🔄 进行中 / ✅ 完成 / ⏸️ 阻塞
> 修复原则：最小改动（CLAUDE.md §3 surgical changes），只改修复所必需的行，不动相邻代码

## 修复项

| # | 项 | 位置 | 状态 | 改动摘要 |
|---|---|---|---|---|
| 0.1 | WeCom webhook 验签 fail-closed（P0-1） | `crates/channels/src/wecom/webhook.rs` | ✅ | 有 token 时：`<Encrypt>` 缺失直接 403；验签失败 403；有 token 无 AES key 时拒绝明文回退（原逻辑 `&&` 短路可整体跳过验签） |
| 0.2 | 入站媒体文件名清洗 — whatsapp（P0-6） | `crates/channels/src/whatsapp.rs` | ✅ | stem / ext 经 `security::safe_path_component` |
| 0.3 | 入站媒体文件名清洗 — discord（P0-6） | `crates/channels/src/discord.rs` | ✅ | `attachment.id` / `filename` 清洗 |
| 0.4 | 入站媒体文件名清洗 — feishu（P0-6） | `crates/channels/src/feishu.rs` | ✅ | `file_key` 改按字符截断（原按字节切片可 panic）+ 三组件清洗 |
| 0.5 | 入站媒体文件名清洗 — lark（P0-6） | `crates/channels/src/lark.rs` | ✅ | 同上（`safe_key` 按字符截断） |
| 0.6 | 入站媒体文件名清洗 — dingtalk（P0-6） | `crates/channels/src/dingtalk.rs` | ✅ | `download_code` 按字符截断 + 三组件清洗 |
| 0.7 | wecom `media_id` 清洗 + 切片 panic（P1-A） | `crates/channels/src/wecom/media.rs` | ✅ | `media_id` 按字符截断 + 清洗 |
| 0.8 | app_control AppleScript 注入（P0-2） | `crates/tools/src/app_control.rs` | ✅ | `element_ref` 逐段经 `escape_applescript` |
| 0.9 | app_control Python 注入（P0-3） | `crates/tools/src/app_control.rs` | ✅ | 新增 `escape_python_string` 原语，替换裸 `replace('"')` |
| 0.10 | ocr_vision `language` Python 注入（P0-4） | `crates/tools/src/ocr.rs` | ✅ | `language` 与 `path` 均走 `escape_python_string` |
| 0.11 | network_monitor `ssl_check` 命令注入（P0-5） | `crates/tools/src/network_monitor.rs` | ✅ | 两处 `sh -c` 拼接改为 openssl 参数数组直调（新增 `openssl_x509_summary`，用管道替代 shell 管道） |
| 0.12 | email 附件文件名路径穿越（P1-A） | `crates/tools/src/email.rs` | ✅ | 附件名经 `safe_file_name` 归约为单一路径组件 |
| 0.13 | napcat `download_media_to_workspace` 穿越（P1-A） | `crates/tools/src/napcat/common.rs` | ✅ | `filename` + `chat_id` 双入口清洗 |
| 0.14 | **[V3 自查新增]** wecom 长连接媒体文件名 | `crates/channels/src/wecom/webhook.rs` | ✅ | `media_type` / `ext` 清洗（同类漏点） |
| 0.15 | **[V3 自查新增]** napcat 本地文件落盘 | `crates/channels/src/napcat/media/local_file.rs` | ✅ | `chat_id` 子目录 + `filename` 清洗（同类漏点） |
| 0.16 | **[V3 自查新增]** 外部 URL 字节切片 panic | `crates/channels/src/napcat/media/detector.rs`（3 处）、`base64_data.rs`（1 处） | ✅ | 改按字符截断，消除多字节 URL 触发的切片 panic |
| 0.17 | **[V3 自查新增]** 新增转义/归约原语 + 单测 | `crates/tools/src/lib.rs` | ✅ | `escape_python_string` / `safe_file_name` + 2 个单测（对应当前 review 的根因「缺统一转义原语」） |
| 0.18 | **[V5 新增]** SSRF 守卫改确定性判断 | `crates/channels/src/security.rs` | ✅ | `validate_public_http_url` 先按 IP 字面量（含 IPv6 去括号）判定，不再依赖宿主 DNS；修复沙箱下失败的 `ssrf_guard_rejects_local_addresses` |

**未纳入本批**（同族但风险较低，留给 Batch 2/4）：`crates/skills/src/service.rs:1418` 的 `&record.id[..8]` 字节切片、`tools/src/knowledge_graph.rs` 的 rusqlite 阻塞调用。

## 验证

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| V1 | `cargo check -p blockcell-channels -p blockcell-tools` | ✅ | exit 0，无警告 |
| V2 | `cargo clippy -p blockcell-channels -p blockcell-tools --all-targets` | ✅ | exit 0；仅 core/storage 既有警告，本次改动零新增 |
| V3 | 同类漏点回归自查（`sh -c` / `.join(filename)` / 字节切片 grep 全仓） | ✅ | 新增 0.14–0.17；`process.rs:130` 属测试代码、`message.rs:165` 取 `file_name()` 无穿越风险，均判定不需改 |
| V4 | `cargo test -p blockcell-channels -p blockcell-tools` | ✅ | exit 0：channels **161 passed**、tools **481 passed**（含新增 2 个转义/归约单测） |
| V5 | 修复 `security::tests::ssrf_guard_rejects_local_addresses` | ✅ | 原用例对 IPv6 字面量依赖宿主 DNS（本沙箱下失败）；改为确定性 IP 字面量判断后通过，见 0.18 |

## 提交

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| G1 | `git status` / `git diff --stat` 复核改动范围 | ✅ | 17 个文件，+232 / -64，全部落在 `crates/channels` 与 `crates/tools` |
| G2 | 提交（单个 commit） | ✅ | commit `12fdfff`（修复：Batch 0 安全阻断项） |

## 改动文件清单（16 个）

**channels（10）**：`wecom/webhook.rs`、`wecom/media.rs`、`whatsapp.rs`、`discord.rs`、`feishu.rs`、`lark.rs`、`dingtalk.rs`、`napcat/media/local_file.rs`、`napcat/media/detector.rs`、`napcat/media/base64_data.rs`

**tools（6）**：`lib.rs`（新增原语 + 单测）、`app_control.rs`、`ocr.rs`、`network_monitor.rs`、`email.rs`、`napcat/common.rs`

**注**：`crates/channels/src/security.rs` 为 V5 追加的第 17 个文件（SSRF 字面量判定），总计 **17 个文件**。

## 中断恢复指引

1. **进度即本文件的状态列**——恢复时从第一个非 ✅ 的项继续；🔄 表示改到一半，先看 `git diff` 确认落到哪一步。
2. **环境**（见 `.workbuddy-ai/memory/MEMORY.md`）：
   ```sh
   unset BASH_ENV
   export PATH="/usr/bin:/bin:/usr/sbin:/sbin:/Users/apple/.rustup/toolchains/stable-x86_64-apple-darwin/bin"
   CARGO=/Users/apple/.rustup/toolchains/stable-x86_64-apple-darwin/bin/cargo
   ```
3. **不要跑整仓 cargo**：`bin/blockcell/build.rs:101` 的 webui 嵌入哈希断言会让整仓构建失败（exit 101）。用 `-p blockcell-channels` / `-p blockcell-tools` 限定包即可绕开。
4. 修完必须回填本表状态 + 在 `review-2026-10-16/summary.md` 的对应 P0 行标注「已修（commit）」。
