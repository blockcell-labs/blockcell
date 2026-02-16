use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// Git/GitHub integration tool.
///
/// Provides access to GitHub REST API v3 for:
/// - Repository info, search, stars/forks
/// - Pull request management (list, create, merge, review)
/// - Issue operations (list, create, update, comment)
/// - Actions/workflow status
/// - Code search
/// - Release management
/// - Webhook registration
pub struct GitApiTool;

#[async_trait]
impl Tool for GitApiTool {
    fn schema(&self) -> ToolSchema {
        let str_prop = |desc: &str| -> Value { json!({"type": "string", "description": desc}) };
        let obj_prop = |desc: &str| -> Value { json!({"type": "object", "description": desc}) };
        let int_prop = |desc: &str| -> Value { json!({"type": "integer", "description": desc}) };
        let arr_str_prop = |desc: &str| -> Value { json!({"type": "array", "items": {"type": "string"}, "description": desc}) };
        let bool_prop = |desc: &str| -> Value { json!({"type": "boolean", "description": desc}) };

        let mut props = serde_json::Map::new();
        props.insert("action".into(), str_prop("Action: repo_info|repo_search|list_prs|get_pr|create_pr|merge_pr|list_pr_reviews|list_issues|get_issue|create_issue|update_issue|add_comment|list_workflows|get_workflow_run|list_releases|create_release|code_search|list_branches|list_commits|get_commit|list_tags|repo_stats|list_webhooks|create_webhook"));
        props.insert("owner".into(), str_prop("Repository owner (user or org)"));
        props.insert("repo".into(), str_prop("Repository name"));
        props.insert("number".into(), int_prop("PR or Issue number"));
        props.insert("title".into(), str_prop("Title for PR/Issue/Release"));
        props.insert("body".into(), str_prop("Body/description text"));
        props.insert("head".into(), str_prop("(create_pr) Head branch"));
        props.insert("base".into(), str_prop("(create_pr) Base branch (default: main)"));
        props.insert("state".into(), str_prop("Filter by state: open|closed|all (default: open)"));
        props.insert("labels".into(), arr_str_prop("Labels for issue/PR"));
        props.insert("assignees".into(), arr_str_prop("Assignees for issue/PR"));
        props.insert("milestone".into(), int_prop("Milestone number"));
        props.insert("query".into(), str_prop("Search query (for repo_search, code_search)"));
        props.insert("sha".into(), str_prop("Commit SHA"));
        props.insert("branch".into(), str_prop("Branch name"));
        props.insert("tag_name".into(), str_prop("(create_release) Tag name"));
        props.insert("draft".into(), bool_prop("(create_release) Is draft release"));
        props.insert("prerelease".into(), bool_prop("(create_release) Is prerelease"));
        props.insert("workflow_id".into(), str_prop("Workflow ID or filename"));
        props.insert("merge_method".into(), str_prop("(merge_pr) Merge method: merge|squash|rebase"));
        props.insert("comment".into(), str_prop("Comment text for add_comment"));
        props.insert("webhook_url".into(), str_prop("(create_webhook) Payload URL"));
        props.insert("webhook_events".into(), arr_str_prop("(create_webhook) Events to subscribe to"));
        props.insert("per_page".into(), int_prop("Results per page (default: 30, max: 100)"));
        props.insert("page".into(), int_prop("Page number (default: 1)"));
        props.insert("sort".into(), str_prop("Sort field (varies by action)"));
        props.insert("direction".into(), str_prop("Sort direction: asc|desc"));
        props.insert("api_token".into(), str_prop("GitHub personal access token (overrides config/env)"));
        props.insert("api_base".into(), str_prop("API base URL (default: https://api.github.com). Set for GitHub Enterprise."));
        props.insert("fields".into(), obj_prop("Additional fields for create/update operations"));

        ToolSchema {
            name: "git_api",
            description: "Interact with GitHub repositories via REST API. Manage PRs, issues, workflows, releases, branches, and more. Requires a GitHub personal access token via api_token param, config providers.github.api_key, or GITHUB_TOKEN env var.",
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid = [
            "repo_info", "repo_search", "list_prs", "get_pr", "create_pr", "merge_pr",
            "list_pr_reviews", "list_issues", "get_issue", "create_issue", "update_issue",
            "add_comment", "list_workflows", "get_workflow_run", "list_releases",
            "create_release", "code_search", "list_branches", "list_commits", "get_commit",
            "list_tags", "repo_stats", "list_webhooks", "create_webhook",
        ];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        // Actions that require owner+repo
        let needs_repo = [
            "repo_info", "list_prs", "get_pr", "create_pr", "merge_pr", "list_pr_reviews",
            "list_issues", "get_issue", "create_issue", "update_issue", "add_comment",
            "list_workflows", "get_workflow_run", "list_releases", "create_release",
            "list_branches", "list_commits", "get_commit", "list_tags", "repo_stats",
            "list_webhooks", "create_webhook",
        ];
        if needs_repo.contains(&action) {
            if params.get("owner").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                return Err(Error::Tool("'owner' is required for this action".into()));
            }
            if params.get("repo").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                return Err(Error::Tool("'repo' is required for this action".into()));
            }
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap_or("");
        let token = resolve_token(&ctx, &params);
        let base = params.get("api_base").and_then(|v| v.as_str()).unwrap_or("https://api.github.com");
        let owner = params.get("owner").and_then(|v| v.as_str()).unwrap_or("");
        let repo = params.get("repo").and_then(|v| v.as_str()).unwrap_or("");
        let per_page = params.get("per_page").and_then(|v| v.as_u64()).unwrap_or(30);
        let page = params.get("page").and_then(|v| v.as_u64()).unwrap_or(1);

        let client = Client::new();

        match action {
            "repo_info" => {
                gh_get(&client, &format!("{}/repos/{}/{}", base, owner, repo), &token).await
            }
            "repo_search" => {
                let q = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
                let sort = params.get("sort").and_then(|v| v.as_str()).unwrap_or("stars");
                gh_get(&client, &format!("{}/search/repositories?q={}&sort={}&per_page={}&page={}", base, urlencoding::encode(q), sort, per_page, page), &token).await
            }
            "list_prs" => {
                let state = params.get("state").and_then(|v| v.as_str()).unwrap_or("open");
                let sort = params.get("sort").and_then(|v| v.as_str()).unwrap_or("created");
                let dir = params.get("direction").and_then(|v| v.as_str()).unwrap_or("desc");
                gh_get(&client, &format!("{}/repos/{}/{}/pulls?state={}&sort={}&direction={}&per_page={}&page={}", base, owner, repo, state, sort, dir, per_page, page), &token).await
            }
            "get_pr" => {
                let num = params.get("number").and_then(|v| v.as_u64())
                    .ok_or_else(|| Error::Tool("'number' is required for get_pr".into()))?;
                gh_get(&client, &format!("{}/repos/{}/{}/pulls/{}", base, owner, repo, num), &token).await
            }
            "create_pr" => {
                let title = params.get("title").and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Tool("'title' is required".into()))?;
                let head = params.get("head").and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Tool("'head' branch is required".into()))?;
                let base_branch = params.get("base").and_then(|v| v.as_str()).unwrap_or("main");
                let body_text = params.get("body").and_then(|v| v.as_str()).unwrap_or("");
                let mut payload = json!({
                    "title": title,
                    "head": head,
                    "base": base_branch,
                    "body": body_text
                });
                if let Some(draft) = params.get("draft").and_then(|v| v.as_bool()) {
                    payload["draft"] = json!(draft);
                }
                gh_post(&client, &format!("{}/repos/{}/{}/pulls", base, owner, repo), &token, &payload).await
            }
            "merge_pr" => {
                let num = params.get("number").and_then(|v| v.as_u64())
                    .ok_or_else(|| Error::Tool("'number' is required".into()))?;
                let method = params.get("merge_method").and_then(|v| v.as_str()).unwrap_or("merge");
                let payload = json!({"merge_method": method});
                gh_put(&client, &format!("{}/repos/{}/{}/pulls/{}/merge", base, owner, repo, num), &token, &payload).await
            }
            "list_pr_reviews" => {
                let num = params.get("number").and_then(|v| v.as_u64())
                    .ok_or_else(|| Error::Tool("'number' is required".into()))?;
                gh_get(&client, &format!("{}/repos/{}/{}/pulls/{}/reviews?per_page={}", base, owner, repo, num, per_page), &token).await
            }
            "list_issues" => {
                let state = params.get("state").and_then(|v| v.as_str()).unwrap_or("open");
                let sort = params.get("sort").and_then(|v| v.as_str()).unwrap_or("created");
                let dir = params.get("direction").and_then(|v| v.as_str()).unwrap_or("desc");
                let mut url = format!("{}/repos/{}/{}/issues?state={}&sort={}&direction={}&per_page={}&page={}", base, owner, repo, state, sort, dir, per_page, page);
                if let Some(labels) = params.get("labels").and_then(|v| v.as_array()) {
                    let label_str: Vec<&str> = labels.iter().filter_map(|l| l.as_str()).collect();
                    if !label_str.is_empty() {
                        url.push_str(&format!("&labels={}", label_str.join(",")));
                    }
                }
                gh_get(&client, &url, &token).await
            }
            "get_issue" => {
                let num = params.get("number").and_then(|v| v.as_u64())
                    .ok_or_else(|| Error::Tool("'number' is required".into()))?;
                gh_get(&client, &format!("{}/repos/{}/{}/issues/{}", base, owner, repo, num), &token).await
            }
            "create_issue" => {
                let title = params.get("title").and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Tool("'title' is required".into()))?;
                let mut payload = json!({"title": title});
                if let Some(b) = params.get("body").and_then(|v| v.as_str()) { payload["body"] = json!(b); }
                if let Some(l) = params.get("labels") { payload["labels"] = l.clone(); }
                if let Some(a) = params.get("assignees") { payload["assignees"] = a.clone(); }
                if let Some(m) = params.get("milestone").and_then(|v| v.as_u64()) { payload["milestone"] = json!(m); }
                gh_post(&client, &format!("{}/repos/{}/{}/issues", base, owner, repo), &token, &payload).await
            }
            "update_issue" => {
                let num = params.get("number").and_then(|v| v.as_u64())
                    .ok_or_else(|| Error::Tool("'number' is required".into()))?;
                let mut payload = json!({});
                if let Some(t) = params.get("title").and_then(|v| v.as_str()) { payload["title"] = json!(t); }
                if let Some(b) = params.get("body").and_then(|v| v.as_str()) { payload["body"] = json!(b); }
                if let Some(s) = params.get("state").and_then(|v| v.as_str()) { payload["state"] = json!(s); }
                if let Some(l) = params.get("labels") { payload["labels"] = l.clone(); }
                if let Some(a) = params.get("assignees") { payload["assignees"] = a.clone(); }
                if let Some(f) = params.get("fields").and_then(|v| v.as_object()) {
                    for (k, v) in f { payload[k] = v.clone(); }
                }
                gh_patch(&client, &format!("{}/repos/{}/{}/issues/{}", base, owner, repo, num), &token, &payload).await
            }
            "add_comment" => {
                let num = params.get("number").and_then(|v| v.as_u64())
                    .ok_or_else(|| Error::Tool("'number' is required".into()))?;
                let comment = params.get("comment").and_then(|v| v.as_str())
                    .or_else(|| params.get("body").and_then(|v| v.as_str()))
                    .ok_or_else(|| Error::Tool("'comment' or 'body' is required".into()))?;
                gh_post(&client, &format!("{}/repos/{}/{}/issues/{}/comments", base, owner, repo, num), &token, &json!({"body": comment})).await
            }
            "list_workflows" => {
                gh_get(&client, &format!("{}/repos/{}/{}/actions/runs?per_page={}&page={}", base, owner, repo, per_page, page), &token).await
            }
            "get_workflow_run" => {
                let wf_id = params.get("workflow_id").and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Tool("'workflow_id' is required".into()))?;
                gh_get(&client, &format!("{}/repos/{}/{}/actions/runs/{}", base, owner, repo, wf_id), &token).await
            }
            "list_releases" => {
                gh_get(&client, &format!("{}/repos/{}/{}/releases?per_page={}&page={}", base, owner, repo, per_page, page), &token).await
            }
            "create_release" => {
                let tag = params.get("tag_name").and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Tool("'tag_name' is required".into()))?;
                let mut payload = json!({"tag_name": tag});
                if let Some(t) = params.get("title").and_then(|v| v.as_str()) { payload["name"] = json!(t); }
                if let Some(b) = params.get("body").and_then(|v| v.as_str()) { payload["body"] = json!(b); }
                if let Some(d) = params.get("draft").and_then(|v| v.as_bool()) { payload["draft"] = json!(d); }
                if let Some(p) = params.get("prerelease").and_then(|v| v.as_bool()) { payload["prerelease"] = json!(p); }
                if let Some(br) = params.get("branch").and_then(|v| v.as_str()) { payload["target_commitish"] = json!(br); }
                gh_post(&client, &format!("{}/repos/{}/{}/releases", base, owner, repo), &token, &payload).await
            }
            "code_search" => {
                let q = params.get("query").and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Tool("'query' is required for code_search".into()))?;
                let full_q = if owner.is_empty() || repo.is_empty() {
                    q.to_string()
                } else {
                    format!("{} repo:{}/{}", q, owner, repo)
                };
                gh_get(&client, &format!("{}/search/code?q={}&per_page={}&page={}", base, urlencoding::encode(&full_q), per_page, page), &token).await
            }
            "list_branches" => {
                gh_get(&client, &format!("{}/repos/{}/{}/branches?per_page={}&page={}", base, owner, repo, per_page, page), &token).await
            }
            "list_commits" => {
                let mut url = format!("{}/repos/{}/{}/commits?per_page={}&page={}", base, owner, repo, per_page, page);
                if let Some(br) = params.get("branch").and_then(|v| v.as_str()) {
                    url.push_str(&format!("&sha={}", br));
                }
                gh_get(&client, &url, &token).await
            }
            "get_commit" => {
                let sha = params.get("sha").and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Tool("'sha' is required for get_commit".into()))?;
                gh_get(&client, &format!("{}/repos/{}/{}/commits/{}", base, owner, repo, sha), &token).await
            }
            "list_tags" => {
                gh_get(&client, &format!("{}/repos/{}/{}/tags?per_page={}&page={}", base, owner, repo, per_page, page), &token).await
            }
            "repo_stats" => {
                // Get contributors stats (includes commit counts)
                let contributors = gh_get(&client, &format!("{}/repos/{}/{}/contributors?per_page={}", base, owner, repo, per_page), &token).await.unwrap_or(json!([]));
                let languages = gh_get(&client, &format!("{}/repos/{}/{}/languages", base, owner, repo), &token).await.unwrap_or(json!({}));
                let repo_info = gh_get(&client, &format!("{}/repos/{}/{}", base, owner, repo), &token).await.unwrap_or(json!({}));
                Ok(json!({
                    "stars": repo_info.get("stargazers_count"),
                    "forks": repo_info.get("forks_count"),
                    "watchers": repo_info.get("watchers_count"),
                    "open_issues": repo_info.get("open_issues_count"),
                    "size_kb": repo_info.get("size"),
                    "default_branch": repo_info.get("default_branch"),
                    "languages": languages,
                    "top_contributors": contributors
                }))
            }
            "list_webhooks" => {
                gh_get(&client, &format!("{}/repos/{}/{}/hooks?per_page={}", base, owner, repo, per_page), &token).await
            }
            "create_webhook" => {
                let url = params.get("webhook_url").and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Tool("'webhook_url' is required".into()))?;
                let events = params.get("webhook_events").and_then(|v| v.as_array())
                    .map(|a| a.clone())
                    .unwrap_or_else(|| vec![json!("push")]);
                let payload = json!({
                    "config": {
                        "url": url,
                        "content_type": "json"
                    },
                    "events": events,
                    "active": true
                });
                gh_post(&client, &format!("{}/repos/{}/{}/hooks", base, owner, repo), &token, &payload).await
            }
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

