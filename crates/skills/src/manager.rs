use crate::service::{EvolutionService, EvolutionServiceConfig};
use crate::versioning::{VersionManager, VersionSource};
use blockcell_core::{Error, Paths, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

mod markdown;
use markdown::{
    concise_markdown_excerpt, extract_markdown_links, extract_section_by_anchor, find_section,
    is_link_only_line, is_local_markdown_target, join_markdown_parts, parse_markdown_sections,
    resolve_markdown_path, split_markdown_target, MarkdownSection,
};

/// 技能来源标识，区分 BlockCell 原生格式和 OpenClaw 格式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SkillSource {
    /// BlockCell 原生格式 (meta.yaml + 锚点区块 SKILL.md)
    #[default]
    BlockCell,
    /// OpenClaw 格式 (YAML frontmatter SKILL.md)
    OpenClaw,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Explicit tools this skill may use when activated.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Legacy compatibility field. Older skills stored visible tools here.
    /// New skills should use `tools`.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Output format hint (e.g. "markdown", "json", "table").
    #[serde(default)]
    pub output_format: Option<String>,
    /// Fallback strategy when the skill fails.
    #[serde(default)]
    pub fallback: Option<SkillFallback>,
    /// 由模型学习生成（Ghost Learning / skill_manage），其指令的信任级别低于系统策略。
    #[serde(default)]
    pub learned: bool,

    // --- OpenClaw 兼容字段 ---
    /// 技能来源（程序设置，不从 YAML 反序列化）
    #[serde(skip)]
    pub source: SkillSource,
    /// 显示图标 (OpenClaw emoji)
    #[serde(default)]
    pub emoji: Option<String>,
    /// 支持的操作系统列表 (OpenClaw: "darwin", "linux", "win32")
    #[serde(default)]
    pub os: Option<Vec<String>>,
    /// 用户是否可直接调用（默认 true）
    #[serde(default = "default_true")]
    pub user_invocable: bool,
    /// 是否禁止模型自动调用（默认 false）
    #[serde(default)]
    pub disable_model_invocation: bool,
    /// OpenClaw 斜杠命令定义
    #[serde(default)]
    pub commands: Vec<SkillCommandSpec>,
    /// OpenClaw 安装规格
    #[serde(default)]
    pub install: Vec<SkillInstallSpec>,
}

impl Default for SkillMeta {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            requires: SkillRequires::default(),
            permissions: Vec::new(),
            always: false,
            tools: Vec::new(),
            capabilities: Vec::new(),
            output_format: None,
            fallback: None,
            learned: false,
            source: SkillSource::BlockCell,
            emoji: None,
            os: None,
            user_invocable: true,
            disable_model_invocation: false,
            commands: Vec::new(),
            install: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillRequires {
    #[serde(default)]
    pub bins: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    /// OpenClaw: 任一存在即可的二进制程序
    #[serde(default)]
    pub any_bins: Vec<String>,
    /// OpenClaw: 必需的配置路径
    #[serde(default)]
    pub config: Vec<String>,
}

/// OpenClaw 斜杠命令定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCommandSpec {
    pub name: String,
    pub skill_name: String,
    pub description: String,
    #[serde(default)]
    pub dispatch: Option<SkillCommandDispatch>,
    #[serde(default)]
    pub prompt_template: Option<String>,
}

/// OpenClaw 命令分发配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCommandDispatch {
    pub kind: String,
    pub tool_name: String,
    #[serde(default)]
    pub arg_mode: Option<String>,
}

/// OpenClaw 安装规格
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillInstallSpec {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub bins: Vec<String>,
    #[serde(default)]
    pub os: Option<Vec<String>>,
    #[serde(default)]
    pub formula: Option<String>,
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
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

impl SkillMeta {
    pub fn effective_tools(&self) -> Vec<String> {
        if !self.tools.is_empty() {
            self.tools.clone()
        } else {
            self.capabilities.clone()
        }
    }
}

#[derive(Debug, Clone)]
struct SkillDocCache {
    root_md: String,
    linked_docs: HashMap<PathBuf, String>,
    linked_sections: HashMap<String, String>,
    prompt_bundle: String,
    planning_bundle: String,
    summary_bundle: String,
}

impl SkillDocCache {
    fn load(skill_dir: &Path) -> Result<Option<Self>> {
        let root_path = skill_dir.join("SKILL.md");
        if !root_path.exists() {
            return Ok(None);
        }

        let root_md = std::fs::read_to_string(&root_path)?;
        let sections = parse_markdown_sections(&root_md);
        let has_reserved_sections = ["shared", "prompt", "planning", "summary"]
            .iter()
            .any(|anchor| find_section(&sections, anchor).is_some());

        let mut cache = Self {
            root_md: root_md.clone(),
            linked_docs: HashMap::new(),
            linked_sections: HashMap::new(),
            prompt_bundle: String::new(),
            planning_bundle: String::new(),
            summary_bundle: String::new(),
        };

        if !has_reserved_sections {
            let fallback = root_md.trim().to_string();
            cache.prompt_bundle = fallback.clone();
            cache.planning_bundle = fallback.clone();
            cache.summary_bundle = fallback;
            return Ok(Some(cache));
        }

        let shared = cache
            .compile_root_section(skill_dir, &root_md, &sections, "shared")?
            .unwrap_or_default();
        let prompt = cache
            .compile_root_section(skill_dir, &root_md, &sections, "prompt")?
            .unwrap_or_default();
        let planning = cache
            .compile_root_section(skill_dir, &root_md, &sections, "planning")?
            .unwrap_or_default();
        let summary = cache
            .compile_root_section(skill_dir, &root_md, &sections, "summary")?
            .unwrap_or_default();

        cache.prompt_bundle = join_markdown_parts(&[shared.as_str(), prompt.as_str()]);
        cache.planning_bundle = join_markdown_parts(&[shared.as_str(), planning.as_str()]);
        cache.summary_bundle = join_markdown_parts(&[shared.as_str(), summary.as_str()]);

        Ok(Some(cache))
    }

    fn compile_root_section(
        &mut self,
        skill_dir: &Path,
        root_md: &str,
        sections: &[MarkdownSection],
        anchor: &str,
    ) -> Result<Option<String>> {
        let Some(section) = find_section(sections, anchor) else {
            return Ok(None);
        };
        let raw_section = &root_md[section.start..section.end];
        let expanded = self.expand_root_links(skill_dir, raw_section)?;
        Ok(Some(expanded.trim().to_string()))
    }

    fn expand_root_links(&mut self, skill_dir: &Path, text: &str) -> Result<String> {
        let mut output = String::new();

        for line in text.split_inclusive('\n') {
            let local_links = extract_markdown_links(line)
                .into_iter()
                .filter(|link| is_local_markdown_target(&link.target))
                .collect::<Vec<_>>();

            if local_links.is_empty() {
                output.push_str(line);
                continue;
            }

            if local_links.len() == 1 && is_link_only_line(line, &local_links[0]) {
                let included = self.resolve_link_target(skill_dir, &local_links[0].target)?;
                if !included.is_empty() {
                    output.push_str(&included);
                    if !included.ends_with('\n') {
                        output.push('\n');
                    }
                }
                continue;
            }

            let mut last = 0usize;
            for link in local_links {
                output.push_str(&line[last..link.start]);
                output.push_str(&self.resolve_link_target(skill_dir, &link.target)?);
                last = link.end;
            }
            output.push_str(&line[last..]);
        }

        Ok(output)
    }

    fn resolve_link_target(&mut self, skill_dir: &Path, target: &str) -> Result<String> {
        let Some((relative_path, section_anchor)) = split_markdown_target(target) else {
            return Ok(target.to_string());
        };
        let canonical_path = resolve_markdown_path(skill_dir, relative_path)?;
        let doc_text = if let Some(existing) = self.linked_docs.get(&canonical_path) {
            existing.clone()
        } else {
            let content = std::fs::read_to_string(&canonical_path)?;
            self.linked_docs
                .insert(canonical_path.clone(), content.clone());
            content
        };

        if let Some(anchor) = section_anchor {
            let index_key = format!("{}#{}", canonical_path.display(), anchor);
            if let Some(existing) = self.linked_sections.get(&index_key) {
                return Ok(existing.clone());
            }

            let extracted = extract_section_by_anchor(&doc_text, anchor).ok_or_else(|| {
                blockcell_core::Error::Skill(format!(
                    "Skill markdown section '{}' not found in {}",
                    anchor,
                    canonical_path.display()
                ))
            })?;
            self.linked_sections.insert(index_key, extracted.clone());
            Ok(extracted)
        } else {
            Ok(doc_text)
        }
    }
}

fn skill_supports_local_exec(skill: &Skill, _manual: &str) -> bool {
    if skill.path.join("SKILL.py").exists() || skill.path.join("SKILL.rhai").exists() {
        return true;
    }

    if skill_dir_contains_local_script(&skill.path) {
        return true;
    }

    false
}

fn skill_dir_contains_local_script(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if skill_dir_contains_local_script(&path) {
                return true;
            }
            continue;
        }

        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext, "py" | "sh" | "php" | "js" | "ts" | "rb"))
        {
            return true;
        }
    }

    false
}

