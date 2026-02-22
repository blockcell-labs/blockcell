# blockcell 技术文章系列：《一个会自我进化的 AI 智能体框架》

> 面向普通开发者和入门开发者的 blockcell 深度解析，正文共 14 篇（本页为索引）

---

## 系列简介

blockcell 是一个用 **Rust** 编写的开源 AI 智能体框架。它不只是一个聊天机器人，而是一个能真正执行任务、拥有持久记忆、可以自我进化的 AI 工作台。

本系列从零开始，带你了解 blockcell 的每一个核心特性。

blockcell 这个名字来自 **Block + Cell**（模块化基座 + 自进化细胞）的组合，完整故事见：

*番外：[名字由来](./14_name_origin.md)*

---

## 文章目录

| 篇 | 标题 | 核心内容 | 适合读者 |
|----|------|---------|---------|
| 01 | [什么是 blockcell？](./01_what_is_blockcell.md) | 整体介绍、核心特性、与 ChatGPT 的区别 | 所有人 |
| 02 | [5分钟上手](./02_quickstart.md) | 安装、配置 API Key、第一次对话 | 入门 |
| 03 | [工具系统](./03_tools_system.md) | 50+ 内置工具详解，文件/网络/金融/多媒体 | 入门 |
| 04 | [技能（Skill）系统](./04_skill_system.md) | Rhai 脚本、SKILL.md、自定义技能 | 入门 |
| 05 | [记忆系统](./05_memory_system.md) | SQLite + FTS5、持久记忆、自动注入 | 进阶 |
| 06 | [多渠道接入](./06_channels.md) | Telegram/Slack/Discord/飞书配置 | 进阶 |
| 07 | [浏览器自动化](./07_browser_automation.md) | CDP 协议、无障碍树、35+ 动作 | 进阶 |
| 08 | [Gateway 模式](./08_gateway_mode.md) | HTTP API、WebSocket、部署到服务器 | 进阶 |
| 09 | [自我进化](./09_self_evolution.md) | 错误触发、LLM 生成代码、灰度发布 | 深入 |
| 10 | [金融场景实战](./10_finance_use_case.md) | A股/加密货币监控、告警、日报 | 实战 |
| 11 | [子智能体与任务并发](./11_subagents.md) | spawn、TaskManager、非阻塞对话 | 深入 |
| 12 | [架构深度解析](./12_architecture.md) | 为什么用 Rust、Crate 结构、设计模式 | 深入 |
| 13 | [消息处理与自进化生命周期](./13_message_processing_and_evolution.md) | 从收到消息到触发进化的全过程剖析 | 深入 |
| 14 | [名字由来](./14_name_origin.md) | Block + Cell 的含义与起源 | 番外 |


---

## 推荐阅读顺序

**如果你是完全新手：** 01 → 02 → 03 → 04

**如果你想快速部署：** 01 → 02 → 06 → 08

**如果你对金融感兴趣：** 01 → 02 → 03 → 10

**如果你是开发者想贡献代码：** 01 → 12 → 04 → 09

---

## 项目信息

- **GitHub**：https://github.com/blockcell-labs/blockcell
- **官网**：https://blockcell.dev
- **License**：MIT

---

*本系列文章基于 blockcell 真实代码撰写，所有示例均可实际运行。*
