use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolSchema};

/// Tool: system_info — 探测系统硬件和软件环境
///
/// 让 agent 感知自身运行环境。
pub struct SystemInfoTool;

#[async_trait]
impl Tool for SystemInfoTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "system_info",
            description: "Probe system hardware, software, network, and available tools. Use this to discover what the agent can do on this machine.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "category": {
                        "type": "string",
                        "description": "Category to probe: 'all', 'hardware', 'software', 'network', 'tools'",
                        "enum": ["all", "hardware", "software", "network", "tools"]
                    }
                },
                "required": ["category"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        params.get("category")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: category".to_string()))?;
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let category = params["category"].as_str().unwrap_or("all");

        let mut result = json!({});

        if category == "all" || category == "hardware" {
            result["hardware"] = probe_hardware().await;
        }
        if category == "all" || category == "software" {
            result["software"] = probe_software().await;
        }
        if category == "all" || category == "network" {
            result["network"] = probe_network().await;
        }
        if category == "all" || category == "tools" {
            result["tools"] = probe_tools(&ctx).await;
        }

        Ok(result)
    }
}

/// 探测硬件信息
async fn probe_hardware() -> Value {
    let mut hw = json!({});

    // OS info
    hw["os"] = json!({
        "name": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "family": std::env::consts::FAMILY,
    });

    // CPU count
    hw["cpu_cores"] = json!(std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1));

    // Disk space (workspace)
    if let Ok(output) = run_command("df", &["-h", "."]).await {
        hw["disk"] = json!(output);
    }

    // Memory (macOS: vm_stat, Linux: /proc/meminfo)
    if cfg!(target_os = "macos") {
        if let Ok(output) = run_command("sysctl", &["-n", "hw.memsize"]).await {
            if let Ok(bytes) = output.trim().parse::<u64>() {
                hw["memory_total_gb"] = json!(format!("{:.1}", bytes as f64 / 1_073_741_824.0));
            }
        }
    } else if cfg!(target_os = "linux") {
        if let Ok(output) = run_command("grep", &["MemTotal", "/proc/meminfo"]).await {
            hw["memory"] = json!(output.trim());
        }
    }

    // GPU detection
    let gpu = detect_gpu().await;
    if !gpu.is_empty() {
        hw["gpu"] = json!(gpu);
    }

    // Camera detection
    let camera = detect_camera().await;
    hw["camera"] = json!(camera);

    // Microphone detection
    let mic = detect_microphone().await;
    hw["microphone"] = json!(mic);

    // USB devices
    if let Ok(output) = run_command_optional("system_profiler", &["SPUSBDataType", "-detailLevel", "mini"]).await {
        hw["usb_summary"] = json!(truncate_output(&output, 500));
    }

    // Bluetooth
    let bt = detect_bluetooth().await;
    hw["bluetooth"] = json!(bt);

    hw
}

/// 探测软件环境
async fn probe_software() -> Value {
    let mut sw = json!({});

    // Rust toolchain
    sw["rustc"] = json!(check_binary_version("rustc", &["--version"]).await);
    sw["cargo"] = json!(check_binary_version("cargo", &["--version"]).await);

    // Python
    sw["python3"] = json!(check_binary_version("python3", &["--version"]).await);

    // Node.js
    sw["node"] = json!(check_binary_version("node", &["--version"]).await);

    // Git
    sw["git"] = json!(check_binary_version("git", &["--version"]).await);

    // Docker
    sw["docker"] = json!(check_binary_version("docker", &["--version"]).await);

    // FFmpeg (audio/video processing)
    sw["ffmpeg"] = json!(check_binary_version("ffmpeg", &["-version"]).await);

    // ImageMagick
    sw["convert"] = json!(check_binary_version("convert", &["--version"]).await);

    // Chrome/Chromium (for headless browsing)
    let chrome = detect_chrome().await;
    sw["chrome"] = json!(chrome);

    // Shell
    sw["shell"] = json!(std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_string()));

    // Package managers
    sw["brew"] = json!(check_binary_exists("brew").await);
    sw["apt"] = json!(check_binary_exists("apt").await);
    sw["pip3"] = json!(check_binary_exists("pip3").await);
    sw["npm"] = json!(check_binary_exists("npm").await);

    sw
}

