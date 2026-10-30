use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use blockcell_core::config::{RoutingStrategy, ToolCallMode};
use blockcell_core::Config;
use tracing::{debug, info, warn};

use rand::Rng;

use crate::factory::create_provider_with_tool_mode;
use crate::Provider;

/// 单个池条目的运行时健康状态
#[derive(Debug, Clone, PartialEq)]
pub enum EntryHealth {
    /// 正常可用
    Healthy,
    /// 熔断冷却中（记录恢复时间点）
    Cooling(Instant),
    /// 永久不可用（API Key 失效 / 401/403）
    Dead,
}

/// 调用反馈类型，由调用方在 chat() 之后上报
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallResult {
    Success,
    /// 限速（429）— 立即进入冷却
    RateLimit,
    /// 认证失败（401/403）— 永久不可用
    AuthError,
    /// 超时或网络错误 — 累计失败达阈值后冷却
    Transient,
    /// 服务器错误（5xx）— 同上
    ServerError,
}

/// 单条目运行时统计
#[derive(Debug, Default)]
struct EntryStats {
    success_count: u64,
    transient_fail_count: u64,
}

/// Provider 池内部可变状态（Mutex 保护）
struct PoolState {
    health: HashMap<usize, EntryHealth>,
    stats: HashMap<usize, EntryStats>,
}

/// 一个构建好的池条目（不可变，对应 config 中的 ModelEntry）
struct BuiltEntry {
    /// 对应 config model_pool 中的 model 字段（用于日志）
    model: String,
    /// 对应 config model_pool 中的 provider 字段（用于日志）
    provider_name: String,
    /// 权重
    weight: u32,
    /// 优先级（小数字 = 高优先级）
    priority: u32,
    /// 预先构建好的 provider 实例（Arc 以支持多处引用）
    provider: Arc<dyn Provider>,
}

/// Public entry type for constructing a ProviderPool from already-built providers.
pub struct ProviderPoolEntry {
    pub model: String,
    pub provider_name: String,
    pub weight: u32,
    pub priority: u32,
    pub provider: Arc<dyn Provider>,
}

/// 多模型高可用 Provider 池。
///
/// 工作流程：
/// 1. `acquire()` — 按优先级分组，在最高优先级组内加权随机选取健康条目
/// 2. 调用方执行 `provider.chat(...)` 后调用 `report(idx, result)` 上报结果
/// 3. 达到失败阈值自动熔断，冷却到期自动恢复
/// 4. AuthError 直接标记 Dead，不再参与选取
pub struct ProviderPool {
    entries: Vec<BuiltEntry>,
    state: Mutex<PoolState>,
    /// 单条目累计 Transient/ServerError 超过此阈值进入冷却，默认 3
    fail_threshold: u32,
    /// 冷却时长，默认 60 秒
    cooldown: Duration,
}

/// Context used by model routing strategies.
#[derive(Debug, Clone, Default)]
pub struct RoutingContext {
    pub message_count: usize,
    pub estimated_tokens: usize,
    pub intent: Option<String>,
}

impl ProviderPool {
    /// Build a pool from a single already-constructed provider.
    /// Useful for tests and for embedding a deterministic provider in local flows.
    pub fn from_single_provider(
        model: impl Into<String>,
        provider_name: impl Into<String>,
        provider: Arc<dyn Provider>,
    ) -> Arc<Self> {
        Arc::new(Self {
            entries: vec![BuiltEntry {
                model: model.into(),
                provider_name: provider_name.into(),
                weight: 1,
                priority: 1,
                provider,
            }],
            state: Mutex::new(PoolState {
                health: HashMap::from([(0, EntryHealth::Healthy)]),
                stats: HashMap::new(),
            }),
            fail_threshold: 3,
            cooldown: Duration::from_secs(60),
        })
    }

