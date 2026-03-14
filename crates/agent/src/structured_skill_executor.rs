use blockcell_core::{Error, Result};
use blockcell_skills::manager::SkillExecution;
use serde_json::json;
use serde_json::{Map, Value};

#[derive(Debug, Clone)]
pub(crate) struct StructuredDispatchPlan {
    pub method_name: String,
    #[allow(dead_code)]
    pub arguments: Map<String, Value>,
    pub argv: Vec<String>,
}

pub(crate) struct StructuredSkillExecutor;

impl StructuredSkillExecutor {
    pub(crate) fn build_dispatch_plan(
        skill_name: &str,
        execution: &SkillExecution,
        dispatch_json: &Value,
    ) -> Result<StructuredDispatchPlan> {
        let method_name = dispatch_json
            .get("method")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Skill("LLM did not return a 'method' field in JSON".to_string()))?
            .to_string();

        let arguments = dispatch_json
            .get("arguments")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();

        let action = execution
            .actions
            .iter()
            .find(|action| action.name == method_name)
            .ok_or_else(|| {
                let available = execution
                    .actions
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                Error::Skill(format!(
                    "Method '{}' not found in skill '{}' (available: {})",
                    method_name, skill_name, available
                ))
            })?;

        for (arg_name, arg_spec) in &action.arguments {
            if arg_spec.required && !arguments.contains_key(arg_name.as_str()) {
                return Err(Error::Skill(format!(
                    "Required argument '{}' missing for method '{}'",
                    arg_name, method_name
                )));
            }

            if !arg_spec.enum_values.is_empty() {
                if let Some(value) = arguments.get(arg_name.as_str()).and_then(|v| v.as_str()) {
                    if !arg_spec
                        .enum_values
                        .iter()
                        .any(|enum_value| enum_value == value)
                    {
                        return Err(Error::Skill(format!(
                            "Argument '{}' has invalid value '{}' for method '{}'",
                            arg_name, value, method_name
                        )));
                    }
                }
            }
        }

        let argv = action
            .argv
            .iter()
            .map(|template| {
                let mut arg = template.clone();
                for (k, v) in &arguments {
                    let placeholder = format!("{{{}}}", k);
                    let value = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    arg = arg.replace(&placeholder, &value);
                }
                arg
            })
            .collect();

        Ok(StructuredDispatchPlan {
            method_name,
            arguments,
            argv,
        })
    }

    pub(crate) fn build_rhai_context(dispatch_plan: &StructuredDispatchPlan) -> Value {
        json!({
            "invocation": {
                "method": dispatch_plan.method_name,
                "arguments": dispatch_plan.arguments,
            }
        })
    }
}
