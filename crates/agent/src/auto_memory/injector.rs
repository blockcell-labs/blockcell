//! 记忆注入器 - 将 Layer 5 记忆注入到系统提示
//!
//! 加载四种类型记忆文件并注入到 Agent 系统提示中，
//! 按优先级顺序注入，遵守 token 预算限制。

use super::{MemoryType, DEFAULT_INJECTION_MAX_TOKENS};
use crate::token::estimate_tokens;
use blockcell_core::Paths;
use std::collections::HashMap;
use std::path::Path;

/// 记忆注入配置
#[derive(Debug, Clone)]
pub struct InjectionConfig {
    /// 最大注入 token 数
    pub max_tokens: usize,
    /// 注入顺序优先级
    pub priority_order: Vec<MemoryType>,
}

impl Default for InjectionConfig {
    fn default() -> Self {
        Self {
            max_tokens: DEFAULT_INJECTION_MAX_TOKENS,
            priority_order: vec![
                MemoryType::User,      // 用户信息最优先
                MemoryType::Feedback,  // 反馈次之
                MemoryType::Project,   // 项目信息
                MemoryType::Reference, // 参考信息最后
            ],
        }
    }
}

impl From<blockcell_core::config::Layer5Config> for InjectionConfig {
    fn from(c: blockcell_core::config::Layer5Config) -> Self {
        Self {
            max_tokens: c.injection_max_tokens,
            priority_order: vec![
                MemoryType::User,
                MemoryType::Feedback,
                MemoryType::Project,
                MemoryType::Reference,
            ],
        }
    }
}

/// 注入的记忆内容
#[derive(Debug, Clone)]
pub struct InjectedMemory {
    pub memory_type: MemoryType,
    pub content: String,
    pub token_count: usize,
}

/// 记忆注入器
#[derive(Debug, Clone)]
pub struct MemoryInjector {
    config: InjectionConfig,
    cache: HashMap<MemoryType, String>,
}

impl MemoryInjector {
    /// 创建记忆注入器
    pub fn new(config: InjectionConfig) -> Self {
        Self {
            config,
            cache: HashMap::new(),
        }
    }

    /// 使用默认配置创建注入器
    pub fn default_injector() -> Self {
        Self::new(InjectionConfig::default())
    }

    /// Load durable memory exclusively from USER.md and memory/MEMORY.md.
    pub async fn load_canonical(&mut self, paths: &Paths) -> std::io::Result<()> {
        self.cache.clear();
        if let Some(user) = read_optional(&paths.user_md()).await? {
            if !user.trim().is_empty() {
                self.cache.insert(MemoryType::User, user);
            }
        }
        if let Some(memory) = read_optional(&paths.memory_md()).await? {
            for (memory_type, content) in split_canonical_memory(&memory) {
                if !content.trim().is_empty() {
                    self.cache.insert(memory_type, content);
                }
            }
        }
        Ok(())
    }

