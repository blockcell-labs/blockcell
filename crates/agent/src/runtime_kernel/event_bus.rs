use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use blockcell_core::Result;
use futures::FutureExt;
use tracing::warn;

use super::event::{AgentEvent, AgentEventContext, EventDecision};
use super::scope::{ScopeGuard, ScopeId, ScopeTarget, ScopeView, ScopedRegistry};

/// Listener 失败（返回错误 / 超时 / panic）时的处理策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureMode {
    /// 记录 warning 后继续（telemetry 类 listener）。
    FailOpen,
    /// 在可拒绝事件上视为拒绝（policy / approval 类 listener）。
    FailClosed,
}

/// Listener 元数据。`priority` 越小越先执行，同优先级按注册顺序执行。
#[derive(Debug, Clone)]
pub struct ListenerSpec {
    pub name: &'static str,
    pub priority: i32,
    pub failure_mode: FailureMode,
    pub scope: ScopeTarget,
    pub timeout: Option<Duration>,
}

impl ListenerSpec {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            priority: 0,
            failure_mode: FailureMode::FailOpen,
            scope: ScopeTarget::Global,
            timeout: None,
        }
    }

    pub fn priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    pub fn failure_mode(mut self, failure_mode: FailureMode) -> Self {
        self.failure_mode = failure_mode;
        self
    }

    pub fn scope(mut self, scope: ScopeTarget) -> Self {
        self.scope = scope;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

#[async_trait]
pub trait AgentEventListener: Send + Sync {
    fn spec(&self) -> ListenerSpec;

    async fn handle(
        &self,
        ctx: &AgentEventContext,
        event: &mut AgentEvent,
    ) -> Result<EventDecision>;
}

/// EventBus 内部存放的 listener 条目。
pub struct RegisteredListener {
    spec: ListenerSpec,
    listener: Arc<dyn AgentEventListener>,
}

/// Agent 事件总线。listener 按 [`ListenerSpec::scope`] 存入 [`ScopedRegistry`]，
/// 克隆共享同一份 listener 列表。
#[derive(Clone, Default)]
pub struct EventBus {
    listeners: ScopedRegistry<Arc<RegisteredListener>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册长期存在的 listener。
    pub fn register(&self, listener: Arc<dyn AgentEventListener>) -> ScopeId {
        let (scope, registered) = Self::entry(listener);
        self.listeners.insert(scope, registered)
    }

    /// 注册随 guard 销毁而注销的 listener（例如只在某次候选评估期间生效）。
    pub fn register_scoped(
        &self,
        listener: Arc<dyn AgentEventListener>,
    ) -> ScopeGuard<Arc<RegisteredListener>> {
        let (scope, registered) = Self::entry(listener);
        self.listeners.register(scope, registered)
    }

    pub fn unregister(&self, id: ScopeId) -> bool {
        self.listeners.remove(id).is_some()
    }

    fn entry(listener: Arc<dyn AgentEventListener>) -> (ScopeTarget, Arc<RegisteredListener>) {
        let spec = listener.spec();
        (
            spec.scope.clone(),
            Arc::new(RegisteredListener { spec, listener }),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.listeners.is_empty()
    }

    pub fn len(&self) -> usize {
        self.listeners.len()
    }

    /// 按优先级串行派发事件。listener 可修改 `event`；
    /// 仅在 [`AgentEvent::can_reject`] 为真时，拒绝或 fail-closed 失败才会短路返回。
    pub async fn dispatch(&self, ctx: &AgentEventContext, event: &mut AgentEvent) -> EventDecision {
        let mut snapshot = self.listeners.visible(ScopeView {
            agent_id: &ctx.agent_id,
            session_id: &ctx.session_id,
            candidate_id: ctx.candidate_id.as_deref(),
        });
        snapshot.sort_by_key(|item| item.spec.priority);

        for item in snapshot {
            let spec = &item.spec;
            let outcome = run_isolated(item.listener.as_ref(), spec.timeout, ctx, event).await;
            match outcome {
                Ok(EventDecision::Continue) => {}
                Ok(EventDecision::Reject { reason }) => {
                    if event.can_reject() {
                        return EventDecision::Reject { reason };
                    }
                    warn!(
                        listener = spec.name,
                        event = event.name(),
                        "Listener rejected a non-rejectable event; ignored"
                    );
                }
                Err(error) => {
                    warn!(listener = spec.name, event = event.name(), error = %error, "Event listener failed");
                    if spec.failure_mode == FailureMode::FailClosed && event.can_reject() {
                        return EventDecision::Reject {
                            reason: format!("listener '{}' failed: {}", spec.name, error),
                        };
                    }
                }
            }
        }
        EventDecision::Continue
    }
}

async fn run_isolated(
    listener: &dyn AgentEventListener,
    timeout: Option<Duration>,
    ctx: &AgentEventContext,
    event: &mut AgentEvent,
) -> std::result::Result<EventDecision, String> {
    let fut = AssertUnwindSafe(listener.handle(ctx, event)).catch_unwind();
    let caught = match timeout {
        Some(limit) => match tokio::time::timeout(limit, fut).await {
            Ok(caught) => caught,
            Err(_) => return Err(format!("timed out after {}ms", limit.as_millis())),
        },
        None => fut.await,
    };
    match caught {
        Ok(Ok(decision)) => Ok(decision),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("listener panicked".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    struct Recorder {
        spec: ListenerSpec,
        log: Arc<Mutex<Vec<&'static str>>>,
        behavior: Behavior,
    }

    #[derive(Clone, Copy)]
    enum Behavior {
        Continue,
        Reject,
        Error,
        Panic,
        Sleep,
        RewriteArgs,
    }

    #[async_trait]
    impl AgentEventListener for Recorder {
        fn spec(&self) -> ListenerSpec {
            self.spec.clone()
        }

        async fn handle(
            &self,
            _ctx: &AgentEventContext,
            event: &mut AgentEvent,
        ) -> Result<EventDecision> {
            self.log.lock().unwrap().push(self.spec.name);
            match self.behavior {
                Behavior::Continue => Ok(EventDecision::Continue),
                Behavior::Reject => Ok(EventDecision::Reject {
                    reason: "no".into(),
                }),
                Behavior::Error => Err(blockcell_core::Error::Other("boom".into())),
                Behavior::Panic => panic!("listener panic"),
                Behavior::Sleep => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    Ok(EventDecision::Continue)
                }
                Behavior::RewriteArgs => {
                    if let AgentEvent::ToolPreExecute { arguments, .. } = event {
                        *arguments = json!({"rewritten": true});
                    }
                    Ok(EventDecision::Continue)
                }
            }
        }
    }

    fn listener(
        log: &Arc<Mutex<Vec<&'static str>>>,
        spec: ListenerSpec,
        behavior: Behavior,
    ) -> Arc<dyn AgentEventListener> {
        Arc::new(Recorder {
            spec,
            log: log.clone(),
            behavior,
        })
    }

    fn pre_execute() -> AgentEvent {
        AgentEvent::ToolPreExecute {
            tool_name: "exec".into(),
            call_id: "c1".into(),
            arguments: json!({}),
        }
    }

    fn ctx() -> AgentEventContext {
        AgentEventContext {
            agent_id: "default".into(),
            session_id: "s1".into(),
            ..AgentEventContext::default()
        }
    }

    #[tokio::test]
    async fn dispatches_in_priority_then_registration_order() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let bus = EventBus::new();
        bus.register(listener(
            &log,
            ListenerSpec::new("b").priority(10),
            Behavior::Continue,
        ));
        bus.register(listener(
            &log,
            ListenerSpec::new("a").priority(-5),
            Behavior::Continue,
        ));
        bus.register(listener(
            &log,
            ListenerSpec::new("c").priority(10),
            Behavior::Continue,
        ));

        let decision = bus.dispatch(&ctx(), &mut pre_execute()).await;

        assert_eq!(decision, EventDecision::Continue);
        assert_eq!(*log.lock().unwrap(), vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn reject_short_circuits_only_rejectable_events() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let bus = EventBus::new();
        bus.register(listener(&log, ListenerSpec::new("deny"), Behavior::Reject));
        bus.register(listener(
            &log,
            ListenerSpec::new("after"),
            Behavior::Continue,
        ));

        let decision = bus.dispatch(&ctx(), &mut pre_execute()).await;
        assert_eq!(
            decision,
            EventDecision::Reject {
                reason: "no".into()
            }
        );
        assert_eq!(*log.lock().unwrap(), vec!["deny"]);

        let mut turn_end = AgentEvent::TurnEnd {
            final_response: String::new(),
        };
        assert_eq!(
            bus.dispatch(&ctx(), &mut turn_end).await,
            EventDecision::Continue
        );
        assert_eq!(*log.lock().unwrap(), vec!["deny", "deny", "after"]);
    }

    #[tokio::test]
    async fn failures_are_isolated_according_to_failure_mode() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let bus = EventBus::new();
        bus.register(listener(&log, ListenerSpec::new("err"), Behavior::Error));
        bus.register(listener(&log, ListenerSpec::new("panic"), Behavior::Panic));
        bus.register(listener(
            &log,
            ListenerSpec::new("slow").timeout(Duration::from_millis(20)),
            Behavior::Sleep,
        ));
        assert_eq!(
            bus.dispatch(&ctx(), &mut pre_execute()).await,
            EventDecision::Continue
        );

        bus.register(listener(
            &log,
            ListenerSpec::new("policy").failure_mode(FailureMode::FailClosed),
            Behavior::Error,
        ));
        match bus.dispatch(&ctx(), &mut pre_execute()).await {
            EventDecision::Reject { reason } => assert!(reason.contains("policy")),
            other => panic!("expected reject, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn scoped_registration_is_removed_with_guard_or_id() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let bus = EventBus::new();
        let id = bus.register(listener(
            &log,
            ListenerSpec::new("kept"),
            Behavior::Continue,
        ));
        {
            let _guard = bus.register_scoped(listener(
                &log,
                ListenerSpec::new("temp"),
                Behavior::Continue,
            ));
            bus.dispatch(&ctx(), &mut pre_execute()).await;
        }
        bus.dispatch(&ctx(), &mut pre_execute()).await;
        assert_eq!(*log.lock().unwrap(), vec!["kept", "temp", "kept"]);

        assert!(bus.unregister(id));
        assert!(bus.is_empty());
    }

    #[tokio::test]
    async fn scope_filters_listeners_and_mutations_flow_through() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let bus = EventBus::new();
        bus.register(listener(
            &log,
            ListenerSpec::new("other-agent").scope(ScopeTarget::Agent("ops".into())),
            Behavior::Reject,
        ));
        bus.register(listener(
            &log,
            ListenerSpec::new("rewrite").scope(ScopeTarget::Session("s1".into())),
            Behavior::RewriteArgs,
        ));

        let mut event = pre_execute();
        assert_eq!(
            bus.dispatch(&ctx(), &mut event).await,
            EventDecision::Continue
        );
        assert_eq!(*log.lock().unwrap(), vec!["rewrite"]);
        match event {
            AgentEvent::ToolPreExecute { arguments, .. } => {
                assert_eq!(arguments, json!({"rewritten": true}))
            }
            _ => unreachable!(),
        }
    }
}
