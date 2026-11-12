//! # /tasks 命令
//!
//! 任务管理的统一入口，整合了列出、查看、取消、恢复、重启、删除等子命令。
//!
//! - `/tasks` - 列出所有任务
//! - `/tasks <task_id>` - 查看指定任务的详细信息（前缀匹配）
//! - `/tasks cancel <task_id>` - 取消运行中的任务
//! - `/tasks resume <task_id>` - 从断点恢复未完成的任务
//! - `/tasks restart <task_id>` - 重启失败或已取消的任务
//! - `/tasks delete <task_id>` - 删除指定任务
//! - `/tasks clear` - 清空所有已结束的任务（completed/failed/cancelled）

use crate::commands::slash_commands::*;
use blockcell_agent::TaskStatus;

/// /tasks 命令 - 任务管理的统一入口
pub struct TasksCommand;

#[async_trait::async_trait]
impl SlashCommand for TasksCommand {
    fn name(&self) -> &str {
        "tasks"
    }

    fn description(&self) -> &str {
        "Task manager: /tasks | /tasks <id> | /tasks cancel|resume|restart|delete <id> | /tasks clear"
    }

    fn accepts_args(&self) -> bool {
        true
    }

    async fn execute(&self, args: &str, ctx: &CommandContext) -> CommandResult {
        let task_manager = match &ctx.task_manager {
            Some(tm) => tm,
            None => {
                return CommandResult::Handled(CommandResponse::markdown(
                    "⚠️ **Task manager not available**\n".to_string(),
                ));
            }
        };

        let trimmed = args.trim();

        // 子命令: /tasks clear — 清空所有已结束的任务
        if trimmed == "clear" {
            return Self::clear_finished_tasks(task_manager, &ctx.source).await;
        }

        // 子命令: /tasks cancel <task_id> — 取消运行中的任务
        if let Some(task_id) = trimmed.strip_prefix("cancel ") {
            let task_id = task_id.trim();
            if task_id.is_empty() {
                return CommandResult::Handled(CommandResponse::markdown(
                    "⚠️ **用法**: `/tasks cancel <task_id>`\n\n使用 `/tasks` 查看任务列表获取 task_id"
                        .to_string(),
                ));
            }
            return Self::cancel_task(task_id, task_manager, &ctx.source).await;
        }

        // 子命令: /tasks resume [task_id] — 从断点恢复未完成的任务
        if trimmed == "resume" || trimmed.starts_with("resume ") {
            // 使用 if let 替代 unwrap_or，更安全且语义清晰
            let task_id = if let Some(s) = trimmed.strip_prefix("resume ") {
                s.trim()
            } else {
                "" // 仅 "/tasks resume" 无参数时
            };
            return Self::resume_task(task_id, ctx).await;
        }

        // 子命令: /tasks restart <task_id> — 重启失败或已取消的任务
        if let Some(task_id) = trimmed.strip_prefix("restart ") {
            let task_id = task_id.trim();
            if task_id.is_empty() {
                return CommandResult::Handled(CommandResponse::markdown(
                    "⚠️ **用法**: `/tasks restart <task_id>`\n\n使用 `/tasks` 查看任务列表获取 task_id"
                        .to_string(),
                ));
            }
            return Self::restart_task(task_id, task_manager, &ctx.source).await;
        }

        // 子命令: /tasks delete <task_id> — 删除指定任务
        if let Some(task_id) = trimmed.strip_prefix("delete ") {
            let task_id = task_id.trim();
            if task_id.is_empty() {
                return CommandResult::Handled(CommandResponse::markdown(
                    "❌ 请指定要删除的任务 ID\n\n用法: `/tasks delete <task_id>`".to_string(),
                ));
            }
            return Self::delete_task(task_id, task_manager, &ctx.source).await;
        }

        // 如果提供了参数，则查看指定任务详情
        if !trimmed.is_empty() {
            return Self::show_task_detail(trimmed, task_manager, &ctx.source).await;
        }

        // 否则列出所有任务
        Self::list_all_tasks(task_manager, &ctx.source).await
    }
}

