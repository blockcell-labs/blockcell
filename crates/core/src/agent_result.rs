use serde::{Deserialize, Serialize};

/// 结构化内容块
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContentBlock {
    /// 文本内容
    Text { text: String },
    /// 文件操作记录
    File { path: String, action: FileAction },
}

/// 文件操作类型
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum FileAction {
    Read,
    Write,
    Edit,
    Create,
    Delete,
}

/// 用量详情（参考 Claude Code usage 结构）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageMetrics {
    /// 输入 token 数
    pub input_tokens: u64,
    /// 输出 token 数
    pub output_tokens: u64,
    /// 缓存创建 token 数
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    /// 缓存读取 token 数
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

impl UsageMetrics {
    /// 累加用量
    pub fn accumulate(&mut self, input: u64, output: u64, cache_read: u64, cache_creation: u64) {
        self.input_tokens += input;
        self.output_tokens += output;
        self.cache_read_input_tokens += cache_read;
        self.cache_creation_input_tokens += cache_creation;
    }

    /// 计算缓存命中率
    pub fn cache_hit_rate(&self) -> f64 {
        let total =
            self.input_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens;
        if total > 0 {
            self.cache_read_input_tokens as f64 / total as f64
        } else {
            0.0
        }
    }

    /// 合并另一个用量指标
    pub fn merge(&mut self, other: &UsageMetrics) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
    }
}

/// Agent执行结果状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ResultStatus {
    Success,
    Failed,
    Partial,
}

/// Agent结构化返回结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    /// 结果状态
    pub status: ResultStatus,

    /// 人类可读摘要
    pub summary: String,

    // ===== 新增: 结构化内容 =====
    /// 结构化内容块列表
    #[serde(default)]
    pub content: Vec<ContentBlock>,

    // ===== 新增: 用量详情 =====
    /// Token 用量统计
    #[serde(default)]
    pub usage: UsageMetrics,

    // ===== 新增: 执行统计 =====
    /// 工具调用总数
    #[serde(default)]
    pub total_tool_use_count: u32,

    // ===== 现有字段保持不变 =====
    /// 修改的文件列表
    pub files_modified: Vec<String>,

    /// 创建的文件列表
    pub files_created: Vec<String>,

    /// 测试结果
    pub tests_passed: Option<u32>,
    pub tests_failed: Option<u32>,

    /// 错误信息
    pub error_message: Option<String>,

    /// 执行时间（毫秒）
    pub execution_time_ms: u64,
}

impl AgentResult {
    /// 创建成功结果
    pub fn success(summary: String) -> Self {
        Self {
            status: ResultStatus::Success,
            summary,
            content: vec![],
            usage: UsageMetrics::default(),
            total_tool_use_count: 0,
            files_modified: vec![],
            files_created: vec![],
            tests_passed: None,
            tests_failed: None,
            error_message: None,
            execution_time_ms: 0,
        }
    }

    /// 创建失败结果
    pub fn failed(error: String) -> Self {
        Self {
            status: ResultStatus::Failed,
            summary: "Failed".to_string(),
            content: vec![],
            usage: UsageMetrics::default(),
            total_tool_use_count: 0,
            files_modified: vec![],
            files_created: vec![],
            tests_passed: None,
            tests_failed: None,
            error_message: Some(error),
            execution_time_ms: 0,
        }
    }

    /// 添加文件操作记录
    pub fn add_file_action(&mut self, path: String, action: FileAction) {
        self.content.push(ContentBlock::File {
            path: path.clone(),
            action,
        });
        match action {
            FileAction::Create => self.files_created.push(path),
            FileAction::Edit | FileAction::Write => self.files_modified.push(path),
            FileAction::Delete => self.files_modified.push(path), // 删除也记录在 modified 中
            FileAction::Read => {}                                // 读取不记录在修改/创建列表中
        }
    }

    /// 合并用量统计
    pub fn add_usage(&mut self, other: &UsageMetrics) {
        self.usage.merge(other);
    }

    /// 转换为JSON字符串
    /// 序列化失败时记录警告并返回包含错误信息的 fallback JSON
    pub fn to_json_string(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "AgentResult serialization failed, using fallback");
            Self::serialization_fallback(&e.to_string())
        })
    }

    pub(crate) fn serialization_fallback(error: &str) -> String {
        serde_json::json!({
            "status": "Failed",
            "summary": "Result serialization failed",
            "content": [],
            "files_modified": [],
            "files_created": [],
            "execution_time_ms": 0,
            "error_message": format!("serialization failed: {error}")
        })
        .to_string()
    }

    /// 从JSON解析
    pub fn from_json(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }
}

impl Default for AgentResult {
    fn default() -> Self {
        Self::success("Task completed".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_result_success() {
        let result = AgentResult::success("Files modified".to_string());
        assert_eq!(result.status, ResultStatus::Success);
        assert_eq!(result.summary, "Files modified");
        assert!(result.content.is_empty());
        assert_eq!(result.total_tool_use_count, 0);
    }

    #[test]
    fn test_agent_result_failed() {
        let result = AgentResult::failed("Error occurred".to_string());
        assert_eq!(result.status, ResultStatus::Failed);
        assert!(result.error_message.is_some());
    }

    #[test]
    fn test_content_block_text() {
        let block = ContentBlock::Text {
            text: "Hello".to_string(),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("Hello"));
    }

    #[test]
    fn test_usage_metrics_default() {
        let usage = UsageMetrics::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_creation_input_tokens, 0);
        assert_eq!(usage.cache_read_input_tokens, 0);
    }

    #[test]
    fn test_add_file_action() {
        let mut result = AgentResult::success("Test".to_string());
        result.add_file_action("src/main.rs".to_string(), FileAction::Edit);
        result.add_file_action("src/new.rs".to_string(), FileAction::Create);

        assert_eq!(result.content.len(), 2);
        assert_eq!(result.files_modified.len(), 1);
        assert_eq!(result.files_created.len(), 1);
    }

    #[test]
    fn test_add_usage() {
        let mut result = AgentResult::success("Test".to_string());
        let usage = UsageMetrics {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 20,
            cache_read_input_tokens: 0,
        };
        result.add_usage(&usage);

        assert_eq!(result.usage.input_tokens, 100);
        assert_eq!(result.usage.output_tokens, 50);
        assert_eq!(result.usage.cache_creation_input_tokens, 20);
    }

    #[test]
    fn test_agent_result_serialization() {
        let result = AgentResult {
            status: ResultStatus::Success,
            summary: "Test".to_string(),
            content: vec![ContentBlock::Text {
                text: "result".to_string(),
            }],
            usage: UsageMetrics {
                input_tokens: 100,
                output_tokens: 50,
                ..Default::default()
            },
            total_tool_use_count: 5,
            files_modified: vec!["a.rs".to_string(), "b.rs".to_string()],
            files_created: vec![],
            tests_passed: Some(3),
            tests_failed: None,
            error_message: None,
            execution_time_ms: 100,
        };

        let json = result.to_json_string();
        assert!(json.contains("input_tokens"));
        assert!(json.contains("total_tool_use_count"));

        let parsed = AgentResult::from_json(&json);
        assert!(parsed.is_some());
        let parsed = parsed.unwrap();
        assert_eq!(parsed.status, ResultStatus::Success);
        assert_eq!(parsed.files_modified.len(), 2);
        assert_eq!(parsed.usage.input_tokens, 100);
    }
}
