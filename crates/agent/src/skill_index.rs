//! Skill Progressive Index — 渐进式 Skill 加载
//!
//! 核心思路:
//! - 启动时只加载 meta 信息 (name, category, description, tools)
//! - Agent 需要使用 Skill 时, 通过 skill_view 按需加载完整内容
//! - 减少系统提示词长度, 节省 token
//!
//! 参考 Hermes `agent_core/skill_index.py`

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Skill 索引条目 (轻量, 仅 meta)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillIndexEntry {
    /// Skill 名称
    pub name: String,
    /// 分类
    pub category: String,
    /// 描述 (从 meta.yaml 或 SKILL.md frontmatter 提取)
    pub description: String,
    /// 需要的工具列表
    pub tools: Vec<String>,
    /// 是否始终加载 (always: true)
    pub always: bool,
    /// Skill 目录路径
    pub path: PathBuf,
    /// 版本号 (从 meta.json 读取, 默认 "1.0.0")
    #[serde(default = "default_version")]
    pub version: String,
    /// 最后更新时间 (ISO 8601 格式)
    #[serde(default)]
    pub updated_at: Option<String>,
}

fn default_version() -> String {
    "1.0.0".to_string()
}

/// Skill Progressive Index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillIndex {
    /// 所有已索引的 Skill (key: "category/name" or "name" for no-category skills)
    entries: HashMap<String, SkillIndexEntry>,
    /// 已加载完整内容的 Skill (name → content)
    loaded: HashMap<String, String>,
}

/// Build a composite key for the entries map to avoid name collisions across categories
fn composite_key(category: &str, name: &str) -> String {
    if category.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", category, name)
    }
}

