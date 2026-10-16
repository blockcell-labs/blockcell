# gateway HTTP 面 Review 报告（Wave 4 · 入口层）
> 只读审查 · 2026-10-16 · 子代理 w4-gateway-http

## 规模
- 审查范围：`bin/blockcell/src/commands/gateway/` 下除 `websocket.rs`、`streams.rs`、`webhooks.rs`、`chat.rs`、`alerts.rs` 外的 16 个文件 + 路由宿主 `commands/gateway.rs`（另一代理负责 WS/webhook/stream/chat/alerts）。
- 覆盖文件行数（`wc -l`）：

| 文件 | 行数 | 职责 |
|---|---|---|
| `gateway.rs`（含路由/鉴权中间件/state） | 2521 | router、auth middleware、GatewayState、启动编排 |
| `capabilities.rs` | 1666 | tools/skills/evolution/persona/pool/stats |
| `skills_install.rs` | 1409 | skill 删除 / hub 代理安装 / 外部 URL 安装 |
| `channels.rs` | 831 | 渠道列表/配置/owner 绑定 |
| `banner.rs` | 730 | 启动横幅（含安全提示） |
| `files.rs` | 698 | 文件 list/content/download/serve/upload |
| `config_api.rs` | 401 | config 读写/校验/test-provider/ghost |
| `cron.rs` | 397 | cron CRUD + 手动触发 |
| `sessions.rs` | 336 | 会话 list/get/delete/rename |
| `channel_ids.rs` | 347 | 渠道账号/agent 解析（纯函数） |
| `memory.rs` | 404 | memory list/create/delete/stats |
| `toggles.rs` | 191 | skill/tool 开关 |
| `util.rs` | 125 | secure_eq / 路径校验 / 进度节流 |
| `webui.rs` | 123 | 内嵌 WebUI 静态资源 + env.js |
| `outbound.rs` | 76 | outbound → channel 分发桥 |
| `cors.rs` | 33 | API/WebUI CORS 层 |
| 合计 | ~9.6K | |

- 方法：先读 `gateway.rs` 建立路由表，再深读安全关键文件（capabilities/files/config_api/cors/channels/skills_install/sessions/memory/toggles/util）。未运行 cargo/clippy/test。

## 模块地图（路由表 → handler → 下游）

路由在 `gateway.rs:1764-1918` 构建。`auth_middleware`（`gateway.rs:578-617`）与 `build_api_cors_layer`（`cors.rs:10`）以 `.layer()` 施加于 **其上** 注册的所有 `/v1/*` 路由；webhook 三条路由在 `.layer()` **之后** 注册，因此公开无鉴权（符合设计）。

鉴权豁免（`gateway.rs:589-596`）：`/v1/health`、`/v1/auth/login`、`/v1/ws`（WS 在升级 handler 内校验子协议 token）。token 为空时 `auth_middleware` 直接放行（`gateway.rs:583-586`），但启动时保证 api_token 非空（`gateway.rs:768-803,1697-1717`）。

