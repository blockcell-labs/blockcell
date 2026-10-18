use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::{Tool, ToolContext, ToolSchema};

#[derive(Debug, Default)]
struct SkillAssetMetadata {
    has_rhai: bool,
    has_py: bool,
    has_md: bool,
    script_assets: Vec<String>,
}

fn is_script_asset(path: &Path) -> bool {
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "rhai" | "py" | "sh" | "php" | "js" | "ts" | "rb"))
    {
        return true;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(path)
            .ok()
            .is_some_and(|meta| meta.permissions().mode() & 0o111 != 0)
        {
            return true;
        }
    }

    false
}

fn collect_script_assets(root: &Path, dir: &Path, assets: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_script_assets(root, &path, assets);
            continue;
        }

        if path.is_file() && is_script_asset(&path) {
            if let Ok(rel) = path.strip_prefix(root) {
                assets.push(rel.to_path_buf());
            }
        }
    }
}

/// Tool for querying installed learned skills.
pub struct ListSkillsTool;
pub struct SkillViewTool;
pub struct SkillManageTool;

fn get_skill_file_store(ctx: &ToolContext) -> Result<&crate::SkillFileStoreHandle> {
    ctx.skill_file_store
        .as_ref()
        .ok_or_else(|| Error::Tool("Skill file store not available".to_string()))
}

#[async_trait]
impl Tool for SkillViewTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "skill_view".to_string(),
            description: "View a workspace skill's content (SKILL.md, SKILL.rhai, SKILL.py, or metadata-only fallback), meta.yaml, and supporting file list.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Workspace skill name, or category/name for categorized skills."}
                },
                "required": ["name"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        require_string(params, "name")?;
        Ok(())
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- Use `skill_view` before patching learned workspace skills so changes are precise and minimal.".to_string())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let name = require_string(&params, "name")?;
        get_skill_file_store(&ctx)?.view_skill_json(name)
    }
}

#[async_trait]
impl Tool for SkillManageTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "skill_manage".to_string(),
            description: "Create, patch, delete, or update supporting files for workspace skills. Use for reusable procedures learned from successful work.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["create", "edit", "patch", "delete", "write_file", "remove_file", "undo_latest"]},
                    "name": {"type": "string", "description": "Workspace skill name, or category/name for categorized skills."},
                    "description": {"type": "string", "description": "Required for create."},
                    "content": {"type": "string", "description": "Skill body, full SKILL.md rewrite, replacement text, or file content."},
                    "old_text": {"type": "string", "description": "Unique text to replace for patch."},
                    "path": {"type": "string", "description": "Supporting path under references/, templates/, scripts/, or assets/, or top-level SKILL.md/SKILL.rhai/SKILL.py/meta.yaml/meta.json."}
                },
                "required": ["action", "name"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = require_string(params, "action")?;
        require_string(params, "name")?;
        match action {
            "create" => {
                require_string(params, "description")?;
                require_string(params, "content")?;
            }
            "edit" => {
                require_string(params, "content")?;
            }
            "patch" => {
                require_string(params, "old_text")?;
                require_string(params, "content")?;
            }
            "delete" | "undo_latest" => {}
            "write_file" => {
                require_string(params, "path")?;
                require_string(params, "content")?;
            }
            "remove_file" => {
                require_string(params, "path")?;
            }
            _ => {
                return Err(Error::Validation(
                    "action must be create, edit, patch, delete, write_file, remove_file, or undo_latest"
                        .to_string(),
                ));
            }
        }
        Ok(())
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- Use `skill_manage` only for durable reusable procedures. Prefer prompt-only skills unless the user explicitly needs executable assets.".to_string())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let store = get_skill_file_store(&ctx)?;
        let action = require_string(&params, "action")?;
        let name = require_string(&params, "name")?;
        match action {
            "create" => store.create_skill_json(
                name,
                require_string(&params, "description")?,
                require_string(&params, "content")?,
            ),
            "edit" => store.edit_skill_json(name, require_string(&params, "content")?),
            "patch" => store.patch_skill_json(
                name,
                require_string(&params, "old_text")?,
                require_string(&params, "content")?,
            ),
            "delete" => store.delete_skill_json(name),
            "write_file" => store.write_skill_file_json(
                name,
                require_string(&params, "path")?,
                require_string(&params, "content")?,
            ),
            "remove_file" => store.remove_skill_file_json(name, require_string(&params, "path")?),
            "undo_latest" => store.restore_latest_skill_json(name),
            _ => Err(Error::Validation("invalid skill_manage action".to_string())),
        }
    }
}