    /// 加载记忆文件
    ///
    /// 兼容读取已存在的旧 Layer 5 文件；不会创建新的旧格式模板。
    pub async fn load_memories(&mut self, memory_dir: &Path) -> std::io::Result<()> {
        for memory_type in MemoryType::all() {
            let path = memory_dir.join(memory_type.filename());
            match tokio::fs::read_to_string(&path).await {
                Ok(content) => {
                    // 只缓存非空且非模板内容
                    if !content.trim().is_empty() && content.trim() != memory_type.template().trim()
                    {
                        self.cache.insert(memory_type, content);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    continue;
                }
                Err(e) => {
                    // 权限错误、磁盘错误等需要警告
                    tracing::warn!(
                        memory_type = memory_type.name(),
                        path = %path.display(),
                        error = %e,
                        "[MemoryInjector] Failed to load memory file"
                    );
                }
            }
        }
        Ok(())
    }

    /// 注入记忆到系统提示
    pub fn inject_into_system_prompt(&self, system_prompt: &mut String) {
        let injected = self.collect_memories_to_inject();
        if injected.is_empty() {
            return;
        }

        // 构建注入块
        let mut injection_block = String::from("\n\n---\n# 持久化记忆\n\n");
        injection_block.push_str("> 以下是系统自动提取并持久化的跨会话记忆：\n\n");

        for memory in injected {
            injection_block.push_str(&format!(
                "## {} ({})\n\n{}\n\n",
                memory.memory_type.description(),
                memory.memory_type.canonical_filename(),
                memory.content
            ));
        }

        injection_block.push_str("---\n");

        // 注入到系统提示末尾
        system_prompt.push_str(&injection_block);
    }

    /// 构建注入内容字符串（不修改原字符串）
    pub fn build_injection_content(&self) -> String {
        let injected = self.collect_memories_to_inject();
        if injected.is_empty() {
            return String::new();
        }

        let mut injection_block = String::from("\n\n---\n# 持久化记忆\n\n");
        injection_block.push_str("> 以下是系统自动提取并持久化的跨会话记忆：\n\n");

        for memory in injected {
            injection_block.push_str(&format!(
                "## {} ({})\n\n{}\n\n",
                memory.memory_type.description(),
                memory.memory_type.canonical_filename(),
                memory.content
            ));
        }

        injection_block.push_str("---\n");
        injection_block
    }

    /// 收集要注入的记忆
    fn collect_memories_to_inject(&self) -> Vec<InjectedMemory> {
        let mut result = Vec::new();
        let mut used_tokens = 0;

        for memory_type in &self.config.priority_order {
            if let Some(content) = self.cache.get(memory_type) {
                let token_count = estimate_tokens(content);

                if used_tokens + token_count <= self.config.max_tokens {
                    result.push(InjectedMemory {
                        memory_type: *memory_type,
                        content: content.clone(),
                        token_count,
                    });
                    used_tokens += token_count;
                } else {
                    // 截断以适应剩余预算
                    let remaining = self.config.max_tokens - used_tokens;
                    if remaining > 100 {
                        let truncated = truncate_content(content, remaining);
                        result.push(InjectedMemory {
                            memory_type: *memory_type,
                            content: truncated,
                            token_count: remaining,
                        });
                    }
                    break;
                }
            }
        }

        result
    }

    /// 清除缓存
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// 获取缓存中的记忆数量
    pub fn cache_size(&self) -> usize {
        self.cache.len()
    }

    /// 检查是否有缓存内容
    pub fn has_memories(&self) -> bool {
        !self.cache.is_empty()
    }

    /// 获取特定类型的记忆
    pub fn get_memory(&self, memory_type: MemoryType) -> Option<&String> {
        self.cache.get(&memory_type)
    }

    /// 获取各类型记忆的统计信息 (user, project, feedback, reference)
    /// 用于 Layer 5 injection_completed 事件
    pub fn memory_counts(&self) -> (u64, u64, u64, u64) {
        let user = if self.cache.contains_key(&MemoryType::User) {
            1
        } else {
            0
        };
        let project = if self.cache.contains_key(&MemoryType::Project) {
            1
        } else {
            0
        };
        let feedback = if self.cache.contains_key(&MemoryType::Feedback) {
            1
        } else {
            0
        };
        let reference = if self.cache.contains_key(&MemoryType::Reference) {
            1
        } else {
            0
        };
        (user, project, feedback, reference)
    }
}

async fn read_optional(path: &Path) -> std::io::Result<Option<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn split_canonical_memory(content: &str) -> HashMap<MemoryType, String> {
    let mut sections = HashMap::new();
    let mut current = MemoryType::Project;
    for line in content.lines() {
        current = match line.trim() {
            "## Project" => MemoryType::Project,
            "## Feedback" => MemoryType::Feedback,
            "## Reference" => MemoryType::Reference,
            _ => {
                sections
                    .entry(current)
                    .or_insert_with(String::new)
                    .push_str(line);
                sections
                    .entry(current)
                    .or_insert_with(String::new)
                    .push('\n');
                current
            }
        };
    }
    sections
}

/// 截断内容到指定 token 数
///
/// 使用 UTF-8 安全截断，确保不会在多字节字符中间截断导致 panic
fn truncate_content(content: &str, max_tokens: usize) -> String {
    let byte_budget = max_tokens * 4;
    if content.len() <= byte_budget {
        return content.to_string();
    }

    // 找到最近的 UTF-8 字符边界，防止在多字节字符中间截断
    let safe_boundary = if content.is_char_boundary(byte_budget) {
        byte_budget
    } else {
        // 向前查找最近的字符边界
        content
            .char_indices()
            .take_while(|(idx, _)| *idx <= byte_budget)
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0)
    };

    // 尝试在段落边界截断
    let truncated = &content[..safe_boundary];
    if let Some(last_para) = truncated.rfind("\n\n") {
        format!("{}\n\n[... content truncated ...]", &content[..last_para])
    } else {
        format!("{}\n\n[... content truncated ...]", truncated)
    }
}

