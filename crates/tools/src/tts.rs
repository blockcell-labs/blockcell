use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::info;

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for text-to-speech synthesis.
///
/// Supports multiple backends:
/// - macOS `say` command (local, free, multiple voices)
/// - piper TTS (local, neural voices)
/// - OpenAI TTS API (cloud, high quality)
/// - edge-tts (free, Microsoft Edge voices)
pub struct TtsTool;

#[async_trait]
impl Tool for TtsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "tts",
            description: "Convert text to speech audio. Actions: 'speak' generates audio file from text, 'list_voices' lists available voices, 'info' checks available backends.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["speak", "list_voices", "info"],
                        "description": "Action: 'speak' to generate audio, 'list_voices' to list voices, 'info' to check backends"
                    },
                    "text": {
                        "type": "string",
                        "description": "(speak) Text to convert to speech"
                    },
                    "output_path": {
                        "type": "string",
                        "description": "(speak) Output file path. Default: auto-generated in workspace/media/"
                    },
                    "voice": {
                        "type": "string",
                        "description": "(speak) Voice name. For 'say': Ting-Ting (Chinese), Samantha (English), etc. For 'api': alloy/echo/fable/onyx/nova/shimmer. For 'edge': zh-CN-XiaoxiaoNeural, en-US-JennyNeural, etc."
                    },
                    "backend": {
                        "type": "string",
                        "enum": ["auto", "say", "piper", "edge", "api"],
                        "description": "Backend: 'auto' tries local first, 'say' macOS built-in, 'piper' local neural TTS, 'edge' Microsoft Edge TTS (free), 'api' OpenAI TTS API"
                    },
                    "speed": {
                        "type": "number",
                        "description": "(speak) Speech speed multiplier. Default 1.0. Range 0.25-4.0"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["mp3", "wav", "aiff", "opus"],
                        "description": "(speak) Output format. Default: mp3 (api/edge) or aiff (say)"
                    },
                    "language": {
                        "type": "string",
                        "description": "(list_voices) Filter voices by language code, e.g. 'zh', 'en', 'ja'"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if !["speak", "list_voices", "info"].contains(&action) {
            return Err(Error::Tool("action must be 'speak', 'list_voices', or 'info'".into()));
        }
        if action == "speak"
            && params.get("text").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                return Err(Error::Tool("'text' is required for speak".into()));
            }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("info");

        match action {
            "speak" => action_speak(&ctx, &params).await,
            "list_voices" => action_list_voices(&params).await,
            "info" => action_info().await,
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

async fn action_info() -> Result<Value> {
    let say_available = check_command("say").await;
    let piper_available = check_command("piper").await;
    let edge_tts_available = check_command("edge-tts").await;
    let ffmpeg_available = check_command("ffmpeg").await;

    Ok(json!({
        "backends": {
            "say": { "available": say_available, "description": "macOS built-in TTS (free, offline)" },
            "piper": { "available": piper_available, "description": "Piper neural TTS (free, offline, high quality)" },
            "edge": { "available": edge_tts_available, "description": "Microsoft Edge TTS (free, online, many voices)" },
            "api": { "available": true, "description": "OpenAI TTS API (paid, highest quality, requires OPENAI_API_KEY)" }
        },
        "ffmpeg": ffmpeg_available,
        "recommended": if say_available { "say" } else if edge_tts_available { "edge" } else { "api" }
    }))
}

async fn action_list_voices(params: &Value) -> Result<Value> {
    let language = params.get("language").and_then(|v| v.as_str()).unwrap_or("");

    let mut voices = Vec::new();

    // macOS say voices
    if check_command("say").await {
        let output = tokio::process::Command::new("say")
            .arg("-v")
            .arg("?")
            .output()
            .await
            .map_err(|e| Error::Tool(format!("Failed to list say voices: {}", e)))?;

        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                let parts: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
                if parts.is_empty() { continue; }
                let name = parts[0].trim();
                let lang_part = if parts.len() > 1 {
                    parts[1].trim().split('#').next().unwrap_or("").trim()
                } else { "" };

                if !language.is_empty() && !lang_part.to_lowercase().contains(&language.to_lowercase()) {
                    continue;
                }
                voices.push(json!({
                    "name": name,
                    "backend": "say",
                    "language": lang_part
                }));
            }
        }
    }

    // Edge TTS voices
    if check_command("edge-tts").await {
        let output = tokio::process::Command::new("edge-tts")
            .arg("--list-voices")
            .output()
            .await
            .map_err(|e| Error::Tool(format!("Failed to list edge-tts voices: {}", e)))?;

        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                if !line.starts_with("Name:") { continue; }
                let name = line.trim_start_matches("Name:").trim();
                let lang = name.split('-').take(2).collect::<Vec<_>>().join("-");

                if !language.is_empty() && !lang.to_lowercase().contains(&language.to_lowercase()) {
                    continue;
                }
                voices.push(json!({
                    "name": name,
                    "backend": "edge",
                    "language": lang
                }));
            }
        }
    }

    // OpenAI TTS voices (static list)
    let api_voices = ["alloy", "echo", "fable", "onyx", "nova", "shimmer"];
    for v in &api_voices {
        voices.push(json!({
            "name": v,
            "backend": "api",
            "language": "multilingual"
        }));
    }

    Ok(json!({
        "voices": voices,
        "count": voices.len()
    }))
}