impl TasksCommand {
    /// 列出所有任务
    async fn list_all_tasks(
        task_manager: &blockcell_agent::TaskManager,
        source: &CommandSource,
    ) -> CommandResult {
        let (queued, running, completed, failed) =
            task_manager.summary_for_origin(&source.task_origin()).await;
        let task_list = task_manager
            .list_tasks_for_origin(&source.task_origin(), None)
            .await;

        let mut content = String::new();
        content.push_str("📋 **Task overview:**\n\n");
        content.push_str(&format!(
            "- {} queued | {} running | {} completed | {} failed\n",
            queued, running, completed, failed
        ));

        if task_list.is_empty() {
            content.push_str("\n✅ **没有任何任务** — 你可以安全地启动新任务。\n");
        } else {
            // 明确说明运行状态
            if running == 0 {
                content.push_str("\n✅ **当前没有运行中的任务** — 你可以安全地启动新任务。\n");
            }
            content.push_str("\n**Tasks:**\n");
            for t in &task_list {
                let status_icon = match t.status {
                    TaskStatus::Queued => "⏳",
                    TaskStatus::Running => "🔄",
                    TaskStatus::Completed => "✅",
                    TaskStatus::Failed => "❌",
                    TaskStatus::Cancelled => "🚫",
                };
                let short_id = short_task_id(&t.id, 8);
                content.push_str(&format!(
                    "- {} `[{}]` {} - {}\n",
                    status_icon, short_id, t.status, t.label
                ));
            }
            content.push_str("\n💡 `/tasks <id>` 详情 | `/tasks cancel <id>` 取消 | `/tasks resume <id>` 恢复 | `/tasks restart <id>` 重启 | `/tasks delete <id>` 删除 | `/tasks clear` 清空\n");
        }

        CommandResult::Handled(CommandResponse::markdown(content))
    }

    /// 取消运行中的任务
    async fn cancel_task(
        task_id_prefix: &str,
        task_manager: &blockcell_agent::TaskManager,
        source: &CommandSource,
    ) -> CommandResult {
        let matches = task_manager
            .find_task_by_prefix_for_origin(task_id_prefix, &source.task_origin())
            .await;

        match matches.len() {
            0 => CommandResult::Handled(CommandResponse::markdown(format!(
                "❌ 未找到匹配的任务: `{}`\n\n使用 `/tasks` 查看所有任务",
                task_id_prefix
            ))),
            1 => {
                let task = &matches[0];

                // 先在 TaskManager 中标记为 Cancelled
                if let Err(e) = task_manager.cancel_task(&task.id).await {
                    return CommandResult::Handled(CommandResponse::markdown(format!(
                        "❌ 取消失败: {}",
                        e
                    )));
                }

                // 通过 ForwardToRuntime 将取消指令传递给 AgentRuntime
                // Runtime 收到 [cancel:task_id=xxx] 后：
                // 1. 查找 active_abort_tokens[task_id] 并调用 token.cancel()
                // 2. 查找 active_message_tasks[task_id] 并调用 handle.abort()
                // 3. 从 active_chat_tasks / active_abort_tokens 中移除
                CommandResult::ForwardToRuntime {
                    transformed_content: format!("[cancel:task_id={}]", task.id),
                    original_command: format!("/tasks cancel {}", task_id_prefix),
                }
            }
            _ => {
                let mut content = format!(
                    "找到 {} 个匹配的任务，请提供更完整的 ID:\n\n",
                    matches.len()
                );
                for t in &matches {
                    let short_id = short_task_id(&t.id, 8);
                    content.push_str(&format!("- `[{}]` {} ({})\n", short_id, t.label, t.status));
                }
                CommandResult::Handled(CommandResponse::markdown(content))
            }
        }
    }

