use std::sync::Arc;

use async_trait::async_trait;
use blockcell_core::Result;
use futures::future::BoxFuture;
use futures::FutureExt;

/// 调用链中剩余阶段的续延。stage 可以变换输入后调用 `next`，
/// 也可以不调用 `next` 直接返回（短路）。
pub type Next<'a, T> = Box<dyn FnOnce(T) -> BoxFuture<'a, Result<T>> + Send + 'a>;

/// 串行拦截阶段：只有需要改变数据流时才使用，纯通知请使用 EventBus。
#[async_trait]
pub trait Waterfall<T: Send + 'static>: Send + Sync {
    async fn handle(&self, value: T, next: Next<'_, T>) -> Result<T>;
}

pub struct WaterfallChain<T> {
    stages: Vec<Arc<dyn Waterfall<T>>>,
}

impl<T> Default for WaterfallChain<T> {
    fn default() -> Self {
        Self { stages: Vec::new() }
    }
}

impl<T> Clone for WaterfallChain<T> {
    fn clone(&self) -> Self {
        Self {
            stages: self.stages.clone(),
        }
    }
}

impl<T: Send + 'static> WaterfallChain<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, stage: Arc<dyn Waterfall<T>>) {
        self.stages.push(stage);
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    pub async fn run(&self, value: T) -> Result<T> {
        run_from(&self.stages, value).await
    }
}

fn run_from<'a, T: Send + 'static>(
    stages: &'a [Arc<dyn Waterfall<T>>],
    value: T,
) -> BoxFuture<'a, Result<T>> {
    match stages.split_first() {
        None => futures::future::ready(Ok(value)).boxed(),
        Some((head, rest)) => {
            let next: Next<'a, T> = Box::new(move |value| run_from(rest, value));
            head.handle(value, next)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Append(&'static str);

    #[async_trait]
    impl Waterfall<String> for Append {
        async fn handle(&self, value: String, next: Next<'_, String>) -> Result<String> {
            let out = next(format!("{value}>{}", self.0)).await?;
            Ok(format!("{out}<{}", self.0))
        }
    }

    struct Stop;

    #[async_trait]
    impl Waterfall<String> for Stop {
        async fn handle(&self, value: String, _next: Next<'_, String>) -> Result<String> {
            Ok(format!("{value}|stop"))
        }
    }

    #[tokio::test]
    async fn stages_wrap_each_other_in_order() {
        let mut chain = WaterfallChain::new();
        chain.push(Arc::new(Append("a")));
        chain.push(Arc::new(Append("b")));

        assert_eq!(chain.run("in".into()).await.unwrap(), "in>a>b<b<a");
    }

    #[tokio::test]
    async fn stage_can_short_circuit() {
        let mut chain = WaterfallChain::new();
        chain.push(Arc::new(Append("a")));
        chain.push(Arc::new(Stop));
        chain.push(Arc::new(Append("never")));

        assert_eq!(chain.run("in".into()).await.unwrap(), "in>a|stop<a");
    }

    #[tokio::test]
    async fn empty_chain_is_identity() {
        let chain = WaterfallChain::<String>::new();
        assert_eq!(chain.run("x".into()).await.unwrap(), "x");
    }
}
