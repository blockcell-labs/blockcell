//! Self-Evolution 的信任边界：Block = trusted host，Cell = mutable skill layer。
//!
//! 进化只能产出 [`EvolvableArtifact`]，不得修改 [`TrustedComponent`]。
//! 合法路径是 `Candidate → Static Check → Test → Sandbox → Observe → Promote`，
//! 而不是 `LLM → 生成原生代码 → 加载进宿主进程`。

use std::path::{Component, Path};

use crate::{Error, ProviderKind, Result};

/// 不可被进化修改的 Runtime Kernel 组件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustedComponent {
    AgentLoop,
    EventBus,
    Scope,
    Permission,
    Sandbox,
    StorageProtocol,
    /// 编译进宿主或以原生代码加载的能力。
    NativeCode,
}

/// 允许由进化产生或修改的产物。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvolvableArtifact {
    Skill,
    Prompt,
    Workflow,
    MemoryPolicy,
    ToolComposition,
    LearnedProcedure,
    /// 以独立进程 / 解释器运行的脚本能力（bash / python / rhai）。
    ScriptCapability,
}

/// 能力进化允许的 provider 类型：只允许在宿主进程外解释执行的脚本。
pub fn evolvable_provider_artifact(kind: &ProviderKind) -> Result<EvolvableArtifact> {
    match kind {
        ProviderKind::BuiltIn
        | ProviderKind::Process
        | ProviderKind::ExternalApi
        | ProviderKind::RhaiScript => Ok(EvolvableArtifact::ScriptCapability),
        ProviderKind::DynamicLibrary => Err(boundary_violation(
            TrustedComponent::NativeCode,
            "self-evolution must not generate native libraries; use a script provider (process / python / rhai)",
        )),
    }
}

/// 进化产物的文件名必须是单个普通路径分量，确保写入位置停留在给定根目录内。
pub fn ensure_single_path_component(name: &str) -> Result<()> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err(boundary_violation(
            TrustedComponent::StorageProtocol,
            &format!("evolution artifact name '{name}' must be a single path component"),
        )),
    }
}

fn boundary_violation(component: TrustedComponent, detail: &str) -> Error {
    Error::PermissionDenied(format!(
        "evolution boundary violation ({component:?}): {detail}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_out_of_process_script_providers_are_evolvable() {
        for kind in [
            ProviderKind::BuiltIn,
            ProviderKind::Process,
            ProviderKind::ExternalApi,
            ProviderKind::RhaiScript,
        ] {
            assert_eq!(
                evolvable_provider_artifact(&kind).unwrap(),
                EvolvableArtifact::ScriptCapability
            );
        }
        let err = evolvable_provider_artifact(&ProviderKind::DynamicLibrary).unwrap_err();
        assert!(err.to_string().contains("NativeCode"), "{err}");
    }

    #[test]
    fn artifact_names_cannot_escape_their_root() {
        assert!(ensure_single_path_component("weather_fetch").is_ok());
        for bad in ["", "..", "../x", "a/b", "/etc/passwd", "."] {
            assert!(ensure_single_path_component(bad).is_err(), "{bad}");
        }
    }
}
