use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::{Tool, ToolContext, ToolSchema};

fn expand_path(path: &str, workspace: &std::path::Path) -> PathBuf {
    if path.starts_with("~/") {
        dirs::home_dir()
            .map(|h| h.join(&path[2..]))
            .unwrap_or_else(|| PathBuf::from(path))
    } else if path.starts_with('/') {
        PathBuf::from(path)
    } else {
        workspace.join(path)
    }
}

// ============ read_file ============

pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Safe
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_file".to_string(),
            description: "Read the contents of a local file. REQUIRED: always provide string parameter `path`; do not call this tool with `{}`. `path` may be an absolute path, `~/...`, or a workspace-relative file path such as `xhs_feeds.json` or `notes/todo.md`. Supports text files and Office documents (.xlsx, .xls, .docx, .pptx) — binary Office files are automatically parsed and returned as readable text/markdown.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read. Supports text files and Office formats (xlsx, xls, docx, pptx)."
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional 1-based first line to return"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional maximum number of lines to return"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- **read_file**: Always pass `path` explicitly. Never call `read_file` with `{}`. Use a concrete file path such as `{\"path\":\"xhs_feeds.json\"}` or `{\"path\":\"/absolute/path/file.md\"}`.".to_string())
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("path").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: path".to_string(),
            ));
        }
        if params
            .get("offset")
            .is_some_and(|value| value.as_u64().is_none_or(|offset| offset == 0))
        {
            return Err(Error::Validation(
                "offset must be a positive integer".to_string(),
            ));
        }
        if params
            .get("limit")
            .is_some_and(|value| value.as_u64().is_none_or(|limit| limit == 0))
        {
            return Err(Error::Validation(
                "limit must be a positive integer".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let path_str = params["path"].as_str().unwrap();
        let path = expand_path(path_str, &ctx.workspace);

        if !path.exists() {
            return Err(Error::NotFound(format!(
                "File not found: {}",
                path.display()
            )));
        }

        if !path.is_file() {
            return Err(Error::Tool(format!("Not a file: {}", path.display())));
        }

        // Handle office files (xlsx, xls, docx, pptx)
        if crate::office::is_office_file(&path) {
            let path_clone = path.clone();
            let content =
                tokio::task::spawn_blocking(move || crate::office::read_office_file(&path_clone))
                    .await
                    .map_err(|e| Error::Tool(format!("Failed to read office file: {}", e)))??;

            return Ok(read_result(
                &path,
                content,
                params.get("offset").and_then(Value::as_u64),
                params.get("limit").and_then(Value::as_u64),
                Some(
                    path.extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("unknown"),
                ),
            ));
        }

        let content = tokio::fs::read_to_string(&path).await?;
        Ok(read_result(
            &path,
            content,
            params.get("offset").and_then(Value::as_u64),
            params.get("limit").and_then(Value::as_u64),
            None,
        ))
    }
}

fn read_result(
    path: &std::path::Path,
    content: String,
    offset: Option<u64>,
    limit: Option<u64>,
    format: Option<&str>,
) -> Value {
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let start_index = offset.unwrap_or(1).saturating_sub(1) as usize;
    let start_index = start_index.min(total_lines);
    let end_index = limit
        .map(|limit| start_index.saturating_add(limit as usize).min(total_lines))
        .unwrap_or(total_lines);
    let selected_content = if offset.is_none() && limit.is_none() {
        content.clone()
    } else {
        lines[start_index..end_index].join("\n")
    };
    let mut result = json!({
        "path": path.display().to_string(),
        "content": selected_content,
        "start_line": if start_index < total_lines { start_index + 1 } else { 0 },
        "end_line": end_index,
        "total_lines": total_lines,
    });
    if let Some(format) = format {
        result["format"] = json!(format);
    }
    result
}

// ============ write_file ============

pub struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Required
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "write_file".to_string(),
            description: "Write content to a local file, creating parent directories if needed. REQUIRED: always provide both string parameters `path` and `content`; do not call this tool with `{}` and do not omit either field. `path` may be absolute, `~/...`, or workspace-relative such as `generated/out.html`.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- **write_file**: Always pass both `path` and `content`. Never call `write_file` with `{}` or with only one field. Example: `{\"path\":\"generated/out.html\",\"content\":\"<html>...</html>\"}`.".to_string())
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("path").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: path".to_string(),
            ));
        }
        if params.get("content").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: content".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let path_str = params["path"].as_str().unwrap();
        let content = params["content"].as_str().unwrap();
        let path = expand_path(path_str, &ctx.workspace);

        // Create parent directories
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let bytes_written = content.len();
        tokio::fs::write(&path, content).await?;

        Ok(json!({
            "path": path.display().to_string(),
            "bytes_written": bytes_written
        }))
    }
}

