# scheduler Review 报告（Wave 3 · 编排层）

> 只读审查 · 2026-10-16 · 子代理 w3-scheduler

## 规模

| 指标 | 数值 |
|---|---|
| 文件数 | 17（`crates/scheduler/src/**/*.rs`） |
| 总 LOC | 6,549 |
| 测试数 | ≈54（cron_service 10 · consolidator/tests 30 · dream_service 7 · ghost 5 · evolution×2 各 1 · 其余 0） |
| 依赖特点 | 直接依赖 blockcell-core/agent/tools/providers/skills/storage + tokio/chrono/chrono-tz/cron 0.12；自身**不直接**依赖 rusqlite（经 blockcell-storage） |

## 模块地图

| 文件 | LOC | 职责 |
|---|---|---|
| `src/cron_service.rs` | 1,297 | 核心：`CronService` 调度循环（1s tick 可配）、cron/at/every 三种调度、JSON 存储（merge-save + ExclusiveFileLock + atomic_write）、任务派发（InboundMessage + 完成回执 + 15min 超时）、事件发射 |
| `src/job.rs` | 96 | 数据模型 `CronJob/JobSchedule/JobPayload/JobState`（camelCase serde） |
| `src/heartbeat.rs` | 129 | HEARTBEAT.md 30min 周期触发（空文件跳过） |
| `src/dream_service.rs` | 278 | Layer6 梦境 10min 门控检查循环（时间/会话/锁三重门控） |
| `src/consolidator.rs` + 8 子模块 | 774+2,033 | 记忆整合：gather 信号提取、staging 事务提交/回滚/崩溃恢复、门控检查、prune、锁、forged agent 整合 |
| `src/ghost.rs` | 456 | Ghost 维护 cron 循环（60s 检查粒度）、配置 mtime 热重载、每日限额 |
| `src/evolution_workflow.rs` 等 | — | — |
| `src/evolution_worker.rs` | 568 | CoreEvolution 工作流 worker（claim/lease/心跳/retry） |
| `src/skill_evolution_worker.rs` | 587 | Skill evolution 工作流 worker（claim/lease/观察期） |

## 问题清单

