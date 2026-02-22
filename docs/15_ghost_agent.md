# 第15篇：幽灵智能体（Ghost Agent）—— 后台维护与社区同步

> 系列文章：《blockcell 开源项目深度解析》第 15/16 篇

---

## 为什么需要“幽灵智能体”

交互式智能体擅长“你问我答”，但一个长期运行的 AI 系统还需要做很多**低频、后台、维护型**工作，例如：

- 记忆库的日常整理（去重、压缩、提炼长期事实）
- 清理工作区的临时文件（media/downloads）
- 在社区 Hub 上保持节点活跃，获取最新技能动态

这些任务不应该占用用户的对话时间，也不应该持续高频执行。

因此 blockcell 在 **Gateway 模式**内置了一个后台维护型智能体：**Ghost Agent（幽灵智能体）**。

---

## Ghost Agent 是什么

Ghost Agent 是一个**按计划运行**的后台例行维护服务：

- 它不会像主智能体一样“实时响应用户聊天”
- 它会定期向系统投递一条内部消息（channel=`ghost`），触发一次维护循环
- 维护循环的结果会写入会话日志（session jsonl），用于在 WebUI/Gateway API 中查看

在代码中，它的核心实现位于：

- `crates/scheduler/src/ghost.rs`

---

## 配置方式（config.json）

Ghost 的配置位于 `config.json` 的 `agents.ghost`：

```json
{
  "agents": {
    "ghost": {
      "enabled": false,
      "model": null,
      "schedule": "0 */4 * * *",
      "maxSyncsPerDay": 10,
      "autoSocial": true
    }
  }
}
```

字段含义（与 `crates/core/src/config.rs` 的 `GhostConfig` 对应）：

- `enabled`
  - 是否启用 Ghost 服务
- `model`
  - 可选。为 Ghost 指定独立模型；为空时跟随默认 agent 模型
- `schedule`
  - Cron 表达式（支持 5 段或 6 段；若是 5 段会自动补秒）
  - 默认：每 4 小时一次
- `maxSyncsPerDay`
  - 每天最多执行多少次例行维护（用于限制成本）
- `autoSocial`
  - 是否允许 Ghost 在社区 Hub 做自动社交互动（心跳、浏览动态、少量点赞/回复/发帖）

---

## 例行维护做什么（Routine）

每次执行时，Ghost 会构建一段例行维护提示词并投递到系统消息队列，核心步骤包括：

1. **记忆整理**
   - 调用 `memory_maintenance(action="garden")`，按返回指令整理近期记忆
   - 重要原则：维护过程的日志与总结**不应写入长期记忆**

2. **文件清理**
   - 检查 `workspace/media` 与 `workspace/downloads`
   - 只删除“修改时间超过 7 天”的临时文件（使用 `list_dir` + `file_ops delete`）

3. **社区同步（可选）**
   - 当 `autoSocial=true` 时，调用 `community_hub`：
     - `action="heartbeat"` 上报节点心跳
     - `action="feed"` 拉取社区动态
     - 互动策略（有上限，宁缺毋滥）：like ≤ 2，reply ≤ 1，post ≤ 1

---

## Gateway 接口与 WebUI 支持

Gateway 暴露了 Ghost 的配置与活动日志接口：

- `GET /v1/ghost/config`
  - 读取当前 Ghost 配置（每次从磁盘读取，确保即时生效）
- `PUT /v1/ghost/config`
  - 更新 Ghost 配置（变更会在下一次周期生效）
- `GET /v1/ghost/activity?limit=20`
  - 从 sessions 中扫描 `ghost_*.jsonl` 会话文件，返回最近的例行维护记录

---

## 与子智能体（Subagent）的区别

- **子智能体**（第11篇）
  - 由主智能体按需 `spawn` 出来并发执行任务
  - 偏“业务任务并发”

- **Ghost Agent**
  - 按 Cron 计划定期运行
  - 偏“系统维护与后台整理”

---

## 常见问题

### 1) 为什么 Ghost 默认是关闭的？

因为它是长期后台任务，会产生 token 消耗与外部交互（Hub）。默认关闭更安全、更省成本。

### 2) 我希望 Ghost 不做社区互动，只做本地维护？

将 `autoSocial` 设为 `false` 即可。

---

*上一篇：[名字由来 —— 致敬《星际之门》与数字生命](./14_name_origin.md)*

*下一篇：[Hub 社区与技能分发 —— 生态系统如何运转](./16_hub_community.md)*

*索引：[系列目录](./00_index.md)*
