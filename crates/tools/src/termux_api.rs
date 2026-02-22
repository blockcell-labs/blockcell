use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::{Tool, ToolContext, ToolSchema};

/// Comprehensive Termux API tool for controlling Android devices from blockcell.
///
/// Requires `termux-api` package installed on the device:
///   pkg install termux-api
///
/// Also requires the Termux:API companion app from F-Droid/Play Store.
///
/// This tool wraps all major termux-api commands, enabling blockcell to:
/// - Access device sensors (battery, location, sensors, telephony, WiFi)
/// - Control hardware (camera, torch, vibrate, brightness, volume, infrared)
/// - Communicate (SMS, phone calls, contacts, clipboard, notifications, share, dialog)
/// - Media (TTS, speech-to-text, media player, microphone recording, wallpaper)
/// - Security (fingerprint auth, keystore)
/// - System (download, open URL/file, media scan, job scheduler, wake lock, storage, audio info)
pub struct TermuxApiTool;

#[async_trait]
impl Tool for TermuxApiTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "termux_api",
            description: "Control Android device via Termux API. Requires termux-api package and Termux:API app. \
                Actions: battery_status, brightness, camera_info, camera_photo, clipboard_get, clipboard_set, \
                contact_list, call_log, dialog, download, fingerprint, infrared_frequencies, infrared_transmit, \
                keystore, location, media_player, media_scan, microphone_record, notification, notification_remove, \
                open, open_url, sensor, share, sms_list, sms_send, speech_to_text, storage_get, \
                telephony_deviceinfo, telephony_cellinfo, telephony_call, toast, torch, tts_engines, tts_speak, \
                vibrate, volume, wallpaper, wifi_connectioninfo, wifi_scaninfo, wifi_enable, \
                audio_info, wake_lock, wake_unlock, job_scheduler, info",
            parameters: build_schema(),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid_actions = [
            "battery_status", "brightness", "camera_info", "camera_photo",
            "clipboard_get", "clipboard_set", "contact_list", "call_log",
            "dialog", "download", "fingerprint",
            "infrared_frequencies", "infrared_transmit",
            "keystore", "location", "media_player", "media_scan",
            "microphone_record", "notification", "notification_remove",
            "open", "open_url", "sensor", "share",
            "sms_list", "sms_send", "speech_to_text", "storage_get",
            "telephony_deviceinfo", "telephony_cellinfo", "telephony_call",
            "toast", "torch", "tts_engines", "tts_speak",
            "vibrate", "volume", "wallpaper",
            "wifi_connectioninfo", "wifi_scaninfo", "wifi_enable",
            "audio_info", "wake_lock", "wake_unlock", "job_scheduler",
            "info",
        ];
        if !valid_actions.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid actions: {}", action, valid_actions.join(", "))));
        }

        // Validate required params per action
        match action {
            "sms_send" => {
                if params.get("number").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'number' is required for sms_send".into()));
                }
                if params.get("text").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'text' is required for sms_send".into()));
                }
            }
            "telephony_call" => {
                if params.get("number").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'number' is required for telephony_call".into()));
                }
            }
            "clipboard_set" => {
                if params.get("text").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'text' is required for clipboard_set".into()));
                }
            }
            "tts_speak" => {
                if params.get("text").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'text' is required for tts_speak".into()));
                }
            }
            "open_url" => {
                if params.get("url").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'url' is required for open_url".into()));
                }
            }
            "notification_remove" => {
                if params.get("notification_id").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'notification_id' is required for notification_remove".into()));
                }
            }
            "infrared_transmit" => {
                if params.get("frequency").is_none() {
                    return Err(Error::Tool("'frequency' is required for infrared_transmit".into()));
                }
                if params.get("ir_pattern").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Tool("'ir_pattern' is required for infrared_transmit".into()));
                }
            }
            _ => {}
        }

        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("info");

        // First check if we're running on Termux
        if action != "info" {
            if !is_termux_available().await {
                return Err(Error::Tool(
                    "Termux API is not available. Make sure you are running on Android with \
                     'termux-api' package installed (pkg install termux-api) and the Termux:API \
                     companion app is installed.".into()
                ));
            }
        }

        match action {
            "info" => action_info().await,
            "battery_status" => action_simple_command("termux-battery-status").await,
            "camera_info" => action_simple_command("termux-camera-info").await,
            "camera_photo" => action_camera_photo(&ctx, &params).await,
            "clipboard_get" => action_simple_command("termux-clipboard-get").await,
            "clipboard_set" => action_clipboard_set(&params).await,
            "contact_list" => action_simple_command("termux-contact-list").await,
            "call_log" => action_call_log(&params).await,
            "brightness" => action_brightness(&params).await,
            "dialog" => action_dialog(&params).await,
            "download" => action_download(&params).await,
            "fingerprint" => action_simple_command("termux-fingerprint").await,
            "infrared_frequencies" => action_simple_command("termux-infrared-frequencies").await,
            "infrared_transmit" => action_infrared_transmit(&params).await,
            "keystore" => action_keystore(&params).await,
            "location" => action_location(&params).await,
            "media_player" => action_media_player(&params).await,
            "media_scan" => action_media_scan(&params).await,
            "microphone_record" => action_microphone_record(&ctx, &params).await,
            "notification" => action_notification(&params).await,
            "notification_remove" => action_notification_remove(&params).await,
            "open" => action_open(&params).await,
            "open_url" => action_open_url(&params).await,
            "sensor" => action_sensor(&params).await,
            "share" => action_share(&params).await,
            "sms_list" => action_sms_list(&params).await,
            "sms_send" => action_sms_send(&params).await,
            "speech_to_text" => action_simple_command("termux-speech-to-text").await,
            "storage_get" => action_storage_get(&params).await,
            "telephony_deviceinfo" => action_simple_command("termux-telephony-deviceinfo").await,
            "telephony_cellinfo" => action_simple_command("termux-telephony-cellinfo").await,
            "telephony_call" => action_telephony_call(&params).await,
            "toast" => action_toast(&params).await,
            "torch" => action_torch(&params).await,
            "tts_engines" => action_simple_command("termux-tts-engines").await,
            "tts_speak" => action_tts_speak(&params).await,
            "vibrate" => action_vibrate(&params).await,
            "volume" => action_volume(&params).await,
            "wallpaper" => action_wallpaper(&params).await,
            "wifi_connectioninfo" => action_simple_command("termux-wifi-connectioninfo").await,
            "wifi_scaninfo" => action_simple_command("termux-wifi-scaninfo").await,
            "wifi_enable" => action_wifi_enable(&params).await,
            "audio_info" => action_simple_command("termux-audio-info").await,
            "wake_lock" => action_simple_command("termux-wake-lock").await,
            "wake_unlock" => action_simple_command("termux-wake-unlock").await,
            "job_scheduler" => action_job_scheduler(&params).await,
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

// ============================================================================
// Schema builder (programmatic to avoid json! macro recursion limit)
// ============================================================================

fn prop_str(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}

fn prop_str_enum(desc: &str, values: &[&str]) -> Value {
    json!({"type": "string", "enum": values, "description": desc})
}

fn prop_int(desc: &str) -> Value {
    json!({"type": "integer", "description": desc})
}

fn prop_num(desc: &str) -> Value {
    json!({"type": "number", "description": desc})
}

fn prop_bool(desc: &str) -> Value {
    json!({"type": "boolean", "description": desc})
}

fn build_schema() -> Value {
    use serde_json::Map;

    let mut props = Map::new();

    // action enum
    props.insert("action".into(), prop_str_enum(
        "Termux API action to perform",
        &[
            "battery_status", "brightness", "camera_info", "camera_photo",
            "clipboard_get", "clipboard_set", "contact_list", "call_log",
            "dialog", "download", "fingerprint",
            "infrared_frequencies", "infrared_transmit",
            "keystore", "location", "media_player", "media_scan",
            "microphone_record", "notification", "notification_remove",
            "open", "open_url", "sensor", "share",
            "sms_list", "sms_send", "speech_to_text", "storage_get",
            "telephony_deviceinfo", "telephony_cellinfo", "telephony_call",
            "toast", "torch", "tts_engines", "tts_speak",
            "vibrate", "volume", "wallpaper",
            "wifi_connectioninfo", "wifi_scaninfo", "wifi_enable",
            "audio_info", "wake_lock", "wake_unlock", "job_scheduler",
            "info",
        ],
    ));

    // General params
    props.insert("text".into(), prop_str("Text content for: clipboard_set, toast, tts_speak, sms_send, notification (--content), share (stdin text), dialog title"));
    props.insert("number".into(), prop_str("(sms_send, telephony_call) Phone number(s). For SMS, comma-separated for multiple recipients"));
    props.insert("output_path".into(), prop_str("(camera_photo, microphone_record, storage_get) Output file path"));
    props.insert("camera_id".into(), prop_int("(camera_photo) Camera ID, default 0. Use camera_info to list cameras"));
    props.insert("brightness".into(), prop_int("(brightness) Screen brightness 0-255, or -1 for auto"));
    props.insert("title".into(), prop_str("(notification, download, share) Title text"));
    props.insert("url".into(), prop_str("(open_url, download, wallpaper) URL to open/download/set as wallpaper"));
    props.insert("file_path".into(), prop_str("(open, share, media_scan, wallpaper) File path to open/share/scan/set as wallpaper"));
    props.insert("content_type".into(), prop_str("(open, share) MIME content type"));
    props.insert("limit".into(), prop_int("(sms_list, call_log) Max number of items. Default: 10"));
    props.insert("offset".into(), prop_int("(sms_list, call_log) Offset in list. Default: 0"));
    props.insert("duration".into(), prop_int("(vibrate) Duration in ms, default 1000. (microphone_record) Recording limit in seconds"));
    props.insert("enabled".into(), prop_bool("(wifi_enable, torch) true=on, false=off"));
    props.insert("force".into(), prop_bool("(vibrate) Force vibration even in silent mode"));
    props.insert("recursive".into(), prop_bool("(media_scan) Scan directories recursively"));

    // Location
    props.insert("provider".into(), prop_str_enum("(location) Location provider. Default: gps", &["gps", "network", "passive"]));
    props.insert("request".into(), prop_str_enum("(location) Request type. Default: once", &["once", "last", "updates"]));

    // Notification
    props.insert("notification_id".into(), prop_str("(notification, notification_remove) Notification ID"));
    props.insert("priority".into(), prop_str_enum("(notification) Notification priority", &["high", "low", "max", "min", "default"]));
    props.insert("sound".into(), prop_bool("(notification) Play sound with notification"));
    props.insert("vibrate_pattern".into(), prop_str("(notification) Vibrate pattern, comma-separated ms values e.g. '500,1000,200'"));
    props.insert("led_color".into(), prop_str("(notification) LED color as RRGGBB hex"));
    props.insert("notification_action".into(), prop_str("(notification) Action to execute when pressing the notification"));

    // Volume / stream
    props.insert("stream".into(), prop_str_enum("(volume, tts_speak) Audio stream", &["alarm", "music", "notification", "ring", "system", "call"]));
    props.insert("volume_value".into(), prop_int("(volume) Volume level to set"));

    // Share
    props.insert("share_action".into(), prop_str_enum("(share) Action to perform on shared content. Default: view", &["edit", "send", "view"]));

    // SMS
    props.insert("sms_type".into(), prop_str_enum("(sms_list) Type of SMS messages to list. Default: inbox", &["all", "inbox", "sent", "draft", "outbox"]));

    // Dialog
    props.insert("dialog_widget".into(), prop_str_enum("(dialog) Widget type for user input dialog", &["confirm", "checkbox", "counter", "date", "radio", "sheet", "spinner", "speech", "text", "time"]));
    props.insert("dialog_values".into(), prop_str("(dialog) Comma-separated values for checkbox/radio/sheet/spinner widgets"));

    // Sensor
    props.insert("sensor_name".into(), prop_str("(sensor) Sensor name(s) to listen to (partial match). Use 'list' to see available sensors"));
    props.insert("sensor_limit".into(), prop_int("(sensor) Number of sensor readings to take. Default: 1"));
    props.insert("sensor_delay".into(), prop_int("(sensor) Delay between readings in ms"));

    // TTS
    props.insert("tts_engine".into(), prop_str("(tts_speak) TTS engine to use (see tts_engines)"));
    props.insert("tts_language".into(), prop_str("(tts_speak) Language code for TTS"));
    props.insert("tts_pitch".into(), prop_num("(tts_speak) Pitch multiplier, 1.0 is normal"));
    props.insert("tts_rate".into(), prop_num("(tts_speak) Speech rate multiplier, 1.0 is normal"));

    // Microphone
    props.insert("mic_action".into(), prop_str_enum("(microphone_record) Recording action. Default: start", &["start", "info", "stop"]));
    props.insert("encoder".into(), prop_str_enum("(microphone_record) Audio encoder", &["aac", "amr_wb", "amr_nb"]));
    props.insert("bitrate".into(), prop_int("(microphone_record) Recording bitrate in kbps"));
    props.insert("sample_rate".into(), prop_int("(microphone_record) Sampling rate in Hz"));
    props.insert("channels".into(), prop_int("(microphone_record) Channel count (1=mono, 2=stereo)"));

    // Media player
    props.insert("player_action".into(), prop_str_enum("(media_player) Player action", &["play", "play_file", "pause", "stop", "info"]));

    // Infrared
    props.insert("frequency".into(), prop_int("(infrared_transmit) IR carrier frequency in Hz"));
    props.insert("ir_pattern".into(), prop_str("(infrared_transmit) IR on/off pattern, comma-separated intervals e.g. '20,50,20,30'"));

    // Keystore
    props.insert("keystore_action".into(), prop_str_enum("(keystore) Keystore operation", &["list", "generate", "delete", "sign", "verify"]));
    props.insert("key_alias".into(), prop_str("(keystore) Key alias name"));
    props.insert("key_algorithm".into(), prop_str_enum("(keystore generate) Algorithm. Default: RSA", &["RSA", "EC"]));
    props.insert("key_size".into(), prop_int("(keystore generate) Key size. RSA: 2048/3072/4096. EC: 256/384/521"));
    props.insert("sign_algorithm".into(), prop_str("(keystore sign/verify) Signing algorithm e.g. 'SHA256withRSA'"));
    props.insert("sign_data".into(), prop_str("(keystore sign) Data to sign (passed via stdin)"));
    props.insert("signature".into(), prop_str("(keystore verify) Signature file path"));

    // Wallpaper
    props.insert("wallpaper_lockscreen".into(), prop_bool("(wallpaper) Set wallpaper for lockscreen (Android 7+)"));

    // Toast
    props.insert("toast_bg_color".into(), prop_str("(toast) Background color name or #RRGGBB"));
    props.insert("toast_text_color".into(), prop_str("(toast) Text color name or #RRGGBB"));
    props.insert("toast_position".into(), prop_str_enum("(toast) Toast position. Default: middle", &["top", "middle", "bottom"]));
    props.insert("toast_short".into(), prop_bool("(toast) Show toast for a short duration only"));

    // Job scheduler
    props.insert("job_script".into(), prop_str("(job_scheduler) Path to script to schedule"));
    props.insert("job_id".into(), prop_int("(job_scheduler) Job ID"));
    props.insert("job_period_ms".into(), prop_int("(job_scheduler) Repeat period in ms (0=once)"));
    props.insert("job_network".into(), prop_str_enum("(job_scheduler) Required network type", &["any", "unmetered", "cellular", "not_roaming", "none"]));
    props.insert("job_charging".into(), prop_bool("(job_scheduler) Run only when charging"));
    props.insert("job_list_pending".into(), prop_bool("(job_scheduler) List pending jobs instead of scheduling"));

    let mut schema = Map::new();
    schema.insert("type".into(), json!("object"));
    schema.insert("properties".into(), Value::Object(props));
    schema.insert("required".into(), json!(["action"]));
    Value::Object(schema)
}

// ============================================================================
// Helper functions
// ============================================================================

/// Check if termux-api commands are available.
async fn is_termux_available() -> bool {
    tokio::process::Command::new("which")
        .arg("termux-battery-status")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run a simple termux command that takes no arguments and returns JSON.
async fn action_simple_command(cmd: &str) -> Result<Value> {
    let output = run_termux_command(cmd, &[]).await?;
    parse_termux_output(cmd, &output)
}

/// Run a termux command with arguments and return raw stdout + stderr.
async fn run_termux_command(cmd: &str, args: &[&str]) -> Result<std::process::Output> {
    debug!(cmd = cmd, args = ?args, "Running termux command");

    let output = tokio::process::Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|e| Error::Tool(format!("Failed to run {}: {}", cmd, e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            warn!(cmd = cmd, stderr = %stderr, "Termux command returned error");
        }
    }

    Ok(output)
}

/// Run a termux command with stdin input.
async fn run_termux_command_with_stdin(cmd: &str, args: &[&str], stdin_data: &str) -> Result<std::process::Output> {
    debug!(cmd = cmd, args = ?args, "Running termux command with stdin");

    use tokio::io::AsyncWriteExt;

    let mut child = tokio::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::Tool(format!("Failed to spawn {}: {}", cmd, e)))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(stdin_data.as_bytes()).await
            .map_err(|e| Error::Tool(format!("Failed to write stdin to {}: {}", cmd, e)))?;
        drop(stdin);
    }

    let output = child.wait_with_output().await
        .map_err(|e| Error::Tool(format!("Failed to wait for {}: {}", cmd, e)))?;

    Ok(output)
}

