use crate::auto_memory::MemoryInjector;
use crate::prompt_registry::{PromptRegistry, SystemPromptContext};
use crate::retrieval::{
    PromptBudgetAllocator, PromptSections, RetrievalOrchestrator, RetrievalSource,
};
use crate::runtime_kernel::ScopeView;
use blockcell_core::config::{MemoryRecallConfig, MemoryRecallMode};
use blockcell_core::types::ChatMessage;
use blockcell_core::{Config, Paths};
use blockcell_skills::manager::SkillSource;
use blockcell_skills::{EvolutionService, EvolutionServiceConfig, LLMProvider, SkillManager};
use blockcell_tools::MemoryStoreHandle;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

pub(crate) const MAX_AGENTS_MD_TOKENS: usize = 4_000;
pub(crate) const MAX_SOUL_MD_TOKENS: usize = 2_000;
const MAX_RETRIEVED_ITEM_TOKENS: usize = 500;
pub(crate) const MAX_ACTIVE_SKILL_TOKENS: usize = 10_000;
pub(crate) const MAX_TOOL_RULE_TOKENS: usize = 1_000;
const MAX_CACHED_SESSIONS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InteractionMode {
    Skill,
    Chat,
    Coding,
    General,
}

pub(crate) const CODING_MODE_PROMPT: &str = r#"
## Editing Discipline
- Read relevant files before editing. Make the smallest coherent diff and do not modify unrelated files.
- Preserve user changes in a dirty worktree. If an edit fails, reread the current file before retrying.

## Git Discipline
- Inspect `git status` and the relevant diff before and after changes.
- Never discard changes you did not create. Do not use `git reset --hard` or destructive checkout commands unless the user explicitly requests them.

## Verification Discipline
- After every meaningful edit, run the most relevant test, lint, or build command.
- If verification fails, diagnose and fix it before claiming completion. Report any verification that could not be run.

## Plan Discipline
- For tasks with three or more steps, call `update_plan` before implementation.
- Keep exactly one step in progress and update the plan whenever a step is completed.

## Multi-Agent Orchestration
- For complex coding tasks, use `agent(explore)` to map the code, then `agent(plan)`, and persist the accepted steps with `update_plan`.
- Dispatch independent file scopes in parallel with `agent(coder)` and an explicit non-overlapping `workspace_scope`; collect their background task results before integration.
- Finish with `agent(tester)` and `agent(reviewer)`. Route concrete review findings back to `agent(coder)` and repeat testing/review until both pass.
- Do not parallelize work that touches the same file or has unresolved ordering dependencies.
"#;

#[derive(Debug, Clone)]
pub struct ActiveSkillContext {
    pub name: String,
    pub prompt_md: String,
    pub inject_prompt_md: bool,
    pub tools: Vec<String>,
    pub fallback_message: Option<String>,
    /// 技能来源，用于运行时区分（如自进化屏蔽）
    pub source: SkillSource,
    /// 模型学习生成的技能：注入时作为 learned hint，而非系统级指令。
    pub learned: bool,
}

/// Skill 系统引导 (参考 Hermes MEMORY_GUIDANCE)
///
/// 注入到系统提示词中, 引导 Agent 正确使用 Skill 系统。
pub(crate) const SKILL_GUIDANCE: &str = r#"
## Skill System Guidance

You have a skill system for reusable procedural knowledge.

### Creating skills
After completing complex tasks (5+ tool calls, errors overcome, user-corrected approach),
offer to save the workflow as a skill. Use `skill_manage` with action="create".

### Patching skills
When using a skill and discovering issues not covered by it, patch it immediately
with `skill_manage` action="patch" — don't wait to be asked.

### Skill maintenance
Skills that aren't maintained become liabilities. Periodically review skills you use
and patch them when you find stale instructions or missing steps.
"#;

/// Memory 使用指导 — 参考 Hermes MEMORY_GUIDANCE
///
/// 注入到系统提示词中, 引导 Agent 正确使用 Memory 系统。
pub(crate) const MEMORY_GUIDANCE_BODY: &str = r#"
### Durable memory
Use memory for durable facts about the user and environment.

### What to save
- User preferences and habits (communication style, language, formatting)
- Environment facts (OS, shell, project structure, conventions)
- Important decisions and their rationale
- Recurring patterns the user has confirmed

### What NOT to save
- Task progress or temporary state (it changes and becomes stale)
- Full conversation history (already available in context)
- Information the user can easily re-derive
- Speculative or unverified assumptions

### Memory vs Skill boundary
- Memory: declarative facts (preferences, environment, conventions)
- Skill: procedural knowledge (steps, workflows, pitfalls)
- "User prefers concise responses" → memory
- "Deploy to K8s requires pushing image first" → skill
- Procedures and workflows belong in skills, not memory.
"#;

pub struct ContextBuilder {
    paths: Paths,
    skill_manager: Option<SkillManager>,
    ghost_learning_enabled: bool,
    memory_recall_enabled: bool,
    memory_recall_policy: MemoryRecallConfig,
    prompt_budget: blockcell_core::config::PromptBudgetConfig,
    session_cache: Arc<Mutex<SessionContextCache>>,
    memory_store: Option<MemoryStoreHandle>,
    /// Layer 5 记忆注入器 (7 层记忆系统)
    memory_injector: Option<MemoryInjector>,
    /// 跨消息共享的 Layer 5 记忆注入器快照，确保某条消息内懒加载或热重载后后续消息立即可见
    shared_memory_injector: Arc<RwLock<Option<MemoryInjector>>>,
    /// Cached capability brief for prompt injection (updated from tick).
    capability_brief: Option<String>,
    /// Skill 索引摘要 (可用 Skill 列表, 注入到系统提示词)
    /// 使用 Arc<RwLock> 允许后台 Review Agent 在创建/修改 Skill 后刷新
    skill_index_summary: Arc<RwLock<Option<String>>>,
    /// System prompt sections; agent / session scoped sections override global defaults.
    prompt_registry: PromptRegistry,
    /// Agent scope used when resolving prompt sections.
    agent_id: String,
    /// Evolution candidate being evaluated; candidate-scoped sections are visible only then.
    candidate_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct FrozenFileMemorySnapshot {
    user: Option<String>,
    memory: Option<String>,
}

#[derive(Debug, Clone)]
struct FrozenSystemPromptSnapshot {
    fingerprint: u64,
    prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectEnvironmentSnapshot {
    git_branch: Option<String>,
    dirty_entries: Vec<String>,
    dirty_total: usize,
    commands: Vec<String>,
}

struct SessionContextCache {
    file_memory_snapshots: HashMap<String, FrozenFileMemorySnapshot>,
    system_prompt_snapshots: HashMap<String, FrozenSystemPromptSnapshot>,
    injected_retrieval_items: HashMap<String, HashSet<u64>>,
    project_environment_snapshots: HashMap<String, ProjectEnvironmentSnapshot>,
    project_command_cache: HashMap<String, Vec<String>>,
    recency: VecDeque<String>,
    capacity: usize,
}

#[derive(Clone)]
pub(crate) struct SystemPromptSnapshotReader {
    cache: Arc<Mutex<SessionContextCache>>,
}

impl SystemPromptSnapshotReader {
    pub(crate) fn get(&self, session_key: &str) -> Option<String> {
        let cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        cache
            .system_prompt_snapshots
            .get(session_key)
            .map(|snapshot| snapshot.prompt.clone())
    }
}

impl SessionContextCache {
    fn new(capacity: usize) -> Self {
        Self {
            file_memory_snapshots: HashMap::new(),
            system_prompt_snapshots: HashMap::new(),
            injected_retrieval_items: HashMap::new(),
            project_environment_snapshots: HashMap::new(),
            project_command_cache: HashMap::new(),
            recency: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    fn touch(&mut self, session_key: &str) {
        self.recency.retain(|key| key != session_key);
        self.recency.push_back(session_key.to_string());

        while self.recency.len() > self.capacity {
            if let Some(evicted) = self.recency.pop_front() {
                self.file_memory_snapshots.remove(&evicted);
                self.system_prompt_snapshots.remove(&evicted);
                self.injected_retrieval_items.remove(&evicted);
                self.project_environment_snapshots.remove(&evicted);
                self.project_command_cache.remove(&evicted);
            }
        }
    }
}

impl FrozenFileMemorySnapshot {
    fn load(paths: &Paths) -> Self {
        Self {
            user: std::fs::read_to_string(paths.user_md()).ok(),
            memory: std::fs::read_to_string(paths.memory_md()).ok(),
        }
    }
}

impl ProjectEnvironmentSnapshot {
    fn detect(workspace: &Path, commands: Vec<String>) -> Self {
        let git_branch = git_output(workspace, &["branch", "--show-current"]).and_then(|branch| {
            if branch.is_empty() {
                git_output(workspace, &["rev-parse", "--short", "HEAD"])
                    .map(|head| format!("detached@{head}"))
            } else {
                Some(branch)
            }
        });
        let dirty = git_output(
            workspace,
            &["status", "--porcelain", "--untracked-files=normal"],
        )
        .unwrap_or_default();
        let dirty_lines = dirty.lines().map(str::to_string).collect::<Vec<_>>();
        let dirty_total = dirty_lines.len();
        let dirty_entries = dirty_lines.into_iter().take(20).collect();
        Self {
            git_branch,
            dirty_entries,
            dirty_total,
            commands,
        }
    }

    fn render(&self) -> String {
        let mut rendered = String::from("## Project Environment\n");
        rendered.push_str(&format!(
            "Git branch: {}\n",
            self.git_branch
                .as_deref()
                .unwrap_or("(not a git repository)")
        ));
        if self.dirty_entries.is_empty() {
            rendered.push_str("Dirty files: clean\n");
        } else {
            rendered.push_str("Dirty files (first 20):\n");
            for entry in &self.dirty_entries {
                rendered.push_str(entry);
                rendered.push('\n');
            }
            let omitted = self.dirty_total.saturating_sub(self.dirty_entries.len());
            if omitted > 0 {
                rendered.push_str(&format!("... {omitted} more dirty entries omitted\n"));
            }
        }
        if self.commands.is_empty() {
            rendered.push_str("Detected project commands: none\n");
        } else {
            rendered.push_str("Detected project commands:\n");
            for command in &self.commands {
                rendered.push_str(&format!("- `{command}`\n"));
            }
        }
        rendered.trim_end().to_string()
    }
}

fn git_output(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn detect_project_commands(workspace: &Path) -> Vec<String> {
    let mut commands = Vec::new();
    if workspace.join("Cargo.toml").is_file() {
        commands.push("cargo test".to_string());
        commands.push("cargo build".to_string());
    }

    let package_json = workspace.join("package.json");
    if let Ok(content) = std::fs::read_to_string(package_json) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(scripts) = value.get("scripts").and_then(serde_json::Value::as_object) {
                let runner = if workspace.join("pnpm-lock.yaml").is_file() {
                    "pnpm"
                } else if workspace.join("yarn.lock").is_file() {
                    "yarn"
                } else {
                    "npm"
                };
                for name in ["test", "lint", "build"] {
                    if scripts.contains_key(name) {
                        let command = match (runner, name) {
                            ("npm", "test") => "npm test".to_string(),
                            ("npm", name) => format!("npm run {name}"),
                            (runner, name) => format!("{runner} {name}"),
                        };
                        commands.push(command);
                    }
                }
            }
        }
    }

    if let Ok(makefile) = std::fs::read_to_string(workspace.join("Makefile")) {
        let targets = makefile
            .lines()
            .filter(|line| !line.starts_with(['\t', ' ']) && !line.starts_with('#'))
            .filter_map(|line| line.split_once(':').map(|(target, _)| target.trim()))
            .collect::<HashSet<_>>();
        if targets.contains("test") {
            commands.push("make test".to_string());
        }
        if targets.contains("build") {
            commands.push("make build".to_string());
        } else if targets.contains("all") {
            commands.push("make".to_string());
        }
    }

    commands.sort();
    commands.dedup();
    commands
}

impl ContextBuilder {
    pub fn new(paths: Paths, config: Config) -> Self {
        Self::new_with_session_cache_capacity(paths, config, MAX_CACHED_SESSIONS)
    }

