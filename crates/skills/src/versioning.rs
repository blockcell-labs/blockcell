use blockcell_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// 技能版本信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillVersion {
    pub version: String,
    pub hash: String,
    pub created_at: i64,
    pub created_by: VersionSource,
    pub changelog: Option<String>,
    pub parent_version: Option<String>,
}

/// 版本来源
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum VersionSource {
    Manual,
    Evolution,
    Import,
}

/// 版本历史记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionHistory {
    pub skill_name: String,
    pub versions: Vec<SkillVersion>,
    pub current_version: String,
}

/// 版本管理器
pub struct VersionManager {
    skills_dir: PathBuf,
}

impl VersionManager {
    pub fn new(skills_dir: PathBuf) -> Self {
        Self { skills_dir }
    }

    /// 获取技能的版本历史
    pub fn get_history(&self, skill_name: &str) -> Result<VersionHistory> {
        let history_file = self.get_history_file_path(skill_name);
        
        if !history_file.exists() {
            // 如果没有历史文件，创建默认历史
            return Ok(VersionHistory {
                skill_name: skill_name.to_string(),
                versions: vec![],
                current_version: "v1".to_string(),
            });
        }

        let content = std::fs::read_to_string(&history_file)?;
        let history: VersionHistory = serde_json::from_str(&content)?;
        Ok(history)
    }

    /// 保存版本历史
    pub fn save_history(&self, history: &VersionHistory) -> Result<()> {
        let history_file = self.get_history_file_path(&history.skill_name);
        let content = serde_json::to_string_pretty(history)?;
        std::fs::write(&history_file, content)?;
        Ok(())
    }

    /// 创建新版本
    pub fn create_version(
        &self,
        skill_name: &str,
        source: VersionSource,
        changelog: Option<String>,
    ) -> Result<SkillVersion> {
        let mut history = self.get_history(skill_name)?;
        
        // 计算新版本号
        let version_num = history.versions.len() + 1;
        let version = format!("v{}", version_num);
        
        // 计算当前技能内容的 hash
        let hash = self.compute_skill_hash(skill_name)?;
        
        let new_version = SkillVersion {
            version: version.clone(),
            hash,
            created_at: chrono::Utc::now().timestamp(),
            created_by: source,
            changelog,
            parent_version: Some(history.current_version.clone()),
        };

        // 保存版本快照
        self.save_version_snapshot(skill_name, &new_version)?;

        // 更新历史
        history.versions.push(new_version.clone());
        history.current_version = version;
        self.save_history(&history)?;

        info!(
            skill = %skill_name,
            version = %new_version.version,
            "Created new skill version"
        );

        Ok(new_version)
    }

    /// 切换到指定版本
    pub fn switch_to_version(&self, skill_name: &str, version: &str) -> Result<()> {
        let mut history = self.get_history(skill_name)?;
        
        // 检查版本是否存在
        let target_version = history
            .versions
            .iter()
            .find(|v| v.version == version)
            .ok_or_else(|| Error::NotFound(format!("Version {} not found", version)))?;

        // 恢复版本快照
        self.restore_version_snapshot(skill_name, target_version)?;

        // 更新当前版本
        history.current_version = version.to_string();
        self.save_history(&history)?;

        info!(
            skill = %skill_name,
            version = %version,
            "Switched to version"
        );

        Ok(())
    }

    /// 回滚到上一个版本
    pub fn rollback(&self, skill_name: &str) -> Result<()> {
        let history = self.get_history(skill_name)?;
        
        // 找到当前版本的父版本
        let current = history
            .versions
            .iter()
            .find(|v| v.version == history.current_version)
            .ok_or_else(|| Error::NotFound("Current version not found".to_string()))?;

        let parent_version = current
            .parent_version
            .as_ref()
            .ok_or_else(|| Error::Other("No parent version to rollback to".to_string()))?;

        self.switch_to_version(skill_name, parent_version)?;

        warn!(
            skill = %skill_name,
            from = %history.current_version,
            to = %parent_version,
            "Rolled back skill version"
        );

        Ok(())
    }

    /// 列出所有版本
    pub fn list_versions(&self, skill_name: &str) -> Result<Vec<SkillVersion>> {
        let history = self.get_history(skill_name)?;
        Ok(history.versions)
    }

    /// 获取当前版本
    pub fn get_current_version(&self, skill_name: &str) -> Result<String> {
        let history = self.get_history(skill_name)?;
        Ok(history.current_version)
    }

