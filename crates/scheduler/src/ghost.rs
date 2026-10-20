use blockcell_core::{Config, Error, InboundMessage, Paths, Result};
use chrono::Utc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Ghost maintenance system prompt — a background maintenance persona.
/// Optimized for minimal token usage (P2-2).
#[allow(dead_code)]
const GHOST_SYSTEM_PROMPT: &str = r#"You are Ghost, Blockcell's background maintenance agent.
Constraints: background-only, restricted permissions, minimize tokens.
Tools: memory_maintenance, community_hub, list_dir, file_ops, notification (critical only).
Rules: NEVER save routine logs to memory. Maintain SQLite long-term memory only for genuine durable facts. Do not create skills.
Output: respond with a brief JSON summary at the end (see routine prompt for format).
"#;

/// Configuration for the scheduled Ghost maintenance service, read from config.json5 agents.ghost.
///
/// This is intentionally separate from embedded Ghost learning. Embedded learning
/// runs inside AgentRuntime during normal user turns and does not depend on this
/// scheduled maintenance service being enabled.
#[derive(Debug, Clone)]
pub struct GhostMaintenanceServiceConfig {
    pub enabled: bool,
    pub model: Option<String>,
    pub schedule: String,
    pub max_syncs_per_day: u32,
    pub auto_social: bool,
}

impl GhostMaintenanceServiceConfig {
    pub fn from_config(config: &Config) -> Self {
        let ghost = &config.agents.ghost;
        Self {
            enabled: ghost.enabled,
            model: ghost.model.clone(),
            schedule: ghost.schedule.clone(),
            max_syncs_per_day: ghost.max_syncs_per_day,
            auto_social: ghost.auto_social,
        }
    }
}

/// Tracks daily sync count to respect max_syncs_per_day.
struct SyncTracker {
    date: String,
    count: u32,
}

impl SyncTracker {
    fn new() -> Self {
        Self {
            date: String::new(),
            count: 0,
        }
    }

    fn can_sync(&self, max: u32) -> bool {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        if self.date != today {
            return true; // New day, reset
        }
        self.count < max
    }

    fn record_sync(&mut self) {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        if self.date != today {
            self.date = today;
            self.count = 1;
        } else {
            self.count += 1;
        }
    }
}

#[derive(Debug, Default)]
struct GhostConfigReloadTracker {
    last_mtime: Option<std::time::SystemTime>,
    loaded_once: bool,
}

impl GhostConfigReloadTracker {
    fn should_reload(&self, current_mtime: Option<std::time::SystemTime>) -> bool {
        !self.loaded_once || current_mtime != self.last_mtime
    }

    fn record_success(&mut self, current_mtime: Option<std::time::SystemTime>) {
        self.loaded_once = true;
        self.last_mtime = current_mtime;
    }
}

pub struct GhostMaintenanceService {
    config: GhostMaintenanceServiceConfig,
    #[allow(dead_code)]
    paths: Paths,
    inbound_tx: mpsc::Sender<InboundMessage>,
    sync_tracker: SyncTracker,
}

/// Default cron schedule used when the configured schedule is invalid.
/// Runs every 6 hours: minute=0, hour=every-6th, dom=*, month=*, dow=*
const DEFAULT_CRON_SCHEDULE: &str = "0 */6 * * *";

impl GhostMaintenanceService {
    /// Normalize and validate a cron schedule expression.
    ///
    /// The `cron` crate expects 6 or 7 fields (with optional seconds prefix).
    /// - 5 fields (min hour dom month dow): prepend "0" seconds field.
    /// - 6 fields (sec min hour dom month dow): use as-is.
    /// - 7 fields (sec min hour dom month dow year): use as-is.
    ///   Any other field count falls back to [`DEFAULT_CRON_SCHEDULE`].
    fn normalize_cron_schedule(expr: &str) -> String {
        let trimmed = expr.trim();
        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        match fields.len() {
            5 => format!("0 {}", trimmed),
            6 | 7 => trimmed.to_string(),
            _ => {
                warn!(
                    schedule = %trimmed,
                    field_count = fields.len(),
                    "Invalid cron schedule (expected 5-7 fields), using default"
                );
                format!("0 {}", DEFAULT_CRON_SCHEDULE)
            }
        }
    }

