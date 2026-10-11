//! Subagent capability：Agent 不直接依赖"子代理怎么启动"，只依赖 [`SubagentProvider`] 契约。
//!
//! 实现可以是进程内（当前 fork / typed agent）、独立进程、ACP 或远程 Agent。
//! 关键约束：
//! - **权限显式**：请求必须声明 [`SubagentPermissions`]，不存在"复制父代理全部权限"的选项；
//! - **所有权与取消**：[`SubagentHandle`] 拥有子代理；取消沿父 AbortToken 链传播；
//! - **会话血缘**：[`SubagentLineage`] 记录父会话与父 agent，结果据此回传。

use async_trait::async_trait;
use blockcell_core::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentKind {
    /// 继承父会话上下文、同步执行并返回结果。
    Fork,
    /// 按 agent 类型定义启动的专业化后台代理。
    Typed { agent_type: String },
}

/// 子代理可用的工具集合，必须由调用方显式选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolGrant {
    /// 只读工具集（不能写文件、执行命令、发消息或再派生子代理）。
    ReadOnly,
    /// 使用 agent 类型定义中声明的工具集；仅对 [`SubagentKind::Typed`] 有效。
    AgentType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentPermissions {
    pub tools: ToolGrant,
    /// 允许写入的工作区相对路径；为空表示不授予写入范围。
    pub workspace_scope: Vec<String>,
}

impl SubagentPermissions {
    pub fn read_only() -> Self {
        Self {
            tools: ToolGrant::ReadOnly,
            workspace_scope: Vec::new(),
        }
    }

    pub fn agent_type(workspace_scope: Vec<String>) -> Self {
        Self {
            tools: ToolGrant::AgentType,
            workspace_scope,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SubagentRequest {
    pub kind: SubagentKind,
    pub prompt: String,
    pub description: Option<String>,
    pub permissions: SubagentPermissions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentLineage {
    pub agent_id: String,
    pub parent_session_id: String,
    pub parent_agent_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl SubagentStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// 已启动子代理的所有权句柄。
#[async_trait]
pub trait SubagentHandle: Send + Sync {
    fn id(&self) -> &str;
    fn lineage(&self) -> &SubagentLineage;
    async fn status(&self) -> SubagentStatus;
    async fn cancel(&self);
    /// 等待子代理结束并返回结果；取消或失败时返回错误。
    async fn result(&self) -> Result<String>;
}

#[async_trait]
pub trait SubagentProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn spawn(&self, request: SubagentRequest) -> Result<Box<dyn SubagentHandle>>;
}
