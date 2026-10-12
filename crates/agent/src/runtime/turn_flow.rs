use super::*;

impl AgentRuntime {
    pub(crate) async fn chat_with_provider(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
    ) -> Result<LLMResponse> {
        if let Some((pidx, provider)) = self.provider_pool.acquire() {
            let result = provider.chat(messages, tools).await;
            match &result {
                Ok(_) => self.provider_pool.report(pidx, CallResult::Success),
                Err(e) => self
                    .provider_pool
                    .report(pidx, ProviderPool::classify_error(&format!("{}", e))),
            }
            result
        } else {
            Err(blockcell_core::Error::Config(
                "ProviderPool: no healthy providers".to_string(),
            ))
        }
    }

    pub(crate) fn budget_tracker_for_session(&self, session_key: &str) -> BudgetTrackerHandle {
        let mut trackers = self.budget_trackers.lock().unwrap_or_else(|poisoned| {
            warn!("Budget tracker map mutex poisoned, recovering");
            poisoned.into_inner()
        });
        trackers
            .entry(session_key.to_string())
            .or_insert_with(|| Arc::new(BudgetTracker::new(&self.config.budget)))
            .clone()
    }

    pub(crate) fn record_context_window_estimate(
        &self,
        session_key: &str,
        tokens_used: usize,
        token_limit: usize,
        compact_threshold_ratio: f64,
    ) {
        let compact_threshold_tokens =
            (token_limit as f64 * compact_threshold_ratio.clamp(0.0, 1.0)) as u64;
        self.budget_tracker_for_session(session_key)
            .record_context_window(
                tokens_used as u64,
                token_limit as u64,
                compact_threshold_tokens,
            );
    }

    pub(crate) fn record_llm_budget(
        &self,
        session_key: &str,
        response: &LLMResponse,
    ) -> std::result::Result<(), BudgetExhaustedError> {
        let (input_tokens, output_tokens) = extract_llm_usage_tokens(&response.usage);
        let (cache_read_tokens, cache_creation_tokens) = extract_llm_cache_tokens(&response.usage);
        crate::ghost_metrics::get_ghost_metrics(&self.paths).record_prompt_cache_usage(
            input_tokens,
            cache_read_tokens,
            cache_creation_tokens,
        );
        if input_tokens == 0 && output_tokens == 0 {
            return Ok(());
        }

        let cost_micro_usd = self.estimate_llm_cost_micro_usd(input_tokens, output_tokens);
        let tracker = self.budget_tracker_for_session(session_key);
        let snapshot = tracker.record_usage(input_tokens, output_tokens, cost_micro_usd);

        if tracker.should_warn() {
            warn!(
                session_key,
                usage_ratio = snapshot.usage_ratio,
                tokens_used = snapshot.total_tokens_used,
                cost_used_micro_usd = snapshot.cost_used_micro_usd,
                "Budget warning: usage at {:.0}%",
                snapshot.usage_ratio * 100.0
            );
        }

        tracker.check_budget().map(|_| ())
    }

    pub(crate) fn estimate_llm_cost_micro_usd(&self, input_tokens: u64, output_tokens: u64) -> u64 {
        let defaults = &self.config.agents.defaults;
        let priced_entry = defaults
            .model_pool
            .iter()
            .find(|entry| {
                entry.model == defaults.model
                    && defaults
                        .provider
                        .as_ref()
                        .map(|provider| provider == &entry.provider)
                        .unwrap_or(true)
                    && (entry.input_price.is_some() || entry.output_price.is_some())
            })
            .or_else(|| {
                defaults
                    .model_pool
                    .iter()
                    .find(|entry| entry.input_price.is_some() || entry.output_price.is_some())
            });

        let Some(entry) = priced_entry else {
            return 0;
        };

        let cost_micro_usd = input_tokens as f64 * entry.input_price.unwrap_or(0.0)
            + output_tokens as f64 * entry.output_price.unwrap_or(0.0);
        if cost_micro_usd <= 0.0 || !cost_micro_usd.is_finite() {
            0
        } else {
            cost_micro_usd.ceil() as u64
        }
    }

