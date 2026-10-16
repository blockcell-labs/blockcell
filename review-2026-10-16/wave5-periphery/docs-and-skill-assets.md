# 文档与 skills 资产 Review 报告（Wave 5 · 周边）

> 只读审查 · 2026-10-16 · 子代理 w5-docs
> 方法：每篇抽取 3-5 个可验证声称（命令名 / 配置项 / 字段名 / 路径 / 默认值），到代码中 grep 验证。未运行 cargo/clippy/test。

## A. 文档一致性

| 文档 | 验证的声称 | 结论 | 证据（文件:行） |
|------|-----------|------|----------------|
| 03_tools_system | `blockcell tools` 命令存在 | 一致 | bin/blockcell/src/commands/tools_cmd.rs；main.rs clap 注册 |
| 03_tools_system | 内置工具 50+（示例输出 52） | 一致 | crates/tools/src/registry.rs 共 58 处 `registry.register`（+registry_builder.rs 2 处 ExecTool 变体 + 动态 MCP 工具），数量级吻合；示例数字为示意值（P3） |
| 03_tools_system | exec 工具默认超时 30 秒 | **漂移** | crates/core/src/config/tools.rs:58-60 `default_exec_timeout() -> 60`（默认 60s）；crates/tools/src/exec.rs:225 读取 `config.tools.exec.timeout` |
| 03_tools_system | web_search 后端 DuckDuckGo，无需 API Key | **漂移（P1）** | 全仓库无 duckduckgo；crates/tools/src/web.rs:85-155 实际为 Brave API + 百度 API，无 key 时直接报错（"No search API configured..."）。文档宣称"无需 API Key"与实际完全相反，会误导用户 |
| 03_tools_system | Gateway 模式工作目录外一律拒绝 | 一致（间接） | crates/core/src/path_policy.rs:205-243 evaluate()；confirm 动作在无交互渠道由上层运行时处理；bin gateway 无交互确认路径 |
| 03_tools_system | alert_rule 持久化 workspace/alerts/rules.json | 一致 | crates/tools/src/alert_rule.rs:12,108 |
| 03_tools_system | 工具分类（fs/web/exec/browser/finance/office/记忆/调度/多 agent）工具名清单 | 一致 | 各工具文件定义（fs.rs、web.rs、exec.rs、browser/tool.rs、cron.rs、spawn.rs、agent.rs、tasks.rs、system_info.rs 等），名称逐一存在 |
| 04_skill_system | 运行时契约 meta.yaml + SKILL.md | 一致 | crates/skills/src/manager.rs 识别逻辑（meta.yaml/meta.json 优先，OpenClaw frontmatter 兼容） |
| 04_skill_system | meta.yaml 字段 name/description/tools/requires/permissions/fallback | 一致 | crates/skills/src/manager.rs `SkillMeta` 结构体；BLOCKCELL.md:11-14 |
| 04_skill_system | 禁止字段 triggers/capabilities/always/output_format | 一致（契约层面） | SkillMeta 保留 always/capabilities/output_format 为 legacy 兼容字段（manager.rs），无 triggers 字段；契约检查在 crates/skills/src/evolution/compile_deploy.rs |
| 04_skill_system | SKILL.md 必须含 `## Shared {#shared}` / `## Prompt {#prompt}` | 一致 | crates/skills/src/evolution/compile_deploy.rs 明确检查两 section（契约检查阶段） |
| 04_skill_system | exec_local runner 仅 python3/bash/sh/node/php | 轻微漂移（P3） | crates/tools/src/exec_local.rs 允许列表实际含 `python3, python, bash, sh, node, php, uv`（多了 python、uv） |
| 04_skill_system | activate_skill 合成工具、forced_skill_name 旁路 | 一致 | crates/agent/src/runtime/process_message_inner.rs `build_activate_skill_tool_schema`；runtime skill_session 处理 forced_skill_name |
| 04_skill_system | openclawSkillEnabled 配置项 | 一致 | crates/core/src/config.rs `openclaw_skill_enabled`（默认 false） |
| 04_skill_system | WebUI 端点 GET /v1/hub/skills、POST /v1/hub/skills/:name/install、POST /v1/skills/install-external | 一致 | bin/blockcell/src/commands/gateway/skills_install.rs；gateway.rs 路由注册 |
| 04_skill_system | 外部导入限制 5MB / 200 文件 | 一致 | bin/blockcell/src/commands/gateway/skills_install.rs:286-287 `EXTERNAL_MAX_DOWNLOAD_BYTES = 5MB`、`EXTERNAL_MAX_FILES = 200` |
| 04_skill_system | community_hub 工具（trending/search_skills/install_skill 等） | 一致 | crates/tools/src/community_hub.rs |
| 05_memory_system | `blockcell memory list/search/show/delete/clear --scope short_term/stats/maintenance --recycle-days 30/migrate-canonical` | 一致 | bin/blockcell/src/main.rs:1173-1183；commands/memory.rs |
| 05_memory_system | memory_upsert 仅接受 short_term，拒绝 long_term 并提示改用 memory_manage | 一致 | crates/tools/src/memory.rs（schema enum ["short_term"] + 运行期校验） |
| 05_memory_system | knowledge_forget 两阶段 preview/confirm + preview_token | 一致 | crates/tools/src/knowledge_forget.rs |
| 05_memory_system | knowledge_fts/memory_fts 使用 trigram tokenizer | 一致 | crates/storage/src/knowledge_index.rs:375 `tokenize='trigram'`；crates/storage/src/memory.rs FTS5 |
| 05_memory_system | knowledge_index.db / memory.db / ghost_ledger.db 文件布局 | 一致 | crates/core/src/paths.rs:276-277；crates/storage/src/knowledge_index.rs、ghost_ledger.rs |
| 05_memory_system | USER.md/MEMORY.md 为长期事实源、memory_manage 写入 | 一致 | crates/tools/src/memory.rs MemoryManageTool；05 文档描述的规范化/快照/原子写机制与 skills/evolution 中 knowledge 写入路径一致 |
| 08_gateway_mode | 默认端口 18790(API)/18791(WebUI) | 一致 | crates/core/src/config/gateway.rs `default_webui_port() -> 18791`、18790 |
| 08_gateway_mode | POST /v1/chat 异步返回 202 Accepted | 一致 | bin/.../gateway/chat.rs `StatusCode::ACCEPTED`；gateway.rs 路由 |
| 08_gateway_mode | WebSocket 子协议 blockcell-auth.<hex> 鉴权 | 一致 | bin/.../gateway/websocket.rs `strip_prefix("blockcell-auth.")` |
| 08_gateway_mode | gateway.apiToken 空则自动生成并写回 config.json5 | 一致 | bin/.../gateway.rs "Auto-generate and persist api_token if not configured or empty" |
| 08_gateway_mode | 斜杠命令表（/help /tasks /skills /tools /learn /clear /compact /session-metrics /log） | 一致 | bin/.../slash_commands/handlers/（session_metrics.rs 等）；/quit /exit 仅 CLI |
| 08_gateway_mode | GET /v1/health 无需认证 | 一致 | gateway.rs:34 `path == "/v1/health"` 跳过鉴权 |
| 08_gateway_mode | channelAccountOwners/channelOwners 路由 | 一致 | bin/.../gateway/ 消息路由 + docs/12 多 agent 池描述一致（config.rs 相关字段） |
| 09_self_evolution | 6 阶段 Triggered→Generating→Auditing→Compiling→Testing→RollingOut | **过时（P1）** | crates/skills/src/evolution.rs `EvolutionStatus`：当前为 Triggered→Generating→Generated→Auditing→(AuditPassed/Failed)→Compile(Passed/Failed)→Observing→Completed；Testing/RollingOut 仅为"旧记录反序列化兼容"保留的 legacy 变体，不再是活动状态 |
| 09_self_evolution | 灰度发布 10%→50%→100% 比例放量 | **过时（P1）** | 当前为单次部署 + 观察窗口：crates/skills/src/service.rs "Step 4: Deploy and observe" `deploy_and_observe()`；无百分比灰度逻辑 |
| 09_self_evolution | "只能修改 Rhai 脚本，不能改 Rust 核心代码" | **过时（P1）** | 当前自进化覆盖 SKILL.md（prompt-only）、Python、Rhai 三形态：crates/skills/src/evolution/compile.rs 提取 ```rhai/```python/```markdown 块；compile_deploy.rs 契约检查 SKILL.md |
| 09_self_evolution | 审计失败重试循环最多 3 次 | 一致 | crates/skills/src/core_evolution.rs `max_retries: 3` / `max_attempts: 3`（skill evolution 重试路径同量级） |
| 09_self_evolution | `blockcell evolve list` / `blockcell evolve rollback <skill>` | 一致 | bin/blockcell/src/main.rs `EvolveCommands::List` / `Rollback { skill_name, to }` |
| 09_self_evolution | tool_versions/vN_*.rhai 版本目录 | 部分一致 | versioning 模块存在（crates/skills/src/evolution/versioning.rs），但演进产物形态已不止 .rhai（含 SKILL.md/SKILL.py），目录示例仅示意（P3） |
| 09_self_evolution | 固定流程 6 步（补丁→静态审计→LLM 审计→编译检查→契约检查→部署观察） | 一致 | service.rs pipeline 注释 + compile_deploy.rs 契约检查；BLOCKCELL.md:77-84 |
| 12_architecture | workspace 9 个 crate + bin（依赖图 bin→agent→tools/skills/storage/providers→core） | 一致 | Cargo.toml:3-14 members；各 Cargo.toml 依赖（agent 依赖 tools/skills/storage/providers/core；skills 仅依赖 core）。tools→skills 额外依赖未画进图（P3） |
| 12_architecture | 存储层 SQLite + RabitQ 向量 | 一致 | Cargo.toml:128-132 rusqlite/rabitq_rs |
| 18_proxy_and_provider_config | 16 个内置 provider 默认 apiBase（表） | 一致 | crates/core/src/config.rs:125-220 预置表逐一吻合（含 glm→api.z.ai/v1、vllm→localhost:8000/v1、gemini→generativelanguage.../v1beta/openai）；providers/factory.rs 为回退默认（glm 回退 bigmodel，仅无配置时生效，不冲突） |
| 18_proxy_and_provider_config | provider 级 proxy 优先级高于全局 network.proxy，"" 强制直连 | 一致（间接） | ProviderConfig.proxy 字段（config.rs）；providers client 构建时按 provider→global 顺序解析 |
| 18_proxy_and_provider_config | `blockcell onboard` 生成配置、`blockcell config edit` | 一致 | bin/.../commands/onboard.rs、config_cmd.rs |
| 19_mcp_servers | 独立 MCP 层：~/.blockcell/mcp.json + mcp.d/*.json（严格 JSON） | 一致 | crates/core/src/paths.rs:154 `base.join("mcp.json")`；mcp_config.rs 读取 mcp_dir；core/mcp_config.rs 无 JSON5 解析 |
| 19_mcp_servers | defaults.startupTimeoutSecs=20 / callTimeoutSecs=60 / autoStart | 一致 | crates/core/src/mcp_config.rs `default_startup_timeout_secs() -> 20`、`default_call_timeout_secs() -> 60` |
| 19_mcp_servers | agent 通过 allowedMcpServers / allowedMcpTools 绑定 | 一致 | crates/core/src/config/agent.rs:58,61,221-223 |
| 19_mcp_servers | CLI：`blockcell mcp add github --db-path` / list | 一致 | bin/.../main.rs `McpCommands::{List,Show,Add,Remove,Enable,Disable,Edit}`；commands/mcp.rs |
| 20_provider_pool | modelPool 字段 model/provider/weight/priority（小数字=高优先） | 一致 | crates/providers/src/pool.rs `ModelEntry`（config.rs）；pool.rs 选取逻辑 |
| 20_provider_pool | 熔断：连续 3 次失败冷却 60s；401/403 永久 Dead；429 立即冷却 | 一致 | crates/providers/src/pool.rs `fail_threshold: 3`、`cooldown: Duration::from_secs(60)`；AuthError→Dead 注释与实现 |
| 22_path_access_policy | path_access.json5 规则文件、首次启动自动生成模板 | 一致 | crates/core/src/path_policy.rs `default_policy_template()`（含 deny-secrets/confirm-home-exec，与文档示例一致） |
| 22_path_access_policy | 优先级：内置敏感路径 > deny > allow(更长前缀) > confirm > default_policy(confirm) | 一致 | path_policy.rs:205-243 evaluate()；`builtin_sensitive_paths()` 14 条路径与文档列表逐条一致 |
| 22_path_access_policy | ops 映射（read/write/list/exec，write 含 file_ops/data_process/office_write/health_api/encrypt 等） | 一致 | path_policy.rs:25-37 `PathOp::from_tool_name` |
| 22_path_access_policy | security.pathAccess.enabled/policyFile/missingFilePolicy | 一致 | crates/core/src/config/tools.rs `PathAccessConfig` |
| 29_hook_system | hooks.yaml 位置、version/hooks/event/matcher/command/timeout | 一致 | crates/agent/src/hooks.rs（HookConfig/HookManager::load，文件缺失用空配置） |
| 29_hook_system | 事件 pre_tool_use/post_tool_use/session_start/user_prompt/agent_stop；session_end/compaction 预留未触发 | 一致 | hooks.rs `HookEvent` 枚举（7 变体）；文档明确声明后两者"尚未提供统一触发点" |
| 29_hook_system | timeout 默认 30.0 秒；{result} 截断 1000 字符；shell-safe quoting | 一致 | hooks.rs:53-54 `default_hook_timeout() -> 30.0`；:197-198 `truncate_chars(result, 1000)` + `shell_quote` |

## B. skills 资产合规

| skill | 合规问题 | 证据 |
|-------|---------|------|
| ai_news | 无问题：meta.yaml(name/description/tools/permissions/fallback 齐全) + SKILL.md 含 Shared/Prompt | skills/ai_news/meta.yaml；SKILL.md:3,10 |
| shell | 无问题：结构齐全；SKILL.md 安全规则明确（危险动作需用户明确确认） | skills/shell/meta.yaml；SKILL.md:3,9 |
| camera | 无问题：结构齐全，permissions 声明 hardware.camera/fs.write/system.exec | skills/camera/meta.yaml；SKILL.md:3,9 |
| app_control | 无问题：结构齐全；tests/*.json 均可解析 | skills/app_control/meta.yaml；tests/list_apps.json、screenshot_windsurf.json（JSON parse OK） |
| weather | 无问题：结构齐全（仅 tools+fallback，可选字段按需） | skills/weather/meta.yaml；SKILL.md:3,11 |
| tencent-news | 无问题：结构齐全 | skills/tencent-news/meta.yaml；SKILL.md:3,13 |
| deep_analysis | 无问题：仅 name/description（最小合法 skill） | skills/deep_analysis/meta.yaml；SKILL.md:3,13 |
| bcmail | 无问题：Python 用 stdlib(imaplib/smtplib)，全程 try/except + sys.exit(1) 错误处理；无 os.remove/shutil.rmtree/未校验 subprocess；凭据走环境变量 | skills/bcmail/bcmail.py:113-171（_ensure_auth/_connect_imap/_connect_smtp 错误处理）；meta.yaml requires.env BCMAIL_EMAIL/PASSWORD |
| wechat_binance | 无问题：符合 BLOCKCELL.md 错误处理模板（try/except→stderr JSON→exit 1）；HTTP 调用检查 status_code != 200；subprocess 用列表参数 + timeout=30 + 固定 argv，无 shell=True；联系人/消息经 escape_applescript 转义后注入 AppleScript 字符串 | skills/wechat_binance/SKILL.py:16-25,133-134,219-224 |
| （全局） | 9/9 skill 均有 meta.yaml + SKILL.md；9/9 SKILL.md 均含 `## Shared {#shared}` 与 `## Prompt {#prompt}`；无任何 skill 使用禁用遗留路由字段 triggers/capabilities/always/output_format；9 个 meta.yaml 无 tab、无重复顶层键（python3 校验）；6 个 tests/*.json 全部 JSON 可解析 | skills/*/；BLOCKCELL.md:11-15 |
| （全局） | 4 个 SKILL.rhai 资产（ai_news/shell/camera/app_control）无 SKILL.md 引用——符合 04 文档"Rhai 非普通聊天默认入口"的定位，非违规 | docs/04_skill_system.md:56-57,106-113 |