/// 格式化记忆用于上下文注入
pub fn format_memory_for_context(memory_type: MemoryType, content: &str) -> String {
    format!(
        "<{}>\n{}\n</{}>",
        memory_type.filename().replace(".md", ""),
        content,
        memory_type.filename().replace(".md", "")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_injection_config_default() {
        let config = InjectionConfig::default();
        assert_eq!(config.max_tokens, 4000);
        assert_eq!(config.priority_order.len(), 4);
        assert_eq!(config.priority_order[0], MemoryType::User);
    }

    #[test]
    fn test_memory_injector_new() {
        let injector = MemoryInjector::default_injector();
        assert!(!injector.has_memories());
        assert_eq!(injector.cache_size(), 0);
    }

    #[test]
    fn test_inject_into_system_prompt_empty() {
        let injector = MemoryInjector::default_injector();
        let mut prompt = String::from("Original prompt");
        injector.inject_into_system_prompt(&mut prompt);
        assert_eq!(prompt, "Original prompt");
    }

    #[test]
    fn test_inject_into_system_prompt_with_memories() {
        let config = InjectionConfig::default();
        let mut injector = MemoryInjector::new(config);

        // 手动添加缓存
        injector.cache.insert(
            MemoryType::User,
            "User prefers concise responses.".to_string(),
        );

        let mut prompt = String::from("Original prompt");
        injector.inject_into_system_prompt(&mut prompt);

        assert!(prompt.contains("持久化记忆"));
        assert!(prompt.contains("User prefers concise responses"));
    }

    #[test]
    fn test_truncate_content() {
        let long_content = "A".repeat(10000);
        let truncated = truncate_content(&long_content, 100);

        // 100 tokens = 400 chars
        assert!(truncated.len() < 500);
        assert!(truncated.contains("[... content truncated ...]"));
    }

    #[test]
    fn test_truncate_content_at_paragraph() {
        let content = "Paragraph 1\n\nParagraph 2\n\nParagraph 3\n\n";
        // 使用更小的 token 预算来触发截断
        let truncated = truncate_content(content, 5); // 5 tokens = 20 chars

        // 应该在段落边界截断
        assert!(truncated.contains("Paragraph"));
        assert!(truncated.contains("[... content truncated ...]"));
    }

    #[test]
    fn test_format_memory_for_context() {
        let formatted = format_memory_for_context(MemoryType::User, "Test content");

        assert!(formatted.starts_with("<user>"));
        assert!(formatted.contains("Test content"));
        assert!(formatted.ends_with("</user>"));
    }

    #[test]
    fn test_collect_memories_respects_budget() {
        let config = InjectionConfig {
            max_tokens: 200, // 足够小的预算以触发截断，但 > 100 以确保 remaining > 100
            priority_order: vec![MemoryType::User, MemoryType::Project],
        };
        let mut injector = MemoryInjector::new(config);

        // 添加足够大的内容以确保超过预算
        // 使用更多字符确保超过 max_tokens
        injector.cache.insert(
            MemoryType::User,
            "A".repeat(10000), // tiktoken 会压缩重复字符，使用更多字符
        );

        let injected = injector.collect_memories_to_inject();
        // 应该截断 User 记忆
        assert_eq!(injected.len(), 1);
        // 检查截断标记存在
        assert!(
            injected[0].content.contains("[... content truncated ...]")
                || injected[0].content.len() < 10000,
            "Content should be truncated or smaller than original"
        );
    }

    #[test]
    fn test_truncate_content_utf8_multibyte() {
        // 测试 UTF-8 多字节字符截断不会 panic
        // 中文字符每个占 3 字节，emoji 占 4 字节
        let chinese_content = "这是一段很长的中文内容，用于测试UTF-8截断是否安全。".repeat(100);
        let truncated = truncate_content(&chinese_content, 50);

        // 验证截断成功且没有 panic
        assert!(truncated.contains("[... content truncated ...]"));
        // 验证截断后的内容是有效 UTF-8
        assert!(truncated.chars().all(|c| c.len_utf8() > 0));

        // 测试 emoji 截断
        let emoji_content = "🎉🎊🎁🎈🎂🎄🎅".repeat(100);
        let truncated = truncate_content(&emoji_content, 10);
        assert!(truncated.contains("[... content truncated ...]"));
        assert!(truncated.chars().all(|c| c.len_utf8() > 0));
    }
}
