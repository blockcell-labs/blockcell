# CLI 核心与 TUI Review 报告（Wave 4 · 入口层）

> 只读审查 · 2026-10-16 · 子代理 w4-cli-core

## 规模

审查范围（仅入口层，不含 gateway/渠道/工具实现）：

| 文件 | 行数 | 说明 |
|------|------|------|
| `bin/blockcell/src/main.rs` | 1625 | clap 命令树 + `main()` 分发 + 13 个解析单测 |
| `bin/blockcell/src/commands/agent.rs` | 1276 | 交互/单次模式主循环、runtime 装配、stdin 线程、确认流（churn 2690） |
| `bin/blockcell/src/commands/agent/tui.rs` | 1169 | 原始模式输入、命令选择器、渲染（churn 1169） |
| `bin/blockcell/src/commands/agent/setup.rs` | 147 | 启动期纯函数（provider/pool/媒体解析/上下文解析） |
| `bin/blockcell/src/commands/slash_commands/mod.rs` | 302 | `SlashCommand` trait + `SlashCommandHandler::try_handle` 分发 |
| `bin/blockcell/src/commands/slash_commands/registry.rs` | 119 | 默认命令注册 + 全局 `Lazy` 单例 |
| `bin/blockcell/src/commands/slash_commands/context.rs` | 207 | `CommandContext` / `CommandResult` 契约 |
| `.../slash_commands/handlers/*.rs` | 14 文件 | 各斜杠命令实现 |

范围合计约 **7.0K LOC**（其中主审 3 文件约 4.1K）。范围内单测约 **49 个**（main 13、agent 4、slash_commands 32）；`tui.rs` **零测试**。

审查方式：只读。未运行 cargo/clippy/test（遵守铁律）。辅助命令均以 `unset BASH_ENV` + `/bin/*` 绝对路径执行。

---

## 模块地图（命令树/入口拓扑）

### 1. 进程入口（`main.rs`）

```
main()                                   main.rs:842
 ├─ Paths::new() → env_file::ensure_and_load_blockcell_env()
 ├─ Config::load_or_default() → paths.apply_workspace_config()
 ├─ std::env::set_var("BLOCKCELL_WORKSPACE", ...)
 ├─ logging::cleanup_old_logs(3d) → logging::init_logging(config.log.*)
 └─ match cli.command { ... }            main.rs:879  →  各 commands::* 模块
```

`Cli { command: Commands, verbose: bool }`（`main.rs:11-18`）。`verbose` 是 `global = true` 的全局开关，但 `main()` 只消费 `cli.command`。

### 2. clap 命令树（顶层 `Commands`）

```
onboard   --force --interactive --provider --api-key --model --channels-only
setup     --force --provider --api-key --model --channel --skip-provider-test
agent     -m/--message -a/--agent -s/--session --model --provider
run       (run_cmd)
gateway   -p/--port --host
doctor / status / memory / alerts / streams / knowledge / completions / logs
audit / session / config / channels / mcp / skills(子命令) / evolve(子命令)
tools(toggle) / cron / upgrade(可选子命令) / ...
```

### 3. agent 运行拓扑（`agent.rs`）

```
run()                                    agent.rs:48
 ├─ 单次模式 (-m)  ──┬─ 建 inbound channel → runtime_handle
 │                   ├─ spawn event handler（流式打印）
 │                   ├─ process_message → println → event_handler.abort()
 │                   └─ skill-evolution tick
 └─ 交互模式 ────────┬─ confirm_answer_tx/rx (std::sync::mpsc<oneshot::Sender<bool>>)
                     ├─ spawn 确认处理器（消费 ConfirmRequest）
                     ├─ spawn_blocking stdin 线程（handle.block_on）
                     │    └─ read_line_with_command_picker()  ← tui.rs:218
                     │         ├─ enable_raw_mode + event::read() 事件循环
                     │         ├─ 命令选择器（'/' 触发，↑↓/Tab/Enter/Esc）
                     │         ├─ Ctrl+C → disable_raw_mode + shutdown_flag
                     │         └─ 返回 → disable_raw_mode
                     ├─ try_handle(input) → SLASH_COMMAND_HANDLER   ← slash_commands
                     ├─ 非命令 → InboundMessage → inbound_tx
                     └─ 退出：shutdown_flag → shutdown_tx → drop(inbound_tx)
                          → runtime_handle.await → timeout(3s, join_all(handles))
```

