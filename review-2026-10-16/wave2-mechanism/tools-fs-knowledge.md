# tools 文件/记忆/知识集群 Review 报告（Wave 2 · 机制层）

> 只读审查 · 2026-10-16 · 子代理 w2-fs-knowledge

## 规模

| 模块 | 行数 | 测试行数（约） |
|---|---|---|
| lib.rs（Tool trait / ToolContext / run_store_blocking 宿主） | 598 | 0 |
| fs.rs（read/write/edit/list_dir） | 849 | ~360 |
| file_ops.rs（delete/move/copy/compress/decompress/pdf/info） | 842 | ~80 |
| knowledge_graph.rs（SQLite 知识图谱，14 个 action） | 1407 | ~295 |
| knowledge_forget.rs（跨来源两阶段遗忘） | 568 | ~250 |
| memory.rs（memory_manage/query/upsert/forget） | 1298 | ~600 |
| memory_maintenance.rs（Ghost 记忆园丁） | 710 | ~205 |
| session_recall.rs（缓存/持久化输出召回） | 232 | ~22 |
| session_search.rs（只读会话检索） | 140 | ~70 |
| code_search.rs（grep/glob） | 401 | ~125 |
| html_to_md.rs（Markdown for Agents 工具库，非 Tool） | 322 | ~70 |
| **合计** | **7367** | ~2080（28%） |

辅助核查文件：fs_backend.rs（backend seam）、core/path_policy.rs、agent/runtime/path_security.rs（策略执行点）、docs/03、docs/05。

## 模块地图（文件职责表）

| 文件 | 职责 | 关键依赖 |
|---|---|---|
| fs.rs | read_file / write_file / edit_file / list_dir 四个工具；路径解析（expand_path）与参数语义，文件访问全部经注入的 `FilesystemBackend` seam | fs_backend、office、fuzzy_match |
| file_ops.rs | file_ops 单体工具 8 个 action；含压缩/解压（zip/tar.gz）炸弹防护与符号链接拒绝 | zip/tar/flate2/pdf-extract |
| knowledge_graph.rs | 工作区内 SQLite 知识图谱（entities/relations + FTS5 + 触发器同步），CRUD/BFS 路径/子图/导出 | rusqlite（自开连接） |
| knowledge_forget.rs | 跨来源（canonical 文件 / session 文件 / SQLite 短记）两阶段遗忘：preview→token→confirm→tombstone→GhostLedger 审计 | blockcell_storage::KnowledgeIndex / GhostLedger |
| memory.rs | memory_manage（USER.md/MEMORY.md 文件记忆）、memory_query（FTS）、memory_upsert（仅 short_term）、memory_forget（软删/批删/恢复） | MemoryStoreOps / MemoryFileStoreOps trait 句柄 |
| memory_maintenance.rs | Ghost 园丁：garden/cleanup/stats/compact；含 ghost 日志剪除与启发式 junk 清扫 | MemoryStoreOps |
| session_recall.rs | 按 ref_id 召回响应缓存；按 `tool:{id}` 召回 `.tool_results/{session}` 持久化输出 | response_cache 句柄 + tokio::fs |
| session_search.rs | 后台学习评审用的只读会话搜索（limit 1-20 钳制） | SessionSearchOps 句柄 |
| code_search.rs | grep / glob 两个工具，尊重 .gitignore，输出字节上限 256KB | ignore/regex/glob（spawn_blocking） |
| html_to_md.rs | 公共库（非 Tool）：markdown 内容协商 + htmd 本地转换 + scraper 兜底 | reqwest + ssrf + bounded_io |

## 问题清单

