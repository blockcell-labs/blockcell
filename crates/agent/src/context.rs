use blockcell_core::{Config, Paths};
use blockcell_core::types::ChatMessage;
use blockcell_skills::{EvolutionService, EvolutionServiceConfig, LLMProvider, SkillManager};
use blockcell_tools::MemoryStoreHandle;
use std::sync::Arc;
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
        self.build_system_prompt_for_intents_with_channel(intents, disabled_skills, disabled_tools, "")
    }

    pub fn build_system_prompt_for_intents_with_channel(&self, intents: &[IntentCategory], disabled_skills: &HashSet<String>, disabled_tools: &HashSet<String>, channel: &str) -> String {
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
            prompt.push_str("- Use the `message` tool to send images/files/media to the user. Provide file paths in the `media` array parameter. You can also use it to send to a different channel/chat.\n");
            prompt.push_str("- Use tools when needed to accomplish tasks. Tool descriptions in the schema explain usage.\n");
            prompt.push_str("- To use a skill, first read its SKILL.md file using read_file tool.\n");
            prompt.push_str("- The `read_file` tool can directly read Office documents (.xlsx, .xls, .docx, .pptx).\n");
            prompt.push_str("- Use `spawn` for long-running tasks. Use `list_tasks` to check progress.\n");
            prompt.push_str("- Search `memory_query` before asking the user for information you might already know.\n");
            prompt.push_str("- Never hardcode credentials — ask the user or read from config/memory.\n");
            prompt.push_str("- Always note data delays — financial data is informational only, not investment advice.\n");
            prompt.push_str("- **Web content**: `web_fetch` returns markdown by default — uses `Accept: text/markdown` content negotiation (Cloudflare Markdown for Agents). If server supports it, markdown is returned directly with ~80% token savings. Otherwise HTML is converted to markdown locally. Use `browse` for full browser automation (CDP) — `get_content` also returns markdown. Use `web_fetch` extractMode='raw' only when you need the original HTML.\n");
            // Media display rule depends on channel type:
            // - WebUI (ws/cli/ghost/empty): markdown image syntax works, encourage it
            // - IM channels (wecom/feishu/lark/telegram/slack/discord/dingtalk/whatsapp):
            //   markdown is NOT rendered; sending media MUST go through notification tool
            let is_im_channel = matches!(channel, "wecom" | "feishu" | "lark" | "telegram" | "slack" | "discord" | "dingtalk" | "whatsapp");
            if is_im_channel {
                prompt.push_str("- **当前渠道为 IM 聊天（不渲染 Markdown）**: 不要在回复文字中使用 markdown 图片语法（如 `![](path)`），IM 端不会渲染。若需展示图片内容，用文字描述即可。\n");
                prompt.push_str("- **发送图片/文件给用户（⚠️ 必须调用 message 工具，否则文件不会发出）**: 当用户要求发回图片/文件时，**必须**调用 `message` 工具，参数示例：`{\"media\": [\"/root/.blockcell/workspace/media/xxx.jpg\"], \"content\": \"这是你要的图片\"}`。**绝对禁止**在不调用工具的情况下直接回复\"发送成功\"——那是幻觉，图片根本没有发出去。\n");
            } else {
                prompt.push_str("- **Media display**: The WebUI can render images and play audio inline. To show an image or audio file, include the full file path in your response text (e.g. `/root/.blockcell/workspace/photo.jpg`). The frontend will auto-detect media paths and render them. You can also use markdown image syntax: `![description](file_path)`. NEVER say you cannot display images — you CAN.\n");
                prompt.push_str("- **发送图片/文件给用户（通过聊天渠道）**: 调用 `message` 工具，参数 `media=[\"<本地文件路径>\"]`。仅在回复文字中写 markdown 图片语法无法真正发送文件，必须用工具调用。\n");
            }
            prompt.push_str("- **发送语音给用户**: 需要先将文字合成为语音文件（TTS），再用 `message` 工具 `media=[\"<语音文件路径>\"]` 发送。TTS 能力由技能提供——如果用户要求发语音但没有 TTS 技能，请提示用户安装相应技能（如 tts 技能）。\n");
            prompt.push_str("- When user asks to 打开/开启/启用/enable or 关闭/禁用/disable a skill or tool, use `toggle_manage` tool with action='set'. Do NOT use list_skills for this.\n");
            prompt.push_str("- **Community Hub**: Use the `community_hub` tool (NOT a skill directory) for social interactions. Actions: heartbeat, trending, search_skills, feed, post, like, reply, get_replies, node_search. Hub URL and API key are resolved automatically from config — just call the action directly. If not configured, the tool returns an error.\n");
            prompt.push_str("- **Termux API (Android)**: Use `termux_api` tool to control Android devices via Termux. Requires `termux-api` package + Termux:API app. Use action='info' to check availability. Covers: battery, camera, clipboard, contacts, SMS, calls, location, sensors, notifications, TTS, speech-to-text, media player, microphone, torch, brightness, volume, WiFi, vibrate, share, dialog, wallpaper, fingerprint, infrared, keystore, job scheduler, wake lock. Only available when running on Android/Termux.\n");
            prompt.push('\n');
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
            prompt.push('\n');
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
                    prompt.push('\n');
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
                    // For other intents, check if any trigger keyword overlaps with the skill name
                    // or if the skill name contains intent-relevant keywords.
                    let intent_keywords: &[&str] = match intent {
                        IntentCategory::Organization => &["日程", "任务", "提醒", "记忆", "笔记", "calendar", "task", "reminder", "note", "cron"],
                        IntentCategory::WebSearch => &["搜索", "网页", "浏览", "search", "web", "browse"],
                        IntentCategory::FileOps => &["文件", "代码", "脚本", "file", "code", "script"],
                        IntentCategory::DataAnalysis => &["数据", "图表", "统计", "data", "chart", "analysis"],
                        IntentCategory::DevOps => &["部署", "服务器", "git", "cloud", "deploy", "server"],
                        IntentCategory::Lifestyle => &["健康", "地图", "联系人", "health", "map", "contact"],
                        IntentCategory::IoT => &["智能家居", "传感器", "iot", "smart", "sensor"],
                        _ => &[],
                    };
                    let name_lower = name.to_lowercase();
                    intent_keywords.iter().any(|kw| name_lower.contains(kw))
                        || triggers.iter().any(|t| {
                            let t_lower = t.to_lowercase();
                            intent_keywords.iter().any(|kw| t_lower.contains(kw))
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
        self.build_messages_for_intents_with_channel(history, user_content, media, intents, disabled_skills, disabled_tools, "", false)
    }

    /// Build messages with intent-based filtering and channel context.
    /// `pending_intent`: when true the channel already sent an ack; skip image base64 embedding
    /// so the LLM only sees the path text and asks the user what to do instead of auto-analyzing.
    pub fn build_messages_for_intents_with_channel(
        &self,
        history: &[ChatMessage],
        user_content: &str,
        media: &[String],
        intents: &[IntentCategory],
        disabled_skills: &HashSet<String>,
        disabled_tools: &HashSet<String>,
        channel: &str,
        pending_intent: bool,
    ) -> Vec<ChatMessage> {
        let mut messages = Vec::new();

        // System prompt (intent-filtered, channel-aware)
        messages.push(ChatMessage::system(&self.build_system_prompt_for_intents_with_channel(intents, disabled_skills, disabled_tools, channel)));

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
            // Always append file paths as text so the LLM knows the real local path.
            let all_paths: Vec<&str> = media.iter()
                .filter(|p| !p.is_empty())
                .map(|p| p.as_str())
                .collect();
            let text_with_paths = if all_paths.is_empty() {
                trimmed
            } else {
                let paths_str = all_paths.iter()
                    .map(|p| format!("- `{}`", p))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{}\n\n[附件本地路径（发回给用户时请用此路径）]\n{}", trimmed, paths_str)
            };
            if pending_intent {
                // Channel already sent ack; do NOT embed image as base64.
                // LLM only sees the path text and the question — it should ask what to do.
                messages.push(ChatMessage::user(&text_with_paths));
            } else {
                messages.push(self.build_multimodal_message(&text_with_paths, media));
            }
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

    fn _is_image_path(path: &str) -> bool {
        let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
        matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "svg" | "tiff" | "ico")
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
                // Older rounds: keep user question + final assistant text reply only.
                // IMPORTANT: always push both a user AND an assistant message to maintain
                // the alternating-role invariant required by Anthropic/Gemini providers.
                let user_msg = round.iter().find(|m| m.role == "user");
                let final_assistant = round.iter().rev().find(|m| {
                    m.role == "assistant" && m.tool_calls.is_none()
                });

                if let Some(user) = user_msg {
                    let user_text = Self::content_text(user);
                    let assistant_text = final_assistant
                        .map(|m| Self::content_text(m))
                        .unwrap_or_else(|| "(completed with tool calls)".to_string());

                    result.push(ChatMessage::user(&Self::trim_text_head_tail(&user_text, 200)));
                    result.push(ChatMessage::assistant(&Self::trim_text_head_tail(&assistant_text, 400)));
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
