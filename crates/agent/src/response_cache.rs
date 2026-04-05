use blockcell_tools::ResponseCacheOps;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::debug;

/// Per-session cache for large list/table responses.
///
/// When the LLM returns a long numbered/bulleted list, storing the full text in history
/// causes exponential token growth across turns. This cache stores the content separately
/// and replaces the history entry with a compact stub. The LLM can call `session_recall`
/// to retrieve the full content when the user references a specific item.
#[derive(Clone)]
pub struct ResponseCache {
    inner: Arc<Mutex<ResponseCacheInner>>,
}

struct ResponseCacheInner {
    /// session_key → ref_id → CacheEntry
    data: HashMap<String, HashMap<String, CacheEntry>>,
    /// Maximum cached entries per session (evicts oldest on overflow).
    max_per_session: usize,
}

struct CacheEntry {
    content: String,
    #[allow(dead_code)]
    item_count: usize,
    created_at: i64,
}

impl ResponseCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ResponseCacheInner {
                data: HashMap::new(),
                max_per_session: 10,
            })),
        }
    }

    /// 安全获取锁，处理锁中毒情况
    ///
    /// 如果锁中毒（持有锁的线程 panic），会恢复并返回中毒时的数据。
    /// 这是安全的，因为 ResponseCache 只是缓存，数据丢失不影响功能正确性。
    ///
    /// ## 诊断信息
    /// - 锁中毒通常是上游 panic 导致，需要检查相关日志
    /// - 建议在监控系统中跟踪锁中毒频率
    fn get_lock(&self) -> std::sync::MutexGuard<'_, ResponseCacheInner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                // 锁中毒，记录警告并恢复
                // 注意：此处无法获取 session_key，因为锁已中毒
                // 建议检查上游 panic 日志以定位根因
                tracing::warn!(
                    "[response_cache] Lock poisoned (upstream thread likely panicked), recovering with potentially lost cache data. Check upstream panic logs for root cause."
                );
                // into_inner() 返回中毒时的数据，我们继续使用它
                poisoned.into_inner()
            }
        }
    }

    /// If `content` qualifies as a cacheable list/table, stores it and returns a compact stub.
    /// Returns `None` if the content does not meet the caching threshold.
    pub fn maybe_cache_and_stub(&self, session_key: &str, content: &str) -> Option<String> {
        if !Self::is_cacheable(content) {
            return None;
        }
        let items = Self::extract_list_items(content);
        if items.len() < 5 {
            return None;
        }

        let ref_id = Self::generate_ref_id(session_key);
        let preview = items
            .iter()
            .take(3)
            .enumerate()
            .map(|(i, item)| {
                let trimmed: String = item.chars().take(100).collect();
                format!("{}. {}", i + 1, trimmed)
            })
            .collect::<Vec<_>>()
            .join("\n");

        let stub = format!(
            "[已缓存{}条结果，ID: ref:{}]\n{}\n...（共{}条，使用 session_recall 工具获取完整内容）",
            items.len(),
            ref_id,
            preview,
            items.len()
        );

        let entry = CacheEntry {
            content: content.to_string(),
            item_count: items.len(),
            created_at: chrono::Utc::now().timestamp(),
        };

        let mut inner = self.get_lock();
        let max_per_session = inner.max_per_session;
        let session_cache = inner
            .data
            .entry(session_key.to_string())
            .or_default();

        // Evict oldest entry if at capacity
        if session_cache.len() >= max_per_session {
            if let Some(oldest_key) = session_cache
                .iter()
                .min_by_key(|(_, e)| e.created_at)
                .map(|(k, _)| k.clone())
            {
                session_cache.remove(&oldest_key);
            }
        }

        session_cache.insert(ref_id.clone(), entry);
        debug!(
            session_key,
            ref_id = %ref_id,
            item_count = items.len(),
            "Cached large list response"
        );

        Some(stub)
    }

    /// Retrieve cached content by ref_id (with or without "ref:" prefix).
    pub fn recall(&self, session_key: &str, ref_id: &str) -> Option<String> {
        let bare_id = ref_id.strip_prefix("ref:").unwrap_or(ref_id);
        let inner = self.get_lock();
        inner
            .data
            .get(session_key)
            .and_then(|m| m.get(bare_id))
            .map(|e| e.content.clone())
    }

    /// Remove all cache entries for a session (e.g. on session reset).
    pub fn clear_session(&self, session_key: &str) {
        let mut inner = self.get_lock();
        inner.data.remove(session_key);
    }

    // ──────────────────────────────────────────────
    // Internal helpers
    // ──────────────────────────────────────────────

    /// Content is cacheable when it is long enough and contains a list.
    fn is_cacheable(content: &str) -> bool {
        content.chars().count() > 800
    }

    /// Extract list items from a numbered or bulleted list.
    /// Handles: `1. item`, `- item`, `* item`, `• item`
    fn extract_list_items(content: &str) -> Vec<String> {
        let mut items = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Numbered: "1. " / "1) "
            if let Some(rest) = Self::strip_numbered_prefix(trimmed) {
                if !rest.is_empty() {
                    items.push(rest.to_string());
                    continue;
                }
            }
            // Bulleted: "- " / "* " / "• "
            if let Some(rest) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
                .or_else(|| trimmed.strip_prefix("• "))
            {
                if !rest.is_empty() {
                    items.push(rest.to_string());
                }
            }
        }
        items
    }

    fn strip_numbered_prefix(s: &str) -> Option<&str> {
        let mut idx = 0;
        for c in s.chars() {
            if c.is_ascii_digit() {
                idx += c.len_utf8();
            } else {
                break;
            }
        }
        if idx == 0 {
            return None;
        }
        let rest = &s[idx..];
        // Accept ". " or ") "
        if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            Some(r)
        } else {
            None
        }
    }

    /// Generate a short deterministic+random ref_id from session_key + timestamp.
    fn generate_ref_id(session_key: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let ts = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let mut hasher = DefaultHasher::new();
        session_key.hash(&mut hasher);
        ts.hash(&mut hasher);
        let h = hasher.finish();
        // 8 lowercase hex chars
        format!("{:08x}", h & 0xFFFF_FFFF)
    }
}

