//! 工具执行环境声明（P2-3）。
//!
//! 工具用两个**正交**的声明描述一次调用：
//!
//! - [`ExecutionEnvironment`]：在什么环境里运行、触及哪些外部资源。**描述性**，只用于
//!   `tool/call` 事件与审计，本身不触发任何准入；
//! - [`crate::Tool::requires_confirmation`]：本次调用是否必须在执行前获得用户显式确认。
//!   **准入性**，Runtime 的 ExplicitConfirmation 阶段据此拦截。
//!
//! 二者分开，是因为"运行在无限制环境"与"每次都要用户点头"并不等价：`exec` 以
//! full-access 运行（环境即 `FullAccess`），但它的危险命令由 DangerousOperation 阶段
//! 按命令内容把关，不该每次调用都要求确认；`shell` 在 full-access 下没有命令级兜底，
//! 才必须逐次确认。若把两者混为一谈，环境声明就会为了绕开准入而失真（例如把 `exec`
//! 谎报成 `Shell`），审计也随之失准。
//!
//! 单一来源：[`requests_full_access`] 是"`shell` 本次调用是否会以 full-access 运行"的
//! 唯一判定规则，`ShellTool` 的环境 / 确认声明与技能脚本嵌套守卫共用它，避免规则漂移。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 工具声明的执行环境：在什么环境里运行、触及哪些外部资源。
///
/// 纯描述性声明，用于审计（`tool/call` 事件、工具调用审计记录），本身不改变工具的执行
/// 方式，也不决定是否需要用户确认（那是 [`crate::Tool::requires_confirmation`] 的职责）。
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
    /// 启动本地子进程 / shell（受沙箱限制，如 read-only / workspace-write）。
    Shell,
    /// 无限制本地访问（如以 `full-access` 沙箱运行的 shell、直接 `exec` 的命令）。
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

/// `shell` 本次调用是否会以 full-access 运行（因而需要用户显式确认）。
///
/// 目前只有 `shell` 在 `action=run` 且 `sandbox_policy=full-access` 时命中。这是 `ShellTool`
/// 的环境 / 确认声明与技能脚本嵌套守卫共用的唯一判定规则。
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
