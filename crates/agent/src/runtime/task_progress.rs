//! Turn-scoped completion evidence. No natural-language requirements are inferred here.
use blockcell_core::task_contract::{TaskContract, VerificationPolicy};
use blockcell_tools::evidence::{
    CommandOperation, ExecutionAnnotations, VerificationIntent, VerificationKind, WorkspaceEffect,
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
    pub failed: bool,
    pub rejection: Option<&'static str>,
    pub check_candidate: bool,
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
            failed: false,
            rejection: None,
            check_candidate: false,
        }
    }
}

impl ToolEvidence {
    #[cfg(test)]
    pub fn from_execution(tool: &dyn blockcell_tools::Tool, args: &Value, result: &Value) -> Self {
        Self::from_execution_with_policy(tool, args, result, &Default::default())
    }

    pub fn from_execution_with_policy(
        tool: &dyn blockcell_tools::Tool,
        args: &Value,
        result: &Value,
        policy: &blockcell_core::config::VerificationConfig,
    ) -> Self {
        let intent = blockcell_tools::verification::command_intent_with_policy(
            args,
            policy,
            result["pipefail"] == true,
        );
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
        let check_candidate = args.get("verification").is_some() || intent.is_some();
        let rejection = (intent.is_none()
            && operation == CommandOperation::Launch
            && tool.supports_verification())
        .then(|| blockcell_tools::verification::command_rejection_reason(args))
        .flatten();
        // Keep task requirements, but never turn an unchecked declaration into
        // successful verification evidence. Poll/input arguments are not launches.
        annotations.verification =
            if operation == CommandOperation::Launch && tool.supports_verification() {
                intent
            } else {
                None
            };
        Self {
            operation,
            annotations,
            effect: if matches!(tool.schema().name.as_str(), "write_file" | "edit_file")
                && docs_only_paths(args, policy)
            {
                WorkspaceEffect::None
            } else {
                tool.workspace_effect(args)
            },
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
            // A tool can successfully return a failed process result as JSON.
            // Transport-level success must not hide a failed verification.
            failed: result["exit_code"].as_i64().is_some_and(|code| code != 0)
                || result["timed_out"] == true,
            rejection,
            check_candidate,
        }
    }
}

fn docs_only_paths(args: &Value, policy: &blockcell_core::config::VerificationConfig) -> bool {
    let paths: Vec<_> = if let Some(files) = args.get("files").and_then(Value::as_array) {
        files
            .iter()
            .map(|file| file.get("path").and_then(Value::as_str))
            .collect()
    } else {
        vec![args.get("path").and_then(Value::as_str)]
    };
    !paths.is_empty()
        && paths.iter().all(|path| {
            path.is_some_and(|path| {
                std::path::Path::new(path)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        policy
                            .ignore_extensions
                            .iter()
                            .any(|ignored| ignored.eq_ignore_ascii_case(ext))
                    })
            })
        })
}