impl Default for ResponseCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponseCacheOps for ResponseCache {
    fn recall_json(&self, session_key: &str, ref_id: &str) -> String {
        match self.recall(session_key, ref_id) {
            Some(content) => serde_json::json!({
                "ref_id": ref_id,
                "content": content,
                "status": "found"
            })
            .to_string(),
            None => serde_json::json!({
                "ref_id": ref_id,
                "error": "未找到对应的缓存内容，可能已过期或 ID 不正确",
                "status": "not_found"
            })
            .to_string(),
        }
    }
}

// ============================================================================
// Layer 1: 工具结果存储
// ============================================================================

use std::collections::HashSet;
use std::path::PathBuf;

/// 工具结果存储子目录名
pub const TOOL_RESULTS_SUBDIR: &str = "tool-results";

/// 预览大小（字节）
pub const PREVIEW_SIZE_BYTES: usize = 2000;

/// 默认最大结果大小 (~50KB)
pub const DEFAULT_MAX_RESULT_SIZE_CHARS: usize = 50_000;

/// 消息级别上限 (~150KB)
pub const MAX_TOOL_RESULTS_PER_MESSAGE_CHARS: usize = 150_000;

/// 清理标记消息
pub const TIME_BASED_MC_CLEARED_MESSAGE: &str = "[Old tool result content cleared]";

/// 图片/文档 token 估算上限
pub const IMAGE_MAX_TOKEN_SIZE: usize = 2000;

/// 持久化结果信息
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersistedToolResult {
    /// 持久化文件路径
    pub filepath: PathBuf,
    /// 原始内容大小（字符数）
    pub original_size: usize,
    /// 是否为 JSON 格式（数组内容）
    pub is_json: bool,
    /// 预览内容
    pub preview: String,
    /// 是否有更多内容被截断
    pub has_more: bool,
}

/// 持久化失败的错误结果
#[derive(Debug, Clone)]
pub struct PersistToolResultError {
    pub error: String,
}

/// 会话级别的内容替换决策状态
///
/// 关键原则：决策一旦做出，永不改变
/// - seenIds 中的 ID，其命运已确定
/// - 已替换的永远替换相同内容 (存储在 replacements Map)
/// - 未替换的永不替换
///   目的：保证 Prompt Cache 前缀稳定
///
/// ## 线程安全性
///
/// 此类型实现了 `Send`（因为内部集合类型是 `Send`），但**不应在多个并发任务间共享**。
///
/// ### 安全使用模式
///
/// 1. **单任务所有权**: 此类型应始终由单个异步任务独占持有
/// 2. **顺序操作**: 所有读写操作应在同一任务中顺序执行
/// 3. **跨 .await 点**: 使用克隆-修改-写回模式，而非共享引用
///
/// ### 为什么不使用 `Arc<RwLock<...>>`？
///
/// 虽然 `Arc<RwLock<ContentReplacementState>>` 可以安全共享，但会破坏 Prompt Cache 语义：
/// - Prompt Cache 要求决策一旦做出就**永不改变**
/// - 共享状态可能导致不同任务看到不同的决策
/// - 这会破坏缓存前缀的稳定性
///
/// ## 在 MemorySystem 中的安全使用模式
///
/// 当前设计通过以下方式确保安全性：
///
/// 1. **独占所有权**: `AgentRuntime` 持有 `MemorySystem` 的独占所有权 (`&mut self`)
/// 2. **顺序执行**: 所有操作都在单个异步任务中顺序执行
/// 3. **克隆-修改-写回模式**: 跨 `.await` 点时使用
///
/// ### 克隆-修改-写回模式示例
///
/// ```ignore
/// // 1. 克隆状态（在 .await 之前）
/// let state = memory_system.content_replacement_state().clone();
/// let mut state_mut = state.clone();
///
/// // 2. 传递副本给异步函数
/// let result = apply_budget_async(&messages, &candidates, &mut state_mut, ...).await;
///
/// // 3. 写回状态（在 .await 之后）
/// *memory_system.content_replacement_state_mut() = state_mut;
/// ```
///
/// ## Forked Agent 使用
///
/// Forked Agent 通过 `clone_state()` 创建独立副本，与父代理状态隔离：
///
/// ```ignore
/// // 在 SubagentOverrides 中设置
/// let overrides = SubagentOverrides {
///     content_replacement_state: Some(parent_state.clone_state()),
///     ..Default::default()
/// };
/// ```
///
/// 这确保了 Forked Agent 的状态修改不会影响父代理的 Prompt Cache 一致性。
#[derive(Debug, Clone, Default)]
pub struct ContentReplacementState {
    /// 已处理的 tool_use_id 集合
    pub seen_ids: HashSet<String>,
    /// id -> 替换内容映射
    pub replacements: HashMap<String, String>,
    /// 插入顺序（用于 LRU 淘汰）
    insertion_order: Vec<String>,
}

