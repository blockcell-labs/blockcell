use super::*;

impl DreamConsolidator {
    /// 阶段 2: 收集新信号
    ///
    /// 从 session memory 文件中收集信息，提取需要整合的信号。
    /// 优先级：最新的会话 > 旧的会话
    pub(crate) async fn gather(&self) -> Result<Vec<GatheredSignal>, DreamError> {
        tracing::debug!("[dream] Phase 2: Gathering signals");

        let mut signals = Vec::new();
        let sessions_dir = self.config_dir.join("sessions");

        if !fs::try_exists(&sessions_dir).await? {
            return Ok(signals);
        }

        // 收集所有 session memory 文件及其修改时间
        let mut session_files: Vec<(PathBuf, SystemTime)> = Vec::new();
        let mut entries = fs::read_dir(&sessions_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            // 跳过非目录条目（如 .DS_Store）
            if entry.file_type().await.map(|t| !t.is_dir()).unwrap_or(true) {
                continue;
            }
            // 避让正在提取的 Session Memory：
            // 如果目录下存在 .extraction_pending 标记，说明 Layer 3 正在更新 memory.md，
            // 此时读取可能得到旧内容或写入中的半截内容，应跳过。
            // 但如果标记已过期（超过 stale 阈值），说明提取任务已崩溃或被遗弃，
            // 清理过期标记和对应 journal 后继续读取当前 memory.md。
            let pending_marker = entry.path().join(".extraction_pending");
            if fs::try_exists(&pending_marker).await.unwrap_or(false) {
                // stale 阈值：10x Layer3 默认 extraction_stale_threshold (60s * 10 = 600s)
                // 此常量与 agent 侧 Layer3Config::extraction_stale_threshold_ms (默认 60000ms) 关联。
                // scheduler crate 无法直接引用 agent 配置，故使用关联常量。
                // 10x 裕量确保：即使 LLM 提取耗时较长，也不会被 Dream 误清。
                const EXTRACTION_STALE_THRESHOLD_SECS: u64 = 600;
                let is_mtime_stale = fs::metadata(&pending_marker)
                    .await
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|mtime| mtime.elapsed().ok())
                    .map(|elapsed| elapsed.as_secs() >= EXTRACTION_STALE_THRESHOLD_SECS)
                    .unwrap_or(true); // 无法读取 mtime 视为过期

                if is_mtime_stale {
                    // marker 已过期，但在清理前先检查 journal 的 owner_pid/started_at
                    // 避免 Dream 误删长耗时任务的 marker
                    let journal_path = entry.path().join(".extraction_journal");
                    let should_clean = if journal_path.exists() {
                        // 尝试读取 journal 判断任务是否仍在运行
                        match fs::read_to_string(&journal_path).await {
                            Ok(content) => {
                                if let Ok(journal) =
                                    serde_json::from_str::<serde_json::Value>(&content)
                                {
                                    // 检查 owner_pid：如果进程仍存活，任务可能在运行
                                    if let Some(owner_pid) =
                                        journal.get("owner_pid").and_then(|v| v.as_u64())
                                    {
                                        let pid = owner_pid as u32;
                                        if pid == std::process::id() {
                                            // 同一进程：journal 不是孤儿
                                            tracing::debug!(
                                                session_dir = %entry.path().display(),
                                                "[dream] journal owner 是当前进程，保留 marker"
                                            );
                                            false
                                        } else {
                                            // Unix: 检查 /proc/{pid} 是否存在
                                            #[cfg(unix)]
                                            {
                                                if std::path::Path::new(&format!("/proc/{}", pid))
                                                    .exists()
                                                {
                                                    tracing::debug!(
                                                        session_dir = %entry.path().display(),
                                                        pid,
                                                        "[dream] journal owner 进程仍存活，保留 marker"
                                                    );
                                                    false
                                                } else {
                                                    // 进程已死，使用 started_at + 3x 阈值做最终判断
                                                    is_journal_started_at_expired(
                                                        &journal,
                                                        EXTRACTION_STALE_THRESHOLD_SECS * 3,
                                                    )
                                                }
                                            }
                                            #[cfg(not(unix))]
                                            {
                                                // Windows 下无法无依赖检查 PID 存活，
                                                // 使用 started_at + 3x 阈值做保守判断
                                                let _ = pid;
                                                is_journal_started_at_expired(
                                                    &journal,
                                                    EXTRACTION_STALE_THRESHOLD_SECS * 3,
                                                )
                                            }
                                        }
                                    } else {
                                        // 无 owner_pid（旧格式），使用 started_at + 3x 阈值
                                        is_journal_started_at_expired(
                                            &journal,
                                            EXTRACTION_STALE_THRESHOLD_SECS * 3,
                                        )
                                    }
                                } else {
                                    true // 无法解析 JSON，清理
                                }
                            }
                            Err(_) => true, // 无法读取 journal，清理
                        }
                    } else {
                        true // 无 journal，清理
                    };

                    if should_clean {
                        tracing::warn!(
                            session_dir = %entry.path().display(),
                            "[dream] 清理过期的 extraction pending marker 和 journal（journal 确认可清理）"
                        );
                        let _ = fs::remove_file(&pending_marker).await;
                        if journal_path.exists() {
                            let _ = fs::remove_file(&journal_path).await;
                        }
                        // 清理后继续读取当前 memory.md
                    } else {
                        tracing::debug!(
                            session_dir = %entry.path().display(),
                            "[dream] marker 虽然过期但 journal 显示任务仍在运行，跳过"
                        );
                        continue;
                    }
                } else {
                    tracing::debug!(
                        session_dir = %entry.path().display(),
                        "[dream] 跳过正在提取 Session Memory 的会话（marker 未过期）"
                    );
                    continue;
                }
            }
            let memory_file = entry.path().join("memory.md");
            if fs::try_exists(&memory_file).await? {
                if let Ok(metadata) = fs::metadata(&memory_file).await {
                    if let Ok(modified) = metadata.modified() {
                        session_files.push((memory_file, modified));
                    }
                }
            }
        }

