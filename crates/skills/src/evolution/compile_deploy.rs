use super::*;

impl SkillEvolution {
    /// 编译检查（合并了原 dry_run + shadow_test）
    ///
    /// P0-3: 单一编译步骤，返回 (是否通过, 编译错误信息)
    pub async fn compile_check(&self, evolution_id: &str) -> Result<(bool, Option<String>)> {
        let mut record = self.load_record(evolution_id)?;
        let patch = record
            .patch
            .as_ref()
            .ok_or_else(|| Error::Evolution("No patch for compile check".to_string()))?;

        info!(evolution_id = %evolution_id, "Running compile check");

        let compile_result = match record.context.layout {
            SkillLayout::PromptTool => {
                info!(evolution_id = %evolution_id, "🔨 [compile] PromptTool skill — checking SKILL.md content");
                let content = patch.diff.trim();
                if content.is_empty() {
                    (false, Some("SKILL.md content is empty".to_string()))
                } else if content.chars().count() < crate::evolution::MIN_PROMPT_TOOL_CHARS {
                    (
                        false,
                        Some(format!(
                            "SKILL.md content too short ({} chars, need >= {})",
                            content.chars().count(),
                            crate::evolution::MIN_PROMPT_TOOL_CHARS
                        )),
                    )
                } else {
                    (true, None)
                }
            }
            SkillLayout::LocalScript => {
                info!(evolution_id = %evolution_id, "🔨 [compile] LocalScript skill — 对 patch.diff 临时文件做语法/入口验证");
                // 将 patch.diff 写入临时文件再检查，而非读取磁盘旧文件
                let source_path = record.context.source_path.as_deref().ok_or_else(|| {
                    Error::Evolution("Missing source_path for LocalScript skill".to_string())
                })?;
                let temp_path = make_compile_temp_path(&record.skill_name, source_path);
                let temp_guard = TempFileGuard::new(temp_path);
                std::fs::write(temp_guard.path(), &patch.diff)?;
                self.compile_local_script(temp_guard.path()).await?
            }
            SkillLayout::Hybrid => match record.context.skill_type {
                SkillType::Python => {
                    info!(evolution_id = %evolution_id, "🔨 [compile] Hybrid skill — running Python syntax check for local script asset");
                    let final_script =
                        self.resolve_final_script(&record.skill_name, &patch.diff)?;
                    let temp_guard = TempFileGuard::new(
                        std::env::temp_dir().join(format!("{}_compile.py", record.skill_name)),
                    );
                    std::fs::write(temp_guard.path(), &final_script)?;

                    let output = std::process::Command::new("python3")
                        .args(["-m", "py_compile", temp_guard.path().to_str().unwrap_or("")])
                        .output();
                    // TempFileGuard's Drop handles cleanup even on panic/cancellation

                    match output {
                        Ok(out) if out.status.success() => (true, None),
                        Ok(out) => {
                            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                            (false, Some(format!("Python syntax error:\n{}", stderr)))
                        }
                        Err(e) => {
                            warn!(evolution_id = %evolution_id, "🔨 [compile] python3 not found, cannot verify Python syntax: {}", e);
                            // Fail safe: cannot verify syntax without python3,
                            // rather than silently passing potentially broken code
                            (
                                false,
                                Some(format!("python3 not available for syntax check: {}", e)),
                            )
                        }
                    }
                }
                SkillType::LocalScript => {
                    info!(evolution_id = %evolution_id, "🔨 [compile] Hybrid skill — 对 patch.diff 临时文件验证 local script asset");
                    let source_path = record.context.source_path.as_deref().ok_or_else(|| {
                        Error::Evolution("Missing source_path for LocalScript skill".to_string())
                    })?;
                    let temp_path = make_compile_temp_path(&record.skill_name, source_path);
                    let temp_guard = TempFileGuard::new(temp_path);
                    std::fs::write(temp_guard.path(), &patch.diff)?;
                    self.compile_local_script(temp_guard.path()).await?
                }
                SkillType::Rhai => {
                    info!(evolution_id = %evolution_id, "🔨 [compile] Hybrid skill — falling back to Rhai compilation");
                    let final_script =
                        self.resolve_final_script(&record.skill_name, &patch.diff)?;
                    self.compile_rhai_check(evolution_id, &record.skill_name, &final_script)
                        .await?
                }
                SkillType::PromptOnly => {
                    info!(evolution_id = %evolution_id, "🔨 [compile] Hybrid skill — checking prompt content length");
                    let content = patch.diff.trim();
                    if content.is_empty() {
                        (false, Some("SKILL.md content is empty".to_string()))
                    } else if content.chars().count() < crate::evolution::MIN_PROMPT_TOOL_CHARS {
                        (
                            false,
                            Some(format!(
                                "SKILL.md content too short ({} chars, need >= {})",
                                content.chars().count(),
                                crate::evolution::MIN_PROMPT_TOOL_CHARS
                            )),
                        )
                    } else {
                        (true, None)
                    }
                }
            },
            SkillLayout::RhaiOrchestration => {
                let final_script = self.resolve_final_script(&record.skill_name, &patch.diff)?;
                self.compile_rhai_check(evolution_id, &record.skill_name, &final_script)
                    .await?
            }
        };

        let (passed, compile_error) = compile_result;

        let new_status = if passed {
            EvolutionStatus::CompilePassed
        } else {
            EvolutionStatus::CompileFailed
        };
        info!(
            evolution_id = %evolution_id,
            "🔨 [compile] Status -> {:?}",
            new_status
        );
        record.status = new_status;
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)?;