/// Parse termux command output, trying JSON first, falling back to text.
fn parse_termux_output(cmd: &str, output: &std::process::Output) -> Result<Value> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Try to parse as JSON
    if let Ok(json_val) = serde_json::from_str::<Value>(stdout.trim()) {
        return Ok(json!({
            "action": cmd.trim_start_matches("termux-"),
            "result": json_val,
        }));
    }

    // Return as text
    let text = stdout.trim().to_string();
    let mut result = json!({
        "action": cmd.trim_start_matches("termux-"),
        "output": if text.is_empty() { "OK".to_string() } else { text },
    });

    if !stderr.trim().is_empty() {
        result["stderr"] = json!(stderr.trim());
    }

    if !output.status.success() {
        result["success"] = json!(false);
        result["exit_code"] = json!(output.status.code());
    }

    Ok(result)
}

// ============================================================================
// Action implementations
// ============================================================================

async fn action_info() -> Result<Value> {
    let termux_available = is_termux_available().await;

    let mut backends = json!({
        "termux_api": termux_available,
        "platform": if termux_available { "android/termux" } else { std::env::consts::OS },
    });

    if termux_available {
        // Check termux-info for system details
        if let Ok(output) = run_termux_command("termux-info", &[]).await {
            let stdout = String::from_utf8_lossy(&output.stdout);
            backends["termux_info"] = json!(stdout.trim());
        }
    }

    let categories = json!({
        "device_info": ["battery_status", "audio_info", "telephony_deviceinfo", "telephony_cellinfo", "wifi_connectioninfo", "wifi_scaninfo"],
        "sensors": ["location", "sensor"],
        "camera": ["camera_info", "camera_photo"],
        "communication": ["sms_list", "sms_send", "telephony_call", "contact_list", "call_log", "clipboard_get", "clipboard_set"],
        "media": ["tts_speak", "tts_engines", "speech_to_text", "media_player", "microphone_record", "media_scan"],
        "notifications": ["notification", "notification_remove", "toast", "vibrate"],
        "hardware_control": ["torch", "brightness", "volume", "infrared_frequencies", "infrared_transmit", "wallpaper"],
        "network": ["wifi_enable", "download", "open_url"],
        "security": ["fingerprint", "keystore"],
        "system": ["open", "share", "storage_get", "wake_lock", "wake_unlock", "job_scheduler", "dialog"],
    });

    info!("📱 Termux API info: available={}", termux_available);

    Ok(json!({
        "available": termux_available,
        "backends": backends,
        "categories": categories,
        "note": if termux_available {
            "Termux API is available. All actions are ready to use."
        } else {
            "Termux API is NOT available. Install termux-api package (pkg install termux-api) and the Termux:API companion app."
        }
    }))
}

