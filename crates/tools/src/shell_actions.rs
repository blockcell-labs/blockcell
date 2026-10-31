use async_trait::async_trait;
use blockcell_core::{Idempotency, Result};
use serde_json::{json, Value};

use crate::shell_session::ShellTool;
use crate::{ExecutionEnvironment, Tool, ToolContext, ToolSchema};

#[derive(Clone, Copy)]
enum ShellAction {
    Run,
    Poll,
    Input,
    Close,
}

pub struct ShellActionTool(ShellAction);

impl ShellActionTool {
    pub fn run() -> Self {
        Self(ShellAction::Run)
    }

    pub fn poll() -> Self {
        Self(ShellAction::Poll)
    }

    pub fn input() -> Self {
        Self(ShellAction::Input)
    }

    pub fn close() -> Self {
        Self(ShellAction::Close)
    }

    fn name(&self) -> &'static str {
        match self.0 {
            ShellAction::Run => "shell_run",
            ShellAction::Poll => "shell_poll",
            ShellAction::Input => "shell_input",
            ShellAction::Close => "shell_close",
        }
    }

    fn canonical_args(&self, params: &Value) -> Value {
        let action = match self.0 {
            ShellAction::Run => "run",
            ShellAction::Poll => "poll",
            ShellAction::Input => "write_stdin",
            ShellAction::Close => "close",
        };
        let mut args = params.as_object().cloned().unwrap_or_default();
        args.insert("action".to_string(), Value::String(action.to_string()));
        Value::Object(args)
    }
}

pub fn canonical_shell_args(tool_name: &str, params: &Value) -> Option<Value> {
    let action = match tool_name {
        "shell_run" => ShellAction::Run,
        "shell_poll" => ShellAction::Poll,
        "shell_input" => ShellAction::Input,
        "shell_close" => ShellAction::Close,
        _ => return None,
    };
    Some(ShellActionTool(action).canonical_args(params))
}

#[async_trait]
impl Tool for ShellActionTool {
    fn command_operation(&self, params: &Value) -> crate::evidence::CommandOperation {
        ShellTool.command_operation(&self.canonical_args(params))
    }
    fn supports_verification(&self) -> bool {
        matches!(self.0, ShellAction::Run)
    }
    fn coding_exposure(&self) -> crate::base_packs::CodingExposure {
        match self.0 {
            ShellAction::Run => crate::base_packs::CodingExposure::primary(),
            _ => crate::base_packs::CodingExposure::full(),
        }
    }

    fn workspace_effect(&self, params: &Value) -> crate::evidence::WorkspaceEffect {
        ShellTool.workspace_effect(&self.canonical_args(params))
    }
    fn accepts_execution_annotations(&self) -> bool {
        true
    }

    fn execution_environment(&self, params: &Value) -> ExecutionEnvironment {
        ShellTool.execution_environment(&self.canonical_args(params))
    }

    fn requires_confirmation(&self, params: &Value) -> bool {
        ShellTool.requires_confirmation(&self.canonical_args(params))
    }

    fn idempotency(&self, params: &Value) -> Idempotency {
        ShellTool.idempotency(&self.canonical_args(params))
    }

    fn schema(&self) -> ToolSchema {
        let (description, properties, required) = match self.0 {
            ShellAction::Run => (
                "Persistent shell; cwd/env persist; omit session_id to reuse.",
                json!({
                    "command": {"type": "string"},
                    "session_id": {"type": "string"},
                    "timeout_secs": {"type": "integer", "minimum": 0, "maximum": 3600}
                }),
                vec!["command"],
            ),
            ShellAction::Poll => (
                "Poll shell.",
                json!({
                    "session_id": {"type": "string"},
                    "timeout_secs": {"type": "integer", "minimum": 0, "maximum": 3600}
                }),
                vec!["session_id"],
            ),
            ShellAction::Input => (
                "Send input to a persistent shell session.",
                json!({"session_id": {"type": "string"}, "input": {"type": "string"}}),
                vec!["session_id", "input"],
            ),
            ShellAction::Close => (
                "Close a persistent shell session.",
                json!({"session_id": {"type": "string"}}),
                vec!["session_id"],
            ),
        };
        ToolSchema {
            name: self.name().to_string(),
            description: description.to_string(),
            parameters: json!({
                "type": "object",
                "properties": properties,
                "required": required
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        ShellTool.validate(&self.canonical_args(params))
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let mut args = self.canonical_args(&params);
        if matches!(self.0, ShellAction::Run)
            && args.get("session_id").and_then(Value::as_str).is_none()
        {
            if let Some(object) = args.as_object_mut() {
                object.insert(
                    "session_id".to_string(),
                    Value::String(format!("default-{}", ctx.session_key)),
                );
            }
        }
        ShellTool.execute(ctx, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_action_maps_to_legacy_shell_arguments() {
        let cases = [
            (
                "shell_run",
                json!({"command":"echo hi","session_id":"s"}),
                "run",
            ),
            ("shell_poll", json!({"session_id":"s"}), "poll"),
            (
                "shell_input",
                json!({"session_id":"s","input":"x"}),
                "write_stdin",
            ),
            ("shell_close", json!({"session_id":"s"}), "close"),
        ];
        for (name, args, action) in cases {
            let canonical = canonical_shell_args(name, &args).unwrap();
            assert_eq!(canonical["action"], action);
            for (key, value) in args.as_object().unwrap() {
                assert_eq!(&canonical[key], value);
            }
        }
    }

    #[test]
    fn shell_run_delegates_dangerous_validation_and_sandbox_policy() {
        let run = ShellActionTool::run();
        assert!(run.validate(&json!({"command":"rm -rf /"})).is_err());
        assert!(run.requires_confirmation(&json!({"sandbox_policy":"full-access"})));
        assert_eq!(
            run.execution_environment(&json!({"sandbox_policy":"full-access"})),
            ExecutionEnvironment::FullAccess
        );
    }
}