    fn new_with_session_cache_capacity(paths: Paths, config: Config, capacity: usize) -> Self {
        let skills_dir = paths.skills_dir();
        let mut skill_manager = SkillManager::new()
            .with_versioning(skills_dir.clone())
            .with_evolution(
                skills_dir,
                EvolutionServiceConfig::from(config.evolution.clone()),
            );
        skill_manager.set_openclaw_skill_enabled(config.openclaw_skill_enabled);
        let _ = skill_manager.load_from_paths(&paths);

        Self {
            paths,
            skill_manager: Some(skill_manager),
            ghost_learning_enabled: config.memory.learning_system.enabled()
                && config.agents.ghost.learning.capture_enabled(),
            memory_recall_enabled: config.agents.ghost.learning.recall_enabled(),
            memory_recall_policy: config.memory.effective_memory_recall(),
            prompt_budget: config.memory.effective_prompt_budget(),
            session_cache: Arc::new(Mutex::new(SessionContextCache::new(capacity))),
            memory_store: None,
            memory_injector: None,
            shared_memory_injector: Arc::new(RwLock::new(None)),
            capability_brief: None,
            skill_index_summary: Arc::new(RwLock::new(None)),
            prompt_registry: PromptRegistry::with_builtin_sections(),
            agent_id: "default".to_string(),
            candidate_id: None,
        }
    }
}

impl Clone for ContextBuilder {
    fn clone(&self) -> Self {
        let memory_injector = self
            .shared_memory_injector
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .or_else(|| self.memory_injector.clone());
        Self {
            paths: self.paths.clone(),
            skill_manager: self.skill_manager.clone(),
            ghost_learning_enabled: self.ghost_learning_enabled,
            memory_recall_enabled: self.memory_recall_enabled,
            memory_recall_policy: self.memory_recall_policy.clone(),
            prompt_budget: self.prompt_budget.clone(),
            session_cache: Arc::clone(&self.session_cache),
            memory_store: self.memory_store.clone(),
            memory_injector,
            shared_memory_injector: Arc::clone(&self.shared_memory_injector),
            capability_brief: self.capability_brief.clone(),
            skill_index_summary: Arc::clone(&self.skill_index_summary),
            prompt_registry: self.prompt_registry.clone(),
            agent_id: self.agent_id.clone(),
            candidate_id: self.candidate_id.clone(),
        }
    }
}

impl ContextBuilder {

    pub fn prompt_registry(&self) -> &PromptRegistry {
        &self.prompt_registry
    }

    pub fn set_agent_id(&mut self, agent_id: Option<&str>) {
        self.agent_id = agent_id.unwrap_or("default").to_string();
    }

    pub fn set_candidate_scope(&mut self, candidate_id: Option<String>) {
        self.candidate_id = candidate_id;
    }

    pub(crate) fn incremental_project_environment(&self, session_key: &str) -> Option<String> {
        let cached_commands = {
            let mut cache = self
                .session_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            cache.touch(session_key);
            cache.project_command_cache.get(session_key).cloned()
        };
        let commands = cached_commands.unwrap_or_else(|| {
            let detected = detect_project_commands(&self.paths.workspace());
            let mut cache = self
                .session_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            cache.touch(session_key);
            cache
                .project_command_cache
                .entry(session_key.to_string())
                .or_insert_with(|| detected.clone())
                .clone()
        });
        let snapshot = ProjectEnvironmentSnapshot::detect(&self.paths.workspace(), commands);
        let mut cache = self
            .session_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        cache.touch(session_key);
        if cache.project_environment_snapshots.get(session_key) == Some(&snapshot) {
            return None;
        }
        let rendered = snapshot.render();
        cache
            .project_environment_snapshots
            .insert(session_key.to_string(), snapshot);
        Some(rendered)
    }

    pub(crate) fn system_prompt_snapshot_reader(&self) -> SystemPromptSnapshotReader {
        SystemPromptSnapshotReader {
            cache: self.session_cache.clone(),
        }
    }

    pub fn set_skill_manager(&mut self, manager: SkillManager) {
        self.skill_manager = Some(manager);
    }

    pub fn set_memory_store(&mut self, store: MemoryStoreHandle) {
        self.memory_store = Some(store);
    }

    /// Set the Layer 5 memory injector (7-layer memory system).
    pub fn set_memory_injector(&mut self, injector: MemoryInjector) {
        *self
            .shared_memory_injector
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(injector.clone());
        self.memory_injector = Some(injector);
    }

    /// Get the memory injector (for checking if initialized).
    pub fn memory_injector(&self) -> Option<&MemoryInjector> {
        self.memory_injector.as_ref()
    }

    /// Get the memory injector (for async loading).
    pub fn memory_injector_mut(&mut self) -> Option<&mut MemoryInjector> {
        self.memory_injector.as_mut()
    }

    /// Set the cached capability brief (called from tick or initialization).
    pub fn set_capability_brief(&mut self, brief: String) {
        if brief.is_empty() {
            self.capability_brief = None;
        } else {
            self.capability_brief = Some(brief);
        }
    }

    /// 设置 Skill 索引摘要 (可用 Skill 列表, 注入到系统提示词)
    pub fn set_skill_index_summary(&self, summary: String) {
        let mut s = self
            .skill_index_summary
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if summary.is_empty() {
            *s = None;
        } else {
            *s = Some(summary);
        }
    }

    /// 返回 skill_index_summary Arc 的克隆 (供后台任务共享)
    pub fn skill_index_summary_arc(&self) -> Arc<RwLock<Option<String>>> {
        self.skill_index_summary.clone()
    }

    /// 刷新 Skill 索引摘要 (Skill 变更后调用, 使下次 LLM 调用获取最新 Skill 列表)
    /// 使用 `&self` (内部 Arc<RwLock>) 以便后台 Review Agent 在完成后刷新
    pub fn refresh_skill_index_summary(&self) {
        let skills_dir = self.paths.skills_dir();
        let mut summary = self
            .skill_index_summary
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if !skills_dir.exists() {
            *summary = None;
        } else {
            let index = crate::skill_index::SkillIndex::build_from_dir(&skills_dir);
            *summary = if index.entries().is_empty() {
                None
            } else {
                Some(index.to_prompt_summary())
            };
        }
    }

    /// Sync available capability IDs from the registry to the SkillManager.
    /// This allows skills to validate their capability dependencies.
    pub fn sync_capabilities(&mut self, capability_ids: Vec<String>) {
        if let Some(ref mut manager) = self.skill_manager {
            manager.sync_capabilities(capability_ids);
        }
    }

    /// Get missing capabilities across all skills (for auto-triggering evolution).
    pub fn get_missing_capabilities(&self) -> Vec<(String, String)> {
        if let Some(ref manager) = self.skill_manager {
            manager.get_missing_capabilities()
        } else {
            vec![]
        }
    }

    pub fn evolution_service(&self) -> Option<&EvolutionService> {
        self.skill_manager
            .as_ref()
            .and_then(|m| m.evolution_service())
    }

    /// Wire an LLM provider into the EvolutionService so that tick() can automatically
    /// drive the full generate→audit→dry run→shadow test→rollout pipeline.
    /// Call this after the provider is created in agent startup.
    pub fn set_evolution_llm_provider(&mut self, provider: Arc<dyn LLMProvider>) {
        if let Some(ref mut manager) = self.skill_manager {
            if let Some(evo) = manager.evolution_service_mut() {
                evo.set_llm_provider(provider);
            }
        }
    }

