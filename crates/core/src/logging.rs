//! # 日志系统
//!
//! 提供可动态控制的日志输出系统：
//! - 控制台输出（可开关，默认开启）
//! - 文件输出（可开关，默认开启，按日期滚动）
//! - 日志等级动态调整（trace/debug/info/warn/error/off）
//! - 模块过滤（如 blockcell_agent=trace）

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use tracing::Subscriber;
use tracing_appender::rolling::RollingFileAppender;
use tracing_appender::rolling::Rotation;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{
    layer::{Context, Layer},
    reload, EnvFilter, Registry,
};

/// 全局日志控制器单例
pub static LOG_CONTROLLER: OnceLock<LogController> = OnceLock::new();

/// 日志控制器
pub struct LogController {
    /// EnvFilter reload handle
    filter_handle: reload::Handle<EnvFilter, Registry>,
    /// Console output switch (default: true)
    console_enabled: Arc<Mutex<bool>>,
    /// File output switch (default: true)
    file_enabled: Arc<Mutex<bool>>,
    /// Current log file path
    current_file: Arc<Mutex<String>>,
}

/// 日志状态
pub struct LogStatus {
    pub level: String,
    pub module_filters: Vec<String>,
    pub console_enabled: bool,
    pub file_enabled: bool,
    pub log_file: String,
}

impl LogController {
    /// 设置全局日志等级
    pub fn set_level(&self, level: &str) -> Result<(), String> {
        let new_filter = match level {
            "trace" => EnvFilter::new("trace"),
            "debug" => EnvFilter::new("debug"),
            "info" => EnvFilter::new("info"),
            "warn" => EnvFilter::new("warn"),
            "error" => EnvFilter::new("error"),
            "off" => EnvFilter::new("off"),
            other => return Err(format!("Unknown log level: {}", other)),
        };

        self.filter_handle
            .reload(new_filter)
            .map_err(|e| format!("Failed to reload filter: {}", e))?;

        Ok(())
    }

    /// 设置模块过滤
    pub fn set_filter(&self, filter: &str) -> Result<(), String> {
        let new_filter = EnvFilter::new(filter);

        self.filter_handle
            .reload(new_filter)
            .map_err(|e| format!("Failed to reload filter: {}", e))?;

        Ok(())
    }

    /// 切换控制台输出（独立控制，不影响文件）
    pub fn set_console(&self, enabled: bool) {
        *self.console_enabled.lock().unwrap() = enabled;
    }

    /// 切换文件输出（独立控制，不影响控制台）
    pub fn set_file(&self, enabled: bool) {
        *self.file_enabled.lock().unwrap() = enabled;
    }

    /// 获取当前状态
    pub fn status(&self) -> LogStatus {
        let current_filter = self
            .filter_handle
            .with_current(|f| f.to_string())
            .unwrap_or_default();

        let parts: Vec<&str> = current_filter.split(',').collect();
        let level = parts
            .first()
            .map(|s| s.split('=').next().unwrap_or("info"))
            .unwrap_or("info")
            .to_string();

        let module_filters = parts
            .iter()
            .filter(|p| p.contains('='))
            .map(|s| s.to_string())
            .collect();

        LogStatus {
            level,
            module_filters,
            console_enabled: *self.console_enabled.lock().unwrap(),
            file_enabled: *self.file_enabled.lock().unwrap(),
            log_file: self.current_file.lock().unwrap().clone(),
        }
    }
}

/// 可开关的控制台输出层。
///
/// 支持两种输出格式：
/// - 纯文本（默认）：`timestamp [LEVEL] module: message | fields`
/// - JSON（设置 `RUST_LOG_FORMAT=json`）：每行一条 JSON 记录
pub struct SwitchableConsoleLayer {
    enabled: Arc<Mutex<bool>>,
    /// 是否使用 JSON 格式输出（由 `RUST_LOG_FORMAT=json` 环境变量控制）
    json_format: bool,
}

impl SwitchableConsoleLayer {
    pub fn new(enabled: Arc<Mutex<bool>>, json_format: bool) -> Self {
        Self {
            enabled,
            json_format,
        }
    }
}

impl<S> Layer<S> for SwitchableConsoleLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if !*self.enabled.lock().unwrap() {
            return;
        }

        let mut stdout = std::io::stdout().lock();

        if self.json_format {
            let _ = writeln!(stdout, "{}", format_event_json(event));
        } else {
            let _ = writeln!(stdout, "{}", format_event_text(event));
        }
    }
}

/// 可开关的文件输出层。
///
/// 支持两种输出格式（与 [`SwitchableConsoleLayer`] 一致）：
/// - 纯文本（默认）
/// - JSON（设置 `RUST_LOG_FORMAT=json`）
pub struct SwitchableFileLayer {
    enabled: Arc<Mutex<bool>>,
    writer: Arc<Mutex<RollingFileAppender>>,
    /// 是否使用 JSON 格式输出
    json_format: bool,
}

impl SwitchableFileLayer {
    pub fn new(enabled: Arc<Mutex<bool>>, writer: RollingFileAppender, json_format: bool) -> Self {
        Self {
            enabled,
            writer: Arc::new(Mutex::new(writer)),
            json_format,
        }
    }
}

