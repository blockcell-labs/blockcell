//! 进程内 [`SubagentProvider`]：把现有 fork / typed agent 收编到统一的 provider 契约下。
//!
//! - Fork：继承父会话上下文，前台执行，句柄被丢弃且未取结果时自动取消；
//! - Typed：按 agent 类型定义后台执行，状态与取消经 TaskManager，结果回传到发起会话；
//! - 工具权限由请求中的 [`ToolGrant`] 显式决定，不复制父代理权限。

use super::*;
use blockcell_tools::subagent::{
    SubagentHandle, SubagentKind, SubagentLineage, SubagentPermissions, SubagentProvider,
    SubagentRequest, SubagentStatus, ToolGrant,
};

/// Upper bound for waiting on a typed subagent result. A task that stays
/// `Running` forever (stuck provider, lost lease) must not block the calling
/// tool indefinitely.
const TYPED_SUBAGENT_RESULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

#[derive(Clone)]
pub struct InProcessSubagentProvider {
    invocation_target: Option<MainSessionTarget>,
    invocation_budget: Option<BudgetTrackerHandle>,
    model_prices: Vec<blockcell_core::config::ModelEntry>,
    provider_pool: Arc<ProviderPool>,
    /// 与 AgentRuntime 共享，始终读取当前主会话目标。
    main_session_target: Arc<std::sync::RwLock<Option<MainSessionTarget>>>,
    task_manager: TaskManager,
    paths: Paths,
    event_tx: Option<broadcast::Sender<String>>,
    outbound_tx: Option<mpsc::Sender<OutboundMessage>>,
    abort_token: AbortToken,
    agent_type_registry: crate::agent_types::AgentTypeRegistry,
    skill_mutex: blockcell_tools::SkillMutexHandle,
    memory_store: Option<MemoryStoreHandle>,
    memory_file_store: Option<blockcell_tools::MemoryFileStoreHandle>,
    skill_file_store: Option<blockcell_tools::SkillFileStoreHandle>,
    system_prompt_snapshots: crate::context::SystemPromptSnapshotReader,
    event_bus: EventBus,
    agent_id: Option<String>,
}

impl InProcessSubagentProvider {
    pub fn from_runtime(runtime: &AgentRuntime) -> Self {
        Self {
            invocation_target: None,
            invocation_budget: None,
            model_prices: runtime.config.agents.defaults.model_pool.clone(),
            provider_pool: runtime.provider_pool.clone(),
            main_session_target: runtime.shared_session_target.clone(),
            task_manager: runtime.task_manager.clone(),
            paths: runtime.paths.clone(),
            event_tx: runtime.event_tx.clone(),
            outbound_tx: runtime.outbound_tx.clone(),
            abort_token: runtime.abort_token.clone(),
            agent_type_registry: runtime.agent_type_registry.clone(),
            skill_mutex: runtime.skill_mutex.clone(),
            memory_store: runtime.memory_store.clone(),
            memory_file_store: runtime.memory_file_store.clone(),
            skill_file_store: runtime.skill_file_store.clone(),
            system_prompt_snapshots: runtime.context_builder.system_prompt_snapshot_reader(),
            event_bus: runtime.event_bus.clone(),
            agent_id: runtime.agent_id.clone(),
        }
    }

    pub(crate) fn for_invocation(mut self, runtime: &AgentRuntime, msg: &InboundMessage) -> Self {
        self.invocation_budget =
            Some(runtime.budget_tracker_for_session(&persist_session_key_for(msg)));
        self.invocation_target = Some(MainSessionTarget {
            channel: msg.channel.clone(),
            account_id: msg.account_id.clone(),
            chat_id: msg.chat_id.clone(),
            session_key: persist_session_key_for(msg),
            agent_id: runtime.agent_id.clone(),
        });
        self
    }

