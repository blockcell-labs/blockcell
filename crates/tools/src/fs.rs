use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

use crate::fs_backend::{FilesystemBackend, FsEntryKind, LocalFilesystem};
use crate::{expand_path, Tool, ToolContext, ToolSchema};

/// 文件工具通过注入的 [`FilesystemBackend`] 访问文件，工具本身只负责路径解析与参数语义。
macro_rules! filesystem_tool {
    ($name:ident) => {
        pub struct $name {
            fs: Arc<dyn FilesystemBackend>,
        }

        impl $name {
            pub fn new(fs: Arc<dyn FilesystemBackend>) -> Self {
                Self { fs }
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new(Arc::new(LocalFilesystem))
            }
        }
    };
}

filesystem_tool!(ReadFileTool);
filesystem_tool!(WriteFileTool);
filesystem_tool!(EditFileTool);
filesystem_tool!(ListDirTool);

// ============ read_file ============

#[async_trait]
impl Tool for ReadFileTool {
    fn coding_exposure(&self) -> crate::base_packs::CodingExposure {
        crate::base_packs::CodingExposure::primary()
    }

    fn execution_environment(&self, _params: &Value) -> crate::ExecutionEnvironment {
        crate::ExecutionEnvironment::Local
    }

    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Safe
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_file".to_string(),
            description:
                "Read via path or up to 10 paths, not both; do not call this tool with `{}`. Data files (csv/tsv/jsonl/ndjson/log) default to a 20-line, 4000-character sample; set limit explicitly for more."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 1,
                        "maxItems": 10
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 1
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum lines to return. Set explicitly to override default data-file sampling."
                    }
                },
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- **read_file**: Use `path` or `paths` (up to 10), not both. Data files default to a small sample; process the original file with a script, not the sample. Use explicit `offset`/`limit` only when more rows are necessary.".to_string())
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let path_key_present = params.get("path").is_some();
        let path = params.get("path").and_then(Value::as_str);
        let paths = params.get("paths").and_then(Value::as_array);
        if path_key_present == paths.is_some() || (path_key_present && path.is_none()) {
            return Err(Error::Validation(
                "Provide exactly one of path or paths".to_string(),
            ));
        }
        if let Some(paths) = paths {
            if paths.is_empty() || paths.len() > 10 || paths.iter().any(|value| !value.is_string())
            {
                return Err(Error::Validation(
                    "paths must contain between 1 and 10 strings".to_string(),
                ));
            }
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
        let paths: Vec<&str> = if let Some(path) = params.get("path").and_then(Value::as_str) {
            vec![path]
        } else {
            params["paths"]
                .as_array()
                .unwrap()
                .iter()
                .map(|path| path.as_str().unwrap())
                .collect()
        };
        let mut results = Vec::with_capacity(paths.len());
        for path in paths {
            results.push(
                read_file_result(
                    self.fs.as_ref(),
                    &ctx.workspace,
                    path,
                    params.get("offset").and_then(Value::as_u64),
                    params.get("limit").and_then(Value::as_u64),
                )
                .await?,
            );
        }
        if params.get("path").is_some() {
            Ok(results.pop().unwrap())
        } else {
            Ok(json!({"files": results}))
        }
    }
}