    /// Build a pool from already-constructed provider entries.
    pub fn from_entries(entries: Vec<ProviderPoolEntry>) -> Arc<Self> {
        let entries: Vec<BuiltEntry> = entries
            .into_iter()
            .map(|entry| BuiltEntry {
                model: entry.model,
                provider_name: entry.provider_name,
                weight: entry.weight,
                priority: entry.priority,
                provider: entry.provider,
            })
            .collect();
        let health = (0..entries.len())
            .map(|idx| (idx, EntryHealth::Healthy))
            .collect();
        Arc::new(Self {
            entries,
            state: Mutex::new(PoolState {
                health,
                stats: HashMap::new(),
            }),
            fail_threshold: 3,
            cooldown: Duration::from_secs(60),
        })
    }

    /// 从 config 构建 ProviderPool。
    ///
    /// - 如果 `config.agents.defaults.model_pool` 非空，使用 pool 配置。
    /// - 否则沿用旧的单 `model` + `provider` 配置（向后兼容）。
    pub fn from_config(config: &Config) -> anyhow::Result<Arc<Self>> {
        let defaults = &config.agents.defaults;

        // 收集 ModelEntry 列表（兼容旧配置）
        let entries_cfg: Vec<(String, String, u32, u32, ToolCallMode, Option<f32>)> =
            if !defaults.model_pool.is_empty() {
                defaults
                    .model_pool
                    .iter()
                    .map(|e| {
                        (
                            e.model.clone(),
                            e.provider.clone(),
                            e.weight,
                            e.priority,
                            e.tool_call_mode,
                            e.temperature,
                        )
                    })
                    .collect()
            } else {
                // 旧配置：单条目
                let model = defaults.model.clone();
                let provider_name = defaults.provider.clone().unwrap_or_default();
                vec![(model, provider_name, 1, 1, ToolCallMode::Native, None)]
            };

        if entries_cfg.is_empty() {
            return Err(anyhow::anyhow!(
                "model_pool is empty and no default model configured"
            ));
        }

        let mut entries = Vec::with_capacity(entries_cfg.len());
        let mut health_map = HashMap::new();
        let stats_map = HashMap::new();

        for (idx, (model, provider_name, weight, priority, tool_call_mode, temperature)) in
            entries_cfg.into_iter().enumerate()
        {
            let explicit = if provider_name.is_empty() {
                None
            } else {
                Some(provider_name.as_str())
            };
            let temperature = temperature.unwrap_or(defaults.temperature);
            match create_provider_with_tool_mode(
                config,
                &model,
                explicit,
                Some(tool_call_mode),
                Some(temperature),
            ) {
                Ok(p) => {
                    info!(
                        idx, model = %model, provider = %provider_name,
                        weight, priority, "ProviderPool: entry loaded"
                    );
                    let built_idx = entries.len();
                    entries.push(BuiltEntry {
                        model,
                        provider_name,
                        weight,
                        priority,
                        provider: Arc::from(p),
                    });
                    health_map.insert(built_idx, EntryHealth::Healthy);
                }
                Err(e) => {
                    warn!(idx, model = %model, error = %e, "ProviderPool: failed to build entry, skipping");
                }
            }
        }

        if entries.is_empty() {
            return Err(anyhow::anyhow!(
                "ProviderPool: no usable entries could be built from config. \
                 Check that providers have valid api_key values."
            ));
        }

        Ok(Arc::new(Self {
            entries,
            state: Mutex::new(PoolState {
                health: health_map,
                stats: stats_map,
            }),
            fail_threshold: 3,
            cooldown: Duration::from_secs(60),
        }))
    }

