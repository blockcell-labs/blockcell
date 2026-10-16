# gateway WS/Webhook/Stream Review 报告（Wave 4 · 入口层）

> 只读审查 · 2026-10-16 · 子代理 w4-gateway-ws

## 规模

| 文件 | 行数 | 职责 |
|---|---:|---|
| `gateway/websocket.rs` | 964 | WS 升级 + 鉴权 + 收/发双任务 + 会话订阅过滤 + 广播扇出 |
| `gateway/chat.rs` | 379 | `/v1/auth/login`、`/v1/chat`、`/v1/health`、`/v1/tasks`、会话 id 生成、登录限流 |
| `gateway/alerts.rs` | 268 | `/v1/alerts*` 告警规则 CRUD（文件型 JSON store） |
| `gateway/webhooks.rs` | 195 | `/webhook/{lark,wecom,qq}` 公开回调入口 → channels 层 |
| `gateway/streams.rs` | 61 | `/v1/streams*` 流订阅管理 |
| `gateway.rs`（装配部分） | 135–260 / 578–617 / 1020–1153 / 1480–1520 / 1690–1918 | GatewayState、WsEvent、auth_middleware、confirm handler、outbound bridge、Router 挂载 |
| 合计审查量 | ~3.2K LOC（含 gateway.rs 装配切片） | |

关联外部文件（用于契约/转调核对，只读）：`crates/agent/src/runtime/event_listeners.rs`、`crates/agent/src/runtime/message_dispatch.rs`、`crates/channels/src/wecom/webhook.rs`、`webui/src/lib/ws.ts`、`webui/src/lib/store.ts`。

## 模块地图

### 1. WS 消息类型表（后端发出 → 前端 `WsEventType`）

| # | type | 后端发出点 | 载荷关键字段 | 前端声明 |
|---|---|---|---|---|
| 1 | `token` | `message_dispatch.rs:101` | channel/agent_id/chat_id/delta | ✅ |
| 2 | `thinking` | `message_dispatch.rs:114` | channel/agent_id/chat_id/content | ✅ |
| 3 | `stream_reset` | `message_dispatch.rs:234` | channel/agent_id/chat_id | ✅ |
| 4 | `message_done` | `turn_flow.rs` / `run_loop.rs` / `skill_session.rs` / `process_message_*` / websocket.rs(斜杠命令) | content/is_markdown/task_id | ✅ |
| 5 | `tool_call_start` | `event_listeners.rs:42` | **tool**/call_id/params | ✅ |
| 6 | `tool_call_result` | `event_listeners.rs:59` | **tool**/call_id/result/**is_error**/**outcome**/duration_ms | ✅ |
| 7 | `session_bound` | `gateway.rs:141`（`WsEvent::SessionBound`，websocket.rs:404） | client_chat_id/chat_id/agent_id | ✅ |
| 8 | `confirm_request` | `gateway.rs:1090`（confirm handler） | request_id/**tool_name**/paths/ws_connection_id | ✅ |
| 9 | `error` | `gateway.rs:175`（`WsEvent::Error`） | channel/chat_id/message | ✅ |
| 10 | `session_renamed` | `process_message_inner.rs:186` | — | ✅ |
| 11 | `system_event_notification` | `wiring.rs:582` | event_id/priority/title/body | ✅ |
| 12 | `system_event_summary` | `wiring.rs:643` | compact_text/items | ✅ |
| 13 | `agent_progress` | `gateway.rs:147` | task_id/progress_type/tokens_added/… | ✅ |
| 14 | `agent_stage` | `gateway.rs:163` | task_id/stage/percent | ✅ |
| 15 | `skills_updated` | `run_loop.rs:514` / `tool_pipeline.rs:677` | new_skills | ✅ |
| 16 | `evolution_triggered` | `capabilities.rs:987` | — | ✅ |
| 17 | `alert_triggered` | **无后端发出点** | — | ✅（死契约，见 P3-1） |
| — | `evolution_resumed/stopped/deleted` | `capabilities.rs:1072/1175/1221/1240` | — | ❌ 前端 union 未声明（P3-2） |

