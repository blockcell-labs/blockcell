use blockcell_core::types::ChatMessage;
use blockcell_core::{Config, Paths};
use blockcell_skills::{EvolutionService, EvolutionServiceConfig, LLMProvider, SkillManager};
use blockcell_tools::MemoryStoreHandle;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use crate::history_projector::{HistoryProjectionProfile, HistoryProjector};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionMode {
    Skill,
    Chat,
    General,
}

#[derive(Debug, Clone)]
pub struct ActiveSkillContext {
    pub name: String,
    pub prompt_md: String,
    pub inject_prompt_md: bool,
    pub tools: Vec<String>,
    pub fallback_message: Option<String>,
}

/// Lightweight token estimator.
/// Chinese characters ≈ 1 token each, English words ≈ 1.3 tokens each.
/// This is intentionally conservative (over-estimates) to avoid context overflow.
fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let mut tokens: usize = 0;
    let mut ascii_word_chars: usize = 0;
    for ch in text.chars() {
        if ch.is_ascii() {
            if ch.is_ascii_whitespace() || ch.is_ascii_punctuation() {
                if ascii_word_chars > 0 {
                    // ~1.3 tokens per English word, round up
                    tokens += 1 + ascii_word_chars / 4;
                    ascii_word_chars = 0;
                }
                // whitespace/punctuation: ~0.25 tokens each, batch them
                tokens += 1;
            } else {
                ascii_word_chars += 1;
            }
        } else {
            // Flush pending ASCII word
            if ascii_word_chars > 0 {
                tokens += 1 + ascii_word_chars / 4;
                ascii_word_chars = 0;
            }
            // CJK and other multi-byte: ~1 token per character
            tokens += 1;
        }
    }
    // Flush trailing ASCII word
    if ascii_word_chars > 0 {
        tokens += 1 + ascii_word_chars / 4;
    }
    // Add per-message overhead (role markers, formatting)
    tokens + 4
}

/// Estimate tokens for a ChatMessage (content + tool_calls overhead).
fn estimate_message_tokens(msg: &ChatMessage) -> usize {
    let content_tokens = match &msg.content {
        serde_json::Value::String(s) => estimate_tokens(s),
        serde_json::Value::Array(parts) => {
            parts
                .iter()
                .map(|p| {
                    if let Some(text) = p.get("text").and_then(|t| t.as_str()) {
                        estimate_tokens(text)
                    } else if p.get("image_url").is_some() {
                        // Base64 images: ~85 tokens for low-detail, ~765 for high-detail
                        // Use conservative estimate
                        200
                    } else {
                        10
                    }
                })
                .sum()
        }
        _ => 0,
    };
    let tool_call_tokens = msg.tool_calls.as_ref().map_or(0, |calls| {
        calls
            .iter()
            .map(|tc| estimate_tokens(&tc.name) + estimate_tokens(&tc.arguments.to_string()) + 10)
            .sum()
    });
    content_tokens + tool_call_tokens + 4 // role overhead
}

pub struct ContextBuilder {
    paths: Paths,
    config: Config,
    skill_manager: Option<SkillManager>,
    memory_store: Option<MemoryStoreHandle>,
    /// Cached capability brief for prompt injection (updated from tick).
    capability_brief: Option<String>,
}

impl ContextBuilder {
    pub fn new(paths: Paths, config: Config) -> Self {
        let skills_dir = paths.skills_dir();
        let mut skill_manager = SkillManager::new()
            .with_versioning(skills_dir.clone())
            .with_evolution(skills_dir, EvolutionServiceConfig::default());
        let _ = skill_manager.load_from_paths(&paths);

        Self {
            paths,
            config,
            skill_manager: Some(skill_manager),
            memory_store: None,
            capability_brief: None,
        }
    }

    pub fn set_skill_manager(&mut self, manager: SkillManager) {
        self.skill_manager = Some(manager);
    }

    pub fn set_memory_store(&mut self, store: MemoryStoreHandle) {
        self.memory_store = Some(store);
    }

