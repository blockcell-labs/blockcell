use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 能力类型 — 对应文档中的 Capability Substrate 层
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityType {
    /// 硬件能力：camera, mic, gpu, storage, bluetooth, usb
    Hardware,
    /// 系统能力：process, fs, network, clipboard, notifications
    System,
    /// 外部能力：LLM, Search, API, database
    External,
    /// 内部能力：compile, load, reflect, evolve
    Internal,
}

/// 能力权限级别
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PrivilegeLevel {
    /// 只读 / 观察
    ReadOnly,
    /// 有限写入
    Limited,
    /// 完全控制
    Full,
}

/// 能力状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CapabilityStatus {
    /// 已发现但未激活
    Discovered,
    /// 可用
    Available,
    /// 正在加载 / 编译中
    Loading,
    /// 已激活，正常运行
    Active,
    /// 暂时不可用（设备断开等）
    Unavailable { reason: String },
    /// 已废弃（被新版本替代）
    Deprecated,
    /// 进化中（正在生成新版本）
    Evolving,
}

/// 能力的资源消耗估算
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapabilityCost {
    /// CPU 时间估算（毫秒）
    pub cpu_ms: Option<u64>,
    /// 内存消耗估算（字节）
    pub memory_bytes: Option<u64>,
    /// 能耗估算（0.0 - 1.0 相对值）
    pub energy: Option<f64>,
    /// 网络流量估算（字节）
    pub network_bytes: Option<u64>,
}

/// 能力提供者类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// 内置 Rust 工具（编译进二进制）
    BuiltIn,
    /// Rhai 脚本（现有 skill 系统）
    RhaiScript,
    /// 动态库（.dylib / .so / .dll）
    DynamicLibrary,
    /// 独立进程（通过 IPC 通信）
    Process,
    /// 外部 API
    ExternalApi,
}

/// 能力描述符 — 对应文档中的 capability YAML 定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    /// 能力唯一标识，格式: category.name (如 vision.observe, audio.record)
    pub id: String,
    /// 人类可读名称
    pub name: String,
    /// 描述
    pub description: String,
    /// 能力类型
    pub capability_type: CapabilityType,
    /// 提供者类型
    pub provider_kind: ProviderKind,
    /// 权限级别
    pub privilege: PrivilegeLevel,
    /// 当前状态
    pub status: CapabilityStatus,
    /// 输入参数 schema（JSON Schema 格式）
    pub input_schema: Option<serde_json::Value>,
    /// 输出类型描述
    pub output_schema: Option<serde_json::Value>,
    /// 资源消耗估算
    pub cost: CapabilityCost,
    /// 版本号
    pub version: String,
    /// 提供者路径（动态库路径、脚本路径、进程路径等）
    pub provider_path: Option<String>,
    /// 依赖的其他能力 ID
    pub dependencies: Vec<String>,
    /// 元数据
    pub metadata: HashMap<String, serde_json::Value>,
    /// 创建时间
    pub created_at: i64,
    /// 最后更新时间
    pub updated_at: i64,
}

impl CapabilityDescriptor {
    pub fn new(
        id: &str,
        name: &str,
        description: &str,
        capability_type: CapabilityType,
        provider_kind: ProviderKind,
    ) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            id: id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            capability_type,
            provider_kind,
            privilege: PrivilegeLevel::ReadOnly,
            status: CapabilityStatus::Discovered,
            input_schema: None,
            output_schema: None,
            cost: CapabilityCost::default(),
            version: "0.1.0".to_string(),
            provider_path: None,
            dependencies: Vec::new(),
            metadata: HashMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_privilege(mut self, privilege: PrivilegeLevel) -> Self {
        self.privilege = privilege;
        self
    }

    pub fn with_status(mut self, status: CapabilityStatus) -> Self {
        self.status = status;
        self
    }

    pub fn with_provider_path(mut self, path: &str) -> Self {
        self.provider_path = Some(path.to_string());
        self
    }

    pub fn is_available(&self) -> bool {
        matches!(self.status, CapabilityStatus::Available | CapabilityStatus::Active)
    }
}

/// 能力生命周期阶段 — 对应文档中的:
/// draft → compile → validate → load → observe → optimize → replace
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityLifecycle {
    Draft,
    Compiling,
    Validating,
    Loading,
    Observing,
    Optimizing,
    Replacing,
    Active,
    Retired,
}

/// 生存不变量 — 对应文档第 7 节 Meta-Evolution 的关键不变量
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurvivalInvariants {
    /// 是否还能编译新代码？
    pub can_compile: bool,
    /// 是否还能加载新能力？
    pub can_load_capabilities: bool,
    /// 是否还能与主人通信？
    pub can_communicate: bool,
    /// 是否还能继续进化？
    pub can_evolve: bool,
    /// 最后检查时间
    pub last_checked: i64,
    /// 详细诊断信息
    pub diagnostics: HashMap<String, String>,
}

impl Default for SurvivalInvariants {
    fn default() -> Self {
        Self {
            can_compile: false,
            can_load_capabilities: false,
            can_communicate: false,
            can_evolve: false,
            last_checked: 0,
            diagnostics: HashMap::new(),
        }
    }
}

impl SurvivalInvariants {
    /// 所有不变量是否都满足？
    pub fn all_healthy(&self) -> bool {
        self.can_compile && self.can_load_capabilities && self.can_communicate && self.can_evolve
    }

    /// 返回不满足的不变量列表
    pub fn violations(&self) -> Vec<&str> {
        let mut v = Vec::new();
        if !self.can_compile { v.push("cannot compile new code"); }
        if !self.can_load_capabilities { v.push("cannot load new capabilities"); }
        if !self.can_communicate { v.push("cannot communicate with owner"); }
        if !self.can_evolve { v.push("cannot continue evolution"); }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_descriptor_creation() {
        let cap = CapabilityDescriptor::new(
            "vision.observe",
            "Camera Observation",
            "Observe the environment through camera",
            CapabilityType::Hardware,
            ProviderKind::DynamicLibrary,
        ).with_privilege(PrivilegeLevel::Full)
         .with_status(CapabilityStatus::Available);

        assert_eq!(cap.id, "vision.observe");
        assert!(cap.is_available());
        assert_eq!(cap.privilege, PrivilegeLevel::Full);
    }

    #[test]
    fn test_survival_invariants() {
        let mut inv = SurvivalInvariants::default();
        assert!(!inv.all_healthy());
        assert_eq!(inv.violations().len(), 4);

        inv.can_compile = true;
        inv.can_load_capabilities = true;
        inv.can_communicate = true;
        inv.can_evolve = true;
        assert!(inv.all_healthy());
        assert!(inv.violations().is_empty());
    }
}