**核对结论（契约一致性）**：
- `tool_call_result` 载荷**现已包含 `is_error` 与 `outcome`**（`event_listeners.rs:68-69`），与前端 `store.ts:514`（`event.is_error ? 'error' : 'done'`）一致 → 任务书所述「UI 全绿」的已知缺陷**在当前后端载荷下已修复**。
- `tool_call_start` 后端字段名为 `tool`（非 `tool_name`），前端 `store.ts:480` 读 `event.tool` → 一致。
- `confirm_request` 后端发 `tool_name`（`gateway.rs:1093`），前端已按 `tool_name` 修复 → 一致。
- 可见性过滤白名单（`websocket.rs:109-116`）额外放行 `evolution_resumed/stopped/deleted`，但前端类型 union 未收录（P3-2）；`alert_triggered` 两侧不一致（前端有、后端无）（P3-1）。

### 2. 路由 → channels 桥接 / 鉴权分层

```
Router (gateway.rs:1764)
├── 受 auth_middleware(Bearer) 保护 —— 路由在 .layer(...) 之前注册
│   ├── /v1/auth/login  ┐
│   ├── /v1/health      ├ auth_middleware 内显式放行 (gateway.rs:589)
│   ├── /v1/ws          ┘ 放行，改由 handle_ws_upgrade 自校验 (gateway.rs:594)
│   ├── /v1/chat → handle_chat → inbound_tx → dispatcher → per-agent runtime
│   ├── /v1/alerts*   → alerts.rs（文件 store: workspace/alerts/rules.json）
│   ├── /v1/streams*  → streams.rs → blockcell_tools::stream_subscribe
│   └── …(其余 100+ 路由)
├── 公开（.layer 之后注册，无鉴权）
│   ├── /webhook/lark  → blockcell_channels::lark::process_webhook
│   ├── /webhook/wecom → blockcell_channels::wecom::process_webhook  ← P0 转调路径
│   └── /webhook/qq    → QQChannel::handle_webhook_payload
└── 事件总线
    runtime.event_tx ─┐
    WsToolEventListener ├→ broadcast::Sender<String>(cap=1000, gateway.rs:819)
    confirm handler ───┘        │
                       outbound_to_ws_bridge（仅转发非 ws/cli/http 出站）
                                │
                   每连接 broadcast::Receiver → 会话订阅过滤 → ws_sender
```

