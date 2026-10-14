//! 只读工具并行预取（P2-2）。
//!
//! 模型常在一批里提出多个彼此独立的只读调用（读取文件、搜索代码、抓取网页）。当前主循环
//! 串行执行它们，墙钟时间等于各调用之和。这里在进入主循环前，对**满足安全条件**的只读
//! 调用并发预执行；主循环仍按原顺序走完整的准入 / 事件 / 审计流程，只是把工具体的实际
//! 执行替换为预取结果，从而在保持顺序与可观测性的前提下重叠 I/O 延迟。
//!
//! 安全边界：只预取显式白名单内的只读工具，且要求幂等安全、未被开关禁用、Tool Policy
//! 明确 Allow、所有路径参数都在工作区或已授权目录内（无需交互确认）。任何一项不满足就
//! 退回串行管线；预取结果在准入阶段被拒绝时会被丢弃。

use super::*;
use blockcell_core::path_policy::PolicyAction;

/// 允许预取的工具白名单：只读、无副作用（本地读取 / 搜索 / 只读网络）。
const PREFETCHABLE_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "glob",
    "grep",
    "rg",
    "code_search",
    "session_search",
    "web_fetch",
    "web_search",
];

/// 单批最多并发预取的调用数，避免一次性打出过多并发请求。
const MAX_PREFETCH: usize = 8;

/// 预取结果：主循环命中后直接使用，跳过工具体的实际执行。
pub(crate) struct PrefetchedToolResult {
    pub(crate) result: blockcell_core::Result<serde_json::Value>,
    pub(crate) duration_ms: u64,
    /// 预取时使用的参数；若准入阶段 listener 改写了参数则不再复用。
    pub(crate) args: serde_json::Value,
}

impl AgentRuntime {
    /// 对一批工具调用做只读预取，返回按 `call_id` 索引的结果。
    ///
    /// 少于两个可预取调用时不预取（单个调用没有重叠收益，只增加复杂度）。
    pub(crate) async fn prefetch_read_only_tools(
        &self,
        calls: &[ToolCallRequest],
        msg: &InboundMessage,
    ) -> HashMap<String, PrefetchedToolResult> {
        let eligible = calls
            .iter()
            .filter(|call| self.prefetch_eligible(call, msg))
            .take(MAX_PREFETCH)
            .collect::<Vec<_>>();
        if eligible.len() < 2 {
            return HashMap::new();
        }

        let futures = eligible.into_iter().map(|call| {
            let ctx = self.build_tool_context(msg, None);
            let name = call.name.clone();
            let args = call.arguments.clone();
            let id = call.id.clone();
            async move {
                let start = std::time::Instant::now();
                let result = self
                    .execute_runtime_tool_call(&name, ctx, args.clone())
                    .await;
                let duration_ms = start.elapsed().as_millis() as u64;
                (
                    id,
                    PrefetchedToolResult {
                        result,
                        duration_ms,
                        args,
                    },
                )
            }
        });

        futures::future::join_all(futures).await.into_iter().collect()
    }

    /// 是否可安全预取：白名单 + 幂等安全 + 未被禁用 + Policy 明确 Allow +
    /// 所有路径参数都在工作区或已授权目录内（不需要交互确认）。
    fn prefetch_eligible(&self, call: &ToolCallRequest, msg: &InboundMessage) -> bool {
        if !PREFETCHABLE_TOOLS.contains(&call.name.as_str()) {
            return false;
        }
        if load_disabled_toggles(&self.paths, "tools").contains(&call.name)
            || load_disabled_toggles(&self.paths, "skills").contains(&call.name)
        {
            return false;
        }
        if self.tool_registry.idempotency(&call.name, &call.arguments) != Idempotency::Safe {
            return false;
        }
        let eval = self.tool_policy.evaluate(&ToolCallContext {
            tool_name: &call.name,
            tool_args: &call.arguments,
            channel: &msg.channel,
        });
        if eval.decision != ToolPolicyDecision::Allow {
            return false;
        }
        for access in self.extract_path_accesses(&call.name, &call.arguments) {
            let resolved = self.resolve_path(&access.path);
            if self.path_policy.evaluate(&resolved, access.op) == PolicyAction::Deny {
                return false;
            }
            if self.is_path_safe(&resolved) || self.is_path_authorized(&resolved, access.op) {
                continue;
            }
            // 需要用户确认的路径不在无交互的预取阶段处理。
            return false;
        }
        true
    }
}
