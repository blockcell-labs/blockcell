# CLI 运维子命令 Review 报告（Wave 4 · 入口层）

> 只读审查 · 2026-10-16 · 子代理 w4-cli-ops

## 规模

审查范围：`bin/blockcell/src/main.rs`、`bin/blockcell/src/config_watcher.rs`，以及
`bin/blockcell/src/commands/` 下除 `gateway/`、`agent/`、`slash_commands/` 目录及
`gateway.rs`、`agent.rs` 之外的全部文件（运维类子命令）。

| 项 | 值 |
|----|----|
| 文件数 | 30（main.rs + config_watcher.rs + 28 个 commands/*.rs） |
| 代码行数 | ≈ 11,844 LOC（wc -l 统计，不含 gateway/agent 模块） |
| 单元测试 | ≈ 52 个 `#[test]` / `#[tokio::test]`（集中在 main.rs 13、setup 6、onboard 5、mcp 4、status/run_cmd/alerts 各 3） |
| 零测试文件 | `config_watcher.rs`、`audit.rs`、`cron.rs`、`completions_cmd.rs`、`logs_cmd.rs`、`knowledge_cmd.rs`、`tools_cmd.rs`、`channels.rs`、`doctor.rs`、`evolve.rs`（evolve 1045 行、doctor 816 行均无测试） |
| 命令入口 | main.rs 的 `Cli`/`Commands`（clap derive），每个子命令转发到 `commands::*::run` |

审查方式：只读；未运行 cargo/clippy/test。为佐证结论只读了 core 的 `config.rs` /
`file_store.rs` / `paths.rs`（跨切面），未修改任何文件。

## 模块地图（命令清单表）

| 命令 | 入口 | 文件 | 关键行为 / 持久化 |
|------|------|------|------------------|
| `onboard` | `onboard::run` | commands/onboard.rs | 写 config.json5（`write_raw_validated_config_json5`）、工作区 md、内嵌技能；探测环境 |
| `setup` | `setup::run` | commands/setup.rs | 交互向导；`Config::load`→改内存→`config.save()`（整文件重写） |
| `status` | `status::run` | commands/status.rs | 只读汇总（provider/model/agent/channel） |
| `doctor` | `doctor::run` | commands/doctor.rs | 只读诊断 + 探测外部命令 |
| `config show/schema/get/set/edit/providers/reset` | `config_cmd::*` | commands/config_cmd.rs | `set`/`reset` 走 `Config::save` 整文件重写；`show`/`get` 打印展开后配置 |
| `tools list/info/test/toggle` | `tools_cmd::*` | commands/tools_cmd.rs | `toggle` 走 `json_store::update_json`（锁+原子写） |
| `run tool/msg` | `run_cmd::*` | commands/run_cmd.rs | 直接调工具 / 转 `agent::run` |
| `channels status/login/owner *` | `channels::*` | commands/channels.rs | `login weixin`/`owner set/clear` 走 `config.save()` |
| `cron list` | `cron::list` | commands/cron.rs | 只读（`CronService::load`） |
| `skills list/show/enable/disable/reload/learn/install/test/test-all/clear/forget` | `skills::*` | commands/skills.rs | 进化记录 CRUD；`install` 解压 hub 包 |
| `evolve run/trigger/list/show/status/watch/rollback` | `evolve::*` | commands/evolve.rs | 读写 `workspace/evolution_records/*.json` |
| `memory list/show/delete/stats/search/clear/maintenance/retry-vector-sync/reindex/migrate-canonical` | `memory::*` | commands/memory.rs | SQLite memory.db + MEMORY.md |
| `alerts list/history/evaluate/add/remove` | `alerts_cmd::*` | commands/alerts_cmd.rs | `workspace/alerts/rules.json`（`update_json`） |
| `streams list/status/stop/unsubscribe/restore` | `streams_cmd::*` | commands/streams_cmd.rs | 读本地 `subscriptions.json` + 部分走 gateway HTTP |
| `knowledge stats/search/export/list-graphs` | `knowledge_cmd::*` | commands/knowledge_cmd.rs | 直连 `knowledge_graphs/*.db`（rusqlite） |
| `mcp list/show/add/remove/enable/disable/edit` | `mcp::*` | commands/mcp.rs | `mcp.json` + `mcp.d/*.json`（**裸 fs::write**） |
| `session list/replay/verify` | `session_cmd::*` | commands/session_cmd.rs | 只读 session 事件日志 |
| `audit verify` | `audit::verify` | commands/audit.rs | 只读 hash-chain 校验 |
| `logs show/follow/clear` | `logs_cmd::*` | commands/logs_cmd.rs | 读/清 `workspace/logs` |
| `upgrade check/download/apply/rollback/status` | `upgrade::*` | commands/upgrade.rs | 通过 `blockcell_updater` |
| `completions <shell>` | `completions_cmd::run` | commands/completions_cmd.rs | 生成补全（自带一份**复制版** CLI 定义） |
| `embedded_skills` | `embedded_skills::extract_to_workspace` | commands/embedded_skills.rs | 首次解压内嵌技能（不覆盖） |
| `json_store` | `read_json`/`update_json` | commands/json_store.rs | 唯一「锁 + 原子写」JSON store 原语 |
| `env_file` | `ensure_and_load_blockcell_env` | commands/env_file.rs | 启动时写/读 `~/.blockcell/.env` 并 `set_var` |
| `config_watcher` | `ConfigWatcher::start` | config_watcher.rs | notify 热加载（未在 main 中启用，见问题表） |

## 问题清单

级别：P0=数据损坏/安全漏洞/崩溃级正确性 bug；P1=重要缺陷或明确设计问题；P2=中等/可维护性；P3=建议。

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|------|-------------|------|-----------|
| **P0** | commands/config_cmd.rs:262-282（`set`，落盘 274） | `config set <KEY> <VALUE>` 走「整文件重写」：`load_or_default`→`to_value`→改路径→`from_value::<Config>`→`Config::save`。这会把用户手写的 **JSON5 注释全部丢弃**（`stringify_json5_pretty` 实为 `serde_json::to_string_pretty`，输出纯 JSON），并**静默删除 `Config` 结构体未建模的所有键**（`Config` 无 `deny_unknown_fields`，反序列化时未知键被忽略，save 时消失）。文档（docs/17:285-363）正鼓励用户手改 JSON5 配置，一条无关的 `config set` 即可静默破坏用户内容。附带：对未知键 `config set foo.bar 1` 会打印 `✓ Set foo.bar = 1` 但实际**什么都没写入**（假成功）。 | core `config.rs:645-653` `save`；`config.rs:43-45` 无 `deny_unknown_fields`；`stringify_json5_pretty`=`serde_json::to_string_pretty`（config.rs:364-369） |
| **P1** | commands/config_cmd.rs:8-19（`show`）、235-255（`get`，244 行） | CLI 侧明文回显密钥：`config show` 打印**展开后**的完整配置（含所有 provider `apiKey`、渠道 token）；`config get providers.openai.apiKey` 直接打印明文密钥。`main.rs:850` 已先把 `.env` 载入进程环境，故 `${ENV}` 会被展开成真实值。这正是任务所述「gateway `GET /v1/config` 明文回显」的 CLI 同类行为。 | `show` 用 `Config::load_or_default`+`to_value`+`stringify_json5_pretty` 打印；`get` 对 string 直接 `v.as_str().unwrap()` |
| **P1** | commands/config_cmd.rs:262-282（同 `set` 调用面） | **放大 core 的 P2**：`Config::save` 依赖 `preserve_unchanged_env_placeholders`（`config.rs:341-362`）在「旧值含 `${` 且 `expand(旧)==新`」时才回填占位符；该启发式在值被 trim/规范化、或旧文件无法被 `parse_json5_value_preserving_env` 解析（含 JSON5 特性/注释的复杂文件）时失效。`config set` 会在**任意**键上强制整配置重新序列化并落盘，从而把内存中已展开的明文密钥写回文件。审查结论：调用面**确认放大**该问题——它把 core 的「保存时可能泄漏」从「显式改 provider 的路径」扩散到「改任意一个无关配置键的路径」。 | core `config.rs:645-653`、341-362；`config_cmd.rs:265,273-274` |
| **P1** | commands/env_file.rs:44-53（写 46） | `.env` 是文档指定的密钥存放处（`default_env_template` 列了 `OPENAI_API_KEY` 等），但用 `fs::write` 创建，权限为默认（umask 022 → 0644，全局可读）；而 `config.json5` 经 `write_text_atomic_durable` 明确设为 `0o600`（core `config.rs:437`）。密钥文件权限不一致且偏松。 | `paths.ensure_dirs`（paths.rs:352-379）建目录亦为默认 0755 |
| **P1** | commands/env_file.rs:7-9 + 58-75（`set_var` 63-69） | **默认模板会清空真实环境变量**：模板含 `OPENAI_API_KEY=`（空值），`parse_env_assignment` 返回 `("OPENAI_API_KEY","")`，`load_env_file` 无条件 `std::env::set_var` 覆盖——用户 shell 里已 export 的 `OPENAI_API_KEY` 会被覆盖成空串，随后 config 中 `${OPENAI_API_KEY}` 展开为 `""`，鉴权静默失败。该函数在 `main.rs:850` 于**所有**命令前执行，影响面覆盖全部运维子命令。 | `parse_env_assignment`（env_file.rs:11-34）对 `KEY=` 返回 Some；`set_var` 语义为覆盖 |
| **P1** | commands/config_cmd.rs:305-306（`edit`） | `EDITOR`/`VISUAL` 已设置但为空或纯空白时，`editor.split_whitespace().collect::<Vec<_>>()` 得到空 vec，`parts.split_first().unwrap()` **panic**。对比 `mcp.rs:447-449` 用 `parts.next().context("EDITOR is empty")?` 正确处理——同仓库内实现不一致。 | 另：`EDITOR="open -t"` 之类的引号/空格路径也会被 `split_whitespace` 错误拆分（两处皆然） |
| **P1** | commands/alerts_cmd.rs:239-246（`add`） | 契约不一致：`alerts add --source` 要求「含 `tool` 字段的 JSON 对象」，而 docs/17_cli_reference.md:791-810 记载格式为 `tool:action:param`（示例 `finance_api:stock_quote:600519`）。按文档执行必然报错 `--source must be a JSON tool call object`。 | main.rs 的 `--source` help 才是对的；文档未同步 |
| **P2** | commands/skills.rs:1261-1262 | `if out.len() > 200 { &out[..200] }` 按**字节**切片，若输出含多字节字符（`-i` 可传中文输入、脚本输出可能为中文）会在非字符边界 panic。 | 同文件 1215 行 `split('\'').nth(1).unwrap_or(&err_str)` 是安全的，说明作者知道该模式 |
| **P2** | commands/config_cmd.rs:346-352（`providers`） | `&key[..4]` / `&key[key.len()-4..]` 按字节切片掩码显示密钥，多字节密钥会 panic（虽有 `len()>8` 守卫，但仍可能切在字符中间）。 | |
| **P2** | commands/alerts_cmd.rs:105（`list`） | `&id[..8]` 按字节切片；规则 id 若非 ASCII（用户/导入数据）会 panic。 | |
| **P2** | commands/evolve.rs:266-273（`list`，267 行） | `rollout.stages[rollout.current_stage]` 无边界检查；记录损坏或旧版本记录中 `current_stage >= stages.len()` 会 panic。evolve.rs 无任何测试。 | 属「读磁盘记录→直接索引」的不可信输入 |
| **P2** | commands/mcp.rs:193-200、380-388 | `save_server_file`/`update_file_enabled` 用裸 `std::fs::write` 写 `mcp.d/<name>.json` 与 `mcp.json`：**无原子写、无文件锁**（对比 `json_store::update_json` 的锁+`atomic_write`）。写入中途崩溃会留下半截 JSON，导致 MCP 配置损坏。 | `json_store` 原语已存在却未被复用 |
| **P2** | main.rs:16-17、843 | 全局 `--verbose` 被 clap 解析但**从未使用**（`main.rs` 内无 `cli.verbose` 引用；日志等级来自 `config.log.level`）。docs/17:11-15 明确记载它「开启 debug 级别详细日志」——文档化功能为 no-op。 | grep `cli.verbose` 无匹配 |
| **P2** | commands/completions_cmd.rs:47-171 | `build_cli()` 复制了一份精简 CLI，且**与真实命令树脱节**：宣称存在 `cron add/remove/enable/run`（真实仅有 `cron list`）、缺 `config show/schema`、`tools show`、`logs follow`、`memory list/show/delete/migrate-canonical`、`session`、`audit`、`mcp`、`skills enable/disable/reload/install/test` 等。生成的补全会推荐不存在的子命令。 | 维度⑥ 的核心证据：同一命令树维护两份定义 |
| **P2** | commands/status.rs:9-52,414-457 vs commands/doctor.rs:165-185,762-816 vs commands/channels.rs:13-89 | 大段逻辑**跨文件逐字复制**：`agent_owner_bindings`、`resolved_agent_active_provider_and_model`、`primary_pool_entry`、`provider_ready`（status 与 doctor 实现还不一致：doctor 对 ollama 直接返回 true）、`known_account_ids`（channels/doctor 各一份）。任一字段变更需改多处。 | 维度⑥ |
| **P2** | commands/memory.rs:499-519（`clear`） | `memory clear` 直接软删除（无 `--force`/无确认），而 `config reset`（config_cmd.rs:380-392）与 `logs clear`（logs_cmd.rs:180-192）都会二次确认。虽然软删可恢复，但行为不一致且 docs/17:732-738 未提示风险。 | 维度① 幂等/破坏性操作 |
| **P2** | commands/upgrade.rs:24-52、54-84 | `upgrade check` / `download` 把错误吞掉并 `println!` 后返回 `Ok(())` → 进程退出码 0。CI/脚本无法判断检查或下载失败。 | 对比 `apply`/`rollback` 正确向上传播 |
| **P2** | commands/doctor.rs:188、669-681 | `doctor` 统计出 `err_count>0` 仍返回 `Ok(())`（退出码 0）。诊断命令在 CI 中失去「失败即非零」的可用性。 | |
| **P2** | commands/evolve.rs:59-81 | `evolve run` 在 `trigger_manual_evolution` 失败（68-70）或 provider 创建失败（77-80）时打印错误后 `return Ok(())` → 退出码 0，脚本误判成功。 | |
| **P2** | commands/skills.rs:test/test-all 收尾 | `skills test`/`test-all` 在存在失败用例时仍返回 `Ok(())`（仅打印汇总），退出码 0，CI 无法用其做门禁。 | |
| **P2** | commands/memory.rs:177-183、204-210 | `show`/`delete` 把 DB 错误 `println!` 后返回 `Ok(())`，退出码 0；`delete` 对不存在的 id 也返回成功语义。 | |
| **P2** | commands/streams_cmd.rs:156-170（`stop`） | 数据源不一致：`list`/`status` 读本地 `subscriptions.json`，而 `stop` 先请求 gateway `/v1/streams` 再 `DELETE`。gateway 未运行时 `stop` 直接失败；本地存在但 gateway 内存中不存在的订阅会被判为「No matching subscription」。 | 契约/一致性问题 |
| **P2** | commands/channels.rs:298-316（`owner_clear`） | `owner_set` 校验 channel ∈ `SUPPORTED_OWNER_CHANNELS`（262-264），`owner_clear` **不校验**；拼错渠道名会静默写入一个无效绑定（需 `doctor` 才能发现）。 | |
| **P2** | commands/logs_cmd.rs:139-165（`follow`） | 轮询 `metadata().len()` 判断新增：日志轮转（文件被截断/替换/删除）后 `current_size > last_size` 恒为 false，`follow` 永久静默；读到非 UTF-8 会 `read_to_string` 报错退出。首次也把整文件读入内存（125 行）。 | |
| **P2** | commands/setup.rs:111 | 结尾硬编码提示 `Open WebUI: http://localhost:18791/`，未读取 `gateway.webuiHost/webuiPort`（onboard.rs 的 EXAMPLE_CONFIG 里也是写死的 18791）。配置改端口后提示错误。 | |
| **P2** | commands/tools_cmd.rs:16-17,63 vs 150,230 | `tools list`/`info` 用 `Paths::new()`，而 `tools test`/`toggle` 用 `Paths::new_configured()`；对配置了自定义 `workspace` 的用户，两者指向的工作区不一致（`list` 不看 workspace 覆盖）。 | |
| **P2** | commands/onboard.rs:236-250 | `onboard --channels-only` 是 no-op：只打印提示后 `return Ok(())`，不做任何渠道写入；docs/17:106 描述为「仅更新渠道配置」。 | |
| **P2** | commands/evolve.rs:89,188,206,285,349,417,525 | 多处硬编码 `paths.workspace().join("evolution_records")` 与 `.join("skills")`，未用 `Paths` 语义化方法（当前取值相同，但违背单一来源）。 | |
| **P3** | crates/core/src/paths.rs:352-379（main 调用面） | `ensure_dirs` 建目录用默认 0755，`~/.blockcell` 及其子目录对同机其他用户可遍历；叠加 P1 的 0644 `.env` 构成信息泄露面。 | 建议 `~/.blockcell` 0700、secret 文件 0600 |
| **P3** | commands/setup.rs:653-660 | `prompt_line` 直接回显 API key（无隐藏输入），密钥会留在终端 scrollback。 | |
| **P3** | commands/mcp.rs:251-267（`show`） | `show` 打印 server 配置全文，含 `env` 值明文（`--raw --env K=V` 写入的密钥会回显）。 | |
| **P3** | commands/knowledge_cmd.rs:27-33 | `stats` 用 `unwrap_or(0)` 吞掉 SQL 错误，DB 损坏时显示 0 而非报错。 | |
| **P3** | commands/knowledge_cmd.rs:87-111 | FTS5 `MATCH ?1` 对含特殊字符的 query 会在 `query_map` 阶段失败，被 `filter_map(r.ok())` 吞成空结果（仅 `prepare` 失败才回退 LIKE），用户看到「无结果」而非语法错误。 | |
| **P3** | commands/skills.rs:install | 覆盖安装时 `remove_dir_all` 删除既有技能目录且无确认；下载的 hub 产物未见校验和/签名校验即落盘（供应链面）。 | |
| **P3** | commands/onboard.rs:324-341,556-743 | `probe_environment` 串行执行大量外部命令与一次 `1.1.1.1:53` 阻塞 DNS 解析，onboard 可能长时间卡顿。 | |
| **P3** | commands/logs_cmd.rs:47,125 | `show`/`follow` 用 `read_to_string` 整文件读入，大日志文件内存占用高。 | |
| **P3** | commands/config_watcher.rs（整文件） | 未在 main.rs 中启用（无 `ConfigWatcher` 引用），属死代码/未接线；且监听父目录 NonRecursive，同目录任意文件创建/修改都会触发重载；`while let Some(_) = rx.recv()` 为 clippy `while_let_some` 风格。 | 需确认是否另有调用方 |
| **P3** | commands/audit.rs / commands/cron.rs / commands/session_cmd.rs | 已检查：逻辑正确（audit 在链损坏时 `bail!` 返回非零；cron 只读；session verify 报告 mismatch 但返回 Ok，脚本侧需自行解析）。session `verify` 检出 mismatch 时退出码仍为 0，可考虑提供 `--strict`。 | 无缺陷，仅提示 |

### 契约一致性（维度⑦ 汇总）

| 项 | 文档 | 实现 | 结论 |
|----|------|------|------|
| `alerts add --source` 格式 | docs/17:798 `tool:action:param` | 要求 JSON 对象含 `tool` | **不一致（P1）** |
| `streams restore` | docs/17:852「显示可恢复的订阅列表」 | 实际 POST gateway `/v1/streams/restore` | 不一致（P2） |
| `session list/replay/verify` | docs/17 无对应章节 | 已实现 | 文档缺失（P3） |
| `memory migrate-canonical` | docs/17 memory 段未列 | 已实现 | 文档缺失（P3） |
| `--verbose` | docs/17:11「开启 debug 日志」 | 未使用 | **不一致（P2）** |
| `blockcell logs`（无子命令） | docs/02_quickstart.md:381 | clap 要求子命令 | 文档错误（P3） |
| completions 补全内容 | —— | 与真实命令树脱节 | 不一致（P2） |
| `cron add/remove/enable/run` | docs/17:500 已声明「CLI 只提供查看」 | 仅 `cron list` | 一致（文档正确） |
| setup/onboard provider、channel 清单 | docs/17 | 与 `normalize_provider`/`normalize_channel` 一致 | 一致 |

### 并发（维度②）与 json_store

- `commands/json_store.rs:13-34` 是**正确的**并发原语：`ExclusiveFileLock::acquire`（core `file_store.rs:55-71`，30×100ms 重试）+ `atomic_write`（写临时文件→`sync_all`→`rename`→父目录 `sync_all`），并有 `corrupt_json_is_not_replaced`、`concurrent_updates_do_not_lose_changes` 两个测试。多 CLI 实例并发改 alerts/toggles 不会丢更新。
- **问题在复用不足**：全仓仅 `alerts_cmd`、`tools_cmd` 两个调用点用了它；`mcp.rs`（mcp.d/mcp.json）、`streams` 订阅、`onboard` 工作区写入、`knowledge export` 都用裸 `fs::write`，既无锁也无原子性。
- `Config::save` 走 `write_text_atomic_durable`（0600 + 原子替换），但**无跨进程锁**：两个 CLI 同时 `config set` 会互相覆盖（last-writer-wins），且 `main.rs:854` 的 `load_or_default` 在缺字段时也会写回配置——并发下与用户编辑存在竞态。

## 亮点

1. `commands/json_store.rs`：把「锁 + 原子写」抽象成单一原语并配并发/损坏测试，是本次审查中并发正确性最好的部分；`atomic_write` 连父目录都 fsync。
2. `commands/session_cmd.rs`：事件渲染 + 指标汇总设计清晰，`preview` 有单行化与截断测试；`verify` 对 compaction 造成的「预期缺失」给出了明确 note。
3. `commands/memory.rs::migrate_canonical_at`：迁移-去重-退役-重建索引一步到位，且**幂等**（测试 `migrate_canonical_deduplicates_retires_and_reindexes` 验证重跑为 0）。
4. `commands/audit.rs`：链校验失败时 `bail!` 返回非零退出码，并逐条打印 error，行为正确。
5. `commands/mcp.rs::validate_server_name`：拒绝 `.`/`..`/含 `/`、`\`、控制字符的 server 名，杜绝了 `mcp.d/<name>.json` 的路径穿越；`edit` 对空 `EDITOR` 用 `context` 而非 `unwrap`。
6. `commands/embedded_skills.rs`：仅写不存在的文件，永不覆盖用户改动；`onboard.rs::write_if_not_exists` 同理——初始化类写入的幂等性处理得当。
7. `commands/alerts_cmd.rs::remove` 与 `streams_cmd.rs::resolve_subscription_id`：ID 前缀匹配在 0 命中/多命中时都 `bail!`，避免了「模糊删除」。
8. `config_cmd.rs` 与 `onboard.rs` 已有针对 `parse_json5_value` / 占位符保留的单元测试（`provider_patch_preserves_environment_placeholders`），说明团队对 env 占位符问题有意识。

## 建议

1. **（对应 P0）** 让 `config set` 基于**原始文本**做定点修改：用 `parse_json5_value_preserving_env` 读原文件 → 只改目标路径 → 写回，避免 `from_value::<Config>` 丢弃未知键；或至少在 `Config` 上加 `deny_unknown_fields` 并对未知键**报错**而非假成功。注释无法保真时，应在写入前提示「将丢失注释」，或改用 `json5` 保注释编辑器。
2. **（对应 P1 明文回显）** `config show`/`get` 默认对敏感路径（`*.apiKey`、`*Token*`、`*secret*`、`*aesKey*`）脱敏（如 `sk-****`），仅在新增 `--reveal` 时明文；`config show` 增加 `--redact`（默认开）。与 gateway 侧修复保持一致策略。
3. **（对应 P1 放大 core P2）** 在 core 提供 `Config::save_preserving_env(path, original_text)` 语义，或让 CLI 的 `set` 不做全量 `Config` 往返；在 `save` 的占位符回填失败时**拒绝落盘并报错**，而非静默写明文。
4. **（对应 P1 env_file）** `.env` 创建用 `OpenOptions::mode(0o600)`；`~/.blockcell` 用 0700。`load_env_file` 改为**不覆盖已存在的非空环境变量**（`if std::env::var(key).map(|v| v.is_empty()).unwrap_or(true) { set_var }`），至少对空值模板做特殊处理，避免清空 shell 传入的密钥。
5. **（对应 P1 panic）** `config edit` 与 `mcp edit` 统一走一个 `resolve_editor()` helper，空 `EDITOR` 返回明确错误；所有「展示/掩码」场景（`config providers`、`alerts list`、`skills test` 预览）改用 `chars()` 切片而非字节切片。
6. **（对应 P2 契约）** 修正 docs/17 的 `alerts add --source` 示例为 JSON 对象格式；`streams restore` 文档改为「在运行中的 Gateway 恢复订阅」；补 `session`、`memory migrate-canonical` 章节；删除 quickstart 中裸 `blockcell logs`。
7. **（对应 P2 completions）** `completions_cmd::build_cli` 改为复用真实 CLI：把 `Cli` 定义抽到 lib 或 `pub fn command() -> clap::Command`（`Cli::command()`），`generate` 直接消费它，消除双份命令树。
8. **（对应 P2 复用）** 把 `mcp.rs` 的 server 文件读写、`streams` 订阅写入改为复用 `json_store::{read_json, update_json}`；`Config::save` 增加基于 `file_store::ExclusiveFileLock` 的跨进程锁，解决并发 `config set` 覆盖。
9. **（对应 P2 退出码）** 为「可脚本化」命令引入统一约定：`upgrade check/download`、`doctor`、`evolve run`、`skills test/test-all`、`memory show/delete` 在失败时返回非零（或提供 `--strict`/`--json`），并在 docs 中标注退出码语义。
10. **（对应 P2 重复逻辑）** 把 `agent_owner_bindings`、`resolved_agent_active_provider_and_model`、`primary_pool_entry`、`provider_ready`、`known_account_ids` 收敛到 `commands/` 下的共享模块（或 core），status/doctor/channels 共用一份，并统一 ollama 的判定口径。
11. **（可维护性/测试）** 为当前零测试的 `evolve.rs`、`doctor.rs`、`channels.rs`、`logs_cmd.rs` 补关键路径测试，优先覆盖：evolve 记录损坏时的边界、`owner_set/clear` 的校验、logs follow 的轮转行为。

### 已检查未发现问题

- **session/memory 命令的幂等**（维度①）：`session_cmd` 全只读；`memory::migrate_canonical_at` 幂等有测试；`memory maintenance`/`retry_vector_sync`/`reindex` 语义为「尽力而为」并如实打印计数，未见重复执行导致的数据损坏。
- **json_store 并发写（多 CLI 实例）**（维度②）：见上文，原语本身正确，问题只在复用范围。
- **audit 链校验、cron 只读列表、knowledge 的 SQL 注入**：`knowledge_cmd` 全程使用 `rusqlite::params!` 绑定参数，未见拼接注入；`export_dot`/`export_mermaid` 对 `"` 做了转义（`replace('"', "\\\"")`）。