    /// 从池中获取一个 provider 实例及其索引。
    ///
    /// 选取算法：
    /// 1. 过滤出 Healthy（冷却已到期的先被恢复）条目
    /// 2. 按 priority 升序分组，取最高优先级组
    /// 3. 在组内按 weight 加权随机选取
    /// 4. 若没有健康条目，返回 `None`——不会临时解除冷却
    ///
    /// 返回 `(entry_index, Arc<dyn Provider>)`
    pub fn acquire(&self) -> Option<(usize, Arc<dyn Provider>)> {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("ProviderPool 状态 Mutex 被污染，正在恢复");
            poisoned.into_inner()
        });
        self.recover_cooling_entries(&mut state);

        // 收集健康条目索引
        let healthy: Vec<usize> = (0..self.entries.len())
            .filter(|idx| state.health.get(idx) == Some(&EntryHealth::Healthy))
            .collect();

        if healthy.is_empty() {
            if self.entries.len() == 1
                && matches!(state.health.get(&0), Some(EntryHealth::Cooling(_)))
            {
                debug!("ProviderPool: single provider is cooling; caller controls retry timing");
            } else {
                warn!("ProviderPool: all entries are cooling, dead, or unavailable");
            }
            return None;
        }

        let selected = self.select_entry_from_candidates(healthy)?;
        Some((selected, Arc::clone(&self.entries[selected].provider)))
    }

    /// Acquire a provider using an explicit routing strategy and request context.
    pub fn acquire_with_strategy(
        &self,
        strategy: RoutingStrategy,
        context: &RoutingContext,
    ) -> Option<(usize, Arc<dyn Provider>)> {
        self.acquire_with_strategy_excluding(strategy, context, &[])
    }

    /// Acquire using a routing strategy while excluding entries already tried for this call.
    pub fn acquire_with_strategy_excluding(
        &self,
        strategy: RoutingStrategy,
        context: &RoutingContext,
        excluded: &[usize],
    ) -> Option<(usize, Arc<dyn Provider>)> {
        match strategy {
            RoutingStrategy::Manual | RoutingStrategy::QualityFirst => {
                self.acquire_excluding(excluded)
            }
            RoutingStrategy::CostOptimized => {
                if context.message_count <= 4 {
                    self.acquire_cheapest_excluding(excluded)
                } else {
                    self.acquire_excluding(excluded)
                }
            }
            RoutingStrategy::LatencyFirst => self.acquire_lowest_latency_excluding(excluded),
        }
    }

    /// Acquire a healthy provider if one is available, otherwise probe the cooling
    /// provider whose cooldown began earliest.
    pub fn acquire_probe_excluding(
        &self,
        strategy: RoutingStrategy,
        context: &RoutingContext,
        excluded: &[usize],
    ) -> Option<(usize, Arc<dyn Provider>)> {
        if let Some(provider) = self.acquire_with_strategy_excluding(strategy, context, excluded) {
            return Some(provider);
        }

        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("ProviderPool 状态 Mutex 被污染，正在恢复");
            poisoned.into_inner()
        });
        self.recover_cooling_entries(&mut state);

        let selected = (0..self.entries.len())
            .filter(|idx| !excluded.contains(idx))
            .filter_map(|idx| match state.health.get(&idx) {
                Some(EntryHealth::Cooling(since)) => Some((idx, *since)),
                _ => None,
            })
            .min_by_key(|(_, since)| *since)
            .map(|(idx, _)| idx)?;

        Some((selected, Arc::clone(&self.entries[selected].provider)))
    }

    fn acquire_excluding(&self, excluded: &[usize]) -> Option<(usize, Arc<dyn Provider>)> {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("ProviderPool 状态 Mutex 被污染，正在恢复");
            poisoned.into_inner()
        });
        self.recover_cooling_entries(&mut state);

        let candidates = self.healthy_candidate_indices(&mut state, excluded)?;
        let selected = self.select_entry_from_candidates(candidates)?;
        Some((selected, Arc::clone(&self.entries[selected].provider)))
    }

    fn acquire_cheapest_excluding(&self, excluded: &[usize]) -> Option<(usize, Arc<dyn Provider>)> {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("ProviderPool 状态 Mutex 被污染，正在恢复");
            poisoned.into_inner()
        });
        self.recover_cooling_entries(&mut state);

        let healthy = self.healthy_candidate_indices(&mut state, excluded)?;
        let max_priority = healthy
            .iter()
            .map(|idx| self.entries[*idx].priority)
            .max()?;
        let candidates: Vec<usize> = healthy
            .into_iter()
            .filter(|idx| self.entries[*idx].priority == max_priority)
            .collect();
        let selected = self.select_weighted_from_candidates(candidates)?;
        Some((selected, Arc::clone(&self.entries[selected].provider)))
    }

    fn acquire_lowest_latency_excluding(
        &self,
        excluded: &[usize],
    ) -> Option<(usize, Arc<dyn Provider>)> {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("ProviderPool 状态 Mutex 被污染，正在恢复");
            poisoned.into_inner()
        });
        self.recover_cooling_entries(&mut state);

        let healthy = self.healthy_candidate_indices(&mut state, excluded)?;
        let selected = healthy
            .iter()
            .copied()
            .max_by_key(|idx| state.stats.get(idx).map(|s| s.success_count).unwrap_or(0))
            .or_else(|| self.select_entry_from_candidates(healthy))?;
        Some((selected, Arc::clone(&self.entries[selected].provider)))
    }

    /// Acquire a provider that exactly matches the requested model.
    ///
    /// Unlike `acquire()`, this never falls back to another model: callers that set an
    /// explicit model override need the override to be honored or fail loudly.
    pub fn acquire_by_model(&self, model: &str) -> Option<(usize, Arc<dyn Provider>)> {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("ProviderPool 状态 Mutex 被污染，正在恢复");
            poisoned.into_inner()
        });
        self.recover_cooling_entries(&mut state);

        let candidates: Vec<usize> = (0..self.entries.len())
            .filter(|idx| self.entries[*idx].model == model)
            .filter(|idx| state.health.get(idx) == Some(&EntryHealth::Healthy))
            .collect();

        if candidates.is_empty() {
            warn!(
                model,
                "ProviderPool: no healthy provider matches requested model"
            );
            return None;
        }

        let selected = self.select_entry_from_candidates(candidates)?;
        Some((selected, Arc::clone(&self.entries[selected].provider)))
    }

    fn recover_cooling_entries(&self, state: &mut PoolState) {
        let now = Instant::now();

        for (idx, h) in state.health.iter_mut() {
            if let EntryHealth::Cooling(since) = h {
                if now.duration_since(*since) >= self.cooldown {
                    info!(idx, "ProviderPool: entry recovered from cooling");
                    *h = EntryHealth::Healthy;
                }
            }
        }
    }

    /// Healthy 条目索引（排除 `excluded`）；没有健康条目时返回 `None`。
    ///
    /// 注意：这里**不**做"临时解除冷却"的降级保底——复用限速中的后端只会
    /// 继续撞限流，所以宁可返回 `None` 让调用方走别的路径。
    fn healthy_candidate_indices(
        &self,
        state: &mut PoolState,
        excluded: &[usize],
    ) -> Option<Vec<usize>> {
        let healthy: Vec<usize> = (0..self.entries.len())
            .filter(|idx| state.health.get(idx) == Some(&EntryHealth::Healthy))
            .filter(|idx| !excluded.contains(idx))
            .collect();

        if !healthy.is_empty() {
            return Some(healthy);
        }

        if self.entries.len() == 1 && matches!(state.health.get(&0), Some(EntryHealth::Cooling(_)))
        {
            debug!("ProviderPool: single provider is cooling; caller controls retry timing");
        } else {
            warn!("ProviderPool: all eligible entries are cooling, dead, or unavailable");
        }
        None
    }

    fn select_entry_from_candidates(&self, candidates: Vec<usize>) -> Option<usize> {
        // 按 priority 分组，取最高优先级（最小值）
        let min_priority = candidates
            .iter()
            .map(|idx| self.entries[*idx].priority)
            .min()
            .unwrap_or(1);

        let top_group: Vec<usize> = candidates
            .into_iter()
            .filter(|idx| self.entries[*idx].priority == min_priority)
            .collect();

        // 加权随机选取
        let total_weight: u32 = top_group.iter().map(|idx| self.entries[*idx].weight).sum();

        if total_weight == 0 {
            return None;
        }

        // 加权随机选取
        let rand_val = rand::thread_rng().gen_range(0..total_weight);

        let mut cumulative = 0u32;
        let mut selected = top_group[0];
        for idx in &top_group {
            cumulative += self.entries[*idx].weight;
            if rand_val < cumulative {
                selected = *idx;
                break;
            }
        }

        Some(selected)
    }

    fn select_weighted_from_candidates(&self, candidates: Vec<usize>) -> Option<usize> {
        if candidates.is_empty() {
            return None;
        }

        let total_weight: u32 = candidates.iter().map(|idx| self.entries[*idx].weight).sum();
        if total_weight == 0 {
            return None;
        }

        let rand_val = rand::thread_rng().gen_range(0..total_weight);
        let mut cumulative = 0u32;
        let mut selected = candidates[0];
        for idx in &candidates {
            cumulative += self.entries[*idx].weight;
            if rand_val < cumulative {
                selected = *idx;
                break;
            }
        }

        Some(selected)
    }

    /// 上报调用结果，驱动健康状态变更。
    pub fn report(&self, idx: usize, result: CallResult) {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("ProviderPool 状态 Mutex 被污染，正在恢复");
            poisoned.into_inner()
        });

        match result {
            CallResult::Success => {
                let stats = state.stats.entry(idx).or_default();
                stats.success_count += 1;
                stats.transient_fail_count = 0; // 成功后重置连续失败计数
                state.health.insert(idx, EntryHealth::Healthy);
            }
            CallResult::RateLimit => {
                warn!(idx, model = %self.entries[idx].model, "ProviderPool: rate limited, entering cooldown");
                state
                    .health
                    .insert(idx, EntryHealth::Cooling(Instant::now()));
            }
            CallResult::AuthError => {
                warn!(idx, model = %self.entries[idx].model, "ProviderPool: auth error, marking dead");
                state.health.insert(idx, EntryHealth::Dead);
            }
            CallResult::Transient | CallResult::ServerError => {
                let new_count = {
                    let stats = state.stats.entry(idx).or_default();
                    stats.transient_fail_count += 1;
                    stats.transient_fail_count
                };
                if new_count >= self.fail_threshold as u64 {
                    warn!(
                        idx,
                        model = %self.entries[idx].model,
                        fail_count = new_count,
                        "ProviderPool: fail threshold reached, entering cooldown"
                    );
                    state
                        .health
                        .insert(idx, EntryHealth::Cooling(Instant::now()));
                    state.stats.entry(idx).or_default().transient_fail_count = 0;
                }
            }
        }
    }

    /// Prefer structured adapter errors. String parsing is a legacy boundary only.
    pub fn classify_failure(error: &blockcell_core::Error) -> CallResult {
        if let blockcell_core::Error::ProviderFailure(failure) = error {
            match failure.kind {
                blockcell_core::FailureKind::RateLimited => CallResult::RateLimit,
                blockcell_core::FailureKind::AuthenticationFailed => CallResult::AuthError,
                blockcell_core::FailureKind::ProviderUnavailable
                    if failure.status.is_some_and(|status| status >= 500) =>
                {
                    CallResult::ServerError
                }
                _ => CallResult::Transient,
            }
        } else {
            Self::classify_error(&error.to_string())
        }
    }

    /// 将错误字符串分类为 CallResult
    pub fn classify_error(err: &str) -> CallResult {
        let lower = err.to_lowercase();
        if lower.contains("http_status=429")
            || lower.contains("http 429")
            || lower.contains("rate limit")
            || lower.contains("too many requests")
        {
            CallResult::RateLimit
        } else if lower.contains("http_status=401")
            || lower.contains("http_status=403")
            || lower.contains("http 401")
            || lower.contains("http 403")
            || lower.contains("unauthorized")
            || lower.contains("invalid api key")
            || lower.contains("authentication failed")
        {
            CallResult::AuthError
        } else if lower.contains("http_status=500")
            || lower.contains("http_status=502")
            || lower.contains("http_status=503")
            || lower.contains("http_status=504")
            || lower.contains("http 500")
            || lower.contains("http 502")
            || lower.contains("http 503")
            || lower.contains("http 504")
            || lower.contains("server error")
        {
            CallResult::ServerError
        } else {
            CallResult::Transient
        }
    }

    /// 返回池状态摘要（用于日志/status 命令）
    /// `acquire*` 返回的条目对应的模型与 provider 名称。
    pub fn served_by(&self, idx: usize) -> Option<ServedBy> {
        self.entries.get(idx).map(|entry| ServedBy {
            model: entry.model.clone(),
            provider: entry.provider_name.clone(),
        })
    }

    pub fn status_summary(&self) -> Vec<PoolEntryStatus> {
        let state = self.state.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("ProviderPool 状态 Mutex 被污染，正在恢复");
            poisoned.into_inner()
        });
        let now = Instant::now();
        (0..self.entries.len())
            .map(|idx| {
                let entry = &self.entries[idx];
                let health_str = match state.health.get(&idx) {
                    Some(EntryHealth::Healthy) => "healthy".to_string(),
                    Some(EntryHealth::Cooling(since)) => {
                        let remaining = self.cooldown.saturating_sub(now.duration_since(*since));
                        format!("cooling({}s)", remaining.as_secs())
                    }
                    Some(EntryHealth::Dead) => "dead".to_string(),
                    None => "unknown".to_string(),
                };
                let stats = state.stats.get(&idx);
                PoolEntryStatus {
                    index: idx,
                    model: entry.model.clone(),
                    provider: entry.provider_name.clone(),
                    weight: entry.weight,
                    priority: entry.priority,
                    health: health_str,
                    success_count: stats.map(|s| s.success_count).unwrap_or(0),
                    fail_count: stats.map(|s| s.transient_fail_count).unwrap_or(0),
                }
            })
            .collect()
    }
}

