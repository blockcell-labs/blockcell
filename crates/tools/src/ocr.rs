use async_trait::async_trait;
use base64::Engine;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::info;

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for optical character recognition (OCR) — extracting text from images.
///
/// Supports multiple backends:
/// - Tesseract OCR (local, free, many languages)
/// - macOS Vision framework (via Python/objc bridge)
/// - OpenAI Vision API (cloud, multimodal LLM)
pub struct OcrTool;

#[async_trait]
impl Tool for OcrTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "ocr",
            description: "Extract text from images (OCR). Actions: 'recognize' extracts text from an image, 'info' checks available backends.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["recognize", "info"],
                        "description": "Action: 'recognize' to extract text, 'info' to check backends"
                    },
                    "path": {
                        "type": "string",
                        "description": "(recognize) Path to image file (jpg, png, tiff, bmp, webp, pdf)"
                    },
                    "language": {
                        "type": "string",
                        "description": "(recognize) Language hint for OCR. For tesseract: 'chi_sim' (Chinese simplified), 'eng' (English), 'jpn' (Japanese), etc. Multiple: 'chi_sim+eng'. Default: auto"
                    },
                    "backend": {
                        "type": "string",
                        "enum": ["auto", "tesseract", "vision", "api"],
                        "description": "Backend: 'auto' tries local first, 'tesseract' local Tesseract OCR, 'vision' macOS Vision framework, 'api' OpenAI Vision API"
                    },
                    "output_path": {
                        "type": "string",
                        "description": "(recognize) Save extracted text to file. Default: return text directly"
                    },
                    "dpi": {
                        "type": "integer",
                        "description": "(recognize) DPI for PDF rendering. Default: 300"
                    },
                    "psm": {
                        "type": "integer",
                        "description": "(recognize, tesseract) Page segmentation mode. 3=auto, 6=single block, 7=single line, 11=sparse text. Default: 3"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if !["recognize", "info"].contains(&action) {
            return Err(Error::Tool("action must be 'recognize' or 'info'".into()));
        }
        if action == "recognize"
            && params.get("path").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                return Err(Error::Tool("'path' is required for recognize".into()));
            }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("info");

        match action {
            "recognize" => action_recognize(&ctx, &params).await,
            "info" => action_info().await,
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

async fn action_info() -> Result<Value> {
    let tesseract = check_command("tesseract").await;
    let tesseract_langs = if tesseract {
        get_tesseract_languages().await.unwrap_or_default()
    } else {
        Vec::new()
    };

    Ok(json!({
        "backends": {
            "tesseract": {
                "available": tesseract,
                "description": "Tesseract OCR (free, offline, many languages)",
                "languages": tesseract_langs
            },
            "vision": {
                "available": cfg!(target_os = "macos"),
                "description": "macOS Vision framework (free, offline, good for CJK)"
            },
            "api": {
                "available": true,
                "description": "OpenAI Vision API (paid, highest accuracy, requires OPENAI_API_KEY)"
            }
        },
        "recommended": if tesseract { "tesseract" } else { "vision" }
    }))
}

async fn action_recognize(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let backend = params.get("backend").and_then(|v| v.as_str()).unwrap_or("auto");
    let language = params.get("language").and_then(|v| v.as_str());
    let output_path = params.get("output_path").and_then(|v| v.as_str());

    let resolved_path = resolve_path(path, &ctx.workspace);
    if !std::path::Path::new(&resolved_path).exists() {
        return Err(Error::Tool(format!("File not found: {}", resolved_path)));
    }

    let (text, used_backend) = match backend {
        "tesseract" => (ocr_tesseract(&resolved_path, language, params).await?, "tesseract"),
        "vision" => (ocr_vision(&resolved_path, language).await?, "vision"),
        "api" => (ocr_api(ctx, &resolved_path).await?, "api"),
        "auto" => {
            if check_command("tesseract").await {
                (ocr_tesseract(&resolved_path, language, params).await?, "tesseract")
            } else if cfg!(target_os = "macos") {
                match ocr_vision(&resolved_path, language).await {
                    Ok(t) => (t, "vision"),
                    Err(_) => {
                        if check_command("tesseract").await {
                            (ocr_tesseract(&resolved_path, language, params).await?, "tesseract")
                        } else {
                            (ocr_api(ctx, &resolved_path).await?, "api")
                        }
                    }
                }
            } else {
                (ocr_api(ctx, &resolved_path).await?, "api")
            }
        }
        _ => return Err(Error::Tool(format!("Unknown backend: {}", backend))),
    };

    // Save to file if requested
    if let Some(out) = output_path {
        let out_resolved = resolve_path(out, &ctx.workspace);
        if let Some(parent) = std::path::Path::new(&out_resolved).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&out_resolved, &text)
            .map_err(|e| Error::Tool(format!("Failed to write output: {}", e)))?;
    }

    info!(
        backend = %used_backend,
        text_length = text.len(),
        "OCR completed"
    );

    Ok(json!({
        "status": "ok",
        "backend": used_backend,
        "text": text,
        "text_length": text.len(),
        "source": resolved_path
    }))
}

