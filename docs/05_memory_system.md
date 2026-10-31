# 第05篇：记忆系统 —— 让 AI 记住你说过的话

> 系列文章：《blockcell 开源项目深度解析》第 5 篇

---

## 为什么 AI 需要记忆

新开对话后，模型不会天然记得用户偏好、项目事实和之前验证过的经验。blockcell 将这些信息分成长期知识与短期记忆，并在后续任务中按需召回。

---

## 记忆系统的架构

从配置和运维视角，BlockCell 只暴露三个子系统：

| 子系统 | 职责 |
|---|---|
| Context Management | 工具结果缓存、micro/full compact、恢复预算与 Forked Agent 执行 |
| Knowledge System | `USER.md`/`MEMORY.md`、短期记忆、FTS/向量索引、统一召回与 Prompt Budget |
| Learning System | 边界捕获、合并审查、Skill 学习和低频 Dream 维护 |

推荐配置：

```json5
memory: {
  contextManagement: {
    forkedAgent: { enabled: true, maxTurns: 10, timeoutSecs: 120 }
  },
  knowledgeSystem: {
    promptBudget: {
      total: 12000,
      rules: 2000,
      userProfile: 800,
      retrieved: 3000,
      activeSkill: 4000,
      sessionRecovery: 2000
    },
    memoryRecall: { chat: true, general: true, skill: true, internal: false }
  },
  learningSystem: { enabled: true }
}
```

旧 `memory.memorySystem.layer7` 仅保留一个兼容版本，并映射到
`memory.contextManagement.forkedAgent`；同时配置时新键优先。

blockcell 采用“**文件作为长期事实源，SQLite 作为可重建索引、短期存储和审计**”的架构：

```text
~/.blockcell/workspace/
├── USER.md                         # 用户偏好、长期约束
└── memory/
    ├── MEMORY.md                   # 项目事实、反馈与参考知识
    ├── knowledge_index.db          # 可删除、可重建的 FTS/向量索引
    └── memory.db                   # 带 TTL 的短期记忆与旧数据迁移源

~/.blockcell/workspace/ghost/
└── ghost_ledger.db                 # Ghost 学习与遗忘审计
```

核心语义如下：

| 数据 | 角色 | 是否事实源 |
|------|------|------------|
| `USER.md` | 用户维度的稳定偏好、约束和明确陈述 | 是 |
| `memory/MEMORY.md` | 项目、环境、反馈和参考知识 | 是 |
| `knowledge_index.db` | 文件知识的 FTS5、元数据和可选向量索引 | 否，可重建 |
| `memory.db` | 当前会话的短期、可过期记忆 | 否 |
| `GhostLedger` | 自动学习、统一遗忘等操作的审计记录 | 否 |

长期知识只有两个可写入口：`USER.md` 和 `memory/MEMORY.md`。索引损坏或丢失时可从这两个文件重建，不能反向把索引当成权威数据覆盖文件。

---

## 记忆的类型

### 长期知识

- `USER.md`：用户明确表达的偏好、沟通风格、长期约束。
- `memory/MEMORY.md`：项目事实、已验证经验、用户反馈和参考信息。

`MEMORY.md` 使用稳定分类标题组织内容：

```markdown
## Project

## Feedback

## Reference
```

长期条目可以携带机器可读元数据：

```markdown
- [id:pref-new] [scope:user] [source:user_statement] [updated:2026-08-01] [supersedes:pref-old] 用户偏好简洁回答
```

- `id`：稳定条目 ID。
- `scope`：`user` 或 `workspace`。
- `source`：`user_statement`、`verified` 或 `inferred`。
- `updated`：最近确认日期。
- `supersedes`：被当前条目替代的旧条目 ID。

### 短期记忆

短期记忆保存在 `memory.db`，scope 固定为 `short_term`，适合当前任务状态、临时数据和带过期时间的信息。它不是长期知识源。

---

## 记忆工具

### `memory_manage` — 管理长期知识

新增用户偏好：

