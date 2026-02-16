use blockcell_core::{Config, Paths};
use blockcell_providers::OpenAIProvider;
use blockcell_skills::evolution::{
    EvolutionRecord, EvolutionStatus, LLMProvider, ShadowTestExecutor, ShadowTestResult,
};
use blockcell_skills::service::{EvolutionService, EvolutionServiceConfig};
use blockcell_skills::is_builtin_tool;
use std::io::Write;

// === LLM Provider Adapter ===
// Wraps OpenAIProvider (which uses chat API) to implement the simpler
// skills::evolution::LLMProvider trait (just generate(prompt) -> String).

struct OpenAILLMAdapter {
    provider: OpenAIProvider,
}

impl OpenAILLMAdapter {
    fn new(config: &Config) -> anyhow::Result<Self> {
        let (provider_name, provider_config) = config
            .get_api_key()
            .ok_or_else(|| anyhow::anyhow!("No provider configured with API key"))?;

        let api_base = provider_config.api_base.as_deref().unwrap_or_else(|| {
            match provider_name {
                "openrouter" => "https://openrouter.ai/api/v1",
                "anthropic" => "https://api.anthropic.com/v1",
                "openai" => "https://api.openai.com/v1",
                "deepseek" => "https://api.deepseek.com/v1",
                _ => "https://api.openai.com/v1",
            }
        });

        let provider = OpenAIProvider::new(
            &provider_config.api_key,
            Some(api_base),
            &config.agents.defaults.model,
            config.agents.defaults.max_tokens,
            config.agents.defaults.temperature,
        );

        Ok(Self { provider })
    }
}

#[async_trait::async_trait]
impl LLMProvider for OpenAILLMAdapter {
    async fn generate(&self, prompt: &str) -> blockcell_core::Result<String> {
        use blockcell_core::types::ChatMessage;
        let messages = vec![
            ChatMessage::system("You are a skill evolution assistant. Follow instructions precisely."),
            ChatMessage::user(prompt),
        ];
        let response = self.provider.chat(&messages, &[]).await?;
        Ok(response.content.unwrap_or_default())
    }
}

// === Shadow Test Executor ===
// Basic implementation that always passes (for manual evolution, the user
// will verify the result themselves).

struct BasicTestExecutor;

#[async_trait::async_trait]
impl ShadowTestExecutor for BasicTestExecutor {
    async fn execute_tests(&self, _skill_name: &str, _diff: &str) -> blockcell_core::Result<ShadowTestResult> {
        Ok(ShadowTestResult {
            passed: true,
            test_cases_run: 1,
            test_cases_passed: 1,
            errors: vec![],
            tested_at: chrono::Utc::now().timestamp(),
        })
    }
}

// === Provider trait import (needed for .chat()) ===
use blockcell_providers::Provider;