fn require_string<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Validation(format!("{} is required", key)))
}

#[async_trait]
impl Tool for ListSkillsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_skills".to_string(),
            description: "List installed skills available to the assistant, including workspace learned skills and built-in skills. Use when the user asks what reusable skills or learned procedures are available.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "enum": ["learned", "all", "available"],
                        "description": "What to query: 'learned' or 'all' lists installed learned skills; 'available' is an alias. Legacy pipeline state is not exposed."
                    }
                },
                "required": []
            }),
        }
    }

    fn validate(&self, _params: &Value) -> Result<()> {
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("all");

        let skills_dir = ctx.workspace.join("skills");
        let mut external_dirs = Vec::new();
        if let Some(builtin) = ctx.builtin_skills_dir.clone() {
            external_dirs.push(builtin);
        }

        match query {
            "learned" | "available" | "all" => {
                self.get_available_skills(&skills_dir, &external_dirs).await
            }
            _ => self.get_available_skills(&skills_dir, &external_dirs).await,
        }
    }
}

impl ListSkillsTool {
    fn detect_skill_assets(&self, skill_dir: &Path) -> SkillAssetMetadata {
        let has_rhai = skill_dir.join("SKILL.rhai").exists();
        let has_py = skill_dir.join("SKILL.py").exists();
        let has_md = skill_dir.join("SKILL.md").exists();

        let mut script_assets = Vec::new();
        collect_script_assets(skill_dir, skill_dir, &mut script_assets);
        script_assets.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
        script_assets.dedup();

        SkillAssetMetadata {
            has_rhai,
            has_py,
            has_md,
            script_assets: script_assets
                .into_iter()
                .map(|path| path.display().to_string().replace('\\', "/"))
                .collect(),
        }
    }

    /// Get available loaded skills
    async fn get_available_skills(
        &self,
        skills_dir: &std::path::Path,
        external_dirs: &[PathBuf],
    ) -> Result<Value> {
        let mut skills = Vec::new();
        let mut seen_names = std::collections::HashSet::new();

        // Scan workspace skills first (higher priority)
        self.scan_skills_dir(skills_dir, &mut skills, &mut seen_names);

        // Scan external and builtin skills (lower priority, skip duplicates)
        for builtin in external_dirs {
            self.scan_skills_dir(builtin, &mut skills, &mut seen_names);
        }

        Ok(json!({
            "learned_skills": skills,
            "count": skills.len(),
            "note": if skills.is_empty() {
                "No installed learned skills are available yet."
            } else {
                "Installed learned skills available to this assistant."
            }
        }))
    }

    /// Scan a directory for skill subdirectories (recursive, supports category dirs).
    ///
    /// Mirrors the discovery logic in `SkillIndex::scan_dir`:
    /// - A dir with skill markers (SKILL.md / meta.yaml / meta.json / SKILL.rhai / SKILL.py)
    ///   is a skill, keyed by `category/name` (or just `name` when category is empty).
    /// - A dir with `manifest.json` is a skill pack; recurse into it.
    /// - Any other dir is treated as a category; recurse into it looking for skills.
    fn scan_skills_dir(
        &self,
        dir: &std::path::Path,
        skills: &mut Vec<Value>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        self.scan_skills_dir_recursive(dir, "", skills, seen);
    }

