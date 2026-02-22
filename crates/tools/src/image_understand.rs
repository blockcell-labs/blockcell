use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::info;

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for multimodal image understanding via LLM vision APIs.
///
/// Sends images to vision-capable LLMs (GPT-4o, Claude, Gemini) for:
/// - Image description and captioning
/// - Object/scene recognition
/// - Text extraction with context
/// - Chart/diagram interpretation
/// - Comparison of multiple images
pub struct ImageUnderstandTool;

#[async_trait]
impl Tool for ImageUnderstandTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "image_understand",
            description: "Analyze images using multimodal LLM vision. Actions: 'analyze' sends image(s) to a vision LLM with a prompt, 'describe' generates a description/caption, 'compare' compares multiple images, 'extract' extracts structured data from image.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["analyze", "describe", "compare", "extract"],
                        "description": "Action: 'analyze' general analysis with custom prompt, 'describe' auto-caption, 'compare' compare images, 'extract' extract structured data"
                    },
                    "path": {
                        "type": "string",
                        "description": "Path to image file (jpg, png, gif, webp). For 'compare', use 'paths' instead."
                    },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "(compare) Array of image paths to compare"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "(analyze/extract) Custom prompt for the vision LLM. For 'extract', describe the data structure you want."
                    },
                    "provider": {
                        "type": "string",
                        "enum": ["auto", "openai", "anthropic", "gemini"],
                        "description": "Vision LLM provider. Default: 'auto' (uses first available). 'openai' uses GPT-4o, 'anthropic' uses Claude, 'gemini' uses Gemini."
                    },
                    "model": {
                        "type": "string",
                        "description": "Specific model name override. Default: provider's best vision model."
                    },
                    "detail": {
                        "type": "string",
                        "enum": ["low", "high", "auto"],
                        "description": "(openai) Image detail level. 'low' = faster/cheaper, 'high' = more detail. Default: 'auto'"
                    },
                    "max_tokens": {
                        "type": "integer",
                        "description": "Max response tokens. Default: 1024"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if !["analyze", "describe", "compare", "extract"].contains(&action) {
            return Err(Error::Tool("action must be 'analyze', 'describe', 'compare', or 'extract'".into()));
        }
        if action == "compare" {
            let paths = params.get("paths").and_then(|v| v.as_array());
            if paths.map(|a| a.len()).unwrap_or(0) < 2 {
                return Err(Error::Tool("'paths' array with at least 2 images is required for compare".into()));
            }
        } else if params.get("path").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
            return Err(Error::Tool("'path' is required".into()));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("describe");
        let provider = params.get("provider").and_then(|v| v.as_str()).unwrap_or("auto");
        let max_tokens = params.get("max_tokens").and_then(|v| v.as_u64()).unwrap_or(1024) as u32;

        // Collect image paths
        let image_paths: Vec<String> = if action == "compare" {
            params.get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| resolve_path(s, &ctx.workspace))).collect())
                .unwrap_or_default()
        } else {
            let p = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
            vec![resolve_path(p, &ctx.workspace)]
        };

        // Verify all images exist
        for p in &image_paths {
            if !std::path::Path::new(p).exists() {
                return Err(Error::Tool(format!("Image not found: {}", p)));
            }
        }

        // Build the prompt
        let system_prompt = match action {
            "describe" => "Describe this image in detail. Include: main subject, scene, colors, mood, notable elements. Be concise but thorough.".to_string(),
            "compare" => "Compare these images. Describe similarities and differences in detail.".to_string(),
            "extract" => {
                let user_prompt = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("Extract all structured data from this image.");
                format!("Extract structured data from this image. {}", user_prompt)
            }
            "analyze" => {
                params.get("prompt").and_then(|v| v.as_str()).unwrap_or("Analyze this image.").to_string()
            }
            _ => "Describe this image.".to_string(),
        };

        // Encode images
        let encoded_images: Vec<(String, String)> = image_paths.iter()
            .filter_map(|p| encode_image(p).ok())
            .collect();

        if encoded_images.is_empty() {
            return Err(Error::Tool("Failed to encode any images".into()));
        }

        // Select provider and call
        let (response_text, used_provider, used_model) = match provider {
            "openai" => call_openai(&ctx, &system_prompt, &encoded_images, &params, max_tokens).await?,
            "anthropic" => call_anthropic(&ctx, &system_prompt, &encoded_images, max_tokens).await?,
            "gemini" => call_gemini(&ctx, &system_prompt, &encoded_images, max_tokens).await?,
            _ => {
                // Try providers in order: openai → anthropic → gemini
                if has_provider_key(&ctx, "openai") {
                    call_openai(&ctx, &system_prompt, &encoded_images, &params, max_tokens).await?
                } else if has_provider_key(&ctx, "anthropic") {
                    call_anthropic(&ctx, &system_prompt, &encoded_images, max_tokens).await?
                } else if has_provider_key(&ctx, "gemini") {
                    call_gemini(&ctx, &system_prompt, &encoded_images, max_tokens).await?
                } else {
                    return Err(Error::Tool(
                        "No vision API key found. Set OPENAI_API_KEY, ANTHROPIC_API_KEY, or GEMINI_API_KEY.".into()
                    ));
                }
            }
        };

        info!(
            action = %action,
            provider = %used_provider,
            model = %used_model,
            images = image_paths.len(),
            response_len = response_text.len(),
            "Image understanding completed"
        );

        Ok(json!({
            "status": "ok",
            "action": action,
            "provider": used_provider,
            "model": used_model,
            "response": response_text,
            "images_analyzed": image_paths.len()
        }))
    }
}

