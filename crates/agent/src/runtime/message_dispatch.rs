use super::*;
use rand::Rng;

/// 连接层失败在 `llmMaxRetries` 之上额外允许的尝试次数。
///
/// 上界必须确定：provider 彻底不可用时，重试次数不能无限增长。
const TRANSPORT_EXTRA_ATTEMPTS: u32 = 3;

// Runtime policy comes from config; the previous values remain its defaults.
fn retry_delay_ms(
    attempt: u32,
    base_delay_ms: u64,
    backoff_so_far_ms: u64,
    retry_after_ms: Option<u64>,
    jitter: f64,
    total_cap_ms: u64,
    per_attempt_cap_ms: u64,
) -> u64 {
    let remaining_ms = total_cap_ms.saturating_sub(backoff_so_far_ms);
    let multiplier = 1_u64
        .checked_shl(attempt.saturating_sub(1).min(63))
        .unwrap_or(u64::MAX);
    // Retry-After is a minimum server wait, not an exponential-backoff hint.
    // The caller stops if the remaining budget cannot accommodate it.
    if let Some(delay_ms) = retry_after_ms {
        return delay_ms;
    }
    let upper_bound_ms = base_delay_ms
        .saturating_mul(multiplier)
        .min(per_attempt_cap_ms)
        .min(remaining_ms);
    (upper_bound_ms as f64 * jitter.clamp(0.0, 1.0)) as u64
}

/// 连接层失败（DNS/TCP/TLS/连接被重置）由 provider 侧统一标记为 `code="transport"`。
///
/// 它与限流一样属于「等一会儿可能恢复」，但单次尝试本身就可能耗时数十秒
/// （2026-10-30 七牛评测实测：单次请求排队 40–90 s），用普通瞬时错误的 60 s
/// 退避预算会在 6 次内耗尽，导致已经拿到答案的一轮被直接判死。
/// 只认 `transport` 而不认整个 `ProviderUnavailable`，是为了让 content_filter
/// 之类「重试也没用」的失败仍然快速收手。
fn is_transport_failure(error: &blockcell_core::Error) -> bool {
    matches!(
        error,
        blockcell_core::Error::ProviderFailure(failure)
            if failure.code.as_deref() == Some("transport")
    )
}

fn retry_after_delay_ms(error: &blockcell_core::Error) -> Option<u64> {
    if let blockcell_core::Error::ProviderFailure(failure) = error {
        return failure.retry_after_ms;
    }
    // Compatibility for external adapters that still report formatted errors.
    let message = error.to_string();
    let value = message.split_once("retry_after=")?.1;
    let value = value.split(')').next().unwrap_or(value).trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds.saturating_mul(1000));
    }

    let retry_at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    Some(
        retry_at
            .signed_duration_since(chrono::Utc::now())
            .num_milliseconds()
            .max(0) as u64,
    )
}

// Stream indices define execution order. Prefer complete provider records when
// available: they may contain metadata (for example Gemini thought signatures)
// that cannot be represented by ToolCallStart/ToolCallDelta.
fn merge_stream_tool_calls(
    accumulators: HashMap<usize, ToolCallAccumulator>,
    mut completed: Vec<ToolCallRequest>,
) -> Vec<ToolCallRequest> {
    let mut indexed: Vec<_> = accumulators.into_iter().collect();
    indexed.sort_by_key(|(index, _)| *index);
    let mut calls = Vec::new();
    for (_, acc) in indexed {
        if let Some(position) = completed
            .iter()
            .position(|call| !acc.id.is_empty() && call.id == acc.id)
        {
            calls.push(completed.remove(position));
        } else if !acc.id.is_empty() && !acc.name.is_empty() {
            calls.push(acc.to_tool_call_request());
        }
    }
    calls.extend(completed);
    calls
}

