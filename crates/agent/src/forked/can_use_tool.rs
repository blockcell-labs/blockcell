//! 工具权限控制
//!
//! 定义 Forked Agent 的工具权限检查机制。
//! Forked Agent 运行在受限环境中，只能执行特定工具和特定操作。

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 工具权限决策
#[derive(Debug, Clone)]
pub enum ToolPermission {
    /// 允许执行
    Allow,
    /// 拒绝执行，附带原因
    Deny { message: String },
}

/// 工具权限检查函数类型
///
/// 接收工具名称和输入参数，返回权限决策。
pub type CanUseToolFn = Arc<dyn Fn(&str, &serde_json::Value) -> ToolPermission + Send + Sync>;

/// 允许名单策略：只有名单内的工具可执行，其余一律拒绝。
///
/// 供 subagent / typed fork 使用——其可执行工具集必须与传给模型的 schema 名单一致，
/// 不能依赖「未配置即放行」的默认值。
pub fn create_allowlist_can_use_tool(allowed: Vec<String>) -> CanUseToolFn {
    Arc::new(move |tool_name, _input| {
        if allowed.iter().any(|name| name == tool_name) {
            ToolPermission::Allow
        } else if allowed.is_empty() {
            ToolPermission::Deny {
                message: format!(
                    "Tool '{}' denied: this forked agent declares no tools and no tool policy",
                    tool_name
                ),
            }
        } else {
            ToolPermission::Deny {
                message: format!(
                    "Tool '{}' denied: not part of this agent's toolset ({})",
                    tool_name,
                    allowed.join("/")
                ),
            }
        }
    })
}

/// 从 tool schema 列表提取工具名。
///
/// 兼容两种形状：`{"type":"function","function":{"name": …}}`（fork/typed 使用）
/// 与 `{"name": …}`。
pub fn tool_names_from_schemas(schemas: &[serde_json::Value]) -> Vec<String> {
    schemas
        .iter()
        .filter_map(|schema| {
            schema
                .get("function")
                .and_then(|f| f.get("name"))
                .or_else(|| schema.get("name"))
                .and_then(|name| name.as_str())
                .map(str::to_string)
        })
        .collect()
}

/// 工具 trait 的简化接口（避免依赖完整的 Tool 类型）
///
/// 在实际集成时，这应该与 blockcell_tools 中的 Tool trait 对应。
#[allow(dead_code)]
pub trait ToolInfo {
    fn name(&self) -> &str;
}

/// 创建 Session Memory 提取的工具权限检查
///
/// 只允许编辑特定的 memory 文件
pub fn create_memory_file_can_use_tool(memory_path: &Path) -> CanUseToolFn {
    let memory_path = memory_path.to_path_buf();
    Arc::new(move |tool_name: &str, input: &serde_json::Value| {
        // 只允许 file_edit 工具，且只能编辑 memory_path 文件
        if tool_name == "file_edit" || tool_name == "edit_file" {
            if let Some(file_path) = input.get("file_path").and_then(|v| v.as_str()) {
                if let Some(memory_path_str) = memory_path.to_str() {
                    if file_path == memory_path_str {
                        return ToolPermission::Allow;
                    }
                }
            }
        }

        ToolPermission::Deny {
            message: format!("only file_edit on {} is allowed", memory_path.display()),
        }
    })
}

/// 创建自动记忆提取的工具权限检查
///
/// 允许：
/// - REPL（内部会重新调用此函数）
/// - Read/Grep/Glob/ListDir（只读，且仅限记忆目录内的显式路径）
/// - Shell/Exec 只读命令（必须指定记忆目录内的工作目录，且命令参数不能逃逸）
/// - Edit/Write 仅限 memory 目录内
pub fn create_auto_mem_can_use_tool(memory_dir: &Path) -> CanUseToolFn {
    let memory_dir = memory_dir.to_path_buf();
    Arc::new(move |tool_name: &str, input: &serde_json::Value| {
        // 允许 REPL
        if tool_name == "repl" {
            return ToolPermission::Allow;
        }

        // 只读文件工具也必须限制在记忆目录内，避免后台 agent 读取任意本地路径。
        if matches!(tool_name, "read_file" | "grep" | "glob" | "list_dir") {
            return allow_read_only_path(tool_name, input, &memory_dir);
        }

        // shell/exec 难以完整解析参数，必须额外要求工作目录和命令参数都不逃逸记忆目录。
        if matches!(tool_name, "shell" | "bash" | "exec") {
            return allow_restricted_shell(input, &memory_dir);
        }

        // Edit/Write 仅限记忆目录内
        if matches!(
            tool_name,
            "file_edit" | "edit_file" | "file_write" | "write_file"
        ) {
            return allow_write_path(
                tool_name,
                input,
                &memory_dir,
                "Only Edit/Write within memory dir allowed",
            );
        }

        // 其他工具全部拒绝
        ToolPermission::Deny {
            message: "仅允许记忆目录内的只读文件工具和写入工具".to_string(),
        }
    })
}

/// 创建 Dream 整合的工具权限检查
///
/// 与 auto_mem 类似，但所有文件读写都限制在记忆根目录内
pub fn create_dream_can_use_tool(memory_root: &Path) -> CanUseToolFn {
    let memory_root = memory_root.to_path_buf();
    Arc::new(move |tool_name: &str, input: &serde_json::Value| {
        // 允许 REPL
        if tool_name == "repl" {
            return ToolPermission::Allow;
        }

        // 只读工具必须显式访问记忆根目录内的路径，避免 dream agent 泄露本地敏感文件。
        if matches!(tool_name, "read_file" | "grep" | "glob" | "ls" | "list_dir") {
            return allow_read_only_path(tool_name, input, &memory_root);
        }

        // shell/exec 只在工作目录和参数都受限时允许。
        if matches!(tool_name, "shell" | "bash" | "exec") {
            return allow_restricted_shell(input, &memory_root);
        }

        // Edit/Write 仅限记忆目录
        if matches!(
            tool_name,
            "file_edit" | "edit_file" | "file_write" | "write_file"
        ) {
            return allow_write_path(
                tool_name,
                input,
                &memory_root,
                "Only Edit/Write within memory directory",
            );
        }

        ToolPermission::Deny {
            message: "Tool not allowed in dream mode".to_string(),
        }
    })
}