/// 最大条目数限制
pub const MAX_REPLACEMENT_ENTRIES: usize = 1000;

/// 可序列化的替换决策记录，写入 transcript
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContentReplacementRecord {
    /// 替换类型
    pub kind: String,
    /// 工具调用 ID
    pub tool_use_id: String,
    /// 替换后的内容（精确字符串）
    pub replacement: String,
}

/// 待处理的工具结果候选
#[derive(Debug, Clone)]
pub struct ToolResultCandidate {
    pub tool_use_id: String,
    pub content: String,
    pub size: usize,
}

impl ContentReplacementState {
    /// 创建新的状态
    pub fn new() -> Self {
        Self::default()
    }

    /// 检查是否已处理
    pub fn is_seen(&self, tool_use_id: &str) -> bool {
        self.seen_ids.contains(tool_use_id)
    }

    /// 标记为已处理
    pub fn mark_seen(&mut self, tool_use_id: String) {
        if self.seen_ids.insert(tool_use_id.clone()) {
            self.insertion_order.push(tool_use_id);
        }
        self.prune_if_needed();
    }

    /// 获取替换内容
    pub fn get_replacement(&self, tool_use_id: &str) -> Option<&str> {
        self.replacements.get(tool_use_id).map(|s| s.as_str())
    }

    /// 设置替换内容
    pub fn set_replacement(&mut self, tool_use_id: String, replacement: String) {
        let is_new = !self.seen_ids.contains(&tool_use_id);

        self.seen_ids.insert(tool_use_id.clone());
        self.replacements.insert(tool_use_id.clone(), replacement);

        if is_new {
            self.insertion_order.push(tool_use_id);
        }

        self.prune_if_needed();
    }

    /// 如果超过限制，删除最老的条目
    fn prune_if_needed(&mut self) {
        while self.insertion_order.len() > MAX_REPLACEMENT_ENTRIES {
            if let Some(oldest_id) = self.insertion_order.first().cloned() {
                self.seen_ids.remove(&oldest_id);
                self.replacements.remove(&oldest_id);
                self.insertion_order.remove(0);
            } else {
                break;
            }
        }
    }

    /// 克隆状态用于 cache-sharing fork
    pub fn clone_state(&self) -> Self {
        Self {
            seen_ids: self.seen_ids.clone(),
            replacements: self.replacements.clone(),
            insertion_order: self.insertion_order.clone(),
        }
    }

    /// 清空状态
    pub fn clear(&mut self) {
        self.seen_ids.clear();
        self.replacements.clear();
        self.insertion_order.clear();
    }

    /// 从 transcript 记录重建状态
    pub fn reconstruct(
        tool_use_ids: &[String],
        records: &[ContentReplacementRecord],
        inherited_replacements: Option<&HashMap<String, String>>,
    ) -> Self {
        let mut state = Self::default();

        // 收集所有候选 tool_use_id
        for tool_use_id in tool_use_ids {
            state.seen_ids.insert(tool_use_id.clone());
            state.insertion_order.push(tool_use_id.clone());
        }

        // 从 records 恢复 replacements
        for record in records {
            state.replacements.insert(record.tool_use_id.clone(), record.replacement.clone());
            // 确保顺序跟踪
            if !state.insertion_order.contains(&record.tool_use_id) {
                state.insertion_order.push(record.tool_use_id.clone());
            }
        }

        // 从继承的 replacements 填充空缺
        if let Some(inherited) = inherited_replacements {
            for (id, replacement) in inherited {
                if !state.replacements.contains_key(id) {
                    state.replacements.insert(id.clone(), replacement.clone());
                    if !state.insertion_order.contains(id) {
                        state.insertion_order.push(id.clone());
                    }
                }
            }
        }

        state
    }
}