    /// Wire a deploy callback into the EvolutionService so that successful
    /// skill deployments can trigger Ghost learning boundaries.
    pub fn set_evolution_deploy_callback(&mut self, callback: Arc<dyn Fn(&str) + Send + Sync>) {
        if let Some(ref mut manager) = self.skill_manager {
            if let Some(evo) = manager.evolution_service_mut() {
                evo.set_deploy_callback(callback);
            }
        }
    }

    /// Re-scan skill directories and pick up newly created skills.
    /// Returns the names of newly discovered skills.
    pub fn reload_skills(&mut self) -> Vec<String> {
        if let Some(ref mut manager) = self.skill_manager {
            match manager.reload_skills(&self.paths) {
                Ok(new_skills) => new_skills,
                Err(e) => {
                    tracing::warn!(error = ?e, "Failed to reload skills");
                    vec![]
                }
            }
        } else {
            vec![]
        }
    }

    /// Build system prompt with all content (legacy, no intent filtering).
    pub fn build_system_prompt(&self) -> String {
        self.build_system_prompt_for_mode_with_channel(
            InteractionMode::General,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "",
            "",
            &[],
            &[],
        )
    }

    fn frozen_file_memory_snapshot(&self, session_key: &str) -> FrozenFileMemorySnapshot {
        let mut cache = self
            .session_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.touch(session_key);
        cache
            .file_memory_snapshots
            .entry(session_key.to_string())
            .or_insert_with(|| FrozenFileMemorySnapshot::load(&self.paths))
            .clone()
    }

    #[allow(clippy::too_many_arguments)]
    fn frozen_system_prompt(
        &self,
        session_key: &str,
        mode: InteractionMode,
        active_skill: Option<&ActiveSkillContext>,
        available_tool_names: &[String],
        tool_prompt_rules: &[String],
    ) -> String {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        mode.hash(&mut hasher);
        available_tool_names.hash(&mut hasher);
        tool_prompt_rules.hash(&mut hasher);
        self.capability_brief.hash(&mut hasher);
        self.memory_store.is_some().hash(&mut hasher);
        self.prompt_registry.fingerprint().hash(&mut hasher);
        self.agent_id.hash(&mut hasher);
        self.candidate_id.hash(&mut hasher);
        if let Some(skill) = active_skill {
            skill.name.hash(&mut hasher);
            skill.prompt_md.hash(&mut hasher);
            skill.inject_prompt_md.hash(&mut hasher);
            skill.fallback_message.hash(&mut hasher);
        }
        let fingerprint = hasher.finish();
        let mut cache = self
            .session_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.touch(session_key);
        if let Some(snapshot) = cache.system_prompt_snapshots.get(session_key) {
            if snapshot.fingerprint == fingerprint {
                return snapshot.prompt.clone();
            }
        }
        drop(cache);

        let prompt = self.build_system_prompt_inner(
            mode,
            active_skill,
            available_tool_names,
            tool_prompt_rules,
            Some(session_key),
        );
        let mut cache = self
            .session_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.touch(session_key);
        cache.system_prompt_snapshots.insert(
            session_key.to_string(),
            FrozenSystemPromptSnapshot {
                fingerprint,
                prompt: prompt.clone(),
            },
        );
        prompt
    }

    fn filter_incremental_retrieval(
        &self,
        session_key: Option<&str>,
        mut items: Vec<crate::retrieval::RetrievedItem>,
    ) -> (Vec<crate::retrieval::RetrievedItem>, bool) {
        let Some(session_key) = session_key else {
            return (items, false);
        };
        let mut cache = self
            .session_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.touch(session_key);
        let seen = cache
            .injected_retrieval_items
            .entry(session_key.to_string())
            .or_default();
        let original_len = items.len();
        items.retain(|item| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            item.source.hash(&mut hasher);
            item.content.hash(&mut hasher);
            seen.insert(hasher.finish())
        });
        let skipped_previous = items.len() < original_len;
        (items, skipped_previous)
    }

    pub(crate) fn clear_injected_retrieval_for_session(&self, session_key: &str) {
        self.session_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .injected_retrieval_items
            .remove(session_key);
    }

    pub fn resolve_active_skill(
        &self,
        user_input: &str,
        disabled_skills: &HashSet<String>,
    ) -> Option<ActiveSkillContext> {
        let skill_name = user_input.trim();
        if skill_name.is_empty() {
            return None;
        }
        self.resolve_active_skill_by_name(skill_name, disabled_skills)
    }

    pub fn resolve_active_skill_by_name(
        &self,
        skill_name: &str,
        disabled_skills: &HashSet<String>,
    ) -> Option<ActiveSkillContext> {
        if skill_name.is_empty() {
            return None;
        }
        if disabled_skills.contains(skill_name) {
            return None;
        }
        let manager = self.skill_manager.as_ref()?;
        let skill = manager.get(skill_name)?;
        if !skill.available {
            return None;
        }
        let prompt_md = skill.load_prompt_bundle()?;
        Some(ActiveSkillContext {
            name: skill.name.clone(),
            prompt_md,
            inject_prompt_md: true,
            tools: skill.meta.effective_tools(),
            fallback_message: skill
                .meta
                .fallback
                .as_ref()
                .and_then(|fallback| fallback.message.clone()),
            source: skill.meta.source.clone(),
            learned: skill.meta.learned,
        })
    }

