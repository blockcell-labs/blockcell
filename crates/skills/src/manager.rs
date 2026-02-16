use crate::service::{EvolutionService, EvolutionServiceConfig};
use crate::versioning::{VersionManager, VersionSource};
use blockcell_core::{Paths, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillMeta {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub requires: SkillRequires,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub always: bool,
    /// Trigger phrases — when user input matches any of these, this skill is activated.
    #[serde(default)]
    pub triggers: Vec<String>,
    /// Capabilities this skill depends on (capability IDs from the registry).
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Output format hint (e.g. "markdown", "json", "table").
    #[serde(default)]
    pub output_format: Option<String>,
    /// Fallback strategy when the skill fails.
    #[serde(default)]
    pub fallback: Option<SkillFallback>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillRequires {
    #[serde(default)]
    pub bins: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillFallback {
    /// Strategy: "degrade" (use simpler approach), "skip" (inform user), "alternative" (use another skill).
    #[serde(default = "default_fallback_strategy")]
    pub strategy: String,
    /// Message to show user on fallback.
    #[serde(default)]
    pub message: Option<String>,
    /// Alternative skill name to try.
    #[serde(default)]
    pub alternative_skill: Option<String>,
}

fn default_fallback_strategy() -> String {
    "degrade".to_string()
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub path: PathBuf,
    pub meta: SkillMeta,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub current_version: Option<String>,
}

impl Skill {
    /// Check if this skill has a SKILL.rhai orchestration script.
    pub fn has_rhai(&self) -> bool {
        self.path.join("SKILL.rhai").exists()
    }

    /// Check if this skill has a SKILL.md prompt file.
    pub fn has_md(&self) -> bool {
        self.path.join("SKILL.md").exists()
    }

    /// Load the SKILL.md content.
    pub fn load_md(&self) -> Option<String> {
        let md_path = self.path.join("SKILL.md");
        std::fs::read_to_string(md_path).ok()
    }

    /// Load the SKILL.rhai script content.
    pub fn load_rhai(&self) -> Option<String> {
        let rhai_path = self.path.join("SKILL.rhai");
        std::fs::read_to_string(rhai_path).ok()
    }

    /// Get the tests directory path.
    pub fn tests_dir(&self) -> PathBuf {
        self.path.join("tests")
    }

    /// Load test fixtures from the tests/ directory.
    pub fn load_test_fixtures(&self) -> Vec<SkillTestFixture> {
        let tests_dir = self.tests_dir();
        if !tests_dir.exists() {
            return vec![];
        }
        let mut fixtures = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&tests_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |e| e == "json") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(fixture) = serde_json::from_str::<SkillTestFixture>(&content) {
                            fixtures.push(fixture);
                        }
                    }
                }
            }
        }
        fixtures
    }
}

/// A test fixture for shadow testing a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTestFixture {
    /// Test case name.
    pub name: String,
    /// Simulated user input.
    pub input: String,
    /// Expected output (substring match or JSON schema).
    #[serde(default)]
    pub expected_output: Option<String>,
    /// Expected tool calls (in order).
    #[serde(default)]
    pub expected_tools: Vec<String>,
    /// Context variables to inject into Rhai scope.
    #[serde(default)]
    pub context: serde_json::Value,
}

pub struct SkillManager {
    skills: HashMap<String, Skill>,
    version_manager: Option<VersionManager>,
    evolution_service: Option<EvolutionService>,
    /// Known available capability IDs (synced from CapabilityRegistry)
    available_capabilities: std::collections::HashSet<String>,
}

