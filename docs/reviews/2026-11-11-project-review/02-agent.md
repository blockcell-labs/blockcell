# B2：Agent 运行时与权限边界 Review

> 历史记录（2026-11-11）：原 24 项已于 2026-11-12 复核。当前裁决、限定、工具链验证与新增问题以 [优化指导](README.md)、[复核矩阵](08-revalidation-matrix.md) 为准；本文旧统计及 Rust 阻断说明仅代表首次审查时的结果。

日期：2026-11-11。基线：`5ca38be861b4a3e8f7a1e8d332e242b42ed25710`，`main`。业务代码只读。本批确认 **P1 × 2、P2 × 2**，无已证实 P0。

## 实际审查范围

精读或按调用链检查：

- `runtime/tool_pipeline.rs`：准入阶段顺序、shell alias 规范化、权限复核、文件观察、脚本批准缓存、工具上下文与会话授权。
- `runtime/path_security.rs`：路径提取表、技能资源访问、workspace/会话授权/deny 的优先级。
- `runtime/task_progress.rs`：证据生成、Launch/Poll/Input/Close、revision、任务契约及完成条件。
- `runtime/turn_flow.rs`：模型预算、prompt skill loop、手动技能执行及持久化。
- `runtime/process_message_inner.rs`：技能激活分支、工具结果证据采集、验证/交付物完成门、后台任务汇总。
- `runtime/{message_task,run_loop,tool_exec,fork_spawn,subagent_provider,wiring,prefetch}.rs`：取消、消息派生、预取和子代理边界。
- `runtime.rs::fork_for_message_task`、`context.rs::resolve_active_skill_by_name`，以及相关技能 canonical 路径测试。
- `forked/{can_use_tool,agent}.rs`、`forked/agent/{run,tool_exec,event}.rs`：工具名单、记忆目录限制、shell 执行。
- `task_manager.rs` 来源过滤；`auto_memory/extractor.rs` 和 `scheduler/consolidator/consolidate.rs` 的实际后台入口；`skill_file_store.rs` 名称解析（身份不一致问题交由 B3 计数）。
- 缓存方面检查了 `response_cache.rs` 的按会话索引与持久化引用、`runtime/tool_exec.rs` 的工具输出目录，不代表穷尽全部缓存层。

历史回归核对：2026-10-31 报告中的“同步完成 shell 证据丢失”已由当前 `task_progress.rs:346` 区分 Launch/Poll 修复；“FS_NOT_OBSERVED 直接重试提示”已由 `tool_pipeline.rs:37` 明确要求先 `read_file`，本批不重复记为缺陷。

## B2-01 [P1] 后台记忆 shell 可通过目录内 symlink 读取目录外文件

**主要位置：** `crates/agent/src/forked/can_use_tool.rs:846`（`shell_command_paths_are_confined`）；执行位置 `crates/agent/src/forked/agent/tool_exec.rs:520`、`:237`。

**触发前提：** 自动记忆/梦境整合启用，记忆目录中有一个指向目录外普通文件的符号链接，例如 `memory/link -> ../outside.txt`。目标不是沙箱显式禁止的凭据路径。后台模型调用 `shell`，参数为 `command: "cat link"`、`working_dir: <memory 目录>`。

**证据链：**

1. `auto_memory/extractor.rs:254`、`:685` 和 `scheduler/consolidator/consolidate.rs:40` 构造实际 fork，设置 memory working_dir，并使用记忆范围 `can_use_tool`。这些入口仅从 schema 排除 `exec`；`forked/agent/event.rs:267` 的 `shell` schema 仍存在。
2. `create_auto_mem_can_use_tool` / `create_dream_can_use_tool` 将 shell 交给 `allow_restricted_shell`（`can_use_tool.rs:114`、`:150`）。`is_read_only_command("cat link")` 返回 true；`shell_command_paths_are_confined` 只检查字面 token 是否绝对路径、含 `..`、环境变量等，不对相对 operand canonicalize，因此也返回 true。
3. `forked/agent/tool_exec.rs:520` 只调用 `resolve_forked_path` 验证 working_dir，未解析 shell operand；`:527` 交给 `execute_shell_command`。
4. 后者使用 `WorkspaceWrite`（`:237`）。该策略限制写入，却允许普通全盘读取：macOS `tools/sandbox.rs:146` 的 `(allow file-read*)`、Linux bubblewrap `:238` 的 `--ro-bind / /` 都不会把这个普通文件限制在 memory 内。

