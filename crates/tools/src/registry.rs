use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{debug, warn};

use crate::agent::AgentTool;
use crate::agent_status::AgentStatusTool;
use crate::alert_rule::AlertRuleTool;
use crate::app_control::AppControlTool;
use crate::audio_transcribe::AudioTranscribeTool;
use crate::browser::BrowseTool;
use crate::camera::CameraCaptureTool;
use crate::chart_generate::ChartGenerateTool;
use crate::code_search::{GlobTool, GrepTool};
use crate::community_hub::CommunityHubTool;
use crate::context_remaining::GetContextRemainingTool;
use crate::cron::CronTool;
use crate::data_process::DataProcessTool;
use crate::email::EmailTool;
use crate::encrypt::EncryptTool;
use crate::evolution_workflow::EvolutionWorkflowTool;
use crate::exec::ExecTool;
use crate::exec_local::ExecLocalTool;
use crate::exec_skill_script::ExecSkillScriptTool;
use crate::file_ops::FileOpsTool;
use crate::fs::{EditFileTool, ListDirTool, ReadFileTool, WriteFileTool};
use crate::http_request::HttpRequestTool;
use crate::image_understand::ImageUnderstandTool;
use crate::knowledge_forget::KnowledgeForgetTool;
use crate::knowledge_graph::KnowledgeGraphTool;
use crate::memory::{MemoryForgetTool, MemoryManageTool, MemoryQueryTool, MemoryUpsertTool};
use crate::memory_maintenance::MemoryMaintenanceTool;
use crate::message::MessageTool;
use crate::network_monitor::NetworkMonitorTool;
use crate::ocr::OcrTool;
use crate::office_write::OfficeWriteTool;
use crate::plan::UpdatePlanTool;
use crate::send_message::SendMessageTool;
use crate::session_recall::SessionRecallTool;
use crate::session_search::SessionSearchTool;
use crate::shell_session::ShellTool;
use crate::skills::{ListSkillsTool, SkillManageTool, SkillViewTool};
use crate::spawn::SpawnTool;
use crate::stream_subscribe::StreamSubscribeTool;
use crate::system_info::{CapabilityEvolveTool, SystemInfoTool};
use crate::tasks::ListTasksTool;
use crate::termux_api::TermuxApiTool;
use crate::toggle_manage::ToggleManageTool;
use crate::tts::TtsTool;
use crate::video_process::VideoProcessTool;
use crate::web::{WebFetchTool, WebSearchTool};
use crate::{Tool, ToolContext};

pub const GLOBAL_CORE_TOOL_NAMES: &[&str] = &[
    "memory_query",
    "memory_manage",
    "memory_upsert",
    "memory_forget",
    "knowledge_forget",
    "spawn",
    "agent",
    "list_tasks",
    "agent_status",
    "list_skills",
    "skill_view",
    "skill_manage",
    "session_search",
    "cron",
    "toggle_manage",
    "web_fetch",
    "session_recall",
];

/// 全局核心工具名称列表。
///
/// 已废弃：请直接使用 `GLOBAL_CORE_TOOL_NAMES` 常量。
#[deprecated(note = "直接使用 GLOBAL_CORE_TOOL_NAMES 常量")]
pub fn global_core_tool_names() -> &'static [&'static str] {
    GLOBAL_CORE_TOOL_NAMES
}

