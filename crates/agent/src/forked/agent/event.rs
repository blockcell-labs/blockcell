use super::*;

/// Safely truncate a string at a UTF-8 character boundary, appending "..." if truncated.
/// Avoids panics from slicing at non-character boundaries (e.g., CJK text).
pub(crate) fn truncate_str_safe(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut boundary = max_len;
        while boundary > 0 && !s.is_char_boundary(boundary) {
            boundary -= 1;
        }
        format!("{}...", &s[..boundary])
    }
}

/// 从工具参数中提取摘要信息，用于控制台实时显示。
///
/// 例如 read_file → "src/main.rs", grep → "pattern='TODO'", write_file → "config.json"
pub(crate) fn extract_tool_summary(tool_name: &str, input: &serde_json::Value) -> String {
    let obj = match input.as_object() {
        Some(o) => o,
        None => return String::new(),
    };

    match tool_name {
        "read_file" | "file_edit" | "edit_file" => {
            // 显示文件路径
            obj.get("path")
                .or_else(|| obj.get("file_path"))
                .and_then(|v| v.as_str())
                .map(truncate_path)
                .unwrap_or_default()
        }
        "write_file" | "file_write" => obj
            .get("path")
            .or_else(|| obj.get("file_path"))
            .and_then(|v| v.as_str())
            .map(truncate_path)
            .unwrap_or_default(),
        "grep" | "search" => {
            // 显示搜索模式
            let pattern = obj.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let path = obj.get("path").and_then(|v| v.as_str()).map(truncate_path);
            if let Some(p) = path {
                format!("\"{}\" in {}", pattern, p)
            } else {
                format!("\"{}\"", pattern)
            }
        }
        "glob" => obj
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|p| format!("\"{}\"", p))
            .unwrap_or_default(),
        "exec" | "exec_local" => {
            obj.get("command")
                .and_then(|v| v.as_str())
                .map(|c| {
                    // 只显示命令的第一行/前60字符
                    let first_line = c.lines().next().unwrap_or(c);
                    truncate_str_safe(first_line, 60)
                })
                .unwrap_or_default()
        }
        "web_search" | "web_fetch" => obj
            .get("query")
            .or_else(|| obj.get("url"))
            .and_then(|v| v.as_str())
            .map(|q| truncate_str_safe(q, 80))
            .unwrap_or_default(),
        "list_dir" => obj
            .get("path")
            .and_then(|v| v.as_str())
            .map(truncate_path)
            .unwrap_or_default(),
        _ => {
            // 通用：尝试提取最常见的参数名
            for key in &[
                "path",
                "file_path",
                "query",
                "url",
                "name",
                "command",
                "pattern",
                "message",
            ] {
                if let Some(v) = obj.get(*key).and_then(|v| v.as_str()) {
                    return truncate_str_safe(v, 80);
                }
            }
            String::new()
        }
    }
}

/// 截断路径，保留最后两级目录 + 文件名
pub(crate) fn truncate_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    if parts.len() <= 3 {
        path.to_string()
    } else {
        format!(".../{}", parts[parts.len() - 3..].join("/"))
    }
}

/// 构建 Forked Agent 可用工具的 schema 定义。
///
/// 返回 OpenAI function-calling 格式的工具 schema 列表，
/// 根据 disallowed_tools 过滤掉不允许的工具。
///
/// 支持的工具：read_file, list_dir, grep, glob, file_edit, edit_file, file_write, write_file
pub fn default_read_only_fork_disallowed_tools() -> Vec<String> {
    [
        "agent",
        "spawn",
        "cancel_task",
        "exec",
        "shell",
        "edit_file",
        "file_edit",
        "write_file",
        "file_write",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

pub fn build_forked_tool_schemas(disallowed_tools: &[String]) -> Vec<serde_json::Value> {
    use serde_json::json;

    let all_schemas = vec![
        // read_file
        json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read the contents of a file. Returns the file content as text, truncated if too large.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The file path to read. Prefer a relative path resolved from the agent working directory, such as 'reference.md'. Do not prefix paths with 'memory/'. Absolute paths are allowed only when they remain within the allowed directory."
                        }
                    },
                    "required": ["file_path"]
                }
            }
        }),
        // list_dir
        json!({
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List the contents of a directory. Returns file and directory names with type indicators (/ for directories).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The directory path to list. Prefer a relative path resolved from the agent working directory, such as '.'. Do not prefix paths with 'memory/'. Absolute paths are allowed only when they remain within the allowed directory."
                        }
                    },
                    "required": ["path"]
                }
            }
        }),
        // grep
        json!({
            "type": "function",
            "function": {
                "name": "grep",
                "description": "Search for a pattern in a file. Returns matching lines (up to 100).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "The text pattern to search for."
                        },
                        "path": {
                            "type": "string",
                            "description": "The file path to search in. Prefer a relative path resolved from the agent working directory. Do not prefix paths with 'memory/'."
                        }
                    },
                    "required": ["pattern", "path"]
                }
            }
        }),
        // glob
        json!({
            "type": "function",
            "function": {
                "name": "glob",
                "description": "Find files matching a pattern in a directory. Supports basic wildcards like *.rs, src*, etc.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "The glob pattern to match (e.g. '*.rs', 'src*')."
                        },
                        "path": {
                            "type": "string",
                            "description": "The directory path to search in. Prefer a relative path resolved from the agent working directory, such as '.'. Do not prefix paths with 'memory/'. Absolute paths are allowed only when they remain within the allowed directory."
                        }
                    },
                    "required": ["pattern", "path"]
                }
            }
        }),
        // edit_file
        json!({
            "type": "function",
            "function": {
                "name": "edit_file",
                "description": "Edit a file by replacing a unique string with a new string. The old_string must appear exactly once in the file unless replace_all is true.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The file path to edit. Prefer a relative path resolved from the agent working directory. Do not prefix paths with 'memory/'."
                        },
                        "old_string": {
                            "type": "string",
                            "description": "The exact text to find and replace. Must be unique in the file unless replace_all is true."
                        },
                        "new_string": {
                            "type": "string",
                            "description": "The text to replace old_string with."
                        },
                        "replace_all": {
                            "type": "boolean",
                            "description": "If true, replace ALL occurrences of old_string. Default: false."
                        }
                    },
                    "required": ["file_path", "old_string", "new_string"]
                }
            }
        }),
        // write_file
        json!({
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Write content to a file. Creates parent directories if needed.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The file path to write. Prefer a relative path resolved from the agent working directory. Do not prefix paths with 'memory/'."
                        },
                        "content": {
                            "type": "string",
                            "description": "The content to write to the file."
                        }
                    },
                    "required": ["file_path", "content"]
                }
            }
        }),
        // shell
        json!({
            "type": "function",
            "function": {
                "name": "shell",
                "description": "Execute a shell command. Use for explicit verification commands such as cargo check or targeted test runs.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute."
                        },
                        "working_dir": {
                            "type": "string",
                            "description": "Optional working directory for the command. Relative paths resolve within the agent working directory when isolated."
                        }
                    },
                    "required": ["command"]
                }
            }
        }),
        // legacy exec compatibility
        json!({
            "type": "function",
            "function": {
                "name": "exec",
                "description": "Compatibility alias for shell command execution.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"},
                        "working_dir": {"type": "string"}
                    },
                    "required": ["command"]
                }
            }
        }),
    ];

    // Filter out disallowed tools
    all_schemas
        .into_iter()
        .filter(|schema| {
            tool_schema_name(schema)
                .map(|name| !disallowed_tools.iter().any(|d| d == name))
                .unwrap_or(false)
        })
        .collect()
}
