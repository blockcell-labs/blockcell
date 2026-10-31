use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
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
use crate::exec_jobs::{JobKillTool, JobOutputTool};
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
use crate::shell_actions::ShellActionTool;
use crate::shell_session::ShellTool;
use crate::skills::{ListSkillsTool, SkillManageTool, SkillViewTool};
use crate::spawn::SpawnTool;
use crate::stream_subscribe::StreamSubscribeTool;
use crate::system_info::{CapabilityEvolveTool, SystemInfoTool};
use crate::tasks::{CancelTaskTool, ListTasksTool};
use crate::termux_api::TermuxApiTool;
use crate::toggle_manage::ToggleManageTool;
use crate::tool_search::{ToolSearchEntry, ToolSearchTool};
use crate::tts::TtsTool;
use crate::video_process::VideoProcessTool;
use crate::web::{WebFetchTool, WebSearchTool};
use crate::{
    Tool, ToolContext, ToolDefinition, ToolPresentation, ToolPresentationConfig, ToolVisibility,
};
use blockcell_core::scope::{ScopeGuard, ScopeId, ScopeTarget, ScopeView, ScopedRegistry};

pub const GLOBAL_CORE_TOOL_NAMES: &[&str] = &[
    "memory_query",
    "memory_manage",
    "memory_upsert",
    "memory_forget",
    "knowledge_forget",
    "spawn",
    "agent",
    "list_tasks",
    "cancel_task",
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

/// 查询作用域工具时的视图（拥有所有权的 [`ScopeView`]）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolScopeView {
    pub agent_id: String,
    pub session_id: String,
    /// 正在评估的进化候选；生产视图为 `None`。
    pub candidate_id: Option<String>,
}

impl ToolScopeView {
    fn as_view(&self) -> ScopeView<'_> {
        ScopeView {
            agent_id: &self.agent_id,
            session_id: &self.session_id,
            candidate_id: self.candidate_id.as_deref(),
        }
    }
}

/// 注册在非全局作用域中的工具及其注册时的展示。
#[derive(Clone)]
pub struct ScopedTool {
    tool: Arc<dyn Tool>,
    presentation: ToolPresentation,
}

/// Provenance established by host code, never by a tool name or skill manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCapabilityOrigin {
    HostGrant,
    ScopedRegistration,
    McpAllowlist,
}

#[derive(Clone)]
pub struct ToolRegistry {
    /// 全局工具（构建 registry 时注册，对所有视图可见）。
    tools: HashMap<String, Arc<dyn Tool>>,
    /// 注册时确定的展示：工具自身声明，或以 discoverable 方式注册。
    registered_presentation: HashMap<String, ToolPresentation>,
    /// agent 配置中的展示覆盖，叠加在注册展示之上。
    presentation_overrides: HashMap<String, ToolPresentationConfig>,
    /// 按作用域（agent / session / 进化候选）注册的工具，克隆共享；同名时更具体的作用域覆盖全局工具。
    scoped: ScopedRegistry<ScopedTool>,
    /// 当前查询视图，决定哪些作用域工具可见、可执行。
    view: ToolScopeView,
    capability_origins: HashMap<String, ToolCapabilityOrigin>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            registered_presentation: HashMap::new(),
            presentation_overrides: HashMap::new(),
            scoped: ScopedRegistry::new(),
            view: ToolScopeView::default(),
            capability_origins: HashMap::new(),
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
        registry.register(Arc::new(JobOutputTool));
        registry.register(Arc::new(JobKillTool));
        registry.register(Arc::new(ShellActionTool::run()));
        registry.register(Arc::new(ShellActionTool::poll()));
        registry.register(Arc::new(ShellActionTool::input()));
        registry.register(Arc::new(ShellActionTool::close()));
        registry.register(Arc::new(ToolSearchTool));
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
        registry.register(Arc::new(CancelTaskTool));
        registry.register(Arc::new(SendMessageTool));

