use blockcell_tools::ResponseCacheOps;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::debug;

// Import memory_event macro for Layer 1 metrics
use crate::memory_event;

/// 默认可缓存最小字符数（低于此数不缓存）
const DEFAULT_CACHEABLE_MIN_CHARS: usize = 800;

mod util;
use util::{format_bytes, format_chars};
pub use util::{generate_preview, sanitize_session_key, sanitize_tool_use_id};

/// ResponseCache 配置参数
///
/// 封装 Layer1Config 中的缓存相关字段，用于替代硬编码常量。
/// 当从 Layer1Config 构造时，使用用户配置值；当使用 Default 时，回退到硬编码常量。
#[derive(Debug, Clone)]
pub struct ResponseCacheConfig {
    /// 单个工具结果的最大字符数（超过此值触发持久化）
    pub max_result_size_chars: usize,
    /// 每会话最大缓存条目数
    pub cache_max_per_session: usize,
    /// 可缓存最小字符数（低于此数不缓存）
    pub cacheable_min_chars: usize,
    /// 预览大小（以字符为单位）
    pub preview_size_chars: usize,
    /// 消息级别工具结果上限（字符数）
    pub max_tool_results_per_message_chars: usize,
    /// 内容替换最大条目数
    pub max_replacement_entries: usize,
}

impl Default for ResponseCacheConfig {
    fn default() -> Self {
        Self {
            max_result_size_chars: DEFAULT_MAX_RESULT_SIZE_CHARS,
            cache_max_per_session: default_l1_cache_max(),
            cacheable_min_chars: DEFAULT_CACHEABLE_MIN_CHARS,
            preview_size_chars: default_l1_preview_size(),
            max_tool_results_per_message_chars: default_l1_max_per_message(),
            max_replacement_entries: default_l1_max_replacement(),
        }
    }
}

/// Default value functions matching Layer1Config defaults
fn default_l1_cache_max() -> usize {
    10
}
fn default_l1_preview_size() -> usize {
    2_000
}
fn default_l1_max_per_message() -> usize {
    150_000
}
fn default_l1_max_replacement() -> usize {
    1_000
}

impl From<&blockcell_core::config::Layer1Config> for ResponseCacheConfig {
    fn from(c: &blockcell_core::config::Layer1Config) -> Self {
        Self {
            max_result_size_chars: c.max_result_size_chars,
            cache_max_per_session: c.cache_max_per_session,
            cacheable_min_chars: c.cacheable_min_chars,
            preview_size_chars: c.preview_size_chars,
            max_tool_results_per_message_chars: c.max_tool_results_per_message_chars,
            max_replacement_entries: c.max_replacement_entries,
        }
    }
}

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
    /// Configuration parameters (from Layer1Config)
    config: ResponseCacheConfig,
}

struct CacheEntry {
    content: String,
    #[allow(dead_code)]
    item_count: usize,
    created_at: i64,
}

impl ResponseCache {
    pub fn new() -> Self {
        Self::with_config(ResponseCacheConfig::default())
    }

