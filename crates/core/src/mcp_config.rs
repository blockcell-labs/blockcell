use std::collections::HashMap;
use std::fs;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::file_store::atomic_write;
use crate::paths::Paths;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpDefaultsConfig {
    #[serde(default = "default_startup_timeout_secs")]
    pub startup_timeout_secs: u64,
    #[serde(default = "default_call_timeout_secs")]
    pub call_timeout_secs: u64,
    #[serde(default = "default_auto_start")]
    pub auto_start: bool,
}

fn default_startup_timeout_secs() -> u64 {
    20
}

fn default_call_timeout_secs() -> u64 {
    60
}

fn default_auto_start() -> bool {
    true
}

impl Default for McpDefaultsConfig {
    fn default() -> Self {
        Self {
            startup_timeout_secs: default_startup_timeout_secs(),
            call_timeout_secs: default_call_timeout_secs(),
            auto_start: default_auto_start(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub auto_start: bool,
    pub startup_timeout_secs: u64,
    pub call_timeout_secs: u64,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpServerDefinition {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub auto_start: Option<bool>,
    #[serde(default)]
    pub startup_timeout_secs: Option<u64>,
    #[serde(default)]
    pub call_timeout_secs: Option<u64>,
}

impl McpServerDefinition {
    fn resolve(self, defaults: &McpDefaultsConfig) -> McpServerConfig {
        McpServerConfig {
            command: self.command,
            args: self.args,
            env: self.env,
            cwd: self.cwd,
            enabled: self.enabled,
            auto_start: self.auto_start.unwrap_or(defaults.auto_start),
            startup_timeout_secs: self
                .startup_timeout_secs
                .unwrap_or(defaults.startup_timeout_secs),
            call_timeout_secs: self.call_timeout_secs.unwrap_or(defaults.call_timeout_secs),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpRootConfig {
    #[serde(default)]
    pub defaults: McpDefaultsConfig,
    #[serde(default)]
    pub servers: HashMap<String, McpServerDefinition>,
}

impl McpRootConfig {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let content = fs::read_to_string(path).map_err(|e| {
            Error::Config(format!(
                "Cannot read MCP config file {}: {}",
                path.display(),
                e
            ))
        })?;
        serde_json::from_str(&content).map_err(|e| {
            Error::Config(format!(
                "Parse error in MCP config file {}:{} — {}",
                path.display(),
                e.line(),
                e
            ))
        })
    }

    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_write(path, content.as_bytes())?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpFileServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub auto_start: Option<bool>,
    #[serde(default)]
    pub startup_timeout_secs: Option<u64>,
    #[serde(default)]
    pub call_timeout_secs: Option<u64>,
}

impl McpFileServerConfig {
    fn into_parts(self) -> Result<(String, McpServerDefinition)> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            return Err(Error::Config("MCP server name cannot be empty".to_string()));
        }
        if self.command.trim().is_empty() {
            return Err(Error::Config(format!(
                "MCP server '{}' command cannot be empty",
                name
            )));
        }
        Ok((
            name,
            McpServerDefinition {
                command: self.command,
                args: self.args,
                env: self.env,
                cwd: self.cwd,
                enabled: self.enabled,
                auto_start: self.auto_start,
                startup_timeout_secs: self.startup_timeout_secs,
                call_timeout_secs: self.call_timeout_secs,
            },
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpResolvedConfig {
    #[serde(default)]
    pub defaults: McpDefaultsConfig,
    #[serde(default)]
    pub servers: HashMap<String, McpServerConfig>,
}

impl McpResolvedConfig {
    pub fn load_merged(paths: &Paths) -> Result<Self> {
        let root = if paths.mcp_config_file().exists() {
            McpRootConfig::load(&paths.mcp_config_file())?
        } else {
            McpRootConfig::default()
        };

        for (name, definition) in &root.servers {
            if name.trim().is_empty() || definition.command.trim().is_empty() {
                return Err(Error::Config(format!(
                    "MCP server '{name}' requires a non-empty name and command"
                )));
            }
        }
        let defaults = root.defaults.clone();
        let mut servers: HashMap<String, McpServerConfig> = root
            .servers
            .into_iter()
            .map(|(name, def)| (name, def.resolve(&defaults)))
            .collect();

        if paths.mcp_dir().exists() {
            let mut entries = fs::read_dir(paths.mcp_dir())?
                .collect::<std::result::Result<Vec<_>, std::io::Error>>()?;
            entries.sort_by_key(|entry| entry.file_name());

            for entry in entries {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }

                let content = fs::read_to_string(&path).map_err(|e| {
                    Error::Config(format!(
                        "Cannot read MCP server file {}: {}",
                        path.display(),
                        e
                    ))
                })?;
                let file_cfg: McpFileServerConfig = serde_json::from_str(&content)
                    .map_err(|e| Error::Config(format!("Parse error in MCP server file {}:{} — {}\n  Hint: each file must have a \"command\" field (e.g. \"command\": \"uvx\")", path.display(), e.line(), e)))?;
                let (name, def) = file_cfg.into_parts()?;
                if servers.contains_key(&name) {
                    tracing::warn!(
                        server = %name,
                        file = %path.display(),
                        "mcp.d entry overrides server of the same name from mcp.json"
                    );
                }
                servers.insert(name, def.resolve(&defaults));
            }
        }

        Ok(Self { defaults, servers })
    }
}

#[cfg(test)]
mod recheck_contract_probe {
    #[test]
    fn recheck_contract_probe_root_command() {
        let root = std::env::temp_dir().join(format!("p3-mcp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let paths = crate::Paths::with_base(root.clone());
        std::fs::write(
            paths.mcp_config_file(),
            r#"{"servers":{"bad":{"command":"   "}}}"#,
        )
        .unwrap();
        let result = super::McpResolvedConfig::load_merged(&paths);
        std::fs::remove_dir_all(root).unwrap();
        assert!(result.is_err(), "root config accepted whitespace command");
    }
    #[test]
    fn recheck_contract_probe_fallback_json() {
        let fallback = crate::agent_result::AgentResult::serialization_fallback("test");
        assert!(
            crate::agent_result::AgentResult::from_json(&fallback).is_some(),
            "fallback cannot deserialize as AgentResult"
        );
    }
}
