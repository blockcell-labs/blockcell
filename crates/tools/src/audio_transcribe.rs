use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::{debug, info};

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for transcribing audio/video files to text.
///
/// Supports multiple backends:
/// - Local whisper CLI (openai-whisper or whisper.cpp)
/// - OpenAI Whisper API via http_request
/// - ffmpeg for audio extraction/conversion
pub struct AudioTranscribeTool;

#[async_trait]
impl Tool for AudioTranscribeTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "audio_transcribe",
            description: "Transcribe audio/video files to text. Actions: 'transcribe' converts speech to text, 'info' checks available backends, 'extract_audio' extracts audio track from video.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["transcribe", "info", "extract_audio"],
                        "description": "Action: 'transcribe' speech-to-text, 'info' check backends, 'extract_audio' extract audio from video"
                    },
                    "path": {
                        "type": "string",
                        "description": "Path to audio/video file (mp3, wav, m4a, flac, ogg, mp4, mkv, webm, etc.)"
                    },
                    "output_path": {
                        "type": "string",
                        "description": "Output path for transcription text or extracted audio. Default: auto-generated"
                    },
                    "language": {
                        "type": "string",
                        "description": "Language code (e.g. 'zh', 'en', 'ja'). Default: auto-detect"
                    },
                    "model": {
                        "type": "string",
                        "enum": ["tiny", "base", "small", "medium", "large"],
                        "description": "Whisper model size. Default: 'base'. Larger = more accurate but slower"
                    },
                    "backend": {
                        "type": "string",
                        "enum": ["auto", "whisper", "whisper_cpp", "api"],
                        "description": "Backend: 'auto' tries local first then API, 'whisper' uses openai-whisper, 'whisper_cpp' uses whisper.cpp, 'api' uses OpenAI API"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["txt", "srt", "vtt", "json"],
                        "description": "Output format for transcription. Default: 'txt'"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if !["transcribe", "info", "extract_audio"].contains(&action) {
            return Err(Error::Tool("action must be 'transcribe', 'info', or 'extract_audio'".into()));
        }
        if action == "transcribe" || action == "extract_audio" {
            if params.get("path").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                return Err(Error::Tool("'path' is required for transcribe/extract_audio".into()));
            }
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("info");

        match action {
            "transcribe" => {
                let path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let path = expand_path(path, &ctx);
                let language = params.get("language").and_then(|v| v.as_str());
                let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("base");
                let backend = params.get("backend").and_then(|v| v.as_str()).unwrap_or("auto");
                let format = params.get("format").and_then(|v| v.as_str()).unwrap_or("txt");
                let output_path = params.get("output_path")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                action_transcribe(&path, language, model, backend, format, output_path.as_deref(), &ctx).await
            }
            "extract_audio" => {
                let path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let path = expand_path(path, &ctx);
                let output_path = params.get("output_path")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                action_extract_audio(&path, output_path.as_deref(), &ctx).await
            }
            "info" => action_info().await,
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

fn expand_path(path: &str, ctx: &ToolContext) -> String {
    if path.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            return path.replacen('~', &home.to_string_lossy(), 1);
        }
    }
    if std::path::Path::new(path).is_absolute() {
        return path.to_string();
    }
    ctx.workspace.join(path).to_string_lossy().to_string()
}

/// Check available transcription backends.
async fn action_info() -> Result<Value> {
    let has_whisper = which::which("whisper").is_ok();
    let has_whisper_cpp = which::which("whisper-cpp").is_ok()
        || which::which("main").is_ok(); // whisper.cpp binary is often named 'main'
    let has_ffmpeg = which::which("ffmpeg").is_ok();

    let mut backends = Vec::new();
    if has_whisper {
        backends.push("whisper (openai-whisper)");
    }
    if has_whisper_cpp {
        backends.push("whisper_cpp (whisper.cpp)");
    }
    backends.push("api (OpenAI Whisper API via http_request)");

    let recommended = if has_whisper {
        "whisper"
    } else if has_whisper_cpp {
        "whisper_cpp"
    } else {
        "api"
    };

    // Check whisper version if available
    let mut whisper_version = None;
    if has_whisper {
        if let Ok(output) = tokio::process::Command::new("whisper")
            .arg("--help")
            .output()
            .await
        {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(line) = text.lines().next() {
                whisper_version = Some(line.trim().to_string());
            }
        }
    }

    let install_hint = if !has_whisper && !has_whisper_cpp {
        "Install whisper: pip install openai-whisper  OR  brew install whisper-cpp"
    } else {
        ""
    };

    Ok(json!({
        "available_backends": backends,
        "recommended": recommended,
        "has_whisper": has_whisper,
        "has_whisper_cpp": has_whisper_cpp,
        "has_ffmpeg": has_ffmpeg,
        "whisper_version": whisper_version,
        "supported_formats": ["mp3", "wav", "m4a", "flac", "ogg", "mp4", "mkv", "webm", "avi", "mov"],
        "output_formats": ["txt", "srt", "vtt", "json"],
        "install_hint": install_hint,
    }))
}

