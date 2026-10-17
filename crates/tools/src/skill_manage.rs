//! skill_manage 工具 — Skill CRUD 操作
//!
//! 提供 create / patch / view / delete / edit / write_file / remove_file 七个 action, 让 Agent 能:
//! - 从成功任务中提炼新 Skill (Layer 1: Review)
//! - 修补已有 Skill 的遗漏步骤和 Pitfalls (Layer 2: Patch)
//! - 完整替换 SKILL.md 内容 (Edit)
//! - 添加/删除 supporting 文件 (write_file / remove_file)
//! - 按需加载 Skill 完整内容 (渐进式加载)
//! - 删除过时 Skill

use async_trait::async_trait;
use blockcell_core::Result;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::LazyLock;

use crate::fuzzy_match::fuzzy_find_and_replace;
use crate::security_scan::{
    format_report, scan_skill_content, scan_skill_content_with_trust, scan_skill_dir_with_trust,
};
use crate::{Tool, ToolContext, ToolSchema};

/// Skill 名称正则: 仅允许小写字母、数字、点、下划线、连字符, 必须以小写字母或数字开头
const VALID_SKILL_NAME_RE: &str = "^[a-z0-9][a-z0-9._-]*$";

/// Skill 内容最大字符数 (参考 Hermes MAX_SKILL_CONTENT_CHARS)
const MAX_SKILL_CONTENT_CHARS: usize = 100_000;

/// Skill 描述最大长度 (参考 Hermes MAX_DESCRIPTION_LENGTH)
const MAX_DESCRIPTION_LENGTH: usize = 1024;

/// 编译后的 Skill 名称正则 (懒加载, 避免每次调用重新编译)
static VALID_SKILL_NAME_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(VALID_SKILL_NAME_RE).unwrap());

/// skill_manage 工具
pub struct SkillManageTool;

