# B1：基础配置与存储审查

> 历史记录（2026-11-11）：原 24 项已于 2026-11-12 复核。当前裁决、限定、工具链验证与新增问题以 [优化指导](README.md)、[复核矩阵](08-revalidation-matrix.md) 为准；本文旧统计及 Rust 阻断说明仅代表首次审查时的结果。

基线：`5ca38be861b4a3e8f7a1e8d332e242b42ed25710`，分支 `main`。本批确认 **P1 × 1、P2 × 4**，无 P0。仅新增本报告，未修改业务代码或测试。

## 实际覆盖

- 精读 `core/session_key.rs` 的编码、解码及反向查找；`core/paths.rs` 的 agent、session、event 路径构造；`core/budget.rs` 的父子预算归并和检查；`core/scope.rs` 的可见性与注销。
- 精读 `core/config.rs` 的秘密掩码/恢复、未知字段保留、验证和读写入口，核对 Gateway 配置 API、WebUI LLM 配置保存调用链；检查 `core/file_store.rs` 原子写与锁。
- 精读 `core/path_policy.rs` 的路径规范化、优先级及允许写根计算；检查 `core/tool_policy.rs` 的规则编译、无效配置关闭和 Deny/Ask/Allow 决策。
- 精读 `storage/session.rs` 的加载、保存合并、追加、删除；`storage/session_event.rs` 的追加、序号、读取和清理；检查 `storage/file_lock.rs`。
- 精读 `storage/memory/{crud,vector_sync}.rs`，检查 `maintenance/query` 中删除、检索过滤、重试相关路径；检查 `retriever.rs` 的候选融合和二次过滤；检查 `knowledge_index.rs` 的文件重建事务和 schema 触发器。
- 抽查 `storage/audit.rs` 的工具参数掩码、链验证，`rabitq_index.rs` 的写入/dirty epoch/检索，`contacts.rs` 的加锁更新。
- 核对历史基础层报告，未把已修复的事务、tag 过滤、符号链接规范化、锁 inode 删除问题重列为当前缺陷。

未逐行覆盖所有配置子模块、ghost ledger、evolution workflow、审计和向量索引实现；没有实际运行 Gateway、SQLite MemoryStore 或并发压力测试。全工作区 Cargo 验证由主审统一执行，本机 Rust 版本限制见 B0。

## 确认发现

### B1-01 · P1：legacy 会话兼容路径会读入或删除另一个当前会话

- 位置：`crates/storage/src/session.rs:299`（读取回退）、`:413` / `:443`（删除 legacy 路径）；路径规则在 `crates/core/src/session_key.rs:30` / `:52`。
- 前提：同一 agent 下存在会话 A `ws:a:b`；会话 B 的 key 为 `ws:a_b`。这些 key 可以由 WebSocket 自定义 session id 产生，且无需遗留旧版本数据。
- A 的当前文件名为 `ws_a_b.jsonl`；B 的当前文件名为 `s~ws~3Aa_b.jsonl`，但 B 的 legacy 路径也为 `ws_a_b.jsonl`。
- 实际行为：当 B 还没有当前文件时，`SessionStore::load(B)` 的 `read_path` 会返回 A 的历史；即使 A/B 都有当前文件，`clear(B)` 仍会在删除 B 后继续删除 A 的文件及其 dream marker。此分支不检查 legacy 文件的归属，也不持有 A 路径对应的锁。
- 可达入口：`bin/blockcell/src/commands/gateway/sessions.rs:259` 调用 `SessionStore::clear`；`bin/blockcell/src/commands/slash_commands/handlers/clear.rs:50` 也直接按当前会话 key 调用它。
- 影响：会话历史串读及删除错误对象；当前文件名编码虽然彼此不同，兼容回退重新引入了碰撞。
- 证据：实际编译并运行从当前源码原样抽取的编码函数，确认 `current(A) == legacy(B)`；读取/删除后果来自上述完整分支的静态推演，未声称执行了完整 SessionStore。
- 建议：为 legacy 迁移增加可验证的原始 session key/所有权记录，不能把任意相同 legacy stem 认作本会话。若无法确定归属，应要求显式迁移；至少不得无条件删除 canonical 之外的潜在其他会话文件。补充“两个新版本会话并存后只删除其中之一”和“新 key 首次读取不串入旧路径”的回归用例。

