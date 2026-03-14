use std::collections::HashSet;

pub(crate) struct PromptSkillExecutor;

impl PromptSkillExecutor {
    pub(crate) fn resolve_allowed_tool_names(
        skill_tools: &[String],
        available_tool_names: &HashSet<String>,
    ) -> Vec<String> {
        let mut tool_names = skill_tools
            .iter()
            .filter(|name| available_tool_names.contains(name.as_str()))
            .filter(|name| Self::is_tool_allowed(name, available_tool_names))
            .cloned()
            .collect::<Vec<_>>();
        tool_names.sort();
        tool_names.dedup();
        tool_names
    }

    pub(crate) fn is_tool_allowed(tool_name: &str, available_tool_names: &HashSet<String>) -> bool {
        available_tool_names.contains(tool_name) && !Self::is_blocked_tool(tool_name)
    }

    fn is_blocked_tool(tool_name: &str) -> bool {
        matches!(tool_name, "spawn")
    }
}