async fn action_speak(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let backend = params.get("backend").and_then(|v| v.as_str()).unwrap_or("auto");
    let voice = params.get("voice").and_then(|v| v.as_str());
    let speed = params.get("speed").and_then(|v| v.as_f64()).unwrap_or(1.0);
    let format = params.get("format").and_then(|v| v.as_str());

    let output_path = if let Some(p) = params.get("output_path").and_then(|v| v.as_str()) {
        std::path::PathBuf::from(resolve_path(p, &ctx.workspace))
    } else {
        let media_dir = ctx.workspace.join("media");
        std::fs::create_dir_all(&media_dir).ok();
        let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let ext = format.unwrap_or("mp3");
        media_dir.join(format!("tts_{}.{}", ts, ext))
    };

    let result = match backend {
        "say" => speak_say(text, voice, speed, &output_path).await,
        "piper" => speak_piper(text, voice, &output_path).await,
        "edge" => speak_edge(text, voice, speed, &output_path).await,
        "api" => speak_api(ctx, text, voice, speed, format, &output_path).await,
        "auto" => {
            // Try local backends first, then cloud.
            // On failure, fall back to the next backend (ignoring voice param
            // since voice names are backend-specific).
            let mut last_err = None;
            let backends: Vec<(&str, bool)> = vec![
                ("say", check_command("say").await),
                ("edge", check_command("edge-tts").await),
                ("piper", check_command("piper").await),
                ("api", true),
            ];
            let mut success: Option<String> = None;
            for (name, available) in &backends {
                if !available { continue; }
                let is_first_try = last_err.is_none();
                // On first try, use the user-specified voice; on fallback, use default (None)
                let try_voice = if is_first_try { voice } else { None };
                let res = match *name {
                    "say" => speak_say(text, try_voice, speed, &output_path).await,
                    "edge" => speak_edge(text, try_voice, speed, &output_path).await,
                    "piper" => speak_piper(text, try_voice, &output_path).await,
                    "api" => speak_api(ctx, text, try_voice, speed, format, &output_path).await,
                    _ => unreachable!(),
                };
                match res {
                    Ok(backend) => { success = Some(backend); break; }
                    Err(e) => {
                        info!(backend = *name, err = %format!("{}", e), "TTS backend failed, trying next");
                        last_err = Some(e);
                    }
                }
            }
            match success {
                Some(backend) => Ok(backend),
                None => Err(last_err.unwrap_or_else(|| Error::Tool("No TTS backend available".into()))),
            }
        }
        _ => Err(Error::Tool(format!("Unknown backend: {}", backend))),
    };

    match result {
        Ok(used_backend) => {
            let file_size = std::fs::metadata(&output_path).map(|m| m.len()).unwrap_or(0);
            info!(
                backend = %used_backend,
                output = %output_path.display(),
                size = file_size,
                "TTS completed"
            );
            Ok(json!({
                "status": "ok",
                "output_path": output_path.display().to_string(),
                "backend": used_backend,
                "file_size": file_size,
                "text_length": text.len()
            }))
        }
        Err(e) => Err(e),
    }
}