async fn action_camera_photo(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let camera_id = params.get("camera_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let output_path = params.get("output_path")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let media_dir = ctx.workspace.join("media");
            let _ = std::fs::create_dir_all(&media_dir);
            let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
            media_dir.join(format!("termux_photo_{}.jpg", ts))
                .to_string_lossy().to_string()
        });

    let cam_id_str = camera_id.to_string();
    // termux-camera-photo [-c camera-id] output-file
    let mut full_args: Vec<String> = Vec::new();
    if camera_id != 0 {
        full_args.push("-c".to_string());
        full_args.push(cam_id_str.clone());
    }
    full_args.push(output_path.clone());

    let str_args: Vec<&str> = full_args.iter().map(|s| s.as_str()).collect();
    let output = run_termux_command("termux-camera-photo", &str_args).await?;

    if output.status.success() {
        info!(path = %output_path, "📷 Photo captured via Termux");
        Ok(json!({
            "action": "camera_photo",
            "success": true,
            "output_path": output_path,
            "camera_id": camera_id,
        }))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(Error::Tool(format!("Camera capture failed: {}", stderr.trim())))
    }
}

async fn action_clipboard_set(params: &Value) -> Result<Value> {
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let output = run_termux_command_with_stdin("termux-clipboard-set", &[], text).await?;
    info!("📋 Clipboard set ({} chars)", text.len());
    parse_termux_output("termux-clipboard-set", &output)
}

