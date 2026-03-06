use blockcell_core::{
    CapabilityDescriptor, CapabilityLifecycle, CapabilityStatus, CapabilityType, Error,
    ProviderKind, Result,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info};

/// 动态能力的执行接口
///
/// 所有通过动态库或 IPC 加载的能力都实现此 trait。
/// 这是 Capability Substrate 层的核心抽象。
#[async_trait::async_trait]
pub trait CapabilityExecutor: Send + Sync {
    /// 执行能力，输入输出都是 JSON
    async fn execute(&self, input: serde_json::Value) -> Result<serde_json::Value>;
    /// 健康检查
    async fn health_check(&self) -> Result<bool>;
    /// 关闭 / 释放资源
    async fn shutdown(&self) -> Result<()>;
}

/// 进程型能力提供者 — 通过子进程 + stdin/stdout JSON-RPC 通信
pub struct ProcessProvider {
    #[allow(dead_code)]
    capability_id: String,
    command: String,
    args: Vec<String>,
    working_dir: Option<PathBuf>,
    #[allow(dead_code)]
    timeout_secs: u64,
}

impl ProcessProvider {
    pub fn new(capability_id: &str, command: &str) -> Self {
        Self {
            capability_id: capability_id.to_string(),
            command: command.to_string(),
            args: Vec::new(),
            working_dir: None,
            timeout_secs: 30,
        }
    }

    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    pub fn with_working_dir(mut self, dir: PathBuf) -> Self {
        self.working_dir = Some(dir);
        self
    }
}

