use super::*;
use blockcell_tools::shell_executor::{LocalShellExecutor, ShellExecutor, ShellRequest};

pub(crate) fn validate_path_safety(path: &str) -> Result<(), ForkedAgentError> {
    // 检查空字节注入
    if path.contains('\0') {
        return Err(ForkedAgentError::ToolError(
            "Invalid path: contains null byte".to_string(),
        ));
    }

    // 检查路径遍历：先词法归一化，再检查是否逃逸到父目录。
    // 原始路径中的 ".." 在归一化后可能合法（如 "src/../lib" → "lib"），
    // 仅当归一化后的路径仍以 ".." 开头时才拒绝（表示逃逸到工作目录之外）。
    let normalized = normalize_path_lexically(Path::new(path));
    if normalized
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(ForkedAgentError::ToolError(
            "Path traversal detected: path escapes working directory".to_string(),
        ));
    }

    Ok(())
}

pub(crate) fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(std::path::Component::ParentDir.as_os_str());
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// 将路径规范化为"最近存在祖先的 canonical 路径 + 剩余后缀"。
///
/// 当路径不存在时，逐步向上查找存在的祖先，对其进行 canonicalize，
/// 再拼接被剥离的后缀分量。确保在 Windows 下 `\\?\` 前缀一致性。
///
/// 安全性：如果某个祖先目录存在但 canonicalize 失败（权限不足、坏 symlink/junction），
/// 直接返回错误，避免回退到词法路径绕过隔离校验。
pub(crate) fn resolve_to_existing_ancestor(path: &Path) -> Result<PathBuf, ForkedAgentError> {
    match std::fs::canonicalize(path) {
        Ok(real) => Ok(real),
        Err(_) => {
            let mut existing_ancestor = path.to_path_buf();
            let mut suffix = PathBuf::new();
            loop {
                if existing_ancestor.as_os_str().is_empty() {
                    // 路径完全不存在且无任何已存在祖先，使用词法规范化
                    // （此时无法构成逃逸风险，因为路径本身不在磁盘上）
                    break Ok(normalize_path_lexically(path));
                }
                if existing_ancestor.exists() {
                    match std::fs::canonicalize(&existing_ancestor) {
                        Ok(real_ancestor) => {
                            break Ok(real_ancestor.join(suffix));
                        }
                        Err(e) => {
                            // 已存在祖先目录但 canonicalize 失败（权限/symlink/junction）
                            // 出于安全考虑直接拒绝，不回退到词法路径
                            break Err(ForkedAgentError::ToolError(format!(
                                "无法解析路径 '{}': \
                                 已存在祖先 '{}' 的 canonicalize 失败: {}。\
                                 可能是权限不足或符号链接损坏",
                                path.display(),
                                existing_ancestor.display(),
                                e
                            )));
                        }
                    }
                }
                if let Some(parent) = existing_ancestor.parent() {
                    if let Some(file_name) = existing_ancestor.file_name() {
                        suffix = if suffix.as_os_str().is_empty() {
                            PathBuf::from(file_name)
                        } else {
                            PathBuf::from(file_name).join(suffix)
                        };
                    }
                    existing_ancestor = parent.to_path_buf();
                } else {
                    // 已到达文件系统根目录且不存在，使用词法规范化
                    break Ok(normalize_path_lexically(path));
                }
            }
        }
    }
}

pub(crate) fn resolve_forked_path(
    input_path: &str,
    working_dir: &Option<PathBuf>,
) -> Result<PathBuf, ForkedAgentError> {
    validate_path_safety(input_path)?;

    let path = Path::new(input_path);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else if let Some(base) = working_dir {
        base.join(path)
    } else {
        path.to_path_buf()
    };

    if let Some(base) = working_dir {
        // 对 base 和 resolved 使用同一个规范化策略，
        // 避免 Windows 下 canonical/non-canonical 前缀混比（\\?\ 前缀问题）
        let base = resolve_to_existing_ancestor(base)?;
        let resolved = resolve_to_existing_ancestor(&resolved)?;

        if !resolved.starts_with(&base) {
            return Err(ForkedAgentError::ToolError(format!(
                "Path '{}' is outside isolated working directory '{}'",
                input_path,
                base.display()
            )));
        }
        return Ok(resolved);
    }

    Ok(resolved)
}

/// 验证编辑内容安全性
///
/// 检查：
/// 1. new_string 大小限制
/// 2. 内容增长比例限制
pub(crate) fn validate_edit_content(
    original: &str,
    new_string: &str,
    max_new_size: usize,
) -> Result<(), ForkedAgentError> {
    // 检查 new_string 大小
    if new_string.len() > max_new_size {
        return Err(ForkedAgentError::ToolError(format!(
            "new_string too large: {} bytes (max {})",
            new_string.len(),
            max_new_size
        )));
    }

    // 检查内容增长比例（防止爆炸性增长）
    let original_len = original.len().max(1);
    let growth_ratio = new_string.len() as f64 / original_len as f64;
    if growth_ratio > 100.0 {
        return Err(ForkedAgentError::ToolError(format!(
            "Content growth ratio too high: {:.1}x (max 100x)",
            growth_ratio
        )));
    }

    Ok(())
}

pub(crate) fn is_dangerous_shell_command(command: &str) -> bool {
    DANGEROUS_COMMAND_PATTERNS
        .iter()
        .any(|pattern| pattern.is_match(command))
}

pub(crate) fn truncate_output(content: String, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content;
    }

    let mut boundary = max_chars;
    while boundary > 0 && !content.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!(
        "{}...\n[Truncated, total {} chars]",
        &content[..boundary],
        content.len()
    )
}

pub(crate) async fn execute_shell_command(
    command: &str,
    command_working_dir: Option<PathBuf>,
    workspace_scope: &[WorkspaceScopeEntry],
) -> Result<String, ForkedAgentError> {
    if is_dangerous_shell_command(command) {
        return Err(ForkedAgentError::ToolError(
            "Command matches dangerous pattern and is blocked".to_string(),
        ));
    }

    let writable_roots = if workspace_scope.is_empty() {
        command_working_dir.iter().cloned().collect::<Vec<_>>()
    } else {
        workspace_scope
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>()
    };
    let identity = blockcell_core::current_agent_context();
    let output = LocalShellExecutor::default()
        .execute(ShellRequest {
            agent_id: identity
                .as_ref()
                .map(|identity| identity.agent_id.clone())
                .unwrap_or_default(),
            session_id: String::new(),
            command: command.to_string(),
            working_dir: command_working_dir
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default(),
            policy: blockcell_tools::sandbox::SandboxPolicy::WorkspaceWrite,
            writable_roots,
            timeout: Duration::from_secs(60),
            max_output_bytes: 40_000,
        })
        .await
        .map_err(|error| ForkedAgentError::ToolError(error.to_string()))?;

    let stdout = truncate_output(String::from_utf8_lossy(&output.stdout).to_string(), 10_000);
    let stderr = truncate_output(String::from_utf8_lossy(&output.stderr).to_string(), 10_000);

    // 命令非零退出码视为工具失败，防止失败的命令被误记为提取成功
    if !output.success() {
        return Err(ForkedAgentError::ToolError(format!(
            "Command exited with non-zero status (exit_code: {})\nstdout: {}\nstderr: {}",
            output
                .exit_code
                .map_or("N/A".to_string(), |c| c.to_string()),
            stdout,
            stderr,
        )));
    }

    Ok(json!({
        "exit_code": output.exit_code,
        "stdout": stdout,
        "stderr": stderr,
    })
    .to_string())
}

