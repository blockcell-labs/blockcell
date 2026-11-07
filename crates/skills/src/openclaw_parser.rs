//! OpenClaw SKILL.md frontmatter 解析器
//!
//! 解析 OpenClaw 格式的 SKILL.md（YAML frontmatter + Markdown 正文），
//! 将其映射为 BlockCell 的 SkillMeta 结构。

use crate::manager::{SkillInstallSpec, SkillMeta, SkillRequires, SkillSource};
use blockcell_core::Result;
use serde::Deserialize;
use std::path::Path;

// ---------------------------------------------------------------------------
// 内部反序列化结构体（仅用于 YAML 解析，不对外暴露）
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct OpenClawFrontmatter {
    name: Option<String>,
    #[allow(dead_code)]
    version: Option<String>,
    description: Option<String>,
    tools: Option<Vec<String>>,
    capabilities: Option<Vec<String>>,
    #[serde(rename = "allowed-tools")]
    allowed_tools: Option<AllowedTools>,
    triggers: Option<Vec<String>>,
    always: Option<bool>,
    mutating: Option<bool>,
    #[allow(dead_code)]
    homepage: Option<String>,
    #[serde(rename = "user-invocable")]
    user_invocable: Option<bool>,
    #[serde(rename = "disable-model-invocation")]
    disable_model_invocation: Option<bool>,
    metadata: Option<OpenClawMetadataWrapper>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AllowedTools {
    Text(String),
    List(Vec<String>),
}

impl AllowedTools {
    fn into_tools(self) -> Vec<String> {
        match self {
            Self::Text(text) => text.split_whitespace().map(str::to_string).collect(),
            Self::List(tools) => tools,
        }
    }
}

pub(crate) fn is_standard_skill(content: &str) -> bool {
    let Ok((yaml, _)) = extract_frontmatter(content) else {
        return false;
    };
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&yaml) else {
        return false;
    };
    ["name", "description"].iter().all(|key| {
        value
            .get(*key)
            .and_then(|v| v.as_str())
            .is_some_and(|v| !v.trim().is_empty())
    }) && !["triggers", "mutating", "always"]
        .iter()
        .any(|key| value.get(*key).is_some())
        && value
            .get("metadata")
            .and_then(|v| v.get("openclaw"))
            .is_none()
}

pub(crate) fn parse_standard_skill(skill_dir: &Path, content: &str) -> Result<(SkillMeta, String)> {
    let (mut meta, body) = parse_skill(skill_dir, content, true)?;
    #[derive(Deserialize)]
    struct Sidecar {
        policy: Option<Policy>,
    }
    #[derive(Deserialize)]
    struct Policy {
        allow_implicit_invocation: Option<bool>,
    }
    let path = skill_dir.join("agents/openai.yaml");
    if path.exists() {
        let policy = (|| -> Result<Sidecar> {
            if std::fs::metadata(&path)?.len() > 256 * 1024 {
                return Err(blockcell_core::Error::Skill(
                    "Skill sidecar exceeds 256KB limit".into(),
                ));
            }
            Ok(serde_yaml::from_str(&std::fs::read_to_string(&path)?)?)
        })();
        match policy {
            Ok(sidecar) => {
                if sidecar.policy.and_then(|p| p.allow_implicit_invocation) == Some(false) {
                    meta.disable_model_invocation = true;
                }
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Ignoring invalid skill sidecar")
            }
        }
    }
    Ok((meta, body))
}

#[derive(Deserialize)]
struct OpenClawMetadataWrapper {
    openclaw: Option<OpenClawSkillMetadata>,
}

#[derive(Deserialize)]
struct OpenClawSkillMetadata {
    always: Option<bool>,
    emoji: Option<String>,
    os: Option<Vec<String>>,
    requires: Option<OpenClawRequires>,
    install: Option<Vec<OpenClawInstallSpecRaw>>,
}

