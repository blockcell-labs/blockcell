use super::*;
// ---------------------------------------------------------------------------
// Skills management — delete / hub proxy / install external
// ---------------------------------------------------------------------------

/// DELETE /v1/skills/:name — delete a user skill
pub(super) async fn handle_skill_delete(
    State(state): State<GatewayState>,
    AxumPath(skill_name): AxumPath<String>,
) -> impl IntoResponse {
    let skill_dir = state.paths.skills_dir().join(&skill_name);
    if !skill_dir.exists() {
        return Json(serde_json::json!({ "status": "not_found", "skill": skill_name }));
    }
    match std::fs::remove_dir_all(&skill_dir) {
        Ok(_) => Json(serde_json::json!({ "status": "deleted", "skill": skill_name })),
        Err(e) => Json(serde_json::json!({ "status": "error", "message": e.to_string() })),
    }
}

/// GET /v1/hub/skills — proxy community hub skills list
pub(super) async fn handle_hub_skills(State(state): State<GatewayState>) -> impl IntoResponse {
    let hub_url = match state.config.community_hub_url() {
        Some(u) => u,
        None => {
            return Json(
                serde_json::json!({ "error": "Community hub not configured", "skills": [] }),
            )
        }
    };
    let api_key = state.config.community_hub_api_key();
    let url = format!("{}/v1/skills/trending", hub_url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    let mut req = client.get(&url);
    if let Some(k) = &api_key {
        req = req.header("Authorization", format!("Bearer {}", k));
    }

    match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            let body = resp.text().await.unwrap_or_default();
            let val: serde_json::Value =
                serde_json::from_str(&body).unwrap_or(serde_json::json!({ "skills": [] }));
            Json(val)
        }
        Ok(resp) => {
            let status = resp.status().as_u16();
            Json(serde_json::json!({ "error": format!("Hub returned {}", status), "skills": [] }))
        }
        Err(e) => Json(serde_json::json!({ "error": e.to_string(), "skills": [] })),
    }
}

/// POST /v1/hub/skills/:name/install — install a skill from community hub
pub(super) async fn handle_hub_skill_install(
    State(state): State<GatewayState>,
    AxumPath(skill_name): AxumPath<String>,
) -> impl IntoResponse {
    let hub_url = match state.config.community_hub_url() {
        Some(u) => u,
        None => {
            return Json(
                serde_json::json!({ "status": "error", "message": "Community hub not configured" }),
            )
        }
    };
    let api_key = state.config.community_hub_api_key();
    let skills_dir = state.paths.skills_dir();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap_or_default();

    // Fetch skill metadata
    let info_url = format!(
        "{}/v1/skills/{}/latest",
        hub_url,
        urlencoding::encode(&skill_name)
    );
    let mut req = client.get(&info_url);
    if let Some(k) = &api_key {
        req = req.header("Authorization", format!("Bearer {}", k));
    }
    let info: serde_json::Value = match req.send().await {
        Ok(r) if r.status().is_success() => r.json().await.unwrap_or(serde_json::json!({})),
        _ => serde_json::json!({}),
    };

    // Resolve download URL
    let dist_url = info
        .get("dist_url")
        .and_then(|v| v.as_str())
        .or_else(|| info.get("source_url").and_then(|v| v.as_str()));
    let download_url = dist_url
        .map(|u| {
            if u.starts_with("http://") || u.starts_with("https://") {
                u.to_string()
            } else {
                format!(
                    "{}/{}",
                    hub_url.trim_end_matches('/'),
                    u.trim_start_matches('/')
                )
            }
        })
        .unwrap_or_else(|| {
            format!(
                "{}/v1/skills/{}/download",
                hub_url,
                urlencoding::encode(&skill_name)
            )
        });

    let mut dl_req = client.get(&download_url);
    if let Some(k) = &api_key {
        dl_req = dl_req.header("Authorization", format!("Bearer {}", k));
    }

    let resp = match dl_req.send().await {
        Ok(r) => r,
        Err(e) => return Json(serde_json::json!({ "status": "error", "message": e.to_string() })),
    };

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        return Json(
            serde_json::json!({ "status": "error", "message": format!("Download failed: HTTP {}", status) }),
        );
    }

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => return Json(serde_json::json!({ "status": "error", "message": e.to_string() })),
    };

    let skill_dir = skills_dir.join(&skill_name);
    if skill_dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&skill_dir) {
            return Json(serde_json::json!({ "status": "error", "message": e.to_string() }));
        }
    }
    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        return Json(serde_json::json!({ "status": "error", "message": e.to_string() }));
    }

    let cursor = std::io::Cursor::new(&bytes);
    match zip::ZipArchive::new(cursor) {
        Ok(mut archive) => {
            for i in 0..archive.len() {
                if let Ok(mut file) = archive.by_index(i) {
                    let out_path = if let Some(enclosed) = file.enclosed_name() {
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
                        if let Some(p) = out_path.parent() {
                            std::fs::create_dir_all(p).ok();
                        }
                        if let Ok(mut outfile) = std::fs::File::create(&out_path) {
                            std::io::copy(&mut file, &mut outfile).ok();
                        }
                    }
                }
            }
        }
        Err(_) => {
            // Not a zip — write as-is (e.g. tar.gz or raw file); for now just write raw bytes
            if let Err(e) = std::fs::write(skill_dir.join("raw.bin"), &bytes) {
                return Json(serde_json::json!({ "status": "error", "message": e.to_string() }));
            }
        }
    }

    Json(serde_json::json!({
        "status": "installed",
        "skill": skill_name,
        "size_bytes": bytes.len(),
    }))
}