/// 创建 Compact 压缩的工具权限检查
///
/// Compact 不需要工具，只生成文本摘要
pub fn create_compact_can_use_tool() -> CanUseToolFn {
    Arc::new(
        |tool_name: &str, _input: &serde_json::Value| ToolPermission::Deny {
            message: format!(
                "Compact mode does not allow any tools, attempted: {}",
                tool_name
            ),
        },
    )
}

// ── 工具名单（单一来源） ────────────────────────────────────────────
//
// 每种 Review 模式的可用工具集必须只有一个定义处。历史上这里维护了三份独立的
// 名单（执行期 Allow arm、schema 清单、Deny 消息枚举），导致 Deny 消息漏列
// `list_dir` 这类"诊断信息不准"的漂移。现在由 `TOOL_ROSTERS` 单一来源派生三者：
//   1. 执行期 Allow 判定  —— `roster_allows()`
//   2. 传给 LLM 的 schema —— `build_roster_schemas()`
//   3. Deny 消息的枚举    —— `roster_deny_message()`
// 新增或删除工具只需改 `SCHEMA_BUILDERS`，三处自动同步。
//
// 注意：`run.rs` 的 `filter_tool_schemas` 仍会按 `params.tools` 白名单再过滤一次，
// 那是 typed subagent 的语义，与这里的 Review 名单无关。

/// Review 模式。与 `ReviewMode` 的字段一一对应，但这里不依赖 runtime 的枚举，
/// 避免 `forked` 模块反向依赖 `runtime`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewToolMode {
    Skill,
    Memory,
    Combined,
    Flush,
}

impl ReviewToolMode {
    /// 需要遍历断言"三处名单一致"的模式（Flush 是单工具模式，单独覆盖）。
    pub const ALL: [ReviewToolMode; 3] = [
        ReviewToolMode::Skill,
        ReviewToolMode::Memory,
        ReviewToolMode::Combined,
    ];

    /// 该模式的工具名单（顺序即 schema 发送顺序，也是 Deny 消息的列举顺序）。
    pub fn roster(self) -> &'static [&'static str] {
        match self {
            ReviewToolMode::Skill => &SKILL_REVIEW_ROSTER,
            ReviewToolMode::Memory => &MEMORY_REVIEW_ROSTER,
            ReviewToolMode::Combined => &COMBINED_REVIEW_ROSTER,
            ReviewToolMode::Flush => &FLUSH_ROSTER,
        }
    }

    fn schema_order(self) -> &'static [SchemaSlot] {
        match self {
            ReviewToolMode::Skill => &SKILL_REVIEW_SCHEMA_ORDER,
            ReviewToolMode::Memory => &MEMORY_REVIEW_SCHEMA_ORDER,
            ReviewToolMode::Combined => &COMBINED_REVIEW_SCHEMA_ORDER,
            ReviewToolMode::Flush => &FLUSH_SCHEMA_ORDER,
        }
    }
}

const FLUSH_ROSTER: &[&str] = &["memory_manage"];

const SKILL_REVIEW_ROSTER: &[&str] = &[
    "skill_manage",
    "list_skills",
    "read_file",
    "grep",
    "glob",
    "list_dir",
];

const MEMORY_REVIEW_ROSTER: &[&str] = &[
    "memory_manage",
    "memory_query",
    "memory_search",
    "read_file",
    "grep",
    "glob",
    "list_dir",
];

const COMBINED_REVIEW_ROSTER: &[&str] = &[
    "skill_manage",
    "list_skills",
    "memory_manage",
    "memory_query",
    "memory_search",
    "read_file",
    "grep",
    "glob",
    "list_dir",
];