#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    model_hidden_tools: HashSet<String>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            model_hidden_tools: HashSet::new(),
        }
    }

    pub fn with_defaults() -> Self {
        let mut registry = Self::new();

        // File system tools
        registry.register(Arc::new(ReadFileTool::default()));
        registry.register(Arc::new(WriteFileTool::default()));
        registry.register(Arc::new(EditFileTool::default()));
        registry.register(Arc::new(ListDirTool::default()));
        registry.register(Arc::new(GrepTool));
        registry.register(Arc::new(GlobTool));
        registry.register(Arc::new(UpdatePlanTool));
        registry.register(Arc::new(GetContextRemainingTool));

        // Exec tool
        registry.register(Arc::new(ExecTool::default()));
        registry.register(Arc::new(ShellTool));
        registry.register(Arc::new(ExecLocalTool));
        registry.register(Arc::new(ExecSkillScriptTool));

        // Web tools
        registry.register(Arc::new(WebSearchTool));
        registry.register(Arc::new(WebFetchTool));

        // Communication tools
        registry.register(Arc::new(MessageTool));
        registry.register(Arc::new(SpawnTool));

        // Agent tool (typed agents and fork mode)
        registry.register(Arc::new(AgentTool));

        // Task management
        registry.register(Arc::new(ListTasksTool));
        registry.register(Arc::new(SendMessageTool));

        // Browser tools
        registry.register(Arc::new(BrowseTool));

        // Scheduler tools
        registry.register(Arc::new(CronTool));

        // Memory tools
        registry.register(Arc::new(MemoryQueryTool));
        registry.register(Arc::new(MemoryManageTool));
        registry.register(Arc::new(MemoryUpsertTool));
        registry.register(Arc::new(MemoryForgetTool));
        registry.register(Arc::new(KnowledgeForgetTool));
        registry.register(Arc::new(SessionSearchTool));

        // Skill evolution tools
        registry.register(Arc::new(ListSkillsTool));
        registry.register(Arc::new(SkillViewTool));
        registry.register(Arc::new(SkillManageTool));

        // System info & capability evolution tools
        registry.register(Arc::new(SystemInfoTool));
        registry.register(Arc::new(AgentStatusTool));
        registry.register(Arc::new(CapabilityEvolveTool));
        registry.register(Arc::new(EvolutionWorkflowTool::new()));

        // Camera tools
        registry.register(Arc::new(CameraCaptureTool));

        // General app control (any macOS app)
        registry.register(Arc::new(AppControlTool));

        // File operations (delete, rename, move, copy, compress, decompress, PDF)
        registry.register(Arc::new(FileOpsTool));

        // Structured data processing (CSV, stats, query, transform)
        registry.register(Arc::new(DataProcessTool));

        // Generic HTTP/REST API requests
        registry.register(Arc::new(HttpRequestTool));

        // Email (SMTP/IMAP)
        registry.register(Arc::new(EmailTool));

        // Audio transcription (Whisper CLI / API)
        registry.register(Arc::new(AudioTranscribeTool));

        // Chart generation (matplotlib / plotly)
        registry.register(Arc::new(ChartGenerateTool));

        // Office document generation (PPTX / DOCX / XLSX)
        registry.register(Arc::new(OfficeWriteTool));

        // Text-to-speech
        registry.register(Arc::new(TtsTool));

        // OCR (image text recognition)
        registry.register(Arc::new(OcrTool));

        // Multimodal image understanding
        registry.register(Arc::new(ImageUnderstandTool));

        // Video processing (ffmpeg)
        registry.register(Arc::new(VideoProcessTool));

        // Encryption and security utilities
        registry.register(Arc::new(EncryptTool));

        // Network monitoring and diagnostics
        registry.register(Arc::new(NetworkMonitorTool));

        // Knowledge graph (SQLite-backed)
        registry.register(Arc::new(KnowledgeGraphTool));

        // Real-time data streams (WebSocket/SSE)
        registry.register(Arc::new(StreamSubscribeTool));

        // Conditional alert rules
        registry.register(Arc::new(AlertRuleTool));

        // Community Hub (social interactions, skill discovery)
        registry.register(Arc::new(CommunityHubTool));

        // Memory maintenance (Ghost Agent memory gardening)
        registry.register(Arc::new(MemoryMaintenanceTool));

        // Toggle management (enable/disable skills and capabilities)
        registry.register(Arc::new(ToggleManageTool));

        // Termux API (Android device control via Termux)
        registry.register(Arc::new(TermuxApiTool));

        // Session response cache recall
        registry.register(Arc::new(SessionRecallTool));

        // NapCatQQ tools (conditional)
        #[cfg(feature = "napcat")]
        {
            crate::napcat::group::register_group_tools(&mut registry);
            crate::napcat::user::register_user_tools(&mut registry);
            crate::napcat::message::register_message_tools(&mut registry);
            crate::napcat::extend::register_extend_tools(&mut registry);
        }

        registry
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let schema = tool.schema();
        debug!(name = schema.name, "Registering tool");
        self.model_hidden_tools.remove(&schema.name);
        self.tools.insert(schema.name.to_string(), tool);
    }

    /// Register a tool that remains executable but is hidden from the initial
    /// model-visible tool list. Progressive MCP discovery uses this so tools
    /// can be revealed after `mcp_search_tools` returns their schema.
    pub fn register_model_hidden(&mut self, tool: Arc<dyn Tool>) {
        let schema = tool.schema();
        debug!(name = schema.name, "Registering model-hidden tool");
        self.model_hidden_tools.insert(schema.name.clone());
        self.tools.insert(schema.name, tool);
    }

    /// Register all tools exposed by an MCP server provider.
    pub async fn register_mcp_provider(
        &mut self,
        provider: &crate::mcp::provider::McpToolProvider,
    ) {
        let tools = provider.tools().await;
        for tool in tools {
            let schema = tool.schema();
            debug!(name = schema.name, server = %provider.server_name, "Registering MCP tool");
            self.tools.insert(schema.name.to_string(), tool);
        }
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    pub fn get_tool_schemas(&self) -> Vec<Value> {
        let mut schemas: Vec<Value> = self
            .tools
            .values()
            .map(|tool| {
                let schema = tool.schema();
                json!({
                    "type": "function",
                    "function": {
                        "name": schema.name,
                        "description": schema.description,
                        "parameters": schema.parameters
                    }
                })
            })
            .collect();
        sort_schemas_by_name(&mut schemas);
        schemas
    }

    /// Get tool schemas filtered by a list of tool names.
    /// Only returns schemas for tools whose names are in the provided list.
    pub fn get_filtered_schemas(&self, names: &[&str]) -> Vec<Value> {
        let mut schemas: Vec<Value> = self
            .tools
            .iter()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .map(|(_, tool)| {
                let schema = tool.schema();
                json!({
                    "type": "function",
                    "function": {
                        "name": schema.name,
                        "description": schema.description,
                        "parameters": schema.parameters
                    }
                })
            })
            .collect();
        sort_schemas_by_name(&mut schemas);
        schemas
    }

    /// Get tiered schemas: full schemas for core tools, lightweight (name+description only) for others.
    /// Core tools and any tools with required parameters get complete parameter schemas;
    /// non-core tools without required parameters get just name+description so the LLM
    /// knows they exist but we save ~500 tokens per low-risk tool.
    /// When the LLM tries to call a lightweight tool, the runtime dynamically supplements
    /// the full schema and retries.
    pub fn get_tiered_schemas(&self, names: &[&str], core_tools: &[&str]) -> Vec<Value> {
        let mut schemas: Vec<Value> = self
            .tools
            .iter()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .map(|(name, tool)| {
                let schema = tool.schema();
                let has_required_params = schema
                    .parameters
                    .get("required")
                    .and_then(|required| required.as_array())
                    .is_some_and(|required| !required.is_empty());
                if core_tools.contains(&name.as_str()) || has_required_params {
                    // Full schema for core tools and tools that cannot be called safely
                    // without required parameters present in the model-visible schema.
                    json!({
                        "type": "function",
                        "function": {
                            "name": schema.name,
                            "description": schema.description,
                            "parameters": schema.parameters
                        }
                    })
                } else {
                    // Lightweight schema: keep the full description but omit parameters.
                    json!({
                        "type": "function",
                        "function": {
                            "name": schema.name,
                            "description": schema.description,
                            "parameters": { "type": "object", "properties": {} }
                        }
                    })
                }
            })
            .collect();
        sort_schemas_by_name(&mut schemas);
        schemas
    }

    /// Get all registered tool names.
    pub fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.keys().cloned().collect();
        names.sort();
        names
    }

    /// Get names of tools that should be visible in the initial model prompt.
    pub fn model_visible_tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .tools
            .keys()
            .filter(|name| !self.model_hidden_tools.contains(*name))
            .cloned()
            .collect();
        names.sort();
        names
    }

    pub fn is_model_hidden(&self, name: &str) -> bool {
        self.model_hidden_tools.contains(name)
    }

    /// Collect prompt rules from loaded tools.
    /// Returns a sorted list of markdown rule strings for tools that provide them.
    pub fn get_prompt_rules(&self, names: &[&str], ctx: &crate::PromptContext) -> Vec<String> {
        let mut rules: Vec<(String, String)> = self
            .tools
            .iter()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .filter_map(|(name, tool)| tool.prompt_rule(ctx).map(|rule| (name.clone(), rule)))
            .collect();
        rules.sort_by(|a, b| a.0.cmp(&b.0));
        rules.into_iter().map(|(_, rule)| rule).collect()
    }

    /// 工具对本次参数声明的幂等性；未注册的工具视为未知。
    pub fn idempotency(&self, name: &str, params: &Value) -> blockcell_core::Idempotency {
        self.get(name)
            .map(|tool| tool.idempotency(params))
            .unwrap_or(blockcell_core::Idempotency::Unknown)
    }

    pub async fn execute(&self, name: &str, ctx: ToolContext, params: Value) -> Result<Value> {
        let tool = self
            .get(name)
            .ok_or_else(|| Error::Tool(format!("Unknown tool: {}", name)))?;

        // Validate parameters
        if let Err(e) = tool.validate(&params) {
            warn!(tool = name, error = %e, "Tool validation failed");
            return Err(e);
        }

        // Check permissions
        let required = tool.required_permissions(&params);
        if !required.is_subset_of(&ctx.permissions) {
            warn!(tool = name, "Permission denied: insufficient permissions");
            return Err(Error::Tool(format!(
                "Permission denied: tool '{}' requires permissions that are not granted",
                name
            )));
        }

        debug!(tool = name, "Executing tool");
        tool.execute(ctx, params).await
    }
}

