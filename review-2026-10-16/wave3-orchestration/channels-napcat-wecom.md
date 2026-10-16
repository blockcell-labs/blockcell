# napcat/wecom Review 报告（Wave 3 · 编排层）

> 只读审查 · 2026-10-16 · 子代理 w3-channels-im
> 审查范围：`crates/channels/src/napcat/`（19 文件）+ `crates/channels/src/wecom/`（7 文件 + `wecom.rs` 模块根）。未运行 cargo/clippy/test；未修改任何仓库文件。

## 规模

| 模块 | 文件数 | LOC | 说明 |
|---|---|---|---|
| napcat/websocket | 4 | 2,977 | client 1,268 / server 1,382 / sender 311 / mod 16 |
| napcat/media | 8 | 2,864 | mod 583 / types 493 / detector 344 / downloader 333 / direct_http 317 / base64 282 / napcat_stream 276 / local_file 236 |
| napcat 顶层 | 6 | 3,078 | types 1,182 / event 859 / message 724 / channel 133 / outbound 110 / mod 70 |
| **napcat 合计** | **18(+1 目录)** | **8,919** | 含内联单测约 1,900 行 |
| wecom | 8 | 3,164 | media 983 / webhook 715 / longconn 506 / tests 258 / wecom.rs 290 / send 186 / dispatch 130 / polling 96 |
| **合计** | 26 | **12,083** | 不含内联测试约 9,900 行 |

## 模块地图

**napcat（OneBot v11 / NapCatQQ）**
- `types.rs` — `ApiRequest`/`ApiResponse`/retcode 常量/流式分片 `StreamChunkData`、事件枚举。纯 API 构造器，无网络。
- `event.rs` — `MessageEvent` 及各 notice/request 事件结构体 + `deserialize_id`（数字/字符串 ID 兼容）。
- `message.rs` — `OneBotMessage`（untagged Text|Segments）与 `MessageSegment`（22 种段，`#[serde(other)]` 兜底）。
- `websocket/client.rs` — ws-client 模式：连接/心跳/指数退避重连、消息去重、消息事件处理（spawn 任务）、notice/request 事件、API 请求-响应（echo 匹配）、流式分片路由。
- `websocket/server.rs` — ws-server 模式：路径+token 握手校验、多连接注册表（self_id→tx）、MAX_CONNECTIONS=100、每连接 Semaphore(10)、只处理 message 事件。
- `websocket/sender.rs` — 全局单例（outbound/api/stream 三条 mpsc，容量 256），`call_stream_api_via_ws_limited` 分片重组 + 大小限额。
- `media/` — URL 分类（detector）→ NapCatStream / DirectHttp / Base64 / LocalFile 四种下载策略 + 失败回退链；`save_to_local` 含 workspace canonicalize 边界检查。
- `outbound.rs` — chat_id 解析（group:/user:）→ send_group_msg/send_private_msg。

