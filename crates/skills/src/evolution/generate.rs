use super::*;

impl SkillEvolution {
    /// 生成补丁（调用 LLM）
    pub async fn generate_patch(
        &self,
        evolution_id: &str,
        llm_provider: &dyn LLMProvider,
    ) -> Result<GeneratedPatch> {
        let mut record = self.load_record(evolution_id)?;

        // Precondition: must be in Triggered state (or Generating for retry)
        if !matches!(
            record.status,
            EvolutionStatus::Triggered | EvolutionStatus::Generating
        ) {
            return Err(Error::Evolution(format!(
                "Cannot generate patch: expected status Triggered, got {:?}",
                record.status
            )));
        }

        record.status = EvolutionStatus::Generating;
        self.save_record(&record)?;

        info!(evolution_id = %evolution_id, "Generating patch");

        // 构建 prompt
        let prompt = self.build_generation_prompt(&record.context)?;

        info!(
            evolution_id = %evolution_id,
            prompt_len = prompt.len(),
            "📝 [generate] Prompt built"
        );
        debug!(
            evolution_id = %evolution_id,
            "📝 [generate] Full prompt:\n{}",
            prompt
        );

        // 调用 LLM（带超时保护）
        info!(evolution_id = %evolution_id, "📝 [generate] Calling LLM...");
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(self.llm_timeout_secs),
            llm_provider.generate(&prompt),
        )
        .await
        .map_err(|_| {
            Error::Evolution(format!(
                "LLM call timed out after {} seconds",
                self.llm_timeout_secs
            ))
        })?
        .map_err(|e| Error::Evolution(format!("LLM generation failed: {}", e)))?;

        info!(
            evolution_id = %evolution_id,
            response_len = response.len(),
            "📝 [generate] LLM response received"
        );
        debug!(
            evolution_id = %evolution_id,
            "📝 [generate] Full LLM response:\n{}",
            response
        );

        // 解析 diff
        let diff = self.extract_diff_from_response(&response)?;

        info!(
            evolution_id = %evolution_id,
            diff_len = diff.len(),
            diff_lines = diff.lines().count(),
            "📝 [generate] Extracted diff/script ({} chars, {} lines)",
            diff.len(), diff.lines().count()
        );
        debug!(
            evolution_id = %evolution_id,
            "📝 [generate] Extracted content:\n{}",
            diff
        );

        let patch = GeneratedPatch {
            patch_id: format!("patch_{}", chrono::Utc::now().timestamp()),
            skill_name: record.skill_name.clone(),
            diff,
            explanation: response.clone(),
            generated_at: chrono::Utc::now().timestamp(),
        };

        record.patch = Some(patch.clone());
        record.audit = None;
        record.contract = None;
        record.status = EvolutionStatus::Generated;
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)?;

        info!(
            evolution_id = %evolution_id,
            patch_id = %patch.patch_id,
            "📝 [generate] Patch saved, status -> Generated"
        );

        Ok(patch)
    }

    /// 根据反馈重新生成补丁（用于审计/编译/测试失败后的重试）
    pub async fn regenerate_with_feedback(
        &self,
        evolution_id: &str,
        llm_provider: &dyn LLMProvider,
        feedback: &FeedbackEntry,
    ) -> Result<GeneratedPatch> {
        let mut record = self.load_record(evolution_id)?;
        record.attempt += 1;
        record.feedback_history.push(feedback.clone());
        record.status = EvolutionStatus::Generating;
        self.save_record(&record)?;

        info!(
            evolution_id = %evolution_id,
            attempt = record.attempt,
            feedback_stage = %feedback.stage,
            "🔄 [regenerate] Attempt #{}: regenerating after {} failure",
            record.attempt, feedback.stage
        );

        // 构建修复 prompt
        let prompt = self.build_fix_prompt(&record.context, feedback, &record.feedback_history)?;

        info!(
            evolution_id = %evolution_id,
            prompt_len = prompt.len(),
            "🔄 [regenerate] Fix prompt built"
        );
        debug!(
            evolution_id = %evolution_id,
            "🔄 [regenerate] Full fix prompt:\n{}",
            prompt
        );

        // 调用 LLM（带超时保护）
        info!(evolution_id = %evolution_id, "🔄 [regenerate] Calling LLM...");
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(self.llm_timeout_secs),
            llm_provider.generate(&prompt),
        )
        .await
        .map_err(|_| {
            Error::Evolution(format!(
                "LLM call timed out after {} seconds",
                self.llm_timeout_secs
            ))
        })?
        .map_err(|e| Error::Evolution(format!("LLM generation failed: {}", e)))?;

        info!(
            evolution_id = %evolution_id,
            response_len = response.len(),
            "🔄 [regenerate] LLM response received"
        );
        debug!(
            evolution_id = %evolution_id,
            "🔄 [regenerate] Full LLM response:\n{}",
            response
        );

        // 解析 diff
        let diff = self.extract_diff_from_response(&response)?;

        info!(
            evolution_id = %evolution_id,
            diff_len = diff.len(),
            diff_lines = diff.lines().count(),
            "🔄 [regenerate] Extracted fixed script ({} chars, {} lines)",
            diff.len(), diff.lines().count()
        );
        debug!(
            evolution_id = %evolution_id,
            "🔄 [regenerate] Extracted content:\n{}",
            diff
        );

        let patch = GeneratedPatch {
            patch_id: format!(
                "patch_{}_{}",
                chrono::Utc::now().timestamp(),
                record.attempt
            ),
            skill_name: record.skill_name.clone(),
            diff,
            explanation: response.clone(),
            generated_at: chrono::Utc::now().timestamp(),
        };

        record.patch = Some(patch.clone());
        record.audit = None; // 清除旧审计结果
        record.contract = None; // 清除旧契约检查结果
        record.observation = None; // 清除观察窗口配置，确保状态一致性
        record.status = EvolutionStatus::Generated;
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)?;

        info!(
            evolution_id = %evolution_id,
            patch_id = %patch.patch_id,
            attempt = record.attempt,
            "🔄 [regenerate] New patch saved, status -> Generated"
        );

        Ok(patch)
    }
}
