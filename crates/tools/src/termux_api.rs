use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::Value;

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
            name: "termux_api".to_string(),
            description: "Control Android device via Termux API. Requires termux-api package and Termux:API app. \
                Actions: battery_status, brightness, camera_info, camera_photo, clipboard_get, clipboard_set, \
                contact_list, call_log, dialog, download, fingerprint, infrared_frequencies, infrared_transmit, \
                keystore, location, media_player, media_scan, microphone_record, notification, notification_remove, \
                open, open_url, sensor, share, sms_list, sms_send, speech_to_text, storage_get, \
                telephony_deviceinfo, telephony_cellinfo, telephony_call, toast, torch, tts_engines, tts_speak, \
                vibrate, volume, wallpaper, wifi_connectioninfo, wifi_scaninfo, wifi_enable, \
                audio_info, wake_lock, wake_unlock, job_scheduler, info".to_string(),
            parameters: build_schema(),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some("- **Termux API (Android)**: Use `termux_api` tool to control Android devices via Termux. Requires `termux-api` package + Termux:API app. Use action='info' to check availability. Covers: battery, camera, clipboard, contacts, SMS, calls, location, sensors, notifications, TTS, speech-to-text, media player, microphone, torch, brightness, volume, WiFi, vibrate, share, dialog, wallpaper, fingerprint, infrared, keystore, job scheduler, wake lock. Only available when running on Android/Termux.".to_string())
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid_actions = [
            "battery_status",
            "brightness",
            "camera_info",
            "camera_photo",
            "clipboard_get",
            "clipboard_set",
            "contact_list",
            "call_log",
            "dialog",
            "download",
            "fingerprint",
            "infrared_frequencies",
            "infrared_transmit",
            "keystore",
            "location",
            "media_player",
            "media_scan",
            "microphone_record",
            "notification",
            "notification_remove",
            "open",
            "open_url",
            "sensor",
            "share",
            "sms_list",
            "sms_send",
            "speech_to_text",
            "storage_get",
            "telephony_deviceinfo",
            "telephony_cellinfo",
            "telephony_call",
            "toast",
            "torch",
            "tts_engines",
            "tts_speak",
            "vibrate",
            "volume",
            "wallpaper",
            "wifi_connectioninfo",
            "wifi_scaninfo",
            "wifi_enable",
            "audio_info",
            "wake_lock",
            "wake_unlock",
            "job_scheduler",
            "info",
        ];
        if !valid_actions.contains(&action) {
            return Err(Error::Tool(format!(
                "Invalid action '{}'. Valid actions: {}",
                action,
                valid_actions.join(", ")
            )));
        }

        // Validate required params per action
        match action {
            "sms_send" => {
                if params
                    .get("number")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
                {
                    return Err(Error::Tool("'number' is required for sms_send".into()));
                }
                if params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
                {
                    return Err(Error::Tool("'text' is required for sms_send".into()));
                }
            }
            "telephony_call"
                if params
                    .get("number")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty() =>
            {
                return Err(Error::Tool(
                    "'number' is required for telephony_call".into(),
                ));
            }
            "clipboard_set"
                if params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty() =>
            {
                return Err(Error::Tool("'text' is required for clipboard_set".into()));
            }
            "tts_speak"
                if params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty() =>
            {
                return Err(Error::Tool("'text' is required for tts_speak".into()));
            }
            "open_url"
                if params
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty() =>
            {
                return Err(Error::Tool("'url' is required for open_url".into()));
            }
            "notification_remove"
                if params
                    .get("notification_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty() =>
            {
                return Err(Error::Tool(
                    "'notification_id' is required for notification_remove".into(),
                ));
            }
            "infrared_transmit" => {
                if params.get("frequency").is_none() {
                    return Err(Error::Tool(
                        "'frequency' is required for infrared_transmit".into(),
                    ));
                }
                if params
                    .get("ir_pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
                {
                    return Err(Error::Tool(
                        "'ir_pattern' is required for infrared_transmit".into(),
                    ));
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn requires_confirmation(&self, params: &Value) -> bool {
        // Actions with irreversible or privacy-sensitive device side effects.
        // The rest of the Termux:API surface (battery info, sensors, toast, …) is
        // read-only or cosmetic and stays confirmation-free.
        matches!(
            params
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("info"),
            "sms_send"
                | "telephony_call"
                | "clipboard_set"
                | "download"
                | "infrared_transmit"
                | "keystore"
                | "wifi_enable"
                // Privacy-sensitive sensors: these capture camera/microphone/
                // location data and were previously missing from the gate.
                | "camera_photo"
                | "microphone_record"
                | "location"
        )
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("info");

        // First check if we're running on Termux
        if action != "info" && !is_termux_available().await {
            return Err(Error::Tool(
                "Termux API is not available. Make sure you are running on Android with \
                 'termux-api' package installed (pkg install termux-api) and the Termux:API \
                 companion app is installed."
                    .into(),
            ));
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

mod actions;
mod schema;

use actions::*;
use schema::build_schema;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn test_privacy_sensor_actions_require_confirmation() {
        let tool = TermuxApiTool;
        for action in ["camera_photo", "microphone_record", "location", "sms_send"] {
            assert!(
                tool.requires_confirmation(&json!({ "action": action })),
                "{action} must require explicit confirmation"
            );
        }
        for action in ["battery_status", "camera_info", "sensor"] {
            assert!(
                !tool.requires_confirmation(&json!({ "action": action })),
                "{action} is read-only and must not require confirmation"
            );
        }
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
        assert!(tool
            .validate(&json!({"action": "wifi_connectioninfo"}))
            .is_ok());
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
        assert!(tool
            .validate(&json!({"action": "sms_send", "number": "123"}))
            .is_err());
        assert!(tool
            .validate(&json!({"action": "sms_send", "text": "hello"}))
            .is_err());
        assert!(tool
            .validate(&json!({"action": "sms_send", "number": "123", "text": "hello"}))
            .is_ok());
    }

    #[test]
    fn test_termux_api_validate_telephony_call_requires_number() {
        let tool = TermuxApiTool;
        assert!(tool.validate(&json!({"action": "telephony_call"})).is_err());
        assert!(tool
            .validate(&json!({"action": "telephony_call", "number": "123"}))
            .is_ok());
    }

    #[test]
    fn test_termux_api_validate_clipboard_set_requires_text() {
        let tool = TermuxApiTool;
        assert!(tool.validate(&json!({"action": "clipboard_set"})).is_err());
        assert!(tool
            .validate(&json!({"action": "clipboard_set", "text": "hello"}))
            .is_ok());
    }

    #[test]
    fn test_termux_api_validate_tts_speak_requires_text() {
        let tool = TermuxApiTool;
        assert!(tool.validate(&json!({"action": "tts_speak"})).is_err());
        assert!(tool
            .validate(&json!({"action": "tts_speak", "text": "hello"}))
            .is_ok());
    }

    #[test]
    fn test_termux_api_validate_open_url_requires_url() {
        let tool = TermuxApiTool;
        assert!(tool.validate(&json!({"action": "open_url"})).is_err());
        assert!(tool
            .validate(&json!({"action": "open_url", "url": "https://example.com"}))
            .is_ok());
    }

    #[test]
    fn test_termux_api_validate_infrared_transmit_requires_params() {
        let tool = TermuxApiTool;
        assert!(tool
            .validate(&json!({"action": "infrared_transmit"}))
            .is_err());
        assert!(tool
            .validate(&json!({"action": "infrared_transmit", "frequency": 38000}))
            .is_err());
        assert!(tool.validate(&json!({"action": "infrared_transmit", "frequency": 38000, "ir_pattern": "20,50,20,30"})).is_ok());
    }

    #[test]
    fn test_termux_api_validate_notification_remove_requires_id() {
        let tool = TermuxApiTool;
        assert!(tool
            .validate(&json!({"action": "notification_remove"}))
            .is_err());
        assert!(tool
            .validate(&json!({"action": "notification_remove", "notification_id": "my-notif"}))
            .is_ok());
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
