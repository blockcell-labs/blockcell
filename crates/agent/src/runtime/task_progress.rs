//! Turn-scoped completion evidence. No natural-language requirements are inferred here.
use blockcell_core::task_contract::{TaskContract, VerificationPolicy};
use blockcell_tools::evidence::{
    CommandOperation, ExecutionAnnotations, VerificationIntent, WorkspaceEffect,
};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub(crate) struct ToolEvidence {
    pub operation: CommandOperation,
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
            operation: CommandOperation::None,
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
        let operation = tool.command_operation(args);
        // Keep task requirements, but never turn an unchecked declaration into
        // successful verification evidence. Poll/input arguments are not launches.
        annotations.verification =
            if operation == CommandOperation::Launch && tool.supports_verification() {
                blockcell_tools::verification::command_intent(args)
            } else {
                None
            };
        Self {
            operation,
            annotations,
            effect: tool.workspace_effect(args),
            command: (operation == CommandOperation::Launch)
                .then(|| {
                    args.get("command")
                        .or_else(|| args.get("cmd"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .flatten(),
            process,
            running: result["running"] == true || result["status"] == "running",
            completed: blockcell_tools::evidence::completed_successfully(result),
        }
    }
}

pub(crate) struct TaskProgress {
    pub contract: TaskContract,
    revision: usize,
    delivery_revision: usize,
    verification_failed: bool,
    changed: bool,
    uncertain_effects: bool,
    verified_revision: Option<usize>,
    pending: HashMap<String, (VerificationIntent, String, usize)>,
    pub last_verification: Option<(VerificationIntent, String)>,
    initial_outputs: HashMap<String, Option<super::FileVersion>>,
    /// 本轮开始时间。`must_update` 的语义是「本轮更新」，所以基线必须是轮次起点，
    /// 而不是契约注册的时刻——模型常常先写文件、再声明契约，用注册时刻做基线会让
    /// 已经写好的产物永远判为「未更新」，逼它重写一遍同内容。
    turn_started_at: std::time::SystemTime,
}

impl Default for TaskProgress {
    fn default() -> Self {
        Self {
            contract: TaskContract::default(),
            revision: 0,
            delivery_revision: 0,
            verification_failed: false,
            changed: false,
            uncertain_effects: false,
            verified_revision: None,
            pending: HashMap::new(),
            last_verification: None,
            initial_outputs: HashMap::new(),
            turn_started_at: std::time::SystemTime::now(),
        }
    }
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
        let Ok(Some(after)) = super::tool_pipeline::file_version(&path) else {
            return false;
        };
        // 本轮开始后被写过的文件本身就是「本轮更新」。这条判据覆盖了「先写文件、
        // 后声明契约」的常见顺序：此时 initial_outputs 抓到的已经是写后的版本，
        // 再拿它比对永远相等，会误报「要求本轮更新但仍是原版本」。
        if after.modified >= self.turn_started_at {
            return true;
        }
        // 轮次开始前就存在、本轮未被写过的文件：与注册时的快照比较（此时两者必然相同）。
        self.initial_outputs
            .get(output)
            .is_some_and(|before| before.as_ref() != Some(&after))
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
            if evidence.effect == WorkspaceEffect::Changed
                || evidence.annotations.verification.is_none()
            {
                self.delivery_revision = self.delivery_revision.saturating_add(1);
                self.verification_failed = false;
            }
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
        if matches!(
            evidence.operation,
            CommandOperation::Input | CommandOperation::Close
        ) {
            if let Some(process) = &evidence.process {
                self.pending.remove(process);
            }
            // Input can change command behavior; closing never proves success.
            return;
        }
        if evidence.running {
            if let Some(process) = &evidence.process {
                if evidence.operation == CommandOperation::Launch {
                    self.pending.remove(process);
                    if succeeded {
                        if let Some((intent, command)) = verification {
                            self.pending
                                .insert(process.clone(), (intent, command, self.revision));
                        }
                    }
                }
            }
            return;
        }
        // 轮询只能兑现"当初真正执行过"的那条命令：参数里临时声明的验证不算数，
        // 否则一次 poll 就能凭自己带的一段文本把自己标成"验证过"。
        let verification = match evidence.operation {
            CommandOperation::Launch => {
                if let Some(process) = &evidence.process {
                    self.pending.remove(process);
                }
                verification.map(|(intent, command)| (intent, command, self.revision))
            }
            CommandOperation::Poll => evidence
                .process
                .as_ref()
                .and_then(|process| self.pending.remove(process)),
            _ => None,
        };
        if verification.is_some() && !evidence.running && !succeeded {
            self.verification_failed = true;
        }
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

    pub fn revision(&self) -> usize {
        self.delivery_revision
    }

    pub fn verification_failed(&self) -> bool {
        self.verification_failed
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

    #[test]
    fn deliverable_written_before_the_contract_is_declared_counts_as_updated() {
        // t06 的真实顺序：模型先写出 todos.md，过几步才在别的工具调用上声明契约。
        // 基线若取注册时刻，这里会永远判「未更新」，逼模型重写一遍同内容。
        let workspace = tempfile::tempdir().unwrap();
        let mut progress = TaskProgress::default();
        std::fs::write(workspace.path().join("todos.md"), "- item\n").unwrap();

        let contract: TaskContract = serde_json::from_value(json!({"verification":"allowed",
            "deliverables":[{"path":"todos.md","must_update":true,"required":true}]}))
        .unwrap();
        progress.register_contract(&contract, workspace.path());

        assert_eq!(
            progress.first_incomplete_output(workspace.path()),
            None,
            "本轮写出的产物不应被判为未更新"
        );
        assert!(
            !progress
                .contract_for_continuation(workspace.path())
                .deliverables[0]
                .must_update
        );
    }
}
