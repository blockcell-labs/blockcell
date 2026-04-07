//! # /clear 命令
//!
//! 清除当前会话历史。
//!
//! ## session_key 格式
//!
//! session_key 的格式为 `{channel}:{chat_id}`，例如：
//! - CLI: `cli:default`
//! - WebSocket: `ws:{session_id}`
//! - Telegram: `telegram:{chat_id}`
//!
//! 这个格式与 `SessionStore::session_file()` 的路径计算保持一致。

use crate::commands::slash_commands::*;
use blockcell_storage::SessionStore;

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
        let mut results: Vec<String> = Vec::new();

        // session_key 格式: {channel}:{chat_id}
        // 例如: cli:default, ws:abc123, telegram:123456789
        let session_key = format!("{}:{}", ctx.source.channel, ctx.source.chat_id);

        // 1. 调用清除回调（如果存在）
        if let Some(ref callback) = ctx.session_clear_callback {
            if callback() {
                results.push("✅ 会话内存状态已清除".to_string());
            } else {
                results.push("⚠️ 会话内存清除失败".to_string());
            }
        } else {
            results.push("ℹ️ 无内存清除回调（可能是 Gateway 模式）".to_string());
        }

        // 2. 清除会话历史文件 (SessionStore)
        let session_store = SessionStore::new(ctx.paths.clone());
        match session_store.clear(&session_key) {
            Ok(true) => results.push("✅ 会话历史文件已删除".to_string()),
            Ok(false) => results.push("ℹ️ 无会话历史文件（可能从未对话过）".to_string()),
            Err(e) => results.push(format!(
                "⚠️ 会话历史文件删除失败 (session: {}): {}",
                session_key, e
            )),
        }

        // 3. 清除 Session Memory 文件
        let session_memory_path = ctx
            .paths
            .workspace()
            .join("sessions")
            .join(&ctx.source.chat_id)
            .join("memory.md");

        if session_memory_path.exists() {
            match tokio::fs::remove_file(&session_memory_path).await {
                Ok(_) => results.push("✅ Session Memory 文件已删除".to_string()),
                Err(e) => results.push(format!(
                    "⚠️ Session Memory 删除失败 (path: {}): {}",
                    session_memory_path.display(),
                    e
                )),
            }
        }

        // 4. 清除 .active 标记文件
        let active_file = ctx
            .paths
            .workspace()
            .join("sessions")
            .join(&ctx.source.chat_id)
            .join(".active");

        if active_file.exists() {
            let _ = tokio::fs::remove_file(&active_file).await;
        }

        // 5. 构建响应
        let content = if results.is_empty() {
            "✅ 会话历史已清除 (无持久化数据)\n".to_string()
        } else {
            format!("📋 会话清除结果:\n{}\n", results.join("\n"))
        };

        CommandResult::Handled(CommandResponse::markdown(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_clear_command() {
        let cmd = ClearCommand;
        let ctx = CommandContext::test_context();

        let result = cmd.execute("", &ctx).await;
        assert!(matches!(result, CommandResult::Handled(_)));
    }
}