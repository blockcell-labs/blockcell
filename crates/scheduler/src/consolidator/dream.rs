use super::*;

impl DreamConsolidator {
    /// 执行梦境整合
    ///
    /// timeout_secs: forked-agent 整合阶段的超时时间（秒）。
    /// 真实 memory commit 和最终状态保存不受该 timeout 取消，以保证能显式完成或回滚。
    pub async fn dream(&mut self, timeout_secs: u64) -> Result<(), DreamError> {
        // 获取锁
        self.acquire_lock().await?;
        let memory_dir = self.config_dir.join("memory");
        if let Err(e) =
            recover_dream_commit_backups(&self.config_dir, &memory_dir, self.state.is_consolidating)
                .await
        {
            let _ = self.release_lock().await;
            return Err(e);
        }

        // 记录 Layer 6 dream_started 事件
        let sessions_count = self.state.current_session_count;
        let hours_since_last = self
            .state
            .last_consolidation_time
            .map(|t| {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                (now.saturating_sub(t)) / 3600
            })
            .unwrap_or(24);
        memory_event!(layer6, dream_started, sessions_count, hours_since_last);

        // 标记开始（同时记录开始时间戳，用于 stale 检测）
        self.state.is_consolidating = true;
        self.state.consolidating_started_at = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        );
        if let Err(e) = self.state.save(&self.config_dir).await {
            // 保存失败，重置状态并释放锁
            self.state.is_consolidating = false;
            self.state.consolidating_started_at = None;
            let _ = self.release_lock().await;
            return Err(DreamError::Io(e));
        }

        // 在整合开始前保存当前会话数快照，用于成功后推进 last_session_count。
        // 避免整合期间新增的会话被误标为已整合（它们未必被本次 gather/prune 处理）。
        let processed_session_count = self.state.current_session_count;

        let start_time = Instant::now();

        let mut stats = DreamStats::default();
        let mut staging_root: Option<PathBuf> = None;
        let mut pending_memory_commit: Option<StagedMemoryCommit> = None;