    /// Set the cached capability brief (called from tick or initialization).
    pub fn set_capability_brief(&mut self, brief: String) {
        if brief.is_empty() {
            self.capability_brief = None;
        } else {
            self.capability_brief = Some(brief);
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
        _channel: &str,
        user_query: &str,
        available_tool_names: &[String],
        tool_prompt_rules: &[String],
    ) -> String {
        let mut prompt = String::new();
        let is_chat = matches!(mode, InteractionMode::Chat);
        let is_skill_mode = matches!(mode, InteractionMode::Skill);
        let is_general = matches!(mode, InteractionMode::General);

        prompt.push_str("You are blockcell, an AI assistant with access to tools.\n\n");

        if let Some(content) = self.load_file_if_exists(self.paths.agents_md()) {
            prompt.push_str("## Agent Guidelines\n");
            prompt.push_str(&content);
            prompt.push_str("\n\n");
        }

        if let Some(content) = self.load_file_if_exists(self.paths.soul_md()) {
            prompt.push_str("## Personality\n");
            prompt.push_str(&content);
            prompt.push_str("\n\n");
        }

        if let Some(content) = self.load_file_if_exists(self.paths.user_md()) {
            prompt.push_str("## User Preferences\n");
            prompt.push_str(&content);
            prompt.push_str("\n\n");
        }

        if !is_chat {
            prompt.push_str("\n## Tools\n");
            prompt.push_str("- Use tools when needed; otherwise answer directly.\n");
            prompt.push_str("- Prefer fewer tool calls; batch related work.\n");
            prompt.push_str("- Validate tool parameters against schema.\n");
            prompt.push_str("- For filesystem tools such as `list_dir`, `read_file`, `write_file`, and `edit_file`, always pass the required `path` explicitly. Do not call them with `{}` and do not assume an implicit current directory.\n");
            prompt.push_str("- When the user asks about agent nodes, node status, configured agents, or which agent owns which channel/account, use `agent_status` instead of guessing.\n");
            prompt.push_str(
                "- Never hardcode credentials — ask the user or read from config/memory.\n",
            );
            if available_tool_names.is_empty() {
                prompt.push_str("- There are no callable tools available in the current agent scope for this interaction. Do not claim tools outside the current scope.\n");
            } else {
                prompt.push_str(&format!(
                    "- Current callable tools in this interaction: {}\n",
                    available_tool_names.join(", ")
                ));
                prompt.push_str("- When the user asks which tools/capabilities you have, answer only from the current callable tool list above. Do not mention globally registered tools that are not in the current agent scope.\n");
            }
            for rule in tool_prompt_rules {
                prompt.push_str(rule);
                if !rule.ends_with('\n') {
                    prompt.push('\n');
                }
            }
            if tool_prompt_rules.is_empty() {
                prompt.push_str("- **MCP (Model Context Protocol)**: blockcell **已内置 MCP 客户端支持**，可连接任意 MCP 服务器（SQLite、GitHub、文件系统、数据库等）。MCP 工具会以 `<serverName>__<toolName>` 格式出现在工具列表中。若用户询问 MCP 功能或当前工具列表中无 MCP 工具，说明尚未配置 MCP 服务器，请引导用户使用 `blockcell mcp add <template>` 快捷添加，或直接编辑 `~/.blockcell/mcp.json` / `~/.blockcell/mcp.d/*.json`。例如：`blockcell mcp add sqlite --db-path /tmp/test.db`，重启后即可使用。\n");
            }
            prompt.push('\n');
        }

        let now = chrono::Utc::now();
        prompt.push_str(&format!(
            "Current time: {}\n",
            now.format("%Y-%m-%d %H:%M:%S UTC")
        ));
        prompt.push_str(&format!(
            "Workspace: {}\n\n",
            self.paths.workspace().display()
        ));

        if is_skill_mode || is_general {
            if let Some(ref store) = self.memory_store {
                let brief_result = if !user_query.is_empty() {
                    store.generate_brief_for_query(user_query, 8)
                } else {
                    store.generate_brief(5, 3)
                };
                match brief_result {
                    Ok(brief) if !brief.is_empty() => {
                        prompt.push_str("## Memory Brief\n");
                        prompt.push_str(&brief);
                        prompt.push_str("\n\n");
                    }
                    _ => {}
                }
            } else {
                if let Some(content) = self.load_file_if_exists(self.paths.memory_md()) {
                    prompt.push_str("## Long-term Memory\n");
                    prompt.push_str(&content);
                    prompt.push_str("\n\n");
                }
                let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
                if let Some(content) = self.load_file_if_exists(self.paths.daily_memory(&today)) {
                    prompt.push_str("## Today's Notes\n");
                    prompt.push_str(&content);
                    prompt.push_str("\n\n");
                }
            }
        }

        if !disabled_skills.is_empty() || !disabled_tools.is_empty() {
            prompt.push_str("## ⚠️ Disabled Items\n");
            prompt.push_str("The following items have been disabled by the user via toggle.\n");
            prompt.push_str("IMPORTANT: When user asks to 打开/开启/启用/enable any of these, you MUST call `toggle_manage` tool with action='set', category, name, enabled=true. Do NOT use list_skills.\n");
            if !disabled_skills.is_empty() {
                let mut names: Vec<&String> = disabled_skills.iter().collect();
                names.sort();
                prompt.push_str(&format!(
                    "Disabled skills: {}\n",
                    names
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if !disabled_tools.is_empty() {
                let mut names: Vec<&String> = disabled_tools.iter().collect();
                names.sort();
                prompt.push_str(&format!(
                    "Disabled tools: {}\n",
                    names
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            prompt.push('\n');
        }

        if is_skill_mode {
            if let Some(ref brief) = self.capability_brief {
                prompt.push_str("## Dynamic Evolved Tools\n");
                prompt.push_str("The following tools have been dynamically evolved and are available. Use `capability_evolve` tool with action='execute' to invoke them.\n");
                prompt.push_str(brief);
                prompt.push_str("\n\n");
            }
        }

        if let Some(skill) = active_skill {
            prompt.push_str(&format!("## Active Skill: {}\n", skill.name));
            if skill.inject_prompt_md {
                prompt.push_str("The user's input matches this installed skill. Follow the skill's instructions below. Prefer the skill's scoped tools and avoid unrelated tools.\n\n");
                prompt.push_str(&skill.prompt_md);
                prompt.push_str("\n\n");
            } else {
                prompt.push_str("The user's input matches this installed skill. Use the skill's scoped tools and avoid unrelated tools.\n\n");
            }
            if let Some(fallback_message) = &skill.fallback_message {
                prompt.push_str("## Skill Fallback\n");
                prompt.push_str(fallback_message);
                prompt.push_str("\n\n");
            }
        }

        if is_general {
            prompt.push_str("## Core Tool Scope\n");
            prompt.push_str("You currently have access to the minimal built-in tool kernel only. Specialized domain tools are activated by matching installed skills. Prefer the available core tools unless a skill is explicitly active. If the user's request would be better served by specialized domain capabilities that are not currently active, briefly remind the user that they can install the corresponding skills to extend blockcell.\n\n");
        }

        prompt
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
        let is_im_channel = matches!(
            channel,
            "wecom"
                | "feishu"
                | "lark"
                | "telegram"
                | "slack"
                | "discord"
                | "dingtalk"
                | "whatsapp"
                | "napcat"
                | "qq"
        );

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
        let system_tokens = estimate_tokens(&system_prompt);
        messages.push(ChatMessage::system(&system_prompt));

        let user_msg = if media.is_empty() {
            let trimmed = Self::trim_text_head_tail(user_content, 4000);
            ChatMessage::user(&trimmed)
        } else {
            let trimmed = Self::trim_text_head_tail(user_content, 4000);
            let all_paths: Vec<&str> = media
                .iter()
                .filter(|p| !p.is_empty())
                .map(|p| p.as_str())
                .collect();
            let text_with_paths = if all_paths.is_empty() {
                trimmed
            } else {
                let paths_str = all_paths
                    .iter()
                    .map(|p| format!("- `{}`", p))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "{}\n\n[附件本地路径（发回给用户时请用此路径）]\n{}",
                    trimmed, paths_str
                )
            };
            if pending_intent {
                ChatMessage::user(&text_with_paths)
            } else {
                self.build_multimodal_message(&text_with_paths, media)
            }
        };
        let user_msg_tokens = estimate_message_tokens(&user_msg);

        let max_context = self.config.agents.defaults.max_context_tokens as usize;
        let reserved_output = self.config.agents.defaults.max_tokens as usize;
        let safety_margin = 500;
        let history_budget = max_context
            .saturating_sub(system_tokens)
            .saturating_sub(user_msg_tokens)
            .saturating_sub(reserved_output)
            .saturating_sub(safety_margin);

        let projected_history = Self::compress_history_for_user(history, user_content, history_budget);
        let safe_start = Self::find_safe_history_start(&projected_history);
        for msg in &projected_history[safe_start..] {
            messages.push(msg.clone());
        }

        if is_im_channel && messages.len() > 24 {
            let keep = 24;
            let start = messages.len().saturating_sub(keep);
            let mut trimmed = vec![messages[0].clone()];
            trimmed.extend(messages[start..].iter().cloned());
            messages = trimmed;
        }

        messages.push(user_msg);
        messages
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

    /// Compress history by whole rounds so tool chains are never split.
    /// - Prefer keeping the latest 6 complete rounds intact
    /// - Older rounds are summarized to `user + final assistant`
    #[cfg(test)]
    fn compress_history(history: &[ChatMessage], token_budget: usize) -> Vec<ChatMessage> {
        Self::compress_history_for_user(history, "", token_budget)
    }

    fn compress_history_for_user(
        history: &[ChatMessage],
        user_input: &str,
        token_budget: usize,
    ) -> Vec<ChatMessage> {
        if history.is_empty() || token_budget == 0 {
            return Vec::new();
        }

        HistoryProjector::new(history)
            .project(
                user_input,
                HistoryProjectionProfile::Conversation,
                token_budget,
            )
            .messages
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

    fn trim_text_head_tail(s: &str, max_chars: usize) -> String {
        if max_chars == 0 {
            return String::new();
        }

        let char_count = s.chars().count();
        if char_count <= max_chars {
            return s.to_string();
        }

        let head_chars = (max_chars * 2) / 3;
        let tail_chars = max_chars.saturating_sub(head_chars);

        let head = s.chars().take(head_chars).collect::<String>();
        let tail = s.chars().rev().take(tail_chars).collect::<String>();
        let tail = tail.chars().rev().collect::<String>();

        format!(
            "{}\n...<trimmed {} chars>...\n{}",
            head,
            char_count.saturating_sub(max_chars),
            tail
        )
    }

    fn load_file_if_exists<P: AsRef<Path>>(&self, path: P) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn build_tool_round(round: usize) -> Vec<ChatMessage> {
        let call_id = format!("call-{}", round);
        vec![
            ChatMessage::user(&format!("user round {}", round)),
            ChatMessage {
                role: "assistant".to_string(),
                content: serde_json::Value::String(format!("planning round {}", round)),
                reasoning_content: None,
                tool_calls: Some(vec![blockcell_core::types::ToolCallRequest {
                    id: call_id.clone(),
                    name: format!("tool_{}", round),
                    arguments: serde_json::json!({ "round": round }),
                    thought_signature: None,
                }]),
                tool_call_id: None,
                name: None,
            },
            ChatMessage::tool_result(&call_id, &format!(r#"{{"round":{},"ok":true}}"#, round)),
            ChatMessage::assistant(&format!("final round {}", round)),
        ]
    }

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

        assert!(
            builder
                .resolve_active_skill("please deploy the release", &HashSet::new())
                .is_none()
        );
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
    fn test_compress_history_keeps_latest_six_complete_rounds() {
        let mut history = Vec::new();
        for round in 1..=8 {
            history.extend(build_tool_round(round));
        }

        let compressed = ContextBuilder::compress_history(&history, 50_000);

        assert_eq!(compressed.len(), 28);
        assert_eq!(compressed[0].content.as_str(), Some("user round 1"));
        assert_eq!(compressed[1].content.as_str(), Some("final round 1"));
        assert_eq!(compressed[2].content.as_str(), Some("user round 2"));
        assert_eq!(compressed[3].content.as_str(), Some("final round 2"));

        let round_three_index = compressed
            .iter()
            .position(|msg| msg.content.as_str() == Some("user round 3"))
            .expect("round 3 should exist");
        assert_eq!(compressed[round_three_index + 1].role, "assistant");
        assert!(compressed[round_three_index + 1]
            .tool_calls
            .as_ref()
            .is_some_and(|calls| calls.len() == 1));
        assert_eq!(compressed[round_three_index + 2].role, "tool");
        assert_eq!(compressed[round_three_index + 3].content.as_str(), Some("final round 3"));
    }

    #[test]
    fn test_compress_history_never_starts_mid_round() {
        let mut history = Vec::new();
        for round in 1..=3 {
            let mut round_msgs = build_tool_round(round);
            if let Some(ChatMessage {
                content: serde_json::Value::String(text),
                ..
            }) = round_msgs.get_mut(0)
            {
                *text = format!("user round {} {}", round, "x".repeat(120));
            }
            if let Some(ChatMessage {
                content: serde_json::Value::String(text),
                ..
            }) = round_msgs.get_mut(3)
            {
                *text = format!("final round {} {}", round, "y".repeat(160));
            }
            history.extend(round_msgs);
        }

        let compressed = ContextBuilder::compress_history(&history, 140);
        let first = compressed.first().expect("compressed history should not be empty");
        assert_eq!(first.role, "user");
    }
}