/// Transcribe audio/video to text.
async fn action_transcribe(
    path: &str,
    language: Option<&str>,
    model: &str,
    backend: &str,
    format: &str,
    output_path: Option<&str>,
    ctx: &ToolContext,
) -> Result<Value> {
    // Verify input file exists
    if !std::path::Path::new(path).exists() {
        return Err(Error::Tool(format!("File not found: {}", path)));
    }

    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    info!(path = %path, size = file_size, backend = %backend, model = %model, "🎤 Transcribing audio");

    // Determine which backend to use
    let result = match backend {
        "whisper" => try_whisper(path, language, model, format, output_path, ctx).await,
        "whisper_cpp" => try_whisper_cpp(path, language, model, format, output_path, ctx).await,
        "api" => try_api_transcribe(path, language, ctx).await,
        "auto" | _ => {
            // Try local backends first, then API
            if which::which("whisper").is_ok() {
                let r = try_whisper(path, language, model, format, output_path, ctx).await;
                if r.is_ok() { return r; }
                debug!("whisper failed, trying whisper_cpp");
            }
            if which::which("whisper-cpp").is_ok() || which::which("main").is_ok() {
                let r = try_whisper_cpp(path, language, model, format, output_path, ctx).await;
                if r.is_ok() { return r; }
                debug!("whisper_cpp failed, trying API");
            }
            // Fallback: try API
            try_api_transcribe(path, language, ctx).await
        }
    };

    result
}

