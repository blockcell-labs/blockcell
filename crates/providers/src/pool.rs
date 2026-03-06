use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use blockcell_core::Config;
use tracing::{info, warn};

use crate::factory::create_provider;
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
#[derive(Debug, Clone)]
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

impl ProviderPool {
    /// 从 config 构建 ProviderPool。
    ///
    /// - 如果 `config.agents.defaults.model_pool` 非空，使用 pool 配置。
    /// - 否则沿用旧的单 `model` + `provider` 配置（向后兼容）。
    pub fn from_config(config: &Config) -> anyhow::Result<Arc<Self>> {
        let defaults = &config.agents.defaults;

        // 收集 ModelEntry 列表（兼容旧配置）
        let entries_cfg: Vec<(String, String, u32, u32)> = if !defaults.model_pool.is_empty() {
            defaults
                .model_pool
                .iter()
                .map(|e| (e.model.clone(), e.provider.clone(), e.weight, e.priority))
                .collect()
        } else {
            // 旧配置：单条目
            let model = defaults.model.clone();
            let provider_name = defaults.provider.clone().unwrap_or_default();
            vec![(model, provider_name, 1, 1)]
        };

        if entries_cfg.is_empty() {
            return Err(anyhow::anyhow!(
                "model_pool is empty and no default model configured"
            ));
        }

        let mut entries = Vec::with_capacity(entries_cfg.len());
        let mut health_map = HashMap::new();
        let stats_map = HashMap::new();

        for (idx, (model, provider_name, weight, priority)) in entries_cfg.into_iter().enumerate() {
            let explicit = if provider_name.is_empty() {
                None
            } else {
                Some(provider_name.as_str())
            };
            match create_provider(config, &model, explicit) {
                Ok(p) => {
                    info!(
                        idx, model = %model, provider = %provider_name,
                        weight, priority, "ProviderPool: entry loaded"
                    );
                    entries.push(BuiltEntry {
                        model,
                        provider_name,
                        weight,
                        priority,
                        provider: Arc::from(p),
                    });
                    health_map.insert(idx, EntryHealth::Healthy);
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
    /// 1. 过滤出 Healthy（或冷却已到期的）条目
    /// 2. 按 priority 升序分组，取最高优先级组
    /// 3. 在组内按 weight 加权随机选取
    /// 4. 若 healthy 池为空，临时解除所有冷却（降级保底）
    ///
    /// 返回 `(entry_index, Arc<dyn Provider>)`
    pub fn acquire(&self) -> Option<(usize, Arc<dyn Provider>)> {
        let mut state = self.state.lock().unwrap();
        let now = Instant::now();

        // 解除到期的冷却
        for (idx, h) in state.health.iter_mut() {
            if let EntryHealth::Cooling(since) = h {
                if now.duration_since(*since) >= self.cooldown {
                    info!(idx, "ProviderPool: entry recovered from cooling");
                    *h = EntryHealth::Healthy;
                }
            }
        }

        // 收集健康条目索引
        let healthy: Vec<usize> = (0..self.entries.len())
            .filter(|idx| state.health.get(idx) == Some(&EntryHealth::Healthy))
            .collect();

        let candidates = if !healthy.is_empty() {
            healthy
        } else {
            // 降级保底：临时解除所有 Cooling，Dead 不解除
            let fallback: Vec<usize> = (0..self.entries.len())
                .filter(|idx| {
                    matches!(
                        state.health.get(idx),
                        Some(EntryHealth::Healthy) | Some(EntryHealth::Cooling(_))
                    )
                })
                .collect();
            if fallback.is_empty() {
                warn!("ProviderPool: all entries are dead or unavailable");
                return None;
            }
            warn!("ProviderPool: all entries cooling, using fallback selection");
            for idx in &fallback {
                state.health.insert(*idx, EntryHealth::Healthy);
            }
            fallback
        };

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

        // 简单的伪随机：用当前纳秒时间戳 mod total_weight
        let rand_val = {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos();
            nanos % total_weight
        };

        let mut cumulative = 0u32;
        let mut selected = top_group[0];
        for idx in &top_group {
            cumulative += self.entries[*idx].weight;
            if rand_val < cumulative {
                selected = *idx;
                break;
            }
        }

        Some((selected, Arc::clone(&self.entries[selected].provider)))
    }

    /// 上报调用结果，驱动健康状态变更。
    pub fn report(&self, idx: usize, result: CallResult) {
        let mut state = self.state.lock().unwrap();

        match result {
            CallResult::Success => {
                let stats = state.stats.entry(idx).or_default();
                stats.success_count += 1;
                stats.transient_fail_count = 0; // 成功后重置连续失败计数
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

    /// 将错误字符串分类为 CallResult
    pub fn classify_error(err: &str) -> CallResult {
        let lower = err.to_lowercase();
        if lower.contains("429")
            || lower.contains("rate limit")
            || lower.contains("too many requests")
        {
            CallResult::RateLimit
        } else if lower.contains("401")
            || lower.contains("403")
            || lower.contains("unauthorized")
            || lower.contains("invalid api key")
            || lower.contains("authentication")
            || lower.contains("api key")
        {
            CallResult::AuthError
        } else if lower.contains("500")
            || lower.contains("502")
            || lower.contains("503")
            || lower.contains("504")
            || lower.contains("server error")
        {
            CallResult::ServerError
        } else {
            CallResult::Transient
        }
    }

    /// 返回池状态摘要（用于日志/status 命令）
    pub fn status_summary(&self) -> Vec<PoolEntryStatus> {
        let state = self.state.lock().unwrap();
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
            model: "ollama/llama3".to_string(),
            provider: "ollama".to_string(),
            weight: 2,
            priority: 1,
            input_price: None,
            output_price: None,
        }];
        let result = ProviderPool::from_config(&config);
        assert!(
            result.is_ok(),
            "pool with ollama entry should build successfully"
        );
        let pool = result.unwrap();
        let status = pool.status_summary();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].model, "ollama/llama3");
        assert_eq!(status[0].weight, 2);
    }

    #[test]
    fn test_report_transient_fails_then_cooling() {
        let mut config = Config::default();
        config.agents.defaults.model_pool = vec![blockcell_core::config::ModelEntry {
            model: "ollama/llama3".to_string(),
            provider: "ollama".to_string(),
            weight: 1,
            priority: 1,
            input_price: None,
            output_price: None,
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
            model: "ollama/llama3".to_string(),
            provider: "ollama".to_string(),
            weight: 1,
            priority: 1,
            input_price: None,
            output_price: None,
        }];
        let pool = ProviderPool::from_config(&config).unwrap();
        pool.report(0, CallResult::AuthError);
        let status = pool.status_summary();
        assert_eq!(status[0].health, "dead");
    }
}
