//! System Prompt Registry：把系统提示词拆成可独立贡献的 section。
//!
//! - 每个 [`PromptSection`] 只负责自己的内容与出现条件；
//! - [`PromptRegistry`] 基于 [`ScopedRegistry`] 存放 section，同 id 时更具体的作用域
//!   （Candidate > Session > Agent > Global）覆盖默认值，同作用域后注册者覆盖先注册者；
//! - 最终按 priority 排序拼装，再交给 `PromptBudgetAllocator` 做预算裁剪。

use std::collections::HashMap;
use std::sync::Arc;

use blockcell_core::Paths;

use crate::context::{
    ActiveSkillContext, InteractionMode, CODING_MODE_PROMPT, MAX_ACTIVE_SKILL_TOKENS,
    MAX_AGENTS_MD_TOKENS, MAX_SOUL_MD_TOKENS, MAX_TOOL_RULE_TOKENS, MEMORY_GUIDANCE_BODY,
    SKILL_GUIDANCE,
};
use crate::retrieval::truncate_to_token_budget;
use crate::runtime_kernel::{ScopeGuard, ScopeId, ScopeTarget, ScopeView, ScopedRegistry};

/// Section 渲染结果进入的预算槽位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptSlot {
    Rules,
    ActiveSkill,
}

/// 渲染系统提示词时可见的上下文。
pub struct SystemPromptContext<'a> {
    pub mode: InteractionMode,
    pub active_skill: Option<&'a ActiveSkillContext>,
    pub available_tool_names: &'a [String],
    pub tool_prompt_rules: &'a [String],
    pub paths: &'a Paths,
    pub ghost_learning_enabled: bool,
    pub has_memory_store: bool,
    pub capability_brief: Option<&'a str>,
}

impl SystemPromptContext<'_> {
    pub fn is_chat(&self) -> bool {
        matches!(self.mode, InteractionMode::Chat)
    }
}

pub trait PromptSection: Send + Sync {
    fn id(&self) -> &'static str;

    /// 越小越靠前。
    fn priority(&self) -> i32;

    fn slot(&self) -> PromptSlot {
        PromptSlot::Rules
    }

    /// 返回 `None` 表示本次不出现。
    fn render(&self, ctx: &SystemPromptContext<'_>) -> Option<String>;
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RenderedPrompt {
    pub rules: String,
    pub active_skill: String,
}

#[derive(Clone, Default)]
pub struct PromptRegistry {
    sections: ScopedRegistry<Arc<dyn PromptSection>>,
}

impl PromptRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_builtin_sections() -> Self {
        let registry = Self::new();
        let builtins: [Arc<dyn PromptSection>; 11] = [
            Arc::new(IdentitySection),
            Arc::new(FileSection {
                id: "agent_guidelines",
                priority: 20,
                heading: "## Agent Guidelines\n",
                max_tokens: MAX_AGENTS_MD_TOKENS,
                path: Paths::agents_md,
            }),
            Arc::new(FileSection {
                id: "personality",
                priority: 30,
                heading: "## Personality\n",
                max_tokens: MAX_SOUL_MD_TOKENS,
                path: Paths::soul_md,
            }),
            Arc::new(GhostLearningSection),
            Arc::new(ToolsSection),
            Arc::new(CodingModeSection),
            Arc::new(EvolvedToolsSection),
            Arc::new(ActiveSkillSection),
            Arc::new(CoreToolScopeSection),
            Arc::new(SkillGuidanceSection),
            Arc::new(MemoryGuidanceSection),
        ];
        for section in builtins {
            registry.register(ScopeTarget::Global, section);
        }
        registry
    }

    pub fn register(&self, target: ScopeTarget, section: Arc<dyn PromptSection>) -> ScopeId {
        self.sections.insert(target, section)
    }

    pub fn register_scoped(
        &self,
        target: ScopeTarget,
        section: Arc<dyn PromptSection>,
    ) -> ScopeGuard<Arc<dyn PromptSection>> {
        self.sections.register(target, section)
    }

    pub fn unregister(&self, id: ScopeId) -> bool {
        self.sections.remove(id).is_some()
    }

    pub fn fingerprint(&self) -> u64 {
        self.sections.fingerprint()
    }

    /// 解析可见 section（处理同 id 覆盖）并按 priority 拼装。
    pub fn render(&self, view: ScopeView<'_>, ctx: &SystemPromptContext<'_>) -> RenderedPrompt {
        let mut chosen: HashMap<&'static str, (u8, usize, Arc<dyn PromptSection>)> = HashMap::new();
        for (order, (target, section)) in self
            .sections
            .visible_with_target(view)
            .into_iter()
            .enumerate()
        {
            let specificity = match target {
                ScopeTarget::Global => 0,
                ScopeTarget::Agent(_) => 1,
                ScopeTarget::Session(_) => 2,
                ScopeTarget::Candidate(_) => 3,
            };
            match chosen.get(section.id()) {
                Some((existing, _, _)) if *existing > specificity => {}
                _ => {
                    chosen.insert(section.id(), (specificity, order, section));
                }
            }
        }
        let mut sections: Vec<(usize, Arc<dyn PromptSection>)> = chosen
            .into_values()
            .map(|(_, order, section)| (order, section))
            .collect();
        sections.sort_by_key(|(order, section)| (section.priority(), *order));

        let mut rendered = RenderedPrompt::default();
        for (_, section) in sections {
            if let Some(text) = section.render(ctx) {
                match section.slot() {
                    PromptSlot::Rules => rendered.rules.push_str(&text),
                    PromptSlot::ActiveSkill => rendered.active_skill.push_str(&text),
                }
            }
        }
        rendered
    }
}

struct IdentitySection;

impl PromptSection for IdentitySection {
    fn id(&self) -> &'static str {
        "identity"
    }
    fn priority(&self) -> i32 {
        10
    }
    fn render(&self, _ctx: &SystemPromptContext<'_>) -> Option<String> {
        Some("You are blockcell, an AI assistant with access to tools.\n\n".to_string())
    }
}

