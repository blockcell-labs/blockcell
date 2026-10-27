use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

use crate::budget::BudgetConfig;
use crate::error::Result;
use crate::paths::Paths;

// 子模块 — 按职责拆分的配置类型
pub mod channels;
pub use channels::*;
pub mod intent;
pub use intent::*;
pub mod memory;
pub use memory::*;

pub mod agent;
pub use agent::*;
pub mod gateway;
pub use gateway::*;
pub mod ghost;
pub use ghost::*;
pub mod log;
pub use log::*;
pub mod provider;
pub use provider::*;
pub mod tools;
pub use tools::*;
pub mod upgrade;
pub use upgrade::*;

/// 多个配置类型共用的布尔默认值助手（保留在父模块，子模块经 `super::` 引用）。
pub(crate) use crate::default_true;

fn default_memory_vector_table() -> String {
    "memory_vectors".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub budget: BudgetConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default)]
    pub community_hub: CommunityHubConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub channels: ChannelsConfig,
    /// Simplified multi-agent routing table: channel -> owner agent id.
    #[serde(default)]
    pub channel_owners: HashMap<String, String>,
    /// Account-level routing overrides: channel -> account_id -> owner agent id.
    #[serde(default)]
    pub channel_account_owners: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub gateway: GatewayConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(
        default = "intent::default_intent_router_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub intent_router: Option<IntentRouterConfig>,
    #[serde(default)]
    pub auto_upgrade: AutoUpgradeConfig,
    /// 日志配置（等级、输出方式）
    #[serde(default)]
    pub log: LogConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    /// Default timezone for cron jobs and time-related operations.
    /// IANA timezone name, e.g., "Asia/Shanghai", "America/New_York", "Europe/London".
    /// If not set, system timezone is detected, falling back to UTC.
    #[serde(default)]
    pub default_timezone: Option<String>,
    /// Cron service tick interval in seconds. Default: 1 second. Min: 1. Max: 3600.
    /// Higher values reduce CPU/disk I/O but lower time precision.
    #[serde(default = "default_cron_tick_interval")]
    pub cron_tick_interval_secs: u64,
    /// 是否启用 OpenClaw skill 兼容加载（默认 false）
    #[serde(default)]
    pub openclaw_skill_enabled: bool,
    /// Self-Improve 配置 (Nudge + Review)
    #[serde(default)]
    pub self_improve: SelfImproveConfig,
    /// 进化服务配置（错误阈值、冷却期等）
    #[serde(default)]
    pub evolution: EvolutionConfig,
}

fn default_cron_tick_interval() -> u64 {
    1
}

/// Placeholder API key used by providers that do not require authentication
/// (currently the local `vllm` preset). Treated as "not configured" everywhere
/// a real credential is required, including [`Config::get_api_key`].
pub const PLACEHOLDER_API_KEY: &str = "dummy";

/// Returns true for API keys that only exist to satisfy a provider client's
/// non-empty check and carry no credential (local `vllm`/`ollama` presets).
///
/// CLI commands historically hard-coded `api_key != "dummy"`; keeping the rule
/// in one place prevents the display/doctor/status views from disagreeing with
/// [`Config::get_api_key`].
pub fn is_placeholder_api_key(key: &str) -> bool {
    matches!(key.trim(), "" | PLACEHOLDER_API_KEY | "ollama")
}

/// Minimum allowed cron tick interval in seconds.
const MIN_CRON_TICK_INTERVAL_SECS: u64 = 1;
/// Maximum allowed cron tick interval in seconds.
const MAX_CRON_TICK_INTERVAL_SECS: u64 = 3600;