    pub fn skill_manager(&self) -> Option<&SkillManager> {
        self.skill_manager.as_ref()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_system_prompt_for_mode_with_channel(
        &self,
        mode: InteractionMode,
        active_skill: Option<&ActiveSkillContext>,
        disabled_skills: &HashSet<String>,
        disabled_tools: &HashSet<String>,
        channel: &str,
        user_query: &str,
        available_tool_names: &[String],
        tool_prompt_rules: &[String],
    ) -> String {
        self.build_system_prompt_for_mode_with_channel_and_session(
            mode,
            active_skill,
            disabled_skills,
            disabled_tools,
            channel,
            user_query,
            available_tool_names,
            tool_prompt_rules,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_system_prompt_for_mode_with_channel_and_session(
        &self,
        mode: InteractionMode,
        active_skill: Option<&ActiveSkillContext>,
        _disabled_skills: &HashSet<String>,
        _disabled_tools: &HashSet<String>,
        _channel: &str,
        _user_query: &str,
        available_tool_names: &[String],
        tool_prompt_rules: &[String],
        session_key: Option<&str>,
    ) -> String {
        self.build_system_prompt_inner(
            mode,
            active_skill,
            available_tool_names,
            tool_prompt_rules,
            session_key,
        )
    }

    fn build_system_prompt_inner(
        &self,
        mode: InteractionMode,
        active_skill: Option<&ActiveSkillContext>,
        available_tool_names: &[String],
        tool_prompt_rules: &[String],
        session_key: Option<&str>,
    ) -> String {
        let ctx = SystemPromptContext {
            mode,
            active_skill,
            available_tool_names,
            tool_prompt_rules,
            paths: &self.paths,
            ghost_learning_enabled: self.ghost_learning_enabled,
            has_memory_store: self.memory_store.is_some(),
            capability_brief: self.capability_brief.as_deref(),
        };
        let rendered = self.prompt_registry.render(
            ScopeView {
                agent_id: &self.agent_id,
                session_id: session_key.unwrap_or_default(),
                candidate_id: self.candidate_id.as_deref(),
            },
            &ctx,
        );
        for issue in &rendered.issues {
            tracing::warn!(issue = %issue, "System prompt integrity issue");
        }
        PromptBudgetAllocator::new(self.prompt_budget.clone()).assemble(PromptSections {
            rules: rendered.rules,
            user_profile: String::new(),
            retrieved: String::new(),
            active_skill: rendered.active_skill,
            session_recovery: String::new(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build_runtime_context(
        &self,
        mode: InteractionMode,
        disabled_skills: &HashSet<String>,
        disabled_tools: &HashSet<String>,
        channel: &str,
        user_query: &str,
        session_key: Option<&str>,
        memory_snapshot: Option<&FrozenFileMemorySnapshot>,
    ) -> String {
        let now = chrono::Utc::now();
        let local_time = chrono::Local::now();
        let mut runtime_rules = format!(
            "Current time: {} ({} UTC)\nWorkspace: {}\n",
            local_time.format("%Y-%m-%d %H:%M"),
            now.format("%Y-%m-%d %H:%M"),
            self.paths.workspace().display()
        );

        if let Some(plan) = session_key.and_then(blockcell_tools::plan::take_changed_plan_context) {
            runtime_rules.push('\n');
            runtime_rules.push_str(&plan);
            runtime_rules.push('\n');
        }

        if let Some(environment) =
            session_key.and_then(|key| self.incremental_project_environment(key))
        {
            runtime_rules.push('\n');
            runtime_rules.push_str(&environment);
            runtime_rules.push('\n');
        }

        if !disabled_skills.is_empty() || !disabled_tools.is_empty() {
            runtime_rules.push_str("\n## ⚠️ Disabled Items\n");
            runtime_rules
                .push_str("The following items have been disabled by the user via toggle.\n");
            runtime_rules.push_str("IMPORTANT: When user asks to 打开/开启/启用/enable any of these, call `toggle_manage` with action='set', the matching category and name, and enabled=true.\n");
            if !disabled_skills.is_empty() {
                let mut names = disabled_skills
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                names.sort_unstable();
                runtime_rules.push_str(&format!("Disabled skills: {}\n", names.join(", ")));
            }
            if !disabled_tools.is_empty() {
                let mut names = disabled_tools
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                names.sort_unstable();
                runtime_rules.push_str(&format!("Disabled tools: {}\n", names.join(", ")));
            }
        }

        let mut user_profile_context = String::new();
        let mut retrieved_context = String::new();
        let recall_mode = match mode {
            InteractionMode::Chat => MemoryRecallMode::Chat,
            InteractionMode::General | InteractionMode::Coding => MemoryRecallMode::General,
            InteractionMode::Skill => MemoryRecallMode::Skill,
        };
        if self.memory_recall_enabled
            && self.memory_recall_policy.allows(recall_mode, channel)
            && !user_query.is_empty()
        {
            let skill_summary = self
                .skill_index_summary
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            let snapshot = memory_snapshot
                .map(|snapshot| (snapshot.user.as_deref(), snapshot.memory.as_deref()));
            if let Ok(items) = RetrievalOrchestrator::retrieve_with_snapshot(
                &self.paths,
                self.memory_store.as_ref(),
                session_key,
                user_query,
                skill_summary.as_deref(),
                snapshot,
                20,
            ) {
                let (items, skipped_previous) =
                    self.filter_incremental_retrieval(session_key, items);
                if skipped_previous {
                    runtime_rules.push_str(
                        "\nPreviously provided memory remains valid; only new or changed memory is included below.\n",
                    );
                }
                let (profile, retrieved): (Vec<_>, Vec<_>) = items
                    .into_iter()
                    .partition(|item| item.source == RetrievalSource::UserProfile);
                user_profile_context = RetrievalOrchestrator::render_with_item_cap(
                    &profile,
                    MAX_RETRIEVED_ITEM_TOKENS,
                );
                retrieved_context = RetrievalOrchestrator::render_with_item_cap(
                    &retrieved,
                    MAX_RETRIEVED_ITEM_TOKENS,
                );
            }
        }

        let rendered =
            PromptBudgetAllocator::new(self.prompt_budget.clone()).assemble(PromptSections {
                rules: runtime_rules,
                user_profile: user_profile_context,
                retrieved: retrieved_context,
                active_skill: String::new(),
                session_recovery: String::new(),
            });
        format!("<runtime-context>\n{}</runtime-context>", rendered.trim())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_messages_for_mode_with_channel(
        &self,
        history: &[ChatMessage],
        user_content: &str,
        media: &[String],
        mode: InteractionMode,
        active_skill: Option<&ActiveSkillContext>,
        disabled_skills: &HashSet<String>,
        disabled_tools: &HashSet<String>,
        channel: &str,
        pending_intent: bool,
        available_tool_names: &[String],
        tool_prompt_rules: &[String],
    ) -> Vec<ChatMessage> {
        let mut messages = Vec::new();

        let system_prompt = self.build_system_prompt_for_mode_with_channel(
            mode,
            active_skill,
            disabled_skills,
            disabled_tools,
            channel,
            user_content,
            available_tool_names,
            tool_prompt_rules,
        );
        messages.push(ChatMessage::system(&system_prompt));
        self.append_history_and_user_message(
            &mut messages,
            history,
            user_content,
            media,
            pending_intent,
        );
        let runtime_context = self.build_runtime_context(
            mode,
            disabled_skills,
            disabled_tools,
            channel,
            user_content,
            None,
            None,
        );
        Self::insert_ephemeral_context(&mut messages, &runtime_context);
        messages
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_messages_for_session_mode_with_channel(
        &self,
        session_key: &str,
        history: &[ChatMessage],
        user_content: &str,
        media: &[String],
        mode: InteractionMode,
        active_skill: Option<&ActiveSkillContext>,
        disabled_skills: &HashSet<String>,
        disabled_tools: &HashSet<String>,
        channel: &str,
        pending_intent: bool,
        available_tool_names: &[String],
        tool_prompt_rules: &[String],
    ) -> Vec<ChatMessage> {
        let mut messages = Vec::new();
        let memory_snapshot = self.frozen_file_memory_snapshot(session_key);
        let system_prompt = self.frozen_system_prompt(
            session_key,
            mode,
            active_skill,
            available_tool_names,
            tool_prompt_rules,
        );
        messages.push(ChatMessage::system(&system_prompt));
        self.append_history_and_user_message(
            &mut messages,
            history,
            user_content,
            media,
            pending_intent,
        );
        let runtime_context = self.build_runtime_context(
            mode,
            disabled_skills,
            disabled_tools,
            channel,
            user_content,
            Some(session_key),
            Some(&memory_snapshot),
        );
        Self::insert_ephemeral_context(&mut messages, &runtime_context);
        messages
    }

    fn insert_ephemeral_context(messages: &mut Vec<ChatMessage>, content: &str) {
        if content.trim().is_empty() {
            return;
        }
        let insert_at = messages
            .iter()
            .rposition(|message| message.role == "user")
            .unwrap_or(messages.len());
        messages.insert(insert_at, ChatMessage::user(content));
    }

    fn append_history_and_user_message(
        &self,
        messages: &mut Vec<ChatMessage>,
        history: &[ChatMessage],
        user_content: &str,
        media: &[String],
        pending_intent: bool,
    ) {
        let user_msg = if media.is_empty() {
            ChatMessage::user(user_content)
        } else {
            let all_paths: Vec<&str> = media
                .iter()
                .filter(|p| !p.is_empty())
                .map(|p| p.as_str())
                .collect();
            let text_with_paths = if all_paths.is_empty() {
                user_content.to_string()
            } else {
                let paths_str = all_paths
                    .iter()
                    .map(|p| format!("- `{}`", p))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "{}\n\n[附件本地路径（发回给用户时请用此路径）]\n{}",
                    user_content, paths_str
                )
            };
            if pending_intent {
                ChatMessage::user(&text_with_paths)
            } else {
                self.build_multimodal_message(&text_with_paths, media)
            }
        };

        let safe_start = Self::find_safe_history_start(history);
        for msg in &history[safe_start..] {
            messages.push(msg.clone());
        }
        messages.push(user_msg);
    }

    fn build_multimodal_message(&self, text: &str, media: &[String]) -> ChatMessage {
        let mut content_parts = Vec::new();

        // Add media (images as base64)
        for media_path in media {
            if let Some(image_content) = self.encode_image_to_base64(media_path) {
                content_parts.push(serde_json::json!({
                    "type": "image_url",
                    "image_url": {
                        "url": image_content
                    }
                }));
            }
        }

        // Add text
        if !text.is_empty() {
            content_parts.push(serde_json::json!({
                "type": "text",
                "text": text
            }));
        }

        ChatMessage {
            id: None,
            role: "user".to_string(),
            content: serde_json::Value::Array(content_parts),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    fn _is_image_path(path: &str) -> bool {
        let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
        matches!(
            ext.as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "svg" | "tiff" | "ico"
        )
    }

    fn encode_image_to_base64(&self, path: &str) -> Option<String> {
        use base64::Engine;
        use std::path::Path;

        let path = Path::new(path);
        if !path.exists() {
            return None;
        }

        // Check if it's an image file
        let ext = path.extension()?.to_str()?.to_lowercase();
        let mime_type = match ext.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => return None, // Not an image
        };

        // Read and encode
        let bytes = std::fs::read(path).ok()?;
        let base64_str = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Some(format!("data:{};base64,{}", mime_type, base64_str))
    }

    /// Find a safe starting index in truncated history to avoid orphaned tool messages.
    ///
    /// After truncation, the history might start with:
    /// - A "tool" message whose tool_call_id references an assistant message that was cut off
    /// - An "assistant" message with tool_calls but missing subsequent tool responses
    ///
    /// Both cases cause LLM API 400 errors ("tool_call_id not found").
    /// This function skips forward until we find a clean starting point.
    fn find_safe_history_start(history: &[ChatMessage]) -> usize {
        if history.is_empty() {
            return 0;
        }

        let mut i = 0;

        // Skip leading "tool" role messages — they reference tool_calls from a missing assistant message
        while i < history.len() && history[i].role == "tool" {
            i += 1;
        }

        // If we land on an "assistant" message with tool_calls, check that ALL its
        // tool responses are present in the subsequent messages
        while i < history.len() {
            if history[i].role == "assistant" {
                if let Some(ref tool_calls) = history[i].tool_calls {
                    if !tool_calls.is_empty() {
                        // Collect expected tool_call_ids
                        let expected_ids: Vec<&str> =
                            tool_calls.iter().map(|tc| tc.id.as_str()).collect();

                        // Check that all expected tool responses follow
                        let mut found_ids = std::collections::HashSet::new();
                        for msg in history.iter().skip(i + 1) {
                            if msg.role == "tool" {
                                if let Some(ref id) = msg.tool_call_id {
                                    found_ids.insert(id.as_str());
                                }
                            } else {
                                break; // Stop at first non-tool message
                            }
                        }

                        let all_present = expected_ids.iter().all(|id| found_ids.contains(id));
                        if !all_present {
                            // Skip this assistant + its partial tool responses
                            i += 1;
                            while i < history.len() && history[i].role == "tool" {
                                i += 1;
                            }
                            continue;
                        }
                    }
                }
            }
            break;
        }

        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockcell_core::Result;
    use serde_json::{json, Value};
    use std::sync::Arc;

    fn test_chat_message_text(msg: &ChatMessage) -> String {
        match &msg.content {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        }
    }

    struct EmptyMemoryStore;

    impl blockcell_tools::MemoryStoreOps for EmptyMemoryStore {
        fn upsert_json(&self, _params_json: Value) -> Result<Value> {
            Ok(json!({}))
        }
        fn query_json(&self, _params_json: Value) -> Result<Value> {
            Ok(json!([]))
        }
        fn soft_delete(&self, _id: &str) -> Result<bool> {
            Ok(false)
        }
        fn batch_soft_delete_json(&self, _params_json: Value) -> Result<usize> {
            Ok(0)
        }
        fn restore(&self, _id: &str) -> Result<bool> {
            Ok(false)
        }
        fn stats_json(&self) -> Result<Value> {
            Ok(json!({}))
        }
        fn generate_brief(&self, _long_term_max: usize, _short_term_max: usize) -> Result<String> {
            Ok(String::new())
        }
        fn generate_brief_for_query(&self, _query: &str, _max_items: usize) -> Result<String> {
            Ok(String::new())
        }
        fn upsert_session_summary(&self, _session_key: &str, _summary: &str) -> Result<()> {
            Ok(())
        }
        fn get_session_summary(&self, _session_key: &str) -> Result<Option<String>> {
            Ok(None)
        }
        fn maintenance(&self, _recycle_days: i64) -> Result<(usize, usize)> {
            Ok((0, 0))
        }
    }

    struct SessionCapturingMemoryStore {
        seen_session: std::sync::Mutex<Option<String>>,
    }

    struct BriefMemoryStore;

    impl blockcell_tools::MemoryStoreOps for BriefMemoryStore {
        fn upsert_json(&self, _params_json: Value) -> Result<Value> {
            Ok(json!({}))
        }
        fn query_json(&self, _params_json: Value) -> Result<Value> {
            Ok(json!([]))
        }
        fn soft_delete(&self, _id: &str) -> Result<bool> {
            Ok(false)
        }
        fn batch_soft_delete_json(&self, _params_json: Value) -> Result<usize> {
            Ok(0)
        }
        fn restore(&self, _id: &str) -> Result<bool> {
            Ok(false)
        }
        fn stats_json(&self) -> Result<Value> {
            Ok(json!({}))
        }
        fn generate_brief(&self, _long_term_max: usize, _short_term_max: usize) -> Result<String> {
            Ok("Shadow SQLite memory brief.".to_string())
        }
        fn generate_brief_for_query(&self, _query: &str, _max_items: usize) -> Result<String> {
            Ok("Shadow SQLite memory brief.".to_string())
        }
        fn upsert_session_summary(&self, _session_key: &str, _summary: &str) -> Result<()> {
            Ok(())
        }
        fn get_session_summary(&self, _session_key: &str) -> Result<Option<String>> {
            Ok(None)
        }
        fn maintenance(&self, _recycle_days: i64) -> Result<(usize, usize)> {
            Ok((0, 0))
        }
    }

    impl blockcell_tools::MemoryStoreOps for SessionCapturingMemoryStore {
        fn upsert_json(&self, _params_json: Value) -> Result<Value> {
            Ok(json!({}))
        }
        fn query_json(&self, params_json: Value) -> Result<Value> {
            *self.seen_session.lock().unwrap() = params_json
                .get("session_key")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            Ok(json!([]))
        }
        fn soft_delete(&self, _id: &str) -> Result<bool> {
            Ok(false)
        }
        fn batch_soft_delete_json(&self, _params_json: Value) -> Result<usize> {
            Ok(0)
        }
        fn restore(&self, _id: &str) -> Result<bool> {
            Ok(false)
        }
        fn stats_json(&self) -> Result<Value> {
            Ok(json!({}))
        }
        fn generate_brief(&self, _long_term_max: usize, _short_term_max: usize) -> Result<String> {
            Ok(String::new())
        }
        fn generate_brief_for_query(&self, _query: &str, _max_items: usize) -> Result<String> {
            Ok(String::new())
        }
        fn generate_brief_for_query_in_session(
            &self,
            session_key: &str,
            _query: &str,
            _max_items: usize,
        ) -> Result<String> {
            *self.seen_session.lock().unwrap() = Some(session_key.to_string());
            Ok(String::new())
        }
        fn upsert_session_summary(&self, _session_key: &str, _summary: &str) -> Result<()> {
            Ok(())
        }
        fn get_session_summary(&self, _session_key: &str) -> Result<Option<String>> {
            Ok(None)
        }
        fn maintenance(&self, _recycle_days: i64) -> Result<(usize, usize)> {
            Ok((0, 0))
        }
    }

    #[test]
    fn retrieval_receives_current_session_key() {
        let base = std::env::temp_dir().join(format!(
            "blockcell-context-session-memory-test-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = Paths::with_base(base);
        paths.ensure_dirs().expect("ensure dirs");
        let store = Arc::new(SessionCapturingMemoryStore {
            seen_session: std::sync::Mutex::new(None),
        });
        let mut config = Config::default();
        config.agents.ghost.learning.recall_enabled = Some(true);
        let mut builder = ContextBuilder::new(paths, config);
        builder.set_memory_store(store.clone());

        builder.build_messages_for_session_mode_with_channel(
            "ws:account:3:opschat-a",
            &[],
            "remember language",
            &[],
            InteractionMode::General,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "ws",
            false,
            &[],
            &[],
        );

        assert_eq!(
            store.seen_session.lock().unwrap().as_deref(),
            Some("ws:account:3:opschat-a")
        );
    }

    #[tokio::test]
    async fn shadow_mode_excludes_sqlite_brief_and_layer5_memory_from_prompt() {
        let base = std::env::temp_dir().join(format!(
            "blockcell-context-shadow-all-memory-test-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = Paths::with_base(base);
        paths.ensure_dirs().expect("ensure dirs");
        std::fs::write(
            paths.memory_md(),
            "## Project\n\nShadow Layer5 project memory.",
        )
        .expect("write canonical project memory");

        let mut injector = crate::auto_memory::MemoryInjector::default_injector();
        injector
            .load_canonical(&paths)
            .await
            .expect("load canonical memories");

        let mut builder = ContextBuilder::new(paths, Config::default());
        builder.set_memory_store(Arc::new(BriefMemoryStore));
        builder.set_memory_injector(injector);
        let prompt = builder.build_system_prompt_for_mode_with_channel(
            InteractionMode::General,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            "shadow prompt",
            &[],
            &[],
        );

        assert!(!prompt.contains("Shadow SQLite memory brief."));
        assert!(!prompt.contains("Shadow Layer5 project memory."));
    }
    use std::fs;

    #[test]
    fn test_resolve_active_skill_by_name_keeps_manual_injection_for_script_skill() {
        let base =
            std::env::temp_dir().join(format!("blockcell-context-test-{}", uuid::Uuid::new_v4()));
        let paths = Paths::with_base(base);
        let skill_dir = paths.skills_dir().join("structured_demo");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join("meta.yaml"),
            r#"
name: structured_demo
description: structured demo
"#,
        )
        .expect("write meta");
        fs::write(skill_dir.join("SKILL.md"), "structured skill manual").expect("write skill md");
        fs::write(skill_dir.join("SKILL.py"), "print('ok')").expect("write skill py");

        let builder = ContextBuilder::new(paths, Config::default());

        let ctx = builder
            .resolve_active_skill_by_name("structured_demo", &HashSet::new())
            .expect("active skill should resolve");

        assert!(ctx.inject_prompt_md);
    }

    #[test]
    fn test_resolve_active_skill_by_name_uses_prompt_bundle_not_root_skill_md() {
        let base =
            std::env::temp_dir().join(format!("blockcell-context-test-{}", uuid::Uuid::new_v4()));
        let paths = Paths::with_base(base);
        let skill_dir = paths.skills_dir().join("prompt_demo");
        fs::create_dir_all(skill_dir.join("manual")).expect("create manual dir");
        fs::write(
            skill_dir.join("meta.yaml"),
            r#"
name: prompt_demo
description: prompt demo
"#,
        )
        .expect("write meta");
        fs::write(
            skill_dir.join("SKILL.md"),
            r#"# Prompt Demo

## Shared {#shared}
Shared rule.

## Prompt {#prompt}
- [Prompt details](manual/prompt.md#details)

## Planning {#planning}
Planning-only rule.
"#,
        )
        .expect("write skill md");
        fs::write(
            skill_dir.join("manual/prompt.md"),
            r#"## Prompt details {#details}
Prompt-only rule.
"#,
        )
        .expect("write prompt child md");

        let builder = ContextBuilder::new(paths, Config::default());

        let ctx = builder
            .resolve_active_skill_by_name("prompt_demo", &HashSet::new())
            .expect("active skill should resolve");

        assert!(ctx.inject_prompt_md);
        assert!(ctx.prompt_md.contains("Shared rule."));
        assert!(ctx.prompt_md.contains("Prompt-only rule."));
        assert!(!ctx.prompt_md.contains("Planning-only rule."));
    }

    #[test]
    fn test_resolve_active_skill_does_not_match_free_text_without_explicit_name() {
        let base =
            std::env::temp_dir().join(format!("blockcell-context-test-{}", uuid::Uuid::new_v4()));
        let paths = Paths::with_base(base);
        let skill_dir = paths.skills_dir().join("deploy_demo");
        fs::create_dir_all(&skill_dir).expect("create skill dir");
        fs::write(
            skill_dir.join("meta.yaml"),
            r#"
name: deploy_demo
description: deploy demo
"#,
        )
        .expect("write meta");
        fs::write(skill_dir.join("SKILL.md"), "deploy manual").expect("write skill md");

        let builder = ContextBuilder::new(paths, Config::default());

        assert!(builder
            .resolve_active_skill("please deploy the release", &HashSet::new())
            .is_none());
        assert_eq!(
            builder
                .resolve_active_skill("deploy_demo", &HashSet::new())
                .map(|ctx| ctx.name),
            Some("deploy_demo".to_string())
        );
    }

    #[test]
    fn test_build_system_prompt_skips_skill_md_when_prompt_injection_disabled() {
        let builder = ContextBuilder::new(
            Paths::with_base(
                std::env::temp_dir()
                    .join(format!("blockcell-context-test-{}", uuid::Uuid::new_v4())),
            ),
            Config::default(),
        );
        let active_skill = ActiveSkillContext {
            name: "structured_demo".to_string(),
            prompt_md: "DO NOT INCLUDE".to_string(),
            inject_prompt_md: false,
            tools: vec!["finance_api".to_string()],
            fallback_message: Some("fallback".to_string()),
            source: blockcell_skills::manager::SkillSource::BlockCell,
            learned: false,
        };

        let prompt = builder.build_system_prompt_for_mode_with_channel(
            InteractionMode::Skill,
            Some(&active_skill),
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            "",
            &[],
            &[],
        );

        assert!(prompt.contains("## Active Skill: structured_demo"));
        assert!(!prompt.contains("DO NOT INCLUDE"));
        assert!(prompt.contains("fallback"));
    }

    #[test]
    fn test_build_messages_does_not_inject_followup_resolution_hint() {
        let builder = ContextBuilder::new(
            Paths::with_base(
                std::env::temp_dir()
                    .join(format!("blockcell-context-test-{}", uuid::Uuid::new_v4())),
            ),
            Config::default(),
        );
        let messages = builder.build_messages_for_mode_with_channel(
            &[],
            "查看 .env 的内容",
            &[],
            InteractionMode::General,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "ws",
            false,
            &["read_file".to_string()],
            &[],
        );

        let last = messages.last().expect("user message");
        let content = last.content.as_str().expect("string user content");
        assert!(content.contains("查看 .env 的内容"));
        assert!(!content.contains("[Follow-up Reference]"));
        assert!(!content.contains("/Users/apple/.blockcell/.env"));
    }
    #[test]
    fn test_build_system_prompt_injects_file_memory_when_recall_enabled() {
        let base =
            std::env::temp_dir().join(format!("blockcell-context-test-{}", uuid::Uuid::new_v4()));
        let paths = Paths::with_base(base);
        paths.ensure_dirs().expect("ensure dirs");
        std::fs::write(
            paths.memory_md(),
            "Project fact: release verification starts with rollback planning.",
        )
        .expect("write memory md");
        let mut config = Config::default();
        config.agents.ghost.learning.recall_enabled = Some(true);
        let mut builder = ContextBuilder::new(paths, config);
        builder.set_memory_store(Arc::new(EmptyMemoryStore));

        let messages = builder.build_messages_for_session_mode_with_channel(
            "cli:file-memory",
            &[],
            "release verification",
            &[],
            InteractionMode::General,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            false,
            &[],
            &[],
        );
        let prompt = test_chat_message_text(&messages[messages.len() - 2]);

        assert!(prompt.contains("<retrieved-context>"));
        assert!(prompt.contains("[knowledge]"));
        assert!(prompt.contains("release verification starts with rollback planning"));
    }

    #[test]
    fn retrieved_context_is_single_source_tagged_and_deduplicated() {
        let base = std::env::temp_dir().join(format!(
            "blockcell-context-retrieval-test-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = Paths::with_base(base);
        paths.ensure_dirs().expect("ensure dirs");
        let fact = "User prefers concise replies for code changes.";
        std::fs::write(paths.user_md(), fact).expect("write user md");
        std::fs::write(paths.memory_md(), fact).expect("write memory md");
        let mut config = Config::default();
        config.agents.ghost.learning.recall_enabled = Some(true);
        let builder = ContextBuilder::new(paths, config);

        let messages = builder.build_messages_for_session_mode_with_channel(
            "cli:deduplicated-retrieval",
            &[],
            "concise code replies",
            &[],
            InteractionMode::General,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            false,
            &[],
            &[],
        );
        let prompt = test_chat_message_text(&messages[messages.len() - 2]);

        assert!(prompt.contains("<retrieved-context>"));
        assert!(prompt.contains("[user-profile]"));
        assert_eq!(prompt.matches(fact).count(), 1);
        assert!(!prompt.contains("## Durable File Memory"));
        assert!(!prompt.contains("## Memory Brief (SQLite FTS5 Search)"));
    }

    #[test]
    fn retrieved_context_is_incremental_within_each_session() {
        let base = std::env::temp_dir().join(format!(
            "blockcell-context-incremental-retrieval-test-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = Paths::with_base(base);
        paths.ensure_dirs().expect("ensure dirs");
        let fact = "Release verification requires a canary-first rollout.";
        std::fs::write(paths.memory_md(), fact).expect("write memory");
        let mut config = Config::default();
        config.agents.ghost.learning.recall_enabled = Some(true);
        let builder = ContextBuilder::new(paths, config);

        let build = |session_key: &str| {
            builder.build_messages_for_session_mode_with_channel(
                session_key,
                &[],
                "release verification canary",
                &[],
                InteractionMode::General,
                None,
                &HashSet::new(),
                &HashSet::new(),
                "cli",
                false,
                &[],
                &[],
            )
        };

        let first = build("cli:incremental-a");
        let repeated = build("cli:incremental-a");
        let other_session = build("cli:incremental-b");
        let first_context = test_chat_message_text(&first[first.len() - 2]);
        let repeated_context = test_chat_message_text(&repeated[repeated.len() - 2]);
        let other_context = test_chat_message_text(&other_session[other_session.len() - 2]);

        assert!(first_context.contains(fact));
        assert!(!repeated_context.contains(fact));
        assert!(repeated_context.contains("Previously provided memory remains valid"));
        assert!(other_context.contains(fact));
    }

    #[test]
    fn changed_plan_is_incremental_in_runtime_context() {
        let session_key = format!("cli:plan-context-{}", uuid::Uuid::new_v4());
        blockcell_tools::plan::replace_plan_for_session(
            &session_key,
            &serde_json::json!({"plan": [
                {"step": "inspect", "status": "completed"},
                {"step": "implement", "status": "in_progress"}
            ]}),
        )
        .expect("store plan");
        let builder = ContextBuilder::new(
            Paths::with_base(std::env::temp_dir().join(format!(
                "blockcell-plan-context-test-{}",
                uuid::Uuid::new_v4()
            ))),
            Config::default(),
        );
        let build = || {
            builder.build_messages_for_session_mode_with_channel(
                &session_key,
                &[],
                "continue coding",
                &[],
                InteractionMode::General,
                None,
                &HashSet::new(),
                &HashSet::new(),
                "cli",
                false,
                &[],
                &[],
            )
        };

        let first = build();
        let second = build();
        let first_context = test_chat_message_text(&first[first.len() - 2]);
        let second_context = test_chat_message_text(&second[second.len() - 2]);
        assert!(first_context.contains("## Current Plan"));
        assert!(first_context.contains("[in_progress] implement"));
        assert!(!second_context.contains("## Current Plan"));
    }

    #[test]
    fn project_environment_context_is_incremental_and_commands_are_session_cached() {
        let base = std::env::temp_dir().join(format!(
            "blockcell-project-environment-test-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = Paths::with_base(base);
        paths.ensure_dirs().expect("ensure dirs");
        let workspace = paths.workspace();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&workspace)
                .output()
                .expect("run git");
            assert!(output.status.success(), "git {:?} failed", args);
        };
        git(&["init", "-b", "main"]);
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[package]\nname = \"sample\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            workspace.join("package.json"),
            r#"{"scripts":{"test":"vitest","build":"vite build"}}"#,
        )
        .unwrap();
        let builder = ContextBuilder::new(paths, Config::default());
        let session_key = format!("cli:environment-{}", uuid::Uuid::new_v4());
        let build = || {
            builder.build_messages_for_session_mode_with_channel(
                &session_key,
                &[],
                "fix the project",
                &[],
                InteractionMode::Coding,
                None,
                &HashSet::new(),
                &HashSet::new(),
                "cli",
                false,
                &[],
                &[],
            )
        };

        let first = build();
        let first_context = test_chat_message_text(&first[first.len() - 2]);
        assert!(first_context.contains("## Project Environment"));
        assert!(first_context.contains("Git branch: main"));
        assert!(first_context.contains("Cargo.toml"));
        assert!(first_context.contains("cargo test"));
        assert!(first_context.contains("npm test"));
        assert!(first_context.contains("npm run build"));

        let second = build();
        let second_context = test_chat_message_text(&second[second.len() - 2]);
        assert!(!second_context.contains("## Project Environment"));

        std::fs::write(workspace.join("src.rs"), "fn main() {}\n").unwrap();
        std::fs::write(
            workspace.join("package.json"),
            r#"{"scripts":{"test":"vitest","build":"vite build","lint":"eslint ."}}"#,
        )
        .unwrap();
        let changed = build();
        let changed_context = test_chat_message_text(&changed[changed.len() - 2]);
        assert!(changed_context.contains("## Project Environment"));
        assert!(changed_context.contains("src.rs"));
        assert!(!changed_context.contains("npm run lint"));
    }

    #[test]
    fn project_environment_limits_dirty_files_to_twenty() {
        let base = std::env::temp_dir().join(format!(
            "blockcell-project-environment-limit-test-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = Paths::with_base(base);
        paths.ensure_dirs().expect("ensure dirs");
        let workspace = paths.workspace();
        let status = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&workspace)
            .status()
            .expect("run git init");
        assert!(status.success());
        for index in 0..25 {
            std::fs::write(workspace.join(format!("dirty-{index:02}.txt")), "x").unwrap();
        }
        let builder = ContextBuilder::new(paths, Config::default());
        let session_key = format!("cli:environment-limit-{}", uuid::Uuid::new_v4());

        let messages = builder.build_messages_for_session_mode_with_channel(
            &session_key,
            &[],
            "edit files",
            &[],
            InteractionMode::Coding,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            false,
            &[],
            &[],
        );
        let context = test_chat_message_text(&messages[messages.len() - 2]);

        assert_eq!(context.matches("?? dirty-").count(), 20);
        assert!(context.contains("more dirty entries omitted"));
    }

    #[test]
    fn changed_retrieval_content_is_reinjected() {
        let builder = ContextBuilder::new(
            Paths::with_base(std::env::temp_dir().join(format!(
                "blockcell-context-changed-retrieval-test-{}",
                uuid::Uuid::new_v4()
            ))),
            Config::default(),
        );
        let initial = crate::retrieval::RetrievedItem::new(
            RetrievalSource::CanonicalKnowledge,
            "Deploy with a canary rollout.",
        );
        let changed = crate::retrieval::RetrievedItem::new(
            RetrievalSource::CanonicalKnowledge,
            "Deploy with a canary rollout and rollback gate.",
        );

        let (first, _) =
            builder.filter_incremental_retrieval(Some("cli:changed"), vec![initial.clone()]);
        let (repeated, skipped) =
            builder.filter_incremental_retrieval(Some("cli:changed"), vec![initial]);
        let (updated, _) =
            builder.filter_incremental_retrieval(Some("cli:changed"), vec![changed.clone()]);

        assert_eq!(first.len(), 1);
        assert!(repeated.is_empty());
        assert!(skipped);
        assert_eq!(updated, vec![changed]);
    }

    #[test]
    fn compact_reset_allows_retrieval_items_to_be_injected_again() {
        let builder = ContextBuilder::new(
            Paths::with_base(std::env::temp_dir().join(format!(
                "blockcell-context-compact-retrieval-reset-test-{}",
                uuid::Uuid::new_v4()
            ))),
            Config::default(),
        );
        let item = crate::retrieval::RetrievedItem::new(
            RetrievalSource::CanonicalKnowledge,
            "Remember the deployment rollback gate.",
        );

        let (first, _) =
            builder.filter_incremental_retrieval(Some("cli:compact"), vec![item.clone()]);
        let (repeated, _) =
            builder.filter_incremental_retrieval(Some("cli:compact"), vec![item.clone()]);
        builder.clear_injected_retrieval_for_session("cli:compact");
        let (after_compact, _) =
            builder.filter_incremental_retrieval(Some("cli:compact"), vec![item.clone()]);

        assert_eq!(first, vec![item.clone()]);
        assert!(repeated.is_empty());
        assert_eq!(after_compact, vec![item]);
    }

    #[test]
    fn session_context_cache_evicts_least_recent_session_from_all_maps() {
        let builder = ContextBuilder::new_with_session_cache_capacity(
            Paths::with_base(std::env::temp_dir().join(format!(
                "blockcell-context-session-cache-lru-test-{}",
                uuid::Uuid::new_v4()
            ))),
            Config::default(),
            2,
        );
        let item = crate::retrieval::RetrievedItem::new(
            RetrievalSource::CanonicalKnowledge,
            "A bounded session cache must evict old entries.",
        );
        let populate = |session_key: &str| {
            builder.frozen_file_memory_snapshot(session_key);
            builder.frozen_system_prompt(session_key, InteractionMode::General, None, &[], &[]);
            builder.filter_incremental_retrieval(Some(session_key), vec![item.clone()]);
            builder.incremental_project_environment(session_key);
        };

        populate("session-a");
        populate("session-b");
        builder.frozen_file_memory_snapshot("session-a");
        populate("session-c");

        let cache = builder
            .session_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(
            cache.recency.iter().cloned().collect::<Vec<_>>(),
            vec!["session-a", "session-c"]
        );
        assert!(!cache.file_memory_snapshots.contains_key("session-b"));
        assert!(!cache.system_prompt_snapshots.contains_key("session-b"));
        assert!(!cache.injected_retrieval_items.contains_key("session-b"));
        assert!(!cache
            .project_environment_snapshots
            .contains_key("session-b"));
        assert!(!cache.project_command_cache.contains_key("session-b"));
        assert!(cache.file_memory_snapshots.contains_key("session-a"));
        assert!(cache.file_memory_snapshots.contains_key("session-c"));
        assert!(cache
            .project_environment_snapshots
            .contains_key("session-a"));
        assert!(cache
            .project_environment_snapshots
            .contains_key("session-c"));
    }

    #[test]
    fn configured_prompt_total_caps_complete_system_prompt() {
        let base = std::env::temp_dir().join(format!(
            "blockcell-context-budget-test-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = Paths::with_base(base);
        paths.ensure_dirs().expect("ensure dirs");
        let oversized = "必须遵守这条很长的规则。".repeat(8_000);
        std::fs::write(paths.agents_md(), &oversized).expect("write agents md");
        std::fs::write(paths.soul_md(), &oversized).expect("write soul md");
        let mut config = Config::default();
        config.memory.prompt_budget.total = 8_000;
        config.memory.prompt_budget.rules = 8_000;
        let builder = ContextBuilder::new(paths, config);

        let prompt = builder.build_system_prompt_for_mode_with_channel(
            InteractionMode::General,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            "budget",
            &[],
            &[],
        );

        assert!(crate::token::estimate_tokens(&prompt) <= 8_000);
    }

    #[test]
    fn individual_system_prompt_items_have_hard_caps() {
        let base = std::env::temp_dir().join(format!(
            "blockcell-context-item-cap-test-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = Paths::with_base(base);
        paths.ensure_dirs().expect("ensure dirs");
        std::fs::write(
            paths.agents_md(),
            format!("{} AGENTS_ITEM_END", "agent-rule ".repeat(8_000)),
        )
        .expect("write agents");
        std::fs::write(
            paths.soul_md(),
            format!("{} SOUL_ITEM_END", "personality ".repeat(5_000)),
        )
        .expect("write soul");
        let mut config = Config::default();
        config.memory.prompt_budget.total = 40_000;
        config.memory.prompt_budget.rules = 30_000;
        config.memory.prompt_budget.active_skill = 20_000;
        let builder = ContextBuilder::new(paths, config);
        let active_skill = ActiveSkillContext {
            name: "oversized".to_string(),
            prompt_md: format!("{} ACTIVE_SKILL_END", "procedure ".repeat(15_000)),
            inject_prompt_md: true,
            tools: vec![],
            fallback_message: None,
            source: SkillSource::BlockCell,
            learned: false,
        };
        let tool_rule = format!("{} TOOL_RULE_END", "tool-rule ".repeat(3_000));

        let prompt = builder.build_system_prompt_for_mode_with_channel(
            InteractionMode::Skill,
            Some(&active_skill),
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            "",
            &["read_file".to_string()],
            &[tool_rule],
        );

        assert!(!prompt.contains("AGENTS_ITEM_END"));
        assert!(!prompt.contains("SOUL_ITEM_END"));
        assert!(!prompt.contains("ACTIVE_SKILL_END"));
        assert!(!prompt.contains("TOOL_RULE_END"));
    }

    #[test]
    fn test_shadow_mode_excludes_durable_file_memory_from_prompt() {
        let base =
            std::env::temp_dir().join(format!("blockcell-context-test-{}", uuid::Uuid::new_v4()));
        let paths = Paths::with_base(base);
        paths.ensure_dirs().expect("ensure dirs");
        std::fs::write(paths.user_md(), "Shadow user preference.").expect("write user md");
        std::fs::write(paths.memory_md(), "Shadow durable project fact.").expect("write memory md");
        let config = Config::default();
        assert!(!config.agents.ghost.learning.recall_enabled());
        let builder = ContextBuilder::new(paths, config);

        let prompt = builder.build_system_prompt_for_mode_with_channel(
            InteractionMode::General,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            "shadow prompt",
            &[],
            &[],
        );

        assert!(!prompt.contains("Shadow user preference."));
        assert!(!prompt.contains("Shadow durable project fact."));
    }

    #[test]
    fn test_file_memory_prompt_snapshot_is_frozen_per_session() {
        let base =
            std::env::temp_dir().join(format!("blockcell-context-test-{}", uuid::Uuid::new_v4()));
        let paths = Paths::with_base(base);
        paths.ensure_dirs().expect("ensure dirs");
        std::fs::write(paths.memory_md(), "Initial durable memory.").expect("write memory md");
        let mut config = Config::default();
        config.agents.ghost.learning.recall_enabled = Some(true);
        let builder = ContextBuilder::new(paths.clone(), config);

        let first = builder.build_messages_for_session_mode_with_channel(
            "session-a",
            &[],
            "durable memory",
            &[],
            InteractionMode::General,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            false,
            &[],
            &[],
        );
        std::fs::write(paths.memory_md(), "Updated durable memory.").expect("rewrite memory md");
        let same_session = builder.build_messages_for_session_mode_with_channel(
            "session-a",
            &[],
            "durable memory",
            &[],
            InteractionMode::General,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            false,
            &[],
            &[],
        );
        let next_session = builder.build_messages_for_session_mode_with_channel(
            "session-b",
            &[],
            "durable memory",
            &[],
            InteractionMode::General,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            false,
            &[],
            &[],
        );

        let first_prompt = test_chat_message_text(&first[first.len() - 2]);
        let same_prompt = test_chat_message_text(&same_session[same_session.len() - 2]);
        let next_prompt = test_chat_message_text(&next_session[next_session.len() - 2]);
        assert!(first_prompt.contains("Initial durable memory."));
        assert!(!same_prompt.contains("Initial durable memory."));
        assert!(same_prompt.contains("Previously provided memory remains valid"));
        assert!(!same_prompt.contains("Updated durable memory."));
        assert!(next_prompt.contains("Updated durable memory."));
    }

    #[test]
    fn test_build_system_prompt_injects_ghost_learning_guidance_when_enabled() {
        let mut config = Config::default();
        config.agents.ghost.learning.enabled = true;
        let builder = ContextBuilder::new(
            Paths::with_base(
                std::env::temp_dir()
                    .join(format!("blockcell-context-test-{}", uuid::Uuid::new_v4())),
            ),
            config,
        );

        let prompt = builder.build_system_prompt_for_mode_with_channel(
            InteractionMode::General,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            "用户以后 prefers canary deploys",
            &[],
            &[],
        );

        assert!(prompt.contains("## Ghost Learning"));
        assert!(prompt.contains("durable user preferences"));
        assert!(prompt.contains("reusable workflows"));
        assert!(prompt.contains("prompt-only learned skills"));
        assert!(prompt.contains("Write memories as declarative facts"));
        assert!(prompt.contains("Procedures and workflows belong in skills"));
        assert!(prompt.contains("use `session_search`"));
        assert!(prompt.contains("load it with `skill_view`"));
        assert!(prompt.contains("patch it with `skill_manage(action=\"patch\")`"));
        assert!(prompt.contains("Task progress or temporary state"));
        assert!(!prompt.contains("skill candidates"));
    }

    #[test]
    fn learning_guidance_and_tool_metadata_are_not_duplicated() {
        let base = std::env::temp_dir().join(format!(
            "blockcell-context-guidance-dedup-test-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = Paths::with_base(base);
        paths.ensure_dirs().expect("ensure dirs");
        let mut config = Config::default();
        config.agents.ghost.learning.enabled = true;
        let mut builder = ContextBuilder::new(paths, config);
        builder.set_memory_store(Arc::new(EmptyMemoryStore));

        let prompt = builder.build_system_prompt_for_mode_with_channel(
            InteractionMode::General,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            "",
            &["read_file".to_string(), "write_file".to_string()],
            &["- Canonical MCP rule.".to_string()],
        );

        assert_eq!(prompt.matches("### Memory vs Skill boundary").count(), 1);
        assert_eq!(prompt.matches("## Ghost Learning").count(), 1);
        assert!(!prompt.contains("## Memory Guidance"));
        assert!(!prompt.contains("Current callable tools in this interaction"));
        assert!(!prompt.contains("read_file, write_file"));
        assert_eq!(prompt.matches("Canonical MCP rule.").count(), 1);
        assert!(!prompt.contains("blockcell mcp add sqlite"));
    }

    #[test]
    fn chat_mode_recall_injects_matching_canonical_knowledge() {
        let base = std::env::temp_dir().join(format!(
            "blockcell-context-chat-recall-test-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = Paths::with_base(base);
        paths.ensure_dirs().expect("ensure dirs");
        std::fs::write(paths.user_md(), "User prefers concise release summaries.")
            .expect("write user profile");

        let mut config = Config::default();
        config.agents.ghost.learning.recall_enabled = Some(true);
        config.memory.memory_recall.chat = true;
        let builder = ContextBuilder::new(paths, config);

        let messages = builder.build_messages_for_session_mode_with_channel(
            "cli:chat-recall",
            &[],
            "concise",
            &[],
            InteractionMode::Chat,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            false,
            &[],
            &[],
        );
        let prompt = test_chat_message_text(&messages[messages.len() - 2]);

        assert!(prompt.contains("<retrieved-context>"));
        assert!(prompt.contains("[user-profile]"));
        assert!(prompt.contains("concise release summaries"));
    }

    #[test]
    fn preserves_long_current_user_input() {
        let marker = "UNIQUE_MIDDLE_REQUIREMENT";
        let user_content = format!("{}{}{}", "a".repeat(3000), marker, "z".repeat(3000));
        let builder = ContextBuilder::new(
            Paths::with_base(
                std::env::temp_dir()
                    .join(format!("blockcell-context-test-{}", uuid::Uuid::new_v4())),
            ),
            Config::default(),
        );
        let mut messages = Vec::new();

        builder.append_history_and_user_message(&mut messages, &[], &user_content, &[], false);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.as_str(), Some(user_content.as_str()));
        assert!(messages[0].content.as_str().unwrap().contains(marker));
    }

    #[test]
    fn coding_mode_prompt_contains_edit_git_verification_and_plan_disciplines() {
        let builder = ContextBuilder::new(
            Paths::with_base(std::env::temp_dir().join(format!(
                "blockcell-coding-prompt-test-{}",
                uuid::Uuid::new_v4()
            ))),
            Config::default(),
        );

        let prompt = builder.build_system_prompt_for_mode_with_channel(
            InteractionMode::Coding,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            "fix src/lib.rs",
            &[],
            &[],
        );

        assert!(prompt.contains("## Editing Discipline"));
        assert!(prompt.contains("## Git Discipline"));
        assert!(prompt.contains("## Verification Discipline"));
        assert!(prompt.contains("## Plan Discipline"));
        assert!(prompt.contains("update_plan"));
        assert!(prompt.contains("agent(explore)"));
        assert!(prompt.contains("agent(coder)"));
        assert!(prompt.contains("agent(tester)"));
        assert!(prompt.contains("agent(reviewer)"));
        assert!(prompt.contains("reset --hard"));
    }
}
#[test]
fn session_messages_keep_dynamic_context_out_of_system_prompt() {
    let base = std::env::temp_dir().join(format!(
        "blockcell-stable-system-prompt-test-{}",
        uuid::Uuid::new_v4()
    ));
    let paths = Paths::with_base(base);
    paths.ensure_dirs().expect("ensure dirs");
    std::fs::write(
        paths.memory_md(),
        "Release verification requires a rollback checklist.",
    )
    .expect("write memory");
    let mut config = Config::default();
    config.agents.ghost.learning.recall_enabled = Some(true);
    let builder = ContextBuilder::new(paths.clone(), config);
    let disabled_tools = HashSet::from(["shell".to_string()]);

    let messages = builder.build_messages_for_session_mode_with_channel(
        "cli:stable-prefix",
        &[],
        "release verification",
        &[],
        InteractionMode::General,
        None,
        &HashSet::new(),
        &disabled_tools,
        "cli",
        false,
        &[],
        &[],
    );

    let system = messages[0].content.as_str().expect("system prompt");
    assert!(!system.contains("Current time:"));
    assert!(!system.contains("<retrieved-context>"));
    assert!(!system.contains("Disabled tools:"));

    let ephemeral = messages[messages.len() - 2]
        .content
        .as_str()
        .expect("ephemeral context");
    assert_eq!(messages[messages.len() - 2].role, "user");
    assert!(ephemeral.contains("<runtime-context>"));
    assert!(ephemeral.contains("Current time:"));
    assert!(ephemeral.contains(&format!("Workspace: {}", paths.workspace().display())));
    assert!(ephemeral.contains("<retrieved-context>"));
    assert!(ephemeral.contains("rollback checklist"));
    assert!(ephemeral.contains("Disabled tools: shell"));
    assert_eq!(
        messages.last().unwrap().content.as_str(),
        Some("release verification")
    );
}

#[test]
fn session_system_prompt_snapshot_ignores_agents_file_changes() {
    let base = std::env::temp_dir().join(format!(
        "blockcell-frozen-system-prompt-test-{}",
        uuid::Uuid::new_v4()
    ));
    let paths = Paths::with_base(base);
    paths.ensure_dirs().expect("ensure dirs");
    std::fs::write(paths.agents_md(), "Initial agent rule.").expect("write agents md");
    let builder = ContextBuilder::new(paths.clone(), Config::default());

    let first = builder.build_messages_for_session_mode_with_channel(
        "cli:frozen-prefix",
        &[],
        "first",
        &[],
        InteractionMode::General,
        None,
        &HashSet::new(),
        &HashSet::new(),
        "cli",
        false,
        &[],
        &[],
    );
    std::fs::write(paths.agents_md(), "Changed agent rule.").expect("update agents md");
    let second = builder.build_messages_for_session_mode_with_channel(
        "cli:frozen-prefix",
        &[],
        "second",
        &[],
        InteractionMode::General,
        None,
        &HashSet::new(),
        &HashSet::new(),
        "cli",
        false,
        &[],
        &[],
    );

    assert_eq!(first[0].content, second[0].content);
    assert!(first[0]
        .content
        .as_str()
        .unwrap()
        .contains("Initial agent rule."));
    assert!(!second[0]
        .content
        .as_str()
        .unwrap()
        .contains("Changed agent rule."));
}

#[test]
fn frozen_system_prompt_reader_exposes_parent_prefix_for_typed_agents() {
    let base = std::env::temp_dir().join(format!(
        "blockcell-parent-prompt-reader-test-{}",
        uuid::Uuid::new_v4()
    ));
    let paths = Paths::with_base(base);
    paths.ensure_dirs().unwrap();
    let builder = ContextBuilder::new(paths, Config::default());
    let session_key = "cli:parent-prefix";
    let messages = builder.build_messages_for_session_mode_with_channel(
        session_key,
        &[],
        "implement feature",
        &[],
        InteractionMode::Coding,
        None,
        &HashSet::new(),
        &HashSet::new(),
        "cli",
        false,
        &[],
        &[],
    );

    let frozen = builder
        .system_prompt_snapshot_reader()
        .get(session_key)
        .expect("frozen prompt should be shared");
    assert_eq!(frozen, messages[0].content.as_str().unwrap());
}

#[test]
fn agent_scoped_prompt_section_overrides_global_and_invalidates_frozen_prompt() {
    use crate::prompt_registry::{PromptSection, SystemPromptContext};
    use crate::runtime_kernel::ScopeTarget;

    struct Persona;
    impl PromptSection for Persona {
        fn id(&self) -> &'static str {
            "identity"
        }
        fn priority(&self) -> i32 {
            10
        }
        fn render(&self, _ctx: &SystemPromptContext<'_>) -> Option<String> {
            Some("You are the ops agent.\n\n".to_string())
        }
    }

    let mut builder = ContextBuilder::new(
        Paths::with_base(
            std::env::temp_dir().join(format!("blockcell-prompt-scope-{}", uuid::Uuid::new_v4())),
        ),
        Config::default(),
    );
    let before = builder.frozen_system_prompt("s", InteractionMode::Chat, None, &[], &[]);
    assert!(before.contains("You are blockcell"));

    builder
        .prompt_registry()
        .register(ScopeTarget::Agent("ops".into()), Arc::new(Persona));
    let other_agent = builder.frozen_system_prompt("s", InteractionMode::Chat, None, &[], &[]);
    assert!(other_agent.contains("You are blockcell"));

    builder.set_agent_id(Some("ops"));
    let ops = builder.frozen_system_prompt("s", InteractionMode::Chat, None, &[], &[]);
    assert!(ops.contains("You are the ops agent."), "{ops}");
    assert!(!ops.contains("You are blockcell"));
}