/// 从工作区文件加载内容（AGENTS.md / SOUL.md）。
struct FileSection {
    id: &'static str,
    priority: i32,
    heading: &'static str,
    max_tokens: usize,
    path: fn(&Paths) -> std::path::PathBuf,
}

impl PromptSection for FileSection {
    fn id(&self) -> &'static str {
        self.id
    }
    fn priority(&self) -> i32 {
        self.priority
    }
    fn render(&self, ctx: &SystemPromptContext<'_>) -> Option<String> {
        let content = std::fs::read_to_string((self.path)(ctx.paths)).ok()?;
        Some(format!(
            "{}{}\n\n",
            self.heading,
            truncate_to_token_budget(&content, self.max_tokens)
        ))
    }
}

struct GhostLearningSection;

impl PromptSection for GhostLearningSection {
    fn id(&self) -> &'static str {
        "ghost_learning"
    }
    fn priority(&self) -> i32 {
        40
    }
    fn render(&self, ctx: &SystemPromptContext<'_>) -> Option<String> {
        if !ctx.ghost_learning_enabled || ctx.is_chat() {
            return None;
        }
        let mut prompt = String::from("## Ghost Learning\n");
        prompt.push_str(
            "BlockCell may review successful interactions after the response to learn durable user preferences, stable project facts, reusable workflows, and prompt-only learned skills.\n",
        );
        prompt.push_str(MEMORY_GUIDANCE_BODY);
        prompt.push('\n');
        prompt.push_str(
            "- Write memories as declarative facts, not commands to yourself. Example: 'User prefers concise responses' is good; 'Always respond concisely' is not.\n",
        );
        prompt.push_str(
            "- If the user references prior conversations or you suspect relevant history exists, use `session_search` before asking the user to repeat context.\n",
        );
        prompt.push_str(
            "- If an available learned skill is relevant, load it with `skill_view` before proceeding, even if you think you already know the task.\n",
        );
        prompt.push_str(
            "- If a loaded skill is stale, incomplete, or wrong, patch it with `skill_manage(action=\"patch\")` after validating the fix.\n",
        );
        prompt.push_str(
            "- Current user instructions always override learned memory and generated skills.\n\n",
        );
        Some(prompt)
    }
}

struct ToolsSection;

