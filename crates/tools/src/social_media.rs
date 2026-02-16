use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
#[allow(unused_imports)]
use tracing::info;

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for social media platform integrations.
///
/// Supports:
/// - Twitter/X API v2 (post, delete, timeline, search, user info)
/// - Medium (publish articles via API)
/// - WordPress REST API (posts, pages, media)
/// - Generic social (via http_request fallback)
pub struct SocialMediaTool;

#[async_trait]
impl Tool for SocialMediaTool {
    fn schema(&self) -> ToolSchema {
        let mut params = serde_json::Map::new();

        params.insert("platform".to_string(), json!({
            "type": "string",
            "enum": ["twitter", "medium", "wordpress"],
            "description": "Social media platform"
        }));
        params.insert("action".to_string(), json!({
            "type": "string",
            "description": "Action to perform. Twitter: post/delete/timeline/search/user_info/thread. Medium: publish/list_publications. WordPress: create_post/update_post/list_posts/delete_post/upload_media/list_categories."
        }));
        params.insert("api_token".to_string(), json!({
            "type": "string",
            "description": "API token/bearer token. Falls back to config providers or env vars (TWITTER_BEARER_TOKEN, MEDIUM_TOKEN, etc.)"
        }));
        params.insert("api_base".to_string(), json!({
            "type": "string",
            "description": "(wordpress) WordPress site URL, e.g. 'https://example.com'. Required for WordPress."
        }));

        // Twitter params
        params.insert("text".to_string(), json!({
            "type": "string",
            "description": "(twitter:post/thread) Tweet text content. For thread, separate tweets with '---'."
        }));
        params.insert("tweet_id".to_string(), json!({
            "type": "string",
            "description": "(twitter:delete) Tweet ID to delete"
        }));
        params.insert("query".to_string(), json!({
            "type": "string",
            "description": "(twitter:search) Search query"
        }));
        params.insert("username".to_string(), json!({
            "type": "string",
            "description": "(twitter:user_info/timeline) Twitter username (without @)"
        }));
        params.insert("count".to_string(), json!({
            "type": "integer",
            "description": "Number of results to return. Default: 10"
        }));
        params.insert("reply_to".to_string(), json!({
            "type": "string",
            "description": "(twitter:post) Tweet ID to reply to"
        }));

        // Medium params
        params.insert("title".to_string(), json!({
            "type": "string",
            "description": "(medium:publish, wordpress:create_post) Article/post title"
        }));
        params.insert("content".to_string(), json!({
            "type": "string",
            "description": "(medium:publish, wordpress:create_post/update_post) Article content (HTML or Markdown)"
        }));
        params.insert("content_format".to_string(), json!({
            "type": "string",
            "enum": ["html", "markdown"],
            "description": "(medium) Content format. Default: markdown"
        }));
        params.insert("tags".to_string(), json!({
            "type": "array",
            "items": { "type": "string" },
            "description": "(medium:publish, wordpress) Tags for the post"
        }));
        params.insert("publish_status".to_string(), json!({
            "type": "string",
            "enum": ["public", "draft", "unlisted"],
            "description": "(medium) Publish status. Default: draft"
        }));
        params.insert("publication_id".to_string(), json!({
            "type": "string",
            "description": "(medium:publish) Publication ID to publish under"
        }));

        // WordPress params
        params.insert("post_id".to_string(), json!({
            "type": "integer",
            "description": "(wordpress:update_post/delete_post) Post ID"
        }));
        params.insert("status".to_string(), json!({
            "type": "string",
            "enum": ["publish", "draft", "pending", "private"],
            "description": "(wordpress) Post status. Default: draft"
        }));
        params.insert("categories".to_string(), json!({
            "type": "array",
            "items": { "type": "integer" },
            "description": "(wordpress) Category IDs"
        }));
        params.insert("featured_media".to_string(), json!({
            "type": "integer",
            "description": "(wordpress:create_post/update_post) Featured image media ID"
        }));
        params.insert("file_path".to_string(), json!({
            "type": "string",
            "description": "(wordpress:upload_media) Local file path to upload"
        }));

        ToolSchema {
            name: "social_media",
            description: "Manage social media platforms. Supports Twitter/X (post/delete/timeline/search/thread), Medium (publish articles), and WordPress (create/update/list/delete posts, upload media).",
            parameters: Value::Object({
                let mut schema = serde_json::Map::new();
                schema.insert("type".to_string(), json!("object"));
                schema.insert("properties".to_string(), Value::Object(params));
                schema.insert("required".to_string(), json!(["platform", "action"]));
                schema
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let platform = params.get("platform").and_then(|v| v.as_str()).unwrap_or("");
        if !["twitter", "medium", "wordpress"].contains(&platform) {
            return Err(Error::Tool("platform must be 'twitter', 'medium', or 'wordpress'".into()));
        }
        if params.get("action").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
            return Err(Error::Tool("'action' is required".into()));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let platform = params.get("platform").and_then(|v| v.as_str()).unwrap_or("");
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");

        match platform {
            "twitter" => execute_twitter(&ctx, action, &params).await,
            "medium" => execute_medium(&ctx, action, &params).await,
            "wordpress" => execute_wordpress(&ctx, action, &params).await,
            _ => Err(Error::Tool(format!("Unknown platform: {}", platform))),
        }
    }
}

// ═══════════════════════════════════════════════════════════
// Twitter/X API v2
// ═══════════════════════════════════════════════════════════

async fn execute_twitter(ctx: &ToolContext, action: &str, params: &Value) -> Result<Value> {
    let token = resolve_token(ctx, params, "twitter", "TWITTER_BEARER_TOKEN")?;

    match action {
        "post" => twitter_post(&token, params).await,
        "thread" => twitter_thread(&token, params).await,
        "delete" => twitter_delete(&token, params).await,
        "timeline" => twitter_timeline(&token, params).await,
        "search" => twitter_search(&token, params).await,
        "user_info" => twitter_user_info(&token, params).await,
        _ => Err(Error::Tool(format!("Unknown twitter action: {}", action))),
    }
}

async fn twitter_post(token: &str, params: &Value) -> Result<Value> {
    let text = params.get("text").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("'text' is required for twitter post".into()))?;

    let mut body = json!({"text": text});
    if let Some(reply_to) = params.get("reply_to").and_then(|v| v.as_str()) {
        body["reply"] = json!({"in_reply_to_tweet_id": reply_to});
    }

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.twitter.com/2/tweets")
        .header("Authorization", format!("Bearer {}", token))
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Twitter API failed: {}", e)))?;

    parse_response(response, "twitter post").await
}

async fn twitter_thread(token: &str, params: &Value) -> Result<Value> {
    let text = params.get("text").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("'text' is required for twitter thread".into()))?;

    let tweets: Vec<&str> = text.split("---").map(|t| t.trim()).filter(|t| !t.is_empty()).collect();
    if tweets.is_empty() {
        return Err(Error::Tool("No tweets found in thread text. Separate tweets with '---'.".into()));
    }

    let client = reqwest::Client::new();
    let mut results = Vec::new();
    let mut reply_to: Option<String> = None;

    for (i, tweet_text) in tweets.iter().enumerate() {
        let mut body = json!({"text": tweet_text});
        if let Some(ref prev_id) = reply_to {
            body["reply"] = json!({"in_reply_to_tweet_id": prev_id});
        }

        let response = client
            .post("https://api.twitter.com/2/tweets")
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Tool(format!("Twitter thread tweet {} failed: {}", i + 1, e)))?;

        let data = parse_response(response, &format!("thread tweet {}", i + 1)).await?;
        reply_to = data["data"]["id"].as_str().map(|s| s.to_string());
        results.push(data);
    }