impl AgentRuntime {
    /// Extracted sub-function (#15): Call LLM with streaming and retry on transient errors.
    /// Returns the LLM response on success, or the last error on exhaustion.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn call_llm_with_retry(
        &mut self,
        current_messages: &[ChatMessage],
        tools: &[serde_json::Value],
        msg: &InboundMessage,
        ghost_recall_context_block: Option<&str>,
        iteration: &HashMap<String, u32>,
        saw_rate_limit_this_turn: &mut bool,
        requests_sent: &mut u32,
    ) -> std::result::Result<(LLMResponse, ServedBy), blockcell_core::Error> {
        let retry_policy = self.config.agents.defaults.llm_retry_policy.clone();
        let request_started = std::time::Instant::now();
        let remaining_deadline = || {
            retry_policy.deadline_ms.map(|limit| {
                std::time::Duration::from_millis(limit).saturating_sub(request_started.elapsed())
            })
        };
        let max_retries = self.config.agents.defaults.llm_max_retries;
        // 限流放宽尝试次数：免费网关的 429 是"等一会儿就好"，不该按普通瞬时错误的
        // 次数上限直接判死。非限流失败仍然只走 `llmMaxRetries`。
        let max_attempts = max_retries.max(retry_policy.rate_limit_max_retries);
        let base_delay_ms = self.config.agents.defaults.llm_retry_delay_ms;
        let mut last_error = None;
        let mut last_failure: Option<FailureKind> = None;
        let mut backoff_so_far_ms = 0;
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

        for attempt in 0..=max_attempts {
            // 只有限流与连接层失败允许超过常规次数上限；其余失败按原策略及时收手。
            // 连接层失败额外给 `TRANSPORT_EXTRA_ATTEMPTS` 次，避免 provider 短暂抖动
            // 直接判死，同时给「重试到底」设一个确定上界。
            let transport_failure = last_error.as_ref().is_some_and(is_transport_failure);
            let may_exceed_base = matches!(last_failure, Some(FailureKind::RateLimited))
                || (transport_failure && attempt <= max_retries + TRANSPORT_EXTRA_ATTEMPTS);
            if attempt > max_retries && !may_exceed_base {
                break;
            }
            if attempt > 0 {
                let retry_after_ms = last_error.as_ref().and_then(retry_after_delay_ms);
                // 限流与普通瞬时错误分开给退避预算：429 通常几秒内恢复，但密集时需要
                // 更长的总等待窗口；无 Retry-After 时单次指数退避最多 30s。
                // 连接层失败沿用限流档位：单次尝试本身可能耗时数十秒，60s 预算不够。
                let rate_limited = matches!(last_failure, Some(FailureKind::RateLimited));
                let slow_recovery = rate_limited || transport_failure;
                let total_cap_ms = if slow_recovery {
                    retry_policy.rate_limit_backoff_ms
                } else {
                    retry_policy.total_backoff_ms
                };
                let per_attempt_cap_ms = if slow_recovery {
                    retry_policy.rate_limit_attempt_cap_ms
                } else {
                    retry_policy.total_backoff_ms
                };
                let delay_ms = retry_delay_ms(
                    attempt,
                    base_delay_ms,
                    backoff_so_far_ms,
                    retry_after_ms,
                    rand::thread_rng().gen(),
                    total_cap_ms,
                    per_attempt_cap_ms,
                );
                let remaining_ms = total_cap_ms.saturating_sub(backoff_so_far_ms);
                if remaining_ms == 0
                    || delay_ms > remaining_ms
                    || remaining_deadline().is_some_and(|remaining| {
                        std::time::Duration::from_millis(delay_ms) >= remaining
                    })
                {
                    warn!(attempt, remaining_ms, delay_ms,
                        "Retry budget cannot honor the required wait; stopping without an early retry");
                    break;
                }
                backoff_so_far_ms = backoff_so_far_ms.saturating_add(delay_ms);
                warn!(
                    attempt,
                    max_attempts,
                    delay_ms,
                    ?iteration,
                    "Retrying LLM call after transient error"
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            let mut excluded_provider_indices = Vec::new();

            loop {
                if remaining_deadline().is_some_and(|remaining| remaining.is_zero()) {
                    return Err(blockcell_core::Error::Timeout(
                        "LLM request deadline exhausted".into(),
                    ));
                }
                let provider_candidate = self
                    .provider_pool
                    .acquire_with_strategy_excluding(
                        self.config.agents.defaults.routing_strategy,
                        &routing_context,
                        &excluded_provider_indices,
                    )
                    .or_else(|| {
                        (attempt > 0 && excluded_provider_indices.is_empty())
                            .then(|| {
                                self.provider_pool.acquire_probe_excluding(
                                    self.config.agents.defaults.routing_strategy,
                                    &routing_context,
                                    &excluded_provider_indices,
                                )
                            })
                            .flatten()
                    });
                let (pool_idx, provider) = match provider_candidate {
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

                *requests_sent = (*requests_sent).saturating_add(1);
                let stream_started = std::time::Instant::now();
                let first_event_timeout = std::time::Duration::from_secs(
                    self.config
                        .agents
                        .defaults
                        .stream_first_event_timeout_secs
                        .max(1),
                );
                match tokio::time::timeout(
                    remaining_deadline().map_or(first_event_timeout, |remaining| {
                        first_event_timeout.min(remaining)
                    }),
                    provider.chat_stream(&api_messages, tools),
                )
                .await
                {
                    Err(_) => {
                        let message = format!(
                            "Stream header timeout after {} seconds",
                            first_event_timeout.as_secs()
                        );
                        last_error = Some(blockcell_core::Error::Provider(message));
                        self.provider_pool.report(pool_idx, CallResult::Transient);
                        excluded_provider_indices.push(pool_idx);
                        continue;
                    }
                    Ok(Ok(mut stream_rx)) => {
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
                        let mut tool_call_accumulators: HashMap<usize, ToolCallAccumulator> =
                            HashMap::new();
                        let mut emitted_text_delta = false;
                        let mut stream_error: Option<blockcell_core::Error> = None;

                        let first_event_timeout = std::time::Duration::from_secs(
                            self.config
                                .agents
                                .defaults
                                .stream_first_event_timeout_secs
                                .max(1),
                        );
                        let idle_timeout = std::time::Duration::from_secs(
                            self.config.agents.defaults.stream_idle_timeout_secs.max(1),
                        );
                        let mut received_event = false;
                        // 首包延迟（TTFT）与总耗时是判断 provider 卡顿的主要依据。
                        let mut first_event_ms: Option<u128> = None;

                        loop {
                            let timeout = if received_event {
                                idle_timeout
                            } else {
                                first_event_timeout
                            };
                            let timeout = remaining_deadline()
                                .map_or(timeout, |remaining| timeout.min(remaining));
                            let recv_result = tokio::time::timeout(timeout, stream_rx.recv()).await;

                            match recv_result {
                                Ok(Some(chunk)) => {
                                    if first_event_ms.is_none() {
                                        first_event_ms = Some(stream_started.elapsed().as_millis());
                                        debug!(
                                            ttft_ms = first_event_ms.unwrap_or_default() as u64,
                                            attempt,
                                            ?iteration,
                                            pool_idx,
                                            "LLM stream first event"
                                        );
                                    }
                                    match chunk {
                                        StreamChunk::TextDelta { delta } => {
                                            received_event = true;
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
                                            received_event = true;
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
                                        StreamChunk::ToolCallStart { index, id, name } => {
                                            received_event = true;
                                            let acc =
                                                tool_call_accumulators.entry(index).or_default();
                                            if acc.id.is_empty() && !id.is_empty() {
                                                acc.id = id;
                                            }
                                            if acc.name.is_empty() && !name.is_empty() {
                                                acc.name = name;
                                            }
                                        }
                                        StreamChunk::ToolCallDelta { index, id, delta } => {
                                            received_event = true;
                                            let acc =
                                                tool_call_accumulators.entry(index).or_default();
                                            if acc.id.is_empty() && !id.is_empty() {
                                                acc.id = id;
                                            }
                                            acc.arguments.push_str(&delta);
                                        }
                                        StreamChunk::Done { response } => {
                                            // 网关内容护栏存在误伤：实测同一 payload 稍后重试即可
                                            // 通过。把 content_filter 当作瞬时错误交给既有重试逻辑，
                                            // 而不是把拒答话术当作最终答案交付。
                                            if response.finish_reason == "content_filter" {
                                                warn!(
                                                    attempt,
                                                    ?iteration,
                                                    pool_idx,
                                                    "LLM stream ended with content_filter; retrying"
                                                );
                                                stream_error =
                                                    Some(blockcell_core::Error::ProviderFailure(blockcell_core::ProviderFailure {
                                                        kind: if retry_policy.retry_content_filter { FailureKind::ProviderUnavailable } else { FailureKind::InvalidRequest },
                                                        status: None, code: Some("content_filter".into()), retry_after_ms: None,
                                                        message: "LLM content filter rejected the request".into(),
                                                    }));
                                                break;
                                            }
                                            let final_tool_calls = merge_stream_tool_calls(
                                                std::mem::take(&mut tool_call_accumulators),
                                                response.tool_calls.clone(),
                                            );

                                            let final_content = if !accumulated_content.is_empty() {
                                                Some(accumulated_content.clone())
                                            } else {
                                                response.content.clone()
                                            };

                                            let final_reasoning =
                                                if !accumulated_reasoning.is_empty() {
                                                    Some(accumulated_reasoning.clone())
                                                } else {
                                                    response.reasoning_content.clone()
                                                };

                                            self.provider_pool
                                                .report(pool_idx, CallResult::Success);
                                            info!(
                                                ttft_ms = first_event_ms.unwrap_or_default() as u64,
                                                total_ms =
                                                    stream_started.elapsed().as_millis() as u64,
                                                attempt,
                                                ?iteration,
                                                pool_idx,
                                                "LLM stream completed"
                                            );
                                            return Ok((
                                                LLMResponse {
                                                    content: final_content,
                                                    reasoning_content: final_reasoning,
                                                    tool_calls: final_tool_calls,
                                                    finish_reason: response.finish_reason.clone(),
                                                    usage: response.usage.clone(),
                                                },
                                                self.served_by(pool_idx),
                                            ));
                                        }
                                        StreamChunk::Error { message } => {
                                            warn!(error = %message, "Stream error");
                                            stream_error =
                                                Some(blockcell_core::Error::Provider(message));
                                            break;
                                        }
                                    }
                                }
                                Ok(None) => {
                                    break;
                                }
                                Err(_) => {
                                    warn!(
                                        "Stream receive timeout after {} seconds",
                                        timeout.as_secs()
                                    );
                                    stream_error = Some(blockcell_core::Error::Provider(format!(
                                        "Stream timeout after {} seconds",
                                        timeout.as_secs()
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
                            info!(
                                ttft_ms = first_event_ms.unwrap_or_default() as u64,
                                total_ms = stream_started.elapsed().as_millis() as u64,
                                attempt,
                                ?iteration,
                                pool_idx,
                                "LLM stream completed without an explicit done event"
                            );
                            let final_tool_calls =
                                merge_stream_tool_calls(tool_call_accumulators, Vec::new());

                            let served_by = self.served_by(pool_idx);
                            return Ok((
                                LLMResponse {
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
                                },
                                served_by,
                            ));
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
                        let call_result = ProviderPool::classify_failure(&err);
                        if matches!(&call_result, CallResult::RateLimit) {
                            *saw_rate_limit_this_turn = true;
                        }
                        self.provider_pool.report(pool_idx, call_result);
                        let failure = err.provider_failure_kind();
                        last_failure = Some(failure);
                        last_error = Some(err);
                        if failure.request_recovery().switch_provider {
                            excluded_provider_indices.push(pool_idx);
                            continue;
                        }
                        break;
                    }
                    Ok(Err(e)) => {
                        let err_str = format!("{}", e);
                        warn!(error = %err_str, attempt, max_retries, ?iteration, pool_idx, "LLM stream call failed");
                        let call_result = ProviderPool::classify_failure(&e);
                        if matches!(&call_result, CallResult::RateLimit) {
                            *saw_rate_limit_this_turn = true;
                        }
                        self.provider_pool.report(pool_idx, call_result);
                        let failure = e.provider_failure_kind();
                        last_failure = Some(failure);
                        last_error = Some(e);
                        // 限流、服务不可用、凭据失效都只针对当前 provider：本轮先换下一个 provider。
                        if failure.request_recovery().switch_provider {
                            excluded_provider_indices.push(pool_idx);
                            warn!(
                                attempt,
                                max_retries,
                                ?iteration,
                                pool_idx,
                                failure = failure.as_str(),
                                "LLM provider failure; trying fallback provider"
                            );
                            continue;
                        }
                        break;
                    }
                }
            }
            // 所有 provider 都失败后，按失败类型决定是否退避重试；Provider 只报告错误。
            if let Some(failure) = last_failure {
                match failure.request_recovery().retry {
                    RequestRetry::Backoff => {}
                    RequestRetry::Once if attempt == 0 => {}
                    retry => {
                        warn!(
                            failure = failure.as_str(),
                            ?retry,
                            attempt,
                            "LLM failure is not retried; giving up"
                        );
                        break;
                    }
                }
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

#[cfg(test)]
mod retry_delay_tests {
    use super::*;

    #[test]
    fn transport_failures_are_recognised_by_code() {
        let failure = |code: Option<&str>, message: &str| {
            blockcell_core::Error::ProviderFailure(blockcell_core::ProviderFailure {
                kind: FailureKind::ProviderUnavailable,
                status: None,
                code: code.map(str::to_string),
                retry_after_ms: None,
                message: message.to_string(),
            })
        };

        assert!(is_transport_failure(&failure(
            Some("transport"),
            "LLM stream request failed: error sending request"
        )));
        // content_filter 也是 ProviderUnavailable，但重试无用，不能被放宽。
        assert!(!is_transport_failure(&failure(
            Some("content_filter"),
            "LLM content filter rejected the request"
        )));
        // 首包超时走的是换 provider 的路径，不占用 transport 的额外预算。
        assert!(!is_transport_failure(&blockcell_core::Error::Provider(
            "Stream header timeout after 90 seconds".into()
        )));
    }

    #[test]
    fn retry_delay_uses_full_jitter_within_exponential_bounds() {
        assert_eq!(retry_delay_ms(3, 1000, 0, None, 0.0, 60_000, 60_000), 0);
        assert_eq!(retry_delay_ms(3, 1000, 0, None, 0.25, 60_000, 60_000), 1000);
        assert_eq!(retry_delay_ms(3, 1000, 0, None, 1.0, 60_000, 60_000), 4000);
        assert_eq!(retry_delay_ms(3, 1000, 0, None, 1.5, 60_000, 60_000), 4000);
    }

    #[test]
    fn retry_after_is_not_shortened_by_the_remaining_budget() {
        assert_eq!(
            retry_delay_ms(4, 1000, 0, Some(3500), 0.0, 60_000, 60_000),
            3500
        );
        assert_eq!(
            retry_delay_ms(4, 1000, 59_000, Some(3500), 1.0, 60_000, 60_000),
            3500
        );
    }

    #[test]
    fn rate_limit_gets_a_larger_total_backoff_budget() {
        // 限流窗口往往几秒内自行恢复：同样的指数退避，总预算放大后允许更晚放弃。
        assert_eq!(
            retry_delay_ms(4, 1000, 59_000, Some(3500), 1.0, 240_000, 240_000),
            3500
        );
        // attempt=6 → 指数 32 倍，剩余预算 180s，不会被总预算截断
        assert_eq!(
            retry_delay_ms(6, 1000, 60_000, None, 1.0, 240_000, 240_000),
            32_000
        );
        // 普通 60s 封顶时同样的请求会被截到 0（已无剩余预算）
        assert_eq!(
            retry_delay_ms(6, 1000, 60_000, None, 1.0, 60_000, 60_000),
            0
        );
    }

    #[test]
    fn rate_limit_single_wait_is_capped_so_budget_buys_more_attempts() {
        // 单次等待压到 30s：同样的 240s 预算，从"指数涨到 32s 就吃掉大半"变成
        // "每次最多等 30s，能多试好几轮"。
        assert_eq!(
            retry_delay_ms(
                8,
                1000,
                0,
                None,
                1.0,
                240_000,
                blockcell_core::config::LlmRetryPolicy::default().rate_limit_attempt_cap_ms
            ),
            blockcell_core::config::LlmRetryPolicy::default().rate_limit_attempt_cap_ms
        );
        // Retry-After is a minimum wait and must bypass the exponential cap.
        assert_eq!(
            retry_delay_ms(
                2,
                1000,
                0,
                Some(120_000),
                1.0,
                240_000,
                blockcell_core::config::LlmRetryPolicy::default().rate_limit_attempt_cap_ms
            ),
            120_000
        );
    }

    #[test]
    fn retry_after_header_supports_seconds_and_http_dates() {
        let seconds = blockcell_core::Error::Provider(
            "HTTP_STATUS=429 request failed (body_len=0, retry_after=3)".to_string(),
        );
        assert_eq!(retry_after_delay_ms(&seconds), Some(3000));

        let date = blockcell_core::Error::Provider(
            "HTTP_STATUS=429 request failed (body_len=0, retry_after=Wed, 21 Oct 2099 07:28:00 GMT)"
                .to_string(),
        );
        assert!(retry_after_delay_ms(&date).is_some_and(|delay| delay > 0));
    }
}