impl PromptSection for ToolsSection {
    fn id(&self) -> &'static str {
        "tools"
    }
    fn priority(&self) -> i32 {
        50
    }
    fn render(&self, ctx: &SystemPromptContext<'_>) -> Option<String> {
        if ctx.is_chat() {
            return None;
        }
        let mut prompt = String::from("\n## Tools\n");
        prompt.push_str("- Use tools when needed; otherwise answer directly.\n");
        prompt.push_str("- Prefer fewer tool calls; batch related work.\n");
        prompt.push_str("- Validate tool parameters against schema.\n");
        prompt.push_str("- For filesystem tools such as `list_dir`, `read_file`, `write_file`, and `edit_file`, always pass the required `path` explicitly. Do not call them with `{}` and do not assume an implicit current directory.\n");
        prompt.push_str("- When the user asks about agent nodes, node status, configured agents, or which agent owns which channel/account, use `agent_status` instead of guessing.\n");
        prompt
            .push_str("- Never hardcode credentials — ask the user or read from config/memory.\n");
        if ctx.available_tool_names.is_empty() {
            prompt.push_str("- There are no callable tools available in the current agent scope for this interaction. Do not claim tools outside the current scope.\n");
        } else {
            prompt.push_str("- The current callable tools are defined by the attached tool schemas. When asked about capabilities, answer only from those schemas.\n");
        }
        for rule in ctx.tool_prompt_rules {
            let rule = truncate_to_token_budget(rule, MAX_TOOL_RULE_TOKENS);
            prompt.push_str(&rule);
            if !rule.ends_with('\n') {
                prompt.push('\n');
            }
        }
        prompt.push('\n');
        Some(prompt)
    }
}

struct CodingModeSection;

impl PromptSection for CodingModeSection {
    fn id(&self) -> &'static str {
        "coding_mode"
    }
    fn priority(&self) -> i32 {
        60
    }
    fn render(&self, ctx: &SystemPromptContext<'_>) -> Option<String> {
        matches!(ctx.mode, InteractionMode::Coding).then(|| format!("{CODING_MODE_PROMPT}\n"))
    }
}

struct EvolvedToolsSection;

impl PromptSection for EvolvedToolsSection {
    fn id(&self) -> &'static str {
        "evolved_tools"
    }
    fn priority(&self) -> i32 {
        70
    }
    fn render(&self, ctx: &SystemPromptContext<'_>) -> Option<String> {
        if !matches!(ctx.mode, InteractionMode::Skill) {
            return None;
        }
        let brief = ctx.capability_brief?;
        Some(format!(
            "## Dynamic Evolved Tools\nThe following tools have been dynamically evolved and are available. Use `capability_evolve` tool with action='execute' to invoke them.\n{brief}\n\n"
        ))
    }
}

struct ActiveSkillSection;

impl PromptSection for ActiveSkillSection {
    fn id(&self) -> &'static str {
        "active_skill"
    }
    fn priority(&self) -> i32 {
        80
    }
    fn slot(&self) -> PromptSlot {
        PromptSlot::ActiveSkill
    }
    fn render(&self, ctx: &SystemPromptContext<'_>) -> Option<String> {
        let skill = ctx.active_skill?;
        let mut prompt = format!("## Active Skill: {}\n", skill.name);
        if skill.inject_prompt_md {
            prompt.push_str("The user's input matches this installed skill. Follow the skill's instructions below. Prefer the skill's scoped tools and avoid unrelated tools.\n\n");
            prompt.push_str(&truncate_to_token_budget(
                &skill.prompt_md,
                MAX_ACTIVE_SKILL_TOKENS,
            ));
            prompt.push_str("\n\n");
        } else {
            prompt.push_str("The user's input matches this installed skill. Use the skill's scoped tools and avoid unrelated tools.\n\n");
        }
        if let Some(fallback_message) = &skill.fallback_message {
            prompt.push_str("## Skill Fallback\n");
            prompt.push_str(fallback_message);
            prompt.push_str("\n\n");
        }
        Some(prompt)
    }
}

struct CoreToolScopeSection;

impl PromptSection for CoreToolScopeSection {
    fn id(&self) -> &'static str {
        "core_tool_scope"
    }
    fn priority(&self) -> i32 {
        90
    }
    fn render(&self, ctx: &SystemPromptContext<'_>) -> Option<String> {
        matches!(ctx.mode, InteractionMode::General | InteractionMode::Coding).then(|| {
            "## Core Tool Scope\nYou currently have access to the minimal built-in tool kernel only. Specialized domain tools are activated by matching installed skills. Prefer the available core tools unless a skill is explicitly active. If the user's request would be better served by specialized domain capabilities that are not currently active, briefly remind the user that they can install the corresponding skills to extend blockcell.\n\n".to_string()
        })
    }
}

struct SkillGuidanceSection;