        Ok((passed, compile_error))
    }

    /// 部署新版本并进入观察窗口
    ///
    /// P1: 简化模型 — 直接部署，进入观察期（无灰度百分比分流）
    pub async fn deploy_and_observe(&self, evolution_id: &str) -> Result<()> {
        let mut record = self.load_record(evolution_id)?;

        // 检查前置条件（兼容旧状态 DryRunPassed/TestPassed）
        if !record.status.is_compile_passed() {
            return Err(Error::Evolution(format!(
                "Cannot deploy: expected status CompilePassed, got {:?}",
                record.status
            )));
        }
        if record.audit.as_ref().map(|a| !a.passed).unwrap_or(true) {
            return Err(Error::Evolution("Audit not passed".to_string()));
        }
        // 契约检查是 BLOCKCELL.md 固定流程的强制环节：结果持久化在 record 上，
        // 无记录的通过结果（如升级前遗留的 CompilePassed 记录）一律拒绝，
        // 由调用方先跑 contract_check 补写。
        if record.contract.as_ref().map(|c| !c.passed).unwrap_or(true) {
            return Err(Error::Evolution("Contract check not passed".to_string()));
        }

        info!(evolution_id = %evolution_id, "Deploying and starting observation");
        info!(
            evolution_id = %evolution_id,
            skill = %record.skill_name,
            "🚀 [deploy] Pre-conditions met, deploying new version"
        );

        // 首次进化前确保 baseline 版本快照存在，否则 rollback 无法恢复原始内容
        // 但对于 staged 新技能导入，主 skills_dir/<skill> 通常还不存在，
        // 此时 create_version() 会 read_dir 不存在的目录导致失败，所以跳过 baseline
        let main_skill_dir = self.skills_dir.join(&record.skill_name);
        if !record.context.staged || main_skill_dir.exists() {
            self.version_manager.ensure_baseline(&record.skill_name)?;
        }

        // 创建新版本（直接写入）
        self.create_new_version(&record)?;

        // 设置观察窗口
        record.observation = Some(ObservationWindow::default());
        record.observation_total_calls = 0;
        record.observation_error_calls = 0;
        record.status = EvolutionStatus::Observing;
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)?;

        info!(
            evolution_id = %evolution_id,
            skill = %record.skill_name,
            "🚀 [deploy] Version deployed, observation window started (60 min)"
        );

        Ok(())
    }

    /// 检查观察窗口状态
    ///
    /// 返回: Ok(Some(true)) = 观察完成可标记成功, Ok(Some(false)) = 需要回滚, Ok(None) = 仍在观察中
    pub fn check_observation(&self, evolution_id: &str, error_rate: f64) -> Result<Option<bool>> {
        let record = self.load_record(evolution_id)?;

        let obs = record
            .observation
            .as_ref()
            .ok_or_else(|| Error::Evolution("No observation window".to_string()))?;

        // 错误率超阈值 → 回滚
        if error_rate > obs.error_threshold {
            return Ok(Some(false));
        }

        // 观察时间到且错误率正常 → 完成
        let elapsed_minutes = (chrono::Utc::now().timestamp() - obs.started_at) / 60;
        if elapsed_minutes >= obs.duration_minutes as i64 {
            return Ok(Some(true));
        }

        // 仍在观察中
        Ok(None)
    }

    /// 标记进化完成
    pub fn mark_completed(&self, evolution_id: &str) -> Result<()> {
        let mut record = self.load_record(evolution_id)?;
        record.status = EvolutionStatus::Completed;
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)?;
        Ok(())
    }

    /// Contract check: validate SKILL.md structure and meta.yaml required fields.
    ///
    /// Runs after compile check passes. This is a deterministic validation that ensures
    /// the generated code doesn't break the skill's contract (required sections, fields).
    /// 对于 PromptTool/PromptOnly 类型，patch.diff 就是新 SKILL.md 内容，直接检查 patch.diff
    /// 而非磁盘旧文件。对于 meta.yaml，检查从 LLM response 提取的 yaml。
    /// Returns (passed, Option<error_description>).
    pub fn contract_check(&self, evolution_id: &str) -> Result<(bool, Option<String>)> {
        let mut record = self.load_record(evolution_id)?;
        let patch = record
            .patch
            .clone()
            .ok_or_else(|| Error::Evolution("No patch for contract check".to_string()))?;

        let skill_root = self.skill_root_dir_for_record(&record);
        let skill_dir = skill_root.join(&record.skill_name);

        let mut issues: Vec<String> = Vec::new();

        // meta.yaml 检查：LLM response 提取的 yaml（新内容）做严格校验——
        // 必须是合法 YAML mapping 且顶层字段在白名单内（trigger_rules_prompt
        // 宣称"禁止 legacy routing/formatting 字段"，此处实际强制）；
        // 无新 meta 时回退检查磁盘旧文件的必备字段。
        let extracted_meta = self.extract_yaml_from_response(&patch.explanation);
        if let Some(ref meta) = extracted_meta {
            issues.extend(validate_meta_yaml(meta));
        } else if let Ok(content) = std::fs::read_to_string(skill_dir.join("meta.yaml")) {
            if !content.contains("name:") && !content.contains("name :") {
                issues.push("meta.yaml: missing required 'name' field".to_string());
            }
            if !content.contains("description:") && !content.contains("description :") {
                issues.push("meta.yaml: missing required 'description' field".to_string());
            }
            if content.trim().is_empty() {
                issues.push("meta.yaml: file is empty".to_string());
            }
        }

        // SKILL.md 检查：对于 PromptTool/PromptOnly 类型，patch.diff 就是新 SKILL.md 内容
        // 对于其他类型（LocalScript/Rhai 等），SKILL.md 不在 patch.diff 中，仍读磁盘旧文件
        let skill_md_content = match record.context.layout {
            SkillLayout::PromptTool => Some(patch.diff.clone()),
            SkillLayout::Hybrid if record.context.skill_type == SkillType::PromptOnly => {
                Some(patch.diff.clone())
            }
            _ => std::fs::read_to_string(skill_dir.join("SKILL.md")).ok(),
        };

        match record.context.layout {
            SkillLayout::PromptTool => {
                let Some(content) = skill_md_content.as_ref() else {
                    issues.push(
                        "SKILL.md: file is missing (required for PromptTool skills)".to_string(),
                    );
                    return self.finish_contract_check(&mut record, issues);
                };

                if content.trim().chars().count() < crate::evolution::MIN_PROMPT_TOOL_CHARS {
                    issues.push(format!(
                        "SKILL.md: content too short ({} chars, minimum 100)",
                        content.trim().len()
                    ));
                }
                if !content.contains('#') {
                    issues.push("SKILL.md: no markdown headings found".to_string());
                }
                if !content.contains("## Shared") && !content.contains("## Shared {#shared}") {
                    issues.push("SKILL.md: missing '## Shared' section".to_string());
                }
                if !content.contains("## Prompt") && !content.contains("## Prompt {#prompt}") {
                    issues.push("SKILL.md: missing '## Prompt' section".to_string());
                }
            }
            // SKILL.md 对所有 layout 均为必查项：LocalScript/RhaiOrchestration 此前
            // 文件缺失即静默跳过（软检查），与其他布局不一致且允许缺文件的技能部署。
            SkillLayout::LocalScript | SkillLayout::RhaiOrchestration => {
                if let Some(content) = skill_md_content.as_ref() {
                    if !content.contains("## Shared") && !content.contains("## Shared {#shared}") {
                        issues.push("SKILL.md: missing '## Shared' section".to_string());
                    }
                    if !content.contains("## Prompt") && !content.contains("## Prompt {#prompt}") {
                        issues.push("SKILL.md: missing '## Prompt' section".to_string());
                    }
                } else {
                    issues.push("SKILL.md: file is missing".to_string());
                }
            }
            SkillLayout::Hybrid => {
                let Some(content) = skill_md_content.as_ref() else {
                    issues
                        .push("SKILL.md: file is missing (required for Hybrid skills)".to_string());
                    return self.finish_contract_check(&mut record, issues);
                };

                if !content.contains("## Shared") && !content.contains("## Shared {#shared}") {
                    issues.push("SKILL.md: missing '## Shared' section".to_string());
                }
                if !content.contains("## Prompt") && !content.contains("## Prompt {#prompt}") {
                    issues.push("SKILL.md: missing '## Prompt' section".to_string());
                }
            }
        }

        // primary_file 存在性检查：对于 PromptTool/PromptOnly 类型，patch.diff 就是新文件内容，
        // 尚未写入磁盘，所以跳过磁盘存在性检查（已在上方通过 skill_md_content 验证）
        let primary_file_is_new_content = matches!(record.context.layout, SkillLayout::PromptTool)
            || (record.context.layout == SkillLayout::Hybrid
                && record.context.skill_type == SkillType::PromptOnly);

        if !primary_file_is_new_content {
            let primary_file = if let Some(source_path) = record.context.source_path.as_ref() {
                skill_dir.join(source_path)
            } else {
                match record.context.layout {
                    SkillLayout::RhaiOrchestration => skill_dir.join("SKILL.rhai"),
                    SkillLayout::LocalScript => skill_dir.join("scripts/skill.sh"),
                    SkillLayout::PromptTool => skill_dir.join("SKILL.md"),
                    SkillLayout::Hybrid => match record.context.skill_type {
                        SkillType::Python => skill_dir.join("SKILL.py"),
                        SkillType::LocalScript => skill_dir.join("scripts/skill.sh"),
                        _ => skill_dir.join("SKILL.md"),
                    },
                }
            };
            if !primary_file.exists() {
                issues.push(format!(
                    "Primary skill file missing: {}",
                    primary_file
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ));
            }
        }

        self.finish_contract_check(&mut record, issues)
    }

    /// 将契约检查结果持久化到 record 并返回 (passed, error)。
    /// 持久化使 `deploy_and_observe` 能独立校验契约已过——防止不经 service
    /// 编排次序的调用方/resume 路径跳过 contract_check 直接部署。
    fn finish_contract_check(
        &self,
        record: &mut EvolutionRecord,
        issues: Vec<String>,
    ) -> Result<(bool, Option<String>)> {
        let passed = issues.is_empty();
        let error = if passed {
            None
        } else {
            Some(issues.join("\n"))
        };

        record.contract = Some(ContractResult {
            passed,
            issues: issues.clone(),
            checked_at: chrono::Utc::now().timestamp(),
        });
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(record)?;

        if passed {
            info!(evolution_id = %record.id, "📋 [contract] Contract check passed");
        } else {
            warn!(
                evolution_id = %record.id,
                issues = issues.len(),
                "📋 [contract] Contract check found {} issue(s)",
                issues.len()
            );
        }

        Ok((passed, error))
    }

    /// 回滚
    pub async fn rollback(&self, evolution_id: &str, reason: &str) -> Result<()> {
        let mut record = self.load_record(evolution_id)?;

        warn!(
            evolution_id = %evolution_id,
            reason = %reason,
            "Rolling back evolution"
        );

        // staged 新技能导入且只有 v1（无 baseline）时，无法回滚到上一版本，
        // 直接删除已 promoted 的主 skills_dir/<skill> 并清理版本历史
        if record.context.staged {
            let versions = self.version_manager.list_versions(&record.skill_name)?;
            if versions.len() < 2 {
                let main_skill_dir = self.skills_dir.join(&record.skill_name);
                if main_skill_dir.exists() {
                    std::fs::remove_dir_all(&main_skill_dir)?;
                    info!(
                        evolution_id = %evolution_id,
                        skill = %record.skill_name,
                        "🧹 [rollback] 删除 staged 新导入的坏 skill（无 baseline 可回滚）"
                    );
                }
                // 清理版本快照和历史文件（它们在主 skill 目录下，已被删除）
                // 如果主目录不存在则版本文件也不存在，无需额外清理
                record.status = EvolutionStatus::RolledBack;
                record.updated_at = chrono::Utc::now().timestamp();
                self.save_record(&record)?;
                return Ok(());
            }
        }

        // 恢复到上一版本
        self.restore_previous_version(&record.skill_name)?;

        record.status = EvolutionStatus::RolledBack;
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)?;

        Ok(())
    }

    // === 辅助方法 ===
}