### 4. 斜杠命令分发（`slash_commands/mod.rs:166`）

```
try_handle(input, ctx)
 ├─ 非 '/' 开头 → NotACommand
 ├─ 解析 cmd_name / args（首个空格切分）
 ├─ 校验：非空、字母开头、[A-Za-z0-9_-] → 否则 NotACommand
 ├─ 遍历 commands：name 或 aliases 命中
 │    ├─ available_channels 不匹配 → Handled("仅在 X 可用")
 │    ├─ !accepts_args && args 非空 → NotACommand
 │    ├─ requires_permission（当前恒 false，预留）
 │    └─ tokio::time::timeout(cmd.timeout_secs()) → execute()
 └─ 未命中 → Handled("未知命令")
```

注册命令（`registry.rs:17`）：`help tasks skills tools learn clear compact clear-skills forget-skill session-metrics(-aliases) log quit exit`。`CommandResult` 变体：`Handled / NotACommand / PermissionDenied(未用) / Error / ExitRequested / ForwardToRuntime`。

---

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|------|------|------|-----------|
| P1 | `main.rs:15-17`（配合 `main.rs:843,879`） | **`-v/--verbose` 全局开关是死代码，与文档契约不符** | `Cli.verbose` 声明为 `#[arg(short, long, global = true)]` 且 doc-comment 写 "Enable verbose logging"；`docs/17_cli_reference.md:12` 承诺"开启 debug 级别详细日志"。但 `main()` 只读 `cli.command`，全仓 grep `verbose` 无任何消费点（`main.rs:691/724/776/1113` 均为其它子命令的同名局部字段）。日志等级完全由 `config.log.level` 决定（`main.rs:870`）。用户按文档加 `-v` 不会有任何效果。 |
| P1 | `agent.rs:48` 交互模式；对照 `gateway.rs:669` | **交互模式无 SIGINT/SIGTERM 处理：raw mode 之外按 Ctrl+C 会硬杀进程** | 仅 `tui.rs:284` 在 raw mode 下把 Ctrl+C 转成 `shutdown_flag`。用户在 agent 处理消息期间（`read_line` 返回后已 `disable_raw_mode`）按 Ctrl+C → 终端默认 SIGINT → 进程立即终止，跳过优雅收尾（`shutdown_tx` 广播、`drop(inbound_tx)`、`timeout(3s, join_all(handles))` at `agent.rs:1190`）与 session/audit 落盘。banner 却宣传 "Ctrl+C to exit"。`gateway` 已装 `tokio::signal::ctrl_c()`，agent 侧缺失，行为不一致。 |
| P2 | `tui.rs:284-293` | **除 Ctrl+C 外的控制键会被当作普通字符插入输入** | 分支只特判 `c == 'c' && CONTROL`；其余 `KeyCode::Char(c)`（crossterm 把 Ctrl+U/A/E/W/D/K 等投递为带 CONTROL 修饰的 Char）落入 `input.insert(byte_idx, c)`（第 293 行）。按 Ctrl+U（清行）、Ctrl+A（行首）、Ctrl+W（删词）、Ctrl+D 等会插入字面字符 `u/a/w/d`。 |
| P2 | `tui.rs:218`（`enable_raw_mode` 起点） | **raw mode 无 RAII 守卫，panic 后终端残留 raw mode** | `terminal::enable_raw_mode()` 仅在 Enter/Ctrl+C/Esc/`Err` 分支显式 `disable_raw_mode()`；期间任意 panic（本文件或父线程）都会把用户终端留在 raw mode，需手动 `reset`。无 `Drop` guard 类型兜底。 |
| P2 | `agent.rs:1037-1040`（stdin 线程） | **确认流存在竞态：普通输入会被吞作确认答案** | stdin 线程在 `read_line` 返回后才 `confirm_answer_rx.try_recv()`。若确认请求在用户正在输入普通消息时到达，该条消息会被 `answer == "y"/"yes"` 判定为确认答复（其余内容一律视为拒绝），原消息丢失且用户无感知；确认提示文本与普通提示行亦无区分标记。 |
| P2 | `handlers/tasks.rs:97-99`；`docs/17_cli_reference.md:165` | **`/tasks [status]` 契约与实现不符** | 文档称 `/tasks [status]` 可按 `running/completed/failed/cancelled` 过滤；实现把任意裸参数当作 task_id 前缀查询（`show_task_detail(trimmed)`）。`/tasks running` 会去查 id 前缀 "running" 的任务并报未找到。`accepts_args()` 返回 true（`tasks.rs:29`），故不会被 `try_handle` 拦截，而是进入错误分支。 |
| P2 | `agent.rs`（单次模式 ~L240-320 vs 交互模式 ~L1000-1195） | **事件处理闭包大段重复（~180 行）** | 单次模式与交互模式各写了一份流式事件处理 + 输出逻辑，仅少量差异。该文件 churn 2690，重复块是行为漂移（如 `skip_ws_echo`、去重、打印顺序）的高风险源。 |
| P2 | `tui.rs:1131 collect_command_items`（`read_line` 内调用） | **每行输入前做一次全量技能目录递归扫描 + 逐个读 SKILL.md** | `scan_skill_dirs` / `read_skill_description_from_dir` 每次进入提示符都重新遍历工作区技能目录并读文件（`SKILL_EXCLUDED_DIRS` 仅排除少量目录），无缓存/失效策略。交互频繁时是重复 I/O。 |
| P2 | `tui.rs:760-775` | **长输入折行未处理，渲染会残留/错位** | `render_input_line` 用 `Print("\r") + Clear(CurrentLine)` 清行、`MoveLeft(after_width)` 回退，均为单行语义。输入超过终端宽度折成多行时，清理与光标回退只作用于当前行，产生残影。 |
| P2 | `agent.rs:300,1064,1114,1154` | **`chat_id` 由 `session.split(':').nth(1)` 推导，静默降级** | 会话 "work:finance" → `finance`；无冒号（如 `-s mysession`）→ `default`；含多个冒号时丢掉第 3 段起。CLI 来源与 runtime 的 `session_key`（`clear.rs:35` 用 `channel:chat_id`）依赖该推导，取值不一致会导致 `/clear`、任务归属错位。 |
| P2 | `agent.rs:1190-1194` | **退出 join 超时后任务被 detach 而非 abort** | `timeout(3s, join_all(handles))` 超时后 future 被 drop，`JoinHandle` 一并 drop → 任务 detach 继续运行，未 `abort()`。进程退出前后台任务可能仍在写状态。 |
| P3 | `main.rs:34,53`（setup/onboard `--api-key`） | **凭据经 argv 传入** | API key 出现在 `ps` 与 shell history 中；`docs/17` 示例亦推荐该用法。建议支持 env/stdin/文件读取。 |
| P3 | `main.rs` `Tools Toggle { enable, disable }` | 两个互斥标志可同时给出且 `--enable` 被忽略 | 分发处仅用 `!disable` 决定状态；`--enable --disable` 时 disable 生效，二者皆无时默认置为 enabled。 |
| P3 | `context.rs:164,187`；`mod.rs:221-232` | `PermissionDenied` 与 `CommandResponse.is_markdown` 为未启用占位 | 均带 `#[allow(dead_code)]`/TODO；`requires_permission()` 恒 false，权限分支为注释。属已声明的扩展点，非缺陷，但应在文档中标注"未生效"。 |
| P3 | `agent/setup.rs:30`；`agent.rs`（evo provider 装配） | `provider_pool.acquire()` 的租约守卫被立即丢弃 | `if let Some((_, p)) = pool.acquire()` / `.map(|(_, p)| p)` 丢弃首个元组元素。若该元素是必须持有的 lease，则租约在 provider 使用前即释放。需与 provider pool 语义核对（本范围外）。 |
| P3 | `agent/setup.rs:49-83` | `extract_media_from_input` 对每个候选 token 做 `path.exists()` 且无工作区边界 | 性能与路径策略问题；仅用于多模态附件识别，未越权读取（文件由用户本地指定），风险低。 |