| 级别 | 位置 | 问题 | 证据/说明 |
|---|---|---|---|
| P1 | knowledge_graph.rs:129-167 | rusqlite 阻塞调用直接跑在 async 上下文：`execute` 为 `async fn`，但 `Connection::open`、`init_schema`（每次调用都跑 DDL）、全部 action 的 SQL 与 BFS 遍历均为同步执行，未使用 `spawn_blocking`。违反全仓不变量（storage 为 Arc<Mutex>+WAL，阻塞调用须让路） | 对比：file_ops.rs:186、code_search.rs:228、memory.rs:28 `run_store_blocking` 均正确包裹，唯独本文件与 knowledge_forget 裸跑 |
| P1 | knowledge_forget.rs:82-120,124,238,294 | 同上：`KnowledgeIndex::open/rebuild_from_files`、`record_forgotten_content`、`GhostLedger::insert_episode`（rusqlite）与多文件扫描全部同步跑在 `async fn execute` 中 | collect_matches/confirm_matches/audit_forget 三个函数均无 spawn_blocking；且每次 preview 和 confirm 都全量 `rebuild_from_files` |
| P1 | knowledge_forget.rs:96-106 + lib.rs:574 | 删除类工具未实现 `Tool::requires_confirmation`，运行时 ExplicitConfirmation 准入门（registry.rs:561）对本工具永不触发；preview/confirm 的 token 由 LLM 自己两次调用即可完成"自我批准"。跨来源删除（canonical 文件 + session 文件 + SQLite 短记 + tombstone 防复活）全程无用户点头。返回 JSON 里的 `"requires_confirmation": true`（:96）只是输出字段，不接入准入机制，具有误导性 | 全 tools crate 仅 shell_session.rs:700 覆盖了 requires_confirmation；结合本集群 html_to_md.rs 抓取不可信网页，prompt injection→两连调用删除知识是现实威胁面（影响限本人工作区数据，故不列 P0） |
| P1 | memory.rs:460-473 vs 504-528 | memory_upsert 两处守卫互相矛盾：validate 拒绝 `scope=long_term`（:461），而 ghost channel 分支要求 `scope == "long_term"`（:505）。执行路径上 ghost channel 的任何调用必然被拒（long_term 被 validate 拦、short_term 被 ghost 分支拦），:511-527 的"Ghost 仅允许 fact/preference/project/task 长记"守卫为死代码，文档注释所述能力实际不生效 | ghost channel 的 memory_upsert 100% 失败；死分支 + 误导性错误文案 |
| P1 | memory_maintenance.rs:21,53,193,266 | 跨 session 无差别删除：`prune_ghost_maintenance_logs` 与 `sweep_junk_short_term` 的 `query_json` 均不带 `session_key`，随后用全局 `soft_delete`（非 in_session 变体）删除。启发式 junk_score（阈值 4.0）与"指纹去重留最新"可能误删其他活跃 session 的用户记忆 | 与 memory.rs 中 delete/restore 一律走 `soft_delete_in_session` 的 session 隔离纪律不一致 |
| P1 | knowledge_graph.rs:146-149 | 绕过 storage crate 并发不变量：每次工具调用新建独立 rusqlite 连接、无 `busy_timeout`、未开 WAL（默认 rollback journal）。多 agent/多 session 并发写同一 `{graph_name}.db` 时会 SQLITE_BUSY 失败（错误仅映射为 Tool error）；且每次调用重复执行建表 DDL | 与已知不变量"storage 是 Arc<Mutex<Connection>>+WAL"直接相悖 |
| P2 | memory.rs:210-247 | MemoryManageTool 直接同步调用 `store.add_scoped_file_memory_json` 等（文件 I/O：快照、原子落盘、索引同步）跑在 async 上下文，未走同文件定义的 `run_store_blocking` | 与 memory_query/upsert/forget 一律 spawn_blocking 的纪律不一致；canonical 文件写入涉及 fsync，会阻塞 tokio worker |
| P2 | knowledge_graph.rs:884-888 | export 的 `output_path` 以 `~/` 开头时未展开家目录，字面量 `~/...` 被交给 `std::fs::write`，会在 CWD 下生成名为 `~` 的目录（绝对路径写法本身已被 runtime path policy 覆盖为 Write op，此条仅为展开缺失） | path_security.rs:76-78 已把 knowledge_graph output_path 纳入 Write 策略检查，绝对路径无逃逸 |
| P2 | knowledge_graph.rs:601 vs :76 | 契约不一致：schema 声明 `depth`（find_path）默认 2，实现 `unwrap_or(5)` | 模型按 schema 预期调用会拿到更深遍历 |
| P2 | knowledge_forget.rs:243-281 | confirm 非原子：逐条删除中途失败时，先前匹配已删、tombstone 已全部写入（:240 先行 record_forgotten_content），产生"部分删除+全部墓碑"状态；且返回前无回滚 | 至少应在失败时报告 partial 状态 |
| P2 | knowledge_graph.rs:619-634,727-743 | N+1 查询模式：BFS 每个出队节点都 `prepare` 一次邻居查询（find_path/subgraph），大图上为每节点一次 SQL prepare | 可用一次全图加载或缓存 stmt 优化 |
| P3 | fs.rs:131 | read_file 无大小上限：`read_document` 将整个文件（非 Office 时 `read_to_string`）载入内存，GB 级文本文件会打爆内存；二进制文件报裸 UTF-8 错误而非友好提示 | 仓内已有 bounded_io 设施但 fs 路径未用 |
| P3 | fs.rs:334-359 | edit_file read-modify-write 无锁：读取与落盘之间存在 TOCTOU 窗口，并发编辑会互相覆盖（多 edit 在内存中合并正确，原子性仅限单进程内） | 可接受但值得记录 |
| P3 | file_ops.rs:725-732 | file_info 的 `symlink` 分支不可达：`tokio::fs::metadata` 跟随符号链接，`is_symlink()` 恒 false | 死分支 |
| P3 | knowledge_graph.rs:377,910 | `max_results` 实际无上限（`.min(i64::MAX as u64)`），search/export 可把整库序列化为单个 JSON 响应 | 建议硬上限（如 1000） |
| P3 | knowledge_graph.rs:611-614 | find_path 边界：source == target 时返回 not found（起始节点未做目标判定） | 语义瑕疵 |
| P3 | knowledge_graph.rs:922-931 | tag 查询用 LIKE 子串匹配（`%"tag%`），"import" 会命中 "important" | 精度问题 |
| P3 | code_search.rs:94 | grep 对每个匹配文件整体 `read_to_string` 后才逐行匹配；超大文件内存放大（非 UTF-8 已正确跳过，输出有 256KB 上限） | 可用流式/bufreader 分块 |
| P3 | html_to_md.rs:136-138,145-162 | `truncate_utf8(s, max_chars)` 参数名为 chars 但比较的是字节 `s.len()`；htmd 转换（CPU 密集）跑在 async 线程上，未 spawn_blocking（输入已被 bounded_io 限 4MB，风险有限） | 命名与实现语义不符 |
| P3 | session_recall.rs:19 | 未声明 idempotency/execution_environment（其余同集群工具均声明）；`tool:` 输出缺失时返回 Ok(error JSON) 而非 Err，审计口径不一致 | 风格问题 |

