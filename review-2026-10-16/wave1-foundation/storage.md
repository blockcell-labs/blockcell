# storage Review 报告（Wave 1 · 基础层）

> 只读审查 · 2026-10-16 · 子代理 w1-storage

## 规模（文件数/LOC/入口）

- 文件数：27 个 Rust 源文件 + `Cargo.toml` + `tests/knowledge_index.rs`
- LOC：约 12,013 行（src 7,866 + memory 子模块 3,904 + 集成测试 243；其中 `memory/tests.rs` 独占 1,733 行）
- 入口：`crates/storage/src/lib.rs`（34 行，纯 re-export）；各 store 的构造入口为 `MemoryStore::open_with_options`、`GhostLedger::open`、`KnowledgeIndex::open`、`EvolutionWorkflowStore::open`、`RabitqIndex::open_or_create`、`SessionStore::new`、`AuditLogger::new`、`ChannelContacts::new`、`JsonlSessionEventLog::new`
- 依赖：rusqlite（bundled，workspace 统一版本）+ rabitq_rs + fs2 + sha2 + uuid + chrono；无 tokio/async 代码（纯同步库）

## 模块地图（表结构/核心仓储 API/被谁依赖）

**SQLite 存储（5 个独立 DB，均 `Arc<Mutex<Connection>>` 串行化）**
- `memory/`（memory.db）：`memory_items`（FTS5 trigram 外部内容表 + ai/ad/au 触发器同步 + 部分唯一索引防 dedup 竞态）、`memory_vector_queue`（向量同步意图队列）、`memory_meta`。API：upsert/query/soft_delete/restore/batch_soft_delete/maintenance/retry_vector_sync/reindex。
- `ghost_ledger.rs`（ghost_ledger.db）：episodes/episode_sources/review_runs/review_checkpoints/dead_letters，review lease 三件套（claim/heartbeat/cleanup）。被 agent runtime 的 Ghost 学习、knowledge_forget 使用。
- `knowledge_index.rs`（knowledge_index.db）：knowledge_entries + knowledge_fts + knowledge_files（内容 hash 增量索引）+ knowledge_vectors + forgotten（tombstone）。被 knowledge_forget、runtime wiring 使用。
- `evolution_workflow.rs`：evo_workflows/steps/events，lease claim + 指数退避重试。
- `rabitq_index.rs`（vectors.sqlite + index.bin）：向量 ANN，SQLite 行为事实源、index.bin 为可重建缓存；filter 查询走暴力精确搜索。

