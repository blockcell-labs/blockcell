//! # 命令执行上下文
//!
//! 定义斜杠命令执行所需的上下文信息。

use blockcell_agent::{CheckpointManager, TaskManager};
use blockcell_core::{InboundMessage, OutboundMessage, Paths};
use std::sync::Arc;

/// 命令来源
#[derive(Debug, Clone, Default)]
pub struct CommandSource {
    /// 渠道类型: "cli", "ws", "telegram", "slack", etc.
    pub channel: String,
    /// 会话 ID
    pub chat_id: String,
    pub account_id: Option<String>,
    pub agent_id: Option<String>,
    /// 用户 ID (可选)
    #[allow(dead_code)] // TODO: 用于未来的权限检查
    pub sender_id: Option<String>,
}

impl CommandSource {
    pub fn session_key(&self) -> String {
        let mut message = InboundMessage::cli("");
        message.channel = self.channel.clone();
        message.chat_id = self.chat_id.clone();
        message.account_id = self.account_id.clone();
        message.session_key()
    }

    pub fn task_origin(&self) -> blockcell_agent::TaskOrigin {
        blockcell_agent::TaskOrigin::new(self.session_key(), self.agent_id.as_deref())
    }

    /// 创建 CLI 来源
    pub fn cli(chat_id: String) -> Self {
        Self {
            channel: "cli".to_string(),
            chat_id,
            sender_id: Some("user".to_string()),
            account_id: None,
            agent_id: None,
        }
    }

    /// 创建 WebSocket 来源
    pub fn websocket(chat_id: String) -> Self {
        Self {
            channel: "ws".to_string(),
            chat_id,
            sender_id: Some("user".to_string()),
            account_id: None,
            agent_id: None,
        }
    }