**P0：未发现**（无数据损坏 / 安全漏洞 / 崩溃级正确性 bug）。已重点核查：`input.drain`/`insert` 的字节边界（`tui.rs:417-419,462-464,292-294`）均先经 grapheme→byte 转换且 `Backspace/Delete` 有边界守卫；`extract_command_query`（`tui.rs:780`）用 `rfind('/')` 返回 ASCII 边界，`&input[..pos]`（`tui.rs:343`）不会 panic；`try_handle` 的 `&input[1..space_pos]` 中 `space_pos` 来自 `find(' ')`，为合法边界（`mod.rs:175-179`）。

---

## 亮点

- **斜杠命令架构清晰、可扩展**：`SlashCommand` trait（`mod.rs:78`）+ 注册表 + `try_handle` 单入口，CLI/Gateway/渠道复用同一实现；`aliases()` / `available_channels()` / `accepts_args()` / `timeout_secs()` 覆盖了别名、渠道隔离、参数门控与超时（`mod.rs:235`）。命令名格式校验（`mod.rs:185-196`）有效阻断了 `"/ help"`、`/1abc` 等畸形输入。
- **`CommandResult` 语义化契约好**：`NotACommand`（透传）/ `Handled` / `ExitRequested` / `ForwardToRuntime` 明确区分"本地零 token 处理"与"转交 LLM"（`context.rs:158`），`/learn`、`/compact` 的转发与超时（`learn.rs:30` 120s）设计一致。
- **TUI 逻辑与业务解耦**：`tui.rs` 从 `agent.rs` 抽离，模块 doc 明确对外仅暴露 4 个符号（`tui.rs:1-6`）；grapheme 感知的宽度计算（`str_display_width`/`grapheme_count`）对 CJK/emoji 输入友好。
- **确认流的 channel 设计合理**：用 `std::sync::mpsc<oneshot::Sender<bool>>` 把异步 runtime 的确认请求桥接到阻塞 stdin 线程（`agent.rs:969-970`），避免在异步上下文里阻塞读 stdin。
- **共享状态抗 poison**：`restore_prompt_line` 用 `unwrap_or_else(|e| e.into_inner())`（`tui.rs:752`），其余 `lock()` 用 `if let Ok` 忽略毒化，不会因一处 panic 二次崩溃。
- **测试覆盖分发核心**：`try_handle` 的透传/未知命令/带参分支（`mod.rs:272-301`）、渠道限制（`quit.rs:69-86`）、全局单例 `/help`（`registry.rs:74-86`）均有测试；`main.rs` 13 个 clap 解析测试覆盖参数边界。
- **`resolve_agent_context` 参数归一化稳健**（`agent/setup.rs:116`）：`trim` + `filter(!is_empty)` + `unwrap_or("default")`，并对未知 agent 显式 `bail!`。