/// 生成内容预览
///
/// 在换行边界截断以保持可读性，确保在 UTF-8 字符边界处截断避免 panic
pub fn generate_preview(content: &str, max_bytes: usize) -> (String, bool) {
    if content.len() <= max_bytes {
        return (content.to_string(), false);
    }

    // 确保在 UTF-8 字符边界处截断，避免 panic
    // floor_char_boundary 返回不超过 max_bytes 的最大有效字符边界
    let safe_boundary = content.floor_char_boundary(max_bytes);

    // 在安全边界内查找最后一个换行符，避免在行中间截断
    let truncated = &content[..safe_boundary];
    let last_newline = truncated.rfind('\n');

    // 如果找到换行符且位置合理（> 50% 限制），使用它
    let cut_point = last_newline
        .filter(|&pos| pos > safe_boundary / 2)
        .unwrap_or(safe_boundary);

    (content[..cut_point].to_string(), true)
}

/// 格式化文件大小
fn format_file_size(size: usize) -> String {
    if size < 1024 {
        format!("{} B", size)
    } else if size < 1024 * 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    }
}

/// 持久化输出标签
pub const PERSISTED_OUTPUT_TAG: &str = "<persisted-output>";
pub const PERSISTED_OUTPUT_CLOSING_TAG: &str = "</persisted-output>";

/// 内存保留标签（当磁盘持久化失败时使用）
pub const MEMORY_FALLBACK_TAG: &str = "<memory-fallback>";
pub const MEMORY_FALLBACK_CLOSING_TAG: &str = "</memory-fallback>";

/// 磁盘持久化失败时的警告消息
pub const DISK_PERSIST_FAILED_WARNING: &str = "Warning: Disk persistence failed. Content preserved in memory preview.";

/// 清理 tool_use_id 以防止路径注入
///
/// `tool_use_id` 来自 LLM 输出，可能包含：
/// - 路径遍历字符 (`../`, `..\\`)
/// - 换行符 (`\n`, `\r`)
/// - 空字符 (`\0`)
/// - 其他可能导致路径问题的字符
///
/// 清理策略：
/// 1. 只保留字母、数字、连字符和下划线
/// 2. 检查是否为 Windows 保留文件名
/// 3. 限制长度
fn sanitize_tool_use_id(tool_use_id: &str) -> String {
    // 移除或替换危险字符
    let sanitized: String = tool_use_id
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();

    // 如果清理后为空，使用默认值
    if sanitized.is_empty() {
        return format!("tool-{}", uuid::Uuid::new_v4().simple());
    }

    // 限制长度
    let result = if sanitized.len() > 64 {
        sanitized[..64].to_string()
    } else {
        sanitized
    };

    // 检查 Windows 保留文件名
    // CON, PRN, AUX, NUL, COM1-COM9, LPT1-LPT9
    let upper = result.to_uppercase();
    let is_reserved = matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL"
        | "COM1" | "COM2" | "COM3" | "COM4" | "COM5"
        | "COM6" | "COM7" | "COM8" | "COM9"
        | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5"
        | "LPT6" | "LPT7" | "LPT8" | "LPT9"
    );

    if is_reserved {
        // 添加后缀避免保留名
        format!("{}-{}", result, uuid::Uuid::new_v4().simple())
    } else {
        result
    }
}

/// 清理 session_key 防止路径遍历攻击
///
/// 清理策略：
/// 1. 只保留字母、数字、连字符和下划线
/// 2. 限制长度
fn sanitize_session_key(session_key: &str) -> String {
    let sanitized: String = session_key
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();

    // 如果清理后为空，使用默认值
    if sanitized.is_empty() {
        return format!("session-{}", uuid::Uuid::new_v4().simple());
    }

    // 限制长度 (UUID 格式通常是 36 字符，允许稍长)
    if sanitized.len() > 64 {
        sanitized[..64].to_string()
    } else {
        sanitized
    }
}

/// 构建内存 fallback 替换消息（当磁盘持久化失败时）
///
/// 包含预览内容，告知用户磁盘持久化失败但数据已通过预览保留。
fn build_memory_fallback_message(content: &str, tool_use_id: &str) -> String {
    // 清理 tool_use_id 以防止换行符注入到日志/显示中
    let safe_tool_use_id = sanitize_tool_use_id(tool_use_id);

    let (preview, has_more) = generate_preview(content, PREVIEW_SIZE_BYTES);

    let mut message = format!(
        "{}\n{}\n\nTool ID: {}\nPreview (first {}):\n{}",
        MEMORY_FALLBACK_TAG,
        DISK_PERSIST_FAILED_WARNING,
        safe_tool_use_id,
        format_file_size(PREVIEW_SIZE_BYTES),
        preview
    );
    if has_more {
        message.push_str("\n... (content truncated due to disk error)");
    }
    message.push('\n');
    message.push_str(MEMORY_FALLBACK_CLOSING_TAG);
    message
}

