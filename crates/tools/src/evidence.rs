//! Execution annotations describe intent; only a completed result proves success.
use blockcell_core::{task_contract::TaskContract, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkspaceEffect {
    None,
    Changed,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationKind {
    Test,
    Build,
    Lint,
    Syntax,
    Artifact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationIntent {
    pub kind: VerificationKind,
    pub target: String,
}

#[derive(Debug, Clone, Default)]
pub struct ExecutionAnnotations {
    pub task_contract: Option<TaskContract>,
    pub verification: Option<VerificationIntent>,
}

impl ExecutionAnnotations {
    pub fn parse(params: &Value) -> Result<Self> {
        let task_contract = params
            .get("task_contract")
            .map(|value| {
                let contract: TaskContract = serde_json::from_value(value.clone())?;
                contract.validate()?;
                Ok::<_, Error>(contract)
            })
            .transpose()?;
        let verification = params
            .get("verification")
            .map(|value| {
                let intent: VerificationIntent = serde_json::from_value(value.clone())?;
                if intent.target.trim().is_empty() || intent.target.len() > 4096 {
                    return Err(Error::Validation(
                        "Verification requires a bounded, non-empty target".into(),
                    ));
                }
                Ok(intent)
            })
            .transpose()?;
        if verification.is_some()
            && task_contract
                .as_ref()
                .is_some_and(|contract| !contract.verification.permits_execution())
        {
            return Err(Error::Validation(
                "Verification cannot be executed with a forbidden/deferred task contract".into(),
            ));
        }
        Ok(Self {
            task_contract,
            verification,
        })
    }
}

pub fn task_contract_schema() -> Value {
    json!({"type":"object","description":"Requirements explicitly stated by the user. Never invent requirements or override a user prohibition. No permissions are granted.","properties":{
        "verification":{"type":"string","enum":["allowed","required","deferred","forbidden"]},
        "deliverables":{"type":"array","maxItems":32,"items":{"type":"object","properties":{"path":{"type":"string"},"required":{"type":"boolean"},"must_update":{"type":"boolean","description":"Require creation/update during this task; do not accept an old existing copy."}},"required":["path"],"additionalProperties":false}}
    },"additionalProperties":false})
}

pub fn add_annotation_schema(parameters: &mut Value, supports_verification: bool) {
    let Some(properties) = parameters
        .get_mut("properties")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    properties.insert("task_contract".into(), task_contract_schema());
    if !supports_verification {
        return;
    }
    properties.insert("verification".into(), json!({"type":"object","description":"Set only when this command actually verifies the named target. Use a command whose exit status preserves check failures; a declaration alone is not proof. Omit when not verifying. Never execute verification forbidden/deferred by the user.","properties":{"kind":{"type":"string","enum":["test","build","lint","syntax","artifact"]},"target":{"type":"string"}},"required":["kind","target"],"additionalProperties":false}));
}

pub fn completed_successfully(result: &Value) -> bool {
    result["exit_code"].as_i64() == Some(0)
        && result.get("running").is_none_or(|running| running == false)
        && result
            .get("timed_out")
            .is_none_or(|timed_out| timed_out == false)
}
