use blockcell_core::{Config, Paths};
use blockcell_core::types::ChatMessage;
use blockcell_skills::{EvolutionService, EvolutionServiceConfig, SkillManager};
use blockcell_tools::MemoryStoreHandle;
use std::collections::HashSet;
use std::path::Path;

use crate::intent::{IntentCategory, needs_finance_guidelines, needs_skills_list};

pub struct ContextBuilder {
    paths: Paths,
    #[allow(dead_code)]
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
        self.skill_manager.as_ref().and_then(|m| m.evolution_service())
    }

    /// Re-scan skill directories and pick up newly created skills.
    /// Returns the names of newly discovered skills.
    pub fn reload_skills(&mut self) -> Vec<String> {
        if let Some(ref mut manager) = self.skill_manager {
            match manager.reload_skills(&self.paths) {
                Ok(new_skills) => new_skills,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to reload skills");
                    vec![]
                }
            }
        } else {
            vec![]
        }
    }

    /// Build system prompt with all content (legacy, no intent filtering).
    pub fn build_system_prompt(&self) -> String {
        self.build_system_prompt_for_intents(&[IntentCategory::Unknown], &HashSet::new(), &HashSet::new())
    }

    /// Build system prompt filtered by intent categories.
    /// This is the core optimization: only inject relevant rules, tools, and domain knowledge.
    pub fn build_system_prompt_for_intents(&self, intents: &[IntentCategory], disabled_skills: &HashSet<String>, disabled_tools: &HashSet<String>) -> String {
        let mut prompt = String::new();
        let is_chat = intents.len() == 1 && intents[0] == IntentCategory::Chat;

        // ===== Stable prefix (benefits from provider prompt caching) =====

        // Identity
        prompt.push_str("You are blockcell, an AI assistant with access to tools.\n\n");

        // Load bootstrap files (stable across calls)
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

        // Core behavior rules (Method B: ~10 concise rules instead of ~54 verbose tool descriptions)
        if !is_chat {
            prompt.push_str("## Important Rules\n");
            prompt.push_str("- For normal conversation, respond directly with text.\n");
            prompt.push_str("- Only use the `message` tool when you need to send to a specific channel/chat.\n");
            prompt.push_str("- Use tools when needed to accomplish tasks. Tool descriptions in the schema explain usage.\n");
            prompt.push_str("- To use a skill, first read its SKILL.md file using read_file tool.\n");
            prompt.push_str("- The `read_file` tool can directly read Office documents (.xlsx, .xls, .docx, .pptx).\n");
            prompt.push_str("- Use `spawn` for long-running tasks. Use `list_tasks` to check progress.\n");
            prompt.push_str("- Search `memory_query` before asking the user for information you might already know.\n");
            prompt.push_str("- Never hardcode credentials — ask the user or read from config/memory.\n");
            prompt.push_str("- Always note data delays — financial data is informational only, not investment advice.\n");
            prompt.push_str("- **Web content**: `web_fetch` returns markdown by default — uses `Accept: text/markdown` content negotiation (Cloudflare Markdown for Agents). If server supports it, markdown is returned directly with ~80% token savings. Otherwise HTML is converted to markdown locally. Use `browse` for full browser automation (CDP) — `get_content` also returns markdown. Use `web_fetch` extractMode='raw' only when you need the original HTML.\n");
            prompt.push_str("- **Media display**: The WebUI can render images and play audio inline. To show an image or audio file, include the full file path in your response text (e.g. `/Users/apple/.blockcell/workspace/photo.jpg`). The frontend will auto-detect media paths and render them. You can also use markdown image syntax: `![description](file_path)`. NEVER say you cannot display images — you CAN.\n");
            prompt.push_str("- When user asks to 打开/开启/启用/enable or 关闭/禁用/disable a skill or tool, use `toggle_manage` tool with action='set'. Do NOT use list_skills for this.\n");
            prompt.push_str("- **Community Hub**: Use the `community_hub` tool (NOT a skill directory) for social interactions. Actions: heartbeat, trending, search_skills, feed, post, like, reply, get_replies, node_search. Hub URL and API key are resolved automatically from config — just call the action directly. If not configured, the tool returns an error.\n");
            prompt.push_str("\n");
        }

        // ===== Dynamic suffix (changes per call) =====

        // Current time
        let now = chrono::Utc::now();
        prompt.push_str(&format!("Current time: {}\n", now.format("%Y-%m-%d %H:%M:%S UTC")));
        prompt.push_str(&format!("Workspace: {}\n\n", self.paths.workspace().display()));

        // Memory brief
        if let Some(ref store) = self.memory_store {
            match store.generate_brief(20, 10) {
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

        // Disabled toggles section — tell the AI what's currently off
        if !disabled_skills.is_empty() || !disabled_tools.is_empty() {
            prompt.push_str("## ⚠️ Disabled Items\n");
            prompt.push_str("The following items have been disabled by the user via toggle.\n");
            prompt.push_str("IMPORTANT: When user asks to 打开/开启/启用/enable any of these, you MUST call `toggle_manage` tool with action='set', category, name, enabled=true. Do NOT use list_skills.\n");
            if !disabled_skills.is_empty() {
                let mut names: Vec<&String> = disabled_skills.iter().collect();
                names.sort();
                prompt.push_str(&format!("Disabled skills: {}\n", names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
            }
            if !disabled_tools.is_empty() {
                let mut names: Vec<&String> = disabled_tools.iter().collect();
                names.sort();
                prompt.push_str(&format!("Disabled tools: {}\n", names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
            }
            prompt.push_str("\n");
        }

        // Dynamic evolved tools brief (tools the agent has learned via evolution)
        if !is_chat {
            if let Some(ref brief) = self.capability_brief {
                prompt.push_str("## Dynamic Evolved Tools\n");
                prompt.push_str("The following tools have been dynamically evolved and are available. Use `capability_evolve` tool with action='execute' to invoke them.\n");
                prompt.push_str(brief);
                prompt.push_str("\n\n");
            }
        }

        // Skills list (Method D: condensed for non-relevant intents, hidden for Chat)
        if needs_skills_list(intents) {
            self.build_skills_section(&mut prompt, intents, disabled_skills);
        }

        // Financial Analysis Guidelines (Method C: only for Finance/Blockchain intents)
        if needs_finance_guidelines(intents) {
            self.build_finance_guidelines(&mut prompt);
        }

        prompt
    }

    /// Build skills section based on intent (Method D).
    fn build_skills_section(&self, prompt: &mut String, intents: &[IntentCategory], disabled_skills: &HashSet<String>) {
        if let Some(ref manager) = self.skill_manager {
            let mut skills = manager.list_available();
            // Filter out disabled skills
            if !disabled_skills.is_empty() {
                skills.retain(|s| !disabled_skills.contains(&s.name));
            }
            skills.sort_by(|a, b| a.name.cmp(&b.name));
            if skills.is_empty() {
                return;
            }

            let is_unknown = intents.iter().any(|i| matches!(i, IntentCategory::Unknown));

            if is_unknown {
                // For Unknown intent: show category summary only
                let count = skills.len();
                prompt.push_str("## Skills Available\n");
                prompt.push_str(&format!(
                    "{} skills loaded across domains. Use `list_skills query='available'` to see all, or describe your task.\n\n",
                    count
                ));
            } else {
                // For specific intents: show only relevant skills (max 10)
                let relevant: Vec<_> = skills.iter()
                    .filter(|s| !s.meta.triggers.is_empty())
                    .filter(|s| self.skill_matches_intents(s, intents))
                    .take(10)
                    .collect();

                if !relevant.is_empty() {
                    prompt.push_str("## Relevant Skills\n");
                    for skill in relevant {
                        let triggers = skill.meta.triggers.iter()
                            .take(4)
                            .cloned()
                            .collect::<Vec<String>>()
                            .join(" | ");
                        prompt.push_str(&format!("- {} — {}\n", skill.name, triggers));
                    }
                    prompt.push_str("\n");
                }
            }
        }
    }

    /// Check if a skill is relevant to the given intents based on its dependencies/triggers.
    fn skill_matches_intents(&self, skill: &blockcell_skills::Skill, intents: &[IntentCategory]) -> bool {
        let name = &skill.name;
        let caps = &skill.meta.capabilities;
        let triggers = &skill.meta.triggers;

        for intent in intents {
            let matched = match intent {
                IntentCategory::Finance => {
                    caps.iter().any(|c| ["finance_api", "exchange_api", "alert_rule", "stream_subscribe"].contains(&c.as_str()))
                    || ["stock", "bond", "futures", "crypto", "portfolio", "finance", "daily_finance", "macro"].iter().any(|k| name.contains(k))
                }
                IntentCategory::Blockchain => {
                    caps.iter().any(|c| ["blockchain_rpc", "blockchain_tx", "contract_security", "nft_market", "bridge_api", "multisig"].contains(&c.as_str()))
                    || ["crypto", "token", "whale", "defi", "nft", "contract", "wallet", "dao", "treasury"].iter().any(|k| name.contains(k))
                }
                IntentCategory::SystemControl => {
                    caps.iter().any(|c| ["app_control", "chrome_control", "camera_capture", "system_info"].contains(&c.as_str()))
                    || ["app_control", "chrome", "camera"].iter().any(|k| name.contains(k))
                }
                IntentCategory::Media => {
                    caps.iter().any(|c| ["audio_transcribe", "tts", "ocr", "image_understand", "video_process"].contains(&c.as_str()))
                }
                IntentCategory::Communication => {
                    caps.iter().any(|c| ["email", "social_media", "notification"].contains(&c.as_str()))
                }
                _ => {
                    // For other intents, check if any trigger words overlap
                    triggers.iter().any(|t| {
                        let t_lower = t.to_lowercase();
                        intents.iter().any(|_| t_lower.len() > 2)
                    })
                }
            };
            if matched {
                return true;
            }
        }
        false
    }

    /// Build financial analysis guidelines section (Method C: conditional injection).
    fn build_finance_guidelines(&self, prompt: &mut String) {
        prompt.push_str("\n## Financial Analysis Guidelines\n");
        prompt.push_str("When handling financial/stock queries:\n\n");
        prompt.push_str("### Stock Data Sourcing (IMPORTANT — follow this order, do NOT guess URLs)\n");
        prompt.push_str("**A股/港股 (Chinese stocks):**\n");
        prompt.push_str("- **Step 1: Real-time quote** → `finance_api` action='stock_quote' symbol='601318' (直接用6位代码，自动走东方财富API)\n");
        prompt.push_str("- **Step 2: K-line history** → `finance_api` action='stock_history' symbol='601318' interval='1mo'\n");
        prompt.push_str("- **Step 3: Technical indicators** → Calculate locally from K-line data: MA(5/10/20/60), MACD, RSI, KDJ, BOLL. Do NOT search for APIs.\n");
        prompt.push_str("- **Step 4: Advanced data** → Use `http_request` with verified 东方财富 APIs (secid格式: 1.601318=沪市, 0.000001=深市):\n");
        prompt.push_str("  - 资金流向: push2.eastmoney.com/api/qt/stock/fflow/kline/get\n");
        prompt.push_str("  - 北向资金: push2.eastmoney.com/api/qt/kamt.rtmin/get\n");
        prompt.push_str("  - 龙虎榜: datacenter-web.eastmoney.com/api/data/v1/get\n");
        prompt.push_str("  - Headers: Referer='https://quote.eastmoney.com', User-Agent='Mozilla/5.0'\n");
        prompt.push_str("\n**Common Chinese stock codes:** 中国平安=601318, 贵州茅台=600519, 宁德时代=300750, 比亚迪=002594, 招商银行=600036, 腾讯=00700.HK, 阿里巴巴=09988.HK\n");
        prompt.push_str("\n**Technical Indicators** (calculate from K-line, do NOT search for APIs):\n");
        prompt.push_str("- MA(N)=avg of last N closes, MACD: DIF=EMA12-EMA26, DEA=EMA(DIF,9), RSI(N)=100-100/(1+avg_gain/avg_loss), BOLL(20): mid=MA20, upper/lower=mid±2*std\n");
        prompt.push_str("\n**Monitoring**: cron (periodic) + alert_rule (threshold) + stream_subscribe (real-time) + notification (alerts)\n");
        prompt.push_str("**Risk**: Always note data delays — informational only, not investment advice.\n");
    }

    /// Try to match user input against skill triggers.
    /// Returns the matched skill's SKILL.md content and name if found.
    pub fn match_skill(&self, user_input: &str) -> Option<(String, String)> {
        if let Some(ref manager) = self.skill_manager {
            if let Some(skill) = manager.match_skill(user_input) {
                if let Some(md_content) = skill.load_md() {
                    return Some((skill.name.clone(), md_content));
                }
            }
        }
        None
    }

    /// Get a reference to the skill manager.
    pub fn skill_manager(&self) -> Option<&blockcell_skills::SkillManager> {
        self.skill_manager.as_ref()
    }

    pub fn build_messages(&self, history: &[ChatMessage], user_content: &str) -> Vec<ChatMessage> {
        self.build_messages_with_media(history, user_content, &[])
    }

    pub fn build_messages_with_media(
        &self,
        history: &[ChatMessage],
        user_content: &str,
        media: &[String],
    ) -> Vec<ChatMessage> {
        self.build_messages_for_intents(history, user_content, media, &[IntentCategory::Unknown], &HashSet::new(), &HashSet::new())
    }

    /// Build messages with intent-based filtering.
    pub fn build_messages_for_intents(
        &self,
        history: &[ChatMessage],
        user_content: &str,
        media: &[String],
        intents: &[IntentCategory],
        disabled_skills: &HashSet<String>,
        disabled_tools: &HashSet<String>,
    ) -> Vec<ChatMessage> {
        let mut messages = Vec::new();

        // System prompt (intent-filtered)
        messages.push(ChatMessage::system(&self.build_system_prompt_for_intents(intents, disabled_skills, disabled_tools)));

        // History (Method E: smart compression)
        let compressed = Self::compress_history(history);
        let safe_start = Self::find_safe_history_start(&compressed);
        for msg in &compressed[safe_start..] {
            messages.push(Self::trim_chat_message(msg));
        }

        // Current user message with optional media
        if media.is_empty() {
            let trimmed = Self::trim_text_head_tail(user_content, 4000);
            messages.push(ChatMessage::user(&trimmed));
        } else {
            let trimmed = Self::trim_text_head_tail(user_content, 4000);
            messages.push(self.build_multimodal_message(&trimmed, media));
        }

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

    fn encode_image_to_base64(&self, path: &str) -> Option<String> {
        use std::path::Path;
        use base64::Engine;
        
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

    /// Method E: Smart history compression.
    /// - Recent 2 rounds: kept in full
    /// - Older rounds: only user question + final assistant answer (tool calls stripped)
    /// - Max 15 messages total
    fn compress_history(history: &[ChatMessage]) -> Vec<ChatMessage> {
        if history.is_empty() {
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
        let mut result = Vec::new();

        for (i, round) in rounds.iter().enumerate() {
            let is_recent = i >= total_rounds.saturating_sub(2);

            if is_recent {
                // Keep recent 2 rounds in full
                for msg in round {
                    result.push((*msg).clone());
                }
            } else {
                // Older rounds: keep user question + final assistant text reply only
                let user_msg = round.iter().find(|m| m.role == "user");
                let final_assistant = round.iter().rev().find(|m| {
                    m.role == "assistant" && m.tool_calls.is_none()
                });

                if let Some(user) = user_msg {
                    let user_text = Self::content_text(user);
                    let assistant_text = final_assistant
                        .map(|m| Self::content_text(m))
                        .unwrap_or_else(|| "(completed with tool calls)".to_string());

                    let summary = format!(
                        "[Earlier] User: {}\nAssistant: {}",
                        Self::trim_text_head_tail(&user_text, 200),
                        Self::trim_text_head_tail(&assistant_text, 400),
                    );
                    result.push(ChatMessage::user(&summary));
                }
            }
        }

        // Cap at 15 messages from the end
        let max_messages = 15;
        if result.len() > max_messages {
            result = result.split_off(result.len() - max_messages);
        }

        result
    }

    /// Extract text content from a ChatMessage.
    fn content_text(msg: &ChatMessage) -> String {
        match &msg.content {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(parts) => {
                parts.iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(" ")
            }
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
                        let expected_ids: Vec<&str> = tool_calls.iter()
                            .map(|tc| tc.id.as_str())
                            .collect();

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
                                        serde_json::Value::String(Self::trim_text_head_tail(text, max_chars)),
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

        format!("{}\n...<trimmed {} chars>...\n{}", head, char_count.saturating_sub(max_chars), tail)
    }

    fn load_file_if_exists<P: AsRef<Path>>(&self, path: P) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
}
