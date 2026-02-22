# Article 05: The Memory System — Letting AI Remember What You Said

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — 5/14

---

## Why AI needs memory

Anyone who has used ChatGPT knows a pain point: **every time you start a new chat, the AI forgets who you are.**

Last time you told it “I do quantitative trading and focus on China A-share tech,” and this time you have to explain it all over again.

Worse, even within the same conversation, if it gets too long, early messages get “forgotten” once they exceed the context window.

blockcell’s memory system is designed to solve this.

---

## Memory system architecture

blockcell’s memory system is built on **SQLite + FTS5** (full-text search).

```
~/.blockcell/workspace/memory/memory.db
```

It’s a local SQLite database that lives entirely on your machine — nothing is uploaded to any server.

### Database schema

```sql
-- Memory items table
CREATE TABLE memory_items (
    id          TEXT PRIMARY KEY,
    scope       TEXT,    -- 'short_term' | 'long_term'
    type        TEXT,    -- 'fact' | 'preference' | 'project' | 'task' | 'note'
    title       TEXT,
    content     TEXT,
    summary     TEXT,
    tags        TEXT,    -- JSON array
    importance  INTEGER, -- 1-10
    created_at  INTEGER,
    updated_at  INTEGER,
    expires_at  INTEGER, -- optional expiration
    deleted_at  INTEGER, -- soft delete
    dedup_key   TEXT     -- deduplication key
);

-- FTS5 full-text search virtual table
CREATE VIRTUAL TABLE memory_fts USING fts5(
    title, summary, content, tags,
    content=memory_items
);
```

---

## Memory categories

blockcell categorizes memory along two dimensions:

### By retention (scope)

| Type | Description | Typical usage |
|------|-------------|---------------|
| `long_term` | Long-term memory, kept permanently | user preferences, important facts, project info |
| `short_term` | Short-term memory, can expire | current task state, temporary data |

### By content (type)

| Type | Description |
|------|-------------|
| `fact` | objective facts (“Moutai’s symbol is 600519”) |
| `preference` | user preferences (“prefers Python over JS”) |
| `project` | project information (“building a quant trading system”) |
| `task` | task status (“analyzing Q3 earnings”) |
| `note` | general notes |

---

## Three memory tools

### `memory_upsert` — save memory

```json
{
  "tool": "memory_upsert",
  "params": {
    "title": "User preference: programming language",
    "content": "User prefers Python and dislikes JavaScript",
    "type": "preference",
    "scope": "long_term",
    "importance": 8,
    "tags": ["programming", "preference"],
    "dedup_key": "user_lang_preference"
  }
}
```

`dedup_key` is used for deduplication: if a memory item with the same key already exists, it will be updated instead of creating a new entry.

### `memory_query` — search memory

```json
{
  "tool": "memory_query",
  "params": {
    "query": "stocks preference",
    "scope": "long_term",
    "top_k": 5
  }
}
```

This uses FTS5 full-text search (including Chinese tokenization support) and returns the most relevant memory items.

### `memory_forget` — delete memory

```json
{
  "tool": "memory_forget",
  "params": {
    "action": "delete",
    "id": "MEMORY_ID"
  }
}
```

Supports soft delete (recoverable) and batch deletes (filtered by scope/type/tags).

---

## How memory is injected into conversations

At the start of each conversation, blockcell automatically generates a **memory brief** and injects it into the system prompt:

```
[Memory Brief]
Long-term (top 20):
- User prefers Python and focuses on China A-share tech [preference, importance:8]
- Building a quant trading system with backtrader [project, importance:9]
- Moutai (600519) is a key watchlist stock [fact, importance:7]

Short-term (top 10):
- Today: analyzing Q3 financial statement data [task, expires: 2h]
```

This way, the AI “remembers” your background each time without you repeating it.

---

## Real-world scenarios

### Scenario 1: remember user preferences

```
You: I do quant trading and focus on China A-share tech.
    I prefer Python and dislike Excel.

AI: Got it — I’ll remember your preferences.
    [calls memory_upsert]
```

Next time:

```
You: Analyze the recent trend of the tech sector

AI: [knows you care about China A-share tech]
    Sure — let’s analyze China A-share tech...
```

### Scenario 2: remember project info

```
You: I’m building a project called “Smart Stock Picker”,
    using Python + backtrader, aiming to beat CSI 300.

AI: Project info recorded.
    [memory_upsert type=project]
```

### Scenario 3: track task state

```
You: Analyze Moutai’s financial data over the past three months

AI: Sure — starting the analysis...
    [memory_upsert type=task scope=short_term expires_in_days=1]
    ...
    Done — the result has been saved.
```

---

## Memory query scoring

FTS5 search results are ranked by a combined score:

```
Final score = BM25 relevance + importance bonus + recency bonus
```

- **BM25**: a classic full-text relevance algorithm
- **Importance bonus**: `importance` (1–10), higher ranks earlier
- **Recency bonus**: more recently updated items get extra weight

This ensures that the most relevant, most important, and newest memories appear first in briefs.

---

## Memory maintenance

blockcell runs an automatic maintenance task every 60 seconds:

```rust
// tick logic in runtime.rs
store.maintenance(30)  // purge soft-deleted items older than 30 days
```

Maintenance includes:
1. Purging expired short-term memories
2. Purging soft-deleted memories older than 30 days (trash)
3. Updating FTS5 indexes

---

## Managing memory from the CLI

```bash
# List all memories
blockcell memory list

# Search memories
blockcell memory search "stocks"

# Memory statistics
blockcell memory stats

# Clean expired memories
blockcell memory clean
```

---

## Migrating from the old version

If you previously used the Markdown-file-based memory system (`MEMORY.md`), blockcell will automatically migrate on first start:

```rust
// agent.rs
store.migrate_from_files(&paths)
// read sections from MEMORY.md + historical daily notes
// import into SQLite
```

---

## Why SQLite + FTS5?

A common question: why not use a vector database (e.g., Chroma, Pinecone)?

Reasons blockcell chose SQLite + FTS5:

1. **Zero extra services**: no additional server required
2. **Local-first**: all data stays on-device for privacy
3. **Good enough**: FTS5 keyword search is sufficient for personal assistant memory
4. **Fast**: excellent read/write performance
5. **Reliable**: SQLite is one of the most widely used databases in the world

Vector databases can be stronger for semantic search, but they require embedding model calls (costing tokens and time). For structured memory items, keyword search is often more precise.

---

## Summary

blockcell’s memory system provides:

- **Persistence**: local SQLite storage; no loss after restart
- **Full-text search**: FTS5 supports Chinese keyword search
- **Smart injection**: injects the most relevant memory brief each chat
- **Automatic maintenance**: expiration cleanup and soft-delete trash
- **Privacy**: fully local; nothing is uploaded

With memory, blockcell becomes a personal AI assistant that understands you — not just a stateless chat tool.

---

*Previous: [The Skill system — extending AI capabilities with Rhai scripts](./04_skill_system.md)*
*Next: [Multi-channel access — Telegram/Slack/Discord/Feishu all supported](./06_channels.md)*

*Repo: https://github.com/blockcell-labs/blockcell*
*Website: https://blockcell.dev*
