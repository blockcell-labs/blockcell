# webui Review 报告（Wave 5 · 周边）

> 只读审查 · 2026-10-16 · 子代理 w5-webui
> 审查范围：`/Users/apple/rustdev/magicbot/blockcell/webui/src`（未运行 cargo/clippy/test/npm；后端契约经只读 grep 核对）

## 规模（文件数/LOC/入口）

- **57 个 TS/TSX 文件，14,961 LOC**（其中 8 个测试文件 514 LOC，集中在 `lib/`）
- 入口：`main.tsx`（StrictMode）→ `App.tsx`（hash 路由 + 全局 WS 事件总线 + 确认弹窗 + 闹铃提醒）
- 最大文件：evolution-page 1340 / i18n 1286 / channels-page 919 / store 790 / api 743 / chat-page 668
- 路由：无 react-router，hash 路由（`#/page`）+ localStorage 持久化；17 个页面全部 `React.lazy` 独立 chunk

## 模块地图

- **状态管理**：zustand 7 stores（`lib/store.ts`）：chat / system-events / reminder-alerts / connection / theme / agent / sidebar；会话 id 与选中 agent 持久化 localStorage
- **WS 客户端**：`lib/ws.ts` WebSocketManager —— 子协议 hex token 鉴权（`authProtocol`）、指数退避重连（1s→30s，20 次上限）、1006 歧义码处理、`/v1/health` 探针区分 token 失效与服务宕机、generation 防竞态、连接状态 100ms 防抖广播；`lib/ws-batcher.ts` 16ms 窗口合并 token/thinking delta
- **API 层**：`lib/api.ts` —— 统一 fetch + 8s 超时 + Bearer header；文件下载走 blob + objectURL（token 不进 URL）
- **鉴权**：`lib/auth.ts` —— token 存 sessionStorage，带 localStorage→sessionStorage 迁移清理
- **聊天**：chat-page（虚拟列表 + ResizeObserver 测高 + 滚动防抖 + 会话历史竞态守卫）→ message-bubble（ToolCallCard）→ markdown-content（react-markdown + remark-gfm）
- **测试**：`lib/` 下 8 个 vitest 文件（store/ws-batcher/ws-connection/virtual-list/auth/config-merge/file-preview/persona-operations），组件层零测试

## 已知待办逐项核对（4/4 已闭环，HEAD `5a19eae` 今日修复）

| 待办 | 结论 | 证据 |
|---|---|---|
| ① /v1/tools 的 discoverable 过滤 | **属实，已修**（command-picker 侧） | command-picker.tsx:44-52 已 `filter(visibility !== 'discoverable')` 并按 `order` 排序；后端 capabilities.rs:19-21 确认返回 visibility/group/order。**纠偏**：dashboard-page.tsx:197 仍未过滤（全量渲染带 ON/OFF 开关，属 inventory 语义，见 P3-9） |
| ② 被拒绝/未开始调用 tool_call_result 无 outcome/is_error | **属实，已修** | 后端 event_listeners.rs:68-69 现补 `is_error: outcome.is_error()` + `outcome`；forked/run.rs:526 补 `is_error`；前端 store.ts:514 `is_error ? 'error' : 'done'`，store.test.ts:242 有回归测试 |
| ③ confirm_request 前端读 event.tool 工具名为空 | **属实，已修** | App.tsx:83 现为 `event.tool_name \|\| event.tool \|\| ''`；后端 gateway.rs:1093 发 `tool_name` |
| ④ task_update 死代码 | **属实，已移除** | store.ts handler 与 ws.ts 类型联合中的 `task_update` 均已删除（`git show 5a19eae`）。**纠偏**：当前 WS 类型联合为 **17 种**（非背景所述 18 种，18 是删除 task_update 前的计数） |