| 级别 | 位置 | 问题 | 证据/说明 |
|---|---|---|---|
| P0 | — | 未发现 P0（无数据损坏/安全漏洞/崩溃级确定性 bug） | 见下方分维度说明 |
| P1 | `src/cron_service.rs:417-502`（`run_tick`） | **长任务串行阻塞调度器与写 API**：`run_tick` 全程持有 `operation_lock`，对每个到期任务顺序 `await execute_job_internal`，而该函数要等待运行时回执（生产超时 15min）。单任务最长 15 分钟内：① 后续到期任务全部延迟（MissedTickBehavior::Skip 直接丢弃期间的 tick）；② gateway 的 `POST/DELETE /v1/cron`、`add_job/remove_job` 全部挂起等锁（HTTP handler 最多挂 15min）；③ N 个任务同时到期延迟叠加（理论上 N×15min）。任务间无并发/隔离设计 | `cron_service.rs:418` 持锁 → `:494-500` 循环 await（内含 `:710` `timeout(15min, receipt_rx)`）；`add_job:257`/`remove_job:277` 等锁 |
| P1 | `src/evolution_worker.rs:123-390`、`src/skill_evolution_worker.rs:142-319`、心跳任务 `:477-513`/`:511-544` | **阻塞 rusqlite 调用直接置于 async 上下文**，违反全仓不变量：`EvolutionWorkflowStore` 方法（`claim_next/renew_lease/recover_expired_leases/insert_step/...`）均为同步 `pub fn`，内部 `std::sync::Mutex<Connection>` + rusqlite，`busy_timeout=5000`。这些调用在 `async fn tick()` 和 `tokio::spawn` 的心跳任务中被直接调用，全 crate 无一处 `spawn_blocking` 包装。DB 被其他进程写锁竞争时可阻塞 tokio worker 线程最多 5s，拖停轮询/租约续期 | storage 侧 `evolution_workflow.rs:63` `Arc<Mutex<Connection>>`、`:195/269/307` 同步签名；scheduler 侧 `:123` `self.store.claim_next(...)` 无 `.await` 修饰的阻塞调用 |
| P1 | `src/consolidator.rs:191`、`src/consolidator/dream.rs:147,180` | **阻塞型跨进程锁获取放在 async 上下文**：`CrossProcessLock::acquire` 内部是 `std::thread::sleep(200ms)×30` 重试循环（最坏阻塞 ~6s），在 `DreamState::save`（async）和 `dream()` 最终状态保存路径中同步调用，阻塞 worker 线程。同类 `ExclusiveFileLock::acquire` 在 cron save 路径就正确地用了 `spawn_blocking`（`cron_service.rs:188`），两处 barrel 不一致 | `learning/cursor.rs:59-101` 循环 sleep；`consolidator.rs:191` 直接调用；`dream.rs:147/180` 直接调用 |
| P1 | `src/cron_service.rs:777-834`（`calculate_next_cron_run_ms`/`calculate_next_run`）及其 `tests` 模块（:875-1297） | **cron 时间计算零单测**：时区解析（`parse_timezone`）、6 字段表达式解析、`schedule.upcoming(tz)` 下次执行计算、DST 边界、`merge_load` 状态保留，全部无直接测试。现有 10 个测试只覆盖事件/回执/delete_after_run/并发 add。这是本 crate 最核心也最易回归的逻辑（时区/DST/表达字段），却是测试空白 | `cron_service.rs` tests 中无 `calculate_next_cron_run_ms`、无 tz/DST 用例 |
| P2 | `src/cron_service.rs:441-445,546-571` | **At 任务失败重试无退避**：失败后 `next_run` 保持过去时间且 job 保持 enabled，每个 tick（默认 1s）重新完整派发一次；若运行时快速失败（如 provider 报错），形成每秒重试热点，每次还可能触发一次 LLM 调用。测试 `test_failed_due_at_runtime_execution_remains_retryable` 固化了"保持可重试"但无退避设计 | Every/Cron 失败路径会自然顺延（`:524-526,552-556`），唯 At 无 |
| P2 | `src/cron_service.rs:813` | `job.state.next_run_at_ms = Some(now_ms + every_ms)` 未用 `checked_add`（同函数 `:526/:555` 均用 checked_add）：debug 构建溢出 panic 可杀死该 agent 调度循环；release 下缠绕为负值/None，使任务立即假死（既不到期也不再初始化，见 `:526` None → `:469` 重初始化只认 `run_immediately`）。入口 `every_ms` 来自工具/网关，无上卫 | 与 `:526` `checked_add` 的直接不一致 |
| P2 | `src/cron_service.rs:836-872` | **panic 隔离缺失**：`run_loop` 由 `tokio::spawn` 运行（gateway.rs:1223、agent.rs:573 裸 spawn），`run_tick`/`execute_job_internal` 无 `catch_unwind`。任何任务 panic（含第三方 `EventEmitterHandle::emit` 回调 panic）会静默杀死该 agent 的整个 cron 循环——`JoinHandle` 无人 await、不重启、仅表现为"定时任务永远不再触发" | `run_loop` 内 `select!` 直接 `self.run_tick().await`，无 panic 边界 |
| P2 | `src/ghost.rs:254,343` | Ghost 的 `next_scheduled = schedule.upcoming(Utc)` 按 **UTC** 计算下次触发，忽略 `config.default_timezone`；而 cron 任务体系按 tz 运行。同一仓库两套调度时区语义不一致，Ghost config 的 schedule 实际含义与用户时区直觉不符 | `:254`、`:343` 均为 `schedule.upcoming(Utc)` |
| P2 | `src/ghost.rs:44-74,176-217` | `SyncTracker` 每日计数仅存内存不持久化：进程重启即重置 `max_syncs_per_day` 限额；且 `run_routine` 在 send 前即 `record_sync`，send 失败也消耗配额；3 次连续 send 失败直接 `break` 永久停服（有 error 日志但无自恢复/重启机制） | `:186` record_sync 先于 `:210` send |
| P2 | `src/cron_service.rs:301-346` + 交叉面 | **`update_job_enabled`（enable/disable API）全 workspace 无调用方**（WebUI 之外的死代码）；且 job ID 语义三套并存：CLI `cron list` 显示 8 位前缀、CronTool remove 按前缀匹配、`update_job_enabled` 按前缀、gateway `DELETE /v1/cron/:id` 与 `POST /v1/cron/:id/run` 要求完整 ID。用户从 list 复制前缀去 delete 得到 not_found | grep 全仓：`update_job_enabled` 仅定义处出现；`commands/cron.rs:72` 只打印前 8 位 |
| P3 | `src/consolidator/gather.rs:71-90` | `/proc/{pid}` 存活检查在 macOS（`#[cfg(unix)]`）上恒 false，静默落入 `is_journal_started_at_expired` 兜底——行为保守安全，但注释声称的"Unix 检查"在非 Linux 平台是死代码，易误导后续维护者；另外 `:51` `journal_path.exists()` 为 async 中的同步 IO（小文件，影响微） | `:74` `format!("/proc/{}", pid)` |
| P3 | `src/cron_service.rs:686-710` | 崩溃重复触发窗口：`inbound_tx.send` 成功、agent 已受理之后，若进程在 `next_run` 推进/落盘前宕机，重启后 `next_run<=now` 会再次派发（at-least-once）。语义可接受但无文档声明，且无幂等/去重键 | `:483-486` 只在 state 初始化时预保存，完成态在 `:508-582` 才落盘 |
| P3 | `src/evolution_worker.rs:56-68` | `run_with_lease_guard` 的 `select!` 两个分支都返回 `None`：watch 值变更为 false 的良性变更也会取消步骤 future。当前 `lost_tx` 只会发送 true 或被 drop，无实际影响，但结构冗余且脆弱（未来若发送 false 会误取消） | `:59-68` if/else 均 `None` |
| P3 | `src/heartbeat.rs:39-40` | `is_heartbeat_empty` 在 async `trigger()` 中读文件用小同步 `std::fs`（30 分钟一次，影响可忽略）；同类小问题散布于 `gather.rs:51`、`ghost.rs:264` | — |

