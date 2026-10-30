//! Turn-scoped completion evidence. No natural-language requirements are inferred here.
use blockcell_core::task_contract::{TaskContract, VerificationPolicy};
use blockcell_tools::evidence::{ExecutionAnnotations, VerificationIntent, WorkspaceEffect};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub(crate) struct ToolEvidence {
    pub annotations: ExecutionAnnotations,
    pub effect: WorkspaceEffect,
    pub command: Option<String>,
    pub process: Option<String>,
    pub running: bool,
    pub completed: bool,
}

impl Default for ToolEvidence {
    fn default() -> Self {
        Self {
            annotations: ExecutionAnnotations::default(),
            effect: WorkspaceEffect::None,
            command: None,
            process: None,
            running: false,
            completed: false,
        }
    }
}

impl ToolEvidence {
    pub fn from_execution(tool: &dyn blockcell_tools::Tool, args: &Value, result: &Value) -> Self {
        let process = ["job_id", "session_id"].into_iter().find_map(|field| {
            result
                .get(field)
                .and_then(Value::as_str)
                .or_else(|| args.get(field).and_then(Value::as_str))
                .map(|id| format!("{field}:{id}"))
        });
        let mut annotations = if tool.accepts_execution_annotations() {
            ExecutionAnnotations::parse(args).unwrap_or_default()
        } else {
            ExecutionAnnotations::default()
        };
        if tool.supports_verification() && annotations.verification.is_none() {
            annotations.verification = blockcell_tools::verification::command_intent(args);
        }
        Self {
            annotations,
            effect: tool.workspace_effect(args),
            command: args
                .get("command")
                .or_else(|| args.get("cmd"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            process,
            running: result["running"] == true || result["status"] == "running",
            completed: blockcell_tools::evidence::completed_successfully(result),
        }
    }
}

#[derive(Default)]
pub(crate) struct TaskProgress {
    pub contract: TaskContract,
    revision: usize,
    changed: bool,
    uncertain_effects: bool,
    verified_revision: Option<usize>,
    pending: HashMap<String, (VerificationIntent, String, usize)>,
    pub last_verification: Option<(VerificationIntent, String)>,
    initial_outputs: HashMap<String, Option<super::FileVersion>>,
}

impl TaskProgress {
    pub fn register_contract(&mut self, contract: &TaskContract, workspace: &std::path::Path) {
        // An omitted/default field must not silently erase requirements
        // already recorded earlier in this turn.
        self.contract.verification = match (self.contract.verification, contract.verification) {
            (VerificationPolicy::Forbidden, _) | (_, VerificationPolicy::Forbidden) => {
                VerificationPolicy::Forbidden
            }
            (VerificationPolicy::Deferred, _) | (_, VerificationPolicy::Deferred) => {
                VerificationPolicy::Deferred
            }
            (VerificationPolicy::Required, _) | (_, VerificationPolicy::Required) => {
                VerificationPolicy::Required
            }
            _ => VerificationPolicy::Allowed,
        };
        for item in &contract.deliverables {
            if let Some(existing) = self
                .contract
                .deliverables
                .iter_mut()
                .find(|existing| existing.path == item.path)
            {
                existing.required |= item.required;
                existing.must_update |= item.must_update;
            } else if self.contract.deliverables.len() < 32 {
                self.contract.deliverables.push(item.clone());
            }
        }

        for item in &self.contract.deliverables {
            if item.must_update && !self.initial_outputs.contains_key(&item.path) {
                if let Some(path) =
                    blockcell_core::task_contract::resolve_workspace_output(workspace, &item.path)
                {
                    if let Ok(version) = super::tool_pipeline::file_version(&path) {
                        self.initial_outputs.insert(item.path.clone(), version);
                    }
                }
            }
        }
    }

    fn output_was_updated(&self, output: &str, workspace: &std::path::Path) -> bool {
        let Some(path) = blockcell_core::task_contract::resolve_workspace_output(workspace, output)
        else {
            return false;
        };
        self.initial_outputs.get(output).is_some_and(|before| {
            super::tool_pipeline::file_version(&path)
                .ok()
                .is_some_and(|after| after.is_some() && after != *before)
        })
    }

    pub fn contract_for_continuation(&self, workspace: &std::path::Path) -> TaskContract {
        let mut contract = self.contract.clone();
        for output in &mut contract.deliverables {
            if output.must_update && self.output_was_updated(&output.path, workspace) {
                output.must_update = false;
            }
        }
        contract
    }

    pub fn first_incomplete_output(&self, workspace: &std::path::Path) -> Option<String> {
        if let Some(missing) = self.contract.first_missing_output(workspace) {
            return Some(missing);
        }
        self.contract
            .deliverables
            .iter()
            .filter(|item| item.required && item.must_update)
            .find_map(|item| {
                blockcell_core::task_contract::resolve_workspace_output(workspace, &item.path)?;
                let updated = self.output_was_updated(&item.path, workspace);
                (!updated).then(|| item.path.clone())
            })
    }

    pub fn observe(&mut self, evidence: &ToolEvidence, succeeded: bool) {
        // Failed commands can have partially modified files too. Unknown effects
        // invalidate existing evidence but do not claim a mutation occurred.
        if evidence.effect != WorkspaceEffect::None {
            self.revision = self.revision.saturating_add(1);
            self.last_verification = None;
            if evidence.effect == WorkspaceEffect::Changed {
                self.changed = true;
            }
            if evidence.effect == WorkspaceEffect::Unknown {
                self.uncertain_effects = true;
            }
        }
        let verification = evidence
            .annotations
            .verification
            .clone()
            .zip(evidence.command.clone());
        if evidence.running {
            if let Some(process) = &evidence.process {
                if let Some((intent, command)) = verification {
                    self.pending
                        .insert(process.clone(), (intent, command, self.revision));
                } else if evidence.command.is_some() {
                    self.pending.remove(process);
                }
            }
            return;
        }
        // 轮询只能兑现"当初真正执行过"的那条命令：参数里临时声明的验证不算数，
        // 否则一次 poll 就能凭自己带的一段文本把自己标成"验证过"。
        let verification = match &evidence.process {
            Some(process) => self.pending.remove(process),
            None => verification.map(|(intent, command)| (intent, command, self.revision)),
        };
        if succeeded && evidence.completed && self.contract.verification.permits_execution() {
            if let Some((intent, command, revision)) =
                verification.filter(|(_, _, revision)| *revision == self.revision)
            {
                self.verified_revision = Some(revision);
                self.last_verification = Some((intent, command));
            }
        }
    }

    pub fn needs_verification(&self) -> bool {
        self.contract.verification.permits_execution()
            && (self.changed
                || self.uncertain_effects
                || self.contract.verification == VerificationPolicy::Required)
            && self.verified_revision != Some(self.revision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_tools::{exec_jobs::JobOutputTool, shell_actions::ShellActionTool};
    use serde_json::json;

    #[test]
    fn polling_cannot_invent_a_command_that_was_never_executed() {
        let evidence = ToolEvidence::from_execution(
            &JobOutputTool,
            &json!({"job_id":"unrelated", "command":"cargo test",
                "verification":{"kind":"test","target":"workspace"}}),
            &json!({"job_id":"unrelated","running":false,"exit_code":0}),
        );
        let mut progress = TaskProgress::default();
        progress.observe(&evidence, true);
        assert!(progress.last_verification.is_none());
    }

    #[test]
    fn background_verification_is_tied_to_the_launched_command_and_revision() {
        let run = ToolEvidence::from_execution(
            &ShellActionTool::run(),
            &json!({"command":"cargo test"}),
            &json!({"session_id":"check","running":true}),
        );
        let poll = ToolEvidence::from_execution(
            &ShellActionTool::poll(),
            &json!({"session_id":"check"}),
            &json!({"session_id":"check","running":false,"exit_code":0}),
        );
        let mut progress = TaskProgress::default();
        progress.observe(&run, true);
        assert!(progress.needs_verification());
        progress.observe(&poll, true);
        assert!(!progress.needs_verification());
        assert_eq!(
            progress
                .last_verification
                .as_ref()
                .map(|(_, command)| command.as_str()),
            Some("cargo test")
        );

        let mut stale = TaskProgress::default();
        stale.observe(&run, true);
        stale.observe(
            &ToolEvidence {
                effect: WorkspaceEffect::Changed,
                ..ToolEvidence::default()
            },
            true,
        );
        stale.observe(&poll, true);
        assert!(stale.needs_verification());
        assert!(stale.last_verification.is_none());
    }

    #[test]
    fn task_constraints_and_output_baselines_survive_partial_updates() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("report.json"), "old").unwrap();
        let mut progress = TaskProgress::default();
        let contract: TaskContract = serde_json::from_value(json!({"verification":"forbidden",
            "deliverables":[{"path":"report.json","must_update":true}]}))
        .unwrap();
        progress.register_contract(&contract, workspace.path());
        progress.register_contract(&TaskContract::default(), workspace.path());
        progress.observe(
            &ToolEvidence {
                effect: WorkspaceEffect::Changed,
                ..ToolEvidence::default()
            },
            true,
        );
        assert!(!progress.needs_verification());
        assert_eq!(
            progress.first_incomplete_output(workspace.path()),
            Some("report.json".into())
        );
        std::fs::write(workspace.path().join("report.json"), "new content").unwrap();
        assert_eq!(progress.first_incomplete_output(workspace.path()), None);
        assert!(
            !progress
                .contract_for_continuation(workspace.path())
                .deliverables[0]
                .must_update
        );
    }
}