impl SkillIndex {
    /// 创建空索引
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            loaded: HashMap::new(),
        }
    }

    /// 从 skills 目录构建索引
    pub fn build_from_dir(skills_dir: &Path) -> Self {
        Self::build_from_dirs([skills_dir.to_path_buf()])
    }

    /// Build an index from multiple skill roots. Later roots override earlier
    /// roots when they expose the same category/name key.
    pub fn build_from_dirs<I>(skills_dirs: I) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut index = Self::new();

        for skills_dir in skills_dirs {
            index.scan_dir(&skills_dir);
        }

        tracing::info!(
            skill_count = index.entries.len(),
            "[SkillIndex] Built index from configured skill roots"
        );

        index
    }

    /// 判断目录是否包含 skill 标识文件
    fn is_skill_dir(path: &Path) -> bool {
        path.join("SKILL.md").exists()
            || path.join("meta.yaml").exists()
            || path.join("meta.json").exists()
            || path.join("SKILL.rhai").exists()
            || path.join("SKILL.py").exists()
    }

    fn scan_dir(&mut self, skills_dir: &Path) {
        self.scan_dir_recursive(skills_dir, "");
    }

    /// 递归扫描技能目录，维护完整 category 路径
    ///
    /// 与 SkillFileStore 的路径解析保持一致：
    /// - skill 目录 → 输出 `category/name` 复合名称
    /// - manifest.json 包目录 → 递归进入，包目录名计入 category
    /// - 其他非 skill 目录 → 视为 category，递归扫描子目录
    fn scan_dir_recursive(&mut self, dir: &Path, category: &str) {
        if !dir.exists() {
            return;
        }

        const EXCLUDED_DIRS: &[&str] = &[".git", ".github", ".hub", "__pycache__", "node_modules"];

        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let dir_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();

                if EXCLUDED_DIRS.contains(&dir_name.as_str()) {
                    continue;
                }

                let sub_category = if category.is_empty() {
                    dir_name.clone()
                } else {
                    format!("{}/{}", category, dir_name)
                };

                // 判断: 如果该目录直接包含 skill 标识文件, 则它是一个 skill
                if Self::is_skill_dir(&path) {
                    let entry = Self::build_entry(&dir_name, category, &path);
                    self.entries
                        .insert(composite_key(category, &dir_name), entry);
                    // 若同时是 skill 包（含 manifest.json），也递归扫描子目录
                    if path.join("manifest.json").exists() {
                        self.scan_dir_recursive(&path, &sub_category);
                    }
                    continue;
                }

                // 非 skill 目录但含 manifest.json：作为 skill 包递归扫描子目录
                // 包目录名计入 category 路径，确保子 skill 可被 SkillFileStore 解析
                if path.join("manifest.json").exists() {
                    self.scan_dir_recursive(&path, &sub_category);
                    continue;
                }

                // 否则：视为 category 目录，递归扫描子目录
                self.scan_dir_recursive(&path, &sub_category);
            }
        }

        tracing::debug!(
            category = category,
            dir = %dir.display(),
            "[SkillIndex] Scanned directory"
        );
    }

    /// 从单个 Skill 目录构建索引条目
    /// 优先级: meta.yaml → meta.json → SKILL.md frontmatter
    fn build_entry(name: &str, category: &str, path: &Path) -> SkillIndexEntry {
        let mut description = String::new();
        let mut tools = Vec::new();
        let mut always = false;
        let mut version = default_version();
        let mut updated_at: Option<String> = None;

        // 1. 尝试从 meta.yaml 读取
        let meta_yaml_path = path.join("meta.yaml");
        if meta_yaml_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&meta_yaml_path) {
                if let Ok(yaml) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                    if let Some(desc) = yaml.get("description").and_then(|v| v.as_str()) {
                        let trimmed = desc.trim();
                        if !trimmed.is_empty() {
                            description = trimmed.to_string();
                        }
                    }
                    if tools.is_empty() {
                        if let Some(arr) = yaml.get("tools").and_then(|v| v.as_sequence()) {
                            tools = arr
                                .iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect();
                        }
                        // Legacy fallback: capabilities → tools (same as SkillMeta::effective_tools)
                        if tools.is_empty() {
                            if let Some(arr) =
                                yaml.get("capabilities").and_then(|v| v.as_sequence())
                            {
                                tools = arr
                                    .iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect();
                            }
                        }
                    }
                    if !always {
                        always = yaml
                            .get("always")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                    }
                    if version == default_version() {
                        if let Some(v) = yaml.get("version").and_then(|v| v.as_str()) {
                            version = v.to_string();
                        }
                    }
                }
            }
        }

        // 2. 尝试从 meta.json 读取
        let meta_path = path.join("meta.json");
        if meta_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&meta_path) {
                if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&content) {
                    if description.is_empty() {
                        if let Some(desc) = meta.get("description").and_then(|v| v.as_str()) {
                            description = desc.to_string();
                        }
                    }
                    if tools.is_empty() {
                        if let Some(t) = meta.get("tools").and_then(|v| v.as_array()) {
                            tools = t
                                .iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect();
                        }
                        // Legacy fallback: capabilities → tools (same as SkillMeta::effective_tools)
                        if tools.is_empty() {
                            if let Some(c) = meta.get("capabilities").and_then(|v| v.as_array()) {
                                tools = c
                                    .iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect();
                            }
                        }
                    }
                    if !always {
                        if let Some(a) = meta.get("always").and_then(|v| v.as_bool()) {
                            always = a;
                        }
                    }
                    if version == default_version() {
                        if let Some(v) = meta.get("version").and_then(|v| v.as_str()) {
                            version = v.to_string();
                        }
                    }
                    if updated_at.is_none() {
                        if let Some(u) = meta.get("updated_at").and_then(|v| v.as_str()) {
                            updated_at = Some(u.to_string());
                        }
                    }
                }
            }
        }

        // 3. 回退到 SKILL.md frontmatter
        if description.is_empty() {
            let skill_md = path.join("SKILL.md");
            if let Ok(content) = std::fs::read_to_string(&skill_md) {
                description = Self::extract_description(&content);
                if tools.is_empty() {
                    tools = Self::extract_tools(&content);
                }
                if !always {
                    always = Self::extract_always(&content);
                }
                if version == default_version() {
                    version = Self::extract_version(&content);
                }
            }
        }

        // 4. 从文件修改时间获取 updated_at
        if updated_at.is_none() {
            // 尝试 SKILL.md, meta.yaml, meta.json 的修改时间
            for file_name in &["SKILL.md", "meta.yaml", "meta.json"] {
                let fpath = path.join(file_name);
                if fpath.exists() {
                    if let Ok(metadata) = std::fs::metadata(&fpath) {
                        if let Ok(modified) = metadata.modified() {
                            let dt: DateTime<Utc> = modified.into();
                            updated_at = Some(dt.to_rfc3339());
                            break;
                        }
                    }
                }
            }
        }

        // 5. 最终回退
        if description.is_empty() {
            description = format!("Skill: {}", name);
        }

        SkillIndexEntry {
            name: name.to_string(),
            category: category.to_string(),
            description,
            tools,
            always,
            path: path.to_path_buf(),
            version,
            updated_at,
        }
    }

    /// 从 SKILL.md 内容提取描述
    fn extract_description(content: &str) -> String {
        if let Some(frontmatter) = Self::extract_frontmatter_value(content) {
            if let Some(description) = frontmatter.get("description").and_then(|v| v.as_str()) {
                return description.trim().to_string();
            }
        }

        // 回退到第一个标题
        for line in content.lines() {
            let line = line.trim();
            if let Some(heading) = line.strip_prefix("# ") {
                return heading.to_string();
            }
        }

        String::new()
    }

    /// 从 SKILL.md 内容提取工具列表
    fn extract_tools(content: &str) -> Vec<String> {
        if let Some(frontmatter) = Self::extract_frontmatter_value(content) {
            if let Some(tools) = frontmatter.get("tools").and_then(|v| v.as_sequence()) {
                return tools
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }

        Vec::new()
    }

    /// 从 SKILL.md 内容提取 always 标志
    fn extract_always(content: &str) -> bool {
        if let Some(frontmatter) = Self::extract_frontmatter_value(content) {
            return frontmatter
                .get("always")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        }

        false
    }

    /// 从 SKILL.md frontmatter 提取版本号
    fn extract_version(content: &str) -> String {
        if let Some(frontmatter) = Self::extract_frontmatter_value(content) {
            if let Some(version) = frontmatter.get("version") {
                if let Some(v) = version.as_str() {
                    if !v.trim().is_empty() {
                        return v.trim().to_string();
                    }
                } else if let Some(v) = version.as_f64() {
                    return v.to_string();
                } else if let Some(v) = version.as_i64() {
                    return v.to_string();
                }
            }
        }

        default_version()
    }

    fn extract_frontmatter_value(content: &str) -> Option<serde_yaml::Value> {
        let content = content.strip_prefix('\u{feff}').unwrap_or(content);
        let content = content.replace("\r\n", "\n");
        let trimmed = content.trim_start();
        let rest = trimmed.strip_prefix("---")?;
        let end_idx = rest.find("\n---")?;
        let frontmatter = rest[..end_idx].trim();
        serde_yaml::from_str::<serde_yaml::Value>(frontmatter).ok()
    }

    /// 获取所有 Skill 的索引条目
    pub fn entries(&self) -> &HashMap<String, SkillIndexEntry> {
        &self.entries
    }

    /// 获取 always=true 的 Skill (启动时加载)
    pub fn always_skills(&self) -> Vec<&SkillIndexEntry> {
        self.entries.values().filter(|e| e.always).collect()
    }

    /// 按需加载 Skill 完整内容
    /// 支持多种 skill 文件: SKILL.md → SKILL.rhai → SKILL.py
    pub fn load_skill(&mut self, name: &str) -> Option<String> {
        // 检查是否已加载
        if let Some(content) = self.loaded.get(name) {
            return Some(content.clone());
        }

        // 从索引查找路径 — try exact key first, then search by name suffix.
        // 同名技能可存在于多个 category：歧义时返回 None，而不是按 HashMap
        // 迭代顺序随机选中一个（调用方应使用 composite key）。
        let entry = if let Some(entry) = self.entries.get(name) {
            entry
        } else {
            let mut matches = self.entries.values().filter(|e| e.name == name);
            let first = matches.next()?;
            if matches.next().is_some() {
                tracing::warn!(
                    skill_name = name,
                    "[SkillIndex] Ambiguous skill name across categories; use category/name"
                );
                return None;
            }
            first
        };

        // 按优先级尝试读取 skill 文件；meta-only skill 至少应能加载元信息。
        for file_name in &[
            "SKILL.md",
            "SKILL.rhai",
            "SKILL.py",
            "meta.yaml",
            "meta.json",
        ] {
            let file_path = entry.path.join(file_name);
            if file_path.exists() {
                let content = match std::fs::read_to_string(&file_path) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(
                            skill_name = name,
                            path = %file_path.display(),
                            error = %e,
                            "[SkillIndex] Failed to read skill file"
                        );
                        continue;
                    }
                };
                self.loaded.insert(name.to_string(), content.clone());

                tracing::debug!(
                    skill_name = name,
                    file = file_name,
                    content_len = content.len(),
                    "[SkillIndex] Loaded skill content"
                );

                return Some(content);
            }
        }

        tracing::warn!(
            skill_name = name,
            path = %entry.path.display(),
            "[SkillIndex] No readable skill file found"
        );
        None
    }

    /// 卸载 Skill 内容 (释放内存)
    pub fn unload_skill(&mut self, name: &str) {
        self.loaded.remove(name);
    }

    /// 检查 Skill 是否已加载
    pub fn is_loaded(&self, name: &str) -> bool {
        self.loaded.contains_key(name)
    }

    /// 生成系统提示词中的 Skill 索引摘要
    ///
    /// 格式:
    /// ```text
    /// ## Available Skills (index only, use skill_view to load)
    /// - flask-k8s-deploy [devops]: Deploy Flask apps to Kubernetes (tools: exec, write_file)
    /// - rust-debug [software-development]: Debug Rust compilation errors (always loaded)
    /// ```
    pub fn to_prompt_summary(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        let mut summary = String::from(
            "## Available Skills (index only, use `skill_view` to load full content)\n",
        );

        let mut entries: Vec<_> = self.entries.values().collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        for entry in &entries {
            let tools_str = if entry.tools.is_empty() {
                String::new()
            } else {
                format!(" (tools: {})", entry.tools.join(", "))
            };
            let always_str = if entry.always { " [always loaded]" } else { "" };
            summary.push_str(&format!(
                "- {} [{}]: {}{}{}\n",
                entry.name, entry.category, entry.description, tools_str, always_str
            ));
        }

        summary
    }

    /// 根据当前活跃工具列表匹配 Skill
    ///
    /// 返回工具列表与 active_tools 有交集的 Skill (按交集大小降序排列)
    /// 参考 Hermes `skill_index.py` 的 `match_by_tools()`
    pub fn match_by_tools(&self, active_tools: &[String]) -> Vec<&SkillIndexEntry> {
        if active_tools.is_empty() || self.entries.is_empty() {
            return Vec::new();
        }

        let active_set: HashSet<&str> = active_tools.iter().map(|s| s.as_str()).collect();

        let mut matched: Vec<(usize, &SkillIndexEntry)> = self
            .entries
            .values()
            .filter_map(|entry| {
                if entry.tools.is_empty() {
                    return None;
                }
                let overlap = entry
                    .tools
                    .iter()
                    .filter(|t| active_set.contains(t.as_str()))
                    .count();
                if overlap > 0 {
                    Some((overlap, entry))
                } else {
                    None
                }
            })
            .collect();

        // 按交集大小降序排列 (交集越大越相关)
        matched.sort_by_key(|b| std::cmp::Reverse(b.0));

        matched.into_iter().map(|(_, entry)| entry).collect()
    }

    /// 模糊搜索 Skill
    ///
    /// 基于字符重叠度 + 子串匹配的轻量级模糊搜索
    /// 参考 Hermes `skill_index.py` 的 `fuzzy_search()`
    ///
    /// 搜索范围: name, description, category
    /// 评分: 子串完全匹配 > 字符重叠度
    pub fn fuzzy_search(&self, query: &str) -> Vec<&SkillIndexEntry> {
        if query.is_empty() || self.entries.is_empty() {
            return Vec::new();
        }

        let query_lower = query.to_lowercase();
        let query_chars: HashSet<char> = query_lower.chars().collect();

        let mut scored: Vec<(i64, &SkillIndexEntry)> = self
            .entries
            .values()
            .filter_map(|entry| {
                let mut score: i64 = 0;

                // 1. 名称子串匹配 (权重最高)
                let name_lower = entry.name.to_lowercase();
                if name_lower == query_lower {
                    score += 1000; // 完全匹配
                } else if name_lower.contains(&query_lower) {
                    score += 500; // 子串匹配
                } else {
                    // 名称字符重叠度
                    let name_chars: HashSet<char> = name_lower.chars().collect();
                    let overlap = name_chars.intersection(&query_chars).count();
                    score += (overlap as i64) * 10;
                }

                // 2. 描述子串匹配
                let desc_lower = entry.description.to_lowercase();
                if desc_lower.contains(&query_lower) {
                    score += 200;
                } else {
                    // 描述字符重叠度
                    let desc_chars: HashSet<char> = desc_lower.chars().collect();
                    let overlap = desc_chars.intersection(&query_chars).count();
                    score += (overlap as i64) * 3;
                }

                // 3. 分类匹配
                let cat_lower = entry.category.to_lowercase();
                if cat_lower.contains(&query_lower) {
                    score += 100;
                }

                // 阈值: 至少要有一定相关性
                if score > 0 {
                    Some((score, entry))
                } else {
                    None
                }
            })
            .collect();

        // 按分数降序排列
        scored.sort_by_key(|b| std::cmp::Reverse(b.0));

        scored.into_iter().map(|(_, entry)| entry).collect()
    }

    /// 刷新索引 (重新扫描 skills 目录)
    ///
    /// 重新扫描后，已加载的技能内容缓存会被清除，
    /// 因为磁盘上的文件可能已被修改（如 skill_manage patch/edit）。
    pub fn refresh(&mut self, skills_dir: &Path) {
        let new_index = Self::build_from_dir(skills_dir);
        self.entries = new_index.entries;
        // 清除已加载的内容缓存，强制下次 load_skill 从磁盘重新读取
        // 原因: refresh 通常在 skill 被修改后调用，缓存可能已过时
        self.loaded.clear();
    }

    /// 使指定技能的内容缓存失效
    ///
    /// 在 skill_manage patch/edit/delete 修改 SKILL.md 后调用，
    /// 确保下次 load_skill 从磁盘重新读取最新内容。
    pub fn invalidate_skill(&mut self, name: &str) {
        if self.loaded.remove(name).is_some() {
            tracing::debug!(
                skill_name = name,
                "[SkillIndex] Invalidated cached content for skill"
            );
        }
    }
}