/// 探测网络环境
async fn probe_network() -> Value {
    let mut net = json!({});

    // Basic connectivity check
    let online = check_internet().await;
    net["internet"] = json!(online);

    // Hostname
    if let Ok(output) = run_command("hostname", &[]).await {
        net["hostname"] = json!(output.trim());
    }

    // Network interfaces (brief)
    if cfg!(target_os = "macos") {
        if let Ok(output) = run_command("ifconfig", &["-l"]).await {
            net["interfaces"] = json!(output.trim());
        }
    } else {
        if let Ok(output) = run_command("ip", &["-brief", "addr"]).await {
            net["interfaces"] = json!(truncate_output(&output, 500));
        }
    }

    net
}

/// 探测已注册的工具和能力
async fn probe_tools(ctx: &ToolContext) -> Value {
    let mut caps = json!({});

    // Check what the agent can currently do
    let mut abilities: Vec<String> = Vec::new();

    // File system
    abilities.push("fs.read — Read files".to_string());
    abilities.push("fs.write — Write files".to_string());
    abilities.push("fs.list — List directories".to_string());

    // Execution
    abilities.push("exec.shell — Execute shell commands".to_string());

    // Web
    abilities.push("web.search — Web search".to_string());
    abilities.push("web.fetch — Fetch web pages".to_string());
    abilities.push("web.browse — Headless browser".to_string());

    // Memory
    abilities.push("memory.query — Query memory store".to_string());
    abilities.push("memory.upsert — Save to memory".to_string());

    // Communication
    abilities.push("comm.message — Send messages".to_string());
    abilities.push("comm.spawn — Spawn subagents".to_string());

    // Compilation (check if rustc is available)
    if which::which("rustc").is_ok() {
        abilities.push("compile.rust — Compile Rust code (rustc available)".to_string());
    }
    if which::which("python3").is_ok() {
        abilities.push("script.python — Run Python scripts".to_string());
    }
    if which::which("node").is_ok() {
        abilities.push("script.node — Run Node.js scripts".to_string());
    }

    // Hardware detection
    if detect_camera().await {
        abilities.push("hardware.camera — Camera available".to_string());
    }
    if detect_microphone().await {
        abilities.push("hardware.microphone — Microphone available".to_string());
    }
    if !detect_gpu().await.is_empty() {
        abilities.push("hardware.gpu — GPU available".to_string());
    }

    caps["current_abilities"] = json!(abilities);
    caps["workspace"] = json!(ctx.workspace.display().to_string());

    // Survival invariants check
    let can_compile = which::which("rustc").is_ok();
    let can_communicate = true; // We're running, so we can communicate
    let can_evolve = can_compile || which::which("bash").is_ok();

    caps["survival_invariants"] = json!({
        "can_compile": can_compile,
        "can_load_tools": true,
        "can_communicate": can_communicate,
        "can_evolve": can_evolve,
    });

    caps
}

// === Helper functions ===

async fn run_command(cmd: &str, args: &[&str]) -> std::result::Result<String, ()> {
    let output = tokio::process::Command::new(cmd)
        .args(args)
        .output()
        .await
        .map_err(|_| ())?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(())
    }
}

async fn run_command_optional(cmd: &str, args: &[&str]) -> std::result::Result<String, ()> {
    if which::which(cmd).is_err() {
        return Err(());
    }
    run_command(cmd, args).await
}

async fn check_binary_version(cmd: &str, args: &[&str]) -> Value {
    if which::which(cmd).is_err() {
        return json!({"installed": false});
    }
    match run_command(cmd, args).await {
        Ok(output) => {
            let version = output.lines().next().unwrap_or("").trim().to_string();
            json!({"installed": true, "version": version})
        }
        Err(_) => json!({"installed": true, "version": "unknown"}),
    }
}

async fn check_binary_exists(cmd: &str) -> bool {
    which::which(cmd).is_ok()
}