WS 入站分派：`chat`（含斜杠命令拦截）/ `confirm_response` / `cancel` / 未知类型降级为 chat（`websocket.rs:346-661`）。

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| **P0** | `gateway/webhooks.rs:113-121` → `crates/channels/src/wecom/webhook.rs:436-448,459-462` | **WeCom webhook 验签可被绕过（`<Encrypt>` 缺失即跳过验签）**，经公开 `/webhook/wecom` 入口可注入伪造消息 | 转调路径无补充校验：`handle_wecom_webhook` 原样把 `body`/`query` 交给 `process_webhook`。channels 层 POST 分支：`if !token.is_empty() && !msg_encrypt.is_empty() && !verify_signature_4(...)` —— 当攻击者 POST **不含 `<Encrypt>`** 的明文 XML 时 `msg_encrypt` 为空，`&&` 短路，**验签整段被跳过**；随后 `webhook.rs:451-462` 因 `msg_encrypt` 为空走 `else` 分支把 `body` 当明文 XML，`FromUserName`/`Content` 被直接提取（`:465-467`）并投递 `inbound_tx`（`:663-679`）。`/webhook/wecom` 注册在 `auth_middleware` **之后**（`gateway.rs:1911-1917`），公开可达 → 未认证即可伪造任意用户消息进入 agent。 |
| **P1** | `gateway/alerts.rs:61-104,118-163,176-204` | 告警规则文件 `rules.json` 的读-改-写全程**无任何锁**，并发请求/告警引擎并发写入导致丢失更新与文件损坏 | 三个 handler 各自 `read_to_string → mutate → write`，无 `config_write_lock`（GatewayState 有该锁却未使用），也无 per-file mutex。两个并发 create 会互相覆盖；create 与 update 并发时后写者覆盖前写者，且非原子写可能截断 JSON。`/v1/alerts` 是普通鉴权路由，任意已登录客户端可触发。数据损坏级。 |
| **P1** | `gateway/websocket.rs:291-294` vs `315-689` | **广播滞后时只终止发送半，接收循环继续运行** → 僵尸连接 | `send_task` 收到 `RecvError::Lagged` 时 `let Ok(msg) = received else { break }` 直接退出，`ws_sender` 被 drop，但主接收循环（`while let Some(msg) = ws_receiver.next()`）不感知，继续读取并把消息送入 `inbound_tx`。结果：客户端既收不到 close 帧也收不到后续响应，agent 仍持续为该连接处理消息；`send_task.abort()`（`:691`）只在客户端真正断开后执行。慢消费者策略不自洽。 |
| **P1** | `gateway/chat.rs:232-241` + `gateway.rs:1066-1075` | 经 `/v1/chat` 且 `channel="ws"` 驱动时，工具确认**静默自动拒绝** | `handle_chat` 构造 `InboundMessage` 时 `metadata: serde_json::Value::Null`（`chat.rs:239`），不含 `ws_connection_id`；confirm handler 对 `channel=="ws"` 的请求要求连接绑定，缺失即 `response_tx.send(false)` 并 return（`gateway.rs:1066-1075`）。于是需要确认的工具在此路径下永远被拒，用户无任何提示。设计不一致/功能缺陷。 |
| P2 | `gateway/websocket.rs:638-660,662-684` | 未知 `type` 与非法 JSON 均降级为 chat，且未知类型把**整段原始 JSON 字符串**当 content 发送 | `_ =>` 分支用 `content: text.to_string()`（`:652`）——即把 `{"type":"foo",...}` 原文当用户消息。同时该降级与纯文本降级都把 `chat_id` 固定为 `"default"`，多连接共享同一 `("default","default")` 订阅 → 不同标签页/客户端互相看到对方消息。 |
| P2 | `gateway/chat.rs:232-241` | `/v1/chat` 允许调用方任意指定 `channel`/`account_id`/`sender_id`/`chat_id` | 已鉴权客户端可注入 `channel:"telegram"` 等，消息被 agent 处理后经 outbound 路由到该渠道/账号，构成渠道伪造面；`account_id` 完全由客户端控制。虽需 Bearer，仍属越权设计面。 |
| P2 | `gateway.rs:594-596` + `gateway/websocket.rs:206-224` | `/v1/ws` 绕过 `auth_middleware`，鉴权在升级处理器内二次实现 | 两处逻辑分叉：middleware 直接放行 `/v1/ws`；真正校验在 `handle_ws_upgrade`，其中 `_ => true`（无 token 时开放，`websocket.rs:223`）。当前靠 `gateway.rs:1700-1716` 的兜底 token 保证 `api_token` 恒非空，但该不变量是隐式且脆弱的（若未来兜底被移除即退化为开放访问）。 |
| P2 | `gateway/websocket.rs:567-577` | `confirm_response` 锁序为 `pending_confirms` → `subscriptions`，且持全局 map 锁期间去锁每连接 mutex | 当前无反向锁序，暂无死锁，但属未文档化不变量；持全局 `pending_confirms` 锁时获取 per-connection 锁，扩大临界区。建议先取订阅再取 map。 |
| P2 | `gateway/streams.rs:57-60,34-39` | streams 错误响应状态码语义不一致 | `handle_stream_data` 出错返回 **HTTP 200** + `{"error":...}`；`handle_stream_delete` 把**所有**错误映射为 404（含非「未找到」错误）。与 API 其余部分语义不符，客户端难以区分成功/失败。 |
| P2 | `gateway/channel_ids.rs:314-328` | `resolve_requested_agent_id` 允许任意已鉴权 WS/HTTP 客户端寻址任意 agent | 仅校验 `config.agent_exists`，无 per-agent 授权。单租户假设下可接受，但无文档说明；多 agent 部署时应显式声明该信任边界。 |
| P3 | `webui/src/lib/ws.ts:14` | 前端声明 `alert_triggered` 但后端无任何发出点 | 全仓 grep 无 `"alert_triggered"` 发出点 → 死契约项。 |
| P3 | `webui/src/lib/ws.ts:16` vs `capabilities.rs:1072/1175/1221/1240` | 后端广播 `evolution_resumed/stopped/deleted`，前端 union 未声明 | 仅被 `'*'` 通配监听兜底，强类型消费者会漏收。 |
| P3 | `gateway/websocket.rs:332-335` | Ping/Pong 计入入站限流配额 | 每 10s 上限 60 条，含 Pong；高频保活的客户端可能被误断（`ws_rate_limiter_applies_to_non_text_messages` 测试亦固化了此行为）。 |
| P3 | `gateway/websocket.rs:104` | 可见性过滤对「每事件 × 每连接」做完整 JSON 解析 | 注释称用 `WsEventRouting` 避免分配，但 serde 仍会扫描整段字符串，token 热路径上为 O(连接数 × 事件数) 解析开销。 |
| P3 | `gateway/alerts.rs:96-100,155-159,196-200` | `to_string_pretty(&store).unwrap_or_default()` 失败时写入空文件 | 对 `serde_json::Value` 实际不会失败，但模式本身会静默清空 store。 |
| P3 | `gateway/{streams,webhooks,alerts}.rs` | 零单元测试 | websocket.rs/chat.rs 有测试；streams/webhooks/alerts 无任何测试（含 P0/P1 所在路径）。 |