    Ok(json!({
        "status": "ok",
        "thread_length": results.len(),
        "tweets": results
    }))
}

async fn twitter_delete(token: &str, params: &Value) -> Result<Value> {
    let tweet_id = params.get("tweet_id").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("'tweet_id' is required for delete".into()))?;

    let client = reqwest::Client::new();
    let response = client
        .delete(format!("https://api.twitter.com/2/tweets/{}", tweet_id))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Twitter delete failed: {}", e)))?;

    parse_response(response, "twitter delete").await
}

async fn twitter_timeline(token: &str, params: &Value) -> Result<Value> {
    let username = params.get("username").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("'username' is required for timeline".into()))?;
    let count = params.get("count").and_then(|v| v.as_u64()).unwrap_or(10);

    // First get user ID
    let client = reqwest::Client::new();
    let user_resp = client
        .get(format!("https://api.twitter.com/2/users/by/username/{}", username))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Twitter user lookup failed: {}", e)))?;

    let user_data = parse_response(user_resp, "user lookup").await?;
    let user_id = user_data["data"]["id"].as_str()
        .ok_or_else(|| Error::Tool("User not found".into()))?;

    let response = client
        .get(format!("https://api.twitter.com/2/users/{}/tweets", user_id))
        .header("Authorization", format!("Bearer {}", token))
        .query(&[
            ("max_results", count.to_string()),
            ("tweet.fields", "created_at,public_metrics".to_string()),
        ])
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Twitter timeline failed: {}", e)))?;

    parse_response(response, "twitter timeline").await
}