/// schema 槽位：工具名 + 构造器。顺序必须与对应的 `*_ROSTER` 完全一致，
/// 由 `roster_and_schemas_agree` 测试逐模式断言。
type SchemaSlot = (&'static str, fn() -> serde_json::Value);

const FLUSH_SCHEMA_ORDER: &[SchemaSlot] = &[(
    "memory_manage",
    build_memory_manage_schema as fn() -> serde_json::Value,
)];

const SKILL_REVIEW_SCHEMA_ORDER: &[SchemaSlot] = &[
    (
        "skill_manage",
        build_skill_manage_schema as fn() -> serde_json::Value,
    ),
    (
        "list_skills",
        build_list_skills_schema as fn() -> serde_json::Value,
    ),
    (
        "read_file",
        build_read_file_schema as fn() -> serde_json::Value,
    ),
    ("grep", build_grep_schema as fn() -> serde_json::Value),
    ("glob", build_glob_schema as fn() -> serde_json::Value),
    (
        "list_dir",
        build_list_dir_schema as fn() -> serde_json::Value,
    ),
];

const MEMORY_REVIEW_SCHEMA_ORDER: &[SchemaSlot] = &[
    (
        "memory_manage",
        build_memory_manage_schema as fn() -> serde_json::Value,
    ),
    (
        "memory_query",
        build_memory_query_schema as fn() -> serde_json::Value,
    ),
    (
        "memory_search",
        build_memory_search_schema as fn() -> serde_json::Value,
    ),
    (
        "read_file",
        build_read_file_schema as fn() -> serde_json::Value,
    ),
    ("grep", build_grep_schema as fn() -> serde_json::Value),
    ("glob", build_glob_schema as fn() -> serde_json::Value),
    (
        "list_dir",
        build_list_dir_schema as fn() -> serde_json::Value,
    ),
];

const COMBINED_REVIEW_SCHEMA_ORDER: &[SchemaSlot] = &[
    (
        "skill_manage",
        build_skill_manage_schema as fn() -> serde_json::Value,
    ),
    (
        "list_skills",
        build_list_skills_schema as fn() -> serde_json::Value,
    ),
    (
        "memory_manage",
        build_memory_manage_schema as fn() -> serde_json::Value,
    ),
    (
        "memory_query",
        build_memory_query_schema as fn() -> serde_json::Value,
    ),
    (
        "memory_search",
        build_memory_search_schema as fn() -> serde_json::Value,
    ),
    (
        "read_file",
        build_read_file_schema as fn() -> serde_json::Value,
    ),
    ("grep", build_grep_schema as fn() -> serde_json::Value),
    ("glob", build_glob_schema as fn() -> serde_json::Value),
    (
        "list_dir",
        build_list_dir_schema as fn() -> serde_json::Value,
    ),
];

/// 执行期判定：`name` 是否在 `mode` 的名单内。
pub fn roster_allows(mode: ReviewToolMode, name: &str) -> bool {
    mode.roster().contains(&name)
}

/// 由名单派生的 Deny 消息，枚举与 Allow arm 完全一致。
pub fn roster_deny_message(mode: ReviewToolMode, attempted: &str) -> String {
    format!(
        "{} only allows {}, attempted: {}",
        mode.label(),
        mode.roster().join("/"),
        attempted
    )
}

impl ReviewToolMode {
    /// 该模式在错误消息中的显示名。
    pub fn label(self) -> &'static str {
        match self {
            ReviewToolMode::Skill => "Skill Review",
            ReviewToolMode::Memory => "Memory Review",
            ReviewToolMode::Combined => "Combined Review",
            ReviewToolMode::Flush => "Flush mode",
        }
    }
}

/// 由名单派生 schema 列表。顺序与 `roster()` 一致。
fn build_roster_schemas(mode: ReviewToolMode) -> Vec<serde_json::Value> {
    mode.schema_order()
        .iter()
        .map(|(_, build)| build())
        .collect()
}

/// 返回该模式名单对应的工具名集合（顺序同 `roster()`）。
///
/// 供上层在需要"这个 fork 允许哪些工具"时查询，避免它们再维护一份名单。
pub fn roster_names(mode: ReviewToolMode) -> Vec<&'static str> {
    mode.roster().to_vec()
}

pub fn create_skill_review_can_use_tool() -> CanUseToolFn {
    make_roster_guard(ReviewToolMode::Skill)
}

/// 创建 Flush (Memory Flush) 的工具权限检查
///
/// Flush Agent 只能保存记忆, 不能查询或删除:
/// - memory_upsert: 写入/更新记忆项
///
/// 与 Hermes 一致: flush 仅暴露 memory 工具 (等价于 memory_upsert)
pub fn create_flush_can_use_tool() -> CanUseToolFn {
    make_roster_guard(ReviewToolMode::Flush)
}

pub fn build_flush_tool_schemas() -> Vec<serde_json::Value> {
    build_roster_schemas(ReviewToolMode::Flush)
}

/// 创建 Memory Review 的工具权限检查
///
/// Memory Review Agent 只能使用记忆工具和只读文件工具:
/// - memory_upsert: 写入/更新记忆项
/// - memory_query: 查询记忆项
/// - memory_search: 搜索记忆项
/// - memory_forget: 删除记忆项
/// - read_file: 读取文件内容
/// - grep/glob: 搜索文件
pub fn create_memory_review_can_use_tool() -> CanUseToolFn {
    make_roster_guard(ReviewToolMode::Memory)
}

/// 创建 Combined Review (Skill + Memory) 的工具权限检查
///
/// Combined Review Agent 可以使用 Skill 工具和 Memory 工具:
/// - skill_manage: 创建/修补 Skill
/// - list_skills: 查看 Skill 列表
/// - memory_upsert: 写入/更新记忆项
/// - memory_query: 查询记忆项
/// - memory_search: 搜索记忆项
/// - memory_forget: 删除记忆项
/// - read_file: 读取文件内容
/// - grep/glob: 搜索文件
pub fn create_combined_review_can_use_tool() -> CanUseToolFn {
    make_roster_guard(ReviewToolMode::Combined)
}

/// 由名单构造执行期守卫。
fn make_roster_guard(mode: ReviewToolMode) -> CanUseToolFn {
    Arc::new(move |tool_name: &str, _input: &serde_json::Value| {
        if roster_allows(mode, tool_name) {
            ToolPermission::Allow
        } else {
            ToolPermission::Deny {
                message: roster_deny_message(mode, tool_name),
            }
        }
    })
}

/// 构建 Skill Review 模式的 LLM 工具 schema 列表
///
/// 这些 schema 会传给 provider.chat() 的 tools 参数，让 LLM 知道可用的工具及其参数。
pub fn build_skill_review_tool_schemas() -> Vec<serde_json::Value> {
    build_roster_schemas(ReviewToolMode::Skill)
}

/// 构建 Memory Review 模式的 LLM 工具 schema 列表
pub fn build_memory_review_tool_schemas() -> Vec<serde_json::Value> {
    build_roster_schemas(ReviewToolMode::Memory)
}

/// 构建 Combined Review 模式的 LLM 工具 schema 列表
pub fn build_combined_review_tool_schemas() -> Vec<serde_json::Value> {
    build_roster_schemas(ReviewToolMode::Combined)
}

// ── 工具 Schema 定义 ──────────────────────────────────────────────

fn wrap_tool_schema(
    name: &str,
    description: &str,
    parameters: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters,
        }
    })
}

