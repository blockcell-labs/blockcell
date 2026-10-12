# 更新日志

所有值得注意的变更都会记录在此文件中。

格式基于 [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)，
并遵循 [语义化版本](https://semver.org/spec/v2.0.0.html)。

## [Unreleased]

本批次为 Agent Runtime 架构演进：吸收新一代 Agent Runtime 的可组合、可观察、可拦截、可回放设计，同时保留 Self-Evolution / Ghost Learning 能力。

### 新增

- **Runtime Kernel**：新增事件总线（`EventBus`）、串行拦截链（`Waterfall`）与统一作用域原语（`ScopedRegistry` / `ScopeGuard`），事件按优先级、作用域过滤派发，listener 支持超时与故障隔离（fail-open / fail-closed）。
- **Agent 生命周期事件**：新增 `agent/pre-step`、`agent/request`、`agent/request-error`、`turn/start`、`turn/end`、`step/start`、`step/end`、`tool/pre-execute`、`tool/post-execute`、`tool/result`、`model/response`、`assistant/attempt`、`memory/write`、`subagent/spawn`、`subagent/completed`、`evolution/deployed`、`compaction/start`、`compaction/end`，并按 Durable / Live / Capability 分类一致性语义。
- **Guarded Tool Pipeline**：把工具开关、Policy、危险操作、路径访问、Shell 提权等安全检查集中到统一管线，具体工具不再自行处理权限、审批与审计。
- **失败语义**：新增 `FailureKind`、`Idempotency` 与 `ToolOutcome`，区分失败 / 被拒绝 / 取消 / 超时 / 未开始 / 未知；结果未知时不自动重试而是先对账；模型请求错误按类型决定恢复策略（上下文超限先压缩再重试、限流退避、认证失败换 provider）。
- **Session Event Log**：新增追加式会话事件日志（JSONL 侧车、`schema_version`、单调 `seq`、保留期清理），transcript 仍为主存储、事件日志渐进并行。
- **会话回放与评估**：新增 `blockcell session list / replay / verify`，从事件日志重放回合、步骤、模型请求与工具调用；新增 `SessionMetrics` 计算回合 / 步骤 / 工具结论 / 重试 / token / 时长等评估指标。
- **PromptRegistry**：系统提示词改为可插拔 `PromptSection` 注册表拼装，支持作用域覆盖、模板变量解析与完整性校验；新增 `PromptContribution` 与信任级别，学习技能与候选行为以低信任边界注入。
- **Agent Loop 契约**：`AgentLoop` 与默认 Loop 实现分离，一次 turn 的驱动方式可替换。
- **能力 seam**：新增 `SandboxPolicy` / `SandboxProvider`（本机 macOS Seatbelt / Linux Landlock+seccomp，不可用时要求审批）、`ShellExecutor`、`FilesystemBackend`（`LocalFilesystem`）、`BrowserBackend`（`CdpBrowserBackend`）、`SubagentProvider`（fork 与 typed agent 统一契约，句柄提供状态 / 取消 / 结果）。
- **Tool 三层定义**：`ToolDefinition` = `ToolSchema`（模型可见）+ `ToolExecutionSpec`（权限与幂等声明）+ `ToolPresentation`（展示），展示层与执行权限解耦。
- **压缩抽象**：压缩摘要抽象为 `ContextCompactor` 策略（`ModelSummaryCompactor` / `ExtractiveCompactor`）。
- **`agent/pre-step` 决策门**：收拢预算、压缩与上下文注入，并支持可注册策略链改写或终止本步。
- **进化隔离**：新增 Evolution Candidate Scope 与进化信任边界，候选技能 / 提示 / 策略只对显式进入候选评估的视图可见。

### 调整

- Hook 与 WebSocket 工具事件统一接入 EventBus，保留原 `hooks.yaml` 配置兼容。
- ToolRegistry 接入统一 Scope，工具可按 agent / session / 进化候选作用域注册，同名时更具体作用域覆盖。
- Ghost 回合复盘改为消费 Session Event Log，工具结论 / 记忆写入 / 模型错误不再从 transcript 推断。
- 事件日志写入移出 async 线程（`spawn_blocking`），listener 默认 30s 超时。
- 锁文件改为长期同步锚点，避免 unlink 破坏互斥。
- `create_docx` 改用 `docx-rs` 原生生成，不再依赖 python-docx。

### 修复

- 工具安全检查作用于 listener 改写后的最终参数，关闭参数改写绕过 Policy / 危险操作 / 路径检查的漏洞。
- release 构建保留 panic unwind，恢复 listener 与任务监管的 panic 隔离。
- 清空会话时持锁删除事件日志并清理锁文件。
- step 编号在回合内唯一且每个 step 都有 `step/end`；回合异常退出时补发 `turn/end`。
- 工具事件统一归属到持久化会话；强制技能路由回合补发 `turn/end`。
- 能力注册表仅兼容跳过已移除的 `dynamic_library` 条目，其余反序列化失败仍报错。

### 移除

- 移除原生动态库能力进化路径。

## [0.1.7] - 2026-06-28

### 新增
- 新增 ModelRouter 智能路由与连接阶段自动降级，支持 `manual`、`cost_optimized`、`quality_first` 和 `latency_first` 路由策略。
- 新增 Tool Policy 执行策略系统，支持工具名 glob、`|` 多模式、渠道/路径条件、`allow` / `ask` / `deny` 决策、规则组继承和 simulation mode。
- 新增全局 Token / 成本预算控制，按会话跟踪 LLM 用量，避免长任务或异常循环失控消耗。
- 新增 Steering Channel 实时消息注入，允许在 Agent 执行期间继续接收用户指令并注入当前轮次。
- 新增 Agent 生命周期 Hook：`session_start`、`user_prompt`、`pre_tool_use`、`post_tool_use`、`agent_stop`，可通过 `~/.blockcell/hooks.yaml` 执行本机命令。
- 新增审计日志 SHA-256 hash chain 防篡改校验，并补充 SessionStart、SessionEnd、ProviderCall、BudgetEvent 等审计事件。
- 新增 MCP 工具按需发现：当允许的 MCP 工具数量较大时，仅向模型暴露 `mcp_search_tools`，远端工具保持可执行但默认不注入 system prompt。
- 新增 GitHub Actions CI 与多平台 Release 构建流程。

### 调整
- 更新 2026-06 Provider 与默认模型预设，包括 DeepSeek、OpenAI、Anthropic、Gemini、Ollama、GLM、Qwen 等模型族。
- WebUI 改用 `React.lazy` + `Suspense` 分块加载，并优化系统动态区域的按钮布局。
- Gateway 事件广播改为轻量 `WsEventRouting`，避免在路由阶段复制 content/token 等大字段。
- Token 估算与流式读取路径减少无缓存重复计算、历史深拷贝和单行缓冲分配。
- Ghost 配置热重载与 Cron sync 按 mtime 判断是否需要重读配置，减少不必要的读盘和解析。
- SQLite 记忆查询、写入、删除和 maintenance 操作迁移到 `spawn_blocking`，降低阻塞 Tokio worker 的风险。
- 大文件继续按职责拆分，覆盖 runtime、agent、gateway、config、openai、memory、evolution、consolidator 等模块。
- 全仓库 Rust 版本统一为 1.85。

### 修复
- 修复 Gateway API 普通接口接受 URL `?token=` 导致 token 进入访问日志或 Referer 的风险；仅保留 WebSocket 与文件下载/serve 等必要入口。
- 修复 WebSocket / outbound 广播缺少会话隔离、跨会话泄漏，以及任意 WS 客户端可批准其他会话确认请求的问题。
- 修复文件读取/上传缺少大小限制、本地图片 serve 边界、`url_decode` 多字节 UTF-8、CLI Unicode 光标编辑、Home/End/Delete 键处理等问题。
- 修复 `http_request` SSRF 风险、`exec_local` / `exec_skill_script` 路径逃逸、危险 `rm` 命令绕过、超时子进程残留和 symlink 写入逃逸。
- 修复 Ghost review TOCTOU、路径穿越、阻塞主流程、节流泄漏，以及 `.snapshots` / `.skill_file_store.lockdir` 被误扫描的问题。
- 修复 Dream / Session / Auto / Compact 记忆链路中的并发、原子性、事务提交、恢复、锁释放、预算和 Unicode panic 问题。
- 修复 skill evolution 与 core evolution 中状态机、回滚、staging 路径、跨流程污染、无限重试、Python 语法检查和代码围栏注入等安全稳定性问题。
- 修复 OpenAI 兼容 streaming 工具调用、MCP 短名工具加载、hash 算法使用、浏览器导航竞态、CDP 监听器泄漏和 semver 预发布解析问题。
- 修复 Windows 原子写入、路径分隔符、临时目录清理、跨进程锁误删、配置丢失和 schema 不一致等兼容性问题。

### 文档
- 新增 ModelRouter 智能路由与自动降级文档。
- 新增 Hook 生命周期事件系统文档。
- 更新 README / README.en，补充 v0.1.7 的安全、审计、路由和 Hook 能力说明。
- 新增 v0.1.7 Release 说明文档。

## [0.1.6] - 2026-05-09

### 新增
- 新增 Ghost Native 学习闭环：在 turn end、pre-compress、session rotate、session end、delegation end 等边界捕获可复用经验，并沉淀到 `USER.md`、`MEMORY.md` 与 workspace skills。
- 新增 Ghost 学习审计账本与后台 review，记录 episode、review run 和受限工具动作，后台学习失败不会阻塞主对话。
- 新增 Typed Agent 与自定义 Agent 加载能力，支持用户级和项目级 Markdown agent 定义、工具范围、模型、技能、MCP、one-shot、权限模式和后台执行配置。
- 新增多智能体任务增强：checkpoint、AbortToken 链式取消、任务进度事件、任务持久化和结果注入改进。
- 新增统一斜杠命令体系，覆盖 `/help`、`/tasks`、`/skills`、`/tools`、`/learn`、`/clear`、`/compact`、`/session-metrics`、`/log` 等命令，并支持 Gateway/Channel 统一处理。
- 新增 7 层记忆系统指标、熔断器和 `/session-metrics` 观察入口。
- 新增 30+ 个记忆与压缩阈值配置项，可通过 `config.json5` 调整。
- 新增 OpenClaw 技能格式解析、gbrain skills 兼容加载，以及 RabitQ 可选向量索引后端。

### 调整
- DeepSeek 默认体验升级到 DeepSeek V4 Pro 方向，支持 1M 上下文、`reasoningEffort` 配置和 thinking/reasoning 参数注入。
- WebUI 支持 thinking/reasoning 内容流式显示，并改进连接状态、LLM 页面默认值和 agent progress 事件处理。
- skill 加载、CLI 自动补全、Gateway skills/search 与 SkillIndex 统一支持技能包/category 递归扫描和 meta-only skill 可见。
- 技能描述读取统一为 `meta.yaml`/`meta.json` → `SKILL.md` frontmatter → `SKILL.md` 正文，修复多行 YAML description 与搜索权重问题。
- 核心进化与 skill evolution 迁移到 durable workflow，使用 SQLite workflow store、claim/lease、step checkpoint 和后台 worker 推进长流程，避免阻塞 Agent 主循环。
- 记忆系统继续以 SQLite + FTS5 为主存储，并可选接入 RabitQ 做语义向量召回。
- 自学习写入链路增加统一安全扫描、写入守护、快照和撤销能力。

### 修复
- 修复 DeepSeek DSML 工具调用标签解析、跨 delta 分片过滤和 `reasoning_content` 丢失导致的请求错误。
- 修复 WebUI 与 CLI 中 thinking/reasoning 内容不显示、重复输出或日志缺失的问题。
- 修复 skill pack 深层 category、composite name 截断、子 skill 不可见、空目录误注册为 skill 等问题。
- 修复核心进化 tick 阻塞 select 循环导致 Agent 无法响应用户输入的问题。
- 修复多智能体任务生命周期、forked 子代理取消链路、结果注入、任务持久化和 Auto Memory 触发判断问题。
- 修复 Windows 配置原子写入、Python 脚本执行、session_id 冒号路径、versioning 路径分隔符等兼容性问题。
- 修复记忆压缩比例除零 panic、中文文本 UTF-8 边界 panic、配置传播缺失、SQL 注入/安全扫描/写入守护相关问题。

### 文档
- 新增 Ghost Native 学习设计文档。
- 新增统一斜杠命令、session metrics、OpenClaw 兼容、记忆阈值配置、日志命令、intent classifier/loadAllTools、自学习框架等设计文档。
- 更新 Ghost Maintenance 文档，明确它与嵌入式 Ghost 学习的边界。

## [0.1.5] - 2026-04-05

### 新增
- 统一技能运行时，支持 `rhai` 脚本执行链路。
- 增强技能版本管理、审计、演化与服务管理能力。
- 新增 Weixin、QQ 和 NapCatQQ 频道支持。
- 增加 memory 向量索引支持。
- 扩展 WebUI 在聊天、演化与连接状态方面的交互能力。
- 补充技能、CLI、provider 配置、MCP server 和路径访问策略等文档。

### 调整
- 简化技能执行模型与历史处理逻辑。
- 优化消息与工具调用流程，提升核心模块运行一致性。
- 增强 WeCom 长连接支持，并优化频道启动与状态展示。
- 优化 WebUI 聊天体验、消息渲染和前端测试覆盖。
- 更新定时任务的 cron / 时区处理逻辑。

### 修复
- 修复 provider 兼容性问题和部分配置边界情况。
- 修复 gateway / agent / scheduler 的稳定性问题以及重复输出问题。
- 修复路径解析、默认模型调用和媒体处理相关 bug。
- 修复 gateway 断开时 WebUI 卡顿问题。
- 修复 storage / tools / gateway 之间的 memory 规则不一致问题。

### 文档
- 补充并更新了大量中文与英文文档，覆盖技能、渠道、memory、provider 配置和 CLI 使用。
- 新增技能开发相关的 workflow / rules 文档。

## [0.1.4] - 2026-03-25

### 新增
- 当前追踪分支的首次公开版本。
- 核心 agent、provider、storage、scheduler、channels 和 skills 工作区 crate。
- 基础 WebUI 与 gateway 集成。
