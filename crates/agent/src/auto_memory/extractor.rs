//! 鑷姩璁板繂鎻愬彇鍣?//!
//! 鍚庡彴鎻愬彇鍥涚绫诲瀷璁板繂鐨勬牳蹇冮€昏緫銆?
use super::cursor::{ExtractionCursor, ExtractionCursorManager};
use super::{
    ensure_memory_dir, get_memory_file_path, MemoryType, EXTRACTION_COOLDOWN_MESSAGES,
    MAX_MEMORY_FILE_TOKENS,
};
use crate::forked::{
    build_forked_tool_schemas, create_auto_mem_can_use_tool, run_forked_agent, CacheSafeParams,
    ForkedAgentParams,
};
use crate::memory_event;
use crate::session_metrics::{get_memory_extraction_circuit_breaker, get_memory_metrics};
use crate::unified_security_scanner::scan_learned_memory_content;
use blockcell_core::types::ChatMessage;
use blockcell_providers::ProviderPool;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

pub(crate) fn build_message_content_signature(messages: &[ChatMessage]) -> String {
    let mut content = String::new();
    for message in messages {
        content.push_str(&message.role);
        content.push('\n');
        if let Some(text) = message.content.as_str() {
            content.push_str(text);
        } else {
            content.push_str(&message.content.to_string());
        }
        content.push('\n');
    }
    content
}

/// 原子回滚写入: 先写入唯一临时文件, 再 rename 替换目标文件
///
/// 与 `tokio::fs::write` 不同，此函数使用原子写入模式，
/// 避免进程崩溃时留下半写文件导致数据丢失。
async fn atomic_rollback_write(path: &Path, content: &str) -> std::io::Result<()> {
    let pid = std::process::id();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let temp_path = path.with_extension(format!("rollback.{}.{}", pid, timestamp));

    tokio::fs::write(&temp_path, content).await?;
    tokio::fs::rename(&temp_path, path).await?;

    Ok(())
}

/// 提取结果
/// Auto Memory 配置（从 Layer5Config 派生）
#[derive(Debug, Clone)]
pub struct AutoMemoryConfig {
    /// 触发提取的最小消息数
    pub min_messages_for_extraction: usize,
    /// 提取冷却消息数
    pub extraction_cooldown_messages: usize,
    /// 记忆文件最大 token 数
    pub max_memory_file_tokens: usize,
    /// 鎻愬彇鏃堕棿鍐峰嵈闃堝€硷紙绉掞級
    pub extraction_time_cooldown_secs: u64,
    /// 鍐呭鍙樺寲闃堝€硷紙瀛楃鏁帮級
    pub content_change_threshold: usize,
}

impl Default for AutoMemoryConfig {
    fn default() -> Self {
        use super::cursor::{CONTENT_CHANGE_THRESHOLD, TIME_COOLDOWN_SECS};
        Self {
            min_messages_for_extraction: super::MIN_MESSAGES_FOR_EXTRACTION,
            extraction_cooldown_messages: super::EXTRACTION_COOLDOWN_MESSAGES,
            max_memory_file_tokens: super::MAX_MEMORY_FILE_TOKENS,
            extraction_time_cooldown_secs: TIME_COOLDOWN_SECS,
            content_change_threshold: CONTENT_CHANGE_THRESHOLD,
        }
    }
}

impl From<blockcell_core::config::Layer5Config> for AutoMemoryConfig {
    fn from(c: blockcell_core::config::Layer5Config) -> Self {
        Self {
            min_messages_for_extraction: c.min_messages_for_extraction,
            extraction_cooldown_messages: c.extraction_cooldown_messages,
            max_memory_file_tokens: c.max_memory_file_tokens,
            extraction_time_cooldown_secs: c.extraction_time_cooldown_secs,
            content_change_threshold: c.content_change_threshold,
        }
    }
}

/// 鎻愬彇缁撴灉
#[derive(Debug)]
pub struct ExtractionResult {
    /// 璁板繂绫诲瀷
    pub memory_type: MemoryType,
    /// 鎻愬彇鎴愬姛
    pub success: bool,
    /// 杈撳叆 tokens
    pub input_tokens: usize,
    /// 杈撳嚭 tokens
    pub output_tokens: usize,
    /// 閿欒淇℃伅
    pub error: Option<String>,
    /// 娓告爣淇濆瓨鏄惁澶辫触
    pub cursor_save_failed: bool,
}