    fn main_session_target(&self) -> Option<MainSessionTarget> {
        if let Some(target) = self.invocation_target.as_ref() {
            return Some(target.clone());
        }
        self.main_session_target
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn parent_session_id(&self) -> String {
        self.main_session_target()
            .map(|target| target.session_key)
            .unwrap_or_else(|| "internal:default".to_string())
    }

    fn lineage(&self, agent_id: &str) -> SubagentLineage {
        SubagentLineage {
            agent_id: agent_id.to_string(),
            parent_session_id: self.parent_session_id(),
            parent_agent_id: self
                .main_session_target()
                .and_then(|target| target.agent_id),
        }
    }

    /// 把显式授予的工具权限解析为禁用工具列表。
    fn resolve_disallowed_tools(
        permissions: &SubagentPermissions,
        agent_type_defaults: Option<&[String]>,
    ) -> Result<Vec<String>> {
        match (permissions.tools, agent_type_defaults) {
            (ToolGrant::ReadOnly, defaults) => {
                if !permissions.workspace_scope.is_empty() {
                    return Err(blockcell_core::Error::Validation(
                        "a read-only subagent cannot be granted a workspace_scope".to_string(),
                    ));
                }
                let mut disallowed = crate::forked::default_read_only_fork_disallowed_tools();
                for tool in defaults.unwrap_or_default() {
                    if !disallowed.contains(tool) {
                        disallowed.push(tool.clone());
                    }
                }
                Ok(disallowed)
            }
            (ToolGrant::AgentType, Some(defaults)) => Ok(defaults.to_vec()),
            (ToolGrant::AgentType, None) => Err(blockcell_core::Error::Validation(
                "ToolGrant::AgentType requires a typed subagent".to_string(),
            )),
        }
    }

    fn spawn_fork(&self, request: SubagentRequest) -> Result<Box<dyn SubagentHandle>> {
        use crate::forked::{
            run_forked_agent, CacheSafeParams, ForkedAgentParams, SubagentOverrides,
        };
        use blockcell_core::types::ChatMessage;

        let disallowed = Self::resolve_disallowed_tools(&request.permissions, None)?;
        let parent_session_id = self.parent_session_id();
        // 加载父对话历史（用于 fork 上下文继承）
        let parent_history = SessionStore::new(self.paths.clone())
            .load(&parent_session_id)
            .unwrap_or_default();
        let fork_agent_id = format!("fork-{}", short_uuid());
        let identity = AgentIdentity::fork_child(fork_agent_id.clone(), parent_session_id);
        // 子 token 挂在调用方 token 下，父代理取消时链式取消
        let child_abort_token = blockcell_core::current_abort_token()
            .map(|token| token.child())
            .unwrap_or_default();

        let provider = self.clone();
        let prompt = request.prompt;
        let token_for_task = child_abort_token.clone();
        let join = tokio::spawn(scope_abort_token(
            token_for_task.clone(),
            scope_agent_context(identity.clone(), async move {
                info!(agent_id = %identity.agent_id, role = "fork-child", "Executing fork mode");

                let safe_prompt = AgentRuntime::sanitize_fork_prompt(&prompt);
                let fork_messages = vec![
                    ChatMessage::system(
                        "You are a forked agent. Execute directly without spawning subagents.",
                    ),
                    ChatMessage::user(&format!(
                        "<fork_directive>\n\
                    RULES:\n\
                    1. Do NOT spawn sub-agents; execute directly.\n\
                    2. Do NOT converse; execute and report results.\n\
                    3. USE tools: Read, Grep, Glob, Bash (read-only).\n\
                    4. Keep report under 500 words.\n\
                    \n\
                    Task: {}",
                        safe_prompt
                    )),
                ];

                let cache_safe_params = CacheSafeParams {
                    fork_context_messages: parent_history,
                    ..CacheSafeParams::default()
                };
                let overrides = SubagentOverrides {
                    abort_token: Some(token_for_task),
                    ..Default::default()
                };

                let tool_schemas = crate::forked::build_forked_tool_schemas(&disallowed);
                let mut builder = ForkedAgentParams::builder()
                    .provider_pool(provider.provider_pool.clone())
                    .prompt_messages(fork_messages)
                    .cache_safe_params(cache_safe_params)
                    .fork_label("fork")
                    .max_turns(10)
                    .agent_type(None)
                    .disallowed_tools(disallowed)
                    .one_shot(true)
                    .overrides(overrides)
                    .tool_schemas(tool_schemas);
                builder = provider.attach_shared_capabilities(builder);

                let params = builder.build().map_err(|e| {
                    blockcell_core::Error::Tool(format!("ForkedAgentParams build failed: {}", e))
                })?;
                let result = run_forked_agent(params)
                    .await
                    .map_err(|e| blockcell_core::Error::Tool(format!("Fork failed: {}", e)))?;

                // 如果工具结果被截断，追加提示让用户知道
                let mut content = result
                    .final_content
                    .unwrap_or_else(|| "Fork completed with no output".to_string());
                if result.truncated {
                    tracing::warn!("[fork] 工具结果被截断，可能丢失部分信息");
                    content.push_str("\n\n[注意: 结果因长度限制被截断，可能丢失部分信息]");
                }
                Ok(content)
            }),
        ));

        Ok(Box::new(ForkHandle {
            lineage: self.lineage(&fork_agent_id),
            abort_token: child_abort_token,
            join: tokio::sync::Mutex::new(Some(join)),
            finished: std::sync::Mutex::new(None),
        }))
    }

    /// 进度转发、技能/记忆存储等子代理共享的运行时能力。
    fn attach_shared_capabilities(
        &self,
        mut builder: crate::forked::ForkedAgentParamsBuilder,
    ) -> crate::forked::ForkedAgentParamsBuilder {
        if let Some(ref tx) = self.event_tx {
            builder = builder.event_tx(tx.clone());
        }
        if let Some(tx) = self.task_manager.progress_tx() {
            builder = builder.progress_tx(tx);
        }
        if let (Some(target), Some(budget)) = (&self.invocation_target, &self.invocation_budget) {
            builder = builder.observation(crate::forked::ForkObservation {
                paths: self.paths.clone(),
                session_key: target.session_key.clone(),
                budget: budget.clone(),
                model_prices: self.model_prices.clone(),
            });
        }
        builder = builder.skill_mutex(self.skill_mutex.clone());
        if let Some(ref store) = self.memory_store {
            builder = builder.memory_store(store.clone());
        }
        if let Some(ref store) = self.memory_file_store {
            builder = builder.memory_file_store(store.clone());
        }
        if let Some(ref store) = self.skill_file_store {
            builder = builder.skill_file_store(store.clone());
        }
        builder.skills_dir(self.paths.skills_dir())
    }

    async fn spawn_typed(
        &self,
        agent_type: &str,
        request: SubagentRequest,
    ) -> Result<Box<dyn SubagentHandle>> {
        use crate::forked::{
            run_forked_agent, CacheSafeParams, ForkedAgentParams, SubagentOverrides,
        };
        use blockcell_core::types::ChatMessage;

        let def = self.agent_type_registry.get(agent_type).ok_or_else(|| {
            blockcell_core::Error::Tool(format!("Unknown agent type: {}", agent_type))
        })?;
        let disallowed_tools =
            Self::resolve_disallowed_tools(&request.permissions, Some(&def.disallowed_tools))?;
        let SubagentRequest {
            prompt,
            description,
            permissions,
            ..
        } = request;
        let workspace_scope = permissions.workspace_scope;

        let task_id = format!("task-{}", short_uuid());
        let workspace = self.paths.workspace();
        let workspace_scope_entries = resolve_typed_workspace_scope(&workspace, &workspace_scope);
        if agent_type == "coder" {
            if workspace_scope_entries.is_empty() {
                return Err(blockcell_core::Error::Validation(
                    "coder agent requires a non-empty workspace_scope".to_string(),
                ));
            }
            let paths = workspace_scope_entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>();
            self.task_manager
                .try_acquire_workspace_scope(&task_id, &paths)?;
        }

        let lineage = self.lineage(&task_id);
        let parent_system_prompt = self
            .system_prompt_snapshots
            .get(&lineage.parent_session_id)
            .unwrap_or_default();
        let identity = AgentIdentity::typed(
            task_id.clone(),
            agent_type.to_string(),
            lineage.parent_session_id.clone(),
        );
        let target = self.main_session_target();
        let (channel, chat_id) = target
            .as_ref()
            .map(|t| (t.channel.clone(), t.chat_id.clone()))
            .unwrap_or_else(|| ("internal".to_string(), "default".to_string()));
        let session_key_for_persist = target.as_ref().map(|t| t.session_key.clone());
        let reporter = SubagentCompletionReporter::new(
            self.event_bus.clone(),
            self.agent_id.clone(),
            session_key_for_persist.clone().unwrap_or_default(),
            &channel,
            &chat_id,
        );

        // 原子性地创建并标记为 Running（消除竞态条件）
        self.task_manager
            .create_and_start_task(
                &task_id,
                description.as_deref().unwrap_or(agent_type),
                &prompt,
                &channel,
                &chat_id,
                Some(&task_id),
                false,
                Some(agent_type),
                def.one_shot,
            )
            .await;
        self.task_manager
            .send_progress(crate::agent_progress::AgentProgress::Delta {
                task_id: task_id.clone(),
                tokens_added: 0,
                tools_added: 0,
                total_tokens: 0,
                total_tools: 0,
            })
            .await;

        let prompt = if workspace_scope.is_empty() {
            prompt
        } else {
            format!(
                "{}\n\nAssigned workspace_scope (all writes must stay inside it):\n- {}",
                prompt,
                workspace_scope.join("\n- ")
            )
        };
        // Create child AbortToken for chain cancellation
        let child_abort_token = self.abort_token.child();
        self.task_manager
            .register_abort_token(&task_id, child_abort_token.clone());

        // 检查是否需要 worktree 隔离（基于 AgentTypeDefinition.isolation 配置）
        let needs_worktree = def.isolation == Some(crate::agent_types::IsolationMode::Worktree);
        let worktree_path = if !needs_worktree {
            None
        } else if is_in_worktree(&workspace).await {
            warn!(task_id = %task_id, "Already in worktree, skipping nested worktree creation");
            None
        } else {
            match create_worktree(&workspace, &task_id).await {
                Ok(path) => {
                    info!(task_id = %task_id, worktree = %path.display(), "Created worktree for typed agent");
                    Some(path)
                }
                Err(e) => {
                    warn!(task_id = %task_id, error = %e, "Failed to create worktree, proceeding in current directory");
                    None
                }
            }
        };

        let def = def.clone();
        let provider = self.clone();
        let agent_type = agent_type.to_string();
        let task_id_for_task = task_id.clone();
        let parent_agent_id = lineage.parent_agent_id.clone();
        let guard_worktree_path = worktree_path.clone();
        let guard_workspace = workspace.clone();
        let join_handle = tokio::spawn(async move {
            let task_id = task_id_for_task;
            let result = scope_abort_token(
                child_abort_token,
                scope_agent_context(identity.clone(), async {
                    info!(
                        agent_id = %identity.agent_id,
                        agent_type = %agent_type,
                        "Executing typed agent in background"
                    );
                    let messages = vec![
                        ChatMessage::system(def.system_prompt_template.as_deref().unwrap_or(
                            "You are a specialized agent. Execute the task efficiently.",
                        )),
                        ChatMessage::user(&prompt),
                    ];
                    let cache_safe_params = CacheSafeParams::new(
                        parent_system_prompt,
                        def.model.clone().unwrap_or_default(),
                    );
                    let overrides = SubagentOverrides {
                        abort_token: blockcell_core::current_abort_token(),
                        working_dir: worktree_path.clone(),
                        ..Default::default()
                    };
                    let tool_schemas = crate::forked::build_forked_tool_schemas(&disallowed_tools);
                    let mut builder = ForkedAgentParams::builder()
                        .provider_pool(provider.provider_pool.clone())
                        .prompt_messages(messages)
                        .cache_safe_params(cache_safe_params)
                        .fork_label("typed")
                        .agent_type(Some(agent_type.clone()))
                        .task_id(Some(task_id.clone()))
                        .workspace_scope(workspace_scope_entries)
                        .disallowed_tools(disallowed_tools)
                        .one_shot(def.one_shot)
                        .overrides(overrides)
                        .tools(def.tools.clone())
                        .model(def.model.clone())
                        .skills(def.skills.clone())
                        .mcp_servers(def.mcp_servers.clone())
                        .initial_prompt(def.initial_prompt.clone())
                        .background(def.background)
                        .color(def.color.clone())
                        .tool_schemas(tool_schemas);
                    if let Some(turns) = def.max_turns {
                        builder = builder.max_turns(turns);
                    }
                    builder = provider.attach_shared_capabilities(builder);
                    match builder.build() {
                        Ok(p) => run_forked_agent(p).await.map_err(|e| {
                            blockcell_core::Error::Tool(format!("Forked agent error: {}", e))
                        }),
                        Err(e) => Err(blockcell_core::Error::Tool(format!(
                            "ForkedAgentParams build failed: {}",
                            e
                        ))),
                    }
                }),
            )
            .await;

            let task_manager = &provider.task_manager;
            let was_cancelled = task_manager
                .get_task(&task_id)
                .await
                .is_some_and(|task| task.status == TaskStatus::Cancelled);
            let delivery = match result {
                _ if was_cancelled => {
                    info!(task_id = %task_id, "Typed agent stopped after cancellation; suppressing result delivery");
                    task_manager.unregister_abort_token(&task_id);
                    reporter.report(&task_id, false, "cancelled").await;
                    None
                }
                Ok(output) => {
                    let content = output
                        .final_content
                        .unwrap_or_else(|| "Task completed with no output".to_string());
                    let content = normalize_typed_agent_result(&agent_type, &content);
                    task_manager.set_completed(&task_id, &content).await;
                    info!(task_id = %task_id, "Typed agent completed successfully");
                    reporter.report(&task_id, true, &content).await;
                    Some(content)
                }
                Err(e) => {
                    let err_msg = format!("{}", e);
                    task_manager.set_failed(&task_id, &err_msg).await;
                    warn!(task_id = %task_id, error = %e, "Typed agent failed");
                    reporter.report(&task_id, false, &err_msg).await;
                    Some(format!(
                        "\n❌ 后台任务失败: **{}** (ID: {})\n错误: {}",
                        agent_type,
                        truncate_str(&task_id, 8),
                        err_msg
                    ))
                }
            };
            // 结果（或失败信息）回传到发起会话
            if let Some(content) = delivery {
                let session_store = SessionStore::new(provider.paths.clone());
                deliver_subagent_result_to_origin(
                    &channel,
                    &chat_id,
                    &content,
                    &task_id,
                    parent_agent_id.as_deref(),
                    provider.outbound_tx.clone(),
                    provider.event_tx.clone(),
                    Some(&session_store),
                    session_key_for_persist.as_deref(),
                )
                .await;
            }

            if worktree_path.is_some() {
                cleanup_worktree(&workspace, &task_id).await;
            }
        });

        // Guard: if the task panics, mark it Failed so it is not stuck in Running,
        // and clean up the worktree since the task's own cleanup will not run.
        let guard_task_manager = self.task_manager.clone();
        let guard_task_id = task_id.clone();
        tokio::spawn(async move {
            if let Err(e) = join_handle.await {
                if e.is_panic() {
                    warn!(task_id = %guard_task_id, error = %e, "Typed agent task panicked");
                    guard_task_manager
                        .set_failed(&guard_task_id, &format!("Task panicked: {}", e))
                        .await;
                    if guard_worktree_path.is_some() {
                        cleanup_worktree(&guard_workspace, &guard_task_id).await;
                    }
                } else {
                    warn!(task_id = %guard_task_id, "Typed agent task was cancelled (not a panic)");
                }
            }
        });

        Ok(Box::new(TypedHandle {
            lineage,
            task_manager: self.task_manager.clone(),
        }))
    }
}

#[async_trait::async_trait]
impl SubagentProvider for InProcessSubagentProvider {
    fn name(&self) -> &'static str {
        "in-process"
    }

