use blockcell_core::types::ChatMessage;
use blockcell_core::{Config, Paths};
use blockcell_skills::{EvolutionService, EvolutionServiceConfig, LLMProvider, SkillManager};
use blockcell_tools::MemoryStoreHandle;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

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
        )
    }

    pub fn resolve_active_skill(
        &self,
        user_input: &str,
        disabled_skills: &HashSet<String>,
    ) -> Option<ActiveSkillContext> {
        if user_input.is_empty() {
            return None;
        }
        let manager = self.skill_manager.as_ref()?;
        let skill = manager.match_skill(user_input, disabled_skills)?;
        let prompt_md = skill.load_md()?;
        Some(ActiveSkillContext {
            name: skill.name.clone(),
            prompt_md,
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

    pub fn build_system_prompt_for_mode_with_channel(
        &self,
        mode: InteractionMode,
        active_skill: Option<&ActiveSkillContext>,
        disabled_skills: &HashSet<String>,
        disabled_tools: &HashSet<String>,
        _channel: &str,
        user_query: &str,
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
            for rule in tool_prompt_rules {
                prompt.push_str(rule);
                if !rule.ends_with('\n') {
                    prompt.push('\n');
                }
            }
            if tool_prompt_rules.is_empty() {
                prompt.push_str("- **MCP (Model Context Protocol)**: blockcell **已内置 MCP 客户端支持**，可连接任意 MCP 服务器（SQLite、GitHub、文件系统、数据库等）。MCP 工具会以 `<serverName>__<toolName>` 格式出现在工具列表中。若用户询问 MCP 功能或当前工具列表中无 MCP 工具，说明尚未配置 MCP 服务器，请引导用户在 `~/.blockcell/config.json` 的 `mcpServers` 字段中添加配置，示例：`{\"mcpServers\": {\"sqlite\": {\"command\": \"uvx\", \"args\": [\"mcp-server-sqlite\", \"--db-path\", \"/tmp/test.db\"]}}}`，重启后即可使用。\n");
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
            prompt.push_str("The user's input matches this installed skill. Follow the skill's instructions below. Prefer the skill's scoped tools and avoid unrelated tools.\n\n");
            prompt.push_str(&skill.prompt_md);
            prompt.push_str("\n\n");
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
        );

        let system_prompt = self.build_system_prompt_for_mode_with_channel(
            mode,
            active_skill,
            disabled_skills,
            disabled_tools,
            channel,
            user_content,
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

        let compressed = Self::compress_history(history, history_budget);
        let safe_start = Self::find_safe_history_start(&compressed);
        for msg in &compressed[safe_start..] {
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

    /// Method E: Smart history compression with dynamic token budget.
    /// - Recent 2 rounds: kept in full (trimmed per-message)
    /// - Older rounds: compressed to user question + final assistant answer (tool calls stripped)
    /// - Fills from newest to oldest, stopping when token budget is exhausted
    /// - Falls back to hard cap of 30 messages as safety net
    fn compress_history(history: &[ChatMessage], token_budget: usize) -> Vec<ChatMessage> {
        if history.is_empty() || token_budget == 0 {
            return Vec::new();
        }

        // Split history into "rounds" — each round starts with a user message
        let mut rounds: Vec<Vec<&ChatMessage>> = Vec::new();
        let mut current_round: Vec<&ChatMessage> = Vec::new();

        for msg in history {
            if msg.role == "user" && !current_round.is_empty() {
                rounds.push(current_round);
                current_round = Vec::new();
            }
            current_round.push(msg);
        }
        if !current_round.is_empty() {
            rounds.push(current_round);
        }

        let total_rounds = rounds.len();

        // Phase 1: Build recent rounds (last 2) in full, with per-message trim
        let mut recent_msgs: Vec<ChatMessage> = Vec::new();
        let recent_start = total_rounds.saturating_sub(2);
        for round in &rounds[recent_start..] {
            for msg in round {
                recent_msgs.push(Self::trim_chat_message(msg));
            }
        }
        let recent_tokens: usize = recent_msgs.iter().map(|m| estimate_message_tokens(m)).sum();

        // If recent rounds alone exceed budget, just return them (trimmed harder)
        if recent_tokens >= token_budget {
            // Hard-trim recent messages to fit
            let mut result = Vec::new();
            let mut used = 0usize;
            for msg in recent_msgs.into_iter().rev() {
                let t = estimate_message_tokens(&msg);
                if used + t > token_budget && !result.is_empty() {
                    break;
                }
                used += t;
                result.push(msg);
            }
            result.reverse();
            // Safety: skip any leading orphaned tool messages caused by the trim above
            let safe = Self::find_safe_history_start(&result);
            if safe > 0 {
                result = result.split_off(safe);
            }
            return result;
        }

        // Phase 2: Fill older rounds (compressed) from newest to oldest within remaining budget
        let remaining_budget = token_budget.saturating_sub(recent_tokens);
        let mut older_msgs: Vec<ChatMessage> = Vec::new();
        let mut older_tokens = 0usize;

        // Iterate older rounds in reverse (newest-old first) so we keep the most relevant
        for i in (0..recent_start).rev() {
            let round = &rounds[i];
            // Compress: keep user question + final assistant text only
            let user_msg = round.iter().find(|m| m.role == "user");
            let final_assistant = round
                .iter()
                .rev()
                .find(|m| m.role == "assistant" && m.tool_calls.is_none());

            if let Some(user) = user_msg {
                let user_text = Self::content_text(user);
                let assistant_text = final_assistant
                    .map(|m| Self::content_text(m))
                    .unwrap_or_else(|| "(completed with tool calls)".to_string());

                let u = ChatMessage::user(&Self::trim_text_head_tail(&user_text, 200));
                let a = ChatMessage::assistant(&Self::trim_text_head_tail(&assistant_text, 400));
                let pair_tokens = estimate_message_tokens(&u) + estimate_message_tokens(&a);

                if older_tokens + pair_tokens > remaining_budget {
                    break; // Budget exhausted
                }
                older_tokens += pair_tokens;
                // Prepend (we're iterating in reverse)
                older_msgs.push(u);
                older_msgs.push(a);
            }
        }
        // Reverse because we built it newest-first
        older_msgs.reverse();

        // Combine: older compressed + recent full
        let mut result = older_msgs;
        result.extend(recent_msgs);

        // Safety cap: never exceed 30 messages regardless of budget
        let max_messages = 30;
        if result.len() > max_messages {
            result = result.split_off(result.len() - max_messages);
            // After split_off, the new head may be an orphaned tool message
            let safe = Self::find_safe_history_start(&result);
            if safe > 0 {
                result = result.split_off(safe);
            }
        }

        result
    }

    /// Extract text content from a ChatMessage.
    fn content_text(msg: &ChatMessage) -> String {
        match &msg.content {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(parts) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        }
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
                        for j in (i + 1)..history.len() {
                            if history[j].role == "tool" {
                                if let Some(ref id) = history[j].tool_call_id {
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

    fn trim_chat_message(msg: &ChatMessage) -> ChatMessage {
        let mut out = msg.clone();

        let max_chars = match out.role.as_str() {
            "tool" => 2400,
            "system" => 8000,
            _ => 1400,
        };

        match &out.content {
            serde_json::Value::String(s) => {
                let trimmed = Self::trim_text_head_tail(s, max_chars);
                out.content = serde_json::Value::String(trimmed);
            }
            serde_json::Value::Array(parts) => {
                let mut new_parts = Vec::with_capacity(parts.len());
                for part in parts {
                    if let Some(obj) = part.as_object() {
                        if let Some(t) = obj.get("type").and_then(|v| v.as_str()) {
                            if t == "text" {
                                if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                                    let mut new_obj = obj.clone();
                                    new_obj.insert(
                                        "text".to_string(),
                                        serde_json::Value::String(Self::trim_text_head_tail(
                                            text, max_chars,
                                        )),
                                    );
                                    new_parts.push(serde_json::Value::Object(new_obj));
                                    continue;
                                }
                            }
                        }
                    }
                    new_parts.push(part.clone());
                }
                out.content = serde_json::Value::Array(new_parts);
            }
            _ => {}
        }

        out
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