    /// 带 agent/request、agent/request-error、model/response 事件的模型调用。
    async fn chat_with_provider_observed(
        &self,
        msg: &InboundMessage,
        session_id: &str,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
    ) -> Result<LLMResponse> {
        self.dispatch_event(
            msg,
            session_id,
            &mut AgentEvent::AgentRequest {
                message_count: messages.len(),
                tool_count: tools.len(),
            },
        )
        .await;
        let result = self.chat_with_provider(messages, tools).await;
        let mut event = match &result {
            Ok(response) => AgentEvent::ModelResponse {
                content: response.content.clone().unwrap_or_default(),
                tool_calls: serde_json::to_value(&response.tool_calls).unwrap_or_default(),
                finish_reason: response.finish_reason.clone(),
            },
            Err(error) => request_error_event(error),
        };
        self.dispatch_event(msg, session_id, &mut event).await;
        result
    }

    /// 不带工具的收尾模型调用（强制最终回答、子代理结果汇总）作为独立 step 记录，
    /// 保证 model/request、model/response 落在某个 step 内，事件日志可完整重放。
    pub(crate) async fn run_final_model_step(
        &mut self,
        msg: &InboundMessage,
        session_id: &str,
        messages: &[ChatMessage],
    ) -> Result<LLMResponse> {
        let step = self.begin_step();
        self.dispatch_event(
            msg,
            session_id,
            &mut AgentEvent::StepStart { iteration: step },
        )
        .await;
        let result = self
            .chat_with_provider_observed(msg, session_id, messages, &[])
            .await;
        self.end_step(msg, session_id, step, 0).await;
        result
    }

    pub(crate) async fn run_prompt_skill_loop(
        &mut self,
        msg: &InboundMessage,
        messages: Vec<ChatMessage>,
        tools: Vec<serde_json::Value>,
        tool_names: &[String],
        active_skill_dir: Option<PathBuf>,
    ) -> Result<PromptSkillLoopOutput> {
        // 技能循环与主循环共用 turn 内单调递增的 step 编号；结束后恢复外层 step 为当前 step。
        let outer_step = self.current_step;
        let result = Box::pin(self.run_prompt_skill_steps(
            msg,
            messages,
            tools,
            tool_names,
            active_skill_dir,
        ))
        .await;
        self.current_step = outer_step;
        result
    }

    async fn run_prompt_skill_steps(
        &mut self,
        msg: &InboundMessage,
        messages: Vec<ChatMessage>,
        tools: Vec<serde_json::Value>,
        tool_names: &[String],
        active_skill_dir: Option<PathBuf>,
    ) -> Result<PromptSkillLoopOutput> {
        let session_id = persist_session_key_for(msg);
        let allowed_tool_names = tool_names.iter().cloned().collect::<HashSet<_>>();
        let max_iterations = self.config.agents.defaults.max_tool_iterations.max(1);
        let tools_max_iterations = self
            .config
            .agents
            .defaults
            .max_tool_iterations_by_tool
            .clone();
        let mut tool_call_counts: HashMap<String, u32> = HashMap::new();
        let mut consecutive_failure_tracker = ConsecutiveToolFailureTracker::default();
        let mut over_iteration: bool = false;
        let mut current_messages = messages;
        let mut trace_messages = Vec::new();

        let final_response = loop {
            let step = self.begin_step();
            self.dispatch_event(
                msg,
                &session_id,
                &mut AgentEvent::AgentPreStep { iteration: step },
            )
            .await;
            self.dispatch_event(
                msg,
                &session_id,
                &mut AgentEvent::StepStart { iteration: step },
            )
            .await;
            if let Some(plan) = blockcell_tools::plan::take_changed_plan_context(&msg.session_key())
            {
                current_messages.push(ChatMessage::user(&format!(
                    "<runtime-context>\n{}\n</runtime-context>",
                    plan
                )));
            }
            if let Some(environment) = self
                .context_builder
                .incremental_project_environment(&msg.session_key())
            {
                current_messages.push(ChatMessage::user(&format!(
                    "<runtime-context>\n{}\n</runtime-context>",
                    environment
                )));
            }
            let response = match self
                .chat_with_provider_observed(msg, &session_id, &current_messages, &tools)
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    self.end_step(msg, &session_id, step, 0).await;
                    return Err(error);
                }
            };
            let tool_call_count = response.tool_calls.len();