fn ensure_write_in_workspace_scope(
    path: &Path,
    workspace_scope: &[WorkspaceScopeEntry],
) -> Result<(), ForkedAgentError> {
    if workspace_scope.is_empty() || workspace_scope.iter().any(|entry| entry.allows(path)) {
        return Ok(());
    }
    Err(ForkedAgentError::ToolError(format!(
        "Write path '{}' is outside the assigned workspace_scope",
        path.display()
    )))
}

/// 在 skills_dir + external_skills_dirs 中查找 Skill 目录 (与主工具 find_skill_dir 对齐)
///
/// 搜索顺序: skills_dir/{name} → skills_dir/{category}/{name} → 各 external_dir 同理
pub(crate) fn find_skill_dir_forked(
    name: &str,
    category: Option<&str>,
    skills_dir: &Path,
    external_dirs: &[PathBuf],
) -> Option<PathBuf> {
    // 构建搜索目录列表 (主目录优先)
    let mut search_dirs: Vec<PathBuf> = vec![skills_dir.to_path_buf()];
    for dir in external_dirs {
        if dir != skills_dir && dir.exists() {
            search_dirs.push(dir.clone());
        }
    }

    for dir in &search_dirs {
        // 如果指定了 category, 先尝试 {dir}/{category}/{name}
        if let Some(cat) = category {
            let candidate = dir.join(cat).join(name);
            if candidate.is_dir() && candidate.join("SKILL.md").exists() {
                return Some(candidate);
            }
        }

        // 尝试直接匹配 {dir}/{name}
        let direct = dir.join(name);
        if direct.is_dir() && direct.join("SKILL.md").exists() {
            return Some(direct);
        }

        // 遍历 category 子目录查找
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let candidate = path.join(name);
                    if candidate.is_dir() && candidate.join("SKILL.md").exists() {
                        return Some(candidate);
                    }
                }
            }
        }
    }

    None
}