        // state 已标记为 is_consolidating=true。这里之后所有错误都必须汇总进 result，
        // 不能用裸 `?` 早退，否则会绕过底部状态清理和 dream lock 释放。
        let result = async {
            fs::create_dir_all(&memory_dir).await?;
            let pre_memory_snapshot = snapshot_memory_tree(&memory_dir).await?;
            let staging = prepare_dream_staging(&self.config_dir, &memory_dir).await?;
            staging_root = Some(staging.root.clone());
            let staging_memory_dir = staging.memory_dir.clone();

            // timeout 只包住可重跑的整合阶段。真实 memory commit 必须在 timeout 外完成，
            // 否则 timeout 取消 future 时可能跳过 rollback，留下半提交文件。
            tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
                self.orient().await?;
                memory_event!(layer6, phase_completed, "orient");
                let signals = self.gather().await?;
                memory_event!(layer6, phase_completed, "gather");
                self.consolidate(&signals, &staging_memory_dir).await?;
                validate_staged_memory(&staging_memory_dir).await?;
                memory_event!(layer6, phase_completed, "consolidate");
                Ok::<(), DreamError>(())
            })
            .await
            .map_err(|_| {
                tracing::error!(
                    timeout_secs,
                    "[dream] Consolidation timed out, executing cleanup"
                );
                DreamError::Timeout(timeout_secs)
            })??;

            // 真实 memory commit 不放进 timeout：进入提交阶段后必须显式成功或回滚。
            // 备份会保留到最终 dream state 成功落盘之后，避免 state 保存失败时留下已提交 memory。
            let commit = commit_staged_memory_transaction(
                &memory_dir,
                &pre_memory_snapshot,
                &staging_memory_dir,
            )
            .await?;
            stats = commit.stats.clone();
            pending_memory_commit = Some(commit);
            Ok::<(), DreamError>(())
        }
        .await;

        if let Some(staging_root) = &staging_root {
            if let Err(e) = fs::remove_dir_all(staging_root).await {
                tracing::warn!(
                    path = %staging_root.display(),
                    error = %e,
                    "[dream] Failed to clean up dream staging directory"
                );
            }
        }

        // 清理：无论成功、失败或超时，都要释放锁和重置标记
        self.state.is_consolidating = false;
        self.state.consolidating_started_at = None;

        // 只有成功时才推进时间门和会话门，失败/超时保留原值以便重试
        if result.is_ok() {
            let now_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            apply_successful_dream_state(
                &mut self.state,
                &stats,
                processed_session_count,
                now_secs,
            );
        }

        // 最终保存：在同一个跨进程锁保护下完成 read-merge-write，
        // 防止 agent 在 load 和 save 之间递增 session_count 并被覆盖。
        //
        // 关键：is_consolidating=false 必须落盘，否则后续 gate 永远 LockGateFailed。
        // 获取锁失败或 save_unlocked 失败时，必须重试或返回错误，
        // 不能让调用方看到成功但磁盘上仍为 is_consolidating=true。
        {
            let state_lock_path = self
                .config_dir
                .join(DREAM_STATE_FILE)
                .with_extension("json.lock");

            // 重试获取状态锁，最多 3 次（间隔递增），确保 is_consolidating=false 能落盘
            let state_lock_guard = {
                // CrossProcessLock::acquire 内部是 thread::sleep 重试循环（最坏 ~6s），
                // 必须放到 blocking 线程，否则会阻塞 tokio worker。
                let mut guard_result = crate::blocking::run_blocking_io({
                    let path = state_lock_path.clone();
                    move || CrossProcessLock::acquire(&path)
                })
                .await;
                let mut retry_count = 0;
                const MAX_STATE_LOCK_RETRIES: u32 = 3;
                while let Err(e) = guard_result {
                    retry_count += 1;
                    if retry_count > MAX_STATE_LOCK_RETRIES {
                        tracing::error!(
                            error = %e,
                            retries = retry_count,
                            "[dream] 获取状态锁失败（已重试 {retry_count} 次），is_consolidating=false 无法落盘，返回错误"
                        );
                        rollback_pending_memory_commit(&mut pending_memory_commit).await;
                        // 释放 dream lock
                        if let Err(e) = self.release_lock().await {
                            tracing::warn!(error = %e, "[dream] Failed to release lock");
                        }
                        // 返回错误而非成功：调用方必须知道状态未持久化
                        return Err(DreamError::Io(std::io::Error::new(
                            std::io::ErrorKind::WouldBlock,
                            format!(
                                "获取状态锁失败（重试 {} 次），is_consolidating=false 无法落盘: {}",
                                retry_count, e
                            ),
                        )));
                    }
                    tracing::warn!(
                        error = %e,
                        retry = retry_count,
                        "[dream] 获取状态锁失败，重试中"
                    );
                    // 递增等待：100ms, 200ms, 300ms
                    tokio::time::sleep(std::time::Duration::from_millis(100 * retry_count as u64))
                        .await;
                    guard_result = crate::blocking::run_blocking_io({
                        let path = state_lock_path.clone();
                        move || CrossProcessLock::acquire(&path)
                    })
                    .await;
                }
                let guard = match guard_result {
                    Ok(g) => g,
                    Err(e) => {
                        return Err(DreamError::Io(std::io::Error::other(format!(
                            "获取 dream 状态锁失败: {}",
                            e
                        ))))
                    }
                };
                guard
            };

            // 在锁内重新读取磁盘上的 current_session_count，
            // 合并整合期间 agent 递增的增量。
            // current_session_count 可以 merge 磁盘较大值（反映真实总数），
            // 但 last_session_count 只推进到整合开始前的快照值，
            // 防止整合期间新增的会话被误标为已整合。
            if result.is_ok() {
                if let Ok(disk) = DreamState::load(&self.config_dir).await {
                    self.state.current_session_count =
                        std::cmp::max(self.state.current_session_count, disk.current_session_count);
                    // last_session_count 已在上方设为 processed_session_count，
                    // 不再使用 merged current_session_count 更新它
                }
            }

            // 保存最终状态（使用 save_unlocked 避免重复抢锁）
            // 失败时重试最多 2 次，确保 is_consolidating=false 落盘
            let mut save_retry_count = 0;
            const MAX_SAVE_RETRIES: u32 = 2;
            loop {
                match self.state.save_unlocked(&self.config_dir).await {
                    Ok(()) => break,
                    Err(e) => {
                        save_retry_count += 1;
                        if save_retry_count > MAX_SAVE_RETRIES {
                            tracing::error!(
                                error = %e,
                                retries = save_retry_count,
                                "[dream] 最终状态保存失败（已重试 {save_retry_count} 次），is_consolidating=false 未落盘，返回错误"
                            );
                            // 状态锁在 drop 时自动释放
                            drop(state_lock_guard);
                            rollback_pending_memory_commit(&mut pending_memory_commit).await;
                            // 释放 dream lock
                            if let Err(e) = self.release_lock().await {
                                tracing::warn!(error = %e, "[dream] Failed to release lock");
                            }
                            // 返回错误而非成功：调用方必须知道 is_consolidating=false 未落盘
                            return Err(DreamError::Io(e));
                        }
                        tracing::warn!(
                            error = %e,
                            retry = save_retry_count,
                            "[dream] 最终状态保存失败，重试中"
                        );
                    }
                }
            }

            // 状态锁在 _state_lock_guard drop 时自动释放
            drop(state_lock_guard);
        }

        if let Some(commit) = pending_memory_commit.take() {
            commit.finalize().await;
        }

        if result.is_ok() {
            match self.prune().await {
                Ok(prune_stats) => {
                    memory_event!(layer6, phase_completed, "prune");
                    stats.sessions_pruned = prune_stats.sessions_pruned;
                    stats.sessions_processed = prune_stats.sessions_processed;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "[dream] Prune failed after successful consolidation; continuing"
                    );
                }
            }
        }

        // 释放锁（失败时记录警告但继续）
        if let Err(e) = self.release_lock().await {
            tracing::warn!(
                error = %e,
                "[dream] Failed to release lock"
            );
        }

        let elapsed = start_time.elapsed();
        match &result {
            Ok(()) => {
                // 记录 Layer 6 dream_finished 事件（成功，传递实际统计数据）
                memory_event!(
                    layer6,
                    dream_finished,
                    stats.memories_created,
                    stats.memories_updated,
                    stats.memories_deleted,
                    stats.sessions_pruned,
                    stats.sessions_processed
                );
                tracing::info!(
                    elapsed_ms = elapsed.as_millis() as u64,
                    consolidation_count = self.state.consolidation_count,
                    memories_created = stats.memories_created,
                    memories_updated = stats.memories_updated,
                    sessions_pruned = stats.sessions_pruned,
                    "[dream] consolidation completed"
                );
            }
            Err(e) => {
                memory_event!(layer6, dream_failed, e.to_string());
                tracing::error!(
                    elapsed_ms = elapsed.as_millis() as u64,
                    error = %e,
                    "[dream] consolidation failed"
                );
            }
        }

        result
    }
}