#[async_trait]
impl Tool for SkillManageTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "skill_manage".to_string(),
            description: "Manage skills (create, patch, view, delete). Skills are your procedural memory — reusable approaches for recurring task types.\n\nCreate when: complex task succeeded (5+ tool calls), errors overcome, user-corrected approach worked, or non-trivial workflow discovered.\nPatch when: instructions stale/wrong, missing steps or pitfalls found during use. If you used a skill and hit issues not covered by it, patch it immediately — don't wait to be asked.\nView when: you need to load a skill's full content before following its steps.\nDelete when: skill is obsolete or user requests removal.\n\nAfter difficult/iterative tasks, offer to save as a skill. Skip for simple one-offs.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "patch", "view", "delete", "edit", "write_file", "remove_file"],
                        "description": "Action to perform"
                    },
                    "name": {
                        "type": "string",
                        "description": "Skill name (e.g. 'flask-k8s-deploy')"
                    },
                    "category": {
                        "type": "string",
                        "description": "Category (e.g. 'devops', 'software-development'). Optional for create — defaults to no category."
                    },
                    "content": {
                        "type": "string",
                        "description": "Full SKILL.md content for create/edit action"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "Text to find for patch (fuzzy match)"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "Replacement text for patch"
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace all occurrences (default: false)"
                    },
                    "file_path": {
                        "type": "string",
                        "description": "For patch: relative file path within skill dir (default: SKILL.md). For write_file/remove_file: the file path under references/templates/scripts/assets."
                    },
                    "file_content": {
                        "type": "string",
                        "description": "For write_file: content to write to the file"
                    }
                },
                "required": ["action", "name"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");

        // name 参数对所有 action 都是必需的 (包括 view)
        if !action.is_empty()
            && params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .is_empty()
        {
            return Err(blockcell_core::Error::Validation(
                "skill_manage requires 'name' parameter".to_string(),
            ));
        }

        match action {
            "create" => {
                if params.get("content").and_then(|v| v.as_str()).is_none() {
                    return Err(blockcell_core::Error::Validation(
                        "skill_manage create requires 'content' parameter".to_string(),
                    ));
                }
                // category 可选 — 不提供时 Skill 直接放在 skills/ 下
            }
            "patch" => {
                if params.get("old_string").and_then(|v| v.as_str()).is_none() {
                    return Err(blockcell_core::Error::Validation(
                        "skill_manage patch requires 'old_string' parameter".to_string(),
                    ));
                }
                // new_string 可为空 (用于删除匹配文本), 但参数本身必须存在
                if params.get("new_string").is_none() {
                    return Err(blockcell_core::Error::Validation(
                        "skill_manage patch requires 'new_string' parameter (can be empty string to delete matched text)".to_string(),
                    ));
                }
            }
            "edit" => {
                if params.get("content").and_then(|v| v.as_str()).is_none() {
                    return Err(blockcell_core::Error::Validation(
                        "skill_manage edit requires 'content' parameter".to_string(),
                    ));
                }
            }
            "write_file" => {
                if params.get("file_path").and_then(|v| v.as_str()).is_none() {
                    return Err(blockcell_core::Error::Validation(
                        "skill_manage write_file requires 'file_path' parameter".to_string(),
                    ));
                }
                if params
                    .get("file_content")
                    .and_then(|v| v.as_str())
                    .is_none()
                {
                    return Err(blockcell_core::Error::Validation(
                        "skill_manage write_file requires 'file_content' parameter".to_string(),
                    ));
                }
            }
            "remove_file" => {
                if params.get("file_path").and_then(|v| v.as_str()).is_none() {
                    return Err(blockcell_core::Error::Validation(
                        "skill_manage remove_file requires 'file_path' parameter".to_string(),
                    ));
                }
            }
            "view" | "delete" => {}
            _ => {
                return Err(blockcell_core::Error::Validation(format!(
                    "skill_manage: invalid action '{}'. Must be create/patch/view/delete/edit/write_file/remove_file",
                    action
                )));
            }
        }

        Ok(())
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some(
            "- **skill_manage**: Use `action=\"create\"` after complex tasks (5+ tool calls) to save reusable workflows. Use `action=\"patch\"` when a skill has missing steps or pitfalls. Use `action=\"view\"` to load a skill before following it. Do NOT save simple one-off tasks.\n".to_string()
        )
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");

        let skills_dir = ctx.workspace.join("skills");
        // 构建外部搜索目录 (builtin_skills_dir 等)
        let external_dirs: Vec<PathBuf> = ctx
            .builtin_skills_dir
            .iter()
            .filter_map(|d| {
                let dir = d.clone();
                if dir != skills_dir {
                    Some(dir)
                } else {
                    None
                }
            })
            .collect();

        // 获取互斥锁守卫 (防止 TOCTOU 竞态: can_modify + acquire 分离)
        // 守卫在整个写操作期间持有, Drop 时自动释放
        let _guard = match action {
            "create" | "patch" | "edit" | "write_file" | "remove_file" | "delete" => {
                if let Some(ref mutex) = ctx.skill_mutex {
                    mutex.try_acquire(name).ok_or_else(|| {
                        blockcell_core::Error::Skill(format!(
                            "Skill '{}' is currently being executed and cannot be modified. Try again later.",
                            name
                        ))
                    })?
                } else {
                    // 无互斥锁时使用空守卫 (Arc::new(()) 不持有任何锁)
                    Arc::new(())
                }
            }
            _ => Arc::new(()), // view 等只读操作不需要守卫
        };

        match action {
            "create" => {
                // category 可选 — 不提供时 Skill 直接放在 skills/ 下
                let category = params.get("category").and_then(|v| v.as_str());
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                create_skill(name, category, content, &skills_dir).await
            }
            "patch" => {
                let old_string = params
                    .get("old_string")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let new_string = params
                    .get("new_string")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let replace_all = params
                    .get("replace_all")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let file_path = params.get("file_path").and_then(|v| v.as_str());
                patch_skill(
                    name,
                    old_string,
                    new_string,
                    replace_all,
                    file_path,
                    &skills_dir,
                    &external_dirs,
                )
                .await
            }
            "edit" => {
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                edit_skill(name, content, &skills_dir, &external_dirs).await
            }
            "write_file" => {
                let file_path = params
                    .get("file_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let file_content = params
                    .get("file_content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                write_file_skill(name, file_path, file_content, &skills_dir, &external_dirs).await
            }
            "remove_file" => {
                let file_path = params
                    .get("file_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                remove_file_skill(name, file_path, &skills_dir, &external_dirs).await
            }
            "view" => view_skill(name, &skills_dir, &external_dirs).await,
            "delete" => delete_skill(name, &skills_dir, &external_dirs).await,
            _ => Err(blockcell_core::Error::Validation(format!(
                "Invalid action: {}",
                action
            ))),
        }
    }
}

/// 创建新 Skill
async fn create_skill(
    name: &str,
    category: Option<&str>,
    content: &str,
    skills_dir: &Path,
) -> Result<Value> {
    // 0. 验证 name 不含路径遍历, 额外校验格式
    validate_path_component(name, true)?;

    // 1. 构建目录: category 可选
    let skill_dir = if let Some(cat) = category {
        validate_path_component(cat, false)?;
        skills_dir.join(cat).join(name)
    } else {
        skills_dir.join(name)
    };
    tokio::fs::create_dir_all(skills_dir)
        .await
        .map_err(blockcell_core::Error::Io)?;
    ensure_confined_target(skills_dir, &skill_dir)?;

    // 检查是否已存在
    if skill_dir.exists() {
        return Err(blockcell_core::Error::Skill(format!(
            "Skill '{}' already exists. Use patch to modify it.",
            name
        )));
    }

    tokio::fs::create_dir_all(&skill_dir)
        .await
        .map_err(blockcell_core::Error::Io)?;

    // 2. 内容大小限制 (使用字符数, 与 security_scan 一致, 支持多字节内容)
    if content.chars().count() > MAX_SKILL_CONTENT_CHARS {
        let _ = tokio::fs::remove_dir_all(&skill_dir).await;
        return Err(blockcell_core::Error::Validation(format!(
            "Skill content exceeds maximum size of {} characters (got {})",
            MAX_SKILL_CONTENT_CHARS,
            content.chars().count()
        )));
    }

    // 3. 提取并验证 frontmatter
    let meta = extract_frontmatter(content);
    if let Err(e) = validate_frontmatter(&meta) {
        // frontmatter 验证失败: 清理已创建的目录 (防止留下空目录)
        let _ = tokio::fs::remove_dir_all(&skill_dir).await;
        return Err(e);
    }

    // 4. 安全扫描 (在写入前执行, 避免恶意内容短暂存在于磁盘)
    let scan_result = scan_skill_content(content);
    if !scan_result.passed {
        // 不通过就回滚: 删除整个目录
        let _ = tokio::fs::remove_dir_all(&skill_dir).await;
        return Err(blockcell_core::Error::Skill(format!(
            "Security scan failed. Skill creation rolled back.\n{}",
            format_report(&scan_result)
        )));
    }

    // 5. 原子写入 SKILL.md (temp file + rename, 防止进程崩溃留下半写文件)
    let skill_md_path = skill_dir.join("SKILL.md");
    atomic_write_text(&skill_md_path, content).await?;

    // 6. 原子写入 meta.json
    let meta_json = serde_json::to_string_pretty(&meta)
        .map_err(|e| blockcell_core::Error::Skill(format!("Failed to serialize meta: {}", e)))?;
    atomic_write_text(&skill_dir.join("meta.json"), &meta_json).await?;

    tracing::info!(
        skill_name = name,
        category = category.unwrap_or("(none)"),
        skill_dir = %skill_dir.display(),
        "[skill_manage] Skill created"
    );

    Ok(json!({
        "success": true,
        "skill_dir": skill_dir.to_string_lossy(),
        "message": if let Some(cat) = category {
            format!("Skill '{}' created in category '{}'", name, cat)
        } else {
            format!("Skill '{}' created", name)
        },
        "hint": "Use action='write_file' to add reference files, templates, or scripts to this skill.",
        "warnings": scan_result.issues.iter()
            .filter(|i| i.level == crate::security_scan::IssueLevel::Warning)
            .map(|i| &i.message)
            .collect::<Vec<_>>()
    }))
}

/// 修补已有 Skill (模糊匹配)
async fn patch_skill(
    name: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    file_path: Option<&str>,
    skills_dir: &Path,
    external_dirs: &[PathBuf],
) -> Result<Value> {
    // 1. 查找 Skill 目录
    let skill_dir = find_skill_dir(name, skills_dir, external_dirs)?;

    // 确定要 patch 的文件: 默认 SKILL.md, 可通过 file_path 指定其他文件
    let target_file = if let Some(fp) = file_path {
        validate_skill_file_path(fp)?; // 防止路径遍历
                                       // 非 SKILL.md 的 file_path 必须在允许的子目录下 (与 write_file 一致)
        if fp != "SKILL.md" {
            let allowed_prefixes = ["references/", "templates/", "scripts/", "assets/"];
            if !allowed_prefixes.iter().any(|prefix| fp.starts_with(prefix)) {
                return Err(blockcell_core::Error::Validation(format!(
                    "file_path must be SKILL.md or under one of: {}. Got: '{}'",
                    allowed_prefixes.join(", "),
                    fp
                )));
            }
        }
        skill_dir.join(fp)
    } else {
        skill_dir.join("SKILL.md")
    };
    ensure_confined_target(skills_dir, &target_file)?;

    if !target_file.exists() {
        return Err(blockcell_core::Error::Skill(format!(
            "File '{}' not found in skill '{}'",
            target_file.display(),
            name
        )));
    }

    // 2. 读取当前内容 (与 create/edit 同口径的字符数上限, 防止超大文件把
    //    fuzzy 匹配的 O(N×M) 代价放大到挂死 executor)
    let content = tokio::fs::read_to_string(&target_file)
        .await
        .map_err(blockcell_core::Error::Io)?;
    if content.chars().count() > MAX_SKILL_CONTENT_CHARS {
        return Err(blockcell_core::Error::Validation(format!(
            "Skill file exceeds maximum size of {} characters (got {})",
            MAX_SKILL_CONTENT_CHARS,
            content.chars().count()
        )));
    }

    // 3. 模糊匹配替换 —— O(N×M) 的 CPU 密集操作，移出 async executor
    let (content_c, old_c, new_c) = (
        content.clone(),
        old_string.to_string(),
        new_string.to_string(),
    );
    let (new_content, match_count, strategy) = tokio::task::spawn_blocking(move || {
        fuzzy_find_and_replace(&content_c, &old_c, &new_c, replace_all)
    })
    .await
    .map_err(|e| blockcell_core::Error::Tool(format!("Patch task failed: {}", e)))?
    .map_err(|e| blockcell_core::Error::Skill(format!("Fuzzy match failed: {}", e)))?;

    // 4. 备份原内容 (用于回滚)
    let original_content = content.clone();

    // 5. 原子写入
    atomic_write_text(&target_file, &new_content).await?;

    // 6. 安全扫描 (不通过则回滚)
    let scan_result = scan_skill_content(&new_content);
    if !scan_result.passed {
        // 回滚
        let _ = atomic_write_text(&target_file, &original_content).await;
        return Err(blockcell_core::Error::Skill(format!(
            "Security scan failed. Changes rolled back.\n{}",
            format_report(&scan_result)
        )));
    }

    // 7. 更新 meta.json (仅当 patch 的是 SKILL.md)
    if file_path.is_none() || file_path == Some("SKILL.md") {
        let meta = extract_frontmatter(&new_content);
        if let Ok(meta_json) = serde_json::to_string_pretty(&meta) {
            let _ = atomic_write_text(&skill_dir.join("meta.json"), &meta_json).await;
        }
    }

    tracing::info!(
        skill_name = name,
        file = %target_file.display(),
        match_count,
        strategy,
        "[skill_manage] Skill patched"
    );

    Ok(json!({
        "success": true,
        "match_count": match_count,
        "strategy": strategy,
        "message": format!("Patched {} occurrence(s) in '{}' using {} strategy", match_count, target_file.display(), strategy),
        "warnings": scan_result.issues.iter()
            .filter(|i| i.level == crate::security_scan::IssueLevel::Warning)
            .map(|i| &i.message)
            .collect::<Vec<_>>()
    }))
}

/// 查看 Skill 完整内容 (按需加载)
async fn view_skill(name: &str, skills_dir: &Path, external_dirs: &[PathBuf]) -> Result<Value> {
    let skill_dir = find_skill_dir(name, skills_dir, external_dirs)?;
    let skill_md = skill_dir.join("SKILL.md");

    if !skill_md.exists() {
        return Err(blockcell_core::Error::Skill(format!(
            "Skill '{}' has no SKILL.md file",
            name
        )));
    }

    let content = tokio::fs::read_to_string(&skill_md)
        .await
        .map_err(blockcell_core::Error::Io)?;

    // 同时列出 references/ 和 templates/ 子目录
    let references = list_subdir_files(&skill_dir.join("references"));
    let templates = list_subdir_files(&skill_dir.join("templates"));

    // 读取 meta.json
    let meta = read_meta_json(&skill_dir);

    Ok(json!({
        "success": true,
        "name": name,
        "content": content,
        "meta": meta,
        "references": references,
        "templates": templates,
        "skill_dir": skill_dir.to_string_lossy(),
    }))
}

/// 删除 Skill (仅限用户目录, 禁止删除内置 Skill)
async fn delete_skill(name: &str, skills_dir: &Path, external_dirs: &[PathBuf]) -> Result<Value> {
    let skill_dir = find_skill_dir(name, skills_dir, external_dirs)?;
    ensure_confined_target(skills_dir, &skill_dir)?;

    // 安全检查: 禁止删除外部/内置目录中的 Skill
    let is_in_user_dir = skill_dir.starts_with(skills_dir);
    if !is_in_user_dir {
        return Err(blockcell_core::Error::Skill(format!(
            "Cannot delete builtin skill '{}'. Only user-created skills can be deleted.",
            name
        )));
    }

    if !skill_dir.exists() {
        return Err(blockcell_core::Error::Skill(format!(
            "Skill '{}' not found",
            name
        )));
    }

    tokio::fs::remove_dir_all(&skill_dir)
        .await
        .map_err(blockcell_core::Error::Io)?;

    // 清理空 category 目录
    if let Some(category_dir) = skill_dir.parent() {
        if category_dir != skills_dir {
            // 尝试删除空 category 目录 (如果为空则删除, 否则忽略)
            let _ = tokio::fs::remove_dir(category_dir).await;
        }
    }

    tracing::info!(skill_name = name, "[skill_manage] Skill deleted");

    Ok(json!({
        "success": true,
        "message": format!("Skill '{}' deleted", name)
    }))
}

/// 完整替换 SKILL.md 内容 (edit action)
async fn edit_skill(
    name: &str,
    content: &str,
    skills_dir: &Path,
    external_dirs: &[PathBuf],
) -> Result<Value> {
    let skill_dir = find_skill_dir(name, skills_dir, external_dirs)?;

    // 内容大小限制 (使用字符数, 与 security_scan 一致)
    if content.chars().count() > MAX_SKILL_CONTENT_CHARS {
        return Err(blockcell_core::Error::Validation(format!(
            "Skill content exceeds maximum size of {} characters (got {})",
            MAX_SKILL_CONTENT_CHARS,
            content.chars().count()
        )));
    }

    // 验证 frontmatter
    let meta = extract_frontmatter(content);
    validate_frontmatter(&meta)?;

    // 验证 body 内容 (frontmatter 之后必须有实质内容)
    validate_skill_body(content)?;

    // 备份原内容
    let skill_md = skill_dir.join("SKILL.md");
    ensure_confined_target(skills_dir, &skill_md)?;
    let original_content = if skill_md.exists() {
        tokio::fs::read_to_string(&skill_md)
            .await
            .map_err(blockcell_core::Error::Io)?
    } else {
        String::new()
    };

    // 原子写入
    atomic_write_text(&skill_md, content).await?;

    // 安全扫描 (根据 Skill 位置确定信任级别)
    let trust_level = determine_trust_level(&skill_dir, external_dirs.first().map(|p| p.as_path()));
    let scan_result = scan_skill_dir_with_trust(&skill_dir, trust_level);
    if !scan_result.passed {
        // 回滚
        if !original_content.is_empty() {
            let _ = atomic_write_text(&skill_md, &original_content).await;
        } else {
            let _ = tokio::fs::remove_file(&skill_md).await;
        }
        return Err(blockcell_core::Error::Skill(format!(
            "Security scan failed after edit. Changes rolled back.\n{}",
            format_report(&scan_result)
        )));
    }

    // 更新 meta.json
    if let Ok(meta_json) = serde_json::to_string_pretty(&meta) {
        let _ = atomic_write_text(&skill_dir.join("meta.json"), &meta_json).await;
    }

    tracing::info!(
        skill_name = name,
        "[skill_manage] Skill edited (full replacement)"
    );

    Ok(json!({
        "success": true,
        "message": format!("Skill '{}' edited (full content replaced)", name),
        "warnings": scan_result.issues.iter()
            .filter(|i| i.level == crate::security_scan::IssueLevel::Warning)
            .map(|i| &i.message)
            .collect::<Vec<_>>()
    }))
}

/// 添加 supporting 文件到 Skill 目录 (write_file action)
async fn write_file_skill(
    name: &str,
    file_path: &str,
    file_content: &str,
    skills_dir: &Path,
    external_dirs: &[PathBuf],
) -> Result<Value> {
    validate_skill_file_path(file_path)?;

    // 仅允许写入 references/, templates/, scripts/, assets/ 子目录
    let allowed_prefixes = ["references/", "templates/", "scripts/", "assets/"];
    if !allowed_prefixes
        .iter()
        .any(|prefix| file_path.starts_with(prefix))
    {
        return Err(blockcell_core::Error::Validation(format!(
            "file_path must be under one of: {}. Got: '{}'",
            allowed_prefixes.join(", "),
            file_path
        )));
    }

    let skill_dir = find_skill_dir(name, skills_dir, external_dirs)?;
    let target = skill_dir.join(file_path);
    ensure_confined_target(skills_dir, &target)?;

    // 确保父目录存在
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(blockcell_core::Error::Io)?;
    }

    // 安全扫描 (根据 Skill 位置确定信任级别)
    let trust_level = determine_trust_level(&skill_dir, external_dirs.first().map(|p| p.as_path()));
    let scan_result = scan_skill_content_with_trust(file_content, trust_level);
    if !scan_result.passed {
        return Err(blockcell_core::Error::Skill(format!(
            "Security scan failed for file content.\n{}",
            format_report(&scan_result)
        )));
    }

    // 原子写入
    atomic_write_text(&target, file_content).await?;

    // 文件级安全扫描 (检查二进制文件、符号链接、结构问题等)
    let dir_scan = scan_skill_dir_with_trust(&skill_dir, trust_level);
    if !dir_scan.passed {
        // 写入的文件导致目录级安全问题 → 回滚
        let _ = tokio::fs::remove_file(&target).await;
        return Err(blockcell_core::Error::Skill(format!(
            "Directory-level security scan failed after writing file. File removed.\n{}",
            format_report(&dir_scan)
        )));
    }

    tracing::info!(
        skill_name = name,
        file_path,
        "[skill_manage] Supporting file written"
    );

    Ok(json!({
        "success": true,
        "message": format!("File '{}' written to skill '{}'", file_path, name)
    }))
}

/// 删除 Skill 目录中的 supporting 文件 (remove_file action)
async fn remove_file_skill(
    name: &str,
    file_path: &str,
    skills_dir: &Path,
    external_dirs: &[PathBuf],
) -> Result<Value> {
    validate_skill_file_path(file_path)?;

    // 不允许删除 SKILL.md 或 meta.json
    if file_path == "SKILL.md" || file_path == "meta.json" {
        return Err(blockcell_core::Error::Validation(
            "Cannot delete SKILL.md or meta.json. Use delete action to remove the entire skill."
                .to_string(),
        ));
    }

    let skill_dir = find_skill_dir(name, skills_dir, external_dirs)?;
    let target = skill_dir.join(file_path);
    ensure_confined_target(skills_dir, &target)?;

    if !target.exists() {
        return Err(blockcell_core::Error::Skill(format!(
            "File '{}' not found in skill '{}'",
            file_path, name
        )));
    }

    tokio::fs::remove_file(&target)
        .await
        .map_err(blockcell_core::Error::Io)?;

    // 清理空父目录
    if let Some(parent) = target.parent() {
        if parent != skill_dir {
            let _ = tokio::fs::remove_dir(parent).await;
        }
    }

    tracing::info!(
        skill_name = name,
        file_path,
        "[skill_manage] Supporting file removed"
    );

    Ok(json!({
        "success": true,
        "message": format!("File '{}' removed from skill '{}'", file_path, name)
    }))
}

/// 原子写入: 先写入临时文件, 再 rename 替换目标文件
/// 防止进程崩溃留下半写文件 (参考 Hermes _atomic_write_text)
///
/// 使用包含进程 ID 和时间戳的唯一临时文件名，避免并发写入时的临时文件冲突。
pub async fn atomic_write_text(path: &Path, content: &str) -> Result<()> {
    let pid = std::process::id();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let temp_path = path.with_extension(format!("tmp.{}.{}", pid, timestamp));

    // 写入临时文件
    tokio::fs::write(&temp_path, content)
        .await
        .map_err(blockcell_core::Error::Io)?;

    // 原子替换 (rename 在同一文件系统上是原子操作)
    tokio::fs::rename(&temp_path, path)
        .await
        .map_err(blockcell_core::Error::Io)?;

    Ok(())
}

mod helpers;
pub use helpers::extract_frontmatter;
use helpers::{
    determine_trust_level, ensure_confined_target, find_skill_dir, list_subdir_files,
    read_meta_json, validate_frontmatter, validate_path_component, validate_skill_body,
    validate_skill_file_path,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_skills_dir() -> PathBuf {
        let mut dir = std::env::temp_dir();
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        dir.push(format!(
            "blockcell_skill_manage_test_{}_{}",
            std::process::id(),
            now_ns
        ));
        dir
    }

    #[test]
    fn test_schema() {
        let tool = SkillManageTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "skill_manage");
    }

    #[tokio::test]
    async fn test_atomic_write_text_creates_file_with_extension() {
        let dir = temp_skills_dir();
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("memorytest.md");

        atomic_write_text(&path, "temp").await.unwrap();

        let written = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(written, "temp");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn test_validate_create_ok() {
        let tool = SkillManageTool;
        assert!(tool
            .validate(&json!({
                "action": "create",
                "name": "test",
                "category": "devops",
                "content": "# Test Skill"
            }))
            .is_ok());
    }

    #[test]
    fn test_validate_create_missing_content() {
        let tool = SkillManageTool;
        assert!(tool
            .validate(&json!({
                "action": "create",
                "name": "test",
                "category": "devops"
            }))
            .is_err());
    }

    #[test]
    fn test_validate_patch_ok() {
        let tool = SkillManageTool;
        assert!(tool
            .validate(&json!({
                "action": "patch",
                "name": "test",
                "old_string": "foo",
                "new_string": "bar"
            }))
            .is_ok());
    }

    #[test]
    fn test_validate_view_ok() {
        let tool = SkillManageTool;
        assert!(tool
            .validate(&json!({"action": "view", "name": "test"}))
            .is_ok());
    }

    #[test]
    fn test_validate_delete_ok() {
        let tool = SkillManageTool;
        assert!(tool
            .validate(&json!({"action": "delete", "name": "test"}))
            .is_ok());
    }

    #[tokio::test]
    async fn test_create_skill() {
        let skills_dir = temp_skills_dir();
        let result = create_skill(
            "test-skill",
            Some("devops"),
            "---\nname: test-skill\ndescription: A test skill\n---\n# Test Skill\n\nSteps:\n1. Do something\n2. Do something else",
            &skills_dir,
        )
        .await
        .unwrap();

        assert_eq!(result["success"], true);
        assert!(result["message"].as_str().unwrap().contains("test-skill"));

        // 验证文件创建
        let skill_dir = skills_dir.join("devops").join("test-skill");
        assert!(skill_dir.join("SKILL.md").exists());
        assert!(skill_dir.join("meta.json").exists());

        let _ = std::fs::remove_dir_all(&skills_dir);
    }

    #[tokio::test]
    async fn test_create_skill_no_category() {
        let skills_dir = temp_skills_dir();
        let result = create_skill(
            "nocat-skill",
            None,
            "---\nname: nocat-skill\ndescription: A skill without category\n---\n# No Category Skill",
            &skills_dir,
        )
        .await
        .unwrap();

        assert_eq!(result["success"], true);
        // 无 category 时直接放在 skills/ 下
        let skill_dir = skills_dir.join("nocat-skill");
        assert!(skill_dir.join("SKILL.md").exists());

        let _ = std::fs::remove_dir_all(&skills_dir);
    }

    #[tokio::test]
    async fn test_create_skill_duplicate_fails() {
        let skills_dir = temp_skills_dir();
        create_skill(
            "dup-skill",
            Some("general"),
            "---\nname: dup-skill\ndescription: A dup skill\n---\n# Dup",
            &skills_dir,
        )
        .await
        .unwrap();
        let result = create_skill(
            "dup-skill",
            Some("general"),
            "---\nname: dup-skill\ndescription: A dup skill\n---\n# Dup Again",
            &skills_dir,
        )
        .await;
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&skills_dir);
    }

    #[tokio::test]
    async fn test_create_skill_security_block() {
        let skills_dir = temp_skills_dir();
        let result = create_skill(
            "evil-skill",
            Some("general"),
            "---\nname: evil-skill\ndescription: An evil skill\n---\n# Evil\n\nRun: rm -rf /",
            &skills_dir,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Security scan failed"));

        // 验证目录被回滚删除
        let skill_dir = skills_dir.join("general").join("evil-skill");
        assert!(!skill_dir.exists());

        let _ = std::fs::remove_dir_all(&skills_dir);
    }

    #[tokio::test]
    async fn test_view_skill() {
        let skills_dir = temp_skills_dir();
        create_skill(
            "view-skill",
            Some("general"),
            "---\nname: view-skill\ndescription: A view skill\n---\n# View Test\n\nContent here",
            &skills_dir,
        )
        .await
        .unwrap();

        let result = view_skill("view-skill", &skills_dir, &[]).await.unwrap();
        assert_eq!(result["success"], true);
        assert_eq!(result["name"], "view-skill");
        assert!(result["content"].as_str().unwrap().contains("View Test"));

        let _ = std::fs::remove_dir_all(&skills_dir);
    }

    #[tokio::test]
    async fn test_patch_skill() {
        let skills_dir = temp_skills_dir();
        create_skill(
            "patch-skill",
            Some("general"),
            "---\nname: patch-skill\ndescription: A patch skill\n---\n# Patch Test\n\nStep 1: foo\nStep 2: bar",
            &skills_dir,
        )
        .await
        .unwrap();

        let result = patch_skill(
            "patch-skill",
            "Step 1: foo",
            "Step 1: baz",
            false,
            None,
            &skills_dir,
            &[],
        )
        .await
        .unwrap();

        assert_eq!(result["success"], true);
        assert_eq!(result["match_count"], 1);

        // 验证内容已更新
        let content = std::fs::read_to_string(
            skills_dir
                .join("general")
                .join("patch-skill")
                .join("SKILL.md"),
        )
        .unwrap();
        assert!(content.contains("Step 1: baz"));
        assert!(!content.contains("Step 1: foo"));

        let _ = std::fs::remove_dir_all(&skills_dir);
    }

    #[tokio::test]
    async fn test_patch_skill_empty_new_string() {
        // new_string 可为空 — 删除匹配文本
        let skills_dir = temp_skills_dir();
        create_skill(
            "patch-empty",
            Some("general"),
            "---\nname: patch-empty\ndescription: Patch empty test\n---\n# Test\n\nDELETE_ME\nEnd",
            &skills_dir,
        )
        .await
        .unwrap();

        let result = patch_skill(
            "patch-empty",
            "DELETE_ME\n",
            "",
            false,
            None,
            &skills_dir,
            &[],
        )
        .await;
        assert!(result.is_ok());

        let content = std::fs::read_to_string(
            skills_dir
                .join("general")
                .join("patch-empty")
                .join("SKILL.md"),
        )
        .unwrap();
        assert!(!content.contains("DELETE_ME"));
        assert!(content.contains("End"));

        let _ = std::fs::remove_dir_all(&skills_dir);
    }

    #[tokio::test]
    async fn test_delete_skill() {
        let skills_dir = temp_skills_dir();
        create_skill(
            "del-skill",
            Some("general"),
            "---\nname: del-skill\ndescription: A del skill\n---\n# Delete Test",
            &skills_dir,
        )
        .await
        .unwrap();

        let result = delete_skill("del-skill", &skills_dir, &[]).await.unwrap();
        assert_eq!(result["success"], true);

        // 验证目录已删除
        assert!(!skills_dir.join("general").join("del-skill").exists());

        let _ = std::fs::remove_dir_all(&skills_dir);
    }

    #[tokio::test]
    async fn test_edit_skill() {
        let skills_dir = temp_skills_dir();
        create_skill(
            "edit-skill",
            Some("general"),
            "---\nname: edit-skill\ndescription: Old description\n---\n# Old Content",
            &skills_dir,
        )
        .await
        .unwrap();

        let new_content = "---\nname: edit-skill\ndescription: New description\n---\n# New Content\n\nUpdated via edit action";
        let result = edit_skill("edit-skill", new_content, &skills_dir, &[])
            .await
            .unwrap();
        assert_eq!(result["success"], true);

        // 验证内容已替换
        let content = std::fs::read_to_string(
            skills_dir
                .join("general")
                .join("edit-skill")
                .join("SKILL.md"),
        )
        .unwrap();
        assert!(content.contains("New Content"));
        assert!(!content.contains("Old Content"));

        let _ = std::fs::remove_dir_all(&skills_dir);
    }

    #[tokio::test]
    async fn test_write_file_skill() {
        let skills_dir = temp_skills_dir();
        create_skill(
            "wf-skill",
            Some("general"),
            "---\nname: wf-skill\ndescription: Write file test\n---\n# Test",
            &skills_dir,
        )
        .await
        .unwrap();

        // 写入 references 文件
        let result = write_file_skill(
            "wf-skill",
            "references/api_doc.md",
            "# API Documentation\n\n## Endpoints\n- GET /health",
            &skills_dir,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(result["success"], true);

        let ref_path = skills_dir
            .join("general")
            .join("wf-skill")
            .join("references")
            .join("api_doc.md");
        assert!(ref_path.exists());

        let _ = std::fs::remove_dir_all(&skills_dir);
    }

    #[tokio::test]
    async fn test_remove_file_skill() {
        let skills_dir = temp_skills_dir();
        create_skill(
            "rf-skill",
            Some("general"),
            "---\nname: rf-skill\ndescription: Remove file test\n---\n# Test",
            &skills_dir,
        )
        .await
        .unwrap();

        // 先写入一个文件
        write_file_skill(
            "rf-skill",
            "scripts/deploy.sh",
            "#!/bin/bash\necho deploy",
            &skills_dir,
            &[],
        )
        .await
        .unwrap();

        // 删除该文件
        let result = remove_file_skill("rf-skill", "scripts/deploy.sh", &skills_dir, &[])
            .await
            .unwrap();
        assert_eq!(result["success"], true);

        let file_path = skills_dir
            .join("general")
            .join("rf-skill")
            .join("scripts")
            .join("deploy.sh");
        assert!(!file_path.exists());

        let _ = std::fs::remove_dir_all(&skills_dir);
    }

    #[tokio::test]
    async fn test_frontmatter_validation_rejects_missing_name() {
        let skills_dir = temp_skills_dir();
        let result = create_skill(
            "no-name",
            Some("general"),
            "---\ndescription: No name field\n---\n# Test",
            &skills_dir,
        )
        .await;
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&skills_dir);
    }

    #[tokio::test]
    async fn test_content_size_limit() {
        let skills_dir = temp_skills_dir();
        // 超过 MAX_SKILL_CONTENT_CHARS 限制
        let long_content = format!(
            "---\nname: too-long\ndescription: Too long skill\n---\n# Test\n{}",
            "x".repeat(MAX_SKILL_CONTENT_CHARS + 1)
        );
        let result = create_skill("too-long", Some("general"), &long_content, &skills_dir).await;
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&skills_dir);
    }

    #[test]
    fn test_extract_frontmatter() {
        let content = "---\nname: test\ndescription: A test skill\nalways: true\ntools: [read_file, exec]\n---\n\n# Content";
        let meta = extract_frontmatter(content);
        assert_eq!(meta["name"], "test");
        assert_eq!(meta["description"], "A test skill");
        assert_eq!(meta["always"], true);
        assert!(meta["tools"].is_array());
    }

    #[test]
    fn test_extract_frontmatter_no_frontmatter() {
        let content = "# Just content\n\nNo frontmatter here";
        let meta = extract_frontmatter(content);
        assert!(meta.as_object().unwrap().is_empty());
    }

    #[test]
    fn test_find_skill_dir() {
        let skills_dir = temp_skills_dir();
        let skill_dir = skills_dir.join("devops").join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        // 创建 SKILL.md 以标识为有效的 Skill 目录
        std::fs::write(skill_dir.join("SKILL.md"), "# My Skill").unwrap();

        // 通过 name 查找 (遍历 category)
        let found = find_skill_dir("my-skill", &skills_dir, &[]).unwrap();
        assert_eq!(found, skill_dir);

        let _ = std::fs::remove_dir_all(&skills_dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_write_file_rejects_symlinked_support_directory() {
        use std::os::unix::fs::symlink;

        let skills_dir = temp_skills_dir();
        let outside = skills_dir.with_extension("outside");
        std::fs::create_dir_all(&outside).unwrap();
        create_skill(
            "linked-skill",
            None,
            "---\nname: linked-skill\ndescription: Link test\n---\n# Test",
            &skills_dir,
        )
        .await
        .unwrap();
        symlink(&outside, skills_dir.join("linked-skill").join("scripts")).unwrap();

        let result = write_file_skill(
            "linked-skill",
            "scripts/escape.sh",
            "echo escaped",
            &skills_dir,
            &[],
        )
        .await;
        assert!(result.is_err());
        assert!(!outside.join("escape.sh").exists());

        let _ = std::fs::remove_dir_all(&skills_dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn test_find_skill_dir_rejects_symlinked_skill() {
        use std::os::unix::fs::symlink;

        let skills_dir = temp_skills_dir();
        let outside = skills_dir.with_extension("outside-skill");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("SKILL.md"), "# Outside").unwrap();
        symlink(&outside, skills_dir.join("linked-skill")).unwrap();

        assert!(find_skill_dir("linked-skill", &skills_dir, &[]).is_err());

        let _ = std::fs::remove_dir_all(&skills_dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn test_path_traversal_blocked() {
        // 路径遍历攻击应被阻止 (is_skill_name=false, 仅检查路径遍历)
        assert!(validate_path_component("../../etc/passwd", false).is_err());
        assert!(validate_path_component("foo/bar", false).is_err());
        assert!(validate_path_component("foo\\bar", false).is_err());
        assert!(validate_path_component("..", false).is_err());
        assert!(validate_path_component("", false).is_err());

        // 正常名称应通过 (is_skill_name=true, 额外校验格式)
        assert!(validate_path_component("my-skill", true).is_ok());
        assert!(validate_path_component("flask_k8s_deploy", true).is_ok());
        // category 不需要格式校验
        assert!(validate_path_component("devops", false).is_ok());

        // Skill 名称格式校验: 大写、中文、空格应被拒绝
        assert!(validate_path_component("MySkill", true).is_err());
        assert!(validate_path_component("中文技能", true).is_err());
        assert!(validate_path_component("has space", true).is_err());
    }
}