async fn read_file_result(
    fs: &dyn FilesystemBackend,
    workspace: &Path,
    path_str: &str,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<Value> {
    let path = expand_path(path_str, workspace);
    match fs.kind(&path).await? {
        None => {
            return Err(Error::NotFound(format!(
                "File not found: {}",
                path.display()
            )))
        }
        Some(FsEntryKind::File) => {}
        Some(_) => return Err(Error::Tool(format!("Not a file: {}", path.display()))),
    }

    if !crate::office::is_office_file(&path) {
        const MAX_READ_BYTES: u64 = 32 * 1024 * 1024;
        if let Some(size) = fs.size(&path).await? {
            if size > MAX_READ_BYTES {
                return Err(Error::Validation(format!(
                    "File is too large to read as text ({} bytes, limit {}). \
                     Use grep/code_search or read a slice with another tool.",
                    size, MAX_READ_BYTES
                )));
            }
        }
    }

    let content = fs.read_document(&path).await?;
    let format = crate::office::is_office_file(&path).then(|| {
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("unknown")
    });
    Ok(read_result(&path, content, offset, limit, format))
}

fn read_result(
    path: &std::path::Path,
    content: String,
    offset: Option<u64>,
    limit: Option<u64>,
    format: Option<&str>,
) -> Value {
    // Record-oriented data is normally processed by a script, not copied into
    // every subsequent model request. Explicit limits retain deliberate reads.
    let sample_data = limit.is_none()
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "csv" | "tsv" | "jsonl" | "ndjson" | "log"
                )
            });
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    let start_index = offset.unwrap_or(1).saturating_sub(1) as usize;
    let start_index = start_index.min(total_lines);
    let effective_limit = limit.or(sample_data.then_some(20));
    let mut end_index = effective_limit
        .map(|limit| start_index.saturating_add(limit as usize).min(total_lines))
        .unwrap_or(total_lines);
    let mut selected_content = if offset.is_none() && limit.is_none() && end_index == total_lines {
        content.clone()
    } else {
        lines[start_index..end_index].join("\n")
    };
    let char_truncated = sample_data && selected_content.chars().count() > 4000;
    if char_truncated {
        selected_content = crate::safe_truncate(&selected_content, 4000).to_string();
        end_index = start_index + selected_content.lines().count();
    }
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
    if sample_data {
        let truncated = end_index < total_lines || char_truncated;
        result["truncated"] = json!(truncated);
        if truncated {
            // Do not skip a partially returned record. An explicit limit can
            // retrieve that whole line, even when it exceeds the sample cap.
            let next_offset = if char_truncated && !selected_content.ends_with('\n') {
                end_index
            } else {
                end_index + 1
            };
            result["next_offset"] = json!(next_offset);
            result["hint"] = json!("Sample only. Process the original file with one script and validate the output there. Do not infer totals from this sample or page through all rows unnecessarily. For a required full row/range, set offset and limit explicitly; next_offset may repeat a partially returned line.");
        }
    }
    result
}

async fn write_file_result(
    fs: &dyn FilesystemBackend,
    workspace: &Path,
    path_str: &str,
    content: &str,
) -> Result<Value> {
    let path = expand_path(path_str, workspace);
    let _write_guard = fs.lock_for_write(&path).await?;
    fs.write(&path, content).await?;
    Ok(json!({
        "path": path.display().to_string(),
        "bytes_written": content.len()
    }))
}

// ============ write_file ============

#[async_trait]
impl Tool for WriteFileTool {
    fn coding_exposure(&self) -> crate::base_packs::CodingExposure {
        crate::base_packs::CodingExposure::primary()
    }

    fn accepts_execution_annotations(&self) -> bool {
        true
    }