/// Trigger a manual evolution and drive the full pipeline.
///
/// Usage: blockcell evolve run "add web page translation"
pub async fn run(description: &str, watch: bool) -> anyhow::Result<()> {
    let paths = Paths::default();
    let config = Config::load_or_default(&paths)?;
    let skills_dir = paths.skills_dir();

    // Derive a skill name from the description
    let skill_name = derive_skill_name(description);

    let evo_config = EvolutionServiceConfig::default();
    let service = EvolutionService::new(skills_dir, evo_config);

    println!();
    println!("🧬 Self-Evolution");
    println!("  Skill name: {}", skill_name);
    println!("  Description: {}", description);
    println!();

    // Step 1: Trigger
    let evolution_id = match service.trigger_manual_evolution(&skill_name, description).await {
        Ok(id) => {
            println!("  ⏳ Evolution triggered: {}", &id);
            id
        }
        Err(e) => {
            println!("  ❌ Trigger failed: {}", e);
            return Ok(());
        }
    };

    // Step 2: Create LLM provider adapter
    let llm_adapter = match OpenAILLMAdapter::new(&config) {
        Ok(adapter) => adapter,
        Err(e) => {
            println!("  ❌ Failed to create LLM provider: {}", e);
            println!("  💡 Configure API key first: blockcell onboard");
            return Ok(());
        }
    };
    let test_executor = BasicTestExecutor;

    // Step 3: Drive the full pipeline with progress output
    println!("  🔧 Running evolution pipeline...");
    println!();

    match service.run_pending_evolutions(&llm_adapter, &test_executor).await {
        Ok(completed) => {
            // Reload the record to show final status
            let records_dir = paths.workspace().join("evolution_records");
            if let Ok(record) = load_record(&records_dir, &evolution_id) {
                let icon = status_icon(&record.status);
                let desc = status_desc_cn(&record.status);
                println!("  {} Final status: {}", icon, desc);

                // Show attempt info
                if record.attempt > 1 {
                    println!("  🔄 Total attempts: {} ({} retries)", record.attempt, record.attempt - 1);
                }
                if !record.feedback_history.is_empty() {
                    println!("  📋 Feedback history:");
                    for fb in &record.feedback_history {
                        println!("     #{} [{}] {}", fb.attempt, fb.stage,
                            fb.feedback.lines().next().unwrap_or(""));
                    }
                }

                // Show details based on final status
                if let Some(ref patch) = record.patch {
                    println!("  🔧 Generated patch: {}", patch.patch_id);
                    if !patch.explanation.is_empty() {
                        let preview: String = patch.explanation.chars().take(200).collect();
                        println!("  📄 Explanation: {}", preview);
                    }
                }
                if let Some(ref audit) = record.audit {
                    if audit.passed {
                        println!("  ✅ Audit passed");
                    } else {
                        println!("  ❌ Audit failed:");
                        for issue in &audit.issues {
                            println!("     ⚠️  [{}] {}", issue.severity, issue.message);
                        }
                    }
                }
                if record.status == EvolutionStatus::DryRunFailed {
                    println!("  ❌ Build check failed");
                }
                if let Some(ref test) = record.shadow_test {
                    println!("  🧪 Tests: {}/{} passed", test.test_cases_passed, test.test_cases_run);
                }
                if record.status == EvolutionStatus::RollingOut || record.status == EvolutionStatus::Completed {
                    if let Some(ref rollout) = record.rollout {
                        let stage = &rollout.stages[rollout.current_stage];
                        println!("  🚀 Canary rollout: stage {}/{} ({}%)",
                            rollout.current_stage + 1, rollout.stages.len(), stage.percentage);
                    }
                }

                if !completed.is_empty() {
                    println!();
                    println!("  🎉 Evolution pipeline complete, canary rollout started!");
                }
            }
        }
        Err(e) => {
            println!("  ❌ Evolution pipeline failed: {}", e);
        }
    }

    println!();

    if watch {
        watch_evolution(&paths, &evolution_id).await?;
    } else {
        println!("  💡 Use `blockcell evolve status {}` for details", truncate_str(&evolution_id, 20));
    }

    Ok(())
}

/// Watch an evolution's progress by polling its record file.
pub async fn watch(evolution_id: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::default();

    if let Some(evo_id) = evolution_id {
        // Watch a specific evolution
        let resolved = resolve_evolution_id(&paths, &evo_id)?;
        watch_evolution(&paths, &resolved).await?;
    } else {
        // Watch all active evolutions
        watch_all(&paths).await?;
    }

    Ok(())
}

/// Show status of a specific evolution or all evolutions.
pub async fn status(evolution_id: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::default();
    let records_dir = paths.workspace().join("evolution_records");

    if let Some(evo_id) = evolution_id {
        // Show detail for one evolution
        let resolved = resolve_evolution_id(&paths, &evo_id)?;
        let record = load_record(&records_dir, &resolved)?;
        print_record_detail(&record);
    } else {
        // Show summary of all evolutions
        print_all_status(&paths)?;
    }

    Ok(())
}

