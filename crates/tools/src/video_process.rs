use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// Video processing tool based on ffmpeg.
///
/// Capabilities:
/// - **clip**: Extract a segment from a video (start/end time)
/// - **merge**: Concatenate multiple video files
/// - **subtitle**: Burn SRT subtitles into video (hardcoded)
/// - **thumbnail**: Extract thumbnail frames at specified times
/// - **convert**: Format conversion (mp4, webm, avi, mov, mkv, gif)
/// - **extract_audio**: Extract audio track from video
/// - **resize**: Resize/scale video
/// - **info**: Get video metadata (duration, resolution, codec, bitrate)
/// - **compress**: Reduce file size with quality control
/// - **watermark**: Add image/text watermark
pub struct VideoProcessTool;

#[async_trait]
impl Tool for VideoProcessTool {
    fn schema(&self) -> ToolSchema {
        let str_prop = |desc: &str| -> Value { json!({"type": "string", "description": desc}) };
        let num_prop = |desc: &str| -> Value { json!({"type": "number", "description": desc}) };
        let int_prop = |desc: &str| -> Value { json!({"type": "integer", "description": desc}) };
        let arr_str_prop = |desc: &str| -> Value { json!({"type": "array", "items": {"type": "string"}, "description": desc}) };
        let arr_num_prop = |desc: &str| -> Value { json!({"type": "array", "items": {"type": "number"}, "description": desc}) };
        let bool_prop = |desc: &str| -> Value { json!({"type": "boolean", "description": desc}) };

        let mut props = serde_json::Map::new();
        props.insert("action".into(), str_prop("Action: clip|merge|subtitle|thumbnail|convert|extract_audio|resize|info|compress|watermark"));
        props.insert("input".into(), str_prop("Input video file path"));
        props.insert("inputs".into(), arr_str_prop("(merge) Multiple input file paths to concatenate"));
        props.insert("output".into(), str_prop("Output file path. Default: auto-generated in workspace/media/"));
        props.insert("start".into(), str_prop("(clip) Start time in HH:MM:SS or seconds format"));
        props.insert("end".into(), str_prop("(clip) End time in HH:MM:SS or seconds format"));
        props.insert("duration".into(), str_prop("(clip) Duration instead of end time"));
        props.insert("subtitle_file".into(), str_prop("(subtitle) Path to SRT/ASS subtitle file"));
        props.insert("subtitle_style".into(), str_prop("(subtitle) Style override: 'FontSize=24,PrimaryColour=&HFFFFFF&' etc."));
        props.insert("times".into(), arr_num_prop("(thumbnail) Timestamps in seconds to extract frames"));
        props.insert("interval".into(), num_prop("(thumbnail) Extract a frame every N seconds"));
        props.insert("format".into(), str_prop("(convert/thumbnail) Output format: mp4|webm|avi|mov|mkv|gif|mp3|wav|jpg|png"));
        props.insert("width".into(), int_prop("(resize) Target width in pixels (-1 for auto-scale)"));
        props.insert("height".into(), int_prop("(resize) Target height in pixels (-1 for auto-scale)"));
        props.insert("quality".into(), int_prop("(compress) Quality level 1-51 (lower=better, default: 23 for h264)"));
        props.insert("bitrate".into(), str_prop("(compress/convert) Target bitrate (e.g. '2M', '500k')"));
        props.insert("codec".into(), str_prop("Video codec: h264|h265|vp9|copy"));
        props.insert("audio_codec".into(), str_prop("Audio codec: aac|mp3|opus|copy|none"));
        props.insert("fps".into(), num_prop("Target frame rate"));
        props.insert("watermark_image".into(), str_prop("(watermark) Path to watermark image"));
        props.insert("watermark_text".into(), str_prop("(watermark) Text to overlay"));
        props.insert("watermark_position".into(), str_prop("(watermark) Position: top-left|top-right|bottom-left|bottom-right|center (default: bottom-right)"));
        props.insert("no_audio".into(), bool_prop("Strip audio track from output"));
        props.insert("extra_args".into(), str_prop("Additional ffmpeg arguments (advanced)"));

        ToolSchema {
            name: "video_process",
            description: "Process videos using ffmpeg: clip segments, merge files, burn subtitles (SRT→hardcoded), extract thumbnails, convert formats, extract audio, resize, compress, add watermarks, and get video info. Requires ffmpeg installed on the system.",
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid = ["clip", "merge", "subtitle", "thumbnail", "convert", "extract_audio", "resize", "info", "compress", "watermark"];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        match action {
            "merge" => {
                if params.get("inputs").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0) < 2 {
                    return Err(Error::Tool("'inputs' must contain at least 2 files for merge".into()));
                }
            }
            "subtitle" => {
                if params.get("subtitle_file").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'subtitle_file' is required for subtitle action".into()));
                }
            }
            _ => {
                if action != "info" || params.get("input").is_some() {
                    // Most actions need input
                    if params.get("input").and_then(|v| v.as_str()).unwrap_or("").is_empty() && action != "merge"
                        && (action != "thumbnail" || params.get("input").and_then(|v| v.as_str()).unwrap_or("").is_empty()) {
                            // Allow info without input (returns ffmpeg version)
                            if action != "info" {
                                return Err(Error::Tool("'input' file path is required".into()));
                            }
                        }
                }
            }
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        // Check ffmpeg availability
        let ffmpeg_check = tokio::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .await;
        if ffmpeg_check.is_err() {
            return Err(Error::Tool("ffmpeg is not installed or not in PATH. Install it with: brew install ffmpeg".into()));
        }

        let action = params["action"].as_str().unwrap_or("");
        match action {
            "info" => self.action_info(&ctx, &params).await,
            "clip" => self.action_clip(&ctx, &params).await,
            "merge" => self.action_merge(&ctx, &params).await,
            "subtitle" => self.action_subtitle(&ctx, &params).await,
            "thumbnail" => self.action_thumbnail(&ctx, &params).await,
            "convert" => self.action_convert(&ctx, &params).await,
            "extract_audio" => self.action_extract_audio(&ctx, &params).await,
            "resize" => self.action_resize(&ctx, &params).await,
            "compress" => self.action_compress(&ctx, &params).await,
            "watermark" => self.action_watermark(&ctx, &params).await,
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

impl VideoProcessTool {
    fn resolve_input(ctx: &ToolContext, params: &Value) -> String {
        let input = params.get("input").and_then(|v| v.as_str()).unwrap_or("");
        resolve_path(ctx, input)
    }

    fn resolve_output(ctx: &ToolContext, params: &Value, default_ext: &str) -> String {
        if let Some(out) = params.get("output").and_then(|v| v.as_str()) {
            if !out.is_empty() {
                return resolve_path(ctx, out);
            }
        }
        let media_dir = ctx.workspace.join("media");
        let _ = std::fs::create_dir_all(&media_dir);
        let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
        media_dir.join(format!("video_{}.{}", ts, default_ext)).to_string_lossy().to_string()
    }

    async fn run_ffmpeg(args: &[&str]) -> Result<(String, String)> {
        debug!(args = ?args, "Running ffmpeg");
        let output = tokio::process::Command::new("ffmpeg")
            .args(args)
            .output()
            .await
            .map_err(|e| Error::Tool(format!("Failed to run ffmpeg: {}", e)))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !output.status.success() {
            let err_msg = if stderr.len() > 1000 {
                format!("{}...", &stderr[stderr.len()-1000..])
            } else {
                stderr
            };
            return Err(Error::Tool(format!("ffmpeg failed: {}", err_msg)));
        }
        Ok((stdout, stderr))
    }

    async fn run_ffprobe(input: &str) -> Result<Value> {
        let output = tokio::process::Command::new("ffprobe")
            .args([
                "-v", "quiet",
                "-print_format", "json",
                "-show_format",
                "-show_streams",
                input,
            ])
            .output()
            .await
            .map_err(|e| Error::Tool(format!("Failed to run ffprobe: {}", e)))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(&stdout)
            .map_err(|e| Error::Tool(format!("Failed to parse ffprobe output: {}", e)))
    }

    async fn action_info(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let input = params.get("input").and_then(|v| v.as_str()).unwrap_or("");
        if input.is_empty() {
            // Return ffmpeg version info
            let output = tokio::process::Command::new("ffmpeg")
                .arg("-version")
                .output()
                .await
                .map_err(|e| Error::Tool(format!("Failed to run ffmpeg: {}", e)))?;
            let version = String::from_utf8_lossy(&output.stdout);
            return Ok(json!({"ffmpeg_version": version.lines().next().unwrap_or("unknown")}));
        }

        let input_path = resolve_path(ctx, input);
        let probe = Self::run_ffprobe(&input_path).await?;

        let mut result = json!({
            "file": input_path,
        });

        if let Some(format) = probe.get("format") {
            result["duration"] = format.get("duration").cloned().unwrap_or(json!(null));
            result["size_bytes"] = format.get("size").cloned().unwrap_or(json!(null));
            result["bit_rate"] = format.get("bit_rate").cloned().unwrap_or(json!(null));
            result["format_name"] = format.get("format_name").cloned().unwrap_or(json!(null));
        }

        if let Some(streams) = probe.get("streams").and_then(|v| v.as_array()) {
            for stream in streams {
                let codec_type = stream.get("codec_type").and_then(|v| v.as_str()).unwrap_or("");
                match codec_type {
                    "video" => {
                        result["video_codec"] = stream.get("codec_name").cloned().unwrap_or(json!(null));
                        result["width"] = stream.get("width").cloned().unwrap_or(json!(null));
                        result["height"] = stream.get("height").cloned().unwrap_or(json!(null));
                        result["fps"] = stream.get("r_frame_rate").cloned().unwrap_or(json!(null));
                    }
                    "audio" => {
                        result["audio_codec"] = stream.get("codec_name").cloned().unwrap_or(json!(null));
                        result["sample_rate"] = stream.get("sample_rate").cloned().unwrap_or(json!(null));
                        result["channels"] = stream.get("channels").cloned().unwrap_or(json!(null));
                    }
                    _ => {}
                }
            }
        }

        Ok(result)
    }

    async fn action_clip(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let input = Self::resolve_input(ctx, params);
        let output = Self::resolve_output(ctx, params, "mp4");
        let start = params.get("start").and_then(|v| v.as_str()).unwrap_or("0");
        let codec = params.get("codec").and_then(|v| v.as_str()).unwrap_or("copy");
        let audio_codec = params.get("audio_codec").and_then(|v| v.as_str()).unwrap_or("copy");

        let mut args: Vec<&str> = vec!["-y", "-i", &input, "-ss", start];

        let end_str;
        let dur_str;
        if let Some(end) = params.get("end").and_then(|v| v.as_str()) {
            end_str = end.to_string();
            args.extend_from_slice(&["-to", &end_str]);
        } else if let Some(dur) = params.get("duration").and_then(|v| v.as_str()) {
            dur_str = dur.to_string();
            args.extend_from_slice(&["-t", &dur_str]);
        }

        args.extend_from_slice(&["-c:v", codec, "-c:a", audio_codec]);
        if params.get("no_audio").and_then(|v| v.as_bool()).unwrap_or(false) {
            args.push("-an");
        }
        args.push(&output);

        Self::run_ffmpeg(&args).await?;
        Ok(json!({"output": output, "action": "clip"}))
    }

    async fn action_merge(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let inputs = params.get("inputs").and_then(|v| v.as_array())
            .ok_or_else(|| Error::Tool("'inputs' array is required".into()))?;
        let output = Self::resolve_output(ctx, params, "mp4");

        // Create concat file
        let concat_path = ctx.workspace.join("media").join("_concat_list.txt");
        let _ = std::fs::create_dir_all(ctx.workspace.join("media"));
        let mut concat_content = String::new();
        for input in inputs {
            if let Some(path) = input.as_str() {
                let resolved = resolve_path(ctx, path);
                concat_content.push_str(&format!("file '{}'\n", resolved));
            }
        }
        std::fs::write(&concat_path, &concat_content)
            .map_err(|e| Error::Tool(format!("Failed to write concat list: {}", e)))?;

        let concat_str = concat_path.to_string_lossy().to_string();
        let args = vec!["-y", "-f", "concat", "-safe", "0", "-i", &concat_str, "-c", "copy", &output];
        Self::run_ffmpeg(&args).await?;

        // Cleanup
        let _ = std::fs::remove_file(&concat_path);
        Ok(json!({"output": output, "action": "merge", "input_count": inputs.len()}))
    }

    async fn action_subtitle(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let input = Self::resolve_input(ctx, params);
        let output = Self::resolve_output(ctx, params, "mp4");
        let sub_file = params.get("subtitle_file").and_then(|v| v.as_str()).unwrap_or("");
        let sub_path = resolve_path(ctx, sub_file);

        let style = params.get("subtitle_style").and_then(|v| v.as_str()).unwrap_or("");
        let filter = if style.is_empty() {
            format!("subtitles='{}'", sub_path.replace('\'', "'\\''"))
        } else {
            format!("subtitles='{}':force_style='{}'", sub_path.replace('\'', "'\\''"), style)
        };

        let args = vec!["-y", "-i", &input, "-vf", &filter, "-c:a", "copy", &output];
        Self::run_ffmpeg(&args).await?;
        Ok(json!({"output": output, "action": "subtitle", "subtitle_file": sub_path}))
    }

    async fn action_thumbnail(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let input = Self::resolve_input(ctx, params);
        let format = params.get("format").and_then(|v| v.as_str()).unwrap_or("jpg");
        let media_dir = ctx.workspace.join("media");
        let _ = std::fs::create_dir_all(&media_dir);

        let mut outputs = Vec::new();

        if let Some(times) = params.get("times").and_then(|v| v.as_array()) {
            for (i, t) in times.iter().enumerate() {
                let ts = t.as_f64().unwrap_or(0.0);
                let ts_str = format!("{}", ts);
                let out = media_dir.join(format!("thumb_{}_{}.{}", chrono::Utc::now().format("%Y%m%d_%H%M%S"), i, format));
                let out_str = out.to_string_lossy().to_string();
                let args = vec!["-y", "-i", &input, "-ss", &ts_str, "-vframes", "1", &out_str];
                Self::run_ffmpeg(&args).await?;
                outputs.push(out_str);
            }
        } else if let Some(interval) = params.get("interval").and_then(|v| v.as_f64()) {
            let fps_val = 1.0 / interval;
            let fps_filter = format!("fps={}", fps_val);
            let out_pattern = media_dir.join(format!("thumb_%04d.{}", format));
            let out_str = out_pattern.to_string_lossy().to_string();
            let args = vec!["-y", "-i", &input, "-vf", &fps_filter, &out_str];
            Self::run_ffmpeg(&args).await?;
            outputs.push(format!("Pattern: {}", out_str));
        } else {
            // Default: extract frame at 0s
            let out = media_dir.join(format!("thumb_{}.{}", chrono::Utc::now().format("%Y%m%d_%H%M%S"), format));
            let out_str = out.to_string_lossy().to_string();
            let args = vec!["-y", "-i", &input, "-ss", "0", "-vframes", "1", &out_str];
            Self::run_ffmpeg(&args).await?;
            outputs.push(out_str);
        }

        Ok(json!({"outputs": outputs, "action": "thumbnail", "count": outputs.len()}))
    }

    async fn action_convert(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let input = Self::resolve_input(ctx, params);
        let format = params.get("format").and_then(|v| v.as_str()).unwrap_or("mp4");
        let output = Self::resolve_output(ctx, params, format);
        let codec = params.get("codec").and_then(|v| v.as_str());
        let audio_codec = params.get("audio_codec").and_then(|v| v.as_str());

        let mut args: Vec<String> = vec!["-y".into(), "-i".into(), input.clone()];

        if let Some(c) = codec {
            args.extend_from_slice(&["-c:v".into(), c.into()]);
        }
        if let Some(ac) = audio_codec {
            if ac == "none" {
                args.push("-an".into());
            } else {
                args.extend_from_slice(&["-c:a".into(), ac.into()]);
            }
        }
        if let Some(br) = params.get("bitrate").and_then(|v| v.as_str()) {
            args.extend_from_slice(&["-b:v".into(), br.into()]);
        }
        if let Some(fps) = params.get("fps").and_then(|v| v.as_f64()) {
            args.extend_from_slice(&["-r".into(), format!("{}", fps)]);
        }
        if params.get("no_audio").and_then(|v| v.as_bool()).unwrap_or(false) {
            args.push("-an".into());
        }
        if let Some(extra) = params.get("extra_args").and_then(|v| v.as_str()) {
            for arg in extra.split_whitespace() {
                args.push(arg.into());
            }
        }
        args.push(output.clone());

        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        Self::run_ffmpeg(&arg_refs).await?;
        Ok(json!({"output": output, "action": "convert", "format": format}))
    }

    async fn action_extract_audio(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let input = Self::resolve_input(ctx, params);
        let format = params.get("format").and_then(|v| v.as_str()).unwrap_or("mp3");
        let output = Self::resolve_output(ctx, params, format);

        let codec = match format {
            "mp3" => "libmp3lame",
            "wav" => "pcm_s16le",
            "aac" | "m4a" => "aac",
            "opus" => "libopus",
            "flac" => "flac",
            _ => "copy",
        };

        let args = vec!["-y", "-i", &input, "-vn", "-acodec", codec, &output];
        Self::run_ffmpeg(&args).await?;
        Ok(json!({"output": output, "action": "extract_audio", "format": format}))
    }

    async fn action_resize(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let input = Self::resolve_input(ctx, params);
        let output = Self::resolve_output(ctx, params, "mp4");
        let width = params.get("width").and_then(|v| v.as_i64()).unwrap_or(-1);
        let height = params.get("height").and_then(|v| v.as_i64()).unwrap_or(-1);

        // Ensure even dimensions for h264
        let scale = format!("scale={}:{}", 
            if width > 0 { format!("{}", width - width % 2) } else { "-2".to_string() },
            if height > 0 { format!("{}", height - height % 2) } else { "-2".to_string() }
        );

        let args = vec!["-y", "-i", &input, "-vf", &scale, "-c:a", "copy", &output];
        Self::run_ffmpeg(&args).await?;
        Ok(json!({"output": output, "action": "resize", "scale": scale}))
    }

    async fn action_compress(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let input = Self::resolve_input(ctx, params);
        let output = Self::resolve_output(ctx, params, "mp4");
        let quality = params.get("quality").and_then(|v| v.as_u64()).unwrap_or(23);
        let quality_str = quality.to_string();
        let codec = params.get("codec").and_then(|v| v.as_str()).unwrap_or("h264");

        let mut args: Vec<String> = vec!["-y".into(), "-i".into(), input.clone()];

        match codec {
            "h265" | "hevc" => {
                args.extend_from_slice(&["-c:v".into(), "libx265".into(), "-crf".into(), quality_str.clone()]);
            }
            "vp9" => {
                args.extend_from_slice(&["-c:v".into(), "libvpx-vp9".into(), "-crf".into(), quality_str.clone(), "-b:v".into(), "0".into()]);
            }
            _ => {
                args.extend_from_slice(&["-c:v".into(), "libx264".into(), "-crf".into(), quality_str.clone(), "-preset".into(), "medium".into()]);
            }
        }

        if let Some(br) = params.get("bitrate").and_then(|v| v.as_str()) {
            args.extend_from_slice(&["-b:v".into(), br.into()]);
        }

        args.extend_from_slice(&["-c:a".into(), "aac".into(), "-b:a".into(), "128k".into()]);
        if params.get("no_audio").and_then(|v| v.as_bool()).unwrap_or(false) {
            args.push("-an".into());
        }
        args.push(output.clone());

        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        Self::run_ffmpeg(&arg_refs).await?;

        // Report size reduction
        let input_size = std::fs::metadata(&input).map(|m| m.len()).unwrap_or(0);
        let output_size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
        let reduction = if input_size > 0 {
            ((1.0 - output_size as f64 / input_size as f64) * 100.0).round()
        } else { 0.0 };

        Ok(json!({
            "output": output,
            "action": "compress",
            "codec": codec,
            "quality": quality,
            "input_size_bytes": input_size,
            "output_size_bytes": output_size,
            "size_reduction_percent": reduction
        }))
    }

    async fn action_watermark(&self, ctx: &ToolContext, params: &Value) -> Result<Value> {
        let input = Self::resolve_input(ctx, params);
        let output = Self::resolve_output(ctx, params, "mp4");
        let position = params.get("watermark_position").and_then(|v| v.as_str()).unwrap_or("bottom-right");

        if let Some(img) = params.get("watermark_image").and_then(|v| v.as_str()) {
            let img_path = resolve_path(ctx, img);
            let overlay = match position {
                "top-left" => "overlay=10:10",
                "top-right" => "overlay=W-w-10:10",
                "bottom-left" => "overlay=10:H-h-10",
                "center" => "overlay=(W-w)/2:(H-h)/2",
                _ => "overlay=W-w-10:H-h-10", // bottom-right
            };
            let args = vec!["-y", "-i", &input, "-i", &img_path, "-filter_complex", overlay, "-c:a", "copy", &output];
            Self::run_ffmpeg(&args).await?;
        } else if let Some(text) = params.get("watermark_text").and_then(|v| v.as_str()) {
            let (x, y) = match position {
                "top-left" => ("10", "10"),
                "top-right" => ("w-tw-10", "10"),
                "bottom-left" => ("10", "h-th-10"),
                "center" => ("(w-tw)/2", "(h-th)/2"),
                _ => ("w-tw-10", "h-th-10"),
            };
            let drawtext = format!(
                "drawtext=text='{}':fontsize=24:fontcolor=white:x={}:y={}:shadowcolor=black:shadowx=2:shadowy=2",
                text.replace('\'', "'\\''"), x, y
            );
            let args = vec!["-y", "-i", &input, "-vf", &drawtext, "-c:a", "copy", &output];
            Self::run_ffmpeg(&args).await?;
        } else {
            return Err(Error::Tool("Either 'watermark_image' or 'watermark_text' is required".into()));
        }

        Ok(json!({"output": output, "action": "watermark", "position": position}))
    }
}

fn resolve_path(ctx: &ToolContext, path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else if path.starts_with("~/") {
        dirs::home_dir()
            .map(|h| h.join(&path[2..]).to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string())
    } else {
        ctx.workspace.join(path).to_string_lossy().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = VideoProcessTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "video_process");
        assert!(schema.description.contains("ffmpeg"));
    }

    #[test]
    fn test_validate_valid() {
        let tool = VideoProcessTool;
        assert!(tool.validate(&json!({"action": "info"})).is_ok());
        assert!(tool.validate(&json!({"action": "clip", "input": "test.mp4", "start": "0", "end": "10"})).is_ok());
        assert!(tool.validate(&json!({"action": "convert", "input": "test.mp4", "format": "webm"})).is_ok());
        assert!(tool.validate(&json!({"action": "merge", "inputs": ["a.mp4", "b.mp4"]})).is_ok());
    }

    #[test]
    fn test_validate_invalid_action() {
        let tool = VideoProcessTool;
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }

    #[test]
    fn test_validate_merge_needs_two() {
        let tool = VideoProcessTool;
        assert!(tool.validate(&json!({"action": "merge", "inputs": ["a.mp4"]})).is_err());
    }

    #[test]
    fn test_validate_subtitle_needs_file() {
        let tool = VideoProcessTool;
        assert!(tool.validate(&json!({"action": "subtitle", "input": "test.mp4"})).is_err());
        assert!(tool.validate(&json!({"action": "subtitle", "input": "test.mp4", "subtitle_file": "sub.srt"})).is_ok());
    }

    #[test]
    fn test_resolve_path() {
        let ctx = ToolContext {
            workspace: std::path::PathBuf::from("/tmp/workspace"),
            builtin_skills_dir: None,
            session_key: String::new(),
            channel: String::new(),
            chat_id: String::new(),
            config: blockcell_core::Config::default(),
            permissions: blockcell_core::types::PermissionSet::new(),
            task_manager: None,
            memory_store: None,
            outbound_tx: None,
            spawn_handle: None,
            capability_registry: None,
            core_evolution: None,
        };
        assert_eq!(resolve_path(&ctx, "/absolute/path.mp4"), "/absolute/path.mp4");
        assert_eq!(resolve_path(&ctx, "relative.mp4"), "/tmp/workspace/relative.mp4");
    }
}