| 方法 路径 | handler (文件:行) | 下游 |
|---|---|---|
| POST `/v1/auth/login` | `chat.rs:153`（范围外） | `LoginRateLimiter`、`secure_eq`、返回 api_token |
| POST `/v1/chat` | `chat.rs:209`（范围外） | inbound_tx |
| GET `/v1/health` | `chat.rs:265`（范围外，**公开**） | config（泄漏 active model/version） |
| GET `/v1/tasks` | `chat.rs:278`（范围外） | TaskManager |
| GET `/v1/ws` | `websocket.rs:188`（范围外） | WS 升级 |
| GET `/v1/sessions` | `sessions.rs:72` | SessionStore/spawn_blocking |
| GET/DELETE `/v1/sessions/:id` | `sessions.rs:181,244` | `resolve_session_key_from_id`（白名单解析） |
| PUT `/v1/sessions/:id/rename` | `sessions.rs:286` | `_meta.json` |
| GET/PUT `/v1/config` | `config_api.rs:22,54` | config.json5（**回显明文密钥**） |
| GET/PUT `/v1/config/raw` | `config_api.rs:27,75` | config.json5 原文 |
| POST `/v1/config/reload` | `config_api.rs:94` | 校验 |
| POST `/v1/config/test-provider` | `config_api.rs:116` | 任意 api_base 建 provider |
| GET/PUT `/v1/ghost/config` | `config_api.rs:211,221` | config |
| GET `/v1/ghost/activity` | `config_api.rs:260` | sessions_dir/spawn_blocking |
| GET `/v1/ghost/metrics` | `config_api.rs:372` | ghost_metrics |
| GET `/v1/ghost/model-options` | `config_api.rs:378` | provider 名列表 |
| GET/POST `/v1/memory` | `memory.rs:69,92` | memory store / `memory_upsert` tool |
| GET `/v1/memory/stats` / DELETE `/v1/memory/:id` | `memory.rs:126,109` | memory store |
| GET `/v1/tools` | `capabilities.rs:8` | tool_registry（visibility/group/order） |
| GET `/v1/skills` | `capabilities.rs:522` | skills_dir 递归扫描 |
| POST `/v1/skills/search` | `capabilities.rs:582` | 递归扫描 |
| DELETE `/v1/skills/:name` | `skills_install.rs:115` | `validate_installed_skill_name` |
| GET `/v1/hub/skills` | `skills_install.rs:134` | community hub 代理 |
| POST `/v1/hub/skills/:name/install` | `skills_install.rs:175` | hub 下载+事务安装 |
| POST `/v1/skills/install-external` | `skills_install.rs:927` | 外部 URL/zip/GitHub → staging → 进化 |
| GET `/v1/evolution`(+子路由) | `capabilities.rs:698…1534` | evolution_records / EvolutionService |
| GET `/v1/stats` | `capabilities.rs:1633` | task/memory 汇总 |
| GET `/v1/channels/status`,`/v1/channels` | `channels.rs:25,45` | ChannelManager / config（**回显明文密钥**） |
| PUT `/v1/channels/:id` | `channels.rs:339` | config 写入（**未校验 channel id**） |
| PUT `/v1/channels/:id/config` | `channels.rs:432` | config（校验 channel id） |
| GET/PUT/DELETE `/v1/channel-owners…` | `channels.rs:604-703` | config |
| GET `/v1/persona/files`,`/v1/persona/file` | `capabilities.rs:858,893,916` | workspace（白名单文件名） |
| GET `/v1/pool/status` | `capabilities.rs:816` | config |
| GET `/v1/cron`,`DELETE/run` | `cron.rs:7,168,249,266` | CronService |
| GET/PUT `/v1/toggles` | `toggles.rs:10,99` | toggles.json（原子写 + 全局锁） |
| GET `/v1/alerts*` | `alerts.rs`（范围外） | — |
| GET `/v1/streams*` | `streams.rs`（范围外） | — |
| GET `/v1/files`,`/content`,`/download`,`/serve` | `files.rs:139,263,380,431` | workspace/media_dir（canonicalize 包含校验） |
| POST `/v1/files/upload` | `files.rs:518`（DefaultBodyLimit 16MB） | workspace（路径校验 + symlink 检查） |
| POST `/webhook/*` | `webhooks.rs`（范围外，公开） | — |

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| P1 | `capabilities.rs:758, 769, 1095, 1198, 1234, 1466-1470, 1502-1506` | **路径穿越：evolution 系列 handler 直接用路径参数拼文件名** | `handle_evolution_detail/stop/delete/versions/tool_versions` 取 `AxumPath<String>` 后 `records_dir.join(format!("{}.json", evolution_id))` / `skills_dir().join(&skill_name).join("version_history.json")`，无 `..`/分隔符校验。axum 0.7 的 `Path` 提取器对捕获段做百分号解码，`..%2F..%2Ffoo` 解码为 `../../foo`（`/` 不参与路由切分），`join` 后即 `records_dir/../../foo.json`。`handle_evolution_delete` 会 `tokio::fs::remove_file`（`capabilities.rs:1211,1236`）→ **任意 `.json` 文件删除/读取**。需鉴权，但 WebUI 登录即下发同一 api_token（`chat.rs:190-193`），故持有 WebUI 口令者即可利用。 |
| P1 | `channels.rs:67,85,103-104,124-126,144,163-166,202-204,222,242,261`；`config_api.rs:22-24` | **敏感凭据明文回显** | `GET /v1/channels` 在 `fields[].value` 返回 telegram token、discord/slack botToken、feishu/lark appSecret+encryptKey+verificationToken、wecom corpSecret+callbackToken+encodingAesKey、napcat accessToken、weixin token 等**明文**（仅标记 `"secret": true` 供前端遮罩，服务端不脱敏）。`GET /v1/config` 注释即写明 “returns plaintext API keys”，且 `load_config_value_or_state`→`parse_json5_value`（`config.rs:331-333`）会**展开 `${ENV}` 环境变量**，把密钥解析为明文返回。均需 api_token，但令牌即全权凭据，泄漏面大。 |
| P2 | `cors.rs:12-14` | **默认宽松 CORS** | `allowed_origins` 为空（默认，`core/config/gateway.rs:55`）时用 `CorsLayer::permissive()`，允许任意 Origin。`gateway.host` 默认 `localhost` 尚可，但用户为公网暴露改为 `0.0.0.0` 时若未配置 `allowed_origins` 仍全放开；`docs/08_gateway_mode.md` 未提及该配置项（契约缺口）。因鉴权走 Authorization 头而非 Cookie（`allow_credentials(false)`，`cors.rs:14,22`），直接利用受限，属纵深防御缺陷。 |
| P2 | `capabilities.rs:76,125,195,246,332,702,788,1544,1571,1596,1603`；`config_api.rs:9,27`；`sessions.rs:23-44` | **async handler 内做阻塞 `std::fs`** | `collect_skill_entries*`/`handle_evolution*`/`handle_evolution_summary` 用 `std::fs::read_dir`+`std::fs::read_to_string`（同步）直接在 async 上下文执行；`handle_config_get` 经 `load_config_value_or_state` 同步 `std::fs::read_to_string`（`config_api.rs:9`）；`handle_files_list` 同样先 `canonicalize`/`exists` 同步。大量技能目录扫描时会阻塞 tokio worker。`files.rs`/`sessions.rs` 已用 `spawn_blocking` 或 tokio::fs，标准不统一。 |
| P2 | `files.rs:155-169` | **`handle_files_list` canonicalize 失败分支的包含校验退化为词法比较** | canonicalize 失败但 `target.exists()` 为真时用未规范化的 `target.clone()`，再 `!canonical.starts_with(&ws_canonical)`；`Path::starts_with` 按组件前缀匹配，`ws/foo/../../etc` 会因首组件 `ws` 命中而通过。触发需 “canonicalize 失败且 exists 为真” 的窄窗口（权限/竞态），其余 handler 无此退化分支。 |
| P2 | `capabilities.rs:1328` | **HTTP 请求内同步跑完整 agent turn，无请求超时** | `handle_evolution_test` 在 handler 内 `runtime.process_message(inbound).await`，一次技能测试可能持续数分钟；`axum::serve`（`gateway.rs:1926`）未加 `TimeoutLayer`，长请求长期占用连接/worker，构成可用性风险。 |
| P2 | `capabilities.rs:951-982` | **`handle_evolution_trigger` 绕过 toggles 写锁** | 该处直接 `tokio::fs::read_to_string`+`write` 写 `toggles_file`，未获取 `toggles.rs:6` 的 `TOGGLES_WRITE_LOCK`，且写入语义（`store["skills"][name]=false`）与 `toggles.rs:35-47` 不一致；与 `PUT /v1/toggles` 并发时存在读改写竞争/丢失更新。 |
| P2 | 全目录测试 | **无 router 级集成测试** | 目录内无 `Router::new()`/`tower::ServiceExt::oneshot` 用法；测试均为纯函数单测（`gateway.rs:2086`、`files.rs:618`、`channels.rs:705`、`toggles.rs:133`、`cron.rs:297`、`memory.rs:141`、`skills_install.rs:4`）。鉴权中间件、CORS 生效范围、路由接线、错误映射均无端到端覆盖。 |
| P2 | `channels.rs:339-361` | **`PUT /v1/channels/:id` 未校验 channel id** | 与 `handle_channel_config_update`（`channels.rs:381` 校验 `SUPPORTED_OWNER_CHANNELS`）不一致：`apply_channel_fields` 对任意 `channel_id` 执行 `channels.entry(channel_id).or_insert(...)`，可在 config 注入任意渠道键。仅 JSON 键，无路径穿越，但破坏配置一致性。 |
| P3 | `chat.rs:265-276`（范围外文件，HTTP 面） | `/v1/health` 公开泄漏 active model 与版本号 | 无需鉴权即可枚举模型名与 `CARGO_PKG_VERSION`，轻度信息暴露。 |
| P3 | `config_api.rs:168-196` | `test-provider` 接受任意 `api_base` | 用请求体 `api_base` 建 `OpenAIProvider` 并发起出站请求，形成受限 SSRF（需鉴权，且持有 token 者已有配置写权限，风险较低）。 |
| P3 | `files.rs:509` | `/v1/files/serve` 对媒体文件返回 `Cache-Control: public` | 共享代理/浏览器可能缓存，含敏感媒体时欠妥；可改 `private`。 |
| P3 | `files.rs:407-416` | `Content-Disposition` 文件名未转义 | `format!("attachment; filename=\"{}\"", filename)`；文件名含 `"`/CRLF（可经 upload 构造）会使 `HeaderValue` 构造失败→500，非注入但稳健性差。 |
| P3 | `config_api.rs:61-71,75-91,221-257`；`channels.rs:357-360` 等 | 错误以 HTTP 200 + `{"status":"error"}` 返回 | 与 `files.rs`/`sessions.rs` 用真实状态码的风格不一致，客户端需解析 body 判断成败，契约不统一。 |
| P3 | `skills_install.rs:796,896` | GitHub 递归中每个 URL 都重新 DNS 解析并重建 `reqwest::Client` | 每文件一次 `build_external_client`，重复解析/建连开销；可复用 client。 |
| P3 | `cron.rs:212-219` | `handle_cron_create` 不校验 `skill_name` | `skill_name` 原样写入 job，`resolve_cron_skill_payload_kind` 用它拼 `skills_dir().join(skill_name)` 做存在性探测（`cron.rs:58-59`），可含 `../`；仅探测，无读写越界，但应白名单化。 |
| P3 | `capabilities.rs:945` | `handle_evolution_trigger` 在长 await 期间持有 `evolution_service` 互斥锁 | `let evo = state.evolution_service.lock().await;` 跨 `trigger_manual_evolution(...).await` 持有，可能阻塞其他 evolution 端点。 |