/// 鎻愬彇鍙傛暟
///
/// 封装 `extract` 方法所需的所有参数，避免函数签名过长。
pub struct ExtractionParams {
    /// LLM Provider 池（必需）
    pub provider_pool: Arc<ProviderPool>,
    /// 璁板繂绫诲瀷
    pub memory_type: MemoryType,
    /// 绯荤粺鎻愮ず
    pub system_prompt: Arc<String>,
    /// 妯″瀷鍚嶇О
    pub model: String,
    /// 娑堟伅鍘嗗彶
    pub messages: Vec<ChatMessage>,
    /// 鏈€鍚庝竴鏉℃秷鎭殑 UUID
    pub last_message_uuid: Uuid,
    /// 褰撳墠娑堟伅鎬绘暟
    pub message_count: usize,
}

/// 自动记忆提取器
pub struct AutoMemoryExtractor {
    /// 閰嶇疆鐩綍
    config_dir: std::path::PathBuf,
    /// 游标管理器
    cursor_manager: ExtractionCursorManager,
    /// 閰嶇疆
    config: AutoMemoryConfig,
}

impl AutoMemoryExtractor {
    /// 创建提取器
    pub async fn new(config_dir: &Path) -> std::io::Result<Self> {
        Self::with_config(config_dir, AutoMemoryConfig::default()).await
    }

    /// 创建提取器（带可配置参数）
    pub async fn with_config(config_dir: &Path, config: AutoMemoryConfig) -> std::io::Result<Self> {
        let mut cursor_manager = ExtractionCursorManager::new(config_dir);
        cursor_manager.load().await?;

        Ok(Self {
            config_dir: config_dir.to_path_buf(),
            cursor_manager,
            config,
        })
    }

    /// 检查是否需要提取
    pub fn should_extract(
        &self,
        current_message_count: usize,
        current_content: &str,
    ) -> Vec<MemoryType> {
        if current_message_count < self.config.min_messages_for_extraction {
            return Vec::new();
        }

        self.cursor_manager
            .all_cursors()
            .iter()
            .filter(|c| {
                matches!(
                    c.should_extract_full(
                        current_message_count,
                        current_content,
                        self.config.extraction_cooldown_messages,
                        self.config.extraction_time_cooldown_secs,
                        true,
                        self.config.content_change_threshold,
                    ),
                    super::cursor::ExtractionDecision::Proceed
                )
            })
            .map(|c| c.memory_type)
            .collect()
    }

