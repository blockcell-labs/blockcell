use async_trait::async_trait;
use blockcell_core::{Error, Result};
use blockcell_skills::dispatcher::{SkillDispatchResult, SkillDispatcher};
use blockcell_skills::SkillMeta;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::exec_local::{
    resolve_script_path, validate_relative_skill_path, validate_runner, ExecLocalTool,
};
use crate::registry::ToolRegistry;
use crate::tool_guard::{current_nested_tool_admission, NestedToolGuard};
use crate::{Tool, ToolContext, ToolSchema};

#[derive(Default)]
pub struct ExecSkillScriptTool {
    process: ExecLocalTool,
}

impl ExecSkillScriptTool {
    pub fn from_config(config: &blockcell_core::config::ExecConfig) -> Self {
        Self {
            process: ExecLocalTool::from_config(config),
        }
    }
}

fn nested_tool_registry(config: &blockcell_core::Config) -> ToolRegistry {
    let mut registry = ToolRegistry::with_defaults();
    registry.register(Arc::new(ExecLocalTool::from_config(&config.tools.exec)));
    registry.register(Arc::new(crate::exec::ExecTool::from_config(
        &config.tools.exec,
    )));
    registry
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScriptRuntime {
    Rhai,
    Process,
}

fn resolve_runtime(path: &str) -> ScriptRuntime {
    if path.ends_with(".rhai") {
        ScriptRuntime::Rhai
    } else {
        ScriptRuntime::Process
    }
}

fn load_declared_skill_tools(skill_dir: &std::path::Path) -> Result<HashSet<String>> {
    let meta_path = skill_dir.join("meta.yaml");
    if !meta_path.exists() {
        return Ok(HashSet::new());
    }
    let metadata = std::fs::read_to_string(&meta_path).map_err(|error| {
        Error::Tool(format!(
            "Failed to read skill metadata '{}': {}",
            meta_path.display(),
            error
        ))
    })?;
    let metadata: SkillMeta = serde_yaml::from_str(&metadata).map_err(|error| {
        Error::Tool(format!(
            "Failed to parse skill metadata '{}': {}",
            meta_path.display(),
            error
        ))
    })?;
    Ok(metadata.effective_tools().into_iter().collect())
}

fn parse_args(params: &Value) -> Result<Vec<String>> {
    params
        .get("args")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| Error::Validation("`args` must be an array of strings".to_string()))
        })
        .collect()
}

fn normalize_process_result(path: &str, result: Value) -> Value {
    json!({
        "runtime": "process",
        "path": path,
        "resolved_path": result.get("resolved_path").cloned().unwrap_or(Value::Null),
        "success": result.get("exit_code").and_then(|value| value.as_i64()) == Some(0),
        "exit_code": result.get("exit_code").cloned().unwrap_or(Value::Null),
        "stdout": result.get("stdout").cloned().unwrap_or(Value::String(String::new())),
        "stderr": result.get("stderr").cloned().unwrap_or(Value::String(String::new())),
        "command": result.get("command").cloned().unwrap_or(Value::Null),
    })
}

fn normalize_rhai_result(
    path: &str,
    resolved_path: &std::path::Path,
    result: SkillDispatchResult,
) -> Value {
    json!({
        "runtime": "rhai",
        "path": path,
        "resolved_path": resolved_path.display().to_string(),
        "success": result.success,
        "output": result.output,
        "error": result.error,
        "tool_calls": result.tool_calls.iter().map(|call| {
            json!({
                "tool_name": call.tool_name,
                "params": call.params,
                "result": call.result,
                "success": call.success,
            })
        }).collect::<Vec<_>>(),
    })
}