## 亮点
- **`skills_install.rs` 供应链防护扎实**：下载 URL 强制同源（`resolve_hub_download_url` `:615-635`，含 scheme/port 比较）、`redirect(Policy::none())`、IP 黑名单含 IPv4-mapped IPv6（`is_blocked_ip` `:504-524`）、`resolve_to_addrs` 固定解析结果（缓解 DNS rebinding，`:564-573`）、zip 用 `enclosed_name()` + 归档条目/解压字节预算（`extract_hub_package` `:350-405`）、事务式安装（staging→backup→rename，`:407-456`）、技能名严格单组件校验（`validate_installed_skill_name` `:602-613`）；配套单测覆盖逃逸/预算/流上限。
- **`files.rs` 路径边界**：所有读写前 `canonicalize()` + `starts_with(workspace/media_dir)`（`:155-169,275-284,391-400,454-460`），上传叠加 `validate_workspace_relative_path`（`util.rs:75-100`，拒 `..`/绝对路径）与 `resolve_for_policy`（`path_policy.rs:317-342`，解析 symlink），并有 symlink 逃逸回归测试（`files.rs:622-648`）；响应字节上限齐全。
- **`util.rs:64-73` `secure_eq`** 常量时间比较，用于 API token 与登录口令（`gateway.rs:604`、`chat.rs:172`）。
- **`outbound.rs:43-75`** 有专门测试确保非 ws 渠道消息不泄漏进 WS 广播，注释亦点明意图。
- **鉴权/路由布局正确**：webhook 有意置于 auth layer 之外；`/v1/ws` 交由子协议校验；api_token 缺失时自动生成并持久化（`gateway.rs:768-803`），暴露到 `0.0.0.0` 时横幅打印安全告警（`banner.rs:157`）。
- **`toggles.rs`** 原子写（temp+fsync+rename）+ 全局写锁 + 单测。
- **`sessions.rs`** 通过 `resolve_session_key_from_id` 以真实文件 stem 白名单解析，天然免疫 `session_id` 穿越。
- **`memory.rs`/`channels.rs`/`cron.rs`** 单测质量较好，`memory.rs:319-378` 验证 provenance 字段（source/channel/session_key）被服务端覆盖，防伪造。