async fn twitter_search(token: &str, params: &Value) -> Result<Value> {
    let query = params.get("query").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("'query' is required for search".into()))?;
    let count = params.get("count").and_then(|v| v.as_u64()).unwrap_or(10);

    let client = reqwest::Client::new();
    let response = client
        .get("https://api.twitter.com/2/tweets/search/recent")
        .header("Authorization", format!("Bearer {}", token))
        .query(&[
            ("query", query.to_string()),
            ("max_results", count.max(10).to_string()),
            ("tweet.fields", "created_at,public_metrics,author_id".to_string()),
        ])
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Twitter search failed: {}", e)))?;

    parse_response(response, "twitter search").await
}

async fn twitter_user_info(token: &str, params: &Value) -> Result<Value> {
    let username = params.get("username").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("'username' is required for user_info".into()))?;

    let client = reqwest::Client::new();
    let response = client
        .get(format!("https://api.twitter.com/2/users/by/username/{}", username))
        .header("Authorization", format!("Bearer {}", token))
        .query(&[("user.fields", "description,public_metrics,created_at,profile_image_url")])
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Twitter user info failed: {}", e)))?;

    parse_response(response, "twitter user_info").await
}

// ═══════════════════════════════════════════════════════════
// Medium API
// ═══════════════════════════════════════════════════════════

async fn execute_medium(ctx: &ToolContext, action: &str, params: &Value) -> Result<Value> {
    let token = resolve_token(ctx, params, "medium", "MEDIUM_TOKEN")?;

    match action {
        "publish" => medium_publish(&token, params).await,
        "list_publications" => medium_list_publications(&token).await,
        _ => Err(Error::Tool(format!("Unknown medium action: {}", action))),
    }
}

async fn medium_publish(token: &str, params: &Value) -> Result<Value> {
    let title = params.get("title").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("'title' is required for medium publish".into()))?;
    let content = params.get("content").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("'content' is required for medium publish".into()))?;
    let content_format = params.get("content_format").and_then(|v| v.as_str()).unwrap_or("markdown");
    let publish_status = params.get("publish_status").and_then(|v| v.as_str()).unwrap_or("draft");
    let tags: Vec<String> = params.get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    // Get user ID first
    let client = reqwest::Client::new();
    let me_resp = client
        .get("https://api.medium.com/v1/me")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Medium API failed: {}", e)))?;

    let me_data = parse_response(me_resp, "medium me").await?;
    let user_id = me_data["data"]["id"].as_str()
        .ok_or_else(|| Error::Tool("Failed to get Medium user ID".into()))?;

    // Publish
    let url = if let Some(pub_id) = params.get("publication_id").and_then(|v| v.as_str()) {
        format!("https://api.medium.com/v1/publications/{}/posts", pub_id)
    } else {
        format!("https://api.medium.com/v1/users/{}/posts", user_id)
    };

    let mut body = json!({
        "title": title,
        "contentFormat": content_format,
        "content": content,
        "publishStatus": publish_status
    });
    if !tags.is_empty() {
        body["tags"] = json!(tags);
    }

    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", token))
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Medium publish failed: {}", e)))?;

    parse_response(response, "medium publish").await
}