/// Tesseract OCR
async fn ocr_tesseract(path: &str, language: Option<&str>, params: &Value) -> Result<String> {
    let mut cmd = tokio::process::Command::new("tesseract");
    cmd.arg(path);
    cmd.arg("stdout"); // Output to stdout

    if let Some(lang) = language {
        cmd.arg("-l").arg(lang);
    }

    let psm = params.get("psm").and_then(|v| v.as_i64()).unwrap_or(3);
    cmd.arg("--psm").arg(psm.to_string());

    let output = cmd.output().await
        .map_err(|e| Error::Tool(format!("tesseract failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!("tesseract error: {}", stderr)));
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(text)
}

/// macOS Vision framework OCR via Python
async fn ocr_vision(path: &str, language: Option<&str>) -> Result<String> {
    let lang_array = if let Some(lang) = language {
        let langs: Vec<&str> = lang.split('+').collect();
        let mapped: Vec<String> = langs.iter().map(|l| {
            match *l {
                "chi_sim" | "zh" => "zh-Hans".to_string(),
                "chi_tra" => "zh-Hant".to_string(),
                "eng" | "en" => "en-US".to_string(),
                "jpn" | "ja" => "ja-JP".to_string(),
                "kor" | "ko" => "ko-KR".to_string(),
                other => other.to_string(),
            }
        }).collect();
        format!("[{}]", mapped.iter().map(|l| format!("'{}'", l)).collect::<Vec<_>>().join(","))
    } else {
        "None".to_string()
    };

    let script = format!(r#"
import Vision
import Quartz
from Foundation import NSURL

path = '{path}'
url = NSURL.fileURLWithPath_(path)
langs = {lang_array}

# Load image
source = Quartz.CGImageSourceCreateWithURL(url, None)
if source is None:
    print("ERROR: Cannot load image")
    exit(1)
image = Quartz.CGImageSourceCreateImageAtIndex(source, 0, None)
if image is None:
    print("ERROR: Cannot create image")
    exit(1)

handler = Vision.VNImageRequestHandler.alloc().initWithCGImage_options_(image, None)
request = Vision.VNRecognizeTextRequest.alloc().init()
request.setRecognitionLevel_(1)  # accurate
if langs is not None:
    request.setRecognitionLanguages_(langs)
request.setUsesLanguageCorrection_(True)

handler.performRequests_error_([request], None)
results = request.results()
if results:
    for obs in results:
        candidates = obs.topCandidates_(1)
        if candidates:
            print(candidates[0].string())
"#, path = path.replace('\'', "\\'"), lang_array = lang_array);

    let output = tokio::process::Command::new("python3")
        .arg("-c")
        .arg(&script)
        .output()
        .await
        .map_err(|e| Error::Tool(format!("Vision OCR failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!("Vision OCR error: {}", stderr)));
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.starts_with("ERROR:") {
        return Err(Error::Tool(text));
    }
    Ok(text)
}

/// OpenAI Vision API OCR
async fn ocr_api(ctx: &ToolContext, path: &str) -> Result<String> {
    let api_key = resolve_api_key(ctx)?;

    // Read and encode image
    let bytes = std::fs::read(path)
        .map_err(|e| Error::Tool(format!("Failed to read image: {}", e)))?;

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
        "tiff" | "tif" => "image/tiff",
        "bmp" => "image/bmp",
        _ => "image/png",
    };

    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let data_url = format!("data:{};base64,{}", mime, b64);

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.openai.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "model": "gpt-4o-mini",
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": "Extract ALL text from this image. Return ONLY the extracted text, preserving the original layout and formatting as much as possible. Do not add any commentary or explanation."
                    },
                    {
                        "type": "image_url",
                        "image_url": { "url": data_url }
                    }
                ]
            }],
            "max_tokens": 4096
        }))
        .send()
        .await
        .map_err(|e| Error::Tool(format!("OpenAI Vision API failed: {}", e)))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Tool(format!("OpenAI Vision API error {}: {}", status, text)));
    }

    let data: Value = response.json().await
        .map_err(|e| Error::Tool(format!("Failed to parse response: {}", e)))?;

    let text = data["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();

    Ok(text)
}