/// 构建大结果消息
pub fn build_large_tool_result_message(result: &PersistedToolResult) -> String {
    let mut message = format!(
        "{}\nOutput too large ({}). Full output saved to: {}\n\n",
        PERSISTED_OUTPUT_TAG,
        format_file_size(result.original_size),
        result.filepath.display()
    );
    message.push_str(&format!(
        "Preview (first {}):\n{}",
        format_file_size(PREVIEW_SIZE_BYTES),
        result.preview
    ));
    if result.has_more {
        message.push_str("\n...\n");
    } else {
        message.push('\n');
    }
    message.push_str(PERSISTED_OUTPUT_CLOSING_TAG);
    message
}

/// 持久化工具结果到磁盘
///
/// 异步函数，将大型工具输出保存到磁盘并返回预览
pub async fn persist_tool_result(
    content: &str,
    tool_use_id: &str,
    session_key: &str,
    workspace_dir: &std::path::Path,
) -> Result<PersistedToolResult, PersistToolResultError> {
    // 清理 tool_use_id 防止路径注入
    let safe_tool_use_id = sanitize_tool_use_id(tool_use_id);

    // 清理 session_key 防止路径遍历
    let safe_session_key = sanitize_session_key(session_key);

    let dir = workspace_dir
        .join("sessions")
        .join(&safe_session_key)
        .join(TOOL_RESULTS_SUBDIR);

    // 验证目录路径仍在工作目录内（防止路径遍历攻击）
    // 注意：由于 session_key 已被清理，这主要是防御性编程
    let dir_canonical = match std::fs::canonicalize(&dir.parent().unwrap_or(&dir)) {
        Ok(p) => p,
        Err(_) => dir.clone(), // 目录不存在时使用原始路径
    };
    let workspace_canonical = match std::fs::canonicalize(workspace_dir) {
        Ok(p) => p,
        Err(_) => workspace_dir.to_path_buf(),
    };
    if !dir_canonical.starts_with(&workspace_canonical) {
        return Err(PersistToolResultError {
            error: "Path traversal detected: directory escapes workspace".to_string(),
        });
    }

    // 创建目录
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return Err(PersistToolResultError {
            error: format!("Failed to create directory: {}", e),
        });
    }

    // 判断是否 JSON（数组内容）
    let is_json = content.trim_start().starts_with('[');
    let ext = if is_json { "json" } else { "txt" };
    let filepath = dir.join(format!("{}.{}", safe_tool_use_id, ext));

    // 验证最终文件路径仍在预期目录内
    if !filepath.starts_with(&dir) {
        return Err(PersistToolResultError {
            error: "Path traversal detected: file escapes target directory".to_string(),
        });
    }

    // 原子写入：使用 create_new 避免竞争
    let content_str = if is_json {
        // 格式化 JSON
        match serde_json::from_str::<serde_json::Value>(content) {
            Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| content.to_string()),
            Err(_) => content.to_string(),
        }
    } else {
        content.to_string()
    };

    match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&filepath)
        .await
    {
        Ok(mut file) => {
            use tokio::io::AsyncWriteExt;
            if let Err(e) = file.write_all(content_str.as_bytes()).await {
                return Err(PersistToolResultError {
                    error: format!("Failed to write file: {}", e),
                });
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // 文件已存在，说明之前已持久化，跳过写入
        }
        Err(e) => {
            return Err(PersistToolResultError {
                error: format!("Failed to open file: {}", e),
            });
        }
    }

    let (preview, has_more) = generate_preview(content, PREVIEW_SIZE_BYTES);

    Ok(PersistedToolResult {
        filepath,
        original_size: content.len(),
        is_json,
        preview,
        has_more,
    })
}

#[cfg(test)]
mod layer1_tests {
    use super::*;

    #[test]
    fn test_generate_preview_short() {
        let content = "short content";
        let (preview, has_more) = generate_preview(content, 100);
        assert_eq!(preview, content);
        assert!(!has_more);
    }

    #[test]
    fn test_generate_preview_long() {
        let content = "line1\nline2\nline3\nline4\nline5\n";
        let (preview, has_more) = generate_preview(content, 20);
        assert!(preview.len() <= 20);
        assert!(has_more);
        // Should break at newline
        assert!(preview.ends_with('\n') || preview.len() < 20);
    }

    #[test]
    fn test_content_replacement_state() {
        let mut state = ContentReplacementState::default();
        state.seen_ids.insert("tool-1".to_string());
        state.replacements.insert("tool-1".to_string(), "replacement".to_string());

        let cloned = state.clone_state();
        assert!(cloned.seen_ids.contains("tool-1"));
        assert_eq!(cloned.replacements.get("tool-1"), Some(&"replacement".to_string()));
    }