async fn medium_list_publications(token: &str) -> Result<Value> {
    let client = reqwest::Client::new();

    let me_resp = client
        .get("https://api.medium.com/v1/me")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Medium API failed: {}", e)))?;

    let me_data = parse_response(me_resp, "medium me").await?;
    let user_id = me_data["data"]["id"].as_str()
        .ok_or_else(|| Error::Tool("Failed to get Medium user ID".into()))?;

    let response = client
        .get(format!("https://api.medium.com/v1/users/{}/publications", user_id))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Medium publications failed: {}", e)))?;

    parse_response(response, "medium publications").await
}

// ═══════════════════════════════════════════════════════════
// WordPress REST API
// ═══════════════════════════════════════════════════════════

async fn execute_wordpress(ctx: &ToolContext, action: &str, params: &Value) -> Result<Value> {
    let token = resolve_token(ctx, params, "wordpress", "WORDPRESS_TOKEN")?;
    let api_base_str: String = if let Some(b) = params.get("api_base").and_then(|v| v.as_str()) {
        b.to_string()
    } else if let Some(p) = ctx.config.providers.get("wordpress") {
        p.api_base.clone().unwrap_or_default()
    } else {
        std::env::var("WORDPRESS_URL").unwrap_or_default()
    };
    if api_base_str.is_empty() {
        return Err(Error::Tool("'api_base' (WordPress site URL) is required".into()));
    }
    let api_base = api_base_str.trim_end_matches('/').to_string();

    let wp_api = format!("{}/wp-json/wp/v2", api_base);

    match action {
        "create_post" => wp_create_post(&token, &wp_api, params).await,
        "update_post" => wp_update_post(&token, &wp_api, params).await,
        "list_posts" => wp_list_posts(&token, &wp_api, params).await,
        "delete_post" => wp_delete_post(&token, &wp_api, params).await,
        "upload_media" => wp_upload_media(ctx, &token, &wp_api, params).await,
        "list_categories" => wp_list_categories(&token, &wp_api).await,
        _ => Err(Error::Tool(format!("Unknown wordpress action: {}", action))),
    }
}

async fn wp_create_post(token: &str, api: &str, params: &Value) -> Result<Value> {
    let title = params.get("title").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("'title' is required".into()))?;
    let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let status = params.get("status").and_then(|v| v.as_str()).unwrap_or("draft");

    let mut body = json!({
        "title": title,
        "content": content,
        "status": status
    });

    if let Some(cats) = params.get("categories") {
        body["categories"] = cats.clone();
    }
    if let Some(tags) = params.get("tags").and_then(|v| v.as_array()) {
        // WordPress expects tag IDs, but we accept names and note it
        body["tags"] = json!(tags);
    }
    if let Some(media) = params.get("featured_media").and_then(|v| v.as_i64()) {
        body["featured_media"] = json!(media);
    }

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/posts", api))
        .header("Authorization", format!("Bearer {}", token))
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("WordPress create post failed: {}", e)))?;

    parse_response(response, "wordpress create_post").await
}

async fn wp_update_post(token: &str, api: &str, params: &Value) -> Result<Value> {
    let post_id = params.get("post_id").and_then(|v| v.as_i64())
        .ok_or_else(|| Error::Tool("'post_id' is required for update_post".into()))?;

    let mut body = json!({});
    if let Some(title) = params.get("title").and_then(|v| v.as_str()) {
        body["title"] = json!(title);
    }
    if let Some(content) = params.get("content").and_then(|v| v.as_str()) {
        body["content"] = json!(content);
    }
    if let Some(status) = params.get("status").and_then(|v| v.as_str()) {
        body["status"] = json!(status);
    }

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/posts/{}", api, post_id))
        .header("Authorization", format!("Bearer {}", token))
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("WordPress update post failed: {}", e)))?;

    parse_response(response, "wordpress update_post").await
}

async fn wp_list_posts(token: &str, api: &str, params: &Value) -> Result<Value> {
    let count = params.get("count").and_then(|v| v.as_u64()).unwrap_or(10);

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/posts", api))
        .header("Authorization", format!("Bearer {}", token))
        .query(&[("per_page", count.to_string()), ("_fields", "id,title,status,date,link".to_string())])
        .send()
        .await
        .map_err(|e| Error::Tool(format!("WordPress list posts failed: {}", e)))?;

    parse_response(response, "wordpress list_posts").await
}

