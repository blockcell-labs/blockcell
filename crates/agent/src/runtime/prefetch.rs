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
    /// 少于两个候选时不预取（单个调用没有重叠收益）。每个候选先触发
    /// `tool/pre-execute`（与主管线同一事件）：listener 拒绝的调用不预取；
    /// listener 改写后的参数驱动其余资格检查与实际执行——保证"预取的内容
    /// 就是准入管线批准的参数"。主管线命中预取结果时跳过重复的 PreExecute
    /// 阶段，hook 每次调用只触发一次。被 hook/策略拒绝的调用在主管线中
    /// 仍会走完整准入并再次触发事件——重复触发不可避免，否则无法产生
    /// 正确的拒绝结果。
    pub(super) async fn prefetch_read_only_tools(
        &self,
        calls: &[ToolCallRequest],
        msg: &InboundMessage,
        tool_names: &[String],
        loop_guard: &ToolLoopGuard,
        disabled_tools: &HashSet<String>,
        disabled_skills: &HashSet<String>,
    ) -> HashMap<String, PrefetchedToolResult> {
        // 先用廉价同步条件筛选候选；少于两个可预取调用时不预取（单个调用没有
        // 重叠收益）。门控必须在 hook 触发之前：pre-execute 事件每次调用只应
        // 出现一次，若候选不足就直接返回，避免"预取探一次、管线拒一次"的双发。
        let mut pending_by_tool = HashMap::new();
        let mut pending_total = 0_u32;
        let mut candidates = Vec::new();
        for call in calls {
            // Respect the per-step tool scope and the iteration budget: the
            // main loop would skip/reject such calls, so prefetching them
            // would perform real I/O whose result is thrown away.
            let admitted = tool_names.iter().any(|name| name == &call.name)
                && self.tool_within_capability(&call.name, &call.arguments)
                && loop_guard.would_admit_batch(&call.name, &pending_by_tool, pending_total)
                && self.prefetch_candidate(call, disabled_tools, disabled_skills);
            if admitted {
                *pending_by_tool.entry(call.name.clone()).or_insert(0) += 1;
                pending_total = pending_total.saturating_add(1);
                candidates.push(call);
            }
        }
        if candidates.len() < 2 {
            return HashMap::new();
        }

        let session_key = persist_session_key_for(msg);
        let mut eligible = Vec::new();
        for call in candidates {
            let mut event = AgentEvent::ToolPreExecute {
                tool_name: call.name.clone(),
                call_id: call.id.clone(),
                arguments: call.arguments.clone(),
            };
            let decision = self.dispatch_event(msg, &session_key, &mut event).await;
            let args = match event {
                AgentEvent::ToolPreExecute { arguments, .. } => arguments,
                _ => call.arguments.clone(),
            };
            if matches!(decision, EventDecision::Reject { .. }) {
                // 主管线会再次触发事件并按原路径拒绝；不预取被拒绝的调用。
                continue;
            }
            if !self.prefetch_args_allowed(&call.name, &args, msg, &session_key) {
                continue;
            }
            eligible.push((call.id.clone(), call.name.clone(), args));
            if eligible.len() >= MAX_PREFETCH {
                break;
            }
        }
        if eligible.is_empty() {
            return HashMap::new();
        }

        let futures = eligible.into_iter().map(|(id, name, args)| {
            let ctx = self.build_tool_context(msg, None);
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

        futures::future::join_all(futures)
            .await
            .into_iter()
            .collect()
    }

    /// 廉价同步前置：白名单 + 未被禁用。
    fn prefetch_candidate(
        &self,
        call: &ToolCallRequest,
        disabled_tools: &HashSet<String>,
        disabled_skills: &HashSet<String>,
    ) -> bool {
        if !PREFETCHABLE_TOOLS.contains(&call.name.as_str()) {
            return false;
        }
        !disabled_tools.contains(&call.name) && !disabled_skills.contains(&call.name)
    }

    /// 针对（可能被 listener 改写后的）参数的资格检查：幂等安全 +
    /// Policy 明确 Allow + 所有路径参数都在工作区或已授权目录内。
    fn prefetch_args_allowed(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        msg: &InboundMessage,
        session_key: &str,
    ) -> bool {
        if self.tool_registry.idempotency(tool_name, args) != Idempotency::Safe {
            return false;
        }
        let eval = self.tool_policy.evaluate(&ToolCallContext {
            tool_name,
            tool_args: args,
            channel: &msg.channel,
        });
        if eval.decision != ToolPolicyDecision::Allow {
            return false;
        }
        for access in self.extract_path_accesses(tool_name, args) {
            let resolved = self.resolve_path(&access.path);
            if self.path_policy.evaluate(&resolved, access.op) == PolicyAction::Deny {
                return false;
            }
            if self.is_path_safe(&resolved)
                || self.is_path_authorized(&resolved, access.op, session_key)
            {
                continue;
            }
            // 需要用户确认的路径不在无交互的预取阶段处理。
            return false;
        }
        true
    }
}