#[derive(Deserialize)]
struct OpenClawRequires {
    bins: Option<Vec<String>>,
    #[serde(rename = "anyBins")]
    any_bins: Option<Vec<String>>,
    env: Option<Vec<String>>,
    config: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct OpenClawInstallSpecRaw {
    id: Option<String>,
    kind: String,
    label: Option<String>,
    bins: Option<Vec<String>>,
    os: Option<Vec<String>>,
    formula: Option<String>,
    package: Option<String>,
    module: Option<String>,
    url: Option<String>,
}

// ---------------------------------------------------------------------------
// 公开 API
// ---------------------------------------------------------------------------

/// 从 skill 目录读取描述。优先级：
/// 1. meta.yaml / meta.json（BlockCell 原生格式，短描述）
/// 2. SKILL.md frontmatter（OpenClaw 格式）
/// 3. SKILL.md 正文首行（fallback）
///
/// 返回空字符串表示无描述。
pub fn read_skill_description(skill_dir: &Path) -> String {
    // 1. meta.yaml（BlockCell 原生格式优先）
    let meta_yaml = skill_dir.join("meta.yaml");
    if meta_yaml.exists() {
        if let Ok(content) = std::fs::read_to_string(&meta_yaml) {
            if let Ok(val) = serde_yaml::from_str::<serde_json::Value>(&content) {
                if let Some(desc) = val.get("description").and_then(|v| v.as_str()) {
                    if !desc.is_empty() {
                        return desc.to_string();
                    }
                }
            }
        }
    }

    // 2. meta.json
    let meta_json = skill_dir.join("meta.json");
    if meta_json.exists() {
        if let Ok(content) = std::fs::read_to_string(&meta_json) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(desc) = val.get("description").and_then(|v| v.as_str()) {
                    if !desc.is_empty() {
                        return desc.to_string();
                    }
                }
            }
        }
    }

    // 3. SKILL.md（OpenClaw frontmatter 或纯 Markdown fallback）
    let skill_md = skill_dir.join("SKILL.md");
    if skill_md.exists() {
        if let Ok(content) = std::fs::read_to_string(&skill_md) {
            let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
            let content = content.replace("\r\n", "\n");

            // OpenClaw 格式：用 serde_yaml 解析 frontmatter
            if let Some(rest) = content.strip_prefix("---") {
                if let Some(end) = rest.find("\n---") {
                    let yaml = rest[..end].trim();
                    if let Ok(val) = serde_yaml::from_str::<serde_json::Value>(yaml) {
                        if let Some(desc) = val.get("description").and_then(|v| v.as_str()) {
                            if !desc.is_empty() {
                                return desc.to_string();
                            }
                        }
                    }
                }
            }

            // 非 frontmatter：取第一个非标题非空行
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let char_count = trimmed.chars().count();
                if char_count > 40 {
                    return trimmed.chars().take(40).collect();
                }
                return trimmed.to_string();
            }
        }
    }

    String::new()
}

/// 解析 OpenClaw SKILL.md 的 YAML frontmatter，返回 (SkillMeta, prompt 正文)。
///
/// `skill_dir` 用于：
/// - 当 frontmatter 缺少 `name` 时回退到目录名
/// - 替换正文中的 `{baseDir}` 占位符
/// - 推断工具列表（扫描脚本文件）
pub fn parse_openclaw_skill(skill_dir: &Path, content: &str) -> Result<(SkillMeta, String)> {
    parse_skill(skill_dir, content, false)
}

