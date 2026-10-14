//! 工具执行环境声明（P2-3）。
//!
//! 为工具声明其运行环境与所触及的外部资源，供 Runtime 在执行前判定是否需要额外确认，
//! 并在 `tool/call` 事件与审计中记录。这是"先声明、不重写执行层"的轻量接口：声明的
//! 环境不改变工具自身的执行路径，只让准入与可观测性有据可依——本地工具继续走简单路径。
//!
//! 单一来源：[`requests_full_access`] 是"本次调用是否请求 full-access 执行环境"的唯一
//! 判定规则。Runtime 的 FullAccess 准入阶段与技能脚本嵌套守卫共用它，避免两处规则漂移。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 工具声明的执行环境：在什么环境里运行、触及哪些外部资源。
///
/// 声明用于准入判定（如 `FullAccess` 需要用户显式确认）与审计（`tool/call` 事件），
/// 本身不改变工具的执行方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionEnvironment {
    /// 进程内纯计算：不访问文件 / 网络 / 子进程（如时间、上下文查询）。
    InProcess,
    /// 读取本地文件（工作区或已授权目录内）。
    Local,
    /// 写入本地文件（工作区或已授权目录内）。
    WorkspaceWrite,
    /// 访问网络（HTTP 请求 / 抓取 / 搜索 / 消息外发）。
    Network,
    /// 启动本地子进程 / shell（可能受沙箱限制）。
    Shell,
    /// 无限制本地访问（如 `sandbox_policy=full-access` 的 shell）。
    FullAccess,
}

impl ExecutionEnvironment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProcess => "in-process",
            Self::Local => "local",
            Self::WorkspaceWrite => "workspace-write",
            Self::Network => "network",
            Self::Shell => "shell",
            Self::FullAccess => "full-access",
        }
    }
}

impl Default for ExecutionEnvironment {
    fn default() -> Self {
        Self::InProcess
    }
}

/// 本次调用是否请求 full-access 执行环境。
///
/// 目前只有 `shell` 在 `action=run` 且 `sandbox_policy=full-access` 时请求。这是 Runtime
/// 的 FullAccess 准入阶段与技能脚本嵌套守卫共用的唯一判定规则。
pub fn requests_full_access(tool_name: &str, args: &Value) -> bool {
    tool_name == "shell"
        && args.get("action").and_then(Value::as_str).unwrap_or("run") == "run"
        && args.get("sandbox_policy").and_then(Value::as_str) == Some("full-access")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn environment_names_are_stable() {
        assert_eq!(ExecutionEnvironment::InProcess.as_str(), "in-process");
        assert_eq!(ExecutionEnvironment::WorkspaceWrite.as_str(), "workspace-write");
        assert_eq!(ExecutionEnvironment::FullAccess.as_str(), "full-access");
        assert_eq!(
            serde_json::to_string(&ExecutionEnvironment::Network).unwrap(),
            "\"network\""
        );
    }

    #[test]
    fn full_access_requires_shell_run_and_policy() {
        assert!(requests_full_access(
            "shell",
            &json!({"command": "ls", "sandbox_policy": "full-access"})
        ));
        assert!(requests_full_access(
            "shell",
            &json!({"action": "run", "sandbox_policy": "full-access"})
        ));
        // action 非 run / 未指定 full-access / 非 shell → 不请求
        assert!(!requests_full_access(
            "shell",
            &json!({"action": "kill", "sandbox_policy": "full-access"})
        ));
        assert!(!requests_full_access(
            "shell",
            &json!({"command": "ls", "sandbox_policy": "workspace-write"})
        ));
        assert!(!requests_full_access("shell", &json!({"command": "ls"})));
        assert!(!requests_full_access(
            "exec",
            &json!({"command": "ls", "sandbox_policy": "full-access"})
        ));
    }
}