        // 按修改时间降序排序（最新的优先）
        session_files.sort_by_key(|b| std::cmp::Reverse(b.1));

        // 限制处理数量
        let files_to_process = session_files.iter().take(MAX_SESSIONS_TO_PROCESS);

        for (memory_file, modified_time) in files_to_process {
            match fs::read_to_string(memory_file).await {
                Ok(content) => {
                    // 提取信号
                    let signal = self.extract_signals_from_memory(&content, *modified_time);
                    if !signal.is_empty() {
                        tracing::trace!(
                            path = %memory_file.display(),
                            signal_count = signal.len(),
                            "extracted signals from session memory"
                        );
                        signals.extend(signal);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        path = %memory_file.display(),
                        error = %e,
                        "failed to read session memory"
                    );
                }
            }
        }

        tracing::info!(
            total_signals = signals.len(),
            "[dream] Phase 2: Gathered {} signals",
            signals.len()
        );

        Ok(signals)
    }

    /// 从 session memory 内容中提取信号
    ///
    /// 支持一级标题 (`# `) 和二级标题 (`## `)，与 Session Memory 10-section 模板兼容。
    /// 同时向后兼容旧格式（仅含二级标题的文件）。
    pub(crate) fn extract_signals_from_memory(
        &self,
        content: &str,
        modified_time: SystemTime,
    ) -> Vec<GatheredSignal> {
        let mut signals = Vec::new();

        // 按 markdown 标题分割：支持 `# `（一级）和 `## `（二级）
        // 使用行扫描方式，识别每行开头的 heading marker
        let sections = split_by_markdown_headings(content);

        for section in &sections {
            let section = section.trim();
            if section.is_empty() {
                continue;
            }

            // 提取章节标题（第一行，去除 heading marker）
            let title_end = section.find('\n').unwrap_or(section.len());
            let raw_title = section[..title_end].trim();
            // 去除 heading marker（`# ` 或 `## `）
            let title = raw_title
                .trim_start_matches("# ")
                .trim_start_matches("## ")
                .trim();

            // 提取章节内容（跳过标题行和换行符）
            let section_content = if title_end < section.len() {
                section[title_end..].trim()
            } else {
                ""
            };

            // A section whose body is just `*title*` carries no signal. Compare
            // without allocating a temporary format string per section.
            let title_only = section_content.len() == title.len() + 2
                && section_content.starts_with('*')
                && section_content.ends_with('*')
                && &section_content[1..section_content.len() - 1] == title;
            if !section_content.is_empty() && !title_only {
                // 计算内容的重要性分数
                let importance = self.calculate_signal_importance(title, section_content);

                if importance > 0 {
                    signals.push(GatheredSignal {
                        title: title.to_string(),
                        content: section_content.to_string(),
                        importance,
                        source_time: modified_time,
                    });
                }
            }
        }

        signals
    }

    /// 计算信号的重要性分数 (0-10)
    ///
    /// 基于 Session Memory 10-section 模板的实际章节标题：
    /// - Session Title, Current State, Task specification, Files and Functions,
    ///   Workflow, Errors & Corrections, Codebase and System Documentation,
    ///   Learnings, Key results, Worklog
    pub(crate) fn calculate_signal_importance(&self, title: &str, content: &str) -> u8 {
        // 归一化标题用于匹配（trim + 大小写不敏感）
        let normalized = title.trim().to_lowercase();

        // 高重要性章节：直接影响后续工作的关键信息
        let high_priority = [
            "current state",
            "errors & corrections",
            "errors and corrections",
        ];
        // 中重要性章节：任务定义、文件、关键结果
        let medium_priority = [
            "task specification",
            "files and functions",
            "key results",
            "decisions & preferences",
            "artifacts & files",
        ];
        // 低重要性章节：工作流和工作日志
        let low_priority = ["workflow", "worklog", "work log"];

        if high_priority.iter().any(|t| normalized.contains(t)) {
            8
        } else if medium_priority.iter().any(|t| normalized.contains(t)) {
            5
        } else if low_priority.iter().any(|t| normalized.contains(t)) {
            2
        } else {
            // 根据内容长度判断
            let content_len = content.len();
            if content_len > 500 {
                4
            } else if content_len > 200 {
                3
            } else {
                1
            }
        }
    }
}
