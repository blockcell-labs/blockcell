# 第05篇：记忆系统 —— 让 AI 记住你说过的话

> 系列文章：《blockcell 开源项目深度解析》第 5/14 篇

---

## 为什么 AI 需要记忆

用过 ChatGPT 的人都知道一个痛点：**每次新开对话，AI 就忘了你是谁。**

你上次告诉它"我是做量化交易的，关注 A 股科技板块"，这次又要重新说一遍。

更麻烦的是，即使在同一个对话里，如果对话太长，早期的内容也会被"遗忘"（超出 context window）。

blockcell 的记忆系统解决了这个问题。

---

## 记忆系统的架构

blockcell 的记忆系统基于 **SQLite + FTS5**（全文搜索）构建。

```
~/.blockcell/workspace/memory/memory.db
```

这是一个本地 SQLite 数据库，完全在你的电脑上，不会上传到任何服务器。

### 数据库结构

```sql
-- 记忆条目表
CREATE TABLE memory_items (
    id          TEXT PRIMARY KEY,
    scope       TEXT,    -- 'short_term' | 'long_term'
    type        TEXT,    -- 'fact' | 'preference' | 'project' | 'task' | 'note'
    title       TEXT,
    content     TEXT,
    summary     TEXT,
    tags        TEXT,    -- JSON 数组
    importance  INTEGER, -- 1-10
    created_at  INTEGER,
    updated_at  INTEGER,
    expires_at  INTEGER, -- 可选过期时间
    deleted_at  INTEGER, -- 软删除
    dedup_key   TEXT     -- 去重键
);

-- FTS5 全文搜索虚拟表
CREATE VIRTUAL TABLE memory_fts USING fts5(
    title, summary, content, tags,
    content=memory_items
);
```

---

## 记忆的类型

blockcell 把记忆分为两个维度：

### 按时效分（scope）

| 类型 | 说明 | 典型用途 |
|------|------|---------|
| `long_term` | 长期记忆，永久保存 | 用户偏好、重要事实、项目信息 |
| `short_term` | 短期记忆，可设置过期 | 当前任务状态、临时数据 |

### 按内容分（type）

| 类型 | 说明 |
|------|------|
| `fact` | 客观事实（"茅台代码是 600519"） |
| `preference` | 用户偏好（"喜欢用 Python 而不是 JS"） |
| `project` | 项目信息（"正在开发一个量化交易系统"） |
| `task` | 任务状态（"正在分析 Q3 财报"） |
| `note` | 普通笔记 |

---

## 三个记忆工具

### `memory_upsert` — 保存记忆

```json
{
  "tool": "memory_upsert",
  "params": {
    "title": "用户偏好：编程语言",
    "content": "用户偏好 Python，不喜欢 JavaScript",
    "type": "preference",
    "scope": "long_term",
    "importance": 8,
    "tags": ["编程", "偏好"],
    "dedup_key": "user_lang_preference"
  }
}
```

`dedup_key` 是去重键：如果已有相同 key 的记忆，会更新而不是新建。

### `memory_query` — 搜索记忆

```json
{
  "tool": "memory_query",
  "params": {
    "query": "股票 偏好",
    "scope": "long_term",
    "top_k": 5
  }
}
```

使用 FTS5 全文搜索，支持中文分词，返回最相关的记忆条目。

### `memory_forget` — 删除记忆

```json
{
  "tool": "memory_forget",
  "params": {
    "action": "delete",
    "id": "记忆ID"
  }
}
```

支持软删除（可恢复）和批量删除（按 scope/type/tags 过滤）。

---

## 记忆如何注入到对话

每次对话开始时，blockcell 会自动生成一个**记忆摘要**，注入到系统提示词中：

```
[Memory Brief]
Long-term (top 20):
- 用户偏好 Python 编程，关注 A 股科技板块 [preference, importance:8]
- 正在开发量化交易系统，使用 backtrader 框架 [project, importance:9]
- 茅台(600519)是重点关注股票 [fact, importance:7]

Short-term (top 10):
- 今天正在分析 Q3 财报数据 [task, expires: 2h]
```

