//! Runtime Kernel：Agent 运行时的统一扩展骨架。
//!
//! - [`event`]：事件分类（Durable / Live / Capability）与事件语义 ownership；
//! - [`event_bus`]：带优先级、作用域过滤、超时与故障隔离的事件总线；
//! - [`waterfall`]：需要变换数据流时使用的串行拦截链；
//! - [`scope`]：统一的作用域原语（可见性 + RAII 生命周期）。
//!
//! 原则：纯通知用 Event，需要改变数据流时才用 Waterfall。

pub mod event;
pub mod event_bus;
pub mod scope;
pub mod waterfall;

pub use event::{AgentEvent, AgentEventContext, EventClass, EventDecision};
pub use event_bus::{
    AgentEventListener, EventBus, FailureMode, ListenerSpec, RegisteredListener,
    DEFAULT_LISTENER_TIMEOUT,
};
pub use scope::{ScopeGuard, ScopeId, ScopeTarget, ScopeView, ScopedRegistry};
pub use waterfall::{Next, Waterfall, WaterfallChain};