**wecom（企业微信，三种模式）**
- `wecom.rs` — 模块根：全局注册表（LONGCONN_REGISTRY / CHAT_REQID_REGISTRY / SEEN_MSG_IDS=512 / token cache）、常量、长连接消息 JSON 结构体。
- `webhook.rs` — 回调 URL 验签（GET echostr / POST Encrypt）、SHA1 手写实现、多 account 按签名解析 config、消息类型分派（text/image/voice/video/file/location/link）、msg_id 去重、allowlist。
- `longconn.rs` — 智能机器人 WS 长连接：subscribe/ping-pong/aibot_msg_callback 入站构建 InboundMessage、出站文本/媒体（LoopAction 跳出 select 做分片上传）。
- `media.rs` — 4 参验签、AES-256-CBC 解密（手工 PKCS7 unpad，块大小 32）、REST media/download+upload、长连接分片上传（512KB×100）、ffmpeg amr 转换。
- `dispatch.rs` / `polling.rs` / `send.rs` — 轮询模式（实为 token 心跳）、直连 REST 发送、消息分片（2048 字符）。

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| **P0** | `wecom/webhook.rs:436-462` | **Webhook 签名验证可被整体绕过**：`<Encrypt>` 标签缺失即跳过验签，body 被当作明文 XML 直接解析入站 | 验签条件为 `!token.is_empty() && !msg_encrypt.is_empty() && !verify_signature_4(...)`；`msg_encrypt` 来自 `extract_xml_tag(body,"Encrypt")`（攻击者可控）。攻击者 POST 不含 Encrypt 的明文 XML（`FromUserName`/`Content` 自造）即可穿过 403 分支；随后 `decrypted_body = if !msg_encrypt.is_empty() && !key.is_empty() {...} else { body }` 落入 else，明文 body 直接被当作消息体提取并转发给 agent（webhook.rs:459-467）。即使 token/aes_key 均正确配置也成立，只需回调 URL 可达。结合"allow_from 默认为空"，构成对 LLM agent 的未认证消息注入（prompt injection 主攻击面入口失守） |
| P1 | `napcat/websocket/client.rs:624-710` | **notice/request 事件完全绕过 allowlist/blocklist/群过滤** | message 事件走 `is_user_allowed`/`is_group_allowed`/`should_respond_to_group`，而 recall/increase/decrease/admin/ban/friend_add/poke/friend_request/group_request 处理器直接构造 InboundMessage 入站，无任何权限检查。更严重的是 `friend_request`/`group_request` 的验证信息 `comment` 是攻击者原样文本，被拼接进 content（client.rs:524-532、566-577）——被拉黑用户/群外用户仍可向 agent 注入可控内容；且这些事件无去重 |
| P1 | `napcat/message.rs:159-166` + `client.rs:133`/`server.rs:865` | **@全体成员（@all）协议解析错误**：`is_at_all()` 只匹配 `MessageSegment::AtAll{}`，但 OneBot v11 / NapCat 实际发送的是 `{"type":"at","data":{"qq":"all"}}` | `MessageSegment` 无 `at_all` 这一 tag，`AtAll` 变体永远无法从真实事件反序列化。后果：① `at_only` 群响应模式对 @所有人 不触发（恰是安全默认档位失效）；② `to_plain_text_without_at` 只剥离 at 机器人的段，@all 残留为 `[@all]` 文本进 content；③ 无任何单测覆盖真实 @all JSON |
| P1 | `napcat/websocket/client.rs:1075-1082` | **大群消息风暴背压失效**：每条 WS Text 消息 `tokio::spawn` 一个处理任务，无信号量/上限 | client 模式每条消息 spawn 独立任务（含媒体下载、WS API 调用），任务内 `inbound_tx.send().await` 虽有界耐心等待，但读循环不减速，风暴下任务数与内存（每任务持有 MessageEvent + 媒体下载态）无界增长。对照：server 模式同文件 501 行用 `Semaphore::new(10)` 做了限制，wecom longconn 在 select 内联处理天然背压——同一 crate 内三种模式三种做法 |
| P1 | `napcat/websocket/client.rs:906-909` | **`heartbeat_interval_secs=0`  panic**：`Duration::from_secs(heartbeat_interval_secs as u64)` 直接喂 `tokio::time::interval`，零周期 panic | config（`core/src/config/channels.rs:428`，u32，默认 30）无校验/钳制；用户在 config.json5 填 0 即触发 tokio `interval` "timer duration must be non-zero" panic，channel 任务静默死亡。对照 wecom `longconn.rs:70` 用 `.max(10)` 做了防御——napcat 缺失同类保护 |
| P1 | `wecom/media.rs:76-82` | **media_id 未消毒 → 路径穿越 + 切片 panic**：`filename = "wecom_{type}_{&media_id[..min(16)]}.{ext}"` 直接 `media_dir.join(filename)` | `media_id` 来自解密/明文 XML。明文回退路径（P0 同源）下 `media_id` 可含 `../`（穿越 media_dir 任意写）或中文（16 字节落在字符中间 → slice panic）。`wecom/webhook.rs:90-96` 的长连接版本用时间戳生成文件名（安全），REST 版本却没有 |
| P1 | `wecom/media.rs:501-573` | **媒体上传期间入站消息丢失**：`longconn_wait_response` 独占 `read`，非匹配 req_id 的帧（含 `aibot_msg_callback` 真实用户消息）只 debug 日志后丢弃 | 上传 512KB×N 分片期间（可达 60s 超时上限）到达的用户消息被静默吞掉，既不转发也不入 SEEN_MSG_IDS，WeCom 不保证重投 → 消息级数据丢失。应在等待循环内将 callback 帧分支给正常处理路径 |
| P2 | `napcat/event.rs:344-381` | **OneBot v11 notice 事件字段必需性与协议不符，事件被静默丢弃** | `GroupDecreaseEvent.operator_id: String` 无 `#[serde(default)]`，而协议规定 sub_type="leave"（主动退群）时 operator_id 不存在 → 解析失败，`client.rs:644` `if let Ok(...)` 静默跳过。`GroupBanEvent.operator_id`、`GroupRecallEvent.operator_id` 同为非可选。建议改 `#[serde(default)]` 并对 leave 场景默认填 user_id |
| P2 | `napcat/websocket/server.rs:609-614, 583-587` | **ws-server 模式功能残缺 + 竞态清理**：① notice/request 事件整体不处理（client 模式有的 recall/ban/request 转发全部缺失）；② 同 self_id 重连时新连接注册后，旧连接关闭清理会 `conns.remove(self_id)` 把**新**连接摘掉 | `handle_ws_message` 的 `"notice" => {}`、`"request" => {}` 空实现；清理逻辑仅按 self_id 删除不校验 remote_addr/连接代号 |
| P2 | `napcat/websocket/server.rs:504-580` | **无心跳/读超时的僵尸连接**：server 不主动 ping，无 read timeout | 对端 TCP 静默死亡时 `read.next()` 不返回，连接任务永久挂起、连接记录与 connection_count 泄漏，且占住 outbound 路由（`conns.values().next()` 首个连接可能是死连接） |
| P2 | `wecom/longconn.rs` `webhook.rs:61-82` | **长连接媒体下载无大小上限**：`resp.bytes().await` 全量读入内存，仅靠 client 30s 总超时兜底 | 恶意/被攻陷的 CDN URL 可在 30 秒内推送任意大体量响应造成内存压力。对照 napcat `direct_http.rs:87` 有 `read_response_limited(response, max_bytes)`、`napcat_stream.rs:55` 有大小校验——wecom 两条下载路径（webhook.rs:79、media.rs:84）均缺限流 |
| P2 | `wecom/send.rs:18` | **长连接出站队列满即丢消息**：`tx.try_send` 失败仅 warn | 队列容量 64，突发回复（或上传阻塞导致积压）时消息静默丢失。至少应 await send 或将失败上报给调用方 |
| P2 | `docs/06_channels.md:323-337` / `docs/23_weixin_integration.md` | **契约不一致：long_connection 模式完全无文档** | 代码支持 `long_connection/long-connection/stream` 三态（dispatch.rs:99、send.rs:8、media.rs:680），依赖 bot_id/bot_secret/aibot WS 协议、pingIntervalSecs 等配置；`docs/06_channels.md` 的 wecom 一节只写 webhook/轮询两种方式且配置示例无相关字段；`docs/23_weixin_integration.md`（159 行）通篇不含 wecom。napcat 的 `group_response_mode`、notice 事件转发、媒体自动下载等能力同样无文档 |
| P2 | `napcat/websocket/{client,server}.rs` 测试节 | **协议解析缺少向量化单测**：无完整 OneBot 事件 JSON 反序列化用例 | 现有单测只覆盖 allowlist 逻辑与手工构造的 Rust 结构体。@all 误解析（P1）、operator_id 缺失丢事件（P2）、流式分片路由、dedup 淘汰、半包/超大消息边界均无用例。wecom 侧 sha1 有已知向量（好），但 `decrypt_wecom_msg` 无真实加解密往返用例、`extract_xml_tag` CDATA/属性无边界用例、`percent_decode` 无 `%2B`/截断用例 |
| P3 | `napcat/websocket/client.rs:35`、`server.rs:35` | `mod api;` 悬空声明（文件不存在，疑为误删残留） | 两个文件各有一行 `mod api;`，若 `websocket/api.rs` 不存在应编译失败——需确认是否存在该文件（本次只读审查未展开确认），否则是死代码 |
| P3 | `napcat/websocket/client.rs:984-1008`、`server.rs:326-368` | API 调用失败时 pending 请求键不匹配导致泄漏 | 插入用生成的 `echo`（如 `"42"`），错误分支却 `pending.remove(&request.echo.unwrap_or_default())`（echo 为 None 时删的是 `""`）→ 条目残留；重连时 `pending_requests`/`active_streams` 亦不清空，流式中途断线的 chunk_tx 永久驻留 |
| P3 | `napcat/websocket/client.rs:851-860` | access_token 拼 URL 未做 percent-encoding | 含特殊字符的 token 会破坏 URL；建议 `form_urlencoded` 或仅用 Authorization 头 |
| P3 | `napcat/websocket/client.rs:614-621` | 解析失败时二次 `from_value` 只为拼错误信息 | 同一条消息反序列化两遍，大消息风暴下是纯浪费，可用首次错误直接传播 |
| P3 | `napcat/websocket/client.rs:74-90`、`server.rs:81-85` | dedup 淘汰为 HashSet 任意序删除、容量硬编码 10_000 | 非 LRU，两条消息 ID 差值跨越淘汰窗口可能漏判；`seen.clear()`（wecom）在 512 满时全清同理。建议注释说明取舍或改插入序结构 |
| P3 | `wecom/wecom.rs:24-27` vs `wecom/longconn.rs:355-366` | `SEEN_MSG_IDS` 去重放在 allowlist 之后（webhook）/之前（longconn）顺序不一致 | 语义差异小但行为不一致；另 `std::sync::Mutex::lock().unwrap()` 在 panic 毒化后会连锁 panic |
| P3 | `wecom/send.rs:98`、`media.rs:766` | `chat_id.starts_with("wr")` 魔法字符串启发式区分群/个人 | WeCom 群 chatid 规范以 wr 开头，可行但无出处注释；`chat_type: 1`（media.rs:464）、100 分片上限、300ms 分片间隔、5s 重连延迟均为内联魔法值 |
| P3 | `wecom/media.rs:143-155`、`webhook.rs:401` | 签名比较 `hash == expected` 非恒定时间 | 理论时序侧信道；`subtle`  crate 或自行 xor 累加比较更稳妥（实际利用难度高，仅建议） |
| P3 | `wecom/media.rs:255-259` | 手工 PKCS7 unpad 未校验 `pad <= decrypted.len()` | token 为空 + Encrypt 可造的条件下，16 字节密文 + 末字节 17..32 → `decrypted.len() - pad`  usize 下溢 panic（debug）/越界 slice panic（release）。触发需特定错配配置（P0 同源路径），故列 P1 伴生项/P0 级风险已由 P0 项覆盖，此处单独记一笔 |
| P3 | `wecom/wecom.rs:67-68` | `CHAT_REQID_REGISTRY` 只增不删 | 每个 chat 一条 req_id，长期运行缓慢增长；建议按 TTL/容量淘汰 |
| P3 | `napcat/media/detector.rs:155-166` | QQ CDN 识别用 `domain.contains(pattern)` 后缀/子串匹配 | `notgchat.qpic.cn.evil.com` 会被判为 QQ CDN 影响下载器选择（有回退链兜底，不影响安全边界，仅分类失准） |
| P3 | `napcat/websocket/sender.rs:201-310` | `call_stream_api_via_ws_limited` 在 done=Ok 且缺块时可忙等自旋 | 当前无任何调用方发送 `Ok(())` done 信号（仅错误路径），属潜在死代码路径；`select!` 中 `chunk_rx.recv()` 立即返回 None 时会无 await 点空转烧 CPU |