fn build_skill_manage_schema() -> serde_json::Value {
    wrap_tool_schema(
        "skill_manage",
        "Manage skills: create, patch, view, delete, edit, write_file, or remove_file. Use 'view' to read a skill, 'create' to create a new skill, 'patch' to make targeted edits, 'edit' to replace entire content, 'delete' to remove a skill.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "patch", "view", "delete", "edit", "write_file", "remove_file"],
                    "description": "The action to perform on the skill"
                },
                "name": {
                    "type": "string",
                    "description": "The skill name (lowercase letters, digits, dots, underscores, hyphens)"
                },
                "category": {
                    "type": "string",
                    "description": "Optional category folder for the skill (e.g., 'data', 'web', 'automation'). Defaults to no category."
                },
                "content": {
                    "type": "string",
                    "description": "The content for create/edit actions (Markdown format with YAML frontmatter)"
                },
                "old_string": {
                    "type": "string",
                    "description": "The old string to replace (for patch action)"
                },
                "new_string": {
                    "type": "string",
                    "description": "The new string to replace with (for patch action)"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "If true, replace ALL occurrences of old_string (for patch action). Default: false."
                },
                "file_path": {
                    "type": "string",
                    "description": "The file path within the skill (for patch/write_file/remove_file)"
                },
                "file_content": {
                    "type": "string",
                    "description": "The file content (for write_file action)"
                },
            },
            "required": ["action", "name"]
        }),
    )
}

fn build_list_skills_schema() -> serde_json::Value {
    wrap_tool_schema(
        "list_skills",
        "List all available skills. Returns skill names and whether they have a SKILL.md file.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Optional search query to filter skills",
                },
            },
        }),
    )
}

fn build_memory_manage_schema() -> serde_json::Value {
    wrap_tool_schema(
        "memory_manage",
        "Write durable declarative memory to USER.md or MEMORY.md. Use for stable user preferences, project facts, and reusable non-procedural lessons. Do not use for task logs or temporary progress.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "replace", "remove", "undo_latest"],
                    "description": "Memory operation."
                },
                "target": {
                    "type": "string",
                    "enum": ["user", "memory"],
                    "description": "user = user profile/preferences; memory = durable project/environment facts."
                },
                "scope": {
                    "type": "string",
                    "enum": ["session", "workspace", "user"],
                    "default": "session",
                    "description": "session = current session only; workspace = cross-session project facts; user = cross-session user profile/preferences."
                },
                "content": {
                    "type": "string",
                    "description": "Entry content for add/replace."
                },
                "old_text": {
                    "type": "string",
                    "description": "Unique text identifying the entry for replace/remove."
                }
            },
            "required": ["action", "target"]
        }),
    )
}

fn build_memory_query_schema() -> serde_json::Value {
    wrap_tool_schema(
        "memory_query",
        "Query memory items by category, tags, or keyword search.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "description": "Filter by category"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Filter by tags"
                },
                "query": {
                    "type": "string",
                    "description": "Keyword search query"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return"
                },
            },
        }),
    )
}

fn build_memory_search_schema() -> serde_json::Value {
    wrap_tool_schema(
        "memory_search",
        "Search memory items by keyword. Alias for memory_query with a search focus.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query to find matching memory items"
                },
                "category": {
                    "type": "string",
                    "description": "Optional filter by category"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional filter by tags"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return"
                },
            },
            "required": ["query"]
        }),
    )
}

fn build_read_file_schema() -> serde_json::Value {
    wrap_tool_schema(
        "read_file",
        "Read the contents of a file.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The path to the file to read"
                },
            },
            "required": ["file_path"]
        }),
    )
}

fn build_grep_schema() -> serde_json::Value {
    wrap_tool_schema(
        "grep",
        "Search for a pattern in a file. Returns matching lines.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "The file path to search in"
                },
            },
            "required": ["pattern"]
        }),
    )
}

fn build_glob_schema() -> serde_json::Value {
    wrap_tool_schema(
        "glob",
        "Match files by pattern in a directory. Supports basic wildcards like *.md.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The glob pattern to match (e.g., '*.md', '*')"
                },
                "path": {
                    "type": "string",
                    "description": "The directory to search in"
                },
            },
            "required": ["pattern"]
        }),
    )
}

fn build_list_dir_schema() -> serde_json::Value {
    wrap_tool_schema(
        "list_dir",
        "List the contents of a directory. Returns file and directory names.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The directory path to list. Defaults to current directory."
                },
            },
        }),
    )
}

fn allow_read_only_path(
    tool_name: &str,
    input: &serde_json::Value,
    allowed_dir: &Path,
) -> ToolPermission {
    let Some(path) = read_only_tool_path(tool_name, input) else {
        return ToolPermission::Deny {
            message: format!("{} 必须显式提供记忆目录内的路径", tool_name),
        };
    };

    let resolved_path = resolve_path_against_allowed_dir(path, allowed_dir);

    if is_path_within_directory_path(&resolved_path, allowed_dir) {
        ToolPermission::Allow
    } else {
        ToolPermission::Deny {
            message: format!(
                "{} 只能访问记忆目录内的路径: {} (requested: {}, resolved: {})",
                tool_name,
                allowed_dir.display(),
                path,
                resolved_path.display()
            ),
        }
    }
}

fn resolve_path_against_allowed_dir(path: &str, allowed_dir: &Path) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        allowed_dir.join(path)
    }
}

fn read_only_tool_path<'a>(tool_name: &str, input: &'a serde_json::Value) -> Option<&'a str> {
    match tool_name {
        "read_file" => input.get("file_path").and_then(|v| v.as_str()),
        "grep" | "glob" | "ls" => input.get("path").and_then(|v| v.as_str()),
        "list_dir" => input
            .get("path")
            .or_else(|| input.get("dir_path"))
            .and_then(|v| v.as_str()),
        _ => None,
    }
}