    async fn spawn(&self, request: SubagentRequest) -> Result<Box<dyn SubagentHandle>> {
        match request.kind.clone() {
            SubagentKind::Fork => self.spawn_fork(request),
            SubagentKind::Typed { agent_type } => self.spawn_typed(&agent_type, request).await,
        }
    }
}

/// 前台 fork 子代理。句柄被丢弃而结果未取走时取消子代理。
struct ForkHandle {
    lineage: SubagentLineage,
    abort_token: AbortToken,
    join: tokio::sync::Mutex<Option<tokio::task::JoinHandle<Result<String>>>>,
    finished: std::sync::Mutex<Option<SubagentStatus>>,
}

#[async_trait::async_trait]
impl SubagentHandle for ForkHandle {
    fn id(&self) -> &str {
        &self.lineage.agent_id
    }

    fn lineage(&self) -> &SubagentLineage {
        &self.lineage
    }

    async fn status(&self) -> SubagentStatus {
        if let Some(status) = *self.finished.lock().unwrap_or_else(|p| p.into_inner()) {
            return status;
        }
        if self.abort_token.is_cancelled() {
            SubagentStatus::Cancelled
        } else {
            SubagentStatus::Running
        }
    }

    async fn cancel(&self) {
        self.abort_token.cancel();
    }

