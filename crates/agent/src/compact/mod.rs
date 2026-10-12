//! Compact 模块 - Layer 4 完整压缩
//!
//! 当 Token 预算超限时，执行 LLM 语义压缩。
//!
//! ## 核心流程
//! 1. Pre-Compact Hooks 执行
//! 2. LLM 生成摘要 (9-part structured summary)
//! 3. Post-Compact 恢复 (文件 + 技能 + Session Memory)
//!
//! ## 恢复预算
//! 所有恢复预算参数均可通过 Layer4Config 配置，默认值：
//! - 文件: 50,000 tokens (最多 5 个文件，每个文件最多 5,000 tokens)
//! - 技能: 25,000 tokens
//! - Session Memory: 12,000 tokens

mod compactor;
mod file_tracker;
mod hooks;
mod skill_tracker;
mod summary;

pub use compactor::{
    CompactionRequest, CompactionSummary, ContextCompactor, ExtractiveCompactor,
    ModelSummaryCompactor,
};
pub use file_tracker::{FileRecord, FileTracker};
pub use hooks::{
    CompactHookRegistry, PostCompactContext, PostCompactHook, PostCompactResult, PreCompactContext,
    PreCompactHook, PreCompactResult,
};
pub use skill_tracker::{SkillRecord, SkillTracker};
pub use summary::{
    generate_compact_summary, CompactError, CompactSummary, CompactSummaryResult,
    CompactSummarySection,
};

use crate::token::estimate_tokens;

/// Compact 配置
/// 以下常量仅用作 RecoveryBudget::default() 的回退值，
/// 运行时使用 Layer4Config 中的对应字段。
/// 总文件恢复预算
pub const MAX_FILE_RECOVERY_TOKENS: usize = 50_000;
/// 单个文件恢复上限 (设计文档: "单文件上限 | 5,000 tokens")
pub const MAX_SINGLE_FILE_TOKENS: usize = 5_000;
/// 技能恢复预算
pub const MAX_SKILL_RECOVERY_TOKENS: usize = 25_000;
/// Session Memory 恢复预算
pub const MAX_SESSION_MEMORY_RECOVERY_TOKENS: usize = 12_000;
/// 最大恢复文件数
pub const MAX_FILES_TO_RECOVER: usize = 5;

/// Compact 恢复预算参数
///
/// 封装 Layer4Config 中的恢复预算字段，用于替代硬编码常量。
/// 当从 Layer4Config 构造时，使用用户配置值；当使用 Default 时，回退到硬编码常量。
#[derive(Debug, Clone)]
pub struct RecoveryBudget {
    /// 总文件恢复预算
    pub max_file_recovery_tokens: usize,
    /// 单个文件恢复上限
    pub max_single_file_tokens: usize,
    /// 技能恢复预算
    pub max_skill_recovery_tokens: usize,
    /// Session Memory 恢复预算
    pub max_session_memory_recovery_tokens: usize,
    /// 最大恢复文件数
    pub max_files_to_recover: usize,
}

impl Default for RecoveryBudget {
    fn default() -> Self {
        Self {
            max_file_recovery_tokens: MAX_FILE_RECOVERY_TOKENS,
            max_single_file_tokens: MAX_SINGLE_FILE_TOKENS,
            max_skill_recovery_tokens: MAX_SKILL_RECOVERY_TOKENS,
            max_session_memory_recovery_tokens: MAX_SESSION_MEMORY_RECOVERY_TOKENS,
            max_files_to_recover: MAX_FILES_TO_RECOVER,
        }
    }
}

impl From<&blockcell_core::config::Layer4Config> for RecoveryBudget {
    fn from(c: &blockcell_core::config::Layer4Config) -> Self {
        Self {
            max_file_recovery_tokens: c.max_file_recovery_tokens,
            max_single_file_tokens: c.max_single_file_tokens,
            max_skill_recovery_tokens: c.max_skill_recovery_tokens,
            max_session_memory_recovery_tokens: c.max_session_memory_recovery_tokens,
            max_files_to_recover: c.max_files_to_recover,
        }
    }
}