#[async_trait::async_trait]
impl CapabilityExecutor for ProcessProvider {
    async fn execute(&self, input: serde_json::Value) -> Result<serde_json::Value> {
        use std::process::Stdio;
        use tokio::process::Command;

        let input_str = serde_json::to_string(&input)?;

        let mut cmd = Command::new(&self.command);
        cmd.args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(ref dir) = self.working_dir {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().map_err(|e| {
            Error::Tool(format!("Failed to spawn process '{}': {}", self.command, e))
        })?;

        // Write input to stdin
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin
                .write_all(input_str.as_bytes())
                .await
                .map_err(|e| Error::Tool(format!("Failed to write to process stdin: {}", e)))?;
            drop(stdin);
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| Error::Tool(format!("Process execution failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Tool(format!(
                "Process exited with code {:?}: {}",
                output.status.code(),
                stderr
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let result: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|_| serde_json::json!({ "output": stdout.to_string() }));

        Ok(result)
    }

    async fn health_check(&self) -> Result<bool> {
        // Check if the command binary exists
        Ok(which::which(&self.command).is_ok())
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

/// 脚本型能力提供者 — 通过 shell 脚本执行
pub struct ScriptProvider {
    #[allow(dead_code)]
    capability_id: String,
    script_path: PathBuf,
    interpreter: String,
}

impl ScriptProvider {
    pub fn new(capability_id: &str, script_path: PathBuf) -> Self {
        // Auto-detect interpreter from extension
        let interpreter = match script_path.extension().and_then(|e| e.to_str()) {
            Some("py") => "python3".to_string(),
            Some("js") => "node".to_string(),
            Some("rb") => "ruby".to_string(),
            Some("sh") | Some("bash") => "bash".to_string(),
            _ => "bash".to_string(),
        };
        Self {
            capability_id: capability_id.to_string(),
            script_path,
            interpreter,
        }
    }
}

#[async_trait::async_trait]
impl CapabilityExecutor for ScriptProvider {
    async fn execute(&self, input: serde_json::Value) -> Result<serde_json::Value> {
        use std::process::Stdio;
        use tokio::process::Command;

        let input_str = serde_json::to_string(&input)?;

        let output = Command::new(&self.interpreter)
            .arg(self.script_path.to_str().unwrap_or(""))
            .env("CAPABILITY_INPUT", &input_str)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| Error::Tool(format!("Script execution failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Tool(format!("Script failed: {}", stderr)));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let result: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|_| serde_json::json!({ "output": stdout.to_string() }));

        Ok(result)
    }

    async fn health_check(&self) -> Result<bool> {
        Ok(self.script_path.exists() && which::which(&self.interpreter).is_ok())
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

/// 能力注册表 — 管理所有已发现和已加载的能力
///
/// 这是 Capability Substrate 层的核心注册中心。
/// Agent Core 通过此注册表发现、调度和管理能力。
/// Canary tracking for a capability in shadow stage.
#[derive(Debug, Clone)]
struct CanaryTracker {
    total_calls: u32,
    error_calls: u32,
    #[allow(dead_code)]
    started_at: i64,
}

impl CanaryTracker {
    fn new() -> Self {
        Self {
            total_calls: 0,
            error_calls: 0,
            started_at: chrono::Utc::now().timestamp(),
        }
    }

    fn record(&mut self, is_error: bool) {
        self.total_calls += 1;
        if is_error {
            self.error_calls += 1;
        }
    }

    fn error_rate(&self) -> f64 {
        if self.total_calls == 0 {
            0.0
        } else {
            self.error_calls as f64 / self.total_calls as f64
        }
    }
}

/// Canary configuration for capability validation.
const CANARY_MIN_CALLS: u32 = 5;
const CANARY_MAX_ERROR_RATE: f64 = 0.10;

pub struct CapabilityRegistry {
    /// 所有已知能力的描述符
    descriptors: HashMap<String, CapabilityDescriptor>,
    /// 已加载的能力执行器
    executors: HashMap<String, Arc<dyn CapabilityExecutor>>,
    /// 能力生命周期状态
    lifecycles: HashMap<String, CapabilityLifecycle>,
    /// 持久化目录
    registry_dir: PathBuf,
    /// Canary trackers for capabilities in shadow stage
    canary_trackers: HashMap<String, CanaryTracker>,
}

impl CapabilityRegistry {
    pub fn new(registry_dir: PathBuf) -> Self {
        Self {
            descriptors: HashMap::new(),
            executors: HashMap::new(),
            lifecycles: HashMap::new(),
            registry_dir,
            canary_trackers: HashMap::new(),
        }
    }

    /// 注册一个能力描述符
    pub fn register(&mut self, descriptor: CapabilityDescriptor) {
        info!(
            capability_id = %descriptor.id,
            name = %descriptor.name,
            kind = ?descriptor.provider_kind,
            "🔌 [能力] 注册能力: {}",
            descriptor.id
        );
        let id = descriptor.id.clone();
        self.lifecycles
            .insert(id.clone(), CapabilityLifecycle::Draft);
        self.descriptors.insert(id, descriptor);
    }

    /// 注册并同时绑定执行器
    ///
    /// Newly registered capabilities enter a canary (shadow) stage.
    /// After CANARY_MIN_CALLS successful executions with error rate < CANARY_MAX_ERROR_RATE,
    /// they are automatically promoted to Active.
    pub fn register_with_executor(
        &mut self,
        descriptor: CapabilityDescriptor,
        executor: Arc<dyn CapabilityExecutor>,
    ) {
        let id = descriptor.id.clone();
        let is_builtin = matches!(descriptor.provider_kind, ProviderKind::BuiltIn);
        info!(
            capability_id = %id,
            "🔌 [能力] 注册能力并绑定执行器: {}",
            id
        );

        // Insert descriptor and executor first
        self.descriptors.insert(id.clone(), descriptor);
        self.executors.insert(id.clone(), executor);

        if is_builtin {
            // Built-in capabilities skip canary
            self.lifecycles
                .insert(id.clone(), CapabilityLifecycle::Active);
        } else {
            // Evolved capabilities enter canary (Observing) stage
            self.lifecycles
                .insert(id.clone(), CapabilityLifecycle::Observing);
            self.canary_trackers
                .insert(id.clone(), CanaryTracker::new());
            // Set descriptor status to Available (not Active) until canary passes
            if let Some(desc) = self.descriptors.get_mut(&id) {
                desc.status = CapabilityStatus::Available;
            }
            info!(capability_id = %id, "🔌 [能力] 进入灰度验证阶段 (Observing)");
        }
    }

    /// 获取能力描述符
    pub fn get_descriptor(&self, id: &str) -> Option<&CapabilityDescriptor> {
        self.descriptors.get(id)
    }

    /// 获取能力执行器
    pub fn get_executor(&self, id: &str) -> Option<&Arc<dyn CapabilityExecutor>> {
        self.executors.get(id)
    }

    /// 绑定执行器到已注册的能力
    pub fn bind_executor(&mut self, id: &str, executor: Arc<dyn CapabilityExecutor>) -> Result<()> {
        if !self.descriptors.contains_key(id) {
            return Err(Error::NotFound(format!(
                "Capability '{}' not registered",
                id
            )));
        }
        self.executors.insert(id.to_string(), executor);
        self.lifecycles
            .insert(id.to_string(), CapabilityLifecycle::Active);
        if let Some(desc) = self.descriptors.get_mut(id) {
            desc.status = CapabilityStatus::Active;
            desc.updated_at = chrono::Utc::now().timestamp();
        }
        info!(capability_id = %id, "🔌 [能力] 执行器已绑定: {}", id);
        Ok(())
    }

    /// 执行一个能力
    ///
    /// If the capability is in canary stage, execution results are tracked.
    /// After CANARY_MIN_CALLS with error rate < CANARY_MAX_ERROR_RATE, it is promoted.
    /// If error rate exceeds threshold, the capability is marked unavailable.
    pub async fn execute(
        &mut self,
        id: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let executor = self
            .executors
            .get(id)
            .ok_or_else(|| Error::NotFound(format!("No executor for capability '{}'", id)))?
            .clone();

        debug!(capability_id = %id, "🔌 [能力] 执行: {}", id);
        let result = executor.execute(input).await;

        // Track canary results — collect decision first to avoid borrow conflicts
        let canary_action = if let Some(tracker) = self.canary_trackers.get_mut(id) {
            tracker.record(result.is_err());
            if tracker.total_calls >= CANARY_MIN_CALLS {
                let rate = tracker.error_rate();
                let calls = tracker.total_calls;
                if rate <= CANARY_MAX_ERROR_RATE {
                    Some((true, calls, rate)) // promote
                } else {
                    Some((false, calls, rate)) // fail
                }
            } else {
                None // not enough calls yet
            }
        } else {
            None
        };

        if let Some((passed, calls, rate)) = canary_action {
            self.canary_trackers.remove(id);
            if passed {
                // Promote: Observing → Active
                self.lifecycles
                    .insert(id.to_string(), CapabilityLifecycle::Active);
                if let Some(desc) = self.descriptors.get_mut(id) {
                    desc.status = CapabilityStatus::Active;
                    desc.updated_at = chrono::Utc::now().timestamp();
                }
                info!(
                    capability_id = %id,
                    calls = calls,
                    error_rate = rate,
                    "🔌 [能力] ✅ 灰度验证通过，已提升为 Active: {}", id
                );
            } else {
                info!(
                    capability_id = %id,
                    calls = calls,
                    error_rate = rate,
                    "🔌 [能力] ❌ 灰度验证失败，标记为不可用: {}", id
                );
                self.set_status(
                    id,
                    CapabilityStatus::Unavailable {
                        reason: format!(
                            "Canary failed: error rate {:.0}% after {} calls",
                            rate * 100.0,
                            calls
                        ),
                    },
                );
            }
        }

        result
    }

    /// 列出所有能力
    pub fn list_all(&self) -> Vec<&CapabilityDescriptor> {
        self.descriptors.values().collect()
    }

    /// 按类型列出能力
    pub fn list_by_type(&self, cap_type: &CapabilityType) -> Vec<&CapabilityDescriptor> {
        self.descriptors
            .values()
            .filter(|d| &d.capability_type == cap_type)
            .collect()
    }

    /// 列出可用能力
    pub fn list_available(&self) -> Vec<&CapabilityDescriptor> {
        self.descriptors
            .values()
            .filter(|d| d.is_available())
            .collect()
    }

    /// 按提供者类型列出
    pub fn list_by_provider(&self, kind: &ProviderKind) -> Vec<&CapabilityDescriptor> {
        self.descriptors
            .values()
            .filter(|d| &d.provider_kind == kind)
            .collect()
    }

    /// 更新能力状态
    pub fn set_status(&mut self, id: &str, status: CapabilityStatus) {
        if let Some(desc) = self.descriptors.get_mut(id) {
            desc.status = status;
            desc.updated_at = chrono::Utc::now().timestamp();
        }
    }

    /// 卸载能力（移除执行器但保留描述符）
    pub fn unload(&mut self, id: &str) {
        self.executors.remove(id);
        self.lifecycles
            .insert(id.to_string(), CapabilityLifecycle::Retired);
        self.set_status(
            id,
            CapabilityStatus::Unavailable {
                reason: "Unloaded".to_string(),
            },
        );
        info!(capability_id = %id, "🔌 [能力] 已卸载: {}", id);
    }

    /// 替换能力执行器（热更新）
    pub fn replace_executor(
        &mut self,
        id: &str,
        new_executor: Arc<dyn CapabilityExecutor>,
        new_version: &str,
    ) -> Result<()> {
        if !self.descriptors.contains_key(id) {
            return Err(Error::NotFound(format!(
                "Capability '{}' not registered",
                id
            )));
        }

        // 先标记为替换中
        self.lifecycles
            .insert(id.to_string(), CapabilityLifecycle::Replacing);

        // 替换执行器
        self.executors.insert(id.to_string(), new_executor);

        // 更新版本和状态
        if let Some(desc) = self.descriptors.get_mut(id) {
            desc.version = new_version.to_string();
            desc.status = CapabilityStatus::Active;
            desc.updated_at = chrono::Utc::now().timestamp();
        }
        self.lifecycles
            .insert(id.to_string(), CapabilityLifecycle::Active);

        info!(
            capability_id = %id,
            version = %new_version,
            "🔌 [能力] 热更新完成: {} -> v{}",
            id, new_version
        );
        Ok(())
    }

    /// 健康检查所有已加载的能力
    pub async fn health_check_all(&self) -> HashMap<String, bool> {
        let mut results = HashMap::new();
        for (id, executor) in &self.executors {
            let healthy = executor.health_check().await.unwrap_or(false);
            results.insert(id.clone(), healthy);
        }
        results
    }

    /// 生成能力摘要（用于注入到 system prompt）
    pub fn generate_brief(&self) -> String {
        let mut brief = String::new();

        let by_type: HashMap<CapabilityType, Vec<&CapabilityDescriptor>> = {
            let mut map: HashMap<CapabilityType, Vec<&CapabilityDescriptor>> = HashMap::new();
            for desc in self.descriptors.values() {
                map.entry(desc.capability_type.clone())
                    .or_default()
                    .push(desc);
            }
            map
        };

        let type_order = [
            CapabilityType::Hardware,
            CapabilityType::System,
            CapabilityType::External,
            CapabilityType::Internal,
        ];

        for cap_type in &type_order {
            if let Some(caps) = by_type.get(cap_type) {
                let type_name = match cap_type {
                    CapabilityType::Hardware => "硬件能力",
                    CapabilityType::System => "系统能力",
                    CapabilityType::External => "外部能力",
                    CapabilityType::Internal => "内部能力",
                };
                brief.push_str(&format!("### {}\n", type_name));
                for cap in caps {
                    let is_shadow = self.canary_trackers.contains_key(&cap.id);
                    let status_icon = if is_shadow {
                        "🔬" // shadow / canary
                    } else {
                        match &cap.status {
                            CapabilityStatus::Active => "✅",
                            CapabilityStatus::Available => "🟢",
                            CapabilityStatus::Discovered => "🔍",
                            CapabilityStatus::Loading => "⏳",
                            CapabilityStatus::Evolving => "🧬",
                            CapabilityStatus::Unavailable { .. } => "❌",
                            CapabilityStatus::Deprecated => "⚠️",
                        }
                    };
                    let shadow_tag = if is_shadow { " [shadow]" } else { "" };
                    brief.push_str(&format!(
                        "- {} `{}` (v{}){} — {}\n",
                        status_icon, cap.id, cap.version, shadow_tag, cap.description
                    ));
                }
                brief.push('\n');
            }
        }

        brief
    }

    /// 持久化注册表到磁盘
    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(&self.registry_dir)?;
        let registry_file = self.registry_dir.join("evolved_tools.json");
        let descriptors: Vec<&CapabilityDescriptor> = self.descriptors.values().collect();
        let json = serde_json::to_string_pretty(&descriptors)?;
        std::fs::write(registry_file, json)?;
        debug!("🔌 [能力] 注册表已保存到磁盘");
        Ok(())
    }

    /// 从磁盘加载注册表
    pub fn load(&mut self) -> Result<()> {
        let registry_file = self.registry_dir.join("evolved_tools.json");
        if !registry_file.exists() {
            return Ok(());
        }
        let json = std::fs::read_to_string(&registry_file)?;
        let descriptors: Vec<CapabilityDescriptor> = serde_json::from_str(&json)?;
        for desc in descriptors {
            let id = desc.id.clone();
            self.descriptors.insert(id.clone(), desc);
            // Loaded from disk = Draft until executor is bound
            self.lifecycles
                .entry(id)
                .or_insert(CapabilityLifecycle::Draft);
        }
        info!(
            count = self.descriptors.len(),
            "🔌 [能力] 从磁盘加载了 {} 个能力描述符",
            self.descriptors.len()
        );
        Ok(())
    }

    /// Rehydrate executors from persisted descriptors.
    /// After `load()`, descriptors exist but executors are missing.
    /// This method rebuilds executors for descriptors that have a `provider_path`.
    pub fn rehydrate_executors(&mut self) -> usize {
        let mut rehydrated = 0;
        let ids_to_rehydrate: Vec<(String, String, ProviderKind)> = self
            .descriptors
            .iter()
            .filter(|(id, _)| !self.executors.contains_key(*id))
            .filter_map(|(id, desc)| {
                desc.provider_path
                    .as_ref()
                    .map(|path| (id.clone(), path.clone(), desc.provider_kind.clone()))
            })
            .collect();

        for (id, path, kind) in ids_to_rehydrate {
            if !std::path::Path::new(&path).exists() {
                info!(
                    capability_id = %id,
                    path = %path,
                    "🔌 [能力] 跳过 rehydrate: artifact 文件不存在"
                );
                continue;
            }

            let ext = std::path::Path::new(&path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("sh");

            let executor: Arc<dyn CapabilityExecutor> = match (kind, ext) {
                (ProviderKind::ExternalApi, _) | (_, "py") => {
                    Arc::new(ScriptProvider::new(&id, std::path::PathBuf::from(&path)))
                }
                (ProviderKind::RhaiScript, _) | (_, "rhai") => {
                    Arc::new(ScriptProvider::new(&id, std::path::PathBuf::from(&path)))
                }
                _ => Arc::new(ProcessProvider::new(&id, "bash").with_args(vec![path.clone()])),
            };

            self.executors.insert(id.clone(), executor);
            self.lifecycles
                .insert(id.clone(), CapabilityLifecycle::Active);
            if let Some(desc) = self.descriptors.get_mut(&id) {
                desc.status = CapabilityStatus::Active;
            }
            rehydrated += 1;
            info!(
                capability_id = %id,
                "🔌 [能力] ✅ Rehydrated executor from disk: {}", id
            );
        }

        if rehydrated > 0 {
            info!(
                count = rehydrated,
                "🔌 [能力] Rehydrated {} executors from disk", rehydrated
            );
        }

        rehydrated
    }

    /// 获取注册表统计
    pub fn stats(&self) -> RegistryStats {
        let total = self.descriptors.len();
        let active = self
            .descriptors
            .values()
            .filter(|d| matches!(d.status, CapabilityStatus::Active))
            .count();
        let available = self
            .descriptors
            .values()
            .filter(|d| d.is_available())
            .count();
        let evolving = self
            .descriptors
            .values()
            .filter(|d| matches!(d.status, CapabilityStatus::Evolving))
            .count();

        RegistryStats {
            total,
            active,
            available,
            evolving,
        }
    }
}

/// 注册表统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryStats {
    pub total: usize,
    pub active: usize,
    pub available: usize,
    pub evolving: usize,
}

/// 线程安全的能力注册表句柄
pub type CapabilityRegistryHandle = Arc<Mutex<CapabilityRegistry>>;

/// 创建一个线程安全的注册表句柄
pub fn new_registry_handle(registry_dir: PathBuf) -> CapabilityRegistryHandle {
    Arc::new(Mutex::new(CapabilityRegistry::new(registry_dir)))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct MockExecutor;

    #[async_trait::async_trait]
    impl CapabilityExecutor for MockExecutor {
        async fn execute(&self, input: serde_json::Value) -> Result<serde_json::Value> {
            Ok(serde_json::json!({ "echo": input }))
        }
        async fn health_check(&self) -> Result<bool> {
            Ok(true)
        }
        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_registry_register_and_list() {
        let dir = std::env::temp_dir().join("test_cap_registry");
        let mut registry = CapabilityRegistry::new(dir);

        let cap = CapabilityDescriptor::new(
            "system.clipboard",
            "Clipboard",
            "Read/write system clipboard",
            CapabilityType::System,
            ProviderKind::Process,
        );
        registry.register(cap);

        assert_eq!(registry.list_all().len(), 1);
        assert!(registry.get_descriptor("system.clipboard").is_some());
    }

    #[tokio::test]
    async fn test_registry_execute() {
        let dir = std::env::temp_dir().join("test_cap_registry_exec");
        let mut registry = CapabilityRegistry::new(dir);

        let cap = CapabilityDescriptor::new(
            "test.echo",
            "Echo",
            "Echo input",
            CapabilityType::Internal,
            ProviderKind::BuiltIn,
        )
        .with_status(CapabilityStatus::Available);

        registry.register_with_executor(cap, Arc::new(MockExecutor));

        let result = registry
            .execute("test.echo", serde_json::json!({"msg": "hello"}))
            .await
            .unwrap();
        assert_eq!(result["echo"]["msg"], "hello");
    }

    #[tokio::test]
    async fn test_registry_replace_executor() {
        let dir = std::env::temp_dir().join("test_cap_registry_replace");
        let mut registry = CapabilityRegistry::new(dir);

        let cap = CapabilityDescriptor::new(
            "test.replace",
            "Replace Test",
            "Test hot replacement",
            CapabilityType::Internal,
            ProviderKind::BuiltIn,
        )
        .with_status(CapabilityStatus::Available);

        registry.register_with_executor(cap, Arc::new(MockExecutor));
        assert_eq!(
            registry.get_descriptor("test.replace").unwrap().version,
            env!("CARGO_PKG_VERSION")
        );

        // Replace with new version
        registry
            .replace_executor("test.replace", Arc::new(MockExecutor), "0.2.0")
            .unwrap();
        assert_eq!(
            registry.get_descriptor("test.replace").unwrap().version,
            "0.2.0"
        );
    }

    #[test]
    fn test_registry_stats() {
        let dir = std::env::temp_dir().join("test_cap_registry_stats");
        let mut registry = CapabilityRegistry::new(dir);

        registry.register(
            CapabilityDescriptor::new(
                "a",
                "A",
                "a",
                CapabilityType::Hardware,
                ProviderKind::BuiltIn,
            )
            .with_status(CapabilityStatus::Active),
        );

        registry.register(
            CapabilityDescriptor::new("b", "B", "b", CapabilityType::System, ProviderKind::Process)
                .with_status(CapabilityStatus::Available),
        );

        registry.register(
            CapabilityDescriptor::new(
                "c",
                "C",
                "c",
                CapabilityType::Internal,
                ProviderKind::BuiltIn,
            )
            .with_status(CapabilityStatus::Evolving),
        );

        let stats = registry.stats();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.active, 1);
        assert_eq!(stats.available, 2); // Active + Available
        assert_eq!(stats.evolving, 1);
    }
}