### B1-02 · P2：结构化配置保存会恢复用户明确删除的 Provider

- 位置：`crates/core/src/config.rs:460`、`:976`；调用方 `bin/blockcell/src/commands/gateway/config_api.rs:69`。
- 前提：磁盘中有 `providers.old`，用户在 WebUI LLM 页面删除该 Provider 并保存，或客户端向 `/v1/config` 提交不含它的完整 `providers` 对象。
- 调用链：`webui/src/components/llm/llm-page.tsx:150` 的 `removeProvider` 从 state 删除 key；`:112` 的保存路径通过 `mergeLlmConfig` 提交该 map；Gateway 反序列化后调用 `Config::save`。
- 实际行为：`merge_unknown_keys(previous, next)` 对每个对象无差别递归，把旧对象中所有 next 缺少的键都插回。它无法区分“schema 不认识的扩展字段”和“HashMap 中主动删除的条目”，因此 `providers.old` 被写回，API 仍返回成功。
- 影响：删除配置表面成功，刷新/重启后 Provider 复活；同类问题也影响受此 merge 处理的动态 map。旧值为某些 `skip_serializing_if` 可选字段时，清空字段也可能被恢复。
- 证据：静态调用链和递归合并规则；未实际改写用户配置。
- 建议：根据 schema 只保留真正未知的字段，对 `providers`、账号、路由等动态 map 使用请求中的完整内容；给删除操作使用明确的删除语义。补充从落盘配置删除 Provider 后重新加载仍不存在的测试。

### B1-03 · P2：配置 API 接受并保存启动时必然拒绝的配置

- 位置：`crates/core/src/config.rs:756`、`:793`、`:796`、`:828`、`:936`；`bin/blockcell/src/commands/gateway/config_api.rs:69` / `:89` / `:107`。
- 前提：通过结构化或 raw 配置接口提交语法和类型合法但值无效的配置，例如 `{"cronTickIntervalSecs":0}`，或 `{"defaultTimezone":"Mars/Olympus"}`。
- 实际行为：`validate_config_json5_str/file` 只执行解析和 serde 反序列化；`write_raw_validated_config_json5` 随后直接写盘。结构化更新也只反序列化和 `save`；reload 入口调用相同的非语义验证函数，可以报告验证成功。只有下一次 `Config::load` 才调用 `.validate()`，拒绝上述 cron 范围/时区值。
- 影响：用户收到“保存成功、重启生效”提示后重启，服务因自己先前接受的配置启动失败。非法 `tools.exec.approvalPolicy` 也有相同的验证入口差异。
- 证据：静态；反序列化字段类型分别为 u64/String，范围、IANA 时区和 approval policy 检查仅存在于 `.validate()`。未实际重启或改写配置。
- 建议：提供所有入口共用的完整验证函数，在写盘之前完成与启动加载相同的语义校验；`Config::save` 是否允许未验证对象也应有明确契约。

### B1-04 · P2：密钥掩码把 maxTokens 等数值配置改成字符串