### 各维度小结

- **① 正确性**：WS 协议边界（1MB 上限 + 半包由 tungstenite 处理 + 非法 JSON 降级）基本覆盖；消息分发/广播过滤正确。问题集中在降级路径（P2-1）、`/v1/chat` 确认路径（P1-3）。webhook 路由分派正确但依赖下层验签（P0）。
- **② 并发**：连接断开清理（`send_task.abort()`）基本正确；广播扇出无阻塞（broadcast + 每连接独立 receiver）；**慢消费者/滞后处理不自洽（P1-2）**；alerts 无锁读改写（P1-1）；流式背压依赖 broadcast 容量 1000。
- **③ 安全（主维度）**：**WeCom 验签绕过 P0**（gateway 转调路径无补充校验，已核实）；WS 鉴权本身健全（4401 close + 常量时间 `secure_eq` + 子协议 hex 解码，与文档一致）；`/v1/ws` 双重实现的不变量脆弱（P2）；`/v1/chat` 渠道/账号客户端可控（P2）。
- **④ 性能**：慢消费者被断开（策略见 P1-2）；每事件每连接 JSON 解析（P3）。
- **⑤ 测试覆盖**：WS 过滤/限流/尺寸/确认授权测试充分；streams/webhooks/alerts 无测试。
- **⑥ 可维护性**：模块拆分清晰、注释充分；alerts 三处重复的 read-modify-write 样板可抽公共 store；锁不变量未文档化（P2）。
- **⑦ 契约一致性**：17 类消息与前端基本对齐；**已知 `tool_call_result` 缺 outcome/is_error 的问题已修复**；`alert_triggered`（P3-1）、`evolution_*`（P3-2）存在漂移。`docs/08_gateway_mode.md` 仅示例 token/message_done，未提供完整类型表，无法逐项比对（文档侧缺口）。

## 亮点

