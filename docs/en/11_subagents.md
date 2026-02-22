# Article 11: Subagents and Task Concurrency — Let AI Do Multiple Things at Once

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — 11/14

---

## The problem: AI can only do one thing at a time

Traditional chat-based AI is serial: you ask a question, the AI answers, then you ask the next.

But in real life, many tasks can be done in parallel:

```
You: Do three things at the same time:
    1) Analyze Moutai’s last-month trend
    2) Search today’s AI industry news
    3) Check if I have important unread emails
```

If done serially, this might take 3–5 minutes. If done in parallel, it could finish in ~30 seconds.

blockcell’s **Subagent system** is built to solve this.

---

## What is a subagent?

A subagent is an **independent AI task running in the background**.

The main agent can spawn multiple subagents. Each subagent handles one task independently and reports the result back to the main agent (or directly notifies you).

```
Main agent
├── Subagent 1: analyze Moutai trend (background)
├── Subagent 2: search AI news (background)
└── Subagent 3: check emails (background)
```

All three tasks run concurrently without blocking each other.

---

## The `spawn` tool: creating subagents

You spawn subagents using the `spawn` tool:

```json
{
  "tool": "spawn",
  "params": {
    "task": "Analyze Moutai (600519) over the last month, compute MA20 and MACD, and generate a report",
    "label": "Moutai technical analysis",
    "notify_on_complete": true
  }
}
```

Parameters:
- `task`: task description for the subagent
- `label`: a human-readable label shown in task lists
- `notify_on_complete`: whether to notify via the current channel when finished

---

## A practical demo

```
You: Analyze the technicals of Moutai, Ping An, and CATL in parallel,
    then summarize the results for me.

AI: Sure — I’ll spawn three subagents to analyze in parallel...

    [spawn] Moutai analysis → task_001
    [spawn] Ping An analysis → task_002
    [spawn] CATL analysis → task_003

    All three tasks are running in the background; ETA 1–2 minutes.
    You can type /tasks to check progress. I’ll summarize once all are done.
```

While waiting, you can continue chatting without being blocked:

```
You: /tasks

Task status:
  ✓ Moutai analysis (task_001) - completed
  ⟳ Ping An analysis (task_002) - running (45s elapsed)
  ⟳ CATL analysis (task_003) - running (45s elapsed)

You: Also, what’s BTC price today?
AI: BTC current price: $68,523...
    [background tasks continue running]
```

---

## Task management

### The `/tasks` command

In interactive mode, type `/tasks` to view all background tasks:

```
You: /tasks

Summary: running 2 | completed 5 | failed 0

Running:
  ⟳ [task_002] Ping An analysis (1m 23s elapsed)
  ⟳ [task_003] CATL analysis (1m 23s elapsed)

Recently completed:
  ✓ [task_001] Moutai analysis (52s)
  ✓ [msg_abc] BTC price query (3s)
```

### The `list_tasks` tool

The AI can also query task status proactively:

```json
{
  "tool": "list_tasks",
  "params": {
    "status": "running"
  }
}
```

---

## Non-blocking message handling

The subagent system also solves another issue: **long-running tasks no longer block the chat**.

In older designs, if the AI ran a task that took 5 minutes (e.g., fetching 100 web pages), you couldn’t chat during those 5 minutes.

Now, each incoming message is processed in its own background task:

```rust
// implementation in runtime.rs
async fn run_loop(&mut self) {
    loop {
        select! {
            // new inbound message
            Some(msg) = inbound_rx.recv() => {
                let task_id = format!("msg_{}", uuid::Uuid::new_v4());
                // register task immediately
                task_manager.create_task(&task_id, &msg.content).await;
                // process in background, without blocking the loop
                tokio::spawn(run_message_task(msg, task_id, ...));
            }
            // periodic tick (evolution, maintenance, etc.)
            _ = tick_interval.tick() => {
                self.tick().await;
            }
        }
    }
}
```

This means you can:
- send a long task, then immediately ask another question
- have both run concurrently and receive both results

---

## Tool restrictions for subagents

Subagents run with a restricted toolset and cannot use certain “dangerous” tools.

**Subagents can use:**
- file operations (inside the workspace)
- network tools (`web_search`, `web_fetch`, `http_request`)
- data processing (`data_process`, `chart_generate`)
- finance tools (`finance_api`, `blockchain_rpc`)
- browser automation (`browse`)
- task queries (`list_tasks`)

**Subagents cannot use:**
- `spawn` (cannot spawn more subagents; prevents infinite recursion)
- `message` (cannot send messages directly to channels)
- `cron` (cannot create scheduled tasks)

This prevents subagents from running out of control and keeps the system manageable.

---

## Practical scenarios

### Scenario 1: parallel data collection

```
You: Collect info from 10 competitor websites
    and compile a comparison table

AI: I’ll spawn 10 subagents to crawl in parallel...
    [spawn × 10]
    ETA ~2 minutes; then I’ll generate the comparison table.
```

### Scenario 2: multi-market monitoring

```
You: Monitor CN, HK, and US market open simultaneously.
    Notify me if there are abnormal moves.

AI: Spawning three monitoring subagents...
    [spawn] CN market open
    [spawn] HK market open
    [spawn] US market open (Eastern time)
```

### Scenario 3: long report generation

```
You: Write an in-depth AI industry report for 2025.
    It may require a lot of research.

AI: Sure — I’ll start generating it in the background.
    ETA 5–10 minutes. You can keep doing other things.
    I’ll notify you when it’s done.

    [spawn] AI industry report → task_xyz

You: By the way, what’s today’s weather?
AI: Beijing: sunny, 15°C...
    [background report generation continues]
```

---

## TaskManager: unified task tracking

All tasks (normal messages and subagent tasks) are managed by `TaskManager`:

```rust
struct TaskInfo {
    id: String,
    label: String,
    status: TaskStatus,  // Queued / Running / Completed / Failed
    created_at: DateTime,
    started_at: Option<DateTime>,
    completed_at: Option<DateTime>,
    progress: Option<String>,      // progress description
    result: Option<String>,        // result preview
    error: Option<String>,         // failure reason
    origin_channel: Option<String> // originating channel
}
```

When a task completes, the result is routed back through `outbound_tx` to the originating channel (CLI, Telegram, Slack, etc.).

---

## Combined with Gateway mode

In Gateway mode, you can query task status via HTTP API:

```bash
# list running tasks
curl http://localhost:18790/v1/tasks?status=running \
  -H "Authorization: Bearer YOUR_TOKEN"

# response
{
  "tasks": [
    {
      "id": "task_001",
      "label": "Moutai analysis",
      "status": "running",
      "started_at": "2025-02-18T08:30:00Z",
      "progress": "computing technical indicators..."
    }
  ]
}
```

---

## Summary

blockcell’s subagent system provides:

- **Concurrency**: multiple tasks run at the same time
- **Non-blocking chat**: long tasks run in the background
- **Task observability**: `/tasks` shows progress anytime
- **Safety isolation**: subagents have restricted tools
- **Cross-channel notification**: results go back to the original channel

This turns blockcell from a “one-task-at-a-time assistant” into a true **multi-task AI workbench**.

---

*Previous: [Finance in practice — monitoring stocks and crypto with blockcell](./10_finance_use_case.md)*
*Next: [blockcell architecture deep dive — why Rust for an AI framework](./12_architecture.md)*

*Repo: https://github.com/blockcell-labs/blockcell*
*Website: https://blockcell.dev*