```json
{
  "action": "add",
  "target": "user",
  "scope": "user",
  "content": "用户偏好简洁的中文总结。"
}
```

新增项目知识：

```json
{
  "action": "add",
  "target": "memory",
  "scope": "workspace",
  "content": "发布前必须运行目标测试和 git diff --check。"
}
```

它还支持 `replace`、`remove` 和 `undo_latest`。写入会经过规范化、安全扫描、快照与原子落盘，并同步刷新知识索引。

### `memory_upsert` — 保存短期记忆

```json
{
  "title": "当前任务",
  "content": "正在分析 Q3 财报",
  "type": "task",
  "scope": "short_term",
  "expires_in_days": 1
}
```

`memory_upsert` 只接受 `short_term`。尝试写入 `long_term` 会被拒绝，并提示改用 `memory_manage`。

### `memory_query` — 查询短期 SQLite 记忆

```json
{
  "query": "Q3 财报",
  "scope": "short_term",
  "top_k": 5
}
```

该工具查询 `memory.db` 中的结构化短期记忆。长期文件知识由 `KnowledgeIndex` 在运行时召回。

### `memory_forget` — 软删除旧 SQLite 记忆

```json
{
  "action": "delete",
  "id": "记忆ID"
}
```

该工具保留原有 SQLite 软删除与恢复语义，主要用于短期记忆和兼容数据。

### `knowledge_forget` — 跨来源统一遗忘

统一遗忘采用两阶段确认，覆盖规范文件、当前 Session 文件和 SQLite 短期记忆。

第一步预览：

```json
{
  "action": "preview",
  "query": "简洁回答"
}
```

第二步使用预览返回的精确 token 确认：

```json
{
  "action": "confirm",
  "query": "简洁回答",
  "reason": "用户要求",
  "preview_token": "..."
}
```

确认后会删除匹配知识、重建索引、写入遗忘 tombstone，并在 GhostLedger 中记录审计事件。tombstone 会阻止 Ghost 后台学习、普通写入或 snapshot 恢复再次写回相同内容。

---

## 长期知识如何召回

系统提示词只生成一个 `<retrieved-context>` 区块，所有来源先标注、去重并按
`user-profile > knowledge > session > short-term > skill-index` 解决冲突，然后服从全局
Prompt Budget。Chat、General、Skill 可分别开关；`ghost`、`cron`、`system`、`subagent`
默认视为内部流量，不召回用户知识。

`knowledge_fts` 与 `memory_fts` 使用 trigram tokenizer。旧索引在启动时自动重建；连续中文
查询会展开为重叠 bigram，因此“发版检查什么”可以命中“发布前需要检查 changelog 和版本号”。

`KnowledgeIndex` 增量索引 `USER.md` 和 `memory/MEMORY.md`。召回候选按以下规则收敛：

```text
内容去重
→ 丢弃被 supersedes 指向的旧条目
→ user_statement > verified > inferred
→ 更新时间越新越优先
→ 最后按 FTS relevance 排序
```

Ghost recall 使用该索引检索长期知识，再单独合并当前 Session 文件中的相关内容。运行时注入器只读取两个规范长期文件，不再直接读取旧 Layer 5 文件。

召回内容是临时上下文，不会提升为系统指令；当前用户指令始终优先。

---

## 实际使用场景

### 场景一：记住用户偏好

```text
你：我偏好用 Python，而且代码修改后请给我简洁的中文总结。

AI：好的，我会记住。
    [memory_manage target=user scope=user]
```

### 场景二：记住项目信息

```text
你：这个项目发布前必须运行目标测试和回滚检查。

AI：已记录为项目长期知识。
    [memory_manage target=memory scope=workspace]
```

### 场景三：追踪临时任务

```text
你：帮我分析最近三个月的财务数据。

AI：[memory_upsert scope=short_term expires_in_days=1]
```

### 场景四：彻底忘记错误知识

先调用 `knowledge_forget(action="preview")` 检查影响范围，再携带 `preview_token` 调用 `confirm`，避免模糊查询造成误删。

---

## 自动维护与安全

