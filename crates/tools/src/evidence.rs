//! Execution annotations describe intent; only a completed result proves success.
use blockcell_core::{task_contract::TaskContract, Error, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

/// Execution lifecycle is supplied by the tool, never inferred from caller fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CommandOperation {
    #[default]
    None,
    Launch,
    Poll,
    Input,
    Close,
    /// Scripts or delegation whose verification behavior is not inspectable.
    Opaque,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkspaceEffect {
    None,
    Changed,
    #[default]
    Unknown,
}

/// Conservative presentation-only classification. Never used for authorization.
/// Shell syntax, redirection, interpreters and extensible commands stay unknown.
pub fn is_plain_read_command(command: &str) -> bool {
    if command.contains([
        '\n', '\r', ';', '|', '&', '>', '<', '$', '`', '\\', '(', ')',
    ]) {
        return false;
    }
    let mut words = command.split_whitespace();
    let Some(program) = words.next() else {
        return false;
    };
    if !matches!(
        program,
        "cat" | "head" | "tail" | "wc" | "ls" | "pwd" | "stat" | "file"
    ) {
        return false;
    }
    // Do not assume option arguments are inert across extensible tool variants.
    !words.any(|word| {
        word.starts_with("--") || word.contains(['{', '}'])
        // BSD file -C creates a compiled magic database.
        || (program == "file" && word.starts_with('-') && word.contains('C'))
    })
}

pub use blockcell_core::config::VerificationKind;

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

fn parse_annotation<T: DeserializeOwned>(value: &Value, field: &str, hint: &str) -> Result<T> {
    serde_path_to_error::deserialize(value).map_err(|error| {
        let path = error.path().to_string();
        let mut field = if path == "." {
            field.to_owned()
        } else {
            format!("{field}.{path}")
        };
        let detail = error.inner().to_string();
        if let Some(missing) = detail
            .strip_prefix("missing field `")
            .and_then(|rest| rest.split('`').next())
        {
            field.push('.');
            field.push_str(missing);
        }
        Error::Validation(format!("{field}: {detail}. {hint}"))
    })
}

impl ExecutionAnnotations {
    pub fn parse(params: &Value) -> Result<Self> {
        let task_contract = params
            .get("task_contract")
            .map(|value| {
                let contract: TaskContract = parse_annotation(
                    value,
                    "task_contract",
                    r#"Use task_contract.verification = allowed, required, deferred or forbidden, and task_contract.deliverables as an array of objects. Example: {"verification":"required","deliverables":[{"path":"report.json"}]}"#,
                )?;
                contract.validate()?;
                Ok::<_, Error>(contract)
            })
            .transpose()?;
        let verification = params
            .get("verification")
            .map(|value| {
                let intent: VerificationIntent = parse_annotation(
                    value,
                    "verification",
                    r#"For a recognized check, omit verification and pass command alone. For an explicit annotation, use verification.kind = test, build, lint, syntax or artifact, with a non-empty verification.target. For unittest or pytest use {"kind":"test","target":"tests/test_slug.py"}."#,
                )?;
                if intent.target.trim().is_empty() || intent.target.len() > 4096 {
                    return Err(Error::Validation(
                        r#"verification.target must be a non-empty string of at most 4096 bytes, for example "tests/test_slug.py". Omit verification to infer intent from a recognized command."#.into(),
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
                "task_contract.verification is forbidden or deferred. Choose a non-verification action and omit the verification annotation.".into(),
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

pub fn add_annotation_schema(
    parameters: &mut Value,
    supports_verification: bool,
    expose_contract: bool,
) {
    let Some(properties) = parameters
        .get_mut("properties")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    if expose_contract {
        properties.insert("task_contract".into(), task_contract_schema());
    }
    if !supports_verification {
        return;
    }
    properties.insert("verification".into(), json!({"type":"object","description":"Optional. For ordinary checks, pass command alone so the recognized runner determines the kind. If explicitly annotating, provide both kind and target. Use kind test for unittest and pytest. Preserve failure exit status and never execute checks forbidden or deferred by the user.","properties":{"kind":{"type":"string","enum":["test","build","lint","syntax","artifact"]},"target":{"type":"string"}},"required":["kind","target"],"additionalProperties":false}));
}

pub fn completed_successfully(result: &Value) -> bool {
    result["exit_code"].as_i64() == Some(0)
        && result.get("running").is_none_or(|running| running == false)
        && result
            .get("timed_out")
            .is_none_or(|timed_out| timed_out == false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_errors_explain_how_to_correct_the_call() {
        for (params, expected, example) in [
            (
                json!({"verification":{"kind":"unittest","target":"test_slug.py"}}),
                "verification.kind",
                r#"{"kind":"test","target":"tests/test_slug.py"}"#,
            ),
            (
                json!({"verification":{"kind":"test"}}),
                "verification.target",
                "omit verification and pass command alone",
            ),
            (
                json!({"task_contract":{"output_files":["report.json"]}}),
                "task_contract.deliverables",
                r#"[{"path":"report.json"}]"#,
            ),
            (
                json!({"task_contract":{"deliverables":[{"path":42}]}}),
                "task_contract.deliverables[0].path",
                r#"[{"path":"report.json"}]"#,
            ),
        ] {
            let error = ExecutionAnnotations::parse(&params)
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
            assert!(error.contains(example), "{error}");
        }
    }

    #[test]
    fn legacy_contract_is_parsed_and_prohibitions_are_enforced() {
        let mut params = json!({"task_contract":{"verification":"deferred","deliverables":[{"path":"report.json"}]}});
        let contract = ExecutionAnnotations::parse(&params)
            .unwrap()
            .task_contract
            .unwrap();
        assert!(!contract.verification.permits_execution());
        assert_eq!(contract.deliverables[0].path, "report.json");

        params["verification"] = json!({"kind":"test","target":"tests"});
        assert!(ExecutionAnnotations::parse(&params).is_err());
    }
}

#[cfg(test)]
mod read_only_command_tests {
    use super::is_plain_read_command;

    #[test]
    fn plain_reads_are_recognised() {
        for command in [
            "cat notice.html",
            "head -n 8 sales.csv",
            "tail -n 5 report.md",
            "wc -l sales.csv",
            "ls -la",
            "pwd",
            "stat report.md",
            "file report.md",
        ] {
            assert!(
                is_plain_read_command(command),
                "expected read-only: {command}"
            );
        }
    }

    #[test]
    fn shell_syntax_keeps_the_effect_unknown() {
        for command in [
            "cat a.txt | grep x",
            "cat a.txt > b.txt",
            "cat a.txt; rm b.txt",
            "cat $HOME/secret",
            "cat `whoami`",
            "cat $(ls)",
            "cat a.txt && rm -rf out",
            "cat (x)",
            "echo hi",
            "python3 -c 'print(1)'",
            "node -e 'process.exit(0)'",
            "sed -i 's/a/b/' f.txt",
        ] {
            assert!(
                !is_plain_read_command(command),
                "must stay unknown: {command}"
            );
        }
    }

    #[test]
    fn options_that_can_write_are_not_treated_as_reads() {
        // Long options may map to an extensible variant; BSD `file -C` writes a
        // compiled magic database.
        assert!(!is_plain_read_command("wc --files0-from=list.txt"));
        assert!(!is_plain_read_command("file -C -m magic"));
        assert!(!is_plain_read_command("cat {file}"));
    }

    #[test]
    fn empty_command_is_not_a_read() {
        assert!(!is_plain_read_command(""));
        assert!(!is_plain_read_command("   "));
    }
}