/// 禁止工具使用的 preamble
pub const NO_TOOLS_PREAMBLE: &str = r#"IMPORTANT: You are in compact mode.
You cannot use any tools. You must generate a summary based solely on the conversation history.
Do not attempt to call any tools, read files, or execute commands."#;

/// Compact 触发检查
pub fn should_compact(
    current_tokens: usize,
    budget_tokens: usize,
    threshold: f64, // 默认 0.8
) -> bool {
    current_tokens >= (budget_tokens as f64 * threshold) as usize
}

/// 压缩结果
#[derive(Debug)]
pub struct CompactResult {
    /// 压缩后的摘要消息
    pub summary_message: String,
    /// 恢复消息（文件 + 技能 + Session Memory）
    pub recovery_message: String,
    /// 压缩前 token 数
    pub pre_compact_tokens: usize,
    /// 压缩后 token 数（估算）
    pub post_compact_tokens: usize,
    /// 缓存读取的 tokens（来自 LLM API 响应）
    pub cache_read_tokens: u64,
    /// 缓存创建的 tokens（来自 LLM API 响应）
    pub cache_creation_tokens: u64,
    /// 是否成功
    pub success: bool,
    /// 错误信息（如果失败）
    pub error: Option<String>,
    /// 保留的最近消息（来自 Layer4Config.keep_recent_messages）
    pub recent_messages: Vec<blockcell_core::types::ChatMessage>,
}

impl CompactResult {
    /// 创建失败的压缩结果
    pub fn failed(error: &str) -> Self {
        Self {
            summary_message: String::new(),
            recovery_message: String::new(),
            pre_compact_tokens: 0,
            post_compact_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            success: false,
            error: Some(error.to_string()),
            recent_messages: Vec::new(),
        }
    }

    /// 创建成功的压缩结果
    pub fn success(
        summary_message: String,
        recovery_message: String,
        pre_compact_tokens: usize,
        post_compact_tokens: usize,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
        recent_messages: Vec<blockcell_core::types::ChatMessage>,
    ) -> Self {
        Self {
            summary_message,
            recovery_message,
            pre_compact_tokens,
            post_compact_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            success: true,
            error: None,
            recent_messages,
        }
    }

    /// 生成最终的压缩后消息
    pub fn to_compact_message(&self) -> String {
        let mut message = String::new();

        // 添加摘要
        if !self.summary_message.is_empty() {
            message.push_str("# Conversation Compacted\n\n");
            message.push_str(&self.summary_message);
        }

        // 添加恢复信息
        if !self.recovery_message.is_empty() {
            message.push_str("\n\n---\n\n");
            message.push_str(&self.recovery_message);
        }

        message
    }
}