## 建议
1. **统一路径参数净化**（P1）：为 `AxumPath` 取到的 id/name 增加单组件校验（复用 `validate_installed_skill_name` 或新增 `validate_single_component`），或改用 `PathBuf` 拼接后 `resolve_for_policy` + 目录包含校验；优先修 `handle_evolution_delete/stop`。
2. **凭据脱敏**（P1）：`GET /v1/config`、`GET /v1/channels` 对 `*token*`/`*secret*`/`api_key`/`api_token` 等字段做掩码（如仅回显末 4 位），或新增 `/v1/config/raw` 之外的 `?reveal=` 显式开关；`config_api` 改用 `parse_json5_value_preserving_env` 以免回显展开后的密钥。
3. **CORS 收紧**（P2）：`allowed_origins` 为空且 host 非 loopback 时，至少回退为 “同源 + localhost 系列”，并在 `docs/08` 补充 `allowedOrigins` 说明。
4. **阻塞调用移出 async**（P2）：把 `collect_skill_entries*`、`handle_evolution*`、`handle_ghost_activity` 的目录遍历统一放入 `spawn_blocking`（参照 `sessions.rs:85`）。
5. **补 router 集成测试**（P2）：用 `Router` + `oneshot` 覆盖：无 token 401、`/v1/health` 免鉴权、CORS 预检、webhook 免鉴权、`/v1/config` 脱敏、路径参数穿越被拒。
6. **加请求超时层**（P2）：对 `handle_evolution_test` 等长任务设 `TimeoutLayer` 或改为异步任务+轮询，避免连接长期占用。
7. **清理次要一致性问题**（P3）：`PUT /v1/channels/:id` 补 channel id 校验；`handle_evolution_trigger` 复用 `TOGGLES_WRITE_LOCK` 与统一写语义；错误响应统一状态码；`Content-Disposition` 文件名转义；`/v1/files/serve` 改 `Cache-Control: private`。

