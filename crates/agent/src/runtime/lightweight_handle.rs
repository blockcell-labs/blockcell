use super::*;

/// Lightweight handle for tools that avoids circular Arc<Self> references.
///
/// Captures only the data needed by the subagent provider and context-window lookups,
/// so `ToolContext` can hold this without owning the full `AgentRuntime`.
pub struct LightweightRuntimeHandle {
    subagents: Arc<InProcessSubagentProvider>,
    config: Config,
    budget_trackers: Arc<Mutex<HashMap<String, BudgetTrackerHandle>>>,
}

impl LightweightRuntimeHandle {
    pub fn from_runtime(runtime: &AgentRuntime) -> Self {
        Self {
            subagents: Arc::new(InProcessSubagentProvider::from_runtime(runtime)),
            config: runtime.config.clone(),
            budget_trackers: runtime.budget_trackers.clone(),
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
            .or_insert_with(|| Arc::new(BudgetTracker::new(&self.config.budget)))
            .context_window_snapshot()
    }
}