/// 实际服务一次模型调用的池条目，用于事件记录与评估。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServedBy {
    pub model: String,
    pub provider: String,
}

/// 池中单条目的状态摘要（用于外部展示）
#[derive(Debug, Clone)]
pub struct PoolEntryStatus {
    pub index: usize,
    pub model: String,
    pub provider: String,
    pub weight: u32,
    pub priority: u32,
    pub health: String,
    pub success_count: u64,
    pub fail_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use blockcell_core::types::{ChatMessage, LLMResponse};
    use serde_json::Value;

    struct DummyProvider;

    #[async_trait]
    impl Provider for DummyProvider {
        async fn chat(
            &self,
            _messages: &[ChatMessage],
            _tools: &[Value],
        ) -> blockcell_core::Result<LLMResponse> {
            Ok(LLMResponse::default())
        }
    }

    #[test]
    fn test_classify_error_rate_limit() {
        assert!(matches!(
            ProviderPool::classify_error("HTTP 429 Too Many Requests"),
            CallResult::RateLimit
        ));
        assert!(matches!(
            ProviderPool::classify_error("rate limit exceeded"),
            CallResult::RateLimit
        ));
    }

    #[test]
    fn test_classify_error_auth() {
        assert!(matches!(
            ProviderPool::classify_error("HTTP 401 Unauthorized"),
            CallResult::AuthError
        ));
        assert!(matches!(
            ProviderPool::classify_error("invalid api key provided"),
            CallResult::AuthError
        ));
    }