    async fn result(&self) -> Result<String> {
        let join = self.join.lock().await.take().ok_or_else(|| {
            blockcell_core::Error::Tool("fork result has already been taken".to_string())
        })?;
        let outcome = join
            .await
            .map_err(|e| blockcell_core::Error::Tool(format!("Fork task failed: {}", e)))
            .and_then(|result| result);
        let status = match &outcome {
            Ok(_) => SubagentStatus::Completed,
            Err(_) if self.abort_token.is_cancelled() => SubagentStatus::Cancelled,
            Err(_) => SubagentStatus::Failed,
        };
        *self.finished.lock().unwrap_or_else(|p| p.into_inner()) = Some(status);
        outcome
    }
}

impl Drop for ForkHandle {
    fn drop(&mut self) {
        if let Some(join) = self.join.get_mut().take() {
            self.abort_token.cancel();
            join.abort();
        }
    }
}

/// 后台 typed 子代理：状态、取消与结果都经 TaskManager。
struct TypedHandle {
    lineage: SubagentLineage,
    task_manager: TaskManager,
}

#[async_trait::async_trait]
impl SubagentHandle for TypedHandle {
    fn id(&self) -> &str {
        &self.lineage.agent_id
    }

    fn lineage(&self) -> &SubagentLineage {
        &self.lineage
    }