- 位置：`crates/core/src/config.rs:548`、`:572`；`bin/blockcell/src/commands/gateway/config_api.rs:24`；前端使用位置 `webui/src/components/llm/llm-page.tsx:92` / `:599`。
- 前提：通过 `/v1/config` 读取含 `maxTokens`、`maxContextTokens` 或 `budget.maxTokensPerSession` 的配置。
- 实际行为：`is_secret_key` 只要字段名包含 `token` 即返回 true；`mask_secret_values` 随后不管原类型，统一替换为掩码字符串。前端把响应的 `maxTokens` 原样赋给 model entry，最终传到 `type="number"` 输入框。
- 影响：公开的容量/预算参数无法从该 API 正确读取，数值类型契约被破坏，LLM 页 Max Tokens 展示失真；raw 配置编辑入口不受此项影响。`restore_masked_secrets` 会在原位置恢复未修改的掩码，故不能据此断言普通“原样回写”一定损坏配置。
- 证据：实际运行当前源码原样抽取的 `is_secret_key`，上述三个字段均为 true；掩码递归及前端传值为静态证据。
- 建议：使用明确的凭据字段/配置路径，并保留非凭据数值字段；给 token 额度和 API token 凭据同时存在的对象增加掩码契约测试。

### B1-05 · P2：不存在的记忆 ID 永远走错误分支，而非返回 None

- 位置：`crates/storage/src/memory/crud.rs:203`、`:211`、`:224`；用户入口 `bin/blockcell/src/commands/memory.rs:157`。
- 前提：对已存在数据库执行 `blockcell memory show <不存在的 ID>`，或调用 `MemoryStore::get_by_id` 查询缺失项。
- 实际行为：`get_by_id_inner` 用 Display (`{}`) 将 rusqlite 错误变成 `Get by id error: Query returned no rows`，外层却检查 `msg.contains("QueryReturnedNoRows")`。该字符串是枚举变体名称，不是锁定版本的 Display 文案，因此缺失项不会进入 `Ok(None)`。
- 影响：正常的“未找到记忆”被当成存储故障返回；调用者无法按该公开 API 的 Option 契约区分不存在与数据库异常。
- 证据：静态核对当前实现以及本机缓存的锁定依赖 `rusqlite-0.31.0/src/error.rs:288`（Display 明确输出 `Query returned no rows`）。未运行完整 MemoryStore。2026-10-16 报告已提到字符串判错，此处重新核对确认当前版本仍存在，并非仅“将来文案变化”风险。
- 建议：在 rusqlite 错误尚未转成字符串前使用 `OptionalExtension::optional()` 或匹配 `rusqlite::Error::QueryReturnedNoRows`，保留其他错误。

## 最小验证记录

通过 Python 在临时目录抽取当前 `session_file_stem`、`legacy_session_file_stem`、`is_secret_key` 的原始定义，用本机 `rustc --edition=2021` 编译并执行独立程序，无外部依赖、无网络、未读取或删除真实会话：

```text
A=ws:a:b current=ws_a_b
B=ws:a_b current=s~ws~3Aa_b legacy=ws_a_b
is_secret_key(maxTokens)=true
is_secret_key(maxContextTokens)=true
is_secret_key(maxTokensPerSession)=true
is_secret_key(inputPrice)=false
is_secret_key(apiKey)=true
```

断言：A/B 当前 stem 不同、A 当前 stem 等于 B legacy stem、上述 token 额度字段被判定为秘密，全部通过。这是对源函数的验证，不等同于项目全量测试或完整 API 端到端复现。

## 待验证与本批未计入的风险

- `MemoryStore::upsert` 在数据库事务提交并释放锁后才基于 item 快照同步向量，完成后按 id 无条件清队列；若同一 id 的两个更新并发完成顺序颠倒，可能留下旧向量且无待重试项（`memory/crud.rs:182`、`:191`，`memory/vector_sync.rs:77`、`:242`）。需要用受控 embedder 和真实 MemoryStore 并发用例确认实际调用场景与修复边界，本批未做完整复现，不计入确认总数。
- SQL 以文本比较部分 ISO 时间字段，与 RFC3339 不同时区偏移值的一致性尚需完整输入校验链核对。
- 未将预算计数的极大数值溢出、锁等待无上限等仅有理论前提的情况列为当前确定问题。

## 修复顺序建议

先修 B1-01 的会话归属与删除边界，再统一配置的删除/验证/掩码契约（B1-02～04），最后修 B1-05 的查询缺失语义。每组应补充行为级回归用例后再修改实现；本轮只提供审查证据。