**实际行为与影响：** 与直接 `read_file` 不同，shell 经 symlink 可将记忆目录外文件内容读入后台模型上下文，突破该后台代理明示的记忆目录读取范围。只读写入沙箱不能弥补这条读取授权缺口。

**最小检查（已执行，非整应用端到端测试）：** 在临时目录创建上述结构，直接提取当前源码的两个纯谓词函数，用本机 `rustc` 编译成临时程序，然后执行同一条 `cat link`。所有文件均为人工 marker，执行后清理。结果：

```text
rustc_exit=0
readonly_predicate=true
path_confinement_predicate=true
exit=Some(0); output=outside-memory-marker
probe_exit=0
```

这验证了当前原函数接受该命令及文件系统的 symlink 行为；完整后台模型调用和原生沙箱运行未实测，沙箱允许读取的判断来自上述策略源码。

**修复方向：** 记忆 fork 完全移除通用 shell，使用已有受约束文件工具；如确需 shell，则使用仅允许读取记忆目录与运行库的沙箱范围，并禁止跟随 operand/递归遍历中的外部 symlink。仅校验 command 字符串不足以实现目录隔离。

## B2-02 [P2] 技能执行路径跳过验证与交付物完成检查

**主要位置：** `crates/agent/src/runtime/turn_flow.rs:430`；相关 `:388`、`:516`、`:556`，以及 `runtime/process_message_inner.rs:1491`。

**触发前提：** 消息携带显式 `task_contract`（例如 `verification: required` 或缺失的 required deliverable），并通过手动技能或 `activate_skill` 进入 prompt skill loop。模型在未验证修改、未写所需产物时给出最终答复。

**证据类型：静态完整调用链。**

- `run_prompt_skill_loop` 在 `turn_flow.rs:388` 创建 `TaskProgress`，`:389` 注册消息契约，`:464` / `:516` 还会合并工具携带的契约。
- 该循环从未调用 `task_progress.observe`，不消费执行后的 revision/验证证据；无工具响应在 `:430-440` 直接 break。返回前 `:556-558` 只保存 contract，不调用 `needs_verification` 或 `first_incomplete_output`。
- 手动技能路径 `execute_decided_skill_route` 在 `process_message_inner.rs:535-539` 提前返回。模型激活技能路径在 `:1491-1494` 直接采用技能结果并跳出主循环。
- 普通路径实际有对应检查（`:2155` 验证续轮、`:2191` 交付物补齐），两个技能出口都无法经过它们。

**实际行为与影响：** 相同显式任务契约在普通工具循环会触发补验/补产物，在技能路径则只持久化契约并交付回答。必需文件仍可缺失，或代码修改未经所要求的检查即结束。这里不意味着系统伪造一条已通过的测试记录，而是技能分支缺失已有的完成约束。

**修复方向：** 将工具证据观察与最终完成决策抽为普通循环和技能循环共用的逻辑，技能循环使用配置的 verification policy；返回前统一处理 required 验证、缺失/未更新交付物，并让主循环获得技能的实际证据与执行结果。

## B2-03 [P2] 第 65 个会话会使旧会话预算计数归零

**主要位置：** `crates/agent/src/runtime/turn_flow.rs:127`；阈值 `:5`，重建 `:133-138`。

**触发前提：** 同一长生命周期 runtime 累计服务超过 64 个不同会话，并配置 `max_tokens_per_session` 或 `max_cost_cents_per_session`。被任意驱逐的旧会话随后再次发起模型请求；不要求同时存在 65 个运行中任务。

**证据类型：静态。** `budget_tracker_for_session` 在新 key 且 map 长度达到 `MAX_BUDGET_TRACKERS = 64` 时，直接删除 `trackers.keys().next()`。后续旧 key 通过 `BudgetTracker::with_parent` 重建；`core/budget.rs:128-130` 将 token 和成本计数初始化为 0，没有恢复旧会话的账本。`:278` 的请求前预算检查和 `:204` 的消费记账都再次访问该 map。