    #[test]
    fn test_classify_error_server() {
        assert!(matches!(
            ProviderPool::classify_error("HTTP 503 Service Unavailable"),
            CallResult::ServerError
        ));
    }

    #[test]
    fn test_classify_error_transient() {
        assert!(matches!(
            ProviderPool::classify_error("connection timeout"),
            CallResult::Transient
        ));
    }

    #[test]
    fn test_pool_from_config_single_model_no_key_fails() {
        let config = Config::default();
        // 默认 config 没有 api_key，但有 ollama（不需要 key），pool 构建应成功（ollama 条目）
        // 或者单条目用旧配置路径 → 推断 provider → ollama 作为 fallback
        // 主要验证不 panic
        let _ = ProviderPool::from_config(&config);
    }

    #[test]
    fn test_pool_from_config_with_pool_entries() {
        let mut config = Config::default();
        config.agents.defaults.model_pool = vec![blockcell_core::config::ModelEntry {
            model: "ollama/qwen3.6".to_string(),
            provider: "ollama".to_string(),
            weight: 2,
            priority: 1,
            input_price: None,
            output_price: None,
            temperature: None,
            tool_call_mode: blockcell_core::config::ToolCallMode::Native,
        }];
        let result = ProviderPool::from_config(&config);
        assert!(
            result.is_ok(),
            "pool with ollama entry should build successfully"
        );
        let pool = result.unwrap();
        let status = pool.status_summary();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].model, "ollama/qwen3.6");
        assert_eq!(status[0].weight, 2);
    }

    #[test]
    fn pool_skips_invalid_entry_without_health_index_gap() {
        let mut config = Config::default();
        config.agents.defaults.model_pool = vec![
            blockcell_core::config::ModelEntry {
                model: "missing/model".to_string(),
                provider: "provider-that-does-not-exist".to_string(),
                weight: 1,
                priority: 1,
                input_price: None,
                output_price: None,
                temperature: None,
                tool_call_mode: blockcell_core::config::ToolCallMode::Native,
            },
            blockcell_core::config::ModelEntry {
                model: "ollama/qwen3.6".to_string(),
                provider: "ollama".to_string(),
                weight: 1,
                priority: 2,
                input_price: None,
                output_price: None,
                temperature: None,
                tool_call_mode: blockcell_core::config::ToolCallMode::Native,
            },
        ];

        let pool = ProviderPool::from_config(&config).expect("valid second entry should build");
        let status = pool.status_summary();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].health, "healthy");
        assert_eq!(pool.acquire().map(|(idx, _)| idx), Some(0));
    }

    #[test]
    fn test_report_transient_fails_then_cooling() {
        let mut config = Config::default();
        config.agents.defaults.model_pool = vec![blockcell_core::config::ModelEntry {
            model: "ollama/qwen3.6".to_string(),
            provider: "ollama".to_string(),
            weight: 1,
            priority: 1,
            input_price: None,
            output_price: None,
            temperature: None,
            tool_call_mode: blockcell_core::config::ToolCallMode::Native,
        }];
        let pool = ProviderPool::from_config(&config).unwrap();
        // 连续上报 3 次 Transient 应触发冷却
        pool.report(0, CallResult::Transient);
        pool.report(0, CallResult::Transient);
        pool.report(0, CallResult::Transient);
        let status = pool.status_summary();
        assert!(
            status[0].health.starts_with("cooling"),
            "should enter cooling after 3 transient failures"
        );
    }

    #[test]
    fn test_report_auth_error_dead() {
        let mut config = Config::default();
        config.agents.defaults.model_pool = vec![blockcell_core::config::ModelEntry {
            model: "ollama/qwen3.6".to_string(),
            provider: "ollama".to_string(),
            weight: 1,
            priority: 1,
            input_price: None,
            output_price: None,
            temperature: None,
            tool_call_mode: blockcell_core::config::ToolCallMode::Native,
        }];
        let pool = ProviderPool::from_config(&config).unwrap();
        pool.report(0, CallResult::AuthError);
        let status = pool.status_summary();
        assert_eq!(status[0].health, "dead");
    }

    #[test]
    fn cooling_provider_is_not_reactivated_early() {
        let pool = ProviderPool {
            entries: vec![BuiltEntry {
                model: "model-a".to_string(),
                provider_name: "test".to_string(),
                weight: 1,
                priority: 1,
                provider: Arc::new(DummyProvider),
            }],
            state: Mutex::new(PoolState {
                health: HashMap::from([(0, EntryHealth::Cooling(Instant::now()))]),
                stats: HashMap::new(),
            }),
            fail_threshold: 3,
            cooldown: Duration::from_secs(60),
        };

        assert!(pool.acquire().is_none());
    }

    #[test]
    fn rate_limited_provider_is_available_as_a_probe_after_normal_acquire_fails() {
        let pool = ProviderPool::from_single_provider("model-a", "test", Arc::new(DummyProvider));
        pool.report(0, CallResult::RateLimit);
        let context = RoutingContext::default();

        assert!(pool
            .acquire_with_strategy(RoutingStrategy::Manual, &context)
            .is_none());
        assert_eq!(
            pool.acquire_probe_excluding(RoutingStrategy::Manual, &context, &[])
                .map(|(idx, _)| idx),
            Some(0)
        );
    }

    #[test]
    fn dead_provider_is_never_returned_as_a_probe() {
        let pool = ProviderPool::from_single_provider("model-a", "test", Arc::new(DummyProvider));
        pool.report(0, CallResult::AuthError);

        assert!(pool
            .acquire_probe_excluding(RoutingStrategy::Manual, &RoutingContext::default(), &[])
            .is_none());
    }

    #[test]
    fn successful_probe_restores_provider_health() {
        let pool = ProviderPool::from_single_provider("model-a", "test", Arc::new(DummyProvider));
        pool.report(0, CallResult::RateLimit);
        let (idx, _) = pool
            .acquire_probe_excluding(RoutingStrategy::Manual, &RoutingContext::default(), &[])
            .expect("cooling provider should be probeable");

        pool.report(idx, CallResult::Success);

        assert_eq!(pool.status_summary()[0].health, "healthy");
        assert!(pool.acquire().is_some());
    }

    #[test]
    fn incidental_api_key_text_is_not_auth_error() {
        assert_eq!(
            ProviderPool::classify_error(
                "HTTP_STATUS=500 upstream API key service temporarily unavailable"
            ),
            CallResult::ServerError
        );
        assert_eq!(
            ProviderPool::classify_error("response documentation mentions api key usage"),
            CallResult::Transient
        );
    }

    #[test]
    fn test_acquire_by_model_selects_exact_model() {
        let pool = ProviderPool {
            entries: vec![
                BuiltEntry {
                    model: "model-a".to_string(),
                    provider_name: "test".to_string(),
                    weight: 1,
                    priority: 1,
                    provider: Arc::new(DummyProvider),
                },
                BuiltEntry {
                    model: "model-b".to_string(),
                    provider_name: "test".to_string(),
                    weight: 1,
                    priority: 1,
                    provider: Arc::new(DummyProvider),
                },
            ],
            state: Mutex::new(PoolState {
                health: HashMap::from([(0, EntryHealth::Healthy), (1, EntryHealth::Healthy)]),
                stats: HashMap::new(),
            }),
            fail_threshold: 3,
            cooldown: Duration::from_secs(60),
        };

        let (idx, _) = pool
            .acquire_by_model("model-b")
            .expect("model-b should be available");
        assert_eq!(idx, 1);
        assert!(pool.acquire_by_model("missing-model").is_none());
    }

    #[test]
    fn cost_optimized_short_context_selects_lowest_priority_entry() {
        let pool = ProviderPool {
            entries: vec![
                BuiltEntry {
                    model: "strong-model".to_string(),
                    provider_name: "test".to_string(),
                    weight: 1,
                    priority: 1,
                    provider: Arc::new(DummyProvider),
                },
                BuiltEntry {
                    model: "cheap-model".to_string(),
                    provider_name: "test".to_string(),
                    weight: 1,
                    priority: 10,
                    provider: Arc::new(DummyProvider),
                },
            ],
            state: Mutex::new(PoolState {
                health: HashMap::from([(0, EntryHealth::Healthy), (1, EntryHealth::Healthy)]),
                stats: HashMap::new(),
            }),
            fail_threshold: 3,
            cooldown: Duration::from_secs(60),
        };

        let (idx, _) = pool
            .acquire_with_strategy(
                blockcell_core::config::RoutingStrategy::CostOptimized,
                &RoutingContext {
                    message_count: 2,
                    estimated_tokens: 128,
                    intent: None,
                },
            )
            .expect("cheap entry should be selected");

        assert_eq!(idx, 1);
    }
}