**未发现问题的维度说明**

- 安全·路径穿越（P0 候选）：fs 类工具的写路径全部经 runtime path policy 门（docs/22，path_security.rs:51-78 对 read_file/write_file/edit_file/list_dir/file_ops(含 destination)/knowledge_graph(output_path) 提取并判定），工具层不再需要自行拦截；`graph_name` 白名单校验 + canonicalize 逃逸检查（knowledge_graph.rs:34-55）、session_recall 的 `tool_id` 遍历字符黑名单 + session 目录哈希隔离（:77-133）均到位。**未发现可用的路径穿越**。
- 安全·符号链接：file_ops 的 move/copy/compress/decompress 全部拒绝源与递归路径中的符号链接，zip/tar 解压拒绝链接条目（unix_mode/tar entry_type 双重检查），并有对应单测（file_ops.rs:823-841）。**未发现逃逸**。
- 安全·记忆内容泄漏进日志：memory/knowledge_forget/memory_maintenance 的日志只记计数/scope/type/days，不记 content；GhostLedger 中记录 content 属审计设计（docs/05:200 tombstone 语义要求）。**未发现日志泄漏**。
- 正确性·记忆增删改查一致性：delete/batch_delete/restore 一律走 in_session 变体，memory_upsert 强制 short_term，query 带 session_key，空结果带反幻觉 note——与 docs/05 契约逐条一致（memory_upsert 只收 short_term、knowledge_forget 两阶段、memory_manage 唯一长期写入口均兑现）。
- 正确性·KG 关系完整性：relations 外键 ON DELETE CASCADE + 应用层先删关系再删实体，add_relation 双端存在性校验；FTS5 外内容表 + 三触发器保持同步；SQL 注入有专项测试（:1271-1355）。
- 契约一致性（docs/03/docs/05）：fs 四件套、file_ops、memory 四件套、knowledge_forget、session_recall 均与文档描述吻合；唯一文档未覆盖的机制缺口是 knowledge_forget 的"两阶段确认"在运行时没有用户准入门（见 P1 第 3 条）。

