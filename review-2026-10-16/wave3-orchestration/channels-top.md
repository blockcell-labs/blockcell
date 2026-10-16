# channels 顶层 Review 报告（Wave 3 · 编排层）
> 只读审查 · 2026-10-16 · 子代理 w3-channels-top

## 规模

| 文件 | 行数 | 职责 |
|---|---|---|
| dingtalk.rs | 1275 | 钉钉 Stream SDK（WS 长连接）+ REST 出站 |
| feishu.rs | 1209 | 飞书长连接（Protobuf 帧）+ REST 出站 |
| qq.rs | 1202 | QQ 官方 Bot（WS Gateway，Prod/Sandbox） |
| lark.rs | 1195 | Lark 国际版（Webhook 回调，多账号加密分发） |
| account.rs | 1054 | 账号 → ListenerConfig 展开、凭据匹配反查 |
| telegram.rs | 901 | Telegram Bot API 长轮询 |
| manager.rs | 820 | ChannelManager：出站分发、账号选择、状态 |
| slack.rs | 758 | Slack Socket Mode + 轮询兜底 |
| weixin.rs | 744 | 微信 iLink Bot API（扫码登录 + 轮询） |
| discord.rs | 727 | Discord Gateway（心跳/Identify/重连） |
| whatsapp.rs | 492 | WhatsApp Bridge WebSocket（持久 sink 复用） |
| wecom.rs | 290 | 企业微信门面（子模块在 wecom/，另一代理范围） |
| security.rs | 177 | 文件名清洗、SSRF 防护、限长读取 |
| rate_limit.rs | 176 | 每渠道进程级令牌桶限流器 |
| lib.rs | 39 | 模块声明 |
| **合计** | **11059** | 15 个顶层文件 |

## 模块地图