    /// 鎵ц鎻愬彇
    ///
    /// ## 鍙傛暟
    /// - `params`: 提取参数（参见 `ExtractionParams`）
    pub async fn extract(&mut self, params: ExtractionParams) -> ExtractionResult {
        let ExtractionParams {
            provider_pool,
            memory_type,
            system_prompt,
            model,
            messages,
            last_message_uuid,
            message_count,
        } = params;

        let memory_path = get_memory_file_path(&self.config_dir, memory_type);

        // 熔断器检查：如果熔断器打开，快速失败避免浪费资源
        let cb = get_memory_extraction_circuit_breaker();
        if !cb.allow() {
            return ExtractionResult {
                memory_type,
                success: false,
                input_tokens: 0,
                output_tokens: 0,
                error: Some("电路熔断器打开，记忆提取被阻止".to_string()),
                cursor_save_failed: false,
            };
        }

        let message_content_signature = build_message_content_signature(&messages);

        // 璇诲彇褰撳墠璁板繂鍐呭
        let current_content = tokio::fs::read_to_string(&memory_path)
            .await
            .unwrap_or_else(|_| memory_type.template().to_string());

        // 鏋勫缓鎻愬彇 prompt
        let extraction_prompt = build_extraction_prompt(
            memory_type,
            &current_content,
            self.config.max_memory_file_tokens,
            &memory_path,
        );

        // 鍒涘缓 CacheSafeParams
        let cache_safe_params = CacheSafeParams {
            system_prompt: system_prompt.clone(),
            model: model.clone(),
            fork_context_messages: messages.clone(),
            ..Default::default()
        };

        // 防御性确保记忆目录存在，避免权限门控因目录不存在而拒绝写入
        if let Err(e) = ensure_memory_dir(&self.config_dir).await {
            tracing::warn!(
                path = %self.config_dir.display(),
                error = %e,
                "[auto_memory] 确保记忆目录存在失败"
            );
        }
        let memory_dir = self.config_dir.join("memory");

        // 使用 Builder 模式构建 ForkedAgentParams（强制设置 provider_pool）
        let params = ForkedAgentParams::builder()
            .provider_pool(provider_pool)
            .prompt_messages(vec![ChatMessage::user(&extraction_prompt)])
            .cache_safe_params(cache_safe_params)
            .can_use_tool(create_auto_mem_can_use_tool(&memory_dir))
            // 将执行层也限制在记忆目录内，作为 can_use_tool 之外的第二道边界。
            .working_dir(memory_dir)
            .query_source("auto_memory")
            .fork_label(memory_type.name())
            .max_turns(1)
            .skip_transcript(true)
            .tool_schemas(build_forked_tool_schemas(&[
                "exec".to_string(),
                "shell".to_string(),
            ]))
            .build();

        // 如果 builder 失败（理论上不会，因为已经设置 provider_pool）
        let params = match params {
            Ok(p) => p,
            Err(e) => {
                cb.record_failure();
                get_memory_metrics().layer5.record_extraction_failure();
                return ExtractionResult {
                    memory_type,
                    success: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    error: Some(format!("Failed to build ForkedAgentParams: {}", e)),
                    cursor_save_failed: false,
                };
            }
        };

        // 杩愯 Forked Agent
        // 记录提取开始事件
        memory_event!(
            layer5,
            extraction_started,
            memory_type.name(),
            memory_path.display().to_string()
        );

        let result = run_forked_agent(params).await;

        match result {
            Ok(forked_result) => {
                // 工具调用失败（权限拒绝、old_string not found 等）时不应视为提取成功
                if forked_result.had_tool_error {
                    tracing::warn!(
                        memory_type = memory_type.name(),
                        "[auto_memory] forked agent had tool errors, skipping extraction"
                    );
                    // 回滚到提取前的内容，避免部分写入的损坏文件残留
                    if let Err(rollback_err) =
                        atomic_rollback_write(&memory_path, &current_content).await
                    {
                        tracing::error!(
                            memory_type = memory_type.name(),
                            error = %rollback_err,
                            "[auto_memory] 工具错误后回滚记忆文件失败！数据可能已损坏"
                        );
                    }
                    cb.record_failure();
                    get_memory_metrics().layer5.record_extraction_failure();
                    return ExtractionResult {
                        memory_type,
                        success: false,
                        input_tokens: forked_result.total_usage.input_tokens as usize,
                        output_tokens: forked_result.total_usage.output_tokens as usize,
                        error: Some("Forked agent had tool execution errors".to_string()),
                        cursor_save_failed: false,
                    };
                }
                // 安全扫描: 检查写入后的记忆文件内容
                if let Ok(updated_content) = tokio::fs::read_to_string(&memory_path).await {
                    if let Err(err) = scan_learned_memory_content(&updated_content) {
                        tracing::warn!(
                            memory_type = memory_type.name(),
                            error = %err,
                            "[auto_memory] 瀹夊叏鎵弿鍙戠幇濞佽儊, 鍥炴粴璁板繂鏂囦欢"
                        );
                        // 回滚到提取前的内容（使用原子写入避免崩溃导致数据丢失）
                        if let Err(rollback_err) =
                            atomic_rollback_write(&memory_path, &current_content).await
                        {
                            tracing::error!(
                                memory_type = memory_type.name(),
                                error = %rollback_err,
                                "[auto_memory] 回滚记忆文件失败！数据可能已损坏"
                            );
                        }
                        cb.record_failure();
                        get_memory_metrics().layer5.record_extraction_failure();
                        return ExtractionResult {
                            memory_type,
                            success: false,
                            input_tokens: forked_result.total_usage.input_tokens as usize,
                            output_tokens: forked_result.total_usage.output_tokens as usize,
                            error: Some(format!(
                                "瀹夊叏鎵弿澶辫触, 璁板繂鏂囦欢宸插洖婊? {}",
                                err
                            )),
                            cursor_save_failed: false,
                        };
                    }
                }

                // 注意：内容变更检测使用写入前后的文件内容快照进行比较。
                // 同一记忆类型的并发提取受冷却机制（cooldown）保护，
                // 因此 TOCTOU 竞态在实际运行中不会发生。
                // 内容变更检测：重新读取文件确认内容已变更
                // "无变化"是正常情况（LLM 判断无需修改），不应视为失败。
                // 仍需推进游标以避免重复触发，不计入熔断。
                if let Ok(new_content) = tokio::fs::read_to_string(&memory_path).await {
                    if new_content == current_content {
                        tracing::info!(
                            memory_type = memory_type.name(),
                            "[auto_memory] 提取未产生内容变更（正常 no-op）"
                        );
                        // 正常推进游标，防止下次继续触发
                        let cursor = {
                            let mut c = self.cursor_manager.get_cursor(memory_type);
                            c.update_with_content(
                                last_message_uuid,
                                message_count,
                                &message_content_signature,
                            );
                            c
                        };
                        let cursor_save_failed = match self
                            .cursor_manager
                            .merge_and_save(cursor)
                            .await
                        {
                            Err(e) => {
                                tracing::error!(error = %e, "[auto_memory] failed to save cursor (no-change path)");
                                true
                            }
                            Ok(()) => false,
                        };
                        // cursor 保存失败时记入熔断器失败，避免存储层故障被掩盖
                        if cursor_save_failed {
                            cb.record_failure();
                            get_memory_metrics().layer5.record_extraction_failure();
                        } else {
                            cb.record_success();
                        }
                        return ExtractionResult {
                            memory_type,
                            success: true, // 无变化视为成功
                            input_tokens: forked_result.total_usage.input_tokens as usize,
                            output_tokens: forked_result.total_usage.output_tokens as usize,
                            error: None,
                            cursor_save_failed,
                        };
                    }
                }

                // 更新游标
                let cursor = {
                    let mut c = self.cursor_manager.get_cursor(memory_type);
                    c.update_with_content(
                        last_message_uuid,
                        message_count,
                        &message_content_signature,
                    );
                    c
                };

                // 记录游标更新事件
                memory_event!(
                    layer5,
                    cursor_updated,
                    memory_type.name(),
                    message_count as u64
                );

                // 合并磁盘上的最新状态并保存游标（防止并发覆盖）
                let cursor_save_failed = match self.cursor_manager.merge_and_save(cursor).await {
                    Err(e) => {
                        tracing::error!(error = %e, "[auto_memory] failed to save cursor");
                        true
                    }
                    Ok(()) => false,
                };

                tracing::info!(
                    memory_type = memory_type.name(),
                    input_tokens = forked_result.total_usage.input_tokens,
                    output_tokens = forked_result.total_usage.output_tokens,
                    cursor_save_failed,
                    "[auto_memory] extraction completed"
                );

                // 璁板綍 Layer 5 memory_written 浜嬩欢
                // 读取写入后的文件内容来获取长度
                if let Ok(updated_content) = tokio::fs::read_to_string(&memory_path).await {
                    memory_event!(
                        layer5,
                        memory_written,
                        memory_type.name(),
                        memory_path.to_string_lossy().as_ref(),
                        updated_content.len()
                    );
                }

                // 熔断器记录：cursor 保存失败时记入失败，避免存储层故障被掩盖
                if cursor_save_failed {
                    cb.record_failure();
                    get_memory_metrics().layer5.record_extraction_failure();
                } else {
                    cb.record_success();
                }

                ExtractionResult {
                    memory_type,
                    success: true,
                    input_tokens: forked_result.total_usage.input_tokens as usize,
                    output_tokens: forked_result.total_usage.output_tokens as usize,
                    error: None,
                    cursor_save_failed,
                }
            }
            Err(e) => {
                // 熔断器记录失败
                cb.record_failure();
                get_memory_metrics().layer5.record_extraction_failure();

                tracing::error!(
                    memory_type = memory_type.name(),
                    error = %e,
                    "[auto_memory] extraction failed"
                );

                ExtractionResult {
                    memory_type,
                    success: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    error: Some(e.to_string()),
                    cursor_save_failed: false,
                }
            }
        }
    }
}