**实际行为与影响：** 至少一个旧会话已消费的额度丢失，甚至已耗尽额度的会话也能恢复调用。`HashMap` 任意 key 不等于空闲会话，活跃会话也可能被驱逐。有限大小缓存变成了额度失效的条件，在多会话长期服务中造成超额调用。

**修复方向：** 将额度状态持久化或在驱逐时保存可恢复的累计值；不要把权威消费计数当作可无损丢弃的缓存。对第 65 个会话和被驱逐的已耗尽会话补充回归覆盖。

## B2-04 [P1] 任务来源授权忽略账号，跨账号读取、取消及汇总结果

**主要位置：** `crates/agent/src/task_manager.rs:102`；上下文构造 `runtime/tool_pipeline.rs:1197`；自动汇总 `runtime/process_message_inner.rs:2248`、`:2304`。

**触发前提：** 同一服务、同一 agent 接入同渠道的两个账号，两账号接收相同 `chat_id` 的消息。例如同一 Telegram 用户分别私聊两个已配置 bot，或同一 Discord 频道接入两个 bot；会话之间本应按 account_id 区分。

**证据类型：静态跨模块调用链，与 B4 共同核对入口。**

1. `channels/account.rs:208-225` 会为 Telegram 各账号分别启动配置，`telegram.rs:407-411` 保留各自 `account_id`，`chat_id` 原样使用 `message.chat.id`。相同用户私聊不同 bot 的 chat ID 相同。
2. `core/message.rs:21-31` 明确将 account ID 编入 `session_key`，两条消息的正常持久会话 key 不同。
3. TaskInfo 已保存 `origin_account_id` / `origin_session_key`（`task_manager.rs:65-68`），但 `belongs_to_origin` 只比较 channel/chat ID。`OriginScopedTaskManager`（`:1373-1392`）及工具上下文构造同样没有 account 参数，导致 `list_tasks` / `cancel_task` 通过该弱匹配访问其他账号任务。
4. `/tasks` 列表和取消的 `find_task_by_prefix_for_origin` 也使用同一匹配（`:896-905`、`:985-995`；CLI handler `bin/blockcell/src/commands/slash_commands/handlers/tasks.rs:161`）。
5. 主循环在 `process_message_inner.rs:2248` 查询匹配的 running typed task；若集合非空且本会话预算未耗尽，会等待这些任务，并在 `:2304-2348` 将尚未注入的完成结果加入本次模型上下文。另一账号的 running typed task 也能触发这条等待/汇总路径，完成后混入其结果，无需模型先猜到任务 ID。只有已完成任务、没有任何匹配的 running typed task 时，不会单独触发此自动汇总分支。

**实际行为与影响：** 本应属于独立账号会话的任务描述/结果可相互可见，任务可被另一会话取消，后台结果也可自动串入错误账号的回答。账号信息已到达数据层，却在授权比较中丢失。

**修复方向：** 统一按 canonical origin session key（并保留 agent 维度）查询、授权与汇总，不再仅用 channel/chat ID。创建主消息任务时也填入 origin account/session 信息；补充同 channel/chat、不同 account 的列表、取消、续跑和自动汇总隔离测试。检查点当前也仅按 channel/chat 授权，修复时需一并核对。

## 验证与覆盖限制

- 本批仅运行上述临时纯函数与人工文件 symlink 检查，未修改业务代码或仓库测试，未运行 cargo（由主审统一执行）。
- 未调用实际模型、真实 IM 或后台定时整合，未对 Linux/macOS 原生沙箱做整应用联调。
- 对取消和消息派生做了调用链审查；未做高并发取消/进程压力测试，不能据此宣称所有取消路径无缺陷。
- 记忆七层、ghost 进化、响应缓存全部实现并未逐行穷尽；实际覆盖以本报告开头列出的文件和函数为准。
- 技能脚本批准缓存按 session/skill/path/content hash 复用，现有测试明确覆盖复用；本批未将“参数变化仍复用批准”自行判为缺陷，因为需先明确产品批准范围契约。