        // Browser tools
        registry.register(Arc::new(BrowseTool::default()));

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
        let presentation = tool.presentation();
        self.insert_tool(tool, presentation);
    }

    /// Register an extension and explicitly grant it to this agent registry.
    /// Runtime deny, scope, toggle and execution policies still apply.
    pub fn register_host_granted(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.schema().name;
        self.register(tool);
        self.capability_origins
            .insert(name, ToolCapabilityOrigin::HostGrant);
    }

    /// Register a genuine MCP implementation; runtime rechecks the agent allowlist.
    pub fn register_mcp(&mut self, tool: Arc<dyn Tool>, model_hidden: bool) {
        let name = tool.schema().name;
        if model_hidden {
            self.register_model_hidden(tool);
        } else {
            self.register(tool);
        }
        self.capability_origins
            .insert(name, ToolCapabilityOrigin::McpAllowlist);
    }

    pub fn capability_grants(&self) -> HashMap<String, ToolCapabilityOrigin> {
        let mut grants = self.capability_origins.clone();
        for name in self.visible_scoped().into_keys() {
            grants.insert(name, ToolCapabilityOrigin::ScopedRegistration);
        }
        grants
    }

    /// Register a tool that remains executable but is hidden from the initial
    /// model-visible tool list. Progressive MCP discovery uses this so tools
    /// can be revealed after `mcp_search_tools` returns their schema.
    pub fn register_model_hidden(&mut self, tool: Arc<dyn Tool>) {
        let presentation = ToolPresentation {
            visibility: ToolVisibility::Discoverable,
            ..tool.presentation()
        };
        self.insert_tool(tool, presentation);
    }

    fn insert_tool(&mut self, tool: Arc<dyn Tool>, presentation: ToolPresentation) {
        let name = tool.schema().name;
        // A replacement implementation cannot inherit a previous host grant.
        self.capability_origins.remove(&name);
        debug!(name = %name, visibility = ?presentation.visibility, "Registering tool");
        self.registered_presentation
            .insert(name.clone(), presentation);
        self.tools.insert(name, tool);
    }

    /// 在指定作用域注册工具，返回的 guard 销毁时自动注销（例如只在某次候选评估或会话期间生效）。
    ///
    /// 作用域工具只对可见的视图出现在工具列表中、也只有这些视图能执行它；
    /// 克隆出的 registry 共享作用域工具，但各自持有自己的视图。
    pub fn register_scoped(
        &self,
        target: ScopeTarget,
        tool: Arc<dyn Tool>,
    ) -> ScopeGuard<ScopedTool> {
        let presentation = tool.presentation();
        debug!(name = %tool.schema().name, scope = ?target, "Registering scoped tool");
        self.scoped
            .register(target, ScopedTool { tool, presentation })
    }

    /// 与 [`Self::register_scoped`] 相同，但长期存在，需用 [`Self::unregister_scoped`] 显式注销。
    pub fn insert_scoped(&self, target: ScopeTarget, tool: Arc<dyn Tool>) -> ScopeId {
        let presentation = tool.presentation();
        self.scoped
            .insert(target, ScopedTool { tool, presentation })
    }

    pub fn unregister_scoped(&self, id: ScopeId) -> bool {
        self.scoped.remove(id).is_some()
    }

    /// 当前视图下可见的**作用域工具**名称（经 `register_scoped` / `insert_scoped` 注册）。
    ///
    /// 这些工具由宿主代码注册（进化候选、会话级工具、嵌入方扩展），名字不来自用户消息，
    /// 因此可以作为能力来源；与之相对，skill 声明与 `tool_search` 结果只能改变曝光。
    pub fn scoped_tool_names(&self) -> Vec<String> {
        self.visible_scoped().into_keys().collect()
    }

    /// 设置查询视图（agent / session / 进化候选）。
    pub fn set_scope_view(&mut self, view: ToolScopeView) {
        self.view = view;
    }

    pub fn scope_view(&self) -> &ToolScopeView {
        &self.view
    }

    /// 当前视图可见的作用域工具；同名时取最具体的作用域，同作用域取后注册者。
    fn visible_scoped(&self) -> HashMap<String, ScopedTool> {
        if self.scoped.is_empty() {
            return HashMap::new();
        }
        let mut chosen: HashMap<String, (u8, ScopedTool)> = HashMap::new();
        for (target, entry) in self.scoped.visible_with_target(self.view.as_view()) {
            let specificity = target.specificity();
            let name = entry.tool.schema().name;
            match chosen.get(&name) {
                Some((existing, _)) if *existing > specificity => {}
                _ => {
                    chosen.insert(name, (specificity, entry));
                }
            }
        }
        chosen
            .into_iter()
            .map(|(name, (_, entry))| (name, entry))
            .collect()
    }

    /// 当前视图下的有效工具集合：全局工具叠加可见的作用域工具。
    fn effective_tools(&self) -> HashMap<String, Arc<dyn Tool>> {
        let mut tools = self.tools.clone();
        tools.extend(
            self.visible_scoped()
                .into_iter()
                .map(|(name, entry)| (name, entry.tool)),
        );
        tools
    }

    /// 设置 agent 配置中的展示覆盖（描述、可见性、分组、排序），不影响能否执行。
    pub fn set_presentation_overrides(
        &mut self,
        overrides: HashMap<String, ToolPresentationConfig>,
    ) {
        self.presentation_overrides = overrides;
    }

    /// 工具最终的展示方式：注册展示叠加 agent 配置覆盖。未注册的工具返回 `None`。
    pub fn presentation(&self, name: &str) -> Option<ToolPresentation> {
        let base = match self.visible_scoped().remove(name) {
            Some(entry) => entry.presentation,
            None => self.registered_presentation.get(name)?.clone(),
        };
        Some(match self.presentation_overrides.get(name) {
            Some(overrides) => base.with_overrides(overrides),
            None => base,
        })
    }

    /// 模型看到的 function schema；描述可被展示层覆盖。`include_parameters` 为假时
    /// 只保留名称与描述（轻量 schema）。
    fn model_schema(&self, tool: &Arc<dyn Tool>, include_parameters: bool) -> Value {
        let schema = tool.schema();
        let description = self
            .presentation(&schema.name)
            .and_then(|presentation| presentation.description)
            .unwrap_or(schema.description);
        let parameters = if include_parameters {
            let mut parameters = schema.parameters;
            if tool.accepts_execution_annotations() {
                crate::evidence::add_annotation_schema(
                    &mut parameters,
                    tool.supports_verification(),
                );
            }
            parameters
        } else {
            json!({ "type": "object", "properties": {} })
        };
        json!({
            "type": "function",
            "function": {
                "name": schema.name,
                "description": description,
                "parameters": parameters
            }
        })
    }

    /// 单个工具的完整 model-facing schema（渐进发现、schema 补全时使用）。
    pub fn full_model_schema(&self, name: &str) -> Option<Value> {
        self.get(name).map(|tool| self.model_schema(&tool, true))
    }

    pub fn tool_search_entries(&self, names: &[String]) -> Vec<ToolSearchEntry> {
        names
            .iter()
            .filter_map(|name| {
                let schema = self.full_model_schema(name)?;
                Some(ToolSearchEntry {
                    name: name.clone(),
                    description: schema["function"]["description"]
                        .as_str()
                        .unwrap_or_default()
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                })
            })
            .collect()
    }

    pub fn sort_model_schemas(&self, schemas: &mut [Value]) {
        self.sort_schemas(schemas);
    }

    pub fn coding_exposure(&self, name: &str) -> crate::base_packs::CodingExposure {
        self.get(name)
            .map(|tool| tool.coding_exposure())
            .unwrap_or_default()
    }

    pub fn get_coding_schemas(&self, names: &[&str]) -> Vec<Value> {
        let mut schemas: Vec<_> = self
            .effective_tools()
            .iter()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .map(|(_, tool)| self.model_schema(tool, tool.coding_exposure().full_schema))
            .collect();
        self.sort_schemas(&mut schemas);
        schemas
    }

    pub fn get_tiered_schemas_core_only(&self, names: &[&str], core_tools: &[&str]) -> Vec<Value> {
        let mut schemas: Vec<Value> = self
            .effective_tools()
            .iter()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .map(|(name, tool)| self.model_schema(tool, core_tools.contains(&name.as_str())))
            .collect();
        self.sort_schemas(&mut schemas);
        schemas
    }

    /// 按展示排序（order 小者在前，相同时按名称）。
    fn sort_schemas(&self, schemas: &mut [Value]) {
        let key = |schema: &Value| {
            let name = schema["function"]["name"].as_str().unwrap_or_default();
            let order = self.presentation(name).map_or(0, |p| p.order);
            (order, name.to_string())
        };
        schemas.sort_by_cached_key(key);
    }

    /// Register all tools exposed by an MCP server provider.
    pub async fn register_mcp_provider(
        &mut self,
        provider: &crate::mcp::provider::McpToolProvider,
    ) {
        let tools = provider.tools().await;
        for tool in tools {
            debug!(name = %tool.schema().name, server = %provider.server_name, "Registering MCP tool");
            self.register_mcp(tool, false);
        }
    }

    /// 当前视图下名为 `name` 的工具：可见的作用域工具优先于全局工具。
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        match self.visible_scoped().remove(name) {
            Some(entry) => Some(entry.tool),
            None => self.tools.get(name).cloned(),
        }
    }

    pub fn get_tool_schemas(&self) -> Vec<Value> {
        let mut schemas: Vec<Value> = self
            .effective_tools()
            .values()
            .map(|tool| self.model_schema(tool, true))
            .collect();
        self.sort_schemas(&mut schemas);
        schemas
    }

    /// Get tool schemas filtered by a list of tool names.
    /// Only returns schemas for tools whose names are in the provided list.
    pub fn get_filtered_schemas(&self, names: &[&str]) -> Vec<Value> {
        let mut schemas: Vec<Value> = self
            .effective_tools()
            .iter()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .map(|(_, tool)| self.model_schema(tool, true))
            .collect();
        self.sort_schemas(&mut schemas);
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
            .effective_tools()
            .iter()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .map(|(name, tool)| {
                let parameters = tool.schema().parameters;
                let has_required_params = parameters
                    .get("required")
                    .and_then(|required| required.as_array())
                    .is_some_and(|required| !required.is_empty())
                    || parameters
                        .get("oneOf")
                        .and_then(Value::as_array)
                        .is_some_and(|alternatives| {
                            alternatives.iter().any(|alternative| {
                                alternative
                                    .get("required")
                                    .and_then(Value::as_array)
                                    .is_some_and(|required| !required.is_empty())
                            })
                        });
                // Full schema for core tools and tools that cannot be called safely
                // without required parameters present in the model-visible schema.
                // Lightweight schema: keep the full description but omit parameters.
                self.model_schema(
                    tool,
                    core_tools.contains(&name.as_str()) || has_required_params,
                )
            })
            .collect();
        self.sort_schemas(&mut schemas);
        schemas
    }

    /// Get all registered tool names.
    pub fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.effective_tools().into_keys().collect();
        names.sort();
        names
    }

    /// Get names of tools that should be visible in the initial model prompt.
    pub fn model_visible_tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .effective_tools()
            .into_keys()
            .filter(|name| self.presentation(name).is_some_and(|p| p.is_visible()))
            .collect();
        names.sort();
        names
    }

    pub fn is_model_hidden(&self, name: &str) -> bool {
        self.presentation(name).is_some_and(|p| !p.is_visible())
    }

    /// Collect prompt rules from loaded tools.
    /// Returns a sorted list of markdown rule strings for tools that provide them.
    pub fn get_prompt_rules(&self, names: &[&str], ctx: &crate::PromptContext) -> Vec<String> {
        let mut rules: Vec<(String, String)> = self
            .effective_tools()
            .into_iter()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .filter_map(|(name, tool)| tool.prompt_rule(ctx).map(|rule| (name, rule)))
            .collect();
        if names.iter().any(|name| {
            self.get(name)
                .is_some_and(|tool| tool.accepts_execution_annotations())
        }) {
            rules.push(("execution_annotations".into(), "- **Task requirements and evidence**: When the user names required output files or constrains verification, record those requirements with `task_contract` on your first execution/write call or `update_plan`. Record only explicit requirements; current user instructions take precedence. Never run tests/builds/checks that the user prohibits or defers. When executing a check, set `verification` with its kind and target, preserve failure exit status, and report exactly what was checked; the kind must match the recognized runner. A declaration alone or an unknown custom script does not establish verification evidence. Under a forbidden/deferred contract, use file tools or simple read-only commands; do not switch to scripts, shell input or delegated agents.".into()));
        }
        rules.sort_by(|a, b| a.0.cmp(&b.0));
        rules.into_iter().map(|(_, rule)| rule).collect()
    }

    /// 当前视图下工具对本次参数的三层定义；未注册的工具返回 `None`。
    pub fn definition(&self, name: &str, params: &Value) -> Option<ToolDefinition> {
        let tool = self.get(name)?;
        Some(ToolDefinition {
            schema: tool.schema(),
            execution: tool.execution_spec(params),
            presentation: self.presentation(name)?,
        })
    }

    /// 工具对本次参数声明的幂等性；未注册的工具视为未知。
    pub fn idempotency(&self, name: &str, params: &Value) -> blockcell_core::Idempotency {
        self.get(name)
            .map(|tool| tool.execution_spec(params).idempotency)
            .unwrap_or(blockcell_core::Idempotency::Unknown)
    }

    /// 工具对本次参数声明的执行环境；未注册的工具按进程内纯计算对待。
    pub fn execution_environment(&self, name: &str, params: &Value) -> crate::ExecutionEnvironment {
        self.get(name)
            .map(|tool| tool.execution_environment(params))
            .unwrap_or(crate::ExecutionEnvironment::InProcess)
    }

    /// 本次调用是否需要用户显式确认；未注册的工具按无需确认对待。
    pub fn requires_confirmation(&self, name: &str, params: &Value) -> bool {
        self.get(name)
            .map(|tool| tool.requires_confirmation(params))
            .unwrap_or(false)
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
        let required = tool.execution_spec(&params).permissions;
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
    fn strict_tiering_only_expands_the_coding_core_set() {
        let reg = ToolRegistry::with_defaults();
        let names = reg.tool_names();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let core = ["read_file", "write_file", "exec", "tool_search"];
        let schemas = reg.get_tiered_schemas_core_only(&refs, &core);
        for schema in schemas {
            let name = schema["function"]["name"].as_str().unwrap();
            let properties = schema["function"]["parameters"]["properties"]
                .as_object()
                .unwrap();
            if !properties.is_empty() {
                assert!(
                    core.contains(&name),
                    "{name} unexpectedly has a full schema"
                );
            }
        }
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

    #[test]
    fn test_presentation_overrides_change_model_view_but_not_execution() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(NoRequiredTool));
        reg.register(Arc::new(crate::exec::ExecTool::default()));
        reg.set_presentation_overrides(HashMap::from([
            (
                "no_required_tool".to_string(),
                ToolPresentationConfig {
                    description: Some("Agent-specific description".to_string()),
                    order: Some(-1),
                    ..Default::default()
                },
            ),
            (
                "exec".to_string(),
                ToolPresentationConfig {
                    visibility: Some(ToolVisibility::Discoverable),
                    group: Some("system".to_string()),
                    ..Default::default()
                },
            ),
        ]));

        let schemas = reg.get_tool_schemas();
        assert_eq!(schemas[0]["function"]["name"], "no_required_tool");
        assert_eq!(
            schemas[0]["function"]["description"],
            "Agent-specific description"
        );
        assert_eq!(
            reg.full_model_schema("no_required_tool").unwrap()["function"]["description"],
            "Agent-specific description"
        );

        assert!(reg.is_model_hidden("exec"));
        assert_eq!(reg.model_visible_tool_names(), vec!["no_required_tool"]);
        assert!(reg.get("exec").is_some());
        assert_eq!(
            reg.presentation("exec").unwrap().group.as_deref(),
            Some("system")
        );
    }

    #[test]
    fn test_presentation_override_can_reveal_discoverable_tool() {
        let mut reg = ToolRegistry::new();
        reg.register_model_hidden(Arc::new(NoRequiredTool));
        reg.set_presentation_overrides(HashMap::from([(
            "no_required_tool".to_string(),
            ToolPresentationConfig {
                visibility: Some(ToolVisibility::Visible),
                ..Default::default()
            },
        )]));

        assert!(!reg.is_model_hidden("no_required_tool"));
        assert!(reg.presentation("missing").is_none());
    }

    struct NamedTool(&'static str, &'static str);

    #[async_trait]
    impl Tool for NamedTool {
        fn schema(&self) -> crate::ToolSchema {
            crate::ToolSchema {
                name: self.0.to_string(),
                description: self.1.to_string(),
                parameters: json!({ "type": "object", "properties": {} }),
            }
        }

        fn validate(&self, _params: &Value) -> Result<()> {
            Ok(())
        }

        async fn execute(&self, _ctx: ToolContext, _params: Value) -> Result<Value> {
            Ok(json!({ "by": self.1 }))
        }
    }

    fn scope_view(agent: &str, session: &str, candidate: Option<&str>) -> ToolScopeView {
        ToolScopeView {
            agent_id: agent.into(),
            session_id: session.into(),
            candidate_id: candidate.map(str::to_string),
        }
    }

    fn description(reg: &ToolRegistry, name: &str) -> Option<String> {
        reg.full_model_schema(name)
            .map(|schema| schema["function"]["description"].as_str().unwrap().into())
    }

    #[test]
    fn capability_grants_require_explicit_host_authority() {
        let mut reg = ToolRegistry::with_defaults();
        assert!(reg.capability_grants().is_empty());
        reg.register(Arc::new(NamedTool("custom", "implementation only")));
        reg.register(Arc::new(NamedTool("fake__tool", "not an MCP registration")));
        assert!(reg.capability_grants().is_empty());
        reg.register_host_granted(Arc::new(NamedTool("custom", "host granted")));
        assert_eq!(
            reg.capability_grants().get("custom"),
            Some(&ToolCapabilityOrigin::HostGrant)
        );
        assert!(!reg.capability_grants().contains_key("fake__tool"));
    }

    #[test]
    fn capability_grants_are_revoked_when_implementation_is_replaced() {
        let mut reg = ToolRegistry::new();
        reg.register_host_granted(Arc::new(NamedTool("custom", "first")));
        reg.register(Arc::new(NamedTool("custom", "ordinary replacement")));
        assert!(!reg.capability_grants().contains_key("custom"));
        reg.register_host_granted(Arc::new(NamedTool("custom", "second")));
        reg.register_model_hidden(Arc::new(NamedTool("custom", "hidden replacement")));
        assert!(!reg.capability_grants().contains_key("custom"));
        assert!(reg.is_model_hidden("custom"));
    }

    #[test]
    fn capability_grants_record_mcp_provenance_independently_of_visibility() {
        let mut reg = ToolRegistry::new();
        reg.register_mcp(Arc::new(NamedTool("server__direct", "direct")), false);
        reg.register_mcp(Arc::new(NamedTool("server__hidden", "hidden")), true);
        for name in ["server__direct", "server__hidden"] {
            assert_eq!(
                reg.capability_grants().get(name),
                Some(&ToolCapabilityOrigin::McpAllowlist)
            );
        }
        assert!(!reg.is_model_hidden("server__direct"));
        assert!(reg.is_model_hidden("server__hidden"));
        reg.register(Arc::new(NamedTool("server__direct", "replacement")));
        assert!(!reg.capability_grants().contains_key("server__direct"));
    }

    #[test]
    fn capability_grants_follow_scoped_view_and_guard_lifetime() {
        let mut reg = ToolRegistry::new();
        reg.set_scope_view(scope_view("ops", "cli:1", None));
        let guard = reg.register_scoped(
            ScopeTarget::Session("cli:1".into()),
            Arc::new(NamedTool("session_tool", "scoped")),
        );
        assert_eq!(
            reg.capability_grants().get("session_tool"),
            Some(&ToolCapabilityOrigin::ScopedRegistration)
        );
        reg.set_scope_view(scope_view("ops", "cli:2", None));
        assert!(!reg.capability_grants().contains_key("session_tool"));
        reg.set_scope_view(scope_view("ops", "cli:1", None));
        drop(guard);
        assert!(!reg.capability_grants().contains_key("session_tool"));
    }

    #[test]
    fn capability_grants_restore_global_origin_after_scoped_override() {
        let mut reg = ToolRegistry::new();
        reg.register_host_granted(Arc::new(NamedTool("custom", "global")));
        reg.set_scope_view(scope_view("ops", "cli:1", None));
        let guard = reg.register_scoped(
            ScopeTarget::Session("cli:1".into()),
            Arc::new(NamedTool("custom", "scoped")),
        );
        assert_eq!(
            reg.capability_grants().get("custom"),
            Some(&ToolCapabilityOrigin::ScopedRegistration)
        );
        drop(guard);
        assert_eq!(
            reg.capability_grants().get("custom"),
            Some(&ToolCapabilityOrigin::HostGrant)
        );
    }

    #[test]
    fn scoped_tools_are_visible_only_to_matching_views() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(NamedTool("shared", "global")));
        let _agent = reg.register_scoped(
            ScopeTarget::Agent("ops".into()),
            Arc::new(NamedTool("deploy", "agent")),
        );
        let _session = reg.register_scoped(
            ScopeTarget::Session("cli:1".into()),
            Arc::new(NamedTool("session_only", "session")),
        );

        assert_eq!(reg.tool_names(), vec!["shared"]);
        assert!(reg.get("deploy").is_none());

        reg.set_scope_view(scope_view("ops", "cli:1", None));
        assert_eq!(reg.tool_names(), vec!["deploy", "session_only", "shared"]);
        assert_eq!(
            reg.model_visible_tool_names(),
            vec!["deploy", "session_only", "shared"]
        );
        assert_eq!(reg.get_filtered_schemas(&["deploy"]).len(), 1);

        // 克隆共享作用域工具，但视图独立：子 agent 不继承父 agent 作用域
        let mut child = reg.clone();
        child.set_scope_view(scope_view("ops/child", "cli:2", None));
        assert_eq!(child.tool_names(), vec!["shared"]);
    }

    #[test]
    fn more_specific_scope_overrides_global_tool_and_guard_restores_it() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(NamedTool("deploy", "global")));
        reg.set_scope_view(scope_view("ops", "cli:1", Some("evo-1")));
        {
            let _agent = reg.register_scoped(
                ScopeTarget::Agent("ops".into()),
                Arc::new(NamedTool("deploy", "agent")),
            );
            let _candidate = reg.register_scoped(
                ScopeTarget::Candidate("evo-1".into()),
                Arc::new(NamedTool("deploy", "candidate")),
            );
            assert_eq!(description(&reg, "deploy").as_deref(), Some("candidate"));
            assert_eq!(reg.tool_names(), vec!["deploy"]);

            reg.set_scope_view(scope_view("ops", "cli:1", None));
            assert_eq!(description(&reg, "deploy").as_deref(), Some("agent"));
        }
        assert_eq!(description(&reg, "deploy").as_deref(), Some("global"));

        let id = reg.insert_scoped(
            ScopeTarget::Session("cli:1".into()),
            Arc::new(NamedTool("deploy", "session")),
        );
        assert_eq!(description(&reg, "deploy").as_deref(), Some("session"));
        assert!(reg.unregister_scoped(id));
        assert_eq!(description(&reg, "deploy").as_deref(), Some("global"));
    }

    #[test]
    fn definition_combines_schema_execution_spec_and_presentation() {
        let mut reg = ToolRegistry::with_defaults();
        reg.set_presentation_overrides(HashMap::from([(
            "http_request".to_string(),
            ToolPresentationConfig {
                group: Some("network".to_string()),
                ..Default::default()
            },
        )]));

        let get = reg
            .definition(
                "http_request",
                &json!({"url": "https://example.com", "method": "GET"}),
            )
            .unwrap();
        assert_eq!(get.schema.name, "http_request");
        assert_eq!(get.presentation.group.as_deref(), Some("network"));
        assert_eq!(
            get.execution.idempotency,
            reg.idempotency(
                "http_request",
                &json!({"url": "https://example.com", "method": "GET"})
            )
        );
        assert!(reg.definition("missing", &json!({})).is_none());
    }

    #[test]
    fn scoped_tool_keeps_its_presentation() {
        let mut reg = ToolRegistry::new();
        let _guard =
            reg.register_scoped(ScopeTarget::Agent("ops".into()), Arc::new(NoRequiredTool));
        reg.set_scope_view(scope_view("ops", "s", None));
        assert!(reg.presentation("no_required_tool").is_some());
        assert!(!reg.is_model_hidden("no_required_tool"));
        assert!(
            reg.idempotency("no_required_tool", &json!({})) == blockcell_core::Idempotency::Unknown
        );
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

    #[test]
    fn test_execution_environment_declared_per_tool() {
        use crate::ExecutionEnvironment::{self, *};
        let reg = ToolRegistry::with_defaults();
        let cases: &[(&str, Value, ExecutionEnvironment)] = &[
            ("read_file", json!({"path": "a"}), Local),
            ("list_dir", json!({"path": "."}), Local),
            ("grep", json!({"pattern": "x"}), Local),
            ("glob", json!({"pattern": "*"}), Local),
            ("session_search", json!({"query": "x"}), Local),
            (
                "write_file",
                json!({"path": "a", "content": "x"}),
                WorkspaceWrite,
            ),
            ("edit_file", json!({"path": "a"}), WorkspaceWrite),
            (
                "file_ops",
                json!({"action": "delete", "path": "a"}),
                WorkspaceWrite,
            ),
            // 默认 exec 受 workspace-write 沙箱限制 → Shell；exec 不接受 per-call
            // sandbox_policy（仅 config 可放宽），参数被忽略 → 仍声明 Shell
            ("exec", json!({"command": "ls"}), Shell),
            (
                "exec",
                json!({"command": "ls", "sandbox_policy": "full-access"}),
                Shell,
            ),
            ("exec_local", json!({"path": "scripts/run.py"}), FullAccess),
            (
                "exec_skill_script",
                json!({"path": "scripts/run.py"}),
                FullAccess,
            ),
            ("shell", json!({"command": "ls"}), Shell),
            (
                "shell",
                json!({"command": "ls", "sandbox_policy": "full-access"}),
                FullAccess,
            ),
            ("web_fetch", json!({"url": "https://example.com"}), Network),
            ("web_search", json!({"query": "x"}), Network),
            (
                "http_request",
                json!({"url": "https://example.com"}),
                Network,
            ),
            ("email", json!({"action": "list"}), Network),
            ("message", json!({"text": "hi"}), Network),
            // 未注册 / 未声明的工具 → 进程内纯计算
            ("update_plan", json!({}), InProcess),
            ("definitely_not_a_tool", json!({}), InProcess),
        ];
        for (name, params, expected) in cases {
            assert_eq!(
                reg.execution_environment(name, params),
                *expected,
                "unexpected environment for {name} with {params}"
            );
        }
    }

    #[test]
    fn test_requires_confirmation_is_declared_per_call_not_by_environment() {
        let reg = ToolRegistry::with_defaults();
        // 只有 full-access 的 shell 需要逐次确认……
        assert!(reg.requires_confirmation(
            "shell",
            &json!({"command": "ls", "sandbox_policy": "full-access"})
        ));
        // ……受限 shell 不需要……
        assert!(!reg.requires_confirmation("shell", &json!({"command": "ls"})));
        // ……而同样以 full-access 运行的 exec 也不需要（危险命令走命令级策略）。
        assert!(!reg.requires_confirmation("exec", &json!({"command": "ls"})));
        assert!(!reg.requires_confirmation("read_file", &json!({"path": "a"})));
        assert!(!reg.requires_confirmation("definitely_not_a_tool", &json!({})));
    }
}
