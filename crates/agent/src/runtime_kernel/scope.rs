//! 统一的作用域原语，避免每个 registry 自己发明 scope。
//!
//! Scope 同时控制**可见性**（谁能看到）与**生命周期**（RAII 注销）。
//! 不存在隐式父子继承：子 agent 拥有自己的 agent_id，看不到父 agent 的 Agent 作用域条目；
//! 如需共享，应显式在目标作用域重新注册（capability snapshot）。
//!
//! `Candidate` 作用域是 Self-Evolution 的隔离边界：候选 skill / prompt / policy 只对
//! 显式进入该候选评估的视图可见，生产视图永远看不到。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, Weak};

/// 单次注册的唯一标识，用于显式注销。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScopeId(u64);

/// 条目归属的作用域。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeTarget {
    Global,
    Agent(String),
    Session(String),
    /// 进化候选，仅对 `candidate_id` 相同的视图可见。
    Candidate(String),
}

/// 查询方所处的位置，用于判定可见性。
#[derive(Debug, Clone, Copy)]
pub struct ScopeView<'a> {
    pub agent_id: &'a str,
    pub session_id: &'a str,
    /// 正在评估的进化候选；生产视图为 `None`。
    pub candidate_id: Option<&'a str>,
}

impl ScopeTarget {
    pub fn is_visible_to(&self, view: ScopeView<'_>) -> bool {
        match self {
            Self::Global => true,
            Self::Agent(agent_id) => agent_id == view.agent_id,
            Self::Session(session_id) => session_id == view.session_id,
            Self::Candidate(candidate_id) => view.candidate_id == Some(candidate_id.as_str()),
        }
    }
}

struct Entry<T> {
    id: ScopeId,
    target: ScopeTarget,
    value: T,
}

type Entries<T> = RwLock<Vec<Entry<T>>>;

/// 按作用域存放条目的 registry。克隆共享同一份数据，按注册顺序返回可见条目。
pub struct ScopedRegistry<T> {
    entries: Arc<Entries<T>>,
    next_id: Arc<AtomicU64>,
}

impl<T> Clone for ScopedRegistry<T> {
    fn clone(&self) -> Self {
        Self {
            entries: Arc::clone(&self.entries),
            next_id: Arc::clone(&self.next_id),
        }
    }
}

impl<T> Default for ScopedRegistry<T> {
    fn default() -> Self {
        Self {
            entries: Arc::new(RwLock::new(Vec::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }
}

impl<T: Clone> ScopedRegistry<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个长期存在的条目，返回可用于 [`Self::remove`] 的标识。
    pub fn insert(&self, target: ScopeTarget, value: T) -> ScopeId {
        let id = ScopeId(self.next_id.fetch_add(1, Ordering::Relaxed));
        self.entries
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .push(Entry { id, target, value });
        id
    }

    /// 注册一个随 guard 销毁而自动注销的条目。
    pub fn register(&self, target: ScopeTarget, value: T) -> ScopeGuard<T> {
        let id = self.insert(target, value);
        ScopeGuard {
            id,
            entries: Arc::downgrade(&self.entries),
        }
    }

    pub fn remove(&self, id: ScopeId) -> Option<T> {
        remove_entry(&self.entries, id)
    }

    pub fn visible(&self, view: ScopeView<'_>) -> Vec<T> {
        self.entries
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|entry| entry.target.is_visible_to(view))
            .map(|entry| entry.value.clone())
            .collect()
    }

    /// 与 [`Self::visible`] 相同，但同时返回条目所属作用域，供调用方做覆盖决策。
    pub fn visible_with_target(&self, view: ScopeView<'_>) -> Vec<(ScopeTarget, T)> {
        self.entries
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|entry| entry.target.is_visible_to(view))
            .map(|entry| (entry.target.clone(), entry.value.clone()))
            .collect()
    }

    /// 注册集合的指纹：任何注册或注销都会改变它，可用于缓存失效判断。
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for entry in self
            .entries
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
        {
            std::hash::Hash::hash(&entry.id, &mut hasher);
        }
        std::hash::Hasher::finish(&hasher)
    }

    pub fn len(&self) -> usize {
        self.entries.read().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn remove_entry<T>(entries: &Entries<T>, id: ScopeId) -> Option<T> {
    let mut entries = entries.write().unwrap_or_else(|p| p.into_inner());
    let index = entries.iter().position(|entry| entry.id == id)?;
    Some(entries.remove(index).value)
}

/// RAII 注册句柄：drop 时从 registry 注销对应条目。
#[must_use = "dropping the guard immediately unregisters the entry"]
pub struct ScopeGuard<T> {
    id: ScopeId,
    entries: Weak<Entries<T>>,
}

impl<T> ScopeGuard<T> {
    pub fn id(&self) -> ScopeId {
        self.id
    }
}

impl<T> Drop for ScopeGuard<T> {
    fn drop(&mut self) {
        if let Some(entries) = self.entries.upgrade() {
            remove_entry(&entries, self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(agent_id: &'a str, session_id: &'a str) -> ScopeView<'a> {
        ScopeView {
            agent_id,
            session_id,
            candidate_id: None,
        }
    }

    #[test]
    fn visibility_follows_scope_target_without_inheritance() {
        let registry = ScopedRegistry::new();
        registry.insert(ScopeTarget::Global, "global");
        registry.insert(ScopeTarget::Agent("parent".into()), "parent-only");
        registry.insert(ScopeTarget::Session("s1".into()), "session-only");

        assert_eq!(
            registry.visible(view("parent", "s1")),
            vec!["global", "parent-only", "session-only"]
        );
        // 子 agent 不会隐式继承父 agent 作用域
        assert_eq!(registry.visible(view("parent/child", "s2")), vec!["global"]);
    }

    #[test]
    fn candidate_entries_are_only_visible_inside_their_candidate_view() {
        let registry = ScopedRegistry::new();
        registry.insert(ScopeTarget::Global, "prod");
        registry.insert(ScopeTarget::Candidate("evo-1".into()), "candidate");

        assert_eq!(registry.visible(view("a", "s")), vec!["prod"]);
        let candidate_view = ScopeView {
            candidate_id: Some("evo-1"),
            ..view("a", "s")
        };
        assert_eq!(registry.visible(candidate_view), vec!["prod", "candidate"]);
        let other_candidate = ScopeView {
            candidate_id: Some("evo-2"),
            ..view("a", "s")
        };
        assert_eq!(registry.visible(other_candidate), vec!["prod"]);
    }

    #[test]
    fn guard_unregisters_on_drop_and_remove_is_explicit() {
        let registry = ScopedRegistry::new();
        let kept = registry.insert(ScopeTarget::Global, 1);
        {
            let _guard = registry.register(ScopeTarget::Global, 2);
            assert_eq!(registry.visible(view("a", "s")), vec![1, 2]);
        }
        assert_eq!(registry.visible(view("a", "s")), vec![1]);
        assert_eq!(registry.remove(kept), Some(1));
        assert!(registry.is_empty());
    }

    #[test]
    fn guard_outliving_registry_is_harmless() {
        let registry = ScopedRegistry::new();
        let guard = registry.register(ScopeTarget::Global, 1);
        drop(registry);
        drop(guard);
    }
}