// ============ edit_file ============

pub struct EditFileTool;

#[async_trait]
impl Tool for EditFileTool {
    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Required
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "edit_file".to_string(),
            description: "Edit a local file with one replacement or an atomic `edits` array; do not call this tool with `{}`. Accepts `old_text`/`new_text` and `old_string`/`new_string` aliases. Every old value must be a unique exact match; no file is written unless every edit succeeds.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit"
                    },
                    "old_text": {
                        "type": "string",
                        "description": "Text to find and replace (must match exactly)"
                    },
                    "new_text": {
                        "type": "string",
                        "description": "Text to replace old_text with"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "Alias for old_text"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "Alias for new_text"
                    },
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "description": "Ordered replacements applied atomically",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_text": {"type": "string"},
                                "new_text": {"type": "string"},
                                "old_string": {"type": "string"},
                                "new_string": {"type": "string"}
                            }
                        }
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- **edit_file**: Always pass `path` plus either one `old_text`/`new_text` pair (aliases: `old_string`/`new_string`) or an `edits` array. Read the target first. Multi-edit calls are atomic: every old value must match uniquely before the file is written.".to_string())
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("path").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: path".to_string(),
            ));
        }
        let edits = parse_edits(params)?;
        if edits.is_empty() {
            return Err(Error::Validation("edits must not be empty".to_string()));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let path_str = params["path"].as_str().unwrap();
        let edits = parse_edits(&params)?;
        let path = expand_path(path_str, &ctx.workspace);

        if !path.exists() {
            return Err(Error::NotFound(format!(
                "File not found: {}",
                path.display()
            )));
        }

        let original = tokio::fs::read_to_string(&path).await?;
        let mut new_content = original.clone();

        for (index, edit) in edits.iter().enumerate() {
            let count = new_content.matches(edit.old_text).count();
            if count == 0 {
                let closest =
                    crate::fuzzy_match::closest_line_context(&new_content, edit.old_text, 5)
                        .unwrap_or_else(|| "<empty file>".to_string());
                return Err(Error::Tool(format!(
                    "edit {} failed: old_text not found in {}\nclosest matching context (±5 lines):\n{}",
                    index + 1,
                    path.display(),
                    closest
                )));
            }
            if count > 1 {
                return Err(Error::Tool(format!(
                    "edit {} failed: old_text appears {} times in file; provide more context so it is unique",
                    index + 1,
                    count
                )));
            }
            new_content = new_content.replacen(edit.old_text, edit.new_text, 1);
        }
        tokio::fs::write(&path, &new_content).await?;

        Ok(json!({
            "path": path.display().to_string(),
            "status": "edited",
            "edits_applied": edits.len()
        }))
    }
}

struct Edit<'a> {
    old_text: &'a str,
    new_text: &'a str,
}

fn parse_edits(params: &Value) -> Result<Vec<Edit<'_>>> {
    if let Some(edits) = params.get("edits") {
        let edits = edits
            .as_array()
            .filter(|edits| !edits.is_empty())
            .ok_or_else(|| Error::Validation("edits must be a non-empty array".to_string()))?;
        return edits
            .iter()
            .enumerate()
            .map(|(index, edit)| parse_edit(edit, Some(index + 1)))
            .collect();
    }
    parse_edit(params, None).map(|edit| vec![edit])
}

fn parse_edit(params: &Value, index: Option<usize>) -> Result<Edit<'_>> {
    let label = index
        .map(|index| format!("edit {index}"))
        .unwrap_or_else(|| "edit".to_string());
    let old_text = params
        .get("old_text")
        .and_then(Value::as_str)
        .or_else(|| params.get("old_string").and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Error::Validation(format!("{label} requires non-empty old_text or old_string"))
        })?;
    let new_text = params
        .get("new_text")
        .and_then(Value::as_str)
        .or_else(|| params.get("new_string").and_then(Value::as_str))
        .ok_or_else(|| Error::Validation(format!("{label} requires new_text or new_string")))?;
    Ok(Edit { old_text, new_text })
}