- **协议抽象（无 trait，靠约定）**：没有 `Channel` trait；统一契约是 `InboundMessage`/`OutboundMessage`（blockcell-core message.rs），每个适配器自行实现 `run_loop(shutdown)` + `send_message(config, chat_id, text)` 自由函数。session 映射由 `InboundMessage::session_key()` 统一完成（`account:{len}:{id}{chat}` 长度前缀防串号）。
- **入站桥接（inbound）**：11 个适配器各自把平台事件转成 `InboundMessage` 后 `inbound_tx.send().await`（背压直通，无缓冲丢消息）。allow_from 白名单在各适配器 `handle_message` 入口强制（空 = 全放行）。
- **出站（outbound）**：`ChannelManager::start_outbound_dispatcher` 单任务顺序消费 mpsc，按 channel 名分发到各适配器自由函数；账号选择走 `config_for_outbound`（克隆 channels 子配置 + 覆盖凭据）。feishu/qq/lark/dingtalk/wecom 有 token 缓存与失效重试；其余直发。
- **账号/监听器**：`account.rs` 把 `channels.<ch>.accounts` 展开为每账号一个 `ListenerConfig`（凭据反查 `*_account_id` 用于回填 `InboundMessage.account_id`），legacy 顶层配置作为兜底。manager 的 `pick_account` 处理出站账号选择。
- **横切**：security.rs（SSRF/限长/文件名）、rate_limit.rs（进程级单例，跨账号共享）。

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| P0 | whatsapp.rs:348-353；discord.rs:393；feishu.rs:516-534；lark.rs:471-478；dingtalk.rs:553-732 | **入站媒体文件名未清洗 → 路径穿越任意文件写**。5 个适配器把平台侧用户可控文件名/扩展名直接拼进 `media_dir.join(...)`。whatsapp 最严重：`stem = filename.rsplit('.').nth(1).unwrap_or(n)`（无点时用整串），`../../x` 原样进入路径；discord/feishu/lark/dingtalk 经扩展名段（`rsplit('.').next()` 对无点文件名返回整串）同样可注入 `/`。仅 telegram 用了 `security::unique_media_filename`（safe_filename），slack 做了 `replace(['/','\\',':'],"_")`。触发条件低（allow_from 默认空 = 任何人可发），写入内容为攻击者控制的媒体字节，可覆盖 `~/.blockcell/config.json5`（token 存储）等。 | 对照 security.rs:9-22 已有现成防护但仅 telegram/napcat 使用；建议全部改走 `safe_filename`/`unique_media_filename` |
| P1 | manager.rs:311-324 | **出站分发器单任务顺序执行，队头阻塞**：`while let Some(msg) = outbound_rx.recv().await` 内 `dispatch_outbound_msg(&msg).await` 串行。单个渠道慢/挂起（如 wecom 长连接媒体等 oneshot、telegram 120s 媒体超时）会阻塞所有渠道出站；且无重试、无幂等键，媒体失败仅 `error!` 记日志后继续（部分投递：媒体已发、文本失败无回滚）。 | dispatch 无 per-channel 并行、无超时包裹（reqwest 超时 30–120s 各自生效但叠加仍阻塞全局）；media 失败语义与 text 失败语义不一致 |
| P1 | whatsapp.rs:329（base64 全量解码）；dingtalk.rs:735-738（`file_resp.bytes()` 全量读） | **入站媒体下载无大小上限**：未使用 `security::read_response_limited`（25MB 上限）。tokio-tungstenite 默认不限制 WS 帧大小，巨大的 base64 附件/下载响应直接进内存 → OOM DoS（外部可触发）。 | feishu/slack/discord/lark/telegram 均用了 `read_response_limited`，仅此两处遗漏 |
| P2 | telegram.rs:597-652；slack.rs:504-537；dingtalk.rs:976-982；discord.rs:529+；feishu.rs:833-846（分块发送） | **分块出站部分失败无回滚无重试**：`for chunk` 中 `?` 提前返回，前面的 chunk 已送达 → 用户收到截断消息且无法区分。仅 feishu 有 token 失效单次重试。 | 所有 split_message 实现同构问题 |
| P2 | whatsapp.rs:116-118；feishu.rs:341-343；discord.rs:176-178 | **干净关闭（Ok）路径立即重连，无 sleep/退避**：若服务端 connect 后立刻 close（如鉴权拒绝），变成毫秒级热循环打爆对端。slack/discord/dingtalk 的 Err 路径有指数退避，但 Ok 路径同样裸重连（discord:476 backoff 重置也在 Ok 后）。 | 对照 dingtalk.rs:649-678 的完整退避写法 |
| P2 | dingtalk.rs:731；feishu.rs:532；lark.rs:662-665 | **对服务端返回串做裸字节切片，非 ASCII 会 panic**：`&download_code[..16]`、`&file_key[..8.min(..)]`、`&safe_key[..24]` 均未按 char_boundary 收敛。当前平台返回 ASCII 不触发，属潜伏崩溃（async 任务 panic 不会杀进程但会丢该消息且污染 span）。 | 建议统一 `security` 增加 `truncate_chars` 辅助 |
| P2 | whatsapp.rs:270；telegram.rs:203-211 | **会话映射/游标语义**：① WhatsApp 群聊按 `chat_id = sender` 建会话且回复发回 sender（群聊被降级成逐人 1:1，metadata.is_group 记录了但路由未用）；② Telegram offset 仅存内存，重启后 `getUpdates` 无 offset → 重投未确认更新 → agent 重复处理（at-least-once 无幂等）。 | 与"ID 映射错误会串消息"相关：whatsapp 群消息不串但错路；telegram 重启重复 |
| P2 | 全适配器（telegram.rs:141-159 等 11 处） | **allow_from 空 = 全放行（fail-open）**。docs/06:159 明确警告"一定要设置"，但代码默认开放且无启动期告警（仅 whatsapp/discord 等无 token 时才 warn）。配合 P0 路径穿越，未配置白名单时任意外部用户可触发文件写。 | 建议：空 allow_from 时启动 WARN 一次，或提供 `deny_by_default` 选项 |
| P2 | telegram.rs:314-343（顺序下载媒体阻塞轮询循环）；wecom.rs:61-68/wecom 子模块注册表（另一代理范围，仅记录依赖） | telegram `handle_message` 内 await 媒体下载 + ack 发送 + `inbound_tx.send().await`，阻塞长轮询循环；背压时整个 getUpdates 停摆（消息在服务端堆积，尚不丢）。性能类，量级可控。 | 可改为 spawn 处理或限定并发 |
| P3 | rate_limit.rs:98-142 | 进程级限流器按 channel 单例，**多账号共享同一桶**：同渠道 2 个 bot 互相挤占配额（如 telegram 30/s 总量而非每账号）。 | 可按 (channel, account) 分桶 |
| P3 | manager.rs:64 | `pick_account` 在 accounts 非空但无 `default_account_id` 且无 "default" 键时静默 `Ok(None)` → 回落顶层（可能为空）配置，报错延迟到发送时（"token not set"）。 | 建议显式错误或启动期校验 |
| P3 | account.rs:292-305 vs manager.rs:497-500 vs account.rs:302-304 | wecom 长连接模式判定（`long_connection`/`long-connection`/`stream`）魔改字符串重复 4+ 处，且 `channel_configured`（account.rs:497-502）不认 bot_id-only 长连接账号 → 状态展示与 listener 实际可用性不一致。 | 抽取 `is_long_conn_mode()` 助手 |
| P3 | whatsapp.rs:379-414 | 手搓 base64 解码器且**无测试**；`base64` crate 已是依赖（wecom.rs:3 使用）。尾部 1–3 字符静默丢弃，属潜伏数据损坏路径。 | 直接换 crate 解码 |
| P3 | lark.rs:284-309 | `resolve_lark_webhook_account_id` 循环内 `serde_json::from_str(&plaintext).ok()?`：解密成功但内层解析失败时**直接返回 None**，不再尝试其余 listener 的 encrypt_key（多账号场景下误判）。 | `?` 应为 `continue` |
| P3 | telegram.rs:433-462 | 入站语音触发本地 `whisper` CLI（PATH 查找、固定 /tmp 输出）。无 shell 注入面（参数为文件路径），但对外部媒体执行本地进程 + /tmp 竞争写入，建议收敛输出目录到 media_dir。 | 低危观察项 |
| P3 | telegram.rs:111；feishu.rs:214；wecom.rs:90 | 构造函数 `Client::builder().build().expect(...)`：非测试代码仅此 3 处 expect，均属罕见失败路径，但可统一改 `map_err`/`unwrap_or_else`。 | 全仓非测试 unwrap/expect 密度整体极低（本次 awk 全量扫描仅此 3 处 + whatsapp/send 等处的 `unwrap_or_else` 兜底），纪律良好 |
| P3 | whatsapp.rs:140-186 | Bridge WS 无客户端保活/读超时：TCP 静默死亡时 `read.next().await` 永久挂起，直至下次发送失败才自愈（发送失败会清 sink 并回落短连接）。 | 可加 ping 定时器或 read 超时 |
| P3 | 各适配器（feishu.rs:564-577、whatsapp.rs:219-232、dingtalk DedupCache、qq/lark SEEN_EVENTS） | 去重集合淘汰策略为"超限后 `iter().take(100)` 任意删"，可能误删新键 → 短窗重复处理；增长有界（900–2048）尚可。 | 改环形缓冲或 LRU |
| P3 | weixin.rs:132-153, 279 | `rand_u32()` 用 `RandomState` 哈希时间戳（非 CSPRNG），uin/client_id 非机密标识，风险低；但 `X-WECHAT-UIN` 每请求随机生成，若该头应为稳定 bot 标识则语义存疑（docs/23 未约定，无法静态判定）。 | 建议与微信 API 文档核对 |
| P3 | weixin.rs | 唯一没有限流器的渠道（其余 9 个均有）；多 chunk 回复每 chunk 都携带同一 `context_token`（ilink 语义是否允许多次回复同一 token 待核）；图片类消息 item 静默丢弃（media 恒为空）。 | 功能完整性观察 |
| P3 | telegram.rs:543-595 | `escape_markdown_v2` 状态机：三反引号检测条件 `i + 2 < n` 在文本恰以 ``` 结尾时 `i+2 == n-1` 成立（OK），但"未闭合代码块吞掉后续转义"行为对 LLM 输出鲁棒性依赖调用方；测试已覆盖常见用例。 | 低优先 |

## 亮点

- **security.rs 设计扎实**：SSRF 防护（`validate_public_http_url` 含 IPv4-mapped/回环/链路本地/云元数据 100.100.100.200 特判）、流式限长读取（content_length 预检 + 累加上限双保险）、文件名清洗，且全部有单测（napcat/telegram 已接入）。
- **session_key 长度前缀防串号**（core/message.rs:21-32）：`account:{len}:{id}{chat}` 从根上消除 `("a","bc")` vs `("ab","c")` 碰撞，多账号会话隔离正确。
- **account.rs 的 listener 展开 + legacy 兜底 + 11 个单测**：账号就绪判定、按 id 排序保证确定性、scoped config 内只留单账号（防止下游误用其他账号凭据），测试覆盖展开/兜底/不完整账号跳过等边界。
- **ACK-before-process 模式**（feishu.rs:423-442、slack.rs:161-167、dingtalk.rs:339-356）：先回 ACK 再处理，避免平台重投风暴；配合 event_id/msg_id 去重。
- **重连质量分层**：dingtalk（指数退避 2s→60s）、slack（2s→30s）、discord（致命关闭码 4004/4013/4014 精确停连并给出 Portal 配置提示）均为正例；feishu token 失效自动失效缓存 + 单次重试（feishu.rs:833-846）。
- **非测试代码 unwrap/expect 近乎为零**（awk 全量扫描：仅 3 处构造期 expect），send 路径普遍 `unwrap_or_else(|_| Client::new())` 兜底；测试总量约 35+ 个，覆盖 split/markdown 转义/UTF-8 边界/账号展开/限流桶等关键逻辑。

## 建议

1. **立即修复 P0**：5 个适配器入站媒体落盘统一走 `security::safe_filename`/`unique_media_filename`（whatsapp 优先，其 stem 逻辑整串透传）；补一个"文件名穿越"回归测试（可只测 helper 组合）。
2. **manager 出站改造**：dispatch 按 channel 并行（`tokio::spawn` 或 per-channel worker），为每次发送加整体超时；引入 `OutboundMessage` 幂等键或至少分块失败时记 WARN 带 chunk 序号；媒体失败与文本失败语义对齐（要么都容忍要么都报错）。
3. **媒体下载补齐 25MB 上限**：whatsapp base64 解码前按 `len*3/4` 预检，dingtalk 改 `read_response_limited`。
4. **fail-closed 选项**：allow_from 为空时启动期 WARN（docs 已警告但代码无提示），评估增加 `allowFrom: ["*"]` 显式通配。
5. **收敛重复实现**：`split_message`、`CachedToken`、去重集合、长连接模式判定、截断助手各抽到 channels 公共模块（当前 5×/5×/5× 复制）。
6. **游标语义文档化**：telegram 内存 offset（重启重投）与 weixin buf 先发后处理（失败即丢）的 at-least-once/at-most-once 差异应在 docs/06 或代码注释中明示，便于 runtime 侧做幂等。

## 各维度结论

- ① 正确性：见 P0/P1/P2（路径穿越、队头阻塞、分块部分投递、游标语义）；session_key 与账号反查映射经审查**无串号缺陷**。
- ② 并发：任务生命周期（heartbeat abort、sink 清理、shutdown select）整体规范；Ok 路径热重连（P2）、whatsapp 无保活（P3）为缺口；WS ping/pong/心跳各适配器齐备。
- ③ 安全：P0 路径穿越 + fail-open 白名单为主要问题；凭据管理（Config 承载、无日志泄漏、Authorization 头）良好；无出站未授权发送面（agent 发起，allow_from 不适用出站——符合设计，但 channels 层无审计钩子，与 Tool 确认机制的对应关系在 runtime 层）。
- ④ 性能：顺序分发（P1）、顺序媒体下载（P2）、进程级限流跨账号耦合（P3）；广播扇出不在此层。
- ⑤ 测试覆盖：核心文件测试扎实；适配器入站路径无集成测试（合理，缺 mock 框架）；whatsapp base64 手搓解码零测试（P3）。
- ⑥ 可维护性：大量跨适配器复制（P3 建议 5）；wecom/weixin 魔改字符串重复（P3）。
- ⑦ 契约一致性：docs/06 的 11 渠道清单、allowFrom 语义、登录流程（QR→token 自动保存）与代码一致；docs/23 与 weixin.rs 配置字段一致；未发现文档与实现的矛盾（X-WECHAT-UIN 每请求随机属文档未覆盖区域，标 P3 待核）。
