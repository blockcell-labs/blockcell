use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Notify;

/// Agent级别的取消信号
/// 参考: Claude Code AbortController 链
///
/// 设计说明：parent 使用 Arc<AbortToken> 是必要的，因为：
/// 1. AbortToken 包含 Option<AbortToken> 会导致无限大小
/// 2. Arc 提供间接引用打破递归
/// 3. cancelled 字段使用 Arc<AtomicBool> 共享取消状态
#[derive(Clone)]
pub struct AbortToken {
    /// 是否已取消（所有 clone 的 token 共享此状态）
    cancelled: Arc<AtomicBool>,
    /// Wakes async operations waiting for cancellation.
    notify: Arc<Notify>,
    /// 父级取消信号（形成链）
    /// Arc 是必要的，用于打破递归类型
    parent: Option<Arc<AbortToken>>,
}

/// 取消错误
#[derive(Debug, Clone, thiserror::Error)]
#[error("Operation cancelled: {message}")]
pub struct CancelledError {
    pub message: String,
}

impl AbortToken {
    /// 创建新的取消信号
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
            parent: None,
        }
    }

    /// 创建子取消信号（链式传递）
    /// 父取消时，子自动取消
    ///
    /// Arc 包装是必要的，用于打破递归类型（AbortToken 包含 parent）
    pub fn child(&self) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
            parent: Some(Arc::new(self.clone())),
        }
    }

    /// Return the root (top-most parent) of this cancellation chain.
    ///
    /// Background work that must outlive a single message turn should be a
    /// child of the root rather than of the per-turn token: the next message
    /// cancels the previous turn token, which would otherwise abort unrelated
    /// background work.
    pub fn root(&self) -> &AbortToken {
        let mut current = self;
        while let Some(parent) = &current.parent {
            current = parent.as_ref();
        }
        current
    }

    /// 检查是否已取消
    pub fn is_cancelled(&self) -> bool {
        let mut current = self;
        loop {
            if current.cancelled.load(Ordering::SeqCst) {
                return true;
            }
            match &current.parent {
                Some(parent) => current = parent.as_ref(),
                None => return false,
            }
        }
    }

    /// 检查取消状态，返回错误
    pub fn check(&self) -> Result<(), CancelledError> {
        if self.is_cancelled() {
            return Err(CancelledError {
                message: "Agent was cancelled".to_string(),
            });
        }
        Ok(())
    }

    /// Wait until this token or any parent token is cancelled.
    pub async fn cancelled(&self) {
        let local_wait = self.notify.notified();
        tokio::pin!(local_wait);

        // Register the waiter before checking the atomic flag so cancellation
        // cannot race between the check and waiter registration. `notified()`
        // is lazy — the future only enters the waiter list on first poll — so
        // `enable()` performs that poll up-front. Without it, a cancel() racing
        // between the flag check and the first poll would be lost
        // (`notify_waiters` stores no permit) and this waiter would hang.
        local_wait.as_mut().enable();
        if self.is_cancelled() {
            return;
        }

        if let Some(parent) = &self.parent {
            let mut parent_wait = Box::pin(parent.cancelled());
            tokio::select! {
                _ = &mut local_wait => {}
                _ = parent_wait.as_mut() => {}
            }
        } else {
            local_wait.await;
        }
    }

    /// 触发取消
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// 取消并附带原因
    pub fn cancel_with_reason(&self, reason: String) {
        self.cancel();
        tracing::info!(reason = %reason, "AbortToken cancelled");
    }
}

impl Default for AbortToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Cleanup 函数类型别名
type CleanupHandler = Box<dyn FnOnce() + Send>;

/// 注册表内部状态：`generation` 在每次 `run_all` drain 时递增，使旧代
/// `CleanupHandle` 的 index 不会误触新注册的 handler。
struct CleanupState {
    generation: u64,
    handlers: Vec<CleanupHandler>,
}

/// Cleanup 函数注册表
pub struct CleanupRegistry {
    state: Arc<StdMutex<CleanupState>>,
}

/// Cleanup 注销句柄
pub struct CleanupHandle {
    registry: Arc<StdMutex<CleanupState>>,
    index: usize,
    generation: u64,
}

impl CleanupRegistry {
    pub fn new() -> Self {
        Self {
            state: Arc::new(StdMutex::new(CleanupState {
                generation: 0,
                handlers: Vec::new(),
            })),
        }
    }

    /// 注册 cleanup 函数，返回注销句柄
    pub fn register<F: FnOnce() + Send + 'static>(&self, handler: F) -> CleanupHandle {
        let mut state = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let index = state.handlers.len();
        state.handlers.push(Box::new(handler));

        CleanupHandle {
            registry: self.state.clone(),
            index,
            generation: state.generation,
        }
    }

    /// 执行所有 cleanup 函数
    pub fn run_all(&self) {
        let handlers: Vec<_> = {
            let mut state = match self.state.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            state.generation += 1;
            state.handlers.drain(..).collect()
        };
        for handler in handlers {
            handler();
        }
    }
}

