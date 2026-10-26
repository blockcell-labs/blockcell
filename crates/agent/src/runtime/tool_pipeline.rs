//! Guarded Tool Pipeline：把安全与观测逻辑从具体工具中集中出来。
//!
//! ```text
//! ToolCall
//!  → admission: Toggle → tool/pre-execute（listener 可改写参数或拒绝）
//!               → Policy → DangerousOperation → PathAccess → FileObservation → ExplicitConfirmation
//!  → tool/call（任一阶段拒绝则直接记录 outcome=rejected 的 tool/result）
//!  → execute（含 shell sandbox escalation 审批与重试）
//!  → tool/post-execute
//!  → classify outcome（自进化上报 / 学习提示）
//!  → tool/result + audit + 学习计数 + Layer4 追踪
//! ```
//!
//! 具体工具只需实现业务 `execute`，不再自行处理 permission / approval / audit。
//! 所有安全检查都作用于 listener 改写之后的最终参数。

use super::tool_exec::{
    full_access_retry_args, shell_action_is_run, shell_sandbox_escalation_reason,
};
use super::*;
use blockcell_core::config::ApprovalPolicy;
use blockcell_core::path_policy::{resolve_for_policy, PathOp, PolicyAction};
use blockcell_tools::tool_guard::requires_human_confirmation;