    /// 删除旧版本（保留最近 N 个）
    pub fn cleanup_old_versions(&self, skill_name: &str, keep_count: usize) -> Result<()> {
        let mut history = self.get_history(skill_name)?;
        
        if history.versions.len() <= keep_count {
            return Ok(());
        }

        // 保留最近的 N 个版本
        let to_remove = history.versions.len() - keep_count;
        let removed_versions: Vec<_> = history.versions.drain(..to_remove).collect();

        // 删除版本快照
        for version in &removed_versions {
            let snapshot_dir = self.get_version_snapshot_dir(skill_name, &version.version);
            if snapshot_dir.exists() {
                std::fs::remove_dir_all(&snapshot_dir)?;
                debug!(
                    skill = %skill_name,
                    version = %version.version,
                    "Removed old version snapshot"
                );
            }
        }

        self.save_history(&history)?;

        info!(
            skill = %skill_name,
            removed = removed_versions.len(),
            "Cleaned up old versions"
        );

        Ok(())
    }

    /// 比较两个版本
    pub fn diff_versions(
        &self,
        skill_name: &str,
        version1: &str,
        version2: &str,
    ) -> Result<String> {
        let snapshot1 = self.get_version_snapshot_dir(skill_name, version1);
        let snapshot2 = self.get_version_snapshot_dir(skill_name, version2);

        let content1 = std::fs::read_to_string(snapshot1.join("SKILL.rhai"))?;
        let content2 = std::fs::read_to_string(snapshot2.join("SKILL.rhai"))?;

        // 简单的行级 diff
        let diff = self.compute_diff(&content1, &content2);
        Ok(diff)
    }

    // === 辅助方法 ===

    fn get_history_file_path(&self, skill_name: &str) -> PathBuf {
        self.skills_dir
            .join(skill_name)
            .join("version_history.json")
    }

    fn get_version_snapshot_dir(&self, skill_name: &str, version: &str) -> PathBuf {
        self.skills_dir
            .join(skill_name)
            .join("versions")
            .join(version)
    }

    fn compute_skill_hash(&self, skill_name: &str) -> Result<String> {
        let skill_file = self.skills_dir.join(skill_name).join("SKILL.rhai");
        
        if !skill_file.exists() {
            return Ok("empty".to_string());
        }

        let content = std::fs::read_to_string(&skill_file)?;
        let hash = format!("{:x}", md5::compute(content.as_bytes()));
        Ok(hash)
    }

    fn save_version_snapshot(&self, skill_name: &str, version: &SkillVersion) -> Result<()> {
        let snapshot_dir = self.get_version_snapshot_dir(skill_name, &version.version);
        std::fs::create_dir_all(&snapshot_dir)?;

        let skill_dir = self.skills_dir.join(skill_name);

        // 复制 SKILL.rhai
        let skill_file = skill_dir.join("SKILL.rhai");
        if skill_file.exists() {
            std::fs::copy(&skill_file, snapshot_dir.join("SKILL.rhai"))?;
        }

        // 复制 meta.yaml 或 meta.json
        let meta_yaml = skill_dir.join("meta.yaml");
        if meta_yaml.exists() {
            std::fs::copy(&meta_yaml, snapshot_dir.join("meta.yaml"))?;
        }

        let meta_json = skill_dir.join("meta.json");
        if meta_json.exists() {
            std::fs::copy(&meta_json, snapshot_dir.join("meta.json"))?;
        }

        // 复制 SKILL.md
        let skill_md = skill_dir.join("SKILL.md");
        if skill_md.exists() {
            std::fs::copy(&skill_md, snapshot_dir.join("SKILL.md"))?;
        }

        // 保存版本元数据
        let version_meta = serde_json::to_string_pretty(version)?;
        std::fs::write(snapshot_dir.join("version.json"), version_meta)?;

        Ok(())
    }