fn skill_local_exec_entrypoints(skill: &Skill) -> Vec<String> {
    let mut entrypoints = Vec::new();

    for candidate in ["SKILL.py", "SKILL.rhai"] {
        if skill.path.join(candidate).exists() {
            entrypoints.push(candidate.to_string());
        }
    }

    for directory in ["scripts", "bin"] {
        if skill_dir_contains_local_script(&skill.path.join(directory)) {
            entrypoints.push(format!("{}/", directory));
        }
    }

    let Ok(entries) = std::fs::read_dir(&skill.path) else {
        entrypoints.sort();
        entrypoints.dedup();
        return entrypoints;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if matches!(
            file_name,
            "SKILL.md" | "SKILL.py" | "SKILL.rhai" | "meta.yaml" | "meta.json" | "CHANGELOG.md"
        ) {
            continue;
        }

        let ext_ok = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext, "py" | "sh" | "php" | "js" | "ts" | "rb"));
        let no_ext_exec = path.extension().is_none() && looks_executable(&path);
        if ext_ok || no_ext_exec {
            if let Ok(rel_path) = path.strip_prefix(&skill.path) {
                entrypoints.push(rel_path.display().to_string());
            }
        }
    }

    entrypoints.sort();
    entrypoints.dedup();
    entrypoints
}

fn skill_execution_layout(skill: &Skill, manual: &str) -> String {
    let has_rhai = skill.path.join("SKILL.rhai").exists();
    let supports_local_exec = skill_supports_local_exec(skill, manual);

    match (has_rhai, supports_local_exec) {
        (true, true) => "RhaiOrchestration + LocalScript".to_string(),
        (true, false) => "RhaiOrchestration".to_string(),
        (false, true) => "PromptTool + LocalScript".to_string(),
        (false, false) => "PromptTool".to_string(),
    }
}