impl Default for CleanupRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CleanupHandle {
    fn drop(&mut self) {
        // Drop 时执行 cleanup handler
        // Recover from poisoned mutex to avoid panic-during-drop (process abort)
        let handler = {
            let mut state = match self.registry.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            // 仅当仍属注册时的同一代才允许按 index 取出——run_all 之后 index 已失效。
            if state.generation == self.generation {
                state
                    .handlers
                    .get_mut(self.index)
                    .map(|handler| std::mem::replace(handler, Box::new(|| {})))
            } else {
                None
            }
        };
        if let Some(handler) = handler {
            // Use catch_unwind to prevent panic-during-drop from aborting the process
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(handler));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_token_outlives_intermediate_cancellation() {
        let root = AbortToken::new();
        let turn = root.child();
        let background = turn.root().child();
        turn.cancel();
        assert!(turn.is_cancelled());
        assert!(
            !background.is_cancelled(),
            "background work attached to the root must survive a turn cancellation"
        );
        root.cancel();
        assert!(background.is_cancelled());
    }

    #[test]
    fn test_abort_token_new_not_cancelled() {
        let token = AbortToken::new();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn test_abort_token_cancel() {
        let token = AbortToken::new();
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn test_abort_token_chain_parent_cancels_child() {
        let parent = AbortToken::new();
        let child = parent.child();

        assert!(!child.is_cancelled());
        parent.cancel();
        assert!(child.is_cancelled()); // 子自动取消
    }

    #[test]
    fn test_abort_token_deep_chain_inherits_root_cancellation() {
        let root = AbortToken::new();
        let mut leaf = root.clone();
        for _ in 0..32 {
            leaf = leaf.child();
        }

        root.cancel();

        assert!(leaf.is_cancelled());
    }

    #[test]
    fn test_abort_token_check_returns_error() {
        let token = AbortToken::new();
        assert!(token.check().is_ok());

        token.cancel();
        assert!(token.check().is_err());
    }

    #[tokio::test]
    async fn cancelled_wait_completes_after_local_cancel() {
        let token = AbortToken::new();
        let waiter_token = token.clone();
        let waiter = tokio::spawn(async move { waiter_token.cancelled().await });

        token.cancel();

        tokio::time::timeout(std::time::Duration::from_millis(200), waiter)
            .await
            .expect("local cancellation should wake waiter")
            .expect("waiter task should complete");
    }

    #[tokio::test]
    async fn cancelled_wait_completes_after_parent_cancel() {
        let parent = AbortToken::new();
        let child = parent.child();
        let waiter = tokio::spawn(async move { child.cancelled().await });

        parent.cancel();

        tokio::time::timeout(std::time::Duration::from_millis(200), waiter)
            .await
            .expect("parent cancellation should wake child waiter")
            .expect("waiter task should complete");
    }

    #[test]
    fn test_cleanup_registry_runs_handlers() {
        let registry = CleanupRegistry::new();
        let counter = Arc::new(StdMutex::new(0));

        let counter_clone = counter.clone();
        registry.register(move || {
            *counter_clone.lock().unwrap() += 1;
        });

        registry.run_all();
        assert_eq!(*counter.lock().unwrap(), 1);
    }

    #[test]
    fn test_cleanup_handler_can_register_another_handler() {
        let registry = Arc::new(CleanupRegistry::new());
        let registry_for_handler = registry.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let handle = registry.register(move || {
            let nested = registry_for_handler.register(|| {});
            std::mem::forget(nested);
            done_tx.send(()).expect("signal completion");
        });
        std::mem::forget(handle);

        let registry_for_thread = registry.clone();
        std::thread::spawn(move || registry_for_thread.run_all());

        assert!(done_rx
            .recv_timeout(std::time::Duration::from_millis(250))
            .is_ok());
    }

    #[test]
    fn test_cleanup_handle_runs_on_drop() {
        let counter = Arc::new(StdMutex::new(0));
        let registry = CleanupRegistry::new();

        {
            let counter_clone = counter.clone();
            let _handle = registry.register(move || {
                *counter_clone.lock().unwrap() += 1;
            });
            // handle dropped here
        }

        assert_eq!(*counter.lock().unwrap(), 1);
    }
}

#[cfg(test)]
mod cleanup_generation_tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn stale_generation_handle_cannot_run_new_handler() {
        let registry = CleanupRegistry::new();
        let stale = registry.register(|| {});
        registry.run_all();

        let counter = Arc::new(StdMutex::new(0));
        let counter_clone = counter.clone();
        let fresh = registry.register(move || {
            *counter_clone.lock().unwrap() += 1;
        });
        // 旧代 handle 的 index 已失效——drop 不得误消费新注册的 handler。
        drop(stale);
        assert_eq!(*counter.lock().unwrap(), 0);
        drop(fresh);
        assert_eq!(*counter.lock().unwrap(), 1);
    }
}