    async fn status(&self) -> SubagentStatus {
        match self
            .task_manager
            .get_task(self.id())
            .await
            .map(|t| t.status)
        {
            Some(TaskStatus::Completed) => SubagentStatus::Completed,
            Some(TaskStatus::Failed) | None => SubagentStatus::Failed,
            Some(TaskStatus::Cancelled) => SubagentStatus::Cancelled,
            Some(TaskStatus::Queued | TaskStatus::Running) => SubagentStatus::Running,
        }
    }

    async fn cancel(&self) {
        if let Err(error) = self.task_manager.cancel_task(self.id()).await {
            warn!(task_id = %self.id(), error = %error, "Failed to cancel typed subagent");
        }
    }

    async fn result(&self) -> Result<String> {
        let deadline = tokio::time::Instant::now() + TYPED_SUBAGENT_RESULT_TIMEOUT;
        loop {
            let task = self.task_manager.get_task(self.id()).await.ok_or_else(|| {
                blockcell_core::Error::NotFound(format!("subagent task {}", self.id()))
            })?;
            match task.status {
                TaskStatus::Completed => return Ok(task.result.unwrap_or_default()),
                TaskStatus::Failed => {
                    return Err(blockcell_core::Error::Tool(
                        task.error.unwrap_or_else(|| "subagent failed".to_string()),
                    ))
                }
                TaskStatus::Cancelled => {
                    return Err(blockcell_core::Error::Tool(format!(
                        "subagent {} was cancelled",
                        self.id()
                    )))
                }
                TaskStatus::Queued | TaskStatus::Running => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(blockcell_core::Error::Timeout(format!(
                            "subagent {} did not finish within {}s",
                            self.id(),
                            TYPED_SUBAGENT_RESULT_TIMEOUT.as_secs()
                        )));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await
                }
            }
        }
    }
}