/// 鏀舵暃瑙勫垯 + Memory/Skill 杈圭晫瑙勫垯 (鍙傝€?Hermes MEMORY_GUIDANCE)
const EXTRACTION_ENHANCEMENT: &str = r#"
## Convergence Rules
- If nothing has changed since the last extraction, do NOT modify the file 鈥?just stop.
- Do not duplicate information already present in the file.
- Do not add trivial or obvious information.

## Memory vs Skill Boundary
- Memory stores declarative facts: preferences, environment, conventions, user background.
- Skill stores procedural knowledge: steps, workflows, pitfalls, how-to guides.
- "User prefers concise responses" 鈫?memory (save here)
- "Deploy to K8s requires pushing image first" 鈫?skill-domain procedural knowledge (do not save here)
- This extractor is memory-only: do not create, patch, or request skills, and do not write procedural workflows into memory.
"#;

/// 鏋勫缓鎻愬彇 prompt
fn build_extraction_prompt(
    memory_type: MemoryType,
    current_content: &str,
    max_memory_file_tokens: usize,
    memory_path: &Path,
) -> String {
    format!(
        r#"Update the {} memory file.

## Current Memory Content:
{}

## Memory Type Purpose:
{}

## Instructions:
- Review the recent conversation and extract relevant information for this memory type
- DO NOT modify the YAML frontmatter
- DO NOT modify section headers
- ONLY update content below the section headers
- Keep the total content under {} tokens
- Preserve all existing important information
- Add new information that matches this memory type's purpose
{}
Use the edit_file tool to update the memory file at: {}.
"#,
        memory_type.name(),
        current_content,
        memory_type.usage_guide(),
        max_memory_file_tokens,
        EXTRACTION_ENHANCEMENT,
        memory_path.display(),
    )
}