/// Forked Agent 支持的工具：
/// - read_file: 读取文件内容
/// - list_dir: 列出目录内容
/// - file_edit / edit_file: 编辑文件（字符串替换）
/// - file_write / write_file: 写入文件
/// - exec: 执行 shell 命令
/// - grep: 在文件中搜索模式（简化版）
/// - glob: 匹配文件模式（简化版，支持基本通配符）
/// - skill_manage: 技能管理（create/edit/patch/view/delete/write_file/remove_file）
/// - memory_upsert: 写入/更新记忆项（需要 memory_store）
/// - memory_query: 查询记忆项（需要 memory_store）
/// - memory_forget: 删除记忆项（需要 memory_store）
///
/// 其他工具会返回错误。
#[allow(deprecated, clippy::too_many_arguments)]
pub(crate) async fn execute_forked_tool(
    tool_name: &str,
    input: &serde_json::Value,
    can_use_tool: &CanUseToolFn,
    disallowed_tools: &[String],
    memory_store: &Option<MemoryStoreHandle>,
    memory_file_store: &Option<MemoryFileStoreHandle>,
    skill_file_store: &Option<SkillFileStoreHandle>,
    skills_dir: &Option<PathBuf>,
    external_skills_dirs: &[PathBuf],
    skill_mutex: &Option<SkillMutexHandle>,
    working_dir: &Option<PathBuf>,
    workspace_scope: &[WorkspaceScopeEntry],
) -> Result<String, ForkedAgentError> {
    // Check disallowed tools list
    if disallowed_tools.iter().any(|d| d == tool_name) {
        return Ok(format!(
            "Tool '{}' is not allowed in this agent. Disallowed tools: {}",
            tool_name,
            disallowed_tools.join(", ")
        ));
    }

    // 辅助函数: 解析文件路径 (相对于 working_dir，用于 worktree 隔离)
    // 首先检查权限
    match can_use_tool(tool_name, input) {
        ToolPermission::Allow => {}
        ToolPermission::Deny { message } => {
            // 记录 Layer 7 tool_denied 事件
            crate::memory_event!(layer7, tool_denied, tool_name, &message);
            // 权限拒绝视为工具失败（而非 Ok），避免上层 tool_result.is_ok() 误判为成功，
            // 进而防止 memory extraction 跳过失败记录、推进游标。
            return Err(ForkedAgentError::ToolError(format!(
                "Tool '{}' denied: {}",
                tool_name, message
            )));
        }
    }

    // SkillMutex 检查: 写入操作前获取互斥锁
    // 注意: _skill_guard 必须在整个 match 块中存活, 才能保护操作期间不被并发修改
    //
    // 重要: 当 skill_file_store 可用时, SkillFileStore 内部已有 WriteGuard 保护
    // (create/edit/patch/delete/write_file/remove_file 都会调用 acquire_write_guard),
    // 不需要在此预获取 skill_mutex, 否则同一 WriteTarget 被重复获取会导致自我冲突
    let _skill_guard = if tool_name == "skill_manage" {
        // SkillFileStore 路径: 内部已自带 write guard, 跳过预获取避免自我冲突
        if skill_file_store.is_some() {
            None
        } else {
            let is_write_action = matches!(
                input.get("action").and_then(|v| v.as_str()).unwrap_or(""),
                "create" | "patch" | "edit" | "delete" | "write_file" | "remove_file"
            );
            if is_write_action {
                if let Some(name) = input.get("name").and_then(|v| v.as_str()) {
                    if let Some(ref mutex) = skill_mutex {
                        // 直接获取写锁（acquire 内部已包含活跃检查）
                        // 不再先调用 can_modify() 再 acquire()，避免 TOCTOU 竞态
                        match mutex.try_acquire(name) {
                            Some(guard) => Some(guard),
                            None => {
                                tracing::warn!(skill = %name, "SkillMutex acquire failed (skill is active), rejecting write");
                                return Ok(json!({
                                    "success": false,
                                    "message": format!("Skill '{}' is currently being modified. Please try again later.", name)
                                }).to_string());
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
    } else {
        None
    };

    match tool_name {
        "read_file" => {
            let file_path = input.get("file_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ForkedAgentError::ToolError("Missing file_path parameter".to_string()))?;

            let resolved = resolve_forked_path(file_path, working_dir)?;

            // 检查文件大小
            let metadata = match tokio::fs::metadata(&resolved).await {
                Ok(metadata) => metadata,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(format!(
                        "File not found: {}. Use list_dir or glob to inspect existing files before reading.",
                        file_path
                    ));
                }
                Err(e) => return Err(ForkedAgentError::Io(e)),
            };
            if metadata.len() as usize > MAX_FILE_SIZE {
                return Err(ForkedAgentError::ToolError(format!(
                    "File too large: {} bytes (max {})",
                    metadata.len(), MAX_FILE_SIZE
                )));
            }

            let content = tokio::fs::read_to_string(&resolved)
                .await
                .map_err(ForkedAgentError::Io)?;

            // 截断过长的输出（安全处理 UTF-8 边界）
            let truncated = if content.len() > MAX_OUTPUT_CHARS {
                // 找到安全的 UTF-8 边界
                let mut boundary = MAX_OUTPUT_CHARS;
                while boundary > 0 && !content.is_char_boundary(boundary) {
                    boundary -= 1;
                }
                format!("{}...\n[Truncated, total {} chars]",
                    &content[..boundary], content.len())
            } else {
                content
            };

            Ok(truncated)
        },

        "list_dir" => {
            let dir_path = input.get("path")
                .or_else(|| input.get("dir_path"))
                .and_then(|v| v.as_str())
                .unwrap_or(".");

            let base_path = resolve_forked_path(dir_path, working_dir)?;
            let mut entries = Vec::new();

            match tokio::fs::read_dir(&base_path).await {
                Ok(mut dir_entries) => {
                    while let Ok(Some(entry)) = dir_entries.next_entry().await {
                        let file_name = entry.file_name().to_string_lossy().to_string();
                        let metadata = entry.metadata().await;
                        let type_indicator = match &metadata {
                            Ok(m) if m.is_dir() => "/",
                            _ => "",
                        };
                        entries.push(format!("{}{}", file_name, type_indicator));
                        if entries.len() >= 500 {
                            entries.push("... [truncated, max 500 entries]".to_string());
                            break;
                        }
                    }
                }
                Err(e) => {
                    return Err(ForkedAgentError::Io(e));
                }
            }

            if entries.is_empty() {
                Ok(format!("Empty directory: {}", dir_path))
            } else {
                Ok(entries.join("\n"))
            }
        },

        "exec" | "shell" => {
            let command = input
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ForkedAgentError::ToolError("Missing command parameter".to_string())
                })?;

            let command_working_dir = input
                .get("working_dir")
                .and_then(|v| v.as_str())
                .map(|dir| resolve_forked_path(dir, working_dir))
                .transpose()?
                .or_else(|| working_dir.clone());

            execute_shell_command(command, command_working_dir, workspace_scope).await
        },

        "file_edit" | "edit_file" => {
            let file_path = input.get("file_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ForkedAgentError::ToolError("Missing file_path parameter".to_string()))?;

            let old_string = input.get("old_string")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ForkedAgentError::ToolError("Missing old_string parameter".to_string()))?;

            let new_string = input.get("new_string")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ForkedAgentError::ToolError("Missing new_string parameter".to_string()))?;

            // 验证编辑内容安全性
            validate_edit_content(old_string, new_string, MAX_EDIT_SIZE)?;

            let resolved = resolve_forked_path(file_path, working_dir)?;
            ensure_write_in_workspace_scope(&resolved, workspace_scope)?;

            // 读取文件
            let content = tokio::fs::read_to_string(&resolved)
                .await
                .map_err(ForkedAgentError::Io)?;

            // 执行替换（默认只替换第一个匹配，与主 edit_file 工具一致）
            let new_content = if content.contains(old_string) {
                let replace_all = input.get("replace_all")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if replace_all {
                    content.replace(old_string, new_string)
                } else {
                    // 仅替换第一个匹配
                    match content.find(old_string) {
                        Some(pos) => {
                            let mut result = String::with_capacity(content.len() - old_string.len() + new_string.len());
                            result.push_str(&content[..pos]);
                            result.push_str(new_string);
                            result.push_str(&content[pos + old_string.len()..]);
                            result
                        }
                        None => content.clone(),
                    }
                }
            } else {
                // old_string 未找到视为工具失败，避免上层误判为编辑成功
                return Err(ForkedAgentError::ToolError(format!("old_string not found in {}", file_path)));
            };

            // 原子写回文件 (temp file + rename, 防止崩溃时损坏)
            atomic_write_text(&resolved, &new_content)
                .await
                .map_err(|e| ForkedAgentError::ToolError(format!("Failed to write file: {}", e)))?;

            Ok(format!("Successfully edited {}", file_path))
        },

        "file_write" | "write_file" => {
            let file_path = input.get("file_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ForkedAgentError::ToolError("Missing file_path parameter".to_string()))?;

            let content = input.get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ForkedAgentError::ToolError("Missing content parameter".to_string()))?;

            // 检查内容大小
            if content.len() > MAX_FILE_SIZE {
                return Err(ForkedAgentError::ToolError(format!(
                    "Content too large: {} bytes (max {})",
                    content.len(), MAX_FILE_SIZE
                )));
            }

            let resolved = resolve_forked_path(file_path, working_dir)?;
            ensure_write_in_workspace_scope(&resolved, workspace_scope)?;

            // 确保父目录存在（create_dir_all 会处理已存在的情况）
            if let Some(parent) = resolved.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(ForkedAgentError::Io)?;
            }

            // 原子写入文件 (temp file + rename, 防止崩溃时损坏)
            atomic_write_text(&resolved, content)
                .await
                .map_err(|e| ForkedAgentError::ToolError(format!(
                    "Failed to write file '{}': {}",
                    resolved.display(),
                    e
                )))?;

            Ok(format!("Successfully wrote {}", file_path))
        },

        "grep" => {
            let pattern = input.get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ForkedAgentError::ToolError("Missing pattern parameter".to_string()))?;

            let path = input.get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".");

            let resolved = resolve_forked_path(path, working_dir)?;

            // 简化版 grep - 只搜索单个文件
            let content = tokio::fs::read_to_string(&resolved)
                .await
                .map_err(ForkedAgentError::Io)?;

            let matches: Vec<&str> = content
                .lines()
                .filter(|line| line.contains(pattern))
                .take(100)  // 限制结果数量
                .collect();

            if matches.is_empty() {
                Ok(format!("No matches found for pattern '{}'", pattern))
            } else {
                Ok(matches.join("\n"))
            }
        },

        "glob" => {
            let pattern = input.get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ForkedAgentError::ToolError("Missing pattern parameter".to_string()))?;

            let path = input.get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".");

            // 简化版 glob - 只支持基本模式
            let base_path = resolve_forked_path(path, working_dir)?;
            let mut results = Vec::new();

            // 使用 tokio 异步读取目录
            match tokio::fs::read_dir(&base_path).await {
                Ok(mut entries) => {
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        let file_name = entry.file_name().to_string_lossy().to_string();
                        // 简单的通配符匹配
                        if simple_glob_match(pattern, &file_name) {
                            results.push(entry.path().to_string_lossy().to_string());
                        }
                        if results.len() >= 100 {
                            break;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, path = %base_path.display(), "[forked] Failed to read directory");
                }
            }

            if results.is_empty() {
                Ok(format!("No files matching '{}'", pattern))
            } else {
                Ok(results.join("\n"))
            }
        },

        // 记忆工具: memory_upsert
        "memory_manage" => {
            match memory_file_store {
                Some(store) => {
                    let action = input
                        .get("action")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let target = input
                        .get("target")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let scope = input
                        .get("scope")
                        .and_then(|v| v.as_str())
                        .unwrap_or("session");
                    if !matches!(scope, "session" | "workspace" | "user")
                        || (scope == "workspace" && target != "memory")
                        || (scope == "user" && target != "user")
                    {
                        return Err(ForkedAgentError::ToolError(
                            "memory_manage scope/target combination is invalid".to_string(),
                        ));
                    }
                    let result = match action {
                        "add" => store.add_scoped_file_memory_json(
                            scope,
                            target,
                            input.get("content").and_then(|v| v.as_str()).unwrap_or(""),
                        ),
                        "replace" => store.replace_scoped_file_memory_json(
                            scope,
                            target,
                            input.get("old_text").and_then(|v| v.as_str()).unwrap_or(""),
                            input.get("content").and_then(|v| v.as_str()).unwrap_or(""),
                        ),
                        "remove" => store.remove_scoped_file_memory_json(
                            scope,
                            target,
                            input.get("old_text").and_then(|v| v.as_str()).unwrap_or(""),
                        ),
                        "undo_latest" => {
                            store.restore_latest_scoped_file_memory_json(scope, target)
                        }
                        _ => Err(blockcell_core::Error::Validation(
                            "memory_manage action must be add, replace, remove, or undo_latest"
                                .to_string(),
                        )),
                    }
                    .map_err(|e| {
                        ForkedAgentError::ToolError(format!("memory_manage error: {}", e))
                    })?;
                    Ok(serde_json::to_string(&result)
                        .unwrap_or_else(|_| "memory_manage completed".to_string()))
                }
                None => Ok("Memory file store not available".to_string()),
            }
        },

        "memory_upsert" => {
            match memory_store {
                Some(store) => {
                    let result = store.upsert_json(input.clone())
                        .map_err(|e| ForkedAgentError::ToolError(format!("memory_upsert error: {}", e)))?;
                    Ok(serde_json::to_string(&result)
                        .unwrap_or_else(|_| "memory_upsert completed".to_string()))
                }
                None => Ok("Memory store not available".to_string()),
            }
        },

        // 记忆工具: memory_query / memory_search
        "memory_query" | "memory_search" => {
            match memory_store {
                Some(store) => {
                    let result = store.query_json(input.clone())
                        .map_err(|e| ForkedAgentError::ToolError(format!("memory_query error: {}", e)))?;
                    Ok(serde_json::to_string(&result)
                        .unwrap_or_else(|_| "memory_query completed".to_string()))
                }
                None => Ok("Memory store not available".to_string()),
            }
        },

        // 记忆工具: memory_forget
        "memory_forget" => {
            match memory_store {
                Some(store) => {
                    // memory_forget 支持两种模式: 按 id 或按 filter
                    if let Some(id) = input.get("id").and_then(|v| v.as_str()) {
                        let success = store.soft_delete(id)
                            .map_err(|e| ForkedAgentError::ToolError(format!("memory_forget error: {}", e)))?;
                        Ok(if success { format!("Memory item '{}' forgotten", id) } else { format!("Memory item '{}' not found", id) })
                    } else {
                        // 按 filter 批量删除
                        let count = store.batch_soft_delete_json(input.clone())
                            .map_err(|e| ForkedAgentError::ToolError(format!("memory_forget error: {}", e)))?;
                        Ok(format!("{} memory items forgotten", count))
                    }
                }
                None => Ok("Memory store not available".to_string()),
            }
        },

        // Skill 工具: list_skills
        // 支持 category 子目录结构: {skills_dir}/{category}/{name}/
        "list_skills" => {
            match &skills_dir {
                Some(dir) => {
                    let query = input.get("query")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    if !dir.exists() {
                        return Ok(json!({"skills": [], "count": 0}).to_string());
                    }

                    let mut entries = Vec::new();
                    if let Ok(read_dir) = std::fs::read_dir(dir) {
                        for entry in read_dir.flatten() {
                            if let Ok(file_type) = entry.file_type() {
                                if file_type.is_dir() {
                                    let entry_name = entry.file_name().to_string_lossy().to_string();
                                    // 检查是否是 category 目录 (包含子目录) 或直接是 skill 目录 (包含 SKILL.md)
                                    let has_skill_md = entry.path().join("SKILL.md").exists();
                                    if has_skill_md {
                                        // 直接是 skill 目录 (无 category)
                                        if query.is_empty() || entry_name.to_lowercase().contains(&query.to_lowercase()) {
                                            entries.push(json!({
                                                "name": entry_name,
                                                "has_skill_md": true,
                                            }));
                                        }
                                    } else {
                                        // 可能是 category 目录，搜索其下的 skill 子目录
                                        if let Ok(sub_entries) = std::fs::read_dir(entry.path()) {
                                            for sub_entry in sub_entries.flatten() {
                                                if sub_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                                                    let skill_name = sub_entry.file_name().to_string_lossy().to_string();
                                                    let has_md = sub_entry.path().join("SKILL.md").exists();
                                                    if has_md && (query.is_empty() || skill_name.to_lowercase().contains(&query.to_lowercase())) {
                                                        entries.push(json!({
                                                            "name": skill_name,
                                                            "category": entry_name,
                                                            "has_skill_md": true,
                                                        }));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if entries.is_empty() {
                        Ok(json!({"skills": [], "count": 0, "message": "No skills found"}).to_string())
                    } else {
                        let count = entries.len();
                        Ok(json!({"skills": entries, "count": count}).to_string())
                    }
                }
                None => Ok(json!({"skills": [], "count": 0, "message": "Skills directory not available"}).to_string()),
            }
        },

        // Skill 工具: skill_manage
        // 与主 skill_manage 工具 (crates/tools/src/skill_manage.rs) 保持一致:
        // - 返回 JSON 格式 {"success": true, "message": "..."} 供 extract_review_summary 解析
        // - patch 使用 fuzzy_match 9-strategy 模糊匹配
        // - create/edit/write_file 执行 security_scan 安全扫描
        // - create 验证 YAML frontmatter (name + description)
        // - 支持 category 参数
        "skill_manage" => {
            if let Some(store) = skill_file_store {
                let action = input
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let name = input.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let content = input
                    .get("content")
                    .or_else(|| input.get("new_string"))
                    .or_else(|| input.get("file_content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let result = match action {
                    "view" => store.view_skill_json(name),
                    "create" => {
                        let meta = extract_frontmatter(content);
                        let description = input
                            .get("description")
                            .and_then(|v| v.as_str())
                            .or_else(|| meta.get("description").and_then(|v| v.as_str()))
                            .unwrap_or("Learned reusable procedure");
                        store.create_skill_json(name, description, content)
                    }
                    "edit" => store.edit_skill_json(name, content),
                    "patch" => store.patch_skill_json(
                        name,
                        input
                            .get("old_text")
                            .or_else(|| input.get("old_string"))
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                        content,
                    ),
                    "delete" => store.delete_skill_json(name),
                    "write_file" => store.write_skill_file_json(
                        name,
                        input
                            .get("path")
                            .or_else(|| input.get("file_path"))
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                        content,
                    ),
                    "remove_file" => store.remove_skill_file_json(
                        name,
                        input
                            .get("path")
                            .or_else(|| input.get("file_path"))
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                    ),
                    "undo_latest" => store.restore_latest_skill_json(name),
                    _ => Err(blockcell_core::Error::Validation(
                        "skill_manage action must be create, patch, view, delete, edit, write_file, remove_file, or undo_latest"
                            .to_string(),
                    )),
                }
                .map_err(|e| ForkedAgentError::ToolError(format!("skill_manage error: {}", e)))?;
                return Ok(serde_json::to_string(&result)
                    .unwrap_or_else(|_| "skill_manage completed".to_string()));
            }

            match &skills_dir {
                Some(dir) => {
                    let action = input.get("action")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let name = input.get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let category = input.get("category")
                        .and_then(|v| v.as_str());

                    if name.is_empty() {
                        return Ok(json!({"success": false, "message": "skill_manage: 'name' parameter is required"}).to_string());
                    }

                    // 验证 skill 名称安全性 (路径遍历 + 正则格式)
                    if name.contains("..") || name.contains('/') || name.contains('\\') || name.contains('\0') {
                        return Ok(json!({"success": false, "message": format!("skill_manage: invalid skill name '{}'", name)}).to_string());
                    }
                    if !VALID_SKILL_NAME_RE.is_match(name) {
                        return Ok(json!({"success": false, "message": format!("skill_manage: invalid skill name '{}' (must match pattern: lowercase letters, digits, dots, underscores, hyphens, starting with letter or digit)", name)}).to_string());
                    }

                    // 支持 category 子目录 (与主工具一致: {skills_dir}/{category}/{name}/)
                    let skill_dir = if let Some(cat) = category {
                        if cat.contains("..") || cat.contains('/') || cat.contains('\\') || cat.contains('\0') {
                            return Ok(json!({"success": false, "message": format!("skill_manage: invalid category '{}'", cat)}).to_string());
                        }
                        dir.join(cat).join(name)
                    } else {
                        dir.join(name)
                    };

                    match action {
                        "view" => {
                            // 使用 find_skill_dir_forked 跨目錄搜索 (與主工具 find_skill_dir 對齊)
                            if let Some(found_dir) = find_skill_dir_forked(name, category, dir, external_skills_dirs) {
                                // 推斷 category: 如果 found_dir 的 parent != skills_dir, 則 parent name 為 category
                                let inferred_cat = if let Some(parent) = found_dir.parent() {
                                    if parent != dir {
                                        parent.file_name().map(|n| n.to_string_lossy().to_string())
                                    } else { None }
                                } else { None };
                                build_view_response_for_skill(&found_dir, name, inferred_cat.as_deref().or(category)).await
                            } else {
                                Ok(json!({"success": false, "message": format!("Skill '{}' not found (no SKILL.md)", name)}).to_string())
                            }
                        }
                        "create" => {
                            let content = input.get("content")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            if content.is_empty() {
                                return Ok(json!({"success": false, "message": "skill_manage create: 'content' parameter is required"}).to_string());
                            }

                            // 安全检查：内容大小限制 (与主工具一致, 使用字节数)
                            if content.len() > MAX_SKILL_CONTENT_CHARS {
                                return Ok(json!({"success": false, "message": format!("skill_manage create: content too large ({} bytes, max {})", content.len(), MAX_SKILL_CONTENT_CHARS)}).to_string());
                            }

                            // Frontmatter 验证: 检查 YAML frontmatter 包含 name 和 description
                            let frontmatter_issues = validate_skill_frontmatter(content);
                            if !frontmatter_issues.is_empty() {
                                return Ok(json!({
                                    "success": false,
                                    "message": format!("Frontmatter validation failed: {}", frontmatter_issues.join("; ")),
                                }).to_string());
                            }

                            // 安全扫描
                            let scan_result = scan_skill_content(content);
                            if !scan_result.passed {
                                return Ok(json!({
                                    "success": false,
                                    "message": format!("Security scan failed: {}",
                                        scan_result.issues.iter()
                                            .filter(|i| matches!(i.level, blockcell_tools::security_scan::IssueLevel::Critical))
                                            .map(|i| i.message.as_str())
                                            .collect::<Vec<_>>()
                                            .join("; ")),
                                }).to_string());
                            }

                            // 创建 skill 目录 — 先检查是否已存在
                            if skill_dir.exists() {
                                return Ok(json!({"success": false, "message": format!("Skill '{}' already exists. Use patch to modify it.", name)}).to_string());
                            }
                            tokio::fs::create_dir_all(&skill_dir).await
                                .map_err(ForkedAgentError::Io)?;

                            // 原子写入 SKILL.md (temp file + rename, 防止崩溃时损坏)
                            let skill_md_path = skill_dir.join("SKILL.md");
                            if let Err(e) = atomic_write_text(&skill_md_path, content).await {
                                // 写入失败: 回滚删除整个目录 (与主工具一致)
                                let _ = tokio::fs::remove_dir_all(&skill_dir).await;
                                return Err(ForkedAgentError::ToolError(format!("Failed to write SKILL.md: {}", e)));
                            }

                            // 生成 meta.json (从 frontmatter 提取元数据)
                            let meta = extract_frontmatter(content);
                            let meta_path = skill_dir.join("meta.json");
                            let meta_json = serde_json::to_string_pretty(&meta)
                                .unwrap_or_else(|_| "{}".to_string());
                            if let Err(e) = atomic_write_text(&meta_path, &meta_json).await {
                                // meta.json 写入失败不影响 Skill 创建, 仅记录警告
                                tracing::warn!(error = %e, "[forked] Failed to write meta.json for skill '{}'", name);
                            }

                            Ok(json!({
                                "success": true,
                                "message": if let Some(cat) = category {
                                    format!("Skill '{}' created in category '{}'", name, cat)
                                } else {
                                    format!("Skill '{}' created", name)
                                },
                                "hint": "Use action='write_file' to add reference files, templates, or scripts to this skill.",
                                "warnings": scan_result.issues.iter()
                                    .filter(|i| matches!(i.level, blockcell_tools::security_scan::IssueLevel::Warning))
                                    .map(|i| i.message.as_str())
                                    .collect::<Vec<_>>()
                            }).to_string())
                        }
                        "patch" => {
                            let old_string = input.get("old_string")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let new_string = input.get("new_string")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let file_path = input.get("file_path")
                                .and_then(|v| v.as_str())
                                .unwrap_or("SKILL.md");
                            let replace_all = input.get("replace_all")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);

                            if old_string.is_empty() {
                                return Ok(json!({"success": false, "message": "skill_manage patch: 'old_string' is required"}).to_string());
                            }

                            // 安全检查：file_path 不能包含路径遍历、反斜杠或空组件 (与主工具一致)
                            if file_path.contains("..") || file_path.contains('\0') || file_path.contains('\\') {
                                return Ok(json!({"success": false, "message": format!("skill_manage patch: invalid file_path '{}'", file_path)}).to_string());
                            }
                            // 验证每个路径组件不为空 (防止 // 等异常路径)
                            for component in file_path.split('/') {
                                if component.is_empty() {
                                    return Ok(json!({"success": false, "message": format!("skill_manage patch: invalid file_path '{}' (empty path component)", file_path)}).to_string());
                                }
                            }

                            // 使用 find_skill_dir_forked 跨目录搜索 (与主工具 find_skill_dir 对齐)
                            let patch_skill_dir = match find_skill_dir_forked(name, category, dir, external_skills_dirs) {
                                Some(d) => d,
                                None => {
                                    // 如果 skill_dir 本身存在 (可能是新 skill 还没有 SKILL.md), 也尝试
                                    if skill_dir.is_dir() { skill_dir.clone() }
                                    else { return Ok(json!({"success": false, "message": format!("Skill '{}' not found", name)}).to_string()); }
                                }
                            };

                            let target = patch_skill_dir.join(file_path);
                            if !target.exists() {
                                return Ok(json!({"success": false, "message": format!("skill_manage patch: file '{}' not found in skill '{}'", file_path, name)}).to_string());
                            }

                            let content = tokio::fs::read_to_string(&target).await
                                .map_err(ForkedAgentError::Io)?;

                            // 使用 fuzzy_match 的 9-strategy 模糊匹配 (与主工具一致)
                            match fuzzy_find_and_replace(&content, old_string, new_string, replace_all) {
                                Ok((new_content, match_count, strategy)) => {
                                    // 安全扫描
                                    let scan_result = scan_skill_content(&new_content);
                                    if !scan_result.passed {
                                        return Ok(json!({
                                            "success": false,
                                            "message": format!("Security scan failed. Changes not applied.\nCritical issues: {}",
                                                scan_result.issues.iter()
                                                    .filter(|i| matches!(i.level, blockcell_tools::security_scan::IssueLevel::Critical))
                                                    .map(|i| i.message.as_str())
                                                    .collect::<Vec<_>>()
                                                    .join("; ")),
                                        }).to_string());
                                    }

                                    // 原子写入 (temp file + rename)
                                    atomic_write_text(&target, &new_content).await
                                        .map_err(|e| ForkedAgentError::ToolError(format!("Failed to write patch: {}", e)))?;

                                    // 如果 patch 的是 SKILL.md，更新 meta.json
                                    if file_path == "SKILL.md" {
                                        let meta = extract_frontmatter(&new_content);
                                        let meta_path = patch_skill_dir.join("meta.json");
                                        let meta_json = serde_json::to_string_pretty(&meta)
                                            .unwrap_or_else(|_| "{}".to_string());
                                        let _ = atomic_write_text(&meta_path, &meta_json).await;
                                    }

                                    Ok(json!({
                                        "success": true,
                                        "match_count": match_count,
                                        "strategy": strategy,
                                        "message": format!("Patched {} occurrence(s) in '{}' using {} strategy", match_count, file_path, strategy),
                                        "warnings": scan_result.issues.iter()
                                            .filter(|i| matches!(i.level, blockcell_tools::security_scan::IssueLevel::Warning))
                                            .map(|i| i.message.as_str())
                                            .collect::<Vec<_>>()
                                    }).to_string())
                                }
                                Err(e) => {
                                    Ok(json!({
                                        "success": false,
                                        "message": format!("Fuzzy match failed: {}", e),
                                    }).to_string())
                                }
                            }
                        }
                        "delete" => {
                            // 使用 find_skill_dir_forked 跨目录搜索
                            let del_skill_dir = match find_skill_dir_forked(name, category, dir, external_skills_dirs) {
                                Some(d) => d,
                                None => return Ok(json!({"success": false, "message": format!("Skill '{}' not found", name)}).to_string()),
                            };
                            tokio::fs::remove_dir_all(&del_skill_dir).await
                                .map_err(ForkedAgentError::Io)?;
                            // 清理空的分类目录 (与主工具一致)
                            if let Some(category_dir) = del_skill_dir.parent() {
                                if category_dir != dir {
                                    let _ = tokio::fs::remove_dir(category_dir).await;
                                }
                            }
                            Ok(json!({"success": true, "message": format!("Skill '{}' deleted", name)}).to_string())
                        }
                        "edit" => {
                            let content = input.get("content")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            if content.is_empty() {
                                return Ok(json!({"success": false, "message": "skill_manage edit: 'content' parameter is required"}).to_string());
                            }

                            // 安全检查：内容大小限制 (与主工具一致, 使用字节数)
                            if content.len() > MAX_SKILL_CONTENT_CHARS {
                                return Ok(json!({"success": false, "message": format!("skill_manage edit: content too large ({} bytes, max {})", content.len(), MAX_SKILL_CONTENT_CHARS)}).to_string());
                            }

                            // 安全扫描
                            let scan_result = scan_skill_content(content);
                            if !scan_result.passed {
                                return Ok(json!({
                                    "success": false,
                                    "message": format!("Security scan failed: {}",
                                        scan_result.issues.iter()
                                            .filter(|i| matches!(i.level, blockcell_tools::security_scan::IssueLevel::Critical))
                                            .map(|i| i.message.as_str())
                                            .collect::<Vec<_>>()
                                            .join("; ")),
                                }).to_string());
                            }

                            // 使用 find_skill_dir_forked 跨目录搜索
                            let edit_skill_dir = match find_skill_dir_forked(name, category, dir, external_skills_dirs) {
                                Some(d) => d,
                                None => return Ok(json!({"success": false, "message": format!("Skill '{}' not found", name)}).to_string()),
                            };

                            let skill_md = edit_skill_dir.join("SKILL.md");
                            if !skill_md.exists() {
                                return Ok(json!({"success": false, "message": format!("Skill '{}' not found (no SKILL.md)", name)}).to_string());
                            }

                            // 备份原内容 (用于回滚, 与主工具一致)
                            let original_content = tokio::fs::read_to_string(&skill_md).await
                                .map_err(ForkedAgentError::Io)?;

                            if let Err(e) = atomic_write_text(&skill_md, content).await {
                                // 写入失败, 但原文件仍完好 (原子写入不会损坏原文件)
                                return Err(ForkedAgentError::ToolError(format!("Failed to write edit: {}", e)));
                            }

                            // 更新 meta.json
                            let meta = extract_frontmatter(content);
                            let meta_path = edit_skill_dir.join("meta.json");
                            let meta_json = serde_json::to_string_pretty(&meta)
                                .unwrap_or_else(|_| "{}".to_string());
                            if let Err(e) = atomic_write_text(&meta_path, &meta_json).await {
                                // meta.json 写入失败: 回滚 SKILL.md (与主工具一致)
                                let _ = atomic_write_text(&skill_md, &original_content).await;
                                tracing::warn!(error = %e, "[forked] Failed to write meta.json, rolling back SKILL.md for skill '{}'", name);
                                return Ok(json!({"success": false, "message": format!("Failed to write meta.json: {}", e)}).to_string());
                            }

                            Ok(json!({
                                "success": true,
                                "message": format!("Skill '{}' edited (full content replaced)", name),
                                "warnings": scan_result.issues.iter()
                                    .filter(|i| matches!(i.level, blockcell_tools::security_scan::IssueLevel::Warning))
                                    .map(|i| i.message.as_str())
                                    .collect::<Vec<_>>()
                            }).to_string())
                        }
                        "write_file" => {
                            let file_path = input.get("file_path")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let file_content = input.get("file_content")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            if file_path.is_empty() {
                                return Ok(json!({"success": false, "message": "skill_manage write_file: 'file_path' is required"}).to_string());
                            }

                            // 安全检查：file_path 不能包含路径遍历或反斜杠
                            if file_path.contains("..") || file_path.contains('\0') || file_path.contains('\\') {
                                return Ok(json!({"success": false, "message": format!("skill_manage write_file: invalid file_path '{}'", file_path)}).to_string());
                            }

                            // 安全检查：file_path 必须在允许的子目录下 (与主工具一致)
                            let allowed_prefixes = ["references/", "templates/", "scripts/", "assets/"];
                            if !allowed_prefixes.iter().any(|prefix| file_path.starts_with(prefix)) {
                                return Ok(json!({"success": false, "message": format!("skill_manage write_file: file_path must be under one of: {}", allowed_prefixes.join(", "))}).to_string());
                            }

                            // 安全检查：内容大小限制
                            if file_content.len() > MAX_FILE_SIZE {
                                return Ok(json!({"success": false, "message": format!("skill_manage write_file: content too large ({} bytes, max {})", file_content.len(), MAX_FILE_SIZE)}).to_string());
                            }

                            // 使用 find_skill_dir_forked 跨目录搜索
                            let wf_skill_dir = match find_skill_dir_forked(name, category, dir, external_skills_dirs) {
                                Some(d) => d,
                                None => return Ok(json!({"success": false, "message": format!("Skill '{}' not found", name)}).to_string()),
                            };

                            // 安全扫描
                            let scan_result = scan_skill_content(file_content);
                            if !scan_result.passed {
                                return Ok(json!({
                                    "success": false,
                                    "message": format!("Security scan failed: {}",
                                        scan_result.issues.iter()
                                            .filter(|i| matches!(i.level, blockcell_tools::security_scan::IssueLevel::Critical))
                                            .map(|i| i.message.as_str())
                                            .collect::<Vec<_>>()
                                            .join("; ")),
                                }).to_string());
                            }

                            let target = wf_skill_dir.join(file_path);
                            // 确保父目录存在
                            if let Some(parent) = target.parent() {
                                tokio::fs::create_dir_all(parent).await
                                    .map_err(ForkedAgentError::Io)?;
                            }

                            // 原子写入 (temp file + rename, 防止崩溃时损坏)
                            atomic_write_text(&target, file_content).await
                                .map_err(|e| ForkedAgentError::ToolError(format!("Failed to write file: {}", e)))?;

                            // 目录级安全扫描 (与主工具一致: 写入后检查整个目录)
                            let dir_scan = scan_skill_dir_with_trust(&wf_skill_dir, blockcell_tools::security_scan::TrustLevel::AgentCreated);
                            if !dir_scan.passed {
                                // 写入的文件导致目录级安全问题 → 回滚
                                let _ = tokio::fs::remove_file(&target).await;
                                return Ok(json!({
                                    "success": false,
                                    "message": format!("Directory-level security scan failed after writing file. File removed.\nCritical issues: {}",
                                        dir_scan.issues.iter()
                                            .filter(|i| matches!(i.level, blockcell_tools::security_scan::IssueLevel::Critical))
                                            .map(|i| i.message.as_str())
                                            .collect::<Vec<_>>()
                                            .join("; ")),
                                }).to_string());
                            }

                            Ok(json!({
                                "success": true,
                                "message": format!("File '{}' written to skill '{}'", file_path, name),
                                "warnings": scan_result.issues.iter()
                                    .filter(|i| matches!(i.level, blockcell_tools::security_scan::IssueLevel::Warning))
                                    .map(|i| i.message.as_str())
                                    .collect::<Vec<_>>()
                            }).to_string())
                        }
                        "remove_file" => {
                            let file_path = input.get("file_path")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            if file_path.is_empty() {
                                return Ok(json!({"success": false, "message": "skill_manage remove_file: 'file_path' is required"}).to_string());
                            }

                            // 不允许删除 SKILL.md 或 meta.json (与主工具一致)
                            if file_path == "SKILL.md" || file_path == "meta.json" {
                                return Ok(json!({"success": false, "message": "Cannot delete SKILL.md or meta.json. Use delete action to remove the entire skill."}).to_string());
                            }

                            // 安全检查 (与主工具一致: 包含反斜杠检查)
                            if file_path.contains("..") || file_path.contains('\0') || file_path.contains('\\') {
                                return Ok(json!({"success": false, "message": format!("skill_manage remove_file: invalid file_path '{}'", file_path)}).to_string());
                            }

                            // 使用 find_skill_dir_forked 跨目录搜索
                            let rf_skill_dir = match find_skill_dir_forked(name, category, dir, external_skills_dirs) {
                                Some(d) => d,
                                None => return Ok(json!({"success": false, "message": format!("Skill '{}' not found", name)}).to_string()),
                            };

                            let target = rf_skill_dir.join(file_path);
                            if target.exists() {
                                tokio::fs::remove_file(&target).await
                                    .map_err(ForkedAgentError::Io)?;
                                // 清理空父目录 (与主工具一致)
                                if let Some(parent) = target.parent() {
                                    if parent != rf_skill_dir {
                                        let _ = tokio::fs::remove_dir(parent).await;
                                    }
                                }
                                Ok(json!({"success": true, "message": format!("File '{}' removed from skill '{}'", file_path, name)}).to_string())
                            } else {
                                Ok(json!({"success": false, "message": format!("File '{}' not found in skill '{}'", file_path, name)}).to_string())
                            }
                        }
                        _ => Ok(json!({"success": false, "message": format!("skill_manage: unknown action '{}'. Supported: create, patch, view, delete, edit, write_file, remove_file", action)}).to_string())
                    }
                }
                None => Ok(json!({"success": false, "message": "Skills directory not available"}).to_string()),
            }
        },

        // 不支持的工具
        _ => {
            Ok(format!("Tool '{}' is not supported in forked mode. Supported tools: read_file, file_edit, file_write, exec, grep, glob, memory_upsert, memory_query, memory_forget, skill_manage, list_skills", tool_name))
        }
    }
}

/// 为 skill_manage "view" 构建完整响应 (包含 meta, references, templates)
pub(crate) async fn build_view_response_for_skill(
    skill_dir: &Path,
    skill_name: &str,
    category: Option<&str>,
) -> Result<String, ForkedAgentError> {
    let skill_md = skill_dir.join("SKILL.md");
    let content = tokio::fs::read_to_string(&skill_md)
        .await
        .map_err(ForkedAgentError::Io)?;
    let truncated = if content.len() > MAX_OUTPUT_CHARS {
        let mut boundary = MAX_OUTPUT_CHARS;
        while boundary > 0 && !content.is_char_boundary(boundary) {
            boundary -= 1;
        }
        format!(
            "{}...\n[Truncated, total {} chars]",
            &content[..boundary],
            content.len()
        )
    } else {
        content
    };
    let meta = read_meta_json(skill_dir);
    let references = list_dir_files(&skill_dir.join("references"));
    let templates = list_dir_files(&skill_dir.join("templates"));
    let mut resp = json!({
        "success": true,
        "name": skill_name,
        "content": truncated,
        "meta": meta,
        "references": references,
        "templates": templates,
    });
    if let Some(cat) = category {
        resp["category"] = json!(cat);
    }
    Ok(resp.to_string())
}

/// 列出目录中的文件 (仅文件名, 最多 50)
pub(crate) fn list_dir_files(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    if dir.exists() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    files.push(entry.file_name().to_string_lossy().to_string());
                    if files.len() >= 50 {
                        break;
                    }
                }
            }
        }
    }
    files
}

/// 读取 meta.json 内容 (如果存在)
pub(crate) fn read_meta_json(skill_dir: &Path) -> Option<serde_json::Value> {
    let meta_path = skill_dir.join("meta.json");
    if meta_path.exists() {
        std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    } else {
        None
    }
}

/// 验证 Skill frontmatter: 检查 YAML frontmatter 包含必需的 name 和 description 字段
///
/// 返回问题列表，空列表表示通过验证
pub(crate) fn validate_skill_frontmatter(content: &str) -> Vec<String> {
    let mut issues = Vec::new();

    // 检查是否以 frontmatter 分隔符开头
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        issues.push("Missing YAML frontmatter: content must start with '---'".to_string());
        return issues;
    }

    // 提取 frontmatter 内容
    let after_first = &trimmed[3..]; // skip leading ---
    let fm_end = match after_first.find("\n---") {
        Some(pos) => pos,
        None => {
            issues.push("Unclosed YAML frontmatter: missing closing '---'".to_string());
            return issues;
        }
    };

    let fm_content = &after_first[..fm_end];

    // 检查必需的 name 字段 (包括空值检查)
    let has_valid_name = fm_content.lines().any(|line| {
        let trimmed = line.trim();
        if trimmed == "name:" || trimmed.starts_with("name:") || trimmed.starts_with("name :") {
            // 检查值是否非空
            if let Some(val) = trimmed.split_once(':') {
                let value = val.1.trim();
                return !value.is_empty();
            }
        }
        false
    });
    if !has_valid_name {
        issues.push("Missing or empty required field 'name' in frontmatter".to_string());
    }

    // 检查必需的 description 字段 (包括空值和长度检查)
    let max_desc_len = 1024;
    let has_valid_desc = fm_content.lines().any(|line| {
        let trimmed = line.trim();
        if trimmed == "description:"
            || trimmed.starts_with("description:")
            || trimmed.starts_with("description :")
        {
            // 检查值是否非空且不超过长度限制
            if let Some(val) = trimmed.split_once(':') {
                let value = val.1.trim();
                return !value.is_empty() && value.len() <= max_desc_len;
            }
        }
        false
    });
    if !has_valid_desc {
        issues.push(
            "Missing or empty required field 'description' in frontmatter (max 1024 chars)"
                .to_string(),
        );
    }

    issues
}

/// 简化的 glob 匹配
pub(crate) fn simple_glob_match(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(ext) = pattern.strip_prefix("*.") {
        return name.ends_with(ext);
    }
    if let Some(prefix) = pattern.strip_suffix("*") {
        return name.starts_with(prefix);
    }
    name == pattern
}
