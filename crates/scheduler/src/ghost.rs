use blockcell_core::{Config, InboundMessage, Paths, Result};
use chrono::Utc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Ghost Agent system prompt — a background maintenance persona.
#[allow(dead_code)]
const GHOST_SYSTEM_PROMPT: &str = r#"You are the Ghost Agent of Blockcell, a background maintenance system.
Your goal is to keep the agent healthy, organized, and socially connected.

CONSTRAINTS:
1. You run in the background. DO NOT interact with the user unless strictly necessary (use 'notification' tool for critical issues only).
2. You have RESTRICTED permissions. You cannot run shell commands or browse the web randomly.
3. Be efficient and concise — minimize token usage.

YOUR ROUTINE (execute in order):
1. Memory Gardening: Query recent daily memories. Extract important facts into long-term memory. Delete expired or trivial entries. Compress duplicates.
2. System Check: Check workspace disk usage. Look for old temporary files in workspace/media and workspace/downloads.
3. Community Sync: Call community_hub tool directly to send heartbeat and browse feed. The tool reads config automatically — if not configured, it returns an error.
4. Cleanup: Remove temporary files older than 7 days from workspace/media and workspace/downloads.

RULES:
- Use memory_maintenance tool for memory operations.
- Use community_hub tool for social interactions. Hub URL and API key are resolved by the tool internally — never try to check or guess URLs yourself.
- Use list_dir + file_ops for cleanup.
- Use notification tool ONLY for critical findings (e.g., disk full, API key expired).
- NEVER save maintenance routine logs/summaries to memory (memory_upsert). Maintenance results are ephemeral — just output them as your final text response. Only save genuinely important discoveries (e.g., user preference found during gardening) to long-term memory.
- After each routine, output a brief text summary of what you did. This summary goes to the chat log, NOT to memory.
"#;

/// Configuration for the Ghost Agent, read from config.json agents.ghost.
#[derive(Debug, Clone)]
pub struct GhostServiceConfig {
    pub enabled: bool,
    pub model: Option<String>,
    pub schedule: String,
    pub max_syncs_per_day: u32,
    pub auto_social: bool,
}

impl GhostServiceConfig {
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

pub struct GhostService {
    config: GhostServiceConfig,
    #[allow(dead_code)]
    paths: Paths,
    inbound_tx: mpsc::Sender<InboundMessage>,
    sync_tracker: SyncTracker,
}

impl GhostService {
    fn normalize_cron_schedule(expr: &str) -> String {
        let parts: Vec<&str> = expr.split_whitespace().filter(|p| !p.is_empty()).collect();
        if parts.len() == 5 {
            format!("0 {}", expr.trim())
        } else {
            expr.trim().to_string()
        }
    }

    fn parse_cron_schedule(expr: &str) -> std::result::Result<cron::Schedule, cron::error::Error> {
        let normalized = Self::normalize_cron_schedule(expr);
        normalized.parse::<cron::Schedule>()
    }

    pub fn new(
        config: GhostServiceConfig,
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

    /// Build the routine prompt based on config
    pub fn build_routine_prompt(config: &GhostServiceConfig) -> String {
        let mut prompt_parts = vec![
            "System: 执行Ghost Agent例行维护任务。请按顺序执行以下步骤：".to_string(),
            "⚠️ 重要规则：本次维护的所有日志和总结只需作为最终文本回复输出，绝对不要调用 memory_upsert 保存维护日志到记忆中。记忆系统只用于保存用户相关的重要事实。".to_string(),
            "1. 【记忆整理】调用 memory_maintenance(action=\"garden\") 整理最近的记忆。根据返回的 instruction 处理记忆条目（提取重要事实到长期记忆、删除琐碎条目），但不要把维护日志本身写入记忆。".to_string(),
            "2. 【文件清理】检查 workspace/media 和 workspace/downloads 目录，用 list_dir 列出文件，只删除**修改时间超过7天**的文件（file_ops delete）。今天的文件、近期文件一律不删。如果无法判断文件时间，跳过不删。".to_string(),
        ];

        if config.auto_social {
            prompt_parts.push(
                "3. 【社区互动】直接调用 community_hub 工具执行以下操作（连接信息由工具在系统侧内部读取，你不需要关心）：\n   3.1 调用 community_hub，参数 {\"action\": \"heartbeat\"} 上报状态。\n   3.2 调用 community_hub，参数 {\"action\": \"feed\"} 拉取社区动态。\n   3.3 互动策略（上限：like≤2，reply≤1，post≤1；宁缺毋滥）：\n       - 优先：挑 1 条有价值的帖子 reply。\n       - 次优：对最多 2 条帖子点赞。\n       - 发帖：只在 feed 没有合适回复对象时才发 1 条短帖。\n       - 禁止：广告、刷屏、泄露隐私。\n   如果工具返回错误，直接报告错误信息即可，不要自行尝试任何网络连接或猜测配置。".to_string()
            );
        }

        prompt_parts.push("完成后简要总结你做了什么（直接输出文本，不要保存到记忆）。".to_string());
        prompt_parts.join("\n")
    }

    /// Run a single ghost routine cycle.
    async fn run_routine(&mut self) -> Result<()> {
        if !self.sync_tracker.can_sync(self.config.max_syncs_per_day) {
            debug!("Ghost: daily sync limit reached ({}/{}), skipping",
                self.sync_tracker.count, self.config.max_syncs_per_day);
            return Ok(());
        }

        info!("👻 Ghost Agent: starting routine cycle");
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
            sender_id: "ghost".to_string(),
            chat_id: format!("ghost_{}", Utc::now().format("%Y%m%d_%H%M%S")),
            content,
            media: vec![],
            metadata,
            timestamp_ms: Utc::now().timestamp_millis(),
        };

        if let Err(e) = self.inbound_tx.send(msg).await {
            error!(error = %e, "Ghost: failed to send routine message");
        }

        info!("👻 Ghost Agent: routine message dispatched");
        Ok(())
    }

    /// Parse the cron schedule and run the ghost loop.
    pub async fn run_loop(mut self, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        info!(
            schedule = %self.config.schedule,
            max_syncs = self.config.max_syncs_per_day,
            auto_social = self.config.auto_social,
            enabled = self.config.enabled,
            "👻 GhostService started"
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
                "0 0 */4 * * *".parse::<cron::Schedule>().unwrap()
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

        loop {
            tokio::select! {
                _ = check_interval.tick() => {
                    // Hot-reload config
                    if let Ok(new_config) = Config::load_or_default(&config_paths) {
                        let new_ghost = GhostServiceConfig::from_config(&new_config);
                        
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
                                info!("👻 GhostService disabled via config");
                            } else {
                                info!("👻 GhostService enabled/updated via config: {}", self.config.schedule);
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
                            warn!(error = %e, "Ghost routine failed");
                        }
                    }
                }
                _ = shutdown.recv() => {
                    info!("👻 GhostService shutting down");
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
    fn test_ghost_config_from_config() {
        let config = Config::default();
        let ghost_config = GhostServiceConfig::from_config(&config);
        assert!(!ghost_config.enabled);
        assert!(ghost_config.model.is_none());
        assert_eq!(ghost_config.max_syncs_per_day, 10);
        assert!(ghost_config.auto_social);
    }
}
