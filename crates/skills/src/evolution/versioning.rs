use super::*;

impl SkillEvolution {
    pub fn record_observation_call(&self, evolution_id: &str, is_error: bool) -> Result<()> {
        let records_dir = self.records_dir();
        let _guard = crate::file_owner_lock::FileOwnerLock::acquire(
            &records_dir,
            "evolution-record",
            evolution_id,
        )?;
        let mut record = self.load_record(evolution_id)?;
        if *record.status.normalize() != EvolutionStatus::Observing {
            return Ok(());
        }
        record.observation_total_calls += 1;
        if is_error {
            record.observation_error_calls += 1;
        }
        record.updated_at = chrono::Utc::now().timestamp();
        self.save_record(&record)
    }

    /// P0-2: create_new_version 直接写入完整脚本（不再 apply diff）
    ///
    /// 原子性保证：先记录当前版本号，写入新内容后创建 v2 快照。
    /// 若快照创建失败（IO 错误/崩溃），回退到写入前的版本恢复磁盘内容。
    ///
    /// 部署事务：写入、promote、版本历史迁移、快照创建任一步骤失败，
    /// 都统一恢复旧版本或 .bak，避免 partial 状态留在磁盘。
    pub(crate) fn create_new_version(&self, record: &EvolutionRecord) -> Result<()> {
        let patch = record
            .patch
            .as_ref()
            .ok_or_else(|| Error::Evolution("No patch to deploy".to_string()))?;

        // 记住写入前的版本号，用于失败时恢复
        let pre_write_version = self
            .version_manager
            .get_current_version(&record.skill_name)
            .ok();

        let skill_root = self.skill_root_dir_for_record(record);
        let staged_skill_dir = skill_root.join(&record.skill_name);

        // Ensure skill directory exists (for new skills)
        std::fs::create_dir_all(&staged_skill_dir)?;

        // PromptOnly 写入 SKILL.md，Python 写入 SKILL.py，LocalScript 写入原始脚本，Rhai 技能写入 SKILL.rhai
        let skill_path = if let Some(source_path) = record.context.source_path.as_ref() {
            staged_skill_dir.join(source_path)
        } else {
            match record.context.skill_type {
                SkillType::PromptOnly => staged_skill_dir.join("SKILL.md"),
                SkillType::Python => staged_skill_dir.join("SKILL.py"),
                SkillType::LocalScript => staged_skill_dir.join("scripts/skill.sh"),
                SkillType::Rhai => staged_skill_dir.join("SKILL.rhai"),
            }
        };

        if let Some(parent) = skill_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // 直接写入完整内容（所有生成都是完整文件）
        // 写入失败时尝试恢复：如果是非 staged 技能，pre_write_version 对应的文件
        // 可能已被 truncate，但 write 失败意味着内容未完整写入，恢复也无从下手。
        // 因此写入失败直接返回——此时 skill 文件可能损坏，但至少不会留下
        // 错误的版本快照。staged 技能写入的是 staging 目录，不影响主目录。
        if let Err(e) = std::fs::write(&skill_path, &patch.diff) {
            // 非 staged：尝试用 pre_write_version 恢复磁盘文件
            if !record.context.staged {
                if let Some(ref prev_ver) = pre_write_version {
                    warn!(
                        skill = %record.skill_name,
                        error = %e,
                        pre_write_version = %prev_ver,
                        "Write to skill file failed, attempting to restore pre-write version"
                    );
                    if let Err(restore_err) = self
                        .version_manager
                        .switch_to_version(&record.skill_name, prev_ver)
                    {
                        warn!(
                            skill = %record.skill_name,
                            error = %restore_err,
                            "Failed to restore pre-write version after write failure"
                        );
                    }
                }
            }
            return Err(Error::from(e));
        }

        if let Some(meta) = self.extract_yaml_from_response(&patch.explanation) {
            let meta_path = staged_skill_dir.join("meta.yaml");
            let meta_issues = validate_meta_yaml(&meta);
            if meta_issues.is_empty() {
                if let Err(e) = std::fs::write(&meta_path, &meta) {
                    warn!(
                        skill = %record.skill_name,
                        error = %e,
                        "Failed to write meta.yaml to staged skill"
                    );
                }
            } else {
                warn!(
                    skill = %record.skill_name,
                    issues = %meta_issues.join("; "),
                    "Refusing to write invalid meta.yaml from LLM response"
                );
            }
        }

        // If this is a staged external skill, promote it into the main skills dir now.
        let mut version_history_migrated = false;
        if record.context.staged {
            let dest_skill_dir = self.skills_dir.join(&record.skill_name);
            std::fs::create_dir_all(&self.skills_dir)?;

            // Atomic promotion: rename old → .bak, rename new → dest.
            // .bak 保留到版本快照成功后再清理，失败时可用于恢复。
            let mut bak_exists = false;
            if dest_skill_dir.exists() {
                let bak_dir = dest_skill_dir.with_extension("bak");
                // 清理旧 .bak：如果删除失败说明有权限/锁问题，
                // 后续会误迁移 stale .bak 的版本历史，因此必须中止。
                if bak_dir.exists() {
                    std::fs::remove_dir_all(&bak_dir).map_err(|e| {
                        Error::Evolution(format!(
                            "Cannot remove stale .bak dir for staged promote: {}",
                            e
                        ))
                    })?;
                }
                // 将旧目录 rename 为 .bak 作为备份。
                // 如果 rename 失败，不采用 remove_dir_all fallback——
                // 那会让旧 skill 无备份就丢失，后续快照失败时无法恢复。
                if let Err(e) = std::fs::rename(&dest_skill_dir, &bak_dir) {
                    warn!(
                        skill = %record.skill_name,
                        error = %e,
                        "🧹 [create_new_version] staged 备份旧目录到 .bak 失败，中止 promote"
                    );
                    return Err(Error::Evolution(format!(
                        "Cannot backup existing skill dir for staged promote (rename failed: {}). \
                         Refusing to proceed without backup — old skill would be lost on rollback.",
                        e
                    )));
                }
                bak_exists = true;
            }

            // Prefer atomic rename if possible; fallback to copy+remove.
            if let Err(e) = std::fs::rename(&staged_skill_dir, &dest_skill_dir) {
                warn!(
                    skill = %record.skill_name,
                    error = %e,
                    "Staged skill promote via rename failed, falling back to copy"
                );
                if let Err(copy_err) = copy_dir_all(&staged_skill_dir, &dest_skill_dir) {
                    // copy fallback 失败：恢复 .bak（如果存在）
                    warn!(
                        skill = %record.skill_name,
                        error = %copy_err,
                        "🧹 [create_new_version] staged promote copy fallback 失败，恢复 .bak"
                    );
                    if bak_exists {
                        let bak_dir = dest_skill_dir.with_extension("bak");
                        if dest_skill_dir.exists() {
                            std::fs::remove_dir_all(&dest_skill_dir).ok();
                        }
                        if let Err(restore_err) = std::fs::rename(&bak_dir, &dest_skill_dir) {
                            warn!(
                                skill = %record.skill_name,
                                error = %restore_err,
                                "Failed to restore .bak after promote copy failure, .bak preserved"
                            );
                        }
                    }
                    return Err(Error::Evolution(format!(
                        "Staged skill promote failed: rename ({}) and copy ({})",
                        e, copy_err
                    )));
                }
                std::fs::remove_dir_all(&staged_skill_dir).ok();
            }

            // 将 .bak 中的版本历史和快照迁移到新主目录，
            // 确保 create_version() 能看到已有 baseline，新版本成为 v2 而非 v1。
            // 迁移失败时恢复 .bak → 主目录，避免旧 skill 停留在 .bak 丢失。
            let bak_dir = dest_skill_dir.with_extension("bak");
            if bak_dir.exists() {
                let bak_history = bak_dir.join("version_history.json");
                let bak_versions = bak_dir.join("versions");
                let migrate_result: Result<()> = (|| {
                    if bak_history.exists() {
                        std::fs::copy(&bak_history, dest_skill_dir.join("version_history.json"))
                            .map_err(|e| {
                                Error::Evolution(format!(
                                    "Failed to migrate version_history.json from .bak: {}",
                                    e
                                ))
                            })?;
                    }
                    if bak_versions.exists() {
                        copy_dir_all(&bak_versions, &dest_skill_dir.join("versions")).map_err(
                            |e| {
                                Error::Evolution(format!(
                                    "Failed to migrate versions/ from .bak: {}",
                                    e
                                ))
                            },
                        )?;
                    }
                    Ok(())
                })();

                if let Err(e) = migrate_result {
                    warn!(
                        skill = %record.skill_name,
                        error = %e,
                        "🧹 [create_new_version] staged 版本历史迁移失败，恢复 .bak 到主目录"
                    );
                    // 删除 promoted 的新目录，恢复 .bak
                    if dest_skill_dir.exists() {
                        std::fs::remove_dir_all(&dest_skill_dir).ok();
                    }
                    if let Err(restore_err) = std::fs::rename(&bak_dir, &dest_skill_dir) {
                        warn!(
                            skill = %record.skill_name,
                            error = %restore_err,
                            "Failed to restore .bak after migration failure, .bak preserved"
                        );
                    }
                    return Err(e);
                }
                version_history_migrated = true;
            }

            if let Some(meta) = self.extract_yaml_from_response(&patch.explanation) {
                let meta_path = dest_skill_dir.join("meta.yaml");
                let meta_issues = validate_meta_yaml(&meta);
                if meta_issues.is_empty() {
                    if let Err(e) = std::fs::write(&meta_path, &meta) {
                        warn!(
                            skill = %record.skill_name,
                            error = %e,
                            "Failed to write meta.yaml to promoted skill"
                        );
                    }
                } else {
                    warn!(
                        skill = %record.skill_name,
                        issues = %meta_issues.join("; "),
                        "Refusing to write invalid meta.yaml from LLM response"
                    );
                }
            }

            info!(
                skill = %record.skill_name,
                from = %skill_root.display(),
                to = %self.skills_dir.display(),
                "🚚 [promote] External skill promoted into main skills directory"
            );

            // Clean up the staging root directory after successful promote.
            // The skill subdirectory was already moved/removed above, but the
            // parent staging directory (staging_skills_dir) may still exist as an
            // orphan. Only remove it if empty to avoid deleting other staged skills.
            if let Some(ref staging_dir) = record.context.staging_skills_dir {
                let staging_path = PathBuf::from(staging_dir);
                if staging_path.exists() {
                    // Try to remove only if the directory is empty
                    if std::fs::read_dir(&staging_path)
                        .is_ok_and(|mut entries| entries.next().is_none())
                    {
                        if let Err(e) = std::fs::remove_dir(&staging_path) {
                            warn!(path = %staging_dir, error = %e, "Failed to clean up empty staging directory after promote");
                        }
                    }
                }
            }
        }

        // 通过 VersionManager 创建版本快照
        let changelog = Some(format!(
            "Evolution {}: {}",
            record.id,
            patch.explanation.chars().take(200).collect::<String>()
        ));
        let version = match self.version_manager.create_version(
            &record.skill_name,
            VersionSource::Evolution,
            changelog,
        ) {
            Ok(v) => v,
            Err(e) => {
                // 快照创建失败：磁盘已被新内容覆盖，但版本历史里没有 v2。
                // 尝试恢复到写入前的版本快照，避免 rollback() 因版本不足被拒绝。
                if let Some(ref prev_ver) = pre_write_version {
                    warn!(
                        skill = %record.skill_name,
                        error = %e,
                        pre_write_version = %prev_ver,
                        "Version snapshot creation failed after live write, restoring pre-write version"
                    );
                    if let Err(restore_err) = self
                        .version_manager
                        .switch_to_version(&record.skill_name, prev_ver)
                    {
                        warn!(
                            skill = %record.skill_name,
                            error = %restore_err,
                            "Failed to restore pre-write version after snapshot failure"
                        );
                    }
                }

                // staged 技能快照失败后的清理：区分"新导入"和"覆盖已有"
                if record.context.staged {
                    let main_skill_dir = self.skills_dir.join(&record.skill_name);
                    let bak_dir = main_skill_dir.with_extension("bak");
                    if bak_dir.exists() {
                        // 覆盖已有 skill 的情况：.bak 仍保留着旧版本，
                        // 尝试恢复 .bak → 主目录，避免数据丢失
                        warn!(
                            skill = %record.skill_name,
                            "🧹 [create_new_version] staged 覆盖已有 skill 快照失败，从 .bak 恢复旧版本"
                        );
                        if main_skill_dir.exists() {
                            std::fs::remove_dir_all(&main_skill_dir).ok();
                        }
                        if let Err(restore_err) = std::fs::rename(&bak_dir, &main_skill_dir) {
                            warn!(
                                skill = %record.skill_name,
                                error = %restore_err,
                                "Failed to restore .bak after snapshot failure, .bak preserved"
                            );
                        }
                    } else if pre_write_version.is_none() || main_skill_dir.exists() {
                        // 全新 staged skill（无 baseline、无 .bak）：删除主目录避免残留坏 skill
                        if main_skill_dir.exists() {
                            std::fs::remove_dir_all(&main_skill_dir).ok();
                            warn!(
                                skill = %record.skill_name,
                                "🧹 [create_new_version] staged 新技能快照失败且无 .bak 可恢复，删除已 promoted 的主目录"
                            );
                        }
                    }
                }
                return Err(e);
            }
        };

        info!(
            skill = %record.skill_name,
            version = %version.version,
            "New skill version deployed via evolution"
        );

        // 版本快照成功后，清理 staged promote 保留的 .bak 目录。
        // 只有版本历史迁移成功时才清理，否则保留 .bak 以备手动恢复
        if record.context.staged && version_history_migrated {
            let bak_dir = self
                .skills_dir
                .join(&record.skill_name)
                .with_extension("bak");
            if bak_dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&bak_dir) {
                    warn!(
                        skill = %record.skill_name,
                        error = %e,
                        "Failed to clean up .bak directory after version snapshot success"
                    );
                }
            }
        }

        // Clean up skill directory — remove temp/cache/backup files
        let final_skill_dir = self.skills_dir.join(&record.skill_name);
        self.cleanup_skill_dir(&final_skill_dir, &record.skill_name);

        Ok(())
    }

    /// 清理技能目录：删除非必要文件，只保留 SKILL.rhai/SKILL.py/SKILL.md, meta.yaml, tests/, CHANGELOG.md
    pub(crate) fn cleanup_skill_dir(&self, skill_dir: &Path, skill_name: &str) {
        if !skill_dir.exists() {
            return;
        }

        // Files/dirs we always keep
        let keep_files: &[&str] = &[
            "SKILL.rhai",
            "SKILL.py",
            "SKILL.md",
            "meta.yaml",
            "CHANGELOG.md",
        ];
        let keep_dirs: &[&str] = &["tests", "manual"];

        let entries = match std::fs::read_dir(skill_dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        let mut removed = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if path.is_dir() {
                if keep_dirs.contains(&name_str.as_ref()) {
                    continue;
                }
                // Remove __pycache__ and other cache dirs
                if (name_str == "__pycache__" || name_str.starts_with('.'))
                    && std::fs::remove_dir_all(&path).is_ok()
                {
                    removed += 1;
                }
            } else {
                if keep_files.contains(&name_str.as_ref()) {
                    continue;
                }
                // Remove temp files, .pyc, .bak, .tmp, .orig, swap files
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                let should_remove =
                    matches!(ext, "pyc" | "pyo" | "bak" | "tmp" | "orig" | "swp" | "swo")
                        || name_str.ends_with(".bak")
                        || name_str.ends_with(".orig")
                        || name_str.starts_with('.')
                        || name_str.ends_with('~');
                if should_remove && std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }

        if removed > 0 {
            info!(
                skill = %skill_name,
                removed = removed,
                "🧹 [cleanup] Removed {} non-essential files from skill directory",
                removed
            );
        }
    }

    pub(crate) fn restore_previous_version(&self, skill_name: &str) -> Result<()> {
        self.version_manager
            .rollback(skill_name)
            .map_err(|e| Error::Evolution(format!("Rollback failed: {}", e)))
    }

    pub fn save_record_public(&self, record: &EvolutionRecord) -> Result<()> {
        self.save_record(record)
    }

    /// P2-7: 原子写入 — write-tmp-then-rename，避免崩溃时文件损坏
    pub(crate) fn save_record(&self, record: &EvolutionRecord) -> Result<()> {
        let records_dir = self
            .evolution_db
            .parent()
            .unwrap()
            .join("evolution_records");
        std::fs::create_dir_all(&records_dir)?;

        let record_file = records_dir.join(format!("{}.json", record.id));
        // Use a unique temp file name to avoid races when multiple tick loops/processes
        // attempt to write the same record concurrently.
        let counter = RECORD_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let temp_file = records_dir.join(format!(
            "{}.json.tmp_{}_{}_{}",
            record.id,
            chrono::Utc::now().timestamp_millis(),
            pid,
            counter
        ));
        let json = serde_json::to_string_pretty(record)?;

        // 先写入临时文件
        std::fs::write(&temp_file, &json)?;
        swap_record_file(&temp_file, &record_file)?;

        Ok(())
    }

    pub fn load_record(&self, evolution_id: &str) -> Result<EvolutionRecord> {
        let records_dir = self
            .evolution_db
            .parent()
            .unwrap()
            .join("evolution_records");
        let record_file = records_dir.join(format!("{}.json", evolution_id));

        let json = std::fs::read_to_string(record_file)?;
        let record = serde_json::from_str(&json)?;

        Ok(record)
    }
}

