# 第11篇：子智能体与任务并发 —— 让 AI 同时做多件事

> 系列文章：《blockcell 开源项目深度解析》第 11/14 篇

---

## 问题：AI 一次只能做一件事

传统的 AI 对话是串行的：你问一个问题，AI 回答，然后你再问下一个。

但现实中，很多任务是可以并行的：

```
你: 帮我同时做三件事：
    1. 分析茅台最近一个月的走势
    2. 搜索今天的 AI 行业新闻
    3. 检查我的邮件里有没有重要的未读邮件
```

如果串行执行，可能需要 3-5 分钟。如果并行，30 秒就能完成。

blockcell 的**子智能体（Subagent）系统**解决了这个问题。

---

## 子智能体是什么

子智能体是一个**在后台独立运行的 AI 任务**。

主智能体可以派生多个子智能体，每个子智能体独立处理一个任务，完成后把结果报告给主智能体（或直接通知用户）。

```
主智能体
├── 子智能体1：分析茅台走势（后台运行）
├── 子智能体2：搜索 AI 新闻（后台运行）
└── 子智能体3：检查邮件（后台运行）
```

三个任务同时执行，互不干扰。

---

## spawn 工具：派生子智能体

派生子智能体使用 `spawn` 工具：

```json
{
  "tool": "spawn",
  "params": {
    "task": "分析茅台（600519）最近一个月的走势，计算 MA20 和 MACD，生成分析报告",
    "label": "茅台走势分析",
    "notify_on_complete": true
  }
}
```

参数说明：
- `task`：子智能体要完成的任务描述
- `label`：任务标签，用于在任务列表中识别
- `notify_on_complete`：完成后是否通知（通过当前渠道）

---

## 实际演示

```
你: 帮我同时分析茅台、平安、宁德三只股票的技术面，
    分析完成后汇总给我

AI: 好的，我来派生三个子智能体并行分析...

    [spawn] 茅台走势分析 → task_001
    [spawn] 平安走势分析 → task_002
    [spawn] 宁德走势分析 → task_003

    三个分析任务已在后台启动，预计 1-2 分钟完成。
    你可以用 /tasks 查看进度，完成后我会汇总结果。
```

在等待期间，你可以继续和 AI 对话，不会被阻塞：

```
你: /tasks

任务状态：
  ✓ 茅台走势分析 (task_001) - 完成
  ⟳ 平安走势分析 (task_002) - 运行中（已用时 45s）
  ⟳ 宁德走势分析 (task_003) - 运行中（已用时 45s）

你: 顺便帮我查一下今天的 BTC 价格
AI: 比特币当前价格：$68,523...
    [后台任务继续运行，不受影响]
```

---

## 任务管理

### `/tasks` 命令

在交互模式下，输入 `/tasks` 可以查看所有后台任务：

```
你: /tasks

任务摘要：运行中 2 | 已完成 5 | 失败 0

运行中：
  ⟳ [task_002] 平安走势分析 (已用时 1m 23s)
  ⟳ [task_003] 宁德走势分析 (已用时 1m 23s)

最近完成：
  ✓ [task_001] 茅台走势分析 (耗时 52s)
  ✓ [msg_abc]  查询BTC价格 (耗时 3s)
```

### `list_tasks` 工具

AI 也可以主动查询任务状态：

```json
{
  "tool": "list_tasks",
  "params": {
    "status": "running"
  }
}
```

---

## 非阻塞消息处理

子智能体系统还解决了另一个问题：**长时间任务不阻塞对话。**

在旧版本中，如果 AI 在执行一个需要 5 分钟的任务（比如抓取 100 个网页），你在这 5 分钟内无法和它对话。

现在，每条消息都在独立的后台任务中处理：

```rust
// runtime.rs 中的实现
async fn run_loop(&mut self) {
    loop {
        select! {
            // 收到新消息
            Some(msg) = inbound_rx.recv() => {
                let task_id = format!("msg_{}", uuid::Uuid::new_v4());
                // 立即注册任务
                task_manager.create_task(&task_id, &msg.content).await;
                // 在后台处理，不阻塞循环
                tokio::spawn(run_message_task(msg, task_id, ...));
            }
            // 定时 tick（进化、维护等）
            _ = tick_interval.tick() => {
                self.tick().await;
            }
        }
    }
}
```