impl Default for Config {
    fn default() -> Self {
        let mut providers = HashMap::new();
        providers.insert(
            "openrouter".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://openrouter.ai/api/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert("anthropic".to_string(), ProviderConfig::default());
        providers.insert("openai".to_string(), ProviderConfig::default());
        providers.insert("deepseek".to_string(), ProviderConfig::default());
        providers.insert(
            "groq".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.groq.com/openai/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert("zhipu".to_string(), ProviderConfig::default());
        providers.insert(
            "vllm".to_string(),
            ProviderConfig {
                api_key: PLACEHOLDER_API_KEY.to_string(),
                api_base: Some("http://localhost:8000/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "gemini".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some(
                    "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
                ),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "kimi".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.moonshot.cn/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "xai".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.x.ai/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "mistral".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.mistral.ai/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "minimax".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.minimaxi.com/anthropic".to_string()),
                proxy: None,
                api_type: "anthropic".to_string(),
            },
        );
        providers.insert(
            "qwen".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "glm".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.z.ai/api/paas/v4".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "siliconflow".to_string(),
            ProviderConfig {
                api_key: String::new(),
                api_base: Some("https://api.siliconflow.cn/v1".to_string()),
                proxy: None,
                api_type: "openai".to_string(),
            },
        );
        providers.insert(
            "ollama".to_string(),
            ProviderConfig {
                api_key: "ollama".to_string(),
                api_base: Some("http://localhost:11434".to_string()),
                proxy: None,
                api_type: "ollama".to_string(),
            },
        );

        Self {
            providers,
            budget: BudgetConfig::default(),
            memory: MemoryConfig::default(),
            network: NetworkConfig::default(),
            community_hub: CommunityHubConfig::default(),
            agents: AgentsConfig::default(),
            channels: ChannelsConfig::default(),
            channel_owners: HashMap::new(),
            channel_account_owners: HashMap::new(),
            gateway: GatewayConfig::default(),
            tools: ToolsConfig::default(),
            intent_router: Some(IntentRouterConfig::default()),
            auto_upgrade: AutoUpgradeConfig::default(),
            log: LogConfig::default(),
            security: SecurityConfig::default(),
            default_timezone: None,
            cron_tick_interval_secs: default_cron_tick_interval(),
            openclaw_skill_enabled: false,
            self_improve: SelfImproveConfig::default(),
            evolution: EvolutionConfig::default(),
        }
    }
}

fn format_json5_parse_error(
    path: Option<&Path>,
    context: &str,
    error: &json5::Error,
) -> crate::error::Error {
    let path_text = path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<inline>".to_string());
    crate::error::Error::Config(format!(
        "{} parse error in {}: {}",
        context, path_text, error
    ))
}

fn expand_env_vars_in_text(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let bytes = content.as_bytes();
    let mut index = 0;
    let mut quote = None;
    let mut line_comment = false;
    let mut block_comment = false;
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        if line_comment {
            line_comment = !matches!(byte, b'\n' | b'\r');
        } else if block_comment {
            if byte == b'*' && next == Some(b'/') {
                out.push_str("*/");
                index += 2;
                block_comment = false;
                continue;
            }
        } else if byte == b'$' && next == Some(b'{') {
            if let Some(end) = content[index + 2..].find('}') {
                let end = index + 2 + end;
                let raw = expand_env_expr(&content[index + 2..end]);
                if quote.is_some() {
                    out.push_str(&escape_env_value(&raw));
                } else {
                    // Unquoted interpolation represents one scalar, never JSON5 syntax.
                    match json5::from_str::<Value>(&raw) {
                        Ok(value @ (Value::Null | Value::Bool(_) | Value::Number(_))) => {
                            out.push_str(&value.to_string());
                        }
                        _ => out.push('\0'), // force the normal parse-error path
                    }
                }
                index = end + 1;
                continue;
            }
        } else if let Some(delimiter) = quote {
            if byte == b'\\' {
                out.push('\\');
                index += 1;
                if let Some(ch) = content[index..].chars().next() {
                    out.push(ch);
                    index += ch.len_utf8();
                }
                continue;
            }
            if byte == delimiter {
                quote = None;
            }
        } else if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        } else if byte == b'/' && next == Some(b'/') {
            line_comment = true;
        } else if byte == b'/' && next == Some(b'*') {
            block_comment = true;
        }
        let ch = content[index..].chars().next().unwrap();
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

fn expand_env_expr(expr: &str) -> String {
    let raw = if let Some((name, default)) = expr.split_once(":-") {
        let name = name.trim();
        if name.is_empty() {
            return String::new();
        }
        std::env::var(name)
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| default.to_string())
    } else {
        let name = expr.trim();
        if name.is_empty() {
            return String::new();
        }
        std::env::var(name).unwrap_or_default()
    };
    raw
}

/// ${ENV} 在 JSON5 解析前做文本替换——值中的 `"` 可闭合外层引号注入任意 JSON 键。
/// 转义 JSON5 的两种引号、反斜杠及换行；未加引号的插值另行限制为单一标量。
fn escape_env_value(raw: &str) -> String {
    if !raw.contains(['\\', '"', '\'', '\n', '\r', '\u{2028}', '\u{2029}']) {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len() + 8);
    for ch in raw.chars() {
        match ch {
            '\n' => {
                out.push_str("\\n");
                continue;
            }
            '\r' => {
                out.push_str("\\r");
                continue;
            }
            '\u{2028}' => {
                out.push_str("\\u2028");
                continue;
            }
            '\u{2029}' => {
                out.push_str("\\u2029");
                continue;
            }
            _ => {}
        }
        if ch == '\\' || ch == '"' || ch == '\'' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

pub fn parse_json5_str<T>(content: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    parse_json5_str_with_context(content, None, "JSON5")
}

pub fn parse_json5_str_with_context<T>(
    content: &str,
    path: Option<&Path>,
    context: &str,
) -> Result<T>
where
    T: DeserializeOwned,
{
    let expanded = expand_env_vars_in_text(content);
    json5::from_str(&expanded).map_err(|e| format_json5_parse_error(path, context, &e))
}

pub fn parse_json5_value(content: &str) -> Result<Value> {
    parse_json5_str(content)
}

/// Parse JSON5 without expanding environment references. This is intended for
/// read/modify/write operations that must preserve `${NAME}` expressions.
pub fn parse_json5_value_preserving_env(content: &str) -> Result<Value> {
    json5::from_str(content).map_err(|e| format_json5_parse_error(None, "JSON5", &e))
}

fn preserve_unchanged_env_placeholders(previous: &Value, next: &mut Value) {
    match (previous, next) {
        (Value::Object(previous), Value::Object(next)) => {
            for (key, next_value) in next {
                if let Some(previous_value) = previous.get(key) {
                    preserve_unchanged_env_placeholders(previous_value, next_value);
                }
            }
        }
        (Value::Array(previous), Value::Array(next)) => {
            for (previous_value, next_value) in previous.iter().zip(next.iter_mut()) {
                preserve_unchanged_env_placeholders(previous_value, next_value);
            }
        }
        (Value::String(previous), Value::String(next))
            if previous.contains("${")
                && serde_json::to_string(previous)
                    .ok()
                    .and_then(|quoted| parse_json5_str::<String>(&quoted).ok())
                    .is_some_and(|expanded| expanded == *next) =>
        {
            *next = previous.clone();
        }
        _ => {}
    }
}

/// Deep-merge keys that exist in the on-disk config but are unknown to the
/// `Config` schema back into the serialized output, so a whole-file `save`
/// does not silently drop user-defined fields (JSON5 comments are still lost —
/// the serializer does not preserve them).
fn merge_unknown_keys(previous: &Value, next: &mut Value) {
    if let (Value::Object(previous), Value::Object(next)) = (previous, next) {
        for (key, previous_value) in previous {
            match next.get_mut(key) {
                Some(next_value) => merge_unknown_keys(previous_value, next_value),
                None => {
                    next.insert(key.clone(), previous_value.clone());
                }
            }
        }
    }
}

/// Keep operator comments when normalizing configuration. Comments are emitted
/// as a preamble in their original order; quoted URL/string content is untouched.
fn preserved_json5_comments(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut result = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == quote {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i..].starts_with(b"//") {
            let start = i;
            i += 2;
            while i < bytes.len() && !matches!(bytes[i], b'\n' | b'\r') {
                if raw.is_char_boundary(i)
                    && (raw[i..].starts_with('\u{2028}') || raw[i..].starts_with('\u{2029}'))
                {
                    break;
                }
                i += 1;
            }
            result.push_str(&raw[start..i]);
            result.push('\n');
        } else if bytes[i..].starts_with(b"/*") {
            let start = i;
            i += 2;
            while i + 1 < bytes.len() && !bytes[i..].starts_with(b"*/") {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            result.push_str(&raw[start..i]);
            result.push('\n');
        } else {
            i += 1;
        }
    }
    result
}

pub fn stringify_json5_pretty<T>(value: &T) -> Result<String>
where
    T: Serialize,
{
    Ok(serde_json::to_string_pretty(value)?)
}

pub fn write_json5_pretty<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    let content = stringify_json5_pretty(value)?;
    write_text_atomic_durable(path, &content)
}

/// Placeholder substituted for secret values in API responses and CLI output.
///
/// The same constant doubles as the "leave unchanged" marker on the write path: a
/// client that received the mask may send it back without wiping the real secret
/// (see [`restore_masked_secrets`]).
pub const SECRET_MASK: &str = "********";

/// Whether a config key names a credential that must never be echoed verbatim.
///
/// Matching is name-based and therefore approximate: `key`-suffixed names plus a few
/// well-known markers. A false positive only hides a non-secret value from display.
pub fn is_secret_key(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    const SECRET_MARKERS: &[&str] = &[
        "apikey",
        "token",
        "secret",
        "password",
        "passwd",
        "pass",
        "credential",
        "privatekey",
        "accesskey",
        "signingkey",
        "encryptkey",
        "verificationkey",
    ];
    SECRET_MARKERS.iter().any(|marker| lowered.contains(marker)) || lowered.ends_with("key")
}

/// Replace every secret-looking value in `value` with [`SECRET_MASK`], in place.
pub fn mask_secret_values(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                if is_secret_key(key) {
                    if !entry.is_null() {
                        *entry = Value::String(SECRET_MASK.to_string());
                    }
                } else {
                    mask_secret_values(entry);
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                mask_secret_values(item);
            }
        }
        _ => {}
    }
}