## 问题清单

| 级别 | 位置 | 问题 | 证据/说明 |
|------|------|------|-----------|
| P0 | — | 无。未发现安全/契约被破坏的声称 | skills 资产契约 100% 合规；path_policy/hook/契约检查均有真实实现 |
| P1 | docs/03_tools_system.md:85 | web_search 宣称后端为"DuckDuckGo（无需 API Key）"，实际为 Brave+百度双 API 且无 key 直接报错，方向性误导 | crates/tools/src/web.rs:85-155；全仓 grep 无 duckduckgo |
| P1 | docs/09_self_evolution.md:39-45,101-115 | 进化阶段名（Testing/RollingOut）与"10%→50%→100% 灰度发布"均基于 legacy 变体，当前实现为 Compile→Observing 单次部署+观察窗口 | crates/skills/src/evolution.rs EvolutionStatus（legacy 注释）；service.rs "Step 4: Deploy and observe" |
| P1 | docs/09_self_evolution.md:198 | "只能修改 Rhai 脚本"过时：当前进化覆盖 SKILL.md/Python/Rhai 三形态并分别校验 | crates/skills/src/evolution/compile.rs；compile_deploy.rs |
| P2 | docs/03_tools_system.md:124 | exec 默认超时写 30 秒，实际默认 60 秒 | crates/core/src/config/tools.rs:58-60 |
| P2 | docs/03_tools_system.md:365 | 工具数量示例 "(52)" 与实际静态注册 58+ 动态 MCP 不符（示意值，易被当实测） | crates/tools/src/registry.rs |
| P2 | docs/12_architecture.md:119-125 | 依赖图未画 tools→skills（agent 之外的 tools 依赖），读者可能误判工具层不含技能层 | crates/tools/Cargo.toml |
| P2 | docs/04_skill_system.md:88 | exec_local runner 清单漏 python、uv（文档称"只允许 5 种"） | crates/tools/src/exec_local.rs:7 |
| P3 | docs/09_self_evolution.md:158-162 | tool_versions 版本目录示例仅列 .rhai，未体现 SKILL.md/SKILL.py 产物 | crates/skills/src/evolution/versioning.rs |
| P3 | docs/04_skill_system.md:129 | "包含 SKILL.md、meta.yaml 或 meta.json 任意一个标识文件即可被识别"略欠精确：SKILL.md 单独存在仅当以 `---` frontmatter 开头才按 OpenClaw 识别 | crates/skills/src/manager.rs 识别逻辑 |
| P3 | docs/22_path_access_policy.md:208 | "不支持热重载，reload_on_change 字段预留"——配置结构体中并无该字段，属预留说明而非漂移（可接受） | crates/core/src/config/tools.rs PathAccessConfig |

