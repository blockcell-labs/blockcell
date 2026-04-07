//! # /learn 命令
//!
//! 学习新技能。
//!
//! 此命令返回 `ForwardToRuntime`，将转换后的消息转发给 AgentRuntime 处理。

use crate::commands::slash_commands::*;

/// /learn 命令 - 学习新技能
///
/// 注意：此命令会调用 LLM，消耗 Token。
/// 返回 `ForwardToRuntime`，携带转换后的消息内容供 AgentRuntime 处理。
pub struct LearnCommand;

#[async_trait::async_trait]
impl SlashCommand for LearnCommand {
    fn name(&self) -> &str {
        "learn"
    }

    fn description(&self) -> &str {
        "Learn a new skill by description (uses LLM)"
    }

    fn timeout_secs(&self) -> u64 {
        120 // 学习技能需要更长超时
    }

    async fn execute(&self, args: &str, _ctx: &CommandContext) -> CommandResult {
        let description = args.trim();

        if description.is_empty() {
            return CommandResult::Handled(CommandResponse::text(
                "  Usage: /learn <skill description>\n".to_string(),
            ));
        }

        // 构造转换后的消息内容，供 AgentRuntime 理解技能学习请求
        let transformed_content = format!(
            "Please learn the following skill: {}\n\n\
            If this skill is already learned (has a record in list_skills query=learned), just tell me it's done.\n\
            Otherwise, start learning this skill and report progress.",
            description
        );

        CommandResult::ForwardToRuntime {
            transformed_content,
            original_command: format!("/learn {}", description),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_learn_command_empty() {
        let cmd = LearnCommand;
        let ctx = CommandContext::test_context();

        let result = cmd.execute("", &ctx).await;
        assert!(matches!(result, CommandResult::Handled(_)));

        if let CommandResult::Handled(response) = result {
            assert!(response.content.contains("Usage"));
        }
    }

    #[tokio::test]
    async fn test_learn_command_with_description() {
        let cmd = LearnCommand;
        let ctx = CommandContext::test_context();

        let result = cmd.execute("data analysis skill", &ctx).await;
        // /learn 返回 ForwardToRuntime，包含转换后的消息
        assert!(matches!(result, CommandResult::ForwardToRuntime { .. }));

        if let CommandResult::ForwardToRuntime {
            transformed_content,
            original_command,
        } = result
        {
            assert!(transformed_content.contains("data analysis skill"));
            assert!(transformed_content.contains("Please learn"));
            assert_eq!(original_command, "/learn data analysis skill");
        }
    }
}