fn parse_skill(skill_dir: &Path, content: &str, standard: bool) -> Result<(SkillMeta, String)> {
    // 1. 提取 frontmatter
    let (yaml_str, body) = extract_frontmatter(content)?;

    // 2. 解析 YAML
    let fm: OpenClawFrontmatter = serde_yaml::from_str(&yaml_str).map_err(|e| {
        blockcell_core::Error::Skill(format!("OpenClaw frontmatter YAML parse error: {}", e))
    })?;

    // 3. 映射到 SkillMeta
    let oc = fm.metadata.and_then(|m| m.openclaw);
    let requires = oc.as_ref().and_then(|o| o.requires.as_ref());

    let dir_name = skill_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    // 4. 处理 body 中的 {baseDir} 占位符
    let base_dir = skill_dir.to_string_lossy();
    let body = body.replace("{baseDir}", &base_dir);

    // 5. 推断工具列表
    let tools = if standard {
        fm.allowed_tools
            .map(AllowedTools::into_tools)
            .or_else(|| fm.tools.clone())
            .or_else(|| fm.capabilities.clone())
            .unwrap_or_else(|| {
                [
                    "exec",
                    "shell_run",
                    "shell_poll",
                    "shell_close",
                    "read_file",
                    "list_dir",
                ]
                .into_iter()
                .map(str::to_string)
                .collect()
            })
    } else {
        fm.tools
            .clone()
            .or_else(|| fm.capabilities.clone())
            .filter(|tools| !tools.is_empty())
            .unwrap_or_else(|| infer_tools_for_openclaw(skill_dir, &body))
    };

    let output_format = build_gbrain_output_hint(
        fm.mutating.unwrap_or(false),
        fm.triggers.as_deref().unwrap_or(&[]),
    );

    let meta = SkillMeta {
        name: fm.name.unwrap_or(dir_name),
        description: fm.description.unwrap_or_default(),
        source: if standard {
            SkillSource::Standard
        } else {
            SkillSource::OpenClaw
        },
        requires: SkillRequires {
            bins: requires.and_then(|r| r.bins.clone()).unwrap_or_default(),
            env: requires.and_then(|r| r.env.clone()).unwrap_or_default(),
            any_bins: requires
                .and_then(|r| r.any_bins.clone())
                .unwrap_or_default(),
            config: requires.and_then(|r| r.config.clone()).unwrap_or_default(),
        },
        always: fm
            .always
            .or_else(|| oc.as_ref().and_then(|o| o.always))
            .unwrap_or(false),
        emoji: oc.as_ref().and_then(|o| o.emoji.clone()),
        os: oc.as_ref().and_then(|o| o.os.clone()),
        user_invocable: fm.user_invocable.unwrap_or(true),
        disable_model_invocation: fm.disable_model_invocation.unwrap_or(false),
        tools,
        output_format,
        install: oc
            .as_ref()
            .and_then(|o| o.install.as_ref())
            .map(|specs| specs.iter().map(map_install_spec).collect())
            .unwrap_or_default(),
        // 其他字段使用默认值
        ..Default::default()
    };

    Ok((meta, body))
}

// ---------------------------------------------------------------------------
// 内部辅助函数
// ---------------------------------------------------------------------------

/// 提取 YAML frontmatter（两个 `---` 之间的内容）和正文。
/// 支持 \r\n 换行和 UTF-8 BOM。
fn extract_frontmatter(content: &str) -> Result<(String, String)> {
    // 去除 UTF-8 BOM
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    // 统一换行符为 \n
    let content = content.replace("\r\n", "\n");

    if !content.starts_with("---") {
        return Err(blockcell_core::Error::Skill(
            "OpenClaw SKILL.md missing frontmatter (must start with ---)".into(),
        ));
    }
    let rest = &content[3..];
    let end = rest.find("\n---").ok_or_else(|| {
        blockcell_core::Error::Skill("OpenClaw SKILL.md has unclosed frontmatter".into())
    })?;
    let yaml = rest[..end].trim().to_string();
    let body = rest[end + 4..].trim().to_string();
    Ok((yaml, body))
}