fn resolve_token(ctx: &ToolContext, params: &Value) -> String {
    params.get("api_token").and_then(|v| v.as_str()).map(String::from)
        .or_else(|| ctx.config.providers.get("github").map(|p| p.api_key.clone()))
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .or_else(|| std::env::var("GH_TOKEN").ok())
        .unwrap_or_default()
}

async fn gh_get(client: &Client, url: &str, token: &str) -> Result<Value> {
    debug!(url = %url, "GitHub GET");
    let mut req = client.get(url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "blockcell-agent")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if !token.is_empty() {
        req = req.header("Authorization", format!("Bearer {}", token));
    }
    let resp = req.send().await
        .map_err(|e| Error::Tool(format!("GitHub API request failed: {}", e)))?;
    parse_response(resp).await
}

async fn gh_post(client: &Client, url: &str, token: &str, body: &Value) -> Result<Value> {
    debug!(url = %url, "GitHub POST");
    let resp = client.post(url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "blockcell-agent")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("Authorization", format!("Bearer {}", token))
        .json(body)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("GitHub API request failed: {}", e)))?;
    parse_response(resp).await
}

async fn gh_put(client: &Client, url: &str, token: &str, body: &Value) -> Result<Value> {
    debug!(url = %url, "GitHub PUT");
    let resp = client.put(url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "blockcell-agent")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("Authorization", format!("Bearer {}", token))
        .json(body)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("GitHub API request failed: {}", e)))?;
    parse_response(resp).await
}