impl SkillManager {
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
            version_manager: None,
            evolution_service: None,
            available_capabilities: std::collections::HashSet::new(),
        }
    }

    /// Sync available capability IDs from the CapabilityRegistry.
    /// Called periodically from the runtime tick to keep skill availability up to date.
    pub fn sync_capabilities(&mut self, capability_ids: Vec<String>) {
        self.available_capabilities = capability_ids.into_iter().collect();
    }

    /// Get the list of missing capabilities across all skills.
    /// Returns (skill_name, missing_capability_id) pairs.
    /// Filters out capability IDs that match built-in tool names — those are
    /// already available as tools and should not trigger capability evolution.
    pub fn get_missing_capabilities(&self) -> Vec<(String, String)> {
        let mut missing = Vec::new();
        for skill in self.skills.values() {
            for cap_id in &skill.meta.capabilities {
                if !self.available_capabilities.contains(cap_id)
                    && !crate::service::is_builtin_tool(cap_id)
                {
                    missing.push((skill.name.clone(), cap_id.clone()));
                }
            }
        }
        missing
    }

    pub fn with_versioning(mut self, skills_dir: PathBuf) -> Self {
        self.version_manager = Some(VersionManager::new(skills_dir));
        self
    }

    pub fn with_evolution(mut self, skills_dir: PathBuf, config: EvolutionServiceConfig) -> Self {
        self.evolution_service = Some(EvolutionService::new(skills_dir, config));
        self
    }

    pub fn evolution_service(&self) -> Option<&EvolutionService> {
        self.evolution_service.as_ref()
    }

    pub fn load_from_paths(&mut self, paths: &Paths) -> Result<()> {
        // Load built-in skills first (lower priority)
        let builtin_dir = paths.builtin_skills_dir();
        if builtin_dir.exists() {
            debug!(path = %builtin_dir.display(), "Loading built-in skills");
            self.scan_directory_with_priority(&builtin_dir, false)?;
        }

        // Load workspace skills (higher priority, can override built-in)
        let workspace_dir = paths.skills_dir();
        if workspace_dir.exists() {
            debug!(path = %workspace_dir.display(), "Loading workspace skills");
            self.scan_directory_with_priority(&workspace_dir, true)?;
        }

        Ok(())
    }

    /// Re-scan skill directories and pick up any newly created or modified skills.
    /// Returns the names of newly discovered skills (not previously loaded).
    pub fn reload_skills(&mut self, paths: &Paths) -> Result<Vec<String>> {
        let before: std::collections::HashSet<String> = self.skills.keys().cloned().collect();
        self.load_from_paths(paths)?;
        let new_skills: Vec<String> = self.skills.keys()
            .filter(|k| !before.contains(*k))
            .cloned()
            .collect();
        if !new_skills.is_empty() {
            info!(new_skills = ?new_skills, "Hot-reloaded new skills");
        }
        Ok(new_skills)
    }

    fn scan_directory_with_priority(&mut self, dir: &PathBuf, is_workspace: bool) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            
            if path.is_dir() {
                if let Some(skill) = self.load_skill(&path)? {
                    let skill_name = skill.name.clone();
                    
                    // Workspace skills override built-in skills
                    if is_workspace || !self.skills.contains_key(&skill_name) {
                        let source = if is_workspace { "workspace" } else { "built-in" };
                        debug!(
                            name = %skill_name, 
                            available = skill.available, 
                            source = source,
                            "Loaded skill"
                        );
                        self.skills.insert(skill_name, skill);
                    }
                }
            }
        }

        Ok(())
    }

    fn load_skill(&self, skill_dir: &PathBuf) -> Result<Option<Skill>> {
        let name = skill_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Try to load meta from meta.yaml or meta.json
        let meta = self.load_meta(skill_dir)?;

        // Check availability
        let (available, reason) = self.check_availability(&meta);

        // 获取当前版本
        let current_version = if let Some(vm) = &self.version_manager {
            vm.get_current_version(&name).ok()
        } else {
            None
        };

        Ok(Some(Skill {
            name: if meta.name.is_empty() { name } else { meta.name.clone() },
            path: skill_dir.clone(),
            meta,
            available,
            unavailable_reason: reason,
            current_version,
        }))
    }

    fn load_meta(&self, skill_dir: &PathBuf) -> Result<SkillMeta> {
        // Try meta.yaml first
        let yaml_path = skill_dir.join("meta.yaml");
        if yaml_path.exists() {
            let content = std::fs::read_to_string(&yaml_path)?;
            return Ok(serde_yaml::from_str(&content)?);
        }

        // Try meta.json
        let json_path = skill_dir.join("meta.json");
        if json_path.exists() {
            let content = std::fs::read_to_string(&json_path)?;
            return Ok(serde_json::from_str(&content)?);
        }

        // Return default meta
        Ok(SkillMeta::default())
    }

    fn check_availability(&self, meta: &SkillMeta) -> (bool, Option<String>) {
        // Check required binaries
        for bin in &meta.requires.bins {
            if which::which(bin).is_err() {
                return (false, Some(format!("Missing binary: {}", bin)));
            }
        }

        // Check required environment variables
        for env_var in &meta.requires.env {
            if std::env::var(env_var).is_err() {
                return (false, Some(format!("Missing env var: {}", env_var)));
            }
        }

        // Check required capabilities from the registry
        // Skip capability IDs that match built-in tool names (those are always available)
        for cap_id in &meta.capabilities {
            if !crate::service::is_builtin_tool(cap_id)
                && !self.available_capabilities.contains(cap_id)
            {
                return (false, Some(format!("Missing capability: {}", cap_id)));
            }
        }

        (true, None)
    }

    pub fn get_summary_xml(&self) -> String {
        let mut xml = String::from("<skills>\n");

        for skill in self.skills.values() {
            xml.push_str(&format!(
                "  <skill available=\"{}\">\n",
                skill.available
            ));
            xml.push_str(&format!("    <name>{}</name>\n", skill.name));
            xml.push_str(&format!(
                "    <description>{}</description>\n",
                skill.meta.description
            ));
            xml.push_str(&format!(
                "    <location>{}/SKILL.md</location>\n",
                skill.path.display()
            ));
            
            if !skill.available {
                if let Some(reason) = &skill.unavailable_reason {
                    xml.push_str(&format!("    <requires>{}</requires>\n", reason));
                }
            }
            
            xml.push_str("  </skill>\n");
        }

        xml.push_str("</skills>");
        xml
    }

    pub fn get_always_skills(&self) -> Vec<&Skill> {
        self.skills
            .values()
            .filter(|s| s.meta.always && s.available)
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// Find a skill whose trigger phrases match the user input.
    /// Returns the first matching skill.
    pub fn match_skill(&self, user_input: &str) -> Option<&Skill> {
        let input_lower = user_input.to_lowercase();
        self.skills.values()
            .filter(|s| s.available && !s.meta.triggers.is_empty())
            .find(|s| {
                s.meta.triggers.iter().any(|trigger| {
                    input_lower.contains(&trigger.to_lowercase())
                })
            })
    }

    /// List all available skills.
    pub fn list_available(&self) -> Vec<&Skill> {
        self.skills.values().filter(|s| s.available).collect()
    }

    // === 版本管理方法 ===

    /// 创建技能的新版本
    pub fn create_version(
        &self,
        skill_name: &str,
        source: VersionSource,
        changelog: Option<String>,
    ) -> Result<()> {
        let vm = self.version_manager.as_ref()
            .ok_or_else(|| blockcell_core::Error::Other("Version manager not initialized".to_string()))?;
        
        vm.create_version(skill_name, source, changelog)?;
        info!(skill = %skill_name, "Created new skill version");
        Ok(())
    }

    /// 切换到指定版本
    pub fn switch_version(&self, skill_name: &str, version: &str) -> Result<()> {
        let vm = self.version_manager.as_ref()
            .ok_or_else(|| blockcell_core::Error::Other("Version manager not initialized".to_string()))?;
        
        vm.switch_to_version(skill_name, version)?;
        Ok(())
    }

    /// 回滚到上一个版本
    pub fn rollback_version(&self, skill_name: &str) -> Result<()> {
        let vm = self.version_manager.as_ref()
            .ok_or_else(|| blockcell_core::Error::Other("Version manager not initialized".to_string()))?;
        
        vm.rollback(skill_name)?;
        Ok(())
    }

    /// 列出技能的所有版本
    pub fn list_versions(&self, skill_name: &str) -> Result<Vec<crate::versioning::SkillVersion>> {
        let vm = self.version_manager.as_ref()
            .ok_or_else(|| blockcell_core::Error::Other("Version manager not initialized".to_string()))?;
        
        vm.list_versions(skill_name)
    }

    /// 清理旧版本
    pub fn cleanup_old_versions(&self, skill_name: &str, keep_count: usize) -> Result<()> {
        let vm = self.version_manager.as_ref()
            .ok_or_else(|| blockcell_core::Error::Other("Version manager not initialized".to_string()))?;
        
        vm.cleanup_old_versions(skill_name, keep_count)?;
        Ok(())
    }

    /// 比较两个版本
    pub fn diff_versions(
        &self,
        skill_name: &str,
        version1: &str,
        version2: &str,
    ) -> Result<String> {
        let vm = self.version_manager.as_ref()
            .ok_or_else(|| blockcell_core::Error::Other("Version manager not initialized".to_string()))?;
        
        vm.diff_versions(skill_name, version1, version2)
    }

    /// 导出版本
    pub fn export_version(
        &self,
        skill_name: &str,
        version: &str,
        output_path: &std::path::Path,
    ) -> Result<()> {
        let vm = self.version_manager.as_ref()
            .ok_or_else(|| blockcell_core::Error::Other("Version manager not initialized".to_string()))?;
        
        vm.export_version(skill_name, version, output_path)?;
        Ok(())
    }

    /// 导入版本
    pub fn import_version(
        &self,
        skill_name: &str,
        archive_path: &std::path::Path,
    ) -> Result<()> {
        let vm = self.version_manager.as_ref()
            .ok_or_else(|| blockcell_core::Error::Other("Version manager not initialized".to_string()))?;
        
        vm.import_version(skill_name, archive_path)?;
        Ok(())
    }
}

impl Default for SkillManager {
    fn default() -> Self {
        Self::new()
    }
}