pub(crate) struct TaskProgress {
    pub verification_policy: blockcell_core::config::VerificationConfig,
    pub contract: TaskContract,
    revision: usize,
    delivery_revision: usize,
    verification_failed: bool,
    changed: bool,
    uncertain_effects: bool,
    verified_revision: Option<usize>,
    pending: HashMap<String, (VerificationIntent, String, usize)>,
    pub last_verification: Option<(VerificationIntent, String)>,
    last_rejection: Option<(&'static str, bool)>,
    test_required: bool,
    successful_check: Option<(String, usize)>,
    pub duplicate_check: bool,
    initial_outputs: HashMap<String, Option<super::FileVersion>>,
    /// 本轮开始时间。`must_update` 的语义是「本轮更新」，所以基线必须是轮次起点，
    /// 而不是契约注册的时刻——模型常常先写文件、再声明契约，用注册时刻做基线会让
    /// 已经写好的产物永远判为「未更新」，逼它重写一遍同内容。
    turn_started_at: std::time::SystemTime,
}

impl Default for TaskProgress {
    fn default() -> Self {
        Self {
            verification_policy: Default::default(),
            contract: TaskContract::default(),
            revision: 0,
            delivery_revision: 0,
            verification_failed: false,
            changed: false,
            uncertain_effects: false,
            verified_revision: None,
            pending: HashMap::new(),
            last_verification: None,
            last_rejection: None,
            test_required: false,
            successful_check: None,
            duplicate_check: false,
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
        self.duplicate_check = succeeded
            && evidence.completed
            && evidence.check_candidate
            && evidence.command.as_ref().is_some_and(|command| {
                self.successful_check
                    .as_ref()
                    .is_some_and(|(previous, revision)| {
                        previous == command && *revision == self.revision
                    })
            });
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
            self.last_rejection = None;
            if evidence.effect == WorkspaceEffect::Changed {
                self.changed = true;
            }
            if evidence.effect == WorkspaceEffect::Unknown {
                self.uncertain_effects = true;
            }
        }
        if evidence.operation == CommandOperation::Launch {
            self.last_rejection = evidence
                .rejection
                .map(|reason| (reason, succeeded && evidence.completed));
            if evidence
                .annotations
                .verification
                .as_ref()
                .is_some_and(|intent| intent.kind == VerificationKind::Test)
            {
                self.test_required = true;
            }
            if succeeded && evidence.completed && evidence.check_candidate {
                if let Some(command) = &evidence.command {
                    self.successful_check = Some((command.clone(), self.revision));
                }
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
        if verification.is_some() && !evidence.running && (!succeeded || evidence.failed) {
            self.verification_failed = true;
        }
        if succeeded && evidence.completed && self.contract.verification.permits_execution() {
            if let Some((intent, command, revision)) =
                verification.filter(|(_, _, revision)| *revision == self.revision)
            {
                self.verified_revision = Some(revision);
                self.last_verification = Some((intent, command));
                self.verification_failed = false;
            }
        }
    }

    pub fn needs_verification(&self) -> bool {
        self.verification_policy.mode != blockcell_core::config::VerificationMode::Off
            && self.contract.verification.permits_execution()
            && (self.changed
                || self.uncertain_effects
                || self.contract.verification == VerificationPolicy::Required)
            && (self.verified_revision != Some(self.revision)
                || (self.test_required
                    && !self
                        .last_verification
                        .as_ref()
                        .is_some_and(|(intent, _)| intent.kind == VerificationKind::Test)))
    }

    pub fn verification_reason(&self) -> &'static str {
        if !self.contract.verification.permits_execution() {
            return "verification prohibited or deferred by task policy";
        }
        if !self.needs_verification() && self.last_verification.is_some() {
            return if self
                .last_verification
                .as_ref()
                .is_some_and(|(intent, _)| intent.kind == VerificationKind::Syntax)
            {
                "recognized syntax check succeeded; semantic correctness is not established"
            } else {
                "recognized check succeeded for the current revision"
            };
        }
        if self.test_required
            && self
                .last_verification
                .as_ref()
                .is_some_and(|(intent, _)| intent.kind != VerificationKind::Test)
        {
            return "only non-test evidence is current; syntax/build/lint checks do not replace an attempted semantic test";
        }
        if self.verification_failed {
            return "the recognized verification command failed";
        }
        if let Some((reason, _)) = self.last_rejection {
            return reason;
        }
        if !self.pending.is_empty() {
            return "verification is still running or its completion has not been observed";
        }
        "no completed recognized check for the current workspace revision"
    }

    pub fn verification_hint(&self) -> String {
        let repeat = if self.last_rejection.is_some_and(|(_, completed)| completed) {
            "The unsupported command already completed successfully at this revision. Do not repeat it: repetition cannot make it evidence."
        } else {
            "For a wrapped check, run the recognized runner directly once without wrappers."
        };
        format!("Reason: {}. {} Use a relevant recognized runner (pytest/unittest for semantic tests) or configured commands: {:?}. Piped output to numeric tail/head filters is fine only with executor-confirmed pipefail. Otherwise report the custom check's scope as unconfirmed. Syntax evidence proves parsing only, not semantic correctness.", self.verification_reason(), repeat, self.verification_policy.commands)
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
    fn documentation_edits_do_not_require_checks_but_code_does() {
        let policy = blockcell_core::config::VerificationConfig::default();
        for args in [
            serde_json::json!({"path":"README.md"}),
            serde_json::json!({"files":[{"path":"a.txt"},{"path":"b.rst"}]}),
        ] {
            let evidence = ToolEvidence::from_execution_with_policy(
                &blockcell_tools::fs::WriteFileTool::default(),
                &args,
                &serde_json::json!({}),
                &policy,
            );
            let mut progress = TaskProgress::default();
            progress.observe(&evidence, true);
            assert!(!progress.needs_verification());
        }
        for args in [
            serde_json::json!({"path":"main.rs"}),
            serde_json::json!({"files":[{"path":"a.md"},{"path":"b.py"}]}),
        ] {
            let evidence = ToolEvidence::from_execution_with_policy(
                &blockcell_tools::fs::WriteFileTool::default(),
                &args,
                &serde_json::json!({}),
                &policy,
            );
            let mut progress = TaskProgress::default();
            progress.observe(&evidence, true);
            assert!(progress.needs_verification());
        }
        assert!(!docs_only_paths(&serde_json::json!({"files":[]}), &policy));
        assert!(!docs_only_paths(&serde_json::json!({}), &policy));
    }

    #[test]
    fn piped_checks_require_executor_flag_and_success() {
        for (flag, exit, expected) in [(true, 0, false), (false, 0, true), (true, 1, true)] {
            let evidence = ToolEvidence::from_execution(
                &ShellActionTool::run(),
                &serde_json::json!({"command":"pytest | tail -n 5"}),
                &serde_json::json!({"exit_code":exit, "pipefail":flag, "stdout":"PASS"}),
            );
            let mut progress = TaskProgress::default();
            progress.observe(&evidence, exit == 0);
            assert_eq!(progress.needs_verification(), expected);
        }
    }
    fn check(command: &str, exit_code: i32) -> ToolEvidence {
        ToolEvidence::from_execution(
            &ShellActionTool::run(),
            &json!({"command":command}),
            &json!({"exit_code":exit_code,"running":false,"output":"PASS"}),
        )
    }

    #[test]
    fn syntax_cannot_discharge_a_semantic_test_and_failure_does_not_trust_pass_text() {
        let mut progress = TaskProgress::default();
        progress.observe(&check("pytest", 1), false);
        progress.observe(&check("python3 -m json.tool report.json", 0), true);
        assert!(progress.needs_verification());
        assert!(progress.verification_reason().contains("do not replace"));
        progress.observe(&check("pytest", 0), true);
        assert!(!progress.needs_verification());
        let mut syntax = TaskProgress::default();
        syntax.observe(&check("python3 -m json.tool report.json", 0), true);
        assert!(!syntax.needs_verification());
        syntax.observe(&check("python3 -m json.tool report.json", 1), false);
        assert!(syntax.needs_verification());
        assert!(syntax.verification_reason().contains("failed"));
    }

    #[test]
    fn dedicated_reads_preserve_evidence_even_when_a_read_fails_but_shell_stays_unknown() {
        use blockcell_tools::{
            code_search::{GlobTool, GrepTool},
            fs::{ListDirTool, ReadFileTool},
            Tool,
        };
        let mut progress = TaskProgress::default();
        progress.observe(&check("pytest", 0), true);
        for tool in [
            &ReadFileTool::default() as &dyn Tool,
            &ListDirTool::default(),
            &GrepTool,
            &GlobTool,
        ] {
            let read = ToolEvidence::from_execution(tool, &json!({"path":"."}), &json!({}));
            assert_eq!(read.effect, WorkspaceEffect::None);
            progress.observe(&read, false);
            assert!(!progress.needs_verification());
        }
        progress.observe(&check("cat report.json", 0), true);
        assert!(progress.needs_verification());
    }

    #[test]
    fn rejected_checks_explain_reason_without_requesting_identical_successful_reruns() {
        for (command, reason) in [
            ("python3 custom.py", "unsupported runner"),
            ("pytest | tail", "pipes"),
        ] {
            let mut progress = TaskProgress::default();
            progress.observe(&check(command, 0), true);
            assert!(progress.needs_verification());
            let hint = progress.verification_hint();
            assert!(hint.contains(reason) && hint.contains("Do not repeat"));
            assert!(hint.contains("pytest/unittest") && hint.contains("scope"));
            progress.observe(
                &ToolEvidence {
                    effect: WorkspaceEffect::Changed,
                    ..ToolEvidence::default()
                },
                true,
            );
            assert!(!progress.verification_hint().contains("already completed"));
        }
        let mut progress = TaskProgress::default();
        progress.observe(&check("pytest | tail", 1), false);
        assert!(progress.verification_hint().contains("directly once"));
    }

    #[test]
    fn identical_successful_checks_only_get_a_soft_signal_until_workspace_changes() {
        let mut progress = TaskProgress::default();
        progress.observe(&check("pytest", 0), true);
        assert!(!progress.duplicate_check);
        progress.observe(&ToolEvidence::default(), true);
        progress.observe(&check("pytest", 0), true);
        assert!(progress.duplicate_check);
        assert!(!progress.needs_verification());
        progress.observe(
            &ToolEvidence {
                effect: WorkspaceEffect::Changed,
                ..ToolEvidence::default()
            },
            true,
        );
        progress.observe(&check("pytest", 0), true);
        assert!(!progress.duplicate_check);
        progress.observe(&check("pytest", 1), false);
        assert!(!progress.duplicate_check);
        assert!(progress.needs_verification());
    }

    #[test]
    fn piped_checks_require_one_direct_status_preserving_check() {
        for command in ["cargo test 2>&1 | tail -n 20", "npm test | head -20"] {
            let evidence = ToolEvidence::from_execution(
                &ShellActionTool::run(),
                &json!({"command":command,"verification":{"kind":"test","target":"workspace"}}),
                &json!({"exit_code":0,"running":false,"output":"PASS"}),
            );
            let mut progress = TaskProgress::default();
            progress.observe(&evidence, true);
            assert!(progress.needs_verification());
            assert!(progress.last_verification.is_none());
            let direct = ToolEvidence::from_execution(
                &ShellActionTool::run(),
                &json!({"command":"cargo test"}),
                &json!({"exit_code":0,"running":false}),
            );
            progress.observe(&direct, true);
            assert!(!progress.needs_verification());
            let failed = ToolEvidence::from_execution(
                &ShellActionTool::run(),
                &json!({"command":"cargo test"}),
                &json!({"exit_code":1,"running":false,"output":"PASS"}),
            );
            progress.observe(&failed, false);
            assert!(progress.needs_verification());
            assert!(progress.last_verification.is_none());
        }
    }

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
        std::fs::File::options()
            .write(true)
            .open(workspace.path().join("todos.md"))
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(progress.turn_started_at + std::time::Duration::from_secs(1)),
            )
            .unwrap();

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
