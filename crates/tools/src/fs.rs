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
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_file",
            description: "Read the contents of a local file. REQUIRED: always provide string parameter `path`; do not call this tool with `{}`. `path` may be an absolute path, `~/...`, or a workspace-relative file path such as `xhs_feeds.json` or `notes/todo.md`. Supports text files and Office documents (.xlsx, .xls, .docx, .pptx) — binary Office files are automatically parsed and returned as readable text/markdown.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read. Supports text files and Office formats (xlsx, xls, docx, pptx)."
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

            return Ok(json!({
                "path": path.display().to_string(),
                "format": path.extension().and_then(|e| e.to_str()).unwrap_or("unknown"),
                "content": content
            }));
        }

        let content = tokio::fs::read_to_string(&path).await?;
        Ok(json!({
            "path": path.display().to_string(),
            "content": content
        }))
    }
}

// ============ write_file ============

pub struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "write_file",
            description: "Write content to a local file, creating parent directories if needed. REQUIRED: always provide both string parameters `path` and `content`; do not call this tool with `{}` and do not omit either field. `path` may be absolute, `~/...`, or workspace-relative such as `generated/out.html`.",
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
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "edit_file",
            description: "Edit a local file by replacing `old_text` with `new_text`. REQUIRED: always provide `path`, `old_text`, and `new_text`; do not call this tool with `{}`. IMPORTANT: before calling this tool, read the target file and copy the exact existing text into `old_text` verbatim. `old_text` must match the file content exactly, including whitespace, indentation, and line breaks, and it must appear only once. Prefer a longer unique contiguous snippet rather than a short fragment.",
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
                    }
                },
                "required": ["path", "old_text", "new_text"]
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- **edit_file**: Always pass `path`, `old_text`, and `new_text`. Never call `edit_file` with `{}`. Before editing, call `read_file` on the target path and copy the exact existing text into `old_text` verbatim. `old_text` must match the file exactly, including spaces, indentation, and line breaks, and it must be unique in the file. If the edit fails with `old_text not found`, read the file again and choose a larger contiguous snippet around the target change.".to_string())
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("path").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: path".to_string(),
            ));
        }
        if params.get("old_text").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: old_text".to_string(),
            ));
        }
        if params.get("new_text").and_then(|v| v.as_str()).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: new_text".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let path_str = params["path"].as_str().unwrap();
        let old_text = params["old_text"].as_str().unwrap();
        let new_text = params["new_text"].as_str().unwrap();
        let path = expand_path(path_str, &ctx.workspace);

        if !path.exists() {
            return Err(Error::NotFound(format!(
                "File not found: {}",
                path.display()
            )));
        }

        let content = tokio::fs::read_to_string(&path).await?;

        let count = content.matches(old_text).count();
        if count == 0 {
            return Err(Error::Tool(format!(
                "bad parameter: old_text in {} not exists, please read_file firstly and then call this with correct old_text",
                path.display()
            )));
        }
        if count > 1 {
            return Err(Error::Tool(format!(
                "old_text appears {} times in file. Must be unique for safe editing.",
                count
            )));
        }

        let new_content = content.replacen(old_text, new_text, 1);
        tokio::fs::write(&path, &new_content).await?;

        Ok(json!({
            "path": path.display().to_string(),
            "status": "edited"
        }))
    }
}

pub struct ListDirTool;

#[async_trait]
impl Tool for ListDirTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_dir",
            description: "List contents of a directory. REQUIRED: always provide string parameter `path`; do not call this tool with `{}` and do not assume an implicit current directory. Use `{\"path\":\".\"}` for the current workspace directory, or pass an absolute / `~/...` / workspace-relative directory path explicitly.",
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
        assert!(read_schema.description.contains("do not call this tool with `{}`"));
        assert!(edit_schema.description.contains("do not call this tool with `{}`"));
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