/// Transcribe using openai-whisper CLI.
async fn try_whisper(
    path: &str,
    language: Option<&str>,
    model: &str,
    format: &str,
    output_path: Option<&str>,
    ctx: &ToolContext,
) -> Result<Value> {
    let output_dir = if let Some(op) = output_path {
        if let Some(parent) = std::path::Path::new(op).parent() {
            parent.to_string_lossy().to_string()
        } else {
            ctx.workspace.join("transcripts").to_string_lossy().to_string()
        }
    } else {
        let dir = ctx.workspace.join("transcripts");
        let _ = std::fs::create_dir_all(&dir);
        dir.to_string_lossy().to_string()
    };

    let mut cmd = tokio::process::Command::new("whisper");
    cmd.arg(path);
    cmd.args(["--model", model]);
    cmd.args(["--output_format", format]);
    cmd.args(["--output_dir", &output_dir]);

    if let Some(lang) = language {
        cmd.args(["--language", lang]);
    }

    // Verbose off for cleaner output
    cmd.arg("--verbose");
    cmd.arg("False");

    info!(model = %model, "🎤 Running whisper CLI");

    let output = cmd.output().await
        .map_err(|e| Error::Tool(format!("whisper command failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!("whisper failed: {}", truncate(&stderr, 500))));
    }

    // Find the output file
    let input_stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let ext = match format {
        "srt" => "srt",
        "vtt" => "vtt",
        "json" => "json",
        _ => "txt",
    };
    let result_file = format!("{}/{}.{}", output_dir, input_stem, ext);

    // Read the transcription
    let transcript = if std::path::Path::new(&result_file).exists() {
        std::fs::read_to_string(&result_file).unwrap_or_default()
    } else {
        // Try to get from stdout
        String::from_utf8_lossy(&output.stdout).to_string()
    };

    // If custom output_path, rename
    let final_path = if let Some(op) = output_path {
        if op != result_file {
            let _ = std::fs::rename(&result_file, op);
        }
        op.to_string()
    } else {
        result_file
    };

    info!(path = %final_path, chars = transcript.len(), "🎤 Transcription complete (whisper)");

    Ok(json!({
        "success": true,
        "backend": "whisper",
        "model": model,
        "output_path": final_path,
        "format": format,
        "text": truncate(&transcript, 5000),
        "text_length": transcript.len(),
    }))
}

/// Transcribe using whisper.cpp CLI.
async fn try_whisper_cpp(
    path: &str,
    language: Option<&str>,
    model: &str,
    format: &str,
    output_path: Option<&str>,
    ctx: &ToolContext,
) -> Result<Value> {
    // whisper.cpp needs WAV input — convert if needed
    let wav_path = ensure_wav(path, ctx).await?;

    // Find whisper.cpp binary
    let binary = if which::which("whisper-cpp").is_ok() {
        "whisper-cpp"
    } else if which::which("main").is_ok() {
        "main"
    } else {
        return Err(Error::Tool("whisper.cpp binary not found".into()));
    };

    // Model path — whisper.cpp expects model files in specific location
    let model_name = format!("ggml-{}.bin", model);
    let model_paths = vec![
        format!("/usr/local/share/whisper-cpp/models/{}", model_name),
        format!("{}/.local/share/whisper-cpp/models/{}", dirs::home_dir().unwrap_or_default().display(), model_name),
        format!("/opt/homebrew/share/whisper-cpp/models/{}", model_name),
    ];

    let model_path = model_paths.iter()
        .find(|p| std::path::Path::new(p).exists())
        .cloned()
        .unwrap_or_else(|| model_name.clone());

    let mut cmd = tokio::process::Command::new(binary);
    cmd.args(["-m", &model_path]);
    cmd.args(["-f", &wav_path]);

    if let Some(lang) = language {
        cmd.args(["-l", lang]);
    }

    match format {
        "srt" => { cmd.arg("--output-srt"); }
        "vtt" => { cmd.arg("--output-vtt"); }
        _ => { cmd.arg("--output-txt"); }
    }

    let output = cmd.output().await
        .map_err(|e| Error::Tool(format!("whisper.cpp failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!("whisper.cpp failed: {}", truncate(&stderr, 500))));
    }

    let transcript = String::from_utf8_lossy(&output.stdout).to_string();

    // Save to file
    let final_path = if let Some(op) = output_path {
        if let Some(parent) = std::path::Path::new(op).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(op, &transcript);
        op.to_string()
    } else {
        let dir = ctx.workspace.join("transcripts");
        let _ = std::fs::create_dir_all(&dir);
        let stem = std::path::Path::new(path).file_stem()
            .and_then(|s| s.to_str()).unwrap_or("output");
        let ext = match format { "srt" => "srt", "vtt" => "vtt", _ => "txt" };
        let out = dir.join(format!("{}.{}", stem, ext));
        let _ = std::fs::write(&out, &transcript);
        out.to_string_lossy().to_string()
    };

    // Clean up temp wav if we created one
    if wav_path != path {
        let _ = std::fs::remove_file(&wav_path);
    }

    info!(path = %final_path, chars = transcript.len(), "🎤 Transcription complete (whisper.cpp)");

    Ok(json!({
        "success": true,
        "backend": "whisper_cpp",
        "model": model,
        "output_path": final_path,
        "format": format,
        "text": truncate(&transcript, 5000),
        "text_length": transcript.len(),
    }))
}

/// Transcribe using OpenAI Whisper API.
async fn try_api_transcribe(
    path: &str,
    language: Option<&str>,
    ctx: &ToolContext,
) -> Result<Value> {
    // Get API key: try config providers (openai) first, then env var
    let api_key = ctx.config.providers.get("openai")
        .map(|p| p.api_key.clone())
        .filter(|k| !k.is_empty())
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .ok_or_else(|| Error::Tool(
            "OpenAI API key not found. Set OPENAI_API_KEY env var or configure openai provider in blockcell config. \
             Alternatively, install whisper locally: pip install openai-whisper".into()
        ))?;

    // Read file
    let file_bytes = std::fs::read(path)
        .map_err(|e| Error::Tool(format!("Failed to read audio file: {}", e)))?;

    let file_name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("audio.mp3");

    info!(path = %path, size = file_bytes.len(), "🎤 Calling OpenAI Whisper API");

    // Build multipart form
    let mut form = reqwest::multipart::Form::new()
        .text("model", "whisper-1")
        .text("response_format", "json");

    if let Some(lang) = language {
        form = form.text("language", lang.to_string());
    }

    let part = reqwest::multipart::Part::bytes(file_bytes)
        .file_name(file_name.to_string())
        .mime_str("application/octet-stream")
        .map_err(|e| Error::Tool(format!("Failed to create multipart: {}", e)))?;
    form = form.part("file", part);

    let client = reqwest::Client::new();
    let response: reqwest::Response = client
        .post("https://api.openai.com/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .timeout(std::time::Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| Error::Tool(format!("API request failed: {}", e)))?;

    let status = response.status();
    let body: String = response.text().await
        .map_err(|e| Error::Tool(format!("Failed to read API response: {}", e)))?;

    if !status.is_success() {
        return Err(Error::Tool(format!("OpenAI API error ({}): {}", status, truncate(&body, 500))));
    }

    let result: Value = serde_json::from_str(&body)
        .map_err(|e| Error::Tool(format!("Failed to parse API response: {}", e)))?;

    let transcript = result.get("text").and_then(|v| v.as_str()).unwrap_or("");

    // Save to file
    let dir = ctx.workspace.join("transcripts");
    let _ = std::fs::create_dir_all(&dir);
    let stem = std::path::Path::new(path).file_stem()
        .and_then(|s| s.to_str()).unwrap_or("output");
    let out_path = dir.join(format!("{}.txt", stem));
    let _ = std::fs::write(&out_path, transcript);

    info!(chars = transcript.len(), "🎤 Transcription complete (OpenAI API)");

    Ok(json!({
        "success": true,
        "backend": "openai_api",
        "model": "whisper-1",
        "output_path": out_path.to_string_lossy(),
        "format": "txt",
        "text": truncate(transcript, 5000),
        "text_length": transcript.len(),
    }))
}

/// Extract audio track from video file using ffmpeg.
async fn action_extract_audio(path: &str, output_path: Option<&str>, ctx: &ToolContext) -> Result<Value> {
    if !std::path::Path::new(path).exists() {
        return Err(Error::Tool(format!("File not found: {}", path)));
    }

    if which::which("ffmpeg").is_err() {
        return Err(Error::Tool("ffmpeg not installed. Install via: brew install ffmpeg".into()));
    }

    let out = if let Some(op) = output_path {
        op.to_string()
    } else {
        let dir = ctx.workspace.join("audio");
        let _ = std::fs::create_dir_all(&dir);
        let stem = std::path::Path::new(path).file_stem()
            .and_then(|s| s.to_str()).unwrap_or("output");
        dir.join(format!("{}.wav", stem)).to_string_lossy().to_string()
    };

    if let Some(parent) = std::path::Path::new(&out).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    info!(input = %path, output = %out, "🎤 Extracting audio");

    let output = tokio::process::Command::new("ffmpeg")
        .args(["-i", path, "-vn", "-acodec", "pcm_s16le", "-ar", "16000", "-ac", "1", "-y", &out])
        .output()
        .await
        .map_err(|e| Error::Tool(format!("ffmpeg failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!("ffmpeg extract failed: {}", truncate(&stderr, 500))));
    }

    let file_size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);

    Ok(json!({
        "success": true,
        "input": path,
        "output_path": out,
        "format": "wav",
        "sample_rate": 16000,
        "channels": 1,
        "file_size_bytes": file_size,
    }))
}

/// Convert audio to WAV format for whisper.cpp (which requires WAV input).
async fn ensure_wav(path: &str, ctx: &ToolContext) -> Result<String> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if ext == "wav" {
        return Ok(path.to_string());
    }

    if which::which("ffmpeg").is_err() {
        return Err(Error::Tool("ffmpeg needed to convert audio to WAV. Install: brew install ffmpeg".into()));
    }

    let tmp_dir = ctx.workspace.join("tmp");
    let _ = std::fs::create_dir_all(&tmp_dir);
    let stem = std::path::Path::new(path).file_stem()
        .and_then(|s| s.to_str()).unwrap_or("audio");
    let wav_path = tmp_dir.join(format!("{}_tmp.wav", stem));
    let wav_str = wav_path.to_string_lossy().to_string();

    let output = tokio::process::Command::new("ffmpeg")
        .args(["-i", path, "-ar", "16000", "-ac", "1", "-y", &wav_str])
        .output()
        .await
        .map_err(|e| Error::Tool(format!("ffmpeg conversion failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Tool(format!("Audio conversion failed: {}", truncate(&stderr, 300))));
    }

    Ok(wav_str)
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...(truncated)", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = AudioTranscribeTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "audio_transcribe");
    }

    #[test]
    fn test_validate_transcribe() {
        let tool = AudioTranscribeTool;
        assert!(tool.validate(&json!({"action": "transcribe", "path": "/tmp/test.mp3"})).is_ok());
        assert!(tool.validate(&json!({"action": "transcribe"})).is_err());
        assert!(tool.validate(&json!({"action": "info"})).is_ok());
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }

    #[test]
    fn test_validate_extract() {
        let tool = AudioTranscribeTool;
        assert!(tool.validate(&json!({"action": "extract_audio", "path": "/tmp/video.mp4"})).is_ok());
        assert!(tool.validate(&json!({"action": "extract_audio"})).is_err());
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello...(truncated)");
    }
}