/// Atomically replace `record_file` with `temp_file`.
/// On Windows, rename over an existing file fails, so we use a backup-based
/// approach: rename existing → `.bak`, rename temp → target, remove `.bak`.
/// If the temp→target rename fails, the `.bak` is restored so the original
/// record is never lost.
fn swap_record_file(temp_file: &Path, record_file: &Path) -> std::io::Result<()> {
    if !record_file.exists() {
        return std::fs::rename(temp_file, record_file);
    }
    let backup_path = record_file.with_extension("json.bak");
    let _ = std::fs::rename(record_file, &backup_path);
    match std::fs::rename(temp_file, record_file) {
        Ok(()) => {
            let _ = std::fs::remove_file(&backup_path);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::rename(&backup_path, record_file);
            let _ = std::fs::remove_file(temp_file);
            Err(e)
        }
    }
}

#[cfg(test)]
mod swap_record_file_tests {
    use super::swap_record_file;

    fn test_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "blockcell_swap_record_{}_{}",
            tag,
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn swap_restores_original_when_temp_rename_fails() {
        let dir = test_dir("restore");
        let record = dir.join("ev1.json");
        std::fs::write(&record, "original").unwrap();
        // Nonexistent temp makes temp→target rename fail after .bak was created.
        let missing_temp = dir.join("ev1.json.tmp_missing");

        let err = swap_record_file(&missing_temp, &record).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        // Original content is restored from .bak, and no stale .bak remains.
        assert_eq!(std::fs::read_to_string(&record).unwrap(), "original");
        assert!(!record.with_extension("json.bak").exists());
    }

    #[test]
    fn swap_replaces_existing_and_cleans_backup() {
        let dir = test_dir("replace");
        let record = dir.join("ev1.json");
        std::fs::write(&record, "old").unwrap();
        let temp = dir.join("ev1.json.tmp_x");
        std::fs::write(&temp, "new").unwrap();

        swap_record_file(&temp, &record).unwrap();
        assert_eq!(std::fs::read_to_string(&record).unwrap(), "new");
        assert!(!record.with_extension("json.bak").exists());
    }
}