async fn action_call_log(params: &Value) -> Result<Value> {
    let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);
    let offset = params.get("offset").and_then(|v| v.as_i64()).unwrap_or(0);

    let limit_str = limit.to_string();
    let offset_str = offset.to_string();
    let output = run_termux_command("termux-call-log", &["-l", &limit_str, "-o", &offset_str]).await?;
    parse_termux_output("termux-call-log", &output)
}

async fn action_brightness(params: &Value) -> Result<Value> {
    let brightness = params.get("brightness").and_then(|v| v.as_i64());
    let arg = match brightness {
        Some(-1) => "auto".to_string(),
        Some(v) => v.to_string(),
        None => "auto".to_string(),
    };
    let output = run_termux_command("termux-brightness", &[&arg]).await?;
    info!("🔆 Brightness set to {}", arg);
    parse_termux_output("termux-brightness", &output)
}

async fn action_dialog(params: &Value) -> Result<Value> {
    let widget = params.get("dialog_widget").and_then(|v| v.as_str()).unwrap_or("text");
    let title = params.get("title").and_then(|v| v.as_str());
    let values = params.get("dialog_values").and_then(|v| v.as_str());

    let mut args: Vec<String> = vec![widget.to_string()];
    if let Some(t) = title {
        args.push("-t".to_string());
        args.push(t.to_string());
    }
    if let Some(v) = values {
        args.push("-v".to_string());
        args.push(v.to_string());
    }

    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = run_termux_command("termux-dialog", &str_args).await?;
    parse_termux_output("termux-dialog", &output)
}

async fn action_download(params: &Value) -> Result<Value> {
    let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let title = params.get("title").and_then(|v| v.as_str());
    let description = params.get("text").and_then(|v| v.as_str());

    let mut args: Vec<String> = Vec::new();
    if let Some(t) = title {
        args.push("-t".to_string());
        args.push(t.to_string());
    }
    if let Some(d) = description {
        args.push("-d".to_string());
        args.push(d.to_string());
    }
    args.push(url.to_string());

    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = run_termux_command("termux-download", &str_args).await?;
    info!(url = url, "📥 Download started");
    parse_termux_output("termux-download", &output)
}

async fn action_infrared_transmit(params: &Value) -> Result<Value> {
    let frequency = params.get("frequency").and_then(|v| v.as_i64()).unwrap_or(0);
    let pattern = params.get("ir_pattern").and_then(|v| v.as_str()).unwrap_or("");

    let freq_str = frequency.to_string();
    let output = run_termux_command("termux-infrared-transmit", &["-f", &freq_str, pattern]).await?;
    info!(freq = frequency, "📡 IR transmitted");
    parse_termux_output("termux-infrared-transmit", &output)
}