async fn wp_delete_post(token: &str, api: &str, params: &Value) -> Result<Value> {
    let post_id = params.get("post_id").and_then(|v| v.as_i64())
        .ok_or_else(|| Error::Tool("'post_id' is required for delete_post".into()))?;

    let client = reqwest::Client::new();
    let response = client
        .delete(format!("{}/posts/{}", api, post_id))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| Error::Tool(format!("WordPress delete post failed: {}", e)))?;

    parse_response(response, "wordpress delete_post").await
}

async fn wp_upload_media(ctx: &ToolContext, token: &str, api: &str, params: &Value) -> Result<Value> {
    let file_path = params.get("file_path").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("'file_path' is required for upload_media".into()))?;

    let resolved = resolve_path(file_path, &ctx.workspace);
    let bytes = std::fs::read(&resolved)
        .map_err(|e| Error::Tool(format!("Failed to read file: {}", e)))?;

    let filename = std::path::Path::new(&resolved)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("upload");

    let ext = std::path::Path::new(&resolved)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin")
        .to_lowercase();
    let content_type = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    };

    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename.to_string())
        .mime_str(content_type)
        .map_err(|e| Error::Tool(format!("Failed to create multipart: {}", e)))?;

    let form = reqwest::multipart::Form::new().part("file", part);

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/media", api))
        .header("Authorization", format!("Bearer {}", token))
        .multipart(form)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("WordPress upload failed: {}", e)))?;

    parse_response(response, "wordpress upload_media").await
}

async fn wp_list_categories(token: &str, api: &str) -> Result<Value> {
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/categories", api))
        .header("Authorization", format!("Bearer {}", token))
        .query(&[("per_page", "100")])
        .send()
        .await
        .map_err(|e| Error::Tool(format!("WordPress list categories failed: {}", e)))?;

    parse_response(response, "wordpress list_categories").await
}

// ═══════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════

fn resolve_token(ctx: &ToolContext, params: &Value, provider: &str, env_var: &str) -> Result<String> {
    // 1. Explicit param
    if let Some(token) = params.get("api_token").and_then(|v| v.as_str()) {
        if !token.is_empty() { return Ok(token.to_string()); }
    }
    // 2. Config providers section
    if let Some(p) = ctx.config.providers.get(provider) {
        if !p.api_key.is_empty() { return Ok(p.api_key.clone()); }
    }
    // 3. Environment variable
    if let Ok(token) = std::env::var(env_var) {
        if !token.is_empty() { return Ok(token); }
    }
    Err(Error::Tool(format!(
        "API token not found for {}. Set via api_token param, config providers.{}.api_key, or {} env var.",
        provider, provider, env_var
    )))
}

async fn parse_response(response: reqwest::Response, context: &str) -> Result<Value> {
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Tool(format!("{} error {}: {}", context, status, text)));
    }

    let data: Value = response.json().await
        .map_err(|e| Error::Tool(format!("Failed to parse {} response: {}", context, e)))?;

    Ok(data)
}

fn resolve_path(path: &str, workspace: &std::path::Path) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else if path.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            home.join(&path[2..]).display().to_string()
        } else {
            path.to_string()
        }
    } else {
        workspace.join(path).display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_social_media_schema() {
        let tool = SocialMediaTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "social_media");
    }

    #[test]
    fn test_validate_twitter() {
        let tool = SocialMediaTool;
        assert!(tool.validate(&json!({"platform": "twitter", "action": "post"})).is_ok());
        assert!(tool.validate(&json!({"platform": "twitter", "action": ""})).is_err());
    }

    #[test]
    fn test_validate_medium() {
        let tool = SocialMediaTool;
        assert!(tool.validate(&json!({"platform": "medium", "action": "publish"})).is_ok());
    }

    #[test]
    fn test_validate_wordpress() {
        let tool = SocialMediaTool;
        assert!(tool.validate(&json!({"platform": "wordpress", "action": "create_post"})).is_ok());
    }

    #[test]
    fn test_validate_invalid_platform() {
        let tool = SocialMediaTool;
        assert!(tool.validate(&json!({"platform": "instagram", "action": "post"})).is_err());
    }

    #[test]
    fn test_validate_missing_action() {
        let tool = SocialMediaTool;
        assert!(tool.validate(&json!({"platform": "twitter"})).is_err());
    }
}