/// POST /v1/skills/install-external — install OpenClaw-compatible external skill
#[derive(Deserialize)]
pub(super) struct InstallExternalRequest {
    url: String,
}

/// Represents a downloaded file (name + text content).
pub(super) struct DownloadedFile {
    name: String,
    content: String,
}

const EXTERNAL_MAX_DOWNLOAD_BYTES: usize = 5 * 1024 * 1024; // 5MB
const EXTERNAL_MAX_FILES: usize = 200;
const EXTERNAL_MAX_GITHUB_DEPTH: usize = 6;

fn is_blocked_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.octets()[0] == 0
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
        }
    }
}

async fn validate_external_url(url: &reqwest::Url) -> Result<(), String> {
    match url.scheme() {
        "http" | "https" => {}
        s => return Err(format!("Unsupported URL scheme: {}", s)),
    }

    let host = url.host_str().ok_or("URL host is required")?.to_lowercase();
    if host == "localhost" || host.ends_with(".localhost") {
        return Err("Blocked host: localhost".to_string());
    }
    if host.ends_with(".local") {
        return Err("Blocked host: .local".to_string());
    }

    // If it's already an IP literal, validate directly.
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if is_blocked_ip(ip) {
            return Err(format!("Blocked IP: {}", ip));
        }
        return Ok(());
    }

    let port = url.port_or_known_default().unwrap_or(443);
    let addrs = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| format!("DNS lookup failed: {}", e))?;
    for addr in addrs {
        if is_blocked_ip(addr.ip()) {
            return Err(format!("Blocked resolved IP: {}", addr.ip()));
        }
    }
    Ok(())
}

fn sanitize_skill_name(raw: &str) -> Result<String, String> {
    let mut out = String::new();
    for ch in raw.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if matches!(c, ' ' | '-' | '.' | '_') && !out.ends_with('_') {
                out.push('_');
            }
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        return Err("Invalid skill name (empty after sanitization)".to_string());
    }
    if out.len() > 64 {
        return Err("Invalid skill name (too long)".to_string());
    }
    if out.contains("__") {
        // Not a security issue, but avoid pathological names.
        // Keep as-is; consumers may rely on underscores.
    }
    Ok(out)
}