/// macOS `say` command
async fn speak_say(
    text: &str,
    voice: Option<&str>,
    speed: f64,
    output_path: &std::path::Path,
) -> Result<String> {
    let mut cmd = tokio::process::Command::new("say");

    if let Some(v) = voice {
        cmd.arg("-v").arg(v);
    }

    // say uses words per minute, default ~175
    let rate = (175.0 * speed) as u32;
    cmd.arg("-r").arg(rate.to_string());

    // say outputs AIFF natively; convert to desired format
    let ext = output_path.extension().and_then(|e| e.to_str()).unwrap_or("aiff");
    if ext == "aiff" || ext == "aif" {
        cmd.arg("-o").arg(output_path);
    } else {
        // Output to temp AIFF, then convert with ffmpeg
        let temp_aiff = output_path.with_extension("aiff");
        cmd.arg("-o").arg(&temp_aiff);
        cmd.arg(text);

        let output = cmd.output().await
            .map_err(|e| Error::Tool(format!("say failed: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Tool(format!("say error: {}", stderr)));
        }

        // Convert with ffmpeg
        let ffmpeg = tokio::process::Command::new("ffmpeg")
            .args(["-y", "-i"])
            .arg(&temp_aiff)
            .arg(output_path)
            .output()
            .await
            .map_err(|e| Error::Tool(format!("ffmpeg conversion failed: {}", e)))?;

        let _ = std::fs::remove_file(&temp_aiff);

        if !ffmpeg.status.success() {
            return Err(Error::Tool("ffmpeg conversion failed".into()));
        }
        return Ok("say".to_string());
    }

    cmd.arg(text);
    let output = cmd.output().await
        .map_err(|e| Error::Tool(format!("say failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!("say error: {}", stderr)));
    }

    Ok("say".to_string())
}

/// Piper TTS (local neural voices)
async fn speak_piper(
    text: &str,
    voice: Option<&str>,
    output_path: &std::path::Path,
) -> Result<String> {
    let mut cmd = tokio::process::Command::new("piper");

    if let Some(v) = voice {
        cmd.arg("--model").arg(v);
    }

    cmd.arg("--output_file").arg(output_path);

    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::Tool(format!("piper failed to start: {}", e)))?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(text.as_bytes()).await
            .map_err(|e| Error::Tool(format!("Failed to write to piper stdin: {}", e)))?;
    }

    let output = child.wait_with_output().await
        .map_err(|e| Error::Tool(format!("piper failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!("piper error: {}", stderr)));
    }

    Ok("piper".to_string())
}

/// Microsoft Edge TTS (free, online)
async fn speak_edge(
    text: &str,
    voice: Option<&str>,
    speed: f64,
    output_path: &std::path::Path,
) -> Result<String> {
    let mut cmd = tokio::process::Command::new("edge-tts");

    let default_voice = if text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
        "zh-CN-XiaoxiaoNeural"
    } else {
        "en-US-JennyNeural"
    };
    let voice = voice.unwrap_or(default_voice);
    cmd.arg("--voice").arg(voice);

    // edge-tts uses percentage for rate: +0% is normal, +50% is 1.5x
    let rate_pct = ((speed - 1.0) * 100.0) as i32;
    let rate_str = if rate_pct >= 0 {
        format!("+{}%", rate_pct)
    } else {
        format!("{}%", rate_pct)
    };
    cmd.arg("--rate").arg(&rate_str);

    cmd.arg("--text").arg(text);
    cmd.arg("--write-media").arg(output_path);

    let output = cmd.output().await
        .map_err(|e| Error::Tool(format!("edge-tts failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!("edge-tts error: {}", stderr)));
    }

    Ok("edge".to_string())
}

/// OpenAI TTS API
async fn speak_api(
    ctx: &ToolContext,
    text: &str,
    voice: Option<&str>,
    speed: f64,
    format: Option<&str>,
    output_path: &std::path::Path,
) -> Result<String> {
    let api_key = resolve_api_key(ctx)?;
    let voice = voice.unwrap_or("alloy");
    let format = format.unwrap_or("mp3");
    let model = "tts-1";

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.openai.com/v1/audio/speech")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&json!({
            "model": model,
            "input": text,
            "voice": voice,
            "speed": speed,
            "response_format": format
        }))
        .send()
        .await
        .map_err(|e| Error::Tool(format!("OpenAI TTS API request failed: {}", e)))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(Error::Tool(format!("OpenAI TTS API error {}: {}", status, text)));
    }

    let bytes = response.bytes().await
        .map_err(|e| Error::Tool(format!("Failed to read TTS response: {}", e)))?;

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(output_path, &bytes)
        .map_err(|e| Error::Tool(format!("Failed to write audio file: {}", e)))?;

    Ok("api".to_string())
}

fn resolve_api_key(ctx: &ToolContext) -> Result<String> {
    // Try config providers section
    if let Some(provider) = ctx.config.providers.get("openai") {
        if !provider.api_key.is_empty() {
            return Ok(provider.api_key.clone());
        }
    }
    // Try environment variable
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.is_empty() {
            return Ok(key);
        }
    }
    Err(Error::Tool("OpenAI API key not found. Set it in config providers.openai.api_key or OPENAI_API_KEY env var.".into()))
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
    fn test_tts_schema() {
        let tool = TtsTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "tts");
    }

    #[test]
    fn test_tts_validate_speak() {
        let tool = TtsTool;
        assert!(tool.validate(&json!({"action": "speak", "text": "hello"})).is_ok());
        assert!(tool.validate(&json!({"action": "speak"})).is_err());
        assert!(tool.validate(&json!({"action": "speak", "text": ""})).is_err());
    }

    #[test]
    fn test_tts_validate_info() {
        let tool = TtsTool;
        assert!(tool.validate(&json!({"action": "info"})).is_ok());
        assert!(tool.validate(&json!({"action": "list_voices"})).is_ok());
    }

    #[test]
    fn test_tts_validate_invalid() {
        let tool = TtsTool;
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }
}
