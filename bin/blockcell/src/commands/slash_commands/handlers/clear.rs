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

        // 与 InboundMessage 使用同一账号感知的会话身份。
        let session_key = ctx.source.session_key();

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
        // 使用 session_file_stem 确保路径兼容性（Windows 不允许冒号）
        let safe_session_key = blockcell_core::session_file_stem(&session_key);
        let session_dir = ctx
            .paths
            .workspace()
            .join("sessions")
            .join(&safe_session_key);
        let session_memory_path = session_dir.join("memory.md");

        if session_memory_path.exists() {
            match tokio::fs::remove_file(&session_memory_path).await {
                Ok(_) => results.push("✅ Session Memory 文件已删除".to_string()),
                Err(e) => results.push(format!(
                    "⚠️ Session Memory 删除失败 (session: {}, path: {}): {}",
                    session_key,
                    session_memory_path.display(),
                    e
                )),
            }
        }

        // 4. 清除 .active 标记文件
        let active_file = session_dir.join(".active");

        if active_file.exists() {
            match tokio::fs::remove_file(&active_file).await {
                Ok(_) => tracing::trace!(
                    session_key = %session_key,
                    "[/clear] .active marker deleted"
                ),
                Err(e) => tracing::warn!(
                    session_key = %session_key,
                    error = %e,
                    "[/clear] Failed to delete .active marker"
                ),
            }
        }

        // 5. 清除持久化的工具结果目录 (Layer 1 Tool Results)
        let tool_results_dir = session_dir.join("tool-results");

        if tool_results_dir.exists() {
            match tokio::fs::remove_dir_all(&tool_results_dir).await {
                Ok(_) => {
                    results.push("✅ 工具结果文件已删除".to_string());
                }
                Err(e) => results.push(format!(
                    "⚠️ 工具结果目录删除失败 (session: {}, path: {}): {}",
                    session_key,
                    tool_results_dir.display(),
                    e
                )),
            }
        }

        // 6. 重置 session metrics 的实时状态值
        // 这些是"当前状态"指标，清除会话后应该归零
        let metrics = blockcell_agent::session_metrics::get_memory_metrics();
        // Layer 1: 工具结果存储计数
        metrics.layer1.update_stored_count(0);
        // Layer 3: Session Memory 大小和章节数 (使用 update 方法避免增加计数)
        metrics.layer3.update_current_size(0);
        metrics.layer3.update_section_count(0);
        // Layer 4: 当前 token 使用量
        metrics.layer4.update_token_usage(0);
        // Layer 7: Forked Agent 统计 (Spawned, Active, Completed, Failed 等)
        metrics.layer7.reset();
        tracing::debug!(
            session_key = %session_key,
            "[/clear] Session metrics real-time state reset completed"
        );

        // 7. 构建响应 (使用 Markdown 列表格式)
        let content = if results.is_empty() {
            "✅ 会话历史已清除 (无持久化数据)\n".to_string()
        } else {
            // 使用 Markdown 列表语法，每条记录前加 `-` 前缀
            let formatted_results: Vec<String> =
                results.iter().map(|r| format!("- {}", r)).collect();
            format!("📋 会话清除结果:\n{}\n", formatted_results.join("\n"))
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
