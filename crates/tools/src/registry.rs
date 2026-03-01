use std::collections::HashMap;
use std::sync::Arc;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::{Tool, ToolContext};
use crate::fs::{ReadFileTool, WriteFileTool, EditFileTool, ListDirTool};
use crate::exec::ExecTool;
use crate::web::{WebSearchTool, WebFetchTool};
use crate::message::MessageTool;
use crate::spawn::SpawnTool;
use crate::cron::CronTool;
use crate::tasks::ListTasksTool;
use crate::browser::BrowseTool;
use crate::memory::{MemoryQueryTool, MemoryUpsertTool, MemoryForgetTool};
use crate::skills::ListSkillsTool;
use crate::system_info::{SystemInfoTool, CapabilityEvolveTool};
use crate::camera::CameraCaptureTool;

use crate::app_control::AppControlTool;
use crate::file_ops::FileOpsTool;
use crate::data_process::DataProcessTool;
use crate::http_request::HttpRequestTool;
use crate::email::EmailTool;
use crate::audio_transcribe::AudioTranscribeTool;
use crate::chart_generate::ChartGenerateTool;
use crate::office_write::OfficeWriteTool;
use crate::calendar_api::CalendarApiTool;
use crate::iot_control::IotControlTool;
use crate::tts::TtsTool;
use crate::ocr::OcrTool;
use crate::image_understand::ImageUnderstandTool;
use crate::social_media::SocialMediaTool;
use crate::notification::NotificationTool;
use crate::cloud_api::CloudApiTool;
use crate::git_api::GitApiTool;
use crate::finance_api::FinanceApiTool;
use crate::video_process::VideoProcessTool;
use crate::health_api::HealthApiTool;
use crate::map_api::MapApiTool;
use crate::contacts::ContactsTool;
use crate::encrypt::EncryptTool;
use crate::network_monitor::NetworkMonitorTool;
use crate::knowledge_graph::KnowledgeGraphTool;
use crate::stream_subscribe::StreamSubscribeTool;
use crate::alert_rule::AlertRuleTool;
use crate::blockchain_rpc::BlockchainRpcTool;
use crate::exchange_api::ExchangeApiTool;
use crate::blockchain_tx::BlockchainTxTool;
use crate::contract_security::ContractSecurityTool;
use crate::bridge_api::BridgeApiTool;
use crate::nft_market::NftMarketTool;
use crate::multisig::MultisigTool;
use crate::community_hub::CommunityHubTool;
use crate::memory_maintenance::MemoryMaintenanceTool;
use crate::toggle_manage::ToggleManageTool;
use crate::termux_api::TermuxApiTool;