    #[test]
    fn test_build_large_tool_result_message() {
        let result = PersistedToolResult {
            filepath: PathBuf::from("/path/to/file.json"),
            original_size: 100_000,
            is_json: true,
            preview: "preview content".to_string(),
            has_more: true,
        };

        let message = build_large_tool_result_message(&result);
        assert!(message.starts_with(PERSISTED_OUTPUT_TAG));
        assert!(message.ends_with(PERSISTED_OUTPUT_CLOSING_TAG));
        assert!(message.contains("97.7 KB"));
        assert!(message.contains("preview content"));
    }

    #[test]
    fn test_format_file_size() {
        assert_eq!(format_file_size(500), "500 B");
        assert_eq!(format_file_size(1024), "1.0 KB");
        assert_eq!(format_file_size(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn test_process_tool_result_small() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let state = ContentReplacementState::default();
            let content = "small content".to_string();
            let workspace = std::path::Path::new("/tmp/test");

            let result = process_tool_result(
                &content,
                "tool-1",
                "test-session",  // session_key
                &state,
                DEFAULT_MAX_RESULT_SIZE_CHARS,
                workspace,
            ).await;

            // 小内容不需要持久化
            assert!(result.is_none());
        });
    }

    // ========== 核心路径测试 ==========

    #[test]
    fn test_content_replacement_state_seen_tracking() {
        let mut state = ContentReplacementState::default();

        // 初始状态：未处理
        assert!(!state.is_seen("tool-1"));

        // 标记为已处理
        state.mark_seen("tool-1".to_string());
        assert!(state.is_seen("tool-1"));

        // 重复标记不会出问题
        state.mark_seen("tool-1".to_string());
        assert!(state.is_seen("tool-1"));
    }

    #[test]
    fn test_content_replacement_state_replacement() {
        let mut state = ContentReplacementState::default();

        // 设置替换内容
        state.set_replacement("tool-1".to_string(), "replacement content".to_string());

        // 验证替换
        assert!(state.is_seen("tool-1"));
        assert_eq!(state.get_replacement("tool-1"), Some("replacement content"));

        // 未处理的工具没有替换
        assert!(!state.is_seen("tool-2"));
        assert_eq!(state.get_replacement("tool-2"), None);
    }

    #[test]
    fn test_content_replacement_state_reconstruct() {
        let tool_ids = vec!["tool-1".to_string(), "tool-2".to_string()];
        let records = vec![
            ContentReplacementRecord {
                kind: "persist".to_string(),
                tool_use_id: "tool-1".to_string(),
                replacement: "replacement-1".to_string(),
            },
        ];
        let inherited = Some(&HashMap::from([("tool-3".to_string(), "inherited".to_string())]));

        let state = ContentReplacementState::reconstruct(&tool_ids, &records, inherited);

        // 验证 tool_ids 中的 ID 被标记为已处理
        assert!(state.is_seen("tool-1"));
        assert!(state.is_seen("tool-2"));

        // 验证替换内容
        assert_eq!(state.get_replacement("tool-1"), Some("replacement-1"));
        // inherited ID 有替换内容，但不被标记为 seen（因为不在 tool_ids 中）
        assert_eq!(state.get_replacement("tool-3"), Some("inherited"));
    }

    #[test]
    fn test_content_replacement_state_pruning() {
        let mut state = ContentReplacementState::default();

        // 添加超过限制的条目
        for i in 0..MAX_REPLACEMENT_ENTRIES + 100 {
            state.set_replacement(format!("tool-{}", i), format!("content-{}", i));
        }

        // 验证条目数被限制
        assert!(state.seen_ids.len() <= MAX_REPLACEMENT_ENTRIES);
        assert!(state.replacements.len() <= MAX_REPLACEMENT_ENTRIES);
        assert!(state.insertion_order.len() <= MAX_REPLACEMENT_ENTRIES);
    }