    fn execution_environment(&self, _params: &Value) -> crate::ExecutionEnvironment {
        crate::ExecutionEnvironment::WorkspaceWrite
    }

    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Required
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "write_file".to_string(),
            description: "Write one file or up to 10 files; never mix.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"},
                    "files": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 10,
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {"type": "string"},
                                "content": {"type": "string"}
                            },
                            "required": ["path", "content"]
                        }
                    }
                },
                "oneOf": [
                    {"required": ["path", "content"]},
                    {"required": ["files"]}
                ]
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- **write_file**: Use `path`+`content` or up to 10 `files`; batch all new-project files. Never call with `{}`. Example: `{\"path\":\"out.txt\",\"content\":\"text\"}`. To change an existing file, use `read_file` first so its current version is recorded. If a write returns FS_NOT_OBSERVED, follow its read_file instruction before retrying; the diagnostic preview alone does not record an observation. Never replace a whole file based on a truncated preview.".to_string())
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if let Some(files) = params.get("files") {
            if params.get("path").is_some() || params.get("content").is_some() {
                return Err(Error::Validation(
                    "files is mutually exclusive with path and content".to_string(),
                ));
            }
            let files = files
                .as_array()
                .filter(|files| !files.is_empty() && files.len() <= 10)
                .ok_or_else(|| {
                    Error::Validation("files must contain between 1 and 10 objects".to_string())
                })?;
            if files.iter().any(|file| {
                file.get("path").and_then(Value::as_str).is_none()
                    || file.get("content").and_then(Value::as_str).is_none()
            }) {
                return Err(Error::Validation(
                    "each files entry requires string path and content".to_string(),
                ));
            }
            return Ok(());
        }
        if params
            .get("path")
            .and_then(|value| value.as_str())
            .is_none()
        {
            return Err(Error::Validation(
                "Missing required parameter: path".to_string(),
            ));
        }
        if params
            .get("content")
            .and_then(|value| value.as_str())
            .is_none()
        {
            return Err(Error::Validation(
                "Missing required parameter: content".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        if let Some(files) = params.get("files").and_then(Value::as_array) {
            let mut results = Vec::with_capacity(files.len());
            for file in files {
                let path = file["path"].as_str().unwrap();
                let content = file["content"].as_str().unwrap();
                match write_file_result(self.fs.as_ref(), &ctx.workspace, path, content).await {
                    Ok(result) => results.push(json!({"ok": true, "result": result})),
                    Err(error) => results.push(json!({
                        "ok": false,
                        "path": path,
                        "error": error.to_string()
                    })),
                }
            }
            return Ok(json!({"files": results}));
        }
        let path_str = params["path"].as_str().unwrap();
        let content = params["content"].as_str().unwrap();
        write_file_result(self.fs.as_ref(), &ctx.workspace, path_str, content).await
    }
}

// ============ edit_file ============

#[async_trait]
impl Tool for EditFileTool {
    fn coding_exposure(&self) -> crate::base_packs::CodingExposure {
        crate::base_packs::CodingExposure::primary()
    }

    fn accepts_execution_annotations(&self) -> bool {
        true
    }

    fn execution_environment(&self, _params: &Value) -> crate::ExecutionEnvironment {
        crate::ExecutionEnvironment::WorkspaceWrite
    }

    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Required
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "edit_file".to_string(),
            description: "Replace exact old_text once; do not call this tool with `{}`."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_text": {"type": "string"},
                    "new_text": {"type": "string"},
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_text": {"type": "string"},
                                "new_text": {"type": "string"}
                            }
                        }
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some(
            "- **edit_file**: Read first; `old_text` must match exactly. Edits are atomic."
                .to_string(),
        )
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
        let _write_guard = self.fs.lock_for_write(&path).await?;

        if self.fs.kind(&path).await?.is_none() {
            return Err(Error::NotFound(format!(
                "File not found: {}",
                path.display()
            )));
        }

        let original = self.fs.read_to_string(&path).await?;
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
        self.fs.write(&path, &new_content).await?;

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

#[async_trait]
impl Tool for ListDirTool {
    fn coding_exposure(&self) -> crate::base_packs::CodingExposure {
        crate::base_packs::CodingExposure::full()
    }

    fn execution_environment(&self, _params: &Value) -> crate::ExecutionEnvironment {
        crate::ExecutionEnvironment::Local
    }

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

        match self.fs.kind(&path).await? {
            None => {
                return Err(Error::NotFound(format!(
                    "Directory not found: {}",
                    path.display()
                )));
            }
            Some(FsEntryKind::Directory) => {}
            Some(_) => {
                return Err(Error::Tool(format!("Not a directory: {}", path.display())));
            }
        }

        let entries: Vec<Value> = self
            .fs
            .list_dir(&path)
            .await?
            .into_iter()
            .filter(|entry| {
                entry.kind.as_str() != "directory"
                    || !matches!(
                        entry.name.as_str(),
                        ".git" | "__pycache__" | "node_modules" | "target" | ".blockcell"
                    )
            })
            .map(|entry| {
                json!({
                    "name": entry.name,
                    "type": entry.kind.as_str()
                })
            })
            .collect();

