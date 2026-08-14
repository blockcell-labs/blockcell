use async_trait::async_trait;
use blockcell_core::{Config, Paths, Result};
use reqwest::Url;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::{Tool, ToolContext, ToolSchema};
use blockcell_skills::audit::static_audit;
use blockcell_skills::SkillType;

/// CommunityHubTool — interact with the Blockcell Community Hub.
/// Used by Ghost Agent for social interactions and by users for skill discovery.
pub struct CommunityHubTool;

fn validate_skill_name(name: &str) -> Result<()> {
    let mut components = std::path::Path::new(name).components();
    let single_normal = matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none();
    if !single_normal || name == "." || name == ".." || name.contains('\0') {
        return Err(blockcell_core::Error::Validation(format!(
            "Invalid skill name '{name}': expected a single path component"
        )));
    }
    Ok(())
}

fn confined_skill_dir(workspace: &std::path::Path, name: &str) -> Result<std::path::PathBuf> {
    validate_skill_name(name)?;
    let workspace = std::fs::canonicalize(workspace).map_err(blockcell_core::Error::Io)?;
    let skills_dir = workspace.join("skills");
    std::fs::create_dir_all(&skills_dir).map_err(blockcell_core::Error::Io)?;
    let canonical_skills = std::fs::canonicalize(&skills_dir).map_err(blockcell_core::Error::Io)?;
    if !canonical_skills.starts_with(&workspace) {
        return Err(blockcell_core::Error::PermissionDenied(
            "Skills directory resolves outside the workspace".to_string(),
        ));
    }

    let target = canonical_skills.join(name);
    if target.exists() {
        let canonical_target = std::fs::canonicalize(&target).map_err(blockcell_core::Error::Io)?;
        if !canonical_target.starts_with(&canonical_skills) {
            return Err(blockcell_core::Error::PermissionDenied(format!(
                "Skill '{name}' resolves outside the skills directory"
            )));
        }
    }
    Ok(target)
}