    /// 从断点恢复未完成的任务
    async fn resume_task(task_id_prefix: &str, ctx: &CommandContext) -> CommandResult {
        let checkpoint_manager = match &ctx.checkpoint_manager {
            Some(cm) => cm,
            None => {
                return CommandResult::Handled(CommandResponse::markdown(
                    "⚠️ **Checkpoint manager not available**\n".to_string(),
                ));
            }
        };

        if task_id_prefix.is_empty() {
            // 列出所有可恢复的 checkpoint
            let unfinished =
                checkpoint_manager.find_unfinished_for_origin(&ctx.source.task_origin());
            if unfinished.is_empty() {
                return CommandResult::Handled(CommandResponse::markdown(
                    "✅ 没有可恢复的断点任务\n".to_string(),
                ));
            }

            let mut content = String::new();
            content.push_str(&format!(
                "🔄 **可恢复的任务** ({} 个):\n\n",
                unfinished.len()
            ));
            for cp in &unfinished {
                let short_id = short_task_id(&cp.task_id, 8);
                content.push_str(&format!(
                    "- `[{}]` 轮次: {}, 消息数: {}, 时间: {}\n",
                    short_id,
                    cp.turn,
                    cp.messages.len(),
                    cp.created_at.format("%Y-%m-%d %H:%M:%S")
                ));
            }
            content.push_str("\n💡 使用 `/tasks resume <task_id>` 恢复指定任务\n");

            return CommandResult::Handled(CommandResponse::markdown(content));
        }

        // 查找匹配的 checkpoint
        let unfinished = checkpoint_manager.find_unfinished_for_origin(&ctx.source.task_origin());
        let matches: Vec<_> = unfinished
            .iter()
            .filter(|cp| cp.task_id.starts_with(task_id_prefix))
            .collect();

        match matches.len() {
            0 => CommandResult::Handled(CommandResponse::markdown(format!(
                "❌ 未找到匹配的可恢复任务: `{}`\n\n使用 `/tasks resume` 查看所有可恢复任务",
                task_id_prefix
            ))),
            1 => {
                let cp = matches[0];
                // 通过 ForwardToRuntime 将恢复指令传递给 AgentRuntime
                // Runtime 收到 [resume_task:task_id=xxx] 后：
                // 1. 从 CheckpointManager 加载完整对话历史
                // 2. 将历史消息注入当前会话
                // 3. 自动从断点轮次继续执行
                CommandResult::ForwardToRuntime {
                    transformed_content: format!("[resume_task:task_id={}]", cp.task_id),
                    original_command: format!("/tasks resume {}", cp.task_id),
                }
            }
            _ => {
                let mut content = format!(
                    "找到 {} 个匹配的可恢复任务，请提供更完整的 ID:\n\n",
                    matches.len()
                );
                for cp in &matches {
                    let short_id = short_task_id(&cp.task_id, 8);
                    content.push_str(&format!(
                        "- `[{}]` 轮次: {}, 消息数: {}\n",
                        short_id,
                        cp.turn,
                        cp.messages.len()
                    ));
                }
                CommandResult::Handled(CommandResponse::markdown(content))
            }
        }
    }

    /// 重启失败或已取消的任务
    async fn restart_task(
        task_id_prefix: &str,
        task_manager: &blockcell_agent::TaskManager,
        source: &CommandSource,
    ) -> CommandResult {
        let matches = task_manager
            .find_task_by_prefix_for_origin(task_id_prefix, &source.task_origin())
            .await;

        match matches.len() {
            0 => CommandResult::Handled(CommandResponse::markdown(format!(
                "❌ 未找到匹配的任务: `{}`\n\n使用 `/tasks` 查看所有任务",
                task_id_prefix
            ))),
            1 => {
                let task = &matches[0];
                // 只允许重启失败或已取消的任务
                match task.status {
                    TaskStatus::Failed | TaskStatus::Cancelled => {
                        // 将原任务描述作为 ForwardToRuntime 重新提交给 runtime
                        // runtime 会将其作为新消息处理，创建新任务并执行
                        CommandResult::ForwardToRuntime {
                            transformed_content: task.task_description.clone(),
                            original_command: format!("/tasks restart {}", task_id_prefix),
                        }
                    }
                    TaskStatus::Running => CommandResult::Handled(CommandResponse::markdown(
                        "⚠️ 任务正在运行中，请先使用 `/tasks cancel <id>` 取消后再重启".to_string(),
                    )),
                    TaskStatus::Completed => CommandResult::Handled(CommandResponse::markdown(
                        "ℹ️ 任务已完成，无需重启。如需重新执行，请提交新的任务".to_string(),
                    )),
                    TaskStatus::Queued => CommandResult::Handled(CommandResponse::markdown(
                        "ℹ️ 任务正在排队中，无需重启".to_string(),
                    )),
                }
            }
            _ => {
                let mut content = format!(
                    "找到 {} 个匹配的任务，请提供更完整的 ID:\n\n",
                    matches.len()
                );
                for t in &matches {
                    let short_id = short_task_id(&t.id, 8);
                    content.push_str(&format!("- `[{}]` {} ({})\n", short_id, t.label, t.status));
                }
                CommandResult::Handled(CommandResponse::markdown(content))
            }
        }
    }

