use super::*;

impl SkillEvolution {
    pub fn new(skills_dir: PathBuf, llm_timeout_secs: u64) -> Self {
        let evolution_db = skills_dir
            .parent()
            .unwrap_or(Path::new("."))
            .join("evolution.db");
        let version_manager = VersionManager::new(skills_dir.clone());

        Self {
            skills_dir,
            evolution_db,
            version_manager,
            llm_timeout_secs,
        }
    }

    pub fn version_manager(&self) -> &VersionManager {
        &self.version_manager
    }

    /// Get the skills directory path.
    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }

    pub(crate) fn is_openclaw_import_description(description: &str) -> bool {
        description.contains("Convert the following OpenClaw-compatible skill into a Blockcell")
    }

    pub(crate) fn trigger_rules_prompt() -> &'static str {
        "## meta.yaml rules\n\
- Keep `meta.yaml` minimal.\n\
- Required fields: `name`, `description`.\n\
- Optional fields: `tools`, `requires`, `permissions`, `fallback`.\n\
- `tools` must be a short YAML string list of ordinary host tools actually used by the skill.\n\
- Do NOT include `exec_local` in `tools`; local execution belongs in `SKILL.md` instructions.\n\
- `requires` may contain `bins` and `env` only when there is a real local dependency.\n\
- `permissions` should be an empty list unless the skill truly needs explicit permission declarations.\n\
- `fallback` is optional; when present, keep it simple with a `strategy` and user-facing `message`.\n\
- Do NOT generate any legacy routing or formatting fields.\n\n"
    }

    /// Get the evolution records directory path.
    pub fn records_dir(&self) -> PathBuf {
        self.evolution_db
            .parent()
            .unwrap()
            .join("evolution_records")
    }

    pub(crate) fn skill_root_dir_for_record(&self, record: &EvolutionRecord) -> PathBuf {
        if record.context.staged {
            if let Some(ref dir) = record.context.staging_skills_dir {
                let p = PathBuf::from(dir);
                if p.is_absolute() {
                    // Validate: staging dir must be within the workspace directory tree
                    // (the parent of skills_dir) to prevent path traversal attacks.
                    // This allows sibling directories like workspace/import_staging/skills
                    // while rejecting arbitrary paths outside the workspace.
                    if let Ok(canonical_staging) = p.canonicalize() {
                        if let Ok(canonical_skills) = self.skills_dir.canonicalize() {
                            let canonical_workspace = canonical_skills.parent();
                            if let Some(workspace) = canonical_workspace {
                                if canonical_staging.starts_with(workspace) {
                                    return p;
                                }
                            }
                            // Fallback: also accept if staging is within skills_dir itself
                            if canonical_staging.starts_with(&canonical_skills) {
                                return p;
                            }
                        }
                        warn!(
                            path = %dir,
                            "staging_skills_dir is outside workspace directory tree, ignoring"
                        );
                    } else {
                        warn!(
                            path = %dir,
                            "staging_skills_dir canonicalize failed — path may not exist, ignoring"
                        );
                    }
                }
            }
        }
        self.skills_dir.clone()
    }

    /// Load the current skill source for a skill (returns None if not found).
    /// Checks SKILL.rhai, SKILL.py, and SKILL.md in that order.
    pub fn load_skill_source(&self, skill_name: &str) -> Result<Option<String>> {
        let skill_dir = self.skills_dir.join(skill_name);
        for filename in &["SKILL.rhai", "SKILL.py", "SKILL.md"] {
            let path = skill_dir.join(filename);
            if path.exists() {
                return Ok(std::fs::read_to_string(&path).ok());
            }
        }
        Ok(None)
    }

    /// 触发技能进化
    pub async fn trigger_evolution(&self, context: EvolutionContext) -> Result<String> {
        validate_skill_name(&context.skill_name)?;
        // Milliseconds alone can collide for two triggers of the same skill within
        // one tick (both would write the same record file). Append a process-local
        // sequence suffix, matching core_evolution's id scheme.
        let seq = RECORD_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let evolution_id = format!(
            "evo_{}_{:x}_{}",
            context.skill_name,
            chrono::Utc::now().timestamp_millis(),
            seq
        );

        info!(
            skill = %context.skill_name,
            evolution_id = %evolution_id,
            "Triggering skill evolution"
        );

        let record = EvolutionRecord {
            id: evolution_id.clone(),
            skill_name: context.skill_name.clone(),
            context,
            patch: None,
            audit: None,
            contract: None,
            shadow_test: None,
            observation: None,
            observation_total_calls: 0,
            observation_error_calls: 0,
            rollout: None,
            status: EvolutionStatus::Triggered,
            attempt: 1,
            feedback_history: Vec::new(),
            created_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
        };

        self.save_record(&record)?;
        Ok(evolution_id)
    }
}