pub struct ListDirTool;

#[async_trait]
impl Tool for ListDirTool {
    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Safe
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_dir".to_string(),
            description: "List contents of a directory. REQUIRED: always provide string parameter `path`; do not call this tool with `{}` and do not assume an implicit current directory. Use `{\"path\":\".\"}` for the current workspace directory, or pass an absolute / `~/...` / workspace-relative directory path explicitly.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Required. Absolute path, ~/path, or workspace-relative path to the directory to list. No default value."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- **list_dir**: Always pass `path` explicitly. Never call `list_dir` with `{}`. For the current workspace directory, use exactly `{\"path\":\".\"}`.".to_string())
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("path").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: path".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let path_str = params["path"].as_str().unwrap();
        let path = expand_path(path_str, &ctx.workspace);

        if !path.exists() {
            return Err(Error::NotFound(format!(
                "Directory not found: {}",
                path.display()
            )));
        }

        if !path.is_dir() {
            return Err(Error::Tool(format!("Not a directory: {}", path.display())));
        }

        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(&path).await?;

        while let Some(entry) = read_dir.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            let file_type = entry.file_type().await?;
            let kind = if file_type.is_dir() {
                "directory"
            } else if file_type.is_file() {
                "file"
            } else {
                "other"
            };
            entries.push(json!({
                "name": name,
                "type": kind
            }));
        }

        Ok(json!({
            "path": path.display().to_string(),
            "entries": entries
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PromptContext;
    use serde_json::json;
    use tempfile::tempdir;

    fn tool_context(workspace: &std::path::Path) -> ToolContext {
        ToolContext {
            workspace: workspace.to_path_buf(),
            base: workspace.to_path_buf(),
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "test-session".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: None,
            chat_id: "test-chat".to_string(),
            config: blockcell_core::Config::default(),
            permissions: crate::PermissionSet::new(),
            task_manager: None,
            memory_store: None,
            memory_file_store: None,
            ghost_memory_lifecycle: None,
            skill_file_store: None,
            session_search: None,
            outbound_tx: None,
            spawn_handle: None,
            capability_registry: None,
            core_evolution: None,
            event_emitter: None,
            channel_contacts_file: None,
            response_cache: None,
            runtime_handle: None,
            agent_identity: None,
            skill_mutex: None,
            agent_type_registry: None,
            evolution_workflow_store: None,
        }
    }

    #[tokio::test]
    async fn read_file_supports_one_based_offset_and_limit() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("sample.txt"), "one\ntwo\nthree\nfour\n").unwrap();

        let result = ReadFileTool
            .execute(
                tool_context(dir.path()),
                json!({"path": "sample.txt", "offset": 2, "limit": 2}),
            )
            .await
            .unwrap();

        assert_eq!(result["content"], "two\nthree");
        assert_eq!(result["start_line"], 2);
        assert_eq!(result["end_line"], 3);
        assert_eq!(result["total_lines"], 4);
    }

    #[tokio::test]
    async fn edit_file_accepts_old_string_and_new_string_aliases() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        std::fs::write(&path, "before\n").unwrap();

        EditFileTool
            .execute(
                tool_context(dir.path()),
                json!({"path": "sample.txt", "old_string": "before", "new_string": "after"}),
            )
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(path).unwrap(), "after\n");
    }

    #[tokio::test]
    async fn edit_file_applies_multiple_edits_atomically() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

        let result = EditFileTool
            .execute(
                tool_context(dir.path()),
                json!({
                    "path": "sample.txt",
                    "edits": [
                        {"old_text": "alpha", "new_text": "one"},
                        {"old_string": "gamma", "new_string": "three"}
                    ]
                }),
            )
            .await
            .unwrap();

        assert_eq!(result["edits_applied"], 2);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "one\nbeta\nthree\n");
    }

    #[tokio::test]
    async fn edit_file_reports_failed_edit_and_leaves_file_unchanged() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        let original = "header\nalpha value\nbeta value\ngamma value\nfooter\n";
        std::fs::write(&path, original).unwrap();

        let error = EditFileTool
            .execute(
                tool_context(dir.path()),
                json!({
                    "path": "sample.txt",
                    "edits": [
                        {"old_text": "header", "new_text": "changed"},
                        {"old_text": "beta values", "new_text": "replacement"}
                    ]
                }),
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("edit 2"), "{error}");
        assert!(error.contains("beta value"), "{error}");
        assert!(error.contains("closest"), "{error}");
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }

    #[test]
    fn enhanced_file_schemas_expose_ranges_aliases_and_edits() {
        let read = ReadFileTool.schema();
        let edit = EditFileTool.schema();
        assert!(read.parameters["properties"].get("offset").is_some());
        assert!(read.parameters["properties"].get("limit").is_some());
        assert!(edit.parameters["properties"].get("old_string").is_some());
        assert!(edit.parameters["properties"].get("new_string").is_some());
        assert_eq!(edit.parameters["properties"]["edits"]["type"], "array");
    }

    #[test]
    fn test_read_file_schema() {
        let tool = ReadFileTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "read_file");
    }

    #[test]
    fn test_read_file_validate() {
        let tool = ReadFileTool;
        assert!(tool.validate(&json!({"path": "/tmp/test.txt"})).is_ok());
        assert!(tool.validate(&json!({})).is_err());
    }

    #[test]
    fn test_write_file_schema() {
        let tool = WriteFileTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "write_file");
    }

    #[test]
    fn test_write_file_validate() {
        let tool = WriteFileTool;
        assert!(tool
            .validate(&json!({"path": "/tmp/t.txt", "content": "hi"}))
            .is_ok());
        assert!(tool.validate(&json!({"path": "/tmp/t.txt"})).is_err());
        assert!(tool.validate(&json!({"content": "hi"})).is_err());
    }

    #[test]
    fn test_edit_file_schema() {
        let tool = EditFileTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "edit_file");
    }

    #[test]
    fn test_edit_file_validate() {
        let tool = EditFileTool;
        assert!(tool
            .validate(&json!({"path": "f", "old_text": "a", "new_text": "b"}))
            .is_ok());
        assert!(tool
            .validate(&json!({"path": "f", "old_text": "a"}))
            .is_err());
    }

    #[test]
    fn test_list_dir_schema() {
        let tool = ListDirTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "list_dir");
    }

    #[test]
    fn test_list_dir_validate() {
        let tool = ListDirTool;
        assert!(tool.validate(&json!({"path": "/tmp"})).is_ok());
        assert!(tool.validate(&json!({})).is_err());
    }

    #[test]
    fn test_list_dir_prompt_rule_requires_explicit_path_and_current_dir_example() {
        let tool = ListDirTool;
        let rule = tool
            .prompt_rule(&PromptContext {
                channel: "webui",
                intents: &[],
                default_timezone: None,
            })
            .expect("list_dir should expose a prompt rule");
        assert!(rule.contains("`path`"));
        assert!(rule.contains("{\"path\":\".\"}"));
        assert!(rule.contains("`{}`"));
    }

    #[test]
    fn test_write_file_prompt_rule_requires_path_and_content() {
        let tool = WriteFileTool;
        let rule = tool
            .prompt_rule(&PromptContext {
                channel: "webui",
                intents: &[],
                default_timezone: None,
            })
            .expect("write_file should expose a prompt rule");
        assert!(rule.contains("`path`"));
        assert!(rule.contains("`content`"));
        assert!(rule.contains("{\"path\":"));
        assert!(rule.contains("`{}`"));
    }

    #[test]
    fn test_read_and_edit_file_schemas_warn_against_empty_args() {
        let read_schema = ReadFileTool.schema();
        let edit_schema = EditFileTool.schema();
        assert!(read_schema
            .description
            .contains("do not call this tool with `{}`"));
        assert!(edit_schema
            .description
            .contains("do not call this tool with `{}`"));
    }

    #[test]
    fn test_expand_path_absolute() {
        let ws = std::path::PathBuf::from("/workspace");
        assert_eq!(
            expand_path("/etc/hosts", &ws),
            std::path::PathBuf::from("/etc/hosts")
        );
    }

    #[test]
    fn test_expand_path_relative() {
        let ws = std::path::PathBuf::from("/workspace");
        assert_eq!(
            expand_path("foo/bar.txt", &ws),
            std::path::PathBuf::from("/workspace/foo/bar.txt")
        );
    }

    #[test]
    fn test_expand_path_tilde() {
        let ws = std::path::PathBuf::from("/workspace");
        let expanded = expand_path("~/test.txt", &ws);
        assert!(expanded.to_string_lossy().contains("test.txt"));
        assert!(!expanded.starts_with("/workspace"));
    }
}