fn allow_write_path(
    _tool_name: &str,
    input: &serde_json::Value,
    allowed_dir: &Path,
    deny_message: &str,
) -> ToolPermission {
    let Some(file_path) = input.get("file_path").and_then(|v| v.as_str()) else {
        return ToolPermission::Deny {
            message: deny_message.to_string(),
        };
    };

    let resolved_path = resolve_path_against_allowed_dir(file_path, allowed_dir);
    if is_auto_mem_path(&resolved_path.to_string_lossy(), allowed_dir) {
        ToolPermission::Allow
    } else {
        ToolPermission::Deny {
            message: format!(
                "{} (requested: {}, resolved: {})",
                deny_message,
                file_path,
                resolved_path.display()
            ),
        }
    }
}

fn allow_restricted_shell(input: &serde_json::Value, allowed_dir: &Path) -> ToolPermission {
    let Some(cmd) = input.get("command").and_then(|v| v.as_str()) else {
        return ToolPermission::Deny {
            message: "shell/exec 必须提供 command".to_string(),
        };
    };

    if !is_read_only_command(cmd) {
        return ToolPermission::Deny {
            message: "shell/exec 仅允许只读命令".to_string(),
        };
    }

    let Some(working_dir) = input.get("working_dir").and_then(|v| v.as_str()) else {
        return ToolPermission::Deny {
            message: "shell/exec 必须显式指定记忆目录内的 working_dir".to_string(),
        };
    };

    if !is_path_within_directory(working_dir, allowed_dir) {
        return ToolPermission::Deny {
            message: format!(
                "shell/exec 的 working_dir 只能位于记忆目录内: {}",
                allowed_dir.display()
            ),
        };
    }

    // 只读命令本身还不能携带绝对路径、环境变量或父目录跳转，
    // 否则即使 current_dir 受限，也可能读取到记忆目录外的文件。
    if !shell_command_paths_are_confined(cmd) {
        return ToolPermission::Deny {
            message: "shell/exec 命令参数不能包含绝对路径、环境变量或父目录跳转".to_string(),
        };
    }

    ToolPermission::Allow
}

fn shell_command_paths_are_confined(cmd: &str) -> bool {
    cmd.split_whitespace().all(|raw_token| {
        let token = raw_token.trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\'' | ',' | ':' | '(' | ')' | '[' | ']' | '{' | '}'
            )
        });
        if token.is_empty() {
            return true;
        }

        let path_like = token
            .split_once('=')
            .map(|(_, value)| value)
            .unwrap_or(token);

        if path_like.is_empty() || (token.starts_with('-') && !token.contains('=')) {
            return true;
        }

        if path_like.starts_with('~')
            || path_like.starts_with('/')
            || path_like.starts_with('\\')
            || path_like.contains("..")
            || path_like.contains('$')
            || path_like.contains('%')
            || Path::new(path_like).is_absolute()
        {
            return false;
        }

        true
    })
}

/// 检查是否只读命令
///
/// 安全检查：
/// 1. 命令必须以只读前缀开头
/// 2. 不能包含输出重定向符号 (>, >>)
/// 3. 不能包含管道符号 (|)
/// 4. 不能包含命令替换 ($(), ``)
/// 5. 不能包含输入重定向 (<)
/// 6. 不能包含换行符（防止命令注入）
/// 7. 不能包含 null 字节
///
/// 注意：`env` 和 `printenv` 已从允许列表中移除，因为可能泄露敏感环境变量。
fn is_read_only_command(cmd: &str) -> bool {
    // 快速拒绝：检查控制字符和危险字符
    if cmd.contains('\0') || cmd.contains('\n') || cmd.contains('\r') {
        return false;
    }

    let read_only_prefixes = [
        "ls",
        "find",
        "grep",
        "cat",
        "stat",
        "wc",
        "head",
        "tail",
        "git status",
        "git log",
        "git diff",
        "git show",
        "git branch",
        "echo",
        "pwd",
        "which",
        "whoami",
        "type",
        "file",
        "du",
        "tree",
    ];

    let cmd_trimmed = cmd.trim();
    let cmd_lower = cmd_trimmed.to_lowercase();

    // 安全检查：检测危险符号
    // 1. 管道 (|) - 可能将数据传递给写入命令
    // 2. 命令替换 ($(), ``) - 可能执行任意命令
    // 3. 分号 (;) 和逻辑运算符 (&&, ||) - 可能链接多个命令
    // 4. 后台执行 (&) - 可能在后台执行危险操作
    // 5. 输入重定向 (<) - 可能绕过路径限制读取外部文件
    // 6. 换行转义 (\n, \r) - 可能注入新命令
    let dangerous_patterns = [
        "|", "$(", "`", ";", "&&", "||", "&", "<", "\\n", "\\r", "\n", "\r",
    ];

    for pattern in &dangerous_patterns {
        if cmd_lower.contains(pattern) {
            return false;
        }
    }

    // 输出重定向 (>, >>) - 需要更精确的检测
    // 排除 >= (比较运算符) 和 => (箭头函数)
    // 简单策略: 如果 > 后面紧跟 =, 或前面紧跟 =, 则不是重定向
    if cmd_lower.contains('>') {
        // >> 追加重定向
        if cmd_lower.contains(">>") {
            return false;
        }
        // 检查每个 > 是否是重定向
        let bytes = cmd_lower.as_bytes();
        let len = bytes.len();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'>' {
                let next_is_eq = i + 1 < len && bytes[i + 1] == b'=';
                let prev_is_eq = i > 0 && bytes[i - 1] == b'=';
                // >= 或 => 不是重定向
                if next_is_eq || prev_is_eq {
                    continue;
                }
                // 其他 > 视为重定向
                return false;
            }
        }
    }

    // 检查是否以只读前缀开头
    // 使用更严格的匹配：命令必须紧随空格或结束
    read_only_prefixes.iter().any(|&prefix| {
        if cmd_lower == prefix {
            return true;
        }
        if cmd_lower.starts_with(prefix) {
            // 检查前缀后是否是空格或参数结束
            let after_prefix = &cmd_trimmed[prefix.len()..];
            after_prefix.starts_with(' ') || after_prefix.is_empty()
        } else {
            false
        }
    })
}