impl Default for SkillIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_skills_dir() -> PathBuf {
        let mut dir = std::env::temp_dir();
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        dir.push(format!(
            "blockcell_skill_index_test_{}_{}",
            std::process::id(),
            now_ns
        ));
        dir
    }

    fn create_test_skill(dir: &Path, category: &str, name: &str, content: &str) {
        let skill_dir = dir.join(category).join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn test_empty_index() {
        let index = SkillIndex::new();
        assert!(index.entries().is_empty());
    }

    #[test]
    fn test_build_from_dir() {
        let dir = temp_skills_dir();
        create_test_skill(
            &dir,
            "devops",
            "flask-deploy",
            "---\nname: flask-deploy\ndescription: Deploy Flask apps\ntools: [exec, write_file]\n---\n\n# Flask Deploy",
        );
        create_test_skill(&dir, "general", "hello", "# Hello Skill\n\nSimple greeting");

        let index = SkillIndex::build_from_dir(&dir);
        assert_eq!(index.entries().len(), 2);
        assert!(index.entries().contains_key("devops/flask-deploy"));
        assert!(index.entries().contains_key("general/hello"));

        let flask = index.entries().get("devops/flask-deploy").unwrap();
        assert_eq!(flask.category, "devops");
        assert_eq!(flask.description, "Deploy Flask apps");
        assert_eq!(flask.tools, vec!["exec", "write_file"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_always_skills() {
        let dir = temp_skills_dir();
        create_test_skill(
            &dir,
            "general",
            "always-skill",
            "---\nname: always-skill\ndescription: Always loaded\nalways: true\n---\n\n# Always",
        );
        create_test_skill(
            &dir,
            "general",
            "normal-skill",
            "---\nname: normal-skill\ndescription: Normal\n---\n\n# Normal",
        );

        let index = SkillIndex::build_from_dir(&dir);
        let always = index.always_skills();
        assert_eq!(always.len(), 1);
        assert_eq!(always[0].name, "always-skill");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_skill() {
        let dir = temp_skills_dir();
        create_test_skill(
            &dir,
            "general",
            "load-test",
            "# Load Test\n\nFull content here",
        );

        let mut index = SkillIndex::build_from_dir(&dir);
        assert!(!index.is_loaded("load-test"));

        let content = index.load_skill("load-test").unwrap();
        assert!(content.contains("Full content here"));
        assert!(index.is_loaded("load-test"));

        // 再次加载应返回缓存
        let content2 = index.load_skill("load-test").unwrap();
        assert_eq!(content, content2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_unload_skill() {
        let dir = temp_skills_dir();
        create_test_skill(&dir, "general", "unload-test", "# Unload Test");

        let mut index = SkillIndex::build_from_dir(&dir);
        index.load_skill("unload-test").unwrap();
        assert!(index.is_loaded("unload-test"));

        index.unload_skill("unload-test");
        assert!(!index.is_loaded("unload-test"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_to_prompt_summary() {
        let dir = temp_skills_dir();
        create_test_skill(
            &dir,
            "devops",
            "deploy",
            "---\ndescription: Deploy apps\ntools: [exec]\n---\n\n# Deploy",
        );

        let index = SkillIndex::build_from_dir(&dir);
        let summary = index.to_prompt_summary();
        assert!(summary.contains("Available Skills"));
        assert!(summary.contains("deploy"));
        assert!(summary.contains("devops"));
        assert!(summary.contains("skill_view"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_to_prompt_summary_empty() {
        let index = SkillIndex::new();
        let summary = index.to_prompt_summary();
        assert!(summary.is_empty());
    }

    #[test]
    fn test_refresh() {
        let dir = temp_skills_dir();
        create_test_skill(&dir, "general", "skill1", "# Skill 1");

        let mut index = SkillIndex::build_from_dir(&dir);
        assert_eq!(index.entries().len(), 1);

        // 添加新 skill
        create_test_skill(&dir, "general", "skill2", "# Skill 2");
        index.refresh(&dir);
        assert_eq!(index.entries().len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_nonexistent_dir() {
        let index = SkillIndex::build_from_dir(Path::new("/nonexistent/path"));
        assert!(index.entries().is_empty());
    }

    #[test]
    fn test_extract_description_from_heading() {
        let content = "# My Great Skill\n\nSome content";
        let desc = SkillIndex::extract_description(content);
        assert_eq!(desc, "My Great Skill");
    }

    #[test]
    fn test_extract_description_from_frontmatter() {
        let content = "---\ndescription: A test skill\n---\n\n# Title";
        let desc = SkillIndex::extract_description(content);
        assert_eq!(desc, "A test skill");
    }

    #[test]
    fn test_extract_gbrain_frontmatter_blocks() {
        let content = r#"---
name: query
version: 1.0.0
description: |
  Answer questions using the brain's knowledge with search and citations.
tools:
  - search
  - query
  - get_page
---

# Query Skill
"#;
        let desc = SkillIndex::extract_description(content);
        let tools = SkillIndex::extract_tools(content);
        assert!(desc.contains("brain's knowledge"));
        assert_eq!(tools, vec!["search", "query", "get_page"]);
        assert_eq!(SkillIndex::extract_version(content), "1.0.0");
    }

    #[test]
    fn test_match_by_tools() {
        let dir = temp_skills_dir();
        create_test_skill(
            &dir,
            "devops",
            "deploy",
            "---\ndescription: Deploy apps\ntools: [exec, write_file]\n---\n\n# Deploy",
        );
        create_test_skill(
            &dir,
            "general",
            "hello",
            "---\ndescription: Hello\ntools: [read_file]\n---\n\n# Hello",
        );
        create_test_skill(
            &dir,
            "general",
            "no-tools",
            "---\ndescription: No tools\n---\n\n# No Tools",
        );

        let index = SkillIndex::build_from_dir(&dir);

        // 匹配 exec 工具
        let matched = index.match_by_tools(&["exec".to_string()]);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].name, "deploy");

        // 匹配多个工具 (交集大的排前面)
        let matched = index.match_by_tools(&[
            "exec".to_string(),
            "write_file".to_string(),
            "read_file".to_string(),
        ]);
        assert_eq!(matched.len(), 2);
        assert_eq!(matched[0].name, "deploy"); // 交集=2, 排前面
        assert_eq!(matched[1].name, "hello"); // 交集=1

        // 空工具列表
        let matched = index.match_by_tools(&[]);
        assert!(matched.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_fuzzy_search() {
        let dir = temp_skills_dir();
        create_test_skill(
            &dir,
            "devops",
            "flask-deploy",
            "---\ndescription: Deploy Flask apps to Kubernetes\ntools: [exec, write_file]\n---\n\n# Flask Deploy",
        );
        create_test_skill(
            &dir,
            "general",
            "rust-debug",
            "---\ndescription: Debug Rust compilation errors\ntools: [exec, read_file]\n---\n\n# Rust Debug",
        );
        create_test_skill(&dir, "general", "hello", "# Hello World");

        let index = SkillIndex::build_from_dir(&dir);

        // 按名称精确匹配
        let results = index.fuzzy_search("flask-deploy");
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "flask-deploy");

        // 按描述搜索
        let results = index.fuzzy_search("kubernetes");
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "flask-deploy");

        // 按名称模糊搜索
        let results = index.fuzzy_search("rust");
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "rust-debug");

        // 空查询
        let results = index.fuzzy_search("");
        assert!(results.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_version_and_updated_at() {
        let dir = temp_skills_dir();
        let skill_dir = dir.join("general").join("versioned-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        // 写 meta.json 带版本号
        std::fs::write(
            skill_dir.join("meta.json"),
            r#"{"description":"Versioned skill","version":"2.1.0","updated_at":"2025-01-15T10:30:00Z"}"#,
        ).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Versioned Skill").unwrap();

        let index = SkillIndex::build_from_dir(&dir);
        let entry = index.entries().get("general/versioned-skill").unwrap();
        assert_eq!(entry.version, "2.1.0");
        assert_eq!(entry.updated_at.as_deref(), Some("2025-01-15T10:30:00Z"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_extract_version_from_frontmatter() {
        let content = "---\nversion: 3.0.1\n---\n\n# Title";
        let version = SkillIndex::extract_version(content);
        assert_eq!(version, "3.0.1");
    }

    #[test]
    fn test_capabilities_fallback_in_meta_yaml() {
        let dir = temp_skills_dir();
        let skill_dir = dir.join("general").join("legacy-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        // meta.yaml with capabilities but no tools
        std::fs::write(
            skill_dir.join("meta.yaml"),
            "name: legacy-skill\ndescription: Legacy skill with capabilities\ncapabilities:\n  - web_fetch\n  - web_search\n",
        ).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Legacy Skill").unwrap();

        let index = SkillIndex::build_from_dir(&dir);
        let entry = index.entries().get("general/legacy-skill").unwrap();
        assert_eq!(entry.tools, vec!["web_fetch", "web_search"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_meta_only_skill() {
        let dir = temp_skills_dir();
        let skill_dir = dir.join("general").join("metadata-only");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("meta.yaml"),
            "name: metadata-only\ndescription: Metadata only skill\n",
        )
        .unwrap();

        let mut index = SkillIndex::build_from_dir(&dir);
        let content = index.load_skill("general/metadata-only").unwrap();
        assert!(content.contains("Metadata only skill"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_capabilities_fallback_in_meta_json() {
        let dir = temp_skills_dir();
        let skill_dir = dir.join("general").join("json-legacy-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        // meta.json with capabilities but no tools
        std::fs::write(
            skill_dir.join("meta.json"),
            r#"{"description":"JSON legacy skill","capabilities":["exec","read_file"]}"#,
        )
        .unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# JSON Legacy Skill").unwrap();

        let index = SkillIndex::build_from_dir(&dir);
        let entry = index.entries().get("general/json-legacy-skill").unwrap();
        assert_eq!(entry.tools, vec!["exec", "read_file"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tools_preferred_over_capabilities() {
        let dir = temp_skills_dir();
        let skill_dir = dir.join("general").join("both-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        // meta.yaml with both tools and capabilities — tools should win
        std::fs::write(
            skill_dir.join("meta.yaml"),
            "name: both-skill\ndescription: Skill with both\ntools:\n  - finance_api\n  - chart_generate\ncapabilities:\n  - web_fetch\n",
        ).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Both Skill").unwrap();

        let index = SkillIndex::build_from_dir(&dir);
        let entry = index.entries().get("general/both-skill").unwrap();
        assert_eq!(entry.tools, vec!["finance_api", "chart_generate"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