## 已检查未发现明显问题的维度

- **① 协议事件解析边界（半包/乱序/超大消息）**：WebSocket 帧重组由 tokio-tungstenite 负责，半包不会到达文本层；tungstenite 默认消息大小上限兜底；乱序在本协议内（单 WS 有序流）不适用。事件解析失败均为静默跳过而非 panic（除上述 operator_id 丢弃与 @all 误判）。消息段编解码 `OneBotMessage` untagged + `#[serde(other)]` 兜底设计良好，`string_or_number` helper 处理 string/number 两形态 ID 覆盖面正确。**撤回/编辑事件**：`group_recall` 已实现（通知 operator 与 user_id），**编辑事件（message_edited / notify+group_msg）未实现**——OneBot v11 无标准编辑事件，但 NapCat 有扩展上报，属功能缺口而非缺陷，记此备查。
- **② 并发-心跳/重连**：napcat client 的指数退避（5s→60s 封顶、成功重置、睡眠期可被 shutdown 打断）、wecom 的 5s 固定重连 + shutdown 竞速选择均正确；token 缓存带 300s 提前刷新余量、40014/42001 自动清缓存重试一次，实现扎实。
- **③ 安全-凭据管理**：access_token 不进日志（server.rs 启动日志只打 configured/none；client.rs:873-882 只打脱敏 URL）；wecom 诊断结构体有防泄漏单测（webhook.rs:692-713）、`decrypt_errors_do_not_echo_secrets` 单测存在；napcat ws-server 的 `validate_server_auth` 对非 loopback 绑定强制 token（含单测）是本轮亮点。
- **④ 性能（媒体路径）**：napcat 媒体下载链路完整——大小上限（max_auto_download_size 多处强制）、`read_response_limited`、workspace canonicalize 边界检查、`safe_path_component`/`unique_media_filename`、DirectHttp 禁重定向（`redirect::Policy::none()`）防 SSRF 跳转；数据先落内存再写盘，无流式落盘（超大文件内存峰值=文件大小，有上限兜底，可接受）。
- **⑤ 测试覆盖**：见问题清单 P2；补充：两条通道的"纯函数"层（types/消息构建/分片/验签/配置解析）单测密度良好且有真实向量（SHA1("abc")、多 account 签名匹配）。
- **⑥ 可维护性**：模块拆分清晰（wecom 从单体拆出 6 子模块、napcat media 策略模式 + trait 对象、回退链有序），注释解释"为什么"到位（如 QQ CDN 必须走 DirectHttp 的踩坑说明）；扣分项主要是上述魔法常量与 client/server 双份重复代码（`is_duplicate`/`DEDUP_CACHE`/`is_user_allowed`/`should_respond_to_group` 在两个文件各实现一遍，行为已现分歧——server 缺 notice 处理）。
- **⑦ 契约一致性**：NapCatClient 双模式、allow_from/block_from/allow_groups、群响应模式与 `docs/06_channels.md:33` 的定位描述一致；主要缺口是 wecom long_connection 与 napcat 高级特性无文档（P2）。
- **全仓不变量核对**：`Tool::execution_environment` vs `requires_confirmation` 正交性不在本目录（无 Tool 实现），不适用。外部输入注入面：确认 channels 层为入口且**当前存在认证绕过路径**（P0），建议在 gateway/review 其他子代理处交叉确认回调端点的暴露面与鉴权中间件。