---

## 建议

按优先级：

1. **（P1）接通 `--verbose` 或移除它**：在 `main()` 中于 `init_logging` 前判断 `cli.verbose`，将日志等级提升为 `debug` 并强制 `console_enabled = true`；若产品上不打算支持，则从 `Cli` 删除该字段并同步删除 `docs/17_cli_reference.md:12` 的说明，避免文档与 `--help` 撒谎。
2. **（P1）为 agent 交互模式安装信号处理**：仿 `gateway.rs:669` 用 `tokio::signal::ctrl_c()` 监听，在 cooked 模式（处理中）收到 SIGINT 时置 `shutdown_flag` 并走既有优雅退出路径，而非默认终止。可复用同一 `shutdown_flag`/`shutdown_tx`。
3. **（P2）补齐 TUI 健壮性**：① 引入 raw-mode 的 RAII guard（`Drop` 中 `disable_raw_mode`）；② 控制键白名单化——对带 `CONTROL` 修饰的 Char 只处理 Ctrl+C（可扩展 Ctrl+U/A/E/W/D），其余丢弃；③ `render_input_line` 处理折行（按 `terminal::size()` 计算行数并用 `MoveUp`/多行 Clear）。
4. **（P2）消除重复的事件处理闭包**：把单次/交互两处流式输出逻辑抽为一个 `fn handle_runtime_event(...)`，减少 churn 下的行为漂移。
5. **（P2）修正文档或实现 `/tasks [status]`**：二选一——实现 `running/completed/failed/cancelled` 过滤，或把 `docs/17_cli_reference.md:165` 改为实际的 `/tasks <id> | cancel|resume|restart|delete <id> | clear`。
6. **（P2）缓存技能目录扫描**：`collect_command_items` 增加基于 mtime 的缓存或在 agent 启动时构建一次，避免每次提示符重复递归 I/O。
7. **（P2）统一 `chat_id` 推导**：抽一个 `fn chat_id_from_session(&str) -> String` 并对无冒号/多冒号给出确定性规则，避免 4 处 `nth(1)` 各自为政。
8. **（P2）退出收尾**：`join_all` 超时后对残留 `JoinHandle` 调 `abort()`，或改用 `JoinSet` 以在超时后强制取消。
9. **（P3）凭据输入**：`--api-key` 增加 `BLOCKCELL_API_KEY` env / `--api-key-file` 支持，文档弱化 argv 用法。