async fn detect_gpu() -> Vec<String> {
    let mut gpus = Vec::new();

    if cfg!(target_os = "macos") {
        if let Ok(output) = run_command("system_profiler", &["SPDisplaysDataType"]).await {
            for line in output.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("Chipset Model:") || trimmed.starts_with("Chip:") {
                    gpus.push(trimmed.to_string());
                }
            }
        }
    } else if cfg!(target_os = "linux") {
        if let Ok(output) = run_command("lspci", &[]).await {
            for line in output.lines() {
                if line.contains("VGA") || line.contains("3D") || line.contains("Display") {
                    gpus.push(line.trim().to_string());
                }
            }
        }
    }

    gpus
}

async fn detect_camera() -> bool {
    if cfg!(target_os = "macos") {
        // Check if any camera device exists
        if let Ok(output) = run_command("system_profiler", &["SPCameraDataType"]).await {
            return output.contains("FaceTime") || output.contains("Camera") || output.contains("camera");
        }
    } else if cfg!(target_os = "linux") {
        // Check for /dev/video* devices
        if let Ok(entries) = std::fs::read_dir("/dev") {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with("video") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

async fn detect_microphone() -> bool {
    if cfg!(target_os = "macos") {
        if let Ok(output) = run_command("system_profiler", &["SPAudioDataType"]).await {
            return output.contains("Input") || output.contains("Microphone") || output.contains("microphone");
        }
    } else if cfg!(target_os = "linux") {
        // Check for ALSA capture devices
        if let Ok(output) = run_command("arecord", &["-l"]).await {
            return output.contains("card");
        }
    }
    false
}

async fn detect_bluetooth() -> bool {
    if cfg!(target_os = "macos") {
        if let Ok(output) = run_command("system_profiler", &["SPBluetoothDataType"]).await {
            return output.contains("Bluetooth");
        }
    } else if cfg!(target_os = "linux") {
        return which::which("bluetoothctl").is_ok();
    }
    false
}

async fn detect_chrome() -> Value {
    let candidates = if cfg!(target_os = "macos") {
        vec![
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ]
    } else {
        vec!["google-chrome", "chromium-browser", "chromium"]
    };

    for candidate in candidates {
        if std::path::Path::new(candidate).exists() || which::which(candidate).is_ok() {
            return json!({"installed": true, "path": candidate});
        }
    }
    json!({"installed": false})
}

async fn check_internet() -> bool {
    // Quick DNS check
    match tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::net::TcpStream::connect("1.1.1.1:53"),
    ).await {
        Ok(Ok(_)) => true,
        _ => false,
    }
}

fn truncate_output(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}... (truncated)", truncated)
    }
}

/// Tool: capability_evolve — 请求进化新的核心能力
///
/// 让 agent 能够主动请求学习新能力（如操作硬件、调用新 API 等）
pub struct CapabilityEvolveTool;