    /// 删除指定任务
    async fn delete_task(
        task_id_prefix: &str,
        task_manager: &blockcell_agent::TaskManager,
        source: &CommandSource,
    ) -> CommandResult {
        let matches = task_manager
            .find_task_by_prefix_for_origin(task_id_prefix, &source.task_origin())
            .await;

        match matches.len() {
            0 => CommandResult::Handled(CommandResponse::markdown(format!(
                "❌ 未找到匹配的任务: `{}`\n\n使用 `/tasks` 查看所有任务",
                task_id_prefix
            ))),
            1 => {
                let t = &matches[0];
                // 不允许删除正在运行的任务
                if t.status == TaskStatus::Running {
                    return CommandResult::Handled(CommandResponse::markdown(format!(
                        "⚠️ 任务 `[{}]` 正在运行，无法删除。\n\n请先使用 `/tasks cancel {}` 取消任务",
                        t.id, t.id
                    )));
                }
                let label = t.label.clone();
                let id = t.id.clone();
                task_manager.remove_task(&id).await;
                CommandResult::Handled(CommandResponse::markdown(format!(
                    "🗑️ 已删除任务 `[{}]` ({})",
                    id, label
                )))
            }
            _ => {
                let mut content = format!(
                    "找到 {} 个匹配的任务，请提供更完整的 ID:\n\n",
                    matches.len()
                );
                for t in &matches {
                    let short_id = short_task_id(&t.id, 8);
                    content.push_str(&format!("- `[{}]` {} ({})\n", short_id, t.label, t.status));
                }
                CommandResult::Handled(CommandResponse::markdown(content))
            }
        }
    }

    /// 清空所有已结束的任务（completed/failed/cancelled）
    async fn clear_finished_tasks(
        task_manager: &blockcell_agent::TaskManager,
        source: &CommandSource,
    ) -> CommandResult {
        let task_list = task_manager
            .list_tasks_for_origin(&source.task_origin(), None)
            .await;
        let to_remove: Vec<_> = task_list
            .iter()
            .filter(|t| {
                t.status == TaskStatus::Completed
                    || t.status == TaskStatus::Failed
                    || t.status == TaskStatus::Cancelled
            })
            .map(|t| t.id.clone())
            .collect();

        let count = to_remove.len();
        if count == 0 {
            // 即使没有已结束任务需要清空，也报告当前Running状态
            let running_count = task_list
                .iter()
                .filter(|t| t.status == TaskStatus::Running)
                .count();
            if running_count == 0 {
                return CommandResult::Handled(CommandResponse::markdown(
                    "✅ 当前没有任何任务（没有已结束的任务需要清空，也没有运行中的任务）\n\n你可以安全地启动新任务。".to_string(),
                ));
            }
            return CommandResult::Handled(CommandResponse::markdown(format!(
                "*(没有已结束的任务需要清空)*\n\n⚠️ 当前有 {} 个任务仍在运行中。",
                running_count
            )));
        }

        for id in &to_remove {
            task_manager.remove_task(id).await;
        }

        // 清空后再次检查Running状态
        let remaining = task_manager
            .list_tasks_for_origin(&source.task_origin(), None)
            .await;
        let running_count = remaining
            .iter()
            .filter(|t| t.status == TaskStatus::Running)
            .count();

        if running_count == 0 {
            CommandResult::Handled(CommandResponse::markdown(format!(
                "🗑️ 已清空 {} 个已结束的任务\n\n✅ 当前没有运行中的任务，你可以安全地启动新任务。",
                count
            )))
        } else {
            CommandResult::Handled(CommandResponse::markdown(format!(
                "🗑️ 已清空 {} 个已结束的任务\n\n⚠️ 当前仍有 {} 个任务在运行中。",
                count, running_count
            )))
        }
    }