**文件存储（fs2 跨进程锁 + 原子 rename）**
- `session.rs`（sessions/*.jsonl，metadata 行 + 消息行）、`session_event.rs`（session_events/*.jsonl sidecar，seq 单调）、`audit.rs`（audit/YYYY-MM-DD.jsonl，SHA-256 哈希链）、`contacts.rs`（channel_contacts.json）、`file_lock.rs`（共享的锁与原子写基础设施）。

**派生物**
- `session_metrics.rs` / `session_projection.rs`（事件→指标/transcript 投影与一致性校验）、`retriever.rs`（FTS+向量 RRF 混合召回）、`memory_service.rs` + `memory_contract.rs`（写入规范化层）。

被依赖方：crates/agent（runtime/ghost/learning）、crates/tools（memory/knowledge_forget/memory_maintenance）、bin/blockcell CLI。

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| P1 | crates/storage/src/audit.rs:16, 332-375 | 审计哈希链无跨进程互斥，链完整性可被并发进程破坏 | `AUDIT_WRITE_LOCK` 是进程内 `Lazy<Mutex>`；gateway(axum) 与 CLI 可同时追加同一 `audit/<date>.jsonl`。两进程各自 `recover_prev_hash()` 读到同一末行 → 两条记录 `prev_hash` 相同 → `verify_chain` 报 prev_hash mismatch。已有测试 `concurrent_logger_instances_preserve_chain` 只覆盖同进程多线程。且 `write_event` 无 fsync，崩溃即丢尾行 |
| P1 | crates/storage/src/ghost_ledger.rs:211-232 | `merge_episode_metadata` 读-改-写跨两次锁获取，存在丢失更新 | `get_episode()`（持锁）与 `UPDATE`（重新持锁）之间，其他线程/其他 GhostLedger 实例可并发改 metadata，后写覆盖先写。应放入单事务并用 updated_at 乐观校验或 json_patch |
| P1 | crates/tools/src/knowledge_forget.rs:82（storage API 使用方） | 阻塞 SQLite 调用直接位于 async fn 内 | 工具 `execute` 为 `async fn`，其中直接 `KnowledgeIndex::open` / `GhostLedger::open` 并同步执行 SQLite 与文件操作，无 `spawn_blocking`。storage 本身无 async（已检查），但阻塞调用混入 tokio 上下文会卡住 worker。agent crate 有 7 处 spawn_blocking 可参照 |
| P2 | crates/storage/src/memory/schema.rs:82-97 | 每次开库都执行全表 UPDATE+GROUP BY 去重迁移 | `open_with_options` 无条件跑 `UPDATE memory_items SET dedup_key=NULL WHERE ... rowid NOT IN (SELECT MAX(rowid)...GROUP BY dedup_key, session_key)`，大表下每次启动都有 O(N) 扫表+排序。应记 `memory_meta` 迁移版本号后跳过 |
| P2 | crates/storage/src/knowledge_index.rs:241-260 | 每次 search 全量加载 supersedes 与 forgotten 两张表 | 两个无 LIMIT 的 `SELECT` 装入 HashSet，随遗忘/tombstone 增长线性变贵；应改为 JOIN 或 `WHERE supersedes IN (...)` 子查询 |
| P2 | crates/storage/src/memory/query.rs:335-359, 423-442 | N+1 查询模式 | `load_items_by_ids` 按 id 逐条 `SELECT`（retriever 每次搜索及向量候选校验各调一次，retriever.rs:124-126）；`record_accesses` 对 top_k（≤50）条结果逐条 UPDATE。应改 `IN (...)` 分块与单条 `UPDATE ... WHERE id IN`/`executemany` |
| P2 | crates/storage/src/audit.rs:35-53, 102-111 | 工具调用参数/结果全量明文落盘，无脱敏 | `ToolCall{params,result}` 原样序列化进审计 JSONL；工具参数可能含凭据（API key、token、env）。需评估 redaction 策略或审计目录权限收敛 |
| P2 | crates/storage/src/rabitq_index.rs:47 | 唯一未设置 WAL/busy_timeout 的 SQLite 存储 | 其余 4 个 store 均 `PRAGMA journal_mode=WAL; busy_timeout=5000`；此处裸 `Connection::open`，多组件并发打开（wiring/reindex）时立即 SQLITE_BUSY 而非等待 |
| P2 | crates/storage/src/memory/crud.rs:183 | 用错误消息字符串判断"记录不存在" | `msg.contains("QueryReturnedNoRows")` 依赖 rusqlite 错误文案；同文件其他处均用 `OptionalExtension`。文案变化会静默改变语义 |
| P2 | crates/storage/src/fts.rs:97-99 | FTS schema 探测靠 DDL 子串匹配，误判会反复删表重建 | `contains("tokenize='trigram'")` 对空格/引号变体（`tokenize = 'trigram'`）不匹配 → 每次开库 DROP TABLE + rebuild。建议以 `PRAGMA user_version` 或 memory_meta 版本号判定 |
| P2 | 全局（schema.rs / ghost_ledger.rs:955-976 / rabitq_index.rs:508-514） | 无版本化迁移框架，schema 演进靠幂等 DDL + 错误字符串嗅探 | ghost_ledger 靠 `"duplicate column name"` 文案；memory 的 `memory_meta` 注释写 "Migration tracking" 但从不写版本行；三处 store 三种迁移风格。随 schema 增长易漏迁 |
| P3 | crates/storage/src/memory_service.rs:25 | 存储层接受 long_term，与文档"memory_upsert 只接受 short_term"存在纵深缺口 | docs/05_memory_system.md:151 声明只接受 short_term；实际拦截在 tools 层（crates/tools/src/memory.rs:461），storage 层仍可写 long_term。建议存储层一并拒绝 |
| P3 | crates/storage/src/session.rs:483 | `_meta.json` 用 `std::fs::write` 非原子写 | 持锁但崩溃可截断文件（仅丢会话显示名）；同模块已有 `write_atomically` 可复用 |
| P3 | crates/storage/src/evolution_workflow.rs:84 | `open` 未创建父目录 | 其他 store 均 `create_dir_all(parent)`；此处父目录缺失会直接打开失败 |
| P3 | crates/storage/src/memory/maintenance.rs:207-216 | 块内 `return Ok(0)` 直接从函数返回 | 语义正确（空 ids → 0）但控制流隐晦，易误读为块返回值 |
| P3 | crates/storage/src/memory.rs:164, ghost_ledger.rs:14, evolution_workflow.rs:65 | `db_path` 字段 `#[allow(dead_code)]` 长期留存 | 三处 store 同模式；如无计划使用应删除 |
| P3 | crates/storage/src/memory.rs:330 | `and_hms_opt(0,0,0).unwrap()` | 实际不可失败（NaiveDate 恒定有零点），但可用 `expect` 或 `unwrap_or` 消警报 |

## 亮点

- **并发模型统一且安全**：所有 SQLite store 均为 `Arc<Mutex<Connection>>` 单连接串行化 + WAL + busy_timeout，避免 rusqlite 连接池的锁竞争/死锁；RabbitQIndex 的 db/state 双 Mutex 锁序经过核对无嵌套获取，无死锁路径。
- **租约语义扎实**：ghost_ledger 与 evolution_workflow 的 claim 全部采用 Immediate 事务 + 条件 UPDATE（`affected==1` 才算赢），lease heartbeat/cleanup 闭环；`claim_next` 的 `lease_until < ?3`（now）参数顺序经核对正确。
- **SQL 注入防护到位**：所有用户值均参数化；动态标识符有白名单（`latest_episode_field`）或清洗（`sanitize_identifier`）；FTS 查询经 `build_fts_query` 转义引号。
- **文件写入工程化**：统一 fs2 sidecar 锁 + temp+rename 原子写 + fsync + 权限继承；锁文件不 unlink 的 inode 互斥 reasoning 有文档和测试背书。
- **测试覆盖厚**：memory 1,733 行测试（含向量队列失败重放、dedup 唯一索引、迁移路径）、audit 哈希链含篡改/删行/畸形行/并发写入检测、session 含跨线程锁等待测试、knowledge_index 有独立集成测试。
- **与 docs/05 契约基本一致**：trigram 启动自动重建、两阶段 knowledge_forget + tombstone 防复活、migrate-canonical 幂等、DB 文件命名（memory.db / knowledge_index.db / ghost_ledger.db）均与文档相符。

## 建议（不执行的后续动作）

1. 给 audit 写路径加 fs2 跨进程文件锁（复用 `file_lock::lock_exclusive`）并在持锁内 recover+append+fsync，将链断裂窗口从进程级缩到无；补充多进程 chain 测试。
2. `merge_episode_metadata` 改为单事务内 `json_patch` 或以 `updated_at` 为版本的 CAS。
3. tools 层 async fn 中的 storage 调用统一 `tokio::task::spawn_blocking`（可先在 knowledge_forget 与 memory 工具试点，主代理统筹）。
4. 引入轻量迁移版本机制（`PRAGMA user_version` 或 memory_meta 版本行），替换三处 ad-hoc 迁移；schema 大表去重 UPDATE 加版本跳过。
5. 性能修补：`load_items_by_ids` 改 `IN` 分块、`record_accesses` 合批、knowledge_index search 的 supersedes/forgotten 改子查询、rabitq `open_or_create` 补 WAL+busy_timeout pragma。
6. 评估审计日志中工具 params/result 的凭据脱敏或目录权限策略。
7. `get_by_id` 的字符串嗅探改 `OptionalExtension`；`prepare_trigram_fts` 改版本号判定 FTS 重建。

## 维度核对说明

- ① 正确性：边界条件（空 ids、top_k=0、空向量）均有 guard；unwrap/expect 在非测试代码仅 memory.rs:330 一处且实际安全；无 panic! 路径；SQL 全部参数化；事务边界普遍正确（Immediate + 条件 UPDATE），问题见上表。
- ② 并发：进程内串行化设计正确；跨进程仅 audit 链与（潜在）多 GhostLedger 实例的 merge 存在缺口；阻塞调用混入 async 的问题在使用方（tools）。
- ③ 安全：DB 路径均来自 `Paths`（配置基目录），无用户可控路径穿越；凭据落盘风险见 audit P2。
- ④ 性能：索引覆盖主要查询路径（scope/type/deleted/expires/dedup/importance、知识库 file/hash、Ghost status/boundary/subject）；N+1 与全表加载问题见上表。
- ⑤ 测试覆盖：storage 自身覆盖良好（含并发、哈希链、迁移、锁语义）；缺 DB 损坏/锁超时/rabitq 场景测试。
- ⑥ 可维护性：schema 迁移管理是主要欠债（P2）；死代码仅 3 个 allow(dead_code) 字段。
- ⑦ 契约一致性：与 docs/05_memory_system.md 逐条核对，仅 memory_upsert scope 纵深拦截一处偏差（P3）。