/// 检查是否在记忆目录内
///
/// 安全检查：解析符号链接，防止路径遍历攻击
fn is_auto_mem_path(path: &str, memory_dir: &Path) -> bool {
    let path = Path::new(path);

    // 首先检查直接路径前缀（快速路径）
    if path.starts_with(memory_dir) {
        // 如果路径存在，验证它不是符号链接或符号链接目标仍在目录内
        if path.exists() {
            if let Ok(canonical) = path.canonicalize() {
                // 确保解析后的路径仍在记忆目录内
                if let Ok(canonical_dir) = memory_dir.canonicalize() {
                    return canonical.starts_with(&canonical_dir);
                }
            }
        }
        // 路径不存在或无法解析，检查父目录
        if let Some(parent) = path.parent() {
            if parent.exists() {
                if let Ok(canonical_parent) = parent.canonicalize() {
                    if let Ok(canonical_dir) = memory_dir.canonicalize() {
                        return canonical_parent.starts_with(&canonical_dir);
                    }
                }
            }
        }
        // 路径以 memory_dir 开头但文件和父目录都不存在。
        // 如果没有路径遍历组件，且 memory_dir 本身存在，允许创建新文件。
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return false;
        }
        // memory_dir 必须存在才能允许在其下创建新文件
        return memory_dir.exists();
    }

    // 路径不以 memory_dir 开头，保守拒绝（安全优先）
    // 注意：移除了基于文件名的 fallback 检查，因为它允许访问任意位置的文件
    // 这是安全漏洞：攻击者可构造 /etc/user.md 等路径绕过目录限制
    false
}

/// 检查路径是否在指定目录内（安全版本，解析符号链接）
fn is_path_within_directory(path: &str, directory: &Path) -> bool {
    is_path_within_directory_path(Path::new(path), directory)
}