    fn parse_cron_schedule(expr: &str) -> std::result::Result<cron::Schedule, cron::error::Error> {
        let normalized = Self::normalize_cron_schedule(expr);
        normalized.parse::<cron::Schedule>()
    }

    pub fn new(
        config: GhostMaintenanceServiceConfig,
        paths: Paths,
        inbound_tx: mpsc::Sender<InboundMessage>,
    ) -> Self {
        Self {
            config,
            paths,
            inbound_tx,
            sync_tracker: SyncTracker::new(),
        }
    }

    /// Build the routine prompt based on config.
    /// Optimized for minimal token usage (P2-2): concise instructions + JSON output format.
    pub fn build_routine_prompt(config: &GhostMaintenanceServiceConfig) -> String {
        let mut steps = vec![
            "1. memory_maintenance(action=\"garden\") → follow returned instructions. Promote only stable facts to SQLite long_term memory; delete trivial entries. Do not create skills.".to_string(),
            "2. list_dir media + downloads → file_ops delete files >7 days old. Paths are relative to workspace; do not prefix workspace/. Skip if age unknown.".to_string(),
        ];

        if config.auto_social {
            steps.push(
                "3. community_hub: heartbeat → feed → interact (limits: like≤2, reply≤1, post≤1). Report errors as-is.".to_string()
            );
        }

        let steps_str = steps.join("\n");
        format!(
            "Ghost routine. Execute steps in order:\n{}\n\n\
             Rules: NEVER memory_upsert routine logs. Only maintain genuine durable facts/preferences/projects/tasks in SQLite long_term memory. Do not create skills.\n\n\
             After all steps, output ONLY this JSON (no other text):\n\
             {{\"memory\":{{\"gardened\":N,\"promoted\":N,\"deleted\":N}},\
             \"cleanup\":{{\"files_deleted\":N}},\
             \"social\":{{\"heartbeat\":bool,\"likes\":N,\"replies\":N,\"posts\":N}},\
             \"issues\":[]}}",
            steps_str
        )
    }

    /// Run a single ghost routine cycle.
    async fn run_routine(&mut self) -> Result<()> {
        if !self.sync_tracker.can_sync(self.config.max_syncs_per_day) {
            debug!(
                "Ghost: daily sync limit reached ({}/{}), skipping",
                self.sync_tracker.count, self.config.max_syncs_per_day
            );
            return Ok(());
        }

        info!("👻 GhostMaintenanceService: starting routine cycle");
        self.sync_tracker.record_sync();

        let content = Self::build_routine_prompt(&self.config);

        let mut metadata = serde_json::json!({
            "ghost": true,
            "routine": true,
        });

        if let Some(model) = &self.config.model {
            metadata["model"] = serde_json::Value::String(model.clone());
        }

        let msg = InboundMessage {
            channel: "ghost".to_string(),
            account_id: None,
            sender_id: "ghost".to_string(),
            chat_id: format!("ghost_{}", Utc::now().format("%Y%m%d_%H%M%S")),
            content,
            media: vec![],
            metadata,
            timestamp_ms: Utc::now().timestamp_millis(),
        };

        if let Err(e) = self.inbound_tx.send(msg).await {
            error!(error = %e, "Ghost: failed to send routine message, shutting down");
            return Err(Error::Channel(format!("Ghost routine send failed: {}", e)));
        }

        info!("👻 GhostMaintenanceService: routine message dispatched");
        Ok(())
    }