impl<S> Layer<S> for SwitchableFileLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if !*self.enabled.lock().unwrap() {
            return;
        }

        let mut writer = self.writer.lock().unwrap();

        if self.json_format {
            let _ = writeln!(writer, "{}", format_event_json(event));
        } else {
            let _ = writeln!(writer, "{}", format_event_text(event));
        }
    }
}

/// 消息访问器 — captures the "message" field as the primary text,
/// and all other structured fields as key=value pairs appended after it.
struct MessageVisitor {
    message: String,
    /// 非消息字段，格式: key1=val1, key2=val2
    fields: String,
}

impl MessageVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: String::new(),
        }
    }
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        } else {
            if !self.fields.is_empty() {
                self.fields.push_str(", ");
            }
            self.fields
                .push_str(&format!("{}={:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            if !self.fields.is_empty() {
                self.fields.push_str(", ");
            }
            self.fields.push_str(&format!("{}={}", field.name(), value));
        }
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if !self.fields.is_empty() {
            self.fields.push_str(", ");
        }
        self.fields.push_str(&format!("{}={}", field.name(), value));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if !self.fields.is_empty() {
            self.fields.push_str(", ");
        }
        self.fields.push_str(&format!("{}={}", field.name(), value));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if !self.fields.is_empty() {
            self.fields.push_str(", ");
        }
        self.fields.push_str(&format!("{}={}", field.name(), value));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        if !self.fields.is_empty() {
            self.fields.push_str(", ");
        }
        self.fields.push_str(&format!("{}={}", field.name(), value));
    }

    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        if !self.fields.is_empty() {
            self.fields.push_str(", ");
        }
        self.fields.push_str(&format!("{}={}", field.name(), value));
    }
}

/// 将事件格式化为纯文本日志行。
fn format_event_text(event: &tracing::Event<'_>) -> String {
    let now = chrono::Local::now();
    let timestamp = now.format("%Y-%m-%d %H:%M:%S%.3f");

    let level = event.metadata().level();
    let module = event.metadata().module_path().unwrap_or("unknown");

    let mut visitor = MessageVisitor::new();
    event.record(&mut visitor);

    if visitor.fields.is_empty() {
        format!("{} [{}] {}: {}", timestamp, level, module, visitor.message)
    } else {
        format!(
            "{} [{}] {}: {} | {}",
            timestamp, level, module, visitor.message, visitor.fields
        )
    }
}

/// 将事件格式化为 JSON 日志行。
///
/// 输出的 JSON 结构类似 tracing-subscriber JSON 格式：
/// ```json
/// {"timestamp":"2025-01-01T00:00:00.000Z","level":"INFO","target":"module::path","message":"...","fields":{}}
/// ```
fn format_event_json(event: &tracing::Event<'_>) -> String {
    use serde_json::json;

    let now = chrono::Utc::now();
    let timestamp = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();

    let level = event.metadata().level().to_string();
    let target = event
        .metadata()
        .module_path()
        .unwrap_or("unknown")
        .to_string();

    let mut visitor = JsonVisitor::new();
    event.record(&mut visitor);

    let record = json!({
        "timestamp": timestamp,
        "level": level,
        "target": target,
        "message": visitor.message,
        "fields": visitor.fields,
    });

    record.to_string()
}

/// JSON 格式的字段访问器。
struct JsonVisitor {
    message: String,
    fields: serde_json::Map<String, serde_json::Value>,
}

impl JsonVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: serde_json::Map::new(),
        }
    }
}

impl tracing::field::Visit for JsonVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let val = format!("{:?}", value);
        if field.name() == "message" {
            // 尝试用 JSON 反解去除 Debug 格式化产生的引号和转义（如 "\"text\"" → "text"）。
            // 失败时保留原始 debug 字符串，避免破坏非标准内容。
            let decoded = serde_json::from_str::<String>(&val);
            self.message = decoded.unwrap_or(val);
        } else {
            self.fields
                .insert(field.name().to_string(), serde_json::Value::String(val));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.insert(
                field.name().to_string(),
                serde_json::Value::String(value.to_string()),
            );
        }
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), serde_json::json!(value));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), serde_json::json!(value));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), serde_json::json!(value));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.fields
            .insert(field.name().to_string(), serde_json::json!(value));
    }

    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        self.fields.insert(
            field.name().to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }
}

