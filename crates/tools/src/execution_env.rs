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
//! 唯一判定规则——新建会话看显式 `sandbox_policy`，复用会话看创建时固定的策略。`ShellTool`
//! 的环境 / 确认声明与技能脚本嵌套守卫共用它，避免规则漂移。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 工具声明的执行环境：在什么环境里运行、触及哪些外部资源。
///
/// 纯描述性声明，用于审计（`tool/call` 事件、工具调用审计记录），本身不改变工具的执行
/// 方式，也不决定是否需要用户确认（那是 [`crate::Tool::requires_confirmation`] 的职责）。
///
/// 字符串表示以 [`ExecutionEnvironment::as_str`] 为唯一来源：`Display`、`Serialize` 与
/// `Deserialize` 都走它，避免 `as_str` 与 serde 各写一份而漂移。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ExecutionEnvironment {
    /// 进程内纯计算：不访问文件 / 网络 / 子进程（如时间、上下文查询）。
    #[default]
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

    /// 解析 [`as_str`](Self::as_str) 产出的字符串；未知取值返回 `None`。
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "in-process" => Self::InProcess,
            "local" => Self::Local,
            "workspace-write" => Self::WorkspaceWrite,
            "network" => Self::Network,
            "shell" => Self::Shell,
            "full-access" => Self::FullAccess,
            _ => return None,
        })
    }
}

impl std::fmt::Display for ExecutionEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ExecutionEnvironment {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ExecutionEnvironment {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EnvironmentVisitor;

        impl serde::de::Visitor<'_> for EnvironmentVisitor {
            type Value = ExecutionEnvironment;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a tool execution environment string")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                ExecutionEnvironment::parse(value).ok_or_else(|| {
                    E::unknown_variant(
                        value,
                        &[
                            "in-process",
                            "local",
                            "workspace-write",
                            "network",
                            "shell",
                            "full-access",
                        ],
                    )
                })
            }
        }

        deserializer.deserialize_str(EnvironmentVisitor)
    }
}

/// `shell` 本次调用是否会向 full-access 会话注入执行或输入（因而需要用户显式确认）。
///
/// 这是 `ShellTool` 的确认声明与技能脚本嵌套守卫共用的唯一判定规则。
pub fn requests_full_access(tool_name: &str, args: &Value) -> bool {
    if tool_name != "shell" {
        return false;
    }
    let action = args.get("action").and_then(Value::as_str).unwrap_or("run");
    if !matches!(action, "run" | "write_stdin") {
        return false;
    }
    effective_sandbox_is_full_access(args)
}

/// 本次 `shell` 调用生效的沙箱策略是否为 full-access。
///
/// 新建会话看显式 `sandbox_policy`；复用会话（带 `session_id`）看**创建时固定**的策略——
/// 会话策略创建后不可更改（`execute` 会拒绝不一致的 `sandbox_policy`），所以复用时即便不
/// 重复声明，命令实际仍以会话策略运行。漏掉这一路会让"先建 full-access 会话、后续复用"
/// 绕过确认。
pub fn effective_sandbox_is_full_access(args: &Value) -> bool {
    if let Some(policy) = args.get("sandbox_policy").and_then(Value::as_str) {
        return policy == "full-access";
    }
    args.get("session_id")
        .and_then(Value::as_str)
        .and_then(crate::shell_session::session_sandbox_policy)
        .is_some_and(|policy| policy == crate::shell_session::SandboxPolicy::FullAccess)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn environment_names_are_stable() {
        // 固定对外字符串（审计与事件依赖它们，改动即破坏兼容）
        assert_eq!(ExecutionEnvironment::InProcess.as_str(), "in-process");
        assert_eq!(ExecutionEnvironment::Local.as_str(), "local");
        assert_eq!(ExecutionEnvironment::WorkspaceWrite.as_str(), "workspace-write");
        assert_eq!(ExecutionEnvironment::Network.as_str(), "network");
        assert_eq!(ExecutionEnvironment::Shell.as_str(), "shell");
        assert_eq!(ExecutionEnvironment::FullAccess.as_str(), "full-access");
        assert_eq!(ExecutionEnvironment::default(), ExecutionEnvironment::InProcess);

        // as_str / Display / 序列化 / 反序列化 共用同一份字符串，不会漂移
        for environment in [
            ExecutionEnvironment::InProcess,
            ExecutionEnvironment::Local,
            ExecutionEnvironment::WorkspaceWrite,
            ExecutionEnvironment::Network,
            ExecutionEnvironment::Shell,
            ExecutionEnvironment::FullAccess,
        ] {
            let encoded = serde_json::to_string(&environment).unwrap();
            assert_eq!(encoded, format!("\"{}\"", environment.as_str()));
            assert_eq!(environment.to_string(), environment.as_str());
            assert_eq!(
                serde_json::from_str::<ExecutionEnvironment>(&encoded).unwrap(),
                environment
            );
            assert_eq!(
                ExecutionEnvironment::parse(environment.as_str()),
                Some(environment)
            );
        }

        assert_eq!(ExecutionEnvironment::parse("nope"), None);
        assert!(serde_json::from_str::<ExecutionEnvironment>("\"nope\"").is_err());
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

    #[test]
    fn full_access_via_reused_session_is_detected() {
        use crate::shell_session::{insert_session_policy_for_test, SandboxPolicy};

        insert_session_policy_for_test("env-test-full", SandboxPolicy::FullAccess);
        insert_session_policy_for_test("env-test-write", SandboxPolicy::WorkspaceWrite);

        // 复用 full-access 会话：即使不重复声明 sandbox_policy，也必须按 full-access 处理
        assert!(requests_full_access(
            "shell",
            &json!({"command": "ls", "session_id": "env-test-full"})
        ));
        // 复用受限会话 / 未知会话 → 不请求
        assert!(!requests_full_access(
            "shell",
            &json!({"command": "ls", "session_id": "env-test-write"})
        ));
        assert!(!requests_full_access(
            "shell",
            &json!({"command": "ls", "session_id": "env-test-missing"})
        ));
        // 显式策略优先于会话查询
        assert!(!requests_full_access(
            "shell",
            &json!({
                "command": "ls",
                "session_id": "env-test-full",
                "sandbox_policy": "workspace-write"
            })
        ));
        // poll / close 不发送新命令或输入，不请求逐次确认，但 write_stdin 必须请求确认
        assert!(!requests_full_access(
            "shell",
            &json!({"action": "poll", "session_id": "env-test-full"})
        ));
        assert!(requests_full_access(
            "shell",
            &json!({"action": "write_stdin", "session_id": "env-test-full", "input": "id\n"})
        ));
        assert!(!requests_full_access(
            "shell",
            &json!({"action": "write_stdin", "session_id": "env-test-write", "input": "id\n"})
        ));
        // 环境声明对 full-access 会话的 poll / write_stdin / close 均如实反映 FullAccess
        assert!(effective_sandbox_is_full_access(
            &json!({"action": "poll", "session_id": "env-test-full"})
        ));
        assert!(effective_sandbox_is_full_access(
            &json!({"action": "write_stdin", "session_id": "env-test-full", "input": "id\n"})
        ));
    }
}