async fn gh_patch(client: &Client, url: &str, token: &str, body: &Value) -> Result<Value> {
    debug!(url = %url, "GitHub PATCH");
    let resp = client.patch(url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "blockcell-agent")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("Authorization", format!("Bearer {}", token))
        .json(body)
        .send()
        .await
        .map_err(|e| Error::Tool(format!("GitHub API request failed: {}", e)))?;
    parse_response(resp).await
}

async fn parse_response(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let body = resp.text().await
        .map_err(|e| Error::Tool(format!("Failed to read response: {}", e)))?;
    if !status.is_success() {
        let truncated = if body.len() > 500 { format!("{}...", crate::safe_truncate(&body, 500)) } else { body.clone() };
        return Err(Error::Tool(format!("GitHub API error ({}): {}", status, truncated)));
    }
    Ok(serde_json::from_str(&body).unwrap_or_else(|_| json!({"output": body})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = GitApiTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "git_api");
        assert!(schema.description.contains("GitHub"));
    }

    #[test]
    fn test_validate_valid() {
        let tool = GitApiTool;
        assert!(tool.validate(&json!({"action": "repo_info", "owner": "rust-lang", "repo": "rust"})).is_ok());
        assert!(tool.validate(&json!({"action": "repo_search", "query": "rust"})).is_ok());
        assert!(tool.validate(&json!({"action": "list_prs", "owner": "a", "repo": "b"})).is_ok());
    }

    #[test]
    fn test_validate_missing_owner() {
        let tool = GitApiTool;
        assert!(tool.validate(&json!({"action": "repo_info"})).is_err());
        assert!(tool.validate(&json!({"action": "list_issues", "owner": "a"})).is_err());
    }

    #[test]
    fn test_validate_invalid_action() {
        let tool = GitApiTool;
        assert!(tool.validate(&json!({"action": "invalid_action"})).is_err());
    }

    #[test]
    fn test_validate_search_no_repo() {
        let tool = GitApiTool;
        // repo_search and code_search don't strictly need owner/repo
        assert!(tool.validate(&json!({"action": "repo_search", "query": "test"})).is_ok());
    }
}
