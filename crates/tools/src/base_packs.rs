//! Named host-configured foundations. Selecting a pack grants capabilities;
//! registration and discovery do not implicitly select packs.
use blockcell_core::config::ToolBasePack;

/// Schema preferences belong to the tool, independently of host capability grants.
#[derive(Debug, Clone, Copy, Default)]
pub struct CodingExposure {
    pub initial: bool,
    pub full_schema: bool,
    pub defer: bool,
}

impl CodingExposure {
    pub const fn primary() -> Self {
        Self {
            initial: true,
            full_schema: true,
            defer: false,
        }
    }
    pub const fn full() -> Self {
        Self {
            initial: false,
            full_schema: true,
            defer: false,
        }
    }
    pub const fn deferred(full_schema: bool) -> Self {
        Self {
            initial: false,
            full_schema,
            defer: true,
        }
    }
}

pub const CODING_DEFAULT_TOOL_NAMES: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "shell_run",
    "grep",
    "tool_search",
];

pub fn tool_names(pack: ToolBasePack) -> Vec<&'static str> {
    match pack {
        ToolBasePack::Core => crate::registry::GLOBAL_CORE_TOOL_NAMES.to_vec(),
        ToolBasePack::Coding => CODING_DEFAULT_TOOL_NAMES
            .iter()
            .copied()
            .chain(["shell_poll", "shell_input", "shell_close", "glob"])
            .collect(),
        ToolBasePack::Skills => vec!["exec_skill_script", "exec_local", "activate_skill"],
        ToolBasePack::Discovery => vec!["tool_search", crate::mcp::search::MCP_SEARCH_TOOL_NAME],
    }
}