impl PromptSection for SkillGuidanceSection {
    fn id(&self) -> &'static str {
        "skill_guidance"
    }
    fn priority(&self) -> i32 {
        100
    }
    fn render(&self, ctx: &SystemPromptContext<'_>) -> Option<String> {
        (!ctx.is_chat()).then(|| format!("{SKILL_GUIDANCE}\n"))
    }
}

struct MemoryGuidanceSection;

impl PromptSection for MemoryGuidanceSection {
    fn id(&self) -> &'static str {
        "memory_guidance"
    }
    fn priority(&self) -> i32 {
        110
    }
    fn render(&self, ctx: &SystemPromptContext<'_>) -> Option<String> {
        (ctx.has_memory_store && (!ctx.ghost_learning_enabled || ctx.is_chat()))
            .then(|| format!("## Memory Guidance\n{MEMORY_GUIDANCE_BODY}\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed {
        id: &'static str,
        priority: i32,
        text: &'static str,
    }

    impl PromptSection for Fixed {
        fn id(&self) -> &'static str {
            self.id
        }
        fn priority(&self) -> i32 {
            self.priority
        }
        fn render(&self, _ctx: &SystemPromptContext<'_>) -> Option<String> {
            Some(self.text.to_string())
        }
    }

    fn fixed(id: &'static str, priority: i32, text: &'static str) -> Arc<dyn PromptSection> {
        Arc::new(Fixed { id, priority, text })
    }

    fn render(registry: &PromptRegistry, agent_id: &str, mode: InteractionMode) -> RenderedPrompt {
        let paths = Paths::with_base(std::env::temp_dir().join("blockcell-prompt-registry-test"));
        let ctx = SystemPromptContext {
            mode,
            active_skill: None,
            available_tool_names: &[],
            tool_prompt_rules: &[],
            paths: &paths,
            ghost_learning_enabled: false,
            has_memory_store: false,
            capability_brief: None,
        };
        registry.render(
            ScopeView {
                agent_id,
                session_id: "s1",
                candidate_id: None,
            },
            &ctx,
        )
    }

    #[test]
    fn sections_render_in_priority_order() {
        let registry = PromptRegistry::new();
        registry.register(ScopeTarget::Global, fixed("b", 20, "B"));
        registry.register(ScopeTarget::Global, fixed("a", 10, "A"));
        registry.register(ScopeTarget::Global, fixed("c", 20, "C"));

        assert_eq!(
            render(&registry, "default", InteractionMode::General).rules,
            "ABC"
        );
    }

    #[test]
    fn more_specific_scope_overrides_same_section_id() {
        let registry = PromptRegistry::new();
        registry.register(ScopeTarget::Global, fixed("persona", 10, "global"));
        registry.register(
            ScopeTarget::Agent("ops".into()),
            fixed("persona", 10, "ops"),
        );

        assert_eq!(
            render(&registry, "ops", InteractionMode::General).rules,
            "ops"
        );
        assert_eq!(
            render(&registry, "default", InteractionMode::General).rules,
            "global"
        );

        registry.register(
            ScopeTarget::Session("s1".into()),
            fixed("persona", 10, "session"),
        );
        assert_eq!(
            render(&registry, "ops", InteractionMode::General).rules,
            "session"
        );
    }

    #[test]
    fn scoped_registration_changes_fingerprint_and_is_removed_on_drop() {
        let registry = PromptRegistry::with_builtin_sections();
        let before = registry.fingerprint();
        {
            let _guard =
                registry.register_scoped(ScopeTarget::Global, fixed("extra", 1000, "EXTRA"));
            assert_ne!(registry.fingerprint(), before);
            assert!(render(&registry, "default", InteractionMode::Chat)
                .rules
                .ends_with("EXTRA"));
        }
        assert_eq!(registry.fingerprint(), before);
    }

    #[test]
    fn builtin_sections_follow_mode_conditions() {
        let registry = PromptRegistry::with_builtin_sections();
        let chat = render(&registry, "default", InteractionMode::Chat).rules;
        assert!(chat.starts_with("You are blockcell"));
        assert!(!chat.contains("## Tools"));

        let coding = render(&registry, "default", InteractionMode::Coding).rules;
        assert!(coding.contains("## Tools"));
        assert!(coding.contains("## Editing Discipline"));
        assert!(coding.contains("## Core Tool Scope"));
        assert!(coding.find("## Tools") < coding.find("## Editing Discipline"));
    }
}
