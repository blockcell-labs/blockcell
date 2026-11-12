# B5：入口、Gateway 与前端审查

> 历史记录（2026-11-11）：原 24 项已于 2026-11-12 复核。当前裁决、限定、工具链验证与新增问题以 [优化指导](README.md)、[复核矩阵](08-revalidation-matrix.md) 为准；本文旧统计及 Rust 阻断说明仅代表首次审查时的结果。

基线：`5ca38be`，分支 `main`。确认 **P1 × 1、P2 × 2**。

## 实际覆盖

- 精读 Gateway 路由分组、Bearer 中间件、WebSocket 升级凭据、事件订阅可见性、chat/cancel/confirm/斜杠命令分派。
- 精读 session list/get/delete/rename 与 SessionStore 的衔接，配置结构化/raw 保存及 reload；文件列表、内容、上传路径约束和有界读取；memory/cron 的 agent 范围选择。
- 精读 WebUI `auth.ts`、`api.ts`、`ws.ts`、`store.ts` 聊天事件分派，`App.tsx` 事件连接、`chat-page.tsx` 历史加载竞争保护；核对 `config-merge.ts`、LLM/config 页保存、带凭据文件 blob URL 获取。
- 精读 slash command context/clear，抽查 CLI 命令入口和静态 WebUI env 注入。

其他页面与 CLI 子命令仅抽查，未逐行审完所有 UI 组件、TUI、外部技能下载实现或 webhook 协议；未启动完整 Gateway 或浏览器。本轮配置/存储公共根因统一归 B1，不在此重复计数。

## 确认发现

### B5-01 · P1：非默认 Agent 的 Gateway /clear 操作默认 Agent 的持久数据

- 位置：`bin/blockcell/src/commands/gateway/websocket.rs:420`（传入 `state.paths.clone()`）；`bin/blockcell/src/commands/slash_commands/handlers/clear.rs:49`、`:63`、`:108`；普通渠道同类入口 `bin/blockcell/src/commands/gateway.rs:1380`。
- 前提：配置了 `ops` 等非默认 Agent。向 `/v1/ws` 发送 `{"type":"chat","agent_id":"ops","chat_id":"shared-id","content":"/clear"}`。默认 Agent 是否存在相同 chat id 决定是否同时误删其数据；不需要该前提就已经会清错目录。
- 调用链：WS 已解析 `resolved_agent_id`，并按它清除 response cache，但创建 CommandContext 时未调用 `state.paths.for_agent(&resolved_agent_id)`。`for_websocket` 原样保存 paths；ClearCommand 用它创建 SessionStore 并删除 workspace session memory/tool-results。
- 同类路径：普通 IM 渠道的 interceptor 已解析 agent_id，但 `CommandContext::for_channel` 同样传入根 `slash_paths.clone()`；因此问题不限于 WebSocket UI。
- 实际行为：目标仍是根目录 `base/sessions/…` 和根 workspace；预期应为 `base/agents/ops/sessions/…` 等。ops 的持久历史保留，默认 Agent 同 chat id 的历史/会话资源反而被删除。响应却标记 `agent_id=ops` 并报告清理结果。
- 影响：多 Agent 数据边界错误，清理无效并可能删除其他 Agent 历史。此项与 B1-01 的 legacy 文件名碰撞独立，即使 key 没有特殊字符也成立。
- 证据类型：静态完整调用链；`core/paths.rs:165` 明确非默认 agent 使用独立 base，`gateway/sessions.rs` 的 HTTP 删除入口已正确使用 `for_agent`，提供对照。没有执行真实数据删除。
- 建议：在创建所有本地斜杠命令 context 前解析 agent paths，并保持缓存、文件、任务/checkpoint 的 agent 范围一致。增加默认/ops 同 chat id 下 `/clear` 只影响 ops 的隔离回归。

### B5-02 · P2：前端把冒号与下划线会话 ID 合并，破坏服务端身份契约