fn is_path_within_directory_path(path: &Path, directory: &Path) -> bool {
    // 快速路径：直接前缀检查
    if !path.starts_with(directory) {
        return false;
    }

    // 安全检查：解析符号链接
    // 如果路径存在，解析并验证
    if path.exists() {
        if let Ok(canonical_path) = path.canonicalize() {
            if let Ok(canonical_dir) = directory.canonicalize() {
                return canonical_path.starts_with(&canonical_dir);
            }
        }
    } else {
        // 路径不存在，检查父目录
        if let Some(parent) = path.parent() {
            if parent.exists() {
                if let Ok(canonical_parent) = parent.canonicalize() {
                    if let Ok(canonical_dir) = directory.canonicalize() {
                        return canonical_parent.starts_with(&canonical_dir);
                    }
                }
            }
        }
    }

    // 无法验证，保守拒绝
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── 工具名单单一来源（P1-0） ──────────────────────────────────
    //
    // 目标是消灭"三份名单各自漂移"这个类别。下面四个断言对应文档
    // `docs/design/ghost-review-cache-alignment-design.md` §7 验收标准第 6 条：
    //   Allow 集合 == schema 工具名集合，且 Deny 消息枚举与 Allow arm 一致。

    fn schema_names(mode: ReviewToolMode) -> Vec<String> {
        build_roster_schemas(mode)
            .iter()
            .map(|s| {
                s["function"]["name"]
                    .as_str()
                    .expect("schema must carry function.name")
                    .to_string()
            })
            .collect()
    }

    /// roster 与 schema_order 逐模式一致（含顺序）。
    /// 这是最核心的一条：一旦有人只改一侧，这里立刻红。
    #[test]
    fn roster_and_schemas_agree_per_mode() {
        for mode in ReviewToolMode::ALL {
            let roster: Vec<String> = mode.roster().iter().map(|s| s.to_string()).collect();
            assert_eq!(
                schema_names(mode),
                roster,
                "{:?}: schema 顺序/内容与 roster 不一致",
                mode
            );
        }
    }

    /// Flush 是单工具模式，单独覆盖（不在 `ALL` 里）。
    #[test]
    fn flush_roster_and_schema_agree() {
        assert_eq!(
            schema_names(ReviewToolMode::Flush),
            vec!["memory_manage".to_string()]
        );
    }

    /// `create_*_can_use_tool` 的 Allow 集合必须等于各自的 roster。
    #[test]
    fn guards_allow_exactly_their_roster() {
        for mode in ReviewToolMode::ALL {
            let guard = make_roster_guard(mode);
            for allowed in mode.roster() {
                assert!(
                    matches!(guard(allowed, &json!({})), ToolPermission::Allow),
                    "{:?}: roster 内的 {:?} 应被 Allow",
                    mode,
                    allowed
                );
            }
            // 名单外一律 Deny（取几个确定不在任何名单里的名字）
            for denied in ["exec", "shell", "write_file", "file_write", "web_fetch"] {
                assert!(
                    matches!(guard(denied, &json!({})), ToolPermission::Deny { .. }),
                    "{:?}: 名单外的 {:?} 应被 Deny",
                    mode,
                    denied
                );
            }
        }
    }

    /// Deny 消息必须完整枚举 Allow 集合——这是本次修正的真实缺陷
    /// （三条消息都漏列 `list_dir`）。
    #[test]
    fn deny_message_enumerates_full_roster() {
        for mode in ReviewToolMode::ALL {
            let guard = make_roster_guard(mode);
            let ToolPermission::Deny { message } = guard("definitely_not_a_tool", &json!({}))
            else {
                panic!("{:?}: 名单外工具应被 Deny", mode);
            };
            for allowed in mode.roster() {
                assert!(
                    message.contains(allowed),
                    "{:?}: Deny 消息漏列 {:?}（实际消息: {}）",
                    mode,
                    allowed,
                    message
                );
            }
            // 反向：消息里不应出现名单外的名字
            for outside in ["exec", "shell", "write_file"] {
                assert!(
                    !message.contains(outside),
                    "{:?}: Deny 消息多列了 {:?}（实际消息: {}）",
                    mode,
                    outside,
                    message
                );
            }
        }
    }

    /// 各模式的 `create_*` / `build_*` 公开 API 与 roster 派生结果等价
    /// （防止公开入口绕开 roster）。
    #[test]
    fn public_apis_are_roster_backed() {
        assert!(matches!(
            create_skill_review_can_use_tool()("list_dir", &json!({})),
            ToolPermission::Allow
        ));
        assert!(matches!(
            create_memory_review_can_use_tool()("list_dir", &json!({})),
            ToolPermission::Allow
        ));
        assert!(matches!(
            create_combined_review_can_use_tool()("list_dir", &json!({})),
            ToolPermission::Allow
        ));
        assert_eq!(schema_names(ReviewToolMode::Skill).len(), 6);
        assert_eq!(schema_names(ReviewToolMode::Memory).len(), 7);
        assert_eq!(schema_names(ReviewToolMode::Combined).len(), 9);
    }

    #[test]
    fn test_read_only_commands() {
        assert!(is_read_only_command("ls -la"));
        assert!(is_read_only_command("grep pattern file.txt"));
        assert!(is_read_only_command("git status"));
        assert!(is_read_only_command("cat file.txt"));
        assert!(!is_read_only_command("rm file.txt"));
        assert!(!is_read_only_command("npm install"));
    }

    #[test]
    fn test_read_only_command_redirect_detection() {
        // 输出重定向 - 应该被拒绝
        assert!(!is_read_only_command("echo hello > file.txt"));
        assert!(!is_read_only_command("echo hello >> file.txt"));
        assert!(!is_read_only_command("cat file > output.txt"));
        assert!(!is_read_only_command("ls -la > listing.txt"));

        // 管道 - 应该被拒绝
        assert!(!is_read_only_command("ls | grep foo"));
        assert!(!is_read_only_command("cat file | grep pattern"));

        // 命令替换 - 应该被拒绝
        assert!(!is_read_only_command("echo $(cat secret)"));
        assert!(!is_read_only_command("echo `whoami`"));

        // 输入重定向 - 应该被拒绝，避免绕过路径限制读取外部文件
        assert!(!is_read_only_command("cat < /etc/passwd"));

        // 分号和逻辑运算符 - 应该被拒绝
        assert!(!is_read_only_command("ls; rm file"));
        assert!(!is_read_only_command("ls && rm file"));
        assert!(!is_read_only_command("ls || rm file"));

        // 后台执行 - 应该被拒绝
        assert!(!is_read_only_command("ls &"));

        // 纯只读命令 - 应该通过
        assert!(is_read_only_command("ls -la"));
        assert!(is_read_only_command("echo hello"));
        assert!(is_read_only_command("cat file.txt"));
        assert!(is_read_only_command("grep -r pattern"));
        assert!(is_read_only_command("git status"));
    }

    #[test]
    fn test_memory_file_permission() {
        let memory_path = Path::new("/path/to/memory.md");
        let can_use = create_memory_file_can_use_tool(memory_path);

        // 允许编辑正确的文件
        let result = can_use("file_edit", &json!({"file_path": "/path/to/memory.md"}));
        assert!(matches!(result, ToolPermission::Allow));

        // 拒绝编辑其他文件
        let result = can_use("file_edit", &json!({"file_path": "/other/file.md"}));
        assert!(matches!(result, ToolPermission::Deny { .. }));

        // 拒绝其他工具
        let result = can_use("read_file", &json!({"file_path": "/path/to/memory.md"}));
        assert!(matches!(result, ToolPermission::Deny { .. }));
    }

    #[test]
    fn test_auto_mem_permission() {
        use std::fs;

        // 创建临时目录进行测试
        let temp_dir = std::env::temp_dir().join("blockcell_test_memory");
        fs::create_dir_all(&temp_dir).ok();
        let memory_dir = &temp_dir;

        let can_use = create_auto_mem_can_use_tool(memory_dir);

        // 允许读取记忆目录内的显式路径
        let memory_read_file = temp_dir.join("reference.md");
        let memory_read_file_str = memory_read_file.to_string_lossy();
        assert!(matches!(
            can_use(
                "read_file",
                &json!({"file_path": memory_read_file_str.as_ref()})
            ),
            ToolPermission::Allow
        ));

        // 拒绝读取记忆目录外的路径
        assert!(matches!(
            can_use("read_file", &json!({"file_path": "/any/file"})),
            ToolPermission::Deny { .. }
        ));

        // 允许受限的只读 shell 命令
        let memory_dir_str = temp_dir.to_string_lossy();
        assert!(matches!(
            can_use(
                "shell",
                &json!({"command": "ls -la", "working_dir": memory_dir_str.as_ref()})
            ),
            ToolPermission::Allow
        ));

        // 拒绝未指定受限工作目录的 shell 命令
        assert!(matches!(
            can_use("shell", &json!({"command": "ls -la"})),
            ToolPermission::Deny { .. }
        ));

        // 拒绝 shell 读取绝对路径
        assert!(matches!(
            can_use(
                "shell",
                &json!({"command": "cat /etc/passwd", "working_dir": memory_dir_str.as_ref()})
            ),
            ToolPermission::Deny { .. }
        ));

        // 拒绝写入 shell 命令
        assert!(matches!(
            can_use(
                "shell",
                &json!({"command": "rm file", "working_dir": memory_dir_str.as_ref()})
            ),
            ToolPermission::Deny { .. }
        ));

        // 允许在 memory 目录内写入（使用临时目录路径）
        let memory_file = temp_dir.join("user.md");
        let memory_file_str = memory_file.to_string_lossy();
        assert!(matches!(
            can_use("file_edit", &json!({"file_path": memory_file_str.as_ref()})),
            ToolPermission::Allow
        ));
        assert!(matches!(
            can_use("file_edit", &json!({"file_path": "user.md"})),
            ToolPermission::Allow
        ));

        // 拒绝在 memory 目录外写入
        assert!(matches!(
            can_use("file_edit", &json!({"file_path": "/other/file.md"})),
            ToolPermission::Deny { .. }
        ));

        // 清理临时目录
        fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn test_dream_read_tools_are_limited_to_memory_root() {
        use std::fs;

        let temp_dir = std::env::temp_dir().join("blockcell_test_dream_memory");
        fs::create_dir_all(&temp_dir).ok();
        let can_use = create_dream_can_use_tool(&temp_dir);

        let memory_file = temp_dir.join("project.md");
        let memory_file_str = memory_file.to_string_lossy();
        assert!(matches!(
            can_use("read_file", &json!({"file_path": memory_file_str.as_ref()})),
            ToolPermission::Allow
        ));

        assert!(matches!(
            can_use("read_file", &json!({"file_path": "/etc/passwd"})),
            ToolPermission::Deny { .. }
        ));

        assert!(matches!(
            can_use("glob", &json!({"pattern": "*.md"})),
            ToolPermission::Deny { .. }
        ));

        fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn test_dream_read_tools_allow_relative_paths_within_memory_root() {
        let temp_dir = tempfile::tempdir().unwrap();
        let memory_root = temp_dir.path();
        std::fs::write(memory_root.join("reference.md"), "memory").unwrap();
        let can_use = create_dream_can_use_tool(memory_root);

        assert!(matches!(
            can_use("list_dir", &json!({"path": "."})),
            ToolPermission::Allow
        ));
        assert!(matches!(
            can_use("read_file", &json!({"file_path": "reference.md"})),
            ToolPermission::Allow
        ));
        assert!(matches!(
            can_use("write_file", &json!({"file_path": "summary.md"})),
            ToolPermission::Allow
        ));
        assert!(matches!(
            can_use("edit_file", &json!({"file_path": "reference.md"})),
            ToolPermission::Allow
        ));

        assert!(matches!(
            can_use("list_dir", &json!({"path": ".."})),
            ToolPermission::Deny { .. }
        ));
        assert!(matches!(
            can_use("read_file", &json!({"file_path": "../secret.md"})),
            ToolPermission::Deny { .. }
        ));
        assert!(matches!(
            can_use("write_file", &json!({"file_path": "../secret.md"})),
            ToolPermission::Deny { .. }
        ));
    }

    #[test]
    fn test_path_traversal_security() {
        use std::fs;

        // 测试路径遍历安全：文件名匹配不应绕过目录边界检查
        // 使用临时目录确保 memory_dir 存在（is_auto_mem_path 要求目录存在才允许创建新文件）
        let temp_dir = std::env::temp_dir().join("blockcell_test_path_traversal");
        fs::create_dir_all(&temp_dir).ok();
        let memory_dir = &temp_dir;
        let memory_dir_str = memory_dir.to_str().unwrap();

        // 路径不在 memory_dir 内，即使文件名匹配也应返回 false
        assert!(!is_auto_mem_path("/etc/user.md", memory_dir));
        assert!(!is_auto_mem_path("/root/.ssh/project.md", memory_dir));
        assert!(!is_auto_mem_path("/var/log/feedback.md", memory_dir));
        assert!(!is_auto_mem_path("/tmp/reference.md", memory_dir));
        assert!(!is_auto_mem_path("/etc/cron.d/user.md", memory_dir));

        // 路径在 memory_dir 内且不含 .. 组件时，即使路径不存在也返回 true
        // 这是允许创建新文件的合理行为
        let new_file_path = format!("{}/user.md", memory_dir_str);
        let result = is_auto_mem_path(&new_file_path, memory_dir);
        assert!(
            result,
            "path under memory_dir without .. should be allowed for new file creation"
        );

        // 路径遍历攻击：含 .. 组件的路径即使在 memory_dir 前缀下也应被拒绝
        let traversal_path1 = format!("{}/../../../etc/passwd", memory_dir_str);
        assert!(!is_auto_mem_path(&traversal_path1, memory_dir));
        let traversal_path2 = format!("{}/../../root/.ssh/id_rsa", memory_dir_str);
        assert!(!is_auto_mem_path(&traversal_path2, memory_dir));

        // 清理临时目录
        fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn allowlist_policy_denies_everything_outside_the_list() {
        let guard =
            create_allowlist_can_use_tool(vec!["read_file".to_string(), "list_dir".to_string()]);

        assert!(matches!(
            guard("read_file", &json!({})),
            ToolPermission::Allow
        ));
        assert!(matches!(
            guard("list_dir", &json!({})),
            ToolPermission::Allow
        ));
        match guard("exec", &json!({})) {
            ToolPermission::Deny { message } => assert!(message.contains("read_file")),
            other => panic!("expected deny, got {:?}", other),
        }
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let guard = create_allowlist_can_use_tool(Vec::new());
        assert!(matches!(
            guard("read_file", &json!({})),
            ToolPermission::Deny { .. }
        ));
    }

    #[test]
    fn tool_names_are_extracted_from_both_schema_shapes() {
        let schemas = vec![
            json!({"type": "function", "function": {"name": "read_file"}}),
            json!({"name": "list_dir"}),
        ];
        assert_eq!(
            tool_names_from_schemas(&schemas),
            vec!["read_file".to_string(), "list_dir".to_string()]
        );
    }
}