#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        
        // File system tools
        registry.register(Arc::new(ReadFileTool));
        registry.register(Arc::new(WriteFileTool));
        registry.register(Arc::new(EditFileTool));
        registry.register(Arc::new(ListDirTool));
        
        // Exec tool
        registry.register(Arc::new(ExecTool));
        
        // Web tools
        registry.register(Arc::new(WebSearchTool));
        registry.register(Arc::new(WebFetchTool));
        
        // Communication tools
        registry.register(Arc::new(MessageTool));
        registry.register(Arc::new(SpawnTool));
        
        // Task management
        registry.register(Arc::new(ListTasksTool));
        
        // Browser tools
        registry.register(Arc::new(BrowseTool));
        
        // Scheduler tools
        registry.register(Arc::new(CronTool));
        
        // Memory tools
        registry.register(Arc::new(MemoryQueryTool));
        registry.register(Arc::new(MemoryUpsertTool));
        registry.register(Arc::new(MemoryForgetTool));
        
        // Skill evolution tools
        registry.register(Arc::new(ListSkillsTool));
        
        // System info & capability evolution tools
        registry.register(Arc::new(SystemInfoTool));
        registry.register(Arc::new(CapabilityEvolveTool));
        
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
        
        // Business APIs (Calendar, Notion, CRM, Ticketing)
        registry.register(Arc::new(CalendarApiTool));
        
        // IoT / Smart Home (Home Assistant, MQTT, generic)
        registry.register(Arc::new(IotControlTool));
        
        // Text-to-speech
        registry.register(Arc::new(TtsTool));
        
        // OCR (image text recognition)
        registry.register(Arc::new(OcrTool));
        
        // Multimodal image understanding
        registry.register(Arc::new(ImageUnderstandTool));
        
        // Social media (Twitter/Medium/WordPress)
        registry.register(Arc::new(SocialMediaTool));
        
        // Multi-channel notifications (SMS/Push/Webhook/Desktop)
        registry.register(Arc::new(NotificationTool));
        
        // Cloud platform management (AWS/GCP/Azure)
        registry.register(Arc::new(CloudApiTool));
        
        // Git/GitHub integration
        registry.register(Arc::new(GitApiTool));
        
        // Financial data (stocks/crypto/forex)
        registry.register(Arc::new(FinanceApiTool));
        
        // Video processing (ffmpeg)
        registry.register(Arc::new(VideoProcessTool));
        
        // Health data (Apple Health/Fitbit/Google Fit)
        registry.register(Arc::new(HealthApiTool));
        
        // Map and navigation (Google Maps / Amap)
        registry.register(Arc::new(MapApiTool));
        
        // Contacts management (macOS / Google / CardDAV)
        registry.register(Arc::new(ContactsTool));
        
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
        
        // Blockchain RPC (EVM JSON-RPC + ABI encoding/decoding)
        registry.register(Arc::new(BlockchainRpcTool));
        
        // CEX trading (Binance/OKX/Bybit)
        registry.register(Arc::new(ExchangeApiTool));
        
        // Blockchain transactions (sign & send)
        registry.register(Arc::new(BlockchainTxTool));
        
        // Contract & token security (GoPlus API)
        registry.register(Arc::new(ContractSecurityTool));
        
        // Cross-chain bridge (LI.FI / Stargate / LayerZero)
        registry.register(Arc::new(BridgeApiTool));
        
        // NFT marketplace (OpenSea / Reservoir)
        registry.register(Arc::new(NftMarketTool));
        
        // Multisig wallet (Gnosis Safe)
        registry.register(Arc::new(MultisigTool));
        
        // Community Hub (social interactions, skill discovery)
        registry.register(Arc::new(CommunityHubTool));
        
        // Memory maintenance (Ghost Agent memory gardening)
        registry.register(Arc::new(MemoryMaintenanceTool));
        
        // Toggle management (enable/disable skills and capabilities)
        registry.register(Arc::new(ToggleManageTool));
        
        // Termux API (Android device control via Termux)
        registry.register(Arc::new(TermuxApiTool));
        
        registry
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let schema = tool.schema();
        debug!(name = schema.name, "Registering tool");
        self.tools.insert(schema.name.to_string(), tool);
    }

    /// Register all tools exposed by an MCP server provider.
    pub async fn register_mcp_provider(&mut self, provider: &crate::mcp::provider::McpToolProvider) {
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
        self.tools
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
            .collect()
    }

    /// Get tool schemas filtered by a list of tool names.
    /// Only returns schemas for tools whose names are in the provided list.
    pub fn get_filtered_schemas(&self, names: &[&str]) -> Vec<Value> {
        self.tools
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
            .collect()
    }

    /// Get tiered schemas: full schemas for core tools, lightweight (name+description only) for others.
    /// Core tools get complete parameter schemas; non-core tools get just name+description
    /// so the LLM knows they exist but we save ~500 tokens per non-core tool.
    /// When the LLM tries to call a lightweight tool, the runtime dynamically supplements
    /// the full schema and retries.
    pub fn get_tiered_schemas(&self, names: &[&str], core_tools: &[&str]) -> Vec<Value> {
        self.tools
            .iter()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .map(|(name, tool)| {
                let schema = tool.schema();
                if core_tools.contains(&name.as_str()) {
                    // Full schema for core tools
                    json!({
                        "type": "function",
                        "function": {
                            "name": schema.name,
                            "description": schema.description,
                            "parameters": schema.parameters
                        }
                    })
                } else {
                    // Lightweight schema: name + first sentence of description, no parameters
                    let desc = schema.description;
                    let short_desc = desc.split_once(". ")
                        .map(|(first, _)| format!("{}.", first))
                        .unwrap_or_else(|| {
                            let chars: String = desc.chars().take(120).collect();
                            if desc.chars().count() > 120 { format!("{}...", chars) } else { chars }
                        });
                    json!({
                        "type": "function",
                        "function": {
                            "name": schema.name,
                            "description": short_desc,
                            "parameters": { "type": "object", "properties": {} }
                        }
                    })
                }
            })
            .collect()
    }

    /// Get all registered tool names.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    pub async fn execute(&self, name: &str, ctx: ToolContext, params: Value) -> Result<Value> {
        let tool = self.get(name).ok_or_else(|| {
            Error::Tool(format!("Unknown tool: {}", name))
        })?;

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

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(names.contains(&"blockchain_rpc".to_string()));
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
        reg.register(Arc::new(crate::exec::ExecTool));
        assert!(reg.get("exec").is_some());
        assert_eq!(reg.tool_names().len(), 1);
    }
}