async fn action_keystore(params: &Value) -> Result<Value> {
    let ks_action = params.get("keystore_action").and_then(|v| v.as_str()).unwrap_or("list");
    let alias = params.get("key_alias").and_then(|v| v.as_str()).unwrap_or("");
    let algorithm = params.get("key_algorithm").and_then(|v| v.as_str());
    let key_size = params.get("key_size").and_then(|v| v.as_i64());
    let sign_algo = params.get("sign_algorithm").and_then(|v| v.as_str()).unwrap_or("");
    let sign_data = params.get("sign_data").and_then(|v| v.as_str()).unwrap_or("");
    let sig_file = params.get("signature").and_then(|v| v.as_str()).unwrap_or("");

    let mut args: Vec<String> = Vec::new();

    match ks_action {
        "list" => {
            args.push("list".to_string());
            args.push("-d".to_string());
        }
        "generate" => {
            args.push("generate".to_string());
            args.push(alias.to_string());
            if let Some(alg) = algorithm {
                args.push("-a".to_string());
                args.push(alg.to_string());
            }
            if let Some(size) = key_size {
                args.push("-s".to_string());
                args.push(size.to_string());
            }
        }
        "delete" => {
            args.push("delete".to_string());
            args.push(alias.to_string());
        }
        "sign" => {
            args.push("sign".to_string());
            args.push(alias.to_string());
            args.push(sign_algo.to_string());
            // Data is passed via stdin
            let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let output = run_termux_command_with_stdin("termux-keystore", &str_args, sign_data).await?;
            return parse_termux_output("termux-keystore", &output);
        }
        "verify" => {
            args.push("verify".to_string());
            args.push(alias.to_string());
            args.push(sign_algo.to_string());
            args.push(sig_file.to_string());
            // Data is passed via stdin
            let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let output = run_termux_command_with_stdin("termux-keystore", &str_args, sign_data).await?;
            return parse_termux_output("termux-keystore", &output);
        }
        _ => {
            return Err(Error::Tool(format!("Unknown keystore action: {}", ks_action)));
        }
    }

    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = run_termux_command("termux-keystore", &str_args).await?;
    parse_termux_output("termux-keystore", &output)
}

async fn action_location(params: &Value) -> Result<Value> {
    let provider = params.get("provider").and_then(|v| v.as_str()).unwrap_or("gps");
    let request = params.get("request").and_then(|v| v.as_str()).unwrap_or("once");

    let output = run_termux_command("termux-location", &["-p", provider, "-r", request]).await?;
    info!(provider = provider, "📍 Location requested");
    parse_termux_output("termux-location", &output)
}

async fn action_media_player(params: &Value) -> Result<Value> {
    let player_action = params.get("player_action").and_then(|v| v.as_str()).unwrap_or("info");
    let file_path = params.get("file_path").and_then(|v| v.as_str());

    let mut args: Vec<String> = Vec::new();

    match player_action {
        "play_file" => {
            args.push("play".to_string());
            if let Some(f) = file_path {
                args.push(f.to_string());
            }
        }
        other => {
            args.push(other.to_string());
        }
    }

    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = run_termux_command("termux-media-player", &str_args).await?;
    parse_termux_output("termux-media-player", &output)
}

async fn action_media_scan(params: &Value) -> Result<Value> {
    let file_path = params.get("file_path").and_then(|v| v.as_str()).unwrap_or(".");
    let recursive = params.get("recursive").and_then(|v| v.as_bool()).unwrap_or(false);

    let mut args: Vec<String> = Vec::new();
    if recursive {
        args.push("-r".to_string());
    }
    args.push("-v".to_string());
    args.push(file_path.to_string());

    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = run_termux_command("termux-media-scan", &str_args).await?;
    parse_termux_output("termux-media-scan", &output)
}

async fn action_microphone_record(ctx: &ToolContext, params: &Value) -> Result<Value> {
    let mic_action = params.get("mic_action").and_then(|v| v.as_str()).unwrap_or("start");

    match mic_action {
        "info" => {
            let output = run_termux_command("termux-microphone-record", &["-i"]).await?;
            return parse_termux_output("termux-microphone-record", &output);
        }
        "stop" => {
            let output = run_termux_command("termux-microphone-record", &["-q"]).await?;
            return parse_termux_output("termux-microphone-record", &output);
        }
        "start" => {
            let output_path = params.get("output_path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| {
                    let media_dir = ctx.workspace.join("media");
                    let _ = std::fs::create_dir_all(&media_dir);
                    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
                    media_dir.join(format!("termux_recording_{}.m4a", ts))
                        .to_string_lossy().to_string()
                });

            let mut args: Vec<String> = vec!["-f".to_string(), output_path.clone()];

            if let Some(limit) = params.get("duration").and_then(|v| v.as_i64()) {
                args.push("-l".to_string());
                args.push(limit.to_string());
            }
            if let Some(encoder) = params.get("encoder").and_then(|v| v.as_str()) {
                args.push("-e".to_string());
                args.push(encoder.to_string());
            }
            if let Some(bitrate) = params.get("bitrate").and_then(|v| v.as_i64()) {
                args.push("-b".to_string());
                args.push(bitrate.to_string());
            }
            if let Some(rate) = params.get("sample_rate").and_then(|v| v.as_i64()) {
                args.push("-r".to_string());
                args.push(rate.to_string());
            }
            if let Some(channels) = params.get("channels").and_then(|v| v.as_i64()) {
                args.push("-c".to_string());
                args.push(channels.to_string());
            }

            let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let output = run_termux_command("termux-microphone-record", &str_args).await?;
            info!(path = %output_path, "🎤 Recording started");

            let mut result = parse_termux_output("termux-microphone-record", &output)?;
            result["output_path"] = json!(output_path);
            Ok(result)
        }
        _ => Err(Error::Tool(format!("Unknown mic_action: {}", mic_action))),
    }
}