fn short_uuid() -> String {
    uuid::Uuid::new_v4()
        .to_string()
        .split('-')
        .next()
        .unwrap_or("unknown")
        .to_string()
}

/// Detect if the workspace is already inside a git worktree.
async fn is_in_worktree(workspace: &std::path::Path) -> bool {
    let git_file = workspace.join(".git");
    if !tokio::fs::try_exists(&git_file).await.unwrap_or(false) {
        return false;
    }
    tokio::fs::read_to_string(&git_file)
        .await
        .is_ok_and(|content| content.starts_with("gitdir:"))
}

fn worktree_name(task_id: &str) -> String {
    format!("agent-{}", &task_id[..16.min(task_id.len())])
}

fn worktree_path(workspace: &std::path::Path, task_id: &str) -> PathBuf {
    workspace
        .join(".claude")
        .join("worktrees")
        .join(worktree_name(task_id))
}

/// Create a git worktree for isolated agent execution.
async fn create_worktree(workspace: &std::path::Path, task_id: &str) -> Result<PathBuf> {
    let worktree_name = worktree_name(task_id);
    let worktree_path = worktree_path(workspace, task_id);
    let worktree_parent = worktree_path.parent().ok_or_else(|| {
        blockcell_core::Error::Other(format!(
            "Invalid worktree path: {}",
            worktree_path.display()
        ))
    })?;
    tokio::fs::create_dir_all(worktree_parent)
        .await
        .map_err(blockcell_core::Error::Io)?;

    let output = tokio::process::Command::new("git")
        .args([
            "worktree",
            "add",
            "-b",
            &worktree_name,
            &worktree_path.display().to_string(),
        ])
        .current_dir(workspace)
        .output()
        .await
        .map_err(blockcell_core::Error::Io)?;
    if !output.status.success() {
        return Err(blockcell_core::Error::Other(format!(
            "Failed to create worktree: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    info!(
        "Created worktree at {} with branch {}",
        worktree_path.display(),
        worktree_name
    );
    Ok(worktree_path)
}

/// Clean up a git worktree after agent task completion.
/// 检查未提交更改，避免 --force 丢失工作。
async fn cleanup_worktree(workspace: &std::path::Path, task_id: &str) {
    let worktree_name = worktree_name(task_id);
    let worktree_path = worktree_path(workspace, task_id);
    let status_result = tokio::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&worktree_path)
        .output()
        .await;
    if status_result
        .as_ref()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
    {
        warn!(
            worktree = %worktree_name,
            "Worktree has uncommitted changes, preserving it for manual review"
        );
        return;
    }

    let removed = tokio::process::Command::new("git")
        .args(["worktree", "remove", &worktree_path.display().to_string()])
        .current_dir(workspace)
        .output()
        .await;
    if !removed.is_ok_and(|output| output.status.success()) {
        warn!("Failed to remove worktree: {}", worktree_name);
    }
    let branch = tokio::process::Command::new("git")
        .args(["branch", "-D", &worktree_name])
        .current_dir(workspace)
        .output()
        .await;
    if branch.is_ok_and(|output| output.status.success()) {
        info!("Cleaned up worktree and branch {}", worktree_name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_must_be_granted_explicitly() {
        let read_only = InProcessSubagentProvider::resolve_disallowed_tools(
            &SubagentPermissions::read_only(),
            None,
        )
        .unwrap();
        assert_eq!(
            read_only,
            crate::forked::default_read_only_fork_disallowed_tools()
        );

        // 只读授权与 agent 类型的禁用列表取并集，而不是被类型默认放宽
        let defaults = vec!["custom_tool".to_string()];
        let typed_read_only = InProcessSubagentProvider::resolve_disallowed_tools(
            &SubagentPermissions::read_only(),
            Some(&defaults),
        )
        .unwrap();
        assert!(typed_read_only.contains(&"custom_tool".to_string()));
        assert!(read_only.iter().all(|tool| typed_read_only.contains(tool)));

        assert_eq!(
            InProcessSubagentProvider::resolve_disallowed_tools(
                &SubagentPermissions::agent_type(vec!["src".into()]),
                Some(&defaults),
            )
            .unwrap(),
            defaults
        );
        assert!(InProcessSubagentProvider::resolve_disallowed_tools(
            &SubagentPermissions::agent_type(Vec::new()),
            None,
        )
        .is_err());
        assert!(InProcessSubagentProvider::resolve_disallowed_tools(
            &SubagentPermissions {
                tools: ToolGrant::ReadOnly,
                workspace_scope: vec!["src".into()],
            },
            None,
        )
        .is_err());
    }
}