blockcell 会维护过期短期记忆、软删除回收站和可重建索引。长期文件写入具备：

- 内容规范化与去重。
- prompt injection、凭据和隐藏控制字符扫描。
- 写前 snapshot。
- 进程内 mutex、跨进程 lockdir 和原子写。
- 写后索引同步。
- tombstone 防止已遗忘知识复活。

---

## 命令行管理

```bash
# 列出、搜索和查看 SQLite 记忆
blockcell memory list
blockcell memory search "财报"
blockcell memory show <ID>

# 删除、清理和统计 SQLite 记忆
blockcell memory delete <ID>
blockcell memory clear --scope short_term
blockcell memory stats
blockcell memory maintenance --recycle-days 30

# 将遗留 SQLite long_term 行迁移到规范文件
blockcell memory migrate-canonical
```

---

## 从旧版迁移

### SQLite 长期记忆

运行：

```bash
blockcell memory migrate-canonical
```

该命令会规范化并去重遗留 `long_term` 行，将其写入 `USER.md` 或 `memory/MEMORY.md`，成功后软删除旧行并同步索引。命令可重复运行，保持幂等。

### 旧 Layer 5 文件

运行时会自动将以下文件收敛到两个规范文件：

```text
base/memory/user.md       → workspace/USER.md
base/memory/project.md    → workspace/memory/MEMORY.md / Project
base/memory/feedback.md   → workspace/memory/MEMORY.md / Feedback
base/memory/reference.md  → workspace/memory/MEMORY.md / Reference
```

迁移会按规范化内容去重；成功后旧文件重置为兼容模板，不再作为注入来源。

---

## 为什么文件是长期事实源，SQLite 仍然存在

文件化长期知识便于人工审查、直接编辑、Git diff、备份和迁移，也能明确回答“哪份数据才是真的”。SQLite 继续承担它更适合的职责：

- `knowledge_index.db` 提供快速全文检索、元数据排序和可选向量缓存。
- `memory.db` 管理短期、结构化、可过期记忆。
- `GhostLedger` 保存学习与遗忘审计。

因此，删除索引不会丢失长期知识；修改规范文件后，索引可以重新生成。

---

## 小结

blockcell 的知识与记忆体系现在遵循四条原则：

- `USER.md` 与 `memory/MEMORY.md` 是唯一长期事实源。
- `memory_upsert` 只写短期记忆，长期知识统一由 `memory_manage` 管理。
- `KnowledgeIndex` 负责增量检索、冲突消解和排序，但可以随时重建。
- `knowledge_forget` 通过预览、确认、tombstone 和审计提供真正的跨来源遗忘。

---

*上一篇：[技能（Skill）系统 —— 用 Rhai 脚本扩展 AI 能力](./04_skill_system.md)*

## 长期记忆写到哪：canonical 与 shadow

`memory_manage` 的显式长期写入（`scope=workspace|user`）固定落到 **canonical** 文件：

| 文件 | 路径 |
|---|---|
| `USER.md` | `<workspace>/USER.md` |
| `MEMORY.md` | `<workspace>/memory/MEMORY.md` |

`<workspace>` 默认是 `~/.blockcell/workspace`，可用 `agents.defaults.workspace` 覆盖
（`Paths::user_md` / `Paths::memory_md`）。

工具的返回值会带上 `canonical`、`shadow` 与 `path`，调用方可以据此确认是否真的落盘。

Ghost 的**自动**学习/复盘另走一套存储：当 `agents.ghost.learning.shadowMode=true`（默认）时，
自动写入只落在 `memory/shadow/` 影子副本，不会改动 canonical 文件，返回值会显式标注
`shadow: true` 与 `"Saved to shadow memory only; canonical durable memory was not changed."`。
显式记忆与自动沉淀互不影响：关闭 shadow 模式不会影响显式写入，反之亦然。

*下一篇：[多渠道接入 —— Telegram/Slack/Discord/飞书都能用](./06_channels.md)*

*项目地址：https://github.com/blockcell-labs/blockcell*

*官网：https://blockcell.dev*