    fn scan_skills_dir_recursive(
        &self,
        dir: &std::path::Path,
        category: &str,
        skills: &mut Vec<Value>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        if !dir.exists() || !dir.is_dir() {
            return;
        }

        const EXCLUDED_DIRS: &[&str] = &[".git", ".github", ".hub", "__pycache__", "node_modules"];

        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let dir_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();

                if EXCLUDED_DIRS.contains(&dir_name.as_str()) {
                    continue;
                }

                // 统一构造 sub_category，确保深层 category 路径累积完整
                let sub_category = if category.is_empty() {
                    dir_name.clone()
                } else {
                    format!("{}/{}", category, dir_name)
                };

                let is_skill = path.join("SKILL.md").exists()
                    || path.join("meta.yaml").exists()
                    || path.join("meta.json").exists()
                    || path.join("SKILL.rhai").exists()
                    || path.join("SKILL.py").exists();

                if is_skill {
                    let composite_name = sub_category.clone();

                    // Skip if already seen (workspace overrides builtin)
                    if !seen.insert(composite_name.clone()) {
                        // 仍需递归扫描 skill 包子目录，使用 sub_category 保持路径完整
                        if path.join("manifest.json").exists() {
                            self.scan_skills_dir_recursive(&path, &sub_category, skills, seen);
                        }
                        continue;
                    }

                    let meta = self.read_skill_meta(&path);
                    let assets = self.detect_skill_assets(&path);

                    skills.push(json!({
                        "name": composite_name,
                        "category": category,
                        "description": meta.get("description").unwrap_or(&Value::Null),
                        "always": meta.get("always").unwrap_or(&json!(false)),
                        "has_rhai": assets.has_rhai,
                        "has_py": assets.has_py,
                        "has_md": assets.has_md,
                        "has_script_assets": !assets.script_assets.is_empty(),
                        "script_assets": assets.script_assets,
                        "path": path.display().to_string(),
                    }));

                    // 若同时是 skill 包（含 manifest.json），也递归扫描子目录
                    // 使用 sub_category 保持路径完整，确保子 skill 可被 SkillFileStore 解析
                    if path.join("manifest.json").exists() {
                        self.scan_skills_dir_recursive(&path, &sub_category, skills, seen);
                    }
                    continue;
                }

                // 非 skill 目录但含 manifest.json：作为 skill 包递归扫描子目录
                // pack 目录本身是路径的一部分，需要计入 category
                if path.join("manifest.json").exists() {
                    self.scan_skills_dir_recursive(&path, &sub_category, skills, seen);
                    continue;
                }

                // 否则：视为 category 目录，递归扫描子目录
                // 使用 sub_category 累积路径，避免深层 category 截断
                self.scan_skills_dir_recursive(&path, &sub_category, skills, seen);
            }
        }
    }

    /// Read skill meta.yaml or meta.json
    fn read_skill_meta(&self, skill_dir: &std::path::Path) -> Value {
        // Try meta.json first (simpler to parse)
        let json_path = skill_dir.join("meta.json");
        if json_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&json_path) {
                if let Ok(meta) = serde_json::from_str::<Value>(&content) {
                    return meta;
                }
            }
        }

        // Try meta.yaml (parse key: value lines manually to avoid serde_yaml dependency)
        let yaml_path = skill_dir.join("meta.yaml");
        if yaml_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&yaml_path) {
                let mut meta = serde_json::Map::new();
                for line in content.lines() {
                    if let Some((key, val)) = line.split_once(':') {
                        let key = key.trim().to_string();
                        let val = val.trim();
                        // Handle boolean
                        if val == "true" {
                            meta.insert(key, Value::Bool(true));
                        } else if val == "false" {
                            meta.insert(key, Value::Bool(false));
                        } else {
                            meta.insert(key, Value::String(val.to_string()));
                        }
                    }
                }
                if !meta.is_empty() {
                    return Value::Object(meta);
                }
            }
        }

        json!({})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Default)]
    struct CaptureSkillFileStore {
        calls: Mutex<Vec<String>>,
    }

    impl crate::SkillFileStoreOps for CaptureSkillFileStore {
        fn view_skill_json(&self, name: &str) -> Result<Value> {
            self.calls.lock().unwrap().push(format!("view:{name}"));
            Ok(json!({"success": true, "name": name, "content": "skill"}))
        }

        fn create_skill_json(&self, name: &str, description: &str, content: &str) -> Result<Value> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("create:{name}:{description}:{content}"));
            Ok(json!({"success": true, "skillName": name, "action": "create"}))
        }

        fn edit_skill_json(&self, name: &str, content: &str) -> Result<Value> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("edit:{name}:{content}"));
            Ok(json!({"success": true, "skillName": name, "action": "edit"}))
        }

        fn patch_skill_json(&self, name: &str, old_text: &str, content: &str) -> Result<Value> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("patch:{name}:{old_text}:{content}"));
            Ok(json!({"success": true, "skillName": name, "action": "patch"}))
        }

        fn delete_skill_json(&self, name: &str) -> Result<Value> {
            self.calls.lock().unwrap().push(format!("delete:{name}"));
            Ok(json!({"success": true, "skillName": name, "action": "delete"}))
        }

        fn write_skill_file_json(&self, name: &str, path: &str, content: &str) -> Result<Value> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("write:{name}:{path}:{content}"));
            Ok(json!({"success": true, "skillName": name, "action": "write_file"}))
        }

        fn remove_skill_file_json(&self, name: &str, path: &str) -> Result<Value> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("remove:{name}:{path}"));
            Ok(json!({"success": true, "skillName": name, "action": "remove_file"}))
        }

        fn restore_latest_skill_json(&self, name: &str) -> Result<Value> {
            self.calls.lock().unwrap().push(format!("undo:{name}"));
            Ok(json!({"success": true, "skillName": name, "action": "restore_latest"}))
        }
    }

    fn tool_context(store: Arc<dyn crate::SkillFileStoreOps + Send + Sync>) -> ToolContext {
        ToolContext {
            workspace: PathBuf::from("/tmp/workspace"),
            base: PathBuf::from("/tmp/blockcell"),
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "cli:test".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: None,
            sender_role: None,
            chat_id: "chat-1".to_string(),
            config: blockcell_core::Config::default(),
            permissions: blockcell_core::types::PermissionSet::new(),
            task_manager: None,
            memory_store: None,
            memory_file_store: None,
            ghost_memory_lifecycle: None,
            skill_file_store: Some(store),
            session_search: None,
            outbound_tx: None,
            spawn_handle: None,
            capability_registry: None,
            core_evolution: None,
            event_emitter: None,
            channel_contacts_file: None,
            response_cache: None,
            skill_mutex: None,
            agent_type_registry: None,
            evolution_workflow_store: None,
            runtime_handle: None,
            agent_identity: None,
        }
    }

    fn tool_context_with_workspace(
        workspace: PathBuf,
        store: Option<Arc<dyn crate::SkillFileStoreOps + Send + Sync>>,
    ) -> ToolContext {
        ToolContext {
            workspace: workspace.clone(),
            base: workspace,
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "cli:test".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: None,
            sender_role: None,
            chat_id: "chat-1".to_string(),
            config: blockcell_core::Config::default(),
            permissions: blockcell_core::types::PermissionSet::new(),
            task_manager: None,
            memory_store: None,
            memory_file_store: None,
            ghost_memory_lifecycle: None,
            skill_file_store: store,
            session_search: None,
            outbound_tx: None,
            spawn_handle: None,
            capability_registry: None,
            core_evolution: None,
            event_emitter: None,
            channel_contacts_file: None,
            response_cache: None,
            skill_mutex: None,
            agent_type_registry: None,
            evolution_workflow_store: None,
            runtime_handle: None,
            agent_identity: None,
        }
    }

    #[tokio::test]
    async fn test_skill_view_routes_to_file_store() {
        let store = Arc::new(CaptureSkillFileStore::default());
        let result = SkillViewTool
            .execute(
                tool_context(store.clone()),
                json!({"name": "release_checklist"}),
            )
            .await
            .unwrap();
        assert_eq!(result["name"], json!("release_checklist"));
        assert_eq!(
            store.calls.lock().unwrap().as_slice(),
            ["view:release_checklist"]
        );
    }

    #[tokio::test]
    async fn test_skill_manage_create_routes_to_file_store() {
        let store = Arc::new(CaptureSkillFileStore::default());
        let result = SkillManageTool
            .execute(
                tool_context(store.clone()),
                json!({
                    "action": "create",
                    "name": "release_checklist",
                    "description": "Release checklist",
                    "content": "Confirm rollback plan."
                }),
            )
            .await
            .unwrap();
        assert_eq!(result["action"], json!("create"));
        assert_eq!(
            store.calls.lock().unwrap().as_slice(),
            ["create:release_checklist:Release checklist:Confirm rollback plan."]
        );
    }

    #[tokio::test]
    async fn test_skill_manage_edit_routes_to_file_store() {
        let store = Arc::new(CaptureSkillFileStore::default());
        let result = SkillManageTool
            .execute(
                tool_context(store.clone()),
                json!({
                    "action": "edit",
                    "name": "release_checklist",
                    "content": "# release_checklist\n\nUpdated full skill."
                }),
            )
            .await
            .unwrap();
        assert_eq!(result["action"], json!("edit"));
        assert_eq!(
            store.calls.lock().unwrap().as_slice(),
            ["edit:release_checklist:# release_checklist\n\nUpdated full skill."]
        );
    }

    #[tokio::test]
    async fn test_skill_manage_file_lifecycle_routes_to_file_store() {
        let store = Arc::new(CaptureSkillFileStore::default());
        let write = SkillManageTool
            .execute(
                tool_context(store.clone()),
                json!({
                    "action": "write_file",
                    "name": "release_checklist",
                    "path": "references/checklist.md",
                    "content": "# Checklist"
                }),
            )
            .await
            .unwrap();
        assert_eq!(write["action"], json!("write_file"));

        let remove = SkillManageTool
            .execute(
                tool_context(store.clone()),
                json!({
                    "action": "remove_file",
                    "name": "release_checklist",
                    "path": "references/checklist.md"
                }),
            )
            .await
            .unwrap();
        assert_eq!(remove["action"], json!("remove_file"));

        let delete = SkillManageTool
            .execute(
                tool_context(store.clone()),
                json!({
                    "action": "delete",
                    "name": "release_checklist"
                }),
            )
            .await
            .unwrap();
        assert_eq!(delete["action"], json!("delete"));

        assert_eq!(
            store.calls.lock().unwrap().as_slice(),
            [
                "write:release_checklist:references/checklist.md:# Checklist",
                "remove:release_checklist:references/checklist.md",
                "delete:release_checklist"
            ]
        );
    }

    #[tokio::test]
    async fn test_skill_manage_undo_latest_routes_to_file_store() {
        let store = Arc::new(CaptureSkillFileStore::default());
        let result = SkillManageTool
            .execute(
                tool_context(store.clone()),
                json!({"action": "undo_latest", "name": "release_checklist"}),
            )
            .await
            .unwrap();

        assert_eq!(result["action"], json!("restore_latest"));
        assert_eq!(
            store.calls.lock().unwrap().as_slice(),
            ["undo:release_checklist"]
        );
    }

    #[test]
    fn test_list_skills_schema() {
        let tool = ListSkillsTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "list_skills");
        assert!(!schema.description.contains("evolution"));
        assert!(!schema.description.contains("evolving"));
        let query_enum = schema.parameters["properties"]["query"]["enum"]
            .as_array()
            .expect("query enum");
        assert!(!query_enum.iter().any(|value| value == "learning"));
    }

    #[test]
    fn test_list_skills_validate() {
        let tool = ListSkillsTool;
        assert!(tool.validate(&json!({})).is_ok());
        assert!(tool.validate(&json!({"query": "learned"})).is_ok());
    }

    #[tokio::test]
    async fn test_list_skills_ignores_legacy_evolution_records() {
        let tool = ListSkillsTool;
        let mut workspace = std::env::temp_dir();
        workspace.push(format!(
            "blockcell_list_skills_no_evolution_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let skills_dir = workspace.join("skills").join("release_checklist");
        let records_dir = workspace.join("evolution_records");
        std::fs::create_dir_all(&skills_dir).expect("create skill dir");
        std::fs::create_dir_all(&records_dir).expect("create records dir");
        std::fs::write(skills_dir.join("SKILL.md"), "# Release checklist\n").expect("write skill");
        std::fs::write(
            skills_dir.join("meta.yaml"),
            "name: release_checklist\ndescription: Release checklist\nsource: blockcell\n",
        )
        .expect("write meta");
        std::fs::write(
            records_dir.join("legacy.json"),
            serde_json::json!({
                "id": "legacy-evolution",
                "skill_name": "old_pipeline",
                "status": "Generating"
            })
            .to_string(),
        )
        .expect("write legacy record");

        let result = tool
            .execute(
                tool_context_with_workspace(workspace.clone(), None),
                json!({"query": "all"}),
            )
            .await
            .expect("list skills");

        assert!(result.get("learning").is_none());
        assert!(result.get("evolution_records").is_none());
        assert_eq!(result["count"], json!(1));
        assert_eq!(
            result["learned_skills"][0]["name"],
            json!("release_checklist")
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn test_read_skill_meta_missing() {
        let tool = ListSkillsTool;
        let meta = tool.read_skill_meta(std::path::Path::new("/nonexistent"));
        assert!(meta.is_object());
        assert!(meta.as_object().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_available_skills_includes_python_skill() {
        let tool = ListSkillsTool;

        let mut root = std::env::temp_dir();
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        root.push(format!(
            "blockcell_list_skills_py_{}_{}",
            std::process::id(),
            now_ns
        ));

        let py_skill_dir = root.join("py_demo_skill");
        std::fs::create_dir_all(&py_skill_dir).expect("create py skill dir");
        std::fs::write(py_skill_dir.join("SKILL.py"), "print('ok')\n").expect("write SKILL.py");
        std::fs::write(
            py_skill_dir.join("meta.yaml"),
            "name: py_demo_skill\ndescription: python skill\n",
        )
        .expect("write meta.yaml");

        let result = tool
            .get_available_skills(&root, &[])
            .await
            .expect("get available skills");

        let count = result.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
        assert_eq!(count, 1);

        let names: Vec<String> = result
            .get("learned_skills")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        v.get("name")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert!(names.iter().any(|n| n == "py_demo_skill"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn test_list_skills_metadata_reports_script_assets_consistently() {
        let tool = ListSkillsTool;

        let mut root = std::env::temp_dir();
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        root.push(format!(
            "blockcell_list_skills_assets_{}_{}",
            std::process::id(),
            now_ns
        ));

        let skill_dir = root.join("asset_skill");
        std::fs::create_dir_all(skill_dir.join("scripts")).expect("create scripts dir");
        std::fs::create_dir_all(skill_dir.join("bin")).expect("create bin dir");
        std::fs::write(skill_dir.join("SKILL.rhai"), "set_output(\"compat\");\n")
            .expect("write SKILL.rhai");
        std::fs::write(skill_dir.join("SKILL.py"), "print('compat')\n").expect("write SKILL.py");
        std::fs::write(skill_dir.join("SKILL.md"), "# Asset skill\n").expect("write SKILL.md");
        std::fs::write(
            skill_dir.join("scripts/flow.rhai"),
            "set_output(\"nested\");\n",
        )
        .expect("write scripts/flow.rhai");
        std::fs::write(skill_dir.join("scripts/report.py"), "print('nested')\n")
            .expect("write scripts/report.py");
        std::fs::write(skill_dir.join("bin/run"), "#!/bin/sh\necho ok\n").expect("write bin/run");
        #[cfg(unix)]
        {
            let mut perms = std::fs::metadata(skill_dir.join("bin/run"))
                .expect("stat bin/run")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(skill_dir.join("bin/run"), perms).expect("chmod bin/run");
        }

        let result = tool
            .get_available_skills(&root, &[])
            .await
            .expect("get available skills");

        let skill = result
            .get("learned_skills")
            .and_then(|v| v.as_array())
            .and_then(|skills| {
                skills.iter().find(|skill| {
                    skill.get("name").and_then(|value| value.as_str()) == Some("asset_skill")
                })
            })
            .expect("find asset_skill entry");

        assert_eq!(skill.get("has_rhai"), Some(&Value::Bool(true)));
        assert_eq!(skill.get("has_py"), Some(&Value::Bool(true)));
        assert_eq!(skill.get("has_md"), Some(&Value::Bool(true)));
        assert_eq!(skill.get("has_script_assets"), Some(&Value::Bool(true)));

        let script_assets = skill
            .get("script_assets")
            .and_then(|value| value.as_array())
            .expect("script_assets should be an array");
        let asset_paths: Vec<&str> = script_assets
            .iter()
            .filter_map(|value| value.as_str())
            .collect();

        assert!(asset_paths.contains(&"SKILL.rhai"));
        assert!(asset_paths.contains(&"SKILL.py"));
        assert!(asset_paths.contains(&"scripts/flow.rhai"));
        assert!(asset_paths.contains(&"scripts/report.py"));
        // bin/run 无扩展名，在 Windows 上无法通过扩展名或可执行位检测为脚本资产
        #[cfg(not(target_os = "windows"))]
        assert!(asset_paths.contains(&"bin/run"));

        let _ = std::fs::remove_dir_all(root);
    }
}