这意味着你可以：
- 发送一个长任务，然后立即发送另一个问题
- 两个任务并行处理，都会得到回复

---

## 子智能体的工具限制

子智能体有一个受限的工具集，不能使用某些"危险"工具：

**子智能体可以使用：**
- 文件读写（工作目录内）
- 网络请求（web_search, web_fetch, http_request）
- 数据处理（data_process, chart_generate）
- 金融数据（finance_api, blockchain_rpc）
- 浏览器（browse）
- 查询任务状态（list_tasks）

**子智能体不能使用：**
- `spawn`（不能再派生子智能体，防止无限递归）
- `message`（不能直接发消息到渠道）
- `cron`（不能创建定时任务）

这个限制防止了子智能体失控，保证了系统的可控性。

---

## 实际应用场景

### 场景一：并行数据收集

```
你: 帮我收集以下 10 个竞争对手的官网信息，
    整理成一份对比表格

AI: 我来派生 10 个子智能体并行抓取...
    [spawn × 10]
    预计 2 分钟完成，完成后生成对比表格。
```

### 场景二：多市场同步监控

```
你: 帮我同时监控 A 股、港股、美股的开盘情况，
    有异常波动立即告诉我

AI: 派生三个监控子智能体...
    [spawn] A股开盘监控
    [spawn] 港股开盘监控
    [spawn] 美股开盘监控（美东时间）
```

### 场景三：长报告生成

```
你: 帮我写一份 2025 年 AI 行业深度报告，
    需要搜索大量资料，可能需要一段时间

AI: 好的，我在后台开始生成，
    这个任务预计需要 5-10 分钟。
    你可以继续做其他事情，完成后我会通知你。

    [spawn] AI行业深度报告 → task_xyz

你: 好的，顺便帮我查一下今天的天气
AI: 今天北京天气：晴，15°C...
    [后台报告生成继续进行]
```

---

## TaskManager：任务追踪系统

所有任务（包括普通消息和子智能体任务）都由 `TaskManager` 统一管理：

```rust
struct TaskInfo {
    id: String,
    label: String,
    status: TaskStatus,  // Queued / Running / Completed / Failed
    created_at: DateTime,
    started_at: Option<DateTime>,
    completed_at: Option<DateTime>,
    progress: Option<String>,  // 进度描述
    result: Option<String>,    // 完成结果摘要
    error: Option<String>,     // 失败原因
    origin_channel: Option<String>,  // 来自哪个渠道
}
```

任务完成后，结果通过 `outbound_tx` 发送回原始渠道（CLI、Telegram、Slack 等）。

---

## 与 Gateway 模式结合

在 Gateway 模式下，可以通过 HTTP API 查询任务状态：

```bash
# 查询所有运行中的任务
curl http://localhost:18790/v1/tasks?status=running \
  -H "Authorization: Bearer 你的token"

# 响应
{
  "tasks": [
    {
      "id": "task_001",
      "label": "茅台走势分析",
      "status": "running",
      "started_at": "2025-02-18T08:30:00Z",
      "progress": "正在计算技术指标..."
    }
  ]
}
```

---

## 小结

blockcell 的子智能体系统：

- **并发执行**：多个任务同时运行，互不阻塞
- **非阻塞对话**：长任务在后台，不影响继续聊天
- **任务追踪**：`/tasks` 命令随时查看进度
- **安全隔离**：子智能体有受限工具集，防止失控
- **跨渠道通知**：任务完成后通过原始渠道通知用户

这让 blockcell 从一个"一次只能做一件事"的助手，变成了一个真正的**多任务 AI 工作台**。
---

*上一篇：[金融场景实战 —— 用 blockcell 监控股票和加密货币](./10_finance_use_case.md)*
*下一篇：[blockcell 架构深度解析 —— 为什么用 Rust 写 AI 框架](./12_architecture.md)*

*项目地址：https://github.com/blockcell-labs/blockcell*
*官网：https://blockcell.dev*
