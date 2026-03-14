# CLAUDE.md

> BlockCell → BlueClaw: 自进化 AI 多智能体框架

## 项目概述

BlockCell 是一个用 Rust 构建的自进化 AI 多智能体框架。它不只是聊天机器人，而是能真正执行任务的 AI 智能体：
读写文件、控制浏览器、分析数据、发送消息，甚至自我进化修复 bug。

### 核心概念

| 概念 | 说明 |
|------|------|
| **Agent** | 智能体运行时，负责接收消息、调用 LLM、执行工具、管理状态 |
| **Tool** | 原子能力单元，如 `read_file`、`web_fetch`、`send_message` |
| **Skill** | 组合多个工具的技能，支持 Markdown 定义 + Rhai/Python 脚本 |
| **Channel** | 外部消息渠道适配器，如 Telegram、Slack、Discord |
| **Provider** | LLM 提供商客户端，支持 OpenAI、DeepSeek、Anthropic 等 |
| **Intent** | 用户意图分类，用于路由到不同的工具集和 Agent |
| **MCP** | Model Context Protocol，用于扩展工具能力 |

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

---

## 分支与 PR 规范

### 分支命名

| 前缀 | 用途 | 示例 |
| ---- | ---- | ---- |
| `feature/` | 新功能 | `feature/feat_support_qq_channel` |
| `fix/` | Bug 修复 | `fix/telegram_rate_limit` |
| `refactor/` | 代码重构 | `refactor/agent_runtime` |
| `docs/` | 文档更新 | `docs/api_reference` |
| `chore/` | 杂项维护 | `chore/update_dependencies` |

### PR 流程

1. **代码质量检查**
   ```bash
   cargo fmt --check           # 格式检查
   cargo clippy -- -D warnings # Lint 检查，必须零警告
   cargo test                  # 所有测试通过
   ```

2. **提交规范**
   - 使用中文提交信息
   - 格式: `类型: 描述` (如 `feat: 添加 QQ 频道支持`)
   - 类型: `feat`/`fix`/`refactor`/`docs`/`chore`

3. **PR 描述模板**
   - 描述改动内容
   - 关联相关 Issue
   - 列出测试步骤

---

## 常见陷阱与最佳实践

### ❌ 常见错误

| 场景 | 错误做法 | 正确做法 |
| ---- | -------- | -------- |
| 异步调用 | 在 async 中使用阻塞调用 | 使用 `tokio::spawn` 或 `tokio::task::spawn_blocking` |
| LLM 调用 | 循环中频繁调用 | 批量处理，减少调用次数 |
| 错误处理 | 用 `unwrap()` 或 `expect()` | 使用 `?` 和 `Result` 传播错误 |
| 日志记录 | 用 `println!` | 使用 `tracing::info!`/`debug!` |
| 配置读取 | 硬编码路径 | 使用 `Config::load()` 和配置文件 |
| 内存管理 | 大量 clone | 使用 `Arc` 共享所有权 |

### ✅ 最佳实践

1. **工具开发**
   - 新工具必须实现 `Tool` trait
   - 在 `registry_builder.rs` 中注册
   - 添加对应的单元测试

2. **Channel 开发**
   - 实现 `ChannelManager` trait
   - 使用 feature flag 控制编译 (`#[cfg(feature = "xxx")]`)
   - 在 `Cargo.toml` 添加 feature 依赖

3. **错误处理**
   ```rust
   #[derive(Debug, thiserror::Error)]
   pub enum MyError {
       #[error("IO error: {0}")]
       Io(#[from] std::io::Error),
       #[error("Config missing: {0}")]
       ConfigMissing(String),
   }
   ```

4. **日志规范**
   ```rust
   // 结构化日志
   tracing::info!(
       channel = %channel_name,
       user_id = %user.id,
       "Processing message"
   );

   // 错误日志
   tracing::error!(error = %e, "Failed to process");
   ```

---

## 不要做的事

- ❌ **不要** 在 main 分支直接提交代码
- ❌ **不要** 跳过 `cargo clippy` 检查
- ❌ **不要** 在 PR 中混入无关改动
- ❌ **不要** 使用 `unwrap()` 处理可能失败的操作
- ❌ **不要** 硬编码 API 密钥或敏感信息
- ❌ **不要** 在 async 函数中执行长时间阻塞操作
- ❌ **不要** 忽略 `cargo check` 的警告

---

## 调试技巧

### 日志级别

```bash
# 设置日志级别
RUST_LOG=debug cargo run -p blockcell -- agent
RUST_LOG=blockcell_agent=trace cargo run -p blockcell -- gateway
```

### 常用调试命令

```bash
# 检查编译错误
cargo check --message-format=short

# 查看依赖树
cargo tree -p blockcell-channels

# 运行单个测试并显示输出
cargo test test_name -- --nocapture

# 检查特定 feature
cargo check -p blockcell-channels --features telegram
```

### 配置文件位置

| 文件 | 路径 |
| ---- | ---- |
| 主配置 | `~/.blockcell/config.json5` |
| 工作目录 | `~/.blockcell/workspace/` |
| 数据库 | `~/.blockcell/blockcell.db` |
| 日志 | `~/.blockcell/logs/` |

---

## Crate 详解

### `crates/core`

核心类型定义，无外部依赖。

- `Message` - 消息类型
- `Capability` - 能力定义
- `SystemEvent` - 系统事件
- `Config` - 配置结构

### `crates/agent`

Agent 运行时核心。

- `runtime.rs` - Agent 主循环
- `task_manager.rs` - 任务调度
- `intent.rs` - 意图分类
- `bus.rs` - 消息总线

### `crates/tools`

50+ 内置工具，按功能分类：

- 文件操作: `file_ops`, `fs`
- 网络: `web`, `http_request`
- 通讯: `message`, `email`
- 数据: `data_process`, `chart_generate`
- 系统: `exec`, `system_info`

### `crates/channels`

消息渠道适配器，每个渠道一个 feature：

```toml
[features]
telegram = ["teloxide"]
discord = ["serenity"]
slack = ["slack-morphism"]
```

### `crates/skills`

技能引擎和自我进化：

- `engine.rs` - Rhai 脚本引擎
- `evolution.rs` - 自我进化逻辑
- `version.rs` - 版本管理