这样 AI 每次对话都能"记住"你的背景信息，不需要重复介绍。

---

## 实际使用场景

### 场景一：记住用户偏好

```
你: 我做量化交易，主要关注 A 股科技板块，
    偏好用 Python，不喜欢 Excel

AI: 好的，我已经记住了你的偏好。
    [调用 memory_upsert 保存这些信息]
```

下次对话：
```
你: 帮我分析一下科技板块的走势

AI: [自动从记忆中知道你关注 A 股科技板块]
    好的，我来分析 A 股科技板块...
```

### 场景二：记住项目信息

```
你: 我在做一个项目叫"智能选股系统"，
    用 Python + backtrader，目标是跑赢沪深300

AI: 已记录项目信息。
    [memory_upsert type=project]
```

### 场景三：任务状态追踪

```
你: 帮我分析茅台最近三个月的财务数据

AI: 好的，开始分析...
    [memory_upsert type=task scope=short_term expires_in_days=1]
    [分析过程...]
    分析完成，结果已保存。
```

---

## 记忆的查询评分机制

FTS5 搜索结果会经过一个综合评分：

```
最终得分 = BM25相关性得分 + 重要性加分 + 时效性加分
```

- **BM25**：经典的全文搜索相关性算法
- **重要性加分**：importance 字段（1-10），越重要排越前
- **时效性加分**：最近更新的记忆有额外加分

这确保了最相关、最重要、最新的记忆优先出现在摘要中。

---

## 记忆维护

blockcell 每 60 秒会自动运行一次维护任务：

```rust
// runtime.rs 中的 tick 逻辑
store.maintenance(30)  // 清理 30 天前软删除的记忆
```

维护内容：
1. 清理已过期的短期记忆
2. 清理 30 天前软删除的记忆（回收站）
3. 更新 FTS5 索引

---

## 命令行管理记忆

```bash
# 列出所有记忆
blockcell memory list

# 搜索记忆
blockcell memory search "股票"

# 查看记忆统计
blockcell memory stats

# 清理过期记忆
blockcell memory clean
```

---

## 从旧版迁移

如果你之前用的是基于 Markdown 文件的旧版记忆系统（`MEMORY.md`），blockcell 会在首次启动时自动迁移：

```rust
// agent.rs
store.migrate_from_files(&paths)
// 读取 MEMORY.md 各章节 + 历史日记文件
// 导入到 SQLite 数据库
```

---

## 为什么选择 SQLite + FTS5

很多人会问：为什么不用向量数据库（如 Chroma、Pinecone）？

blockcell 选择 SQLite + FTS5 的理由：

1. **零依赖**：SQLite 是 Rust 标准库级别的存在，不需要额外服务
2. **本地优先**：所有数据在本地，隐私安全
3. **够用**：对于个人助手场景，FTS5 的全文搜索完全够用
4. **快速**：SQLite 的读写性能对于记忆场景绰绰有余
5. **可靠**：SQLite 是世界上使用最广泛的数据库之一

向量数据库在语义搜索上更强，但需要额外的嵌入模型调用（消耗 token 和时间）。对于结构化的记忆条目，关键词搜索往往更精确。

---

## 小结

blockcell 的记忆系统：

- **持久化**：SQLite 本地存储，重启不丢失
- **全文搜索**：FTS5 支持中文关键词搜索
- **智能注入**：每次对话自动注入最相关的记忆摘要
- **自动维护**：过期清理、软删除回收站
- **隐私安全**：完全本地，不上传

有了记忆系统，blockcell 才真正成为一个了解你的个人 AI 助手，而不只是一个无状态的聊天工具。
---

*上一篇：[技能（Skill）系统 —— 用 Rhai 脚本扩展 AI 能力](./04_skill_system.md)*
*下一篇：[多渠道接入 —— Telegram/Slack/Discord/飞书都能用](./06_channels.md)*

*项目地址：https://github.com/blockcell-labs/blockcell*
*官网：https://blockcell.dev*