fn encode_image(path: &str) -> Result<(String, String)> {
    use base64::Engine;
    let bytes = std::fs::read(path)
        .map_err(|e| Error::Tool(format!("Failed to read image {}: {}", path, e)))?;

    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    };

    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok((format!("data:{};base64,{}", mime, b64), mime.to_string()))
}

fn has_provider_key(ctx: &ToolContext, provider: &str) -> bool {
    // Check config
    if let Some(p) = ctx.config.providers.get(provider) {
        if !p.api_key.is_empty() { return true; }
    }
    // Check env
    let env_var = match provider {
        "openai" => "OPENAI_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        "gemini" => "GEMINI_API_KEY",
        _ => return false,
    };
    std::env::var(env_var).map(|k| !k.is_empty()).unwrap_or(false)
}

fn get_api_key(ctx: &ToolContext, provider: &str) -> Result<String> {
    if let Some(p) = ctx.config.providers.get(provider) {
        if !p.api_key.is_empty() { return Ok(p.api_key.clone()); }
    }
    let env_var = match provider {
        "openai" => "OPENAI_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        "gemini" => "GEMINI_API_KEY",
        _ => return Err(Error::Tool(format!("Unknown provider: {}", provider))),
    };
    std::env::var(env_var)
        .map_err(|_| Error::Tool(format!("{} not set", env_var)))
        .and_then(|k| if k.is_empty() { Err(Error::Tool(format!("{} is empty", env_var))) } else { Ok(k) })
}

/// OpenAI GPT-4o Vision
async fn call_openai(
    ctx: &ToolContext,
    prompt: &str,
    images: &[(String, String)],
    params: &Value,
    max_tokens: u32,
) -> Result<(String, String, String)> {
    let api_key = get_api_key(ctx, "openai")?;
    let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("gpt-4o-mini");
    let detail = params.get("detail").and_then(|v| v.as_str()).unwrap_or("auto");

    let mut content = Vec::new();
    content.push(json!({"type": "text", "text": prompt}));
    for (data_url, _mime) in images {
        content.push(json!({
            "type": "image_url",
            "image_url": { "url": data_url, "detail": detail }
        }));
    }

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.openai.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "model": model,
            "messages": [{"role": "user", "content": content}],
            "max_tokens": max_tokens
        }))
        .send()
        .await
        .map_err(|e| Error::Tool(format!("OpenAI request failed: {}", e)))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Tool(format!("OpenAI error {}: {}", status, text)));
    }

    let data: Value = response.json().await
        .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

    let text = data["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();

    Ok((text, "openai".to_string(), model.to_string()))
}

/// Anthropic Claude Vision
async fn call_anthropic(
    ctx: &ToolContext,
    prompt: &str,
    images: &[(String, String)],
    max_tokens: u32,
) -> Result<(String, String, String)> {
    let api_key = get_api_key(ctx, "anthropic")?;
    let model = "claude-sonnet-4-20250514";

    let mut content = Vec::new();
    for (data_url, mime) in images {
        // Anthropic uses separate media_type and data fields
        let b64_data = data_url.split(",").nth(1).unwrap_or("");
        content.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": mime,
                "data": b64_data
            }
        }));
    }
    content.push(json!({"type": "text", "text": prompt}));

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&json!({
            "model": model,
            "max_tokens": max_tokens,
            "messages": [{"role": "user", "content": content}]
        }))
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Anthropic request failed: {}", e)))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Tool(format!("Anthropic error {}: {}", status, text)));
    }

    let data: Value = response.json().await
        .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

    let text = data["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string();

    Ok((text, "anthropic".to_string(), model.to_string()))
}

/// Google Gemini Vision
async fn call_gemini(
    ctx: &ToolContext,
    prompt: &str,
    images: &[(String, String)],
    max_tokens: u32,
) -> Result<(String, String, String)> {
    let api_key = get_api_key(ctx, "gemini")?;
    let model = "gemini-2.0-flash";

    let mut parts = Vec::new();
    parts.push(json!({"text": prompt}));
    for (data_url, mime) in images {
        let b64_data = data_url.split(",").nth(1).unwrap_or("");
        parts.push(json!({
            "inline_data": {
                "mime_type": mime,
                "data": b64_data
            }
        }));
    }

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        model, api_key
    );

    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .json(&json!({
            "contents": [{"parts": parts}],
            "generationConfig": { "maxOutputTokens": max_tokens }
        }))
        .send()
        .await
        .map_err(|e| Error::Tool(format!("Gemini request failed: {}", e)))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Tool(format!("Gemini error {}: {}", status, text)));
    }

    let data: Value = response.json().await
        .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

    let text = data["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string();

    Ok((text, "gemini".to_string(), model.to_string()))
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
    fn test_image_understand_schema() {
        let tool = ImageUnderstandTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "image_understand");
    }

    #[test]
    fn test_validate_analyze() {
        let tool = ImageUnderstandTool;
        assert!(tool.validate(&json!({"action": "analyze", "path": "/tmp/img.png"})).is_ok());
        assert!(tool.validate(&json!({"action": "analyze"})).is_err());
    }

    #[test]
    fn test_validate_compare() {
        let tool = ImageUnderstandTool;
        assert!(tool.validate(&json!({"action": "compare", "paths": ["/a.png", "/b.png"]})).is_ok());
        assert!(tool.validate(&json!({"action": "compare", "paths": ["/a.png"]})).is_err());
        assert!(tool.validate(&json!({"action": "compare"})).is_err());
    }

    #[test]
    fn test_validate_describe() {
        let tool = ImageUnderstandTool;
        assert!(tool.validate(&json!({"action": "describe", "path": "/tmp/img.png"})).is_ok());
    }

    #[test]
    fn test_validate_invalid() {
        let tool = ImageUnderstandTool;
        assert!(tool.validate(&json!({"action": "bad"})).is_err());
    }
}