## 亮点

1. **fs.rs backend seam 设计**：工具只做路径解析与参数语义，文件访问全部经 `Arc<dyn FilesystemBackend>`，`MemoryFs` 测试证明了注入可行性；edit_file 多编辑"全部唯一匹配才落盘"的原子语义 + fuzzy_match 的 ±5 行上下文报错，LLM 体验考虑周到；约 360 行测试覆盖 offset/limit、别名、原子性、backend 路由。
2. **file_ops 的爆破防护成体系**：条目数/单条目/总量/压缩比四道闸（zip/tar 双侧）、符号链接在 move/copy/compress/递归复制/解压五个入口全部拒绝并有单测，`target_is_within_source` 防复制进自身。
3. **memory.rs 的并发纪律**：`run_store_blocking` 封装 spawn_blocking 并被 memory_maintenance 复用；long_term 写入在 validate 层即被拦截并指向 canonical 文件；session 级删除/恢复/统计全程隔离。
4. **knowledge_forget 的审计闭环**：token = SHA256(query, session_key, matches)，匹配集变化即失效（"run preview again"）；tombstone 阻止复活 + GhostLedger 审计事件 + 删后索引重建，并有端到端单测（preview→confirm→三来源清空→tombstone 生效→审计计数）。
5. **code_search**：spawn_blocking + gitignore 尊重 + follow_links(false) + 256KB 输出上限 + 三个 SQL/正则注入专项测试。
6. **session_recall**：新格式精确目录匹配、旧格式前缀扫描显式隔离（注释解释了为何不再回退以免串轮次）、`tool_id` 黑名单 + session_key 清洗算法与写入侧一致（复用 `stable_hash_session_key`）。
7. **html_to_md**：全程 ssrf 防护 + bounded_io 限长 + content negotiation + 双引擎兜底（htmd→scraper）。

## 建议

1. （P1）knowledge_graph 与 knowledge_forget 的全部 rusqlite/文件 I/O 包进 `spawn_blocking`（knowledge_forget 可直接复用 `memory::run_store_blocking`）；knowledge_graph 每次执行改为缓存连接并设置 `busy_timeout` + `journal_mode=WAL`，init_schema 仅在打开时执行一次。
2. （P1）给 knowledge_forget 实现 `Tool::requires_confirmation`（按 query 影响面分级，如 matches>0 即需确认），让 runtime 准入门真正生效；或至少在文档/描述中明确"两阶段由 LLM 自驱、无用户门"的风险定位。输出 JSON 的 `requires_confirmation` 字段建议改名避免与 trait 语义混淆。
3. （P1）修复 memory_upsert 的 ghost 矛盾：若 ghost 长记写入已废弃，删除 :504-528 整段死分支；若仍需支持，则应放开 validate 对 ghost channel 的 long_term 限制（按 channel 分流校验）。
4. （P1）memory_maintenance 的 prune/sweep 增加 session 维度：至少用 `soft_delete_in_session` 或显式记录"全局维护"语义；junk_score 阈值 4.0 建议提高或加 source=user 硬豁免。
5. （P2）MemoryManageTool 的 file-store 调用包进 run_store_blocking；knowledge_graph export 展开 `~/`；统一 find_path depth 默认值；knowledge_forget confirm 改为收集失败后报告 partial 而非中途即抛。
6. （P3）read_file 加大小上限（可借 bounded_io）并对非 UTF-8 返回明确提示；file_info 删死分支；KG 的 max_results 设硬上限；code_search grep 改流式读取。
