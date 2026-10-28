/// Per-turn routing observations. Execution success is not task correctness.
#[derive(Debug, Default)]
pub(crate) struct RoutingMetrics {
    pub resolved_scope_count: usize,
    pub catalog_count: usize,
    pub exposed_count: usize,
    pub schema_bytes: usize,
    pub selected_calls: usize,
    pub scope_denied_calls: usize,
    pub rejected_calls: usize,
    pub succeeded_calls: usize,
    pub failed_calls: usize,
}

impl RoutingMetrics {
    pub fn record_inventory(
        &mut self,
        capability: usize,
        catalog: usize,
        exposed: usize,
        bytes: usize,
    ) {
        self.resolved_scope_count = capability;
        self.catalog_count = catalog;
        self.exposed_count = exposed;
        self.schema_bytes = bytes;
    }

    pub fn record_selection(&mut self, in_scope: bool) {
        self.selected_calls += 1;
        self.scope_denied_calls += usize::from(!in_scope);
    }

    pub fn record_outcome(&mut self, rejected: bool, succeeded: bool, failed: bool) {
        self.rejected_calls += usize::from(rejected);
        self.succeeded_calls += usize::from(succeeded);
        self.failed_calls += usize::from(failed && !rejected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_stages_do_not_confuse_scope_rejection_with_execution_failure() {
        let mut metrics = RoutingMetrics::default();
        metrics.record_inventory(8, 6, 4, 1200);
        metrics.record_selection(false);
        metrics.record_outcome(true, false, false);
        metrics.record_selection(true);
        metrics.record_outcome(false, true, false);
        metrics.record_selection(true);
        metrics.record_outcome(false, false, true);
        assert_eq!(metrics.selected_calls, 3);
        assert_eq!(metrics.scope_denied_calls, 1);
        assert_eq!(metrics.succeeded_calls, 1);
        assert_eq!(metrics.failed_calls, 1);
        assert_eq!(metrics.rejected_calls, 1);
        assert_eq!(metrics.schema_bytes, 1200);
        assert_eq!(metrics.resolved_scope_count, 8);
        assert_eq!(metrics.catalog_count, 6);
        assert_eq!(metrics.exposed_count, 4);
    }
}