/// Put stored secrets back where `incoming` still carries [`SECRET_MASK`].
///
/// Read endpoints mask credentials; when a client echoes that payload back, every
/// masked leaf means "unchanged" and must be replaced with the value on disk instead
/// of being persisted as the literal mask.
pub fn restore_masked_secrets(current: &Value, incoming: &mut Value) {
    // Checked before destructuring so the mutable borrow of `incoming` is free when
    // the stored value is written back.
    if incoming.as_str() == Some(SECRET_MASK) {
        *incoming = current.clone();
        return;
    }

    match incoming {
        Value::Object(map) => {
            let Value::Object(current_map) = current else {
                return;
            };
            for (key, value) in map.iter_mut() {
                if let Some(stored) = current_map.get(key) {
                    restore_masked_secrets(stored, value);
                }
            }
        }
        Value::Array(items) => {
            let Value::Array(current_items) = current else {
                return;
            };
            for (stored, value) in current_items.iter().zip(items.iter_mut()) {
                restore_masked_secrets(stored, value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod secret_masking_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn masks_credentials_and_keeps_other_values() {
        let mut value = json!({
            "providers": {"openai": {"apiKey": "sk-real", "api_base": "https://api.openai.com/v1"}},
            "channels": {"telegram": {"token": "123:abc", "proxy": "socks5://127.0.0.1:7890"}},
            "agents": {"defaults": {"model": "gpt-5"}}
        });

        mask_secret_values(&mut value);

        assert_eq!(value["providers"]["openai"]["apiKey"], json!(SECRET_MASK));
        assert_eq!(
            value["providers"]["openai"]["api_base"],
            json!("https://api.openai.com/v1")
        );
        assert_eq!(value["channels"]["telegram"]["token"], json!(SECRET_MASK));
        assert_eq!(
            value["channels"]["telegram"]["proxy"],
            json!("socks5://127.0.0.1:7890")
        );
        assert_eq!(value["agents"]["defaults"]["model"], json!("gpt-5"));
    }

    #[test]
    fn masks_webui_password_key() {
        assert!(is_secret_key("webui_pass"));
        let mut value = json!({"gateway": {"webui_pass": "hunter2", "webui_port": 8080}});
        mask_secret_values(&mut value);
        assert_eq!(value["gateway"]["webui_pass"], json!(SECRET_MASK));
        assert_eq!(value["gateway"]["webui_port"], json!(8080));
    }

    #[test]
    fn restore_puts_stored_secrets_back_for_masked_entries() {
        let current = json!({"providers": {"openai": {"apiKey": "sk-real"}}});
        let mut incoming =
            json!({"providers": {"openai": {"apiKey": SECRET_MASK, "model": "gpt-5"}}});

        restore_masked_secrets(&current, &mut incoming);

        assert_eq!(incoming["providers"]["openai"]["apiKey"], json!("sk-real"));
        assert_eq!(incoming["providers"]["openai"]["model"], json!("gpt-5"));
    }
}

#[cfg(windows)]
fn replace_file_atomic_durable(tmp_path: &Path, path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let tmp_wide: Vec<u16> = tmp_path.as_os_str().encode_wide().chain(Some(0)).collect();
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe {
        MoveFileExW(
            tmp_wide.as_ptr(),
            path_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };

    if ok == 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file_atomic_durable(tmp_path: &Path, path: &Path) -> Result<()> {
    std::fs::rename(tmp_path, path)?;
    Ok(())
}

fn write_text_atomic_durable(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let tmp_path = parent.join(format!(
        ".{}.{}.{}.tmp",
        file_name,
        std::process::id(),
        nonce
    ));

    {
        #[cfg(unix)]
        let mut file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp_path)?
        };
        #[cfg(not(unix))]
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        use std::io::Write;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }

    replace_file_atomic_durable(&tmp_path, path)?;

    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

pub fn validate_config_json5_str(content: &str) -> Result<Config> {
    parse_json5_str(content)
}

pub fn validate_config_json5_file(path: &Path, content: &str) -> Result<Config> {
    parse_json5_str_with_context(content, Some(path), "Config JSON5")
}

pub fn write_raw_validated_config_json5(path: &Path, content: &str) -> Result<Config> {
    let config = validate_config_json5_str(content)?;
    write_text_atomic_durable(path, content)?;
    Ok(config)
}

/// Detect system timezone using iana-time-zone crate.
/// Returns None if detection fails (will fall back to UTC in calling code).
fn detect_system_timezone() -> Option<String> {
    match iana_time_zone::get_timezone() {
        Ok(tz) if !tz.is_empty() => {
            // Validate the detected timezone is a valid IANA timezone
            if tz.parse::<chrono_tz::Tz>().is_ok() {
                tracing::info!(timezone = %tz, "Detected system timezone");
                Some(tz)
            } else {
                tracing::warn!(timezone = %tz, "Detected timezone is not a valid IANA timezone, falling back to UTC");
                None
            }
        }
        Ok(_) => {
            tracing::debug!("System timezone detection returned empty string, using UTC");
            None
        }
        Err(e) => {
            tracing::debug!(error = %e, "Failed to detect system timezone, using UTC");
            None
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = validate_config_json5_file(path, &content)?;
        config.validate()
    }

    /// Load config from file, or create default if not exists.
    /// Also ensures default_timezone and cron_tick_interval_secs are set,
    /// updating the config file if necessary.
    pub fn load_or_default(paths: &Paths) -> Result<Self> {
        let config_path = paths.config_file();

        let config = if config_path.exists() {
            Self::load(&config_path)?
        } else {
            // New config: detect system timezone once
            let detected_tz = detect_system_timezone();
            if let Some(ref tz) = detected_tz {
                tracing::info!(timezone = %tz, "Detected system timezone for new config");
            }
            Self {
                default_timezone: detected_tz,
                ..Default::default()
            }
        };

        // Check if we need to update the config file with missing fields
        let mut needs_save = config.default_timezone.is_none() && config_path.exists();

        // Ensure openclawSkillEnabled field exists in config file
        if config_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&config_path) {
                if !raw.contains("openclawSkillEnabled") {
                    tracing::info!("Adding missing openclawSkillEnabled field to config");
                    needs_save = true;
                }
                // Ensure loadAllTools field exists in intentRouter (if present)
                if raw.contains("intentRouter") && !raw.contains("loadAllTools") {
                    tracing::info!("Adding missing loadAllTools field to intentRouter config");
                    needs_save = true;
                }
                // Ensure log config section exists
                if !raw.contains("\"log\"") && !raw.contains("log:") {
                    tracing::info!("Adding missing log config section to config");
                    needs_save = true;
                } else {
                    // log section exists, check for missing fields
                    if !raw.contains("consoleEnabled") {
                        tracing::info!(
                            "Adding missing consoleEnabled field to log config (default: true)"
                        );
                        needs_save = true;
                    }
                    if !raw.contains("fileEnabled") {
                        tracing::info!(
                            "Adding missing fileEnabled field to log config (default: false)"
                        );
                        needs_save = true;
                    }
                    if !raw.contains("level") {
                        tracing::info!("Adding missing level field to log config (default: info)");
                        needs_save = true;
                    }
                }
                // Ensure memorySystem config section exists
                if !raw.contains("memorySystem") {
                    tracing::info!(
                        "Adding missing memorySystem config section to config (7-layer memory thresholds)"
                    );
                    needs_save = true;
                }
            }
        }

        // Detect timezone if not set (only for existing configs with missing field)
        let config = if config.default_timezone.is_none() {
            // Only reached for existing configs with missing default_timezone
            let detected_tz = detect_system_timezone();
            if let Some(ref tz) = detected_tz {
                tracing::info!(timezone = %tz, "Setting detected timezone in config");
            }
            Config {
                default_timezone: detected_tz,
                ..config
            }
        } else {
            config
        };

        // Save if we added missing fields
        if needs_save || !config_path.exists() {
            if let Err(e) = config.save(&config_path) {
                tracing::warn!(error = %e, "Failed to save updated config file");
            } else {
                tracing::info!(path = %config_path.display(), "Config file updated with missing fields");
            }
        }

        // Validate memorySystem config and log warnings (non-fatal)
        let mem_warnings = config.memory.memory_system.validate();
        for warning in &mem_warnings {
            tracing::warn!(warning, "memorySystem config warning");
        }

        Ok(config)
    }

    /// Validate config values and return self if valid.
    fn validate(self) -> Result<Self> {
        tools::ApprovalPolicy::parse(self.tools.exec.approval_policy.as_deref())?;

        // Validate cron_tick_interval_secs
        if self.cron_tick_interval_secs < MIN_CRON_TICK_INTERVAL_SECS {
            tracing::warn!(
                value = self.cron_tick_interval_secs,
                min = MIN_CRON_TICK_INTERVAL_SECS,
                "cron_tick_interval_secs too small, rejecting config"
            );
            return Err(crate::Error::Config(format!(
                "cron_tick_interval_secs must be at least {} seconds, got {}",
                MIN_CRON_TICK_INTERVAL_SECS, self.cron_tick_interval_secs
            )));
        }
        if self.cron_tick_interval_secs > MAX_CRON_TICK_INTERVAL_SECS {
            tracing::warn!(
                value = self.cron_tick_interval_secs,
                max = MAX_CRON_TICK_INTERVAL_SECS,
                "cron_tick_interval_secs too large, rejecting config"
            );
            return Err(crate::Error::Config(format!(
                "cron_tick_interval_secs must be at most {} seconds, got {}",
                MAX_CRON_TICK_INTERVAL_SECS, self.cron_tick_interval_secs
            )));
        }

        // Validate default_timezone if set
        if let Some(ref tz) = self.default_timezone {
            if tz.parse::<chrono_tz::Tz>().is_err() {
                return Err(crate::Error::Config(format!(
                    "Invalid default_timezone '{}'. Use IANA timezone like 'Asia/Shanghai', 'America/New_York'",
                    tz
                )));
            }
        }

        Ok(self)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut next = serde_json::to_value(self)?;
        let mut comments = String::new();
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(previous) = parse_json5_value_preserving_env(&raw) {
                preserve_unchanged_env_placeholders(&previous, &mut next);
                merge_unknown_keys(&previous, &mut next);
                comments = preserved_json5_comments(&raw);
            }
        }
        comments.push_str(&stringify_json5_pretty(&next)?);
        write_text_atomic_durable(path, &comments)
    }

    /// Return the highest-priority provider that has a usable API key.
    ///
    /// Placeholder keys (see [`PLACEHOLDER_API_KEY`]) are treated as "not
    /// configured" so that a fresh install does not report the local `vllm`
    /// preset as an active provider. This matches the CLI's `has_key` checks.
    pub fn get_api_key(&self) -> Option<(&str, &ProviderConfig)> {
        let priority = [
            "openrouter",
            "deepseek",
            "anthropic",
            "openai",
            "kimi",
            "gemini",
            "zhipu",
            "groq",
            "vllm",
            "ollama",
        ];

        for name in priority {
            if let Some(provider) = self.providers.get(name) {
                if !is_placeholder_api_key(&provider.api_key) {
                    return Some((name, provider));
                }
            }
        }
        None
    }

    pub fn get_provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }

    pub fn community_hub_url(&self) -> Option<String> {
        if let Some(url) = self.community_hub.hub_url.as_ref() {
            let url = url.trim();
            if !url.is_empty() {
                return Some(url.trim_end_matches('/').to_string());
            }
        }
        None
    }

    pub fn community_hub_api_key(&self) -> Option<String> {
        if let Some(key) = self.community_hub.api_key.as_ref() {
            let key = key.trim();
            if !key.is_empty() {
                return Some(key.to_string());
            }
        }
        None
    }

    pub fn resolve_channel_owner(&self, channel: &str) -> Option<&str> {
        self.channel_owners
            .get(channel)
            .map(|owner| owner.as_str())
            .filter(|owner| !owner.trim().is_empty())
    }

    pub fn resolve_channel_account_owner(&self, channel: &str, account_id: &str) -> Option<&str> {
        let account_id = account_id.trim();
        if account_id.is_empty() {
            return None;
        }

        self.channel_account_owners
            .get(channel)
            .and_then(|owners| owners.get(account_id))
            .map(|owner| owner.as_str())
            .filter(|owner| !owner.trim().is_empty())
    }

    pub fn resolve_effective_channel_owner(
        &self,
        channel: &str,
        account_id: Option<&str>,
    ) -> Option<&str> {
        account_id
            .and_then(|account_id| self.resolve_channel_account_owner(channel, account_id))
            .or_else(|| self.resolve_channel_owner(channel))
    }

    pub fn is_external_channel_enabled(&self, channel: &str) -> bool {
        match channel {
            "telegram" => self.channels.telegram.enabled,
            "whatsapp" => self.channels.whatsapp.enabled,
            "feishu" => self.channels.feishu.enabled,
            "slack" => self.channels.slack.enabled,
            "discord" => self.channels.discord.enabled,
            "dingtalk" => self.channels.dingtalk.enabled,
            "wecom" => self.channels.wecom.enabled,
            "lark" => self.channels.lark.enabled,
            "qq" => self.channels.qq.enabled,
            "napcat" => self.channels.napcat.enabled,
            "weixin" => self.channels.weixin.enabled,
            _ => false,
        }
    }

    pub fn known_agent_ids(&self) -> Vec<String> {
        let mut ids = vec!["default".to_string()];
        for agent in self.agents.list.iter().filter(|agent| agent.enabled) {
            let agent_id = agent.id.trim();
            if agent_id.is_empty() || agent_id == "default" {
                continue;
            }
            if !ids.iter().any(|id| id == agent_id) {
                ids.push(agent_id.to_string());
            }
        }
        ids
    }

    pub fn agent_exists(&self, agent_id: &str) -> bool {
        let agent_id = agent_id.trim();
        !agent_id.is_empty() && self.known_agent_ids().iter().any(|id| id == agent_id)
    }

    pub fn resolve_agent_spec(&self, agent_id: &str) -> Option<ResolvedAgentConfig> {
        let agent_id = agent_id.trim();
        if agent_id.is_empty() {
            return None;
        }

        let agent = self
            .agents
            .list
            .iter()
            .find(|agent| agent.enabled && agent.id.trim() == agent_id);

        if agent_id != "default" && agent.is_none() {
            return None;
        }

        let mut defaults = self.agents.defaults.clone();
        if let Some(agent) = agent {
            let explicit_model = agent
                .model
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let explicit_provider = agent
                .provider
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let has_single_model_override = explicit_model.is_some() || explicit_provider.is_some();

            if let Some(model) = explicit_model {
                defaults.model = model;
            }
            if let Some(provider) = explicit_provider {
                defaults.provider = Some(provider);
            }
            if !agent.model_pool.is_empty() {
                defaults.model_pool = agent.model_pool.clone();
            } else if has_single_model_override {
                defaults.model_pool.clear();
            }
            if let Some(routing_strategy) = agent.routing_strategy {
                defaults.routing_strategy = routing_strategy;
            }
            if let Some(max_tokens) = agent.max_tokens {
                defaults.max_tokens = max_tokens;
            }
            if let Some(temperature) = agent.temperature {
                defaults.temperature = temperature;
            }
            if let Some(max_tool_iterations) = agent.max_tool_iterations {
                defaults.max_tool_iterations = max_tool_iterations;
            }
            if !agent.max_tool_iterations_by_tool.is_empty() {
                defaults.max_tool_iterations_by_tool = agent.max_tool_iterations_by_tool.clone();
            }
            if let Some(llm_max_retries) = agent.llm_max_retries {
                defaults.llm_max_retries = llm_max_retries;
            }
            if let Some(llm_retry_delay_ms) = agent.llm_retry_delay_ms {
                defaults.llm_retry_delay_ms = llm_retry_delay_ms;
            }
            if let Some(max_context_tokens) = agent.max_context_tokens {
                defaults.max_context_tokens = max_context_tokens;
            }
            if let Some(evolution_model) = agent
                .evolution_model
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
            {
                defaults.evolution_model = Some(evolution_model);
            }
            if let Some(evolution_provider) = agent
                .evolution_provider
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
            {
                defaults.evolution_provider = Some(evolution_provider);
            }
            if let Some(allowed_mcp_servers) = &agent.allowed_mcp_servers {
                defaults.allowed_mcp_servers = allowed_mcp_servers.clone();
            }
            if let Some(allowed_mcp_tools) = &agent.allowed_mcp_tools {
                defaults.allowed_mcp_tools = allowed_mcp_tools.clone();
            }
            for (tool, overrides) in &agent.tool_presentation {
                let merged = defaults
                    .tool_presentation
                    .get(tool)
                    .cloned()
                    .unwrap_or_default()
                    .overlay(overrides);
                defaults.tool_presentation.insert(tool.clone(), merged);
            }
        }

        Some(ResolvedAgentConfig {
            id: agent_id.to_string(),
            name: agent.and_then(|entry| entry.name.clone()),
            defaults,
            intent_profile: self.resolve_intent_profile_id(Some(agent_id)),
        })
    }

    pub fn resolved_agents(&self) -> Vec<ResolvedAgentConfig> {
        self.known_agent_ids()
            .into_iter()
            .filter_map(|agent_id| self.resolve_agent_spec(&agent_id))
            .collect()
    }

    pub fn config_for_agent(&self, agent_id: &str) -> Option<Config> {
        let resolved = self.resolve_agent_spec(agent_id)?;
        let mut config = self.clone();
        config.agents.defaults = resolved.defaults;
        Some(config)
    }

    pub fn resolve_intent_profile_id(&self, agent_id: Option<&str>) -> Option<String> {
        let router = self.intent_router.clone().unwrap_or_default();

        let requested_agent_id = agent_id.map(str::trim).filter(|id| !id.is_empty());

        if let Some(agent_id) = requested_agent_id {
            if let Some(profile) = self
                .agents
                .list
                .iter()
                .find(|agent| agent.enabled && agent.id.trim() == agent_id)
                .and_then(|agent| agent.intent_profile.as_deref())
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
            {
                return Some(profile.to_string());
            }

            if let Some(profile) = router
                .agent_profiles
                .get(agent_id)
                .map(String::as_str)
                .map(str::trim)
                .filter(|profile| !profile.is_empty())
            {
                return Some(profile.to_string());
            }
        }

        let default_profile = router.default_profile.trim();
        if default_profile.is_empty() {
            None
        } else {
            Some(default_profile.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn validate_rejects_invalid_approval_policy() {
        let config = Config {
            tools: ToolsConfig {
                exec: tools::ExecConfig {
                    approval_policy: Some("bogus".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            config.validate(),
            Err(crate::Error::Validation(_))
        ));
    }

    #[test]
    fn memory_recall_policy_deserializes_per_mode_and_internal_channel() {
        let cfg: Config = json5::from_str(
            r#"{
                memory: {
                    memoryRecall: {
                        chat: true,
                        general: true,
                        skill: true,
                        internal: false,
                    },
                },
            }"#,
        )
        .expect("parse memory recall policy");

        assert!(cfg.memory.memory_recall.chat);
        assert!(cfg.memory.memory_recall.general);
        assert!(cfg.memory.memory_recall.skill);
        assert!(!cfg.memory.memory_recall.internal);
        assert!(cfg
            .memory
            .memory_recall
            .allows(MemoryRecallMode::Chat, "cli"));
        assert!(!cfg
            .memory
            .memory_recall
            .allows(MemoryRecallMode::General, "system"));
    }

    #[test]
    fn agent_tool_presentation_overlays_defaults_field_by_field() {
        let cfg: Config = json5::from_str(
            r#"{
                agents: {
                    defaults: {
                        toolPresentation: {
                            exec: { description: "Run commands", group: "system", order: 5 },
                        },
                    },
                    list: [
                        {
                            id: "ops",
                            toolPresentation: {
                                exec: { visibility: "discoverable", order: 1 },
                                web_search: { description: "Search the web for ops" },
                            },
                        },
                    ],
                },
            }"#,
        )
        .expect("parse tool presentation config");

        let ops = cfg.config_for_agent("ops").unwrap().agents.defaults;
        let exec = &ops.tool_presentation["exec"];
        assert_eq!(exec.description.as_deref(), Some("Run commands"));
        assert_eq!(exec.group.as_deref(), Some("system"));
        assert_eq!(exec.visibility, Some(ToolVisibility::Discoverable));
        assert_eq!(exec.order, Some(1));
        assert_eq!(
            ops.tool_presentation["web_search"].description.as_deref(),
            Some("Search the web for ops")
        );

        let default_agent = cfg.config_for_agent("default").unwrap().agents.defaults;
        assert_eq!(default_agent.tool_presentation["exec"].visibility, None);
        assert!(!default_agent.tool_presentation.contains_key("web_search"));
    }

    #[test]
    fn three_subsystem_config_prefers_new_forked_agent_over_legacy_layer7() {
        let legacy: Config = json5::from_str(
            r#"{
                memory: {
                    memorySystem: {
                        layer7: { enabled: false, maxTurns: 6, timeoutSecs: 90 }
                    }
                }
            }"#,
        )
        .expect("parse legacy layer7 config");
        let legacy_effective = legacy.memory.effective_forked_agent();
        assert!(!legacy_effective.enabled);
        assert_eq!(legacy_effective.max_turns, 6);
        assert_eq!(legacy_effective.timeout_secs, 90);

        let modern: Config = json5::from_str(
            r#"{
                memory: {
                    memorySystem: {
                        layer7: { enabled: false, maxTurns: 6, timeoutSecs: 90 }
                    },
                    contextManagement: {
                        forkedAgent: { enabled: true, maxTurns: 12, timeoutSecs: 150 }
                    },
                    knowledgeSystem: {
                        promptBudget: { total: 8000 },
                        memoryRecall: { chat: false }
                    },
                    learningSystem: { enabled: false }
                }
            }"#,
        )
        .expect("parse three subsystem config");
        let modern_effective = modern.memory.effective_forked_agent();
        assert!(modern_effective.enabled);
        assert_eq!(modern_effective.max_turns, 12);
        assert_eq!(modern_effective.timeout_secs, 150);
        assert_eq!(modern.memory.effective_prompt_budget().total, 8000);
        assert!(!modern.memory.effective_memory_recall().chat);
        assert!(!modern.memory.learning_system.enabled());
    }

    fn temp_config_path(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blockcell-config-tests-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir.join(name)
    }

    #[test]
    fn test_config_load_accepts_json5_comments_and_trailing_commas() {
        let path = temp_config_path("config.json5");
        fs::write(
            &path,
            r#"{
  // provider config in JSON5
  providers: {
    openai: {
      apiKey: 'sk-test',
    },
  },
  agents: {
    defaults: {
      model: 'gpt-4.1',
    },
  },
}"#,
        )
        .expect("write config.json5");

        let cfg = Config::load(&path).expect("load json5 config");
        assert_eq!(cfg.agents.defaults.model, "gpt-4.1");
        assert_eq!(
            cfg.providers.get("openai").map(|p| p.api_key.as_str()),
            Some("sk-test")
        );
    }

    #[test]
    fn test_embedded_ghost_learning_defaults_are_safe() {
        let cfg = GhostConfig::default();
        assert!(cfg.learning.enabled);
        assert!(cfg.learning.legacy_shadow_mode());
        assert_eq!(cfg.learning.recall_max_items, 4);
    }

    #[test]
    fn test_config_save_round_trips_via_json5_loader() {
        let path = temp_config_path("config.json5");
        let mut cfg = Config::default();
        cfg.agents.defaults.model = "deepseek-chat".to_string();
        cfg.memory.vector.enabled = true;
        cfg.memory.vector.provider = "openai".to_string();
        cfg.memory.vector.model = "text-embedding-3-small".to_string();
        cfg.memory.vector.uri = Some("./memory/vectors.rabitq".to_string());

        cfg.save(&path).expect("save config json5");
        let content = fs::read_to_string(&path).expect("read saved config");
        assert!(content.contains("deepseek-chat"));
        assert!(content.contains("text-embedding-3-small"));

        let loaded = Config::load(&path).expect("reload saved config");
        assert_eq!(loaded.agents.defaults.model, "deepseek-chat");
        assert!(loaded.memory.vector.enabled);
        assert_eq!(loaded.memory.vector.provider, "openai");
        assert_eq!(loaded.memory.vector.model, "text-embedding-3-small");
        assert_eq!(
            loaded.memory.vector.uri.as_deref(),
            Some("./memory/vectors.rabitq")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_config_save_uses_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_config_path("secure-config.json5");
        fs::write(&path, "{}\n").expect("seed config");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .expect("set permissive seed mode");

        Config::default().save(&path).expect("save config");

        let mode = fs::metadata(&path)
            .expect("config metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "saved config must not be readable by other users"
        );
    }

    #[test]
    fn test_config_save_preserves_unchanged_env_placeholders() {
        let path = temp_config_path("env-config.json5");
        unsafe {
            std::env::set_var("BLOCKCELL_TEST_SAVE_KEY", "sk-expanded-secret");
        }
        fs::write(
            &path,
            r#"{
  providers: {
    openai: { apiKey: "${BLOCKCELL_TEST_SAVE_KEY}" },
  },
  agents: { defaults: { model: "old-model" } },
}"#,
        )
        .expect("write config with env placeholder");

        let mut config = Config::load(&path).expect("load expanded config");
        config.agents.defaults.model = "new-model".to_string();
        config.save(&path).expect("save updated config");

        let saved = fs::read_to_string(&path).expect("read saved config");
        assert!(saved.contains("${BLOCKCELL_TEST_SAVE_KEY}"));
        assert!(!saved.contains("sk-expanded-secret"));
        assert!(saved.contains("new-model"));

        unsafe {
            std::env::remove_var("BLOCKCELL_TEST_SAVE_KEY");
        }
    }

    #[test]
    fn test_config_save_preserves_comments() {
        let path = temp_config_path("comments.json5");
        fs::write(
            &path,
            "// operator notes\n{ agents: { defaults: { model: 'old' /* rationale */ } } }",
        )
        .unwrap();
        let mut config = Config::load(&path).unwrap();
        config.agents.defaults.model = "new".into();
        config.save(&path).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("// operator notes"));
        assert!(saved.contains("/* rationale */"));
        assert_eq!(Config::load(&path).unwrap().agents.defaults.model, "new");
        config.save(&path).unwrap();
        assert_eq!(saved, fs::read_to_string(&path).unwrap());
    }

    #[test]
    fn comment_extraction_ignores_quotes_and_urls() {
        let raw = r#"{ url: 'https://example.test/a//b', text: "escaped\"/* not a comment */", x: 1 /* 中文备注 */ } // tail"#;
        assert_eq!(preserved_json5_comments(raw), "/* 中文备注 */\n// tail\n");
    }

    #[test]
    fn test_config_save_preserves_unknown_keys() {
        let path = temp_config_path("unknown-keys-config.json5");
        fs::write(
            &path,
            r#"{
  agents: { defaults: { model: "old-model", myCustomField: "keep-me" } },
  unknownTopLevel: { nested: 42 },
}"#,
        )
        .expect("write config with unknown keys");

        let mut config = Config::load(&path).expect("load config");
        config.agents.defaults.model = "new-model".to_string();
        config.save(&path).expect("save updated config");

        let saved = fs::read_to_string(&path).expect("read saved config");
        assert!(saved.contains("new-model"));
        assert!(
            saved.contains("myCustomField") && saved.contains("keep-me"),
            "unknown nested key must survive a full-file save: {saved}"
        );
        assert!(
            saved.contains("unknownTopLevel") && saved.contains("42"),
            "unknown top-level key must survive a full-file save: {saved}"
        );
    }

    #[test]
    fn test_config_load_expands_env_vars_in_json5() {
        let path = temp_config_path("config.json5");
        // SAFETY: This test runs in a single-threaded context. The environment
        // variable modification is isolated to this test's scope and will be
        // cleaned up at the end of the test. No other threads access these vars.
        unsafe {
            std::env::set_var("BLOCKCELL_TEST_OPENAI_KEY", "sk-from-env");
            std::env::remove_var("BLOCKCELL_TEST_MODEL");
        }

        fs::write(
            &path,
            r#"{
  providers: {
    openai: {
      apiKey: "${BLOCKCELL_TEST_OPENAI_KEY}",
    },
  },
  agents: {
    defaults: {
      model: "${BLOCKCELL_TEST_MODEL:-gpt-4.1}",
    },
  },
}"#,
        )
        .expect("write config.json5");

        let cfg = Config::load(&path).expect("load env-expanded json5 config");
        assert_eq!(
            cfg.providers.get("openai").map(|p| p.api_key.as_str()),
            Some("sk-from-env")
        );
        assert_eq!(cfg.agents.defaults.model, "gpt-4.1");

        unsafe {
            std::env::remove_var("BLOCKCELL_TEST_OPENAI_KEY");
            std::env::remove_var("BLOCKCELL_TEST_MODEL");
        }
    }

    #[test]
    fn env_value_cannot_break_out_of_quotes_to_inject_keys() {
        let path = temp_config_path("config-env-injection.json5");
        // SAFETY: single-threaded test; the var is removed at the end.
        unsafe {
            std::env::set_var("BLOCKCELL_TEST_EVIL", r#"x", "evil": "1"#);
        }

        fs::write(
            &path,
            r#"{
  providers: {
    openai: {
      apiKey: "${BLOCKCELL_TEST_EVIL}",
    },
  },
}"#,
        )
        .expect("write config.json5");

        let cfg = Config::load(&path).expect("load config with escaped env value");
        let openai = cfg.providers.get("openai").expect("openai provider");
        // 值中的引号被转义：api_key 是含引号的字面字符串，而非注入的新键。
        assert_eq!(openai.api_key, r#"x", "evil": "1"#);
        assert!(!cfg.providers.contains_key("evil"));

        unsafe {
            std::env::remove_var("BLOCKCELL_TEST_EVIL");
        }
    }

    #[test]
    fn test_config_loads_memory_vector_config() {
        let raw = r#"{
  providers: {
    openai: {
      apiKey: "sk-test"
    }
  },
  memory: {
    vector: {
      enabled: true,
      provider: "openai",
      model: "text-embedding-3-small",
      uri: "./memory/rabitq",
      table: "memory_vectors"
    }
  }
}"#;

        let cfg: Config = json5::from_str(raw).expect("parse config");
        assert!(cfg.memory.vector.enabled);
        assert_eq!(cfg.memory.vector.provider, "openai");
        assert_eq!(cfg.memory.vector.model, "text-embedding-3-small");
        assert_eq!(cfg.memory.vector.uri.as_deref(), Some("./memory/rabitq"));
        assert_eq!(cfg.memory.vector.table, "memory_vectors");
    }

    #[test]
    fn test_memory_circuit_breaker_tracks_explicit_presence() {
        let omitted_raw = r#"{
  "memory": {
    "memorySystem": {}
  }
}"#;
        let omitted: Config = serde_json::from_str(omitted_raw).unwrap();
        assert!(!omitted.memory.memory_system.circuit_breaker.is_configured());

        let explicit_default_raw = r#"{
  "memory": {
    "memorySystem": {
      "circuitBreaker": {
        "maxFailures": 3,
        "resetTimeoutSecs": 60
      }
    }
  }
}"#;
        let explicit_default: Config = serde_json::from_str(explicit_default_raw).unwrap();
        assert!(explicit_default
            .memory
            .memory_system
            .circuit_breaker
            .is_configured());
        assert_eq!(
            explicit_default
                .memory
                .memory_system
                .circuit_breaker
                .reset_timeout_secs,
            60
        );

        let implicit_json = serde_json::to_string(&omitted.memory.memory_system).unwrap();
        assert!(!implicit_json.contains("circuitBreaker"));

        let explicit_json = serde_json::to_string(&explicit_default.memory.memory_system).unwrap();
        assert!(explicit_json.contains("circuitBreaker"));
    }

    #[test]
    fn test_get_api_key_ignores_placeholder_preset() {
        let cfg = Config::default();
        assert!(
            cfg.get_api_key().is_none(),
            "fresh config must not report a local placeholder key as an active provider"
        );

        let mut cfg = Config::default();
        cfg.providers
            .get_mut("openai")
            .expect("openai preset")
            .api_key = "sk-real".to_string();
        assert_eq!(cfg.get_api_key().map(|(name, _)| name), Some("openai"));
    }

    #[test]
    fn test_community_hub_top_level() {
        let raw = r#"{
  "communityHub": { "hubUrl": "http://example.com/", "apiKey": "k" },
  "providers": {}
}"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();
        assert_eq!(
            cfg.community_hub_url().as_deref(),
            Some("http://example.com")
        );
        assert_eq!(cfg.community_hub_api_key().as_deref(), Some("k"));
    }

    #[test]
    fn test_channel_owners_and_accounts_deserialize() {
        let raw = r#"{
  "agents": {
    "list": [
      { "id": "chat", "enabled": true }
    ]
  },
  "channelOwners": {
    "telegram": "chat"
  },
  "channels": {
    "telegram": {
      "enabled": true,
      "defaultAccountId": "default",
      "accounts": {
        "default": {
          "enabled": true,
          "token": "tg-token"
        }
      }
    }
  }
}"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();
        assert_eq!(cfg.resolve_channel_owner("telegram"), Some("chat"));
        assert!(cfg.is_external_channel_enabled("telegram"));
        assert_eq!(
            cfg.channels.telegram.default_account_id.as_deref(),
            Some("default")
        );
        let acc = cfg.channels.telegram.accounts.get("default").unwrap();
        assert_eq!(acc.token, "tg-token");
        assert!(cfg.agent_exists("chat"));
    }

    #[test]
    fn test_channel_account_owner_override_deserializes_and_resolves() {
        let raw = r#"{
  "agents": {
    "list": [
      { "id": "ops", "enabled": true }
    ]
  },
  "channelOwners": {
    "telegram": "default"
  },
  "channelAccountOwners": {
    "telegram": {
      "bot2": "ops"
    }
  },
  "channels": {
    "telegram": {
      "enabled": true,
      "accounts": {
        "bot1": { "enabled": true, "token": "tg-bot1" },
        "bot2": { "enabled": true, "token": "tg-bot2" }
      }
    }
  }
}"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();

        assert_eq!(
            cfg.resolve_channel_account_owner("telegram", "bot2"),
            Some("ops")
        );
        assert_eq!(
            cfg.resolve_effective_channel_owner("telegram", Some("bot2")),
            Some("ops")
        );
        assert_eq!(
            cfg.resolve_effective_channel_owner("telegram", Some("bot1")),
            Some("default")
        );
        assert_eq!(
            cfg.resolve_effective_channel_owner("telegram", None),
            Some("default")
        );
    }

    #[test]
    fn test_channel_account_owner_resolution_ignores_blank_values() {
        let raw = r#"{
  "channelOwners": {
    "telegram": "default"
  },
  "channelAccountOwners": {
    "telegram": {
      "bot1": "   "
    }
  }
}"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();

        assert_eq!(cfg.resolve_channel_account_owner("telegram", "bot1"), None);
        assert_eq!(
            cfg.resolve_effective_channel_owner("telegram", Some("bot1")),
            Some("default")
        );
    }

    #[test]
    fn test_legacy_single_channel_fields_still_work() {
        let raw = r#"{
  "channels": {
    "telegram": {
      "enabled": true,
      "token": "legacy-token"
    }
  }
}"#;
        let cfg: Config = serde_json::from_str(raw).unwrap();
        assert_eq!(cfg.channels.telegram.token, "legacy-token");
        assert!(cfg.channels.telegram.accounts.is_empty());
        assert_eq!(cfg.channels.telegram.default_account_id, None);
        assert!(cfg.agent_exists("default"));
    }

    #[test]
    fn test_known_agent_ids_fallback_to_default() {
        let cfg = Config::default();
        let ids = cfg.known_agent_ids();
        assert_eq!(ids, vec!["default".to_string()]);
    }

    #[test]
    fn test_intent_router_deserializes_and_resolves_agent_profile() {
        let raw = r#"{
  "agents": {
    "list": [
      { "id": "ops", "enabled": true, "intentProfile": "ops" }
    ]
  },
  "intentRouter": {
    "enabled": true,
    "defaultProfile": "default",
    "profiles": {
      "default": {
        "coreTools": ["read_file", "message"],
        "intentTools": {
          "Chat": { "inheritBase": false, "tools": [] },
          "Unknown": ["browse"]
        }
      },
      "ops": {
        "coreTools": ["read_file", "exec"],
        "intentTools": {
          "DevOps": ["git_api"],
          "Unknown": ["http_request"]
        },
        "denyTools": ["email"]
      }
    }
  }
}"#;

        let cfg: Config = serde_json::from_str(raw).unwrap();
        let router = cfg.intent_router.as_ref().expect("intent router");
        assert!(router.enabled);
        assert_eq!(
            cfg.resolve_intent_profile_id(Some("ops")),
            Some("ops".to_string())
        );
        assert_eq!(
            cfg.resolve_intent_profile_id(Some("missing")),
            Some("default".to_string())
        );
        assert_eq!(
            cfg.resolve_intent_profile_id(None),
            Some("default".to_string())
        );
    }

    #[test]
    fn test_default_config_includes_intent_router_defaults() {
        let cfg = Config::default();
        let router = cfg.intent_router.as_ref().expect("default intent router");

        assert!(router.profiles.contains_key("default"));
        assert_eq!(
            cfg.resolve_intent_profile_id(Some("default")),
            Some("default".to_string())
        );
    }

    #[test]
    fn test_missing_intent_router_uses_default_router() {
        let cfg: Config = serde_json::from_str("{}").unwrap();
        let router = cfg.intent_router.as_ref().expect("defaulted intent router");

        assert!(router.enabled);
        assert!(router.profiles.contains_key("default"));
        assert_eq!(
            cfg.resolve_intent_profile_id(None),
            Some("default".to_string())
        );
    }

    #[test]
    fn test_resolved_agent_falls_back_to_implicit_default() {
        let cfg = Config::default();
        let resolved = cfg
            .resolve_agent_spec("default")
            .expect("implicit default agent");

        assert_eq!(resolved.id, "default");
        assert_eq!(resolved.defaults.model, cfg.agents.defaults.model);
        assert_eq!(resolved.defaults.provider, cfg.agents.defaults.provider);
        assert_eq!(resolved.intent_profile.as_deref(), Some("default"));
    }

    #[test]
    fn test_resolved_agent_inherits_and_overrides_defaults() {
        let raw = r#"{
  "agents": {
    "defaults": {
      "model": "deepseek-chat",
      "provider": "deepseek",
      "modelPool": [
        { "model": "deepseek-chat", "provider": "deepseek", "weight": 1, "priority": 1 }
      ]
    },
    "list": [
      {
        "id": "ops",
        "enabled": true,
        "intentProfile": "ops",
        "model": "gpt-4.1",
        "provider": "openai"
      }
    ]
  },
  "intentRouter": {
    "enabled": true,
    "defaultProfile": "default",
    "profiles": {
      "default": {
        "coreTools": ["read_file"],
        "intentTools": { "Unknown": [] }
      },
      "ops": {
        "coreTools": ["exec"],
        "intentTools": { "Unknown": ["http_request"] }
      }
    }
  }
}"#;

        let cfg: Config = serde_json::from_str(raw).unwrap();
        let resolved = cfg.resolve_agent_spec("ops").expect("resolved ops agent");

        assert_eq!(resolved.id, "ops");
        assert_eq!(resolved.defaults.model, "gpt-4.1");
        assert_eq!(resolved.defaults.provider.as_deref(), Some("openai"));
        assert!(
            resolved.defaults.model_pool.is_empty(),
            "explicit model/provider override should disable inherited model_pool"
        );
        assert_eq!(resolved.intent_profile.as_deref(), Some("ops"));
    }

    #[test]
    fn test_routing_strategy_deserializes_and_agent_overrides_default() {
        let raw = r#"{
  "agents": {
    "defaults": {
      "routingStrategy": "cost_optimized"
    },
    "list": [
      {
        "id": "ops",
        "enabled": true,
        "routingStrategy": "quality_first"
      }
    ]
  }
}"#;

        let cfg: Config = serde_json::from_str(raw).unwrap();
        assert_eq!(
            cfg.agents.defaults.routing_strategy,
            RoutingStrategy::CostOptimized
        );

        let resolved = cfg.resolve_agent_spec("ops").expect("resolved ops agent");
        assert_eq!(
            resolved.defaults.routing_strategy,
            RoutingStrategy::QualityFirst
        );
    }

    #[test]
    fn test_resolved_agents_always_include_default() {
        let raw = r#"{
  "agents": {
    "list": [
      { "id": "ops", "enabled": true, "intentProfile": "ops" }
    ]
  },
  "intentRouter": {
    "enabled": true,
    "defaultProfile": "default",
    "profiles": {
      "default": {
        "coreTools": ["read_file"],
        "intentTools": { "Unknown": [] }
      },
      "ops": {
        "coreTools": ["exec"],
        "intentTools": { "Unknown": ["http_request"] }
      }
    }
  }
}"#;

        let cfg: Config = serde_json::from_str(raw).unwrap();
        let ids: Vec<String> = cfg
            .resolved_agents()
            .into_iter()
            .map(|agent| agent.id)
            .collect();
        assert_eq!(ids, vec!["default".to_string(), "ops".to_string()]);
    }
}