fn normalize_relative_path(rel: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(rel);
    let mut clean = std::path::PathBuf::new();
    for comp in p.components() {
        match comp {
            std::path::Component::Normal(s) => clean.push(s),
            std::path::Component::CurDir => {}
            // Block absolute paths and any parent traversal.
            std::path::Component::RootDir
            | std::path::Component::Prefix(_)
            | std::path::Component::ParentDir => return None,
        }
    }
    if clean.as_os_str().is_empty() {
        None
    } else {
        Some(clean)
    }
}

fn ensure_within_dir(root: &std::path::Path, path: &std::path::Path) -> bool {
    if let (Ok(r), Ok(p)) = (root.canonicalize(), path.canonicalize()) {
        return p.starts_with(r);
    }
    // If canonicalize fails (e.g. path doesn't exist yet), fall back to lexical check.
    path.starts_with(root)
}

/// Convert a GitHub HTML URL to the GitHub API tree URL for directory listing.
/// e.g. https://github.com/openclaw/skills/tree/main/skills/foo/bar
///   -> https://api.github.com/repos/openclaw/skills/contents/skills/foo/bar?ref=main
fn github_html_to_api_url(url: &str) -> Option<String> {
    // Match: github.com/{owner}/{repo}/tree/{branch}/{path}
    let stripped = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    if !stripped.starts_with("github.com/") {
        return None;
    }
    let parts: Vec<&str> = stripped
        .trim_start_matches("github.com/")
        .splitn(5, '/')
        .collect();
    if parts.len() < 4 || parts[2] != "tree" {
        return None;
    }
    let owner = parts[0];
    let repo = parts[1];
    let branch = parts[3];
    let path = if parts.len() == 5 { parts[4] } else { "" };
    Some(format!(
        "https://api.github.com/repos/{}/{}/contents/{}?ref={}",
        owner, repo, path, branch
    ))
}

/// Convert a GitHub blob URL to the raw content URL.
/// e.g. https://github.com/openclaw/skills/blob/main/skills/foo/SKILL.md
///   -> https://raw.githubusercontent.com/openclaw/skills/main/skills/foo/SKILL.md
fn github_blob_to_raw_url(url: &str) -> Option<String> {
    let stripped = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    if !stripped.starts_with("github.com/") {
        return None;
    }
    let rest = stripped.trim_start_matches("github.com/");
    let parts: Vec<&str> = rest.splitn(5, '/').collect();
    if parts.len() < 5 || parts[2] != "blob" {
        return None;
    }
    let owner = parts[0];
    let repo = parts[1];
    let branch = parts[3];
    let path = parts[4];
    Some(format!(
        "https://raw.githubusercontent.com/{}/{}/{}/{}",
        owner, repo, branch, path
    ))
}

/// Extract skill name and description from OpenClaw SKILL.md YAML frontmatter.
/// Returns (name, description).
fn parse_openclaw_frontmatter(content: &str) -> (Option<String>, Option<String>) {
    if !content.starts_with("---") {
        return (None, None);
    }
    let after_open = &content[3..];
    let end = after_open.find("\n---").unwrap_or(0);
    if end == 0 {
        return (None, None);
    }
    let frontmatter = &after_open[..end];
    let mut name: Option<String> = None;
    let mut desc: Option<String> = None;
    let mut in_desc_block = false;
    let mut desc_lines: Vec<String> = Vec::new();

    for line in frontmatter.lines() {
        if in_desc_block {
            if line.starts_with("  ") || line.starts_with('\t') {
                desc_lines.push(line.trim().to_string());
                continue;
            } else {
                in_desc_block = false;
                if !desc_lines.is_empty() {
                    desc = Some(desc_lines.join(" "));
                }
            }
        }
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(v.trim().trim_matches('"').trim_matches('\'').to_string());
        } else if let Some(v) = line.strip_prefix("description:") {
            let trimmed = v.trim();
            if trimmed == "|" || trimmed == ">" {
                in_desc_block = true;
                desc_lines.clear();
            } else if !trimmed.is_empty() {
                desc = Some(trimmed.trim_matches('"').trim_matches('\'').to_string());
            }
        }
    }
    if in_desc_block && !desc_lines.is_empty() {
        desc = Some(desc_lines.join(" "));
    }
    (name, desc)
}