async fn action_notification(params: &Value) -> Result<Value> {
    let content = params.get("text").and_then(|v| v.as_str());
    let title = params.get("title").and_then(|v| v.as_str());
    let id = params.get("notification_id").and_then(|v| v.as_str());
    let priority = params.get("priority").and_then(|v| v.as_str());
    let sound = params.get("sound").and_then(|v| v.as_bool()).unwrap_or(false);
    let vibrate_pattern = params.get("vibrate_pattern").and_then(|v| v.as_str());
    let led_color = params.get("led_color").and_then(|v| v.as_str());
    let action = params.get("notification_action").and_then(|v| v.as_str());
    let buttons = params.get("notification_buttons").and_then(|v| v.as_array());

    let mut args: Vec<String> = Vec::new();

    if let Some(c) = content {
        args.push("--content".to_string());
        args.push(c.to_string());
    }
    if let Some(t) = title {
        args.push("--title".to_string());
        args.push(t.to_string());
    }
    if let Some(i) = id {
        args.push("--id".to_string());
        args.push(i.to_string());
    }
    if let Some(p) = priority {
        args.push("--priority".to_string());
        args.push(p.to_string());
    }
    if sound {
        args.push("--sound".to_string());
    }
    if let Some(vp) = vibrate_pattern {
        args.push("--vibrate".to_string());
        args.push(vp.to_string());
    }
    if let Some(lc) = led_color {
        args.push("--led-color".to_string());
        args.push(lc.to_string());
    }
    if let Some(a) = action {
        args.push("--action".to_string());
        args.push(a.to_string());
    }
    if let Some(btns) = buttons {
        for (i, btn) in btns.iter().enumerate().take(3) {
            let num = i + 1;
            if let Some(text) = btn.get("text").and_then(|v| v.as_str()) {
                args.push(format!("--button{}", num));
                args.push(text.to_string());
            }
            if let Some(btn_action) = btn.get("action").and_then(|v| v.as_str()) {
                args.push(format!("--button{}-action", num));
                args.push(btn_action.to_string());
            }
        }
    }

    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = run_termux_command("termux-notification", &str_args).await?;
    info!("🔔 Notification sent");
    parse_termux_output("termux-notification", &output)
}

async fn action_notification_remove(params: &Value) -> Result<Value> {
    let id = params.get("notification_id").and_then(|v| v.as_str()).unwrap_or("");
    let output = run_termux_command("termux-notification-remove", &[id]).await?;
    parse_termux_output("termux-notification-remove", &output)
}

async fn action_open(params: &Value) -> Result<Value> {
    let file_path = params.get("file_path").and_then(|v| v.as_str());
    let url = params.get("url").and_then(|v| v.as_str());
    let content_type = params.get("content_type").and_then(|v| v.as_str());
    let share_action = params.get("share_action").and_then(|v| v.as_str());

    let target = file_path.or(url).unwrap_or("");

    let mut args: Vec<String> = Vec::new();
    if let Some(ct) = content_type {
        args.push("--content-type".to_string());
        args.push(ct.to_string());
    }
    if let Some(sa) = share_action {
        if sa == "send" {
            args.push("--send".to_string());
        } else if sa == "view" {
            args.push("--view".to_string());
        }
    }
    args.push(target.to_string());

    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = run_termux_command("termux-open", &str_args).await?;
    info!(target = target, "📂 Opened");
    parse_termux_output("termux-open", &output)
}

async fn action_open_url(params: &Value) -> Result<Value> {
    let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let output = run_termux_command("termux-open-url", &[url]).await?;
    info!(url = url, "🌐 URL opened");
    parse_termux_output("termux-open-url", &output)
}

async fn action_sensor(params: &Value) -> Result<Value> {
    let sensor_name = params.get("sensor_name").and_then(|v| v.as_str());
    let limit = params.get("sensor_limit").and_then(|v| v.as_i64()).unwrap_or(1);
    let delay = params.get("sensor_delay").and_then(|v| v.as_i64());

    // If sensor_name is "list", list available sensors
    if sensor_name == Some("list") {
        let output = run_termux_command("termux-sensor", &["-l"]).await?;
        return parse_termux_output("termux-sensor", &output);
    }

    let mut args: Vec<String> = Vec::new();

    if let Some(name) = sensor_name {
        args.push("-s".to_string());
        args.push(name.to_string());
    } else {
        // Default: list sensors
        args.push("-l".to_string());
        let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = run_termux_command("termux-sensor", &str_args).await?;
        return parse_termux_output("termux-sensor", &output);
    }

    let limit_str = limit.to_string();
    args.push("-n".to_string());
    args.push(limit_str);

    if let Some(d) = delay {
        let delay_str = d.to_string();
        args.push("-d".to_string());
        args.push(delay_str);
    }

    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = run_termux_command("termux-sensor", &str_args).await?;
    parse_termux_output("termux-sensor", &output)
}

async fn action_share(params: &Value) -> Result<Value> {
    let file_path = params.get("file_path").and_then(|v| v.as_str());
    let text = params.get("text").and_then(|v| v.as_str());
    let title = params.get("title").and_then(|v| v.as_str());
    let content_type = params.get("content_type").and_then(|v| v.as_str());
    let share_action = params.get("share_action").and_then(|v| v.as_str());

    let mut args: Vec<String> = Vec::new();

    if let Some(sa) = share_action {
        args.push("-a".to_string());
        args.push(sa.to_string());
    }
    if let Some(ct) = content_type {
        args.push("-c".to_string());
        args.push(ct.to_string());
    }
    if let Some(t) = title {
        args.push("-t".to_string());
        args.push(t.to_string());
    }
    if let Some(f) = file_path {
        args.push(f.to_string());
        let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = run_termux_command("termux-share", &str_args).await?;
        return parse_termux_output("termux-share", &output);
    }

    // Share text via stdin
    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let stdin_text = text.unwrap_or("");
    let output = run_termux_command_with_stdin("termux-share", &str_args, stdin_text).await?;
    info!("📤 Shared content");
    parse_termux_output("termux-share", &output)
}

async fn action_sms_list(params: &Value) -> Result<Value> {
    let sms_type = params.get("sms_type").and_then(|v| v.as_str()).unwrap_or("inbox");
    let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);
    let offset = params.get("offset").and_then(|v| v.as_i64()).unwrap_or(0);

    let limit_str = limit.to_string();
    let offset_str = offset.to_string();
    let output = run_termux_command("termux-sms-list", &[
        "-d", "-n",
        "-t", sms_type,
        "-l", &limit_str,
        "-o", &offset_str,
    ]).await?;
    parse_termux_output("termux-sms-list", &output)
}

