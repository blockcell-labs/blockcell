use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::subagent::{SubagentKind, SubagentPermissions, SubagentRequest};
use crate::{Tool, ToolContext, ToolSchema};

/// Agent 工具参数
#[derive(Debug, Deserialize)]
pub struct AgentToolParams {
    /// Agent 类型 (省略则使用 Fork 模式 - 继承父级上下文)
    pub subagent_type: Option<String>,

    /// 任务描述 - 发送给子 Agent 的完整提示
    pub prompt: String,

    /// 简短描述 (3-5 个词) 用于显示和通知
    pub description: Option<String>,

    /// 强制 spawn，即使同类型任务已在运行
    #[serde(default)]
    pub force: bool,

    /// Workspace-relative files or directories the typed agent may modify.
    #[serde(default)]
    pub workspace_scope: Vec<String>,
}

/// Agent 工具 - 启动专用子 Agent 处理复杂多步任务
///
/// Fork 模式 (省略 subagent_type):
/// - 继承父级完整对话上下文
/// - 共享 Prompt Cache 提高效率
/// - 同步执行，直接返回结果 (不是 task_id)
/// - 不能继续 spawn 子 Agent (防止递归)
///
/// Typed Agent (指定 subagent_type):
/// - 后台运行，返回 task_id 用于进度追踪
pub struct AgentTool;

