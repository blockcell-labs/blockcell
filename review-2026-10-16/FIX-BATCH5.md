# Batch 5 修复清单（async 纪律）

> 基线 HEAD `d08b0cc`（Batch 0-4 已提交）· 开始 2026-10-16 · 来源：`review-2026-10-16/summary.md` §3-F + §7 Batch 5
> 状态标记：⬜ 待办 / 🔄 进行中 / ✅ 完成 / ⏸️ 阻塞 / ⏭️ 有意不做（附理由）

## 背景

Batch 5 的目标是把**阻塞原语赶出 async 上下文**：rusqlite 同步调用与跨进程文件锁
（`thread::sleep` 重试）直接跑在 `async fn` 里时，会占用 tokio worker 线程直到锁可用
（SQLite `busy_timeout` 最多 5s、`CrossProcessLock` 最坏 ~6s），拖停同一 executor 上的
无关任务。本批只做「出 async」这一件事，不改业务语义。

## 修复项

| # | 项 | 位置 | 状态 | 改动摘要 |
|---|---|---|---|---|
| 5.1 | `knowledge_graph` 的 rusqlite 阻塞跑在 async；自开连接无 WAL/busy_timeout | `crates/tools/src/knowledge_graph.rs` | ✅ | `execute` 整体包 `spawn_blocking`；`open` 后补 `PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000`——该工具不走 storage crate，需自行复现其并发卫生 |
| 5.2 | `knowledge_forget` 的 index 重建 / SQLite 写入 / 文件删除跑在 async | `crates/tools/src/knowledge_forget.rs` | ✅ | `execute` 整体包 `spawn_blocking`（`collect_matches` 会全量扫描知识目录，属阻塞重活） |
| 5.3 | `EvolutionWorkflowStore` 同步 SQL 跑在 async（两个 worker + 心跳任务） | `crates/scheduler/src/blocking.rs`（新增）、`evolution_worker.rs`、`skill_evolution_worker.rs` | ✅ | 新增 `blocking` 模块，为 store 的同步方法提供 async 包装；`tick` / 心跳 / `determine_next_step` / `cancelled` / `recover` / `enqueue_pending_records` / `finalize_observation_workflows` 全部改 `await` |
| 5.4 | `CrossProcessLock::acquire`（`thread::sleep` 重试最坏 ~6s）跑在 async | `crates/scheduler/src/consolidator.rs`、`consolidator/dream.rs` | ✅ | 改走 `run_blocking_io`，与 cron save 路径既有的 `spawn_blocking` 纪律一致 |
| 5.5 | `cron run_tick` 全程持 `operation_lock` 顺序 `await execute_job_internal`（15min 回执） | `crates/scheduler/src/cron_service.rs` | ✅ | 锁只护「决策 + 落盘」与「提交结果」两个短临界区，投递阶段显式 `drop(operation_guard)`；新增回归测试 |
| 5.6 | `memory_manage` 的 canonical 文件写入（快照 + 原子写 + fsync）跑在 async | `crates/tools/src/memory.rs` | ✅ | 改走同文件的 `run_store_blocking`（P2 同族，顺手收敛） |

## 实现要点

- **`blocking` 模块的形态**：`run_blocking`（`blockcell_core::Result`）镜像
  `blockcell_tools::memory::run_store_blocking`；`run_blocking_io`（`std::io::Result`）供
  跨进程锁使用。store 包装函数一律「克隆 `Arc` 句柄 + 参数转 owned 后移进闭包」，
  调用方保持 `Ok(true)/Ok(false)/Err` 的三态语义不变（`JoinError` 映射为 `Error::Other`）。
- **5.5 的锁语义**：投递阶段释放 `operation_lock` 后，`add_job` / `remove_job` /
  `update_job_enabled` / `save` 可在回执等待期间正常执行；提交阶段重新加锁，
  按 `job_id` 在最新的 `self.jobs` 里定位（作业期间被删除的 job 直接跳过）。
  唯一生产调用方 `run_loop` 顺序 `await run_tick`，不存在并发 tick，故不会重复投递。
- **反证**：临时注释 `drop(operation_guard)` 后
  `test_run_tick_releases_operation_lock_during_job_delivery` 失败（`add_job` 500ms 超时），
  确认回归测试真的锁住了该行为。

## 验证

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| V1 | `cargo test -p blockcell-tools -p blockcell-scheduler -p blockcell-storage` | ✅ | exit 0：scheduler **58** / storage **84+6** / tools **487+8** passed，0 failed |
| V2 | `cargo clippy`（tools / scheduler） | ✅ | exit 0；改动文件零新增警告（`data_process.rs:791` MSRV 与 `evolution_worker.rs:64` `if_same_then_else` 均为既有告警） |
| V3 | `cargo check --workspace` | ✅ | exit 0（bin crate 依赖 scheduler，一并覆盖） |
| V4 | `cargo fmt --all --check`（改动文件） | ✅ | 改动文件 fmt 干净（仓内其余 31 处 diff 为既有） |

## 提交

| # | 项 | 状态 | 结果 |
|---|---|---|---|
| G1 | 提交 A：tools（5.1 / 5.2 / 5.6） | ✅ | `a0cf2d5`（3 文件） |
| G2 | 提交 B：scheduler（5.3 / 5.4 / 5.5） | ✅ | `887eeec`（7 文件） |
| G3 | 提交 C：本文件 + summary 状态更新 | ✅ | 见下方 |

## 有意不做 / 留待后续

- **连接缓存**（review §85 建议「knowledge_graph 缓存连接 + `init_schema` 只跑一次」）：
  本批只补 WAL + `busy_timeout`（消除 `SQLITE_BUSY` 失败模式）。缓存连接需要
  「按 db 路径持有长生命周期连接 + 失效/驱逐」设计（工具是 unit struct，且测试会重建 db 文件），
  属性能优化，风险高于收益，留待单独决策。
- **背压类三项**（napcat 每消息 spawn、`stream_subscribe`、WS 帧上限）仍在 Batch 4 延后项中，
  需先定「丢弃 vs 排队」语义。
- **cron 任务间并发**（review §73「长任务隔离」的完整形态：spawn 任务 + 完成回写）本批只做了
  「出锁」，未改成并发投递——后者会改变同一 tick 内多任务的重试/串行语义，需单独设计。

## 中断恢复指引

1. 进度即本文件状态列；⬜ 从第一个未完成项继续。
2. 环境：`unset BASH_ENV`；PATH 用 `/bin:/usr/bin:/sbin:/usr/sbin:/usr/local/bin`（`/usr/bin/ls` 与 `/bin/grep` 均不存在，用 `/bin/ls` 与工具自带 Grep）；cargo 用 rustup toolchain 绝对路径。
3. **踩坑**：`sed -i.bak` 后用 `mv` 还原会保留旧 mtime，cargo 会复用旧产物（表现为
   幽灵告警 / 测试结果与源码不符）。改动文件后如需强制重建，先 `touch` 目标文件。
4. 5.5 的语义注意：`drop(operation_guard)` 必须位于「投递循环之前」，提交阶段需重新
   `let _operation_guard = self.operation_lock.lock().await;`，两处缺一都会破坏并发正确性
   （前者缺失 = 修了等于没修，后者缺失 = 提交与 add/remove 竞争）。
