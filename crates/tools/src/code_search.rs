use async_trait::async_trait;
use blockcell_core::{Error, Result};
use ignore::WalkBuilder;
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::{Tool, ToolContext, ToolSchema};

const DEFAULT_MAX_RESULTS: usize = 200;
const MAX_RESULTS: usize = 2_000;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

pub struct GrepTool;

pub struct GlobTool;

fn required_string<'a>(params: &'a Value, name: &str) -> Result<&'a str> {
    params
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::Validation(format!("Missing required parameter: {name}")))
}

fn max_results(params: &Value) -> usize {
    params
        .get("max_results")
        .and_then(Value::as_u64)
        .map(|value| value.clamp(1, MAX_RESULTS as u64) as usize)
        .unwrap_or(DEFAULT_MAX_RESULTS)
}

fn build_glob(pattern: Option<&str>) -> Result<Option<glob::Pattern>> {
    let Some(pattern) = pattern.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    glob::Pattern::new(pattern)
        .map(Some)
        .map_err(|e| Error::Validation(format!("Invalid glob pattern: {e}")))
}

fn walker(root: &Path) -> ignore::Walk {
    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(true)
        .require_git(false)
        .hidden(false)
        .follow_links(false);
    builder.build()
}

fn path_matches(root: &Path, path: &Path, pattern: Option<&glob::Pattern>) -> bool {
    pattern.is_none_or(|pattern| pattern.matches(&relative_path(root, path)))
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn estimated_json_bytes(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

fn grep_sync(root: PathBuf, params: Value) -> Result<Value> {
    let pattern = required_string(&params, "pattern")?;
    let regex = Regex::new(pattern)
        .map_err(|e| Error::Validation(format!("Invalid regular expression: {e}")))?;
    let file_glob = build_glob(params.get("glob").and_then(Value::as_str))?;
    let context = params
        .get("context")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(50) as usize;
    let result_limit = max_results(&params);
    let mut matches = Vec::new();
    let mut output_bytes = 0usize;
    let mut truncated = false;

    'files: for entry in walker(&root) {
        let entry = match entry {
            Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_file()) => entry,
            _ => continue,
        };
        if !path_matches(&root, entry.path(), file_glob.as_ref()) {
            continue;
        }
        // Skip huge files instead of slurping them into memory: grep output is
        // already capped, but the read itself was unbounded.
        const MAX_GREP_FILE_BYTES: u64 = 32 * 1024 * 1024;
        if std::fs::metadata(entry.path())
            .map(|metadata| metadata.len() > MAX_GREP_FILE_BYTES)
            .unwrap_or(false)
        {
            tracing::debug!(
                path = %entry.path().display(),
                "Skipping oversized file during grep"
            );
            continue;
        }
        let content = match std::fs::read_to_string(entry.path()) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let lines: Vec<&str> = content.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !regex.is_match(line) {
                continue;
            }
            let before_start = index.saturating_sub(context);
            let after_end = (index + context + 1).min(lines.len());
            let item = json!({
                "path": relative_path(&root, entry.path()),
                "line_number": index + 1,
                "line": line,
                "before": lines[before_start..index],
                "after": lines[index + 1..after_end],
            });
            let item_bytes = estimated_json_bytes(&item);
            if matches.len() >= result_limit
                || output_bytes.saturating_add(item_bytes) > MAX_OUTPUT_BYTES
            {
                truncated = true;
                break 'files;
            }
            output_bytes += item_bytes;
            matches.push(item);
        }
    }

    Ok(json!({
        "matches": matches,
        "count": matches.len(),
        "truncated": truncated,
    }))
}

fn glob_sync(root: PathBuf, params: Value) -> Result<Value> {
    let pattern = required_string(&params, "pattern")?;
    let file_glob = build_glob(Some(pattern))?;
    let result_limit = max_results(&params);
    let mut files = Vec::new();

    for entry in walker(&root) {
        let entry = match entry {
            Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_file()) => entry,
            _ => continue,
        };
        if !path_matches(&root, entry.path(), file_glob.as_ref()) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        files.push(json!({
            "path": relative_path(&root, entry.path()),
            "modified_ms": modified_ms,
            "size": metadata.len(),
        }));
    }

    files.sort_by(|left, right| {
        right["modified_ms"]
            .as_u64()
            .cmp(&left["modified_ms"].as_u64())
            .then_with(|| left["path"].as_str().cmp(&right["path"].as_str()))
    });

    let total = files.len();
    let mut selected = Vec::new();
    let mut output_bytes = 0usize;
    for file in files {
        let item_bytes = estimated_json_bytes(&file);
        if selected.len() >= result_limit
            || output_bytes.saturating_add(item_bytes) > MAX_OUTPUT_BYTES
        {
            break;
        }
        output_bytes += item_bytes;
        selected.push(file);
    }
    let truncated = selected.len() < total;

    Ok(json!({
        "files": selected,
        "count": selected.len(),
        "truncated": truncated,
    }))
}

