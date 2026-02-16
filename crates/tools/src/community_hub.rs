use async_trait::async_trait;
use blockcell_core::Result;
use reqwest::Url;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::{Tool, ToolContext, ToolSchema};

/// CommunityHubTool — interact with the Blockcell Community Hub.
/// Used by Ghost Agent for social interactions and by users for skill discovery.
pub struct CommunityHubTool;

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

fn resolve_hub_url(ctx: &ToolContext, params: &Value) -> Option<String> {
    let _ = params;
    // 1. Config community_hub.hub_url
    if let Some(url) = ctx.config.community_hub_url() {
        return Some(url);
    }
    // 1b. Fallback: reload config.json from disk (runtime may have stale config)
    if let Some(url) = load_config_from_disk(ctx)
        .and_then(|cfg| cfg.community_hub_url())
    {
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
    if let Some(key) = load_config_from_disk(ctx)
        .and_then(|cfg| cfg.community_hub_api_key())
    {
        return Some(key);
    }
    if let Ok(key) = std::env::var("BLOCKCELL_HUB_API_KEY") {
        if !key.is_empty() {
            return Some(key);
        }
    }
    None
}

fn load_config_from_disk(ctx: &ToolContext) -> Option<blockcell_core::Config> {
    // ctx.workspace is typically ~/.blockcell/workspace
    // Config lives at ~/.blockcell/config.json
    let base_dir = ctx.workspace.parent()?.to_path_buf();
    let config_path = base_dir.join("config.json");
    let content = std::fs::read_to_string(&config_path).ok()?;
    serde_json::from_str::<blockcell_core::Config>(&content).ok()
}

async fn hub_get(client: &reqwest::Client, url: &str, api_key: &Option<String>) -> Result<Value> {
    let mut req = client.get(url);
    if let Some(key) = api_key {
        req = req.header("Authorization", format!("Bearer {}", key));
    }
    let resp = req.send().await.map_err(|e| blockcell_core::Error::Tool(format!("Hub request failed: {}", e)))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(blockcell_core::Error::Tool(format!("Hub returned {}: {}", status, body)));
    }
    serde_json::from_str(&body).map_err(|e| blockcell_core::Error::Tool(format!("Invalid JSON from hub: {}", e)))
}

async fn hub_post(client: &reqwest::Client, url: &str, api_key: &Option<String>, body: Value) -> Result<Value> {
    let mut req = client.post(url).json(&body);
    if let Some(key) = api_key {
        req = req.header("Authorization", format!("Bearer {}", key));
    }
    let resp = req.send().await.map_err(|e| blockcell_core::Error::Tool(format!("Hub request failed: {}", e)))?;
    let status = resp.status();
    let resp_body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(blockcell_core::Error::Tool(format!("Hub returned {}: {}", status, resp_body)));
    }
    serde_json::from_str(&resp_body).map_err(|e| blockcell_core::Error::Tool(format!("Invalid JSON from hub: {}", e)))
}