1. **WS 鉴权设计正确**：HTTP 升级后以 close code **4401** 表达未授权（`websocket.rs:234-243`），使浏览器端可区分 `auth_failed` 与 `1006` 网络异常；token 走子协议 UTF-8-hex（`token_from_websocket_protocol:248`），与 `docs/08_gateway_mode.md:254-260` 一致，且明确不从 URL query 读取。
2. **常量时间比较**：`secure_eq`（`util.rs:64`）用于 token/密码比对，避免时序侧信道。
3. **会话级可见性过滤**：`ws_event_visible_to_connection`（`websocket.rs:99-143`）按 `(agent_id, chat_id)` 订阅集合隔离，并强制 `channel=="ws"`，防止跨 agent/跨会话/外部渠道事件泄漏；`confirm_request` 额外绑定 `ws_connection_id`（`websocket.rs:138-140`），单元测试覆盖充分。
4. **出站不镜像到 WS**：`outbound_to_ws_bridge`（`outbound.rs:6-10`）刻意不把外部渠道出站消息广播给 WebUI，并有回归测试防止泄漏。
5. **已知缺陷已修复（需归档）**：AgentEvent→WS 转译（`event_listeners.rs:52-71`）现同时输出 `is_error` 与 `outcome`，与前端 `store.ts:514` 对齐；`tool_call_start` 的 `tool` 字段与 `confirm_request` 的 `tool_name` 字段均与前端一致。
6. **入站防护**：1MB 消息/帧上限 + 60 msg/10s 限流 + 关闭连接，均有对应单元测试（`websocket.rs:895-939`）。

## 建议

1. **[P0]** 在 `crates/channels/src/wecom/webhook.rs` 收紧 POST 分支：`callback_token` 非空时，`msg_encrypt` 为空**必须直接 403**，不得走明文分支；或要求 `msg_encrypt` 非空才能进入处理。gateway 侧可在 `handle_wecom_webhook` 增加前置断言（配置了 token 时 body 必须含 `<Encrypt>`）作为纵深防御。同时补一条「无 Encrypt 必须 403」的回归测试。
2. **[P1]** alerts store 引入与 `config_write_lock` 同级的串行化（per-file mutex 或复用 `config_write_lock`），并把读-改-写抽为单一 `with_alert_store(|store| ...)` helper 消除三处重复。
3. **[P1]** 统一慢消费者策略：`Lagged` 时**跳过丢失事件并继续**（记 warning + 计数），仅在连续多次滞后或发送失败时关闭；或在 `send_task` 退出时同步关闭接收半（`ws_receiver.close()` / 通过 `Notify` 通知主循环 break）。
4. **[P1]** 修复 `/v1/chat` + ws 确认：`handle_chat` 为 `channel=="ws"` 生成并注入 `ws_connection_id`（或对 HTTP 来源的 ws 请求禁用需确认工具并回传明确错误），使确认语义一致。
5. **[P2]** 未知 `type`/非法 JSON 降级时使用解析出的 `content` 字段而非原文；并为降级会话分配独立 `chat_id`，避免共享 `default` 会话导致跨标签页串消息。
6. **[P2]** `/v1/chat` 对 `channel` 做白名单/所有权校验（或强制 `route_match_level`），收紧 `account_id` 注入面。
7. **[P2]** 在 `auth_middleware` 中显式注释 `/v1/ws` 放行的原因与不变量；为 `api_token` 恒非空加 `debug_assert` 或类型化（`NonEmptyToken`），避免兜底逻辑被误删后静默开放。
8. **[P2]** 调整 `confirm_response` 锁序为先订阅后 map，并在代码注释中记录锁序不变量。
9. **[P3]** 对齐前端/后端事件类型集合：移除或实现 `alert_triggered`，把 `evolution_resumed/stopped/deleted` 纳入前端 union；在 `docs/08_gateway_mode.md` 补全 WS 消息类型表。
10. **[P3]** 为 streams/webhooks/alerts 补最小单元测试（尤其 alerts 并发写、webhook 缺 Encrypt）。
