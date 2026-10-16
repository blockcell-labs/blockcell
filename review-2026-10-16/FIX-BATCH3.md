# Batch 3 修复清单（路径与凭据收敛）

> 基线 HEAD `964421a`（Batch 0/1/2 已提交）· 开始 2026-10-16 · 来源：`review-2026-10-16/summary.md` §7 Batch 3
> 状态标记：⬜ 待办 / 🔄 进行中 / ✅ 完成 / ⏸️ 阻塞 / ⏭️ 有意不做（附理由）

## 修复项

| # | 项 | 位置 | 状态 | 改动摘要 |
|---|---|---|---|---|
| 3.1 | `video_process` 写路径策略完全失效 | `crates/tools/src/video_process.rs` | ✅ | 参数名 `output` → `output_path`，与工具 description、`docs/22`、runtime `path_security::extract_path_accesses` 对齐，恢复 deny/confirm 与默认写确认 |
| 3.2 | `network_monitor` 绕过 SSRF 防线 | `crates/tools/src/network_monitor.rs` | ✅ | `http_check` / `bandwidth` 改用 `ssrf::safe_client_builder()` + `ensure_url_allowed()` + `redirect_policy(true)`，与 `http_request` 同一防线 |
| 3.3a | `GET /v1/channels` 明文回显渠道密钥 | `bin/.../gateway/channels.rs` | ✅ | 按 payload 自带 `secret: true` 掩码（只掩非空值）；`apply_channel_fields` 遇掩码视为「保持原值」 |
| 3.3b | `GET /v1/config` 明文回显 provider 密钥 | `bin/.../gateway/config_api.rs` | ✅ | 返回前按 key 名掩码；`PUT /v1/config` 收到掩码时先从磁盘 `restore_masked_secrets` 还原，避免把密钥写成 `********` |
| 3.3c | CLI `config show` / `config get` 打印明文密钥 | `bin/.../config_cmd.rs` | ✅ | 同一套掩码；`config get <密钥名>` 只输出掩码 |
| 3.4 | `.env` 权限 0644 + 空模板覆盖真实环境变量 | `bin/.../commands/env_file.rs` | ✅ | 创建即 0600（已存在文件也收紧）；模板空值不再覆盖 shell 已导出的非空变量 |
| 3.5 | `GET /v1/config/raw` 明文回显 | `bin/.../gateway/config_api.rs` | ⏭️ | **本批不改**：该端点是前端「原始配置编辑」的数据源，掩码会使其失去作用；它属于「网关鉴权模型」问题（`api_token` 可选、默认无鉴权），需与 gateway 鉴权一并设计，已在 summary 记录 |

## 验证

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| V1 | `cargo test -p blockcell-tools`（3.1 / 3.2） | ✅ | 487 passed / 0 failed |
| V2 | `cargo test -p blockcell-core`（掩码 helper） | ✅ | 129 passed / 0 failed（含新增 2 个掩码/还原用例） |
| V3 | `cargo test -p blockcell`（3.3 / 3.4，bin） | ✅ | 167 passed / 0 failed |
| V4 | `cargo clippy`（相关 crate，无新增警告） | ✅ | exit 0；本次改动文件零警告 |

## 提交

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| G1 | 提交 A：tools（3.1 / 3.2） | ✅ | `30c9b90`（2 文件） |
| G2 | 提交 B：core 掩码原语 + gateway/CLI 脱敏（3.3） | ✅ | `f805820`（4 文件） |
| G3 | 提交 C：env 权限与覆盖语义（3.4） | ✅ | `6e52264`（1 文件） |

## 未纳入本批的同族问题（留待 Batch 4/5）

- `tools/sandbox.rs`：Linux Landlock 缺读限制（read-only 名不符实）
- `tools/tool_guard.rs`：确认门漏检 `rm --recursive` 长选项
- `network_monitor` 的 `port_scan` / `ping` / `dig` 等原始探测仍可直达内网——它们是显式网络诊断功能，与 HTTP 取数不同，需单独决定策略（是否受 `BLOCKCELL_ALLOW_PRIVATE_NETWORK` 约束）

## 中断恢复指引

1. 进度即本文件状态列；⬜ 从第一个未完成项继续。
2. 环境：`unset BASH_ENV`；PATH 用 `/bin:/usr/bin:/sbin:/usr/sbin:/usr/local/bin`（`/usr/bin/ls` 不存在，用 `/bin/ls`）；cargo 用 rustup toolchain 绝对路径。
3. bin crate 现在可编译（webui 已重建，见 Batch 2）；若再改 `webui/src`，需重跑 `npm run build`。
4. 3.3 的兼容性要点：掩码常量同时用于「读时脱敏」与「写时忽略」，改常量时必须同步改 `apply_channel_fields` 的判断，否则 UI 保存会把密钥写成掩码。