    /// Parse the cron schedule and run the ghost loop.
    pub async fn run_loop(mut self, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        info!(
            schedule = %self.config.schedule,
            max_syncs = self.config.max_syncs_per_day,
            auto_social = self.config.auto_social,
            enabled = self.config.enabled,
            "👻 GhostMaintenanceService started"
        );

        // Parse cron schedule to determine check interval.
        // We check every 60 seconds whether the cron expression matches.
        let mut schedule = match Self::parse_cron_schedule(&self.config.schedule) {
            Ok(s) => s,
            Err(e) => {
                let normalized = Self::normalize_cron_schedule(&self.config.schedule);
                error!(
                    error = %e,
                    schedule = %self.config.schedule,
                    normalized_schedule = %normalized,
                    "Ghost: invalid cron schedule, falling back to every 4 hours"
                );
                // Fallback: every 4 hours
                "0 0 */4 * * *"
                    .parse::<cron::Schedule>()
                    .expect("hardcoded fallback cron schedule is valid")
            }
        };

        let mut check_interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        check_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // 修复：记录下一次计划执行时间，当 now >= next_scheduled 时触发。
        // 原逻辑用 upcoming().next() 返回未来时间再判断差值 <= 60s，
        // 由于 check_interval 也是 60s，两次 check 之间的触发点可能被完全错过。
        let mut next_scheduled: Option<chrono::DateTime<Utc>> = schedule.upcoming(Utc).next();

        // Clone paths for config reloading
        let config_paths = self.paths.clone();
        // 仅在 config.json5 mtime 变化时才重新读盘+解析（JSON5 解析较重）。
        // 否则每 60s tick 都全量读+解析一次配置，而配置几乎从不变动。
        // 首次 tick 强制加载一次（loaded_once=false），与原“总是加载”行为对齐；
        // 加载后重新记录 mtime，以吸收 load_or_default 写入默认字段造成的 mtime 变化。
        let config_file = config_paths.config_file();
        let config_mtime = |path: &std::path::Path| -> Option<std::time::SystemTime> {
            std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
        };
        let mut reload_tracker = GhostConfigReloadTracker::default();

        let mut consecutive_failures = 0u32;
        const MAX_CONSECUTIVE_FAILURES: u32 = 3;

        loop {
            tokio::select! {
                _ = check_interval.tick() => {
                    // Hot-reload config（按 mtime 短路，避免每分钟重复读+解析）
                    let current_mtime = config_mtime(&config_file);
                    if reload_tracker.should_reload(current_mtime) {
                        let load_paths = config_paths.clone();
                        match crate::blocking::run_blocking(move || Config::load_or_default(&load_paths)).await {
                            Ok(new_config) => {
                                let new_ghost = GhostMaintenanceServiceConfig::from_config(&new_config);

                                // Check if relevant fields changed
                                let schedule_changed = new_ghost.schedule != self.config.schedule;
                                let changed = new_ghost.enabled != self.config.enabled ||
                                             schedule_changed ||
                                             new_ghost.model != self.config.model ||
                                             new_ghost.max_syncs_per_day != self.config.max_syncs_per_day ||
                                             new_ghost.auto_social != self.config.auto_social;

                                if changed {
                                    info!("👻 Ghost config updated via hot-reload");
                                    self.config = new_ghost;

                                    // Re-parse schedule if changed
                                    if schedule_changed {
                                        schedule = match Self::parse_cron_schedule(&self.config.schedule) {
                                            Ok(s) => s,
                                            Err(e) => {
                                                let normalized = Self::normalize_cron_schedule(&self.config.schedule);
                                                error!(
                                                    error = %e,
                                                    schedule = %self.config.schedule,
                                                    normalized_schedule = %normalized,
                                                    "Ghost: invalid cron schedule, falling back to every 4 hours"
                                                );
                                                "0 0 */4 * * *".parse::<cron::Schedule>().unwrap()
                                            }
                                        };
                                        // 修复：schedule 变更后重置 next_scheduled，
                                        // 避免旧的 last_run 去重逻辑阻止新 schedule 的首次执行。
                                        next_scheduled = schedule.upcoming(Utc).next();
                                    }

                                    if !self.config.enabled {
                                        info!("👻 GhostMaintenanceService disabled via config");
                                    } else {
                                        info!("👻 GhostMaintenanceService enabled/updated via config: {}", self.config.schedule);
                                    }
                                }

                                // load_or_default may write defaults and change the mtime.
                                reload_tracker.record_success(config_mtime(&config_file));
                            }
                            Err(error) => {
                                warn!(error = %error, "Ghost config reload failed; will retry on next tick");
                            }
                        }
                    }

                    if !self.config.enabled {
                        continue;
                    }

                    let now = Utc::now();

                    // 触发判断：当前时间已超过或到达计划时间则执行。
                    let should_run = match next_scheduled {
                        Some(scheduled_at) => now >= scheduled_at,
                        None => false,
                    };

                    if should_run {
                        // 推进到下一个计划时间
                        next_scheduled = schedule.upcoming(Utc).next();
                        if let Err(e) = self.run_routine().await {
                            warn!(error = %e.to_string(), "Ghost routine failed");
                            consecutive_failures += 1;
                            if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                                error!("Ghost: too many consecutive failures, stopping maintenance loop");
                                break;
                            }
                        } else {
                            consecutive_failures = 0;
                        }
                    }
                }
                _ = shutdown.recv() => {
                    info!("👻 GhostMaintenanceService shutting down");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghost_config_reload_detects_deleted_file() {
        let previous = std::time::UNIX_EPOCH + std::time::Duration::from_secs(10);
        let tracker = GhostConfigReloadTracker {
            last_mtime: Some(previous),
            loaded_once: true,
        };

        assert!(tracker.should_reload(None));
    }

    #[test]
    fn ghost_config_reload_failure_remains_retryable() {
        let current = std::time::UNIX_EPOCH + std::time::Duration::from_secs(20);
        let tracker = GhostConfigReloadTracker::default();

        assert!(tracker.should_reload(Some(current)));
        assert!(tracker.should_reload(Some(current)));
        assert!(!tracker.loaded_once);
        assert_eq!(tracker.last_mtime, None);
    }

    #[test]
    fn ghost_config_reload_success_records_latest_mtime() {
        let current = std::time::UNIX_EPOCH + std::time::Duration::from_secs(30);
        let mut tracker = GhostConfigReloadTracker::default();

        tracker.record_success(Some(current));

        assert!(tracker.loaded_once);
        assert_eq!(tracker.last_mtime, Some(current));
        assert!(!tracker.should_reload(Some(current)));
    }

    #[test]
    fn test_sync_tracker() {
        let mut tracker = SyncTracker::new();
        assert!(tracker.can_sync(3));
        tracker.record_sync();
        assert!(tracker.can_sync(3));
        tracker.record_sync();
        tracker.record_sync();
        assert!(!tracker.can_sync(3));
    }

    #[test]
    fn test_ghost_maintenance_config_from_config() {
        let config = Config::default();
        let ghost_config = GhostMaintenanceServiceConfig::from_config(&config);
        assert!(!ghost_config.enabled);
        assert!(ghost_config.model.is_none());
        assert_eq!(ghost_config.max_syncs_per_day, 10);
        assert!(ghost_config.auto_social);
    }

    #[test]
    fn test_routine_prompt_allows_sqlite_long_term_maintenance() {
        let config = GhostMaintenanceServiceConfig {
            enabled: true,
            model: None,
            schedule: "0 0 */4 * * *".to_string(),
            max_syncs_per_day: 10,
            auto_social: false,
        };
        let prompt = GhostMaintenanceService::build_routine_prompt(&config);

        assert!(prompt.contains("memory_maintenance"));
        assert!(prompt.contains("SQLite long_term memory"));
        assert!(prompt.contains(&format!("memory_{}", "upsert")));
        assert!(prompt.contains("Do not create skills"));
    }

    #[test]
    fn test_routine_prompt_uses_workspace_relative_cleanup_paths() {
        let config = GhostMaintenanceServiceConfig {
            enabled: true,
            model: None,
            schedule: "0 0 */4 * * *".to_string(),
            max_syncs_per_day: 10,
            auto_social: false,
        };
        let prompt = GhostMaintenanceService::build_routine_prompt(&config);

        assert!(prompt.contains("list_dir media + downloads"));
        assert!(!prompt.contains("workspace/media"));
        assert!(!prompt.contains("workspace/downloads"));
    }
}