#[async_trait]
impl Tool for CommunityHubTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "community_hub",
            description: "Interact with the Blockcell Community Hub — send heartbeat, browse skills, view feed, post updates, like posts. Connection settings are resolved internally. If not configured, the tool returns an error.",
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
                if params.get("query").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    Err(blockcell_core::Error::Tool("'query' is required for search actions".into()))
                } else {
                    Ok(())
                }
            }
            "skill_info" | "install_skill" | "uninstall_skill" => {
                if params.get("skill_name").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    Err(blockcell_core::Error::Tool("'skill_name' is required for this action".into()))
                } else {
                    Ok(())
                }
            }
            "list_installed" => Ok(()),
            "post" => {
                if params.get("content").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    Err(blockcell_core::Error::Tool("'content' is required for post action".into()))
                } else {
                    Ok(())
                }
            }
            "reply" => {
                if params.get("content").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    Err(blockcell_core::Error::Tool("'content' is required for reply action".into()))
                } else if params.get("post_id").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    Err(blockcell_core::Error::Tool("'post_id' is required for reply action".into()))
                } else {
                    Ok(())
                }
            }
            "like" | "get_replies" => {
                if params.get("post_id").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    Err(blockcell_core::Error::Tool("'post_id' is required for this action".into()))
                } else {
                    Ok(())
                }
            }
            _ => Err(blockcell_core::Error::Tool(format!("Unknown action: {}", action))),
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
                                std::fs::read_to_string(&meta_path).ok()
                                    .and_then(|s| {
                                        for line in s.lines() {
                                            if line.starts_with("description:") {
                                                return Some(line.trim_start_matches("description:").trim().trim_matches('"').to_string());
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
            let name = params.get("skill_name").and_then(|v| v.as_str()).unwrap_or("");
            let skill_dir = ctx.workspace.join("skills").join(name);
            if !skill_dir.exists() {
                return Err(blockcell_core::Error::Tool(format!("Skill '{}' is not installed", name)));
            }
            std::fs::remove_dir_all(&skill_dir).map_err(|e| blockcell_core::Error::Tool(format!("Failed to remove skill: {}", e)))?;
            info!(skill = %name, "Skill uninstalled");
            return Ok(json!({
                "status": "uninstalled",
                "skill_name": name,
            }));
        }

        // All remaining actions require hub_url
        let hub_url = resolve_hub_url(&ctx, &params)
            .ok_or_else(|| blockcell_core::Error::Tool(
                "Community Hub 未启用或未配置。".into()
            ))?;
        let api_key = resolve_api_key(&ctx, &params);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        match action {
            "heartbeat" => {
                let tags = params.get("tags")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let tags: Vec<String> = tags.into_iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();

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
                let url = format!("{}/v1/skills/search?q={}", hub_url, urlencoding::encode(query));
                debug!(hub = %redact_hub_url(&hub_url), "Community Hub: searching skills");
                let result = hub_get(&client, &url, &api_key).await?;
                Ok(json!({ "results": result }))
            }

            "skill_info" => {
                let name = params.get("skill_name").and_then(|v| v.as_str()).unwrap_or("");
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
                let name = params.get("skill_name").and_then(|v| v.as_str()).unwrap_or("");
                info!(skill = %name, "Community Hub: installing skill");

                // Download zip from hub
                let url = format!("{}/v1/skills/{}/download", hub_url, urlencoding::encode(name));
                let mut req = client.get(&url);
                if let Some(ref key) = api_key {
                    req = req.header("Authorization", format!("Bearer {}", key));
                }
                let resp = req.send().await.map_err(|e| blockcell_core::Error::Tool(format!("Download failed: {}", e)))?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(blockcell_core::Error::Tool(format!("Download failed ({}): {}", status, body)));
                }
                let zip_bytes = resp.bytes().await.map_err(|e| blockcell_core::Error::Tool(format!("Failed to read response: {}", e)))?;

                // Extract to workspace/skills/{name}/
                let skills_dir = ctx.workspace.join("skills");
                let skill_dir = skills_dir.join(name);
                if skill_dir.exists() {
                    std::fs::remove_dir_all(&skill_dir).map_err(|e| blockcell_core::Error::Tool(format!("Failed to remove existing skill dir: {}", e)))?;
                }
                std::fs::create_dir_all(&skill_dir).map_err(|e| blockcell_core::Error::Tool(format!("Failed to create skill dir: {}", e)))?;

                // Unzip
                let cursor = std::io::Cursor::new(&zip_bytes);
                let mut archive = zip::ZipArchive::new(cursor).map_err(|e| blockcell_core::Error::Tool(format!("Invalid zip: {}", e)))?;
                for i in 0..archive.len() {
                    let mut file = archive.by_index(i).map_err(|e| blockcell_core::Error::Tool(format!("Zip read error: {}", e)))?;
                    let out_path = if let Some(enclosed) = file.enclosed_name() {
                        // Strip the top-level directory if the zip contains one
                        let components: Vec<_> = enclosed.components().collect();
                        if components.len() > 1 {
                            skill_dir.join(components[1..].iter().collect::<std::path::PathBuf>())
                        } else {
                            skill_dir.join(enclosed)
                        }
                    } else {
                        continue;
                    };
                    if file.is_dir() {
                        std::fs::create_dir_all(&out_path).ok();
                    } else {
                        if let Some(parent) = out_path.parent() {
                            std::fs::create_dir_all(parent).ok();
                        }
                        let mut outfile = std::fs::File::create(&out_path).map_err(|e| blockcell_core::Error::Tool(format!("Failed to create file: {}", e)))?;
                        std::io::copy(&mut file, &mut outfile).map_err(|e| blockcell_core::Error::Tool(format!("Failed to write file: {}", e)))?;
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
                let tags = params.get("tags")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(","))
                    .unwrap_or_default();
                let url = format!("{}/v1/nodes/search?q={}&tags={}", hub_url, urlencoding::encode(query), urlencoding::encode(&tags));
                debug!(url = %url, "Community Hub: searching nodes");
                let result = hub_get(&client, &url, &api_key).await?;
                Ok(json!({ "nodes": result }))
            }

            _ => {
                warn!(action = %action, "Community Hub: unknown action");
                Err(blockcell_core::Error::Tool(format!("Unknown action: {}", action)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(tool.validate(&json!({"action": "search_skills", "query": "finance"})).is_ok());
    }

    #[test]
    fn test_validate_post_requires_content() {
        let tool = CommunityHubTool;
        assert!(tool.validate(&json!({"action": "post"})).is_err());
        assert!(tool.validate(&json!({"action": "post", "content": "hello"})).is_ok());
    }

    #[test]
    fn test_validate_like_requires_post_id() {
        let tool = CommunityHubTool;
        assert!(tool.validate(&json!({"action": "like"})).is_err());
        assert!(tool.validate(&json!({"action": "like", "post_id": "abc123"})).is_ok());
    }
}