fn file_version(path: &Path) -> std::io::Result<Option<FileVersion>> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some(FileVersion {
            len: metadata.len(),
            modified: metadata.modified()?,
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn canonicalize_shell_tool_call(call: &ToolCallRequest) -> ToolCallRequest {
    let Some(args) =
        blockcell_tools::shell_actions::canonical_shell_args(&call.name, &call.arguments)
    else {
        return call.clone();
    };
    let mut canonical = call.clone();
    canonical.name = "shell".to_string();
    canonical.arguments = args;
    canonical
}

#[cfg(test)]
mod shell_wrapper_tests {
    use super::canonicalize_shell_tool_call;
    use blockcell_core::types::ToolCallRequest;
    use serde_json::json;

    #[test]
    fn canonicalization_keeps_call_id_for_original_wrapper_result() {
        let original = ToolCallRequest {
            id: "call-123".to_string(),
            name: "shell_run".to_string(),
            arguments: json!({"command":"echo hi"}),
            thought_signature: None,
        };
        let canonical = canonicalize_shell_tool_call(&original);
        assert_eq!(canonical.id, original.id);
        assert_eq!(canonical.name, "shell");
        assert_eq!(canonical.arguments["action"], "run");
        assert_eq!(original.name, "shell_run");
    }
}

/// 单次工具调用在管线中流动的状态。
pub(crate) struct ToolInvocation<'a> {
    pub(crate) call: &'a ToolCallRequest,
    pub(crate) msg: &'a InboundMessage,
    /// 经 listener 改写后、被所有 guard 检查并最终交给工具的参数。
    pub(crate) args: serde_json::Value,
    /// Policy 阶段已获得用户显式确认，后续审批阶段不再重复询问。
    pub(crate) policy_confirmed: bool,
    /// DangerousOperation 阶段已获得用户批准。与 `policy_confirmed` 的区别：
    /// 危险命令批准只证明"用户同意这条命令"，作用域限于本次调用——可以跳过
    /// 后续确认弹窗，但不能把 Confirm 级路径写入会话级授权缓存。
    pub(crate) dangerous_confirmed: bool,
}

/// 工具实际执行后的返回内容。
pub(crate) struct ToolExecution {
    pub(crate) result_str: String,
    pub(crate) result_json: serde_json::Value,
    pub(crate) duration_ms: u64,
    pub(crate) outcome: ToolOutcome,
    pub(crate) idempotency: Idempotency,
}

impl ToolExecution {
    pub(crate) fn is_error(&self) -> bool {
        self.outcome.is_error()
    }
}

/// 准入阶段拒绝时直接作为工具结果返回给模型的字符串。
type GuardResult = std::result::Result<(), String>;

#[derive(Debug, Clone, Copy)]
enum AdmissionStage {
    Toggle,
    PreExecute,
    Policy,
    DangerousOperation,
    PathAccess,
    FileObservation,
    /// 工具声明 [`blockcell_tools::Tool::requires_confirmation`] 时要求用户显式确认（P2-3）。
    /// 判定依据是"是否需要确认"这一声明，而非执行环境，也不是硬编码的工具名。
    ExplicitConfirmation,
}

/// pre-execute 必须先于所有安全检查，确保检查的是 listener 改写后的最终参数。
const ADMISSION_STAGES: [AdmissionStage; 7] = [
    AdmissionStage::Toggle,
    AdmissionStage::PreExecute,
    AdmissionStage::Policy,
    AdmissionStage::DangerousOperation,
    AdmissionStage::PathAccess,
    AdmissionStage::FileObservation,
    AdmissionStage::ExplicitConfirmation,
];

fn workspace_auto_command(tool_name: &str, args: &serde_json::Value) -> Option<String> {
    let field = |key: &str| args.get(key).and_then(|value| value.as_str()).unwrap_or("");
    match tool_name {
        "exec" => args
            .get("command")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        "shell" => match args
            .get("action")
            .and_then(|value| value.as_str())
            .unwrap_or("run")
        {
            "run" => args
                .get("command")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            "write_stdin" => Some(blockcell_tools::shell_session::preview_session_stdin(
                field("session_id"),
                field("input"),
            )),
            _ => None,
        },
        _ => None,
    }
}

// Registered memory-writing tools. `auto_memory` is a background subsystem,
// not a tool in the registry, so it must not appear here (a dead entry makes
// future readers believe such a tool exists).
const MEMORY_WRITE_TOOLS: [&str; 3] = ["memory_manage", "memory_upsert", "memory_forget"];

/// 成功的工具调用对应的领域事件：记忆写入、后台子代理启动。
fn semantic_tool_event(call: &ToolCallRequest, result: &serde_json::Value) -> Option<AgentEvent> {
    let tool_name = call.name.clone();
    let call_id = call.id.clone();
    if MEMORY_WRITE_TOOLS.contains(&call.name.as_str()) {
        return Some(AgentEvent::MemoryWrite { tool_name, call_id });
    }
    if matches!(call.name.as_str(), "spawn" | "agent") {
        let task_id = result.get("task_id")?.as_str()?.to_string();
        return Some(AgentEvent::SubagentSpawn {
            task_id,
            tool_name,
            call_id,
        });
    }
    None
}

/// 工具结论对技能进化的意义：`Some(is_error)` 表示计入观察窗口，`None` 表示不计入。
///
/// 只有"工具已执行并明确失败"才算技能缺陷；取消、超时、结果未知、执行前被拒绝或未开始
/// 都不是技能本身的问题，既不触发进化，也不污染观察窗口的错误率。
pub(super) fn evolution_signal(outcome: ToolOutcome) -> Option<bool> {
    match outcome {
        ToolOutcome::Succeeded => Some(false),
        ToolOutcome::Failed => Some(true),
        ToolOutcome::Rejected
        | ToolOutcome::Cancelled
        | ToolOutcome::TimedOut
        | ToolOutcome::NotStarted
        | ToolOutcome::Unknown => None,
    }
}

/// Unknown ≠ Failed：调用可能已经生效，必须先对账再决定是否重试。
fn unknown_outcome_hint(tool_name: &str) -> String {
    format!(
        "[系统] 工具 `{tool_name}` 的执行结果未知（超时或中断，操作可能已经生效）。\
        不要直接重试；先查询目标状态确认是否已生效，无法确认时请用户决定是否重试。"
    )
}

impl AgentRuntime {
    pub(crate) async fn execute_tool_call(
        &mut self,
        tool_call: &ToolCallRequest,
        msg: &InboundMessage,
        active_skill_dir: Option<PathBuf>,
    ) -> ToolCallOutput {
        self.execute_tool_call_prefetched(tool_call, msg, active_skill_dir, None)
            .await
    }

    /// 与 [`Self::execute_tool_call`] 相同，但可携带只读预取结果；命中时跳过工具体执行，
    /// 仍完整走准入 / 事件 / 审计 / 归类流程。
    pub(crate) async fn execute_tool_call_prefetched(
        &mut self,
        tool_call: &ToolCallRequest,
        msg: &InboundMessage,
        active_skill_dir: Option<PathBuf>,
        prefetched: Option<super::prefetch::PrefetchedToolResult>,
    ) -> ToolCallOutput {
        // 管线 future 较大，放到堆上避免把调用方（主循环）的 future 撑大导致栈溢出。
        Box::pin(self.run_tool_pipeline(tool_call, msg, active_skill_dir, prefetched)).await
    }

    async fn run_tool_pipeline(
        &mut self,
        tool_call: &ToolCallRequest,
        msg: &InboundMessage,
        active_skill_dir: Option<PathBuf>,
        prefetched: Option<super::prefetch::PrefetchedToolResult>,
    ) -> ToolCallOutput {
        let original_name = tool_call.name.clone();
        // 预取阶段已触发过 tool/pre-execute：以（可能被 listener 改写后的）
        // 预取参数为准并跳过重复触发，保证 hook 每次调用只执行一次且
        // 后续准入检查作用于同一组参数。
        let pre_execute_done = prefetched.is_some();
        let mut args = prefetched
            .as_ref()
            .map(|prefetched| prefetched.args.clone())
            .unwrap_or_else(|| tool_call.arguments.clone());
        if let Some(canonical_args) =
            blockcell_tools::shell_actions::canonical_shell_args(&original_name, &args)
        {
            args = canonical_args;
        }
        let mut canonical_call = canonicalize_shell_tool_call(tool_call);
        canonical_call.arguments = args.clone();
        let tool_call = &canonical_call;
        if tool_call.name == "shell" {
            args = blockcell_tools::shell_session::normalize_shell_args(args);
        }
        if tool_call.name == "shell"
            && args
                .get("action")
                .and_then(|value| value.as_str())
                .unwrap_or("run")
                == "run"
        {
            if let Some(object) = args.as_object_mut() {
                if !object.contains_key("sandbox_policy") {
                    if let Some(policy) = &self.config.tools.exec.sandbox_policy {
                        object.insert(
                            "sandbox_policy".to_string(),
                            serde_json::Value::String(policy.clone()),
                        );
                    }
                }
            }
        }
        let mut inv = ToolInvocation {
            call: tool_call,
            msg,
            args,
            policy_confirmed: false,
            dangerous_confirmed: false,
        };
        if let Err(denied) = self.admit_tool_call(&mut inv, pre_execute_done).await {
            self.record_rejected_tool_call(&inv, &denied).await;
            return ToolCallOutput::rejected(denied);
        }
        self.dispatch_event(
            msg,
            &persist_session_key_for(msg),
            &mut AgentEvent::ToolCall {
                tool_name: tool_call.name.clone(),
                call_id: tool_call.id.clone(),
                arguments: inv.args.clone(),
                environment: self
                    .tool_registry
                    .execution_environment(&tool_call.name, &inv.args),
            },
        )
        .await;

        let mut ctx = self.build_tool_context(msg, active_skill_dir);
        if inv.call.name == "exec" {
            if let Some(working_dir) = inv.args.get("working_dir").and_then(|value| value.as_str())
            {
                let root = resolve_for_policy(&self.resolve_path(working_dir));
                if self.path_policy.evaluate(&root, PathOp::Write) != PolicyAction::Deny {
                    if let Some(roots) = ctx.sandbox_writable_roots.as_mut() {
                        if !roots.contains(&root) {
                            roots.push(root);
                        }
                    }
                }
            }
        }
        let outcome = self.execute_guarded_tool(&mut inv, ctx, prefetched).await;

        self.after_tool_execute(&inv, &outcome).await;
        let learning_hint = self.classify_tool_outcome(tool_call, &outcome).await;
        self.record_tool_result(&inv, &outcome).await;

        // 结果未知时提示先对账，避免模型直接重试造成重复副作用；再追加学习提示，让 LLM 自然地回复用户
        let reconcile_hint = (outcome.outcome == ToolOutcome::Unknown)
            .then(|| unknown_outcome_hint(&tool_call.name));
        let text = [reconcile_hint, learning_hint]
            .into_iter()
            .flatten()
            .fold(outcome.result_str, |text, hint| format!("{text}\n\n{hint}"));
        ToolCallOutput {
            text,
            outcome: outcome.outcome,
        }
    }

    async fn admit_tool_call(
        &mut self,
        inv: &mut ToolInvocation<'_>,
        pre_execute_done: bool,
    ) -> GuardResult {
        for stage in ADMISSION_STAGES {
            if pre_execute_done && matches!(stage, AdmissionStage::PreExecute) {
                continue;
            }
            match stage {
                AdmissionStage::Toggle => self.guard_disabled_toggles(inv),
                AdmissionStage::PreExecute => self.dispatch_tool_pre_execute(inv).await,
                AdmissionStage::Policy => self.guard_tool_policy(inv).await,
                AdmissionStage::DangerousOperation => self.guard_dangerous_operation(inv).await,
                AdmissionStage::PathAccess => self.guard_path_access(inv).await,
                AdmissionStage::FileObservation => self.guard_file_observation(inv),
                AdmissionStage::ExplicitConfirmation => self.guard_explicit_confirmation(inv).await,
            }?;
        }
        Ok(())
    }

    /// 被拒绝的调用同样产生 durable 的 tool/result，保证拦截行为可审计、可回放。
    /// 审计记录使用经 listener 改写、实际被 guard 检查的参数。
    async fn record_rejected_tool_call(&mut self, inv: &ToolInvocation<'_>, denied: &str) {
        self.record_unexecuted(inv.call, &inv.args, inv.msg, ToolOutcome::Rejected, denied)
            .await;
    }

    /// 没有执行的调用（被拒绝、未开始）以 tool/result 收尾，保证模型提议的每个调用都有结论。
    /// `result_text` 是返回给模型的内容，JSON 对象原样记录，其余包装为 `{"error": ...}`。
    pub(crate) async fn record_unexecuted_tool_call(
        &mut self,
        call: &ToolCallRequest,
        msg: &InboundMessage,
        outcome: ToolOutcome,
        result_text: &str,
    ) {
        self.record_unexecuted(call, &call.arguments, msg, outcome, result_text)
            .await;
    }

    async fn record_unexecuted(
        &mut self,
        call: &ToolCallRequest,
        args: &serde_json::Value,
        msg: &InboundMessage,
        outcome: ToolOutcome,
        result_text: &str,
    ) {
        let result = serde_json::from_str::<serde_json::Value>(result_text)
            .ok()
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({ "error": result_text }));
        self.audit_tool_call(call, args, result.clone(), msg, outcome, 0)
            .await;
        self.dispatch_event(
            msg,
            &persist_session_key_for(msg),
            &mut AgentEvent::ToolResult {
                tool_name: call.name.clone(),
                call_id: call.id.clone(),
                result,
                duration_ms: 0,
                outcome,
                idempotency: self.tool_registry.idempotency(&call.name, args),
            },
        )
        .await;
    }

    /// 审计记录以回合 id 作为 trace_id，并携带 call_id 与执行结论，与 Session Event Log 对齐。
    async fn audit_tool_call(
        &self,
        call: &ToolCallRequest,
        args: &serde_json::Value,
        result: serde_json::Value,
        msg: &InboundMessage,
        outcome: ToolOutcome,
        duration_ms: u64,
    ) {
        // Audit must use the same session key as the `tool/call` event and the
        // persisted session log; cron delivery resolves to the target session.
        let session_key = persist_session_key_for(msg);
        // 与 `tool/call` 事件同名对齐：审计同样记录本次调用声明的执行环境。
        let environment = self.tool_registry.execution_environment(&call.name, args);
        // 写盘 + 双锁（AUDIT_WRITE_LOCK + fs2 advisory）可能耗时数 ms，移入
        // spawn_blocking 避免阻塞 async worker；调用点串行 await，哈希链顺序不变。
        let logger = std::sync::Arc::clone(&self.audit_logger);
        let tool_name = call.name.clone();
        let params = args.clone();
        let trace_id = self.current_turn_id.clone();
        let call_id = call.id.clone();
        let outcome_str = outcome.as_str();
        let env_str = environment.as_str();
        match tokio::task::spawn_blocking(move || {
            let mut logger = logger.lock().unwrap_or_else(|e| e.into_inner());
            logger.set_session_id(&session_key);
            logger.log_tool_call_audit(blockcell_storage::ToolCallAudit {
                tool_name: &tool_name,
                params,
                result,
                session_key: &session_key,
                trace_id,
                duration_ms: Some(duration_ms),
                call_id: Some(call_id),
                outcome: Some(outcome_str),
                environment: Some(env_str),
            })
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!(tool = %call.name, error = %e, "Tool-call audit write failed"),
            Err(e) => warn!(tool = %call.name, error = %e, "Tool-call audit task failed"),
        }
    }

    /// 非交互通道依据用户原文中的显式确认，交互通道走确认请求。
    pub(super) async fn approve_dangerous_operation(
        &mut self,
        tool_name: &str,
        args: &serde_json::Value,
        items: Vec<String>,
        msg: &InboundMessage,
    ) -> bool {
        if self.confirm_tx.is_none() {
            let prefix = format!("确认执行 {} ", tool_name);
            let supplied = msg
                .content
                .trim()
                .strip_prefix(&prefix)
                .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok());
            if supplied.as_ref() != Some(args) {
                return false;
            }
            // 文本确认按消息一次性消费：同一条用户消息最多放行一个危险操作。
            // 否则"确认执行"四字等价于本回合全部危险操作全权放行。
            let key = format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}",
                msg.channel, msg.sender_id, msg.timestamp_ms, tool_name
            );
            let mut used = self
                .consumed_text_confirmations
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // Fail closed at capacity instead of making old approvals replayable.
            if used.len() >= 1024 {
                return false;
            }
            used.insert(key)
        } else {
            self.confirm_dangerous_operation(tool_name, items, msg)
                .await
        }
    }

    /// Hard block: reject disabled tools at execution level (not just prompt filtering)
    fn guard_disabled_toggles(&self, inv: &ToolInvocation<'_>) -> GuardResult {
        let name = &inv.call.name;
        let (disabled_tools, disabled_skills) =
            load_disabled_toggles_pair(&self.paths, "tools", "skills");
        if disabled_tools.contains(name) {
            return Err(disabled_tool_result(name));
        }
        // Also block disabled skills invoked as tools (skill scripts registered as tools)
        if disabled_skills.contains(name) {
            return Err(disabled_skill_result(name));
        }
        Ok(())
    }

    async fn guard_tool_policy(&mut self, inv: &mut ToolInvocation<'_>) -> GuardResult {
        match self
            .check_tool_policy(&inv.call.name, &inv.args, inv.msg)
            .await
        {
            PolicyOutcome::Proceed => Ok(()),
            PolicyOutcome::ProceedConfirmed => {
                inv.policy_confirmed = true;
                Ok(())
            }
            PolicyOutcome::Denied(reason) => Err(serde_json::json!({
                "error": reason,
                "tool": inv.call.name,
                "policy": "tool_policy"
            })
            .to_string()),
        }
    }

    /// Dangerous-operation gate: require explicit user confirmation before executing
    /// self-destructive commands or destructive file operations.
    async fn guard_dangerous_operation(&mut self, inv: &mut ToolInvocation<'_>) -> GuardResult {
        if inv.policy_confirmed {
            return Ok(());
        }
        let tool_name = inv.call.name.as_str();
        let approval_policy =
            ApprovalPolicy::parse(self.config.tools.exec.approval_policy.as_deref())
                .map_err(|error| error.to_string())?;
        let workspace_auto = approval_policy == ApprovalPolicy::WorkspaceAuto
            && matches!(tool_name, "exec" | "shell" | "file_ops");
        let mut items = dangerous_operation_items(tool_name, &inv.args);
        if workspace_auto {
            if workspace_auto_command(tool_name, &inv.args)
                .as_deref()
                .is_some_and(requires_human_confirmation)
                || self.workspace_auto_deletes_root(inv)
            {
                return Err(self.workspace_auto_denial(tool_name));
            }
            if items.is_empty() {
                return Ok(());
            }
            if self.workspace_auto_can_approve(inv) {
                return Ok(());
            }
            return Err(self.workspace_auto_denial(tool_name));
        }
        if items.is_empty() {
            return Ok(());
        }
        // 披露本次调用的执行环境（如 shell sandbox_policy=full-access）：批准
        // 危险命令会置 dangerous_confirmed 并短路 ExplicitConfirmation 阶段，
        // 提示若只显示 command 会让用户在不知情的情况下放行提权执行。
        let environment = self
            .tool_registry
            .execution_environment(tool_name, &inv.args);
        items.insert(0, format!("environment: {}", environment.as_str()));
        let interactive = self.confirm_tx.is_some();
        if self
            .approve_dangerous_operation(tool_name, &inv.args, items, inv.msg)
            .await
        {
            // 危险操作批准即视为本次调用的整体确认：后续的 PathAccess/ExplicitConfirmation
            // 阶段不再重复弹窗（Confirm 路径本次放行但不写入会话缓存，Deny 路径仍然硬性拒绝）。
            inv.dangerous_confirmed = true;
            Ok(())
        } else if matches!(tool_name, "exec" | "shell") {
            let denial = dangerous_exec_denied(tool_name, interactive);
            Err(if interactive {
                denial
            } else {
                format!("{}\n确认格式：确认执行 {} {}", denial, tool_name, inv.args)
            })
        } else {
            Err(dangerous_file_ops_denied())
        }
    }

    /// Check path safety before executing filesystem/exec tools
    async fn guard_path_access(&mut self, inv: &ToolInvocation<'_>) -> GuardResult {
        self.check_path_permission_with_confirmation(
            &inv.call.name,
            &inv.args,
            inv.msg,
            inv.policy_confirmed || inv.dangerous_confirmed,
            inv.policy_confirmed,
        )
        .await
        .map_err(|reason| crate::error::path_access_denied(&inv.call.name, &reason))
    }

    fn guard_file_observation(&self, inv: &ToolInvocation<'_>) -> GuardResult {
        if !matches!(inv.call.name.as_str(), "edit_file" | "write_file") {
            return Ok(());
        }
        let Some(path_str) = inv.args.get("path").and_then(serde_json::Value::as_str) else {
            return Ok(());
        };
        let path = self.resolve_observed_file_path(path_str);
        let current = file_version(&path);
        if inv.call.name == "write_file" && current.as_ref().is_ok_and(|version| version.is_none())
        {
            return Ok(());
        }

        let key = (inv.msg.session_key(), path.clone());
        let observed = self
            .file_observations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entries
            .get(&key)
            .copied();
        match observed {
            None if inv.call.name == "write_file" => Ok(()),
            None if inv.call.name == "edit_file" && path.is_file() => {
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                self.update_file_observation(&inv.msg.session_key(), path_str);
                let content: String = content.chars().take(8000).collect();
                Err(serde_json::json!({
                    "error": format!(
                        "FS_NOT_OBSERVED: current contents of {} follow; retry the edit using this content:\n{}",
                        path.display(),
                        content
                    ),
                    "code": "FS_NOT_OBSERVED",
                    "current_content": content
                })
                .to_string())
            }
            None => Err(serde_json::json!({
                "error": format!(
                    "FS_NOT_OBSERVED: {} has not been read in this session — read_file it, then retry",
                    path.display()
                ),
                "code": "FS_NOT_OBSERVED"
            })
            .to_string()),
            Some(observed) if current.ok().flatten() != Some(observed) => {
                Err(serde_json::json!({
                    "error": format!(
                        "FS_STALE_VERSION: {} changed since you last read it — read_file it again, then retry",
                        path.display()
                    ),
                    "code": "FS_STALE_VERSION"
                })
                .to_string())
            }
            Some(_) => Ok(()),
        }
    }

    fn resolve_observed_file_path(&self, path: &str) -> PathBuf {
        let path = self.resolve_path(path);
        path.canonicalize().unwrap_or(path)
    }

    fn update_file_observation(&self, session_key: &str, path: &str) {
        const MAX_FILE_OBSERVATIONS: usize = 4096;

        let path = self.resolve_observed_file_path(path);
        let Ok(Some(version)) = file_version(&path) else {
            return;
        };
        let key = (session_key.to_string(), path);
        let mut state = self
            .file_observations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.order.retain(|existing| existing != &key);
        state.order.push_back(key.clone());
        state.entries.insert(key, version);
        while state.entries.len() > MAX_FILE_OBSERVATIONS {
            if let Some(oldest) = state.order.pop_front() {
                state.entries.remove(&oldest);
            }
        }
    }

    /// 工具声明"本次调用需要用户显式确认"（如 `sandbox_policy=full-access` 的 shell）时，
    /// 先确认再执行。判定依据来自工具自身的 [`blockcell_tools::Tool::requires_confirmation`]
    /// 声明，而非执行环境，也不是硬编码工具名——环境是描述性的，只有需要确认才拦截。
    async fn guard_explicit_confirmation(&mut self, inv: &ToolInvocation<'_>) -> GuardResult {
        if inv.policy_confirmed || inv.dangerous_confirmed {
            return Ok(());
        }
        if matches!(inv.call.name.as_str(), "exec" | "shell")
            && ApprovalPolicy::parse(self.config.tools.exec.approval_policy.as_deref())
                .map_err(|error| error.to_string())?
                == ApprovalPolicy::WorkspaceAuto
        {
            return Ok(());
        }
        if !self
            .tool_registry
            .requires_confirmation(&inv.call.name, &inv.args)
        {
            return Ok(());
        }
        let environment = self
            .tool_registry
            .execution_environment(&inv.call.name, &inv.args);
        // Non-command tools would otherwise render an empty confirmation popup;
        // fall back to the generic argument summary so users see what is approved.
        let detail = inv
            .args
            .get("command")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| summarize_tool_args(&inv.args));
        let items = vec![format!(
            "{} {}: {}",
            environment.as_str(),
            inv.call.name,
            detail
        )];
        if self
            .approve_dangerous_operation(&inv.call.name, &inv.args, items, inv.msg)
            .await
        {
            Ok(())
        } else {
            Err(serde_json::json!({
                "error": format!(
                    "{} {} execution was not approved",
                    environment.as_str(),
                    inv.call.name
                ),
                "tool": inv.call.name,
                "environment": environment.as_str()
            })
            .to_string())
        }
    }

    fn workspace_auto_can_approve(&self, inv: &ToolInvocation<'_>) -> bool {
        let accesses = super::path_security::extract_path_accesses(&inv.call.name, &inv.args);
        if accesses.is_empty() {
            return false;
        }
        let session_key = persist_session_key_for(inv.msg);
        accesses.iter().all(|access| {
            let resolved = self.resolve_path(&access.path);
            let action = self.path_policy.evaluate(&resolved, access.op);
            action != PolicyAction::Deny
                && (self.is_path_safe(&resolved)
                    || self.is_path_authorized(&resolved, access.op, &session_key)
                    || action == PolicyAction::Allow)
        })
    }

    fn workspace_auto_deletes_root(&self, inv: &ToolInvocation<'_>) -> bool {
        let tool_name = inv.call.name.as_str();
        let mut targets = Vec::new();
        match tool_name {
            "file_ops"
                if inv.args.get("action").and_then(|value| value.as_str()) == Some("delete") =>
            {
                if let Some(path) = inv.args.get("path").and_then(|value| value.as_str()) {
                    targets.push((path.to_string(), PathOp::Write));
                }
                if let Some(paths) = inv.args.get("paths").and_then(|value| value.as_array()) {
                    targets.extend(paths.iter().filter_map(|value| {
                        value.as_str().map(|path| (path.to_string(), PathOp::Read))
                    }));
                }
            }
            "exec" | "shell" => {
                let Some(command) = workspace_auto_command(tool_name, &inv.args) else {
                    return false;
                };
                for segment in command.split(['&', '|', ';', '\n']) {
                    let command_name = segment
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_start_matches(['!', '{', '('])
                        .trim_end_matches(')');
                    if matches!(command_name, "cd" | "pushd")
                        || !segment
                            .split(|character: char| !character.is_ascii_alphanumeric())
                            .any(|word| matches!(word, "rm" | "rmdir" | "trash" | "unlink"))
                    {
                        continue;
                    }
                    let segment_args = serde_json::json!({"command": segment});
                    let accesses =
                        super::path_security::extract_path_accesses(tool_name, &segment_args);
                    targets.extend(accesses.into_iter().map(|access| (access.path, access.op)));
                }
            }
            _ => return false,
        }

        let workspace_root = resolve_for_policy(&self.paths.workspace());
        let session_key = persist_session_key_for(inv.msg);
        let authorized_dirs = self
            .authorized_dirs
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        targets.into_iter().any(|(path, op)| {
            let resolved = resolve_for_policy(&self.resolve_path(&path));
            resolved == workspace_root
                || authorized_dirs.iter().any(|(key, root, authorized_op)| {
                    key == &session_key && *authorized_op == op && root == &resolved
                })
        })
    }

    fn workspace_auto_denial(&self, tool_name: &str) -> String {
        let denial = if matches!(tool_name, "exec" | "shell") {
            dangerous_exec_denied(tool_name, self.confirm_tx.is_some())
        } else {
            dangerous_file_ops_denied()
        };
        let mut payload = serde_json::from_str::<serde_json::Value>(&denial)
            .unwrap_or_else(|_| serde_json::json!({"error": denial}));
        if let Some(object) = payload.as_object_mut() {
            object.insert(
                "hint".to_string(),
                serde_json::Value::String(
                    "approvalPolicy=workspace-auto only auto-approves workspace-internal operations; use workspace paths or ask the user."
                        .to_string(),
                ),
            );
        }
        payload.to_string()
    }

    async fn dispatch_tool_pre_execute(&mut self, inv: &mut ToolInvocation<'_>) -> GuardResult {
        let mut event = AgentEvent::ToolPreExecute {
            tool_name: inv.call.name.clone(),
            call_id: inv.call.id.clone(),
            arguments: inv.args.clone(),
        };
        let decision = self
            .dispatch_event(inv.msg, &persist_session_key_for(inv.msg), &mut event)
            .await;
        match event {
            AgentEvent::ToolPreExecute { arguments, .. } => inv.args = arguments,
            other => warn!(
                tool = %inv.call.name,
                event = other.name(),
                "tool/pre-execute listener replaced the event kind; keeping original arguments"
            ),
        }
        match decision {
            EventDecision::Continue => Ok(()),
            EventDecision::Reject { reason } => Err(serde_json::json!({
                "error": reason,
                "tool": inv.call.name,
                "policy": "event_listener"
            })
            .to_string()),
        }
    }

    pub(super) fn sandbox_writable_roots_for_session(&self, session_key: &str) -> Vec<PathBuf> {
        let mut roots = vec![resolve_for_policy(&self.paths.workspace())];
        roots.extend(self.path_policy.allowed_write_roots());
        let authorized_dirs = self
            .authorized_dirs
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        roots.extend(
            authorized_dirs
                .iter()
                .filter(|(key, _, op)| {
                    key == session_key && matches!(op, PathOp::Write | PathOp::Exec)
                })
                .map(|(_, root, _)| root.clone())
                .filter(|root| {
                    self.path_policy.evaluate(root, PathOp::Write) != PolicyAction::Deny
                }),
        );
        roots.retain(|root| {
            root.parent().is_some()
                && self.path_policy.evaluate(root, PathOp::Write) != PolicyAction::Deny
        });
        roots.sort();
        roots.dedup();
        roots
    }

    pub(crate) fn build_tool_context(
        &self,
        msg: &InboundMessage,
        active_skill_dir: Option<PathBuf>,
    ) -> blockcell_tools::ToolContext {
        // Build TaskManager handle for tools
        let tm_handle: TaskManagerHandle = Arc::new(OriginScopedTaskManager::new(
            self.task_manager.clone(),
            &msg.channel,
            &msg.chat_id,
        ));

        // Build spawn handle for tools
        let spawn_handle = Arc::new(RuntimeSpawnHandle {
            memory_file_store: self.memory_file_store.clone(),
            config: self.config.clone(),
            paths: self.paths.clone(),
            task_manager: self.task_manager.clone(),
            outbound_tx: self.outbound_tx.clone(),
            provider_pool: Arc::clone(&self.provider_pool),
            agent_id: resolve_routed_agent_id(&msg.metadata).or_else(|| self.agent_id.clone()),
            event_tx: self.event_tx.clone(),
            origin_account_id: msg.account_id.clone(),
            origin_session_key: msg.session_key(),
            response_cache: self.response_cache.clone(),
            event_emitter: self.system_event_emitter.clone(),
            abort_token: Some(self.abort_token.clone()),
            completion_reporter: super::subagent::SubagentCompletionReporter::new(
                self.event_bus.clone(),
                self.agent_id.clone(),
                persist_session_key_for(msg),
                &msg.channel,
                &msg.chat_id,
            ),
            parent_budget: Some(self.budget_tracker_for_session(&persist_session_key_for(msg))),
        });

        blockcell_tools::ToolContext {
            workspace: self.paths.workspace(),
            sandbox_writable_roots: Some(
                self.sandbox_writable_roots_for_session(&persist_session_key_for(msg)),
            ),
            base: self.paths.base.clone(),
            builtin_skills_dir: Some(self.paths.builtin_skills_dir()),
            active_skill_dir,
            session_key: msg.session_key(),
            channel: msg.channel.clone(),
            account_id: msg.account_id.clone(),
            sender_id: Some(msg.sender_id.clone()),
            sender_role: msg
                .metadata
                .get("sender_role")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            chat_id: msg.chat_id.clone(),
            config: self.config.clone(),
            permissions: self.build_tool_permissions(
                &msg.channel,
                Some(&msg.sender_id),
                &msg.chat_id,
            ),
            task_manager: Some(tm_handle),
            memory_store: self.memory_store.clone(),
            memory_file_store: self.memory_file_store.clone(),
            ghost_memory_lifecycle: self.ghost_memory_lifecycle.clone().map(|manager| {
                manager as Arc<dyn blockcell_tools::GhostMemoryLifecycleOps + Send + Sync>
            }),
            skill_file_store: self.skill_file_store.clone(),
            session_search: Some(Arc::new(RuntimeSessionSearch::new(
                self.paths.clone(),
                Some(msg.session_key()),
            ))),
            outbound_tx: self.outbound_tx.clone(),
            spawn_handle: Some(spawn_handle),
            capability_registry: self.capability_registry.clone(),
            core_evolution: self.core_evolution.clone(),
            event_emitter: Some(self.system_event_emitter.clone()),
            channel_contacts_file: Some(self.paths.channel_contacts_file()),
            response_cache: Some(
                Arc::new(self.response_cache.clone()) as blockcell_tools::ResponseCacheHandle
            ),
            runtime_handle: self.runtime_handle.clone(),
            agent_identity: blockcell_core::current_agent_context(),
            skill_mutex: Some(self.skill_mutex.clone()),
            agent_type_registry: Some(Arc::new(self.agent_type_registry.clone())
                as blockcell_tools::AgentTypeRegistryHandle),
            evolution_workflow_store: self.evolution_workflow_store.clone().map(|store| {
                Arc::new(EvolutionWorkflowStoreAdapter::new((*store).clone()))
                    as blockcell_tools::EvolutionWorkflowStoreHandle
            }),
        }
    }

    /// 执行工具；shell 在沙箱内失败且需要提权时，审批通过后以 full-access 重试。
    ///
    /// 若携带了只读预取结果且准入阶段未改写参数，则直接复用预取结果，跳过工具体的实际
    /// 执行（准入、事件、审计、归类流程保持不变）。
    async fn execute_guarded_tool(
        &mut self,
        inv: &mut ToolInvocation<'_>,
        ctx: blockcell_tools::ToolContext,
        prefetched: Option<super::prefetch::PrefetchedToolResult>,
    ) -> ToolExecution {
        let tool_name = &inv.call.name;
        if let Some(prefetched) = prefetched {
            if prefetched.args == inv.args {
                return self.finish_tool_execution(
                    tool_name,
                    &inv.args,
                    prefetched.result,
                    prefetched.duration_ms,
                );
            }
        }
        let start = std::time::Instant::now();
        let mut result = self
            .execute_runtime_tool_call(tool_name, ctx.clone(), inv.args.clone())
            .await;
        if tool_name == "shell" && shell_action_is_run(&inv.args) {
            if let Some(reason) = shell_sandbox_escalation_reason(&result) {
                let approved = self
                    .approve_dangerous_operation(
                        "shell",
                        &full_access_retry_args(&inv.args),
                        vec![format!(
                            "sandbox escalation ({reason}): {}",
                            inv.args
                                .get("command")
                                .and_then(|value| value.as_str())
                                .unwrap_or_default()
                        )],
                        inv.msg,
                    )
                    .await;
                if approved {
                    inv.args = full_access_retry_args(&inv.args);
                    result = self
                        .execute_runtime_tool_call(tool_name, ctx, inv.args.clone())
                        .await;
                } else {
                    result = Err(blockcell_core::Error::PermissionDenied(format!(
                        "sandbox escalation was not approved: {reason}"
                    )));
                }
            }
        }
        let duration_ms = start.elapsed().as_millis() as u64;
        self.finish_tool_execution(tool_name, &inv.args, result, duration_ms)
    }

    /// 由工具执行结果构造 [`ToolExecution`]（结论、幂等性、结果字符串）。
    fn finish_tool_execution(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        result: blockcell_core::Result<serde_json::Value>,
        duration_ms: u64,
    ) -> ToolExecution {
        let idempotency = self.tool_registry.idempotency(tool_name, args);
        let failure = result
            .as_ref()
            .err()
            .map(|error| FailureKind::classify_tool_error(error, idempotency));
        let (result_str, result_json) = match result {
            Ok(val) => (val.to_string(), val),
            Err(e) => {
                let err_str = format!("Error: {}", e);
                (err_str.clone(), serde_json::json!({"error": err_str}))
            }
        };
        // Tools often report failure as `Ok({"error": ...})` / `{"status": "error"}`
        // instead of an Err. Classify those as failed as well, consistent with
        // `tool_call_failed` on the main loop — otherwise evolution signals and
        // audit outcomes record soft failures as successes.
        let outcome = ToolOutcome::from_failure(failure.or_else(|| {
            tool_result_indicates_error(&result_str).then_some(FailureKind::ToolFailed)
        }));
        ToolExecution {
            result_str,
            result_json,
            duration_ms,
            outcome,
            idempotency,
        }
    }

    /// tool/post-execute 事件以及依赖执行结果的运行时副作用（技能热加载、索引刷新）。
    async fn after_tool_execute(&mut self, inv: &ToolInvocation<'_>, outcome: &ToolExecution) {
        let tool_call = inv.call;
        self.dispatch_event(
            inv.msg,
            &persist_session_key_for(inv.msg),
            &mut AgentEvent::ToolPostExecute {
                tool_name: tool_call.name.clone(),
                call_id: tool_call.id.clone(),
                arguments: inv.args.clone(),
                result: outcome.result_json.clone(),
                is_error: outcome.is_error(),
                duration_ms: outcome.duration_ms,
            },
        )
        .await;

        if outcome.is_error() {
            return;
        }

        if matches!(
            tool_call.name.as_str(),
            "read_file" | "write_file" | "edit_file"
        ) {
            if let Some(path) = inv.args.get("path").and_then(serde_json::Value::as_str) {
                self.update_file_observation(&inv.msg.session_key(), path);
            }
            if tool_call.name == "read_file" {
                if let Some(paths) = inv.args.get("paths").and_then(serde_json::Value::as_array) {
                    for path in paths.iter().filter_map(serde_json::Value::as_str) {
                        self.update_file_observation(&inv.msg.session_key(), path);
                    }
                }
            }
        }

        // Detect writes to the skills directory and trigger hot-reload + Dashboard refresh
        if matches!(
            tool_call.name.as_str(),
            "write_file" | "edit_file" | "skill_manage"
        ) {
            if let Some(path_str) = inv.args.get("path").and_then(|v| v.as_str()) {
                let resolved = self.resolve_path(path_str);
                let skills_dir = self.paths.skills_dir();
                let in_skills = resolved.starts_with(&skills_dir)
                    || resolved.canonicalize().ok().is_some_and(|c| {
                        skills_dir
                            .canonicalize()
                            .ok()
                            .is_some_and(|sd| c.starts_with(&sd))
                    });
                if in_skills {
                    info!(path = %path_str, "🔄 Detected write to skills directory, reloading...");
                    let new_skills = self.context_builder.reload_skills();
                    if !new_skills.is_empty() {
                        info!(skills = ?new_skills, "🔄 Hot-reloaded new skills");
                    }
                    // 刷新 Skill 索引摘要 (使下次 LLM 调用获取最新 Skill 列表)
                    self.context_builder.refresh_skill_index_summary();
                    // Always broadcast so Dashboard refreshes (even for updates to existing skills)
                    if let Some(ref event_tx) = self.event_tx {
                        let event = serde_json::json!({
                            "type": "skills_updated",
                            "new_skills": new_skills,
                        });
                        let _ = event_tx.send(event.to_string());
                    }
                }
            }
        }

        // Detect skill_manage changes and refresh Skill index summary
        if tool_call.name == "skill_manage" {
            let action = inv
                .args
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if matches!(
                action,
                "create" | "patch" | "delete" | "edit" | "write_file" | "remove_file"
            ) {
                debug!(
                    action = action,
                    "🔄 skill_manage modified skills, refreshing index summary"
                );
                self.context_builder.refresh_skill_index_summary();
            }
        }
    }

    /// 结果归类：失败时上报自进化并生成学习提示，同时上报灰度统计。
    async fn classify_tool_outcome(
        &mut self,
        tool_call: &ToolCallRequest,
        outcome: &ToolExecution,
    ) -> Option<String> {
        let mut learning_hint: Option<String> = None;
        let signal = evolution_signal(outcome.outcome);
        if outcome.is_error() && outcome.result_str.contains("Unknown tool:") {
            learning_hint = Some(format!(
                "[系统] 工具 `{}` 未注册/不可用（Unknown tool）。这不是可通过技能自进化修复的问题。\
                请改用已存在的工具完成任务，或提示用户安装/启用对应工具。",
                tool_call.name
            ));
        } else if signal == Some(true) {
            if let Some(evo_service) = self.context_builder.evolution_service() {
                // OpenClaw skill 不触发自进化
                let is_openclaw = self
                    .context_builder
                    .skill_manager()
                    .is_some_and(|sm| sm.is_tool_from_openclaw(&tool_call.name));
                if is_openclaw {
                    debug!(
                        tool = %tool_call.name,
                        "Skipping evolution for OpenClaw skill"
                    );
                } else {
                    // Preserve any legacy top-level Rhai asset as supplemental evolution context.
                    let source_snippet = self
                        .context_builder
                        .skill_manager()
                        .and_then(|sm| sm.get(&tool_call.name))
                        .and_then(|skill| skill.load_rhai());
                    match evo_service
                        .report_error(&tool_call.name, &outcome.result_str, source_snippet, vec![])
                        .await
                    {
                        Ok(report) => {
                            if report.evolution_triggered.is_some() {
                                if let Some(ref worker) = self.skill_evolution_worker {
                                    worker.notify();
                                }
                                learning_hint = Some(format!(
                                    "[系统] 技能 `{}` 执行失败，已自动触发进化学习。\
                                请向用户坦诚说明：你暂时还不具备这个技能，但已经开始学习，\
                                学会后会自动生效。同时尝试用其他方式帮助用户解决当前问题。",
                                    tool_call.name
                                ));
                            } else if report.evolution_in_progress {
                                learning_hint = Some(format!(
                                    "[系统] 技能 `{}` 执行失败，该技能正在学习改进中。\
                                请告诉用户：这个技能正在学习中，请稍后再试。",
                                    tool_call.name
                                ));
                            }
                        }
                        Err(e) => {
                            debug!(error = %e, "Evolution report_error failed");
                        }
                    }
                }
            }
        }
        // 报告调用结果给灰度统计（OpenClaw skill 跳过；非技能缺陷类结论不计入）
        if let (Some(is_error), Some(evo_service)) =
            (signal, self.context_builder.evolution_service())
        {
            let is_openclaw = self
                .context_builder
                .skill_manager()
                .is_some_and(|sm| sm.is_tool_from_openclaw(&tool_call.name));
            if !is_openclaw {
                evo_service
                    .report_skill_call(&tool_call.name, is_error)
                    .await;
            }
        }
        learning_hint
    }

    /// tool/result 事件、审计日志、学习计数与 Layer4 文件追踪。
    async fn record_tool_result(&mut self, inv: &ToolInvocation<'_>, outcome: &ToolExecution) {
        let tool_call = inv.call;
        let msg = inv.msg;
        self.dispatch_event(
            msg,
            &persist_session_key_for(msg),
            &mut AgentEvent::ToolResult {
                tool_name: tool_call.name.clone(),
                call_id: tool_call.id.clone(),
                result: outcome.result_json.clone(),
                duration_ms: outcome.duration_ms,
                outcome: outcome.outcome,
                idempotency: outcome.idempotency,
            },
        )
        .await;
        if !outcome.is_error() {
            if let Some(mut event) = semantic_tool_event(tool_call, &outcome.result_json) {
                self.dispatch_event(msg, &persist_session_key_for(msg), &mut event)
                    .await;
            }
        }

        self.audit_tool_call(
            tool_call,
            &inv.args,
            outcome.result_json.clone(),
            msg,
            outcome.outcome,
            outcome.duration_ms,
        )
        .await;

        // Skill Nudge: 两个独立计数器 (Skill + Memory)
        // 与 Hermes 一致: 只有 skill_manage 写操作重置 Skill 计数器 (view/list_skills 等只读操作不重置)
        // 与 Hermes 一致: 只有 memory 写操作重置 Memory 计数器 (memory_query 等只读操作不重置)
        let tool_name_str = tool_call.name.as_str();
        let is_skill_write_tool = tool_name_str == "skill_manage"
            && matches!(
                inv.args
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                "create" | "patch" | "edit" | "delete" | "write_file" | "remove_file"
            );
        let is_memory_write_tool = matches!(
            tool_name_str,
            "memory_manage" | "memory_upsert" | "memory_forget" | "auto_memory"
        );

        // Skill/Memory write tools reset corresponding counters via learning coordinator
        if is_skill_write_tool {
            self.learning_coordinator.reset_skill();
        }
        if is_memory_write_tool {
            self.learning_coordinator.reset_memory();
            self.context_builder.invalidate_file_memory_snapshots();
        }

        // Layer 4: Track file reads for Post-Compact recovery
        // 追踪多种文件访问工具的结果，用于 Compact 后恢复
        if !outcome.is_error() {
            let result_str = &outcome.result_str;
            let file_content_to_track: Option<(std::path::PathBuf, &str)> =
                match tool_call.name.as_str() {
                    "read_file" => {
                        // read_file: 直接追踪文件内容
                        if let Some(path_str) = inv.args.get("path").and_then(|v| v.as_str()) {
                            Some((self.resolve_path(path_str), result_str))
                        } else {
                            None
                        }
                    }
                    "grep" | "rg" => {
                        // grep/rg: 追踪搜索路径和匹配结果
                        let path = inv.args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                        Some((self.resolve_path(path), result_str))
                    }
                    "glob" => {
                        // glob: 追踪匹配的文件列表
                        let path = inv.args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                        Some((self.resolve_path(path), result_str))
                    }
                    _ => None,
                };

            if let Some((path, content)) = file_content_to_track {
                if let Some(memory_system) = self.memory_system.as_mut() {
                    memory_system.record_file_read(path.clone(), content);
                    debug!(path = %path.display(), tool = %tool_call.name, "[layer4] Tracked file access for recovery");
                }
            }
        }
    }
}
