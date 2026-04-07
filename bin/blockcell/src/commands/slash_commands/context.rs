//! # 命令执行上下文
//!
//! 定义斜杠命令执行所需的上下文信息。

use blockcell_core::Paths;
use blockcell_agent::TaskManager;
use std::sync::Arc;

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

impl CommandSource {
    /// 创建 CLI 来源
    pub fn cli(chat_id: String) -> Self {
        Self {
            channel: "cli".to_string(),
            chat_id,
            sender_id: Some("user".to_string()),
        }
    }

    /// 创建 WebSocket 来源
    pub fn websocket(chat_id: String) -> Self {
        Self {
            channel: "ws".to_string(),
            chat_id,
            sender_id: Some("user".to_string()),
        }
    }

    /// 创建 Channel 来源
    pub fn channel(channel: String, chat_id: String, sender_id: Option<String>) -> Self {
        Self {
            channel,
            chat_id,
            sender_id,
        }
    }
}

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
    ///
    /// 回调由 AgentRuntime 在启动时注册，用于清除内存中的消息历史。
    /// 在 Gateway 模式下此回调通常为 None。
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

    /// 创建 CLI 上下文
    pub fn for_cli(paths: Paths, task_manager: TaskManager, chat_id: String) -> Self {
        Self {
            paths,
            task_manager: Some(task_manager),
            source: CommandSource::cli(chat_id),
            session_clear_callback: None,
        }
    }

    /// 创建 WebSocket 上下文
    pub fn for_websocket(paths: Paths, task_manager: TaskManager, chat_id: String) -> Self {
        Self {
            paths,
            task_manager: Some(task_manager),
            source: CommandSource::websocket(chat_id),
            session_clear_callback: None,
        }
    }

    /// 创建 Channel 上下文
    pub fn for_channel(
        paths: Paths,
        task_manager: TaskManager,
        channel: String,
        chat_id: String,
        sender_id: Option<String>,
    ) -> Self {
        Self {
            paths,
            task_manager: Some(task_manager),
            source: CommandSource::channel(channel, chat_id, sender_id),
            session_clear_callback: None,
        }
    }

    /// 设置会话清除回调
    pub fn with_clear_callback(mut self, callback: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.session_clear_callback = Some(callback);
        self
    }

    /// 判断是否为 CLI 渠道
    pub fn is_cli(&self) -> bool {
        self.source.channel == "cli"
    }

    /// 判断是否为 WebSocket 渠道
    pub fn is_websocket(&self) -> bool {
        self.source.channel == "ws"
    }

    /// 判断是否为外部 Channel 渠道
    pub fn is_external_channel(&self) -> bool {
        !self.is_cli() && !self.is_websocket()
    }
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
    /// 包含转换后的消息内容，供 AgentRuntime 使用。
    /// 用于那些需要 LLM 参与的命令，如学习新技能。
    ForwardToRuntime {
        /// 转换后的消息内容
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

impl CommandResponse {
    /// 创建纯文本响应
    pub fn text(content: String) -> Self {
        Self {
            content,
            is_markdown: false,
        }
    }

    /// 创建 Markdown 响应
    pub fn markdown(content: String) -> Self {
        Self {
            content,
            is_markdown: true,
        }
    }
}