/// 初始化日志系统
/// 参数：
/// - logs_dir: 日志目录路径
/// - level: 日志等级 (trace/debug/info/warn/error/off)
/// - console_enabled: 是否输出到控制台
/// - file_enabled: 是否输出到文件
pub fn init_logging(
    logs_dir: &Path,
    level: &str,
    console_enabled: bool,
    file_enabled: bool,
) -> Result<(), String> {
    use tracing_subscriber::prelude::*;

    if let Err(e) = std::fs::create_dir_all(logs_dir) {
        return Err(format!("Failed to create logs directory: {}", e));
    }

    // 读取 RUST_LOG_FORMAT 环境变量决定输出格式
    // json 模式不再跳过 file layer 和 LOG_CONTROLLER 初始化，
    // 而是作为 console/file layer 的输出格式选项
    let log_format = std::env::var("RUST_LOG_FORMAT").unwrap_or_default();
    let json_format = log_format == "json";

    let file_appender = RollingFileAppender::new(Rotation::DAILY, logs_dir, "agent.log");

    let filter = EnvFilter::new(level);
    let (filter_layer, filter_handle) = reload::Layer::new(filter);

    let console_enabled_flag = Arc::new(Mutex::new(console_enabled));
    let file_enabled_flag = Arc::new(Mutex::new(file_enabled));

    let console_layer = SwitchableConsoleLayer::new(console_enabled_flag.clone(), json_format);
    let file_layer =
        SwitchableFileLayer::new(file_enabled_flag.clone(), file_appender, json_format);

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(console_layer)
        .with(file_layer)
        .init();

    let controller = LogController {
        filter_handle,
        console_enabled: console_enabled_flag,
        file_enabled: file_enabled_flag,
        current_file: Arc::new(Mutex::new(logs_dir.join("agent.log").display().to_string())),
    };

    LOG_CONTROLLER
        .set(controller)
        .map_err(|_| "Log controller already initialized")?;

    Ok(())
}

/// 清理旧日志文件（超过 retention_days 天）
pub fn cleanup_old_logs(logs_dir: &Path, retention_days: u64) {
    let cutoff = SystemTime::now() - Duration::from_secs(retention_days * 86400);

    if let Ok(entries) = std::fs::read_dir(logs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            // 匹配 agent.log 或 agent.log.YYYY-MM-DD 格式
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let is_log_file = file_name == "agent.log" || file_name.starts_with("agent.log.");

            if path.is_file() && is_log_file {
                if let Ok(metadata) = entry.metadata() {
                    if let Ok(time) = metadata.modified() {
                        if time < cutoff {
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                }
            }
        }
    }
}

/// 清理所有日志文件，返回 (成功删除数, 删除的总大小)
///
/// RollingFileAppender 始终持有当日文件的 fd：对它 unlink 会让后续日志写入
/// 已删除的 inode（静默丢失直到重启/跨日轮转），因此对当日活动文件改为
/// 原地 truncate——写入偏移处产生空洞但后续日志不丢，跨日轮转后自愈。
/// 历史轮替文件不被持有，照常删除。
pub fn clear_all_logs(logs_dir: &Path) -> (usize, u64) {
    let mut count = 0;
    let mut total_size = 0u64;

    // RollingFileAppender 按本地日期命名当日活动文件
    let active_name = format!("agent.log.{}", chrono::Local::now().format("%Y-%m-%d"));

    if let Ok(entries) = std::fs::read_dir(logs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            // 匹配 agent.log 或 agent.log.YYYY-MM-DD 格式
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let is_log_file = file_name == "agent.log" || file_name.starts_with("agent.log.");

            if path.is_file() && is_log_file {
                // 先获取文件大小
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if file_name == active_name {
                    // 可能被 appender 持有——原地 truncate，不 unlink
                    if std::fs::OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open(&path)
                        .is_ok()
                    {
                        count += 1;
                        total_size += size;
                    }
                    continue;
                }
                // 尝试删除
                if std::fs::remove_file(&path).is_ok() {
                    count += 1;
                    total_size += size;
                } else {
                    // 如果删除失败（可能文件正在被写入），尝试清空内容
                    if std::fs::write(&path, "").is_ok() {
                        count += 1;
                        total_size += size;
                    }
                }
            }
        }
    }

    (count, total_size)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    #[test]
    fn test_logs_dir_path() {
        let dir = PathBuf::from("/tmp/logs");
        assert!(dir.ends_with("logs"));
    }

    #[test]
    fn clear_all_logs_truncates_active_file_instead_of_unlinking() {
        // 模拟 appender：当日活动文件被 fd 持有。unlink 会让之后经该 fd 的
        // 写入落到已删 inode（静默丢失）；truncate 则保留 fd 与路径的关联。
        let logs_dir = std::env::temp_dir()
            .join(format!("blockcell-logging-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&logs_dir).unwrap();
        let logs_dir = logs_dir.as_path();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let active = logs_dir.join(format!("agent.log.{today}"));
        let old = logs_dir.join("agent.log.2000-01-01");

        let mut held = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&active)
            .unwrap();
        held.write_all(b"line-before-clear\n").unwrap();
        std::fs::write(&old, b"old line\n").unwrap();

        let (count, _) = super::clear_all_logs(logs_dir);
        assert_eq!(count, 2);

        // 活动文件仍在但已清空；持 fd 继续写入的内容出现在该路径上（未丢失）
        assert!(active.exists());
        held.write_all(b"line-after-clear\n").unwrap();
        drop(held);
        let content = std::fs::read(&active).unwrap();
        assert!(
            content.windows(b"line-after-clear".len()).any(|w| w == b"line-after-clear"),
            "writes through the held fd must not be lost: {content:?}"
        );

        // 历史轮替文件被删除
        assert!(!old.exists());
    }
}