async fn action_sms_send(params: &Value) -> Result<Value> {
    let number = params.get("number").and_then(|v| v.as_str()).unwrap_or("");
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");

    let output = run_termux_command_with_stdin("termux-sms-send", &["-n", number], text).await?;
    info!(to = number, "📱 SMS sent");
    parse_termux_output("termux-sms-send", &output)
}

async fn action_storage_get(params: &Value) -> Result<Value> {
    let output_path = params.get("output_path").and_then(|v| v.as_str()).unwrap_or("/tmp/termux_storage_file");
    let output = run_termux_command("termux-storage-get", &[output_path]).await?;
    let mut result = parse_termux_output("termux-storage-get", &output)?;
    result["output_path"] = json!(output_path);
    Ok(result)
}

async fn action_telephony_call(params: &Value) -> Result<Value> {
    let number = params.get("number").and_then(|v| v.as_str()).unwrap_or("");
    let output = run_termux_command("termux-telephony-call", &[number]).await?;
    info!(number = number, "📞 Calling");
    parse_termux_output("termux-telephony-call", &output)
}

async fn action_toast(params: &Value) -> Result<Value> {
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("Hello from blockcell!");
    let bg_color = params.get("toast_bg_color").and_then(|v| v.as_str());
    let text_color = params.get("toast_text_color").and_then(|v| v.as_str());
    let position = params.get("toast_position").and_then(|v| v.as_str());
    let short = params.get("toast_short").and_then(|v| v.as_bool()).unwrap_or(false);

    let mut args: Vec<String> = Vec::new();

    if let Some(bg) = bg_color {
        args.push("-b".to_string());
        args.push(bg.to_string());
    }
    if let Some(tc) = text_color {
        args.push("-c".to_string());
        args.push(tc.to_string());
    }
    if let Some(pos) = position {
        args.push("-g".to_string());
        args.push(pos.to_string());
    }
    if short {
        args.push("-s".to_string());
    }
    args.push(text.to_string());

    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = run_termux_command("termux-toast", &str_args).await?;
    info!("🍞 Toast shown");
    parse_termux_output("termux-toast", &output)
}

async fn action_torch(params: &Value) -> Result<Value> {
    let enabled = params.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let arg = if enabled { "on" } else { "off" };
    let output = run_termux_command("termux-torch", &[arg]).await?;
    info!(state = arg, "🔦 Torch");
    parse_termux_output("termux-torch", &output)
}

async fn action_tts_speak(params: &Value) -> Result<Value> {
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let engine = params.get("tts_engine").and_then(|v| v.as_str());
    let language = params.get("tts_language").and_then(|v| v.as_str());
    let pitch = params.get("tts_pitch").and_then(|v| v.as_f64());
    let rate = params.get("tts_rate").and_then(|v| v.as_f64());
    let stream = params.get("stream").and_then(|v| v.as_str());

    let mut args: Vec<String> = Vec::new();

    if let Some(e) = engine {
        args.push("-e".to_string());
        args.push(e.to_string());
    }
    if let Some(l) = language {
        args.push("-l".to_string());
        args.push(l.to_string());
    }
    if let Some(p) = pitch {
        args.push("-p".to_string());
        args.push(p.to_string());
    }
    if let Some(r) = rate {
        args.push("-r".to_string());
        args.push(r.to_string());
    }
    if let Some(s) = stream {
        args.push("-s".to_string());
        args.push(s.to_string());
    }

    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = run_termux_command_with_stdin("termux-tts-speak", &str_args, text).await?;
    info!("🗣️ TTS speaking ({} chars)", text.len());
    parse_termux_output("termux-tts-speak", &output)
}

async fn action_vibrate(params: &Value) -> Result<Value> {
    let duration = params.get("duration").and_then(|v| v.as_i64()).unwrap_or(1000);
    let force = params.get("force").and_then(|v| v.as_bool()).unwrap_or(false);

    let duration_str = duration.to_string();
    let mut args: Vec<&str> = vec!["-d", &duration_str];
    if force {
        args.push("-f");
    }

    let output = run_termux_command("termux-vibrate", &args).await?;
    info!(duration = duration, "📳 Vibrate");
    parse_termux_output("termux-vibrate", &output)
}

async fn action_volume(params: &Value) -> Result<Value> {
    let stream = params.get("stream").and_then(|v| v.as_str());
    let volume_value = params.get("volume_value").and_then(|v| v.as_i64());

    match (stream, volume_value) {
        (Some(s), Some(v)) => {
            let vol_str = v.to_string();
            let output = run_termux_command("termux-volume", &[s, &vol_str]).await?;
            info!(stream = s, volume = v, "🔊 Volume set");
            parse_termux_output("termux-volume", &output)
        }
        _ => {
            // No args: show volume info for all streams
            let output = run_termux_command("termux-volume", &[]).await?;
            parse_termux_output("termux-volume", &output)
        }
    }
}

async fn action_wallpaper(params: &Value) -> Result<Value> {
    let file_path = params.get("file_path").and_then(|v| v.as_str());
    let url = params.get("url").and_then(|v| v.as_str());
    let lockscreen = params.get("wallpaper_lockscreen").and_then(|v| v.as_bool()).unwrap_or(false);

    let mut args: Vec<String> = Vec::new();

    if lockscreen {
        args.push("-l".to_string());
    }

    if let Some(f) = file_path {
        args.push("-f".to_string());
        args.push(f.to_string());
    } else if let Some(u) = url {
        args.push("-u".to_string());
        args.push(u.to_string());
    } else {
        return Err(Error::Tool("Either 'file_path' or 'url' is required for wallpaper".into()));
    }

    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = run_termux_command("termux-wallpaper", &str_args).await?;
    info!("🖼️ Wallpaper set");
    parse_termux_output("termux-wallpaper", &output)
}

async fn action_wifi_enable(params: &Value) -> Result<Value> {
    let enabled = params.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let arg = if enabled { "true" } else { "false" };
    let output = run_termux_command("termux-wifi-enable", &[arg]).await?;
    info!(enabled = enabled, "📶 WiFi");
    parse_termux_output("termux-wifi-enable", &output)
}

