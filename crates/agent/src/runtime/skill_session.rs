use super::*;

impl AgentRuntime {
    pub(crate) async fn run_prompt_skill_for_session(
        &mut self,
        active_skill: &ActiveSkillContext,
        msg: &InboundMessage,
        history: &[ChatMessage],
        session_key: &str,
        tool_names: &[String],
    ) -> Result<(String, Vec<ChatMessage>, serde_json::Value)> {
        let (disabled_tools, disabled_skills) =
            load_disabled_toggles_pair(&self.paths, "tools", "skills");

        let mode_names = vec![format!("Skill:{}", active_skill.name)];
        let prompt_ctx = blockcell_tools::PromptContext {
            channel: &msg.channel,
            intents: &mode_names,
            default_timezone: self.config.default_timezone.as_deref(),
        };
        let tool_name_refs = tool_names
            .iter()
            .map(|name| name.as_str())
            .collect::<Vec<_>>();
        let tool_prompt_rules = self
            .tool_registry
            .get_prompt_rules(&tool_name_refs, &prompt_ctx);
        let pending_intent = msg
            .metadata
            .get("media_pending_intent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut session_metadata = self.session_store.load_metadata(session_key)?;
        let mut task_context = crate::task_context::TaskContext::from_metadata(&session_metadata);
        let continuing = task_context.observe_user(&msg.content);
        let mut skill_msg = msg.clone();
        if let Some(contract) = msg.metadata.get("task_contract").or_else(|| {
            continuing
                .then(|| session_metadata.get("runtime_task_contract"))
                .flatten()
        }) {
            skill_msg.metadata["task_contract"] = contract.clone();
        }
        // 先异步预取项目环境探测（git 子进程），避免同步构建阻塞 executor
        self.context_builder
            .prefetch_project_environment(session_key)
            .await;
        let mut messages = self
            .context_builder
            .build_messages_for_session_mode_with_channel(
                session_key,
                history,
                &msg.content,
                &msg.media,
                InteractionMode::Skill,
                Some(active_skill),
                &disabled_skills,
                &disabled_tools,
                &msg.channel,
                pending_intent,
                tool_names,
                &tool_prompt_rules,
            );

        if let Some(context) = task_context.render() {
            messages = append_ephemeral_context_to_latest_user_message(&messages, Some(&context))
                .into_owned();
        }

        let mut tools = if tool_names.is_empty() {
            Vec::new()
        } else {
            self.tool_registry.get_tiered_schemas(
                &tool_name_refs,
                blockcell_tools::registry::GLOBAL_CORE_TOOL_NAMES,
            )
        };
        if !disabled_tools.is_empty() {
            tools.retain(|schema| {
                let name = schema
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                !disabled_tools.contains(name)
            });
        }

        let prompt_result = self
            .run_prompt_skill_loop(
                &skill_msg,
                messages,
                tools,
                tool_names,
                self.context_builder
                    .skill_manager()
                    .and_then(|manager| manager.get(&active_skill.name))
                    .map(|skill| skill.path.clone()),
            )
            .await?;

        session_metadata["runtime_task_contract"] =
            serde_json::to_value(&prompt_result.task_contract)?;

        for (name, outcome) in &prompt_result.tool_outcomes {
            task_context.record_tool(name, outcome);
        }
        task_context.write_metadata(&mut session_metadata);
        let mut final_response = prompt_result.final_response;
        if let Some(last_local_exec_tool_name) =
            Self::last_local_exec_tool_name(&prompt_result.trace_messages)
        {
            if let Some(summary_bundle) = self
                .context_builder
                .skill_manager()
                .and_then(|manager| manager.get(&active_skill.name))
                .and_then(|skill| skill.load_summary_bundle())
            {
                let summary_system_prompt = concat!(
                    "You are blockcell, an AI assistant with access to tools.\n\n",
                    "You are in a final summary-only step for a script-backed skill. ",
                    "Follow the skill summary instructions, preserve factual meaning, and output only the user-facing answer. ",
                    "Do not call tools.\n"
                );
                let summary_prompt = build_script_skill_summary_prompt(
                    &msg.content,
                    &active_skill.name,
                    &last_local_exec_tool_name,
                    &summary_bundle,
                    &final_response,
                );
                let summary_messages = vec![
                    ChatMessage::system(summary_system_prompt),
                    ChatMessage::user(&summary_prompt),
                ];
                let summary_response = self
                    .chat_with_provider(&summary_messages, &[])
                    .await?
                    .0
                    .content
                    .unwrap_or_default();
                if !summary_response.trim().is_empty() {
                    final_response = summary_response;
                }
            }
        }

        if final_response.trim().is_empty() {
            if let Some(summary) = self
                .summarize_skill_tool_trace(&msg.content, &prompt_result.trace_messages)
                .await
            {
                final_response = summary;
            }
        }

        final_response =
            apply_skill_fallback_response(final_response, active_skill.fallback_message.as_deref());

        Ok((
            final_response,
            prompt_result.trace_messages,
            session_metadata,
        ))
    }

    pub(super) async fn summarize_skill_tool_trace(
        &self,
        user_content: &str,
        trace_messages: &[ChatMessage],
    ) -> Option<String> {
        if !trace_messages.iter().any(|message| message.role == "tool") {
            return None;
        }
        let mut messages = vec![
            ChatMessage::system(
                "Summarize for the user, in the user's language, what was done and the results/outcome of the tool calls below; if the task failed say exactly what failed. Do not call tools.",
            ),
            ChatMessage::user(user_content),
        ];
        messages.extend_from_slice(trace_messages);
        match self.chat_with_provider(&messages, &[]).await {
            Ok((response, _)) => response
                .content
                .filter(|content| !content.trim().is_empty()),
            Err(error) => {
                tracing::warn!(error = %error, "Failed to summarize skill tool results");
                None
            }
        }
    }

    pub(crate) async fn deliver_skill_response(
        &self,
        msg: &InboundMessage,
        final_response: &str,
        cron_kind: Option<&str>,
    ) {
        if let Some((channel, to)) = resolve_cron_deliver_target(msg) {
            if channel == "ws" {
                if let Some(ref event_tx) = self.event_tx {
                    let mut event = serde_json::json!({
                        "type": "message_done",
                        "channel": "ws",
                        "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                        "chat_id": to,
                        "task_id": "",
                        "content": final_response,
                        "tool_calls": 0,
                        "duration_ms": 0,
                        "media": [],
                        "background_delivery": true,
                        "delivery_kind": "cron",
                    });
                    if let Some(cron_kind) = cron_kind {
                        event["cron_kind"] = serde_json::json!(cron_kind);
                    }
                    let _ = event_tx.send(event.to_string());
                }
                return;
            }

            if let Some(tx) = &self.outbound_tx {
                let mut outbound = OutboundMessage::new(&channel, &to, final_response);
                outbound.account_id = msg.account_id.clone();
                let _ = tx.send(outbound).await;
            }
            return;
        }

        if msg.channel == "ws" {
            if let Some(ref event_tx) = self.event_tx {
                let event = serde_json::json!({
                    "type": "message_done",
                    "channel": msg.channel,
                    "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                    "chat_id": msg.chat_id,
                    "task_id": "",
                    "content": final_response,
                    "tool_calls": 0,
                    "duration_ms": 0,
                    "media": [],
                });
                let _ = event_tx.send(event.to_string());
            }
        }

        if let Some(tx) = &self.outbound_tx {
            let mut outbound = OutboundMessage::new(&msg.channel, &msg.chat_id, final_response);
            outbound.account_id = msg.account_id.clone();
            outbound.metadata = extract_reply_metadata(msg);
            let _ = tx.send(outbound).await;
        }
    }

    #[allow(dead_code)]
    #[deprecated(
        note = "Legacy compatibility helper for direct SKILL.rhai execution. Prefer SKILL.md-driven exec_skill_script flows."
    )]
    pub(crate) async fn run_rhai_script_with_context(
        &self,
        rhai_path: &std::path::Path,
        skill_name: &str,
        msg: &InboundMessage,
        extra_ctx: Option<serde_json::Value>,
    ) -> Result<String> {
        use blockcell_skills::dispatcher::SkillDispatcher;
        use std::collections::HashMap;

        let script = tokio::fs::read_to_string(rhai_path).await.map_err(|e| {
            blockcell_core::Error::Skill(format!("Failed to read {}: {}", rhai_path.display(), e))
        })?;

        // Build a synchronous tool executor that uses the tool registry
        let registry = self.tool_registry.clone();
        let config = self.config.clone();
        let paths = self.paths.clone();
        let session_key = msg.session_key();
        let channel = msg.channel.clone();
        let chat_id = msg.chat_id.clone();
        let task_manager = self.task_manager.clone();
        let memory_store = self.memory_store.clone();
        let outbound_tx = self.outbound_tx.clone();
        let capability_registry = self.capability_registry.clone();
        let core_evolution = self.core_evolution.clone();
        let event_emitter = self.system_event_emitter.clone();
        let evolution_workflow_store = self.evolution_workflow_store.clone();
        let ghost_memory_lifecycle = self.ghost_memory_lifecycle.clone();

        let tool_executor =
            move |tool_name: &str, params: serde_json::Value| -> Result<serde_json::Value> {
                // Security gate: block disabled tools/skills in skill scripts
                let (disabled_tools, disabled_skills) =
                    load_disabled_toggles_pair(&paths, "tools", "skills");
                if disabled_tools.contains(tool_name) {
                    return Err(blockcell_core::Error::Tool(format!(
                        "Tool '{}' is disabled via toggles",
                        tool_name
                    )));
                }
                if disabled_skills.contains(tool_name) {
                    return Err(blockcell_core::Error::Tool(format!(
                        "Skill '{}' is disabled via toggles",
                        tool_name
                    )));
                }

                // Security gate: block dangerous exec commands from skill scripts
                if tool_name == "exec" {
                    if let Some(cmd) = params.get("command").and_then(|v| v.as_str()) {
                        if is_dangerous_exec_command(cmd) {
                            return Err(blockcell_core::Error::Tool(format!(
                                "Dangerous command blocked in skill script: {}",
                                cmd
                            )));
                        }
                    }
                }

                // Security gate: validate filesystem paths are within workspace
                let fs_tools = [
                    "read_file",
                    "write_file",
                    "edit_file",
                    "list_dir",
                    "file_ops",
                ];
                if fs_tools.contains(&tool_name) {
                    let workspace = paths.workspace();
                    for key in &["path", "destination", "output_path"] {
                        if let Some(p) = params.get(*key).and_then(|v| v.as_str()) {
                            let resolved = if std::path::Path::new(p).is_absolute() {
                                std::path::PathBuf::from(p)
                            } else {
                                workspace.join(p)
                            };
                            if !is_path_within_base(&workspace, &resolved) {
                                return Err(blockcell_core::Error::Tool(format!(
                                    "Path '{}' is outside workspace — blocked in skill script",
                                    p
                                )));
                            }
                        }
                    }
                }

                let ctx = blockcell_tools::ToolContext {
                    workspace: paths.workspace(),
                    sandbox_writable_roots: None,
                    base: paths.base.clone(),
                    builtin_skills_dir: Some(paths.builtin_skills_dir()),
                    active_skill_dir: None,
                    session_key: session_key.clone(),
                    channel: channel.clone(),
                    account_id: None,
                    sender_id: None, // Cron jobs have no sender
                    sender_role: None,
                    chat_id: chat_id.clone(),
                    config: config.clone(),
                    permissions: blockcell_core::types::PermissionSet::new(),
                    task_manager: Some(Arc::new(task_manager.clone())),
                    memory_store: memory_store.clone(),
                    memory_file_store: None,
                    ghost_memory_lifecycle: ghost_memory_lifecycle.clone().map(|manager| {
                        manager as Arc<dyn blockcell_tools::GhostMemoryLifecycleOps + Send + Sync>
                    }),
                    skill_file_store: None,
                    session_search: None,
                    outbound_tx: outbound_tx.clone(),
                    spawn_handle: None, // No spawning from cron skill scripts
                    capability_registry: capability_registry.clone(),
                    core_evolution: core_evolution.clone(),
                    event_emitter: Some(event_emitter.clone()),
                    channel_contacts_file: Some(paths.channel_contacts_file()),
                    response_cache: None,
                    runtime_handle: None,
                    agent_identity: None,
                    skill_mutex: None,
                    agent_type_registry: None,
                    evolution_workflow_store: evolution_workflow_store.clone().map(|store| {
                        Arc::new(EvolutionWorkflowStoreAdapter::new((*store).clone()))
                            as blockcell_tools::EvolutionWorkflowStoreHandle
                    }),
                };

                // Execute tool synchronously via a new tokio runtime handle
                let rt = tokio::runtime::Handle::current();
                let tool_name_owned = tool_name.to_string();
                std::thread::scope(|s| {
                    s.spawn(|| {
                        rt.block_on(async { registry.execute(&tool_name_owned, ctx, params).await })
                    })
                    .join()
                    .unwrap_or_else(|_| {
                        Err(blockcell_core::Error::Tool(
                            "Tool execution panicked".into(),
                        ))
                    })
                })
            };

        // Context variables for the legacy compatibility script.
        let mut context_vars = HashMap::new();
        context_vars.insert("skill_name".to_string(), serde_json::json!(skill_name));
        context_vars.insert("trigger".to_string(), serde_json::json!("cron"));

        let invocation = extra_ctx
            .as_ref()
            .and_then(|ctx| ctx.get("invocation"))
            .cloned();

        // Build a `ctx` map so legacy Rhai assets can use `ctx.user_input`, `ctx.channel`, etc.
        let mut ctx_json = serde_json::json!({
            "user_input": msg.content,
            "skill_name": skill_name,
            "trigger": "cron",
            "channel": msg.channel,
            "chat_id": msg.chat_id,
            "message": msg.content,
            "metadata": msg.metadata,
        });
        if let Some(invocation_value) = invocation.clone() {
            context_vars.insert("invocation".to_string(), invocation_value.clone());
            if let Some(ctx_obj) = ctx_json.as_object_mut() {
                ctx_obj.insert("invocation".to_string(), invocation_value);
            }
        }
        context_vars.insert("ctx".to_string(), ctx_json);

        // Execute the compatibility Rhai asset in a blocking task.
        let dispatcher = SkillDispatcher::new();
        let user_input = msg.content.clone();

        let result = tokio::task::spawn_blocking(move || {
            dispatcher.execute_sync(&script, &user_input, context_vars, tool_executor)
        })
        .await
        .map_err(|e| {
            blockcell_core::Error::Skill(format!("Skill execution join error: {}", e))
        })??;

        if result.success {
            // Format output as string
            let output_str = match &result.output {
                serde_json::Value::String(s) => s.clone(),
                other => serde_json::to_string_pretty(other).unwrap_or_default(),
            };
            info!(
                skill = %skill_name,
                tool_calls = result.tool_calls.len(),
                "Legacy Rhai compatibility execution succeeded"
            );
            Ok(output_str)
        } else {
            let err = result.error.unwrap_or_else(|| "Unknown error".to_string());
            warn!(
                skill = %skill_name,
                error = %err,
                "Legacy Rhai compatibility execution failed"
            );
            Err(blockcell_core::Error::Skill(err))
        }
    }
}