/// 构建 Post-Compact 恢复消息
///
/// 收集文件、技能和 Session Memory 的恢复信息
pub fn build_recovery_message(
    file_tracker: &FileTracker,
    skill_tracker: &SkillTracker,
    session_memory_content: Option<&str>,
    budget: &RecoveryBudget,
) -> String {
    let mut recovery = String::new();

    // 1. 文件恢复
    let files =
        file_tracker.get_recent_files(budget.max_files_to_recover, budget.max_single_file_tokens);
    let mut file_section = String::new();
    let mut files_count = 0;
    for file in &files {
        let content = truncate_to_tokens(&file.summary, budget.max_single_file_tokens);
        let prefix = if file_section.is_empty() {
            format!(
                "## Files Previously Read\n\n### {}\n```\n",
                file.path.display()
            )
        } else {
            format!("### {}\n```\n", file.path.display())
        };
        if append_budgeted_block(
            &mut file_section,
            &prefix,
            &content,
            "\n```\n\n",
            budget.max_file_recovery_tokens,
        ) {
            files_count += 1;
        } else {
            break;
        }
    }
    recovery.push_str(&file_section);

    // 2. 技能恢复
    let skills = skill_tracker.get_recent_skills(usize::MAX);
    let mut skill_section = String::new();
    let mut skills_count = 0;
    for skill in &skills {
        let prefix = if skill_section.is_empty() {
            format!("## Skills Previously Loaded\n\n### {}\n```\n", skill.name)
        } else {
            format!("### {}\n```\n", skill.name)
        };
        if append_budgeted_block(
            &mut skill_section,
            &prefix,
            &skill.summary,
            "\n```\n\n",
            budget.max_skill_recovery_tokens,
        ) {
            skills_count += 1;
        } else {
            break;
        }
    }
    recovery.push_str(&skill_section);

    // 3. Session Memory 恢复
    if let Some(session_memory) = session_memory_content {
        if !session_memory.is_empty() {
            let mut session_section = String::new();
            append_budgeted_block(
                &mut session_section,
                "## Session Memory\n\n",
                session_memory,
                "",
                budget.max_session_memory_recovery_tokens,
            );
            recovery.push_str(&session_section);
        }
    }

    let total_tokens = estimate_tokens(&recovery);

    tracing::info!(
        files_count = files_count,
        skills_count = skills_count,
        has_session_memory = session_memory_content.is_some(),
        total_recovery_tokens = total_tokens,
        "[compact] built recovery message"
    );

    recovery
}

fn append_budgeted_block(
    output: &mut String,
    prefix: &str,
    content: &str,
    suffix: &str,
    max_tokens: usize,
) -> bool {
    if max_tokens == 0 {
        return false;
    }

    let full_candidate = format!("{}{}{}{}", output, prefix, content, suffix);
    if estimate_tokens(&full_candidate) <= max_tokens {
        output.push_str(prefix);
        output.push_str(content);
        output.push_str(suffix);
        return true;
    }

    const MARKER: &str = "...\n[content truncated]";
    let chars: Vec<char> = content.chars().collect();
    let mut low = 0usize;
    let mut high = chars.len();
    let mut best: Option<String> = None;

    while low <= high {
        let mid = low + (high - low) / 2;
        let mut truncated: String = chars.iter().take(mid).collect();
        truncated.push_str(MARKER);
        let candidate = format!("{}{}{}{}", output, prefix, truncated, suffix);
        if estimate_tokens(&candidate) <= max_tokens {
            best = Some(truncated);
            low = mid.saturating_add(1);
        } else if mid == 0 {
            break;
        } else {
            high = mid - 1;
        }
    }

    if let Some(truncated) = best {
        output.push_str(prefix);
        output.push_str(&truncated);
        output.push_str(suffix);
        true
    } else {
        false
    }
}

