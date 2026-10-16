# Batch 4 修复清单（资源上限统一）

> 基线 HEAD `2b574e0`（Batch 0-3 已提交）· 开始 2026-10-16 · 来源：`review-2026-10-16/summary.md` §3-E + §7 Batch 4
> 状态标记：⬜ 待办 / 🔄 进行中 / ✅ 完成 / ⏸️ 阻塞 / ⏭️ 有意不做（附理由）

## 修复项

| # | 项 | 位置 | 状态 | 改动摘要 |
|---|---|---|---|---|
| 4.1 | OOXML「解压炸弹」（docx/pptx/xlsx） | `crates/tools/src/office.rs` | ✅ | 新增 `read_zip_entry`：单 entry 解压上限 64 MB；pptx 幻灯片数上限 500；xlsx 交给 calamine 前先查文件大小（128 MB） |
| 4.2 | CSV 全量入内存（`limit` 解析后才截断） | `crates/tools/src/data_process.rs` | ✅ | 读取前查文件大小（128 MB）；解析期行数上限 50 000——超出只计数不materialise，`total_rows` 仍准确，`limit` 不再需要事后 truncate |
| 4.3 | Whisper 上传无上限 | `crates/tools/src/audio_transcribe.rs` | ✅ | 读文件前 25 MB 预检（API 上限），避免为一次必败的请求把 GB 级文件读进内存 |
| 4.4 | 入站媒体下载无上限 | `crates/channels/src/{whatsapp,dingtalk}.rs` | ✅ | whatsapp：解码**前**按 base64 膨胀率（≈4/3）预检 25 MB；dingtalk：`file_resp.bytes()` → `security::read_response_limited` |
| 4.5 | `session_memory_systems` 池无上限 | `crates/agent/src/runtime.rs` | ✅ | 池上限 64，满时任意驱逐（条目可重建，无需 LRU 精确）；两条插入路径收敛为 `return_memory_system_to_pool` |
| 4.6 | napcat 每条 WS 消息 `spawn` 无背压 | `crates/channels/src/napcat/websocket/client.rs` | ⏭️ | **本批不做**：需要先决定并发上限与「丢弃 vs 排队」策略——直接影响消息可靠性，不能顺手加个信号量了事 |
| 4.7 | `stream_subscribe` 全局锁串行 + 无总量上限 | `crates/tools/src/stream_subscribe.rs` | ⏭️ | **本批不做**：需要设计背压/丢弃语义（丢哪条、如何告知调用方） |
| 4.8 | WebSocket 帧大小上限 | `crates/channels`（tokio-tungstenite 默认不限制） | ⏭️ | **本批不做**：需与 webui 侧最大 payload 期望一起定，误伤大帧会直接断功能 |

## 验证

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| V1 | `cargo test -p blockcell-tools -p blockcell-channels -p blockcell-agent` | ✅ | exit 0：tools **487** / channels **161** / agent **842** passed，0 failed |
| V2 | `cargo clippy`（相关 crate，无新增警告） | ✅ | exit 0；本次改动文件零新增警告（`data_process.rs:791` 的 MSRV 与 `runtime.rs:2564` 的 dead-code 均为既有告警） |

## 提交

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| G1 | 提交 A：tools（4.1 / 4.2 / 4.3） | ✅ | `73ea2e9`（3 文件） |
| G2 | 提交 B：channels（4.4） | ✅ | `9eb3617`（2 文件） |
| G3 | 提交 C：agent 池上限（4.5） | ✅ | `1dd95b0`（2 文件） |

## 中断恢复指引

1. 进度即本文件状态列；⬜ 从第一个未完成项继续。
2. 环境：`unset BASH_ENV`；PATH 用 `/bin:/usr/bin:/sbin:/usr/sbin:/usr/local/bin`（`/usr/bin/ls` 不存在）；cargo 用 rustup toolchain 绝对路径。
3. bin crate 可编译（webui 已重建）；若改 `webui/src` 需重跑 `npm run build`。
4. 4.2 的语义注意：`total_rows` 仍是文件真实行数，`returned_rows` 受 `min(limit, 50_000)` 约束——改上限时同步改这两处的注释。