fn sort_schemas_by_name(schemas: &mut [Value]) {
    schemas.sort_by(|a, b| {
        let a_name = a["function"]["name"].as_str().unwrap_or_default();
        let b_name = b["function"]["name"].as_str().unwrap_or_default();
        a_name.cmp(b_name)
    });
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use blockcell_core::Result;
    use serde_json::json;

    struct NoRequiredTool;

    #[async_trait]
    impl Tool for NoRequiredTool {
        fn schema(&self) -> crate::ToolSchema {
            crate::ToolSchema {
                name: "no_required_tool".to_string(),
                description: "Tool without required parameters".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "optional_value": {
                            "type": "string",
                            "description": "Optional value"
                        }
                    }
                }),
            }
        }

        fn validate(&self, _params: &Value) -> Result<()> {
            Ok(())
        }

        async fn execute(&self, _ctx: ToolContext, _params: Value) -> Result<Value> {
            Ok(json!({"ok": true}))
        }
    }

    #[test]
    fn test_registry_new_empty() {
        let reg = ToolRegistry::new();
        assert!(reg.tool_names().is_empty());
        assert!(reg.get("read_file").is_none());
    }

    #[test]
    fn test_registry_with_defaults_has_core_tools() {
        let reg = ToolRegistry::with_defaults();
        let names = reg.tool_names();
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"write_file".to_string()));
        assert!(names.contains(&"exec".to_string()));
        assert!(names.contains(&"web_search".to_string()));
        assert!(names.contains(&"browse".to_string()));
        assert!(names.contains(&"http_request".to_string()));
        assert!(names.contains(&"toggle_manage".to_string()));
    }

    #[test]
    fn test_registry_tool_count() {
        let reg = ToolRegistry::with_defaults();
        // Should have a large number of tools registered
        assert!(reg.tool_names().len() >= 40);
    }

    #[test]
    fn test_registry_get_tool_schemas() {
        let reg = ToolRegistry::with_defaults();
        let schemas = reg.get_tool_schemas();
        assert!(!schemas.is_empty());
        // Each schema should have type=function and function.name
        for schema in &schemas {
            assert_eq!(schema["type"], "function");
            assert!(schema["function"]["name"].is_string());
            assert!(schema["function"]["description"].is_string());
        }
    }

    #[test]
    fn test_registry_schema_and_name_outputs_are_sorted() {
        let reg = ToolRegistry::with_defaults();
        let schema_names = |schemas: Vec<Value>| {
            schemas
                .into_iter()
                .map(|schema| {
                    schema["function"]["name"]
                        .as_str()
                        .expect("tool schema should have a name")
                        .to_string()
                })
                .collect::<Vec<_>>()
        };
        let assert_sorted = |values: Vec<String>| {
            let mut expected = values.clone();
            expected.sort();
            assert_eq!(values, expected);
        };

        assert_sorted(schema_names(reg.get_tool_schemas()));
        assert_sorted(schema_names(reg.get_filtered_schemas(&[
            "write_file",
            "exec",
            "read_file",
            "web_fetch",
        ])));
        assert_sorted(schema_names(reg.get_tiered_schemas(
            &["write_file", "exec", "read_file", "web_fetch"],
            GLOBAL_CORE_TOOL_NAMES,
        )));
        assert_sorted(reg.tool_names());
        assert_sorted(reg.model_visible_tool_names());
    }

    #[test]
    fn test_registry_get_filtered_schemas() {
        let reg = ToolRegistry::with_defaults();
        let filtered = reg.get_filtered_schemas(&["read_file", "exec"]);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_registry_get_filtered_schemas_empty() {
        let reg = ToolRegistry::with_defaults();
        let filtered = reg.get_filtered_schemas(&["nonexistent_tool"]);
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_registry_register_custom() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(crate::exec::ExecTool::default()));
        assert!(reg.get("exec").is_some());
        assert_eq!(reg.tool_names().len(), 1);
    }

    #[test]
    fn test_registry_model_hidden_tools_remain_executable_but_not_visible() {
        let mut reg = ToolRegistry::new();
        reg.register_model_hidden(Arc::new(NoRequiredTool));

        assert!(reg.get("no_required_tool").is_some());
        assert!(reg.tool_names().contains(&"no_required_tool".to_string()));
        assert!(!reg
            .model_visible_tool_names()
            .contains(&"no_required_tool".to_string()));
        assert!(reg.is_model_hidden("no_required_tool"));
    }

    #[test]
    fn test_hidden_tool_schema_can_be_requested_after_discovery() {
        let mut reg = ToolRegistry::new();
        reg.register_model_hidden(Arc::new(NoRequiredTool));

        let schemas = reg.get_tiered_schemas(&["no_required_tool"], GLOBAL_CORE_TOOL_NAMES);

        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0]["function"]["name"], "no_required_tool");
    }

    #[test]
    fn test_tiered_schemas_keep_web_fetch_full_parameters() {
        let reg = ToolRegistry::with_defaults();
        let schemas = reg.get_tiered_schemas(&["web_fetch"], GLOBAL_CORE_TOOL_NAMES);

        assert_eq!(schemas.len(), 1);
        let properties = schemas[0]["function"]["parameters"]["properties"]
            .as_object()
            .expect("web_fetch properties should be an object");
        assert!(properties.contains_key("url"));
        assert!(properties.contains_key("extractMode"));
        assert!(properties.contains_key("maxChars"));
    }

    #[test]
    fn test_tiered_schemas_keep_required_param_tools_full() {
        let reg = ToolRegistry::with_defaults();
        let schemas = reg.get_tiered_schemas(&["write_file"], GLOBAL_CORE_TOOL_NAMES);

        assert_eq!(schemas.len(), 1);
        let properties = schemas[0]["function"]["parameters"]["properties"]
            .as_object()
            .expect("write_file properties should be an object");
        assert!(properties.contains_key("path"));
        assert!(properties.contains_key("content"));
    }

    #[test]
    fn test_tiered_schemas_still_keep_no_required_tools_lightweight() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(NoRequiredTool));
        let schemas = reg.get_tiered_schemas(&["no_required_tool"], GLOBAL_CORE_TOOL_NAMES);

        assert_eq!(schemas.len(), 1);
        let properties = schemas[0]["function"]["parameters"]["properties"]
            .as_object()
            .expect("no_required_tool properties should be an object");
        assert!(properties.is_empty());
    }

    fn assert_no_array_without_items(value: &Value, path: &str) {
        match value {
            Value::Object(map) => {
                if map.get("type").and_then(Value::as_str) == Some("array") {
                    assert!(
                        map.contains_key("items"),
                        "array schema missing items at {}",
                        path
                    );
                }
                for (key, child) in map {
                    let child_path = if path.is_empty() {
                        key.to_string()
                    } else {
                        format!("{}.{}", path, key)
                    };
                    assert_no_array_without_items(child, &child_path);
                }
            }
            Value::Array(items) => {
                for (idx, child) in items.iter().enumerate() {
                    assert_no_array_without_items(child, &format!("{}[{}]", path, idx));
                }
            }
            _ => {}
        }
    }

    #[test]
    fn test_all_tool_schemas_have_valid_array_items() {
        let reg = ToolRegistry::with_defaults();
        let schemas = reg.get_tool_schemas();

        for schema in &schemas {
            let tool_name = schema["function"]["name"].as_str().unwrap_or("unknown");
            let parameters = &schema["function"]["parameters"];
            assert_no_array_without_items(parameters, tool_name);
        }
    }
}