fn resolve_api_key(ctx: &ToolContext) -> Result<String> {
    if let Some(provider) = ctx.config.providers.get("openai") {
        if !provider.api_key.is_empty() {
            return Ok(provider.api_key.clone());
        }
    }
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.is_empty() {
            return Ok(key);
        }
    }
    Err(Error::Tool("OpenAI API key not found. Set config providers.openai.api_key or OPENAI_API_KEY env var.".into()))
}

async fn get_tesseract_languages() -> Result<Vec<String>> {
    let output = tokio::process::Command::new("tesseract")
        .arg("--list-langs")
        .output()
        .await
        .map_err(|e| Error::Tool(format!("tesseract --list-langs failed: {}", e)))?;

    let text = String::from_utf8_lossy(&output.stdout);
    let langs: Vec<String> = text.lines()
        .skip(1) // Skip header line
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    Ok(langs)
}

async fn check_command(cmd: &str) -> bool {
    tokio::process::Command::new("which")
        .arg(cmd)
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn resolve_path(path: &str, workspace: &std::path::Path) -> String {
    if path.starts_with('/') || path.starts_with('~') {
        if path.starts_with('~') {
            if let Some(home) = dirs::home_dir() {
                return home.join(&path[2..]).display().to_string();
            }
        }
        path.to_string()
    } else {
        workspace.join(path).display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_ocr_schema() {
        let tool = OcrTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "ocr");
    }

    #[test]
    fn test_ocr_validate_recognize() {
        let tool = OcrTool;
        assert!(tool.validate(&json!({"action": "recognize", "path": "/tmp/test.png"})).is_ok());
        assert!(tool.validate(&json!({"action": "recognize"})).is_err());
        assert!(tool.validate(&json!({"action": "recognize", "path": ""})).is_err());
    }

    #[test]
    fn test_ocr_validate_info() {
        let tool = OcrTool;
        assert!(tool.validate(&json!({"action": "info"})).is_ok());
    }

    #[test]
    fn test_ocr_validate_invalid() {
        let tool = OcrTool;
        assert!(tool.validate(&json!({"action": "bad"})).is_err());
    }

    #[test]
    fn test_resolve_path() {
        let ws = std::path::Path::new("/workspace");
        assert_eq!(resolve_path("/abs/path.png", ws), "/abs/path.png");
        assert_eq!(resolve_path("rel/path.png", ws), "/workspace/rel/path.png");
    }
}
