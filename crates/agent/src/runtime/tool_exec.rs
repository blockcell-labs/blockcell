use super::*;

impl AgentRuntime {
    pub(crate) async fn execute_runtime_tool_call(
        &self,
        tool_name: &str,
        ctx: blockcell_tools::ToolContext,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        if let Some(manager) = self.ghost_memory_lifecycle.as_ref() {
            if manager.has_tool(tool_name) {
                return manager.handle_tool_call(tool_name, arguments);
            }
        }
        self.tool_registry.execute(tool_name, ctx, arguments).await
    }

    /// Layer 1: 在截断前将大型工具结果持久化到磁盘。
    ///
    /// 如果持久化成功，返回 `<persisted-output>` 存根字符串；
    /// 如果失败则返回 `None`，由调用方进行内联截断。
    ///
    /// 路径格式：`.tool_results/{session_key}/{tool_id}_{call_uuid}/output.txt`
    /// 引入 `session_key` 和 `call_uuid` 避免 `text_call_0`/`ollama_call_0` 等
    /// 通用 ID 跨轮次、跨会话重复，导致旧会话 recall 拿到被覆盖的错误内容。
    ///
    /// `.tool_results/` 目录通过 maintenance tick 中的 `cleanup_tool_results` 定期清理
    ///（TTL 7 天 + 每会话上限 50 条目），长期运行不会无限累积磁盘占用。
    pub(crate) async fn try_persist_large_tool_result(
        &self,
        content: &str,
        tool_call_id: Option<&str>,
        session_key: &str,
        call_uuid: &str,
    ) -> Option<String> {
        let tool_id = sanitize_tool_use_id(tool_call_id.unwrap_or("unknown"));
        // 使用 sanitize_session_key 替代 sanitize_tool_use_id，防止不同会话映射到同一目录
        // （sanitize_tool_use_id 会删除分隔符，导致 "a.b" 和 "a-b" 冲突）
        let session_id = sanitize_session_key(session_key);
        let dir_name = format!("{}_{call_uuid}", tool_id);
        let persistence_dir = self
            .paths
            .workspace()
            .join(".tool_results")
            .join(&session_id)
            .join(&dir_name);
        let output_file = persistence_dir.join("output.txt");

        match tokio::fs::create_dir_all(&persistence_dir).await {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(
                    tool_id = %tool_id,
                    error = %e,
                    "[layer1] Failed to create tool result persistence directory"
                );
                return None;
            }
        }