#[cfg(unix)]
fn looks_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn looks_executable(_path: &Path) -> bool {
    false
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillCard {
    pub name: String,
    pub description: String,
    pub execution_layout: String,
    pub when_to_use: String,
    pub outputs: String,
    pub allowed_tools: Vec<String>,
    pub local_exec_entrypoints: Vec<String>,
    pub supports_local_exec: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillPromptSnapshot {
    version: u32,
    generated_at: String,
    skills: Vec<SkillPromptSnapshotEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillPromptSnapshotEntry {
    name: String,
    description: String,
    source: SkillSource,
    available: bool,
    path: String,
    prompt_bundle: String,
    summary_bundle: String,
    tools: Vec<String>,
}

const SKILLS_PROMPT_SNAPSHOT_FILE: &str = ".skills_prompt_snapshot.json";

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub path: PathBuf,
    pub meta: SkillMeta,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub current_version: Option<String>,
    /// Root SKILL.md and compiled phase bundles cached at load time.
    cached_docs: Option<SkillDocCache>,
    /// OpenClaw skill 解析后的 prompt 正文（已替换 {baseDir}）。
    /// BlockCell 原生 skill 此字段为 None，使用 cached_docs。
    prompt_override: Option<String>,
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

    /// Return the SKILL.md content, using the in-memory cache populated at load time.
    pub fn load_md(&self) -> Option<String> {
        if let Some(cached_docs) = &self.cached_docs {
            return Some(cached_docs.root_md.clone());
        }
        // Fallback: read from disk (e.g. if skill was constructed outside SkillManager)
        let md_path = self.path.join("SKILL.md");
        std::fs::read_to_string(md_path).ok()
    }

    pub fn load_prompt_bundle(&self) -> Option<String> {
        // OpenClaw skill 使用 prompt_override
        if let Some(ref override_md) = self.prompt_override {
            return Some(override_md.clone());
        }
        self.cached_docs
            .as_ref()
            .map(|cached_docs| cached_docs.prompt_bundle.clone())
            .or_else(|| self.load_md())
    }

    pub fn load_planning_bundle(&self) -> Option<String> {
        if let Some(ref override_md) = self.prompt_override {
            return Some(override_md.clone());
        }
        self.cached_docs
            .as_ref()
            .map(|cached_docs| cached_docs.planning_bundle.clone())
            .or_else(|| self.load_md())
    }

    pub fn load_summary_bundle(&self) -> Option<String> {
        if let Some(ref override_md) = self.prompt_override {
            return Some(override_md.clone());
        }
        self.cached_docs
            .as_ref()
            .map(|cached_docs| cached_docs.summary_bundle.clone())
            .or_else(|| self.load_md())
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
                if path.extension().is_some_and(|e| e == "json") {
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
    /// Optional tool parameters used by some fixtures.
    #[serde(default)]
    pub params: serde_json::Value,
    /// Optional constraint hints used by some fixtures.
    #[serde(default)]
    pub constraints: serde_json::Value,
}

// ---------------------------------------------------------------------------
// OpenClaw 兼容：格式检测与可用性检查
// ---------------------------------------------------------------------------

/// Escape text for inclusion in an XML text node / attribute value.
///
/// Skill names and descriptions come from disk (possibly model-generated); a
/// raw `<`/`&` would corrupt the prompt fragment built by `get_summary_xml`.
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Read a skill metadata file with a hard size limit.
///
/// `meta.yaml`/`meta.json` are tiny by contract; an oversized or corrupted
/// file must fail loudly rather than being slurped into memory.
fn read_meta_bounded(path: &Path, max_bytes: u64) -> Result<String> {
    let size = std::fs::metadata(path)?.len();
    if size > max_bytes {
        return Err(Error::Other(format!(
            "{} exceeds {} byte limit (size: {})",
            path.display(),
            max_bytes,
            size
        )));
    }
    Ok(std::fs::read_to_string(path)?)
}

/// 检测技能目录的格式来源。
///
/// 优先级规则：
/// 1. 有 meta.yaml 或 meta.json → BlockCell（优先，避免误判）
/// 2. SKILL.md 以 "---" 开头 → OpenClaw
/// 3. 默认 → BlockCell
fn detect_skill_format(skill_dir: &Path) -> SkillSource {
    // 优先检查 BlockCell 标志文件
    if skill_dir.join("meta.yaml").exists() || skill_dir.join("meta.json").exists() {
        return SkillSource::BlockCell;
    }

    // 检查 SKILL.md 是否使用 YAML frontmatter
    let skill_md = skill_dir.join("SKILL.md");
    if skill_md.exists() {
        if let Ok(file) = std::fs::File::open(&skill_md) {
            use std::io::Read;
            let mut buf = [0u8; 16];
            let mut reader = std::io::BufReader::new(file);
            if let Ok(n) = reader.read(&mut buf) {
                let prefix = &buf[..n];
                // 支持 UTF-8 BOM（\xEF\xBB\xBF）
                let prefix = prefix.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(prefix);
                if prefix.starts_with(b"---\n") || prefix.starts_with(b"---\r\n") {
                    return SkillSource::OpenClaw;
                }
            }
        }
    }

    SkillSource::BlockCell
}

fn is_gbrain_skill_dir(skill_dir: &Path) -> bool {
    let Some(root) = skill_dir.parent() else {
        return false;
    };
    let manifest_path = root.join("manifest.json");
    let Ok(content) = std::fs::read_to_string(manifest_path) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    manifest
        .get("conformance_version")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value == "gbrain-rs")
}

/// OpenClaw 专用可用性检查。
///
/// 检查 bins、any_bins、env、config 路径。
/// 返回 (available, reason) 与现有 check_availability 签名一致。
fn check_openclaw_availability(meta: &SkillMeta) -> (bool, Option<String>) {
    // 检查必需的 bins
    for bin in &meta.requires.bins {
        if which::which(bin).is_err() {
            return (false, Some(format!("Missing binary: {}", bin)));
        }
    }

    // 检查 any_bins (任一存在即可)
    if !meta.requires.any_bins.is_empty() {
        let any_found = meta
            .requires
            .any_bins
            .iter()
            .any(|bin| which::which(bin).is_ok());
        if !any_found {
            return (
                false,
                Some(format!(
                    "Missing any of: {}",
                    meta.requires.any_bins.join(", ")
                )),
            );
        }
    }

    // 检查必需的环境变量
    for env_var in &meta.requires.env {
        if std::env::var(env_var).is_err() {
            return (false, Some(format!("Missing env var: {}", env_var)));
        }
    }

    // 检查 config 路径
    for config_path in &meta.requires.config {
        let expanded = shellexpand::tilde(config_path);
        if !Path::new(expanded.as_ref()).exists() {
            return (false, Some(format!("Missing config: {}", config_path)));
        }
    }

    (true, None)
}

#[derive(Clone)]
pub struct SkillManager {
    skills: HashMap<String, Skill>,
    version_manager: Option<VersionManager>,
    evolution_service: Option<EvolutionService>,
    /// Known available capability IDs (synced from CapabilityRegistry)
    available_capabilities: std::collections::HashSet<String>,
    /// 是否启用 OpenClaw skill 兼容加载
    openclaw_skill_enabled: bool,
}

impl SkillManager {
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
            version_manager: None,
            evolution_service: None,
            available_capabilities: std::collections::HashSet::new(),
            openclaw_skill_enabled: false,
        }
    }

    /// 设置是否启用 OpenClaw skill 兼容加载
    pub fn set_openclaw_skill_enabled(&mut self, enabled: bool) {
        self.openclaw_skill_enabled = enabled;
    }

    /// 检查指定技能是否为 OpenClaw 来源
    pub fn is_openclaw_skill(&self, name: &str) -> bool {
        self.skills
            .get(name)
            .map(|s| s.meta.source == SkillSource::OpenClaw)
            .unwrap_or(false)
    }

    /// 检查指定工具名是否属于某个 OpenClaw 技能（技能名或其工具列表中包含该名称）
    pub fn is_tool_from_openclaw(&self, tool_name: &str) -> bool {
        if self.is_openclaw_skill(tool_name) {
            return true;
        }
        self.skills.values().any(|s| {
            s.meta.source == SkillSource::OpenClaw && s.meta.tools.iter().any(|t| t == tool_name)
        })
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
            for tool_name in skill.meta.effective_tools() {
                if !self.available_capabilities.contains(&tool_name)
                    && !crate::service::is_builtin_tool(&tool_name)
                    && !tool_name.contains("__")
                {
                    missing.push((skill.name.clone(), tool_name));
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

    pub fn evolution_service_mut(&mut self) -> Option<&mut EvolutionService> {
        self.evolution_service.as_mut()
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

        self.write_prompt_snapshot(paths)?;

        Ok(())
    }

    /// Re-scan skill directories and pick up any newly created or modified skills.
    /// Returns the names of newly discovered skills (not previously loaded).
    pub fn reload_skills(&mut self, paths: &Paths) -> Result<Vec<String>> {
        let before: std::collections::HashSet<String> = self.skills.keys().cloned().collect();
        Self::invalidate_prompt_snapshot(paths)?;
        let previous_skills = std::mem::take(&mut self.skills);
        if let Err(error) = self.load_from_paths(paths) {
            self.skills = previous_skills;
            return Err(error);
        }
        let new_skills: Vec<String> = self
            .skills
            .keys()
            .filter(|k| !before.contains(*k))
            .cloned()
            .collect();
        if !new_skills.is_empty() {
            info!(new_skills = ?new_skills, "Hot-reloaded new skills");
        }
        Ok(new_skills)
    }

    /// Invalidate the doc cache for a specific skill after evolution deployment.
    /// This ensures the next access reads fresh SKILL.md content from disk.
    pub fn invalidate_skill_cache(&mut self, skill_name: &str) {
        if let Some(skill) = self.skills.get_mut(skill_name) {
            skill.cached_docs = None;
            debug!(skill = %skill_name, "Invalidated doc cache for skill after evolution deploy");
        }
    }

    pub fn invalidate_prompt_snapshot(paths: &Paths) -> Result<()> {
        let snapshot_path = paths.skills_dir().join(SKILLS_PROMPT_SNAPSHOT_FILE);
        if snapshot_path.exists() {
            std::fs::remove_file(snapshot_path)?;
        }
        Ok(())
    }

    fn write_prompt_snapshot(&self, paths: &Paths) -> Result<()> {
        let skills_dir = paths.skills_dir();
        std::fs::create_dir_all(&skills_dir)?;
        let mut entries = self
            .skills
            .values()
            .map(|skill| SkillPromptSnapshotEntry {
                name: skill.name.clone(),
                description: skill.meta.description.clone(),
                source: skill.meta.source.clone(),
                available: skill.available,
                path: skill.path.display().to_string(),
                prompt_bundle: skill.load_prompt_bundle().unwrap_or_default(),
                summary_bundle: skill.load_summary_bundle().unwrap_or_default(),
                tools: skill.meta.effective_tools(),
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        let snapshot = SkillPromptSnapshot {
            version: 1,
            generated_at: Utc::now().to_rfc3339(),
            skills: entries,
        };
        let snapshot_path = skills_dir.join(SKILLS_PROMPT_SNAPSHOT_FILE);
        let tmp_path = snapshot_path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::write(&tmp_path, serde_json::to_string_pretty(&snapshot)?)?;
        if let Err(e) = std::fs::rename(&tmp_path, &snapshot_path) {
            // On Windows, rename may fail if the target is open. Fall back to
            // copy+remove, and log a warning about the orphan temp file.
            warn!(error = %e, "Failed to rename snapshot temp file, falling back to copy+remove");
            std::fs::copy(&tmp_path, &snapshot_path)?;
            let _ = std::fs::remove_file(&tmp_path);
        }
        Ok(())
    }

    fn scan_directory_with_priority(&mut self, dir: &PathBuf, is_workspace: bool) -> Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;

            if file_type.is_symlink() || !file_type.is_dir() {
                continue;
            }

            // Skill 包目录：包含 manifest.json 的目录，其子目录是独立 skill。
            // 递归扫描子目录，逐个加载每个子 skill。
            if path.join("manifest.json").exists() {
                debug!(path = %path.display(), "Scanning skill pack");
                self.scan_directory_with_priority(&path, is_workspace)?;
                continue;
            }

            // 跳过没有 skill 标识文件的目录，避免将资料目录（如 conventions/）
            // 误注册为空 skill。
            if !path.join("SKILL.md").exists()
                && !path.join("meta.yaml").exists()
                && !path.join("meta.json").exists()
            {
                continue;
            }

            let skill = match self.load_skill(&path) {
                Ok(skill) => skill,
                Err(error) => {
                    warn!(
                        path = %path.display(),
                        error = %error,
                        "Failed to load skill, skipping isolated invalid entry"
                    );
                    continue;
                }
            };

            if let Some(skill) = skill {
                let skill_name = skill.name.clone();

                // Workspace skills override built-in skills
                if is_workspace || !self.skills.contains_key(&skill_name) {
                    let source = if is_workspace {
                        "workspace"
                    } else {
                        "built-in"
                    };
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

        Ok(())
    }

    fn load_skill(&self, skill_dir: &std::path::Path) -> Result<Option<Skill>> {
        let source = detect_skill_format(skill_dir);

        match source {
            SkillSource::OpenClaw => {
                if !self.openclaw_skill_enabled && !is_gbrain_skill_dir(skill_dir) {
                    warn!(
                        path = %skill_dir.display(),
                        "Skipping OpenClaw-format skill because openclaw_skill_enabled=false. \
                         Set \"openclawSkillEnabled\": true in config.json5 to enable it."
                    );
                    return Ok(None);
                }
                self.load_openclaw_skill(skill_dir)
            }
            SkillSource::BlockCell => self.load_blockcell_skill(skill_dir),
        }
    }

    /// 加载 BlockCell 原生格式技能（现有逻辑，未修改）
    fn load_blockcell_skill(&self, skill_dir: &std::path::Path) -> Result<Option<Skill>> {
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

        let cached_docs = SkillDocCache::load(skill_dir)?;
        Ok(Some(Skill {
            name: if meta.name.is_empty() {
                name
            } else {
                meta.name.clone()
            },
            path: skill_dir.to_path_buf(),
            meta,
            available,
            unavailable_reason: reason,
            current_version,
            cached_docs,
            prompt_override: None,
        }))
    }

    /// 加载 OpenClaw 格式技能
    fn load_openclaw_skill(&self, skill_dir: &std::path::Path) -> Result<Option<Skill>> {
        let skill_md_path = skill_dir.join("SKILL.md");

        // 文件大小检查（对齐 OpenClaw 的 256KB 限制）
        let file_meta = std::fs::metadata(&skill_md_path)?;
        if file_meta.len() > 256 * 1024 {
            warn!(
                path = %skill_md_path.display(),
                size = file_meta.len(),
                "OpenClaw SKILL.md exceeds 256KB limit, skipping"
            );
            return Ok(None);
        }

        let content = std::fs::read_to_string(&skill_md_path)?;

        // 解析 frontmatter + body
        let (meta, prompt_body) =
            match crate::openclaw_parser::parse_openclaw_skill(skill_dir, &content) {
                Ok(result) => result,
                Err(e) => {
                    warn!(
                        path = %skill_md_path.display(),
                        error = %e,
                        "Failed to parse OpenClaw SKILL.md, skipping"
                    );
                    return Ok(None);
                }
            };

        // OS 过滤
        if let Some(ref os_list) = meta.os {
            let current_os = std::env::consts::OS;
            let mapped = match current_os {
                "windows" => "win32",
                "macos" => "darwin",
                other => other,
            };
            if !os_list.iter().any(|o| o == mapped) {
                info!(
                    skill = %meta.name,
                    current_os = current_os,
                    supported = ?os_list,
                    "OpenClaw skill not supported on this OS, skipping"
                );
                return Ok(None);
            }
        }

        // 可用性检查
        let (available, reason) = check_openclaw_availability(&meta);

        // 如果不可用且有安装提示，记录日志
        if !available {
            if let Some(ref reason_str) = reason {
                let hint = crate::openclaw_parser::generate_install_hint(&meta, reason_str);
                info!(skill = %meta.name, "{}", hint);
            }
        }

        let skill_name = meta.name.clone();
        Ok(Some(Skill {
            name: skill_name,
            path: skill_dir.to_path_buf(),
            meta,
            available,
            unavailable_reason: reason,
            current_version: None, // OpenClaw 技能不使用 BlockCell 版本管理
            cached_docs: None,     // OpenClaw 技能不使用锚点区块缓存
            prompt_override: Some(prompt_body),
        }))
    }

    fn load_meta(&self, skill_dir: &std::path::Path) -> Result<SkillMeta> {
        /// Metadata is small by contract; refuse oversized files instead of
        /// reading them fully into memory (symmetric with the SKILL.md limit).
        const MAX_META_BYTES: u64 = 256 * 1024;

        // Try meta.yaml first
        let yaml_path = skill_dir.join("meta.yaml");
        if yaml_path.exists() {
            let content = read_meta_bounded(&yaml_path, MAX_META_BYTES)?;
            return Ok(serde_yaml::from_str(&content)?);
        }

        // Try meta.json
        let json_path = skill_dir.join("meta.json");
        if json_path.exists() {
            let content = read_meta_bounded(&json_path, MAX_META_BYTES)?;
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

        // Check required tools / legacy capabilities from the registry.
        // Built-in tool ids are always available here; evolved capabilities must exist.
        for tool_name in meta.effective_tools() {
            if tool_name.contains("__") {
                continue;
            }
            if !crate::service::is_builtin_tool(&tool_name)
                && !self.available_capabilities.contains(&tool_name)
            {
                return (false, Some(format!("Missing capability: {}", tool_name)));
            }
        }

        (true, None)
    }

    pub fn get_summary_xml(&self) -> String {
        let mut xml = String::from("<skills>\n");

        for skill in self.skills.values() {
            xml.push_str(&format!("  <skill available=\"{}\">\n", skill.available));
            xml.push_str(&format!("    <name>{}</name>\n", escape_xml(&skill.name)));
            xml.push_str(&format!(
                "    <description>{}</description>\n",
                escape_xml(&skill.meta.description)
            ));
            xml.push_str(&format!(
                "    <location>{}/SKILL.md</location>\n",
                escape_xml(&skill.path.display().to_string())
            ));

            if !skill.available {
                if let Some(reason) = &skill.unavailable_reason {
                    xml.push_str(&format!(
                        "    <requires>{}</requires>\n",
                        escape_xml(reason)
                    ));
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

    pub fn build_skill_card(skill: &Skill) -> SkillCard {
        let cached_docs = skill
            .cached_docs
            .clone()
            .or_else(|| SkillDocCache::load(&skill.path).ok().flatten());
        let manual = cached_docs
            .as_ref()
            .map(|docs| docs.root_md.clone())
            .or_else(|| skill.load_md())
            .unwrap_or_default();
        let prompt_bundle = cached_docs
            .as_ref()
            .map(|docs| docs.prompt_bundle.clone())
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| manual.clone());
        let summary_bundle = cached_docs
            .as_ref()
            .map(|docs| docs.summary_bundle.clone())
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| manual.clone());
        let shared_section = extract_section_by_anchor(&manual, "shared").unwrap_or_default();
        let prompt_section = extract_section_by_anchor(&manual, "prompt").unwrap_or_default();
        let summary_section = extract_section_by_anchor(&manual, "summary").unwrap_or_default();
        let when_to_use_source =
            join_markdown_parts(&[shared_section.as_str(), prompt_section.as_str()]);
        let when_to_use = concise_markdown_excerpt(
            if when_to_use_source.trim().is_empty() {
                &prompt_bundle
            } else {
                &when_to_use_source
            },
            220,
        );
        let outputs = concise_markdown_excerpt(
            if summary_section.trim().is_empty() {
                &summary_bundle
            } else {
                &summary_section
            },
            220,
        );

        SkillCard {
            name: skill.name.clone(),
            description: skill.meta.description.clone(),
            execution_layout: skill_execution_layout(skill, &manual),
            when_to_use: if when_to_use.is_empty() {
                concise_markdown_excerpt(&manual, 220)
            } else {
                when_to_use
            },
            outputs: if outputs.is_empty() {
                skill
                    .meta
                    .output_format
                    .clone()
                    .unwrap_or_else(|| "Answer the user's request directly.".to_string())
            } else {
                outputs
            },
            allowed_tools: skill.meta.effective_tools(),
            local_exec_entrypoints: skill_local_exec_entrypoints(skill),
            supports_local_exec: skill_supports_local_exec(skill, &manual),
        }
    }

    pub fn list_enabled_skills<'a>(&'a self, disabled_skills: &HashSet<String>) -> Vec<&'a Skill> {
        self.skills
            .values()
            .filter(|s| s.available && !disabled_skills.contains(&s.name))
            .collect()
    }

    pub fn list_enabled_skill_cards(&self, disabled_skills: &HashSet<String>) -> Vec<SkillCard> {
        self.list_enabled_skills(disabled_skills)
            .into_iter()
            .map(Self::build_skill_card)
            .collect()
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
        let vm = self.version_manager.as_ref().ok_or_else(|| {
            blockcell_core::Error::Other("Version manager not initialized".to_string())
        })?;

        vm.create_version(skill_name, source, changelog)?;
        info!(skill = %skill_name, "Created new skill version");
        Ok(())
    }

    /// 切换到指定版本
    pub fn switch_version(&self, skill_name: &str, version: &str) -> Result<()> {
        let vm = self.version_manager.as_ref().ok_or_else(|| {
            blockcell_core::Error::Other("Version manager not initialized".to_string())
        })?;

        vm.switch_to_version(skill_name, version)?;
        Ok(())
    }

    /// 回滚到上一个版本
    pub fn rollback_version(&self, skill_name: &str) -> Result<()> {
        let vm = self.version_manager.as_ref().ok_or_else(|| {
            blockcell_core::Error::Other("Version manager not initialized".to_string())
        })?;

        vm.rollback(skill_name)?;
        Ok(())
    }

    /// 列出技能的所有版本
    pub fn list_versions(&self, skill_name: &str) -> Result<Vec<crate::versioning::SkillVersion>> {
        let vm = self.version_manager.as_ref().ok_or_else(|| {
            blockcell_core::Error::Other("Version manager not initialized".to_string())
        })?;

        vm.list_versions(skill_name)
    }

    /// 清理旧版本
    pub fn cleanup_old_versions(&self, skill_name: &str, keep_count: usize) -> Result<()> {
        let vm = self.version_manager.as_ref().ok_or_else(|| {
            blockcell_core::Error::Other("Version manager not initialized".to_string())
        })?;

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
        let vm = self.version_manager.as_ref().ok_or_else(|| {
            blockcell_core::Error::Other("Version manager not initialized".to_string())
        })?;

        vm.diff_versions(skill_name, version1, version2)
    }

    /// 导出版本
    pub fn export_version(
        &self,
        skill_name: &str,
        version: &str,
        output_path: &std::path::Path,
    ) -> Result<()> {
        let vm = self.version_manager.as_ref().ok_or_else(|| {
            blockcell_core::Error::Other("Version manager not initialized".to_string())
        })?;

        vm.export_version(skill_name, version, output_path)?;
        Ok(())
    }

    /// 导入版本
    pub fn import_version(&self, skill_name: &str, archive_path: &std::path::Path) -> Result<()> {
        let vm = self.version_manager.as_ref().ok_or_else(|| {
            blockcell_core::Error::Other("Version manager not initialized".to_string())
        })?;

        vm.import_version(skill_name, archive_path)?;
        Ok(())
    }
}

impl Default for SkillManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_skill_dir(prefix: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nonce));
        fs::create_dir_all(&dir).expect("create temp skill dir");
        dir
    }

    #[test]
    fn p3_metadata_size_is_bounded() {
        let root = temp_skill_dir("p3_meta_limit");
        fs::write(root.join("meta.yaml"), "x".repeat(256 * 1024 + 1)).unwrap();
        let error = SkillManager::new().load_meta(&root).unwrap_err();
        assert!(error.to_string().contains("limit"), "{error}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn test_skill_meta_prefers_tools_over_capabilities() {
        let meta: SkillMeta = serde_yaml::from_str(
            r#"
name: stock_analysis
tools:
  - finance_api
  - chart_generate
capabilities:
  - web_fetch
"#,
        )
        .expect("meta should parse");

        assert_eq!(
            meta.effective_tools(),
            vec!["finance_api".to_string(), "chart_generate".to_string()]
        );
    }

    #[test]
    fn test_skill_meta_falls_back_to_capabilities_when_tools_missing() {
        let meta: SkillMeta = serde_yaml::from_str(
            r#"
name: weather
capabilities:
  - web_fetch
  - web_search
"#,
        )
        .expect("legacy meta should parse");

        assert_eq!(
            meta.effective_tools(),
            vec!["web_fetch".to_string(), "web_search".to_string()]
        );
    }

    #[test]
    fn test_skill_manager_lists_enabled_skills_without_free_text_matching() {
        let mut manager = SkillManager::new();
        manager.skills = HashMap::from([
            (
                "disabled_skill".to_string(),
                Skill {
                    name: "disabled_skill".to_string(),
                    path: PathBuf::from("/tmp/disabled_skill"),
                    meta: SkillMeta {
                        name: "disabled_skill".to_string(),
                        ..SkillMeta::default()
                    },
                    available: true,
                    unavailable_reason: None,
                    current_version: None,
                    cached_docs: None,
                    prompt_override: None,
                },
            ),
            (
                "active_skill".to_string(),
                Skill {
                    name: "active_skill".to_string(),
                    path: PathBuf::from("/tmp/active_skill"),
                    meta: SkillMeta {
                        name: "active_skill".to_string(),
                        ..SkillMeta::default()
                    },
                    available: true,
                    unavailable_reason: None,
                    current_version: None,
                    cached_docs: None,
                    prompt_override: None,
                },
            ),
        ]);

        let disabled_skills = HashSet::from(["disabled_skill".to_string()]);
        let listed = manager.list_enabled_skills(&disabled_skills);

        assert_eq!(
            listed
                .into_iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["active_skill"]
        );
    }

    #[test]
    fn test_skill_meta_serializes_without_legacy_fields() {
        let meta: SkillMeta = serde_yaml::from_str(
            r#"
name: demo
description: minimal
permissions:
  - network
requires:
  bins: ["python3"]
fallback:
  strategy: degrade
  message: failed
"#,
        )
        .expect("meta should parse");

        assert_eq!(meta.name, "demo");
        assert_eq!(meta.description, "minimal");
        assert_eq!(meta.permissions, vec!["network"]);
        assert_eq!(meta.requires.bins, vec!["python3"]);
        assert_eq!(
            meta.fallback
                .as_ref()
                .and_then(|fallback| fallback.message.as_deref()),
            Some("failed")
        );

        let value = serde_json::to_value(&meta).expect("serialize skill meta");
        let object = value
            .as_object()
            .expect("skill meta should serialize to an object");
        let keys = object
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let expected = std::collections::HashSet::from([
            "name".to_string(),
            "description".to_string(),
            "requires".to_string(),
            "permissions".to_string(),
            "always".to_string(),
            "tools".to_string(),
            "capabilities".to_string(),
            "output_format".to_string(),
            "fallback".to_string(),
            "learned".to_string(),
            "emoji".to_string(),
            "os".to_string(),
            "user_invocable".to_string(),
            "disable_model_invocation".to_string(),
            "commands".to_string(),
            "install".to_string(),
        ]);
        assert_eq!(keys, expected);
    }

    #[test]
    fn test_skill_doc_bundles_expand_root_markdown_links_in_order() {
        let skill_dir = temp_skill_dir("blockcell-skill-doc-bundles");
        fs::create_dir_all(skill_dir.join("manual")).expect("create manual dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            r#"# Demo Skill

## Shared {#shared}
Shared preface.
- [Shared rules](manual/shared.md#rules)

## Prompt {#prompt}
Prompt rules.

## Planning {#planning}
Planning intro.
- [Build argv](manual/planning.md#build-argv)
Planning tail.

## Summary {#summary}
Summary intro.
- [Final answer](manual/summary.md#final-answer)
"#,
        )
        .expect("write root skill md");
        fs::write(
            skill_dir.join("manual/shared.md"),
            r#"## Shared rules {#rules}
Use cached auth token.
- [Do not expand](nested.md#ignored)
"#,
        )
        .expect("write shared child md");
        fs::write(
            skill_dir.join("manual/planning.md"),
            r#"## Build argv {#build-argv}
argv[0] must be the action.

### Flags
Keep deterministic ordering.

## Other {#other}
ignore me
"#,
        )
        .expect("write planning child md");
        fs::write(
            skill_dir.join("manual/summary.md"),
            r#"## Final answer {#final-answer}
Summarize in Chinese with concise bullets.
"#,
        )
        .expect("write summary child md");

        let manager = SkillManager::new();
        let skill = manager
            .load_skill(&skill_dir)
            .expect("load skill result")
            .expect("skill should load");

        let prompt_bundle = skill
            .load_prompt_bundle()
            .expect("prompt bundle should be cached");
        let planning_bundle = skill
            .load_planning_bundle()
            .expect("planning bundle should be cached");
        let summary_bundle = skill
            .load_summary_bundle()
            .expect("summary bundle should be cached");

        assert!(prompt_bundle.contains("Shared preface."));
        assert!(prompt_bundle.contains("Use cached auth token."));
        assert!(prompt_bundle.contains("Prompt rules."));
        assert!(!prompt_bundle.contains("argv[0] must be the action."));

        let shared_index = planning_bundle
            .find("Shared preface.")
            .expect("shared in planning");
        let child_index = planning_bundle
            .find("argv[0] must be the action.")
            .expect("planning child in bundle");
        let tail_index = planning_bundle
            .find("Planning tail.")
            .expect("planning tail in bundle");
        assert!(shared_index < child_index);
        assert!(child_index < tail_index);
        assert!(planning_bundle.contains("Keep deterministic ordering."));
        assert!(planning_bundle.contains("[Do not expand](nested.md#ignored)"));
        assert!(!planning_bundle.contains("ignore me"));

        assert!(summary_bundle.contains("Summary intro."));
        assert!(summary_bundle.contains("Summarize in Chinese with concise bullets."));
        assert!(!summary_bundle.contains("Planning intro."));
    }

    #[test]
    fn test_skill_doc_bundles_reject_parent_path_escape() {
        let skill_dir = temp_skill_dir("blockcell-skill-doc-escape");
        let outside_file = skill_dir
            .parent()
            .expect("temp skill dir should have parent")
            .join("secret.md");
        fs::write(&outside_file, "## Hack {#hack}\nsecret\n").expect("write outside file");
        fs::write(
            skill_dir.join("SKILL.md"),
            r#"# Escape Demo

## Planning {#planning}
- [Oops](../secret.md#hack)
"#,
        )
        .expect("write root skill md");

        let manager = SkillManager::new();
        let err = manager
            .load_skill(&skill_dir)
            .expect_err("parent escape should fail");

        assert!(format!("{}", err).contains("outside"));
    }

    #[test]
    fn test_skill_card_is_built_from_meta_and_skill_doc() {
        let skill_dir = temp_skill_dir("blockcell-skill-card");
        fs::write(
            skill_dir.join("meta.yaml"),
            r#"
name: weather
description: 查询天气
tools:
  - web_fetch
"#,
        )
        .expect("write meta");
        fs::write(
            skill_dir.join("SKILL.md"),
            r#"# Weather

## Shared {#shared}
适合查询城市天气、未来天气和天气趋势。

## Prompt {#prompt}
先确认用户想查哪个城市、哪一天。

## Summary {#summary}
输出天气概览、温度和降雨提示。
"#,
        )
        .expect("write skill md");
        fs::write(skill_dir.join("weather.py"), "print('ok')").expect("write local script");

        let skill = Skill {
            name: "weather".to_string(),
            path: skill_dir,
            meta: SkillMeta {
                name: "weather".to_string(),
                description: "查询天气".to_string(),
                tools: vec!["web_fetch".to_string()],
                ..SkillMeta::default()
            },
            available: true,
            unavailable_reason: None,
            current_version: None,
            cached_docs: None,
            prompt_override: None,
        };

        let card = SkillManager::build_skill_card(&skill);
        assert_eq!(card.name, "weather");
        assert_eq!(card.description, "查询天气");
        assert_eq!(card.execution_layout, "PromptTool + LocalScript");
        assert!(card.when_to_use.contains("适合查询城市天气"));
        assert!(card.outputs.contains("输出天气概览"));
        assert_eq!(card.allowed_tools, vec!["web_fetch"]);
        assert!(card
            .local_exec_entrypoints
            .iter()
            .any(|entry| entry == "weather.py"));
        assert!(card.supports_local_exec);
    }

    #[test]
    fn test_skill_prompt_snapshot_is_written_and_invalidated_by_reload() {
        let base = temp_skill_dir("blockcell-skill-prompt-snapshot-root");
        let paths = Paths::with_base(base.clone());
        let skill_dir = paths.skills_dir().join("deploy_docs");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join("meta.yaml"),
            r#"
name: deploy_docs
description: Deploy docs
"#,
        )
        .expect("write meta");
        fs::write(skill_dir.join("SKILL.md"), "Write deploy docs.").expect("write skill md");

        let snapshot_path = paths.skills_dir().join(".skills_prompt_snapshot.json");
        let mut manager = SkillManager::new();
        manager.load_from_paths(&paths).expect("load skills");

        assert!(snapshot_path.exists());
        let first_snapshot = fs::read_to_string(&snapshot_path).expect("read snapshot");
        assert!(first_snapshot.contains("deploy_docs"));
        assert!(first_snapshot.contains("Write deploy docs."));

        fs::write(
            skill_dir.join("SKILL.md"),
            "Write deploy docs with rollback notes.",
        )
        .expect("rewrite skill md");
        manager.reload_skills(&paths).expect("reload skills");

        let second_snapshot = fs::read_to_string(&snapshot_path).expect("read updated snapshot");
        assert!(second_snapshot.contains("rollback notes"));
        assert_ne!(first_snapshot, second_snapshot);
    }

    #[test]
    fn test_reload_removes_deleted_skill() {
        let base = temp_skill_dir("blockcell-skill-reload-delete");
        let paths = Paths::with_base(base.clone());
        let skill_dir = paths.skills_dir().join("temporary_skill");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join("meta.yaml"),
            "name: temporary_skill\ndescription: temporary\n",
        )
        .expect("write meta");
        fs::write(skill_dir.join("SKILL.md"), "Temporary skill.").expect("write skill doc");
        let mut manager = SkillManager::new();
        manager.load_from_paths(&paths).expect("load skill");
        assert!(manager.get("temporary_skill").is_some());

        fs::remove_dir_all(&skill_dir).expect("remove skill");
        manager.reload_skills(&paths).expect("reload skills");

        assert!(manager.get("temporary_skill").is_none());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn test_malformed_skill_does_not_abort_directory_scan() {
        let base = temp_skill_dir("blockcell-skill-malformed-scan");
        let paths = Paths::with_base(base.clone());
        let malformed = paths.skills_dir().join("a_malformed");
        let valid = paths.skills_dir().join("z_valid");
        fs::create_dir_all(&malformed).expect("create malformed skill");
        fs::create_dir_all(&valid).expect("create valid skill");
        fs::write(malformed.join("meta.yaml"), "name: [\n").expect("write malformed meta");
        fs::write(malformed.join("SKILL.md"), "Malformed.").expect("write malformed doc");
        fs::write(
            valid.join("meta.yaml"),
            "name: z_valid\ndescription: valid\n",
        )
        .expect("write valid meta");
        fs::write(valid.join("SKILL.md"), "Valid skill.").expect("write valid doc");

        let mut manager = SkillManager::new();
        manager
            .load_from_paths(&paths)
            .expect("one malformed skill should be isolated");

        assert!(manager.get("z_valid").is_some());
        assert!(manager.get("a_malformed").is_none());
        let _ = fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn test_script_detection_does_not_follow_directory_symlink() {
        use std::os::unix::fs::symlink;

        let root = temp_skill_dir("blockcell-skill-script-symlink");
        let outside = temp_skill_dir("blockcell-skill-script-outside");
        fs::write(outside.join("run.sh"), "echo outside\n").expect("write outside script");
        symlink(&outside, root.join("linked_scripts")).expect("create directory symlink");

        assert!(!skill_dir_contains_local_script(&root));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn test_skill_scan_does_not_load_symlinked_directory() {
        use std::os::unix::fs::symlink;

        let base = temp_skill_dir("blockcell-skill-scan-symlink");
        let paths = Paths::with_base(base.clone());
        let outside = temp_skill_dir("blockcell-skill-scan-outside");
        fs::write(
            outside.join("meta.yaml"),
            "name: outside_skill\ndescription: outside\n",
        )
        .expect("write outside meta");
        fs::write(outside.join("SKILL.md"), "Outside skill.").expect("write outside doc");
        fs::create_dir_all(paths.skills_dir()).expect("create skills root");
        symlink(&outside, paths.skills_dir().join("linked_skill")).expect("create skill symlink");

        let mut manager = SkillManager::new();
        manager.load_from_paths(&paths).expect("scan skills");

        assert!(manager.get("outside_skill").is_none());
        let _ = fs::remove_dir_all(base);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn test_skill_cards_are_short_and_do_not_inline_full_skill_md() {
        let skill_dir = temp_skill_dir("blockcell-skill-card-short");
        fs::write(
            skill_dir.join("meta.yaml"),
            r#"
name: ppt
description: 生成 PPT 页面
"#,
        )
        .expect("write meta");
        fs::write(
            skill_dir.join("SKILL.md"),
            format!(
                "# PPT\n\n## Shared {{#shared}}\n适合把信息整理成演示页面。\n\n## Prompt {{#prompt}}\n{}\n",
                "VERY_DETAILED_INTERNAL_STEP ".repeat(80)
            ),
        )
        .expect("write skill md");

        let mut manager = SkillManager::new();
        manager.skills = HashMap::from([(
            "ppt".to_string(),
            Skill {
                name: "ppt".to_string(),
                path: skill_dir,
                meta: SkillMeta {
                    name: "ppt".to_string(),
                    description: "生成 PPT 页面".to_string(),
                    ..SkillMeta::default()
                },
                available: true,
                unavailable_reason: None,
                current_version: None,
                cached_docs: None,
                prompt_override: None,
            },
        )]);

        let cards = manager.list_enabled_skill_cards(&HashSet::new());
        assert_eq!(cards.len(), 1);
        assert!(cards[0].when_to_use.len() <= 240);
        assert!(!cards[0]
            .when_to_use
            .contains("VERY_DETAILED_INTERNAL_STEP VERY_DETAILED_INTERNAL_STEP VERY_DETAILED_INTERNAL_STEP VERY_DETAILED_INTERNAL_STEP VERY_DETAILED_INTERNAL_STEP"));
    }

    #[test]
    fn test_skill_card_detects_exec_local_from_manual_and_nested_script() {
        let skill_dir = temp_skill_dir("blockcell-skill-card-exec-local");
        fs::create_dir_all(skill_dir.join("scripts")).expect("create scripts dir");
        fs::write(
            skill_dir.join("meta.yaml"),
            r#"
name: local_demo
description: 本地脚本 demo
"#,
        )
        .expect("write meta");
        fs::write(
            skill_dir.join("SKILL.md"),
            r#"# Local Demo

## Shared {#shared}
适合执行 skill 目录内的本地脚本。

## Prompt {#prompt}
如果要运行本地脚本，使用 exec_local。
"#,
        )
        .expect("write skill md");
        fs::write(skill_dir.join("scripts/hello.sh"), "#!/bin/sh\necho ok\n")
            .expect("write script");

        let skill = Skill {
            name: "local_demo".to_string(),
            path: skill_dir,
            meta: SkillMeta {
                name: "local_demo".to_string(),
                description: "本地脚本 demo".to_string(),
                ..SkillMeta::default()
            },
            available: true,
            unavailable_reason: None,
            current_version: None,
            cached_docs: None,
            prompt_override: None,
        };

        let card = SkillManager::build_skill_card(&skill);
        assert!(card.supports_local_exec);
        assert_eq!(card.execution_layout, "PromptTool + LocalScript");
        assert!(card
            .local_exec_entrypoints
            .iter()
            .any(|entry| entry == "scripts/"));
    }

    #[test]
    fn test_skill_card_does_not_treat_install_instructions_as_local_exec() {
        let skill_dir = temp_skill_dir("blockcell-skill-card-cli-install");
        fs::write(
            skill_dir.join("meta.yaml"),
            r#"
name: tencent-news
description: 获取腾讯新闻热点、早报、晚报和 AI 精选内容。
"#,
        )
        .expect("write meta");
        fs::write(
            skill_dir.join("SKILL.md"),
            r#"# Tencent News

## Shared {#shared}
- 适合通过工具 `exec` 执行对应的 `tencent-news-cli` 命令来查询腾讯新闻热点、早报、晚报、AI 精选，以及提交使用反馈。
- 这个 skill 依赖本地 CLI 和 API Key；如果 CLI 未安装或未配置，先引导用户完成安装与配置，再继续查询。

## Prompt {#prompt}
执行 tencent-news-cli hot

## 安装与配置
```bash
curl -fsSL https://example.invalid/setup.sh | sh
```
"#,
        )
        .expect("write skill md");

        let skill = Skill {
            name: "tencent-news".to_string(),
            path: skill_dir,
            meta: SkillMeta {
                name: "tencent-news".to_string(),
                description: "获取腾讯新闻热点、早报、晚报和 AI 精选内容。".to_string(),
                ..SkillMeta::default()
            },
            available: true,
            unavailable_reason: None,
            current_version: None,
            cached_docs: None,
            prompt_override: None,
        };

        let card = SkillManager::build_skill_card(&skill);
        assert_eq!(card.execution_layout, "PromptTool");
        assert!(!card.supports_local_exec);
        assert!(card.local_exec_entrypoints.is_empty());
    }

    #[test]
    fn test_detect_skill_format_blockcell_with_meta_yaml() {
        let dir = temp_skill_dir("detect-bc-yaml");
        fs::write(dir.join("meta.yaml"), "name: test\n").unwrap();
        fs::write(dir.join("SKILL.md"), "---\nname: test\n---\nBody").unwrap();
        // meta.yaml 存在时优先判定为 BlockCell
        assert_eq!(detect_skill_format(&dir), SkillSource::BlockCell);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_skill_format_openclaw_frontmatter() {
        let dir = temp_skill_dir("detect-oc");
        fs::write(dir.join("SKILL.md"), "---\nname: test\n---\nBody").unwrap();
        assert_eq!(detect_skill_format(&dir), SkillSource::OpenClaw);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_gbrain_skill_dir_detects_manifest() {
        let root = temp_skill_dir("gbrain-root");
        fs::write(
            root.join("manifest.json"),
            r#"{"name":"gbrain-rs-skills","conformance_version":"gbrain-rs"}"#,
        )
        .unwrap();
        let dir = root.join("query");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "---\nname: query\n---\nBody").unwrap();
        assert!(is_gbrain_skill_dir(&dir));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_load_gbrain_skill_without_openclaw_flag() {
        let root = temp_skill_dir("gbrain-load-root");
        fs::write(
            root.join("manifest.json"),
            r#"{"name":"gbrain-rs-skills","conformance_version":"gbrain-rs"}"#,
        )
        .unwrap();
        let dir = root.join("query");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            r#"---
name: query
description: Query the brain
tools:
  - search
  - query
---

# Query
"#,
        )
        .unwrap();

        let manager = SkillManager::new();
        let skill = manager.load_skill(&dir).unwrap().unwrap();
        assert_eq!(skill.name, "query");
        assert_eq!(skill.meta.effective_tools(), vec!["search", "query"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_detect_skill_format_openclaw_crlf() {
        let dir = temp_skill_dir("detect-oc-crlf");
        fs::write(dir.join("SKILL.md"), "---\r\nname: test\r\n---\r\nBody").unwrap();
        assert_eq!(detect_skill_format(&dir), SkillSource::OpenClaw);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_skill_format_openclaw_with_bom() {
        let dir = temp_skill_dir("detect-oc-bom");
        let mut content = vec![0xEF, 0xBB, 0xBF]; // UTF-8 BOM
        content.extend_from_slice(b"---\nname: test\n---\nBody");
        fs::write(dir.join("SKILL.md"), &content).unwrap();
        assert_eq!(detect_skill_format(&dir), SkillSource::OpenClaw);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_skill_format_plain_skill_md() {
        let dir = temp_skill_dir("detect-plain");
        fs::write(dir.join("SKILL.md"), "# Just a heading\nNo frontmatter").unwrap();
        assert_eq!(detect_skill_format(&dir), SkillSource::BlockCell);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_skill_format_empty_dir() {
        let dir = temp_skill_dir("detect-empty");
        assert_eq!(detect_skill_format(&dir), SkillSource::BlockCell);
        let _ = fs::remove_dir_all(&dir);
    }
}