    fn restore_version_snapshot(&self, skill_name: &str, version: &SkillVersion) -> Result<()> {
        let snapshot_dir = self.get_version_snapshot_dir(skill_name, &version.version);
        
        if !snapshot_dir.exists() {
            return Err(Error::NotFound(format!(
                "Version snapshot not found: {}",
                version.version
            )));
        }

        let skill_dir = self.skills_dir.join(skill_name);

        // 恢复 SKILL.rhai
        let snapshot_skill = snapshot_dir.join("SKILL.rhai");
        if snapshot_skill.exists() {
            std::fs::copy(&snapshot_skill, skill_dir.join("SKILL.rhai"))?;
        }

        // 恢复 meta 文件
        let snapshot_meta_yaml = snapshot_dir.join("meta.yaml");
        if snapshot_meta_yaml.exists() {
            std::fs::copy(&snapshot_meta_yaml, skill_dir.join("meta.yaml"))?;
        }

        let snapshot_meta_json = snapshot_dir.join("meta.json");
        if snapshot_meta_json.exists() {
            std::fs::copy(&snapshot_meta_json, skill_dir.join("meta.json"))?;
        }

        // 恢复 SKILL.md
        let snapshot_md = snapshot_dir.join("SKILL.md");
        if snapshot_md.exists() {
            std::fs::copy(&snapshot_md, skill_dir.join("SKILL.md"))?;
        }

        Ok(())
    }

    fn compute_diff(&self, content1: &str, content2: &str) -> String {
        let lines1: Vec<&str> = content1.lines().collect();
        let lines2: Vec<&str> = content2.lines().collect();

        let mut diff = String::new();
        diff.push_str("--- version 1\n");
        diff.push_str("+++ version 2\n");

        let max_len = lines1.len().max(lines2.len());
        for i in 0..max_len {
            let line1 = lines1.get(i);
            let line2 = lines2.get(i);

            match (line1, line2) {
                (Some(l1), Some(l2)) if l1 == l2 => {
                    diff.push_str(&format!("  {}\n", l1));
                }
                (Some(l1), Some(l2)) => {
                    diff.push_str(&format!("- {}\n", l1));
                    diff.push_str(&format!("+ {}\n", l2));
                }
                (Some(l1), None) => {
                    diff.push_str(&format!("- {}\n", l1));
                }
                (None, Some(l2)) => {
                    diff.push_str(&format!("+ {}\n", l2));
                }
                (None, None) => break,
            }
        }

        diff
    }

    /// 导出版本到文件
    pub fn export_version(&self, skill_name: &str, version: &str, output_path: &Path) -> Result<()> {
        let snapshot_dir = self.get_version_snapshot_dir(skill_name, version);
        
        if !snapshot_dir.exists() {
            return Err(Error::NotFound(format!("Version {} not found", version)));
        }

        // 创建 tar.gz 归档
        let file = std::fs::File::create(output_path)?;
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        tar.append_dir_all(skill_name, &snapshot_dir)?;
        tar.finish()?;

        info!(
            skill = %skill_name,
            version = %version,
            output = %output_path.display(),
            "Exported version"
        );

        Ok(())
    }

    /// 导入版本
    pub fn import_version(&self, skill_name: &str, archive_path: &Path) -> Result<SkillVersion> {
        let file = std::fs::File::open(archive_path)?;
        let dec = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(dec);

        // 解压到临时目录
        let temp_dir = std::env::temp_dir().join(format!("skill_import_{}", chrono::Utc::now().timestamp()));
        std::fs::create_dir_all(&temp_dir)?;
        archive.unpack(&temp_dir)?;

        // 读取版本元数据
        let version_meta_path = temp_dir.join(skill_name).join("version.json");
        let version_meta_content = std::fs::read_to_string(&version_meta_path)?;
        let mut version: SkillVersion = serde_json::from_str(&version_meta_content)?;

        // 修改版本号和来源
        let mut history = self.get_history(skill_name)?;
        let version_num = history.versions.len() + 1;
        version.version = format!("v{}", version_num);
        version.created_by = VersionSource::Import;
        version.created_at = chrono::Utc::now().timestamp();

        // 复制到版本目录
        let snapshot_dir = self.get_version_snapshot_dir(skill_name, &version.version);
        std::fs::create_dir_all(&snapshot_dir)?;
        
        for entry in std::fs::read_dir(temp_dir.join(skill_name))? {
            let entry = entry?;
            let file_name = entry.file_name();
            std::fs::copy(entry.path(), snapshot_dir.join(&file_name))?;
        }

        // 更新历史
        history.versions.push(version.clone());
        self.save_history(&history)?;

        // 清理临时目录
        let _ = std::fs::remove_dir_all(&temp_dir);

        info!(
            skill = %skill_name,
            version = %version.version,
            "Imported version"
        );

        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_source() {
        let source = VersionSource::Evolution;
        assert_eq!(source, VersionSource::Evolution);
    }
}