fn is_executable_candidate(root: &std::path::Path, path: &std::path::Path, content: &str) -> bool {
    if content.starts_with("#!") {
        return true;
    }
    path.strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .and_then(|component| match component {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .is_some_and(|dir| matches!(dir, "scripts" | "bin" | "hooks"))
}

fn audit_hub_skill_dir(root: &std::path::Path) -> Result<()> {
    fn visit(root: &std::path::Path, dir: &std::path::Path) -> Result<()> {
        for entry in std::fs::read_dir(dir).map_err(blockcell_core::Error::Io)? {
            let entry = entry.map_err(blockcell_core::Error::Io)?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(blockcell_core::Error::Io)?;
            if file_type.is_dir() {
                visit(root, &path)?;
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let known_type = if file_name.eq_ignore_ascii_case("SKILL.md") {
                Some(SkillType::PromptOnly)
            } else {
                match extension.as_str() {
                    "py" => Some(SkillType::Python),
                    "rhai" => Some(SkillType::Rhai),
                    "sh" | "bash" | "zsh" | "js" | "php" | "rb" => Some(SkillType::LocalScript),
                    _ => None,
                }
            };

            let bytes = std::fs::read(&path).map_err(blockcell_core::Error::Io)?;
            let content = match String::from_utf8(bytes) {
                Ok(content) => content,
                Err(_) if known_type.is_some() => {
                    return Err(blockcell_core::Error::Tool(format!(
                        "Hub skill audit failed: executable file '{}' is not valid UTF-8",
                        path.strip_prefix(root).unwrap_or(&path).display()
                    )));
                }
                Err(_) => continue,
            };
            let skill_type = known_type.or_else(|| {
                is_executable_candidate(root, &path, &content).then_some(SkillType::LocalScript)
            });
            let Some(skill_type) = skill_type else {
                continue;
            };

            let audit = static_audit(&skill_type, &content);
            let blocking = audit
                .violations
                .iter()
                .filter(|violation| violation.severity == "error")
                .map(|violation| format!("{}: {}", violation.rule, violation.message))
                .collect::<Vec<_>>();
            if !blocking.is_empty() {
                return Err(blockcell_core::Error::Tool(format!(
                    "Hub skill audit failed for '{}': {}",
                    path.strip_prefix(root).unwrap_or(&path).display(),
                    blocking.join("; ")
                )));
            }
        }
        Ok(())
    }

    visit(root, root)
}

fn redact_hub_url(url: &str) -> String {
    // Avoid leaking internal endpoints or hostnames to logs/LLM.
    // We keep only scheme + host if possible.
    if let Ok(u) = Url::parse(url) {
        let scheme = u.scheme();
        let host = u.host_str().unwrap_or("hub");
        let port = u.port().map(|p| format!(":{}", p)).unwrap_or_default();
        return format!("{}://{}{}", scheme, host, port);
    }
    "hub".to_string()
}

fn resolve_download_url(hub_url: &str, skill_name: &str, info: &Value) -> String {
    let dist_url = info.get("dist_url").and_then(|v| v.as_str());
    let source_url = info.get("source_url").and_then(|v| v.as_str());

    if let Some(u) = dist_url.or(source_url) {
        if u.starts_with("http://") || u.starts_with("https://") {
            return u.to_string();
        }
        let base = hub_url.trim_end_matches('/');
        if u.starts_with('/') {
            return format!("{}{}", base, u);
        }
        return format!("{}/{}", base, u);
    }

    format!(
        "{}/v1/skills/{}/download",
        hub_url.trim_end_matches('/'),
        urlencoding::encode(skill_name)
    )
}

fn resolve_hub_url(ctx: &ToolContext, params: &Value) -> Option<String> {
    let _ = params;
    // 1. Config community_hub.hub_url
    if let Some(url) = ctx.config.community_hub_url() {
        return Some(url);
    }
    // 1b. Fallback: reload config.json5 from disk (runtime may have stale config)
    if let Some(url) = load_config_from_disk(ctx).and_then(|cfg| cfg.community_hub_url()) {
        return Some(url);
    }
    // 2. Environment variable
    if let Ok(url) = std::env::var("BLOCKCELL_HUB_URL") {
        if !url.is_empty() {
            return Some(url.trim_end_matches('/').to_string());
        }
    }
    None
}

fn resolve_api_key(ctx: &ToolContext, params: &Value) -> Option<String> {
    let _ = params;
    if let Some(key) = ctx.config.community_hub_api_key() {
        return Some(key);
    }
    if let Some(key) = load_config_from_disk(ctx).and_then(|cfg| cfg.community_hub_api_key()) {
        return Some(key);
    }
    if let Ok(key) = std::env::var("BLOCKCELL_HUB_API_KEY") {
        if !key.is_empty() {
            return Some(key);
        }
    }
    None
}

fn load_config_from_disk(ctx: &ToolContext) -> Option<Config> {
    // 使用 ctx.base 直接获取 base 目录（如 ~/.blockcell），
    // 而非从 workspace 反推。当配置了 workspace_override 时，
    // workspace.parent() 不再是 base 目录。
    let paths = Paths::with_base(ctx.base.clone());
    Config::load(&paths.config_file()).ok()
}

async fn hub_get(client: &reqwest::Client, url: &str, api_key: &Option<String>) -> Result<Value> {
    let mut req = client.get(url);
    if let Some(key) = api_key {
        req = req.header("Authorization", format!("Bearer {}", key));
    }
    let resp = req
        .send()
        .await
        .map_err(|e| blockcell_core::Error::Tool(format!("Hub request failed: {}", e)))?;
    let status = resp.status();
    let (body, truncated) = crate::bounded_io::read_response_limited(resp, 2 * 1024 * 1024).await?;
    if truncated {
        return Err(blockcell_core::Error::Tool(
            "Hub response exceeded size limit".to_string(),
        ));
    }
    let body = String::from_utf8_lossy(&body).into_owned();
    if !status.is_success() {
        return Err(blockcell_core::Error::Tool(format!(
            "Hub returned {}: {}",
            status, body
        )));
    }
    serde_json::from_str(&body)
        .map_err(|e| blockcell_core::Error::Tool(format!("Invalid JSON from hub: {}", e)))
}

async fn hub_post(
    client: &reqwest::Client,
    url: &str,
    api_key: &Option<String>,
    body: Value,
) -> Result<Value> {
    let mut req = client.post(url).json(&body);
    if let Some(key) = api_key {
        req = req.header("Authorization", format!("Bearer {}", key));
    }
    let resp = req
        .send()
        .await
        .map_err(|e| blockcell_core::Error::Tool(format!("Hub request failed: {}", e)))?;
    let status = resp.status();
    let (resp_body, truncated) =
        crate::bounded_io::read_response_limited(resp, 2 * 1024 * 1024).await?;
    if truncated {
        return Err(blockcell_core::Error::Tool(
            "Hub response exceeded size limit".to_string(),
        ));
    }
    let resp_body = String::from_utf8_lossy(&resp_body).into_owned();
    if !status.is_success() {
        return Err(blockcell_core::Error::Tool(format!(
            "Hub returned {}: {}",
            status, resp_body
        )));
    }
    serde_json::from_str(&resp_body)
        .map_err(|e| blockcell_core::Error::Tool(format!("Invalid JSON from hub: {}", e)))
}

#[async_trait]
impl Tool for CommunityHubTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "community_hub".to_string(),
            description: "Interact with the Blockcell Community Hub. You MUST provide `action`. action='heartbeat'|'trending'|'feed'|'list_installed': no extra params. action='search_skills'|'node_search': requires `query`, optional `tags`. action='skill_info'|'install_skill'|'uninstall_skill': requires `skill_name`. action='post': requires `content`. action='like'|'get_replies': requires `post_id`. action='reply': requires `post_id` and `content`. Connection settings are resolved internally.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["heartbeat", "trending", "search_skills", "skill_info", "install_skill", "uninstall_skill", "list_installed", "feed", "post", "like", "reply", "get_replies", "node_search"],
                        "description": "Action to perform"
                    },
                    "query": {
                        "type": "string",
                        "description": "Search query (for search_skills, node_search)"
                    },
                    "skill_name": {
                        "type": "string",
                        "description": "Skill name (for skill_info, install_skill, uninstall_skill)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Post content (for post action)"
                    },
                    "post_id": {
                        "type": "string",
                        "description": "Post ID (for like action)"
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Tags for heartbeat or node_search"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- **Community Hub 技能安装**: 用户说「安装技能」「从Hub安装」「下载技能」「install skill」时，**必须**使用 `community_hub` 工具，流程：①先调用 action='list_installed' 查本地是否已装；②调用 action='skill_info' skill_name='xxx' 查Hub上该技能信息；③调用 action='install_skill' skill_name='xxx' 下载安装。卸载用 action='uninstall_skill'，浏览用 action='trending' 或 action='search_skills'。Hub URL 和 API key 自动从配置读取，无需手动填写。".to_string())
    }

    fn validate(&self, params: &Value) -> Result<()> {
        // Security: hub_url/api_key must never be accepted from tool params.
        // These values are resolved internally from config/env only.
        if params.get("hub_url").is_some() || params.get("api_key").is_some() {
            return Err(blockcell_core::Error::Tool(
                "Do not pass connection settings in tool params. Configure Community Hub on the system side.".into(),
            ));
        }

        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "heartbeat" | "trending" | "feed" => Ok(()),
            "search_skills" | "node_search" => {
                if params
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
                {
                    Err(blockcell_core::Error::Tool(
                        "'query' is required for search actions".into(),
                    ))
                } else {
                    Ok(())
                }
            }
            "skill_info" | "install_skill" | "uninstall_skill" => {
                let name = params
                    .get("skill_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if name.is_empty() {
                    return Err(blockcell_core::Error::Tool(
                        "'skill_name' is required for this action".into(),
                    ));
                }
                validate_skill_name(name)
            }
            "list_installed" => Ok(()),
            "post" => {
                if params
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
                {
                    Err(blockcell_core::Error::Tool(
                        "'content' is required for post action".into(),
                    ))
                } else {
                    Ok(())
                }
            }
            "reply" => {
                if params
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
                {
                    Err(blockcell_core::Error::Tool(
                        "'content' is required for reply action".into(),
                    ))
                } else if params
                    .get("post_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
                {
                    Err(blockcell_core::Error::Tool(
                        "'post_id' is required for reply action".into(),
                    ))
                } else {
                    Ok(())
                }
            }
            "like" | "get_replies" => {
                if params
                    .get("post_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
                {
                    Err(blockcell_core::Error::Tool(
                        "'post_id' is required for this action".into(),
                    ))
                } else {
                    Ok(())
                }
            }
            _ => Err(blockcell_core::Error::Tool(format!(
                "Unknown action: {}",
                action
            ))),
        }
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");

        // Local-only actions that don't need hub_url
        if action == "list_installed" {
            let skills_dir = ctx.workspace.join("skills");
            let mut installed = Vec::new();
            if skills_dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                    for entry in entries.flatten() {
                        if entry.path().is_dir() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            let meta_path = entry.path().join("meta.yaml");
                            let description = if meta_path.exists() {
                                std::fs::read_to_string(&meta_path)
                                    .ok()
                                    .and_then(|s| {
                                        for line in s.lines() {
                                            if line.starts_with("description:") {
                                                return Some(
                                                    line.trim_start_matches("description:")
                                                        .trim()
                                                        .trim_matches('"')
                                                        .to_string(),
                                                );
                                            }
                                        }
                                        None
                                    })
                                    .unwrap_or_default()
                            } else {
                                String::new()
                            };
                            installed.push(json!({ "name": name, "description": description }));
                        }
                    }
                }
            }
            return Ok(json!({
                "installed_skills": installed,
                "count": installed.len(),
                "skills_dir": skills_dir.display().to_string(),
            }));
        }

        if action == "uninstall_skill" {
            let name = params
                .get("skill_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let skill_dir = confined_skill_dir(&ctx.workspace, name)?;
            if !skill_dir.exists() {
                return Err(blockcell_core::Error::Tool(format!(
                    "Skill '{}' is not installed",
                    name
                )));
            }
            std::fs::remove_dir_all(&skill_dir).map_err(|e| {
                blockcell_core::Error::Tool(format!("Failed to remove skill: {}", e))
            })?;
            info!(skill = %name, "Skill uninstalled");
            return Ok(json!({
                "status": "uninstalled",
                "skill_name": name,
            }));
        }

        // All remaining actions require hub_url
        let hub_url = resolve_hub_url(&ctx, &params)
            .ok_or_else(|| blockcell_core::Error::Tool("Community Hub 未启用或未配置。".into()))?;
        let api_key = resolve_api_key(&ctx, &params);

        let client = crate::ssrf::safe_client_builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| blockcell_core::Error::Tool(format!("Failed to build Hub client: {e}")))?;

        match action {
            "heartbeat" => {
                let tags = params
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let tags: Vec<String> = tags
                    .into_iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();

                let body = json!({
                    "tags": tags,
                    "version": env!("CARGO_PKG_VERSION"),
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });

                let url = format!("{}/v1/nodes/heartbeat", hub_url);
                info!(hub = %redact_hub_url(&hub_url), "Community Hub: sending heartbeat");
                let result = hub_post(&client, &url, &api_key, body).await?;
                Ok(json!({ "status": "ok", "response": result }))
            }

            "trending" => {
                let url = format!("{}/v1/skills/trending", hub_url);
                debug!(hub = %redact_hub_url(&hub_url), "Community Hub: fetching trending skills");
                let result = hub_get(&client, &url, &api_key).await?;
                Ok(json!({ "trending_skills": result }))
            }

            "search_skills" => {
                let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let url = format!(
                    "{}/v1/skills/search?q={}",
                    hub_url,
                    urlencoding::encode(query)
                );
                debug!(hub = %redact_hub_url(&hub_url), "Community Hub: searching skills");
                let result = hub_get(&client, &url, &api_key).await?;
                Ok(json!({ "results": result }))
            }

            "skill_info" => {
                let name = params
                    .get("skill_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let url = format!("{}/v1/skills/{}/latest", hub_url, urlencoding::encode(name));
                debug!(hub = %redact_hub_url(&hub_url), "Community Hub: fetching skill info");
                let result = hub_get(&client, &url, &api_key).await?;
                Ok(result)
            }

            "feed" => {
                let url = format!("{}/v1/feed", hub_url);
                debug!(hub = %redact_hub_url(&hub_url), "Community Hub: fetching feed");
                let result = hub_get(&client, &url, &api_key).await?;
                Ok(json!({ "status": "posted", "response": result }))
            }

            "reply" => {
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let post_id = params.get("post_id").and_then(|v| v.as_str()).unwrap_or("");
                let url = format!("{}/v1/feed/{}/reply", hub_url, post_id);
                let body = json!({ "content": content });
                info!(post_id = %post_id, hub = %redact_hub_url(&hub_url), "Community Hub: replying to post");
                let result = hub_post(&client, &url, &api_key, body).await?;
                Ok(json!({ "status": "replied", "response": result }))
            }

            "get_replies" => {
                let post_id = params.get("post_id").and_then(|v| v.as_str()).unwrap_or("");
                let url = format!("{}/v1/feed/{}/replies", hub_url, post_id);
                debug!(post_id = %post_id, hub = %redact_hub_url(&hub_url), "Community Hub: fetching replies");
                let result = hub_get(&client, &url, &api_key).await?;
                Ok(json!({ "replies": result }))
            }

            "post" => {
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let url = format!("{}/v1/feed", hub_url);
                let body = json!({ "content": content });
                info!("Community Hub: posting update");
                let result = hub_post(&client, &url, &api_key, body).await?;
                Ok(json!({ "status": "posted", "response": result }))
            }

            "like" => {
                let post_id = params.get("post_id").and_then(|v| v.as_str()).unwrap_or("");
                let url = format!("{}/v1/feed/{}/like", hub_url, post_id);
                let result = hub_post(&client, &url, &api_key, json!({})).await?;
                Ok(json!({ "status": "liked", "response": result }))
            }

            "install_skill" => {
                let name = params
                    .get("skill_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                info!(skill = %name, "Community Hub: installing skill");

                let info_url = format!(
                    "{}/v1/skills/{}/latest",
                    hub_url.trim_end_matches('/'),
                    urlencoding::encode(name)
                );
                debug!(skill = %name, url = %info_url, "Community Hub: resolving skill metadata");
                let info = hub_get(&client, &info_url, &api_key).await.unwrap_or_else(|e| {
                    warn!(skill = %name, err = %e, "Community Hub: failed to fetch skill metadata; falling back to download endpoint");
                    json!({})
                });

                let url = resolve_download_url(&hub_url, name, &info);
                debug!(skill = %name, url = %url, "Community Hub: resolved download url");

                crate::ssrf::ensure_url_allowed(&url).await?;

                let mut req = client.get(&url);
                let same_origin = reqwest::Url::parse(&url)
                    .ok()
                    .zip(reqwest::Url::parse(&hub_url).ok())
                    .is_some_and(|(download, hub)| {
                        download.scheme() == hub.scheme()
                            && download.host_str() == hub.host_str()
                            && download.port_or_known_default() == hub.port_or_known_default()
                    });
                if same_origin {
                    if let Some(ref key) = api_key {
                        req = req.header("Authorization", format!("Bearer {}", key));
                    }
                }
                let resp = req
                    .send()
                    .await
                    .map_err(|e| blockcell_core::Error::Tool(format!("Download failed: {}", e)))?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let (body, _) =
                        crate::bounded_io::read_response_limited(resp, 64 * 1024).await?;
                    let body = String::from_utf8_lossy(&body);
                    return Err(blockcell_core::Error::Tool(format!(
                        "Download failed ({}): {}",
                        status, body
                    )));
                }
                let (zip_bytes, truncated) =
                    crate::bounded_io::read_response_limited(resp, 20 * 1024 * 1024).await?;
                if truncated {
                    return Err(blockcell_core::Error::Tool(
                        "Skill archive exceeded the 20 MiB download limit".to_string(),
                    ));
                }

                // Extract into a sibling staging directory so untrusted content is audited
                // before it can replace an installed skill.
                let skill_dir = confined_skill_dir(&ctx.workspace, name)?;
                let skills_dir = skill_dir.parent().ok_or_else(|| {
                    blockcell_core::Error::Tool("Invalid skills directory".to_string())
                })?;
                let install_id = uuid::Uuid::new_v4();
                let staging_dir = skills_dir.join(format!(".{name}.install-{install_id}"));
                std::fs::create_dir(&staging_dir).map_err(|e| {
                    blockcell_core::Error::Tool(format!(
                        "Failed to create skill staging dir: {}",
                        e
                    ))
                })?;

                let extraction_result = (|| -> Result<()> {
                    let cursor = std::io::Cursor::new(&zip_bytes);
                    let mut archive = zip::ZipArchive::new(cursor)
                        .map_err(|e| blockcell_core::Error::Tool(format!("Invalid zip: {}", e)))?;
                    if archive.len() > 2_000 {
                        return Err(blockcell_core::Error::Tool(
                            "Skill archive contains too many entries".to_string(),
                        ));
                    }
                    let mut total_uncompressed = 0u64;
                    for i in 0..archive.len() {
                        let mut file = archive.by_index(i).map_err(|e| {
                            blockcell_core::Error::Tool(format!("Zip read error: {}", e))
                        })?;
                        total_uncompressed = total_uncompressed.saturating_add(file.size());
                        if file.size() > 20 * 1024 * 1024
                            || total_uncompressed > 100 * 1024 * 1024
                            || (file.compressed_size() > 0
                                && file.size() / file.compressed_size() > 1_000)
                        {
                            return Err(blockcell_core::Error::Tool(
                                "Skill archive exceeds extraction safety limits".to_string(),
                            ));
                        }
                        let out_path = if let Some(enclosed) = file.enclosed_name() {
                            // Strip the top-level directory if the zip contains one.
                            let components: Vec<_> = enclosed.components().collect();
                            if components.len() > 1 {
                                staging_dir
                                    .join(components[1..].iter().collect::<std::path::PathBuf>())
                            } else {
                                staging_dir.join(enclosed)
                            }
                        } else {
                            continue;
                        };
                        if file.is_dir() {
                            std::fs::create_dir_all(&out_path).map_err(|e| {
                                blockcell_core::Error::Tool(format!(
                                    "Failed to create skill directory: {}",
                                    e
                                ))
                            })?;
                        } else {
                            if let Some(parent) = out_path.parent() {
                                std::fs::create_dir_all(parent).map_err(|e| {
                                    blockcell_core::Error::Tool(format!(
                                        "Failed to create skill directory: {}",
                                        e
                                    ))
                                })?;
                            }
                            let mut outfile = std::fs::File::create(&out_path).map_err(|e| {
                                blockcell_core::Error::Tool(format!("Failed to create file: {}", e))
                            })?;
                            std::io::copy(&mut file, &mut outfile).map_err(|e| {
                                blockcell_core::Error::Tool(format!("Failed to write file: {}", e))
                            })?;
                        }
                    }
                    audit_hub_skill_dir(&staging_dir)
                })();

                if let Err(error) = extraction_result {
                    let _ = std::fs::remove_dir_all(&staging_dir);
                    return Err(error);
                }

                let backup_dir = skills_dir.join(format!(".{name}.backup-{install_id}"));
                if skill_dir.exists() {
                    std::fs::rename(&skill_dir, &backup_dir).map_err(|e| {
                        let _ = std::fs::remove_dir_all(&staging_dir);
                        blockcell_core::Error::Tool(format!(
                            "Failed to stage existing skill for replacement: {}",
                            e
                        ))
                    })?;
                }
                if let Err(error) = std::fs::rename(&staging_dir, &skill_dir) {
                    if backup_dir.exists() {
                        let _ = std::fs::rename(&backup_dir, &skill_dir);
                    }
                    let _ = std::fs::remove_dir_all(&staging_dir);
                    return Err(blockcell_core::Error::Tool(format!(
                        "Failed to promote audited skill: {}",
                        error
                    )));
                }
                if backup_dir.exists() {
                    if let Err(error) = std::fs::remove_dir_all(&backup_dir) {
                        warn!(
                            skill = %name,
                            path = %backup_dir.display(),
                            error = %error,
                            "Failed to remove replaced skill backup"
                        );
                    }
                }

                info!(skill = %name, path = %skill_dir.display(), "Skill installed successfully");
                Ok(json!({
                    "status": "installed",
                    "skill_name": name,
                    "install_path": skill_dir.display().to_string(),
                    "size_bytes": zip_bytes.len(),
                }))
            }

            "node_search" => {
                let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let tags = params
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default();
                let url = format!(
                    "{}/v1/nodes/search?q={}&tags={}",
                    hub_url,
                    urlencoding::encode(query),
                    urlencoding::encode(&tags)
                );
                debug!(url = %url, "Community Hub: searching nodes");
                let result = hub_get(&client, &url, &api_key).await?;
                Ok(json!({ "nodes": result }))
            }

            _ => {
                warn!(action = %action, "Community Hub: unknown action");
                Err(blockcell_core::Error::Tool(format!(
                    "Unknown action: {}",
                    action
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_schema() {
        let tool = CommunityHubTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "community_hub");
    }

    #[test]
    fn test_validate_heartbeat() {
        let tool = CommunityHubTool;
        assert!(tool.validate(&json!({"action": "heartbeat"})).is_ok());
    }

    #[test]
    fn test_validate_search_requires_query() {
        let tool = CommunityHubTool;
        assert!(tool.validate(&json!({"action": "search_skills"})).is_err());
        assert!(tool
            .validate(&json!({"action": "search_skills", "query": "finance"}))
            .is_ok());
    }

    #[test]
    fn test_validate_post_requires_content() {
        let tool = CommunityHubTool;
        assert!(tool.validate(&json!({"action": "post"})).is_err());
        assert!(tool
            .validate(&json!({"action": "post", "content": "hello"}))
            .is_ok());
    }

    #[test]
    fn test_validate_like_requires_post_id() {
        let tool = CommunityHubTool;
        assert!(tool.validate(&json!({"action": "like"})).is_err());
        assert!(tool
            .validate(&json!({"action": "like", "post_id": "abc123"}))
            .is_ok());
    }

    #[test]
    fn test_validate_skill_name_rejects_path_escape() {
        let tool = CommunityHubTool;
        for name in ["../outside", "nested/skill", "/tmp/skill", ".", ".."] {
            assert!(
                tool.validate(&json!({"action": "install_skill", "skill_name": name}))
                    .is_err(),
                "unsafe skill name should be rejected: {name}"
            );
            assert!(
                tool.validate(&json!({"action": "uninstall_skill", "skill_name": name}))
                    .is_err(),
                "unsafe skill name should be rejected: {name}"
            );
        }
        assert!(tool
            .validate(&json!({"action": "install_skill", "skill_name": "safe-skill"}))
            .is_ok());
    }

    #[test]
    fn test_hub_skill_audit_rejects_python_bypass_payload() {
        let temp = tempfile::tempdir().expect("create temp dir");
        fs::write(
            temp.path().join("SKILL.py"),
            "import subprocess as sp\nsp.run(['sh', '-c', 'echo pwned'])\n",
        )
        .expect("write payload");

        let error = audit_hub_skill_dir(temp.path()).expect_err("payload must be rejected");
        assert!(error.to_string().contains("SKILL.py"));
    }

    #[test]
    fn test_hub_skill_audit_rejects_shell_bypass_payload() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let scripts = temp.path().join("scripts");
        fs::create_dir_all(&scripts).expect("create scripts dir");
        fs::write(scripts.join("cleanup.sh"), "#!/bin/sh\nrm -Rf ~/.config\n")
            .expect("write payload");

        let error = audit_hub_skill_dir(temp.path()).expect_err("payload must be rejected");
        assert!(error.to_string().contains("cleanup.sh"));
    }

    #[test]
    fn test_hub_skill_audit_accepts_clean_skill() {
        let temp = tempfile::tempdir().expect("create temp dir");
        fs::write(
            temp.path().join("SKILL.md"),
            "# Safe skill\n\nThis skill formats local input without running external commands or changing files. It returns a concise text summary to the user.\n",
        )
        .expect("write skill manual");
        fs::write(temp.path().join("SKILL.py"), "print('safe')\n").expect("write script");

        audit_hub_skill_dir(temp.path()).expect("clean skill should pass");
    }
}
