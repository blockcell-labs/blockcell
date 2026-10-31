use super::*;

/// Lightweight handle for tools that avoids circular Arc<Self> references.
///
/// Captures only the data needed by the subagent provider and context-window lookups,
/// so `ToolContext` can hold this without owning the full `AgentRuntime`.
pub struct LightweightRuntimeHandle {
    subagents: Arc<InProcessSubagentProvider>,
    config: Config,
    budget_trackers: Arc<Mutex<HashMap<String, BudgetTrackerHandle>>>,
    /// 父会话预算，用于新建的 session tracker 归并（与 AgentRuntime 保持一致）。
    parent_budget: Option<BudgetTrackerHandle>,
}

impl LightweightRuntimeHandle {
    pub(crate) fn for_invocation(runtime: &AgentRuntime, msg: &InboundMessage) -> Self {
        let mut handle = Self::from_runtime(runtime);
        handle.subagents =
            Arc::new(InProcessSubagentProvider::from_runtime(runtime).for_invocation(runtime, msg));
        handle
    }

    pub fn from_runtime(runtime: &AgentRuntime) -> Self {
        Self {
            subagents: Arc::new(InProcessSubagentProvider::from_runtime(runtime)),
            config: runtime.config.clone(),
            budget_trackers: runtime.budget_trackers.clone(),
            parent_budget: runtime.parent_budget.clone(),
        }
    }
}

impl blockcell_tools::RuntimeHandle for LightweightRuntimeHandle {
    fn subagents(&self) -> Arc<dyn blockcell_tools::subagent::SubagentProvider> {
        self.subagents.clone()
    }

    fn context_window_snapshot(&self, session_key: &str) -> blockcell_core::ContextWindowSnapshot {
        let mut trackers = self
            .budget_trackers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        trackers
            .entry(session_key.to_string())
            .or_insert_with(|| {
                Arc::new(BudgetTracker::with_parent(
                    &self.config.budget,
                    self.parent_budget.clone(),
                ))
            })
            .context_window_snapshot()
    }
}