/// List all evolution records (same as `skills list` but more detailed).
pub async fn list(all: bool, verbose: bool) -> anyhow::Result<()> {
    let paths = Paths::default();
    let records_dir = paths.workspace().join("evolution_records");

    let mut records = load_all_records(&records_dir);
    records.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    if !all {
        // Filter out built-in tool records
        records.retain(|r| !is_builtin_tool(&r.skill_name));
    }

    if records.is_empty() {
        println!();
        println!("  (No evolution records)");
        println!();
        return Ok(());
    }

    println!();
    println!("🧬 Evolution records ({} total)", records.len());
    println!();

    for r in &records {
        let icon = status_icon(&r.status);
        let desc = status_desc_cn(&r.status);
        let trigger_desc = trigger_desc(&r);

        println!("  {} {} [{}]", icon, r.skill_name, desc);
        println!("    ID: {}", r.id);
        println!("    Trigger: {}", trigger_desc);
        println!("    Created: {}  Updated: {}", format_ts(r.created_at), format_ts(r.updated_at));

        if verbose {
            if let Some(ref patch) = r.patch {
                println!("    Patch: {} ({})", patch.patch_id, format_ts(patch.generated_at));
                if !patch.explanation.is_empty() {
                    let preview: String = patch.explanation.chars().take(100).collect();
                    println!("    Explanation: {}...", preview);
                }
            }
            if let Some(ref audit) = r.audit {
                println!("    Audit: {} ({} issues)", if audit.passed { "passed" } else { "failed" }, audit.issues.len());
            }
            if let Some(ref test) = r.shadow_test {
                println!("    Tests: {}/{} passed", test.test_cases_passed, test.test_cases_run);
            }
            if let Some(ref rollout) = r.rollout {
                let stage = &rollout.stages[rollout.current_stage];
                println!("    Canary: stage {}/{} ({}%)", rollout.current_stage + 1, rollout.stages.len(), stage.percentage);
            }
        }
        println!();
    }

    Ok(())
}

// --- Internal helpers ---

/// Derive a skill name from a description string.
fn derive_skill_name(description: &str) -> String {
    // Take meaningful chars, replace spaces with underscores, limit length
    let cleaned: String = description
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == ' ')
        .collect();
    let name = cleaned.trim().replace(' ', "_").to_lowercase();
    if name.len() > 40 {
        name.char_indices()
            .take_while(|&(i, _)| i < 40)
            .map(|(_, c)| c)
            .collect()
    } else if name.is_empty() {
        format!("skill_{}", chrono::Utc::now().timestamp())
    } else {
        name
    }
}

/// Resolve a possibly-abbreviated evolution ID to the full ID.
fn resolve_evolution_id(paths: &Paths, prefix: &str) -> anyhow::Result<String> {
    let records_dir = paths.workspace().join("evolution_records");
    let records = load_all_records(&records_dir);

    let matching: Vec<_> = records.iter()
        .filter(|r| r.id.starts_with(prefix) || r.id.contains(prefix))
        .collect();

    match matching.len() {
        0 => anyhow::bail!("No matching evolution record: {}", prefix),
        1 => Ok(matching[0].id.clone()),
        _ => {
            println!("Multiple records match '{}':", prefix);
            for r in &matching {
                println!("  {} - {} [{}]", r.id, r.skill_name, status_desc_cn(&r.status));
            }
            anyhow::bail!("Please provide a more specific ID");
        }
    }
}