            if response.tool_calls.is_empty() {
                self.dispatch_event(
                    msg,
                    &session_id,
                    &mut AgentEvent::StepEnd {
                        iteration: step,
                        tool_calls: 0,
                    },
                )
                .await;
                break response.content.unwrap_or_default();
            }

            let assistant_tool_call = ChatMessage {
                id: None,
                role: "assistant".to_string(),
                content: serde_json::Value::String(response.content.unwrap_or_default()),
                reasoning_content: response.reasoning_content.clone(),
                tool_calls: Some(response.tool_calls.clone()),
                tool_call_id: None,
                name: None,
            };
            current_messages.push(assistant_tool_call.clone());
            trace_messages.push(assistant_tool_call);

            for tool_call in response.tool_calls {
                let tool_result =
                    if crate::prompt_skill_executor::PromptSkillExecutor::is_tool_allowed(
                        &tool_call.name,
                        &allowed_tool_names,
                    ) {
                        let max_iterations = tools_max_iterations
                            .get(&tool_call.name)
                            .copied()
                            .unwrap_or(max_iterations);
                        let count = tool_call_counts.entry(tool_call.name.clone()).or_insert(0);

                        *count += 1;
                        if *count > max_iterations {
                            over_iteration = true;
                            let denied = serde_json::json!({
                                "error": format!(
                                    "Tool '{}' execeeded max call limit ({}).",
                                    tool_call.name, max_iterations
                                ),
                                "tool": tool_call.name,
                                "hint": "Reduce repeated tool calls or adjust maxToolIterationsByTool."
                            })
                            .to_string();
                            self.record_unexecuted_tool_call(
                                &tool_call,
                                msg,
                                ToolOutcome::Rejected,
                                &denied,
                            )
                            .await;
                            denied
                        } else {
                            self.execute_tool_call(&tool_call, msg, active_skill_dir.clone())
                                .await
                        }
                    } else {
                        let denied = serde_json::json!({
                            "error": format!(
                                "Tool '{}' is not available inside prompt skill scope.",
                                tool_call.name
                            ),
                            "tool": tool_call.name,
                            "hint": "Use only the tools declared by the active skill."
                        })
                        .to_string();
                        self.record_unexecuted_tool_call(
                            &tool_call,
                            msg,
                            ToolOutcome::Rejected,
                            &denied,
                        )
                        .await;
                        denied
                    };
                if consecutive_failure_tracker
                    .record(&tool_call, tool_result_indicates_error(&tool_result))
                {
                    over_iteration = true;
                    warn!(
                        tool = %tool_call.name,
                        "Stopping prompt skill loop after three identical tool-call failures"
                    );
                }
                let mut tool_message = ChatMessage::tool_result(&tool_call.id, &tool_result);
                tool_message.name = Some(tool_call.name.clone());
                current_messages.push(tool_message.clone());
                trace_messages.push(tool_message);
            }
            self.dispatch_event(
                msg,
                &session_id,
                &mut AgentEvent::StepEnd {
                    iteration: step,
                    tool_calls: tool_call_count,
                },
            )
            .await;