        Ok(json!({
            "path": path.display().to_string(),
            "entries": entries
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn efficiency_regression_default_data_reads_sample_but_explicit_ranges_recover_rows() {
        let path = std::path::Path::new("events.jsonl");
        let content = (1..=100)
            .map(|n| format!("{{\"id\":{n},\"name\":\"用户\"}}\n"))
            .collect::<String>();
        let sampled = read_result(path, content.clone(), None, None, None);
        assert_eq!(sampled["content"].as_str().unwrap().lines().count(), 20);
        assert_eq!(sampled["start_line"], 1);
        assert_eq!(sampled["end_line"], 20);
        assert_eq!(sampled["total_lines"], 100);
        assert_eq!(sampled["truncated"], true);
        assert_eq!(sampled["next_offset"], 21);
        assert!(sampled["hint"].as_str().unwrap().contains("original file"));
        let remaining = read_result(path, content.clone(), Some(21), Some(80), None);
        let recovered = format!(
            "{}\n{}",
            sampled["content"].as_str().unwrap(),
            remaining["content"].as_str().unwrap()
        );
        assert_eq!(recovered, content.trim_end());
        let full = read_result(path, content.clone(), None, Some(100), None);
        assert_eq!(full["content"], content.trim_end());
        let offset_only = read_result(path, content, Some(21), None, None);
        assert_eq!(offset_only["end_line"], 40);
    }

    #[test]
    fn efficiency_regression_unicode_record_is_bounded_without_skipping_partial_line() {
        let content = format!("{{\"message\":\"{}\"}}\n", "数".repeat(6000));
        let sampled = read_result(
            std::path::Path::new("records.ndjson"),
            content.clone(),
            None,
            None,
            None,
        );
        assert!(sampled["content"].as_str().unwrap().chars().count() <= 4000);
        assert_eq!(sampled["truncated"], true);
        assert_eq!(sampled["end_line"], 1);
        assert_eq!(
            sampled["next_offset"], 1,
            "retry the partial line with an explicit limit"
        );
        let full = read_result(
            std::path::Path::new("records.ndjson"),
            content.clone(),
            Some(1),
            Some(1),
            None,
        );
        assert_eq!(full["content"], content.trim_end());
        let source = read_result(
            std::path::Path::new("src.rs"),
            content.clone(),
            None,
            None,
            None,
        );
        assert_eq!(source["content"], content);
        let small = read_result(
            std::path::Path::new("small.csv"),
            "id,value\n1,2\n".into(),
            None,
            None,
            None,
        );
        assert_eq!(small["content"], "id,value\n1,2\n");
    }
    use crate::PromptContext;
    use serde_json::json;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn tool_context(workspace: &std::path::Path) -> ToolContext {
        ToolContext {
            workspace: workspace.to_path_buf(),
            sandbox_writable_roots: None,
            base: workspace.to_path_buf(),
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "test-session".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: None,
            sender_role: None,
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

        let result = ReadFileTool::default()
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
    async fn read_file_batches_multiple_paths() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("one.txt"), "first").unwrap();
        std::fs::write(dir.path().join("two.txt"), "second").unwrap();

        let result = ReadFileTool::default()
            .execute(
                tool_context(dir.path()),
                json!({"paths": ["one.txt", "two.txt"]}),
            )
            .await
            .unwrap();

        assert_eq!(result["files"].as_array().unwrap().len(), 2);
        assert_eq!(result["files"][0]["content"], "first");
        assert_eq!(result["files"][1]["content"], "second");
    }

    #[tokio::test]
    async fn edit_file_accepts_old_string_and_new_string_aliases() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        std::fs::write(&path, "before\n").unwrap();

        EditFileTool::default()
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

        let result = EditFileTool::default()
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

        let error = EditFileTool::default()
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

    /// 只存在于内存中的文件系统，用于验证文件工具完全经由 backend 访问文件。
    #[derive(Default)]
    struct MemoryFs(std::sync::Mutex<std::collections::BTreeMap<PathBuf, String>>);

    #[async_trait]
    impl FilesystemBackend for MemoryFs {
        async fn kind(&self, path: &std::path::Path) -> Result<Option<FsEntryKind>> {
            let files = self.0.lock().unwrap();
            Ok(if files.contains_key(path) {
                Some(FsEntryKind::File)
            } else if files.keys().any(|file| file.parent() == Some(path)) {
                Some(FsEntryKind::Directory)
            } else {
                None
            })
        }
        async fn read_to_string(&self, path: &std::path::Path) -> Result<String> {
            let content = self
                .0
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| Error::NotFound(path.display().to_string()));
            tokio::task::yield_now().await;
            content
        }
        async fn read_document(&self, path: &std::path::Path) -> Result<String> {
            self.read_to_string(path).await
        }
        async fn size(&self, path: &std::path::Path) -> Result<Option<u64>> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(path)
                .map(|content| content.len() as u64))
        }
        async fn write(&self, path: &std::path::Path, content: &str) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(path.to_path_buf(), content.to_string());
            Ok(())
        }
        async fn list_dir(
            &self,
            path: &std::path::Path,
        ) -> Result<Vec<crate::fs_backend::FsEntry>> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .keys()
                .filter(|file| file.parent() == Some(path))
                .map(|file| crate::fs_backend::FsEntry {
                    name: file.file_name().unwrap().to_string_lossy().to_string(),
                    kind: FsEntryKind::File,
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn concurrent_edits_from_separate_tools_preserve_both_changes() {
        let workspace = std::path::Path::new("/virtual-workspace");
        let fs = Arc::new(MemoryFs::default());
        let path = workspace.join("shared.txt");
        fs.write(&path, "alpha beta").await.unwrap();
        let first = EditFileTool::new(fs.clone());
        let second = EditFileTool::new(fs.clone());
        let (a, b) = tokio::join!(
            first.execute(
                tool_context(workspace),
                json!({"path":"shared.txt", "old_text":"alpha", "new_text":"A"})
            ),
            second.execute(
                tool_context(workspace),
                json!({"path":"shared.txt", "old_text":"beta", "new_text":"B"})
            ),
        );
        a.unwrap();
        b.unwrap();
        assert_eq!(fs.read_to_string(&path).await.unwrap(), "A B");
    }

    #[tokio::test]
    async fn file_tools_route_through_injected_backend() {
        let workspace = std::path::Path::new("/virtual-workspace");
        let fs: Arc<dyn FilesystemBackend> = Arc::new(MemoryFs::default());
        let ctx = || tool_context(workspace);

        let batch = WriteFileTool::new(fs.clone())
            .execute(
                ctx(),
                json!({"files":[
                    {"path":"notes/a.txt","content":"alpha"},
                    {"path":"batch/b.txt","content":"bravo"}
                ]}),
            )
            .await
            .unwrap();
        assert_eq!(batch["files"].as_array().unwrap().len(), 2);
        assert_eq!(batch["files"][0]["ok"], true);
        assert_eq!(batch["files"][1]["ok"], true);

        WriteFileTool::new(fs.clone())
            .execute(ctx(), json!({"path": "notes/a.txt", "content": "alpha"}))
            .await
            .unwrap();
        EditFileTool::new(fs.clone())
            .execute(
                ctx(),
                json!({"path": "notes/a.txt", "old_text": "alpha", "new_text": "beta"}),
            )
            .await
            .unwrap();
        let read = ReadFileTool::new(fs.clone())
            .execute(ctx(), json!({"path": "notes/a.txt"}))
            .await
            .unwrap();
        assert_eq!(read["content"], "beta");

        let listed = ListDirTool::new(fs.clone())
            .execute(ctx(), json!({"path": "notes"}))
            .await
            .unwrap();
        assert_eq!(
            listed["entries"],
            json!([{"name": "a.txt", "type": "file"}])
        );

        let error = ReadFileTool::new(fs)
            .execute(ctx(), json!({"path": "notes"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Not a file"), "{error}");
        assert!(!workspace.exists());
    }

    #[tokio::test]
    async fn list_dir_skips_common_noise_directories() {
        let workspace = tempdir().unwrap();
        for name in [
            ".git",
            "__pycache__",
            "node_modules",
            "target",
            ".blockcell",
            "src",
        ] {
            std::fs::create_dir(workspace.path().join(name)).unwrap();
        }
        let listed = ListDirTool::default()
            .execute(tool_context(workspace.path()), json!({"path": "."}))
            .await
            .unwrap();
        let names = listed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["src"]);
    }

    #[test]
    fn enhanced_file_schemas_expose_ranges_and_canonical_edits() {
        let read = ReadFileTool::default().schema();
        let edit = EditFileTool::default().schema();
        assert!(read.parameters["properties"].get("offset").is_some());
        assert!(read.parameters["properties"].get("limit").is_some());
        assert!(edit.parameters["properties"].get("old_text").is_some());
        assert!(edit.parameters["properties"].get("new_text").is_some());
        assert!(edit.parameters["properties"].get("old_string").is_none());
        assert!(edit.parameters["properties"].get("new_string").is_none());
        assert_eq!(edit.parameters["properties"]["edits"]["type"], "array");
        assert!(
            edit.parameters["properties"]["edits"]["items"]["properties"]
                .get("old_text")
                .is_some()
        );
        assert!(
            edit.parameters["properties"]["edits"]["items"]["properties"]
                .get("new_text")
                .is_some()
        );
        assert!(
            edit.parameters["properties"]["edits"]["items"]["properties"]
                .get("old_string")
                .is_none()
        );
        assert!(
            edit.parameters["properties"]["edits"]["items"]["properties"]
                .get("new_string")
                .is_none()
        );
    }

    #[test]
    fn test_read_file_schema() {
        let tool = ReadFileTool::default();
        let schema = tool.schema();
        assert_eq!(schema.name, "read_file");
    }

    #[test]
    fn test_read_file_validate() {
        let tool = ReadFileTool::default();
        assert!(tool.validate(&json!({"path": "/tmp/test.txt"})).is_ok());
        assert!(tool.validate(&json!({"paths": ["a", "b"]})).is_ok());
        assert!(tool
            .validate(&json!({"path": "a", "paths": ["b"]}))
            .is_err());
        assert!(tool.validate(&json!({"paths": []})).is_err());
        assert!(tool
            .validate(&json!({"paths": ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k"]}))
            .is_err());
        assert!(tool.validate(&json!({})).is_err());
    }

    #[test]
    fn test_write_file_schema() {
        let tool = WriteFileTool::default();
        let schema = tool.schema();
        assert_eq!(schema.name, "write_file");
    }

    #[test]
    fn test_write_file_validate() {
        let tool = WriteFileTool::default();
        assert!(tool
            .validate(&json!({"path": "/tmp/t.txt", "content": "hi"}))
            .is_ok());
        assert!(tool.validate(&json!({"path": "/tmp/t.txt"})).is_err());
        assert!(tool.validate(&json!({"content": "hi"})).is_err());
        assert!(tool
            .validate(&json!({"files":[{"path":"a","content":"1"}]}))
            .is_ok());
        assert!(tool
            .validate(&json!({"files":[{"path":"a","content":"1"}],"path":"b","content":"2"}))
            .is_err());
        let too_many = (0..11)
            .map(|index| json!({"path":format!("{index}.txt"),"content":"x"}))
            .collect::<Vec<_>>();
        assert!(tool.validate(&json!({"files":too_many})).is_err());
    }

    #[test]
    fn test_edit_file_schema() {
        let tool = EditFileTool::default();
        let schema = tool.schema();
        assert_eq!(schema.name, "edit_file");
    }

    #[test]
    fn test_edit_file_validate() {
        let tool = EditFileTool::default();
        assert!(tool
            .validate(&json!({"path": "f", "old_text": "a", "new_text": "b"}))
            .is_ok());
        assert!(tool
            .validate(&json!({"path": "f", "old_text": "a"}))
            .is_err());
    }

    #[test]
    fn test_list_dir_schema() {
        let tool = ListDirTool::default();
        let schema = tool.schema();
        assert_eq!(schema.name, "list_dir");
    }

    #[test]
    fn test_list_dir_validate() {
        let tool = ListDirTool::default();
        assert!(tool.validate(&json!({"path": "/tmp"})).is_ok());
        assert!(tool.validate(&json!({})).is_err());
    }

    #[test]
    fn test_list_dir_prompt_rule_requires_explicit_path_and_current_dir_example() {
        let tool = ListDirTool::default();
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
        let tool = WriteFileTool::default();
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
        let read_schema = ReadFileTool::default().schema();
        let edit_schema = EditFileTool::default().schema();
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