## 亮点

- **skills/ 资产契约执行到位**：9/9 meta.yaml 必填字段齐全、0 个使用禁用遗留字段；9/9 SKILL.md 严格包含 Shared/Prompt 锚点区块；契约不只是文档——`compile_deploy.rs` 在进化部署前做机器校验。
- **Python skill 安全实践标杆**：wechat_binance/SKILL.py 完整实现 BLOCKCELL.md 的错误处理模板（stdout display_text / stderr error JSON / exit 1）、非 200 检查、subprocess 列表参数+超时+固定 argv、AppleScript 注入转义；bcmail.py 凭据全部走环境变量。
- **文档 05/08/18/19/20/22/29 与代码高度一致**：端口、默认超时（MCP 20/60、hook 30s、pool 熔断 3 次/60s）、字段名（allowedMcpServers、modelPool、path_access.json5）、文件路径（mcp.json/mcp.d、knowledge_index.db）逐项对得上，说明这批文档确实"基于真实代码整理"。
- **契约检查与资产合规形成闭环**：BLOCKCELL.md 声明 → 代码校验（compile_deploy.rs）→ 资产实际达标，三者一致。

## 建议

1. **优先修 03 的 web_search 段落**（P1）：改为"Brave Search API + 百度 API 双后端，需配置 `tools.web.search.apiKey` 或 `baiduApiKey`/`BAIDU_API_KEY`；无 key 时报错"，删除"DuckDuckGo/无需 API Key"表述，避免用户按文档配置后工具直接失败。
2. **重写 09 的进化流水线与发布段落**（P1 ×3）：阶段名改为 Triggered→Generating→Auditing→Compiling→Observing→Completed/RolledBack；把"灰度 10/50/100"改为"部署 + 观察窗口，错误率高于旧版自动回滚"；"只能改 Rhai"改为"可进化 SKILL.md / SKILL.py / SKILL.rhai 三类资产，分别走提示词审计 / py_compile / Rhai 编译检查"。
3. **03 exec 超时改 60 秒**（P2），并把工具数量示例改为"50+"不给具体数字或标注"示例，实际随 MCP 加载变化"。
4. **12 依赖图补 tools→skills 边**（P2）；**04 exec_local runner 清单补 python/uv**（P2）。
5. **09 tool_versions 示例补充 SKILL.md/SKILL.py 产物形态**（P3），避免用户以为进化只产 rhai 文件。
6. 后续可考虑对 docs/ 系列增加一个"验证标记"（如每篇标注最后核对 commit/日期），便于快速定位漂移篇目。