    /// 创建 Channel 来源
    pub fn channel(channel: String, chat_id: String, sender_id: Option<String>) -> Self {
        Self {
            channel,
            chat_id,
            sender_id,
            account_id: None,
            agent_id: None,
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
    /// 断点恢复管理器
    pub checkpoint_manager: Option<CheckpointManager>,
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
    #[allow(dead_code)] // 用于单元测试
    pub fn test_context() -> Self {
        Self {
            source: CommandSource {
                channel: "cli".to_string(),
                chat_id: "test-chat".to_string(),
                sender_id: Some("test-user".to_string()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Build a gateway command context from its original incoming message.
    pub fn for_gateway(
        paths: Paths,
        task_manager: TaskManager,
        message: &InboundMessage,
        agent_id: &str,
    ) -> Self {
        let paths = paths.for_agent(agent_id);
        let checkpoint_manager = CheckpointManager::new(&paths.workspace());
        let mut context = Self::for_channel(
            paths,
            task_manager,
            checkpoint_manager,
            message.channel.clone(),
            message.chat_id.clone(),
            Some(message.sender_id.clone()),
        );
        context.source.account_id = message.account_id.clone();
        context.source.agent_id = Some(agent_id.to_string());
        context
    }

    /// 创建 CLI 上下文
    pub fn for_cli(
        paths: Paths,
        task_manager: TaskManager,
        checkpoint_manager: CheckpointManager,
        chat_id: String,
        agent_id: String,
    ) -> Self {
        let mut source = CommandSource::cli(chat_id);
        source.agent_id = Some(agent_id);
        Self {
            paths,
            task_manager: Some(task_manager),
            checkpoint_manager: Some(checkpoint_manager),
            source,
            session_clear_callback: None,
        }
    }

    /// 创建 WebSocket 上下文
    pub fn for_websocket(
        paths: Paths,
        task_manager: TaskManager,
        chat_id: String,
        agent_id: &str,
    ) -> Self {
        let mut message = InboundMessage::cli("");
        message.channel = "ws".to_string();
        message.chat_id = chat_id;
        Self::for_gateway(paths, task_manager, &message, agent_id)
    }

    /// 创建 Channel 上下文
    pub fn for_channel(
        paths: Paths,
        task_manager: TaskManager,
        checkpoint_manager: CheckpointManager,
        channel: String,
        chat_id: String,
        sender_id: Option<String>,
    ) -> Self {
        Self {
            paths,
            task_manager: Some(task_manager),
            checkpoint_manager: Some(checkpoint_manager),
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
    #[allow(dead_code)] // TODO: 用于未来的渠道特定逻辑
    pub fn is_cli(&self) -> bool {
        self.source.channel == "cli"
    }

    /// 判断是否为 WebSocket 渠道
    #[allow(dead_code)] // TODO: 用于未来的渠道特定逻辑
    pub fn is_websocket(&self) -> bool {
        self.source.channel == "ws"
    }

    /// 判断是否为外部 Channel 渠道
    #[allow(dead_code)] // TODO: 用于未来的渠道特定逻辑
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
    #[allow(dead_code)] // TODO: 实现权限验证后使用
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
    #[allow(dead_code)] // TODO: 用于前端 Markdown 渲染
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

/// Construct every immediate channel command response through the same route.
pub fn reply_to_source(message: &InboundMessage, content: &str) -> OutboundMessage {
    let mut reply = OutboundMessage::new(&message.channel, &message.chat_id, content);
    reply.account_id = message.account_id.clone();
    reply
}

#[cfg(test)]
mod origin_regression_tests {
    use super::*;
    use crate::commands::slash_commands::{handlers::ClearCommand, SlashCommand};
    use blockcell_storage::SessionStore;

    #[tokio::test]
    async fn gateway_clear_preserves_other_agents_and_accounts() {
        struct TemporaryDirectory(std::path::PathBuf);
        impl Drop for TemporaryDirectory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let temp = TemporaryDirectory(
            std::env::temp_dir().join(format!("blockcell-slash-origin-{}", uuid::Uuid::new_v4())),
        );
        let paths = Paths::with_base(temp.0.clone());
        for (channel, account) in [("ws", None), ("telegram", Some("bot-b"))] {
            let mut message = InboundMessage::cli("/clear");
            message.channel = channel.into();
            message.chat_id = "review-clear".into();
            message.account_id = account.map(str::to_owned);
            let key = message.session_key();
            let default_store = SessionStore::new(paths.clone());
            let selected_store = SessionStore::new(paths.for_agent("ops"));
            default_store
                .append(
                    &key,
                    &blockcell_core::types::ChatMessage::user("default sentinel"),
                )
                .unwrap();
            selected_store
                .append(
                    &key,
                    &blockcell_core::types::ChatMessage::user("selected history"),
                )
                .unwrap();
            let mut other = message.clone();
            other.account_id = Some("bot-a".into());
            let other_key = other.session_key();
            selected_store
                .append(
                    &other_key,
                    &blockcell_core::types::ChatMessage::user("other account"),
                )
                .unwrap();
            for scoped in [paths.clone(), paths.for_agent("ops")] {
                let session_dir = scoped
                    .workspace()
                    .join("sessions")
                    .join(blockcell_core::session_file_stem(&key));
                std::fs::create_dir_all(session_dir.join("tool-results")).unwrap();
                std::fs::write(session_dir.join("memory.md"), "memory sentinel").unwrap();
                std::fs::write(session_dir.join("tool-results/result.txt"), "tool sentinel")
                    .unwrap();
                let events = scoped.session_events_file(&key);
                std::fs::create_dir_all(events.parent().unwrap()).unwrap();
                std::fs::write(events, "event sentinel").unwrap();
            }
            let ctx =
                CommandContext::for_gateway(paths.clone(), TaskManager::new(), &message, "ops");
            ClearCommand.execute("", &ctx).await;
            assert_eq!(
                default_store.load(&key).unwrap().len(),
                1,
                "other agent history must survive"
            );
            assert!(
                selected_store.load(&key).unwrap().is_empty(),
                "selected history must be cleared"
            );
            assert_eq!(
                selected_store.load(&other_key).unwrap().len(),
                1,
                "other account must survive"
            );
            for (scoped, preserved) in [(paths.clone(), true), (paths.for_agent("ops"), false)] {
                let session_dir = scoped
                    .workspace()
                    .join("sessions")
                    .join(blockcell_core::session_file_stem(&key));
                assert_eq!(session_dir.join("memory.md").exists(), preserved);
                assert_eq!(
                    session_dir.join("tool-results/result.txt").exists(),
                    preserved
                );
                assert_eq!(scoped.session_events_file(&key).exists(), preserved);
            }
        }
    }

    #[test]
    fn cli_command_origin_keeps_agent_and_complete_chat_id() {
        let base =
            std::env::temp_dir().join(format!("blockcell-cli-origin-{}", uuid::Uuid::new_v4()));
        let paths = Paths::with_base(base.clone()).for_agent("ops");
        let checkpoint = CheckpointManager::new(&paths.workspace());
        let context = CommandContext::for_cli(
            paths,
            TaskManager::new(),
            checkpoint,
            "a:b".into(),
            "ops".into(),
        );
        assert_eq!(
            context.source.task_origin(),
            blockcell_agent::TaskOrigin::new("cli:a:b", Some("ops"))
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn immediate_command_replies_keep_the_incoming_account() {
        let mut message = InboundMessage::cli("/help");
        message.channel = "telegram".into();
        message.account_id = Some("bot-b".into());
        for content in [
            "help",
            "permission denied",
            "command error",
            "unsupported command",
        ] {
            let reply = reply_to_source(&message, content);
            assert_eq!(reply.account_id, message.account_id);
            assert_eq!(reply.channel, message.channel);
            assert_eq!(reply.chat_id, message.chat_id);
        }
    }
}
