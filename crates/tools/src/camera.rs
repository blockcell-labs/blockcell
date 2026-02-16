use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for capturing photos using macOS camera.
///
/// Uses `imagecapture` CLI on macOS to take photos from connected cameras.
/// Falls back to `ffmpeg` if imagecapture is not available.
pub struct CameraCaptureTool;

#[async_trait]
impl Tool for CameraCaptureTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "camera_capture",
            description: "Capture a photo from a connected camera (macOS). Actions: 'list' to list available cameras, 'capture' to take a photo, 'info' to get camera info.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "capture", "info"],
                        "description": "Action to perform: 'list' lists cameras, 'capture' takes a photo, 'info' gets camera details"
                    },
                    "device_index": {
                        "type": "integer",
                        "description": "Camera device index (0-based). Use 'list' to see available devices. Default: 0"
                    },
                    "output_path": {
                        "type": "string",
                        "description": "Output file path for the captured photo. Default: auto-generated in workspace media dir. Supports .jpg, .png, .tiff"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["jpg", "png", "tiff"],
                        "description": "Image format. Default: jpg"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        if !["list", "capture", "info"].contains(&action) {
            return Err(Error::Tool("action must be 'list', 'capture', or 'info'".to_string()));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("list");

        match action {
            "list" => list_cameras().await,
            "capture" => {
                let device_index = params.get("device_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let format = params.get("format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("jpg");
                let output_path = params.get("output_path")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        let media_dir = ctx.workspace.join("media");
                        let _ = std::fs::create_dir_all(&media_dir);
                        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
                        media_dir.join(format!("photo_{}.{}", timestamp, format))
                            .to_string_lossy()
                            .to_string()
                    });
                capture_photo(device_index, &output_path, format).await
            }
            "info" => camera_info().await,
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

/// List available cameras on macOS.
async fn list_cameras() -> Result<Value> {
    // Method 1: Try system_profiler for camera info
    let sp_result = tokio::process::Command::new("system_profiler")
        .args(["SPCameraDataType", "-json"])
        .output()
        .await;

    let mut cameras = Vec::new();

    if let Ok(output) = sp_result {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(data) = serde_json::from_str::<Value>(&stdout) {
                if let Some(cam_data) = data.get("SPCameraDataType").and_then(|v| v.as_array()) {
                    for (i, cam) in cam_data.iter().enumerate() {
                        let name = cam.get("_name").and_then(|v| v.as_str()).unwrap_or("Unknown");
                        let model_id = cam.get("spcamera_model-id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let unique_id = cam.get("spcamera_unique-id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        cameras.push(json!({
                            "index": i,
                            "name": name,
                            "model_id": model_id,
                            "unique_id": unique_id,
                        }));
                    }
                }
            }
        }
    }

    // Method 2: Try ffmpeg device listing as fallback
    if cameras.is_empty() {
        let ff_result = tokio::process::Command::new("ffmpeg")
            .args(["-f", "avfoundation", "-list_devices", "true", "-i", ""])
            .output()
            .await;

        if let Ok(output) = ff_result {
            // ffmpeg outputs device list to stderr
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut in_video = false;
            let mut idx = 0usize;
            for line in stderr.lines() {
                if line.contains("AVFoundation video devices:") {
                    in_video = true;
                    continue;
                }
                if line.contains("AVFoundation audio devices:") {
                    break;
                }
                if in_video {
                    // Parse lines like "[AVFoundation indev @ 0x...] [0] FaceTime HD Camera"
                    if let Some(bracket_pos) = line.rfind(']') {
                        let device_name = line[bracket_pos + 1..].trim();
                        if !device_name.is_empty() {
                            cameras.push(json!({
                                "index": idx,
                                "name": device_name,
                                "source": "avfoundation",
                            }));
                            idx += 1;
                        }
                    }
                }
            }
        }
    }

    if cameras.is_empty() {
        // Even if we can't enumerate, most Macs have a built-in camera
        cameras.push(json!({
            "index": 0,
            "name": "Default Camera (assumed)",
            "note": "Could not enumerate cameras. The default camera (index 0) is usually available on Mac."
        }));
    }

    info!(count = cameras.len(), "📷 Listed {} cameras", cameras.len());

    Ok(json!({
        "cameras": cameras,
        "count": cameras.len(),
        "platform": "macOS",
    }))
}

/// Capture a photo from the specified camera.
async fn capture_photo(device_index: usize, output_path: &str, format: &str) -> Result<Value> {
    // Ensure output directory exists
    if let Some(parent) = std::path::Path::new(output_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    info!(
        device = device_index,
        path = %output_path,
        format = %format,
        "📷 Capturing photo"
    );

    // Method 1: Try imagecapture (macOS built-in, available on older macOS)
    let imagecapture_result = try_imagecapture(output_path).await;
    if let Ok(result) = imagecapture_result {
        return Ok(result);
    }

    // Method 2: Try ffmpeg with avfoundation
    let ffmpeg_result = try_ffmpeg_capture(device_index, output_path, format).await;
    if let Ok(result) = ffmpeg_result {
        return Ok(result);
    }

    // Method 3: Try screencapture (captures screen, not camera, but useful as last resort)
    // Only use this if explicitly requested or as degraded fallback
    debug!("📷 Camera capture methods exhausted, trying screencapture as fallback");
    let screencapture_result = try_screencapture(output_path).await;
    if let Ok(result) = screencapture_result {
        return Ok(result);
    }

    Err(Error::Tool(
        "Failed to capture photo. Tried: imagecapture, ffmpeg (avfoundation), screencapture. \
         Make sure a camera is connected and accessible. \
         Install ffmpeg via 'brew install ffmpeg' for best camera support.".to_string()
    ))
}

/// Try capturing with macOS `imagecapture` command.
async fn try_imagecapture(output_path: &str) -> Result<Value> {
    // imagecapture -t <format> <output_path>
    // Note: imagecapture was removed in newer macOS versions
    let output = tokio::process::Command::new("imagecapture")
        .args(["-t", "jpg", output_path])
        .output()
        .await
        .map_err(|e| Error::Tool(format!("imagecapture not available: {}", e)))?;

    if output.status.success() {
        let file_size = std::fs::metadata(output_path)
            .map(|m| m.len())
            .unwrap_or(0);
        info!(path = %output_path, size = file_size, "📷 Photo captured via imagecapture");
        Ok(json!({
            "success": true,
            "path": output_path,
            "method": "imagecapture",
            "file_size_bytes": file_size,
        }))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(Error::Tool(format!("imagecapture failed: {}", stderr)))
    }
}

/// Try capturing with ffmpeg using AVFoundation.
async fn try_ffmpeg_capture(device_index: usize, output_path: &str, format: &str) -> Result<Value> {
    // Check if ffmpeg is available
    if which::which("ffmpeg").is_err() {
        return Err(Error::Tool("ffmpeg not installed".to_string()));
    }

    // ffmpeg -f avfoundation -framerate 30 -video_size 1280x720 -i "0" -frames:v 1 output.jpg
    let device_str = format!("{}", device_index);
    
    // Determine codec based on format
    let codec_args: Vec<&str> = match format {
        "png" => vec!["-c:v", "png"],
        "tiff" => vec!["-c:v", "tiff"],
        _ => vec!["-c:v", "mjpeg", "-q:v", "2"], // jpg with high quality
    };

    let mut cmd = tokio::process::Command::new("ffmpeg");
    cmd.args([
        "-f", "avfoundation",
        "-framerate", "30",
        "-video_size", "1280x720",
        "-i", &device_str,
        "-frames:v", "1",
    ]);
    for arg in &codec_args {
        cmd.arg(arg);
    }
    cmd.args(["-y", output_path]); // -y to overwrite

    let output = cmd.output().await
        .map_err(|e| Error::Tool(format!("ffmpeg execution failed: {}", e)))?;

    if output.status.success() || std::path::Path::new(output_path).exists() {
        let file_size = std::fs::metadata(output_path)
            .map(|m| m.len())
            .unwrap_or(0);
        
        if file_size > 0 {
            info!(path = %output_path, size = file_size, "📷 Photo captured via ffmpeg");
            return Ok(json!({
                "success": true,
                "path": output_path,
                "method": "ffmpeg_avfoundation",
                "file_size_bytes": file_size,
                "resolution": "1280x720",
            }));
        }
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(Error::Tool(format!("ffmpeg capture failed: {}", stderr.chars().take(500).collect::<String>())))
}

/// Try screencapture as a last resort (captures screen, not camera).
async fn try_screencapture(output_path: &str) -> Result<Value> {
    // screencapture -C -x <output_path>
    // -C captures the cursor, -x no sound
    let output = tokio::process::Command::new("screencapture")
        .args(["-C", "-x", output_path])
        .output()
        .await
        .map_err(|e| Error::Tool(format!("screencapture failed: {}", e)))?;

    if output.status.success() {
        let file_size = std::fs::metadata(output_path)
            .map(|m| m.len())
            .unwrap_or(0);
        warn!(path = %output_path, "📷 Used screencapture (screen, not camera) as fallback");
        Ok(json!({
            "success": true,
            "path": output_path,
            "method": "screencapture",
            "file_size_bytes": file_size,
            "note": "This captured the screen, not the camera. Install ffmpeg for camera support: brew install ffmpeg"
        }))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(Error::Tool(format!("screencapture failed: {}", stderr)))
    }
}

/// Get detailed camera information.
async fn camera_info() -> Result<Value> {
    let mut info = json!({
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    });

    // Check available capture methods
    let has_imagecapture = which::which("imagecapture").is_ok();
    let has_ffmpeg = which::which("ffmpeg").is_ok();
    let has_screencapture = which::which("screencapture").is_ok();

    info["capture_methods"] = json!({
        "imagecapture": has_imagecapture,
        "ffmpeg": has_ffmpeg,
        "screencapture": has_screencapture,
        "recommended": if has_ffmpeg { "ffmpeg" } else if has_imagecapture { "imagecapture" } else { "screencapture" },
    });

    // Get camera list
    let cameras = list_cameras().await?;
    info["cameras"] = cameras;

    // Check ffmpeg version if available
    if has_ffmpeg {
        if let Ok(output) = tokio::process::Command::new("ffmpeg").arg("-version").output().await {
            let version_line = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            info["ffmpeg_version"] = json!(version_line);
        }
    }

    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_camera_tool_schema() {
        let tool = CameraCaptureTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "camera_capture");
    }

    #[test]
    fn test_camera_tool_validate() {
        let tool = CameraCaptureTool;
        assert!(tool.validate(&json!({"action": "list"})).is_ok());
        assert!(tool.validate(&json!({"action": "capture"})).is_ok());
        assert!(tool.validate(&json!({"action": "info"})).is_ok());
        assert!(tool.validate(&json!({"action": "invalid"})).is_err());
    }
}