/// 检查是否应该提取自动记忆（使用配置参数）
pub fn should_extract_auto_memory_with_config(
    cursor_manager: &ExtractionCursorManager,
    current_message_count: usize,
    current_content: &str,
    config: &AutoMemoryConfig,
) -> Vec<MemoryType> {
    if current_message_count < config.min_messages_for_extraction {
        return Vec::new();
    }

    cursor_manager
        .all_cursors()
        .iter()
        .filter(|c| {
            matches!(
                c.should_extract_full(
                    current_message_count,
                    current_content,
                    config.extraction_cooldown_messages,
                    config.extraction_time_cooldown_secs,
                    true,
                    config.content_change_threshold,
                ),
                super::cursor::ExtractionDecision::Proceed
            )
        })
        .map(|c| c.memory_type)
        .collect()
}

/// 执行单次自动记忆提取
///
/// ## 参数
/// - `provider_pool`: LLM Provider 池（必需）
/// - `config_dir`: 配置目录
/// - `memory_type`: 记忆类型
/// - `system_prompt`: 系统提示
/// - `model`: 模型名称
/// - `messages`: 消息历史
/// - `cursor`: 用于验证提取条件的游标
///
/// ## Deprecated
/// This standalone function uses hardcoded constants instead of runtime `Layer5Config` values.
/// Use `AutoMemoryExtractor::extract()` instead, which respects the full config.
#[deprecated(
    since = "0.1.5",
    note = "Use `AutoMemoryExtractor::extract()` instead; this function ignores runtime Layer5Config"
)]
#[allow(dead_code)]
pub async fn extract_auto_memory(
    provider_pool: Arc<ProviderPool>,
    config_dir: &Path,
    memory_type: MemoryType,
    system_prompt: Arc<String>,
    model: &str,
    messages: Vec<ChatMessage>,
    cursor: ExtractionCursor, // 用于验证提取条件
) -> ExtractionResult {
    // 使用 cursor 验证是否满足提取条件
    let current_message_count = messages.len();
    let message_content_signature = build_message_content_signature(&messages);
    if !matches!(
        cursor.should_extract_full(
            current_message_count,
            &message_content_signature,
            EXTRACTION_COOLDOWN_MESSAGES,
            AutoMemoryConfig::default().extraction_time_cooldown_secs,
            true,
            AutoMemoryConfig::default().content_change_threshold,
        ),
        super::cursor::ExtractionDecision::Proceed
    ) {
        tracing::debug!(
            memory_type = memory_type.name(),
            last_count = cursor.last_message_count,
            current_count = current_message_count,
            "[auto_memory] extraction skipped - cooldown/content conditions not met"
        );
        return ExtractionResult {
            memory_type,
            success: false,
            input_tokens: 0,
            output_tokens: 0,
            error: Some("Extraction cooldown/content conditions not met".to_string()),
            cursor_save_failed: false,
        };
    }

    let memory_path = get_memory_file_path(config_dir, memory_type);

    // 熔断器检查：如果熔断器打开，快速失败避免浪费资源
    let cb = get_memory_extraction_circuit_breaker();
    if !cb.allow() {
        return ExtractionResult {
            memory_type,
            success: false,
            input_tokens: 0,
            output_tokens: 0,
            error: Some("电路熔断器打开，记忆提取被阻止".to_string()),
            cursor_save_failed: false,
        };
    }

    // 读取当前内容
    let current_content = tokio::fs::read_to_string(&memory_path)
        .await
        .unwrap_or_else(|_| memory_type.template().to_string());

    // 构建 prompt
    let extraction_prompt = build_extraction_prompt(
        memory_type,
        &current_content,
        MAX_MEMORY_FILE_TOKENS,
        &memory_path,
    );

    // 创建 CacheSafeParams
    let cache_safe_params = CacheSafeParams {
        system_prompt,
        model: model.to_string(),
        fork_context_messages: messages,
        ..Default::default()
    };

    // 使用 Builder 模式构建 ForkedAgentParams
    // 这确保必需参数（provider_pool）在编译时被验证
    if let Err(e) = ensure_memory_dir(config_dir).await {
        tracing::warn!(
            path = %config_dir.display(),
            error = %e,
            "[auto_memory] 确保记忆目录存在失败"
        );
    }
    let memory_dir = config_dir.join("memory");
    let params = match ForkedAgentParams::builder()
        .provider_pool(provider_pool)
        .prompt_messages(vec![ChatMessage::user(&extraction_prompt)])
        .cache_safe_params(cache_safe_params)
        .can_use_tool(create_auto_mem_can_use_tool(&memory_dir))
        // 将执行层也限制在记忆目录内，作为 can_use_tool 之外的第二道边界。
        .working_dir(memory_dir)
        .query_source("auto_memory")
        .fork_label(memory_type.name())
        .max_turns(1)
        .skip_transcript(true)
        .tool_schemas(build_forked_tool_schemas(&[
            "exec".to_string(),
            "shell".to_string(),
        ]))
        .build()
    {
        Ok(p) => p,
        Err(e) => {
            return ExtractionResult {
                memory_type,
                success: false,
                input_tokens: 0,
                output_tokens: 0,
                error: Some(e.to_string()),
                cursor_save_failed: false,
            };
        }
    };

    // 运行 Forked Agent
    let result = run_forked_agent(params).await;

    match result {
        Ok(forked_result) => {
            // 工具调用失败时不应视为提取成功
            if forked_result.had_tool_error {
                tracing::warn!(
                    memory_type = memory_type.name(),
                    "[auto_memory] deprecated path: forked agent had tool errors, skipping extraction"
                );
                // 回滚到提取前的内容，避免部分写入的损坏文件残留
                if let Err(rollback_err) =
                    atomic_rollback_write(&memory_path, &current_content).await
                {
                    tracing::error!(
                        memory_type = memory_type.name(),
                        error = %rollback_err,
                        "[auto_memory] deprecated 路径工具错误后回滚记忆文件失败！数据可能已损坏"
                    );
                }
                cb.record_failure();
                get_memory_metrics().layer5.record_extraction_failure();
                return ExtractionResult {
                    memory_type,
                    success: false,
                    input_tokens: forked_result.total_usage.input_tokens as usize,
                    output_tokens: forked_result.total_usage.output_tokens as usize,
                    error: Some("Forked agent had tool execution errors".to_string()),
                    cursor_save_failed: false,
                };
            }
            // 安全扫描: 检查写入后的记忆文件内容
            if let Ok(updated_content) = tokio::fs::read_to_string(&memory_path).await {
                if let Err(err) = scan_learned_memory_content(&updated_content) {
                    tracing::warn!(
                        memory_type = memory_type.name(),
                        error = %err,
                        "[auto_memory] security scan found threats, rolling back memory file"
                    );
                    if let Err(rollback_err) =
                        atomic_rollback_write(&memory_path, &current_content).await
                    {
                        tracing::error!(
                            memory_type = memory_type.name(),
                            error = %rollback_err,
                            "[auto_memory] memory file rollback failed; data may be corrupted"
                        );
                    }
                    // deprecated 路径仍需记录熔断失败，保持与 extract() 一致
                    cb.record_failure();
                    get_memory_metrics().layer5.record_extraction_failure();
                    return ExtractionResult {
                        memory_type,
                        success: false,
                        input_tokens: forked_result.total_usage.input_tokens as usize,
                        output_tokens: forked_result.total_usage.output_tokens as usize,
                        error: Some(format!(
                            "Security scan failed; memory file rolled back: {}",
                            err
                        )),
                        cursor_save_failed: false,
                    };
                }
            }

            tracing::info!(
                memory_type = memory_type.name(),
                input_tokens = forked_result.total_usage.input_tokens,
                output_tokens = forked_result.total_usage.output_tokens,
                "[auto_memory] extraction completed"
            );

            // 熔断器记录成功
            cb.record_success();

            ExtractionResult {
                memory_type,
                success: true,
                input_tokens: forked_result.total_usage.input_tokens as usize,
                output_tokens: forked_result.total_usage.output_tokens as usize,
                error: None,
                cursor_save_failed: false,
            }
        }
        Err(e) => {
            // 熔断器记录失败
            cb.record_failure();
            get_memory_metrics().layer5.record_extraction_failure();

            ExtractionResult {
                memory_type,
                success: false,
                input_tokens: 0,
                output_tokens: 0,
                error: Some(e.to_string()),
                cursor_save_failed: false,
            }
        }
    }
}
/// 检查是否应该提取自动记忆（使用默认常量）
pub fn should_extract_auto_memory(
    cursor_manager: &ExtractionCursorManager,
    current_message_count: usize,
    current_content: &str,
) -> Vec<MemoryType> {
    should_extract_auto_memory_with_config(
        cursor_manager,
        current_message_count,
        current_content,
        &AutoMemoryConfig::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_extraction_prompt() {
        let prompt = build_extraction_prompt(
            MemoryType::User,
            "current content",
            4000,
            std::path::Path::new("/config/memory/user.md"),
        );
        assert!(prompt.contains("user memory file"));
        assert!(prompt.contains("current content"));
        assert!(prompt.contains("4000 tokens"));
        assert!(prompt.contains("/config/memory/user.md"));
    }

    #[test]
    fn test_should_extract_auto_memory() {
        let manager = ExtractionCursorManager::new(Path::new("/config"));
        let content = "user\nremember that I prefer concise answers\n";

        // 消息数不足
        let types = should_extract_auto_memory(&manager, 5, content);
        assert!(types.is_empty());

        // 消息数足够
        let types = should_extract_auto_memory(&manager, 15, content);
        assert!(!types.is_empty());
        assert!(types.contains(&MemoryType::User));
    }

    #[test]
    fn test_should_extract_auto_memory_respects_content_change() {
        let mut manager = ExtractionCursorManager::new(Path::new("/config"));
        let content = "user\nremember that I prefer concise answers\n";
        let mut cursor = ExtractionCursor::new(MemoryType::User);
        cursor.update_with_content(Uuid::new_v4(), 10, content);
        manager.update_cursor(cursor);
        let config = AutoMemoryConfig {
            min_messages_for_extraction: 0,
            extraction_cooldown_messages: 5,
            max_memory_file_tokens: 4000,
            extraction_time_cooldown_secs: 0,
            content_change_threshold: 10,
        };

        let unchanged = should_extract_auto_memory_with_config(&manager, 20, content, &config);
        assert!(!unchanged.contains(&MemoryType::User));

        let changed = format!("{content}{}", "x".repeat(20));
        let changed_types = should_extract_auto_memory_with_config(&manager, 20, &changed, &config);
        assert!(changed_types.contains(&MemoryType::User));
    }

    #[test]
    fn test_should_extract_auto_memory_respects_time_cooldown() {
        let mut manager = ExtractionCursorManager::new(Path::new("/config"));
        let content = "user\nremember that I prefer concise answers\n";
        let mut cursor = ExtractionCursor::new(MemoryType::User);
        cursor.update_with_content(Uuid::new_v4(), 10, content);
        manager.update_cursor(cursor);
        let config = AutoMemoryConfig {
            min_messages_for_extraction: 0,
            extraction_cooldown_messages: 5,
            max_memory_file_tokens: 4000,
            extraction_time_cooldown_secs: 3600,
            content_change_threshold: 0,
        };

        let changed = format!("{content}{}", "x".repeat(20));
        let types = should_extract_auto_memory_with_config(&manager, 20, &changed, &config);
        assert!(!types.contains(&MemoryType::User));
    }
}