/// Download text files from a GitHub directory via the GitHub Contents API.
/// Traverses subdirectories up to a fixed depth (iterative, avoids async recursion).
async fn fetch_github_directory_recursive(
    client: &reqwest::Client,
    api_url: &str,
    root_prefix: &str,
    depth: usize,
    remaining_files: &mut usize,
    remaining_bytes: &mut usize,
) -> Result<Vec<DownloadedFile>, String> {
    let mut result: Vec<DownloadedFile> = Vec::new();
    let mut stack: Vec<(String, usize)> = vec![(api_url.to_string(), depth)];

    while let Some((url, d)) = stack.pop() {
        if d > EXTERNAL_MAX_GITHUB_DEPTH {
            continue;
        }
        if *remaining_files == 0 {
            return Err(format!(
                "Too many files in GitHub directory (max {})",
                EXTERNAL_MAX_FILES
            ));
        }
        if *remaining_bytes == 0 {
            return Err(format!(
                "Downloaded content too large (max {} bytes)",
                EXTERNAL_MAX_DOWNLOAD_BYTES
            ));
        }

        let resp = client
            .get(&url)
            .header("User-Agent", "blockcell-agent/1.0")
            .header("Accept", "application/vnd.github.v3+json")
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!(
                "GitHub API returned HTTP {}",
                resp.status().as_u16()
            ));
        }

        let entries: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse GitHub API response: {}", e))?;

        let files_array = entries
            .as_array()
            .ok_or("GitHub API returned non-array response")?;

        for entry in files_array {
            if *remaining_files == 0 {
                return Err(format!(
                    "Too many files in GitHub directory (max {})",
                    EXTERNAL_MAX_FILES
                ));
            }
            if *remaining_bytes == 0 {
                return Err(format!(
                    "Downloaded content too large (max {} bytes)",
                    EXTERNAL_MAX_DOWNLOAD_BYTES
                ));
            }

            let file_type = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let file_name = entry
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let download_url = entry
                .get("download_url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let entry_path = entry
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if file_type == "dir" {
                if let Some(next_url) = entry.get("url").and_then(|v| v.as_str()) {
                    stack.push((next_url.to_string(), d + 1));
                }
                continue;
            }

            if file_type != "file" || download_url.is_empty() {
                continue;
            }

            let ext = file_name.rsplit('.').next().unwrap_or("").to_lowercase();
            let is_text = matches!(
                ext.as_str(),
                "md" | "rhai"
                    | "yaml"
                    | "yml"
                    | "json"
                    | "toml"
                    | "sh"
                    | "py"
                    | "ts"
                    | "js"
                    | "txt"
            ) || file_name == "SKILL.md"
                || file_name == "SKILL.rhai"
                || file_name == "meta.yaml";

            if !is_text {
                continue;
            }

            let mut rel = file_name.clone();
            if !root_prefix.is_empty() {
                let prefix = format!("{}/", root_prefix.trim_end_matches('/'));
                if entry_path.starts_with(&prefix) {
                    rel = entry_path[prefix.len()..].to_string();
                }
            }
            let Some(rel_path) = normalize_relative_path(&rel) else {
                continue;
            };

            match client
                .get(&download_url)
                .header("User-Agent", "blockcell-agent/1.0")
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    if let Ok(text) = r.text().await {
                        if text.len() > *remaining_bytes {
                            return Err(format!(
                                "Downloaded content too large (max {} bytes)",
                                EXTERNAL_MAX_DOWNLOAD_BYTES
                            ));
                        }
                        *remaining_bytes = remaining_bytes.saturating_sub(text.len());
                        *remaining_files = remaining_files.saturating_sub(1);
                        result.push(DownloadedFile {
                            name: rel_path.to_string_lossy().to_string(),
                            content: text,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    Ok(result)
}

pub(super) async fn handle_skill_install_external(
    State(state): State<GatewayState>,
    Json(req): Json<InstallExternalRequest>,
) -> impl IntoResponse {
    let url = req.url.trim().to_string();
    if url.is_empty() {
        return Json(serde_json::json!({ "status": "error", "message": "url is required" }));
    }

    let parsed_url = match reqwest::Url::parse(&url) {
        Ok(u) => u,
        Err(e) => {
            return Json(serde_json::json!({
                "status": "error",
                "message": format!("Invalid URL: {}", e)
            }))
        }
    };
    if let Err(e) = validate_external_url(&parsed_url).await {
        return Json(serde_json::json!({ "status": "error", "message": e }));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap_or_default();

    // ── Step 1: Download skill files ────────────────────────────────────────

    let mut downloaded_files: Vec<DownloadedFile> = Vec::new();

    if url.ends_with(".zip") || url.contains(".zip?") {
        // zip bundle download
        let resp: reqwest::Response = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                return Json(
                    serde_json::json!({ "status": "error", "message": format!("Download failed: {}", e) }),
                )
            }
        };
        if !resp.status().is_success() {
            return Json(
                serde_json::json!({ "status": "error", "message": format!("HTTP {}", resp.status().as_u16()) }),
            );
        }
        if let Some(len) = resp.content_length() {
            if len as usize > EXTERNAL_MAX_DOWNLOAD_BYTES {
                return Json(serde_json::json!({
                    "status": "error",
                    "message": format!("ZIP too large ({} bytes, max {})", len, EXTERNAL_MAX_DOWNLOAD_BYTES)
                }));
            }
        }

        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return Json(serde_json::json!({ "status": "error", "message": e.to_string() }))
            }
        };
        if bytes.len() > EXTERNAL_MAX_DOWNLOAD_BYTES {
            return Json(serde_json::json!({
                "status": "error",
                "message": format!("ZIP too large ({} bytes, max {})", bytes.len(), EXTERNAL_MAX_DOWNLOAD_BYTES)
            }));
        }
        let cursor = std::io::Cursor::new(&bytes);
        if let Ok(mut archive) = zip::ZipArchive::new(cursor) {
            let mut files_left = EXTERNAL_MAX_FILES;
            let mut remaining_bytes = EXTERNAL_MAX_DOWNLOAD_BYTES;
            for i in 0..archive.len() {
                if files_left == 0 {
                    return Json(serde_json::json!({
                        "status": "error",
                        "message": format!("Too many files in ZIP (max {})", EXTERNAL_MAX_FILES)
                    }));
                }
                if let Ok(mut file) = archive.by_index(i) {
                    if file.is_dir() {
                        continue;
                    }

                    let raw_name = file.name();
                    // Skip common junk directories
                    if raw_name.starts_with("__MACOSX/") {
                        continue;
                    }
                    let Some(rel_path) = normalize_relative_path(raw_name) else {
                        continue;
                    };

                    let mut content = String::new();
                    use std::io::Read;
                    if file.read_to_string(&mut content).is_ok() {
                        if content.len() > remaining_bytes {
                            return Json(serde_json::json!({
                                "status": "error",
                                "message": format!("Downloaded content too large (max {} bytes)", EXTERNAL_MAX_DOWNLOAD_BYTES)
                            }));
                        }
                        remaining_bytes = remaining_bytes.saturating_sub(content.len());
                        files_left = files_left.saturating_sub(1);
                        downloaded_files.push(DownloadedFile {
                            name: rel_path.to_string_lossy().to_string(),
                            content,
                        });
                    }
                }
            }
        }
    } else if let Some(api_url) = github_html_to_api_url(&url) {
        // GitHub directory URL → use Contents API
        let root_prefix = url
            .split("/tree/")
            .nth(1)
            .and_then(|s| s.split_once('/').map(|x| x.1))
            .unwrap_or("")
            .trim_matches('/')
            .to_string();
        let mut remaining = EXTERNAL_MAX_FILES;
        let mut remaining_bytes = EXTERNAL_MAX_DOWNLOAD_BYTES;
        match fetch_github_directory_recursive(
            &client,
            &api_url,
            &root_prefix,
            0,
            &mut remaining,
            &mut remaining_bytes,
        )
        .await
        {
            Ok(files) => downloaded_files = files,
            Err(e) => return Json(serde_json::json!({ "status": "error", "message": e })),
        }
    } else {
        // Single file URL (blob or raw)
        let raw_url = if url.contains("github.com/") && url.contains("/blob/") {
            github_blob_to_raw_url(&url).unwrap_or_else(|| url.clone())
        } else {
            url.clone()
        };

        let raw_parsed = match reqwest::Url::parse(&raw_url) {
            Ok(u) => u,
            Err(e) => {
                return Json(serde_json::json!({
                    "status": "error",
                    "message": format!("Invalid URL: {}", e)
                }))
            }
        };
        if let Err(e) = validate_external_url(&raw_parsed).await {
            return Json(serde_json::json!({ "status": "error", "message": e }));
        }

        let resp: reqwest::Response = match client.get(&raw_url).send().await {
            Ok(r) => r,
            Err(e) => {
                return Json(
                    serde_json::json!({ "status": "error", "message": format!("Download failed: {}", e) }),
                )
            }
        };
        if !resp.status().is_success() {
            return Json(
                serde_json::json!({ "status": "error", "message": format!("HTTP {}", resp.status().as_u16()) }),
            );
        }

        if let Some(len) = resp.content_length() {
            if len as usize > EXTERNAL_MAX_DOWNLOAD_BYTES {
                return Json(serde_json::json!({
                    "status": "error",
                    "message": format!("File too large ({} bytes, max {})", len, EXTERNAL_MAX_DOWNLOAD_BYTES)
                }));
            }
        }
        let content = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                return Json(serde_json::json!({ "status": "error", "message": e.to_string() }))
            }
        };
        if content.len() > EXTERNAL_MAX_DOWNLOAD_BYTES {
            return Json(serde_json::json!({
                "status": "error",
                "message": format!("File too large ({} bytes, max {})", content.len(), EXTERNAL_MAX_DOWNLOAD_BYTES)
            }));
        }
        let fname = raw_url.rsplit('/').next().unwrap_or("SKILL.md").to_string();
        let rel =
            normalize_relative_path(&fname).unwrap_or_else(|| std::path::PathBuf::from("SKILL.md"));
        downloaded_files.push(DownloadedFile {
            name: rel.to_string_lossy().to_string(),
            content,
        });
    }

    if downloaded_files.is_empty() {
        return Json(
            serde_json::json!({ "status": "error", "message": "No skill files could be downloaded from the provided URL" }),
        );
    }

    // ── Step 2: Determine skill name ─────────────────────────────────────────

    // Try to parse from SKILL.md frontmatter first
    let skill_md_content = downloaded_files
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case("SKILL.md"))
        .map(|f| f.content.as_str())
        .unwrap_or("");

    let (fm_name, fm_description) = parse_openclaw_frontmatter(skill_md_content);

    // Derive a filesystem-safe skill name
    let raw_skill_name = fm_name.clone().unwrap_or_else(|| {
        // Fall back to last path segment from the URL
        url.trim_end_matches('/')
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or("external_skill")
            .trim_end_matches(".zip")
            .trim_end_matches(".md")
            .to_string()
    });
    let skill_name = match sanitize_skill_name(&raw_skill_name) {
        Ok(s) => s,
        Err(e) => {
            return Json(serde_json::json!({
                "status": "error",
                "message": format!("Invalid skill name: {}", e)
            }))
        }
    };

    let existing_dir = state.paths.skills_dir().join(&skill_name);
    if existing_dir.exists() {
        return Json(serde_json::json!({
            "status": "error",
            "message": format!("Skill '{}' already exists. Please rename it (e.g. change frontmatter name) before importing.", skill_name)
        }));
    }

    let staging_dir_existing = state.paths.import_staging_skills_dir().join(&skill_name);
    if staging_dir_existing.exists() {
        return Json(serde_json::json!({
            "status": "error",
            "message": format!("Skill '{}' is already staged for import. If it is still evolving, please wait for it to complete.", skill_name)
        }));
    }

    {
        let svc = state.evolution_service.lock().await;
        if let Ok(records) = svc.list_all_records() {
            for r in records {
                if r.skill_name != skill_name {
                    continue;
                }
                let status = r.status.normalize();
                let in_progress = matches!(
                    *status,
                    blockcell_skills::evolution::EvolutionStatus::Triggered
                        | blockcell_skills::evolution::EvolutionStatus::Generating
                        | blockcell_skills::evolution::EvolutionStatus::Generated
                        | blockcell_skills::evolution::EvolutionStatus::Auditing
                        | blockcell_skills::evolution::EvolutionStatus::AuditPassed
                        | blockcell_skills::evolution::EvolutionStatus::CompilePassed
                        | blockcell_skills::evolution::EvolutionStatus::Observing
                        | blockcell_skills::evolution::EvolutionStatus::RollingOut
                );
                if in_progress {
                    return Json(serde_json::json!({
                        "status": "error",
                        "message": format!("Skill '{}' has an in-progress evolution record ({}, {:?}). Please wait for it to complete or clean it up first.", skill_name, r.id, status),
                        "skill": skill_name,
                        "evolution_id": r.id,
                    }));
                }
            }
        }
    }

    // ── Step 3: Write files to skill staging directory ───────────────────────

    let skill_dir = state.paths.import_staging_skills_dir().join(&skill_name);
    if skill_dir.exists() {
        let staging_root = state.paths.import_staging_skills_dir();
        if ensure_within_dir(&staging_root, &skill_dir) {
            std::fs::remove_dir_all(&skill_dir).ok();
        } else {
            return Json(serde_json::json!({
                "status": "error",
                "message": "Refusing to delete directory outside staging root"
            }));
        }
    }
    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        return Json(
            serde_json::json!({ "status": "error", "message": format!("Cannot create skill dir: {}", e) }),
        );
    }

    let mut total_bytes = 0usize;
    for df in &downloaded_files {
        total_bytes += df.content.len();
        if total_bytes > EXTERNAL_MAX_DOWNLOAD_BYTES {
            return Json(serde_json::json!({
                "status": "error",
                "message": format!("Downloaded content too large (>{} bytes)", EXTERNAL_MAX_DOWNLOAD_BYTES)
            }));
        }
        let Some(rel) = normalize_relative_path(&df.name) else {
            continue;
        };
        let out_path = skill_dir.join(rel);
        if let Some(parent) = out_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(out_path, &df.content).ok();
    }

    // Generate meta.yaml so blockcell's SkillManager can recognize the skill
    // even before the evolution pipeline completes.
    if !skill_dir.join("meta.yaml").exists() {
        let display_name = fm_name.as_deref().unwrap_or(&skill_name);
        let desc = fm_description
            .as_deref()
            .unwrap_or("External skill (evolving)");
        let meta_content = format!(
            "name: {}\ndescription: {}\ntriggers:\n  - {}\npermissions: []\n",
            display_name, desc, skill_name
        );
        std::fs::write(skill_dir.join("meta.yaml"), &meta_content).ok();
    }

    // ── Step 4: Build evolution context and trigger the self-evolution pipeline

    // Collect all file contents into a single description block for the LLM
    let mut openclaw_content = String::new();
    openclaw_content.push_str(&format!("## OpenClaw Skill Source (from {})\n\n", url));
    for df in &downloaded_files {
        openclaw_content.push_str(&format!("### {}\n```\n{}\n```\n\n", df.name, df.content));
    }

    // Detect skill type from downloaded files
    let has_py = downloaded_files.iter().any(|f| f.name.ends_with(".py"));
    let has_rhai = downloaded_files.iter().any(|f| f.name.ends_with(".rhai"));
    let ext_skill_type = if has_rhai {
        blockcell_skills::SkillType::Rhai
    } else if has_py {
        blockcell_skills::SkillType::Python
    } else {
        blockcell_skills::SkillType::PromptOnly
    };

    let description = match ext_skill_type {
        blockcell_skills::SkillType::Python => format!(
            "Convert the following OpenClaw-compatible skill into a Blockcell SKILL.py script.\n\
            Skill name: {}\n\
            {}\n\
            \n\
            Generate a COMPLETE SKILL.py and a compatible meta.yaml.\n\
            Blockcell Python runtime contract:\n\
            - Script is executed as `python3 SKILL.py`\n\
            - User input is provided from stdin as plain text\n\
            - Additional JSON context is available in env `BLOCKCELL_SKILL_CONTEXT`\n\
            - Output final user-facing result to stdout\n\
            - Do NOT require command-line JSON arguments\n\
            \n\
            Reuse useful logic from legacy OpenClaw scripts (e.g. scripts/*.py),\n\
            but adapt the entrypoint and output format to Blockcell style.\n\
            \n\
            {}",
            fm_name.as_deref().unwrap_or(&skill_name),
            fm_description
                .as_deref()
                .map(|d| format!("Description: {}", d))
                .unwrap_or_default(),
            openclaw_content,
        ),
        blockcell_skills::SkillType::Rhai => format!(
            "Convert the following OpenClaw-compatible skill into a Blockcell SKILL.rhai script.\n\
            Skill name: {}\n\
            {}\n\
            \n\
            Generate a COMPLETE SKILL.rhai and a compatible meta.yaml.\n\
            Use Blockcell tool-call style and produce clear user-facing output.\n\
            \n\
            {}",
            fm_name.as_deref().unwrap_or(&skill_name),
            fm_description
                .as_deref()
                .map(|d| format!("Description: {}", d))
                .unwrap_or_default(),
            openclaw_content,
        ),
        blockcell_skills::SkillType::PromptOnly => format!(
            "Convert the following OpenClaw-compatible skill into a Blockcell SKILL.md document.\n\
            Skill name: {}\n\
            {}\n\
            \n\
            Generate an improved SKILL.md that describes how the AI agent should handle requests\n\
            for this skill, including: goal, tools to use, step-by-step scenarios, and fallback strategy.\n\
            Also generate meta.yaml with name/description/triggers/permissions fields.\n\
            Base the content on the OpenClaw SKILL.md instructions below.\n\
            \n\
            {}",
            fm_name.as_deref().unwrap_or(&skill_name),
            fm_description
                .as_deref()
                .map(|d| format!("Description: {}", d))
                .unwrap_or_default(),
            openclaw_content,
        ),
    };

    let context = blockcell_skills::EvolutionContext {
        skill_name: skill_name.clone(),
        current_version: "0.0.0".to_string(),
        trigger: blockcell_skills::TriggerReason::ManualRequest { description },
        error_stack: None,
        source_snippet: None,
        tool_schemas: vec![],
        timestamp: chrono::Utc::now().timestamp(),
        skill_type: ext_skill_type,
        staged: true,
        staging_skills_dir: Some(
            state
                .paths
                .import_staging_skills_dir()
                .to_string_lossy()
                .to_string(),
        ),
    };

    let evolution_id = {
        let svc = state.evolution_service.lock().await;
        match svc.trigger_external_evolution(context).await {
            Ok(id) => id,
            Err(e) => {
                return Json(serde_json::json!({
                    "status": "error",
                    "message": format!("Failed to queue evolution: {}", e)
                }))
            }
        }
    };

    tracing::info!(
        skill = %skill_name,
        evolution_id = %evolution_id,
        files = downloaded_files.len(),
        "External skill queued for self-evolution"
    );

    Json(serde_json::json!({
        "status": "evolving",
        "skill": skill_name,
        "evolution_id": evolution_id,
        "files_downloaded": downloaded_files.len(),
        "size_bytes": total_bytes,
        "message": "技能已进入自进化流程，系统将自动将其转换为 Blockcell 格式并部署"
    }))
}

// ---------------------------------------------------------------------------
