use super::*;

/// 运行 Forked Agent
///
/// 这是 Forked Agent 的主要入口点。
///
/// ## 参数
///
/// - `params`: Forked Agent 配置参数
///
/// ## 返回
///
/// - `Ok(ForkedAgentResult)`: 执行成功，包含消息和用量
/// - `Err(ForkedAgentError)`: 执行失败
///
/// ## 示例
///
/// ```ignore
/// let result = run_forked_agent(ForkedAgentParams {
///     prompt_messages: vec![ChatMessage::user("分析这段对话")],
///     cache_safe_params,
///     provider_pool,
///     can_use_tool: create_auto_mem_can_use_tool(&memory_dir),
///     query_source: "auto_memory",
///     fork_label: "auto_memory",
///     max_turns: Some(5),
///     ..Default::default()
/// }).await?;
/// ```
pub async fn run_forked_agent(
    params: ForkedAgentParams,
) -> Result<ForkedAgentResult, ForkedAgentError> {
    let start_time = Instant::now();
    let mut output_messages = Vec::new();
    let mut total_usage = UsageMetrics::default();
    let mut files_modified = Vec::new();

    // 准备子代理上下文覆盖（包含 working_dir）
    let mut overrides = params.overrides.unwrap_or_default();
    if let Some(ref working_dir) = params.working_dir {
        overrides.working_dir = Some(working_dir.clone());
    }

    // Get the current AbortToken from task-local context for chain propagation
    let parent_abort_token = blockcell_core::current_abort_token();

    // 创建子代理上下文
    let context = create_subagent_context(
        None,                        // parent_file_state - 在实际集成时从 runtime 获取
        None,                        // parent_replacement_state
        None,                        // parent_abort_controller (legacy)
        parent_abort_token.as_ref(), // Wire parent abort token for chain cancellation
        overrides,
    );

    // 检查是否已取消（使用新的 AbortToken）
    if let Err(e) = context.abort_token.check() {
        return Err(ForkedAgentError::Aborted(e.message));
    }

    // 同时检查 legacy AbortController
    if context.abort_controller.is_aborted() {
        return Err(ForkedAgentError::Aborted(
            context
                .abort_controller
                .reason()
                .unwrap_or_else(|| "Aborted".to_string()),
        ));
    }

    // 构建初始消息（父消息前缀 + 子代理增量输入，避免重复插入 system message）
    let mut messages = build_initial_forked_messages(
        &params.cache_safe_params,
        &params.prompt_messages,
        params.system_prompt.as_deref(),
    );

    inject_preloaded_skills(
        &mut messages,
        &params.skills,
        params.skills_dir.as_deref(),
        &params.external_skills_dirs,
    );

    if !params.mcp_servers.is_empty() {
        tracing::warn!(
            fork_label = params.fork_label,
            mcp_servers = ?params.mcp_servers,
            "[forked_agent] Custom agent mcp_servers are parsed as metadata; forked direct MCP execution is not available yet"
        );
    }

    // 注入 initial_prompt（自定义 Agent 的首轮提示）
    if let Some(ref initial_prompt) = params.initial_prompt {
        tracing::debug!(
            initial_prompt_len = initial_prompt.len(),
            "[forked_agent] 注入 initial_prompt"
        );
        insert_initial_prompt(&mut messages, initial_prompt);
    }

    // 构建工具 schema（根据 tools 白名单和 disallowed_tools 黑名单过滤）
    let filtered_tool_schemas = filter_tool_schemas(
        &params.tool_schemas,
        params.tools.as_deref(),
        &params.disallowed_tools,
    );

    // 记录开始
    tracing::info!(
        fork_label = params.fork_label,
        query_source = params.query_source,
        message_count = messages.len(),
        max_turns = ?params.max_turns,
        agent_type = ?params.agent_type,
        one_shot = params.one_shot,
        disallowed_tools = ?params.disallowed_tools,
        tools_whitelist = ?params.tools,
        filtered_tool_count = filtered_tool_schemas.len(),
        "[forked_agent] starting"
    );

    // 记录 Layer 7 agent_spawned 事件
    memory_event!(
        layer7,
        agent_spawned,
        params.fork_label,
        params.max_turns.unwrap_or(5),
        "main"
    );

    // 获取 Provider（带重试和指数退避）
    let provider_pool = match params.provider_pool.as_ref() {
        Some(pool) => pool,
        None => {
            // 记录 Layer 7 agent_failed 事件（Provider 未配置）
            memory_event!(layer7, agent_failed, params.fork_label, "no_provider", 0);
            return Err(ForkedAgentError::NoProviderAvailable);
        }
    };

    let provider = match acquire_provider_with_retry(
        provider_pool,
        params.model.as_deref(),
        PROVIDER_RETRY_MAX_ATTEMPTS,
        PROVIDER_RETRY_INITIAL_DELAY_MS,
        PROVIDER_RETRY_MAX_DELAY_MS,
        &context.abort_token,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            // 记录 Layer 7 agent_failed 事件（Provider 获取失败）
            memory_event!(
                layer7,
                agent_failed,
                params.fork_label,
                "provider_acquire_failed",
                0
            );
            return Err(e);
        }
    };

    // 模型覆盖提示（当自定义 Agent 指定了特定模型时）
    if let Some(ref model_override) = params.model {
        tracing::info!(
            fork_label = params.fork_label,
            model_override,
            "[forked_agent] 自定义 Agent 指定的模型覆盖已生效"
        );
    }

    let max_turns = params.max_turns.unwrap_or(5);
    let mut current_messages = messages.clone();
    let mut final_content = None;
    let mut truncated = false;
    // 跟踪是否有工具调用失败（权限拒绝、old_string not found 等），
    // memory extraction 据此决定是否推进游标和 record_success
    let mut had_tool_error = false;
    // Track the actual number of turns used (as opposed to max_turns cap).
    let mut actual_turns: u32 = max_turns;

    for turn in 0..max_turns {
        // 检查取消（使用新的 AbortToken）
        if context.abort_token.is_cancelled() {
            tracing::warn!(
                fork_label = params.fork_label,
                turn,
                "[forked_agent] cancelled via AbortToken"
            );
            memory_event!(layer7, agent_failed, params.fork_label, "cancelled", turn);
            return Err(ForkedAgentError::Aborted(
                "Cancelled via AbortToken".to_string(),
            ));
        }

        // 检查中止（legacy AbortController）
        if context.abort_controller.is_aborted() {
            tracing::warn!(
                fork_label = params.fork_label,
                turn,
                "[forked_agent] aborted"
            );
            // 记录 Layer 7 agent_failed 事件
            memory_event!(layer7, agent_failed, params.fork_label, "aborted", turn);
            return Err(ForkedAgentError::Aborted(
                context
                    .abort_controller
                    .reason()
                    .unwrap_or_else(|| "Aborted".to_string()),
            ));
        }

        // 调用 LLM 前：发送进度事件，让用户知道子 agent 正在工作
        if let Some(ref event_tx) = params.event_tx {
            let agent_type_str = params.agent_type.as_deref().unwrap_or("fork");
            let percent = max_turns
                .checked_div(1)
                .map(|mt| (turn * 100 / mt).min(100) as u8)
                .unwrap_or(0);
            let event = serde_json::json!({
                "type": "agent_progress",
                "agent_type": agent_type_str,
                "task_id": params.task_id,
                "turn": turn,
                "max_turns": max_turns,
                "stage": "Thinking...",
                "percent": percent,
            });
            let _ = event_tx.send(event.to_string());
        }

        // 调用 LLM（传入过滤后的工具 schema，让 LLM 知道可以调用哪些工具）
        let provider_result = tokio::select! {
            biased;
            _ = context.abort_token.cancelled() => {
                return Err(ForkedAgentError::Aborted(
                    "Cancelled while waiting for provider".to_string(),
                ));
            }
            result = provider.chat(&current_messages, &filtered_tool_schemas) => result,
        };
        let response = match provider_result {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    fork_label = params.fork_label,
                    turn,
                    error = %e,
                    "[forked_agent] LLM call failed"
                );
                // 记录 Layer 7 agent_failed 事件
                memory_event!(layer7, agent_failed, params.fork_label, "llm_error", turn);
                return Err(ForkedAgentError::ProviderError(format!("{}", e)));
            }
        };

        // 提取用量（兼容 Anthropic/OpenAI/Gemini/Ollama 归一化后的 prompt_tokens/completion_tokens 与 input_tokens/output_tokens）
        if !response.usage.is_null() {
            let (input, output, cache_read, cache_creation) =
                extract_forked_usage(&response.usage);
            total_usage.accumulate(input, output, cache_read, cache_creation);
        }

        // 提取内容
        let content = response.content.clone();
        final_content = content.clone();

        // 通过 event_tx 通知父级：子agent完成了一个 turn（进度反馈）
        if let Some(ref event_tx) = params.event_tx {
            let agent_type_str = params.agent_type.as_deref().unwrap_or("fork");
            let stage = if response.tool_calls.is_empty() {
                "Generating response".to_string()
            } else {
                let tools: Vec<&str> = response
                    .tool_calls
                    .iter()
                    .map(|tc| tc.name.as_str())
                    .collect();
                format!("Calling: {}", tools.join(", "))
            };
            // 计算百分比：基于当前 turn / max_turns
            let percent = max_turns
                .checked_div(1)
                .map(|mt| ((turn + 1) * 100 / mt).min(100) as u8)
                .unwrap_or(0);
            let event = serde_json::json!({
                "type": "agent_progress",
                "agent_type": agent_type_str,
                "task_id": params.task_id,
                "turn": turn + 1,
                "max_turns": max_turns,
                "stage": stage,
                "percent": percent,
            });
            match event_tx.send(event.to_string()) {
                Ok(n) => tracing::debug!(receivers = n, "[forked_agent] sent agent_progress event"),
                Err(e) => {
                    tracing::warn!(error = %e, "[forked_agent] failed to send agent_progress event (no receivers?)")
                }
            }
        } else {
            tracing::debug!("[forked_agent] event_tx is None, skipping agent_progress event");
        }

        // 创建 assistant 消息 — preserve reasoning_content to avoid DeepSeek 400 errors
        let assistant_msg = if !response.tool_calls.is_empty() {
            // 有工具调用
            ChatMessage {
                id: None,
                role: "assistant".to_string(),
                content: serde_json::Value::String(content.clone().unwrap_or_default()),
                reasoning_content: response.reasoning_content.clone(),
                tool_calls: Some(response.tool_calls.clone()),
                tool_call_id: None,
                name: None,
            }
        } else {
            ChatMessage::assistant_with_reasoning(
                content.as_deref().unwrap_or(""),
                response.reasoning_content.clone(),
            )
        };

        current_messages.push(assistant_msg.clone());
        output_messages.push(assistant_msg);

        // 检查是否有工具调用
        if !response.tool_calls.is_empty() {
            tracing::debug!(
                fork_label = params.fork_label,
                turn,
                tool_count = response.tool_calls.len(),
                "[forked_agent] executing tool calls"
            );

            // 执行每个工具调用
            for tool_call in &response.tool_calls {
                let tool_name = &tool_call.name;
                let tool_input = &tool_call.arguments;

                tracing::debug!(
                    fork_label = params.fork_label,
                    turn,
                    tool_name,
                    "[forked_agent] executing tool"
                );

                // 通过 event_tx 通知父级：子agent正在调用工具
                if let Some(ref event_tx) = params.event_tx {
                    let agent_type_str = params.agent_type.as_deref().unwrap_or("fork");
                    // 从工具参数中提取摘要信息（文件路径、搜索模式等）
                    let tool_summary = extract_tool_summary(tool_name, tool_input);
                    let event = serde_json::json!({
                        "type": "tool_call_start",
                        "tool": tool_name,
                        "call_id": tool_call.id,
                        "agent_type": agent_type_str,
                        "task_id": params.task_id,
                        "summary": tool_summary,
                        "params": tool_input,
                    });
                    if let Err(e) = event_tx.send(event.to_string()) {
                        tracing::warn!(error = %e, tool = tool_name, "[forked_agent] failed to send tool_call_start event");
                    }
                }

                // 通过 progress_tx 转发工具调用事件到外部渠道
                if let Some(ref progress_tx) = params.progress_tx {
                    let agent_type_str = params.agent_type.as_deref().unwrap_or("fork");
                    let tool_summary = extract_tool_summary(tool_name, tool_input);
                    let _ = progress_tx
                        .send(crate::agent_progress::AgentProgress::ToolCallStart {
                            task_id: params.task_id.clone().unwrap_or_default(),
                            tool: tool_name.clone(),
                            call_id: tool_call.id.clone(),
                            agent_type: agent_type_str.to_string(),
                            summary: tool_summary,
                        })
                        .await;
                }

                // 执行工具
                let tool_result = tokio::select! {
                    biased;
                    _ = context.abort_token.cancelled() => {
                        return Err(ForkedAgentError::Aborted(
                            format!("Cancelled while executing tool '{}'", tool_name),
                        ));
                    }
                    result = execute_forked_tool(
                        tool_name,
                        tool_input,
                        &params.can_use_tool,
                        &params.disallowed_tools,
                        &params.memory_store,
                        &params.memory_file_store,
                        &params.skill_file_store,
                        &params.skills_dir,
                        &params.external_skills_dirs,
                        &params.skill_mutex,
                        &params.working_dir,
                        &params.workspace_scope,
                    ) => result,
                };

                // 跟踪修改的文件
                if tool_result.is_ok() {
                    match tool_name.as_str() {
                        "file_edit" | "edit_file" | "file_write" | "write_file" => {
                            if let Some(file_path) =
                                tool_input.get("file_path").and_then(|v| v.as_str())
                            {
                                if !files_modified.contains(&file_path.to_string()) {
                                    files_modified.push(file_path.to_string());
                                }
                            }
                        }
                        "skill_manage" => {
                            if let Some(name) = tool_input.get("name").and_then(|v| v.as_str()) {
                                let action = tool_input
                                    .get("action")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if matches!(
                                    action,
                                    "create"
                                        | "edit"
                                        | "patch"
                                        | "delete"
                                        | "write_file"
                                        | "remove_file"
                                ) {
                                    let skill_path = format!("skills/{}/", name);
                                    if !files_modified.contains(&skill_path) {
                                        files_modified.push(skill_path);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }

                // 构建工具结果消息，包含详细的错误上下文
                let tool_success = tool_result.is_ok();
                if !tool_success {
                    had_tool_error = true;
                }
                let result_content = match tool_result {
                    Ok(result) => {
                        // 跟踪修改的文件（edit_file / write_file）
                        if matches!(
                            tool_name.as_str(),
                            "edit_file" | "write_file" | "file_edit" | "file_write"
                        ) {
                            let file_path = tool_input
                                .get("file_path")
                                .or_else(|| tool_input.get("path"))
                                .and_then(|v| v.as_str());
                            if let Some(path) = file_path {
                                if !files_modified.iter().any(|f| f == path) {
                                    files_modified.push(path.to_string());
                                }
                            }
                        }
                        result
                    }
                    Err(ref e) => {
                        // 包含错误类型和详细信息，便于调试
                        let error_type = match e {
                            ForkedAgentError::ProviderError(_) => "ProviderError",
                            ForkedAgentError::ToolError(_) => "ToolError",
                            ForkedAgentError::Io(_) => "IoError",
                            ForkedAgentError::Json(_) => "JsonError",
                            ForkedAgentError::ToolNotSupported(_) => "ToolNotSupported",
                            ForkedAgentError::MaxTurnsExceeded => "MaxTurnsExceeded",
                            ForkedAgentError::NoProviderAvailable => "NoProviderAvailable",
                            ForkedAgentError::Aborted(_) => "Aborted",
                        };
                        tracing::warn!(
                            event = "tool_failed",
                            fork_label = %params.fork_label,
                            tool_name = %tool_name,
                            error_type,
                            error = %e,
                            input = %tool_input,
                            "[forked_agent] tool execution failed"
                        );
                        format!("Tool execution error ({}): {}", error_type, e)
                    }
                };

                // 通过 event_tx 通知父级：子agent工具调用完成
                if let Some(ref event_tx) = params.event_tx {
                    let agent_type_str = params.agent_type.as_deref().unwrap_or("fork");
                    // tool_call_end: CLI event_handler 使用
                    let event = serde_json::json!({
                        "type": "tool_call_end",
                        "tool": tool_name,
                        "call_id": tool_call.id,
                        "agent_type": agent_type_str,
                        "task_id": params.task_id,
                        "success": tool_success,
                    });
                    if let Err(e) = event_tx.send(event.to_string()) {
                        tracing::warn!(error = %e, tool = tool_name, "[forked_agent] failed to send tool_call_end event");
                    }
                    // tool_call_result: WebUI 使用，更新工具调用状态从 running -> done
                    let result_event = serde_json::json!({
                        "type": "tool_call_result",
                        "call_id": tool_call.id,
                        "task_id": params.task_id,
                        "result": result_content,
                        "duration_ms": 0,
                    });
                    if let Err(e) = event_tx.send(result_event.to_string()) {
                        tracing::warn!(error = %e, tool = tool_name, "[forked_agent] failed to send tool_call_result event");
                    }
                }

                // 通过 progress_tx 转发工具调用完成事件到外部渠道
                if let Some(ref progress_tx) = params.progress_tx {
                    let agent_type_str = params.agent_type.as_deref().unwrap_or("fork");
                    let _ = progress_tx
                        .send(crate::agent_progress::AgentProgress::ToolCallEnd {
                            task_id: params.task_id.clone().unwrap_or_default(),
                            tool: tool_name.clone(),
                            call_id: tool_call.id.clone(),
                            agent_type: agent_type_str.to_string(),
                            success: tool_success,
                        })
                        .await;
                }

                // 添加工具结果到消息
                let tool_result_msg = ChatMessage {
                    id: None,
                    role: "tool".to_string(),
                    content: serde_json::Value::String(result_content),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some(tool_call.id.clone()),
                    name: Some(tool_name.clone()),
                };

                current_messages.push(tool_result_msg.clone());
                output_messages.push(tool_result_msg);
            }

            // 继续循环让 LLM 处理工具结果
            // 如果这是最后一个 turn 且仍有工具调用，标记为截断
            if turn == max_turns - 1 {
                truncated = true;
                tracing::warn!(
                    fork_label = params.fork_label,
                    max_turns,
                    "[forked_agent] 达到 max_turns 上限，结果被截断"
                );
            }
            continue;
        }

        // 没有工具调用，检查是否应该结束循环
        // one_shot 模式下，如果 turn 0 就没有工具调用，LLM 可能只是在"思考"
        // （例如先列出分析计划），给一次额外机会继续到 turn 1
        if params.one_shot && turn == 0 {
            tracing::debug!(
                fork_label = params.fork_label,
                turn,
                "[forked_agent] one_shot: turn 0 had no tool calls, continuing to turn 1"
            );
            continue;
        }
        actual_turns = turn + 1;
        break;
    }

    // 清理资源
    drop(context);

    // 记录分析事件
    let duration_ms = start_time.elapsed().as_millis() as u64;
    tracing::info!(
        fork_label = params.fork_label,
        query_source = params.query_source,
        duration_ms,
        message_count = output_messages.len(),
        input_tokens = total_usage.input_tokens,
        output_tokens = total_usage.output_tokens,
        cache_hit_rate = total_usage.cache_hit_rate(),
        "[forked_agent] completed"
    );

    let cache_hit_rate = total_usage.cache_hit_rate();

    // 记录 Layer 7 agent_completed 事件（带 duration 和 cache_hit_rate）
    memory_event!(
        layer7,
        agent_completed_with_duration,
        params.fork_label,
        actual_turns as u64,
        total_usage.input_tokens + total_usage.output_tokens,
        duration_ms,
        cache_hit_rate
    );

    Ok(ForkedAgentResult {
        messages: output_messages,
        total_usage,
        files_modified,
        final_content,
        truncated,
        had_tool_error,
    })
}

pub(crate) fn build_initial_forked_messages(
    cache_safe_params: &CacheSafeParams,
    prompt_messages: &[ChatMessage],
    system_prompt_override: Option<&str>,
) -> Vec<ChatMessage> {
    let mut messages: Vec<ChatMessage> = cache_safe_params
        .fork_context_messages
        .iter()
        .cloned()
        .chain(prompt_messages.iter().cloned())
        .collect();

    let system_prompt = system_prompt_override.unwrap_or(cache_safe_params.system_prompt.as_str());

    if !system_prompt.is_empty() {
        if messages.first().map(|m| m.role.as_str()) == Some("system") {
            if system_prompt_override.is_some() {
                messages[0] = ChatMessage::system(system_prompt);
            }
        } else {
            messages.insert(0, ChatMessage::system(system_prompt));
        }
    }

    messages
}

pub(crate) fn insert_initial_prompt(messages: &mut Vec<ChatMessage>, initial_prompt: &str) {
    let insert_at = messages
        .iter()
        .position(|message| message.role != "system")
        .unwrap_or(messages.len());
    messages.insert(insert_at, ChatMessage::user(initial_prompt));
}

pub(crate) fn tool_schema_name(schema: &serde_json::Value) -> Option<&str> {
    schema
        .get("function")
        .and_then(|function| function.get("name"))
        .and_then(|name| name.as_str())
        .or_else(|| schema.get("name").and_then(|name| name.as_str()))
}

pub(crate) fn filter_tool_schemas(
    tool_schemas: &[serde_json::Value],
    allowed_tools: Option<&[String]>,
    disallowed_tools: &[String],
) -> Vec<serde_json::Value> {
    let allows_all = allowed_tools
        .map(|tools| tools.iter().any(|tool| tool == "*"))
        .unwrap_or(true);

    tool_schemas
        .iter()
        .filter(|schema| {
            let Some(name) = tool_schema_name(schema) else {
                return false;
            };
            let allowed = allowed_tools
                .map(|tools| allows_all || tools.iter().any(|tool| tool == name))
                .unwrap_or(true);
            let disallowed = disallowed_tools.iter().any(|tool| tool == name);
            allowed && !disallowed
        })
        .cloned()
        .collect()
}

pub(crate) fn append_to_system_prompt(messages: &mut Vec<ChatMessage>, section: &str) {
    if let Some(system_message) = messages.iter_mut().find(|message| message.role == "system") {
        let existing = system_message.content.as_str().unwrap_or_default();
        system_message.content = serde_json::Value::String(format!("{}{}", existing, section));
    } else {
        messages.insert(0, ChatMessage::system(section));
    }
}

pub(crate) fn inject_preloaded_skills(
    messages: &mut Vec<ChatMessage>,
    skill_names: &[String],
    skills_dir: Option<&Path>,
    external_skills_dirs: &[PathBuf],
) {
    if skill_names.is_empty() {
        return;
    }

    let Some(skills_dir) = skills_dir else {
        tracing::warn!(
            skills = ?skill_names,
            "[forked_agent] Cannot preload custom agent skills without skills_dir"
        );
        return;
    };

    let mut section = String::from(
        "\n\n## Preloaded Skills\nThese skills are preloaded by this custom agent definition. Treat them as active task guidance.\n",
    );
    let mut loaded = 0usize;

    for skill_name in skill_names {
        let Some(skill_dir) =
            find_skill_dir_forked(skill_name, None, skills_dir, external_skills_dirs)
        else {
            tracing::warn!(skill = %skill_name, "[forked_agent] Preloaded skill not found");
            continue;
        };

        match std::fs::read_to_string(skill_dir.join("SKILL.md")) {
            Ok(content) => {
                loaded += 1;
                section.push_str(&format!(
                    "\n### {}\n{}\n",
                    skill_name,
                    truncate_output(content, 8_000)
                ));
            }
            Err(error) => {
                tracing::warn!(
                    skill = %skill_name,
                    error = %error,
                    "[forked_agent] Failed to read preloaded skill"
                );
            }
        }
    }

    if loaded > 0 {
        append_to_system_prompt(messages, &section);
    }
}

/// 带重试的 Provider 获取
///
/// 使用指数退避策略重试获取 provider，避免因短暂不可用而直接失败。
///
/// 在每次重试前和 sleep 期间（每 200ms）检查 `abort_token`，
/// 如果已取消则立即返回 `ForkedAgentError::Aborted`。
pub(crate) async fn acquire_provider_with_retry(
    provider_pool: &Arc<ProviderPool>,
    model_override: Option<&str>,
    max_attempts: usize,
    initial_delay_ms: u64,
    max_delay_ms: u64,
    abort_token: &blockcell_core::AbortToken,
) -> Result<Arc<dyn blockcell_providers::Provider>, ForkedAgentError> {
    let mut delay_ms = initial_delay_ms;

    for attempt in 0..max_attempts {
        // 检查取消信号，避免在已取消时继续重试
        if abort_token.is_cancelled() {
            return Err(ForkedAgentError::Aborted(
                "Operation aborted while acquiring provider".to_string(),
            ));
        }

        let acquired = if let Some(model) = model_override {
            provider_pool.acquire_by_model(model)
        } else {
            provider_pool.acquire()
        };

        match acquired {
            Some((_name, provider)) => {
                if attempt > 0 {
                    tracing::info!(
                        attempt = attempt + 1,
                        "[forked_agent] Provider acquired after retry"
                    );
                }
                return Ok(provider);
            }
            None => {
                if attempt < max_attempts - 1 {
                    tracing::warn!(
                        attempt = attempt + 1,
                        max_attempts,
                        delay_ms,
                        "[forked_agent] No provider available, retrying..."
                    );
                    // 分段 sleep，每 200ms 检查一次取消信号
                    let mut remaining = delay_ms;
                    let check_interval = 200u64;
                    while remaining > 0 {
                        let sleep_ms = remaining.min(check_interval);
                        tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                        remaining = remaining.saturating_sub(sleep_ms);
                        if abort_token.is_cancelled() {
                            return Err(ForkedAgentError::Aborted(
                                "Operation aborted while waiting for provider".to_string(),
                            ));
                        }
                    }
                    // 指数退避，但不超过最大延迟
                    delay_ms = (delay_ms * 2).min(max_delay_ms);
                }
            }
        }
    }

    Err(ForkedAgentError::NoProviderAvailable)
}

pub(crate) fn extract_forked_usage(usage: &serde_json::Value) -> (u64, u64, u64, u64) {
    fn read_u64(usage: &serde_json::Value, keys: &[&str]) -> u64 {
        keys.iter()
            .find_map(|key| usage.get(*key))
            .and_then(|value| {
                value
                    .as_u64()
                    .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
                    .or_else(|| value.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
            })
            .unwrap_or(0)
    }

    let input = read_u64(
        usage,
        &[
            "input_tokens",
            "inputTokens",
            "prompt_tokens",
            "promptTokens",
            "promptTokenCount",
        ],
    );
    let output = read_u64(
        usage,
        &[
            "output_tokens",
            "outputTokens",
            "completion_tokens",
            "completionTokens",
            "candidatesTokenCount",
        ],
    );
    let cache_read = read_u64(
        usage,
        &["cache_read_input_tokens", "cacheReadInputTokens"],
    );
    let cache_creation = read_u64(
        usage,
        &["cache_creation_input_tokens", "cacheCreationInputTokens"],
    );
    (input, output, cache_read, cache_creation)
}

#[cfg(test)]
mod usage_tests {
    use super::extract_forked_usage;
    use serde_json::json;

    #[test]
    fn extract_forked_usage_supports_normalized_prompt_tokens_and_raw_input_tokens() {
        let anthropic_normalized = json!({
            "prompt_tokens": 120,
            "completion_tokens": 30,
            "cache_read_input_tokens": 90,
            "cache_creation_input_tokens": 15
        });
        assert_eq!(
            extract_forked_usage(&anthropic_normalized),
            (120, 30, 90, 15)
        );

        let raw_usage = json!({
            "input_tokens": 200,
            "output_tokens": 45,
            "cache_read_input_tokens": 150,
            "cache_creation_input_tokens": 0
        });
        assert_eq!(extract_forked_usage(&raw_usage), (200, 45, 150, 0));
    }

    #[test]
    fn build_initial_forked_messages_preserves_single_parent_system_message() {
        use crate::forked::CacheSafeParams;
        use blockcell_core::types::ChatMessage;

        let parent_snapshot = vec![
            ChatMessage::system("frozen parent system prompt with skill cards"),
            ChatMessage::user("deploy the service"),
            ChatMessage::assistant("service deployed"),
        ];
        let cache_safe = CacheSafeParams::new(
            "frozen parent system prompt with skill cards",
            "claude-sonnet-4-5",
        )
        .with_fork_messages(parent_snapshot);
        let prompt_messages = vec![ChatMessage::user("Review the conversation above")];

        let built = super::build_initial_forked_messages(&cache_safe, &prompt_messages, None);
        let system_messages: Vec<_> = built.iter().filter(|m| m.role == "system").collect();
        assert_eq!(system_messages.len(), 1, "must not duplicate system message");
        assert_eq!(
            built[0].content.as_str(),
            Some("frozen parent system prompt with skill cards")
        );
        assert_eq!(built.len(), 4);
        assert_eq!(
            built[3].content.as_str(),
            Some("Review the conversation above")
        );
    }
}
