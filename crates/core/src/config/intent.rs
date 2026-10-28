//! Intent Router 配置类型
//!
//! 包含意图路由、工具规则和意图分类相关的配置定义。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentToolRuleConfig {
    #[serde(default = "super::default_true")]
    pub inherit_base: bool,
    #[serde(default)]
    pub tools: Vec<String>,
}

impl Default for IntentToolRuleConfig {
    fn default() -> Self {
        Self {
            inherit_base: true,
            tools: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IntentToolEntryConfig {
    Tools(Vec<String>),
    Rule(IntentToolRuleConfig),
}

impl IntentToolEntryConfig {
    pub fn inherit_base(&self) -> bool {
        match self {
            Self::Tools(_) => true,
            Self::Rule(rule) => rule.inherit_base,
        }
    }

    pub fn tools(&self) -> &[String] {
        match self {
            Self::Tools(tools) => tools,
            Self::Rule(rule) => &rule.tools,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct IntentToolProfileConfig {
    /// Omitted: legacy foundation. Explicit []: no implicit capabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_packs: Option<Vec<ToolBasePack>>,
    /// Explicit host approval grouped by skill. Included in the agent ceiling;
    /// the skill manifest/learned flag itself cannot grant capabilities.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub skill_grants: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub core_tools: Vec<String>,
    #[serde(default)]
    pub intent_tools: HashMap<String, IntentToolEntryConfig>,
    #[serde(default)]
    pub deny_tools: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolBasePack {
    Core,
    Coding,
    Skills,
    Discovery,
}

impl ToolBasePack {
    pub const LEGACY: &'static [Self] = &[Self::Core, Self::Coding, Self::Skills, Self::Discovery];
}

/// 配置文件中自定义的意图匹配规则，与代码内置规则互补。
/// 每条规则对应一个 IntentCategory，命中即叠加到分类结果中。
/// 注意：`category` 必须填写，空字符串会被 `with_extra_rules` 跳过并 warn。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentRuleConfig {
    /// 意图类别名称，对应 IntentCategory::as_str()，如 "Finance"、"FileOps"
    pub category: String,
    /// 关键词列表（大小写不敏感，出现即命中）
    #[serde(default)]
    pub keywords: Vec<String>,
    /// 正则表达式列表（任意一条匹配即命中）
    #[serde(default)]
    pub patterns: Vec<String>,
    /// 否定关键词（出现时跳过该规则）
    #[serde(default)]
    pub negative: Vec<String>,
    /// 优先级（0-255，越高越优先）
    #[serde(default = "default_intent_rule_priority")]
    pub priority: u8,
}

fn default_intent_rule_priority() -> u8 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentRouterConfig {
    /// Optional unreliable lexical guidance, appended only to the current user turn.
    #[serde(default)]
    pub lexical_hints: bool,
    /// Static capability union by default; `gate` retains legacy intent gating.
    #[serde(default)]
    pub tool_selection: ToolSelection,
    #[serde(default = "super::default_true")]
    pub enabled: bool,
    /// 当 enabled=false 时，是否全量加载所有可用工具。
    /// - load_all_tools=true: 全量加载所有工具，让 LLM 自己选择
    /// - load_all_tools=false: 走 Unknown profile（由配置决定工具）
    #[serde(default)]
    pub load_all_tools: bool,
    #[serde(default = "default_intent_router_profile")]
    pub default_profile: String,
    #[serde(default)]
    pub agent_profiles: HashMap<String, String>,
    #[serde(default = "default_intent_router_profiles")]
    pub profiles: HashMap<String, IntentToolProfileConfig>,
    /// 配置文件中自定义的意图匹配规则，与代码内置规则互补（叠加，不覆盖）。
    #[serde(default)]
    pub intent_rules: Vec<IntentRuleConfig>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSelection {
    #[default]
    Rank,
    Gate,
}

impl Default for IntentRouterConfig {
    fn default() -> Self {
        Self {
            tool_selection: ToolSelection::Rank,
            lexical_hints: false,
            enabled: true,
            load_all_tools: false,
            default_profile: default_intent_router_profile(),
            agent_profiles: HashMap::new(),
            profiles: default_intent_router_profiles(),
            intent_rules: Vec::new(),
        }
    }
}

fn default_intent_router_profile() -> String {
    "default".to_string()
}

fn default_intent_router_profiles() -> HashMap<String, IntentToolProfileConfig> {
    let mut profiles = HashMap::new();
    profiles.insert(
        "default".to_string(),
        IntentToolProfileConfig {
            base_packs: None,
            skill_grants: HashMap::new(),
            core_tools: vec![
                "read_file".to_string(),
                "write_file".to_string(),
                "list_dir".to_string(),
                "exec".to_string(),
                "web_search".to_string(),
                "web_fetch".to_string(),
                "memory_query".to_string(),
                "memory_upsert".to_string(),
                "toggle_manage".to_string(),
                "message".to_string(),
                "agent_status".to_string(),
                "session_recall".to_string(),
            ],
            intent_tools: HashMap::from([
                (
                    "Chat".to_string(),
                    IntentToolEntryConfig::Rule(IntentToolRuleConfig {
                        inherit_base: false,
                        tools: vec![],
                    }),
                ),
                (
                    "FileOps".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "edit_file".to_string(),
                        "file_ops".to_string(),
                        "data_process".to_string(),
                        "office_write".to_string(),
                    ]),
                ),
                (
                    "WebSearch".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "browse".to_string(),
                        "http_request".to_string(),
                    ]),
                ),
                (
                    "Finance".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "http_request".to_string(),
                        "data_process".to_string(),
                        "chart_generate".to_string(),
                        "alert_rule".to_string(),
                        "stream_subscribe".to_string(),
                        "knowledge_graph".to_string(),
                        "cron".to_string(),
                        "office_write".to_string(),
                        "browse".to_string(),
                    ]),
                ),
                (
                    "Blockchain".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "stream_subscribe".to_string(),
                        "http_request".to_string(),
                        "knowledge_graph".to_string(),
                    ]),
                ),
                (
                    "DataAnalysis".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "edit_file".to_string(),
                        "file_ops".to_string(),
                        "data_process".to_string(),
                        "chart_generate".to_string(),
                        "office_write".to_string(),
                        "http_request".to_string(),
                    ]),
                ),
                (
                    "Communication".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "email".to_string(),
                        "message".to_string(),
                        "http_request".to_string(),
                        "community_hub".to_string(),
                        // NapCatQQ - User tools
                        "napcat_get_login_info".to_string(),
                        "napcat_get_status".to_string(),
                        "napcat_get_version_info".to_string(),
                        "napcat_get_stranger_info".to_string(),
                        "napcat_get_friend_list".to_string(),
                        "napcat_send_like".to_string(),
                        "napcat_set_friend_remark".to_string(),
                        "napcat_delete_friend".to_string(),
                        "napcat_set_qq_profile".to_string(),
                        // NapCatQQ - Group tools
                        "napcat_get_group_list".to_string(),
                        "napcat_get_group_info".to_string(),
                        "napcat_get_group_member_list".to_string(),
                        "napcat_get_group_member_info".to_string(),
                        "napcat_set_group_kick".to_string(),
                        "napcat_set_group_ban".to_string(),
                        "napcat_set_group_whole_ban".to_string(),
                        "napcat_set_group_admin".to_string(),
                        "napcat_set_group_card".to_string(),
                        "napcat_set_group_name".to_string(),
                        "napcat_set_group_special_title".to_string(),
                        "napcat_set_group_leave".to_string(),
                        // NapCatQQ - Message tools
                        "napcat_delete_msg".to_string(),
                        "napcat_get_msg".to_string(),
                        "napcat_set_friend_add_request".to_string(),
                        "napcat_set_group_add_request".to_string(),
                        "napcat_get_cookies".to_string(),
                        "napcat_get_csrf_token".to_string(),
                        // NapCatQQ - Extend tools
                        "napcat_get_forward_msg".to_string(),
                        "napcat_set_msg_emoji_like".to_string(),
                        "napcat_mark_msg_as_read".to_string(),
                        "napcat_set_essence_msg".to_string(),
                        "napcat_delete_essence_msg".to_string(),
                        "napcat_get_essence_msg_list".to_string(),
                        "napcat_get_group_at_all_remain".to_string(),
                        "napcat_get_image".to_string(),
                        "napcat_get_record".to_string(),
                        "napcat_download_file".to_string(),
                    ]),
                ),
                (
                    "SystemControl".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "system_info".to_string(),
                        "capability_evolve".to_string(),
                        "app_control".to_string(),
                        "camera_capture".to_string(),
                        "browse".to_string(),
                        "image_understand".to_string(),
                        "termux_api".to_string(),
                    ]),
                ),
                (
                    "Organization".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "cron".to_string(),
                        "memory_forget".to_string(),
                        "knowledge_graph".to_string(),
                        "list_tasks".to_string(),
                        "spawn".to_string(),
                        "list_skills".to_string(),
                        "memory_maintenance".to_string(),
                        "community_hub".to_string(),
                    ]),
                ),
                (
                    "IoT".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "http_request".to_string(),
                        "cron".to_string(),
                    ]),
                ),
                (
                    "Media".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "audio_transcribe".to_string(),
                        "tts".to_string(),
                        "ocr".to_string(),
                        "image_understand".to_string(),
                        "video_process".to_string(),
                        "file_ops".to_string(),
                    ]),
                ),
                (
                    "DevOps".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "network_monitor".to_string(),
                        "encrypt".to_string(),
                        "http_request".to_string(),
                        "edit_file".to_string(),
                        "file_ops".to_string(),
                    ]),
                ),
                (
                    "Lifestyle".to_string(),
                    IntentToolEntryConfig::Tools(vec!["http_request".to_string()]),
                ),
                (
                    "Unknown".to_string(),
                    IntentToolEntryConfig::Tools(vec![
                        "edit_file".to_string(),
                        "file_ops".to_string(),
                        "office_write".to_string(),
                        "http_request".to_string(),
                        "browse".to_string(),
                        "spawn".to_string(),
                        "list_tasks".to_string(),
                        "cron".to_string(),
                        "memory_forget".to_string(),
                        "list_skills".to_string(),
                        "community_hub".to_string(),
                        "memory_maintenance".to_string(),
                        // NapCatQQ core tools for Unknown intent
                        "napcat_get_login_info".to_string(),
                        "napcat_get_group_list".to_string(),
                        "napcat_get_group_info".to_string(),
                        "napcat_get_msg".to_string(),
                        "napcat_get_forward_msg".to_string(),
                        "napcat_get_image".to_string(),
                        "napcat_get_record".to_string(),
                    ]),
                ),
            ]),
            deny_tools: Vec::new(),
        },
    );
    profiles
}

pub fn default_intent_router_option() -> Option<IntentRouterConfig> {
    Some(IntentRouterConfig::default())
}