    #[test]
    fn test_collect_tool_result_candidates() {
        use blockcell_core::types::ChatMessage;

        let messages = vec![
            ChatMessage::user("Hello"),
            ChatMessage::tool_result("call-1", "result 1"),
            ChatMessage::assistant("Hi"),
            ChatMessage::tool_result("call-2", "result 2 with more content"),
        ];

        let candidates = collect_tool_result_candidates(&messages);

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].tool_use_id, "call-1");
        assert_eq!(candidates[1].tool_use_id, "call-2");
        assert!(candidates[1].size > candidates[0].size);
    }

    #[test]
    fn test_apply_budget_basic() {
        use blockcell_core::types::ChatMessage;

        let messages = vec![
            ChatMessage::user("Hello"),
            ChatMessage::tool_result("call-1", &"x".repeat(60_000)),
            ChatMessage::tool_result("call-2", &"y".repeat(60_000)),
        ];

        let candidates = collect_tool_result_candidates(&messages);
        let mut state = ContentReplacementState::default();
        let budget = 100_000; // 150KB budget

        let result = apply_budget(&messages, &candidates, &mut state, budget);

        // 应该触发替换
        assert!(state.is_seen("call-1") || state.is_seen("call-2"));
        // 消息数量保持一致
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_generate_preview_utf8_boundary() {
        // 测试 UTF-8 边界处理
        let content = "你好世界".repeat(1000); // 多字节字符
        let (preview, has_more) = generate_preview(&content, 100);

        // 预览应该在安全边界截断
        assert!(preview.len() <= 105); // 允许一点误差
        assert!(has_more);
        // 确保没有 panic
    }

    #[test]
    fn test_sanitize_tool_use_id() {
        // 正常 ID
        assert_eq!(sanitize_tool_use_id("tool-123"), "tool-123");

        // 包含路径遍历
        let sanitized = sanitize_tool_use_id("../../../etc/passwd");
        assert!(!sanitized.contains(".."));
        assert!(!sanitized.contains("/"));

        // 空字符串生成默认值
        let empty = sanitize_tool_use_id("");
        assert!(empty.starts_with("tool-"));

        // Windows 保留文件名
        let con = sanitize_tool_use_id("CON");
        assert!(con.starts_with("CON-"));
        assert!(con.len() > "CON".len());

        let aux = sanitize_tool_use_id("aux");
        assert!(aux.starts_with("aux-"));

        let com1 = sanitize_tool_use_id("COM1");
        assert!(com1.starts_with("COM1-"));

        let lpt9 = sanitize_tool_use_id("LPT9");
        assert!(lpt9.starts_with("LPT9-"));

        // 非保留名不受影响
        let normal = sanitize_tool_use_id("normal_file");
        assert_eq!(normal, "normal_file");
    }
}

// ============================================================================
// Layer 1: 两层预算执行逻辑
// ============================================================================

/// 第一层：处理单个工具结果
///
/// 如果内容超过阈值，持久化到磁盘并返回替换消息。
/// 如果内容在阈值内，返回 None（无需处理）。
///
/// ## 状态冻结原则
/// - 一旦决定持久化某个工具结果，该决定永不改变
/// - 替换内容存储在 `state.replacements` 中，保证缓存一致性
pub async fn process_tool_result(
    content: &str,
    tool_use_id: &str,
    session_key: &str,
    state: &ContentReplacementState,
    threshold: usize,
    workspace_dir: &std::path::Path,
) -> Option<String> {
    // 检查是否已经处理过
    if state.is_seen(tool_use_id) {
        // 返回之前的决定
        return state.get_replacement(tool_use_id).map(|s| s.to_string());
    }

    // 检查内容大小
    if content.len() <= threshold {
        return None;
    }

    // 需要持久化
    match persist_tool_result(content, tool_use_id, session_key, workspace_dir).await {
        Ok(result) => {
            let message = build_large_tool_result_message(&result);
            Some(message)
        }
        Err(e) => {
            // 磁盘持久化失败，使用内存 fallback
            tracing::error!(
                tool_use_id = %tool_use_id,
                error = %e.error,
                "[process_tool_result] Failed to persist, using memory fallback"
            );
            let fallback_message = build_memory_fallback_message(content, tool_use_id);
            Some(fallback_message)
        }
    }
}

/// 第二层：收集工具结果候选
///
/// 从消息中提取所有工具结果，计算总大小，返回需要处理的候选列表。
/// 使用场景：Query 循环开始时检查消息级别预算。
pub fn collect_tool_result_candidates(
    messages: &[blockcell_core::types::ChatMessage],
) -> Vec<ToolResultCandidate> {
    let mut candidates = Vec::new();

    for message in messages {
        if message.role != "tool" {
            continue;
        }

        let tool_call_id = match &message.tool_call_id {
            Some(id) => id.clone(),
            None => continue,
        };

        let content = match &message.content {
            serde_json::Value::String(s) => s.clone(),
            _ => continue,
        };

        let size = content.len();
        candidates.push(ToolResultCandidate {
            tool_use_id: tool_call_id,
            content,
            size,
        });
    }

    candidates
}