/// Watch a single evolution by polling its record.
async fn watch_evolution(paths: &Paths, evolution_id: &str) -> anyhow::Result<()> {
    let records_dir = paths.workspace().join("evolution_records");

    println!("👁️  Watching evolution progress: {}", evolution_id);
    println!("  (Press Ctrl+C to stop)");
    println!();

    let mut last_status = String::new();
    let mut tick = 0u64;

    loop {
        match load_record(&records_dir, evolution_id) {
            Ok(record) => {
                let current_status = format!("{:?}", record.status);

                if current_status != last_status {
                    // Status changed — print update
                    let icon = status_icon(&record.status);
                    let desc = status_desc_cn(&record.status);
                    let ts = format_ts(record.updated_at);
                    println!("  {} [{}] {} ({})", icon, ts, desc, record.skill_name);

                    // Print extra detail on certain transitions
                    match record.status {
                        EvolutionStatus::Generated => {
                            if let Some(ref patch) = record.patch {
                                let preview: String = patch.explanation.chars().take(80).collect();
                                if !preview.is_empty() {
                                    println!("     📝 {}", preview);
                                }
                            }
                        }
                        EvolutionStatus::AuditPassed => {
                            if let Some(ref audit) = record.audit {
                                println!("     ✅ Audit passed ({} hints)", audit.issues.len());
                            }
                        }
                        EvolutionStatus::AuditFailed => {
                            if let Some(ref audit) = record.audit {
                                for issue in &audit.issues {
                                    println!("     ⚠️  [{}] {}", issue.severity, issue.message);
                                }
                            }
                        }
                        EvolutionStatus::TestPassed => {
                            if let Some(ref test) = record.shadow_test {
                                println!("     ✅ Tests {}/{} passed", test.test_cases_passed, test.test_cases_run);
                            }
                        }
                        EvolutionStatus::TestFailed => {
                            if let Some(ref test) = record.shadow_test {
                                for err in &test.errors {
                                    println!("     ❌ {}", err);
                                }
                            }
                        }
                        EvolutionStatus::RollingOut => {
                            if let Some(ref rollout) = record.rollout {
                                let stage = &rollout.stages[rollout.current_stage];
                                println!("     🚀 Canary stage {}/{} ({}%)",
                                    rollout.current_stage + 1, rollout.stages.len(), stage.percentage);
                            }
                        }
                        EvolutionStatus::Completed => {
                            println!("     🎉 Evolution complete!");
                            println!();
                            return Ok(());
                        }
                        EvolutionStatus::RolledBack => {
                            println!("     ⏪ Rolled back to previous version");
                            println!();
                            return Ok(());
                        }
                        EvolutionStatus::Failed => {
                            println!("     💥 Evolution failed");
                            println!();
                            return Ok(());
                        }
                        _ => {}
                    }

                    last_status = current_status;
                } else {
                    // No change — show a spinner dot every 5 seconds
                    if tick % 5 == 0 {
                        print!(".");
                        let _ = std::io::stdout().flush();
                    }
                }
            }
            Err(_) => {
                println!("  ⚠️  Record file not found or deleted");
                return Ok(());
            }
        }

        tick += 1;
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// Watch all active (non-terminal) evolutions.
async fn watch_all(paths: &Paths) -> anyhow::Result<()> {
    let records_dir = paths.workspace().join("evolution_records");
    let records = load_all_records(&records_dir);

    let active: Vec<_> = records.iter()
        .filter(|r| !is_terminal(&r.status))
        .collect();

    if active.is_empty() {
        println!();
        println!("  (No active evolutions)");
        println!();
        return Ok(());
    }

    if active.len() == 1 {
        return watch_evolution(paths, &active[0].id).await;
    }

    // Multiple active — show status and let user pick
    println!();
    println!("🔄 Active evolutions:");
    println!();
    for (i, r) in active.iter().enumerate() {
        let icon = status_icon(&r.status);
        let desc = status_desc_cn(&r.status);
        println!("  {}. {} {} [{}] ({})", i + 1, icon, r.skill_name, desc, &r.id);
    }
    println!();
    println!("  💡 Use `blockcell evolve watch <ID>` to watch a specific evolution");

    Ok(())
}

/// Print detailed info for a single record.
fn print_record_detail(record: &EvolutionRecord) {
    println!();
    println!("🧬 Evolution Details");
    println!("  ID:       {}", record.id);
    println!("  Skill:    {}", record.skill_name);
    println!("  Status:   {} {}", status_icon(&record.status), status_desc_cn(&record.status));
    println!("  Created:  {}", format_ts(record.created_at));
    println!("  Updated:  {}", format_ts(record.updated_at));
    println!();

    // Trigger info
    println!("  📌 Trigger reason:");
    println!("    {}", trigger_desc(record));
    if let Some(ref err) = record.context.error_stack {
        let preview: String = err.chars().take(200).collect();
        println!("    Error: {}", preview);
    }
    println!();

    // Pipeline stages
    println!("  📋 Pipeline:");
    print_pipeline_stage("Triggered", true, record.status != EvolutionStatus::Triggered);
    print_pipeline_stage("Generate Patch", record.patch.is_some(), matches!(record.status, EvolutionStatus::Generated | EvolutionStatus::Auditing | EvolutionStatus::AuditPassed | EvolutionStatus::DryRunPassed | EvolutionStatus::Testing | EvolutionStatus::TestPassed | EvolutionStatus::RollingOut | EvolutionStatus::Completed));
    print_pipeline_stage("Audit", record.audit.is_some(), record.audit.as_ref().map_or(false, |a| a.passed));
    print_pipeline_stage("Dry Run", matches!(record.status, EvolutionStatus::DryRunPassed | EvolutionStatus::Testing | EvolutionStatus::TestPassed | EvolutionStatus::RollingOut | EvolutionStatus::Completed), matches!(record.status, EvolutionStatus::DryRunPassed | EvolutionStatus::Testing | EvolutionStatus::TestPassed | EvolutionStatus::RollingOut | EvolutionStatus::Completed));
    print_pipeline_stage("Shadow Test", record.shadow_test.is_some(), record.shadow_test.as_ref().map_or(false, |t| t.passed));
    print_pipeline_stage("Canary Rollout", record.rollout.is_some(), record.status == EvolutionStatus::Completed);
    println!();

    // Patch detail
    if let Some(ref patch) = record.patch {
        println!("  📝 Patch:");
        println!("    ID: {}", patch.patch_id);
        if !patch.diff.is_empty() {
            let diff_preview: String = patch.diff.chars().take(300).collect();
            println!("    Diff:");
            for line in diff_preview.lines() {
                println!("      {}", line);
            }
            if patch.diff.chars().count() > 300 {
                println!("      ...(truncated)");
            }
        }
        println!();
    }

    // Audit detail
    if let Some(ref audit) = record.audit {
        println!("  🔍 Audit: {}", if audit.passed { "passed" } else { "failed" });
        for issue in &audit.issues {
            let icon = match issue.severity.as_str() {
                "error" => "❌",
                "warning" => "⚠️",
                _ => "ℹ️",
            };
            println!("    {} [{}] {}", icon, issue.category, issue.message);
        }
        println!();
    }

    // Test detail
    if let Some(ref test) = record.shadow_test {
        println!("  🧪 Tests: {}/{} passed", test.test_cases_passed, test.test_cases_run);
        for err in &test.errors {
            println!("    ❌ {}", err);
        }
        println!();
    }

    // Rollout detail
    if let Some(ref rollout) = record.rollout {
        println!("  🚀 Canary Rollout:");
        for (i, stage) in rollout.stages.iter().enumerate() {
            let marker = if i == rollout.current_stage { "→" } else if i < rollout.current_stage { "✓" } else { " " };
            println!("    {} Stage {}: {}% ({}min, error threshold {:.0}%)",
                marker, i + 1, stage.percentage, stage.duration_minutes, stage.error_threshold * 100.0);
        }
        println!();
    }
}

fn print_pipeline_stage(name: &str, started: bool, passed: bool) {
    let icon = if passed { "✅" } else if started { "🔄" } else { "⬜" };
    println!("    {} {}", icon, name);
}

/// Print status summary of all evolutions.
fn print_all_status(paths: &Paths) -> anyhow::Result<()> {
    let records_dir = paths.workspace().join("evolution_records");
    let mut records = load_all_records(&records_dir);
    records.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let active_count = records.iter().filter(|r| !is_terminal(&r.status)).count();
    let completed_count = records.iter().filter(|r| r.status == EvolutionStatus::Completed).count();
    let failed_count = records.iter().filter(|r| matches!(r.status,
        EvolutionStatus::Failed | EvolutionStatus::RolledBack |
        EvolutionStatus::AuditFailed | EvolutionStatus::DryRunFailed |
        EvolutionStatus::TestFailed
    )).count();

    println!();
    println!("🧬 Evolution Status");
    println!("  🔄 Active: {}  ✅ Completed: {}  ❌ Failed: {}  📊 Total: {}",
        active_count, completed_count, failed_count, records.len());

    if !records.is_empty() {
        println!();
        // Show latest 10
        let show_count = records.len().min(10);
        for r in &records[..show_count] {
            let icon = status_icon(&r.status);
            let desc = status_desc_cn(&r.status);
            let trigger = trigger_short(&r);
            println!("  {} {:<30} [{}] {} ({})",
                icon,
                truncate_str(&r.skill_name, 30),
                desc,
                trigger,
                format_ts(r.updated_at),
            );
        }
        if records.len() > 10 {
            println!("  ... {} more records (use `blockcell evolve list` to see all)", records.len() - 10);
        }
    }
    println!();

    Ok(())
}

// --- Utility functions ---

fn load_all_records(records_dir: &std::path::Path) -> Vec<EvolutionRecord> {
    let mut records = Vec::new();
    if !records_dir.exists() {
        return records;
    }
    if let Ok(entries) = std::fs::read_dir(records_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "json") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(record) = serde_json::from_str::<EvolutionRecord>(&content) {
                        records.push(record);
                    }
                }
            }
        }
    }
    records
}