/// 根据技能目录结构和 SKILL.md 内容推断工具列表。
///
/// OpenClaw 技能默认包含 exec_local，因为其核心执行模型
/// 就是通过 exec 工具调用外部 CLI 命令。
fn infer_tools_for_openclaw(skill_dir: &Path, skill_body: &str) -> Vec<String> {
    let mut tools = vec![];

    // OpenClaw 技能默认添加 exec_local（核心执行通道）
    tools.push("exec_local".to_string());

    // 如果有脚本文件，添加 exec_skill_script
    let has_scripts = skill_dir.join("scripts").is_dir()
        || skill_dir.join("SKILL.rhai").exists()
        || skill_dir.join("SKILL.py").exists();
    if has_scripts {
        tools.push("exec_skill_script".to_string());
    }

    // 按需推断：扫描 SKILL.md 正文关键词（仅匹配工具全名，避免裸词误匹配）
    let body_lower = skill_body.to_lowercase();
    if body_lower.contains("web_fetch") {
        tools.push("web_fetch".to_string());
    }
    if body_lower.contains("web_search") {
        tools.push("web_search".to_string());
    }
    if body_lower.contains("read_file") || body_lower.contains("read file") {
        tools.push("read_file".to_string());
    }
    if body_lower.contains("write_file") || body_lower.contains("write file") {
        tools.push("write_file".to_string());
    }

    tools
}

fn build_gbrain_output_hint(mutating: bool, triggers: &[String]) -> Option<String> {
    if !mutating && triggers.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    if mutating {
        parts.push("This skill may mutate the brain; report the user-visible outcome clearly.");
    }
    if !triggers.is_empty() {
        parts.push("Use the declared triggers as routing hints for when this skill applies.");
    }

    Some(parts.join(" "))
}

/// 将内部反序列化结构体映射到输出侧的 SkillInstallSpec。
fn map_install_spec(spec: &OpenClawInstallSpecRaw) -> SkillInstallSpec {
    SkillInstallSpec {
        id: spec.id.clone(),
        kind: spec.kind.clone(),
        label: spec.label.clone(),
        bins: spec.bins.clone().unwrap_or_default(),
        os: spec.os.clone(),
        formula: spec.formula.clone(),
        package: spec.package.clone(),
        module: spec.module.clone(),
        url: spec.url.clone(),
    }
}