/// 第二层：应用预算限制
///
/// 如果工具结果总和超过预算，选择最大的结果进行持久化。
/// 返回替换后的消息列表。
///
/// ## 参数
/// - `messages`: 原始消息列表
/// - `candidates`: 工具结果候选列表
/// - `state`: 内容替换状态（会被更新）
/// - `budget`: 消息级别预算
/// - `workspace_dir`: 工作目录
pub fn apply_budget(
    messages: &[blockcell_core::types::ChatMessage],
    candidates: &[ToolResultCandidate],
    state: &mut ContentReplacementState,
    budget: usize,
) -> Vec<blockcell_core::types::ChatMessage> {
    // 计算总大小
    let total_size: usize = candidates.iter().map(|c| c.size).sum();

    // 如果未超预算，直接返回原消息
    if total_size <= budget {
        return messages.to_vec();
    }

    // 需要持久化哪些结果？
    // 策略：按大小降序排列，持久化最大的，直到总大小低于预算
    let mut sorted_candidates: Vec<_> = candidates.iter().collect();
    sorted_candidates.sort_by(|a, b| b.size.cmp(&a.size));

    // 标记需要持久化的候选
    let mut to_persist = std::collections::HashSet::new();
    let mut current_size = total_size;

    for candidate in &sorted_candidates {
        if current_size <= budget {
            break;
        }

        // 检查是否已经处理过
        if state.is_seen(&candidate.tool_use_id) {
            continue;
        }

        to_persist.insert(candidate.tool_use_id.clone());
        current_size = current_size.saturating_sub(candidate.size);
    }

    // 如果没有需要持久化的，返回原消息
    if to_persist.is_empty() {
        return messages.to_vec();
    }

    // 应用替换
    messages
        .iter()
        .map(|msg| {
            if msg.role != "tool" {
                return msg.clone();
            }

            let tool_call_id = match &msg.tool_call_id {
                Some(id) => id,
                None => return msg.clone(),
            };

            if to_persist.contains(tool_call_id) {
                // 标记为已处理
                state.mark_seen(tool_call_id.clone());

                // 创建替换消息（实际路径需要持久化后获取）
                let replacement = format!(
                    "{}\nOutput too large, persisted to disk.\n\nPreview:\n{}\n{}",
                    PERSISTED_OUTPUT_TAG,
                    TIME_BASED_MC_CLEARED_MESSAGE,
                    PERSISTED_OUTPUT_CLOSING_TAG
                );

                state.set_replacement(tool_call_id.clone(), replacement.clone());

                let mut new_msg = msg.clone();
                new_msg.content = serde_json::Value::String(replacement);
                new_msg
            } else {
                msg.clone()
            }
        })
        .collect()
}

/// 异步版本：应用预算限制并持久化
///
/// 这是完整的第二层实现，包含实际的磁盘持久化操作。
pub async fn apply_budget_async(
    messages: &[blockcell_core::types::ChatMessage],
    candidates: &[ToolResultCandidate],
    state: &mut ContentReplacementState,
    budget: usize,
    workspace_dir: &std::path::Path,
    session_key: &str,
) -> Vec<blockcell_core::types::ChatMessage> {
    // 计算总大小
    let total_size: usize = candidates.iter().map(|c| c.size).sum();

    // 如果未超预算，直接返回原消息
    if total_size <= budget {
        return messages.to_vec();
    }

    // 需要持久化哪些结果？
    let mut sorted_candidates: Vec<_> = candidates.iter().collect();
    sorted_candidates.sort_by(|a, b| b.size.cmp(&a.size));

    let mut to_persist = std::collections::HashSet::new();
    let mut current_size = total_size;

    for candidate in &sorted_candidates {
        if current_size <= budget {
            break;
        }

        if state.is_seen(&candidate.tool_use_id) {
            continue;
        }

        to_persist.insert(candidate.tool_use_id.clone());
        current_size = current_size.saturating_sub(candidate.size);
    }

    if to_persist.is_empty() {
        return messages.to_vec();
    }

    // 持久化并构建替换映射
    let mut replacements: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for candidate in candidates {
        if !to_persist.contains(&candidate.tool_use_id) {
            continue;
        }

        match persist_tool_result(
            &candidate.content,
            &candidate.tool_use_id,
            session_key,
            workspace_dir,
        )
        .await
        {
            Ok(result) => {
                let message = build_large_tool_result_message(&result);
                replacements.insert(candidate.tool_use_id.clone(), message);
            }
            Err(e) => {
                // 磁盘持久化失败，生成内存 fallback 替换消息
                // 这样可以：1) 压缩历史内容 2) 告知用户持久化失败 3) 通过预览保留关键信息
                tracing::error!(
                    tool_use_id = %candidate.tool_use_id,
                    error = %e.error,
                    "Failed to persist tool result to disk, using memory fallback"
                );

                let fallback_message = build_memory_fallback_message(
                    &candidate.content,
                    &candidate.tool_use_id,
                );
                replacements.insert(candidate.tool_use_id.clone(), fallback_message);
            }
        }
    }

    // 应用替换
    messages
        .iter()
        .map(|msg| {
            if msg.role != "tool" {
                return msg.clone();
            }

            let tool_call_id = match &msg.tool_call_id {
                Some(id) => id,
                None => return msg.clone(),
            };

            if let Some(replacement) = replacements.get(tool_call_id) {
                state.mark_seen(tool_call_id.clone());
                state.set_replacement(tool_call_id.clone(), replacement.clone());

                let mut new_msg = msg.clone();
                new_msg.content = serde_json::Value::String(replacement.clone());
                new_msg
            } else {
                msg.clone()
            }
        })
        .collect()
}