#[async_trait]
impl Tool for GrepTool {
    fn execution_environment(&self, _params: &Value) -> crate::ExecutionEnvironment {
        crate::ExecutionEnvironment::Local
    }

    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Safe
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "grep".to_string(),
            description: "Search text files with a regular expression. Respects .gitignore and returns paths, line numbers, and optional context lines.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regular expression to search for"},
                    "glob": {"type": "string", "description": "Optional file glob such as src/**/*.rs"},
                    "context": {"type": "integer", "minimum": 0, "maximum": 50, "description": "Lines before and after each match"},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": MAX_RESULTS}
                },
                "required": ["pattern"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let pattern = required_string(params, "pattern")?;
        Regex::new(pattern)
            .map(|_| ())
            .map_err(|e| Error::Validation(format!("Invalid regular expression: {e}")))?;
        build_glob(params.get("glob").and_then(Value::as_str))?;
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        tokio::task::spawn_blocking(move || grep_sync(ctx.workspace, params))
            .await
            .map_err(|e| Error::Tool(format!("grep worker failed: {e}")))?
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn execution_environment(&self, _params: &Value) -> crate::ExecutionEnvironment {
        crate::ExecutionEnvironment::Local
    }

    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Safe
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "glob".to_string(),
            description:
                "Find files by glob pattern. Respects .gitignore and returns newest files first."
                    .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "File glob such as **/*.rs"},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": MAX_RESULTS}
                },
                "required": ["pattern"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let pattern = required_string(params, "pattern")?;
        build_glob(Some(pattern))?;
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        tokio::task::spawn_blocking(move || glob_sync(ctx.workspace, params))
            .await
            .map_err(|e| Error::Tool(format!("glob worker failed: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Tool, ToolContext};
    use serde_json::json;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};
    use tempfile::tempdir;

    fn context(workspace: &std::path::Path) -> ToolContext {
        ToolContext {
            workspace: workspace.to_path_buf(),
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
    async fn grep_respects_gitignore_and_reports_line_context() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(
            dir.path().join("main.rs"),
            "before\nfn target_symbol() {}\nafter\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("ignored.rs"), "fn target_symbol() {}\n").unwrap();

        let result = GrepTool
            .execute(
                context(dir.path()),
                json!({"pattern": "target_symbol", "context": 1}),
            )
            .await
            .unwrap();

        let matches = result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["path"], "main.rs");
        assert_eq!(matches[0]["line_number"], 2);
        assert_eq!(matches[0]["before"][0], "before");
        assert_eq!(matches[0]["after"][0], "after");
    }

    #[tokio::test]
    async fn grep_filters_paths_with_glob_and_truncates_output() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "needle\nneedle\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "needle\n").unwrap();

        let result = GrepTool
            .execute(
                context(dir.path()),
                json!({"pattern": "needle", "glob": "src/**/*.rs", "max_results": 1}),
            )
            .await
            .unwrap();

        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
        assert_eq!(result["matches"][0]["path"], "src/lib.rs");
        assert_eq!(result["truncated"], true);
    }

    #[tokio::test]
    async fn glob_returns_newest_files_first_and_respects_gitignore() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.rs\n").unwrap();
        let old = dir.path().join("old.rs");
        let new = dir.path().join("new.rs");
        std::fs::write(&old, "old").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&new, "new").unwrap();
        std::fs::write(dir.path().join("ignored.rs"), "ignored").unwrap();

        let result = GlobTool
            .execute(context(dir.path()), json!({"pattern": "**/*.rs"}))
            .await
            .unwrap();

        let files = result["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["path"], "new.rs");
        assert_eq!(files[1]["path"], "old.rs");
        assert!(
            files[0]["modified_ms"].as_u64().unwrap()
                >= SystemTime::UNIX_EPOCH.elapsed().unwrap().as_millis() as u64 - 60_000
        );
    }

    #[test]
    fn search_schemas_expose_expected_parameters() {
        let grep = GrepTool.schema();
        let glob = GlobTool.schema();
        assert_eq!(grep.name, "grep");
        assert!(grep.parameters["properties"].get("context").is_some());
        assert!(grep.parameters["properties"].get("glob").is_some());
        assert_eq!(glob.name, "glob");
        assert!(glob.parameters["properties"].get("pattern").is_some());
        let _tools: Vec<Arc<dyn Tool>> = vec![Arc::new(GrepTool), Arc::new(GlobTool)];
    }
}