## 分维度结论

- **① 正确性**：核心 bug 未发现。停机补跑语义正确（`next_run<=now` 立即补跑一次、不累计风暴，`cron_service.rs:467-468`）；`save/merge` 三方合并（disk/memory + added/deleted/persisted 集合）+ mtime 短路设计扎实，跨进程（CronTool 直写盘）合并正确；`merge_load` 显式保留 in-memory `next_run/last_run` 避免了旧 `load()` 覆盖 bug。**但 cron 表达式与时区/DST 计算路径（`upcoming(tz)`）零单测**（P1），At 失败无退避（P2）、`:813` 算术不一致（P2）。
- **② 并发**：任务 panic 不能杀死调度器的目标**未达成**（P2，裸 spawn 无 catch_unwind）；长任务阻塞是确定的设计缺陷（P1：串行 + operation_lock 全程持有波及 API）；rusqlite/CrossProcessLock 阻塞进 async（P1×2）。事件 emitter 用 `lock().ok()` 防中毒是可取点。
- **③ 安全**：scheduler 自身不执行 shell/脚本，payload `message`/`skill_name` 仅封装为 `InboundMessage` 交既有 runtime 分发，注入面收敛在 runtime（超范围）。`ghost.rs` 的 GHOST_SYSTEM_PROMPT 使用 `#[allow(dead_code)]` 悬置（可维护性）。已检查未发现 scheduler 范围内的注入漏洞。
- **④ 性能**：1s tick + mtime 短路 + 全部 5 个 loop 一致 `MissedTickBehavior::Skip`，正常负载无轮询风暴；问题集中在到期任务的 15min 串行化（P1）和 At 失败 1s 重试热点（P2）。
- **⑤ 测试覆盖**：consolidator staging 事务（提交/冲突/回滚/崩溃恢复/stale 锁）覆盖优秀（≈30 用例）；**cron 时间计算与时区完全空白**（P1）；evolution/dream_service 的 db/llm 主路径基本无集成测试（仅 1 个 lease guard 单测）。
- **⑥ 可维护性**：模块拆分清晰（consolidator 子模块化、job.rs 数据模型独立）、错误类型 `DreamError` 完备、中文设计注释质量高、"失败不用裸 `?` 早退"等不变量有显式注释。扣分项：`update_job_enabled` 死代码、`:813` 与兄弟路径算术风格不一致、`/proc` 跨平台误导注释。
- **⑦ 契约一致性**：CLI 仅 `cron list`（与 docs/17_cli_reference.md:483-497 一致）；**gateway 两条创建路径契约分叉**（见建议第 1 条，P1 级但位置超范围）；job ID 前缀 vs 完整 ID 语义不统一（P2）。

