# CLAUDE.md

> BlockCell → BlueClaw: 自进化 AI 多智能体框架

## 项目概述

BlockCell 是一个用 Rust 构建的自进化 AI 多智能体框架。它不只是聊天机器人，而是能真正执行任务的 AI 智能体：
读写文件、控制浏览器、分析数据、发送消息，甚至自我进化修复 bug。

## 项目结构

```text
blockcell/
├── bin/blockcell/          # CLI 入口和命令定义
├── crates/
│   ├── core/               # 核心类型、消息、能力定义
│   ├── agent/              # Agent 运行时、任务管理、事件编排
│   ├── tools/              # 50+ 内置工具实现
│   ├── skills/             # 技能引擎、版本管理、自我进化
│   ├── scheduler/          # Cron 任务、心跳、后台作业
│   ├── channels/           # 多渠道适配 (Telegram/Slack/Discord/飞书等)
│   ├── providers/          # LLM 提供商客户端
│   ├── storage/            # SQLite 存储 (会话/记忆/审计)
│   └── updater/            # 自动更新机制
├── webui/                  # Web 前端 (Vue.js)
├── skills/                 # 用户技能目录
└── docs/                   # 文档
```

## 快速开始

### 安装

```bash
# 方式一: 安装脚本 (推荐)
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/main/install.sh | sh

# 方式二: 从源码构建
cargo build -p blockcell --release
```

### 配置

```bash
blockcell setup  # 首次设置，创建 ~/.blockcell/config.json5
```

最小配置示例 (`~/.blockcell/config.json5`):

```json
{
  "providers": {
    "deepseek": {
      "apiKey": "YOUR_API_KEY",
      "apiBase": "https://api.deepseek.com"
    }
  },
  "agents": {
    "defaults": { "model": "deepseek-chat" }
  }
}
```

### 运行

```bash
blockcell status   # 检查状态
blockcell agent    # 交互模式
blockcell gateway  # 守护进程 + WebUI
```

## 架构说明

### 核心流程

```text
用户消息 → Channel Adapter → Agent Core → LLM Provider
                ↓                              ↓
            Task Manager ← Tool Execution ← Response
                ↓
            Storage (SQLite)
```

### 关键组件

| Crate       | 职责                                           |
| ----------- | ---------------------------------------------- |
| `core`      | Message, Capability, SystemEvent 等核心类型    |
| `agent`     | Agent 运行时、Intent 解析、任务调度            |
| `tools`     | 文件/浏览器/邮件/金融等 50+ 工具               |
| `skills`    | Rhai 脚本引擎、热更新、版本控制                |
| `scheduler` | Cron 作业、心跳检测、后台任务                  |
| `channels`  | Telegram/Slack/Discord/飞书/钉钉等适配器       |
| `providers` | OpenAI/DeepSeek/Anthropic 等 LLM 客户端        |
| `storage`   | SQLite 持久化 (会话、记忆、审计日志)           |

## 常用命令

```bash
# 开发
cargo build                    # 构建所有 crates
cargo build -p blockcell       # 仅构建 CLI
cargo test                     # 运行测试
cargo check                    # 快速检查 (零警告)
cargo clippy -- -D warnings    # Lint 检查

# 运行
cargo run -p blockcell -- agent      # 交互模式
cargo run -p blockcell -- gateway    # 守护进程

# 发布
cargo build -p blockcell --release   # 优化构建
```

## 开发规范

### 工作流编排

1. **Plan Mode Default**: 非平凡任务 (3+ 步骤或架构决策) 先进入计划模式
2. **Subagent Strategy**: 大量使用 subagent 保持主 context 清洁
3. **Verification Before Done**: 完成任务前必须验证 (运行测试、检查日志)
4. **Autonomous Bug Fixing**: 遇到 bug 直接修复，无需用户介入

### 核心原则

- **Simplicity First**: 每次修改尽可能简单
- **No Laziness**: 找到根本原因，不写临时修复
- **Minimal Impact**: 只触碰必要的代码
- **Layered Architecture**: UI → State → Business → Services 分层
- **Zero Warnings**: 保持 `cargo check` 无警告
- **Visual Consistency**: 使用主题系统统一 UI 组件
- **User Experience**: 复杂 UI 默认折叠显示

### 代码风格

```rust
// 错误处理: 使用 thiserror 定义具体错误
#[derive(Debug, thiserror::Error)]
pub enum MyError {
    #[error("Configuration missing: {0}")]
    ConfigMissing(String),
}

// 异步: 使用 tokio, 避免阻塞
async fn process(&self) -> Result<(), MyError> { ... }

// 日志: 使用 tracing
tracing::info!(user_id = %id, "Processing request");
```

## 测试要求

```bash
# 运行所有测试
cargo test

# 运行特定 crate 测试
cargo test -p blockcell-agent

# 运行特定测试
cargo test test_intent_mcp_validation
```

## 技术栈详情

| 类别     | 技术                                              |
| -------- | ------------------------------------------------- |
| 运行时   | Tokio (async), Rhai (scripting)                   |
| HTTP     | Axum, Tower                                       |
| 数据库   | SQLite (rusqlite)                                 |
| 序列化   | serde, serde_json, json5                          |
| LLM      | OpenAI-compatible API                             |
| 通讯     | WebSocket, Telegram Bot API, Slack Socket Mode    |
| 加密     | ed25519-dalek, sha2                               |

## 相关文档

- [Quick Start](QUICKSTART.md) - 单智能体最佳实践
- [Multi-Agent](QUICKSTART.multi-agent.md) - 多智能体路由
- [README](README.md) - 完整项目介绍
- [Docs](docs/) - 详细文档

## 关键文件

| 文件                              | 用途                   |
| --------------------------------- | ---------------------- |
| `bin/blockcell/src/commands/`     | CLI 命令实现           |
| `crates/agent/src/lib.rs`         | Agent 核心逻辑         |
| `crates/tools/src/`               | 工具实现               |
| `crates/skills/src/engine.rs`     | 技能引擎               |
| `~/.blockcell/config.json5`       | 用户配置               |