            if over_iteration {
                let mut final_messages = current_messages.clone();
                final_messages.push(ChatMessage::user(
                    "请基于以上技能上下文和工具结果，直接给出最终答案。不要再调用任何工具。",
                ));
                let final_response = self
                    .run_final_model_step(msg, &session_id, &final_messages)
                    .await?
                    .content
                    .unwrap_or_default();
                break final_response;
            }
        };

        let final_response = strip_fake_tool_calls(final_response.trim());
        Ok(PromptSkillLoopOutput {
            final_response: final_response.trim().to_string(),
            trace_messages,
        })
    }

    pub(crate) fn last_local_exec_tool_name(trace_messages: &[ChatMessage]) -> Option<String> {
        for message in trace_messages.iter().rev() {
            if let Some(name) = message.name.as_deref() {
                if matches!(name, "exec_skill_script" | "exec_local") {
                    return Some(name.to_string());
                }
            }

            if let Some(tool_calls) = message.tool_calls.as_ref() {
                for call in tool_calls.iter().rev() {
                    if matches!(call.name.as_str(), "exec_skill_script" | "exec_local") {
                        return Some(call.name.clone());
                    }
                }
            }
        }

        None
    }

    pub(crate) async fn decide_interaction(
        &mut self,
        msg: &InboundMessage,
        disabled_skills: &HashSet<String>,
        classifier: &crate::intent::IntentClassifier,
        history: &[ChatMessage],
        session_metadata: &serde_json::Value,
    ) -> Result<InteractionDecision> {
        let forced_skill_name = msg
            .metadata
            .get("forced_skill_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let chat_intents = classifier.classify(&msg.content);
        let session_skill_name = continued_skill_name(session_metadata, history);

        if !forced_skill_name.is_empty() {
            let active_skill = self
                .context_builder
                .resolve_active_skill_by_name(forced_skill_name, disabled_skills)
                .map(|skill| {
                    suppress_prompt_reinjection_for_continued_skill(
                        skill,
                        session_skill_name.as_deref(),
                    )
                })
                .ok_or_else(|| {
                    blockcell_core::Error::Skill(format!(
                        "Forced skill '{}' is not available",
                        forced_skill_name
                    ))
                })?;

            info!(
                mode = ?InteractionMode::Skill,
                active_skill = %active_skill.name,
                "Interaction mode resolved from forced skill"
            );

            return Ok(InteractionDecision {
                active_skill: Some(active_skill),
                chat_intents,
                mode: InteractionMode::Skill,
            });
        }

        let mode = if chat_intents.contains(&IntentCategory::Coding) {
            InteractionMode::Coding
        } else {
            InteractionMode::General
        };
        info!(
            mode = ?mode,
            intents = ?chat_intents,
            recent_skill = session_skill_name.as_deref(),
            "Interaction mode resolved from unified entry"
        );
        Ok(InteractionDecision {
            active_skill: None,
            chat_intents,
            mode,
        })
    }

    pub(crate) async fn execute_decided_skill_route(
        &mut self,
        decision: &InteractionDecision,
        msg: &InboundMessage,
        persist_session_key: &str,
    ) -> Option<Result<String>> {
        if !matches!(decision.mode, InteractionMode::Skill) {
            return None;
        }

        let skill_ctx = decision.active_skill.as_ref()?.clone();
        info!(
            skill = %skill_ctx.name,
            "Skill matched — entering unified skill executor"
        );
        Some(
            self.execute_skill_for_user(&skill_ctx, msg, persist_session_key)
                .await
                .map(|result| result.final_response),
        )
    }

    pub(crate) async fn execute_skill_for_user(
        &mut self,
        active_skill: &ActiveSkillContext,
        msg: &InboundMessage,
        persist_session_key: &str,
    ) -> Result<SkillExecutionResult> {
        // Layer 4: Track skill activation for Post-Compact recovery
        // 在技能执行入口处追踪，覆盖手动激活和意图路由自动加载
        if let Some(memory_system) = self.memory_system.as_mut() {
            memory_system.record_skill_load(&active_skill.name, &active_skill.prompt_md);
            debug!(skill_name = %active_skill.name, "[layer4] Tracked skill activation for recovery (auto-routed or manual)");
        }
        self.dispatch_event(
            msg,
            persist_session_key,
            &mut AgentEvent::SkillActivated {
                name: active_skill.name.clone(),
            },
        )
        .await;

        let history = self.session_store.load(persist_session_key)?;
        let (result, mut session_metadata, allowed_tools) = self
            .run_skill_for_turn(active_skill, msg, &history, persist_session_key)
            .await?;
        record_active_skill_name(&mut session_metadata, &active_skill.name);
        let mut updated_history = history;
        persist_prompt_skill_history(
            &mut updated_history,
            &msg.content,
            &active_skill.name,
            &allowed_tools,
            &result.trace_messages,
            &result.final_response,
        );
        self.end_turn(
            msg,
            persist_session_key,
            result.final_response.clone(),
            None,
        )
        .await;
        self.session_store.save_with_metadata(
            persist_session_key,
            &updated_history,
            &session_metadata,
        )?;
        self.deliver_skill_response(msg, &result.final_response, Some("skill"))
            .await;

        Ok(result)
    }

    pub(crate) fn resolved_skill_tool_names(
        &self,
        active_skill: &ActiveSkillContext,
    ) -> Vec<String> {
        let available_tools = self
            .tool_registry
            .model_visible_tool_names()
            .into_iter()
            .collect::<HashSet<_>>();
        let mut declared_tools = active_skill.tools.clone();
        if self
            .context_builder
            .skill_manager()
            .and_then(|manager| manager.get(&active_skill.name))
            .map(blockcell_skills::SkillManager::build_skill_card)
            .is_some_and(|card| card.supports_local_exec)
        {
            declared_tools.push("exec_skill_script".to_string());
            declared_tools.push("exec_local".to_string());
        }
        crate::prompt_skill_executor::PromptSkillExecutor::resolve_allowed_tool_names(
            &declared_tools,
            &available_tools,
        )
    }

    pub(crate) async fn run_skill_for_turn(
        &mut self,
        active_skill: &ActiveSkillContext,
        msg: &InboundMessage,
        history: &[ChatMessage],
        session_key: &str,
    ) -> Result<(SkillExecutionResult, serde_json::Value, Vec<String>)> {
        let manual_mode = determine_manual_load_mode(&active_skill.name, history);
        info!(
            skill = %active_skill.name,
            manual_mode = ?manual_mode,
            "Unified skill executor starting"
        );

        let mut prompt_skill = active_skill.clone();
        prompt_skill.inject_prompt_md =
            prompt_skill.inject_prompt_md && manual_mode.should_load_manual();

        let allowed_tools = self.resolved_skill_tool_names(&prompt_skill);
        let (final_response, trace_messages, session_metadata) = self
            .run_prompt_skill_for_session(&prompt_skill, msg, history, session_key, &allowed_tools)
            .await?;

        Ok((
            SkillExecutionResult {
                final_response,
                trace_messages,
            },
            session_metadata,
            allowed_tools,
        ))
    }

    pub(crate) async fn persist_and_deliver_final_response(
        &mut self,
        ctx: FinalResponseContext<'_>,
    ) -> Result<String> {
        let FinalResponseContext {
            msg,
            persist_session_key,
            history,
            session_metadata,
            final_response,
            collected_media,
            cron_deliver_target,
        } = ctx;
        let final_response = strip_fake_tool_calls(final_response.trim());
        info!(target: "chat::output", content = %final_response, "Final response");

        // Extract reasoning_content from the last assistant message in history
        // (populated by DeepSeek thinking mode etc.) so channels can display it.
        let reasoning_content = history
            .iter()
            .rev()
            .find(|m| m.role == "assistant")
            .and_then(|m| m.reasoning_content.clone())
            .filter(|r| !r.is_empty());

        // Only cache if this turn had substantive tool results — prevents caching
        // LLM-hallucinated lists from empty/error tool results.
        // A tool message with empty/null content (e.g. memory_query returning [])
        // should not qualify as "real" data backing the assistant's list.
        // 注意：只扫描当前 turn（最后一条 user 消息之后）的 tool 结果，
        // 而非整个历史，避免曾经的工具调用导致后续纯文本回复被错误缓存
        let current_turn_start = history
            .iter()
            .rposition(|m| m.role == "user")
            .map(|pos| pos + 1)
            .unwrap_or(0);
        let has_tool_results = history[current_turn_start..].iter().any(|m| {
            m.role == "tool"
                && match &m.content {
                    serde_json::Value::String(s) => {
                        !s.is_empty() && s != "[]" && !s.starts_with("{\"error\"")
                    }
                    serde_json::Value::Null => false,
                    _ => true,
                }
        });
        if let Some(stub) = self.response_cache.maybe_cache_and_stub(
            persist_session_key,
            &final_response,
            has_tool_results,
        ) {
            overwrite_last_assistant_message(history, &stub);
        }

        self.end_turn(msg, persist_session_key, final_response.clone(), None)
            .await;

        self.session_store
            .save_with_metadata(persist_session_key, history, session_metadata)?;

        if history.len() >= 6 {
            if let Some(ref store) = self.memory_store {
                let summary = Self::build_extractive_summary(history);
                if !summary.is_empty() {
                    if let Err(e) = store.upsert_session_summary(persist_session_key, &summary) {
                        debug!(error = %e, "Failed to upsert session summary");
                    }
                }
            }
        }

        if msg.channel == "cron"
            && msg
                .metadata
                .get("cron_agent")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        {
            if let Some(tx) = &self.outbound_tx {
                let mut outbound =
                    OutboundMessage::new(&msg.channel, &msg.chat_id, &final_response);
                outbound.account_id = msg.account_id.clone();
                outbound.reasoning_content = reasoning_content.clone();
                outbound.media = collected_media.clone();
                outbound.metadata = extract_reply_metadata(msg);
                let _ = tx.send(outbound).await;
            }

            if let Some((channel, to)) = cron_deliver_target {
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
                            "media": collected_media,
                            "background_delivery": true,
                            "delivery_kind": "cron",
                            "cron_kind": "agent",
                        });
                        if let Some(ref rc) = reasoning_content {
                            event["reasoning_content"] = serde_json::Value::String(rc.clone());
                        }
                        let _ = event_tx.send(event.to_string());
                    }
                    if let Some(tx) = &self.outbound_tx {
                        let mut outbound = OutboundMessage::new(&channel, &to, &final_response);
                        outbound.account_id = msg.account_id.clone();
                        outbound.reasoning_content = reasoning_content.clone();
                        outbound.media = collected_media.clone();
                        let _ = tx.send(outbound).await;
                    }
                } else if let Some(tx) = &self.outbound_tx {
                    let mut outbound = OutboundMessage::new(&channel, &to, &final_response);
                    outbound.account_id = msg.account_id.clone();
                    outbound.reasoning_content = reasoning_content.clone();
                    outbound.media = collected_media.clone();
                    let _ = tx.send(outbound).await;
                }
            }

            return Ok(final_response.to_string());
        }

        if msg.channel == "ws" || msg.channel == "cli" {
            if let Some(ref event_tx) = self.event_tx {
                let mut event = serde_json::json!({
                    "type": "message_done",
                    "channel": msg.channel,
                    "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                    "chat_id": msg.chat_id,
                    "task_id": "",
                    "content": final_response,
                    "tool_calls": 0,
                    "duration_ms": 0,
                    "media": collected_media,
                });
                if let Some(ref rc) = reasoning_content {
                    event["reasoning_content"] = serde_json::Value::String(rc.clone());
                }
                let _ = event_tx.send(event.to_string());
            }
        }

        if msg.channel != "ghost" {
            if let Some(tx) = &self.outbound_tx {
                let mut outbound =
                    OutboundMessage::new(&msg.channel, &msg.chat_id, &final_response);
                outbound.account_id = msg.account_id.clone();
                outbound.reasoning_content = reasoning_content.clone();
                outbound.media = collected_media.clone();
                outbound.metadata = extract_reply_metadata(msg);
                // The runtime already sent message_done via event_tx for ws channel;
                // tell the bridge not to echo it back as a second message_done.
                outbound.skip_ws_echo = true;
                let _ = tx.send(outbound).await;
            }
        }

        if msg.channel == "cron" {
            if let Some(deliver) = msg.metadata.get("deliver").and_then(|v| v.as_bool()) {
                if deliver {
                    if let (Some(channel), Some(to)) = (
                        msg.metadata.get("deliver_channel").and_then(|v| v.as_str()),
                        msg.metadata.get("deliver_to").and_then(|v| v.as_str()),
                    ) {
                        if let Some(tx) = &self.outbound_tx {
                            let mut outbound = OutboundMessage::new(channel, to, &final_response);
                            outbound.reasoning_content = reasoning_content.clone();
                            let _ = tx.send(outbound).await;
                        }
                    }
                }
            }
        }

        Ok(final_response.to_string())
    }
}