## 亮点

1. **napcat ws-server 的安全基线**：非 loopback 绑定强制 access_token，否则拒绝启动（server.rs:54-67 + 单测）；握手回调同时校验路径与 token（Authorization Bearer 头 + `?access_token=` 双通道，兼容 OneBot 11 各家实现）。
2. **媒体下载防御纵深**：`file:` 源统一拒绝（downloader.rs:157-161 + 单测）、`save_to_local` 写盘前 canonicalize + `starts_with(workspace)` 断言、文件名 sanitize、多策略回退链（NapCatStream→DirectHttp→Base64）、DirectHttp 禁重定向——路径穿越面在 napcat 侧基本封闭。
3. **wecom 解密细节的踩坑沉淀**：AES key 尾部比特宽容解码（media.rs:213-218）、PKCS7 块大小 32 的手工 unpad、echostr percent-decode（webhook.rs:365-369 注释解释 WeCom 签名对象是原始 base64）——都是难查的协议坑，注释完整。
4. **流式下载重组设计**：`call_stream_api_via_ws_limited` 按 chunk_index 重排、stream_id 一致性校验、max_bytes 限额、120s 超时、placeholder→实际 stream_id 重映射，考虑了首包未知名场景。
5. **多账号/多监听器支持**：wecom 按 msg_signature 区分 account（webhook.rs:241-328），napcat 按 self_id 路由连接，均为正确的多租户思路且有单测。
6. **去重、毒化防护、日志脱敏**等工程细节普遍到位（msg_id 去重、`from_user.starts_with('@')` 过滤机器人回声、token 不落日志、错误不回显密钥）。