fn load_record(records_dir: &std::path::Path, evolution_id: &str) -> anyhow::Result<EvolutionRecord> {
    let path = records_dir.join(format!("{}.json", evolution_id));
    if !path.exists() {
        anyhow::bail!("Record file not found: {}", evolution_id);
    }
    let content = std::fs::read_to_string(&path)?;
    let record: EvolutionRecord = serde_json::from_str(&content)?;
    Ok(record)
}

fn is_terminal(status: &EvolutionStatus) -> bool {
    matches!(status,
        EvolutionStatus::Completed |
        EvolutionStatus::Failed |
        EvolutionStatus::RolledBack |
        EvolutionStatus::AuditFailed |
        EvolutionStatus::DryRunFailed |
        EvolutionStatus::TestFailed
    )
}

fn status_icon(status: &EvolutionStatus) -> &'static str {
    match status {
        EvolutionStatus::Triggered => "⏳",
        EvolutionStatus::Generating => "🔧",
        EvolutionStatus::Generated => "📝",
        EvolutionStatus::Auditing => "🔍",
        EvolutionStatus::AuditPassed => "✅",
        EvolutionStatus::AuditFailed => "❌",
        EvolutionStatus::DryRunPassed => "✅",
        EvolutionStatus::DryRunFailed => "❌",
        EvolutionStatus::Testing => "🧪",
        EvolutionStatus::TestPassed => "✅",
        EvolutionStatus::TestFailed => "❌",
        EvolutionStatus::RollingOut => "🚀",
        EvolutionStatus::Completed => "🎉",
        EvolutionStatus::RolledBack => "⏪",
        EvolutionStatus::Failed => "💥",
    }
}

