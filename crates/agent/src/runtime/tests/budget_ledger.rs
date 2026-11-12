use super::*;

fn ledger_runtime() -> AgentRuntime {
    let mut runtime = test_runtime();
    runtime.config.budget.max_tokens_per_session = 100;
    runtime.config.budget.max_cost_cents_per_session = 1;
    runtime
}

#[test]
fn budget_ledger_keeps_historical_token_and_cost_totals_after_129_sessions() {
    let runtime = ledger_runtime();
    let mut expected = Vec::new();
    for index in 0..129 {
        let key = format!("cli:history-{index}");
        let (input, output, cost) = match index % 3 {
            0 => (100, 20, 2_000), // exhausted by tokens
            1 => (5, 5, 12_000),   // exhausted by cost
            _ => (10, 5, 1_000),   // still usable
        };
        let tracker = runtime.budget_tracker_for_session(&key);
        let snapshot = tracker.record_usage(input, output, cost);
        expected.push((key, snapshot, tracker.is_exhausted()));
        // No external Arc remains to rescue historical state from eviction.
    }

    for (key, snapshot, exhausted) in expected {
        let tracker = runtime.budget_tracker_for_session(&key);
        assert_eq!(
            tracker.snapshot(),
            snapshot,
            "historical spend lost for {key}"
        );
        assert_eq!(
            tracker.check_budget().is_err(),
            exhausted,
            "exhaustion changed for {key}"
        );
    }
}

#[test]
fn budget_ledger_live_parent_child_and_message_fork_keep_one_authority() {
    let runtime = ledger_runtime();
    let (steering, sender) = SteeringChannel::new(4);
    let fork = runtime.fork_for_message_task(steering, sender, runtime.abort_token.child());
    let mut live = Vec::new();
    for index in 0..96 {
        let key = format!("cli:live-parent-{index}");
        let parent = runtime.budget_tracker_for_session(&key);
        parent.record_usage(10, 2, 1_000);
        let child = Arc::new(BudgetTracker::with_parent(
            &runtime.config.budget,
            Some(parent.clone()),
        ));
        live.push((key, parent, child));
    }
    for index in 0..129 {
        runtime.budget_tracker_for_session(&format!("cli:pressure-{index}"));
    }
    for (key, parent, _) in &live {
        let from_fork = fork.budget_tracker_for_session(key);
        assert!(
            Arc::ptr_eq(parent, &from_fork),
            "live parent split into a second ledger for {key}"
        );
    }

    // A child retained before pressure completes while the message fork also
    // records usage. Both must charge the original authoritative parent once.
    std::thread::scope(|scope| {
        let delayed_children = scope.spawn(|| {
            for (_, _, child) in &live {
                child.record_usage(5, 3, 200);
            }
        });
        for (key, _, _) in &live {
            fork.budget_tracker_for_session(key).record_usage(2, 1, 30);
        }
        delayed_children.join().unwrap();
    });
    for (key, parent, child) in &live {
        let current = runtime.budget_tracker_for_session(key).snapshot();
        assert_eq!(
            current,
            parent.snapshot(),
            "parent spend diverged for {key}"
        );
        assert_eq!(current.input_tokens_used, 17);
        assert_eq!(current.output_tokens_used, 6);
        assert_eq!(current.cost_used_micro_usd, 1_230);
        assert_eq!(child.snapshot().total_tokens_used, 8);
    }

    let weak_parent = Arc::downgrade(&live[0].1);
    drop(live);
    drop(runtime);
    assert!(
        weak_parent.upgrade().is_some(),
        "message fork owns the shared accounting period"
    );
    drop(fork);
    assert!(
        weak_parent.upgrade().is_none(),
        "ledger must release when its final registry owner drops"
    );
}

#[test]
fn budget_ledger_context_inspection_and_compaction_do_not_reset_spend() {
    let runtime = ledger_runtime();
    let handle = super::super::lightweight_handle::LightweightRuntimeHandle::from_runtime(&runtime);
    let mut keys = Vec::new();
    for index in 0..96 {
        let key = format!("cli:context-budget-{index}");
        runtime
            .budget_tracker_for_session(&key)
            .record_usage(90, 20, 15_000);
        runtime.record_context_window_estimate(&key, 900, 1_000, 0.8);
        keys.push(key);
    }
    for index in 0..129 {
        runtime.budget_tracker_for_session(&format!("cli:context-pressure-{index}"));
    }

    for key in keys {
        let before = blockcell_tools::RuntimeHandle::context_window_snapshot(&handle, &key);
        assert_eq!(
            before.tokens_used, 900,
            "runtime handle read a replacement tracker for {key}"
        );
        runtime.record_context_window_estimate(&key, 100, 1_000, 0.8);
        let after = blockcell_tools::RuntimeHandle::context_window_snapshot(&handle, &key);
        assert_eq!(after.tokens_used, 100);
        let tracker = runtime.budget_tracker_for_session(&key);
        assert_eq!(tracker.snapshot().total_tokens_used, 110);
        assert_eq!(tracker.snapshot().cost_used_micro_usd, 15_000);
        assert!(
            tracker.check_budget().is_err(),
            "compaction must not restore spent allowance for {key}"
        );
    }
}