- 位置：`webui/src/lib/store.ts:6`、`:268`、`:282`、`:313`；发送/取消方 `webui/src/components/chat/chat-page.tsx:352`、`:374`。
- 前提：同一 Agent 先后在同一 WS 连接使用 `a:b` 和 `a_b` 两个 chat id，或服务端自动生成 `default:<timestamp>` 形式的新会话。服务端 `assign_session_id` 对已有非 default id 原样保留，存储当前编码已区分两者。
- 实际行为：`normalizeSessionId` 无条件把 `:` 替换为 `_`；事件比较因此将两个不同会话视为相同，旧会话 token 可以进入当前会话。`session_bound` 也保存替换后的 id；发送/取消时 `chat-page.tsx` 再把所有 `_` 改回 `:`，因此原本含字面下划线的会话 `a_b` 会被发往/取消为 `a:b`。正常自动生成的 `default:123` 可经这对替换恢复，不能把自动会话第二轮一概认定为分叉。
- 影响：会话消息混入、会话列表条目合并或后续回合分叉到另一个持久会话。属于同一登录用户内的数据一致性问题，不把它夸大为绕过 Gateway 登录。
- 跨层核对：HTTP 列表也有同类有损身份转换。`core/session_key.rs:83` 的 `session_id_from_file_stem` 将当前 A 文件 `ws_a_b` 与当前 B 文件 `s~ws~3Aa_b` 都返回 `a_b`，`gateway/sessions.rs:129` 原样作为列表 id。当前持久文件路径能区分 A/B，不等于所有服务端 ID 接口都已无损；该跨层问题归本项一次计数。
- 动态证据：将当前真实 `store.ts` 使用项目 esbuild 打包到临时目录，在浏览器 storage/window 最小桩下导入，执行实际 store 方法：

```text
set currentSessionId=a_b; handle token(chat_id=a:b, delta=WRONG SESSION)
=> {"currentSession":"a_b","messages":["WRONG SESSION"]}
handle session_bound(chat_id=default:123, client_chat_id="")
=> {"currentSession":"default_123","serverChatId":"default:123"}
```

- 建议：在网络协议、UI state、localStorage 中保留原始 opaque chat id；旧 ID 兼容应在明确的迁移边界处理，不能用有损替换做身份比较。测试加入 `a:b` 与 `a_b` 并存和自动分配 id 的第二轮发送。

### B5-03 · P2：stream_reset 和 error 未按会话/Agent 过滤，影响当前聊天

- 位置：`webui/src/lib/store.ts:263`、`:429`、`:539`；来源 `crates/agent/src/runtime/message_dispatch.rs:480`。
- 前提：同一浏览器连接曾向会话 A 发送消息，随后切到会话 B 或另一 Agent；A 的响应流稍后中断/重试，或返回错误。WS 的 subscriptions 按发送过的会话累计，切换 UI 不会移除 A，因此 A 的事件可以合法送到这个连接。
- 实际行为：chatEventTypes 过滤列表只含 message_done/token/tool_call_start/tool_call_result/thinking，遗漏 stream_reset/error。A 的 reset 无条件删除当前 state 最后一条 streaming assistant；A 的 error 无条件追加到当前聊天并把 isLoading 设为 false。
- 影响：当前 B 的部分回答被删除，错误显示在错误会话，B 仍在执行时界面提前停止加载。这与 B5-02 不同：即使 chat id 完全不同也会发生。
- 动态证据：真实 store，当前 `chat-B` 存在 streaming assistant，发送 `stream_reset(chat_id=chat-A,agent_id=ops)` 后 messageCount 变为 0；再发送 A 的 error 后，B 新增 `❌ Error: A failed` 且 isLoading=false。服务端中断流确实发带 chat_id/agent_id 的 reset，已静态核对。
- 建议：所有有会话归属、会修改聊天 state 的事件使用同一 agent/chat scope 检查，独立区分全局错误。增加旧会话 reset/error 在切换后不得修改当前 state 的行为回归。

## 验证与历史复核

- 现有前端 29 单测、TypeScript 检查通过。现有 reset 测试只验证当前会话的删除行为，不能发现 B5-03；源码最小复现没有改动生产源码或测试断言。
- B1-01 的会话 legacy 删除问题已核对 HTTP `handle_session_delete` 的实际入口；不在此重复统计。
- 配置恢复密钥、文件有界读取、Bearer/subprotocol 鉴权、前端断线发送失败返回、历史加载的 agent/session 竞争检查当前已有实现，未把历史相关问题不经复核再次收录。
- 未进行完整端到端复现、真实恶意文件竞态或外部 webhook 联调；路径检查和后续打开之间的 TOCTOU 只保留为进一步核查方向。
