use super::*;

impl DreamConsolidator {
    /// 阶段 3: 整合知识
    pub(crate) async fn consolidate(
        &self,
        signals: &[GatheredSignal],
        memory_dir: &Path,
    ) -> Result<(), DreamError> {
        tracing::debug!(
            signal_count = signals.len(),
            "[dream] Phase 3: Consolidating knowledge"
        );

        // 检查 provider_pool
        let provider_pool = self
            .provider_pool
            .as_ref()
            .ok_or(DreamError::NoProviderPool)?;

        // 构建整合提示（包含收集的信号）
        fs::create_dir_all(&memory_dir).await?;
        let prompt = self.build_consolidation_prompt(memory_dir, signals);

        // 创建工具权限检查
        let can_use_tool = create_dream_can_use_tool(memory_dir);

        // 创建 CacheSafeParams（使用默认系统提示）
        let cache_safe_params = CacheSafeParams::default();

        // 熔断器检查：如果熔断器打开，跳过整合
        let cb = get_dream_circuit_breaker();
        if !cb.allow() {
            tracing::warn!("[dream] Circuit breaker is open, skipping consolidation");
            return Err(DreamError::CircuitBreakerOpen);
        }

        // 运行 Forked Agent 进行整合
        // 使用 Builder 模式构建参数
        let params = ForkedAgentParams::builder()
            .provider_pool(provider_pool.clone())
            .prompt_messages(vec![ChatMessage::user(&prompt)])
            .cache_safe_params(cache_safe_params)
            .can_use_tool(can_use_tool)
            // 将执行层也限制在记忆目录内，避免无 working_dir 时接受任意绝对路径。
            .working_dir(memory_dir.to_path_buf())
            .query_source("auto_dream")
            .fork_label("auto_dream")
            .max_turns(DREAM_FORKED_AGENT_MAX_TURNS)
            .skip_transcript(true)
            .tool_schemas(build_forked_tool_schemas(&[
                "exec".to_string(),
                "shell".to_string(),
            ]))
            .build()
            .map_err(|e| {
                DreamError::ConsolidationFailed(format!("Failed to build params: {}", e))
            })?;

        let result = run_forked_agent(params).await;

        match result {
            Ok(agent_result) => {
                // 检查工具调用失败和 max_turns 截断：二者都不应视为 consolidation 成功。
                if let Err(reason) = validate_dream_agent_result(&agent_result) {
                    // 熔断器记录失败
                    cb.record_failure();

                    tracing::error!(
                        files_modified = ?agent_result.files_modified,
                        response = ?agent_result.final_content,
                        truncated = agent_result.truncated,
                        had_tool_error = agent_result.had_tool_error,
                        reason = %reason,
                        "[dream] Forked Agent did not complete consolidation"
                    );
                    return Err(DreamError::ConsolidationFailed(reason));
                }

                // 熔断器记录成功
                cb.record_success();

                tracing::info!(
                    input_tokens = agent_result.total_usage.input_tokens,
                    output_tokens = agent_result.total_usage.output_tokens,
                    cache_hit_rate = agent_result.total_usage.cache_hit_rate(),
                    "[dream] Forked Agent completed"
                );
                Ok(())
            }
            Err(e) => {
                // 熔断器记录失败
                cb.record_failure();

                tracing::error!(error = %e, "[dream] Forked Agent failed");
                Err(DreamError::ConsolidationFailed(format!("{}", e)))
            }
        }
    }

    /// 构建整合提示
    pub(crate) fn build_consolidation_prompt(
        &self,
        _memory_dir: &Path,
        signals: &[GatheredSignal],
    ) -> String {
        // 按重要性排序信号
        let mut sorted_signals = signals.to_vec();
        sorted_signals.sort_by_key(|b| std::cmp::Reverse(b.importance));

        // 构建信号摘要
        let signals_section = if sorted_signals.is_empty() {
            "无新信号需要整合。\n".to_string()
        } else {
            let mut section = String::new();
            section.push_str("以下是从最近会话中收集的新信号（按重要性排序）：\n\n");

            for signal in sorted_signals.iter().take(20) {
                // 限制最多20个信号
                section.push_str(&format!(
                    "### {} (重要性: {}/10)\n{}\n\n",
                    signal.title, signal.importance, signal.content
                ));
            }

            section
        };

        format!(
            r#"# Dream: Memory Consolidation

## 任务
对记忆文件进行回顾、整理、更新和索引优化。

## 记忆目录
当前工作目录就是记忆目录。所有文件工具都必须使用相对路径，不要复制或猜测绝对路径，也不要加 `memory/` 前缀。
示例：
- list_dir: path="."
- read_file: file_path="reference.md"
- glob: path="."
- grep: path="reference.md"（只能对已确认存在的具体文件使用，不能对 "." 目录使用）
- edit_file/write_file: file_path="reference.md" 或其他现有/需要创建的 .md 相对路径
- 只能读取 list_dir/glob 已确认存在的文件；不要猜测 memory.md、index.md 等入口文件名
- 如果 read_file 返回 File not found，不要重试该路径，回到 list_dir/glob 结果继续
- 完成必要写入后立即返回最终简短总结，不要继续探索
- 错误示例: file_path="memory/reference.md", path="../", file_path="/absolute/path/reference.md", file_path="memory.md"（除非已列出存在）, grep path="."

## 收集的新信号
{}

## 执行阶段

### Phase 1 — Orient (定位)
- `ls` 记忆目录查看现有内容
- 读取入口文件理解当前索引
- 浏览现有主题文件避免重复创建

### Phase 2 — Gather recent signal (收集新信号)
优先级排序：
1. Daily logs（日志流）
2. 已过时的记忆（需要修正）
3. Transcript search（特定上下文搜索）

### Phase 3 — Consolidate (整合)
- 合并新信号到现有主题文件
- 将相对日期转换为绝对日期
- 删除被证伪的事实
- 更新过时信息

### Phase 4 — Prune and index (修剪和索引)
- 更新入口文件（保持 < 100 行, < 25KB）
- 移除过时指针
- 添加新指针
- 优化索引结构

## 工具限制
- 只读工具: read_file/list_dir/grep/glob 使用相对路径，路径会自动限定在当前记忆目录内
- Shell/Exec: 默认不提供；如被调用，也必须限定在记忆目录内
- Edit/Write: 仅限记忆目录内的 .md 文件；不要创建测试文件或临时文件

## 注意事项
- 不要删除现有记忆，除非确认过时
- 合并相似条目
- 保持信息密度
"#,
            signals_section
        )
    }
}