        match tokio::fs::write(&output_file, content).await {
            Ok(()) => {
                let byte_size = content.len();
                let char_count = content.chars().count();
                // 使用 Layer1 配置的预览大小（以字符为单位，默认 2000），
                // 按 2/3 头部 + 1/3 尾部分配，与 fallback 截断路径保持一致
                let preview_size = self.response_cache.preview_size_chars();
                let head_size = preview_size * 2 / 3;
                let tail_size = preview_size - head_size;
                let head: String = content.chars().take(head_size).collect();
                let tail: String = content
                    .chars()
                    .rev()
                    .take(tail_size)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                let trimmed_chars = char_count.saturating_sub(preview_size);
                // 包含完整 dir_name（含 UUID）作为精确引用 ID，
                // session_recall 通过 tool: 前缀识别工具结果 ID，
                // 格式 tool:{tool_id}:{call_uuid} 支持 UUID 精确匹配
                let recall_id = format!(
                    "tool:{tool_id}:{call_uuid}",
                    tool_id = tool_id,
                    call_uuid = call_uuid
                );
                let stub = format!(
                    "<persisted-output>\n\
                     tool_use_id: {tool_id}\n\
                     recall_id: {recall_id}\n\
                     file: {output_path}\n\
                     size: {byte_size} bytes ({char_count} chars)\n\
                     \n\
                     {head}\n\
                     ...<trimmed {trimmed_chars} chars>...\n\
                     {tail}\n\
                     \n\
                     </persisted-output>",
                    tool_id = tool_id,
                    recall_id = recall_id,
                    output_path = output_file.display(),
                    byte_size = byte_size,
                    char_count = char_count,
                    head = head,
                    trimmed_chars = trimmed_chars,
                    tail = tail,
                );
                // Record metrics event — 使用真实的 session_key、preview 大小和 truncated 标志
                let filepath_display = output_file.display().to_string();
                let preview_size = head.len() + tail.len();
                memory_event!(
                    layer1,
                    persisted,
                    tool_id,
                    byte_size as u64,
                    preview_size as u64,
                    filepath_display.as_str(),
                    session_key,
                    true // 工具输出已被截断替换为 preview
                );
                memory_event!(layer1, preview_generated, tool_id, stub.len() as u64);
                tracing::info!(
                    tool_id = %tool_id,
                    path = %output_file.display(),
                    byte_size = byte_size,
                    "[layer1] Persisted large tool result to disk"
                );
                Some(stub)
            }
            Err(e) => {
                tracing::warn!(
                    tool_id = %tool_id,
                    error = %e,
                    "[layer1] Failed to persist large tool result"
                );
                None
            }
        }
    }

    pub(crate) async fn check_tool_policy(
        &mut self,
        tool_name: &str,
        tool_args: &serde_json::Value,
        msg: &InboundMessage,
    ) -> PolicyOutcome {
        let eval = self.tool_policy.evaluate(&ToolCallContext {
            tool_name,
            tool_args,
            channel: &msg.channel,
        });
        self.audit_policy_decision(tool_name, &eval, msg);

        match eval.decision {
            ToolPolicyDecision::Allow => PolicyOutcome::Proceed,
            ToolPolicyDecision::Deny => {
                let reason = eval.description.unwrap_or_else(|| {
                    format!(
                        "Policy denied tool call by rule: {}",
                        eval.matched_rule.as_deref().unwrap_or("default")
                    )
                });
                PolicyOutcome::Denied(reason)
            }
            ToolPolicyDecision::Ask => {
                let items = vec![format!("{}: {}", tool_name, summarize_tool_args(tool_args))];
                if self
                    .confirm_dangerous_operation(tool_name, items, msg)
                    .await
                {
                    PolicyOutcome::ProceedConfirmed
                } else {
                    PolicyOutcome::Denied("用户拒绝了该操作".to_string())
                }
            }
        }
    }

    pub(crate) fn audit_policy_decision(
        &mut self,
        tool_name: &str,
        eval: &PolicyEvalResult,
        msg: &InboundMessage,
    ) {
        self.audit_logger.set_session_id(&msg.session_key());
        let _ = self.audit_logger.log_permission_decision(
            tool_name,
            format!("{:?}", eval.decision),
            eval.matched_rule.clone(),
            eval.description.clone(),
            eval.simulated,
            &msg.session_key(),
        );
    }

    pub(crate) async fn execute_tool_call(
        &mut self,
        tool_call: &ToolCallRequest,
        msg: &InboundMessage,
        active_skill_dir: Option<PathBuf>,
    ) -> String {
        // Hard block: reject disabled tools at execution level (not just prompt filtering)
        let disabled_tools = load_disabled_toggles(&self.paths, "tools");
        if disabled_tools.contains(&tool_call.name) {
            return disabled_tool_result(&tool_call.name);
        }
        // Also block disabled skills invoked as tools (skill scripts registered as tools)
        let disabled_skills = load_disabled_toggles(&self.paths, "skills");
        if disabled_skills.contains(&tool_call.name) {
            return disabled_skill_result(&tool_call.name);
        }

        let policy_confirmed = match self
            .check_tool_policy(&tool_call.name, &tool_call.arguments, msg)
            .await
        {
            PolicyOutcome::Proceed => false,
            PolicyOutcome::ProceedConfirmed => true,
            PolicyOutcome::Denied(reason) => {
                return serde_json::json!({
                    "error": reason,
                    "tool": tool_call.name,
                    "policy": "tool_policy"
                })
                .to_string();
            }
        };

        // Dangerous-operation gate: require explicit user confirmation before executing
        // self-destructive commands or destructive file operations.
        if !policy_confirmed && tool_call.name == "exec" {
            if let Some(cmd) = tool_call.arguments.get("command").and_then(|v| v.as_str()) {
                if is_dangerous_exec_command(cmd) {
                    let items = vec![format!("command: {}", cmd)];
                    if self.confirm_tx.is_none() {
                        if !user_explicitly_confirms_dangerous_op(&msg.content) {
                            return dangerous_exec_denied(false);
                        }
                    } else if !self.confirm_dangerous_operation("exec", items, msg).await {
                        return dangerous_exec_denied(true);
                    }
                }
            }
        }

        if !policy_confirmed && tool_call.name == "file_ops" {
            let action = tool_call
                .arguments
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let path = tool_call
                .arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let destination = tool_call
                .arguments
                .get("destination")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let recursive = tool_call
                .arguments
                .get("recursive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let mut items = Vec::new();
            if action == "delete" && recursive {
                items.push(format!("file_ops delete recursive=true path={}", path));
            }
            if (action == "delete" || action == "rename" || action == "move")
                && (is_sensitive_filename(path) || is_sensitive_filename(destination))
            {
                items.push(format!(
                    "file_ops {} sensitive file (config*) path={} destination={}",
                    action, path, destination
                ));
            }

            if !items.is_empty() {
                if self.confirm_tx.is_none() {
                    if !user_explicitly_confirms_dangerous_op(&msg.content) {
                        return dangerous_file_ops_denied();
                    }
                } else if !self
                    .confirm_dangerous_operation("file_ops", items, msg)
                    .await
                {
                    return dangerous_file_ops_denied();
                }
            }
        }

        // Check path safety before executing filesystem/exec tools
        if !self
            .check_path_permission_with_confirmation(
                &tool_call.name,
                &tool_call.arguments,
                msg,
                policy_confirmed,
            )
            .await
        {
            return crate::error::path_access_denied(&tool_call.name, "outside workspace");
        }

        // Build TaskManager handle for tools
        let tm_handle: TaskManagerHandle = Arc::new(OriginScopedTaskManager::new(
            self.task_manager.clone(),
            &msg.channel,
            &msg.chat_id,
        ));

        // Build spawn handle for tools
        let spawn_handle = Arc::new(RuntimeSpawnHandle {
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
        });

        let ctx = blockcell_tools::ToolContext {
            workspace: self.paths.workspace(),
            base: self.paths.base.clone(),
            builtin_skills_dir: Some(self.paths.builtin_skills_dir()),
            active_skill_dir,
            session_key: msg.session_key(),
            channel: msg.channel.clone(),
            account_id: msg.account_id.clone(),
            sender_id: Some(msg.sender_id.clone()),
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
        };

        let mut effective_args = tool_call.arguments.clone();
        if tool_call.name == "shell"
            && shell_action_is_run(&effective_args)
            && shell_requests_full_access(&effective_args)
            && !policy_confirmed
        {
            let approved = if self.confirm_tx.is_none() {
                user_explicitly_confirms_dangerous_op(&msg.content)
            } else {
                self.confirm_dangerous_operation(
                    "shell",
                    vec![format!(
                        "full-access command: {}",
                        effective_args
                            .get("command")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                    )],
                    msg,
                )
                .await
            };
            if !approved {
                return serde_json::json!({
                    "error": "full-access shell execution was not approved",
                    "tool": "shell",
                    "sandbox_policy": "full-access"
                })
                .to_string();
            }
        }

        let mut pre_execute = AgentEvent::ToolPreExecute {
            tool_name: tool_call.name.clone(),
            call_id: tool_call.id.clone(),
            arguments: effective_args,
        };
        let pre_decision = self
            .dispatch_event(msg, &msg.session_key(), &mut pre_execute)
            .await;
        let AgentEvent::ToolPreExecute { arguments, .. } = pre_execute else {
            unreachable!("tool/pre-execute listeners cannot replace the event kind");
        };
        effective_args = arguments;
        if let EventDecision::Reject { reason } = pre_decision {
            return serde_json::json!({
                "error": reason,
                "tool": tool_call.name,
                "policy": "event_listener"
            })
            .to_string();
        }

        let start = std::time::Instant::now();
        let mut result = self
            .execute_runtime_tool_call(&tool_call.name, ctx.clone(), effective_args.clone())
            .await;
        if tool_call.name == "shell" && shell_action_is_run(&effective_args) {
            if let Some(reason) = shell_sandbox_escalation_reason(&result) {
                let approved = policy_confirmed
                    || if self.confirm_tx.is_none() {
                        user_explicitly_confirms_dangerous_op(&msg.content)
                    } else {
                        self.confirm_dangerous_operation(
                            "shell",
                            vec![format!(
                                "sandbox escalation ({reason}): {}",
                                effective_args
                                    .get("command")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or_default()
                            )],
                            msg,
                        )
                        .await
                    };
                if approved {
                    effective_args = full_access_retry_args(&effective_args);
                    result = self
                        .execute_runtime_tool_call(&tool_call.name, ctx, effective_args.clone())
                        .await;
                } else {
                    result = Err(blockcell_core::Error::PermissionDenied(format!(
                        "sandbox escalation was not approved: {reason}"
                    )));
                }
            }
        }
        let duration_ms = start.elapsed().as_millis() as u64;

        let is_error = result.is_err();
        let (result_str, result_json) = match &result {
            Ok(val) => (val.to_string(), val.clone()),
            Err(e) => {
                let err_str = format!("Error: {}", e);
                (err_str.clone(), serde_json::json!({"error": err_str}))
            }
        };

        self.dispatch_event(
            msg,
            &msg.session_key(),
            &mut AgentEvent::ToolPostExecute {
                tool_name: tool_call.name.clone(),
                call_id: tool_call.id.clone(),
                arguments: effective_args.clone(),
                result: result_json.clone(),
                is_error,
                duration_ms,
            },
        )
        .await;

        // Detect writes to the skills directory and trigger hot-reload + Dashboard refresh
        if !is_error
            && matches!(
                tool_call.name.as_str(),
                "write_file" | "edit_file" | "skill_manage"
            )
        {
            if let Some(path_str) = tool_call.arguments.get("path").and_then(|v| v.as_str()) {
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
        if !is_error && tool_call.name == "skill_manage" {
            let action = tool_call
                .arguments
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

        let mut learning_hint: Option<String> = None;
        if is_error {
            let is_unknown_tool = result_str.contains("Unknown tool:");

            if is_unknown_tool {
                learning_hint = Some(format!(
                    "[系统] 工具 `{}` 未注册/不可用（Unknown tool）。这不是可通过技能自进化修复的问题。\
                    请改用已存在的工具完成任务，或提示用户安装/启用对应工具。",
                    tool_call.name
                ));
            } else if let Some(evo_service) = self.context_builder.evolution_service() {
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
                        .report_error(&tool_call.name, &result_str, source_snippet, vec![])
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
        // 报告调用结果给灰度统计（OpenClaw skill 跳过）
        if let Some(evo_service) = self.context_builder.evolution_service() {
            let is_openclaw = self
                .context_builder
                .skill_manager()
                .is_some_and(|sm| sm.is_tool_from_openclaw(&tool_call.name));
            if !is_openclaw {
                let reported_name = tool_call.name.clone();
                evo_service
                    .report_skill_call(&reported_name, is_error)
                    .await;
            }
        }

        self.dispatch_event(
            msg,
            &msg.session_key(),
            &mut AgentEvent::ToolResult {
                tool_name: tool_call.name.clone(),
                call_id: tool_call.id.clone(),
                result: result_json.clone(),
                is_error,
                duration_ms,
            },
        )
        .await;

        // Log to audit
        self.audit_logger.set_session_id(&msg.session_key());
        let _ = self.audit_logger.log_tool_call(
            &tool_call.name,
            effective_args.clone(),
            result_json,
            &msg.session_key(),
            None, // trace_id can be added later
            Some(duration_ms),
        );

        // Skill Nudge: 两个独立计数器 (Skill + Memory)
        // 与 Hermes 一致: 只有 skill_manage 写操作重置 Skill 计数器 (view/list_skills 等只读操作不重置)
        // 与 Hermes 一致: 只有 memory 写操作重置 Memory 计数器 (memory_query 等只读操作不重置)
        let tool_name_str = tool_call.name.as_str();
        let is_skill_write_tool = tool_name_str == "skill_manage"
            && matches!(
                tool_call
                    .arguments
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
        }

        // Layer 4: Track file reads for Post-Compact recovery
        // 追踪多种文件访问工具的结果，用于 Compact 后恢复
        if !is_error {
            let file_content_to_track: Option<(std::path::PathBuf, &str)> =
                match tool_call.name.as_str() {
                    "read_file" => {
                        // read_file: 直接追踪文件内容
                        if let Some(path_str) =
                            tool_call.arguments.get("path").and_then(|v| v.as_str())
                        {
                            Some((self.resolve_path(path_str), &result_str))
                        } else {
                            None
                        }
                    }
                    "grep" | "rg" => {
                        // grep/rg: 追踪搜索路径和匹配结果
                        let path = tool_call
                            .arguments
                            .get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or(".");
                        Some((self.resolve_path(path), &result_str))
                    }
                    "glob" => {
                        // glob: 追踪匹配的文件列表
                        let path = tool_call
                            .arguments
                            .get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or(".");
                        Some((self.resolve_path(path), &result_str))
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

        // 在工具结果中追加学习提示，让 LLM 自然地回复用户
        match learning_hint {
            Some(hint) => format!("{}\n\n{}", result_str, hint),
            None => result_str,
        }
    }
}

fn shell_action_is_run(arguments: &serde_json::Value) -> bool {
    arguments
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or("run")
        == "run"
}

fn shell_requests_full_access(arguments: &serde_json::Value) -> bool {
    arguments
        .get("sandbox_policy")
        .and_then(|value| value.as_str())
        == Some("full-access")
}

pub(super) fn shell_sandbox_escalation_reason(
    result: &blockcell_core::Result<serde_json::Value>,
) -> Option<String> {
    match result {
        Err(blockcell_core::Error::PermissionDenied(message))
            if message.contains("sandbox") && message.contains("approval required") =>
        {
            Some(message.clone())
        }
        Ok(value)
            if value.get("sandbox_policy").and_then(|item| item.as_str())
                != Some("full-access")
                && value
                    .get("exit_code")
                    .and_then(|item| item.as_i64())
                    .is_some_and(|code| code != 0) =>
        {
            let output = value
                .get("output")
                .and_then(|item| item.as_str())
                .unwrap_or_default();
            let lower = output.to_ascii_lowercase();
            [
                "operation not permitted",
                "permission denied",
                "network is unreachable",
                "sandbox violation",
            ]
            .iter()
            .find(|marker| lower.contains(**marker))
            .map(|marker| (*marker).to_string())
        }
        _ => None,
    }
}

fn full_access_retry_args(arguments: &serde_json::Value) -> serde_json::Value {
    let mut retry = arguments.clone();
    if let Some(object) = retry.as_object_mut() {
        object.remove("session_id");
        object.insert(
            "sandbox_policy".to_string(),
            serde_json::Value::String("full-access".to_string()),
        );
    }
    retry
}