#[cfg(test)]
mod recheck_probe_env {
    #[test]
    fn interpolation_preserves_scalars_strings_and_comment_context() {
        let value = super::parse_json5_value(
            r#"{
                // don't let comment quotes alter string detection: '
                n: ${P3_CONTEXT_UNSET_72C:-12},
                b: ${P3_CONTEXT_UNSET_72C:-true},
                z: ${P3_CONTEXT_UNSET_72C:-null},
                url: '${P3_CONTEXT_UNSET_72C:-https://example.com/a,b}',
                /* ' " */ text: "${P3_CONTEXT_UNSET_72C:-hello}",
            }"#,
        )
        .unwrap();
        assert_eq!(value["n"], 12);
        assert_eq!(value["b"], true);
        assert!(value["z"].is_null());
        assert_eq!(value["url"], "https://example.com/a,b");
        assert_eq!(value["text"], "hello");
    }

    #[test]
    fn unquoted_env_cannot_inject_config_fields() {
        std::env::set_var("P3_UNQUOTED_INJECT", "null, injected: true, tail: null");
        let result = super::parse_json5_value("{value: ${P3_UNQUOTED_INJECT}}");
        std::env::remove_var("P3_UNQUOTED_INJECT");
        assert!(
            result.is_err(),
            "non-scalar unquoted expansion must be rejected"
        );
    }

    #[test]
    fn recheck_probe_single_quote() {
        std::env::set_var("P3_RECHECK_INJECT", "x', injected: true, tail: 'y");
        let result = super::parse_json5_value("{value: '${P3_RECHECK_INJECT}'}").unwrap();
        std::env::remove_var("P3_RECHECK_INJECT");
        assert!(
            result.get("injected").is_none(),
            "unexpected injected key: {result}"
        );
    }
}