#[async_trait]
impl Tool for CapabilityEvolveTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "capability_evolve",
            description: "Manage dynamic evolved tools: request new ones, execute existing ones, check status, or list all. Evolved tools are generated, compiled, validated, and hot-loaded at runtime.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "Action: 'request' to evolve a new tool, 'execute' to invoke an existing evolved tool, 'status' to check evolution status, 'list' to list all evolved tools",
                        "enum": ["request", "execute", "status", "list"]
                    },
                    "input": {
                        "type": "object",
                        "description": "Input JSON to pass to the evolved tool when action='execute'. The schema depends on the specific tool."
                    },
                    "capability_id": {
                        "type": "string",
                        "description": "Tool ID in format 'category.name' (e.g. 'hardware.camera_capture', 'system.clipboard', 'api.weather'). Required for 'request' and 'status'."
                    },
                    "description": {
                        "type": "string",
                        "description": "Description of what the tool should do. Required for 'request'."
                    },
                    "provider_type": {
                        "type": "string",
                        "description": "How to implement: 'script' (bash), 'python' (Python script), 'process' (standalone process). Default: 'script'.",
                        "enum": ["script", "python", "process"]
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: action".to_string()))?;

        if action == "request" {
            params.get("capability_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Validation("Missing required parameter: capability_id for 'request' action".to_string()))?;
            params.get("description")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Validation("Missing required parameter: description for 'request' action".to_string()))?;
        }

        if action == "execute" {
            params.get("capability_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Validation("Missing required parameter: capability_id for 'execute' action".to_string()))?;
        }

        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap();

        match action {
            "list" => {
                // List evolved tools from the registry
                if let Some(ref registry_handle) = ctx.capability_registry {
                    let registry = registry_handle.lock().await;
                    let caps = registry.list_all_json().await;
                    let stats = registry.stats_json().await;

                    Ok(json!({
                        "tools": caps,
                        "stats": stats,
                    }))
                } else {
                    Ok(json!({
                        "tools": [],
                        "note": "Tool evolution registry not initialized"
                    }))
                }
            }
            "status" => {
                let cap_id = params["capability_id"].as_str().unwrap_or("");
                if let Some(ref registry_handle) = ctx.capability_registry {
                    let registry = registry_handle.lock().await;
                    if let Some(desc) = registry.get_descriptor_json(cap_id).await {
                        Ok(desc)
                    } else {
                        Ok(json!({"error": format!("Evolved tool '{}' not found", cap_id)}))
                    }
                } else {
                    Ok(json!({"error": "Tool evolution registry not initialized"}))
                }
            }
            "execute" => {
                let cap_id = params["capability_id"].as_str().unwrap();
                let input = params.get("input").cloned().unwrap_or(json!({}));

                if let Some(ref registry_handle) = ctx.capability_registry {
                    let registry = registry_handle.lock().await;
                    match registry.execute_capability(cap_id, input).await {
                        Ok(output) => Ok(json!({
                            "capability_id": cap_id,
                            "status": "success",
                            "output": output
                        })),
                        Err(e) => Ok(json!({
                            "capability_id": cap_id,
                            "status": "error",
                            "error": format!("{}", e)
                        }))
                    }
                } else {
                    Ok(json!({"error": "Tool evolution registry not initialized"}))
                }
            }
            "request" => {
                let cap_id = params["capability_id"].as_str().unwrap();
                let description = params["description"].as_str().unwrap();
                let provider_type = params.get("provider_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("script");

                if let Some(ref core_evo_handle) = ctx.core_evolution {
                    let core_evo = core_evo_handle.lock().await;
                    match core_evo.request_capability(cap_id, description, provider_type).await {
                        Ok(result) => Ok(result),
                        Err(e) => {
                            Ok(json!({
                                "error": format!("Failed to request tool evolution: {}", e)
                            }))
                        }
                    }
                } else {
                    Ok(json!({
                        "error": "Core evolution engine not initialized"
                    }))
                }
            }
            _ => {
                Ok(json!({"error": format!("Unknown action: {}", action)}))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_system_info_schema() {
        let tool = SystemInfoTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "system_info");
    }

    #[test]
    fn test_system_info_validate() {
        let tool = SystemInfoTool;
        assert!(tool.validate(&json!({"category": "all"})).is_ok());
        assert!(tool.validate(&json!({"category": "hardware"})).is_ok());
        assert!(tool.validate(&json!({})).is_err());
    }

    #[test]
    fn test_capability_evolve_schema() {
        let tool = CapabilityEvolveTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "capability_evolve");
    }

    #[test]
    fn test_capability_evolve_validate() {
        let tool = CapabilityEvolveTool;
        assert!(tool.validate(&json!({"action": "list"})).is_ok());
        assert!(tool.validate(&json!({"action": "status", "capability_id": "test.tool"})).is_ok());
        assert!(tool.validate(&json!({"action": "request", "capability_id": "test.tool", "description": "do stuff"})).is_ok());
        assert!(tool.validate(&json!({"action": "request"})).is_err());
        assert!(tool.validate(&json!({"action": "execute"})).is_err());
        assert!(tool.validate(&json!({})).is_err());
    }

    #[test]
    fn test_truncate_output() {
        assert_eq!(truncate_output("hello", 10), "hello");
        assert!(truncate_output("hello world this is long", 10).contains("..."));
    }
}