背景"AgentEvent → WS 只转译 ToolPreExecute/ToolResult，其余只进 Session Event Log"复核**属实**（event_listeners.rs:37-72 仅这两个事件构造 WS 载荷；token/thinking/message_done 等由 message_dispatch.rs / run_loop.rs 另行直发）。

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| P1 | lib/notifications.ts:5 | 浏览器通知权限从未申请，alert/system-event 通知实际永不可达 | `requestNotificationPermission` 全仓无调用点；`sendNotification`（:19）在 `permission !== 'granted'` 时静默 return。除非用户手动在浏览器设置授权，`notifyAlertTriggered`/`notifySystemEvent` 全部空转 |
| P2 | lib/store.ts:513-524 | `tool_call_result` 只更新"最后一条 assistant 消息"，乱序/跨消息时工具卡永久卡在 'running' | `updateLastAssistantMessage` 从数组尾部找 role==='assistant' 的消息再按 call_id 匹配；若结果事件到达时最后一条 assistant 已是新消息（alert_triggered 插入、后台投递、乱序），旧消息的 toolCalls 收不到结果，spinner 不停止。应改为从后向前搜索含该 call_id 的消息 |
| P2 | components/chat/chat-page.tsx:329-335 | FileReader 无 onerror/onabort，读文件失败时 Promise 永不 resolve | `reader.onload` 有、`onerror` 无；失败则 `handleSend` 卡死，`uploading` 永远为 true，发送按钮一直转圈 |
| P2 | components/files/files-page.tsx:60-79 | `fetchFiles` 竞态守卫只比对 agentId、不比对 currentPath | 快速切换目录时旧响应可覆盖新目录内容；对比 chat-page.tsx:220 `loadSessionHistory` 做了 sessionId+agentId 双守卫，此处缺失 |
| P2 | lib/ws.ts:4-21 | WsEventType 类型联合缺 `evolution_deleted` | 后端 capabilities.rs:1221/1240 实际发射该事件，evolution-page.tsx:190 已订阅——靠 string 字面量绕过类型检查，契约类型不完整（17 种 + 此 = 18 种） |
| P2 | lib/store.ts:479, 517-519 | 空 `call_id` 时 ToolCallCard key 为 ''（多条冲突）且 result 永不匹配 | `id: event.call_id \|\| ''`；`tc.id === event.call_id`（undefined）不成立，卡片停留 running |
| P3 | lib/notifications.ts:60 | `notifyTaskCompleted` 成为死代码 | task_update 移除后无调用点 |
| P3 | lib/store.ts:235-237 | handleWsEvent 直读 localStorage 的 `blockcell_selected_agent` | 与 useAgentStore 双状态源，切换 agent 时存在短暂不一致窗口 |
| P3 | components/dashboard/dashboard-page.tsx:194, 197 | tools 区不过滤 discoverable，计数含之 | 与 command-picker 语义不一致；若 inventory+开关属有意设计，建议注释说明 |
| P3 | 全局（98 处 `: any`/`as any`，50 处在 .tsx） | API 响应弱类型集中在页面层 | api.ts 中 getTools/getConfig/getMemories 等返回 `any[]`；页面层透传。可渐进收窄 |
| P3 | tasks/files/cron/alerts/streams/memory/evolution 页 | 除 chat 外大列表未虚拟化 | 本地 CLI 场景量级可接受；memory 页搜索结果多时有风险 |
| P3 | lib/ws.ts:334 | `emitInternal` 用 `{ type: 'token' }` 假事件投递 _connected/_disconnected | 内部 hack，可定义独立内部事件类型 |
| P3 | App.tsx:241-254 等 | 确认弹窗/闹铃文案中英混排硬编码 | 未接入 i18n（`useT`）体系 |
| P3 | components/login-page.tsx:34 | 手写 fetch 而非复用 `api.login()` | 逻辑重复，两处需同步维护 |
| P3 | 组件层 | 无 React 组件测试 | 仅 lib 层 514 LOC 测试；store.test.ts 对 WS 事件处理覆盖较充分（含今日修复回归） |

## 亮点

- **WS 客户端工程质量高**：指数退避+20 次上限、1006 歧义码不误判为鉴权失败、health 探针区分 token 失效/服务宕机（gen 防竞态）、连接状态 100ms 防抖、`forCurrentSocket` 防止旧 socket 回调污染新连接
- **安全基线干净**：token 存 sessionStorage（含 localStorage 迁移清理）；文件访问 Bearer header + blob objectURL，token 不进 URL/query；全仓**无** `dangerouslySetInnerHTML`/`innerHTML`；markdown 经 react-markdown+remark-gfm（无 rehype-raw），外链 `rel="noopener noreferrer"`；`img` 本地路径统一走鉴权 serve 端点
- **聊天性能**：自研虚拟列表（chat-page + virtual-list.ts，含未测量视口时的初始 21 项切片）+ ResizeObserver 测高 + 自动滚动 50ms 防抖 + WS token delta 16ms 批处理合并
- **异步卫生**：WS 订阅、ResizeObserver、定时器、blob URL、窗口监听均成对清理；多页面异步加载有 agent/session 竞态守卫
- **事件隔离**：chat 类事件按 agent_id + normalizeSessionId 双维过滤，防跨会话/跨 agent 串台；`containsToolTraceContent` 清理伪 tool-call 文本

## 建议

1. （P1）在 App 挂载或 setup-wizard 步骤中调用 `requestNotificationPermission()`，否则删除通知功能避免误导
2. （P2）`tool_call_result` 改为从后向前全消息搜索 call_id；`tool_call_start` 空 call_id 时用随机 id 兜底
3. （P2）chat-page FileReader 补 `onerror/onabort`（reject）；files-page fetchFiles 补 currentPath 守卫
4. （P2）ws.ts 类型联合补 `evolution_deleted`（并加一条 store/页面级测试）
5. （P3）清理 notifyTaskCompleted 死代码；handleWsEvent 改用 useAgentStore.getState()；组件层引入至少一条 render/smoke 测试；渐进收窄 api.ts 的 any

## 维度小结

| 维度 | 结论 |
|---|---|
| ① 正确性 | 已检查：WS 重连/断线处理成熟；发现 tool_call_result 仅更新末条 assistant 消息（P2）、空 call_id（P2） |
| ② 异步 | 已检查：订阅/observer/timer/blob 清理完整；FileReader 缺 onerror（P2）、files 页缺 path 守卫（P2） |
| ③ 安全 | 已检查未发现 XSS/凭据泄露；唯一问题是通知权限从未申请导致功能空转（P1，非安全漏洞） |
| ④ 性能 | 已检查：chat 虚拟化+批处理良好；其余列表页未虚拟化（P3） |
| ⑤ 测试覆盖 | 已检查：lib 层 8 文件 514 LOC 覆盖核心逻辑；组件层无测试（P3） |
| ⑥ 可维护性 | 已检查：98 处 any（P3）、少量死代码/重复逻辑（P3）、类型联合缺 1 事件（P2） |
| ⑦ 契约一致性 | 已检查：WS 17 种类型 + 字段与后端逐一核对一致（含 HEAD 今日修复）；`evolution_deleted` 缺类型声明（P2） |

**P0：0 个 · P1：1 个 · P2：5 个 · P3：9 个**