## 建议（按优先级）

1. **立即修 P0**：webhook 中把"解析 `<Encrypt>`"改为强校验——配置了 encoding_aes_key 或 callback_token 时，缺少 Encrypt/msg_signature 一律 400/403，禁止明文回退；明文模式仅当两者均未配置且显式声明时允许。同时为 media_id 增加 `safe_path_component` 式消毒，堵住穿越与切片 panic。
2. **修 P1**：notice/request 事件统一走 allowlist/blocklist（至少 friend/group request 的 comment 注入面要拦住，可配置"是否转发 notice"）；`is_at_me`/`is_at_all` 增加对 `At{qq:"all"}` 的判定并补真实 JSON 单测；client 模式引入 Semaphore 或有界 worker 池对齐 server 模式；`heartbeat_interval_secs` 加载时 `.max(1)` 或 `max(5)` 钳制；`longconn_wait_response` 内将 callback 帧转交正常处理。
3. **补齐协议兼容与文档**：notice 事件 operator_id 改可选；server 模式对齐 notice 处理；`docs/06_channels.md` 增加 napcat 群响应/媒体自动下载与 wecom long_connection（bot_id/bot_secret/ping_interval_secs）章节。
4. **测试补强**：以真实抓包的 OneBot v11 事件 JSON（含 @all、缺 operator_id 的 leave、撤回、文件段）做反序列化向量测试；wecom 补 `decrypt_wecom_msg` 加解密往返、XML CDATA/属性边界、`percent_decode("%2B")` 边界用例。
5. **可维护性**：client/server 共享的 `DEDUP_CACHE`/权限检查逻辑提取到 napcat 公共模块消除双份实现；清理两处悬空的 `mod api;`；`ChatReqIdRegistry` 加容量上限；统一 `pending_requests` 错误清理的 echo 键。

---

*统计：P0 × 1，P1 × 6，P2 × 7，P3 × 12。所有结论均基于静态阅读，未运行构建与测试；P3 中 `mod api;` 一条建议在修复前先用 `ls websocket/api.rs` 确认；P3 的 PKCS7 unpad 边界项与 P1 的 media_id 项同属明文回退路径，修复 P0 时可一并处理。*