## 亮点

1. **cron 持久化的 merge-save 设计**（`cron_service.rs:175-254`）：`ExclusiveFileLock` + `spawn_blocking(atomic_write)`，保存时读盘合并、按 `added/deleted/persisted` 集合排序决策，正确处理了"进程内改动 vs 磁盘改动"双向合并，且写路径全程不阻塞 async executor。
2. **`merge_load` 的状态保留修复**（`:354-415`）：注释明确记录了旧 `load()` 全量覆盖导致 `next_run` 丢失/重复触发的历史 bug，修复策略（按 ID 保留 in-memory 执行态）正确，配套 mtime 短路。
3. **完成回执机制**：任务"完成"以运行时回执为准（`register_message_receipt` + 15min 超时 + `cancel_message_receipt`），而非入队即成功，杜绝"假完成"；事件链 started/completed/failed × Critical 优先级完备。
4. **consolidator 的 staging 事务是数据安全标杆**：pre/post 指纹快照 + `current==pre` 并发冲突检测（`staging.rs:264`）+ 备份 manifest + 逐文件原子写失败回滚 + 崩溃后 `recover_dream_commit_backups` + `is_consolidating` stale 自动清除，每个失败分支都有测试。
5. **停机/错过语义**三类调度都合理：At 补跑一次、Every/Cron 到点即跑不补跑风暴；tick `Skip` 策略全 crate 一致，服务 pauses 不会积压触发。
6. **Ghost config 热重载**按 mtime 短路 + 5 字段 cron 归一化（比 `CronService` 的原样解析更宽容，可作为后者的参照）。

## 建议

1. **（超范围，P1 级）gateway `POST /v1/cron` 应立即对齐 CronTool 的校验**：`bin/blockcell/src/commands/gateway/cron.rs:189-197` 接受任意 `cron_expr` 且不支持 `tz`，而 `crates/tools/src/cron.rs:122-141` 严格校验 6 字段表达式与 IANA tz。非法/5 字段表达式经网关静默落盘后永不触发（仅每 tick error 日志）。同时建议补齐网关侧 `every_seconds`/@tz 校验与 `tz` 字段。
2. **（超范围）`crates/tools/src/cron.rs:116` `every * 1000` 未做 checked 乘法**，与 scheduler `:813` 共同构成算术不一致面；建议统一 `checked_mul(1000)` + 上限校验（网关 `build_every_schedule` 的 checked 写法可作模板）。
3. **（超范围）`gateway/cron.rs:58` `paths.skills_dir().join(skill_name)` 未拒绝 `..` 路径组件**，可探测 skills 目录之外的路径存在性（low impact，但建议与 `staging.rs:17-26` 的路径遍历卫同款处理）。
4. **cron 计算补测（最高优先级）**：`calculate_next_cron_run_ms` 至少覆盖——① 标准 6 字段表达式下次时间；② 含 tz 时 DST 前后切换（如 `America/New_York` 的 02:30 春凌晨、秋凌晨）；③ 无效表达式/无效 tz 返回 None；④ `merge_load` 对 in-memory next_run 的保留与外盘新增 job 的拾取。
5. **长任务隔离**：将 `execute_job_internal` 从 `run_tick` 的锁内串行 await 中解耦（如 spawn 任务 + 完成后回写状态，或至少把"等回执"移出 `operation_lock`），并给 `add/remove/enable` 一个独立的短临界区，避免 API 挂起。
6. **阻塞调用治理**：对 `EvolutionWorkflowStore` 的同步方法统一包 `spawn_blocking`（或在 storage 侧提供 async API），`CrossProcessLock::acquire` 同样处理；可加 CI 检查（如 `tokio::task::block_in_place` 白名单）防止回潮。
7. **防回归小项**：`:813` 改 `checked_add` 并对 None 显式告警；给 `run_loop` 的 spawn 加 `catch_unwind` + 重启或 supervised restart；At 任务失败重试加指数退避（或复用 `schedule_retry_if_owned` 的 backoff 思路）；清理 `update_job_enabled` 或接一个真实入口（并统一 job ID 语义为"完整 ID 或显式前缀参数"）。

---
报告完。