#[async_trait]
impl Tool for ExecSkillScriptTool {
    fn command_operation(&self, _params: &Value) -> crate::evidence::CommandOperation {
        crate::evidence::CommandOperation::Opaque
    }
    fn execution_environment(&self, params: &Value) -> crate::ExecutionEnvironment {
        if params
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| resolve_runtime(path) == ScriptRuntime::Rhai)
        {
            crate::ExecutionEnvironment::FullAccess
        } else {
            self.process.execution_environment(params)
        }
    }

    fn requires_confirmation(&self, params: &Value) -> bool {
        !params
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| resolve_runtime(path) == ScriptRuntime::Rhai)
            && self.process.requires_confirmation(params)
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "exec_skill_script".to_string(),
            description: "Execute a skill-local script asset. `.rhai` runs in-process; other paths run like exec_local. The `path` must be RELATIVE (e.g. `scripts/run.py`), never absolute. If the skill manual shows `{baseDir}/scripts/...`, strip the `{baseDir}/` prefix.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "RELATIVE path to the script inside the active skill directory (e.g. `scripts/run.py`). Must NOT be absolute. The tool auto-infers the interpreter from the file extension (.py→python3, .sh→sh, .js→node)."
                    },
                    "runner": {
                        "type": "string",
                        "description": "Optional interpreter override. Allowed: python3, python, bash, sh, node, php, uv. Auto-inferred from extension when omitted."
                    },
                    "args": {
                        "type": "array",
                        "description": "Arguments passed to a process-backed asset.",
                        "items": {
                            "type": "string"
                        }
                    },
                    "cwd_mode": {
                        "type": "string",
                        "description": "Working directory mode. Only `skill` is supported.",
                        "enum": ["skill"]
                    },
                    "user_input": {
                        "type": "string",
                        "description": "Optional Rhai `user_input` value."
                    },
                    "context": {
                        "type": "object",
                        "description": "Optional Rhai context variables."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let path = params
            .get("path")
            .and_then(|value| value.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: path".to_string()))?;
        validate_relative_skill_path(path)?;

        if let Some(runner) = params.get("runner").and_then(|value| value.as_str()) {
            validate_runner(runner)?;
        }

        let _ = parse_args(params)?;

        if let Some(cwd_mode) = params.get("cwd_mode").and_then(|value| value.as_str()) {
            if cwd_mode != "skill" {
                return Err(Error::Validation(
                    "`cwd_mode` only supports `skill`".to_string(),
                ));
            }
        }

        if params
            .get("context")
            .is_some_and(|value| !value.is_object())
        {
            return Err(Error::Validation(
                "`context` must be an object when provided".to_string(),
            ));
        }

        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let skill_dir = ctx.active_skill_dir.clone().ok_or_else(|| {
            // Use Tool error (not PermissionDenied) to avoid Permanent classification.
            // Hint tells the LLM to use activate_skill first.
            Error::Tool(
                "exec_skill_script requires an active skill context. \
                Use the `activate_skill` tool first, e.g. \
                activate_skill({skill_name: \"<skill-name>\", goal: \"<goal>\"})"
                    .to_string(),
            )
        })?;
        let path = params
            .get("path")
            .and_then(|value| value.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: path".to_string()))?
            .to_string();

        match resolve_runtime(&path) {
            ScriptRuntime::Process => {
                let result = self.process.execute(ctx, params).await?;
                Ok(normalize_process_result(&path, result))
            }
            ScriptRuntime::Rhai => {
                let resolved_path = resolve_script_path(&skill_dir, &path)?;
                let script = std::fs::read_to_string(&resolved_path).map_err(|error| {
                    Error::Tool(format!(
                        "Failed to read skill script '{}': {}",
                        resolved_path.display(),
                        error
                    ))
                })?;
                let user_input = params
                    .get("user_input")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let mut context_vars = HashMap::new();
                if let Some(context) = params.get("context").and_then(|value| value.as_object()) {
                    for (key, value) in context {
                        context_vars.insert(key.clone(), value.clone());
                    }
                }

                let handle = tokio::runtime::Handle::current();
                let rhai_ctx = ctx.clone();
                let declared_tools = load_declared_skill_tools(&skill_dir)?;
                let admission = current_nested_tool_admission();
                let dispatcher = SkillDispatcher::new();
                let result = tokio::task::spawn_blocking(move || {
                    // Nested exec must use the same configured shell policy as
                    // the parent agent. A process-wide default registry would
                    // silently fall back to workspace-write here.
                    let registry = nested_tool_registry(&rhai_ctx.config);
                    // 嵌套调用不经过 Runtime 的 Guarded Tool Pipeline，由守卫执行同一套准入检查与审计
                    let guard = NestedToolGuard::with_admission(&rhai_ctx, admission);
                    dispatcher.execute_sync(
                        &script,
                        &user_input,
                        context_vars,
                        move |tool_name, tool_params| {
                            if tool_name == "exec_skill_script" {
                                return Err(Error::Tool(
                                    "Nested `exec_skill_script` execution is not supported inside Rhai assets".to_string(),
                                ));
                            }
                            if !declared_tools.contains(tool_name) {
                                return Err(Error::Tool(format!(
                                    "Tool '{}' is not declared by this skill's metadata",
                                    tool_name
                                )));
                            }
                            guard.admit(tool_name, &tool_params)?;
                            let registry = registry.clone();
                            let ctx = rhai_ctx.clone();
                            let audited_params = tool_params.clone();
                            let start = std::time::Instant::now();
                            let result = std::thread::scope(|scope| {
                                scope
                                    .spawn(|| {
                                        handle.block_on(async {
                                            registry.execute(tool_name, ctx, tool_params).await
                                        })
                                    })
                                    .join()
                                    .unwrap_or_else(|_| {
                                        Err(Error::Tool(
                                            "Nested tool execution panicked".to_string(),
                                        ))
                                    })
                            });
                            let (audited_result, outcome) = match &result {
                                Ok(value) => (value.clone(), "succeeded"),
                                Err(error) => (json!({ "error": error.to_string() }), "failed"),
                            };
                            guard.audit(
                                tool_name,
                                &audited_params,
                                audited_result,
                                outcome,
                                start.elapsed().as_millis() as u64,
                            );
                            result
                        },
                    )
                })
                .await
                .map_err(|error| Error::Tool(format!("Failed to join Rhai execution: {}", error)))??;

                Ok(normalize_rhai_result(&path, &resolved_path, result))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::Config;
    use serde_json::{json, Value};
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn temp_skill_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{}-{}", prefix, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp skill dir");
        dir
    }

    fn tool_context(skill_dir: PathBuf) -> ToolContext {
        let workspace = skill_dir.clone();
        ToolContext {
            workspace,
            sandbox_writable_roots: None,
            base: std::env::temp_dir().join("blockcell"),
            builtin_skills_dir: None,
            active_skill_dir: Some(skill_dir),
            session_key: "cli:test".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: None,
            sender_role: None,
            chat_id: "chat-1".to_string(),
            config: Config::default(),
            permissions: blockcell_core::types::PermissionSet::new(),
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

    async fn run_exec_skill_script(skill_dir: PathBuf, params: Value) -> Value {
        let mut registry = ToolRegistry::with_defaults();
        registry.register(Arc::new(ExecSkillScriptTool::from_config(
            &blockcell_core::config::ExecConfig {
                sandbox_policy: Some("full-access".into()),
                ..Default::default()
            },
        )));
        let tool = registry
            .get("exec_skill_script")
            .expect("exec_skill_script should be registered");
        tool.validate(&params).expect("params should validate");
        tool.execute(tool_context(skill_dir), params)
            .await
            .expect("exec_skill_script should succeed")
    }

    #[tokio::test]
    async fn test_exec_skill_script_runs_top_level_rhai() {
        let skill_dir = temp_skill_dir("blockcell-exec-skill-script-rhai-top");
        fs::write(
            skill_dir.join("SKILL.rhai"),
            r#"set_output("top-level-ok");"#,
        )
        .expect("write rhai script");

        let result = run_exec_skill_script(skill_dir, json!({"path": "SKILL.rhai"})).await;
        assert_eq!(result["runtime"], "rhai");
        assert_eq!(result["success"], true);
        assert_eq!(result["output"], "top-level-ok");
    }

    #[tokio::test]
    async fn test_exec_skill_script_runs_nested_rhai() {
        let skill_dir = temp_skill_dir("blockcell-exec-skill-script-rhai-nested");
        let nested_dir = skill_dir.join("scripts").join("nested");
        fs::create_dir_all(&nested_dir).expect("create nested script dir");
        fs::write(
            nested_dir.join("flow.rhai"),
            r#"set_output(#{ "message": "nested-ok" });"#,
        )
        .expect("write nested rhai script");

        let result =
            run_exec_skill_script(skill_dir, json!({"path": "scripts/nested/flow.rhai"})).await;
        assert_eq!(result["runtime"], "rhai");
        assert_eq!(result["success"], true);
        assert_eq!(result["output"]["message"], "nested-ok");
    }

    #[tokio::test]
    async fn test_rhai_rejects_tool_not_declared_by_skill() {
        let skill_dir = temp_skill_dir("blockcell-exec-skill-script-rhai-denied-tool");
        fs::write(
            skill_dir.join("SKILL.rhai"),
            r#"let result = call_tool("list_dir", #{ path: "." }); set_output(result);"#,
        )
        .expect("write rhai script");

        let result = run_exec_skill_script(skill_dir, json!({"path": "SKILL.rhai"})).await;
        assert_eq!(result["tool_calls"][0]["success"], false);
        assert!(result["tool_calls"][0]["result"]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("not declared"));
    }

    #[tokio::test]
    async fn test_rhai_nested_call_respects_tool_policy() {
        let skill_dir = temp_skill_dir("blockcell-exec-skill-script-rhai-policy");
        fs::write(
            skill_dir.join("meta.yaml"),
            "name: policy-tool\ndescription: test\ntools:\n  - list_dir\n",
        )
        .expect("write metadata");
        fs::write(
            skill_dir.join("SKILL.rhai"),
            r#"let result = call_tool("list_dir", #{ path: "." }); set_output(result);"#,
        )
        .expect("write rhai script");
        let base = temp_skill_dir("blockcell-exec-skill-script-policy-base");
        fs::write(
            base.join("tool_policy.yaml"),
            "rules:\n  - name: deny-list\n    tool: list_dir\n    decision: deny\n    description: list_dir is disabled\n",
        )
        .expect("write tool policy");

        let mut ctx = tool_context(skill_dir);
        ctx.base = base.clone();
        let result = ExecSkillScriptTool::default()
            .execute(ctx, json!({"path": "SKILL.rhai"}))
            .await
            .expect("exec_skill_script should succeed");

        assert_eq!(result["tool_calls"][0]["success"], false);
        assert!(result["tool_calls"][0]["result"]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("list_dir is disabled"));
        let audit = fs::read_dir(base.join("audit"))
            .expect("audit dir")
            .flatten()
            .map(|entry| fs::read_to_string(entry.path()).unwrap_or_default())
            .collect::<String>();
        assert!(audit.contains("list_dir"), "{audit}");
    }

    #[tokio::test]
    async fn test_rhai_allows_tool_declared_by_skill() {
        let skill_dir = temp_skill_dir("blockcell-exec-skill-script-rhai-allowed-tool");
        fs::write(
            skill_dir.join("meta.yaml"),
            "name: allowed-tool\ndescription: test\ntools:\n  - list_dir\n",
        )
        .expect("write metadata");
        fs::write(
            skill_dir.join("SKILL.rhai"),
            r#"let result = call_tool("list_dir", #{ path: "." }); set_output(result);"#,
        )
        .expect("write rhai script");

        let result = run_exec_skill_script(skill_dir, json!({"path": "SKILL.rhai"})).await;
        assert_eq!(result["runtime"], "rhai");
        assert_eq!(result["success"], true);
    }

    fn nested_file_fixture() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().expect("temporary fixture");
        let workspace = dir.path().join("workspace");
        let skill_dir = workspace.join("skills/file-access");
        let base = dir.path().join("base");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::create_dir_all(&base).unwrap();
        fs::write(
            skill_dir.join("meta.yaml"),
            "name: file-access\ndescription: test\ntools: [read_file, write_file, list_dir]\n",
        )
        .unwrap();
        fs::write(
            skill_dir.join("SKILL.rhai"),
            "let result = call_tool(tool_name, call_params); set_output(result);",
        )
        .unwrap();
        let mut ctx = tool_context(skill_dir);
        ctx.workspace = workspace;
        ctx.base = base;
        ctx.config.security.path_access.policy_file = String::new();
        write_file_path_policy(&ctx, json!([]));
        (dir, ctx)
    }

    fn write_file_path_policy(ctx: &ToolContext, rules: Value) {
        fs::write(
            ctx.base.join("path_access.json5"),
            json!({
                "version": 1,
                "default_policy": "confirm",
                "builtin_protected_paths": false,
                "rules": rules,
            })
            .to_string(),
        )
        .unwrap();
    }

    async fn run_nested_file_call(ctx: &ToolContext, name: &str, params: Value) -> Value {
        ExecSkillScriptTool::default()
            .execute(
                ctx.clone(),
                json!({
                    "path": "SKILL.rhai",
                    "context": {"tool_name": name, "call_params": params},
                }),
            )
            .await
            .expect("Rhai dispatcher should record the nested result")
    }

    #[tokio::test]
    async fn test_rhai_nested_path_allows_workspace_file() {
        let (_dir, ctx) = nested_file_fixture();
        fs::write(ctx.workspace.join("allowed.txt"), "allowed marker").unwrap();
        let result = run_nested_file_call(&ctx, "read_file", json!({"path": "allowed.txt"})).await;
        assert_eq!(result["tool_calls"][0]["success"], true, "{result}");
        assert!(result["output"].to_string().contains("allowed marker"));
    }

    #[tokio::test]
    async fn test_rhai_nested_path_denies_workspace_policy_block() {
        let (_dir, ctx) = nested_file_fixture();
        let blocked = ctx.workspace.join("blocked.txt");
        fs::write(&blocked, "blocked marker").unwrap();
        write_file_path_policy(
            &ctx,
            json!([{"name": "blocked", "action": "deny", "ops": ["read"], "paths": [blocked]}]),
        );
        let result = run_nested_file_call(&ctx, "read_file", json!({"path": "blocked.txt"})).await;
        assert_eq!(result["tool_calls"][0]["success"], false, "{result}");
        assert!(!result["output"].to_string().contains("blocked marker"));
    }

    #[tokio::test]
    async fn test_rhai_nested_path_rejects_unapproved_external_file() {
        let (dir, ctx) = nested_file_fixture();
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, "outside marker").unwrap();
        let result = run_nested_file_call(&ctx, "read_file", json!({"path": outside})).await;
        assert_eq!(result["tool_calls"][0]["success"], false, "{result}");
        assert!(!result["output"].to_string().contains("outside marker"));
    }

    #[tokio::test]
    async fn test_rhai_nested_path_allows_explicit_external_rule() {
        let (dir, ctx) = nested_file_fixture();
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, "approved marker").unwrap();
        write_file_path_policy(
            &ctx,
            json!([{"name": "approved", "action": "allow", "ops": ["read"], "paths": [outside]}]),
        );
        let result = run_nested_file_call(&ctx, "read_file", json!({"path": outside})).await;
        assert_eq!(result["tool_calls"][0]["success"], true, "{result}");
        assert!(result["output"].to_string().contains("approved marker"));
    }

    #[tokio::test]
    async fn test_rhai_nested_path_rejects_batch_before_writing_any_file() {
        let (_dir, ctx) = nested_file_fixture();
        let blocked = ctx.workspace.join("blocked.txt");
        fs::write(&blocked, "original marker").unwrap();
        write_file_path_policy(
            &ctx,
            json!([{"name": "blocked", "action": "deny", "ops": ["write"], "paths": [blocked]}]),
        );
        let result = run_nested_file_call(
            &ctx,
            "write_file",
            json!({"files": [
                {"path": "allowed.txt", "content": "first"},
                {"path": "blocked.txt", "content": "second"},
            ]}),
        )
        .await;
        assert_eq!(result["tool_calls"][0]["success"], false, "{result}");
        assert!(!ctx.workspace.join("allowed.txt").exists());
        assert_eq!(fs::read_to_string(blocked).unwrap(), "original marker");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_rhai_nested_path_rejects_symlink_to_unapproved_file() {
        let (dir, ctx) = nested_file_fixture();
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, "outside marker").unwrap();
        std::os::unix::fs::symlink(&outside, ctx.workspace.join("link.txt")).unwrap();
        let result = run_nested_file_call(&ctx, "read_file", json!({"path": "link.txt"})).await;
        assert_eq!(result["tool_calls"][0]["success"], false, "{result}");
        assert!(!result["output"].to_string().contains("outside marker"));
    }

    #[tokio::test]
    async fn test_rhai_nested_path_scopes_host_admission_across_blocking_dispatch() {
        use crate::tool_guard::{with_nested_tool_admission, NestedToolAdmission};

        let (dir, ctx) = nested_file_fixture();
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, "approved by host").unwrap();
        let approved_path = outside.clone();
        let allow: NestedToolAdmission = Arc::new(move |name, args| {
            if name == "read_file" && args["path"] == json!(approved_path) {
                Ok(())
            } else {
                Err(Error::PermissionDenied("not approved by host".into()))
            }
        });
        let deny: NestedToolAdmission = Arc::new(|_, _| {
            Err(Error::PermissionDenied(
                "denied by other host invocation".into(),
            ))
        });
        let (approved, denied) = tokio::join!(
            with_nested_tool_admission(
                allow,
                run_nested_file_call(&ctx, "read_file", json!({"path": outside})),
            ),
            with_nested_tool_admission(
                deny,
                run_nested_file_call(&ctx, "read_file", json!({"path": outside})),
            ),
        );
        assert_eq!(approved["tool_calls"][0]["success"], true, "{approved}");
        assert_eq!(denied["tool_calls"][0]["success"], false, "{denied}");
        let unscoped = run_nested_file_call(&ctx, "read_file", json!({"path": outside})).await;
        assert_eq!(unscoped["tool_calls"][0]["success"], false, "{unscoped}");
    }

    #[cfg(not(target_os = "windows"))]
    #[tokio::test]
    async fn test_exec_skill_script_runs_process_script() {
        let skill_dir = temp_skill_dir("blockcell-exec-skill-script-process");
        let scripts_dir = skill_dir.join("scripts");
        fs::create_dir_all(&scripts_dir).expect("create scripts dir");
        let script_path = scripts_dir.join("hello.sh");
        fs::write(&script_path, "#!/bin/sh\necho \"process $1\"\n").expect("write shell script");
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&script_path)
                .expect("script metadata")
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script_path, perms).expect("set script perms");
        }

        let result = run_exec_skill_script(
            skill_dir,
            json!({"path": "scripts/hello.sh", "args": ["ok"]}),
        )
        .await;

        assert_eq!(result["runtime"], "process");
        assert_eq!(result["success"], true);
        assert_eq!(result["exit_code"], 0);
        assert!(result["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("process ok"));
    }

    #[cfg(not(target_os = "windows"))]
    #[tokio::test]
    async fn test_exec_skill_script_runs_cli_binary() {
        let skill_dir = temp_skill_dir("blockcell-exec-skill-script-cli");
        let bin_dir = skill_dir.join("bin");
        fs::create_dir_all(&bin_dir).expect("create bin dir");
        let cli_path = bin_dir.join("hello");
        fs::write(&cli_path, "#!/bin/sh\necho \"cli $1\"\n").expect("write cli script");
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&cli_path).expect("cli metadata").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&cli_path, perms).expect("set cli perms");
        }

        let result =
            run_exec_skill_script(skill_dir, json!({"path": "bin/hello", "args": ["ok"]})).await;
        assert_eq!(result["runtime"], "process");
        assert_eq!(result["success"], true);
        assert_eq!(result["exit_code"], 0);
        assert!(result["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("cli ok"));
    }

    #[tokio::test]
    async fn test_exec_skill_script_rejects_parent_escape() {
        let skill_dir = temp_skill_dir("blockcell-exec-skill-script-escape");
        let registry = ToolRegistry::with_defaults();
        let tool = registry
            .get("exec_skill_script")
            .expect("exec_skill_script should be registered");

        let err = tool
            .execute(tool_context(skill_dir), json!({"path": "../outside.sh"}))
            .await
            .expect_err("parent escape should be rejected");
        assert!(format!("{}", err).contains("cannot escape"));
    }
}