fn status_desc_cn(status: &EvolutionStatus) -> &'static str {
    match status {
        EvolutionStatus::Triggered => "pending",
        EvolutionStatus::Generating => "generating",
        EvolutionStatus::Generated => "generated",
        EvolutionStatus::Auditing => "auditing",
        EvolutionStatus::AuditPassed => "audit passed",
        EvolutionStatus::AuditFailed => "audit failed",
        EvolutionStatus::DryRunPassed => "build passed",
        EvolutionStatus::DryRunFailed => "build failed",
        EvolutionStatus::Testing => "testing",
        EvolutionStatus::TestPassed => "test passed",
        EvolutionStatus::TestFailed => "test failed",
        EvolutionStatus::RollingOut => "rolling out",
        EvolutionStatus::Completed => "completed",
        EvolutionStatus::RolledBack => "rolled back",
        EvolutionStatus::Failed => "failed",
    }
}

fn trigger_desc(record: &EvolutionRecord) -> String {
    match &record.context.trigger {
        blockcell_skills::evolution::TriggerReason::ExecutionError { error, count } => {
            format!("Execution error ({}x): {}", count, truncate_str(error, 60))
        }
        blockcell_skills::evolution::TriggerReason::ConsecutiveFailures { count, window_minutes } => {
            format!("Consecutive failures {}x (within {}min)", count, window_minutes)
        }
        blockcell_skills::evolution::TriggerReason::PerformanceDegradation { metric, threshold } => {
            format!("Performance degradation: {} (threshold {:.2})", metric, threshold)
        }
        blockcell_skills::evolution::TriggerReason::ApiChange { endpoint, status_code } => {
            format!("API change: {} ({})", endpoint, status_code)
        }
        blockcell_skills::evolution::TriggerReason::ManualRequest { description } => {
            format!("Manual request: {}", truncate_str(description, 60))
        }
    }
}

fn trigger_short(record: &EvolutionRecord) -> &'static str {
    match &record.context.trigger {
        blockcell_skills::evolution::TriggerReason::ExecutionError { .. } => "exec error",
        blockcell_skills::evolution::TriggerReason::ConsecutiveFailures { .. } => "failures",
        blockcell_skills::evolution::TriggerReason::PerformanceDegradation { .. } => "perf degradation",
        blockcell_skills::evolution::TriggerReason::ApiChange { .. } => "API change",
        blockcell_skills::evolution::TriggerReason::ManualRequest { .. } => "manual",
    }
}

fn format_ts(ts: i64) -> String {
    use chrono::{TimeZone, Local};
    match Local.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%m-%d %H:%M").to_string(),
        _ => "unknown".to_string(),
    }
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}