### 关于 `build.rs:101` WebUI 哈希守卫（W0 发现）的评估

**现状**：`build.rs` 读取 `webui/dist/source-hashes.txt`，对 `ROOT_INPUTS` + `public/src` 递归集合逐个比对 SHA-256，任一不匹配或集合变化即 `assert_eq!` panic（`build.rs:88-107`）。守卫在 **build script 阶段**执行，因此 `webui/src/App.tsx` 改动后未跑 `npm run build`，会使 `cargo build/clippy/test` 整体失败（"hash left != right"）。

**利**：
- 强保证嵌入产物与源码一致，杜绝"改了前端却发布旧 UI"的静默错误；`rerun-if-changed` 也让增量构建正确重跑。
- 校验还包含清单路径安全（拒绝绝对路径/`..`，`build.rs:56-63`）与摘要格式（`build.rs:52-55`），防御性不错。

**弊**：
- 把一个**非 Rust 前置条件**（Node 构建）硬耦合进所有 Rust 编译路径：纯后端开发者、只跑 clippy、乃至 CI 的 lint job 都会被前端源码状态阻塞，与"Rust 工程可独立编译"的直觉冲突。
- 失败形式是 build script panic，报错对不熟悉该机制的贡献者不友好，且**无逃生舱**（无 env/feature 开关）。
- 交叉编译 / 打包场景下若无 `webui/dist`，`build.rs:73-78` 直接 panic（"embedded WebUI is not built"），阻塞非 WebUI 用途的构建。

**改进建议**：
- 用 **cargo feature 门控**（如 `webui-embed`，默认开）：非该 feature 构建时跳过哈希校验并嵌入占位页；release/打包流水线显式开启。
- 或将硬 `assert` 降级为**分级策略**：`debug` profile 下 `println!("cargo:warning=...")` 并继续（嵌入 stale 资源但可编译），`release`/CI 下保持硬失败。
- 失败信息补充可执行的一键修复指引（`cd webui && npm ci && npm run build`），并在 `docs/02_quickstart.md` 的构建前置条件中显式列出该依赖与常见报错。
- 保留路径安全与摘要格式校验（这部分无争议）。

---

*说明：本报告为只读审查产物，未修改任何源码/配置；唯一写入文件即本报告。未执行 cargo/clippy/test。*
