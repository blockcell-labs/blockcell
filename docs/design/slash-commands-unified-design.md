# BlockCell 斜杠命令统一设计方案

> 在 Gateway 模式及所有 Channel 中支持斜杠命令的设计文档

---

## 目录

1. [概述](#一概述)
2. [现状分析](#二现状分析)
3. [设计目标](#三设计目标)
4. [架构设计](#四架构设计)
5. [实现方案](#五实现方案)
6. [命令列表](#六命令列表)
7. [WebUI 集成](#七webui-集成)
8. [渠道特定命令处理](#八渠道特定命令处理)
9. [安全考量](#九安全考量)
10. [测试计划](#十测试计划)

---

## 一、概述

### 1.1 背景

BlockCell 目前支持多种消息输入渠道：
- **CLI 模式** (`blockcell agent`): 通过 stdin 交互
- **Gateway 模式**: 通过 WebSocket 连接
- **Channel**: Telegram、Slack、Discord、飞书、钉钉等

当前斜杠命令（如 `/help`、`/tasks`）仅在 CLI 模式的 stdin 线程中实现，Gateway 和 Channel 用户无法使用这些便捷命令。

### 1.2 目标

设计统一的斜杠命令处理机制，使所有渠道的用户都能：
- 快速查询系统状态
- 执行常用操作
- 大部分命令零 Token 消耗（不经过 LLM）

---

## 二、现状分析

### 2.1 当前架构

```
┌─────────────────────────────────────────────────────────────────┐
│                        消息流现状                                │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  CLI (stdin)                                                    │
│  └─► stdin 线程 ──► 解析斜杠命令 ──► 本地执行                   │
│                    │                                            │
│                    └─► 非命令 ──► AgentRuntime ──► LLM          │
│                                                                 │
│  Gateway (WebSocket)                                            │
│  └─► websocket.rs ──► InboundMessage ──► AgentRuntime ──► LLM   │
│       (无斜杠命令拦截)                                          │
│                                                                 │
│  Channel (Telegram/Slack/...)                                   │
│  └─► channel.rs ──► InboundMessage ──► AgentRuntime ──► LLM     │
│       (无斜杠命令拦截)                                          │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 现有命令实现位置

当前斜杠命令在 `bin/blockcell/src/commands/agent.rs` 的 stdin 线程中硬编码实现：

```rust
// bin/blockcell/src/commands/agent.rs:850-989
// /help, /tasks, /skills, /tools, /learn, /clear, /clear-skills, /forget-skill, /quit
```

### 2.3 问题

| 渠道 | 斜杠命令支持 | 原因 |
|------|-------------|------|
| CLI | ✅ 支持 | stdin 线程有命令解析逻辑 |
| Gateway | ❌ 不支持 | 消息直接透传给 AgentRuntime |
| Channel | ❌ 不支持 | 消息直接透传给 AgentRuntime |

---

## 三、设计目标

### 3.1 功能目标

- 所有渠道统一支持斜杠命令
- 大部分命令执行不消耗 Token（`/learn` 例外）
- 响应速度快（本地执行）
- 命令列表可扩展

### 3.2 非功能目标

- 命令执行时间 < 100ms
- 不影响正常消息处理性能
- 支持命令权限控制

### 3.3 设计原则

1. **统一处理**: 所有渠道共享同一套命令处理逻辑
2. **模块化**: 命令处理器独立于消息渠道
3. **可扩展**: 新增命令只需注册，无需修改核心逻辑
4. **安全性**: 敏感命令需要权限验证
5. **位置统一**: 模块放在 `bin/blockcell/src/commands/slash_commands/`

---

## 四、架构设计

### 4.1 新架构

```
┌─────────────────────────────────────────────────────────────────┐
│                        消息流新架构                              │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  CLI (stdin)                                                    │
│  └─► stdin 线程 ──► SlashCommandHandler ──► 本地执行            │
│                         │                                       │
│                         └─► 非命令 ──► AgentRuntime ──► LLM     │
│                                                                 │
│  Gateway (WebSocket)                                            │
│  └─► websocket.rs ──► SlashCommandHandler ──► 本地执行          │
│                         │        ▲                              │
│                         │        │                              │
│                         └─► 非命令┘──► AgentRuntime ──► LLM     │
│                                                                 │
│  Channel (Telegram/Slack/...)                                   │
│  └─► gateway.rs ──► SlashCommandHandler ──► 本地执行            │
│                         │        ▲                              │
│                         │        │                              │
│                         └─► 非命令┘──► AgentRuntime ──► LLM     │
│                                                                 │
│                         ▲                                       │
│                         │                                       │
│            ┌────────────┴────────────┐                          │
│            │   SlashCommandHandler   │                          │
│            │   (统一命令处理器)       │                          │
│            └─────────────────────────┘                          │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### 4.2 核心组件

```
bin/blockcell/src/commands/
├── slash_commands/
│   ├── mod.rs              # 统一入口，命令路由
│   ├── context.rs          # 命令执行上下文
│   ├── registry.rs         # 命令注册表
│   ├── handlers/           # 各命令处理器
│   │   ├── help.rs         # /help
│   │   ├── tasks.rs        # /tasks
│   │   ├── skills.rs       # /skills
│   │   ├── tools.rs        # /tools
│   │   ├── clear.rs        # /clear (新增完整实现)
│   │   ├── learn.rs        # /learn
│   │   ├── quit.rs         # /quit, /exit
│   │   └── skill_mgmt.rs   # /clear-skills, /forget-skill
│   └── output.rs           # 输出格式化
```

### 4.3 数据流

```
Channel 消息到达 (Telegram/Slack/...)
    │
    ▼
┌─────────────────────┐
│  allowFrom 检查     │  ← Channel 层：白名单验证（现有机制）
│  (在 channel.rs)    │
└─────────┬───────────┘
          │
    ┌─────┴─────┐
    │           │
  拒绝        通过
    │           │
    ▼           ▼
  忽略    发送 InboundMessage
              │
              ▼
        ┌─────────────────────┐
        │ Gateway Interceptor │  ← Gateway 层：统一拦截
        │ (slash_commands)    │
        └─────────┬───────────┘
                  │
            ┌─────┴─────┐
            │           │
        是斜杠命令   非斜杠命令
            │           │
            ▼           ▼
        本地执行    AgentRuntime
            │
            ▼
        返回结果到原渠道
```

**关键点**：

- **allowFrom 在前**：安全第一，非白名单用户完全无法交互
- **斜杠命令在后**：只有通过 allowFrom 的用户才能使用命令
- **统一拦截层**：在 Gateway 层统一处理，Channel 无需额外实现

---

## 五、实现方案

### 5.1 核心接口定义

```rust
// bin/blockcell/src/commands/slash_commands/mod.rs

use blockcell_core::{InboundMessage, OutboundMessage, Paths};
use blockcell_agent::TaskManager;
use std::sync::Arc;

/// 命令执行上下文
#[derive(Default)]
pub struct CommandContext {
    /// 工作路径
    pub paths: Paths,
    /// 任务管理器
    pub task_manager: Option<TaskManager>,
    /// 原始消息来源
    pub source: CommandSource,
    /// 会话清除回调（用于 /clear 命令）
    pub session_clear_callback: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl CommandContext {
    /// 创建测试用上下文
    pub fn test_context() -> Self {
        Self {
            source: CommandSource {
                channel: "cli".to_string(),
                chat_id: "test-chat".to_string(),
                sender_id: Some("test-user".to_string()),
            },
            ..Default::default()
        }
    }
}

/// 命令来源
#[derive(Debug, Clone, Default)]
pub struct CommandSource {
    /// 渠道类型: "cli", "ws", "telegram", "slack", etc.
    pub channel: String,
    /// 会话 ID
    pub chat_id: String,
    /// 用户 ID (可选)
    pub sender_id: Option<String>,
}

/// 命令处理结果
pub enum CommandResult {
    /// 命令已处理，返回响应
    Handled(CommandResponse),
    /// 非斜杠命令，交给下游处理
    NotACommand,
    /// 命令需要权限，拒绝执行
    PermissionDenied(String),
    /// 命令执行错误
    Error(String),
    /// 请求退出交互模式 (仅 /quit 和 /exit)
    ExitRequested,
    /// 命令需要转发给 AgentRuntime 处理（如 /learn）
    ///
    /// 用于那些需要 LLM 参与的命令。命令处理器会将原始命令转换为
    /// AgentRuntime 可理解的消息格式，然后由各渠道的拦截层转发。
    ForwardToRuntime {
        /// 转换后的消息内容，供 AgentRuntime 使用
        transformed_content: String,
        /// 原始命令内容（用于日志）
        original_command: String,
    },
}

/// 命令响应
pub struct CommandResponse {
    /// 响应内容
    pub content: String,
    /// 是否为 Markdown 格式
    pub is_markdown: bool,
}

/// 斜杠命令处理器 trait
#[async_trait::async_trait]
pub trait SlashCommand: Send + Sync {
    /// 命令名称 (不含斜杠)
    fn name(&self) -> &str;

    /// 命令描述 (用于 /help 显示)
    fn description(&self) -> &str;

    /// 是否需要权限验证
    fn requires_permission(&self) -> bool {
        false
    }

    /// 支持的渠道列表 (None 表示所有渠道)
    fn available_channels(&self) -> Option<Vec<&'static str>> {
        None
    }

    /// 命令执行超时时间（秒），默认 10 秒
    /// 注意：/learn 命令会调用 LLM，需要更长超时
    fn timeout_secs(&self) -> u64 {
        10
    }

    /// 执行命令
    async fn execute(&self, args: &str, ctx: &CommandContext) -> CommandResult;
}

/// 统一命令处理器
pub struct SlashCommandHandler {
    commands: Vec<Box<dyn SlashCommand>>,
}

impl SlashCommandHandler {
    /// 创建新的处理器
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }
    
    /// 注册命令
    pub fn register<C: SlashCommand + 'static>(&mut self, command: C) {
        self.commands.push(Box::new(command));
    }
    
    /// 尝试处理输入
    pub async fn try_handle(
        &self,
        input: &str,
        ctx: &CommandContext,
    ) -> CommandResult {
        let input = input.trim();

        // 检查是否为斜杠命令
        if !input.starts_with('/') {
            return CommandResult::NotACommand;
        }

        // 解析命令和参数
        let (cmd_name, args) = if let Some(space_pos) = input.find(' ') {
            (&input[1..space_pos], &input[space_pos + 1..])
        } else {
            (&input[1..], "")
        };

        // 查找命令处理器
        for command in &self.commands {
            if command.name() == cmd_name {
                // 渠道限制检查
                if let Some(channels) = command.available_channels() {
                    if !channels.iter().any(|c| *c == ctx.source.channel) {
                        return CommandResult::Handled(CommandResponse {
                            content: format!("命令 /{} 仅在 {} 模式可用", cmd_name, channels.join(", ")),
                            is_markdown: false,
                        });
                    }
                }

                // 权限检查
                if command.requires_permission() {
                    // TODO: 实现权限验证
                }

                // 带超时执行命令
                let timeout_duration = std::time::Duration::from_secs(command.timeout_secs());
                return match tokio::time::timeout(timeout_duration, command.execute(args, ctx)).await {
                    Ok(result) => result,
                    Err(_) => CommandResult::Error(format!(
                        "命令 /{} 执行超时 ({}秒)",
                        cmd_name,
                        command.timeout_secs()
                    )),
                };
            }
        }

        // 未知命令
        CommandResult::Handled(CommandResponse {
            content: format!("未知命令: /{}。输入 /help 查看可用命令。", cmd_name),
            is_markdown: false,
        })
    }
}
```

### 5.2 命令注册

```rust
// bin/blockcell/src/commands/slash_commands/registry.rs

use super::*;

/// 创建默认命令处理器
pub fn create_default_handler() -> SlashCommandHandler {
    let mut handler = SlashCommandHandler::new();
    
    // 注册内置命令
    handler.register(HelpCommand);
    handler.register(TasksCommand);
    handler.register(SkillsCommand);
    handler.register(ToolsCommand);
    handler.register(ClearCommand);
    handler.register(LearnCommand);
    handler.register(QuitCommand);
    handler.register(ClearSkillsCommand);
    handler.register(ForgetSkillCommand);
    
    handler
}

/// 全局命令处理器实例
pub static SLASH_COMMAND_HANDLER: once_cell::sync::Lazy<Arc<SlashCommandHandler>> =
    once_cell::sync::Lazy::new(|| Arc::new(create_default_handler()));
```

> **设计说明**: 使用 `once_cell::sync::Lazy` 替代 `lazy_static`，更现代且无需宏依赖。Rust 1.70+ 可使用 `std::sync::OnceLock`。

### 5.3 ForwardToRuntime 机制与 /learn 命令

> **设计背景**: 某些斜杠命令需要 LLM 参与（如 `/learn`），但又需要在命令处理器中做参数验证和消息转换。为此引入 `ForwardToRuntime` 变体。

#### 5.3.1 设计原理

`/learn` 命令的处理流程如下：

```
用户输入: /learn 数据分析技能
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ SlashCommandHandler.try_handle()                             │
│                                                              │
│  1. 解析命令: name="learn", args="数据分析技能"              │
│  2. 找到 LearnCommand 处理器                                  │
│  3. 调用 LearnCommand.execute("数据分析技能", ctx)           │
│     │                                                        │
│     ▼                                                        │
│  LearnCommand:                                               │
│    - 验证参数非空                                             │
│    - 构造转换后的消息:                                        │
│      "Please learn the following skill: 数据分析技能         │
│       If this skill is already learned... Otherwise..."      │
│    - 返回 CommandResult::ForwardToRuntime {                  │
│        transformed_content: "Please learn...",               │
│        original_command: "/learn 数据分析技能"               │
│      }                                                       │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 拦截层处理 (CLI/Gateway/WebSocket)                           │
│                                                              │
│  match result {                                              │
│    ForwardToRuntime { transformed_content, .. } => {        │
│      // 使用转换后的内容创建 InboundMessage                   │
│      inbound.content = transformed_content;                  │
│      // 转发给 AgentRuntime                                  │
│    }                                                         │
│  }                                                           │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ AgentRuntime                                                 │
│                                                              │
│  接收转换后的消息: "Please learn the following skill: ..."   │
│  调用 LLM 处理技能学习请求                                    │
└─────────────────────────────────────────────────────────────┘
```

#### 5.3.2 为什么不直接返回 NotACommand？

**旧方案的问题**（已废弃）：
```rust
// LearnCommand 旧实现
async fn execute(&self, args: &str, _ctx: &CommandContext) -> CommandResult {
    // ...验证参数
    CommandResult::NotACommand  // 返回"不是命令"
}

// CLI 层需要再次检查
if input.starts_with("/learn ") {
    let description = input.trim_start_matches("/learn ");
    let learn_msg = format!("Please learn...{}", description);
    // 转发
}
```

问题：
1. **语义混乱**: 有效命令返回 `NotACommand`，违反直觉
2. **重复逻辑**: 消息转换逻辑需要在 CLI/Gateway/WebSocket 三处重复实现
3. **易出错**: 忘记在某个渠道添加转换逻辑会导致功能缺失

**新方案的优势**：
```rust
// LearnCommand 新实现
async fn execute(&self, args: &str, _ctx: &CommandContext) -> CommandResult {
    let transformed_content = format!(
        "Please learn the following skill: {}\n\n\
        If this skill is already learned (has a record in list_skills query=learned), \
        just tell me it's done.\n\
        Otherwise, start learning this skill and report progress.",
        args.trim()
    );
    CommandResult::ForwardToRuntime {
        transformed_content,
        original_command: format!("/learn {}", args.trim()),
    }
}
```

优势：
1. **语义清晰**: 明确表示"转发给 Runtime"
2. **单一职责**: 消息转换逻辑只在命令处理器中实现一次
3. **类型安全**: 编译器强制所有渠道处理 `ForwardToRuntime` 变体
4. **可追踪**: 包含 `original_command` 用于日志

#### 5.3.3 LearnCommand 完整实现

```rust
// bin/blockcell/src/commands/slash_commands/handlers/learn.rs

pub struct LearnCommand;

#[async_trait::async_trait]
impl SlashCommand for LearnCommand {
    fn name(&self) -> &str { "learn" }
    
    fn description(&self) -> &str {
        "Learn a new skill by description (uses LLM)"
    }
    
    fn timeout_secs(&self) -> u64 { 120 }

    async fn execute(&self, args: &str, _ctx: &CommandContext) -> CommandResult {
        let description = args.trim();

        if description.is_empty() {
            return CommandResult::Handled(CommandResponse::text(
                "  Usage: /learn <skill description>\n".to_string(),
            ));
        }

        // 构造转换后的消息内容
        let transformed_content = format!(
            "Please learn the following skill: {}\n\n\
            If this skill is already learned (has a record in list_skills query=learned), \
            just tell me it's done.\n\
            Otherwise, start learning this skill and report progress.",
            description
        );

        CommandResult::ForwardToRuntime {
            transformed_content,
            original_command: format!("/learn {}", description),
        }
    }
}
```

#### 5.3.4 各渠道处理 ForwardToRuntime

**CLI (agent.rs)**:
```rust
CommandResult::ForwardToRuntime {
    transformed_content,
    original_command,
} => {
    tracing::info!(command = %original_command, "Forwarding command to AgentRuntime");
    let inbound = InboundMessage {
        content: transformed_content,  // 使用转换后的内容
        ...
    };
    stdin_tx.blocking_send(inbound);
    continue;
}
```

**Gateway (gateway.rs)**:
```rust
CommandResult::ForwardToRuntime { transformed_content, .. } => {
    msg.content = transformed_content;  // 替换消息内容
    // 继续正常流程，转发给 AgentRuntime
}
```

**WebSocket (websocket.rs)**:
```rust
CommandResult::ForwardToRuntime { transformed_content, .. } => {
    content = transformed_content;  // 替换内容
    // 继续正常流程
}
```

### 5.4 /clear 命令完整实现

> **设计说明**: AgentRuntime 的消息历史存储在 `run_loop` 的局部变量中，无法直接访问。因此 `/clear` 采用回调模式 + 文件清理的组合方案。

```rust
// bin/blockcell/src/commands/slash_commands/handlers/clear.rs

use super::*;

/// /clear 命令 - 清除当前会话历史
pub struct ClearCommand;

#[async_trait::async_trait]
impl SlashCommand for ClearCommand {
    fn name(&self) -> &str {
        "clear"
    }

    fn description(&self) -> &str {
        "Clear current session history"
    }

    async fn execute(&self, _args: &str, ctx: &CommandContext) -> CommandResult {
        let mut results = Vec::new();

        // 1. 调用 AgentRuntime 注册的清除回调（如果存在）
        // 回调由 AgentRuntime 在启动时注册，用于清除内存中的消息历史
        if let Some(ref callback) = ctx.session_clear_callback {
            if callback() {
                results.push("✅ 会话内存状态已清除");
            } else {
                results.push("⚠️ 会话内存清除失败");
            }
        } else {
            results.push("ℹ️ 无内存清除回调（可能是 Gateway 模式）");
        }

        // 2. 清除 Session Memory 文件 (Layer 3)
        let session_memory_path = ctx.paths.workspace_dir()
            .join("sessions")
            .join(&ctx.source.chat_id)
            .join("memory.md");

        if session_memory_path.exists() {
            match tokio::fs::remove_file(&session_memory_path).await {
                Ok(_) => results.push("✅ Session Memory 文件已删除"),
                Err(e) => results.push(&format!("⚠️ Session Memory 删除失败: {}", e)),
            }
        }

        // 3. 清除 .active 标记文件
        let active_file = ctx.paths.workspace_dir()
            .join("sessions")
            .join(&ctx.source.chat_id)
            .join(".active");

        if active_file.exists() {
            let _ = tokio::fs::remove_file(&active_file).await;
        }

        // 4. 清除 Session Cache (SQLite 中的会话缓存，如果存在)
        // 注：Session Cache 有 TTL，会自动过期，这里不主动清除

        // 5. 构建响应
        let content = if results.is_empty() {
            "✅ 会话历史已清除 (无持久化数据)".to_string()
        } else {
            format!("📋 会话清除结果:\n{}", results.join("\n"))
        };

        CommandResult::Handled(CommandResponse {
            content,
            is_markdown: true,
        })
    }
}
```

**AgentRuntime 侧的回调注册** (在 `runtime.rs` 或 `gateway.rs` 中实现):

```rust
// 在 spawn_agent_runtime 或 AgentRuntime 中创建清除回调
let session_clear_callback: Arc<dyn Fn() -> bool + Send + Sync> = {
    // 注：AgentRuntime 的消息历史在 run_loop 的局部变量 current_messages 中
    // 无法直接访问，需要通过消息通道或状态共享来实现

    // 方案 1: 使用共享的 AtomicBool 标记（推荐）
    // 在 run_loop 中检查标记，收到信号后在下一次循环清除消息
    let clear_flag = Arc::new(AtomicBool::new(false));
    let flag_clone = clear_flag.clone();

    Arc::new(move || {
        flag_clone.store(true, Ordering::SeqCst);
        true
    })
};

// 在 run_loop 中检查标记
loop {
    if clear_flag.load(Ordering::SeqCst) {
        current_messages.clear();
        clear_flag.store(false, Ordering::SeqCst);
        info!("[runtime] Session cleared by /clear command");
    }
    // ... 正常消息处理
}
```

> **注意**: 完整实现需要在 AgentRuntime 中添加清除标记字段和检查逻辑。P1 阶段可以先只清除文件，P2 阶段再完善内存清除。

### 5.4 Gateway 集成

> **重要**: 代码插入位置必须准确，否则会破坏现有功能。以下位置基于现有代码结构分析。

#### 5.4.1 WebSocket 消息拦截

**文件**: `bin/blockcell/src/commands/gateway/websocket.rs`
**位置**: 在 "chat" 消息处理分支中，约第 142-155 行

```rust
// websocket.rs - 在 "chat" 分支中添加斜杠命令拦截
"chat" => {
    let content = parsed.get("content").and_then(|v| v.as_str()).unwrap_or("");

    // 新增：斜杠命令拦截（在创建 InboundMessage 之前）
    if content.starts_with('/') {
        let ctx = CommandContext {
            paths: state.paths.clone(),
            task_manager: Some(state.task_manager.clone()),
            source: CommandSource {
                channel: "ws".to_string(),
                chat_id: chat_id.clone(),
                sender_id: Some("user".to_string()),
            },
            session_clear_callback: None,
        };

        match SLASH_COMMAND_HANDLER.try_handle(content, &ctx).await {
            CommandResult::Handled(response) => {
                // 复用 message_done 事件（前端已支持）
                let event = serde_json::json!({
                    "type": "message_done",
                    "chat_id": chat_id,
                    "content": response.content,
                    "task_id": "",
                });
                let _ = state.ws_broadcast.send(event.to_string());
                continue;  // 不转发给 AgentRuntime
            }
            CommandResult::NotACommand => {
                // 非斜杠命令，继续正常流程
            }
            CommandResult::PermissionDenied(msg) => {
                let _ = state.ws_broadcast.send(serde_json::json!({
                    "type": "error",
                    "chat_id": chat_id,
                    "message": format!("权限不足: {}", msg),
                }).to_string());
                continue;
            }
            CommandResult::Error(e) => {
                let _ = state.ws_broadcast.send(serde_json::json!({
                    "type": "error",
                    "chat_id": chat_id,
                    "message": format!("命令执行错误: {}", e),
                }).to_string());
                continue;
            }
        }
    }

    // 原有逻辑：创建 InboundMessage 并发送
    let inbound = InboundMessage {
        channel: "ws".to_string(),
        chat_id: chat_id.clone(),
        content: content.to_string(),
        ...
    };
    if let Err(e) = inbound_tx.send(inbound).await {
        warn!(error = %e, "Failed to send inbound message");
    }
}
```

#### 5.4.2 Channel 消息拦截

**文件**: `bin/blockcell/src/commands/gateway.rs`
**位置**: 在现有 `interceptor_handle` 的 loop 内部，约第 1267 行后（confirm reply 检查之后）

```rust
// gateway.rs - 在现有 interceptor_handle 的 loop 中添加
let interceptor_handle = tokio::spawn(async move {
    let mut inbound_rx = inbound_rx;
    loop {
        let msg = tokio::select! {
            msg = inbound_rx.recv() => match msg {
                Some(m) => m,
                None => break,
            },
            _ = interceptor_shutdown_rx.recv() => break,
        };

        // 现有逻辑：检查 pending channel confirm replies
        if !is_internal_channel(&msg.channel) {
            let confirm_key = format!("{}:{}", msg.channel, msg.chat_id);
            let maybe_tx = {
                let mut map = pending_ch_for_interceptor.lock().await;
                map.remove(&confirm_key)
            };
            if let Some(tx) = maybe_tx {
                // ... 现有 confirm reply 处理逻辑
                continue;
            }
        }

        // 新增：斜杠命令拦截（在 confirm reply 检查之后，转发给 runtime 之前）
        if !is_internal_channel(&msg.channel) && msg.content.starts_with('/') {
            let ctx = CommandContext {
                paths: paths.clone(),
                task_manager: Some(task_manager.clone()),
                source: CommandSource {
                    channel: msg.channel.clone(),
                    chat_id: msg.chat_id.clone(),
                    sender_id: Some(msg.sender_id.clone()),
                },
                session_clear_callback: None,
            };

            match SLASH_COMMAND_HANDLER.try_handle(&msg.content, &ctx).await {
                CommandResult::Handled(response) => {
                    // 发送响应回原渠道
                    let reply = OutboundMessage::new(&msg.channel, &msg.chat_id, &response.content);
                    if let Err(e) = outbound_tx.send(reply).await {
                        warn!(error = %e, "Failed to send command response");
                    }
                    continue;  // 不转发给 AgentRuntime
                }
                CommandResult::NotACommand => {
                    // 非斜杠命令，继续正常流程
                }
                _ => {
                    // 其他情况继续正常流程
                }
            }
        }

        // 原有逻辑：转发给 runtime dispatcher
        if filtered_inbound_tx.send(msg).await.is_err() {
            break;
        }
    }
});
```

#### 5.4.3 关键要点

1. **WebSocket 拦截位置**: 在 `websocket.rs` 的 "chat" 分支内，创建 `InboundMessage` 之前
2. **Channel 拦截位置**: 在 `gateway.rs` 现有 interceptor 的 loop 中，confirm reply 检查之后
3. **不要创建新的 interceptor 任务**: 复用现有的 interceptor_handle，避免资源竞争
4. **事件类型**: 复用 `message_done`，无需前端改动

### 5.5 CLI 集成 (重构)

```rust
// bin/blockcell/src/commands/agent.rs (重构)

// 替换现有的硬编码命令处理逻辑

let stdin_handle = tokio::task::spawn_blocking(move || {
    loop {
        let input = read_line_with_command_picker(...);

        // 使用统一的命令处理器
        let ctx = CommandContext {
            paths: stdin_paths.clone(),
            task_manager: Some(stdin_task_manager.clone()),
            source: CommandSource {
                channel: "cli".to_string(),
                chat_id: session_clone.clone(),
                sender_id: Some("user".to_string()),
            },
            session_clear_callback: Some(Arc::new(|| {
                // 调用 runtime 的清除方法
                true
            })),
        };

        // 同步执行（stdin 线程）
        let result = tokio::runtime::Handle::current()
            .block_on(SLASH_COMMAND_HANDLER.try_handle(&input, &ctx));

        match result {
            CommandResult::Handled(response) => {
                println!("{}", response.content);
                continue;
            }
            CommandResult::ExitRequested => {
                println!("退出交互模式...");
                break;  // 退出 stdin 循环
            }
            CommandResult::NotACommand => {
                // 发送给 AgentRuntime
            }
            CommandResult::PermissionDenied(msg) => {
                eprintln!("权限不足: {}", msg);
                continue;
            }
            CommandResult::Error(e) => {
                eprintln!("命令执行错误: {}", e);
                continue;
            }
        }

        // 正常消息流程
        let inbound = InboundMessage { ... };
        stdin_tx.blocking_send(inbound);
    }
});
```

> **关键点**: `ExitRequested` 变体仅由 `/quit` 和 `/exit` 命令返回，CLI 层收到后 `break` 退出循环。其他渠道收到此结果时会显示 "命令仅 CLI 可用" 提示（因为在 `try_handle` 中已检查 `available_channels`）。

---

## 六、命令列表

### 6.1 内置命令

| 命令 | 参数 | 说明 | Token 消耗 | 权限 |
|------|------|------|-----------|------|
| `/help` | 无 | 显示所有可用命令 | 无 | 无 |
| `/tasks` | 无 | 列出后台任务状态 | 无 | 无 |
| `/skills` | 无 | 列出技能和演化状态 | 无 | 无 |
| `/tools` | 无 | 列出所有注册工具 | 无 | 无 |
| `/learn` | `<描述>` | 学习新技能 | **有** | 无 |
| `/clear` | 无 | 清除当前会话历史 | 无 | 无 |
| `/clear-skills` | 无 | 清除所有技能演化记录 | 无 | 管理员 |
| `/forget-skill` | `<名称>` | 删除指定技能记录 | 无 | 管理员 |
| `/quit`, `/exit` | 无 | 退出交互模式 | 无 | 无 |

### 6.2 命令渠道可用性

| 命令 | CLI | Gateway | Channel |
|------|-----|---------|---------|
| `/help` | ✅ | ✅ | ✅ |
| `/tasks` | ✅ | ✅ | ✅ |
| `/skills` | ✅ | ✅ | ✅ |
| `/tools` | ✅ | ✅ | ✅ |
| `/learn` | ✅ | ✅ | ✅ |
| `/clear` | ✅ | ✅ | ✅ |
| `/clear-skills` | ✅ | ✅ | ✅ |
| `/forget-skill` | ✅ | ✅ | ✅ |
| `/quit` | ✅ | ⚠️ 提示不可用 | ⚠️ 提示不可用 |
| `/exit` | ✅ | ⚠️ 提示不可用 | ⚠️ 提示不可用 |

### 6.3 关于 `/cancel` 命令

**设计决策**: 不实现 `/cancel` 斜杠命令。

**原因**:
1. WebSocket 已有独立的 `"cancel"` 消息类型 (websocket.rs:184-234)
2. 取消操作需要立即传递给 AgentRuntime，不适合作为斜杠命令处理
3. CLI 模式下可使用 `Ctrl+C` 中断

如果未来需要支持 `/cancel`，可作为快捷方式发送 `[cancel]` 消息，但这属于增强功能，不在 P0-P3 范围内。

---

## 七、WebUI 集成

### 7.1 复用现有 `message_done` 事件

**无需新增事件类型**。斜杠命令响应直接复用现有的 `message_done` 事件：

```rust
// 后端发送命令响应
let event = serde_json::json!({
    "type": "message_done",
    "chat_id": chat_id,
    "content": response.content,
    "task_id": "",
});
ws_broadcast.send(event.to_string());
```

前端 `store.ts` 已有处理逻辑：

```typescript
case 'message_done': {
    const finalContent = event.content ?? '';
    // 自动添加到消息列表，作为 assistant 消息显示
}
```

### 7.2 Markdown 渲染

现有 `MessageBubble` 组件已支持 Markdown 渲染，命令响应中的 Markdown 内容会自动正确显示。

### 7.3 实现清单

- [ ] 后端：命令响应使用 `message_done` 事件（无需前端改动）

---

## 八、渠道特定命令处理

### 8.1 QuitCommand 实现

```rust
// bin/blockcell/src/commands/slash_commands/handlers/quit.rs

use super::*;

/// /quit, /exit 命令 - 仅在 CLI 模式可用
pub struct QuitCommand;

#[async_trait::async_trait]
impl SlashCommand for QuitCommand {
    fn name(&self) -> &str {
        "quit"
    }

    fn description(&self) -> &str {
        "Exit interactive mode (CLI only)"
    }

    fn available_channels(&self) -> Option<Vec<&'static str>> {
        Some(vec!["cli"])  // 仅 CLI 可用
    }

    async fn execute(&self, _args: &str, _ctx: &CommandContext) -> CommandResult {
        // 返回 ExitRequested，由 CLI 层处理退出逻辑
        CommandResult::ExitRequested
    }
}

/// /exit 命令 (quit 的别名)
pub struct ExitCommand;

#[async_trait::async_trait]
impl SlashCommand for ExitCommand {
    fn name(&self) -> &str {
        "exit"
    }
    
    fn description(&self) -> &str {
        "Exit interactive mode (CLI only)"
    }
    
    fn available_channels(&self) -> Option<Vec<&'static str>> {
        Some(vec!["cli"])
    }
    
    async fn execute(&self, args: &str, ctx: &CommandContext) -> CommandResult {
        QuitCommand.execute(args, ctx).await
    }
}
```

### 8.2 渠道限制检查流程

```text
用户输入: /quit
    │
    ▼
┌─────────────────────────┐
│ SlashCommandHandler     │
│ try_handle()            │
└───────────┬─────────────┘
            │
            ▼
    找到 QuitCommand
            │
            ▼
    ┌───────────────────┐
    │ available_channels│
    │ == ["cli"]        │
    └───────┬───────────┘
            │
            ▼
    ┌───────────────────┐
    │ ctx.source.channel│
    │ == "ws" ?         │
    └───────┬───────────┘
            │
    ┌───────┴───────┐
    │               │
   是              否
    │               │
    ▼               ▼
返回提示:       执行命令
"命令 /quit
仅在 CLI 
模式可用"
```

---

## 九、安全考量

### 9.1 现有安全机制

**allowFrom 白名单验证**（已在各 Channel 实现中存在）：

```rust
// 在 telegram.rs, slack.rs 等 Channel 实现中
// 消息到达时首先检查 allowFrom
if !is_allowed(&msg.sender_id, &config.allow_from) {
    return Ok(()); // 静默忽略非白名单用户
}
```

**安全流程**：

1. **Channel 层**: allowFrom 白名单验证 → 非白名单用户消息被丢弃
2. **Gateway 层**: 斜杠命令拦截 → 处理命令或转发给 AgentRuntime
3. **AgentRuntime**: 正常消息处理

### 9.2 权限控制

某些敏感命令需要权限验证：

```rust
/// 命令权限级别
pub enum CommandPermission {
    /// 所有人可用
    Public,
    /// 需要登录用户
    Authenticated,
    /// 需要管理员权限
    Admin,
}

impl SlashCommand for ClearSkillsCommand {
    fn requires_permission(&self) -> bool {
        true
    }
    
    fn permission_level(&self) -> CommandPermission {
        CommandPermission::Admin
    }
}
```

### 9.3 速率限制

防止命令滥用：

```rust
use std::time::{Duration, Instant};
use std::collections::HashMap;
use std::sync::Mutex;

pub static COMMAND_RATE_LIMITER: once_cell::sync::Lazy<Mutex<HashMap<String, Instant>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

fn check_rate_limit(sender_id: &str) -> bool {
    let mut limiter = COMMAND_RATE_LIMITER.lock().unwrap();
    let now = Instant::now();

    if let Some(last) = limiter.get(sender_id) {
        if now.duration_since(*last) < Duration::from_secs(1) {
            return false;
        }
    }

    limiter.insert(sender_id.to_string(), now);
    true
}
```

---

## 十、测试计划

### 10.1 单元测试

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_help_command() {
        let handler = create_default_handler();
        let ctx = CommandContext::test_context();
        
        let result = handler.try_handle("/help", &ctx).await;
        assert!(matches!(result, CommandResult::Handled(_)));
        
        if let CommandResult::Handled(response) = result {
            assert!(response.content.contains("/tasks"));
            assert!(response.content.contains("/skills"));
        }
    }
    
    #[tokio::test]
    async fn test_non_command_passthrough() {
        let handler = create_default_handler();
        let ctx = CommandContext::test_context();
        
        let result = handler.try_handle("hello world", &ctx).await;
        assert!(matches!(result, CommandResult::NotACommand));
    }
    
    #[tokio::test]
    async fn test_unknown_command() {
        let handler = create_default_handler();
        let ctx = CommandContext::test_context();
        
        let result = handler.try_handle("/unknowncommand", &ctx).await;
        assert!(matches!(result, CommandResult::Handled(_)));
        
        if let CommandResult::Handled(response) = result {
            assert!(response.content.contains("未知命令"));
        }
    }
    
    #[tokio::test]
    async fn test_quit_command_channel_restriction() {
        let handler = create_default_handler();
        
        // CLI 渠道应该成功
        let ctx_cli = CommandContext {
            source: CommandSource {
                channel: "cli".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let result = handler.try_handle("/quit", &ctx_cli).await;
        assert!(matches!(result, CommandResult::ExitRequested));

        // Gateway 渠道应该返回提示
        let ctx_ws = CommandContext {
            source: CommandSource {
                channel: "ws".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let result = handler.try_handle("/quit", &ctx_ws).await;
        if let CommandResult::Handled(response) = result {
            assert!(response.content.contains("仅") || response.content.contains("CLI"));
        } else {
            panic!("Expected Handled response for channel restriction");
        }
    }

    #[tokio::test]
    async fn test_exit_requested_only_for_cli() {
        let handler = create_default_handler();

        // /quit 返回 ExitRequested
        let ctx = CommandContext::test_context();
        let result = handler.try_handle("/quit", &ctx).await;
        assert!(matches!(result, CommandResult::ExitRequested));

        // /exit 也返回 ExitRequested
        let result = handler.try_handle("/exit", &ctx).await;
        assert!(matches!(result, CommandResult::ExitRequested));
    }
}
```

### 10.2 集成测试

| 测试场景 | 输入 | 预期输出 |
|---------|------|---------|
| CLI 输入 /help | `/help` | 显示命令列表 |
| WebSocket 输入 /tasks | `{"type":"chat","content":"/tasks"}` | 返回任务列表 JSON |
| Telegram 输入 /skills | `/skills` | 返回技能列表 |
| 非命令消息 | `hello` | 正常发给 AgentRuntime |
| 未知命令 | `/unknown` | 提示未知命令 |
| WebSocket 输入 /quit | `/quit` | 返回"仅 CLI 模式可用"提示 |

---

## 附录: 实现清单

### Phase 1: 核心框架 (P0)

- [x] `bin/blockcell/src/commands/slash_commands/mod.rs` - 核心接口
- [x] `bin/blockcell/src/commands/slash_commands/context.rs` - CommandContext, CommandResult (含 ForwardToRuntime)
- [x] `bin/blockcell/src/commands/slash_commands/registry.rs` - 命令注册
- [x] `bin/blockcell/src/commands/slash_commands/handlers/help.rs` - /help 命令
- [x] `bin/blockcell/src/commands/slash_commands/handlers/tasks.rs` - /tasks 命令
- [x] Gateway WebSocket 集成
- [x] Gateway Channel 拦截层

### Phase 2: 迁移现有命令 (P1)

- [x] 迁移 /skills 命令
- [x] 迁移 /tools 命令
- [x] 迁移 /learn 命令（使用 ForwardToRuntime 机制）
  - [x] LearnCommand 返回 ForwardToRuntime
  - [x] CLI/Gateway/WebSocket 处理 ForwardToRuntime
  - [x] 消息转换逻辑统一在命令处理器中
- [x] 迁移 /clear 命令（完整实现）
  - [x] 清除会话历史文件 (`SessionStore::clear`)
  - [x] 清除 Session Memory 文件
  - [x] 清除 .active 标记文件
  - [x] 支持会话清除回调
  - [x] 错误信息包含 session_key 上下文
- [x] 迁移 /quit, /exit 命令（渠道限制）
- [x] 迁移 /clear-skills, /forget-skill 命令
- [x] 重构 agent.rs stdin 处理逻辑

### Phase 3: WebUI 集成 (P2)

> **注**: WebUI 无需额外改动。斜杠命令响应复用现有 `message_done` 事件，Markdown 渲染已支持。

- [x] 验证 `message_done` 事件正常工作

### Phase 4: 安全增强 (P3)

- [ ] 权限控制系统
- [ ] 速率限制
- [ ] 审计日志

---

## 附录: session_key 格式规范

`session_key` 用于标识会话，格式为 `{channel}:{chat_id}`：

| 渠道 | session_key 示例 | 说明 |
|------|-----------------|------|
| CLI | `cli:default` | channel="cli", chat_id 来自 agent_id |
| WebSocket | `ws:abc123` | channel="ws", chat_id 由 assign_session_id 生成 |
| Telegram | `telegram:123456789` | channel="telegram", chat_id 为 Telegram chat_id |
| Slack | `slack:C12345678` | channel="slack", chat_id 为 Slack channel ID |

**格式约定**:
- 使用 `:` 作为分隔符
- `session_key` 与 `SessionStore::session_file()` 的路径计算保持一致
- 文件路径: `{workspace}/sessions/{chat_id}/session.json`

**代码示例**:
```rust
// 构造 session_key
let session_key = format!("{}:{}", ctx.source.channel, ctx.source.chat_id);

// 清除会话文件
let session_store = SessionStore::new(ctx.paths.clone());
session_store.clear(&session_key)?;
```

---

> 文档版本: 2026-04-07 (更新: ForwardToRuntime 机制、session_key 格式规范)
> 目标框架: BlockCell Rust 多智能体框架