    /// 查看指定任务的详细信息
    async fn show_task_detail(
        task_id_prefix: &str,
        task_manager: &blockcell_agent::TaskManager,
        source: &CommandSource,
    ) -> CommandResult {
        let matches = task_manager
            .find_task_by_prefix_for_origin(task_id_prefix, &source.task_origin())
            .await;

        match matches.len() {
            0 => CommandResult::Handled(CommandResponse::markdown(format!(
                "❌ 未找到匹配的任务: `{}`\n\n使用 `/tasks` 查看所有任务",
                task_id_prefix
            ))),
            1 => {
                let t = &matches[0];
                let mut content = String::new();
                content.push_str("📋 **任务详情**\n\n");
                content.push_str(&format!("- **ID**: `{}`\n", t.id));
                content.push_str(&format!("- **标签**: {}\n", t.label));
                content.push_str(&format!("- **状态**: {}\n", t.status));
                content.push_str(&format!("- **描述**: {}\n", t.task_description));
                content.push_str(&format!(
                    "- **创建时间**: {}\n",
                    t.created_at.format("%Y-%m-%d %H:%M:%S")
                ));

                if let Some(started) = t.started_at {
                    content.push_str(&format!(
                        "- **开始时间**: {}\n",
                        started.format("%Y-%m-%d %H:%M:%S")
                    ));
                }
                if let Some(completed) = t.completed_at {
                    content.push_str(&format!(
                        "- **完成时间**: {}\n",
                        completed.format("%Y-%m-%d %H:%M:%S")
                    ));
                }
                if let Some(ref agent_type) = t.agent_type {
                    content.push_str(&format!("- **Agent 类型**: {}\n", agent_type));
                }
                if let Some(ref agent_id) = t.agent_id {
                    content.push_str(&format!("- **Agent ID**: {}\n", agent_id));
                }
                content.push_str(&format!("- **来源渠道**: {}\n", t.origin_channel));
                // 遮蔽 chat_id，仅显示前8字符
                let masked_chat_id: String = t.origin_chat_id.chars().take(8).collect();
                content.push_str(&format!("- **来源 chat_id**: {}…\n", masked_chat_id));
                content.push_str(&format!("- **ONE_SHOT**: {}\n", t.one_shot));

                if let Some(ref progress) = t.progress {
                    content.push_str(&format!("- **进度**: {}\n", progress));
                }
                if let Some(ref result) = t.result {
                    // 截断长结果，防止 WebSocket/终端输出过多
                    let display_result = if result.chars().count() > 500 {
                        let truncated: String = result.chars().take(500).collect();
                        format!(
                            "{}… (共 {} 字符，已截断)",
                            truncated,
                            result.chars().count()
                        )
                    } else {
                        result.clone()
                    };
                    content.push_str(&format!("- **结果**: {}\n", display_result));
                }
                if let Some(ref err) = t.error {
                    content.push_str(&format!("- **错误**: {}\n", err));
                }

                CommandResult::Handled(CommandResponse::markdown(content))
            }
            _ => {
                let mut content = format!(
                    "找到 {} 个匹配的任务，请提供更完整的 ID:\n\n",
                    matches.len()
                );
                for t in &matches {
                    let short_id = short_task_id(&t.id, 8);
                    content.push_str(&format!("- `[{}]` {} ({})\n", short_id, t.label, t.status));
                }
                CommandResult::Handled(CommandResponse::markdown(content))
            }
        }
    }
}

/// 从 task_id 中提取短标识符，剥离 "task-" 前缀
fn short_task_id(task_id: &str, max_chars: usize) -> String {
    let meaningful = if let Some(rest) = task_id.strip_prefix("task-") {
        rest
    } else {
        task_id
    };
    meaningful.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_tasks_command_no_manager() {
        let cmd = TasksCommand;
        let ctx = CommandContext::default();

        let result = cmd.execute("", &ctx).await;
        assert!(matches!(result, CommandResult::Handled(_)));

        if let CommandResult::Handled(response) = result {
            assert!(response.content.contains("not available"));
        }
    }

    #[tokio::test]
    async fn test_tasks_command_only_lists_current_origin() {
        let manager = blockcell_agent::TaskManager::new();
        manager
            .create_and_start_task(
                "task-local",
                "local task",
                "local",
                "ws",
                "chat-a",
                None,
                false,
                None,
                false,
            )
            .await;
        manager
            .create_and_start_task(
                "task-foreign",
                "foreign task",
                "foreign",
                "ws",
                "chat-b",
                None,
                false,
                None,
                false,
            )
            .await;
        let mut ctx = CommandContext::test_context();
        ctx.source = CommandSource::websocket("chat-a".to_string());
        ctx.task_manager = Some(manager);

        let result = TasksCommand.execute("", &ctx).await;
        let CommandResult::Handled(response) = result else {
            panic!("expected handled response");
        };
        assert!(response.content.contains("local task"));
        assert!(!response.content.contains("foreign task"));
    }
}