/// 当可用性检查失败且有 install 规格时，生成安装提示。
pub fn generate_install_hint(meta: &SkillMeta, error: &str) -> String {
    if meta.install.is_empty() {
        return format!("Skill '{}' is unavailable: {}", meta.name, error);
    }

    let mut hint = format!(
        "Skill '{}' is unavailable: {}\n\nInstall options:\n",
        meta.name, error
    );

    for spec in &meta.install {
        match spec.kind.as_str() {
            "brew" => {
                if let Some(ref formula) = spec.formula {
                    hint.push_str(&format!("  brew install {}\n", formula));
                }
            }
            "node" => {
                if let Some(ref package) = spec.package {
                    hint.push_str(&format!("  npm install -g {}\n", package));
                }
            }
            "go" => {
                if let Some(ref module) = spec.module {
                    hint.push_str(&format!("  go install {}\n", module));
                }
            }
            "uv" => {
                if let Some(ref package) = spec.package {
                    hint.push_str(&format!("  uv tool install {}\n", package));
                }
            }
            "download" => {
                if let Some(ref url) = spec.url {
                    let label = spec.label.as_deref().unwrap_or("Download");
                    hint.push_str(&format!("  {}: {}\n", label, url));
                }
            }
            _ => {}
        }
    }

    hint
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_dir() -> PathBuf {
        PathBuf::from("/tmp/test_skill")
    }

    #[test]
    fn test_extract_frontmatter_basic() {
        let content = "---\nname: test\n---\n\n# Hello";
        let (yaml, body) = extract_frontmatter(content).unwrap();
        assert_eq!(yaml, "name: test");
        assert_eq!(body, "# Hello");
    }

    #[test]
    fn test_extract_frontmatter_missing() {
        let content = "# No frontmatter here";
        assert!(extract_frontmatter(content).is_err());
    }

    #[test]
    fn test_extract_frontmatter_unclosed() {
        let content = "---\nname: test\n# No closing";
        assert!(extract_frontmatter(content).is_err());
    }

    #[test]
    fn test_extract_frontmatter_empty() {
        let content = "---\n---\n\nBody text";
        let (yaml, body) = extract_frontmatter(content).unwrap();
        assert_eq!(yaml, "");
        assert_eq!(body, "Body text");
    }

    #[test]
    fn test_parse_minimal_frontmatter() {
        let content = "---\nname: minimal\ndescription: A minimal skill\n---\n\n# Minimal Skill\n\nDo something.";
        let (meta, body) = parse_openclaw_skill(&test_dir(), content).unwrap();
        assert_eq!(meta.name, "minimal");
        assert_eq!(meta.description, "A minimal skill");
        assert_eq!(meta.source, SkillSource::OpenClaw);
        assert!(meta.user_invocable);
        assert!(!meta.disable_model_invocation);
        assert!(!meta.always);
        assert!(meta.emoji.is_none());
        assert!(meta.os.is_none());
        assert!(body.contains("# Minimal Skill"));
    }

    #[test]
    fn test_parse_full_frontmatter() {
        let content = r#"---
name: github
description: "GitHub operations via gh CLI"
user-invocable: true
disable-model-invocation: false
metadata:
  openclaw:
    emoji: "🐙"
    always: false
    os:
      - darwin
      - linux
    requires:
      bins:
        - gh
      anyBins:
        - git
        - hub
      env:
        - GITHUB_TOKEN
      config:
        - ~/.config/gh/hosts.yml
    install:
      - id: brew
        kind: brew
        formula: gh
        bins:
          - gh
---

# GitHub Skill

Use `gh` CLI to manage repos.
"#;
        let (meta, body) = parse_openclaw_skill(&test_dir(), content).unwrap();
        assert_eq!(meta.name, "github");
        assert_eq!(meta.emoji, Some("🐙".to_string()));
        assert_eq!(
            meta.os,
            Some(vec!["darwin".to_string(), "linux".to_string()])
        );
        assert_eq!(meta.requires.bins, vec!["gh"]);
        assert_eq!(meta.requires.any_bins, vec!["git", "hub"]);
        assert_eq!(meta.requires.env, vec!["GITHUB_TOKEN"]);
        assert_eq!(meta.requires.config, vec!["~/.config/gh/hosts.yml"]);
        assert_eq!(meta.install.len(), 1);
        assert_eq!(meta.install[0].kind, "brew");
        assert_eq!(meta.install[0].formula, Some("gh".to_string()));
        assert!(body.contains("# GitHub Skill"));
    }

    #[test]
    fn test_parse_name_fallback_to_dir() {
        let content = "---\ndescription: No name field\n---\n\nBody";
        let dir = PathBuf::from("/skills/my_skill");
        let (meta, _) = parse_openclaw_skill(&dir, content).unwrap();
        assert_eq!(meta.name, "my_skill");
    }

    #[test]
    fn test_parse_gbrain_frontmatter_tools() {
        let content = r#"---
name: query
description: |
  Answer questions using the brain's knowledge with search and citations.
triggers:
  - "what do we know about"
tools:
  - search
  - query
  - get_page
  - get_backlinks
mutating: false
---

# Query Skill
"#;
        let (meta, body) = parse_openclaw_skill(&test_dir(), content).unwrap();
        assert_eq!(meta.name, "query");
        assert!(meta.description.contains("brain's knowledge"));
        assert_eq!(
            meta.tools,
            vec!["search", "query", "get_page", "get_backlinks"]
        );
        assert!(!meta.tools.contains(&"exec_local".to_string()));
        assert!(meta.output_format.is_some());
        assert!(body.contains("# Query Skill"));
    }

    #[test]
    fn test_basedir_replacement() {
        let content =
            "---\nname: test\ndescription: test\n---\n\nRun: python3 {baseDir}/scripts/run.py";
        let dir = PathBuf::from("/home/user/skills/test");
        let (_, body) = parse_openclaw_skill(&dir, content).unwrap();
        assert!(body.contains("/home/user/skills/test/scripts/run.py"));
        assert!(!body.contains("{baseDir}"));
    }

    #[test]
    fn test_tool_inference_with_scripts_dir() {
        // 无法在测试中创建真实目录，所以测试不存在目录的情况
        let dir = test_dir();
        let body = "Use web_fetch to get data. Also read_file for local data.";
        let tools = infer_tools_for_openclaw(&dir, body);
        assert!(tools.contains(&"exec_local".to_string()));
        assert!(tools.contains(&"web_fetch".to_string()));
        assert!(tools.contains(&"read_file".to_string()));
        // scripts 目录不存在，不应包含 exec_skill_script
        assert!(!tools.contains(&"exec_skill_script".to_string()));
    }

    #[test]
    fn test_tool_inference_always_has_exec_local() {
        let dir = test_dir();
        let body = "Simple skill with no special tools.";
        let tools = infer_tools_for_openclaw(&dir, body);
        assert!(tools.contains(&"exec_local".to_string()));
    }

    #[test]
    fn test_parse_malformed_yaml() {
        let content = "---\nname: [invalid yaml\n---\n\nBody";
        let result = parse_openclaw_skill(&test_dir(), content);
        assert!(result.is_err());
    }

    #[test]
    fn test_generate_install_hint_with_brew() {
        let meta = SkillMeta {
            name: "github".to_string(),
            install: vec![SkillInstallSpec {
                kind: "brew".to_string(),
                formula: Some("gh".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let hint = generate_install_hint(&meta, "Missing binary: gh");
        assert!(hint.contains("brew install gh"));
    }

    #[test]
    fn test_generate_install_hint_empty() {
        let meta = SkillMeta {
            name: "test".to_string(),
            ..Default::default()
        };
        let hint = generate_install_hint(&meta, "Missing binary: foo");
        assert!(hint.contains("unavailable"));
        assert!(!hint.contains("Install options"));
    }

    #[test]
    fn test_invocation_policy_fields() {
        let content = "---\nname: restricted\ndescription: test\nuser-invocable: false\ndisable-model-invocation: true\n---\n\nBody";
        let (meta, _) = parse_openclaw_skill(&test_dir(), content).unwrap();
        assert!(!meta.user_invocable);
        assert!(meta.disable_model_invocation);
    }

    #[test]
    fn test_extract_frontmatter_crlf() {
        let content = "---\r\nname: test\r\n---\r\n\r\n# Hello";
        let (yaml, body) = extract_frontmatter(content).unwrap();
        assert_eq!(yaml, "name: test");
        assert_eq!(body, "# Hello");
    }

    #[test]
    fn test_extract_frontmatter_utf8_bom() {
        let content = "\u{feff}---\nname: test\n---\n\n# Hello";
        let (yaml, body) = extract_frontmatter(content).unwrap();
        assert_eq!(yaml, "name: test");
        assert_eq!(body, "# Hello");
    }

    #[test]
    fn test_tool_inference_no_bare_word_match() {
        let dir = test_dir();
        // "fetch" 和 "search" 裸词不应触发工具推断
        let body = "Fetch data from the API. Search for results.";
        let tools = infer_tools_for_openclaw(&dir, body);
        assert!(tools.contains(&"exec_local".to_string()));
        assert!(!tools.contains(&"web_fetch".to_string()));
        assert!(!tools.contains(&"web_search".to_string()));
    }

    #[test]
    fn test_generate_install_hint_download() {
        let meta = SkillMeta {
            name: "tool".to_string(),
            install: vec![SkillInstallSpec {
                kind: "download".to_string(),
                label: Some("Download binary".to_string()),
                url: Some("https://example.com/tool".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let hint = generate_install_hint(&meta, "Missing binary");
        assert!(hint.contains("Download binary: https://example.com/tool"));
    }
}