async fn action_job_scheduler(params: &Value) -> Result<Value> {
    let list_pending = params.get("job_list_pending").and_then(|v| v.as_bool()).unwrap_or(false);

    if list_pending {
        let output = run_termux_command("termux-job-scheduler", &["--pending", "true"]).await?;
        return parse_termux_output("termux-job-scheduler", &output);
    }

    let script = params.get("job_script").and_then(|v| v.as_str());
    let job_id = params.get("job_id").and_then(|v| v.as_i64());
    let period_ms = params.get("job_period_ms").and_then(|v| v.as_i64());
    let network = params.get("job_network").and_then(|v| v.as_str());
    let charging = params.get("job_charging").and_then(|v| v.as_bool());

    let mut args: Vec<String> = Vec::new();

    if let Some(s) = script {
        args.push("--script".to_string());
        args.push(s.to_string());
    }
    if let Some(id) = job_id {
        args.push("--job-id".to_string());
        args.push(id.to_string());
    }
    if let Some(p) = period_ms {
        args.push("--period-ms".to_string());
        args.push(p.to_string());
    }
    if let Some(n) = network {
        args.push("--network".to_string());
        args.push(n.to_string());
    }
    if let Some(c) = charging {
        args.push("--charging".to_string());
        args.push(c.to_string());
    }

    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let output = run_termux_command("termux-job-scheduler", &str_args).await?;
    info!("⏰ Job scheduled");
    parse_termux_output("termux-job-scheduler", &output)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_termux_api_tool_schema() {
        let tool = TermuxApiTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "termux_api");
        assert!(schema.description.contains("Termux"));
        // Verify the schema has action enum
        let params = &schema.parameters;
        let action_enum = params["properties"]["action"]["enum"].as_array().unwrap();
        assert!(action_enum.len() >= 40);
    }

    #[test]
    fn test_termux_api_validate_valid_actions() {
        let tool = TermuxApiTool;
        assert!(tool.validate(&json!({"action": "battery_status"})).is_ok());
        assert!(tool.validate(&json!({"action": "camera_info"})).is_ok());
        assert!(tool.validate(&json!({"action": "location"})).is_ok());
        assert!(tool.validate(&json!({"action": "sms_list"})).is_ok());
        assert!(tool.validate(&json!({"action": "info"})).is_ok());
        assert!(tool.validate(&json!({"action": "toast"})).is_ok());
        assert!(tool.validate(&json!({"action": "vibrate"})).is_ok());
        assert!(tool.validate(&json!({"action": "wifi_connectioninfo"})).is_ok());
    }

    #[test]
    fn test_termux_api_validate_invalid_action() {
        let tool = TermuxApiTool;
        assert!(tool.validate(&json!({"action": "nonexistent"})).is_err());
        assert!(tool.validate(&json!({"action": ""})).is_err());
    }

    #[test]
    fn test_termux_api_validate_sms_send_requires_number_and_text() {
        let tool = TermuxApiTool;
        assert!(tool.validate(&json!({"action": "sms_send"})).is_err());
        assert!(tool.validate(&json!({"action": "sms_send", "number": "123"})).is_err());
        assert!(tool.validate(&json!({"action": "sms_send", "text": "hello"})).is_err());
        assert!(tool.validate(&json!({"action": "sms_send", "number": "123", "text": "hello"})).is_ok());
    }

    #[test]
    fn test_termux_api_validate_telephony_call_requires_number() {
        let tool = TermuxApiTool;
        assert!(tool.validate(&json!({"action": "telephony_call"})).is_err());
        assert!(tool.validate(&json!({"action": "telephony_call", "number": "123"})).is_ok());
    }

    #[test]
    fn test_termux_api_validate_clipboard_set_requires_text() {
        let tool = TermuxApiTool;
        assert!(tool.validate(&json!({"action": "clipboard_set"})).is_err());
        assert!(tool.validate(&json!({"action": "clipboard_set", "text": "hello"})).is_ok());
    }

    #[test]
    fn test_termux_api_validate_tts_speak_requires_text() {
        let tool = TermuxApiTool;
        assert!(tool.validate(&json!({"action": "tts_speak"})).is_err());
        assert!(tool.validate(&json!({"action": "tts_speak", "text": "hello"})).is_ok());
    }

    #[test]
    fn test_termux_api_validate_open_url_requires_url() {
        let tool = TermuxApiTool;
        assert!(tool.validate(&json!({"action": "open_url"})).is_err());
        assert!(tool.validate(&json!({"action": "open_url", "url": "https://example.com"})).is_ok());
    }

    #[test]
    fn test_termux_api_validate_infrared_transmit_requires_params() {
        let tool = TermuxApiTool;
        assert!(tool.validate(&json!({"action": "infrared_transmit"})).is_err());
        assert!(tool.validate(&json!({"action": "infrared_transmit", "frequency": 38000})).is_err());
        assert!(tool.validate(&json!({"action": "infrared_transmit", "frequency": 38000, "ir_pattern": "20,50,20,30"})).is_ok());
    }

    #[test]
    fn test_termux_api_validate_notification_remove_requires_id() {
        let tool = TermuxApiTool;
        assert!(tool.validate(&json!({"action": "notification_remove"})).is_err());
        assert!(tool.validate(&json!({"action": "notification_remove", "notification_id": "my-notif"})).is_ok());
    }

    #[test]
    fn test_parse_termux_output_json() {
        let output = std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: b"{\"percentage\":85,\"status\":\"CHARGING\"}".to_vec(),
            stderr: Vec::new(),
        };
        let result = parse_termux_output("termux-battery-status", &output);
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["action"], "battery-status");
        assert_eq!(val["result"]["percentage"], 85);
    }

    #[test]
    fn test_parse_termux_output_text() {
        let output = std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: b"Hello World".to_vec(),
            stderr: Vec::new(),
        };
        let result = parse_termux_output("termux-clipboard-get", &output);
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["action"], "clipboard-get");
        assert_eq!(val["output"], "Hello World");
    }

    #[test]
    fn test_parse_termux_output_empty() {
        let output = std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        let result = parse_termux_output("termux-vibrate", &output);
        assert!(result.is_ok());
        let val = result.unwrap();
        assert_eq!(val["output"], "OK");
    }
}