#[async_trait]
impl Tool for AgentTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "agent".to_string(),
            description: "Launch a new agent to handle complex, multi-step tasks autonomously. \
Available built-in types: explore (codebase exploration), plan (implementation planning), \
coder (bounded code implementation), reviewer (read-only code review), tester (test execution), \
verification (testing/validation), viper (production code), general (complex tasks). \
Custom types may also be available from ~/.blockcell/workspace/agents/ or .blockcell/agents/. \
Omit subagent_type for fork mode (inherits parent context, shares prompt cache, synchronous). \
Specify subagent_type for typed agents (background execution, returns task_id)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "subagent_type": {
                        "type": "string",
                        "description": "Agent type to use. Omit for fork mode (inherits parent conversation context, shares prompt cache). Available types include built-in and custom agents defined in ~/.blockcell/workspace/agents/ or .blockcell/agents/."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The task for the agent to perform. Be specific and include relevant details. For open-ended research, describe what information is needed."
                    },
                    "description": {
                        "type": "string",
                        "description": "A short (3-5 word) description of the task. Shown in notifications and task lists."
                    },
                    "force": {
                        "type": "boolean",
                        "default": false,
                        "description": "Force spawn even if a same-type agent task is already running. Use when the previous task result is stale or the user explicitly wants a new task."
                    },
                    "workspace_scope": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Workspace-relative files or directories assigned to this typed agent. Directory scopes should end with '/'. Required for coder agents; overlapping coder scopes fail fast."
                    }
                },
                "required": ["prompt"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let prompt = params
            .get("prompt")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty());

        if prompt.is_none() {
            return Err(Error::Validation(
                "'prompt' 是必填字段且不能为空".to_string(),
            ));
        }

        if let Some(type_str) = params.get("subagent_type").and_then(|v| v.as_str()) {
            // 验证 subagent_type: 基本格式检查
            // 运行时 execute() 会通过 AgentTypeRegistry 做完整验证
            let char_count = type_str.chars().count();
            if !(3..=50).contains(&char_count) {
                return Err(Error::Validation(format!(
                    "subagent_type 长度无效: {} (需要 3-50 字符)",
                    type_str
                )));
            }
            // 检查字符格式：只允许字母、数字、连字符
            let chars: Vec<char> = type_str.chars().collect();
            if !chars[0].is_ascii_alphanumeric() {
                return Err(Error::Validation(format!(
                    "subagent_type '{}' 必须以字母或数字开头",
                    type_str
                )));
            }
            if !chars[chars.len() - 1].is_ascii_alphanumeric() {
                return Err(Error::Validation(format!(
                    "subagent_type '{}' 必须以字母或数字结尾",
                    type_str
                )));
            }
            for &ch in &chars {
                if !ch.is_ascii_alphanumeric() && ch != '-' {
                    return Err(Error::Validation(format!(
                        "subagent_type '{}' 包含非法字符 '{}'",
                        type_str, ch
                    )));
                }
            }

            if type_str == "coder" {
                let scope = params
                    .get("workspace_scope")
                    .and_then(Value::as_array)
                    .filter(|entries| !entries.is_empty())
                    .ok_or_else(|| {
                        Error::Validation("coder agent 必须提供非空 workspace_scope".to_string())
                    })?;
                for entry in scope {
                    let raw = entry
                        .as_str()
                        .filter(|value| !value.trim().is_empty())
                        .ok_or_else(|| {
                            Error::Validation("workspace_scope 只能包含非空字符串".to_string())
                        })?;
                    let path = std::path::Path::new(raw);
                    if path.is_absolute()
                        || path.components().any(|component| {
                            matches!(
                                component,
                                std::path::Component::ParentDir
                                    | std::path::Component::RootDir
                                    | std::path::Component::Prefix(_)
                            )
                        })
                    {
                        return Err(Error::Validation(format!(
                            "workspace_scope 必须是 workspace 内的安全相对路径: {raw}"
                        )));
                    }
                }
            }
        }

        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let parsed: AgentToolParams = serde_json::from_value(params)?;

        // ===== 检查 spawn 权限 =====
        if !ctx.can_spawn_subagent() {
            return Ok(json!({
                "error": "Cannot spawn subagent",
                "reason": "ForkChild 和 ONE_SHOT Agent 不能继续 spawn 子 Agent",
                "hint": "请直接使用工具 (Read, Grep, Write, Edit)"
            }));
        }

        // 检查 runtime handle 是否可用
        let runtime_handle = ctx
            .runtime_handle
            .as_ref()
            .ok_or_else(|| Error::Tool("Runtime handle 不可用".to_string()))?;

        let subagents = runtime_handle.subagents();

        // 如果省略 subagent_type -> Fork 模式 (同步执行，只读权限)
        if parsed.subagent_type.is_none() {
            let handle = subagents
                .spawn(SubagentRequest {
                    kind: SubagentKind::Fork,
                    prompt: parsed.prompt,
                    description: parsed.description,
                    permissions: SubagentPermissions::read_only(),
                })
                .await?;
            let result = handle.result().await?;
            return Ok(json!({
                "mode": "fork",
                "result": result,
                "completed": true
            }));
        }

        // Typed Agent 执行 (始终后台运行)
        let agent_type = parsed.subagent_type.unwrap();

        // ===== 从注册表验证 agent_type =====
        if let Some(ref registry) = ctx.agent_type_registry {
            if !registry.has_type(&agent_type) {
                let available = registry.type_names();
                return Err(Error::Validation(format!(
                    "无效的 subagent_type '{}', 可用类型: {}",
                    agent_type,
                    available.join(", ")
                )));
            }
        }

        // ===== 检查是否有同类型的 Running 任务 =====
        if !parsed.force && agent_type != "coder" {
            if let Some(ref tm) = ctx.task_manager {
                let running_tasks = tm.list_tasks_json(Some("running".to_string())).await;
                if let Some(tasks_array) = running_tasks.as_array() {
                    let same_type_running: Vec<_> = tasks_array
                        .iter()
                        .filter(|t| {
                            t.get("agent_type")
                                .and_then(|v| v.as_str())
                                .map(|at| at == agent_type)
                                .unwrap_or(false)
                        })
                        .collect();

                    if !same_type_running.is_empty() {
                        let running_ids: Vec<String> = same_type_running
                            .iter()
                            .filter_map(|t| {
                                t.get("id").and_then(|v| v.as_str()).map(|s| {
                                    let short: String = s.chars().take(12).collect();
                                    format!("{} ({})", short, agent_type)
                                })
                            })
                            .collect();

                        return Ok(json!({
                            "mode": "typed",
                            "status": "duplicate_check",
                            "message": format!(
                                "已有 {} Agent 任务在运行 (task_id: {}). \
                                 如需强制启动新任务，请使用 force=true. \
                                 如需等待现有任务完成，请使用 /tasks 监控进度.",
                                agent_type,
                                running_ids.join(", ")
                            ),
                            "running_task_ids": running_ids,
                            "agent_type": agent_type,
                            "hint": "设置 force=true 强制 spawn，或等待现有任务完成"
                        }));
                    }
                }
            }
        }

        let handle = subagents
            .spawn(SubagentRequest {
                kind: SubagentKind::Typed { agent_type },
                prompt: parsed.prompt,
                description: parsed.description,
                permissions: SubagentPermissions::agent_type(parsed.workspace_scope),
            })
            .await?;

        Ok(json!({
            "mode": "typed",
            "task_id": handle.id()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_schema() {
        let tool = AgentTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "agent");
        // 检查描述包含关键信息
        let desc = schema.description;
        assert!(desc.contains("explore"));
        assert!(desc.contains("plan"));
        assert!(desc.contains("coder"));
        assert!(desc.contains("reviewer"));
        assert!(desc.contains("tester"));
        assert!(desc.contains("verification"));
        assert!(desc.contains("viper"));
        assert!(desc.contains("general"));
        assert!(desc.contains("fork mode"));
        assert!(desc.contains("background"));
    }

    #[test]
    fn test_agent_schema_parameters() {
        let tool = AgentTool;
        let schema = tool.schema();
        let params = schema.parameters.as_object().unwrap();
        let props = params.get("properties").unwrap().as_object().unwrap();

        // 检查必填字段
        assert!(params
            .get("required")
            .unwrap()
            .as_array()
            .unwrap()
            .contains(&json!("prompt")));

        // 检查所有属性存在
        assert!(props.contains_key("subagent_type"));
        assert!(props.contains_key("prompt"));
        assert!(props.contains_key("description"));
        assert!(props.contains_key("force"));
        assert!(props.contains_key("workspace_scope"));
    }

    #[test]
    fn test_agent_validate_prompt_required() {
        let tool = AgentTool;
        // 有效: 有 prompt
        assert!(tool
            .validate(&json!({"prompt": "analyze the codebase"}))
            .is_ok());
        // 无效: 没有 prompt
        assert!(tool.validate(&json!({})).is_err());
        // 无效: 空 prompt
        assert!(tool.validate(&json!({"prompt": ""})).is_err());
        assert!(tool.validate(&json!({"prompt": "   \n\t"})).is_err());
    }

    #[test]
    fn test_agent_validate_subagent_type() {
        let tool = AgentTool;
        // 有效内置类型
        for agent_type in [
            "explore",
            "plan",
            "reviewer",
            "tester",
            "verification",
            "viper",
            "general",
        ] {
            assert!(tool
                .validate(&json!({"prompt": "test", "subagent_type": agent_type}))
                .is_ok());
        }
        assert!(tool
            .validate(&json!({
                "prompt": "test",
                "subagent_type": "coder",
                "workspace_scope": ["src/"]
            }))
            .is_ok());
        // 自定义类型 (长度有效) - validate 只做基本格式检查
        assert!(tool
            .validate(&json!({"prompt": "test", "subagent_type": "code-reviewer"}))
            .is_ok());
        // 无效类型 (长度太短)
        assert!(tool
            .validate(&json!({"prompt": "test", "subagent_type": "ab"}))
            .is_err());
    }

    #[test]
    fn test_agent_validate_optional_fields() {
        let tool = AgentTool;
        // description 是可选的
        assert!(tool
            .validate(&json!({"prompt": "test", "description": "short desc"}))
            .is_ok());
        // 所有可选字段
        assert!(tool
            .validate(&json!({
                "prompt": "test",
                "subagent_type": "explore",
                "description": "code analysis"
            }))
            .is_ok());
    }

    #[test]
    fn coder_requires_safe_workspace_scope() {
        let tool = AgentTool;

        assert!(tool
            .validate(&json!({"prompt": "edit", "subagent_type": "coder"}))
            .is_err());
        assert!(tool
            .validate(&json!({
                "prompt": "edit",
                "subagent_type": "coder",
                "workspace_scope": ["src/", "tests/example.rs"]
            }))
            .is_ok());
        assert!(tool
            .validate(&json!({
                "prompt": "escape",
                "subagent_type": "coder",
                "workspace_scope": ["../outside"]
            }))
            .is_err());
    }

    #[test]
    fn test_agent_params_deserialization() {
        // 最小参数
        let params: AgentToolParams =
            serde_json::from_value(json!({"prompt": "test prompt"})).unwrap();
        assert_eq!(params.prompt, "test prompt");
        assert!(params.subagent_type.is_none());
        assert!(params.description.is_none());
        assert!(params.workspace_scope.is_empty());

        // 完整参数
        let params: AgentToolParams = serde_json::from_value(json!({
            "prompt": "analyze code",
            "subagent_type": "explore",
            "description": "code analysis"
        }))
        .unwrap();
        assert_eq!(params.prompt, "analyze code");
        assert_eq!(params.subagent_type, Some("explore".to_string()));
        assert_eq!(params.description, Some("code analysis".to_string()));
    }

    #[test]
    fn test_agent_fork_mode_detection() {
        // Fork 模式: 没有 subagent_type
        let params: AgentToolParams =
            serde_json::from_value(json!({"prompt": "fork task"})).unwrap();
        assert!(params.subagent_type.is_none());

        // Typed 模式: 有 subagent_type
        let params: AgentToolParams = serde_json::from_value(json!({
            "prompt": "typed task",
            "subagent_type": "explore"
        }))
        .unwrap();
        assert!(params.subagent_type.is_some());
    }

    #[test]
    fn test_agent_schema_description() {
        let tool = AgentTool;
        let schema = tool.schema();
        // 检查描述包含关键信息
        assert!(schema.description.contains("explore"));
        assert!(schema.description.contains("plan"));
        assert!(schema.description.contains("fork mode"));
        assert!(schema.description.contains("background"));
    }
}