### 各维度结论
- ① 正确性：路由/handler 边界清晰；主要问题是错误码风格不统一（P3）与 evolution 路径参数未净化（P1）。
- ② 并发：`config_write_lock` 覆盖所有 config 读改写；`toggles` 有独立锁；**唯一致命并发缺陷**是 `handle_evolution_trigger` 绕过 toggles 锁（P2）。长请求无超时（P2）。
- ③ 安全：鉴权覆盖所有 `/v1/*` 且常量时间比较；CORS 默认宽松（P2）；files 路径穿越防护到位；**evolution 路径参数穿越（P1）与凭据明文回显（P1）为主要风险**；request 体有默认 2MB、上传 16MB 限制；供应链安装防护优秀。
- ④ 性能：多处 async 内阻塞 `std::fs`（P2）；大 body 有上限。
- ⑤ 测试：纯函数单测较好，缺 router 级集成测试（P2）。
- ⑥ 可维护性：模块拆分清晰（cors/util/channel_ids 抽离），错误处理风格需统一。
- ⑦ 契约一致性：`/v1/tools` 返回 visibility/group/order 与 webui 预期一致；鉴权/token 传递与 `docs/08_gateway_mode.md` 一致；`docs/08` 未覆盖 `allowedOrigins`（P2 缺口）。