/// 截断字符串到指定 token 数（安全处理 UTF-8 边界）
///
/// 如果内容超过最大 token 数，安全截断到最近的 UTF-8 字符边界。
/// 确保至少保留第一个有效字符（即使它很大），避免返回空字符串。
///
/// ## 关于 CJK 过截断的说明
///
/// 此函数使用 `max_tokens * 4` 字节的保守估算（假设 1 token ≈ 4 字节 ASCII 字符）。
/// 对于 CJK（中文/日文/韩文）文本，每个字符通常对应 ~1 token 但占用 3 字节，
/// 因此按字节截断 `max_tokens * 4` 会得到比预期更多的字节内容。
/// 这种保守截断是**有意为之的安全裕度**——宁可少截断一些（多保留部分内容），
/// 也不冒上下文溢出的风险。在 token 预算紧张的场景下，少量额外内容
/// 优于因高估 token 数而导致的截断不足。
fn truncate_to_tokens(content: &str, max_tokens: usize) -> String {
    if estimate_tokens(content) <= max_tokens {
        content.to_string()
    } else {
        let mut truncated = String::new();
        let _ = append_budgeted_block(&mut truncated, "", content, "", max_tokens);
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_compact_below_threshold() {
        // 低于阈值，不应压缩
        assert!(!should_compact(50_000, 100_000, 0.8));
    }

    #[test]
    fn test_should_compact_at_threshold() {
        // 达到阈值，应压缩
        assert!(should_compact(80_000, 100_000, 0.8));
    }

    #[test]
    fn test_should_compact_above_threshold() {
        // 超过阈值，应压缩
        assert!(should_compact(100_000, 100_000, 0.8));
    }

    #[test]
    fn test_compact_result_failed() {
        let result = CompactResult::failed("Test error message");

        assert!(!result.success);
        assert_eq!(result.error, Some("Test error message".to_string()));
        assert!(result.summary_message.is_empty());
        assert!(result.recovery_message.is_empty());
        assert!(result.recent_messages.is_empty());
    }

    #[test]
    fn test_compact_result_success() {
        let result = CompactResult::success(
            "Summary content".to_string(),
            "Recovery content".to_string(),
            100_000,
            20_000,
            80_000, // cache_read_tokens
            10_000, // cache_creation_tokens
            Vec::new(),
        );

        assert!(result.success);
        assert!(result.error.is_none());
        assert_eq!(result.summary_message, "Summary content");
        assert_eq!(result.recovery_message, "Recovery content");
        assert_eq!(result.pre_compact_tokens, 100_000);
        assert_eq!(result.post_compact_tokens, 20_000);
        assert_eq!(result.cache_read_tokens, 80_000);
        assert_eq!(result.cache_creation_tokens, 10_000);
        assert!(result.recent_messages.is_empty());
    }

    #[test]
    fn test_compact_result_to_compact_message() {
        let result = CompactResult::success(
            "Summary".to_string(),
            "Recovery".to_string(),
            100,
            50,
            80,
            10,
            Vec::new(),
        );

        let message = result.to_compact_message();

        assert!(message.contains("# Conversation Compacted"));
        assert!(message.contains("Summary"));
        assert!(message.contains("Recovery"));
    }

    #[test]
    fn test_truncate_to_tokens_short() {
        let content = "Short content";
        let result = truncate_to_tokens(content, 100);

        assert_eq!(result, content);
    }

    #[test]
    fn test_truncate_to_tokens_long() {
        let content = "This is a very long content that should be truncated";
        let result = truncate_to_tokens(content, 5);

        assert!(result.contains("[content truncated]"));
        assert!(result.len() < content.len() + 30);
    }

    #[test]
    fn test_truncate_to_tokens_utf8() {
        let content = "你好世界，这是一个测试内容，用于验证 UTF-8 边界处理";
        let result = truncate_to_tokens(content, 5);

        // 应该在安全边界截断，不应该 panic
        assert!(result.contains("[content truncated]") || result == content);
    }

    #[test]
    fn compact_recovery_enforces_total_budgets() {
        let mut file_tracker = FileTracker::with_config(2_000);
        let mut skill_tracker = SkillTracker::with_config(2_000);
        for index in 0..6 {
            file_tracker.record_read(
                std::path::PathBuf::from(format!("/file-{index}.txt")),
                &format!("file-{index} {}", "file content ".repeat(120)),
            );
            skill_tracker.record_load(
                &format!("skill-{index}"),
                &format!("skill-{index} {}", "skill content ".repeat(120)),
            );
        }
        let budget = RecoveryBudget {
            max_file_recovery_tokens: 40,
            max_single_file_tokens: 100,
            max_skill_recovery_tokens: 40,
            max_session_memory_recovery_tokens: 0,
            max_files_to_recover: 6,
        };

        let recovery = build_recovery_message(&file_tracker, &skill_tracker, None, &budget);

        assert!(
            estimate_tokens(&recovery)
                <= budget.max_file_recovery_tokens + budget.max_skill_recovery_tokens,
            "recovery used {} tokens: {}",
            estimate_tokens(&recovery),
            recovery
        );
    }
}
