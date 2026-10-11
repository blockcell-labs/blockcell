use super::*;

impl AgentRuntime {
    /// Extracted sub-function (#15): Call LLM with streaming and retry on transient errors.
    /// Returns the LLM response on success, or the last error on exhaustion.
    pub(crate) async fn call_llm_with_retry(
        &mut self,
        current_messages: &[ChatMessage],
        tools: &[serde_json::Value],
        msg: &InboundMessage,
        ghost_recall_context_block: Option<&str>,
        iteration: &HashMap<String, u32>,
        saw_rate_limit_this_turn: &mut bool,
    ) -> std::result::Result<LLMResponse, blockcell_core::Error> {
        let max_retries = self.config.agents.defaults.llm_max_retries;
        let base_delay_ms = self.config.agents.defaults.llm_retry_delay_ms;
        let mut last_error = None;
        let mut last_failure: Option<FailureKind> = None;
        let api_messages = append_ephemeral_context_to_latest_user_message(
            current_messages,
            ghost_recall_context_block,
        );
        let routing_context = RoutingContext {
            message_count: api_messages.len(),
            estimated_tokens: estimate_messages_tokens(&api_messages),
            intent: msg
                .metadata
                .get("intent")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        };
        // Computed once instead of re-cloning the Option<String> on every streamed delta.
        let agent_id = self
            .agent_id
            .clone()
            .unwrap_or_else(|| "default".to_string());

        for attempt in 0..=max_retries {
            if attempt > 0 {
                let delay_ms = base_delay_ms * (1u64 << (attempt - 1).min(4));
                warn!(
                    attempt,
                    max_retries,
                    delay_ms,
                    ?iteration,
                    "Retrying LLM call after transient error"
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            let mut excluded_provider_indices = Vec::new();

            loop {
                let (pool_idx, provider) = match self.provider_pool.acquire_with_strategy_excluding(
                    self.config.agents.defaults.routing_strategy,
                    &routing_context,
                    &excluded_provider_indices,
                ) {
                    Some(p) => p,
                    None => {
                        if last_error.is_none() {
                            last_error = Some(blockcell_core::Error::Config(
                                "ProviderPool: no healthy providers available".to_string(),
                            ));
                        }
                        break;
                    }
                };

                match provider.chat_stream(&api_messages, tools).await {
                    Ok(mut stream_rx) => {
                        if attempt > 0 {
                            info!(
                                attempt,
                                ?iteration,
                                pool_idx,
                                "LLM stream call succeeded after retry"
                            );
                        }
                        let mut accumulated_content = String::new();
                        let mut accumulated_reasoning = String::new();
                        let mut tool_call_accumulators: HashMap<String, ToolCallAccumulator> =
                            HashMap::new();
                        let mut emitted_text_delta = false;
                        let mut stream_error: Option<blockcell_core::Error> = None;

                        const STREAM_TIMEOUT_SECS: u64 = 300;

                        loop {
                            let recv_result = tokio::time::timeout(
                                std::time::Duration::from_secs(STREAM_TIMEOUT_SECS),
                                stream_rx.recv(),
                            )
                            .await;

                            match recv_result {
                                Ok(Some(chunk)) => match chunk {
                                    StreamChunk::TextDelta { delta } => {
                                        accumulated_content.push_str(&delta);
                                        emitted_text_delta = true;
                                        if let Some(ref event_tx) = self.event_tx {
                                            let event = serde_json::json!({
                                                "type": "token",
                                                "channel": msg.channel,
                                                "agent_id": agent_id.as_str(),
                                                "chat_id": msg.chat_id.clone(),
                                                "delta": delta,
                                            });
                                            let _ = event_tx.send(event.to_string());
                                        }
                                    }
                                    StreamChunk::ReasoningDelta { delta } => {
                                        accumulated_reasoning.push_str(&delta);
                                        if let Some(ref event_tx) = self.event_tx {
                                            let event = serde_json::json!({
                                                "type": "thinking",
                                                "channel": msg.channel,
                                                "agent_id": agent_id.as_str(),
                                                "chat_id": msg.chat_id.clone(),
                                                "content": delta,
                                            });
                                            let _ = event_tx.send(event.to_string());
                                        }
                                    }
                                    StreamChunk::ToolCallStart { index: _, id, name } => {
                                        let acc =
                                            tool_call_accumulators.entry(id.clone()).or_default();
                                        acc.id = id.clone();
                                        acc.name = name.clone();
                                    }
                                    StreamChunk::ToolCallDelta {
                                        index: _,
                                        id,
                                        delta,
                                    } => {
                                        if let Some(acc) = tool_call_accumulators.get_mut(&id) {
                                            acc.arguments.push_str(&delta);
                                        }
                                    }
                                    StreamChunk::Done { response } => {
                                        let final_tool_calls = if !tool_call_accumulators.is_empty()
                                        {
                                            tool_call_accumulators
                                                .drain()
                                                .map(|(_, acc)| acc.to_tool_call_request())
                                                .collect()
                                        } else {
                                            response.tool_calls.clone()
                                        };

                                        let final_content = if !accumulated_content.is_empty() {
                                            Some(accumulated_content.clone())
                                        } else {
                                            response.content.clone()
                                        };

                                        let final_reasoning = if !accumulated_reasoning.is_empty() {
                                            Some(accumulated_reasoning.clone())
                                        } else {
                                            response.reasoning_content.clone()
                                        };

                                        self.provider_pool.report(pool_idx, CallResult::Success);
                                        return Ok(LLMResponse {
                                            content: final_content,
                                            reasoning_content: final_reasoning,
                                            tool_calls: final_tool_calls,
                                            finish_reason: response.finish_reason.clone(),
                                            usage: response.usage.clone(),
                                        });
                                    }
                                    StreamChunk::Error { message } => {
                                        warn!(error = %message, "Stream error");
                                        stream_error =
                                            Some(blockcell_core::Error::Provider(message));
                                        break;
                                    }
                                },
                                Ok(None) => {
                                    break;
                                }
                                Err(_) => {
                                    warn!(
                                        "Stream receive timeout after {} seconds",
                                        STREAM_TIMEOUT_SECS
                                    );
                                    stream_error = Some(blockcell_core::Error::Provider(format!(
                                        "Stream timeout after {} seconds",
                                        STREAM_TIMEOUT_SECS
                                    )));
                                    break;
                                }
                            }
                        }

                        // Fallback: tolerate providers that close the stream cleanly without an
                        // explicit Done event. If the stream ended with an error, retry instead of
                        // committing a partial answer.
                        if stream_error.is_none()
                            && (!tool_call_accumulators.is_empty()
                                || !accumulated_content.is_empty())
                        {
                            self.provider_pool.report(pool_idx, CallResult::Success);
                            let final_tool_calls: Vec<ToolCallRequest> = tool_call_accumulators
                                .into_values()
                                .map(|acc| acc.to_tool_call_request())
                                .collect();

                            return Ok(LLMResponse {
                                content: if accumulated_content.is_empty() {
                                    None
                                } else {
                                    Some(accumulated_content)
                                },
                                reasoning_content: if accumulated_reasoning.is_empty() {
                                    None
                                } else {
                                    Some(accumulated_reasoning)
                                },
                                tool_calls: final_tool_calls,
                                finish_reason: "stop".to_string(),
                                usage: serde_json::Value::Null,
                            });
                        }

                        if emitted_text_delta {
                            if let Some(ref event_tx) = self.event_tx {
                                let event = serde_json::json!({
                                    "type": "stream_reset",
                                    "channel": msg.channel,
                                    "agent_id": agent_id.as_str(),
                                    "chat_id": msg.chat_id.clone(),
                                });
                                let _ = event_tx.send(event.to_string());
                            }
                        }

                        let err = stream_error.unwrap_or_else(|| {
                            blockcell_core::Error::Provider(
                                "Stream ended unexpectedly before completion".to_string(),
                            )
                        });
                        let err_str = format!("{}", err);
                        if !accumulated_content.is_empty() || !tool_call_accumulators.is_empty() {
                            self.dispatch_event(
                                msg,
                                &persist_session_key_for(msg),
                                &mut AgentEvent::AssistantAttempt {
                                    content: accumulated_content,
                                    tool_calls: tool_call_accumulators
                                        .into_values()
                                        .map(|acc| acc.name)
                                        .collect(),
                                    error: err_str.clone(),
                                },
                            )
                            .await;
                        }
                        let call_result = ProviderPool::classify_error(&err_str);
                        if matches!(&call_result, CallResult::RateLimit) {
                            *saw_rate_limit_this_turn = true;
                        }
                        self.provider_pool.report(pool_idx, call_result);
                        last_failure = Some(FailureKind::classify_provider_message(&err_str));
                        last_error = Some(err);
                        break;
                    }
                    Err(e) => {
                        let err_str = format!("{}", e);
                        warn!(error = %err_str, attempt, max_retries, ?iteration, pool_idx, "LLM stream call failed");
                        let call_result = ProviderPool::classify_error(&err_str);
                        if matches!(&call_result, CallResult::RateLimit) {
                            *saw_rate_limit_this_turn = true;
                        }
                        self.provider_pool.report(pool_idx, call_result);
                        let failure = FailureKind::classify_provider_message(&err_str);
                        last_failure = Some(failure);
                        // 认证失败只说明当前 provider 不可用：换下一个 provider，而不是重试同一个。
                        if failure == FailureKind::AuthenticationFailed
                            || is_connection_phase_error(&err_str)
                        {
                            excluded_provider_indices.push(pool_idx);
                            warn!(
                                attempt,
                                max_retries,
                                ?iteration,
                                pool_idx,
                                error = %err_str,
                                "Connection-phase LLM failure; trying fallback provider"
                            );
                            last_error = Some(e);
                            continue;
                        }
                        last_error = Some(e);
                        break;
                    }
                }
            }
            // 模型调用本身无副作用（Idempotency::Safe），但上下文超限、认证失败、取消重试也不会成功。
            if let Some(failure) =
                last_failure.filter(|kind| !kind.retry_allowed(Idempotency::Safe))
            {
                warn!(
                    failure = failure.as_str(),
                    attempt, "LLM failure is not retryable; giving up"
                );
                break;
            }
        }

        Err(last_error.unwrap_or_else(|| {
            blockcell_core::Error::Provider("LLM call failed with no error details".to_string())
        }))
    }

    pub async fn process_message(&mut self, msg: InboundMessage) -> Result<String> {
        // Wrap execution in AbortToken + AgentIdentity context so:
        // - forked sub-agents inherit cancellation via current_abort_token().child()
        // - the agent tool can check can_spawn_subagent() via current_agent_context()
        let abort_token = self.abort_token.clone();
        let agent_id = self
            .agent_id
            .clone()
            .unwrap_or_else(|| "default".to_string());
        let identity = AgentIdentity::lead(agent_id, "lead".to_string());
        let agent_loop = Arc::clone(&self.agent_loop);
        scope_agent_context(identity, async move {
            let turn_msg = msg.clone();
            let result = scope_abort_token(abort_token, agent_loop.run(self, msg)).await;
            // 无论 loop 实现如何退出（含错误提前返回），已开始的回合都必须以 turn/end 关闭。
            if self.current_turn_id.is_some() {
                let (final_response, error) = match &result {
                    Ok(reply) => (reply.clone(), None),
                    Err(error) => (String::new(), Some(error.to_string())),
                };
                self.end_turn(
                    &turn_msg,
                    &persist_session_key_for(&turn_msg),
                    final_response,
                    error,
                )
                .await;
            }
            result
        })
        .await
    }

    pub(crate) fn drain_steering_messages(
        &mut self,
        current_messages: &mut Vec<ChatMessage>,
        history: &mut Vec<ChatMessage>,
        active_msg: &InboundMessage,
    ) -> usize {
        let mut injected = 0;
        for steer_msg in self.steering.drain() {
            if steer_msg.content.trim().is_empty() {
                continue;
            }
            info!(
                channel = %steer_msg.channel,
                chat_id = %steer_msg.chat_id,
                "Injecting steering message into active agent loop"
            );
            let chat_message = ChatMessage::user(&steer_msg.content);
            current_messages.push(chat_message.clone());
            history.push(chat_message);
            injected += 1;

            if let Some(event_tx) = &self.event_tx {
                let _ = event_tx.send(
                    serde_json::json!({
                        "type": "steering_received",
                        "channel": active_msg.channel,
                        "agent_id": self.agent_id.clone().unwrap_or_else(|| "default".to_string()),
                        "chat_id": active_msg.chat_id,
                    })
                    .to_string(),
                );
            }
        }
        injected
    }
}