    /// Create ResponseCache with configurable parameters from Layer1Config
    pub fn with_config(config: ResponseCacheConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ResponseCacheInner {
                data: HashMap::new(),
                config,
            })),
        }
    }

    /// 获取配置的最大工具结果字符数阈值（超过此值触发持久化）
    pub fn max_result_size_chars(&self) -> usize {
        self.get_lock().config.max_result_size_chars
    }

    /// 获取配置的预览大小（以字符为单位），用于持久化后的截断预览
    pub fn preview_size_chars(&self) -> usize {
        self.get_lock().config.preview_size_chars
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
    ///
    /// ## `has_tool_results` guard
    /// Only applies caching when the conversation contains actual tool results.
    /// Without this guard, the LLM can hallucinate a numbered list from empty tool
    /// results (e.g. `memory_query` returning `[]`) and the stub would replace the
    /// hallucinated content with an unreadable cache reference.
    pub fn maybe_cache_and_stub(
        &self,
        session_key: &str,
        content: &str,
        has_tool_results: bool,
    ) -> Option<String> {
        // Skip caching if there were no tool results in this turn —
        // the LLM may have hallucinated a list from empty/missing data.
        if !has_tool_results {
            return None;
        }

        // Acquire lock once and check cacheability inside the lock to avoid
        // TOCTOU between is_cacheable() and the cache insertion below.
        // Previously, is_cacheable() acquired its own lock, then the insertion
        // acquired a second lock — this was a double-lock pattern that could
        // observe inconsistent config between the two calls.
        let mut inner = self.get_lock();
        // 容量为 0 明确表示禁用会话响应缓存，不能生成无法保存的引用。
        if inner.config.cache_max_per_session == 0 {
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

        // Check cacheability inside the lock (min_chars from config)
        let min_chars = inner.config.cacheable_min_chars;
        if content.chars().count() <= min_chars {
            return None;
        }

        let max_per_session = inner.config.cache_max_per_session;
        let session_cache = inner.data.entry(session_key.to_string()).or_default();

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

        let ts = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_else(|| {
            tracing::warn!(
                "[response_cache] timestamp_nanos_opt returned None (timestamp out of range), using 0 as fallback ref_id"
            );
            0
        });
        let mut hasher = DefaultHasher::new();
        session_key.hash(&mut hasher);
        ts.hash(&mut hasher);
        let h = hasher.finish();
        // 16 lowercase hex chars (full u64)
        format!("{:016x}", h)
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

/// 预览大小（字符数）— 仅用作 Default 回退值，运行时使用 Layer1Config.preview_size_chars
pub const PREVIEW_SIZE_CHARS: usize = 2000;

/// 默认最大结果大小 (~50KB) — 仅用作 Default 回退值，运行时使用 Layer1Config.max_result_size_chars
pub const DEFAULT_MAX_RESULT_SIZE_CHARS: usize = 50_000;

/// 消息级别上限 (~150KB) — 仅用作 Default 回退值，运行时使用 Layer1Config.max_tool_results_per_message_chars
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
    /// 原始内容大小（字节数，content.len()）
    pub original_size_bytes: usize,
    /// 是否为 JSON 格式（数组内容）
    pub is_json: bool,
    /// 预览内容
    pub preview: String,
    /// 是否有更多内容被截断
    pub has_more: bool,
    /// 可召回引用 ID，格式为 `tool:{sanitized_tool_use_id}`
    /// session_recall 工具通过此 ID 恢复完整输出
    pub tool_ref: String,
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
#[derive(Debug, Clone)]
pub struct ContentReplacementState {
    /// 已处理的 tool_use_id 集合
    pub seen_ids: HashSet<String>,
    /// id -> 替换内容映射
    pub replacements: HashMap<String, String>,
    /// 插入顺序（用于 LRU 淘汰）
    insertion_order: Vec<String>,
    /// 最大条目数限制（来自 Layer1Config）
    max_replacement_entries: usize,
}

/// 最大条目数限制 — 仅用作 Default 回退值，运行时使用 Layer1Config.max_replacement_entries
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

impl Default for ContentReplacementState {
    fn default() -> Self {
        Self {
            seen_ids: HashSet::new(),
            replacements: HashMap::new(),
            insertion_order: Vec::new(),
            max_replacement_entries: MAX_REPLACEMENT_ENTRIES,
        }
    }
}

impl ContentReplacementState {
    /// 创建新的状态
    pub fn new() -> Self {
        Self::default()
    }

    /// 创建指定最大条目数的 ContentReplacementState
    pub fn with_max_entries(max_replacement_entries: usize) -> Self {
        Self {
            seen_ids: HashSet::new(),
            replacements: HashMap::new(),
            insertion_order: Vec::new(),
            max_replacement_entries,
        }
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
    /// 使用 drain 而非 remove(0) 避免 O(n) 复制开销
    fn prune_if_needed(&mut self) {
        while self.insertion_order.len() > self.max_replacement_entries {
            if let Some(oldest_id) = self.insertion_order.first().cloned() {
                self.seen_ids.remove(&oldest_id);
                self.replacements.remove(&oldest_id);
                // drain(0..1) removes the first element in O(1) amortized
                self.insertion_order.drain(0..1);
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
            max_replacement_entries: self.max_replacement_entries,
        }
    }

    /// 清空状态
    pub fn clear(&mut self) {
        self.seen_ids.clear();
        self.replacements.clear();
        self.insertion_order.clear();
    }

    /// 从 transcript 记录重建状态
    ///
    /// 使用 `max_replacement_entries` 参数确保重建后的状态保留
    /// 用户配置的条目上限，而非静默回退到默认值。
    pub fn reconstruct(
        tool_use_ids: &[String],
        records: &[ContentReplacementRecord],
        inherited_replacements: Option<&HashMap<String, String>>,
        max_replacement_entries: usize,
    ) -> Self {
        let mut state = Self::with_max_entries(max_replacement_entries);

        // 收集所有候选 tool_use_id
        for tool_use_id in tool_use_ids {
            state.seen_ids.insert(tool_use_id.clone());
            state.insertion_order.push(tool_use_id.clone());
        }

        // 从 records 恢复 replacements
        for record in records {
            // 将 record 的 tool_use_id 加入 seen_ids，维持 "一旦决定，永不更改" 不变量
            state.seen_ids.insert(record.tool_use_id.clone());
            state
                .replacements
                .insert(record.tool_use_id.clone(), record.replacement.clone());
            // 确保顺序跟踪
            if !state.insertion_order.contains(&record.tool_use_id) {
                state.insertion_order.push(record.tool_use_id.clone());
            }
        }

        // 从继承的 replacements 填充空缺
        if let Some(inherited) = inherited_replacements {
            for (id, replacement) in inherited {
                // 将继承的 ID 也加入 seen_ids
                state.seen_ids.insert(id.clone());
                if !state.replacements.contains_key(id) {
                    state.replacements.insert(id.clone(), replacement.clone());
                    if !state.insertion_order.contains(id) {
                        state.insertion_order.push(id.clone());
                    }
                }
            }
        }

        // 强制执行 max_replacement_entries 限制
        state.prune_if_needed();

        state
    }
}

/// 持久化输出标签
pub const PERSISTED_OUTPUT_TAG: &str = "<persisted-output>";
pub const PERSISTED_OUTPUT_CLOSING_TAG: &str = "</persisted-output>";

/// 内存保留标签（当磁盘持久化失败时使用）
pub const MEMORY_FALLBACK_TAG: &str = "<memory-fallback>";
pub const MEMORY_FALLBACK_CLOSING_TAG: &str = "</memory-fallback>";

/// 磁盘持久化失败时的警告消息
pub const DISK_PERSIST_FAILED_WARNING: &str =
    "Warning: Disk persistence failed. Content preserved in memory preview.";

/// 构建内存 fallback 替换消息（当磁盘持久化失败时）
///
/// 包含预览内容，告知用户磁盘持久化失败但数据已通过预览保留。
fn build_memory_fallback_message(
    content: &str,
    tool_use_id: &str,
    preview_size_chars: usize,
) -> String {
    // 清理 tool_use_id 以防止换行符注入到日志/显示中
    let safe_tool_use_id = sanitize_tool_use_id(tool_use_id);

    let (preview, has_more) = generate_preview(content, preview_size_chars);

    let mut message = format!(
        "{}\n{}\n\nTool ID: {}\nPreview (first {}):\n{}",
        MEMORY_FALLBACK_TAG,
        DISK_PERSIST_FAILED_WARNING,
        safe_tool_use_id,
        format_chars(preview_size_chars),
        preview
    );
    if has_more {
        message.push_str("\n... (content truncated due to disk error)");
    }
    message.push('\n');
    message.push_str(MEMORY_FALLBACK_CLOSING_TAG);
    message
}

/// 构建大结果消息（含可召回 tool: 引用）
///
/// 生成的 stub 包含 `tool:{id}` 格式的引用，用户可通过
/// `session_recall(id="tool:{id}")` 恢复完整输出。
pub fn build_large_tool_result_message(
    result: &PersistedToolResult,
    preview_size_chars: usize,
) -> String {
    let mut message = format!(
        "{}\nOutput too large ({}). Full output saved to: {}\n\
         Tool ID: {}\n\
         Recall with: session_recall(id=\"{}\")\n\n",
        PERSISTED_OUTPUT_TAG,
        format_bytes(result.original_size_bytes),
        result.filepath.display(),
        result.tool_ref,
        result.tool_ref,
    );
    message.push_str(&format!(
        "Preview (first {}):\n{}",
        format_chars(preview_size_chars),
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
/// 统一写入 `.tool_results/` 目录，与 `try_persist_large_tool_result` 使用相同的
/// 路径格式，确保 `session_recall` 工具可通过 `tool:{id}` 恢复完整输出。
/// 每次调用生成唯一 UUID 后缀，防止重复 tool_use_id 导致文件覆盖。
pub async fn persist_tool_result(
    content: &str,
    tool_use_id: &str,
    session_key: &str,
    workspace_dir: &std::path::Path,
    preview_size_chars: usize,
) -> Result<PersistedToolResult, PersistToolResultError> {
    // 清理 tool_use_id 防止路径注入
    let safe_tool_use_id = sanitize_tool_use_id(tool_use_id);

    // 使用 sanitize_session_key 替代 sanitize_tool_use_id，防止不同会话映射到同一目录
    // （sanitize_tool_use_id 会删除分隔符，导致 "a.b" 和 "a-b" 冲突）
    let safe_session_key = sanitize_session_key(session_key);

    // 生成唯一后缀，防止重复 tool_use_id 导致文件覆盖
    let call_uuid = uuid::Uuid::new_v4().simple();
    let dir_name = format!("{}_{}", safe_tool_use_id, call_uuid);

    // 统一使用 .tool_results/ 路径，与 runtime try_persist_large_tool_result 一致
    // session_recall 通过 tool:{id} 格式在 .tool_results/{session_id}/{tool_id}_* 下搜索
    let persistence_dir = workspace_dir
        .join(".tool_results")
        .join(&safe_session_key)
        .join(&dir_name);

    // 验证目录路径仍在工作目录内（防止路径遍历攻击）
    let dir_canonical =
        match std::fs::canonicalize(persistence_dir.parent().unwrap_or(&persistence_dir)) {
            Ok(p) => p,
            Err(_) => persistence_dir.clone(), // 目录不存在时使用原始路径
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
    if let Err(e) = tokio::fs::create_dir_all(&persistence_dir).await {
        return Err(PersistToolResultError {
            error: format!("Failed to create directory: {}", e),
        });
    }

    let output_file = persistence_dir.join("output.txt");

    // 验证最终文件路径仍在预期目录内
    if !output_file.starts_with(&persistence_dir) {
        return Err(PersistToolResultError {
            error: "Path traversal detected: file escapes target directory".to_string(),
        });
    }

    // 写入文件（UUID 后缀保证唯一性，无需 create_new）
    if let Err(e) = tokio::fs::write(&output_file, content).await {
        return Err(PersistToolResultError {
            error: format!("Failed to write file: {}", e),
        });
    }

    let (preview, has_more) = generate_preview(content, preview_size_chars);

    // 构建可召回引用 ID，包含 UUID 后缀用于精确定位目录
    // 新格式 tool:{tool_id}:{call_uuid} — session_recall 优先按精确目录名匹配；
    // 旧格式 tool:{tool_id} 仅作为回退（prefix latest），防止 tool_id 复用导致读错输出
    let tool_ref = format!("tool:{}:{}", safe_tool_use_id, call_uuid);

    Ok(PersistedToolResult {
        filepath: output_file,
        original_size_bytes: content.len(),
        is_json: content.trim_start().starts_with('['),
        preview,
        has_more,
        tool_ref,
    })
}

/// 清理 `.tool_results/` 目录中的过期条目。
///
/// ## 清理策略
/// - **TTL 过期**：移除修改时间超过 `max_age_days` 的条目（默认 7 天）
/// - **每会话上限**：每个会话目录最多保留 `max_entries_per_session` 个条目（默认 50），
///   超出部分按修改时间最早的优先删除
/// - **空目录清理**：清理后为空的会话目录一并删除
///
/// ## 参数
/// - `workspace_dir`: 工作区根目录
/// - `max_age_days`: 条目最大保留天数
/// - `max_entries_per_session`: 每个会话目录最大条目数
///
/// ## 返回值
/// `(removed_entries, removed_dirs)` — 删除的条目目录数和会话目录数
pub async fn cleanup_tool_results(
    workspace_dir: &std::path::Path,
    max_age_days: i64,
    max_entries_per_session: usize,
) -> (usize, usize) {
    let tool_results_dir = workspace_dir.join(".tool_results");
    if !tool_results_dir.exists() {
        return (0, 0);
    }

    let now = std::time::SystemTime::now();
    let cutoff = now - std::time::Duration::from_secs((max_age_days * 86400) as u64);

    let mut removed_entries: usize = 0;
    let mut removed_dirs: usize = 0;

    // 遍历每个会话目录
    let mut session_dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&tool_results_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.path().is_dir() {
                session_dirs.push(entry.path());
            }
        }
    }

    for session_dir in &session_dirs {
        // 收集该会话下的所有条目目录及其修改时间
        let mut entries: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
        if let Ok(mut dir_entries) = tokio::fs::read_dir(session_dir).await {
            while let Ok(Some(entry)) = dir_entries.next_entry().await {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                // 检查是否有 output.txt
                if !path.join("output.txt").exists() {
                    continue;
                }
                if let Ok(meta) = entry.metadata().await {
                    if let Ok(modified) = meta.modified() {
                        entries.push((path, modified));
                        continue;
                    }
                }
                // 无法获取元数据时用 epoch 作为保守估计
                entries.push((path, std::time::UNIX_EPOCH));
            }
        }

        if entries.is_empty() {
            continue;
        }

        // 按修改时间降序排列（最新的在前）
        entries.sort_by_key(|b| std::cmp::Reverse(b.1));

        // 阶段 1：TTL 过期清理
        let mut kept: Vec<&(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
        for entry in &entries {
            if entry.1 < cutoff {
                // 过期：删除
                if let Err(e) = tokio::fs::remove_dir_all(&entry.0).await {
                    tracing::warn!(
                        path = %entry.0.display(),
                        error = %e,
                        "[tool_results cleanup] 删除过期条目目录失败"
                    );
                } else {
                    removed_entries += 1;
                }
            } else {
                kept.push(entry);
            }
        }

        // 阶段 2：每会话上限清理（保留最新的 max_entries_per_session 个）
        if kept.len() > max_entries_per_session {
            for entry in kept.iter().skip(max_entries_per_session) {
                if let Err(e) = tokio::fs::remove_dir_all(&entry.0).await {
                    tracing::warn!(
                        path = %entry.0.display(),
                        error = %e,
                        "[tool_results cleanup] 删除超限条目目录失败"
                    );
                } else {
                    removed_entries += 1;
                }
            }
        }

        // 阶段 3：如果会话目录为空，删除它
        if let Ok(mut remaining) = tokio::fs::read_dir(session_dir).await {
            if remaining.next_entry().await.ok().flatten().is_none() {
                if let Err(e) = tokio::fs::remove_dir(session_dir).await {
                    tracing::warn!(
                        dir = %session_dir.display(),
                        error = %e,
                        "[tool_results cleanup] 删除空会话目录失败"
                    );
                } else {
                    removed_dirs += 1;
                }
            }
        }
    }

    if removed_entries > 0 || removed_dirs > 0 {
        tracing::info!(
            removed_entries,
            removed_dirs,
            max_age_days,
            max_entries_per_session,
            "[tool_results cleanup] 清理完成"
        );
    }

    (removed_entries, removed_dirs)
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod layer1_tests {
    use super::*;

    #[test]
    fn zero_capacity_disables_response_cache() {
        let cache = ResponseCache::with_config(ResponseCacheConfig {
            cache_max_per_session: 0,
            cacheable_min_chars: 0,
            ..ResponseCacheConfig::default()
        });
        let content = (1..=6)
            .map(|index| format!("{}. 第 {} 条缓存测试内容", index, index))
            .collect::<Vec<_>>()
            .join("\n");

        let stub = cache.maybe_cache_and_stub("ws:test", &content, true);

        assert!(stub.is_none());
        assert!(!cache.get_lock().data.contains_key("ws:test"));
    }

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
        // 按字符数截断：预览字符数不超过 20
        assert!(preview.chars().count() <= 20);
        assert!(has_more);
        // 应在换行符处截断
        assert!(preview.ends_with('\n') || preview.chars().count() < 20);
    }

    #[test]
    fn test_content_replacement_state() {
        let mut state = ContentReplacementState::default();
        state.seen_ids.insert("tool-1".to_string());
        state
            .replacements
            .insert("tool-1".to_string(), "replacement".to_string());

        let cloned = state.clone_state();
        assert!(cloned.seen_ids.contains("tool-1"));
        assert_eq!(
            cloned.replacements.get("tool-1"),
            Some(&"replacement".to_string())
        );
    }

    #[test]
    fn test_build_large_tool_result_message() {
        let result = PersistedToolResult {
            filepath: PathBuf::from("/path/to/file.json"),
            original_size_bytes: 100_000,
            is_json: true,
            preview: "preview content".to_string(),
            has_more: true,
            tool_ref: "tool:call_abc123:a1b2c3d4e5f6a7b8".to_string(),
        };

        let message = build_large_tool_result_message(&result, PREVIEW_SIZE_CHARS);
        assert!(message.starts_with(PERSISTED_OUTPUT_TAG));
        assert!(message.ends_with(PERSISTED_OUTPUT_CLOSING_TAG));
        assert!(message.contains("97.7 KB"));
        assert!(message.contains("preview content"));
        // 验证 stub 包含可召回的 tool: 引用（含 UUID 后缀用于精确定位）
        assert!(message.contains("Tool ID: tool:call_abc123:a1b2c3d4e5f6a7b8"));
        assert!(message.contains("session_recall(id=\"tool:call_abc123:a1b2c3d4e5f6a7b8\")"));
    }

    #[test]
    fn test_format_chars() {
        assert_eq!(format_chars(500), "500 chars");
        assert_eq!(format_chars(1024), "1.0K chars");
        assert_eq!(format_chars(1024 * 1024), "1.0M chars");
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
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
                "test-session", // session_key
                &state,
                DEFAULT_MAX_RESULT_SIZE_CHARS,
                workspace,
                PREVIEW_SIZE_CHARS,
            )
            .await;

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
        let records = vec![ContentReplacementRecord {
            kind: "persist".to_string(),
            tool_use_id: "tool-1".to_string(),
            replacement: "replacement-1".to_string(),
        }];
        let inherited = Some(&HashMap::from([(
            "tool-3".to_string(),
            "inherited".to_string(),
        )]));

        let state = ContentReplacementState::reconstruct(
            &tool_ids,
            &records,
            inherited,
            MAX_REPLACEMENT_ENTRIES,
        );

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

        let result = apply_budget(&messages, &candidates, &mut state, budget, 2000);

        // 应该触发替换
        assert!(state.is_seen("call-1") || state.is_seen("call-2"));
        // 消息数量保持一致
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_generate_preview_utf8_boundary() {
        // 测试按字符数截断：每个中文字符 3 字节
        let content = "你好世界".repeat(1000); // 多字节字符，共 4000 字符 / 12000 字节
        let (preview, has_more) = generate_preview(&content, 100);

        // 预览字符数不超过 100（字节长度可能 > 100，因为每字符 3 字节）
        assert!(preview.chars().count() <= 100);
        assert!(has_more);
        // 确保没有 panic 且字符串有效
        assert!(preview.is_char_boundary(preview.len()));
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

    #[test]
    fn test_sanitize_session_key_uniqueness() {
        // 不同 session_key 不应映射到同一目录名
        let a = sanitize_session_key("wechat:user@domain");
        let b = sanitize_session_key("wechat:user#domain");
        let c = sanitize_session_key("wechat-user-domain");

        // 三者必须不同（虽然前缀可能相似，但哈希后缀保证唯一性）
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);

        // 同一个 session_key 产生确定性结果
        assert_eq!(sanitize_session_key("test"), sanitize_session_key("test"));
    }

    #[test]
    fn test_sanitize_session_key_format() {
        // 正常 session_key 包含可读前缀 + SHA-256 哈希后缀
        let key = sanitize_session_key("cli:test-session");
        assert!(key.starts_with("clitest-session_"));
        // 哈希后缀为 16 位十六进制（SHA-256 前 64 位）
        let parts: Vec<&str> = key.rsplitn(2, '_').collect();
        assert_eq!(parts[0].len(), 16);
        assert!(parts[0].chars().all(|c| c.is_ascii_hexdigit()));

        // 空 session_key 使用默认前缀
        let empty = sanitize_session_key("");
        assert!(empty.starts_with("session_"));
    }

    #[test]
    fn test_sanitize_session_key_special_chars() {
        // 特殊字符被过滤，但哈希保证唯一性
        let key = sanitize_session_key("a.b/c\\d:e@f");
        // 不包含路径分隔符
        assert!(!key.contains('/'));
        assert!(!key.contains('\\'));
        // 点号、冒号、@ 被过滤
        let prefix_part = key.rsplit_once('_').unwrap().0;
        // 前缀仍然以 '_' 结尾（因为过滤后可能存在 trailing _）
        // 关键是不包含危险字符
        assert!(!prefix_part.contains('.'));
        assert!(!prefix_part.contains('@'));
        assert!(!prefix_part.contains(':'));
    }

    #[tokio::test]
    async fn test_cleanup_tool_results_empty_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tool_results = tmp.path().join(".tool_results");
        // 空目录或无目录都应返回 (0, 0)
        let (entries, dirs) = cleanup_tool_results(tmp.path(), 7, 50).await;
        assert_eq!(entries, 0);
        assert_eq!(dirs, 0);
        let _ = tool_results; // 未创建时也无错误
    }

    #[tokio::test]
    async fn test_cleanup_tool_results_removes_old_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_dir = tmp
            .path()
            .join(".tool_results")
            .join("test_session_abc12345");
        let entry_dir = session_dir.join("tool_call_old_entry");
        tokio::fs::create_dir_all(&entry_dir).await.unwrap();
        tokio::fs::write(entry_dir.join("output.txt"), "old content")
            .await
            .unwrap();

        // 使用 TTL=0 天（立即清理所有条目）
        let (entries, _) = cleanup_tool_results(tmp.path(), 0, 50).await;
        assert!(entries >= 1, "应删除过期条目，实际删除: {entries}");

        // 条目目录应已删除
        assert!(!entry_dir.exists(), "过期条目目录应已删除");
    }

    #[tokio::test]
    async fn test_cleanup_tool_results_respects_max_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_dir = tmp
            .path()
            .join(".tool_results")
            .join("test_session_def12345");
        // 创建 5 个条目
        for i in 0..5 {
            let entry_dir = session_dir.join(format!("tool_call_{i}_uuid{i}"));
            tokio::fs::create_dir_all(&entry_dir).await.unwrap();
            tokio::fs::write(entry_dir.join("output.txt"), format!("content {i}"))
                .await
                .unwrap();
        }
        // 限制为 2 个条目，TTL=365 天（不过期）
        let (removed, _) = cleanup_tool_results(tmp.path(), 365, 2).await;
        assert_eq!(removed, 3, "应删除超限的 3 个条目");

        // 验证只剩下 2 个条目
        let mut count = 0;
        if let Ok(mut entries) = tokio::fs::read_dir(&session_dir).await {
            while let Ok(Some(_)) = entries.next_entry().await {
                count += 1;
            }
        }
        assert_eq!(count, 2, "应保留 2 个条目");
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
    preview_size_chars: usize,
) -> Option<String> {
    // 检查是否已经处理过
    if state.is_seen(tool_use_id) {
        // 返回之前的决定
        return state.get_replacement(tool_use_id).map(|s| {
            memory_event!(layer1, replacement_frozen, tool_use_id, s.len());
            s.to_string()
        });
    }

    // 检查内容大小
    if content.len() <= threshold {
        return None;
    }

    // 需要持久化
    match persist_tool_result(
        content,
        tool_use_id,
        session_key,
        workspace_dir,
        preview_size_chars,
    )
    .await
    {
        Ok(result) => {
            memory_event!(
                layer1,
                preview_generated,
                tool_use_id,
                result.original_size_bytes
            );
            let message = build_large_tool_result_message(&result, preview_size_chars);
            Some(message)
        }
        Err(e) => {
            // 磁盘持久化失败，使用内存 fallback
            tracing::error!(
                tool_use_id = %tool_use_id,
                error = %e.error,
                "[process_tool_result] Failed to persist, using memory fallback"
            );
            let fallback_message =
                build_memory_fallback_message(content, tool_use_id, preview_size_chars);
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
/// - `preview_size_chars`: 预览字符数限制
pub fn apply_budget(
    messages: &[blockcell_core::types::ChatMessage],
    candidates: &[ToolResultCandidate],
    state: &mut ContentReplacementState,
    budget: usize,
    preview_size_chars: usize,
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
    sorted_candidates.sort_by_key(|b| std::cmp::Reverse(b.size));

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

                // 同步路径无法执行磁盘持久化，使用 memory-fallback 标签
                // 而非 persisted-output，避免产生可通过 session_recall 召回的假象。
                // 如需真正的磁盘持久化，应使用 apply_budget_async。
                let content_preview = match &msg.content {
                    serde_json::Value::String(s) => {
                        let (preview, _has_more) = generate_preview(s, preview_size_chars);
                        preview
                    }
                    _ => String::new(),
                };
                let replacement = format!(
                    "{}\nOutput too large for inline display. Not persisted to disk; preview only.\n\nPreview (first {}):\n{}\n\n{}",
                    MEMORY_FALLBACK_TAG,
                    format_chars(preview_size_chars),
                    content_preview,
                    MEMORY_FALLBACK_CLOSING_TAG
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
    preview_size_chars: usize,
) -> Vec<blockcell_core::types::ChatMessage> {
    // 计算总大小
    let total_size: usize = candidates.iter().map(|c| c.size).sum();

    // 如果未超预算，直接返回原消息
    if total_size <= budget {
        return messages.to_vec();
    }

    // 预算超限，记录 Layer 1 事件
    memory_event!(
        layer1,
        budget_exceeded,
        total_size,
        budget,
        candidates.len()
    );

    // 需要持久化哪些结果？
    let mut sorted_candidates: Vec<_> = candidates.iter().collect();
    sorted_candidates.sort_by_key(|b| std::cmp::Reverse(b.size));

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
    let mut replacements: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for candidate in candidates {
        if !to_persist.contains(&candidate.tool_use_id) {
            continue;
        }

        match persist_tool_result(
            &candidate.content,
            &candidate.tool_use_id,
            session_key,
            workspace_dir,
            preview_size_chars,
        )
        .await
        {
            Ok(result) => {
                // 记录 Layer 1 持久化事件
                memory_event!(
                    layer1,
                    persisted,
                    &candidate.tool_use_id,
                    result.original_size_bytes,
                    result.preview.len()
                );
                // 更新当前存储计数
                crate::session_metrics::get_memory_metrics()
                    .layer1
                    .increment_stored_count();
                let message = build_large_tool_result_message(&result, preview_size_chars);
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
                    preview_size_chars,
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
